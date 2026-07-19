use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::future::poll_fn;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::{Context, Poll};

use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::{ServerNotification, ThreadArchivedNotification};
use codex_protocol::ThreadId;
#[cfg(test)]
use tokio::sync::mpsc::error::TryRecvError;
use tracing::warn;

use crate::error::{Error, Result};
use crate::event::{EventTarget, event_target};
use crate::runtime::mailbox::{
    ActiveMailbox, MailboxReceiver, PendingEvents, closed_mailbox, open_mailbox,
};

// Inactive slots prevent late events from recreating unbounded pending queues.
// Active streams and in-flight lifecycle requests are never evicted.
const INACTIVE_ROUTE_CAPACITY: usize = 1_024;

pub(crate) struct EventReceiver {
    mailbox: MailboxReceiver,
    _lease: RouteLease,
}

impl EventReceiver {
    pub(crate) fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<AppServerEvent>> {
        self.mailbox.poll_recv(cx)
    }

    #[cfg(test)]
    pub(crate) async fn recv(&mut self) -> Option<AppServerEvent> {
        poll_fn(|cx| self.poll_recv(cx)).await
    }

    #[cfg(test)]
    pub(crate) fn try_recv(
        &mut self,
    ) -> std::result::Result<AppServerEvent, TryRecvError> {
        self.mailbox.try_recv()
    }
}

struct RouteLease {
    router: Weak<EventRouter>,
    target: RouteTarget,
    generation: u64,
}

enum RouteTarget {
    Runtime,
    Thread(ThreadId),
}

impl Drop for RouteLease {
    fn drop(&mut self) {
        if let Some(router) = self.router.upgrade() {
            router.detach(&self.target, self.generation);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadAttachmentKind {
    Resume,
    Unarchive,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttachmentPhase {
    Ready,
    AwaitingUnarchive,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TerminalSource {
    Notification,
    Authoritative,
}

impl From<ThreadAttachmentKind> for AttachmentPhase {
    fn from(kind: ThreadAttachmentKind) -> Self {
        match kind {
            ThreadAttachmentKind::Resume => Self::Ready,
            ThreadAttachmentKind::Unarchive => Self::AwaitingUnarchive,
        }
    }
}

pub(crate) struct ThreadEventReservation {
    router: Arc<EventRouter>,
    thread_id: ThreadId,
    generation: u64,
    finished: bool,
}

impl ThreadEventReservation {
    pub(crate) fn claim(mut self) -> Result<EventReceiver> {
        let receiver = self
            .router
            .claim_reserved_thread(self.thread_id, self.generation)?;
        self.finished = true;
        Ok(receiver)
    }
}

impl Drop for ThreadEventReservation {
    fn drop(&mut self) {
        if !self.finished {
            self.router
                .cancel_reserved_thread(self.thread_id, self.generation);
        }
    }
}

pub(super) struct EventRouter {
    state: Mutex<RouterState>,
    stream_capacity: usize,
}

struct RouterState {
    runtime: RuntimeRoute,
    threads: HashMap<ThreadId, ThreadRoute>,
    inactive: VecDeque<ThreadId>,
    next_generation: u64,
    closed: bool,
}

enum RuntimeRoute {
    /// Runtime stream has not been taken yet.
    Pending(PendingEvents),
    /// Runtime stream is owned by one receiver.
    Active {
        generation: u64,
        mailbox: Arc<ActiveMailbox>,
    },
    /// The unique runtime receiver was dropped and cannot be replaced.
    Detached,
}

enum ThreadRoute {
    /// Events arrived before the SDK attached the thread handle.
    Pending(PendingEvents),
    /// A resume or unarchive request owns the next attachment.
    Reserved {
        generation: u64,
        pending: PendingEvents,
        phase: AttachmentPhase,
    },
    /// One receiver currently owns this attachment.
    Active {
        generation: u64,
        mailbox: Arc<ActiveMailbox>,
        phase: AttachmentPhase,
    },
    /// A terminal event arrived before the receiver was attached.
    Closed(PendingEvents),
    /// Terminal tombstone used to deduplicate late lifecycle notifications.
    Terminated,
    /// The receiver was dropped; late ordinary events are discarded.
    Detached,
}

impl EventRouter {
    pub(super) fn new(stream_capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(RouterState {
                runtime: RuntimeRoute::Pending(PendingEvents::default()),
                threads: HashMap::new(),
                inactive: VecDeque::new(),
                next_generation: 1,
                closed: false,
            }),
            stream_capacity: stream_capacity.max(1),
        })
    }

    pub(super) fn prepare_thread(
        self: &Arc<Self>,
        thread_id: &str,
        kind: ThreadAttachmentKind,
    ) -> Result<ThreadEventReservation> {
        let thread_id = parse_thread_id(thread_id)?;
        let mut state = self.state();
        if state.closed {
            return Err(Error::RuntimeClosed);
        }

        let pending = match state.remove_thread(&thread_id) {
            None
            | Some(ThreadRoute::Closed(_))
            | Some(ThreadRoute::Terminated)
            | Some(ThreadRoute::Detached) => PendingEvents::default(),
            Some(ThreadRoute::Pending(pending)) => pending,
            Some(route @ ThreadRoute::Active { .. }) => {
                state.threads.insert(thread_id, route);
                return Err(Error::ThreadEventStreamTaken {
                    thread_id: thread_id.to_string(),
                });
            }
            Some(route @ ThreadRoute::Reserved { .. }) => {
                state.threads.insert(thread_id, route);
                return Err(Error::ThreadLifecycleInProgress {
                    thread_id: thread_id.to_string(),
                });
            }
        };

        let generation = state.allocate_generation();
        state.threads.insert(
            thread_id,
            ThreadRoute::Reserved {
                generation,
                pending,
                phase: kind.into(),
            },
        );
        drop(state);

        Ok(ThreadEventReservation {
            router: Arc::clone(self),
            thread_id,
            generation,
            finished: false,
        })
    }

    pub(super) fn take_thread(
        self: &Arc<Self>,
        thread_id: &str,
    ) -> Result<EventReceiver> {
        let thread_id = parse_thread_id(thread_id)?;
        let mut state = self.state();
        if state.closed {
            return Err(Error::RuntimeClosed);
        }

        match state.threads.get(&thread_id) {
            Some(ThreadRoute::Active { .. }) => {
                return Err(Error::ThreadEventStreamTaken {
                    thread_id: thread_id.to_string(),
                });
            }
            Some(ThreadRoute::Reserved { .. }) => {
                return Err(Error::ThreadLifecycleInProgress {
                    thread_id: thread_id.to_string(),
                });
            }
            _ => {}
        }

        let generation = state.allocate_generation();
        let route = state.remove_thread(&thread_id);
        let (next_route, receiver) =
            self.activate_thread(thread_id, route, generation, AttachmentPhase::Ready);
        if next_route.is_inactive() {
            state.set_inactive(thread_id, next_route);
        } else {
            state.threads.insert(thread_id, next_route);
        }
        Ok(receiver)
    }

    pub(super) fn take_runtime(self: &Arc<Self>) -> Result<EventReceiver> {
        let mut state = self.state();
        if state.closed {
            return Err(Error::RuntimeClosed);
        }

        if !matches!(&state.runtime, RuntimeRoute::Pending(_)) {
            return Err(Error::CodexEventStreamTaken);
        }

        let generation = state.allocate_generation();
        let RuntimeRoute::Pending(pending) =
            std::mem::replace(&mut state.runtime, RuntimeRoute::Detached)
        else {
            unreachable!("runtime route was checked before activation");
        };
        let (mailbox, receiver) = open_mailbox(self.stream_capacity, pending);
        state.runtime = RuntimeRoute::Active {
            generation,
            mailbox,
        };
        let receiver = EventReceiver {
            mailbox: receiver,
            _lease: self.lease(RouteTarget::Runtime, generation),
        };
        Ok(receiver)
    }

    /// Complete the local archive boundary from the successful response. Codex
    /// sends the matching notification after the response and its upstream
    /// queue currently treats that notification as best-effort.
    pub(super) fn complete_archive(&self, thread_id: &str) -> Result<()> {
        let thread_id = parse_thread_id(thread_id)?;
        self.close_thread(
            thread_id,
            AppServerEvent::ServerNotification(ServerNotification::ThreadArchived(
                ThreadArchivedNotification {
                    thread_id: thread_id.to_string(),
                },
            )),
            TerminalSource::Authoritative,
        );
        Ok(())
    }

    pub(super) async fn route(&self, event: AppServerEvent) {
        if matches!(&event, AppServerEvent::Disconnected { .. }) {
            self.close(event);
            return;
        }
        if matches!(&event, AppServerEvent::Lagged { .. }) {
            self.broadcast_lag(event).await;
            return;
        }

        match event_target(&event) {
            EventTarget::Thread(thread_id) => {
                if thread_event_terminates_stream(&event) {
                    self.close_thread(thread_id, event, TerminalSource::Notification);
                } else if let Some(route) = self.enqueue_thread(thread_id, event) {
                    let required = event_requires_delivery(&route.event);
                    if !route.mailbox.forward(route.event, required).await {
                        self.detach_active_thread(thread_id, &route.mailbox);
                    }
                }
            }
            EventTarget::Runtime => {
                if let Some(route) = self.enqueue_runtime(event) {
                    let required = event_requires_delivery(&route.event);
                    if !route.mailbox.forward(route.event, required).await {
                        self.detach_active_runtime(&route.mailbox);
                    }
                }
            }
            EventTarget::InvalidThread => {
                warn!("dropping app-server event with an invalid thread id");
            }
        }
    }

    pub(super) fn close(&self, event: AppServerEvent) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        state.closed = true;
        state.runtime.close(event.clone());
        for route in state.threads.values_mut() {
            route.close(event.clone());
        }
    }

    fn claim_reserved_thread(
        self: &Arc<Self>,
        thread_id: ThreadId,
        generation: u64,
    ) -> Result<EventReceiver> {
        let mut state = self.state();
        if state.closed {
            return Err(Error::RuntimeClosed);
        }

        let Some(route) = state.remove_thread(&thread_id) else {
            return Err(Error::ThreadLifecycleInProgress {
                thread_id: thread_id.to_string(),
            });
        };
        match route {
            ThreadRoute::Reserved {
                generation: current,
                pending,
                phase,
            } if current == generation => {
                let (route, receiver) =
                    self.activate_pending(thread_id, pending, generation, phase);
                state.threads.insert(thread_id, route);
                Ok(receiver)
            }
            route => {
                if route.is_inactive() {
                    state.set_inactive(thread_id, route);
                } else {
                    state.threads.insert(thread_id, route);
                }
                Err(Error::ThreadLifecycleInProgress {
                    thread_id: thread_id.to_string(),
                })
            }
        }
    }

    fn cancel_reserved_thread(&self, thread_id: ThreadId, generation: u64) {
        let mut state = self.state();
        if matches!(
            state.threads.get(&thread_id),
            Some(ThreadRoute::Reserved {
                generation: current,
                ..
            }) if *current == generation
        ) {
            state.set_inactive(thread_id, ThreadRoute::Detached);
        }
    }

    fn activate_thread(
        self: &Arc<Self>,
        thread_id: ThreadId,
        route: Option<ThreadRoute>,
        generation: u64,
        phase: AttachmentPhase,
    ) -> (ThreadRoute, EventReceiver) {
        match route {
            None | Some(ThreadRoute::Detached) | Some(ThreadRoute::Terminated) => self
                .activate_pending(thread_id, PendingEvents::default(), generation, phase),
            Some(ThreadRoute::Pending(pending)) => {
                self.activate_pending(thread_id, pending, generation, phase)
            }
            Some(ThreadRoute::Closed(pending)) => {
                let receiver = EventReceiver {
                    mailbox: closed_mailbox(pending),
                    _lease: self.lease(RouteTarget::Thread(thread_id), generation),
                };
                (ThreadRoute::Terminated, receiver)
            }
            Some(ThreadRoute::Active { .. }) | Some(ThreadRoute::Reserved { .. }) => {
                unreachable!("active and reserved routes are checked before activation")
            }
        }
    }

    fn activate_pending(
        self: &Arc<Self>,
        thread_id: ThreadId,
        pending: PendingEvents,
        generation: u64,
        phase: AttachmentPhase,
    ) -> (ThreadRoute, EventReceiver) {
        let (mailbox, receiver) = open_mailbox(self.stream_capacity, pending);
        let receiver = EventReceiver {
            mailbox: receiver,
            _lease: self.lease(RouteTarget::Thread(thread_id), generation),
        };
        (
            ThreadRoute::Active {
                generation,
                mailbox,
                phase,
            },
            receiver,
        )
    }

    fn enqueue_thread(
        &self,
        thread_id: ThreadId,
        event: AppServerEvent,
    ) -> Option<ActiveEvent> {
        let mut state = self.state();
        if state.closed {
            return None;
        }
        if !state.threads.contains_key(&thread_id) {
            state.set_inactive(thread_id, ThreadRoute::pending());
        }
        state
            .threads
            .get_mut(&thread_id)
            .and_then(|route| route.enqueue(event))
    }

    fn enqueue_runtime(&self, event: AppServerEvent) -> Option<ActiveEvent> {
        let mut state = self.state();
        if state.closed {
            return None;
        }
        state.runtime.enqueue(event)
    }

    fn close_thread(
        &self,
        thread_id: ThreadId,
        event: AppServerEvent,
        source: TerminalSource,
    ) {
        let mut state = self.state();
        if state.closed {
            return;
        }

        let Some(route) = state.remove_thread(&thread_id) else {
            // A terminal notification for a thread that has never had an SDK
            // attachment is not useful and must not create a permanent route.
            return;
        };

        match route.close_thread(event, source) {
            route if route.is_inactive() => state.set_inactive(thread_id, route),
            route => {
                state.threads.insert(thread_id, route);
            }
        }
    }

    async fn broadcast_lag(&self, event: AppServerEvent) {
        let active = {
            let mut state = self.state();
            if state.closed {
                return;
            }

            let mut active = Vec::new();
            if let Some(route) = state.runtime.enqueue(event.clone()) {
                active.push((None, route.mailbox, route.event));
            }
            for (thread_id, route) in &mut state.threads {
                if let Some(route) = route.enqueue(event.clone()) {
                    active.push((Some(*thread_id), route.mailbox, route.event));
                }
            }
            active
        };

        for (thread_id, mailbox, event) in active {
            let required = event_requires_delivery(&event);
            if !mailbox.forward(event, required).await {
                match thread_id {
                    Some(thread_id) => self.detach_active_thread(thread_id, &mailbox),
                    None => self.detach_active_runtime(&mailbox),
                }
            }
        }
    }

    fn detach_active_thread(&self, thread_id: ThreadId, active: &Arc<ActiveMailbox>) {
        let mut state = self.state();
        if matches!(
            state.threads.get(&thread_id),
            Some(ThreadRoute::Active { mailbox, .. }) if Arc::ptr_eq(mailbox, active)
        ) {
            state.set_inactive(thread_id, ThreadRoute::Detached);
        }
    }

    fn detach_active_runtime(&self, active: &Arc<ActiveMailbox>) {
        let mut state = self.state();
        if matches!(
            &state.runtime,
            RuntimeRoute::Active { mailbox, .. } if Arc::ptr_eq(mailbox, active)
        ) {
            state.runtime = RuntimeRoute::Detached;
        }
    }

    fn detach(&self, target: &RouteTarget, generation: u64) {
        let mut state = self.state();
        match target {
            RouteTarget::Runtime => {
                if matches!(
                    &state.runtime,
                    RuntimeRoute::Active {
                        generation: current,
                        ..
                    } if *current == generation
                ) {
                    state.runtime = RuntimeRoute::Detached;
                }
            }
            RouteTarget::Thread(thread_id) => {
                if matches!(
                    state.threads.get(thread_id),
                    Some(ThreadRoute::Active {
                        generation: current,
                        ..
                    }) if *current == generation
                ) {
                    state.set_inactive(*thread_id, ThreadRoute::Detached);
                }
            }
        }
    }

    fn lease(self: &Arc<Self>, target: RouteTarget, generation: u64) -> RouteLease {
        RouteLease {
            router: Arc::downgrade(self),
            target,
            generation,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, RouterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct ActiveEvent {
    mailbox: Arc<ActiveMailbox>,
    event: AppServerEvent,
}

impl RouterState {
    fn allocate_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        generation
    }

    fn remove_thread(&mut self, thread_id: &ThreadId) -> Option<ThreadRoute> {
        self.inactive.retain(|candidate| candidate != thread_id);
        self.threads.remove(thread_id)
    }

    fn set_inactive(&mut self, thread_id: ThreadId, route: ThreadRoute) {
        debug_assert!(route.is_inactive());
        self.inactive.retain(|candidate| *candidate != thread_id);
        self.threads.insert(thread_id, route);
        self.inactive.push_back(thread_id);

        while self.inactive.len() > INACTIVE_ROUTE_CAPACITY {
            let Some(evicted) = self.inactive.pop_front() else {
                break;
            };
            if self
                .threads
                .get(&evicted)
                .is_some_and(ThreadRoute::is_inactive)
            {
                self.threads.remove(&evicted);
                tracing::debug!(
                    thread_id = %evicted,
                    "evicting inactive thread event route"
                );
            }
        }
    }
}

impl RuntimeRoute {
    fn enqueue(&mut self, event: AppServerEvent) -> Option<ActiveEvent> {
        match self {
            Self::Pending(pending) => {
                pending.push(event);
                None
            }
            Self::Active { mailbox, .. } => Some(ActiveEvent {
                mailbox: Arc::clone(mailbox),
                event,
            }),
            Self::Detached => None,
        }
    }

    fn close(&mut self, event: AppServerEvent) {
        match std::mem::replace(self, Self::Detached) {
            Self::Active { mailbox, .. } => mailbox.close(event),
            Self::Pending(mut pending) => pending.push(event),
            Self::Detached => {}
        }
    }
}

impl ThreadRoute {
    fn pending() -> Self {
        Self::Pending(PendingEvents::default())
    }

    fn is_inactive(&self) -> bool {
        matches!(
            self,
            Self::Pending(_) | Self::Closed(_) | Self::Terminated | Self::Detached
        )
    }

    fn enqueue(&mut self, event: AppServerEvent) -> Option<ActiveEvent> {
        let unarchive_boundary = matches!(
            &event,
            AppServerEvent::ServerNotification(ServerNotification::ThreadUnarchived(_))
        );
        match self {
            Self::Pending(pending) => {
                pending.push(event);
                None
            }
            Self::Reserved { pending, phase, .. } => {
                if unarchive_boundary {
                    *phase = AttachmentPhase::Ready;
                }
                pending.push(event);
                None
            }
            Self::Active { mailbox, phase, .. } => {
                if unarchive_boundary {
                    *phase = AttachmentPhase::Ready;
                }
                Some(ActiveEvent {
                    mailbox: Arc::clone(mailbox),
                    event,
                })
            }
            Self::Closed(_) | Self::Terminated | Self::Detached => None,
        }
    }

    fn close(&mut self, event: AppServerEvent) {
        let current = std::mem::replace(self, Self::Detached);
        *self = current.close_thread(event, TerminalSource::Authoritative);
    }

    fn close_thread(self, event: AppServerEvent, source: TerminalSource) -> Self {
        let is_archive = matches!(
            &event,
            AppServerEvent::ServerNotification(ServerNotification::ThreadArchived(_))
        );
        match self {
            Self::Pending(mut pending) | Self::Closed(mut pending) => {
                pending.push(event);
                Self::Closed(pending)
            }
            Self::Reserved {
                generation,
                mut pending,
                phase,
            } => {
                if source == TerminalSource::Notification
                    && is_archive
                    && phase == AttachmentPhase::AwaitingUnarchive
                {
                    return Self::Reserved {
                        generation,
                        pending,
                        phase,
                    };
                }
                pending.push(event);
                Self::Closed(pending)
            }
            Self::Active {
                generation,
                mailbox,
                phase,
            } => {
                if source == TerminalSource::Notification
                    && is_archive
                    && phase == AttachmentPhase::AwaitingUnarchive
                {
                    return Self::Active {
                        generation,
                        mailbox,
                        phase,
                    };
                }
                mailbox.close(event);
                Self::Terminated
            }
            Self::Terminated => Self::Terminated,
            Self::Detached => Self::Terminated,
        }
    }
}

fn event_requires_delivery(event: &AppServerEvent) -> bool {
    // Mirrors codex-app-server-client rust-v0.144.4 for transcript and
    // completion notifications. The SDK additionally keeps ServerRequest and
    // Lagged reliable because callers must observe and act on them.
    match event {
        AppServerEvent::ServerRequest(_) | AppServerEvent::Lagged { .. } => true,
        AppServerEvent::ServerNotification(notification) => matches!(
            notification,
            ServerNotification::TurnCompleted(_)
                | ServerNotification::ThreadSettingsUpdated(_)
                | ServerNotification::ItemCompleted(_)
                | ServerNotification::ExternalAgentConfigImportCompleted(_)
                | ServerNotification::AgentMessageDelta(_)
                | ServerNotification::PlanDelta(_)
                | ServerNotification::ReasoningSummaryTextDelta(_)
                | ServerNotification::ReasoningTextDelta(_)
        ),
        AppServerEvent::Disconnected { .. } => true,
    }
}

fn thread_event_terminates_stream(event: &AppServerEvent) -> bool {
    matches!(
        event,
        AppServerEvent::ServerNotification(
            ServerNotification::ThreadArchived(_)
                | ServerNotification::ThreadDeleted(_)
                | ServerNotification::ThreadClosed(_)
        )
    )
}

fn parse_thread_id(thread_id: &str) -> Result<ThreadId> {
    ThreadId::from_string(thread_id).map_err(|_| Error::InvalidThreadId {
        thread_id: thread_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use codex_app_server_protocol::{
        ServerNotification, ThreadArchivedNotification, ThreadClosedNotification,
        ThreadDeletedNotification, ThreadUnarchivedNotification, Turn,
        TurnCompletedNotification, TurnItemsView, TurnStatus, WarningNotification,
    };

    use super::*;

    const CAPACITY: usize = 4;

    #[tokio::test]
    async fn routes_events_only_to_their_owning_thread() {
        let router = EventRouter::new(CAPACITY);
        let first_id = ThreadId::new();
        let second_id = ThreadId::new();
        let mut first = router
            .take_thread(&first_id.to_string())
            .expect("first thread should attach");
        let mut second = router
            .take_thread(&second_id.to_string())
            .expect("second thread should attach");

        router.route(turn_completed(&first_id, "turn-1")).await;

        assert_eq!(turn_id(first.recv().await).as_deref(), Some("turn-1"));
        assert!(matches!(first.try_recv(), Err(TryRecvError::Empty)));
        assert!(matches!(second.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn turn_completion_does_not_close_a_thread_route() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let mut receiver = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        router.route(turn_completed(&thread_id, "turn-1")).await;
        router.route(turn_completed(&thread_id, "turn-2")).await;

        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("turn-1"));
        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("turn-2"));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn thread_close_is_delivered_before_the_route_ends() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let mut receiver = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        router
            .route(AppServerEvent::ServerNotification(
                ServerNotification::ThreadClosed(ThreadClosedNotification {
                    thread_id: thread_id.to_string(),
                }),
            ))
            .await;

        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::ThreadClosed(_)
            ))
        ));
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn thread_archive_is_delivered_before_the_route_ends() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let thread_id_string = thread_id.to_string();
        let mut receiver = router
            .take_thread(&thread_id_string)
            .expect("thread should attach");

        router.route(thread_archived(&thread_id)).await;

        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::ThreadArchived(_)
            ))
        ));
        assert!(receiver.recv().await.is_none());
        router
            .prepare_thread(&thread_id_string, ThreadAttachmentKind::Unarchive)
            .expect("archived route should allow a new lifecycle attachment");
    }

    #[test]
    fn delete_is_a_terminal_thread_event() {
        let thread_id = ThreadId::new();
        assert!(thread_event_terminates_stream(
            &AppServerEvent::ServerNotification(ServerNotification::ThreadDeleted(
                ThreadDeletedNotification {
                    thread_id: thread_id.to_string(),
                },
            ))
        ));
    }

    #[tokio::test]
    async fn reservation_survives_a_late_archive_notification() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let reservation = router
            .prepare_thread(&thread_id.to_string(), ThreadAttachmentKind::Unarchive)
            .expect("unarchive route should reserve");

        router.route(thread_archived(&thread_id)).await;
        router
            .route(AppServerEvent::ServerNotification(
                ServerNotification::ThreadUnarchived(ThreadUnarchivedNotification {
                    thread_id: thread_id.to_string(),
                }),
            ))
            .await;
        let mut receiver = reservation.claim().expect("reservation should attach");
        router.route(turn_completed(&thread_id, "fresh")).await;

        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::ThreadUnarchived(_)
            ))
        ));
        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("fresh"));
    }

    #[tokio::test]
    async fn claimed_unarchive_ignores_the_old_archive_until_its_boundary() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let reservation = router
            .prepare_thread(&thread_id.to_string(), ThreadAttachmentKind::Unarchive)
            .expect("unarchive route should reserve");
        let mut receiver = reservation.claim().expect("reservation should attach");

        router.route(thread_archived(&thread_id)).await;
        router
            .route(AppServerEvent::ServerNotification(
                ServerNotification::ThreadUnarchived(ThreadUnarchivedNotification {
                    thread_id: thread_id.to_string(),
                }),
            ))
            .await;
        router.route(turn_completed(&thread_id, "fresh")).await;

        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::ThreadUnarchived(_)
            ))
        ));
        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("fresh"));
    }

    #[tokio::test]
    async fn successful_archive_closes_locally_and_deduplicates_the_notification() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let mut receiver = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        router
            .complete_archive(&thread_id.to_string())
            .expect("archive boundary should be valid");
        router.route(thread_archived(&thread_id)).await;

        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::ThreadArchived(_)
            ))
        ));
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn terminal_delivery_bypasses_a_full_live_queue() {
        let router = EventRouter::new(1);
        let thread_id = ThreadId::new();
        let mut receiver = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");
        router.route(turn_completed(&thread_id, "queued")).await;

        router
            .complete_archive(&thread_id.to_string())
            .expect("archive should not wait for live queue capacity");

        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("queued"));
        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::ThreadArchived(_)
            ))
        ));
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_successful_new_archive_is_not_treated_as_a_stale_notification() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let reservation = router
            .prepare_thread(&thread_id.to_string(), ThreadAttachmentKind::Unarchive)
            .expect("unarchive route should reserve");
        let mut receiver = reservation.claim().expect("reservation should attach");
        router
            .complete_archive(&thread_id.to_string())
            .expect("new archive should establish a terminal boundary");

        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::ThreadArchived(_)
            ))
        ));
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn an_unknown_terminal_event_does_not_create_a_route() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();

        router
            .route(AppServerEvent::ServerNotification(
                ServerNotification::ThreadClosed(ThreadClosedNotification {
                    thread_id: thread_id.to_string(),
                }),
            ))
            .await;

        assert!(!router.state().threads.contains_key(&thread_id));
    }

    #[tokio::test]
    async fn a_terminal_event_after_pending_events_remains_replayable() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        router.route(turn_completed(&thread_id, "pending")).await;
        router
            .route(AppServerEvent::ServerNotification(
                ServerNotification::ThreadClosed(ThreadClosedNotification {
                    thread_id: thread_id.to_string(),
                }),
            ))
            .await;

        let mut receiver = router
            .take_thread(&thread_id.to_string())
            .expect("pending route should remain claimable");
        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("pending"));
        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::ThreadClosed(_)
            ))
        ));
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn inactive_routes_are_bounded() {
        let router = EventRouter::new(CAPACITY);
        for _ in 0..=INACTIVE_ROUTE_CAPACITY {
            let thread_id = ThreadId::new();
            router.route(thread_warning(&thread_id, "pending")).await;
        }

        let state = router.state();
        assert_eq!(state.inactive.len(), INACTIVE_ROUTE_CAPACITY);
        assert_eq!(state.threads.len(), INACTIVE_ROUTE_CAPACITY);
    }

    #[test]
    fn receiver_lease_detaches_its_route() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let receiver = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");
        drop(receiver);

        assert!(matches!(
            router.state().threads.get(&thread_id),
            Some(ThreadRoute::Detached)
        ));
    }

    #[tokio::test]
    async fn runtime_events_are_not_duplicated_into_thread_routes() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let mut runtime = router.take_runtime().expect("runtime stream should attach");
        let mut thread = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        router
            .route(AppServerEvent::ServerNotification(
                ServerNotification::Warning(WarningNotification {
                    thread_id: None,
                    message: "runtime warning".to_string(),
                }),
            ))
            .await;

        assert!(matches!(
            runtime.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::Warning(_)
            ))
        ));
        assert!(matches!(thread.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn lag_is_broadcast_to_runtime_and_thread_routes() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let mut runtime = router.take_runtime().expect("runtime stream should attach");
        let mut thread = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        router.route(AppServerEvent::Lagged { skipped: 7 }).await;

        assert!(matches!(
            runtime.recv().await,
            Some(AppServerEvent::Lagged { skipped: 7 })
        ));
        assert!(matches!(
            thread.recv().await,
            Some(AppServerEvent::Lagged { skipped: 7 })
        ));
    }

    #[tokio::test]
    async fn disconnect_is_delivered_to_every_attached_route() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let mut runtime = router.take_runtime().expect("runtime stream should attach");
        let mut thread = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        router
            .route(AppServerEvent::Disconnected {
                message: "runtime closed".to_string(),
            })
            .await;

        assert!(matches!(
            runtime.recv().await,
            Some(AppServerEvent::Disconnected { .. })
        ));
        assert!(matches!(
            thread.recv().await,
            Some(AppServerEvent::Disconnected { .. })
        ));
        assert!(runtime.recv().await.is_none());
        assert!(thread.recv().await.is_none());
    }

    #[tokio::test]
    async fn replays_events_that_arrive_before_a_thread_is_attached() {
        let router = EventRouter::new(1);
        let thread_id = ThreadId::new();

        router.route(turn_completed(&thread_id, "turn-1")).await;
        router.route(turn_completed(&thread_id, "turn-2")).await;
        let mut receiver = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("turn-1"));
        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("turn-2"));
    }

    #[test]
    fn each_route_has_only_one_receiver() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let _runtime = router.take_runtime().expect("runtime stream should attach");
        let _thread = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        assert!(matches!(
            router.take_runtime(),
            Err(Error::CodexEventStreamTaken)
        ));
        assert!(matches!(
            router.take_thread(&thread_id.to_string()),
            Err(Error::ThreadEventStreamTaken { .. })
        ));
    }

    #[tokio::test]
    async fn a_dropped_runtime_stream_cannot_be_taken_again() {
        let router = EventRouter::new(CAPACITY);
        let runtime = router.take_runtime().expect("runtime stream should attach");
        drop(runtime);

        router
            .route(AppServerEvent::ServerNotification(
                ServerNotification::Warning(WarningNotification {
                    thread_id: None,
                    message: "after drop".to_string(),
                }),
            ))
            .await;

        assert!(matches!(
            router.take_runtime(),
            Err(Error::CodexEventStreamTaken)
        ));
    }

    #[tokio::test]
    async fn reservation_is_exclusive_and_cancellation_discards_late_events() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let thread_id_string = thread_id.to_string();
        let reservation = router
            .prepare_thread(&thread_id_string, ThreadAttachmentKind::Resume)
            .expect("first reservation should succeed");
        assert!(matches!(
            router.prepare_thread(&thread_id_string, ThreadAttachmentKind::Resume),
            Err(Error::ThreadLifecycleInProgress { .. })
        ));

        drop(reservation);
        router.route(turn_completed(&thread_id, "stale")).await;

        let reservation = router
            .prepare_thread(&thread_id_string, ThreadAttachmentKind::Resume)
            .expect("cancelled route should be reusable");
        let mut receiver = reservation.claim().expect("reservation should attach");
        router.route(turn_completed(&thread_id, "fresh")).await;
        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("fresh"));
    }

    #[tokio::test]
    async fn a_closed_thread_route_can_be_reserved_again() {
        let router = EventRouter::new(CAPACITY);
        let thread_id = ThreadId::new();
        let thread_id_string = thread_id.to_string();
        let mut first = router
            .take_thread(&thread_id_string)
            .expect("first thread stream should attach");

        router
            .route(AppServerEvent::ServerNotification(
                ServerNotification::ThreadClosed(ThreadClosedNotification {
                    thread_id: thread_id_string.clone(),
                }),
            ))
            .await;
        assert!(first.recv().await.is_some());
        assert!(first.recv().await.is_none());

        let reservation = router
            .prepare_thread(&thread_id_string, ThreadAttachmentKind::Resume)
            .expect("closed route should be reusable");
        let mut second = reservation.claim().expect("reserved stream should attach");
        router
            .route(turn_completed(&thread_id, "turn-after-resume"))
            .await;

        assert_eq!(
            turn_id(second.recv().await).as_deref(),
            Some("turn-after-resume")
        );
    }

    #[tokio::test]
    async fn best_effort_overflow_is_reported_before_a_required_event() {
        let router = EventRouter::new(1);
        let thread_id = ThreadId::new();
        let mut receiver = router
            .take_thread(&thread_id.to_string())
            .expect("thread should attach");

        router.route(thread_warning(&thread_id, "first")).await;
        router.route(thread_warning(&thread_id, "dropped")).await;

        let routing = {
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                router.route(turn_completed(&thread_id, "required")).await;
            })
        };

        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::ServerNotification(
                ServerNotification::Warning(_)
            ))
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(AppServerEvent::Lagged { skipped: 1 })
        ));
        assert_eq!(turn_id(receiver.recv().await).as_deref(), Some("required"));
        routing.await.expect("routing task should finish");
    }

    fn thread_warning(thread_id: &ThreadId, message: &str) -> AppServerEvent {
        AppServerEvent::ServerNotification(ServerNotification::Warning(
            WarningNotification {
                thread_id: Some(thread_id.to_string()),
                message: message.to_string(),
            },
        ))
    }

    fn thread_archived(thread_id: &ThreadId) -> AppServerEvent {
        AppServerEvent::ServerNotification(ServerNotification::ThreadArchived(
            ThreadArchivedNotification {
                thread_id: thread_id.to_string(),
            },
        ))
    }

    fn turn_completed(thread_id: &ThreadId, turn_id: &str) -> AppServerEvent {
        AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(
            TurnCompletedNotification {
                thread_id: thread_id.to_string(),
                turn: Turn {
                    id: turn_id.to_string(),
                    items: Vec::new(),
                    items_view: TurnItemsView::Full,
                    status: TurnStatus::Completed,
                    error: None,
                    started_at: Some(1),
                    completed_at: Some(2),
                    duration_ms: Some(1_000),
                },
            },
        ))
    }

    fn turn_id(event: Option<AppServerEvent>) -> Option<String> {
        let AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(
            completed,
        )) = event?
        else {
            return None;
        };
        Some(completed.turn.id)
    }
}
