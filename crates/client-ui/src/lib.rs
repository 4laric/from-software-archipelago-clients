//! Renderer- and game-independent state exchanged between a client worker and a UI host.
//!
//! A host only reads immutable [`ClientSnapshot`] values and sends [`UiAction`] values back to
//! the worker. In particular, rendering code never receives a game-process handle. This keeps a
//! stalled or crashed renderer outside the delivery acknowledgement path.

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// The item at the front of the queue has not moved past its budget. Nothing behind it
    /// delivers until it does; `delivery_detail` says why, in the client's own words.
    Blocked,
    /// Deliveries are flowing, but one or more earlier items are parked in the ledger for an
    /// operator to inspect. A park never holds up the queue.
    Parked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityKind {
    Message,
    Command,
    CommandResult,
    LocationCheck,
    ReceivedItem,
    StorageDelivery,
    ParkedDelivery,
    /// A failure the player needs to see: an Archipelago socket/protocol error, or a client-side
    /// fault the worker chose to surface. Distinct from `Message` so a renderer can colour it and
    /// a filter can keep it visible when chat is hidden.
    Error,
    /// An Archipelago hint. Separate from `Message` because hints are the one server print players
    /// actively hunt for in a long feed.
    Hint,
}

/// Archipelago's item classification, as carried on `NetworkItem.flags`. Feed rows that name
/// an item are coloured by it, in the colours the Archipelago text client and Universal Tracker
/// use, so a player reads the same meaning here as everywhere else in their multiworld.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemClass {
    Progression,
    Useful,
    Trap,
    Filler,
}

impl ItemClass {
    /// From the raw Archipelago flag bits: progression wins over useful, useful over trap.
    pub fn from_flags(progression: bool, useful: bool, trap: bool) -> Self {
        if progression {
            Self::Progression
        } else if useful {
            Self::Useful
        } else if trap {
            Self::Trap
        } else {
            Self::Filler
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// Monotonic, source-owned sequence number. Hosts must preserve this order.
    pub sequence: u64,
    pub kind: ActivityKind,
    pub text: String,
    /// Wall-clock milliseconds since the Unix epoch, stamped by the producer when the event was
    /// pushed.  Milliseconds rather than a `SystemTime` so the value serialises, compares and
    /// tests as a plain number; a renderer that wants `HH:MM:SS` converts it once.
    ///
    /// `#[serde(default)]` keeps older serialised feeds readable: an absent stamp reads as 0,
    /// which a renderer treats as "no time known" rather than as midnight 1970.
    #[serde(default)]
    pub timestamp_ms: u64,
    /// The Archipelago classification of the item this row names, when it names one. Absent for
    /// rows that are not about an item and for older serialised feeds.
    #[serde(default)]
    pub item_class: Option<ItemClass>,
}

impl ActivityEvent {
    /// Stamped constructor. Producers should prefer [`SnapshotReducer::activity`], which owns the
    /// sequence numbering as well.
    pub fn now(sequence: u64, kind: ActivityKind, text: impl Into<String>) -> Self {
        Self {
            sequence,
            kind,
            text: text.into(),
            timestamp_ms: unix_millis_now(),
            item_class: None,
        }
    }

    pub fn with_item_class(mut self, item_class: Option<ItemClass>) -> Self {
        self.item_class = item_class;
        self
    }
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            since.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerTotals {
    pub queued: u32,
    pub delivered: u32,
    /// Confirmed storage routes when a producer has loaded destination diagnostics. `None` means
    /// unknown, not zero; ordinary ledger acknowledgement cannot prove a native destination.
    pub storage_routed: Option<u32>,
    pub parked: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationTotals {
    pub checked: u32,
    pub total: u32,
}

/// One unchecked location as advertised by the connected world. `region` is optional because
/// older seed contracts do not provide grouping metadata; renderers must keep those rows visible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncheckedLocation {
    pub name: String,
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationGroup {
    pub region: String,
    pub locations: Vec<String>,
}

/// Deterministically group unchecked locations without inferring world logic from their names.
/// Missing and blank region metadata is deliberately retained under `Other`.
pub fn group_unchecked_locations(locations: &[UncheckedLocation]) -> Vec<LocationGroup> {
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for location in locations {
        let region = location
            .region
            .as_deref()
            .map(str::trim)
            .filter(|region| !region.is_empty())
            .unwrap_or("Other");
        groups
            .entry(region.to_owned())
            .or_default()
            .push(location.name.clone());
    }
    groups
        .into_iter()
        .map(|(region, mut locations)| {
            locations.sort();
            LocationGroup { region, locations }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VictorySummary {
    pub goal: String,
    pub completed_at_ms: u64,
    pub elapsed_seconds: Option<u64>,
    pub checks_completed: Option<u32>,
    pub checks_total: Option<u32>,
    pub received_items: Option<u32>,
    pub sent_items: Option<u32>,
    pub deaths: Option<u32>,
    pub death_links: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedEntry {
    pub index: u64,
    pub item_name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSnapshot {
    pub revision: u64,
    pub process: ProcessState,
    pub ap: ApState,
    pub delivery: DeliveryState,
    /// Why delivery is in its current state, when the worker knows and the reason is worth a
    /// player's attention.  Populated for [`DeliveryState::Blocked`] (the harness `status
    /// (detail)` the worker already prints); ignored for every other state.
    #[serde(default)]
    pub delivery_detail: Option<String>,
    pub server: Option<String>,
    pub slot: Option<String>,
    pub seed: Option<String>,
    /// Human-readable binary/runtime identity shown persistently by renderers.
    #[serde(default)]
    pub version: Option<String>,
    pub goal: Option<String>,
    pub go_mode: Option<bool>,
    #[serde(default)]
    pub victory: Option<VictorySummary>,
    pub locations: Option<LocationTotals>,
    #[serde(default)]
    pub unchecked_locations: Vec<UncheckedLocation>,
    pub ledger: LedgerTotals,
    #[serde(default)]
    pub blocked: Vec<BlockedEntry>,
    #[serde(default)]
    pub save_identity: Option<String>,
    #[serde(default)]
    pub gameplay_ready: bool,
    #[serde(default)]
    pub receive_cursor: Option<u64>,
    pub activity: VecDeque<ActivityEvent>,
    pub stale: bool,
}

/// Facts owned by the client worker after one delivery poll.  The UI reducer deliberately
/// consumes outcomes the delivery machine has already validated; it never probes game memory or
/// invents readiness from process attachment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeliveryFacts {
    pub process: ProcessState,
    pub ap: ApState,
    pub delivery: DeliveryState,
    pub delivery_detail: Option<String>,
    pub server: Option<String>,
    pub slot: Option<String>,
    pub seed: Option<String>,
    pub version: Option<String>,
    pub goal: Option<String>,
    pub go_mode: Option<bool>,
    pub victory: Option<VictorySummary>,
    pub locations: Option<LocationTotals>,
    pub unchecked_locations: Vec<UncheckedLocation>,
    pub ledger: LedgerTotals,
    pub blocked: Vec<BlockedEntry>,
    pub save_identity: Option<String>,
    pub gameplay_ready: bool,
    pub receive_cursor: Option<u64>,
}

/// Stateful projection from validated worker facts to immutable renderer snapshots.
/// Activity survives ordinary polling updates and is sequence-numbered and bounded here rather
/// than in a renderer, so replacing or restarting a renderer cannot reorder delivery history.
#[derive(Clone, Debug)]
pub struct SnapshotReducer {
    snapshot: ClientSnapshot,
    activity_capacity: usize,
    next_sequence: u64,
}

/// Default retained activity depth.  Raised from 100 when the feed became scrollable: a renderer
/// that virtualises can show a whole session's worth of history, and a player diagnosing a missed
/// delivery scrolls back rather than reopening `client.log`.
pub const DEFAULT_ACTIVITY_CAPACITY: usize = 500;

impl Default for SnapshotReducer {
    fn default() -> Self {
        Self::new(DEFAULT_ACTIVITY_CAPACITY)
    }
}

impl SnapshotReducer {
    pub fn new(activity_capacity: usize) -> Self {
        Self {
            snapshot: ClientSnapshot::default(),
            activity_capacity,
            next_sequence: 1,
        }
    }

    pub fn reduce(&mut self, facts: DeliveryFacts) -> ClientSnapshot {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
        self.snapshot.process = facts.process;
        self.snapshot.ap = facts.ap;
        self.snapshot.delivery = facts.delivery;
        self.snapshot.delivery_detail = facts.delivery_detail;
        self.snapshot.server = facts.server;
        self.snapshot.slot = facts.slot;
        self.snapshot.seed = facts.seed;
        self.snapshot.version = facts.version;
        self.snapshot.goal = facts.goal;
        self.snapshot.go_mode = facts.go_mode;
        self.snapshot.victory = facts.victory;
        self.snapshot.locations = facts.locations;
        self.snapshot.unchecked_locations = facts.unchecked_locations;
        self.snapshot.ledger = facts.ledger;
        self.snapshot.blocked = facts.blocked;
        self.snapshot.save_identity = facts.save_identity;
        self.snapshot.gameplay_ready = facts.gameplay_ready;
        self.snapshot.receive_cursor = facts.receive_cursor;
        self.snapshot.stale = false;
        self.snapshot.clone()
    }

    pub fn activity(&mut self, kind: ActivityKind, text: impl Into<String>) {
        self.activity_with_class(kind, text, None);
    }

    /// An activity row that names an item of a known Archipelago class.
    pub fn activity_with_class(
        &mut self,
        kind: ActivityKind,
        text: impl Into<String>,
        item_class: Option<ItemClass>,
    ) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.snapshot.push_activity(
            ActivityEvent::now(sequence, kind, text).with_item_class(item_class),
            self.activity_capacity,
        );
    }

    pub fn mark_stale(&mut self) -> ClientSnapshot {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
        self.snapshot.stale = true;
        self.snapshot.clone()
    }
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

/// How loudly a renderer should present a line. Deliberately three levels: a player reading an
/// overlay at a glance distinguishes "fine", "wait", and "broken", and nothing finer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Ok,
    Warn,
    Bad,
}

/// The one-sentence delivery status, in player language.
///
/// This lives here rather than in a renderer for two reasons: the debug-formatted enum names
/// (`WaitingForGameplay`, `NotArmed`) that used to reach the window are a presentation decision,
/// and a presentation decision that every future renderer must make identically belongs in one
/// tested function. Renderers must not re-derive this from the raw enum.
pub fn delivery_headline(snapshot: &ClientSnapshot) -> (Severity, String) {
    match snapshot.delivery {
        DeliveryState::NotArmed => (
            Severity::Bad,
            "Item delivery is not armed - open diagnostics and fix setup before playing."
                .to_owned(),
        ),
        DeliveryState::WaitingForGameplay => (
            Severity::Warn,
            "Waiting for you to gain control in-game...".to_owned(),
        ),
        DeliveryState::Ready => (Severity::Ok, "Delivering normally.".to_owned()),
        DeliveryState::CommandPending => (Severity::Warn, "Command in flight...".to_owned()),
        DeliveryState::Blocked => (
            Severity::Bad,
            match snapshot.delivery_detail.as_deref() {
                // An empty reason is treated as no reason: "Delivery stalled: " with nothing after
                // the colon reads as a truncated string, which is worse than the bare sentence.
                Some(reason) if !reason.trim().is_empty() => {
                    format!("Delivery stalled: {}", reason.trim())
                }
                _ => "Delivery stalled - the item at the front of the queue has not moved."
                    .to_owned(),
            },
        ),
        DeliveryState::Parked => (
            Severity::Warn,
            match snapshot.delivery_detail.as_deref() {
                Some(reason) if !reason.trim().is_empty() => {
                    format!("Delivering; {}", reason.trim())
                }
                _ => "Delivering; some items are parked - type `blocked` to inspect.".to_owned(),
            },
        ),
    }
}

/// How long a changed headline must remain true before replacing the one already on screen.
pub const GUIDANCE_DEBOUNCE_MS: u64 = 10_000;

/// The highest-priority session fact, phrased as one player-facing action/status sentence.
///
/// Delivery failures win over setup, setup wins over connection, and connection wins over healthy
/// delivery. This order is centralized so renderers cannot disagree when several raw states move
/// during the same loading screen.
pub fn guidance_candidate(snapshot: &ClientSnapshot) -> (Severity, String) {
    if snapshot.delivery == DeliveryState::Blocked {
        return delivery_headline(snapshot);
    }
    if snapshot.delivery == DeliveryState::NotArmed {
        return delivery_headline(snapshot);
    }
    match snapshot.process {
        ProcessState::Lost => {
            return (
                Severity::Bad,
                "Bloodborne closed or detached - restart the game to resume.".to_owned(),
            );
        }
        ProcessState::Waiting | ProcessState::Attaching => {
            return (
                Severity::Warn,
                "Waiting for Bloodborne - start the game and load your character.".to_owned(),
            );
        }
        ProcessState::Attached => {}
    }
    match snapshot.ap {
        ApState::Disconnected => (
            Severity::Bad,
            "Not connected to Archipelago - check the server and slot, then connect.".to_owned(),
        ),
        ApState::Connecting => (Severity::Warn, "Connecting to Archipelago...".to_owned()),
        ApState::Reconnecting => (
            Severity::Warn,
            "Connection lost - reconnecting to Archipelago...".to_owned(),
        ),
        ApState::Authenticated => delivery_headline(snapshot),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GuidanceLine(Severity, String);

/// Holds transient loading-state changes for [`GUIDANCE_DEBOUNCE_MS`] before publishing them.
#[derive(Clone, Debug, Default)]
pub struct GuidanceGate {
    shown: Option<GuidanceLine>,
    pending: Option<(GuidanceLine, u64)>,
}

impl GuidanceGate {
    pub fn observe(&mut self, snapshot: &ClientSnapshot, now_ms: u64) -> (Severity, String) {
        let (severity, text) = guidance_candidate(snapshot);
        let candidate = GuidanceLine(severity, text);
        let Some(shown) = self.shown.as_ref() else {
            self.shown = Some(candidate.clone());
            return (candidate.0, candidate.1);
        };
        if shown == &candidate {
            self.pending = None;
            return (shown.0, shown.1.clone());
        }

        match self.pending.as_mut() {
            Some((pending, since)) if pending == &candidate => {
                if now_ms.saturating_sub(*since) >= GUIDANCE_DEBOUNCE_MS {
                    self.shown = Some(candidate.clone());
                    self.pending = None;
                    (candidate.0, candidate.1)
                } else {
                    (shown.0, shown.1.clone())
                }
            }
            _ => {
                self.pending = Some((candidate, now_ms));
                (shown.0, shown.1.clone())
            }
        }
    }
}

/// The full-width banner shown while [`ClientSnapshot::stale`] is set. Separate from
/// [`delivery_headline`] because staleness invalidates every other reading on the window: the
/// worker stopped reporting, so the delivery state on screen is the last one it managed to send,
/// not the current one.
pub const STALE_BANNER: &str = "Client state is stale - the worker stopped reporting.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAction {
    Connect {
        server: String,
        slot: String,
        password: Option<String>,
    },
    Disconnect,
    SubmitCommand(String),
    RetryBlocked {
        index: u64,
    },
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

#[derive(Clone)]
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
    fn a_cloned_host_endpoint_can_take_over_the_same_ui_bridge() {
        let (client, host) = UiBridge::new(2).split();
        let fallback = host.clone();
        drop(host);

        let snapshot = ClientSnapshot {
            slot: Some("fallback hunter".into()),
            ..ClientSnapshot::default()
        };
        client.publish(snapshot);
        assert_eq!(
            fallback
                .latest_snapshot()
                .and_then(|snapshot| snapshot.slot),
            Some("fallback hunter".into())
        );

        fallback
            .send_action(UiAction::SubmitCommand("still alive".into()))
            .expect("fallback action");
        assert_eq!(
            client.try_action().expect("shared action channel"),
            UiAction::SubmitCommand("still alive".into())
        );
    }

    #[test]
    fn activity_is_ordered_and_bounded() {
        let mut state = ClientSnapshot::default();
        for sequence in [1, 2, 2, 1, 3] {
            state.push_activity(
                ActivityEvent::now(sequence, ActivityKind::Message, sequence.to_string()),
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

    #[test]
    fn reducer_preserves_activity_and_uses_explicit_delivery_facts() {
        let mut reducer = SnapshotReducer::new(2);
        reducer.activity(ActivityKind::ReceivedItem, "Saw Cleaver");
        let first = reducer.reduce(DeliveryFacts {
            process: ProcessState::Attached,
            ap: ApState::Authenticated,
            delivery: DeliveryState::CommandPending,
            ..Default::default()
        });
        let second = reducer.reduce(DeliveryFacts {
            process: ProcessState::Attached,
            ap: ApState::Authenticated,
            delivery: DeliveryState::Ready,
            ..Default::default()
        });
        assert_eq!(first.delivery, DeliveryState::CommandPending);
        assert_eq!(second.delivery, DeliveryState::Ready);
        assert_eq!(second.activity.len(), 1);
        assert_eq!(second.revision, first.revision + 1);
    }

    #[test]
    fn reducer_staleness_is_explicit_and_cleared_by_fresh_facts() {
        let mut reducer = SnapshotReducer::new(4);
        assert!(reducer.mark_stale().stale);
        assert!(!reducer.reduce(DeliveryFacts::default()).stale);
    }

    #[test]
    fn reducer_replaces_structured_blocked_rows_without_spending_activity_capacity() {
        let mut reducer = SnapshotReducer::new(1);
        reducer.activity(ActivityKind::Message, "kept");
        let entry = BlockedEntry {
            index: 3,
            item_name: "Fire Paper x2".into(),
            reason: "quantity mismatch".into(),
        };
        let present = reducer.reduce(DeliveryFacts {
            blocked: vec![entry.clone()],
            save_identity: Some("slot-1".into()),
            gameplay_ready: true,
            receive_cursor: Some(9),
            ..Default::default()
        });
        assert_eq!(present.blocked, vec![entry]);
        assert_eq!(present.activity.len(), 1);
        assert_eq!(present.save_identity.as_deref(), Some("slot-1"));
        assert_eq!(present.receive_cursor, Some(9));

        let absent = reducer.reduce(DeliveryFacts::default());
        assert!(absent.blocked.is_empty());
        assert_eq!(absent.activity.len(), 1);
    }

    #[test]
    fn full_activity_and_rescue_snapshot_stays_small_enough_to_coalesce() {
        let mut reducer = SnapshotReducer::default();
        for index in 0..DEFAULT_ACTIVITY_CAPACITY {
            reducer.activity(ActivityKind::Message, format!("activity {index}"));
        }
        let blocked = (0..100)
            .map(|index| BlockedEntry {
                index,
                item_name: format!("Item {index}"),
                reason: "bounded terminal delivery reason".into(),
            })
            .collect();
        let snapshot = reducer.reduce(DeliveryFacts {
            blocked,
            ..Default::default()
        });
        let rendered = format!("{snapshot:?}");
        assert!(
            rendered.len() < 128 * 1024,
            "snapshot grew to {} debug bytes",
            rendered.len()
        );
    }
    #[test]
    fn delivery_headline_covers_every_variant_in_player_language() {
        // The table is exhaustive on purpose: adding a DeliveryState without a sentence should
        // fail here rather than reach a player as a debug-formatted enum name.
        let headline = |delivery, detail: Option<&str>| {
            delivery_headline(&ClientSnapshot {
                delivery,
                delivery_detail: detail.map(str::to_owned),
                ..Default::default()
            })
        };

        assert_eq!(
            headline(DeliveryState::NotArmed, None),
            (
                Severity::Bad,
                "Item delivery is not armed - open diagnostics and fix setup before playing."
                    .to_owned()
            )
        );
        assert_eq!(
            headline(DeliveryState::WaitingForGameplay, None),
            (
                Severity::Warn,
                "Waiting for you to gain control in-game...".to_owned()
            )
        );
        assert_eq!(
            headline(DeliveryState::Ready, None),
            (Severity::Ok, "Delivering normally.".to_owned())
        );
        assert_eq!(
            headline(DeliveryState::CommandPending, None),
            (Severity::Warn, "Command in flight...".to_owned())
        );
        assert_eq!(
            headline(DeliveryState::Blocked, Some("harness refused (no slot)")),
            (
                Severity::Bad,
                "Delivery stalled: harness refused (no slot)".to_owned()
            )
        );
    }

    #[test]
    fn a_parked_delivery_is_a_warning_that_says_delivery_continues() {
        let (severity, text) = delivery_headline(&ClientSnapshot {
            delivery: DeliveryState::Parked,
            delivery_detail: Some(
                "1 parked: Butcher Gloves (unreviewed_attire); type `blocked` to inspect".into(),
            ),
            ..Default::default()
        });
        assert_eq!(severity, Severity::Warn);
        assert!(text.starts_with("Delivering; 1 parked"), "{text}");
        let (severity, text) = delivery_headline(&ClientSnapshot {
            delivery: DeliveryState::Parked,
            ..Default::default()
        });
        assert_eq!(severity, Severity::Warn);
        assert!(text.contains("type `blocked`"), "{text}");
        // A park does not outrank connection or setup guidance the way a stall does.
        let guidance = guidance_candidate(&ClientSnapshot {
            delivery: DeliveryState::Parked,
            process: ProcessState::Lost,
            ..Default::default()
        });
        assert!(guidance.1.contains("restart the game"), "{}", guidance.1);
    }

    #[test]
    fn a_blocked_delivery_without_a_usable_reason_still_reads_as_a_sentence() {
        for detail in [None, Some(""), Some("   ")] {
            let (severity, text) = delivery_headline(&ClientSnapshot {
                delivery: DeliveryState::Blocked,
                delivery_detail: detail.map(str::to_owned),
                ..Default::default()
            });
            assert_eq!(severity, Severity::Bad);
            assert_eq!(
                text,
                "Delivery stalled - the item at the front of the queue has not moved."
            );
        }
    }

    #[test]
    fn a_detail_is_ignored_unless_delivery_is_blocked() {
        // A stale reason left over from an earlier block must not caption a healthy delivery.
        let (severity, text) = delivery_headline(&ClientSnapshot {
            delivery: DeliveryState::Ready,
            delivery_detail: Some("harness refused (no slot)".into()),
            ..Default::default()
        });
        assert_eq!(
            (severity, text.as_str()),
            (Severity::Ok, "Delivering normally.")
        );
    }

    #[test]
    fn guidance_priority_is_blocked_then_setup_then_connection_then_delivery() {
        let snapshot = |process, ap, delivery| ClientSnapshot {
            process,
            ap,
            delivery,
            ..Default::default()
        };
        assert!(
            guidance_candidate(&snapshot(
                ProcessState::Lost,
                ApState::Disconnected,
                DeliveryState::Blocked
            ))
            .1
            .starts_with("Delivery stalled")
        );
        assert!(
            guidance_candidate(&snapshot(
                ProcessState::Lost,
                ApState::Disconnected,
                DeliveryState::Ready
            ))
            .1
            .starts_with("Bloodborne closed")
        );
        assert!(
            guidance_candidate(&snapshot(
                ProcessState::Attached,
                ApState::Disconnected,
                DeliveryState::Ready
            ))
            .1
            .starts_with("Not connected")
        );
        assert_eq!(
            guidance_candidate(&snapshot(
                ProcessState::Attached,
                ApState::Authenticated,
                DeliveryState::Ready
            )),
            (Severity::Ok, "Delivering normally.".to_owned())
        );
    }

    #[test]
    fn guidance_debounces_loading_flaps_and_commits_a_stable_change() {
        let ready = ClientSnapshot {
            process: ProcessState::Attached,
            ap: ApState::Authenticated,
            delivery: DeliveryState::Ready,
            ..Default::default()
        };
        let loading = ClientSnapshot {
            process: ProcessState::Attaching,
            ..ready.clone()
        };
        let mut gate = GuidanceGate::default();
        assert_eq!(gate.observe(&ready, 0).1, "Delivering normally.");
        assert_eq!(gate.observe(&loading, 1_000).1, "Delivering normally.");
        assert_eq!(gate.observe(&ready, 5_000).1, "Delivering normally.");
        assert_eq!(gate.observe(&loading, 6_000).1, "Delivering normally.");
        assert_eq!(gate.observe(&loading, 15_999).1, "Delivering normally.");
        assert!(gate.observe(&loading, 16_000).1.starts_with("Waiting for"));
    }

    #[test]
    fn reducer_stamps_activity_with_a_wall_clock_time() {
        let mut reducer = SnapshotReducer::default();
        reducer.activity(ActivityKind::Hint, "Saw Cleaver is at Central Yharnam");
        let snapshot = reducer.reduce(DeliveryFacts::default());
        let event = snapshot.activity.back().expect("one event");
        assert_eq!(event.kind, ActivityKind::Hint);
        // 2020-01-01 in epoch millis: any real clock is past it, and a zero would mean unstamped.
        assert!(event.timestamp_ms > 1_577_836_800_000);
    }

    #[test]
    fn the_default_reducer_retains_a_whole_session_of_activity() {
        assert_eq!(DEFAULT_ACTIVITY_CAPACITY, 500);
        let mut reducer = SnapshotReducer::default();
        for index in 0..600 {
            reducer.activity(ActivityKind::Message, format!("line {index}"));
        }
        let snapshot = reducer.reduce(DeliveryFacts::default());
        assert_eq!(snapshot.activity.len(), 500);
        assert_eq!(snapshot.activity.back().unwrap().text, "line 599");
    }

    #[test]
    fn a_delivery_detail_is_carried_and_cleared_by_the_reducer() {
        let mut reducer = SnapshotReducer::default();
        let blocked = reducer.reduce(DeliveryFacts {
            delivery: DeliveryState::Blocked,
            delivery_detail: Some("refused (no slot)".into()),
            ..Default::default()
        });
        assert_eq!(
            blocked.delivery_detail.as_deref(),
            Some("refused (no slot)")
        );
        let recovered = reducer.reduce(DeliveryFacts {
            delivery: DeliveryState::Ready,
            ..Default::default()
        });
        assert_eq!(recovered.delivery_detail, None);
    }

    #[test]
    fn unchecked_locations_group_deterministically_and_keep_unknown_rows() {
        let groups = group_unchecked_locations(&[
            UncheckedLocation {
                name: "Old Yharnam second".into(),
                region: Some("Old Yharnam".into()),
            },
            UncheckedLocation {
                name: "unknown".into(),
                region: None,
            },
            UncheckedLocation {
                name: "Old Yharnam first".into(),
                region: Some("Old Yharnam".into()),
            },
            UncheckedLocation {
                name: "blank metadata".into(),
                region: Some("  ".into()),
            },
        ]);

        assert_eq!(
            groups,
            vec![
                LocationGroup {
                    region: "Old Yharnam".into(),
                    locations: vec!["Old Yharnam first".into(), "Old Yharnam second".into()],
                },
                LocationGroup {
                    region: "Other".into(),
                    locations: vec!["blank metadata".into(), "unknown".into()],
                },
            ]
        );
    }

    #[test]
    fn reducer_replaces_the_unchecked_location_snapshot_on_reconnect() {
        let mut reducer = SnapshotReducer::default();
        let before = reducer.reduce(DeliveryFacts {
            unchecked_locations: vec![UncheckedLocation {
                name: "Cleric Beast".into(),
                region: Some("Central Yharnam".into()),
            }],
            ..Default::default()
        });
        assert_eq!(before.unchecked_locations.len(), 1);

        let after = reducer.reduce(DeliveryFacts::default());
        assert!(after.unchecked_locations.is_empty());
    }
}
