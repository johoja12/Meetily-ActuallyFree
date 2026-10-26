# Library sync through a shared folder (Google Drive and similar)

Status: approved 2026-10-27.

## Intent

The user records and edits meetings on two Macs, and a Windows PC may join
later. Both should show the same library — meetings, transcripts, speaker
names, contacts, groups, notes, summaries, action items, voices — and both
should be able to play back and match voices on any meeting. Sync goes through
a folder the user's cloud client (Google Drive for desktop, Dropbox, OneDrive,
iCloud Drive) already syncs. The app makes no cloud API calls.

Decisions from the user:

- Record and edit on both machines, sometimes the same day.
- Two Macs now; paths and file handling must not prevent Windows later.
- Audio syncs too.
- Turning sync on merges each machine's existing library; nothing is deleted.

Success: after both machines have synced, every synced row is identical on
both, edits made on either machine while the other is offline appear on the
other after it syncs, and no Drive conflict copies are produced.

## Constraints

- The SQLite database never lives in the shared folder. A cloud client syncs a
  database file as one blob; two writers corrupt it or overwrite each other.
- Each machine writes only its own files in the shared folder.
- Not synced: models, app settings and API keys (`settings`,
  `transcript_settings`, `licensing`), recording preferences, Labs switches,
  crash reports, the microphone user's voiceprint.
- Capture stays nonblocking; sync work runs on a native worker.
- Rows keep their ids, text and timestamps; speaker provenance is preserved.

## Library layout

```
<library>/
  library.json                      format marker: { "format": 1 }
  recordings/<recording folder>/    audio + metadata.json + transcripts.json
  devices/<device-id>/              written only by that device
    device.json                     { id, name, platform, app_version, updated }
    meetings/<meeting-id>.json      every record scoped to that meeting
    library.json                    people, groups, redirects (global records)
    voices.json                     voice shares and forgotten voices
```

Files are written to `<name>.tmp` and renamed. Readers ignore `.tmp` files and
files that do not parse (a cloud client may still be downloading them); they
retry on the next poll. Readers ignore their own device folder.

## Records and versions

A record is `{ "t": table, "k": key, "v": version, "row": {..} | null }`.
`row: null` is a tombstone. `key` is the JSON object of the record's key
columns:

| table | key | scope |
|---|---|---|
| meetings | id | meeting |
| transcripts | id | meeting |
| person_speakers | meeting_id, speaker_label | meeting |
| meeting_notes | meeting_id | meeting |
| summary_processes | meeting_id | meeting (only `completed`/`failed` rows) |
| action_items | id | meeting |
| meeting_whisper_vocabulary | meeting_id | meeting |
| transcript_chunks | meeting_id | meeting |
| voice_speaker_matches | meeting_id, original_label, person_id | meeting |
| people | id | library |
| groups | id | library |
| sync_redirects | kind, from_id | library |

Versions are hybrid logical clock strings
`<unix-ms 13 digits>-<counter 6 digits>-<device-id>`, compared as strings: a
later wall clock wins, the counter orders edits within one millisecond and
after clock regressions, and the device id breaks ties deterministically. The
local clock state persists, and every imported version advances it, so a
machine with a slow clock still produces versions newer than what it has seen.

`sync_versions(table_name, row_key, version, deleted, meeting_id)` holds the
version each machine knows for every synced row it has, local or imported.

## Capture

Migration adds `sync_state(key, value)`, `sync_outbox(seq, table_name,
row_key, meeting_id)`, `sync_versions`, and `sync_redirects(kind, from_id,
to_id, created_at)`. Each synced table gets AFTER INSERT/UPDATE/DELETE triggers
that append `(table, key, meeting)` to `sync_outbox`, guarded by
`WHEN (SELECT value FROM sync_state WHERE key = 'capture') = '1'`.

- `capture` is `'1'` only while sync is on, so the outbox does not grow when
  it is off.
- The import transaction sets `capture = '0'` at its start and back to `'1'`
  before commit. SQLite allows one writer, and other connections never see the
  uncommitted `'0'`, so no local write is missed and no import is echoed.
- Foreign-key cascades fire the child tables' triggers, so a deleted meeting
  also records its children's deletion.

## Sync worker (`sync/` module)

One native task owns sync. It runs when the outbox changes (debounced 3 s),
every 30 s, at launch, on "Sync now", and when the app quits.

Export:

1. Drain the outbox in `seq` order. For each entry assign a fresh version and
   upsert `sync_versions` (`deleted` = the row no longer exists).
2. Rewrite `meetings/<id>.json` for each touched meeting from current rows
   plus that meeting's tombstones, and `library.json` if a global record was
   touched. A file holds this machine's full view, imported rows included, so
   any device's file alone can rebuild the meeting.
3. `meetings.folder_path` is exported relative to the library root
   (`recordings/<name>`, `/` separators). A recording outside the library is
   exported as null until it is moved in.

Import (per other device, files whose size/mtime changed since last import):

1. `library.json` first: redirects, then people and groups, then tombstones.
2. Meeting files: the meetings row first, then children.
3. A record is applied only when its version is greater than the local one.
   Upsert uses `INSERT … ON CONFLICT(<key>) DO UPDATE`, never REPLACE (REPLACE
   would cascade-delete a meeting's children). A tombstone deletes the row.
4. Person and group ids in imported rows go through the redirect map. A record
   whose referenced person or meeting does not exist yet is skipped without
   recording its version and retried on the next pass.
5. `folder_path` is resolved against the local library root.
6. Emits `sync-imported { meeting_ids, library }` so open pages refresh.

## Merge rules beyond newest-wins

- Same-name contacts or groups (unique `normalized_name`) with different ids:
  the smaller id is kept and `sync_redirects(person|group, larger → smaller)`
  is written. Applying a redirect re-points `person_speakers`, `action_items`,
  `voice_speaker_matches` (or `meetings.group_id`) and removes the redirected
  row. Every machine applies the same rule, so they converge.
- `merge_people` writes a redirect from the merged contact to the kept one,
  so a tag made concurrently on another machine lands on the kept contact
  instead of being cascade-deleted with the merged one.
- Voices: `voices.json` carries each voice's per-meeting shares with a
  version and a forgotten-at version per person. The newest share per
  (person, meeting) wins, a share older than the person's forget is dropped,
  and each machine rebuilds embeddings from the merged shares
  (`build_profile`, `MAX_SOURCES`).

## Audio

Recording is unchanged and local. A recording folder outside the library moves
to `<library>/recordings/` once its `metadata.json` status is `completed`, no
file in it changed for 2 minutes, nothing is recording, and the diarization
operation guard is free. The move is a rename when the library is on the same
volume (Google Drive for desktop on macOS is), else copy, verify sizes, and
delete. `meetings.folder_path` is updated, which syncs it. Files a cloud client
keeps online-only are fetched on read; a meeting whose audio is unavailable is
skipped by voice matching and retried later. The UI recommends mirrored mode.

## Setup and UI

Settings > Sync: choose the library folder (create or join), device name,
status (last sync, other devices and when they last synced, last error), Sync
now, turn off. Turning on: write the layout, assign versions to every existing
synced row, export everything, import other devices, and start moving finished
recordings. Turning off stops capture and the worker; local data is kept.

## Testing

- Unit: version ordering and clock persistence, record keys, relative paths,
  partial/unparsable files skipped, redirect resolution and chains.
- Trigger capture and the capture flag against a migrated in-memory database.
- Two-device convergence: two migrated databases and one temporary library
  folder; create, rename speakers, edit notes, merge contacts, delete meetings
  and join same-name contacts on both, sync both ways, assert identical synced
  rows.
- Voice share merge and forget.
- Manual qualification on two Macs with Google Drive (not automated).

## Limits

- Newest wins per row: concurrent edits of one row (a note, a speaker label)
  keep one edit. Notes and summaries are not text-merged.
- A device's clock far in the future makes its edits win until others pass it.
- Copying the app's data folder to another machine copies its device id; the
  setup screen offers to reset the device id.
- Windows is supported by the format but not qualified by this work.
