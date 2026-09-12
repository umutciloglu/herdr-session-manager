//! Durable state: messages, the live-session registry, and read cursors.
//!
//! The API is synchronous — sqlite is fast enough that hiding it behind a mutex beats
//! the complexity of an async pool, and every caller already owns an `Arc<Store>`.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use crate::domain::{Address, Harness, Message, MessageStatus, Registration, MIN_PREFIX};
use crate::error::{Error, Result};
use crate::ids;
use crate::paths::Paths;
use crate::traits::{ProcessProbe, SysinfoProbe};

const SCHEMA_VERSION: i64 = 2;

/// How long a registration with no pid is taken at its word. A SessionStart hook can
/// only say "this session existed"; nothing tells us when its pane closed, so the claim
/// expires instead of standing forever.
pub const HOOK_ROW_TTL: chrono::Duration = chrono::Duration::minutes(10);

/// When `prune` gives up on an unproven row entirely.
pub const PRUNE_AFTER: chrono::Duration = chrono::Duration::hours(24);

const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const MIGRATE_ATTEMPTS: usize = 3;
const MIGRATE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// What we can actually say about a registration's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// A pid we can see.
    Proven,
    /// A hook row inside the freshness window: plausibly running, not provable.
    /// A `Directory`, when there is one, gets the final say.
    Unproven,
    /// Nothing suggests this session is running.
    Gone,
}

pub struct Store {
    conn: Mutex<Connection>,
    probe: Box<dyn ProcessProbe>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Store::from_connection(Connection::open(path)?)
    }

    /// The store at the resolved state dir, creating it if needed.
    pub fn open_at(paths: &Paths) -> Result<Store> {
        paths.ensure()?;
        Store::open(paths.db())
    }

    pub fn open_in_memory() -> Result<Store> {
        Store::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut conn: Connection) -> Result<Store> {
        // First, before anything that can block.
        conn.busy_timeout(BUSY_TIMEOUT)?;

        // WAL so a draining hook and a waiting MCP process do not block each other.
        // It is a property of the file, set once and persisted, and switching it needs
        // an exclusive lock that sqlite refuses outright — without running the busy
        // handler — while any other connection holds the database. So this is best
        // effort: whoever is holding it has already set WAL, or the next open will.
        if let Err(e) = conn.pragma_update(None, "journal_mode", "WAL") {
            if !is_busy_sqlite(&e) {
                return Err(e.into());
            }
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate_with_retry(&mut conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
            probe: Box::new(SysinfoProbe),
        })
    }

    pub fn with_probe(mut self, probe: Box<dyn ProcessProbe>) -> Self {
        self.probe = probe;
        self
    }

    /// A poisoned mutex only means some other thread panicked mid-query; the
    /// connection itself is still usable, so keep serving rather than cascading.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn schema_version(&self) -> Result<i64> {
        let conn = self.conn();
        Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    // ---- messages ----------------------------------------------------------

    pub fn enqueue(&self, msg: &Message) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages
               (id, from_addr, to_addr, text, reply_to, expects_reply, status, created_at, delivered_at, error, pushed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                msg.id,
                msg.from.to_string(),
                msg.to.to_string(),
                msg.text,
                msg.reply_to,
                msg.expects_reply as i64,
                msg.status.as_str(),
                ids::to_rfc3339(&msg.created_at),
                msg.delivered_at.as_ref().map(ids::to_rfc3339),
                msg.error,
                msg.pushed_at.as_ref().map(ids::to_rfc3339),
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<Message>> {
        let conn = self.conn();
        let msg = conn
            .query_row(
                "SELECT id, from_addr, to_addr, text, reply_to, expects_reply, status,
                        created_at, delivered_at, error, pushed_at
                 FROM messages WHERE id = ?1",
                params![id],
                message_from_row,
            )
            .optional()?;
        msg.transpose()
    }

    /// Re-point a message once the resolver has turned a target into a real address.
    pub fn retarget(&self, id: &str, to: &Address) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE messages SET to_addr = ?2 WHERE id = ?1",
            params![id, to.to_string()],
        )?;
        Ok(())
    }

    /// Move every message on `old` — sent or received — to `new`.
    ///
    /// A session that started before it could identify itself writes rows under a
    /// provisional address; once the real id turns up, those rows have to follow it, or
    /// a peer's reply is stranded on an address nobody listens to.
    pub fn retarget_address(&self, old: &Address, new: &Address) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (old, new) = (old.to_string(), new.to_string());
        let mut moved = tx.execute(
            "UPDATE messages SET to_addr = ?2 WHERE to_addr = ?1",
            params![old, new],
        )?;
        moved += tx.execute(
            "UPDATE messages SET from_addr = ?2 WHERE from_addr = ?1",
            params![old, new],
        )?;
        tx.commit()?;
        Ok(moved)
    }

    pub fn pending_for(&self, addr: &Address) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, from_addr, to_addr, text, reply_to, expects_reply, status,
                    created_at, delivered_at, error, pushed_at
             FROM messages
             WHERE to_addr = ?1 AND status = 'pending'
             ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![addr.to_string()], message_from_row)?;
        collect(rows)
    }

    /// Oldest message the recipient has not consumed yet. `wait` uses this, so it
    /// includes `delivered` rows: delivered means "pushed", not "read".
    pub fn next_for(&self, addr: &Address, reply_to: Option<&str>) -> Result<Option<Message>> {
        let conn = self.conn();
        let sql = "SELECT id, from_addr, to_addr, text, reply_to, expects_reply, status,
                          created_at, delivered_at, error, pushed_at
                   FROM messages
                   WHERE to_addr = ?1 AND status IN ('pending','delivered')";
        let found = match reply_to {
            Some(rt) => conn
                .query_row(
                    &format!("{sql} AND reply_to = ?2 ORDER BY created_at ASC, rowid ASC LIMIT 1"),
                    params![addr.to_string(), rt],
                    message_from_row,
                )
                .optional()?,
            None => conn
                .query_row(
                    &format!("{sql} ORDER BY created_at ASC, rowid ASC LIMIT 1"),
                    params![addr.to_string()],
                    message_from_row,
                )
                .optional()?,
        };
        found.transpose()
    }

    pub fn inbox(&self, addr: &Address, include_read: bool, limit: usize) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, from_addr, to_addr, text, reply_to, expects_reply, status,
                    created_at, delivered_at, error, pushed_at
             FROM messages
             WHERE to_addr = ?1 AND (?2 = 1 OR status != 'read')
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![addr.to_string(), include_read as i64, limit as i64],
            message_from_row,
        )?;
        collect(rows)
    }

    pub fn outbox(&self, addr: &Address, limit: usize) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, from_addr, to_addr, text, reply_to, expects_reply, status,
                    created_at, delivered_at, error, pushed_at
             FROM messages
             WHERE from_addr = ?1
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![addr.to_string(), limit as i64], message_from_row)?;
        collect(rows)
    }

    /// Take everything pending for `addr` and mark it delivered in one go.
    ///
    /// `BEGIN IMMEDIATE` takes the write lock before the select, so two hook processes
    /// draining the same session cannot both claim the same rows and hand the model the
    /// same mail twice. Callers that only want to look use `pending_for`.
    pub fn claim_pending(&self, addr: &Address) -> Result<Vec<Message>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let claimed = {
            let mut stmt = tx.prepare(
                "SELECT id, from_addr, to_addr, text, reply_to, expects_reply, status,
                        created_at, delivered_at, error, pushed_at
                 FROM messages
                 WHERE to_addr = ?1 AND status = 'pending'
                 ORDER BY created_at ASC, rowid ASC",
            )?;
            let rows = stmt.query_map(params![addr.to_string()], message_from_row)?;
            collect(rows)?
        };
        if !claimed.is_empty() {
            let now = ids::to_rfc3339(&ids::now());
            let mut stmt = tx.prepare(
                "UPDATE messages
                 SET status = 'delivered',
                     delivered_at = COALESCE(delivered_at, ?2),
                     error = NULL
                 WHERE id = ?1",
            )?;
            for msg in &claimed {
                stmt.execute(params![msg.id, now])?;
            }
        }
        tx.commit()?;
        Ok(claimed)
    }

    pub fn mark_delivered(&self, ids_: &[String]) -> Result<()> {
        self.set_status(ids_, MessageStatus::Delivered, true)
    }

    /// Records that these messages went out over a channel. They stay `Pending`: a
    /// channel push is an attempt, and only the recipient's transcript proves it landed.
    pub fn mark_pushed(&self, ids_: &[String]) -> Result<()> {
        if ids_.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = ids::to_rfc3339(&ids::now());
        {
            let mut stmt = tx
                .prepare("UPDATE messages SET pushed_at = COALESCE(pushed_at, ?2) WHERE id = ?1")?;
            for id in ids_ {
                stmt.execute(params![id, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mark_read(&self, ids_: &[String]) -> Result<()> {
        self.set_status(ids_, MessageStatus::Read, false)
    }

    pub fn mark_failed(&self, id: &str, err: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE messages SET status = 'failed', error = ?2 WHERE id = ?1",
            params![id, err],
        )?;
        Ok(())
    }

    fn set_status(&self, ids_: &[String], status: MessageStatus, stamp: bool) -> Result<()> {
        if ids_.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = ids::to_rfc3339(&ids::now());
        {
            let mut stmt = tx.prepare(
                "UPDATE messages
                 SET status = ?2,
                     delivered_at = CASE WHEN ?3 = 1 AND delivered_at IS NULL THEN ?4 ELSE delivered_at END,
                     error = NULL
                 WHERE id = ?1",
            )?;
            for id in ids_ {
                stmt.execute(params![id, status.as_str(), stamp as i64, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ---- cursors -----------------------------------------------------------

    pub fn cursor(&self, addr: &Address) -> Result<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT last_read_id FROM cursors WHERE address = ?1",
                params![addr.to_string()],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn set_cursor(&self, addr: &Address, last_read_id: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO cursors (address, last_read_id) VALUES (?1, ?2)
             ON CONFLICT(address) DO UPDATE SET last_read_id = excluded.last_read_id",
            params![addr.to_string(), last_read_id],
        )?;
        Ok(())
    }

    // ---- registry ----------------------------------------------------------

    pub fn register(&self, reg: &Registration) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO registry
               (harness, session_id, alias, pid, cwd, poke_path, herdr_pane, started_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(harness, session_id) DO UPDATE SET
               alias = COALESCE(excluded.alias, registry.alias),
               pid = COALESCE(excluded.pid, registry.pid),
               cwd = excluded.cwd,
               poke_path = COALESCE(excluded.poke_path, registry.poke_path),
               herdr_pane = COALESCE(excluded.herdr_pane, registry.herdr_pane),
               last_seen = excluded.last_seen",
            params![
                reg.harness.as_str(),
                reg.session_id,
                reg.alias,
                reg.pid,
                reg.cwd.to_string_lossy(),
                reg.poke_path,
                reg.herdr_pane,
                ids::to_rfc3339(&reg.started_at),
                ids::to_rfc3339(&reg.last_seen),
            ],
        )?;
        Ok(())
    }

    pub fn deregister(&self, harness: &Harness, session_id: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM registry WHERE harness = ?1 AND session_id = ?2",
            params![harness.as_str(), session_id],
        )?;
        Ok(())
    }

    pub fn touch(&self, harness: &Harness, session_id: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE registry SET last_seen = ?3 WHERE harness = ?1 AND session_id = ?2",
            params![harness.as_str(), session_id, ids::to_rfc3339(&ids::now())],
        )?;
        Ok(())
    }

    pub fn get_registration(
        &self,
        harness: &Harness,
        session_id: &str,
    ) -> Result<Option<Registration>> {
        let conn = self.conn();
        let reg = conn
            .query_row(
                &format!("{REG_SELECT} WHERE harness = ?1 AND session_id = ?2"),
                params![harness.as_str(), session_id],
                registration_from_row,
            )
            .optional()?;
        reg.transpose()
    }

    pub fn find(&self, addr: &Address) -> Result<Option<Registration>> {
        self.get_registration(&addr.harness, &addr.id)
    }

    /// Every row, newest first, with no liveness filtering and no deletions. The
    /// resolver uses this: an offline session is still a legitimate target.
    pub fn registrations(&self) -> Result<Vec<Registration>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{REG_SELECT} ORDER BY last_seen DESC"))?;
        let rows = stmt.query_map([], registration_from_row)?;
        collect(rows)
    }

    /// The single place that decides whether a registration means "running".
    pub fn liveness(&self, reg: &Registration) -> Liveness {
        // A person is never a process, however recently they were seen.
        if reg.is_human() {
            return Liveness::Gone;
        }
        match reg.pid {
            Some(pid) if self.probe.is_alive(pid) => Liveness::Proven,
            Some(_) => Liveness::Gone,
            None if ids::now() - reg.last_seen <= HOOK_ROW_TTL => Liveness::Unproven,
            None => Liveness::Gone,
        }
    }

    /// Registrations that plausibly have a process behind them. Rows with a provably
    /// dead pid are deleted on the way past; unproven rows are only filtered out,
    /// because a stale hook row is still an addressable offline session.
    pub fn live(&self) -> Result<Vec<Registration>> {
        let mut live = Vec::new();
        let mut dead = Vec::new();
        for reg in self.registrations()? {
            match self.liveness(&reg) {
                Liveness::Proven | Liveness::Unproven => live.push(reg),
                Liveness::Gone if reg.pid.is_some() => dead.push(reg),
                Liveness::Gone => {}
            }
        }
        for reg in &dead {
            self.deregister(&reg.harness, &reg.session_id)?;
        }
        Ok(live)
    }

    /// Drop rows nothing can reach any more: dead pids, and unproven rows older than
    /// `PRUNE_AFTER`. `human:` rows survive forever — they are reply sinks, not sessions.
    /// Returns how many rows went. Callers: MCP startup and `agentmail doctor`.
    pub fn prune(&self, now: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let mut gone = 0;
        for reg in self.registrations()? {
            if reg.is_human() {
                continue;
            }
            let expired = match reg.pid {
                Some(pid) => !self.probe.is_alive(pid),
                None => now - reg.last_seen > PRUNE_AFTER,
            };
            if expired {
                self.deregister(&reg.harness, &reg.session_id)?;
                gone += 1;
            }
        }
        Ok(gone)
    }

    /// Live registrations whose session id starts with `prefix`. Prefixes shorter than
    /// `MIN_PREFIX` are rejected outright (protocol rule), so callers cannot widen a
    /// match by accident.
    pub fn find_by_prefix(&self, harness: &Harness, prefix: &str) -> Result<Vec<Registration>> {
        if prefix.len() < MIN_PREFIX {
            return Ok(Vec::new());
        }
        Ok(self
            .live()?
            .into_iter()
            .filter(|r| &r.harness == harness && r.address().matches_prefix(prefix))
            .collect())
    }

    pub fn find_alias(&self, name: &str) -> Result<Option<Registration>> {
        Ok(self.live()?.into_iter().find(|r| {
            r.alias
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(name))
        }))
    }

    pub fn set_alias(
        &self,
        harness: &Harness,
        session_id: &str,
        alias: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE registry SET alias = ?3 WHERE harness = ?1 AND session_id = ?2",
            params![harness.as_str(), session_id, alias],
        )?;
        if changed == 0 {
            return Err(Error::not_found(format!("{harness}:{session_id}")));
        }
        Ok(())
    }
}

const REG_SELECT: &str = "SELECT harness, session_id, alias, pid, cwd, poke_path, herdr_pane,
                                 started_at, last_seen
                          FROM registry";

/// Opening the store must survive another process holding the write lock, so a busy
/// database is retried rather than reported.
fn migrate_with_retry(conn: &mut Connection) -> Result<()> {
    let mut last = None;
    for attempt in 1..=MIGRATE_ATTEMPTS {
        match migrate(conn) {
            Ok(()) => return Ok(()),
            Err(e) if is_busy(&e) => {
                last = Some(e);
                if attempt < MIGRATE_ATTEMPTS {
                    std::thread::sleep(MIGRATE_RETRY_DELAY);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| Error::parse("migration retries exhausted")))
}

fn migrate(conn: &mut Connection) -> Result<()> {
    // Cheap unlocked read: an already-current database is the overwhelming common case
    // and must not take the write lock on every open.
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    // BEGIN IMMEDIATE grabs the write lock before we read the version, so a second
    // process waits here (busy_timeout) instead of racing us into the same ALTER TABLE.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Re-read under the lock: whoever we waited for may have done the work already.
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    if version < 1 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
               id            TEXT PRIMARY KEY,
               from_addr     TEXT NOT NULL,
               to_addr       TEXT NOT NULL,
               text          TEXT NOT NULL,
               reply_to      TEXT,
               expects_reply INTEGER NOT NULL DEFAULT 0,
               status        TEXT NOT NULL,
               created_at    TEXT NOT NULL,
               delivered_at  TEXT,
               error         TEXT
             );
             CREATE INDEX IF NOT EXISTS messages_to_status ON messages(to_addr, status, created_at);
             CREATE INDEX IF NOT EXISTS messages_reply_to ON messages(reply_to);
             CREATE INDEX IF NOT EXISTS messages_from ON messages(from_addr, created_at);

             CREATE TABLE IF NOT EXISTS registry (
               harness    TEXT NOT NULL,
               session_id TEXT NOT NULL,
               alias      TEXT,
               pid        INTEGER,
               cwd        TEXT NOT NULL,
               poke_path  TEXT,
               herdr_pane TEXT,
               started_at TEXT NOT NULL,
               last_seen  TEXT NOT NULL,
               PRIMARY KEY (harness, session_id)
             );
             CREATE INDEX IF NOT EXISTS registry_alias ON registry(alias);

             CREATE TABLE IF NOT EXISTS cursors (
               address      TEXT PRIMARY KEY,
               last_read_id TEXT
             );",
        )?;
    }

    if version < 2 {
        // A channel push is recorded, not assumed: see `Message::pushed_at`.
        add_column(&tx, "ALTER TABLE messages ADD COLUMN pushed_at TEXT;")?;
    }

    // Same transaction as the steps, so the version can never run ahead of the schema.
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// `ADD COLUMN` is not idempotent. A partial run from before this was transactional can
/// leave the column behind, and that is exactly the state we were after — so take it as
/// applied rather than refusing to start.
fn add_column(tx: &Transaction<'_>, sql: &str) -> Result<()> {
    match tx.execute_batch(sql) {
        Err(e) if is_duplicate_column(&e) => Ok(()),
        other => Ok(other?),
    }
}

fn is_duplicate_column(e: &rusqlite::Error) -> bool {
    e.to_string()
        .to_ascii_lowercase()
        .contains("duplicate column name")
}

fn is_busy_sqlite(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == rusqlite::ErrorCode::DatabaseBusy
                || inner.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

fn is_busy(e: &Error) -> bool {
    matches!(e, Error::Sqlite(inner) if is_busy_sqlite(inner))
}

type RowResult<T> = rusqlite::Result<Result<T>>;

/// Row mapping is fallible twice over: sqlite errors, then our own parsing of the
/// text columns. The inner `Result` carries the second kind out of rusqlite.
fn message_from_row(row: &Row<'_>) -> RowResult<Message> {
    Ok(build_message(row))
}

fn build_message(row: &Row<'_>) -> Result<Message> {
    let from: String = row.get(1)?;
    let to: String = row.get(2)?;
    let status: String = row.get(6)?;
    let created: String = row.get(7)?;
    let delivered: Option<String> = row.get(8)?;
    Ok(Message {
        id: row.get(0)?,
        from: from.parse()?,
        to: to.parse()?,
        text: row.get(3)?,
        reply_to: row.get(4)?,
        expects_reply: row.get::<_, i64>(5)? != 0,
        status: status.parse()?,
        created_at: ids::parse_rfc3339(&created)?,
        delivered_at: delivered.as_deref().map(ids::parse_rfc3339).transpose()?,
        error: row.get(9)?,
        pushed_at: row
            .get::<_, Option<String>>(10)?
            .as_deref()
            .map(ids::parse_rfc3339)
            .transpose()?,
    })
}

fn registration_from_row(row: &Row<'_>) -> RowResult<Registration> {
    Ok(build_registration(row))
}

fn build_registration(row: &Row<'_>) -> Result<Registration> {
    let harness: String = row.get(0)?;
    let cwd: String = row.get(4)?;
    let started: String = row.get(7)?;
    let last_seen: String = row.get(8)?;
    Ok(Registration {
        harness: harness.parse()?,
        session_id: row.get(1)?,
        alias: row.get(2)?,
        pid: row.get::<_, Option<i64>>(3)?.map(|p| p as u32),
        cwd: cwd.into(),
        poke_path: row.get(5)?,
        herdr_pane: row.get(6)?,
        started_at: ids::parse_rfc3339(&started)?,
        last_seen: ids::parse_rfc3339(&last_seen)?,
    })
}

fn collect<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> RowResult<T>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row??);
    }
    Ok(out)
}
