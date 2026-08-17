use std::collections::VecDeque;
use std::time::{Duration, Instant};
use std::{iter::ExactSizeIterator, mem};

use anyhow::{Error, Result, bail};
use archipelago_rs as ap;
use log::*;
use serde::de::DeserializeOwned;
use ustr::Ustr;

use crate::{Game, SectionProfiler, config::Config};

/// The maximum number of log messages to store.
const LOG_BUFFER_LIMIT: usize = 1000;

/// The grace period between MapItemMan starting to exist and the mod beginning
/// to take actions.
const GRACE_PERIOD: Duration = Duration::from_secs(10);

/// The base struct for implementations of [Core].
pub struct CoreBase<G: Game, S: DeserializeOwned + Send + 'static> {
    /// The name of the game that's being played.
    game: Ustr,

    /// The configuration for the current Archipelago connection. This is not
    /// guaranteed to be complete *or* accurate; it's the mod's responsibility
    /// to ensure it makes sense before actually interacting with an individual
    /// game.
    config: Config<G>,

    /// The Archipelago client connection. `None` when there's no connection info yet (empty URL or
    /// slot), which keeps us from attempting a doomed connection to an empty URL.
    connection: Option<ap::Connection<S>>,

    /// The log of prints that can be displayed in the overlay, along with the
    /// times they were received.
    log_buffer: VecDeque<(ap::Print, Instant)>,

    /// Events we're waiting to process until the player loads a save. This is
    /// always empty unless a connection is connected and the player is on the
    /// main menu (or in the initial waiting period during a load).
    event_buffer: Vec<ap::Event>,

    /// The time at which we noticed the game loading (as indicated by
    /// MapItemMan coming into existence). Used to compute the grace period
    /// before we start doing stuff in game. None if the game is not currently
    /// loaded.
    load_time: Option<Instant>,

    /// The fatal error that this has encountered, if any. If this is not
    /// `None`, most in-game processing will be disabled.
    error: Option<Error>,

    /// A profiler that can be used to track how long various sections of the
    /// mod take to run.
    profiler: SectionProfiler,

    /// Whether the server currently believes this slot carries the `DeathLink` tag.
    ///
    /// We connect UNTAGGED, so this starts `false` on every new connection and only becomes `true`
    /// after a successful [`Core::reconcile_death_link_tag`] send. See [`next_tag_advertisement`].
    advertised_death_link: bool,

    /// CHAT-RELAY (2026-07-02): commands queued from server chat ("@<slot> <cmd> [args]"),
    /// dispatched through the game core's `handle_command` by [Core::update]. Exists because a
    /// game whose InputBlocker hasn't been RE'd (ER) can't take keyboard focus in the overlay
    /// in-world, so the say-console commands are otherwise unreachable -- any text client on
    /// the session (or the server console) becomes the keyboard instead.
    pending_chat_commands: Vec<(String, Option<String>)>,
}

impl<G: Game, S: DeserializeOwned + Send + 'static> CoreBase<G, S> {
    /// Creates a new instance of [CoreBase].
    pub fn new(game: impl Into<Ustr>) -> Result<Self> {
        let game = game.into();
        let config = Config::load()?;
        // Install BEFORE anything can ask `probes::enabled`, so no probe can read an empty map and
        // conclude it is off. This is the only call site.
        crate::probes::install(config.probes().clone());
        let connection = Self::new_connection(game, &config);

        Ok(Self {
            game,
            config,
            connection,
            log_buffer: Default::default(),
            event_buffer: vec![],
            load_time: None,
            error: None,
            advertised_death_link: false,
            profiler: Default::default(),
            pending_chat_commands: vec![],
        })
    }

    /// Creates a new [ClientConnection] based on the connection information in [config], or `None` if
    /// there isn't enough info to connect yet.
    fn new_connection(game: Ustr, config: &Config<G>) -> Option<ap::Connection<S>> {
        // Don't attempt to connect until we have both a server URL and a slot. Otherwise
        // archipelago_rs tries to open an empty/incomplete URL and surfaces a confusing
        // "HTTP format error: empty string". `None` lets the overlay prompt for the details and
        // connect cleanly once they're entered.
        // `is_connectable` also rejects a url whose PORT is not a number -- the shipped
        // apconfig.json default is `archipelago.gg:PORT`, a deliberate placeholder, and without
        // this it would reach tungstenite as `wss://archipelago.gg:PORT` and produce exactly the
        // confusing parser-error loop described above rather than the connect form.
        if !crate::config::is_connectable(config.url()) || config.slot().is_empty() {
            return None;
        }

        // NO TAGS AT CONNECT -- deliberately.
        //
        // This used to be `.tags(vec!["DeathLink"])`, unconditionally, for every game. The server
        // therefore routed every other player's death to this slot whatever the slot's own
        // `death_link` option said (observed twice in `archipelago-2026-08-01.log` on a slot with
        // the option OFF), and it did so for Sekiro too, which has no DeathLink implementation at
        // all -- those deaths were delivered and dropped on the floor.
        //
        // It cannot be fixed here: the tag set is decided BEFORE the socket opens, and
        // `death_link` lives in SLOT DATA, which does not exist until after the server accepts the
        // connection. So we connect advertising nothing and add the tag afterwards, once the
        // answer is actually known -- see [`Core::reconcile_death_link_tag`].
        //
        // Untagged is the FAIL-CLOSED direction. If the option is never resolved (slot data never
        // parses, the update send fails), a slot with DeathLink on quietly misses deaths, which is
        // this client's problem alone. The other order -- advertise, then retract -- fails by
        // consuming other players' deaths during the window, which is everyone's problem.
        let mut options =
            ap::ConnectionOptions::new().receive_items(ap::ItemHandling::OtherWorlds {
                own_world: G::OWN_WORLD,
                starting_inventory: true,
            });
        if let Some(password) = config.password() {
            options = options.password(password);
        }

        Some(ap::Connection::new(
            config.url(),
            config.slot(),
            Some(game),
            options,
        ))
    }

    /// The section profiler.
    pub fn profiler(&mut self) -> &mut SectionProfiler {
        &mut self.profiler
    }

    /// Returns the current connection type.
    pub(crate) fn connection_state_type(&self) -> ap::ConnectionStateType {
        self.connection
            .as_ref()
            .map_or(ap::ConnectionStateType::Disconnected, |c| c.state_type())
    }

    /// Returns whether the current connection is disconnected.
    pub(crate) fn is_disconnected(&self) -> bool {
        self.connection.as_ref().is_none_or(|c| c.is_disconnected())
    }

    /// Retries the Archipelago connection with the same information.
    pub(crate) fn reconnect(&mut self) {
        if self.connection_state_type() == ap::ConnectionStateType::Disconnected {
            self.log("Reconnecting...");
        }

        self.connection = Self::new_connection(self.game, &self.config);
        self.advertised_death_link = false; // a fresh socket carries no tags
    }

    /// Updates the full set of connection info (server URL, slot, and optional
    /// password), saves the config, and reconnects. Used by the in-game connect
    /// overlay so a fresh install can be configured without a pre-existing
    /// apconfig.json.
    /// (url, slot, password) as currently configured -- lets the ER config watcher seed itself with
    /// what we actually connected with, so its first tick cannot fire a spurious reconnect.
    pub fn config_snapshot(&self) -> (String, String, Option<String>) {
        (
            self.config.url().to_string(),
            self.config.slot().to_string(),
            self.config.password().map(|s| s.to_string()),
        )
    }

    /// Also used by the ER config hot-reload watcher (eldenring-archipelago::config_watch), so a
    /// tester can change server/slot by editing apconfig.json instead of fighting the game for input
    /// (ER has no InputBlocker, so the overlay cannot take focus cleanly).
    pub fn update_connection_info(
        &mut self,
        url: impl AsRef<str>,
        slot: impl AsRef<str>,
        password: Option<String>,
    ) -> Result<()> {
        if self.connection_state_type() == ap::ConnectionStateType::Disconnected {
            self.log("Connecting...");
        }

        self.config.set_url(url);
        self.config.set_slot(slot);
        self.config.set_password(password);
        self.config.save()?;
        self.connection = Self::new_connection(self.game, &self.config);
        self.advertised_death_link = false; // a fresh socket carries no tags
        Ok(())
    }

    /// Returns whether the config has the minimum info needed to connect (a
    /// non-empty server URL and slot). Drives the overlay's first-run connect
    /// prompt.
    pub(crate) fn is_configured(&self) -> bool {
        !self.config.url().is_empty() && !self.config.slot().is_empty()
    }

    /// If this client has encountered a fatal error, takes ownership of it.
    pub(crate) fn take_error(&mut self) -> Option<Error> {
        if let Some(err) = self.error.take() {
            self.error = Some(ap::Error::Elsewhere.into());
            Some(err)
        } else {
            None
        }
    }

    /// Returns the current user config.
    pub(crate) fn config(&self) -> &Config<G> {
        &self.config
    }

    /// Returns the list of all logs that have been emitted in the current
    /// session.
    ///
    /// Public so game crates can scan the print stream (e.g. the ER item
    /// tracker accumulates `Print::Hint`s from it). NOTE for such scanners:
    /// this is a bounded ring ([LOG_BUFFER_LIMIT]) — once full, old entries
    /// pop off the front and indices shift.
    pub fn logs(&self) -> impl ExactSizeIterator<Item = &(ap::Print, Instant)> {
        self.log_buffer.iter()
    }

    /// Updates the Archipelago connection, adds any events that need processing
    /// to [event_buffer].
    ///
    /// This is always run regardless of whether the client is connected or the
    /// mod has experienced a fatal error.
    fn update_always(&mut self) {
        use ap::Event::*;
        let (mut state, mut events) = match self.connection.as_mut() {
            Some(conn) => (conn.state_type(), conn.update()),
            None => return,
        };

        // Process events that should happen even when the player isn't in an
        // active save.
        for event in events.extract_if(.., |e| matches!(e, Connected | Error(_) | Print(_))) {
            match event {
                Connected => {
                    state = ap::ConnectionStateType::Connected;
                }
                Error(err) if err.is_fatal() => {
                    let err = self.connection.as_ref().map(|c| c.err());
                    self.log(
                        // client#181: the two socket-level kinds are OPPOSITE diagnoses and
                        // used to share one sentence -- one that pointed at the URL, which this
                        // arm has already excluded (the name resolved; the slot details were
                        // never sent). See `connect_error` for the four rounds of triage that
                        // cost.
                        if let Some(ap::Error::WebSocket(tungstenite::Error::Io(io))) = err
                            && let Some(failure) = crate::connect_error::classify(io.kind())
                        {
                            vec![
                                ap::RichText::Color {
                                    text: failure.headline().into(),
                                    color: ap::TextColor::Red,
                                },
                                failure.advice().into(),
                            ]
                        } else if state == ap::ConnectionStateType::Connected {
                            vec![
                                ap::RichText::Color {
                                    text: "Connection failed: ".into(),
                                    color: ap::TextColor::Red,
                                },
                                err.map(|e| e.to_string()).unwrap_or_default().into(),
                            ]
                        } else {
                            vec![
                                ap::RichText::Color {
                                    text: "Disconnected: ".into(),
                                    color: ap::TextColor::Red,
                                },
                                err.map(|e| e.to_string()).unwrap_or_default().into(),
                            ]
                        },
                    );
                    self.event_buffer.clear();
                }
                Error(err) => self.log(err.to_string()),
                Print(print) => {
                    // CHAT-RELAY: "@<slot> <cmd> [args]" in plain chat (NOT "!"/"/" -- the
                    // server doesn't relay "!" commands and "/" is client-local) queues <cmd>
                    // for the game core's say-console handle_command. Slot-addressed so one
                    // client in a multi-FromSoft session executes it, self included (our own
                    // say is echoed back by the server, so the overlay say box also works).
                    if let ap::Print::Chat { message, .. } | ap::Print::ServerChat { message, .. } =
                        &print
                    {
                        let slot = self.config.slot();
                        if !slot.is_empty()
                            && let Some(rest) = message
                                .strip_prefix('@')
                                .and_then(|m| m.strip_prefix(slot))
                                .and_then(|m| m.strip_prefix(' '))
                        {
                            let mut parts = rest.trim().splitn(2, ' ');
                            if let Some(cmd) = parts.next().filter(|c| !c.is_empty()) {
                                let arg = parts.next().map(|s| s.trim().to_string());
                                info!("chat-relay: queued command !{cmd} (arg {arg:?})");
                                self.pending_chat_commands.push((format!("!{cmd}"), arg));
                            }
                        }
                    }
                    let is_compression_warning = format!("{print}")
                        .to_lowercase()
                        .contains("compressed websocket");
                    info!("[APS] {print}");
                    if self.log_buffer.len() >= LOG_BUFFER_LIMIT {
                        self.log_buffer.pop_front();
                    }
                    self.log_buffer.push_back((print, Instant::now()));
                    // The AP server warns once that the client lacks compressed-websocket
                    // support. No Rust AP lib supports permessage-deflate (tungstenite-rs#2);
                    // it is purely cosmetic and does NOT affect Elden Ring AP. Reassure the
                    // player so the red server warning does not read as a real error.
                    if is_compression_warning {
                        self.log(
                            "Note: the \"compressed websocket\" server warning above is harmless \
                             -- Elden Ring Archipelago works normally without it."
                                .to_string(),
                        );
                    }
                }
                _ => {}
            }
        }

        if state == ap::ConnectionStateType::Connected {
            self.event_buffer.extend(events);
        } else {
            debug_assert!(self.event_buffer.is_empty());
        }
    }

    /// Returns an error if the user's static randomizer version doesn't match
    /// this mod's version.
    fn check_version_conflict(&self, expected_version: &str) -> Result<()> {
        if let Some(client_version) = self.config().client_version()
            && client_version != expected_version
        {
            bail!(
                "Your apconfig.json was generated using static randomizer v{}, but this client is \
                 v{}. Re-run the static randomizer with the current version.",
                client_version,
                expected_version,
            );
        } else {
            Ok(())
        }
    }

    /// Writes a message to the log buffer that we display to the user in the
    /// overlay, as well as to the internal logger.
    fn log(&mut self, message: impl Into<ap::Print>) {
        let print = message.into();
        info!("[APC] {print}");
        // Consider making this a circular buffer if it ends up eating too much
        // memory over time.
        if self.log_buffer.len() >= LOG_BUFFER_LIMIT {
            self.log_buffer.pop_front();
        }
        self.log_buffer.push_back((print, Instant::now()));
    }
}

/// Whether to send a `ConnectUpdate` this tick, and what it should say.
///
/// * `desired` — what the slot's own option says: `Some(true)` participate, `Some(false)` do not,
///   `None` **not yet known** (slot data has not parsed). `None` must send NOTHING: guessing here
///   is exactly the bug this replaced.
/// * `advertised` — what the server currently believes. `false` on every fresh socket, because
///   [`CoreBase::new_connection`] connects with no tags at all.
///
/// Returns `Some(want)` to send tags for `want`, or `None` to stay quiet. Note that the common
/// case — option off, connected untagged — is `None`: a slot with DeathLink disabled never sends
/// a single tag packet.
///
/// The retract path (`Some(false)` while `advertised`) is not hypothetical: ER re-parses slot data
/// on a genuine SEED CHANGE without reopening the socket, so a reconnect to a different seed with
/// the option off has to take the tag back.
fn next_tag_advertisement(desired: Option<bool>, advertised: bool) -> Option<bool> {
    match desired {
        Some(d) if d != advertised => Some(d),
        _ => None,
    }
}

/// A trait for the core runners of FromSoftware game mods. This encapsulates
/// the interface that the shared overlay logic needs to interact with these
/// games.
pub trait Core: Send + Sized {
    /// The slot data for this runner.
    type SlotData: DeserializeOwned + Send + 'static;

    /// The game this is for.
    type Game: Game;

    /// Creates a new instance of the mod.
    fn new() -> Result<Self>;

    /// Returns the base struct.
    fn base(&self) -> &CoreBase<Self::Game, Self::SlotData>;

    /// Returns the mutable base struct.
    fn base_mut(&mut self) -> &mut CoreBase<Self::Game, Self::SlotData>;

    /// Updates the game logic and checks for common errors. This is only run if
    /// we're currently connected to the Archipelago server and the mod has not
    /// encountered a fatal error.
    fn update_live(&mut self) -> Result<()>;

    /// Implementors may override this to handles custom command inputs via the
    /// say console. Returns whether a command was handled.
    ///
    /// By default, this doesn't handle any commands.
    fn handle_command(&mut self, _command: &str, _arg: Option<&str>) -> bool {
        false
    }

    /// Usage strings for the game-specific dev console.
    ///
    /// The overlay renders this exact slice; implementations should also derive their `!help`
    /// output from it so the visible command list has one owner. Empty by default because each
    /// game's dispatcher is different.
    fn console_command_usages(&self) -> &'static [&'static str] {
        &[]
    }

    /// Whether this slot participates in DeathLink, or `None` while the answer is not yet known.
    ///
    /// Drives the `DeathLink` connection tag (see [`Self::reconcile_death_link_tag`]). The tag is
    /// what makes the server route other players' deaths here, so this must be the SLOT's answer,
    /// not a guess: return `None` until slot data has actually been read. Returning `Some(false)`
    /// early is fine; returning `Some(true)` early is how deaths get delivered to a slot that
    /// never asked for them.
    ///
    /// **The default is `None`, and a game with no DeathLink implementation must keep it.** Sekiro
    /// is exactly that case: it advertised the tag for its whole life and had nowhere to put the
    /// deaths the server duly sent it.
    fn death_link_enabled(&self) -> Option<bool> {
        None
    }

    /// Converges the server's idea of our tags onto [`Self::death_link_enabled`].
    ///
    /// Runs every tick while connected and is a no-op once the two agree, so it costs one
    /// comparison in the steady state. A failed send deliberately does NOT move the latch, which
    /// makes the next tick retry it.
    fn reconcile_death_link_tag(&mut self) {
        let Some(want) =
            next_tag_advertisement(self.death_link_enabled(), self.base().advertised_death_link)
        else {
            return;
        };
        let tags: &[&str] = if want { &["DeathLink"] } else { &[] };
        // Take the send's result before touching `base_mut` -- `client_mut` holds a mutable borrow
        // of `self` for as long as `client` is alive.
        let result = match self.client_mut() {
            Some(client) => client.update_connection(None, Some(tags.iter().copied())),
            None => return,
        };
        match result {
            Ok(()) => {
                self.base_mut().advertised_death_link = want;
                info!(
                    "DeathLink tag {} the server",
                    if want {
                        "advertised to"
                    } else {
                        "retracted from"
                    }
                );
            }
            Err(e) => warn!("DeathLink tag: ConnectUpdate failed ({e}) -- retrying next tick"),
        }
    }

    /// Lets a game add its own items to the overlay menu bar. Default: nothing.
    fn render_overlay_menu_items(&mut self, _ui: &imgui::Ui) {}

    /// Lets a game render its own overlay windows each frame (called at frame scope,
    /// not nested inside another window). Default: nothing.
    fn render_overlay_windows(&mut self, _ui: &imgui::Ui) {}

    /// Returns a reference to the Archipelago client, if it's connected.
    fn client(&self) -> Option<&ap::Client<Self::SlotData>> {
        self.base().connection.as_ref().and_then(|c| c.client())
    }

    /// Returns a mutable reference to the Archipelago client, if it's connected.
    fn client_mut(&mut self) -> Option<&mut ap::Client<Self::SlotData>> {
        self.base_mut()
            .connection
            .as_mut()
            .and_then(|c| c.client_mut())
    }

    /// Returns the seed the game expects to connect to.
    fn seed(&self) -> &str {
        self.base().config.seed()
    }

    /// Writes a message to the log buffer that we display to the user in the
    /// overlay, as well as to the internal logger.
    fn log(&mut self, message: impl Into<ap::Print>) {
        self.base_mut().log(message);
    }

    /// Consumes and returns all the as-yet-unprocessed events from the player's
    /// save.
    fn take_events(&mut self) -> Vec<ap::Event> {
        mem::take(&mut self.base_mut().event_buffer)
    }

    /// Runs the core logic of the mod. This may set [error], which should be
    /// surfaced to the user. Implementations should not override this; they
    /// should override [Self::update_live] instead.
    fn update(&mut self, is_main_menu: bool) {
        self.base_mut().update_always();

        if self.client().is_none() || self.base().error.is_some() {
            return;
        }

        // Tell the server whether we take DeathLink, now that slot data can answer it. Cheap and
        // idempotent; see the "NO TAGS AT CONNECT" note in `new_connection` for why it is here and
        // not there.
        self.reconcile_death_link_tag();

        // CHAT-RELAY dispatch: run commands queued from server chat through the same
        // handle_command path as overlay say input. Before the load/grace gating so
        // diagnostics work from the main menu too (flag writers degrade gracefully there).
        // Unknown commands log locally only -- never echoed back to chat (no relay loops).
        // `mem::take` rather than `drain(..).collect()`: draining EVERY element into a fresh Vec of the
        // same type allocates a second Vec for no reason (clippy::drain_collect). Same semantics --
        // pending_chat_commands is left empty either way.
        let pending: Vec<(String, Option<String>)> =
            std::mem::take(&mut self.base_mut().pending_chat_commands);
        for (cmd, arg) in pending {
            if !self.handle_command(&cmd, arg.as_deref()) {
                self.base_mut().log(ap::Print::message(format!(
                    "chat-relay: unknown command {cmd}"
                )));
            }
        }

        if is_main_menu {
            self.base_mut().load_time = None;
        } else if self.base().load_time.is_none() {
            self.base_mut().load_time = Some(Instant::now());
        }

        if let Some(time) = self.base().load_time
            && time.elapsed() < GRACE_PERIOD
        {
            return;
        }

        self.base_mut().error = match self
            .base()
            .check_version_conflict(Self::Game::CLIENT_VERSION)
        {
            Err(err) => Some(err),
            Ok(_) => self.update_live().err(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::next_tag_advertisement;

    /// Slot data has not landed yet. Any guess is wrong, so say nothing.
    #[test]
    fn unknown_never_sends() {
        assert_eq!(next_tag_advertisement(None, false), None);
        assert_eq!(next_tag_advertisement(None, true), None);
    }

    /// The motivating case (#258): the option is OFF and we connected untagged, so the server
    /// already believes the right thing. Not one packet.
    #[test]
    fn disabled_on_a_fresh_socket_is_silent() {
        assert_eq!(next_tag_advertisement(Some(false), false), None);
    }

    #[test]
    fn enabled_advertises_once_and_then_stays_quiet() {
        assert_eq!(next_tag_advertisement(Some(true), false), Some(true));
        assert_eq!(next_tag_advertisement(Some(true), true), None);
    }

    /// ER re-parses slot data on a seed change without reopening the socket, so the tag has to be
    /// takeable back.
    #[test]
    fn a_seed_change_to_death_link_off_retracts() {
        assert_eq!(next_tag_advertisement(Some(false), true), Some(false));
    }
}
