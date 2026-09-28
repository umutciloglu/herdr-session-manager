pub mod browse;
pub mod doctor;
pub mod index;
pub mod open;
pub mod sessions;
pub mod setup_chat;
pub mod startup;

use hsm_core::{HarnessKind, ProcessKind, Session, Tier};

/// `<harness>:<id>` split into what `Index::get` wants. A bare id (no colon)
/// searches every harness.
pub fn split_address(raw: &str) -> (Option<HarnessKind>, String) {
    match raw.split_once(':') {
        Some((h, id)) if !h.is_empty() && !id.is_empty() => {
            (Some(HarnessKind::from_name(h)), id.to_string())
        }
        _ => (None, raw.to_string()),
    }
}

/// Same states the popup shows, for the plain-text listing.
pub fn state_word(s: &Session) -> &'static str {
    match &s.last_pane {
        Some(p) if p.live => match p.status.as_deref() {
            Some("working") => "working",
            Some("blocked") => "blocked",
            _ => "live",
        },
        // The popup's `job` and `run` tags: running, but with no pane.
        _ => match s.process.as_ref().map(|p| p.kind) {
            Some(ProcessKind::Job) => "job",
            Some(ProcessKind::Interactive) => "run",
            None => match s.tier {
                Tier::Hot => "hot",
                Tier::Warm => "warm",
                Tier::Gone => "gone",
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_split_into_harness_and_id() {
        let (h, id) = split_address("claude:8890a685");
        assert_eq!(h, Some(HarnessKind::Claude));
        assert_eq!(id, "8890a685");

        let (h, id) = split_address("8890a685");
        assert_eq!(h, None);
        assert_eq!(id, "8890a685");

        // A windows path-shaped ref keeps its colon in the id half.
        let (h, id) = split_address("pi:C:/tmp/s.json");
        assert_eq!(h, Some(HarnessKind::Pi));
        assert_eq!(id, "C:/tmp/s.json");
    }

    #[test]
    fn state_word_prefers_liveness() {
        let mut s = Session::new(HarnessKind::Claude, "x", "/p/demo");
        s.tier = Tier::Gone;
        assert_eq!(state_word(&s), "gone");

        // A background job runs without a pane, so it beats the tier word.
        s.process = Some(hsm_core::ProcessRef {
            pid: 57845,
            kind: ProcessKind::Job,
            status: Some("busy".into()),
            name: None,
            pane_id: None,
        });
        assert_eq!(state_word(&s), "job");
        s.process = Some(hsm_core::ProcessRef {
            kind: ProcessKind::Interactive,
            ..s.process.clone().expect("process")
        });
        assert_eq!(state_word(&s), "run");

        s.last_pane = Some(hsm_core::PaneRef {
            pane_id: "w1:p1".into(),
            live: true,
            status: Some("working".into()),
            ..hsm_core::PaneRef::default()
        });
        assert_eq!(state_word(&s), "working");
    }
}
