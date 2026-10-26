//! This device's sync settings, kept in `sync.json` in the app data folder
//! (never in the shared library). The device id names this device's folder in
//! the library and ends every version it writes.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncConfig {
    pub device_id: String,
    pub device_name: String,
    pub library_root: Option<PathBuf>,
    pub enabled: bool,
}

static CACHE: Mutex<Option<SyncConfig>> = Mutex::new(None);

fn path() -> PathBuf {
    crate::paths::install_data_root().join("sync.json")
}

fn default_name() -> String {
    std::env::var("HOST")
        .ok()
        .or_else(|| hostname_from_system())
        .unwrap_or_else(|| "This computer".into())
}

fn hostname_from_system() -> Option<String> {
    let output = std::process::Command::new("hostname").arg("-s").output().ok()?;
    let name = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

pub fn new_device_id() -> String {
    format!("device-{}", uuid::Uuid::new_v4().simple())
}

/// The saved config, creating a device id on first use.
pub fn load() -> SyncConfig {
    if let Ok(cache) = CACHE.lock() {
        if let Some(config) = cache.as_ref() {
            return config.clone();
        }
    }
    let mut config: SyncConfig = std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if config.device_id.is_empty() {
        config.device_id = new_device_id();
        config.device_name = default_name();
        let _ = save(&config);
    }
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some(config.clone());
    }
    config
}

pub fn save(config: &SyncConfig) -> anyhow::Result<()> {
    super::files::write_json_atomic(&path(), config)?;
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some(config.clone());
    }
    Ok(())
}

/// A version for data kept outside the database (voice shares): wall-clock
/// milliseconds and this device's id, in the same format as row versions.
pub fn stamp() -> String {
    format!("{:013}-{:06}-{}", super::clock::now_ms(), 0, load().device_id)
}
