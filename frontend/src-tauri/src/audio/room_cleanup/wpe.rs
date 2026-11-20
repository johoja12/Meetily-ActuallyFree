//! Single-channel WPE (weighted prediction error) dereverberation, streamed.
//!
//! A port of nara_wpe's `wpe` (v7) with its STFT: periodic Blackman analysis
//! window, 1024-sample frames every 256 samples at 48 kHz, zero "fading" pads of
//! `size - shift` on both ends, and a biorthogonal synthesis window, so an
//! unmodified spectrum reconstructs the input exactly. Per frequency bin it
//! predicts the late reverberation of frame t from frames t-delay ..
//! t-delay-taps+1 and subtracts it, re-estimating the prediction filter
//! `iterations` times with the current estimate's power as weights.
//!
//! nara_wpe estimates one filter from the whole recording. Here the filter is
//! estimated per block of `block_frames` (60 s by default) so an hour-long
//! meeting needs ~100 MB rather than gigabytes; the previous block's last
//! `taps + delay - 1` input frames supply the prediction history, so blocks
//! join without gaps. Output samples are aligned with input samples: `finish`
//! returns exactly as many samples in total as were pushed.
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rayon::prelude::*;
use std::sync::Arc;

type C32 = Complex<f32>;
type C64 = Complex<f64>;

#[derive(Debug, Clone, Copy)]
pub struct WpeConfig {
    pub size: usize,
    pub shift: usize,
    pub taps: usize,
    pub delay: usize,
    pub iterations: usize,
    pub block_frames: usize,
}

impl WpeConfig {
    /// The settings qualified by listening on room-microphone meetings (48 kHz).
    pub fn room_48k() -> Self {
        Self { size: 1024, shift: 256, taps: 10, delay: 3, iterations: 3, block_frames: 60 * 48_000 / 256 }
    }
}

pub struct StreamingWpe {
    cfg: WpeConfig,
    bins: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    analysis: Vec<f32>,
    synthesis: Vec<f32>,
    /// Padded-input samples not yet consumed by a frame (starts with the fade-in pad).
    input: Vec<f32>,
    /// Spectra of frames waiting for their block, frame-major `[frame][bin]`.
    pending: Vec<C32>,
    /// Last `taps + delay - 1` input spectra of the previous block, frame-major.
    history: Vec<C32>,
    /// Overlap-add accumulator; index 0 is padded-sample `ola_start`.
    ola: Vec<f32>,
    ola_start: usize,
    frames_out: usize,
    /// Padded samples still to drop from the front of the output (the fade-in pad).
    skip: usize,
    pushed: usize,
    emitted: usize,
    scratch_time: Vec<f32>,
    scratch_freq: Vec<C32>,
}

impl StreamingWpe {
    pub fn new(cfg: WpeConfig) -> Self {
        assert!(cfg.size % cfg.shift == 0 && cfg.taps > 0 && cfg.block_frames > cfg.taps + cfg.delay);
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(cfg.size);
        let ifft = planner.plan_fft_inverse(cfg.size);
        // scipy.signal.windows.blackman(size + 1)[:-1]
        let analysis: Vec<f32> = (0..cfg.size)
            .map(|n| {
                let x = 2.0 * std::f64::consts::PI * n as f64 / cfg.size as f64;
                (0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos()) as f32
            })
            .collect();
        let synthesis = biorthogonal(&analysis, cfg.shift);
        let fade = cfg.size - cfg.shift;
        Self {
            bins: cfg.size / 2 + 1,
            fft,
            ifft,
            analysis,
            synthesis,
            input: vec![0.0; fade],
            pending: Vec::new(),
            history: Vec::new(),
            ola: Vec::new(),
            ola_start: 0,
            frames_out: 0,
            skip: fade,
            pushed: 0,
            emitted: 0,
            scratch_time: vec![0.0; cfg.size],
            scratch_freq: vec![C32::new(0.0, 0.0); cfg.size / 2 + 1],
            cfg,
        }
    }

    pub fn push(&mut self, samples: &[f32]) -> Vec<f32> {
        self.pushed += samples.len();
        self.input.extend_from_slice(samples);
        self.analyze_ready_frames();
        let mut out = Vec::new();
        while self.pending.len() / self.bins >= self.cfg.block_frames {
            let block: Vec<C32> = self.pending.drain(..self.cfg.block_frames * self.bins).collect();
            self.process_block(block, &mut out);
        }
        out
    }

    /// Flush the fade-out pad and the final block. Total output length equals
    /// total input length.
    pub fn finish(&mut self) -> Vec<f32> {
        let (size, shift) = (self.cfg.size, self.cfg.shift);
        // nara_wpe: fade-out pad, then zero-pad so the last frame is complete.
        let padded = self.pushed + 2 * (size - shift);
        let frames = if padded <= size { 1 } else { (padded - size).div_ceil(shift) + 1 };
        let consumed = self.frames_analyzed() * shift;
        let needed = (frames - 1) * shift + size;
        let total = consumed + self.input.len();
        self.input.extend(std::iter::repeat_n(0.0, needed.saturating_sub(total)));
        self.analyze_ready_frames();
        let mut out = Vec::new();
        let block = std::mem::take(&mut self.pending);
        if !block.is_empty() {
            self.process_block(block, &mut out);
        }
        // Everything up to the end of the last frame is now final.
        let end = self.ola_start + self.ola.len();
        self.emit_until(end, &mut out);
        let excess = self.emitted.saturating_sub(self.pushed);
        out.truncate(out.len().saturating_sub(excess));
        self.emitted -= excess;
        out
    }

    fn frames_analyzed(&self) -> usize {
        self.frames_out + self.pending.len() / self.bins
    }

    fn analyze_ready_frames(&mut self) {
        let (size, shift) = (self.cfg.size, self.cfg.shift);
        let mut offset = 0;
        while self.input.len() - offset >= size {
            for (dst, (x, w)) in self.scratch_time.iter_mut().zip(self.input[offset..offset + size].iter().zip(&self.analysis)) {
                *dst = x * w;
            }
            self.fft.process(&mut self.scratch_time, &mut self.scratch_freq).expect("fixed-size FFT");
            self.pending.extend_from_slice(&self.scratch_freq);
            offset += shift;
        }
        self.input.drain(..offset);
    }

    fn process_block(&mut self, block: Vec<C32>, out: &mut Vec<f32>) {
        let (bins, frames) = (self.bins, block.len() / self.bins);
        let hist_len = self.cfg.taps + self.cfg.delay - 1;
        let hist_frames = self.history.len() / bins;
        // Bin-major series: [history | block] per bin, so bins run in parallel.
        let len = hist_frames + frames;
        let mut series = vec![C32::new(0.0, 0.0); bins * len];
        for f in 0..bins {
            let row = &mut series[f * len..(f + 1) * len];
            for t in 0..hist_frames {
                row[t] = self.history[t * bins + f];
            }
            for t in 0..frames {
                row[hist_frames + t] = block[t * bins + f];
            }
        }
        let keep = hist_len.min(len);
        self.history.clear();
        for t in len - keep..len {
            for f in 0..bins {
                self.history.push(series[f * len + t]);
            }
        }
        let cfg = self.cfg;
        // nara_wpe floors the power at 1e-10 of its maximum over all bins and frames.
        let mut x: Vec<C32> = (0..bins).flat_map(|f| series[f * len + hist_frames..(f + 1) * len].to_vec()).collect();
        for _ in 0..cfg.iterations {
            let max_power = x.par_iter().map(|v| v.norm_sqr()).reduce(|| 0.0, f32::max) as f64;
            x.par_chunks_mut(frames).enumerate().for_each(|(f, xf)| {
                let y = &series[f * len..(f + 1) * len];
                dereverberate_bin(y, hist_frames, xf, cfg.taps, cfg.delay, max_power);
            });
        }
        // Back to frame-major, then overlap-add.
        let mut frame = vec![C32::new(0.0, 0.0); bins];
        for t in 0..frames {
            for f in 0..bins {
                frame[f] = x[f * frames + t];
            }
            self.synthesize(&mut frame, out);
        }
    }

    fn synthesize(&mut self, spectrum: &mut [C32], out: &mut Vec<f32>) {
        let (size, shift) = (self.cfg.size, self.cfg.shift);
        // numpy's irfft ignores the imaginary part of DC and Nyquist.
        spectrum[0].im = 0.0;
        spectrum[self.bins - 1].im = 0.0;
        self.ifft.process(spectrum, &mut self.scratch_time).expect("fixed-size inverse FFT");
        let start = self.frames_out * shift;
        let needed = start + size - self.ola_start;
        if self.ola.len() < needed {
            self.ola.resize(needed, 0.0);
        }
        let base = start - self.ola_start;
        let norm = 1.0 / size as f32;
        for n in 0..size {
            self.ola[base + n] += self.scratch_time[n] * norm * self.synthesis[n];
        }
        self.frames_out += 1;
        // Samples before the next frame's start receive no more contributions.
        self.emit_until(self.frames_out * shift, out);
    }

    fn emit_until(&mut self, padded_end: usize, out: &mut Vec<f32>) {
        let ready = padded_end.saturating_sub(self.ola_start).min(self.ola.len());
        let drop = self.skip.min(ready);
        self.skip -= drop;
        out.extend_from_slice(&self.ola[drop..ready]);
        self.emitted += ready - drop;
        self.ola.drain(..ready);
        self.ola_start += ready;
    }
}

/// One WPE iteration for one bin. `y` is `[history | block]`; `x` holds the
/// current estimate for the block frames and is replaced by the next one.
fn dereverberate_bin(y: &[C32], hist: usize, x: &mut [C32], taps: usize, delay: usize, max_power: f64) {
    let frames = x.len();
    let eps = 1e-10 * max_power;
    let tilde = |t: usize, k: usize| -> C64 {
        // Y[t - delay - k] in block coordinates, zero before the recording.
        let idx = (hist + t) as isize - (delay + k) as isize;
        if idx < 0 { C64::new(0.0, 0.0) } else { let v = y[idx as usize]; C64::new(v.re as f64, v.im as f64) }
    };
    let mut r = vec![C64::new(0.0, 0.0); taps * taps];
    let mut p = vec![C64::new(0.0, 0.0); taps];
    let mut yt = vec![C64::new(0.0, 0.0); taps];
    for t in 0..frames {
        let power = x[t].norm_sqr() as f64;
        let inv = if max_power == 0.0 { 1.0 } else { 1.0 / power.max(eps) };
        for (k, v) in yt.iter_mut().enumerate() {
            *v = tilde(t, k);
        }
        let cur = y[hist + t];
        let cur = C64::new(cur.re as f64, cur.im as f64).conj();
        for i in 0..taps {
            let a = yt[i] * inv;
            p[i] += a * cur;
            for j in i..taps {
                r[i * taps + j] += a * yt[j].conj();
            }
        }
    }
    for i in 0..taps {
        for j in 0..i {
            r[i * taps + j] = r[j * taps + i].conj();
        }
    }
    let g = solve_hermitian(&mut r, &p, taps);
    for t in 0..frames {
        let cur = y[hist + t];
        let mut v = C64::new(cur.re as f64, cur.im as f64);
        for (k, gk) in g.iter().enumerate() {
            v -= gk.conj() * tilde(t, k);
        }
        x[t] = C32::new(v.re as f32, v.im as f32);
    }
}

/// Solve R g = p for a Hermitian positive semi-definite R by Cholesky, adding
/// diagonal loading when R is singular (silent bins). Returns zeros (no
/// dereverberation for that bin) if it still fails.
fn solve_hermitian(r: &mut [C64], p: &[C64], n: usize) -> Vec<C64> {
    let trace: f64 = (0..n).map(|i| r[i * n + i].re).sum();
    for loading in [0.0, 1e-9, 1e-6] {
        let mut l = r.to_vec();
        for i in 0..n {
            l[i * n + i].re += loading * trace / n as f64 + if loading > 0.0 { f64::MIN_POSITIVE } else { 0.0 };
        }
        if let Some(g) = cholesky_solve(&mut l, p, n) {
            return g;
        }
    }
    vec![C64::new(0.0, 0.0); n]
}

fn cholesky_solve(a: &mut [C64], b: &[C64], n: usize) -> Option<Vec<C64>> {
    // In place: lower triangle becomes L with A = L L^H.
    for j in 0..n {
        let mut d = a[j * n + j].re;
        for k in 0..j {
            d -= a[j * n + k].norm_sqr();
        }
        if !(d > 0.0) || !d.is_finite() {
            return None;
        }
        let d = d.sqrt();
        a[j * n + j] = C64::new(d, 0.0);
        for i in j + 1..n {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= a[i * n + k] * a[j * n + k].conj();
            }
            a[i * n + j] = s / d;
        }
    }
    let mut z = vec![C64::new(0.0, 0.0); n];
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= a[i * n + k] * z[k];
        }
        z[i] = s / a[i * n + i].re;
    }
    let mut g = vec![C64::new(0.0, 0.0); n];
    for i in (0..n).rev() {
        let mut s = z[i];
        for k in i + 1..n {
            s -= a[k * n + i].conj() * g[k];
        }
        g[i] = s / a[i * n + i].re;
    }
    g.iter().all(|v| v.re.is_finite() && v.im.is_finite()).then_some(g)
}

/// nara_wpe `_biorthogonal_window_brute_force`: analysis / sum of shifted squares.
fn biorthogonal(analysis: &[f32], shift: usize) -> Vec<f32> {
    let size = analysis.len();
    let width = (size - 1) / shift;
    let mut denom = vec![0.0f64; size];
    for i in -(width as isize)..=(width as isize) {
        let s = i * shift as isize;
        for n in 0..size as isize {
            let m = n - s;
            if (0..size as isize).contains(&m) {
                denom[n as usize] += (analysis[m as usize] as f64).powi(2);
            }
        }
    }
    analysis.iter().zip(&denom).map(|(w, d)| (*w as f64 / d) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cfg: WpeConfig, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut wpe = StreamingWpe::new(cfg);
        let mut out = Vec::new();
        for piece in input.chunks(chunk) {
            out.extend(wpe.push(piece));
        }
        out.extend(wpe.finish());
        out
    }

    fn noise(len: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5
            })
            .collect()
    }

    #[test]
    fn stft_round_trip_is_exact_without_filtering() {
        // With zero taps contributing (iterations = 0) the output is the input.
        let cfg = WpeConfig { iterations: 0, block_frames: 50, ..WpeConfig::room_48k() };
        let input = noise(48_000 + 123, 1);
        for chunk in [1000, 4096, 50_000] {
            let out = run(cfg, &input, chunk);
            assert_eq!(out.len(), input.len());
            let err = input.iter().zip(&out).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(err < 1e-4, "chunk {chunk}: max error {err}");
        }
    }

    #[test]
    fn short_and_empty_inputs_keep_their_length() {
        let cfg = WpeConfig::room_48k();
        assert!(run(cfg, &[], 10).is_empty());
        assert_eq!(run(cfg, &noise(100, 2), 7).len(), 100);
    }

    #[test]
    fn removes_a_recursive_room_echo() {
        // Speech-like bursts through a feedback echo (40 ms, gain 0.7): a
        // linearly predictable tail, which is what WPE removes. Energy in the
        // 0.25-0.6 s after each burst ends is reverb only.
        let sr = 48_000;
        let burst = noise(sr / 5, 3);
        let mut wet = vec![0.0f32; sr * 12];
        let echo = sr / 25;
        for start in (0..wet.len() - sr).step_by(sr) {
            wet[start..start + burst.len()].copy_from_slice(&burst);
        }
        for n in echo..wet.len() {
            wet[n] += 0.7 * wet[n - echo];
        }
        let out = run(WpeConfig { block_frames: 1000, ..WpeConfig::room_48k() }, &wet, 9600);
        let late = |x: &[f32]| -> f64 {
            (0..11).map(|b| x[b * sr + sr / 4..b * sr + 3 * sr / 5].iter().map(|v| (*v as f64).powi(2)).sum::<f64>()).sum()
        };
        let reduction_db = 10.0 * (late(&wet) / late(&out)).log10();
        assert!(reduction_db > 10.0, "echo tail reduced by only {reduction_db:.1} dB");
    }

    #[test]
    fn silent_input_stays_silent_and_finite() {
        let out = run(WpeConfig { block_frames: 100, ..WpeConfig::room_48k() }, &vec![0.0; 30_000], 4000);
        assert!(out.iter().all(|v| *v == 0.0));
    }
}

#[cfg(test)]
mod reference {
    use super::*;

    /// Compare with nara_wpe (ignored; manual). Reads raw f32 LE mono 48 kHz
    /// from `MEETILY_WPE_IN`, writes `MEETILY_WPE_OUT`; `MEETILY_WPE_BLOCK`
    /// overrides the block length in frames (0 = whole input in one block).
    #[test]
    #[ignore]
    fn wpe_raw_file() {
        let input: Vec<f32> = std::fs::read(std::env::var("MEETILY_WPE_IN").unwrap())
            .unwrap()
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let block = std::env::var("MEETILY_WPE_BLOCK").ok().and_then(|v| v.parse().ok()).unwrap_or(WpeConfig::room_48k().block_frames);
        let block = if block == 0 { input.len() / 256 + 16 } else { block };
        let t = std::time::Instant::now();
        let mut wpe = StreamingWpe::new(WpeConfig { block_frames: block, ..WpeConfig::room_48k() });
        let mut out = Vec::new();
        for piece in input.chunks(48_000) {
            out.extend(wpe.push(piece));
        }
        out.extend(wpe.finish());
        println!("{} samples in {:.2} s", out.len(), t.elapsed().as_secs_f64());
        let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(std::env::var("MEETILY_WPE_OUT").unwrap(), bytes).unwrap();
    }
}
