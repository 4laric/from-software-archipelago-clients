//! The single-in-flight native delivery engine.
//!
//! Wraps one [`GrantSession`] and exposes a poll-per-call `grant` interface that
//! maps cleanly onto `BloodborneBackend::grant_item`: the client loop calls it
//! once per iteration, and it returns [`GrantStep::Pending`] until the delivery
//! reaches a terminal status. Only one grant is ever in flight -- the same
//! invariant the file bridge and the receive ledger's single `PendingItem`
//! already enforce.
//!
//! Category branch (contract `source_selection`): category-4 goods use the
//! in-frame descriptor, category-0 equipment the persistent one. Both are
//! chosen inside [`GrantSession`] by whether the raw id takes the
//! persistent-source branch. *Every* quantity change, existing stack or not,
//! goes through the cave on the game thread -- an insert (`request = 1`) when
//! the stack is absent, a delta (`request = 2`) when it exists. There is no
//! external-write lane any more (clients#433). The
//! raw/normalized descriptor pairing is validated here before anything is
//! queued, matching `FileBackend::grant_item` and `config.rs`.

use std::collections::HashMap;

use anyhow::{Result, bail};

use super::contract::{DescriptorFormula, Policy};
use super::delivery::{DurableState, GrantCommand, GrantSession, Runtime};
use super::descriptor::{CATEGORY_EQUIPMENT, CATEGORY_GOODS};

/// The outcome of one `grant` poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GrantStep {
    /// Not done yet; call again next iteration.
    Pending,
    /// The item is in the inventory (granted or recovered as already present).
    Complete,
    /// A terminal failure the caller should park (never retry blindly).
    Failed { status: String, detail: String },
}

/// A validated grant request from the client, before it becomes a
/// [`GrantCommand`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeGrantRequest {
    pub tag: String,
    pub raw_descriptor: u32,
    pub normalized_item_id: u32,
    pub item_category: u8,
    pub quantity: u32,
    /// Durable baseline from the client's ledger for replay recovery, or `None`
    /// to sample the live baseline.
    pub expected_before: Option<u32>,
}

impl NativeGrantRequest {
    /// Fail closed on any descriptor pairing the contract has not validated --
    /// the same category-4 / category-0 checks `FileBackend::grant_item` runs.
    fn into_command(self, formula: &DescriptorFormula) -> Result<GrantCommand> {
        match self.item_category {
            CATEGORY_GOODS => anyhow::ensure!(
                self.normalized_item_id & 0xF000_0000 == formula.goods_normalized_prefix
                    && self.raw_descriptor & 0xF000_0000 == formula.goods_raw_prefix
                    && (self.normalized_item_id & 0x0FFF_FFFF)
                        == (self.raw_descriptor & 0x0FFF_FFFF),
                "grant {} has an invalid category-4 raw/normalized descriptor pair",
                self.tag
            ),
            CATEGORY_EQUIPMENT => anyhow::ensure!(
                self.normalized_item_id & 0xF000_0000 == 0
                    && self.raw_descriptor & 0xF000_0000 == formula.persistent_source_marker
                    && (self.normalized_item_id & 0x0FFF_FFFF)
                        == (self.raw_descriptor & 0x0FFF_FFFF),
                "grant {} has an invalid category-0 raw/normalized descriptor pair",
                self.tag
            ),
            category => bail!(
                "grant {} uses unsupported Bloodborne item category {category}",
                self.tag
            ),
        }
        Ok(GrantCommand {
            raw_id: self.raw_descriptor,
            normalized_id: self.normalized_item_id,
            quantity: self.quantity,
            tag: self.tag,
            expected_before: self.expected_before,
        })
    }
}

pub struct NativeDelivery<R: Runtime> {
    session: GrantSession<R>,
    formula: DescriptorFormula,
    current_tag: Option<String>,
    finished: HashMap<String, GrantStep>,
    manual_trigger: bool,
}

impl<R: Runtime> NativeDelivery<R> {
    pub fn new(runtime: R, formula: DescriptorFormula, policy: Policy) -> Self {
        Self {
            session: GrantSession::new(runtime, formula, policy),
            formula,
            current_tag: None,
            finished: HashMap::new(),
            manual_trigger: false,
        }
    }

    /// Resume with a durable prior for the given tag, so a restart mid-grant can
    /// decide "already applied" (replay recovery). The caller is responsible for
    /// only supplying a prior that its ledger vouches for.
    pub fn with_prior(
        runtime: R,
        formula: DescriptorFormula,
        policy: Policy,
        prior: DurableState,
    ) -> Self {
        Self {
            session: GrantSession::with_prior(runtime, formula, policy, prior),
            formula,
            current_tag: None,
            finished: HashMap::new(),
            manual_trigger: false,
        }
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        self.session.runtime_mut()
    }

    /// The live view of one stack, for the client's fresh-grant baseline
    /// (clients#427). `None` means the inventory geometry is not hydrated --
    /// never "absent". Read-only: it queues nothing and cannot disturb an
    /// in-flight grant.
    pub fn observe_stack(&mut self, normalized_id: u32) -> Option<super::delivery::StackView> {
        let runtime = self.session.runtime_mut();
        if !runtime.inventory_ready() {
            return None;
        }
        runtime.find_stack(normalized_id)
    }

    /// Whether the command published for `tag` may already have applied
    /// (clients#427 follow-up). False only for the statuses that provably
    /// precede any write -- the command is held in the machine and nothing has
    /// been queued for the cave or written to the stack -- and for a tag this
    /// machine has no record of at all. Those are exactly the states in which
    /// the client's recorded baseline must be re-sampled instead of compared,
    /// because the only thing that can have moved the stack is the player.
    ///
    /// A durable prior restored by [`Self::with_prior`] is classified the same
    /// way, so a restart mid-execution still replays against its baseline.
    pub fn command_may_have_applied(&self, tag: &str) -> bool {
        if self.finished.contains_key(tag) {
            return true;
        }
        let state = self.session.state();
        if state.tag != tag {
            return false;
        }
        !matches!(
            state.status.as_str(),
            "queued" | "awaiting_inventory" | "busy"
        )
    }

    /// The live durable state of the current grant, for the client to persist.
    pub fn state(&self) -> &DurableState {
        self.session.state()
    }

    /// Advance the delivery of one request by one poll.
    pub fn grant(&mut self, request: NativeGrantRequest) -> Result<GrantStep> {
        let tag = request.tag.clone();
        if let Some(step) = self.finished.get(&tag) {
            return Ok(step.clone());
        }
        if self.current_tag.as_deref() != Some(tag.as_str()) {
            anyhow::ensure!(
                self.current_tag.is_none(),
                "native delivery is busy with {:?}; refusing to start {:?}",
                self.current_tag,
                tag
            );
            let command = request.into_command(&self.formula)?;
            self.session.submit(command, self.manual_trigger)?;
            self.current_tag = Some(tag.clone());
        }
        let status = self.session.poll();
        let step = classify(&status, self.session.state());
        if !matches!(step, GrantStep::Pending) {
            self.finished.insert(tag, step.clone());
            self.current_tag = None;
        }
        Ok(step)
    }

    /// Best-effort withdrawal of an in-flight request the client can no longer
    /// vouch for: clear the request cell if the native call has not completed.
    /// Returns `true` when an unexecuted request was actually cleared.
    ///
    /// A witnessed (`done`) native call is left alone -- deleting the arm signal
    /// cannot stop a call the cave already ran, and the completion is needed for
    /// the recovery path -- mirroring the file bridge's "unwitnessed" rule.
    pub fn withdraw_stale(&mut self) -> bool {
        let runtime = self.session.runtime_mut();
        let pending = runtime.request_pending();
        let done = runtime.native_done();
        self.current_tag = None;
        if pending && !done {
            runtime.clear_request();
            true
        } else {
            false
        }
    }
}

fn classify(status: &str, state: &DurableState) -> GrantStep {
    match status {
        "completed" | "recovered_complete" => GrantStep::Complete,
        "failed" | "quantity_mismatch" | "command_rejected" | "write_error" => GrantStep::Failed {
            status: status.to_string(),
            detail: state.detail.clone(),
        },
        _ => GrantStep::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::super::contract::contract;
    use super::super::delivery::{Runtime, SlotRecord, StackView};
    use super::super::descriptor::ItemGrantDescriptor;
    use super::*;
    use std::collections::HashMap;

    // A minimal cave-modelling runtime (a trimmed copy of delivery's fake):
    // the existing-stack delta lane, applied on the next `native_done`.
    #[derive(Default)]
    struct FakeRuntime {
        ready: bool,
        stacks: HashMap<u32, StackView>,
        queued: Option<(u32, u32, u32)>, // normalized id, delta, slot
        result: u32,
        writes: usize,
        // clients#443: quantity of the same id the player acquires while the
        // native call is in flight.
        concurrent_pickup: u32,
    }

    impl Runtime for FakeRuntime {
        fn inventory_ready(&mut self) -> bool {
            self.ready
        }
        fn find_stack(&mut self, normalized_id: u32) -> Option<StackView> {
            if !self.ready {
                return None;
            }
            Some(
                self.stacks
                    .get(&normalized_id)
                    .copied()
                    .unwrap_or(StackView {
                        quantity: 0,
                        exists: false,
                        slot: None,
                        quantity_address: None,
                    }),
            )
        }
        fn read_slot_record(&mut self, slot: u32) -> SlotRecord {
            let found = self
                .stacks
                .iter()
                .find(|(_, view)| view.slot == Some(slot))
                .map(|(id, view)| (*id, view.quantity, view.quantity_address));
            match found {
                Some((id, quantity, address)) => SlotRecord {
                    normalized_id: Some(id),
                    quantity: Some(quantity),
                    address,
                },
                None => SlotRecord::default(),
            }
        }
        fn write_quantity(&mut self, _address: u64, _value: u32) -> bool {
            // clients#433: nothing may reach this. Counted, never honoured.
            self.writes += 1;
            false
        }
        fn request_pending(&mut self) -> bool {
            self.queued.is_some()
        }
        fn queue_native(
            &mut self,
            d: &ItemGrantDescriptor,
            q: u32,
            s: Option<u32>,
            _a: Option<u64>,
            _m: bool,
        ) {
            self.queued = Some((d.normalized_id, q, s.unwrap_or(0)));
        }
        fn native_done(&mut self) -> bool {
            if let Some((normalized, delta, slot)) = self.queued.take() {
                let stack = self.stacks.entry(normalized).or_insert(StackView {
                    quantity: 0,
                    exists: true,
                    slot: Some(slot),
                    quantity_address: Some(0x2000),
                });
                stack.quantity += delta + self.concurrent_pickup;
                stack.exists = true;
                self.result = slot;
            }
            true
        }
        fn native_result(&mut self) -> u32 {
            self.result
        }
        fn clear_request(&mut self) {}
    }

    fn goods_request(goods: u32, qty: u32, tag: &str, before: Option<u32>) -> NativeGrantRequest {
        let d = ItemGrantDescriptor::for_goods(&contract().descriptor, goods).unwrap();
        NativeGrantRequest {
            tag: tag.into(),
            raw_descriptor: d.raw_id,
            normalized_item_id: d.normalized_id,
            item_category: 4,
            quantity: qty,
            expected_before: before,
        }
    }

    /// clients#443: a surplus completion is a COMPLETION at the engine seam.
    /// This is what the client loop consumes, and `GrantStep::Complete` is the
    /// only thing it needs to acknowledge the item in order, mark the grant
    /// complete, and move the cursor -- byte for byte the normal path, with
    /// the AP item's own quantity recorded and the player's pickup recorded
    /// nowhere. A `Failed` here is what parked oz's delivered item.
    #[test]
    fn a_concurrent_pickup_reaches_the_client_as_a_normal_completion() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime {
            ready: true,
            concurrent_pickup: 1,
            ..Default::default()
        };
        runtime.stacks.insert(
            normalized,
            StackView {
                quantity: 3,
                exists: true,
                slot: Some(1),
                quantity_address: Some(0x2000),
            },
        );
        let mut engine = NativeDelivery::new(runtime, contract().descriptor, contract().policy);
        assert_eq!(
            engine
                .grant(goods_request(0x384, 2, "ap_0", Some(3)))
                .unwrap(),
            GrantStep::Pending
        );
        let step = engine
            .grant(goods_request(0x384, 2, "ap_0", Some(3)))
            .unwrap();
        assert_eq!(
            step,
            GrantStep::Complete,
            "the client must acknowledge, not park: {:?}",
            engine.state()
        );
        // The stack really does hold the surplus, and the durable rows the
        // client persists still describe the GRANT, not the pickup.
        assert_eq!(engine.runtime_mut().stacks[&normalized].quantity, 6);
        assert_eq!(engine.state().expected_after, Some(5));
        assert!(engine.state().is_success());
        assert_eq!(engine.runtime_mut().writes, 0);
        // Idempotent like any other completion: re-asking never re-drives.
        assert_eq!(
            engine
                .grant(goods_request(0x384, 2, "ap_0", Some(3)))
                .unwrap(),
            GrantStep::Complete
        );
        assert_eq!(engine.runtime_mut().stacks[&normalized].quantity, 6);
    }

    /// clients#433: the existing-stack grant is the cave's delta lane. Two
    /// polls (queue, then verify the cave's completion), and the cached
    /// terminal step answers every later ask without re-driving.
    #[test]
    fn an_existing_stack_grant_completes_over_the_delta_lane_and_is_idempotent() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime {
            ready: true,
            ..Default::default()
        };
        runtime.stacks.insert(
            normalized,
            StackView {
                quantity: 3,
                exists: true,
                slot: Some(1),
                quantity_address: Some(0x2000),
            },
        );
        let mut engine = NativeDelivery::new(runtime, contract().descriptor, contract().policy);
        assert_eq!(
            engine
                .grant(goods_request(0x384, 2, "recv_9", Some(3)))
                .unwrap(),
            GrantStep::Pending
        );
        let step = engine
            .grant(goods_request(0x384, 2, "recv_9", Some(3)))
            .unwrap();
        assert_eq!(step, GrantStep::Complete);
        assert_eq!(engine.runtime_mut().writes, 0);
        // Asking again returns the cached terminal step without re-driving.
        assert_eq!(
            engine
                .grant(goods_request(0x384, 2, "recv_9", Some(3)))
                .unwrap(),
            GrantStep::Complete
        );
    }

    #[test]
    fn a_bad_descriptor_pairing_is_refused() {
        let mut engine = NativeDelivery::new(
            FakeRuntime {
                ready: true,
                ..Default::default()
            },
            contract().descriptor,
            contract().policy,
        );
        let bad = NativeGrantRequest {
            tag: "recv_bad".into(),
            raw_descriptor: 0x1234_5678, // wrong high nibble for category 4
            normalized_item_id: 0x4000_0384,
            item_category: 4,
            quantity: 1,
            expected_before: Some(0),
        };
        assert!(engine.grant(bad).is_err());
    }

    #[test]
    fn a_quantity_mismatch_is_a_failed_step() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime {
            ready: true,
            ..Default::default()
        };
        runtime.stacks.insert(
            normalized,
            StackView {
                quantity: 20,
                exists: true,
                slot: Some(1),
                quantity_address: Some(0x2000),
            },
        );
        let mut engine = NativeDelivery::new(runtime, contract().descriptor, contract().policy);
        let step = engine
            .grant(goods_request(0x384, 2, "recv_m", Some(5)))
            .unwrap();
        assert!(matches!(step, GrantStep::Failed { status, .. } if status == "quantity_mismatch"));
    }

    /// clients#427 follow-up: the machine tells the client whether a published
    /// command may already have applied. A command retained in
    /// `awaiting_inventory` (the stack is absent, the operator is being asked
    /// to acquire one) has not -- so the client re-observes instead of
    /// comparing the baseline it sampled before that wait. A command that ran
    /// has, and keeps its baseline for replay recovery.
    #[test]
    fn a_retained_command_has_not_applied_but_an_executed_one_has() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut engine = NativeDelivery::new(
            FakeRuntime::default(),
            contract().descriptor,
            contract().policy,
        );
        // Unknown tag: nothing was ever published, so nothing can have applied.
        assert!(!engine.command_may_have_applied("recv_1"));

        // Inventory is not hydrated: the command is retained, not executed.
        assert_eq!(
            engine
                .grant(goods_request(0x384, 2, "recv_1", Some(3)))
                .unwrap(),
            GrantStep::Pending
        );
        assert_eq!(engine.state().status, "awaiting_inventory");
        assert!(!engine.command_may_have_applied("recv_1"));

        // The stack shows up and the delta is queued: now it may have
        // applied, and stays that way once terminal.
        engine.runtime_mut().ready = true;
        engine.runtime_mut().stacks.insert(
            normalized,
            StackView {
                quantity: 3,
                exists: true,
                slot: Some(1),
                quantity_address: Some(0x2000),
            },
        );
        assert_eq!(
            engine
                .grant(goods_request(0x384, 2, "recv_1", Some(3)))
                .unwrap(),
            GrantStep::Pending
        );
        assert!(engine.command_may_have_applied("recv_1"));
        assert_eq!(
            engine
                .grant(goods_request(0x384, 2, "recv_1", Some(3)))
                .unwrap(),
            GrantStep::Complete
        );
        assert!(engine.command_may_have_applied("recv_1"));
    }
}
