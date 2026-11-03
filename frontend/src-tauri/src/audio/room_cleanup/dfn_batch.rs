//! DeepFilterNet 3 over whole blocks of frames instead of one 10 ms frame per
//! model call (issue #20).
//!
//! `StreamingDfn` runs libDF's pulsed tract graphs once per 480-sample hop.
//! Every matrix product then has a single column, and tract 0.19 computes it
//! with its 8x8 kernel, so most of the arithmetic is thrown away. Here the
//! same three ONNX graphs (encoder, ERB decoder, DF decoder) run with the time
//! axis fixed to a block of frames, while features, masking, deep filtering,
//! the attenuation limit and synthesis follow libDF's `DfTract::process`
//! frame for frame:
//!
//! - libDF's pulsed encoder sees features up to frame t and its outputs are
//!   applied to spectrum frame t - conv_lookahead. Deep filtering for that
//!   frame uses frames t - 4 ..= t (df_order 5, df_lookahead 2).
//! - Per frame, the encoder's local SNR selects the stages: zero mask below
//!   -15 dB, nothing above 35 dB, mask only above 35 dB (DF threshold), else
//!   mask plus deep filtering.
//! - The attenuation limit mixes the noisy spectrum back in before synthesis.
//!
//! The graphs' GRU states cannot be passed between blocks, so each block
//! starts `WARMUP_FRAMES` early and drops those outputs. Output sample n
//! corresponds to input sample n and the length is unchanged, like
//! `StreamingDfn`.
use anyhow::{anyhow, bail, Context, Result};
use df::{Complex32, DFState};
use std::io::Read;
use tract_core::prelude::*;
use tract_onnx::prelude::*;

use super::dfn::{ATTENUATION_LIMIT_DB, SAMPLE_RATE};

const FFT_SIZE: usize = 960;
const HOP: usize = 480;
const NB_ERB: usize = 32;
const NB_DF: usize = 96;
const MIN_NB_ERB_FREQS: usize = 2;
const DF_ORDER: usize = 5;
const DF_LOOKAHEAD: usize = 2;
const CONV_LOOKAHEAD: usize = 2;
const CONV_CH: usize = 64;
/// libDF's exponential feature normalization for tau = 1 s at a 10 ms hop.
const NORM_ALPHA: f32 = 0.99;
const MIN_DB_THRESH: f32 = -15.0;
const MAX_DB_ERB_THRESH: f32 = 35.0;
const MAX_DB_DF_THRESH: f32 = 35.0;
const N_FREQS: usize = FFT_SIZE / 2 + 1;

/// Frames run through the graphs per call (10 s).
pub const BLOCK_FRAMES: usize = 1000;
/// Frames of earlier context per block, discarded, so the GRUs settle.
pub const WARMUP_FRAMES: usize = 100;

/// The three DeepFilterNet 3 graphs from libDF's `DeepFilterNet3_onnx.tar.gz`.
pub struct DfnGraphs {
    enc: Vec<u8>,
    erb_dec: Vec<u8>,
    df_dec: Vec<u8>,
}

impl DfnGraphs {
    pub fn from_targz(bytes: &[u8]) -> Result<Self> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        let (mut enc, mut erb_dec, mut df_dec, mut config) = (Vec::new(), Vec::new(), Vec::new(), String::new());
        for entry in archive.entries().context("DeepFilterNet model archive")? {
            let mut entry = entry?;
            let path = entry.path()?.to_path_buf();
            if path.ends_with("enc.onnx") {
                entry.read_to_end(&mut enc)?;
            } else if path.ends_with("erb_dec.onnx") {
                entry.read_to_end(&mut erb_dec)?;
            } else if path.ends_with("df_dec.onnx") {
                entry.read_to_end(&mut df_dec)?;
            } else if path.ends_with("config.ini") {
                entry.read_to_string(&mut config)?;
            }
        }
        if enc.is_empty() || erb_dec.is_empty() || df_dec.is_empty() {
            bail!("DeepFilterNet model archive is missing a graph");
        }
        check_config(&config)?;
        Ok(Self { enc, erb_dec, df_dec })
    }
}

/// The constants above are this model's; refuse a different one.
fn check_config(config: &str) -> Result<()> {
    let expected = [
        ("sr", SAMPLE_RATE),
        ("fft_size", FFT_SIZE),
        ("hop_size", HOP),
        ("nb_erb", NB_ERB),
        ("nb_df", NB_DF),
        ("df_order", DF_ORDER),
        ("df_lookahead", DF_LOOKAHEAD),
        ("conv_lookahead", CONV_LOOKAHEAD),
        ("conv_ch", CONV_CH),
        ("min_nb_erb_freqs", MIN_NB_ERB_FREQS),
    ];
    for (key, value) in expected {
        let found = config.lines().find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_string())
        });
        if found.as_deref() != Some(value.to_string().as_str()) {
            bail!("DeepFilterNet config {key} is {found:?}, expected {value}");
        }
    }
    let model = config.lines().find_map(|l| {
        l.split_once('=')
            .filter(|(k, _)| k.trim() == "model")
            .map(|(_, v)| v.trim().to_string())
    });
    if model.as_deref() != Some("deepfilternet3") {
        bail!("DeepFilterNet model is {model:?}, expected deepfilternet3");
    }
    Ok(())
}

type Plan = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

fn plan(bytes: &[u8], inputs: &[(&str, [usize; 4])], outputs: &[&str], frames: usize) -> Result<Plan> {
    let mut model = tract_onnx::onnx()
        .with_ignore_output_shapes(true)
        .model_for_read(&mut std::io::Cursor::new(bytes))?;
    for (index, (name, shape)) in inputs.iter().enumerate() {
        let shape: Vec<usize> = shape
            .iter()
            .map(|&d| if d == usize::MAX { frames } else { d })
            .filter(|&d| d != 0)
            .collect();
        model = model.with_input_fact(index, InferenceFact::dt_shape(f32::datum_type(), &shape))?;
        let _ = name;
    }
    let names: Vec<String> = inputs.iter().map(|(n, _)| n.to_string()).collect();
    model = model.with_input_names(&names)?;
    if !outputs.is_empty() {
        model = model.with_output_names(outputs)?;
    }
    Ok(model.into_optimized()?.into_runnable()?)
}

/// Graphs compiled for one block length.
pub struct BatchedDfn {
    enc: Plan,
    erb_dec: Plan,
    df_dec: Plan,
    frames: usize,
}

/// `usize::MAX` marks the time axis and 0 an absent axis in `plan` shapes.
const T: usize = usize::MAX;

impl BatchedDfn {
    pub fn new(graphs: &DfnGraphs) -> Result<Self> {
        let frames = BLOCK_FRAMES + WARMUP_FRAMES;
        let hidden = CONV_CH * NB_ERB / 4;
        let enc = plan(
            &graphs.enc,
            &[("feat_erb", [1, 1, T, NB_ERB]), ("feat_spec", [1, 2, T, NB_DF])],
            &["e0", "e1", "e2", "e3", "emb", "c0", "lsnr"],
            frames,
        )?;
        let erb_dec = plan(
            &graphs.erb_dec,
            &[
                ("emb", [1, T, hidden, 0]),
                ("e3", [1, CONV_CH, T, NB_ERB / 4]),
                ("e2", [1, CONV_CH, T, NB_ERB / 4]),
                ("e1", [1, CONV_CH, T, NB_ERB / 2]),
                ("e0", [1, CONV_CH, T, NB_ERB]),
            ],
            &[],
            frames,
        )?;
        let df_dec = plan(
            &graphs.df_dec,
            &[("emb", [1, T, hidden, 0]), ("c0", [1, CONV_CH, T, NB_DF])],
            &["coefs"],
            frames,
        )?;
        Ok(Self {
            enc,
            erb_dec,
            df_dec,
            frames,
        })
    }

    pub fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        enhance(self, input)
    }
}

/// Raw model outputs for `n` frames starting at `first`, from features.
pub trait BlockModel {
    /// Longest block the model accepts.
    fn max_frames(&self) -> usize;
    /// Returns per-frame lsnr `[n]`, ERB gains `[n, NB_ERB]` and DF coefs
    /// `[n, NB_DF, DF_ORDER, 2]` (real/imaginary), all row-major.
    fn run(&mut self, erb: &[f32], cplx: &[f32], n: usize) -> Result<(Vec<f32>, Vec<f32>, Vec<f32>)>;
}

/// Feature planes for frames `first..first + n`: ERB `[n, NB_ERB]` and the
/// complex features as `[2, n, NB_DF]` (real plane, then imaginary).
fn block_features(feat_erb: &[f32], feat_cplx: &[Complex32], first: usize, n: usize) -> (Vec<f32>, Vec<f32>) {
    let erb = feat_erb[first * NB_ERB..(first + n) * NB_ERB].to_vec();
    let mut cplx = vec![0f32; 2 * n * NB_DF];
    for t in 0..n {
        for f in 0..NB_DF {
            let v = feat_cplx[(first + t) * NB_DF + f];
            cplx[t * NB_DF + f] = v.re;
            cplx[n * NB_DF + t * NB_DF + f] = v.im;
        }
    }
    (erb, cplx)
}

/// Enhance a whole 48 kHz mono signal with `model`.
pub fn enhance(model: &mut dyn BlockModel, input: &[f32]) -> Result<Vec<f32>> {
    enhance_with_snr(model, input).map(|(out, _)| out)
}

/// `enhance`, also returning the encoder's local SNR per frame step.
pub fn enhance_with_snr(model: &mut dyn BlockModel, input: &[f32]) -> Result<(Vec<f32>, Vec<f32>)> {
    let total_frames = input.len().div_ceil(HOP);
    // Spectra and features for every frame, sequentially (cheap), exactly
    // as the streaming state would compute them.
    let mut state = DFState::new(SAMPLE_RATE, FFT_SIZE, HOP, NB_ERB, MIN_NB_ERB_FREQS);
    state.init_norm_states(NB_DF);
    let mut spec = vec![Complex32::default(); total_frames * N_FREQS];
    let mut feat_erb = vec![0f32; total_frames * NB_ERB];
    let mut feat_cplx = vec![Complex32::default(); total_frames * NB_DF];
    let mut frame = vec![0f32; HOP];
    for t in 0..total_frames {
        let start = t * HOP;
        let end = (start + HOP).min(input.len());
        frame.fill(0.0);
        for (dst, src) in frame.iter_mut().zip(&input[start..end]) {
            *dst = src.clamp(-0.9999, 0.9999);
        }
        let s = &mut spec[t * N_FREQS..(t + 1) * N_FREQS];
        state.analysis(&frame, s);
        state.feat_erb(s, NORM_ALPHA, &mut feat_erb[t * NB_ERB..(t + 1) * NB_ERB]);
        state.feat_cplx(&s[..NB_DF], NORM_ALPHA, &mut feat_cplx[t * NB_DF..(t + 1) * NB_DF]);
    }

    // Model outputs per frame t (the encoder having seen features up to t).
    let mut lsnr = vec![0f32; total_frames];
    let mut gains = vec![0f32; total_frames * NB_ERB];
    let mut coefs = vec![Complex32::default(); total_frames * NB_DF * DF_ORDER];
    let mut block_start = 0;
    while block_start < total_frames {
        let first = block_start.saturating_sub(WARMUP_FRAMES);
        let keep_from = block_start - first;
        let n = (total_frames - first).min(model.max_frames());
        let (erb, cplx) = block_features(&feat_erb, &feat_cplx, first, n);
        let (snr, mask, df) = model.run(&erb, &cplx, n)?;
        if snr.len() != n || mask.len() != n * NB_ERB || df.len() != n * NB_DF * DF_ORDER * 2 {
            bail!(
                "DeepFilterNet block outputs have unexpected sizes: {} {} {}",
                snr.len(),
                mask.len(),
                df.len()
            );
        }
        for t in keep_from..n {
            let g = first + t;
            lsnr[g] = snr[t];
            gains[g * NB_ERB..(g + 1) * NB_ERB].copy_from_slice(&mask[t * NB_ERB..(t + 1) * NB_ERB]);
            for i in 0..NB_DF * DF_ORDER {
                let k = t * NB_DF * DF_ORDER * 2 + i * 2;
                coefs[g * NB_DF * DF_ORDER + i] = Complex32::new(df[k], df[k + 1]);
            }
        }
        block_start = first + n;
    }

    // Mask, deep filtering, attenuation limit and synthesis, frame by frame
    // as in DfTract::process: step t enhances spectrum frame t - 2.
    let lim = 10f32.powf(-ATTENUATION_LIMIT_DB / 20.0);
    let delay = CONV_LOOKAHEAD.max(DF_LOOKAHEAD);
    let zero = vec![Complex32::default(); N_FREQS];
    let frame_at = |i: isize| -> &[Complex32] {
        if i < 0 || i as usize >= total_frames {
            &zero
        } else {
            &spec[i as usize * N_FREQS..(i as usize + 1) * N_FREQS]
        }
    };
    let mut out = Vec::with_capacity((total_frames + delay) * HOP);
    let mut enh = vec![Complex32::default(); N_FREQS];
    let mut hop_out = vec![0f32; HOP];
    // Steps past the end flush the lookahead, like StreamingDfn::finish.
    for step in 0..total_frames + delay {
        let target = step as isize - delay as isize;
        let noisy = frame_at(target);
        if step >= total_frames {
            // StreamingDfn::finish feeds zeros here; libDF returns silence
            // for a silent hop without running synthesis.
            out.extend(std::iter::repeat_n(0.0, HOP));
            continue;
        }
        {
            let snr = lsnr[step];
            let (apply_gains, zero_gains, apply_df) = if snr < MIN_DB_THRESH {
                (false, true, false)
            } else if snr > MAX_DB_ERB_THRESH {
                (false, false, false)
            } else if snr > MAX_DB_DF_THRESH {
                (true, false, false)
            } else {
                (true, false, true)
            };
            enh.copy_from_slice(noisy);
            if apply_gains {
                state.apply_mask(&mut enh, &gains[step * NB_ERB..(step + 1) * NB_ERB]);
            } else if zero_gains {
                state.apply_mask(&mut enh, &[0f32; NB_ERB]);
            }
            if apply_df {
                enh[..NB_DF].fill(Complex32::default());
                let c = &coefs[step * NB_DF * DF_ORDER..(step + 1) * NB_DF * DF_ORDER];
                for order in 0..DF_ORDER {
                    let source = frame_at(step as isize - (DF_ORDER - 1 - order) as isize);
                    for f in 0..NB_DF {
                        enh[f] += source[f] * c[f * DF_ORDER + order];
                    }
                }
            }
        }
        for (e, n) in enh.iter_mut().zip(noisy) {
            *e = *e * (1.0 - lim) + *n * lim;
        }
        state.synthesis(&mut enh, &mut hop_out);
        out.extend_from_slice(&hop_out);
    }
    // Drop the STFT delay plus the lookahead delay, then trim to length.
    let skip = FFT_SIZE - HOP + delay * HOP;
    let mut out = out.split_off(skip.min(out.len()));
    out.resize(input.len(), 0.0);
    Ok((out, lsnr))
}

impl BlockModel for BatchedDfn {
    fn max_frames(&self) -> usize {
        self.frames
    }

    fn run(&mut self, erb_in: &[f32], cplx_in: &[f32], n: usize) -> Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        let frames = self.frames;
        // The plans are compiled for `frames`; pad the block with zeros.
        let mut erb = tract_ndarray::Array4::<f32>::zeros((1, 1, frames, NB_ERB));
        let mut cplx = tract_ndarray::Array4::<f32>::zeros((1, 2, frames, NB_DF));
        for t in 0..n {
            for b in 0..NB_ERB {
                erb[[0, 0, t, b]] = erb_in[t * NB_ERB + b];
            }
            for f in 0..NB_DF {
                cplx[[0, 0, t, f]] = cplx_in[t * NB_DF + f];
                cplx[[0, 1, t, f]] = cplx_in[n * NB_DF + t * NB_DF + f];
            }
        }
        let mut enc = self.enc.run(tvec!(erb.into_tensor().into(), cplx.into_tensor().into()))?;
        let snr = enc.pop().ok_or_else(|| anyhow!("encoder lsnr"))?;
        let c0 = enc.pop().ok_or_else(|| anyhow!("encoder c0"))?;
        let emb = enc.pop().ok_or_else(|| anyhow!("encoder emb"))?;
        let e3 = enc.pop().ok_or_else(|| anyhow!("encoder e3"))?;
        let e2 = enc.pop().ok_or_else(|| anyhow!("encoder e2"))?;
        let e1 = enc.pop().ok_or_else(|| anyhow!("encoder e1"))?;
        let e0 = enc.pop().ok_or_else(|| anyhow!("encoder e0"))?;
        let mask = self.erb_dec.run(tvec!(emb.clone(), e3, e2, e1, e0))?.remove(0);
        let df = self.df_dec.run(tvec!(emb, c0))?.remove(0);
        Ok((
            snr.as_slice::<f32>()?[..n].to_vec(),
            mask.as_slice::<f32>()?[..n * NB_ERB].to_vec(),
            df.as_slice::<f32>()?[..n * NB_DF * DF_ORDER * 2].to_vec(),
        ))
    }
}

/// The same graphs on ONNX Runtime (CPU), whose GRU and depthwise convolution
/// kernels handle a long time axis well; tract 0.19 steps its GRU frame by
/// frame with the same single-column products as streaming.
pub struct OrtDfn {
    enc: ort::session::Session,
    erb_dec: ort::session::Session,
    df_dec: ort::session::Session,
}

impl OrtDfn {
    pub fn new(graphs: &DfnGraphs, threads: usize) -> Result<Self> {
        use ort::session::{builder::GraphOptimizationLevel, Session};
        let session = |bytes: &[u8]| -> Result<Session> {
            Ok(Session::builder()?
                .with_intra_threads(threads)?
                .with_inter_threads(1)?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .commit_from_memory(bytes)?)
        };
        Ok(Self {
            enc: session(&graphs.enc)?,
            erb_dec: session(&graphs.erb_dec)?,
            df_dec: session(&graphs.df_dec)?,
        })
    }
}

impl BlockModel for OrtDfn {
    fn max_frames(&self) -> usize {
        BLOCK_FRAMES + WARMUP_FRAMES
    }

    fn run(&mut self, erb: &[f32], cplx: &[f32], n: usize) -> Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        use ort::{inputs, value::TensorRef};
        let erb = ndarray::ArrayView4::from_shape((1, 1, n, NB_ERB), erb)?;
        let cplx = ndarray::ArrayView4::from_shape((1, 2, n, NB_DF), cplx)?;
        let enc = self.enc.run(inputs![
            "feat_erb" => TensorRef::from_array_view(erb)?,
            "feat_spec" => TensorRef::from_array_view(cplx)?,
        ])?;
        let get = |name: &str| -> Result<ndarray::ArrayD<f32>> {
            Ok(enc
                .get(name)
                .ok_or_else(|| anyhow!("encoder output {name} missing"))?
                .try_extract_array::<f32>()?
                .to_owned())
        };
        let (e0, e1, e2, e3, emb, c0, snr) = (
            get("e0")?,
            get("e1")?,
            get("e2")?,
            get("e3")?,
            get("emb")?,
            get("c0")?,
            get("lsnr")?,
        );
        drop(enc);
        let mask = {
            let out = self.erb_dec.run(inputs![
                "emb" => TensorRef::from_array_view(emb.view())?,
                "e3" => TensorRef::from_array_view(e3.view())?,
                "e2" => TensorRef::from_array_view(e2.view())?,
                "e1" => TensorRef::from_array_view(e1.view())?,
                "e0" => TensorRef::from_array_view(e0.view())?,
            ])?;
            out[0].try_extract_array::<f32>()?.iter().copied().collect::<Vec<f32>>()
        };
        let df = {
            let out = self.df_dec.run(inputs![
                "emb" => TensorRef::from_array_view(emb.view())?,
                "c0" => TensorRef::from_array_view(c0.view())?,
            ])?;
            out[0].try_extract_array::<f32>()?.iter().copied().collect::<Vec<f32>>()
        };
        Ok((snr.iter().copied().collect(), mask, df))
    }
}
