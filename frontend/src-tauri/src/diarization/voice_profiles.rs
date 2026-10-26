//! Opt-in WeSpeaker voice profiles for named people. A profile is enrolled only
//! from user-labeled, non-overlapping system-track turns; anonymous diarization
//! channel numbers are never treated as durable identities.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::sync::OnceLock;

use super::models::DiarizationModels;

const MODEL: &str = "wespeaker-resnet34-LM/lda-128";
const MATCH_THRESHOLD: f32 = 0.80;
const MATCH_MARGIN: f32 = 0.08;
const MAX_PROFILES: usize = 50;
/// Clear turns learned from one meeting, spread across it.
const LEARN_TURNS: usize = 20;
/// Turns compared per voice when matching during diarization.
const MATCH_TURNS: usize = 8;
/// Meetings a voice is learned from; the oldest share drops off first.
const MAX_SOURCES: usize = 12;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceProfile {
    pub person_id: String,
    pub name: String,
    pub embedding: Vec<f32>,
    pub samples: u32,
    pub model: String,
    /// Each meeting's share of the voice, so learning from a meeting again
    /// replaces its share instead of counting it twice. A profile saved before
    /// shares were kept has none, and counts as one earlier share.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<VoiceSource>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceSource {
    /// Empty for the share of a profile saved before shares were kept.
    pub meeting_id: String,
    /// Sum of the meeting's turn embeddings.
    pub sum: Vec<f32>,
    pub turns: u32,
    /// Sync version (`sync::config::stamp`) of this share; empty for shares
    /// saved before library sync.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct VoiceProfileInfo {
    pub person_id: String,
    pub name: String,
    pub samples: u32,
    /// Meetings the voice was learned from.
    pub meetings: u32,
}

impl VoiceProfile {
    fn info(&self) -> VoiceProfileInfo {
        VoiceProfileInfo {
            person_id: self.person_id.clone(),
            name: self.name.clone(),
            samples: self.samples,
            meetings: self.sources.len().max(1) as u32,
        }
    }

    /// The meeting shares the voice was built from.
    fn shares(&self) -> Vec<VoiceSource> {
        if !self.sources.is_empty() {
            return self.sources.clone();
        }
        vec![VoiceSource {
            meeting_id: String::new(),
            sum: self.embedding.iter().map(|value| value * self.samples as f32).collect(),
            turns: self.samples,
            version: String::new(),
        }]
    }
}

/// Up to `count` items, evenly spaced from first to last, so a long meeting
/// is learned from all of it rather than its opening minutes.
fn spread<T: Copy>(items: &[T], count: usize) -> Vec<T> {
    if items.len() <= count {
        return items.to_vec();
    }
    (0..count).map(|index| items[index * items.len() / count]).collect()
}

fn share_of(meeting_id: &str, vectors: &[Vec<f32>]) -> VoiceSource {
    let mut sum = vec![0.0f32; 128];
    for vector in vectors {
        for (target, value) in sum.iter_mut().zip(vector) { *target += *value; }
    }
    VoiceSource { meeting_id: meeting_id.to_string(), sum, turns: vectors.len() as u32, version: String::new() }
}

/// Adds shares to a voice's earlier ones. A meeting learned again replaces
/// its earlier share; past `MAX_SOURCES`, the oldest shares drop off.
fn merge_shares(mut earlier: Vec<VoiceSource>, added: Vec<VoiceSource>) -> Vec<VoiceSource> {
    for share in added {
        if !share.meeting_id.is_empty() {
            earlier.retain(|existing| existing.meeting_id != share.meeting_id);
        }
        earlier.push(share);
    }
    if earlier.len() > MAX_SOURCES {
        let extra = earlier.len() - MAX_SOURCES;
        earlier.drain(..extra);
    }
    earlier
}

/// A voice is the direction of the mean of every turn it was learned from.
fn build_profile(person_id: String, name: String, sources: Vec<VoiceSource>) -> Result<VoiceProfile> {
    let turns: u32 = sources.iter().map(|source| source.turns).sum();
    if turns < 2 {
        bail!("{name} needs at least two clear turns of 2 to 15 seconds, with nobody talking over them.");
    }
    let mut embedding = vec![0.0f32; 128];
    for source in sources.iter().filter(|source| source.sum.len() == 128) {
        for (target, value) in embedding.iter_mut().zip(&source.sum) { *target += *value; }
    }
    let norm = embedding.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm <= 1e-6 { bail!("Voice embedding is empty"); }
    for value in &mut embedding { *value /= norm; }
    Ok(VoiceProfile { person_id, name, embedding, samples: turns, model: MODEL.into(), sources })
}

fn path() -> PathBuf {
    crate::paths::install_data_root().join("voice_profiles.json")
}

fn enabled_path() -> PathBuf {
    crate::paths::install_data_root().join("voice_profiles_enabled.txt")
}

static ENABLED: OnceLock<AtomicBool> = OnceLock::new();
static PROFILE_WRITE: Mutex<()> = Mutex::new(());
static LIVE_MATCHER: Mutex<Option<LiveMatcher>> = Mutex::new(None);
/// Names live matching gave out during the current recording. They are
/// automatic, so saving the meeting must not learn a voice from them.
static LIVE_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(crate) fn reset_live_names() {
    if let Ok(mut names) = LIVE_NAMES.lock() { names.clear(); }
}

pub(crate) fn note_live_name(name: &str) {
    if let Ok(mut names) = LIVE_NAMES.lock() {
        if !names.iter().any(|known| known == name) { names.push(name.to_string()); }
    }
}

/// The names live matching gave out since the recording started; called once
/// when the recording is saved.
pub(crate) fn take_live_names() -> Vec<String> {
    LIVE_NAMES.lock().map(|mut names| std::mem::take(&mut *names)).unwrap_or_default()
}

struct LiveMatcher {
    models: DiarizationModels,
    profiles: Vec<VoiceProfile>,
    names: HashMap<String, String>,
}

/// Identity matching supplements Nemotron's meeting-local channels; it does not
/// run a second diarization engine or infer a name from the channel number.
pub fn start_live_matcher() -> Result<()> {
    reset_live_names();
    let mut guard = LIVE_MATCHER.lock().map_err(|_| anyhow::anyhow!("Live profile lock poisoned"))?;
    *guard = None;
    let profiles = load_for_matching()?;
    if profiles.is_empty() { return Ok(()); }
    let models = DiarizationModels::load(&super::diarization_model_dir())?;
    *guard = Some(LiveMatcher { models, profiles, names: HashMap::new() });
    Ok(())
}

pub fn stop_live_matcher() {
    if let Ok(mut guard) = LIVE_MATCHER.lock() { *guard = None; }
}

/// Called on a blocking ASR worker, never on the capture thread.
pub fn name_live_nemotron_turn(label: &str, samples: &[f32]) -> Option<String> {
    if !label.starts_with("Speaker ") { return None; }
    let mut guard = LIVE_MATCHER.lock().ok()?;
    let matcher = guard.as_mut()?;
    // Short VAD fragments are poor enrollment comparisons. A verified earlier
    // match for this meeting-local channel can still label them.
    if samples.len() >= 32_000 && samples.len() <= 240_000 {
        if let Ok(embedding) = matcher.models.embed(samples) {
            if let Some(profile) = best_match(&embedding, &matcher.profiles) {
                matcher.names.insert(label.to_string(), profile.name.clone());
            }
        }
    }
    let name = matcher.names.get(label).cloned();
    if let Some(name) = &name { note_live_name(name); }
    name
}

/// Compare clean, non-overlapping system-track turns for each diarized remote
/// channel. The caller keeps all unnamed channels and original timestamps.
pub fn match_offline_speakers(track: &Path, segments: &[super::DiarizationSegment]) -> Result<HashMap<usize, String>> {
    let profiles = load_for_matching()?;
    if profiles.is_empty() { return Ok(HashMap::new()); }
    let audio = crate::audio::decoder::decode_audio_file(track)?.to_whisper_format();
    let mut models = DiarizationModels::load(&super::diarization_model_dir())?;
    let mut vectors: HashMap<usize, Vec<Vec<f32>>> = HashMap::new();
    for segment in segments {
        let duration = segment.end - segment.start;
        // Cross-track overlap with the local mic does not contaminate the
        // separate system file. Only another remote channel makes it unsafe.
        let remote_overlap = segments.iter().any(|other| {
            other.speaker != segment.speaker
                && other.start < segment.end
                && other.end > segment.start
        });
        if remote_overlap || !(2.0..=15.0).contains(&duration) {
            continue;
        }
        let entries = vectors.entry(segment.speaker).or_default();
        if entries.len() >= MATCH_TURNS { continue; }
        let first = (segment.start * 16_000.0) as usize;
        let last = (segment.end * 16_000.0) as usize;
        if first >= last || last > audio.len() { continue; }
        if let Ok(vector) = models.embed(&audio[first..last]) {
            if vector.len() == 128 { entries.push(vector); }
        }
    }
    let mut names = HashMap::new();
    for (speaker, samples) in vectors {
        if samples.len() < 2 { continue; }
        let mut mean = vec![0.0f32; 128];
        for vector in &samples {
            for (target, value) in mean.iter_mut().zip(vector) { *target += *value; }
        }
        if let Some(profile) = best_match(&mean, &profiles) {
            names.insert(speaker, profile.name.clone());
        }
    }
    Ok(names)
}

fn enabled_flag() -> &'static AtomicBool {
    ENABLED.get_or_init(|| {
        let value = std::fs::read_to_string(enabled_path())
            .map(|text| text.trim() == "true")
            .unwrap_or(false);
        AtomicBool::new(value)
    })
}

pub fn enabled() -> bool {
    enabled_flag().load(Ordering::Relaxed)
}

#[tauri::command]
pub fn get_voice_profiles_enabled() -> bool {
    enabled()
}

#[tauri::command]
pub fn set_voice_profiles_enabled(value: bool) -> Result<(), String> {
    let file = enabled_path();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(file, if value { "true" } else { "false" })
        .map_err(|error| error.to_string())?;
    enabled_flag().store(value, Ordering::Relaxed);
    Ok(())
}

fn load() -> Result<Vec<VoiceProfile>> {
    let bytes = match std::fs::read(path()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    serde_json::from_slice(&bytes).context("Voice profile data is invalid")
}

fn save(profiles: &[VoiceProfile]) -> Result<()> {
    let file = path();
    if let Some(parent) = file.parent() { std::fs::create_dir_all(parent)?; }
    let temp = file.with_extension("json.tmp");
    let backup = file.with_extension("json.bak");
    std::fs::write(&temp, serde_json::to_vec_pretty(profiles)?)?;
    if file.exists() {
        if backup.exists() { std::fs::remove_file(&backup)?; }
        std::fs::rename(&file, &backup)?;
    }
    if let Err(error) = std::fs::rename(&temp, &file) {
        if backup.exists() { let _ = std::fs::rename(&backup, &file); }
        return Err(error.into());
    }
    if backup.exists() { let _ = std::fs::remove_file(backup); }
    Ok(())
}

pub fn load_for_matching() -> Result<Vec<VoiceProfile>> {
    if !enabled() { return Ok(Vec::new()); }
    let _guard = PROFILE_WRITE.lock().map_err(|_| anyhow::anyhow!("Voice profile lock poisoned"))?;
    Ok(load()?.into_iter().filter(|profile| profile.model == MODEL && profile.embedding.len() == 128).collect())
}

pub fn active_person_links() -> Vec<(String, String)> {
    load_for_matching().unwrap_or_default().into_iter()
        .map(|profile| (profile.name, profile.person_id)).collect()
}

fn similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != 128 || b.len() != 128 { return None; }
    let dot = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    (na > 1e-6 && nb > 1e-6).then_some(dot / (na * nb))
}

pub fn best_match<'a>(embedding: &[f32], profiles: &'a [VoiceProfile]) -> Option<&'a VoiceProfile> {
    best_match_scored(embedding, profiles).map(|(profile, _)| profile)
}

/// The confident match and its cosine score, so callers can prefer the
/// stronger of two speakers that match one voice.
pub(crate) fn best_match_scored<'a>(embedding: &[f32], profiles: &'a [VoiceProfile]) -> Option<(&'a VoiceProfile, f32)> {
    let mut ranked: Vec<(f32, &VoiceProfile)> = profiles
        .iter()
        .filter_map(|profile| {
            similarity(embedding, &profile.embedding).map(|score| (score, profile))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (score, profile) = *ranked.first()?;
    let runner_up = ranked.get(1).map(|entry| entry.0).unwrap_or(-1.0);
    (score >= MATCH_THRESHOLD && score - runner_up >= MATCH_MARGIN).then_some((profile, score))
}

/// Why a voice could not be learned. `Unavailable` holds for every meeting
/// (the feature is off, the models are missing, the list is full), so there
/// is no point trying another one.
enum EnrollError {
    Unavailable(String),
    Meeting(String),
}

impl From<EnrollError> for String {
    fn from(error: EnrollError) -> Self {
        match error {
            EnrollError::Unavailable(message) | EnrollError::Meeting(message) => message,
        }
    }
}

/// Embeds up to `LEARN_TURNS` clean turns of one meeting's call audio.
fn embed_turns(track: &Path, turns: &[(f64, f64)]) -> Result<Vec<Vec<f32>>> {
    let audio = crate::audio::decoder::decode_audio_file(track)?.to_whisper_format();
    let mut models = DiarizationModels::load(&super::diarization_model_dir())?;
    let mut vectors = Vec::new();
    for &(start, end) in turns.iter().take(LEARN_TURNS) {
        let first = (start * 16_000.0) as usize;
        let last = (end * 16_000.0) as usize;
        if first >= last || last > audio.len() { continue; }
        if let Ok(vector) = models.embed(&audio[first..last]) {
            if vector.len() == 128 { vectors.push(vector); }
        }
    }
    Ok(vectors)
}

/// The recording that holds the voices a meeting's speaker labels describe:
/// the room microphone in room mode, otherwise the separate call track.
pub(crate) fn call_track(folder: &Path) -> PathBuf {
    folder.join(
        if crate::audio::recording_saver::is_room_microphone_recording(folder) {
            "mic.mp4"
        } else {
            "system.mp4"
        },
    )
}

/// Every saved line's label and recording-relative span, in seconds.
pub(crate) type TranscriptTurn = (String, Option<f64>, Option<f64>);

pub(crate) async fn transcript_turns(pool: &SqlitePool, meeting_id: &str) -> Result<Vec<TranscriptTurn>, sqlx::Error> {
    sqlx::query_as(
        "SELECT COALESCE(speaker, ''), audio_start_time, audio_end_time FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time",
    ).bind(meeting_id).fetch_all(pool).await
}

/// Spans of 2 to 15 seconds that carry exactly `speaker` and overlap no line
/// with any other label, combined labels included.
pub(crate) fn clean_turns(rows: &[TranscriptTurn], speaker: &str) -> Vec<(f64, f64)> {
    let span = |(_, start, end): &TranscriptTurn| Some((start.as_ref()?.to_owned(), end.as_ref()?.to_owned()));
    let others: Vec<(f64, f64)> = rows.iter().filter(|(label, _, _)| label != speaker).filter_map(span).collect();
    rows.iter().filter(|(label, _, _)| label == speaker).filter_map(span)
        .filter(|(start, end)| {
            start.is_finite()
                && end.is_finite()
                && *start >= 0.0
                && end - start >= 2.0
                && end - start <= 15.0
        })
        .filter(|(start, end)| {
            !others
                .iter()
                .any(|(other_start, other_end)| other_start < end && other_end > start)
        })
        .collect()
}

/// Mean embedding per label from up to `MATCH_TURNS` of its turns. Labels
/// with fewer than two embedded turns are left out. Blocking: decodes the
/// track and runs WeSpeaker, so callers use `spawn_blocking`.
pub(crate) fn embed_label_means(track: &Path, turns: &HashMap<String, Vec<(f64, f64)>>) -> Result<HashMap<String, Vec<f32>>> {
    let audio = crate::audio::decoder::decode_audio_file(track)?.to_whisper_format();
    let mut models = DiarizationModels::load(&super::diarization_model_dir())?;
    let mut means = HashMap::new();
    for (label, spans) in turns {
        let mut mean = vec![0.0f32; 128];
        let mut count = 0;
        for &(start, end) in spread(spans, MATCH_TURNS).iter() {
            let first = (start * 16_000.0) as usize;
            let last = (end * 16_000.0) as usize;
            if first >= last || last > audio.len() { continue; }
            if let Ok(vector) = models.embed(&audio[first..last]) {
                if vector.len() == 128 {
                    for (target, value) in mean.iter_mut().zip(&vector) { *target += *value; }
                    count += 1;
                }
            }
        }
        if count >= 2 { means.insert(label.clone(), mean); }
    }
    Ok(means)
}

/// True when this contact's appearance in the meeting was named by a voice
/// match rather than by the user. Such meetings never train their voice.
pub(crate) async fn is_automatic(pool: &SqlitePool, meeting_id: &str, person_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM voice_speaker_matches WHERE meeting_id = ? AND person_id = ? AND status = 'applied'",
    ).bind(meeting_id).bind(person_id).fetch_one(pool).await.map(|count| count > 0).unwrap_or(false)
}

/// Learns a voice after the user names a speaker. Quietly does nothing when
/// the feature is off, the models are missing, the name came from a voice
/// match, or the meeting has no clear audio of them.
pub(crate) async fn learn_tagged_speaker(pool: &SqlitePool, meeting_id: &str, speaker: &str) -> Option<VoiceProfileInfo> {
    if !enabled() || !super::pyannote_models_available() { return None; }
    let person: Option<String> = sqlx::query_scalar(
        "SELECT person_id FROM person_speakers WHERE meeting_id = ? AND speaker_label = ?",
    ).bind(meeting_id).bind(speaker).fetch_optional(pool).await.ok().flatten();
    if let Some(person) = &person {
        if is_automatic(pool, meeting_id, person).await { return None; }
    }
    let outcome = match meeting_share(pool, meeting_id, speaker).await {
        Ok((person_id, name, share)) => store_shares(&person_id, &name, vec![share], false),
        Err(error) => Err(error),
    };
    match outcome {
        Ok(info) => {
            log::info!("Learned {}'s voice from a tagged meeting ({} turns)", info.name, info.samples);
            Some(info)
        }
        Err(error) => {
            log::info!("Did not learn a voice for a tagged speaker: {}", String::from(error));
            None
        }
    }
}

/// Removes one meeting's share after the contact is no longer named in it.
/// A voice left without two turns is forgotten. Returns whether it changed.
pub(crate) fn drop_meeting_share(person_id: &str, meeting_id: &str) -> bool {
    let mut changed = false;
    if let Err(error) = edit_profiles(|profiles| {
        changed = remove_share(profiles, person_id, meeting_id);
        changed
    }) {
        log::warn!("Could not remove a meeting from a voice: {error}");
    }
    if changed {
        record_tombstone(|tombstones| {
            tombstones.removed.insert(format!("{person_id}|{meeting_id}"), crate::sync::config::stamp());
        });
    }
    changed
}

fn remove_share(profiles: &mut Vec<VoiceProfile>, person_id: &str, meeting_id: &str) -> bool {
    let Some(index) = profiles.iter().position(|profile| profile.person_id == person_id) else { return false };
    let shares = profiles[index].shares();
    if !shares.iter().any(|share| share.meeting_id == meeting_id) { return false; }
    let kept: Vec<VoiceSource> = shares.into_iter().filter(|share| share.meeting_id != meeting_id).collect();
    let profile = &profiles[index];
    match build_profile(profile.person_id.clone(), profile.name.clone(), kept) {
        Ok(rebuilt) => profiles[index] = rebuilt,
        Err(_) => { profiles.remove(index); }
    }
    true
}

/// One meeting's share of a named speaker's voice, with the contact it
/// belongs to.
async fn meeting_share(
    pool: &SqlitePool,
    meeting_id: &str,
    speaker: &str,
) -> Result<(String, String, VoiceSource), EnrollError> {
    use EnrollError::{Meeting, Unavailable};
    let failed = |error: sqlx::Error| Meeting(error.to_string());
    if !enabled() {
        return Err(Unavailable("Turn on Voice profiles in Settings > Labs first.".into()));
    }
    if !super::pyannote_models_available() {
        return Err(Unavailable("Voice profiles need the speaker models. Download them in Settings > Transcription.".into()));
    }
    if !crate::database::repositories::person::is_person_name(speaker) {
        if speaker.trim().to_ascii_lowercase().starts_with("speaker ") {
            return Err(Meeting("Name this speaker before learning their voice.".into()));
        }
        return Err(Meeting("Only a named speaker on the call can have a voice profile.".into()));
    }
    let mut identity: Option<(String, String)> = sqlx::query_as(
        "SELECT p.id, p.display_name FROM person_speakers ps JOIN people p ON p.id = ps.person_id \
         WHERE ps.meeting_id = ? AND ps.speaker_label = ?",
    ).bind(meeting_id).bind(speaker).fetch_optional(pool).await.map_err(failed)?;
    if identity.is_none() {
        // Older saves and names entered during live capture can have the name
        // on transcript rows without a durable person_speakers link.
        let saved: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM transcripts WHERE meeting_id = ? AND speaker = ?",
        ).bind(meeting_id).bind(speaker).fetch_one(pool).await.map_err(failed)?;
        if saved == 0 {
            return Err(Meeting("This named speaker is not in the saved transcript yet.".into()));
        }
        crate::database::repositories::person::PeopleRepository::rename_meeting_speaker(
            pool, meeting_id, speaker, speaker,
        ).await.map_err(|error| Meeting(format!("Could not link the named speaker: {error}")))?;
        identity = sqlx::query_as(
            "SELECT p.id, p.display_name FROM person_speakers ps JOIN people p ON p.id = ps.person_id \
             WHERE ps.meeting_id = ? AND ps.speaker_label = ?",
        ).bind(meeting_id).bind(speaker).fetch_optional(pool).await.map_err(failed)?;
    }
    let (person_id, name) = identity.ok_or_else(|| Meeting("Could not link this named speaker to a contact.".into()))?;
    let folder: Option<String> = sqlx::query_scalar("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(meeting_id).fetch_optional(pool).await.map_err(failed)?.flatten();
    let folder = folder.ok_or_else(|| Meeting("This meeting has no saved audio.".into()))?;
    let track = call_track(Path::new(&folder));
    if !track.is_file() {
        return Err(Meeting(
            "This meeting has no separate call audio to learn from. Recordings made with Save audio on keep it; imported files do not.".into(),
        ));
    }
    let rows = transcript_turns(pool, meeting_id).await.map_err(failed)?;
    let turns = spread(&clean_turns(&rows, speaker), LEARN_TURNS);
    let no_clear_turn = || {
        Meeting(format!(
        "{name} has no clear turn of 2 to 15 seconds, with nobody talking over them, in this meeting."
    ))
    };
    if turns.is_empty() {
        return Err(no_clear_turn());
    }
    let vectors = tokio::task::spawn_blocking(move || embed_turns(&track, &turns))
        .await.map_err(|error| Meeting(error.to_string()))?.map_err(|error| Meeting(error.to_string()))?;
    if vectors.is_empty() {
        return Err(no_clear_turn());
    }
    Ok((person_id, name, share_of(meeting_id, &vectors)))
}

/// Adds (or, with `replace`, rebuilds from) meeting shares of a contact's
/// voice and saves it.
fn store_shares(person_id: &str, name: &str, shares: Vec<VoiceSource>, replace: bool) -> Result<VoiceProfileInfo, EnrollError> {
    use EnrollError::{Meeting, Unavailable};
    let _guard = PROFILE_WRITE.lock().map_err(|_| Unavailable("Voice profile lock poisoned".into()))?;
    let mut profiles = load().map_err(|error| Unavailable(error.to_string()))?;
    let existing = profiles.iter().position(|profile| profile.person_id == person_id);
    let earlier = match existing {
        Some(index) if !replace => profiles[index].shares(),
        _ => Vec::new(),
    };
    let stamp = crate::sync::config::stamp();
    let shares = shares.into_iter().map(|share| VoiceSource { version: stamp.clone(), ..share }).collect();
    let profile = build_profile(person_id.to_string(), name.to_string(), merge_shares(earlier, shares))
        .map_err(|error| Meeting(error.to_string()))?;
    if existing.is_none() && profiles.len() >= MAX_PROFILES {
        return Err(Unavailable(format!("Meetily keeps up to {MAX_PROFILES} voices. Forget one to learn another.")));
    }
    let info = profile.info();
    match existing {
        Some(index) => profiles[index] = profile,
        None => profiles.push(profile),
    }
    save(&profiles).map_err(|error| Unavailable(error.to_string()))?;
    Ok(info)
}

/// Learns a named speaker's voice from one meeting. A voice learned before
/// keeps its other meetings; this meeting's share is added or replaced.
#[tauri::command]
pub async fn enroll_voice_profile(
    state: tauri::State<'_, crate::state::AppState>,
    meeting_id: String,
    speaker: String,
) -> Result<VoiceProfileInfo, String> {
    let pool = state.db_manager.pool();
    let (person_id, name, share) = meeting_share(pool, &meeting_id, &speaker).await?;
    let info = store_shares(&person_id, &name, vec![share], false)?;
    // Remembering a voice from an automatically named speaker confirms the
    // name, so this meeting becomes an ordinary source of their voice.
    let _ = sqlx::query("DELETE FROM voice_speaker_matches WHERE meeting_id = ? AND person_id = ? AND status = 'applied'")
        .bind(&meeting_id).bind(&person_id).execute(pool).await;
    // A changed voice can name unnamed speakers in any meeting.
    super::voice_learning::check_all();
    Ok(info)
}

/// Learns a contact's voice. With a meeting, that meeting's audio is added to
/// the voice; without one, the voice is rebuilt from all their recent
/// meetings, so it picks up everything recorded since it was first learned.
#[tauri::command]
pub async fn enroll_person_voice(
    state: tauri::State<'_, crate::state::AppState>,
    person_id: String,
    meeting_id: Option<String>,
) -> Result<VoiceProfileInfo, String> {
    let pool = state.db_manager.pool();
    let name: String = sqlx::query_scalar("SELECT display_name FROM people WHERE id = ?")
        .bind(&person_id).fetch_optional(pool).await.map_err(|error| error.to_string())?
        .ok_or("Contact not found")?;
    let meetings: Vec<(String, String)> = sqlx::query_as(
        "SELECT ps.meeting_id, ps.speaker_label FROM person_speakers ps \
         JOIN meetings m ON m.id = ps.meeting_id \
         WHERE ps.person_id = ? AND (? IS NULL OR ps.meeting_id = ?) \
         AND NOT EXISTS (SELECT 1 FROM voice_speaker_matches v WHERE v.meeting_id = ps.meeting_id \
             AND v.person_id = ps.person_id AND v.status = 'applied') \
         ORDER BY m.created_at DESC LIMIT ?",
    ).bind(&person_id).bind(&meeting_id).bind(&meeting_id).bind(MAX_SOURCES as i64)
        .fetch_all(pool).await.map_err(|error| error.to_string())?;
    if meetings.is_empty() {
        return Err(format!("{name} is not named as a speaker in a saved meeting yet."));
    }
    let tried = meetings.len();
    let mut shares = Vec::new();
    let mut last_problem = String::new();
    // Oldest first, so the newest meetings are the ones a full list keeps.
    for (meeting, label) in meetings.into_iter().rev() {
        match meeting_share(pool, &meeting, &label).await {
            Ok((_, _, share)) => shares.push(share),
            Err(EnrollError::Unavailable(message)) => return Err(message),
            Err(EnrollError::Meeting(message)) => last_problem = message,
        }
    }
    if shares.is_empty() {
        if tried == 1 {
            return Err(last_problem);
        }
        return Err(format!(
            "None of {name}'s last {tried} meetings has clear call audio of them. Learning a voice needs meetings recorded with Save audio on, where they speak for turns of 2 to 15 seconds."
        ));
    }
    let info = store_shares(&person_id, &name, shares, meeting_id.is_none())?;
    super::voice_learning::check_all();
    Ok(info)
}

/// Voices Meetily knows, each under its contact's current name. A voice whose
/// contact no longer exists is forgotten here.
#[tauri::command]
pub async fn list_voice_profiles(state: tauri::State<'_, crate::state::AppState>) -> Result<Vec<VoiceProfileInfo>, String> {
    let people: HashMap<String, String> = sqlx::query_as::<_, (String, String)>("SELECT id, display_name FROM people")
        .fetch_all(state.db_manager.pool()).await.map_err(|error| error.to_string())?
        .into_iter().collect();
    let _guard = PROFILE_WRITE.lock().map_err(|_| "Voice profile lock poisoned")?;
    let mut profiles = load().map_err(|error| error.to_string())?;
    if reconcile_with_contacts(&mut profiles, &people) {
        save(&profiles).map_err(|error| error.to_string())?;
    }
    Ok(profiles.iter().map(VoiceProfile::info).collect())
}

#[tauri::command]
pub fn delete_voice_profile(person_id: String) -> Result<(), String> {
    edit_profiles(|profiles| forget(profiles, &person_id)).map_err(|error| error.to_string())?;
    record_forgotten(&person_id);
    Ok(())
}

fn record_forgotten(person_id: &str) {
    record_tombstone(|tombstones| {
        tombstones.forgotten.insert(person_id.to_string(), crate::sync::config::stamp());
    });
}

/// Voices and shares deleted on this device, so library sync does not bring
/// them back from another device's older copy. Kept next to the voices.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct VoiceTombstones {
    /// person id -> version of the forget.
    #[serde(default)]
    pub forgotten: std::collections::BTreeMap<String, String>,
    /// "person|meeting" -> version of the share's removal.
    #[serde(default)]
    pub removed: std::collections::BTreeMap<String, String>,
}

/// What this device publishes as `voices.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VoicesFile {
    pub profiles: Vec<VoiceProfile>,
    #[serde(default)]
    pub tombstones: VoiceTombstones,
}

fn tombstones_path() -> PathBuf {
    crate::paths::install_data_root().join("voice_profiles_sync.json")
}

fn load_tombstones() -> VoiceTombstones {
    std::fs::read(tombstones_path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn record_tombstone(change: impl FnOnce(&mut VoiceTombstones)) {
    let Ok(_guard) = PROFILE_WRITE.lock() else { return };
    let mut tombstones = load_tombstones();
    change(&mut tombstones);
    if let Err(error) = crate::sync::files::write_json_atomic(&tombstones_path(), &tombstones) {
        log::warn!("Could not record a forgotten voice for sync: {error}");
    }
}

pub(crate) fn export_voices() -> Result<VoicesFile> {
    let _guard = PROFILE_WRITE.lock().map_err(|_| anyhow::anyhow!("Voice profile lock poisoned"))?;
    Ok(VoicesFile { profiles: load()?, tombstones: load_tombstones() })
}

/// Merges another device's voices; returns whether this device's changed.
pub(crate) fn import_voices(remote: &VoicesFile, people: &HashMap<String, String>) -> Result<bool> {
    let _guard = PROFILE_WRITE.lock().map_err(|_| anyhow::anyhow!("Voice profile lock poisoned"))?;
    let mut profiles = load()?;
    let mut tombstones = load_tombstones();
    let before_tombstones = tombstones.clone();
    let changed = merge_remote(&mut profiles, &mut tombstones, remote, people);
    if changed {
        save(&profiles)?;
    }
    if tombstones != before_tombstones {
        crate::sync::files::write_json_atomic(&tombstones_path(), &tombstones)?;
    }
    Ok(changed)
}

/// Newest share per (person, meeting) wins; a forget or a share removal
/// newer than a share drops it; each voice is rebuilt from what remains.
/// `people` maps redirected contact ids to the ids that are kept.
fn merge_remote(
    profiles: &mut Vec<VoiceProfile>,
    tombstones: &mut VoiceTombstones,
    remote: &VoicesFile,
    people: &HashMap<String, String>,
) -> bool {
    let kept = |id: &str| crate::sync::engine::resolve(people, id).to_string();
    let newer = |slot: Option<&String>, version: &str| slot.map_or(true, |known| known.as_str() < version);
    for (person, version) in &remote.tombstones.forgotten {
        let person = kept(person);
        if newer(tombstones.forgotten.get(&person), version) {
            tombstones.forgotten.insert(person, version.clone());
        }
    }
    for (pair, version) in &remote.tombstones.removed {
        let Some((person, meeting)) = pair.split_once('|') else { continue };
        let pair = format!("{}|{meeting}", kept(person));
        if newer(tombstones.removed.get(&pair), version) {
            tombstones.removed.insert(pair, version.clone());
        }
    }

    let mut voices: Vec<(String, String, Vec<VoiceSource>)> = profiles
        .iter()
        .map(|profile| (profile.person_id.clone(), profile.name.clone(), profile.shares()))
        .collect();
    for profile in &remote.profiles {
        let person = kept(&profile.person_id);
        let index = match voices.iter().position(|(id, _, _)| *id == person) {
            Some(index) => index,
            None => {
                voices.push((person.clone(), profile.name.clone(), Vec::new()));
                voices.len() - 1
            }
        };
        let shares = &mut voices[index].2;
        // Shares saved before sync have no meeting or version to merge by.
        for share in profile.sources.iter().filter(|share| !share.meeting_id.is_empty() && !share.version.is_empty()) {
            match shares.iter_mut().find(|local| local.meeting_id == share.meeting_id) {
                Some(local) if local.version.as_str() < share.version.as_str() => *local = share.clone(),
                Some(_) => {}
                None => shares.push(share.clone()),
            }
        }
    }

    let mut rebuilt = Vec::new();
    for (person, name, mut shares) in voices {
        let forgotten = tombstones.forgotten.get(&person);
        shares.retain(|share| {
            forgotten.map_or(true, |forgot| share.version.as_str() > forgot.as_str())
                && tombstones.removed.get(&format!("{person}|{}", share.meeting_id))
                    .map_or(true, |removed| share.version.as_str() > removed.as_str())
        });
        shares.sort_by(|a, b| a.version.cmp(&b.version));
        if let Ok(profile) = build_profile(person, name, merge_shares(Vec::new(), shares)) {
            rebuilt.push(profile);
        }
    }
    let same = serde_json::to_value(&rebuilt).ok() == serde_json::to_value(&*profiles).ok();
    *profiles = rebuilt;
    !same
}

/// Loads the saved voices, applies a change, and saves them if it changed any.
fn edit_profiles(change: impl FnOnce(&mut Vec<VoiceProfile>) -> bool) -> Result<()> {
    let _guard = PROFILE_WRITE.lock().map_err(|_| anyhow::anyhow!("Voice profile lock poisoned"))?;
    let mut profiles = load()?;
    if change(&mut profiles) {
        save(&profiles)?;
    }
    Ok(())
}

fn rename(profiles: &mut [VoiceProfile], person_id: &str, name: &str) -> bool {
    let mut changed = false;
    for profile in profiles.iter_mut().filter(|profile| profile.person_id == person_id && profile.name != name) {
        profile.name = name.to_string();
        changed = true;
    }
    changed
}

/// The kept contact learns from both voices' meetings, or takes over the
/// merged contact's voice when it had none.
fn merge(profiles: &mut Vec<VoiceProfile>, source_id: &str, target_id: &str, target_name: &str) -> bool {
    let Some(index) = profiles.iter().position(|profile| profile.person_id == source_id) else {
        return rename(profiles, target_id, target_name);
    };
    let source = profiles.remove(index);
    if let Some(target) = profiles.iter_mut().find(|profile| profile.person_id == target_id) {
        if let Ok(combined) = build_profile(target_id.to_string(), target_name.to_string(), merge_shares(target.shares(), source.shares())) {
            *target = combined;
        }
    } else {
        profiles.push(VoiceProfile { person_id: target_id.to_string(), ..source });
    }
    rename(profiles, target_id, target_name);
    true
}

fn forget(profiles: &mut Vec<VoiceProfile>, person_id: &str) -> bool {
    let before = profiles.len();
    profiles.retain(|profile| profile.person_id != person_id);
    profiles.len() != before
}

/// `people` maps each contact id to its current name.
fn reconcile_with_contacts(profiles: &mut Vec<VoiceProfile>, people: &HashMap<String, String>) -> bool {
    let before = profiles.len();
    profiles.retain(|profile| people.contains_key(&profile.person_id));
    let mut changed = profiles.len() != before;
    for profile in profiles.iter_mut() {
        if let Some(name) = people.get(&profile.person_id) {
            if &profile.name != name {
                profile.name = name.clone();
                changed = true;
            }
        }
    }
    changed
}

// Contact edits keep the voices in step, so a matched voice is always named
// and linked like its contact. A failure here never fails the contact edit.

pub fn contact_renamed(person_id: &str, name: &str) {
    if let Err(error) = edit_profiles(|profiles| rename(profiles, person_id, name)) {
        log::warn!("Could not rename a voice profile: {error}");
    }
}

pub fn contacts_merged(source_id: &str, target_id: &str, target_name: &str) {
    if let Err(error) = edit_profiles(|profiles| merge(profiles, source_id, target_id, target_name)) {
        log::warn!("Could not move a voice profile to the merged contact: {error}");
    }
}

pub fn contact_deleted(person_id: &str) {
    if let Err(error) = edit_profiles(|profiles| forget(profiles, person_id)) {
        log::warn!("Could not forget a deleted contact's voice: {error}");
    }
    record_forgotten(person_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_requires_margin_and_correct_dimension() {
        let mut vector = vec![0.0; 128]; vector[0] = 1.0;
        let profile = VoiceProfile { person_id: "p1".into(), name: "Alice".into(), embedding: vector.clone(), samples: 2, model: MODEL.into(), sources: Vec::new() };
        assert_eq!(best_match(&vector, &[profile.clone()]).unwrap().name, "Alice");
        assert!(best_match(&vector, &[profile.clone(), profile]).is_none());
        assert!(best_match(&[1.0, 0.0], &[]).is_none());
    }

    fn voice(person_id: &str, name: &str) -> VoiceProfile {
        let mut embedding = vec![0.0; 128];
        embedding[0] = 1.0;
        VoiceProfile { person_id: person_id.into(), name: name.into(), embedding, samples: 2, model: MODEL.into(), sources: Vec::new() }
    }

    /// A share of `turns` turns pointing along axis `axis`.
    fn share(meeting_id: &str, axis: usize, turns: u32) -> VoiceSource {
        let mut sum = vec![0.0; 128];
        sum[axis] = turns as f32;
        VoiceSource { meeting_id: meeting_id.into(), sum, turns, version: String::new() }
    }

    #[test]
    fn renaming_a_contact_renames_only_their_voice() {
        let mut profiles = vec![voice("p1", "Alice"), voice("p2", "Bob")];
        assert!(rename(&mut profiles, "p1", "Alice Smith"));
        assert_eq!(profiles[0].name, "Alice Smith");
        assert_eq!(profiles[1].name, "Bob");
        assert!(!rename(&mut profiles, "p1", "Alice Smith"));
    }

    #[test]
    fn merging_contacts_combines_their_voices() {
        // Only the merged contact had a voice: it moves to the kept contact.
        let mut profiles = vec![voice("dup", "Al")];
        assert!(merge(&mut profiles, "dup", "kept", "Alice"));
        assert_eq!((profiles[0].person_id.as_str(), profiles[0].name.as_str()), ("kept", "Alice"));

        // Both had one: the kept contact learns from both.
        let mut profiles = vec![voice("dup", "Al"), voice("kept", "Alice")];
        assert!(merge(&mut profiles, "dup", "kept", "Alice"));
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].person_id, "kept");
        assert_eq!(profiles[0].samples, 4);
        assert_eq!(profiles[0].info().meetings, 2);

        // Neither had one: nothing to save.
        let mut profiles = vec![voice("other", "Bob")];
        assert!(!merge(&mut profiles, "dup", "kept", "Alice"));
    }

    #[test]
    fn deleted_and_missing_contacts_lose_their_voices() {
        let mut profiles = vec![voice("p1", "Alice"), voice("p2", "Bob")];
        assert!(forget(&mut profiles, "p2"));
        assert!(!forget(&mut profiles, "p2"));
        assert_eq!(profiles.len(), 1);

        let mut profiles = vec![voice("p1", "Alice"), voice("gone", "Carol")];
        let people = HashMap::from([("p1".to_string(), "Alice Smith".to_string())]);
        assert!(reconcile_with_contacts(&mut profiles, &people));
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Alice Smith");
        assert!(!reconcile_with_contacts(&mut profiles, &people));
    }

    #[test]
    fn learning_from_a_meeting_again_replaces_its_share() {
        let first = build_profile("p1".into(), "Alice".into(), merge_shares(Vec::new(), vec![share("m1", 0, 4)])).unwrap();
        let again = merge_shares(first.shares(), vec![share("m1", 0, 6)]);
        assert_eq!(again.len(), 1);
        assert_eq!(build_profile("p1".into(), "Alice".into(), again).unwrap().samples, 6);
    }

    #[test]
    fn a_new_meeting_adds_to_the_voice() {
        let first = build_profile("p1".into(), "Alice".into(), vec![share("m1", 0, 3)]).unwrap();
        let updated = build_profile("p1".into(), "Alice".into(), merge_shares(first.shares(), vec![share("m2", 1, 3)])).unwrap();
        assert_eq!((updated.samples, updated.info().meetings), (6, 2));
        // Equal turns on two axes point between them.
        assert!((updated.embedding[0] - updated.embedding[1]).abs() < 1e-6);
    }

    #[test]
    fn an_older_profile_counts_as_one_earlier_share() {
        let legacy = voice("p1", "Alice");
        let shares = merge_shares(legacy.shares(), vec![share("m1", 1, 2)]);
        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0].meeting_id, "");
        assert_eq!(shares[0].turns, 2);
    }

    #[test]
    fn only_the_latest_meetings_are_kept() {
        let shares: Vec<_> = (0..MAX_SOURCES + 3).map(|index| share(&format!("m{index}"), 0, 1)).collect();
        let kept = merge_shares(Vec::new(), shares);
        assert_eq!(kept.len(), MAX_SOURCES);
        assert_eq!(kept[0].meeting_id, "m3");
    }

    #[test]
    fn turns_are_taken_from_across_the_meeting() {
        let turns: Vec<u32> = (0..100).collect();
        let picked = spread(&turns, 20);
        assert_eq!(picked.len(), 20);
        assert_eq!((picked[0], picked[19]), (0, 95));
        assert_eq!(spread(&turns[..5], 20).len(), 5);
    }

    fn stamped(meeting_id: &str, axis: usize, turns: u32, version: &str) -> VoiceSource {
        VoiceSource { version: version.into(), ..share(meeting_id, axis, turns) }
    }

    fn remote(profiles: Vec<VoiceProfile>, tombstones: VoiceTombstones) -> VoicesFile {
        VoicesFile { profiles, tombstones }
    }

    #[test]
    fn newest_share_per_meeting_wins() {
        let local = build_profile("p1".into(), "Alice".into(), vec![stamped("m1", 0, 2, "0002-a")]).unwrap();
        let theirs = build_profile("p1".into(), "Alice".into(), vec![stamped("m1", 1, 3, "0003-b"), stamped("m2", 0, 2, "0001-b")]).unwrap();
        let mut profiles = vec![local.clone()];
        let mut tombstones = VoiceTombstones::default();
        assert!(merge_remote(&mut profiles, &mut tombstones, &remote(vec![theirs.clone()], VoiceTombstones::default()), &HashMap::new()));
        let shares = profiles[0].shares();
        assert_eq!(shares.len(), 2);
        assert_eq!(shares.iter().find(|share| share.meeting_id == "m1").unwrap().version, "0003-b");
        // Merging the same file again changes nothing.
        assert!(!merge_remote(&mut profiles, &mut tombstones, &remote(vec![theirs], VoiceTombstones::default()), &HashMap::new()));

        // An older remote share does not replace a newer local one.
        let mut profiles = vec![build_profile("p1".into(), "Alice".into(), vec![stamped("m1", 0, 2, "0009-a")]).unwrap()];
        let older = build_profile("p1".into(), "Alice".into(), vec![stamped("m1", 1, 2, "0003-b")]).unwrap();
        merge_remote(&mut profiles, &mut VoiceTombstones::default(), &remote(vec![older], VoiceTombstones::default()), &HashMap::new());
        assert_eq!(profiles[0].shares()[0].version, "0009-a");
    }

    #[test]
    fn forget_and_removal_drop_older_shares_only() {
        let mut profiles = vec![build_profile("p1".into(), "Alice".into(), vec![stamped("m1", 0, 2, "0003-a")]).unwrap()];
        let forgot = VoiceTombstones { forgotten: [("p1".to_string(), "0005-b".to_string())].into(), ..Default::default() };
        assert!(merge_remote(&mut profiles, &mut VoiceTombstones::default(), &remote(Vec::new(), forgot.clone()), &HashMap::new()));
        assert!(profiles.is_empty());

        // Learned again after the forget: kept.
        let mut profiles = vec![build_profile("p1".into(), "Alice".into(), vec![stamped("m1", 0, 2, "0006-a")]).unwrap()];
        let mut tombstones = VoiceTombstones::default();
        merge_remote(&mut profiles, &mut tombstones, &remote(Vec::new(), forgot), &HashMap::new());
        assert_eq!(profiles.len(), 1);
        assert_eq!(tombstones.forgotten["p1"], "0005-b");

        // An untagged meeting's share is removed; the rest of the voice stays.
        let mut profiles = vec![build_profile("p1".into(), "Alice".into(), vec![stamped("m1", 0, 2, "0003-a"), stamped("m2", 0, 2, "0003-a")]).unwrap()];
        let removed = VoiceTombstones { removed: [("p1|m1".to_string(), "0004-b".to_string())].into(), ..Default::default() };
        merge_remote(&mut profiles, &mut VoiceTombstones::default(), &remote(Vec::new(), removed), &HashMap::new());
        assert_eq!(profiles[0].shares().iter().map(|share| share.meeting_id.as_str()).collect::<Vec<_>>(), vec!["m2"]);
    }

    #[test]
    fn imported_voices_follow_contact_redirects() {
        let theirs = build_profile("p-dup".into(), "Al".into(), vec![stamped("m1", 0, 2, "0003-b")]).unwrap();
        let mut profiles = Vec::new();
        let people = HashMap::from([("p-dup".to_string(), "p-kept".to_string())]);
        merge_remote(&mut profiles, &mut VoiceTombstones::default(), &remote(vec![theirs], VoiceTombstones::default()), &people);
        assert_eq!(profiles[0].person_id, "p-kept");
        assert_eq!(profiles[0].samples, 2);
    }

    #[test]
    fn untagging_removes_only_that_meetings_share() {
        let both = build_profile("p1".into(), "Alice".into(), vec![share("m1", 0, 2), share("m2", 1, 3)]).unwrap();
        let mut profiles = vec![both];
        assert!(remove_share(&mut profiles, "p1", "m1"));
        assert_eq!((profiles[0].samples, profiles[0].info().meetings), (3, 1));
        assert!(!remove_share(&mut profiles, "p1", "m1"));
        // Its last meeting gone, the voice is forgotten.
        assert!(remove_share(&mut profiles, "p1", "m2"));
        assert!(profiles.is_empty());
    }

    #[test]
    fn clean_turns_skip_short_long_and_overlapped_lines() {
        let row = |label: &str, start: f64, end: f64| (label.to_string(), Some(start), Some(end));
        let rows = vec![
            row("Speaker 1", 0.0, 3.0),
            row("Speaker 1", 3.0, 4.0),
            row("Speaker 1", 10.0, 30.0),
            row("Speaker 1", 40.0, 45.0),
            row("Speaker 1 + Speaker 2", 44.0, 46.0),
            row("Speaker 1", 50.0, 55.0),
            ("Speaker 1".to_string(), None, None),
        ];
        assert_eq!(clean_turns(&rows, "Speaker 1"), vec![(0.0, 3.0), (50.0, 55.0)]);
        assert!(clean_turns(&rows, "Speaker 2").is_empty());
    }

    #[test]
    fn a_voice_needs_two_clear_turns() {
        assert!(build_profile("p1".into(), "Alice".into(), vec![share("m1", 0, 1)]).is_err());
        assert!(build_profile("p1".into(), "Alice".into(), vec![share("m1", 0, 1), share("m2", 0, 1)]).is_ok());
    }
}
