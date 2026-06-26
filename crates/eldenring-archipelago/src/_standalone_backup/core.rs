//! MILESTONE A (connect-first): lifecycle only, empty `update_live`. This is the smallest thing
//! that proves the whole `shared` bet — DllMain -> initialize -> Game impl -> CoreBase connection ->
//! overlay. It depends on NOTHING but `shared` + serde_json, so the first build doesn't need the
//! funnel modules, hook, shims, or er-logic. Swap to the full `core.rs` once this connects in-game.

use anyhow::Result;
use serde_json::Value;
use shared::CoreBase;

pub struct Core {
    base: CoreBase<crate::game::EldenRing, Value>,
}

impl shared::Core for Core {
    type SlotData = Value; // tolerant; keeps er-logic parsers usable when features get wired
    type Game = crate::game::EldenRing;

    fn new() -> Result<Self> {
        // CoreBase::new loads apconfig.json + opens the AP connection. The game name MUST match the
        // apworld's registered `game` exactly — this fork registers as "EldenRing" (no space), per
        // the seed spoiler; "Elden Ring" gets InvalidGame from the server.
        Ok(Self { base: CoreBase::new("EldenRing")? })
    }
    fn base(&self) -> &CoreBase<Self::Game, Self::SlotData> {
        &self.base
    }
    fn base_mut(&mut self) -> &mut CoreBase<Self::Game, Self::SlotData> {
        &mut self.base
    }

    fn update_live(&mut self) -> Result<()> {
        Ok(()) // connect-first: no game logic yet
    }
}
