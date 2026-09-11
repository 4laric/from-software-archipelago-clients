//! Replay the production receive decision, trap claim, and persisted JSON together.
use er_logic::{
    receive::{self, GrantAction, NetHook, RecvItem},
    receive_cursor::claim_trap,
    save_state::SaveState,
};
use std::collections::HashMap;

struct Hook;
impl NetHook for Hook {
    fn on_item_received(&mut self, _: &str) {}
    fn progressive_on_item_received(&mut self, _: &str, _: i64) -> bool {
        false
    }
}

fn replay(state: &mut SaveState, pushed: &mut i64, count: i64) -> Vec<i64> {
    let mut dispatched = 0;
    let mut queued_and_linked = Vec::new();
    for index in 0..count {
        let item = RecvItem {
            index,
            ap_item_id: 123,
            name: "Trap: Rune Thief".into(),
            echo_skip: false,
        };
        if matches!(
            receive::process_received_item(
                &item,
                &mut dispatched,
                pushed,
                &HashMap::new(),
                &HashMap::new(),
                &mut Hook
            ),
            GrantAction::SkipUnmapped { .. }
        ) && claim_trap(&mut state.traps_received_through, index)
        {
            queued_and_linked.push(index);
        }
    }
    queued_and_linked
}

#[test]
fn restarting_and_rewinding_item_recovery_does_not_replay_traps_or_traplink() {
    let mut state = SaveState::default();
    let mut cursor = 0;
    assert_eq!(replay(&mut state, &mut cursor, 3), [0, 1, 2]);
    state = SaveState::from_json(&state.to_json());
    assert!(replay(&mut state, &mut cursor, 3).is_empty());
    // Fresh-character binding or an item-cursor repair must not repeat punishments.
    cursor = 0;
    assert!(replay(&mut state, &mut cursor, 3).is_empty());
    // A trap received while disconnected is still owed, even with the same name.
    assert_eq!(replay(&mut state, &mut cursor, 4), [3]);
    assert!(replay(&mut state, &mut cursor, 4).is_empty());
    let mut another_room = SaveState::default();
    assert_eq!(replay(&mut another_room, &mut 0, 1), [0]);
}

#[test]
fn previous_release_saves_migrate_both_frontier_shapes() {
    for (json, frontier) in [
        (r#"{"last_received_index":3}"#, 3),
        (
            r#"{"last_received_index":0,"received_cursors":{"0":{"index":3,"play_time_ms":90000},"6":{"index":1,"play_time_ms":1200}}}"#,
            3,
        ),
        (
            r#"{"last_received_index":4,"received_cursors":{"0":{"index":3,"play_time_ms":90000}}}"#,
            4,
        ),
    ] {
        let mut state = SaveState::from_json(json);
        assert_eq!(state.traps_received_through, frontier);
        state = SaveState::from_json(&state.to_json());
        assert_eq!(replay(&mut state, &mut 0, frontier + 1), [frontier]);
    }
}

#[test]
fn explicit_trap_frontier_survives_ordinary_cursor_changes() {
    let mut state =
        SaveState::from_json(r#"{"traps_received_through":3,"last_received_index":50}"#);
    assert_eq!(state.traps_received_through, 3);
    assert!(!claim_trap(&mut state.traps_received_through, -1));
    assert_eq!(replay(&mut state, &mut 0, 4), [3]);
}
