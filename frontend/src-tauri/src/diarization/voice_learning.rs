//! Automatic voice learning and matching for Labs voice profiles.
//!
//! One native worker owns this work so it finishes even if the WebView that
//! renamed a speaker reloads. Callers only enqueue: a rename never waits on
//! audio decoding or WeSpeaker inference. Requests coalesce, so tagging one
//! person several times is one job.
//!
//! Learning: naming a speaker adds that meeting's share to the contact's voice;
//! untagging removes it. Matching: each meeting's unnamed `Speaker N` labels are
//! compared with the saved voices using the meeting's own transcript spans (no
//! re-diarization), and a confident match renames the label like a manual edit.
//! Names set this way are recorded in `voice_speaker_matches` and never train a
//! voice; undoing one records a rejection so it is not applied again. Your own
//! voice names a speaker "You" only in room-microphone meetings, the one case
//! where your voice is on the track being matched; those matches are kept in
//! `self_voice_matches`.
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use super::voice_profiles::{self, is_self_label, VoiceProfile, SELF_ID, SELF_LABEL};
use crate::database::repositories::person::{is_person_name, PeopleRepository};

pub const STATUS_EVENT: &str = "voice-learning-status";
pub const PROFILES_CHANGED_EVENT: &str = "voice-profiles-changed";
pub const MATCHES_APPLIED_EVENT: &str = "voice-matches-applied";

#[derive(Debug, Default, PartialEq)]
struct Pending {
    /// (meeting, label) of a speaker the user named.
    learn: BTreeSet<(String, String)>,
    /// (person, meeting) of a contact no longer named in a meeting.
    forget: BTreeSet<(String, String)>,
    /// Meetings to match against the saved voices.
    check: BTreeSet<String>,
    /// Match every meeting.
    all: bool,
}

impl Pending {
    const fn new() -> Self {
        Self { learn: BTreeSet::new(), forget: BTreeSet::new(), check: BTreeSet::new(), all: false }
    }

    fn is_empty(&self) -> bool {
        self.learn.is_empty() && self.forget.is_empty() && self.check.is_empty() && !self.all
    }
}

static PENDING: Mutex<Pending> = Mutex::new(Pending::new());
static WAKE: OnceLock<Notify> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);
static APP: OnceLock<AppHandle> = OnceLock::new();

fn wake() -> &'static Notify {
    WAKE.get_or_init(Notify::new)
}

/// Starts the worker. Called once from app setup.
pub fn start(app: AppHandle) {
    if APP.set(app).is_err() {
        return;
    }
    tauri::async_runtime::spawn(worker());
}

fn enqueue(change: impl FnOnce(&mut Pending)) {
    if let Ok(mut pending) = PENDING.lock() {
        change(&mut pending);
    }
    // A permit is stored when the worker is busy, so nothing is missed.
    wake().notify_one();
}

pub fn learn(meeting_id: &str, label: &str) {
    let (meeting, label) = (meeting_id.to_string(), label.trim().to_string());
    enqueue(|pending| { pending.learn.insert((meeting, label)); });
}

pub fn forget(person_id: &str, meeting_id: &str) {
    let key = (person_id.to_string(), meeting_id.to_string());
    enqueue(|pending| { pending.forget.insert(key); });
}

pub fn check(meeting_id: &str) {
    let meeting = meeting_id.to_string();
    enqueue(|pending| { pending.check.insert(meeting); });
}

pub fn check_all() {
    enqueue(|pending| pending.all = true);
}

fn take() -> Pending {
    PENDING.lock().map(|mut pending| std::mem::take(&mut *pending)).unwrap_or_default()
}

fn pool() -> Option<SqlitePool> {
    let app = APP.get()?;
    let state = app.try_state::<crate::state::AppState>()?;
    Some(state.db_manager.pool().clone())
}

fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(app) = APP.get() {
        if let Err(error) = app.emit(event, payload) {
            log::warn!("Could not announce {event}: {error}");
        }
    }
}

fn set_running(running: bool) {
    if RUNNING.swap(running, Ordering::SeqCst) != running {
        emit(STATUS_EVENT, StatusPayload { running });
    }
}

#[derive(Clone, Serialize)]
struct StatusPayload {
    running: bool,
}

async fn worker() {
    loop {
        wake().notified().await;
        loop {
            // The database is managed after setup; keep requests until it is.
            let Some(pool) = pool() else {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            };
            let batch = take();
            if batch.is_empty() {
                break;
            }
            set_running(true);
            process(&pool, batch).await;
        }
        set_running(false);
    }
}

async fn process(pool: &SqlitePool, batch: Pending) {
    let mut changed = false;
    for (person, meeting) in &batch.forget {
        changed |= voice_profiles::drop_meeting_share(person, meeting);
    }
    for (meeting, label) in &batch.learn {
        changed |= voice_profiles::learn_tagged_speaker(pool, meeting, label).await.is_some();
    }
    if changed {
        emit(PROFILES_CHANGED_EVENT, ());
    }

    // A changed voice can name speakers anywhere, so every meeting is checked.
    let meetings: Vec<String> = if changed || batch.all {
        sqlx::query_scalar("SELECT id FROM meetings WHERE folder_path IS NOT NULL ORDER BY created_at DESC")
            .fetch_all(pool)
            .await
            .unwrap_or_default()
    } else {
        batch.check.into_iter().collect()
    };
    if meetings.is_empty() || !super::pyannote_models_available() {
        return;
    }
    let profiles = match voice_profiles::load_all_for_matching() {
        Ok(profiles) if !profiles.is_empty() => profiles,
        Ok(_) => return,
        Err(error) => {
            log::warn!("Saved voices unavailable for matching: {error}");
            return;
        }
    };
    let mut applied = Vec::new();
    for meeting in meetings {
        match match_meeting(pool, &meeting, &profiles).await {
            Ok(matches) => applied.extend(matches),
            Err(error) => log::warn!("Voice matching skipped meeting {meeting}: {error}"),
        }
    }
    if !applied.is_empty() {
        log::info!("Saved voices named {} speaker(s)", applied.len());
        emit(MATCHES_APPLIED_EVENT, MatchesApplied { matches: applied });
    }
}

/// A name a saved voice gave to a meeting's unnamed speaker.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VoiceMatch {
    pub meeting_id: String,
    pub meeting_title: String,
    pub person_id: String,
    pub name: String,
    /// The meeting-local label it replaced, e.g. "Speaker 3".
    pub original_label: String,
}

#[derive(Clone, Serialize)]
struct MatchesApplied {
    matches: Vec<VoiceMatch>,
}

#[derive(Clone, Debug, PartialEq)]
struct Scored {
    label: String,
    person_id: String,
    score: f32,
}

fn is_generic(label: &str) -> bool {
    label.trim().to_ascii_lowercase().starts_with("speaker ") && !label.contains(" + ")
}

/// Generic single labels anywhere in the meeting, including inside combined
/// labels such as "Speaker 1 + Speaker 3".
fn generic_labels(rows: &[voice_profiles::TranscriptTurn]) -> BTreeSet<String> {
    rows.iter()
        .flat_map(|(label, _, _)| label.split(" + ").map(str::trim).map(str::to_string).collect::<Vec<_>>())
        .filter(|label| is_generic(label))
        .collect()
}

/// Which matches to apply: labels still present and unnamed, contacts not
/// already in the meeting, pairs the user has not undone, and one label per
/// contact (the strongest).
fn choose(
    mut scored: Vec<Scored>,
    present: &BTreeSet<String>,
    named: &HashSet<String>,
    rejected: &HashSet<(String, String)>,
) -> Vec<Scored> {
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut people = HashSet::new();
    let mut labels = HashSet::new();
    scored
        .into_iter()
        .filter(|candidate| present.contains(&candidate.label))
        .filter(|candidate| !named.contains(&candidate.person_id))
        .filter(|candidate| !rejected.contains(&(candidate.label.clone(), candidate.person_id.clone())))
        .filter(|candidate| people.insert(candidate.person_id.clone()) && labels.insert(candidate.label.clone()))
        .collect()
}

async fn match_meeting(pool: &SqlitePool, meeting_id: &str, profiles: &[VoiceProfile]) -> Result<Vec<VoiceMatch>, String> {
    let failed = |error: sqlx::Error| error.to_string();
    let meeting: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT COALESCE(title, ''), folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id).fetch_optional(pool).await.map_err(failed)?;
    let Some((title, Some(folder))) = meeting else { return Ok(Vec::new()) };
    let track = voice_profiles::call_track(Path::new(&folder));
    if !track.is_file() {
        return Ok(Vec::new());
    }
    // Outside room mode the matched track is the call, where you are not.
    let room = crate::audio::recording_saver::is_room_microphone_recording(Path::new(&folder));
    let profiles: Vec<VoiceProfile> = profiles.iter().filter(|profile| room || profile.person_id != SELF_ID).cloned().collect();
    if profiles.is_empty() {
        return Ok(Vec::new());
    }
    let rows = voice_profiles::transcript_turns(pool, meeting_id).await.map_err(failed)?;
    let turns: HashMap<String, Vec<(f64, f64)>> = generic_labels(&rows)
        .into_iter()
        .map(|label| {
            let spans = voice_profiles::clean_turns(&rows, &label);
            (label, spans)
        })
        .filter(|(_, spans)| spans.len() >= 2)
        .collect();
    if turns.is_empty() {
        return Ok(Vec::new());
    }
    let means = tokio::task::spawn_blocking(move || voice_profiles::embed_label_means(&track, &turns))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let scored: Vec<Scored> = means
        .iter()
        .filter_map(|(label, mean)| {
            voice_profiles::best_match_scored(mean, &profiles).map(|(profile, score)| Scored {
                label: label.clone(),
                person_id: profile.person_id.clone(),
                score,
            })
        })
        .collect();
    if scored.is_empty() {
        return Ok(Vec::new());
    }

    // Labels may have changed while the audio was analyzed; decide and write
    // under the same guard as manual speaker edits and diarization.
    let _guard = super::operation_guard().await;
    apply(pool, meeting_id, &title, scored).await
}

/// Decides and writes the names for one meeting. The caller holds the
/// operation guard.
async fn apply(pool: &SqlitePool, meeting_id: &str, title: &str, scored: Vec<Scored>) -> Result<Vec<VoiceMatch>, String> {
    let failed = |error: sqlx::Error| error.to_string();
    let rows = voice_profiles::transcript_turns(pool, meeting_id).await.map_err(failed)?;
    let mut named: HashSet<String> = sqlx::query_scalar("SELECT person_id FROM person_speakers WHERE meeting_id = ?")
        .bind(meeting_id).fetch_all(pool).await.map_err(failed)?.into_iter().collect();
    if rows.iter().any(|(label, _, _)| label.split(" + ").any(is_self_label)) {
        named.insert(SELF_ID.to_string());
    }
    let mut rejected: HashSet<(String, String)> = sqlx::query_as(
        "SELECT original_label, person_id FROM voice_speaker_matches WHERE meeting_id = ? AND status = 'rejected'",
    ).bind(meeting_id).fetch_all(pool).await.map_err(failed)?.into_iter().collect();
    let self_rejected: Vec<String> = sqlx::query_scalar(
        "SELECT original_label FROM self_voice_matches WHERE meeting_id = ? AND status = 'rejected'",
    ).bind(meeting_id).fetch_all(pool).await.map_err(failed)?;
    rejected.extend(self_rejected.into_iter().map(|label| (label, SELF_ID.to_string())));
    let mut applied = Vec::new();
    for chosen in choose(scored, &generic_labels(&rows), &named, &rejected) {
        let own = chosen.person_id == SELF_ID;
        // The contact's current name, so the label links to that contact.
        let name: String = if own {
            SELF_LABEL.to_string()
        } else {
            let Some(name) = sqlx::query_scalar("SELECT display_name FROM people WHERE id = ?")
                .bind(&chosen.person_id).fetch_optional(pool).await.map_err(failed)?
            else { continue };
            name
        };
        PeopleRepository::rename_meeting_speaker(pool, meeting_id, &chosen.label, &name).await.map_err(failed)?;
        let record = if own {
            sqlx::query(
                "INSERT INTO self_voice_matches (meeting_id, original_label, status, created_at) \
                 VALUES (?, ?, 'applied', datetime('now')) \
                 ON CONFLICT(meeting_id, original_label) DO UPDATE SET status = 'applied'",
            ).bind(meeting_id).bind(&chosen.label)
        } else {
            sqlx::query(
                "INSERT INTO voice_speaker_matches (meeting_id, original_label, person_id, status, created_at) \
                 VALUES (?, ?, ?, 'applied', datetime('now')) \
                 ON CONFLICT(meeting_id, original_label, person_id) DO UPDATE SET status = 'applied'",
            ).bind(meeting_id).bind(&chosen.label).bind(&chosen.person_id)
        };
        record.execute(pool).await.map_err(failed)?;
        log::info!("Saved voice named {} in meeting {meeting_id} (cosine {:.2})", chosen.label, chosen.score);
        applied.push(VoiceMatch {
            meeting_id: meeting_id.to_string(),
            meeting_title: title.to_string(),
            person_id: chosen.person_id,
            name,
            original_label: chosen.label,
        });
    }
    Ok(applied)
}

/// Who a label belonged to before a manual speaker edit, so the edit can
/// update their voice afterwards.
pub(crate) struct SpeakerEdit {
    meeting_id: String,
    person_id: Option<String>,
    automatic: bool,
}

pub(crate) async fn before_speaker_edit(pool: &SqlitePool, meeting_id: &str, label: &str) -> SpeakerEdit {
    let person_id: Option<String> = if is_self_label(label) {
        Some(SELF_ID.to_string())
    } else {
        sqlx::query_scalar("SELECT person_id FROM person_speakers WHERE meeting_id = ? AND speaker_label = ?")
            .bind(meeting_id).bind(label.trim()).fetch_optional(pool).await.ok().flatten()
    };
    let automatic = match &person_id {
        Some(person) => voice_profiles::is_automatic(pool, meeting_id, person).await,
        None => false,
    };
    SpeakerEdit { meeting_id: meeting_id.to_string(), person_id, automatic }
}

/// After a manual edit: a newly named speaker teaches their voice; a contact
/// the edit removed from the meeting loses that meeting's share, or, when a
/// voice match had named them, the match is recorded as rejected.
pub(crate) async fn after_speaker_edit(pool: &SqlitePool, edit: SpeakerEdit, new_label: &str) {
    if let Some(person) = &edit.person_id {
        let own = person == SELF_ID;
        let still_named: i64 = if own {
            sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM transcripts WHERE meeting_id = ? AND {}",
                voice_profiles::self_label_sql("speaker"),
            )).bind(&edit.meeting_id).fetch_one(pool).await.unwrap_or(1)
        } else {
            sqlx::query_scalar("SELECT COUNT(*) FROM person_speakers WHERE meeting_id = ? AND person_id = ?")
                .bind(&edit.meeting_id).bind(person).fetch_one(pool).await.unwrap_or(1)
        };
        if still_named == 0 {
            if edit.automatic {
                let reject = if own {
                    sqlx::query("UPDATE self_voice_matches SET status = 'rejected' WHERE meeting_id = ? AND status = 'applied'")
                        .bind(&edit.meeting_id)
                } else {
                    sqlx::query(
                        "UPDATE voice_speaker_matches SET status = 'rejected' \
                         WHERE meeting_id = ? AND person_id = ? AND status = 'applied'",
                    ).bind(&edit.meeting_id).bind(person)
                };
                let _ = reject.execute(pool).await;
            } else {
                forget(person, &edit.meeting_id);
            }
        }
    }
    // "This is me" teaches your voice like naming a contact teaches theirs.
    if is_person_name(new_label) || is_self_label(new_label) {
        learn(&edit.meeting_id, new_label);
    }
}

/// After a meeting is saved: learn from the names the user gave during the
/// recording (not ones live matching gave), then match its unnamed speakers.
pub(crate) async fn meeting_saved(pool: &SqlitePool, meeting_id: &str) {
    let automatic = voice_profiles::take_live_names();
    let labels: Vec<String> = sqlx::query_scalar("SELECT speaker_label FROM person_speakers WHERE meeting_id = ?")
        .bind(meeting_id).fetch_all(pool).await.unwrap_or_default();
    for label in labels.iter().filter(|label| !automatic.contains(label)) {
        learn(meeting_id, label);
    }
    check(meeting_id);
}

#[derive(Debug, Deserialize)]
pub struct VoiceMatchRef {
    pub meeting_id: String,
    pub person_id: String,
    pub original_label: String,
}

/// Restores the labels that voice matches replaced and remembers each as
/// rejected. Returns the meetings that changed.
#[tauri::command]
pub async fn undo_voice_matches(
    state: tauri::State<'_, crate::state::AppState>,
    matches: Vec<VoiceMatchRef>,
) -> Result<Vec<String>, String> {
    let _guard = super::operation_guard().await;
    undo(state.db_manager.pool(), matches).await
}

async fn undo(pool: &SqlitePool, matches: Vec<VoiceMatchRef>) -> Result<Vec<String>, String> {
    let mut changed = Vec::new();
    for reference in matches {
        let own = reference.person_id == SELF_ID;
        let status: Option<String> = if own {
            sqlx::query_scalar("SELECT status FROM self_voice_matches WHERE meeting_id = ? AND original_label = ?")
                .bind(&reference.meeting_id).bind(&reference.original_label)
                .fetch_optional(pool).await.map_err(|error| error.to_string())?
        } else {
            sqlx::query_scalar(
                "SELECT status FROM voice_speaker_matches WHERE meeting_id = ? AND original_label = ? AND person_id = ?",
            ).bind(&reference.meeting_id).bind(&reference.original_label).bind(&reference.person_id)
                .fetch_optional(pool).await.map_err(|error| error.to_string())?
        };
        if status.as_deref() != Some("applied") {
            continue;
        }
        let label: Option<String> = if own {
            // Renaming "You" also renames it inside combined labels.
            let present: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM transcripts WHERE meeting_id = ? AND {}",
                voice_profiles::self_label_sql("speaker"),
            )).bind(&reference.meeting_id).fetch_one(pool).await.map_err(|error| error.to_string())?;
            (present > 0).then(|| SELF_LABEL.to_string())
        } else {
            sqlx::query_scalar("SELECT speaker_label FROM person_speakers WHERE meeting_id = ? AND person_id = ? LIMIT 1")
                .bind(&reference.meeting_id).bind(&reference.person_id)
                .fetch_optional(pool).await.map_err(|error| error.to_string())?
        };
        if let Some(label) = label {
            let rows = voice_profiles::transcript_turns(pool, &reference.meeting_id).await.map_err(|error| error.to_string())?;
            // An empty destination takes the next free "Speaker N".
            let restore = if generic_labels(&rows).contains(&reference.original_label) { "" } else { reference.original_label.as_str() };
            PeopleRepository::rename_meeting_speaker(pool, &reference.meeting_id, &label, restore)
                .await.map_err(|error| format!("Could not undo the name: {error}"))?;
        }
        let reject = if own {
            sqlx::query("UPDATE self_voice_matches SET status = 'rejected' WHERE meeting_id = ? AND original_label = ?")
                .bind(&reference.meeting_id).bind(&reference.original_label)
        } else {
            sqlx::query(
                "UPDATE voice_speaker_matches SET status = 'rejected' WHERE meeting_id = ? AND original_label = ? AND person_id = ?",
            ).bind(&reference.meeting_id).bind(&reference.original_label).bind(&reference.person_id)
        };
        reject.execute(pool).await.map_err(|error| error.to_string())?;
        if !changed.contains(&reference.meeting_id) {
            changed.push(reference.meeting_id);
        }
    }
    Ok(changed)
}

/// Checks every saved meeting against the saved voices in the background.
#[tauri::command]
pub fn apply_voices_to_past_meetings() -> Result<(), String> {
    if !voice_profiles::enabled() {
        return Err("Turn on Voice profiles in Settings > Labs first.".into());
    }
    if !super::pyannote_models_available() {
        return Err("Voice profiles need the speaker models. Download them in Settings > Transcription.".into());
    }
    if voice_profiles::load_all_for_matching().map_err(|error| error.to_string())?.is_empty() {
        return Err("No voices yet. Name a speaker in a meeting to learn their voice.".into());
    }
    check_all();
    Ok(())
}

#[tauri::command]
pub fn get_voice_learning_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scored(label: &str, person: &str, score: f32) -> Scored {
        Scored { label: label.into(), person_id: person.into(), score }
    }

    fn row(label: &str) -> voice_profiles::TranscriptTurn {
        (label.to_string(), Some(0.0), Some(1.0))
    }

    #[test]
    fn generic_labels_include_combined_components_only_once() {
        let rows = vec![row("Speaker 1"), row("Speaker 1 + Speaker 3"), row("Alice"), row("You"), row("Room")];
        let labels: Vec<String> = generic_labels(&rows).into_iter().collect();
        assert_eq!(labels, vec!["Speaker 1".to_string(), "Speaker 3".to_string()]);
    }

    #[test]
    fn one_label_per_contact_and_the_strongest_wins() {
        let present: BTreeSet<String> = ["Speaker 1", "Speaker 2"].iter().map(|s| s.to_string()).collect();
        let chosen = choose(
            vec![scored("Speaker 1", "p1", 0.84), scored("Speaker 2", "p1", 0.91)],
            &present,
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(chosen, vec![scored("Speaker 2", "p1", 0.91)]);
    }

    #[test]
    fn named_contacts_rejected_pairs_and_vanished_labels_are_skipped() {
        let present: BTreeSet<String> = ["Speaker 1", "Speaker 2"].iter().map(|s| s.to_string()).collect();
        let named: HashSet<String> = ["p-named".to_string()].into_iter().collect();
        let rejected: HashSet<(String, String)> = [("Speaker 2".to_string(), "p2".to_string())].into_iter().collect();
        let chosen = choose(
            vec![
                scored("Speaker 1", "p-named", 0.95),
                scored("Speaker 2", "p2", 0.93),
                scored("Speaker 9", "p3", 0.92),
                scored("Speaker 1", "p4", 0.85),
            ],
            &present,
            &named,
            &rejected,
        );
        assert_eq!(chosen, vec![scored("Speaker 1", "p4", 0.85)]);
    }

    async fn meeting_pool() -> SqlitePool {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE meetings (id TEXT PRIMARY KEY, title TEXT, folder_path TEXT); \
             CREATE TABLE transcripts (id TEXT PRIMARY KEY, meeting_id TEXT, speaker TEXT, audio_start_time REAL, audio_end_time REAL); \
             CREATE TABLE people (id TEXT PRIMARY KEY, display_name TEXT NOT NULL, normalized_name TEXT NOT NULL UNIQUE, \
                 notes TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL); \
             CREATE TABLE person_speakers (person_id TEXT NOT NULL, meeting_id TEXT NOT NULL, speaker_label TEXT NOT NULL, \
                 UNIQUE(meeting_id, speaker_label)); \
             INSERT INTO meetings VALUES ('m1', 'Standup', NULL); \
             INSERT INTO people VALUES ('p-david', 'David Gurley', 'david gurley', NULL, 'now', 'now'); \
             INSERT INTO transcripts VALUES ('t1', 'm1', 'Speaker 3', 0, 4), ('t2', 'm1', 'Speaker 3 + Speaker 4', 4, 6), \
                 ('t3', 'm1', 'Speaker 4', 6, 9);",
        ).execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/20261027000000_add_voice_speaker_matches.sql"))
            .execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/20261028010000_add_self_voice_matches.sql"))
            .execute(&pool).await.unwrap();
        pool
    }

    async fn self_status(pool: &SqlitePool) -> Option<String> {
        sqlx::query_scalar("SELECT status FROM self_voice_matches WHERE meeting_id = 'm1' AND original_label = 'Speaker 3'")
            .fetch_optional(pool).await.unwrap()
    }

    #[tokio::test]
    async fn your_voice_names_you_and_undo_restores_and_remembers_it() {
        let pool = meeting_pool().await;
        let applied = apply(&pool, "m1", "Standup", vec![scored("Speaker 3", SELF_ID, 0.9)]).await.unwrap();
        assert_eq!((applied[0].name.as_str(), applied[0].person_id.as_str()), ("You", SELF_ID));
        assert_eq!(labels(&pool).await, vec!["You", "You + Speaker 4", "Speaker 4"]);
        assert_eq!(self_status(&pool).await.as_deref(), Some("applied"));
        assert!(voice_profiles::is_automatic(&pool, "m1", SELF_ID).await);
        // No contact link is made for you.
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM person_speakers").fetch_one(&pool).await.unwrap();
        assert_eq!(links, 0);

        // You are already in the meeting: a second pass changes nothing.
        assert!(apply(&pool, "m1", "Standup", vec![scored("Speaker 4", SELF_ID, 0.95)]).await.unwrap().is_empty());

        let reference = VoiceMatchRef { meeting_id: "m1".into(), person_id: SELF_ID.into(), original_label: "Speaker 3".into() };
        assert_eq!(undo(&pool, vec![reference]).await.unwrap(), vec!["m1".to_string()]);
        assert_eq!(labels(&pool).await, vec!["Speaker 3", "Speaker 3 + Speaker 4", "Speaker 4"]);
        assert_eq!(self_status(&pool).await.as_deref(), Some("rejected"));
        assert!(apply(&pool, "m1", "Standup", vec![scored("Speaker 3", SELF_ID, 0.97)]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn renaming_you_away_after_a_match_rejects_it_and_this_is_me_learns() {
        let pool = meeting_pool().await;
        apply(&pool, "m1", "Standup", vec![scored("Speaker 3", SELF_ID, 0.9)]).await.unwrap();
        let edit = before_speaker_edit(&pool, "m1", "You").await;
        assert!(edit.automatic);
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "You", "").await.unwrap();
        after_speaker_edit(&pool, edit, "Speaker 1").await;
        assert_eq!(self_status(&pool).await.as_deref(), Some("rejected"));
        assert!(take().forget.is_empty());

        // Marking a speaker as you is a manual name, so it teaches your voice.
        let edit = before_speaker_edit(&pool, "m1", "Speaker 4").await;
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "Speaker 4", "You").await.unwrap();
        after_speaker_edit(&pool, edit, "You").await;
        assert!(take().learn.contains(&("m1".to_string(), "You".to_string())));
    }

    async fn labels(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT speaker FROM transcripts ORDER BY id").fetch_all(pool).await.unwrap()
    }

    async fn status(pool: &SqlitePool) -> Option<String> {
        sqlx::query_scalar("SELECT status FROM voice_speaker_matches WHERE meeting_id = 'm1' AND person_id = 'p-david'")
            .fetch_optional(pool).await.unwrap()
    }

    #[tokio::test]
    async fn a_match_names_the_label_and_undo_restores_and_remembers_it() {
        let pool = meeting_pool().await;
        let applied = apply(&pool, "m1", "Standup", vec![scored("Speaker 3", "p-david", 0.9)]).await.unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!((applied[0].name.as_str(), applied[0].original_label.as_str()), ("David Gurley", "Speaker 3"));
        assert_eq!(labels(&pool).await, vec!["David Gurley", "David Gurley + Speaker 4", "Speaker 4"]);
        assert_eq!(status(&pool).await.as_deref(), Some("applied"));
        assert!(voice_profiles::is_automatic(&pool, "m1", "p-david").await);

        // Already named in the meeting: a second pass changes nothing.
        assert!(apply(&pool, "m1", "Standup", vec![scored("Speaker 4", "p-david", 0.95)]).await.unwrap().is_empty());

        let reference = VoiceMatchRef { meeting_id: "m1".into(), person_id: "p-david".into(), original_label: "Speaker 3".into() };
        assert_eq!(undo(&pool, vec![reference]).await.unwrap(), vec!["m1".to_string()]);
        assert_eq!(labels(&pool).await, vec!["Speaker 3", "Speaker 3 + Speaker 4", "Speaker 4"]);
        assert_eq!(status(&pool).await.as_deref(), Some("rejected"));

        // The undone pair is not applied again.
        assert!(apply(&pool, "m1", "Standup", vec![scored("Speaker 3", "p-david", 0.97)]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn renaming_an_automatic_name_away_rejects_it_without_touching_the_voice() {
        let pool = meeting_pool().await;
        apply(&pool, "m1", "Standup", vec![scored("Speaker 3", "p-david", 0.9)]).await.unwrap();
        let edit = before_speaker_edit(&pool, "m1", "David Gurley").await;
        assert!(edit.automatic);
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "David Gurley", "").await.unwrap();
        after_speaker_edit(&pool, edit, "Speaker 1").await;
        assert_eq!(status(&pool).await.as_deref(), Some("rejected"));
        assert!(take().forget.is_empty());
    }

    #[test]
    fn requests_coalesce() {
        let mut pending = Pending::new();
        assert!(pending.is_empty());
        pending.learn.insert(("m1".into(), "Alice".into()));
        pending.learn.insert(("m1".into(), "Alice".into()));
        pending.check.insert("m1".into());
        pending.check.insert("m1".into());
        assert_eq!((pending.learn.len(), pending.check.len()), (1, 1));
        assert!(!pending.is_empty());
    }
}
