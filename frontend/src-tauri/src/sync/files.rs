//! The shared library folder: layout, atomic writes, tolerant reads, and
//! recording paths relative to its root.
//!
//! A cloud client may hand us a file it is still downloading, so every read
//! tolerates a missing, partial or unparsable file by returning `None`; the
//! caller retries on the next pass. Writes go to `<name>.tmp` and are renamed,
//! so other devices never see a half-written file from us.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub const FORMAT: u32 = 1;

/// One synced row. `row: None` is a tombstone.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub t: String,
    pub k: Value,
    pub v: String,
    pub row: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RecordFile {
    pub format: u32,
    pub records: Vec<Record>,
}

#[derive(Clone, Debug)]
pub struct Library {
    pub root: PathBuf,
}

impl Library {
    /// Opens a library folder, creating its layout when it is empty or new.
    pub fn create_or_open(root: &Path) -> Result<Library> {
        let library = Library { root: root.to_path_buf() };
        std::fs::create_dir_all(library.recordings_dir()).with_context(|| format!("Cannot create {}", root.display()))?;
        std::fs::create_dir_all(root.join("devices"))?;
        let marker = root.join("library.json");
        if !marker.exists() {
            write_json_atomic(&marker, &serde_json::json!({ "format": FORMAT }))?;
        }
        Ok(library)
    }

    pub fn recordings_dir(&self) -> PathBuf {
        self.root.join("recordings")
    }

    pub fn device_dir(&self, device: &str) -> PathBuf {
        self.root.join("devices").join(device)
    }

    pub fn meeting_file(&self, device: &str, meeting_id: &str) -> PathBuf {
        self.device_dir(device).join("meetings").join(format!("{meeting_id}.json"))
    }

    pub fn library_file(&self, device: &str) -> PathBuf {
        self.device_dir(device).join("library.json")
    }

    pub fn voices_file(&self, device: &str) -> PathBuf {
        self.device_dir(device).join("voices.json")
    }

    /// Ids of the other devices that have written to this library.
    pub fn other_devices(&self, own: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.root.join("devices")) else { return Vec::new() };
        let mut devices: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name != own && !name.starts_with('.'))
            .collect();
        devices.sort();
        devices
    }

    /// `recordings/<name>` with `/` separators, or None outside the library.
    pub fn to_relative(&self, path: &Path) -> Option<String> {
        let inside = path.strip_prefix(&self.root).ok().map(Path::to_path_buf).or_else(|| {
            // The same folder can be reached through a symlink or a differently
            // spelled path (cloud clients mount under ~/Library/CloudStorage).
            let root = self.root.canonicalize().ok()?;
            path.canonicalize().ok()?.strip_prefix(root).ok().map(Path::to_path_buf)
        })?;
        let parts: Vec<String> = inside
            .components()
            .map(|component| match component {
                Component::Normal(part) => part.to_str().map(str::to_string),
                _ => None,
            })
            .collect::<Option<_>>()?;
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    /// A relative path from another device, resolved under this root. Paths
    /// that would leave the library are refused.
    pub fn to_absolute(&self, relative: &str) -> Option<PathBuf> {
        let mut path = self.root.clone();
        for part in relative.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') || Path::new(part).is_absolute() {
                return None;
            }
            path.push(part);
        }
        Some(path)
    }
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, serde_json::to_vec(value)?)?;
    std::fs::rename(&temp, path).with_context(|| format!("Cannot write {}", path.display()))?;
    Ok(())
}

/// None for a missing, partial or unparsable file, and for `.tmp` files.
pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    if path.extension().is_some_and(|extension| extension == "tmp") {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("meetily-sync-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn partial_file_is_skipped() {
        let root = temp_root("partial");
        let library = Library::create_or_open(&root).unwrap();
        let file = library.meeting_file("dev-a", "m1");
        write_json_atomic(&file, &RecordFile { format: FORMAT, records: Vec::new() }).unwrap();
        assert!(read_json::<RecordFile>(&file).is_some());
        std::fs::write(&file, br#"{"format":1,"records":[{"t":"meet"#).unwrap();
        assert!(read_json::<RecordFile>(&file).is_none());
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, br#"{"format":1,"records":[]}"#).unwrap();
        assert!(read_json::<RecordFile>(&tmp).is_none());
        assert!(read_json::<RecordFile>(&root.join("missing.json")).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relative_paths_use_forward_slashes_and_stay_inside() {
        let root = temp_root("paths");
        let library = Library::create_or_open(&root).unwrap();
        let folder = library.recordings_dir().join("Meeting · Mon").join("sub");
        assert_eq!(library.to_relative(&folder).as_deref(), Some("recordings/Meeting · Mon/sub"));
        assert!(library.to_relative(Path::new("/elsewhere/rec")).is_none());
        assert_eq!(library.to_absolute("recordings/Meeting · Mon/sub").unwrap(), folder);
        assert!(library.to_absolute("../outside").is_none());
        assert!(library.to_absolute("/etc").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn layout_is_created_and_other_devices_exclude_own() {
        let root = temp_root("devices");
        let library = Library::create_or_open(&root).unwrap();
        assert!(root.join("library.json").is_file());
        assert!(library.recordings_dir().is_dir());
        std::fs::create_dir_all(library.device_dir("dev-a")).unwrap();
        std::fs::create_dir_all(library.device_dir("dev-b")).unwrap();
        std::fs::write(root.join("devices").join("stray.txt"), b"x").unwrap();
        assert_eq!(library.other_devices("dev-a"), vec!["dev-b".to_string()]);
        // Reopening keeps the layout.
        assert!(Library::create_or_open(&root).is_ok());
        let _ = std::fs::remove_dir_all(root);
    }
}
