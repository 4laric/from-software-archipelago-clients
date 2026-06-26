//! MILESTONE B — increment #7 (ECHO model, own_world:true). Supersedes increment #6c's core.
//!
//! With `own_world:true`, the server echoes our own checks back as received items, so the
//! received-item path is the SINGLE grant path (and it runs progressive / region-open / notify by
//! name for self-found items too). The detour + inventory-scan therefore only REPORT checks
//! (mark_checked) and suppress; they no longer grant locally. This fixes self-found progressive,
//! region keys, and notify items, which the old local-grant path silently skipped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use archipelago_rs as ap;
use er_logic::progressive::ProgressiveState;
use er_logic::save_state::SaveState;
use serde_json::Value;
use shared::CoreBase;

pub struct Core {
    base: CoreBase<crate::game::EldenRing, Value>,
    detour_installed: bool,
    received_through: usize,
    dispatched_through: usize,
    item_map: Option<HashMap<i64, i64>>,
    item_counts: HashMap<i64, i64>,
    region: Option<crate::region::RegionConfig>,
    progressive: ProgressiveState,
    slot_data_parsed: bool,
    my_name: Option<String>,
    save_path: Option<PathBuf>,
    save_loaded: bool,
    last_persisted_index: i64,
    valid_locations: HashSet<i64>,
    locations_loaded: bool,
    /// Bake-emitted location->flag map (apconfig) for detour-bypass checks (NPC gifts, death drops).
    flag_poll: Option<crate::flagpoll::FlagPollConfig>,
    /// slot_data dungeonSweeps: trigger location -> member locations.
    dungeon_sweeps: HashMap<i64, Vec<i64>>,
    /// Throttle the (potentially large) flag poll to a few times a second.
    poll_counter: u32,
    /// Start-of-run grants (items / graces / map reveal).
    start: Option<crate::startgrants::StartConfig>,
    start_flags_done: bool,
    /// Persisted (SaveState): start items granted once for this save.
    start_items_granted: bool,
}

impl shared::Core for Core {
    type SlotData = Value;
    type Game = crate::game::EldenRing;

    fn new() -> Result<Self> {
        Ok(Self {
            base: CoreBase::new("EldenRing")?,
            detour_installed: false,
            received_through: 0,
            dispatched_through: 0,
            item_map: None,
            item_counts: HashMap::new(),
            region: None,
            progressive: ProgressiveState::new(HashMap::new()),
            slot_data_parsed: false,
            my_name: None,
            save_path: None,
            save_loaded: false,
            last_persisted_index: -1,
            valid_locations: HashSet::new(),
            locations_loaded: false,
            flag_poll: None,
            dungeon_sweeps: HashMap::new(),
            poll_counter: 0,
            start: None,
            start_flags_done: false,
            start_items_granted: false,
        })
    }
    fn base(&self) -> &CoreBase<Self::Game, Self::SlotData> {
        &self.base
    }
    fn base_mut(&mut self) -> &mut CoreBase<Self::Game, Self::SlotData> {
        &mut self.base
    }

    fn update_live(&mut self) -> Result<()> {
        if !self.detour_installed {
            match crate::detour::install() {
                Ok(()) => self.detour_installed = true,
                Err(e) => log::warn!("AddItemFunc detour install deferred: {e}"),
            }
        }

        // 1. Report suppressed (world-pickup) synthetics. The echo grants them.
        let checks = crate::detour::take_pending_checks();
        if !checks.is_empty()
            && let Some(client) = self.client_mut()
            && let Err(e) = client.mark_checked(checks.iter().copied())
        {
            log::warn!("mark_checked failed for {checks:?}: {e}");
        }

        // 2. Parse slot_data once.
        if !self.slot_data_parsed {
            let parsed = self.client().map(|client| {
                let sd = client.slot_data();
                crate::deathlink::set_enabled(
                    sd.pointer("/options/death_link").and_then(|v| v.as_bool()).unwrap_or(false),
                );
                crate::upgrades::set_auto_upgrade(
                    sd.pointer("/options/auto_upgrade").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                crate::upgrades::set_global_scadu_blessing(
                    sd.pointer("/options/global_scadutree_blessing").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                let map = i64_map(sd.get("apIdsToItemIds"));
                let counts = i64_map(sd.get("itemCounts"));
                let region = crate::region::parse(sd);
                let prog_cfg = er_logic::progressive::parse(sd);
                let name = client.this_player().alias().to_string();
                let sweeps = crate::flagpoll::parse_dungeon_sweeps(sd);
                let start = crate::startgrants::parse(sd);

                // Connect banner: build identity + slot_data contract version (+ gate result) so any
                // logfile self-identifies which build / contract produced it. Then a one-line
                // start-config summary of the exact fields we previously had to decompile the
                // multidata to see — startRegion, startGraces count, reveal_all_maps, the random-start
                // warp/area/done flags, and the area-lock count.
                let versions = sd.get("versions").and_then(|v| v.as_str()).unwrap_or("(none)");
                let gate = er_logic::version::version_gate(sd, env!("CARGO_PKG_VERSION"));
                let start_region = sd.get("startRegion").and_then(|v| v.as_str()).unwrap_or("");
                log::info!(
                    "=== ER-AP client {} | contract {versions} (gate {gate:?}) | slot '{name}' ===",
                    crate::game::CLIENT_BUILD
                );
                log::info!(
                    "startcfg: start_region={start_region:?} | startGraces={} reveal_maps={} startItems={} | randomStart warp/area/done={}/{}/{} | area_locks={}",
                    start.start_graces.len(),
                    start.reveal_all_maps,
                    start.start_items.len(),
                    region.random_start_warp_flag,
                    region.random_start_area_id,
                    region.random_start_done_flag,
                    region.area_lock_flags.len()
                );

                (map, counts, region, prog_cfg, name, sweeps, start)
            });
            if let Some((map, counts, region, prog_cfg, name, sweeps, start)) = parsed {
                log::info!(
                    "slot_data parsed: {} item-map, {} area-lock, {} progressive; player '{name}'",
                    map.len(),
                    region.area_lock_flags.len(),
                    prog_cfg.len()
                );
                self.item_map = Some(map);
                self.item_counts = counts;
                self.region = Some(region);
                self.progressive = ProgressiveState::new(prog_cfg);
                self.my_name = Some(name);
                self.dungeon_sweeps = sweeps;
                self.flag_poll = Some(crate::flagpoll::load());
                self.start = Some(start);
                self.slot_data_parsed = true;
            }
        }

        // 2b. Load the persisted save once (resume watermark + progressive tiers).
        if self.slot_data_parsed && !self.save_loaded {
            if let Some(path) = save_file_path(self.seed(), self.my_name.as_deref().unwrap_or("")) {
                let st = std::fs::read_to_string(&path)
                    .ok()
                    .map(|t| SaveState::from_json(&t))
                    .unwrap_or_default();
                self.received_through = st.last_received_index.max(0) as usize;
                self.progressive.restore(
                    st.progressive_counter
                        .iter()
                        .map(|(k, &v)| (k.clone(), v))
                        .collect(),
                    st.progressive_high_index,
                );
                self.start_items_granted = st.start_items_granted;
                self.last_persisted_index = st.last_received_index;
                self.save_path = Some(path);
                log::info!(
                    "save loaded: resume at received index {}",
                    self.received_through
                );
            }
            self.save_loaded = true;
        }

        // 2c. Cache the slot's valid location set once (shop-scan dedup guard).
        if !self.locations_loaded {
            let v: HashSet<i64> = self
                .client()
                .map(|client| {
                    client
                        .checked_locations()
                        .map(|l| l.id())
                        .chain(client.unchecked_locations().map(|l| l.id()))
                        .collect()
                })
                .unwrap_or_default();
            if !v.is_empty() {
                self.valid_locations = v;
                self.locations_loaded = true;
            }
        }

        // 2d. Start grants: graces + map reveal (once, retried until the flag holder is up) and start
        //     items (Torrent etc.) once per save (persisted), gated on a captured inventory pointer.
        if self.slot_data_parsed {
            let already_flags = self.start_flags_done;
            let already_items = self.start_items_granted;
            let has_inv = crate::detour::has_inventory();
            let mut did_flags = false;
            let mut did_items = false;
            if let Some(sc) = self.start.as_ref() {
                // Gate start FLAGS on a loaded world (has_inventory), not just CSEventFlagMan being
                // up: setting grace/map flags during the load screen lets the subsequent save-data
                // load clobber them, which is the suspected cause of "no graces/maps in-game" despite
                // correct slot_data. (The standalone gated its grace flush the same way.) After
                // applying, read a sentinel grace back — only latch `done` once it sticks; a false
                // read-back means it was clobbered, so we log it and retry next tick.
                if !already_flags && has_inv && crate::startgrants::apply_start_flags(sc) {
                    let sentinel = sc.start_graces.first().copied();
                    let stuck = sentinel.is_none_or(crate::flags::get_event_flag);
                    if stuck {
                        did_flags = true;
                    } else {
                        log::warn!(
                            "start graces set but sentinel flag {sentinel:?} read back FALSE (clobbered by save load?) — retrying next tick"
                        );
                    }
                }
                if !already_items
                    && has_inv
                    && (sc.start_items.is_empty()
                        || sc
                            .start_items
                            .iter()
                            .all(|&id| crate::detour::grant_full_id(id, 1)))
                {
                    did_items = true;
                }
            }
            if did_flags {
                self.start_flags_done = true;
                if let Some(sc) = self.start.as_ref() {
                    log::info!(
                        "start graces + map reveal applied: {} grace flag(s), reveal_maps={}",
                        sc.start_graces.len(),
                        sc.reveal_all_maps
                    );
                }
            }
            if did_items {
                self.start_items_granted = true;
                log::info!("start items granted");
                self.write_save();
            }
        }

        // Prime the inventory pointer from a game static (if enabled+confirmed) so grants flush
        // without waiting for the player's first in-game pickup. No-op until USE_STATIC_INVENTORY_PRIME
        // is turned on; the detour still captures the game's own pointer on a real pickup regardless.
        crate::detour::prime_inventory_if_needed();

        // 3. Collect region-open names + received-item grant candidates in one borrow. Under
        //    own_world:true this stream now ALSO carries the echoes of our own self-found checks.
        let mut disp = self.dispatched_through;
        let recv = self.received_through;
        let can_grant = crate::detour::has_inventory();
        let mut names_to_open: Vec<String> = Vec::new();
        let mut candidates: Vec<(i64, String, Option<i64>, i64)> = Vec::new();
        let mut new_recv = recv;
        // Cumulative set of ALL received item names — natural-key triggers need the full history
        // (a clause may require an item received many ticks ago), not just this tick's new names.
        let mut received_all: HashSet<String> = HashSet::new();
        if let Some(client) = self.client() {
            let items = client.received_items();
            if items.len() < disp {
                disp = 0;
            }
            for ri in items.iter() {
                received_all.insert(ri.item().name().to_string());
            }
            for ri in items.iter().skip(disp) {
                names_to_open.push(ri.item().name().to_string());
            }
            disp = items.len();
            if can_grant {
                for (idx, ri) in items.iter().enumerate().skip(recv) {
                    let name = ri.item().name().to_string();
                    let ap_id = ri.item().id();
                    let full = self.item_map.as_ref().and_then(|m| m.get(&ap_id).copied());
                    let qty = self.item_counts.get(&ap_id).copied().unwrap_or(1).max(1);
                    candidates.push((idx as i64, name, full, qty));
                }
                new_recv = items.len();
            }
        }

        // 4a. Per received name: set vanilla obtained flags (spirit bell / whetblades / Rold / …),
        //     open regions, and collect region-unlock notifications for the overlay console.
        let mut unlocked: Vec<String> = Vec::new();
        for name in &names_to_open {
            crate::keyitems::set_acquire_flags(name);
            if let Some(cfg) = self.region.as_ref()
                && crate::region::open_on_received_name(cfg, name)
            {
                unlocked.push(name.trim_end_matches(" Lock").to_string());
            }
        }
        for region in unlocked {
            self.log(ap::Print::message(format!("Region unlocked: {region}")));
        }
        // 4b. Grant received items (progressive routes through tiers; everything else its mapped item).
        for (idx, name, full, qty) in candidates {
            let eff = self.progressive.on_item_received(&name, idx);
            if eff.handled {
                for f in &eff.flags {
                    crate::flags::set_event_flag(*f, true);
                }
                for g in &eff.grants {
                    crate::detour::grant_full_id(*g, 1);
                }
            } else if let Some(f) = full {
                crate::detour::grant_full_id(f as i32, qty as i32);
            }
            // Great runes additionally grant their "(Restored)" goods row (equippable immediately).
            if let Some(restored) = crate::keyitems::restored_great_rune_goods(&name) {
                crate::detour::grant_full_id(restored, 1);
            }
        }
        self.dispatched_through = disp;
        self.received_through = new_recv;

        // 4c. Persist on watermark advance.
        if self.received_through as i64 != self.last_persisted_index {
            self.write_save();
            self.last_persisted_index = self.received_through as i64;
        }

        // 5. Shop / NPC / offline discovery: synthetic placeholders that bypassed the detour are in
        //    the bag. REPORT only (echo grants); dedup by checked-location so it can't re-report.
        if can_grant && self.locations_loaded {
            let scanned = crate::inventory::scan_synthetics();
            if !scanned.is_empty() {
                let mut to_check: Vec<i64> = Vec::new();
                if let Some(client) = self.client() {
                    for s in &scanned {
                        if self.valid_locations.contains(&s.location)
                            && !client.is_local_location_checked(s.location)
                        {
                            to_check.push(s.location);
                        }
                    }
                }
                if !to_check.is_empty() {
                    log::info!("shop/offline discovery: {} new check(s)", to_check.len());
                    if let Some(client) = self.client_mut()
                        && let Err(e) = client.mark_checked(to_check.iter().copied())
                    {
                        log::warn!("shop mark_checked failed: {e}");
                    }
                }
            }
        }

        // 5b. Flag-poll: report detour-bypass checks (NPC gifts, NPC death drops, offline pickups)
        //     whose guarding event flag has fired, plus dungeon/boss sweeps. Throttled — flags don't
        //     change fast and the map can be large. Dedup via the server's checked set (reload-safe).
        self.poll_counter = self.poll_counter.wrapping_add(1);
        if self.locations_loaded && self.poll_counter.is_multiple_of(15) {
            let mut to_check: Vec<i64> = Vec::new();
            if let (Some(fp), Some(client)) = (self.flag_poll.as_ref(), self.client()) {
                for (&loc, &flag) in &fp.location_flags {
                    if self.valid_locations.contains(&loc)
                        && !client.is_local_location_checked(loc)
                        && crate::flags::get_event_flag(flag)
                    {
                        to_check.push(loc);
                    }
                }
                for (trigger, members) in &self.dungeon_sweeps {
                    if let Some(&flag) = fp.location_flags.get(trigger)
                        && crate::flags::get_event_flag(flag)
                    {
                        for &m in members {
                            if self.valid_locations.contains(&m)
                                && !client.is_local_location_checked(m)
                            {
                                to_check.push(m);
                            }
                        }
                    }
                }
                for (&flag, locs) in &fp.sweep_flags {
                    if crate::flags::get_event_flag(flag) {
                        for &loc in locs {
                            if self.valid_locations.contains(&loc)
                                && !client.is_local_location_checked(loc)
                            {
                                to_check.push(loc);
                            }
                        }
                    }
                }
            }
            if !to_check.is_empty() {
                to_check.sort_unstable();
                to_check.dedup();
                log::info!("flag-poll: {} new check(s)", to_check.len());
                if let Some(client) = self.client_mut() {
                    let _ = client.mark_checked(to_check.iter().copied());
                }
            }
        }

        // 6. Region-lock KICK + random-start warp trigger (order matters: the warp sets the
        //    done-flag that KICK's start-window guard waits on, so fire it before the kick check).
        if let Some(cfg) = self.region.as_ref() {
            crate::region::tick_random_start_warp(cfg);
            // Natural-key regions (Raya/Mountaintops/Snowfield/...) bloom when their vanilla-key
            // disjunction is satisfied. Gated on a loaded world (can_grant) so the flags it sets
            // aren't clobbered by the save load — same reason the start graces are gated.
            if can_grant {
                crate::region::tick_natural_key_triggers(cfg, &received_all);
            }
            crate::region::tick_kick(cfg);
        }

        // 7. DeathLink.
        let my_name = self.my_name.clone();
        for ev in self.take_events() {
            if let ap::Event::DeathLink { source, .. } = ev {
                let foreign = my_name.as_deref().map(|n| n != source).unwrap_or(true);
                if foreign {
                    log::info!("DeathLink received from '{source}'");
                    crate::deathlink::latch_incoming_kill();
                }
            }
        }
        crate::deathlink::drive_kill();
        if crate::deathlink::is_enabled()
            && crate::deathlink::poll_local_death()
            && let Some(client) = self.client_mut()
        {
            let _ = client.death_link(ap::DeathLinkOptions::default());
        }

        // 8. Scadutree blessing writer.
        crate::upgrades::tick_global_scadu();

        Ok(())
    }
}

impl Core {
    fn write_save(&self) {
        let Some(path) = self.save_path.as_ref() else {
            return;
        };
        let (counter, high) = self.progressive.snapshot();
        let st = SaveState {
            last_received_index: self.received_through as i64,
            start_items_granted: self.start_items_granted,
            notify_granted: Default::default(),
            progressive_counter: counter.into_iter().collect::<BTreeMap<_, _>>(),
            progressive_high_index: high,
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, st.to_json()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn save_file_path(seed: &str, name: &str) -> Option<PathBuf> {
    let dir = shared::utils::mod_directory().ok()?;
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    };
    Some(dir.join(format!("ap_save_{}_{}.json", safe(seed), safe(name))))
}

fn i64_map(v: Option<&Value>) -> HashMap<i64, i64> {
    let mut m = HashMap::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let (Ok(key), Some(value)) = (k.parse::<i64>(), val.as_i64()) {
                m.insert(key, value);
            }
        }
    }
    m
}
