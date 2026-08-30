//! `NativeBackend`: a [`BloodborneBackend`] that grants items in-process via the
//! native `bb-native-grant-v7` payload, replacing the Cheat Engine file bridge.
//!
//! Native is now the default delivery backend (see `main.rs`). Regardless of how
//! it is selected, [`NativeBackend::attach`] itself always fails closed: when the
//! running image does not verify against the contract it returns a clear error
//! and patches nothing -- this layer never silently falls back. `main.rs` never
//! turns that failure into a CE-bridge fallback: on the *default* path (no
//! explicit `--delivery`) a failed attach hard-fails with guidance telling the
//! player to load the Cheat Engine table and re-run with `--delivery=ce-bridge`,
//! and an explicit `--delivery=native` propagates the raw error. A silent
//! fallback would arm a bridge the player has no CE table loaded for, so grants
//! would vanish; clients#413 tracks the liveness handshake that will let the
//! client detect a loaded table and offer the bridge safely.
//!
//! The live attach/install path is `#[cfg(windows)]` and CI/owner-validated; the
//! grant, flag and context logic it drives is host-tested through `engine.rs`,
//! `delivery.rs` and the `event_flags` resilient reads.
//!
//! Replay recovery is coordinated with the receive ledger, not a parallel store:
//! `grant_item` feeds the ledger's durable `expected_before` straight into the
//! delivery machine, so a restart mid-grant recognises an already-applied stack
//! (`recovered_complete`) instead of granting twice. Since clients#427 that
//! baseline is the quantity `observe_stack_quantity` read at submit time and the
//! ledger recorded, not the lifetime delivered sum -- a sum that is simply not
//! the current inventory of anything the player can spend.

use anyhow::{Result, bail};

use crate::backend::{
    BloodborneBackend, EquipRequest, GrantTerminalFailure, ItemGrant, LocationContext,
    OperationProgress, StackObservation,
};
use crate::client_eprintln;
use crate::event_flags::LiveEventFlags;

use super::diagnostics::{DiagnosticSink, GrantContext, JsonlFile, diagnostics_path_for_ledger};
use super::engine::{GrantStep, NativeDelivery, NativeGrantRequest};
use super::flag_gate::FlagGate;
use super::gem_capture::GemCapture;
use super::guest::GuestRuntime;
use super::mem::NativeMemory;

/// Consecutive gameplay-ready probes required before the unsafe assumed-save
/// mode reports readiness. Mirrors `FileBackend`.
const ASSUMED_CONTEXT_STABLE_READS: u8 = 3;

#[derive(Clone, Debug)]
struct AssumedContextGate {
    identity: String,
    consecutive_ready: u8,
}

impl AssumedContextGate {
    fn new(identity: String) -> Self {
        Self {
            identity,
            consecutive_ready: 0,
        }
    }

    fn observe(&mut self, ready: bool) -> LocationContext {
        if ready {
            self.consecutive_ready = self.consecutive_ready.saturating_add(1);
        } else {
            self.consecutive_ready = 0;
        }
        LocationContext {
            save_identity: self.identity.clone(),
            gameplay_ready: self.consecutive_ready >= ASSUMED_CONTEXT_STABLE_READS,
        }
    }
}

pub struct NativeBackend {
    delivery: NativeDelivery<GuestRuntime<NativeMemory>>,
    /// clients#420: the flag half arms lazily. The event-flag manager is a
    /// guest global that is null until the game has loaded a character, which
    /// attach routinely beats; delivery does not depend on it, so attach
    /// succeeds pending and the loop arms this gate when the manager appears.
    event_flags: FlagGate<LiveEventFlags>,
    /// Kept so a pending gate can retry the attach at the base this process
    /// already confirmed, without re-reading the appended log (clients#418).
    shad_log: std::path::PathBuf,
    assumed_context: Option<AssumedContextGate>,
    base: u64,
    /// clients#427: consecutive polls a stack has read as absent, per
    /// normalized id. An absent reading right after a load can be the
    /// inventory not having hydrated yet, so the same `min_absent_polls`
    /// grace the delivery machine applies before declaring a stack absent
    /// gates the observed baseline too -- otherwise a hydration lie would be
    /// recorded durably as a baseline of zero.
    absent_observations: std::collections::HashMap<u32, u32>,
    /// clients#445: the last context the loop's `location_context` call
    /// produced, stamped onto each diagnostic record. Not a guest read -- it is
    /// the value this backend already returned to the loop this iteration.
    last_context: GrantContext,
    gem_capture: Option<GemCapture>,
}

impl NativeBackend {
    /// The verified eboot base the payload was installed at.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// True once the live event-flag accessor is armed. While false, item
    /// delivery runs and location checks abstain (clients#420).
    pub fn event_flags_armed(&self) -> bool {
        self.event_flags.is_armed()
    }

    /// Arm the passive per-grant delivery diagnostics (clients#445) beside the
    /// receive ledger. One JSON line per terminal grant, appended; a write
    /// failure warns once and never touches a delivery.
    ///
    /// The path is derived from the ledger rather than taken as a new CLI
    /// argument: the launcher already puts `ledger.json`, `client.log` and this
    /// file in one per-session folder, and the ledger path names that folder.
    pub fn arm_delivery_diagnostics(&mut self, ledger: &std::path::Path) {
        let path = diagnostics_path_for_ledger(ledger);
        client_eprintln!(
            "Delivery diagnostics: one line per delivered item into {} - send it back with client.log if a delivery looks wrong (clients#445).",
            path.display()
        );
        self.delivery
            .arm_diagnostics(DiagnosticSink::new(Box::new(JsonlFile::new(path))));
        match GemCapture::beside_ledger(ledger) {
            Ok(capture) => {
                client_eprintln!(
                    "Blood-gem diagnostics: natural inventory changes stream beside the ledger to blood-gem-capture.jsonl."
                );
                self.gem_capture = Some(capture);
            }
            Err(error) => client_eprintln!("Blood-gem diagnostics unavailable: {error}"),
        }
    }

    /// The live `location_context`, before the diagnostics stamp is taken.
    fn location_context_inner(&mut self) -> Result<Option<LocationContext>> {
        // clients#420: every loop gives the pending flag gate one cheap chance
        // to arm. Still-null is not an error here -- it reports not-ready.
        let arming = self.arm_event_flags();
        // Disjoint borrows: the readiness gate and the flag accessor are two
        // different fields of `self`.
        let Self {
            event_flags,
            assumed_context,
            ..
        } = self;
        let Some(gate) = assumed_context.as_mut() else {
            // Normal live mode stays fail-closed until a real save-identity
            // accessor exists, exactly like FileBackend.
            return Ok(None);
        };
        if let Err(error) = arming {
            gate.observe(false);
            return Err(error);
        }
        let Some(flags) = event_flags.armed_mut() else {
            // Waiting for the game to finish loading: not gameplay-ready, which
            // is exactly what the existing send-gate consumes
            // (`require_runtime_context` -> Ok(None) -> no checks, no sends).
            return Ok(Some(gate.observe(false)));
        };
        if let Err(error) = flags.probe_manager_resilient() {
            gate.observe(false);
            return Err(error);
        }
        Ok(Some(gate.observe(true)))
    }

    /// Retry the pending event-flag attach at the already-confirmed base.
    /// A no-op once armed. Emits the one armed notice on the transition.
    fn arm_event_flags(&mut self) -> Result<()> {
        if self.event_flags.is_armed() {
            return Ok(());
        }
        let shad_log = self.shad_log.clone();
        let base = self.base;
        self.event_flags.poll(
            || LiveEventFlags::attach_at_base(&shad_log, base),
            &mut |line: &str| client_eprintln!("{line}"),
        )
    }
}

#[cfg(windows)]
impl NativeBackend {
    /// Attach to shadPS4, verify the image against the contract, install the
    /// native payload atomically, and arm delivery. Fails closed on any image
    /// mismatch or an uncleared detour window; nothing is written on failure.
    ///
    /// `assumed_identity` arms the explicitly unsafe assumed-correct-save mode.
    pub fn attach(shad_log: &std::path::Path, assumed_identity: Option<String>) -> Result<Self> {
        Self::attach_with_policy(
            shad_log,
            assumed_identity,
            super::attach_wait::WaitPolicy::default(),
        )
    }

    /// [`NativeBackend::attach`] with an explicit fresh-base wait policy.
    pub fn attach_with_policy(
        shad_log: &std::path::Path,
        assumed_identity: Option<String>,
        policy: super::attach_wait::WaitPolicy,
    ) -> Result<Self> {
        use super::attach_wait::{BaseCheck, SystemAttachClock, wait_for_verified_base};
        use super::contract::contract;
        use super::install::{self, InstallConfig};
        use super::mem::{require_validated_image, verify_base};
        use super::threads::WindowsThreadController;

        let contract = contract();
        let memory = NativeMemory::open_shad()?;
        let process_id = memory.process_id();

        let log = std::fs::read_to_string(shad_log).map_err(|error| {
            anyhow::Error::new(error).context(format!("reading shad_log {}", shad_log.display()))
        })?;

        // clients#418: the shad log is appended across runs, so the base already
        // in it may belong to the PREVIOUS run. Verify it live, and if it cannot
        // be confirmed, wait -- bounded -- for a base line written after this
        // point in the file. Fails closed either way: nothing is written until a
        // base is both confirmed and validated.
        let mut clock = SystemAttachClock::default();
        let base = wait_for_verified_base(
            &shad_log.display().to_string(),
            &log,
            || std::fs::read_to_string(shad_log).ok(),
            |candidate| match verify_base(&memory, candidate, contract) {
                // Not the running image's base yet: a stale line, or a page the
                // loader has not mapped. Keep waiting.
                Ok(false) | Err(_) => BaseCheck::Unverified,
                // Confirmed base: CUSA00900 and every other build are refused
                // here, and waiting longer cannot change a build.
                Ok(true) => match require_validated_image(&memory, candidate, contract) {
                    Ok(()) => BaseCheck::Attached,
                    Err(error) => BaseCheck::ImageRejected(format!("{error:#}")),
                },
            },
            |line| client_eprintln!("{line}"),
            &mut clock,
            policy,
        )
        .map_err(anyhow::Error::new)?;

        let mut threads = WindowsThreadController::new(process_id);
        install::install(
            &memory,
            base,
            contract,
            &mut threads,
            InstallConfig::default(),
            std::thread::sleep,
        )?;

        // clients#418: hand over the base this attach already confirmed rather
        // than letting the event-flag attach re-read the log and re-run the race.
        //
        // clients#420: the event-flag manager is a *later* boot step than the
        // image being mapped and validated -- it stays null until the game has
        // loaded a character. That is not a reason to refuse the attach: item
        // delivery does not read it, and location checks cannot fire before
        // gameplay anyway. So a not-initialized manager leaves the flag gate
        // pending (one notice) and the client loop arms it; anything else
        // (signature mismatch, process gone) is still terminal here.
        let event_flags = match LiveEventFlags::attach_at_base(shad_log, base) {
            Ok(flags) => FlagGate::armed(flags),
            Err(error) if crate::event_flags::is_manager_not_initialized(&error) => {
                FlagGate::pending(&mut |line: &str| client_eprintln!("{line}"))
            }
            Err(error) => return Err(error),
        };
        let guest = GuestRuntime::new(memory, base)?;
        let delivery = NativeDelivery::new(guest, contract.descriptor, contract.policy);
        Ok(Self {
            delivery,
            event_flags,
            shad_log: shad_log.to_owned(),
            assumed_context: assumed_identity.map(AssumedContextGate::new),
            base,
            absent_observations: std::collections::HashMap::new(),
            last_context: GrantContext::default(),
            gem_capture: None,
        })
    }
}

impl BloodborneBackend for NativeBackend {
    fn location_context(&mut self) -> Result<Option<LocationContext>> {
        if let Some(capture) = &mut self.gem_capture {
            let generated = capture.observe(self.delivery.runtime_mut().inventory_entries());
            let candidate = generated.into_iter().next();
            if let Some(probe) = self
                .delivery
                .runtime_mut()
                .probe_generated_object(candidate)
            {
                capture.record_generated_object(&probe);
            }
        }
        let result = self.location_context_inner();
        // clients#445: remember what the loop was told, so a grant record can
        // say what the client's own readiness looked like around it. `None` is
        // "this backend reported no context", never "not ready".
        self.last_context = GrantContext {
            gameplay_ready: match &result {
                Ok(Some(context)) => Some(context.gameplay_ready),
                _ => None,
            },
            event_flags_armed: self.event_flags.is_armed(),
        };
        result
    }

    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        // `None` means "the live accessor is not available", never "false", so
        // a check can never be missed by reading through a pending gate.
        self.arm_event_flags()?;
        match self.event_flags.armed_mut() {
            Some(flags) => flags.read_resilient(event_flag).map(Some),
            None => Ok(None),
        }
    }

    fn target_weapon_level(&mut self) -> Result<Option<u8>> {
        Ok(self.delivery.target_weapon_level())
    }

    fn observe_stack_quantity(
        &mut self,
        normalized_item_id: u32,
        _reinforcement_level: Option<u8>,
    ) -> Result<StackObservation> {
        let min_absent_polls = super::contract::contract().policy.min_absent_polls;
        let Some(stack) = self.delivery.observe_stack(normalized_item_id) else {
            return Ok(StackObservation::NotReady);
        };
        if stack.exists {
            self.absent_observations.remove(&normalized_item_id);
            return Ok(StackObservation::Quantity(stack.quantity));
        }
        let seen = self
            .absent_observations
            .entry(normalized_item_id)
            .or_insert(0);
        *seen = seen.saturating_add(1);
        if *seen < min_absent_polls {
            return Ok(StackObservation::NotReady);
        }
        Ok(StackObservation::Quantity(stack.quantity))
    }

    fn grant_may_have_applied(&mut self, tag: &str) -> Result<bool> {
        Ok(self.delivery.command_may_have_applied(tag))
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
        let request = NativeGrantRequest {
            tag: grant.tag.clone(),
            raw_descriptor: grant.raw_descriptor,
            normalized_item_id: grant.normalized_item_id,
            item_category: grant.item_category,
            quantity: grant.quantity,
            // The ledger's durable baseline -- the quantity observed when this
            // command was first submitted. This is what makes a restart
            // mid-grant recover instead of double-granting.
            expected_before: Some(grant.expected_before),
        };
        self.delivery.set_context(self.last_context);
        let step = self
            .delivery
            .grant_with_warning(request, &mut |line: &str| client_eprintln!("{line}"))?;
        match step {
            GrantStep::Pending => Ok(OperationProgress::Pending),
            GrantStep::Complete => Ok(OperationProgress::Complete),
            GrantStep::Failed { status, detail } => Err(GrantTerminalFailure {
                tag: grant.tag.clone(),
                status,
                detail,
            }
            .into()),
        }
    }

    fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress> {
        bail!(
            "native auto-equip is not armed for {:?}; item {} remains durably pending",
            request.target,
            request.tag
        )
    }

    fn withdraw_unwitnessed_grant(&mut self, _tag: &str) -> Result<bool> {
        // The native request cell lives in guest memory; a leftover arm from a
        // previous process is cleared best-effort. The durable plan stays in the
        // ledger and re-publishes under a validated context.
        Ok(self.delivery.withdraw_stale())
    }
}
