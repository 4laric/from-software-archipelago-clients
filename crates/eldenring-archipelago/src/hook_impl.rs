//! The live-game side of the er-logic seam (SHARED-CONVERGENCE-PLAN.md): `EldenRingHook` maps every
//! `er_logic::hook::GameHook` verb 1:1 onto the existing game modules (`flags` / `detour` /
//! `deathlink` / `upgrades`), and `ReceiveDispatch` adapts `er_logic::receive::NetHook`'s
//! name-dispatch callbacks onto the keyitems fast path, region opening, and progressive routing.
//! No decision logic lives here — decisions are host-tested in `er-logic`; this file only forwards.

use er_logic::hook::GameHook;
use er_logic::progressive::ProgressiveState;
use er_logic::receive::NetHook;

/// Zero-sized `GameHook` impl forwarding 1:1 to the live game. Construct freely (`EldenRingHook`);
/// all state lives in the game process / module statics, not here.
pub struct EldenRingHook;

impl GameHook for EldenRingHook {
    fn get_event_flag(&self, flag: u32) -> bool {
        crate::flags::get_event_flag(flag)
    }
    fn set_event_flag(&mut self, flag: u32, on: bool) {
        crate::flags::set_event_flag(flag, on);
    }
    fn try_set_event_flag(&mut self, flag: u32, on: bool) -> bool {
        crate::flags::try_set_event_flag(flag, on)
    }
    fn in_world(&self) -> bool {
        crate::flags::in_world()
    }
    fn play_region_id(&self) -> Option<i32> {
        crate::flags::play_region_id()
    }
    fn grant_full_id(&mut self, full_id: i32, qty: i32) -> bool {
        crate::detour::grant_full_id(full_id, qty)
    }
    fn player_hp(&self) -> Option<i32> {
        crate::deathlink::read_local_hp()
    }
    /// Live-semantics override: PURE-RUNTIME kills are a direct HP write (`kill_local_player`,
    /// which pre-arms the DeathLink echo suppressor), with the reactor flag set best-effort for
    /// bake-compat setups — the trait default's flag-only kill is inert on a vanilla game.
    fn kill_player(&mut self) -> bool {
        let killed = crate::deathlink::kill_local_player();
        if killed {
            let _ = crate::flags::try_set_event_flag(er_logic::hook::DEATHLINK_KILL_FLAG, true);
        }
        killed
    }
    fn weapon_track_and_cap(&self, base: i32) -> Option<(i32, bool)> {
        crate::upgrades::weapon_track_and_cap(base)
    }
    fn highest_held_level(&self, somber: bool) -> Option<i32> {
        crate::upgrades::highest_held_level(somber)
    }
    fn scadutree_blessing(&self) -> Option<i32> {
        crate::upgrades::stored_blessing()
    }
    fn set_scadutree_blessing(&mut self, level: i32) {
        crate::upgrades::write_stored_blessing(level);
    }
}

/// Call-site `NetHook` adapter for the receive seam: borrows the `Core` fields the two name-keyed
/// dispatch callbacks need. Only constructed when `can_grant` holds (loaded world + live inventory
/// pointer), so its flag writes / tier grants aren't discarded at menu/load (SWEEP H3).
pub struct ReceiveDispatch<'a> {
    pub region: Option<&'a crate::region::RegionConfig>,
    pub progressive: &'a mut ProgressiveState,
    pub hook: &'a mut EldenRingHook,
    /// Region-lock names opened this pass, for the overlay's "Region unlocked" console lines.
    pub unlocked: Vec<String>,
}

impl NetHook for ReceiveDispatch<'_> {
    /// Idempotent per-name side effects, exactly the old core.rs step 4a: vanilla obtained flags
    /// (spirit bell / whetblades / Rold / ...) + region open/reveal/grace flags. Best-effort
    /// one-shots; the reconcile ticks (`tick_keyitem_flags` / `tick_reconcile_received_locks`)
    /// self-heal a write lost to a not-ready flag holder.
    fn on_item_received(&mut self, name: &str) {
        crate::keyitems::set_acquire_flags(name);
        if let Some(cfg) = self.region {
            // lockGrantItems rider (SPEC-region-spine-surgery.md SS3.5): physically grant this
            // lock's rider items (unpooled medallions) on its FIRST open. Checked BEFORE
            // open_on_received_name flips the open flag (the flag is the once-latch). Safe to
            // grant here: ReceiveDispatch is only constructed under can_grant (in-world + live
            // inventory pointer), the same guarantee the main item grant path relies on.
            for full_id in crate::region::first_open_grants(cfg, name) {
                if self.hook.grant_full_id(full_id, 1) {
                    log::info!("lockGrantItems: '{name}' rider {full_id:#x} granted");
                } else {
                    log::warn!(
                        "lockGrantItems: '{name}' rider {full_id:#x} failed to place \
                         (in-world grant path) -- no retry; re-grantable by reloading the save \
                         before re-receiving the lock"
                    );
                }
            }
            if crate::region::open_on_received_name(cfg, name) {
                self.unlocked.push(name.trim_end_matches(" Lock").to_string());
            }
        }
    }

    /// Progressive tier routing (er_logic::progressive), exactly the old step 4b progressive arm:
    /// apply the tier's flags + goods; `true` (handled) makes the seam skip the normal grant.
    /// Tier grants remain best-effort (the tier counter has no rollback API) — but the caller only
    /// runs this in-world with a live inventory pointer, and a failed placement is now logged
    /// (it was silently discarded before the seam).
    fn progressive_on_item_received(&mut self, name: &str, ap_index: i64) -> bool {
        let eff = self.progressive.on_item_received(name, ap_index);
        if eff.handled {
            for &f in &eff.flags {
                self.hook.set_event_flag(f, true);
            }
            for &g in &eff.grants {
                if !self.hook.grant_full_id(g, 1) {
                    log::warn!(
                        "progressive '{name}' (idx {ap_index}): tier grant {g:#x} failed to place -- tier already advanced"
                    );
                }
            }
        }
        eff.handled
    }
}
