//! `NativeBackend`: a [`BloodborneBackend`] that grants items in-process via the
//! native `bb-native-grant-v7` payload.
//!
//! Native is now the default delivery backend (see `main.rs`). Regardless of how
//! it is selected, [`NativeBackend::attach`] itself always fails closed: when the
//! running image does not verify against the contract it returns a clear error
//! and patches nothing. `main.rs` reports actionable native diagnostics and
//! never arms another backend.
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

use anyhow::{Context, Result, bail};

use crate::backend::{
    BloodborneBackend, EquipRequest, GrantTerminalFailure, ItemGrant, LocationContext,
    OperationProgress, StackObservation,
};
use crate::client_eprintln;
use crate::event_flags::LiveEventFlags;

use super::diagnostics::{DiagnosticSink, GrantContext, JsonlFile, diagnostics_path_for_ledger};
use super::engine::{GrantStep, NativeDelivery, NativeGrantRequest};
use super::flag_gate::FlagGate;
use super::gem_alloc_probe::{self, AllocCapture};
use super::gem_capture::GemCapture;
use super::guest::GuestRuntime;
use super::mem::{NativeMemory, ProcessMemory};
use super::pickup_notification_capture::PickupNotificationCapture;
use super::probe_pack::{BOSS_FLAGS, BossFlagCensus, RuneCapture};
use super::save_identity::SaveIdentityTracker;
use super::shop_capture::ShopCapture;
use super::vial_capture::VialCapture;

/// Consecutive gameplay-ready probes required before the unsafe assumed-save
/// mode reports readiness.
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
    save_identity: SaveIdentityTracker,
    live_identity_candidate: Option<String>,
    live_identity_ready_reads: u8,
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
    item_grant_probe_state: Option<u64>,
    gem_capture: Option<GemCapture>,
    shop_capture: Option<ShopCapture>,
    vial_capture: Option<VialCapture>,
    pickup_notification_capture: Option<PickupNotificationCapture>,
    boss_flag_census: Option<BossFlagCensus>,
    rune_capture: Option<RuneCapture>,
    gem_alloc_probe_state: Option<u64>,
    gem_alloc_capture: Option<AllocCapture>,
    category8_scratch: Option<u64>,
    category8_gen_ctx_rsi: Option<u64>,
    category8_last_generated: Option<(u32, u32)>,
    category8_inserted: std::collections::HashSet<u32>,
    process_id: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProbeOptions {
    pub pickup_notification: bool,
    pub boss_flags: bool,
    pub runes: bool,
    pub insight: bool,
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
    pub fn arm_delivery_diagnostics(&mut self, ledger: &std::path::Path, probes: ProbeOptions) {
        let path = diagnostics_path_for_ledger(ledger);
        client_eprintln!(
            "Delivery diagnostics: one line per delivered item into {} - send it back with client.log if a delivery looks wrong (clients#445).",
            path.display()
        );
        self.delivery
            .arm_diagnostics(DiagnosticSink::new(Box::new(JsonlFile::new(path))));
        if self.item_grant_probe_state.is_some() {
            match GemCapture::beside_ledger(ledger, self.base) {
                Ok(capture) => {
                    client_eprintln!(
                        "Blood-gem diagnostics: read-only ItemGrant calls and completed category-8 inventory records stream beside the ledger to blood-gem-capture.jsonl."
                    );
                    self.gem_capture = Some(capture);
                }
                Err(error) => client_eprintln!("Blood-gem diagnostics unavailable: {error}"),
            }
        } else {
            client_eprintln!(
                "Blood-gem diagnostics inactive because the ItemGrant probe did not arm."
            );
        }
        match VialCapture::beside_ledger(ledger) {
            Ok(capture) => {
                client_eprintln!(
                    "Zero-Vial diagnostics: read-only samples stream beside the ledger to blood-vial-capture.jsonl (bb-archipelago#70)."
                );
                self.vial_capture = Some(capture);
            }
            Err(error) => client_eprintln!("Zero-Vial diagnostics unavailable: {error}"),
        }
        match ShopCapture::beside_ledger(ledger) {
            Ok(capture) => {
                client_eprintln!(
                    "Shop diagnostics: read-only badge and purchase transitions stream beside the ledger to shop-capture.jsonl."
                );
                self.shop_capture = Some(capture);
            }
            Err(error) => client_eprintln!("Shop diagnostics unavailable: {error}"),
        }
        if probes.pickup_notification {
            match self.install_pickup_presentation_probe() {
                Ok(()) => client_eprintln!(
                    "Pickup-dialog lifecycle probe armed for GetItem, ObjGetItemData, ItemGet, and ItemGetPlate."
                ),
                Err(error) => client_eprintln!(
                    "Pickup-presentation probe inactive (gameplay and delivery remain available): {error:#}"
                ),
            }
            match PickupNotificationCapture::beside_ledger(ledger, self.base) {
                Ok(capture) => {
                    client_eprintln!(
                        "Pickup-notification probe ACTIVE: observation-only correlation records stream beside the ledger to pickup-notification-capture.jsonl (clients#510)."
                    );
                    self.pickup_notification_capture = Some(capture);
                }
                Err(error) => client_eprintln!("Pickup-notification probe unavailable: {error}"),
            }
        }
        if probes.boss_flags {
            match BossFlagCensus::beside_ledger(ledger) {
                Ok(capture) => self.boss_flag_census = Some(capture),
                Err(error) => client_eprintln!("Boss-flag census unavailable: {error}"),
            }
        }
        if probes.runes {
            match RuneCapture::beside_ledger(ledger) {
                Ok(capture) => self.rune_capture = Some(capture),
                Err(error) => client_eprintln!("Rune capture unavailable: {error}"),
            }
            match self.install_gem_alloc_probe(ledger) {
                Ok(()) => client_eprintln!("gem-alloc-probe armed (entry capture / option B)."),
                Err(error) => {
                    client_eprintln!("gem-alloc-probe inactive; delivery unaffected: {error:#}")
                }
            }
        }
        if probes.insight {
            client_eprintln!(
                "Insight probe requested but not armed: the reviewed player-stat candidate manifest is still empty; no addresses were guessed."
            );
        }
    }

    #[cfg(windows)]
    fn install_gem_alloc_probe(&mut self, ledger: &std::path::Path) -> Result<()> {
        use super::threads::WindowsThreadController;
        let state = self
            .delivery
            .runtime_mut()
            .memory()
            .allocate(gem_alloc_probe::STATE_SIZE)?;
        let scratch = self.delivery.runtime_mut().memory().allocate(0x300)?;
        let mut threads = WindowsThreadController::new(self.process_id);
        let prologue = gem_alloc_probe::install(
            self.delivery.runtime_mut().memory(),
            self.base,
            state,
            &mut threads,
        )?;
        self.gem_alloc_capture = Some(AllocCapture::beside_ledger(ledger, self.base, &prologue)?);
        self.gem_alloc_probe_state = Some(state);
        self.category8_scratch = Some(scratch);
        Ok(())
    }

    #[cfg(not(windows))]
    fn install_gem_alloc_probe(&mut self, _ledger: &std::path::Path) -> Result<()> {
        bail!("gem allocator probe is Windows-only")
    }

    #[cfg(windows)]
    fn install_pickup_presentation_probe(&mut self) -> Result<()> {
        use super::pickup_presentation_probe;
        use super::threads::WindowsThreadController;

        let mut threads = WindowsThreadController::new(self.process_id);
        pickup_presentation_probe::install(
            self.delivery.runtime_mut().memory(),
            self.base,
            &mut threads,
        )
    }

    #[cfg(not(windows))]
    fn install_pickup_presentation_probe(&mut self) -> Result<()> {
        bail!("live pickup presentation instrumentation is Windows-only")
    }

    /// The live `location_context`, before the diagnostics stamp is taken.
    fn location_context_inner(&mut self) -> Result<Option<LocationContext>> {
        // clients#420: every loop gives the pending flag gate one cheap chance
        // to arm. Still-null is not an error here -- it reports not-ready.
        let arming = self.arm_event_flags();
        // Disjoint borrows: the readiness gate and the flag accessor are two
        // different fields of `self`.
        if let Err(error) = arming {
            if let Some(gate) = self.assumed_context.as_mut() {
                gate.observe(false);
            }
            self.save_identity.clear();
            self.live_identity_candidate = None;
            self.live_identity_ready_reads = 0;
            return Err(error);
        }
        let Some(flags) = self.event_flags.armed_mut() else {
            // Waiting for the game to finish loading: not gameplay-ready, which
            // is exactly what the existing send-gate consumes
            // (`require_runtime_context` -> Ok(None) -> no checks, no sends).
            self.save_identity.clear();
            self.live_identity_candidate = None;
            self.live_identity_ready_reads = 0;
            return Ok(self
                .assumed_context
                .as_mut()
                .map(|gate| gate.observe(false)));
        };
        if let Err(error) = flags.probe_manager_resilient() {
            if let Some(gate) = self.assumed_context.as_mut() {
                gate.observe(false);
            }
            self.save_identity.clear();
            self.live_identity_candidate = None;
            self.live_identity_ready_reads = 0;
            return Err(error);
        }
        if let Some(gate) = self.assumed_context.as_mut() {
            return Ok(Some(gate.observe(true)));
        }

        let polled = self.save_identity.poll()?;
        if let Some(path) = self.save_identity.take_write_denial() {
            client_eprintln!(
                "WARNING: shadPS4 could not write the game's save file ({path}): Windows refused the write. \
                 Nothing you do is being saved; the game will reload its last successful save when you die. \
                 Close the game, clear the read-only attribute on the files in that folder (or fix its \
                 permissions), and relaunch. Checks and deliveries still work meanwhile, but they are not persisted."
            );
        }
        let Some(identity) = polled else {
            self.live_identity_candidate = None;
            self.live_identity_ready_reads = 0;
            return Ok(None);
        };
        if self.live_identity_candidate.as_deref() == Some(identity.as_str()) {
            self.live_identity_ready_reads = self.live_identity_ready_reads.saturating_add(1);
        } else {
            self.live_identity_candidate = Some(identity.clone());
            self.live_identity_ready_reads = 1;
        }
        Ok(Some(LocationContext {
            save_identity: identity,
            gameplay_ready: self.live_identity_ready_reads >= ASSUMED_CONTEXT_STABLE_READS,
        }))
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
        use super::item_grant_probe;
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
        let item_grant_probe_state = match memory.allocate(item_grant_probe::PROBE_STATE_SIZE) {
            Ok(state_address) => {
                match item_grant_probe::install(&memory, base, state_address, &mut threads) {
                    Ok(()) => {
                        client_eprintln!("Blood-gem ItemGrant probe armed.");
                        Some(state_address)
                    }
                    Err(error) => {
                        client_eprintln!(
                            "Blood-gem ItemGrant probe inactive (delivery remains available): {error:#}"
                        );
                        None
                    }
                }
            }
            Err(error) => {
                client_eprintln!(
                    "Blood-gem ItemGrant probe inactive (delivery remains available): allocating capture state: {error:#}"
                );
                None
            }
        };

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
        let save_identity = SaveIdentityTracker::after_current_log(shad_log)?;
        Ok(Self {
            delivery,
            event_flags,
            shad_log: shad_log.to_owned(),
            assumed_context: assumed_identity.map(AssumedContextGate::new),
            save_identity,
            live_identity_candidate: None,
            live_identity_ready_reads: 0,
            base,
            absent_observations: std::collections::HashMap::new(),
            last_context: GrantContext::default(),
            item_grant_probe_state,
            gem_capture: None,
            shop_capture: None,
            vial_capture: None,
            pickup_notification_capture: None,
            boss_flag_census: None,
            rune_capture: None,
            gem_alloc_probe_state: None,
            gem_alloc_capture: None,
            category8_scratch: None,
            category8_gen_ctx_rsi: None,
            category8_last_generated: None,
            category8_inserted: std::collections::HashSet::new(),
            process_id,
        })
    }
}

impl BloodborneBackend for NativeBackend {
    #[allow(clippy::chunks_exact_to_as_chunks)]
    fn category8_generate(&mut self, gem_gen_param: u32) -> Result<String> {
        anyhow::ensure!(
            matches!(gem_gen_param, 102_901 | 123_000 | 90_040),
            "GemGenParam {gem_gen_param} is outside the #214 experiment allowlist"
        );
        let scratch = self
            .category8_scratch
            .context("category-8 experiment is not armed; enable the research/probe option")?;
        let rsi = self.category8_gen_ctx_rsi.context(
            "no live generator context; open the Blood Gem workshop tab once, close it, and retry",
        )?;
        anyhow::ensure!(
            (0x1_0000_0000..0x10_0000_0000).contains(&rsi),
            "captured generator context is not a canonical guest pointer"
        );
        let contract = super::contract::contract();
        let cell = |name: &str| -> Result<u64> { Ok(self.base + contract.state_cell(name)?.rva) };
        let request = cell("request")?;
        let quantity = cell("quantity")?;
        let result = cell("result")?;
        let done = cell("done")?;
        let descriptor = cell("descriptor")?;
        let scratch_cell = cell("item_quantity_pointer")?;
        let memory = self.delivery.runtime_mut().memory();
        anyhow::ensure!(
            memory.read_u32(request)? == 0,
            "native game-thread lane is busy"
        );
        memory.write(scratch, &[0; 0x300])?;
        // Every captured fresh-generation frame (enemy and fixed-map rows)
        // has this same self-relative shape. Zeroed writable memory is not a
        // valid argument: the generator follows these four pointers.
        memory.write_u64(scratch, 0xDEAD_BEEF_5432_1ABC)?;
        memory.write_u64(scratch + 8, 0xDEAD_BEEF_5432_1ABC)?;
        memory.write_u64(scratch + 0x10, rsi)?;
        memory.write_u64(scratch + 0x18, scratch + 0xD8)?;
        memory.write_u64(scratch + 0x20, scratch + 0x50)?;
        memory.write_u64(scratch + 0x28, scratch + 0x278)?;
        memory.write_u64(scratch + 0x30, scratch + 0x200)?;
        memory.write_u64(scratch + 0x38, self.base + 0x01A8_83A1)?;
        memory.write(descriptor, &[0; 24])?;
        memory.write_u64(descriptor + 8, rsi)?;
        memory.write_u64(scratch_cell, scratch)?;
        memory.write_u32(quantity, gem_gen_param)?;
        memory.write_u32(result, u32::MAX)?;
        memory.write_u32(done, 0)?;
        memory.write_u32(request, 3)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline && memory.read_u32(done)? != 1 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::ensure!(
            memory.read_u32(done)? == 1,
            "category-8 generator timed out"
        );
        anyhow::ensure!(
            memory.read_u32(request)? == 0,
            "category-8 request was not retired"
        );
        let rax = memory.read_u64(descriptor)?;
        let bytes = memory.read(scratch, 0x300)?;
        let mut handles = bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .filter(|word| word & 0xFFFF_0000 == 0xC080_0000)
            .collect::<Vec<_>>();
        for word in [rax as u32, (rax >> 32) as u32] {
            if word & 0xFFFF_0000 == 0xC080_0000 {
                handles.push(word);
            }
        }
        handles.sort_unstable();
        handles.dedup();
        let words = bytes
            .chunks_exact(8)
            .map(|chunk| format!("{:016X}", u64::from_le_bytes(chunk.try_into().unwrap())))
            .collect::<Vec<_>>()
            .join(" ");
        self.category8_last_generated = match handles.as_slice() {
            [handle] => Some((gem_gen_param, *handle)),
            _ => None,
        };
        Ok(format!(
            "AUDIT gem-gen id={gem_gen_param} rsi=0x{rsi:X} scratch=0x{scratch:X} rax=0x{rax:X} handles={handles:X?} words={words} (constructed only; inventory untouched)"
        ))
    }

    fn category8_insert(&mut self, variant: u8) -> Result<String> {
        anyhow::ensure!(variant <= 2, "variant must be 0, 1, or 2");
        let (id, handle) = self
            .category8_last_generated
            .context("no uniquely decoded B1 instance; run gem-gen first and inspect its result")?;
        anyhow::ensure!(
            !self.category8_inserted.contains(&handle),
            "handle 0x{handle:08X} was already inserted by this client"
        );
        let scratch = self
            .category8_scratch
            .context("category-8 scratch is unavailable")?;
        let contract = super::contract::contract();
        let cell = |name: &str| -> Result<u64> { Ok(self.base + contract.state_cell(name)?.rva) };
        let request = cell("request")?;
        let quantity = cell("quantity")?;
        let result = cell("result")?;
        let done = cell("done")?;
        let descriptor = cell("descriptor")?;
        let memory = self.delivery.runtime_mut().memory();
        anyhow::ensure!(
            memory.read_u32(request)? == 0,
            "native game-thread lane is busy"
        );
        let normalized = if variant == 2 { 0 } else { 0x8000_0000 | id };
        let object = if variant == 0 { scratch } else { 0 };
        memory.write(descriptor, &[0; 24])?;
        memory.write_u32(descriptor, handle)?;
        memory.write_u64(descriptor + 8, object)?;
        memory.write_u32(descriptor + 16, normalized)?;
        memory.write_u32(quantity, 1)?;
        memory.write_u32(result, u32::MAX)?;
        memory.write_u32(done, 0)?;
        memory.write_u32(request, 1)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline && memory.read_u32(done)? != 1 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::ensure!(
            memory.read_u32(done)? == 1,
            "category-8 insertion timed out"
        );
        anyhow::ensure!(
            memory.read_u32(request)? == 0,
            "category-8 insertion request was not retired"
        );
        let slot = memory.read_u32(result)?;
        anyhow::ensure!(
            slot != u32::MAX,
            "ItemGrant refused category-8 variant {variant}"
        );
        let matched = self
            .delivery
            .runtime_mut()
            .inventory_entries()
            .context("inventory geometry was unavailable after ItemGrant")?
            .into_iter()
            .find(|entry| {
                entry.word(0) == handle && (normalized == 0 || entry.word(4) == normalized)
            });
        let entry = matched.context(
            "ItemGrant returned a slot but the generated instance was not found in held inventory",
        )?;
        let record = entry
            .bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>();
        self.category8_inserted.insert(handle);
        Ok(format!(
            "AUDIT gem-insert variant={variant} id={id} handle=0x{handle:08X} object=0x{object:X} normalized=0x{normalized:08X} slot={slot} record={} PASS",
            record
        ))
    }

    fn record_location_checks(&mut self, locations: &[i64]) {
        if let Some(capture) = &mut self.pickup_notification_capture {
            capture.location_checks(locations);
        }
    }

    fn record_presentation_marker(&mut self, note: &str) -> bool {
        let Some(capture) = &mut self.pickup_notification_capture else {
            return false;
        };
        capture.marker(note);
        true
    }

    fn location_context(&mut self) -> Result<Option<LocationContext>> {
        let entries = self.delivery.runtime_mut().inventory_entries();
        if let Some(capture) = &mut self.shop_capture {
            capture.observe(entries.clone());
        }
        if let Some(capture) = &mut self.vial_capture {
            capture.observe(entries.clone());
        }
        if let Some(capture) = &mut self.rune_capture {
            capture.observe(entries.clone());
        }
        if let Some(capture) = &mut self.gem_capture {
            capture.observe_inventory(entries.clone());
        }
        if let Some(state) = self.gem_alloc_probe_state
            && let Some(capture) = &mut self.gem_alloc_capture
        {
            let rows = gem_alloc_probe::snapshots(self.delivery.runtime_mut().memory(), state);
            for row in &rows {
                let rsi = row.registers[2];
                let rcx = row.registers[4];
                if rcx != u64::from(u32::MAX) && (0x1_0000_0000..0x10_0000_0000).contains(&rsi) {
                    self.category8_gen_ctx_rsi = Some(rsi);
                }
            }
            capture.observe(rows);
        }
        if let Some(state_address) = self.item_grant_probe_state
            && let Some(capture) = &mut self.gem_capture
        {
            let snapshots = self
                .delivery
                .runtime_mut()
                .item_grant_probe_snapshots(state_address);
            capture.observe(snapshots);
        }
        if let Some(state_address) = self.item_grant_probe_state
            && let Some(capture) = &mut self.pickup_notification_capture
        {
            let snapshots = self
                .delivery
                .runtime_mut()
                .item_grant_probe_snapshots(state_address);
            capture.observe_native_calls(snapshots);
        }
        if let Some(capture) = &mut self.pickup_notification_capture {
            let snapshots = self.delivery.runtime_mut().pickup_presentation_snapshots();
            capture.observe_presentation_calls(snapshots);
        }
        let result = self.location_context_inner();
        if let Some(census) = &mut self.boss_flag_census {
            let (save_identity, gameplay_ready) = match &result {
                Ok(Some(context)) => (Some(context.save_identity.as_str()), context.gameplay_ready),
                _ => (None, false),
            };
            let values = match self.event_flags.armed_mut() {
                Some(flags) => BOSS_FLAGS
                    .iter()
                    .map(|&(flag, label)| (flag, label, flags.read_resilient(flag).ok()))
                    .collect::<Vec<_>>(),
                None => BOSS_FLAGS
                    .iter()
                    .map(|&(flag, label)| (flag, label, None))
                    .collect(),
            };
            census.observe(save_identity, gameplay_ready, &values);
        }
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
        if !matches!(&result, Ok(Some(context)) if context.gameplay_ready) {
            // A pointer captured before a load is not permission to kill the
            // next character. The hook repopulates it on the next live HP read.
            let _ = self.delivery.runtime_mut().clear_player_status();
        }
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

    fn write_event_flag(&mut self, event_flag: u32, enabled: bool) -> Result<()> {
        self.arm_event_flags()?;
        self.event_flags
            .armed_mut()
            .context("live event-flag writer is unavailable")?
            .write_resilient(event_flag, enabled)
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

    fn last_grant_went_to_storage(&mut self, tag: &str) -> bool {
        self.delivery.last_completion_went_to_storage(tag)
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
        if let Some(capture) = &mut self.pickup_notification_capture {
            capture.grant_state(grant, "submitted");
        }
        if grant.item_category == 255 {
            self.arm_event_flags()?;
            let Some(flags) = self.event_flags.armed_mut() else {
                if let Some(capture) = &mut self.pickup_notification_capture {
                    capture.grant_state(grant, "pending");
                }
                return Ok(OperationProgress::Pending);
            };
            flags.write_resilient(grant.normalized_item_id, true)?;
            if let Some(capture) = &mut self.pickup_notification_capture {
                capture.grant_state(grant, "complete");
            }
            return Ok(OperationProgress::Complete);
        }
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
            GrantStep::Pending => {
                if let Some(capture) = &mut self.pickup_notification_capture {
                    capture.grant_state(grant, "pending");
                }
                Ok(OperationProgress::Pending)
            }
            GrantStep::Complete => {
                if let Some(capture) = &mut self.pickup_notification_capture {
                    capture.grant_state(grant, "complete");
                }
                if self.delivery.last_completion_went_to_storage(&grant.tag) {
                    client_eprintln!(
                        "Delivered AP item {} to storage because it did not enter held inventory. Check the storage box in the Hunter's Dream.",
                        grant.tag
                    );
                }
                Ok(OperationProgress::Complete)
            }
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

    fn death_link_kill(&mut self) -> Result<bool> {
        // Use the same gameplay/save gate as every other mutation. A stale HP
        // pointer during a load is never permission to write.
        if !self
            .location_context_inner()?
            .is_some_and(|context| context.gameplay_ready)
        {
            let _ = self.delivery.runtime_mut().clear_player_status();
            return Ok(false);
        }
        self.delivery.runtime_mut().death_link_kill()
    }

    fn withdraw_unwitnessed_grant(&mut self, _tag: &str) -> Result<bool> {
        // The native request cell lives in guest memory; a leftover arm from a
        // previous process is cleared best-effort. The durable plan stays in the
        // ledger and re-publishes under a validated context.
        Ok(self.delivery.withdraw_stale())
    }

    fn retire_grant(&mut self, tag: &str, reason: &str) -> Result<bool> {
        Ok(self.delivery.retire_current(tag, reason))
    }
}
