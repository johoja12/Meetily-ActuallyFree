# Calendar names and invite attendees Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Auto-name recordings from the current calendar meeting (with Undo) and offer a calendar-named meeting's attendees first in the speaker picker.

**Architecture:** EventKit returns attendees; Rust normalizes them and stores a per-meeting link (`meeting_calendar_events`, synced). The window holds the live link on the live title and links it after save. The picker ranks attendees from the link; contact emails are filled natively after linking and renames.

**Tech Stack:** Tauri 2, Rust/sqlx SQLite, Objective-C EventKit bridge, Next.js/React, Bun isolated tests.

**Spec:** docs/superpowers/specs/2026-10-27-calendar-attendees-design.md

## Global Constraints

- Gated by calendar `enabled && suggest_titles`; off means no behavior change.
- Notes and location never leave native code.
- Only `is_meeting` events are applied automatically.
- A pending group keeps its title.
- Never infer identity from a speaker number; picking an attendee goes through the existing rename path.

## Review Focus

1. An attendee whose EventKit name is an email or empty: picker shows a readable name.
2. Two attendees with the same name: no email is guessed for the contact.
3. Undo after the user already typed a title: Undo is no longer offered to override it (toast action checks the title is still the calendar one).
4. Meeting deleted: its link row is removed on this device and via sync on others.
5. Live title rename / group change keeps `calendarEvent`.

---

### Task 1: Attendees from EventKit and normalization

**Files:** Create `frontend/src-tauri/src/calendar/attendees.rs`; modify `calendar/mod.rs` (CalendarMeeting gains `#[serde(default)] attendees: Vec<CalendarAttendee>`; normalize in `refresh` and `saved.rs` reads), `calendar/macos.m`.

**Produces:** `CalendarAttendee { name: String, email: Option<String>, organizer: bool }`; `pub fn normalize(raw: Vec<CalendarAttendee>) -> Vec<CalendarAttendee>`; `pub fn name_from_email(&str) -> Option<String>`.

Tests (RED first): name from email (`jane.doe+x@a.com` → "Jane Doe", `j_smith-2@a.com` → "J Smith"); a name that is an email is replaced; dedupe by email case-insensitively keeping organizer flag; entries with neither dropped; cap 100; event JSON without `attendees` still parses.

### Task 2: Link table, commands, email fill, sync

**Files:** Create migration `20261027020000_add_meeting_calendar_events.sql`, `frontend/src-tauri/src/calendar/link.rs`; modify `meeting.rs` delete, `sync/tables.rs` TABLES, `diarization/mod.rs` rename/reassign hooks, `lib.rs` registration.

**Produces:** commands `link_meeting_calendar_event(meeting_id, event: CalendarMeeting) -> MeetingCalendarLink`, `get_meeting_calendar_event(meeting_id) -> Option<MeetingCalendarLink>`, `unlink_meeting_calendar_event(meeting_id)`; `MeetingCalendarLink { event_id, title, start_ms, end_ms, attendees }` (camelCase JSON: eventId, startMs, endMs); `pub async fn fill_attendee_emails(pool, meeting_id) -> sqlx::Result<u64>`.

Tests: link/get/replace/unlink; invalid event refused; missing meeting refused; delete meeting removes link; fill: single match sets email, ambiguous name none, contact with email untouched, email owned by another contact none; sync two-device round trip of the link.

### Task 3: Frontend logic

**Files:** Create `frontend/src/lib/calendar-attendees.ts`; modify `lib/calendar-meetings.ts` (type gains `attendees?: CalendarAttendee[]`, add `autoCalendarChoice`), `lib/live-session.ts` (LiveTitle gains `calendarEvent?: CalendarMeeting | null`; `beginLiveSession(title, startedAt, manual, calendarEvent?)`), tests `tests/lib/calendar-attendees.test.ts`, extend `tests/lib/calendar-meetings.test.ts`.

**Produces:** `autoCalendarChoice(events, now): { kind: 'one'; event } | { kind: 'many'; count } | null`; `inviteSuggestions(attendees, people, speakerLabels): InviteSuggestion[]`; `OPEN_CALENDAR_TITLE_EVENT`.

### Task 4: Wiring

- `useRecordingStart`: when no reminder selection and no pending group, read `calendar_snapshot`; if enabled+suggest_titles, `autoCalendarChoice` → apply / toast.
- `LiveSession`: rename keeps `calendarEvent`; Calendar title menu passes the event and listens for the open event; Undo/choose.
- `useRecordingStop`: after save, link `readLiveTitle()?.calendarEvent`.
- `SavedCalendarTitleSuggestions`: after a successful rename, link the event.
- `SpeakerIdentityDialog`: invite section; `useMeetingCalendarLink(meetingId)` hook; refresh on `VOICE`-style event after link.
- Preview mocks for the new commands.

Verify: `pnpm exec tsc --noEmit`, isolated tests, Next build (serialized).

### Task 5: Docs

Update `docs/CALENDAR_MEETING_SUGGESTIONS.md` (flow, privacy, tests, limits) and `docs/DEVELOPMENT_MAP.md`.
