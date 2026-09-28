//! Read-only checks against a real herdr server. Ignored by default because
//! they need one running; nothing here mutates session state.
//!
//! `cargo test -p herdr-client -- --ignored --nocapture`
//!
//! Set `HERDR_LIVE_REQUIRED=1` to fail instead of skip when no server answers. CI uses
//! that on Windows, where this is the only test of the real named pipe.

use herdr_client::{HerdrClient, PaneRead, ReadSource};

async fn client() -> Option<HerdrClient> {
    match HerdrClient::connect().await {
        Ok(client) => Some(client),
        Err(error) if std::env::var_os("HERDR_LIVE_REQUIRED").is_some() => {
            panic!("no herdr server: {error}")
        }
        Err(error) => {
            eprintln!("no herdr server: {error}");
            None
        }
    }
}

#[tokio::test]
#[ignore = "needs a running herdr server"]
async fn live_read_only_surface() {
    let Some(client) = client().await else { return };

    let version = client.ping().await.expect("ping");
    assert!(!version.is_empty());

    let snapshot = client.session_snapshot().await.expect("snapshot");
    assert!(!snapshot.workspaces.is_empty());
    assert_eq!(
        snapshot.tabs.len() as u64,
        snapshot.workspaces.iter().map(|w| w.tab_count).sum::<u64>()
    );

    let agents = client.agent_list().await.expect("agent.list");
    for agent in &agents {
        assert!(!agent.pane_id.is_empty());
        assert!(!agent.workspace_id.is_empty());
    }

    let current = client.pane_current(None).await.expect("pane.current");
    assert!(!current.pane_id.is_empty());

    let read = client
        .pane_read(&PaneRead::new(&current.pane_id, ReadSource::Visible).lines(5))
        .await
        .expect("pane.read");
    assert_eq!(read.pane_id, current.pane_id);

    println!(
        "herdr {version}: {} workspaces, {} tabs, {} panes, {} agents; current {} ({})",
        snapshot.workspaces.len(),
        snapshot.tabs.len(),
        snapshot.panes.len(),
        agents.len(),
        current.pane_id,
        current.agent_status
    );
    for agent in agents.iter().take(3) {
        println!(
            "  {} {} {} {:?}",
            agent.pane_id,
            agent.agent.as_deref().unwrap_or("-"),
            agent.agent_status,
            agent.agent_session.as_ref().map(|s| s.value.as_str())
        );
    }
}
