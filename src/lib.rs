//! Ergonomic Rust SDK for embedding Codex.

mod client;
mod entry;
mod error;
mod event;
mod observability;
mod runtime;
mod thread;
mod turn;
mod types;
mod warmup;

pub use client::{Codex, CodexBuilder, CodexRemoteBuilder, CodexWithConfigBuilder};
pub use codex_app_server_protocol::{
    Account, AskForApproval, GetAccountParams, GetAccountResponse, Model,
    ModelListParams, ModelListResponse, RequestId, SandboxMode, SandboxPolicy,
    ServerNotification, ServerRequest, ThreadArchiveResponse, ThreadCompactStartResponse,
    ThreadForkParams, ThreadForkResponse, ThreadListParams, ThreadListResponse,
    ThreadReadResponse, ThreadResumeParams, ThreadResumeResponse, ThreadSetNameResponse,
    ThreadStartParams, ThreadUnarchiveResponse, TurnInterruptResponse, TurnStartParams,
    TurnSteerResponse, UserInput,
};
pub use codex_core::config::Config;
pub use codex_protocol::config_types::{Personality, ReasoningSummary};
pub use codex_protocol::openai_models::ReasoningEffort;
pub use entry::{CodexMain, run_main};
pub use error::{Error, Result};
pub use event::TurnEvent;
pub use observability::{
    Observability, ObservabilityBuilder, ObservabilityGuard, OtelExporter,
    OtelHttpProtocol, OtelProvider, OtelSettings, OtelTlsConfig, StatsigMetricsSettings,
    current_span_trace_id, current_span_w3c_trace_context, inject_span_w3c_trace_headers,
    set_parent_from_w3c_trace_context, span_w3c_trace_context,
    traceparent_context_from_env,
};
pub use thread::{Thread, ThreadBuilder};
pub use turn::{
    CodexTurnBuilder, IntoTurnInput, TurnBuilder, TurnHandle, TurnResult, TurnStream,
};
pub use types::{ThreadId, TurnId};
pub use warmup::{WarmupBuilder, WarmupFailure, WarmupResult};

/// Common imports for applications using the SDK.
pub mod prelude {
    pub use crate::{
        Account, AskForApproval, Codex, CodexBuilder, CodexMain, CodexRemoteBuilder,
        CodexTurnBuilder, CodexWithConfigBuilder, Config, Error, GetAccountParams,
        IntoTurnInput, Model, ModelListParams, Observability, ObservabilityBuilder,
        ObservabilityGuard, OtelExporter, OtelHttpProtocol, OtelSettings, Personality,
        ReasoningEffort, ReasoningSummary, RequestId, Result, SandboxMode, SandboxPolicy,
        ServerNotification, ServerRequest, Thread, ThreadBuilder, ThreadForkParams,
        ThreadListParams, ThreadResumeParams, ThreadStartParams, TurnBuilder, TurnEvent,
        TurnHandle, TurnResult, TurnStartParams, TurnStream, UserInput, WarmupBuilder,
        WarmupFailure, WarmupResult, run_main,
    };
}
