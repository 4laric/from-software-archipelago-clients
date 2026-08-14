use std::sync::{Arc, Mutex};

use anyhow::{Error, Result};
use hudhook::{ImguiRenderLoop, RenderContext};
use imgui::*;

use crate::{Core, Game, InputBlocker, InputFlags, overlay::Overlay, utils::PopupModalExt};

/// A wrapper around the rest of the mod's UI that doesn't expect any state to
/// exist. This allows the full [Overlay] to assume that its [Core] exists while
/// still using Hudhook and ImGui to surface fatal errors that may occur during
/// initialization.
pub(crate) struct ErrorDisplay<G: Game> {
    /// The struct that's used to block and unblock input going to the game.
    input_blocker: G::InputBlocker,

    /// The main overlay if it managed to initialize correctly, or [None]
    /// otherwise.
    overlay: Option<Overlay<G>>,

    /// The core game logic. Used to extract fatal errors to display to the
    /// user.
    core: Option<Arc<Mutex<G::Core>>>,

    /// A fatal error to display. Once set, this can't be changed, even if other
    /// fatal errors are detected later.
    error: Option<Error>,

    /// Whether to display the full error information or just the summary.
    show_full_error: bool,
}

impl<G: Game> ErrorDisplay<G> {
    /// Creates a new [ErrorDisplay].
    pub fn new(core: Result<Arc<Mutex<G::Core>>>, input_blocker: G::InputBlocker) -> Self {
        match core {
            Ok(core) => Self {
                input_blocker,
                overlay: Some(Overlay::new()),
                core: Some(core),
                error: None,
                show_full_error: false,
            },
            Err(error) => Self {
                input_blocker,
                overlay: None,
                core: None,
                error: Some(error),
                show_full_error: false,
            },
        }
    }

    /// Displays a fatal error to the user if one is set.
    fn render_error(&mut self, ui: &mut Ui) {
        let Some(error) = &self.error else { return };

        // Make sure the cursor is visible even if the player is loaded into a
        // save with the menu closed.
        //
        // Safety: This is only ever run on the main thread.
        unsafe {
            G::force_cursor_visible();
        }

        unsafe {
            imgui_sys::igSetNextWindowSize(
                [800., if self.show_full_error { 500. } else { 0. }].into(),
                Condition::Always as i32,
            );
        }

        ui.open_popup("#fatal-error");
        ui.modal_popup_config("#fatal-error")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .size(
                [800., if self.show_full_error { 500. } else { 0. }],
                Condition::Always,
            )
            .build(|| {
                ui.checkbox("Show full error", &mut self.show_full_error);
                ui.text_wrapped(if self.show_full_error {
                    format!("{:?}", error)
                } else {
                    error.to_string()
                });

                ui.separator();
                if ui.button("Exit") {
                    std::process::exit(1);
                }
            });
    }
}

/// Which input classes the game must stop seeing this frame. Pure, so the truth table is testable
/// off Windows -- the crate that owns the hooks is not.
///
/// The pad follows the KEYBOARD rather than "mouse and keyboard both captured", which is what it
/// used to require. A controller player can essentially never satisfy that: they have no cursor
/// over the window, so `want_capture_mouse` is false and the pad drove the character in every state
/// except typing-with-the-mouse-hovering. Same defect as the keyboard one, one device over.
fn input_flags(want_mouse: bool, want_keyboard: bool, overlay_focused: bool) -> InputFlags {
    let mut flag = InputFlags::empty();
    if want_mouse {
        flag |= InputFlags::Mouse;
    }
    if want_keyboard || overlay_focused {
        flag |= InputFlags::Keyboard;
    }
    if flag.contains(InputFlags::Keyboard) {
        flag |= InputFlags::GamePad;
    }
    flag
}

impl<G: Game> ImguiRenderLoop for ErrorDisplay<G> {
    fn render(&mut self, ui: &mut Ui) {
        if let Some(core) = &mut self.core {
            let mut core = core.lock().unwrap();
            if let Some(overlay) = &mut self.overlay {
                overlay.render(ui, &mut core);
            }

            if self.error.is_none() {
                self.error = core.base_mut().take_error();
            }
        }

        self.render_error(ui);

        // ---- INPUT BLOCKING, and it runs AFTER the frame's windows are submitted ----------------
        //
        // WHY `is_focused()` AND NOT JUST `want_capture_keyboard` (Alaric, 2026-08-13: "input still
        // bleeds through to the game even when client has focus"). `want_capture_keyboard` is
        // imgui's "a text field wants these keystrokes" -- NOT "the overlay is focused". So with the
        // overlay open, on top and clicked into, but no field active, it is FALSE, the Keyboard flag
        // was never set, and every hook in eldenring-archipelago::input passed the real state
        // through: WASD walked the character while the player was reading their item list.
        //
        // The module's own motivating case ("typing `!markerprobe` no longer walks/rolls your
        // character") is the CONSOLE, where a text field IS focused. That is why that case worked
        // and this one did not, and why it went unnoticed for so long.
        //
        // 🛑 THIS COSTS YOU MOVEMENT WHILE THE OVERLAY IS FOCUSED, deliberately, and Alaric accepted
        // that trade explicitly. Click the overlay and you cannot dodge until you click away or hide
        // it (F5). The alternative -- a modal that steals your keys only sometimes -- is what this
        // is fixing.
        //
        // ⭐ IT CANNOT LOCK YOU OUT of the overlay's own hotkeys. F5/F6 and the rest are read with
        // `ui.is_key_pressed`, i.e. from imgui's io, which hudhook fills from WM_KEYDOWN -- a path
        // this blocker does not touch. It only changes what the GAME sees.
        //
        // The block is computed here rather than at the top of the frame so `want_capture_*` and
        // `is_focused()` both describe THIS frame instead of the previous one. They used to be a
        // frame stale together; now they are current together, which matters most on the frame the
        // overlay opens.
        let overlay_focused = self.overlay.as_ref().is_some_and(|o| o.is_focused());
        let io = ui.io();
        self.input_blocker.block_only(input_flags(
            io.want_capture_mouse,
            io.want_capture_keyboard,
            overlay_focused,
        ));
    }

    fn initialize<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        ctx.set_clipboard_backend(crate::clipboard::WindowsClipboardBackend {});
    }

    fn before_render<'a>(
        &'a mut self,
        ctx: &mut Context,
        render_context: &'a mut dyn RenderContext,
    ) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.before_render(ctx, render_context);
        } else {
            // Set the font scale here to match the overlay's logic.
            ctx.io_mut().font_global_scale = 1.8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE 11 MOTIVATING CASE. Alaric, 2026-08-13: "input still bleeds through to the game even
    /// when client has focus". The overlay was open, on top and clicked into, with no text field
    /// active -- so `want_capture_keyboard` was false and WASD walked his character.
    #[test]
    fn a_focused_overlay_blocks_the_keyboard_with_no_text_field() {
        let f = input_flags(false, false, true);
        assert!(f.contains(InputFlags::Keyboard));
        assert!(f.contains(InputFlags::GamePad));
    }

    /// The case that always worked, and the reason the one above went unnoticed: the console has a
    /// text field, so imgui asked for the keys itself.
    #[test]
    fn a_text_field_still_blocks_the_keyboard_on_its_own() {
        assert!(input_flags(false, true, false).contains(InputFlags::Keyboard));
    }

    /// 🛑 THE PAD USED TO NEED BOTH. A controller player has no cursor over the window, so
    /// `want_capture_mouse` is false and the old rule left the pad driving the character while they
    /// read their item list.
    #[test]
    fn the_pad_no_longer_needs_the_mouse_to_be_captured() {
        assert!(input_flags(false, true, false).contains(InputFlags::GamePad));
        assert!(input_flags(false, false, true).contains(InputFlags::GamePad));
    }

    /// 🛑 AND THE GAME KEEPS ITS INPUT WHEN NOTHING OF OURS WANTS IT. This is the assertion that
    /// stops the fix from becoming "the overlay eats everything forever" -- an unfocused, unhovered
    /// overlay must block NOTHING, or the player cannot play with the client open at all.
    #[test]
    fn an_unfocused_overlay_blocks_nothing() {
        assert_eq!(input_flags(false, false, false), InputFlags::empty());
    }

    /// The mouse is still its own question: hovering an overlay window takes the cursor without
    /// taking the keyboard, which is what makes the client usable mid-fight.
    #[test]
    fn hovering_takes_the_mouse_only() {
        let f = input_flags(true, false, false);
        assert!(f.contains(InputFlags::Mouse));
        assert!(!f.contains(InputFlags::Keyboard));
        assert!(!f.contains(InputFlags::GamePad));
    }
}
