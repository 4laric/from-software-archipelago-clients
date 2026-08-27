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
use super::diagnostics::{DeliveryRecord, DiagnosticSink, GrantContext};

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
    /// clients#445. Disabled unless the client arms it; when armed, exactly one
    /// line per terminal grant. Nothing below ever branches on it.
    diagnostics: DiagnosticSink,
    /// The client-side context, refreshed by the loop through
    /// [`Self::set_context`]. Latched at submit, re-read at the terminal step.
    context: GrantContext,
    submit_context: GrantContext,
}

impl<R: Runtime> NativeDelivery<R> {
    pub fn new(runtime: R, formula: DescriptorFormula, policy: Policy) -> Self {
        Self {
            session: GrantSession::new(runtime, formula, policy),
            formula,
            current_tag: None,
            finished: HashMap::new(),
            manual_trigger: false,
            diagnostics: DiagnosticSink::disabled(),
            context: GrantContext::default(),
            submit_context: GrantContext::default(),
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
            diagnostics: DiagnosticSink::disabled(),
            context: GrantContext::default(),
            submit_context: GrantContext::default(),
        }
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        self.session.runtime_mut()
    }

    /// Arm the passive per-grant diagnostics (clients#445). Off until called.
    pub fn arm_diagnostics(&mut self, sink: DiagnosticSink) {
        self.diagnostics = sink;
    }

    pub fn diagnostics_armed(&self) -> bool {
        self.diagnostics.is_armed()
    }

    /// Refresh the client-side context stamped onto the next record. Cheap and
    /// idempotent; the loop calls it with state it already has in hand.
    pub fn set_context(&mut self, context: GrantContext) {
        self.context = context;
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
        self.grant_with_warning(request, &mut |_: &str| {})
    }

    /// [`Self::grant`] with an explicit warning sink, so the one-shot
    /// diagnostics write failure has somewhere to go that a test can read.
    pub fn grant_with_warning(
        &mut self,
        request: NativeGrantRequest,
        warn: &mut dyn FnMut(&str),
    ) -> Result<GrantStep> {
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
            self.submit_context = self.context;
            self.current_tag = Some(tag.clone());
        }
        let status = self.session.poll();
        let step = classify(&status, self.session.state());
        if !matches!(step, GrantStep::Pending) {
            self.emit_diagnostic(&status, matches!(step, GrantStep::Complete), warn);
            self.finished.insert(tag, step.clone());
            self.current_tag = None;
        }
        Ok(step)
    }

    /// One line per terminal grant. Nothing here can fail into the delivery:
    /// the sink swallows its own errors after a single warning.
    fn emit_diagnostic(&mut self, status: &str, is_success: bool, warn: &mut dyn FnMut(&str)) {
        if !self.diagnostics.is_armed() {
            return;
        }
        let record = DeliveryRecord::build(
            self.session.trace(),
            status,
            &self.session.state().detail,
            is_success,
            self.submit_context,
            self.context,
        );
        self.diagnostics.record(&record, warn);
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
    use super::super::diagnostics::DeliveryRecord;
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
        // clients#443's other direction: quantity spent -- or overflowed into
        // storage -- in the same window. This is the shape clients#445 counts.
        concurrent_spend: u32,
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
                stack.quantity = (stack.quantity + delta + self.concurrent_pickup)
                    .saturating_sub(self.concurrent_spend);
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

    // ----------------------------------------------------------------------
    // clients#445: the passive per-grant delivery diagnostic.
    // ----------------------------------------------------------------------

    #[derive(Clone, Default)]
    struct CapturingWriter {
        lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl super::super::diagnostics::DiagnosticWriter for CapturingWriter {
        fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.lines.lock().expect("lines").push(line.to_string());
            Ok(())
        }
    }

    struct RefusingWriter;

    impl super::super::diagnostics::DiagnosticWriter for RefusingWriter {
        fn write_line(&mut self, _line: &str) -> std::io::Result<()> {
            Err(std::io::Error::other("no diagnostics for you"))
        }
    }

    type CapturedLines = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    fn armed_engine(runtime: FakeRuntime) -> (NativeDelivery<FakeRuntime>, CapturedLines) {
        let writer = CapturingWriter::default();
        let lines = std::sync::Arc::clone(&writer.lines);
        let mut engine = NativeDelivery::new(runtime, contract().descriptor, contract().policy);
        engine.arm_diagnostics(super::super::diagnostics::DiagnosticSink::new(Box::new(
            writer,
        )));
        engine.set_context(super::super::diagnostics::GrantContext {
            gameplay_ready: Some(true),
            event_flags_armed: true,
        });
        (engine, lines)
    }

    fn stocked(quantity: u32) -> FakeRuntime {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime {
            ready: true,
            ..Default::default()
        };
        runtime.stacks.insert(
            normalized,
            StackView {
                quantity,
                exists: true,
                slot: Some(1),
                quantity_address: Some(0x2000),
            },
        );
        runtime
    }

    fn drain(
        engine: &mut NativeDelivery<FakeRuntime>,
        request: NativeGrantRequest,
    ) -> Result<GrantStep> {
        let mut step = GrantStep::Pending;
        for _ in 0..64 {
            step = engine.grant(request.clone())?;
            if !matches!(step, GrantStep::Pending) {
                return Ok(step);
            }
        }
        Ok(step)
    }

    fn only_record(lines: &CapturedLines) -> DeliveryRecord {
        let captured = lines.lock().expect("lines");
        assert_eq!(
            captured.len(),
            1,
            "exactly one record per grant: {captured:?}"
        );
        json::from_str(&captured[0]).expect("a diagnostics line is valid json")
    }

    /// The Saw Spear id, already owned at slot 1 -- the clients#451 shape:
    /// `find_stack` matches the weapon's record, so the pre-guard machine chose
    /// the delta lane and the diagnostics record said `delta`.
    fn owned_weapon() -> FakeRuntime {
        let mut runtime = FakeRuntime {
            ready: true,
            ..Default::default()
        };
        runtime.stacks.insert(
            0x006C_5660,
            StackView {
                quantity: 1,
                exists: true,
                slot: Some(1),
                quantity_address: Some(0x2000),
            },
        );
        runtime
    }

    fn weapon_request(tag: &str) -> NativeGrantRequest {
        NativeGrantRequest {
            tag: tag.into(),
            raw_descriptor: contract().descriptor.persistent_source_marker | 0x006C_5660,
            normalized_item_id: 0x006C_5660,
            item_category: 0,
            quantity: 1,
            expected_before: None,
        }
    }

    /// clients#451 at the diagnostics seam (clients#447): an equipment grant
    /// against an ALREADY-OWNED matching record must report lane `insert`, and
    /// must publish no count arithmetic -- a stack total for an instance record
    /// is not an instance count, so there is nothing to infer a destination
    /// from. Before the guard this record read `lane: "delta"`.
    #[test]
    fn an_owned_weapon_records_the_insert_lane() {
        let (mut engine, lines) = armed_engine(owned_weapon());
        let step = drain(&mut engine, weapon_request("ap_7")).unwrap();
        assert_eq!(step, GrantStep::Complete, "{step:?}");
        let record = only_record(&lines);
        assert_eq!(record.lane.as_deref(), Some("insert"));
        assert_eq!(record.source.as_deref(), Some("persistent"));
        assert_eq!(record.terminal_status, "completed");
        assert_eq!(record.observed_before, None);
        assert_eq!(record.expected_after, None);
        assert_eq!(record.readback_surplus, None);
        assert_eq!(record.inferred_destination, "unknown");
    }

    /// A plain, boring, successful delivery still writes its line -- that is the
    /// whole point of a PASSIVE diagnostic. A file that only ever records
    /// failures cannot answer "how often does this happen", which is what
    /// clients#445 is asking.
    #[test]
    fn a_completed_grant_writes_one_record_with_the_delivery_it_actually_saw() {
        let (mut engine, lines) = armed_engine(stocked(3));
        assert_eq!(
            drain(&mut engine, goods_request(0x384, 2, "ap_7", Some(3))).unwrap(),
            GrantStep::Complete
        );
        let record = only_record(&lines);
        assert_eq!(record.tag, "ap_7");
        assert_eq!(record.ap_index, Some(7));
        assert_eq!(record.quantity, 2);
        assert_eq!(record.observed_before, Some(3));
        assert_eq!(record.expected_after, Some(5));
        assert_eq!(record.lane.as_deref(), Some("delta"));
        assert_eq!(record.source.as_deref(), Some("in_frame"));
        assert_eq!(record.terminal_status, "completed");
        assert_eq!(record.readbacks.last().copied().flatten(), Some(5));
        assert_eq!(record.readback_surplus, Some(0));
        assert!(record.execution_evidence);
        assert_eq!(record.inferred_destination, "held");
        assert_eq!(record.gameplay_ready_at_submit, Some(true));
        assert_eq!(record.gameplay_ready_at_terminal, Some(true));
        assert!(record.event_flags_armed_at_terminal);
        assert_eq!(
            record.item_id_normalized,
            contract().descriptor.goods_normalized_prefix | 0x384
        );
    }

    /// clients#443's surplus direction: the player looted one more of the same
    /// id mid-flight. The held stack still absorbed the grant, so the inference
    /// is `held` and the surplus is recorded as the +1 it is.
    #[test]
    fn a_concurrent_pickup_completion_records_its_surplus_and_still_infers_held() {
        let mut runtime = stocked(3);
        runtime.concurrent_pickup = 1;
        let (mut engine, lines) = armed_engine(runtime);
        assert_eq!(
            drain(&mut engine, goods_request(0x384, 2, "ap_0", Some(3))).unwrap(),
            GrantStep::Complete
        );
        let record = only_record(&lines);
        assert_eq!(record.terminal_status, "completed");
        assert_eq!(record.expected_after, Some(5));
        assert_eq!(record.readbacks.last().copied().flatten(), Some(6));
        assert_eq!(record.readback_surplus, Some(1));
        assert_eq!(record.inferred_destination, "held");
        assert!(
            record.terminal_detail.contains("concurrent pickup"),
            "{}",
            record.terminal_detail
        );
    }

    /// clients#443's DEFICIT direction, which is the case clients#445 exists
    /// for: the cave provably executed and the held stack came in under
    /// `expected_after`. That is the shape a capped pouch overflowing into
    /// storage produces -- and a concurrent spend produces it too, which is why
    /// the field is `inferred_destination` and the value is `storage_suspected`
    /// rather than `storage`.
    #[test]
    fn an_executed_deficit_completion_records_the_deficit_and_suspects_storage() {
        let mut runtime = stocked(3);
        runtime.concurrent_spend = 2;
        let (mut engine, lines) = armed_engine(runtime);
        assert_eq!(
            drain(&mut engine, goods_request(0x384, 2, "ap_1", Some(3))).unwrap(),
            GrantStep::Complete
        );
        let record = only_record(&lines);
        assert_eq!(record.terminal_status, "completed");
        assert_eq!(record.expected_after, Some(5));
        assert_eq!(record.readbacks.last().copied().flatten(), Some(3));
        assert_eq!(record.readback_surplus, Some(-2));
        assert!(record.execution_evidence);
        assert_eq!(record.inferred_destination, "storage_suspected");
        assert!(
            record
                .terminal_detail
                .contains("concurrent spend or storage overflow"),
            "{}",
            record.terminal_detail
        );
    }

    /// A parked grant records its park, and infers nothing from it. The whole
    /// value of the file is that the parks and the completions are counted in
    /// the same place.
    #[test]
    fn a_parked_grant_records_its_park_and_infers_no_destination() {
        // Baseline 3 against a live stack of 9: off-baseline, so the machine
        // parks before anything is queued.
        let (mut engine, lines) = armed_engine(stocked(9));
        let step = drain(&mut engine, goods_request(0x384, 2, "ap_2", Some(3))).unwrap();
        assert!(matches!(step, GrantStep::Failed { .. }), "{step:?}");
        let record = only_record(&lines);
        assert_eq!(record.terminal_status, "quantity_mismatch");
        assert_eq!(record.observed_before, Some(3));
        assert_eq!(record.expected_after, Some(5));
        assert_eq!(record.readbacks.last().copied().flatten(), Some(9));
        assert!(!record.execution_evidence);
        assert_eq!(record.native_result, None, "nothing was ever queued");
        assert_eq!(record.inferred_destination, "unknown");
        assert!(
            record.terminal_detail.contains("expected_before=3"),
            "{}",
            record.terminal_detail
        );
    }

    /// The diagnostic must never become a new way for a delivery to fail. A
    /// writer that refuses every line still leaves the grant COMPLETE, and says
    /// so exactly once.
    #[test]
    fn a_refusing_writer_warns_once_and_the_item_is_still_delivered() {
        let mut engine = NativeDelivery::new(stocked(3), contract().descriptor, contract().policy);
        engine.arm_diagnostics(super::super::diagnostics::DiagnosticSink::new(Box::new(
            RefusingWriter,
        )));
        let mut warnings = Vec::new();
        let request = goods_request(0x384, 2, "ap_3", Some(3));
        let mut step = GrantStep::Pending;
        for _ in 0..64 {
            step = engine
                .grant_with_warning(request.clone(), &mut |line| warnings.push(line.to_string()))
                .expect("a diagnostics failure is never a grant error");
            if !matches!(step, GrantStep::Pending) {
                break;
            }
        }
        assert_eq!(step, GrantStep::Complete, "the item is delivered anyway");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("deliveries are unaffected"),
            "{warnings:?}"
        );
    }

    /// Off by default: a client that never arms the sink writes nothing and
    /// behaves exactly as it did before clients#445.
    #[test]
    fn diagnostics_are_off_until_armed() {
        let mut engine = NativeDelivery::new(stocked(3), contract().descriptor, contract().policy);
        assert!(!engine.diagnostics_armed());
        assert_eq!(
            drain(&mut engine, goods_request(0x384, 2, "ap_4", Some(3))).unwrap(),
            GrantStep::Complete
        );
    }
}
