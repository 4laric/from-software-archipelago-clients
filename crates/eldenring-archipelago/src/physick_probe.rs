//! physick_probe -- READ-ONLY diagnostic for #334 phase 2: WHERE the Flask of Wondrous Physick's two
//! mixture slots live, and WHETHER they are refcounted. No writes, ever. Gated on `ER_PHYSICK_PROBE`.
//!
//! ## Why a value-search and not an offset guess
//!
//! Phase 1 closed every static lead: zero EMEVD references to any tear row across all 589 decompiled
//! files, and the pinned `eldenring` crate has no physick struct, field or param table -- only a prose
//! mention of `WONDROUS_PHYSICK_TEAR` and `MenuType::PhysickMenu = 21`. The mixture is engine state,
//! so the only instrument left is to watch live memory change when the player mixes.
//!
//! Two of the three candidate structures are already FULLY mapped by the crate, and their sizes prove
//! there is no room for a hidden pair of slots:
//!
//! * `EquipItemData` = `vftable + quick_slots[10] + pouch_slots[6] + great_rune + 2 ptrs + i32 + unka4`
//!   = 0xA8, and `unka4` sits at 0xA4. Complete. (This one mattered: `great_rune` is a single
//!   `EquipDataItem` living beside the quick slots, i.e. EXACTLY the shape a physick pair would take.
//!   It is not there.)
//! * `EquipMagicData` = `vftable + ptr + entries[14] + selected_slot + unk84` = 0x88. Complete.
//!
//! What is left is the unnamed holes: `EquipGameData.unk60/unk68`, `unk3e0/unk3e8` and the 0xAC-byte
//! tail `unk404`, plus the holes in `PlayerGameData` itself. Rather than pick one, this probe scans
//! the WHOLE of `PlayerGameData` -- which contains `EquipGameData` (and therefore `EquipItemData`)
//! inline -- so every hole in both structs is covered in one pass and a negative result is a real
//! negative rather than "we looked in the wrong place".
//!
//! ## The refcount question is answered in the same pass
//!
//! A scan for tear PARAM ROWS alone cannot answer it, because the two representations do not share a
//! value:
//!
//! * plain data (`equipment_entries`, `equipment_param_ids`) stores the FullID `0x40002AFA` or the
//!   bare row `11002` -- searchable directly;
//! * a refcounted slot (`chr_asm.gaitem_handles`, `EquipDataItem.gaitem_handle`) stores a
//!   `GaitemHandle`, which is NOT derived from the row by any function. Searching for `11002` in a
//!   handle-shaped world finds nothing at all.
//!
//! So the probe searches for BOTH: the tear's FullID, its bare row, AND the raw `GaitemHandle` that
//! the tear's own inventory entry currently carries. Whichever form turns up tells us the shape, and
//! `Form` is reported on every hit. If the answer is `Handle`, a direct write leaks a reference and
//! destroys the item on the next menu interaction -- the mixture must then go through the game's own
//! copy-assign the way `auto_equip` does, and "I found two dwords that change" is NOT permission to
//! start writing.
//!
//! ## How to run it
//!
//! `ER_PHYSICK_PROBE=1`, then, at a Site of Grace:
//!   1. load in -- the probe logs a BASELINE (every offset currently holding a tear-ish value);
//!   2. mix a tear -- it logs only what CHANGED;
//!   3. mix a DIFFERENT tear, then UNMIX. One correlation is a coincidence; the slot is the offset
//!      that tracks every change.
//!
//! Delta-only logging is deliberate: a per-frame dump of a ~2.8 KB window would flood the log the way
//! the overlay render spam did, and the interesting event is the change, not the state.
//!
//! ## Not covered by this probe
//!
//! If the mixture lives behind a pointer into a separately-allocated block, this scan finds nothing.
//! That is still a useful result -- it eliminates the contiguous player state -- and the follow-up is
//! `GameDataMan`'s unnamed regions, which are scanned here too, and then `CSMenuMan` (`PhysickMenu`).
//! Following unknown pointer-looking words is deliberately NOT done: `scaling_probe` v1 faulted doing
//! exactly that on `base+0x20` (`0x1_0000_0001`, inline packed data, not a pointer).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use eldenring::cs::{
    EquipGameData, EquipParamGoods, GaitemHandle, GameDataMan, PlayerGameData, SoloParamRepository,
};
use fromsoftware_shared::FromStatic;

/// `GaitemHandle` is a single-`u32` bitfield newtype whose field the crate keeps private. The raw
/// value has to be read through the pointer; this assert is what makes that read sound, and fails the
/// build instead of silently reading half a wider struct if the crate ever reshapes it.
const _: () = assert!(size_of::<GaitemHandle>() == 4);

/// Minimum gap between scans. The mixing menu is not a per-frame event and a 2.8 KB scan plus a few
/// hundred param lookups has no business running at frame rate.
const SCAN_INTERVAL_MS: u128 = 500;

/// Which representation a hit was found in -- the whole point of the probe (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Form {
    /// The value equals the tear's GOODS FullID (`0x4000_2AFA`). Plain data, like
    /// `equipment_entries`.
    FullId,
    /// The value equals the bare param row (`11002`). Plain data, like `equipment_param_ids`.
    ParamRow,
    /// The value equals the raw `GaitemHandle` of the tear's inventory entry. REFCOUNTED -- do not
    /// write it directly.
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

/// What we found last scan: window name + byte offset -> the word that was there.
type Snapshot = BTreeMap<(&'static str, usize), u32>;

struct State {
    last_scan: Instant,
    baseline_logged: bool,
    seen: Snapshot,
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
/// SAFETY: `base` is a live object and `off + 4 <= len`, so the read stays inside it. No dereference.
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
        baseline_logged: false,
        seen: Snapshot::new(),
    });
    if state.last_scan.elapsed().as_millis() < SCAN_INTERVAL_MS {
        return;
    }
    state.last_scan = Instant::now();

    let pgd: &PlayerGameData = gdm.main_player_game_data.as_ref();

    // --- 1. What are we looking for? Every tear the player actually owns, in all three forms. -----
    // Tears are KEY items, so the key list is walked first and is NOT optional -- `normal_entries()`
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
        if !state.baseline_logged {
            log::info!(
                "[physick-probe] armed, but the player owns no physick tear -- nothing to search \
                 for. Pick one up (or receive one) and the probe starts."
            );
            state.baseline_logged = true;
        }
        return;
    }

    // --- 2. The windows. All inline reads of live, crate-typed objects; no guessed derefs. --------
    let magic = &*pgd.equipment.equip_magic_data;
    let windows = [
        Window {
            name: "PlayerGameData",
            base: pgd as *const PlayerGameData as usize,
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

    // --- 3. Scan. -------------------------------------------------------------------------------
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

    // --- 4. Report. Baseline once, then deltas only. ---------------------------------------------
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
             distinct value(s). Now mix a tear.",
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
    // CLEARED reports only what WAS there. Re-reading the offset would mean recovering the window
    // base by name, and the `unwrap_or(0)` that invites is a read of address `off` -- the same shape
    // that faulted `scaling_probe` v1. The delta is the datum; the current value is not needed.
    for (&(window, off), &word) in state.seen.iter() {
        if !now.contains_key(&(window, off)) {
            log::info!("[physick-probe] CLEARED  {}", describe(window, off, word));
        }
    }
    state.seen = now;
}
