use anyhow::Result;

use crate::{Core, InputBlocker};

/// Semantic colours for a game's in-process overlay.
///
/// The shared renderer owns the widget mapping; games only choose a palette. Keeping this free of
/// `imgui` types makes the contract small and prevents game crates from styling individual widgets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayTheme {
    pub background: [f32; 3],
    pub title_background: [f32; 3],
    pub border: [f32; 3],
    pub text: [f32; 3],
    pub muted_text: [f32; 3],
    pub accent: [f32; 3],
    pub selection: [f32; 3],
}

/// A trait that encapsulates specific behavior for each individual game that's
/// used by the shared library. We try to keep this minimal, with most game
/// interactions being left in the individual game mod crates.
pub trait Game: Send + Sync + 'static {
    /// This game's core mod type.
    type Core: Core;

    /// The hudhook type for this game's graphics implementation.
    type GraphicsHooks: hudhook::Hooks;

    /// The input blocker type to block input to this game.
    type InputBlocker: InputBlocker;

    /// Which game this represents.
    const TYPE: GameType;

    /// The version of this client.
    const CLIENT_VERSION: &str;
    /// The BUILD identity shown on player-visible surfaces (the overlay window title). Defaults to
    /// the plain version; a game crate that bakes a build stamp (ER: git SHA via build.rs) overrides
    /// this so a player's screenshot pins the exact build -- two builds can share CLIENT_VERSION
    /// (the 2026-07-30 "I'm on the new version" flood report was undecidable from a screenshot
    /// because the version-bump commit and the fix commit both said 0.2.17). Version-conflict
    /// checks keep using CLIENT_VERSION; this const is identity, not compatibility.
    const CLIENT_BUILD: &str = Self::CLIENT_VERSION;
    /// Optional semantic palette. `None` deliberately leaves imgui's defaults untouched, so games
    /// which have not opted in cannot change appearance when another game adds a theme.
    const OVERLAY_THEME: Option<OverlayTheme> = None;
    /// Echo own checks back (items_handling own_world bit); ER overrides to true.
    const OWN_WORLD: bool = false;

    /// Schedules `task` to be run each frame, ideally at the beginning of the
    /// frame, on the game's main thread.
    ///
    /// This blocks until the task running infrastructure is available, and so
    /// should not be called on the game's main thread.
    fn run_recurring_task(task: impl FnMut() + 'static + Send) -> Result<()>;

    /// Returns whether the game is currently showing the main menu (or earlier
    /// during the initial load process).
    ///
    /// ## Safety
    ///
    /// This must be called on the main thread when no other references exist to
    /// the game's internal state.
    unsafe fn is_main_menu() -> bool;

    /// Forces the cursor to be visible on-screen.
    ///
    /// By default, does nothing.
    ///
    /// ## Safety
    ///
    /// This must be called on the main thread when no other references exist to
    /// the game's internal state.
    unsafe fn force_cursor_visible() {}

    /// Returns whether the player is currently in a menu, as opposed to
    /// actively playing the game.
    ///
    /// By default, this always returns false.
    ///
    /// ## Safety
    ///
    /// This must be called on the main thread when no other references exist to
    /// the game's internal state.
    unsafe fn is_menu_open() -> bool {
        false
    }
}

/// An enum of From Software games, for situtations where the shared code just
/// needs to do some small difference for each one.
pub enum GameType {
    DarkSoulsIII,
    Sekiro,
    EldenRing,
}

impl GameType {
    /// Returns a short, human-friendly name for this game.
    pub fn short_name(&self) -> &str {
        match self {
            GameType::DarkSoulsIII => "DS3",
            GameType::Sekiro => "Sekiro",
            GameType::EldenRing => "ER",
        }
    }

    /// The basename for the static randomizer for this game.
    pub fn static_randomizer_basename(&self) -> &str {
        match self {
            GameType::DarkSoulsIII => "DS3Randomizer.exe",
            GameType::Sekiro => "SekiroRandomizer.exe",
            GameType::EldenRing => "EldenRingRandomizer.exe",
        }
    }
}
