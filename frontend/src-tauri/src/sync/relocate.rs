//! Moves finished recordings into the shared library so other devices get the
//! audio. Recording itself stays local: a cloud client must never upload a file
//! that is still being written, so a folder moves only once it has settled.
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::files::Library;

/// No file in a folder may have changed for this long before it moves.
pub const SETTLE: Duration = Duration::from_secs(120);

fn newest_change(folder: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(folder).ok()?.modified().ok()?;
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let metadata = entry.metadata().ok()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            }
            if let Ok(modified) = metadata.modified() {
                newest = newest.max(modified);
            }
        }
    }
    Some(newest)
}

/// The recording is finished (`metadata.json` status `completed`) and nothing
/// in its folder changed for `SETTLE`.
pub fn is_settled(folder: &Path, now: SystemTime) -> bool {
    let completed = std::fs::read(folder.join("metadata.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|metadata| metadata.get("status").and_then(|status| status.as_str()) == Some("completed"));
    completed
        && newest_change(folder)
            .and_then(|changed| now.duration_since(changed).ok())
            .is_some_and(|age| age >= SETTLE)
}

/// A free destination under `recordings/`: the folder's own name, or the name
/// with " (2)", " (3)" … when another device already used it.
fn destination(library: &Library, folder: &Path) -> Option<PathBuf> {
    let name = folder.file_name()?.to_str()?.to_string();
    let recordings = library.recordings_dir();
    (1..1000)
        .map(|attempt| if attempt == 1 { recordings.join(&name) } else { recordings.join(format!("{name} ({attempt})")) })
        .find(|candidate| !candidate.exists())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            let copied = std::fs::copy(entry.path(), &target)?;
            if copied != entry.metadata()?.len() {
                return Err(std::io::Error::other(format!("Short copy of {}", entry.path().display())));
            }
        }
    }
    Ok(())
}

/// Rename when the library is on the same volume; otherwise copy, check
/// sizes, and only then remove the original.
pub fn move_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if let Err(error) = copy_dir(from, to) {
        let _ = std::fs::remove_dir_all(to);
        return Err(error);
    }
    std::fs::remove_dir_all(from)
}

/// Moves every settled recording that is outside the library into it and
/// points its meeting there. Returns the meetings moved. Does nothing while
/// recording, retranscribing, or identifying speakers, since those write into
/// recording folders.
pub async fn relocate_settled(pool: &SqlitePool, library: &Library, now: SystemTime) -> Vec<String> {
    if crate::audio::recording_commands::is_recording_active()
        || crate::audio::retranscription::is_retranscription_in_progress()
    {
        return Vec::new();
    }
    let Ok(_guard) = crate::diarization::try_operation_guard() else { return Vec::new() };
    relocate_unguarded(pool, library, now).await
}

async fn relocate_unguarded(pool: &SqlitePool, library: &Library, now: SystemTime) -> Vec<String> {
    let meetings: Vec<(String, String)> =
        sqlx::query_as("SELECT id, folder_path FROM meetings WHERE folder_path IS NOT NULL")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let mut moved = Vec::new();
    for (meeting, folder) in meetings {
        let folder = PathBuf::from(folder);
        if library.to_relative(&folder).is_some() || !folder.is_dir() || !is_settled(&folder, now) {
            continue;
        }
        let Some(target) = destination(library, &folder) else { continue };
        if let Err(error) = move_folder(&folder, &target) {
            log::warn!("Could not move {} into the library: {error}", folder.display());
            continue;
        }
        let updated = sqlx::query("UPDATE meetings SET folder_path = ? WHERE id = ?")
            .bind(target.to_string_lossy().as_ref())
            .bind(&meeting)
            .execute(pool)
            .await;
        match updated {
            Ok(_) => moved.push(meeting),
            Err(error) => {
                // Put it back so the meeting still finds its audio.
                log::warn!("Could not record the moved recording for {meeting}: {error}");
                let _ = move_folder(&target, &folder);
            }
        }
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::migrated_pool;

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("meetily-sync-relocate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn recording(parent: &Path, name: &str, status: &str) -> PathBuf {
        let folder = parent.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("metadata.json"), format!(r#"{{"status":"{status}"}}"#)).unwrap();
        std::fs::write(folder.join("audio.mp4"), b"audio").unwrap();
        folder
    }

    fn later() -> SystemTime {
        SystemTime::now() + SETTLE + Duration::from_secs(5)
    }

    #[test]
    fn unsettled_recording_is_not_moved() {
        let root = temp("unsettled");
        let fresh = recording(&root, "fresh", "completed");
        assert!(!is_settled(&fresh, SystemTime::now()));
        assert!(is_settled(&fresh, later()));
        let unfinished = recording(&root, "unfinished", "recording");
        assert!(!is_settled(&unfinished, later()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn settled_recording_moves_and_updates_path() {
        let root = temp("moves");
        let library = Library::create_or_open(&root.join("library")).unwrap();
        let folder = recording(&root, "Meeting 1", "completed");
        let pool = migrated_pool().await;
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES ('m1', 'T', 'c', 'u', ?)")
            .bind(folder.to_string_lossy().as_ref()).execute(&pool).await.unwrap();
        assert_eq!(relocate_unguarded(&pool, &library, later()).await, vec!["m1".to_string()]);
        let moved = library.recordings_dir().join("Meeting 1");
        assert!(moved.join("audio.mp4").is_file());
        assert!(!folder.exists());
        let path: String = sqlx::query_scalar("SELECT folder_path FROM meetings").fetch_one(&pool).await.unwrap();
        assert_eq!(PathBuf::from(path), moved);
        // Already inside: left alone.
        assert!(relocate_unguarded(&pool, &library, later()).await.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn name_clash_gets_suffix() {
        let root = temp("clash");
        let library = Library::create_or_open(&root.join("library")).unwrap();
        std::fs::create_dir_all(library.recordings_dir().join("Meeting 1")).unwrap();
        let folder = recording(&root, "Meeting 1", "completed");
        assert_eq!(destination(&library, &folder).unwrap(), library.recordings_dir().join("Meeting 1 (2)"));
        let _ = std::fs::remove_dir_all(root);
    }
}
