use std::collections::HashMap;

use chrono::Utc;
use rusqlite::Connection;

use crate::domain::{HarnessKind, Session};
use crate::error::Result;
use crate::index::{row_to_session, Index, SESSION_COLUMNS};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub harness: Option<HarnessKind>,
    /// Sessions in this project are boosted, not filtered — the caller usually
    /// passes the cwd it was invoked from.
    pub project: Option<String>,
    pub limit: usize,
}

impl Query {
    pub fn text(text: impl Into<String>) -> Query {
        Query {
            text: text.into(),
            limit: 50,
            ..Query::default()
        }
    }
}

/// Relative pull of each signal. Text relevance is normalised to 0..1 first so
/// these stay comparable no matter how BM25 scales for a given corpus.
const W_TEXT: f64 = 10.0;
const W_RECENCY: f64 = 3.0;
const W_PROJECT: f64 = 2.0;
const W_LIVE: f64 = 5.0;
const W_PINNED: f64 = 1.5;
/// Days after which the recency bonus has decayed to 1/e.
const RECENCY_TAU_DAYS: f64 = 30.0;
/// A hit in the conversation body counts for less than one in the title.
const BODY_DISCOUNT: f64 = 0.6;
/// How many rows to score before trimming to `limit`.
const CANDIDATE_FACTOR: usize = 8;
const MIN_CANDIDATES: usize = 200;

impl Index {
    /// BM25 over titles, first prompts and message bodies, then boosted by
    /// liveness, project match, pins and recency. An empty query is `recent`.
    pub fn search(&self, q: &Query) -> Result<Vec<Session>> {
        let limit = if q.limit == 0 { 50 } else { q.limit };
        let candidates = (limit * CANDIDATE_FACTOR).max(MIN_CANDIDATES);

        let (mut sessions, text_scores) = match fts_query(&q.text) {
            Some(match_expr) => {
                let scores = text_scores(&self.conn, &match_expr, candidates)?;
                if scores.is_empty() {
                    return Ok(Vec::new());
                }
                (
                    self.load(&scores.keys().cloned().collect::<Vec<_>>())?,
                    scores,
                )
            }
            None => (self.recent(candidates)?, HashMap::new()),
        };

        if let Some(h) = &q.harness {
            sessions.retain(|s| &s.harness == h);
        }

        let max_text = text_scores.values().copied().fold(0.0_f64, f64::max);
        let text_norm = if max_text > 0.0 {
            W_TEXT / max_text
        } else {
            0.0
        };
        let now = Utc::now();

        let mut scored: Vec<(f64, Session)> = sessions
            .into_iter()
            .map(|s| {
                let mut score = text_scores.get(&s.id).copied().unwrap_or(0.0) * text_norm;
                if let Some(last) = s.last_active_at {
                    let days = (now - last).num_seconds() as f64 / 86_400.0;
                    score += W_RECENCY * (-days.max(0.0) / RECENCY_TAU_DAYS).exp();
                }
                if q.project
                    .as_deref()
                    .is_some_and(|p| !p.is_empty() && p == s.project)
                {
                    score += W_PROJECT;
                }
                if s.is_live() {
                    score += W_LIVE;
                }
                if s.pinned {
                    score += W_PINNED;
                }
                (score, s)
            })
            .collect();

        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Stable tie-break so two identical scores never flip order.
                .then_with(|| b.1.last_active_at.cmp(&a.1.last_active_at))
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, s)| s).collect())
    }

    fn load(&self, ids: &[String]) -> Result<Vec<Session>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let holes = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id IN ({holes})");
        let mut stmt = self.conn.prepare(&sql)?;
        let params = rusqlite::params_from_iter(ids.iter());
        let rows = stmt.query_map(params, |r| Ok(row_to_session(r, self.hot_days())))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// Best BM25 score per session id, taking title/prompt hits and body hits.
/// SQLite's `bm25()` is negative and more negative is better, so it is flipped.
fn text_scores(conn: &Connection, match_expr: &str, limit: usize) -> Result<HashMap<String, f64>> {
    let mut out: HashMap<String, f64> = HashMap::new();

    let mut stmt = conn.prepare(
        "SELECT s.id, -bm25(sessions_fts) AS score \
         FROM sessions_fts JOIN sessions s ON s.rowid = sessions_fts.rowid \
         WHERE sessions_fts MATCH ?1 ORDER BY score DESC LIMIT ?2",
    )?;
    for row in stmt.query_map(rusqlite::params![match_expr, limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    })? {
        let (id, score) = row?;
        let e = out.entry(id).or_insert(0.0);
        *e = e.max(score);
    }
    drop(stmt);

    // bm25() cannot appear inside an aggregate, so the hits are ranked in a
    // subquery and folded to one score per session outside it.
    let mut stmt = conn.prepare(
        "SELECT id, MAX(score) FROM ( \
           SELECT m.id AS id, -bm25(messages_fts) AS score \
           FROM messages_fts JOIN messages m ON m.rowid = messages_fts.rowid \
           WHERE messages_fts MATCH ?1 ORDER BY score DESC LIMIT ?2 \
         ) GROUP BY id",
    )?;
    for row in stmt.query_map(rusqlite::params![match_expr, limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    })? {
        let (id, score) = row?;
        let e = out.entry(id).or_insert(0.0);
        *e = e.max(score * BODY_DISCOUNT);
    }
    Ok(out)
}

/// Free text -> an FTS5 expression. Every token is quoted (so `-`, `:` and `*`
/// in a user's query cannot become operators) and prefix-matched.
fn fts_query(text: &str) -> Option<String> {
    let tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PaneRef, Session};
    use crate::index::SOURCE_TRANSCRIPT;
    use chrono::Duration;

    fn session(id: &str, project: &str, title: &str, days_ago: i64) -> Session {
        let mut s = Session::new(HarnessKind::Claude, id, format!("/p/{project}"));
        s.title = Some(title.to_string());
        s.first_prompt = Some(format!("{title} please"));
        s.last_active_at = Some(Utc::now() - Duration::days(days_ago));
        s.transcript_present = true;
        s
    }

    fn index_with(sessions: &[Session]) -> Index {
        let idx = Index::open_in_memory().expect("open");
        for s in sessions {
            idx.upsert(s, SOURCE_TRANSCRIPT).expect("upsert");
        }
        idx
    }

    #[test]
    fn empty_query_is_recent() {
        let idx = index_with(&[
            session("old", "demo", "alpha", 40),
            session("new", "demo", "beta", 1),
        ]);
        let got = idx
            .search(&Query {
                limit: 10,
                ..Query::default()
            })
            .expect("search");
        assert_eq!(
            got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["new", "old"]
        );
    }

    #[test]
    fn same_project_wins_when_text_relevance_ties() {
        let idx = index_with(&[
            session("other", "unrelated", "auth middleware", 3),
            session("mine", "demo", "auth middleware", 3),
        ]);
        let q = Query {
            text: "auth middleware".into(),
            project: Some("demo".into()),
            limit: 10,
            ..Query::default()
        };
        let got = idx.search(&q).expect("search");
        assert_eq!(got[0].id, "mine");

        // Without the project hint the tie falls back to id order, not to "mine".
        let q = Query { project: None, ..q };
        let got = idx.search(&q).expect("search");
        assert_eq!(got[0].id, "mine", "ids sort before 'other'");
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn recency_orders_equally_relevant_hits() {
        let idx = index_with(&[
            session("stale", "demo", "auth middleware", 400),
            session("fresh", "demo", "auth middleware", 0),
        ]);
        let got = idx.search(&Query::text("auth")).expect("search");
        assert_eq!(
            got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["fresh", "stale"]
        );
    }

    #[test]
    fn a_live_pane_outranks_an_equally_relevant_session() {
        let mut live = session("live", "demo", "auth middleware", 200);
        live.last_pane = Some(PaneRef {
            pane_id: "3".into(),
            live: true,
            status: Some("working".into()),
            ..PaneRef::default()
        });
        let idx = index_with(&[session("cold", "demo", "auth middleware", 200)]);
        idx.upsert(&live, SOURCE_TRANSCRIPT).expect("upsert");
        idx.conn
            .execute(
                "UPDATE sessions SET last_pane_json = ?1 WHERE id = 'live'",
                [serde_json::to_string(&live.last_pane).expect("json")],
            )
            .expect("pane");

        let got = idx.search(&Query::text("auth")).expect("search");
        assert_eq!(
            got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["live", "cold"]
        );
    }

    #[test]
    fn message_bodies_are_searchable() {
        let idx = index_with(&[session("s1", "demo", "nothing useful", 1)]);
        idx.conn
            .execute(
                "INSERT INTO messages (harness, id, seq, role, ts, text) \
                 VALUES ('claude', 's1', 0, 'user', 0, 'the postgres connection pool leaks')",
                [],
            )
            .expect("message");
        let got = idx.search(&Query::text("postgres")).expect("search");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "s1");
    }

    #[test]
    fn harness_filter_and_punctuation_are_safe() {
        let mut codex = session("c1", "demo", "auth middleware", 1);
        codex.harness = HarnessKind::Codex;
        let idx = index_with(&[session("a1", "demo", "auth middleware", 1), codex]);

        let q = Query {
            text: "auth".into(),
            harness: Some(HarnessKind::Codex),
            limit: 10,
            ..Query::default()
        };
        assert_eq!(
            idx.search(&q)
                .expect("search")
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>(),
            vec!["c1"]
        );

        // Operators in the raw text must not reach FTS5.
        let q = Query::text("auth* OR \"x\" -y NEAR(");
        assert!(idx.search(&q).is_ok());
    }

    #[test]
    fn a_query_with_no_usable_tokens_falls_back_to_recent() {
        let idx = index_with(&[session("s1", "demo", "alpha", 1)]);
        assert_eq!(
            idx.search(&Query::text("!!! ???")).expect("search").len(),
            1
        );
    }
}
