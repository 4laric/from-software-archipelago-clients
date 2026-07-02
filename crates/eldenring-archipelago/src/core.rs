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
use er_logic::hook::GameHook;
use er_logic::progressive::ProgressiveState;
use er_logic::receive::{GrantAction, RecvItem};
use er_logic::save_state::SaveState;
use serde_json::Value;
use shared::CoreBase;

use crate::hook_impl::{EldenRingHook, ReceiveDispatch};

pub struct Core {
    base: CoreBase<crate::game::EldenRing, Value>,
    detour_installed: bool,
    received_through: usize,
    dispatched_through: usize,
    item_map: Option<HashMap<i64, i64>>,
    item_counts: HashMap<i64, i64>,
    region: Option<crate::region::RegionConfig>,
    /// Region-lock fog-wall visuals (cosmetic; KICK still enforces).
    fogwall: Option<crate::fogwall::FogWallConfig>,
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
    /// Session-scoped (R11, SWEEP): indices into start_items that verifiably granted -- only the
    /// failed ones re-attempt; `start_items_granted` latches once ALL have landed.
    start_items_ok: HashSet<usize>,
    /// Pre-scout: resolves each shop reward's name/owner/ER-sell-id (pumped on the tick).
    scout: Option<crate::scout_proof::ScoutProof>,
    /// Goal-send (SPEC-goal-send-20260701.md): goalLocations split flag/checked at parse.
    goal: Option<crate::goal::GoalConfig>,
    /// Session latch: Goal sent once per connect (NOT persisted -- re-send is idempotent).
    sent_goal: bool,
}

impl shared::Core for Core {
    type SlotData = Value;
    type Game = crate::game::EldenRing;

    /// Debug console commands, typed into the overlay's say input (2026-07-01, playtest tooling).
    /// Unrecognized "!" commands fall through to server chat.
    fn handle_command(&mut self, command: &str, arg: Option<&str>) -> bool {
        match command {
            "!flag" => {
                match arg.and_then(|a| a.trim().parse::<u32>().ok()) {
                    Some(f) => {
                        let v = crate::flags::get_event_flag(f);
                        self.log(ap::Print::message(format!("flag {f} = {v}")));
                    }
                    None => self.log(ap::Print::message("usage: !flag <id>".to_string())),
                }
                true
            }
            "!setflag" => {
                let parts: Vec<&str> = arg.unwrap_or("").split_whitespace().collect();
                match parts.first().and_then(|s| s.parse::<u32>().ok()) {
                    Some(f) => {
                        let on = parts.get(1).map(|s| *s != "0").unwrap_or(true);
                        let ok = crate::flags::try_set_event_flag(f, on);
                        self.log(ap::Print::message(format!(
                            "setflag {f} {on} -> {}",
                            if ok { "OK" } else { "NOT READY" }
                        )));
                    }
                    None => self.log(ap::Print::message("usage: !setflag <id> [0|1]".to_string())),
                }
                true
            }
            "!region" => {
                let pr = crate::flags::play_region_id();
                self.log(ap::Print::message(format!("play_region = {pr:?}")));
                true
            }
            "!warp" => {
                // Playtest tooling for the pure-runtime warp primitive (also unblocks a
                // random-start seed by hand if the auto-warp misfires). Full grace ENTITY id,
                // e.g. `!warp 11102950` = Table of Lost Grace (Roundtable Hold).
                match arg.and_then(|a| a.trim().parse::<u32>().ok()) {
                    Some(g) => {
                        let msg = match crate::warp::warp_to_grace(g) {
                            Ok(()) => format!("warp requested -> grace {g}"),
                            Err(e) => format!("warp FAILED: {e}"),
                        };
                        self.log(ap::Print::message(msg));
                    }
                    None => self.log(ap::Print::message(
                        "usage: !warp <grace entity id> (11102950 = Roundtable)".to_string(),
                    )),
                }
                true
            }
            "!grace" => {
                let Some(q) = arg.map(|s| s.to_lowercase()) else {
                    self.log(ap::Print::message("usage: !grace <name substring>".to_string()));
                    return true;
                };
                let mut lines: Vec<String> = Vec::new();
                if let Some(cfg) = self.region.as_ref() {
                    for (name, &f) in &cfg.grace_items {
                        if name.to_lowercase().contains(&q) {
                            lines.push(format!(
                                "{name}: flag {f} = {}",
                                crate::flags::get_event_flag(f)
                            ));
                        }
                    }
                    for (name, fs) in &cfg.region_graces {
                        if name.to_lowercase().contains(&q) {
                            for &f in fs {
                                lines.push(format!(
                                    "{name} bundle: flag {f} = {}",
                                    crate::flags::get_event_flag(f)
                                ));
                            }
                        }
                    }
                }
                if lines.is_empty() {
                    lines.push(format!("no grace/lock matching '{q}'"));
                }
                for l in lines {
                    self.log(ap::Print::message(l));
                }
                true
            }
            "!help" => {
                self.log(ap::Print::message(
                    "!flag <id> | !setflag <id> [0|1] | !region | !grace <name substring>".to_string(),
                ));
                true
            }
            _ => false,
        }
    }

    fn new() -> Result<Self> {
        Ok(Self {
            base: CoreBase::new("EldenRing")?,
            detour_installed: false,
            received_through: 0,
            dispatched_through: 0,
            item_map: None,
            item_counts: HashMap::new(),
            region: None,
            fogwall: None,
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
            start_items_ok: HashSet::new(),
            scout: None,
            goal: None,
            sent_goal: false,
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
                // int-or-bool tolerant (er_logic::options): the apworld serializes options
                // as ints (death_link: 1), which .as_bool() silently read as false.
                crate::deathlink::set_enabled(er_logic::options::parse_death_link(sd));
                crate::no_weapon_reqs::set_enabled(er_logic::options::parse_bool_option(
                    sd,
                    "no_weapon_requirements",
                ));
                crate::upgrades::set_auto_upgrade(
                    sd.pointer("/options/auto_upgrade").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                crate::upgrades::set_global_scadu_blessing(
                    sd.pointer("/options/global_scadutree_blessing").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                let map = i64_map(sd.get("apIdsToItemIds"));
                let counts = i64_map(sd.get("itemCounts"));
                let region = crate::region::parse(sd);
                let fogwall = crate::fogwall::parse(sd);
                let prog_cfg = er_logic::progressive::parse(sd);
                let name = client.this_player().alias().to_string();
                let sweeps = crate::flagpoll::parse_dungeon_sweeps(sd);
                let start = crate::startgrants::parse(sd);

                // Shop system (SHOP-SYSTEM-HANDOFF.md §3): configure from slot_data, build the scout.
                let loc_flags = i64_to_u32_map(sd.get("locationFlags"));
                let preview: Vec<(i64, i32)> = i64_map(sd.get("shopPreviewGoods"))
                    .into_iter()
                    .map(|(l, g)| (l, g as i32))
                    .collect();
                crate::scout_proof::configure_item_map(map.clone());
                crate::shop_flags::configure(
                    i64_to_u32_map(sd.get("shopRowFlags"))
                        .into_iter()
                        .map(|(r, f)| (r as u32, f))
                        .collect(),
                );
                crate::shop_flags::configure_check_flags(loc_flags.values().copied().collect());
                crate::shop_sell::configure(loc_flags.clone());
                crate::shop_preview::configure(preview.clone());
                crate::shop_icon::configure(preview);
                crate::scaling::configure(sd); // runtime enemy scaling (regionSphereTargets)
                // checkItemFlags: full raw item id -> check acquisition flags (the PORT-GAP
                // vanilla-suppress table; LIVE in the detour since 2026-07-01).
                let check_flags: std::collections::HashMap<u32, Vec<u32>> = sd
                    .get("checkItemFlags")
                    .and_then(|v| v.as_object())
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| {
                                let id: u32 = k.parse().ok()?;
                                let fl = v.as_array()?
                                    .iter()
                                    .filter_map(|f| f.as_u64().map(|x| x as u32))
                                    .collect();
                                Some((id, fl))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                crate::detour::configure_check_item_flags(check_flags);
                let scout = crate::scout_proof::ScoutProof::new(loc_flags.keys().copied().collect());
                // Goal-send: split goalLocations into flag-detected / checked-fallback buckets
                // against loc_flags (SPEC-goal-send-20260701.md; do NOT route through flagpoll).
                let goal_cfg = crate::goal::parse(sd, &loc_flags);

                // Connect banner: build identity + slot_data contract version (+ gate result) so any
                // logfile self-identifies which build / contract produced it. Then a one-line
                // start-config summary of the exact fields we previously had to decompile the
                // multidata to see — startRegion, startGraces count, reveal_all_maps, the random-start
                // warp/area/done flags, and the area-lock count.
                let versions = sd.get("versions").and_then(|v| v.as_str()).unwrap_or("(none)");
                let gate = er_logic::version::version_gate(sd, env!("CARGO_PKG_VERSION"));
                // Warn-only contract (er_logic::version): Some(false) must NOT tear the
                // connection down, but it must be user-visible. Build the message here; it is
                // pushed to the persistent overlay console (same channel as "Region unlocked")
                // after this client borrow ends.
                let gate_warn = (gate == Some(false)).then(|| {
                    format!(
                        "apworld/client version mismatch: seed wants {versions}, client is {} — update the client",
                        env!("CARGO_PKG_VERSION")
                    )
                });
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

                (map, counts, region, fogwall, prog_cfg, name, sweeps, start, scout, gate_warn, loc_flags, goal_cfg)
            });
            if let Some((map, counts, region, fogwall, prog_cfg, name, sweeps, start, scout, gate_warn, loc_flags, goal_cfg)) =
                parsed
            {
                log::info!(
                    "slot_data parsed: {} item-map, {} area-lock, {} progressive; player '{name}'",
                    map.len(),
                    region.area_lock_flags.len(),
                    prog_cfg.len()
                );
                self.item_map = Some(map);
                self.item_counts = counts;
                self.region = Some(region);
                self.fogwall = Some(fogwall);
                self.progressive = ProgressiveState::new(prog_cfg);
                self.my_name = Some(name);
                self.dungeon_sweeps = sweeps;
                // F2 fix (2026-07-01): the flag-poll table travels in slot_data ("locationFlags")
                // now; baker-era apconfig.json no longer carries location_flags, so fresh installs
                // polled an EMPTY map (world pickups never sent checks -- seed looked vanilla).
                // slot_data wins; a legacy apconfig table still contributes sweep_flags / extras.
                let mut fp = crate::flagpoll::load();
                for (loc, flag) in loc_flags {
                    fp.location_flags.insert(loc, flag);
                }
                log::info!(
                    "flag-poll table: {} location flags ({} sweep groups)",
                    fp.location_flags.len(),
                    fp.sweep_flags.len()
                );
                self.flag_poll = Some(fp);
                self.start = Some(start);
                self.scout = Some(scout);
                self.goal = Some(goal_cfg);
                self.slot_data_parsed = true;
                if let Some(warning) = gate_warn {
                    log::error!("{warning}");
                    self.log(ap::Print::message(warning));
                }
            }
        }

        // 2b. Load the persisted save once (resume watermark + progressive tiers).
        if self.slot_data_parsed && !self.save_loaded {
            // SAVE-KEY FIX (2026-07-02): key the save by the ROOM's seed_name (RoomInfo ground
            // truth), not the apconfig seed. The staged apconfig ships "seed":"", so every seed
            // shared ONE file (ap_save__<slot>.json) and a fresh world resumed at the previous
            // world's watermark -- seen live on the ER+HK seed: "resume at received index 134"
            // on a brand-new multiworld, so the first 134 receives (start items included) were
            // treated as already-granted and never placed. Region opens self-healed via the
            // reconcile ticks, which masked everything except the missing bag items.
            let room_seed = self
                .client()
                .map(|c| c.seed_name().to_string())
                .unwrap_or_default();
            let seed_key = if room_seed.is_empty() {
                // No RoomInfo seed (shouldn't happen once slot_data parsed) -- fall back to the
                // apconfig seed rather than never arming persistence.
                self.seed().to_string()
            } else {
                room_seed
            };
            if let Some(path) = save_file_path(&seed_key, self.my_name.as_deref().unwrap_or("")) {
                let st = match std::fs::read_to_string(&path) {
                    Ok(saved_text) => {
                        // R7 (SWEEP): from_json is tolerant -- a present-but-corrupt save would
                        // silently reset every watermark (duplicate start items + regrant burst).
                        if serde_json::from_str::<Value>(&saved_text).is_err() {
                            log::error!(
                                "save file {} is CORRUPT (not valid JSON) -- watermarks reset to defaults",
                                path.display()
                            );
                        }
                        SaveState::from_json(&saved_text)
                    }
                    Err(_) => SaveState::default(), // absent = fresh save (normal first run)
                };
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
                log::info!("save persistence armed at {}", path.display());
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
            // Same in_world tightening as can_grant (SWEEP H3): the inventory pointer never
            // resets on quit-to-menu, so menu-time start grants would write through a stale one.
            let has_inv = crate::detour::has_inventory() && crate::flags::in_world();
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
                // R11 (SWEEP): `.all(grant_full_id)` re-granted already-succeeded items on a
                // partial failure next tick (duplicates). Track per-item success (by index,
                // session-scoped) so only the FAILED ones re-attempt; latch once all landed.
                if !already_items && has_inv {
                    let mut all_ok = true;
                    for (i, &id) in sc.start_items.iter().enumerate() {
                        if self.start_items_ok.contains(&i) {
                            continue;
                        }
                        if crate::detour::grant_full_id(id, 1) {
                            self.start_items_ok.insert(i);
                        } else {
                            all_ok = false;
                        }
                    }
                    if all_ok {
                        did_items = true;
                    }
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

        // 3. Snapshot the received-item stream in one client borrow (RecvItem mirrors for the
        //    seam, plus the cumulative name set the reconcile ticks need). Under own_world:true
        //    this stream ALSO carries the echoes of our own self-found checks.
        let mut disp = self.dispatched_through;
        // SWEEP H3 (verified watermark, via er_logic::receive below): both name-dispatch and
        // grants only run with a loaded world + live inventory pointer — menu-time writes go
        // through a stale pointer / get discarded, and used to advance the watermark on faith.
        let can_grant = crate::detour::has_inventory() && crate::flags::in_world();
        // Cumulative set of ALL received item names — natural-key triggers need the full history
        // (a clause may require an item received many ticks ago), not just this tick's new names.
        let mut received_all: HashSet<String> = HashSet::new();
        let mut snapshot: Vec<RecvItem> = Vec::new();
        if let Some(client) = self.client() {
            let items = client.received_items();
            if items.len() < disp {
                disp = 0; // reconnect shrank the stream -> replay name dispatch from index 0
            }
            // Items below BOTH watermarks are AlreadyPushed no-ops; skip snapshotting them.
            let floor = disp.min(self.received_through);
            for (idx, ri) in items.iter().enumerate() {
                let name = ri.item().name().to_string();
                if can_grant && idx >= floor {
                    snapshot.push(RecvItem {
                        index: idx as i64,
                        ap_item_id: ri.item().id(),
                        name: name.clone(),
                    });
                }
                received_all.insert(name);
            }
        }

        // 4. The receive seam (er_logic::receive, host-tested): per item, name-dispatch when
        //    idx >= dispatched_through (keyitems fast path / region open / progressive routing,
        //    via ReceiveDispatch), then grant when idx >= received_through. received_through only
        //    advances past items whose grant VERIFIABLY placed (SWEEP H3): on a failed placement
        //    it is rolled back to the failed item and the tail retries in order next tick.
        //    dispatched_through keeps its advance regardless — name effects are idempotent and
        //    the section-6 reconcile ticks self-heal any lost flag write.
        let mut dispatched = disp as i64;
        let mut pushed = self.received_through as i64;
        let mut unlocked: Vec<String> = Vec::new();
        if can_grant && !snapshot.is_empty() {
            let empty_map = HashMap::new();
            let item_map = self.item_map.as_ref().unwrap_or(&empty_map);
            let mut game = EldenRingHook;
            let mut dispatch = ReceiveDispatch {
                region: self.region.as_ref(),
                progressive: &mut self.progressive,
                hook: &mut game,
                unlocked: Vec::new(),
            };
            for ri in &snapshot {
                let pushed_before = pushed;
                let action = er_logic::receive::process_received_item(
                    ri,
                    &mut dispatched,
                    &mut pushed,
                    item_map,
                    &self.item_counts,
                    &mut dispatch,
                );
                match action {
                    GrantAction::Enqueue { full_id, qty, name, .. } => {
                        if dispatch.hook.grant_full_id(full_id, qty) {
                            // Great runes additionally grant their "(Restored)" goods row
                            // (equippable immediately). Idempotent: restored rows dedup in-game.
                            if let Some(restored) = crate::keyitems::restored_great_rune_goods(&name)
                            {
                                let _ = dispatch.hook.grant_full_id(restored, 1);
                            }
                        } else {
                            // H3: the grant did NOT place — hold received_through at this item
                            // and stop so the tail replays in order next tick (never advance the
                            // watermark past an unverified grant).
                            pushed = pushed_before;
                            log::warn!(
                                "grant '{name}' (idx {}) failed to place -- receive watermark held for retry",
                                ri.index
                            );
                            break;
                        }
                    }
                    GrantAction::SkipProgressive => {
                        // Tier effects already applied in the dispatch (ReceiveDispatch). Mirror
                        // the old loop's unconditional rune-restore for every candidate branch.
                        if let Some(restored) = crate::keyitems::restored_great_rune_goods(&ri.name)
                        {
                            let _ = dispatch.hook.grant_full_id(restored, 1);
                        }
                    }
                    GrantAction::SkipUnmapped { ap_item_id } => {
                        // R5 (SWEEP): AP id absent from apIdsToItemIds and progressive didn't
                        // handle it — nothing granted; without this the item vanishes traceless.
                        warn_unmapped_once(&ri.name, ap_item_id);
                        if let Some(restored) = crate::keyitems::restored_great_rune_goods(&ri.name)
                        {
                            let _ = dispatch.hook.grant_full_id(restored, 1);
                        }
                    }
                    GrantAction::AlreadyPushed => {}
                }
            }
            unlocked = dispatch.unlocked;
        }
        self.dispatched_through = dispatched.max(0) as usize;
        self.received_through = pushed.max(0) as usize;
        for region in unlocked {
            self.log(ap::Print::message(format!("Region unlocked: {region}")));
        }

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
                if let Some(client) = self.client_mut()
                    && let Err(e) = client.mark_checked(to_check.iter().copied())
                {
                    log::warn!("flag-poll mark_checked failed: {e}");
                }
            }
        }

        // 5c. Goal-send (SPEC-goal-send-20260701.md): once EVERY goalLocations entry is done —
        //     local DefeatFlag first (immune to another slot's !collect), checked-set fallback
        //     for detection-table stragglers — send ClientStatus::Goal. Same throttle as the
        //     flag poll; gated on a loaded world so flags are never read during a load screen.
        //     Session latch only: a re-send after reconnect is idempotent server-side.
        if !self.sent_goal
            && can_grant
            && self.locations_loaded
            && self.poll_counter.is_multiple_of(15)
        {
            let met = match (self.goal.as_ref(), self.client()) {
                (Some(cfg), Some(client)) => crate::goal::is_met(
                    cfg,
                    crate::flags::get_event_flag,
                    // Pre-filter against valid_locations: is_local_location_checked PANICS on
                    // ids the datapackage doesn't know (archipelago_rs client.rs).
                    |l| self.valid_locations.contains(&l) && client.is_local_location_checked(l),
                ),
                _ => false,
            };
            if met {
                let sent = match self.client_mut() {
                    Some(client) => match client.set_status(ap::ClientStatus::Goal) {
                        Ok(_) => true,
                        Err(e) => {
                            log::warn!("goal: set_status(Goal) failed (will retry next poll): {e}");
                            false
                        }
                    },
                    None => false,
                };
                if sent {
                    self.sent_goal = true;
                    log::info!("goal: all goal locations complete -> ClientStatus::Goal sent");
                    self.log(ap::Print::message(
                        "GOAL COMPLETE! Victory sent to Archipelago.".to_string(),
                    ));
                }
            }
        }

        // 6. Region-lock KICK + random-start warp trigger (order matters: the warp sets the
        //    done-flag that KICK's start-window guard waits on, so fire it before the kick check).
        let mut graces_lit: Vec<String> = Vec::new();
        // Player-facing overlay messages from the region ticks (warp requested / arrival /
        // kick) -- collected here because cfg borrows self, logged after the borrow ends.
        let mut region_msgs: Vec<String> = Vec::new();
        if let Some(cfg) = self.region.as_ref() {
            if let Some(m) = crate::region::tick_random_start_warp(cfg) {
                region_msgs.push(m);
            }
            // Natural-key regions (Raya/Mountaintops/Snowfield/...) bloom when their vanilla-key
            // disjunction is satisfied. Gated on a loaded world (can_grant) so the flags it sets
            // aren't clobbered by the save load — same reason the start graces are gated.
            if can_grant {
                crate::region::tick_natural_key_triggers(cfg, &received_all);
                // Re-apply lock unlocks whose one-shot receive was discarded at menu/load
                // (lost graces/open flags -- 2026-07-01 playtest). Latched on the open flag.
                crate::region::tick_reconcile_received_locks(cfg, &received_all);
                // R3 (SWEEP): key-item obtained flags, same reconcile family -- the one-shot
                // write in 4a is lost at menu/load; this re-applies with the flag as the latch.
                crate::keyitems::tick_keyitem_flags(&received_all);
                // grace_rando: light received "Grace: ..." items (graceItems port-gap, 2026-07-01).
                graces_lit = crate::region::tick_grace_items(cfg, &received_all);
            }
            if let Some(m) = crate::region::tick_kick(cfg) {
                region_msgs.push(m);
            }
        }
        for g in graces_lit {
            self.log(ap::Print::message(format!("{g} unlocked")));
        }
        for m in region_msgs {
            self.log(ap::Print::message(m));
        }

        // 7. DeathLink.
        let my_name = self.my_name.clone();
        for ev in self.take_events() {
            if let ap::Event::DeathLink { source, .. } = ev {
                let foreign = my_name.as_deref().map(|n| n != source).unwrap_or(true);
                if foreign {
                    // R2 (SWEEP H2): honor the slot's death_link option on the INCOMING side too
                    // (the tag is advertised unconditionally; only the outgoing send was gated).
                    if crate::deathlink::is_enabled() {
                        log::info!("DeathLink received from '{source}'");
                        crate::deathlink::latch_incoming_kill();
                    } else {
                        log::info!(
                            "DeathLink received from '{source}' but disabled for this slot -- ignored"
                        );
                    }
                }
            }
        }
        crate::deathlink::drive_kill();
        if crate::deathlink::is_enabled()
            && crate::deathlink::poll_local_death()
            && let Some(client) = self.client_mut()
        {
            log::info!("DeathLink: local death detected -> broadcasting");
            if let Err(e) = client.death_link(ap::DeathLinkOptions::default()) {
                log::warn!("DeathLink: broadcast failed: {e}");
            }
        }

        // 8. Scadutree blessing writer.
        crate::upgrades::tick_global_scadu();

        // 8b. no_weapon_requirements runtime param zeroing (latched once applied).
        crate::no_weapon_reqs::tick();

        // 8c. Ticker-only pickup notifs: set showDialogCondType=0 game-wide so AP grants show the
        //     native right-side ticker, not the blocking "NEW Y:OK" modal (was a retired-baker
        //     regulation edit; ported to runtime, latched once applied).
        crate::notif_ticker::tick();

        // 9. Shop system (SHOP-SYSTEM-HANDOFF.md tick order). Pump the scout first (needs client_mut;
        //    take() to dodge the self double-borrow), then run each shop edit in order. Each self-gates
        //    on cache_ready / param-repo and latches DONE after one in-world pass.
        let mut scout = self.scout.take();
        if let Some(sp) = scout.as_mut()
            && let Some(client) = self.client_mut()
        {
            sp.pump(client);
        }
        self.scout = scout;
        if crate::flags::in_world() {
            let _ = crate::fmg_inject::run();
            let _ = crate::shop_flags::run(&[]);
            let _ = crate::shop_sell::run();
            let _ = crate::shop_preview::run();
            let _ = crate::shop_icon::run();
            crate::scaling::tick();
            // Region-lock fog-wall visuals (cosmetic marker at locked borders; the KICK reactor,
            // not this, does the blocking). Runs on the game thread (FrameBegin task) so the
            // CSWorldGeomMan::spawn_geometry call is main-thread-safe.
            if let Some(fw) = self.fogwall.as_mut() {
                crate::fogwall::tick(fw);
            }
        }

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
        // R7 (SWEEP): surface write/rename failures -- a silently-lost save resets the
        // watermarks next session (duplicate start items + regrant burst).
        match std::fs::write(&tmp, st.to_json()) {
            Ok(()) => {
                if let Err(e) = std::fs::rename(&tmp, path) {
                    log::error!(
                        "save persistence: rename {} -> {} FAILED: {e}",
                        tmp.display(),
                        path.display()
                    );
                }
            }
            Err(e) => log::error!("save persistence: write {} FAILED: {e}", tmp.display()),
        }
    }
}

/// R5 (SWEEP): one warning per unmapped AP item id -- the grant loop would otherwise drop the
/// item with no trace, every session, on every replay.
static UNMAPPED_LOGGED: std::sync::Mutex<Option<HashSet<i64>>> = std::sync::Mutex::new(None);

fn warn_unmapped_once(name: &str, ap_id: i64) {
    let mut guard = UNMAPPED_LOGGED.lock().unwrap();
    if guard.get_or_insert_with(HashSet::new).insert(ap_id) {
        log::warn!(
            "item '{name}' (ap id {ap_id}) has no ER mapping -- NOT granted (contract drift?)"
        );
    }
}

fn save_file_path(seed: &str, name: &str) -> Option<PathBuf> {
    let dir = match shared::utils::mod_directory() {
        Ok(d) => d,
        Err(e) => {
            // R7 (SWEEP): was `.ok()?` -- save persistence silently never armed.
            log::error!(
                "save persistence UNAVAILABLE ({e}) -- watermarks will reset every session"
            );
            return None;
        }
    };
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

/// `{ "<i64>": <u32> }` slot_data object -> `i64 -> u32`. Tolerant: skips malformed entries. Used by
/// the shop system (locationFlags / shopRowFlags).
fn i64_to_u32_map(v: Option<&Value>) -> HashMap<i64, u32> {
    let mut m = HashMap::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let (Ok(key), Some(value)) = (k.parse::<i64>(), val.as_u64()) {
                m.insert(key, value as u32);
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    /// The slot_data "versions" contract band the apworld emits. MUST stay in lockstep with
    /// Archipelago/worlds/eldenring/__init__.py fill_slot_data "versions" — when the apworld
    /// bumps its band, bump this const AND `[package] version` in this crate's Cargo.toml together.
    const EXPECTED_SLOT_DATA_VERSIONS: &str = ">=0.1.0-beta.4 <0.1.0-beta.5";

    /// Connect-gate lockstep guard: the crate's own version must sit INSIDE the apworld band.
    /// (er-semver orders a bare release 0.1.0 ABOVE every 0.1.0-beta.*, so a plain "0.1.0"
    /// package version made version_gate return Some(false) on every connect — the exact
    /// bug this test pins down.)
    #[test]
    fn client_version_is_inside_apworld_contract_band() {
        let sd = serde_json::json!({ "versions": EXPECTED_SLOT_DATA_VERSIONS });
        assert_eq!(
            er_logic::version::version_gate(&sd, env!("CARGO_PKG_VERSION")),
            Some(true),
            "CARGO_PKG_VERSION {} is outside the apworld slot_data band {EXPECTED_SLOT_DATA_VERSIONS}",
            env!("CARGO_PKG_VERSION"),
        );
    }
}
