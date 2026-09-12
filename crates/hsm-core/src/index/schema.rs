use rusqlite::Connection;

use crate::error::Result;

pub const SCHEMA_VERSION: i64 = 1;

/// FTS5 external-content tables are kept in sync by triggers so every write
/// path (upsert, delete, the refresh batches) stays a plain SQL statement.
const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
  harness            TEXT    NOT NULL,
  id                 TEXT    NOT NULL PRIMARY KEY,
  cwd                TEXT    NOT NULL DEFAULT '',
  project            TEXT    NOT NULL DEFAULT '',
  title              TEXT,
  first_prompt       TEXT,
  started_at         INTEGER,
  last_active_at     INTEGER,
  size_bytes         INTEGER NOT NULL DEFAULT 0,
  transcript_path    TEXT,
  transcript_present INTEGER NOT NULL DEFAULT 0,
  pinned             INTEGER NOT NULL DEFAULT 0,
  last_pane_json     TEXT,
  source             TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS sessions_by_activity ON sessions(last_active_at DESC);
CREATE INDEX IF NOT EXISTS sessions_by_harness  ON sessions(harness);
CREATE INDEX IF NOT EXISTS sessions_by_project  ON sessions(project);

CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(
  title, first_prompt, project,
  content='sessions', content_rowid='rowid', tokenize='unicode61'
);

CREATE TRIGGER IF NOT EXISTS sessions_ai AFTER INSERT ON sessions BEGIN
  INSERT INTO sessions_fts(rowid, title, first_prompt, project)
  VALUES (new.rowid, new.title, new.first_prompt, new.project);
END;
CREATE TRIGGER IF NOT EXISTS sessions_ad AFTER DELETE ON sessions BEGIN
  INSERT INTO sessions_fts(sessions_fts, rowid, title, first_prompt, project)
  VALUES ('delete', old.rowid, old.title, old.first_prompt, old.project);
END;
CREATE TRIGGER IF NOT EXISTS sessions_au AFTER UPDATE ON sessions BEGIN
  INSERT INTO sessions_fts(sessions_fts, rowid, title, first_prompt, project)
  VALUES ('delete', old.rowid, old.title, old.first_prompt, old.project);
  INSERT INTO sessions_fts(rowid, title, first_prompt, project)
  VALUES (new.rowid, new.title, new.first_prompt, new.project);
END;

CREATE TABLE IF NOT EXISTS messages (
  harness TEXT    NOT NULL,
  id      TEXT    NOT NULL,
  seq     INTEGER NOT NULL,
  role    TEXT    NOT NULL,
  ts      INTEGER,
  text    TEXT    NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS messages_key        ON messages(harness, id, seq);
CREATE INDEX        IF NOT EXISTS messages_by_session ON messages(id);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  text, content='messages', content_rowid='rowid', tokenize='unicode61'
);

CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
END;
CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
  INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text);
END;

CREATE TABLE IF NOT EXISTS scan_state (
  path        TEXT    PRIMARY KEY,
  byte_offset INTEGER NOT NULL DEFAULT 0,
  mtime       INTEGER NOT NULL DEFAULT 0,
  size        INTEGER NOT NULL DEFAULT 0
);
"#;

pub fn init(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}
