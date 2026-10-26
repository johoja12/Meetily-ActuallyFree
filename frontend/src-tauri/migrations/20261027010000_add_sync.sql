-- Library sync through a shared folder (docs/LIBRARY_SYNC.md).
-- Triggers on the synced tables are installed at runtime by sync::tables so
-- their column lists stay in step with the specs there; these tables only hold
-- sync bookkeeping and never leave this device.
CREATE TABLE sync_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Capture is off until sync is turned on, so the outbox does not grow.
INSERT INTO sync_state (key, value) VALUES ('capture', '0');

-- Rows changed locally since the last export, in change order.
CREATE TABLE sync_outbox (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    table_name TEXT NOT NULL,
    row_key TEXT NOT NULL,
    meeting_id TEXT
);

-- The version this device knows for every synced row, local or imported.
-- A deleted row keeps its entry as a tombstone.
CREATE TABLE sync_versions (
    table_name TEXT NOT NULL,
    row_key TEXT NOT NULL,
    version TEXT NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    meeting_id TEXT,
    PRIMARY KEY (table_name, row_key)
);
CREATE INDEX idx_sync_versions_meeting ON sync_versions(meeting_id);

-- A contact or group id that now means another one: same-name records from
-- two devices, or a contact merged into another. Synced like other records.
CREATE TABLE sync_redirects (
    kind TEXT NOT NULL CHECK (kind IN ('person', 'group')),
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (kind, from_id)
);
