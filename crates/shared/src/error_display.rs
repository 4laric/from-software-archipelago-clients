use std::sync::{Arc, Mutex};

use anyhow::{Error, Result};
use hudhook::{ImguiRenderLoop, MessageFilter, RenderContext};
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
fn input_flags(want_mouse: bool, want_keyboard: bool, keyboard_surface_active: bool) -> InputFlags {
    let mut flag = InputFlags::empty();
    if want_mouse {
        flag |= InputFlags::Mouse;
    }
    if want_keyboard || keyboard_surface_active {
        flag |= InputFlags::Keyboard;
    }
    if flag.contains(InputFlags::Keyboard) {
        flag |= InputFlags::GamePad;
    }
    flag
}

/// Window messages the game must not receive while an imgui surface owns input.
///
/// The per-game [`InputBlocker`] remains the primary path for polled input. This is the other half:
/// menus can consume `WM_KEY*`, mouse, or `WM_INPUT` directly from the window procedure without
/// touching XInput/DirectInput/GetKeyState. Hudhook has already copied every message into imgui's
/// queue before applying this filter, so swallowing it here affects only the game behind the
/// overlay.
fn window_message_filter(inputs: InputFlags) -> MessageFilter {
    let mut filter = MessageFilter::empty();
    if inputs.contains(InputFlags::Keyboard) {
        filter |= MessageFilter::InputKeyboard | MessageFilter::InputRaw;
    }
    if inputs.contains(InputFlags::Mouse) {
        // Do not add InputRaw for a mere hover. Hudhook cannot distinguish raw keyboard from raw
        // mouse messages, so doing so would quietly steal movement keys whenever the cursor crossed
        // the ordinary overlay. A keyboard-owning surface above takes InputRaw deliberately.
        filter |= MessageFilter::InputMouse;
    }
    filter
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
        // TWO TERMS, BOTH NARROW, ORed:
        //
        //   * `want_capture_keyboard` -- imgui's "a text field wants these keystrokes". Covers the
        //     say input in the main window, and the console input, for free.
        //   * `blocks_keyboard()` -- OUR keyboard surfaces re-asserted this frame: the dev console
        //     while focused, the connect modal while it is up. See `keyboard_surface_active`.
        //
        // 🛑 WHAT THIS NO LONGER DOES (#196 -> Alaric, 2026-08-14). It used to add
        // `Overlay::is_focused()` -- "the main window has focus" -- on the theory that a
        // clicked-into overlay should never leak keys. Too wide: that is true while the player is
        // merely READING their item list, so clicking the overlay cost them their dodge for no
        // benefit, and via #202 could cost it permanently. The surfaces that actually want your
        // keys are the ones you TYPE into, and they are enumerable, so they are enumerated.
        //
        // ⭐ #196's real motivating case survives, sharper: the CONNECT MODAL. That was the
        // report ("mostly change connection"), and `want_capture_keyboard` does not cover it --
        // the modal is up but no field is focused until you click one, so arrows and the pad drove
        // the character behind the dialog. `blocks_keyboard()` claims while the modal draws.
        //
        // ⭐ The console claims explicitly too rather than riding `want_capture_keyboard`, because
        // whether that ever fired for it was never confirmed (Alaric: "i'm not 100% sure it was
        // working for console"). Two independent terms, either sufficient.
        //
        // ⭐ IT CANNOT LOCK YOU OUT of the overlay's own hotkeys. F5/F6 and the rest are read with
        // `ui.is_key_pressed`, i.e. from imgui's io, which hudhook fills from WM_KEYDOWN -- a path
        // this blocker does not touch. It only changes what the GAME sees.
        //
        // The block is computed here rather than at the top of the frame so both terms describe
        // THIS frame. `keyboard_surface_active` is zeroed at the top of `Overlay::render` and
        // re-asserted by whichever surface drew, so unlike #202's flag it cannot outlive its
        // window.
        let keyboard_surface_active = self.overlay.as_ref().is_some_and(|o| o.blocks_keyboard());
        let cursor_capture_active = self.overlay.as_ref().is_some_and(|o| o.blocks_mouse());
        let io = ui.io();
        self.input_blocker.block_only(input_flags(
            io.want_capture_mouse || cursor_capture_active,
            io.want_capture_keyboard,
            keyboard_surface_active,
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

    fn message_filter(&self, io: &Io) -> MessageFilter {
        let keyboard_surface_active = self.overlay.as_ref().is_some_and(|o| o.blocks_keyboard());
        let cursor_capture_active = self.overlay.as_ref().is_some_and(|o| o.blocks_mouse());
        window_message_filter(input_flags(
            io.want_capture_mouse || cursor_capture_active,
            io.want_capture_keyboard,
            keyboard_surface_active,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE 11 MOTIVATING CASE, RESTATED 2026-08-14. The 2026-08-13 report behind #196 was "input
    /// still bleeds through to the game even when client has focus", and it was answered with "the
    /// main window is focused". Alaric narrowed it: "mostly change connection". So the surface that
    /// must block with NO text field focused is the connect modal, which is up and owns interaction
    /// before the player has clicked into Server or Slot -- precisely when `want_capture_keyboard`
    /// is still false and arrows and the pad reached the character behind it.
    #[test]
    fn a_keyboard_surface_blocks_with_no_text_field_focused() {
        let f = input_flags(false, false, true);
        assert!(f.contains(InputFlags::Keyboard));
        assert!(f.contains(InputFlags::GamePad));
    }

    /// 🛑 THE COST #196 CHARGED AND THIS STOPS CHARGING. Reading your item list is not typing: the
    /// main window can be focused, clicked into and on top, and the game keeps its keys. This is
    /// assertion that fails if anyone reintroduces `Overlay::is_focused()` at the call site -- the
    /// only reason it can be written at all is that "the overlay is focused" is no longer an
    /// argument to this function.
    #[test]
    fn reading_the_item_list_does_not_cost_you_your_dodge() {
        assert!(input_flags(false, false, false).is_empty());
        // ...and hovering it for the scrollbar still takes only the mouse.
        let f = input_flags(true, false, false);
        assert!(!f.contains(InputFlags::Keyboard));
        assert!(!f.contains(InputFlags::GamePad));
    }

    /// The say input and the console input are real imgui text fields, so imgui asks for the keys
    /// itself and this term alone is enough for them.
    ///
    /// 🛑 It is NOT trusted as the console's only cover. Whether it ever actually fired there was
    /// never confirmed -- Alaric, 2026-08-14: "i'm not 100% sure it was working for console" -- so
    /// the console ALSO claims explicitly while focused. Two independent terms, either sufficient;
    /// this test pins the imgui one, `a_keyboard_surface_blocks_with_no_text_field_focused` pins
    /// ours, and neither can quietly become load-bearing alone.
    #[test]
    fn a_text_field_still_blocks_the_keyboard_on_its_own() {
        assert!(input_flags(false, true, false).contains(InputFlags::Keyboard));
    }

    /// 🛑 THE PAD USED TO NEED BOTH. A controller player has no cursor over the window, so
    /// `want_capture_mouse` is false and the old rule left the pad driving the character while the
    /// connect dialog was up in front of them.
    #[test]
    fn the_pad_no_longer_needs_the_mouse_to_be_captured() {
        assert!(input_flags(false, true, false).contains(InputFlags::GamePad));
        assert!(input_flags(false, false, true).contains(InputFlags::GamePad));
    }

    /// 🛑 AND THE GAME KEEPS ITS INPUT WHEN NOTHING OF OURS WANTS IT. This is the assertion that
    /// stops the fix from becoming "the overlay eats everything forever" -- an overlay with no
    /// keyboard surface up must block NOTHING, or the player cannot play with the client open.
    #[test]
    fn an_unfocused_overlay_blocks_nothing() {
        // `is_empty()`, not `assert_eq!(.., InputFlags::empty())`: the real `InputFlags` derives
        // only Debug/Clone/Copy, so `==` does not exist on it and assert_eq! will not compile.
        // It did compile against a hand-written stub while proving this test out off Windows --
        // 🛑 A STUB MORE CAPABLE THAN THE REAL TYPE IS A TEST THAT ONLY PASSES LOCALLY, and this is
        // the one that got through. Mirror the real derives when stubbing, never improve on them.
        assert!(input_flags(false, false, false).is_empty());
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

    /// The title menu consumes window messages directly, bypassing ER's polled-input hooks. The
    /// connect modal must therefore swallow keyboard and raw-input messages at the WndProc too.
    #[test]
    fn a_keyboard_surface_filters_the_title_menu_message_path() {
        let f = window_message_filter(input_flags(false, false, true));
        assert!(f.contains(MessageFilter::InputKeyboard));
        assert!(f.contains(MessageFilter::InputRaw));
        assert!(!f.contains(MessageFilter::InputMouse));
    }

    /// Keep #196's boundary: merely displaying/reading the overlay does not steal the game's
    /// window messages. Only a real input-owning surface enables the WndProc filter.
    #[test]
    fn ordinary_overlay_viewing_filters_no_window_messages() {
        assert!(window_message_filter(input_flags(false, false, false)).is_empty());
    }

    /// Hudhook's raw-input filter is all-or-nothing. Hovering the ordinary overlay can take mouse
    /// messages, but must not take raw input because that may also contain gameplay keyboard data.
    #[test]
    fn hovering_does_not_filter_raw_input() {
        let f = window_message_filter(input_flags(true, false, false));
        assert!(f.contains(MessageFilter::InputMouse));
        assert!(!f.contains(MessageFilter::InputRaw));
        assert!(!f.contains(MessageFilter::InputKeyboard));
    }
}
