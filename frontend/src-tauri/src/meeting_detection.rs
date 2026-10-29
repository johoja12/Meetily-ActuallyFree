//! Meeting Detection.
//!
//! A lightweight background monitor that watches the list of running processes
//! and, when it sees a known meeting/conferencing app start (Zoom, Teams,
//! Slack huddles, Webex, …), emits a `meeting-detected` event so the
//! UI can offer a one-click "Start recording". Fully local — no network, no
//! telemetry; just process-name matching via `sysinfo`.
//!
//! Everything is user-configurable and persisted install-locally in
//! `meeting_detection.json`: the poll interval, the list of app keywords to
//! watch, and an "ignored apps" list to suppress false positives.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Runtime};

/// Global run flag for the monitor loop. Setting this to `false` makes the
/// currently-running loop exit on its next tick.
static MONITOR_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Current settings, shared with the running loop so changes apply live.
static SETTINGS: Mutex<Option<MeetingDetectionSettings>> = Mutex::new(None);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeetingDetectionSettings {
    /// Master on/off switch.
    pub enabled: bool,
    /// How often to scan the process list, in seconds.
    pub interval_secs: u64,
    /// Process-name keywords that indicate a meeting app (matched
    /// case-insensitively as substrings of the executable name).
    pub meeting_apps: Vec<String>,
    /// Process-name keywords to always ignore (suppress false positives).
    pub ignored_apps: Vec<String>,
    /// Also raise a native OS notification (handled on the frontend) in
    /// addition to the in-app prompt.
    pub notify: bool,
}

impl Default for MeetingDetectionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: 15,
            meeting_apps: default_meeting_apps(),
            ignored_apps: Vec::new(),
            notify: true,
        }
    }
}

/// Default keyword list. Deliberately excludes bare "meet" to avoid matching
/// this app's own process ("meetily") and unrelated software.
fn default_meeting_apps() -> Vec<String> {
    [
        "zoom",
        "teams",
        "msteams",
        "slack",
        "webex",
        "gotomeeting",
        "bluejeans",
        "chime",
        "skype",
        "ringcentral",
        "whereby",
        "jitsi",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Map a matched keyword to a friendly display name for the notification.
fn friendly_name(keyword: &str) -> String {
    match keyword {
        "zoom" => "Zoom",
        "teams" | "msteams" => "Microsoft Teams",
        "slack" => "Slack",
        "webex" => "Webex",
        "gotomeeting" => "GoToMeeting",
        "bluejeans" => "BlueJeans",
        "chime" => "Amazon Chime",
        "skype" => "Skype",
        "ringcentral" => "RingCentral",
        "whereby" => "Whereby",
        "jitsi" => "Jitsi",
        other => other,
    }
    .to_string()
}

fn config_path() -> std::path::PathBuf {
    crate::paths::install_data_root().join("meeting_detection.json")
}

fn load_settings_from_disk() -> MeetingDetectionSettings {
    let path = config_path();
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(mut settings) = serde_json::from_slice::<MeetingDetectionSettings>(&bytes) {
            sanitize_settings(&mut settings);
            return settings;
        }
    }
    MeetingDetectionSettings::default()
}

/// Drop keywords we no longer detect from a loaded config. Discord was removed
/// from the default list, but existing installs may still have it persisted in
/// `meeting_detection.json`; strip it here so it isn't detected anymore.
fn sanitize_settings(settings: &mut MeetingDetectionSettings) {
    settings
        .meeting_apps
        .retain(|k| k.trim().to_lowercase() != "discord");
}

fn save_settings_to_disk(settings: &MeetingDetectionSettings) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

/// Payload emitted when a meeting app is detected.
#[derive(Clone, Serialize)]
struct MeetingDetectedPayload {
    /// Friendly app name, e.g. "Zoom".
    app: String,
    /// Raw process name that matched, e.g. "zoom.exe".
    process: String,
    /// Whether the user asked for a native notification too.
    notify: bool,
    /// Windows CapabilityAccessManager reports active mic/camera use.
    active_media: bool,
}

#[derive(Clone, Serialize)]
struct MeetingEndedPayload {
    app: String,
    process: String,
}

/// True for our own process — never treat Meetily as a "meeting app".
fn is_self_process(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("meetily") || n.contains("meetily-actually")
}

/// Scan the process list once and, if a (non-ignored) meeting app is present,
/// return `(friendly_name, process_name)`.
///
/// On Windows, when CapabilityAccessManager reports mic/camera holders, we only
/// alert if a *known* meeting app is among them. We never invent a name from a
/// random exe (that produced the nonsense "Meetily meeting detected" toast when
/// this app itself held the mic).
fn scan_for_meeting_app(settings: &MeetingDetectionSettings) -> Option<(String, String, bool)> {
    use sysinfo::System;

    let mut sys = System::new_all();
    sys.refresh_all();

    let ignored: Vec<String> = settings
        .ignored_apps
        .iter()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.trim().is_empty())
        .collect();

    let is_skipped = |name: &str| -> bool {
        if name.is_empty() || is_self_process(name) {
            return true;
        }
        ignored.iter().any(|ig| name.contains(ig))
    };

    // macOS: an app that is merely open is not a meeting (Webex, Teams and
    // Slack are chat apps too). Only an app running microphone and speakers
    // counts, and that is an active-media signal for meeting automation.
    #[cfg(target_os = "macos")]
    if let Some(clients) = macos_audio_clients(&sys) {
        return call_app_match(&clients, &settings.meeting_apps, &ignored).map(|(friendly, app)| (friendly, app, true));
    }

    #[cfg(windows)]
    let media_in_use: std::collections::HashSet<String> = windows_media_in_use_exes()
        .into_iter()
        .filter(|e| !is_self_process(e))
        .collect();
    #[cfg(not(windows))]
    let media_in_use: std::collections::HashSet<String> = std::collections::HashSet::new();

    #[cfg(windows)]
    let browser_meeting_pids = windows_browser_meeting_pids();

    let matches_media = |name: &str| -> bool {
        if media_in_use.is_empty() {
            return true; // no CAM signal → don't require it
        }
        let stem = name.trim_end_matches(".exe");
        media_in_use
            .iter()
            .any(|m| m == name || m.contains(stem) || stem.contains(m.trim_end_matches(".exe")))
    };

    // 1) Known meeting app that is actively using mic/camera (best signal).
    if !media_in_use.is_empty() {
        for process in sys.processes().values() {
            let name = process.name().to_string_lossy().to_lowercase();
            if is_skipped(&name) || !matches_media(&name) {
                continue;
            }
            #[cfg(windows)]
            if is_browser(&name) && browser_meeting_pids.contains(&process.pid().as_u32()) {
                return Some(("Browser meeting".into(), name, true));
            }
            for keyword in &settings.meeting_apps {
                let kw = keyword.trim().to_lowercase();
                if kw.is_empty() {
                    continue;
                }
                if process_matches_keyword(&name, &kw) {
                    return Some((friendly_name(&kw), name, true));
                }
            }
        }
        // CAM has holders but none are known meeting apps (e.g. only Meetily or
        // a browser). Do not fall through to process-only — that re-fires on idle
        // Zoom/Teams sitting in the tray.
        return None;
    }

    // 2) No CAM signal: process-name match only (macOS/Linux / older Windows).
    for process in sys.processes().values() {
        let name = process.name().to_string_lossy().to_lowercase();
        if is_skipped(&name) {
            continue;
        }
        for keyword in &settings.meeting_apps {
            let kw = keyword.trim().to_lowercase();
            if kw.is_empty() {
                continue;
            }
            if process_matches_keyword(&name, &kw) {
                return Some((friendly_name(&kw), name, false));
            }
        }
    }
    None
}

/// Windows: executables currently holding microphone or webcam via
/// CapabilityAccessManager NonPackaged consent-store entries
/// (LastUsedTimeStart > LastUsedTimeStop ⇒ in use).
#[cfg(windows)]
fn windows_media_in_use_exes() -> std::collections::HashSet<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let mut out = std::collections::HashSet::new();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for cap in ["microphone", "webcam"] {
        for suffix in ["NonPackaged", ""] {
            let path = format!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\{cap}\\{suffix}"
            );
            let Ok(root) = hkcu.open_subkey(&path) else { continue };
            let Ok(keys) = root.enum_keys().collect::<Result<Vec<_>, _>>() else { continue };
            for key_name in keys {
            if key_name.eq_ignore_ascii_case("NonPackaged") { continue; }
            let Ok(sub) = root.open_subkey(&key_name) else {
                continue;
            };
            // Values are FILETIME-like u64; Start > Stop means currently open.
            let start: u64 = sub.get_value("LastUsedTimeStart").unwrap_or(0);
            let stop: u64 = sub.get_value("LastUsedTimeStop").unwrap_or(0);
            if start == 0 {
                continue;
            }
            // 0xFFFFFFFFFFFFFFFF stop means "still in use" on some builds;
            // otherwise start > stop.
            let in_use = stop == u64::MAX || start > stop;
            if !in_use {
                continue;
            }
            // Key names look like C:#Program Files#...#Teams.exe
            let exe = key_name
                .rsplit('#')
                .next()
                .unwrap_or(&key_name)
                .to_lowercase();
            if exe.ends_with(".exe") {
                out.insert(exe);
            } else if key_name.to_lowercase().contains("msteams") {
                // Packaged Teams stores a package family name rather than an exe.
                out.insert("ms-teams.exe".into());
            }
            }
        }
    }
    out
}

#[cfg(windows)]
fn is_browser(name: &str) -> bool {
    matches!(name, "chrome.exe" | "msedge.exe" | "firefox.exe")
}

#[cfg(windows)]
fn windows_browser_meeting_pids() -> std::collections::HashSet<u32> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible};
    unsafe extern "system" fn visit(hwnd: HWND, state: isize) -> i32 {
        let matches = &mut *(state as *mut std::collections::HashSet<u32>);
        if IsWindowVisible(hwnd) == 0 { return 1; }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 || len > 1024 { return 1; }
        let mut title = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        if copied <= 0 { return 1; }
        let title = String::from_utf16_lossy(&title[..copied as usize]).to_lowercase();
        if browser_title_is_meeting(&title) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != 0 { matches.insert(pid); }
        }
        1
    }
    let mut matches = std::collections::HashSet::new();
    unsafe { EnumWindows(Some(visit), &mut matches as *mut _ as isize); }
    matches
}

#[cfg(windows)]
fn browser_title_is_meeting(title: &str) -> bool {
    let title = title.to_lowercase();
    ["google meet", "meet.google.com", "zoom meeting", "zoom.us/j/", "meeting | microsoft teams", "teams.microsoft.com", "slack huddle"]
        .iter().any(|needle| title.contains(needle))
}

/// One CoreAudio client process, named by the app it belongs to.
#[derive(Clone, Debug)]
struct AudioClient {
    app: String,
    input: bool,
    output: bool,
}

/// Lowercased name of the outermost `.app` bundle containing `exe`, so helper
/// processes (`Webex.app/…/Spark Helper.app/…`) count for their app. Falls back
/// to the process name for executables outside a bundle.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn audio_client_app(exe: Option<&std::path::Path>, process_name: &str) -> String {
    exe.and_then(|path| {
        path.components()
            .filter_map(|part| part.as_os_str().to_str())
            .find_map(|part| part.strip_suffix(".app"))
            .map(str::to_lowercase)
    })
    .unwrap_or_else(|| process_name.to_lowercase())
}

/// The first known meeting app that is both capturing input and playing output,
/// as `(friendly name, app name)`. Apps that only stay open (Webex, Teams and
/// Slack chat), only play a sound, or only listen (Webex proximity pairing) are
/// not calls. Input and output may come from different helper processes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn call_app_match(clients: &[AudioClient], keywords: &[String], ignored: &[String]) -> Option<(String, String)> {
    let mut apps: Vec<&str> = clients.iter().map(|client| client.app.as_str()).collect();
    apps.dedup();
    apps.into_iter().find_map(|app| {
        let in_call = clients.iter().any(|c| c.app == app && c.input) && clients.iter().any(|c| c.app == app && c.output);
        let skipped = app.is_empty() || is_self_process(app) || ignored.iter().any(|ig| !ig.trim().is_empty() && app.contains(&ig.trim().to_lowercase()));
        if !in_call || skipped {
            return None;
        }
        keywords
            .iter()
            .map(|keyword| keyword.trim().to_lowercase())
            .find(|keyword| !keyword.is_empty() && process_matches_keyword(app, keyword))
            .map(|keyword| (friendly_name(&keyword), app.to_string()))
    })
}

/// macOS: every process CoreAudio has as a client, with whether it is running
/// input and output (any device, so a USB headset or Desk Pro counts). None when
/// the query is unavailable, which keeps process-only detection.
#[cfg(target_os = "macos")]
fn macos_audio_clients(sys: &sysinfo::System) -> Option<Vec<AudioClient>> {
    use cidre::core_audio as ca;
    let processes = ca::System::processes().ok()?;
    let mut clients: Vec<AudioClient> = processes
        .iter()
        .filter_map(|process| {
            let pid = process.pid().ok()?;
            let info = sys.process(sysinfo::Pid::from_u32(u32::try_from(pid).ok()?));
            let name = info.map(|p| p.name().to_string_lossy().to_string()).unwrap_or_default();
            Some(AudioClient {
                app: audio_client_app(info.and_then(|p| p.exe()), &name),
                input: process.is_running_input().unwrap_or(false),
                output: process.is_running_output().unwrap_or(false),
            })
        })
        .collect();
    clients.sort_by(|a, b| a.app.cmp(&b.app));
    Some(clients)
}

/// Only a call that is not yet reported, with active media, needs to be heard
/// first. An already reported call (or one replacing another) is not re-checked.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn needs_sound_check(alerted: Option<&(String, String, bool)>, found: &(String, String, bool)) -> bool {
    found.2 && alerted.is_none()
}

/// Does a process name match a meeting-app keyword?
///
/// We deliberately do NOT use a raw substring test. Naive `contains("teams")`
/// famously matches `steamservice.exe` (Steam) — `s[teams]ervice` — and reports
/// a phantom "Microsoft Teams meeting." Instead we split the process name into
/// alphanumeric tokens (so `.exe`, `-`, `_`, spaces, and digits are boundaries)
/// and require a *token* to start with the keyword. This keeps forgiving
/// matches that should work — `ms-teams.exe` -> ["ms","teams"], `webexmta.exe`
/// -> ["webexmta"] — while rejecting keywords buried mid-word like the Steam
/// service.
fn process_matches_keyword(process_name: &str, keyword: &str) -> bool {
    process_name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| !token.is_empty() && token.starts_with(keyword))
}

#[cfg(test)]
mod tests {
    use super::{is_self_process, process_matches_keyword};

    #[test]
    fn never_treats_meetily_as_a_meeting() {
        assert!(is_self_process("meetily.exe"));
        assert!(is_self_process("Meetily - Actually Free.exe"));
        assert!(!is_self_process("ms-teams.exe"));
    }

    #[test]
    fn matches_real_meeting_apps() {
        assert!(process_matches_keyword("teams.exe", "teams"));
        assert!(process_matches_keyword("ms-teams.exe", "teams"));
        assert!(process_matches_keyword("msteams.exe", "msteams"));
        assert!(process_matches_keyword("zoom.exe", "zoom"));
        assert!(process_matches_keyword("webexmta.exe", "webex"));
        assert!(process_matches_keyword("slack.exe", "slack"));
    }

    #[test]
    fn rejects_mid_word_false_positives() {
        // The reported bug: Steam's service is not a Teams meeting.
        assert!(!process_matches_keyword("steamservice.exe", "teams"));
        assert!(!process_matches_keyword("steam.exe", "teams"));
        assert!(!process_matches_keyword("steamwebhelper.exe", "teams"));
    }

    #[test]
    fn audio_clients_are_named_by_their_outermost_app() {
        use super::audio_client_app;
        let helper = std::path::Path::new("/Applications/Webex.app/Contents/Frameworks/Spark Helper.app/Contents/MacOS/Spark Helper");
        assert_eq!(audio_client_app(Some(helper), "Spark Helper"), "webex");
        assert_eq!(audio_client_app(Some(std::path::Path::new("/Applications/zoom.us.app/Contents/MacOS/zoom.us")), "zoom.us"), "zoom.us");
        assert_eq!(audio_client_app(Some(std::path::Path::new("/usr/libexec/avconferenced")), "avconferenced"), "avconferenced");
        assert_eq!(audio_client_app(None, "Slack"), "slack");
    }

    #[test]
    fn an_open_app_is_not_a_call_until_it_uses_the_microphone_and_speakers() {
        use super::{call_app_match, default_meeting_apps, AudioClient};
        let keywords = default_meeting_apps();
        let client = |app: &str, input: bool, output: bool| AudioClient { app: app.into(), input, output };
        // Webex open for chat (or only playing a message sound): no call.
        assert_eq!(call_app_match(&[client("webex", false, true)], &keywords, &[]), None);
        // Listening only, as Webex proximity pairing does: no call.
        assert_eq!(call_app_match(&[client("webex", true, false)], &keywords, &[]), None);
        // Microphone in one helper and speakers in another still count for the app.
        let call = [client("webex", true, false), client("webex", false, true)];
        assert_eq!(call_app_match(&call, &keywords, &[]), Some(("Webex".into(), "webex".into())));
        // An unknown app or Meetily itself using the microphone is not a meeting app.
        assert_eq!(call_app_match(&[client("garageband", true, true), client("meetily", true, true)], &keywords, &[]), None);
        // Ignored apps stay ignored.
        assert_eq!(call_app_match(&[client("microsoft teams", true, true)], &keywords, &["teams".into()]), None);
        assert_eq!(
            call_app_match(&[client("microsoft teams", true, true)], &keywords, &[]),
            Some(("Microsoft Teams".into(), "microsoft teams".into()))
        );
    }

    #[test]
    fn only_a_new_active_call_waits_to_be_heard() {
        use super::needs_sound_check;
        let call = ("Webex".to_string(), "webex".to_string(), true);
        assert!(needs_sound_check(None, &call));
        assert!(!needs_sound_check(Some(&call), &call));
        assert!(!needs_sound_check(None, &("Webex".into(), "webex".into(), false)));
    }

    #[cfg(windows)]
    #[test]
    fn browser_title_requires_meeting_context() {
        assert!(super::browser_title_is_meeting("Google Meet - Project Sync"));
        assert!(super::browser_title_is_meeting("Meeting | Microsoft Teams - Edge"));
        assert!(!super::browser_title_is_meeting("Meetily documentation - Chrome"));
        assert!(!super::browser_title_is_meeting("Microsoft Teams home - Edge"));
    }
}

/// Start (or restart) the background monitor with the given settings.
fn start_monitor<R: Runtime>(app: &AppHandle<R>) {
    // A generation token prevents an old sleeping loop from reviving after a restart.
    let generation = MONITOR_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    let settings = {
        let guard = SETTINGS.lock().unwrap();
        guard.clone().unwrap_or_default()
    };
    if !settings.enabled {
        log::info!("Meeting detection disabled; monitor not started");
        return;
    }

    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        log::info!("🔍 Meeting detection monitor started");
        // Whether we've already alerted for the currently-ongoing meeting app,
        // so we prompt once per meeting rather than every tick.
        let mut alerted: Option<(String, String, bool)> = None;
        let mut missing_since: Option<std::time::Instant> = None;
        // The app last logged as waiting for speech, so the log says it once.
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut, unused_variables))]
        let mut waiting_for: Option<String> = None;

        // Give the loop a moment before the first heavy scan.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        while MONITOR_GENERATION.load(Ordering::SeqCst) == generation {
            let current = {
                let guard = SETTINGS.lock().unwrap();
                guard.clone().unwrap_or_default()
            };
            if !current.enabled {
                break;
            }

            let scan_settings = current.clone();
            let scan = match tokio::task::spawn_blocking(move || scan_for_meeting_app(&scan_settings)).await {
                Ok(scan) => scan,
                Err(error) => { log::warn!("Meeting detection scan failed: {error}"); None }
            };
            if MONITOR_GENERATION.load(Ordering::SeqCst) != generation { break; }
            // A process-only fallback must not keep a formerly active meeting
            // alive after its microphone lease ends while the app stays open.
            let scan = match (alerted.as_ref(), scan) {
                (Some((_, process, true)), Some((_, candidate, false))) if process == &candidate => None,
                (_, other) => other,
            };
            // macOS: an app with microphone and speakers open may only be
            // showing its join preview (Webex's microphone check and chime).
            // Report a new call once the recording microphone hears speech;
            // until then, keep checking each scan.
            #[cfg(target_os = "macos")]
            let scan_was_empty = scan.is_none();
            #[cfg(target_os = "macos")]
            let scan = match scan {
                Some(found) if needs_sound_check(alerted.as_ref(), &found) && !crate::audio::recording_commands::is_recording_active() => {
                    use crate::calendar::listen;
                    let device = listen::preferred_mic(&app);
                    let sampled = device.clone();
                    let heard = tokio::task::spawn_blocking(move || {
                        listen::sample_frames(sampled.as_deref(), listen::SAMPLE).map(|frames| listen::heard_speech(&frames))
                    })
                    .await;
                    match heard {
                        Ok(Ok(false)) => {
                            if waiting_for.as_deref() != Some(found.1.as_str()) {
                                log::info!(
                                    "Meeting app {} has audio open but no speech yet on {}; waiting to report it",
                                    found.0,
                                    device.as_deref().unwrap_or("the default microphone")
                                );
                                waiting_for = Some(found.1.clone());
                            }
                            None
                        }
                        // Heard, or the microphone could not be read: report as before.
                        _ => Some(found),
                    }
                }
                other => other,
            };
            if MONITOR_GENERATION.load(Ordering::SeqCst) != generation { break; }
            match scan {
                Some((friendly, process, active_media)) => {
                    missing_since = None;
                    if alerted.as_ref() != Some(&(friendly.clone(), process.clone(), active_media)) {
                        alerted = Some((friendly.clone(), process.clone(), active_media));
                        #[cfg(target_os = "macos")]
                        {
                            waiting_for = None;
                        }
                        log::info!("🔔 Meeting app detected: {} ({})", friendly, process);
                        let _ = app.emit(
                            "meeting-detected",
                            MeetingDetectedPayload {
                                app: friendly,
                                process,
                                notify: current.notify,
                                active_media,
                            },
                        );
                    }
                }
                None => {
                    // Seen again after an absence, a call is logged again.
                    #[cfg(target_os = "macos")]
                    if alerted.is_none() && scan_was_empty {
                        waiting_for = None;
                    }
                    if let Some((app_name, process, _)) = &alerted {
                        let since = missing_since.get_or_insert_with(std::time::Instant::now);
                        if since.elapsed() >= std::time::Duration::from_secs(45) {
                            let _ = app.emit("meeting-ended", MeetingEndedPayload { app: app_name.clone(), process: process.clone() });
                            alerted = None;
                            missing_since = None;
                        }
                    }
                }
            }

            let interval = current.interval_secs.clamp(3, 3600);
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        }

        log::info!("🔍 Meeting detection monitor stopped");
    });
}

/// Called once at app startup to load persisted settings and start the monitor
/// if the user had it enabled.
pub fn initialize<R: Runtime>(app: &AppHandle<R>) {
    let settings = load_settings_from_disk();
    {
        let mut guard = SETTINGS.lock().unwrap();
        *guard = Some(settings.clone());
    }
    if settings.enabled {
        start_monitor(app);
    }
}

// ============================================================================
// Tauri commands
// ============================================================================

#[tauri::command]
pub async fn get_meeting_detection_settings() -> Result<MeetingDetectionSettings, String> {
    let guard = SETTINGS.lock().unwrap();
    Ok(guard.clone().unwrap_or_else(load_settings_from_disk))
}

#[tauri::command]
pub async fn set_meeting_detection_settings<R: Runtime>(
    app: AppHandle<R>,
    settings: MeetingDetectionSettings,
) -> Result<(), String> {
    // Sanity-clamp the interval.
    let mut settings = settings;
    settings.interval_secs = settings.interval_secs.clamp(3, 3600);

    save_settings_to_disk(&settings)?;
    {
        let mut guard = SETTINGS.lock().unwrap();
        *guard = Some(settings.clone());
    }

    // Apply immediately: (re)start or stop the monitor.
    if settings.enabled {
        start_monitor(&app);
    } else {
        MONITOR_GENERATION.fetch_add(1, Ordering::SeqCst);
    }
    Ok(())
}

/// Manually (re)start the monitor using current settings.
#[tauri::command]
pub async fn start_meeting_detection<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    start_monitor(&app);
    Ok(())
}

/// Manually stop the monitor.
#[tauri::command]
pub async fn stop_meeting_detection() -> Result<(), String> {
    MONITOR_GENERATION.fetch_add(1, Ordering::SeqCst);
    Ok(())
}
