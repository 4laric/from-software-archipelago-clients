//! Experimental native rebirth entry point. Command 113 (no arguments) is used by
//! TGA's Rebirth.cea and TarnishedTool's TalkCommands.Rebirth. This deliberately
//! does not run Rennala's talk script, remove goods, or change quest flags.
//!
//! All engine calls and callback-owner access belong to FrameBegin. The overlay
//! only posts a one-shot request. A single TalkScript allocation is retained for
//! the process lifetime: on an interrupted menu we cannot prove the engine has
//! discarded its callbacks, so we refuse reuse instead of freeing their owner.
//! See docs/FREE_RESPEC.md for source provenance and the outstanding live gate.

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use eldenring::cs::{
    BlockId, CSMenuManImp, CSSessionManager, FieldInsHandle, GameDataMan, GameMan, LobbyState,
    MenuType, ProtocolState, TalkScript, WorldChrMan,
};
use eldenring::ez_state::EzStateValue;
use er_logic::respec::{Notice, Observation, State};
use fromsoftware_shared::FromStatic;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static HOLD: AtomicBool = AtomicBool::new(false);
static INPUT: AtomicBool = AtomicBool::new(false);
static MESSAGE: Mutex<Option<String>> = Mutex::new(None);

thread_local! {
    static RUNTIME: RefCell<Runtime> = RefCell::new(Runtime::default());
}

#[derive(Default)]
struct Runtime {
    // Intentionally bounded to ONE leaked allocation. Never freed or retargeted
    // after loss of an active owner; see module doc. No unsafe Send/Sync impl.
    script: Option<&'static mut TalkScript>,
    state: State,
    clock: Option<Instant>,
    owner: Option<(FieldInsHandle, usize)>,
    before: Option<Stats>,
    stable_player: Option<(FieldInsHandle, usize)>,
    stable_since: u64,
    owner_lost: bool,
}

#[derive(Debug, Clone, Copy)]
struct Stats {
    level: u32,
    class: u8,
    attributes: [u32; 8],
}

fn stats() -> Option<Stats> {
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let p = gdm.main_player_game_data.as_ref();
    Some(Stats {
        level: p.level,
        class: p.archetype,
        attributes: [
            p.vigor,
            p.mind,
            p.endurance,
            p.strength,
            p.dexterity,
            p.intelligence,
            p.faith,
            p.arcane,
        ],
    })
}

pub fn request() {
    if !busy() {
        REQUESTED.store(true, Ordering::Release);
    }
}

pub fn busy() -> bool {
    REQUESTED.load(Ordering::Acquire) || HOLD.load(Ordering::Acquire)
}

pub fn owns_input() -> bool {
    INPUT.load(Ordering::Acquire)
}

pub fn take_message() -> Option<String> {
    MESSAGE.lock().ok()?.take()
}

fn report(message: impl Into<String>) {
    let message = message.into();
    log::info!("respec: {message}");
    if let Ok(mut slot) = MESSAGE.lock() {
        *slot = Some(message);
    }
}

fn player_identity() -> Option<(FieldInsHandle, usize)> {
    let wcm = unsafe { WorldChrMan::instance() }.ok()?;
    let p = wcm.main_player.as_ref()?;
    Some((p.chr_ins.field_ins_handle, p as *const _ as usize))
}

/// All these reads precede creating/invoking the synthetic talk script.
fn ready() -> Result<(FieldInsHandle, usize), &'static str> {
    let identity = player_identity().ok_or("Load a character before respeccing.")?;
    let wcm = unsafe { WorldChrMan::instance() }.map_err(|_| "Player unavailable.")?;
    let p = wcm.main_player.as_ref().ok_or("Player unavailable.")?;
    if p.chr_ins.modules.data.hp <= 0 {
        return Err("Wait until you are alive before respeccing.");
    }
    if p.chr_ins.modules.ride.is_mounted || p.chr_ins.modules.ride.is_mounting {
        return Err("Dismount before respeccing.");
    }
    let gm = unsafe { GameMan::instance() }.map_err(|_| "Game state unavailable.")?;
    if gm.warp_requested {
        return Err("Wait until travel has finished.");
    }
    let session =
        unsafe { CSSessionManager::instance() }.map_err(|_| "Session state unavailable.")?;
    if session.lobby_state != LobbyState::None || session.protocol_state != ProtocolState::None {
        return Err("Respec is unavailable during game multiplayer.");
    }
    let menu = unsafe { CSMenuManImp::instance() }.map_err(|_| "Menu state unavailable.")?;
    if menu
        .player_menu_ctrl
        .chr_menu_flags
        .flags
        .pause_menu_state()
    {
        return Err("Wait until the game allows menus again.");
    }
    if crate::flags::boss_healthbar_npc_param_id() != Some(0) {
        return Err("Respec is unavailable during a boss fight.");
    }
    if !crate::esd_probe::talk_is_quiet() {
        return Err("Finish the current conversation, then try again.");
    }
    Ok(identity)
}

fn query(script: &mut TalkScript, id: i32, args: &[i32]) -> Result<bool, String> {
    let values = args.iter().copied().map(EzStateValue::Int32);
    let value = script.env((id, values)).map_err(|e| e.to_string())?;
    match i32::from(value) {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(format!(
            "Unexpected menu query {id} result {other}; respec refused."
        )),
    }
}

impl Runtime {
    fn open(&mut self, now: u64) -> Result<(), String> {
        if self.state != State::Idle {
            return Err(
                "Respec is unavailable after an interrupted menu; restart the game.".into(),
            );
        }
        let identity = ready()?;
        if self.stable_player != Some(identity)
            || now.saturating_sub(self.stable_since) < er_logic::respec::SETTLE_MS
        {
            return Err("Wait for the character to finish loading, then try again.".into());
        }
        let script = self.script.get_or_insert_with(|| {
            Box::leak(Box::new(TalkScript::new(BlockId::none(), 1000, identity.0)))
        });
        script.npc_talk.base.field_ins_handle = identity.0;
        // ESDLang Talk documentation: env 25 uses GLOBAL menu IDs, not MenuType.
        // Only documented menus are queried; no guessed UI-state byte offsets.
        for menu in [11, 12, 13, 23, 25, 26, 30, 31, 36, 63] {
            if query(script, 25, &[menu])? {
                return Err("Close the current game menu before respeccing.".into());
            }
        }
        // ESDLang identifies env 0 as GetWhetherEnemiesAreNearby(unk).
        // Argument 0 is a prototype candidate; live acceptance must verify its
        // ordinary-enemy behaviour. The boss gate above is independently typed.
        if query(script, 0, &[0])? {
            return Err("Move away from enemies before respeccing.".into());
        }
        self.before = Some(stats().ok_or("Player stats unavailable.")?);
        self.owner = Some(identity);
        log::info!("respec: requesting command 113; before={:?}", self.before);
        HOLD.store(true, Ordering::Release);
        script.event(113).map_err(|e| e.to_string())?;
        self.state.request(now);
        Ok(())
    }

    fn tick(&mut self) {
        let now = self
            .clock
            .get_or_insert_with(Instant::now)
            .elapsed()
            .as_millis() as u64;
        let identity = player_identity();
        let transitioning = unsafe { GameMan::instance() }
            .map(|gm| gm.warp_requested)
            .unwrap_or(true);
        if identity != self.stable_player || transitioning {
            self.stable_player = identity;
            self.stable_since = now;
        }
        let requested = REQUESTED.swap(false, Ordering::AcqRel);
        if requested {
            if let Err(e) = self.open(now) {
                report(e);
            }
        }
        if self.state != State::Idle {
            self.owner_lost |= identity.is_none()
                || identity != self.owner
                || transitioning
                || crate::deathlink::read_local_hp().is_none_or(|hp| hp <= 0);
            let observation = if self.owner_lost {
                Observation::OwnerLost
            } else if let Some(script) = self.script.as_mut() {
                if matches!(self.state, State::Faulted { .. }) {
                    // No more engine calls after a fault, even if the old handle
                    // happens to resolve to a newly spawned character.
                    if script.npc_talk.menu_state.current_open_menu == MenuType::None
                        && script
                            .npc_talk
                            .menu_state
                            .open_menu_job
                            .finalize_callback_job
                            .is_none()
                    {
                        Observation::Closed
                    } else {
                        Observation::Pending
                    }
                } else {
                    // Same checked query used by the pinned invoke-esd example.
                    match query(script, 59, &[MenuType::ReallocateAttributes as i32, 0]) {
                        Ok(true) => Observation::Open,
                        Ok(false)
                            if script.npc_talk.menu_state.current_open_menu == MenuType::None
                                && script
                                    .npc_talk
                                    .menu_state
                                    .open_menu_job
                                    .finalize_callback_job
                                    .is_none() =>
                        {
                            Observation::Closed
                        }
                        _ => Observation::Pending,
                    }
                }
            } else {
                Observation::OwnerLost
            };
            if let Some(notice) = self.state.observe(now, observation) {
                match notice {
                    Notice::Opened => log::info!("respec: native menu observed open"),
                    Notice::Closed => {
                        let after = stats();
                        log::info!(
                            "respec: native menu retired; before={:?}; after={after:?}",
                            self.before
                        );
                        if let (Some(before), Some(after)) = (self.before, after) {
                            if before.level != after.level
                                || before.class != after.class
                                || before.attributes.iter().map(|&v| u64::from(v)).sum::<u64>()
                                    != after.attributes.iter().map(|&v| u64::from(v)).sum::<u64>()
                            {
                                self.state = State::Faulted {
                                    hold_effects: false,
                                };
                                report(
                                    "Respec stat verification failed. Stop testing and inspect the respec log.",
                                );
                            } else {
                                report("Respec menu closed; level and attribute total verified.");
                            }
                        } else {
                            report("Respec menu closed; stat read-back unavailable.");
                        }
                    }
                    Notice::FailedToOpen => report(
                        "Respec opening could not be confirmed. Restart the game before retrying.",
                    ),
                    Notice::FailedToClose => report(
                        "Respec callback cleanup could not be confirmed. Restart the game before retrying; AP effects remain held.",
                    ),
                    Notice::Interrupted => report(
                        "Respec interrupted by a world change. Restart the game before retrying.",
                    ),
                }
            }
        }
        HOLD.store(self.state.holds_effects(), Ordering::Release);
        INPUT.store(self.state.owns_input(), Ordering::Release);
    }
}

/// Called before the shared AP update, even when disconnected or loading.
pub fn tick() {
    RUNTIME.with(|runtime| runtime.borrow_mut().tick());
}
