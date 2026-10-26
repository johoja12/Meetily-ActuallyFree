//! Historical lookups are independent of the live cache and reminder queues.
use super::*;
use crate::{database::models::MeetingModel, state::AppState};

const NEARBY_MS: i64 = 10 * 60_000;
#[derive(Serialize)]
pub struct RecordingSuggestions {
    start_ms: i64,
    end_ms: Option<i64>,
    matches: Vec<RecordingMatch>,
}
#[derive(Serialize)]
struct RecordingMatch {
    event: CalendarMeeting,
    overlap_ms: i64,
    gap_ms: i64,
}

fn recording_range(
    database_start: i64,
    transcript_duration: Option<f64>,
    metadata: Option<&serde_json::Value>,
) -> Result<(i64, Option<i64>), String> {
    // Import timestamps describe import time, not when the original call happened.
    if metadata.and_then(|m| m["source"].as_str()) == Some("import") {
        return Err("This imported file has no original recording date. Calendar matching needs the original meeting time.".into());
    }
    let start = metadata
        .and_then(|m| m["created_at"].as_str())
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis())
        .unwrap_or(database_start);
    let duration = metadata
        .and_then(|m| m["duration_seconds"].as_f64())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .or(transcript_duration.filter(|seconds| seconds.is_finite() && *seconds > 0.0));
    if duration.is_some_and(|seconds| seconds > 86_400.0) {
        return Err("Calendar matching supports recordings up to 24 hours.".into());
    }
    let end = duration.and_then(|seconds| start.checked_add((seconds * 1000.0).round() as i64));
    Ok((start, end))
}

fn matches(events: Vec<CalendarMeeting>, start: i64, end: Option<i64>) -> Vec<RecordingMatch> {
    let finish = end.unwrap_or(start);
    let mut seen = std::collections::HashSet::new();
    let mut ranked: Vec<_> = events
        .into_iter()
        .filter(CalendarMeeting::valid)
        .filter(|event| seen.insert(event.key()))
        .filter_map(|event| {
            let overlap_ms = (finish.min(event.end_ms) - start.max(event.start_ms)).max(0);
            let gap_ms = (event.start_ms - finish).max(start - event.end_ms).max(0);
            (gap_ms <= NEARBY_MS).then_some(RecordingMatch {
                event,
                overlap_ms,
                gap_ms,
            })
        })
        .collect();
    ranked.sort_by_key(|item| {
        (
            std::cmp::Reverse(item.overlap_ms),
            item.gap_ms,
            item.event.start_ms.abs_diff(start),
            item.event.key(),
        )
    });
    ranked.truncate(12);
    ranked
}

#[tauri::command]
pub async fn calendar_recording_suggestions(
    db: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<RecordingSuggestions, String> {
    let (settings, revision) = {
        let state = state().lock().unwrap();
        if !state.settings.enabled || !state.settings.suggest_titles {
            return Err("Enable calendar title suggestions in Settings > Calendar.".into());
        }
        if state.settings.selected_calendar_ids.is_empty() {
            return Err("Choose a calendar in Settings > Calendar first.".into());
        }
        (state.settings.clone(), state.revision)
    };
    let pool = db.db_manager.pool();
    let meeting = sqlx::query_as::<_, MeetingModel>(
        "SELECT id, title, created_at, updated_at, folder_path FROM meetings WHERE id = ?",
    )
    .bind(&meeting_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| "Could not read the saved recording")?
    .ok_or("This recording no longer exists")?;
    let duration: Option<f64> = sqlx::query_scalar(
        "SELECT MAX(COALESCE(audio_end_time, audio_start_time + COALESCE(duration, 0))) FROM transcripts WHERE meeting_id = ?",
    ).bind(&meeting_id).fetch_one(pool).await.map_err(|_| "Could not read recording timing")?;
    // The path comes from the saved meeting, never a caller-supplied filesystem path.
    let metadata = tauri::async_runtime::spawn_blocking(move || {
        meeting.folder_path.and_then(|folder| {
            let path = std::path::Path::new(&folder).join("metadata.json");
            // Bound malformed/local metadata reads; no transcript/audio content is read.
            let file = std::fs::File::open(path).ok()?;
            if file.metadata().ok()?.len() > 1_048_576 {
                return None;
            }
            serde_json::from_reader::<_, serde_json::Value>(file).ok()
        })
    })
    .await
    .map_err(|_| "Could not read recording metadata")?;
    let (start, end) = recording_range(
        meeting.created_at.0.timestamp_millis(),
        duration,
        metadata.as_ref(),
    )?;
    if QUERY_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Calendar is refreshing. Try again in a moment.".into());
    }
    let job = tauri::async_runtime::spawn_blocking(move || {
        let _guard = QueryGuard;
        let result = read_platform(
            false,
            &settings.selected_calendar_ids,
            start - NEARBY_MS,
            end.unwrap_or(start) + NEARBY_MS,
        )?;
        if state().lock().unwrap().revision != revision {
            return Err("Calendar settings changed. Open suggestions again.".into());
        }
        if let Some(error) = result.error {
            return Err(error);
        }
        if result.authorization != "authorized" {
            return Err("Calendar access is unavailable. Reconnect in Settings > Calendar.".into());
        }
        // No cache replacement or queue_due_prompts: old meetings cannot create reminders.
        Ok(RecordingSuggestions {
            start_ms: start,
            end_ms: end,
            matches: matches(result.events, start, end),
        })
    });
    tokio::time::timeout(std::time::Duration::from_secs(15), job)
        .await
        .map_err(|_| "Calendar lookup is taking too long. Try again shortly.".to_string())?
        .map_err(|_| "Calendar lookup could not finish".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(id: &str, start: i64, end: i64) -> CalendarMeeting {
        CalendarMeeting {
            id: id.into(),
            title: id.into(),
            calendar_id: "work".into(),
            calendar_name: "Work".into(),
            start_ms: start,
            end_ms: end,
            is_meeting: true,
        }
    }
    #[test]
    fn historical_range_prefers_capture_metadata_not_processing_completion() {
        let metadata = serde_json::json!({"created_at":"2025-01-02T10:00:00+04:00", "duration_seconds":1800, "completed_at":"2025-01-03T00:00:00Z"});
        let (start, end) = recording_range(0, Some(10.0), Some(&metadata)).unwrap();
        assert_eq!(
            start,
            chrono::DateTime::parse_from_rfc3339("2025-01-02T06:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(end, Some(start + 1_800_000));
    }
    #[test]
    fn imports_unknown_duration_and_oversized_ranges_are_explicit() {
        assert!(
            recording_range(0, Some(20.0), Some(&serde_json::json!({"source":"import"}))).is_err()
        );
        assert_eq!(recording_range(1000, None, None).unwrap(), (1000, None));
        assert_eq!(
            recording_range(1000, Some(20.0), None).unwrap(),
            (1000, Some(21000))
        );
        assert!(recording_range(0, Some(86_401.0), None).is_err());
    }
    #[test]
    fn historical_matches_rank_overlap_keep_choices_and_bound_nearby_events() {
        let large = event("large", 1_000_000, 3_000_000);
        let result = matches(
            vec![
                event("nearby", 3_100_000, 4_000_000),
                event("small", 2_500_000, 4_000_000),
                large.clone(),
                large,
                event("far", 4_000_000, 5_000_000),
            ],
            1_000_000,
            Some(3_000_000),
        );
        assert_eq!(
            result
                .iter()
                .map(|m| m.event.id.as_str())
                .collect::<Vec<_>>(),
            vec!["large", "small", "nearby"]
        );
        assert_eq!(result[0].overlap_ms, 2_000_000);
        assert_eq!(result[2].gap_ms, 100_000);
        let point = matches(vec![event("contains", 900_000, 2_000_000)], 1_000_000, None);
        assert_eq!(point.len(), 1);
    }
}
