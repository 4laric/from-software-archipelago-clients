//! `reconcile` — the single, host-testable state RECONCILER that subsumes the client's scattered
//! grant/snapshot idempotency hacks.
//!
//! # Why this exists
//!
//! Today every client feature (start-item grants, key-item/great-rune goods, region-open flags,
//! map-reveal flags, the flag-poll baseline, consumable grants) hand-rolls its OWN idempotency,
//! driven off DISCRETE EVENTS — `connect`, save `load`, each `ItemReceived`. That is one root cause
//! behind a whole family of pinned bugs:
//!
//!   * flask double-grant on tutorial-death reload  (er-flask-double-grant-reconnect)
//!   * Torch clobbered by the load-screen grant       (gf-start-item-clobber)
//!   * great-rune double-GRANT on reconnect           (gf-great-rune-double-grant)
//!   * map-piece ITEMS handed out on connect          (er-map-pieces-granted-on-connect)
//!   * reconnect re-snapshot eats earned checks        (gf-flagpoll-newsave-default-flags)
//!   * bundle-lock graces never self-heal after loss   (er-bundle-lock-grace-reconcile-gap)
//!   * reconnect-to-a-new-seed panic (stale tables)    (er-reconnect-newseed-panic)
//!
//! Each of those is an event-ordering / re-fire / lost-state bug. The fix is structural: stop
//! mutating on events. Instead, on a cadence:
//!
//!   1. compute the DESIRED state from server-authoritative inputs (seed, received items, slot_data),
//!   2. read the OBSERVED live state,
//!   3. apply the DIFF, idempotently,
//!   4. all of it gated on "world loaded and stable".
//!
//! Events stop being mutators; they only mark the reconciler DIRTY. Because the desired state is a
//! pure function of the *set* of received items (not their order) and the loop drains to a FIXPOINT,
//! event order / duplication / interleaved load screens can no longer change the result. That
//! invariant is proven in [`crate::reconciler_replay`].
//!
//! # Structure
//!
//! This module is PURE (no `eldenring`/`windows`/socket deps) except for the [`GameIo`] trait — the
//! seam the Windows client implements against the live `fromsoftware-rs` singletons (see
//! `eldenring-archipelago/src/reconcile_io.rs`). The host-side [`MockGame`] implements the same
//! trait so tests drive the ACTUAL reconcile loop, not a stub.

use std::collections::{BTreeMap, BTreeSet};

/// A live-game event flag id (`CSEventFlagMan`).
pub type FlagId = u32;
/// An Elden Ring goods / FullID (signed; the high `0x4000_0000` bit is legal, so `i32`).
pub type GoodsId = i32;
/// An AP `received_items` stream index (monotonic, server-authoritative).
pub type ItemIndex = i64;

// ---------------------------------------------------------------------------------------------
// Inputs — server-authoritative
// ---------------------------------------------------------------------------------------------

/// Identifies WHICH save the reconciler state (esp. the ledger watermark) belongs to. A consumable
/// ledger is meaningless without knowing which character consumed it, so the watermark is persisted
/// per `SaveIdentity` (character slot). Distinct saves never share a watermark.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SaveIdentity(pub String);

/// What one received AP item MEANS to the client. Precomputed by the client from slot_data (which
/// knows the ER FullID / flag mapping of each AP item id) so this module needs no item database.
///
/// One progressive-item tier: the unique goods to grant and the observable flags to set when that
/// tier lands. Mirrors [`crate::progressive::ProgTier`] but with the goods pre-packed to their grant
/// FullIDs by the client mapper (`| GOODS_FULLID`), so this module stays item-database-free.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgTier {
    pub goods: Vec<GoodsId>,
    /// Observable rung flags: set-only + self-healing for OWNED and CONSUMED tiers alike (flags
    /// are observable state, not inventory).
    pub flags: Vec<FlagId>,
    /// `true` = this rung's goods are CONSUMED by the player (spent at a grace, e.g. the
    /// flask-upgrade ladder's Golden Seeds / Sacred Tears): granted exactly ONCE via the ledger,
    /// keyed by the copy's stream index — exactly like overflow. Presence-diffing a spendable
    /// good re-grants it every time it leaves the inventory (upgrade -> re-grant -> upgrade ->
    /// ... until flask potency runs past its cap and the game CTDs — the 2026-07-12 live crash).
    /// `false` (the default) = OWNED (stone bell bearings): a `unique_goods` entry that
    /// self-heals when lost.
    pub consumed: bool,
}

/// The variants are the whole point of the design: a map piece is a [`MapReveal`](Self::MapReveal)
/// carrying ONLY flags, so "grant a map-piece good on connect" is structurally *unrepresentable*.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemSemantics {
    /// Observable region-open / grace-bundle flags to SET (e.g. a `<Region> Lock`). These flags are
    /// OWNED by the reconciler (it may seal them too), so they land in the clear-allowlist.
    RegionFlags(Vec<FlagId>),
    /// A map-piece: sets map-REVEAL FLAGS ONLY (fragment flags, and 82001 for the underground view).
    /// Never a good — this is what makes map-pieces-on-connect impossible (er-map-pieces-granted).
    MapReveal(Vec<FlagId>),
    /// A unique KEY ITEM: an observable good plus its vanilla obtained-flag(s) (`4000xx`) as
    /// companions, set atomically with the grant.
    KeyItem {
        goods: GoodsId,
        obtained_flags: Vec<FlagId>,
    },
    /// A GREAT RUNE: an observable good plus its `restored` companion flag. The restored flag is
    /// ALSO an observable flag that self-heals independently (gf-great-rune-double-grant fix:
    /// grant the good ONCE, never re-emit while it is present).
    GreatRune {
        goods: GoodsId,
        restored_flag: FlagId,
    },
    /// A CONSUMABLE (flask / rune / smithing stone): non-observable (the player spends it), so it is
    /// LEDGERED and applied exactly once by stream index. Count-diffing is wrong here.
    ///
    /// `echo_skip` marks a NATIVE-SOLD shop echo (mirrors [`crate::receive::RecvItem::echo_skip`]):
    /// the rewritten shop row already delivered this exact item at purchase time, so the AP echo must
    /// NOT be granted again. It is still ledgered — with `apply=false` — so the per-save watermark
    /// advances PAST its index (a reload never reconsiders it). `false` = an ordinary consumable that
    /// grants once (two Golden Runes at two indices are two grants).
    Consumable {
        full_id: GoodsId,
        qty: i32,
        echo_skip: bool,
    },
    /// A PROGRESSIVE item: the Nth received copy of this NAME lands tier N (that tier's unique
    /// goods plus its observable flags); every copy past the last tier yields ONE overflow consumable
    /// (`overflow_full_id`, e.g. a Lord's Rune). The desired state depends only on the COUNT of
    /// copies received (order-independent); the overflow grants are LEDGERED per stream index so a
    /// reconnect replays none of them. All copies of one name carry the same `tiers` table.
    Progressive {
        tiers: Vec<ProgTier>,
        overflow_full_id: GoodsId,
    },
    /// A pure goal / progress flag.
    GoalFlag(FlagId),
    /// No client effect (unmapped AP id): contributes nothing to desired state.
    Inert,
}

/// One received AP item, mirrored out of `archipelago_rs` with its client semantics attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedItem {
    pub index: ItemIndex,
    pub name: String,
    pub semantics: ItemSemantics,
}

/// One slot-data START ITEM (Torrent, flasks). Non-observable, granted ONCE per save.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartItem {
    pub full_id: GoodsId,
    pub qty: i32,
}

/// The reserved NEGATIVE synthetic-index band the slot-data start items are ledgered into. Real AP
/// received-stream indices are always `>= 0`, so placing start items below zero lets the SINGLE
/// per-save watermark cover both classes: a fresh save's watermark starts at the band floor (all
/// start items owed), then advances into the `>= 0` real stream; a reload leaves the negative indices
/// behind the frontier so they never re-grant. Start item `i` takes `START_ITEM_INDEX_BASE + i`.
pub const START_ITEM_INDEX_BASE: ItemIndex = -1_000_000;

/// The slot-data-derived, per-seed configuration the desired-state builder needs beyond the item
/// stream. Kept tiny on purpose; the client fills it from parsed slot_data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotData {
    /// Region open-flags that start SEALED: desired `false`, and OWNED so the reconciler may clear
    /// (re-seal) them. A received `RegionFlags` for the same flag overrides it to `true` (opened).
    pub seal_flags: Vec<FlagId>,
    /// Start-of-run GRACE flags (the Limgrave warp graces) granted from slot_data. Desired-SET
    /// observable flags that SELF-HEAL if a load clobbers them, but are NOT owned (never cleared).
    pub start_graces: Vec<FlagId>,
    /// Map-reveal flags set UNCONDITIONALLY at start (e.g. the 82001 underground view-unlock, which
    /// gates the underground map layer regardless of `reveal_all_maps`). Desired-SET, self-heal, not
    /// owned.
    pub always_map_flags: Vec<FlagId>,
    /// When `reveal_all_maps` is on, these world-map reveal flags become desired-SET (self-heal, not
    /// owned). Ignored when it is off. The client pre-resolves the base (+DLC) list.
    pub reveal_all_maps: bool,
    pub map_reveal_flags: Vec<FlagId>,
    /// Slot-data START ITEMS: ledgered ONCE per save at the [`START_ITEM_INDEX_BASE`] negative band.
    pub start_items: Vec<StartItem>,
    /// GOAL-SEND: when `goal_met` is true this flag becomes desired-SET. The Windows glue maps the
    /// flag write to `ClientStatus::Goal` (or a real completion flag). `None` / not-met => nothing.
    /// Report-side, so it is never owned (never cleared).
    pub goal_flag: Option<FlagId>,
    pub goal_met: bool,
}

/// Everything server-authoritative the reconciler derives desired state from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesiredInputs {
    pub seed: String,
    pub save: SaveIdentity,
    pub received: Vec<ReceivedItem>,
    pub slot_data: SlotData,
}

// ---------------------------------------------------------------------------------------------
// Desired state — three OBSERVABILITY classes
// ---------------------------------------------------------------------------------------------

/// A unique observable good plus the companion flags set atomically when it is granted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UniqueGood {
    pub companion_flags: Vec<FlagId>,
}

/// A single consumable grant: apply the `full_id` x `qty` exactly once, keyed by its stream `index`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgeredGrant {
    pub index: ItemIndex,
    pub full_id: GoodsId,
    pub qty: i32,
    /// `false` for a deduped shop-native echo: advance the watermark PAST this index WITHOUT granting
    /// (the vanilla shop already delivered it). `true` for a real grant.
    pub apply: bool,
}

/// The desired end-state, split by OBSERVABILITY so each class gets the right idempotency rule.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesiredState {
    /// OBSERVABLE flags: region open/seal, grace bundles, map-reveal (incl. 62060-64 + 82001),
    /// great-rune `restored`, key-item obtained `4000xx`, goal. `true` = must be set, `false` = must
    /// be clear (only cleared if OWNED — see [`owned_flags`](Self::owned_flags)).
    pub flags: BTreeMap<FlagId, bool>,
    /// OBSERVABLE unique goods (key items, great runes): granted iff absent from the live inventory.
    pub unique_goods: BTreeMap<GoodsId, UniqueGood>,
    /// NON-observable consumables: desired == "server indices `>= watermark`, each applied once".
    pub ledgered: Vec<LedgeredGrant>,
    /// The clear-allowlist: only flags in here may ever be [`Action::ClearFlag`]'d. A vanilla-owned
    /// flag whose desired value is `false` is LEFT ALONE (we never clear something we don't own).
    pub owned_flags: BTreeSet<FlagId>,
}

impl DesiredState {
    /// Fold the received-item stream + slot_data into the desired end-state. Pure and
    /// ORDER-INDEPENDENT for the flag/unique classes (they are keyed sets); the ledger keeps the
    /// per-item stream index so a consume-and-reconnect can't re-grant.
    pub fn build(inputs: &DesiredInputs) -> DesiredState {
        let mut d = DesiredState::default();

        // 1) Regions default to SEALED (desired=false) and are OWNED (the reconciler may re-seal).
        for &f in &inputs.slot_data.seal_flags {
            d.flags.entry(f).or_insert(false);
            d.owned_flags.insert(f);
        }

        // 1b) Slot-data BULK grants (start graces, unconditional + reveal_all_maps map flags, the
        //     met goal flag): first-class desired-SET observable flags that self-heal but are NOT
        //     owned (never cleared). These previously rode the scattered startgrants / goal handlers;
        //     folding them here makes the reconciler the single source of the whole desired state.
        for &f in &inputs.slot_data.start_graces {
            d.flags.insert(f, true);
        }
        for &f in &inputs.slot_data.always_map_flags {
            d.flags.insert(f, true);
        }
        if inputs.slot_data.reveal_all_maps {
            for &f in &inputs.slot_data.map_reveal_flags {
                d.flags.insert(f, true);
            }
        }
        if inputs.slot_data.goal_met {
            if let Some(f) = inputs.slot_data.goal_flag {
                d.flags.insert(f, true);
            }
        }

        // 1c) Slot-data START ITEMS: LEDGERED once each at the reserved negative band, granted exactly
        //     once per save via the watermark. (A depletion-SAFE grant: the ledger owes an index, not an
        //     inventory presence, so a flask/pot that empties never re-owes itself.)
        //
        //     History: an earlier fix presence-diffed GOODS-category start items into `unique_goods`
        //     (grant iff absent) to dodge a stale-watermark stranding bug. That OVER-GRANTED depletable
        //     goods — Crimson/Cerulean flasks (and pots) re-granted every time their charge/quantity hit
        //     0 via DRINKING or grace REALLOCATION (`has_good` reads false at 0), and it collapsed
        //     repeated-FullID stacks (10x Cracked Pot) to one copy. The real stranding fix lives in the
        //     watermark SEEDING (`Reconciler::seeded` distrusts a slot-keyed `reconcile.json` watermark
        //     that sits ahead of this save's `received_through`, re-owing the whole negative band on a
        //     fresh character), so presence-diff was never actually needed here — ledger-once is both
        //     correct AND depletion-safe. Reverted to the plain ledger below.
        for (i, si) in inputs.slot_data.start_items.iter().enumerate() {
            d.ledgered.push(LedgeredGrant {
                index: START_ITEM_INDEX_BASE + i as ItemIndex,
                full_id: si.full_id,
                qty: si.qty,
                apply: true,
            });
        }

        // 2) Fold each received item into its observability class.
        for it in &inputs.received {
            match &it.semantics {
                ItemSemantics::RegionFlags(fs) => {
                    for &f in fs {
                        d.flags.insert(f, true); // opening a region OVERRIDES its default seal
                        d.owned_flags.insert(f);
                    }
                }
                ItemSemantics::MapReveal(fs) => {
                    for &f in fs {
                        d.flags.insert(f, true);
                        d.owned_flags.insert(f); // reveal flags are ours (never vanilla-critical)
                    }
                    // NOTE: no unique_goods entry — a map piece can never become a granted good.
                }
                ItemSemantics::KeyItem {
                    goods,
                    obtained_flags,
                } => {
                    d.unique_goods.insert(
                        *goods,
                        UniqueGood {
                            companion_flags: obtained_flags.clone(),
                        },
                    );
                    for &f in obtained_flags {
                        d.flags.insert(f, true); // obtained flags self-heal; never cleared (not owned)
                    }
                }
                ItemSemantics::GreatRune {
                    goods,
                    restored_flag,
                } => {
                    d.unique_goods.insert(
                        *goods,
                        UniqueGood {
                            companion_flags: vec![*restored_flag],
                        },
                    );
                    d.flags.insert(*restored_flag, true);
                }
                ItemSemantics::Consumable {
                    full_id,
                    qty,
                    echo_skip,
                } => {
                    d.ledgered.push(LedgeredGrant {
                        index: it.index,
                        full_id: *full_id,
                        qty: *qty,
                        // A native-sold shop echo advances the watermark PAST its index but grants
                        // nothing (the vanilla shop already delivered it at purchase).
                        apply: !*echo_skip,
                    });
                }
                ItemSemantics::GoalFlag(f) => {
                    d.flags.insert(*f, true);
                }
                // Progressive is COUNT-based, so it can't be folded per-item here; it needs all of a
                // name's copies at once. Handled in the name-grouping post-pass below.
                ItemSemantics::Progressive { .. } => {}
                ItemSemantics::Inert => {}
            }
        }

        // 3) PROGRESSIVE: fold each NAME's received copies into landed tiers. Grouping by name and
        //    sorting by stream index makes this a pure function of the received multiset: tiers
        //    0..min(count, tiers.len()) contribute their goods + observable flags, and every
        //    copy past the last tier contributes ONE ledgered overflow consumable (keyed by its own
        //    index, so a reconnect re-grants none of them). An OWNED rung's goods self-heal via
        //    `unique_goods`; a CONSUMED rung's goods are spendable and land in the ledger instead
        //    (granted once by stream index, like overflow). Tier flags self-heal set-only; they
        //    are never OWNED (never cleared), like key-item obtained flags.
        let mut prog: BTreeMap<&str, (&Vec<ProgTier>, GoodsId, Vec<ItemIndex>)> = BTreeMap::new();
        for it in &inputs.received {
            if let ItemSemantics::Progressive {
                tiers,
                overflow_full_id,
            } = &it.semantics
            {
                let e =
                    prog.entry(it.name.as_str())
                        .or_insert((tiers, *overflow_full_id, Vec::new()));
                e.2.push(it.index);
            }
        }
        for (_name, (tiers, overflow, mut idxs)) in prog {
            idxs.sort_unstable();
            for (pos, idx) in idxs.iter().enumerate() {
                if let Some(t) = tiers.get(pos) {
                    if t.consumed {
                        // CONSUMED rung: the player SPENDS these goods, so a `unique_goods`
                        // presence-diff would resurrect them after every spend (the flask-CTD
                        // loop). Ledgered instead — granted exactly once, keyed by this copy's
                        // stream index, exactly like overflow below. NOTE: the ledger watermark
                        // protocol assumes at most one entry per index; the slot_data contract
                        // carries ONE goods id per consumed rung, which preserves that.
                        for &g in &t.goods {
                            d.ledgered.push(LedgeredGrant {
                                index: *idx,
                                full_id: g,
                                qty: 1,
                                apply: true,
                            });
                        }
                    } else {
                        for &g in &t.goods {
                            d.unique_goods.entry(g).or_default();
                        }
                    }
                    // Rung flags are observable state (not inventory): set-only + self-healing
                    // for BOTH owned and consumed rungs.
                    for &f in &t.flags {
                        d.flags.insert(f, true);
                    }
                } else {
                    d.ledgered.push(LedgeredGrant {
                        index: *idx,
                        full_id: overflow,
                        qty: 1,
                        apply: true,
                    });
                }
            }
        }

        d.ledgered.sort_by_key(|l| l.index);
        d
    }

    /// The FLOOR a fresh save's watermark starts at: `0` for a normal seed, or the negative
    /// start-item band floor when slot-data start items are present. Clamped with `.min(0)` so it is
    /// never ABOVE 0 (real received indices are `>= 0` and are owed via the `index >= watermark`
    /// filter regardless), while the negative start-item band still pulls it below zero so those
    /// grants are owed on a fresh save. Empty ledger => 0.
    pub fn ledger_floor(&self) -> ItemIndex {
        self.ledgered
            .iter()
            .map(|l| l.index)
            .min()
            .unwrap_or(0)
            .min(0)
    }
}

// ---------------------------------------------------------------------------------------------
// Observed state + the diff
// ---------------------------------------------------------------------------------------------

/// The live state as READ from the game this tick, plus the persisted ledger watermark. Only the
/// flags/goods actually referenced by the desired state are snapshotted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObservedState {
    pub flags: BTreeMap<FlagId, bool>,
    pub unique_goods: BTreeSet<GoodsId>,
    /// Contiguous "already applied" frontier for the consumable ledger (persisted per save). Grants
    /// with `index >= applied_watermark` are still owed.
    pub applied_watermark: ItemIndex,
}

/// One idempotent mutation the reconciler wants to perform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Set an observable flag true.
    SetFlag(FlagId),
    /// Clear an owned observable flag (re-seal). Only ever emitted for `owned_flags`.
    ClearFlag(FlagId),
    /// Grant a unique good and set its companion (obtained/restored) flags atomically.
    GrantUnique(GoodsId, Vec<FlagId>),
    /// Apply a ledgered consumable grant once, advancing the watermark past `index`.
    GrantLedgered {
        index: ItemIndex,
        full_id: GoodsId,
        qty: i32,
    },
    /// Advance the watermark PAST a deduped shop-native echo index WITHOUT granting anything (the
    /// vanilla shop already delivered the item). Costs no game call and no tick budget.
    SkipLedgered { index: ItemIndex },
}

/// The pure DIFF: the minimal, deterministically-ordered set of actions to move `observed` toward
/// `desired`. Flags first (BTree order), then unique goods (BTree order), then ledger (index order).
///
///   * a flag wanted-set but observed-clear -> [`Action::SetFlag`];
///   * a flag wanted-clear but observed-set -> [`Action::ClearFlag`] ONLY if it is owned (else left
///     alone — never clear a vanilla-owned flag);
///   * a unique good absent from inventory -> [`Action::GrantUnique`] (present -> nothing: no
///     double-grant);
///   * a ledger grant at/above the watermark -> [`Action::GrantLedgered`] (below -> already applied).
pub fn diff(desired: &DesiredState, observed: &ObservedState) -> Vec<Action> {
    let mut out = Vec::new();

    for (&f, &want) in &desired.flags {
        let have = observed.flags.get(&f).copied().unwrap_or(false);
        if want && !have {
            out.push(Action::SetFlag(f));
        } else if !want && have && desired.owned_flags.contains(&f) {
            out.push(Action::ClearFlag(f));
        }
    }

    for (&g, ug) in &desired.unique_goods {
        if !observed.unique_goods.contains(&g) {
            out.push(Action::GrantUnique(g, ug.companion_flags.clone()));
        }
    }

    // Ledger: index order, only the still-owed tail. (build() already sorted; filter is stable.)
    // A real grant emits `GrantLedgered`; a deduped shop-native echo (`apply=false`) emits
    // `SkipLedgered`, which advances the watermark past its index without touching the game.
    for l in desired
        .ledgered
        .iter()
        .filter(|l| l.index >= observed.applied_watermark)
    {
        if l.apply {
            out.push(Action::GrantLedgered {
                index: l.index,
                full_id: l.full_id,
                qty: l.qty,
            });
        } else {
            out.push(Action::SkipLedgered { index: l.index });
        }
    }

    out
}

// ---------------------------------------------------------------------------------------------
// Stability gate
// ---------------------------------------------------------------------------------------------

/// The world-loaded-and-stable predicate. Generalizes the Torch-fix gate
/// ([`crate::start_grant_replay::start_items_settled`]) to EVERY reconciler mutation AND snapshot:
/// nothing is read or written until the world is genuinely live, so a load-screen or bulk-inventory
/// clobber can never race a grant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldStability {
    pub in_game: bool,
    pub player_valid: bool,
    /// Continuous in-world dwell time this session, in ms.
    pub dwell_ms: u64,
    /// A real game-driven `AddItem` has fired (bulk load done) — the inventory is genuinely live.
    pub real_pickup_seen: bool,
    /// A MONOTONIC session clock in ms that — UNLIKE `dwell_ms` — never resets across a load screen.
    /// Feeds the grant PACING gate ([`TickBudget::min_grant_interval_ms`]) so a large received-item
    /// delta drains a few grants per interval instead of firing every `AddItemFunc` in one frame (the
    /// mass-grant CTD). Injected (never read from `std` in this pure crate), mirroring
    /// [`crate::region_lock`]'s `now_ms` convention. `0` in tests that don't exercise pacing.
    pub now_ms: u64,
}

impl WorldStability {
    /// Fallback dwell before we trust the inventory when no real pickup has fired.
    pub const SETTLE_MS: u64 = 8_000;

    /// Stable == in-game, player pointer valid, AND (a real pickup has fired OR we have dwelled long
    /// enough that the bulk inventory load must be done).
    pub fn stable(&self) -> bool {
        self.in_game
            && self.player_valid
            && (self.real_pickup_seen || self.dwell_ms >= Self::SETTLE_MS)
    }
}

// ---------------------------------------------------------------------------------------------
// The game seam
// ---------------------------------------------------------------------------------------------

/// Every live-game verb the reconciler needs. The Windows client implements this against the
/// `fromsoftware-rs` singletons (`reconcile_io.rs`); [`MockGame`] implements it in-memory for tests.
pub trait GameIo {
    /// The current stability reading (in-game, player-valid, dwell, real-pickup-seen).
    fn stability(&self) -> WorldStability;
    /// Read an event flag.
    fn get_flag(&self, flag: FlagId) -> bool;
    /// Write an event flag. Returns `false` when the flag holder isn't ready yet (retry next tick).
    fn set_flag(&mut self, flag: FlagId, on: bool) -> bool;
    /// Is this unique good present in the live inventory?
    fn has_good(&self, goods: GoodsId) -> bool;
    /// Grant a unique good and set its companion flags. Returns `false` if the inventory pointer
    /// isn't captured yet (retry next tick, nothing placed).
    fn grant_good(&mut self, goods: GoodsId, companion_flags: &[FlagId]) -> bool;
    /// Apply a ledgered consumable grant. Returns `false` if the inventory pointer isn't ready.
    fn grant_ledgered(&mut self, full_id: GoodsId, qty: i32) -> bool;
}

// ---------------------------------------------------------------------------------------------
// The reconciler
// ---------------------------------------------------------------------------------------------

/// Which OBSERVABILITY classes this tick is allowed to APPLY. The strangler cutover flips classes
/// on one at a time (`RECONCILE_APPLY=flags` then `flags,goods` then `flags,goods,ledger`): a
/// disabled class's actions are simply not this reconciler's job yet — the OLD handler still owns
/// them — so they are dropped from the action list (never applied, never block convergence).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplyClasses {
    pub flags: bool,
    pub goods: bool,
    pub ledger: bool,
}

impl ApplyClasses {
    /// Own everything (the end state, and what plain [`Reconciler::tick`] uses).
    pub const ALL: Self = Self {
        flags: true,
        goods: true,
        ledger: true,
    };
    /// Own nothing (equivalent to dry-run for the apply path).
    pub const NONE: Self = Self {
        flags: false,
        goods: false,
        ledger: false,
    };

    /// Is `action` in an enabled class?
    pub fn allows(&self, action: &Action) -> bool {
        match action {
            Action::SetFlag(_) | Action::ClearFlag(_) => self.flags,
            Action::GrantUnique(..) => self.goods,
            Action::GrantLedgered { .. } | Action::SkipLedgered { .. } => self.ledger,
        }
    }
}

/// Per-tick mutation budget so a large reconnect backlog drains over several ticks instead of
/// stalling the game thread. `GrantUnique` + `GrantLedgered` share the `goods` budget; `SetFlag` +
/// `ClearFlag` share the `flags` budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickBudget {
    pub goods: usize,
    pub flags: usize,
    /// Minimum wall-clock gap (ms, measured on [`WorldStability::now_ms`]) between GRANT bursts.
    /// While fewer than this many ms have elapsed since the last landed good/ledger grant, the
    /// goods + ledger classes are HELD for a later tick (flags still flow) so a large delta drains a
    /// burst-at-a-time rather than firing every `AddItemFunc` in one frame — the mass-grant CTD guard.
    /// `0` DISABLES pacing (the historical behavior; what [`Default`] and every pre-existing test use).
    pub min_grant_interval_ms: u64,
}

impl Default for TickBudget {
    fn default() -> Self {
        // Pacing OFF by default so existing callers/tests keep the original drain-within-budget
        // behavior; the LIVE client (`reconcile_io::tick`) builds a PACED budget explicitly.
        TickBudget {
            goods: 4,
            flags: 32,
            min_grant_interval_ms: 0,
        }
    }
}

/// What one [`Reconciler::tick`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickOutcome {
    /// Actions that LANDED this tick (in apply order).
    pub applied: Vec<Action>,
    /// The tick did nothing because the world was not stable (no read, no write).
    pub skipped_unstable: bool,
    /// The desired state was fully reached this tick (nothing left to do).
    pub converged: bool,
}

/// The tick-driven, event-nudged reconciler. Holds the server-authoritative inputs, the derived
/// desired state, and the persisted ledger watermark. Orchestration is PURE + host-testable; the
/// thread + memory I/O binding lives in the client's `reconcile_io.rs`.
#[derive(Clone, Debug)]
pub struct Reconciler {
    inputs: DesiredInputs,
    desired: DesiredState,
    /// The seed the current `desired` was built for (guards the reconnect-new-seed swap).
    session_seed: String,
    /// Persisted-per-save contiguous frontier for the consumable ledger.
    applied_watermark: ItemIndex,
    dirty: bool,
    /// [`WorldStability::now_ms`] of the most recent tick that LANDED a good/ledger grant, or `None`
    /// if none has landed yet. Drives the [`TickBudget::min_grant_interval_ms`] pacing gate. NOT
    /// persisted (a fresh session re-paces from its first grant); the ledger watermark — not this —
    /// is what guards against re-granting an already-applied item.
    last_grant_ms: Option<u64>,
}

impl Reconciler {
    /// Build from fresh inputs; use [`from_persisted`] to resume a save.
    ///
    /// The watermark starts at the desired state's [`ledger_floor`](DesiredState::ledger_floor) — 0
    /// when there are no start items, or the negative start-item band floor when there are — so a
    /// fresh save still OWES its slot-data start items (a blind 0 would strand their negative indices
    /// behind the frontier and never grant them).
    ///
    /// [`from_persisted`]: Reconciler::from_persisted
    pub fn new(inputs: DesiredInputs) -> Self {
        let desired = DesiredState::build(&inputs);
        let floor = desired.ledger_floor();
        let session_seed = inputs.seed.clone();
        Reconciler {
            inputs,
            desired,
            session_seed,
            applied_watermark: floor,
            dirty: true,
            last_grant_ms: None,
        }
    }

    /// Resume a save: seed the ledger watermark from persisted per-save state so consumables already
    /// applied on a previous connect are NOT re-granted (flask double-grant fix).
    pub fn from_persisted(inputs: DesiredInputs, applied_watermark: ItemIndex) -> Self {
        let desired = DesiredState::build(&inputs);
        let session_seed = inputs.seed.clone();
        Reconciler {
            inputs,
            desired,
            session_seed,
            applied_watermark,
            dirty: true,
            last_grant_ms: None,
        }
    }

    /// The SESSION-INIT seeding policy: pick the ledger watermark from the persisted `reconcile.json`
    /// entry AND this save's authoritative received frontier (`received_through`, the client save
    /// state's `last_received_index`), cross-checked against each other. This is the fix for the
    /// received-grant regression (er-reconciler-received-grant-regression): the persisted entry is
    /// keyed by SLOT NAME only, so a FRESH character on a slot whose earlier sessions persisted a
    /// positive watermark would inherit it and have its whole received stream (indices `0..N <
    /// stale_wm`) filtered out of the diff -- picked up, vanilla drop suppressed, nothing granted.
    ///
    /// Neither value alone is sufficient:
    ///   * `persisted` alone can be STALE-HIGH (another character/seed's frontier via the slot-name
    ///     key) -> strands this save's received stream (the regression).
    ///   * `received_through` alone can run AHEAD of what was actually placed: under the full
    ///     cutover the old receive path advances it past every item WITHOUT granting (grant is the
    ///     reconciler's job), so seeding from it blindly would skip items the reconciler never
    ///     placed (e.g. it sat unstable through the whole prior session).
    ///
    /// So: TRUST `persisted` only when it is `<= received_through` (a frontier can never legally sit
    /// past what this save has received -- if it does, it belongs to someone else). Distrusted or
    /// absent, fall back to `received_through` itself when positive (first cutover on an existing
    /// save: the OLD path already granted that prefix + the start items) or a truly fresh
    /// [`new`](Self::new) (ledger floor: everything owed).
    ///
    /// A trusted `persisted < received_through` is deliberate: it re-owes the gap the reconciler
    /// never placed (and, in the crash-before-`reconcile.json`-flush case, replays a small tail --
    /// the same replay-from-last-persisted semantics the old receive path always had). GOODS start
    /// items are presence-diffed (never on this watermark), so no seed choice can strand them.
    pub fn seeded(
        inputs: DesiredInputs,
        persisted: Option<ItemIndex>,
        received_through: ItemIndex,
    ) -> Self {
        match persisted.filter(|&wm| wm <= received_through) {
            Some(wm) => Self::from_persisted(inputs, wm),
            None if received_through > 0 => Self::from_persisted(inputs, received_through),
            None => Self::new(inputs),
        }
    }

    /// The persisted-per-save ledger watermark (the client writes this next to the save).
    pub fn applied_watermark(&self) -> ItemIndex {
        self.applied_watermark
    }

    /// Whether a convergence attempt is owed (an event nudged us, or a prior tick didn't finish).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// A read-only view of the current desired state (for the dry-run / logging path).
    pub fn desired(&self) -> &DesiredState {
        &self.desired
    }

    /// A read-only view of the current inputs.
    pub fn inputs(&self) -> &DesiredInputs {
        &self.inputs
    }

    /// EVENT NUDGE: connect / load / ItemReceived call this instead of mutating the game. It only
    /// marks the reconciler dirty; the next [`tick`](Self::tick) does the (idempotent) work.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Atomically swap in new server-authoritative inputs. If the room seed genuinely CHANGED
    /// (reconnect to a different seed without an ER reload), the per-seed state is rebuilt from
    /// scratch and the ledger watermark resets — the fix for the reconnect-new-seed panic
    /// (er-reconnect-newseed-panic): the old seed's tables never linger to be indexed into.
    ///
    /// Same-seed reconnects KEEP the watermark (so consumables don't re-grant) and just rebuild
    /// desired from the (possibly grown) received prefix. The swap is atomic: `inputs`, `desired`,
    /// `session_seed` and `applied_watermark` all move together, so no half-updated table is ever
    /// observed by a concurrent tick.
    pub fn set_inputs(&mut self, inputs: DesiredInputs) {
        let desired = DesiredState::build(&inputs);
        if crate::seed_change::is_seed_change(Some(&self.session_seed), &inputs.seed) {
            // Brand-new seed: reset to the NEW desired's band floor (not a blind 0) so the new seed's
            // own start items are re-owed rather than stranded behind a stale frontier.
            self.applied_watermark = desired.ledger_floor();
        }
        self.session_seed = inputs.seed.clone();
        self.desired = desired;
        self.inputs = inputs;
        self.dirty = true;
    }

    /// Snapshot ONLY the flags/goods the desired state references. Gated by the caller on stability.
    fn snapshot(&self, io: &dyn GameIo) -> ObservedState {
        let mut flags = BTreeMap::new();
        for &f in self.desired.flags.keys() {
            flags.insert(f, io.get_flag(f));
        }
        let mut goods = BTreeSet::new();
        for &g in self.desired.unique_goods.keys() {
            if io.has_good(g) {
                goods.insert(g);
            }
        }
        ObservedState {
            flags,
            unique_goods: goods,
            applied_watermark: self.applied_watermark,
        }
    }

    /// DRY-RUN (phase 0): compute — but do NOT apply — the actions the next [`tick`](Self::tick)
    /// WOULD take against the current live observation. Read-only (it snapshots via `io` and diffs;
    /// it never mutates the game or the watermark), so the client can log the real per-action diff
    /// under `RECONCILE_DRYRUN=1` before any mutation path is switched over. Empty while unstable.
    pub fn dry_run_actions(&self, io: &dyn GameIo) -> Vec<Action> {
        if !io.stability().stable() {
            return Vec::new();
        }
        let observed = self.snapshot(io);
        diff(&self.desired, &observed)
    }

    /// One convergence attempt. Reads stability; if not stable, does NOTHING (no read, no write) and
    /// stays dirty. Otherwise snapshots, diffs, and applies up to the per-tick budget. Flag-holder /
    /// inventory not-ready responses (and budget exhaustion) leave the remainder for the next tick.
    pub fn tick(&mut self, io: &mut dyn GameIo, budget: TickBudget) -> TickOutcome {
        self.tick_with_classes(io, budget, ApplyClasses::ALL)
    }

    /// Like [`tick`](Self::tick) but only applies actions in the enabled [`ApplyClasses`] — the
    /// mechanism the strangler cutover uses to hand ONE class at a time to the reconciler while the
    /// old handlers still own the rest. Disabled-class actions are dropped from the plan (not
    /// applied, not counted, not blocking convergence), so `converged` reflects only the classes
    /// this reconciler currently owns.
    pub fn tick_with_classes(
        &mut self,
        io: &mut dyn GameIo,
        budget: TickBudget,
        classes: ApplyClasses,
    ) -> TickOutcome {
        let stab = io.stability();
        if !stab.stable() {
            return TickOutcome {
                applied: Vec::new(),
                skipped_unstable: true,
                converged: false,
            };
        }

        let observed = self.snapshot(io);
        let mut actions = diff(&self.desired, &observed);
        actions.retain(|a| classes.allows(a));
        if actions.is_empty() {
            self.dirty = false;
            return TickOutcome {
                applied: Vec::new(),
                skipped_unstable: false,
                converged: true,
            };
        }

        // PACING GATE (mass-grant CTD guard): hold the goods + ledger classes until
        // `min_grant_interval_ms` has elapsed since the last landed grant, so a large delta drains a
        // burst-at-a-time instead of flooding `AddItemFunc` in one frame. `interval == 0` disables it
        // (the historical all-at-once-within-budget path). Flags are cheap `CSEventFlagMan` writes and
        // are NEVER paced — a held goods class must not stall region-open / map-reveal.
        let now = stab.now_ms;
        let grants_allowed = budget.min_grant_interval_ms == 0
            || match self.last_grant_ms {
                None => true, // nothing granted yet -> the first burst is always allowed
                Some(t) => now.saturating_sub(t) >= budget.min_grant_interval_ms,
            };

        let mut applied = Vec::new();
        let mut flags_used = 0usize;
        let mut goods_used = 0usize;
        let mut deferred = false;
        let mut granted_this_tick = false;

        // Pass 1: flags + unique goods (independent budgets; order = the diff's deterministic order).
        for a in &actions {
            match a {
                Action::SetFlag(f) => {
                    if flags_used >= budget.flags {
                        deferred = true;
                        continue;
                    }
                    if io.set_flag(*f, true) {
                        flags_used += 1;
                        applied.push(a.clone());
                    } else {
                        deferred = true; // holder not ready -> retry next tick
                    }
                }
                Action::ClearFlag(f) => {
                    if flags_used >= budget.flags {
                        deferred = true;
                        continue;
                    }
                    if io.set_flag(*f, false) {
                        flags_used += 1;
                        applied.push(a.clone());
                    } else {
                        deferred = true;
                    }
                }
                Action::GrantUnique(g, comp) => {
                    if !grants_allowed {
                        deferred = true; // paced: hold this grant for a later tick
                        continue;
                    }
                    if goods_used >= budget.goods {
                        deferred = true;
                        continue;
                    }
                    if io.grant_good(*g, comp) {
                        goods_used += 1;
                        granted_this_tick = true;
                        applied.push(a.clone());
                    } else {
                        deferred = true; // inventory not ready -> retry next tick
                    }
                }
                Action::GrantLedgered { .. } | Action::SkipLedgered { .. } => {} // pass 2 (contiguity)
            }
        }

        // Pass 2: the ledger, in index order, advancing the watermark ONLY across a contiguous run
        // of successful grants. A budget stop or a not-ready inventory holds the watermark so the
        // tail replays next tick (mirrors receive.rs's rollback protocol).
        for a in &actions {
            match a {
                Action::GrantLedgered {
                    index,
                    full_id,
                    qty,
                } => {
                    if !grants_allowed {
                        deferred = true; // paced: hold the ledger tail for a later tick
                        break;
                    }
                    if goods_used >= budget.goods {
                        deferred = true;
                        break;
                    }
                    if io.grant_ledgered(*full_id, *qty) {
                        goods_used += 1;
                        granted_this_tick = true;
                        self.applied_watermark = index + 1;
                        applied.push(a.clone());
                    } else {
                        deferred = true;
                        break;
                    }
                }
                // Deduped shop echo: no game call, no budget cost — just advance the frontier PAST it
                // so it stays contiguous and a reload won't reconsider it.
                Action::SkipLedgered { index } => {
                    self.applied_watermark = index + 1;
                    applied.push(a.clone());
                }
                _ => {}
            }
        }

        // Arm the pacing cooldown from THIS tick's clock if any grant landed, so the next burst waits
        // a full `min_grant_interval_ms`. Recorded once per tick: a whole `budget.goods` burst shares
        // one timestamp.
        if granted_this_tick {
            self.last_grant_ms = Some(now);
        }

        let converged = !deferred && applied.len() == actions.len();
        self.dirty = !converged;
        TickOutcome {
            applied,
            skipped_unstable: false,
            converged,
        }
    }

    /// Drive [`tick`](Self::tick) until convergence or `max_ticks` (test/dry-run helper). Returns the
    /// number of ticks spent. A tick skipped for instability still counts (so a caller can bound
    /// spins) but does not advance toward convergence.
    pub fn run_to_fixpoint(
        &mut self,
        io: &mut dyn GameIo,
        budget: TickBudget,
        max_ticks: usize,
    ) -> usize {
        let mut n = 0;
        while n < max_ticks {
            n += 1;
            let out = self.tick(io, budget);
            if out.converged {
                break;
            }
        }
        n
    }
}

// ---------------------------------------------------------------------------------------------
// Host-side mock game (also used by the sibling reconciler_replay module's tests)
// ---------------------------------------------------------------------------------------------

/// In-memory [`GameIo`] used by every reconciler test (and the replay harness) so the tests drive
/// the ACTUAL reconcile loop. Models a live flag store, a unique-good inventory, a consumable grant
/// log (which ACCUMULATES — a double-grant would show up as a second entry), and a scriptable
/// stability timeline plus flag-holder / inventory readiness.
#[derive(Clone, Debug)]
pub struct MockGame {
    pub flags: BTreeMap<FlagId, bool>,
    pub goods: BTreeSet<GoodsId>,
    /// Every consumable grant that LANDED: (full_id, qty). Length is the double-grant detector.
    pub ledger_log: Vec<(GoodsId, i32)>,
    /// Flag holder ready (false => set_flag returns false, caller retries).
    pub flag_ready: bool,
    /// Inventory pointer captured (false => grants return false, caller retries).
    pub inventory_ready: bool,
    pub stability: WorldStability,
}

impl Default for MockGame {
    fn default() -> Self {
        MockGame {
            flags: BTreeMap::new(),
            goods: BTreeSet::new(),
            ledger_log: Vec::new(),
            flag_ready: true,
            inventory_ready: true,
            stability: WorldStability {
                in_game: true,
                player_valid: true,
                dwell_ms: WorldStability::SETTLE_MS,
                real_pickup_seen: true,
                now_ms: 0,
            },
        }
    }
}

impl MockGame {
    /// A stable, ready game (fully live world).
    pub fn stable() -> Self {
        MockGame::default()
    }

    /// A game that is NOT stable yet (loading; before the settle window, no real pickup).
    pub fn loading() -> Self {
        MockGame {
            stability: WorldStability {
                in_game: true,
                player_valid: true,
                dwell_ms: 0,
                real_pickup_seen: false,
                now_ms: 0,
            },
            ..MockGame::default()
        }
    }

    pub fn set_stable(&mut self, v: bool) {
        // Preserve the injected monotonic clock across a stability toggle (a load screen must not
        // rewind `now_ms` — the pacing cooldown is measured on it).
        let now_ms = self.stability.now_ms;
        if v {
            self.stability = WorldStability {
                in_game: true,
                player_valid: true,
                dwell_ms: WorldStability::SETTLE_MS,
                real_pickup_seen: true,
                now_ms,
            };
        } else {
            self.stability = WorldStability {
                in_game: true,
                player_valid: true,
                dwell_ms: 0,
                real_pickup_seen: false,
                now_ms,
            };
        }
    }

    /// Advance the injected monotonic session clock (`now_ms`) by `dt` ms. Pacing tests use this to
    /// let a grant cooldown elapse; a load-screen toggle (`set_stable`) preserves it.
    pub fn advance_ms(&mut self, dt: u64) {
        self.stability.now_ms = self.stability.now_ms.saturating_add(dt);
    }

    /// How many times a consumable full_id was granted (>1 == double-grant).
    pub fn ledger_count(&self, full_id: GoodsId) -> usize {
        self.ledger_log
            .iter()
            .filter(|&&(id, _)| id == full_id)
            .count()
    }

    /// Force a good OUT of inventory (models a bulk-load clobber / a lost item), so the next tick's
    /// diff must re-grant it — the self-heal path.
    pub fn drop_good(&mut self, goods: GoodsId) {
        self.goods.remove(&goods);
    }
}

impl GameIo for MockGame {
    fn stability(&self) -> WorldStability {
        self.stability
    }
    fn get_flag(&self, flag: FlagId) -> bool {
        self.flags.get(&flag).copied().unwrap_or(false)
    }
    fn set_flag(&mut self, flag: FlagId, on: bool) -> bool {
        if !self.flag_ready {
            return false;
        }
        self.flags.insert(flag, on);
        true
    }
    fn has_good(&self, goods: GoodsId) -> bool {
        self.goods.contains(&goods)
    }
    fn grant_good(&mut self, goods: GoodsId, companion_flags: &[FlagId]) -> bool {
        if !self.inventory_ready {
            return false;
        }
        self.goods.insert(goods);
        for &f in companion_flags {
            self.flags.insert(f, true);
        }
        true
    }
    fn grant_ledgered(&mut self, full_id: GoodsId, qty: i32) -> bool {
        if !self.inventory_ready {
            return false;
        }
        self.ledger_log.push((full_id, qty));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- item builders -------------------------------------------------------------------

    fn region(index: ItemIndex, name: &str, flags: &[FlagId]) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::RegionFlags(flags.to_vec()),
        }
    }
    fn map_piece(index: ItemIndex, name: &str, flags: &[FlagId]) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::MapReveal(flags.to_vec()),
        }
    }
    fn great_rune(index: ItemIndex, name: &str, goods: GoodsId, restored: FlagId) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::GreatRune {
                goods,
                restored_flag: restored,
            },
        }
    }
    fn consumable(index: ItemIndex, name: &str, full_id: GoodsId, qty: i32) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::Consumable {
                full_id,
                qty,
                echo_skip: false,
            },
        }
    }
    /// A shop-native consumable: `echo_skip=true` is the echo of a natively-sold reward (dedup).
    fn shop_consumable(
        index: ItemIndex,
        name: &str,
        full_id: GoodsId,
        qty: i32,
        echo_skip: bool,
    ) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::Consumable {
                full_id,
                qty,
                echo_skip,
            },
        }
    }

    fn key_item(index: ItemIndex, name: &str, goods: GoodsId, obtained: &[FlagId]) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::KeyItem {
                goods,
                obtained_flags: obtained.to_vec(),
            },
        }
    }
    fn goal_flag(index: ItemIndex, name: &str, flag: FlagId) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::GoalFlag(flag),
        }
    }
    /// A single received copy of a progressive item. All copies of one name carry the same tiers.
    fn progressive(
        index: ItemIndex,
        name: &str,
        tiers: &[(&[GoodsId], &[FlagId])],
        overflow: GoodsId,
    ) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::Progressive {
                tiers: tiers
                    .iter()
                    .map(|(g, f)| ProgTier {
                        goods: g.to_vec(),
                        flags: f.to_vec(),
                        consumed: false,
                    })
                    .collect(),
                overflow_full_id: overflow,
            },
        }
    }

    /// A received copy of a progressive item whose ladder mixes OWNED and CONSUMED rungs
    /// (per-rung `consumed` in the third tuple slot).
    fn progressive_mixed(
        index: ItemIndex,
        name: &str,
        tiers: &[(&[GoodsId], &[FlagId], bool)],
        overflow: GoodsId,
    ) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::Progressive {
                tiers: tiers
                    .iter()
                    .map(|(g, f, c)| ProgTier {
                        goods: g.to_vec(),
                        flags: f.to_vec(),
                        consumed: *c,
                    })
                    .collect(),
                overflow_full_id: overflow,
            },
        }
    }

    /// The flask-upgrade ladder shape from the live CTD: every rung a CONSUMED good (Golden
    /// Seed / Sacred Tear) plus an observable rung flag.
    fn flask_tiers() -> Vec<(&'static [GoodsId], &'static [FlagId], bool)> {
        vec![
            (&[10010][..], &[71001][..], true),
            (&[10020][..], &[71002][..], true),
        ]
    }

    /// A two-tier progressive bell used across the progressive tests.
    fn bell_tiers() -> Vec<(&'static [GoodsId], &'static [FlagId])> {
        vec![(&[8101][..], &[70001][..]), (&[8102][..], &[70002][..])]
    }

    fn inputs(seed: &str, received: Vec<ReceivedItem>, seal_flags: Vec<FlagId>) -> DesiredInputs {
        DesiredInputs {
            seed: seed.into(),
            save: SaveIdentity("slot0".into()),
            received,
            slot_data: SlotData {
                seal_flags,
                ..Default::default()
            },
        }
    }

    /// Inputs carrying only slot-data (no received stream) for the bulk-grant tests.
    fn bulk_inputs(sd: SlotData) -> DesiredInputs {
        DesiredInputs {
            seed: "A".into(),
            save: SaveIdentity("slot0".into()),
            received: vec![],
            slot_data: sd,
        }
    }

    // ---- desired-state construction rules -----------------------------------------------

    #[test]
    fn map_piece_never_becomes_a_unique_good() {
        // er-map-pieces-granted-on-connect: a map piece must contribute ONLY reveal flags, never a
        // grantable good. Structurally guaranteed by the MapReveal variant, asserted end-to-end.
        let d = DesiredState::build(&inputs(
            "A",
            vec![map_piece(0, "Underground Map", &[62060, 62061, 82001])],
            vec![],
        ));
        assert!(
            d.unique_goods.is_empty(),
            "map pieces must produce no unique goods"
        );
        assert_eq!(
            d.flags.get(&82001),
            Some(&true),
            "the 82001 view-unlock must be desired"
        );
        assert_eq!(d.flags.get(&62060), Some(&true));
        let obs = ObservedState::default();
        let actions = diff(&d, &obs);
        assert!(
            !actions.iter().any(|a| matches!(a, Action::GrantUnique(..))),
            "no GrantUnique may ever be emitted for a map piece"
        );
    }

    #[test]
    fn great_rune_is_good_plus_restored_flag() {
        let d = DesiredState::build(&inputs(
            "A",
            vec![great_rune(0, "Godrick's Great Rune", 191, 6901)],
            vec![],
        ));
        assert!(d.unique_goods.contains_key(&191));
        assert_eq!(d.unique_goods[&191].companion_flags, vec![6901]);
        assert_eq!(
            d.flags.get(&6901),
            Some(&true),
            "restored flag is an independent observable flag"
        );
    }

    #[test]
    fn region_seal_default_overridden_by_received_lock() {
        let sealed = DesiredState::build(&inputs("A", vec![], vec![76980]));
        assert_eq!(sealed.flags.get(&76980), Some(&false));
        assert!(
            sealed.owned_flags.contains(&76980),
            "seal flags are owned (clearable)"
        );

        let opened = DesiredState::build(&inputs(
            "A",
            vec![region(0, "Caelid Lock", &[76980])],
            vec![76980],
        ));
        assert_eq!(
            opened.flags.get(&76980),
            Some(&true),
            "the Lock opens the region"
        );
    }

    // ---- diff semantics ------------------------------------------------------------------

    #[test]
    fn clear_only_touches_owned_flags() {
        let mut d = DesiredState::default();
        d.flags.insert(76980, false);
        d.owned_flags.insert(76980); // owned
        d.flags.insert(9999, false); // not owned

        let mut obs = ObservedState::default();
        obs.flags.insert(76980, true);
        obs.flags.insert(9999, true);

        let actions = diff(&d, &obs);
        assert!(actions.contains(&Action::ClearFlag(76980)));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::ClearFlag(9999))),
            "a non-owned flag must never be cleared"
        );
    }

    // ---- stability gate ------------------------------------------------------------------

    #[test]
    fn torch_grant_blocked_until_stable() {
        // gf-start-item-clobber generalized: no mutation may occur while the world is not stable.
        let mut r = Reconciler::new(inputs("A", vec![consumable(0, "Torch", 2008, 1)], vec![]));
        let mut g = MockGame::loading(); // dwell 0, no real pickup -> !stable

        let out = r.tick(&mut g, TickBudget::default());
        assert!(out.skipped_unstable, "an unstable tick must skip");
        assert!(
            g.ledger_log.is_empty(),
            "no grant may land while unstable (no clobber race)"
        );
        assert_eq!(
            r.applied_watermark(),
            0,
            "watermark must not advance on an unstable tick"
        );

        g.set_stable(true);
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(2008),
            1,
            "the Torch grants exactly once once stable"
        );
    }

    // ---- flask double-grant (ledger watermark) ------------------------------------------

    #[test]
    fn flask_ledger_grants_once_across_a_reload() {
        // er-flask-double-grant-reconnect: a tutorial-death reload re-derives the start items; the
        // persisted ledger watermark must make the post-reload diff EMPTY (no second grant).
        let items = vec![
            consumable(0, "Flask of Crimson Tears", 1001, 3),
            consumable(1, "Flask of Cerulean Tears", 1002, 1),
        ];
        let mut g = MockGame::stable();

        let mut r = Reconciler::new(inputs("A", items.clone(), vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(g.ledger_count(1001), 1);
        assert_eq!(g.ledger_count(1002), 1);
        let wm = r.applied_watermark();
        assert_eq!(wm, 2, "watermark advanced past both consumables");

        // RELOAD: rebuild the reconciler FROM PERSISTED watermark and the full stream.
        let mut r2 = Reconciler::from_persisted(inputs("A", items, vec![]), wm);
        r2.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(1001),
            1,
            "no second grant after reload (flask double-grant fix)"
        );
        assert_eq!(g.ledger_count(1002), 1);
    }

    // ---- great-rune double-grant (unique-good diff) -------------------------------------

    #[test]
    fn great_rune_grants_once_then_diff_is_empty() {
        // gf-great-rune-double-grant: once the good is present the diff for it is empty, so a second
        // convergence pass (reconnect) re-grants NOTHING.
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs(
            "A",
            vec![great_rune(0, "Godrick's Great Rune", 191, 6901)],
            vec![],
        ));

        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(g.has_good(191), "the rune good is granted once");
        assert!(g.get_flag(6901), "the restored companion flag is set");

        let obs = ObservedState {
            flags: [(6901u32, true)].into_iter().collect(),
            unique_goods: [191i32].into_iter().collect(),
            applied_watermark: 0,
        };
        assert!(
            diff(r.desired(), &obs).is_empty(),
            "present good -> empty diff (no double-grant)"
        );
    }

    #[test]
    fn tick_with_classes_owns_only_enabled_classes() {
        // Strangler phase 1 (flags only): the reconciler sets flags but leaves goods + ledger to the
        // old handlers. Converged is reached even though goods/ledger actions still "exist".
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs(
            "A",
            vec![
                region(0, "Limgrave Lock", &[76971]),
                great_rune(1, "Godrick's Great Rune", 191, 6901),
                consumable(2, "Torch", 2008, 1),
            ],
            vec![],
        ));

        let flags_only = ApplyClasses {
            flags: true,
            goods: false,
            ledger: false,
        };
        let mut n = 0;
        loop {
            let out = r.tick_with_classes(&mut g, TickBudget::default(), flags_only);
            n += 1;
            if out.converged || n > 8 {
                break;
            }
        }
        assert!(g.get_flag(76971), "region flag applied under flags-only");
        assert!(
            !g.has_good(191),
            "the rune good is NOT granted while goods is disabled"
        );
        assert_eq!(
            g.ledger_count(2008),
            0,
            "no consumable granted while ledger is disabled"
        );

        // Phase 3: enable everything; the good + consumable now land, still exactly once.
        r.mark_dirty();
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(g.has_good(191), "rune granted once ALL classes are enabled");
        assert_eq!(g.ledger_count(2008), 1, "Torch granted exactly once");
    }

    #[test]
    fn dry_run_actions_compute_but_never_mutate() {
        // Phase-0 dry run: dry_run_actions reports what a tick WOULD do, but touches nothing.
        let mut g = MockGame::stable();
        let r = Reconciler::new(inputs(
            "A",
            vec![
                region(0, "Limgrave Lock", &[76971]),
                great_rune(1, "Godrick's Great Rune", 191, 6901),
                consumable(2, "Torch", 2008, 1),
            ],
            vec![],
        ));

        let planned = r.dry_run_actions(&g);
        assert!(planned.iter().any(|a| matches!(a, Action::SetFlag(76971))));
        assert!(planned
            .iter()
            .any(|a| matches!(a, Action::GrantUnique(191, _))));
        assert!(planned
            .iter()
            .any(|a| matches!(a, Action::GrantLedgered { full_id: 2008, .. })));
        // Nothing was applied: no flags, no goods, no ledger entries, watermark untouched.
        assert!(g.flags.is_empty() && g.goods.is_empty() && g.ledger_log.is_empty());
        assert_eq!(r.applied_watermark(), 0);

        // An unstable game plans nothing.
        g.set_stable(false);
        assert!(
            r.dry_run_actions(&g).is_empty(),
            "no plan while the world is unstable"
        );
    }

    // ---- bundle-lock grace self-heal ----------------------------------------------------

    #[test]
    fn grace_flag_self_heals_when_lost() {
        // er-bundle-lock-grace-reconcile-gap: a region grace bundle flag DESIRED set but reading
        // clear (lost after a save-load) must be re-set on the next stable tick — repeatedly.
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs(
            "A",
            vec![region(0, "Limgrave Lock", &[76971, 76972])],
            vec![],
        ));

        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(g.get_flag(76971) && g.get_flag(76972), "grace bundle set");

        g.flags.insert(76971, false); // the game loses a bundle flag
        r.mark_dirty();
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(
            g.get_flag(76971),
            "the lost grace flag self-heals on the next reconcile"
        );
    }

    // ---- reconnect-new-seed swap --------------------------------------------------------

    #[test]
    fn seed_change_resets_ledger_and_rebuilds_desired_without_panic() {
        // er-reconnect-newseed-panic: reconnecting to a DIFFERENT seed atomically swaps inputs,
        // rebuilds the per-seed desired state, and resets the ledger watermark so seed B's own
        // consumables grant fresh. No indexing, no stale table survives.
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs(
            "A",
            vec![consumable(0, "Golden Rune", 5001, 1)],
            vec![],
        ));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(r.applied_watermark(), 1);

        r.set_inputs(inputs(
            "B",
            vec![consumable(0, "Golden Rune", 5002, 1)],
            vec![],
        ));
        assert_eq!(
            r.applied_watermark(),
            0,
            "a genuine seed change resets the ledger watermark"
        );
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(5002),
            1,
            "seed B's consumable grants fresh after the swap"
        );
    }

    #[test]
    fn same_seed_reconnect_keeps_watermark() {
        let mut r =
            Reconciler::from_persisted(inputs("A", vec![consumable(0, "X", 42, 1)], vec![]), 1);
        r.set_inputs(inputs(
            "A",
            vec![consumable(0, "X", 42, 1), consumable(1, "Y", 43, 1)],
            vec![],
        ));
        assert_eq!(
            r.applied_watermark(),
            1,
            "same-seed reconnect preserves the watermark"
        );
    }

    // ---- budget draining -----------------------------------------------------------------

    #[test]
    fn a_large_backlog_drains_over_several_ticks_within_budget() {
        let mut received = Vec::new();
        for i in 0..20i64 {
            received.push(consumable(i, &format!("C{i}"), 6000 + i as i32, 1));
        }
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", received, vec![]));

        let budget = TickBudget {
            goods: 4,
            flags: 32,
            min_grant_interval_ms: 0,
        };
        let out = r.tick(&mut g, budget);
        assert_eq!(
            out.applied.len(),
            4,
            "one tick applies at most `goods` ledger grants"
        );
        assert!(!out.converged, "still more to do");

        let ticks = r.run_to_fixpoint(&mut g, budget, 50);
        assert_eq!(
            g.ledger_log.len(),
            20,
            "all 20 consumables eventually land, each once"
        );
        assert!(
            ticks <= 6,
            "20 items / 4 per tick drains in a handful of ticks, took {ticks}"
        );
    }

    // ---- grant pacing (mass-grant CTD guard) ---------------------------------------------

    /// The core guard the CTD motivated: a large consumable delta must NOT all grant in one frame.
    /// With `min_grant_interval_ms` set, one tick grants at most a `goods`-sized burst and then HOLDS
    /// until the injected clock advances past the interval — no matter how many ticks fire meanwhile.
    #[test]
    fn grant_pacing_spaces_a_large_delta_into_bursts() {
        let received: Vec<_> = (0..12i64)
            .map(|i| consumable(i, &format!("C{i}"), 6000 + i as i32, 1))
            .collect();
        let mut g = MockGame::stable(); // now_ms starts at 0
        let mut r = Reconciler::new(inputs("A", received, vec![]));
        let budget = TickBudget {
            goods: 2,
            flags: 32,
            min_grant_interval_ms: 150,
        };

        // t=0: first burst lands (capped at `goods`), then the cooldown arms.
        let out = r.tick(&mut g, budget);
        assert_eq!(out.applied.len(), 2, "first burst is capped at `goods`");
        assert!(!out.converged, "backlog remains");

        // Ticking WITHOUT advancing the clock grants nothing — the cooldown has not elapsed.
        for _ in 0..5 {
            let out = r.tick(&mut g, budget);
            assert!(
                out.applied.is_empty(),
                "no grant lands before the interval elapses"
            );
            assert!(!out.converged, "still owed, stays dirty");
        }
        assert_eq!(
            g.ledger_log.len(),
            2,
            "clock frozen => still only the first burst landed"
        );

        // Advancing past the interval releases exactly one more burst.
        g.advance_ms(150);
        let out = r.tick(&mut g, budget);
        assert_eq!(
            out.applied.len(),
            2,
            "a second burst lands once the cooldown clears"
        );
        assert_eq!(g.ledger_log.len(), 4);

        // Just short of the interval holds again (boundary check).
        g.advance_ms(149);
        let out = r.tick(&mut g, budget);
        assert!(
            out.applied.is_empty(),
            "149ms < 150ms interval => still held"
        );
    }

    /// Pacing must not LOSE or DUPLICATE anything: advancing the clock each tick eventually drains the
    /// whole delta, every consumable exactly once (the CTD fix must keep the idempotency guarantee).
    #[test]
    fn grant_pacing_still_drains_everything_exactly_once() {
        let received: Vec<_> = (0..12i64)
            .map(|i| consumable(i, &format!("C{i}"), 6000 + i as i32, 1))
            .collect();
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", received, vec![]));
        let budget = TickBudget {
            goods: 2,
            flags: 32,
            min_grant_interval_ms: 150,
        };

        for _ in 0..20 {
            r.tick(&mut g, budget);
            if g.ledger_log.len() == 12 {
                break;
            }
            g.advance_ms(150); // a full interval per tick => each tick may release a burst
        }
        assert_eq!(
            g.ledger_log.len(),
            12,
            "all consumables eventually land under pacing"
        );
        for i in 0..12i32 {
            assert_eq!(g.ledger_count(6000 + i), 1, "no double-grant under pacing");
        }
    }

    /// Flags are NEVER paced: while a good is held in cooldown, a freshly-arrived region-open flag
    /// still lands immediately (a held goods class must not stall region access / map reveal). Also
    /// proves the held good self-heals once the interval clears.
    #[test]
    fn pacing_holds_goods_but_lets_flags_through() {
        let budget = TickBudget {
            goods: 4,
            flags: 32,
            min_grant_interval_ms: 150,
        };
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", vec![key_item(0, "K", 9000, &[])], vec![]));

        // t=0: the good grants (first grant is always allowed) and arms the cooldown.
        r.tick(&mut g, budget);
        assert!(g.has_good(9000), "the key-item good lands at t=0");

        // Same instant: a bulk-load clobbers the good AND a new region lock arrives.
        g.drop_good(9000);
        r.set_inputs(inputs(
            "A",
            vec![key_item(0, "K", 9000, &[]), region(1, "L", &[76971])],
            vec![],
        ));
        let out = r.tick(&mut g, budget); // clock still 0 => the good re-grant is held
        assert!(
            g.get_flag(76971),
            "the flag lands immediately (flags are never paced)"
        );
        assert!(
            !g.has_good(9000),
            "the good re-grant is HELD until the interval elapses"
        );
        assert!(!out.converged, "still owes the good, stays dirty");

        // Once the cooldown clears, the held good self-heals.
        g.advance_ms(150);
        r.run_to_fixpoint(&mut g, budget, 4);
        assert!(
            g.has_good(9000),
            "the good self-heals after the cooldown clears"
        );
    }

    /// Pacing is OFF by default (`TickBudget::default().min_grant_interval_ms == 0`): the historical
    /// drain-within-budget behavior is unchanged for every pre-existing caller/test.
    #[test]
    fn default_budget_leaves_pacing_disabled() {
        assert_eq!(TickBudget::default().min_grant_interval_ms, 0);
        let received: Vec<_> = (0..10i64)
            .map(|i| consumable(i, &format!("C{i}"), 6000 + i as i32, 1))
            .collect();
        let mut g = MockGame::stable(); // clock never advances
        let mut r = Reconciler::new(inputs("A", received, vec![]));
        // With no pacing, a frozen clock still drains fully (budget is the only limiter).
        r.run_to_fixpoint(&mut g, TickBudget::default(), 20);
        assert_eq!(
            g.ledger_log.len(),
            10,
            "unpaced: drains without needing the clock to move"
        );
    }

    #[test]
    fn flag_holder_not_ready_defers_without_losing_the_flag() {
        let mut g = MockGame::stable();
        g.flag_ready = false; // holder not ready
        let mut r = Reconciler::new(inputs("A", vec![region(0, "L", &[76971])], vec![]));

        let out = r.tick(&mut g, TickBudget::default());
        assert!(!out.converged, "holder-not-ready must defer");
        assert!(
            !g.get_flag(76971),
            "nothing set while the holder is not ready"
        );

        g.flag_ready = true;
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(
            g.get_flag(76971),
            "the deferred flag lands once the holder is ready"
        );
    }

    // ---- progressive items (count-based tiers) -------------------------------------------

    #[test]
    fn progressive_tiers_land_as_unique_goods_plus_flags() {
        let tiers = bell_tiers();
        let d = DesiredState::build(&inputs(
            "A",
            vec![
                progressive(0, "progressive_stone_bell", &tiers, 2919),
                progressive(1, "progressive_stone_bell", &tiers, 2919),
            ],
            vec![],
        ));
        assert!(d.unique_goods.contains_key(&8101) && d.unique_goods.contains_key(&8102));
        assert_eq!(d.flags.get(&70001), Some(&true));
        assert_eq!(d.flags.get(&70002), Some(&true));
        assert!(
            d.ledgered.is_empty(),
            "no overflow while copies <= tier count"
        );
        assert!(
            !d.owned_flags.contains(&70001),
            "tier flags are set-only, never owned"
        );
    }

    #[test]
    fn progressive_overflow_becomes_a_ledgered_consumable() {
        let tiers = bell_tiers();
        let d = DesiredState::build(&inputs(
            "A",
            vec![
                progressive(0, "progressive_stone_bell", &tiers, 2919),
                progressive(1, "progressive_stone_bell", &tiers, 2919),
                progressive(2, "progressive_stone_bell", &tiers, 2919),
            ],
            vec![],
        ));
        assert_eq!(d.unique_goods.len(), 2, "both tiers still unique goods");
        assert_eq!(d.ledgered.len(), 1, "exactly one overflow consumable");
        assert_eq!(d.ledgered[0].full_id, 2919);
        assert_eq!(d.ledgered[0].qty, 1);
    }

    #[test]
    fn progressive_desired_is_order_independent() {
        let tiers = bell_tiers();
        let a = DesiredState::build(&inputs(
            "A",
            vec![
                progressive(0, "progressive_stone_bell", &tiers, 2919),
                progressive(1, "progressive_stone_bell", &tiers, 2919),
                progressive(2, "progressive_stone_bell", &tiers, 2919),
            ],
            vec![],
        ));
        let b = DesiredState::build(&inputs(
            "A",
            vec![
                progressive(2, "progressive_stone_bell", &tiers, 2919),
                progressive(0, "progressive_stone_bell", &tiers, 2919),
                progressive(1, "progressive_stone_bell", &tiers, 2919),
            ],
            vec![],
        ));
        assert_eq!(a.unique_goods, b.unique_goods);
        assert_eq!(a.flags, b.flags);
        assert_eq!(a.ledgered.len(), b.ledgered.len());
        assert_eq!(a.ledgered[0].full_id, b.ledgered[0].full_id);
    }

    #[test]
    fn progressive_grants_each_tier_and_overflow_once_across_a_reload() {
        let tiers = bell_tiers();
        let items = vec![
            progressive(0, "progressive_stone_bell", &tiers, 2919),
            progressive(1, "progressive_stone_bell", &tiers, 2919),
            progressive(2, "progressive_stone_bell", &tiers, 2919),
        ];
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", items.clone(), vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert!(
            g.has_good(8101) && g.has_good(8102),
            "both bell tiers granted"
        );
        assert!(
            g.get_flag(70001) && g.get_flag(70002),
            "both tier flags set"
        );
        assert_eq!(g.ledger_count(2919), 1, "exactly one overflow Lord's Rune");
        let wm = r.applied_watermark();

        let mut r2 = Reconciler::from_persisted(inputs("A", items, vec![]), wm);
        r2.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert_eq!(
            g.ledger_count(2919),
            1,
            "no second overflow grant after reload"
        );
    }

    // ---- CONSUMED progressive tiers (the flask-upgrade CTD, 2026-07-12) -------------------
    //
    // A rung with `consumed: true` grants goods the player SPENDS (Golden Seed / Sacred Tear at
    // a Site of Grace). Presence-diffing them (`unique_goods`) re-granted them the instant they
    // left the inventory: upgrade -> re-grant -> upgrade -> ... until flask potency ran past its
    // cap and the game crashed. Consumed rungs are LEDGERED (granted exactly once, keyed by the
    // copy's stream index) exactly like overflow; OWNED rungs (no `consumed`) keep self-healing.

    #[test]
    fn consumed_tier_is_ledgered_never_a_unique_good() {
        let tiers = flask_tiers();
        let d = DesiredState::build(&inputs(
            "A",
            vec![
                progressive_mixed(3, "Progressive Flask Upgrade", &tiers, 2919),
                progressive_mixed(7, "Progressive Flask Upgrade", &tiers, 2919),
            ],
            vec![],
        ));
        assert!(
            d.unique_goods.is_empty(),
            "a consumed rung must NEVER become a self-healing unique good"
        );
        let got: Vec<_> = d
            .ledgered
            .iter()
            .map(|l| (l.index, l.full_id, l.qty, l.apply))
            .collect();
        assert_eq!(
            got,
            vec![(3, 10010, 1, true), (7, 10020, 1, true)],
            "each consumed rung is ledgered once, keyed by ITS copy's stream index"
        );
        assert_eq!(
            d.flags.get(&71001),
            Some(&true),
            "rung flags stay observable + self-healing"
        );
        assert_eq!(d.flags.get(&71002), Some(&true));
        assert!(
            !d.owned_flags.contains(&71001),
            "rung flags set-only, never owned"
        );
    }

    #[test]
    fn consumed_tier_grants_once_and_stays_spent_after_consumption() {
        // THE CTD, expressed as a test. Pre-fix, the rung goods sat in `unique_goods`, so every
        // spend was "healed" back -- this test fails on that code (ledger_count is 0 there, and
        // the spent good resurrects).
        let tiers = flask_tiers();
        let items = vec![
            progressive_mixed(0, "Progressive Flask Upgrade", &tiers, 2919),
            progressive_mixed(1, "Progressive Flask Upgrade", &tiers, 2919),
        ];
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", items, vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert_eq!(
            g.ledger_count(10010),
            1,
            "rung 0 granted exactly once, via the ledger"
        );
        assert_eq!(
            g.ledger_count(10020),
            1,
            "rung 1 granted exactly once, via the ledger"
        );
        assert!(g.get_flag(71001) && g.get_flag(71002), "rung flags set");

        // The player spends both at a grace. (Under the fix the goods are never presence-tracked;
        // under the bug they were unique_goods, so dropping models the upgrade spend.)
        g.drop_good(10010);
        g.drop_good(10020);
        for _ in 0..3 {
            r.mark_dirty();
            r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
            assert!(
                !g.has_good(10010) && !g.has_good(10020),
                "a SPENT consumed rung good must STAY spent -- re-granting it is the flask-CTD loop"
            );
        }
        assert_eq!(g.ledger_count(10010), 1, "no re-grant after the spend");
        assert_eq!(g.ledger_count(10020), 1, "no re-grant after the spend");
    }

    #[test]
    fn consumed_tier_not_regranted_on_reconnect_or_resnapshot() {
        let tiers = flask_tiers();
        let items = vec![
            progressive_mixed(0, "Progressive Flask Upgrade", &tiers, 2919),
            progressive_mixed(1, "Progressive Flask Upgrade", &tiers, 2919),
        ];
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", items.clone(), vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert_eq!(g.ledger_count(10010), 1);
        assert_eq!(g.ledger_count(10020), 1);

        // Same-seed re-snapshot (a reconnect replaying the stream): the watermark is kept, so
        // nothing re-grants.
        r.set_inputs(inputs("A", items.clone(), vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert_eq!(
            g.ledger_count(10010),
            1,
            "re-snapshot must not re-grant a consumed rung"
        );
        assert_eq!(g.ledger_count(10020), 1);

        // Full reload: a NEW reconciler resumed from the persisted watermark re-grants none.
        let wm = r.applied_watermark();
        let mut r2 = Reconciler::from_persisted(inputs("A", items, vec![]), wm);
        r2.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert_eq!(
            g.ledger_count(10010),
            1,
            "reload must not re-grant a consumed rung"
        );
        assert_eq!(g.ledger_count(10020), 1);
    }

    #[test]
    fn owned_tier_still_self_heals_after_loss() {
        // Regression guard for the stone bell bearings: a ladder WITHOUT `consumed` keeps
        // today's `unique_goods` semantics exactly -- a bearing lost to a save-scum comes back.
        let tiers = bell_tiers();
        let items = vec![
            progressive(0, "progressive_stone_bell", &tiers, 2919),
            progressive(1, "progressive_stone_bell", &tiers, 2919),
        ];
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", items, vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert!(g.has_good(8101) && g.has_good(8102));

        g.drop_good(8101);
        r.mark_dirty();
        r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert!(
            g.has_good(8101),
            "an OWNED tier good must self-heal when lost (bell bearings)"
        );
        assert!(
            g.ledger_log.is_empty(),
            "owned rungs never touch the ledger"
        );
    }

    #[test]
    fn mixed_ladder_routes_each_rung_by_its_own_consumed_flag() {
        // rung 0 OWNED bell bearing, rung 1 CONSUMED golden seed, copy 2 overflows.
        let tiers: Vec<(&[GoodsId], &[FlagId], bool)> = vec![
            (&[8101][..], &[70001][..], false),
            (&[10010][..], &[71001][..], true),
        ];
        let items = vec![
            progressive_mixed(0, "Progressive Mixed", &tiers, 2919),
            progressive_mixed(1, "Progressive Mixed", &tiers, 2919),
            progressive_mixed(2, "Progressive Mixed", &tiers, 2919),
        ];
        let d = DesiredState::build(&inputs("A", items.clone(), vec![]));
        assert_eq!(
            d.unique_goods.len(),
            1,
            "only the OWNED rung is a unique good"
        );
        assert!(d.unique_goods.contains_key(&8101));
        assert_eq!(
            d.ledgered
                .iter()
                .map(|l| (l.index, l.full_id))
                .collect::<Vec<_>>(),
            vec![(1, 10010), (2, 2919)],
            "consumed rung + overflow both ledgered at their own stream indices"
        );

        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", items.clone(), vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert!(g.has_good(8101));
        assert_eq!(g.ledger_count(10010), 1);
        assert_eq!(
            g.ledger_count(2919),
            1,
            "overflow unchanged alongside consumed rungs"
        );

        // Per-rung semantics: the bearing self-heals; the spent seed stays spent.
        g.drop_good(8101);
        g.drop_good(10010); // no-op under the fix (never presence-tracked); the BUG resurrected it
        r.mark_dirty();
        r.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert!(g.has_good(8101), "owned rung self-heals");
        assert!(!g.has_good(10010), "consumed rung stays spent");
        assert_eq!(g.ledger_count(10010), 1);

        // Reload from the persisted watermark: neither the seed nor the overflow re-grants.
        let wm = r.applied_watermark();
        let mut r2 = Reconciler::from_persisted(inputs("A", items, vec![]), wm);
        r2.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert_eq!(g.ledger_count(10010), 1);
        assert_eq!(g.ledger_count(2919), 1);
    }

    #[test]
    fn consumed_ladder_desired_is_order_independent() {
        // Same invariant the owned ladder pins: desired is a pure function of the received
        // multiset, so consumed rungs land at the same (index, full_id) pairs in any order.
        let tiers = flask_tiers();
        let a = DesiredState::build(&inputs(
            "A",
            vec![
                progressive_mixed(0, "Progressive Flask Upgrade", &tiers, 2919),
                progressive_mixed(1, "Progressive Flask Upgrade", &tiers, 2919),
                progressive_mixed(2, "Progressive Flask Upgrade", &tiers, 2919),
            ],
            vec![],
        ));
        let b = DesiredState::build(&inputs(
            "A",
            vec![
                progressive_mixed(2, "Progressive Flask Upgrade", &tiers, 2919),
                progressive_mixed(0, "Progressive Flask Upgrade", &tiers, 2919),
                progressive_mixed(1, "Progressive Flask Upgrade", &tiers, 2919),
            ],
            vec![],
        ));
        assert_eq!(a, b, "consumed-ladder desired state is order-independent");
        assert_eq!(
            a.ledgered
                .iter()
                .map(|l| (l.index, l.full_id))
                .collect::<Vec<_>>(),
            vec![(0, 10010), (1, 10020), (2, 2919)]
        );
    }

    #[test]
    fn key_item_and_goal_flag_reach_desired() {
        let d = DesiredState::build(&inputs(
            "A",
            vec![
                key_item(0, "Rold Medallion", 9000, &[400001]),
                goal_flag(1, "Goal", 9600),
            ],
            vec![],
        ));
        assert!(d.unique_goods.contains_key(&9000));
        assert_eq!(d.unique_goods[&9000].companion_flags, vec![400001]);
        assert_eq!(
            d.flags.get(&400001),
            Some(&true),
            "obtained flag desired set"
        );
        assert_eq!(d.flags.get(&9600), Some(&true), "goal flag desired set");
        assert!(
            !d.owned_flags.contains(&400001),
            "obtained flag not owned (never cleared)"
        );
    }

    // ---- Gap 1: slot-data BULK grants (start graces / start items / reveal_all_maps / goal) ----

    #[test]
    fn reveal_all_maps_on_desires_every_map_flag_off_desires_only_the_unconditional_view_unlock() {
        let on = DesiredState::build(&bulk_inputs(SlotData {
            reveal_all_maps: true,
            map_reveal_flags: vec![62010, 62011, 62012],
            always_map_flags: vec![82001],
            ..Default::default()
        }));
        for f in [62010u32, 62011, 62012, 82001] {
            assert_eq!(
                on.flags.get(&f),
                Some(&true),
                "map flag {f} desired when reveal_all_maps on"
            );
            assert!(
                !on.owned_flags.contains(&f),
                "map flags self-heal but are never owned/cleared"
            );
        }

        let off = DesiredState::build(&bulk_inputs(SlotData {
            reveal_all_maps: false,
            map_reveal_flags: vec![62010, 62011, 62012],
            always_map_flags: vec![82001],
            ..Default::default()
        }));
        assert_eq!(
            off.flags.get(&82001),
            Some(&true),
            "the unconditional view-unlock is still desired"
        );
        for f in [62010u32, 62011, 62012] {
            assert!(
                off.flags.get(&f).is_none(),
                "reveal_all_maps OFF: world-map flag {f} not desired"
            );
        }
    }

    #[test]
    fn start_graces_are_desired_set_and_self_heal_only() {
        let d = DesiredState::build(&bulk_inputs(SlotData {
            start_graces: vec![76900, 76901],
            ..Default::default()
        }));
        assert_eq!(d.flags.get(&76900), Some(&true));
        assert_eq!(d.flags.get(&76901), Some(&true));
        assert!(
            !d.owned_flags.contains(&76900),
            "start graces are set-only, never cleared"
        );
    }

    #[test]
    fn goal_flag_is_desired_only_when_the_goal_is_met() {
        let unmet = DesiredState::build(&bulk_inputs(SlotData {
            goal_flag: Some(9990),
            goal_met: false,
            ..Default::default()
        }));
        assert!(
            unmet.flags.get(&9990).is_none(),
            "goal flag not desired until the goal is met"
        );

        let met = DesiredState::build(&bulk_inputs(SlotData {
            goal_flag: Some(9990),
            goal_met: true,
            ..Default::default()
        }));
        assert_eq!(
            met.flags.get(&9990),
            Some(&true),
            "goal flag desired once the goal is met"
        );
        assert!(
            !met.owned_flags.contains(&9990),
            "goal flag is report-side, never cleared"
        );
    }

    #[test]
    fn start_items_ledger_grants_once_across_a_reload() {
        let sd = SlotData {
            start_items: vec![
                StartItem {
                    full_id: 130,
                    qty: 1,
                }, // Torrent-like
                StartItem {
                    full_id: 1001,
                    qty: 3,
                }, // Flask
            ],
            ..Default::default()
        };
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(bulk_inputs(sd.clone()));
        assert!(
            r.applied_watermark() <= START_ITEM_INDEX_BASE,
            "a fresh save's watermark sits at/below the negative start-item band floor"
        );

        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(130),
            1,
            "the start item Torrent grants exactly once"
        );
        assert_eq!(
            g.ledger_count(1001),
            1,
            "the start item Flask grants exactly once"
        );
        let wm = r.applied_watermark();
        assert!(
            wm > START_ITEM_INDEX_BASE + 1,
            "watermark advanced past the whole start-item band"
        );

        // RELOAD from the persisted watermark: no start item re-grants (start-item double-grant fix).
        let mut r2 = Reconciler::from_persisted(bulk_inputs(sd), wm);
        r2.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(130),
            1,
            "no second Torrent grant after reload"
        );
        assert_eq!(
            g.ledger_count(1001),
            1,
            "no second Flask grant after reload"
        );
    }

    #[test]
    fn start_items_are_ledgered_never_presence_diffed() {
        // Start items (goods AND non-goods) ride the negative-band LEDGER, never `unique_goods`.
        // Presence-diffing goods start items re-granted DEPLETABLE ones (flasks/pots) on empty.
        let whistle = crate::progressive::GOODS_FULLID | 130; // goods-category
        let longsword = 1030000; // weapon-category FullID (nibble 0)
        let d = DesiredState::build(&bulk_inputs(SlotData {
            start_items: vec![
                StartItem {
                    full_id: whistle,
                    qty: 1,
                },
                StartItem {
                    full_id: longsword,
                    qty: 1,
                },
            ],
            ..Default::default()
        }));
        assert!(
            d.unique_goods.is_empty(),
            "no start item is ever a presence-diffed unique good"
        );
        for id in [whistle, longsword] {
            assert!(
                d.ledgered.iter().any(|l| l.full_id == id && l.index < 0),
                "start item {id} is ledgered in the negative band"
            );
        }
    }

    #[test]
    fn depletable_goods_start_item_is_not_re_granted_when_it_empties() {
        // FLASK BUG (2026-07-09): presence-diffing goods start items re-granted a Crimson/Cerulean flask
        // every time its charge count (the good's inventory quantity) reached 0 via DRINKING or grace
        // REALLOCATION (`has_good` reads false at 0). The ledger owes an INDEX, not a presence, so once
        // granted the flask is never re-owed — even after it leaves the inventory-presence view.
        let crimson = crate::progressive::GOODS_FULLID | 1001;
        let sd = SlotData {
            start_items: vec![StartItem {
                full_id: crimson,
                qty: 1,
            }],
            ..Default::default()
        };
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(bulk_inputs(sd));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(g.ledger_count(crimson), 1, "the flask grants once at start");

        // Empty it (drink / reallocate to 0). Under presence-diff this re-granted; under the ledger the
        // advanced watermark leaves the flask index behind the frontier, so nothing re-owes.
        g.drop_good(crimson);
        r.mark_dirty();
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(crimson),
            1,
            "an emptied flask is NOT re-granted"
        );
    }

    #[test]
    fn stacked_goods_start_items_grant_the_full_quantity() {
        // The loadout ships stacks as REPEATED FullIDs (10x Cracked Pot, 4x Ritual Pot) — each copy is
        // its own negative-band ledger entry, so all 10/4 grant (presence-diff collapsed them to one).
        // None re-grant on reload.
        let cracked_pot = crate::progressive::GOODS_FULLID | 9500;
        let ritual_pot = crate::progressive::GOODS_FULLID | 9501;
        let mut start_items: Vec<StartItem> = Vec::new();
        start_items.extend((0..10).map(|_| StartItem {
            full_id: cracked_pot,
            qty: 1,
        }));
        start_items.extend((0..4).map(|_| StartItem {
            full_id: ritual_pot,
            qty: 1,
        }));
        let sd = SlotData {
            start_items,
            ..Default::default()
        };

        let d = DesiredState::build(&bulk_inputs(sd.clone()));
        assert_eq!(
            d.ledgered
                .iter()
                .filter(|l| l.full_id == cracked_pot)
                .count(),
            10
        );
        assert_eq!(
            d.ledgered
                .iter()
                .filter(|l| l.full_id == ritual_pot)
                .count(),
            4
        );
        assert!(d.unique_goods.is_empty());

        let mut g = MockGame::stable();
        let mut r = Reconciler::new(bulk_inputs(sd.clone()));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 32);
        assert_eq!(
            g.ledger_count(cracked_pot),
            10,
            "all 10 Cracked Pots granted"
        );
        assert_eq!(g.ledger_count(ritual_pot), 4, "all 4 Ritual Pots granted");
        let wm = r.applied_watermark();
        let mut r2 = Reconciler::from_persisted(bulk_inputs(sd), wm);
        r2.run_to_fixpoint(&mut g, TickBudget::default(), 32);
        assert_eq!(g.ledger_count(cracked_pot), 10, "no re-grant after reload");
        assert_eq!(g.ledger_count(ritual_pot), 4, "no re-grant after reload");
    }

    #[test]
    fn seeded_grants_start_items_despite_a_stale_slot_watermark() {
        // The stranding fix lives in SEEDING now, not presence-diff: a fresh character (received_through
        // 0) with a stale positive slot watermark distrusts it and re-owes the negative start-item band.
        let whistle = crate::progressive::GOODS_FULLID | 130;
        let sd = SlotData {
            start_items: vec![StartItem {
                full_id: whistle,
                qty: 1,
            }],
            ..Default::default()
        };
        let mut g = MockGame::stable();
        let mut r = Reconciler::seeded(bulk_inputs(sd), Some(5), 0);
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(whistle),
            1,
            "fresh char re-owes + grants the whistle despite persisted=5"
        );
    }

    #[test]
    fn seed_change_re_owes_the_new_seeds_start_items() {
        // A genuine seed change re-owes the NEW seed's start items: the watermark resets to the new
        // desired's band floor, not a blind 0 that would strand the negative-index start items.
        let sd = SlotData {
            start_items: vec![StartItem {
                full_id: 130,
                qty: 1,
            }],
            ..Default::default()
        };
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(bulk_inputs(sd.clone()));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(g.ledger_count(130), 1);

        r.set_inputs(DesiredInputs {
            seed: "B".into(),
            save: SaveIdentity("slot0".into()),
            received: vec![],
            slot_data: sd,
        });
        assert!(
            r.applied_watermark() <= START_ITEM_INDEX_BASE,
            "a genuine seed change resets the watermark to the band floor"
        );
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(130),
            2,
            "seed B re-grants its own start item"
        );
    }

    #[test]
    fn from_persisted_watermark_skips_the_already_granted_stream() {
        // RESUME path: a save with a persisted reconcile.json watermark rebuilds via `from_persisted`
        // and must NOT re-grant anything at/below that watermark -- only the still-owed tail. (This is
        // the monotonic-frontier guarantee. `init` reaches here through `Reconciler::seeded`, which
        // trusts a persisted watermark only when it is `<= received_through` -- see the seeded_* tests
        // below for the cross-check policy.)
        let sd = SlotData {
            start_items: vec![StartItem {
                full_id: 130,
                qty: 1,
            }], // negative-band start item
            ..Default::default()
        };
        let inputs = DesiredInputs {
            seed: "A".into(),
            save: SaveIdentity("slot0".into()),
            received: vec![
                consumable(0, "Flask", 1001, 1),
                consumable(1, "Rune", 1002, 1),
                consumable(2, "Stone", 1003, 1),
            ],
            slot_data: sd,
        };
        let mut g = MockGame::stable();
        // received_through == 2: the old path granted indices 0 and 1 (plus the start item). Seed there.
        let mut r = Reconciler::from_persisted(inputs, 2);
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(1001),
            0,
            "index 0 already granted by the old path -> not re-granted"
        );
        assert_eq!(
            g.ledger_count(1002),
            0,
            "index 1 already granted by the old path -> not re-granted"
        );
        assert_eq!(
            g.ledger_count(1003),
            1,
            "index 2 is the still-owed tail -> granted exactly once"
        );
        assert_eq!(
            g.ledger_count(130),
            0,
            "start item is behind the seeded watermark -> not re-granted"
        );
    }

    // ---- session-init watermark seeding (Reconciler::seeded) ----------------------------

    /// The received stream + goods start item every seeded_* test below shares: three consumables at
    /// indices 0..=2 (what a picked-up own-world weapon classifies to) plus the whistle loadout.
    fn seeded_inputs() -> DesiredInputs {
        let whistle = crate::progressive::GOODS_FULLID | 130;
        DesiredInputs {
            seed: "A".into(),
            save: SaveIdentity("slot0".into()),
            received: vec![
                consumable(0, "Dragon Greatclaw", 3011, 1),
                consumable(1, "Marika's Hammer", 3012, 1),
                consumable(2, "Ash of War: Seppuku", 3013, 1),
            ],
            slot_data: SlotData {
                start_items: vec![StartItem {
                    full_id: whistle,
                    qty: 1,
                }],
                ..Default::default()
            },
        }
    }

    #[test]
    fn seeded_fresh_character_grants_the_stream_despite_a_stale_slot_watermark() {
        // THE REGRESSION (er-reconciler-received-grant-regression): reconcile.json is keyed by SLOT
        // NAME only, so a FRESH character (received_through = 0) inherits the previous character's
        // persisted POSITIVE watermark. Trusting it filtered every received consumable at index
        // 0..N < stale_wm out of the diff -- vanilla drop suppressed, item never delivered. `seeded`
        // must DISTRUST a persisted watermark above received_through and grant the full stream.
        let mut g = MockGame::stable();
        let mut r = Reconciler::seeded(seeded_inputs(), Some(5), 0); // stale wm=5, fresh character
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(3011),
            1,
            "index 0 grants despite the stale positive watermark"
        );
        assert_eq!(g.ledger_count(3012), 1, "index 1 grants");
        assert_eq!(g.ledger_count(3013), 1, "index 2 grants");
        assert_eq!(
            g.ledger_count(crate::progressive::GOODS_FULLID | 130),
            1,
            "the ledgered goods start item is re-owed from the floor and grants too"
        );
        assert_eq!(
            r.applied_watermark(),
            3,
            "the frontier ends past the granted stream"
        );
    }

    #[test]
    fn seeded_genuine_resume_still_skips_the_already_granted_prefix() {
        // A REAL resume: this character's own persisted watermark (== received_through) must still be
        // trusted, so already-granted consumables are NOT re-granted -- only the still-owed tail.
        let mut g = MockGame::stable();
        g.goods.insert(crate::progressive::GOODS_FULLID | 130); // whistle already in inventory
        let mut r = Reconciler::seeded(seeded_inputs(), Some(2), 2); // granted 0..=1 last session
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(3011),
            0,
            "index 0 already granted -> skipped"
        );
        assert_eq!(
            g.ledger_count(3012),
            0,
            "index 1 already granted -> skipped"
        );
        assert_eq!(
            g.ledger_count(3013),
            1,
            "index 2 is the owed tail -> granted once"
        );
        assert_eq!(g.ledger_count(crate::progressive::GOODS_FULLID | 130), 0);
    }

    #[test]
    fn seeded_trusts_a_persisted_watermark_behind_received_through() {
        // Under the full cutover the OLD receive path advances received_through WITHOUT granting
        // (placement is the reconciler's job), so received_through can run AHEAD of what was actually
        // placed (e.g. the reconciler sat unstable all session). The persisted watermark is the
        // actually-granted frontier: when it is BEHIND received_through it must win, re-owing the gap.
        let mut g = MockGame::stable();
        let mut r = Reconciler::seeded(seeded_inputs(), Some(1), 3); // placed 0; saw 0..=2
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(3011),
            0,
            "index 0 was actually placed -> skipped"
        );
        assert_eq!(
            g.ledger_count(3012),
            1,
            "index 1 was seen but never placed -> re-owed"
        );
        assert_eq!(
            g.ledger_count(3013),
            1,
            "index 2 was seen but never placed -> re-owed"
        );
    }

    #[test]
    fn seeded_first_cutover_on_an_existing_save_owes_only_the_tail() {
        // No reconcile.json yet, but the OLD path already granted the prefix (received_through = 2)
        // plus the start items: seed THERE so the deep-save consumable stream is NOT re-granted, and
        // a NON-goods start item (negative band, granted via the old start_items_granted latch) stays
        // behind the frontier.
        let longsword = 1030000; // weapon-category FullID (nibble 0) -> negative-band ledgered
        let mut inputs = seeded_inputs();
        inputs.slot_data.start_items.push(StartItem {
            full_id: longsword,
            qty: 1,
        });
        let mut g = MockGame::stable();
        g.goods.insert(crate::progressive::GOODS_FULLID | 130); // old path granted the whistle too
        let mut r = Reconciler::seeded(inputs, None, 2);
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(3011),
            0,
            "old-path-granted prefix is not re-granted"
        );
        assert_eq!(g.ledger_count(3012), 0);
        assert_eq!(g.ledger_count(3013), 1, "the un-granted tail is owed");
        assert_eq!(
            g.ledger_count(longsword),
            0,
            "non-goods start item stays behind the seed"
        );
    }

    #[test]
    fn seeded_fresh_save_owes_everything_from_the_floor() {
        // Nothing persisted, nothing received before: identical to `Reconciler::new` -- the ledger
        // floor owes the negative-band start items AND the whole received stream.
        let longsword = 1030000;
        let mut inputs = seeded_inputs();
        inputs.slot_data.start_items.push(StartItem {
            full_id: longsword,
            qty: 1,
        });
        let mut g = MockGame::stable();
        let mut r = Reconciler::seeded(inputs, None, 0);
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(longsword),
            1,
            "fresh save: negative-band start item grants"
        );
        assert_eq!(g.ledger_count(3011), 1);
        assert_eq!(g.ledger_count(3012), 1);
        assert_eq!(g.ledger_count(3013), 1);
        assert_eq!(
            g.ledger_count(crate::progressive::GOODS_FULLID | 130),
            1,
            "the goods start item grants from the floor"
        );
    }

    #[test]
    fn bulk_grants_reach_desired_end_to_end_and_only_start_items_are_goods() {
        // One of every bulk class at once: graces + map flags + goal flag SET; start item LEDGERED.
        let sd = SlotData {
            start_graces: vec![76900],
            always_map_flags: vec![82001],
            reveal_all_maps: true,
            map_reveal_flags: vec![62010],
            start_items: vec![StartItem {
                full_id: 130,
                qty: 1,
            }],
            goal_flag: Some(9700),
            goal_met: true,
            ..Default::default()
        };
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(bulk_inputs(sd));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        for f in [76900u32, 82001, 62010, 9700] {
            assert!(g.get_flag(f), "bulk flag {f} set");
        }
        assert!(
            g.goods.is_empty(),
            "no bulk grant becomes a unique good (start items are ledgered)"
        );
        assert_eq!(g.ledger_count(130), 1, "the start item lands exactly once");
    }

    // ---- Gap 2: shop native-sold consumable echo-dedup ----------------------------------

    #[test]
    fn shop_native_sold_echo_advances_watermark_without_double_granting() {
        // The real buy delivers the item (index 0, echo_skip=false -> a grant); the SAME location
        // then echoes it (index 1, echo_skip=true). The echo must NOT grant, but its index must be
        // advanced-past so a reload never reconsiders it.
        let items = vec![
            shop_consumable(0, "Shop Golden Rune", 5001, 1, false), // real buy
            shop_consumable(1, "Shop Golden Rune", 5001, 1, true),  // native-sold echo
        ];
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", items.clone(), vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(
            g.ledger_count(5001),
            1,
            "granted exactly once (the echo does not double-grant)"
        );
        assert_eq!(
            r.applied_watermark(),
            2,
            "the watermark advanced PAST both the buy and its echo"
        );

        // Reload: neither grants again.
        let mut r2 = Reconciler::from_persisted(inputs("A", items, vec![]), r.applied_watermark());
        r2.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(g.ledger_count(5001), 1, "no re-grant after reload");
    }

    #[test]
    fn a_pure_native_sold_echo_grants_nothing_but_still_advances_the_watermark() {
        // Only the echo is seen (the native sale already delivered the item): the reconciler grants
        // nothing yet still advances the watermark so the frontier stays contiguous.
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs(
            "A",
            vec![shop_consumable(0, "Sold", 5002, 1, true)],
            vec![],
        ));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(g.ledger_count(5002), 0, "a native-sold echo grants nothing");
        assert_eq!(
            r.applied_watermark(),
            1,
            "the watermark still advances past the echo index"
        );
    }
}
