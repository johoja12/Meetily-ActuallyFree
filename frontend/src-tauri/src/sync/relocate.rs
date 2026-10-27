//! Moves finished recordings into the shared library so other devices get the
//! audio. Recording itself stays local: a cloud client must never upload a file
//! that is still being written, so a folder moves only once it has settled.
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::files::Library;

/// Folders moved during this run, old path to new. A window that loaded a
/// meeting before its recording moved can still hold the old path; commands
/// that take a path from the window follow these instead of failing.
static MOVES: std::sync::Mutex<Vec<(PathBuf, PathBuf)>> = std::sync::Mutex::new(Vec::new());

pub fn record_move(from: &Path, to: &Path) {
    if let Ok(mut moves) = MOVES.lock() {
        moves.retain(|(old, _)| old != from);
        moves.push((from.to_path_buf(), to.to_path_buf()));
        // One folder moves per pass; this stays small for any real session.
        if moves.len() > 1000 {
            moves.remove(0);
        }
    }
}

/// `path`, or where it is now if it was inside a folder this run moved.
pub fn follow_moves(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    let Ok(moves) = MOVES.lock() else { return path.to_path_buf() };
    // Newest first, so a folder moved twice resolves to where it is now.
    for (from, to) in moves.iter().rev() {
        // strip_prefix compares whole components: "Meeting 10" is not inside "Meeting 1".
        if let Ok(rest) = path.strip_prefix(from) {
            return if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) };
        }
    }
    path.to_path_buf()
}

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

/// Puts a copy of `from` at `to`: a rename when the library is on the same
/// volume (returns true), otherwise a size-checked copy that leaves the
/// original in place (returns false) until the meeting points at the copy.
fn place_folder(from: &Path, to: &Path) -> std::io::Result<bool> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(true);
    }
    if let Err(error) = copy_dir(from, to) {
        let _ = std::fs::remove_dir_all(to);
        return Err(error);
    }
    Ok(false)
}

fn inside(root: &Path, folder: &Path) -> bool {
    Library { root: root.to_path_buf() }.to_relative(folder).is_some()
}

/// Moves every settled recording that is outside the library into it and
/// points its meeting there. Returns the meetings moved. Does nothing while
/// recording, retranscribing, or identifying speakers, since those write into
/// recording folders.
pub async fn relocate_settled(pool: &SqlitePool, library: &Library, now: SystemTime, exclude: &[PathBuf]) -> Vec<String> {
    if crate::audio::recording_commands::is_recording_active()
        || crate::audio::retranscription::is_retranscription_in_progress()
    {
        return Vec::new();
    }
    let Ok(_guard) = crate::diarization::try_operation_guard() else { return Vec::new() };
    relocate_unguarded(pool, library, now, exclude).await
}

/// Moves at most one recording per pass, so the guards checked by the caller
/// hold for the whole move and a big first sync does not block other work.
/// Folders inside `exclude` (libraries this device used before) are shared
/// with other devices and are never moved out.
async fn relocate_unguarded(pool: &SqlitePool, library: &Library, now: SystemTime, exclude: &[PathBuf]) -> Vec<String> {
    let meetings: Vec<(String, String)> =
        sqlx::query_as("SELECT id, folder_path FROM meetings WHERE folder_path IS NOT NULL ORDER BY created_at")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    for (meeting, folder) in meetings {
        let folder = PathBuf::from(folder);
        if inside(&library.root, &folder)
            || exclude.iter().any(|root| inside(root, &folder))
            || !folder.is_dir()
            || !is_settled(&folder, now)
        {
            continue;
        }
        let Some(target) = destination(library, &folder) else { continue };
        let (from, to) = (folder.clone(), target.clone());
        let renamed = match tokio::task::spawn_blocking(move || place_folder(&from, &to)).await {
            Ok(Ok(renamed)) => renamed,
            Ok(Err(error)) => {
                log::warn!("Could not move {} into the library: {error}", folder.display());
                return Vec::new();
            }
            Err(error) => {
                log::warn!("Recording move task failed: {error}");
                return Vec::new();
            }
        };
        let updated = sqlx::query("UPDATE meetings SET folder_path = ? WHERE id = ?")
            .bind(target.to_string_lossy().as_ref())
            .bind(&meeting)
            .execute(pool)
            .await;
        if let Err(error) = updated {
            // Undo so the meeting still finds its audio where it was.
            log::warn!("Could not record the moved recording for {meeting}: {error}");
            let _ = if renamed { std::fs::rename(&target, &folder) } else { std::fs::remove_dir_all(&target) };
            return Vec::new();
        }
        record_move(&folder, &target);
        if !renamed {
            // The meeting points at the verified copy; the original is spare.
            let original = folder.clone();
            if let Ok(Err(error)) = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&original)).await {
                log::warn!("Moved {} into the library but could not remove the original: {error}", folder.display());
            }
        }
        return vec![meeting];
    }
    Vec::new()
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
        assert_eq!(relocate_unguarded(&pool, &library, later(), &[]).await, vec!["m1".to_string()]);
        let moved = library.recordings_dir().join("Meeting 1");
        assert!(moved.join("audio.mp4").is_file());
        assert!(!folder.exists());
        let path: String = sqlx::query_scalar("SELECT folder_path FROM meetings").fetch_one(&pool).await.unwrap();
        assert_eq!(PathBuf::from(path), moved);
        // Already inside: left alone.
        assert!(relocate_unguarded(&pool, &library, later(), &[]).await.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn one_folder_moves_per_pass_and_previous_libraries_stay() {
        let root = temp("one-per-pass");
        let library = Library::create_or_open(&root.join("library")).unwrap();
        let old_library = root.join("old-library");
        let first = recording(&root, "First", "completed");
        let second = recording(&root, "Second", "completed");
        let shared = recording(&old_library.join("recordings"), "Shared", "completed");
        let pool = migrated_pool().await;
        for (id, folder) in [("m1", &first), ("m2", &second), ("m3", &shared)] {
            sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES (?, 'T', 'c', 'u', ?)")
                .bind(id).bind(folder.to_string_lossy().as_ref()).execute(&pool).await.unwrap();
        }
        let exclude = vec![old_library.clone()];
        assert_eq!(relocate_unguarded(&pool, &library, later(), &exclude).await.len(), 1);
        assert_eq!(relocate_unguarded(&pool, &library, later(), &exclude).await.len(), 1);
        assert!(relocate_unguarded(&pool, &library, later(), &exclude).await.is_empty());
        // A recording inside a library this device used before is shared with
        // devices still using it, so it is never moved out.
        assert!(shared.join("audio.mp4").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stale_paths_follow_a_move() {
        let root = temp("follow");
        let to = root.join("library/recordings/Meeting 1");
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("audio.mp4"), b"a").unwrap();
        let from = root.join("old/Meeting 1");
        record_move(&from, &to);
        assert_eq!(follow_moves(&from.join("audio.mp4")), to.join("audio.mp4"));
        assert_eq!(follow_moves(&from), to);
        // A sibling whose name only starts the same is not inside the folder.
        let sibling = root.join("old/Meeting 10/audio.mp4");
        assert_eq!(follow_moves(&sibling), sibling);
        // A path that still exists is left alone.
        assert_eq!(follow_moves(&to.join("audio.mp4")), to.join("audio.mp4"));
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
