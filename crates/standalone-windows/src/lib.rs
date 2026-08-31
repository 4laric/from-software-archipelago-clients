//! Standalone Windows-host policy shared by external Archipelago clients.
//!
//! This first slice deliberately owns no game or delivery state. A future renderer plugs into
//! [`client_ui::HostEndpoint`], while this crate owns only durable window behavior.

pub use client_ui;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowOptions {
    pub always_on_top: bool,
    pub click_through: bool,
    pub compact: bool,
    pub opacity: f32,
    pub geometry: WindowGeometry,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            always_on_top: true,
            click_through: false,
            compact: false,
            opacity: 0.85,
            geometry: WindowGeometry::default(),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
            snapshot.ledger.storage_routed,
            snapshot.ledger.parked
        ),
    ];
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
    use std::thread;
    use std::time::Duration;

    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::UpdateWindow;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetClientRect, IDC_ARROW, LWA_ALPHA, LoadCursorW, MSG, MoveWindow, PM_REMOVE, PeekMessageW,
        PostQuitMessage, RegisterClassW, SW_SHOW, SetLayeredWindowAttributes, SetWindowTextW,
        ShowWindow, TranslateMessage, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_QUIT, WNDCLASSW,
        WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows::core::{HSTRING, Result as WindowsResult, w};

    use super::{StandaloneHost, WindowOptions, client_ui, render_snapshot};

    /// Dependency-light native shell. It owns only windowing and rendering; the game client owns
    /// networking, memory access and delivery on its existing worker thread.
    #[derive(Default)]
    pub struct NativeWindowHost;

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

    impl StandaloneHost for NativeWindowHost {
        type Error = windows::core::Error;

        fn run(
            self,
            endpoint: client_ui::HostEndpoint,
            options: WindowOptions,
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
                    geometry.height - 72,
                    Some(window),
                    None,
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
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    if let Some(snapshot) = endpoint.latest_snapshot() {
                        SetWindowTextW(body, &HSTRING::from(render_snapshot(&snapshot)))?;
                    }
                    let mut bounds = RECT::default();
                    GetClientRect(window, &mut bounds)?;
                    MoveWindow(
                        body,
                        16,
                        16,
                        (bounds.right - 32).max(1),
                        (bounds.bottom - 32).max(1),
                        true,
                    )?;
                    thread::sleep(Duration::from_millis(50));
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
        thread::spawn(move || NativeWindowHost.run(endpoint, options))
    }
}

#[cfg(windows)]
pub use native::{NativeWindowHost, spawn};

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
            stale: true,
            ..Default::default()
        };
        let rendered = render_snapshot(&snapshot);
        assert!(rendered.contains("Game: Attached  |  AP: Authenticated"));
        assert!(rendered.contains("Delivery: Blocked"));
        assert!(rendered.contains("state is stale"));
    }
}
