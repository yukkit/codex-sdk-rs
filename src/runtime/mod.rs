mod app_server;

use std::sync::Arc;

use app_server::{AppServerDriver, AppServerHandle};
use codex_app_server_client::{
    AppServerClient, AppServerEvent, DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    EnvironmentManager, ExecServerRuntimePaths, InProcessAppServerClient,
    InProcessClientStartArgs, RemoteAppServerClient, RemoteAppServerConnectArgs,
    StateDbHandle,
};
use codex_app_server_protocol::{
    ClientRequest, ConfigWarningNotification, RequestId, Result as JsonRpcResult,
};
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use codex_feedback::CodexFeedback;
use codex_protocol::protocol::SessionSource;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::error::{Error, Result};
use crate::types::ClientInfo;

pub(crate) struct RuntimeHandle {
    /// Unified handle for requests and server-request responses.
    app_server: AppServerHandle,
    /// Shared event distribution channel.
    event_tx: broadcast::Sender<AppServerEvent>,
    /// Serialized, cancellation-safe ownership of runtime shutdown.
    shutdown: Mutex<RuntimeShutdown>,
}

struct RuntimeShutdown {
    signal: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<()>>>,
}

impl RuntimeHandle {
    pub(crate) async fn start(
        arg0_paths: Arg0DispatchPaths,
        config: Config,
        client_info: ClientInfo,
        channel_capacity: usize,
    ) -> Result<Arc<Self>> {
        let channel_capacity = channel_capacity.max(1);
        tracing::info!(
            client_name = %client_info.name,
            cwd = %config.cwd.display(),
            "starting Codex in-process runtime"
        );

        let runtime_paths = ExecServerRuntimePaths::from_optional_paths(
            arg0_paths.codex_self_exe.clone(),
            arg0_paths.codex_linux_sandbox_exe.clone(),
        )
        .map_err(Error::runtime_start)?;
        let environment_manager = EnvironmentManager::from_codex_home(
            config.codex_home.to_path_buf(),
            Some(runtime_paths),
        )
        .await
        .map_err(Error::runtime_start)?;
        let state_db = codex_core::init_state_db(&config).await;
        let config_warnings = config_warnings(&config);

        let start_args = sdk_in_process_client_start_args(
            arg0_paths,
            config,
            state_db,
            environment_manager,
            config_warnings,
            client_info,
            channel_capacity,
        );

        let client = InProcessAppServerClient::start(start_args)
            .await
            .map_err(Error::runtime_start)?;
        let client = AppServerClient::InProcess(client);

        Ok(Self::start_with_client(client, channel_capacity))
    }

    pub(crate) async fn connect_remote(
        mut args: RemoteAppServerConnectArgs,
    ) -> Result<Arc<Self>> {
        tracing::info!("connecting to remote Codex app-server runtime");

        args.channel_capacity = args.channel_capacity.max(1);
        let channel_capacity = args.channel_capacity;
        let client = RemoteAppServerClient::connect(args)
            .await
            .map_err(Error::runtime_start)?;
        let client = AppServerClient::Remote(client);

        Ok(Self::start_with_client(client, channel_capacity))
    }

    fn start_with_client(client: AppServerClient, channel_capacity: usize) -> Arc<Self> {
        debug_assert!(channel_capacity > 0);
        let (event_tx, _) = broadcast::channel(channel_capacity);
        let (app_server_driver, app_server) =
            AppServerDriver::new(client, channel_capacity);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task_handle =
            spawn_event_loop(app_server_driver, event_tx.clone(), shutdown_rx);

        Arc::new(Self {
            app_server,
            event_tx,
            shutdown: Mutex::new(RuntimeShutdown::new(shutdown_tx, task_handle)),
        })
    }

    pub(crate) fn next_request_id(&self) -> RequestId {
        self.app_server.next_request_id()
    }

    pub(crate) async fn request_typed<T>(&self, request: ClientRequest) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.app_server.request_typed(request).await
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<AppServerEvent> {
        self.event_tx.subscribe()
    }

    pub(crate) async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> Result<()> {
        self.app_server
            .resolve_server_request(request_id, result)
            .await
    }

    pub(crate) async fn reject_server_request(
        &self,
        request_id: RequestId,
        message: impl Into<String>,
    ) -> Result<()> {
        self.app_server
            .reject_server_request(request_id, message)
            .await
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        // The state keeps owning the JoinHandle while it is polled. Cancelling
        // this caller therefore leaves the handle available to the next caller.
        self.shutdown.lock().await.shutdown().await
    }
}

impl RuntimeShutdown {
    fn new(signal: oneshot::Sender<()>, task: JoinHandle<Result<()>>) -> Self {
        Self {
            signal: Some(signal),
            task: Some(task),
        }
    }

    async fn shutdown(&mut self) -> Result<()> {
        if let Some(sender) = self.signal.take() {
            let _ = sender.send(());
        }

        let result = match self.task.as_mut() {
            Some(task) => task.await,
            None => return Ok(()),
        };
        self.task.take();

        result.map_err(Error::runtime_task)??;
        Ok(())
    }
}

/// Build app-server startup args for the embedded SDK mode.
///
/// Most `InProcessClientStartArgs` fields are app-server transport or CLI
/// compatibility knobs. The SDK runtime owns those defaults here so
/// `RuntimeHandle::start` only passes values that are meaningful for embedding.
fn sdk_in_process_client_start_args(
    arg0_paths: Arg0DispatchPaths,
    config: Config,
    state_db: Option<StateDbHandle>,
    environment_manager: EnvironmentManager,
    config_warnings: Vec<ConfigWarningNotification>,
    client_info: ClientInfo,
    channel_capacity: usize,
) -> InProcessClientStartArgs {
    let client_name = client_info.name;
    InProcessClientStartArgs {
        arg0_paths,
        config: Arc::new(config),
        cli_overrides: Vec::new(),
        loader_overrides: Default::default(),
        strict_config: false,
        cloud_config_bundle: Default::default(),
        feedback: CodexFeedback::new(),
        log_db: None,
        state_db,
        environment_manager: Arc::new(environment_manager),
        config_warnings,
        session_source: SessionSource::Custom(client_name.clone()),
        enable_codex_api_key_env: true,
        client_name,
        client_version: client_info.version,
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity,
    }
}

fn config_warnings(config: &Config) -> Vec<ConfigWarningNotification> {
    config
        .startup_warnings
        .iter()
        .map(|warning| ConfigWarningNotification {
            summary: warning.clone(),
            details: None,
            path: None,
            range: None,
        })
        .collect()
}

fn spawn_event_loop(
    mut app_server: AppServerDriver,
    event_tx: broadcast::Sender<AppServerEvent>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> JoinHandle<Result<()>> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    let _ = event_tx.send(AppServerEvent::Disconnected {
                        message: "Codex app-server runtime shut down".to_string(),
                    });
                    break;
                }
                event = app_server.next_event() => {
                    let Some(event) = event else {
                        let _ = event_tx.send(AppServerEvent::Disconnected {
                            message: "Codex app-server event stream closed".to_string(),
                        });
                        break;
                    };
                    if let AppServerEvent::Disconnected { message } = &event {
                        warn!(%message, "Codex app-server disconnected");
                    }
                    let disconnected = matches!(event, AppServerEvent::Disconnected { .. });
                    let _ = event_tx.send(event);
                    if disconnected {
                        break;
                    }
                }
            }
        }

        app_server.shutdown().await
    })
}

pub(crate) const DEFAULT_CHANNEL_CAPACITY: usize = DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn shutdown_can_resume_after_waiter_is_cancelled() {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = shutdown_rx.await;
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Ok(())
        });
        let shutdown = Arc::new(Mutex::new(RuntimeShutdown::new(shutdown_tx, task)));

        let first_shutdown = Arc::clone(&shutdown);
        let first_waiter =
            tokio::spawn(async move { first_shutdown.lock().await.shutdown().await });
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .expect("shutdown task should start promptly")
            .expect("shutdown task should start");
        first_waiter.abort();
        assert!(
            first_waiter
                .await
                .expect_err("waiter should be cancelled")
                .is_cancelled()
        );

        let second_shutdown = Arc::clone(&shutdown);
        let second_waiter =
            tokio::spawn(async move { second_shutdown.lock().await.shutdown().await });
        let _ = release_tx.send(());
        tokio::time::timeout(Duration::from_secs(1), second_waiter)
            .await
            .expect("second waiter should finish promptly")
            .expect("second waiter should run")
            .expect("shutdown should complete");

        shutdown
            .lock()
            .await
            .shutdown()
            .await
            .expect("shutdown should be idempotent");
    }
}
