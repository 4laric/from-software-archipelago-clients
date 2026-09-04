//! Presentation decisions, with no renderer in them.
//!
//! Everything a player reads on the standalone window is derived here: the colour a feed row gets,
//! the sentence under the status pills, the shape of the items line. None of it touches egui, so
//! all of it builds and tests on any host -- which is the point. The previous shell made these
//! decisions inline inside a Win32 message loop, where the only way to check that `WaitingForGameplay`
//! reached a player as a debug-formatted enum name was to launch the game.

use client_ui::{
    ActivityEvent, ActivityKind, ApState, ClientSnapshot, DeliveryState, ItemClass, LedgerTotals,
    ProcessState,
};

pub const TOAST_LIFETIME_MS: u64 = 4_000;
pub const TOAST_FADE_MS: u64 = 1_000;
pub const MAX_VISIBLE_TOASTS: usize = 3;

/// An sRGB colour, as bytes. Deliberately not `egui::Color32`: this module must stay compilable on
/// a host with no windowing stack at all.
pub type Rgb = [u8; 3];

/// Dark-theme palette. One place, so a row's colour and a pill's colour cannot drift apart.
pub mod palette {
    use super::Rgb;

    pub const TEXT: Rgb = [0xd4, 0xd0, 0xc8];
    pub const MUTED: Rgb = [0x8a, 0x86, 0x80];
    pub const OK: Rgb = [0x6f, 0xc2, 0x76];
    pub const WARN: Rgb = [0xdd, 0xb5, 0x4f];
    pub const BAD: Rgb = [0xe0, 0x6c, 0x6c];
    /// Checks sent by this slot: the accent the window is otherwise built around.
    pub const ACCENT: Rgb = [0x7a, 0xb8, 0xe8];
    /// Items granted into the game.
    pub const ITEM: Rgb = [0xc9, 0xa6, 0xe8];
    /// Hints. Distinct from `ITEM` on purpose -- a hint names an item and would otherwise be
    /// indistinguishable from actually receiving one.
    pub const HINT: Rgb = [0xb0, 0x8c, 0xd8];
    pub const VICTORY: Rgb = [0xd8, 0xb4, 0x63];
    /// Archipelago's own item colours, as the text client and Universal Tracker draw them:
    /// plum for progression, slate blue for useful, salmon for traps, cyan for filler. A row
    /// that names an item takes the item's colour so the window agrees with the tracker.
    pub const PROGRESSION: Rgb = [0xaf, 0x99, 0xef];
    pub const USEFUL: Rgb = [0x6d, 0x8b, 0xe8];
    pub const TRAP: Rgb = [0xfa, 0x80, 0x72];
    pub const FILLER: Rgb = [0x00, 0xee, 0xee];
    pub const BACKGROUND: Rgb = [0x14, 0x14, 0x16];
    pub const PANEL: Rgb = [0x1c, 0x1c, 0x20];
}

/// How one activity row is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivityStyle {
    pub color: Rgb,
    /// Leading glyph, or `""` for rows that get none. ASCII only: the standalone window inherits
    /// whatever font the host has, and the in-game toast path next door is ASCII-only for the same
    /// reason. A missing glyph is a worse bug than a plain row.
    pub glyph: &'static str,
    /// Command echoes and their results are machine text; a proportional font makes them harder to
    /// compare against the log.
    pub monospace: bool,
}

/// The whole styling table for the feed. Exhaustive over [`ActivityKind`] so a new kind cannot be
/// added without deciding how a player sees it.
pub fn activity_style(kind: &ActivityKind) -> ActivityStyle {
    let style = |color, glyph, monospace| ActivityStyle {
        color,
        glyph,
        monospace,
    };
    match kind {
        ActivityKind::LocationCheck => style(palette::ACCENT, "+", false),
        ActivityKind::ReceivedItem => style(palette::ITEM, "->", false),
        ActivityKind::StorageDelivery => style(palette::ITEM, "->", false),
        ActivityKind::ParkedDelivery => style(palette::WARN, "||", false),
        ActivityKind::Error => style(palette::BAD, "!", false),
        ActivityKind::Hint => style(palette::HINT, "?", false),
        ActivityKind::Command => style(palette::MUTED, ">", true),
        ActivityKind::CommandResult => style(palette::MUTED, "", true),
        ActivityKind::Message => style(palette::TEXT, "", false),
    }
}

/// The colour Archipelago gives an item class everywhere else.
pub fn item_class_color(class: ItemClass) -> Rgb {
    match class {
        ItemClass::Progression => palette::PROGRESSION,
        ItemClass::Useful => palette::USEFUL,
        ItemClass::Trap => palette::TRAP,
        ItemClass::Filler => palette::FILLER,
    }
}

/// The style for one row: the kind's style, recoloured by the item's Archipelago class when the
/// row names an item whose class the producer knew. Rows about anything other than an item, and
/// parked deliveries (whose colour is the warning, not the item), keep the kind colour.
pub fn event_style(event: &ActivityEvent) -> ActivityStyle {
    let mut style = activity_style(&event.kind);
    if let Some(class) = event.item_class
        && matches!(
            event.kind,
            ActivityKind::LocationCheck
                | ActivityKind::ReceivedItem
                | ActivityKind::StorageDelivery
        )
    {
        style.color = item_class_color(class);
    }
    style
}

/// Recent pickup events for compact mode. The full window already has the feed,
/// so it deliberately does not render this second presentation.
pub fn toast_events(snapshot: &ClientSnapshot, now_ms: u64) -> Vec<&client_ui::ActivityEvent> {
    snapshot
        .activity
        .iter()
        .rev()
        .filter(|event| {
            matches!(
                event.kind,
                ActivityKind::LocationCheck
                    | ActivityKind::ReceivedItem
                    | ActivityKind::StorageDelivery
            ) && event.timestamp_ms > 0
                && now_ms.saturating_sub(event.timestamp_ms) < TOAST_LIFETIME_MS
        })
        .take(MAX_VISIBLE_TOASTS)
        .collect()
}

pub fn toast_alpha(timestamp_ms: u64, now_ms: u64) -> f32 {
    let age = now_ms.saturating_sub(timestamp_ms);
    if age >= TOAST_LIFETIME_MS {
        return 0.0;
    }
    let fade_start = TOAST_LIFETIME_MS - TOAST_FADE_MS;
    if age <= fade_start {
        1.0
    } else {
        (TOAST_LIFETIME_MS - age) as f32 / TOAST_FADE_MS as f32
    }
}

/// A header status pill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pill {
    pub label: &'static str,
    pub value: &'static str,
    pub tone: Tone,
}

/// Pill colouring. `Muted` exists for exactly one situation: the worker has stopped reporting, so
/// every pill on screen describes the past and must stop claiming to describe the present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Bad,
    Muted,
}

impl Tone {
    pub fn color(self) -> Rgb {
        match self {
            Tone::Ok => palette::OK,
            Tone::Warn => palette::WARN,
            Tone::Bad => palette::BAD,
            Tone::Muted => palette::MUTED,
        }
    }
}

/// The three header pills, in fixed order: Game, AP, Delivery.
pub fn pills(snapshot: &ClientSnapshot) -> [Pill; 3] {
    let mute = |pill: Pill| {
        if snapshot.stale {
            Pill {
                tone: Tone::Muted,
                ..pill
            }
        } else {
            pill
        }
    };
    let (game_value, game_tone) = match snapshot.process {
        ProcessState::Attached => ("Attached", Tone::Ok),
        ProcessState::Attaching => ("Attaching", Tone::Warn),
        ProcessState::Waiting => ("Waiting", Tone::Warn),
        ProcessState::Lost => ("Lost", Tone::Bad),
    };
    let (ap_value, ap_tone) = match snapshot.ap {
        ApState::Authenticated => ("Connected", Tone::Ok),
        ApState::Connecting => ("Connecting", Tone::Warn),
        ApState::Reconnecting => ("Reconnecting", Tone::Warn),
        ApState::Disconnected => ("Offline", Tone::Bad),
    };
    let (delivery_value, delivery_tone) = match snapshot.delivery {
        DeliveryState::Ready => ("Ready", Tone::Ok),
        DeliveryState::WaitingForGameplay => ("Waiting", Tone::Warn),
        DeliveryState::CommandPending => ("Working", Tone::Warn),
        DeliveryState::NotArmed => ("Not armed", Tone::Bad),
        DeliveryState::Blocked => ("Stalled", Tone::Bad),
        DeliveryState::Parked => ("Parked", Tone::Warn),
    };
    [
        mute(Pill {
            label: "Game",
            value: game_value,
            tone: game_tone,
        }),
        mute(Pill {
            label: "AP",
            value: ap_value,
            tone: ap_tone,
        }),
        mute(Pill {
            label: "Delivery",
            value: delivery_value,
            tone: delivery_tone,
        }),
    ]
}

/// Slot name and server, as two separately styled pieces. `None` for the slot means the worker has
/// not published an identity yet.
pub fn identity(snapshot: &ClientSnapshot) -> (String, Option<String>) {
    match (&snapshot.slot, &snapshot.server) {
        (Some(slot), Some(server)) => (slot.clone(), Some(format!("@ {server}"))),
        (Some(slot), None) => (slot.clone(), None),
        (None, _) => ("Starting...".to_owned(), None),
    }
}

/// `checked / total` plus the 0.0..=1.0 bar fraction, or `None` when the seed contract has not
/// been parsed yet. A bar drawn against a zero total is a bar that always reads empty.
pub fn checks_progress(snapshot: &ClientSnapshot) -> Option<(f32, String)> {
    let locations = snapshot.locations.as_ref()?;
    if locations.total == 0 {
        return None;
    }
    let fraction = (f64::from(locations.checked) / f64::from(locations.total)).clamp(0.0, 1.0);
    Some((
        fraction as f32,
        format!("{} / {}", locations.checked, locations.total),
    ))
}

/// The items line: `N delivered - N queued`, with parked appended only when there are any.
///
/// Storage routing is absent by design. `storage_routed: None` means "ordinary ledger
/// acknowledgement cannot prove a native destination", and the old shell rendered that as
/// `unknown storage`, which players read as breakage. The count appears only when it is a fact;
/// the caveat belongs in [`STORAGE_TOOLTIP`].
pub fn items_line(ledger: &LedgerTotals) -> String {
    let mut parts = vec![
        format!("{} delivered", ledger.delivered),
        format!("{} queued", ledger.queued),
    ];
    if let Some(routed) = ledger.storage_routed {
        parts.push(format!("{routed} to storage"));
    }
    if ledger.parked > 0 {
        parts.push(format!("{} parked", ledger.parked));
    }
    parts.join(" \u{b7} ")
}

/// Shown on hover over the items line whenever storage routing is unproven.
pub const STORAGE_TOOLTIP: &str = "storage routing unverified this session";

/// The goal line, and whether the GO MODE badge is drawn beside it.
///
/// `go_mode: None` and `go_mode: Some(false)` both render no badge: the word "unknown" on a goal
/// line is read as a fault, and the absence of a badge already says everything a player needs.
pub fn goal_line(snapshot: &ClientSnapshot) -> Option<(String, bool)> {
    let goal = snapshot.goal.as_ref()?;
    Some((format!("Goal: {goal}"), snapshot.go_mode == Some(true)))
}

fn optional_count(value: Option<u32>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

pub fn victory_lines(summary: &client_ui::VictorySummary) -> [String; 4] {
    let elapsed = summary.elapsed_seconds.map_or_else(
        || "unknown".to_owned(),
        |seconds| {
            format!(
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                (seconds % 3600) / 60,
                seconds % 60
            )
        },
    );
    let checks = match (summary.checks_completed, summary.checks_total) {
        (Some(done), Some(total)) => format!("{done}/{total}"),
        _ => "unknown".to_owned(),
    };
    [
        format!("VICTORY - {}", summary.goal),
        format!("Time {elapsed}  |  Checks {checks}"),
        format!(
            "Items received {}  |  sent {}",
            optional_count(summary.received_items),
            optional_count(summary.sent_items)
        ),
        format!(
            "Deaths {}  |  DeathLinks {}",
            optional_count(summary.deaths),
            optional_count(summary.death_links)
        ),
    ]
}

/// `HH:MM:SS` in the viewer's local time, from a Unix-epoch millisecond stamp.
///
/// Hand-rolled rather than pulled from a date crate because this is the only date arithmetic in
/// the window and the whole of it is "seconds within a day". `offset_seconds` is the local UTC
/// offset supplied by the caller; an unstamped event (`0`) renders as blanks so the column stays
/// aligned instead of claiming a time.
pub fn clock_label(timestamp_ms: u64, offset_seconds: i64) -> String {
    if timestamp_ms == 0 {
        return "--:--:--".to_owned();
    }
    let seconds = (timestamp_ms / 1000) as i64 + offset_seconds;
    let day_seconds = seconds.rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60
    )
}

/// Session-local command history for the Up/Down arrows.
///
/// Bounded and duplicate-collapsing: a player retrying `status` six times wants one entry back,
/// not six. Never persisted -- a command history that outlives the process would carry rescue
/// commands into a session where they mean something different.
#[derive(Clone, Debug, Default)]
pub struct CommandHistory {
    entries: Vec<String>,
    /// `None` means "editing a fresh line"; `Some(i)` indexes `entries` from the newest end.
    cursor: Option<usize>,
}

impl CommandHistory {
    pub const CAPACITY: usize = 50;

    pub fn record(&mut self, command: &str) {
        self.cursor = None;
        if command.is_empty() {
            return;
        }
        if self.entries.last().is_some_and(|last| last == command) {
            return;
        }
        self.entries.push(command.to_owned());
        if self.entries.len() > Self::CAPACITY {
            self.entries.remove(0);
        }
    }

    /// Steps one entry towards the past (`older`, not `previous`: an `Option`-returning `next` on a
    /// non-iterator is a clippy lint and a genuine misreading). Returns the line to place in the input, or `None` when
    /// there is no history at all.
    pub fn older(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => 0,
            Some(index) => (index + 1).min(self.entries.len() - 1),
        };
        self.cursor = Some(next);
        self.entries.get(self.entries.len() - 1 - next).cloned()
    }

    /// Steps one entry towards the present. Returns `Some("")` when it walks off the newest entry,
    /// which restores the empty input line rather than sticking on the last command.
    pub fn newer(&mut self) -> Option<String> {
        match self.cursor {
            None | Some(0) => {
                self.cursor = None;
                Some(String::new())
            }
            Some(index) => {
                self.cursor = Some(index - 1);
                self.entries.get(self.entries.len() - index).cloned()
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_ui::{ActivityEvent, LocationTotals, Severity, SnapshotReducer, delivery_headline};

    #[test]
    fn rows_that_name_an_item_take_archipelagos_class_colour() {
        let check = ActivityEvent::now(1, ActivityKind::LocationCheck, "Fire Paper x2 -> oz")
            .with_item_class(Some(ItemClass::Progression));
        assert_eq!(event_style(&check).color, palette::PROGRESSION);
        assert_eq!(event_style(&check).glyph, "+");
        let useful = ActivityEvent::now(2, ActivityKind::ReceivedItem, "Oedon Tomb Key")
            .with_item_class(Some(ItemClass::Useful));
        assert_eq!(event_style(&useful).color, palette::USEFUL);
        let filler = ActivityEvent::now(3, ActivityKind::StorageDelivery, "Blood Vial")
            .with_item_class(Some(ItemClass::Filler));
        assert_eq!(event_style(&filler).color, palette::FILLER);
        let trap = ActivityEvent::now(4, ActivityKind::ReceivedItem, "Frenzy")
            .with_item_class(Some(ItemClass::Trap));
        assert_eq!(event_style(&trap).color, palette::TRAP);
        // No class known: the kind colour, as before.
        let unknown = ActivityEvent::now(5, ActivityKind::ReceivedItem, "Saw Cleaver");
        assert_eq!(event_style(&unknown).color, palette::ITEM);
        // A parked delivery stays a warning whatever the item is.
        let parked = ActivityEvent::now(6, ActivityKind::ParkedDelivery, "Parked Butcher Gloves")
            .with_item_class(Some(ItemClass::Progression));
        assert_eq!(event_style(&parked).color, palette::WARN);
        assert_eq!(
            ItemClass::from_flags(true, true, true),
            ItemClass::Progression
        );
        assert_eq!(ItemClass::from_flags(false, true, true), ItemClass::Useful);
        assert_eq!(ItemClass::from_flags(false, false, true), ItemClass::Trap);
        assert_eq!(
            ItemClass::from_flags(false, false, false),
            ItemClass::Filler
        );
    }

    #[test]
    fn toast_deck_is_recent_bounded_and_newest_first() {
        let mut snapshot = ClientSnapshot::default();
        for (sequence, kind, timestamp_ms) in [
            (1, ActivityKind::LocationCheck, 1_000),
            (2, ActivityKind::Message, 2_000),
            (3, ActivityKind::ReceivedItem, 3_000),
            (4, ActivityKind::StorageDelivery, 4_000),
            (5, ActivityKind::LocationCheck, 5_000),
        ] {
            snapshot.push_activity(
                ActivityEvent {
                    sequence,
                    kind,
                    text: sequence.to_string(),
                    timestamp_ms,
                    item_class: None,
                },
                10,
            );
        }
        assert_eq!(
            toast_events(&snapshot, 5_500)
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [5, 4, 3]
        );
        assert!(toast_events(&snapshot, 9_001).is_empty());
    }

    #[test]
    fn toast_is_solid_then_fades_during_its_last_second() {
        assert_eq!(toast_alpha(1_000, 4_000), 1.0);
        assert!((toast_alpha(1_000, 4_500) - 0.5).abs() < f32::EPSILON);
        assert_eq!(toast_alpha(1_000, 5_000), 0.0);
    }

    #[test]
    fn every_activity_kind_has_a_distinct_enough_style() {
        use ActivityKind::*;

        assert_eq!(
            activity_style(&LocationCheck),
            ActivityStyle {
                color: palette::ACCENT,
                glyph: "+",
                monospace: false
            }
        );
        assert_eq!(activity_style(&ReceivedItem).color, palette::ITEM);
        assert_eq!(activity_style(&ReceivedItem).glyph, "->");
        assert_eq!(activity_style(&Error).color, palette::BAD);
        assert_eq!(activity_style(&Hint).color, palette::HINT);
        assert_eq!(activity_style(&ParkedDelivery).color, palette::WARN);
        assert_eq!(activity_style(&Message).color, palette::TEXT);

        // Command traffic is the only monospaced traffic, and the only traffic that is muted.
        for kind in [Command, CommandResult] {
            assert!(activity_style(&kind).monospace, "{kind:?}");
            assert_eq!(activity_style(&kind).color, palette::MUTED);
        }
        for kind in [
            Message,
            LocationCheck,
            ReceivedItem,
            StorageDelivery,
            ParkedDelivery,
            Error,
            Hint,
        ] {
            assert!(!activity_style(&kind).monospace, "{kind:?}");
        }

        // A failure must never be styled as ordinary chat: this is the pairing the old one-blob
        // shell could not make, and the reason a dead bridge looked like a healthy one.
        assert_ne!(activity_style(&Error).color, activity_style(&Message).color);
        assert_ne!(
            activity_style(&Hint).color,
            activity_style(&ReceivedItem).color
        );

        // Every style is ASCII, because the window inherits the host's font.
        for kind in [
            Message,
            Command,
            CommandResult,
            LocationCheck,
            ReceivedItem,
            StorageDelivery,
            ParkedDelivery,
            Error,
            Hint,
        ] {
            assert!(activity_style(&kind).glyph.is_ascii(), "{kind:?}");
        }
    }

    #[test]
    fn pills_read_as_words_not_as_enum_names() {
        let snapshot = ClientSnapshot {
            process: ProcessState::Attached,
            ap: ApState::Authenticated,
            delivery: DeliveryState::WaitingForGameplay,
            ..Default::default()
        };
        let pills = pills(&snapshot);
        assert_eq!(pills[0].value, "Attached");
        assert_eq!(pills[0].tone, Tone::Ok);
        assert_eq!(pills[1].value, "Connected");
        assert_eq!(pills[2].value, "Waiting");
        assert_eq!(pills[2].tone, Tone::Warn);
        for pill in &pills {
            assert!(!pill.value.contains("ForGameplay"), "{pill:?}");
        }
    }

    #[test]
    fn a_stale_snapshot_greys_every_pill_including_the_healthy_ones() {
        let snapshot = ClientSnapshot {
            process: ProcessState::Attached,
            ap: ApState::Authenticated,
            delivery: DeliveryState::Ready,
            stale: true,
            ..Default::default()
        };
        for pill in pills(&snapshot) {
            assert_eq!(pill.tone, Tone::Muted, "{pill:?}");
        }
    }

    #[test]
    fn the_headline_and_the_delivery_pill_never_disagree() {
        for delivery in [
            DeliveryState::NotArmed,
            DeliveryState::WaitingForGameplay,
            DeliveryState::Ready,
            DeliveryState::CommandPending,
            DeliveryState::Blocked,
            DeliveryState::Parked,
        ] {
            let snapshot = ClientSnapshot {
                delivery: delivery.clone(),
                ..Default::default()
            };
            let expected = match delivery_headline(&snapshot).0 {
                Severity::Ok => Tone::Ok,
                Severity::Warn => Tone::Warn,
                Severity::Bad => Tone::Bad,
            };
            assert_eq!(pills(&snapshot)[2].tone, expected, "{delivery:?}");
        }
    }

    #[test]
    fn the_items_line_hides_what_it_cannot_prove() {
        assert_eq!(
            items_line(&LedgerTotals {
                delivered: 12,
                queued: 1,
                storage_routed: None,
                parked: 0,
            }),
            "12 delivered \u{b7} 1 queued"
        );
        assert_eq!(
            items_line(&LedgerTotals {
                delivered: 12,
                queued: 0,
                storage_routed: Some(3),
                parked: 2,
            }),
            "12 delivered \u{b7} 0 queued \u{b7} 3 to storage \u{b7} 2 parked"
        );
        // The word players read as breakage never appears.
        assert!(!items_line(&LedgerTotals::default()).contains("unknown"));
    }

    #[test]
    fn a_goal_never_says_unknown() {
        assert_eq!(goal_line(&ClientSnapshot::default()), None);
        let with_goal = |go_mode| {
            goal_line(&ClientSnapshot {
                goal: Some("Moon Presence".into()),
                go_mode,
                ..Default::default()
            })
            .unwrap()
        };
        assert_eq!(with_goal(Some(true)), ("Goal: Moon Presence".into(), true));
        assert_eq!(
            with_goal(Some(false)),
            ("Goal: Moon Presence".into(), false)
        );
        assert_eq!(with_goal(None), ("Goal: Moon Presence".into(), false));
    }

    #[test]
    fn every_supported_goal_and_missing_counter_renders_truthfully() {
        for goal in ["Submit to Gehrman", "Refuse Gehrman", "Moon Presence"] {
            let lines = victory_lines(&client_ui::VictorySummary {
                goal: goal.into(),
                completed_at_ms: 1,
                elapsed_seconds: None,
                checks_completed: Some(10),
                checks_total: None,
                received_items: None,
                sent_items: Some(10),
                deaths: None,
                death_links: None,
            });
            assert!(lines[0].contains(goal));
            assert!(lines[1].contains("Time unknown"));
            assert!(lines[1].contains("Checks unknown"));
            assert!(lines[2].contains("received unknown"));
            assert!(lines[3].contains("Deaths unknown"));
        }
    }

    #[test]
    fn checks_progress_is_absent_rather_than_empty_before_the_contract_loads() {
        assert_eq!(checks_progress(&ClientSnapshot::default()), None);
        assert_eq!(
            checks_progress(&ClientSnapshot {
                locations: Some(LocationTotals {
                    checked: 0,
                    total: 0
                }),
                ..Default::default()
            }),
            None
        );
        let (fraction, label) = checks_progress(&ClientSnapshot {
            locations: Some(LocationTotals {
                checked: 42,
                total: 168,
            }),
            ..Default::default()
        })
        .unwrap();
        assert!((fraction - 0.25).abs() < f32::EPSILON);
        assert_eq!(label, "42 / 168");
    }

    #[test]
    fn identity_degrades_to_a_skeleton_rather_than_a_panic() {
        assert_eq!(
            identity(&ClientSnapshot::default()),
            ("Starting...".to_owned(), None)
        );
        assert_eq!(
            identity(&ClientSnapshot {
                slot: Some("hunter".into()),
                server: Some("archipelago.gg:12345".into()),
                ..Default::default()
            }),
            (
                "hunter".to_owned(),
                Some("@ archipelago.gg:12345".to_owned())
            )
        );
    }

    #[test]
    fn the_clock_column_stays_aligned_even_without_a_stamp() {
        assert_eq!(clock_label(0, 0), "--:--:--");
        // 2026-08-31T01:02:03Z
        assert_eq!(clock_label(1_787_101_323_000, 0), "01:02:03");
        // ... read an hour later, and across the midnight wrap in the other direction.
        assert_eq!(clock_label(1_787_101_323_000, 3600), "02:02:03");
        assert_eq!(clock_label(1_787_101_323_000, -2 * 3600), "23:02:03");
    }

    #[test]
    fn command_history_walks_both_ways_and_returns_to_a_blank_line() {
        let mut history = CommandHistory::default();
        for command in ["status", "status", "blocked", "export"] {
            history.record(command);
        }
        assert_eq!(history.len(), 3, "consecutive repeats collapse");

        assert_eq!(history.older().as_deref(), Some("export"));
        assert_eq!(history.older().as_deref(), Some("blocked"));
        assert_eq!(history.older().as_deref(), Some("status"));
        assert_eq!(
            history.older().as_deref(),
            Some("status"),
            "clamps at the oldest"
        );
        assert_eq!(history.newer().as_deref(), Some("blocked"));
        assert_eq!(history.newer().as_deref(), Some("export"));
        assert_eq!(
            history.newer().as_deref(),
            Some(""),
            "walks off into a fresh line"
        );
        assert_eq!(history.newer().as_deref(), Some(""));
    }

    #[test]
    fn an_empty_history_never_hijacks_the_arrow_keys() {
        let mut history = CommandHistory::default();
        assert!(history.is_empty());
        assert_eq!(history.older(), None);
    }

    #[test]
    fn history_is_bounded_at_fifty() {
        let mut history = CommandHistory::default();
        for index in 0..80 {
            history.record(&format!("flag {index}"));
        }
        assert_eq!(history.len(), CommandHistory::CAPACITY);
        assert_eq!(history.older().as_deref(), Some("flag 79"));
    }

    #[test]
    fn a_real_reducer_feed_styles_end_to_end() {
        // Guards the seam rather than the table: an event that came through the reducer carries a
        // kind this module can style and a stamp this module can print.
        let mut reducer = SnapshotReducer::default();
        reducer.activity(ActivityKind::ReceivedItem, "Saw Cleaver from Oz");
        reducer.activity(ActivityKind::Error, "Archipelago error: connection reset");
        let snapshot = reducer.reduce(Default::default());
        let events: Vec<&ActivityEvent> = snapshot.activity.iter().collect();
        assert_eq!(activity_style(&events[0].kind).glyph, "->");
        assert_eq!(activity_style(&events[1].kind).color, palette::BAD);
        assert_ne!(clock_label(events[1].timestamp_ms, 0), "--:--:--");
    }
}
