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
    pub flags: Vec<FlagId>,
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
    Consumable { full_id: GoodsId, qty: i32 },
    /// A PROGRESSIVE item: the Nth received copy of this NAME lands tier N (that tier's unique goods
    /// + observable flags); every copy past the last tier yields exactly ONE overflow consumable
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

/// The slot-data-derived, per-seed configuration the desired-state builder needs beyond the item
/// stream. Kept tiny on purpose; the client fills it from parsed slot_data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotData {
    /// Region open-flags that start SEALED: desired `false`, and OWNED so the reconciler may clear
    /// (re-seal) them. A received `RegionFlags` for the same flag overrides it to `true` (opened).
    pub seal_flags: Vec<FlagId>,
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
                ItemSemantics::Consumable { full_id, qty } => {
                    d.ledgered.push(LedgeredGrant {
                        index: it.index,
                        full_id: *full_id,
                        qty: *qty,
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
        //    0..min(count, tiers.len()) contribute their unique goods + observable flags, and every
        //    copy past the last tier contributes ONE ledgered overflow consumable (keyed by its own
        //    index, so a reconnect re-grants none of them). Tier flags/goods self-heal set-only; they
        //    are never OWNED (never cleared), like key-item obtained flags.
        let mut prog: BTreeMap<&str, (&Vec<ProgTier>, GoodsId, Vec<ItemIndex>)> = BTreeMap::new();
        for it in &inputs.received {
            if let ItemSemantics::Progressive { tiers, overflow_full_id } = &it.semantics {
                let e = prog
                    .entry(it.name.as_str())
                    .or_insert((tiers, *overflow_full_id, Vec::new()));
                e.2.push(it.index);
            }
        }
        for (_name, (tiers, overflow, mut idxs)) in prog {
            idxs.sort_unstable();
            for (pos, idx) in idxs.iter().enumerate() {
                if let Some(t) = tiers.get(pos) {
                    for &g in &t.goods {
                        d.unique_goods.entry(g).or_default();
                    }
                    for &f in &t.flags {
                        d.flags.insert(f, true);
                    }
                } else {
                    d.ledgered.push(LedgeredGrant {
                        index: *idx,
                        full_id: overflow,
                        qty: 1,
                    });
                }
            }
        }

        d.ledgered.sort_by_key(|l| l.index);
        d
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
    for l in desired.ledgered.iter().filter(|l| l.index >= observed.applied_watermark) {
        out.push(Action::GrantLedgered {
            index: l.index,
            full_id: l.full_id,
            qty: l.qty,
        });
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
    pub const ALL: Self = Self { flags: true, goods: true, ledger: true };
    /// Own nothing (equivalent to dry-run for the apply path).
    pub const NONE: Self = Self { flags: false, goods: false, ledger: false };

    /// Is `action` in an enabled class?
    pub fn allows(&self, action: &Action) -> bool {
        match action {
            Action::SetFlag(_) | Action::ClearFlag(_) => self.flags,
            Action::GrantUnique(..) => self.goods,
            Action::GrantLedgered { .. } => self.ledger,
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
}

impl Default for TickBudget {
    fn default() -> Self {
        TickBudget {
            goods: 4,
            flags: 32,
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
}

impl Reconciler {
    /// Build from fresh inputs (watermark starts at 0; use [`from_persisted`] to resume a save).
    ///
    /// [`from_persisted`]: Reconciler::from_persisted
    pub fn new(inputs: DesiredInputs) -> Self {
        Self::from_persisted(inputs, 0)
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
        if crate::seed_change::is_seed_change(Some(&self.session_seed), &inputs.seed) {
            self.applied_watermark = 0; // brand-new seed: nothing applied for it yet
        }
        self.session_seed = inputs.seed.clone();
        self.desired = DesiredState::build(&inputs);
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
        if !io.stability().stable() {
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

        let mut applied = Vec::new();
        let mut flags_used = 0usize;
        let mut goods_used = 0usize;
        let mut deferred = false;

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
                    if goods_used >= budget.goods {
                        deferred = true;
                        continue;
                    }
                    if io.grant_good(*g, comp) {
                        goods_used += 1;
                        applied.push(a.clone());
                    } else {
                        deferred = true; // inventory not ready -> retry next tick
                    }
                }
                Action::GrantLedgered { .. } => {} // handled in pass 2 (contiguity matters)
            }
        }

        // Pass 2: the ledger, in index order, advancing the watermark ONLY across a contiguous run
        // of successful grants. A budget stop or a not-ready inventory holds the watermark so the
        // tail replays next tick (mirrors receive.rs's rollback protocol).
        for a in &actions {
            if let Action::GrantLedgered {
                index,
                full_id,
                qty,
            } = a
            {
                if goods_used >= budget.goods {
                    deferred = true;
                    break;
                }
                if io.grant_ledgered(*full_id, *qty) {
                    goods_used += 1;
                    self.applied_watermark = index + 1;
                    applied.push(a.clone());
                } else {
                    deferred = true;
                    break;
                }
            }
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
            },
            ..MockGame::default()
        }
    }

    pub fn set_stable(&mut self, v: bool) {
        if v {
            self.stability = WorldStability {
                in_game: true,
                player_valid: true,
                dwell_ms: WorldStability::SETTLE_MS,
                real_pickup_seen: true,
            };
        } else {
            self.stability = WorldStability {
                in_game: true,
                player_valid: true,
                dwell_ms: 0,
                real_pickup_seen: false,
            };
        }
    }

    /// How many times a consumable full_id was granted (>1 == double-grant).
    pub fn ledger_count(&self, full_id: GoodsId) -> usize {
        self.ledger_log.iter().filter(|&&(id, _)| id == full_id).count()
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
            semantics: ItemSemantics::Consumable { full_id, qty },
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
    fn progressive(index: ItemIndex, name: &str, tiers: &[(&[GoodsId], &[FlagId])], overflow: GoodsId) -> ReceivedItem {
        ReceivedItem {
            index,
            name: name.into(),
            semantics: ItemSemantics::Progressive {
                tiers: tiers
                    .iter()
                    .map(|(g, f)| ProgTier { goods: g.to_vec(), flags: f.to_vec() })
                    .collect(),
                overflow_full_id: overflow,
            },
        }
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
            slot_data: SlotData { seal_flags },
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
        assert!(d.unique_goods.is_empty(), "map pieces must produce no unique goods");
        assert_eq!(d.flags.get(&82001), Some(&true), "the 82001 view-unlock must be desired");
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
        let d = DesiredState::build(&inputs("A", vec![great_rune(0, "Godrick's Great Rune", 191, 6901)], vec![]));
        assert!(d.unique_goods.contains_key(&191));
        assert_eq!(d.unique_goods[&191].companion_flags, vec![6901]);
        assert_eq!(d.flags.get(&6901), Some(&true), "restored flag is an independent observable flag");
    }

    #[test]
    fn region_seal_default_overridden_by_received_lock() {
        let sealed = DesiredState::build(&inputs("A", vec![], vec![76980]));
        assert_eq!(sealed.flags.get(&76980), Some(&false));
        assert!(sealed.owned_flags.contains(&76980), "seal flags are owned (clearable)");

        let opened = DesiredState::build(&inputs("A", vec![region(0, "Caelid Lock", &[76980])], vec![76980]));
        assert_eq!(opened.flags.get(&76980), Some(&true), "the Lock opens the region");
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
        assert!(g.ledger_log.is_empty(), "no grant may land while unstable (no clobber race)");
        assert_eq!(r.applied_watermark(), 0, "watermark must not advance on an unstable tick");

        g.set_stable(true);
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(g.ledger_count(2008), 1, "the Torch grants exactly once once stable");
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
        assert_eq!(g.ledger_count(1001), 1, "no second grant after reload (flask double-grant fix)");
        assert_eq!(g.ledger_count(1002), 1);
    }

    // ---- great-rune double-grant (unique-good diff) -------------------------------------

    #[test]
    fn great_rune_grants_once_then_diff_is_empty() {
        // gf-great-rune-double-grant: once the good is present the diff for it is empty, so a second
        // convergence pass (reconnect) re-grants NOTHING.
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", vec![great_rune(0, "Godrick's Great Rune", 191, 6901)], vec![]));

        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(g.has_good(191), "the rune good is granted once");
        assert!(g.get_flag(6901), "the restored companion flag is set");

        let obs = ObservedState {
            flags: [(6901u32, true)].into_iter().collect(),
            unique_goods: [191i32].into_iter().collect(),
            applied_watermark: 0,
        };
        assert!(diff(r.desired(), &obs).is_empty(), "present good -> empty diff (no double-grant)");
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

        let flags_only = ApplyClasses { flags: true, goods: false, ledger: false };
        let mut n = 0;
        loop {
            let out = r.tick_with_classes(&mut g, TickBudget::default(), flags_only);
            n += 1;
            if out.converged || n > 8 {
                break;
            }
        }
        assert!(g.get_flag(76971), "region flag applied under flags-only");
        assert!(!g.has_good(191), "the rune good is NOT granted while goods is disabled");
        assert_eq!(g.ledger_count(2008), 0, "no consumable granted while ledger is disabled");

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
        assert!(planned.iter().any(|a| matches!(a, Action::GrantUnique(191, _))));
        assert!(planned.iter().any(|a| matches!(a, Action::GrantLedgered { full_id: 2008, .. })));
        // Nothing was applied: no flags, no goods, no ledger entries, watermark untouched.
        assert!(g.flags.is_empty() && g.goods.is_empty() && g.ledger_log.is_empty());
        assert_eq!(r.applied_watermark(), 0);

        // An unstable game plans nothing.
        g.set_stable(false);
        assert!(r.dry_run_actions(&g).is_empty(), "no plan while the world is unstable");
    }

    // ---- bundle-lock grace self-heal ----------------------------------------------------

    #[test]
    fn grace_flag_self_heals_when_lost() {
        // er-bundle-lock-grace-reconcile-gap: a region grace bundle flag DESIRED set but reading
        // clear (lost after a save-load) must be re-set on the next stable tick — repeatedly.
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", vec![region(0, "Limgrave Lock", &[76971, 76972])], vec![]));

        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(g.get_flag(76971) && g.get_flag(76972), "grace bundle set");

        g.flags.insert(76971, false); // the game loses a bundle flag
        r.mark_dirty();
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(g.get_flag(76971), "the lost grace flag self-heals on the next reconcile");
    }

    // ---- reconnect-new-seed swap --------------------------------------------------------

    #[test]
    fn seed_change_resets_ledger_and_rebuilds_desired_without_panic() {
        // er-reconnect-newseed-panic: reconnecting to a DIFFERENT seed atomically swaps inputs,
        // rebuilds the per-seed desired state, and resets the ledger watermark so seed B's own
        // consumables grant fresh. No indexing, no stale table survives.
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inputs("A", vec![consumable(0, "Golden Rune", 5001, 1)], vec![]));
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(r.applied_watermark(), 1);

        r.set_inputs(inputs("B", vec![consumable(0, "Golden Rune", 5002, 1)], vec![]));
        assert_eq!(r.applied_watermark(), 0, "a genuine seed change resets the ledger watermark");
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert_eq!(g.ledger_count(5002), 1, "seed B's consumable grants fresh after the swap");
    }

    #[test]
    fn same_seed_reconnect_keeps_watermark() {
        let mut r = Reconciler::from_persisted(inputs("A", vec![consumable(0, "X", 42, 1)], vec![]), 1);
        r.set_inputs(inputs("A", vec![consumable(0, "X", 42, 1), consumable(1, "Y", 43, 1)], vec![]));
        assert_eq!(r.applied_watermark(), 1, "same-seed reconnect preserves the watermark");
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

        let budget = TickBudget { goods: 4, flags: 32 };
        let out = r.tick(&mut g, budget);
        assert_eq!(out.applied.len(), 4, "one tick applies at most `goods` ledger grants");
        assert!(!out.converged, "still more to do");

        let ticks = r.run_to_fixpoint(&mut g, budget, 50);
        assert_eq!(g.ledger_log.len(), 20, "all 20 consumables eventually land, each once");
        assert!(ticks <= 6, "20 items / 4 per tick drains in a handful of ticks, took {ticks}");
    }

    #[test]
    fn flag_holder_not_ready_defers_without_losing_the_flag() {
        let mut g = MockGame::stable();
        g.flag_ready = false; // holder not ready
        let mut r = Reconciler::new(inputs("A", vec![region(0, "L", &[76971])], vec![]));

        let out = r.tick(&mut g, TickBudget::default());
        assert!(!out.converged, "holder-not-ready must defer");
        assert!(!g.get_flag(76971), "nothing set while the holder is not ready");

        g.flag_ready = true;
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(g.get_flag(76971), "the deferred flag lands once the holder is ready");
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
        assert!(d.ledgered.is_empty(), "no overflow while copies <= tier count");
        assert!(!d.owned_flags.contains(&70001), "tier flags are set-only, never owned");
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
        assert!(g.has_good(8101) && g.has_good(8102), "both bell tiers granted");
        assert!(g.get_flag(70001) && g.get_flag(70002), "both tier flags set");
        assert_eq!(g.ledger_count(2919), 1, "exactly one overflow Lord's Rune");
        let wm = r.applied_watermark();

        let mut r2 = Reconciler::from_persisted(inputs("A", items, vec![]), wm);
        r2.run_to_fixpoint(&mut g, TickBudget::default(), 16);
        assert_eq!(g.ledger_count(2919), 1, "no second overflow grant after reload");
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
        assert_eq!(d.flags.get(&400001), Some(&true), "obtained flag desired set");
        assert_eq!(d.flags.get(&9600), Some(&true), "goal flag desired set");
        assert!(!d.owned_flags.contains(&400001), "obtained flag not owned (never cleared)");
    }
}
