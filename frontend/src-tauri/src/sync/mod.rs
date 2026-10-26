//! Library sync through a folder that a cloud client (Google Drive for
//! desktop, Dropbox, OneDrive, iCloud Drive) keeps in step between machines.
//! See docs/LIBRARY_SYNC.md for the layout, versions and merge rules.
pub mod clock;
pub mod config;
pub mod engine;
pub mod files;
pub mod relocate;
pub mod tables;

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
