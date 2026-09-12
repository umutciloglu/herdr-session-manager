//! Process-wide wiring: config, the tokio runtime, the index, the socket.

use std::future::Future;

use anyhow::{Context, Result};
use herdr_client::{HerdrClient, PluginEnv};
use hsm_core::{paths, Config, Index};

use crate::herdr_adapter::HerdrLive;

pub struct Ctx {
    pub plugin: PluginEnv,
    pub config: Config,
    /// Multi-threaded on purpose: the TUI is sync and blocks the main thread on
    /// this handle, and only a multi-thread runtime keeps driving IO then.
    runtime: tokio::runtime::Runtime,
}

impl Ctx {
    pub fn load() -> Result<Ctx> {
        let config_path = paths::config_path();
        let config = Config::load(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .context("starting the tokio runtime")?;
        Ok(Ctx {
            plugin: PluginEnv::from_env(),
            config,
            runtime,
        })
    }

    pub fn block_on<F: Future>(&self, f: F) -> F::Output {
        self.runtime.block_on(f)
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    pub fn index(&self) -> Result<Index> {
        let path = paths::index_path();
        Index::open(&path).with_context(|| format!("opening {}", path.display()))
    }

    /// `None` when herdr is not running. Everything except opening a pane still
    /// works then, so this is a debug-level fact, not an error.
    pub fn herdr(&self) -> Option<HerdrClient> {
        match self.block_on(HerdrClient::connect()) {
            Ok(client) => Some(client),
            Err(error) => {
                tracing::debug!(%error, "herdr socket is not reachable");
                None
            }
        }
    }

    /// The live-session view for an index refresh.
    pub fn live(&self, client: Option<HerdrClient>) -> Option<HerdrLive> {
        client.map(|c| HerdrLive::new(c, self.handle()))
    }
}
