use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use archipelago_rs::{ClientStatus, Connection, ConnectionOptions, Event, ItemHandling};
use bb_archipelago::backend::{
    BloodborneBackend, EquipRequest, ItemGrant, LocationContext, MockBackend, OperationProgress,
    StackObservation,
};
use bb_archipelago::client_loop::{ClientLoop, IncomingItem, ItemPollResult, OperatorGrantPoll};
use bb_archipelago::config::RuntimeConfig;
use bb_archipelago::event_flags::is_manager_not_initialized;
use bb_archipelago::health::{HealthReporter, ReadinessState};
use bb_archipelago::ledger::{ReceiveLedger, VictoryRecord, WatermarkOutcome};
use bb_archipelago::logging;
use bb_archipelago::native::attach_wait::AttachWaitFailure;
use bb_archipelago::native::backend::NativeBackend;
use bb_archipelago::native::backend::ProbeOptions;
use bb_archipelago::toasts;
use bb_archipelago::{RUNTIME_BUILD, client_version};
use bb_archipelago::{client_debugln, client_eprintln};

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
}

const ITEM_ERROR_DEDUP: Duration = Duration::from_secs(10);
const ITEM_DELIVERY_RECOVERED: &str = "Bloodborne item delivery recovered.";
/// Give Bloodborne's inventory routine time to settle between release-flood
/// grants. A 2026-08-30 live capture stayed healthy for ordinary deliveries
/// but began routing items to storage after sustained 130-170 ms deltas.
const ITEM_DELIVERY_COOLDOWN: Duration = Duration::from_secs(1);

impl ItemErrorReporter {
    /// Returns the line to print for `error`, or `None` to stay quiet.
    fn report(&mut self, error: &anyhow::Error, now: Instant) -> Option<String> {
        let message = format!("{error:#}");
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

fn should_submit_goal(
    already_reported: bool,
    configured_goal: Option<i64>,
    newly_witnessed: &[i64],
) -> bool {
    !already_reported && configured_goal.is_some_and(|goal| newly_witnessed.contains(&goal))
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
    Mock(Box<MockBackend>),
    Native(Box<NativeBackend>),
}

impl BloodborneBackend for Backend {
    fn category8_generate(&mut self, gem_gen_param: u32) -> Result<String> {
        match self {
            Self::Mock(backend) => backend.category8_generate(gem_gen_param),
            Self::Native(backend) => backend.category8_generate(gem_gen_param),
        }
    }
    fn category8_insert(&mut self, variant: u8) -> Result<String> {
        match self {
            Self::Mock(backend) => backend.category8_insert(variant),
            Self::Native(backend) => backend.category8_insert(variant),
        }
    }
    fn record_presentation_marker(&mut self, note: &str) -> bool {
        match self {
            Self::Mock(backend) => backend.record_presentation_marker(note),
            Self::Native(backend) => backend.record_presentation_marker(note),
        }
    }
    fn record_location_checks(&mut self, locations: &[i64]) {
        match self {
            Self::Mock(backend) => backend.record_location_checks(locations),
            Self::Native(backend) => backend.record_location_checks(locations),
        }
    }
    fn location_context(&mut self) -> Result<Option<LocationContext>> {
        match self {
            Self::Mock(backend) => backend.location_context(),
            Self::Native(backend) => backend.location_context(),
        }
    }

    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        match self {
            Self::Mock(backend) => backend.read_event_flag(event_flag),
            Self::Native(backend) => backend.read_event_flag(event_flag),
        }
    }

    fn write_event_flag(&mut self, event_flag: u32, enabled: bool) -> Result<()> {
        match self {
            Self::Mock(backend) => backend.write_event_flag(event_flag, enabled),
            Self::Native(backend) => backend.write_event_flag(event_flag, enabled),
        }
    }

    fn target_weapon_level(&mut self) -> Result<Option<u8>> {
        match self {
            Self::Mock(backend) => backend.target_weapon_level(),
            Self::Native(backend) => backend.target_weapon_level(),
        }
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
        match self {
            Self::Mock(backend) => backend.grant_item(grant),
            Self::Native(backend) => backend.grant_item(grant),
        }
    }

    // clients#427: these two must be forwarded or the shipped binary silently
    // loses the native backend's live-stack baseline -- every dispatch in the
    // real client goes through this enum, and the tests exercise MockBackend
    // directly. They are required trait methods now, so a future addition that
    // misses this wrapper fails to compile instead of failing at a player's
    // house.
    fn observe_stack_quantity(
        &mut self,
        normalized_item_id: u32,
        reinforcement_level: Option<u8>,
    ) -> Result<StackObservation> {
        match self {
            Self::Mock(backend) => {
                backend.observe_stack_quantity(normalized_item_id, reinforcement_level)
            }
            Self::Native(backend) => {
                backend.observe_stack_quantity(normalized_item_id, reinforcement_level)
            }
        }
    }

    fn grant_may_have_applied(&mut self, tag: &str) -> Result<bool> {
        match self {
            Self::Mock(backend) => backend.grant_may_have_applied(tag),
            Self::Native(backend) => backend.grant_may_have_applied(tag),
        }
    }

    fn death_link_kill(&mut self) -> Result<bool> {
        match self {
            Self::Mock(backend) => backend.death_link_kill(),
            Self::Native(backend) => backend.death_link_kill(),
        }
    }

    fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress> {
        match self {
            Self::Mock(backend) => backend.equip_item(request),
            Self::Native(backend) => backend.equip_item(request),
        }
    }

    fn withdraw_unwitnessed_grant(&mut self, tag: &str) -> Result<bool> {
        match self {
            Self::Mock(backend) => backend.withdraw_unwitnessed_grant(tag),
            Self::Native(backend) => backend.withdraw_unwitnessed_grant(tag),
        }
    }

    fn retire_grant(&mut self, tag: &str, reason: &str) -> Result<bool> {
        match self {
            Self::Mock(backend) => backend.retire_grant(tag, reason),
            #[cfg(windows)]
            Self::Native(backend) => backend.retire_grant(tag, reason),
        }
    }

    // Forward the watermark hooks rather than inheriting the attested-mode
    // defaults, or mock mode could never exercise the watermark path.
    fn read_save_watermark(&mut self) -> Result<Option<u64>> {
        match self {
            Self::Mock(backend) => backend.read_save_watermark(),
            Self::Native(backend) => backend.read_save_watermark(),
        }
    }

    fn write_save_watermark(&mut self, cursor: u64) -> Result<bool> {
        match self {
            Self::Mock(backend) => backend.write_save_watermark(cursor),
            Self::Native(backend) => backend.write_save_watermark(cursor),
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
    /// True only when the user passed `--delivery=...` explicitly. Shapes the
    /// native attach-failure message; retained for `--delivery=native`
    /// compatibility while the backend selector itself is removed.
    delivery_explicit: bool,
    /// `--log-file <path>`: tee everything this client prints into that file as
    /// well as the console (clients#425). Absent means console only, exactly as
    /// before -- the launcher's generated plan supplies it, a hand-started
    /// client need not.
    log_file: Option<PathBuf>,
    /// Opacity of the Windows console, as a player-facing percentage. The
    /// translucent default lets the client sit over the game without hiding it;
    /// 100 restores the ordinary opaque console.
    window_opacity: u8,
    /// `--legacy-window`: run the original raw-Win32 shell instead of the egui
    /// window. Kept for one release as the fallback path while the new renderer
    /// collects live-session acceptance; it is not a supported configuration
    /// beyond that, and the flag goes when the Win32 shell does.
    legacy_window: bool,
}

const DEFAULT_WINDOW_OPACITY: u8 = 70;

fn parse_window_opacity(value: &str) -> Result<u8> {
    let percent: u8 = value
        .parse()
        .with_context(|| format!("invalid --window-opacity {value:?}; expected 35-100"))?;
    anyhow::ensure!(
        (35..=100).contains(&percent),
        "invalid --window-opacity {percent}; expected 35-100"
    );
    Ok(percent)
}

#[cfg(windows)]
fn apply_console_opacity(percent: u8) -> Result<()> {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongW, IsWindowVisible, LWA_ALPHA, SetLayeredWindowAttributes,
        SetWindowLongW, WS_EX_LAYERED,
    };

    if percent == 100 {
        return Ok(());
    }
    let window = unsafe { GetConsoleWindow() };
    anyhow::ensure!(
        !window.0.is_null(),
        "this client does not own a console window"
    );
    anyhow::ensure!(
        unsafe { IsWindowVisible(window) }.as_bool(),
        "the console is hosted by a terminal that controls its own opacity"
    );
    let style = unsafe { GetWindowLongW(window, GWL_EXSTYLE) };
    unsafe { SetWindowLongW(window, GWL_EXSTYLE, style | WS_EX_LAYERED.0 as i32) };
    let alpha = ((u16::from(percent) * 255 + 50) / 100) as u8;
    unsafe { SetLayeredWindowAttributes(window, COLORREF(0), alpha, LWA_ALPHA) }
        .context("setting client console opacity")
}

#[cfg(not(windows))]
fn apply_console_opacity(_percent: u8) -> Result<()> {
    Ok(())
}

/// Process-lifetime ownership of one receive ledger (clients#430).
///
/// The lock file may remain on disk, but the OS lock cannot go stale: the kernel releases it when
/// the process exits or crashes.  Holding the file handle in this guard keeps ownership until the
/// client finishes, and taking it before backend attachment prevents a losing second instance from
/// touching guest memory.
struct LedgerLock {
    _file: File,
}

impl LedgerLock {
    fn acquire(ledger: &Path) -> Result<Self> {
        let mut lock_name = ledger.as_os_str().to_os_string();
        lock_name.push(".lock");
        let lock_path = PathBuf::from(lock_name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| {
                format!(
                    "opening instance lock {} for receive ledger {}",
                    lock_path.display(),
                    ledger.display()
                )
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => bail!(
                "another bb-ap-client instance is already running for receive ledger {}. Close the other client before starting this one",
                ledger.display()
            ),
            Err(TryLockError::Error(error)) => Err(error).with_context(|| {
                format!(
                    "locking {} for receive ledger {}",
                    lock_path.display(),
                    ledger.display()
                )
            }),
        }
    }
}

/// The explicitly unsafe assumed-correct-save identity token. Set by
/// `--assume-correct-save`; consulted by the native path.
const ASSUMED_IDENTITY: &str = "unsafe-operator-attested-correct-save";

/// Actionable guidance shown when native delivery cannot validate the running
/// image. Unsupported builds fail closed before any item can be acknowledged.
const UNRECOGNIZED_BUILD_GUIDANCE: &str = "This game build was not recognized, so native item delivery cannot run safely. Delivery was not armed and no Archipelago item was acknowledged. Use the launcher's Open Logs & Diagnostics action and send the session bundle so native support can be added for this build.";

/// Map a native attach/validate failure onto the error the client exits with.
///
/// Both paths hard-fail -- native fails closed, so nothing was patched or
/// written. The default path (no explicit `--delivery`) wraps the failure with
/// [`UNRECOGNIZED_BUILD_GUIDANCE`] so an unrecognised build tells the player how
/// to gather diagnostics; an explicit `--delivery=native` propagates the raw
/// error because the user asked for native specifically.
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
/// against.
fn points_at_the_shad_log(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<AttachWaitFailure>()
            .is_some_and(AttachWaitFailure::is_stale_log_evidence)
    })
}

const LIVE_ATTACH_TIMEOUT: Duration = Duration::from_secs(600);

/// Attach the native backend, tolerating shadPS4 starting after the client:
/// retry only while the process is not
/// yet open, but fail fast on an image mismatch or any other refusal so a wrong
/// build never spins silently.
#[cfg(windows)]
fn attach_native_backend(
    shad_log: &Path,
    assumed_identity: Option<String>,
    health: &mut HealthReporter,
) -> Result<NativeBackend> {
    let deadline = Instant::now() + LIVE_ATTACH_TIMEOUT;
    let mut next_report = Instant::now();
    loop {
        health.readiness(ReadinessState::WaitingForProcess);
        let _ = health.publish(
            false,
            false,
            "Waiting for shadPS4; delivery is not armed yet",
        );
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
    _health: &mut HealthReporter,
) -> Result<NativeBackend> {
    bail!("native Bloodborne delivery requires Windows")
}

fn arguments() -> Result<Arguments> {
    parse_args(env::args().skip(1))
}

fn parse_args<I: Iterator<Item = String>>(mut args: I) -> Result<Arguments> {
    let Some(server) = args.next() else {
        bail!(
            "usage: bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD] [--mock] [--assume-correct-save] [--delivery=native] [--log-file PATH] [--window-opacity 35-100] [--legacy-window] (native delivery is required; client window opacity defaults to 70)"
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
    let mut delivery_explicit = false;
    let mut log_file = None;
    let mut window_opacity = DEFAULT_WINDOW_OPACITY;
    let mut legacy_window = false;
    while let Some(argument) = args.next() {
        if argument == "--legacy-window" {
            legacy_window = true;
        } else if argument == "--mock" {
            mock = true;
        } else if argument == "--assume-correct-save" {
            assume_correct_save = true;
        } else if let Some(mode) = argument.strip_prefix("--delivery=") {
            match mode {
                "native" => delivery_explicit = true,
                "ce-bridge" => bail!(
                    "--delivery=ce-bridge has been removed; Bloodborne now requires native delivery. Rebuild the launch plan with the current launcher."
                ),
                other => bail!("unknown --delivery mode {other:?}; expected native"),
            }
        } else if argument == "--log-file" {
            // Two-token form: what the launcher's generated plan emits
            // ("--log-file", "{client_log}"). A bare flag is refused rather
            // than swallowed as the optional PASSWORD positional, which would
            // send a path to the server as a password.
            let path = args.next().context("--log-file requires a path argument")?;
            log_file = Some(PathBuf::from(path));
        } else if let Some(path) = argument.strip_prefix("--log-file=") {
            log_file = Some(PathBuf::from(path));
        } else if argument == "--window-opacity" {
            let value = args
                .next()
                .context("--window-opacity requires a percentage from 35 to 100")?;
            window_opacity = parse_window_opacity(&value)?;
        } else if let Some(value) = argument.strip_prefix("--window-opacity=") {
            window_opacity = parse_window_opacity(value)?;
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
        delivery_explicit,
        log_file,
        window_opacity,
        legacy_window,
    })
}

/// The process entry point: run the client, and print a terminal error through
/// the tee rather than letting it escape (clients#437).
///
/// `main` used to return `Result<()>`, which handed a bubbled `Err` to Rust's
/// default termination handler. That handler prints `Error: {err:?}` onto the
/// process's real stderr -- and since clients#426 the client owns its own tee
/// and the launcher no longer pipes the child's stderr, so that line reached
/// neither `client.log` nor the launcher's early-exit dialog. A terminal
/// startup failure looked like a log that simply stopped mid-startup under an
/// "exit code 1" dialog with a healthy-looking tail (Badgerous, live: a
/// `verify_suppression_install` binder refusal whose reason evaporated).
///
/// So the run lives in [`run`] and the final print happens here, through
/// [`client_eprintln!`], in the same text the default handler produced. The
/// exit code stays 1.
///
/// No path in [`run`] both prints an error and returns it, so this never
/// doubles a line: the two waiting loops print progress while retrying and the
/// error they eventually return is a *different*, terminal one; the recoverable
/// lanes (`Grant bridge state unavailable at startup`, pending-command
/// reconciliation, park re-queue, location/item polling) print and carry on
/// without returning `Err` at all.
fn main() {
    match contract_check_request(env::args().skip(1)) {
        Ok(Some(code)) => std::process::exit(code),
        Ok(None) => {}
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(1);
        }
    }
    match version_request(env::args().skip(1)) {
        Ok(Some(version)) => {
            println!("{version}");
            return;
        }
        Ok(None) => {}
        Err(error) => {
            client_eprintln!("{}", logging::terminal_error_report(&error));
            std::process::exit(1);
        }
    }
    if let Err(error) = run() {
        client_eprintln!("{}", logging::terminal_error_report(&error));
        std::process::exit(1);
    }
}

/// `bb-ap-client --check-contract SLOT_DATA.json`: load a seed contract the
/// way a live session would and report what this build would do with it.
/// Exit 0 when every binding is deliverable, 2 when the contract loads but
/// some items would be parked (a world newer than this client), 1 when it
/// does not load at all. CI runs it against the contract the built apworld
/// emits so a world/client skew fails the release, not a player's launch.
fn contract_check_request<I: Iterator<Item = String>>(mut args: I) -> Result<Option<i32>> {
    let Some(first) = args.next() else {
        return Ok(None);
    };
    if first != "--check-contract" {
        return Ok(None);
    }
    let path = args
        .next()
        .context("--check-contract needs the slot_data JSON path")?;
    anyhow::ensure!(
        args.next().is_none(),
        "--check-contract does not accept any other arguments"
    );
    let slot_data: json::Value =
        json::from_slice(&std::fs::read(&path).with_context(|| format!("reading {path}"))?)
            .with_context(|| format!("parsing {path}"))?;
    Ok(Some(check_contract(&slot_data)))
}

fn check_contract(slot_data: &json::Value) -> i32 {
    let base: RuntimeConfig = match json::from_value(json::json!({})) {
        Ok(base) => base,
        Err(error) => {
            eprintln!("contract check: cannot build a base config: {error:#}");
            return 1;
        }
    };
    let config = match base.apply_slot_data(slot_data) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("contract check: REFUSED: {error:#}");
            return 1;
        }
    };
    let mut parked: Vec<(i64, String)> = config
        .items
        .iter()
        .filter(|(_, binding)| !binding.descriptor_evidence.is_known())
        .map(|(id, binding)| (*id, binding.descriptor_evidence.as_str().to_owned()))
        .collect();
    parked.sort();
    println!(
        "contract check: {} locations, {} items, {} category-8 awards, goal {:?}, sustain item {}",
        config.locations.len(),
        config.items.len(),
        config.category8_awards.len(),
        config.goal_location,
        if config.sustain_item.is_some() {
            "published"
        } else {
            "client default"
        }
    );
    if parked.is_empty() {
        println!("contract check: OK, every binding is deliverable by this client");
        0
    } else {
        for (id, evidence) in &parked {
            println!("contract check: AP item {id} would be PARKED ({evidence})");
        }
        eprintln!(
            "contract check: {} item(s) would be parked: this client is older than the world that emitted the contract",
            parked.len()
        );
        2
    }
}

fn version_request<I: Iterator<Item = String>>(mut args: I) -> Result<Option<String>> {
    let Some(first) = args.next() else {
        return Ok(None);
    };
    if first != "--version" {
        return Ok(None);
    }
    anyhow::ensure!(
        args.next().is_none(),
        "--version does not accept any other arguments"
    );
    Ok(Some(client_version()))
}

/// Resolve an Archipelago item id to the name the datapackage gives it.
///
/// The window's half of this rule is that it never sees an id at all: the renderer receives text.
/// See [`bb_archipelago::names`] for the formatting policy and its tests.
#[cfg(windows)]
fn item_label(game: &archipelago_rs::Game, ap_item_id: i64) -> String {
    let name = game.item(ap_item_id).map(|item| item.name());
    bb_archipelago::names::item_label(name.as_ref().map(|name| name.as_str()), ap_item_id)
}

#[cfg(windows)]
fn location_label(game: &archipelago_rs::Game, location_id: i64) -> String {
    game.location(location_id).map_or_else(
        || format!("location #{location_id}"),
        |location| location.name().to_string(),
    )
}
/// Which feed lane a server print belongs in.
///
/// Hints get their own lane because they are the one server print players actively hunt for in a
/// long feed; command results get the monospaced lane so they line up with the console. Everything
/// else is chat.
#[cfg(windows)]
fn print_activity_kind(print: &archipelago_rs::Print) -> client_ui::ActivityKind {
    match print {
        archipelago_rs::Print::Hint { .. } => client_ui::ActivityKind::Hint,
        archipelago_rs::Print::CommandResult { .. }
        | archipelago_rs::Print::AdminCommandResult { .. } => {
            client_ui::ActivityKind::CommandResult
        }
        _ => client_ui::ActivityKind::Message,
    }
}

#[cfg(windows)]
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic payload")
}

/// Keep the game client alive when the presentation-only egui renderer fails.
///
/// The renderer deliberately lives on another thread because the AP worker owns
/// the process main thread.  Discarding its join handle made a startup error or
/// panic completely silent: delivery carried on safely, but the player was left
/// with only the console.  Supervise that handle, tee the reason into the normal
/// session log, and hand the same renderer-neutral endpoint to the proven Win32
/// shell.  A normal window close still requests shutdown through `on_exit` and
/// must not reopen itself.
#[cfg(windows)]
fn spawn_supervised_gui(
    host: client_ui::HostEndpoint,
    options: standalone_windows::WindowOptions,
    window_state: PathBuf,
) {
    thread::spawn(move || {
        let egui = standalone_egui::spawn_persisted(host.clone(), options, window_state.clone());
        let failure = match egui.join() {
            Ok(Ok(())) => return,
            Ok(Err(error)) => format!("exited with an error: {error}"),
            Err(payload) => format!("panicked: {}", panic_payload_message(payload.as_ref())),
        };
        client_eprintln!(
            "WARNING: the default Bloodborne client window {failure}. Falling back to the legacy window; item delivery remains armed."
        );

        match standalone_windows::spawn_persisted(host, options, window_state).join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => client_eprintln!(
                "WARNING: the fallback Bloodborne client window exited with an error: {error}"
            ),
            Err(payload) => client_eprintln!(
                "WARNING: the fallback Bloodborne client window panicked: {}",
                panic_payload_message(payload.as_ref())
            ),
        }
    });
}

fn run() -> Result<()> {
    let args = arguments()?;
    // The tee is armed before ANY other output (clients#425), so the session log
    // carries the whole run rather than its tail. Everything below prints through
    // `client_eprintln!`, which reaches both the console the player is watching
    // and this file -- including the terminal error, which `main` prints from the
    // `Err` this function returns. Residual, accepted and unchanged: a failure
    // inside `arguments()` above happens before the path is known, so `main`
    // prints it to the console only.
    if let Some(path) = args.log_file.as_deref() {
        logging::install_log_file(path)
            .with_context(|| format!("could not open the client log {}", path.display()))?;
    }
    match apply_console_opacity(args.window_opacity) {
        Ok(()) if args.window_opacity < 100 => client_eprintln!(
            "Client console opacity: {}% (use --window-opacity 35-100 to adjust)",
            args.window_opacity
        ),
        Err(error) => client_eprintln!(
            "WARNING: could not make the client window {}% opaque: {error:#}",
            args.window_opacity
        ),
        Ok(()) => {}
    }
    let _ledger_lock = LedgerLock::acquire(&args.ledger)?;
    let mut health = HealthReporter::beside_ledger(&args.ledger);
    if let Err(error) = health.publish(false, false, "Starting; delivery is not armed yet") {
        client_eprintln!("WARNING: could not publish launcher health status: {error}");
    }
    #[cfg(windows)]
    let (ui_client, mut ui_reducer) = {
        let (client, host) = client_ui::UiBridge::new(16).split();
        let options = standalone_windows::WindowOptions {
            opacity: f32::from(args.window_opacity) / 100.0,
            ..Default::default()
        };
        let window_state = args
            .ledger
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("client-window.json");
        // The egui window is the default renderer; `--legacy-window` keeps the
        // original Win32 shell available for one release as a fallback. Both
        // consume the same `HostEndpoint` and emit the same `UiAction`s, so the
        // choice is a renderer choice and nothing more.
        if args.legacy_window {
            client_eprintln!(
                "Using the legacy Win32 client window (--legacy-window). This shell is scheduled for removal; report anything the default window does worse."
            );
            standalone_windows::spawn_persisted(host, options, window_state);
        } else {
            spawn_supervised_gui(host, options, window_state);
        }
        let mut reducer = client_ui::SnapshotReducer::default();
        client.publish(reducer.reduce(client_ui::DeliveryFacts {
            process: client_ui::ProcessState::Attaching,
            ap: client_ui::ApState::Connecting,
            delivery: client_ui::DeliveryState::NotArmed,
            server: Some(args.server.clone()),
            slot: Some(args.slot.clone()),
            ..Default::default()
        }));
        (client, reducer)
    };
    client_eprintln!(
        "Bloodborne AP client {} | runtime build {RUNTIME_BUILD}",
        client_version()
    );
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
        "native",
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
    config.apply_probe_env_overrides();
    if config.readiness_durations {
        health.enable_readiness_durations();
    }
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
    } else {
        let shad_log = config
            .shad_log
            .as_deref()
            .context("native delivery requires shad_log in the runtime config")?;
        if args.delivery_explicit {
            client_eprintln!(
                "Native delivery selected explicitly (--delivery=native). It fails closed on any image mismatch and will NOT fall back: a build it cannot validate is refused, not delivered through the bridge."
            );
        } else {
            client_eprintln!(
                "Native delivery is required. It fails closed on any image mismatch: a build it cannot validate is refused before delivery is armed."
            );
        }
        let assumed_identity = args
            .assume_correct_save
            .then(|| ASSUMED_IDENTITY.to_string());
        match attach_native_backend(shad_log, assumed_identity, &mut health) {
            Ok(mut backend) => {
                // clients#445: passive per-grant forensics beside the ledger.
                // Armed unconditionally on the native path -- it costs one
                // appended line per delivered item and it is the only way the
                // storage-routing question gets answered from ordinary play.
                backend.arm_delivery_diagnostics(
                    &args.ledger,
                    ProbeOptions {
                        pickup_notification: config.pickup_notification_probe,
                        boss_flags: config.boss_flag_census,
                        runes: config.rune_capture,
                        insight: config.insight_probe,
                    },
                );
                client_eprintln!(
                    "Bloodborne AP client {} | CUSA03173 01.09 | native payload installed | eboot 0x{:X} | native delivery armed",
                    client_version(),
                    backend.base()
                );
                Backend::Native(Box::new(backend))
            }
            // Native could not attach and validate this image -- an unknown
            // serial/build, a failed image assert, or another refusal. Fail
            // closed before delivery is armed or any AP item is acknowledged.
            Err(error) => return Err(native_attach_failure(error, args.delivery_explicit)),
        }
    };
    // Native/mock delivery is armed only by successful backend construction.
    let delivery_armed = true;
    if let Err(error) = health.publish(
        false,
        delivery_armed,
        "Delivery backend attached; connecting to AP",
    ) {
        client_eprintln!("WARNING: could not publish launcher health status: {error}");
    }
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
    let mut goal_name = None;
    let mut location_ids = HashSet::new();
    let mut location_regions = HashMap::new();
    let mut checked_location_count = 0_u32;
    let mut goal_reported = false;
    let run_started = Instant::now();
    let mut ap_detail_printed = false;
    let mut last_location_error: Option<(String, Instant)> = None;
    let mut item_errors = ItemErrorReporter::default();
    #[cfg(windows)]
    let mut placement_scouts: Option<toasts::PlacementScouts> = None;
    let mut death_link_tag_advertised = false;
    let mut pending_death_links: VecDeque<(String, Option<String>)> = VecDeque::new();
    let mut last_death_link_error: Option<String> = None;
    let mut health_write_warning_printed = false;

    // A deliberately small, offline-capable rescue surface. stdin lives on a
    // reader thread so a disconnected AP socket never makes the console hang.
    let (console_tx, console_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if console_tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    client_eprintln!(
        "Developer rescue console ready. Type 'help'. Mutations require an explicit CONFIRM token and reuse the validated delivery pipeline."
    );

    // A pending item that stays at the front of the queue past this long is
    // reported once, with its diagnosis, and shown as a stall instead of the
    // neutral "Working" pill.
    const STALL_AFTER: Duration = Duration::from_secs(60);
    let mut pending_since: Option<(u64, Instant)> = None;
    let mut stall_reported_for: Option<u64> = None;

    loop {
        #[cfg(windows)]
        let mut ui_delivery = if runtime.is_some() {
            client_ui::DeliveryState::WaitingForGameplay
        } else {
            client_ui::DeliveryState::NotArmed
        };
        // Why delivery is where it is, in the words the console already uses.
        // The window shows this instead of a bare `Blocked`, which told a player
        // nothing they could act on.
        #[cfg(windows)]
        let mut ui_delivery_detail: Option<String> = None;
        let mut command_lines = console_rx
            .try_iter()
            .map(|line| (line, false))
            .collect::<Vec<_>>();
        #[cfg(windows)]
        while let Ok(action) = ui_client.try_action() {
            match action {
                client_ui::UiAction::SubmitCommand(line) => command_lines.push((line, true)),
                client_ui::UiAction::RequestShutdown => return Ok(()),
                // Connection identity is owned by this process invocation. These actions become
                // active when the connection form lands; silently changing the current seed or
                // socket here would violate the ledger binding.
                client_ui::UiAction::OpenSessionFolder => {
                    if let Some(folder) = args.ledger.parent()
                        && let Err(error) =
                            std::process::Command::new("explorer").arg(folder).spawn()
                    {
                        client_eprintln!(
                            "Could not open session folder {}: {error}",
                            folder.display()
                        );
                    }
                }
                client_ui::UiAction::RetryBlocked { index } => {
                    let result = match runtime.as_mut() {
                        Some(runtime) => runtime.rescue_retry_blocked(index).map_or_else(
                            |error| format!("Rescue retry refused: {error:#}"),
                            |()| format!("AUDIT rescue retry index={index}: requeued through the normal ordered delivery pipeline."),
                        ),
                        None => "Rescue retry refused: runtime contract not loaded yet.".to_owned(),
                    };
                    client_eprintln!("{result}");
                    ui_reducer.activity(client_ui::ActivityKind::CommandResult, result);
                }
                client_ui::UiAction::Connect { .. } | client_ui::UiAction::Disconnect => {}
            }
        }
        for (line, from_ui) in command_lines {
            #[cfg(windows)]
            if from_ui {
                ui_reducer.activity(client_ui::ActivityKind::Command, format!("> {line}"));
            }
            let words = line.split_whitespace().collect::<Vec<_>>();
            let command = words.first().map(|word| word.to_ascii_lowercase());
            let result = match command.as_deref() {
                None | Some("") => continue,
                Some("help") => "Rescue commands: help | status | flag EVENT_FLAG | mark popup|modal|NOTE... | blocked | retry INDEX CONFIRM | export | setflag FLAG CONFIRM | give INDEX CONFIRM | census CONFIRM | rescue [NAME CONFIRM] (named repairs; run bare to list) | rebind CONFIRM (release the bound character while nothing is delivered). Unknown/unmapped writes and warps fail closed.".to_owned(),
                Some("status") => match runtime.as_mut() {
                    Some(runtime) => runtime
                        .rescue_status()
                        .unwrap_or_else(|error| format!("Rescue status unavailable: {error:#}")),
                    None => "Runtime contract not loaded yet; AP may be offline before the first successful connection.".to_owned(),
                },
                Some("flag") => match (runtime.as_mut(), words.get(1)) {
                    (Some(runtime), Some(raw)) => match raw.parse::<u32>() {
                        Ok(flag) => runtime
                            .rescue_read_flag(flag)
                            .unwrap_or_else(|error| format!("Rescue flag read refused: {error:#}")),
                        Err(_) => "Usage: flag EVENT_FLAG".to_owned(),
                    },
                    (None, _) => "Runtime contract not loaded yet.".to_owned(),
                    _ => "Usage: flag EVENT_FLAG".to_owned(),
                },
                Some("blocked") => runtime.as_ref().map_or_else(
                    || "Runtime contract not loaded yet.".to_owned(),
                    |runtime| {
                        runtime.rescue_list_blocked_with_names(|ap_item_id| {
                            connection.client().map_or_else(
                                || bb_archipelago::names::item_label(None, ap_item_id),
                                |client| item_label(client.this_game(), ap_item_id),
                            )
                        })
                    },
                ),
                Some("retry") => match (runtime.as_mut(), words.get(1), words.get(2)) {
                    (Some(runtime), Some(raw), Some(confirm))
                        if confirm.eq_ignore_ascii_case("CONFIRM") =>
                    {
                        match raw.parse::<u64>() {
                            Ok(index) => runtime.rescue_retry_blocked(index).map_or_else(
                                |error| format!("Rescue retry refused: {error:#}"),
                                |()| format!("AUDIT rescue retry index={index}: requeued through the normal ordered delivery pipeline."),
                            ),
                            Err(_) => "Usage: retry INDEX CONFIRM".to_owned(),
                        }
                    }
                    (None, _, _) => "Runtime contract not loaded yet.".to_owned(),
                    _ => "Usage: retry INDEX CONFIRM (inspect 'blocked' first; this is audited)".to_owned(),
                },
                Some("export") => match runtime.as_ref() {
                    Some(runtime) => runtime.rescue_export().map_or_else(
                        |error| format!("Diagnostic export failed: {error:#}"),
                        |path| format!("Exported rescue diagnostics to {}", path.display()),
                    ),
                    None => "Runtime contract not loaded yet.".to_owned(),
                },
                Some("setflag") => match (runtime.as_mut(), words.get(1), words.get(2)) {
                    (Some(runtime), Some(raw), Some(confirm))
                        if confirm.eq_ignore_ascii_case("CONFIRM") =>
                    {
                        match raw.parse::<u32>() {
                            Ok(flag) => runtime
                                .rescue_location_for_flag(flag)
                                .and_then(|location_id| {
                                    let name = connection.client().map_or_else(
                                        || format!("location #{location_id}"),
                                        |client| location_label(client.this_game(), location_id),
                                    );
                                    runtime.rescue_set_flag(flag, &name).map(|_| {
                                        format!("AUDIT rescue setflag flag={flag} ({name:?}): written. If this location was not legitimately reached, its check has now been sent anyway.")
                                    })
                                })
                                .unwrap_or_else(|error| format!("Rescue setflag refused: {error:#}")),
                            Err(_) => "Usage: setflag EVENT_FLAG CONFIRM".to_owned(),
                        }
                    }
                    (None, _, _) => "Runtime contract not loaded yet.".to_owned(),
                    _ => "Usage: setflag EVENT_FLAG CONFIRM (contract flags only; this sends the check)".to_owned(),
                },
                Some("give") => match (runtime.as_mut(), words.get(1), words.get(2)) {
                    (Some(runtime), Some(raw), Some(confirm))
                        if confirm.eq_ignore_ascii_case("CONFIRM") =>
                    {
                        match raw.parse::<i64>() {
                            Ok(index) => {
                                let name = connection.client().map_or_else(
                                    || bb_archipelago::names::item_label(None, index),
                                    |client| item_label(client.this_game(), index),
                                );
                                match runtime.rescue_give(index, &name) {
                                    Ok(true) => format!("AUDIT rescue give index={index} ({name:?}): queued through normal delivery."),
                                    Ok(false) => format!("AUDIT rescue give index={index} ({name:?}): already recorded; no second grant queued."),
                                    Err(error) => format!("Rescue give refused: {error:#}"),
                                }
                            }
                            Err(_) => "Usage: give ITEM_INDEX CONFIRM".to_owned(),
                        }
                    }
                    (None, _, _) => "Runtime contract not loaded yet.".to_owned(),
                    _ => "Usage: give ITEM_INDEX CONFIRM (contract items only)".to_owned(),
                },
                Some("census") => match (runtime.as_mut(), words.get(1)) {
                    (Some(runtime), Some(confirm)) if confirm.eq_ignore_ascii_case("CONFIRM") => {
                        let resolve_item = |ap_item_id: i64| {
                            connection.client().map_or_else(
                                || bb_archipelago::names::item_label(None, ap_item_id),
                                |client| item_label(client.this_game(), ap_item_id),
                            )
                        };
                        runtime.rescue_equipment_census(resolve_item).map_or_else(
                            |error| format!("Equipment census refused: {error:#}"),
                            |(queued, skipped)| format!(
                                "AUDIT equipment census: queued {queued} weapon/attire grants through the serial delivery lane; skipped {skipped} already recorded."
                            ),
                        )
                    }
                    (None, _) => "Runtime contract not loaded yet.".to_owned(),
                    _ => "Usage: census CONFIRM (queues every contract weapon and attire; throwaway save only)".to_owned(),
                },
                Some("rescue") => match (runtime.as_mut(), words.get(1), words.get(2)) {
                    (Some(runtime), Some(name), Some(confirm))
                        if confirm.eq_ignore_ascii_case("CONFIRM") =>
                    {
                        let resolve_item = |ap_item_id: i64| {
                            connection.client().map_or_else(
                                || bb_archipelago::names::item_label(None, ap_item_id),
                                |client| item_label(client.this_game(), ap_item_id),
                            )
                        };
                        let resolve_location = |location_id: i64| {
                            connection.client().map_or_else(
                                || format!("location #{location_id}"),
                                |client| location_label(client.this_game(), location_id),
                            )
                        };
                        match runtime.rescue_recipe(name, resolve_item, resolve_location) {
                            Ok(lines) => lines.join("\n"),
                            Err(error) => format!("Rescue recipe refused: {error:#}"),
                        }
                    }
                    (Some(_), Some(name), _) => format!(
                        "Usage: rescue {name} CONFIRM (this mutates the save and is audited; run 'rescue' alone to read what each recipe does first)"
                    ),
                    (Some(_), None, _) => format!(
                        "Rescue recipes (run 'rescue NAME CONFIRM'):\n{}",
                        bb_archipelago::client_loop::rescue_recipe_listing()
                    ),
                    (None, _, _) => "Runtime contract not loaded yet.".to_owned(),
                },
                Some("rebind") => match (runtime.as_mut(), words.get(1)) {
                    (Some(runtime), Some(confirm)) if confirm.eq_ignore_ascii_case("CONFIRM") => {
                        match runtime.rescue_rebind() {
                            Ok(message) => format!("AUDIT rescue rebind: {message}"),
                            Err(error) => format!("Rescue rebind refused: {error:#}"),
                        }
                    }
                    (None, _) => "Runtime contract not loaded yet.".to_owned(),
                    _ => "Usage: rebind CONFIRM (releases the bound character; only while nothing has been delivered)".to_owned(),
                },
                Some("item" | "warp") => "That mutation is unavailable: this build has no proven named mapping for it. Refusing instead of exposing arbitrary memory writes.".to_owned(),
                Some("mark") => match (runtime.as_mut(), words.get(1..)) {
                    (Some(runtime), Some(parts)) if !parts.is_empty() => {
                        let note = parts.join(" ");
                        if runtime.record_presentation_marker(&note) {
                            format!("Recorded presentation marker: {note}")
                        } else {
                            "Pickup-notification capture is not armed; marker was not recorded."
                                .to_owned()
                        }
                    }
                    (None, _) => "Runtime contract not loaded yet.".to_owned(),
                    _ => "Usage: mark popup | mark modal | mark NOTE...".to_owned(),
                },
                Some(other) => format!("Unknown rescue command {other:?}; type 'help'."),
            };
            client_eprintln!("{result}");
            #[cfg(windows)]
            if from_ui || result.starts_with("AUDIT ") {
                ui_reducer.activity(client_ui::ActivityKind::CommandResult, result);
            }
        }
        let mut connected_now = false;
        let mut ap_error_seen = false;
        for event in connection.update() {
            match event {
                Event::Connected => {
                    connected_now = true;
                    death_link_tag_advertised = false;
                    // The one recovery line, for the first connect and every
                    // reconnect alike (clients#423): `ReconnectPolicy` prints
                    // none of its own, so this is never doubled.
                    client_eprintln!("{CONNECTED_LINE}");
                    #[cfg(windows)]
                    ui_reducer.activity(client_ui::ActivityKind::Message, CONNECTED_LINE);
                }
                Event::Print(message) => {
                    #[cfg(windows)]
                    ui_reducer.activity(print_activity_kind(&message), message.to_string());
                    client_eprintln!("{message}");
                }
                Event::Error(error) => {
                    ap_error_seen = true;
                    #[cfg(windows)]
                    ui_reducer.activity(
                        client_ui::ActivityKind::Error,
                        format!("Archipelago error: {error}"),
                    );
                    client_eprintln!("Archipelago error: {error}");
                }
                Event::DeathLink { source, cause, .. } => {
                    pending_death_links.push_back((source, cause));
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
            if let Some(runtime) = runtime.as_mut() {
                // A fresh transport is the best opportunity to recover a
                // check that was written into the old zombie socket. Do not
                // carry that socket's retry delay across the reconnect.
                runtime.reset_location_retry_backoff();
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

        let ap_connected = connection.client().is_some();
        let health_detail = if ap_connected && delivery_armed {
            "Ready: AP connected and delivery armed"
        } else if ap_connected {
            "AP connected; delivery readiness is not proven"
        } else if connection.is_disconnected() {
            "AP disconnected; retrying automatically"
        } else {
            "Connecting to AP; delivery backend is armed"
        };
        let readiness = if runtime.is_none() {
            ReadinessState::Attaching
        } else if item_errors.last.is_some() {
            ReadinessState::Blocked
        } else if last_location_error
            .as_ref()
            .is_some_and(|(message, _)| message.contains("no validated gameplay/save identity"))
        {
            ReadinessState::SaveUnvalidated
        } else if !ap_connected {
            ReadinessState::GameplayGate
        } else {
            ReadinessState::DeliveryReady
        };
        health.readiness(readiness);
        if let Err(error) = health.publish(ap_connected, delivery_armed, health_detail)
            && !health_write_warning_printed
        {
            client_eprintln!(
                "WARNING: could not update launcher health status ({error}); gameplay and delivery are unaffected."
            );
            health_write_warning_printed = true;
        }

        if runtime.is_none()
            && let Some(client) = connection.client()
        {
            let seed_config = config.clone().apply_slot_data(client.slot_data())?;
            match seed_config.verify_suppression_install()? {
                Some(digest) => {
                    client_eprintln!(
                        "Verified installed vanilla-suppression binder SHA-256 {digest}."
                    )
                }
                None => {
                    client_eprintln!("Seed does not claim installed vanilla-award suppression.")
                }
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
            // clients: one line, once, at slot-data parse. Provenance is
            // bookkeeping -- these deliver like any other binding -- but the
            // operator should see the promotion surface.
            if let Some(notice) = seed_config.inferred_evidence_notice() {
                client_eprintln!("{notice}");
            }
            goal_location = seed_config.goal_location;
            goal_name = goal_location.map(|location| {
                client.this_game().location(location).map_or_else(
                    || format!("Location {location}"),
                    |location| location.name().to_string(),
                )
            });
            location_ids = seed_config
                .locations
                .iter()
                .map(|location| location.ap_location_id)
                .collect();
            location_regions = seed_config
                .locations
                .iter()
                .filter_map(|location| {
                    location
                        .region
                        .as_ref()
                        .map(|region| (location.ap_location_id, region.clone()))
                })
                .collect();
            #[cfg(windows)]
            {
                placement_scouts = Some(toasts::PlacementScouts::new(location_ids.iter().copied()));
            }
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
            // clients#427 + clients#433: two park causes are known to be
            // fixed, so those parks re-enter the delivery queue instead of
            // needing one manual bb-blocked invocation each --
            // `quantity_mismatch` (the precondition compared the stack against
            // the ledger's lifetime delivered sum; it is the observed live
            // quantity now) and `write_error (... quantity write failed)` (the
            // external write into the guest inventory page that shadPS4
            // refuses; existing-stack grants run on the game thread now). No
            // other park reason auto-unparks.
            match new_runtime.requeue_fixed_cause_parks() {
                Ok(indices) if !indices.is_empty() => eprintln!(
                    "Re-queued {} item(s) whose park cause is fixed (indices {}); \
                     they deliver against the observed inventory now.",
                    indices.len(),
                    indices
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Ok(_) => {}
                Err(error) => eprintln!("Re-queuing parked items failed: {error:#}"),
            }
            runtime = Some(new_runtime);
            // A durable summary is the local acknowledgement latch. Merely
            // seeing a historical server check on startup must not celebrate
            // or resubmit a stale/wrong save.
            goal_reported = runtime.as_ref().is_some_and(|runtime| {
                runtime
                    .victory()
                    .is_some_and(|record| Some(record.goal_location) == goal_location)
            });
            if goal_reported
                && let Some(runtime) = runtime.as_ref()
                && let Err(error) = runtime.write_victory_summary()
            {
                client_eprintln!(
                    "WARNING: completed goal is restored, but victory summary text could not be written: {error:#}"
                );
            }
        }

        if !death_link_tag_advertised
            && runtime.as_ref().is_some_and(ClientLoop::death_link_enabled)
            && let Some(client) = connection.client_mut()
        {
            client.update_connection(None, Some(["DeathLink"]))?;
            death_link_tag_advertised = true;
            client_eprintln!(
                "DeathLink receive is enabled; outbound Bloodborne deaths remain disabled pending live signal validation."
            );
        }

        if let (Some(runtime), Some((source, cause))) =
            (runtime.as_mut(), pending_death_links.front())
        {
            match runtime.receive_death_link() {
                Ok(true) => {
                    client_eprintln!(
                        "DeathLink received from {source}: {}",
                        cause.as_deref().unwrap_or("linked death")
                    );
                    pending_death_links.pop_front();
                    last_death_link_error = None;
                }
                Ok(false) => {}
                Err(error) => {
                    let message = format!("{error:#}");
                    if last_death_link_error.as_deref() != Some(&message) {
                        client_eprintln!("DeathLink kill unavailable: {message}");
                        last_death_link_error = Some(message);
                    }
                }
            }
        }

        if let (Some(runtime), Some(client)) = (runtime.as_mut(), connection.client_mut()) {
            #[cfg(windows)]
            if let Some(scouts) = placement_scouts.as_mut() {
                scouts.pump(client);
            }
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
                // clients#455: checked_locations() is optimistic --
                // mark_checked() updates it before the server echoes the
                // LocationChecks back. A write into a dead-but-undetected
                // socket therefore looked acknowledged forever. Derive from
                // the server-only view so a true game flag is resent until a
                // RoomUpdate confirms it; LocationChecks are idempotent.
                .server_checked_locations()
                .map(|location| location.id())
                .collect::<HashSet<_>>();
            checked_location_count = checked.intersection(&location_ids).count() as u32;
            match runtime.poll_locations(&checked) {
                Ok(newly_checked) => {
                    if last_location_error.take().is_some() {
                        client_eprintln!("Bloodborne location polling recovered.");
                    }
                    if !newly_checked.is_empty() {
                        #[cfg(windows)]
                        for &location_id in &newly_checked {
                            let fallback = client.this_game().location(location_id).map_or_else(
                                || format!("location #{location_id}"),
                                |location| location.name().to_owned(),
                            );
                            let line = placement_scouts.as_ref().map_or_else(
                                || format!("\u{2713} {fallback}"),
                                |scouts| scouts.sent_line(location_id, &fallback),
                            );
                            let class = placement_scouts
                                .as_ref()
                                .and_then(|scouts| scouts.placed_class(location_id));
                            ui_reducer.activity_with_class(
                                client_ui::ActivityKind::LocationCheck,
                                line,
                                class,
                            );
                        }
                        runtime.record_location_checks(&newly_checked);
                        if should_submit_goal(goal_reported, goal_location, &newly_checked) {
                            // Send the irreversible goal status before retiring
                            // the check locally. If this send fails, the next
                            // poll sees the flag as new and retries both.
                            client.set_status(ClientStatus::Goal)?;
                            goal_reported = true;
                            client_eprintln!(
                                "Witnessed configured Bloodborne goal; sent goal status."
                            );

                            let checks_after = checked_location_count.saturating_add(
                                newly_checked
                                    .iter()
                                    .filter(|location| {
                                        location_ids.contains(location)
                                            && !checked.contains(location)
                                    })
                                    .count() as u32,
                            );
                            let record = VictoryRecord {
                                goal_location: goal_location.expect("new goal check has a goal"),
                                goal_name: goal_name
                                    .clone()
                                    .unwrap_or_else(|| "Bloodborne goal".to_owned()),
                                completed_at_ms: SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .map_or(0, |elapsed| {
                                        elapsed.as_millis().min(u128::from(u64::MAX)) as u64
                                    }),
                                elapsed_seconds: Some(run_started.elapsed().as_secs()),
                                checks_completed: Some(checks_after),
                                checks_total: Some(location_ids.len() as u32),
                                received_items: u32::try_from(client.received_items().len()).ok(),
                                sent_items: Some(checks_after),
                                deaths: None,
                                death_links: None,
                            };
                            // Goal submission is authoritative and came first.
                            // Summary persistence/presentation can fail loudly,
                            // but can never roll back or block ClientStatus::Goal.
                            match runtime.record_victory(record) {
                                Ok(_) => match runtime.write_victory_summary() {
                                    Ok(path) => client_eprintln!(
                                        "Victory summary saved to {}.",
                                        path.display()
                                    ),
                                    Err(error) => client_eprintln!(
                                        "WARNING: goal sent, but victory summary text could not be written: {error:#}"
                                    ),
                                },
                                Err(error) => client_eprintln!(
                                    "WARNING: goal sent, but victory summary could not be persisted: {error:#}"
                                ),
                            }
                        }
                        let queued = runtime.queue_sustain_for_checks(&newly_checked)?;
                        if !queued.is_empty() {
                            client_debugln!("Queued pickup sustain for locations: {queued:?}");
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
            let operator_busy = match runtime.poll_operator_grant() {
                Ok(OperatorGrantPoll::Idle) => false,
                Ok(OperatorGrantPoll::Pending) => true,
                Ok(OperatorGrantPoll::Completed(ap_item_id)) => {
                    let line = format!(
                        "AUDIT rescue give index={ap_item_id} ({:?}): delivered through normal delivery.",
                        item_label(client.this_game(), ap_item_id)
                    );
                    client_eprintln!("{line}");
                    #[cfg(windows)]
                    ui_reducer.activity(client_ui::ActivityKind::CommandResult, line);
                    false
                }
                Err(error) => {
                    let line = format!("Rescue give delivery held: {error:#}");
                    client_eprintln!("{line}");
                    #[cfg(windows)]
                    ui_reducer.activity(client_ui::ActivityKind::CommandResult, line);
                    true
                }
            };
            let item_poll = if operator_busy {
                Ok(ItemPollResult::Pending)
            } else {
                runtime.poll_items(&received)
            };
            let items_idle = match item_poll {
                Ok(ItemPollResult::Completed(item)) => {
                    #[cfg(windows)]
                    {
                        ui_delivery = client_ui::DeliveryState::Ready;
                        let (name, sender, class) = client
                            .received_items()
                            .iter()
                            .find(|received| received.index() as u64 == item.index)
                            .map_or_else(
                                || {
                                    (
                                        item_label(client.this_game(), item.ap_item_id),
                                        "Archipelago".to_owned(),
                                        None,
                                    )
                                },
                                |received| {
                                    (
                                        received.item().name().to_owned(),
                                        received.sender().alias().to_owned(),
                                        Some(client_ui::ItemClass::from_flags(
                                            received.is_progression(),
                                            received.is_useful(),
                                            received.is_trap(),
                                        )),
                                    )
                                },
                            );
                        ui_reducer.activity_with_class(
                            client_ui::ActivityKind::ReceivedItem,
                            toasts::received_line(&name, &sender),
                            class,
                        );
                    }
                    if let Some(line) = item_errors.recovered() {
                        client_eprintln!("{line}");
                    }
                    client_debugln!(
                        "Acknowledged AP item index {} id {} | received level {:?} | target {:?} | delivered {:?} | equip {:?}.",
                        item.index,
                        item.ap_item_id,
                        item.received_level,
                        item.target_level,
                        item.delivered_level,
                        item.equip_target
                    );
                    thread::sleep(ITEM_DELIVERY_COOLDOWN);
                    false
                }
                Ok(ItemPollResult::Blocked(blocked)) => {
                    #[cfg(windows)]
                    {
                        // A park never holds up the queue; say so, and keep the
                        // stall state for an item that actually is not moving.
                        ui_delivery = client_ui::DeliveryState::Parked;
                        ui_delivery_detail = Some(format!(
                            "{} parked, latest {} ({}); type `blocked` to inspect",
                            runtime.parked_count(),
                            item_label(client.this_game(), blocked.ap_item_id),
                            blocked.status
                        ));
                        ui_reducer.activity(
                            client_ui::ActivityKind::ParkedDelivery,
                            format!(
                                "Parked {} - {} ({})",
                                item_label(client.this_game(), blocked.ap_item_id),
                                blocked.status,
                                blocked.detail
                            ),
                        );
                    }
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
                    false
                }
                Ok(ItemPollResult::Idle) => {
                    pending_since = None;
                    stall_reported_for = None;
                    #[cfg(windows)]
                    {
                        ui_delivery = client_ui::DeliveryState::Ready;
                    }
                    true
                }
                Ok(ItemPollResult::Pending) => {
                    let now = Instant::now();
                    let stalled = match (runtime.pending_index(), pending_since) {
                        (Some(index), Some((since_index, since))) if since_index == index => {
                            now.duration_since(since) >= STALL_AFTER
                        }
                        (Some(index), _) => {
                            pending_since = Some((index, now));
                            false
                        }
                        (None, _) => {
                            pending_since = None;
                            false
                        }
                    };
                    if stalled {
                        let diagnosis = runtime.pending_diagnosis().unwrap_or_else(|| {
                            "the item at the front of the queue has not moved".to_owned()
                        });
                        if stall_reported_for != runtime.pending_index() {
                            stall_reported_for = runtime.pending_index();
                            client_eprintln!("STALLED: {diagnosis}");
                        }
                        #[cfg(windows)]
                        {
                            ui_delivery = client_ui::DeliveryState::Blocked;
                            ui_delivery_detail = Some(diagnosis);
                        }
                    } else {
                        #[cfg(windows)]
                        {
                            ui_delivery = client_ui::DeliveryState::CommandPending;
                        }
                    }
                    false
                }
                // Held and Reconciled are surfaced through the watermark
                // notice channel above, exactly once per transition.
                Ok(ItemPollResult::Held | ItemPollResult::Reconciled(_)) => false,
                Err(error) => {
                    if let Some(line) = item_errors.report(&error, Instant::now()) {
                        client_eprintln!("{line}");
                    }
                    false
                }
            };
            if items_idle {
                match runtime.poll_sustain() {
                    Ok(bb_archipelago::client_loop::SustainPollResult::Completed(location)) => {
                        client_eprintln!(
                            "Pickup sustain delivered: 1 Quicksilver Bullet for location {location}."
                        );
                    }
                    Ok(_) => {}
                    Err(error) => client_eprintln!(
                        "Pickup sustain pending independently of AP item delivery: {error:#}"
                    ),
                }
            }
            if let Some(bb_archipelago::client_loop::SustainPollResult::Retired {
                location,
                command_withdrawn,
                reason,
            }) = runtime.take_sustain_notice()
            {
                client_eprintln!(
                    "Pickup sustain retired for location {location}: {reason}; native command withdrawn={command_withdrawn}. AP item delivery remains available."
                );
            }
        }

        #[cfg(windows)]
        {
            let ap = if connection.client().is_some() {
                client_ui::ApState::Authenticated
            } else if connection.is_disconnected() {
                client_ui::ApState::Reconnecting
            } else {
                client_ui::ApState::Connecting
            };
            let attached = runtime.is_some();
            let game = connection.client().map(|client| client.this_game());
            let unchecked_locations = connection.client().map_or_else(Vec::new, |client| {
                client
                    .unchecked_locations()
                    .filter(|location| location_ids.contains(&location.id()))
                    .map(|location| client_ui::UncheckedLocation {
                        name: location.name().to_string(),
                        region: location_regions.get(&location.id()).cloned(),
                    })
                    .collect()
            });
            let (seed, ledger, blocked, save_identity, gameplay_ready, receive_cursor) =
                runtime.as_mut().map_or_else(
                    || {
                        (
                            None,
                            client_ui::LedgerTotals::default(),
                            Vec::new(),
                            None,
                            false,
                            None,
                        )
                    },
                    |runtime| {
                        let totals = runtime
                            .ledger()
                            .slot(runtime.seed_name(), &args.slot)
                            .map_or_else(client_ui::LedgerTotals::default, |slot| {
                                let parked = slot.blocked_entries().count() as u32;
                                client_ui::LedgerTotals {
                                    queued: u32::from(slot.pending.is_some())
                                        + slot.redeliver.len() as u32,
                                    delivered: (slot.acknowledged.len() as u32)
                                        .saturating_sub(parked),
                                    storage_routed: None,
                                    parked,
                                }
                            });
                        let blocked = runtime
                            .rescue_blocked_entries()
                            .into_iter()
                            .map(|(index, ap_item_id, reason)| client_ui::BlockedEntry {
                                index,
                                item_name: game.map_or_else(
                                    || bb_archipelago::names::item_label(None, ap_item_id),
                                    |game| item_label(game, ap_item_id),
                                ),
                                reason,
                            })
                            .collect();
                        let context = runtime.rescue_context().ok().flatten();
                        let save_identity = context
                            .as_ref()
                            .map(|context| context.save_identity.clone());
                        let gameplay_ready = context.is_some_and(|context| context.gameplay_ready);
                        let receive_cursor = runtime
                            .ledger()
                            .slot(runtime.seed_name(), &args.slot)
                            .and_then(|slot| slot.highest_processed_index);
                        (
                            Some(runtime.seed_name().to_owned()),
                            totals,
                            blocked,
                            save_identity,
                            gameplay_ready,
                            receive_cursor,
                        )
                    },
                );
            if ledger.parked > 0 {
                ui_delivery = client_ui::DeliveryState::Blocked;
                ui_delivery_detail.get_or_insert_with(|| {
                    format!(
                        "{} item(s) parked; type `blocked` to inspect",
                        ledger.parked
                    )
                });
            }
            ui_client.publish(
                ui_reducer.reduce(client_ui::DeliveryFacts {
                    process: if attached {
                        client_ui::ProcessState::Attached
                    } else {
                        client_ui::ProcessState::Attaching
                    },
                    ap,
                    delivery: ui_delivery,
                    delivery_detail: ui_delivery_detail,
                    server: Some(args.server.clone()),
                    slot: Some(args.slot.clone()),
                    seed,
                    goal: goal_name.clone(),
                    victory: runtime
                        .as_ref()
                        .and_then(|runtime| runtime.victory())
                        .map(|record| client_ui::VictorySummary {
                            goal: record.goal_name.clone(),
                            completed_at_ms: record.completed_at_ms,
                            elapsed_seconds: record.elapsed_seconds,
                            checks_completed: record.checks_completed,
                            checks_total: record.checks_total,
                            received_items: record.received_items,
                            sent_items: record.sent_items,
                            deaths: record.deaths,
                            death_links: record.death_links,
                        }),
                    locations: (!location_ids.is_empty()).then_some(client_ui::LocationTotals {
                        checked: checked_location_count,
                        total: location_ids.len() as u32,
                    }),
                    unchecked_locations,
                    ledger,
                    blocked,
                    save_identity,
                    gameplay_ready,
                    receive_cursor,
                    ..Default::default()
                }),
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn gui_supervisor_preserves_text_panic_reasons() {
        let borrowed: Box<dyn std::any::Any + Send> = Box::new("borrowed reason");
        assert_eq!(panic_payload_message(borrowed.as_ref()), "borrowed reason");
        let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("owned reason"));
        assert_eq!(panic_payload_message(owned.as_ref()), "owned reason");
        let opaque: Box<dyn std::any::Any + Send> = Box::new(7_u8);
        assert_eq!(
            panic_payload_message(opaque.as_ref()),
            "unknown panic payload"
        );
    }

    #[test]
    fn every_goal_submits_only_on_its_new_positioned_witness() {
        for goal in [12_259_361, 12_259_362, 12_259_363] {
            assert!(!should_submit_goal(false, Some(goal), &[]));
            assert!(!should_submit_goal(false, Some(goal), &[goal + 10]));
            assert!(should_submit_goal(false, Some(goal), &[goal]));
            assert!(!should_submit_goal(true, Some(goal), &[goal]));
        }
        assert!(!should_submit_goal(false, None, &[12_259_363]));
    }

    /// clients#427 motivating case: the shipped client dispatches through the
    /// `Backend` enum, not through a bare `MockBackend`. Before this fix the
    /// enum had no `observe_stack_quantity` arm, so the trait default fired and
    /// every fresh grant fell back to the ledger-sum baseline -- which is why
    /// the clients#428 build still parked with a climbing `expected_before`
    /// against an actual stack of 2. Asserting through the enum is the only
    /// way to witness that.
    #[test]
    fn enum_backend_forwards_observe_stack_quantity_to_the_inner_backend() {
        let mut mock = MockBackend::default();
        mock.inventory.insert((0x4000_0000, None), 7);
        let mut backend = Backend::Mock(Box::new(mock));

        let observed = backend
            .observe_stack_quantity(0x4000_0000, None)
            .expect("observing through the enum must not error");
        assert_eq!(
            observed,
            StackObservation::Quantity(7),
            "the enum wrapper must reach the inner backend, not the trait default"
        );
    }

    /// The clients#427 follow-up half of the same trap: a retained-but-
    /// unwitnessed command's baseline is not binding, and the enum must say so
    /// instead of inheriting the conservative `true`.
    #[test]
    fn enum_backend_forwards_grant_may_have_applied_to_the_inner_backend() {
        let mut mock = MockBackend::default();
        mock.retained_unwitnessed.insert("ap_7".to_string());
        let mut backend = Backend::Mock(Box::new(mock));

        let retained = backend
            .grant_may_have_applied("ap_7")
            .expect("asking through the enum must not error");
        assert!(
            !retained,
            "a retained, unwitnessed command cannot have applied"
        );

        let untracked = backend
            .grant_may_have_applied("ap_8")
            .expect("asking through the enum must not error");
        assert!(untracked, "an unretained command may have applied");
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
    fn a_different_error_is_reported_immediately() {
        let mut reporter = ItemErrorReporter::default();
        let start = Instant::now();
        assert!(reporter.report(&anyhow::anyhow!("first"), start).is_some());
        let other = anyhow::anyhow!("native grant protocol mismatch");
        let line = reporter
            .report(&other, start + Duration::from_secs(1))
            .expect("an unrelated failure is not silenced");
        assert!(line.contains("protocol mismatch"), "{line}");
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
        assert!(!args.delivery_explicit);
    }

    #[test]
    fn removed_ce_bridge_has_actionable_migration_error() {
        let error = parse_args(base_args(&["--delivery=ce-bridge"]).into_iter()).unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("has been removed"), "{rendered}");
        assert!(rendered.contains("native delivery"), "{rendered}");
    }

    #[test]
    fn explicit_native_selects_native_and_is_explicit() {
        let args = parse_args(base_args(&["--delivery=native"]).into_iter()).expect("parse");
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
        let args = parse_args(base_args(&["--log-file=client.log"]).into_iter()).expect("parse");
        assert_eq!(args.log_file.as_deref(), Some(Path::new("client.log")));
    }

    /// Without the flag nothing about logging changes: no path, and therefore
    /// no file is ever opened.
    #[test]
    fn no_log_file_by_default() {
        let args = parse_args(base_args(&[]).into_iter()).expect("parse");
        assert!(args.log_file.is_none());
    }

    #[test]
    fn client_window_is_translucent_by_default() {
        let args = parse_args(base_args(&[]).into_iter()).expect("parse");
        assert_eq!(args.window_opacity, 70);
    }

    /// The fallback shell has to stay reachable by flag until a live Windows session has accepted
    /// the egui window; a typo'd flag that silently parsed as the PASSWORD positional would send
    /// "--legacy-window" to the server as a password.
    #[test]
    fn the_legacy_window_is_opt_in_and_is_not_mistaken_for_a_password() {
        let default = parse_args(
            ["server", "slot", "config", "ledger"]
                .into_iter()
                .map(String::from),
        )
        .unwrap();
        assert!(!default.legacy_window);
        assert_eq!(default.password, None);

        let legacy = parse_args(
            ["server", "slot", "config", "ledger", "--legacy-window"]
                .into_iter()
                .map(String::from),
        )
        .unwrap();
        assert!(legacy.legacy_window);
        assert_eq!(legacy.password, None);
    }

    #[test]
    fn window_opacity_accepts_separate_and_joined_forms() {
        let separate = parse_args(base_args(&["--window-opacity", "70"]).into_iter())
            .expect("parse separate form");
        let joined = parse_args(base_args(&["--window-opacity=100"]).into_iter())
            .expect("parse joined form");
        assert_eq!(separate.window_opacity, 70);
        assert_eq!(joined.window_opacity, 100);
    }

    #[test]
    fn window_opacity_refuses_invisible_or_invalid_values() {
        for value in ["0", "34", "101", "mist"] {
            let error = parse_args(base_args(&["--window-opacity", value]).into_iter())
                .expect_err("unsafe opacity must fail");
            assert!(
                format!("{error:#}").contains("--window-opacity"),
                "{error:#}"
            );
        }
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
    fn contract_check_reports_deliverable_parked_and_refused_contracts() {
        let clean = json::json!({
            "runtime_locations": {"12259363": {"event_flag": 52410800, "vanilla_award_suppressed": false}},
            "runtime_items": {"12255488": {"raw_descriptor": 0xB000_03E8_u32, "normalized_item_id": 0x4000_03E8_u32,
                "item_category": 4, "descriptor_evidence": "goods_formula_observed", "quantity": 1,
                "feed_effect": "not_equippable"}},
            "goal_location": 12259363
        });
        assert_eq!(check_contract(&clean), 0);

        let skewed = json::json!({
            // Protector 292000 has no EquipParamProtector row and is refused by
            // the reviewed allowlist on purpose, so it stays a parked example.
            "runtime_items": {"12255740": {"raw_descriptor": 0x9004_7498_u32, "normalized_item_id": 0x1004_7498_u32,
                "item_category": 1, "descriptor_evidence": "param_id_inferred", "quantity": 1,
                "feed_effect": "attire_hands"}}
        });
        assert_eq!(check_contract(&skewed), 2);

        let broken = json::json!({"runtime_items": {"not-an-id": {}}});
        assert_eq!(check_contract(&broken), 1);

        let dir = std::env::temp_dir().join(format!("bb-contract-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slot.json");
        std::fs::write(&path, clean.to_string()).unwrap();
        let code = contract_check_request(
            [
                "--check-contract".to_string(),
                path.to_string_lossy().into_owned(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(code, Some(0));
        assert!(
            contract_check_request(["server".to_string()].into_iter())
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn version_flag_prints_the_exact_binary_version_without_runtime_arguments() {
        let version = version_request(["--version".to_string()].into_iter())
            .expect("version request")
            .expect("version line");
        assert_eq!(version, client_version());
    }

    #[test]
    fn version_flag_refuses_ambiguous_trailing_arguments() {
        let error = version_request(["--version".to_string(), "server".to_string()].into_iter())
            .unwrap_err();
        assert!(format!("{error:#}").contains("does not accept"));
    }

    #[test]
    fn receive_ledger_has_exactly_one_live_process_owner() {
        let ledger =
            env::temp_dir().join(format!("bb-ledger-lock-test-{}.json", std::process::id()));
        let lock_path = PathBuf::from(format!("{}.lock", ledger.display()));
        let first = LedgerLock::acquire(&ledger).expect("first owner");
        let error = LedgerLock::acquire(&ledger)
            .err()
            .expect("second owner must be refused");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("another bb-ap-client instance"),
            "{rendered}"
        );
        assert!(
            rendered.contains(&ledger.display().to_string()),
            "{rendered}"
        );

        drop(first);
        let replacement =
            LedgerLock::acquire(&ledger).expect("a crashed/exited owner cannot leave a stale lock");
        drop(replacement);
        let _ = std::fs::remove_file(lock_path);
    }

    // Native attachment needs a live shadPS4 process and `#[cfg(windows)]`
    // seams. What is host-testable is the fail-closed error policy.
    #[test]
    fn default_native_failure_hard_fails_with_guidance_not_fallback() {
        let error = native_attach_failure(anyhow::anyhow!("image assert: unknown build"), false);
        let rendered = format!("{error:#}");
        assert!(rendered.contains("was not recognized"), "{rendered}");
        assert!(rendered.contains("Open Logs & Diagnostics"), "{rendered}");
        assert!(!rendered.contains("Cheat Engine"), "{rendered}");
        assert!(!rendered.contains("ce-bridge"), "{rendered}");
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
    /// the suspect, so build guidance must NOT be appended -- the message
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
    /// class must never carry unrecognised-build guidance.
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
    /// IS an unrecognised build, so native diagnostic guidance still applies.
    #[test]
    fn a_rejected_image_after_the_wait_still_gets_the_build_guidance() {
        let failure = AttachWaitFailure::ImageRejected {
            base: 0x5570000,
            detail: String::from("assert consume_hook mismatched"),
        };
        let error = native_attach_failure(anyhow::Error::new(failure), false);
        let rendered = format!("{error:#}");
        assert!(rendered.contains("was not recognized"), "{rendered}");
        assert!(rendered.contains("Open Logs & Diagnostics"), "{rendered}");
        assert!(!rendered.contains("ce-bridge"), "{rendered}");
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
