//! Packs VAD speech segments into longer decode windows for batch Whisper runs.
//!
//! Whisper encodes a fixed 30 s window whatever the input length, so decoding
//! 5 s VAD segments one by one makes the encoder do ~6x the necessary work
//! (issue #14). A window concatenates consecutive segments of one source,
//! filling the gaps between them with silence, so window time maps linearly to
//! recording time: `recording_ms = window.start_ms + offset_ms`. Gaps longer
//! than `max_gap_ms` start a new window rather than feeding Whisper long
//! silences, which invite hallucinations.

use crate::audio::vad::SpeechSegment;

const SAMPLES_PER_MS: f64 = 16.0; // 16 kHz mono

#[derive(Debug, Clone)]
pub struct SpeechWindow {
    /// Recording time of the first sample, in milliseconds.
    pub start_ms: f64,
    /// 16 kHz mono samples; gaps between the packed segments are zeros.
    pub samples: Vec<f32>,
    /// Source provenance shared by every packed segment ("You", "Guest", "Room").
    pub speaker_hint: Option<&'static str>,
    /// How many VAD segments were packed into this window.
    pub segment_count: usize,
}

impl SpeechWindow {
    pub fn end_ms(&self) -> f64 {
        self.start_ms + self.samples.len() as f64 / SAMPLES_PER_MS
    }

    /// Samples covering `[start_ms, end_ms)` of recording time, clamped to the window.
    pub fn slice_ms(&self, start_ms: f64, end_ms: f64) -> &[f32] {
        let to_index = |ms: f64| {
            (((ms - self.start_ms) * SAMPLES_PER_MS).round().max(0.0) as usize).min(self.samples.len())
        };
        let (a, b) = (to_index(start_ms), to_index(end_ms));
        &self.samples[a..b.max(a)]
    }
}

/// Packs segments (already sorted by start time, possibly interleaving sources)
/// into windows of at most `max_samples`. Sources never share a window.
pub fn pack_speech_windows(
    segments: &[(SpeechSegment, Option<&'static str>)],
    max_samples: usize,
    max_gap_ms: f64,
) -> Vec<SpeechWindow> {
    let mut open: Vec<SpeechWindow> = Vec::new(); // at most one open window per source
    let mut done: Vec<SpeechWindow> = Vec::new();

    for (segment, hint) in segments {
        let slot = open.iter().position(|w| w.speaker_hint == *hint);
        if let Some(index) = slot {
            let window = &open[index];
            let gap_ms = segment.start_timestamp_ms - window.end_ms();
            // A VAD segment can start a few samples before the previous one ended;
            // drop the overlap so the linear time mapping still holds.
            let skip = if gap_ms < 0.0 { ((-gap_ms) * SAMPLES_PER_MS).round() as usize } else { 0 };
            let gap = if gap_ms > 0.0 { (gap_ms * SAMPLES_PER_MS).round() as usize } else { 0 };
            let added = gap + segment.samples.len().saturating_sub(skip);
            if gap_ms > max_gap_ms || window.samples.len() + added > max_samples {
                done.push(open.swap_remove(index));
            } else {
                let window = &mut open[index];
                window.samples.resize(window.samples.len() + gap, 0.0);
                window.samples.extend_from_slice(&segment.samples[skip.min(segment.samples.len())..]);
                window.segment_count += 1;
                continue;
            }
        }
        open.push(SpeechWindow {
            start_ms: segment.start_timestamp_ms,
            samples: segment.samples.clone(),
            speaker_hint: *hint,
            segment_count: 1,
        });
    }

    done.extend(open);
    done.sort_by(|a, b| a.start_ms.partial_cmp(&b.start_ms).unwrap_or(std::cmp::Ordering::Equal));
    done
}

/// Whisper batch windows: up to 25 s of packed speech, split at gaps over 1.5 s.
pub const WHISPER_WINDOW_SAMPLES: usize = 25 * 16_000;
pub const WHISPER_WINDOW_MAX_GAP_MS: f64 = 1_500.0;
/// Trailing silence so whisper.cpp still decodes a short final utterance.
const WHISPER_TRAILING_PAD_SAMPLES: usize = 16_000;
/// Greedy. Until issue #22 both settings actually decoded with one beam at
/// temperature 0.2 (whisper.cpp ignores the beam size above temperature 0),
/// so the comparison in issue #14 measured noise. With real beam search, on
/// three AMI far-field test meetings (55 min, manual transcripts) WER was
/// greedy 31.8%, beam 3 32.2%, beam 5 34.1%, and the old setting 31.8%;
/// beam 3 and 5 took 21-58% longer than greedy (M4, CoreML encoder).
pub const WHISPER_BATCH_DECODING: crate::whisper_engine::BatchDecoding =
    crate::whisper_engine::BatchDecoding::Greedy;

/// RMS and peak of 16 kHz samples.
pub fn signal_level(samples: &[f32]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let rms = (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt();
    let peak = samples.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    (rms, peak)
}

/// A transcribed span in recording time.
pub struct PackedText {
    pub text: String,
    pub start_ms: f64,
    pub end_ms: f64,
    /// RMS of the audio behind the text, for silence-hallucination checks.
    pub rms: f32,
    pub speaker_hint: Option<&'static str>,
}

/// Decodes `segments` with Whisper in packed windows. `before_window(index,
/// count, window)` runs before each window and returns false to cancel.
pub async fn transcribe_packed(
    engine: &crate::whisper_engine::WhisperEngine,
    segments: &[(SpeechSegment, Option<&'static str>)],
    language: Option<String>,
    initial_prompt: Option<&str>,
    mut before_window: impl FnMut(usize, usize, &SpeechWindow) -> bool,
) -> anyhow::Result<Option<Vec<PackedText>>> {
    let windows = pack_speech_windows(segments, WHISPER_WINDOW_SAMPLES, WHISPER_WINDOW_MAX_GAP_MS);
    log::info!(
        "Processing {} speech segments in {} Whisper windows ({:?} decoding)",
        segments.len(), windows.len(), WHISPER_BATCH_DECODING
    );
    let mut out = Vec::new();
    for (index, window) in windows.iter().enumerate() {
        if !before_window(index, windows.len(), window) {
            return Ok(None);
        }
        let mut padded = Vec::with_capacity(window.samples.len() + WHISPER_TRAILING_PAD_SAMPLES);
        padded.extend_from_slice(&window.samples);
        padded.resize(window.samples.len() + WHISPER_TRAILING_PAD_SAMPLES, 0.0);
        let timed = engine
            .transcribe_timed(&padded, language.clone(), initial_prompt, WHISPER_BATCH_DECODING)
            .await
            .map_err(|e| anyhow::anyhow!("Whisper transcription failed at {:.1}s: {}", window.start_ms / 1000.0, e))?;
        for segment in timed {
            // Window time maps linearly to recording time; the trailing pad is clamped off.
            let end_ms = (window.start_ms + segment.end_ms).min(window.end_ms());
            let start_ms = (window.start_ms + segment.start_ms).min(end_ms);
            let (rms, _) = signal_level(window.slice_ms(start_ms, end_ms));
            out.push(PackedText { text: segment.text, start_ms, end_ms, rms, speaker_hint: window.speaker_hint });
        }
    }
    Ok(Some(out))
}

/// Text-length confidence heuristic shared with the per-segment path.
pub fn text_confidence(text: &str) -> f32 {
    (text.len() as f32 / 100.0).min(0.9) + 0.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: f64, len_ms: f64, value: f32) -> SpeechSegment {
        SpeechSegment {
            samples: vec![value; (len_ms * SAMPLES_PER_MS) as usize],
            start_timestamp_ms: start_ms,
            end_timestamp_ms: start_ms + len_ms,
            confidence: 1.0,
        }
    }

    #[test]
    fn adjacent_segments_share_a_window_and_keep_recording_time() {
        let segments = vec![
            (seg(1_000.0, 5_000.0, 0.1), Some("Room")),
            (seg(6_000.0, 5_000.0, 0.2), Some("Room")),
            (seg(11_500.0, 2_000.0, 0.3), Some("Room")),
        ];
        let windows = pack_speech_windows(&segments, 30 * 16_000, 1_500.0);
        assert_eq!(windows.len(), 1);
        let w = &windows[0];
        assert_eq!(w.segment_count, 3);
        assert_eq!(w.start_ms, 1_000.0);
        assert_eq!(w.end_ms(), 13_500.0);
        // The 500 ms gap is silence, and the third segment lands at its recording time.
        assert!(w.slice_ms(11_000.0, 11_500.0).iter().all(|&s| s == 0.0));
        assert!(w.slice_ms(11_500.0, 13_500.0).iter().all(|&s| s == 0.3));
    }

    #[test]
    fn long_gaps_and_full_windows_start_new_windows() {
        let segments = vec![
            (seg(0.0, 5_000.0, 0.1), None),
            (seg(9_000.0, 5_000.0, 0.1), None),   // 4 s gap
            (seg(14_000.0, 21_000.0, 0.1), None), // would exceed 25 s
        ];
        let windows = pack_speech_windows(&segments, 25 * 16_000, 1_500.0);
        let spans: Vec<(f64, f64)> = windows.iter().map(|w| (w.start_ms, w.end_ms())).collect();
        assert_eq!(spans, vec![(0.0, 5_000.0), (9_000.0, 14_000.0), (14_000.0, 35_000.0)]);
    }

    #[test]
    fn sources_never_share_a_window() {
        let segments = vec![
            (seg(0.0, 3_000.0, 0.1), Some("You")),
            (seg(1_000.0, 3_000.0, 0.2), Some("Guest")),
            (seg(3_000.0, 3_000.0, 0.1), Some("You")),
            (seg(4_000.0, 3_000.0, 0.2), Some("Guest")),
        ];
        let windows = pack_speech_windows(&segments, 30 * 16_000, 1_500.0);
        assert_eq!(windows.len(), 2);
        for w in &windows {
            assert_eq!(w.segment_count, 2);
            let expected = if w.speaker_hint == Some("You") { 0.1 } else { 0.2 };
            assert!(w.samples.iter().all(|&s| s == expected));
        }
        assert_eq!(windows[0].speaker_hint, Some("You"));
    }

    #[test]
    fn overlapping_segment_is_trimmed_to_keep_the_time_mapping() {
        let segments = vec![
            (seg(0.0, 5_000.0, 0.1), None),
            (seg(4_900.0, 1_100.0, 0.2), None), // starts 100 ms early
        ];
        let windows = pack_speech_windows(&segments, 30 * 16_000, 1_500.0);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].end_ms(), 6_000.0);
        assert!(windows[0].slice_ms(5_000.0, 6_000.0).iter().all(|&s| s == 0.2));
    }
}

/// Real-audio comparison for issue #14; nothing private is checked in.
///
/// MEETILY_BENCH_AUDIO=/path/mic.mp4 MEETILY_BENCH_MODELS=~/Library/Application\ Support/Meetily/models \
/// cargo test --release -p meetily --lib --features metal speech_windows::bench -- --ignored --nocapture
#[cfg(test)]
mod bench {
    use super::*;
    use crate::whisper_engine::{BatchDecoding, WhisperEngine};
    use std::time::Instant;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
            .filter(|w| !w.is_empty())
            .collect()
    }

    /// Word edit distance divided by the reference length.
    fn disagreement(reference: &[String], other: &[String]) -> f64 {
        let mut prev: Vec<usize> = (0..=other.len()).collect();
        for (i, r) in reference.iter().enumerate() {
            let mut cur = vec![i + 1; other.len() + 1];
            for (j, o) in other.iter().enumerate() {
                cur[j + 1] = (prev[j] + usize::from(r != o)).min(prev[j + 1] + 1).min(cur[j] + 1);
            }
            prev = cur;
        }
        prev[other.len()] as f64 / reference.len().max(1) as f64
    }

    #[tokio::test]
    #[ignore = "needs a local recording and Whisper model"]
    async fn bench() {
        let audio = std::env::var("MEETILY_BENCH_AUDIO").expect("MEETILY_BENCH_AUDIO");
        let models = std::env::var("MEETILY_BENCH_MODELS").expect("MEETILY_BENCH_MODELS");
        let model = std::env::var("MEETILY_BENCH_MODEL").unwrap_or_else(|_| "large-v3-turbo-q5_0".into());
        let max_window_s: usize = std::env::var("MEETILY_BENCH_WINDOW_S").ok().and_then(|v| v.parse().ok()).unwrap_or(25);
        let skip_baseline = std::env::var("MEETILY_BENCH_SKIP_BASELINE").is_ok();
        let out_dir = std::env::var("MEETILY_BENCH_OUT").ok();
        let only_beam = std::env::var("MEETILY_BENCH_ONLY_BEAM").is_ok();

        let samples = crate::audio::decoder::decode_audio_file(std::path::Path::new(&audio)).unwrap().to_whisper_format();
        let minutes = samples.len() as f64 / 16_000.0 / 60.0;
        let segments = crate::audio::vad::get_speech_chunks_with_thresholds_and_progress(&samples, 2000, 0.20, 0.10, |_, _| true).unwrap();
        let mut split = Vec::new();
        for s in &segments {
            for part in crate::audio::common::split_segment_at_silence(s, 25 * 16_000) {
                let rms = (part.samples.iter().map(|x| x * x).sum::<f32>() / part.samples.len().max(1) as f32).sqrt();
                let peak = part.samples.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
                if part.samples.len() >= 1600 && !(rms < 0.005 && peak < 0.01) {
                    split.push((part, Some("Room")));
                }
            }
        }
        let engine = WhisperEngine::new_with_models_dir(Some(models.into())).unwrap();
        engine.discover_models().await.unwrap();
        engine.load_model(&model).await.unwrap();
        println!("audio {minutes:.1} min, {} VAD segments, model {model}", split.len());

        let language = Some("en".to_string());
        let mut baseline_words = Vec::new();
        if !skip_baseline {
            let t = Instant::now();
            let mut dump = String::new();
            for (s, _) in &split {
                let (text, _, _) = engine.transcribe_audio_with_confidence(s.samples.clone(), language.clone(), None).await.unwrap();
                baseline_words.extend(words(&text));
                dump += &format!("{:.1}-{:.1} {}\n", s.start_timestamp_ms / 1000.0, s.end_timestamp_ms / 1000.0, text);
            }
            if let Some(dir) = &out_dir { std::fs::write(format!("{dir}/per-segment.txt"), dump).unwrap(); }
            let secs = t.elapsed().as_secs_f64();
            println!("per-segment beam: {secs:.1}s ({:.1}x real time), {} words", minutes * 60.0 / secs, baseline_words.len());
        }

        // Independent reference: Parakeet on the same segments.
        let mut reference_words = Vec::new();
        if let Ok(parakeet_dir) = std::env::var("MEETILY_BENCH_PARAKEET_MODELS") {
            let parakeet = crate::parakeet_engine::ParakeetEngine::new_with_models_dir(Some(parakeet_dir.into())).unwrap();
            parakeet.discover_models().await.unwrap();
            parakeet.load_model("parakeet-tdt-0.6b-v3-int8").await.unwrap();
            let t = Instant::now();
            for (s, _) in &split {
                reference_words.extend(words(&parakeet.transcribe_audio(s.samples.clone(), None).await.unwrap()));
            }
            println!("parakeet reference: {:.1}s, {} words", t.elapsed().as_secs_f64(), reference_words.len());
            if !baseline_words.is_empty() {
                println!("per-segment beam vs parakeet: {:.1}% word disagreement", 100.0 * disagreement(&reference_words, &baseline_words));
            }
        }

        let windows = pack_speech_windows(&split, max_window_s * 16_000, 1_500.0);
        let decodings: &[BatchDecoding] = if only_beam { &[BatchDecoding::Beam] } else { &[BatchDecoding::Beam, BatchDecoding::Greedy] };
        for &decoding in decodings {
            let t = Instant::now();
            let mut all = Vec::new();
            let mut rows = Vec::new();
            for w in &windows {
                let mut padded = w.samples.clone();
                padded.resize(padded.len() + 16_000, 0.0);
                for seg in engine.transcribe_timed(&padded, language.clone(), None, decoding).await.unwrap() {
                    all.extend(words(&seg.text));
                    rows.push((w.start_ms + seg.start_ms, (w.start_ms + seg.end_ms).min(w.end_ms()), seg.text));
                }
            }
            let secs = t.elapsed().as_secs_f64();
            if let Some(dir) = &out_dir {
                let dump: String = rows.iter().map(|(a, b, t)| format!("{:.1}-{:.1} {}\n", a / 1000.0, b / 1000.0, t)).collect();
                std::fs::write(format!("{dir}/packed-{decoding:?}.txt"), dump).unwrap();
            }
            let lens: Vec<f64> = rows.iter().map(|(a, b, _)| (b - a) / 1000.0).collect();
            let beyond = rows.iter().filter(|(a, b, _)| b < a).count();
            println!(
                "packed {decoding:?} ({} windows of <= {max_window_s}s): {secs:.1}s ({:.1}x real time), {} words, {} rows (avg {:.1}s, max {:.1}s, {beyond} inverted){}",
                windows.len(), minutes * 60.0 / secs, all.len(), rows.len(),
                lens.iter().sum::<f64>() / lens.len().max(1) as f64,
                lens.iter().cloned().fold(0.0, f64::max),
                if baseline_words.is_empty() { String::new() } else { format!(", {:.1}% word disagreement with per-segment beam", 100.0 * disagreement(&baseline_words, &all)) },
            );
            if !reference_words.is_empty() {
                println!("packed {decoding:?} vs parakeet: {:.1}% word disagreement", 100.0 * disagreement(&reference_words, &all));
            }
            for (a, b, text) in rows.iter().skip(40).take(3) {
                println!("   {:>7.1}-{:>7.1}s {}", a / 1000.0, b / 1000.0, text.chars().take(90).collect::<String>());
            }
        }
    }
}
