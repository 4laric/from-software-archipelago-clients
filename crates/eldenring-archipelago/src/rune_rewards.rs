//! Opt-in direct rune reward scaling for completion-scaled enemies and bosses (#1091).
//!
//! Ordinary enemy payouts live in `NpcParam.getSoul`; boss victory payouts live in the two
//! `GameAreaParam.bonusSoul` fields. We snapshot the values the game actually loaded once, then
//! always derive writes from that immutable baseline. That makes region changes idempotent and,
//! importantly, composes with a regulation-based enemy randomizer: its already-randomized values
//! become the baseline instead of receiving another cumulative multiplier.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use eldenring::cs::{GameAreaParam, NpcParam, SoloParamRepository};
use fromsoftware_shared::FromStatic;
use serde_json::Value;

const NO_TIER: usize = usize::MAX;

#[derive(Default)]
struct Baseline {
    enemies: Vec<(u32, u32)>,
    bosses: Vec<(u32, u32, u32)>,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static LAST_TIER: AtomicUsize = AtomicUsize::new(NO_TIER);
static BASELINE: Mutex<Option<Baseline>> = Mutex::new(None);

pub fn configure(sd: &Value) {
    let enabled = er_logic::options::parse_bool_option(sd, "scale_rune_rewards");
    ENABLED.store(enabled, Ordering::Relaxed);
    LAST_TIER.store(NO_TIER, Ordering::Relaxed);
    if let Ok(mut baseline) = BASELINE.lock() {
        *baseline = None;
    }
    log::info!(
        "rune-reward-scaling: {}",
        if enabled { "enabled" } else { "disabled" }
    );
}

pub fn armed() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Apply the current region tier once. Returns false while param holders are unavailable so the
/// caller naturally retries after restream/startup; true means either applied or intentionally off.
pub fn run(target_tier: usize) -> bool {
    if !ENABLED.load(Ordering::Relaxed) {
        return true;
    }
    if LAST_TIER.load(Ordering::Relaxed) == target_tier {
        return true;
    }
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return false;
    };
    if !crate::param_guard::is_available::<NpcParam>(repo, "rune reward scaling")
        || !crate::param_guard::is_available::<GameAreaParam>(repo, "rune reward scaling")
    {
        return false;
    }

    let Ok(mut baseline) = BASELINE.lock() else {
        return false;
    };
    if baseline.is_none() {
        let Some(enemy_rows) =
            crate::param_guard::rows_mut::<NpcParam>(repo, "rune reward scaling")
        else {
            return false;
        };
        let enemies = enemy_rows
            .filter_map(|(id, row)| (row.get_soul() != 0).then_some((id, row.get_soul())))
            .collect();
        let Some(boss_rows) =
            crate::param_guard::rows_mut::<GameAreaParam>(repo, "rune reward scaling")
        else {
            return false;
        };
        let bosses = boss_rows
            .filter_map(|(id, row)| {
                let single = row.bonus_soul_single();
                let multi = row.bonus_soul_multi();
                (single != 0 || multi != 0).then_some((id, single, multi))
            })
            .collect();
        *baseline = Some(Baseline { enemies, bosses });
    }

    let baseline = baseline.as_ref().expect("baseline initialized above");
    let mut enemy_count = 0usize;
    for &(id, vanilla) in &baseline.enemies {
        let Some(row) = crate::param_guard::get_mut::<NpcParam>(repo, id, "rune reward scaling")
        else {
            continue;
        };
        row.set_get_soul(er_logic::scaling::scale_rune_reward(vanilla, target_tier));
        enemy_count += 1;
    }
    let mut boss_count = 0usize;
    for &(id, vanilla_single, vanilla_multi) in &baseline.bosses {
        let Some(row) =
            crate::param_guard::get_mut::<GameAreaParam>(repo, id, "rune reward scaling")
        else {
            continue;
        };
        row.set_bonus_soul_single(er_logic::scaling::scale_rune_reward(
            vanilla_single,
            target_tier,
        ));
        row.set_bonus_soul_multi(er_logic::scaling::scale_rune_reward(
            vanilla_multi,
            target_tier,
        ));
        boss_count += 1;
    }
    LAST_TIER.store(target_tier, Ordering::Relaxed);
    log::info!(
        "rune-reward-scaling: tier {target_tier}, rewrote {enemy_count} enemy and {boss_count} boss reward rows"
    );
    true
}
