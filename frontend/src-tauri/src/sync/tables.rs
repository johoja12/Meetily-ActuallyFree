//! The synced tables, their capture triggers, and generic row <-> JSON.
//!
//! Rows travel as JSON objects keyed by column name. Columns are read from the
//! live schema, so a column one device has and another lacks is simply left
//! out on the device that lacks it.
use serde_json::{Map, Value};
use sqlx::{Row, SqliteConnection, SqlitePool};

pub struct TableSpec {
    pub name: &'static str,
    /// Columns that identify a row across devices.
    pub key: &'static [&'static str],
    /// Column holding the meeting id for meeting-scoped tables; global tables
    /// (contacts, groups, redirects) have none.
    pub meeting: Option<&'static str>,
    /// Other synced rows refer to this table's rows, so an upsert must update
    /// in place: deleting and re-inserting would cascade to them.
    pub parent: bool,
}

pub const TABLES: &[TableSpec] = &[
    TableSpec { name: "meetings", key: &["id"], meeting: Some("id"), parent: true },
    TableSpec { name: "transcripts", key: &["id"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "person_speakers", key: &["meeting_id", "speaker_label"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "meeting_notes", key: &["meeting_id"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "summary_processes", key: &["meeting_id"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "action_items", key: &["id"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "meeting_whisper_vocabulary", key: &["meeting_id"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "transcript_chunks", key: &["meeting_id"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "voice_speaker_matches", key: &["meeting_id", "original_label", "person_id"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "meeting_calendar_events", key: &["meeting_id"], meeting: Some("meeting_id"), parent: false },
    TableSpec { name: "people", key: &["id"], meeting: None, parent: true },
    TableSpec { name: "groups", key: &["id"], meeting: None, parent: true },
    TableSpec { name: "sync_redirects", key: &["kind", "from_id"], meeting: None, parent: false },
];

pub fn spec(name: &str) -> Option<&'static TableSpec> {
    TABLES.iter().find(|spec| spec.name == name)
}

fn json_object_sql(prefix: &str, columns: &[&str]) -> String {
    let pairs: Vec<String> = columns.iter().map(|column| format!("'{column}', {prefix}\"{column}\"")).collect();
    format!("json_object({})", pairs.join(", "))
}

fn meeting_sql(spec: &TableSpec, prefix: &str) -> String {
    spec.meeting.map(|column| format!("{prefix}\"{column}\"")).unwrap_or_else(|| "NULL".into())
}

/// Installs the capture triggers. Idempotent; runs at every start.
pub async fn install_triggers(pool: &SqlitePool) -> sqlx::Result<()> {
    for spec in TABLES {
        let t = spec.name;
        let new_key = json_object_sql("NEW.", spec.key);
        let old_key = json_object_sql("OLD.", spec.key);
        let guard = "WHEN (SELECT value FROM sync_state WHERE key = 'capture') = '1'";
        let insert = |key: &str, meeting: &str| {
            format!("INSERT INTO sync_outbox (table_name, row_key, meeting_id) VALUES ('{t}', {key}, {meeting});")
        };
        let statements = [
            format!(
                "CREATE TRIGGER IF NOT EXISTS sync_{t}_insert AFTER INSERT ON \"{t}\" {guard} BEGIN {} END",
                insert(&new_key, &meeting_sql(spec, "NEW."))
            ),
            format!(
                "CREATE TRIGGER IF NOT EXISTS sync_{t}_update AFTER UPDATE ON \"{t}\" {guard} BEGIN {} \
                 INSERT INTO sync_outbox (table_name, row_key, meeting_id) SELECT '{t}', {old_key}, {} WHERE {old_key} <> {new_key}; END",
                insert(&new_key, &meeting_sql(spec, "NEW.")),
                meeting_sql(spec, "OLD.")
            ),
            format!(
                "CREATE TRIGGER IF NOT EXISTS sync_{t}_delete AFTER DELETE ON \"{t}\" {guard} BEGIN {} END",
                insert(&old_key, &meeting_sql(spec, "OLD."))
            ),
        ];
        for statement in statements {
            sqlx::query(&statement).execute(pool).await?;
        }
    }
    Ok(())
}

/// Turns change capture on or off. Inside an import transaction this hides the
/// import's own writes; other connections never see the uncommitted '0'.
pub async fn set_capture(conn: &mut SqliteConnection, on: bool) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO sync_state (key, value) VALUES ('capture', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(if on { "1" } else { "0" })
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn columns(conn: &mut SqliteConnection, table: &str) -> sqlx::Result<Vec<String>> {
    let rows = sqlx::query(&format!("PRAGMA table_info(\"{table}\")")).fetch_all(conn).await?;
    Ok(rows.iter().map(|row| row.get::<String, _>("name")).collect())
}

fn key_where(spec: &TableSpec) -> String {
    spec.key
        .iter()
        .map(|column| format!("\"{column}\" IS json_extract(?1, '$.{column}')"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// The key object of a row.
pub fn key_of(spec: &TableSpec, row: &Value) -> Value {
    let mut key = Map::new();
    for column in spec.key {
        key.insert((*column).to_string(), row.get(*column).cloned().unwrap_or(Value::Null));
    }
    Value::Object(key)
}

fn parse(text: String) -> Value {
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

pub async fn row_json(conn: &mut SqliteConnection, spec: &TableSpec, key: &Value) -> sqlx::Result<Option<Value>> {
    let columns = columns(&mut *conn, spec.name).await?;
    let names: Vec<&str> = columns.iter().map(String::as_str).collect();
    let sql = format!("SELECT {} FROM \"{}\" WHERE {}", json_object_sql("", &names), spec.name, key_where(spec));
    let row: Option<String> = sqlx::query_scalar(&sql).bind(key.to_string()).fetch_optional(conn).await?;
    Ok(row.map(parse))
}

async fn select_rows(conn: &mut SqliteConnection, spec: &TableSpec, filter: &str, bind: Option<&str>) -> sqlx::Result<Vec<(Value, Value)>> {
    let columns = columns(&mut *conn, spec.name).await?;
    let names: Vec<&str> = columns.iter().map(String::as_str).collect();
    let sql = format!(
        "SELECT {}, {} FROM \"{}\" {filter}",
        json_object_sql("", spec.key),
        json_object_sql("", &names),
        spec.name
    );
    let mut query = sqlx::query_as::<_, (String, String)>(&sql);
    if let Some(value) = bind {
        query = query.bind(value.to_string());
    }
    Ok(query.fetch_all(conn).await?.into_iter().map(|(key, row)| (parse(key), parse(row))).collect())
}

pub async fn rows_for_meeting(conn: &mut SqliteConnection, spec: &TableSpec, meeting_id: &str) -> sqlx::Result<Vec<(Value, Value)>> {
    let Some(column) = spec.meeting else { return Ok(Vec::new()) };
    select_rows(conn, spec, &format!("WHERE \"{column}\" = ?1"), Some(meeting_id)).await
}

pub async fn all_rows(conn: &mut SqliteConnection, spec: &TableSpec) -> sqlx::Result<Vec<(Value, Value)>> {
    select_rows(conn, spec, "", None).await
}

/// Writes a row received from another device. Columns this schema lacks are
/// ignored; columns the row lacks keep their defaults.
pub async fn upsert(conn: &mut SqliteConnection, spec: &TableSpec, row: &Value) -> sqlx::Result<()> {
    let Some(object) = row.as_object() else { return Ok(()) };
    let local = columns(&mut *conn, spec.name).await?;
    let present: Vec<&str> = local.iter().map(String::as_str).filter(|column| object.contains_key(*column)).collect();
    if present.is_empty() {
        return Ok(());
    }
    if !spec.parent {
        delete(&mut *conn, spec, &key_of(spec, row)).await?;
    }
    let column_list = present.iter().map(|column| format!("\"{column}\"")).collect::<Vec<_>>().join(", ");
    let values = present.iter().map(|column| format!("json_extract(?1, '$.{column}')")).collect::<Vec<_>>().join(", ");
    let updates: Vec<String> = present
        .iter()
        .filter(|column| !spec.key.contains(column))
        .map(|column| format!("\"{column}\" = excluded.\"{column}\""))
        .collect();
    let conflict = if spec.parent {
        let target = spec.key.iter().map(|column| format!("\"{column}\"")).collect::<Vec<_>>().join(", ");
        if updates.is_empty() {
            format!(" ON CONFLICT({target}) DO NOTHING")
        } else {
            format!(" ON CONFLICT({target}) DO UPDATE SET {}", updates.join(", "))
        }
    } else {
        String::new()
    };
    let sql = format!("INSERT INTO \"{}\" ({column_list}) SELECT {values} WHERE true{conflict}", spec.name);
    sqlx::query(&sql).bind(row.to_string()).execute(conn).await?;
    Ok(())
}

pub async fn delete(conn: &mut SqliteConnection, spec: &TableSpec, key: &Value) -> sqlx::Result<()> {
    let sql = format!("DELETE FROM \"{}\" WHERE {}", spec.name, key_where(spec));
    sqlx::query(&sql).bind(key.to_string()).execute(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::migrated_pool;
    use serde_json::json;

    async fn outbox(pool: &SqlitePool) -> Vec<(String, String)> {
        sqlx::query_as("SELECT table_name, row_key FROM sync_outbox ORDER BY seq").fetch_all(pool).await.unwrap()
    }

    async fn add_meeting(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES (?, 'T', 'c', 'u')")
            .bind(id).execute(pool).await.unwrap();
    }

    #[tokio::test]
    async fn triggers_capture_only_when_on() {
        let pool = migrated_pool().await;
        install_triggers(&pool).await.unwrap();
        add_meeting(&pool, "m0").await;
        assert!(outbox(&pool).await.is_empty());
        set_capture(&mut pool.acquire().await.unwrap(), true).await.unwrap();
        add_meeting(&pool, "m1").await;
        sqlx::query("DELETE FROM meetings WHERE id = 'm1'").execute(&pool).await.unwrap();
        assert_eq!(outbox(&pool).await, vec![
            ("meetings".to_string(), r#"{"id":"m1"}"#.to_string()),
            ("meetings".to_string(), r#"{"id":"m1"}"#.to_string()),
        ]);
    }

    #[tokio::test]
    async fn update_changing_key_records_both_keys() {
        let pool = migrated_pool().await;
        install_triggers(&pool).await.unwrap();
        add_meeting(&pool, "m1").await;
        sqlx::query("INSERT INTO people (id, display_name, normalized_name, created_at, updated_at) VALUES ('p1', 'A', 'a', 'c', 'u')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO person_speakers VALUES ('p1', 'm1', 'Speaker 1')").execute(&pool).await.unwrap();
        set_capture(&mut pool.acquire().await.unwrap(), true).await.unwrap();
        sqlx::query("UPDATE person_speakers SET speaker_label = 'A' WHERE meeting_id = 'm1'").execute(&pool).await.unwrap();
        let keys: Vec<String> = outbox(&pool).await.into_iter().map(|(_, key)| key).collect();
        assert_eq!(keys, vec![
            r#"{"meeting_id":"m1","speaker_label":"A"}"#.to_string(),
            r#"{"meeting_id":"m1","speaker_label":"Speaker 1"}"#.to_string(),
        ]);
    }

    #[tokio::test]
    async fn row_json_round_trips_through_upsert() {
        let pool = migrated_pool().await;
        add_meeting(&pool, "m1").await;
        sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, speaker) VALUES ('t1', 'm1', 'hi', 'ts', 1.5, 'Speaker 2')")
            .execute(&pool).await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let spec = spec("transcripts").unwrap();
        let mut row = row_json(&mut conn, spec, &json!({"id": "t1"})).await.unwrap().unwrap();
        assert_eq!(row["audio_start_time"], json!(1.5));
        row["speaker"] = json!("Alice");
        row["column_from_newer_app"] = json!("ignored");
        upsert(&mut conn, spec, &row).await.unwrap();
        let again = row_json(&mut conn, spec, &json!({"id": "t1"})).await.unwrap().unwrap();
        assert_eq!(again["speaker"], json!("Alice"));
        assert_eq!(again["transcript"], json!("hi"));
        assert_eq!(all_rows(&mut conn, spec).await.unwrap().len(), 1);
        delete(&mut conn, spec, &json!({"id": "t1"})).await.unwrap();
        assert!(row_json(&mut conn, spec, &json!({"id": "t1"})).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upsert_never_cascades_children() {
        let pool = migrated_pool().await;
        add_meeting(&pool, "m1").await;
        sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp) VALUES ('t1', 'm1', 'hi', 'ts')")
            .execute(&pool).await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let meetings = spec("meetings").unwrap();
        let mut row = row_json(&mut conn, meetings, &json!({"id": "m1"})).await.unwrap().unwrap();
        row["title"] = json!("Renamed");
        upsert(&mut conn, meetings, &row).await.unwrap();
        assert_eq!(rows_for_meeting(&mut conn, spec("transcripts").unwrap(), "m1").await.unwrap().len(), 1);
        let title: String = sqlx::query_scalar("SELECT title FROM meetings WHERE id = 'm1'").fetch_one(&mut *conn).await.unwrap();
        assert_eq!(title, "Renamed");
    }
}
