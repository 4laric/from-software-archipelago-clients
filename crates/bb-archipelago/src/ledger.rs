use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::feed::EquipTarget;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReceiveLedger {
    pub slots: BTreeMap<String, SlotLedger>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SlotLedger {
    /// Stable game-save identity bound to this AP seed/slot. Older ledgers bind
    /// on their first validated runtime context before any game mutation.
    #[serde(default)]
    pub bound_save_identity: Option<String>,
    pub highest_processed_index: Option<u64>,
    pub acknowledged: BTreeMap<u64, AcknowledgedItem>,
    #[serde(default)]
    pub pending: Option<PendingItem>,
    /// The last AP receive cursor confirmed written into the save itself
    /// (docs/SAVE-RECONCILIATION.md §5/§7). `None` means the watermark was
    /// never active for this slot -- the attested-mode status quo, not an
    /// error -- so older ledgers load unchanged.
    #[serde(default)]
    pub save_watermark: Option<u64>,
    /// Indices below the cursor that must be delivered again (clients#427):
    /// entries that were parked with `quantity_mismatch` under the old
    /// ledger-sum precondition and are safe to retry now that the precondition
    /// is the observed live quantity. `next_index` prefers the lowest of these,
    /// so every ordering ensure in this file keeps holding unchanged. Older
    /// ledgers have no such field and load with it empty.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub redeliver: BTreeSet<u64>,
    /// Randomized fixed pickups whose one-bullet sustain award has been
    /// queued but not yet witnessed in game (clients#511). The AP location id
    /// is the idempotency key; the optional quantity is recorded before the
    /// native command is published so an interrupted grant can be recovered
    /// without duplicating it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pending_sustain: BTreeMap<i64, Option<u32>>,
    /// Sustain awards whose native grant completed. Kept independently from
    /// received-item acknowledgement because checking a location must never
    /// block or duplicate its randomized AP item.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub completed_sustain: BTreeSet<i64>,
    /// Qualifying local deaths forgiven in the current DeathLink-amnesty
    /// cycle. Kept with the seed/slot ledger so reconnecting or relaunching
    /// cannot silently reset the cadence.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub death_link_amnesty_used: u32,
    /// Console rescue grants, isolated from the AP receive cursor. The map key
    /// is a seed-contract AP item id; retaining the completed plan makes a
    /// repeated command or restart a fixed point.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub operator_grants: BTreeMap<i64, PendingItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operator_actions: Vec<OperatorAction>,
    /// Authoritative goal transition witnessed through this slot's validated
    /// positioned-save location poll. Historical server checks never create
    /// this record; retaining it prevents restart/reconnect celebrations and
    /// duplicate Goal packets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victory: Option<VictoryRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VictoryRecord {
    pub goal_location: i64,
    pub goal_name: String,
    pub completed_at_ms: u64,
    pub elapsed_seconds: Option<u64>,
    pub checks_completed: Option<u32>,
    pub checks_total: Option<u32>,
    pub received_items: Option<u32>,
    pub sent_items: Option<u32>,
    pub deaths: Option<u32>,
    pub death_links: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorAction {
    pub timestamp_ms: u64,
    pub command: String,
    pub argument: i64,
    pub resolved_name: String,
}

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// The single defined outcome of comparing the save-resident receive
/// watermark against the durable ledger cursor (docs/SAVE-RECONCILIATION.md
/// §5, invariant I4: every restore/switch shape has exactly one outcome).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatermarkOutcome {
    /// Save and ledger agree; nothing to do.
    Resume,
    /// The save regressed behind the ledger (a restore): the acknowledged
    /// tail was rewound and will be re-issued in index order.
    Reissue,
    /// The ledger regressed behind the save (ledger loss/rollback): the save
    /// cursor is adopted and nothing is re-granted (I1).
    AdoptSaveCursor,
    /// A watermark was recorded for this slot but could not be read now:
    /// no grants, no checks, operator-visible (I3).
    Hold,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcknowledgedItem {
    pub ap_item_id: i64,
    #[serde(default)]
    pub raw_descriptor: u32,
    pub normalized_item_id: u32,
    #[serde(default = "legacy_goods_category")]
    pub item_category: u8,
    pub quantity: u32,
    #[serde(default)]
    pub reinforcement_level: Option<u8>,
    #[serde(default)]
    pub equip_target: Option<EquipTarget>,
    /// The harness's terminal failure detail when this item was PARKED rather
    /// than delivered (clients#399): the acknowledgement advanced the stream
    /// without a physical grant, and the detail is the operator's evidence for
    /// resolving it with bb-blocked. `None` is an ordinary delivery. Older
    /// ledgers predate the field and load as `None`.
    #[serde(default)]
    pub blocked: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingItem {
    pub index: u64,
    pub ap_item_id: i64,
    #[serde(default)]
    pub raw_descriptor: u32,
    pub normalized_item_id: u32,
    #[serde(default = "legacy_goods_category")]
    pub item_category: u8,
    pub quantity: u32,
    #[serde(default)]
    pub upgrade_target_level: Option<u8>,
    pub reinforcement_level: Option<u8>,
    pub equip_target: Option<EquipTarget>,
    #[serde(default)]
    pub grant_complete: bool,
    #[serde(default)]
    pub equip_complete: bool,
    /// The live stack quantity observed at the moment this grant was first
    /// submitted (clients#427). It is the delivery precondition, and it is
    /// durable so that a restart mid-grant compares against the SAME baseline
    /// the interrupted command used -- which is what lets the delivery machine
    /// answer `recovered_complete` instead of granting twice. `None` means the
    /// baseline has not been sampled yet: the next poll observes and records
    /// it. Older ledgers load as `None` and simply sample on their next poll.
    #[serde(default)]
    pub observed_before: Option<u32>,
}

const fn legacy_goods_category() -> u8 {
    4
}

const LEDGER_LOCK_ATTEMPTS: usize = 6;
const LEDGER_LOCK_RETRY_DELAY: Duration = Duration::from_millis(50);

fn is_windows_sharing_violation(error: &io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))
}

fn retry_sharing_violation<T>(
    action: &str,
    operation: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    retry_io_with(
        action,
        operation,
        is_windows_sharing_violation,
        thread::sleep,
    )
}

/// Retry only a short-lived sharing/lock violation. The injected classifier
/// and sleeper make the retry contract deterministic in tests without
/// manufacturing a real Windows file lock.
fn retry_io_with<T>(
    action: &str,
    mut operation: impl FnMut() -> io::Result<T>,
    should_retry: impl Fn(&io::Error) -> bool,
    mut sleep: impl FnMut(Duration),
) -> io::Result<T> {
    for attempt in 1..=LEDGER_LOCK_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if should_retry(&error) && attempt < LEDGER_LOCK_ATTEMPTS => {
                eprintln!(
                    "Bloodborne ledger: {action} hit a transient Windows file lock; \
                     retrying ({attempt}/{LEDGER_LOCK_ATTEMPTS}) in {} ms: {error}",
                    LEDGER_LOCK_RETRY_DELAY.as_millis()
                );
                sleep(LEDGER_LOCK_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded retry loop always returns")
}

impl ReceiveLedger {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let temporary = path.with_extension("tmp");
        let backup = path.with_extension("bak");
        let bytes = json::to_vec_pretty(self)?;
        fs::write(&temporary, bytes).with_context(|| format!("writing {}", temporary.display()))?;
        if path.exists() {
            if backup.exists() {
                retry_sharing_violation("removing the previous ledger backup", || {
                    fs::remove_file(&backup)
                })
                .with_context(|| format!("removing previous backup {}", backup.display()))?;
            }
            retry_sharing_violation("backing up the receive ledger", || {
                fs::rename(path, &backup)
            })
            .with_context(|| format!("backing up {}", path.display()))?;
        }
        if let Err(error) = retry_sharing_violation("publishing the receive ledger", || {
            fs::rename(&temporary, path)
        }) {
            if backup.exists()
                && let Err(restore_error) =
                    retry_sharing_violation("restoring the receive ledger backup", || {
                        fs::rename(&backup, path)
                    })
            {
                return Err(anyhow::Error::new(error)).with_context(|| {
                    format!(
                        "publishing {}; restoring {} also failed: {restore_error}",
                        path.display(),
                        backup.display()
                    )
                });
            }
            return Err(error).with_context(|| format!("publishing {}", path.display()));
        }
        if let Err(error) = retry_sharing_violation("removing the committed ledger backup", || {
            fs::remove_file(&backup)
        }) && error.kind() != io::ErrorKind::NotFound
        {
            // The new ledger is already durable. Leaving the prior generation
            // behind is safer than reporting the completed save as failed.
            eprintln!(
                "Bloodborne ledger: committed {}, but could not remove {}: {error}",
                path.display(),
                backup.display()
            );
        }
        Ok(())
    }
}

impl ReceiveLedger {
    pub fn slot_key(seed_name: &str, slot_name: &str) -> String {
        format!("{seed_name}\u{1f}{slot_name}")
    }

    pub fn slot_mut(&mut self, seed_name: &str, slot_name: &str) -> &mut SlotLedger {
        self.slots
            .entry(Self::slot_key(seed_name, slot_name))
            .or_default()
    }

    pub fn slot(&self, seed_name: &str, slot_name: &str) -> Option<&SlotLedger> {
        self.slots.get(&Self::slot_key(seed_name, slot_name))
    }
}

impl SlotLedger {
    /// The next AP index to deliver: a requeued index first (clients#427),
    /// otherwise one past the cursor.
    pub fn next_index(&self) -> u64 {
        self.redeliver
            .iter()
            .next()
            .copied()
            .unwrap_or_else(|| self.highest_processed_index.map_or(0, |index| index + 1))
    }

    pub fn delivered_quantity(
        &self,
        normalized_item_id: u32,
        reinforcement_level: Option<u8>,
    ) -> u32 {
        self.acknowledged
            .values()
            .filter(|item| {
                item.normalized_item_id == normalized_item_id
                    && item.reinforcement_level == reinforcement_level
            })
            .map(|item| item.quantity)
            .fold(0, u32::saturating_add)
    }

    pub fn begin(&mut self, pending: PendingItem) -> Result<()> {
        anyhow::ensure!(
            pending.index == self.next_index(),
            "pending item is out of order"
        );
        if let Some(existing) = &self.pending {
            // The observed baseline is sampled state, not part of the plan: a
            // re-plan of the same item legitimately arrives without one.
            let mut candidate = pending;
            candidate.observed_before = existing.observed_before;
            anyhow::ensure!(
                existing == &candidate,
                "pending item plan changed before acknowledgement"
            );
            return Ok(());
        }
        self.pending = Some(pending);
        Ok(())
    }

    pub fn pending_for(&self, index: u64, ap_item_id: i64) -> Result<Option<&PendingItem>> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(None);
        };
        anyhow::ensure!(
            pending.index == index && pending.ap_item_id == ap_item_id,
            "received item does not match the durable pending plan"
        );
        Ok(Some(pending))
    }

    /// Record the live baseline this grant was submitted against (clients#427).
    /// Written durably BEFORE the grant can execute, so a restart replays the
    /// interrupted command against the same number.
    pub fn record_observed_before(&mut self, observed: u32) -> Result<()> {
        let pending = self
            .pending
            .as_mut()
            .context("no pending item to record an observed baseline for")?;
        pending.observed_before = Some(observed);
        Ok(())
    }

    /// Forget the recorded baseline because the command it belonged to was
    /// proven unexecuted and withdrawn (clients#296 + clients#427). The next
    /// publication samples the live stack again, so a player who spends between
    /// the withdrawal and the retry is not parked on a stale number.
    pub fn clear_observed_before(&mut self) -> bool {
        match self.pending.as_mut() {
            Some(pending) => pending.observed_before.take().is_some(),
            None => false,
        }
    }

    pub fn mark_grant_complete(&mut self) -> Result<()> {
        let pending = self
            .pending
            .as_mut()
            .context("no pending item to mark granted")?;
        pending.grant_complete = true;
        Ok(())
    }

    pub fn mark_equip_complete(&mut self) -> Result<()> {
        let pending = self
            .pending
            .as_mut()
            .context("no pending item to mark equipped")?;
        anyhow::ensure!(
            pending.grant_complete,
            "cannot equip before grant completion"
        );
        pending.equip_complete = true;
        Ok(())
    }

    /// Park an item whose grant the harness terminally failed (clients#399):
    /// acknowledge it in order WITHOUT the grant/equip completion ensures, with
    /// the failure detail recorded on the entry. The stream continues with the
    /// next index and every cursor invariant still holds; the parked record is
    /// the operator's evidence for bb-blocked. Never call this for a grant that
    /// might still execute.
    pub fn acknowledge_blocked(&mut self, index: u64, item: AcknowledgedItem) -> Result<()> {
        anyhow::ensure!(
            item.blocked.is_some(),
            "a parked acknowledgement must carry the failure detail"
        );
        anyhow::ensure!(
            index == self.next_index(),
            "blocked acknowledgement is out of order"
        );
        let pending = self
            .pending
            .as_ref()
            .context("cannot park an item without a durable pending plan")?;
        anyhow::ensure!(pending.index == index, "pending item index changed");
        self.acknowledged.insert(index, item);
        self.advance_cursor(index);
        self.pending = None;
        Ok(())
    }

    /// Retire a delivered index: it leaves the requeue set, and the cursor only
    /// ever moves forward (a requeued index is BELOW it).
    fn advance_cursor(&mut self, index: u64) {
        self.redeliver.remove(&index);
        self.highest_processed_index = Some(
            self.highest_processed_index
                .map_or(index, |highest| highest.max(index)),
        );
    }

    /// The park reason recorded on a blocked entry: the harness status token,
    /// which `client_loop` writes as `"{status} ({detail})"`.
    fn park_status(item: &AcknowledgedItem) -> Option<&str> {
        let blocked = item.blocked.as_deref()?;
        Some(blocked.split(' ').next().unwrap_or(blocked))
    }

    /// The park detail recorded on a blocked entry, i.e. everything inside
    /// the parentheses of `"{status} ({detail})"`.
    fn park_detail(item: &AcknowledgedItem) -> Option<&str> {
        let blocked = item.blocked.as_deref()?;
        let open = blocked.find(" (")?;
        blocked[open + 2..].strip_suffix(')')
    }

    /// Whether a park is one of the two whose *cause is known to be fixed*, so
    /// re-delivering it either lands the item or fails for a real reason.
    ///
    /// * `quantity_mismatch` (clients#427) -- produced by the fresh-grant
    ///   precondition comparing the stack against the ledger's lifetime
    ///   delivered sum, which any spent consumable falsifies. That
    ///   precondition is now the observed live quantity.
    /// * `write_error` whose detail is exactly `quantity write failed`
    ///   (clients#433) -- produced by the external `WriteProcessMemory` into
    ///   the guest inventory page that shadPS4 intermittently refuses. That
    ///   lane is gone; existing-stack grants run on the game thread now.
    ///
    /// * `failed` on the goods lane with NO execution evidence
    ///   (`native_result=4294967295`, clients#613) -- the game refused the
    ///   call, so nothing was granted and re-delivery cannot duplicate. The
    ///   refused-insert-with-a-stack-present shape now re-plans as a delta.
    ///   A `failed` carrying any other `native_result` is execution evidence
    ///   (clients#443) and stays parked.
    ///
    /// Every other park stays put for `bb-blocked`, because its cause is not
    /// known to be fixed. In particular `write_error (... quantity pointer
    /// missing)` is NOT requeued: a missing record pointer is a geometry
    /// problem, not the refused write.
    fn park_cause_is_fixed(item: &AcknowledgedItem) -> bool {
        match Self::park_status(item) {
            Some("quantity_mismatch") => true,
            Some("write_error") => {
                Self::park_detail(item).is_some_and(|d| d.ends_with("quantity write failed"))
            }
            Some("failed") => Self::park_detail(item).is_some_and(|d| {
                d.contains("expected_after=") && d.contains("native_result=4294967295")
            }),
            _ => false,
        }
    }

    /// Requeue every park whose cause is known to be fixed
    /// (clients#427, clients#433). Returns the requeued indices in order.
    pub fn requeue_fixed_cause_parks(&mut self) -> Vec<u64> {
        let indices: Vec<u64> = self
            .acknowledged
            .iter()
            .filter(|(_, item)| Self::park_cause_is_fixed(item))
            .map(|(index, _)| *index)
            .collect();
        for index in &indices {
            self.acknowledged.remove(index);
            self.redeliver.insert(*index);
        }
        indices
    }

    /// The parked (blocked) entries, for bb-blocked's listing.
    pub fn blocked_entries(&self) -> impl Iterator<Item = (u64, &AcknowledgedItem)> {
        self.acknowledged
            .iter()
            .filter(|(_, item)| item.blocked.is_some())
            .map(|(index, item)| (*index, item))
    }

    /// Requeue one terminally parked delivery for the normal, ordered receive
    /// pipeline. The caller is responsible for operator confirmation and save
    /// identity validation; this method deliberately creates no second grant
    /// path.
    pub fn requeue_blocked(&mut self, index: u64) -> Result<()> {
        let item = self
            .acknowledged
            .get(&index)
            .with_context(|| format!("no acknowledged entry at index {index}"))?;
        anyhow::ensure!(
            item.blocked.is_some(),
            "entry at index {index} is not a parked delivery"
        );
        anyhow::ensure!(self.pending.is_none(), "a delivery is already pending");
        self.acknowledged.remove(&index);
        self.redeliver.insert(index);
        Ok(())
    }

    /// Operator-confirmed resolution of a parked entry (bb-blocked INDEX
    /// --confirm): clears the blocked marker after the operator has verified
    /// the item physically arrived. Returns the detail that was cleared.
    /// Never re-grants -- re-issuing an already-delivered item duplicates it.
    pub fn unblock(&mut self, index: u64) -> Result<String> {
        let item = self
            .acknowledged
            .get_mut(&index)
            .with_context(|| format!("no acknowledged entry at index {index}"))?;
        item.blocked
            .take()
            .with_context(|| format!("entry at index {index} is not blocked"))
    }

    pub fn acknowledge(&mut self, index: u64, item: AcknowledgedItem) -> Result<()> {
        anyhow::ensure!(
            index == self.next_index(),
            "item acknowledgement is out of order"
        );
        let pending = self
            .pending
            .as_ref()
            .context("cannot acknowledge an item without a durable pending plan")?;
        anyhow::ensure!(pending.index == index, "pending item index changed");
        anyhow::ensure!(
            pending.grant_complete,
            "cannot acknowledge before grant completion"
        );
        anyhow::ensure!(
            pending.equip_target.is_none() || pending.equip_complete,
            "cannot acknowledge before equip completion"
        );
        self.acknowledged.insert(index, item);
        self.advance_cursor(index);
        self.pending = None;
        Ok(())
    }

    /// Rewind the durable cursor so that `first_missing` is the next index to
    /// process, dropping every acknowledged entry from that index on and any
    /// pending plan (it re-plans identically from the seed contract on the
    /// next poll). Returns how many acknowledged entries were rewound.
    ///
    /// This is only sound against a *proven* regression (a save restore, or an
    /// operator attestation): never drive it from inventory absence
    /// (docs/SAVE-RECONCILIATION.md §4).
    pub fn rewind_to(&mut self, first_missing: u64) -> usize {
        let before = self.acknowledged.len();
        self.acknowledged.retain(|index, _| *index < first_missing);
        self.redeliver.retain(|index| *index < first_missing);
        self.highest_processed_index = first_missing.checked_sub(1);
        self.pending = None;
        before - self.acknowledged.len()
    }

    /// Operator-attested restore (docs/SAVE-RECONCILIATION.md §5 MVP): "I
    /// restored the save to before index K". Rewinds so K is re-issued. The
    /// save watermark is untouched -- attested mode has none -- and the ledger
    /// stays loadable by builds that predate the watermark field.
    pub fn attest_restore(&mut self, first_missing: u64) -> usize {
        self.rewind_to(first_missing)
    }

    /// Compare the save-resident watermark with the durable cursor and apply
    /// the one defined outcome (docs/SAVE-RECONCILIATION.md §5). Identity
    /// mismatches never reach here: `require_runtime_context` refuses first.
    pub fn reconcile_save_watermark(&mut self, observed: Option<u64>) -> WatermarkOutcome {
        let recorded = self.save_watermark;
        let Some(watermark) = observed else {
            // No watermark support at all is the attested-mode status quo; a
            // slot that HAS recorded a watermark and now cannot read it holds.
            return if recorded.is_some() {
                WatermarkOutcome::Hold
            } else {
                WatermarkOutcome::Resume
            };
        };
        let cursor = self.highest_processed_index;
        if cursor == Some(watermark) {
            self.save_watermark = Some(watermark);
            return WatermarkOutcome::Resume;
        }
        if cursor.is_none_or(|highest| watermark > highest) {
            // The save is ahead of the ledger: ledger loss or rollback. Adopt
            // the save cursor and re-grant nothing (I1) -- the acknowledged
            // entries before the cursor may be gone, and that is accepted
            // rather than reconstructed.
            self.highest_processed_index = Some(watermark);
            self.pending = None;
            self.save_watermark = Some(watermark);
            return WatermarkOutcome::AdoptSaveCursor;
        }
        // The save is behind the ledger: a proven restore. Rewind so the
        // erased tail is re-issued in order.
        self.rewind_to(watermark + 1);
        self.save_watermark = Some(watermark);
        WatermarkOutcome::Reissue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transient_test_error() -> io::Error {
        io::Error::other("synthetic sharing violation")
    }

    #[test]
    fn transient_file_lock_recovers_within_the_bound() {
        let mut calls = 0;
        let mut sleeps = Vec::new();
        let result = retry_io_with(
            "testing ledger rotation",
            || {
                calls += 1;
                if calls < 3 {
                    Err(transient_test_error())
                } else {
                    Ok("published")
                }
            },
            |_| true,
            |delay| sleeps.push(delay),
        )
        .unwrap();

        assert_eq!(result, "published");
        assert_eq!(calls, 3);
        assert_eq!(sleeps, vec![LEDGER_LOCK_RETRY_DELAY; 2]);
    }

    #[test]
    fn persistent_file_lock_stops_at_the_retry_bound() {
        let mut calls = 0;
        let mut sleeps = 0;
        let error = retry_io_with::<()>(
            "testing ledger rotation",
            || {
                calls += 1;
                Err(transient_test_error())
            },
            |_| true,
            |_| sleeps += 1,
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "synthetic sharing violation");
        assert_eq!(calls, LEDGER_LOCK_ATTEMPTS);
        assert_eq!(sleeps, LEDGER_LOCK_ATTEMPTS - 1);
    }

    #[test]
    fn non_retryable_io_error_fails_immediately() {
        let mut calls = 0;
        let mut slept = false;
        let error = retry_io_with::<()>(
            "testing ledger rotation",
            || {
                calls += 1;
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            },
            |_| false,
            |_| slept = true,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 1);
        assert!(!slept);
    }

    #[test]
    fn missing_ledger_starts_at_zero() {
        let path = std::env::temp_dir().join(format!(
            "bb-ledger-missing-{}-{}.json",
            std::process::id(),
            u64::MAX
        ));
        let _ = fs::remove_file(&path);
        assert_eq!(
            ReceiveLedger::load(&path).unwrap(),
            ReceiveLedger::default()
        );
    }

    fn park(slot: &mut SlotLedger, index: u64, status: &str) {
        slot.begin(PendingItem {
            index,
            ap_item_id: 1,
            raw_descriptor: 0xB000_04CE,
            normalized_item_id: 0x4000_04CE,
            item_category: 4,
            quantity: 1,
            upgrade_target_level: None,
            reinforcement_level: None,
            equip_target: None,
            grant_complete: false,
            equip_complete: false,
            observed_before: None,
        })
        .unwrap();
        slot.acknowledge_blocked(
            index,
            AcknowledgedItem {
                ap_item_id: 1,
                raw_descriptor: 0xB000_04CE,
                normalized_item_id: 0x4000_04CE,
                item_category: 4,
                quantity: 1,
                reinforcement_level: None,
                equip_target: None,
                blocked: Some(format!("{status} (mock detail)")),
            },
        )
        .unwrap();
    }

    /// A minimal pending plan for one Pebble, at the given index.
    fn pending_pebble(index: u64) -> PendingItem {
        PendingItem {
            index,
            ap_item_id: 1,
            raw_descriptor: 0xB000_04CE,
            normalized_item_id: 0x4000_04CE,
            item_category: 4,
            quantity: 1,
            reinforcement_level: None,
            equip_target: None,
            upgrade_target_level: None,
            grant_complete: false,
            equip_complete: false,
            observed_before: None,
        }
    }

    /// The same, with the exact detail text `client_loop` records.
    fn park_with(slot: &mut SlotLedger, index: u64, status: &str, detail: &str) {
        park(slot, index, status);
        slot.acknowledged
            .get_mut(&index)
            .unwrap()
            .blocked
            .replace(format!("{status} ({detail})"));
    }

    /// clients#433: a `write_error` park re-enters the queue if and only if its
    /// detail is the shadPS4-refused quantity write, whose lane is gone. A
    /// `quantity pointer missing` write_error is a different cause and stays
    /// parked -- the status alone is NOT the discriminator.
    #[test]
    fn only_the_refused_quantity_write_requeues_among_write_errors() {
        let mut slot = SlotLedger::default();
        park_with(
            &mut slot,
            0,
            "write_error",
            "tag=ap_0 quantity write failed",
        );
        park_with(
            &mut slot,
            1,
            "write_error",
            "tag=ap_1 quantity pointer missing",
        );
        park_with(&mut slot, 2, "failed", "tag=ap_2 quantity write failed");
        park_with(
            &mut slot,
            3,
            "quantity_mismatch",
            "tag=ap_3 expected_before=5 actual=20",
        );

        assert_eq!(slot.requeue_fixed_cause_parks(), vec![0, 3]);
        let still_parked: Vec<u64> = slot.blocked_entries().map(|(index, _)| index).collect();
        assert_eq!(still_parked, vec![1, 2]);
    }

    /// clients#613: a goods-lane `failed` with the result cell still the
    /// sentinel is a call the game refused. Nothing landed, so re-delivery is
    /// safe, and the refused insert now re-plans as a delta. Jennifer's live
    /// park is verbatim below. The instance-insert shape (no `expected_after`)
    /// is not covered: its evidence rules are different (clients#451).
    #[test]
    fn a_refused_goods_grant_without_execution_evidence_requeues() {
        let mut slot = SlotLedger::default();
        park_with(
            &mut slot,
            0,
            "failed",
            "tag=ap_43 expected_after=2 actual=Some(10) native_result=4294967295 retry_budget=20",
        );
        park_with(
            &mut slot,
            1,
            "failed",
            "tag=ap_44 instance insert unwitnessed: native_result=4294967295 slot_record=None retry_budget=20",
        );
        assert_eq!(slot.requeue_fixed_cause_parks(), vec![0]);
        let still_parked: Vec<u64> = slot.blocked_entries().map(|(index, _)| index).collect();
        assert_eq!(still_parked, vec![1]);
    }

    /// clients#443: a park carrying EXECUTION EVIDENCE means the item landed,
    /// so the startup unpark must never touch it -- requeueing would deliver a
    /// second copy. oz's live park is verbatim below. It cannot match today
    /// (the status is `failed`, and the filter takes only `quantity_mismatch`
    /// and the refused quantity write), and this pins that it stays that way:
    /// the operator resolves such an entry with `bb-blocked --confirm`, never
    /// by redelivering it.
    #[test]
    fn an_execution_evidenced_park_is_never_requeued() {
        let mut slot = SlotLedger::default();
        park_with(
            &mut slot,
            0,
            "failed",
            "tag=ap_0 expected_after=7 actual=Some(8) native_result=8 retry_budget=20",
        );
        park_with(
            &mut slot,
            1,
            "quantity_mismatch",
            "tag=ap_1 expected_before=5 actual=20",
        );
        assert_eq!(
            slot.requeue_fixed_cause_parks(),
            vec![1],
            "a delivered item must not re-enter the delivery queue"
        );
        let still_parked: Vec<u64> = slot.blocked_entries().map(|(index, _)| index).collect();
        assert_eq!(still_parked, vec![0]);

        // And the operator's resolution is the exit: it clears the marker
        // without ever re-granting.
        let detail = slot.unblock(0).unwrap();
        assert!(detail.contains("native_result=8"));
        assert_eq!(slot.blocked_entries().count(), 0);
        assert!(
            !slot.redeliver.contains(&0),
            "resolving is not redelivering"
        );
    }

    /// clients#443: a completion driven by concurrent inventory activity --
    /// in EITHER direction, a pickup above `expected_after` or a spend/storage
    /// overflow below it -- is acknowledged like any other, and
    /// the acknowledgement drops the pending plan -- baseline included. So the
    /// NEXT grant of the same item plans with `observed_before: None` and
    /// re-observes the live stack, which is the only reading that includes the
    /// player's concurrent activity. Neither direction can poison the next
    /// grant, and the ledger never needs to know which one happened.
    #[test]
    fn a_completion_drops_the_baseline_so_the_next_grant_re_observes() {
        let mut slot = SlotLedger::default();
        let mut first = pending_pebble(0);
        slot.begin(first.clone()).unwrap();
        // The grant was submitted against a live reading of 5.
        slot.record_observed_before(5).unwrap();
        slot.mark_grant_complete().unwrap();
        first.observed_before = Some(5);
        first.grant_complete = true;
        assert_eq!(slot.pending, Some(first));

        slot.acknowledge(0, acknowledged(0)).unwrap();
        assert!(
            slot.pending.is_none(),
            "the acknowledged baseline must not survive its grant"
        );
        // The AP item's own quantity is what is recorded -- the pickup is the
        // player's and belongs to no delivery.
        assert_eq!(slot.delivered_quantity(0x4000_04CE, None), 1);

        // The next grant of the SAME item carries no baseline at all, so the
        // client samples the live stack (pickup included) instead of the
        // number the previous grant expected.
        slot.begin(pending_pebble(1)).unwrap();
        assert_eq!(slot.pending.as_ref().unwrap().observed_before, None);
    }

    /// clients#427: only the park reason this issue fixed re-enters the queue,
    /// it is delivered before anything new, and retiring it never regresses the
    /// cursor or re-delivers a neighbour.
    #[test]
    fn requeued_parks_are_delivered_before_the_cursor_without_regressing_it() {
        let mut slot = SlotLedger::default();
        park(&mut slot, 0, "quantity_mismatch");
        park(&mut slot, 1, "write_error");
        park(&mut slot, 2, "quantity_mismatch");
        assert_eq!(slot.next_index(), 3);

        assert_eq!(slot.requeue_fixed_cause_parks(), vec![0, 2]);
        assert_eq!(slot.blocked_entries().count(), 1);
        assert_eq!(slot.next_index(), 0);

        for index in [0, 2] {
            slot.begin(PendingItem {
                index,
                ap_item_id: 1,
                raw_descriptor: 0xB000_04CE,
                normalized_item_id: 0x4000_04CE,
                item_category: 4,
                quantity: 1,
                upgrade_target_level: None,
                reinforcement_level: None,
                equip_target: None,
                grant_complete: true,
                equip_complete: false,
                observed_before: Some(0),
            })
            .unwrap();
            slot.acknowledge(
                index,
                AcknowledgedItem {
                    ap_item_id: 1,
                    raw_descriptor: 0xB000_04CE,
                    normalized_item_id: 0x4000_04CE,
                    item_category: 4,
                    quantity: 1,
                    reinforcement_level: None,
                    equip_target: None,
                    blocked: None,
                },
            )
            .unwrap();
            // The cursor only ever moves forward.
            assert_eq!(slot.highest_processed_index, Some(2));
        }
        assert_eq!(slot.next_index(), 3);
        assert!(slot.redeliver.is_empty());
        // A second startup finds nothing left to requeue.
        assert!(slot.requeue_fixed_cause_parks().is_empty());
        assert_eq!(slot.blocked_entries().count(), 1);
    }

    /// clients#427: the observed baseline is sampled state, not part of the
    /// plan -- a re-plan of the same item must not read as a changed plan.
    #[test]
    fn a_recorded_baseline_does_not_make_a_replan_look_changed() {
        let mut slot = SlotLedger::default();
        let plan = PendingItem {
            index: 0,
            ap_item_id: 1,
            raw_descriptor: 0xB000_04CE,
            normalized_item_id: 0x4000_04CE,
            item_category: 4,
            quantity: 1,
            upgrade_target_level: None,
            reinforcement_level: None,
            equip_target: None,
            grant_complete: false,
            equip_complete: false,
            observed_before: None,
        };
        slot.begin(plan.clone()).unwrap();
        slot.record_observed_before(7).unwrap();
        slot.begin(plan.clone()).unwrap();
        assert_eq!(slot.pending.as_ref().unwrap().observed_before, Some(7));
        assert!(slot.clear_observed_before());
        assert!(!slot.clear_observed_before());

        let mut changed = plan;
        changed.quantity = 2;
        slot.begin(changed).unwrap_err();
    }

    #[test]
    fn ledgers_are_isolated_by_seed_and_slot() {
        let mut ledger = ReceiveLedger::default();
        ledger
            .slot_mut("seed-a", "hunter")
            .acknowledge(
                0,
                AcknowledgedItem {
                    ap_item_id: 1,
                    raw_descriptor: 0xB000_04CE,
                    normalized_item_id: 0x4000_04CE,
                    item_category: 4,
                    quantity: 1,
                    reinforcement_level: None,
                    equip_target: None,
                    blocked: None,
                },
            )
            .unwrap_err();
        ledger
            .slot_mut("seed-a", "hunter")
            .begin(PendingItem {
                index: 0,
                ap_item_id: 1,
                raw_descriptor: 0xB000_04CE,
                normalized_item_id: 0x4000_04CE,
                item_category: 4,
                quantity: 1,
                reinforcement_level: None,
                equip_target: None,
                upgrade_target_level: None,
                grant_complete: false,
                equip_complete: false,
                observed_before: None,
            })
            .unwrap();
        ledger
            .slot_mut("seed-a", "hunter")
            .mark_grant_complete()
            .unwrap();
        ledger
            .slot_mut("seed-a", "hunter")
            .acknowledge(
                0,
                AcknowledgedItem {
                    ap_item_id: 1,
                    raw_descriptor: 0xB000_04CE,
                    normalized_item_id: 0x4000_04CE,
                    item_category: 4,
                    quantity: 1,
                    reinforcement_level: None,
                    equip_target: None,
                    blocked: None,
                },
            )
            .unwrap();
        assert_eq!(ledger.slot("seed-a", "hunter").unwrap().next_index(), 1);
        assert!(ledger.slot("seed-a", "other").is_none());
        assert!(ledger.slot("seed-b", "hunter").is_none());
    }

    #[test]
    fn pending_plan_survives_serialization_and_cannot_drift() {
        let mut ledger = ReceiveLedger::default();
        let slot = ledger.slot_mut("seed", "slot");
        let pending = PendingItem {
            index: 0,
            ap_item_id: 9,
            raw_descriptor: 0x8010_0000,
            normalized_item_id: 0x1000,
            item_category: 0,
            quantity: 1,
            reinforcement_level: Some(6),
            equip_target: Some(EquipTarget::RightHand(0)),
            upgrade_target_level: Some(6),
            grant_complete: false,
            equip_complete: false,
            observed_before: None,
        };
        slot.begin(pending.clone()).unwrap();
        let bytes = json::to_vec(&ledger).unwrap();
        let decoded: ReceiveLedger = json::from_slice(&bytes).unwrap();
        assert_eq!(
            decoded
                .slot("seed", "slot")
                .unwrap()
                .pending_for(0, 9)
                .unwrap(),
            Some(&pending)
        );
    }

    fn acknowledged(_index: u64) -> AcknowledgedItem {
        AcknowledgedItem {
            ap_item_id: 1,
            raw_descriptor: 0xB000_04CE,
            normalized_item_id: 0x4000_04CE,
            item_category: 4,
            quantity: 1,
            reinforcement_level: None,
            equip_target: None,
            blocked: None,
        }
    }

    fn slot_with_acks(up_to_inclusive: u64) -> SlotLedger {
        let mut slot = SlotLedger::default();
        for index in 0..=up_to_inclusive {
            slot.begin(PendingItem {
                index,
                ap_item_id: 1,
                raw_descriptor: 0xB000_04CE,
                normalized_item_id: 0x4000_04CE,
                item_category: 4,
                quantity: 1,
                reinforcement_level: None,
                equip_target: None,
                upgrade_target_level: None,
                grant_complete: false,
                equip_complete: false,
                observed_before: None,
            })
            .unwrap();
            slot.mark_grant_complete().unwrap();
            slot.acknowledge(index, acknowledged(index)).unwrap();
        }
        slot
    }

    #[test]
    fn older_ledgers_without_a_watermark_load_unchanged() {
        let mut slot = slot_with_acks(1);
        let legacy = json::to_vec(&slot).unwrap();
        // Simulate a pre-watermark receipt file by dropping the field.
        let mut value: json::Value = json::from_slice(&legacy).unwrap();
        value.as_object_mut().unwrap().remove("save_watermark");
        let mut decoded: SlotLedger = json::from_slice(&json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(decoded.save_watermark, None);
        assert_eq!(decoded.next_index(), 2);
        // Attested mode: nothing observed, nothing recorded -> resume.
        assert_eq!(
            decoded.reconcile_save_watermark(None),
            WatermarkOutcome::Resume
        );
        slot.save_watermark = None;
        assert_eq!(decoded, slot);
    }

    #[test]
    fn rewind_reissues_exactly_the_erased_tail() {
        let mut slot = slot_with_acks(3);
        assert_eq!(slot.rewind_to(2), 2);
        assert_eq!(slot.highest_processed_index, Some(1));
        assert_eq!(slot.next_index(), 2);
        assert_eq!(slot.acknowledged.len(), 2);
        assert!(slot.pending.is_none());
        // The cursor can be rewound past zero.
        assert_eq!(slot.rewind_to(0), 2);
        assert_eq!(slot.highest_processed_index, None);
        assert_eq!(slot.next_index(), 0);
        assert!(slot.acknowledged.is_empty());
    }

    #[test]
    fn watermark_reconciliation_assigns_exactly_one_outcome_per_shape() {
        // Resume: save and ledger agree.
        let mut slot = slot_with_acks(2);
        slot.save_watermark = Some(2);
        assert_eq!(
            slot.reconcile_save_watermark(Some(2)),
            WatermarkOutcome::Resume
        );
        assert_eq!(slot.next_index(), 3);

        // Reissue: the save regressed (restore) -> rewind the erased tail.
        let mut slot = slot_with_acks(3);
        slot.save_watermark = Some(3);
        assert_eq!(
            slot.reconcile_save_watermark(Some(1)),
            WatermarkOutcome::Reissue
        );
        assert_eq!(slot.next_index(), 2);
        assert_eq!(slot.acknowledged.len(), 2);
        assert_eq!(slot.save_watermark, Some(1));

        // Adopt: the ledger regressed (loss/rollback) -> no re-grant.
        let mut slot = slot_with_acks(1);
        slot.save_watermark = Some(1);
        assert_eq!(
            slot.reconcile_save_watermark(Some(5)),
            WatermarkOutcome::AdoptSaveCursor
        );
        assert_eq!(slot.next_index(), 6);
        assert_eq!(slot.acknowledged.len(), 2);
        assert_eq!(slot.save_watermark, Some(5));

        // A fresh ledger against a played save is the same adopt shape.
        let mut slot = SlotLedger::default();
        assert_eq!(
            slot.reconcile_save_watermark(Some(4)),
            WatermarkOutcome::AdoptSaveCursor
        );
        assert_eq!(slot.next_index(), 5);

        // Hold: a watermark was recorded but is now unreadable.
        let mut slot = slot_with_acks(2);
        slot.save_watermark = Some(2);
        assert_eq!(slot.reconcile_save_watermark(None), WatermarkOutcome::Hold);
        assert_eq!(slot.next_index(), 3);
    }

    #[test]
    fn acknowledge_blocked_parks_in_order_without_grant_completion() {
        let mut slot = SlotLedger::default();
        slot.begin(PendingItem {
            index: 0,
            ap_item_id: 1,
            raw_descriptor: 0xB000_04CE,
            normalized_item_id: 0x4000_04CE,
            item_category: 4,
            quantity: 2,
            reinforcement_level: None,
            equip_target: None,
            upgrade_target_level: None,
            grant_complete: false,
            equip_complete: false,
            observed_before: None,
        })
        .unwrap();
        // The grant never completed: the ordinary acknowledge refuses, the
        // parking one does not, and the detail is the operator's evidence.
        slot.acknowledge(0, acknowledged(0)).unwrap_err();
        let mut parked = acknowledged(0);
        parked.blocked = Some("failed (tag=ap_0 expected_after=2 actual=10)".to_string());
        slot.acknowledge_blocked(0, parked.clone()).unwrap();
        assert_eq!(slot.next_index(), 1);
        assert!(slot.pending.is_none());
        assert_eq!(slot.acknowledged[&0], parked);
        assert_eq!(
            slot.blocked_entries().collect::<Vec<_>>(),
            vec![(0, &parked)]
        );

        // A parked entry must carry its detail, and the stream stays ordered.
        slot.acknowledge_blocked(1, acknowledged(1)).unwrap_err();
        slot.acknowledge_blocked(2, {
            let mut item = acknowledged(2);
            item.blocked = Some("failed".to_string());
            item
        })
        .unwrap_err();
    }

    #[test]
    fn blocked_entries_survive_serialization_and_predate_nothing() {
        let mut ledger = ReceiveLedger::default();
        let slot = ledger.slot_mut("seed", "slot");
        slot.begin(PendingItem {
            index: 0,
            ap_item_id: 1,
            raw_descriptor: 0xB000_04CE,
            normalized_item_id: 0x4000_04CE,
            item_category: 4,
            quantity: 2,
            reinforcement_level: None,
            equip_target: None,
            upgrade_target_level: None,
            grant_complete: false,
            equip_complete: false,
            observed_before: None,
        })
        .unwrap();
        let mut parked = acknowledged(0);
        parked.blocked = Some("failed (detail)".to_string());
        slot.acknowledge_blocked(0, parked).unwrap();
        let decoded: ReceiveLedger = json::from_slice(&json::to_vec(&ledger).unwrap()).unwrap();
        assert_eq!(decoded, ledger);

        // A ledger written before the field existed loads with blocked=None.
        let legacy = json::to_vec(&ledger).unwrap();
        let mut value: json::Value = json::from_slice(&legacy).unwrap();
        for slot in value["slots"].as_object_mut().unwrap().values_mut() {
            for item in slot["acknowledged"].as_object_mut().unwrap().values_mut() {
                item.as_object_mut().unwrap().remove("blocked");
            }
        }
        let decoded: ReceiveLedger = json::from_slice(&json::to_vec(&value).unwrap()).unwrap();
        assert!(
            decoded
                .slot("seed", "slot")
                .unwrap()
                .blocked_entries()
                .next()
                .is_none()
        );
    }
}
