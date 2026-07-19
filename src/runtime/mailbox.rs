use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use codex_app_server_client::AppServerEvent;
use tokio::sync::mpsc;
#[cfg(test)]
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::error::TrySendError;
use tracing::warn;

// Events can precede the response that reveals their thread id. This bounded
// replay buffer is deliberately separate from each live stream's queue.
pub(super) const PENDING_EVENT_CAPACITY: usize = 32_768;

pub(super) struct MailboxReceiver {
    initial: VecDeque<AppServerEvent>,
    live: mpsc::Receiver<AppServerEvent>,
    terminal: Arc<Mutex<VecDeque<AppServerEvent>>>,
}

pub(super) struct ActiveMailbox {
    sender: mpsc::Sender<AppServerEvent>,
    terminal: Arc<Mutex<VecDeque<AppServerEvent>>>,
    skipped: AtomicUsize,
    closed: AtomicBool,
}

#[derive(Default)]
pub(super) struct PendingEvents {
    events: VecDeque<AppServerEvent>,
    skipped: usize,
}

pub(super) fn open_mailbox(
    capacity: usize,
    pending: PendingEvents,
) -> (Arc<ActiveMailbox>, MailboxReceiver) {
    let (sender, live) = mpsc::channel(capacity.max(1));
    let terminal = Arc::new(Mutex::new(VecDeque::new()));
    let receiver = MailboxReceiver {
        initial: pending.into_events(),
        live,
        terminal: Arc::clone(&terminal),
    };
    let mailbox = Arc::new(ActiveMailbox {
        sender,
        terminal,
        skipped: AtomicUsize::new(0),
        closed: AtomicBool::new(false),
    });
    (mailbox, receiver)
}

pub(super) fn closed_mailbox(pending: PendingEvents) -> MailboxReceiver {
    let (sender, live) = mpsc::channel(1);
    drop(sender);
    MailboxReceiver {
        initial: pending.into_events(),
        live,
        terminal: Arc::new(Mutex::new(VecDeque::new())),
    }
}

impl MailboxReceiver {
    pub(super) fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<AppServerEvent>> {
        if let Some(event) = self.initial.pop_front() {
            return Poll::Ready(Some(event));
        }

        match Pin::new(&mut self.live).poll_recv(cx) {
            Poll::Ready(None) => Poll::Ready(self.pop_terminal()),
            poll => poll,
        }
    }

    #[cfg(test)]
    pub(super) fn try_recv(
        &mut self,
    ) -> std::result::Result<AppServerEvent, TryRecvError> {
        if let Some(event) = self.initial.pop_front() {
            return Ok(event);
        }

        match self.live.try_recv() {
            Ok(event) => Ok(event),
            Err(TryRecvError::Disconnected) => {
                self.pop_terminal().ok_or(TryRecvError::Disconnected)
            }
            Err(TryRecvError::Empty) if self.live.is_closed() => {
                self.pop_terminal().ok_or(TryRecvError::Disconnected)
            }
            Err(error) => Err(error),
        }
    }

    fn pop_terminal(&self) -> Option<AppServerEvent> {
        self.terminal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
    }
}

impl ActiveMailbox {
    pub(super) async fn forward(&self, event: AppServerEvent, required: bool) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }

        let skipped = self.skipped.swap(0, Ordering::AcqRel);
        if skipped > 0 {
            let lagged = AppServerEvent::Lagged { skipped };
            let delivered = if required {
                self.sender.send(lagged).await.is_ok()
            } else {
                match self.sender.try_send(lagged) {
                    Ok(()) => true,
                    Err(TrySendError::Full(_)) => {
                        self.add_skipped(skipped.saturating_add(1));
                        return true;
                    }
                    Err(TrySendError::Closed(_)) => return false,
                }
            };
            if !delivered {
                return false;
            }
        }

        if required {
            // Reliable delivery intentionally backpressures the shared event
            // pump, so applications must continuously drain active streams.
            return self.sender.send(event).await.is_ok();
        }

        match self.sender.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.add_skipped(1);
                warn!(
                    "dropping best-effort app-server event because consumer queue is full"
                );
                true
            }
            Err(TrySendError::Closed(_)) => false,
        }
    }

    pub(super) fn close(&self, event: AppServerEvent) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }

        let mut terminal = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        let skipped = self.skipped.swap(0, Ordering::AcqRel);
        if skipped > 0 {
            terminal.push_back(AppServerEvent::Lagged { skipped });
        }
        terminal.push_back(event);
    }

    fn add_skipped(&self, count: usize) {
        let _ =
            self.skipped
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |skipped| {
                    Some(skipped.saturating_add(count))
                });
    }
}

impl PendingEvents {
    pub(super) fn push(&mut self, event: AppServerEvent) {
        if self.events.len() == PENDING_EVENT_CAPACITY {
            self.events.pop_front();
            self.skipped = self.skipped.saturating_add(1);
        }
        self.events.push_back(event);
    }

    fn into_events(self) -> VecDeque<AppServerEvent> {
        let mut events = self.events;
        if self.skipped > 0 {
            events.push_front(AppServerEvent::Lagged {
                skipped: self.skipped,
            });
        }
        events
    }
}
