//! Pure, validated bingo protocol and victory decisions. Game observations stay in the DLL.
use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

/// GoodsName.fmg identities: include empty/full forms and the obsolete +7-era rows.
pub fn flask_potency(row: u32) -> Option<u32> {
    match row {
        1000..=1025 => Some((row - 1000) / 2),
        1050..=1075 => Some((row - 1050) / 2),
        200..=215 => Some((row - 200) / 2),
        220..=235 => Some((row - 220) / 2),
        _ => None,
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub location: i64,
    pub flag: u32,
    pub region: String,
    pub label: String,
    #[serde(default)]
    pub state: Option<StateGoal>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateGoal {
    pub metric: String,
    pub target: u32,
}

impl StateGoal {
    fn valid(&self) -> bool {
        match self.metric.as_str() {
            "level" => matches!(self.target, 55 | 60 | 65 | 80 | 85),
            "faith" | "arcane" | "intelligence" => self.target == 30,
            "flask_potency" => self.target == 7,
            "flask_charges" => self.target == 10,
            "scadutree" => matches!(self.target, 9..=11),
            "spirit_ash" => self.target == 5,
            _ => false,
        }
    }
}

impl Cell {
    /// Server acknowledgement is intentionally absent: only native evidence earns a square.
    pub fn earned(
        &self,
        flag_read: impl Fn(u32) -> bool,
        earned: &std::collections::BTreeSet<i64>,
    ) -> bool {
        if self.state.is_some() {
            earned.contains(&self.location)
        } else {
            flag_read(self.flag)
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Board {
    pub version: u32,
    pub catalogue: String,
    pub hash: String,
    pub cells: Vec<Cell>,
    pub goal: String,
    pub count: usize,
    pub line_sweep: Vec<i64>,
}

impl Board {
    pub fn protects_flag(&self, flag: u32) -> bool {
        flag != 0 && self.cells.iter().any(|cell| cell.flag == flag)
    }

    /// Latch native observations before reporting. Respecs and reconnects cannot unearn them.
    pub fn observe_states(
        &self,
        values: &std::collections::BTreeMap<String, u32>,
        earned: &mut std::collections::BTreeSet<i64>,
    ) {
        for cell in &self.cells {
            if let Some(state) = &cell.state {
                if values
                    .get(&state.metric)
                    .is_some_and(|v| *v >= state.target)
                {
                    earned.insert(cell.location);
                }
            }
        }
    }

    pub fn parse(value: &Value) -> Result<Self, String> {
        let board: Self = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if !matches!(
            (board.version, board.catalogue.as_str()),
            (1, "ap-boss-board-v1") | (2, "ap-e1-board-v1")
        ) {
            return Err("unsupported bingo schema or catalogue".into());
        }
        if board.cells.len() != 25
            || !(1..=25).contains(&board.count)
            || !matches!(board.goal.as_str(), "line" | "count" | "blackout")
            || board.hash.len() != 64
            || !board.hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid bingo board size, goal, count or identity".into());
        }
        let mut ids = HashSet::new();
        let mut flags = HashSet::new();
        for cell in &board.cells {
            if cell.location <= 0
                || cell.label.is_empty()
                || cell.region.is_empty()
                || !ids.insert(cell.location)
                || match &cell.state {
                    Some(state) => board.version != 2 || cell.flag != 0 || !state.valid(),
                    None => cell.flag == 0 || !flags.insert(cell.flag),
                }
            {
                return Err("invalid or duplicate bingo cell identity".into());
            }
        }
        if board.line_sweep.len() > 50
            || board
                .line_sweep
                .iter()
                .any(|&id| id <= 0 || ids.contains(&id))
            || board.line_sweep.iter().collect::<HashSet<_>>().len() != board.line_sweep.len()
        {
            return Err("invalid first-line sweep membership".into());
        }
        Ok(board)
    }

    pub fn has_line(done: &[bool; 25]) -> bool {
        (0..5).any(|r| (0..5).all(|c| done[r * 5 + c]))
            || (0..5).any(|c| (0..5).all(|r| done[r * 5 + c]))
            || (0..5).all(|i| done[i * 6])
            || (0..5).all(|i| done[(i + 1) * 4])
    }

    pub fn is_complete(&self, is_checked: impl Fn(i64) -> bool) -> bool {
        let done = std::array::from_fn(|i| is_checked(self.cells[i].location));
        match self.goal.as_str() {
            "line" => Self::has_line(&done),
            "count" => done.iter().filter(|&&v| v).count() >= self.count,
            "blackout" => done.iter().all(|&v| v),
            _ => false,
        }
    }

    /// Present the board's action targets through the existing boss-marker ABI.
    /// Native checks and synthetic square checks share a boss marker, not an AP ID.
    /// Only local defeat evidence removes an objective; server collection cannot.
    pub fn overlay_map_states(
        &self,
        states: &mut Vec<crate::mfg_match::LotCheckState>,
        valid: &HashSet<i64>,
        open_regions: &HashSet<String>,
        flag_read: impl Fn(u32) -> bool,
    ) {
        use crate::mfg_match::{CHECK, IN_LOGIC, PROGRESSION, PROGRESSION_IN_LOGIC};
        let board_flags: HashSet<_> = self.cells.iter().map(|cell| cell.flag).collect();
        // In bingo, the progression-only filter means outstanding board objectives.
        // Ordinary check visibility remains available when that filter is disabled.
        states.retain(|entry| entry.lot_table != 3 || !board_flags.contains(&entry.lot_row));
        for entry in states.iter_mut() {
            entry.flags &= !(PROGRESSION | PROGRESSION_IN_LOGIC);
        }
        for cell in &self.cells {
            if cell.state.is_some() || !valid.contains(&cell.location) || flag_read(cell.flag) {
                continue;
            }
            let reachable = open_regions.contains(&cell.region);
            states.push(crate::mfg_match::LotCheckState {
                lot_table: 3,
                lot_row: cell.flag,
                flags: CHECK
                    | PROGRESSION
                    | if reachable {
                        IN_LOGIC | PROGRESSION_IN_LOGIC
                    } else {
                        0
                    },
            });
        }
        states.sort_unstable_by_key(|entry| (entry.lot_table, entry.lot_row));
    }

    /// AP collect/sweep truth deduplicates reports; only local encounters earn credit.
    pub fn pending_checks(
        &self,
        flag_read: impl Fn(u32) -> bool,
        checked: impl Fn(i64) -> bool,
    ) -> Vec<i64> {
        self.pending_earned(flag_read, checked, &Default::default())
    }

    pub fn pending_earned(
        &self,
        flag_read: impl Fn(u32) -> bool,
        checked: impl Fn(i64) -> bool,
        earned: &std::collections::BTreeSet<i64>,
    ) -> Vec<i64> {
        let done: [bool; 25] = std::array::from_fn(|i| self.cells[i].earned(&flag_read, earned));
        let mut out: Vec<i64> = self
            .cells
            .iter()
            .enumerate()
            .filter(|(i, cell)| done[*i] && !checked(cell.location))
            .map(|(_, cell)| cell.location)
            .collect();
        if Self::has_line(&done) {
            out.extend(self.line_sweep.iter().copied().filter(|&id| !checked(id)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({"version":1,"catalogue":"ap-boss-board-v1","hash":"a".repeat(64),
            "goal":"line","count":13,"line_sweep":[],"cells":(0..25).map(|i|json!({"location":100+i,
            "flag":200+i,"region":"Test","label":"Defeat test boss"})).collect::<Vec<_>>()})
    }

    #[test]
    fn e1_thresholds_latch_and_require_native_evidence_after_collect() {
        let mut p = payload();
        p["version"] = json!(2);
        p["catalogue"] = json!("ap-e1-board-v1");
        for i in 0..5 {
            p["cells"][i]["flag"] = json!(0);
            p["cells"][i]["state"] = json!({"metric":"faith", "target":30});
        }
        let board = Board::parse(&p).unwrap();
        let mut earned = Default::default();
        let mut values = std::collections::BTreeMap::from([("faith".into(), 29)]);
        board.observe_states(&values, &mut earned);
        assert!(board
            .pending_earned(|_| false, |_| false, &earned)
            .is_empty());
        values.insert("faith".into(), 30);
        board.observe_states(&values, &mut earned);
        assert_eq!(
            board.pending_earned(|_| false, |_| false, &earned),
            vec![100, 101, 102, 103, 104]
        );
        values.insert("faith".into(), 10);
        board.observe_states(&values, &mut earned);
        assert!(board.is_complete(|id| earned.contains(&id)));
        assert!(!board.is_complete(|_| false));
        assert!(board
            .pending_earned(|_| false, |_| true, &Default::default())
            .is_empty());
        assert!(board.protects_flag(205));
        assert!(!board.protects_flag(0));
        p["cells"][0]["state"]["metric"] = json!("effective_faith");
        assert!(Board::parse(&p).is_err());
        p["cells"][0]["state"]["metric"] = json!("faith");
        p["version"] = json!(1);
        assert!(Board::parse(&p).is_err());
    }

    #[test]
    fn flask_rows_include_current_goods_and_empty_flasks() {
        for row in [1014, 1015, 1064, 1065, 214, 215, 234, 235] {
            assert_eq!(flask_potency(row), Some(7));
        }
        assert_eq!(flask_potency(1025), Some(12));
        assert_eq!(flask_potency(1075), Some(12));
        assert_eq!(flask_potency(250), None); // Physick
    }

    #[test]
    fn every_line_requires_all_five_cells() {
        let mut lines = Vec::new();
        for k in 0..5 {
            lines.push((0..5).map(|i| k * 5 + i).collect::<Vec<_>>());
            lines.push((0..5).map(|i| i * 5 + k).collect::<Vec<_>>());
        }
        lines.push((0..5).map(|i| i * 6).collect());
        lines.push((0..5).map(|i| (i + 1) * 4).collect());
        for line in lines {
            let mut done = [false; 25];
            for &i in &line {
                done[i] = true;
            }
            assert!(Board::has_line(&done));
            done[line[0]] = false;
            assert!(!Board::has_line(&done));
        }
    }

    #[test]
    fn rejects_duplicate_and_unknown_protocol_values() {
        let mut p = payload();
        assert!(Board::parse(&p).is_ok());
        p["cells"][1]["flag"] = p["cells"][0]["flag"].clone();
        assert!(Board::parse(&p).is_err());
        p = payload();
        p["goal"] = json!("future");
        assert!(Board::parse(&p).is_err());
        p = payload();
        p["cells"].as_array_mut().unwrap().pop();
        assert!(Board::parse(&p).is_err());
    }

    #[test]
    fn count_blackout_and_replayed_checks_use_distinct_ids() {
        let mut board = Board::parse(&payload()).unwrap();
        board.goal = "count".into();
        assert!(!board.is_complete(|id| id < 112));
        assert!(board.is_complete(|id| id < 113));
        board.goal = "blackout".into();
        assert!(!board.is_complete(|id| id != 124));
        assert!(board.is_complete(|_| true));
    }

    #[test]
    fn first_line_rewards_retry_without_duplicate_or_collect_credit() {
        let mut board = Board::parse(&payload()).unwrap();
        board.line_sweep = vec![300, 301];
        assert!(board.pending_checks(|_| false, |_| true).is_empty());
        let first = board.pending_checks(|flag| flag < 205, |_| false);
        assert_eq!(first, vec![100, 101, 102, 103, 104, 300, 301]);
        assert!(board
            .pending_checks(|flag| flag < 205, |id| first.contains(&id))
            .is_empty());
        assert_eq!(
            board.pending_checks(|flag| flag < 205, |id| id != 301),
            vec![301]
        );
    }

    #[test]
    fn map_objectives_join_native_markers_and_do_not_leak_other_progression() {
        use crate::mfg_match::{LotCheckState, CHECK, IN_LOGIC, PROGRESSION};
        let board = Board::parse(&payload()).unwrap();
        let mut states = vec![
            LotCheckState {
                lot_table: 3,
                lot_row: 200,
                flags: 15,
            },
            LotCheckState {
                lot_table: 1,
                lot_row: 99,
                flags: 15,
            },
        ];
        board.overlay_map_states(
            &mut states,
            &[100, 101, 102].into_iter().collect(),
            &["Test".to_owned()].into_iter().collect(),
            |flag| flag == 201,
        );
        assert_eq!(states.len(), 3);
        assert_eq!(
            states
                .iter()
                .filter(|s| s.lot_table == 3 && s.lot_row == 200)
                .count(),
            1
        );
        assert!(states.iter().any(|s| s.lot_row == 202 && s.flags == 15));
        assert!(!states.iter().any(|s| s.lot_row == 201));
        assert_eq!(states[0].flags, CHECK | IN_LOGIC);
        assert_eq!(
            states.iter().filter(|s| s.flags & PROGRESSION != 0).count(),
            2
        );
    }

    #[test]
    fn locked_objectives_remain_visible_without_claiming_access_or_server_credit() {
        use crate::mfg_match::{CHECK, PROGRESSION};
        let board = Board::parse(&payload()).unwrap();
        let valid = board.cells.iter().map(|cell| cell.location).collect();
        let mut states = Vec::new();
        board.overlay_map_states(&mut states, &valid, &HashSet::new(), |_| false);
        assert_eq!(states.len(), 25);
        assert!(states
            .iter()
            .all(|state| state.flags == CHECK | PROGRESSION));
        board.overlay_map_states(&mut states, &valid, &HashSet::new(), |_| true);
        assert!(states.is_empty());
    }
}
