//! CoreML conversion of Parakeet TDT 0.6B v3 (`FluidInference/parakeet-tdt-0.6b-v3-coreml`,
//! CC-BY-4.0), pinned by revision, length and SHA-256. Same checkpoint and
//! token ids as the ONNX export (8192 tokens, blank = 8192), so Meetily's
//! vocabulary, glossary boosting and detokenizer are shared.
//!
//! Four fixed-shape models: Preprocessor (15 s of 16 kHz audio -> 1501 mel
//! frames), Encoder (-> 188 frames x 1024), Decoder (one prediction-network
//! LSTM step) and JointDecisionv3 (token argmax, duration bin and the top 64
//! token logits for one encoder frame). The TDT loop lives in `model.rs`.
use crate::coreml::{Input, Model, Output, COMPUTE_UNITS_CPU, COMPUTE_UNITS_CPU_AND_NEURAL_ENGINE};
use anyhow::{ensure, Result};
use std::path::{Path, PathBuf};

/// Audio the preprocessor accepts per window (15 s at 16 kHz).
pub const WINDOW_SAMPLES: usize = 240_000;
/// Audio samples per 80 ms encoder frame.
pub const SAMPLES_PER_FRAME: usize = 1280;
const MEL_BINS: usize = 128;
const MEL_FRAMES: usize = 1501;
const ENCODER_DIM: usize = 1024;
const ENCODER_FRAMES: usize = 188;
const DECODER_DIM: usize = 640;
const LSTM_LAYERS: usize = 2;
pub const TOP_K: usize = 64;

pub const REVISION: &str = "7dd20fe6b1797d35f5e3307e8b1732d9a178edfe";
pub const REPO: &str = "FluidInference/parakeet-tdt-0.6b-v3-coreml";
/// Folder under the Parakeet models directory.
pub const DIR_NAME: &str = "parakeet-tdt-0.6b-v3-coreml";
/// (relative path, bytes, sha256). README.md carries the license notice.
pub const ASSETS: [(&str, u64, &str); 21] = [
    ("README.md", 4285, "4155baa7d7d6f0ada748e1db3c1b91d18c9f7eb7155efc1abfbbe9d4a6507712"),
    ("Preprocessor.mlmodelc/analytics/coremldata.bin", 243, "c9beeb989c8d66f8be11df59bc6df277ec76cee404f6865b46243835ef562f6d"),
    ("Preprocessor.mlmodelc/coremldata.bin", 486, "dbde3f2300842c1fd51ef3ff948a0bcffe65ffd2dca10707f2509f32c1d65b1d"),
    ("Preprocessor.mlmodelc/metadata.json", 2841, "2a98699e22d279dd37fa1d238aeb1c6db1df0d6fad687775324157689d8f3acf"),
    ("Preprocessor.mlmodelc/model.mil", 28181, "4b8518a956450fec57f06c2a21bdffc26973f7f1fa6842fb38fe917f896b6b93"),
    ("Preprocessor.mlmodelc/weights/weight.bin", 491_072, "129b76e3aeafa8afa3ea76d995b964b145fe83700d579f6ff42c4c38fa0968ea"),
    ("Encoder.mlmodelc/analytics/coremldata.bin", 243, "42e638870d73f26b332918a3496ce36793fbb413a81cbd3d16ba01328637a105"),
    ("Encoder.mlmodelc/coremldata.bin", 485, "d48034a167a82e88fc3df64f60af963ab3983538271175b8319e7d5720a0fb86"),
    ("Encoder.mlmodelc/metadata.json", 2921, "da24da9cca943fb29d7fa8e376d57fca7cb3aa08ca51b956b0b0e56813f087e9"),
    ("Encoder.mlmodelc/model.mil", 959_769, "ed7b19156ca29fa7dfd6891deb9fda4b0e8893f68597c985d135736546a43808"),
    ("Encoder.mlmodelc/weights/weight.bin", 445_187_200, "e2020f323703477a5b21d7c2d282c403e371afb5962e79877e3033e73ba6f421"),
    ("Decoder.mlmodelc/analytics/coremldata.bin", 243, "4238c4e81ecd0dc94bd7dfbb60f7e2cc824107c1ffe0387b8607b72833dba350"),
    ("Decoder.mlmodelc/coremldata.bin", 554, "18647af085d87bd8f3121c8a9b4d4564c1ede038dab63d295b4e745cf2d7fb99"),
    ("Decoder.mlmodelc/metadata.json", 3427, "a39e93cd8371b8ded92635c7804fcd0590f0d1dd9415c6d19a0484be073077d9"),
    ("Decoder.mlmodelc/model.mil", 13110, "ef2a0a281695398a62fde86ac269c68f73d5b578d7ed3b31f2ba91a2d1ea1f35"),
    ("Decoder.mlmodelc/weights/weight.bin", 23_604_992, "48adf0f0d47c406c8253d4f7fef967436a39da14f5a65e66d5a4b407be355d41"),
    ("JointDecisionv3.mlmodelc/analytics/coremldata.bin", 243, "26def4bf73dd56d29dee21c8ef97cb8969e62f6120ed1adc91e46828e2737b6c"),
    ("JointDecisionv3.mlmodelc/coremldata.bin", 521, "f5fc08b741400f0088492c9e839418b1e18522f19cba28d361dd030c5f398342"),
    ("JointDecisionv3.mlmodelc/metadata.json", 3453, "d9307211b9a37e0f0ac260c7660b1571a3de25841035cfdf9b58fd40425f890f"),
    ("JointDecisionv3.mlmodelc/model.mil", 11775, "be60732943389a047175111a83f8839f3eb39d4803adafa828a0871b2f39818d"),
    ("JointDecisionv3.mlmodelc/weights/weight.bin", 12_642_764, "4e0e63d840032f7f07ddb1d64446051166281e5491bf22da8a945c41f6eedb3e"),
];

/// Where the conversion lives for the ONNX model directory `onnx_dir`
/// (`models/parakeet/<model>` -> `models/parakeet/parakeet-tdt-0.6b-v3-coreml`).
pub fn dir_for(onnx_dir: &Path) -> PathBuf {
    onnx_dir.parent().map(|p| p.join(DIR_NAME)).unwrap_or_else(|| PathBuf::from(DIR_NAME))
}

/// Every pinned file is present at its published length (the download hashes
/// before renaming into place; `verify` hashes again once per process).
pub fn available(dir: &Path) -> bool {
    cfg!(target_os = "macos") && ASSETS.iter().all(|(p, size, _)| dir.join(p).metadata().is_ok_and(|m| m.is_file() && m.len() == *size))
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verify the files before passing them to native code, once per process while
/// a file's length and modification time are unchanged.
pub fn verify(dir: &Path) -> Result<()> {
    use std::sync::Mutex;
    static VERIFIED: Mutex<Vec<(PathBuf, u64, std::time::SystemTime)>> = Mutex::new(Vec::new());
    for (relative, size, sha) in ASSETS {
        let path = dir.join(relative);
        let meta = path.metadata()?;
        ensure!(meta.len() == size, "Incomplete Parakeet CoreML file {relative}");
        let key = (path.clone(), meta.len(), meta.modified()?);
        if VERIFIED.lock().map(|v| v.contains(&key)).unwrap_or(false) { continue; }
        ensure!(sha256_file(&path)? == sha, "Parakeet CoreML file {relative} failed its checksum");
        if let Ok(mut verified) = VERIFIED.lock() { verified.push(key); }
    }
    Ok(())
}

/// How the model is told where the audio ends inside the padded window. The
/// default (all false) is the normal path; the others are FluidAudio's
/// recovery ladder for windows that decode to nothing although they carry
/// speech (FluidAudio #909: v3 has input cuts on which the joint never beats
/// blank, and the output is on a knife edge with respect to the length inputs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LengthPolicy {
    /// Declare all mel frames valid to the encoder.
    pub encoder_full: bool,
    /// Declare the whole padded window valid to the preprocessor.
    pub preprocessor_full: bool,
    /// Declare the audio 0.2 s shorter and silence that tail.
    pub trimmed_tail: bool,
}

impl LengthPolicy {
    const fn new(encoder_full: bool, preprocessor_full: bool, trimmed_tail: bool) -> Self {
        Self { encoder_full, preprocessor_full, trimmed_tail }
    }
    /// The order FluidAudio tries them; trimming costs the final 0.2 s, so last.
    pub const RECOVERY: [Self; 5] = [
        Self::new(true, false, false),
        Self::new(false, true, false),
        Self::new(false, false, true),
        Self::new(true, false, true),
        Self::new(false, true, true),
    ];
}

/// Recovery thresholds (FluidAudio): at least 2 s of audio above about
/// -50 dBFS, and a recovered decode of 2+ tokens at mean confidence >= 0.7.
pub const RECOVERY_MIN_SAMPLES: usize = 2 * 16000;
pub const RECOVERY_MIN_RMS: f64 = 0.003;
pub const RECOVERY_MIN_TOKENS: usize = 2;
pub const RECOVERY_MIN_CONFIDENCE: f32 = 0.7;
const TRIMMED_TAIL_SAMPLES: usize = 16000 / 5;

/// Whether an empty decode of `samples` deserves the recovery ladder.
pub fn should_recover_empty(samples: &[f32]) -> bool {
    samples.len() >= RECOVERY_MIN_SAMPLES
        && (samples.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / samples.len() as f64).sqrt() >= RECOVERY_MIN_RMS
}

/// Whether a recovered decode is credible enough to replace an empty one.
pub fn recovery_is_credible(confidences: &[f32]) -> bool {
    confidences.len() >= RECOVERY_MIN_TOKENS
        && confidences.iter().sum::<f32>() / confidences.len() as f32 >= RECOVERY_MIN_CONFIDENCE
}

/// One Joint decision: argmax token, its probability, duration bin, and the
/// top-K token logits.
pub struct Decision {
    pub token: i32,
    pub probability: f32,
    pub duration_bin: usize,
    pub top_ids: [i32; TOP_K],
    pub top_logits: [f32; TOP_K],
}

/// Prediction-network LSTM state and its latest projection.
#[derive(Clone)]
pub struct DecoderState {
    h: Vec<f32>,
    c: Vec<f32>,
    pub output: Vec<f32>,
}

pub struct ParakeetCoreMl {
    preprocessor: Model,
    encoder: Model,
    decoder: Model,
    joint: Model,
}

impl ParakeetCoreMl {
    pub fn load(dir: &Path) -> Result<Self> {
        verify(dir)?;
        // FluidAudio's defaults for this conversion: the preprocessor (an STFT)
        // on the CPU, the rest on CPU + Neural Engine without GPU dispatch.
        Ok(Self {
            preprocessor: Model::load(&dir.join("Preprocessor.mlmodelc"), COMPUTE_UNITS_CPU)?,
            encoder: Model::load(&dir.join("Encoder.mlmodelc"), COMPUTE_UNITS_CPU_AND_NEURAL_ENGINE)?,
            decoder: Model::load(&dir.join("Decoder.mlmodelc"), COMPUTE_UNITS_CPU_AND_NEURAL_ENGINE)?,
            joint: Model::load(&dir.join("JointDecisionv3.mlmodelc"), COMPUTE_UNITS_CPU_AND_NEURAL_ENGINE)?,
        })
    }

    /// Encode up to one window. The declared length is frame-aligned, as the
    /// conversion expects. Returns encoder output `[ENCODER_DIM, ENCODER_FRAMES]`
    /// (channel-major) and the number of valid frames.
    pub fn encode(&mut self, samples: &[f32], policy: LengthPolicy) -> Result<(Vec<f32>, usize)> {
        ensure!(samples.len() <= WINDOW_SAMPLES, "Parakeet CoreML window holds at most 15 s");
        let aligned = samples.len().div_ceil(SAMPLES_PER_FRAME) * SAMPLES_PER_FRAME;
        let aligned = aligned.min(WINDOW_SAMPLES);
        let mut audio = vec![0.0f32; WINDOW_SAMPLES];
        audio[..samples.len()].copy_from_slice(samples);
        let mut declared = aligned;
        if policy.trimmed_tail {
            let trimmed = RECOVERY_MIN_SAMPLES.max(aligned.saturating_sub(TRIMMED_TAIL_SAMPLES)) / SAMPLES_PER_FRAME * SAMPLES_PER_FRAME;
            if trimmed < aligned { audio[trimmed..aligned].fill(0.0); }
            declared = trimmed;
        }
        if policy.preprocessor_full { declared = WINDOW_SAMPLES; }
        let mut mel = vec![0.0f32; MEL_BINS * MEL_FRAMES];
        let mut mel_length = [0i32; 1];
        self.preprocessor.predict(
            &[("audio_signal", &[1, WINDOW_SAMPLES], Input::F32(&audio)), ("audio_length", &[1], Input::I32(&[declared as i32]))],
            &mut [("mel", Output::F32(&mut mel)), ("mel_length", Output::I32(&mut mel_length))],
        )?;
        if policy.encoder_full { mel_length[0] = MEL_FRAMES as i32; }
        let mut encoded = vec![0.0f32; ENCODER_DIM * ENCODER_FRAMES];
        let mut encoded_length = [0i32; 1];
        self.encoder.predict(
            &[("mel", &[1, MEL_BINS, MEL_FRAMES], Input::F32(&mel)), ("mel_length", &[1], Input::I32(&mel_length))],
            &mut [("encoder", Output::F32(&mut encoded)), ("encoder_length", Output::I32(&mut encoded_length))],
        )?;
        ensure!(encoded.iter().all(|v| v.is_finite()), "Non-finite Parakeet CoreML encoder output");
        // Valid frames: the encoder's length, never more than the audio covers
        // (also under a recovery policy: the decoder still stops at real frames).
        let frames = (encoded_length[0].max(0) as usize).min(aligned / SAMPLES_PER_FRAME).min(ENCODER_FRAMES);
        Ok((encoded, frames))
    }

    pub fn initial_state() -> DecoderState {
        DecoderState { h: vec![0.0; LSTM_LAYERS * DECODER_DIM], c: vec![0.0; LSTM_LAYERS * DECODER_DIM], output: vec![0.0; DECODER_DIM] }
    }

    /// Advance the prediction network by `token` (blank primes it as SOS).
    pub fn decoder_step(&mut self, token: i32, state: &DecoderState) -> Result<DecoderState> {
        let mut next = DecoderState { h: vec![0.0; state.h.len()], c: vec![0.0; state.c.len()], output: vec![0.0; DECODER_DIM] };
        self.decoder.predict(
            &[
                ("targets", &[1, 1], Input::I32(&[token])),
                ("target_length", &[1], Input::I32(&[1])),
                ("h_in", &[LSTM_LAYERS, 1, DECODER_DIM], Input::F32(&state.h)),
                ("c_in", &[LSTM_LAYERS, 1, DECODER_DIM], Input::F32(&state.c)),
            ],
            &mut [("decoder", Output::F32(&mut next.output)), ("h_out", Output::F32(&mut next.h)), ("c_out", Output::F32(&mut next.c))],
        )?;
        Ok(next)
    }

    /// Joint decision for encoder frame `t` of `encoded` (channel-major).
    pub fn joint(&mut self, encoded: &[f32], t: usize, decoder_output: &[f32]) -> Result<Decision> {
        let frame: Vec<f32> = (0..ENCODER_DIM).map(|d| encoded[d * ENCODER_FRAMES + t]).collect();
        let (mut token, mut duration, mut probability) = ([0i32; 1], [0i32; 1], [0f32; 1]);
        let mut top_ids = [0i32; TOP_K];
        let mut top_logits = [0f32; TOP_K];
        self.joint.predict(
            &[("encoder_step", &[1, ENCODER_DIM, 1], Input::F32(&frame)), ("decoder_step", &[1, DECODER_DIM, 1], Input::F32(decoder_output))],
            &mut [
                ("token_id", Output::I32(&mut token)),
                ("token_prob", Output::F32(&mut probability)),
                ("duration", Output::I32(&mut duration)),
                ("top_k_ids", Output::I32(&mut top_ids)),
                ("top_k_logits", Output::F32(&mut top_logits)),
            ],
        )?;
        Ok(Decision { token: token[0], probability: probability[0].clamp(0.0, 1.0), duration_bin: duration[0].max(0) as usize, top_ids, top_logits })
    }
}

/// Split audio longer than one window at its quietest 100 ms near each window
/// end (searching the last 3 s), so a cut rarely lands inside a word.
pub fn split_windows(samples: &[f32]) -> Vec<std::ops::Range<usize>> {
    const STEP: usize = 1600; // 100 ms
    const SEARCH: usize = 48_000; // 3 s
    let mut ranges = Vec::new();
    let mut start = 0;
    while samples.len() - start > WINDOW_SAMPLES {
        let limit = start + WINDOW_SAMPLES;
        let mut best = (f32::INFINITY, limit);
        let mut at = limit.saturating_sub(SEARCH).max(start + STEP);
        while at + STEP <= limit {
            let energy: f32 = samples[at..at + STEP].iter().map(|v| v * v).sum();
            if energy < best.0 { best = (energy, at + STEP / 2); }
            at += STEP / 2;
        }
        ranges.push(start..best.1);
        start = best.1;
    }
    ranges.push(start..samples.len());
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_decode_recovery_needs_speech_and_credible_tokens() {
        assert!(!should_recover_empty(&vec![0.1; RECOVERY_MIN_SAMPLES - 1]), "too short");
        assert!(!should_recover_empty(&vec![0.001; RECOVERY_MIN_SAMPLES]), "silence");
        assert!(should_recover_empty(&vec![0.01; RECOVERY_MIN_SAMPLES]));
        assert!(!recovery_is_credible(&[0.95]), "one token");
        assert!(!recovery_is_credible(&[0.6, 0.7]), "low confidence");
        assert!(recovery_is_credible(&[0.9, 0.8]));
        assert_eq!(LengthPolicy::RECOVERY.len(), 5);
        assert!(!LengthPolicy::RECOVERY.contains(&LengthPolicy::default()));
    }

    #[test]
    fn long_audio_splits_at_quiet_points_within_windows() {
        let mut audio = vec![0.5f32; 40 * 16000];
        // Quiet 200 ms at 13.5 s and 27 s.
        for i in 216_000..219_200 { audio[i] = 0.0; }
        for i in 432_000..435_200 { audio[i] = 0.0; }
        let ranges = split_windows(&audio);
        assert!(ranges.iter().all(|r| r.len() <= WINDOW_SAMPLES), "{ranges:?}");
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, audio.len());
        assert!(ranges.windows(2).all(|w| w[0].end == w[1].start));
        assert!((216_000..219_200).contains(&ranges[0].end), "{ranges:?}");
        assert_eq!(split_windows(&audio[..WINDOW_SAMPLES]), vec![0..WINDOW_SAMPLES]);
    }

    #[test]
    fn assets_are_pinned_and_cover_all_four_models() {
        for (path, size, sha) in ASSETS {
            assert!(size > 0 && sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()), "{path}");
            assert!(!path.starts_with('/') && !path.contains(".."), "{path}");
        }
        for model in ["Preprocessor", "Encoder", "Decoder", "JointDecisionv3"] {
            for file in ["coremldata.bin", "model.mil", "metadata.json", "weights/weight.bin", "analytics/coremldata.bin"] {
                let wanted = format!("{model}.mlmodelc/{file}");
                assert!(ASSETS.iter().any(|(p, _, _)| *p == wanted), "missing {wanted}");
            }
        }
        assert_eq!(REVISION.len(), 40);
        assert_eq!(dir_for(Path::new("/m/parakeet/parakeet-tdt-0.6b-v3-int8")), Path::new("/m/parakeet").join(DIR_NAME));
    }
}

fn assets_in(dir: &Path) -> Vec<crate::diarization::download::Asset> {
    ASSETS.iter().map(|(path, size, sha256)| crate::diarization::download::Asset {
        name: format!("Parakeet CoreML {path}"),
        dest: dir.join(path),
        size: *size,
        url: format!("https://huggingface.co/{REPO}/resolve/{REVISION}/{path}"),
        sha256,
    }).collect()
}

/// Make an installed Parakeet v3 use CoreML by default: fetch the pinned
/// conversion quietly if missing (ONNX keeps transcribing meanwhile), load it
/// once so CoreML compiles it for this Mac, then switch the loaded engine model
/// over unless a recording is running (it switches on the next load otherwise).
pub fn prepare_in_background() {
    tauri::async_runtime::spawn(async {
        static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let Ok(_install) = INSTALL.try_lock() else { return };
        let onnx_dir = crate::paths::models_dir().join("parakeet").join("parakeet-tdt-0.6b-v3-int8");
        if !onnx_dir.join("vocab.txt").is_file() { return; }
        let dir = dir_for(&onnx_dir);
        if !available(&dir) {
            log::info!("Parakeet CoreML: fetching the CoreML conversion for this install");
            if let Err(error) = crate::diarization::download::install_assets::<tauri::Wry>(None, &assets_in(&dir), "Parakeet CoreML installed").await {
                log::warn!("Parakeet CoreML download failed; ONNX Runtime stays in use: {error}");
                return;
            }
        }
        let started = std::time::Instant::now();
        let warm = dir.clone();
        match tauri::async_runtime::spawn_blocking(move || ParakeetCoreMl::load(&warm)).await {
            Ok(Ok(_)) => log::info!("Parakeet CoreML model ready in {:.1} s", started.elapsed().as_secs_f32()),
            Ok(Err(error)) => { log::warn!("Parakeet CoreML failed to load; ONNX Runtime stays in use: {error}"); return; }
            Err(error) => { log::warn!("Parakeet CoreML warm-up task failed: {error}"); return; }
        }
        if crate::audio::recording_commands::is_recording().await {
            log::info!("Parakeet CoreML: recording in progress; switching on the next model load");
            return;
        }
        let engine = crate::parakeet_engine::commands::PARAKEET_ENGINE.lock().ok().and_then(|g| g.clone());
        if let Some(engine) = engine {
            if let Err(error) = engine.reload_current_model().await {
                log::warn!("Parakeet CoreML: could not switch the loaded model: {error}");
            }
        }
    });
}

#[cfg(test)]
mod install_tests {
    use super::*;

    /// Fetches the pinned files through the real install path into a temporary
    /// folder (~480 MB), then loads all four models.
    #[tokio::test]
    #[ignore = "Downloads ~480 MB from Hugging Face"]
    async fn install_downloads_verifies_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        crate::diarization::download::install_assets::<tauri::Wry>(None, &assets_in(dir.path()), "done").await.unwrap();
        assert!(available(dir.path()));
        let root = dir.path().to_path_buf();
        tokio::task::spawn_blocking(move || ParakeetCoreMl::load(&root).unwrap()).await.unwrap();
    }

    #[test]
    fn download_urls_are_pinned_and_stay_in_the_folder() {
        let dir = Path::new("/m/parakeet").join(DIR_NAME);
        for asset in assets_in(&dir) {
            assert!(asset.dest.starts_with(&dir));
            assert!(asset.url.starts_with(&format!("https://huggingface.co/{REPO}/resolve/{REVISION}/")), "{}", asset.url);
        }
    }
}
