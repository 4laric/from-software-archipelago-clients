//! The click-through escape hatch.
//!
//! Click-through makes the window ignore the mouse, which means it also ignores the checkbox that
//! turned it on. Shipping that without a way back is shipping a window a player can lose, so the
//! toggle is armed by a system-wide hotkey registered outside the egui event loop:
//! **Ctrl+Shift+F10** sets a flag that the next frame reads and clears.
//!
//! It is a thread of its own because `RegisterHotKey` delivers `WM_HOTKEY` to the *registering*
//! thread's message queue, and this crate does not own the queue winit runs on. The thread holds
//! no window handle, no client state and no lock the renderer waits on; it flips one atomic.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// Set when the hotkey fires. The UI thread takes the flag and turns click-through off.
#[derive(Clone, Default)]
pub struct Escape(Arc<AtomicBool>);

impl Escape {
    /// Consumes a pending press, if there is one.
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::Relaxed)
    }
}

/// Registers the hotkey and returns the flag it sets.
///
/// A failed registration -- most often another application already owns Ctrl+Shift+F10 -- returns
/// `None` rather than an error: it is a reason to disable the click-through toggle, not a reason to
/// fail a client launch.
pub fn register() -> Option<Escape> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_CONTROL, MOD_SHIFT, RegisterHotKey, VK_F10,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    const HOTKEY_ID: i32 = 0x4243;

    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let escape = Escape::default();
    let flag = Arc::clone(&escape.0);
    thread::spawn(move || {
        // SAFETY: the hotkey is registered against this thread's own message queue (a null window
        // handle), and every message read below is owned by that queue.
        let registered = unsafe {
            RegisterHotKey(
                None,
                HOTKEY_ID,
                MOD_CONTROL | MOD_SHIFT,
                u32::from(VK_F10.0),
            )
        }
        .is_ok();
        let _ = ready_tx.send(registered);
        if !registered {
            return;
        }
        let mut message = MSG::default();
        // SAFETY: `GetMessageW` blocks this thread on its own queue and writes into `message`.
        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
            if message.message == WM_HOTKEY && message.wParam.0 as i32 == HOTKEY_ID {
                flag.store(true, Ordering::Relaxed);
            }
        }
    });

    ready_rx.recv().ok().and_then(|ok| ok.then_some(escape))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_escape_press_is_consumed_exactly_once() {
        let escape = Escape::default();
        assert!(!escape.take(), "no press, no toggle");
        escape.0.store(true, Ordering::Relaxed);
        assert!(escape.take());
        assert!(!escape.take(), "a single press must not toggle twice");
    }
}
