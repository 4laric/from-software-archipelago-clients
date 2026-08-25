use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use archipelago_rs::{ClientStatus, Connection, ConnectionOptions, Event, ItemHandling};
use bb_archipelago::RUNTIME_BUILD;
use bb_archipelago::backend::{
    BloodborneBackend, EquipRequest, FileBackend, ItemGrant, LocationContext, MockBackend,
    OperationProgress,
};
use bb_archipelago::bridge::FileBridge;
use bb_archipelago::client_loop::{ClientLoop, IncomingItem, ItemPollResult};
use bb_archipelago::config::RuntimeConfig;
use bb_archipelago::event_flags::LiveEventFlags;
use bb_archipelago::ledger::{ReceiveLedger, WatermarkOutcome};
use bb_archipelago::native::backend::NativeBackend;

enum Backend {
    Live(FileBackend),
    Mock(Box<MockBackend>),
    Native(Box<NativeBackend>),
}

impl BloodborneBackend for Backend {
    fn location_context(&mut self) -> Result<Option<LocationContext>> {
        match self {
            Self::Live(backend) => backend.location_context(),
            Self::Mock(backend) => backend.location_context(),
            Self::Native(backend) => backend.location_context(),
        }
    }

    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        match self {
            Self::Live(backend) => backend.read_event_flag(event_flag),
            Self::Mock(backend) => backend.read_event_flag(event_flag),
            Self::Native(backend) => backend.read_event_flag(event_flag),
        }
    }

    fn target_weapon_level(&mut self) -> Result<Option<u8>> {
        match self {
            Self::Live(backend) => backend.target_weapon_level(),
            Self::Mock(backend) => backend.target_weapon_level(),
            Self::Native(backend) => backend.target_weapon_level(),
        }
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
        match self {
            Self::Live(backend) => backend.grant_item(grant),
            Self::Mock(backend) => backend.grant_item(grant),
            Self::Native(backend) => backend.grant_item(grant),
        }
    }

    fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress> {
        match self {
            Self::Live(backend) => backend.equip_item(request),
            Self::Mock(backend) => backend.equip_item(request),
            Self::Native(backend) => backend.equip_item(request),
        }
    }

    fn withdraw_unwitnessed_grant(&mut self, tag: &str) -> Result<bool> {
        match self {
            Self::Live(backend) => backend.withdraw_unwitnessed_grant(tag),
            Self::Mock(backend) => backend.withdraw_unwitnessed_grant(tag),
            Self::Native(backend) => backend.withdraw_unwitnessed_grant(tag),
        }
    }

    // Forward the watermark hooks rather than inheriting the attested-mode
    // defaults, or mock mode could never exercise the watermark path.
    fn read_save_watermark(&mut self) -> Result<Option<u64>> {
        match self {
            Self::Live(backend) => backend.read_save_watermark(),
            Self::Mock(backend) => backend.read_save_watermark(),
            Self::Native(backend) => backend.read_save_watermark(),
        }
    }

    fn write_save_watermark(&mut self, cursor: u64) -> Result<bool> {
        match self {
            Self::Live(backend) => backend.write_save_watermark(cursor),
            Self::Mock(backend) => backend.write_save_watermark(cursor),
            Self::Native(backend) => backend.write_save_watermark(cursor),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryMode {
    /// Grant via the Cheat Engine file bridge. No longer the default; kept as an
    /// explicit escape (`--delivery=ce-bridge`) and as the automatic fallback
    /// the default native path drops to when it cannot validate the image.
    CeBridge,
    /// The default: grant in-process via the native `bb-native-grant-v5` payload
    /// (stage 2). Fails closed on any image mismatch; on the default path (no
    /// explicit `--delivery`) an image it cannot validate falls back to the
    /// Cheat Engine bridge instead of hard-failing the player.
    Native,
}

struct Arguments {
    server: String,
    slot: String,
    config: PathBuf,
    ledger: PathBuf,
    password: Option<String>,
    mock: bool,
    assume_correct_save: bool,
    delivery: DeliveryMode,
    /// True only when the user passed `--delivery=...` explicitly. The default
    /// native path falls back to the Cheat Engine bridge on an image it cannot
    /// validate; an explicit `--delivery=native` hard-fails instead, because the
    /// user asked for native specifically.
    delivery_explicit: bool,
}

/// The explicitly unsafe assumed-correct-save identity token. Set by
/// `--assume-correct-save`; consulted by both the native and Cheat Engine paths.
const ASSUMED_IDENTITY: &str = "unsafe-operator-attested-correct-save";

const LIVE_ATTACH_TIMEOUT: Duration = Duration::from_secs(600);

fn attach_live_event_flags(shad_log: &Path) -> Result<LiveEventFlags> {
    let deadline = Instant::now() + LIVE_ATTACH_TIMEOUT;
    let mut next_report = Instant::now();
    loop {
        match LiveEventFlags::attach(shad_log) {
            Ok(flags) => return Ok(flags),
            Err(error) => {
                if Instant::now() >= deadline {
                    bail!(
                        "timed out after {} seconds waiting for live Bloodborne event flags; last error: {error:#}",
                        LIVE_ATTACH_TIMEOUT.as_secs()
                    );
                }
                if Instant::now() >= next_report {
                    eprintln!(
                        "Waiting for shadPS4 and Bloodborne gameplay initialization: {error:#}"
                    );
                    next_report = Instant::now() + Duration::from_secs(5);
                }
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

/// Attach the native (stage 2) backend, tolerating shadPS4 starting after the
/// client the same way the CE path does: retry only while the process is not
/// yet open, but fail fast on an image mismatch or any other refusal so a wrong
/// build never spins silently.
#[cfg(windows)]
fn attach_native_backend(
    shad_log: &Path,
    assumed_identity: Option<String>,
) -> Result<NativeBackend> {
    let deadline = Instant::now() + LIVE_ATTACH_TIMEOUT;
    let mut next_report = Instant::now();
    loop {
        match NativeBackend::attach(shad_log, assumed_identity.clone()) {
            Ok(backend) => return Ok(backend),
            Err(error) => {
                let waiting_for_process = format!("{error:#}").contains("is not running");
                if !waiting_for_process {
                    // Image mismatch, base verification failure, uncleared detour
                    // window: fail closed, do not retry.
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    bail!(
                        "timed out after {} seconds waiting for shadPS4 to start; last error: {error:#}",
                        LIVE_ATTACH_TIMEOUT.as_secs()
                    );
                }
                if Instant::now() >= next_report {
                    eprintln!(
                        "Waiting for shadPS4 to start before arming native delivery: {error:#}"
                    );
                    next_report = Instant::now() + Duration::from_secs(5);
                }
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_native_backend(
    _shad_log: &Path,
    _assumed_identity: Option<String>,
) -> Result<NativeBackend> {
    bail!("native Bloodborne delivery requires Windows")
}

/// Attach the Cheat Engine file-bridge backend (the `ce-bridge` delivery path).
///
/// Used both when the bridge is selected directly (`--delivery=ce-bridge`) and
/// as the automatic fallback the default native path drops to when it cannot
/// validate the running image. Keeping it in one function means the fallback and
/// the explicit selection arm the identical guarded backend.
fn attach_live_backend(config: &RuntimeConfig, assume_correct_save: bool) -> Result<Backend> {
    let shad_log = config
        .shad_log
        .as_deref()
        .context("live mode requires shad_log in the runtime config")?;
    // clients#369: fail fast on path misconfiguration (bridge_root) before
    // arming. shad_log is deliberately not preflighted: the attach retry loop
    // tolerates shadPS4 starting after the client, and its error now names the
    // setting.
    config.preflight_paths()?;
    let event_flags = attach_live_event_flags(shad_log)?;
    let attachment = event_flags.info();
    eprintln!(
        "Bloodborne AP client {} | CUSA03173 01.09 | shad PID {} | eboot 0x{:X} | direct flag backend ready",
        env!("CARGO_PKG_VERSION"),
        attachment.process_id,
        attachment.eboot_base
    );
    let bridge = FileBridge::new(&config.bridge_root);
    match bridge.read_state() {
        Ok(state) => eprintln!(
            "Grant bridge reports build {} | protocol {} | harness {}",
            state.build.as_deref().unwrap_or("missing"),
            state.protocol.as_deref().unwrap_or("missing"),
            state.harness.as_deref().unwrap_or("missing")
        ),
        Err(error) => eprintln!("Grant bridge state unavailable at startup: {error:#}"),
    }
    Ok(Backend::Live(if assume_correct_save {
        FileBackend::assuming_correct_save(bridge, event_flags, ASSUMED_IDENTITY.into())
    } else {
        FileBackend::new(bridge, event_flags)
    }))
}

fn arguments() -> Result<Arguments> {
    let mut args = env::args().skip(1);
    let Some(server) = args.next() else {
        bail!(
            "usage: bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD] [--mock] [--assume-correct-save] [--delivery=native|ce-bridge] (native is the default; on an image it cannot validate the default falls back to ce-bridge)"
        )
    };
    let slot = args.next().context("missing SLOT")?;
    let config = args.next().context("missing CONFIG")?.into();
    let ledger = args.next().context("missing LEDGER")?.into();
    let mut password = None;
    let mut mock = false;
    let mut assume_correct_save = false;
    // Native is the default delivery backend. It fails closed on any image it
    // cannot validate; the default path then falls back to the Cheat Engine
    // bridge (see `main`), so this default never strands the player.
    let mut delivery = DeliveryMode::Native;
    let mut delivery_explicit = false;
    for argument in args {
        if argument == "--mock" {
            mock = true;
        } else if argument == "--assume-correct-save" {
            assume_correct_save = true;
        } else if let Some(mode) = argument.strip_prefix("--delivery=") {
            delivery = match mode {
                "ce-bridge" => DeliveryMode::CeBridge,
                "native" => DeliveryMode::Native,
                other => bail!("unknown --delivery mode {other:?}; expected native or ce-bridge"),
            };
            delivery_explicit = true;
        } else if password.replace(argument).is_some() {
            bail!("only one password may be supplied");
        }
    }
    Ok(Arguments {
        server,
        slot,
        config,
        ledger,
        password,
        mock,
        assume_correct_save,
        delivery,
        delivery_explicit,
    })
}

fn main() -> Result<()> {
    let args = arguments()?;
    eprintln!("Bloodborne AP runtime build {RUNTIME_BUILD}");
    anyhow::ensure!(
        !(args.mock && args.assume_correct_save),
        "--mock and --assume-correct-save cannot be combined"
    );
    let mut config = RuntimeConfig::load(&args.config)?;
    if args.assume_correct_save {
        config.expected_save_identity = Some(ASSUMED_IDENTITY.into());
        eprintln!(
            "WARNING: UNSAFE MVP MODE ARMED. The client cannot identify the loaded character. You attest that the correct save for AP slot {:?} is loaded; do not switch characters while connected.",
            args.slot
        );
    }
    let backend = if args.mock {
        let mut backend = MockBackend::default();
        backend
            .set_flags
            .extend(config.mock_set_flags.iter().copied());
        Backend::Mock(Box::new(backend))
    } else if args.delivery == DeliveryMode::Native {
        let shad_log = config
            .shad_log
            .as_deref()
            .context("native delivery requires shad_log in the runtime config")?;
        config.preflight_paths()?;
        if args.delivery_explicit {
            eprintln!(
                "Native delivery selected explicitly (--delivery=native). It fails closed on any image mismatch and will NOT fall back: a build it cannot validate is refused, not delivered through the bridge."
            );
        } else {
            eprintln!(
                "Native delivery is the default. It fails closed on any image mismatch; if this image cannot be validated the client falls back to the Cheat Engine bridge automatically (pass --delivery=native to force native and fail closed, or --delivery=ce-bridge to select the bridge directly)."
            );
        }
        let assumed_identity = args
            .assume_correct_save
            .then(|| ASSUMED_IDENTITY.to_string());
        match attach_native_backend(shad_log, assumed_identity) {
            Ok(backend) => {
                eprintln!(
                    "Bloodborne AP client {} | CUSA03173 01.09 | native payload installed | eboot 0x{:X} | native delivery armed",
                    env!("CARGO_PKG_VERSION"),
                    backend.base()
                );
                Backend::Native(Box::new(backend))
            }
            Err(error) if !args.delivery_explicit => {
                // Default path only. Native could not attach and validate this
                // image -- an unknown serial/build, a failed image assert, or
                // another refusal. Native fails closed by design, so nothing was
                // patched or written; rather than strand the player on the
                // default we drop, loudly, to the guarded Cheat Engine bridge.
                // An explicit --delivery=native never reaches this arm: the user
                // asked for native specifically, so that path hard-fails below.
                eprintln!(
                    "Native delivery could not validate this image, so it will NOT be used: {error:#}"
                );
                eprintln!(
                    "Falling back to the Cheat Engine bridge (default auto-fallback). Pass --delivery=native to force native and fail closed instead, or --delivery=ce-bridge to select the bridge directly."
                );
                attach_live_backend(&config, args.assume_correct_save)?
            }
            Err(error) => return Err(error),
        }
    } else {
        attach_live_backend(&config, args.assume_correct_save)?
    };
    let ledger = ReceiveLedger::load(&args.ledger)
        .with_context(|| format!("loading receive ledger {}", args.ledger.display()))?;
    let mut backend = Some(backend);
    let mut ledger = Some(ledger);
    let mut options = ConnectionOptions::new().receive_items(ItemHandling::OtherWorlds {
        own_world: true,
        starting_inventory: true,
    });
    if let Some(password) = args.password {
        options = options.password(password);
    }
    let mut connection =
        Connection::<json::Value>::new(args.server, args.slot.clone(), Some("Bloodborne"), options);
    let mut runtime = None;
    let mut goal_location = None;
    let mut goal_reported = false;
    let mut ap_detail_printed = false;
    let mut last_location_error: Option<(String, Instant)> = None;
    let mut last_item_error: Option<(String, Instant)> = None;

    loop {
        let mut connected_now = false;
        let mut ap_error_seen = false;
        for event in connection.update() {
            match event {
                Event::Connected => {
                    connected_now = true;
                    eprintln!("Connected to Archipelago.");
                }
                Event::Print(message) => eprintln!("{message}"),
                Event::Error(error) => {
                    ap_error_seen = true;
                    eprintln!("Archipelago error: {error}");
                }
                _ => {}
            }
        }
        // Event::Error can be the placeholder variant; the stored
        // Disconnected error carries the real connect failure.
        if ap_error_seen
            && !ap_detail_printed
            && let archipelago_rs::ConnectionState::Disconnected(stored) = connection.state()
        {
            eprintln!("Archipelago connection failure detail: {stored}");
            ap_detail_printed = true;
        }
        if connected_now && let Some(client) = connection.client_mut() {
            client.sync()?;
        }

        if runtime.is_none()
            && let Some(client) = connection.client()
        {
            let seed_config = config.clone().apply_slot_data(client.slot_data())?;
            match seed_config.verify_suppression_install()? {
                Some(digest) => {
                    eprintln!("Verified installed vanilla-suppression binder SHA-256 {digest}.")
                }
                None => eprintln!("Seed does not claim installed vanilla-award suppression."),
            }
            let unsuppressed = seed_config
                .locations
                .iter()
                .filter(|location| !location.vanilla_award_suppressed)
                .count();
            let location_mode = if args.mock {
                "mock checks use bound save identity plus debounce"
            } else if args.assume_correct_save {
                "UNSAFE live checks use operator-attested save identity, a three-read gameplay gate, and per-location debounce"
            } else {
                "live check sends remain disarmed until gameplay/save identity is validated"
            };
            eprintln!(
                "Loaded {} location flag(s) and {} item binding(s) from the seed contract; {} location(s) still award vanilla contents; {}.",
                seed_config.locations.len(),
                seed_config.items.len(),
                unsuppressed,
                location_mode
            );
            goal_location = seed_config.goal_location;
            let mut new_runtime = ClientLoop::new(
                backend.take().context("backend was already initialized")?,
                seed_config,
                ledger.take().context("ledger was already initialized")?,
                args.ledger.clone(),
                client.seed_name(),
                args.slot.clone(),
            );
            // clients#296: before any polling, withdraw a grant command left
            // over by a previous process. It was published under a context this
            // process has not witnessed, and the harness would execute it
            // against whatever save is loaded now. The ledger keeps the plan;
            // the first validated poll re-publishes.
            match new_runtime.reconcile_pending_command() {
                Ok(true) => eprintln!(
                    "Withdrew an unwitnessed grant command left over from a previous session; \
                     the durable plan re-publishes it once the save context validates."
                ),
                Ok(false) => {}
                Err(error) => eprintln!("Pending-command reconciliation failed: {error:#}"),
            }
            runtime = Some(new_runtime);
        }

        if let (Some(runtime), Some(client)) = (runtime.as_mut(), connection.client_mut()) {
            while let Some(outcome) = runtime.take_watermark_notice() {
                // docs/SAVE-RECONCILIATION.md §8: every non-resume comparison
                // prints one line a player can act on, once per transition.
                match outcome {
                    WatermarkOutcome::Resume => eprintln!(
                        "Save watermark readable again; delivery state verified, resuming."
                    ),
                    WatermarkOutcome::Reissue => eprintln!(
                        "Restore detected: the save is behind the delivery ledger; \
                         re-delivering the erased items in order."
                    ),
                    WatermarkOutcome::AdoptSaveCursor => eprintln!(
                        "The delivery ledger is behind the save (ledger loss or rollback); \
                         adopting the save cursor -- nothing is re-granted."
                    ),
                    WatermarkOutcome::Hold => eprintln!(
                        "Held: delivery state could not be verified; no items granted, \
                         no checks sent."
                    ),
                }
            }
            let checked = client
                .checked_locations()
                .map(|location| location.id())
                .collect::<HashSet<_>>();
            if !goal_reported && goal_location.is_some_and(|goal| checked.contains(&goal)) {
                client.set_status(ClientStatus::Goal)?;
                goal_reported = true;
                eprintln!("Re-sent Bloodborne goal status from the server-checked goal location.");
            }
            match runtime.poll_locations(&checked) {
                Ok(newly_checked) => {
                    if last_location_error.take().is_some() {
                        eprintln!("Bloodborne location polling recovered.");
                    }
                    if !newly_checked.is_empty() {
                        if !goal_reported
                            && goal_location.is_some_and(|goal| newly_checked.contains(&goal))
                        {
                            // Send the irreversible goal status before retiring
                            // the check locally. If this send fails, the next
                            // poll sees the flag as new and retries both.
                            client.set_status(ClientStatus::Goal)?;
                            goal_reported = true;
                            eprintln!("Father Gascoigne defeated; sent Bloodborne goal status.");
                        }
                        client.mark_checked(newly_checked.iter().copied())?;
                        eprintln!("Sent location checks: {newly_checked:?}");
                    }
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    let report = last_location_error.as_ref().is_none_or(|(previous, when)| {
                        previous != &message || when.elapsed() >= Duration::from_secs(10)
                    });
                    if report {
                        eprintln!("Bloodborne location polling unavailable: {message}");
                        last_location_error = Some((message, Instant::now()));
                    }
                }
            }

            let received = client
                .received_items()
                .iter()
                .map(|item| IncomingItem {
                    index: item.index() as u64,
                    ap_item_id: item.item().id(),
                })
                .collect::<Vec<_>>();
            match runtime.poll_items(&received) {
                Ok(ItemPollResult::Completed(item)) => {
                    if last_item_error.take().is_some() {
                        eprintln!("Bloodborne item delivery recovered.");
                    }
                    eprintln!(
                        "Acknowledged AP item index {} id {} | received level {:?} | target {:?} | delivered {:?} | equip {:?}.",
                        item.index,
                        item.ap_item_id,
                        item.received_level,
                        item.target_level,
                        item.delivered_level,
                        item.equip_target
                    );
                }
                Ok(ItemPollResult::Blocked(blocked)) => {
                    if last_item_error.take().is_some() {
                        eprintln!("Bloodborne item delivery recovered.");
                    }
                    eprintln!(
                        "PARKED AP item index {} id {}: the grant terminally failed in the harness ({}: {}). \
                         The item is recorded as blocked and later items keep delivering. \
                         Inspect and resolve it with: bb-blocked {} \"{}\" \"{}\"",
                        blocked.index,
                        blocked.ap_item_id,
                        blocked.status,
                        blocked.detail,
                        args.ledger.display(),
                        client.seed_name(),
                        args.slot,
                    );
                }
                Ok(ItemPollResult::Idle | ItemPollResult::Pending) => {}
                // Held and Reconciled are surfaced through the watermark
                // notice channel above, exactly once per transition.
                Ok(ItemPollResult::Held | ItemPollResult::Reconciled(_)) => {}
                Err(error) => {
                    let message = format!("{error:#}");
                    let report = last_item_error.as_ref().is_none_or(|(previous, when)| {
                        previous != &message || when.elapsed() >= Duration::from_secs(10)
                    });
                    if report {
                        eprintln!("Bloodborne item delivery blocked: {message}");
                        last_item_error = Some((message, Instant::now()));
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(50));
    }
}
