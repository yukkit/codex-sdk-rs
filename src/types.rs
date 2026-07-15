use std::path::PathBuf;

/// Identifier assigned by the Codex app-server to a conversation thread.
pub type ThreadId = String;

/// Identifier assigned by the Codex app-server to a single turn.
pub type TurnId = String;

/// Metadata used to identify this SDK client to the Codex runtime.
#[derive(Debug, Clone)]
pub(crate) struct ClientInfo {
    /// Human-readable application name reported to Codex.
    pub(crate) name: String,
    /// Application or SDK version reported alongside [`name`](Self::name).
    pub(crate) version: String,
}

impl Default for ClientInfo {
    fn default() -> Self {
        Self {
            name: "codex-sdk-rs".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

pub(crate) fn cwd_to_string(cwd: impl Into<PathBuf>) -> String {
    cwd.into().to_string_lossy().to_string()
}
