mod refresh;
mod schema;
mod search;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row};

use crate::domain::{HarnessKind, PaneRef, Session, Tier};
use crate::error::{Error, Result};

pub use refresh::{RefreshOptions, RefreshReport};
pub use search::Query;

/// Where a row's metadata came from. Kept so a herdr-only stub can be told
/// apart from a fully parsed transcript.
pub const SOURCE_TRANSCRIPT: &str = "transcript";
pub const SOURCE_HERDR: &str = "herdr";

const DEFAULT_HOT_DAYS: u32 = 30;

/// `meta` key holding the end of the last refresh, in epoch milliseconds.
pub(crate) const LAST_REFRESH_KEY: &str = "last_refresh_at";

pub struct Index {
    conn: Connection,
    hot_days: u32,
}

impl Index {
    pub fn open(path: &Path) -> Result<Index> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
            }
        }
        let conn = Connection::open(path)?;
        schema::init(&conn)?;
        let mut index = Index {
            conn,
            hot_days: DEFAULT_HOT_DAYS,
        };
        index.hot_days = index.meta_u32("hot_days").unwrap_or(DEFAULT_HOT_DAYS);
        Ok(index)
    }

    pub fn open_in_memory() -> Result<Index> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        Ok(Index {
            conn,
            hot_days: DEFAULT_HOT_DAYS,
        })
    }

    pub fn hot_days(&self) -> u32 {
        self.hot_days
    }

    pub fn set_hot_days(&mut self, days: u32) -> Result<()> {
        self.hot_days = days;
        self.put_meta("hot_days", &days.to_string())
    }

    /// When [`Index::refresh`] last finished, or `None` for an index that has
    /// never been scanned. Callers that run on someone else's schedule — the
    /// agentmail provider asks for a card per resource read — use it to skip a
    /// scan that just happened.
    pub fn last_refresh(&self) -> Option<DateTime<Utc>> {
        self.meta(LAST_REFRESH_KEY)?
            .parse()
            .ok()
            .and_then(from_millis)
    }

    /// Full metadata write. Never clears a field the caller does not know
    /// about, so a cheap pass cannot erase what an expensive one found.
    pub fn upsert(&self, s: &Session, source: &str) -> Result<()> {
        upsert_session(&self.conn, s, source)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions \
             ORDER BY pinned DESC, COALESCE(last_active_at, 0) DESC LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([limit as i64], |r| Ok(self.to_session(r)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Exact id first, then a unique prefix. `harness` narrows the search when
    /// the caller parsed an address.
    pub fn get(
        &self,
        harness: Option<&HarnessKind>,
        id_or_prefix: &str,
    ) -> Result<Option<Session>> {
        let harness_filter = harness.map(|h| h.to_string()).unwrap_or_default();
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions \
             WHERE (?1 = '' OR harness = ?1) AND (id = ?2 OR id LIKE ?2 || '%') LIMIT 3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![harness_filter, id_or_prefix], |r| {
            Ok(self.to_session(r))
        })?;
        let mut found = rows.collect::<std::result::Result<Vec<_>, _>>()?;

        if let Some(pos) = found.iter().position(|s| s.id == id_or_prefix) {
            return Ok(Some(found.swap_remove(pos)));
        }
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.pop()),
            _ => Err(Error::AmbiguousId(id_or_prefix.to_string())),
        }
    }

    pub fn pin(&self, harness: &HarnessKind, id: &str) -> Result<bool> {
        self.set_pinned(harness, id, true)
    }

    pub fn unpin(&self, harness: &HarnessKind, id: &str) -> Result<bool> {
        self.set_pinned(harness, id, false)
    }

    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    pub fn message_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    fn set_pinned(&self, harness: &HarnessKind, id: &str, pinned: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE sessions SET pinned = ?3 WHERE harness = ?1 AND id = ?2",
            rusqlite::params![harness.to_string(), id, pinned as i64],
        )?;
        Ok(n > 0)
    }

    fn meta(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    fn meta_u32(&self, key: &str) -> Option<u32> {
        self.meta(key)?.parse().ok()
    }

    fn put_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    fn to_session(&self, r: &Row<'_>) -> Session {
        row_to_session(r, self.hot_days)
    }
}

pub(crate) fn row_to_session(r: &Row<'_>, hot_days: u32) -> Session {
    let harness = HarnessKind::from_name(&r.get::<_, String>(0).unwrap_or_default());
    let cwd: String = r.get(2).unwrap_or_default();
    let mut s = Session::new(harness, r.get::<_, String>(1).unwrap_or_default(), cwd);
    s.project = r.get(3).unwrap_or_default();
    s.title = r.get(4).unwrap_or(None);
    s.first_prompt = r.get(5).unwrap_or(None);
    s.started_at = r
        .get::<_, Option<i64>>(6)
        .ok()
        .flatten()
        .and_then(from_millis);
    s.last_active_at = r
        .get::<_, Option<i64>>(7)
        .ok()
        .flatten()
        .and_then(from_millis);
    s.size_bytes = r.get::<_, i64>(8).unwrap_or(0).max(0) as u64;
    s.transcript_path = r
        .get::<_, Option<String>>(9)
        .unwrap_or(None)
        .map(PathBuf::from);
    s.transcript_present = r.get::<_, i64>(10).unwrap_or(0) != 0;
    s.pinned = r.get::<_, i64>(11).unwrap_or(0) != 0;
    s.last_pane = r
        .get::<_, Option<String>>(12)
        .unwrap_or(None)
        .and_then(|j| serde_json::from_str::<PaneRef>(&j).ok());
    s.tier = tier_of(&s, hot_days);
    s
}

fn tier_of(s: &Session, hot_days: u32) -> Tier {
    // Only a harness whose store we actually scan can tell us a transcript is
    // gone. For every other kind the herdr session ref is all there ever was,
    // and it still resumes.
    if crate::harness::registry::has_transcript_store(&s.harness) && !s.transcript_present {
        return Tier::Gone;
    }
    let Some(last) = s.last_active_at else {
        return Tier::Warm;
    };
    let age = Utc::now().signed_duration_since(last);
    if age.num_days() <= i64::from(hot_days) {
        Tier::Hot
    } else {
        Tier::Warm
    }
}

pub(crate) fn to_millis(t: Option<DateTime<Utc>>) -> Option<i64> {
    t.map(|t| t.timestamp_millis())
}

pub(crate) fn from_millis(ms: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms)
}

pub(crate) const SESSION_COLUMNS: &str =
    "harness, id, cwd, project, title, first_prompt, started_at, last_active_at, \
     size_bytes, transcript_path, transcript_present, pinned, last_pane_json";

pub(crate) fn upsert_session(conn: &Connection, s: &Session, source: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions \
           (harness, id, cwd, project, title, first_prompt, started_at, last_active_at, \
            size_bytes, transcript_path, transcript_present, pinned, source) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
         ON CONFLICT(id) DO UPDATE SET \
           harness            = excluded.harness, \
           cwd                = CASE WHEN excluded.cwd <> '' THEN excluded.cwd ELSE sessions.cwd END, \
           project            = CASE WHEN excluded.cwd <> '' THEN excluded.project ELSE sessions.project END, \
           title              = COALESCE(excluded.title, sessions.title), \
           first_prompt       = COALESCE(excluded.first_prompt, sessions.first_prompt), \
           started_at         = COALESCE(excluded.started_at, sessions.started_at), \
           last_active_at     = MAX(COALESCE(excluded.last_active_at, 0), \
                                    COALESCE(sessions.last_active_at, 0)), \
           size_bytes         = MAX(excluded.size_bytes, sessions.size_bytes), \
           transcript_path    = CASE WHEN excluded.size_bytes >= sessions.size_bytes \
                                     THEN COALESCE(excluded.transcript_path, sessions.transcript_path) \
                                     ELSE sessions.transcript_path END, \
           transcript_present = CASE WHEN excluded.size_bytes >= sessions.size_bytes \
                                     THEN excluded.transcript_present \
                                     ELSE sessions.transcript_present END, \
           pinned             = MAX(sessions.pinned, excluded.pinned), \
           source             = excluded.source",
        rusqlite::params![
            s.harness.to_string(),
            s.id,
            s.cwd.to_string_lossy(),
            s.project,
            s.title,
            s.first_prompt,
            to_millis(s.started_at),
            to_millis(s.last_active_at),
            s.size_bytes as i64,
            s.transcript_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            s.transcript_present as i64,
            s.pinned as i64,
            source,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::HarnessKind;

    fn seeded() -> Index {
        let idx = Index::open_in_memory().expect("open");
        let mut a = Session::new(HarnessKind::Claude, "aaaaaaaa-1111", "/p/demo");
        a.title = Some("auth middleware".into());
        a.last_active_at = Some(Utc::now());
        a.transcript_present = true;
        idx.upsert(&a, SOURCE_TRANSCRIPT).expect("a");

        let mut b = Session::new(HarnessKind::Codex, "aaaaaaaa-2222", "/p/other");
        b.title = Some("something else".into());
        b.last_active_at = Some(Utc::now() - chrono::Duration::days(2));
        b.transcript_present = true;
        idx.upsert(&b, SOURCE_TRANSCRIPT).expect("b");
        idx
    }

    #[test]
    fn recent_orders_by_activity_then_pins() {
        let idx = seeded();
        let got = idx.recent(10).expect("recent");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "aaaaaaaa-1111");

        assert!(idx.pin(&HarnessKind::Codex, "aaaaaaaa-2222").expect("pin"));
        let got = idx.recent(10).expect("recent");
        assert_eq!(got[0].id, "aaaaaaaa-2222");
        assert!(got[0].pinned);

        assert!(idx
            .unpin(&HarnessKind::Codex, "aaaaaaaa-2222")
            .expect("unpin"));
        assert_eq!(idx.recent(10).expect("recent")[0].id, "aaaaaaaa-1111");
    }

    #[test]
    fn get_by_exact_id_prefix_and_ambiguity() {
        let idx = seeded();
        let s = idx.get(None, "aaaaaaaa-1111").expect("get").expect("some");
        assert_eq!(s.harness, HarnessKind::Claude);

        // Same prefix in two harnesses: narrowing resolves it.
        assert!(matches!(
            idx.get(None, "aaaaaaaa"),
            Err(Error::AmbiguousId(_))
        ));
        let s = idx
            .get(Some(&HarnessKind::Codex), "aaaaaaaa")
            .expect("get")
            .expect("some");
        assert_eq!(s.id, "aaaaaaaa-2222");

        assert!(idx.get(None, "zzzz").expect("get").is_none());
    }

    #[test]
    fn tier_follows_transcript_presence_and_age() {
        let idx = Index::open_in_memory().expect("open");
        let mut gone = Session::new(HarnessKind::Claude, "gone", "/p/demo");
        gone.last_active_at = Some(Utc::now());
        gone.transcript_present = false;
        idx.upsert(&gone, SOURCE_TRANSCRIPT).expect("gone");

        let mut warm = Session::new(HarnessKind::Claude, "warm", "/p/demo");
        warm.last_active_at = Some(Utc::now() - chrono::Duration::days(120));
        warm.transcript_present = true;
        idx.upsert(&warm, SOURCE_TRANSCRIPT).expect("warm");

        let by_id = |id: &str| idx.get(None, id).expect("get").expect("some").tier;
        assert_eq!(by_id("gone"), Tier::Gone);
        assert_eq!(by_id("warm"), Tier::Warm);
    }

    #[test]
    fn upsert_keeps_what_the_new_row_does_not_know() {
        let idx = Index::open_in_memory().expect("open");
        let mut full = Session::new(HarnessKind::Claude, "x", "/p/demo");
        full.title = Some("kept".into());
        full.first_prompt = Some("kept prompt".into());
        full.transcript_present = true;
        full.last_active_at = Some(Utc::now());
        idx.upsert(&full, SOURCE_TRANSCRIPT).expect("full");

        let stub = Session::new(HarnessKind::Claude, "x", "");
        idx.upsert(&stub, SOURCE_HERDR).expect("stub");

        let s = idx.get(None, "x").expect("get").expect("some");
        assert_eq!(s.title.as_deref(), Some("kept"));
        assert_eq!(s.first_prompt.as_deref(), Some("kept prompt"));
        assert_eq!(s.cwd, PathBuf::from("/p/demo"));
    }
}
