//! `hsm sessions` — the agentmail session provider (docs/protocol.md) and a
//! plain listing for humans.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use hsm_core::{HarnessKind, Index, Query, Session, SessionCard};
use serde_json::json;

use crate::commands::index::refresh;
use crate::commands::{split_address, state_word};
use crate::runtime::Ctx;

/// The provider caps what it asks for; this caps what we are willing to build.
const MAX_LIMIT: usize = 500;
/// docs/protocol.md: a prefix resolves from 8 characters up.
const MIN_PREFIX: usize = 8;
/// How stale the index may be before a provider call pays for a scan. agentmail
/// reads a card per resource, so a burst of them must cost one refresh, not one
/// each; `browse` and `startup` still refresh unconditionally.
const PROVIDER_MAX_AGE: Duration = Duration::from_secs(10);

pub fn run(
    ctx: &Ctx,
    json_out: bool,
    query: Option<&str>,
    limit: usize,
    harness: Option<&str>,
    project: Option<&str>,
) -> Result<()> {
    let mut index = ctx.index()?;
    // agentmail calls this without the startup hook having run, and liveness is
    // stored rather than computed, so bring the index up to date first. A failed
    // refresh only costs freshness; the listing still answers.
    let refreshed = refresh_if_stale(&mut index, Utc::now(), |index| {
        let live = ctx.live(ctx.herdr());
        refresh(index, &ctx.config, false, live.as_ref()).map(|_| ())
    });
    if let Err(error) = refreshed {
        tracing::warn!(%error, "could not refresh the index");
    }
    let limit = limit.clamp(1, MAX_LIMIT);
    let rows = find(&index, query, limit, harness, project)?;

    if json_out {
        let cards: Vec<SessionCard> = rows.iter().map(SessionCard::from).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "sessions": cards }))?
        );
    } else {
        print_table(&rows);
    }
    Ok(())
}

/// Runs `refresh` unless the index was already scanned inside the window.
/// Returns whether it ran. The work is a closure so a fresh index also skips
/// connecting to herdr, which is the other half of the per-call cost.
fn refresh_if_stale(
    index: &mut Index,
    now: DateTime<Utc>,
    refresh: impl FnOnce(&mut Index) -> Result<()>,
) -> Result<bool> {
    let fresh = index
        .last_refresh()
        .and_then(|last| (now - last).to_std().ok())
        .is_some_and(|age| age < PROVIDER_MAX_AGE);
    if fresh {
        return Ok(false);
    }
    refresh(index)?;
    Ok(true)
}

fn find(
    index: &Index,
    query: Option<&str>,
    limit: usize,
    harness: Option<&str>,
    project: Option<&str>,
) -> Result<Vec<Session>> {
    let q = Query {
        text: query.unwrap_or_default().to_string(),
        harness: harness.map(HarnessKind::from_name),
        project: project.map(str::to_string),
        // `project` filters below, so ask for enough rows to filter from.
        limit: if project.is_some() { limit * 8 } else { limit },
    };
    let mut rows = index.search(&q)?;
    if let Some(p) = project {
        rows.retain(|s| s.project.eq_ignore_ascii_case(p));
    }
    // An exact address or id prefix outranks anything the text search found:
    // the caller named a session, it did not describe one.
    if let Some(hit) = exact_hit(index, query.unwrap_or_default(), harness, project) {
        rows.retain(|s| !(s.harness == hit.harness && s.id == hit.id));
        rows.insert(0, hit);
    }
    rows.truncate(limit);
    Ok(rows)
}

/// The query as a session reference rather than as prose. agentmail fills a
/// card this way: `hsm sessions --json --query <address> --limit 1`.
fn exact_hit(
    index: &Index,
    query: &str,
    harness: Option<&str>,
    project: Option<&str>,
) -> Option<Session> {
    let query = query.trim();
    let (kind, id) = split_address(query);
    if id.is_empty() || id.contains(char::is_whitespace) {
        return None;
    }
    // A bare token has to look like an id; with an explicit `<harness>:` the
    // caller already said it is one.
    if kind.is_none() && !looks_like_id(&id) {
        return None;
    }

    // An ambiguous prefix is not an exact hit, and neither is a sqlite hiccup:
    // in both cases the ranked rows stand on their own.
    let hit = index.get(kind.as_ref(), &id).ok().flatten()?;
    if harness.is_some_and(|h| hit.harness != HarnessKind::from_name(h)) {
        return None;
    }
    if project.is_some_and(|p| !hit.project.eq_ignore_ascii_case(p)) {
        return None;
    }
    Some(hit)
}

/// Hex, uuid or ULID shaped and at least 8 characters — what a session id looks
/// like in every harness we index, and what docs/protocol.md accepts as a
/// prefix. Prose cannot reach this by accident: one non-hex letter is enough to
/// disqualify it.
fn looks_like_id(s: &str) -> bool {
    if s.len() < MIN_PREFIX {
        return false;
    }
    let hex_or_uuid = s.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let ulid = s.len() == 26
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() && !"ILOUilou".contains(c));
    hex_or_uuid || ulid
}

fn print_table(rows: &[Session]) {
    if rows.is_empty() {
        println!("no sessions (run `hsm index`)");
        return;
    }
    let now = Utc::now();
    for s in rows {
        println!(
            "{:<7} {:<7} {:<16} {:<44} {:>5}  {}",
            state_word(s),
            cut(s.harness.as_str(), 7),
            cut(&s.project, 16),
            cut(&headline(s), 44),
            age(s.last_active_at, now),
            s.address(),
        );
    }
}

fn headline(s: &Session) -> String {
    s.title
        .clone()
        .or_else(|| s.first_prompt.clone())
        .unwrap_or_else(|| "-".to_string())
        .replace('\n', " ")
}

fn cut(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

fn age(t: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(t) = t else {
        return "-".to_string();
    };
    let secs = (now - t).num_seconds().max(0);
    match secs {
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hsm_core::{index::SOURCE_TRANSCRIPT, NoLive, RefreshOptions, Tier};

    fn seeded() -> Index {
        let index = Index::open_in_memory().expect("index");
        let mut a = Session::new(HarnessKind::Claude, "aaaa1111-2222", "/p/trade-help");
        a.title = Some("API authentication".into());
        a.last_active_at = Some(Utc::now());
        a.transcript_present = true;
        index.upsert(&a, SOURCE_TRANSCRIPT).expect("a");

        let mut b = Session::new(HarnessKind::Codex, "bbbb1111-2222", "/p/other");
        b.title = Some("codex thread".into());
        b.last_active_at = Some(Utc::now() - chrono::Duration::days(1));
        b.transcript_present = true;
        index.upsert(&b, SOURCE_TRANSCRIPT).expect("b");
        index
    }

    #[test]
    fn harness_narrows_and_project_filters() {
        let index = seeded();
        assert_eq!(find(&index, None, 10, None, None).expect("all").len(), 2);
        let claude = find(&index, None, 10, Some("claude"), None).expect("claude");
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].harness, HarnessKind::Claude);

        let scoped = find(&index, None, 10, None, Some("trade-help")).expect("project");
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].project, "trade-help");
        assert!(find(&index, None, 10, None, Some("nothing"))
            .expect("empty")
            .is_empty());
    }

    #[test]
    fn a_query_ranks_the_match_first() {
        let index = seeded();
        let hits = find(&index, Some("authentication"), 10, None, None).expect("query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "aaaa1111-2222");
    }

    /// The target's own text shares no word with its id; a decoy's text does,
    /// so the id lookup and the text search return different rows.
    fn seeded_with_ids() -> Index {
        let index = seeded();
        let mut decoy = Session::new(HarnessKind::Claude, "dddd5555-6666", "/p/notes");
        decoy.title = Some("aaaa1111 lookalike".into());
        decoy.last_active_at = Some(Utc::now());
        decoy.transcript_present = true;
        index.upsert(&decoy, SOURCE_TRANSCRIPT).expect("decoy");
        index
    }

    #[test]
    fn an_id_prefix_query_finds_the_session_its_text_cannot() {
        let index = seeded_with_ids();
        // Text alone reaches the decoy, never the session that *has* that id.
        let by_text = index
            .search(&Query {
                text: "aaaa1111".into(),
                limit: 10,
                ..Query::default()
            })
            .expect("fts");
        assert_eq!(by_text.len(), 1);
        assert_eq!(by_text[0].id, "dddd5555-6666");

        let hits = find(&index, Some("aaaa1111"), 10, None, None).expect("prefix");
        assert_eq!(hits[0].id, "aaaa1111-2222", "the named session comes first");
        assert_eq!(hits[0].harness, HarnessKind::Claude);
        assert!(
            hits.iter().any(|s| s.id == "dddd5555-6666"),
            "text hits follow"
        );

        // What agentmail's resources/read asks for.
        let hits = find(&index, Some("aaaa1111-2222"), 1, None, None).expect("full id");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "aaaa1111-2222");
    }

    #[test]
    fn an_address_query_names_one_session() {
        let index = seeded_with_ids();
        let hits = find(&index, Some("claude:aaaa1111"), 10, None, None).expect("address");
        assert_eq!(hits[0].id, "aaaa1111-2222");
        // Named once, listed once.
        assert_eq!(hits.iter().filter(|s| s.id == "aaaa1111-2222").count(), 1);

        // A harness flag that contradicts the address drops the exact hit.
        let hits = find(&index, Some("claude:aaaa1111"), 10, Some("codex"), None).expect("codex");
        assert!(hits.iter().all(|s| s.harness == HarnessKind::Codex));
    }

    #[test]
    fn prose_is_never_looked_up_as_an_id() {
        let index = seeded_with_ids();
        let hits = find(&index, Some("authentication"), 10, None, None).expect("prose");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "aaaa1111-2222");

        // An id-shaped query that matches nothing is simply empty.
        assert!(find(&index, Some("deadbeef"), 10, None, None)
            .expect("miss")
            .is_empty());
    }

    #[test]
    fn id_shapes_are_recognised_but_words_are_not() {
        assert!(looks_like_id("aaaa1111"));
        assert!(looks_like_id("8890a685-a0f1-4a9e-949d-f7f386bc4cb6"));
        assert!(looks_like_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        // Too short to resolve a prefix.
        assert!(!looks_like_id("8890a68"));
        // Prose, and anything with a space.
        assert!(!looks_like_id("authentication"));
        assert!(!looks_like_id("shared bank"));
    }

    #[test]
    fn cards_carry_the_protocol_fields() {
        let index = seeded();
        let rows = find(&index, None, 1, Some("claude"), None).expect("rows");
        let card = SessionCard::from(&rows[0]);
        assert_eq!(card.address, "claude:aaaa1111-2222");
        assert_eq!(card.harness, "claude");
        assert_eq!(card.project, "trade-help");
        assert!(card.resumable);
        assert_eq!(rows[0].tier, Tier::Hot);
    }

    /// A burst of provider calls must cost one scan, not one each.
    #[test]
    fn a_second_provider_call_inside_the_window_does_not_refresh() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("index.sqlite");
        let mut index = Index::open(&path).expect("open");
        // One real refresh, with nothing on this machine in scope, to stamp it.
        let scanned = scan(&mut index, dir.path());
        let first = index.last_refresh().expect("stamped");
        assert_eq!(scanned, 1);

        let mut scans = scanned;
        let ran = refresh_if_stale(&mut index, first + chrono::Duration::seconds(1), |i| {
            scans += scan(i, dir.path());
            Ok(())
        })
        .expect("gate");
        assert!(!ran, "still inside the window");
        assert_eq!(scans, 1, "no second scan");
        assert_eq!(index.last_refresh(), Some(first), "the stamp did not move");

        let ran = refresh_if_stale(&mut index, first + chrono::Duration::seconds(11), |i| {
            scans += scan(i, dir.path());
            Ok(())
        })
        .expect("gate");
        assert!(ran, "past the window");
        assert_eq!(scans, 2);
        assert!(index.last_refresh().expect("restamped") >= first);
    }

    /// A refresh that looks at an empty directory instead of the real stores.
    fn scan(index: &mut Index, dir: &std::path::Path) -> u32 {
        let roots = vec![dir.join("no-transcripts")];
        let none = NoLive;
        let opts = RefreshOptions {
            disabled: &[HarnessKind::Claude, HarnessKind::Codex],
            store_roots_override: Some(&roots),
            ..RefreshOptions::new(&none)
        };
        index.refresh(&opts).expect("refresh");
        1
    }
}
