//! Player-facing Archipelago pickup lines for the standalone overlay.
//!
//! Scouting is display-only. A missing, late, or failed scout must never delay a
//! location check, item delivery, or acknowledgement.

use std::collections::HashMap;

use archipelago_rs::{Client, CreateAsHint, LocatedItem};
use oneshot::{Receiver, TryRecvError};

const MAX_SCOUT_RETRIES: u8 = 2;

pub struct PlacementScouts {
    locations: Vec<i64>,
    pending: bool,
    receiver: Option<Receiver<Result<Vec<LocatedItem>, archipelago_rs::Error>>>,
    placements: HashMap<i64, Placement>,
    retries: u8,
    finished: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Placement {
    item: String,
    receiver: String,
}

impl PlacementScouts {
    pub fn new(locations: impl IntoIterator<Item = i64>) -> Self {
        Self {
            locations: locations.into_iter().collect(),
            pending: true,
            receiver: None,
            placements: HashMap::new(),
            retries: 0,
            finished: false,
        }
    }

    /// Issue or poll the non-hinting scout request. This is deliberately
    /// best-effort: callers always continue normal check processing afterwards.
    pub fn pump(&mut self, client: &mut Client<json::Value>) {
        if self.finished {
            return;
        }
        if self.pending {
            self.pending = false;
            if self.locations.is_empty() {
                self.finished = true;
                return;
            }
            self.receiver =
                Some(client.scout_locations(self.locations.iter().copied(), CreateAsHint::No));
            return;
        }

        let Some(receiver) = self.receiver.as_mut() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(items)) => {
                self.placements = items
                    .into_iter()
                    .map(|placed| {
                        (
                            placed.location().id(),
                            Placement {
                                item: placed.item().name().to_owned(),
                                receiver: placed.receiver().alias().to_owned(),
                            },
                        )
                    })
                    .collect();
                self.receiver = None;
                self.finished = true;
            }
            Ok(Err(error)) => {
                crate::client_debugln!("Pickup toast scout failed: {error}");
                self.retry();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                crate::client_debugln!(
                    "Pickup toast scout was interrupted; reconnect will retry it."
                );
                self.retry();
            }
        }
    }

    fn retry(&mut self) {
        self.receiver = None;
        if self.retries < MAX_SCOUT_RETRIES {
            self.retries += 1;
            self.pending = true;
        } else {
            self.finished = true;
        }
    }

    pub fn sent_line(&self, location: i64, fallback: &str) -> String {
        self.placements.get(&location).map_or_else(
            || format!("\u{2713} {fallback}"),
            |placed| sent_line(&placed.item, &placed.receiver),
        )
    }
}

pub fn sent_line(item: &str, receiver: &str) -> String {
    format!("\u{2713} {item} \u{2192} {receiver}")
}

pub fn received_line(item: &str, sender: &str) -> String {
    format!("\u{2192} {item} (from {sender})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sent_and_received_lines_name_the_people_involved() {
        assert_eq!(
            sent_line("Fire Paper x2", "oz"),
            "\u{2713} Fire Paper x2 \u{2192} oz"
        );
        assert_eq!(
            received_line("Great One's Wisdom", "oz"),
            "\u{2192} Great One's Wisdom (from oz)"
        );
    }

    #[test]
    fn an_unscouted_location_still_produces_an_honest_line() {
        let scouts = PlacementScouts::new([]);
        assert_eq!(
            scouts.sent_line(12, "Central Yharnam - corpse by lamp"),
            "\u{2713} Central Yharnam - corpse by lamp"
        );
    }
}
