//! `NativeBackend`: a [`BloodborneBackend`] that grants items in-process via the
//! native `bb-native-grant-v5` payload, replacing the Cheat Engine file bridge.
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
//! `grant_item` feeds the ledger-derived `expected_before` straight into the
//! delivery machine, so a restart mid-grant recognises an already-applied stack
//! (`recovered_complete`) instead of granting twice -- the same durable baseline
//! `client_loop.rs` already computes from `SlotLedger::delivered_quantity`.

use anyhow::{Result, bail};

use crate::backend::{
    BloodborneBackend, EquipRequest, GrantTerminalFailure, ItemGrant, LocationContext,
    OperationProgress,
};
use crate::event_flags::LiveEventFlags;

use super::engine::{GrantStep, NativeDelivery, NativeGrantRequest};
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
    event_flags: LiveEventFlags,
    assumed_context: Option<AssumedContextGate>,
    base: u64,
}

impl NativeBackend {
    /// The verified eboot base the payload was installed at.
    pub fn base(&self) -> u64 {
        self.base
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
        use super::contract::contract;
        use super::install::{self, InstallConfig};
        use super::mem::{logged_eboot_base, require_validated_image, verify_base};
        use super::threads::WindowsThreadController;

        let contract = contract();
        let memory = NativeMemory::open_shad()?;
        let process_id = memory.process_id();

        let log = std::fs::read_to_string(shad_log).map_err(|error| {
            anyhow::Error::new(error).context(format!("reading shad_log {}", shad_log.display()))
        })?;
        let base = logged_eboot_base(&log)
            .ok_or_else(|| anyhow::anyhow!("shad_log has no eboot base_virtual_addr"))?;
        anyhow::ensure!(
            verify_base(&memory, base, contract)?,
            "the logged eboot base 0x{base:X} failed hook-original verification; refusing to patch a base we cannot confirm"
        );

        // Fail closed on any image mismatch -- CUSA00900 and every other build
        // land here and are refused.
        require_validated_image(&memory, base, contract)?;

        let mut threads = WindowsThreadController::new(process_id);
        install::install(
            &memory,
            base,
            contract,
            &mut threads,
            InstallConfig::default(),
            std::thread::sleep,
        )?;

        let event_flags = LiveEventFlags::attach(shad_log)?;
        let guest = GuestRuntime::new(memory, base)?;
        let delivery = NativeDelivery::new(guest, contract.descriptor, contract.policy);
        Ok(Self {
            delivery,
            event_flags,
            assumed_context: assumed_identity.map(AssumedContextGate::new),
            base,
        })
    }
}

impl BloodborneBackend for NativeBackend {
    fn location_context(&mut self) -> Result<Option<LocationContext>> {
        let Some(gate) = self.assumed_context.as_mut() else {
            // Normal live mode stays fail-closed until a real save-identity
            // accessor exists, exactly like FileBackend.
            return Ok(None);
        };
        if let Err(error) = self.event_flags.probe_manager_resilient() {
            gate.observe(false);
            return Err(error);
        }
        Ok(Some(gate.observe(true)))
    }

    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        self.event_flags.read_resilient(event_flag).map(Some)
    }

    fn target_weapon_level(&mut self) -> Result<Option<u8>> {
        // Weapon inventory/reinforcement state is not resolved on v0.18.
        Ok(None)
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
        let request = NativeGrantRequest {
            tag: grant.tag.clone(),
            raw_descriptor: grant.raw_descriptor,
            normalized_item_id: grant.normalized_item_id,
            item_category: grant.item_category,
            quantity: grant.quantity,
            // The ledger-derived durable baseline: this is what makes a restart
            // mid-grant recover instead of double-granting.
            expected_before: Some(grant.expected_before),
        };
        match self.delivery.grant(request)? {
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
