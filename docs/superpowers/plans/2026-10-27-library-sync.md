# Library Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep two installs' libraries identical through a folder synced by Google Drive (or similar), with audio.

**Architecture:** SQLite triggers record changed rows in an outbox; a native worker gives each change a hybrid-clock version and writes this device's view of each meeting (and the global library) as JSON in `devices/<id>/` of the shared folder; it imports other devices' files with newest-version-wins per row, redirects for same-name contacts/groups and merged contacts, and moves finished recordings into the library.

**Tech Stack:** Rust (Tauri 2, sqlx/SQLite, tokio, serde_json), Next.js/React frontend, Bun tests.

**Spec:** `docs/superpowers/specs/2026-10-27-library-sync-design.md`

## Global Constraints

- The SQLite database never lives in the shared folder; each device writes only `devices/<own-id>/` (plus `library.json` at creation and `recordings/` moves).
- Not synced: `settings`, `transcript_settings`, `licensing`, recording preferences, Labs switches, models, crash reports, `voiceprint`.
- Upserts use `INSERT … ON CONFLICT(<key>) DO UPDATE`, never `REPLACE`.
- Versions: `<unix-ms:013>-<counter:06>-<device-id>`, compared as strings.
- Files written as `<name>.tmp` then renamed; unparsable or `.tmp` files are skipped and retried.
- Paths exported relative to the library root with `/` separators.
- Capture stays nonblocking; the worker owns sync; no cloud API calls.
- Mock-heavy Bun groups run as separate invocations (`scripts/test-isolated.mjs`).

## Review Focus

1. A cloud client delivers a half-downloaded device file → it is skipped and retried, never partially applied. (Task 4 test `partial_file_is_skipped`.)
2. A record references a person or meeting not yet imported (files arrive out of order) → skipped without recording its version, applied next pass. (Task 5 test `child_before_parent_is_retried`.)
3. Both devices already have "David Gurley" with different ids → one contact remains on both, all tags kept. (Task 6 test `same_name_contacts_join`.)
4. A meeting deleted on A while B renamed a speaker in it earlier → the meeting stays deleted on both. (Task 5 test `tombstone_beats_older_edit`.)
5. A recording still being finished/post-processed → not moved until settled. (Task 8 test `unsettled_recording_is_not_moved`.)

---

## File Structure

- `frontend/src-tauri/migrations/20261027010000_add_sync.sql` — `sync_state`, `sync_outbox`, `sync_versions`, `sync_redirects`.
- `frontend/src-tauri/src/sync/mod.rs` — config (`sync.json` in app data), commands, worker loop, events.
- `src/sync/clock.rs` — hybrid logical clock.
- `src/sync/tables.rs` — synced table specs, trigger installation, row ⇄ JSON.
- `src/sync/files.rs` — library layout, atomic writes, device file reading, relative paths.
- `src/sync/engine.rs` — export (outbox → versions → files) and import (files → rows), redirects.
- `src/sync/relocate.rs` — moving settled recordings into the library.
- `src/diarization/voice_profiles.rs` — share versions, export/import of `voices.json`.
- `src/database/repositories/person.rs` — `merge_people` writes a redirect.
- `frontend/src/lib/sync.ts`, `frontend/src/components/SyncSettings.tsx`, `frontend/src/components/SyncNotifier.tsx`, settings page section.
- Docs: `docs/LIBRARY_SYNC.md`, link from `docs/DEVELOPMENT_MAP.md`.

### Task 1: Migration and hybrid clock

**Files:** Create migration, `src/sync/clock.rs`, `src/sync/mod.rs` (module skeleton), register `mod sync;` in `lib.rs`.

**Interfaces — Produces:** `clock::Hlc { fn new(device: &str, last: Option<&str>) -> Self; fn tick(&mut self, now_ms: u64) -> String; fn observe(&mut self, version: &str); fn last(&self) -> String }`.

- [ ] Test: `tick_is_monotonic_under_clock_regression` (now goes backwards → counter increments, string order preserved); `observe_advances_past_remote`; `versions_compare_as_strings`.
- [ ] Implement; run `cargo test -p meetily --lib sync::clock`; commit.

### Task 2: Table specs, triggers, row JSON

**Files:** `src/sync/tables.rs`.

**Interfaces — Produces:** `struct TableSpec { name, key: &[&str], meeting: Option<&str> /* column holding meeting id */ }`, `const TABLES: &[TableSpec]`, `async fn install_triggers(pool)`, `async fn set_capture(conn, on: bool)`, `async fn row_json(conn, spec, key: &Value) -> Option<Value>`, `async fn rows_for_meeting(conn, spec, meeting_id) -> Vec<(Value key, Value row)>`, `async fn global_rows(conn, spec) -> Vec<(Value, Value)>`, `async fn upsert(conn, spec, row: &Value)`, `async fn delete(conn, spec, key: &Value)`, `fn key_of(spec, row: &Value) -> Value`.

Trigger SQL per table and operation (OLD for DELETE):

```sql
CREATE TRIGGER IF NOT EXISTS sync_<t>_<op> AFTER <OP> ON <t>
WHEN (SELECT value FROM sync_state WHERE key = 'capture') = '1'
BEGIN INSERT INTO sync_outbox (table_name, row_key, meeting_id)
VALUES ('<t>', json_object('<k1>', NEW.<k1>, ...), NEW.<meeting col or NULL>); END;
```

UPDATE also records OLD's key when it differs (key columns changed). Row JSON via `json_object` over `PRAGMA table_info` columns; upsert via `INSERT INTO t (cols) SELECT json_extract(?1,'$.c')… WHERE true ON CONFLICT(key) DO UPDATE SET c = excluded.c`, columns = remote keys ∩ local columns.

- [ ] Tests on a migrated in-memory DB (`sqlx::migrate!` run in test): `triggers_capture_only_when_on`, `update_changing_key_records_both_keys`, `row_json_round_trips_through_upsert`, `upsert_never_cascades_children` (upserting an existing meeting keeps its transcripts).
- [ ] Implement; run `cargo test -p meetily --lib sync::tables`; commit.

### Task 3: Library files and paths

**Files:** `src/sync/files.rs`.

**Interfaces — Produces:** `struct Library { root: PathBuf }` with `fn create_or_open(root) -> Result<Library>`, `fn device_dir(&self, id)`, `fn write_json_atomic(path, &Value)`, `fn read_json(path) -> Option<Value>`, `fn other_devices(&self, own) -> Vec<String>`, `fn to_relative(&self, abs: &Path) -> Option<String>`, `fn to_absolute(&self, rel: &str) -> PathBuf`; `struct RecordFile { records: Vec<Record> }`, `struct Record { t: String, k: Value, v: String, row: Option<Value> }`.

- [ ] Tests: `partial_file_is_skipped` (truncated JSON and `.tmp` → None), `relative_paths_use_forward_slashes`, `outside_library_is_none`, `other_devices_excludes_own`.
- [ ] Implement; commit.

### Task 4: Export

**Files:** `src/sync/engine.rs`.

**Interfaces — Consumes:** Tasks 1–3. **Produces:** `async fn export(pool, lib: &Library, device: &str, clock: &mut Hlc) -> Result<ExportSummary>`, `async fn seed_all(pool, clock)` (assign versions to every existing row and queue every meeting).

Drain `sync_outbox` by `seq`; upsert `sync_versions`; collect touched meetings and whether global tables changed; write `devices/<id>/meetings/<mid>.json` (rows + tombstones from `sync_versions` with that meeting id; `meetings.folder_path` mapped through `to_relative`) and `devices/<id>/library.json`; delete drained outbox rows in the same transaction after files are written. `summary_processes` rows are exported only when `status IN ('completed','failed')`.

- [ ] Tests: `export_writes_meeting_file_with_versions`, `deleted_meeting_exports_tombstone`, `folder_path_is_relative_or_null`.
- [ ] Implement; commit.

### Task 5: Import

**Files:** `src/sync/engine.rs`.

**Interfaces — Produces:** `async fn import(pool, lib, device, clock) -> Result<ImportSummary { meetings: Vec<String>, library: bool }>`. Remembers per-file `size:mtime` in `sync_state` (`seen:<path>`).

Within one transaction per file: `set_capture(false)`, apply records whose `v` > local version (library file: redirects → people/groups → tombstones; meeting file: meetings → children), map ids through redirects, resolve `folder_path` via `to_absolute`, meeting tombstone deletes all scoped tables' rows for that meeting explicitly, skip (no version) records whose FK parent is missing, `clock.observe(v)`, `set_capture(true)`, commit.

- [ ] Tests (two migrated DBs + temp library): `two_devices_converge` (create meeting on A, rename speaker on B, notes on A, sync both ways, compare all synced rows), `tombstone_beats_older_edit`, `child_before_parent_is_retried`, `import_does_not_echo` (outbox empty after import), `newer_local_edit_survives_older_remote`.
- [ ] Implement; commit.

### Task 6: Redirects (same-name join, merged contacts)

**Files:** `src/sync/engine.rs`, `src/database/repositories/person.rs` (`merge_people` inserts `sync_redirects('person', source, target)`).

On a `people`/`groups` upsert that hits the unique `normalized_name` of a different id: keep `min(id)`, insert redirect `max → min`, apply redirect (re-point `person_speakers`, `action_items`, `voice_speaker_matches`, or `meetings.group_id`; delete redirected row), then upsert. Redirects are synced records and resolved transitively.

- [ ] Tests: `same_name_contacts_join` (both devices have "David Gurley" with different ids and tags; after sync both have one contact with all tags), `merge_on_one_device_keeps_concurrent_tag_on_other`, `redirect_chains_resolve`.
- [ ] Implement; commit.

### Task 7: Voices

**Files:** `src/diarization/voice_profiles.rs`, `src/sync/engine.rs`.

**Interfaces — Produces:** `VoiceSource.version: String` (serde default ""), `VoiceProfile.forgotten` not needed — `voices.json` = `{ profiles: [{person_id, name, sources}], forgotten: {person_id: version} }`; `pub(crate) fn export_voices(clock) -> Value`, `pub(crate) fn import_voices(&Value, redirects) -> bool` (changed). Local forget records a forgotten version in `voice_profiles_sync.json`.

- [ ] Tests: `newest_share_per_meeting_wins`, `forget_drops_older_shares`, `imported_shares_rebuild_embedding`.
- [ ] Implement; commit.

### Task 8: Recording relocation

**Files:** `src/sync/relocate.rs`.

**Interfaces — Produces:** `async fn relocate_settled(pool, lib, now) -> Vec<String>`; `fn is_settled(folder, now, recording: bool) -> bool`.

Settled: `metadata.json` status `completed`, newest file mtime ≥ 120 s old, no recording active, `try_operation_guard` free. Rename into `recordings/` (suffix ` (2)` on name clash), else copy-verify-delete; update `meetings.folder_path` (captured → syncs).

- [ ] Tests: `unsettled_recording_is_not_moved`, `settled_recording_moves_and_updates_path`, `name_clash_gets_suffix`.
- [ ] Implement; commit.

### Task 9: Worker, config, commands

**Files:** `src/sync/mod.rs`, `lib.rs` (setup start, exit flush, register commands).

Config `sync.json` in `install_data_root()`: `{ device_id, device_name, library_root, enabled }`. Commands: `get_sync_status`, `enable_sync(root, device_name)`, `disable_sync`, `sync_now`. Worker: on enable → `install_triggers`, `set_capture(true)`, `seed_all` (first time per library), loop every 30 s or on wake (outbox debounce 3 s via a poll of `sync_outbox` count): export → import → relocate; emits `sync-status` and `sync-imported`. Errors recorded in status, never panic.

- [ ] Tests: `config_round_trips`, `enable_on_new_folder_creates_layout`.
- [ ] Implement; full `cargo test -p meetily --lib`; commit.

### Task 10: Frontend

**Files:** `src/lib/sync.ts`, `src/lib/sync-status.ts` (pure wording), `src/components/SyncSettings.tsx`, `src/components/SyncNotifier.tsx`, settings page section `sync`, `src/dev/preview/index.ts` mocks, `tests/lib/sync-status.test.ts`.

SyncNotifier listens to `sync-imported` → `announceChange('meetings')`, `announceChange('people')`, and the meeting-page refresh event (reuse `announceVoiceMatchChanges(meetingIds)` window event renamed generically as `MEETING_LABELS_CHANGED`… keep `VOICE_MATCHES_CHANGED_EVENT` consumer, dispatch it with imported meeting ids).

- [ ] Test wording: `describeLastSync`, `describeDevices`.
- [ ] Implement; run isolated tests, `pnpm run build`; commit.

### Task 11: Docs, full verification, build, merge

- [ ] `docs/LIBRARY_SYNC.md` (data flow, interfaces, tests, limits), link in `DEVELOPMENT_MAP.md`.
- [ ] Full native lib tests, isolated frontend tests, Next build.
- [ ] Ask the user before merging, pushing, or rebuilding/installing the app.
