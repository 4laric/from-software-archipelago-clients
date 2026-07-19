//! flask — HISTORY-AGNOSTIC reconciled LEVELED flask state (charges + potency).
//!
//! This is a NEW reconcile state class in the spirit of `reconcile.rs`, but for a value that lives
//! ENTIRELY in the character's own data (not the AP ledger). Every reconcile tick, when in-world, we:
//!   1. count how many "Progressive Flask Upgrade" AP items have been received (`received_count`,
//!      passed in from `core.rs`'s received-item snapshot — AP replays the whole set on connect, so
//!      the count is stable across reconnect/save-load with NO ledger),
//!   2. compute the DESIRED `{charges, potency}` rung from the slot_data `flaskLadder`
//!      ([`er_logic::flask_reconcile::desired`]),
//!   3. read the LIVE flask off `PlayerGameData`,
//!   4. write the HIGHER of the two ([`er_logic::flask_reconcile::reconcile`] — upward-only, clamped,
//!      idempotent).
//!
//! Because desired is a pure function of the received COUNT and we only ever raise, this is
//! self-healing: a reconnect, a save-load, or a grace reallocation re-runs it and converges to the
//! same state without ever double-granting or lowering the flask.
//!
//! Two axes (see the `er_logic::flask_reconcile` module doc):
//!   * CHARGES (Golden Seeds): the allocation `max_hp_flask + max_fp_flask` on `PlayerGameData`.
//!     These fields are CLEANLY writable — Hexinton's "Flask Allocation" cheat writes exactly them
//!     (pointer path `GameDataMan+0x8 -> +0x101/+0x102`), and unlike `max_equip_load` the game does
//!     NOT recompute them every frame, so a plain field write STICKS. We add the whole charge deficit
//!     to `max_hp_flask` (the simplest sound split; the player is free to reallocate at a grace, which
//!     keeps the TOTAL constant so this reconcile stays converged).
//!   * POTENCY (Sacred Tears, 0..12): the TIER of the held flask ITEM. We raise it Hexinton-style by
//!     OVERWRITING the held flask inventory slot's item id in place to `base + level*2`.
//!
//! NOTE(windows-verify) — UNCERTAIN HALF. The CHARGE write (`max_hp_flask`/`max_fp_flask`) is
//! well-established (the Flask Allocation cheat). The POTENCY item-tier swap is NOT yet confirmed by a
//! live set->readback: it assumes (a) the held flask is a plain `Goods` inventory entry whose
//! `item_id` we can rewrite in place via `items_mut()`, and (b) potency really is keyed by which
//! `base + L*2` item id is held (the Hexinton "Set flask level" model). If in-game the potency is
//! instead driven by the derived `hp_estus_rate`/`hp_estus_additional` fields, this swap will not move
//! the heal amount and those fields become the source of truth. VERIFY on Windows: receive an upgrade,
//! confirm the held Crimson/Cerulean item id changed to the next tier AND the heal amount rose. The
//! charge axis can ship independently of this.

use std::sync::Mutex;

use eldenring::cs::{GameDataMan, ItemCategory, ItemId};
use fromsoftware_shared::FromStatic;

use er_logic::flask_reconcile::{
    self, classify_flask_item, flask_item_id, FlaskState, FlaskTarget,
};

/// The exact AP item name we count for the reconcile. The gen side no longer routes this through
/// `progressiveGrants`; this module is now its sole consumer.
pub const FLASK_UPGRADE_ITEM: &str = "Progressive Flask Upgrade";

/// The parsed `flaskLadder` from slot_data. `None`/empty => feature OFF (tick is a hard no-op).
/// `Mutex<Option<..>>` because `Vec::new()` is not const and this is a `static` (a bare
/// `HashSet::new()`/`Vec::new()` const-init would not compile on the Windows build).
static LADDER: Mutex<Option<Vec<FlaskTarget>>> = Mutex::new(None);

/// Configure from slot_data at connect (called from `core.rs`). Empty ladder disables the feature.
pub fn set_ladder(ladder: Vec<FlaskTarget>) {
    let on = !ladder.is_empty();
    *LADDER.lock().unwrap() = if on { Some(ladder) } else { None };
    if on {
        log::info!(
            "flask: enabled (flaskLadder reconcile; {} rungs)",
            LADDER.lock().unwrap().as_ref().map_or(0, |l| l.len())
        );
    }
}

/// Per-tick. When configured + in-world: reconcile the live flask up to the rung implied by
/// `received_count`. Idempotent — a converged flask does nothing. Gated on `in_world()` exactly like
/// `no_fall_damage`/`no_equip_load` (the player game data + inventory aren't settled at boot/menu).
pub fn tick(received_count: usize) {
    // MENU/BOOT GATE: GameDataMan / the inventory aren't stable before the world is live.
    if !crate::flags::in_world() {
        return;
    }

    // Desired rung from the received count. `None` => ladder absent/empty or count 0 => no-op.
    let target = {
        let guard = LADDER.lock().unwrap();
        let Some(ladder) = guard.as_ref() else {
            return;
        };
        match flask_reconcile::desired(ladder, received_count) {
            Some(t) => t,
            None => return,
        }
    };

    // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin / reconcile tick.
    let Ok(gdm) = (unsafe { GameDataMan::instance_mut() }) else {
        return;
    };
    let pgd = gdm.main_player_game_data.as_mut();

    // --- read the live flask -------------------------------------------------------------
    let cur_charges = pgd.max_hp_flask as u32 + pgd.max_fp_flask as u32;
    // Potency = the max tier among held flask items. `None` when the player holds no flask item this
    // tick (transient / inventory not populated) — we then skip the potency axis but still do charges.
    let cur_potency: Option<u32> = {
        let mut max_lvl: Option<u32> = None;
        for entry in pgd.equipment.equip_inventory_data.items_data.items() {
            if entry.item_id.category() != ItemCategory::Goods {
                continue;
            }
            if let Some((_, lvl)) = classify_flask_item(entry.item_id.param_id() as i32) {
                max_lvl = Some(max_lvl.map_or(lvl, |m| m.max(lvl)));
            }
        }
        max_lvl
    };

    let current = FlaskState {
        charges: cur_charges,
        potency: cur_potency.unwrap_or(0),
    };
    let actions = flask_reconcile::reconcile(current, target);
    if actions.is_noop() {
        return;
    }

    // --- CHARGES: add the whole deficit to max_hp_flask (documented simplest sound split) --------
    if actions.add_charges > 0 {
        // Bounded by MAX_CHARGES (14) upstream, so this never overflows u8; saturate defensively.
        let headroom = u8::MAX - pgd.max_hp_flask;
        let add = actions.add_charges.min(headroom as u32) as u8;
        pgd.max_hp_flask = pgd.max_hp_flask.saturating_add(add);
        log::info!(
            "flask: charges {} -> {} (max_hp_flask += {add}; target rung {{charges: {}, potency: {}}})",
            cur_charges,
            actions.target_charges,
            target.charges,
            target.potency,
        );
    }

    // --- POTENCY: raise the held flask items' tier in place (Hexinton "Set flask level") ---------
    // Only when we actually hold a flask item this tick (else there is nothing to swap). See the
    // module-level windows-verify note: this axis is the UNCONFIRMED half of the feature.
    if actions.raise_potency && cur_potency.is_some() {
        let mut swapped = 0usize;
        for entry in pgd.equipment.equip_inventory_data.items_data.items_mut() {
            if entry.item_id.category() != ItemCategory::Goods {
                continue;
            }
            let row = entry.item_id.param_id() as i32;
            if let Some((base, lvl)) = classify_flask_item(row) {
                if lvl < actions.target_potency {
                    let new_row = flask_item_id(base, actions.target_potency);
                    if let Ok(new_id) = ItemId::new(ItemCategory::Goods, new_row as u32) {
                        // In-place tier swap: keep quantity/slot, bump the item id (Hexinton model).
                        entry.item_id = new_id;
                        swapped += 1;
                    }
                }
            }
        }
        if swapped > 0 {
            log::info!(
                "flask: potency {} -> {} (swapped {swapped} held flask item(s) to base+{}*2)",
                cur_potency.unwrap_or(0),
                actions.target_potency,
                actions.target_potency,
            );
        }
    }
}
