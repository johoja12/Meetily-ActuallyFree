//! Neural echo cancellation core: DTLN-aec (two small ONNX models with LSTM
//! state), streamed in 8 ms hops at 16 kHz. Ported from Hyprnote/anarlog
//! (crates/aec, MIT); the models are DTLN-aec by Westhausen & Meyer (MIT).
//!
//! The model has no delay search: the caller must feed the reference slightly
//! ahead of its echo (see `echo_cancellation`). Output trails input by
//! `LATENCY` samples (overlap-add of 512-sample blocks).

use ndarray::{Array3, Array4};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use realfft::{num_complex::Complex, ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// DTLN-aec "128" models (MIT, see resources/dtln-aec/LICENSE).
pub const MODEL_1: &[u8] = include_bytes!("../../resources/dtln-aec/model_128_1.onnx");
pub const MODEL_2: &[u8] = include_bytes!("../../resources/dtln-aec/model_128_2.onnx");
pub const MODEL_STATE_SIZE: usize = 128;
const BLOCK: usize = 512;
const SHIFT: usize = 128;
/// Output delay relative to input, in samples.
pub const LATENCY: usize = BLOCK - SHIFT;
/// Samples consumed per model step (8 ms).
pub const HOP: usize = SHIFT;
/// A reference block whose peak stays below this (-60 dBFS) cannot leave an
/// audible echo, so the model is skipped and the mic passes through.
const SILENT_REFERENCE: f32 = 1e-3;
/// Model steps until a restarted overlap-add covers a whole output hop.
const PRIMING_HOPS: usize = BLOCK / SHIFT - 1;

pub struct NeuralEcho {
    model_1: Session,
    model_2: Session,
    states_1: Array4<f32>,
    states_2: Array4<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    mic_block: Vec<f32>,
    ref_block: Vec<f32>,
    out_block: Vec<f32>,
    pending_mic: Vec<f32>,
    pending_ref: Vec<f32>,
    failed_hops: u64,
    /// Model steps since the last skipped (silent-reference) step.
    primed: usize,
    #[cfg(test)]
    pub(crate) model_hops: u64,
    // scratch, reused every hop
    mic_mag: Array3<f32>,
    ref_mag: Array3<f32>,
    estimate: Array3<f32>,
    ref_time: Array3<f32>,
    time: Vec<f32>,
    spec: Vec<Complex<f32>>,
    ref_spec: Vec<Complex<f32>>,
    fft_in: Vec<f32>,
    scratch: Vec<Complex<f32>>,
}

fn session(model: &[u8]) -> ort::Result<Session> {
    // Small per-frame models: one thread is fastest and avoids contention.
    Session::builder()?
        .with_intra_threads(1)?
        .with_inter_threads(1)?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .commit_from_memory(model)
}

impl NeuralEcho {
    pub fn new(model_1: &[u8], model_2: &[u8], state_size: usize) -> ort::Result<Self> {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(BLOCK);
        let ifft = planner.plan_fft_inverse(BLOCK);
        let scratch_len = fft.get_scratch_len().max(ifft.get_scratch_len());
        Ok(Self {
            model_1: session(model_1)?,
            model_2: session(model_2)?,
            states_1: Array4::zeros((1, 2, state_size, 2)),
            states_2: Array4::zeros((1, 2, state_size, 2)),
            fft,
            ifft,
            mic_block: vec![0.0; BLOCK],
            ref_block: vec![0.0; BLOCK],
            out_block: vec![0.0; BLOCK],
            pending_mic: Vec::new(),
            pending_ref: Vec::new(),
            failed_hops: 0,
            primed: 0,
            #[cfg(test)]
            model_hops: 0,
            mic_mag: Array3::zeros((1, 1, BLOCK / 2 + 1)),
            ref_mag: Array3::zeros((1, 1, BLOCK / 2 + 1)),
            estimate: Array3::zeros((1, 1, BLOCK)),
            ref_time: Array3::zeros((1, 1, BLOCK)),
            time: vec![0.0; BLOCK],
            spec: vec![Complex::default(); BLOCK / 2 + 1],
            ref_spec: vec![Complex::default(); BLOCK / 2 + 1],
            fft_in: vec![0.0; BLOCK],
            scratch: vec![Complex::default(); scratch_len],
        })
    }

    /// Feed aligned 16 kHz mic and reference of equal length; returns one
    /// output sample per input sample once a full hop is available (the
    /// remainder waits for the next call). A hop the model fails on passes the
    /// mic through, on the same timeline. So does a hop whose reference block
    /// is silent (most of a call: the other side is listening), which skips
    /// the model; after such a gap the first `PRIMING_HOPS` model outputs are
    /// incomplete overlap-adds, so the mic keeps passing through until the
    /// model's output is whole. The reference leads its echo, so the model is
    /// running before any echo arrives.
    pub fn process(&mut self, mic: &[f32], reference: &[f32]) -> Vec<f32> {
        let n = mic.len().min(reference.len());
        self.pending_mic.extend_from_slice(&mic[..n]);
        self.pending_ref.extend_from_slice(&reference[..n]);
        let hops = self.pending_mic.len() / SHIFT;
        let mut out = Vec::with_capacity(hops * SHIFT);
        for h in 0..hops {
            let r = h * SHIFT..(h + 1) * SHIFT;
            self.mic_block.copy_within(SHIFT.., 0);
            self.mic_block[BLOCK - SHIFT..].copy_from_slice(&self.pending_mic[r.clone()]);
            self.ref_block.copy_within(SHIFT.., 0);
            self.ref_block[BLOCK - SHIFT..].copy_from_slice(&self.pending_ref[r]);
            if self.ref_block.iter().all(|x| x.abs() < SILENT_REFERENCE) {
                self.primed = 0;
                out.extend_from_slice(&self.mic_block[..SHIFT]);
                self.out_block.copy_within(SHIFT.., 0);
                self.out_block[BLOCK - SHIFT..].fill(0.0);
                continue;
            }
            match self.hop() {
                Ok(()) if self.primed < PRIMING_HOPS => {
                    self.primed += 1;
                    out.extend_from_slice(&self.mic_block[..SHIFT]);
                }
                Ok(()) => out.extend_from_slice(&self.out_block[..SHIFT]),
                Err(e) => {
                    self.failed_hops += 1;
                    if self.failed_hops == 1 || self.failed_hops % 1000 == 0 {
                        log::warn!("Neural echo cancellation failed for {} hops: {e}", self.failed_hops);
                    }
                    // The mic `LATENCY` samples ago lines up with the output.
                    out.extend_from_slice(&self.mic_block[..SHIFT]);
                    self.out_block.copy_within(SHIFT.., 0);
                    self.out_block[BLOCK - SHIFT..].fill(0.0);
                }
            }
        }
        self.pending_mic.drain(..hops * SHIFT);
        self.pending_ref.drain(..hops * SHIFT);
        out
    }

    fn hop(&mut self) -> ort::Result<()> {
        #[cfg(test)]
        {
            self.model_hops += 1;
        }
        // Magnitude spectra of the current mic and reference blocks.
        self.fft_in.copy_from_slice(&self.mic_block);
        let _ = self.fft.process_with_scratch(&mut self.fft_in, &mut self.spec, &mut self.scratch);
        self.fft_in.copy_from_slice(&self.ref_block);
        let _ = self.fft.process_with_scratch(&mut self.fft_in, &mut self.ref_spec, &mut self.scratch);
        for (m, c) in self.mic_mag.iter_mut().zip(&self.spec) {
            *m = c.norm();
        }
        for (m, c) in self.ref_mag.iter_mut().zip(&self.ref_spec) {
            *m = c.norm();
        }

        // Stage 1: spectral mask.
        let outputs = self.model_1.run(ort::inputs![
            TensorRef::from_array_view(self.mic_mag.view())?,
            TensorRef::from_array_view(self.states_1.view())?,
            TensorRef::from_array_view(self.ref_mag.view())?
        ])?;
        let mask = outputs["Identity"].try_extract_array::<f32>()?;
        for (c, m) in self.spec.iter_mut().zip(mask.iter()) {
            *c *= *m;
        }
        let states = outputs["Identity_1"].try_extract_array::<f32>()?;
        self.states_1.assign(&states.view().into_shape_with_order(self.states_1.raw_dim()).map_err(|e| ort::Error::new(e.to_string()))?);
        drop(outputs);
        let _ = self.ifft.process_with_scratch(&mut self.spec, &mut self.time, &mut self.scratch);
        for (e, t) in self.estimate.iter_mut().zip(&self.time) {
            *e = *t / BLOCK as f32;
        }
        for (r, x) in self.ref_time.iter_mut().zip(&self.ref_block) {
            *r = *x;
        }

        // Stage 2: time-domain refinement.
        let outputs = self.model_2.run(ort::inputs![
            TensorRef::from_array_view(self.estimate.view())?,
            TensorRef::from_array_view(self.states_2.view())?,
            TensorRef::from_array_view(self.ref_time.view())?
        ])?;
        // Overlap-add.
        self.out_block.copy_within(SHIFT.., 0);
        self.out_block[BLOCK - SHIFT..].fill(0.0);
        let block = outputs["Identity"].try_extract_array::<f32>()?;
        for (o, b) in self.out_block.iter_mut().zip(block.iter()) {
            *o += *b;
        }
        let states = outputs["Identity_1"].try_extract_array::<f32>()?;
        self.states_2.assign(&states.view().into_shape_with_order(self.states_2.raw_dim()).map_err(|e| ort::Error::new(e.to_string()))?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (i as f32 * 0.03).sin()).collect()
    }

    #[test]
    fn silent_reference_skips_the_model_and_passes_the_mic_through_aligned() {
        let mut echo = NeuralEcho::new(MODEL_1, MODEL_2, MODEL_STATE_SIZE).unwrap();
        let mic = tone(16_000, 0.3);
        let out = echo.process(&mic, &vec![0.0; mic.len()]);
        assert_eq!(echo.model_hops, 0);
        assert_eq!(out.len(), mic.len());
        // Output trails input by LATENCY samples, exactly.
        assert_eq!(&out[LATENCY..], &mic[..mic.len() - LATENCY]);
    }

    #[test]
    fn audible_reference_runs_the_model_after_priming() {
        let mut echo = NeuralEcho::new(MODEL_1, MODEL_2, MODEL_STATE_SIZE).unwrap();
        let mic = tone(16_000, 0.3);
        let reference = tone(16_000, 0.3);
        let out = echo.process(&mic, &reference);
        assert_eq!(echo.model_hops as usize, mic.len() / HOP);
        // The first PRIMING_HOPS hops pass the mic through; later hops are model output.
        assert_eq!(&out[..PRIMING_HOPS * HOP], &[vec![0.0; LATENCY], mic[..PRIMING_HOPS * HOP - LATENCY].to_vec()].concat()[..]);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
