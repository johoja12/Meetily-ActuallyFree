//! Continuous remote-audio diarization. Inference never runs on the capture thread.
//! Microphone provenance remains authoritative for "You"; remote channels are
//! meeting-local identities, not voiceprints or names carried between recordings.
use anyhow::{anyhow, Result};
use std::{collections::VecDeque, sync::{mpsc, Arc, Condvar, Mutex}, thread, time::Duration};
use tauri::Emitter;
use super::nemotron::{NemotronDiarizationModel, StreamingSpeakerSegment, SAMPLES_PER_FRAME};

const RATE: u64 = 16_000;
/// How long a transcript turn waits for its speaker. A turn's end is covered
/// once capture passes it by up to one stride plus lookahead (2.32 s on ONNX,
/// 1.04 s on CoreML) and that step's inference; ASR has usually taken ~1 s.
const LABEL_WAIT: Duration = Duration::from_millis(2500);

/// Samples emitted per model step and held back as lookahead. Comes from the
/// loaded model: CoreML and ONNX run different live profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cadence { stride: u64, lookahead: u64 }
impl Cadence {
    fn of(model: &NemotronDiarizationModel) -> Self {
        let (chunk, right_context) = model.streaming_frames();
        Self { stride: (chunk * SAMPLES_PER_FRAME) as u64, lookahead: (right_context * SAMPLES_PER_FRAME) as u64 }
    }
    /// Audio before this sample has final labels once `cursor` samples were fed.
    fn ready_until(self, cursor: u64) -> u64 { cursor.saturating_sub(self.lookahead) / self.stride * self.stride }
}
const HISTORY: u64 = 10 * 60 * RATE;

#[derive(Default)]
struct Timeline {
    segments: VecDeque<StreamingSpeakerSegment>,
    ready_until: u64,
    finished: bool,
    failed: bool,
}
impl Timeline {
    fn append(&mut self, segments: Vec<StreamingSpeakerSegment>, ready_until: u64) {
        self.segments.extend(segments);
        self.ready_until = ready_until;
        let oldest = ready_until.saturating_sub(HISTORY);
        self.segments.retain(|s| s.end >= oldest);
    }

    fn label(&self, start: u64, end: u64) -> Option<String> {
        if self.failed || end <= start || start < self.ready_until.saturating_sub(HISTORY) { return None; }
        let mut coverage = [0u64; 8];
        for s in &self.segments {
            if s.speaker_id < coverage.len() {
                coverage[s.speaker_id] += s.end.min(end).saturating_sub(s.start.max(start));
            }
        }
        let best = *coverage.iter().max()?;
        if best == 0 { return None; }
        // A transcript turn may include multiple speakers. Do not fabricate word
        // alignment or concatenate identities into a new pseudo-speaker label.
        let speaker = coverage.iter().position(|&value| value == best)?;
        Some(format!("Speaker {}", speaker + 1))
    }
}

struct Audio { start: u64, samples: Vec<f32> }
struct Session {
    sender: Option<mpsc::SyncSender<Audio>>,
    shared: Arc<(Mutex<Timeline>, Condvar)>,
    worker: Option<thread::JoinHandle<()>>,
    notify: Arc<dyn Fn(String) + Send + Sync>,
}
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn report_failure(notify: &Arc<dyn Fn(String) + Send + Sync>, shared: &Arc<(Mutex<Timeline>, Condvar)>, message: String) {
    log::error!("Live Nemotron: {message}");
    if let Ok(mut state) = shared.0.lock() { state.failed = true; }
    shared.1.notify_all();
    notify(format!("Nemotron live speaker labeling stopped: {message}. Transcription continues with source labels."));
}

pub fn start<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<()> {
    stop();
    let cfg = super::get_diarization_config();
    let model = NemotronDiarizationModel::new_live(
        super::diarization_user_model_dir().join(super::nemotron::NEMOTRON_MODEL_FILENAME),
        8, cfg.nemotron_threshold,
    )?;
    let cadence = Cadence::of(&model);
    let runtime = model.runtime();
    let notify: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |message| { let _ = app.emit("live-diarization-error", message); });
    let session = spawn_session(model, notify)?;
    *SESSION.lock().map_err(|_| anyhow!("Live Nemotron lock poisoned"))? = Some(session);
    log::info!("Live Nemotron started on {runtime:?}: {:.2} s buffering, continuous system audio, 8 speaker channels",
        (cadence.stride + cadence.lookahead) as f64 / RATE as f64);
    Ok(())
}

fn spawn_session(mut model: NemotronDiarizationModel, notify: Arc<dyn Fn(String) + Send + Sync>) -> Result<Session> {
    let cadence = Cadence::of(&model);
    // 30 s of 50 ms capture windows. An overloaded queue disables labeling
    // explicitly instead of blocking capture or silently dropping timeline audio.
    let (sender, receiver) = mpsc::sync_channel::<Audio>(600);
    let shared = Arc::new((Mutex::new(Timeline::default()), Condvar::new()));
    let state = shared.clone();
    let worker_notify = notify.clone();
    let worker = thread::Builder::new().name("nemotron-live".into()).spawn(move || {
        let result = (|| -> Result<()> {
            let mut cursor = 0u64;
            for audio in receiver {
                if state.0.lock().map_err(|_| anyhow!("Timeline lock poisoned"))?.failed { return Ok(()); }
                // Device gaps and pause/resume use recording-relative VAD time.
                // Feed silence in bounded pieces, preserving the model's cache.
                while cursor < audio.start {
                    if state.0.lock().map_err(|_| anyhow!("Timeline lock poisoned"))?.failed { return Ok(()); }
                    let count = (audio.start - cursor).min(RATE) as usize;
                    let segments = model.feed(&vec![0.0; count])?;
                    cursor += count as u64;
                    publish(&state, cadence, segments, cursor, false)?;
                }
                let skip = cursor.saturating_sub(audio.start).min(audio.samples.len() as u64) as usize;
                let samples = &audio.samples[skip..];
                let segments = model.feed(samples)?;
                cursor += samples.len() as u64;
                publish(&state, cadence, segments, cursor, false)?;
            }
            let segments = model.flush()?;
            publish(&state, cadence, segments, cursor, true)
        })();
        if let Err(error) = result { report_failure(&worker_notify, &state, error.to_string()); }
    })?;
    Ok(Session {
        sender: Some(sender), shared, worker: Some(worker), notify,
    })
}

fn publish(shared: &Arc<(Mutex<Timeline>, Condvar)>, cadence: Cadence, segments: Vec<StreamingSpeakerSegment>, cursor: u64, finished: bool) -> Result<()> {
    let ready = if finished { cursor } else { cadence.ready_until(cursor) };
    let mut state = shared.0.lock().map_err(|_| anyhow!("Timeline lock poisoned"))?;
    state.append(segments, ready);
    state.finished = finished;
    shared.1.notify_all();
    Ok(())
}

pub fn active() -> bool { SESSION.lock().map(|s| s.is_some()).unwrap_or(false) }

pub fn feed(start: u64, samples: &[f32]) {
    let Ok(mut guard) = SESSION.lock() else { return; };
    let Some(session) = guard.as_mut() else { return; };
    let Some(sender) = &session.sender else { return; };
    if sender.try_send(Audio { start, samples: samples.to_vec() }).is_err() {
        session.sender.take();
        report_failure(&session.notify, &session.shared, "Inference queue unavailable or overloaded".into());
    }
}

/// Closing input drains queued audio and flushes the final lookahead. Keep the
/// timeline alive until the transcription worker has consumed the final turns.
pub fn finish() {
    if let Ok(mut guard) = SESSION.lock() {
        if let Some(session) = guard.as_mut() { session.sender.take(); }
    }
}

pub fn label(start_seconds: f64, duration: f64) -> Option<String> {
    if !start_seconds.is_finite() || !duration.is_finite() || duration <= 0.0 { return None; }
    let shared = SESSION.lock().ok()?.as_ref()?.shared.clone();
    let start = (start_seconds.max(0.0) * RATE as f64).round() as u64;
    let end = start.saturating_add((duration * RATE as f64).round() as u64);
    let state = shared.0.lock().ok()?;
    let (state, _) = shared.1.wait_timeout_while(state, LABEL_WAIT, |s| {
        !s.failed && !s.finished && s.ready_until < end
    }).ok()?;
    state.label(start, end)
}

pub fn stop() {
    let session = SESSION.lock().ok().and_then(|mut s| s.take());
    if let Some(mut session) = session {
        session.sender.take();
        if let Some(worker) = session.worker.take() { let _ = worker.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires pinned model and multispeaker fixture"]
    fn live_nemotron_worker_drains_and_keeps_final_turns_available() {
        let path = std::env::var("MEETILY_NEMOTRON_MODEL").unwrap();
        let wav = std::env::var("MEETILY_NEMOTRON_WAV").unwrap();
        let (audio, rate) = super::super::dsp::read_wav(std::path::Path::new(&wav)).unwrap();
        assert_eq!(rate, 16000);
        let mut model = NemotronDiarizationModel::onnx(path.as_ref(), 0.5).unwrap();
        model.enable_live_streaming().unwrap();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let reported = errors.clone();
        let mut session = spawn_session(model, Arc::new(move |message| reported.lock().unwrap().push(message))).unwrap();
        for (i, samples) in audio.chunks(800).enumerate() {
            session.sender.as_ref().unwrap().send(Audio { start: (i * 800) as u64, samples: samples.to_vec() }).unwrap();
        }
        session.sender.take();
        session.worker.take().unwrap().join().unwrap();
        assert!(errors.lock().unwrap().is_empty());
        let state = session.shared.0.lock().unwrap();
        assert!(state.finished);
        assert_eq!(state.ready_until, audio.len() as u64);
        assert!(state.label(0, 10 * RATE).is_some());
        assert!(state.label(audio.len() as u64 - 10 * RATE, audio.len() as u64).is_some());
    }
    /// Qualification for live profiles: steady-state (second-half) real-time
    /// factor fed in 50 ms windows as capture does, and agreement with the ONNX
    /// offline profile on frames where it hears exactly one speaker. Runs the
    /// ONNX live profile and NVIDIA's 1.04 s preset; with
    /// `MEETILY_NEMOTRON_COREML_DIR`, also CoreML's live and offline models.
    #[test]
    #[ignore = "Requires pinned model and a 16 kHz WAV; real-model qualification"]
    fn live_nemotron_live_profile_keeps_up_and_matches_offline() {
        use super::super::nemotron::StreamingProfile;
        let path = std::env::var("MEETILY_NEMOTRON_MODEL").unwrap();
        let wav = std::env::var("MEETILY_NEMOTRON_WAV").unwrap();
        let coreml = std::env::var("MEETILY_NEMOTRON_COREML_DIR").ok();
        let (audio, rate) = super::super::dsp::read_wav(std::path::Path::new(&wav)).unwrap();
        assert_eq!(rate, 16000);
        let frames = audio.len() / 160;
        let onnx = |profile: Option<StreamingProfile>| {
            let mut model = NemotronDiarizationModel::onnx(path.as_ref(), 0.5).unwrap();
            match profile {
                Some(profile) => model.set_profile_for_test(profile).unwrap(),
                None => model.enable_live_streaming().unwrap(),
            }
            model
        };
        let run = |mut model: NemotronDiarizationModel| {
            let mut segments = Vec::new();
            let half = audio.len() / 2;
            let mut tail = std::time::Duration::ZERO;
            for (i, samples) in audio.chunks(800).enumerate() {
                let t = std::time::Instant::now();
                segments.extend(model.feed(samples).unwrap());
                if i * 800 >= half { tail += t.elapsed(); }
            }
            segments.extend(model.flush().unwrap());
            let mut active = vec![0u8; frames];
            for s in segments {
                for f in (s.start / 160) as usize..((s.end / 160) as usize).min(frames) { active[f] |= 1 << s.speaker_id; }
            }
            (active, tail.as_secs_f64() / ((audio.len() - half) as f64 / RATE as f64))
        };
        // Optional: one byte per 10 ms frame (bit = speaker channel) for external comparison.
        let dump = |name: &str, active: &[u8]| {
            if let Ok(dir) = std::env::var("MEETILY_NEMOTRON_DUMP_DIR") {
                std::fs::write(std::path::Path::new(&dir).join(format!("meetily-{name}.bin")), active).unwrap();
            }
        };
        let (reference, _) = run(onnx(Some(StreamingProfile::offline())));
        dump("offline", &reference);
        // (name, model, asserted production profile)
        let mut candidates: Vec<(&str, Box<dyn Fn() -> NemotronDiarizationModel>, bool)> = vec![
            ("onnx-nvidia-1.04s", Box::new(|| onnx(Some(StreamingProfile::low_latency()))), false),
            ("onnx-live", Box::new(|| onnx(None)), true),
        ];
        if let Some(dir) = coreml.as_deref() {
            let dir = std::path::PathBuf::from(dir);
            let live_dir = dir.clone();
            candidates.push(("coreml-live", Box::new(move || NemotronDiarizationModel::coreml(&live_dir, 0.5, true).unwrap()), true));
            candidates.push(("coreml-offline", Box::new(move || NemotronDiarizationModel::coreml(&dir, 0.5, false).unwrap()), false));
        }
        for (name, build, production) in candidates {
            let model = build();
            let (chunk, rc) = model.streaming_frames();
            let (active, tail_rtf) = run(model);
            dump(name, &active);
            // Map candidate channels to reference channels by greatest co-activity.
            let mut overlap = [[0usize; 8]; 8];
            for (r, c) in reference.iter().zip(&active) {
                for i in 0..8 { for j in 0..8 { if r & (1 << i) != 0 && c & (1 << j) != 0 { overlap[i][j] += 1; } } }
            }
            let map: Vec<usize> = (0..8).map(|j| (0..8).max_by_key(|&i| overlap[i][j]).unwrap()).collect();
            let (mut single, mut agree) = (0, 0);
            for (r, c) in reference.iter().zip(&active) {
                if r.count_ones() == 1 {
                    single += 1;
                    let mapped = (0..8).filter(|&j| c & (1 << j) != 0).fold(0u8, |m, j| m | (1 << map[j]));
                    if mapped & r != 0 { agree += 1; }
                }
            }
            let identical = reference.iter().zip(&active).filter(|(r, c)| r == c).count() as f64 / frames as f64;
            let agreement = agree as f64 / single.max(1) as f64;
            println!("{name}: chunk={chunk} rc={rc} buffering {:.2} s, steady RTF {tail_rtf:.3}, single-speaker agreement with ONNX offline {:.1}% of {single} frames, identical frames {:.2}%",
                (chunk + rc) as f64 * 0.08, 100.0 * agreement, 100.0 * identical);
            if production {
                // Headroom for ASR sharing the CPU; 0.8 already overflows within a meeting.
                assert!(tail_rtf < 0.4, "{name} too slow: RTF {tail_rtf:.3}");
                assert!(agreement > 0.97, "{name} diverges from offline: {agreement:.3}");
            }
            if name == "coreml-offline" {
                // Same checkpoint and profile on another runtime (fp16 on the ANE/GPU).
                assert!(identical > 0.99, "CoreML offline diverges from ONNX offline: {identical:.4}");
            }
        }
    }
    fn segment(start: u64, end: u64, speaker_id: usize) -> StreamingSpeakerSegment {
        StreamingSpeakerSegment { start, end, speaker_id }
    }
    #[test]
    fn labels_follow_overlap_not_the_last_detected_speaker() {
        let mut timeline = Timeline::default();
        timeline.append(vec![segment(0, RATE * 2, 2), segment(RATE * 2, RATE * 3, 0)], RATE * 3);
        assert_eq!(timeline.label(0, RATE), Some("Speaker 3".into()));
        assert_eq!(timeline.label(RATE * 2, RATE * 3), Some("Speaker 1".into()));
        assert_eq!(timeline.label(RATE * 4, RATE * 5), None);
        timeline.failed = true;
        assert_eq!(timeline.label(0, RATE), None);
    }
    #[test]
    fn history_is_bounded_and_old_turns_are_not_assigned_new_identities() {
        let mut timeline = Timeline::default();
        timeline.append(vec![segment(0, RATE, 0), segment(HISTORY, HISTORY + RATE, 1)], HISTORY + RATE * 2);
        assert_eq!(timeline.segments.len(), 1);
        assert_eq!(timeline.label(0, RATE), None);
        assert_eq!(timeline.label(HISTORY, HISTORY + RATE), Some("Speaker 2".into()));
    }
    /// The ONNX live profile (25 + 4) and CoreML's NVIDIA preset (9 + 4).
    const CADENCES: [Cadence; 2] = [
        Cadence { stride: 25 * 1280, lookahead: 4 * 1280 },
        Cadence { stride: 9 * 1280, lookahead: 4 * 1280 },
    ];
    #[test]
    fn flush_publishes_the_buffered_tail() {
        for cadence in CADENCES {
            let Cadence { stride, lookahead } = cadence;
            let shared = Arc::new((Mutex::new(Timeline::default()), Condvar::new()));
            publish(&shared, cadence, vec![], stride + lookahead - 1, false).unwrap();
            assert_eq!(shared.0.lock().unwrap().ready_until, 0);
            publish(&shared, cadence, vec![], stride + lookahead, false).unwrap();
            assert_eq!(shared.0.lock().unwrap().ready_until, stride);
            publish(&shared, cadence, vec![segment(stride, stride + lookahead, 1)], stride + lookahead, true).unwrap();
            let state = shared.0.lock().unwrap();
            assert!(state.finished);
            assert_eq!(state.ready_until, stride + lookahead);
        }
    }
    #[test]
    fn cadence_follows_the_loaded_profile() {
        // ready_until only advances in whole strides once the lookahead is buffered.
        let onnx = CADENCES[0];
        assert_eq!(onnx.ready_until(onnx.stride * 3 + onnx.lookahead), onnx.stride * 3);
        assert_eq!(onnx.ready_until(onnx.stride * 3 + onnx.lookahead - 1), onnx.stride * 2);
        let coreml = CADENCES[1];
        assert_eq!(coreml.ready_until(RATE * 10), (RATE * 10 - coreml.lookahead) / coreml.stride * coreml.stride);
    }
}
