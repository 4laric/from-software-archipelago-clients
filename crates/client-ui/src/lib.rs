//! Renderer- and game-independent state exchanged between a client worker and a UI host.
//!
//! A host only reads immutable [`ClientSnapshot`] values and sends [`UiAction`] values back to
//! the worker. In particular, rendering code never receives a game-process handle. This keeps a
//! stalled or crashed renderer outside the delivery acknowledgement path.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessState {
    #[default]
    Waiting,
    Attaching,
    Attached,
    Lost,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApState {
    #[default]
    Disconnected,
    Connecting,
    Authenticated,
    Reconnecting,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryState {
    #[default]
    NotArmed,
    WaitingForGameplay,
    Ready,
    CommandPending,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityKind {
    Message,
    LocationCheck,
    ReceivedItem,
    StorageDelivery,
    ParkedDelivery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// Monotonic, source-owned sequence number. Hosts must preserve this order.
    pub sequence: u64,
    pub kind: ActivityKind,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerTotals {
    pub queued: u32,
    pub delivered: u32,
    pub storage_routed: u32,
    pub parked: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSnapshot {
    pub revision: u64,
    pub process: ProcessState,
    pub ap: ApState,
    pub delivery: DeliveryState,
    pub server: Option<String>,
    pub slot: Option<String>,
    pub seed: Option<String>,
    pub goal: Option<String>,
    pub go_mode: Option<bool>,
    pub ledger: LedgerTotals,
    pub activity: VecDeque<ActivityEvent>,
    pub stale: bool,
}

impl ClientSnapshot {
    /// Appends an activity entry while retaining a bounded, ordered view for the renderer.
    pub fn push_activity(&mut self, event: ActivityEvent, capacity: usize) {
        if self
            .activity
            .back()
            .is_some_and(|previous| event.sequence <= previous.sequence)
        {
            return;
        }
        if capacity == 0 {
            self.activity.clear();
            return;
        }
        while self.activity.len() >= capacity {
            self.activity.pop_front();
        }
        self.activity.push_back(event);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAction {
    Connect {
        server: String,
        slot: String,
        password: Option<String>,
    },
    Disconnect,
    SubmitCommand(String),
    OpenSessionFolder,
    RequestShutdown,
}

/// Bounded, non-blocking bridge. Snapshot publication coalesces to the newest revision so a slow
/// renderer cannot back-pressure networking or delivery.
pub struct UiBridge {
    snapshot: Arc<Mutex<Option<ClientSnapshot>>>,
    actions_tx: SyncSender<UiAction>,
    actions_rx: Receiver<UiAction>,
}

impl UiBridge {
    pub fn new(action_capacity: usize) -> Self {
        let (actions_tx, actions_rx) = sync_channel(action_capacity);
        Self {
            snapshot: Arc::new(Mutex::new(None)),
            actions_tx,
            actions_rx,
        }
    }

    pub fn split(self) -> (ClientEndpoint, HostEndpoint) {
        (
            ClientEndpoint {
                snapshot: Arc::clone(&self.snapshot),
                actions_rx: self.actions_rx,
            },
            HostEndpoint {
                snapshot: self.snapshot,
                actions_tx: self.actions_tx,
            },
        )
    }
}

pub struct ClientEndpoint {
    snapshot: Arc<Mutex<Option<ClientSnapshot>>>,
    actions_rx: Receiver<UiAction>,
}

impl ClientEndpoint {
    /// Replaces the pending snapshot. A slow renderer observes the newest complete state without
    /// building a backlog. The lock is held only for the pointer-sized option replacement.
    pub fn publish(&self, snapshot: ClientSnapshot) {
        *self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(snapshot);
    }

    pub fn try_action(&self) -> Result<UiAction, TryRecvError> {
        self.actions_rx.try_recv()
    }
}

pub struct HostEndpoint {
    snapshot: Arc<Mutex<Option<ClientSnapshot>>>,
    actions_tx: SyncSender<UiAction>,
}

impl HostEndpoint {
    /// Takes the newest unpublished state, if any.
    pub fn latest_snapshot(&self) -> Option<ClientSnapshot> {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub fn send_action(&self, action: UiAction) -> Result<(), TrySendError<UiAction>> {
        self.actions_tx.try_send(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_is_ordered_and_bounded() {
        let mut state = ClientSnapshot::default();
        for sequence in [1, 2, 2, 1, 3] {
            state.push_activity(
                ActivityEvent {
                    sequence,
                    kind: ActivityKind::Message,
                    text: sequence.to_string(),
                },
                2,
            );
        }
        assert_eq!(
            state
                .activity
                .iter()
                .map(|e| e.sequence)
                .collect::<Vec<_>>(),
            [2, 3]
        );
    }

    #[test]
    fn a_stalled_host_gets_the_newest_snapshot() {
        let (client, host) = UiBridge::new(1).split();
        client.publish(ClientSnapshot {
            revision: 1,
            ..Default::default()
        });
        client.publish(ClientSnapshot {
            revision: 2,
            ..Default::default()
        });
        assert_eq!(host.latest_snapshot().unwrap().revision, 2);
        assert!(host.latest_snapshot().is_none());
    }

    #[test]
    fn actions_are_bounded_and_non_blocking() {
        let (_client, host) = UiBridge::new(1).split();
        assert!(host.send_action(UiAction::Disconnect).is_ok());
        assert!(matches!(
            host.send_action(UiAction::RequestShutdown),
            Err(TrySendError::Full(_))
        ));
    }
}
