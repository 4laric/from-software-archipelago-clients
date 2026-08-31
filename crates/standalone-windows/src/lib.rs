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
        use windows::Win32::UI::WindowsAndMessaging::{WS_EX_LAYERED, WS_EX_TRANSPARENT};

        if self.click_through {
            WS_EX_LAYERED | WS_EX_TRANSPARENT
        } else {
            WS_EX_LAYERED
        }
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
        use windows::Win32::UI::WindowsAndMessaging::{WS_EX_LAYERED, WS_EX_TRANSPARENT};

        assert_eq!(WindowOptions::default().extended_style(), WS_EX_LAYERED);
        assert_eq!(
            WindowOptions {
                click_through: true,
                ..Default::default()
            }
            .extended_style(),
            WS_EX_LAYERED | WS_EX_TRANSPARENT
        );
    }
}
