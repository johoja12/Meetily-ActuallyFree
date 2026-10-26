# Local calendar meeting titles and recording prompts

Settings > Calendar connects to calendars synced into macOS Calendar. An Outlook
work account can be added through Calendar > Add Account; Meetily does not reuse
Outlook's sign-in or require its own Microsoft application registration. This
implementation is macOS-only. The app reads calendar data and never edits events.

## User flow

Connect requests macOS Calendar access only after the user clicks it. The user
then chooses calendars; an empty selection reads no events. Title suggestions
and recording reminders have separate switches. The idle recording screen lists
current/soon events with explicit Record actions. During a recording, Calendar
title opens event choices matched to the recording's start. Overlaps remain
choices; neither timing nor an attendee name establishes which call is attended.

A confirmed Record action rechecks native capture state, hands the selected title
to the existing recorder, and starts through the usual model/capture checks. A
live title selection uses the existing rename action. Both count as user-owned
titles: `api_save_transcript.title_is_manual` is optional for old callers and is
persisted in the meeting/transcript transaction. A later summary cannot overwrite
a confirmed calendar title, including one that resembles a default title.

Saved meeting details now include **Suggest name** beside the date/group controls.
Opening it invokes `calendar_recording_suggestions(meetingId)`, which resolves the
saved recording's start and duration natively and queries that historical interval
plus ten minutes on either side. Capture metadata takes precedence over the database
start and transcript-end fallback; post-processing completion time is never used.
Matches rank by overlap, gap and start proximity, retain competing occurrences,
and show up to 12 choices. Selecting a choice calls the existing explicit rename
flow, preserving manual title ownership. Opening suggestions never changes a title.
Missing duration uses the start time; recordings longer than 24 hours return an
explanation. Imported files marked `source: import` are excluded because their saved
start is the import time. Legacy recordings without metadata use the database's
saved date, which may be approximate. Only events still available in selected
synced calendars can be matched; this cannot recover deleted calendar history.

## Native ownership and privacy

`calendar/mod.rs` owns settings, the memory-only event cache, permission completion,
a single EventKit query slot, polling and reminder deduplication. `calendar/macos.m`
is an ARC Objective-C EventKit bridge compiled by `build.rs`; it queries actual
occurrences through EventKit, so timezone/DST conversion and recurring exceptions
are owned by the OS. Public timestamps are Unix milliseconds. The bridge receives
Unix seconds for its NSDate query boundaries; it returns an owned UTF-8 allocation
that Rust always frees. Calendar sources/events do not enter telemetry or logs.
Notes/location are inspected only locally for a meeting-link signal; they are not
returned to JavaScript, persisted, or sent to a model.

`calendar_settings.json` stores only switches and selected calendar identifiers.
Connection activation is persisted by native completion after permission succeeds,
so navigation cannot abandon a required JS preference save. Disconnect stops reads
and clears cached events/prompts; OS Calendar permission remains managed by macOS.
The app declares NSCalendarsFullAccessUsageDescription and the calendars entitlement.
EventKit requires full calendar permission (read/write) to read events, even though
Meetily performs no calendar writes.
Mac bundle validation retains strict entitlement checks with the new declared key.

The worker reads every minute while the app is open and a feature is enabled.
Configuration changes coalesce through a native Notify. One blocking query is
allowed at a time. Permission waits are bounded to 120 seconds; ordinary callers
stop waiting after 15 seconds, retaining the native slot until the operation truly
finishes. A late result cannot restore disconnected/changed settings: revisions
arbitrate result acceptance. Provider errors clear events and queued prompts.
No calendar operation runs on the audio capture callback.

The live cache spans the prior 12 hours and next 24 hours, returning at most 500 events.
Saved-recording queries use their own historical window and share the same single
native query slot, timeout and revision check. They never replace the live cache
or touch reminder queues. A busy query asks the user to retry. Historical event
choices stay in the open popover and are discarded on close, navigation or settings
changes; only an explicitly chosen title is saved.
All-day, free, cancelled and declined events are excluded. Recording prompts need
participants or a recognized meeting link, and are eligible from the selected
lead time (0–15 minutes) through ten minutes after the start, never after the end.
Titles can be chosen from all eligible timed events, including solo appointments.
Reminder history includes occurrence start time, is bounded to 2,048 keys, and is
kept in native memory across WebView reloads. The pending queue holds 20 prompts.
A running recording suppresses prompts for that occurrence. A generic system
notification uses the existing consent, reminder and Do Not Disturb preferences;
private event titles stay in the in-app prompt. The UI requests `calendar_take_prompts` only after completing setup/disclosure. Calendar events
never automatically start or stop capture.

`useCalendar` only reads the native cache, with event invalidation and a 30-second
fallback. Slow earlier responses cannot overwrite newer data. The root
CalendarRecordingPrompts survives route navigation; the compact bar does not own
calendar prompts. Recording start uses a one-use, ten-minute calendar selection
in sessionStorage. Invalid/stale selections fall back to normal title behavior.

## Verification and remaining limits

Native regressions cover reminder windows, exclusions for non-meetings, recurrence
keys, overlaps, deduplication, in-progress recording suppression, bounded queues,
input validation, and explicit title ownership in the database transaction.
Frontend regressions cover event ranking, overlaps, corrupt/stale handoffs,
explicit recording/title actions, stale prompts, consent/setup gating, permission
failure, native save ownership, cache response ordering and listener cleanup.
Frontend files must run in separate Bun invocations, as in DEVELOPMENT_MAP.md.

The browser preview uses explicitly labeled sample calendars/events. Native unit
tests do not request Calendar permission or read real calendars. Live access,
Exchange synchronization, actual recurring/DST events, permission revocation and
real scheduled notification delivery require installed-app qualification with a
user-authorized calendar. Events beyond the query/cache bounds are not offered.
Reminder timing has approximately one-minute polling precision. Prompts need the
app open and history resets when the process exits; quitting can result in another
prompt for an occurrence still in the eligibility window. This is local macOS
Calendar access, not a direct Outlook/Graph or ICS subscription integration.

Qualification on October 26: the native library suite passed 387 tests with eight
opt-in tests ignored. Nine targeted calendar tests passed, including historical range precedence,
import/unknown-duration limits and overlap ranking. All 39 isolated frontend files passed, including
calendar handoff, cache lifecycle, saved-title explicit selection, navigation/error
handling, explicit reminder actions and post-call
cancellation. Browser preview confirmed sample calendar selection, an explicit
Record action adopting the chosen title, and overlapping title choices in the
live header. These browser events were synthetic; no private calendar fixture
was saved. The compact top-left post-call progress card was rendered visually
with a synthetic progress fixture; the temporary preview route was removed.

The combined local v0.2.22 Apple Silicon package built successfully, including
saved-recording suggestions and the compact top-left processing card. Browser
preview verified two historical sample choices, overlap labels and explicit rename
updating both the meeting header and sidebar. Deep signing, exact audio/calendar
entitlements, Calendar usage description, arm64/dependency checks, permission helper,
llama-helper ping/shutdown and synthetic-silence FFmpeg roundtrip passed.
DMG SHA-256: `64f85a46e37ed496b5904554a5f8774c3711d688032d5e919dffbfafc9af3ddf`.
The verified replacement is staged locally; installation remains pending because
the user is actively recording. This is local development, not a published release.
Real EventKit historical matching remains unqualified until the work account is
synced and Calendar access is granted.
