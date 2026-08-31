//! fmg_groups.rs — the group-span arithmetic behind an FMG (MsgData) entry write.
//!
//! A runtime `MsgData` block is `GroupRecord[]` + a string-offset table. Each record is
//! `{stringIndexBase, firstId, lastId}` and means "ids `firstId..=lastId` live at offset-table
//! indices `stringIndexBase + (id - firstId)`". The game resolves an id by BINARY SEARCHING that
//! record array, so the array must be ASCENDING BY ID and NON-OVERLAPPING — that invariant is the
//! whole reason this module exists as testable code instead of an assumption in a comment.
//!
//! ## Why (issue #300, second half)
//!
//! `fmg_inject::extend_swap_overrides` could only REDIRECT the string slot of an id that already
//! lived in a vanilla group. An id in no group was silently unwritable, and the client's only answer
//! was a warning. That is fine for one category and wrong across three, because the three goods
//! categories DO NOT SHARE AN ID SET:
//!
//! ```text
//! shop-preview: 53 foreign/gem slot(s) ... -> extend-swap names=53 infos=25 captions=25
//! [WARN] FMG extend-swap(cat 20): 28 of 53 id(s) are in NO vanilla group ...
//! ```
//! (boblerrr, client `0.3.1 (f2ef85d3c920)`.)
//!
//! GoodsName (cat 10) covers every spare goods row the world hands out; GoodsInfo (20) and
//! GoodsCaption (24) cover only 25 of them. The world already knows this — `spare_goods.tsv` carries
//! an `fmg_full` column and emits the 25 complete rows FIRST — but a seed needing 53 distinct
//! previews spends them and falls through to 28 name-only rows. Those 28 render `?GoodsInfo?` in the
//! item panel WHETHER OR NOT the client writes anything, because the id has no entry in that
//! category at all. So refusing to name them cannot fix them; only CREATING the entry can.
//!
//! Creating it means inserting a new group MID-ARRAY (a spare goods row sits in the middle of the
//! vanilla id range, not above it). That is legal — the records only have to stay ascending and
//! disjoint — but it is the invariant the old append-only path got for free and a mid-insert does
//! not. [`is_ordered_disjoint`] is that check, and `build_block` refuses to allocate a block that
//! fails it.

/// One `GroupRecord`'s inclusive id range. Deliberately WITHOUT `stringIndexBase`: ordering and
/// coverage are decided by ids alone, and leaving the string index out of the type means a test
/// cannot accidentally pass by matching a string index that the game never consults for ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub first_id: u32,
    pub last_id: u32,
}

impl Span {
    pub fn new(first_id: u32, last_id: u32) -> Self {
        Self { first_id, last_id }
    }
    pub fn contains(&self, id: u32) -> bool {
        self.first_id <= id && id <= self.last_id
    }
    /// A record with `lastId < firstId` covers nothing and would make the game's binary search read
    /// a negative index; `fmg_inject::parse` already rejects such a block on the way in.
    pub fn is_valid(&self) -> bool {
        self.first_id <= self.last_id
    }
}

/// A run of newly-inserted ids and the offset-table index its first id maps to. One run becomes one
/// `GroupRecord`; contiguous ids share a run so 28 ids cost 4 records rather than 28.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub string_index_base: u32,
    pub first_id: u32,
    pub last_id: u32,
}

impl Run {
    pub fn span(&self) -> Span {
        Span::new(self.first_id, self.last_id)
    }
}

/// Does any group already carry `id`? Linear on purpose: the caller has a few hundred groups and a
/// few dozen ids, and a binary search here would silently depend on the input being sorted, which is
/// the very property [`is_ordered_disjoint`] exists to stop us assuming.
pub fn covers(spans: &[Span], id: u32) -> bool {
    spans.iter().any(|s| s.contains(id))
}

/// THE SAFETY GATE for a rebuilt group array: ascending by id, non-overlapping, every record valid.
///
/// Equal-or-overlapping neighbours are rejected, not merged. Two records that both claim an id give
/// the game's binary search two answers, and which one it returns is a function of the array length
/// — i.e. an intermittent wrong string (or an out-of-range offset-table index) rather than an
/// obvious break. `is_ordered_disjoint(&[a, a]) == false`.
pub fn is_ordered_disjoint(spans: &[Span]) -> bool {
    for (i, s) in spans.iter().enumerate() {
        if !s.is_valid() {
            return false;
        }
        if i > 0 && spans[i - 1].last_id >= s.first_id {
            return false;
        }
    }
    true
}

/// Shape census for a vanilla FMG group array (clients#35).
///
/// FromSoft titles have used a "wide claim" convention where a record's
/// inclusive `last_id` equals the next record's `first_id`. That gives two
/// records ownership of one id and makes insertion unsafe until normalized.
/// Keep this as measurement, not policy: a strict overlap is a different and
/// more serious shape than an equal boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConventionStats {
    pub groups: usize,
    pub ordered_disjoint: bool,
    pub wide_claims: usize,
    pub strict_overlaps: usize,
}

pub fn convention_stats(spans: &[Span]) -> ConventionStats {
    let mut wide_claims = 0;
    let mut strict_overlaps = 0;
    for pair in spans.windows(2) {
        if pair[0].last_id == pair[1].first_id {
            wide_claims += 1;
        } else if pair[0].last_id > pair[1].first_id {
            strict_overlaps += 1;
        }
    }
    ConventionStats {
        groups: spans.len(),
        ordered_disjoint: is_ordered_disjoint(spans),
        wide_claims,
        strict_overlaps,
    }
}

/// How a batch of override ids splits against the category's EXISTING coverage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    /// Ids that already live in a group: their string slot is REDIRECTED at the new text. Input
    /// order, deduped.
    pub redirect: Vec<u32>,
    /// Ids in no group: they need a NEW record and a NEW offset-table slot. Sorted ascending and
    /// deduped, which is what [`runs_from_ids`] requires.
    pub insert: Vec<u32>,
}

/// Split override ids into "redirect an existing slot" and "insert a new one".
///
/// The old code computed only the first half and dropped the second with a warning; the count of
/// `insert` IS the defect (28, in the log above), so it is returned rather than logged away.
pub fn split_by_coverage(spans: &[Span], ids: &[u32]) -> Split {
    let mut redirect: Vec<u32> = Vec::new();
    let mut insert: Vec<u32> = Vec::new();
    for &id in ids {
        if covers(spans, id) {
            if !redirect.contains(&id) {
                redirect.push(id);
            }
        } else if !insert.contains(&id) {
            insert.push(id);
        }
    }
    insert.sort_unstable();
    Split { redirect, insert }
}

/// Merge strictly-ascending ids into contiguous runs, handing each run the offset-table index its
/// first id maps to (`base` for the first id overall, then one slot per id in order).
///
/// `None` — never a silently empty vec — if the input is not strictly ascending. A run's ids must
/// map to CONSECUTIVE offset-table slots, so an unsorted or duplicated input would produce records
/// pointing at the wrong strings, and returning an empty plan would look identical to "nothing to
/// insert".
pub fn runs_from_ids(ids: &[u32], base: u32) -> Option<Vec<Run>> {
    if ids.windows(2).any(|w| w[0] >= w[1]) {
        return None;
    }
    let mut runs: Vec<Run> = Vec::new();
    let mut k = 0usize;
    while k < ids.len() {
        let mut j = k;
        while j + 1 < ids.len() && ids[j + 1] == ids[j] + 1 {
            j += 1;
        }
        runs.push(Run {
            string_index_base: base.checked_add(k as u32)?,
            first_id: ids[k],
            last_id: ids[j],
        });
        k = j + 1;
    }
    Some(runs)
}

/// The final group array the block will carry: vanilla records plus the inserted runs, ascending.
///
/// `None` if the result would break the game's binary search — which is exactly what happens if a
/// caller asks to INSERT an id some group already covers, so this is also the guard against
/// [`split_by_coverage`] being bypassed.
pub fn merged_order(vanilla: &[Span], inserted: &[Run]) -> Option<Vec<Span>> {
    let mut all: Vec<Span> = vanilla.to_vec();
    all.extend(inserted.iter().map(|r| r.span()));
    all.sort_by_key(|s| s.first_id);
    if is_ordered_disjoint(&all) {
        Some(all)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 25 spare goods rows that carry GoodsName + GoodsInfo + GoodsCaption, and the 28 name-only
    /// rows a 53-preview seed falls through to. REAL ids, read off `greenfield/spare_goods.tsv`
    /// (`fmg_full` column, pool order) in the apworld repo — nothing here is invented.
    const FULL_25: [u32; 25] = [
        8853, 8854, 8914, 8949, 8950, 9314, 9315, 9316, 9317, 9318, 9319, 9332, 9333, 9334, 9335,
        9336, 9337, 9338, 9339, 9394, 9395, 9396, 9397, 9398, 9399,
    ];
    const NAME_ONLY_28: [u32; 28] = [
        9349, 9350, 9351, 9352, 9353, 9354, 9355, 9356, 9357, 9358, 9359, 9366, 9367, 9368, 9369,
        9370, 9404, 9405, 9406, 9407, 9408, 9409, 9410, 9424, 9425, 9426, 9427, 9428,
    ];

    /// A MODEL of GoodsInfo (cat 20) coverage, not a dump of the vanilla record table: the runs of
    /// `FULL_25`, which is precisely the property `spare_goods.tsv` asserts about those ids (they
    /// have a GoodsInfo entry; the other 40 spares do not).
    fn goods_info_spans() -> Vec<Span> {
        let mut spans: Vec<Span> = Vec::new();
        for &id in FULL_25.iter() {
            match spans.last_mut() {
                Some(s) if s.last_id + 1 == id => s.last_id = id,
                _ => spans.push(Span::new(id, id)),
            }
        }
        spans
    }

    #[test]
    fn ordered_disjoint_accepts_ascending_gaps() {
        assert!(is_ordered_disjoint(&[]));
        assert!(is_ordered_disjoint(&[Span::new(10, 20)]));
        assert!(is_ordered_disjoint(&[
            Span::new(10, 20),
            Span::new(21, 21),
            Span::new(30, 40)
        ]));
    }

    #[test]
    fn ordered_disjoint_rejects_every_way_the_binary_search_can_break() {
        // descending
        assert!(!is_ordered_disjoint(&[
            Span::new(30, 40),
            Span::new(10, 20)
        ]));
        // overlapping
        assert!(!is_ordered_disjoint(&[
            Span::new(10, 25),
            Span::new(20, 30)
        ]));
        // touching (both claim id 20)
        assert!(!is_ordered_disjoint(&[
            Span::new(10, 20),
            Span::new(20, 30)
        ]));
        // duplicated record
        assert!(!is_ordered_disjoint(&[
            Span::new(10, 20),
            Span::new(10, 20)
        ]));
        // last < first
        assert!(!is_ordered_disjoint(&[Span::new(20, 10)]));
    }

    #[test]
    fn convention_census_separates_wide_claims_from_strict_overlaps() {
        let spans = [
            Span::new(10, 20),
            Span::new(20, 30),
            Span::new(25, 40),
            Span::new(50, 60),
        ];
        assert_eq!(
            convention_stats(&spans),
            ConventionStats {
                groups: 4,
                ordered_disjoint: false,
                wide_claims: 1,
                strict_overlaps: 1,
            }
        );
    }

    #[test]
    fn convention_census_reports_a_clean_array_without_inventing_overlap() {
        assert_eq!(
            convention_stats(&[Span::new(1, 2), Span::new(3, 4)]),
            ConventionStats {
                groups: 2,
                ordered_disjoint: true,
                wide_claims: 0,
                strict_overlaps: 0,
            }
        );
    }

    #[test]
    fn covers_is_inclusive_at_both_ends() {
        let s = [Span::new(100, 110)];
        assert!(covers(&s, 100));
        assert!(covers(&s, 110));
        assert!(!covers(&s, 99));
        assert!(!covers(&s, 111));
    }

    /// THE MOTIVATING CASE (issue #300): a 53-preview seed against GoodsInfo coverage splits
    /// 25 redirect / 28 insert — the exact `infos=25` / `28 of 53` boblerrr's log reported. The
    /// old redirect-only writer produced the 25 and dropped the 28; if a change ever makes
    /// `insert` empty again, this fails.
    #[test]
    fn boblerrr_53_previews_split_25_redirect_28_insert() {
        let spans = goods_info_spans();
        let mut ids: Vec<u32> = FULL_25.to_vec();
        ids.extend_from_slice(&NAME_ONLY_28);
        assert_eq!(ids.len(), 53);

        let split = split_by_coverage(&spans, &ids);
        assert_eq!(
            split.redirect.len(),
            25,
            "redirects (the `infos=25` in the log)"
        );
        assert_eq!(
            split.insert.len(),
            28,
            "inserts (the `28 of 53` the old path DROPPED)"
        );
        assert_eq!(split.redirect, FULL_25.to_vec());
        assert_eq!(split.insert, NAME_ONLY_28.to_vec());
        // Every insert really is unwritable by redirect, and every redirect really is writable.
        assert!(split.insert.iter().all(|&id| !covers(&spans, id)));
        assert!(split.redirect.iter().all(|&id| covers(&spans, id)));
    }

    /// 16 of the 28 sit BETWEEN covered runs — 9349..=9359 and 9366..=9370 fall in the gap between
    /// the 9332..=9339 and 9394..=9399 records. Mid-array insertion is therefore REQUIRED: the
    /// append-only rule "injected ids must sort above every vanilla id" cannot express this, which
    /// is exactly why the world's datamine script wrote those ids off as unnameable.
    #[test]
    fn the_28_require_a_mid_array_insert_not_an_append() {
        let spans = goods_info_spans();
        let highest = spans.iter().map(|s| s.last_id).max().unwrap();
        let below: Vec<u32> = NAME_ONLY_28
            .iter()
            .copied()
            .filter(|&id| id < highest)
            .collect();
        assert!(
            !below.is_empty(),
            "if none of the 28 fell below the top vanilla group, an append would have sufficed"
        );
        assert_eq!(below.len(), 16); // 9349..=9359 and 9366..=9370
    }

    #[test]
    fn runs_merge_the_28_into_four_records() {
        let runs = runs_from_ids(&NAME_ONLY_28, 1000).expect("strictly ascending");
        assert_eq!(
            runs,
            vec![
                Run {
                    string_index_base: 1000,
                    first_id: 9349,
                    last_id: 9359
                },
                Run {
                    string_index_base: 1011,
                    first_id: 9366,
                    last_id: 9370
                },
                Run {
                    string_index_base: 1016,
                    first_id: 9404,
                    last_id: 9410
                },
                Run {
                    string_index_base: 1023,
                    first_id: 9424,
                    last_id: 9428
                },
            ]
        );
        // Every id maps to its own consecutive offset-table slot, in input order.
        for (i, &id) in NAME_ONLY_28.iter().enumerate() {
            let r = runs.iter().find(|r| r.span().contains(id)).unwrap();
            assert_eq!(r.string_index_base + (id - r.first_id), 1000 + i as u32);
        }
    }

    #[test]
    fn runs_reject_unsorted_or_duplicated_input() {
        assert_eq!(runs_from_ids(&[5, 4], 0), None);
        assert_eq!(runs_from_ids(&[4, 4], 0), None);
        assert_eq!(runs_from_ids(&[], 7), Some(vec![]));
    }

    #[test]
    fn merged_order_puts_the_new_records_in_their_sorted_place() {
        let spans = goods_info_spans();
        let runs = runs_from_ids(&NAME_ONLY_28, 5000).unwrap();
        let merged = merged_order(&spans, &runs).expect("ascending + disjoint");
        assert_eq!(merged.len(), spans.len() + runs.len());
        assert!(is_ordered_disjoint(&merged));
        // A record for 9349 now exists, and it is NOT at the end of the array.
        let at = merged.iter().position(|s| s.contains(9349)).unwrap();
        assert!(
            at < merged.len() - 1,
            "9349's record must sort into the middle"
        );
        for &id in NAME_ONLY_28.iter().chain(FULL_25.iter()) {
            assert!(covers(&merged, id));
        }
    }

    /// The gate must FAIL when the bug it guards is present: inserting an id a group already covers
    /// produces two records claiming it, and `merged_order` refuses to build that array.
    #[test]
    fn merged_order_refuses_a_double_claimed_id() {
        let spans = goods_info_spans();
        let bogus = vec![Run {
            string_index_base: 5000,
            first_id: 8853,
            last_id: 8853,
        }];
        assert_eq!(merged_order(&spans, &bogus), None);
    }

    #[test]
    fn append_only_input_is_unchanged_by_the_merge() {
        // The synthetic-goods path (`fmg_inject::run`) injects ids above every vanilla id; the
        // sorted merge must be a no-op for it, or this change would move records it already ships.
        let vanilla = vec![Span::new(100, 200), Span::new(300, 400)];
        let runs = runs_from_ids(&[3_780_000, 3_780_001, 3_790_000], 900).unwrap();
        let merged = merged_order(&vanilla, &runs).unwrap();
        assert_eq!(merged[0], vanilla[0]);
        assert_eq!(merged[1], vanilla[1]);
        assert_eq!(merged[2], Span::new(3_780_000, 3_780_001));
        assert_eq!(merged[3], Span::new(3_790_000, 3_790_000));
    }
}
