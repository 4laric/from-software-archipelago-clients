//! Pure, validated bingo protocol and victory decisions. Game observations stay in the DLL.
use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub location: i64,
    pub flag: u32,
    pub region: String,
    pub label: String,
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
    pub fn parse(value: &Value) -> Result<Self, String> {
        let board: Self = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if board.version != 1 || board.catalogue != "ap-boss-board-v1" {
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
                || cell.flag == 0
                || cell.label.is_empty()
                || cell.region.is_empty()
                || !ids.insert(cell.location)
                || !flags.insert(cell.flag)
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

    /// AP collect/sweep truth deduplicates reports; only local encounters earn credit.
    pub fn pending_checks(
        &self,
        flag_read: impl Fn(u32) -> bool,
        checked: impl Fn(i64) -> bool,
    ) -> Vec<i64> {
        let done: [bool; 25] = std::array::from_fn(|i| flag_read(self.cells[i].flag));
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
}
