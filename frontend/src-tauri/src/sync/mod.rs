//! Library sync through a folder that a cloud client (Google Drive for
//! desktop, Dropbox, OneDrive, iCloud Drive) keeps in step between machines.
//! See docs/LIBRARY_SYNC.md for the layout, versions and merge rules.
//!
//! One native worker owns sync: it exports this device's changes, imports
//! other devices' files, merges voices, and moves settled recordings into the
//! library. Commands only change the config and wake it.
pub mod clock;
pub mod config;
pub mod engine;
pub mod files;
pub mod relocate;
pub mod tables;

use serde::Serialize;
use sqlx::SqlitePool;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use clock::Hlc;
use config::SyncConfig;
use files::{read_json, write_json_atomic, Library};

pub const STATUS_EVENT: &str = "sync-status";
pub const IMPORTED_EVENT: &str = "sync-imported";
/// How often the worker checks for local changes and other devices' files.
const TICK: Duration = Duration::from_secs(3);
/// Other devices' files are read at least this often.
const POLL: Duration = Duration::from_secs(30);

static APP: OnceLock<AppHandle> = OnceLock::new();
static WAKE: OnceLock<Notify> = OnceLock::new();
static STATUS: Mutex<PassStatus> = Mutex::new(PassStatus { running: false, last_sync: None, last_error: None });

#[derive(Clone, Debug, Default, Serialize)]
struct PassStatus {
    running: bool,
    last_sync: Option<String>,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub app_version: String,
    /// When that device last wrote to the library (RFC 3339).
    pub updated: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub library_root: Option<String>,
    pub device_id: String,
    pub device_name: String,
    pub running: bool,
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
    pub devices: Vec<DeviceInfo>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Imported {
    pub meeting_ids: Vec<String>,
    pub library: bool,
    pub voices: bool,
}

fn wake() -> &'static Notify {
    WAKE.get_or_init(Notify::new)
}

fn pool() -> Option<SqlitePool> {
    let state = APP.get()?.try_state::<crate::state::AppState>()?;
    Some(state.db_manager.pool().clone())
}

fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(app) = APP.get() {
        let _ = app.emit(event, payload);
    }
}

/// Starts the worker. Called once from app setup.
pub fn start(app: AppHandle) {
    if APP.set(app).is_err() {
        return;
    }
    tauri::async_runtime::spawn(worker());
}

async fn worker() {
    // The database is managed after setup.
    let pool = loop {
        if let Some(pool) = pool() {
            break pool;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    if let Err(error) = tables::install_triggers(&pool).await {
        log::error!("Sync triggers could not be installed: {error}");
        return;
    }
    let mut since_poll = POLL;
    loop {
        let config = config::load();
        if config.enabled && config.library_root.is_some() {
            let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox").fetch_one(&pool).await.unwrap_or(0);
            if pending > 0 || since_poll >= POLL {
                since_poll = Duration::ZERO;
                pass(&pool, &config).await;
            }
        }
        tokio::select! {
            _ = wake().notified() => since_poll = POLL,
            _ = tokio::time::sleep(TICK) => since_poll += TICK,
        }
    }
}

fn set_status(change: impl FnOnce(&mut PassStatus)) {
    if let Ok(mut status) = STATUS.lock() {
        change(&mut status);
    }
    emit(STATUS_EVENT, status());
}

async fn pass(pool: &SqlitePool, config: &SyncConfig) {
    set_status(|status| status.running = true);
    let outcome = run_pass(pool, config, true).await;
    let now = chrono::Utc::now().to_rfc3339();
    match outcome {
        Ok(imported) => {
            if imported != Imported::default() {
                emit(IMPORTED_EVENT, imported);
            }
            set_status(|status| {
                status.running = false;
                status.last_sync = Some(now);
                status.last_error = None;
            });
        }
        Err(error) => {
            log::warn!("Library sync pass failed: {error:#}");
            set_status(|status| {
                status.running = false;
                status.last_error = Some(error.to_string());
            });
        }
    }
}

/// One full pass. `voices` is false in tests, which must not touch the real
/// voice files in app data.
async fn run_pass(pool: &SqlitePool, config: &SyncConfig, voices: bool) -> anyhow::Result<Imported> {
    let root = config.library_root.as_ref().ok_or_else(|| anyhow::anyhow!("No library folder chosen"))?;
    if !root.is_dir() {
        anyhow::bail!("The library folder {} is not available", root.display());
    }
    let library = Library::create_or_open(root)?;
    let device = &config.device_id;
    let mut conn = pool.acquire().await?;
    tables::set_capture(&mut conn, true).await?;
    let seeded_key = format!("seeded:{}", root.display());
    if engine::get_state(&mut conn, &seeded_key).await?.is_none() {
        drop(conn);
        engine::seed_all(pool).await?;
        conn = pool.acquire().await?;
        sqlx::query("INSERT OR REPLACE INTO sync_state (key, value) VALUES (?, '1')").bind(&seeded_key).execute(&mut *conn).await?;
    }
    let last = engine::get_state(&mut conn, "clock").await?;
    drop(conn);
    let mut clock = Hlc::new(device, last.as_deref());

    engine::export(pool, &library, device, &mut clock).await?;
    let imported = engine::import(pool, &library, device, &mut clock).await?;
    let mut voices_changed = false;
    if voices {
        voices_changed = engine::import_voices(pool, &library, device).await?;
        engine::export_voices(&library, device)?;
    }
    if !relocate::relocate_settled(pool, &library, std::time::SystemTime::now()).await.is_empty() {
        engine::export(pool, &library, device, &mut clock).await?;
    }
    write_device_info(&library, config)?;
    Ok(Imported {
        meeting_ids: imported.meetings.into_iter().collect(),
        library: imported.library,
        voices: voices_changed,
    })
}

fn write_device_info(library: &Library, config: &SyncConfig) -> anyhow::Result<()> {
    let info = DeviceInfo {
        id: config.device_id.clone(),
        name: config.device_name.clone(),
        platform: std::env::consts::OS.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        updated: chrono::Utc::now().to_rfc3339(),
    };
    write_json_atomic(&library.device_dir(&config.device_id).join("device.json"), &info)
}

fn devices(config: &SyncConfig) -> Vec<DeviceInfo> {
    let Some(root) = &config.library_root else { return Vec::new() };
    let library = Library { root: root.clone() };
    library
        .other_devices(&config.device_id)
        .iter()
        .filter_map(|device| read_json::<DeviceInfo>(&library.device_dir(device).join("device.json")))
        .collect()
}

fn status() -> SyncStatus {
    let config = config::load();
    let pass = STATUS.lock().map(|status| status.clone()).unwrap_or_default();
    SyncStatus {
        enabled: config.enabled,
        library_root: config.library_root.as_ref().map(|root| root.display().to_string()),
        device_id: config.device_id.clone(),
        device_name: config.device_name.clone(),
        running: pass.running,
        last_sync: pass.last_sync,
        last_error: pass.last_error,
        devices: devices(&config),
    }
}

/// Exports pending changes before the app quits, bounded so quitting never
/// hangs on a slow cloud folder.
pub async fn flush() {
    let config = config::load();
    let (true, Some(root), Some(pool)) = (config.enabled, config.library_root.clone(), pool()) else { return };
    let work = async {
        let library = Library::create_or_open(&root)?;
        let mut conn = pool.acquire().await?;
        let last = engine::get_state(&mut conn, "clock").await?;
        drop(conn);
        let mut clock = Hlc::new(&config.device_id, last.as_deref());
        engine::export(&pool, &library, &config.device_id, &mut clock).await?;
        anyhow::Ok(())
    };
    if let Ok(Err(error)) = tokio::time::timeout(Duration::from_secs(5), work).await {
        log::warn!("Could not export library changes before quitting: {error:#}");
    }
}

#[tauri::command]
pub fn get_sync_status() -> SyncStatus {
    status()
}

/// Starts syncing with a library folder: a new folder becomes a library, an
/// existing one is joined. This device's existing meetings are published.
#[tauri::command]
pub fn enable_sync(library_root: String, device_name: Option<String>) -> Result<SyncStatus, String> {
    let root = std::path::PathBuf::from(library_root.trim());
    if !root.is_absolute() || !root.is_dir() {
        return Err("Choose a folder that exists, such as one inside Google Drive.".into());
    }
    if root.starts_with(crate::paths::install_data_root()) {
        return Err("Choose a folder outside Meetily's own data folder.".into());
    }
    Library::create_or_open(&root).map_err(|error| format!("Cannot use this folder: {error}"))?;
    let mut config = config::load();
    config.enabled = true;
    config.library_root = Some(root);
    if let Some(name) = device_name.map(|name| name.trim().to_string()).filter(|name| !name.is_empty()) {
        config.device_name = name;
    }
    config::save(&config).map_err(|error| error.to_string())?;
    wake().notify_one();
    Ok(status())
}

/// Stops syncing. Local meetings stay. Changes keep being noted, so turning
/// sync back on publishes what changed meanwhile.
#[tauri::command]
pub fn disable_sync() -> Result<SyncStatus, String> {
    let mut config = config::load();
    config.enabled = false;
    config::save(&config).map_err(|error| error.to_string())?;
    Ok(status())
}

#[tauri::command]
pub fn sync_now() {
    wake().notify_one();
}

#[tauri::command]
pub fn set_sync_device_name(name: String) -> Result<SyncStatus, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name for this computer.".into());
    }
    let mut config = config::load();
    config.device_name = name.to_string();
    config::save(&config).map_err(|error| error.to_string())?;
    wake().notify_one();
    Ok(status())
}

#[cfg(test)]
pub(crate) mod test_support {
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    /// A fresh database with every app migration applied. One connection,
    /// because each `:memory:` connection is its own database.
    pub async fn migrated_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enable_on_new_folder_creates_layout_and_publishes() {
        let root = std::env::temp_dir().join(format!("meetily-sync-pass-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let pool = test_support::migrated_pool().await;
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1', 'Before sync', 'c', 'u')")
            .execute(&pool).await.unwrap();
        tables::install_triggers(&pool).await.unwrap();
        let config = SyncConfig {
            device_id: "device-test".into(),
            device_name: "Test Mac".into(),
            library_root: Some(root.clone()),
            enabled: true,
        };
        let imported = run_pass(&pool, &config, false).await.unwrap();
        assert_eq!(imported, Imported::default());
        assert!(root.join("library.json").is_file());
        assert!(root.join("devices/device-test/meetings/m1.json").is_file());
        let info: DeviceInfo = read_json(&root.join("devices/device-test/device.json")).unwrap();
        assert_eq!(info.name, "Test Mac");
        // Capture is now on: a new edit reaches the next pass.
        sqlx::query("UPDATE meetings SET title = 'After' WHERE id = 'm1'").execute(&pool).await.unwrap();
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox").fetch_one(&pool).await.unwrap();
        assert_eq!(pending, 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn config_round_trips() {
        let config = SyncConfig {
            device_id: "device-1".into(),
            device_name: "Mac".into(),
            library_root: Some("/Users/me/Google Drive/Meetily".into()),
            enabled: true,
        };
        let text = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<SyncConfig>(&text).unwrap(), config);
        // Older files without optional fields still load.
        let minimal: SyncConfig = serde_json::from_str(r#"{"device_id":"d","device_name":"n","library_root":null,"enabled":false}"#).unwrap();
        assert!(!minimal.enabled);
    }
}
