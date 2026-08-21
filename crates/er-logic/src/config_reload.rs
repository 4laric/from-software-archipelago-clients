//! Hot-reload of the connection config: change server/slot WITHOUT fighting the game for input.
//!
//! WHY. Once Elden Ring has focus, editing connection info in the in-game overlay is miserable:
//! clicking closes the ER menu, and Escape (which opens the ER menu) closes the client's input
//! window. The root cause is that ER's `InputBlocker` is `shared::NoOpInputBlocker` -- it blocks
//! NOTHING -- so every click and keypress reaches the overlay AND the game. DS3 has a real blocker
//! (nex3/fromsoftware-extra hooks the engine's `dluid_*_device_should_block_input`); no such crate
//! exists for ER, so the overlay simply cannot take focus cleanly.
//!
//! Rather than reverse-engineer ER's input path to fix a text box, sidestep it: the client already
//! reads `apconfig.json` ({"url","slot","password"}). Watch it. Edit it in any text editor, alt-tab
//! back, and the client reconnects. No overlay, no input fight. (The proper InputBlocker is still
//! worth doing -- it is just not a prerequisite for changing a server address.)
//!
//! THE TRAP this predicate exists for: `update_connection_info` SAVES the config, so a naive watcher
//! sees its own write and reconnects again -- forever. And a text editor's save is not atomic: a
//! half-written file parses to empty fields, and reconnecting to `url: ""` drops a live session for
//! nothing. Both are decided here, purely, and both are pinned by the replay.

/// The connection fields the client actually uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnInfo {
    pub url: String,
    pub slot: String,
    pub password: Option<String>,
}

impl ConnInfo {
    /// Enough to connect with. A file caught mid-write parses to empty fields; that is not a config.
    pub fn is_complete(&self) -> bool {
        !self.url.trim().is_empty() && !self.slot.trim().is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReloadAction {
    /// Nothing to do -- unchanged, incomplete, or it is the echo of our own save.
    Ignore,
    /// The file names a different, complete connection: apply it and reconnect.
    Reconnect(ConnInfo),
}

/// `applied` -- what the client is currently connected with (what it last wrote/read).
/// `on_disk` -- what apconfig.json says right now.
pub fn reload_action(applied: &ConnInfo, on_disk: &ConnInfo) -> ReloadAction {
    if !on_disk.is_complete() {
        return ReloadAction::Ignore; // half-written save, or a config that cannot connect anyway
    }
    if on_disk == applied {
        return ReloadAction::Ignore; // unchanged -- including the echo of our OWN save (no loop)
    }
    ReloadAction::Reconnect(on_disk.clone())
}

// ---------------------------------------------------------------------------------------------
// Live probe toggles (client#166) -- the SAME watcher, a different key family
// ---------------------------------------------------------------------------------------------

/// The `probes` object of apconfig.json: probe name -> on/off. Kept as the plain map because
/// `shared` is game-agnostic and does not enumerate a game's probe names.
pub type ProbeMap = std::collections::BTreeMap<String, bool>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeAction {
    /// Nothing to do -- unchanged, INCLUDING the echo of our own `save()` (which serialises the
    /// probes map too: same storm trap as the connection half, same remedy).
    Ignore,
    /// The probes object on disk differs from what is installed: install it, re-arm the
    /// active-probe announcement (so it says so ONCE PER CHANGE, not once per process), and fold
    /// the map into the in-memory config so a later connection save cannot clobber the edit.
    Apply(ProbeMap),
}

/// The probe-toggle twin of [`reload_action`]. No completeness gate: unlike a connection, an
/// EMPTY probes object is a meaningful edit ("turn them all off"), and a torn write never reaches
/// here at all -- `read_on_disk` rejects unparseable JSON before either predicate runs.
pub fn probe_reload_action(applied: &ProbeMap, on_disk: &ProbeMap) -> ProbeAction {
    if on_disk == applied {
        return ProbeAction::Ignore;
    }
    ProbeAction::Apply(on_disk.clone())
}
