//! The eframe shell. Everything it *decides* lives in [`crate::view`]; this file only draws.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use client_ui::{ActivityEvent, ClientSnapshot, GuidanceGate, HostEndpoint, Severity, UiAction};
use egui::{Align, Color32, FontId, Layout, RichText, TextStyle, Vec2};
use standalone_windows::{WindowGeometry, WindowOptions, normalize_command_input};

use crate::hotkey::{self, Escape};
use crate::view::{
    self, ActivityStyle, CommandHistory, STORAGE_TOOLTIP, Tone, checks_progress, clock_label,
    event_style, goal_line, identity, items_line, pills, toast_alpha, toast_events, victory_lines,
};

/// Heartbeat. The old shell repainted a whole text blob every 50 ms, which is where its flicker
/// came from. Here a repaint is scheduled a quarter-second out purely so the clock column and the
/// "starting" skeleton cannot sit still forever; every *interesting* repaint is driven by a new
/// snapshot revision or by user input.
const HEARTBEAT: Duration = Duration::from_millis(250);

const MIN_SIZE: [f32; 2] = [360.0, 240.0];
const DEFAULT_SIZE: [f32; 2] = [420.0, 560.0];
const COMPACT_SIZE: [f32; 2] = [420.0, 160.0];

fn activity_scroll_area() -> egui::ScrollArea {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
}

fn color(rgb: view::Rgb) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn rescue_rows(snapshot: &ClientSnapshot) -> Vec<client_ui::BlockedEntry> {
    snapshot.blocked.clone()
}

fn retry_confirmation(rows: &[client_ui::BlockedEntry]) -> String {
    if rows.len() == 1 {
        format!(
            "Requeue {} (index {}) through normal delivery?",
            rows[0].item_name, rows[0].index
        )
    } else {
        let listed = rows
            .iter()
            .map(|row| format!("{} (index {})", row.item_name, row.index))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Requeue these items through normal delivery, in index order? {listed}")
    }
}

pub struct StandaloneApp {
    endpoint: HostEndpoint,
    state_path: Option<PathBuf>,
    options: WindowOptions,
    snapshot: Option<ClientSnapshot>,
    input: String,
    history: CommandHistory,
    /// Whether the feed follows new rows. Cleared when the player scrolls up, restored when they
    /// scroll back to the bottom or press the jump chip.
    pinned: bool,
    jump_requested: bool,
    /// Local UTC offset in seconds, sampled once. A client that runs across a DST boundary shows
    /// the offset it started with; re-deriving it per frame is not worth a timezone lookup at
    /// 4 Hz, and the column is a reading aid rather than a record.
    utc_offset_seconds: i64,
    /// Set when the last applied viewport state differs from `options`, so always-on-top,
    /// passthrough and compact are pushed once on change rather than every frame.
    applied: Option<WindowOptions>,
    shutdown_sent: bool,
    /// Ctrl+Shift+F10. `None` when the hotkey could not be registered, which is the one condition
    /// under which the click-through toggle is refused: an escape hatch that does not exist is
    /// worse than a feature that is missing.
    escape: Option<Escape>,
    guidance: GuidanceGate,
    rescue_open: bool,
    rescue_confirm: Vec<client_ui::BlockedEntry>,
    retry_pending: BTreeSet<u64>,
    last_activity_sequence: u64,
}

impl StandaloneApp {
    fn new(endpoint: HostEndpoint, options: WindowOptions, state_path: Option<PathBuf>) -> Self {
        Self {
            endpoint,
            state_path,
            options,
            snapshot: None,
            input: String::new(),
            history: CommandHistory::default(),
            pinned: true,
            jump_requested: false,
            utc_offset_seconds: i64::from(chrono::Local::now().offset().local_minus_utc()),
            applied: None,
            shutdown_sent: false,
            escape: hotkey::register(),
            guidance: GuidanceGate::default(),
            rescue_open: false,
            rescue_confirm: Vec::new(),
            retry_pending: BTreeSet::new(),
            last_activity_sequence: 0,
        }
    }

    /// Never blocks: the bridge coalesces to the newest revision, so draining it to empty is the
    /// whole synchronisation this renderer performs against the worker.
    fn drain_snapshots(&mut self) -> bool {
        let mut updated = false;
        while let Some(snapshot) = self.endpoint.latest_snapshot() {
            if snapshot.activity.iter().any(|event| {
                event.sequence > self.last_activity_sequence
                    && event.kind == client_ui::ActivityKind::CommandResult
                    && event.text.starts_with("Rescue retry refused:")
            }) {
                self.retry_pending.clear();
            }
            self.last_activity_sequence = snapshot
                .activity
                .back()
                .map_or(self.last_activity_sequence, |event| event.sequence);
            self.snapshot = Some(snapshot);
            updated = true;
        }
        updated
    }

    fn send(&self, action: UiAction) {
        // A full action channel means the worker is busy, not that the click was wrong. Dropping
        // is correct here and is the reason this call cannot stall the UI thread.
        let _ = self.endpoint.send_action(action);
    }

    fn submit_command(&mut self) {
        let Some(command) = normalize_command_input(&self.input) else {
            return;
        };
        // Clear on successful queue only -- exactly the Win32 shell's rule. A command dropped by a
        // full channel stays in the box so the player can send it again.
        if self
            .endpoint
            .send_action(UiAction::SubmitCommand(command.clone()))
            .is_ok()
        {
            self.history.record(&command);
            self.input.clear();
            self.pinned = true;
        }
    }

    fn persist(&self) {
        if let Some(path) = self.state_path.as_deref() {
            let _ = standalone_windows::store_options(path, &self.options);
        }
    }

    fn apply_viewport(&mut self, ctx: &egui::Context) {
        if self.applied == Some(self.options) {
            return;
        }
        let compact_changed =
            self.applied.map(|previous| previous.compact) != Some(self.options.compact);
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            if self.options.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            },
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(
            self.options.click_through,
        ));
        if compact_changed {
            let size = if self.options.compact {
                COMPACT_SIZE
            } else {
                let geometry = self.options.geometry;
                [
                    (geometry.width as f32).max(MIN_SIZE[0]),
                    (geometry.height as f32).max(MIN_SIZE[1]),
                ]
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::from(size)));
        }
        self.applied = Some(self.options);
        self.persist();
    }

    /// Track the window's own geometry so the next launch restores it, clamped to the monitor the
    /// player is actually using rather than to the hard-coded 4K rectangle the Win32 shell assumed.
    fn record_geometry(&mut self, ctx: &egui::Context) {
        if self.options.compact {
            // Compact is a temporary shape, not a saved one; persisting it would restore a
            // 160-pixel-tall window as if the player had resized it there.
            return;
        }
        let (outer, monitor) = ctx.input(|input| {
            let viewport = input.viewport();
            (viewport.outer_rect, viewport.monitor_size)
        });
        let Some(outer) = outer else { return };
        let work = monitor.map_or(
            WindowGeometry {
                x: 0,
                y: 0,
                width: 3840,
                height: 2160,
            },
            |size| WindowGeometry {
                x: 0,
                y: 0,
                width: size.x as i32,
                height: size.y as i32,
            },
        );
        let geometry = WindowGeometry {
            x: outer.min.x as i32,
            y: outer.min.y as i32,
            width: outer.width() as i32,
            height: outer.height() as i32,
        };
        let normalized = WindowOptions {
            geometry,
            ..self.options
        }
        .normalized(work);
        if normalized.geometry != self.options.geometry {
            self.options.geometry = normalized.geometry;
        }
    }

    fn visible_events<'a>(&self, snapshot: &'a ClientSnapshot) -> Vec<&'a ActivityEvent> {
        snapshot
            .activity
            .iter()
            .filter(|event| self.options.filters.admits(&event.kind))
            .collect()
    }

    fn header(&mut self, ui: &mut egui::Ui, snapshot: &ClientSnapshot) {
        let (slot, server) = identity(snapshot);
        ui.horizontal(|ui| {
            let title = ui.add(
                egui::Label::new(RichText::new(slot).strong().size(15.0))
                    .sense(egui::Sense::click()),
            );
            // Double-click anywhere on the title toggles compact, so the overlay shape is one
            // gesture away without opening the settings popover.
            if title.double_clicked() {
                self.options.compact = !self.options.compact;
            }
            if let Some(server) = server {
                ui.label(RichText::new(server).color(color(view::palette::MUTED)));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button("\u{2699}", |ui| self.settings(ui));
            });
        });

        ui.horizontal_wrapped(|ui| {
            for pill in pills(snapshot) {
                let tint = color(pill.tone.color());
                let text = if pill.tone == Tone::Muted {
                    RichText::new(format!("\u{25cf} {} {}", pill.label, pill.value)).color(tint)
                } else {
                    RichText::new(format!("\u{25cf} {} {}", pill.label, pill.value))
                        .color(tint)
                        .strong()
                };
                ui.label(text);
                ui.add_space(6.0);
            }
        });

        if snapshot.stale {
            // The banner, not a feed line. Staleness invalidates every other reading on the
            // window, and the old shell let that fact scroll away like any other message.
            let frame = egui::Frame::new()
                .fill(color(view::palette::BAD).gamma_multiply(0.25))
                .inner_margin(egui::Margin::symmetric(8, 4));
            frame.show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.label(
                        RichText::new(client_ui::STALE_BANNER)
                            .color(color(view::palette::BAD))
                            .strong(),
                    );
                });
            });
        } else {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as u64);
            let (severity, headline) = self.guidance.observe(snapshot, now_ms);
            let tint = match severity {
                Severity::Ok => view::palette::OK,
                Severity::Warn => view::palette::WARN,
                Severity::Bad => view::palette::BAD,
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(headline).color(color(tint)));
                if snapshot.delivery == client_ui::DeliveryState::Blocked
                    && ui.link("Rescue…").clicked()
                {
                    self.rescue_open = true;
                }
            });
        }
    }

    fn progress(&mut self, ui: &mut egui::Ui, snapshot: &ClientSnapshot) {
        if let Some(victory) = &snapshot.victory {
            egui::Frame::new()
                .fill(color(view::palette::VICTORY).gamma_multiply(0.12))
                .stroke(egui::Stroke::new(1.0_f32, color(view::palette::VICTORY)))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    for (index, line) in victory_lines(victory).into_iter().enumerate() {
                        let text = RichText::new(line).color(color(view::palette::VICTORY));
                        ui.label(if index == 0 { text.strong() } else { text });
                    }
                });
            ui.add_space(5.0);
        }
        if let Some((fraction, label)) = checks_progress(snapshot) {
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_height(10.0)
                    .fill(color(view::palette::ACCENT))
                    .text(RichText::new(format!("Checks {label}")).size(11.0)),
            );
        }
        if !snapshot.unchecked_locations.is_empty() {
            let groups = client_ui::group_unchecked_locations(&snapshot.unchecked_locations);
            egui::CollapsingHeader::new(format!(
                "Unchecked locations ({})",
                snapshot.unchecked_locations.len()
            ))
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "All unchecked locations. Known reachability is not supplied by this seed.",
                    )
                    .color(color(view::palette::MUTED))
                    .size(11.0),
                );
                egui::ScrollArea::vertical()
                    .id_salt("unchecked-locations")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for group in groups {
                            egui::CollapsingHeader::new(format!(
                                "{} ({})",
                                group.region,
                                group.locations.len()
                            ))
                            .default_open(false)
                            .show(ui, |ui| {
                                for location in group.locations {
                                    ui.label(
                                        RichText::new(location)
                                            .color(color(view::palette::TEXT))
                                            .size(11.0),
                                    );
                                }
                            });
                        }
                    });
            });
        }
        let items =
            ui.label(RichText::new(items_line(&snapshot.ledger)).color(color(view::palette::TEXT)));
        if snapshot.ledger.storage_routed.is_none() {
            items.on_hover_text(STORAGE_TOOLTIP);
        }
        if snapshot.ledger.parked > 0
            && ui
                .button(
                    RichText::new(format!("\u{23f8} {} parked", snapshot.ledger.parked))
                        .color(color(view::palette::WARN)),
                )
                .clicked()
        {
            self.rescue_open = true;
        }
        if let Some((goal, go_mode)) = goal_line(snapshot) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(goal).color(color(view::palette::MUTED)));
                if go_mode {
                    ui.label(
                        RichText::new(" GO MODE ")
                            .color(Color32::BLACK)
                            .background_color(color(view::palette::OK))
                            .strong()
                            .size(11.0),
                    );
                }
            });
        }
    }

    fn feed(&mut self, ui: &mut egui::Ui, snapshot: &ClientSnapshot) {
        ui.horizontal_wrapped(|ui| {
            let filters = &mut self.options.filters;
            ui.toggle_value(&mut filters.checks, "Checks");
            ui.toggle_value(&mut filters.items, "Items");
            ui.toggle_value(&mut filters.chat, "Chat");
            ui.toggle_value(&mut filters.system, "System");
        });
        ui.separator();

        let events = self.visible_events(snapshot);
        let offset = self.utc_offset_seconds;
        // Let egui's scroll state follow a growing log.  Do not express "the bottom" as an
        // infinite offset: `ScrollArea` applies an explicit offset before it knows this frame's
        // content height, which can briefly place interactive children at infinite coordinates.
        // Their hit rectangles then contain NaNs after clipping; `egui::hit_test_on_close` compares
        // those rectangles on the next pass and panics because NaN makes a widget unequal to the
        // copy it just selected (clients#557).  `stick_to_bottom` is the lifecycle-safe API for a
        // terminal-style feed and automatically releases when the player scrolls away.
        let scroll = activity_scroll_area();
        let jump_requested = std::mem::take(&mut self.jump_requested);
        let output = scroll.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for event in &events {
                row(ui, event, offset);
            }
            if events.is_empty() {
                ui.label(RichText::new("Nothing here yet.").color(color(view::palette::MUTED)));
            }
            if jump_requested {
                // This targets the finite end cursor after all rows have been laid out. It also
                // re-arms ScrollArea's native sticky state for subsequent activity.
                ui.scroll_to_cursor(Some(Align::BOTTOM));
            }
        });

        // Pin detection rather than pin control: the feed follows the newest row whenever the
        // player is already at the bottom, and stops the moment they scroll away from it.
        let bottom = (output.content_size.y - output.inner_rect.height()).max(0.0);
        self.pinned = output.state.offset.y >= bottom - 8.0;
        if !self.pinned {
            ui.with_layout(Layout::right_to_left(Align::BOTTOM), |ui| {
                if ui.button("\u{2193} jump to latest").clicked() {
                    self.jump_requested = true;
                    self.pinned = true;
                }
            });
        }
    }

    fn command_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.menu_button("\u{22ef}", |ui| {
                if ui.button("Rescue\u{2026}").clicked() {
                    self.rescue_open = true;
                    ui.close();
                }
                if ui.button("Status").clicked() {
                    self.send(UiAction::SubmitCommand("status".into()));
                    ui.close();
                }
            });
            let send = ui.button("Send");
            let available = ui.available_width();
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .desired_width(available)
                    .hint_text("command \u{2014} try \"status\" or \"!help\"")
                    .font(TextStyle::Monospace),
            );
            let entered = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if input.has_focus() {
                if ui.input(|i| i.key_pressed(egui::Key::ArrowUp))
                    && let Some(line) = self.history.older()
                {
                    self.input = line;
                }
                if ui.input(|i| i.key_pressed(egui::Key::ArrowDown))
                    && let Some(line) = self.history.newer()
                {
                    self.input = line;
                }
            }
            // Enter and Send share one path, so neither can double-fire and both clear only on a
            // successful queue.
            if entered || send.clicked() {
                self.submit_command();
                input.request_focus();
            }
        });
    }

    fn toast_deck(&self, ui: &mut egui::Ui, snapshot: &ClientSnapshot) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                elapsed.as_millis().min(u128::from(u64::MAX)) as u64
            });
        let events = toast_events(snapshot, now_ms);
        if events.is_empty() {
            ui.label(
                RichText::new("Waiting for checks and items...").color(color(view::palette::MUTED)),
            );
            return;
        }
        for event in events {
            let alpha = toast_alpha(event.timestamp_ms, now_ms);
            let style = event_style(event);
            egui::Frame::new()
                .fill(color(view::palette::PANEL).gamma_multiply(0.92 * alpha))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    color(style.color).gamma_multiply(alpha),
                ))
                .corner_radius(5.0)
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(&event.text)
                            .color(color(style.color).gamma_multiply(alpha))
                            .strong(),
                    );
                });
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(240.0);
        let mut percent = (self.options.opacity * 100.0).round();
        if ui
            .add(egui::Slider::new(&mut percent, 35.0..=100.0).text("Opacity %"))
            .changed()
        {
            self.options.opacity = percent / 100.0;
        }
        ui.checkbox(&mut self.options.always_on_top, "Always on top");
        ui.checkbox(&mut self.options.compact, "Compact mode");
        // Refused rather than offered when the hotkey is unavailable: with no way back, this
        // checkbox is a control that removes the ability to un-press it.
        let armed = self.escape.is_some();
        ui.add_enabled_ui(armed, |ui| {
            ui.checkbox(&mut self.options.click_through, "Click-through");
        });
        ui.label(
            RichText::new(if armed {
                "Click-through makes the window ignore the mouse. Press Ctrl+Shift+F10 to take it \
                 back."
            } else {
                "Click-through is unavailable: another application already owns Ctrl+Shift+F10, \
                 and without that key there would be no way to switch it back off."
            })
            .color(color(view::palette::MUTED))
            .size(11.0),
        );
    }

    fn rescue_panel(&mut self, ctx: &egui::Context, snapshot: &ClientSnapshot) {
        let blocked = rescue_rows(snapshot);
        self.retry_pending
            .retain(|index| blocked.iter().any(|entry| entry.index == *index));
        if !self.rescue_open {
            return;
        }
        let mut open = self.rescue_open;
        egui::Window::new("Rescue")
            .open(&mut open)
            .default_width(520.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.heading("Session facts");
                let identity = snapshot.save_identity.as_deref().unwrap_or("unvalidated");
                let facts = format!(
                    "Seed: {}\nSlot: {}\nSave: {}\nGameplay ready: {}\nReceive cursor: {}\nItems: {} delivered, {} queued, {} parked\nChecks: {}",
                    snapshot.seed.as_deref().unwrap_or("unknown"),
                    snapshot.slot.as_deref().unwrap_or("unknown"),
                    identity,
                    snapshot.gameplay_ready,
                    snapshot.receive_cursor.map_or_else(|| "none".to_owned(), |value| value.to_string()),
                    snapshot.ledger.delivered,
                    snapshot.ledger.queued,
                    snapshot.ledger.parked,
                    snapshot.locations.as_ref().map_or_else(|| "unknown".to_owned(), |v| format!("{}/{}", v.checked, v.total)),
                );
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&facts).monospace()).selectable(true));
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(facts.clone());
                    }
                });
                if snapshot.save_identity.is_none() {
                    ui.label(RichText::new("Save identity is unvalidated; rescue mutations remain disarmed.").color(color(view::palette::WARN)));
                }

                ui.separator();
                ui.horizontal(|ui| {
                    ui.heading("Parked deliveries");
                    if !blocked.is_empty()
                        && ui.button("Retry all").clicked()
                    {
                        self.rescue_confirm = blocked.clone();
                    }
                });
                if blocked.is_empty() {
                    ui.label("No parked deliveries.");
                    ui.label(RichText::new("Items land here instead of being lost when delivery isn't safe; they wait here until you retry them.").color(color(view::palette::MUTED)));
                } else {
                    egui::Grid::new("rescue_blocked").striped(true).show(ui, |ui| {
                        for entry in &blocked {
                            ui.label(RichText::new(entry.index.to_string()).color(color(view::palette::MUTED)).monospace());
                            ui.label(&entry.item_name);
                            ui.add(egui::Label::new(&entry.reason).wrap());
                            let pending = self.retry_pending.contains(&entry.index);
                            if ui.add_enabled(!pending, egui::Button::new(if pending { "Queued…" } else { "Retry" })).clicked() {
                                self.rescue_confirm = vec![entry.clone()];
                            }
                            ui.end_row();
                        }
                    });
                }

                ui.separator();
                ui.heading("Diagnostics");
                ui.horizontal(|ui| {
                    if ui.button("Export diagnostics").clicked() {
                        self.send(UiAction::SubmitCommand("export".into()));
                    }
                    if ui.button("Open session folder").clicked() {
                        self.send(UiAction::OpenSessionFolder);
                    }
                });
                ui.separator();
                ui.label(RichText::new("Typed equivalents: status · blocked · retry N CONFIRM · flag N · export").color(color(view::palette::MUTED)).size(11.0));
            });
        self.rescue_open = open;

        if !self.rescue_confirm.is_empty() {
            let rows = self.rescue_confirm.clone();
            let title = if rows.len() == 1 {
                "Retry parked delivery?"
            } else {
                "Retry all parked deliveries?"
            };
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(retry_confirmation(&rows));
                    ui.label(RichText::new("This is audited and safe to repeat — a delivery that already applied will not double-grant.").color(color(view::palette::MUTED)));
                    ui.horizontal(|ui| {
                        if ui.button("Confirm retry").clicked() {
                            let mut indices = rows.iter().map(|row| row.index).collect::<Vec<_>>();
                            indices.sort_unstable();
                            for index in indices {
                                self.send(UiAction::RetryBlocked { index });
                                self.retry_pending.insert(index);
                            }
                            self.rescue_confirm.clear();
                        }
                        if ui.button("Cancel").clicked() { self.rescue_confirm.clear(); }
                    });
                });
        }
    }
}

/// One activity row: muted clock, kind glyph, then selectable text that wraps.
fn row(ui: &mut egui::Ui, event: &ActivityEvent, utc_offset_seconds: i64) {
    let ActivityStyle {
        color: tint,
        glyph,
        monospace,
    } = event_style(event);
    let response = ui
        .horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                RichText::new(clock_label(event.timestamp_ms, utc_offset_seconds))
                    .color(color(view::palette::MUTED))
                    .font(FontId::monospace(11.0)),
            );
            if !glyph.is_empty() {
                ui.label(RichText::new(glyph).color(color(tint)));
            }
            let mut text = RichText::new(&event.text).color(color(tint));
            if monospace {
                text = text.font(FontId::monospace(12.0));
            }
            // Selectable, so a player can copy a line into a bug report -- the single most
            // requested thing the one-blob STATIC control could not do.
            ui.add(egui::Label::new(text).wrap().selectable(true));
        })
        .response;

    response.context_menu(|ui| {
        if ui.button("Copy line").clicked() {
            ui.ctx().copy_text(event.text.clone());
            ui.close();
        }
    });
}

impl eframe::App for StandaloneApp {
    /// Transparent, so the window's own translucency is the panel fill's alpha rather than a
    /// layered-window call. Opacity therefore works identically whatever the host compositor does.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_snapshots();
        // Both directions of the same rule: the hotkey turns click-through off, and a restored
        // state file that asks for click-through is refused outright when there is no hotkey to
        // turn it off with. A window that starts out ignoring the mouse is a window a player
        // cannot get back.
        if self.escape.as_ref().is_some_and(Escape::take)
            || (self.escape.is_none() && self.options.click_through)
        {
            self.options.click_through = false;
        }
        self.apply_viewport(ctx);
        self.record_geometry(ctx);

        if ctx.input(|input| input.viewport().close_requested()) && !self.shutdown_sent {
            // Same contract as the Win32 shell: closing the window asks the worker to stop, it
            // does not stop the worker.
            self.shutdown_sent = true;
            self.send(UiAction::RequestShutdown);
            self.persist();
        }

        let alpha = (self.options.opacity.clamp(0.2, 1.0) * 255.0).round() as u8;
        let fill = |rgb: view::Rgb| Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], alpha);
        let panel = egui::Frame::new()
            .fill(fill(view::palette::PANEL))
            .inner_margin(egui::Margin::symmetric(10, 8));

        let snapshot = self.snapshot.clone().unwrap_or_default();
        let compact = self.options.compact;

        egui::TopBottomPanel::top("header")
            .frame(panel)
            .show(ctx, |ui| {
                self.header(ui, &snapshot);
                ui.add_space(4.0);
                self.progress(ui, &snapshot);
            });

        if !compact {
            egui::TopBottomPanel::bottom("command")
                .frame(panel)
                .show(ctx, |ui| self.command_bar(ui));
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(fill(view::palette::BACKGROUND))
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| {
                if compact {
                    // Full mode already has the feed. Compact is the transient pickup surface:
                    // newest first, bounded and fading, so it can sit over active play.
                    self.toast_deck(ui, &snapshot);
                } else {
                    self.feed(ui, &snapshot);
                }
            });

        if !compact {
            self.rescue_panel(ctx, &snapshot);
        }

        ctx.request_repaint_after(HEARTBEAT);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist();
        if !self.shutdown_sent {
            self.send(UiAction::RequestShutdown);
        }
    }
}

fn native_options(options: WindowOptions) -> eframe::NativeOptions {
    let size = if options.compact {
        COMPACT_SIZE
    } else {
        [
            (options.geometry.width as f32).max(DEFAULT_SIZE[0]),
            (options.geometry.height as f32).max(DEFAULT_SIZE[1]),
        ]
    };
    let viewport = egui::ViewportBuilder::default()
        .with_title("Bloodborne Archipelago")
        .with_inner_size(size)
        .with_min_inner_size(MIN_SIZE)
        .with_position([options.geometry.x as f32, options.geometry.y as f32])
        .with_transparent(true)
        .with_mouse_passthrough(options.click_through)
        .with_always_on_top();

    eframe::NativeOptions {
        viewport,
        // The client worker owns the process's main thread; winit needs explicit permission to
        // start an event loop anywhere else. Windows-only, which is the only target this runs on.
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    }
}

/// Runs the window to completion on the calling thread.
///
/// The error is flattened to a string on purpose: `eframe::Error` is not `Send`, and this runs on
/// a spawned thread whose result the worker may want to look at. A window that failed to open is
/// a message, not a value anyone reconstructs.
fn run(
    endpoint: HostEndpoint,
    options: WindowOptions,
    state_path: Option<PathBuf>,
) -> Result<(), String> {
    let restored = state_path
        .as_deref()
        .and_then(standalone_windows::load_options)
        .map_or(options, |saved| WindowOptions {
            // Opacity stays under the launcher's `--window-opacity` for this launch: the flag is
            // an explicit instruction and a saved file is a remembered preference.
            opacity: options.opacity,
            ..saved
        });
    eframe::run_native(
        "Bloodborne Archipelago",
        native_options(restored),
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(StandaloneApp::new(endpoint, restored, state_path)))
        }),
    )
    .map_err(|error| error.to_string())
}

pub fn spawn(
    endpoint: HostEndpoint,
    options: WindowOptions,
) -> thread::JoinHandle<Result<(), String>> {
    thread::spawn(move || run(endpoint, options, None))
}

pub fn spawn_persisted(
    endpoint: HostEndpoint,
    options: WindowOptions,
    state_path: PathBuf,
) -> thread::JoinHandle<Result<(), String>> {
    thread::spawn(move || run(endpoint, options, Some(state_path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_ui::ActivityKind;
    use egui::{Event, Pos2, RawInput, Rect};
    use standalone_windows::FeedFilters;

    #[test]
    fn a_compact_window_is_not_persisted_as_the_players_chosen_size() {
        // Guards the rule in `record_geometry`: compact is a shape, not a saved preference.
        let options = WindowOptions {
            compact: true,
            ..Default::default()
        };
        assert_eq!(options.geometry, WindowGeometry::default());
    }

    #[test]
    fn filters_start_permissive() {
        assert_eq!(WindowOptions::default().filters, FeedFilters::default());
    }

    #[test]
    fn rapidly_replaced_activity_never_creates_non_finite_scroll_geometry() {
        // Reproduces the lifecycle around clients#557: the pointer remains over a selectable row
        // while readiness replaces a short startup feed with a long live feed and back again.
        // An explicit `vertical_scroll_offset(f32::INFINITY)` laid the children out at infinity;
        // egui's next-pass hit test then compared NaN-bearing WidgetRects and panicked.
        let ctx = egui::Context::default();
        let counts = [1, 180, 2, 220, 0, 64, 3];

        for (pass, count) in counts.into_iter().enumerate() {
            let mut offset = None;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(420.0, 300.0))),
                time: Some(pass as f64 / 60.0),
                events: vec![Event::PointerMoved(Pos2::new(120.0, 120.0))],
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let shown = activity_scroll_area().show(ui, |ui| {
                        for sequence in 0..count {
                            row(
                                ui,
                                &ActivityEvent {
                                    sequence,
                                    kind: ActivityKind::Message,
                                    text: format!("readiness activity {sequence}"),
                                    timestamp_ms: sequence,
                                    item_class: None,
                                },
                                0,
                            );
                        }
                    });
                    offset = Some(shown.state.offset.y);
                });
            });
            assert!(offset.unwrap().is_finite());
            // Tessellation walks every generated clip rectangle and catches non-finite geometry
            // in the same frame instead of leaving it for the native renderer.
            let _ = ctx.tessellate(output.shapes, output.pixels_per_point);
        }
    }

    #[test]
    fn rescue_view_model_preserves_named_rows_and_confirmation_identity() {
        let mut snapshot = ClientSnapshot::default();
        snapshot.blocked.push(client_ui::BlockedEntry {
            index: 7,
            item_name: "Fire Paper x2".into(),
            reason: "quantity mismatch".into(),
        });
        let rows = rescue_rows(&snapshot);
        assert_eq!(rows, snapshot.blocked);
        let copy = retry_confirmation(&rows);
        assert!(copy.contains("Fire Paper x2"));
        assert!(copy.contains("index 7"));
    }
}
