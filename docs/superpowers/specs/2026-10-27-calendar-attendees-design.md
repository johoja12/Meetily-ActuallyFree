# Calendar names and invite attendees

Status: approved design, 2026-10-27.

## Goal

1. When a recording starts while one calendar meeting is happening, the
   recording takes that meeting's name automatically, with Undo.
2. When a meeting's name came from a calendar event, the speaker picker
   offers that event's attendees first.

## Decisions (from the user)

- Auto-apply with Undo. One current meeting: apply its title and show
  "Named from calendar: <title>" with **Undo** and **Choose another**.
  Several: keep the automatic name and show "N calendar meetings now" with
  **Choose**.
- Picker: a top section "In this meeting's invite" with every attendee except
  the user, matched to an existing contact by email, then by name. Attendees
  already used for another speaker in this meeting sink to the bottom of the
  section. Picking one who is not a contact creates the contact with their
  name and email.

## Scope and assumptions

- macOS only (EventKit). The existing calendar connection and the existing
  "Suggest titles" switch gate everything; with it off nothing changes.
- A calendar reminder's Record button keeps today's behavior (title applied,
  no toast) and now also links attendees.
- A pending group (chosen before recording) keeps the group title; the
  automatic calendar title is skipped.
- Only events with `is_meeting` (attendees or a meeting link) are applied
  automatically; the manual menus still list every event.

## Design

### Attendees from EventKit (`calendar/macos.m`, `calendar/mod.rs`)

Each event gains `attendees: [{name, email, organizer}]`:
- from `event.attendees` plus `event.organizer` when not already listed;
- excluding `isCurrentUser`, `participantType` room/resource/group, and
  `participantStatus` declined;
- email from a `mailto:` URL; at most 100 per event.

Rust (`calendar/attendees.rs`) normalizes: trims, drops a name that is
itself an email, derives a missing name from the email local part
(`jane.doe+x@a.com` → "Jane Doe"), dedupes by lowercased email (else by
normalized name), drops entries with neither, caps at 100 and 200 chars.
Notes and location still never leave native code.

### The link (`migrations/20261027020000_add_meeting_calendar_events.sql`)

`meeting_calendar_events(meeting_id PK → meetings, event_id, title,
start_ms, end_ms, attendees JSON, linked_at)`. One per meeting; a new choice
replaces it. Deleted with the meeting (explicitly in
`delete_meeting_with_transaction`) and synced as a meeting-scoped table.

Commands:
- `link_meeting_calendar_event(meeting_id, event)` validates and normalizes,
  upserts, then fills contact emails (below).
- `get_meeting_calendar_event(meeting_id)` → the link or null.
- `unlink_meeting_calendar_event(meeting_id)`.

Written when a calendar title is applied: automatically at start, from a
reminder, from the live "Calendar title" menu (all held on the live title in
the window and linked right after the meeting is saved, like live notes), and
from "Suggest name" on a saved meeting. Typing a new title keeps the link;
Undo removes it.

### Contact emails

`fill_attendee_emails(pool, meeting_id)`: for each contact tagged in the
meeting with no email, when exactly one attendee has the same normalized
name and no other contact already has that email, set it. Runs after
linking and after `rename_meeting_speaker` / `reassign_transcript_speaker`.
This is how "creates the contact with their email" works for saved and live
meetings alike, through the existing rename path.

### Recording start (`useRecordingStart`, `lib/calendar-meetings.ts`)

`autoCalendarChoice(events, now)` over `calendarTitleSuggestions` filtered
to `is_meeting`: one → apply it (manual title, linked event); several →
"many"; none → nothing. The snapshot is read with `calendar_snapshot` at
start. Undo writes the automatic title back (not manual) and clears the
event. Choose / Choose another open the header's Calendar title menu
(window event `af-open-calendar-title`).

### Picker (`SpeakerIdentityDialog`, `lib/calendar-attendees.ts`)

`inviteSuggestions(attendees, people, speakerLabels)` returns entries
`{key, name, email, personId?, assigned}`; `name` is the matched contact's
display name or the attendee's. Unassigned first, then assigned, each in
invite order. Contacts shown in the invite section are left out of
"Contacts". Saved meetings load the link with `get_meeting_calendar_event`;
live recordings read it from the live title.

## Privacy

Attendee names and emails are stored with the meeting in the local database,
and in the sync folder when library sync is on. They are not sent to any model
or service. `CALENDAR_MEETING_SUGGESTIONS.md` is updated to say so.

## Testing

- Rust: attendee normalization; link/get/unlink; replace; meeting delete
  removes the link; email fill (single match, ambiguous, existing email,
  email taken); sync round-trip of the table.
- Frontend: `autoCalendarChoice`; live-title event kept on rename and
  cleared on Undo; `inviteSuggestions` matching and ordering.
- Not verifiable here: EventKit attendee fields against a real account.

## Limits

- A recovered recording (after a crash) loses the live link; "Suggest name"
  re-links it.
- Attendees are a snapshot taken when linked; later invite changes are not
  followed.
