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
use bb_archipelago::bridge::{FileBridge, missing_bridge_state};
use bb_archipelago::client_loop::{ClientLoop, IncomingItem, ItemPollResult};
use bb_archipelago::config::RuntimeConfig;
use bb_archipelago::event_flags::{LiveEventFlags, is_manager_not_initialized};
use bb_archipelago::client_eprintln;
use bb_archipelago::logging;
use bb_archipelago::ledger::{ReceiveLedger, WatermarkOutcome};
use bb_archipelago::native::attach_wait::AttachWaitFailure;
use bb_archipelago::native::backend::NativeBackend;

/// Console reporting policy for item-delivery failures (clients#404).
///
/// Two regimes share one dedup slot:
///
/// * A *missing grant bridge* is a standing condition with a remedy, not a
///   stream of incidents. It prints one actionable sentence the first time it
///   is seen and then stays silent for as long as it persists -- no matter how
///   much time passes -- because reprinting the same raw `os error 2` every
///   ten seconds is the wall of spam the issue is about.
/// * Every other error keeps the original behaviour: reprint when the message
///   changes, or when [`ITEM_ERROR_DEDUP`] has elapsed.
///
/// Either way a delivery success clears the slot, and clearing a non-empty
/// slot is what prints the recovery line.
#[derive(Debug, Default)]
struct ItemErrorReporter {
    last: Option<(String, Instant)>,
    bridge_missing: bool,
}

const ITEM_ERROR_DEDUP: Duration = Duration::from_secs(10);
const ITEM_DELIVERY_RECOVERED: &str = "Bloodborne item delivery recovered.";

impl ItemErrorReporter {
    /// Returns the line to print for `error`, or `None` to stay quiet.
    fn report(&mut self, error: &anyhow::Error, now: Instant) -> Option<String> {
        let message = format!("{error:#}");
        if let Some(missing) = missing_bridge_state(error) {
            let already_said = self.bridge_missing
                && self
                    .last
                    .as_ref()
                    .is_some_and(|(previous, _)| previous == &message);
            self.bridge_missing = true;
            self.last = Some((message, now));
            if already_said {
                return None;
            }
            return Some(format!(
                "Item grants are paused: no grant bridge state at {}. \
                 The Cheat Engine grant table is not running -- start it from the launcher, \
                 or re-run the client without --delivery=ce-bridge to use native delivery. \
                 Nothing is lost: queued items deliver once the bridge appears, \
                 and this line stays quiet until then.",
                missing.path.display()
            ));
        }
        self.bridge_missing = false;
        let report = self.last.as_ref().is_none_or(|(previous, when)| {
            previous != &message || now.duration_since(*when) >= ITEM_ERROR_DEDUP
        });
        if !report {
            return None;
        }
        self.last = Some((message.clone(), now));
        Some(format!("Bloodborne item delivery blocked: {message}"))
    }

    /// Returns the recovery line when a failure regime was in effect.
    fn recovered(&mut self) -> Option<&'static str> {
        self.bridge_missing = false;
        self.last.take().map(|_| ITEM_DELIVERY_RECOVERED)
    }
}

/// How the client answers a `Disconnected` connection state (clients#423).
///
/// `Disconnected` is terminal *for a `Connection` object* -- that is the
/// archipelago-rs contract, and reconnecting means constructing a new
/// `Connection`, exactly as shared `Core::reconnect()` does for ER/DS3/SDT.
/// What differs per error is whether reconnecting can possibly help.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisconnectVerdict {
    /// A transport failure: refused/closed socket, IO error, dropped
    /// connection. The room may simply be asleep. Retry with backoff.
    Retryable,
    /// The server (or our own argument handling) rejected this login. Retrying
    /// forever would mask a configuration error, so stop loudly.
    Terminal,
}

/// Classify the error stored in `ConnectionState::Disconnected`.
///
/// Retryable: [`Error::WebSocket`] (every tungstenite failure -- refused
/// connect, `ConnectionClosed`, a mid-session drop, transport IO),
/// [`Error::Async`] (smol IO), [`Error::ConnectionInterrupted`] (a panic in the
/// connect future -- it says nothing about the configuration), and the two
/// non-fatal-by-contract variants [`Error::ProtocolError`] /
/// [`Error::InvalidPacket`], which should never reach a `Disconnected` state at
/// all but must not be turned into a loud configuration accusation if they ever
/// do. [`Error::Elsewhere`] is a placeholder that carries no diagnosis, so it is
/// retried rather than blamed on the player's settings.
///
/// Terminal: [`Error::ConnectionRefused`] -- every `ConnectionError` reason is a
/// login rejection (`InvalidSlot`, `InvalidGame`, `InvalidVersion`,
/// `InvalidPassword`, `InvalidItemsHandling`, and `Unknown`, which the server
/// sent us as a refusal reason and which is quoted verbatim) -- plus
/// [`Error::ArgumentError`] (we called the library wrong),
/// [`Error::Serialize`] (we produced an unsendable message), and
/// [`Error::ClientDisconnected`] (this client asked to stop).
fn classify_disconnect(error: &archipelago_rs::Error) -> DisconnectVerdict {
    use archipelago_rs::Error;
    match error {
        Error::WebSocket(_)
        | Error::Async(_)
        | Error::ConnectionInterrupted
        | Error::ProtocolError(_)
        | Error::InvalidPacket(_)
        | Error::Elsewhere => DisconnectVerdict::Retryable,
        Error::ConnectionRefused(_)
        | Error::ArgumentError(_)
        | Error::Serialize(_)
        | Error::ClientDisconnected => DisconnectVerdict::Terminal,
    }
}

/// The loud, final line for a login the server (or our own call) rejected.
fn terminal_disconnect_message(address: &str, error: &archipelago_rs::Error) -> String {
    format!(
        "Archipelago refused this connection to {address} and the client will NOT retry: {error}. \
         Retrying a rejected login forever would only hide a configuration error. Check the server \
         address, the slot name, the password, that the slot's game is Bloodborne, and that this \
         client's version matches the server's."
    )
}

const RECONNECT_FIRST_DELAY: Duration = Duration::from_secs(5);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(60);
const RECONNECT_REMINDER: Duration = Duration::from_secs(60);
/// Printed by the `Event::Connected` arm for the first connect and for every
/// reconnect alike. [`ReconnectPolicy`] deliberately prints no recovery line of
/// its own, so recovery is announced exactly once.
const CONNECTED_LINE: &str = "Connected to Archipelago.";

/// Backoff and console policy for automatic reconnection (clients#423).
///
/// Messaging follows the clients#415 [`ItemErrorReporter`] shape: being offline
/// is one standing condition with a remedy, not a stream of incidents. One line
/// when retrying begins (naming the address and the sleeping-room remedy), a
/// short reminder about once a minute while it persists, and nothing on
/// recovery.
///
/// Time is injected (`now: Instant`) so the whole policy is host-testable.
#[derive(Debug, Default)]
struct ReconnectPolicy {
    /// `None` while connected/connecting. `Some(delay)` while offline.
    delay: Option<Duration>,
    /// When the next `Connection::new` may be constructed.
    next_attempt: Option<Instant>,
    /// When the opening notice or the last reminder was printed.
    last_notice: Option<Instant>,
}

impl ReconnectPolicy {
    /// Called on every tick where the connection is `Disconnected` with a
    /// retryable error. Returns the line to print, or `None` to stay quiet.
    fn notice(&mut self, now: Instant, address: &str) -> Option<String> {
        match self.last_notice {
            None => {
                self.last_notice = Some(now);
                self.delay = Some(RECONNECT_FIRST_DELAY);
                self.next_attempt = Some(now + RECONNECT_FIRST_DELAY);
                Some(format!(
                    "Lost the Archipelago connection to {address}; retrying automatically (first \
                     retry in {}s, backing off to {}s). If this is an archipelago.gg room, its \
                     port closes while the room sits idle -- open the room's page in your browser \
                     to wake it. Nothing is lost while offline: checks found now are sent when the \
                     connection returns, and so are the items you are owed. This line stays quiet \
                     until then.",
                    RECONNECT_FIRST_DELAY.as_secs(),
                    RECONNECT_MAX_DELAY.as_secs(),
                ))
            }
            Some(last) if now.duration_since(last) >= RECONNECT_REMINDER => {
                self.last_notice = Some(now);
                Some(format!(
                    "Still offline from {address}; still retrying. (Open the room's page to wake a \
                     sleeping archipelago.gg room.)"
                ))
            }
            Some(_) => None,
        }
    }

    /// Whether a fresh `Connection` should be constructed now. Consuming an
    /// attempt doubles the delay, capped at [`RECONNECT_MAX_DELAY`].
    fn attempt_due(&mut self, now: Instant) -> bool {
        let Some(next_attempt) = self.next_attempt else {
            return false;
        };
        if now < next_attempt {
            return false;
        }
        let delay = self.delay.unwrap_or(RECONNECT_FIRST_DELAY);
        let next = (delay * 2).min(RECONNECT_MAX_DELAY);
        self.delay = Some(next);
        self.next_attempt = Some(now + next);
        true
    }

    /// Called when the connection comes back. Clears the offline regime; prints
    /// nothing, because the `Event::Connected` arm already prints
    /// [`CONNECTED_LINE`] for a reconnect exactly as it does for the first
    /// connect.
    fn connected(&mut self) {
        self.delay = None;
        self.next_attempt = None;
        self.last_notice = None;
    }
}

/// Refuse a reconnect that landed on a different multiworld (clients#423).
///
/// Slot data is parsed once, and the runtime plus the receive ledger are keyed
/// by the seed name from that first `Connected`. If the host regenerated the
/// seed while we were offline, the reconnect hands us a *different* seed name;
/// continuing would deliver the new seed's items against the old seed's ledger
/// cursor. That is corruption, so it is terminal and loud.
fn guard_seed_name(bound: &str, offered: &str) -> Result<()> {
    anyhow::ensure!(
        bound == offered,
        "Refusing to continue: this Archipelago connection is serving seed {offered:?}, but this \
         client and its receive ledger are bound to seed {bound:?}. The host regenerated the \
         multiworld. Continuing would deliver the new seed's items against the old seed's delivery \
         ledger. Reconnect to the original room, or start the client again with a fresh ledger for \
         the new seed."
    );
    Ok(())
}

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
    /// Grant via the Cheat Engine file bridge. Not the default; selected
    /// explicitly with `--delivery=ce-bridge`, and the remedy the default native
    /// path points the player to when it cannot validate the running image.
    CeBridge,
    /// The default: grant in-process via the native `bb-native-grant-v5` payload
    /// (stage 2). Fails closed on any image it cannot validate; on the default
    /// path (no explicit `--delivery`) an unrecognised image hard-fails with
    /// guidance rather than silently falling back to the bridge (clients#413).
    Native,
}

impl DeliveryMode {
    /// ASCII console label for the resolved delivery backend. Derived from the
    /// resolved `DeliveryMode` so the startup banner reads correctly whichever
    /// default is in effect (clients#412 flips the default to native).
    fn label(self) -> &'static str {
        match self {
            DeliveryMode::Native => "native",
            DeliveryMode::CeBridge => "ce-bridge",
        }
    }
}

#[derive(Debug)]
struct Arguments {
    server: String,
    slot: String,
    config: PathBuf,
    ledger: PathBuf,
    password: Option<String>,
    mock: bool,
    assume_correct_save: bool,
    delivery: DeliveryMode,
    /// True only when the user passed `--delivery=...` explicitly. Shapes the
    /// native attach-failure message: the default path hard-fails with guidance
    /// to load the Cheat Engine table and re-run with `--delivery=ce-bridge`,
    /// while an explicit `--delivery=native` propagates the raw error.
    delivery_explicit: bool,
    /// `--log-file <path>`: tee everything this client prints into that file as
    /// well as the console (clients#425). Absent means console only, exactly as
    /// before -- the launcher's generated plan supplies it, a hand-started
    /// client need not.
    log_file: Option<PathBuf>,
}

/// The explicitly unsafe assumed-correct-save identity token. Set by
/// `--assume-correct-save`; consulted by both the native and Cheat Engine paths.
const ASSUMED_IDENTITY: &str = "unsafe-operator-attested-correct-save";

/// Actionable guidance shown when the *default* native path cannot attach to and
/// validate the running image. The default deliberately does NOT silently fall
/// back to the Cheat Engine bridge: with native as the default the CE table will
/// not be loaded, so a file-drop grant would sit unconsumed and delivered items
/// would silently vanish. clients#413 tracks the liveness handshake that will
/// let the client detect a loaded table before offering the bridge; until then
/// an unrecognised build hard-fails and tells the player exactly how to play now.
const UNRECOGNIZED_BUILD_GUIDANCE: &str = "This game build was not recognized, so native item delivery cannot run safely. To play now, load the Cheat Engine table and re-run with --delivery=ce-bridge. Otherwise this build is not yet supported.";

/// Map a native attach/validate failure onto the error the client exits with.
///
/// Both paths hard-fail -- native fails closed, so nothing was patched or
/// written. The default path (no explicit `--delivery`) wraps the failure with
/// [`UNRECOGNIZED_BUILD_GUIDANCE`] so an unrecognised build tells the player how
/// to play now; an explicit `--delivery=native` propagates the raw error,
/// because the user asked for native specifically. Neither path silently falls
/// back to the Cheat Engine bridge (clients#413).
fn native_attach_failure(error: anyhow::Error, delivery_explicit: bool) -> anyhow::Error {
    if delivery_explicit || points_at_the_shad_log(&error) || is_manager_not_initialized(&error) {
        error
    } else {
        error.context(UNRECOGNIZED_BUILD_GUIDANCE)
    }
}

/// clients#420: a startup-ordering failure -- the event-flag manager global is
/// still null because the game has not loaded a character -- is never an
/// unrecognised build. Attach itself no longer treats it as terminal (the flag
/// gate waits), but if it ever reaches here it must say what it is: the Cheat
/// Engine lane does not provide flag reads either, so routing the player to the
/// bridge over this is actively wrong (clients#416).
///
/// True when the attach failure is the clients#418 "no fresh eboot base ever
/// appeared" outcome: the suspect is the configured `shad_log` path, not the
/// game build, and the failure already names the path and what to compare it
/// against. Appending [`UNRECOGNIZED_BUILD_GUIDANCE`] here would send the player
/// to the Cheat Engine table over a misconfigured log file.
fn points_at_the_shad_log(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<AttachWaitFailure>()
            .is_some_and(AttachWaitFailure::is_stale_log_evidence)
    })
}

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
                    client_eprintln!(
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
                    client_eprintln!(
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
/// Selected explicitly with `--delivery=ce-bridge`; it is the remedy the default
/// native path points the player to when it cannot validate the running image.
/// Factored out so the explicit selection here has one home. Note the default
/// native path does NOT call this on failure -- it hard-fails with guidance
/// rather than silently arming a bridge the player has no CE table loaded for
/// (clients#413).
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
    client_eprintln!(
        "Bloodborne AP client {} | CUSA03173 01.09 | shad PID {} | eboot 0x{:X} | direct flag backend ready",
        env!("CARGO_PKG_VERSION"),
        attachment.process_id,
        attachment.eboot_base
    );
    let bridge = FileBridge::new(&config.bridge_root);
    match bridge.read_state() {
        Ok(state) => client_eprintln!(
            "Grant bridge reports build {} | protocol {} | harness {}",
            state.build.as_deref().unwrap_or("missing"),
            state.protocol.as_deref().unwrap_or("missing"),
            state.harness.as_deref().unwrap_or("missing")
        ),
        Err(error) => client_eprintln!("Grant bridge state unavailable at startup: {error:#}"),
    }
    Ok(Backend::Live(if assume_correct_save {
        FileBackend::assuming_correct_save(bridge, event_flags, ASSUMED_IDENTITY.into())
    } else {
        FileBackend::new(bridge, event_flags)
    }))
}

fn arguments() -> Result<Arguments> {
    parse_args(env::args().skip(1))
}

fn parse_args<I: Iterator<Item = String>>(mut args: I) -> Result<Arguments> {
    let Some(server) = args.next() else {
        bail!(
            "usage: bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD] [--mock] [--assume-correct-save] [--delivery=native|ce-bridge] [--log-file PATH] (native is the default; on an image it cannot validate the default hard-fails and asks you to load the Cheat Engine table and re-run with --delivery=ce-bridge)"
        )
    };
    let slot = args.next().context("missing SLOT")?;
    let config = args.next().context("missing CONFIG")?.into();
    let ledger = args.next().context("missing LEDGER")?.into();
    let mut password = None;
    let mut mock = false;
    let mut assume_correct_save = false;
    // Native is the default delivery backend. It fails closed on any image it
    // cannot validate; on the default path an unrecognised image hard-fails with
    // guidance -- it does NOT fall back to the bridge (see `main`, clients#413).
    let mut delivery = DeliveryMode::Native;
    let mut delivery_explicit = false;
    let mut log_file = None;
    while let Some(argument) = args.next() {
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
        } else if argument == "--log-file" {
            // Two-token form: what the launcher's generated plan emits
            // ("--log-file", "{client_log}"). A bare flag is refused rather
            // than swallowed as the optional PASSWORD positional, which would
            // send a path to the server as a password.
            let path = args
                .next()
                .context("--log-file requires a path argument")?;
            log_file = Some(PathBuf::from(path));
        } else if let Some(path) = argument.strip_prefix("--log-file=") {
            log_file = Some(PathBuf::from(path));
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
        log_file,
    })
}

fn main() -> Result<()> {
    let args = arguments()?;
    // The tee is armed before ANY other output (clients#425), so the session log
    // carries the whole run rather than its tail. Everything below prints through
    // `client_eprintln!`, which reaches both the console the player is watching
    // and this file. Residual, accepted: a failure inside `arguments()` above
    // happens before the path is known and can only reach the console.
    if let Some(path) = args.log_file.as_deref() {
        logging::install_log_file(path)
            .with_context(|| format!("could not open the client log {}", path.display()))?;
    }
    client_eprintln!("Bloodborne AP runtime build {RUNTIME_BUILD}");
    // Console-legibility banner (clients#404 companion): a normal, working
    // launch otherwise prints only build/attach diagnostics interleaved with
    // long silent waits, so a healthy client looked identical to a frozen or
    // dead one -- a playtester (oz, 2026-08-24) saw the console and thought his
    // run was broken. Print exactly one at-a-glance "alive" line, on every
    // launch, before any attach/connect work and NOT gated behind any error
    // path. The delivery label is derived from the *resolved* mode, so it stays
    // correct whichever default is in effect. This client streams all of its
    // diagnostics to this console, which is the answer to "is it working /
    // where do I look"; with `--log-file` the same lines are also teed to that
    // file, and the banner names it so the player knows what to send back.
    client_eprintln!(
        "bb-ap-client running - delivery: {} - server: {} - slot: {} - diagnostics stream to this console{}",
        args.delivery.label(),
        args.server,
        args.slot,
        match args.log_file.as_deref() {
            Some(path) => format!(" and to {}", path.display()),
            None => String::new(),
        }
    );
    anyhow::ensure!(
        !(args.mock && args.assume_correct_save),
        "--mock and --assume-correct-save cannot be combined"
    );
    let mut config = RuntimeConfig::load(&args.config)?;
    if args.assume_correct_save {
        config.expected_save_identity = Some(ASSUMED_IDENTITY.into());
        client_eprintln!(
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
            client_eprintln!(
                "Native delivery selected explicitly (--delivery=native). It fails closed on any image mismatch and will NOT fall back: a build it cannot validate is refused, not delivered through the bridge."
            );
        } else {
            client_eprintln!(
                "Native delivery is the default. It fails closed on any image mismatch: a build it cannot validate is refused, not delivered. If this build is not recognized the client stops and asks you to load the Cheat Engine table and re-run with --delivery=ce-bridge (it does NOT silently fall back to the bridge)."
            );
        }
        let assumed_identity = args
            .assume_correct_save
            .then(|| ASSUMED_IDENTITY.to_string());
        match attach_native_backend(shad_log, assumed_identity) {
            Ok(backend) => {
                client_eprintln!(
                    "Bloodborne AP client {} | CUSA03173 01.09 | native payload installed | eboot 0x{:X} | native delivery armed",
                    env!("CARGO_PKG_VERSION"),
                    backend.base()
                );
                Backend::Native(Box::new(backend))
            }
            // Native could not attach and validate this image -- an unknown
            // serial/build, a failed image assert, or another refusal. Native
            // fails closed by design, so nothing was patched or written. We do
            // NOT silently fall back to the Cheat Engine bridge: with native as
            // the default the CE table will not be loaded, so a file-drop grant
            // would sit unconsumed and delivered items would vanish (clients#413
            // tracks the liveness handshake that will make a safe fallback
            // possible). The default path hard-fails with actionable guidance;
            // an explicit --delivery=native propagates the raw error.
            Err(error) => return Err(native_attach_failure(error, args.delivery_explicit)),
        }
    } else {
        attach_live_backend(&config, args.assume_correct_save)?
    };
    let ledger = ReceiveLedger::load(&args.ledger)
        .with_context(|| format!("loading receive ledger {}", args.ledger.display()))?;
    let mut backend = Some(backend);
    let mut ledger = Some(ledger);
    // Rebuilt for every connection attempt: `ConnectionOptions` is not `Clone`,
    // and clients#423 constructs a fresh `Connection` per retry.
    let password = args.password.clone();
    let connection_options = move || {
        let options = ConnectionOptions::new().receive_items(ItemHandling::OtherWorlds {
            own_world: true,
            starting_inventory: true,
        });
        match password.clone() {
            Some(password) => options.password(password),
            None => options,
        }
    };
    let mut connection = Connection::<json::Value>::new(
        args.server.clone(),
        args.slot.clone(),
        Some("Bloodborne"),
        connection_options(),
    );
    let mut reconnect = ReconnectPolicy::default();
    // Annotated because the clients#423 seed guard reads `runtime` above the
    // point where `ClientLoop::new` would otherwise infer it.
    let mut runtime: Option<ClientLoop<Backend>> = None;
    let mut goal_location = None;
    let mut goal_reported = false;
    let mut ap_detail_printed = false;
    let mut last_location_error: Option<(String, Instant)> = None;
    let mut item_errors = ItemErrorReporter::default();

    loop {
        let mut connected_now = false;
        let mut ap_error_seen = false;
        for event in connection.update() {
            match event {
                Event::Connected => {
                    connected_now = true;
                    // The one recovery line, for the first connect and every
                    // reconnect alike (clients#423): `ReconnectPolicy` prints
                    // none of its own, so this is never doubled.
                    client_eprintln!("{CONNECTED_LINE}");
                }
                Event::Print(message) => client_eprintln!("{message}"),
                Event::Error(error) => {
                    ap_error_seen = true;
                    client_eprintln!("Archipelago error: {error}");
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
            client_eprintln!("Archipelago connection failure detail: {stored}");
            ap_detail_printed = true;
        }
        if connected_now {
            reconnect.connected();
            ap_detail_printed = false;
            // clients#423: a reconnect that landed on a regenerated seed must
            // never continue against the old seed's ledger bindings. Checked
            // before any polling this tick, and before the slot-data parse
            // below can be reached again.
            if let (Some(runtime), Some(client)) = (runtime.as_ref(), connection.client()) {
                guard_seed_name(runtime.seed_name(), client.seed_name())?;
            }
        }
        if connected_now && let Some(client) = connection.client_mut() {
            // A fresh socket knows nothing about what we have already checked:
            // `sync()` re-requests server state, and the location poll below
            // re-derives `newly_checked` from that fresh set (never from a
            // local already-sent cache), so a check found while offline is
            // re-sent on the next tick.
            client.sync()?;
        }
        if connection.is_disconnected() {
            let error = connection.err();
            let verdict = classify_disconnect(error);
            if verdict == DisconnectVerdict::Terminal {
                bail!("{}", terminal_disconnect_message(&args.server, error));
            }
            let now = Instant::now();
            if let Some(line) = reconnect.notice(now, &args.server) {
                client_eprintln!("{line}");
            }
            if reconnect.attempt_due(now) {
                connection = Connection::<json::Value>::new(
                    args.server.clone(),
                    args.slot.clone(),
                    Some("Bloodborne"),
                    connection_options(),
                );
            }
        }

        if runtime.is_none()
            && let Some(client) = connection.client()
        {
            let seed_config = config.clone().apply_slot_data(client.slot_data())?;
            match seed_config.verify_suppression_install()? {
                Some(digest) => {
                    client_eprintln!("Verified installed vanilla-suppression binder SHA-256 {digest}.")
                }
                None => client_eprintln!("Seed does not claim installed vanilla-award suppression."),
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
            client_eprintln!(
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
                Ok(true) => client_eprintln!(
                    "Withdrew an unwitnessed grant command left over from a previous session; \
                     the durable plan re-publishes it once the save context validates."
                ),
                Ok(false) => {}
                Err(error) => client_eprintln!("Pending-command reconciliation failed: {error:#}"),
            }
            runtime = Some(new_runtime);
        }

        if let (Some(runtime), Some(client)) = (runtime.as_mut(), connection.client_mut()) {
            while let Some(outcome) = runtime.take_watermark_notice() {
                // docs/SAVE-RECONCILIATION.md §8: every non-resume comparison
                // prints one line a player can act on, once per transition.
                match outcome {
                    WatermarkOutcome::Resume => client_eprintln!(
                        "Save watermark readable again; delivery state verified, resuming."
                    ),
                    WatermarkOutcome::Reissue => client_eprintln!(
                        "Restore detected: the save is behind the delivery ledger; \
                         re-delivering the erased items in order."
                    ),
                    WatermarkOutcome::AdoptSaveCursor => client_eprintln!(
                        "The delivery ledger is behind the save (ledger loss or rollback); \
                         adopting the save cursor -- nothing is re-granted."
                    ),
                    WatermarkOutcome::Hold => client_eprintln!(
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
                client_eprintln!("Re-sent Bloodborne goal status from the server-checked goal location.");
            }
            match runtime.poll_locations(&checked) {
                Ok(newly_checked) => {
                    if last_location_error.take().is_some() {
                        client_eprintln!("Bloodborne location polling recovered.");
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
                            client_eprintln!("Father Gascoigne defeated; sent Bloodborne goal status.");
                        }
                        client.mark_checked(newly_checked.iter().copied())?;
                        client_eprintln!("Sent location checks: {newly_checked:?}");
                    }
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    let report = last_location_error.as_ref().is_none_or(|(previous, when)| {
                        previous != &message || when.elapsed() >= Duration::from_secs(10)
                    });
                    if report {
                        client_eprintln!("Bloodborne location polling unavailable: {message}");
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
                    if let Some(line) = item_errors.recovered() {
                        client_eprintln!("{line}");
                    }
                    client_eprintln!(
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
                    if let Some(line) = item_errors.recovered() {
                        client_eprintln!("{line}");
                    }
                    client_eprintln!(
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
                    if let Some(line) = item_errors.report(&error, Instant::now()) {
                        client_eprintln!("{line}");
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bb_archipelago::bridge::BridgeStateMissing;

    fn missing_bridge_error() -> anyhow::Error {
        // The condition reaches the console wrapped in grant context, exactly
        // as it does in the live poll loop.
        anyhow::Error::new(BridgeStateMissing {
            path: PathBuf::from("C:\\bridge\\native-grant-state.txt"),
        })
        .context("granting ap_7")
    }

    /// clients#404 motivating case, end to end: one actionable sentence, then
    /// silence for as long as the bridge stays missing, then the recovery line
    /// on the first successful delivery.
    #[test]
    fn missing_bridge_says_something_actionable_once_then_goes_quiet_then_recovers() {
        let mut reporter = ItemErrorReporter::default();
        let start = Instant::now();

        let first = reporter
            .report(&missing_bridge_error(), start)
            .expect("the first missing-bridge failure must say something");
        assert!(first.contains("Item grants are paused"), "{first}");
        assert!(first.contains("native-grant-state.txt"), "{first}");
        assert!(first.contains("Cheat Engine grant table"), "{first}");
        assert!(first.contains("--delivery=ce-bridge"), "{first}");
        assert!(first.is_ascii(), "in-console strings stay ASCII: {first}");

        // Quiet, and quiet even far past the generic dedup window: this is a
        // standing condition, not a stream of incidents.
        for elapsed in [1, 10, 11, 600] {
            let later = start + Duration::from_secs(elapsed);
            assert_eq!(reporter.report(&missing_bridge_error(), later), None);
        }

        assert_eq!(reporter.recovered(), Some(ITEM_DELIVERY_RECOVERED));
        // ...and only once: a second success is silent.
        assert_eq!(reporter.recovered(), None);
    }

    #[test]
    fn missing_bridge_speaks_again_after_a_recovery() {
        let mut reporter = ItemErrorReporter::default();
        let start = Instant::now();
        assert!(reporter.report(&missing_bridge_error(), start).is_some());
        assert_eq!(reporter.recovered(), Some(ITEM_DELIVERY_RECOVERED));
        let after = reporter.report(&missing_bridge_error(), start + Duration::from_secs(1));
        assert!(
            after.is_some(),
            "a fresh outage is a new incident and must be announced again"
        );
    }

    #[test]
    fn other_errors_keep_the_ten_second_dedup() {
        let mut reporter = ItemErrorReporter::default();
        let start = Instant::now();
        let error = || anyhow::anyhow!("grant ap_7 timed out after 30 seconds");

        let first = reporter.report(&error(), start).expect("first report");
        assert!(
            first.starts_with("Bloodborne item delivery blocked:"),
            "{first}"
        );
        assert_eq!(
            reporter.report(&error(), start + Duration::from_secs(9)),
            None
        );
        let reprinted = reporter.report(&error(), start + Duration::from_secs(10));
        assert!(
            reprinted.is_some(),
            "the generic path still reprints once the window elapses"
        );
    }

    #[test]
    fn a_different_error_after_the_missing_bridge_is_reported() {
        let mut reporter = ItemErrorReporter::default();
        let start = Instant::now();
        assert!(reporter.report(&missing_bridge_error(), start).is_some());
        let other = anyhow::anyhow!("grant bridge protocol mismatch");
        let line = reporter
            .report(&other, start + Duration::from_secs(1))
            .expect("an unrelated failure is not silenced by the paused bridge");
        assert!(line.contains("protocol mismatch"), "{line}");
        // Back to missing: that is a new announcement, not a continuation.
        let again = reporter.report(&missing_bridge_error(), start + Duration::from_secs(2));
        assert!(again.is_some());
    }

    fn base_args(extra: &[&str]) -> Vec<String> {
        let mut v = vec![
            "server".to_string(),
            "slot".to_string(),
            "config.json".to_string(),
            "ledger.json".to_string(),
        ];
        v.extend(extra.iter().map(|s| s.to_string()));
        v
    }

    #[test]
    fn default_delivery_is_native_and_not_explicit() {
        let args = parse_args(base_args(&[]).into_iter()).expect("parse");
        assert_eq!(args.delivery, DeliveryMode::Native);
        assert!(!args.delivery_explicit);
    }

    #[test]
    fn explicit_ce_bridge_selects_bridge_and_is_explicit() {
        let args = parse_args(base_args(&["--delivery=ce-bridge"]).into_iter()).expect("parse");
        assert_eq!(args.delivery, DeliveryMode::CeBridge);
        assert!(args.delivery_explicit);
    }

    #[test]
    fn explicit_native_selects_native_and_is_explicit() {
        let args = parse_args(base_args(&["--delivery=native"]).into_iter()).expect("parse");
        assert_eq!(args.delivery, DeliveryMode::Native);
        assert!(args.delivery_explicit);
    }

    /// The two-token form the launcher's generated plan emits
    /// ("--log-file", "{client_log}").
    #[test]
    fn log_file_is_taken_from_the_following_argument() {
        let args = parse_args(base_args(&["--log-file", "sessions/abc/client.log"]).into_iter())
            .expect("parse");
        assert_eq!(
            args.log_file.as_deref(),
            Some(Path::new("sessions/abc/client.log"))
        );
        // The path must not have been mistaken for the optional PASSWORD.
        assert!(args.password.is_none(), "{:?}", args.password);
    }

    #[test]
    fn log_file_also_accepts_the_joined_form() {
        let args =
            parse_args(base_args(&["--log-file=client.log"]).into_iter()).expect("parse");
        assert_eq!(args.log_file.as_deref(), Some(Path::new("client.log")));
    }

    /// Without the flag nothing about logging changes: no path, and therefore
    /// no file is ever opened.
    #[test]
    fn no_log_file_by_default() {
        let args = parse_args(base_args(&[]).into_iter()).expect("parse");
        assert!(args.log_file.is_none());
    }

    /// A bare trailing `--log-file` must fail loudly rather than silently
    /// leaving the client unlogged.
    #[test]
    fn a_bare_log_file_flag_is_refused() {
        let error = parse_args(base_args(&["--log-file"]).into_iter()).unwrap_err();
        assert!(
            format!("{error:#}").contains("--log-file requires a path"),
            "{error:#}"
        );
    }

    /// The usage line advertises the flag: a player reading the refusal has to
    /// be able to find it.
    #[test]
    fn usage_names_the_log_file_flag() {
        let error = parse_args(Vec::<String>::new().into_iter()).unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("--log-file"), "{rendered}");
        assert!(rendered.is_ascii(), "{rendered}");
    }

    #[test]
    fn unknown_delivery_mode_is_rejected() {
        let error = parse_args(base_args(&["--delivery=bogus"]).into_iter()).unwrap_err();
        assert!(format!("{error:#}").contains("unknown --delivery mode"));
    }

    #[test]
    fn delivery_mode_labels_are_stable_and_ascii() {
        // The startup banner (console-legibility line) prints these; they must
        // match the --delivery=... spellings and stay ASCII for the console.
        assert_eq!(DeliveryMode::Native.label(), "native");
        assert_eq!(DeliveryMode::CeBridge.label(), "ce-bridge");
        assert!(DeliveryMode::Native.label().is_ascii());
        assert!(DeliveryMode::CeBridge.label().is_ascii());
    }

    // The backend-construction step (native attach / Live bridge) needs a live
    // shadPS4 process and, for native, `#[cfg(windows)]` seams, so "default on a
    // recognized image -> native" and "explicit ce-bridge -> Live backend"
    // cannot be exercised host-side. What IS host-testable is the decision that
    // governs Direction A: given a native attach/validate failure, the default
    // path must hard-fail with guidance (never a Live-backend fallback), while
    // an explicit --delivery=native propagates the raw error.
    #[test]
    fn default_native_failure_hard_fails_with_guidance_not_fallback() {
        let error = native_attach_failure(anyhow::anyhow!("image assert: unknown build"), false);
        let rendered = format!("{error:#}");
        assert!(rendered.contains("was not recognized"), "{rendered}");
        assert!(rendered.contains("--delivery=ce-bridge"), "{rendered}");
        // The underlying failure is preserved in the error chain.
        assert!(rendered.contains("image assert"), "{rendered}");
    }

    #[test]
    fn explicit_native_failure_propagates_raw_error() {
        let error = native_attach_failure(anyhow::anyhow!("image assert: unknown build"), true);
        let rendered = format!("{error:#}");
        assert!(rendered.contains("image assert"), "{rendered}");
        assert!(!rendered.contains("was not recognized"), "{rendered}");
    }

    /// clients#418 outcome (a): no fresh base ever appeared. The build is not
    /// the suspect, so the CE-table guidance must NOT be appended -- the message
    /// that names the configured log path is the whole answer.
    #[test]
    fn a_stale_log_failure_keeps_the_build_guidance_off() {
        let failure = AttachWaitFailure::NoFreshBase {
            shad_log: String::from("C:\\shadPS4\\user\\log\\shad_log.txt"),
            waited: Duration::from_secs(90),
        };
        let error = native_attach_failure(anyhow::Error::new(failure), false);
        let rendered = format!("{error:#}");
        assert!(rendered.contains("shad_log.txt"), "{rendered}");
        assert!(!rendered.contains("was not recognized"), "{rendered}");
        assert!(!rendered.contains("--delivery=ce-bridge"), "{rendered}");
    }

    /// clients#420: the event-flag manager being uninitialized is a
    /// startup-ordering state, not an unrecognised build. Attach no longer
    /// treats it as terminal, but the routing must fail safe regardless: this
    /// class must never carry the Cheat Engine / ce-bridge guidance, because
    /// the bridge lane provides no flag reads either (clients#416).
    #[test]
    fn an_uninitialized_flag_manager_never_gets_the_cheat_engine_guidance() {
        let error = native_attach_failure(
            anyhow::Error::new(bb_archipelago::event_flags::EventFlagManagerNotInitialized)
                .context("attaching live Bloodborne event flags"),
            false,
        );
        let rendered = format!("{error:#}");
        assert!(rendered.contains("not initialized yet"), "{rendered}");
        assert!(!rendered.contains("Cheat Engine"), "{rendered}");
        assert!(!rendered.contains("ce-bridge"), "{rendered}");
        assert!(!rendered.contains("was not recognized"), "{rendered}");
    }

    /// clients#418 outcome (b): a confirmed base whose image is not ours. That
    /// IS an unrecognised build, so the CE-bridge guidance still applies.
    #[test]
    fn a_rejected_image_after_the_wait_still_gets_the_build_guidance() {
        let failure = AttachWaitFailure::ImageRejected {
            base: 0x5570000,
            detail: String::from("assert consume_hook mismatched"),
        };
        let error = native_attach_failure(anyhow::Error::new(failure), false);
        let rendered = format!("{error:#}");
        assert!(rendered.contains("was not recognized"), "{rendered}");
        assert!(rendered.contains("--delivery=ce-bridge"), "{rendered}");
        assert!(rendered.contains("consume_hook"), "{rendered}");
    }

    // ---- clients#423: automatic reconnection ----------------------------

    fn io_error(kind: std::io::ErrorKind) -> archipelago_rs::Error {
        archipelago_rs::Error::Async(std::io::Error::new(kind, "socket"))
    }

    /// Transport failures -- a refused connect, a dropped socket, an IO error --
    /// are exactly the sleeping-room case the issue is about, so they retry.
    ///
    /// `Error::WebSocket(tungstenite::Error)` belongs to this set too but cannot
    /// be constructed here without taking a `tungstenite` dependency on this
    /// crate; it is covered by the same match arm.
    #[test]
    fn transport_failures_are_retryable() {
        for error in [
            io_error(std::io::ErrorKind::ConnectionRefused),
            io_error(std::io::ErrorKind::ConnectionReset),
            io_error(std::io::ErrorKind::TimedOut),
            archipelago_rs::Error::ConnectionInterrupted,
            archipelago_rs::Error::Elsewhere,
            archipelago_rs::Error::InvalidPacket("bad".into()),
            archipelago_rs::Error::ProtocolError(archipelago_rs::ProtocolError::EmptyPlayers),
        ] {
            assert_eq!(
                classify_disconnect(&error),
                DisconnectVerdict::Retryable,
                "{error}"
            );
        }
    }

    /// A rejected login is a configuration error. Retrying it forever would
    /// hide the one thing the player has to fix, so every refusal reason is
    /// terminal.
    #[test]
    fn login_rejections_are_terminal() {
        use archipelago_rs::ConnectionError;
        for reason in [
            ConnectionError::InvalidSlot,
            ConnectionError::InvalidGame,
            ConnectionError::InvalidVersion,
            ConnectionError::InvalidPassword,
            ConnectionError::InvalidItemsHandling,
            ConnectionError::Unknown("SomethingNew".into()),
        ] {
            let error = archipelago_rs::Error::ConnectionRefused(vec![reason]);
            assert_eq!(
                classify_disconnect(&error),
                DisconnectVerdict::Terminal,
                "{error}"
            );
        }
        for error in [
            archipelago_rs::Error::ArgumentError(archipelago_rs::ArgumentError::InvalidSlot(7)),
            archipelago_rs::Error::ClientDisconnected,
        ] {
            assert_eq!(
                classify_disconnect(&error),
                DisconnectVerdict::Terminal,
                "{error}"
            );
        }
    }

    /// The terminal line has to be loud enough to act on: it names the address,
    /// quotes the server's reason, and says outright that no retry is coming.
    #[test]
    fn the_terminal_line_names_the_address_the_reason_and_the_refusal_to_retry() {
        let error = archipelago_rs::Error::ConnectionRefused(vec![
            archipelago_rs::ConnectionError::InvalidPassword,
        ]);
        let line = terminal_disconnect_message("archipelago.gg:38281", &error);
        assert!(line.contains("archipelago.gg:38281"), "{line}");
        assert!(line.contains("will NOT retry"), "{line}");
        assert!(line.contains("password"), "{line}");
        assert!(line.is_ascii(), "in-console strings stay ASCII: {line}");
    }

    /// The clients#415 shape: one line when the outage starts (naming the
    /// address and the sleeping-room remedy), silence, then a short reminder
    /// about once a minute for as long as it lasts.
    #[test]
    fn the_offline_notice_is_once_then_quiet_then_reminds_each_minute() {
        let mut policy = ReconnectPolicy::default();
        let start = Instant::now();

        let first = policy
            .notice(start, "archipelago.gg:12345")
            .expect("entering retry must say something");
        assert!(first.contains("archipelago.gg:12345"), "{first}");
        assert!(first.contains("retrying automatically"), "{first}");
        assert!(first.contains("open the room's page"), "{first}");
        assert!(first.is_ascii(), "in-console strings stay ASCII: {first}");

        for quiet in [1, 5, 30, 59] {
            assert_eq!(
                policy.notice(start + Duration::from_secs(quiet), "archipelago.gg:12345"),
                None,
                "second {quiet} must stay quiet"
            );
        }

        let reminder = policy
            .notice(start + Duration::from_secs(60), "archipelago.gg:12345")
            .expect("a quiet reminder is due after a minute offline");
        assert!(reminder.contains("Still offline"), "{reminder}");
        assert!(reminder.contains("archipelago.gg:12345"), "{reminder}");
        assert!(reminder.is_ascii(), "{reminder}");

        assert_eq!(
            policy.notice(start + Duration::from_secs(119), "archipelago.gg:12345"),
            None
        );
        assert!(
            policy
                .notice(start + Duration::from_secs(120), "archipelago.gg:12345")
                .is_some(),
            "the reminder cadence continues while the outage does"
        );
    }

    /// 5s, doubling, capped at 60s -- and no attempt before the first delay has
    /// actually elapsed.
    #[test]
    fn the_backoff_starts_at_five_seconds_doubles_and_caps_at_sixty() {
        let mut policy = ReconnectPolicy::default();
        let start = Instant::now();
        assert!(
            !policy.attempt_due(start),
            "no attempt is due before the connection is known to be down"
        );
        let _ = policy.notice(start, "server");

        let mut clock = start;
        for expected_wait in [5u64, 10, 20, 40, 60, 60] {
            assert!(
                !policy.attempt_due(clock + Duration::from_secs(expected_wait - 1)),
                "an attempt fired early at wait {expected_wait}"
            );
            clock += Duration::from_secs(expected_wait);
            assert!(
                policy.attempt_due(clock),
                "the attempt after {expected_wait}s did not fire"
            );
        }
    }

    /// Recovery is announced exactly once, by the `Event::Connected` arm. The
    /// policy contributes no second line, and a later outage is a fresh
    /// incident that announces itself again.
    #[test]
    fn recovery_prints_connected_once_and_the_policy_adds_nothing() {
        let mut policy = ReconnectPolicy::default();
        let start = Instant::now();
        assert!(policy.notice(start, "server").is_some());
        policy.connected();
        assert_eq!(CONNECTED_LINE, "Connected to Archipelago.");

        // Back online: quiet, and no attempt is scheduled.
        assert!(!policy.attempt_due(start + Duration::from_secs(600)));

        let again = policy.notice(start + Duration::from_secs(601), "server");
        assert!(
            again.is_some(),
            "a later outage is a new incident and announces itself again"
        );
    }

    /// A same-seed reconnect is ordinary: it must not refuse, so the runtime and
    /// its ledger cursor are never rebuilt and nothing re-delivers.
    #[test]
    fn a_same_seed_reconnect_is_accepted() {
        guard_seed_name("BB-12345", "BB-12345").expect("the same seed must reconnect cleanly");
    }

    /// The host regenerated the multiworld while we were offline. Continuing
    /// would deliver the new seed's items against the old seed's cursor, so the
    /// client refuses and names both seeds.
    #[test]
    fn a_different_seed_reconnect_refuses_and_names_both_seeds() {
        let error = guard_seed_name("BB-OLD-1", "BB-NEW-2").unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("BB-OLD-1"), "{rendered}");
        assert!(rendered.contains("BB-NEW-2"), "{rendered}");
        assert!(rendered.contains("Refusing to continue"), "{rendered}");
        assert!(rendered.is_ascii(), "{rendered}");
    }
}
