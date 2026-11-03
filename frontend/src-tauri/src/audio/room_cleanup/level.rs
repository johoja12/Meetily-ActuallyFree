//! Makeup gain so the cleaned copy plays at the original's speech level.
//!
//! WPE removes reverb energy that is part of every word, so speech comes out
//! about 5 dB quieter (measured on a room meeting) and an A/B switch would make
//! the cleaned copy sound worse. The gain is the ratio of speech energy (100 ms
//! frames whose input is above -40 dBFS) between input and output over
//! everything processed so far, applied slowly (never attenuating, at most
//! +12 dB), so pauses stay as quiet as the filters made them. Output sample n
//! is matched with input sample n; the input is buffered until its output
//! arrives (WPE emits a block at a time). A soft knee from 0.85 to a 0.95
//! ceiling keeps peaks from clipping, leaving room for AAC overshoot.
use std::collections::VecDeque;

const FRAME: usize = 4_800;
const SPEECH_ENERGY: f64 = 1e-4; // -40 dBFS mean square
const MAX_GAIN: f64 = 3.981; // +12 dB
const SMOOTHING: f32 = 0.05; // per 100 ms frame: ~2 s time constant
const KNEE: f32 = 0.85;
const CEILING: f32 = 0.95;

pub struct LevelMatch {
    input: VecDeque<f32>,
    frame_in: f64,
    frame_out: f64,
    frame_len: usize,
    speech_in: f64,
    speech_out: f64,
    gain: f32,
}

impl LevelMatch {
    pub fn new() -> Self {
        Self { input: VecDeque::new(), frame_in: 0.0, frame_out: 0.0, frame_len: 0, speech_in: 0.0, speech_out: 0.0, gain: 1.0 }
    }

    /// Unprocessed samples, in order, as they enter the filter chain.
    pub fn push_input(&mut self, samples: &[f32]) {
        self.input.extend(samples);
    }

    /// Filtered samples, in order; returns them with makeup gain applied.
    pub fn process(&mut self, output: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(output.len());
        for &y in output {
            let x = self.input.pop_front().unwrap_or(0.0);
            self.frame_in += (x as f64).powi(2);
            self.frame_out += (y as f64).powi(2);
            self.frame_len += 1;
            if self.frame_len == FRAME {
                self.end_frame();
            }
            out.push(soft_limit(y * self.gain));
        }
        out
    }

    fn end_frame(&mut self) {
        let n = self.frame_len as f64;
        if self.frame_in / n > SPEECH_ENERGY {
            self.speech_in += self.frame_in;
            self.speech_out += self.frame_out;
        }
        if self.speech_out > 0.0 {
            let target = (self.speech_in / self.speech_out).sqrt().clamp(1.0, MAX_GAIN) as f32;
            self.gain += (target - self.gain) * SMOOTHING;
        }
        self.frame_in = 0.0;
        self.frame_out = 0.0;
        self.frame_len = 0;
    }
}

fn soft_limit(v: f32) -> f32 {
    let a = v.abs();
    if a <= KNEE {
        v
    } else {
        v.signum() * (KNEE + (CEILING - KNEE) * ((a - KNEE) / (CEILING - KNEE)).tanh())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_speech_level_without_lifting_pauses_or_clipping() {
        // "Speech" at -20 dBFS alternating with near-silence; the filtered copy
        // is 5 dB quieter in speech and much quieter in pauses.
        let sr = 48_000;
        let mut input = Vec::new();
        let mut filtered = Vec::new();
        for second in 0..60 {
            for i in 0..sr {
                let tone = (i as f32 * 0.05).sin();
                let (x, y) = if second % 2 == 0 { (0.14 * tone, 0.079 * tone) } else { (0.002 * tone, 0.0005 * tone) };
                input.push(x);
                filtered.push(y);
            }
        }
        let mut level = LevelMatch::new();
        level.push_input(&input);
        let out: Vec<f32> = filtered.chunks(10_000).flat_map(|c| level.process(c)).collect();
        assert_eq!(out.len(), input.len());
        let db = |x: &[f32]| 10.0 * (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).log10();
        let speech = (50 * sr)..(51 * sr);
        let pause = (51 * sr)..(52 * sr);
        assert!((db(&out[speech.clone()]) - db(&input[speech])).abs() < 1.0, "speech level not restored");
        assert!(db(&out[pause.clone()]) < db(&input[pause]) - 5.0, "pauses were lifted back up");
        assert!(out.iter().all(|v| v.abs() <= CEILING));
    }

    #[test]
    fn never_attenuates_and_limits_peaks() {
        let mut level = LevelMatch::new();
        level.push_input(&vec![0.1; 48_000]);
        let out = level.process(&vec![0.5; 48_000]);
        assert!(out.iter().all(|v| (*v - 0.5).abs() < 1e-6));
        assert!(soft_limit(3.0) <= CEILING && soft_limit(-3.0) >= -CEILING && soft_limit(0.5) == 0.5);
        assert!(soft_limit(0.9) > 0.85 && soft_limit(0.9) < 0.9);
    }
}
