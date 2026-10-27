//! The calendar event a meeting was named from, kept so the speaker picker
//! can offer the invite's attendees. Written only when a calendar title is
//! applied (at record start, from a reminder, or from a title menu).
use serde::Serialize;
use sqlx::SqlitePool;

use super::attendees::{self, CalendarAttendee};
use super::CalendarMeeting;
use crate::database::repositories::person::normalize_person_name;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingCalendarLink {
    pub event_id: String,
    pub title: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub attendees: Vec<CalendarAttendee>,
}

pub async fn link(pool: &SqlitePool, meeting_id: &str, event: CalendarMeeting) -> Result<MeetingCalendarLink, String> {
    let title = event.title.trim().to_string();
    if event.id.is_empty() || event.id.len() > 512 || title.is_empty() || title.chars().count() > 200 || event.end_ms <= event.start_ms {
        return Err("This calendar event cannot be linked".into());
    }
    let linked = MeetingCalendarLink {
        event_id: event.id,
        title,
        start_ms: event.start_ms,
        end_ms: event.end_ms,
        attendees: attendees::normalize(event.attendees),
    };
    let json = serde_json::to_string(&linked.attendees).map_err(|_| "Could not encode attendees")?;
    let written = sqlx::query(
        "INSERT INTO meeting_calendar_events (meeting_id, event_id, title, start_ms, end_ms, attendees, linked_at) \
         SELECT id, ?, ?, ?, ?, ?, ? FROM meetings WHERE id = ? \
         ON CONFLICT(meeting_id) DO UPDATE SET event_id = excluded.event_id, title = excluded.title, \
            start_ms = excluded.start_ms, end_ms = excluded.end_ms, attendees = excluded.attendees, linked_at = excluded.linked_at",
    )
    .bind(&linked.event_id)
    .bind(&linked.title)
    .bind(linked.start_ms)
    .bind(linked.end_ms)
    .bind(&json)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(meeting_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Could not save the calendar link: {e}"))?;
    if written.rows_affected() == 0 {
        return Err("This meeting no longer exists".into());
    }
    if let Err(error) = fill_attendee_emails(pool, meeting_id).await {
        log::warn!("Could not fill contact emails from the invite: {error}");
    }
    Ok(linked)
}

pub async fn get(pool: &SqlitePool, meeting_id: &str) -> sqlx::Result<Option<MeetingCalendarLink>> {
    let row: Option<(String, String, i64, i64, String)> = sqlx::query_as(
        "SELECT event_id, title, start_ms, end_ms, attendees FROM meeting_calendar_events WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(event_id, title, start_ms, end_ms, attendees)| MeetingCalendarLink {
        event_id,
        title,
        start_ms,
        end_ms,
        // A row synced from another device is cleaned again before display.
        attendees: attendees::normalize(serde_json::from_str(&attendees).unwrap_or_default()),
    }))
}

pub async fn unlink(pool: &SqlitePool, meeting_id: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM meeting_calendar_events WHERE meeting_id = ?").bind(meeting_id).execute(pool).await?;
    Ok(())
}

/// Gives contacts tagged in the meeting the email from its invite, when that
/// is unambiguous: the contact has no email, exactly one attendee has the same
/// name, and no other contact already uses the address. Returns how many
/// contacts changed.
pub async fn fill_attendee_emails(pool: &SqlitePool, meeting_id: &str) -> sqlx::Result<u64> {
    fill_emails(pool, meeting_id, None).await
}

/// `only`: normalized names to consider; None for every tagged contact.
async fn fill_emails(pool: &SqlitePool, meeting_id: &str, only: Option<&[String]>) -> sqlx::Result<u64> {
    let Some(linked) = get(pool, meeting_id).await? else { return Ok(0) };
    let tagged: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT p.id, p.normalized_name FROM people p JOIN person_speakers ps ON ps.person_id = p.id \
         WHERE ps.meeting_id = ? AND (p.email IS NULL OR TRIM(p.email) = '')",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?;
    let mut changed = 0;
    for (person_id, normalized) in tagged {
        if only.is_some_and(|names| !names.contains(&normalized)) {
            continue;
        }
        let mut matching = linked
            .attendees
            .iter()
            .filter(|attendee| normalize_person_name(&attendee.name) == normalized);
        let (Some(only), None) = (matching.next(), matching.next()) else { continue };
        let Some(email) = only.email.as_deref() else { continue };
        let updated = sqlx::query(
            "UPDATE people SET email = ?, updated_at = ? WHERE id = ? AND (email IS NULL OR TRIM(email) = '') \
             AND NOT EXISTS (SELECT 1 FROM people other WHERE other.id <> ? AND LOWER(TRIM(other.email)) = ?)",
        )
        .bind(email)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&person_id)
        .bind(&person_id)
        .bind(email)
        .execute(pool)
        .await?;
        changed += updated.rows_affected();
    }
    Ok(changed)
}

/// After a speaker in the meeting was named `to` ("A" or "A + B"): fill the
/// email of only those contacts, so an email the user cleared on someone else
/// is not refilled by an unrelated rename. Returns how many contacts changed.
pub async fn after_rename(pool: &SqlitePool, meeting_id: &str, to: &str) -> u64 {
    let names: Vec<String> = to.split(" + ").map(normalize_person_name).filter(|name| !name.is_empty()).collect();
    if names.is_empty() {
        return 0;
    }
    match fill_emails(pool, meeting_id, Some(&names)).await {
        Ok(changed) => changed,
        Err(error) => {
            log::warn!("Could not fill contact emails from the invite: {error}");
            0
        }
    }
}

#[tauri::command]
pub async fn link_meeting_calendar_event(
    state: tauri::State<'_, crate::state::AppState>,
    meeting_id: String,
    event: CalendarMeeting,
) -> Result<MeetingCalendarLink, String> {
    link(state.db_manager.pool(), &meeting_id, event).await
}

#[tauri::command]
pub async fn get_meeting_calendar_event(
    state: tauri::State<'_, crate::state::AppState>,
    meeting_id: String,
) -> Result<Option<MeetingCalendarLink>, String> {
    get(state.db_manager.pool(), &meeting_id).await.map_err(|_| "Could not read the calendar link".to_string())
}

#[tauri::command]
pub async fn unlink_meeting_calendar_event(
    state: tauri::State<'_, crate::state::AppState>,
    meeting_id: String,
) -> Result<(), String> {
    unlink(state.db_manager.pool(), &meeting_id).await.map_err(|_| "Could not remove the calendar link".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::repositories::person::PeopleRepository;
    use crate::sync::test_support::migrated_pool;

    fn attendee(name: &str, email: &str) -> CalendarAttendee {
        CalendarAttendee { name: name.into(), email: Some(email.into()), organizer: false }
    }

    fn event(id: &str, attendees: Vec<CalendarAttendee>) -> CalendarMeeting {
        CalendarMeeting {
            id: id.into(),
            title: format!("Event {id}"),
            calendar_id: "work".into(),
            calendar_name: "Work".into(),
            start_ms: 1_000,
            end_ms: 2_000,
            is_meeting: true,
            attendees,
        }
    }

    async fn pool_with_meeting() -> SqlitePool {
        let pool = migrated_pool().await;
        sqlx::raw_sql(
            "INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1', 'Standup', 'c', 'u'); \
             INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker) VALUES \
                ('t1', 'm1', 'hello', 'ts', 'Speaker 1'), ('t2', 'm1', 'hi', 'ts', 'Speaker 2');",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn email_of(pool: &SqlitePool, name: &str) -> Option<String> {
        sqlx::query_scalar("SELECT email FROM people WHERE display_name = ?").bind(name).fetch_one(pool).await.unwrap()
    }

    #[tokio::test]
    async fn link_get_replace_and_unlink() {
        let pool = pool_with_meeting().await;
        assert_eq!(get(&pool, "m1").await.unwrap(), None);
        let linked = link(&pool, "m1", event("a", vec![attendee("", "jane.doe@a.com")])).await.unwrap();
        assert_eq!(linked.attendees[0].name, "Jane Doe");
        assert_eq!(get(&pool, "m1").await.unwrap(), Some(linked));
        link(&pool, "m1", event("b", vec![])).await.unwrap();
        let replaced = get(&pool, "m1").await.unwrap().unwrap();
        assert_eq!((replaced.event_id.as_str(), replaced.attendees.len()), ("b", 0));
        unlink(&pool, "m1").await.unwrap();
        assert_eq!(get(&pool, "m1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn invalid_events_and_missing_meetings_are_refused() {
        let pool = pool_with_meeting().await;
        let mut empty_title = event("a", vec![]);
        empty_title.title = "  ".into();
        assert!(link(&pool, "m1", empty_title).await.is_err());
        let mut backwards = event("a", vec![]);
        backwards.end_ms = backwards.start_ms;
        assert!(link(&pool, "m1", backwards).await.is_err());
        assert!(link(&pool, "missing", event("a", vec![])).await.is_err());
    }

    #[tokio::test]
    async fn deleting_the_meeting_removes_its_link() {
        let pool = pool_with_meeting().await;
        link(&pool, "m1", event("a", vec![])).await.unwrap();
        // Legacy databases may run without foreign keys; the delete must not rely on them.
        sqlx::query("PRAGMA foreign_keys = OFF").execute(&pool).await.unwrap();
        assert!(crate::database::repositories::meeting::MeetingsRepository::delete_meeting(&pool, "m1").await.unwrap());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meeting_calendar_events").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn emails_fill_only_for_one_clear_attendee() {
        let pool = pool_with_meeting().await;
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "Speaker 1", "Jane Doe").await.unwrap();
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "Speaker 2", "Ann Lee").await.unwrap();
        link(
            &pool,
            "m1",
            event("a", vec![attendee("Jane Doe", "jane@a.com"), attendee("Ann Lee", "ann1@a.com"), attendee("Ann Lee", "ann2@a.com")]),
        )
        .await
        .unwrap();
        assert_eq!(email_of(&pool, "Jane Doe").await.as_deref(), Some("jane@a.com"));
        // Two invitees share the name: no guess.
        assert_eq!(email_of(&pool, "Ann Lee").await, None);
    }

    #[tokio::test]
    async fn existing_or_taken_emails_are_left_alone() {
        let pool = pool_with_meeting().await;
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "Speaker 1", "Jane Doe").await.unwrap();
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "Speaker 2", "Bob Roe").await.unwrap();
        sqlx::raw_sql(
            "UPDATE people SET email = 'jane@home.com' WHERE display_name = 'Jane Doe'; \
             INSERT INTO people (id, display_name, normalized_name, email, created_at, updated_at) \
                VALUES ('p-other', 'Robert Roe', 'robert roe', 'bob@a.com', 'c', 'u');",
        )
        .execute(&pool)
        .await
        .unwrap();
        link(&pool, "m1", event("a", vec![attendee("Jane Doe", "jane@a.com"), attendee("Bob Roe", "BOB@a.com")])).await.unwrap();
        assert_eq!(email_of(&pool, "Jane Doe").await.as_deref(), Some("jane@home.com"));
        assert_eq!(email_of(&pool, "Bob Roe").await, None);
    }

    #[tokio::test]
    async fn renaming_to_an_attendee_after_linking_fills_their_email() {
        let pool = pool_with_meeting().await;
        link(&pool, "m1", event("a", vec![attendee("Jane Doe", "jane@a.com")])).await.unwrap();
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "Speaker 1", "Jane Doe").await.unwrap();
        assert_eq!(after_rename(&pool, "m1", "Jane Doe").await, 1);
        assert_eq!(email_of(&pool, "Jane Doe").await.as_deref(), Some("jane@a.com"));
    }

    #[tokio::test]
    async fn a_rename_fills_only_the_person_just_named() {
        let pool = pool_with_meeting().await;
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "Speaker 1", "Jane Doe").await.unwrap();
        link(&pool, "m1", event("a", vec![attendee("Jane Doe", "jane@a.com"), attendee("Bob Roe", "bob@a.com")])).await.unwrap();
        // The user clears Jane's email on purpose; naming Bob must not refill it.
        sqlx::query("UPDATE people SET email = NULL WHERE display_name = 'Jane Doe'").execute(&pool).await.unwrap();
        PeopleRepository::rename_meeting_speaker(&pool, "m1", "Speaker 2", "Bob Roe").await.unwrap();
        assert_eq!(after_rename(&pool, "m1", "Bob Roe").await, 1);
        assert_eq!(email_of(&pool, "Bob Roe").await.as_deref(), Some("bob@a.com"));
        assert_eq!(email_of(&pool, "Jane Doe").await, None);
    }
}
