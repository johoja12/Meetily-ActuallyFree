//! CoreML runtime for the fixed-shape Nemotron 3 diarization conversion
//! (`FluidInference/nemotron-3-diarization-coreml`, monolithic/v2).
//!
//! The model is the same network as the ONNX export with every streaming input
//! at its capacity: Rust zero-pads chunk, speaker cache and FIFO and passes the
//! valid lengths. Valid output frames are left-packed `[spkcache | fifo | chunk]`
//! exactly as the dynamic ONNX graph returns them, so the Sortformer state update
//! is shared between both runtimes.
//!
//! ONNX Runtime's own CoreML execution provider is not used: it cannot compile or
//! run the dynamic-shape ONNX graph (see DEVELOPMENT_MAP.md).
use anyhow::{anyhow, ensure, Result};
use ndarray::{s, Array3, ArrayView3};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;

const MELS: usize = 128;
const EMB: usize = 512;
const SPEAKERS: usize = 8;
/// `MLComputeUnitsAll`: CPU, GPU and Neural Engine.
const COMPUTE_UNITS_ALL: i32 = 2;

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct Shapes {
    chunk_frames: i32,
    spkcache_frames: i32,
    fifo_frames: i32,
    preds_rows: i32,
    hires_rows: i32,
    embs_rows: i32,
}

extern "C" {
    fn meetily_coreml_load(path: *const c_char, compute_units: i32, shapes: *mut Shapes, error: *mut *mut c_char) -> *mut c_void;
    #[allow(clippy::too_many_arguments)]
    fn meetily_coreml_predict(
        model: *mut c_void, chunk: *const f32, chunk_length: i32, spkcache: *const f32, spkcache_length: i32,
        fifo: *const f32, fifo_length: i32, preds: *mut f32, hires: *mut f32, embs: *mut f32, error: *mut *mut c_char,
    ) -> i32;
    fn meetily_coreml_free(model: *mut c_void);
    fn meetily_coreml_free_string(value: *mut c_char);
}

fn take_error(error: *mut c_char, fallback: &str) -> anyhow::Error {
    if error.is_null() { return anyhow!("{fallback}"); }
    // SAFETY: the bridge returns a strdup'd NUL-terminated string that we free once.
    let message = unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned();
    unsafe { meetily_coreml_free_string(error) };
    anyhow!("{message}")
}

/// The fixed input capacities a compiled model was converted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub chunk_mel_frames: usize,
    pub spkcache_frames: usize,
    pub fifo_frames: usize,
}

pub struct CoreMlSortformer {
    handle: *mut c_void,
    shapes: Shapes,
    window: Window,
    chunk: Vec<f32>,
    spkcache: Vec<f32>,
    fifo: Vec<f32>,
}

// SAFETY: the handle is an MLModel plus its input buffers; every call goes
// through &mut self, so one thread uses it at a time. CoreML models may be used
// from any thread.
unsafe impl Send for CoreMlSortformer {}

impl CoreMlSortformer {
    pub fn load(path: &Path) -> Result<Self> {
        let c_path = CString::new(path.to_string_lossy().as_bytes())?;
        let mut shapes = Shapes::default();
        let mut error = std::ptr::null_mut();
        // SAFETY: valid C string and out-pointers; the bridge validates the model's I/O.
        let handle = unsafe { meetily_coreml_load(c_path.as_ptr(), COMPUTE_UNITS_ALL, &mut shapes, &mut error) };
        if handle.is_null() { return Err(take_error(error, "CoreML model could not be loaded")); }
        let window = Window {
            chunk_mel_frames: shapes.chunk_frames as usize,
            spkcache_frames: shapes.spkcache_frames as usize,
            fifo_frames: shapes.fifo_frames as usize,
        };
        Ok(Self {
            handle,
            shapes,
            window,
            chunk: vec![0.0; window.chunk_mel_frames * MELS],
            spkcache: vec![0.0; window.spkcache_frames * EMB],
            fifo: vec![0.0; window.fifo_frames * EMB],
        })
    }

    pub fn window(&self) -> Window { self.window }

    /// One streaming step. `chunk` may be shorter than capacity (the final chunk);
    /// `chunk_len` is its valid mel frame count. Returns `(preds_diar, preds_hires,
    /// chunk_pre_encode_embs)` shaped like the ONNX outputs but at capacity.
    pub fn run(&mut self, chunk: ArrayView3<f32>, chunk_len: usize, spkcache: ArrayView3<f32>, fifo: ArrayView3<f32>)
        -> Result<(Array3<f32>, Array3<f32>, Array3<f32>)> {
        let (cache_len, fifo_len) = (spkcache.shape()[1], fifo.shape()[1]);
        ensure!(chunk.shape()[1] <= self.window.chunk_mel_frames && chunk_len <= chunk.shape()[1], "Chunk exceeds the CoreML model's window");
        ensure!(cache_len <= self.window.spkcache_frames && fifo_len <= self.window.fifo_frames, "Speaker cache or FIFO exceeds the CoreML model's window");
        pad_into(&mut self.chunk, chunk, MELS);
        pad_into(&mut self.spkcache, spkcache, EMB);
        pad_into(&mut self.fifo, fifo, EMB);
        let mut preds = Array3::<f32>::zeros((1, self.shapes.preds_rows as usize, SPEAKERS));
        let mut hires = Array3::<f32>::zeros((1, self.shapes.hires_rows as usize, SPEAKERS));
        let mut embs = Array3::<f32>::zeros((1, self.shapes.embs_rows as usize, EMB));
        let mut error = std::ptr::null_mut();
        // SAFETY: every buffer has exactly the capacity reported at load, and the
        // output arrays are standard-layout and owned here.
        let ok = unsafe {
            meetily_coreml_predict(
                self.handle, self.chunk.as_ptr(), chunk_len as i32, self.spkcache.as_ptr(), cache_len as i32,
                self.fifo.as_ptr(), fifo_len as i32, preds.as_mut_ptr(), hires.as_mut_ptr(), embs.as_mut_ptr(), &mut error,
            )
        };
        if ok == 0 { return Err(take_error(error, "CoreML prediction failed")); }
        ensure!(preds.iter().chain(hires.iter()).chain(embs.iter()).all(|v| v.is_finite()), "Non-finite CoreML output");
        Ok((preds, hires, embs))
    }
}

impl Drop for CoreMlSortformer {
    fn drop(&mut self) {
        // SAFETY: handle came from meetily_coreml_load and is released once.
        unsafe { meetily_coreml_free(self.handle) };
    }
}

/// Copy `[1, rows, cols]` into a row-major buffer at capacity, zeroing the tail.
fn pad_into(buffer: &mut [f32], value: ArrayView3<f32>, cols: usize) {
    let rows = value.shape()[1];
    debug_assert_eq!(value.shape()[2], cols);
    for (r, row) in value.slice(s![0, .., ..]).outer_iter().enumerate() {
        for (c, v) in row.iter().enumerate() { buffer[r * cols + c] = *v; }
    }
    buffer[rows * cols..].fill(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_copies_valid_rows_and_clears_stale_ones() {
        let mut buffer = vec![9.0; 4 * 2];
        let value = Array3::from_shape_vec((1, 2, 2), vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        pad_into(&mut buffer, value.view(), 2);
        assert_eq!(buffer, vec![1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0]);
        let empty = Array3::<f32>::zeros((1, 0, 2));
        pad_into(&mut buffer, empty.view(), 2);
        assert!(buffer.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn padding_reads_non_standard_layouts_in_logical_order() {
        // A transposed view: logical [[1, 3], [2, 4]].
        let base = Array3::from_shape_vec((1, 2, 2), vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        let transposed = base.view().permuted_axes([0, 2, 1]);
        let mut buffer = vec![0.0; 4];
        pad_into(&mut buffer, transposed, 2);
        assert_eq!(buffer, vec![1.0, 3.0, 2.0, 4.0]);
    }
}
