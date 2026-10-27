# Library sync through a shared folder

Design: [superpowers/specs/2026-10-27-library-sync-design.md](superpowers/specs/2026-10-27-library-sync-design.md).
Plan: [superpowers/plans/2026-10-27-library-sync.md](superpowers/plans/2026-10-27-library-sync.md).

Two or more installs keep one library — meetings, transcripts, speaker names,
contacts, groups, notes, summaries, action items, voice matches, voices and
recordings — through a folder that Google Drive for desktop (or Dropbox,
OneDrive, iCloud Drive) syncs. The app makes no cloud calls. Each install keeps
its own SQLite database in app data; the database never enters the folder.

Status: implemented and unit/integration tested with in-memory databases and a
temporary folder. Not yet qualified on two machines with a real cloud client.
Windows is supported by the file format (relative `/` paths) but not qualified.

## Layout

```
<library>/library.json                 { "format": 1 }
<library>/recordings/<folder>/         recordings moved in once settled
<library>/devices/<device-id>/         written only by that device
    device.json                        name, platform, app version, updated
    meetings/<meeting-id>.json         RecordFile: every row of that meeting
    library.json                       RecordFile: people, groups, sync_redirects
    voices.json                        VoicesFile: voice shares + tombstones
```

A record is `{t, k, v, row}`; `row: null` is a tombstone. Files are written as
`<name>.tmp` and renamed (`sync/files.rs`); unreadable, partial or `.tmp` files
are skipped and read again on the next pass.

## Data flow (`frontend/src-tauri/src/sync/`)

| File | Owns |
|---|---|
| `clock.rs` | Hybrid logical clock. Versions `<ms:013>-<counter:06>-<device>` compare as strings. |
| `tables.rs` | The synced tables (`TABLES`), capture triggers, row ⇄ JSON, upsert/delete. |
| `files.rs` | Library layout, atomic JSON, relative/absolute recording paths. |
| `engine.rs` | `seed_all`, `export`, `import`, redirects, `export_voices`/`import_voices`. |
| `relocate.rs` | Moving settled recordings into `recordings/`. |
| `config.rs` | `sync.json` in app data: device id/name, library root, enabled. |
| `mod.rs` | Worker loop, status, commands, quit-time `flush`. |

1. Capture: migration `20261027010000_add_sync.sql` adds `sync_state`,
   `sync_outbox`, `sync_versions`, `sync_redirects`. `install_triggers` adds
   AFTER INSERT/UPDATE/DELETE triggers on every table in `TABLES`, active only
   when `sync_state.capture = '1'`. An update that changes key columns records
   both keys.
2. Export: the worker drains the outbox in order, gives each change a version
   in `sync_versions` (tombstone when the row is gone), marks the meeting or
   library file dirty in `sync_state`, and rewrites dirty files. A dirty mark
   is cleared only after its file is written. `meetings.folder_path` is written
   relative to the library or as null. `summary_processes` rows travel only
   when `completed` or `failed`.
3. Import: for each other device, `library.json` then meeting files whose
   size/mtime changed (`sync_state 'seen:<device>/<file>'`). Inside one
   transaction with capture off and deferred foreign keys: redirects, then
   people/groups, then meetings, then children, then tombstones (children
   first). A record applies only if its version is newer and the row has no
   unexported local change in `sync_outbox` (that change wins when it is
   versioned). A child whose meeting or contact is missing is deferred and the
   file is retried; if the parent was deleted, the child is recorded as
   deleted instead. A meeting tombstone deletes the meeting's rows in every
   scoped table and marks this device's own file for rewriting. A remote null
   `folder_path` keeps the local one. Ids from other devices must be plain
   (`[A-Za-z0-9_-]`, `engine::safe_id`) because they become file names, and
   relative paths must be plain components (no `..`, root, drive or `:`). A
   file that fails to apply is logged and retried without blocking others.
4. Merge rules: same `normalized_name` with different ids keeps the smaller
   id, fills the kept row's empty details from the dropped one and keeps both
   notes (`merge_details`), and stores a redirect. The merged row and the
   redirect get new local versions and are published, so a device that never
   saw the dropped id can follow old references. `merge_people` stores a
   redirect from the merged contact. `apply_redirect` re-points `person_speakers`, `action_items`,
   `voice_speaker_matches` (or `meetings.group_id`) and removes the old row.
5. Voices: each share has a version (`config::stamp`); forgets and share
   removals are tombstoned in `voice_profiles_sync.json`. `merge_remote` keeps
   the newest share per (person, meeting), drops shares older than a forget
   or removal, follows redirects, and rebuilds embeddings. Voices whose
   contact has not arrived yet are held back and the file is read again.
6. Recordings: `relocate_settled` moves a folder whose `metadata.json` status
   is `completed` and whose files are unchanged for 2 minutes, while nothing
   records, retranscribes or identifies speakers — one folder per pass, on a
   blocking thread. Rename when possible; otherwise copy and check sizes,
   point the meeting at the copy, then remove the original. Name clashes get
   ` (2)`. Folders inside a library this device used before
   (`previous_roots`) are never moved out, since other devices may still use
   them. Choosing a new library folder republishes every meeting there.
   Moved meetings are reported in `sync-imported` like imported ones, so an open
   meeting page reloads its folder path; native commands that take a folder from
   the window (retranscription) prefer the meeting's current `folder_path`,
   and `relocate::follow_moves` maps a path inside a folder moved during this
   run to its new place (retranscription, `read_audio_file`, waveforms).
7. Worker: polls the outbox every 3 s and other devices every 30 s, runs on
   `sync_now`, and exports on quit (`flush`, bounded to 5 s). Emits
   `sync-status` and `sync-imported { meeting_ids, library, voices }`.

Commands: `get_sync_status`, `enable_sync(library_root, device_name)`,
`disable_sync`, `sync_now`, `set_sync_device_name`. UI: Settings > Sync
(`components/SyncSettings.tsx`); `components/SyncNotifier.tsx` refreshes open
lists and meeting pages after an import.

Turning sync off stops the worker but leaves capture on, so turning it back on
publishes what changed meanwhile without re-versioning everything.

Not synced: `settings`, `transcript_settings`, `licensing`, recording
preferences, Labs switches, models, crash reports, the microphone voiceprint.

## Tests

`cargo test -p meetily --lib sync::` (29 tests) and
`cargo test -p meetily --lib voice_profiles`:

- clock ordering, regression and restore; trigger capture and the capture
  flag; key changes; row JSON round trip; upsert never cascading children;
- partial/`.tmp` files skipped; relative paths and refusal of `..`/absolute;
- export versions, tombstones, relative-or-null paths, seeding existing rows;
- two-device convergence (create, speaker rename, notes, second meeting),
  no echo, delete beating an older edit, newer local edit surviving, child
  before parent retried, remote null folder keeping the local path;
- same-name contacts joining, a merge on one device keeping a concurrent tag
  on the other, redirect chains;
- voice share newest-wins, forget/removal tombstones, redirects;
- recordings: unsettled not moved, settled moved with path update, clashes;
- a full pass on a new folder creating the layout and publishing.

`tests/lib/sync-status.test.ts` covers status wording.

## Limits

- Newest wins per row; concurrent edits of one row keep one. Notes and
  summaries are not text-merged.
- A device clock far in the future wins until others pass it.
- Copying the app data folder to another machine copies its device id; delete
  `sync.json` on the copy before turning sync on.
- Files a cloud client keeps online-only are fetched when read; mirrored
  (Available offline) mode is recommended.
- Not yet qualified on two real machines.
