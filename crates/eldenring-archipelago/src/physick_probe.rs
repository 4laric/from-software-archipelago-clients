//! physick_probe -- READ-ONLY diagnostic for #334. No writes, ever. Gated on `ER_PHYSICK_PROBE`.
//!
//! ## v1 found the slots. Read this before trusting that they are the whole story.
//!
//! Run 2026-08-03 22:05, four log lines, both phase-2 questions answered:
//!
//! ```text
//! APPEARED PlayerGameData+0x694 (= EquipGameData+0x3e4) = 0x40002afa -> row 11002, FullID (plain)
//! APPEARED PlayerGameData+0x698 (= EquipGameData+0x3e8) = 0x40002afc -> row 11004, FullID (plain)
//! CLEARED  PlayerGameData+0x694 (= EquipGameData+0x3e4) = 0x40002afa -> row 11002, FullID (plain)
//! ```
//!
//! * **WHERE**: [`PHYSICK_SLOT_A`] / [`PHYSICK_SLOT_B`] -- the 16 bytes straight after
//!   `equipment_entries`. The crate mistypes the region as `unk3e0: usize, unk3e8: usize`; the pair
//!   is really `u32`s straddling that boundary.
//! * **NOT REFCOUNTED**: both held plain GOODS FullIDs, the same representation as
//!   `equipment_entries`, which the crate documents as having no refcounting and being written
//!   directly. The tears were granted 18s and 25s BEFORE their offsets moved, and this probe scans
//!   every 500ms -- so the write is the MIX, not the acquisition.
//!
//! ## 🛑 Why v2 exists: "two dwords move" is NOT "only two dwords move"
//!
//! v1 searched for tear-VALUED words. That makes it structurally blind to any coupled
//! representation that does not hold the row -- an inventory index, a gaitem handle, a derived
//! SpEffect id. `auto_equip` learned this the expensive way: an equipped piece turned out to be
//! FOUR coupled reps, and the fourth (`EquipGameData + 0x08 + slot*4`, the inventory index the menu
//! reads) was found ONLY by diffing the whole header. Write three of four and the weapon is in your
//! hand while every menu slot renders empty.
//!
//! The flask has burned this project on exactly this shape before: raising potency by an in-place
//! item-id swap CTD'd on death, because ER mirrors flask state across the inventory entry, the
//! equipped/quickslot reference AND the global GaItem, and death's refill hit the half-updated
//! state. A physick mixture written into 0x3E4 alone could display correctly and do nothing, or
//! display nothing and work, or crash one interaction later.
//!
//! So v2 diffs the WHOLE of `PlayerGameData` (which carries `EquipGameData` inline) across a slot
//! event and reports every dword that moved, whatever it holds.
//!
//! ### The noise floor calibrates itself
//!
//! A raw dword diff of live player state is mostly HP, stamina, timers and gauges. Rather than
//! hand-maintaining an ignore list, the probe watches the scans where NOTHING mixed and records
//! every offset that moved on its own as [`State::volatile`]. When a slot event finally lands, the
//! volatile offsets are subtracted and reported only as a count. Standing at the grace for a few
//! seconds before mixing IS the calibration -- no extra step.
//!
//! ⚠️ The one thing this can hide: a genuine coupled rep that also churns idly would be filtered.
//! The suppressed count is logged so that stays visible rather than silent.
//!
//! ## 🛑 The other open question: what does an EMPTY slot hold?
//!
//! v1's `CLEARED` line deliberately did not re-read the offset, so we know a slot stopped holding a
//! tear and not what replaced it -- `0`? `-1`? `0x40000000`? "First empty slot, else clobber the
//! lowest" cannot be implemented without that value. v2 dumps both slots RAW and unconditionally on
//! every slot event, before and after, so the sentinel reads straight off the log.
//!
//! ## How to run it
//!
//! `ER_PHYSICK_PROBE=1`, then at a Site of Grace:
//!   1. load in and **stand still ~10s** (this is the noise calibration);
//!   2. mix a tear;
//!   3. mix a DIFFERENT tear;
//!   4. unmix.
//!
//! ## Safety
//!
//! Every read is inline in a live, crate-typed object. No guessed pointer is ever dereferenced --
//! `scaling_probe` v1 faulted doing exactly that on `base+0x20` (`0x1_0000_0001`, packed data, not a
//! pointer).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Instant;

use eldenring::cs::{
    EquipGameData, EquipParamGoods, GaitemHandle, GameDataMan, PlayerGameData, SoloParamRepository,
};
use fromsoftware_shared::FromStatic;

/// `GaitemHandle` is a single-`u32` bitfield newtype whose field the crate keeps private. The raw
/// value has to be read through the pointer; this assert is what makes that read sound, and fails
/// the build instead of silently reading half a wider struct if the crate ever reshapes it.
const _: () = assert!(size_of::<GaitemHandle>() == 4);

/// First physick mixture slot, `EquipGameData`-relative. Measured 2026-08-03 (see module docs), not
/// guessed: the crate calls this region `unk3e0`/`unk3e8` and types it as two `usize`s.
pub const PHYSICK_SLOT_A: usize = 0x3E4;
/// Second physick mixture slot, `EquipGameData`-relative.
pub const PHYSICK_SLOT_B: usize = 0x3E8;

/// Both slots must land inside `EquipGameData`, or the offsets are stale for the current crate.
const _: () = assert!(PHYSICK_SLOT_B + 4 <= size_of::<EquipGameData>());

/// Minimum gap between scans. The mixing menu is not a per-frame event.
const SCAN_INTERVAL_MS: u128 = 500;

/// Idle scans to watch before the volatile set is considered calibrated. 20 x 500ms = ~10s.
const CALIBRATION_SCANS: u32 = 20;

/// Never print more than this many changed offsets for one event; a pathological diff must not
/// become the overlay render spam.
const MAX_REPORTED: usize = 64;

/// Which representation a tear-valued hit was found in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Form {
    /// The value equals the tear's GOODS FullID (`0x4000_2AFA`). Plain data.
    FullId,
    /// The value equals the bare param row (`11002`). Plain data.
    ParamRow,
    /// The value equals the raw `GaitemHandle` of the tear's inventory entry. REFCOUNTED.
    Handle,
}

impl Form {
    fn label(self) -> &'static str {
        match self {
            Form::FullId => "FullID (plain)",
            Form::ParamRow => "param row (plain)",
            Form::Handle => "GAITEM HANDLE (refcounted)",
        }
    }
}

/// One scanned region, named for the log.
struct Window {
    name: &'static str,
    base: usize,
    len: usize,
}

/// Tear-valued hits: window name + byte offset -> the word that was there.
type Snapshot = BTreeMap<(&'static str, usize), u32>;

struct State {
    last_scan: Instant,
    armed_logged: bool,
    baseline_logged: bool,
    /// v1: semantic hits, by window and offset.
    seen: Snapshot,
    /// v2: the whole `PlayerGameData` as dwords, from the previous scan.
    window: Vec<u32>,
    /// v2: `PlayerGameData` dword offsets that move on their own, with nothing being mixed.
    volatile: BTreeSet<usize>,
    idle_scans: u32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// `ER_PHYSICK_PROBE=1|true|on` arms the probe. Absent => hard no-op.
fn armed() -> bool {
    matches!(
        std::env::var("ER_PHYSICK_PROBE").ok().as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

/// The raw `u32` behind a `GaitemHandle`.
///
/// SAFETY: `GaitemHandle` is a single-`u32` tuple struct (asserted above), so the pointer cast reads
/// exactly the one field. The crate exposes only bitfield accessors, none of which yields the whole
/// word.
fn handle_raw(handle: GaitemHandle) -> u32 {
    unsafe { *(&handle as *const GaitemHandle as *const u32) }
}

/// `PlayerGameData`-relative offset of an `EquipGameData`-relative one.
fn equip_abs(off: usize) -> usize {
    std::mem::offset_of!(PlayerGameData, equipment) + off
}

/// Name an offset against the struct it falls in, so a hit is immediately actionable.
fn label_offset(window: &str, off: usize) -> String {
    let equip_off = std::mem::offset_of!(PlayerGameData, equipment);
    if window == "PlayerGameData"
        && (equip_off..equip_off + size_of::<EquipGameData>()).contains(&off)
    {
        format!(
            "PlayerGameData+{off:#x} (= EquipGameData+{:#x})",
            off - equip_off
        )
    } else {
        format!("{window}+{off:#x}")
    }
}

/// Read one `u32` out of a window.
///
/// SAFETY: `base` is a live object and `off + 4 <= len`, so the read stays inside it. No
/// dereference.
fn word_at(base: usize, off: usize) -> u32 {
    unsafe { ((base + off) as *const u32).read_unaligned() }
}

/// Read-only scan. Call once per reconcile tick from `update_live`; self-gating and self-throttling.
pub fn tick() {
    if !armed() {
        return;
    }

    let (Ok(gdm), Ok(repo)) = (unsafe { GameDataMan::instance() }, unsafe {
        SoloParamRepository::instance()
    }) else {
        return;
    };

    let mut guard = STATE.lock().unwrap();
    let state = guard.get_or_insert_with(|| State {
        // Subtracting the interval means the first tick scans immediately rather than after a delay.
        last_scan: Instant::now() - std::time::Duration::from_millis(SCAN_INTERVAL_MS as u64 + 1),
        armed_logged: false,
        baseline_logged: false,
        seen: Snapshot::new(),
        window: Vec::new(),
        volatile: BTreeSet::new(),
        idle_scans: 0,
    });
    if state.last_scan.elapsed().as_millis() < SCAN_INTERVAL_MS {
        return;
    }
    state.last_scan = Instant::now();

    if !state.armed_logged {
        log::info!(
            "[physick-probe] v2 armed. At a grace: stand still ~10s (noise calibration), then mix a \
             tear, mix a DIFFERENT tear, then unmix. Slots are EquipGameData+{PHYSICK_SLOT_A:#x} / \
             +{PHYSICK_SLOT_B:#x}."
        );
        state.armed_logged = true;
    }

    let pgd: &PlayerGameData = gdm.main_player_game_data.as_ref();
    let pgd_base = pgd as *const PlayerGameData as usize;

    // --- 1. The dword diff. Runs unconditionally: it must not depend on owning a tear. -----------
    let cur: Vec<u32> = (0..size_of::<PlayerGameData>() / 4)
        .map(|i| word_at(pgd_base, i * 4))
        .collect();

    let slot_a = equip_abs(PHYSICK_SLOT_A);
    let slot_b = equip_abs(PHYSICK_SLOT_B);

    if state.window.len() == cur.len() {
        let changed: Vec<usize> = cur
            .iter()
            .zip(state.window.iter())
            .enumerate()
            .filter(|(_, (now, before))| now != before)
            .map(|(i, _)| i * 4)
            .collect();

        let slot_event = changed.contains(&slot_a) || changed.contains(&slot_b);

        if slot_event {
            log::info!(
                "[physick-probe] === SLOT EVENT === slots now A={:#010x} B={:#010x} (were \
                 A={:#010x} B={:#010x}) -- an UNOCCUPIED slot is whichever of these is not a tear \
                 FullID; that value is the EMPTY SENTINEL.",
                cur[slot_a / 4],
                cur[slot_b / 4],
                state.window[slot_a / 4],
                state.window[slot_b / 4],
            );
            let non_slot = changed.iter().filter(|o| **o != slot_a && **o != slot_b);
            let others: Vec<usize> = non_slot
                .clone()
                .copied()
                .filter(|o| !state.volatile.contains(o))
                .collect();
            let suppressed = non_slot.count() - others.len();
            log::info!(
                "[physick-probe] {} other dword(s) moved in the same window ({suppressed} \
                 suppressed as idle-volatile, from {} calibration scan(s)). ANY of these could be a \
                 coupled rep -- auto_equip needed FOUR.",
                others.len(),
                state.idle_scans,
            );
            for off in others.iter().take(MAX_REPORTED) {
                log::info!(
                    "[physick-probe]   MOVED {} : {:#010x} -> {:#010x}",
                    label_offset("PlayerGameData", *off),
                    state.window[off / 4],
                    cur[off / 4],
                );
            }
            if others.len() > MAX_REPORTED {
                log::info!(
                    "[physick-probe]   ... {} more suppressed by MAX_REPORTED",
                    others.len() - MAX_REPORTED
                );
            }
        } else {
            // Nothing mixed -> everything that moved is noise. This IS the calibration.
            state.volatile.extend(changed.iter().copied());
            state.idle_scans += 1;
            if state.idle_scans == CALIBRATION_SCANS {
                log::info!(
                    "[physick-probe] noise floor calibrated: {} idle-volatile offset(s) over {} \
                     scan(s). Mix a tear now.",
                    state.volatile.len(),
                    state.idle_scans,
                );
            }
        }
    }
    state.window = cur;

    // --- 2. The semantic scan: which tear, in which representation. -------------------------------
    // Tears are KEY items, so the key list is walked FIRST and is not optional -- `normal_entries()`
    // alone (what auto_equip uses) would find none of them.
    let inventory = &pgd.equipment.equip_inventory_data.items_data;
    let mut wanted: BTreeMap<u32, (u32, Form)> = BTreeMap::new(); // value -> (row, form)
    for entry in inventory
        .current_key_entries()
        .iter()
        .chain(inventory.normal_entries().iter())
        .filter_map(|slot| slot.as_option())
    {
        let full = entry.item_id.into_inner();
        let Some(row) = er_logic::physick::goods_row(full as i32) else {
            continue;
        };
        let Some(param) = repo.get::<EquipParamGoods>(row) else {
            continue;
        };
        if !er_logic::physick::is_tear(param.goods_type(), param.sort_id()) {
            continue;
        }
        wanted.insert(full, (row, Form::FullId));
        wanted.insert(row, (row, Form::ParamRow));
        wanted.insert(handle_raw(entry.gaitem_handle), (row, Form::Handle));
    }

    if wanted.is_empty() {
        // The dword diff above still runs -- only the semantic labelling needs an owned tear.
        return;
    }

    let magic = &*pgd.equipment.equip_magic_data;
    let windows = [
        Window {
            name: "PlayerGameData",
            base: pgd_base,
            len: size_of::<PlayerGameData>(),
        },
        Window {
            name: "EquipMagicData",
            base: magic as *const _ as usize,
            len: size_of_val(magic),
        },
        Window {
            name: "GameDataMan",
            base: gdm as *const GameDataMan as usize,
            len: size_of::<GameDataMan>(),
        },
    ];

    let mut now: Snapshot = Snapshot::new();
    for w in windows.iter() {
        let mut off = 0usize;
        while off + 4 <= w.len {
            let word = word_at(w.base, off);
            if wanted.contains_key(&word) {
                now.insert((w.name, off), word);
            }
            off += 4;
        }
    }

    // Total by construction: a word recorded by a PREVIOUS scan can be absent from `wanted` now (the
    // player drank or dropped that tear), and indexing would panic inside a diagnostic.
    let describe = |window: &'static str, off: usize, word: u32| -> String {
        let head = format!("{} = {word:#010x}", label_offset(window, off));
        match wanted.get(&word) {
            None => format!("{head} -> no longer an owned tear"),
            Some(&(row, form)) => {
                let canonical = er_logic::physick::canonical_row(row);
                let dup = if canonical == row {
                    String::new()
                } else {
                    format!(" [near-duplicate row; AP catalog knows {canonical}]")
                };
                format!("{head} -> row {row}, {}{dup}", form.label())
            }
        }
    };

    if !state.baseline_logged {
        log::info!(
            "[physick-probe] BASELINE: {} tear-valued word(s) across {} window(s); searching for {} \
             distinct value(s).",
            now.len(),
            windows.len(),
            wanted.len()
        );
        for (&(window, off), &word) in now.iter() {
            log::info!("[physick-probe]   {}", describe(window, off, word));
        }
        state.baseline_logged = true;
        state.seen = now;
        return;
    }

    for (&(window, off), &word) in now.iter() {
        match state.seen.get(&(window, off)) {
            Some(&prev) if prev == word => {}
            Some(&prev) => log::info!(
                "[physick-probe] CHANGED {} (was {prev:#010x})",
                describe(window, off, word)
            ),
            None => log::info!("[physick-probe] APPEARED {}", describe(window, off, word)),
        }
    }
    // CLEARED reports only what WAS there. The raw current value is in the SLOT EVENT line above.
    for (&(window, off), &word) in state.seen.iter() {
        if !now.contains_key(&(window, off)) {
            log::info!("[physick-probe] CLEARED  {}", describe(window, off, word));
        }
    }
    state.seen = now;
}
