//! CPU benchmarks for recording and post-call processing (macOS, ignored).
//!
//! They replay saved recordings through the production code as fast as it
//! runs and report process CPU seconds (user + system, all threads, from
//! getrusage) per minute of audio, plus wall time. CPU time rather than wall
//! time keeps the numbers comparable on a busy machine.
//!
//! Inputs (environment):
//! - `MEETILY_BENCH_MIC`: microphone recording (any decodable file).
//! - `MEETILY_BENCH_SYSTEM`: optional computer-audio recording (or `silence`);
//!   enables echo cancellation and the second VAD, as in personal-microphone mode.
//! - `MEETILY_BENCH_SECONDS`: audio length to use (default 600).
//! - `MEETILY_BENCH_SEGMENTS`: file the pipeline bench writes its speech
//!   segments to and the Parakeet bench reads.
//! - `MEETILY_BENCH_PARAKEET_DIR`, `MEETILY_BENCH_WHISPER_DIR`: model folders.
//!
//! Run one at a time, e.g.
//! `cargo test --release -p meetily --lib perf_bench::live_pipeline -- --ignored --nocapture`.
#![cfg(all(test, target_os = "macos"))]

use crate::audio::recording_state::{AudioChunk, DeviceType, RecordingState};
use crate::audio::vad::SpeechSegment;
use std::path::PathBuf;
use std::time::Instant;

fn cpu_seconds() -> f64 {
    // SAFETY: getrusage fills the zeroed struct.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

struct Meter {
    cpu: f64,
    wall: Instant,
}

impl Meter {
    fn start() -> Self {
        Self { cpu: cpu_seconds(), wall: Instant::now() }
    }
    fn report(&self, label: &str, audio_seconds: f64) {
        let cpu = cpu_seconds() - self.cpu;
        let wall = self.wall.elapsed().as_secs_f64();
        println!(
            "BENCH {label}: audio {audio_seconds:.0} s | CPU {cpu:.2} s ({:.2} CPU-s per audio minute, {:.1}% of one core in real time) | wall {wall:.2} s",
            cpu / audio_seconds * 60.0,
            cpu / audio_seconds * 100.0,
        );
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name).ok().filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn seconds() -> f64 {
    std::env::var("MEETILY_BENCH_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(600.0)
}

/// Mono samples at 48 kHz, at most `seconds()` long.
fn load_48k(path: &PathBuf) -> Vec<f32> {
    let decoded = crate::audio::decoder::decode_audio_file(path).expect("decode");
    let channels = decoded.channels.max(1) as usize;
    let mono: Vec<f32> = decoded.samples.chunks(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect();
    let mono = if decoded.sample_rate == 48_000 {
        mono
    } else {
        crate::audio::audio_processing::resample_audio(&mono, decoded.sample_rate, 48_000)
    };
    let limit = (seconds() * 48_000.0) as usize;
    mono[..mono.len().min(limit)].to_vec()
}

fn write_segments(path: &PathBuf, segments: &[(f64, Vec<f32>)]) {
    let mut out = Vec::new();
    for (start, samples) in segments {
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
    std::fs::write(path, out).unwrap();
}

fn read_segments(path: &PathBuf) -> Vec<(f64, Vec<f32>)> {
    let data = std::fs::read(path).unwrap();
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let start = f64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        let n = u32::from_le_bytes(data[i + 8..i + 12].try_into().unwrap()) as usize;
        i += 12;
        let samples = data[i..i + n * 4].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        i += n * 4;
        out.push((start, samples));
    }
    out
}

/// The live capture pipeline (resampling, echo cancellation with a system
/// track, high-pass, loudness normalization, both VADs, mixing, level
/// metering), fed 10 ms blocks per source. Speech recognition is excluded
/// (see `live_parakeet`).
#[test]
#[ignore]
fn live_pipeline() {
    let mic_path = env_path("MEETILY_BENCH_MIC").expect("MEETILY_BENCH_MIC");
    let mic = load_48k(&mic_path);
    // "silence": a system track that is present but silent (headphones, or the
    // other side listening).
    let system = env_path("MEETILY_BENCH_SYSTEM").map(|p| {
        if p.as_os_str() == "silence" { vec![0.0; mic.len()] } else { load_48k(&p) }
    });
    let audio_seconds = mic.len() as f64 / 48_000.0;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let segments = runtime.block_on(async move {
        let state = RecordingState::new();
        state.set_room_microphone(system.is_none());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (speech_tx, mut speech_rx) = tokio::sync::mpsc::unbounded_channel::<AudioChunk>();
        let (tracks_tx, mut tracks_rx) = tokio::sync::mpsc::unbounded_channel();
        let (levels_tx, mut levels_rx) = tokio::sync::mpsc::unbounded_channel();
        let system_name = if system.is_some() { "system" } else { "No System Audio" };
        let mut pipeline = crate::audio::pipeline::AudioPipeline::new(
            rx, speech_tx, state, 1000, 48_000,
            "mic".into(), crate::audio::device_detection::InputDeviceKind::Unknown,
            system_name.into(), crate::audio::device_detection::InputDeviceKind::Unknown,
        )
        .unwrap();
        pipeline.set_bench_outputs(tracks_tx, levels_tx);
        // Drain outputs concurrently so nothing accumulates.
        let drain_tracks = tokio::spawn(async move { while tracks_rx.recv().await.is_some() {} });
        let drain_levels = tokio::spawn(async move {
            let mut n = 0u64;
            while levels_rx.recv().await.is_some() {
                n += 1;
            }
            n
        });
        let collect = tokio::spawn(async move {
            let mut out = Vec::new();
            while let Some(chunk) = speech_rx.recv().await {
                out.push((chunk.timestamp * 1000.0, chunk.data));
            }
            out
        });
        let meter = Meter::start();
        let block = 480;
        let mut id = 0u64;
        for (i, piece) in mic.chunks(block).enumerate() {
            let timestamp = ((i + 1) * block) as f64 / 48_000.0;
            tx.send(AudioChunk { data: piece.to_vec(), sample_rate: 48_000, timestamp, chunk_id: id, device_type: DeviceType::Microphone }).unwrap();
            id += 1;
            if let Some(system) = &system {
                let lo = i * block;
                if lo < system.len() {
                    let hi = (lo + block).min(system.len());
                    tx.send(AudioChunk { data: system[lo..hi].to_vec(), sample_rate: 48_000, timestamp, chunk_id: id, device_type: DeviceType::System }).unwrap();
                    id += 1;
                }
            }
        }
        drop(tx);
        pipeline.run().await.unwrap();
        drain_tracks.await.unwrap();
        let levels = drain_levels.await.unwrap();
        let segments = collect.await.unwrap();
        let label = match std::env::var("MEETILY_BENCH_SYSTEM").as_deref() { Ok("silence") => "live pipeline (mic + silent system)", Ok(_) => "live pipeline (mic + system)", Err(_) => "live pipeline (room mic)" };
        meter.report(label, audio_seconds);
        println!("BENCH   {} speech segments, {} level updates ({:.1}/s of audio)", segments.len(), levels, levels as f64 / audio_seconds);
        segments
    });
    if let Some(path) = env_path("MEETILY_BENCH_SEGMENTS") {
        write_segments(&path, &segments);
    }
}

/// Live Parakeet on the pipeline's speech segments (written by `live_pipeline`).
#[test]
#[ignore]
fn live_parakeet() {
    let segments = read_segments(&env_path("MEETILY_BENCH_SEGMENTS").expect("MEETILY_BENCH_SEGMENTS"));
    let dir = env_path("MEETILY_BENCH_PARAKEET_DIR").expect("MEETILY_BENCH_PARAKEET_DIR");
    let mut model = crate::parakeet_engine::model::ParakeetModel::new(&dir, true).expect("load Parakeet");
    // Warm-up (first CoreML prediction compiles/caches).
    let _ = model.transcribe_samples(vec![0.0; 16_000], None);
    let speech: f64 = segments.iter().map(|(_, s)| s.len() as f64 / 16_000.0).sum();
    let meter = Meter::start();
    let mut words = 0;
    for (_, samples) in &segments {
        let result = model.transcribe_samples(samples.clone(), None).expect("transcribe");
        words += result.text.split_whitespace().count();
    }
    meter.report(&format!("live Parakeet ({:?}, {} segments)", model.runtime(), segments.len()), speech);
    println!("BENCH   {words} words");
}

/// Post-call: decode + resample, full-recording VAD, Whisper on packed windows.
#[test]
#[ignore]
fn post_call() {
    let mic_path = env_path("MEETILY_BENCH_MIC").expect("MEETILY_BENCH_MIC");
    let limit = seconds();
    let meter = Meter::start();
    let decoded = crate::audio::decoder::decode_audio_file(&mic_path).expect("decode");
    let mut samples = decoded.to_whisper_format();
    samples.truncate((limit * 16_000.0) as usize);
    let audio_seconds = samples.len() as f64 / 16_000.0;
    meter.report("post-call decode + resample (whole file)", decoded.duration_seconds.min(f64::MAX));

    let meter = Meter::start();
    let segments: Vec<SpeechSegment> =
        crate::audio::vad::get_speech_chunks_with_thresholds_and_progress(&samples, 2000, 0.20, 0.10, |_, _| true).expect("vad");
    meter.report(&format!("post-call VAD ({} segments)", segments.len()), audio_seconds);

    let Some(models) = env_path("MEETILY_BENCH_WHISPER_DIR") else { return };
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
        let engine = crate::whisper_engine::WhisperEngine::new_with_models_dir(Some(models)).unwrap();
        let _ = engine.discover_models().await;
        engine.load_model("large-v3-turbo-q5_0").await.expect("load Whisper");
        let tagged: Vec<(SpeechSegment, Option<&'static str>)> = segments.into_iter().map(|s| (s, Some("room"))).collect();
        let meter = Meter::start();
        let out = crate::audio::speech_windows::transcribe_packed(&engine, &tagged, Some("en".into()), None, |_, _, _| true)
            .await
            .unwrap()
            .unwrap();
        meter.report(&format!("post-call Whisper ({} rows)", out.len()), audio_seconds);
        let words: usize = out.iter().map(|t| t.text.split_whitespace().count()).sum();
        println!("BENCH   {words} words");
    });
}

/// Live Nemotron speaker labeling on CoreML (the room-mode live path), fed
/// 50 ms of 16 kHz audio at a time like the production worker.
/// `MEETILY_BENCH_NEMOTRON_DIR`: the `nemotron3-coreml` folder.
#[test]
#[ignore]
fn live_nemotron() {
    let mic = load_48k(&env_path("MEETILY_BENCH_MIC").expect("MEETILY_BENCH_MIC"));
    let audio = crate::audio::audio_processing::resample_audio(&mic, 48_000, 16_000);
    let dir = env_path("MEETILY_BENCH_NEMOTRON_DIR").expect("MEETILY_BENCH_NEMOTRON_DIR");
    let mut model = crate::diarization::nemotron::NemotronDiarizationModel::coreml(&dir, 0.5, true).expect("load Nemotron");
    let _ = model.feed(&vec![0.0; 16_000]);
    let meter = Meter::start();
    let mut segments = 0;
    for samples in audio.chunks(800) {
        segments += model.feed(samples).unwrap().len();
    }
    segments += model.flush().unwrap().len();
    meter.report(&format!("live Nemotron CoreML ({segments} segments)"), audio.len() as f64 / 16_000.0);
}

/// Room playback cleanup stages on the room microphone: WPE, DeepFilterNet
/// and level matching (the FFmpeg decode/encode run as separate processes and
/// are not counted). Uses a two-thread pool like the production worker.
#[test]
#[ignore]
fn room_cleanup() {
    use crate::audio::room_cleanup::{dfn::StreamingDfn, level::LevelMatch, wpe::{StreamingWpe, WpeConfig}};
    let mic = load_48k(&env_path("MEETILY_BENCH_MIC").expect("MEETILY_BENCH_MIC"));
    let audio_seconds = mic.len() as f64 / 48_000.0;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    pool.install(|| {
        let meter = Meter::start();
        let mut wpe = StreamingWpe::new(WpeConfig::room_48k());
        let mut dry = Vec::with_capacity(mic.len());
        for piece in mic.chunks(48_000) {
            dry.extend(wpe.push(piece));
        }
        dry.extend(wpe.finish());
        meter.report("room cleanup WPE", audio_seconds);
        let meter = Meter::start();
        let mut dfn = StreamingDfn::new().unwrap();
        let mut enhanced = Vec::with_capacity(dry.len());
        for piece in dry.chunks(48_000) {
            enhanced.extend(dfn.push(piece).unwrap());
        }
        enhanced.extend(dfn.finish().unwrap());
        meter.report("room cleanup DeepFilterNet", audio_seconds);
        let meter = Meter::start();
        let mut level = LevelMatch::new();
        level.push_input(&mic);
        let out = level.process(&enhanced);
        meter.report("room cleanup level match", audio_seconds);
        assert_eq!(out.len(), mic.len());
    });
}

/// DeepFilterNet streamed (libDF, one frame per call) against the batched
/// graphs in room_cleanup::dfn_batch, on WPE output like production. Needs
/// MEETILY_DFN_ONNX_TAR (libDF's models/DeepFilterNet3_onnx.tar.gz). Reports
/// CPU for both and the SNR of batched against streamed output.
#[test]
#[ignore]
fn room_cleanup_dfn_batched() {
    use crate::audio::room_cleanup::{dfn::StreamingDfn, dfn_batch::{enhance, BatchedDfn, DfnGraphs, OrtDfn}, wpe::{StreamingWpe, WpeConfig}};
    let mic = load_48k(&env_path("MEETILY_BENCH_MIC").expect("MEETILY_BENCH_MIC"));
    let tar = std::fs::read(env_path("MEETILY_DFN_ONNX_TAR").expect("MEETILY_DFN_ONNX_TAR")).unwrap();
    let audio_seconds = mic.len() as f64 / 48_000.0;
    let mut wpe = StreamingWpe::new(WpeConfig::room_48k());
    let mut dry = Vec::with_capacity(mic.len());
    for piece in mic.chunks(48_000) {
        dry.extend(wpe.push(piece));
    }
    dry.extend(wpe.finish());

    let meter = Meter::start();
    let mut dfn = StreamingDfn::new().unwrap();
    let mut streamed = Vec::with_capacity(dry.len());
    for piece in dry.chunks(48_000) {
        streamed.extend(dfn.push(piece).unwrap());
    }
    streamed.extend(dfn.finish().unwrap());
    meter.report("DeepFilterNet streamed", audio_seconds);

    let graphs = DfnGraphs::from_targz(&tar).unwrap();
    let backend = std::env::var("MEETILY_DFN_BACKEND").unwrap_or_else(|_| "ort".into());
    let meter = Meter::start();
    let mut model: Box<dyn crate::audio::room_cleanup::dfn_batch::BlockModel> = if backend == "tract" {
        Box::new(BatchedDfn::new(&graphs).unwrap())
    } else {
        Box::new(OrtDfn::new(&graphs, 2).unwrap())
    };
    meter.report(&format!("DeepFilterNet batched {backend} (load)"), audio_seconds);
    let meter = Meter::start();
    let out = enhance(model.as_mut(), &dry).unwrap();
    meter.report(&format!("DeepFilterNet batched {backend}"), audio_seconds);
    assert_eq!(out.len(), streamed.len());

    let snr = |a: &[f32], b: &[f32]| {
        let signal: f64 = a.iter().map(|v| (*v as f64).powi(2)).sum();
        let noise: f64 = a.iter().zip(b).map(|(x, y)| ((*x - *y) as f64).powi(2)).sum();
        10.0 * (signal / noise.max(1e-20)).log10()
    };
    println!("BENCH batched vs streamed: SNR {:.1} dB over the whole clip", snr(&streamed, &out));
    // Worst 10 s window, to catch block-boundary or alignment problems.
    let window = 480_000;
    let worst = streamed.chunks(window).zip(out.chunks(window)).map(|(a, b)| snr(a, b)).fold(f64::INFINITY, f64::min);
    println!("BENCH batched vs streamed: worst 10 s window SNR {worst:.1} dB");
    if let Some(dir) = env_path("MEETILY_BENCH_OUT") {
        // Raw 48 kHz mono f32 little-endian.
        let write = |name: &str, x: &[f32]| {
            std::fs::write(dir.join(name), x.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        };
        write("dfn-streamed.f32", &streamed);
        write("dfn-batched.f32", &out);
    }
}

/// Alignment check for dfn_batch: libDF's streamed local SNR per step against
/// the batched one at frame shifts -4..=4 (the right shift has ~0 error).
#[test]
#[ignore]
fn room_cleanup_dfn_alignment() {
    use crate::audio::room_cleanup::dfn_batch::{enhance_with_snr, DfnGraphs, OrtDfn};
    use df::tract::{DfParams, DfTract, ReduceMask, RuntimeParams};
    let mic = load_48k(&env_path("MEETILY_BENCH_MIC").expect("MEETILY_BENCH_MIC"));
    let tar = std::fs::read(env_path("MEETILY_DFN_ONNX_TAR").expect("MEETILY_DFN_ONNX_TAR")).unwrap();
    let input: Vec<f32> = mic.iter().map(|v| v.clamp(-0.9999, 0.9999)).collect();
    let params = RuntimeParams::default_with_ch(1).with_atten_lim(12.0).with_thresholds(-15.0, 35.0, 35.0).with_mask_reduce(ReduceMask::MAX);
    let mut model = DfTract::new(DfParams::default(), &params).unwrap();
    let hop = model.hop_size;
    let mut noisy = ndarray015::Array2::<f32>::zeros((1, hop));
    let mut enh = ndarray015::Array2::<f32>::zeros((1, hop));
    let mut streamed = Vec::new();
    for frame in input.chunks_exact(hop) {
        noisy.as_slice_mut().unwrap().copy_from_slice(frame);
        streamed.push(model.process(noisy.view(), enh.view_mut()).unwrap());
    }
    let mut ort = OrtDfn::new(&DfnGraphs::from_targz(&tar).unwrap(), 2).unwrap();
    let (_, batched) = enhance_with_snr(&mut ort, &input[..streamed.len() * hop]).unwrap();
    for shift in -4i64..=4 {
        let pairs: Vec<(f32, f32)> = (0..streamed.len() as i64)
            .filter_map(|t| { let u = t + shift; (u >= 0 && (u as usize) < batched.len()).then(|| (streamed[t as usize], batched[u as usize])) })
            .collect();
        let mean_abs = pairs.iter().map(|(a, b)| (a - b).abs() as f64).sum::<f64>() / pairs.len() as f64;
        println!("BENCH lsnr shift {shift:+}: mean |streamed - batched| {mean_abs:.3} dB");
    }
    for t in [0usize, 1, 2, 3, 4, 5, 8, 12, 20, 30, 50, 80, 120, 200, 400] {
        println!("BENCH lsnr frame {t}: streamed {:.3} batched {:.3}", streamed[t], batched[t]);
    }
    // Error by position within a block, at shift 0: warm-up problems show at the start.
    let block = crate::audio::room_cleanup::dfn_batch::BLOCK_FRAMES;
    for (label, range) in [("block frames 0-20", 0..20), ("20-100", 20..100), ("100-300", 100..300), ("300-600", 300..600), ("600-1000", 600..block)] {
        let errs: Vec<f64> = (0..streamed.len()).filter(|t| range.contains(&(t % block))).map(|t| (streamed[t] - batched[t]).abs() as f64).collect();
        println!("BENCH lsnr {label}: mean abs error {:.3} dB", errs.iter().sum::<f64>() / errs.len().max(1) as f64);
    }
}
