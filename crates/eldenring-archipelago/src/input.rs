//! `EldenRingInputBlocker` — the real `shared::InputBlocker` for Elden Ring, so overlay input stops
//! leaking through to the game (typing `!markerprobe` no longer walks/rolls your character).
//!
//! # Why hooks on the standard input APIs, not the DS3 approach
//!
//! DS3's blocker (`darksouls3_extra::input`) hooks three per-device `dluid_*_device_should_block_input`
//! predicates by RVA. Static analysis of `eldenring.exe` (2026-07-21, see the artifacts repo's
//! `INPUT_BLOCK_RE.md`) shows ER has no such hookable predicate — the per-device "input allowed" check
//! is INLINED into each device's poll. But ER reads all input through STANDARD Windows APIs (confirmed
//! in its import table + disassembly), which are stable, named, version-independent hook targets:
//!
//!   * **GamePad**  — `XInputGetState` (xinput1_4.dll). ER polls controllers here.
//!   * **Keyboard/Mouse** — `IDirectInputDevice8::GetDeviceState` (COM vtable slot 9, `+0x48`).
//!     CONFIRMED: `KeyboardDevice::poll` does `call [rax+0x48]` on its DirectInput device
//!     (`[this+0x7E0]`). Reached by wrapping `DirectInput8Create` -> `IDirectInput8::CreateDevice`
//!     (slot 3, `+0x18`) -> the returned device's shared vtable (patched once; all devices share it).
//!   * **Menu/text** — `GetKeyboardState` / `GetKeyState` (user32), which ER also reads.
//!
//! Each hook, when its [`InputFlags`] bit is set, zeroes the state it returns instead of the real read,
//! so the game sees "nothing pressed" while the overlay owns the keyboard/mouse/pad. Nothing here is
//! version-pinned (unlike our RVA-pinned param/detour hooks) — it survives ER patches.
//!
//! `error_display.rs` already drives this: every frame it turns imgui's `want_capture_*` into
//! `InputFlags` and calls [`InputBlocker::block_only`]. This type just stores the flags and lets the
//! hooks read them.

// This module is almost entirely `unsafe` FFI (WINAPI detours + COM vtable patching): every line
// derefs a raw pointer, transmutes a resolved export, or calls an `unsafe` hook/WINAPI. Wrapping each
// op in its own `unsafe {}` would be pure noise, so opt the module out of the 2024 lint instead.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::mem;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use retour::GenericDetour;
use shared::{InputBlocker, InputFlags};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};
use windows::core::{GUID, PCSTR, s};

/// The currently-blocked input classes (an [`InputFlags`] bit set). Read by every hook; written by
/// [`EldenRingInputBlocker::block_only`]. `Relaxed` is fine: a one-frame stale read only means input
/// flows/stops one frame late, which is imperceptible.
static BLOCKED: AtomicU8 = AtomicU8::new(0);

#[inline]
fn is_blocked(flag: InputFlags) -> bool {
    InputFlags::from_bits_truncate(BLOCKED.load(Ordering::Relaxed)).contains(flag)
}

/// The `shared::InputBlocker` ER hands to `shared::initialize`. Stateless: the block state is the
/// process-global [`BLOCKED`] the hooks read.
pub struct EldenRingInputBlocker;

impl InputBlocker for EldenRingInputBlocker {
    fn block_only(&self, inputs: InputFlags) {
        BLOCKED.store(inputs.bits(), Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------------------------
// Flat WINAPI detours (retour), resolved by name at install time.
// ---------------------------------------------------------------------------------------------

type XInputGetStateFn = unsafe extern "system" fn(u32, *mut XInputState) -> u32;
type GetKeyboardStateFn = unsafe extern "system" fn(*mut u8) -> i32;
type GetKeyStateFn = unsafe extern "system" fn(i32) -> i16;
type DirectInput8CreateFn =
    unsafe extern "system" fn(*mut c_void, u32, *const GUID, *mut *mut c_void, *mut c_void) -> i32;

/// Minimal `XINPUT_STATE` (16-byte `XINPUT_GAMEPAD` + packet number). We only need to zero it.
#[repr(C)]
struct XInputState {
    packet_number: u32,
    gamepad: XInputGamepad,
}
#[repr(C)]
struct XInputGamepad {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    thumb_lx: i16,
    thumb_ly: i16,
    thumb_rx: i16,
    thumb_ry: i16,
}

static XINPUT_HOOK: OnceLock<GenericDetour<XInputGetStateFn>> = OnceLock::new();
static GETKEYBOARDSTATE_HOOK: OnceLock<GenericDetour<GetKeyboardStateFn>> = OnceLock::new();
static GETKEYSTATE_HOOK: OnceLock<GenericDetour<GetKeyStateFn>> = OnceLock::new();
static DINPUT8CREATE_HOOK: OnceLock<GenericDetour<DirectInput8CreateFn>> = OnceLock::new();

unsafe extern "system" fn xinput_get_state_hook(user: u32, state: *mut XInputState) -> u32 {
    let hook = XINPUT_HOOK.get().unwrap();
    let ret = hook.call(user, state);
    // Zero the gamepad to NEUTRAL (not "disconnected", which would pop a UI warning) while blocked.
    if ret == 0 && is_blocked(InputFlags::GamePad) && !state.is_null() {
        (*state).gamepad = XInputGamepad {
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            thumb_lx: 0,
            thumb_ly: 0,
            thumb_rx: 0,
            thumb_ry: 0,
        };
    }
    ret
}

unsafe extern "system" fn get_keyboard_state_hook(buf: *mut u8) -> i32 {
    let hook = GETKEYBOARDSTATE_HOOK.get().unwrap();
    let ret = hook.call(buf);
    if ret != 0 && is_blocked(InputFlags::Keyboard) && !buf.is_null() {
        std::ptr::write_bytes(buf, 0, 256); // the full 256-key state -> nothing down
    }
    ret
}

unsafe extern "system" fn get_key_state_hook(vkey: i32) -> i16 {
    if is_blocked(InputFlags::Keyboard) {
        return 0; // key up, not toggled
    }
    GETKEYSTATE_HOOK.get().unwrap().call(vkey)
}

// ---------------------------------------------------------------------------------------------
// DirectInput8 device vtable wrap (the real keyboard/mouse path).
// ---------------------------------------------------------------------------------------------

/// `IDirectInput8::CreateDevice` = vtable slot 3.
const IDINPUT8_CREATEDEVICE: usize = 3;
/// `IDirectInputDevice8::GetDeviceState` = vtable slot 9 (`+0x48`, confirmed in KeyboardDevice::poll).
/// IMMEDIATE state (held keys / current mouse position) — the gameplay path.
const IDIDEVICE8_GETDEVICESTATE: usize = 9;
/// `IDirectInputDevice8::GetDeviceData` = vtable slot 10 (`+0x50`). BUFFERED events — the MENU / text /
/// key-repeat path. ER menus read this, so blocking only `GetDeviceState` let keystrokes still reach an
/// open menu behind the overlay (observed: typing in Change Connection navigated the game menu).
const IDIDEVICE8_GETDEVICEDATA: usize = 10;
/// The immediate keyboard state buffer is 256 bytes; anything smaller is the mouse. Fallback device
/// discriminator when the device pointer wasn't captured at CreateDevice.
const DIKEYBOARD_STATE_BYTES: u32 = 256;

/// DirectInput system-device GUIDs (used to tag each created device kbd/mouse, so both `GetDeviceState`
/// and `GetDeviceData` — which the shared vtable hook can't otherwise tell apart — know which flag
/// governs them). `{6F1D2B6x-D5A0-11CF-BFC7-444553540000}`.
const GUID_SYS_KEYBOARD: GUID = GUID::from_u128(0x6F1D_2B61_D5A0_11CF_BFC7_4445_5354_0000);
const GUID_SYS_MOUSE: GUID = GUID::from_u128(0x6F1D_2B60_D5A0_11CF_BFC7_4445_5354_0000);

static ORIG_CREATE_DEVICE: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_DEVICE_STATE: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_DEVICE_DATA: AtomicUsize = AtomicUsize::new(0);
static DEVICE_VT_HOOKED: AtomicBool = AtomicBool::new(false);
/// The DirectInput device instances, tagged by GUID at CreateDevice, so the shared-vtable hooks can
/// resolve keyboard vs mouse from the `this` pointer.
static KB_DEVICE: AtomicUsize = AtomicUsize::new(0);
static MOUSE_DEVICE: AtomicUsize = AtomicUsize::new(0);

/// Which input class a DirectInput device belongs to. Prefers the pointer tagged at CreateDevice;
/// falls back to the `GetDeviceState` buffer size (256 = keyboard) when only that is available.
fn device_flag(this: *mut c_void, cb: Option<u32>) -> InputFlags {
    let p = this as usize;
    if p != 0 && p == KB_DEVICE.load(Ordering::Relaxed) {
        return InputFlags::Keyboard;
    }
    if p != 0 && p == MOUSE_DEVICE.load(Ordering::Relaxed) {
        return InputFlags::Mouse;
    }
    match cb {
        Some(n) if n == DIKEYBOARD_STATE_BYTES => InputFlags::Keyboard,
        _ => InputFlags::Mouse,
    }
}

/// Overwrite `vtable[index]` with `hook`, returning the original pointer. The vtable lives in a
/// read-only page, so flip it writable for the 8-byte store.
unsafe fn patch_vtable_slot(vtable: *mut usize, index: usize, hook: usize) -> usize {
    let slot = vtable.add(index);
    let old = *slot;
    let mut prot = PAGE_PROTECTION_FLAGS(0);
    if VirtualProtect(slot as *const c_void, 8, PAGE_READWRITE, &mut prot).is_ok() {
        *slot = hook;
        let _ = VirtualProtect(slot as *const c_void, 8, prot, &mut prot);
    }
    old
}

/// The vtable pointer of a COM object is its first field.
#[inline]
unsafe fn vtable_of(obj: *mut c_void) -> *mut usize {
    *(obj as *const *mut usize)
}

type CreateDeviceFn =
    unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void, *mut c_void) -> i32;
type GetDeviceStateFn = unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> i32;
type GetDeviceDataFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut u32, u32) -> i32;

unsafe extern "system" fn direct_input8_create_hook(
    inst: *mut c_void,
    version: u32,
    riid: *const GUID,
    out: *mut *mut c_void,
    outer: *mut c_void,
) -> i32 {
    let hook = DINPUT8CREATE_HOOK.get().unwrap();
    let hr = hook.call(inst, version, riid, out, outer);
    // On the first successful IDirectInput8, wrap its CreateDevice so we can reach the devices.
    if hr >= 0
        && !out.is_null()
        && !(*out).is_null()
        && ORIG_CREATE_DEVICE.load(Ordering::Relaxed) == 0
    {
        let vt = vtable_of(*out);
        let old = patch_vtable_slot(vt, IDINPUT8_CREATEDEVICE, create_device_hook as usize);
        ORIG_CREATE_DEVICE.store(old, Ordering::Relaxed);
    }
    hr
}

unsafe extern "system" fn create_device_hook(
    this: *mut c_void,
    rguid: *const GUID,
    out: *mut *mut c_void,
    outer: *mut c_void,
) -> i32 {
    let orig: CreateDeviceFn = mem::transmute(ORIG_CREATE_DEVICE.load(Ordering::Relaxed));
    let hr = orig(this, rguid, out, outer);
    if hr >= 0 && !out.is_null() && !(*out).is_null() {
        let dev = *out;
        // Tag the device by GUID so the shared-vtable hooks can resolve kbd vs mouse per call.
        if !rguid.is_null() {
            let g = *rguid;
            if g == GUID_SYS_KEYBOARD {
                KB_DEVICE.store(dev as usize, Ordering::Relaxed);
            } else if g == GUID_SYS_MOUSE {
                MOUSE_DEVICE.store(dev as usize, Ordering::Relaxed);
            }
        }
        // Keyboard + mouse share ONE IDirectInputDevice8 vtable; patch it once — both immediate
        // (GetDeviceState) and buffered (GetDeviceData) — and every device is covered.
        if !DEVICE_VT_HOOKED.swap(true, Ordering::Relaxed) {
            let vt = vtable_of(dev);
            ORIG_GET_DEVICE_STATE.store(
                patch_vtable_slot(
                    vt,
                    IDIDEVICE8_GETDEVICESTATE,
                    get_device_state_hook as usize,
                ),
                Ordering::Relaxed,
            );
            ORIG_GET_DEVICE_DATA.store(
                patch_vtable_slot(vt, IDIDEVICE8_GETDEVICEDATA, get_device_data_hook as usize),
                Ordering::Relaxed,
            );
        }
    }
    hr
}

/// IMMEDIATE state (held keys / mouse delta): zero the whole buffer when the device is blocked.
unsafe extern "system" fn get_device_state_hook(
    this: *mut c_void,
    cb: u32,
    data: *mut c_void,
) -> i32 {
    let orig: GetDeviceStateFn = mem::transmute(ORIG_GET_DEVICE_STATE.load(Ordering::Relaxed));
    let hr = orig(this, cb, data);
    if hr >= 0 && !data.is_null() && is_blocked(device_flag(this, Some(cb))) {
        std::ptr::write_bytes(data as *mut u8, 0, cb as usize); // nothing pressed / no delta
    }
    hr
}

/// BUFFERED events (menu / text / key-repeat): let `orig` DRAIN the device buffer (so events don't pile
/// up), then report ZERO events to the caller when blocked, so no keystroke reaches the game menu.
unsafe extern "system" fn get_device_data_hook(
    this: *mut c_void,
    cb_object_data: u32,
    rgdod: *mut c_void,
    pdw_in_out: *mut u32,
    flags: u32,
) -> i32 {
    let orig: GetDeviceDataFn = mem::transmute(ORIG_GET_DEVICE_DATA.load(Ordering::Relaxed));
    let hr = orig(this, cb_object_data, rgdod, pdw_in_out, flags);
    if hr >= 0 && !pdw_in_out.is_null() && is_blocked(device_flag(this, None)) {
        *pdw_in_out = 0; // events drained by `orig`; caller sees none
    }
    hr
}

// ---------------------------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------------------------

/// Resolve `module!name` and hand back a typed fn pointer, or `None` if the module/export is absent.
unsafe fn resolve<T>(module: PCSTR, name: PCSTR) -> Option<T> {
    let h = GetModuleHandleA(module).ok()?;
    let p = GetProcAddress(h, name)?;
    Some(mem::transmute_copy::<_, T>(&p))
}

/// Install the input hooks. Call ONCE, early (from `DllMain`, before the game's input init runs), so
/// the `DirectInput8Create` wrap is in place before ER creates its keyboard/mouse devices. The flat
/// user32/xinput detours can go in at any time. Failures are logged, not fatal: a missing hook just
/// means that class can't be blocked (degrades to the old leak for that class only).
///
/// # Safety
/// Installs process-wide function detours + a COM vtable patch. Call once, on the main thread.
pub unsafe fn install() {
    // xinput1_4 is a static import of eldenring.exe, so it's already loaded by DllMain time.
    match resolve::<XInputGetStateFn>(s!("xinput1_4.dll"), s!("XInputGetState")) {
        Some(target) => match GenericDetour::new(target, xinput_get_state_hook) {
            Ok(d) => match d.enable() {
                Ok(()) => {
                    let _ = XINPUT_HOOK.set(d);
                    log::info!("input: XInputGetState hooked (gamepad block)");
                }
                Err(e) => log::warn!("input: XInputGetState enable failed: {e}"),
            },
            Err(e) => log::warn!("input: XInputGetState hook failed: {e}"),
        },
        None => log::warn!("input: XInputGetState not found — gamepad won't block"),
    }

    if let Some(target) = resolve::<GetKeyboardStateFn>(s!("user32.dll"), s!("GetKeyboardState")) {
        if let Ok(d) = GenericDetour::new(target, get_keyboard_state_hook) {
            if d.enable().is_ok() {
                let _ = GETKEYBOARDSTATE_HOOK.set(d);
            }
        }
    }
    if let Some(target) = resolve::<GetKeyStateFn>(s!("user32.dll"), s!("GetKeyState")) {
        if let Ok(d) = GenericDetour::new(target, get_key_state_hook) {
            if d.enable().is_ok() {
                let _ = GETKEYSTATE_HOOK.set(d);
            }
        }
    }

    match resolve::<DirectInput8CreateFn>(s!("dinput8.dll"), s!("DirectInput8Create")) {
        Some(target) => match GenericDetour::new(target, direct_input8_create_hook) {
            Ok(d) => match d.enable() {
                Ok(()) => {
                    let _ = DINPUT8CREATE_HOOK.set(d);
                    log::info!("input: DirectInput8Create hooked (keyboard/mouse block)");
                }
                Err(e) => log::warn!("input: DirectInput8Create enable failed: {e}"),
            },
            Err(e) => log::warn!("input: DirectInput8Create hook failed: {e}"),
        },
        None => log::warn!("input: DirectInput8Create not found — kbd/mouse won't block"),
    }
}
