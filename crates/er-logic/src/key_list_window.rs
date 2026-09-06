//! `key_list_window` -- how far the KEY-ITEM inventory walk must look, and why `key_items_len`
//! is not that number.
//!
//! # The model change (2026-09-06, Tako's logs)
//!
//! The pinned `eldenring` crate documents `key_items_len` as a high-water mark ("the length
//! currently in use ... the inventory can have gaps ... and this counts those gaps as part of the
//! length"), and its `key_entries()` slice is bounded by it. Every client walk that answers "does
//! the player hold this key item?" -- `reconcile_io::inventory_has_goods`, the forensics
//! candidates walk, the protector and storage walks, the start-item backfill -- inherited that
//! bound.
//!
//! Two days of Tako's client logs (LOG1 2026-09-05, LOG2 2026-09-06) refute the high-water model:
//!
//! * In 50 of 50 stall-forensics lines the NORMAL list, which the crate walks to CAPACITY, reads
//!   `non_empty == normal_items_len` exactly, including after decreases -- so that `len` is an
//!   OCCUPANCY COUNT, not a high-water mark.
//! * In every one of those lines the KEY list, walked to `len`, reads `non_empty < key_items_len`
//!   -- and the shortfall is exactly the number of entries that went missing: 35/26 after a Twin
//!   Maiden Husks visit (nine hidden, Morgott's Great Rune among them), 43/39 after another
//!   (four hidden: Radahn's and Morgott's runes plus two), 25/23 after a Miriel hand-in.
//! * Three fully-logged windows show `k` key-item receipts raising `non_empty` by `2k`: each new
//!   entry fills a hole AND lifts `len` past one previously hidden entry (LOG2 L13760 -> L14040:
//!   42/37 -> 45/43 after three receipts, and Morgott's rune reappeared in the candidates list
//!   with NO grant in that epoch).
//!
//! Put together: `key_items_len` counts LIVE entries. The game fills the first free slot on an add
//! and clears the slot on a remove, so an NPC hand-in that consumes a lower-index key item (a bell
//! bearing at the Twin Maiden Husks, a prayerbook at Miriel) leaves a hole, decrements `len`, and
//! the highest-index live entry -- the most recently received key item -- now sits at an index
//! `>= len`. The game still holds it (its own slot arithmetic runs over `[0, capacity)`), so every
//! client re-add of that maxNum=1 row is refused, the reconciler reads it back absent, parks it
//! after `MAX_GRANT_ATTEMPTS`, and re-arms on every world edge: the "vanished Great Rune that goes
//! INERT 1-2 s after every load for days" cadence. Nothing vanished. The walk stopped short.
//!
//! Per the world CONTRIBUTING rule -- when the data contradicts the model, the MODEL changes --
//! the key list is walked to CAPACITY, exactly as the crate already walks the normal list, with a
//! sanity cap so a bad read fails loud instead of walking garbage. The multiplay key list is left
//! len-bounded: Great Runes never live there, and its capacity reads 3269 on every build seen.
//!
//! Everything decision-bearing is here and host-tested; `reconcile_io` is the one production
//! caller and does nothing but apply [`walk_bound`] to a raw slice.

/// Largest key-list capacity this walk will trust. Vanilla reads 384; anything above this is a
/// misread (a stale inventory object, a layout drift) and must fall back rather than be walked.
pub const KEY_LIST_SANE_CAP: u32 = 4096;

/// How many slots of the key list a possession walk visits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkBound {
    /// Every allocated slot `[0, capacity)` -- the shape the game's own slot arithmetic uses.
    Capacity(usize),
    /// The capacity read failed sanity; walk `[0, len)` as before. The caller says so ONCE per
    /// session, because under this bound entries beyond `len` are invisible again.
    LenFallback { len: u32, capacity: u32 },
}

impl WalkBound {
    /// The number of slots to walk under this bound.
    pub fn slots(self) -> usize {
        match self {
            WalkBound::Capacity(c) => c,
            WalkBound::LenFallback { len, .. } => len as usize,
        }
    }
}

/// Decide the walk bound from the two fields the game exposes.
///
/// Capacity wins whenever it is sane: non-zero, at most [`KEY_LIST_SANE_CAP`], and not smaller
/// than `len` (a `len > capacity` read means one of the two fields is not what we think it is).
pub fn walk_bound(len: u32, capacity: u32) -> WalkBound {
    if capacity > 0 && capacity <= KEY_LIST_SANE_CAP && len <= capacity {
        WalkBound::Capacity(capacity as usize)
    } else {
        WalkBound::LenFallback { len, capacity }
    }
}

/// How many live entries the occupancy model says sit at an index `>= len`: every hole inside
/// `[0, len)` displaces exactly one live entry beyond it. This is the number a len-bounded walk
/// cannot see; the forensics line prints it next to the entries actually found there so the two
/// can be compared on the next report.
pub fn hidden_beyond_len(len: u32, non_empty_in_len: u32) -> u32 {
    len.saturating_sub(non_empty_in_len)
}

/// One live entry found at an index `>= len`, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BeyondLenEntry {
    pub index: usize,
    /// Category-stripped param row.
    pub row: i32,
    /// The item's category nibble (goods = 4), so a same-row entry of another category is not
    /// mistaken for the good.
    pub category_nibble: u8,
    pub quantity: u32,
}

/// Render the beyond-len entries for a log line: `[idx:row/cat xqty, ...] (+N more)`, or `NONE`.
/// Wording lives here so the exact sentence is host-tested and cannot drift into implying more
/// than the walk saw.
pub fn format_beyond_len(entries: &[BeyondLenEntry], cap: usize) -> String {
    if entries.is_empty() {
        return "NONE".to_string();
    }
    let shown: Vec<String> = entries
        .iter()
        .take(cap)
        .map(|e| {
            format!(
                "{}:{}/{} x{}",
                e.index, e.row, e.category_nibble, e.quantity
            )
        })
        .collect();
    let extra = entries.len().saturating_sub(cap);
    if extra > 0 {
        format!("[{}] (+{extra} more)", shown.join(", "))
    } else {
        format!("[{}]", shown.join(", "))
    }
}

/// A change in the key list's `(len, non_empty_in_len)` pair between two samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyListDelta {
    pub len_before: u32,
    pub len_after: u32,
    pub non_empty_before: u32,
    pub non_empty_after: u32,
}

impl KeyListDelta {
    /// Hidden count before and after, per [`hidden_beyond_len`].
    pub fn hidden(&self) -> (u32, u32) {
        (
            hidden_beyond_len(self.len_before, self.non_empty_before),
            hidden_beyond_len(self.len_after, self.non_empty_after),
        )
    }
}

/// `Some` when the sampled pair moved. Used while the ESD talk gate is closed, so the log can
/// date a `len` decrement to the talk command that was dispatched last.
pub fn key_list_delta(prev: (u32, u32), now: (u32, u32)) -> Option<KeyListDelta> {
    if prev == now {
        return None;
    }
    Some(KeyListDelta {
        len_before: prev.0,
        len_after: now.0,
        non_empty_before: prev.1,
        non_empty_after: now.1,
    })
}

/// Once-per-world-epoch latch for the key-list line: log when the epoch differs from the one
/// last logged (or nothing has been logged yet).
pub fn should_log_epoch(last: Option<u64>, now: u64) -> bool {
    last != Some(now)
}

/// A test model of the game's key list under the OCCUPANCY model: `len` counts live entries, an
/// add fills the first free slot, a remove clears the slot. `slots` holds `(row, quantity)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyListView {
    pub len: u32,
    pub capacity: u32,
    pub slots: Vec<Option<(i32, u32)>>,
}

impl KeyListView {
    /// A list whose live entries sit exactly where `rows` puts them (`None` = hole). `len` is the
    /// live count, per the model.
    pub fn from_slots(capacity: u32, slots: Vec<Option<(i32, u32)>>) -> Self {
        let len = slots.iter().filter(|s| s.is_some()).count() as u32;
        let mut view = KeyListView {
            len,
            capacity,
            slots,
        };
        view.slots.resize(capacity as usize, None);
        view
    }

    /// The `(index, row)` pairs a walk under `bound` reports.
    pub fn visible(&self, bound: WalkBound) -> Vec<(usize, i32)> {
        self.slots
            .iter()
            .take(bound.slots())
            .enumerate()
            .filter_map(|(i, s)| s.map(|(row, _)| (i, row)))
            .collect()
    }

    /// Whether a len-bounded walk sees `row`.
    pub fn visible_under_len(&self, row: i32) -> bool {
        self.visible(WalkBound::LenFallback {
            len: self.len,
            capacity: self.capacity,
        })
        .iter()
        .any(|&(_, r)| r == row)
    }

    /// Whether a capacity-bounded walk sees `row`.
    pub fn visible_under_capacity(&self, row: i32) -> bool {
        self.visible(walk_bound(self.len, self.capacity))
            .iter()
            .any(|&(_, r)| r == row)
    }

    /// Live entries inside `[0, len)` -- the `non_empty key=N` figure the forensics line prints.
    pub fn non_empty_in_len(&self) -> u32 {
        self.slots
            .iter()
            .take(self.len as usize)
            .filter(|s| s.is_some())
            .count() as u32
    }

    /// Live entries at an index `>= len`.
    pub fn beyond_len(&self) -> Vec<BeyondLenEntry> {
        self.slots
            .iter()
            .enumerate()
            .skip(self.len as usize)
            .filter_map(|(i, s)| {
                s.map(|(row, quantity)| BeyondLenEntry {
                    index: i,
                    row,
                    category_nibble: 4,
                    quantity,
                })
            })
            .collect()
    }

    /// The game's remove: clear the slot, decrement the count. `false` if the row is not held.
    pub fn remove_row(&mut self, row: i32) -> bool {
        match self
            .slots
            .iter()
            .position(|s| s.map(|(r, _)| r) == Some(row))
        {
            Some(i) => {
                self.slots[i] = None;
                self.len = self.len.saturating_sub(1);
                true
            }
            None => false,
        }
    }

    /// The game's add: first free slot, count up. `None` if the list is full.
    pub fn add_row(&mut self, row: i32, quantity: u32) -> Option<usize> {
        let i = self.slots.iter().position(|s| s.is_none())?;
        self.slots[i] = Some((row, quantity));
        self.len += 1;
        Some(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MORGOTT: i32 = 8150;
    const RADAHN: i32 = 8149;
    const MOHG: i32 = 8152;
    const CAP: u32 = 384;

    /// A key list with `live_in_window` live rows inside `[0, window)`, `holes` empty slots
    /// scattered inside it, and `beyond` live rows placed right after the window.
    fn view(window: usize, holes: &[usize], beyond: &[i32]) -> KeyListView {
        let mut slots: Vec<Option<(i32, u32)>> = (0..window)
            .map(|i| {
                if holes.contains(&i) {
                    None
                } else {
                    Some((9000 + i as i32, 1))
                }
            })
            .collect();
        for &row in beyond {
            slots.push(Some((row, 1)));
        }
        KeyListView::from_slots(CAP, slots)
    }

    /// THE MOTIVATING CASE (LOG2 L13760, 2026-09-06 16:30): `key 42/384`, 37 non-empty inside
    /// the window, five entries hidden beyond it -- Morgott's Great Rune among them. The len walk
    /// reports it absent; the capacity walk finds it at index 43; the model's hidden count is 5.
    #[test]
    fn wild_20260906_tako_morgott_rune_sits_beyond_key_items_len_after_husks_handin_replay() {
        // 42 slots in the window with 5 holes = 37 live; 5 live beyond, Morgott's at index 43,
        // Radahn's at index 46.
        let v = view(
            42,
            &[3, 11, 20, 27, 38],
            &[8930, MORGOTT, 8931, 8935, RADAHN],
        );
        assert_eq!((v.len, v.capacity), (42, CAP), "the forensics pair");
        assert_eq!(v.non_empty_in_len(), 37);
        assert_eq!(hidden_beyond_len(v.len, v.non_empty_in_len()), 5);

        assert!(
            !v.visible_under_len(MORGOTT),
            "the len-bounded walk is what the client shipped, and it cannot see the rune"
        );
        assert!(
            v.visible_under_capacity(MORGOTT),
            "the capacity walk sees the entry the game still holds"
        );
        assert_eq!(
            v.visible(walk_bound(v.len, v.capacity))
                .iter()
                .find(|&&(_, r)| r == MORGOTT)
                .map(|&(i, _)| i),
            Some(43)
        );
        assert_eq!(
            v.beyond_len().iter().map(|e| e.row).collect::<Vec<_>>(),
            vec![8930, MORGOTT, 8931, 8935, RADAHN]
        );
    }

    /// LOG2 L13760 -> L14040 (16:30 -> 16:34): three key-item receipts, no rune grant, and
    /// Morgott's rune is back in the candidates list while Radahn's is still missing. Under the
    /// model three adds fill three holes and lift `len` 42 -> 45: index 43 is inside, 46 is not.
    /// The capacity walk saw both the whole time; recoveries are reveals, not landings.
    #[test]
    fn wild_20260906_tako_three_key_adds_regrow_len_and_reexpose_morgott_before_radahn_replay() {
        let mut v = view(
            42,
            &[3, 11, 20, 27, 38],
            &[8930, MORGOTT, 8931, 8935, RADAHN],
        );
        for row in [8936, 8937, 8938] {
            let i = v.add_row(row, 1).expect("room");
            assert!(i < 42, "an add fills the first hole, not the tail");
        }
        assert_eq!(
            (v.len, v.non_empty_in_len()),
            (45, 43),
            "the L14040 pair: key 45/384, 43"
        );
        assert!(v.visible_under_len(MORGOTT), "index 43 < 45: revealed");
        assert!(!v.visible_under_len(RADAHN), "index 46 >= 45: still hidden");
        assert!(v.visible_under_capacity(MORGOTT) && v.visible_under_capacity(RADAHN));
        assert_eq!(hidden_beyond_len(45, 43), 2);
    }

    /// LOG1 03:24:23 (`key 25/384`, 23 non-empty) after the Miriel hand-in: Mohg's rune, received
    /// four minutes earlier, is the newest key item and the first to fall outside the window. The
    /// 03:26:30 sweep delivered ten checks; two key items among them are enough to lift `len`
    /// past it, which is when the rune "re-landed" in the log with no grant having been made.
    #[test]
    fn wild_20260905_tako_mohg_rune_hidden_at_miriel_then_visible_after_len_regrows_replay() {
        let mut v = view(25, &[4, 17], &[8926, MOHG]);
        assert_eq!((v.len, v.non_empty_in_len()), (25, 23));
        assert_eq!(hidden_beyond_len(25, 23), 2);
        assert!(!v.visible_under_len(MOHG));
        assert!(v.visible_under_capacity(MOHG));
        v.add_row(8927, 1);
        v.add_row(8928, 1);
        assert_eq!(v.len, 27);
        assert!(
            v.visible_under_len(MOHG),
            "index 26 < 27: back in the window, no grant needed"
        );
    }

    /// Every `(len, non_empty)` pair the two logs printed, and the hidden count each implies.
    #[test]
    fn hidden_count_matches_every_tako_forensics_pair() {
        for (len, non_empty, hidden) in [
            (43, 39, 4),
            (42, 37, 5),
            (45, 43, 2),
            (35, 26, 9),
            (25, 23, 2),
            (44, 43, 1),
        ] {
            assert_eq!(
                hidden_beyond_len(len, non_empty),
                hidden,
                "{len}/{non_empty}"
            );
        }
        assert_eq!(hidden_beyond_len(10, 12), 0, "never negative");
    }

    /// The hand-in itself: remove a low-index key item and the highest-index live entry drops
    /// out of the len window untouched. This is the whole mechanism in four lines.
    #[test]
    fn a_handin_that_clears_a_lower_slot_hides_the_newest_key_item_from_a_len_walk() {
        let mut v = view(30, &[], &[]);
        v.add_row(MORGOTT, 1); // the rune, newest, at index 30
        assert_eq!(v.len, 31);
        assert!(v.visible_under_len(MORGOTT));
        assert!(v.remove_row(9005), "offer a bell bearing at index 5");
        assert_eq!(v.len, 30);
        assert!(!v.visible_under_len(MORGOTT), "index 30 >= len 30");
        assert!(v.visible_under_capacity(MORGOTT));
        assert_eq!(v.beyond_len().len(), 1);
    }

    #[test]
    fn walk_bound_uses_capacity_for_the_vanilla_shape() {
        assert_eq!(walk_bound(42, 384), WalkBound::Capacity(384));
        assert_eq!(walk_bound(0, 3269), WalkBound::Capacity(3269));
        assert_eq!(walk_bound(384, 384), WalkBound::Capacity(384));
        assert_eq!(WalkBound::Capacity(384).slots(), 384);
    }

    #[test]
    fn walk_bound_falls_back_to_len_when_capacity_is_insane() {
        assert_eq!(
            walk_bound(42, 0),
            WalkBound::LenFallback {
                len: 42,
                capacity: 0
            }
        );
        assert_eq!(
            walk_bound(42, 10),
            WalkBound::LenFallback {
                len: 42,
                capacity: 10
            },
            "len > capacity: one of the fields is not what we think"
        );
        assert_eq!(
            walk_bound(42, 5000),
            WalkBound::LenFallback {
                len: 42,
                capacity: 5000
            }
        );
        assert_eq!(walk_bound(42, 5000).slots(), 42);
    }

    #[test]
    fn format_beyond_len_names_rows_and_caps_the_list() {
        assert_eq!(format_beyond_len(&[], 16), "NONE");
        let e = |index, row| BeyondLenEntry {
            index,
            row,
            category_nibble: 4,
            quantity: 1,
        };
        assert_eq!(
            format_beyond_len(&[e(43, MORGOTT), e(46, RADAHN)], 16),
            "[43:8150/4 x1, 46:8149/4 x1]"
        );
        assert_eq!(
            format_beyond_len(&[e(43, MORGOTT), e(46, RADAHN), e(47, 8930)], 2),
            "[43:8150/4 x1, 46:8149/4 x1] (+1 more)"
        );
    }

    /// LOG2 L6219 -> L10314: `key 44/384` 43 non-empty before the Husks visit, `43/384` 39 after.
    #[test]
    fn key_list_delta_reports_the_husks_shape_44_43_to_43_39() {
        let d = key_list_delta((44, 43), (43, 39)).expect("moved");
        assert_eq!(d.hidden(), (1, 4));
        assert_eq!((d.len_before, d.len_after), (44, 43));
        assert_eq!(
            key_list_delta((44, 43), (44, 43)),
            None,
            "no delta, no line"
        );
    }

    #[test]
    fn the_epoch_latch_logs_once_per_epoch() {
        assert!(should_log_epoch(None, 1));
        assert!(!should_log_epoch(Some(1), 1));
        assert!(should_log_epoch(Some(1), 2));
    }
}
