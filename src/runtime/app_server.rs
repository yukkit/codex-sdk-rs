use std::future::Future;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use codex_app_server_client::{AppServerClient, AppServerEvent, AppServerRequestHandle};
use codex_app_server_protocol::{
    ClientRequest, JSONRPCErrorError, RequestId, Result as JsonRpcResult,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::error::{Error, Result};

// Server responses share the driver's event loop. Bounding each write prevents
// a broken connection from stopping event forwarding indefinitely.
const SERVER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const SERVER_RESPONSE_QUEUE_CAPACITY: usize = 32;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Operations that do not require mutable access to the event source. The full
/// `AppServerClient` remains exclusively owned by the driver.
pub(super) struct AppServerHandle {
    requests: AppServerRequestHandle,
    responses: mpsc::Sender<ServerResponseCommand>,
    request_ids: AtomicI64,
}

/// Exclusive owner of `AppServerClient` and its event receiver.
pub(super) struct AppServerDriver {
    client: AppServerClient,
    responses: mpsc::Receiver<ServerResponseCommand>,
}

struct ServerResponseCommand {
    request_id: RequestId,
    action: ServerResponseAction,
    deadline: Instant,
    completion: oneshot::Sender<Result<()>>,
}

enum ServerResponseAction {
    Resolve(JsonRpcResult),
    Reject(JSONRPCErrorError),
}

impl AppServerDriver {
    pub(super) fn new(client: AppServerClient) -> (Self, AppServerHandle) {
        let requests = client.request_handle();
        // Server responses are rare control messages. Their queue has a small
        // fixed bound so event-stream tuning cannot accidentally change it.
        let (response_tx, response_rx) = mpsc::channel(SERVER_RESPONSE_QUEUE_CAPACITY);
        (
            Self {
                client,
                responses: response_rx,
            },
            AppServerHandle {
                requests,
                responses: response_tx,
                request_ids: AtomicI64::new(0),
            },
        )
    }

    /// Return the next app-server event while servicing response commands on
    /// the same client connection.
    pub(super) async fn next_event(&mut self) -> Option<AppServerEvent> {
        loop {
            tokio::select! {
                Some(response) = self.responses.recv() => {
                    // The pinned upstream client does not expose a split server-
                    // response handle, so this write temporarily owns the event
                    // source. Its end-to-end deadline bounds that pause.
                    self.handle_response(response).await;
                }
                event = self.client.next_event() => return event,
            }
        }
    }

    pub(super) async fn shutdown(self) -> Result<()> {
        let Self { client, responses } = self;
        // Wake response callers before waiting on the potentially slower client
        // shutdown. Dropping the receiver closes queued and pending sends.
        drop(responses);

        match tokio::time::timeout(SHUTDOWN_TIMEOUT, client.shutdown()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(Error::runtime_shutdown(error)),
            Err(_) => Err(Error::RuntimeShutdownTimeout {
                timeout: SHUTDOWN_TIMEOUT,
            }),
        }
    }

    async fn handle_response(&self, response: ServerResponseCommand) {
        let ServerResponseCommand {
            request_id,
            action,
            deadline,
            completion,
        } = response;
        if Instant::now() >= deadline {
            let _ = completion.send(server_response_timeout());
            return;
        }
        let result = match action {
            ServerResponseAction::Resolve(result) => {
                with_server_response_deadline(
                    self.client.resolve_server_request(request_id, result),
                    deadline,
                )
                .await
            }
            ServerResponseAction::Reject(error) => {
                with_server_response_deadline(
                    self.client.reject_server_request(request_id, error),
                    deadline,
                )
                .await
            }
        };
        let _ = completion.send(result);
    }
}

async fn with_server_response_deadline<F>(response: F, deadline: Instant) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    match tokio::time::timeout_at(deadline, response).await {
        Ok(result) => result.map_err(Error::approval),
        Err(_) => server_response_timeout(),
    }
}

fn server_response_timeout() -> Result<()> {
    Err(Error::ServerRequestResponseTimeout {
        timeout: SERVER_RESPONSE_TIMEOUT,
    })
}

impl AppServerHandle {
    pub(super) fn next_request_id(&self) -> RequestId {
        RequestId::Integer(self.request_ids.fetch_add(1, Ordering::Relaxed) + 1)
    }

    pub(super) async fn request_typed<T>(&self, request: ClientRequest) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.requests
            .request_typed(request)
            .await
            .map_err(Error::protocol)
    }

    pub(super) async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> Result<()> {
        self.send_server_response(request_id, ServerResponseAction::Resolve(result))
            .await
    }

    pub(super) async fn reject_server_request(
        &self,
        request_id: RequestId,
        message: impl Into<String>,
    ) -> Result<()> {
        self.send_server_response(
            request_id,
            ServerResponseAction::Reject(JSONRPCErrorError {
                code: -32000,
                message: message.into(),
                data: None,
            }),
        )
        .await
    }

    async fn send_server_response(
        &self,
        request_id: RequestId,
        action: ServerResponseAction,
    ) -> Result<()> {
        let (completion, completed) = oneshot::channel();
        let deadline = Instant::now() + SERVER_RESPONSE_TIMEOUT;
        let response = async {
            self.responses
                .send(ServerResponseCommand {
                    request_id,
                    action,
                    deadline,
                    completion,
                })
                .await
                .map_err(|_| Error::RuntimeClosed)?;
            completed.await.map_err(|_| Error::RuntimeClosed)?
        };

        match tokio::time::timeout_at(deadline, response).await {
            Ok(result) => result,
            Err(_) => server_response_timeout(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use super::*;

    #[tokio::test]
    async fn server_response_deadline_bounds_the_entire_write() {
        let result = with_server_response_deadline(
            pending::<std::io::Result<()>>(),
            Instant::now() + Duration::from_millis(1),
        )
        .await;

        assert!(matches!(
            result,
            Err(Error::ServerRequestResponseTimeout { .. })
        ));
    }
}
