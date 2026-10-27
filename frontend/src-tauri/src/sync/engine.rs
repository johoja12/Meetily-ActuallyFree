//! Export (outbox -> versions -> this device's files) and import (other
//! devices' files -> rows), with the merge rules that go beyond newest-wins.
//!
//! Every synced row has a version in `sync_versions`. A record from another
//! device is applied only when its version is newer. Each device writes only
//! its own folder, so the cloud client never has two writers for one file.
use anyhow::Result;
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use super::clock::{now_ms, Hlc};
use super::files::{read_json, write_json_atomic, Library, Record, RecordFile, FORMAT};
use super::tables::{self, TableSpec, TABLES};

#[derive(Debug, Default, PartialEq)]
pub struct ExportSummary {
    pub meetings: usize,
    pub library: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct ImportSummary {
    /// Meetings whose rows changed here.
    pub meetings: BTreeSet<String>,
    /// Contacts, groups or redirects changed here.
    pub library: bool,
    /// Records waiting for a meeting or contact that has not arrived yet.
    pub deferred: usize,
}

/// The row key as the capture triggers spell it: a JSON object with the
/// spec's key columns in spec order.
pub fn key_string(spec: &TableSpec, key: &Value) -> String {
    let parts: Vec<String> = spec
        .key
        .iter()
        .map(|column| format!("{}:{}", Value::String((*column).into()), key.get(*column).cloned().unwrap_or(Value::Null)))
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// Rows in these states are mid-flight on the device that owns them.
fn exportable(spec: &TableSpec, row: &Value) -> bool {
    spec.name != "summary_processes"
        || matches!(row.get("status").and_then(Value::as_str), Some("completed" | "failed"))
}

async fn set_state(conn: &mut SqliteConnection, key: &str, value: &str) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO sync_state (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key).bind(value).execute(conn).await?;
    Ok(())
}

pub async fn get_state(conn: &mut SqliteConnection, key: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?").bind(key).fetch_optional(conn).await
}

/// Queues every synced row that has no version yet, so the first export
/// publishes the whole existing library.
pub async fn seed_all(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin().await?;
    for spec in TABLES {
        for (key, row) in tables::all_rows(&mut tx, spec).await? {
            if !exportable(spec, &row) {
                continue;
            }
            let key = key_string(spec, &key);
            let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sync_versions WHERE table_name = ? AND row_key = ?")
                .bind(spec.name).bind(&key).fetch_optional(&mut *tx).await?;
            if known.is_some() {
                continue;
            }
            let meeting = spec.meeting.and_then(|column| row.get(column)).and_then(Value::as_str).map(str::to_string);
            sqlx::query("INSERT INTO sync_outbox (table_name, row_key, meeting_id) VALUES (?, ?, ?)")
                .bind(spec.name).bind(&key).bind(meeting).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Versions every captured change, then rewrites this device's files for the
/// meetings (and the global library) they touched.
pub async fn export(pool: &SqlitePool, library: &Library, device: &str, clock: &mut Hlc) -> Result<ExportSummary> {
    let mut tx = pool.begin().await?;
    let entries: Vec<(i64, String, String, Option<String>)> =
        sqlx::query_as("SELECT seq, table_name, row_key, meeting_id FROM sync_outbox ORDER BY seq")
            .fetch_all(&mut *tx).await?;
    for (_, table, key_text, meeting) in &entries {
        let Some(spec) = tables::spec(table) else { continue };
        let key: Value = serde_json::from_str(key_text).unwrap_or(Value::Null);
        let row = tables::row_json(&mut tx, spec, &key).await?;
        if row.as_ref().is_some_and(|row| !exportable(spec, row)) {
            continue;
        }
        let meeting = if spec.meeting.is_some() { meeting.clone() } else { None };
        sqlx::query(
            "INSERT INTO sync_versions (table_name, row_key, version, deleted, meeting_id) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(table_name, row_key) DO UPDATE SET version = excluded.version, deleted = excluded.deleted, \
             meeting_id = COALESCE(excluded.meeting_id, sync_versions.meeting_id)",
        )
        .bind(spec.name).bind(key_string(spec, &key)).bind(clock.tick(now_ms())).bind(row.is_none() as i64).bind(&meeting)
        .execute(&mut *tx).await?;
        match (&meeting, spec.meeting) {
            (Some(meeting), _) => set_state(&mut tx, &format!("dirty:meeting:{meeting}"), "1").await?,
            (None, None) => set_state(&mut tx, "dirty:library", "1").await?,
            (None, Some(_)) => {}
        }
    }
    if let Some((last, ..)) = entries.last() {
        sqlx::query("DELETE FROM sync_outbox WHERE seq <= ?").bind(last).execute(&mut *tx).await?;
    }
    set_state(&mut tx, "clock", &clock.last()).await?;
    tx.commit().await?;

    // Files are cleared from the dirty list only once written, so a library
    // folder that is briefly unavailable is caught up on the next pass.
    let mut conn = pool.acquire().await?;
    let dirty: Vec<String> = sqlx::query_scalar("SELECT key FROM sync_state WHERE key LIKE 'dirty:%' ORDER BY key")
        .fetch_all(&mut *conn).await?;
    let mut summary = ExportSummary::default();
    for entry in dirty {
        if entry == "dirty:library" {
            write_library_file(&mut conn, library, device).await?;
            summary.library = true;
        } else if let Some(meeting) = entry.strip_prefix("dirty:meeting:") {
            write_meeting_file(&mut conn, library, device, meeting).await?;
            summary.meetings += 1;
        }
        sqlx::query("DELETE FROM sync_state WHERE key = ?").bind(&entry).execute(&mut *conn).await?;
    }
    Ok(summary)
}

async fn versions(conn: &mut SqliteConnection, meeting: Option<&str>) -> sqlx::Result<HashMap<(String, String), (String, bool)>> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT table_name, row_key, version, deleted FROM sync_versions WHERE meeting_id IS ?",
    ).bind(meeting).fetch_all(conn).await?;
    Ok(rows.into_iter().map(|(table, key, version, deleted)| ((table, key), (version, deleted != 0))).collect())
}

async fn records_for(
    conn: &mut SqliteConnection,
    library: &Library,
    meeting: Option<&str>,
) -> sqlx::Result<Vec<Record>> {
    let known = versions(&mut *conn, meeting).await?;
    let mut records = Vec::new();
    for spec in TABLES.iter().filter(|spec| spec.meeting.is_some() == meeting.is_some()) {
        let rows = match meeting {
            Some(meeting) => tables::rows_for_meeting(&mut *conn, spec, meeting).await?,
            None => tables::all_rows(&mut *conn, spec).await?,
        };
        for (key, mut row) in rows {
            if !exportable(spec, &row) {
                continue;
            }
            let Some((version, false)) = known.get(&(spec.name.to_string(), key_string(spec, &key))) else { continue };
            if spec.name == "meetings" {
                // Machine-local paths stay home; a recording inside the library
                // travels as a path relative to it.
                let relative = row.get("folder_path").and_then(Value::as_str)
                    .and_then(|folder| library.to_relative(Path::new(folder)));
                row["folder_path"] = relative.map(Value::String).unwrap_or(Value::Null);
            }
            records.push(Record { t: spec.name.to_string(), k: key, v: version.clone(), row: Some(row) });
        }
    }
    for ((table, key), (version, deleted)) in known {
        if deleted {
            records.push(Record { t: table, k: serde_json::from_str(&key).unwrap_or(Value::Null), v: version, row: None });
        }
    }
    Ok(records)
}

async fn write_meeting_file(conn: &mut SqliteConnection, library: &Library, device: &str, meeting: &str) -> Result<()> {
    if !safe_id(meeting) {
        log::warn!("Not syncing meeting with unusual id {meeting:?}");
        return Ok(());
    }
    let records = records_for(conn, library, Some(meeting)).await?;
    write_json_atomic(&library.meeting_file(device, meeting), &RecordFile { format: FORMAT, records })
}

async fn write_library_file(conn: &mut SqliteConnection, library: &Library, device: &str) -> Result<()> {
    let records = records_for(conn, library, None).await?;
    write_json_atomic(&library.library_file(device), &RecordFile { format: FORMAT, records })
}

fn fingerprint(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some(format!("{}:{modified}", metadata.len()))
}

/// Applies every other device's files that changed since they were last read.
pub async fn import(pool: &SqlitePool, library: &Library, device: &str, clock: &mut Hlc) -> Result<ImportSummary> {
    let mut summary = ImportSummary::default();
    for other in library.other_devices(device) {
        let mut files = vec![(library.library_file(&other), None)];
        let meetings_dir = library.device_dir(&other).join("meetings");
        if let Ok(entries) = std::fs::read_dir(&meetings_dir) {
            let mut meetings: Vec<(std::path::PathBuf, Option<String>)> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
                .filter_map(|path| {
                    let meeting = path.file_stem()?.to_str()?.to_string();
                    Some((path, Some(meeting)))
                })
                .collect();
            meetings.sort();
            files.extend(meetings);
        }
        for (path, meeting) in files {
            if meeting.as_deref().is_some_and(|meeting| !safe_id(meeting)) {
                continue;
            }
            // A file that cannot be applied is retried next pass; it must not
            // hold back every other file.
            if let Err(error) = import_file(pool, library, &other, &path, meeting.as_deref(), clock, &mut summary).await {
                log::warn!("Could not apply {}: {error:#}", path.display());
            }
        }
    }
    Ok(summary)
}

async fn import_file(
    pool: &SqlitePool,
    library: &Library,
    other: &str,
    path: &Path,
    meeting: Option<&str>,
    clock: &mut Hlc,
    summary: &mut ImportSummary,
) -> Result<()> {
    let Some(print) = fingerprint(path) else { return Ok(()) };
    let seen = format!("seen:{other}/{}", path.file_name().and_then(|name| name.to_str()).unwrap_or_default());
    if get_state(&mut *pool.acquire().await?, &seen).await?.as_deref() == Some(print.as_str()) {
        return Ok(());
    }
    // A file still downloading or from a newer app is read again later.
    let Some(file) = read_json::<RecordFile>(path) else { return Ok(()) };
    if file.format > FORMAT {
        log::warn!("Skipping {} written by a newer Meetily (format {})", path.display(), file.format);
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    // A same-name join re-points tags to the kept contact before its row is
    // written; foreign keys are still enforced, at commit. Resets per transaction.
    sqlx::query("PRAGMA defer_foreign_keys = ON").execute(&mut *tx).await?;
    let capture = get_state(&mut tx, "capture").await?.unwrap_or_else(|| "0".into());
    tables::set_capture(&mut tx, false).await?;
    let deferred = apply_records(&mut tx, library, file.records, meeting, clock, summary).await?;
    tables::set_capture(&mut tx, capture == "1").await?;
    set_state(&mut tx, "clock", &clock.last()).await?;
    if deferred == 0 {
        set_state(&mut tx, &seen, &print).await?;
    }
    tx.commit().await?;
    summary.deferred += deferred;
    Ok(())
}

/// Ids from other devices become file names; only plain ids are accepted.
pub fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The kept contact or group, with details only the dropped one had.
/// Notes from both are kept. Every device computes the same result.
fn merge_details(table: &str, kept: &Value, dropped: &Value) -> Value {
    let mut merged = kept.clone();
    let text = |row: &Value, column: &str| row.get(column).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string);
    let fill: &[&str] = if table == "people" { &["email", "company", "role", "phone"] } else { &["color", "kind", "description", "schedule"] };
    for column in fill {
        if text(&merged, column).is_none() {
            if let Some(value) = dropped.get(*column).filter(|value| !value.is_null()) {
                merged[*column] = value.clone();
            }
        }
    }
    if table == "people" {
        merged["notes"] = match (text(kept, "notes"), text(dropped, "notes")) {
            (Some(a), Some(b)) if a != b => Value::String(format!("{a}\n\n{b}")),
            (a, b) => a.or(b).map(Value::String).unwrap_or(Value::Null),
        };
        let manual = |row: &Value| row.get("is_manual").and_then(Value::as_i64).unwrap_or(0);
        merged["is_manual"] = Value::from(manual(kept).max(manual(dropped)));
    }
    merged
}

async fn tombstoned(conn: &mut SqliteConnection, table: &str, id: &str) -> sqlx::Result<bool> {
    let Some(spec) = tables::spec(table) else { return Ok(false) };
    let key = key_string(spec, &serde_json::json!({ "id": id }));
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sync_versions WHERE table_name = ? AND row_key = ? AND deleted = 1")
        .bind(table).bind(key).fetch_optional(conn).await?;
    Ok(found.is_some())
}

/// Columns that name a contact (`person`) or a group (`group`).
fn reference_columns(spec: &TableSpec) -> &'static [(&'static str, &'static str)] {
    match spec.name {
        "person_speakers" | "action_items" | "voice_speaker_matches" => &[("person_id", "person")],
        "meetings" => &[("group_id", "group")],
        _ => &[],
    }
}

fn priority(table: &str) -> u8 {
    match table {
        "sync_redirects" => 0,
        "people" | "groups" => 1,
        "meetings" => 2,
        _ => 3,
    }
}

async fn redirect_map(conn: &mut SqliteConnection, kind: &str) -> sqlx::Result<HashMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT from_id, to_id FROM sync_redirects WHERE kind = ?")
        .bind(kind).fetch_all(conn).await?;
    Ok(rows.into_iter().collect())
}

fn redirected(row: &mut Value, spec: &TableSpec, people: &HashMap<String, String>, groups: &HashMap<String, String>) {
    for (column, kind) in reference_columns(spec) {
        let map = if *kind == "person" { people } else { groups };
        if let Some(id) = row.get(*column).and_then(Value::as_str) {
            let kept = resolve(map, id).to_string();
            row[*column] = Value::String(kept);
        }
    }
}

async fn exists(conn: &mut SqliteConnection, table: &str, id: &str) -> sqlx::Result<bool> {
    let found: Option<i64> = sqlx::query_scalar(&format!("SELECT 1 FROM \"{table}\" WHERE id = ?"))
        .bind(id).fetch_optional(conn).await?;
    Ok(found.is_some())
}

/// Re-points everything that names `from` at `to` and removes `from`. Runs
/// with capture off: every device applies the same redirect itself.
pub async fn apply_redirect(conn: &mut SqliteConnection, kind: &str, from: &str, to: &str) -> sqlx::Result<()> {
    if from == to {
        return Ok(());
    }
    let statements: &[&str] = if kind == "person" {
        &[
            "UPDATE person_speakers SET person_id = ?2 WHERE person_id = ?1",
            "UPDATE action_items SET person_id = ?2 WHERE person_id = ?1",
            "UPDATE OR IGNORE voice_speaker_matches SET person_id = ?2 WHERE person_id = ?1",
            "DELETE FROM voice_speaker_matches WHERE person_id = ?1",
            "DELETE FROM people WHERE id = ?1",
        ]
    } else {
        &["UPDATE meetings SET group_id = ?2 WHERE group_id = ?1", "DELETE FROM groups WHERE id = ?1"]
    };
    for statement in statements {
        sqlx::query(statement).bind(from).bind(to).execute(&mut *conn).await?;
    }
    Ok(())
}

async fn record_version(conn: &mut SqliteConnection, table: &str, key: &str, version: &str, deleted: bool, meeting: Option<&str>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO sync_versions (table_name, row_key, version, deleted, meeting_id) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(table_name, row_key) DO UPDATE SET version = excluded.version, deleted = excluded.deleted, \
         meeting_id = COALESCE(excluded.meeting_id, sync_versions.meeting_id)",
    ).bind(table).bind(key).bind(version).bind(deleted as i64).bind(meeting).execute(conn).await?;
    Ok(())
}

/// Applies one file's records; returns how many wait for a missing parent.
async fn apply_records(
    conn: &mut SqliteConnection,
    library: &Library,
    mut records: Vec<Record>,
    file_meeting: Option<&str>,
    clock: &mut Hlc,
    summary: &mut ImportSummary,
) -> Result<usize> {
    // Parents before children; deletions after every upsert, children first.
    records.sort_by_key(|record| match record.row {
        Some(_) => (0, priority(&record.t)),
        None => (1, 3 - priority(&record.t)),
    });
    let mut people = redirect_map(&mut *conn, "person").await?;
    let mut groups = redirect_map(&mut *conn, "group").await?;
    let mut deferred = 0;
    for record in records {
        let Some(spec) = tables::spec(&record.t) else { continue };
        let mut row = record.row.clone();
        if let Some(row) = row.as_mut() {
            redirected(row, spec, &people, &groups);
        }
        let mut key = record.k.clone();
        redirected(&mut key, spec, &people, &groups);
        if let Some(row) = &row {
            key = tables::key_of(spec, row);
        }
        let key_text = key_string(spec, &key);
        let local: Option<String> = sqlx::query_scalar("SELECT version FROM sync_versions WHERE table_name = ? AND row_key = ?")
            .bind(spec.name).bind(&key_text).fetch_optional(&mut *conn).await?;
        if local.as_deref().is_some_and(|local| local >= record.v.as_str()) {
            continue;
        }
        // A local edit not exported yet gets a newer version when it is, so
        // it wins over anything already published.
        let pending: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sync_outbox WHERE table_name = ? AND row_key = ? LIMIT 1")
            .bind(spec.name).bind(&key_text).fetch_optional(&mut *conn).await?;
        if pending.is_some() {
            continue;
        }
        let meeting = spec.meeting
            .and_then(|column| row.as_ref().unwrap_or(&key).get(column).and_then(Value::as_str).map(str::to_string))
            .or_else(|| spec.meeting.and(file_meeting.map(str::to_string)));
        if meeting.as_deref().is_some_and(|meeting| !safe_id(meeting)) {
            continue;
        }
        clock.observe(&record.v);
        let kind = match spec.name { "people" => Some("person"), "groups" => Some("group"), _ => None };
        let redirect_away = kind.is_some_and(|kind| {
            let map = if kind == "person" { &people } else { &groups };
            key.get("id").and_then(Value::as_str).is_some_and(|id| map.contains_key(id))
        });

        match row {
            _ if redirect_away => {}
            Some(mut row) => {
                if spec.name == "meetings" {
                    let remote = row.get("folder_path").and_then(Value::as_str).and_then(|relative| library.to_absolute(relative));
                    let folder = match remote {
                        Some(path) => Some(path.to_string_lossy().into_owned()),
                        // The other device lacks the audio; keep this device's link.
                        None => sqlx::query_scalar::<_, Option<String>>("SELECT folder_path FROM meetings WHERE id = ?")
                            .bind(row.get("id").and_then(Value::as_str)).fetch_optional(&mut *conn).await?.flatten(),
                    };
                    row["folder_path"] = folder.map(Value::String).unwrap_or(Value::Null);
                } else if let Some(meeting) = &meeting {
                    if !exists(&mut *conn, "meetings", meeting).await? {
                        // Its meeting was deleted: this change is moot, not late.
                        if tombstoned(&mut *conn, "meetings", meeting).await? {
                            record_version(&mut *conn, spec.name, &key_text, &record.v, true, Some(meeting)).await?;
                        } else {
                            deferred += 1;
                        }
                        continue;
                    }
                }
                let mut missing = false;
                let mut gone = false;
                for (column, kind) in reference_columns(spec) {
                    if let Some(id) = row.get(*column).and_then(Value::as_str) {
                        let table = if *kind == "person" { "people" } else { "groups" };
                        if !exists(&mut *conn, table, id).await? {
                            missing = true;
                            gone |= tombstoned(&mut *conn, table, id).await?;
                        }
                    }
                }
                if gone {
                    record_version(&mut *conn, spec.name, &key_text, &record.v, true, meeting.as_deref()).await?;
                    continue;
                }
                if missing {
                    deferred += 1;
                    continue;
                }
                if let Some(kind) = kind {
                    // Two devices made the same contact or group: keep the
                    // smaller id everywhere. Every device reaches this rule.
                    let remote_id = row.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                    let same_name: Option<String> = sqlx::query_scalar(&format!(
                        "SELECT id FROM \"{}\" WHERE normalized_name = ? AND id <> ?", spec.name
                    )).bind(row.get("normalized_name").and_then(Value::as_str)).bind(&remote_id)
                        .fetch_optional(&mut *conn).await?;
                    if let Some(local_id) = same_name {
                        let local_row = tables::row_json(&mut *conn, spec, &serde_json::json!({ "id": local_id })).await?.unwrap_or(Value::Null);
                        let (keep, drop, kept_row, dropped_row) = if remote_id < local_id {
                            (remote_id.clone(), local_id.clone(), row.clone(), local_row)
                        } else {
                            (local_id.clone(), remote_id.clone(), local_row, row.clone())
                        };
                        // Published like a local edit, so a device that never
                        // saw the dropped id can still follow old references.
                        sqlx::query("INSERT INTO sync_redirects (kind, from_id, to_id, created_at) VALUES (?, ?, ?, datetime('now')) \
                                     ON CONFLICT(kind, from_id) DO NOTHING")
                            .bind(kind).bind(&drop).bind(&keep).execute(&mut *conn).await?;
                        let redirects = tables::spec("sync_redirects").expect("sync_redirects spec");
                        let redirect_key = key_string(redirects, &serde_json::json!({ "kind": kind, "from_id": drop }));
                        record_version(&mut *conn, "sync_redirects", &redirect_key, &clock.tick(now_ms()), false, None).await?;
                        if kind == "person" { people.insert(drop.clone(), keep.clone()); } else { groups.insert(drop.clone(), keep.clone()); }
                        // Re-point and remove the dropped row wherever it exists
                        // here (the local one, or a stale copy of the remote one).
                        apply_redirect(&mut *conn, kind, &drop, &keep).await?;
                        let merged = merge_details(spec.name, &kept_row, &dropped_row);
                        tables::upsert(&mut *conn, spec, &merged).await?;
                        let kept_key = key_string(spec, &serde_json::json!({ "id": keep }));
                        record_version(&mut *conn, spec.name, &kept_key, &clock.tick(now_ms()), false, None).await?;
                        if drop == remote_id {
                            record_version(&mut *conn, spec.name, &key_text, &record.v, true, None).await?;
                        }
                        set_state(&mut *conn, "dirty:library", "1").await?;
                        summary.library = true;
                        continue;
                    } else {
                        tables::upsert(&mut *conn, spec, &row).await?;
                    }
                } else {
                    tables::upsert(&mut *conn, spec, &row).await?;
                }
                if spec.name == "sync_redirects" {
                    let kind = row.get("kind").and_then(Value::as_str).unwrap_or_default().to_string();
                    let from = row.get("from_id").and_then(Value::as_str).unwrap_or_default().to_string();
                    let map = if kind == "person" { &mut people } else { &mut groups };
                    map.insert(from.clone(), row.get("to_id").and_then(Value::as_str).unwrap_or_default().to_string());
                    let to = resolve(map, &from).to_string();
                    apply_redirect(&mut *conn, &kind, &from, &to).await?;
                }
            }
            None => {
                if spec.name == "meetings" {
                    if let Some(id) = key.get("id").and_then(Value::as_str) {
                        // This device's own file for the meeting is now stale.
                        set_state(&mut *conn, &format!("dirty:meeting:{id}"), "1").await?;
                        // Children first; foreign-key cascades may be off.
                        for child in TABLES.iter().filter(|child| child.meeting.is_some() && child.name != "meetings") {
                            let column = child.meeting.unwrap_or("meeting_id");
                            sqlx::query(&format!("DELETE FROM \"{}\" WHERE \"{column}\" = ?", child.name))
                                .bind(id).execute(&mut *conn).await?;
                        }
                    }
                }
                tables::delete(&mut *conn, spec, &key).await?;
            }
        }
        record_version(&mut *conn, spec.name, &key_text, &record.v, record.row.is_none(), meeting.as_deref()).await?;
        match &meeting {
            Some(meeting) => { summary.meetings.insert(meeting.clone()); }
            None => summary.library = true,
        }
    }
    Ok(deferred)
}

/// Publishes this device's voices when they differ from the published copy.
pub fn export_voices(library: &Library, device: &str) -> Result<bool> {
    let voices = crate::diarization::voice_profiles::export_voices()?;
    let path = library.voices_file(device);
    let bytes = serde_json::to_vec(&voices)?;
    if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(false);
    }
    write_json_atomic(&path, &voices)?;
    Ok(true)
}

/// Merges other devices' voices into this device's. Returns whether any
/// voice here changed.
pub async fn import_voices(pool: &SqlitePool, library: &Library, device: &str) -> Result<bool> {
    let mut conn = pool.acquire().await?;
    let people = redirect_map(&mut conn, "person").await?;
    let mut changed = false;
    for other in library.other_devices(device) {
        let path = library.voices_file(&other);
        let Some(print) = fingerprint(&path) else { continue };
        let seen = format!("seen:{other}/voices.json");
        if get_state(&mut conn, &seen).await?.as_deref() == Some(print.as_str()) {
            continue;
        }
        let Some(file) = read_json::<crate::diarization::voice_profiles::VoicesFile>(&path) else { continue };
        let known: std::collections::HashSet<String> = sqlx::query_scalar("SELECT id FROM people")
            .fetch_all(&mut *conn).await?.into_iter().collect();
        let (file, held) = hold_unknown_voices(file, &people, &known);
        changed |= crate::diarization::voice_profiles::import_voices(&file, &people)?;
        // Read again once the held contacts have arrived.
        if held == 0 {
            set_state(&mut conn, &seen, &print).await?;
        }
    }
    Ok(changed)
}

/// Voices whose contact has not arrived yet are left out (and counted), so
/// they are not saved for a contact this device would then forget.
pub fn hold_unknown_voices(
    mut file: crate::diarization::voice_profiles::VoicesFile,
    redirects: &HashMap<String, String>,
    known: &std::collections::HashSet<String>,
) -> (crate::diarization::voice_profiles::VoicesFile, usize) {
    let before = file.profiles.len();
    file.profiles.retain(|profile| known.contains(resolve(redirects, &profile.person_id)));
    let held = before - file.profiles.len();
    (file, held)
}

/// Marks every meeting file and the library file for rewriting, for a
/// library folder this device has not published to yet.
pub async fn mark_all_dirty(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(
        "INSERT OR REPLACE INTO sync_state (key, value) SELECT 'dirty:meeting:' || id, '1' FROM meetings; \
         INSERT OR REPLACE INTO sync_state (key, value) SELECT DISTINCT 'dirty:meeting:' || meeting_id, '1' FROM sync_versions WHERE meeting_id IS NOT NULL; \
         INSERT OR REPLACE INTO sync_state (key, value) VALUES ('dirty:library', '1');",
    ).execute(pool).await?;
    Ok(())
}

/// Follows redirects to the id that is kept. Chains end within a few hops;
/// the bound guards against a cycle in damaged data.
pub fn resolve<'a>(redirects: &'a HashMap<String, String>, id: &'a str) -> &'a str {
    let mut current = id;
    for _ in 0..16 {
        match redirects.get(current) {
            Some(next) if next != current => current = next,
            _ => break,
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::migrated_pool;
    use std::path::PathBuf;

    struct Device {
        id: &'static str,
        pool: SqlitePool,
        clock: Hlc,
    }

    impl Device {
        async fn new(id: &'static str) -> Device {
            let pool = migrated_pool().await;
            tables::install_triggers(&pool).await.unwrap();
            tables::set_capture(&mut pool.acquire().await.unwrap(), true).await.unwrap();
            Device { id, pool, clock: Hlc::new(id, None) }
        }

        async fn export(&mut self, library: &Library) -> ExportSummary {
            export(&self.pool, library, self.id, &mut self.clock).await.unwrap()
        }

        async fn import(&mut self, library: &Library) -> ImportSummary {
            import(&self.pool, library, self.id, &mut self.clock).await.unwrap()
        }

        async fn sync(&mut self, library: &Library) -> ImportSummary {
            self.export(library).await;
            self.import(library).await
        }

        async fn sql(&self, sql: &str) {
            sqlx::raw_sql(sql).execute(&self.pool).await.unwrap();
        }

        async fn text(&self, sql: &str) -> Vec<String> {
            sqlx::query_scalar(sql).fetch_all(&self.pool).await.unwrap()
        }

        /// Every synced row, for comparing two devices.
        async fn synced_rows(&self) -> Vec<String> {
            let mut conn = self.pool.acquire().await.unwrap();
            let mut out = Vec::new();
            for spec in TABLES {
                for (_, row) in tables::all_rows(&mut conn, spec).await.unwrap() {
                    let mut row = row;
                    // Recording paths are machine-local by design.
                    if spec.name == "meetings" { row["folder_path"] = Value::Null; }
                    out.push(format!("{}:{}", spec.name, row));
                }
            }
            out.sort();
            out
        }
    }

    fn temp_library(name: &str) -> (Library, PathBuf) {
        let root = std::env::temp_dir().join(format!("meetily-sync-engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        (Library::create_or_open(&root).unwrap(), root)
    }

    const MEETING: &str = "INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1', 'Standup', 'c', 'u'); \
        INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker) VALUES ('t1', 'm1', 'hello', 'ts', 'Speaker 1'), \
            ('t2', 'm1', 'hi', 'ts', 'Speaker 2');";

    fn meeting_file(library: &Library, device: &str, meeting: &str) -> RecordFile {
        read_json(&library.meeting_file(device, meeting)).unwrap()
    }

    #[tokio::test]
    async fn export_writes_meeting_file_with_versions() {
        let (library, root) = temp_library("export");
        let mut a = Device::new("a").await;
        a.sql(MEETING).await;
        let summary = a.export(&library).await;
        assert_eq!(summary, ExportSummary { meetings: 1, library: false });
        let file = meeting_file(&library, "a", "m1");
        assert_eq!(file.records.len(), 3);
        assert!(file.records.iter().all(|record| record.v.ends_with("-a") && record.row.is_some()));
        assert!(a.text("SELECT CAST(COUNT(*) AS TEXT) FROM sync_outbox").await == vec!["0".to_string()]);
        // Nothing changed: nothing to export.
        assert_eq!(a.export(&library).await, ExportSummary::default());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn deleted_meeting_exports_tombstone() {
        let (library, root) = temp_library("tombstone");
        let mut a = Device::new("a").await;
        a.sql(MEETING).await;
        a.export(&library).await;
        a.sql("DELETE FROM transcripts WHERE meeting_id = 'm1'; DELETE FROM meetings WHERE id = 'm1';").await;
        a.export(&library).await;
        let file = meeting_file(&library, "a", "m1");
        assert_eq!(file.records.len(), 3);
        assert!(file.records.iter().all(|record| record.row.is_none()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn folder_path_is_relative_or_null() {
        let (library, root) = temp_library("paths");
        let mut a = Device::new("a").await;
        let inside = library.recordings_dir().join("Meeting 1");
        a.sql(&format!(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES \
             ('m1', 'In', 'c', 'u', '{}'), ('m2', 'Out', 'c', 'u', '/Users/someone/Movies/rec');",
            inside.display()
        )).await;
        a.export(&library).await;
        let folder = |meeting: &str| meeting_file(&library, "a", meeting).records[0].row.as_ref().unwrap()["folder_path"].clone();
        assert_eq!(folder("m1"), Value::String("recordings/Meeting 1".into()));
        assert_eq!(folder("m2"), Value::Null);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn seed_all_publishes_rows_made_before_sync() {
        let (library, root) = temp_library("seed");
        let pool = migrated_pool().await;
        sqlx::raw_sql(MEETING).execute(&pool).await.unwrap();
        tables::install_triggers(&pool).await.unwrap();
        tables::set_capture(&mut pool.acquire().await.unwrap(), true).await.unwrap();
        seed_all(&pool).await.unwrap();
        let mut clock = Hlc::new("a", None);
        export(&pool, &library, "a", &mut clock).await.unwrap();
        assert_eq!(meeting_file(&library, "a", "m1").records.len(), 3);
        // Seeding again adds nothing.
        seed_all(&pool).await.unwrap();
        assert_eq!(export(&pool, &library, "a", &mut clock).await.unwrap(), ExportSummary::default());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn two_devices_converge() {
        let (library, root) = temp_library("converge");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql(MEETING).await;
        a.sync(&library).await;
        let imported = b.sync(&library).await;
        assert_eq!(imported.meetings.into_iter().collect::<Vec<_>>(), vec!["m1".to_string()]);
        // B names a speaker, A writes notes and records a second meeting.
        crate::database::repositories::person::PeopleRepository::rename_meeting_speaker(&b.pool, "m1", "Speaker 1", "Alice").await.unwrap();
        a.sql("INSERT INTO meeting_notes (meeting_id, notes_markdown, created_at, updated_at) VALUES ('m1', 'notes', 'c', 'u'); \
               INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m2', 'Retro', 'c', 'u');").await;
        a.sync(&library).await;
        b.sync(&library).await;
        a.sync(&library).await;
        assert_eq!(a.synced_rows().await, b.synced_rows().await);
        assert_eq!(a.text("SELECT speaker FROM transcripts WHERE id = 't1'").await, vec!["Alice".to_string()]);
        assert_eq!(a.text("SELECT display_name FROM people").await, vec!["Alice".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn calendar_links_travel_and_follow_meeting_deletes() {
        let (library, root) = temp_library("calendar-link");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql(MEETING).await;
        a.sql("INSERT INTO meeting_calendar_events (meeting_id, event_id, title, start_ms, end_ms, attendees, linked_at) \
               VALUES ('m1', 'e1', 'Standup', 1, 2, '[{\"name\":\"Jane Doe\",\"email\":\"jane@a.com\",\"organizer\":false}]', 'l');").await;
        a.sync(&library).await;
        b.sync(&library).await;
        assert_eq!(b.text("SELECT event_id FROM meeting_calendar_events").await, vec!["e1".to_string()]);
        assert_eq!(a.synced_rows().await, b.synced_rows().await);
        assert!(crate::database::repositories::meeting::MeetingsRepository::delete_meeting(&b.pool, "m1").await.unwrap());
        b.sync(&library).await;
        a.sync(&library).await;
        assert!(a.text("SELECT event_id FROM meeting_calendar_events").await.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn import_does_not_echo() {
        let (library, root) = temp_library("echo");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql(MEETING).await;
        a.sync(&library).await;
        b.import(&library).await;
        assert_eq!(b.text("SELECT CAST(COUNT(*) AS TEXT) FROM sync_outbox").await, vec!["0".to_string()]);
        // Reading the same unchanged file again does nothing.
        assert_eq!(b.import(&library).await, ImportSummary::default());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tombstone_beats_older_edit() {
        let (library, root) = temp_library("delete-wins");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql(MEETING).await;
        a.sync(&library).await;
        b.sync(&library).await;
        b.sql("UPDATE meetings SET title = 'Renamed on B' WHERE id = 'm1';").await;
        b.sync(&library).await;
        a.import(&library).await;
        a.sql("DELETE FROM transcripts WHERE meeting_id = 'm1'; DELETE FROM meetings WHERE id = 'm1';").await;
        a.sync(&library).await;
        b.sync(&library).await;
        a.sync(&library).await;
        assert!(a.text("SELECT id FROM meetings").await.is_empty());
        assert!(b.text("SELECT id FROM meetings").await.is_empty());
        assert!(b.text("SELECT id FROM transcripts").await.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn newer_local_edit_survives_older_remote() {
        let (library, root) = temp_library("newer-local");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql(MEETING).await;
        a.sync(&library).await;
        b.sync(&library).await;
        a.sql("UPDATE meetings SET title = 'A title' WHERE id = 'm1';").await;
        a.export(&library).await;
        // B edits after it has seen A's edit version, so B's is newer.
        b.clock.observe(&a.clock.last());
        b.sql("UPDATE meetings SET title = 'B title' WHERE id = 'm1';").await;
        b.export(&library).await;
        b.import(&library).await;
        a.import(&library).await;
        assert_eq!(b.text("SELECT title FROM meetings").await, vec!["B title".to_string()]);
        assert_eq!(a.text("SELECT title FROM meetings").await, vec!["B title".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn child_before_parent_is_retried() {
        let (library, root) = temp_library("out-of-order");
        let mut a = Device::new("a").await;
        let key = serde_json::json!({"id": "t9"});
        let transcript = Record {
            t: "transcripts".into(),
            k: key,
            v: "0000000001000-000000-b".into(),
            row: Some(serde_json::json!({"id": "t9", "meeting_id": "m9", "transcript": "x", "timestamp": "ts"})),
        };
        let file = library.meeting_file("b", "m9");
        write_json_atomic(&file, &RecordFile { format: FORMAT, records: vec![transcript.clone()] }).unwrap();
        let first = a.import(&library).await;
        assert_eq!(first.deferred, 1);
        assert!(a.text("SELECT id FROM transcripts").await.is_empty());
        let meeting = Record {
            t: "meetings".into(),
            k: serde_json::json!({"id": "m9"}),
            v: "0000000001000-000001-b".into(),
            row: Some(serde_json::json!({"id": "m9", "title": "Late", "created_at": "c", "updated_at": "u"})),
        };
        write_json_atomic(&file, &RecordFile { format: FORMAT, records: vec![transcript, meeting] }).unwrap();
        let second = a.import(&library).await;
        assert_eq!(second.deferred, 0);
        assert_eq!(a.text("SELECT id FROM transcripts").await, vec!["t9".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn imported_null_folder_keeps_local_recording() {
        let (library, root) = temp_library("keep-folder");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql("INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES ('m1', 'T', 'c', 'u', '/local/rec');").await;
        a.sync(&library).await;
        b.sync(&library).await;
        b.sql("UPDATE meetings SET title = 'Edited on B' WHERE id = 'm1';").await;
        b.sync(&library).await;
        a.import(&library).await;
        assert_eq!(a.text("SELECT title || '|' || folder_path FROM meetings").await, vec!["Edited on B|/local/rec".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn same_name_contacts_join() {
        let (library, root) = temp_library("same-name");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('ma', 'A', 'c', 'u'); \
               INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker) VALUES ('ta', 'ma', 'x', 'ts', 'David Gurley'); \
               INSERT INTO people (id, display_name, normalized_name, created_at, updated_at) VALUES ('person-b-zzz', 'David Gurley', 'david gurley', 'c', 'u'); \
               INSERT INTO person_speakers VALUES ('person-b-zzz', 'ma', 'David Gurley');").await;
        b.sql("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('mb', 'B', 'c', 'u'); \
               INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker) VALUES ('tb', 'mb', 'y', 'ts', 'David Gurley'); \
               INSERT INTO people (id, display_name, normalized_name, created_at, updated_at) VALUES ('person-a-aaa', 'David Gurley', 'david gurley', 'c', 'u'); \
               INSERT INTO person_speakers VALUES ('person-a-aaa', 'mb', 'David Gurley');").await;
        a.sync(&library).await;
        b.sync(&library).await;
        a.sync(&library).await;
        b.sync(&library).await;
        for device in [&a, &b] {
            assert_eq!(device.text("SELECT id FROM people").await, vec!["person-a-aaa".to_string()]);
            assert_eq!(
                device.text("SELECT meeting_id || ':' || person_id FROM person_speakers ORDER BY meeting_id").await,
                vec!["ma:person-a-aaa".to_string(), "mb:person-a-aaa".to_string()]
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn merge_on_one_device_keeps_concurrent_tag_on_other() {
        let (library, root) = temp_library("merge");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1', 'One', 'c', 'u'), ('m2', 'Two', 'c', 'u'); \
               INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker) VALUES ('t1', 'm1', 'x', 'ts', 'Tom'), \
                   ('t2', 'm2', 'y', 'ts', 'Speaker 1'), ('t3', 'm1', 'z', 'ts', 'Thomas Becker'); \
               INSERT INTO people (id, display_name, normalized_name, created_at, updated_at) VALUES \
                   ('p-tom', 'Tom', 'tom', 'c', 'u'), ('p-thomas', 'Thomas Becker', 'thomas becker', 'c', 'u'); \
               INSERT INTO person_speakers VALUES ('p-tom', 'm1', 'Tom'), ('p-thomas', 'm1', 'Thomas Becker');").await;
        a.sync(&library).await;
        b.sync(&library).await;
        // Offline from each other: A merges Tom into Thomas, B tags Tom in m2.
        crate::database::repositories::person::PeopleRepository::merge_people(&a.pool, "p-tom", "p-thomas").await.unwrap();
        crate::database::repositories::person::PeopleRepository::rename_meeting_speaker(&b.pool, "m2", "Speaker 1", "Tom").await.unwrap();
        b.sync(&library).await;
        a.sync(&library).await;
        b.sync(&library).await;
        for device in [&a, &b] {
            assert_eq!(device.text("SELECT id FROM people").await, vec!["p-thomas".to_string()]);
            assert_eq!(
                device.text("SELECT person_id FROM person_speakers WHERE meeting_id = 'm2'").await,
                vec!["p-thomas".to_string()]
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    async fn converge(devices: &mut [&mut Device], library: &Library) {
        for _ in 0..3 {
            for device in devices.iter_mut() {
                device.sync(library).await;
            }
        }
    }

    #[tokio::test]
    async fn same_name_join_keeps_details_and_converges() {
        let (library, root) = temp_library("join-details");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql("INSERT INTO people (id, display_name, normalized_name, notes, email, created_at, updated_at) VALUES \
               ('person-b-zzz', 'David Gurley', 'david gurley', 'Met at kickoff', 'dg@example.test', 'c', 'u');").await;
        b.sql("INSERT INTO people (id, display_name, normalized_name, notes, company, created_at, updated_at) VALUES \
               ('person-a-aaa', 'David Gurley', 'david gurley', NULL, 'Acme', 'c', 'u');").await;
        converge(&mut [&mut a, &mut b], &library).await;
        for device in [&a, &b] {
            assert_eq!(
                device.text("SELECT id || '|' || COALESCE(notes, '') || '|' || COALESCE(email, '') || '|' || COALESCE(company, '') FROM people").await,
                vec!["person-a-aaa|Met at kickoff|dg@example.test|Acme".to_string()]
            );
        }
        assert_eq!(a.synced_rows().await, b.synced_rows().await);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rename_into_an_existing_name_converges() {
        let (library, root) = temp_library("rename-join");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1', 'One', 'c', 'u'); \
               INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker) VALUES ('t1', 'm1', 'x', 'ts', 'Dave'); \
               INSERT INTO people (id, display_name, normalized_name, created_at, updated_at) VALUES ('p-zzz', 'Dave', 'dave', 'c', 'u'); \
               INSERT INTO person_speakers VALUES ('p-zzz', 'm1', 'Dave');").await;
        converge(&mut [&mut a, &mut b], &library).await;
        b.sql("INSERT INTO people (id, display_name, normalized_name, created_at, updated_at) VALUES ('p-aaa', 'David Gurley', 'david gurley', 'c', 'u');").await;
        a.sql("UPDATE people SET display_name = 'David Gurley', normalized_name = 'david gurley' WHERE id = 'p-zzz';").await;
        converge(&mut [&mut a, &mut b], &library).await;
        for device in [&a, &b] {
            assert_eq!(device.text("SELECT id FROM people").await, vec!["p-aaa".to_string()]);
            assert_eq!(device.text("SELECT person_id FROM person_speakers").await, vec!["p-aaa".to_string()]);
        }
        assert_eq!(a.synced_rows().await, b.synced_rows().await);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_new_device_follows_join_redirects() {
        let (library, root) = temp_library("third-device");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('ma', 'A', 'c', 'u'); \
               INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker) VALUES ('ta', 'ma', 'x', 'ts', 'David Gurley'); \
               INSERT INTO people (id, display_name, normalized_name, created_at, updated_at) VALUES ('person-b-zzz', 'David Gurley', 'david gurley', 'c', 'u'); \
               INSERT INTO person_speakers VALUES ('person-b-zzz', 'ma', 'David Gurley');").await;
        b.sql("INSERT INTO people (id, display_name, normalized_name, created_at, updated_at) VALUES ('person-a-aaa', 'David Gurley', 'david gurley', 'c', 'u');").await;
        converge(&mut [&mut a, &mut b], &library).await;
        // Any contact edit rewrites A's library file, which no longer lists
        // the dropped id; only a published redirect can explain A's old tag.
        a.sql("UPDATE people SET notes = 'edited' WHERE id = 'person-a-aaa';").await;
        converge(&mut [&mut a, &mut b], &library).await;
        let mut c = Device::new("c").await;
        let mut deferred = 0;
        for _ in 0..3 {
            deferred = c.sync(&library).await.deferred;
        }
        assert_eq!(deferred, 0);
        assert_eq!(c.text("SELECT person_id FROM person_speakers").await, vec!["person-a-aaa".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn children_of_a_deleted_meeting_are_not_retried_forever() {
        let (library, root) = temp_library("deleted-children");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql(MEETING).await;
        converge(&mut [&mut a, &mut b], &library).await;
        crate::database::repositories::person::PeopleRepository::rename_meeting_speaker(&b.pool, "m1", "Speaker 1", "Alice").await.unwrap();
        b.export(&library).await;
        a.sql("DELETE FROM transcripts WHERE meeting_id = 'm1'; DELETE FROM meetings WHERE id = 'm1';").await;
        a.clock.observe(&b.clock.last());
        a.export(&library).await;
        let mut last = ImportSummary::default();
        for _ in 0..3 {
            last = a.sync(&library).await;
            b.sync(&library).await;
        }
        assert_eq!(last.deferred, 0);
        assert!(a.text("SELECT id FROM meetings").await.is_empty());
        assert!(b.text("SELECT id FROM meetings").await.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn unexported_local_edit_is_not_overwritten_by_older_remote() {
        let (library, root) = temp_library("pending-local");
        let mut a = Device::new("a").await;
        let mut b = Device::new("b").await;
        a.sql(MEETING).await;
        converge(&mut [&mut a, &mut b], &library).await;
        a.sql("UPDATE meetings SET title = 'A edit' WHERE id = 'm1';").await;
        a.export(&library).await;
        // B edits later but imports before its next export.
        b.sql("UPDATE meetings SET title = 'B edit' WHERE id = 'm1';").await;
        b.import(&library).await;
        assert_eq!(b.text("SELECT title FROM meetings").await, vec!["B edit".to_string()]);
        converge(&mut [&mut b, &mut a], &library).await;
        assert_eq!(a.text("SELECT title FROM meetings").await, vec!["B edit".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn unsafe_meeting_ids_from_another_device_are_refused() {
        let (library, root) = temp_library("unsafe-ids");
        let mut a = Device::new("a").await;
        let evil = Record {
            t: "meetings".into(),
            k: serde_json::json!({"id": "../../escape"}),
            v: "0000000001000-000000-b".into(),
            row: Some(serde_json::json!({"id": "../../escape", "title": "x", "created_at": "c", "updated_at": "u"})),
        };
        write_json_atomic(&library.meeting_file("b", "m1"), &RecordFile { format: FORMAT, records: vec![evil] }).unwrap();
        a.import(&library).await;
        assert!(a.text("SELECT id FROM meetings").await.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn one_bad_file_does_not_stop_the_others() {
        let (library, root) = temp_library("bad-file");
        let mut a = Device::new("a").await;
        // A meetings row without its required title fails to insert.
        let broken = Record {
            t: "meetings".into(),
            k: serde_json::json!({"id": "m-bad"}),
            v: "0000000001000-000000-b".into(),
            row: Some(serde_json::json!({"id": "m-bad", "title": null, "created_at": "c", "updated_at": "u"})),
        };
        let good = Record {
            t: "meetings".into(),
            k: serde_json::json!({"id": "m-good"}),
            v: "0000000001000-000001-b".into(),
            row: Some(serde_json::json!({"id": "m-good", "title": "Fine", "created_at": "c", "updated_at": "u"})),
        };
        write_json_atomic(&library.meeting_file("b", "m-bad"), &RecordFile { format: FORMAT, records: vec![broken] }).unwrap();
        write_json_atomic(&library.meeting_file("b", "m-good"), &RecordFile { format: FORMAT, records: vec![good] }).unwrap();
        a.import(&library).await;
        assert_eq!(a.text("SELECT id FROM meetings").await, vec!["m-good".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn voices_for_contacts_not_here_yet_are_held_back() {
        use crate::diarization::voice_profiles::{VoiceProfile, VoicesFile};
        let profile = |id: &str| VoiceProfile { person_id: id.into(), name: id.into(), embedding: vec![0.0; 128], samples: 2, model: String::new(), sources: Vec::new() };
        let file = VoicesFile { profiles: vec![profile("p-known"), profile("p-dup"), profile("p-later")], tombstones: Default::default() };
        let known: std::collections::HashSet<String> = ["p-known".to_string(), "p-kept".to_string()].into_iter().collect();
        let redirects = HashMap::from([("p-dup".to_string(), "p-kept".to_string())]);
        let (kept, held) = hold_unknown_voices(file, &redirects, &known);
        assert_eq!(held, 1);
        assert_eq!(kept.profiles.iter().map(|profile| profile.person_id.as_str()).collect::<Vec<_>>(), vec!["p-known", "p-dup"]);
    }

    #[test]
    fn redirect_chains_resolve() {
        let redirects = HashMap::from([
            ("p3".to_string(), "p2".to_string()),
            ("p2".to_string(), "p1".to_string()),
            ("loop".to_string(), "loop".to_string()),
        ]);
        assert_eq!(resolve(&redirects, "p3"), "p1");
        assert_eq!(resolve(&redirects, "p1"), "p1");
        assert_eq!(resolve(&redirects, "loop"), "loop");
    }

    #[test]
    fn key_string_matches_trigger_spelling() {
        let spec = tables::spec("sync_redirects").unwrap();
        let key = serde_json::json!({"from_id": "p2", "kind": "person"});
        assert_eq!(key_string(spec, &key), r#"{"kind":"person","from_id":"p2"}"#);
    }
}
