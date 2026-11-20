//! DeepFilterNet 3 speech enhancement (official libDF, tract backend, embedded
//! model), streamed and delay-compensated.
//!
//! Settings match the `deep-filter -D -a 12` command qualified by listening:
//! at most 12 dB of attenuation (the enhanced signal is mixed with the input so
//! speech never sounds over-processed), no post-filter, the CLI's local-SNR
//! thresholds (-15 / 35 / 35 dB) and max mask reduction. The model's STFT and
//! lookahead delay (`fft_size - hop_size + lookahead * hop_size` samples) is
//! dropped from the front and flushed with zeros at the end, so output sample n
//! corresponds to input sample n and the total length is unchanged.
use anyhow::{anyhow, Result};
use df::tract::{DfParams, DfTract, ReduceMask, RuntimeParams};
use ndarray015::Array2;

pub const SAMPLE_RATE: usize = 48_000;
pub const ATTENUATION_LIMIT_DB: f32 = 12.0;

pub struct StreamingDfn {
    model: DfTract,
    hop: usize,
    /// Output samples still to drop (model delay).
    skip: usize,
    delay: usize,
    input: Vec<f32>,
    noisy: Array2<f32>,
    enhanced: Array2<f32>,
    pushed: usize,
    emitted: usize,
}

impl StreamingDfn {
    pub fn new() -> Result<Self> {
        let params = RuntimeParams::default_with_ch(1)
            .with_atten_lim(ATTENUATION_LIMIT_DB)
            .with_thresholds(-15.0, 35.0, 35.0)
            .with_mask_reduce(ReduceMask::MAX);
        let model = DfTract::new(DfParams::default(), &params).map_err(|e| anyhow!("DeepFilterNet could not start: {e}"))?;
        if model.sr != SAMPLE_RATE {
            return Err(anyhow!("DeepFilterNet model runs at {} Hz, expected {SAMPLE_RATE}", model.sr));
        }
        let hop = model.hop_size;
        let delay = model.fft_size - hop + model.lookahead * hop;
        Ok(Self {
            noisy: Array2::zeros((1, hop)),
            enhanced: Array2::zeros((1, hop)),
            model,
            hop,
            skip: delay,
            delay,
            input: Vec::new(),
            pushed: 0,
            emitted: 0,
        })
    }

    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        self.pushed += samples.len();
        self.input.extend_from_slice(samples);
        self.run_frames()
    }

    /// Flush the model delay; total output length equals total input length.
    pub fn finish(&mut self) -> Result<Vec<f32>> {
        let flush = self.delay + self.hop - (self.input.len() % self.hop);
        self.input.extend(std::iter::repeat_n(0.0, flush));
        let mut out = self.run_frames()?;
        let excess = (self.emitted).saturating_sub(self.pushed);
        out.truncate(out.len().saturating_sub(excess));
        self.emitted -= excess;
        Ok(out)
    }

    fn run_frames(&mut self) -> Result<Vec<f32>> {
        let mut out = Vec::with_capacity(self.input.len());
        let mut offset = 0;
        while self.input.len() - offset >= self.hop {
            for (dst, src) in self.noisy.iter_mut().zip(&self.input[offset..offset + self.hop]) {
                // libDF warns on |x| >= 1 every frame; WPE can overshoot slightly.
                *dst = src.clamp(-0.9999, 0.9999);
            }
            self.model
                .process(self.noisy.view(), self.enhanced.view_mut())
                .map_err(|e| anyhow!("DeepFilterNet failed: {e}"))?;
            let frame = self.enhanced.as_slice().expect("standard layout");
            let drop = self.skip.min(self.hop);
            self.skip -= drop;
            out.extend_from_slice(&frame[drop..]);
            offset += self.hop;
        }
        self.input.drain(..offset);
        self.emitted += out.len();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[f32], chunk: usize) -> Vec<f32> {
        let mut dfn = StreamingDfn::new().unwrap();
        let mut out = Vec::new();
        for piece in input.chunks(chunk) {
            out.extend(dfn.push(piece).unwrap());
        }
        out.extend(dfn.finish().unwrap());
        out
    }

    #[test]
    fn keeps_length_and_timing() {
        // A 1 kHz tone burst that starts at 0.5 s: the enhanced burst must start
        // at the same sample (within one 10 ms hop), not one model delay later.
        let sr = SAMPLE_RATE;
        let mut input = vec![0.0f32; sr * 2 + 77];
        for (i, v) in input.iter_mut().enumerate().skip(sr / 2).take(sr / 2) {
            *v = 0.3 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin();
        }
        let out = run(&input, 4096);
        assert_eq!(out.len(), input.len());
        let onset = |x: &[f32]| x.iter().position(|v| v.abs() > 0.05).unwrap();
        let shift = onset(&out) as isize - onset(&input) as isize;
        assert!(shift.abs() <= 480, "enhanced audio shifted by {shift} samples");
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn attenuation_is_limited_to_12_db() {
        // Pure noise is what DeepFilterNet removes most; the limit keeps at
        // least -12 dB of it (energy ratio >= 10^-1.2, allowing some slack).
        let mut s = 7u64;
        let input: Vec<f32> = (0..SAMPLE_RATE * 3)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5) * 0.2
            })
            .collect();
        let out = run(&input, 48_000);
        let energy = |x: &[f32]| x[SAMPLE_RATE..].iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        let db = 10.0 * (energy(&out) / energy(&input)).log10();
        assert!(db > -14.0, "noise attenuated by {db:.1} dB");
    }
}
