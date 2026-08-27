//! The native-grant delivery state machine.
//!
//! A faithful port of `tools/bb_native_delivery/delivery.py` in the world repo,
//! which is itself the control flow of the Cheat Engine harness's `poll()`.
//! Every guest-memory access is behind the [`Runtime`] trait so the transitions
//! are host-testable with a fake. The semantics the CE table paid for live, and
//! that this port preserves exactly:
//!
//! * **hydration grace** -- a stack that merely *looks* absent right after a
//!   load gets `min_absent_polls` polls before the native-insert path is
//!   allowed, or an early declaration inserts a duplicate stack next to an
//!   invisible one;
//! * **bounded verify** -- `verify_polls` normally, `hydration_verify_polls`
//!   when the evidence shape is "not hydrated yet" rather than "contradicted";
//! * **verify against the reported slot**, not only a whole-inventory scan,
//!   which returns the first matching stack and is the wrong witness when the
//!   game merged into a stack that was invisible at queue time;
//! * **replay recovery** -- a durable `expected_before` from a previous process
//!   lets a restart decide "already applied" instead of granting twice;
//! * **fail closed** -- absent Blood Vial insertion is refused outright after
//!   the live `?ItemInfo?` (`0xF00003E8`) reproduction;
//! * **category before contents** -- the lane is chosen by the item's CATEGORY
//!   first and the inventory's contents second. Only a stackable category
//!   (goods) may take the delta branch; equipment always inserts, because a
//!   duplicate weapon is a second INSTANCE and a weapon record's "quantity"
//!   position is not a count (clients#451);
//! * **never write the guest heap from outside** -- an existing stack is bumped
//!   by the cave's existing-stack delta branch (`request = 2`) on the game
//!   thread, not by an external `WriteProcessMemory` into the inventory page.
//!   shadPS4 protection-tracks those pages: the write fails intermittently
//!   (bb-archipelago#144) and can wound the emulator, which is what parked
//!   oz's grants as `write_error: quantity write failed` (clients#433).
//!
//! Statuses are kept as the exact strings the `BBGRANT1` durable state uses
//! (`native-grant-state.txt`), both so this port can be compared line-for-line
//! with the Python reference and so a native backend and the file bridge speak
//! one status vocabulary.

use super::contract::{DescriptorFormula, Policy};
use super::descriptor::ItemGrantDescriptor;

/// `0xFFFFFFFF`: the empty/none slot sentinel the cave and cells use.
pub const EMPTY_SLOT: u32 = 0xFFFF_FFFF;
/// Goods id of the Blood Vial; the absent-insert of this id is refused.
pub const BLOOD_VIAL_GOODS_ID: u32 = 0x3E8;

/// A grant the machine is asked to deliver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantCommand {
    pub raw_id: u32,
    pub normalized_id: u32,
    pub quantity: u32,
    pub tag: String,
    /// `None` means "sample the live baseline and record it durably".
    pub expected_before: Option<u32>,
}

impl GrantCommand {
    pub fn validate(&self) -> Result<(), DeliveryError> {
        if !(1..=99).contains(&self.quantity) {
            return Err(DeliveryError(
                "grant quantity must be between 1 and 99".into(),
            ));
        }
        if self.tag.is_empty() || self.tag.chars().any(char::is_whitespace) {
            return Err(DeliveryError(
                "grant tag must be one non-empty token".into(),
            ));
        }
        Ok(())
    }
}

/// What an inventory scan found for one normalized id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackView {
    pub quantity: u32,
    pub exists: bool,
    pub slot: Option<u32>,
    pub quantity_address: Option<u64>,
}

/// A single inventory record read back by slot index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct SlotRecord {
    pub normalized_id: Option<u32>,
    pub quantity: Option<u32>,
    pub address: Option<u64>,
}

/// Everything the machine needs from the guest process. `None` from a scan
/// means "geometry unavailable", never "absent".
pub trait Runtime {
    fn inventory_ready(&mut self) -> bool;
    fn find_stack(&mut self, normalized_id: u32) -> Option<StackView>;
    fn read_slot_record(&mut self, slot: u32) -> SlotRecord;
    /// **Not used by the delivery machine, and it must stay that way.**
    ///
    /// An external write to a *guest-heap* address is exactly what clients#433
    /// removed: shadPS4 protection-tracks the inventory pages, so the write
    /// fails intermittently and can wound the emulator. Every quantity change
    /// now goes through [`Runtime::queue_native`], which runs on the game
    /// thread. The method is retained only so a fake can *witness* that no
    /// lane calls it; any future caller must be an eboot-image address, never
    /// an inventory record.
    fn write_quantity(&mut self, address: u64, value: u32) -> bool;
    fn request_pending(&mut self) -> bool;
    fn queue_native(
        &mut self,
        descriptor: &ItemGrantDescriptor,
        quantity: u32,
        slot: Option<u32>,
        quantity_address: Option<u64>,
        manual_trigger: bool,
    );
    fn native_done(&mut self) -> bool;
    fn native_result(&mut self) -> u32;
    fn clear_request(&mut self);
}

/// The rows the durable state carries so a crash between grant and
/// acknowledgement is decidable on the next launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableState {
    pub status: String,
    pub tag: String,
    pub expected_before: Option<u32>,
    pub expected_after: Option<u32>,
    pub detail: String,
}

impl Default for DurableState {
    fn default() -> Self {
        Self {
            status: "awaiting_inventory".into(),
            tag: String::new(),
            expected_before: None,
            expected_after: None,
            detail: String::new(),
        }
    }
}

impl DurableState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed"
                | "recovered_complete"
                | "failed"
                | "quantity_mismatch"
                | "command_rejected"
                | "write_error"
        )
    }

    pub fn is_success(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "recovered_complete")
    }
}

fn is_recoverable_prior(status: &str) -> bool {
    matches!(
        status,
        "executing"
            | "queued"
            | "verify_pending"
            | "recovery_pending"
            | "completed"
            | "recovered_complete"
    )
}

/// The maximum number of individual read-backs one [`GrantTrace`] retains.
/// The verify budget is small, but `hydration_verify_polls` is not bounded by
/// anything this module controls, so the vector is capped: the first half and
/// the most recent half are kept, and [`GrantTrace::readbacks_seen`] carries
/// the true count so a truncated list can never be mistaken for a short one.
const MAX_RECORDED_READBACKS: usize = 16;

/// A passive, write-only record of what one grant's delivery actually saw.
///
/// Every field here is a value the machine already computes for its own
/// decisions -- no extra guest read exists to populate any of it, and nothing
/// in the state machine ever branches on a [`GrantTrace`]. It exists so the
/// storage-routing question (clients#445) can be answered from normal play
/// instead of from a manual probe session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GrantTrace {
    pub tag: String,
    pub raw_id: u32,
    pub normalized_id: u32,
    pub quantity: u32,
    /// The baseline the machine resolved for this grant (durable or sampled).
    pub observed_before: Option<u32>,
    /// `observed_before + quantity`, the total the verify loop demands.
    pub expected_after: Option<u32>,
    /// `"insert"` or `"delta"`, once a lane has been chosen.
    pub lane: Option<&'static str>,
    /// `"persistent"` or `"in_frame"`, the descriptor source branch.
    pub source: Option<&'static str>,
    /// Held-stack totals read back after the native call, in order. `None` is
    /// "geometry unavailable", exactly as [`Runtime::find_stack`] means it.
    pub readbacks: Vec<Option<u32>>,
    /// How many read-backs actually happened, truncation included.
    pub readbacks_seen: u32,
    /// The last value of the cave's result cell the machine observed.
    pub native_result: Option<u32>,
    /// Verify attempts consumed.
    pub verify_polls: u32,
    /// The clients#443 evidence predicate: the native call reported done and
    /// the result cell is no longer the pre-arm sentinel, so the routine ran.
    pub execution_evidence: bool,
}

impl GrantTrace {
    fn begin(command: &GrantCommand) -> Self {
        Self {
            tag: command.tag.clone(),
            raw_id: command.raw_id,
            normalized_id: command.normalized_id,
            quantity: command.quantity,
            ..Self::default()
        }
    }

    fn record_readback(&mut self, value: Option<u32>) {
        self.readbacks_seen = self.readbacks_seen.saturating_add(1);
        if self.readbacks.len() == MAX_RECORDED_READBACKS {
            // Drop the oldest of the recent half, keeping the first half (the
            // shape right after execution) and the tail (the shape at the end).
            self.readbacks.remove(MAX_RECORDED_READBACKS / 2);
        }
        self.readbacks.push(value);
    }

    /// The last held-stack total seen, if any read-back produced one.
    pub fn last_readback(&self) -> Option<u32> {
        self.readbacks.iter().rev().copied().flatten().next()
    }

    /// `last_readback - expected_after`, the clients#443 surplus (positive) or
    /// deficit (negative). `None` when either side is unknown.
    pub fn readback_surplus(&self) -> Option<i64> {
        match (self.last_readback(), self.expected_after) {
            (Some(actual), Some(wanted)) => Some(i64::from(actual) - i64::from(wanted)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryError(pub String);

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DeliveryError {}

/// One grant's lifecycle over a [`Runtime`].
pub struct GrantSession<R: Runtime> {
    runtime: R,
    formula: DescriptorFormula,
    policy: Policy,
    prior: DurableState,
    state: DurableState,
    command: Option<GrantCommand>,
    absent_polls: u32,
    absent_tag: String,
    verify_polls: u32,
    expected_before: Option<u32>,
    active: bool,
    manual: bool,
    /// The in-flight native call is the existing-stack delta branch
    /// (`request = 2`), not an insert. It changes how the verify is read.
    delta_lane: bool,
    /// The in-flight call is an INSERT of a non-stackable instance record
    /// (clients#451). Count read-backs are not a witness for it; see
    /// [`GrantSession::poll_active`].
    instance_insert: bool,
    /// Passive forensics for the current grant (clients#445). Written on every
    /// transition, read by nothing inside this module.
    trace: GrantTrace,
}

impl<R: Runtime> GrantSession<R> {
    pub fn new(runtime: R, formula: DescriptorFormula, policy: Policy) -> Self {
        Self::with_prior(runtime, formula, policy, DurableState::default())
    }

    /// Resume with a durable state recovered from a previous process. That
    /// prior is what makes a restart decide "already applied" instead of
    /// granting twice.
    pub fn with_prior(
        runtime: R,
        formula: DescriptorFormula,
        policy: Policy,
        prior: DurableState,
    ) -> Self {
        Self {
            runtime,
            formula,
            policy,
            prior,
            state: DurableState::default(),
            command: None,
            absent_polls: 0,
            absent_tag: String::new(),
            verify_polls: 0,
            expected_before: None,
            active: false,
            manual: false,
            delta_lane: false,
            instance_insert: false,
            trace: GrantTrace::default(),
        }
    }

    pub fn state(&self) -> &DurableState {
        &self.state
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }

    /// The passive delivery trace for the grant currently held (clients#445).
    pub fn trace(&self) -> &GrantTrace {
        &self.trace
    }

    fn blood_vial_normalized(&self) -> u32 {
        self.formula.goods_normalized_prefix | BLOOD_VIAL_GOODS_ID
    }

    fn set(&mut self, status: &str, detail: String) -> String {
        let tag = self
            .command
            .as_ref()
            .map(|c| c.tag.clone())
            .unwrap_or_else(|| self.state.tag.clone());
        let expected_after = match (self.expected_before, self.command.as_ref()) {
            (Some(before), Some(command)) => Some(before.saturating_add(command.quantity)),
            _ => None,
        };
        self.state = DurableState {
            status: status.to_string(),
            tag,
            expected_before: self.expected_before,
            expected_after,
            detail,
        };
        status.to_string()
    }

    fn finish(&mut self, status: &str, detail: String) -> String {
        let result = self.set(status, detail);
        self.active = false;
        result
    }

    /// Queue one command. Refuses a second in-flight grant.
    pub fn submit(
        &mut self,
        command: GrantCommand,
        manual_trigger: bool,
    ) -> Result<(), DeliveryError> {
        command.validate()?;
        if self.active {
            return Err(DeliveryError("a grant is already in flight".into()));
        }
        if self.absent_tag != command.tag {
            self.absent_polls = 0;
        }
        self.absent_tag = command.tag.clone();
        self.verify_polls = 0;
        self.delta_lane = false;
        self.instance_insert = false;
        self.expected_before = command.expected_before;
        let detail = format!("tag={}", command.tag);
        self.trace = GrantTrace::begin(&command);
        self.command = Some(command);
        self.manual = manual_trigger;
        self.set("queued", detail);
        Ok(())
    }

    /// Advance the machine one poll and return the new status.
    pub fn poll(&mut self) -> String {
        if self.active {
            return self.poll_active();
        }
        if self.command.is_none() {
            return self.state.status.clone();
        }
        if self.state.is_terminal() {
            return self.state.status.clone();
        }
        self.poll_pending()
    }

    fn poll_active(&mut self) -> String {
        let command = self.command.clone().expect("active implies a command");
        if !self.runtime.native_done() {
            return self.set(
                "executing",
                format!("tag={} awaiting native completion", command.tag),
            );
        }
        let native_result = self.runtime.native_result();
        let stack = self.runtime.find_stack(command.normalized_id);
        let actual = stack.map(|s| s.quantity);
        let wanted = self
            .expected_before
            .expect("expected_before set before active")
            .saturating_add(command.quantity);
        let record = self.runtime.read_slot_record(native_result);
        // clients#445 forensics only: these are the values the verify loop is
        // about to branch on, captured before any of the branches run.
        self.trace.native_result = Some(native_result);
        self.trace.execution_evidence = native_result != EMPTY_SLOT;
        if !self.instance_insert {
            self.trace.record_readback(actual);
        }

        if self.instance_insert {
            return self.verify_instance_insert(&command, native_result, record);
        }
        // The insert lane can accept the reported slot as the witness: a
        // freshly inserted stack holding at least `quantity` of the right id
        // IS the grant. The delta lane cannot -- an existing stack of 5 that
        // is owed 2 more already satisfies `q >= quantity` before the delta
        // lands, so the shortcut would report `completed` on an unapplied
        // grant. There the read-back total is the only honest witness.
        let slot_verified = !self.delta_lane
            && record.normalized_id == Some(command.normalized_id)
            && record.quantity.is_some_and(|q| q >= command.quantity);
        if !slot_verified && actual != Some(wanted) {
            // clients#443: on the delta lane, EXECUTION EVIDENCE outranks the
            // equality, in EITHER direction. `native_done` is already true
            // here, and the result cell is no longer the `EMPTY_SLOT` sentinel
            // the guest writes before arming the cave -- and that cell is
            // written only by the routine's own return, so
            // `native_routines.quantity_delta` ran. `quantity_delta` is an
            // unconditional add: if it ran, the delta APPLIED and the item was
            // delivered. What the read-back TOTAL then says is a statement
            // about the player, not about the grant.
            //
            // Above `expected_after`: a concurrent acquisition -- the player
            // loots more of the same id between the dequeue-time observation
            // and the game-thread execution. Below it: a concurrent SPEND in
            // that same window, or a capped stack overflowing into storage
            // (Bloodborne consumables overflow; the pouch count can sit still
            // while the items land in storage). With execution evidence we
            // cannot separate a spend from an overflow, and do not need to --
            // both mean delivered. That race is real and unavoidable (PR #429's
            // body predicted exactly this residue); parking a delivered item
            // for it is not.
            //
            // Without execution evidence nothing changes: the equality and both
            // of its failure directions keep their full meaning there, because
            // an unexecuted delta must never read as delivered.
            if self.delta_lane
                && native_result != EMPTY_SLOT
                && let Some(n) = actual
            {
                let cause = if n > wanted {
                    "concurrent pickup"
                } else {
                    "concurrent spend or storage overflow"
                };
                return self.finish(
                    "completed",
                    format!(
                        "tag={} completed with {cause}: expected_after={wanted} actual={n} native_result={native_result}",
                        command.tag
                    ),
                );
            }
            self.verify_polls += 1;
            self.trace.verify_polls = self.verify_polls;
            let hydrating = !self.delta_lane
                && matches!(record.normalized_id, None | Some(EMPTY_SLOT))
                && !actual.is_some_and(|a| a != 0);
            let budget = if hydrating {
                self.policy.hydration_verify_polls
            } else {
                self.policy.verify_polls
            };
            if self.verify_polls < budget {
                return self.set(
                    "verify_pending",
                    format!(
                        "tag={} expected_after={wanted} actual={actual:?} attempt={}/{budget}",
                        command.tag, self.verify_polls
                    ),
                );
            }
            return self.finish(
                "failed",
                format!(
                    "tag={} expected_after={wanted} actual={actual:?} native_result={native_result} retry_budget={budget}",
                    command.tag
                ),
            );
        }
        self.finish(
            "completed",
            format!("tag={} native_result={native_result}", command.tag),
        )
    }

    /// Verify an INSERT of a non-stackable instance record (clients#451).
    ///
    /// The clients#444/#443 evidence rule is about a delta whose arithmetic the
    /// client can check. Here it cannot: `find_stack` returns the FIRST record
    /// matching the id, and its quantity position is not an instance count, so
    /// "the player now holds two Hunter Pistols" is not a number this client
    /// can read. **Instance-count read-back is not available.** Rather than
    /// park a delivered weapon on a count disagreement that means nothing, the
    /// count checks are skipped here, stated as such, and the witness is:
    ///
    /// * the cave's state cell -- `native_done` is true and `native_result` is
    ///   no longer the pre-arm `EMPTY_SLOT` sentinel, so the routine ran; and
    /// * the slot record the routine reports back: reading `native_result` and
    ///   finding OUR normalized id in it is a genuine read-back of the record
    ///   this insert created, and it is per-slot, so an already-owned duplicate
    ///   in a different slot cannot forge it.
    ///
    /// A reported slot that holds a DIFFERENT id is a contradiction and burns
    /// the normal verify budget. A slot that reads back as empty/unreadable is
    /// the hydration shape and gets the hydration budget; if it never resolves
    /// but the routine provably ran, the grant completes on execution evidence
    /// alone, with the reason recorded in the detail.
    fn verify_instance_insert(
        &mut self,
        command: &GrantCommand,
        native_result: u32,
        record: SlotRecord,
    ) -> String {
        let executed = native_result != EMPTY_SLOT;
        if executed && record.normalized_id == Some(command.normalized_id) {
            return self.finish(
                "completed",
                format!(
                    "tag={} instance insert verified at slot native_result={native_result} (count checks skipped: instance-count read-back is unavailable)",
                    command.tag
                ),
            );
        }
        let unreadable = matches!(record.normalized_id, None | Some(EMPTY_SLOT));
        self.verify_polls += 1;
        self.trace.verify_polls = self.verify_polls;
        let budget = if unreadable {
            self.policy.hydration_verify_polls
        } else {
            self.policy.verify_polls
        };
        if self.verify_polls < budget {
            return self.set(
                "verify_pending",
                format!(
                    "tag={tag} instance insert awaiting slot read-back native_result={native_result} record={record:?} attempt={attempt}/{budget}",
                    tag = command.tag,
                    record = record.normalized_id,
                    attempt = self.verify_polls,
                ),
            );
        }
        if executed && unreadable {
            return self.finish(
                "completed",
                format!(
                    "tag={} instance insert completed on execution evidence: slot {native_result} never read back (count checks skipped: instance-count read-back is unavailable)",
                    command.tag
                ),
            );
        }
        self.finish(
            "failed",
            format!(
                "tag={tag} instance insert unwitnessed: native_result={native_result} slot_record={record:?} retry_budget={budget}",
                tag = command.tag,
                record = record.normalized_id,
            ),
        )
    }

    fn poll_pending(&mut self) -> String {
        let command = self.command.clone().expect("command present in pending");
        if !self.runtime.inventory_ready() {
            return self.set(
                "awaiting_inventory",
                "Command retained; use one bullet once".into(),
            );
        }
        let Some(stack) = self.runtime.find_stack(command.normalized_id) else {
            return self.set(
                "awaiting_inventory",
                "Command retained; inventory geometry is not hydrated yet".into(),
            );
        };
        if !stack.exists {
            self.absent_polls += 1;
            if self.absent_polls < self.policy.min_absent_polls {
                return self.set(
                    "awaiting_inventory",
                    format!(
                        "tag={} waiting for inventory hydration before declaring the stack absent ({}/{})",
                        command.tag, self.absent_polls, self.policy.min_absent_polls
                    ),
                );
            }
        } else {
            self.absent_polls = 0;
        }

        // clients#451: the lane is chosen by the item's CATEGORY first and the
        // inventory's contents second. A stackable category (goods) with an
        // existing stack takes the cave's delta branch; equipment NEVER does,
        // however many matching records the scan finds. A weapon record's
        // "quantity" position is not a count, so a delta there adds into a
        // field that is not quantity -- the live Hunter Pistol duplicate that
        // was delivered as `delta persistent ... storage_suspected`, with the
        // owned weapon's record possibly corrupted. A duplicate weapon is a
        // second INSTANCE; the only correct lane for it is insert.
        let descriptor = ItemGrantDescriptor::new(command.raw_id, command.normalized_id);
        let stackable = descriptor.is_stackable_category(&self.formula);
        let delta = stack.exists && stackable;

        if stackable {
            // Goods, present or absent: the baseline is the live quantity (zero
            // when absent), or the durable prior for a replayed grant.
            self.expected_before = Some(match command.expected_before {
                Some(before) => before,
                None => self.recovered_baseline(&command, stack.quantity),
            });
        } else {
            // Instance insert. `observed_before` as a stack COUNT has no
            // meaning here: `find_stack` returns the first record matching the
            // id, and owning one Hunter Pistol says nothing about whether the
            // second one was delivered. The machine still needs a numeric
            // baseline internally (the durable row carries
            // `expected_before`/`expected_after`), so it uses 0 -- the number
            // of instances THIS grant is responsible for before it runs -- and
            // the trace deliberately leaves `observed_before`/`expected_after`
            // unset so that nothing downstream (the clients#447 destination
            // inference included) performs count arithmetic it cannot justify.
            //
            // Known, stated limitation: quantity-based replay recovery
            // (`recovered_complete`) is not available for an instance insert,
            // because a matching record is not evidence that THIS grant landed.
            // A replayed equipment grant is guarded by the client's ledger
            // acknowledgement, not by an inventory count.
            self.expected_before = Some(0);
        }
        let expected_before = self.expected_before.unwrap();
        let wanted = expected_before.saturating_add(command.quantity);
        if stackable {
            self.trace.observed_before = Some(expected_before);
            self.trace.expected_after = Some(wanted);

            if stack.quantity == wanted {
                self.trace.record_readback(Some(stack.quantity));
                return self.finish(
                    "recovered_complete",
                    format!("tag={} quantity={}", command.tag, stack.quantity),
                );
            }
            if stack.quantity != expected_before {
                self.trace.record_readback(Some(stack.quantity));
                return self.finish(
                    "quantity_mismatch",
                    format!(
                        "tag={} expected_before={expected_before} actual={}",
                        command.tag, stack.quantity
                    ),
                );
            }
        }

        // An existing stack takes the cave's existing-stack branch
        // (`request = 2`, `quantity` read as a DELTA) and needs both of its
        // arguments; without the record pointer the cave cannot address the
        // stack, and there is no fallback -- the external write that used to
        // be one is what clients#433 removed.
        if delta && stack.quantity_address.is_none() {
            return self.finish(
                "write_error",
                format!("tag={} quantity pointer missing", command.tag),
            );
        }

        if !stack.exists
            && command.normalized_id == self.blood_vial_normalized()
            && !self.policy.absent_blood_vial_allowed
        {
            return self.finish(
                "failed",
                format!(
                    "tag={} absent Blood Vial insertion is disabled after the live invalid-record reproduction; acquire one Vial before delivery",
                    command.tag
                ),
            );
        }
        if self.runtime.request_pending() {
            return self.set("busy", "Native request already pending".into());
        }

        // The slot and quantity-address arguments address an EXISTING record
        // for the delta branch. An insert must not carry them: passing the
        // matched weapon record here is precisely what would point the cave at
        // the owned instance.
        let (slot, quantity_address) = if delta {
            (stack.slot, stack.quantity_address)
        } else {
            (None, None)
        };
        self.runtime.queue_native(
            &descriptor,
            command.quantity,
            slot,
            quantity_address,
            self.manual,
        );
        self.active = true;
        self.verify_polls = 0;
        self.delta_lane = delta;
        self.instance_insert = !stackable;
        let source = if descriptor.uses_persistent_source(&self.formula) {
            "persistent"
        } else {
            "in_frame"
        };
        let lane = if delta { "delta" } else { "insert" };
        self.trace.lane = Some(lane);
        self.trace.source = Some(source);
        self.trace.verify_polls = 0;
        self.set(
            "executing",
            format!(
                "tag={} native lane={lane} source={source} expected_after={wanted}",
                command.tag
            ),
        )
    }

    /// A durable prior for the same tag whose recorded delta matches this
    /// command lets the baseline come from the prior, not the (already-applied)
    /// live quantity -- the whole of replay recovery.
    fn recovered_baseline(&self, command: &GrantCommand, live_quantity: u32) -> u32 {
        let prior = &self.prior;
        let recoverable = prior.tag == command.tag
            && prior.expected_before.is_some()
            && prior.expected_after
                == prior
                    .expected_before
                    .map(|b| b.saturating_add(command.quantity))
            && is_recoverable_prior(&prior.status);
        if recoverable {
            prior.expected_before.unwrap()
        } else {
            live_quantity
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::contract::contract;
    use super::*;
    use std::collections::HashMap;

    /// A scripted [`Runtime`] fake: an inventory keyed by normalized id, plus a
    /// native-call model that applies the queued grant on the next poll.
    #[derive(Default)]
    struct FakeRuntime {
        ready: bool,
        stacks: HashMap<u32, StackView>,
        // slot -> record
        slots: HashMap<u32, SlotRecord>,
        request: bool,
        done: bool,
        result: u32,
        // A queued native grant that the next `native_done()` will "apply".
        queued: Option<(ItemGrantDescriptor, u32, Option<u32>, Option<u64>)>,
        writes: Vec<(u64, u32)>,
        // When true, a queued native grant reports done but never actually
        // lands in the inventory, so the bounded verify starves and fails.
        complete_without_applying: bool,
        // clients#443: quantity of the same id the PLAYER acquires while the
        // native call is in flight -- the concurrent pickup that pushes the
        // read-back total above `expected_after`. Applied on the same
        // `native_done` that lands the delta, which is exactly the window the
        // client cannot observe.
        concurrent_pickup: u32,
        // clients#443: quantity of the same id the PLAYER consumes while the
        // native call is in flight -- the concurrent SPEND that pulls the
        // read-back total below `expected_after` even though the delta landed.
        // The inverse of `concurrent_pickup`, and the same unobservable window.
        // It equally models a capped stack overflowing into storage: the delta
        // ran, and the pouch count the client reads back does not show it.
        concurrent_spend: u32,
        // clients#443: report `done` with the result cell still holding the
        // `EMPTY_SLOT` sentinel -- completion signalled with NO execution
        // evidence. Only meaningful together with `complete_without_applying`.
        report_no_result: bool,
        // clients#451: report `done` with a result slot whose record is not
        // readable at all (all-`None`), the hydration shape for an insert.
        blank_insert_record: bool,
    }

    impl FakeRuntime {
        fn with_stack(mut self, normalized: u32, view: StackView) -> Self {
            self.ready = true;
            self.stacks.insert(normalized, view);
            self
        }
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
            self.slots.get(&slot).copied().unwrap_or_default()
        }
        fn write_quantity(&mut self, address: u64, value: u32) -> bool {
            self.writes.push((address, value));
            // Reflect the write into the stack backing that address.
            for stack in self.stacks.values_mut() {
                if stack.quantity_address == Some(address) {
                    stack.quantity = value;
                }
            }
            true
        }
        fn request_pending(&mut self) -> bool {
            self.request
        }
        fn queue_native(
            &mut self,
            descriptor: &ItemGrantDescriptor,
            quantity: u32,
            slot: Option<u32>,
            quantity_address: Option<u64>,
            _manual_trigger: bool,
        ) {
            self.request = true;
            self.done = false;
            self.queued = Some((*descriptor, quantity, slot, quantity_address));
        }
        fn native_done(&mut self) -> bool {
            if self.complete_without_applying {
                // Reports completion and never updates the real stack, so the
                // verify can never be satisfied. For an insert it reports a
                // bogus slot; for a delta it reports the REAL slot, whose
                // record still holds the pre-delta quantity -- the shape the
                // slot shortcut would wrongly accept.
                if let Some((descriptor, _quantity, slot, _address)) = self.queued.take() {
                    // The player's own pickup lands whether or not the cave
                    // ran: it is not part of the grant.
                    if self.concurrent_pickup > 0
                        && let Some(stack) = self.stacks.get_mut(&descriptor.normalized_id)
                    {
                        stack.quantity += self.concurrent_pickup;
                    }
                    self.result = if self.report_no_result {
                        EMPTY_SLOT
                    } else {
                        slot.unwrap_or(7)
                    };
                    if slot.is_none() && !self.blank_insert_record {
                        self.slots.insert(
                            7,
                            SlotRecord {
                                normalized_id: Some(0x0BAD_0BAD),
                                quantity: Some(0),
                                address: Some(0),
                            },
                        );
                    }
                    self.request = false;
                    self.done = true;
                }
                return self.done;
            }
            // Model the cave applying the grant exactly once. Both branches of
            // the contract's `request` cell: with a slot AND a record pointer
            // it is the existing-stack delta (`quantity` ADDED to the live
            // stack, slot unchanged); without them it is the insert.
            if let Some((descriptor, quantity, Some(slot), Some(address))) = self.queued {
                self.queued = None;
                let stack = self
                    .stacks
                    .get_mut(&descriptor.normalized_id)
                    .expect("the delta lane is only queued for a live stack");
                stack.quantity = (stack.quantity + quantity + self.concurrent_pickup)
                    .saturating_sub(self.concurrent_spend);
                let total = stack.quantity;
                self.result = slot;
                self.slots.insert(
                    slot,
                    SlotRecord {
                        normalized_id: Some(descriptor.normalized_id),
                        quantity: Some(total),
                        address: Some(address),
                    },
                );
                self.request = false;
                self.done = true;
                return self.done;
            }
            if let Some((descriptor, quantity, _slot, _address)) = self.queued.take() {
                let new_slot = 7u32;
                self.result = new_slot;
                // clients#443: a concurrent pickup rides along on the insert
                // too -- the inserted stack lands holding more than the grant.
                let quantity = quantity + self.concurrent_pickup;
                self.slots.insert(
                    new_slot,
                    SlotRecord {
                        normalized_id: Some(descriptor.normalized_id),
                        quantity: Some(quantity),
                        address: Some(0xDEAD_0000),
                    },
                );
                self.stacks.insert(
                    descriptor.normalized_id,
                    StackView {
                        quantity,
                        exists: true,
                        slot: Some(new_slot),
                        quantity_address: Some(0xDEAD_0000),
                    },
                );
                self.request = false;
                self.done = true;
            }
            self.done
        }
        fn native_result(&mut self) -> u32 {
            self.result
        }
        fn clear_request(&mut self) {
            self.request = false;
        }
    }

    fn session(runtime: FakeRuntime) -> GrantSession<FakeRuntime> {
        GrantSession::new(runtime, contract().descriptor, contract().policy)
    }

    fn goods_command(goods: u32, qty: u32, tag: &str, before: Option<u32>) -> GrantCommand {
        let d = ItemGrantDescriptor::for_goods(&contract().descriptor, goods).unwrap();
        GrantCommand {
            raw_id: d.raw_id,
            normalized_id: d.normalized_id,
            quantity: qty,
            tag: tag.into(),
            expected_before: before,
        }
    }

    /// clients#433, THE motivating case: an existing stack is bumped by the
    /// cave's existing-stack delta branch on the game thread. The delta lands,
    /// the read-back total verifies, and NOT ONE external write is issued --
    /// the write that shadPS4 intermittently refuses is what parked oz's
    /// grants as `write_error: quantity write failed`.
    #[test]
    fn an_existing_stack_is_bumped_through_the_native_delta_lane() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "recv_1", Some(5)), false)
            .unwrap();
        // Poll 1 queues the delta; the cave has not run yet.
        assert_eq!(session.poll(), "executing", "state: {:?}", session.state());
        assert!(session.state().detail.contains("lane=delta"));
        let queued = session
            .runtime_mut()
            .queued
            .expect("the existing-stack grant queues a native request");
        // Request word 2 semantics: the DELTA (2), plus the slot and the
        // record pointer the cave's existing-stack branch addresses.
        assert_eq!(
            queued.1, 2,
            "the queued quantity is the delta, not the total"
        );
        assert_eq!(queued.2, Some(3));
        assert_eq!(queued.3, Some(0x1000));
        // Poll 2 sees the cave's completion and verifies the read-back total.
        assert_eq!(session.poll(), "completed", "state: {:?}", session.state());
        assert_eq!(
            session
                .runtime_mut()
                .find_stack(normalized)
                .unwrap()
                .quantity,
            7
        );
        assert!(
            session.runtime_mut().writes.is_empty(),
            "no external write may reach a guest inventory page"
        );
    }

    /// clients#433, the witness that the deleted lane stays deleted: whatever
    /// the stack looks like, the machine reaches `write_quantity` never.
    #[test]
    fn no_grant_path_ever_calls_write_quantity() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        for (before, quantity) in [(5u32, 2u32), (0, 1), (98, 1)] {
            let runtime = FakeRuntime::default().with_stack(
                normalized,
                StackView {
                    quantity: before,
                    exists: true,
                    slot: Some(3),
                    quantity_address: Some(0x1000),
                },
            );
            let mut session = session(runtime);
            session
                .submit(
                    goods_command(0x384, quantity, "recv_w", Some(before)),
                    false,
                )
                .unwrap();
            assert_eq!(session.poll(), "executing");
            assert_eq!(session.poll(), "completed", "state: {:?}", session.state());
            assert!(session.runtime_mut().writes.is_empty());
        }
    }

    /// clients#433: the cave's existing-stack branch needs the record pointer.
    /// Without it there is nothing to fall back to -- the external write that
    /// used to be the fallback is exactly what was removed -- so this parks,
    /// and it parks under a detail the startup unpark must NOT requeue.
    #[test]
    fn an_existing_stack_without_a_record_pointer_parks_as_a_pointer_error() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: None,
            },
        );
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "recv_p", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "write_error");
        assert!(session.state().detail.contains("quantity pointer missing"));
        assert!(session.runtime_mut().writes.is_empty());
    }

    /// clients#433: the insert lane may accept the reported slot as its
    /// witness, the delta lane may not. A stack of 5 owed 2 more already
    /// satisfies "the slot holds at least `quantity`" BEFORE the delta lands,
    /// so the shortcut would report `completed` on an unapplied grant. The
    /// delta lane must starve its verify budget and fail instead.
    ///
    /// clients#443 moved this test onto the NO-EVIDENCE path, where its
    /// question is now the only place it is still live. The result cell holds
    /// the pre-arm `EMPTY_SLOT` sentinel, so nothing says the cave ran, and the
    /// unmoved stack must park exactly as before. WITH execution evidence this
    /// same shape completes -- the delta provably applied, and a read-back
    /// below `expected_after` is then the player's spend or a storage
    /// overflow, which is
    /// `a_delta_short_of_its_expected_total_completes_with_execution_evidence`.
    /// The slot shortcut is still refused on the delta lane in both: it is
    /// gated on `!delta_lane` and no evidence rule touches it.
    #[test]
    fn a_delta_that_never_lands_fails_instead_of_passing_the_slot_shortcut() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        // The reported slot keeps showing the pre-delta record: id matches and
        // 5 >= 2, which is precisely the shape the shortcut would accept.
        runtime.complete_without_applying = true;
        // ...and `done` is signalled with the sentinel still in the result
        // cell, so there is no execution evidence to outrank the equality.
        runtime.report_no_result = true;
        runtime.slots.insert(
            3,
            SlotRecord {
                normalized_id: Some(normalized),
                quantity: Some(5),
                address: Some(0x1000),
            },
        );
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "recv_s", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        let mut last = String::new();
        for _ in 0..contract().policy.verify_polls {
            last = session.poll();
        }
        assert_eq!(last, "failed", "state: {:?}", session.state());
        // The short budget, not the hydration one: a live stack is never
        // "not hydrated yet".
        assert!(session.state().detail.contains("retry_budget=20"));
    }

    /// clients#443, THE motivating case: the delta lands AND the player loots
    /// one more of the same id in the window between the dequeue-time
    /// observation and the game-thread execution. The read-back total comes in
    /// at `expected_after + 1` with the result cell carrying the routine's
    /// return -- oz's live park, verbatim:
    ///
    /// ```text
    /// failed (tag=ap_0 expected_after=7 actual=Some(8) native_result=8 retry_budget=20)
    /// ```
    ///
    /// Execution is confirmed, so the item was DELIVERED. A pickup can only
    /// add, and no number of retries will ever bring 8 back down to 7: the
    /// equality is broken by the player, not by the grant. This completes.
    #[test]
    fn a_concurrent_pickup_during_the_delta_completes_instead_of_parking() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        runtime.concurrent_pickup = 1;
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "ap_0", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        assert_eq!(
            session.poll(),
            "completed",
            "a delivered item must not park: {:?}",
            session.state()
        );
        // The surplus is recorded, not hidden: the operator can see why the
        // read-back and the expectation disagree.
        let detail = &session.state().detail;
        assert!(
            detail.contains("completed with concurrent pickup")
                && detail.contains("expected_after=7")
                && detail.contains("actual=8"),
            "detail must name the surplus, got: {detail}"
        );
        // `expected_after` stays the honest arithmetic of the grant -- the
        // pickup is the player's, and the durable rows a replay compares
        // against must not absorb it.
        assert_eq!(session.state().expected_before, Some(5));
        assert_eq!(session.state().expected_after, Some(7));
        assert!(session.state().is_success());
        assert!(session.runtime_mut().writes.is_empty());
        assert_eq!(
            session
                .runtime_mut()
                .find_stack(normalized)
                .unwrap()
                .quantity,
            8
        );
    }

    /// clients#443, owner review of PR #444: the surplus-only rule was half
    /// done. This test asserted a park and now asserts a completion -- a
    /// deliberate premise change, not a loosened assertion. The reviewer's
    /// point: `quantity_delta` is an unconditional ADD and the result cell is
    /// written only by that routine's own return, so execution evidence proves
    /// the delta APPLIED. A read-back that then sits BELOW `expected_after` is
    /// a statement about the player -- a concurrent spend in the
    /// observe-to-execute window, or a capped stack overflowing into storage --
    /// not about the grant, which was delivered either way. Here the fake
    /// reports done with the real slot as the result and the stack never
    /// moves: maximal deficit, full evidence, and it completes.
    #[test]
    fn a_delta_short_of_its_expected_total_completes_with_execution_evidence() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        runtime.complete_without_applying = true;
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "ap_1", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        assert_eq!(session.poll(), "completed", "state: {:?}", session.state());
        let detail = &session.state().detail;
        assert!(
            detail.contains("completed with concurrent spend or storage overflow")
                && detail.contains("expected_after=7")
                && detail.contains("actual=5"),
            "detail must name the deficit and its cause, got: {detail}"
        );
        assert!(
            !detail.contains("concurrent pickup"),
            "a deficit is not a pickup"
        );
        // Durable arithmetic is untouched in this direction too: the rows a
        // replay compares against stay the honest arithmetic of the grant.
        assert_eq!(session.state().expected_before, Some(5));
        assert_eq!(session.state().expected_after, Some(7));
        assert!(session.state().is_success());
        assert!(session.runtime_mut().writes.is_empty());
    }

    /// clients#443: the modelled inverse of the concurrent pickup. The delta
    /// DOES land on the game thread, and the player spends three of the same
    /// id in the same unobservable window, so the read-back total (5 + 2 - 3)
    /// sits below `expected_after=7`. The identical fake stands in for a
    /// capped stack overflowing into storage -- the delta ran, the pouch count
    /// the client reads back does not show it -- which is why the detail names
    /// both and claims neither. Execution evidence completes it, and the
    /// ledger seam is direction-blind: the acknowledgement records the AP
    /// item's own quantity, drops the baseline, and the next grant of the same
    /// id re-observes the live stack -- pinned once for both directions by
    /// `ledger::tests::a_completion_drops_the_baseline_so_the_next_grant_re_observes`.
    #[test]
    fn a_concurrent_spend_during_a_delta_completes_with_execution_evidence() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        runtime.concurrent_spend = 3;
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "ap_4", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        assert_eq!(
            session.poll(),
            "completed",
            "a delivered item must not park: {:?}",
            session.state()
        );
        let detail = &session.state().detail;
        assert!(
            detail.contains("completed with concurrent spend or storage overflow")
                && detail.contains("expected_after=7")
                && detail.contains("actual=4"),
            "detail must name the deficit, got: {detail}"
        );
        assert_eq!(session.state().expected_before, Some(5));
        assert_eq!(session.state().expected_after, Some(7));
        assert!(session.state().is_success());
        assert!(session.runtime_mut().writes.is_empty());
        // The stack really is short of the expectation, and the grant really
        // did land in it: 5 + 2 - 3.
        assert_eq!(
            session
                .runtime_mut()
                .find_stack(normalized)
                .unwrap()
                .quantity,
            4
        );
    }

    /// clients#443 control: a surplus WITHOUT execution evidence is unchanged
    /// today-behaviour. The result cell still holds the `EMPTY_SLOT` sentinel
    /// the guest writes before arming the cave, so nothing proves the delta
    /// ran -- and the stack reading above the expectation is then the player's
    /// pickup ALONE, with the grant still missing. Trusting the count here
    /// would acknowledge an item that was never delivered.
    #[test]
    fn a_surplus_without_execution_evidence_still_parks() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let mut runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        runtime.complete_without_applying = true;
        runtime.report_no_result = true;
        // The player picks up 3 while the (never-executing) call is in flight:
        // 5 + 3 = 8, above the wanted 7, and NOT because of the grant.
        runtime.concurrent_pickup = 3;
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "ap_2", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        let mut last = String::new();
        for _ in 0..contract().policy.verify_polls {
            last = session.poll();
        }
        assert_eq!(last, "failed", "state: {:?}", session.state());
        assert!(
            session
                .state()
                .detail
                .contains(&format!("native_result={EMPTY_SLOT}")),
            "detail: {}",
            session.state().detail
        );
        assert!(!session.state().detail.contains("concurrent pickup"));
    }

    /// clients#443 control: the insert lane is untouched. Its witness is the
    /// reported slot record (`id` + `quantity >= delta`), which is already
    /// surplus-tolerant, and the surplus rule is gated on `delta_lane`. An
    /// insert whose read-back exceeds the expectation completes exactly as it
    /// did before, through `slot_verified`, with no pickup detail.
    #[test]
    fn the_insert_lane_keeps_its_own_witness_and_takes_no_pickup_path() {
        let mut runtime = FakeRuntime {
            ready: true,
            ..Default::default()
        };
        // Pebble is absent, so this is the insert lane; the player picks two
        // more up while the insert is in flight.
        runtime.concurrent_pickup = 2;
        let mut session = session(runtime);
        session
            .submit(goods_command(0x4CE, 1, "ap_3", Some(0)), false)
            .unwrap();
        for _ in 0..(contract().policy.min_absent_polls - 1) {
            assert_eq!(session.poll(), "awaiting_inventory");
        }
        assert_eq!(session.poll(), "executing");
        assert!(session.state().detail.contains("lane=insert"));
        assert_eq!(session.poll(), "completed", "state: {:?}", session.state());
        assert!(
            !session.state().detail.contains("concurrent pickup"),
            "the insert lane verifies through its slot record, not the delta rule"
        );
    }

    /// clients#433 crash window: a restart mid-delta must not double-apply.
    /// The delta lane records `expected_before`/`expected_after` durably before
    /// the cave is armed, exactly as the insert lane does, so the next process
    /// reads the live stack against that prior -- `after` means applied
    /// (`recovered_complete`, nothing queued), `before` means it never landed
    /// and the delta is re-queued.
    #[test]
    fn a_restart_mid_delta_recovers_instead_of_double_applying() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let prior = DurableState {
            status: "executing".into(),
            tag: "recv_c".into(),
            expected_before: Some(5),
            expected_after: Some(7),
            detail: String::new(),
        };
        // (a) The delta HAD landed before the crash: the stack reads 7.
        let applied = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 7,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session = GrantSession::with_prior(
            applied,
            contract().descriptor,
            contract().policy,
            prior.clone(),
        );
        session
            .submit(goods_command(0x384, 2, "recv_c", None), false)
            .unwrap();
        assert_eq!(session.poll(), "recovered_complete");
        assert!(session.runtime_mut().queued.is_none());
        assert!(session.runtime_mut().writes.is_empty());

        // (b) It had NOT landed: the stack still reads 5, so the delta is
        // queued once and completes at 7 -- never 9.
        let unapplied = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session =
            GrantSession::with_prior(unapplied, contract().descriptor, contract().policy, prior);
        session
            .submit(goods_command(0x384, 2, "recv_c", None), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        assert_eq!(session.poll(), "completed");
        assert_eq!(
            session
                .runtime_mut()
                .find_stack(normalized)
                .unwrap()
                .quantity,
            7
        );
    }

    #[test]
    fn absent_stack_waits_out_the_hydration_grace_then_inserts() {
        // Pebble (0x4CE) is absent from the fake's empty inventory.
        let mut session = session(FakeRuntime {
            ready: true,
            ..Default::default()
        });
        session
            .submit(goods_command(0x4CE, 1, "recv_2", Some(0)), false)
            .unwrap();
        let min = contract().policy.min_absent_polls;
        // The first `min-1` polls refuse to declare the stack absent.
        for _ in 0..(min - 1) {
            assert_eq!(session.poll(), "awaiting_inventory");
        }
        // The grace elapses: the machine queues the native insert and then
        // completes on the following poll (the fake applies it).
        assert_eq!(session.poll(), "executing");
        assert_eq!(session.poll(), "completed", "state: {:?}", session.state());
    }

    #[test]
    fn absent_blood_vial_insertion_is_refused() {
        let mut session = session(FakeRuntime {
            ready: true,
            ..Default::default()
        });
        session
            .submit(
                goods_command(BLOOD_VIAL_GOODS_ID, 1, "recv_vial", Some(0)),
                false,
            )
            .unwrap();
        let min = contract().policy.min_absent_polls;
        for _ in 0..(min - 1) {
            assert_eq!(session.poll(), "awaiting_inventory");
        }
        let status = session.poll();
        assert_eq!(status, "failed");
        assert!(session.state().detail.contains("Blood Vial"));
    }

    #[test]
    fn already_applied_stack_is_recovered_not_regranted() {
        // expected_before given: the stack is already at before+quantity.
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 7,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "recv_1", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "recovered_complete");
        assert!(session.runtime_mut().writes.is_empty());
    }

    #[test]
    fn replay_recovery_uses_the_prior_baseline_across_a_restart() {
        // A previous process durably recorded before=5 after=7 for recv_1 and
        // then applied it (the live stack now reads 7). A fresh session with
        // NO expected_before (auto) must recognise this as already applied via
        // the prior, not sample 7 as the baseline and grant again.
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 7,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let prior = DurableState {
            status: "completed".into(),
            tag: "recv_1".into(),
            expected_before: Some(5),
            expected_after: Some(7),
            detail: String::new(),
        };
        let mut session =
            GrantSession::with_prior(runtime, contract().descriptor, contract().policy, prior);
        session
            .submit(goods_command(0x384, 2, "recv_1", None), false)
            .unwrap();
        assert_eq!(
            session.poll(),
            "recovered_complete",
            "state: {:?}",
            session.state()
        );
        assert!(session.runtime_mut().writes.is_empty());
    }

    #[test]
    fn auto_baseline_without_a_matching_prior_grants_from_the_live_quantity() {
        // No prior: auto baseline samples the live quantity (5) and the delta
        // lane bumps to 7.
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "recv_1", None), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        assert_eq!(session.poll(), "completed");
        assert_eq!(
            session
                .runtime_mut()
                .find_stack(normalized)
                .unwrap()
                .quantity,
            7
        );
        assert!(session.runtime_mut().writes.is_empty());
    }

    #[test]
    fn quantity_mismatch_when_the_stack_is_off_baseline() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 9,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "recv_1", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "quantity_mismatch");
    }

    #[test]
    fn native_insert_that_never_verifies_fails_after_the_budget() {
        let runtime = FakeRuntime {
            ready: true,
            complete_without_applying: true,
            ..Default::default()
        };
        let mut session = session(runtime);
        session
            .submit(goods_command(0x4CE, 1, "recv_x", Some(0)), false)
            .unwrap();
        let min = contract().policy.min_absent_polls;
        for _ in 0..(min - 1) {
            assert_eq!(session.poll(), "awaiting_inventory");
        }
        assert_eq!(session.poll(), "executing"); // queues the native insert
        // native reports done but the stack never reflects it, and the reported
        // slot holds a contradicting record -> the short verify budget applies.
        let budget = contract().policy.verify_polls;
        let mut last = String::new();
        for _ in 0..budget {
            last = session.poll();
        }
        assert_eq!(last, "failed", "state: {:?}", session.state());
        // The verify budget, not the long hydration one, was used.
        assert!(session.state().detail.contains("retry_budget=20"));
    }

    #[test]
    fn submit_refuses_a_second_in_flight_grant() {
        let mut session = session(FakeRuntime {
            ready: true,
            complete_without_applying: true,
            ..Default::default()
        });
        session
            .submit(goods_command(0x4CE, 1, "recv_a", Some(0)), false)
            .unwrap();
        // Drive up to the queue so a native call is in flight (active).
        let min = contract().policy.min_absent_polls;
        for _ in 0..min {
            session.poll();
        }
        // Now active; a second submit is refused.
        let err = session
            .submit(goods_command(0x4CE, 1, "recv_b", Some(0)), false)
            .unwrap_err();
        assert!(err.to_string().contains("already in flight"));
    }

    /// Saw Spear id, the validated category-0 pair: raw carries the persistent
    /// source marker, normalized has a zero high nibble.
    fn weapon_command(id: u32, tag: &str) -> GrantCommand {
        GrantCommand {
            raw_id: contract().descriptor.persistent_source_marker | id,
            normalized_id: id,
            quantity: 1,
            tag: tag.into(),
            expected_before: None,
        }
    }

    /// clients#451, THE motivating case: `ap_7` Hunter Pistol was delivered on
    /// the DELTA lane because the player already owned one and `find_stack`
    /// matched the weapon's record, so the cave added the "quantity" into a
    /// field that is not a quantity for an instance record. Equipment must take
    /// the insert lane regardless of a matching record, and must NOT hand the
    /// cave the owned record's slot or pointer. Fails before the guard: the
    /// lane reads `delta` and the queued call carries `Some(3)/Some(0x1000)`.
    #[test]
    fn an_owned_weapon_still_takes_the_insert_lane() {
        let normalized = 0x006C_5660; // Saw Spear
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 1,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session = session(runtime);
        session
            .submit(weapon_command(normalized, "recv_dup"), false)
            .unwrap();
        assert_eq!(session.poll(), "executing", "state: {:?}", session.state());
        assert!(
            session.state().detail.contains("lane=insert"),
            "detail: {}",
            session.state().detail
        );
        assert_eq!(session.trace().lane, Some("insert"));
        assert_eq!(session.trace().source, Some("persistent"));
        let queued = session
            .runtime_mut()
            .queued
            .expect("a native call was queued");
        assert_eq!(queued.2, None, "the owned record's slot must not be passed");
        assert_eq!(
            queued.3,
            None,
            "the owned record's pointer must not be passed"
        );
        // The insert completes on the slot the routine reports back.
        assert_eq!(session.poll(), "completed", "state: {:?}", session.state());
        assert!(session.runtime_mut().writes.is_empty());
    }

    /// The clients#444 evidence rule must not park an equipment insert on a
    /// count disagreement it cannot meaningfully evaluate. The owned Hunter
    /// Pistol means `find_stack` reports a total that has nothing to do with
    /// how many instances this grant delivered, so the trace carries no count
    /// baseline at all and the verify is the reported slot record.
    #[test]
    fn an_equipment_insert_does_not_park_on_a_meaningless_count() {
        let normalized = 0x006C_5660;
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 1,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session = session(runtime);
        session
            .submit(weapon_command(normalized, "recv_pistol"), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        assert_eq!(session.poll(), "completed", "state: {:?}", session.state());
        assert!(
            session.state().detail.contains("count checks skipped"),
            "detail: {}",
            session.state().detail
        );
        // No count arithmetic is published for an instance insert, so nothing
        // downstream can infer a destination from it.
        assert_eq!(session.trace().observed_before, None);
        assert_eq!(session.trace().expected_after, None);
        assert_eq!(session.trace().readback_surplus(), None);
        assert!(session.trace().execution_evidence);
    }

    /// An equipment insert whose reported slot reads back a DIFFERENT id is a
    /// contradiction, not a missing count, and still fails.
    #[test]
    fn an_equipment_insert_with_a_contradicting_slot_fails() {
        let normalized = 0x006C_5660;
        let runtime = FakeRuntime {
            ready: true,
            complete_without_applying: true,
            ..Default::default()
        };
        let mut session = session(runtime);
        session
            .submit(weapon_command(normalized, "recv_bad"), false)
            .unwrap();
        let mut last = session.poll();
        while last == "awaiting_inventory" {
            last = session.poll();
        }
        assert_eq!(last, "executing");
        while last == "executing" || last == "verify_pending" {
            last = session.poll();
        }
        assert_eq!(last, "failed", "state: {:?}", session.state());
        assert!(session.state().detail.contains("instance insert unwitnessed"));
    }

    /// ...but a slot that simply never reads back, with the routine provably
    /// run, completes on execution evidence rather than parking a delivered
    /// weapon on a count check that does not exist for instance records.
    #[test]
    fn an_unreadable_slot_completes_on_execution_evidence() {
        let normalized = 0x006C_5660;
        let runtime = FakeRuntime {
            ready: true,
            complete_without_applying: true,
            blank_insert_record: true,
            ..Default::default()
        };
        let mut session = session(runtime);
        session
            .submit(weapon_command(normalized, "recv_blind"), false)
            .unwrap();
        let mut last = session.poll();
        while last == "awaiting_inventory" {
            last = session.poll();
        }
        assert_eq!(last, "executing");
        while last == "executing" || last == "verify_pending" {
            last = session.poll();
        }
        assert_eq!(last, "completed", "state: {:?}", session.state());
        assert!(
            session
                .state()
                .detail
                .contains("completed on execution evidence"),
            "detail: {}",
            session.state().detail
        );
    }

    /// Control: goods with an existing stack are untouched by the category
    /// guard and still take the delta lane.
    #[test]
    fn goods_with_an_existing_stack_still_delta() {
        let normalized = contract().descriptor.goods_normalized_prefix | 0x384;
        let runtime = FakeRuntime::default().with_stack(
            normalized,
            StackView {
                quantity: 5,
                exists: true,
                slot: Some(3),
                quantity_address: Some(0x1000),
            },
        );
        let mut session = session(runtime);
        session
            .submit(goods_command(0x384, 2, "recv_goods", Some(5)), false)
            .unwrap();
        assert_eq!(session.poll(), "executing");
        assert_eq!(session.trace().lane, Some("delta"));
        let queued = session.runtime_mut().queued.expect("queued");
        assert_eq!(queued.2, Some(3));
        assert_eq!(queued.3, Some(0x1000));
        assert_eq!(session.poll(), "completed", "state: {:?}", session.state());
        assert_eq!(session.trace().observed_before, Some(5));
        assert_eq!(session.trace().expected_after, Some(7));
    }

    #[test]
    fn invalid_command_is_rejected_at_submit() {
        let mut session = session(FakeRuntime::default());
        let bad = GrantCommand {
            raw_id: 0,
            normalized_id: 0,
            quantity: 0,
            tag: "x".into(),
            expected_before: None,
        };
        assert!(session.submit(bad, false).is_err());
    }
}
