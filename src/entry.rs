use std::future::Future;

use codex_arg0::Arg0DispatchPaths;

use crate::client::Codex;
use crate::{CodexBuilder, CodexWithConfigBuilder, Config};

/// Context provided by [`run_main`].
#[derive(Debug, Clone)]
pub struct CodexMain {
    arg0_paths: Arg0DispatchPaths,
}

impl CodexMain {
    /// Create a [`CodexBuilder`] wired to the helper paths discovered by
    /// [`run_main`].
    pub fn builder(&self) -> CodexBuilder {
        Codex::builder().arg0_paths(self.arg0_paths.clone())
    }

    /// Create a config-based builder wired to the helper paths discovered by
    /// [`run_main`].
    pub fn builder_with_config(&self, config: Config) -> CodexWithConfigBuilder {
        Codex::builder_with_config(config).arg0_paths(self.arg0_paths.clone())
    }

    /// Helper executable paths used by Codex for shell, patch, and sandbox
    /// entrypoints.
    pub fn arg0_paths(&self) -> &Arg0DispatchPaths {
        &self.arg0_paths
    }
}

/// Run a Codex-aware async binary entrypoint.
///
/// Use this in binaries before creating a Tokio runtime. It initializes Codex
/// helper aliases for shell execution, `apply_patch`, and platform sandboxes.
pub fn run_main<F, Fut>(main_fn: F) -> anyhow::Result<()>
where
    F: FnOnce(CodexMain) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    codex_arg0::arg0_dispatch_or_else(|arg0_paths| async move {
        main_fn(CodexMain { arg0_paths }).await
    })
}
