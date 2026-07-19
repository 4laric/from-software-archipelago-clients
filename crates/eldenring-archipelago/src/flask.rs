//! flask — HISTORY-AGNOSTIC reconciled flask CHARGE state (charges only).
//!
//! A reconcile state class in the spirit of `reconcile.rs`, but for a value that lives ENTIRELY in the
//! character's own data (not the AP ledger). Every reconcile tick, when in-world, we:
//!   1. count how many "Progressive Flask Upgrade" AP items have been received (`received_count`,
//!      passed in from `core.rs`'s received-item snapshot — AP replays the whole set on connect, so
//!      the count is stable across reconnect/save-load with NO ledger),
//!   2. take the DESIRED charge target from the slot_data `flaskLadder`
//!      ([`er_logic::flask_reconcile::desired`] rung, `.charges`),
//!   3. read the LIVE allocation (`max_hp_flask + max_fp_flask`),
//!   4. add the deficit up to the target ([`er_logic::flask_reconcile::charge_deficit`] — upward-only,
//!      clamped, idempotent). `max_hp_flask`/`max_fp_flask` are CLEANLY writable (Hexinton's "Flask
//!      Allocation" cheat writes exactly them, `GameDataMan+0x8 -> +0x101/+0x102`), and the game does
//!      NOT recompute them each frame, so a plain field write STICKS. We add the whole deficit to
//!      `max_hp_flask`; the player is free to reallocate at a grace (keeps the TOTAL constant, so this
//!      stays converged). Because it's a pure function of the received COUNT and only ever raises, a
//!      reconnect / save-load / reallocation just re-converges — no double-grant, no lowering.
//!
//! POTENCY is NOT handled here. It is delivered as granted Sacred Tears via `progressiveGrants` (the
//! player upgrades at a grace the vanilla way, which updates every flask mirror correctly). An earlier
//! build raised potency by an in-place flask item-id swap (`base + L*2`); that CTD'd on death — ER
//! mirrors the flask tier across the inventory entry, the equipped/quickslot reference, AND the global
//! GaItem, and death's refill crashed on the half-updated state (archipelago20260719.log). Computing
//! the potency axis at all also spammed a per-frame "SKIPPED" log once the swap was gated off. So the
//! client owns ONLY the charge axis; the ladder's `potency` field is ignored here (it's documentation
//! + the gen derives the tear schedule from it).

use std::sync::Mutex;

use eldenring::cs::GameDataMan;
use fromsoftware_shared::FromStatic;

use er_logic::flask_reconcile::{self, FlaskTarget};

/// The exact AP item name we count for the CHARGE reconcile. The same item ALSO rides
/// `progressiveGrants` (a consumed Sacred Tear per copy → the potency axis); that path is handled by
/// the reconciler ledger, independently of the charge count here. Non-overlapping: tears ≠ charges.
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

    // --- CHARGES-ONLY reconcile. Potency is NOT touched here: it is delivered as granted Sacred
    // Tears via progressiveGrants (consumed/ledgered), and the player upgrades potency at a grace the
    // vanilla way. The old in-place item-tier swap CTD'd on death -- ER mirrors the flask tier across
    // the inventory entry, the equipped/quickslot reference, AND the global GaItem, and death's flask
    // refill crashed on the half-updated state (archipelago20260719.log). So the client owns ONLY the
    // charge axis. (The ladder's `potency` field is documentation; this module ignores it -- computing
    // it was what spammed a per-frame "SKIPPED" line once the swap was gated off.)
    let cur_charges = pgd.max_hp_flask as u32 + pgd.max_fp_flask as u32;
    let add = flask_reconcile::charge_deficit(cur_charges, target);
    if add == 0 {
        return; // at/above target allocation -- idempotent no-op, NO per-frame log
    }
    // Bounded by MAX_CHARGES (14) upstream, so this never overflows u8; saturate defensively.
    let headroom = u8::MAX - pgd.max_hp_flask;
    let add = add.min(headroom as u32) as u8;
    if add == 0 {
        return;
    }
    pgd.max_hp_flask = pgd.max_hp_flask.saturating_add(add);
    log::info!(
        "flask: charges {} -> {} (max_hp_flask += {add}; ladder rung charges={})",
        cur_charges,
        cur_charges + add as u32,
        target.charges,
    );
}
