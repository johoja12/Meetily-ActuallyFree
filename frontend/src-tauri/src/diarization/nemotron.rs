//! Optional Nemotron-3 engine. Runs on CoreML (macOS, when its conversion is
//! installed) or the shared ONNX Runtime; both execute the same checkpoint.
//! Speaker identities are meeting-local; only capture provenance identifies You.
use anyhow::{ensure, Result};
use std::path::{Path, PathBuf};
use super::{DiarizationResult, DiarizationSegment};
#[path = "sortformer/mod.rs"]
#[allow(dead_code)] // Keep the attributed reference implementation's API intact.
mod sortformer;
pub(crate) use sortformer::SpeakerSegment as StreamingSpeakerSegment;
#[cfg(test)]
pub(crate) use sortformer::StreamingProfile;

pub const NEMOTRON_MODEL_FILENAME: &str = "nemotron3_diar_v3.onnx";
pub const NEMOTRON_EXPECTED_BYTES: u64 = 400_506_656;
pub const NEMOTRON_SHA256: &str = "915e4fa23b0192ed9fadeb1cdd26847df986d50c92012d177be28d0343bbe03a";
pub const DEFAULT_NEMOTRON_THRESHOLD: f32 = 0.50;
/// ONNX live profile in 80 ms model frames: 2.0 s emitted per step plus 0.32 s
/// lookahead. NVIDIA's 1.04 s preset (9 + 4) runs at ~0.6x real time on an M4
/// CPU by itself, so with Parakeet sharing the cores live labeling fell behind
/// and its queue overflowed. Each step attends over the full speaker cache and
/// FIFO, so a longer chunk is ~2-3x cheaper per second of audio (steady-state
/// RTF ~0.25 vs ~0.57 on a two-speaker meeting) and agreed slightly better with
/// the offline profile. CoreML runs NVIDIA's 1.04 s preset instead (~0.05x).
pub const LIVE_CHUNK_FRAMES: usize = 25;
pub const LIVE_RIGHT_CONTEXT_FRAMES: usize = 4;
/// Audio samples per 80 ms model frame at 16 kHz.
pub const SAMPLES_PER_FRAME: usize = 1280;
pub const DEFAULT_MAX_SPEAKERS: usize = 8;

/// Fixed-shape CoreML conversion of the same checkpoint, pinned by revision,
/// length and SHA-256 (OpenMDW-1.1). Paths are relative to [`coreml_dir`].
/// Each `.mlmodelc` is a directory; its files are verified individually.
pub const COREML_REVISION: &str = "25a90f97f254428d4b30374b76af9c74fdee8327";
pub const COREML_REPO: &str = "FluidInference/nemotron-3-diarization-coreml";
pub const COREML_LIVE_MODEL: &str = "monolithic/v2/Nemotron3Diarizer_low.mlmodelc";
pub const COREML_OFFLINE_MODEL: &str = "monolithic/v2/Nemotron3Diarizer_offline.mlmodelc";
pub const COREML_SILENCE_EMBEDDING: &str = "learnable_sil_emb.bin";
/// (relative path, bytes, sha256)
pub const COREML_ASSETS: [(&str, u64, &str); 10] = [
    ("README.md", 7948, "a091d8d252882018a82593cafdd8020b924102b2d9b8d38a0db0c57413e84115"),
    ("learnable_sil_emb.bin", 2048, "d4417b3c0eabdf7c47032fac2b5b5a7ee83d819a6ddda8fd8eaf74e2b5cc4ac7"),
    ("monolithic/v2/Nemotron3Diarizer_low.mlmodelc/analytics/coremldata.bin", 243, "1ec46d7392b470ebfaac473123de9e5bc62630972b01349b929bf6e48e067c6c"),
    ("monolithic/v2/Nemotron3Diarizer_low.mlmodelc/coremldata.bin", 752, "2c2c82efc948433cb47b60aceb4917182e2e00d1d79097b8898adfd615b0c5f8"),
    ("monolithic/v2/Nemotron3Diarizer_low.mlmodelc/model.mil", 503_842, "8e2ffc4ce5edf86b8a1845e5229a219ca9e8cc0e03aa4a6e7317e81cb7da2da5"),
    ("monolithic/v2/Nemotron3Diarizer_low.mlmodelc/weights/weight.bin", 198_617_472, "88307e2c51d1877de59803299354f689e70cad7f8892a197764d5f3626089495"),
    ("monolithic/v2/Nemotron3Diarizer_offline.mlmodelc/analytics/coremldata.bin", 243, "491594df92282a4f2cef65e96d236e210a5c4627063e37e822ec858aaaad416d"),
    ("monolithic/v2/Nemotron3Diarizer_offline.mlmodelc/coremldata.bin", 758, "8b790c919c65744648c17290a26d3371e0e55db655310e7f8445080757b0bf08"),
    ("monolithic/v2/Nemotron3Diarizer_offline.mlmodelc/model.mil", 505_274, "ea5673d9e9ec785e7c8fb628acd82f9f86214c46b6584eaacdb6faddcb3f0277"),
    ("monolithic/v2/Nemotron3Diarizer_offline.mlmodelc/weights/weight.bin", 198_654_080, "bab76e5f190d0e4a4e174e7fcb1e9beea58c6b2be56e665e2cac8fba6d10f7f1"),
];

/// Folder under the diarization models directory; removed whole on uninstall.
pub const COREML_DIR_NAME: &str = "nemotron3-coreml";
pub fn coreml_dir() -> PathBuf { super::diarization_user_model_dir().join(COREML_DIR_NAME) }

/// Whether every pinned CoreML file is present at its published length. The
/// download verifies SHA-256 before renaming into place; loading verifies again
/// once per process (`verify_coreml_files`).
pub fn coreml_available() -> bool {
    cfg!(target_os = "macos") && COREML_ASSETS.iter().all(|(path, size, _)| {
        coreml_dir().join(path).metadata().is_ok_and(|m| m.is_file() && m.len() == *size)
    })
}

/// Which runtime a model instance executes on, for logs and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NemotronRuntime { CoreMl, Onnx }

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

/// Verify the CoreML files before passing them to native code. A file is
/// hashed once per process while its length and modification time are unchanged,
/// so a recording start does not re-hash ~400 MB.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn verify_coreml_files(dir: &Path) -> Result<()> {
    use std::sync::Mutex;
    static VERIFIED: Mutex<Vec<(PathBuf, u64, std::time::SystemTime)>> = Mutex::new(Vec::new());
    for (relative, size, sha) in COREML_ASSETS {
        let path = dir.join(relative);
        let meta = path.metadata()?;
        ensure!(meta.len() == size, "Incomplete CoreML diarization file {relative}");
        let key = (path.clone(), meta.len(), meta.modified()?);
        if VERIFIED.lock().map(|v| v.contains(&key)).unwrap_or(false) { continue; }
        ensure!(sha256_file(&path)? == sha, "CoreML diarization file {relative} failed its checksum");
        if let Ok(mut verified) = VERIFIED.lock() { verified.push(key); }
    }
    Ok(())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn read_silence_embedding(dir: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(dir.join(COREML_SILENCE_EMBEDDING))?;
    ensure!(bytes.len() == 512 * 4, "Invalid silence embedding");
    Ok(bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

fn diarization_config(threshold: f32) -> sortformer::DiarizationConfig {
    sortformer::DiarizationConfig { onset: threshold, offset: threshold, ..Default::default() }
}

fn onnx_live_profile() -> sortformer::StreamingProfile {
    sortformer::StreamingProfile {
        chunk_len: LIVE_CHUNK_FRAMES, right_context: LIVE_RIGHT_CONTEXT_FRAMES, ..sortformer::StreamingProfile::low_latency()
    }
}

pub struct NemotronDiarizationModel {
    model: sortformer::Sortformer,
    runtime: NemotronRuntime,
}

impl NemotronDiarizationModel {
    /// Offline (post-call) model: CoreML when installed on macOS, else ONNX.
    pub fn new<P: AsRef<Path>>(path: P, max_speakers: usize, threshold: f32) -> Result<Self> {
        Self::select(path.as_ref(), max_speakers, threshold, false)
    }

    /// Live streaming model: CoreML runs NVIDIA's 1.04 s preset; ONNX runs the
    /// cheaper 2.32 s profile so it can keep up on the CPU.
    pub fn new_live<P: AsRef<Path>>(path: P, max_speakers: usize, threshold: f32) -> Result<Self> {
        Self::select(path.as_ref(), max_speakers, threshold, true)
    }

    fn select(path: &Path, max_speakers: usize, threshold: f32, live: bool) -> Result<Self> {
        ensure!((1..=8).contains(&max_speakers), "Nemotron supports 1–8 speaker channels");
        ensure!(threshold.is_finite() && (0.1..=0.9).contains(&threshold), "Invalid speech threshold");
        if coreml_available() {
            match Self::coreml(&coreml_dir(), threshold, live) {
                Ok(model) => return Ok(model),
                // Same checkpoint on another runtime, not a different engine.
                Err(error) => log::warn!("Nemotron: CoreML unavailable, using ONNX Runtime: {error}"),
            }
        }
        let mut model = Self::onnx(path, threshold)?;
        if live { model.model.set_profile(onnx_live_profile())?; }
        Ok(model)
    }

    /// ONNX Runtime with the offline profile (the export's metadata defaults).
    pub fn onnx(path: &Path, threshold: f32) -> Result<Self> {
        ensure!(threshold.is_finite() && (0.1..=0.9).contains(&threshold), "Invalid speech threshold");
        // Also verify manually copied files before passing them to native code.
        let file = std::fs::File::open(path)?;
        ensure!(file.metadata()?.len() == NEMOTRON_EXPECTED_BYTES, "Incomplete Nemotron model; download it again");
        drop(file);
        ensure!(sha256_file(path)? == NEMOTRON_SHA256, "Nemotron model checksum mismatch; download it again");
        // Do not truncate output channels to pretend an exact speaker count.
        // Arrival-ordered channels are model identities, not a clustering limit.
        let model = sortformer::Sortformer::with_config(path, None, diarization_config(threshold))?;
        Ok(Self { model, runtime: NemotronRuntime::Onnx })
    }

    /// CoreML conversion from `dir` (see [`COREML_ASSETS`]); `live` selects the
    /// 1.04 s model, otherwise the offline one. Each compiled model is fixed to
    /// its profile's window.
    #[cfg(target_os = "macos")]
    pub fn coreml(dir: &Path, threshold: f32, live: bool) -> Result<Self> {
        ensure!(threshold.is_finite() && (0.1..=0.9).contains(&threshold), "Invalid speech threshold");
        verify_coreml_files(dir)?;
        let (relative, profile) = if live {
            (COREML_LIVE_MODEL, sortformer::StreamingProfile::low_latency())
        } else {
            (COREML_OFFLINE_MODEL, sortformer::StreamingProfile::offline())
        };
        let model = sortformer::Sortformer::with_coreml(&dir.join(relative), read_silence_embedding(dir)?, profile, diarization_config(threshold))?;
        Ok(Self { model, runtime: NemotronRuntime::CoreMl })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn coreml(_dir: &Path, _threshold: f32, _live: bool) -> Result<Self> {
        anyhow::bail!("CoreML is only available on macOS")
    }

    pub fn runtime(&self) -> NemotronRuntime { self.runtime }

    /// `(emitted frames, lookahead frames)` per step, in 80 ms model frames.
    pub fn streaming_frames(&self) -> (usize, usize) {
        let profile = self.model.profile();
        (profile.chunk_len, profile.right_context)
    }

    pub fn reset_streaming_state(&mut self) { self.model.reset_state(); }

    /// Switch an ONNX model to the live profile. CoreML models are compiled for
    /// one window, so build those with [`Self::new_live`] instead.
    pub fn enable_live_streaming(&mut self) -> Result<()> {
        ensure!(self.runtime == NemotronRuntime::Onnx, "A CoreML model's profile is fixed; use new_live");
        self.model.set_profile(onnx_live_profile())?;
        Ok(())
    }

    #[cfg(test)]
    pub fn set_profile_for_test(&mut self, profile: sortformer::StreamingProfile) -> Result<()> {
        self.model.set_profile(profile)?;
        Ok(())
    }

    pub fn feed(&mut self, samples: &[f32]) -> Result<Vec<StreamingSpeakerSegment>> {
        ensure!(samples.iter().all(|v| v.is_finite()), "Invalid streaming audio samples");
        Ok(self.model.feed(samples)?)
    }

    pub fn flush(&mut self) -> Result<Vec<StreamingSpeakerSegment>> {
        Ok(self.model.flush()?)
    }

    pub fn diarize(&mut self, samples: &[f32], sample_rate: u32) -> Result<DiarizationResult> {
        ensure!(sample_rate > 0, "Invalid sample rate");
        ensure!(samples.iter().all(|v| v.is_finite()), "Invalid audio samples");
        let audio = if sample_rate == 16000 { samples.to_vec() } else {
            crate::audio::audio_processing::resample_audio(samples, sample_rate, 16000)
        };
        let duration = audio.len() as f32 / 16000.0;
        if audio.is_empty() {
            return Ok(DiarizationResult { segments: vec![], num_speakers: 0, duration, user_speaker: None });
        }
        let predicted = self.model.diarize(audio, 16000, 1)?;
        let mut segments: Vec<_> = predicted.into_iter().map(|s| DiarizationSegment {
            start: s.start as f32 / 16000.0,
            end: s.end as f32 / 16000.0,
            speaker: s.speaker_id,
            overlapped: false,
        }).collect();
        let snapshot = segments.clone();
        for segment in &mut segments {
            segment.overlapped = snapshot.iter().any(|other| other.speaker != segment.speaker
                && other.start < segment.end && other.end > segment.start);
        }
        let num_speakers = segments.iter().map(|s| s.speaker).collect::<std::collections::HashSet<_>>().len();
        Ok(DiarizationResult { segments, num_speakers, duration, user_speaker: None })
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-model tests run on ONNX (`MEETILY_NEMOTRON_MODEL`) and, when
    /// `MEETILY_NEMOTRON_COREML_DIR` points at the pinned conversion, on CoreML.
    pub(crate) fn test_models(live: bool) -> Vec<NemotronDiarizationModel> {
        let path = std::env::var("MEETILY_NEMOTRON_MODEL").expect("Set MEETILY_NEMOTRON_MODEL");
        let mut onnx = NemotronDiarizationModel::onnx(Path::new(&path), 0.5).unwrap();
        if live { onnx.enable_live_streaming().unwrap(); }
        let mut models = vec![onnx];
        if let Ok(dir) = std::env::var("MEETILY_NEMOTRON_COREML_DIR") {
            models.push(NemotronDiarizationModel::coreml(Path::new(&dir), 0.5, live).unwrap());
        }
        models
    }

    #[test]
    fn coreml_assets_are_pinned_and_cover_both_models() {
        for (path, size, sha) in COREML_ASSETS {
            assert!(size > 0 && sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()), "{path}");
            assert!(!path.starts_with('/') && !path.contains(".."), "{path}");
        }
        for model in [COREML_LIVE_MODEL, COREML_OFFLINE_MODEL] {
            for file in ["coremldata.bin", "model.mil", "weights/weight.bin", "analytics/coremldata.bin"] {
                let wanted = format!("{model}/{file}");
                assert!(COREML_ASSETS.iter().any(|(path, _, _)| *path == wanted), "missing {wanted}");
            }
        }
        assert!(COREML_ASSETS.iter().any(|(path, _, _)| *path == COREML_SILENCE_EMBEDDING));
        assert_eq!(COREML_REVISION.len(), 40);
    }

    #[test]
    #[ignore = "Requires pinned model and speech fixture"]
    fn live_nemotron_streams_continuously_and_resets_between_meetings() {
        let path = std::env::var("MEETILY_NEMOTRON_MODEL").unwrap();
        let wav = std::env::var("MEETILY_NEMOTRON_WAV").unwrap();
        let (audio, rate) = super::super::dsp::read_wav(Path::new(&wav)).unwrap();
        assert_eq!(rate, 16000);
        let _ = path;
        for mut model in test_models(true) {
        let runtime = model.runtime();
        let started = std::time::Instant::now();
        let mut segments = Vec::new();
        for chunk in audio.chunks(800) { segments.extend(model.feed(chunk).unwrap()); }
        segments.extend(model.flush().unwrap());
        assert!(!segments.is_empty());
        assert!(segments.iter().all(|s| s.start < s.end && s.end <= audio.len() as u64 && s.speaker_id < 8));
        let speakers = segments.iter().map(|s| s.speaker_id).collect::<std::collections::HashSet<_>>();
        assert!(speakers.len() >= 2, "Multispeaker fixture collapsed: {speakers:?}");
        if let Ok(expected) = std::env::var("MEETILY_NEMOTRON_EXPECTED") {
            let turns: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(expected).unwrap()).unwrap();
            let mut voices = std::collections::HashMap::new();
            for turn in turns {
                let name = turn["voice"].as_str().unwrap();
                if name == "overlap" { continue; }
                let start = (turn["start"].as_f64().unwrap() * 16000.0) as u64;
                let end = (turn["end"].as_f64().unwrap() * 16000.0) as u64;
                let mut coverage = [0u64; 8];
                for segment in &segments {
                    coverage[segment.speaker_id] += segment.end.min(end).saturating_sub(segment.start.max(start));
                }
                let (speaker, &duration) = coverage.iter().enumerate().max_by_key(|(_, n)| *n).unwrap();
                assert!(duration > (end - start) / 2, "Insufficient live coverage for {name}");
                if let Some(previous) = voices.insert(name.to_string(), speaker) {
                    assert_eq!(previous, speaker, "Live identity changed for returning speaker {name}");
                }
            }
            assert_eq!(voices.values().collect::<std::collections::HashSet<_>>().len(), voices.len());
        }
        println!("LIVE_NEMOTRON {runtime:?} duration={} inference={:?} speakers={}", audio.len() as f64 / 16000.0, started.elapsed(), speakers.len());
        model.reset_streaming_state();
        assert!(model.feed(&vec![0.0; 16000 * 2]).unwrap().is_empty());
        assert!(model.flush().unwrap().is_empty());
        }
    }

    #[test]
    #[ignore = "Requires MEETILY_NEMOTRON_MODEL pointing to the pinned model"]
    fn real_model_silence_and_optional_speech() {
        for mut model in test_models(false) {
        let runtime = model.runtime();
        let silence = model.diarize(&vec![0.0; 16000 * 65], 16000).unwrap();
        assert_eq!(silence.duration, 65.0);
        assert!(silence.segments.is_empty(), "Silence invented speakers: {:?}", silence.segments);
        assert_eq!(silence.num_speakers, 0);
        assert_eq!(silence.user_speaker, None);
        if let Ok(wav) = std::env::var("MEETILY_NEMOTRON_WAV") {
            let (audio, rate) = super::super::dsp::read_wav(Path::new(&wav)).unwrap();
            let started = std::time::Instant::now();
            let result = model.diarize(&audio, rate).unwrap();
            assert!(!result.segments.is_empty(), "Speech produced no speakers");
            assert_eq!(result.user_speaker, None);
            assert!(result.segments.iter().all(|s| s.start >= 0.0 && s.end > s.start && s.end <= result.duration + 0.01));
            if let Ok(expected) = std::env::var("MEETILY_NEMOTRON_EXPECTED") {
                let turns: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(expected).unwrap()).unwrap();
                let mut voices = std::collections::HashMap::new();
                for turn in turns {
                    let name = turn["voice"].as_str().unwrap();
                    let start = turn["start"].as_f64().unwrap() as f32;
                    let end = turn["end"].as_f64().unwrap() as f32;
                    let mut coverage = [0.0f32; 8];
                    for seg in &result.segments {
                        coverage[seg.speaker] += (seg.end.min(end) - seg.start.max(start)).max(0.0);
                    }
                    if name == "overlap" {
                        assert_eq!(coverage.iter().filter(|&&seconds| seconds > (end-start)*0.5).count(),2, "Both overlapping voices must survive");
                    } else {
                        let (speaker, duration) = coverage.iter().enumerate().max_by(|a,b| a.1.total_cmp(b.1)).unwrap();
                        assert!(*duration > (end-start)*0.5, "Missing speech for {name}");
                        if let Some(previous) = voices.insert(name.to_string(),speaker) { assert_eq!(previous,speaker,"Returning speaker changed identity"); }
                    }
                }
                assert_eq!(voices.values().collect::<std::collections::HashSet<_>>().len(),voices.len(),"Distinct voices merged");
                assert_eq!(result.num_speakers, voices.len());
            }
            println!("NEMOTRON_SPEECH_RESULT {runtime:?} {:?}; elapsed {:?}", result, started.elapsed());
        }
        }
    }
}
