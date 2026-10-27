//! Opt-in local calendar reading. A single bounded worker owns EventKit calls;
//! reminders are user prompts, never authority to start or stop capture.
pub mod attendees;
pub mod link;
pub mod saved;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarSettings {
    pub enabled: bool,
    pub suggest_titles: bool,
    pub prompt_recording: bool,
    pub reminder_minutes: u32,
    pub selected_calendar_ids: Vec<String>,
}
impl Default for CalendarSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            suggest_titles: true,
            prompt_recording: true,
            reminder_minutes: 5,
            selected_calendar_ids: vec![],
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CalendarSource {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CalendarMeeting {
    pub id: String,
    pub title: String,
    pub calendar_id: String,
    pub calendar_name: String,
    /// Unix milliseconds, including the occurrence's offset/DST conversion.
    pub start_ms: i64,
    pub end_ms: i64,
    pub is_meeting: bool,
    /// Everyone invited except the user, rooms and people who declined.
    #[serde(default)]
    pub attendees: Vec<attendees::CalendarAttendee>,
}
impl CalendarMeeting {
    fn valid(&self) -> bool {
        !self.id.is_empty()
            && !self.title.trim().is_empty()
            && self.title.chars().count() <= 200
            && self.end_ms > self.start_ms
    }
    fn key(&self) -> String {
        format!("{}:{}", self.id, self.start_ms)
    }
    fn prompt_due(&self, now: i64, minutes: u32) -> bool {
        self.valid()
            && self.is_meeting
            && now >= self.start_ms - i64::from(minutes.min(15)) * 60_000
            && now < self.end_ms
            && now <= self.start_ms + 10 * 60_000
    }
}
#[derive(Serialize)]
pub struct CalendarSnapshot {
    supported: bool,
    settings: CalendarSettings,
    authorization: String,
    calendars: Vec<CalendarSource>,
    events: Vec<CalendarMeeting>,
    busy: bool,
    error: Option<String>,
    last_updated_ms: Option<i64>,
}
#[derive(Deserialize)]
struct ReadResult {
    #[serde(default)]
    authorization: String,
    #[serde(default)]
    calendars: Vec<CalendarSource>,
    #[serde(default)]
    events: Vec<CalendarMeeting>,
    error: Option<String>,
}
#[derive(Default)]
struct State {
    settings: CalendarSettings,
    authorization: String,
    calendars: Vec<CalendarSource>,
    events: Vec<CalendarMeeting>,
    error: Option<String>,
    last_updated_ms: Option<i64>,
    revision: u64,
    prompted: HashMap<String, i64>,
    pending: Vec<CalendarMeeting>,
}
impl State {
    fn queue_due_prompts(&mut self, now: i64, recording: bool) -> bool {
        self.prompted.retain(|_, end| *end > now - 3_600_000);
        self.pending
            .retain(|event| event.end_ms > now && event.start_ms + 10 * 60_000 >= now);
        let mut added = false;
        if self.settings.enabled && self.settings.prompt_recording {
            for event in self
                .events
                .iter()
                .filter(|event| event.prompt_due(now, self.settings.reminder_minutes))
            {
                if self.prompted.insert(event.key(), event.end_ms).is_none()
                    && !recording
                    && self.pending.len() < 20
                {
                    self.pending.push(event.clone());
                    added = true;
                }
            }
        }
        // Calendar imports and reschedules can change identifiers rapidly.
        // Bound suppression history even if a provider keeps returning new IDs.
        while self.prompted.len() > 2048 {
            if let Some(key) = self
                .prompted
                .iter()
                .min_by_key(|(_, end)| **end)
                .map(|(key, _)| key.clone())
            {
                self.prompted.remove(&key);
            }
        }
        added
    }
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static QUERY_RUNNING: AtomicBool = AtomicBool::new(false);
static REFRESH_REQUESTED: tokio::sync::Notify = tokio::sync::Notify::const_new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| {
        let settings = std::fs::read(config_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CalendarSettings>(&bytes).ok())
            .filter(|s| valid_settings(s))
            .unwrap_or_default();
        Mutex::new(State {
            settings,
            authorization: "not_determined".into(),
            ..State::default()
        })
    })
}
fn config_path() -> std::path::PathBuf {
    crate::paths::install_data_root().join("calendar_settings.json")
}
fn valid_settings(settings: &CalendarSettings) -> bool {
    settings.reminder_minutes <= 15
        && settings.selected_calendar_ids.len() <= 50
        && settings
            .selected_calendar_ids
            .iter()
            .all(|id| !id.is_empty() && id.len() <= 512)
}
fn persist(settings: &CalendarSettings) -> Result<(), String> {
    let path = config_path();
    let temporary = path.with_extension("json.tmp");
    let bytes =
        serde_json::to_vec_pretty(settings).map_err(|_| "Could not encode calendar settings")?;
    std::fs::write(&temporary, bytes)
        .and_then(|_| std::fs::rename(&temporary, &path))
        .map_err(|_| "Could not save calendar settings".into())
}
fn snapshot() -> CalendarSnapshot {
    let state = state().lock().unwrap();
    CalendarSnapshot {
        supported: cfg!(target_os = "macos"),
        settings: state.settings.clone(),
        authorization: state.authorization.clone(),
        calendars: state.calendars.clone(),
        events: state.events.clone(),
        busy: QUERY_RUNNING.load(Ordering::SeqCst),
        error: state.error.clone(),
        last_updated_ms: state.last_updated_ms,
    }
}

#[cfg(target_os = "macos")]
fn read_platform(
    request_access: bool,
    ids: &[String],
    start_ms: i64,
    end_ms: i64,
) -> Result<ReadResult, String> {
    use std::ffi::{CStr, CString};
    extern "C" {
        fn meetily_calendar_read(
            request: bool,
            selected: *const std::os::raw::c_char,
            start: f64,
            end: f64,
        ) -> *mut std::os::raw::c_char;
        fn meetily_calendar_free(value: *mut std::os::raw::c_char);
    }
    let ids = CString::new(serde_json::to_string(ids).map_err(|_| "Invalid calendar selection")?)
        .map_err(|_| "Invalid calendar selection")?;
    // Native NSDate input is Unix seconds; every public event timestamp is ms.
    unsafe {
        let value = meetily_calendar_read(
            request_access,
            ids.as_ptr(),
            start_ms as f64 / 1000.0,
            end_ms as f64 / 1000.0,
        );
        if value.is_null() {
            return Err("macOS returned no calendar data".into());
        }
        let result: Result<ReadResult, String> = serde_json::from_slice(CStr::from_ptr(value).to_bytes())
            .map_err(|_| "macOS returned invalid calendar data".into());
        meetily_calendar_free(value);
        result.map(|mut read| {
            for event in &mut read.events {
                event.attendees = attendees::normalize(std::mem::take(&mut event.attendees));
            }
            read
        })
    }
}
#[cfg(not(target_os = "macos"))]
fn read_platform(_: bool, _: &[String], _: i64, _: i64) -> Result<ReadResult, String> {
    Err("Local calendar access is currently available on macOS".into())
}

struct QueryGuard;
impl Drop for QueryGuard {
    fn drop(&mut self) {
        QUERY_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// A timed-out EventKit call retains its one native query slot until it really
/// finishes. Reloading a WebView cannot launch an unbounded set of native reads.
async fn refresh<R: Runtime>(app: AppHandle<R>, request_access: bool) -> Result<(), String> {
    if QUERY_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }
    let (settings, revision) = {
        let s = state().lock().unwrap();
        (s.settings.clone(), s.revision)
    };
    if !settings.enabled && !request_access {
        QUERY_RUNNING.store(false, Ordering::SeqCst);
        return Ok(());
    }
    let worker_app = app.clone();
    let job = tauri::async_runtime::spawn_blocking(move || {
        let guard = QueryGuard;
        let now = chrono::Utc::now().timestamp_millis();
        let result = read_platform(
            request_access,
            &settings.selected_calendar_ids,
            now - 12 * 3_600_000,
            now + 24 * 3_600_000,
        );
        let mut notify = false;
        let outcome = (|| {
            let mut state = state().lock().unwrap();
            // Disconnect/changed selection wins even if an old native call returns late.
            if state.revision != revision {
                return Ok(());
            }
            let result = result?;
            if let Some(error) = result.error {
                return Err(error);
            }
            state.authorization = result.authorization;
            if state.authorization != "authorized" {
                state.events.clear();
                state.calendars.clear();
                state.pending.clear();
                return Err("Calendar access is unavailable. Allow Meetily in System Settings > Privacy & Security > Calendars, then connect again.".into());
            }
            if request_access {
                let mut next = state.settings.clone();
                next.enabled = true;
                // Activation belongs to native completion, even if the invoking
                // WebView navigated away while the permission prompt was open.
                persist(&next)?;
                state.settings = next;
            }
            state.calendars = result.calendars;
            state.events = result
                .events
                .into_iter()
                .filter(CalendarMeeting::valid)
                .take(500)
                .collect();
            state.last_updated_ms = Some(now);
            state.error = None;
            notify = state
                .queue_due_prompts(now, crate::audio::recording_commands::is_recording_active());
            Ok(())
        })();
        if let Err(error) = &outcome {
            let mut state = state().lock().unwrap();
            if state.revision == revision {
                state.error = Some(error.clone());
                state.events.clear();
                state.pending.clear();
            }
        }
        let superseded = state().lock().unwrap().revision != revision;
        drop(guard);
        if outcome.is_ok() && notify {
            // Native scheduling survives hidden/reloaded WebViews. Keep private
            // event titles off lock-screen notifications; choices stay in-app.
            let notification_app = worker_app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(managed) = notification_app
                    .try_state::<crate::notifications::commands::NotificationManagerState<R>>(
                ) {
                    let manager = managed.read().await;
                    if let Some(manager) = manager.as_ref() {
                        let mut notification =
                            crate::notifications::types::Notification::meeting_reminder(0, None);
                        notification.title = "Calendar meeting starting".into();
                        notification.body = "Open Meetily to choose whether to record.".into();
                        notification.actions.clear();
                        // Keep the existing notification consent, reminder and
                        // Do Not Disturb preferences authoritative.
                        let _ = manager.show_notification(notification).await;
                    }
                }
            });
        }
        if superseded {
            REFRESH_REQUESTED.notify_one();
        }
        let _ = worker_app.emit("calendar-changed", ());
        outcome
    });
    let _ = app.emit("calendar-changed", ());
    match tokio::time::timeout(
        std::time::Duration::from_secs(if request_access { 125 } else { 15 }),
        job,
    )
    .await
    {
        Ok(result) => result.map_err(|_| "Calendar worker could not finish".to_string())?,
        Err(_) => {
            let error = "Calendar access is taking longer than expected. The pending read will finish in the background.".to_string();
            {
                let mut state = state().lock().unwrap();
                if state.revision == revision {
                    state.error = Some(error.clone());
                    state.events.clear();
                    state.pending.clear();
                }
            }
            let _ = app.emit("calendar-changed", ());
            Err(error)
        }
    }
}

pub fn initialize<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let enabled = {
                let state = state().lock().unwrap();
                state.settings.enabled
                    && (state.settings.suggest_titles || state.settings.prompt_recording)
            };
            if enabled {
                let _ = refresh(app.clone(), false).await;
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {},
                _ = REFRESH_REQUESTED.notified() => {},
            }
        }
    });
}
#[tauri::command]
pub fn calendar_snapshot() -> CalendarSnapshot {
    snapshot()
}
#[tauri::command]
pub async fn calendar_connect<R: Runtime>(app: AppHandle<R>) -> Result<CalendarSnapshot, String> {
    refresh(app, true).await?;
    Ok(snapshot())
}
#[tauri::command]
pub async fn calendar_refresh<R: Runtime>(app: AppHandle<R>) -> Result<CalendarSnapshot, String> {
    if state().lock().unwrap().settings.enabled {
        refresh(app, false).await?;
    }
    Ok(snapshot())
}
#[tauri::command]
pub fn calendar_update_settings<R: Runtime>(
    app: AppHandle<R>,
    mut settings: CalendarSettings,
) -> Result<CalendarSnapshot, String> {
    if !valid_settings(&settings) {
        return Err("Choose at most 50 calendars and a reminder between 0 and 15 minutes".into());
    }
    {
        let mut state = state().lock().unwrap();
        settings.enabled = state.settings.enabled;
        persist(&settings)?;
        state.settings = settings;
        state.revision += 1;
        state.events.clear();
        state.pending.clear();
    }
    let _ = app.emit("calendar-changed", ());
    REFRESH_REQUESTED.notify_one();
    Ok(snapshot())
}
#[tauri::command]
pub fn calendar_disconnect<R: Runtime>(app: AppHandle<R>) -> Result<CalendarSnapshot, String> {
    {
        let mut state = state().lock().unwrap();
        let mut settings = state.settings.clone();
        settings.enabled = false;
        persist(&settings)?;
        state.settings = settings;
        state.revision += 1;
        state.events.clear();
        state.calendars.clear();
        state.pending.clear();
        state.error = None;
    }
    let _ = app.emit("calendar-changed", ());
    Ok(snapshot())
}
#[tauri::command]
pub fn calendar_take_prompts() -> Vec<CalendarMeeting> {
    let mut state = state().lock().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let pending = std::mem::take(&mut state.pending);
    if !state.settings.enabled
        || !state.settings.prompt_recording
        || crate::audio::recording_commands::is_recording_active()
    {
        return vec![];
    }
    pending
        .into_iter()
        .filter(|event| event.prompt_due(now, state.settings.reminder_minutes))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> CalendarMeeting {
        CalendarMeeting {
            id: "event".into(),
            title: "Design review".into(),
            calendar_id: "work".into(),
            calendar_name: "Work".into(),
            start_ms: 1_000_000,
            end_ms: 4_600_000,
            is_meeting: true,
            attendees: vec![],
        }
    }
    #[test]
    fn events_without_attendees_still_parse() {
        let read: ReadResult = serde_json::from_str(
            r#"{"authorization":"authorized","events":[{"id":"e","title":"T","calendar_id":"c","calendar_name":"C","start_ms":1,"end_ms":2,"is_meeting":true}]}"#,
        )
        .unwrap();
        assert!(read.events[0].attendees.is_empty());
    }
    #[test]
    fn calendar_prompts_are_bounded_and_require_a_meeting() {
        let event = event();
        assert!(!event.prompt_due(699_999, 5));
        assert!(event.prompt_due(700_000, 5));
        assert!(event.prompt_due(1_600_000, 5));
        assert!(!event.prompt_due(1_600_001, 5));
        assert!(!CalendarMeeting {
            is_meeting: false,
            ..event.clone()
        }
        .prompt_due(1_000_000, 5));
        assert!(!CalendarMeeting {
            end_ms: 1_000_001,
            ..event
        }
        .prompt_due(1_000_001, 5));
    }
    #[test]
    fn calendar_occurrence_keys_include_start_and_keep_overlapping_events_distinct() {
        let event = event();
        assert_ne!(
            event.key(),
            CalendarMeeting {
                start_ms: event.start_ms + 1,
                ..event.clone()
            }
            .key()
        );
        assert_ne!(
            event.key(),
            CalendarMeeting {
                id: "other".into(),
                ..event
            }
            .key()
        );
    }
    #[test]
    fn calendar_reminders_are_once_per_occurrence_and_never_queue_during_capture() {
        let mut state = State::default();
        state.settings.enabled = true;
        state.events = vec![
            event(),
            CalendarMeeting {
                id: "overlap".into(),
                ..event()
            },
        ];
        assert!(state.queue_due_prompts(1_000_000, false));
        assert_eq!(state.pending.len(), 2);
        state.pending.clear();
        assert!(!state.queue_due_prompts(1_000_001, false));
        state.events.push(CalendarMeeting {
            id: "during-recording".into(),
            ..event()
        });
        assert!(!state.queue_due_prompts(1_000_002, true));
        assert!(!state.queue_due_prompts(1_000_003, false));
        state.events.push(CalendarMeeting {
            start_ms: 1_000_004,
            ..event()
        });
        assert!(state.queue_due_prompts(1_000_004, false));
        assert_eq!(state.pending.len(), 1);
    }
    #[test]
    fn calendar_pending_and_history_are_bounded() {
        let mut state = State::default();
        state.settings.enabled = true;
        state.events = (0..2500)
            .map(|i| CalendarMeeting {
                id: format!("event-{i}"),
                ..event()
            })
            .collect();
        state.queue_due_prompts(1_000_000, false);
        assert_eq!(state.pending.len(), 20);
        assert_eq!(state.prompted.len(), 2048);
        state.queue_due_prompts(10_000_000, false);
        assert!(state.pending.is_empty());
        assert!(state.prompted.is_empty());
    }
    #[test]
    fn calendar_settings_are_opt_in_and_validate_bounded_inputs() {
        assert!(!CalendarSettings::default().enabled);
        assert!(valid_settings(&CalendarSettings::default()));
        assert!(!valid_settings(&CalendarSettings {
            reminder_minutes: 16,
            ..CalendarSettings::default()
        }));
        assert!(!valid_settings(&CalendarSettings {
            selected_calendar_ids: vec!["".into()],
            ..CalendarSettings::default()
        }));
    }
    #[test]
    fn calendar_invalid_titles_and_empty_ranges_are_not_used() {
        assert!(!CalendarMeeting {
            title: " ".into(),
            ..event()
        }
        .valid());
        assert!(!CalendarMeeting {
            end_ms: 0,
            ..event()
        }
        .valid());
    }
}
