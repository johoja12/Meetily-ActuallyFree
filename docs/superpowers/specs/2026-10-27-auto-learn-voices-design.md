# Auto-learn voices on tag and apply them to past meetings

Status: approved 2026-10-27. Extends Labs feature 7 (named voice profiles,
[LABS_MACWHISPER_FEATURES.md](../../LABS_MACWHISPER_FEATURES.md)).

## Intent

Today a contact's voice is learned only when the user presses Remember voice or
Learn voice, and saved voices are applied only by live matching or by
re-running post-call diarization on a two-track (non room-microphone) meeting.
Tagging a speaker therefore teaches nothing, and room-microphone meetings are
never matched after the call.

Goal: naming a speaker learns that contact's voice automatically, and every
past meeting with unnamed speakers is checked against the saved voices and
renamed when a voice matches confidently. The user can undo an automatic name,
and an undone name is not applied again.

Constraints carried from the existing feature:

- Opt-in: everything is gated on Settings > Labs > Voice profiles and the
  WeSpeaker model. Off means no learning and no matching.
- Speaker numbers are meeting-local and never identity. Matching uses audio.
- Speaker edits preserve transcript rows, text, timestamps and source.
- Heavy inference runs on blocking workers, never on capture or the request
  path of a rename.
- Native work owns its side effects; it does not depend on a WebView promise.

## Design

### Ownership and queue (`diarization/voice_learning.rs`)

One native worker, started in app setup, owns all automatic learning and
matching. Callers enqueue and return immediately:

- `learn(meeting_id, label)` after a speaker is named;
- `forget(person_id, meeting_id)` after a contact loses a meeting;
- `check(meeting_id)` after a meeting is saved or re-diarized;
- a full pass over all meetings after any successful learn/forget, or when the
  user presses "Apply voices to past meetings".

Requests coalesce (a set keyed by meeting/label, person/meeting, meeting), so
repeated tags of one person become one job. The worker emits
`voice-learning-status` `{ running }` while busy, `voice-profiles-changed`
after it changes a voice, and `voice-matches-applied` with the names it set.

### Auto-learn triggers

- `rename_meeting_speaker` and `reassign_transcript_speaker` when the new label
  is a person name (`is_person_name`).
- `api_save_transcript` for every named label that the save linked (live names).
- Untagging: when a rename leaves a contact with no link in that meeting, that
  meeting's share of their voice is removed. A voice left with fewer than two
  turns is forgotten.

Learning reuses `meeting_share` + `store_shares` (per-meeting share, capped at
twelve meetings). Failures (no clean turns, no audio, Labs off, models
missing) are logged, not shown.

### Automatic names never train a voice

Table `voice_speaker_matches(meeting_id, original_label, person_id, status,
created_at)`, `status` in `applied | rejected`, primary key
`(meeting_id, original_label, person_id)`, cascading on meeting/person delete.

- An `applied` row marks a contact's appearance in that meeting as automatic.
  Auto-learn and Learn voice (all meetings) skip such meetings, so a wrong
  match cannot reinforce itself.
- Pressing Remember voice on an automatically named speaker is a confirmation:
  the row is deleted and the meeting becomes an ordinary learning source.
- Undo, or a manual rename that removes or changes an automatic name, sets the
  row to `rejected`; later passes never give that original label that contact.
- Contact merge moves rows to the kept contact.

The `person_speakers` schema is unchanged; it is pinned by the people-migration
checksum repair in `database/manager.rs`.

### Matching a meeting (no re-diarization)

For one meeting, using its saved transcript labels:

1. Track: `mic.mp4` for a room-microphone recording, else `system.mp4`. No
   track, no match.
2. Candidates: generic single labels (`Speaker N`) present in the meeting.
   Combined labels (`Speaker 1 + Speaker 3`) are not candidates themselves.
3. Turns: rows whose label is exactly the candidate, 2–15 s, overlapping no
   row with another label; up to eight, spread across the meeting. At least
   two embedded turns are required.
4. Score: mean embedding against saved voices with the existing `best_match`
   rule (cosine ≥ 0.80 and ≥ 0.08 over the runner-up).
5. Choose: drop contacts already linked in the meeting and rejected pairs; if
   two labels match one contact, keep the higher score.
6. Apply under the diarization operation guard, re-checking that the label
   still exists and the contact is still absent, through
   `PeopleRepository::rename_meeting_speaker` (combined labels have the
   component replaced, as a manual rename does), then insert the `applied` row.

### UI

- A global notifier shows "Named David Gurley in 2 past meetings" with Undo.
  Undo calls `undo_voice_matches`, which renames each label back to its
  original (or the next free `Speaker N` if that label is taken) and marks
  the rows rejected.
- An open meeting page refetches its transcript when a change touches it.
- Settings > Labs > Voice profiles shows "Learning voices…" while the worker
  runs and an "Apply voices to past meetings" button.

## Testing

- Rust unit tests: clean-turn selection, choosing matches (skip named,
  rejected, one label per contact), share removal, migration plus repository
  round trip for applied/rejected/undo/merge, queue coalescing.
- Frontend Bun test: notifier summary wording.
- No private recording is used as a fixture. Real-model matching stays covered
  by the existing ignored model tests; this change does not add a new one.

## Limits

- Crosstalk-heavy meetings may lack two clean turns per label and stay
  unnamed.
- Room-microphone and call-audio voices of one person may score lower against
  each other than same-channel audio.
- Wrong matches are possible at the existing threshold; Undo is the remedy.
