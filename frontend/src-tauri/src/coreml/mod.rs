//! General CoreML model runner for macOS (`bridge.m`): named row-major inputs
//! and outputs, converted to and from whatever types and strides the compiled
//! model declares. Used by the Parakeet CoreML backend. Nemotron diarization
//! has its own specialized runner (`diarization/coreml.rs`) with persistent
//! fixed-window buffers.
use anyhow::{anyhow, ensure, Result};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;

const F32: i32 = 0;
const I32: i32 = 1;
/// `MLComputeUnits` values.
pub const COMPUTE_UNITS_CPU: i32 = 0;
pub const COMPUTE_UNITS_ALL: i32 = 2;
pub const COMPUTE_UNITS_CPU_AND_NEURAL_ENGINE: i32 = 3;

#[repr(C)]
struct RawInput {
    name: *const c_char,
    dtype: i32,
    rank: i32,
    shape: *const i64,
    data: *const c_void,
}

#[repr(C)]
struct RawOutput {
    name: *const c_char,
    dtype: i32,
    capacity: i64,
    data: *mut c_void,
    rank: i32,
    shape: [i64; 8],
}

extern "C" {
    fn meetily_ml_load(path: *const c_char, compute_units: i32, error: *mut *mut c_char) -> *mut c_void;
    fn meetily_ml_predict(model: *mut c_void, inputs: *const RawInput, input_count: i32, outputs: *mut RawOutput, output_count: i32, error: *mut *mut c_char) -> i32;
    fn meetily_ml_free(model: *mut c_void);
    fn meetily_ml_free_string(value: *mut c_char);
}

fn take_error(error: *mut c_char, fallback: &str) -> anyhow::Error {
    if error.is_null() { return anyhow!("{fallback}"); }
    // SAFETY: the bridge returns a strdup'd NUL-terminated string that we free once.
    let message = unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned();
    unsafe { meetily_ml_free_string(error) };
    anyhow!("{message}")
}

/// A row-major input buffer.
pub enum Input<'a> {
    F32(&'a [f32]),
    I32(&'a [i32]),
}

/// A row-major output buffer the caller sizes to the model's declared shape.
pub enum Output<'a> {
    F32(&'a mut [f32]),
    I32(&'a mut [i32]),
}

pub struct Model {
    handle: *mut c_void,
}

// SAFETY: an MLModel may be used from any thread; Model methods take &mut self,
// so one thread uses a handle at a time.
unsafe impl Send for Model {}
// SAFETY: Model has no &self methods that touch the handle; shared references
// cannot reach CoreML, so sharing them across threads is sound.
unsafe impl Sync for Model {}

impl Model {
    pub fn load(path: &Path, compute_units: i32) -> Result<Self> {
        let c_path = CString::new(path.to_string_lossy().as_bytes())?;
        let mut error = std::ptr::null_mut();
        // SAFETY: valid C string and out-pointer.
        let handle = unsafe { meetily_ml_load(c_path.as_ptr(), compute_units, &mut error) };
        if handle.is_null() { return Err(take_error(error, "CoreML model could not be loaded")); }
        Ok(Self { handle })
    }

    /// Run one prediction. Returns each output's shape; data is written into
    /// the caller's buffers in row-major order.
    pub fn predict(&mut self, inputs: &[(&str, &[usize], Input)], outputs: &mut [(&str, Output)]) -> Result<Vec<Vec<usize>>> {
        let names: Vec<CString> = inputs.iter().map(|(n, _, _)| CString::new(*n)).collect::<Result<_, _>>()?;
        let shapes: Vec<Vec<i64>> = inputs.iter().map(|(_, s, _)| s.iter().map(|&d| d as i64).collect()).collect();
        let mut raw_inputs = Vec::with_capacity(inputs.len());
        for (i, (name, shape, data)) in inputs.iter().enumerate() {
            let elements: usize = shape.iter().product();
            let (dtype, ptr, len) = match data {
                Input::F32(d) => (F32, d.as_ptr() as *const c_void, d.len()),
                Input::I32(d) => (I32, d.as_ptr() as *const c_void, d.len()),
            };
            ensure!(len == elements, "CoreML input {name} has {len} values for shape {shape:?}");
            raw_inputs.push(RawInput { name: names[i].as_ptr(), dtype, rank: shape.len() as i32, shape: shapes[i].as_ptr(), data: ptr });
        }
        let out_names: Vec<CString> = outputs.iter().map(|(n, _)| CString::new(*n)).collect::<Result<_, _>>()?;
        let mut raw_outputs: Vec<RawOutput> = outputs.iter_mut().enumerate().map(|(i, (_, buffer))| {
            let (dtype, data, capacity) = match buffer {
                Output::F32(d) => (F32, d.as_mut_ptr() as *mut c_void, d.len()),
                Output::I32(d) => (I32, d.as_mut_ptr() as *mut c_void, d.len()),
            };
            RawOutput { name: out_names[i].as_ptr(), dtype, capacity: capacity as i64, data, rank: 0, shape: [0; 8] }
        }).collect();
        let mut error = std::ptr::null_mut();
        // SAFETY: input buffers hold product(shape) elements (checked above);
        // output buffers report their capacity and the bridge never exceeds it.
        let ok = unsafe {
            meetily_ml_predict(self.handle, raw_inputs.as_ptr(), raw_inputs.len() as i32, raw_outputs.as_mut_ptr(), raw_outputs.len() as i32, &mut error)
        };
        if ok == 0 { return Err(take_error(error, "CoreML prediction failed")); }
        Ok(raw_outputs.iter().map(|o| o.shape[..o.rank as usize].iter().map(|&d| d as usize).collect()).collect())
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: handle came from meetily_ml_load and is released once.
        unsafe { meetily_ml_free(self.handle) };
    }
}
