//! Cleaned playback copies of room-microphone recordings.
//!
//! A room microphone hears everyone through the room's reverb. After such a
//! recording is saved, a background worker writes `room-clean.m4a` next to it:
//! `mic.mp4` (room mode records no computer audio, so it is the whole meeting)
//! through WPE dereverberation and then DeepFilterNet limited to 12 dB of
//! attenuation, the chain chosen by listening (see ROOM_MICROPHONE.md), then
//! makeup gain back to the original's speech level (`level`).
//!
//! The copy is for listening only. Transcription, speaker labeling and voice
//! enrollment keep reading the untouched recording: on a measured room meeting
//! neither filter made Whisper, Parakeet or Nemotron transcripts agree more,
//! and full-strength DeepFilterNet removed words.
//!
//! Ownership and lifecycle: one native worker thread (started from app setup)
//! owns all processing, so jobs outlive any window. It runs at background
//! priority with a two-thread compute pool, never while a recording is active
//! (a job in progress stops within about a second when capture starts and is
//! retried after the recording ends), and writes to a temporary file that is
//! renamed into place only once complete, so a crash never leaves a truncated
//! copy. At startup it queues every completed room recording that lacks a copy.
pub mod dfn;
pub mod level;
pub mod wpe;

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// The cleaned playback copy, beside the recording.
pub const CLEANED_FILE: &str = "room-clean.m4a";
const SOURCE_FILE: &str = "mic.mp4";
const TEMP_FILE: &str = ".room-clean.m4a.part";
/// Emitted with `{ "folder": … }` when a copy is written.
pub const CLEANED_EVENT: &str = "room-playback-cleaned";
/// Wait this long after a recording ends before resuming, so stopping and the
/// post-call steps that follow get the machine first.
const AFTER_RECORDING_GRACE: Duration = Duration::from_secs(60);
const SAMPLE_RATE: usize = dfn::SAMPLE_RATE;
/// AAC for listening: speech at 128 kbps is transparent and keeps the extra
/// copy smaller than the 192 kbps original.
const OUTPUT_ARGS: &[&str] = &["-c:a", "aac", "-b:a", "128k", "-profile:a", "aac_low", "-movflags", "+faststart", "-f", "mp4"];

/// The cleaned copy for playback, when one has been written.
pub fn cleaned_playback_path(folder: &Path) -> Option<PathBuf> {
    let path = folder.join(CLEANED_FILE);
    std::fs::metadata(&path).ok().filter(|m| m.is_file() && m.len() > 0).map(|_| path)
}

/// A completed room-microphone recording with its microphone track and no copy yet.
pub fn needs_cleanup(folder: &Path) -> bool {
    let completed = std::fs::read(folder.join("metadata.json"))
        .ok()
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
        .is_some_and(|m| m.get("status").and_then(|s| s.as_str()) == Some("completed"));
    completed
        && super::recording_saver::is_room_microphone_recording(folder)
        && folder.join(SOURCE_FILE).is_file()
        && !folder.join(CLEANED_FILE).exists()
}

pub enum Outcome {
    Written { audio_seconds: f64, elapsed: Duration },
    /// `should_stop` asked to stop; nothing was written.
    Stopped,
}

/// Write the cleaned copy for `folder`. Polls `should_stop` about once per
/// second of audio processed.
pub fn clean_recording(folder: &Path, should_stop: &dyn Fn() -> bool) -> Result<Outcome> {
    let ffmpeg = super::ffmpeg::find_ffmpeg_path().ok_or_else(|| anyhow!("FFmpeg is not available"))?;
    let source = folder.join(SOURCE_FILE);
    let temp = folder.join(TEMP_FILE);
    let started = Instant::now();
    let mut decoder = spawn(
        Command::new(&ffmpeg)
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(&source)
            .args(["-map", "0:a:0", "-ac", "1", "-ar", &SAMPLE_RATE.to_string(), "-f", "f32le", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .context("starting the FFmpeg decoder")?;
    let mut encoder = match spawn(
        Command::new(&ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-f", "f32le", "-ar", &SAMPLE_RATE.to_string(), "-ac", "1", "-i", "pipe:0"])
            .args(OUTPUT_ARGS)
            .arg(&temp)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    ) {
        Ok(child) => child,
        Err(e) => {
            let _ = decoder.kill();
            let _ = decoder.wait();
            return Err(e.context("starting the FFmpeg encoder"));
        }
    };
    let result = run_chain(&mut decoder, &mut encoder, should_stop);
    let finished = match result {
        Ok(Some(samples)) => {
            let encoded = encoder.wait_with_output().context("waiting for the FFmpeg encoder")?;
            let decoded = decoder.wait_with_output().context("waiting for the FFmpeg decoder")?;
            if !decoded.status.success() {
                Err(anyhow!("FFmpeg could not read {}: {}", SOURCE_FILE, String::from_utf8_lossy(&decoded.stderr).trim()))
            } else if !encoded.status.success() {
                Err(anyhow!("FFmpeg could not write the cleaned copy: {}", String::from_utf8_lossy(&encoded.stderr).trim()))
            } else if samples == 0 {
                Err(anyhow!("{} has no audio", SOURCE_FILE))
            } else {
                Ok(samples)
            }
        }
        Ok(None) => {
            kill(&mut decoder);
            kill(&mut encoder);
            let _ = std::fs::remove_file(&temp);
            return Ok(Outcome::Stopped);
        }
        Err(e) => {
            kill(&mut decoder);
            kill(&mut encoder);
            Err(e)
        }
    };
    match finished {
        Ok(samples) => {
            std::fs::rename(&temp, folder.join(CLEANED_FILE)).context("moving the cleaned copy into place")?;
            Ok(Outcome::Written { audio_seconds: samples as f64 / SAMPLE_RATE as f64, elapsed: started.elapsed() })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// Decoder stdout -> WPE -> DeepFilterNet -> encoder stdin. Returns the number
/// of samples written, or None when stopped.
fn run_chain(decoder: &mut Child, encoder: &mut Child, should_stop: &dyn Fn() -> bool) -> Result<Option<usize>> {
    let mut input = decoder.stdout.take().ok_or_else(|| anyhow!("FFmpeg decoder has no output"))?;
    let mut output = encoder.stdin.take().ok_or_else(|| anyhow!("FFmpeg encoder has no input"))?;
    let mut wpe = wpe::StreamingWpe::new(wpe::WpeConfig::room_48k());
    let mut dfn = dfn::StreamingDfn::new()?;
    let mut level = level::LevelMatch::new();
    let mut bytes = vec![0u8; SAMPLE_RATE * 4];
    let mut carry: Vec<u8> = Vec::new();
    let mut written = 0usize;
    let mut write = |samples: &[f32], output: &mut std::process::ChildStdin| -> Result<()> {
        let mut buf = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            buf.extend_from_slice(&s.clamp(-1.0, 1.0).to_le_bytes());
        }
        written += samples.len();
        output.write_all(&buf).context("writing to the FFmpeg encoder")
    };
    loop {
        if should_stop() {
            return Ok(None);
        }
        let n = input.read(&mut bytes).context("reading from the FFmpeg decoder")?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&bytes[..n]);
        let whole = carry.len() / 4 * 4;
        let samples: Vec<f32> = carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        carry.drain(..whole);
        level.push_input(&samples);
        let dry = wpe.push(&samples);
        let enhanced = dfn.push(&dry)?;
        write(&level.process(&enhanced), &mut output)?;
    }
    let dry = wpe.finish();
    let mut enhanced = dfn.push(&dry)?;
    enhanced.extend(dfn.finish()?);
    write(&level.process(&enhanced), &mut output)?;
    drop(output);
    Ok(Some(written))
}

fn spawn(command: &mut Command) -> Result<Child> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map_err(|e| anyhow!("{e}"))
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

// ---- Worker -------------------------------------------------------------------

struct Queue {
    folders: Mutex<VecDeque<PathBuf>>,
    ready: Condvar,
}

fn queue() -> &'static Queue {
    static QUEUE: OnceLock<Queue> = OnceLock::new();
    QUEUE.get_or_init(|| Queue { folders: Mutex::new(VecDeque::new()), ready: Condvar::new() })
}

/// Queue a recording folder for a cleaned copy. Cheap; the worker checks
/// whether the folder still needs one when it gets to it.
pub fn enqueue(folder: PathBuf) {
    let q = queue();
    let mut folders = q.folders.lock().unwrap_or_else(|e| e.into_inner());
    if !folders.contains(&folder) {
        folders.push_back(folder);
        q.ready.notify_one();
    }
}

fn next_folder() -> PathBuf {
    let q = queue();
    let mut folders = q.folders.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(folder) = folders.pop_front() {
            return folder;
        }
        folders = q.ready.wait(folders).unwrap_or_else(|e| e.into_inner());
    }
}

/// Start the worker once, from app setup.
pub fn start<R: Runtime>(app: AppHandle<R>) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("room-cleanup".into()).spawn(move || {
        set_background_priority();
        let pool = match rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .thread_name(|i| format!("room-cleanup-{i}"))
            .start_handler(|_| set_background_priority())
            .build()
        {
            Ok(pool) => pool,
            Err(e) => {
                warn!("Room playback cleanup disabled: {e}");
                return;
            }
        };
        queue_backlog(&app);
        run_worker(&app, &pool);
    });
    if let Err(e) = spawned {
        warn!("Room playback cleanup disabled: {e}");
    }
}

fn run_worker<R: Runtime>(app: &AppHandle<R>, pool: &rayon::ThreadPool) {
    let recording = super::recording_commands::is_recording_active;
    let mut failed: HashSet<PathBuf> = HashSet::new();
    loop {
        let folder = next_folder();
        if failed.contains(&folder) {
            continue;
        }
        // Never compete with capture; let the save and post-call work go first.
        let mut waited = false;
        while recording() {
            waited = true;
            std::thread::sleep(Duration::from_secs(5));
        }
        if waited {
            std::thread::sleep(AFTER_RECORDING_GRACE);
        }
        if !needs_cleanup(&folder) {
            continue;
        }
        info!("Room playback cleanup started for {}", folder.display());
        match pool.install(|| clean_recording(&folder, &recording)) {
            Ok(Outcome::Written { audio_seconds, elapsed }) => {
                info!(
                    "Room playback cleanup wrote {} ({:.0} s of audio in {:.1} s, RTF {:.3})",
                    folder.join(CLEANED_FILE).display(),
                    audio_seconds,
                    elapsed.as_secs_f64(),
                    elapsed.as_secs_f64() / audio_seconds.max(1e-9)
                );
                let _ = app.emit(CLEANED_EVENT, serde_json::json!({ "folder": folder.to_string_lossy() }));
            }
            Ok(Outcome::Stopped) => {
                info!("Room playback cleanup paused for a recording; will resume afterwards");
                let q = queue();
                q.folders.lock().unwrap_or_else(|e| e.into_inner()).push_front(folder);
            }
            Err(e) => {
                warn!("Room playback cleanup failed for {}: {e:#}", folder.display());
                failed.insert(folder);
            }
        }
    }
}

/// Queue every completed room recording without a copy, newest first. The
/// database owns current folder locations (library sync may move folders).
fn queue_backlog<R: Runtime>(app: &AppHandle<R>) {
    let deadline = Instant::now() + Duration::from_secs(120);
    let state = loop {
        if let Some(state) = app.try_state::<crate::state::AppState>() {
            break state;
        }
        if Instant::now() > deadline {
            warn!("Room playback cleanup: database not ready; only new recordings will be cleaned");
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    let pool = state.db_manager.pool().clone();
    let folders: Vec<String> = match tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT folder_path FROM meetings WHERE folder_path IS NOT NULL ORDER BY created_at DESC").fetch_all(&pool),
    ) {
        Ok(rows) => rows,
        Err(e) => {
            warn!("Room playback cleanup: could not list meetings: {e}");
            return;
        }
    };
    let pending: Vec<PathBuf> = folders.into_iter().map(PathBuf::from).filter(|f| needs_cleanup(f)).collect();
    if !pending.is_empty() {
        info!("Room playback cleanup: {} earlier room recording(s) queued", pending.len());
    }
    for folder in pending {
        enqueue(folder);
    }
}

/// Background QoS on macOS (efficiency cores, yields to everything
/// interactive); lowest thread priority on Windows.
fn set_background_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: sets the calling thread's own QoS class; no pointers involved.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
    }
    #[cfg(windows)]
    // SAFETY: GetCurrentThread returns a pseudo-handle for the calling thread.
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST};
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_metadata(folder: &Path, status: &str, room: bool) {
        std::fs::write(
            folder.join("metadata.json"),
            serde_json::json!({ "version": "1.0", "status": status, "room_microphone": room }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn only_completed_room_recordings_without_a_copy_need_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path();
        assert!(!needs_cleanup(folder), "no metadata: imported or legacy");
        write_metadata(folder, "completed", true);
        assert!(!needs_cleanup(folder), "no microphone track");
        std::fs::write(folder.join(SOURCE_FILE), b"x").unwrap();
        assert!(needs_cleanup(folder));
        write_metadata(folder, "recording", true);
        assert!(!needs_cleanup(folder), "still recording");
        write_metadata(folder, "completed", false);
        assert!(!needs_cleanup(folder), "personal microphone");
        write_metadata(folder, "completed", true);
        std::fs::write(folder.join(CLEANED_FILE), b"y").unwrap();
        assert!(!needs_cleanup(folder), "already cleaned");
        assert_eq!(cleaned_playback_path(folder), Some(folder.join(CLEANED_FILE)));
        std::fs::write(folder.join(CLEANED_FILE), b"").unwrap();
        assert_eq!(cleaned_playback_path(folder), None, "empty files are not offered");
    }

    #[test]
    fn enqueue_deduplicates() {
        let a = PathBuf::from("/tmp/meetily-room-cleanup-test-a");
        enqueue(a.clone());
        enqueue(a.clone());
        let count = queue().folders.lock().unwrap().iter().filter(|f| **f == a).count();
        queue().folders.lock().unwrap().retain(|f| *f != a);
        assert_eq!(count, 1);
    }

    /// End to end on a real recording (ignored: needs FFmpeg and a recording).
    /// `MEETILY_ROOM_CLEANUP_FOLDER` must hold `mic.mp4`; the copy is written
    /// there as `room-clean.m4a`.
    #[test]
    #[ignore]
    fn cleans_a_real_recording() {
        let folder = PathBuf::from(std::env::var("MEETILY_ROOM_CLEANUP_FOLDER").expect("MEETILY_ROOM_CLEANUP_FOLDER"));
        match clean_recording(&folder, &|| false).unwrap() {
            Outcome::Written { audio_seconds, elapsed } => {
                println!("{audio_seconds:.1} s in {:.1} s (RTF {:.3})", elapsed.as_secs_f64(), elapsed.as_secs_f64() / audio_seconds);
            }
            Outcome::Stopped => panic!("stopped"),
        }
        assert!(cleaned_playback_path(&folder).is_some());
    }

    #[test]
    fn stopping_leaves_no_files() {
        let Some(ffmpeg) = super::super::ffmpeg::find_ffmpeg_path() else { return };
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new(ffmpeg)
            .args(["-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=5", "-ac", "1", "-ar", "48000", "-c:a", "aac"])
            .arg(dir.path().join(SOURCE_FILE))
            .status()
            .unwrap();
        assert!(status.success());
        let calls = std::cell::Cell::new(0);
        let stop = || {
            calls.set(calls.get() + 1);
            calls.get() > 2
        };
        assert!(matches!(clean_recording(dir.path(), &stop).unwrap(), Outcome::Stopped));
        assert!(!dir.path().join(CLEANED_FILE).exists());
        assert!(!dir.path().join(TEMP_FILE).exists());
        // And unstopped, it writes a copy of the same duration.
        assert!(matches!(clean_recording(dir.path(), &|| false).unwrap(), Outcome::Written { .. }));
        assert!(cleaned_playback_path(dir.path()).is_some());
    }
}
