use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use codex_app_server_client::{
    AppServerClient, AppServerEvent, AppServerRequestHandle,
    DEFAULT_IN_PROCESS_CHANNEL_CAPACITY, EnvironmentManager, ExecServerRuntimePaths,
    InProcessAppServerClient, InProcessClientStartArgs, RemoteAppServerClient,
    RemoteAppServerConnectArgs, StateDbHandle,
};
use codex_app_server_protocol::{
    ClientRequest, ConfigWarningNotification, JSONRPCErrorError, RequestId,
    Result as JsonRpcResult,
};
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use codex_feedback::CodexFeedback;
use codex_protocol::protocol::SessionSource;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::error::{Error, Result};
use crate::types::ClientInfo;

pub(crate) struct RuntimeHandle {
    /// Request handle used to send typed client requests into app-server.
    request_handle: AppServerRequestHandle,
    /// Command channel for resolving server-initiated requests.
    command_tx: mpsc::Sender<RuntimeCommand>,
    /// Monotonic JSON-RPC request id source.
    request_ids: AtomicI64,
    /// Broadcast channel for filtered turn streams and SDK consumers.
    event_tx: broadcast::Sender<AppServerEvent>,
    /// Shutdown signal for the background app-server event loop.
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Join handle for the background app-server event loop.
    task_handle: Mutex<Option<JoinHandle<()>>>,
}

enum RuntimeCommand {
    ResolveServerRequest {
        request_id: RequestId,
        result: JsonRpcResult,
        response_tx: oneshot::Sender<Result<()>>,
    },
    RejectServerRequest {
        request_id: RequestId,
        error: JSONRPCErrorError,
        response_tx: oneshot::Sender<Result<()>>,
    },
}

impl RuntimeHandle {
    pub async fn start(
        arg0_paths: Arg0DispatchPaths,
        config: Config,
        client_info: ClientInfo,
        channel_capacity: usize,
    ) -> Result<Arc<Self>> {
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

        Self::start_with_client(client, channel_capacity).await
    }

    pub async fn connect_remote(args: RemoteAppServerConnectArgs) -> Result<Arc<Self>> {
        tracing::info!("connecting to remote Codex app-server runtime");

        let channel_capacity = args.channel_capacity.max(1);
        let client = RemoteAppServerClient::connect(args)
            .await
            .map_err(Error::runtime_start)?;
        let client = AppServerClient::Remote(client);

        Self::start_with_client(client, channel_capacity).await
    }

    async fn start_with_client(
        client: AppServerClient,
        channel_capacity: usize,
    ) -> Result<Arc<Self>> {
        let request_handle = client.request_handle();
        let channel_capacity = channel_capacity.max(1);
        let (event_tx, _) = broadcast::channel(channel_capacity);
        let (command_tx, command_rx) = mpsc::channel(channel_capacity);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let runtime = Arc::new(Self {
            request_handle,
            command_tx,
            request_ids: AtomicI64::new(0),
            event_tx,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            task_handle: Mutex::new(None),
        });
        let task_handle =
            spawn_event_loop(client, runtime.clone(), command_rx, shutdown_rx);
        *runtime.task_handle.lock().await = Some(task_handle);

        Ok(runtime)
    }

    pub fn next_request_id(&self) -> RequestId {
        RequestId::Integer(self.request_ids.fetch_add(1, Ordering::SeqCst) + 1)
    }

    pub async fn request_typed<T>(&self, request: ClientRequest) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.request_handle
            .request_typed(request)
            .await
            .map_err(Error::protocol)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppServerEvent> {
        self.event_tx.subscribe()
    }

    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(RuntimeCommand::ResolveServerRequest {
                request_id,
                result,
                response_tx,
            })
            .await
            .map_err(|_| Error::RuntimeClosed)?;
        response_rx.await.map_err(|_| Error::RuntimeClosed)?
    }

    pub async fn reject_server_request(
        &self,
        request_id: RequestId,
        message: impl Into<String>,
    ) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(RuntimeCommand::RejectServerRequest {
                request_id,
                error: JSONRPCErrorError {
                    code: -32000,
                    message: message.into(),
                    data: None,
                },
                response_tx,
            })
            .await
            .map_err(|_| Error::RuntimeClosed)?;
        response_rx.await.map_err(|_| Error::RuntimeClosed)?
    }

    pub async fn shutdown(&self) -> Result<()> {
        if let Some(sender) = self.shutdown_tx.lock().await.take() {
            let _ = sender.send(());
        }
        let task_handle = self.task_handle.lock().await.take();
        if let Some(task_handle) = task_handle {
            task_handle.await.map_err(Error::runtime_task)?;
        }
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
        channel_capacity: channel_capacity.max(1),
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
    mut client: AppServerClient,
    runtime: Arc<RuntimeHandle>,
    mut command_rx: mpsc::Receiver<RuntimeCommand>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(command) = command_rx.recv() => {
                    handle_runtime_command(&client, command).await;
                }
                event = client.next_event() => {
                    let Some(event) = event else {
                        let _ = runtime.event_tx.send(AppServerEvent::Disconnected {
                            message: "Codex app-server event stream closed".to_string(),
                        });
                        break;
                    };
                    if let AppServerEvent::Disconnected { message } = &event {
                        warn!(%message, "Codex app-server disconnected");
                    }
                    let disconnected = matches!(event, AppServerEvent::Disconnected { .. });
                    let _ = runtime.event_tx.send(event);
                    if disconnected {
                        break;
                    }
                }
                _ = &mut shutdown_rx => {
                    let _ = runtime.event_tx.send(AppServerEvent::Disconnected {
                        message: "Codex app-server runtime shut down".to_string(),
                    });
                    break;
                }
            }
        }

        if let Err(error) = client.shutdown().await {
            warn!(%error, "failed to shutdown Codex app-server runtime cleanly");
        }
    })
}

async fn handle_runtime_command(client: &AppServerClient, command: RuntimeCommand) {
    match command {
        RuntimeCommand::ResolveServerRequest {
            request_id,
            result,
            response_tx,
        } => {
            let result = client
                .resolve_server_request(request_id, result)
                .await
                .map_err(Error::approval);
            let _ = response_tx.send(result);
        }
        RuntimeCommand::RejectServerRequest {
            request_id,
            error,
            response_tx,
        } => {
            let result = client
                .reject_server_request(request_id, error)
                .await
                .map_err(Error::approval);
            let _ = response_tx.send(result);
        }
    }
}

pub(crate) const DEFAULT_CHANNEL_CAPACITY: usize = DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
