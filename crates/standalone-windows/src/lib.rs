//! Standalone Windows-host policy shared by external Archipelago clients.
//!
//! This first slice deliberately owns no game or delivery state. A future renderer plugs into
//! [`client_ui::HostEndpoint`], while this crate owns only durable window behavior.

pub use client_ui;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowOptions {
    pub always_on_top: bool,
    pub click_through: bool,
    pub compact: bool,
    pub opacity: f32,
    pub geometry: WindowGeometry,
    /// Which activity classes the feed shows. Added after this file already shipped, so it is
    /// `#[serde(default)]`: a `client-window.json` written by an older build has no `filters` key
    /// and must still restore the player's geometry and opacity rather than being discarded and
    /// silently replaced by defaults.
    #[serde(default)]
    pub filters: FeedFilters,
}

/// Feed visibility toggles, persisted beside the window's own state.
///
/// These live in `WindowOptions` rather than in renderer-local state because they share its one
/// durable property: a player who hid chat wants it hidden on the next launch, and there is
/// already an atomic tmp+rename writer for exactly this file. Every field defaults to `true`, so
/// an unknown or absent value can only ever show more than the player asked for -- never hide a
/// delivery failure behind a stale saved toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedFilters {
    #[serde(default = "shown")]
    pub checks: bool,
    #[serde(default = "shown")]
    pub items: bool,
    #[serde(default = "shown")]
    pub chat: bool,
    #[serde(default = "shown")]
    pub system: bool,
}

const fn shown() -> bool {
    true
}

impl Default for FeedFilters {
    fn default() -> Self {
        Self {
            checks: true,
            items: true,
            chat: true,
            system: true,
        }
    }
}

impl FeedFilters {
    /// Whether one activity entry survives the current toggles.
    ///
    /// `system` deliberately covers commands, errors and parked deliveries together: they are the
    /// lines a player mutes while playing and un-mutes while diagnosing, and splitting errors out
    /// would let someone hide a failure while believing they had only hidden command echoes.
    pub fn admits(self, kind: &client_ui::ActivityKind) -> bool {
        match kind {
            client_ui::ActivityKind::LocationCheck => self.checks,
            client_ui::ActivityKind::ReceivedItem | client_ui::ActivityKind::StorageDelivery => {
                self.items
            }
            client_ui::ActivityKind::Message | client_ui::ActivityKind::Hint => self.chat,
            client_ui::ActivityKind::Command
            | client_ui::ActivityKind::CommandResult
            | client_ui::ActivityKind::ParkedDelivery
            | client_ui::ActivityKind::Error => self.system,
        }
    }
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            always_on_top: true,
            click_through: false,
            compact: false,
            opacity: 0.85,
            geometry: WindowGeometry::default(),
            filters: FeedFilters::default(),
        }
    }
}

impl WindowOptions {
    pub fn normalized(mut self, work_area: WindowGeometry) -> Self {
        self.opacity = self.opacity.clamp(0.2, 1.0);
        self.geometry.width = self.geometry.width.clamp(320, work_area.width.max(320));
        self.geometry.height = self.geometry.height.clamp(120, work_area.height.max(120));
        let max_x = work_area.x + work_area.width - self.geometry.width;
        let max_y = work_area.y + work_area.height - self.geometry.height;
        self.geometry.x = self.geometry.x.clamp(work_area.x, max_x);
        self.geometry.y = self.geometry.y.clamp(work_area.y, max_y);
        self
    }

    /// Win32 extended styles for the native shell. Layering is always enabled so background
    /// opacity works; click-through is opt-in and therefore reversible from ordinary compact
    /// mode.
    #[cfg(windows)]
    pub fn extended_style(self) -> windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE {
        use windows::Win32::UI::WindowsAndMessaging::{
            WS_EX_LAYERED, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
        };

        let mut style = WS_EX_LAYERED;
        if self.always_on_top {
            style |= WS_EX_TOPMOST;
        }
        if self.click_through {
            style |= WS_EX_TRANSPARENT;
        }
        style
    }

    pub fn alpha_byte(self) -> u8 {
        (self.opacity.clamp(0.2, 1.0) * 255.0).round() as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            x: 80,
            y: 80,
            width: 560,
            height: 720,
        }
    }
}

/// Platform-neutral contract implemented by the eventual native renderer. Keeping this contract
/// free of hudhook is the seam that allows external clients to coexist with injected overlays.
pub trait StandaloneHost {
    type Error;

    fn run(
        self,
        endpoint: client_ui::HostEndpoint,
        options: WindowOptions,
    ) -> Result<(), Self::Error>;
}

pub const CONTROL_STATUS: usize = 1001;
pub const CONTROL_EXPORT: usize = 1002;
pub const CONTROL_SESSION_FOLDER: usize = 1003;
pub const CONTROL_COMMAND_INPUT: usize = 1004;
pub const CONTROL_COMMAND_SEND: usize = 1005;

/// Keep the host a transport rather than a command authority. The worker still parses commands
/// and enforces confirmation/audit policy; this only prevents pasted text from becoming multiple
/// commands and bounds the message sent over the non-blocking channel.
pub fn normalize_command_input(input: &str) -> Option<String> {
    let command = input.replace(['\r', '\n'], " ");
    let command = command.trim();
    (!command.is_empty()).then(|| command.chars().take(512).collect())
}

/// Read persisted window state, or `None` when there is nothing usable on disk.
///
/// Every failure -- missing file, unreadable file, JSON from a build whose schema this one cannot
/// make sense of -- is the same answer: fall back to defaults. Window chrome is never worth
/// failing a client launch over.
pub fn load_options(path: &std::path::Path) -> Option<WindowOptions> {
    let bytes = std::fs::read(path).ok()?;
    json::from_slice::<WindowOptions>(&bytes).ok()
}

/// Persist window state through a temporary file and a rename.
///
/// The rename is the point: a client killed mid-write leaves either the previous state or the new
/// one, never a half-written file that the next launch silently discards.
pub fn store_options(path: &std::path::Path, options: &WindowOptions) -> std::io::Result<()> {
    let bytes = json::to_vec_pretty(options)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}

/// Map native controls onto the same bounded action channel used by every future renderer.
/// This slice intentionally exposes only read-only diagnostics and filesystem navigation.
pub fn control_action(id: usize) -> Option<client_ui::UiAction> {
    match id {
        CONTROL_STATUS => Some(client_ui::UiAction::SubmitCommand("status".into())),
        CONTROL_EXPORT => Some(client_ui::UiAction::SubmitCommand("export".into())),
        CONTROL_SESSION_FOLDER => Some(client_ui::UiAction::OpenSessionFolder),
        _ => None,
    }
}

/// Stable text projection used by the first native shell.  Keeping this reducer outside the
/// Win32 host makes the state presentation testable and gives a later ImGui renderer the same
/// source of truth.
pub fn render_snapshot(snapshot: &client_ui::ClientSnapshot) -> String {
    let identity = match (&snapshot.server, &snapshot.slot) {
        (Some(server), Some(slot)) => format!("{slot} @ {server}"),
        _ => "Waiting for Archipelago identity".to_owned(),
    };
    let mut lines = vec![
        "Bloodborne Archipelago".to_owned(),
        identity,
        format!("Game: {:?}  |  AP: {:?}", snapshot.process, snapshot.ap),
        format!("Delivery: {:?}", snapshot.delivery),
        format!(
            "Items: {} delivered, {} queued, {} storage, {} parked",
            snapshot.ledger.delivered,
            snapshot.ledger.queued,
            snapshot
                .ledger
                .storage_routed
                .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
            snapshot.ledger.parked
        ),
    ];
    if let Some(locations) = &snapshot.locations {
        lines.push(format!(
            "Checks: {} / {}",
            locations.checked, locations.total
        ));
    }
    if snapshot.stale {
        lines.push("WARNING: client state is stale".to_owned());
    }
    if let Some(goal) = &snapshot.goal {
        let go_mode = snapshot
            .go_mode
            .map_or("unknown", |value| if value { "yes" } else { "no" });
        lines.push(format!("Goal: {goal}  |  Go Mode: {go_mode}"));
    }
    if !snapshot.activity.is_empty() {
        lines.push(String::new());
        lines.push("Recent activity".to_owned());
        lines.extend(
            snapshot
                .activity
                .iter()
                .rev()
                .take(12)
                .rev()
                .map(|event| format!("{:?}: {}", event.kind, event.text)),
        );
    }
    lines.join("\r\n")
}

#[cfg(windows)]
mod native {
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::UpdateWindow;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetClientRect, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW,
        IsDialogMessageW, LWA_ALPHA, LoadCursorW, MSG, MoveWindow, PM_REMOVE, PeekMessageW,
        PostQuitMessage, RegisterClassW, SW_SHOW, SetLayeredWindowAttributes, SetWindowTextW,
        ShowWindow, TranslateMessage, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_KEYDOWN,
        WM_QUIT, WNDCLASSW, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
    };
    use windows::core::{HSTRING, Result as WindowsResult, w};

    use super::{
        CONTROL_COMMAND_INPUT, CONTROL_COMMAND_SEND, CONTROL_EXPORT, CONTROL_SESSION_FOLDER,
        CONTROL_STATUS, StandaloneHost, WindowGeometry, WindowOptions, client_ui, control_action,
        normalize_command_input, render_snapshot,
    };

    /// Dependency-light native shell. It owns only windowing and rendering; the game client owns
    /// networking, memory access and delivery on its existing worker thread.
    #[derive(Default)]
    pub struct NativeWindowHost {
        state_path: Option<PathBuf>,
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_CLOSE => {
                // SAFETY: `window` is supplied by the OS for this registered class.
                unsafe {
                    let _ = DestroyWindow(window);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                // SAFETY: posts to this UI thread's queue and does not dereference memory.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            // SAFETY: all unhandled messages retain the platform default behavior.
            _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
    }

    /// Submit the edit control as one bounded worker command. Kept in one helper so clicking
    /// Send and pressing Enter have exactly the same validation, queueing, and clear-on-success
    /// behavior.
    unsafe fn submit_command(
        command_input: HWND,
        endpoint: &client_ui::HostEndpoint,
    ) -> WindowsResult<()> {
        // SAFETY: `command_input` belongs to this UI thread and remains alive for the loop.
        unsafe {
            let length = GetWindowTextLengthW(command_input);
            let mut buffer = vec![0u16; (length + 1) as usize];
            let copied = GetWindowTextW(command_input, &mut buffer);
            let input = String::from_utf16_lossy(&buffer[..copied as usize]);
            if let Some(command) = normalize_command_input(&input)
                && endpoint
                    .send_action(client_ui::UiAction::SubmitCommand(command))
                    .is_ok()
            {
                SetWindowTextW(command_input, w!(""))?;
            }
        }
        Ok(())
    }

    impl StandaloneHost for NativeWindowHost {
        type Error = windows::core::Error;

        fn run(
            self,
            endpoint: client_ui::HostEndpoint,
            mut options: WindowOptions,
        ) -> Result<(), Self::Error> {
            // SAFETY: the class, windows and message queue are created and consumed on this
            // thread; handles are checked before use and destroyed by WM_CLOSE/process teardown.
            unsafe {
                let module = GetModuleHandleW(None)?;
                let instance = HINSTANCE(module.0);
                let class_name = w!("BloodborneArchipelagoStandalone");
                let class = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    hCursor: LoadCursorW(None, IDC_ARROW)?,
                    lpszClassName: class_name,
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    return Err(windows::core::Error::from_thread());
                }

                if let Some(path) = self.state_path.as_deref()
                    && let Ok(bytes) = std::fs::read(path)
                    && let Ok(saved) = json::from_slice::<WindowOptions>(&bytes)
                {
                    options = saved.normalized(WindowGeometry {
                        x: 0,
                        y: 0,
                        width: 3840,
                        height: 2160,
                    });
                }
                let geometry = options.geometry;
                let window = CreateWindowExW(
                    options.extended_style(),
                    class_name,
                    w!("Bloodborne Archipelago"),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    geometry.x,
                    geometry.y,
                    geometry.width,
                    geometry.height,
                    None,
                    None,
                    Some(instance),
                    None,
                )?;
                SetLayeredWindowAttributes(window, COLORREF(0), options.alpha_byte(), LWA_ALPHA)?;

                let body = CreateWindowExW(
                    Default::default(),
                    w!("STATIC"),
                    w!("Starting Bloodborne Archipelago…"),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
                    16,
                    16,
                    geometry.width - 48,
                    geometry.height - 116,
                    Some(window),
                    None,
                    Some(instance),
                    None,
                )?;
                let command_input = CreateWindowExW(
                    Default::default(),
                    w!("EDIT"),
                    w!(""),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0),
                    16,
                    geometry.height - 92,
                    geometry.width - 136,
                    28,
                    Some(window),
                    Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                        CONTROL_COMMAND_INPUT as *mut _,
                    )),
                    Some(instance),
                    None,
                )?;
                let send = CreateWindowExW(
                    Default::default(),
                    w!("BUTTON"),
                    w!("&Send"),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0),
                    geometry.width - 112,
                    geometry.height - 92,
                    96,
                    28,
                    Some(window),
                    Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                        CONTROL_COMMAND_SEND as *mut _,
                    )),
                    Some(instance),
                    None,
                )?;
                let status = CreateWindowExW(
                    Default::default(),
                    w!("BUTTON"),
                    w!("&Status"),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0),
                    16,
                    geometry.height - 92,
                    96,
                    28,
                    Some(window),
                    Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                        CONTROL_STATUS as *mut _,
                    )),
                    Some(instance),
                    None,
                )?;
                let export = CreateWindowExW(
                    Default::default(),
                    w!("BUTTON"),
                    w!("&Export diagnostics"),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0),
                    120,
                    geometry.height - 92,
                    144,
                    28,
                    Some(window),
                    Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                        CONTROL_EXPORT as *mut _,
                    )),
                    Some(instance),
                    None,
                )?;
                let folder = CreateWindowExW(
                    Default::default(),
                    w!("BUTTON"),
                    w!("Open session &folder"),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0),
                    272,
                    geometry.height - 92,
                    152,
                    28,
                    Some(window),
                    Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                        CONTROL_SESSION_FOLDER as *mut _,
                    )),
                    Some(instance),
                    None,
                )?;
                let _ = ShowWindow(window, SW_SHOW);
                let _ = UpdateWindow(window);

                let mut message = MSG::default();
                'running: loop {
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        if message.message == WM_QUIT {
                            break 'running;
                        }
                        // A single-line edit does not emit a button notification for Enter.
                        // Intercept it before translation and route through the same helper as
                        // clicking Send; never forward the keystroke as a second action.
                        if message.hwnd == command_input
                            && message.message == WM_KEYDOWN
                            && message.wParam.0 == 0x0d
                        {
                            submit_command(command_input, &endpoint)?;
                            continue;
                        }
                        if message.message == WM_COMMAND {
                            let control = message.wParam.0 & 0xffff;
                            if control == CONTROL_COMMAND_SEND {
                                submit_command(command_input, &endpoint)?;
                            } else if let Some(action) = control_action(control) {
                                let _ = endpoint.send_action(action);
                            }
                        }
                        // Gives the ordinary child controls standard Tab/Shift+Tab traversal and
                        // activates their ampersand mnemonics without adding UI-owned state.
                        if IsDialogMessageW(window, &message).as_bool() {
                            continue;
                        }
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    if let Some(snapshot) = endpoint.latest_snapshot() {
                        SetWindowTextW(body, &HSTRING::from(render_snapshot(&snapshot)))?;
                    }
                    let mut bounds = RECT::default();
                    GetClientRect(window, &mut bounds)?;
                    let mut window_bounds = RECT::default();
                    if GetWindowRect(window, &mut window_bounds).is_ok() {
                        options.geometry = WindowGeometry {
                            x: window_bounds.left,
                            y: window_bounds.top,
                            width: window_bounds.right - window_bounds.left,
                            height: window_bounds.bottom - window_bounds.top,
                        };
                    }
                    MoveWindow(
                        body,
                        16,
                        16,
                        (bounds.right - 32).max(1),
                        (bounds.bottom - 120).max(1),
                        true,
                    )?;
                    let button_y = (bounds.bottom - 44).max(1);
                    let command_y = (bounds.bottom - 84).max(1);
                    MoveWindow(
                        command_input,
                        16,
                        command_y,
                        (bounds.right - 136).max(1),
                        28,
                        true,
                    )?;
                    MoveWindow(send, (bounds.right - 112).max(1), command_y, 96, 28, true)?;
                    MoveWindow(status, 16, button_y, 96, 28, true)?;
                    MoveWindow(export, 120, button_y, 144, 28, true)?;
                    MoveWindow(folder, 272, button_y, 152, 28, true)?;
                    thread::sleep(Duration::from_millis(50));
                }
                if let Some(path) = self.state_path.as_deref()
                    && let Ok(bytes) = json::to_vec_pretty(&options)
                {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let temporary = path.with_extension("tmp");
                    if std::fs::write(&temporary, bytes).is_ok() {
                        let _ = std::fs::remove_file(path);
                        let _ = std::fs::rename(temporary, path);
                    }
                }
            }
            let _ = endpoint.send_action(client_ui::UiAction::RequestShutdown);
            Ok(())
        }
    }

    pub fn spawn(
        endpoint: client_ui::HostEndpoint,
        options: WindowOptions,
    ) -> thread::JoinHandle<WindowsResult<()>> {
        thread::spawn(move || NativeWindowHost::default().run(endpoint, options))
    }

    pub fn spawn_persisted(
        endpoint: client_ui::HostEndpoint,
        options: WindowOptions,
        state_path: PathBuf,
    ) -> thread::JoinHandle<WindowsResult<()>> {
        thread::spawn(move || {
            NativeWindowHost {
                state_path: Some(state_path),
            }
            .run(endpoint, options)
        })
    }
}

#[cfg(windows)]
pub use native::{NativeWindowHost, spawn, spawn_persisted};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_window_is_visible_and_opacity_is_safe() {
        let work = WindowGeometry {
            x: 100,
            y: 50,
            width: 1920,
            height: 1080,
        };
        let restored = WindowOptions {
            opacity: 0.01,
            geometry: WindowGeometry {
                x: -5000,
                y: 9000,
                width: 4000,
                height: 20,
            },
            ..Default::default()
        }
        .normalized(work);

        assert_eq!(restored.opacity, 0.2);
        assert_eq!(
            restored.geometry,
            WindowGeometry {
                x: 100,
                y: 1010,
                width: 1920,
                height: 120
            }
        );
        assert_eq!(restored.alpha_byte(), 51);
    }

    #[cfg(windows)]
    #[test]
    fn click_through_is_an_explicit_native_style() {
        use windows::Win32::UI::WindowsAndMessaging::{
            WS_EX_LAYERED, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
        };

        assert_eq!(
            WindowOptions::default().extended_style(),
            WS_EX_LAYERED | WS_EX_TOPMOST
        );
        assert_eq!(
            WindowOptions {
                click_through: true,
                ..Default::default()
            }
            .extended_style(),
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TRANSPARENT
        );
    }

    #[test]
    fn snapshot_projection_exposes_distinct_readiness_and_staleness() {
        let snapshot = client_ui::ClientSnapshot {
            process: client_ui::ProcessState::Attached,
            ap: client_ui::ApState::Authenticated,
            delivery: client_ui::DeliveryState::Blocked,
            server: Some("archipelago.gg:12345".into()),
            slot: Some("hunter".into()),
            locations: Some(client_ui::LocationTotals {
                checked: 42,
                total: 166,
            }),
            stale: true,
            ..Default::default()
        };
        let rendered = render_snapshot(&snapshot);
        assert!(rendered.contains("Game: Attached  |  AP: Authenticated"));
        assert!(rendered.contains("Delivery: Blocked"));
        assert!(rendered.contains("Checks: 42 / 166"));
        assert!(rendered.contains("state is stale"));
    }

    #[test]
    fn native_controls_expose_only_read_only_actions() {
        assert!(matches!(
            control_action(CONTROL_STATUS),
            Some(client_ui::UiAction::SubmitCommand(command)) if command == "status"
        ));
        assert!(matches!(
            control_action(CONTROL_EXPORT),
            Some(client_ui::UiAction::SubmitCommand(command)) if command == "export"
        ));
        assert!(matches!(
            control_action(CONTROL_SESSION_FOLDER),
            Some(client_ui::UiAction::OpenSessionFolder)
        ));
        assert!(control_action(9999).is_none());
    }

    #[test]
    fn command_input_is_single_and_bounded() {
        assert_eq!(
            normalize_command_input("  status\r\n"),
            Some("status".into())
        );
        assert_eq!(
            normalize_command_input("flag 1\nretry 2"),
            Some("flag 1 retry 2".into())
        );
        assert_eq!(normalize_command_input("  \n"), None);
        assert_eq!(
            normalize_command_input(&"x".repeat(600)).unwrap().len(),
            512
        );
    }
    /// A `client-window.json` written before the feed filters existed. Byte-for-byte the shape the
    /// shipped Win32 shell persists, kept as a fixture rather than a string literal so a future
    /// schema change has something concrete to be checked against.
    const OLD_SCHEMA_STATE: &str = include_str!("../tests/fixtures/client-window-v1.json");

    #[test]
    fn an_old_state_file_still_parses_and_round_trips() {
        let restored: WindowOptions =
            json::from_str(OLD_SCHEMA_STATE).expect("an old state file must still parse");

        // The fields the old file did carry survive verbatim: the failure this guards against is
        // a parse error that silently discards a player's saved geometry and opacity.
        assert_eq!(restored.opacity, 0.7);
        assert_eq!(
            restored.geometry,
            WindowGeometry {
                x: 120,
                y: 64,
                width: 560,
                height: 720
            }
        );
        assert!(restored.always_on_top);
        assert!(!restored.click_through);
        assert!(!restored.compact);
        // The field the old file did not carry defaults to showing everything.
        assert_eq!(restored.filters, FeedFilters::default());

        // Re-serialising and re-reading is stable, so one launch under the new build does not
        // rewrite the file into something the next launch reads differently.
        let rewritten = json::to_string(&restored).expect("serialise");
        assert_eq!(
            json::from_str::<WindowOptions>(&rewritten).unwrap(),
            restored
        );
    }

    #[test]
    fn a_partial_filter_block_only_ever_defaults_to_visible() {
        // Forward compatibility in the other direction: a file written by a build that knew about
        // fewer toggles must not hide a class the reader does know about.
        let restored: WindowOptions = json::from_str(
            r#"{"always_on_top":false,"click_through":false,"compact":true,"opacity":0.5,
                "geometry":{"x":0,"y":0,"width":420,"height":560},
                "filters":{"chat":false}}"#,
        )
        .expect("parse");
        assert!(!restored.filters.chat);
        assert!(restored.filters.checks);
        assert!(restored.filters.items);
        assert!(restored.filters.system);
    }

    #[test]
    fn filters_route_every_activity_kind_to_exactly_one_toggle() {
        use client_ui::ActivityKind::*;

        let all = FeedFilters::default();
        for kind in [
            Message,
            Command,
            CommandResult,
            LocationCheck,
            ReceivedItem,
            StorageDelivery,
            ParkedDelivery,
            Error,
            Hint,
        ] {
            assert!(all.admits(&kind), "{kind:?} must be visible by default");
        }

        let none = FeedFilters {
            checks: false,
            items: false,
            chat: false,
            system: false,
        };
        for kind in [
            Message,
            Command,
            CommandResult,
            LocationCheck,
            ReceivedItem,
            StorageDelivery,
            ParkedDelivery,
            Error,
            Hint,
        ] {
            assert!(!none.admits(&kind), "{kind:?} must be hidable");
        }

        // Muting chat must not mute a delivery failure.
        let quiet = FeedFilters {
            chat: false,
            ..FeedFilters::default()
        };
        assert!(!quiet.admits(&Message));
        assert!(!quiet.admits(&Hint));
        assert!(quiet.admits(&Error));
        assert!(quiet.admits(&ParkedDelivery));
    }

    #[test]
    fn persisted_options_survive_a_round_trip_through_the_atomic_writer() {
        let directory = std::env::temp_dir().join(format!(
            "bb-window-state-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = directory.join("client-window.json");
        assert_eq!(load_options(&path), None, "no file means no options");

        let options = WindowOptions {
            opacity: 0.55,
            compact: true,
            filters: FeedFilters {
                chat: false,
                ..FeedFilters::default()
            },
            ..Default::default()
        };
        store_options(&path, &options).expect("write");
        assert_eq!(load_options(&path), Some(options));

        // A corrupt file is not an error a player should see; it is a return to defaults.
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_options(&path), None);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
