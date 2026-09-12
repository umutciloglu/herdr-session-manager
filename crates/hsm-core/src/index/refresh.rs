use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};

use crate::domain::{project_of, HarnessKind, PaneRef, ProcessKind, Session};
use crate::error::Result;
use crate::harness::claude_registry::{self, RunningRef};
use crate::harness::herdr_refs::{self, HerdrRef};
use crate::harness::message::ExtractedMessage;
use crate::harness::{claude, codex, jsonl, registry};
use crate::index::{
    to_millis, upsert_session, Index, LAST_REFRESH_KEY, SOURCE_HERDR, SOURCE_REGISTRY,
    SOURCE_TRANSCRIPT,
};
use crate::live::{LivePane, LiveSessions};
use crate::paths;

pub struct RefreshOptions<'a> {
    /// Sessions active within this many days get their messages indexed.
    pub hot_days: u32,
    /// Re-read every transcript from byte 0 and rebuild the message index.
    pub full: bool,
    pub disabled: &'a [HarnessKind],
    pub live: &'a dyn LiveSessions,
    /// Extra directories holding Claude-shaped transcripts (config).
    pub extra_transcript_roots: &'a [PathBuf],
    /// Replaces the built-in store locations instead of adding to them. Set it
    /// to point the scan at a fixture tree; Codex then falls back to reading
    /// rollouts rather than its own sqlite index.
    pub store_roots_override: Option<&'a [PathBuf]>,
    /// Where Claude's running-process registry lives. `None` is
    /// `claude_registry::registry_dir()`.
    pub registry_dir_override: Option<&'a Path>,
}

impl<'a> RefreshOptions<'a> {
    pub fn new(live: &'a dyn LiveSessions) -> Self {
        RefreshOptions {
            hot_days: 30,
            full: false,
            disabled: &[],
            live,
            extra_transcript_roots: &[],
            store_roots_override: None,
            registry_dir_override: None,
        }
    }

    fn enabled(&self, kind: &HarnessKind) -> bool {
        !self.disabled.contains(kind)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshReport {
    pub files_scanned: u64,
    pub files_parsed: u64,
    pub sessions_upserted: u64,
    pub messages_indexed: u64,
    pub panes_seen: u64,
    /// Live harness processes with no pane of their own.
    pub processes_seen: u64,
    pub marked_gone: u64,
    pub elapsed_ms: u128,
    /// Per-file problems. A refresh never fails because one transcript is bad.
    pub errors: Vec<String>,
}

impl Index {
    pub fn refresh(&mut self, opts: &RefreshOptions<'_>) -> Result<RefreshReport> {
        let start = Instant::now();
        self.set_hot_days(opts.hot_days)?;

        let mut report = RefreshReport::default();
        let tx = self.conn.transaction()?;

        if opts.full {
            tx.execute("DELETE FROM scan_state", [])?;
            tx.execute("DELETE FROM messages", [])?;
        }
        clear_live_flags(&tx)?;
        // Nothing in a process ref is worth preserving between passes: it is
        // rebuilt from the registry below, or it is gone.
        tx.execute(
            "UPDATE sessions SET process_json = NULL WHERE process_json IS NOT NULL",
            [],
        )?;

        let hot_cutoff_ms =
            (Utc::now() - chrono::Duration::days(i64::from(opts.hot_days))).timestamp_millis();

        if opts.enabled(&HarnessKind::Claude) {
            claude_pass(&tx, opts, hot_cutoff_ms, &mut report);
        }
        if opts.enabled(&HarnessKind::Codex) {
            codex_pass(&tx, opts, hot_cutoff_ms, &mut report);
        }

        if let Some(p) = paths::herdr_session_file() {
            match herdr_refs::from_session_file(&p) {
                Ok(refs) => {
                    for r in refs {
                        if !opts.enabled(&r.session.harness) {
                            continue;
                        }
                        upsert_pane(&tx, &r)?;
                        report.panes_seen += 1;
                    }
                }
                Err(e) => report.errors.push(format!("{}: {e}", p.display())),
            }
        }

        // Taken once: the registry pass below needs the same snapshot to work
        // out which pane is showing which job.
        let live_panes = opts.live.live();
        for pane in &live_panes {
            let r = HerdrRef::from(pane);
            if !opts.enabled(&r.session.harness) {
                continue;
            }
            upsert_pane(&tx, &r)?;
            report.panes_seen += 1;
        }

        if opts.enabled(&HarnessKind::Claude) {
            if let Some(dir) = registry_dir(opts) {
                for mut r in claude_registry::running(&dir) {
                    r.process.pane_id = pane_showing(&r, &live_panes);
                    upsert_process(&tx, &r)?;
                    report.processes_seen += 1;
                }
            }
        }

        report.marked_gone = mark_gone(&tx)?;
        tx.commit()?;

        // Stamped after the commit so the mark only ever claims work that is
        // actually in the database. Callers on their own schedule read it back
        // through `Index::last_refresh` to skip a scan that just happened.
        self.put_meta(LAST_REFRESH_KEY, &Utc::now().timestamp_millis().to_string())?;

        report.elapsed_ms = start.elapsed().as_millis();
        Ok(report)
    }
}

fn claude_pass(
    conn: &Connection,
    opts: &RefreshOptions<'_>,
    hot_cutoff_ms: i64,
    report: &mut RefreshReport,
) {
    let mut roots = match opts.store_roots_override {
        Some(r) => r.to_vec(),
        None => registry::transcript_roots(&HarnessKind::Claude),
    };
    roots.extend(opts.extra_transcript_roots.iter().cloned());
    for path in claude::transcripts(&roots) {
        if let Err(e) = scan_one(
            conn,
            &path,
            opts.full,
            hot_cutoff_ms,
            report,
            claude::parse_transcript,
            claude::extract_messages,
        ) {
            report.errors.push(format!("{}: {e}", path.display()));
        }
    }
}

fn codex_pass(
    conn: &Connection,
    opts: &RefreshOptions<'_>,
    hot_cutoff_ms: i64,
    report: &mut RefreshReport,
) {
    let db = match opts.store_roots_override {
        Some(_) => None,
        None => registry::codex_state_db().filter(|p| p.exists()),
    };
    let from_db = match &db {
        Some(p) => match codex::from_state_db(p) {
            Ok(sessions) => Some(sessions),
            Err(e) => {
                report.errors.push(format!("{}: {e}", p.display()));
                None
            }
        },
        None => None,
    };

    // Codex's own index is authoritative for metadata; rollouts are only the
    // fallback when it is missing or unreadable.
    match from_db {
        Some(sessions) => {
            for mut s in sessions {
                report.files_parsed += 1;
                if let Some(p) = s.transcript_path.clone() {
                    s.transcript_present = p.exists();
                }
                if let Err(e) = upsert_session(conn, &s, SOURCE_TRANSCRIPT) {
                    report.errors.push(format!("{}: {e}", s.id));
                    continue;
                }
                report.sessions_upserted += 1;
                let hot =
                    s.last_active_at.map(|t| t.timestamp_millis()).unwrap_or(0) >= hot_cutoff_ms;
                if !hot || !s.transcript_present {
                    continue;
                }
                let Some(path) = s.transcript_path.as_ref() else {
                    continue;
                };
                if let Err(e) =
                    index_messages(conn, path, &s, opts.full, report, codex::extract_messages)
                {
                    report.errors.push(format!("{}: {e}", path.display()));
                }
            }
        }
        None => {
            let roots = match opts.store_roots_override {
                Some(r) => r.to_vec(),
                None => registry::transcript_roots(&HarnessKind::Codex),
            };
            for path in codex::rollouts(&roots) {
                if let Err(e) = scan_one(
                    conn,
                    &path,
                    opts.full,
                    hot_cutoff_ms,
                    report,
                    codex::parse_rollout,
                    codex::extract_messages,
                ) {
                    report.errors.push(format!("{}: {e}", path.display()));
                }
            }
        }
    }
}

type Parse = fn(&Path) -> Result<Option<Session>>;
type Extract = fn(&Path, u64) -> Result<(Vec<ExtractedMessage>, u64)>;

/// One transcript file: reparse metadata only when mtime/size moved, then top
/// up the message index if the session is hot.
fn scan_one(
    conn: &Connection,
    path: &Path,
    full: bool,
    hot_cutoff_ms: i64,
    report: &mut RefreshReport,
    parse: Parse,
    extract: Extract,
) -> Result<()> {
    report.files_scanned += 1;
    let stat = jsonl::stat(path)?;
    let prev = scan_state(conn, path)?;
    let unchanged = prev
        .as_ref()
        .is_some_and(|p| p.mtime == stat.mtime_ms && p.size == stat.size);
    if unchanged && !full {
        return Ok(());
    }

    let Some(session) = parse(path)? else {
        save_scan_state(conn, path, prev.map(|p| p.byte_offset).unwrap_or(0), &stat)?;
        return Ok(());
    };
    report.files_parsed += 1;
    upsert_session(conn, &session, SOURCE_TRANSCRIPT)?;
    report.sessions_upserted += 1;

    let hot = session
        .last_active_at
        .map(|t| t.timestamp_millis())
        .unwrap_or(0)
        >= hot_cutoff_ms;
    if hot {
        index_messages(conn, path, &session, full, report, extract)?;
    } else {
        save_scan_state(conn, path, prev.map(|p| p.byte_offset).unwrap_or(0), &stat)?;
    }
    Ok(())
}

fn index_messages(
    conn: &Connection,
    path: &Path,
    session: &Session,
    full: bool,
    report: &mut RefreshReport,
    extract: Extract,
) -> Result<()> {
    let stat = jsonl::stat(path)?;
    let prev = scan_state(conn, path)?;
    let offset = if full {
        0
    } else {
        prev.as_ref().map(|p| p.byte_offset).unwrap_or(0)
    };
    let (messages, new_offset) = extract(path, offset)?;

    if !messages.is_empty() {
        let first_seq = next_seq(conn, &session.harness, &session.id)?;
        let mut stmt = conn.prepare_cached(
            "INSERT OR IGNORE INTO messages (harness, id, seq, role, ts, text) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let harness = session.harness.to_string();
        for (seq, m) in (first_seq..).zip(messages.iter()) {
            stmt.execute(rusqlite::params![
                harness,
                session.id,
                seq,
                m.role.as_str(),
                to_millis(m.ts),
                m.text,
            ])?;
        }
        report.messages_indexed += messages.len() as u64;
    }
    save_scan_state(conn, path, new_offset, &stat)
}

struct ScanState {
    byte_offset: u64,
    mtime: i64,
    size: u64,
}

fn scan_state(conn: &Connection, path: &Path) -> Result<Option<ScanState>> {
    let row = conn
        .query_row(
            "SELECT byte_offset, mtime, size FROM scan_state WHERE path = ?1",
            [path.to_string_lossy()],
            |r| {
                Ok(ScanState {
                    byte_offset: r.get::<_, i64>(0)?.max(0) as u64,
                    mtime: r.get(1)?,
                    size: r.get::<_, i64>(2)?.max(0) as u64,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn save_scan_state(
    conn: &Connection,
    path: &Path,
    byte_offset: u64,
    stat: &jsonl::FileStat,
) -> Result<()> {
    conn.execute(
        "INSERT INTO scan_state (path, byte_offset, mtime, size) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(path) DO UPDATE SET \
           byte_offset = excluded.byte_offset, mtime = excluded.mtime, size = excluded.size",
        rusqlite::params![
            path.to_string_lossy(),
            byte_offset as i64,
            stat.mtime_ms,
            stat.size as i64
        ],
    )?;
    Ok(())
}

fn next_seq(conn: &Connection, harness: &HarnessKind, id: &str) -> Result<i64> {
    let n: Option<i64> = conn.query_row(
        "SELECT MAX(seq) FROM messages WHERE harness = ?1 AND id = ?2",
        rusqlite::params![harness.to_string(), id],
        |r| r.get(0),
    )?;
    Ok(n.unwrap_or(-1) + 1)
}

/// Where to look for Claude's running processes this pass.
fn registry_dir(opts: &RefreshOptions<'_>) -> Option<PathBuf> {
    match (opts.registry_dir_override, opts.store_roots_override) {
        (Some(dir), _) => Some(dir.to_path_buf()),
        // Same rule as the codex state db: once the scan is pointed at a
        // fixture tree, this machine's own stores are off limits.
        (None, Some(_)) => None,
        (None, None) => claude_registry::registry_dir(),
    }
}

/// The pane whose interactive agent is showing this job right now. While a
/// Claude displays a job the cli writes the job's name into the terminal title,
/// which herdr reports back as the pane title, so an exact match is the pane to
/// jump to. Only jobs are matched, and never against a pane running the job's
/// own session id: a job that was resumed interactively would otherwise be
/// found looking at itself.
fn pane_showing(job: &RunningRef, live: &[LivePane]) -> Option<String> {
    if job.process.kind != ProcessKind::Job {
        return None;
    }
    let name = job.process.name.as_deref()?;
    live.iter()
        .find(|p| p.title.as_deref() == Some(name) && p.session.value != job.session_id)
        .map(|p| p.pane_id.clone())
}

/// The process is a snapshot, so this only ever writes `process_json`. The row
/// itself is created when the registry is the first place we hear of a session
/// — a job that has not written a transcript yet.
fn upsert_process(conn: &Connection, r: &RunningRef) -> Result<()> {
    let process_json = serde_json::to_string(&r.process).unwrap_or_else(|_| "null".to_string());
    let cwd = r.cwd.to_string_lossy().into_owned();
    let project = project_of(&r.cwd);

    conn.execute(
        "INSERT INTO sessions \
           (harness, id, cwd, project, title, process_json, source, transcript_present) \
         VALUES ('claude', ?1, ?2, ?3, ?4, ?5, ?6, 0) \
         ON CONFLICT(id) DO UPDATE SET \
           process_json = excluded.process_json, \
           cwd     = CASE WHEN sessions.cwd = '' THEN excluded.cwd ELSE sessions.cwd END, \
           project = CASE WHEN sessions.project = '' THEN excluded.project ELSE sessions.project END, \
           title   = COALESCE(sessions.title, excluded.title)",
        // Claude transcripts carry accurate timestamps, so a running process
        // never bumps last_active_at (same reasoning as `upsert_pane`).
        rusqlite::params![
            r.session_id,
            cwd,
            project,
            r.process.name,
            process_json,
            SOURCE_REGISTRY,
        ],
    )?;
    Ok(())
}

/// Tier-1 row: a pane ref is all we know, so it must not overwrite anything a
/// transcript parse already established.
fn upsert_pane(conn: &Connection, r: &HerdrRef) -> Result<()> {
    let pane_json = serde_json::to_string(&r.pane).unwrap_or_else(|_| "null".to_string());
    let cwd = r.cwd.to_string_lossy().into_owned();
    let project = project_of(&r.cwd);
    // A live pane is the only activity signal for a harness whose store we do
    // not read. Where we do read one, its timestamps are accurate and must not
    // be overwritten with "now" just because the pane is open.
    let last_active = (r.pane.live && !registry::has_transcript_store(&r.session.harness))
        .then(|| Utc::now().timestamp_millis());

    conn.execute(
        "INSERT INTO sessions \
           (harness, id, cwd, project, title, last_pane_json, source, last_active_at, \
            transcript_present) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0) \
         ON CONFLICT(id) DO UPDATE SET \
           last_pane_json = excluded.last_pane_json, \
           cwd     = CASE WHEN sessions.cwd = '' THEN excluded.cwd ELSE sessions.cwd END, \
           project = CASE WHEN sessions.project = '' THEN excluded.project ELSE sessions.project END, \
           title   = COALESCE(sessions.title, excluded.title), \
           last_active_at = MAX(COALESCE(sessions.last_active_at, 0), \
                                COALESCE(excluded.last_active_at, 0))",
        rusqlite::params![
            r.session.harness.to_string(),
            r.session.value,
            cwd,
            project,
            r.label,
            pane_json,
            SOURCE_HERDR,
            last_active,
        ],
    )?;
    Ok(())
}

/// Liveness is a property of the last snapshot, never of the stored row.
fn clear_live_flags(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, last_pane_json FROM sessions \
         WHERE last_pane_json IS NOT NULL AND instr(last_pane_json, '\"live\":true') > 0",
    )?;
    let rows: HashMap<String, String> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);

    for (id, json) in rows {
        let Ok(mut pane) = serde_json::from_str::<PaneRef>(&json) else {
            continue;
        };
        pane.live = false;
        pane.status = None;
        let Ok(updated) = serde_json::to_string(&pane) else {
            continue;
        };
        conn.execute(
            "UPDATE sessions SET last_pane_json = ?2 WHERE id = ?1",
            rusqlite::params![id, updated],
        )?;
    }
    Ok(())
}

/// A row whose transcript vanished (Claude prunes after `cleanupPeriodDays`)
/// stays searchable but becomes `Gone`.
fn mark_gone(conn: &Connection) -> Result<u64> {
    let mut stmt = conn.prepare(
        "SELECT id, transcript_path FROM sessions \
         WHERE transcript_present = 1 AND transcript_path IS NOT NULL",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);

    let mut n = 0;
    for (id, path) in rows {
        if Path::new(&path).exists() {
            continue;
        }
        conn.execute(
            "UPDATE sessions SET transcript_present = 0 WHERE id = ?1",
            [&id],
        )?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RefKind, SessionRef, Tier};
    use crate::live::NoLive;

    struct FakeLive(Vec<LivePane>);

    impl LiveSessions for FakeLive {
        fn live(&self) -> Vec<LivePane> {
            self.0.clone()
        }
    }

    #[test]
    fn live_panes_create_tier_one_rows_and_clear_on_the_next_pass() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut idx = Index::open(&dir.path().join("index.sqlite")).expect("open");

        let pane = LivePane {
            pane_id: "7".into(),
            workspace_id: Some("w2".into()),
            tab_id: Some("1".into()),
            agent: "codex".into(),
            session: SessionRef {
                harness: HarnessKind::Codex,
                kind: RefKind::Id,
                value: "019ffa5d-live".into(),
            },
            cwd: PathBuf::from("/Users/x/Projects/demo"),
            title: Some("gap recovery".into()),
            status: "working".into(),
        };
        let live = FakeLive(vec![pane]);
        let opts = RefreshOptions {
            // Nothing on this machine should be scanned by this test.
            disabled: &[HarnessKind::Claude, HarnessKind::Codex],
            ..RefreshOptions::new(&live)
        };
        // The disabled list also gates pane rows, so re-enable codex for panes.
        let opts = RefreshOptions {
            disabled: &[HarnessKind::Claude],
            ..opts
        };
        let report = idx.refresh(&opts).expect("refresh");
        assert!(report.panes_seen >= 1);

        let s = idx.get(None, "019ffa5d-live").expect("get").expect("row");
        assert_eq!(s.project, "demo");
        assert_eq!(s.title.as_deref(), Some("gap recovery"));
        assert_eq!(s.tier, Tier::Gone, "a pane ref alone has no transcript");
        assert!(s.is_live());

        // herdr is gone on the next pass: the pane is remembered, liveness is not.
        let none = NoLive;
        let opts = RefreshOptions {
            disabled: &[HarnessKind::Claude, HarnessKind::Codex],
            ..RefreshOptions::new(&none)
        };
        idx.refresh(&opts).expect("refresh");
        let s = idx.get(None, "019ffa5d-live").expect("get").expect("row");
        assert!(!s.is_live());
        assert_eq!(s.last_pane.map(|p| p.pane_id), Some("7".to_string()));
        // Codex transcripts carry real timestamps, so an open pane never
        // invents one.
        assert!(s.last_active_at.is_none());
    }

    /// A harness we have no transcript store for is known only from herdr. It
    /// must still resume with the id herdr gave us.
    #[test]
    fn a_herdr_only_session_for_an_unscanned_harness_still_resumes() {
        use crate::domain::SessionCard;
        use crate::open::command_for;

        let dir = tempfile::tempdir().expect("tmp");
        let mut idx = Index::open(&dir.path().join("index.sqlite")).expect("open");

        let live = FakeLive(vec![LivePane {
            pane_id: "12".into(),
            workspace_id: Some("w3".into()),
            tab_id: Some("0".into()),
            agent: "pi".into(),
            session: SessionRef {
                harness: HarnessKind::Pi,
                kind: RefKind::Id,
                value: "pi-session-0001".into(),
            },
            cwd: PathBuf::from("/Users/x/Projects/demo"),
            title: Some("pairing".into()),
            status: "idle".into(),
        }]);
        let roots: Vec<PathBuf> = Vec::new();
        let opts = RefreshOptions {
            store_roots_override: Some(&roots),
            ..RefreshOptions::new(&live)
        };
        idx.refresh(&opts).expect("refresh");

        let s = idx.get(None, "pi-session-0001").expect("get").expect("row");
        assert_eq!(
            s.tier,
            Tier::Hot,
            "a live pane with no scanned store is not Gone"
        );
        assert_eq!(
            command_for(&s),
            (
                "pi".to_string(),
                vec!["--session".to_string(), "pi-session-0001".to_string()]
            )
        );
        assert!(SessionCard::from(&s).resumable);

        // Liveness drops but the session stays resumable.
        let none = NoLive;
        let opts = RefreshOptions {
            store_roots_override: Some(&roots),
            ..RefreshOptions::new(&none)
        };
        idx.refresh(&opts).expect("refresh");
        let s = idx.get(None, "pi-session-0001").expect("get").expect("row");
        assert_ne!(s.tier, Tier::Gone);
        assert!(SessionCard::from(&s).resumable);
        assert_eq!(command_for(&s).1.len(), 2);
    }

    const USER_LINE: &str = concat!(
        r#"{"type":"user","message":{"role":"user","content":"index me"},"#,
        r#""timestamp":"2026-09-11T10:00:00.000Z","cwd":"/Users/x/Projects/demo","#,
        r#""sessionId":"s1"}"#,
        "\n"
    );

    const ASSISTANT_LINE: &str = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","#,
        r#""content":[{"type":"text","text":"on it"}]},"#,
        r#""timestamp":"2026-09-11T10:01:00.000Z","sessionId":"s1"}"#,
        "\n"
    );

    fn fixture_store(name: &str, body: &str) -> tempfile::TempDir {
        let store = tempfile::tempdir().expect("tmp");
        let proj = store.path().join("-Users-x-Projects-demo");
        std::fs::create_dir_all(&proj).expect("mkdir");
        std::fs::write(proj.join(name), body).expect("write");
        store
    }

    /// Claude's process registry, one live entry per `(session id, kind)`. Our
    /// own pid is the only one guaranteed to be alive while the test runs.
    fn fixture_registry(entries: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tmp");
        let pid = std::process::id();
        for (n, (session_id, kind)) in entries.iter().enumerate() {
            let body = format!(
                r#"{{"pid":{pid},"sessionId":"{session_id}","cwd":"/Users/x/Projects/demo",
                    "kind":"{kind}","status":"busy","name":"a background job"}}"#
            );
            std::fs::write(dir.path().join(format!("{pid}-{n}.json")), body).expect("write");
        }
        dir
    }

    #[test]
    fn stores_are_scanned_once_and_rescans_are_incremental() {
        let store = fixture_store("11111111-2222-3333-4444-555555555555.jsonl", USER_LINE);
        let file = store
            .path()
            .join("-Users-x-Projects-demo")
            .join("11111111-2222-3333-4444-555555555555.jsonl");

        let dir = tempfile::tempdir().expect("tmp");
        let mut idx = Index::open(&dir.path().join("index.sqlite")).expect("open");
        let none = NoLive;
        let roots = vec![store.path().to_path_buf()];
        let opts = RefreshOptions {
            hot_days: 3650,
            disabled: &[HarnessKind::Codex],
            store_roots_override: Some(&roots),
            ..RefreshOptions::new(&none)
        };

        let first = idx.refresh(&opts).expect("refresh");
        assert_eq!(first.files_scanned, 1);
        assert_eq!(first.files_parsed, 1);
        assert_eq!(first.messages_indexed, 1);
        let s = idx.get(None, "s1").expect("get").expect("row");
        assert_eq!(s.first_prompt.as_deref(), Some("index me"));

        let second = idx.refresh(&opts).expect("refresh");
        assert_eq!(second.files_scanned, 1);
        assert_eq!(second.files_parsed, 0, "unchanged files are not reparsed");
        assert_eq!(
            idx.message_count().expect("count"),
            1,
            "no duplicate messages"
        );

        // An appended turn is read from the stored byte offset, not from zero.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("open");
        f.write_all(ASSISTANT_LINE.as_bytes()).expect("append");
        drop(f);

        let third = idx.refresh(&opts).expect("refresh");
        assert_eq!(third.messages_indexed, 1, "only the new turn is read");
        assert_eq!(idx.message_count().expect("count"), 2);
    }

    #[test]
    fn a_deleted_transcript_becomes_gone() {
        let store = fixture_store("22222222-2222-3333-4444-555555555555.jsonl", USER_LINE);
        let file = store
            .path()
            .join("-Users-x-Projects-demo")
            .join("22222222-2222-3333-4444-555555555555.jsonl");

        let dir = tempfile::tempdir().expect("tmp");
        let mut idx = Index::open(&dir.path().join("index.sqlite")).expect("open");
        let none = NoLive;
        let roots = vec![store.path().to_path_buf()];
        let opts = RefreshOptions {
            hot_days: 3650,
            disabled: &[HarnessKind::Codex],
            store_roots_override: Some(&roots),
            ..RefreshOptions::new(&none)
        };
        idx.refresh(&opts).expect("refresh");
        assert_ne!(
            idx.get(None, "s1").expect("get").expect("row").tier,
            Tier::Gone
        );

        std::fs::remove_file(&file).expect("rm");
        let report = idx.refresh(&opts).expect("refresh");
        assert_eq!(report.marked_gone, 1);
        assert_eq!(
            idx.get(None, "s1").expect("get").expect("row").tier,
            Tier::Gone
        );
    }

    /// A Claude background job runs as its own process and never gets a pane,
    /// so the registry is the only place its liveness shows up.
    #[test]
    fn a_running_process_marks_its_session_and_clears_on_the_next_pass() {
        let store = fixture_store("33333333-2222-3333-4444-555555555555.jsonl", USER_LINE);
        let registry = fixture_registry(&[("s1", "bg"), ("no-transcript-yet", "interactive")]);
        let gone = tempfile::tempdir().expect("tmp");

        let dir = tempfile::tempdir().expect("tmp");
        let mut idx = Index::open(&dir.path().join("index.sqlite")).expect("open");
        let none = NoLive;
        let roots = vec![store.path().to_path_buf()];
        let opts = RefreshOptions {
            hot_days: 3650,
            disabled: &[HarnessKind::Codex],
            store_roots_override: Some(&roots),
            registry_dir_override: Some(registry.path()),
            ..RefreshOptions::new(&none)
        };

        let report = idx.refresh(&opts).expect("refresh");
        assert_eq!(report.processes_seen, 2);

        let s = idx.get(None, "s1").expect("get").expect("row");
        let p = s.process.clone().expect("process");
        assert_eq!(p.kind, ProcessKind::Job);
        assert_eq!(p.status.as_deref(), Some("busy"));
        assert!(s.is_running(), "alive");
        assert!(!s.is_live(), "but there is no pane to jump to");
        assert_eq!(
            s.first_prompt.as_deref(),
            Some("index me"),
            "what the transcript pass found is kept"
        );

        // A job that has not written a transcript yet is known only here.
        let fresh = idx
            .get(None, "no-transcript-yet")
            .expect("get")
            .expect("row");
        assert_eq!(fresh.harness, HarnessKind::Claude);
        assert_eq!(fresh.project, "demo");
        assert_eq!(fresh.title.as_deref(), Some("a background job"));
        assert_eq!(
            fresh.process.map(|p| p.kind),
            Some(ProcessKind::Interactive)
        );
        assert!(
            fresh.last_active_at.is_none(),
            "the registry carries no timestamps"
        );

        // The processes end, and the marks go with them.
        let opts = RefreshOptions {
            registry_dir_override: Some(gone.path()),
            ..opts
        };
        assert_eq!(idx.refresh(&opts).expect("refresh").processes_seen, 0);
        assert!(idx
            .get(None, "s1")
            .expect("get")
            .expect("row")
            .process
            .is_none());
    }

    /// A pane showing a job, as herdr reports it: the interactive agent's
    /// terminal title is the job's name while the job is on screen.
    fn watching_pane(pane_id: &str, session_id: &str, title: &str) -> LivePane {
        LivePane {
            pane_id: pane_id.into(),
            workspace_id: Some("w9".into()),
            tab_id: Some("w9:t1".into()),
            agent: "claude".into(),
            session: SessionRef {
                harness: HarnessKind::Claude,
                kind: RefKind::Id,
                value: session_id.into(),
            },
            cwd: PathBuf::from("/Users/x/Projects/demo"),
            title: Some(title.into()),
            status: "working".into(),
        }
    }

    #[test]
    fn a_job_is_linked_to_the_pane_whose_title_is_its_name() {
        let registry = fixture_registry(&[("job-1", "bg"), ("run-1", "interactive")]);
        let dir = tempfile::tempdir().expect("tmp");
        let mut idx = Index::open(&dir.path().join("index.sqlite")).expect("open");
        let roots: Vec<PathBuf> = Vec::new();
        let refresh = |idx: &mut Index, live: &FakeLive| {
            let opts = RefreshOptions {
                disabled: &[HarnessKind::Codex],
                store_roots_override: Some(&roots),
                registry_dir_override: Some(registry.path()),
                ..RefreshOptions::new(live)
            };
            idx.refresh(&opts).expect("refresh");
        };

        let live = FakeLive(vec![
            watching_pane("w9:p7", "2c36e684-watching", "a background job"),
            watching_pane("w9:p9", "1f0b1d55-elsewhere", "something else"),
        ]);
        refresh(&mut idx, &live);

        let job = idx.get(None, "job-1").expect("get").expect("row");
        assert_eq!(
            job.process.as_ref().expect("process").pane_id.as_deref(),
            Some("w9:p7")
        );
        assert_eq!(job.jump_pane(), Some("w9:p7"), "Enter goes to the watcher");

        // Same name, but a person is already typing at it: an interactive
        // process is not something a pane can be showing.
        let run = idx.get(None, "run-1").expect("get").expect("row");
        assert_eq!(run.process.expect("process").pane_id, None);

        // A job resumed in a pane of its own is listed first, so only the id
        // check keeps it from matching itself.
        let live = FakeLive(vec![
            watching_pane("w9:p8", "job-1", "a background job"),
            watching_pane("w9:p7", "2c36e684-watching", "a background job"),
        ]);
        refresh(&mut idx, &live);
        let job = idx.get(None, "job-1").expect("get").expect("row");
        assert_eq!(
            job.process.expect("process").pane_id.as_deref(),
            Some("w9:p7")
        );

        // Nobody is watching any more.
        let live = FakeLive(vec![watching_pane("w9:p8", "job-1", "a background job")]);
        refresh(&mut idx, &live);
        let job = idx.get(None, "job-1").expect("get").expect("row");
        assert_eq!(job.process.expect("process").pane_id, None);
    }

    /// The provider skips a scan that just happened, so the stamp has to
    /// survive a round trip through the `meta` table.
    #[test]
    fn refresh_stamps_last_refresh_and_it_round_trips() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("index.sqlite");
        let mut idx = Index::open(&path).expect("open");
        assert!(idx.last_refresh().is_none(), "never scanned");

        let none = NoLive;
        let opts = RefreshOptions {
            // Nothing on this machine should be scanned by this test.
            disabled: &[HarnessKind::Claude, HarnessKind::Codex],
            ..RefreshOptions::new(&none)
        };
        let before = Utc::now() - chrono::Duration::seconds(1);
        idx.refresh(&opts).expect("refresh");
        let first = idx.last_refresh().expect("stamped by refresh");
        assert!(first >= before);

        // A reopened index reads it back out of the table, not out of memory.
        drop(idx);
        let idx = Index::open(&path).expect("reopen");
        assert_eq!(idx.last_refresh(), Some(first));
    }
}
