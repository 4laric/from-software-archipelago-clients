//! Persistent record of progression-surface shop checks the player has viewed.

use ap::DataStorageOperation;
use archipelago_rs as ap;
use oneshot::TryRecvError;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

fn ledger_key(slot: u32) -> String {
    format!("er_region_completion_shop_views_{slot}")
}

#[derive(Default)]
pub struct ShopViews {
    key: Option<String>,
    viewed: HashSet<i64>,
    loaded: bool,
    get_rx: Option<oneshot::Receiver<Result<HashMap<String, Value>, ap::Error>>>,
}

static STATE: LazyLock<Mutex<ShopViews>> = LazyLock::new(|| Mutex::new(ShopViews::default()));

pub fn reset() {
    if let Ok(mut state) = STATE.lock() {
        state.reset();
    }
}

pub fn ready_and_viewed() -> Option<HashSet<i64>> {
    STATE
        .lock()
        .ok()
        .and_then(|state| state.is_ready().then(|| state.viewed().clone()))
}

pub fn pump(client: &mut ap::Client<Value>) {
    if let Ok(mut state) = STATE.lock() {
        state.pump(client);
    }
}

impl ShopViews {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn is_ready(&self) -> bool {
        self.loaded
    }

    pub fn viewed(&self) -> &HashSet<i64> {
        &self.viewed
    }

    pub fn pump(&mut self, client: &mut ap::Client<Value>) {
        if self.key.is_none() {
            self.key = Some(ledger_key(client.this_player().slot()));
        }
        let key = self.key.clone().expect("key set above");
        if !self.loaded {
            if self.get_rx.is_none() {
                self.get_rx = Some(client.get([key.clone()]));
                return;
            }
            let Some(rx) = self.get_rx.as_mut() else {
                return;
            };
            match rx.try_recv() {
                Ok(Ok(map)) => {
                    self.get_rx = None;
                    self.viewed = map
                        .get(&key)
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_i64).collect())
                        .unwrap_or_default();
                    self.loaded = true;
                    log::info!(
                        "region-completion: restored {} viewed shop check(s)",
                        self.viewed.len()
                    );
                }
                Ok(Err(e)) => {
                    self.get_rx = None;
                    log::warn!(
                        "region-completion: could not read viewed-shop ledger ({e}); goal gate stays shut"
                    );
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.get_rx = None;
                    log::warn!(
                        "region-completion: viewed-shop ledger request dropped; goal gate stays shut"
                    );
                }
            }
            return;
        }

        for loc in crate::shop_hints::take_viewed_surface_locations() {
            if !self.viewed.insert(loc) {
                continue;
            }
            if let Err(e) = client.change(
                key.clone(),
                Value::Array(Vec::new()),
                [DataStorageOperation::Appends(vec![Value::from(loc)])],
                false,
            ) {
                self.viewed.remove(&loc);
                log::warn!(
                    "region-completion: could not persist viewed shop check {loc} ({e}); reopen the shop to retry"
                );
            } else {
                log::info!(
                    "region-completion: shop check {loc} satisfied by viewing its merchant inventory"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_is_scoped_to_the_ap_slot() {
        assert_ne!(ledger_key(1), ledger_key(2));
    }
}
