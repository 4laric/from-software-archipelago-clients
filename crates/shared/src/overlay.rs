#[cfg(feature = "profile")]
use std::time::Duration;
use std::time::Instant;
use std::{marker::PhantomData, mem, ptr};

use archipelago_rs::{self as ap, RichText, TextColor};
use hudhook::RenderContext;
use imgui::*;
use imgui_sys::igSetWindowFocus_Str;
use log::*;
use regex_macro::regex;

use crate::{Core, Game, prof};

mod text_input_history;

use text_input_history::TextInputHistory;

/// The duration between debug prints of the frame timing data.
#[cfg(feature = "profile")]
const TIME_PER_FRAME_PRINT: Duration = Duration::from_secs(10);

const GREEN: ImColor32 = ImColor32::from_rgb(0x8A, 0xE2, 0x43);
const RED: ImColor32 = ImColor32::from_rgb(0xFF, 0x44, 0x44);
const WHITE: ImColor32 = ImColor32::from_rgb(0xFF, 0xFF, 0xFF);
// This is the darkest gray that still meets WCAG guidelines for contrast with
// the black background of the overlay.
const BLACK: ImColor32 = ImColor32::from_rgb(0x9C, 0x9C, 0x9C);
const YELLOW: ImColor32 = ImColor32::from_rgb(0xFC, 0xE9, 0x4F);
const BLUE: ImColor32 = ImColor32::from_rgb(0x82, 0xA9, 0xD4);
const MAGENTA: ImColor32 = ImColor32::from_rgb(0xBF, 0x9B, 0xBC);
const CYAN: ImColor32 = ImColor32::from_rgb(0x34, 0xE2, 0xE2);

/// The visual overlay that appears on top of the game.
pub struct Overlay<G: Game> {
    /// The last-known size of the viewport. This is only set once hudhook has
    /// been initialized and the viewport has a non-zero size.
    viewport_size: Option<[f32; 2]>,

    /// The URL field in the modal connection popup.
    popup_url: String,

    /// The slot (player name) field in the modal connection popup.
    popup_slot: String,

    /// The password field in the modal connection popup.
    popup_password: String,

    /// Whether we've already auto-opened the connect prompt this session. Used
    /// to show the connect form once on a fresh (unconfigured) install without
    /// fighting the player if they close it.
    auto_prompted: bool,

    /// Set by the menu-bar "Connection" item to request re-opening the connect
    /// modal. `open_popup` must run at the SAME imgui ID-stack scope where the
    /// modal is begun (the window root) -- calling it from inside `menu_bar`
    /// scopes the popup to the menu bar and it silently never shows. So the menu
    /// item only raises this flag, and the root-scope render loop does the open.
    open_connect_requested: bool,

    /// The text the user typed in the say input.
    say_input: String,

    /// The history of messages sent to the say input.
    say_history: TextInputHistory,

    /// STICKY bottom-follow for the log window. `true` = keep pinning to the newest line;
    /// cleared ONLY by the user deliberately scrolling UP (wheel or scrollbar), re-armed when
    /// they return to the bottom. Sticky rather than measured-per-frame because a BURST of
    /// lines (a boss sweep pays 26+ checks in one pass) grows `scroll_max_y` faster than the
    /// old at-bottom check could observe: the check read false on the burst frame, the latch
    /// died, and the console silently stopped following -- 4laric, 2026-08-21, "it stops being
    /// at the bottom and new ones stop showing up", a long-standing report.
    log_stick_to_bottom: bool,

    /// Where the sticky pin last left the scroll (last frame's `scroll_max_y`). The unstick
    /// test compares the CURRENT scroll against this: content growth never moves the view
    /// above the previous pin, so `scroll_y < last_pin` can only mean the USER scrolled up.
    log_last_pin: f32,

    /// The time of the most recent log we've seen. This is used to determine
    /// when new logs are emitted for [frames_since_new_logs].
    last_log_emitted: Instant,

    /// The number of frames that have elapsed since new logs were last added.
    /// We use this to determine when to auto-scroll the log window.
    frames_since_new_logs: u64,

    /// The current font scale for the overlay UI.
    font_scale: f32,

    /// The unfocused window opacity for the overlay UI.
    unfocused_window_opacity: f32,

    /// Whether the client's own windows (main + settings + console) are drawn. Toggled by F5;
    /// forced back on whenever we are not connected. The rule is [`next_main_window_visible`].
    main_window_visible: bool,

    /// Whether the settings window is currently visible.
    settings_window_visible: bool,

    /// Whether the dev console window (menu bar → Console) is currently visible.
    console_window_visible: bool,
    /// Set by whichever of OUR keyboard surfaces drew and claimed the keys THIS frame -- the dev
    /// console while focused, and the connect modal whenever it is up. Zeroed unconditionally at
    /// the top of every [`Overlay::render`], so unlike `was_window_focused` it cannot outlive its
    /// window: the surface that wants your keys has to re-assert it on every frame it wants them.
    /// Read through [`Overlay::blocks_keyboard`]. See #202 for why that property is not optional.
    keyboard_surface_active: bool,

    /// The text the user typed in the dev console input (separate buffer from `say_input` so both
    /// can exist without sharing state).
    console_input: String,

    /// Whether to focus the console input on the next frame (keeps focus after pressing enter).
    focus_console_input_next_frame: bool,

    /// Whether the game was on the main menu in the previous frame.
    was_main_menu: bool,

    /// Whether the overlay window was focused in the previous frame.
    was_window_focused: bool,

    /// Whether compact mode was enabled in the previous frame.
    was_compact_mode: bool,

    /// Whether to focus the say input on the next frame. Used to keep focus
    /// after the user pressed enter.
    focus_say_input_next_frame: bool,

    /// The size of the main overlay window in the previous frame. Used to
    /// resize when entering and exiting compact mode.
    previous_size: Option<[f32; 2]>,

    /// The time the last profile data was printed.
    #[cfg(feature = "profile")]
    last_profile_printed: Instant,

    /// This allows us to associate a [Game] with the overlay as a whole rather
    /// than having to pass it to each method.
    _marker: PhantomData<G>,
}

// Safety: The sole Overlay instance is owned by Hudhook, which only ever
// interacts with it during frame rendering. We know the games' frame rendering
// always happens on the main thread, and never in parallel, so synchronization
// is not a real concern.
unsafe impl<G: Game> Sync for Overlay<G> {}

/// The F5 hide/show rule for the client's own windows, as a pure function so it can be tested
/// without a game, a frame, or a Windows machine.
///
/// F5 hides the main window, the settings window and the dev console. It deliberately does NOT
/// touch the game-specific windows (`Core::render_overlay_windows`): the ER item tracker has its
/// own F6, and the toasts are the ONLY feedback channel for grants the game itself cannot announce
/// (flask rungs and friends — see `er_logic::toast`), so hiding them would turn a working feature
/// back into an invisible one. The fatal-error modal is drawn by `ErrorDisplay`, outside this
/// type, and is likewise unaffected.
///
/// Two things constrain this beyond a bool flip:
///
/// * **Disconnected FORCES visible.** The fresh-install auto-prompt and the menu bar's
///   "Connection" item both open `#connect-modal` from inside the main window's body, at its root
///   ID scope. Skip the main window and there is no way to connect and no prompt on a fresh
///   install — the overlay is not merely hidden, it is unreachable. (Collapsing the window instead
///   of skipping it does not help: imgui does not run a collapsed window's body either.) So F5 is
///   a "get this out of my screenshot" key for an established session, and is inert while
///   disconnected.
/// * **Hiding must stay cosmetic, and is.** The mod's logic runs on the recurring task spawned in
///   [`crate::initialize`], not from the render loop, so a hidden overlay still reconciles, grants
///   and reports checks. Nothing may be gated on this flag except drawing.
///
/// F5 is a function key for the same reason the tracker's F6 is: a plain letter would fight the
/// say input, which is a live text field whenever the overlay has focus.
fn next_main_window_visible(current: bool, f5_pressed: bool, disconnected: bool) -> bool {
    // `^` is the toggle: a frame with no F5 leaves `current` alone.
    (current ^ f5_pressed)
        // Never leave the player without the connect UI.
        || disconnected
}

/// Whether the client's own window holds focus **for input-blocking purposes**, as a pure function
/// for the same reason [`next_main_window_visible`] is one.
///
/// 🛑 `was_window_focused` IS ONLY WRITTEN WHILE THE MAIN WINDOW IS DRAWN. `render_main_window` is
/// its sole writer, and [`Overlay::render`] calls that only when `main_window_visible`. So hiding a
/// FOCUSED overlay -- F5, or the menu bar's "Hide (F5)" -- freezes the field at `true` for as long
/// as it stays hidden.
///
/// That is not cosmetic, because `crate::error_display` arms the game's input blocker from
/// [`Overlay::is_focused`]: a stale `true` blocks the keyboard AND the pad in-game for good, and
/// the only way out is F5 again plus a click elsewhere -- the one path that reaches the writer.
/// error_display's own comment names F5 as the REMEDY ("you cannot dodge until you click away or
/// hide it (F5)"); it was the trigger.
///
/// The regression came from one crate over. Before #196 the blocker asked
/// `io.want_capture_keyboard`, which imgui recomputes every frame, so a hidden window could not
/// wedge it. #196 swapped in a field that is only refreshed when a particular window renders, and
/// its sibling comment claims `want_capture_*` and `is_focused()` "both describe THIS frame" --
/// true of the first, not of the second once the window stops being drawn.
///
/// Gating the ACCESSOR on visibility, not zeroing the field at the hide site, is on purpose:
/// there is then no frame on which a reader can observe the stale value at all, so the next hide
/// path added cannot reintroduce this. It restores the invariant [`next_main_window_visible`]
/// already states -- "Hiding must stay cosmetic ... Nothing may be gated on this flag except
/// drawing."
fn effective_focus(main_window_visible: bool, was_window_focused: bool) -> bool {
    main_window_visible && was_window_focused
}

impl<G: Game> Overlay<G> {
    /// Creates a new instance of the overlay and the core mod logic.
    pub fn new() -> Self {
        Self {
            font_scale: 1.8,
            unfocused_window_opacity: 0.4,
            was_compact_mode: true,
            main_window_visible: true,

            // Default values. We can't use [Default::default] because G doesn't
            // require `Default`.
            viewport_size: None,
            popup_url: Default::default(),
            popup_slot: Default::default(),
            popup_password: Default::default(),
            auto_prompted: false,
            open_connect_requested: false,
            say_input: Default::default(),
            say_history: Default::default(),
            log_stick_to_bottom: true,
            log_last_pin: 0.0,
            last_log_emitted: Instant::now(),
            frames_since_new_logs: 0,
            settings_window_visible: false,
            console_window_visible: false,
            keyboard_surface_active: false,
            console_input: Default::default(),
            focus_console_input_next_frame: false,
            was_main_menu: false,
            was_window_focused: false,
            focus_say_input_next_frame: false,
            previous_size: None,
            #[cfg(feature = "profile")]
            last_profile_printed: Instant::now(),
            _marker: PhantomData,
        }
    }

    /// Like [ImguiRenderLoop::render], but takes a reference to [Core] as well.
    ///
    /// We don't store `core` directly in the overlay so that we can ensure that
    /// its mutex is only locked once per render.
    /// Was the client's own window focused on the frame just drawn (root **and** child windows)?
    ///
    /// 🛑 PRESENTATION ONLY -- window opacity, and nothing else. **Do not arm the input blocker
    /// from this.** It used to, and "the main window has focus" is far too wide a question for
    /// that: it is true while you are merely READING your item list, which cost the player their
    /// movement for no benefit. [`Overlay::blocks_keyboard`] is the narrow question the blocker
    /// wants.
    ///
    /// ⭐ Both opacity readers go through HERE rather than touching `was_window_focused` directly,
    /// even though they run inside `render_main_window` where the two are identical. That keeps
    /// [`effective_focus`] -- and with it #202's guard -- live and load-bearing, so the next reader
    /// added outside the draw path inherits the fix instead of rediscovering the bug.
    ///
    /// Collapsed counts as NOT focused (`render` zeroes it), which is what you want: a collapsed
    /// title bar should not eat your movement keys. HIDDEN counts as not focused for the same
    /// reason and a sharper one -- see [`effective_focus`], which is where that rule lives.
    pub fn is_focused(&self) -> bool {
        effective_focus(self.main_window_visible, self.was_window_focused)
    }

    /// Whether one of OUR keyboard surfaces claimed the keys on the frame just drawn -- the dev
    /// console while focused, or the connect modal while it is up.
    ///
    /// This is deliberately NARROWER than "the overlay is focused" (see [`Overlay::is_focused`]).
    /// The say input needs no entry here: it is a real imgui text field, so `want_capture_keyboard`
    /// covers it at the call site, and the blocker ORs the two.
    pub fn blocks_keyboard(&self) -> bool {
        self.keyboard_surface_active
    }

    pub fn render(&mut self, ui: &mut Ui, core: &mut G::Core) {
        prof!(core.base_mut().profiler(), "AP overlay", {
            // 🛑 UNCONDITIONAL, and FIRST. Every keyboard surface re-asserts below if it still wants
            // the keys, so a surface that stops being drawn stops blocking on the very next frame.
            // This is the property #202 lacked: a flag only a draw call refreshes, read by a
            // per-frame decision, blocks forever the moment its window goes away.
            self.keyboard_surface_active = false;

            self.main_window_visible = next_main_window_visible(
                self.main_window_visible,
                ui.is_key_pressed(Key::F5),
                core.base().is_disconnected(),
            );

            if self.main_window_visible {
                prof!(core.base_mut().profiler(), "main window", {
                    self.render_main_window(ui, core);
                });

                prof!(core.base_mut().profiler(), "settings window", {
                    self.render_settings_window(ui);
                });

                prof!(core.base_mut().profiler(), "console window", {
                    self.render_console_window(ui, core);
                });
            }

            // Game-specific overlay windows (e.g. the ER item tracker). Called at
            // frame scope — the hook opens its own `ui.window(...)`.
            prof!(core.base_mut().profiler(), "game windows", {
                core.render_overlay_windows(ui);
            });
        });

        #[cfg(feature = "profile")]
        {
            let now = Instant::now();
            if now.duration_since(self.last_profile_printed) >= TIME_PER_FRAME_PRINT {
                core.base_mut().profiler().report();
                self.last_profile_printed = now;
            }
        }
    }

    /// See [ImguiRenderLoop::before_render], but takes a reference to [Core] as
    /// well.
    pub fn before_render<'a>(
        &'a mut self,
        ctx: &mut Context,
        _render_context: &'a mut dyn RenderContext,
    ) {
        self.frames_since_new_logs += 1;
        self.viewport_size = match ctx.main_viewport().size {
            [0., 0.] => None,
            size => Some(size),
        };

        // Set the font scale here because we need the frame height later to
        // calculate the main window size, which depends on it.
        ctx.io_mut().font_global_scale = self.font_scale;
    }

    /// Render the primary overlay window and any popups it opens.
    fn render_main_window(&mut self, ui: &Ui, core: &mut G::Core) {
        let Some(viewport_size) = self.viewport_size else {
            return;
        };

        prof!(core.base_mut().profiler(), "set focus", {
            // By default, imgui doesn't remove focus when escape is pressed,
            // even though it does relinquish its claim to the mouse and
            // keyboard. Because we use focus to determine when to make the
            // overlay transparent, we want it to be removed more aggressivley,
            // so we do so manually.
            if ui.is_key_pressed(Key::Escape) ||
                // Also defocus the window any time the player loads into the
                // game. This ensures that controller players don't have to mess
                // with the keyboard and mouse just to get the overlay
                // unfocused.
                (self.was_main_menu && unsafe { !G::is_main_menu() })
            {
                unsafe { igSetWindowFocus_Str(ptr::null()) };
            }
        });

        let window_opacity = if self.is_focused() {
            1.0
        } else {
            self.unfocused_window_opacity
        };
        let mut bg_color = [0.0, 0.0, 0.0, window_opacity];
        let _bg = ui.push_style_color(StyleColor::WindowBg, bg_color);
        let _menu_bg = ui.push_style_color(StyleColor::MenuBarBg, bg_color);
        bg_color[3] = 1.0; // Popup backgrounds should always be fully opaque.
        let _popup_bg = ui.push_style_color(StyleColor::PopupBg, bg_color);

        let mut builder = ui
            .window(format!(
                "Archipelago Client {} [{}]###ap-client-overlay",
                G::CLIENT_BUILD,
                match core.base().connection_state_type() {
                    ap::ConnectionStateType::Connected => "Connected",
                    ap::ConnectionStateType::Connecting => "Connecting...",
                    ap::ConnectionStateType::Disconnected => "Disconnected",
                }
            ))
            .position([viewport_size[0] - 30., 30.], Condition::FirstUseEver)
            .position_pivot([1., 0.])
            .menu_bar(true);

        // When the menu opens or closes, add or remove space from the bottom of
        // the overlay for the message bar and horizontal scrollbar.
        let is_compact_mode = self.is_compact_mode(core);
        builder = match (self.previous_size, is_compact_mode, self.was_compact_mode) {
            (Some(size), true, false) => {
                let style = ui.clone_style();
                let remove_bottom_space = ui.frame_height() + style.window_padding[1];

                builder.size(
                    [size[0], size[1] - remove_bottom_space.ceil()],
                    Condition::Always,
                )
            }
            (Some(size), false, true) => {
                let style = ui.clone_style();
                let add_bottom_space = ui.frame_height() + style.window_padding[1];

                builder.size(
                    [size[0], size[1] + add_bottom_space.ceil()],
                    Condition::Always,
                )
            }
            _ => builder.size([viewport_size[0] * 0.4, 300.], Condition::FirstUseEver),
        };

        let focus_say_input = mem::take(&mut self.focus_say_input_next_frame);
        let collapsed = builder
            .build(|| {
                prof!(core.base_mut().profiler(), "menu bar", {
                    self.render_menu_bar(ui, core);
                });

                ui.separator();

                prof!(core.base_mut().profiler(), "log window", {
                    self.render_log_window(ui, core);
                });

                if !is_compact_mode {
                    if core.base().is_disconnected() {
                        prof!(core.base_mut().profiler(), "connection buttons", {
                            self.render_connection_buttons(ui, core);
                        });
                    } else {
                        prof!(core.base_mut().profiler(), "say input", {
                            self.render_say_input(ui, core, focus_say_input);
                        });
                    }
                }
                // On a fresh (unconfigured) install, open the connect form once so the
                // player is prompted for server/slot without having to hunt for a button.
                if !self.auto_prompted
                    && core.base().is_disconnected()
                    && !core.base().is_configured()
                {
                    // Seed the form from whatever the config already has (e.g. a slot from a
                    // partial apconfig.json) so the player only fills in what's missing.
                    let config = core.base().config();
                    config.url().clone_into(&mut self.popup_url);
                    config.slot().clone_into(&mut self.popup_slot);
                    self.popup_password = config.password().unwrap_or("").to_string();
                    ui.open_popup("#connect-modal");
                    self.auto_prompted = true;
                }
                // On-demand reopen from the menu-bar "Connection" item. Fired HERE (window-root
                // ID scope, the same scope render_connect_modal begins the modal in) rather than
                // inside menu_bar, so the popup id matches and it actually shows -- the fix for
                // the "Connection entry is dead once connected" gap.
                if mem::take(&mut self.open_connect_requested) {
                    ui.open_popup("#connect-modal");
                }
                prof!(core.base_mut().profiler(), "connect modal", {
                    self.render_connect_modal(ui, core);
                });

                self.was_window_focused =
                    ui.is_window_focused_with_flags(WindowFocusedFlags::ROOT_AND_CHILD_WINDOWS);
                self.previous_size = Some(ui.window_size());
            })
            .is_none();

        self.was_main_menu = unsafe { G::is_main_menu() };
        self.was_compact_mode = is_compact_mode;

        if collapsed {
            self.was_window_focused = false;
        }
    }

    /// Renders the modal popup which queries the player for connection
    /// information (server URL, slot, and optional password).
    fn render_connect_modal(&mut self, ui: &Ui, core: &mut G::Core) {
        ui.modal_popup_config("#connect-modal")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .always_auto_resize(true)
            .build(|| {
                // 🛑 UNCONDITIONAL, not focus-gated: this closure runs only while the modal is UP,
                // and a modal already owns all imgui interaction, so there is nothing else to be
                // focused. It is also the case Alaric actually reported in #196 ("mostly change
                // connection"), and it must hold BEFORE the player clicks into a field -- which is
                // exactly when `want_capture_keyboard` is still false and the old rule leaked
                // arrows and the pad through to the character.
                self.keyboard_surface_active = true;
                {
                    let _item_width = ui.push_item_width(500. * self.font_scale);
                    ui.input_text("Server", &mut self.popup_url)
                        .hint("archipelago.gg:12345")
                        .chars_noblank(true)
                        .build();
                    ui.input_text("Slot", &mut self.popup_slot)
                        .hint("Your player name")
                        .build();
                    ui.input_text("Password", &mut self.popup_password)
                        .password(true)
                        .build();
                }

                let incomplete = self.popup_url.is_empty() || self.popup_slot.is_empty();
                ui.disabled(incomplete, || {
                    if ui.button("Connect") {
                        ui.close_current_popup();
                        let password = if self.popup_password.is_empty() {
                            None
                        } else {
                            Some(self.popup_password.clone())
                        };
                        if let Err(e) = core.base_mut().update_connection_info(
                            &self.popup_url,
                            &self.popup_slot,
                            password,
                        ) {
                            error!("Failed to save config: {e}");
                        }
                    }
                });

                ui.same_line();
                if ui.button("Cancel") {
                    ui.close_current_popup();
                }
            });
    }

    /// Renders the menu bar.
    fn render_menu_bar(&mut self, ui: &Ui, core: &mut G::Core) {
        ui.menu_bar(|| {
            if ui.menu_item("Settings") {
                log::warn!("Click registered");
                self.settings_window_visible = true;
            }

            // Reopen the connect form on demand, even while connected, so the player can
            // switch server/slot without deleting apconfig.json. Seeds the fields from the
            // current config. We must NOT call open_popup here: this runs inside menu_bar's
            // ID scope, and a popup opened there never matches the modal begun at the window
            // root (render_connect_modal) -- the click registered but nothing showed. Instead
            // raise a flag the root-scope render loop consumes, next to the auto-prompt open.
            if ui.menu_item("Connection") {
                let config = core.base().config();
                config.url().clone_into(&mut self.popup_url);
                config.slot().clone_into(&mut self.popup_slot);
                self.popup_password = config.password().unwrap_or("").to_string();
                self.open_connect_requested = true;
            }

            // Dev command console — works even while disconnected (unlike the say box, which is
            // connection-gated). Home of `!markerprobe`, `!flag`, `!warp`, etc.
            if ui.menu_item("Console") {
                self.console_window_visible = true;
                self.focus_console_input_next_frame = true;
            }

            // The hide hotkey has to be discoverable from the UI itself: a player who never
            // reads the guide would otherwise not know F5 exists, and — worse — could not guess
            // it after hiding the overlay by accident, at which point the mod looks dead.
            if ui.menu_item("Hide (F5)") {
                self.main_window_visible = false;
            }

            // Game-specific menu items (e.g. the ER item tracker toggle).
            core.render_overlay_menu_items(ui);
        });
    }

    /// Renders the dev console window (menu bar → Console). Always usable — including while
    /// disconnected — so dev `!` commands (`!markerprobe`, `!flag`, ...) run without an AP session.
    /// Command output appears in the main overlay log window. Plain (non-`!`) text is only sent to
    /// the server when connected (see [`Self::say`]).
    fn render_console_window(&mut self, ui: &Ui, core: &mut G::Core) {
        if !self.console_window_visible {
            return;
        }
        ui.window("Console")
            .size([460.0, 130.0], Condition::FirstUseEver)
            .collapsible(false)
            .build(|| {
                // The console is a keyboard surface: you are here to TYPE. Claim on focus rather
                // than on merely-open, so a console left open and clicked away from does not sit on
                // the player's movement keys. `want_capture_keyboard` alone was not trusted here
                // -- Alaric, 2026-08-14, on whether it ever fired for the console: "i'm not 100%
                // sure it was working".
                if ui.is_window_focused_with_flags(WindowFocusedFlags::ROOT_AND_CHILD_WINDOWS) {
                    self.keyboard_surface_active = true;
                }
                ui.text("Dev console — output shows in the log window.");
                let usages = core.console_command_usages();
                if !usages.is_empty() {
                    ui.text_wrapped(usages.join(" · "));
                }
                ui.separator();

                if mem::take(&mut self.focus_console_input_next_frame) {
                    ui.set_keyboard_focus_here();
                }
                let width = ui.push_item_width(-1.0);
                let send = ui
                    .input_text("##console-input", &mut self.console_input)
                    .enter_returns_true(true)
                    .callback(InputTextCallback::HISTORY, &mut self.say_history)
                    .build();
                drop(width);

                if send {
                    let line = mem::take(&mut self.console_input);
                    self.say_history.add(line.clone());
                    self.say(line, core);
                    self.focus_console_input_next_frame = true;
                }

                if ui.button("Close") {
                    self.console_window_visible = false;
                }
            });
    }

    /// Renders the settings popup.
    fn render_settings_window(&mut self, ui: &Ui) {
        if !self.settings_window_visible {
            return;
        }

        let settings_bg_color = [0.0, 0.0, 0.0, 1.0];
        let _bg = ui.push_style_color(StyleColor::WindowBg, settings_bg_color);

        ui.window("Archipelago Overlay Settings")
            .size([0., 0.], Condition::Appearing)
            .position_pivot([0.5, 0.5])
            .collapsible(false)
            .build(|| {
                ui.text("Font Size ");
                ui.same_line();
                if ui.button("-##font-size-decrease-button") {
                    self.font_scale = (self.font_scale - 0.1).max(0.5);
                }
                ui.same_line();
                if ui.button("+##font-size-increase-button") {
                    self.font_scale = (self.font_scale + 0.1).min(4.0);
                }

                let mut opacity_percent = (self.unfocused_window_opacity * 100.0).round() as i32;
                let _slider_width = ui.push_item_width(150. * self.font_scale);
                ui.text("Unfocused Opacity ");
                ui.same_line();
                ui.slider_config("##unfocused-opacity-slider", 0, 100)
                    .display_format("%d%%")
                    .build(&mut opacity_percent);
                self.unfocused_window_opacity = (opacity_percent as f32) / 100.0;

                if ui.button("Ok") {
                    self.settings_window_visible = false;
                }
            });
    }

    /// Renders the buttons that allow the player to reconnect to Archipelago.
    /// These take the place of the text box when the client is disconnected.
    fn render_connection_buttons(&mut self, ui: &Ui, core: &mut G::Core) {
        if ui.button("Reconnect") {
            core.base_mut().reconnect();
        }

        ui.same_line();
        let button_label = if core.base().is_configured() {
            "Change connection"
        } else {
            "Connect"
        };
        if ui.button(button_label) {
            ui.open_popup("#connect-modal");
            let config = core.base().config();
            config.url().clone_into(&mut self.popup_url);
            config.slot().clone_into(&mut self.popup_slot);
            self.popup_password = config.password().unwrap_or("").to_string();
        }
    }

    /// Renders the log window which displays all the prints sent from the server.
    fn render_log_window(&mut self, ui: &Ui, core: &G::Core) {
        let style = ui.clone_style();

        let scrollbar_bg_opacity = if self.is_focused() { 1.0 } else { 0.0 };
        let scrollbar_bg_color = [0.0, 0.0, 0.0, scrollbar_bg_opacity];
        let _scrollbar_bg = ui.push_style_color(StyleColor::ScrollbarBg, scrollbar_bg_color);

        let _item_spacing = ui.push_style_var(StyleVar::ItemSpacing([
            style.item_spacing[0],
            style.window_padding[1],
        ]));

        let is_compact_mode = self.is_compact_mode(core);
        let input_height = if !is_compact_mode {
            ui.frame_height_with_spacing()
        } else {
            0.0
        };

        ui.child_window("#log")
            .size([0.0, -input_height.ceil()])
            .draw_background(false)
            .always_vertical_scrollbar(true)
            // Messages now wrap to the window width (see write_message_data), so
            // the horizontal scrollbar is no longer needed.
            .always_horizontal_scrollbar(false)
            .build(|| {
                if let Some((_, log_time)) = core.base().logs().last()
                    && log_time > &self.last_log_emitted
                {
                    self.frames_since_new_logs = 0;
                    self.last_log_emitted = *log_time;
                }

                // Render every log row directly instead of using a ListClipper.
                // The clipper assumes a fixed per-item height, but
                // write_message_data wraps long messages onto a variable number
                // of lines. That broken assumption made scroll_max_y unstable,
                // so the bottom check below almost never held and auto-scroll
                // silently stopped keeping the newest text in view.
                for (message, _) in core.base().logs() {
                    use ap::Print::*;
                    write_message_data(
                        ui,
                        message.data(),
                        // De-emphasize miscellaneous server prints.
                        match message {
                            Chat { .. }
                            | ServerChat { .. }
                            | Tutorial { .. }
                            | CommandResult { .. }
                            | AdminCommandResult { .. }
                            | Unknown { .. } => 0xff,
                            ItemSend { item, .. } | ItemCheat { item, .. } | Hint { item, .. }
                                if core.base().config().slot() == item.receiver().name()
                                    || core.base().config().slot() == item.sender().name() =>
                            {
                                0xFF
                            }
                            _ => 0xAA,
                        },
                    );
                }

                // STICKY FOLLOW (2026-08-21). The old latch measured "am I at the bottom"
                // AFTER rendering this frame's rows -- so the one frame a sweep dumps 26 lines,
                // scroll_max_y has already jumped, the check reads false while the view still
                // sits at the OLD bottom, and auto-scroll died exactly when the console most
                // needed to follow. The latch is sticky now, and only the USER can clear it:
                //   * content growth cannot -- growth never moves the view ABOVE last frame's
                //     pin, so `scroll_y < last_pin` is a reliable "the user scrolled up",
                //     whatever scroll_max_y did this frame (wheel and scrollbar drag both
                //     land here; the 1px epsilon eats sub-pixel rounding);
                //   * returning to the bottom re-arms it.
                // The frames_since_new_logs window is gone from this condition on purpose: it
                // existed to stop fighting the user, and the unstick test does that job
                // precisely instead of by timeout.
                let cur = ui.scroll_y();
                let max = ui.scroll_max_y();
                if self.log_stick_to_bottom {
                    if cur + 1.0 < self.log_last_pin.min(max) {
                        self.log_stick_to_bottom = false; // the user scrolled up: let them read
                    }
                } else if cur >= max - 1.0 {
                    self.log_stick_to_bottom = true; // they came back: resume following
                }
                if self.log_stick_to_bottom {
                    ui.set_scroll_y(max);
                    self.log_last_pin = max;
                }
            });
    }

    /// Renders the text box in which users can write chats to the server.
    ///
    /// If `focus` is true, this forces the input to be in focus.
    fn render_say_input(&mut self, ui: &Ui, core: &mut G::Core, focus: bool) {
        ui.disabled(core.client().is_none(), || {
            let arrow_button_width = ui.frame_height(); // Arrow buttons are square buttons.
            let style = ui.clone_style();
            let spacing = style.item_spacing[0] * self.font_scale * 0.7;

            let input_width = ui.push_item_width(-(arrow_button_width + spacing));
            if focus {
                ui.set_keyboard_focus_here();
            }
            let mut send = ui
                .input_text("##say-input", &mut self.say_input)
                .enter_returns_true(true)
                .callback(InputTextCallback::HISTORY, &mut self.say_history)
                .build();
            drop(input_width);

            ui.same_line_with_spacing(0.0, spacing);
            send = ui.arrow_button("##say-button", Direction::Right) || send;

            if send {
                // We don't have a great way to surface these errors, and
                // they're non-fatal, so just ignore them.
                let line = mem::take(&mut self.say_input);
                self.say_history.add(line.clone());
                self.say(line, core);
                self.focus_say_input_next_frame = true;
            }
        });
    }

    /// Handles a command from the player, falling back to sending it to the
    /// server.
    fn say(&mut self, message: String, core: &mut G::Core) {
        let Some(captures) = regex!("^(![^ ]+)( +)?(.*)?$").captures(message.trim()) else {
            // Plain chat: only reaches the server when connected. Dropped while offline.
            if let Some(client) = core.client_mut() {
                let _ = client.say(message);
            }
            return;
        };

        let command = captures.get(1).unwrap().as_str();
        let arg = captures.get(3).map(|c| c.as_str());
        if !core.handle_command(command, arg) {
            // Not a recognized `!` command — forward to the server as chat if connected.
            if let Some(client) = core.client_mut() {
                let _ = client.say(message);
            }
        }
    }

    /// Returns whether the overlay is currently in "compact mode", where the
    /// bottommost widgets are not rendered.
    fn is_compact_mode(&self, core: &G::Core) -> bool {
        // When the connection is inactive, always show the buttons to
        // reconnect.
        !core.base().is_disconnected() && unsafe { !G::is_menu_open() }
    }
}

trait ImColor32Ext {
    /// Returns a copy of [self] with its opacity overridden by [alpha].
    fn with_alpha(&self, alpha: u8) -> ImColor32;
}

impl ImColor32Ext for ImColor32 {
    fn with_alpha(&self, alpha: u8) -> ImColor32 {
        ImColor32::from_bits((self.to_bits() & 0x00ffffff) | ((alpha as u32) << 24))
    }
}

/// Writes the text in [parts] to [ui], wrapping onto new lines when a message
/// is wider than the available space in the log window.
///
/// imgui's built-in text wrapping only applies to a single `text` call, but our
/// messages are composed of several independently-colored [RichText] parts laid
/// out with `same_line`. So we wrap manually: we split each part into words and
/// track the running width of the current line, breaking to a new line whenever
/// the next word wouldn't fit.
fn write_message_data(ui: &Ui, parts: &[RichText], alpha: u8) {
    // Width to wrap at: the space from the current cursor to the right edge of
    // the content region (which already excludes the vertical scrollbar).
    let wrap_width = ui.content_region_avail()[0].max(1.0);
    // Gap between words. Using the width of a real space glyph keeps wrapped
    // lines spaced naturally regardless of the item-spacing style.
    let space_width = ui.calc_text_size(" ")[0];

    // Pixel width of the line currently being laid out.
    let mut line_width = 0.0f32;
    let mut first_word = true;

    for part in parts {
        // TODO: Load in fonts to support bold, maybe write a line manually for
        // underline? I'm not sure there's a reasonable way to support
        // background colors.
        use RichText::*;
        use TextColor::*;
        let color = match part {
            Player { .. } | PlayerName { .. } | Color { color: Blue, .. } => BLUE,
            Item { .. } | Color { color: Magenta, .. } => MAGENTA,
            Location { .. } | EntranceName { .. } | Color { color: Cyan, .. } => CYAN,
            Color { color: Black, .. } => BLACK,
            Color { color: Red, .. } => RED,
            Color { color: Green, .. } => GREEN,
            Color { color: Yellow, .. } => YELLOW,
            _ => WHITE,
        };
        let rgba = color.with_alpha(alpha).to_rgba_f32s();

        // `split(' ')` yields empty strings for consecutive/leading/trailing
        // spaces; skipping them collapses runs of whitespace to a single gap.
        for word in part.to_string().split(' ') {
            if word.is_empty() {
                continue;
            }
            let word_width = ui.calc_text_size(word)[0];

            if first_word {
                // First word of the message: render at the line start.
                ui.text_colored(rgba, word);
                line_width = word_width;
                first_word = false;
            } else if line_width + space_width + word_width > wrap_width {
                // Doesn't fit: drop to a new line (no `same_line`).
                ui.text_colored(rgba, word);
                line_width = word_width;
            } else {
                // Fits: continue the current line.
                ui.same_line_with_spacing(0.0, space_width);
                ui.text_colored(rgba, word);
                line_width += space_width + word_width;
            }
        }
    }

    // An empty message still needs to consume a row so the ListClipper layout
    // stays aligned.
    if first_word {
        ui.new_line();
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_focus, next_main_window_visible};

    #[test]
    fn f5_toggles_while_connected() {
        assert!(!next_main_window_visible(true, true, false));
        assert!(next_main_window_visible(false, true, false));
    }

    #[test]
    fn other_frames_leave_visibility_alone() {
        assert!(next_main_window_visible(true, false, false));
        assert!(!next_main_window_visible(false, false, false));
    }

    /// The motivating case: a hidden overlay must not be able to swallow the connect UI. Whatever
    /// the player last pressed, losing the connection brings the window back.
    #[test]
    fn disconnected_forces_the_window_back() {
        assert!(next_main_window_visible(false, false, true));
        assert!(next_main_window_visible(true, true, true));
        assert!(next_main_window_visible(false, true, true));
    }

    /// The motivating case (bobler, 2026-08-14, client 0.4.2 `edaeb3b`): "do you know why f5
    /// freezes my keyboard input". F5-hiding a FOCUSED overlay left `was_window_focused` frozen at
    /// `true`, and `error_display` went on arming the game's input blocker from it -- so the
    /// keyboard and pad stayed dead in-game until the window was shown again and clicked away from.
    /// Hidden must read as unfocused however stale the underlying field is.
    #[test]
    fn a_hidden_overlay_never_reads_as_focused() {
        assert!(!effective_focus(false, true));
        assert!(!effective_focus(false, false));
    }

    /// The other half: gating on visibility must not cost a VISIBLE overlay its real focus, or
    /// #196 is undone and WASD walks the character while the player reads their item list.
    #[test]
    fn a_visible_overlay_still_reports_its_own_focus() {
        assert!(effective_focus(true, true));
        assert!(!effective_focus(true, false));
    }
}
