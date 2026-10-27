use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{
    Event, Frame, Key, Margin, Modifiers, RichText, Sense, Stroke, SystemTheme, TextureHandle,
    ThemePreference, Ui, UiBuilder, ViewportCommand, vec2,
};
use platform_windows::hotkeys::{Hotkey, HotkeyService};
use platform_windows::monitors::{self, Monitor};
use platform_windows::single_instance::SingleInstance;

use crate::commands::{Action, AppCommand};
use crate::gallery_view::GalleryView;
use crate::record_settings::RecordPanel;
use crate::screen_record::ScreenRecorder;
use crate::settings::{AfterCapture, CloseAction, Settings, ThemeChoice, TrayLeftClick};
use crate::theme::{self, semibold};
use crate::tray::{self, Tray};
use crate::widgets::{self, Icon, palette};

/// Quick picks for the capture highlight; the first is the CapShotX accent.
const HIGHLIGHT_PRESETS: [[u8; 3]; 6] = [
    crate::settings::DEFAULT_HIGHLIGHT,
    [14, 165, 233],
    [16, 185, 129],
    [245, 158, 11],
    [239, 68, 68],
    [236, 72, 153],
];
const MONITOR_REFRESH: Duration = Duration::from_secs(2);
/// Time for the main window to disappear before freezing the screen.
const HIDE_BEFORE_CAPTURE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Gallery,
    Settings,
}

enum ShortcutStatus {
    Active(Hotkey),
    Disabled,
    Failed(String),
}

/// A shortcut being re-recorded in Settings.
struct Recording {
    action: Action,
    hint: Option<&'static str>,
}

pub struct CapShotApp {
    settings: Settings,
    saved_settings: Settings,
    settings_error: Option<String>,
    page: Page,
    logo: TextureHandle,
    commands: Receiver<AppCommand>,
    commands_tx: Sender<AppCommand>,
    quitting: bool,
    hotkeys: HotkeyService,
    shortcut_status: BTreeMap<Action, ShortcutStatus>,
    recording: Option<Recording>,
    monitors: Vec<Monitor>,
    monitors_read_at: Instant,
    gallery: GalleryView,
    screen: ScreenRecorder,
    record_panel: RecordPanel,
    /// Quit was requested mid-recording; quit once the file is saved.
    quit_after_saving: bool,
    tray: Tray,
    _instance: SingleInstance,
}

impl CapShotApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        instance: SingleInstance,
    ) -> anyhow::Result<Self> {
        let ctx = &cc.egui_ctx;
        theme::install(ctx);
        apply_theme(ctx, settings.theme);

        let (tx, commands) = mpsc::channel();
        let tray = tray::install(ctx.clone(), tx.clone())?;
        {
            let (ctx, tx) = (ctx.clone(), tx.clone());
            instance.listen_for_activation(move || {
                let _ = tx.send(AppCommand::Open);
                ctx.request_repaint();
            });
        }
        let hotkeys = {
            let (ctx, tx) = (ctx.clone(), tx.clone());
            HotkeyService::start(move |id| {
                if let Some(action) = Action::from_hotkey_id(id) {
                    let _ = tx.send(AppCommand::Hotkey(action));
                    ctx.request_repaint();
                }
            })?
        };

        let logo = ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], &crate::icon::rgba(64)),
            egui::TextureOptions::LINEAR,
        );

        let monitors = monitors::enumerate();
        for m in &monitors {
            tracing::info!(
                "display {} ({}): {}x{} at ({}, {}), {} dpi{}",
                m.name(),
                m.device,
                m.bounds.width(),
                m.bounds.height(),
                m.bounds.left,
                m.bounds.top,
                m.dpi,
                if m.primary { ", primary" } else { "" }
            );
        }

        let gallery = GalleryView::new(ctx, settings.capture.save_folder(), Settings::thumbnail_cache_dir());

        let mut app = Self {
            saved_settings: settings.clone(),
            settings,
            settings_error: None,
            page: Page::Home,
            logo,
            commands,
            commands_tx: tx.clone(),
            quitting: false,
            hotkeys,
            shortcut_status: BTreeMap::new(),
            recording: None,
            monitors,
            monitors_read_at: Instant::now(),
            gallery,
            screen: ScreenRecorder::new(ctx.clone(), tx.clone()),
            record_panel: RecordPanel::default(),
            quit_after_saving: false,
            tray,
            _instance: instance,
        };
        for &action in Action::ALL {
            app.apply_shortcut(action);
        }
        Ok(app)
    }

    fn show_window(ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn run_action(&mut self, ctx: &egui::Context, action: Action) {
        tracing::debug!("shortcut: {action:?}");
        match action {
            Action::OpenApp => Self::show_window(ctx),
            Action::CaptureRegion => self.start_capture(ctx, capture::CaptureMode::Region, false),
            Action::CaptureWindow => self.start_capture(ctx, capture::CaptureMode::Window, false),
            Action::CaptureMonitor => self.start_capture(ctx, capture::CaptureMode::Monitor, false),
            Action::RecordToggle => self.toggle_screen_recording(ctx),
        }
    }

    fn toggle_screen_recording(&mut self, ctx: &egui::Context) {
        let dark = ctx.theme() == egui::Theme::Dark;
        self.screen.toggle(&self.settings, dark);
        self.tray.set_recording(self.screen.is_busy());
    }

    /// Opens the capture overlay. From the Home page the main window is
    /// hidden first (and restored afterwards) so it isn't in the shot.
    fn start_capture(&mut self, ctx: &egui::Context, mode: capture::CaptureMode, from_app: bool) {
        let c = &self.settings.capture;
        let options = capture::CaptureOptions {
            mode,
            after: c.after.into(),
            magnifier: c.magnifier,
            save_folder: c.save_folder(),
            file_template: c.file_template.clone(),
            highlight: c.highlight,
            sound: c.sound,
            dark: ctx.theme() == egui::Theme::Dark,
        };
        let (tx, ctx2) = (self.commands_tx.clone(), ctx.clone());
        let on_finish = move |finished| {
            let _ = tx.send(AppCommand::CaptureFinished { finished, restore_window: from_app });
            ctx2.request_repaint();
        };
        if from_app {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            let tx = self.commands_tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(HIDE_BEFORE_CAPTURE);
                if capture::start(options, on_finish).is_err() {
                    let _ = tx.send(AppCommand::Open);
                    ctx.request_repaint();
                }
            });
        } else if capture::start(options, on_finish).is_err() {
            tracing::debug!("capture already in progress; ignoring");
        }
    }

    fn on_capture_finished(&mut self, ctx: &egui::Context, finished: capture::Finished, restore: bool) {
        match &finished.outcome {
            capture::CaptureOutcome::Failed(err) => tracing::warn!("capture failed: {err}"),
            outcome => tracing::debug!("capture finished: {outcome:?}"),
        }
        if let capture::CaptureOutcome::Captured { saved: Some(_), .. } = &finished.outcome {
            self.gallery.request_rescan();
        }
        // Z in the overlay toggles the magnifier; remember the choice.
        self.settings.capture.magnifier = finished.magnifier;
        if restore {
            Self::show_window(ctx);
        }
    }

    /// (Re-)registers the global shortcut for `action` from settings.
    fn apply_shortcut(&mut self, action: Action) {
        let id = action.hotkey_id();
        let text = self.settings.shortcut(action);
        let status = if text.is_empty() {
            self.hotkeys.unregister(id);
            ShortcutStatus::Disabled
        } else {
            match text.parse::<Hotkey>() {
                Err(err) => ShortcutStatus::Failed(format!("Invalid shortcut: {err}")),
                Ok(hotkey) => match self.hotkeys.register(id, hotkey) {
                    Ok(()) => ShortcutStatus::Active(hotkey),
                    Err(err) => ShortcutStatus::Failed(err.to_string()),
                },
            }
        };
        if let ShortcutStatus::Failed(err) = &status {
            tracing::warn!("shortcut {text:?} for {action:?} not registered: {err}");
            self.hotkeys.unregister(id);
        }
        self.shortcut_status.insert(action, status);
    }

    fn start_recording(&mut self, action: Action) {
        self.stop_recording();
        // Free the current combination so pressing it again reaches this window.
        self.hotkeys.unregister(action.hotkey_id());
        self.recording = Some(Recording { action, hint: None });
    }

    fn stop_recording(&mut self) {
        if let Some(recording) = self.recording.take() {
            self.apply_shortcut(recording.action);
        }
    }

    /// Consumes the next key press while recording a shortcut.
    fn capture_shortcut(&mut self, ui: &mut Ui) {
        let Some(recording) = &mut self.recording else { return };
        let pressed = ui.input_mut(|i| {
            let mut pressed = None;
            i.events.retain(|event| match event {
                Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } if pressed.is_none() => {
                    pressed = Some((physical_key.unwrap_or(*key), *modifiers));
                    false
                }
                _ => true,
            });
            pressed
        });
        let Some((key, modifiers)) = pressed else { return };
        let action = recording.action;

        match key {
            Key::Escape if modifiers.is_none() => self.stop_recording(),
            Key::Backspace | Key::Delete if modifiers.is_none() => {
                self.settings.set_shortcut(action, String::new());
                self.stop_recording();
            }
            _ => match combo_text(modifiers, key).parse::<Hotkey>() {
                Ok(hotkey) if hotkey.has_modifier() || is_standalone_key(hotkey.vk) => {
                    self.settings.set_shortcut(action, hotkey.to_string());
                    self.stop_recording();
                }
                Ok(_) => recording.hint = Some("Add Ctrl, Alt or Shift to that key."),
                Err(_) => recording.hint = Some("That key can't be used for a shortcut."),
            },
        }
    }

    fn refresh_monitors(&mut self) {
        if self.monitors_read_at.elapsed() >= MONITOR_REFRESH {
            self.monitors = monitors::enumerate();
            self.monitors_read_at = Instant::now();
        }
    }

    fn persist_settings(&mut self, ctx: &egui::Context) {
        if self.settings == self.saved_settings {
            return;
        }
        if self.settings.theme != self.saved_settings.theme {
            apply_theme(ctx, self.settings.theme);
        }
        if self.settings.capture.save_folder != self.saved_settings.capture.save_folder {
            self.gallery.set_folder(self.settings.capture.save_folder());
        }
        self.settings_error = self.settings.save().err().map(|err| format!("{err:#}"));
        if let Some(err) = &self.settings_error {
            tracing::error!("saving settings failed: {err}");
        }
        self.saved_settings = self.settings.clone();
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        egui::Panel::bottom("tray-status")
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new())
            .show(ui, |ui| self.tray_status(ui));

        ui.horizontal(|ui| {
            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(vec2(30.0, 30.0)));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("CapShotX").font(semibold(17.0)).color(p.text));
                ui.label(RichText::new("Preview build").size(11.5).color(p.muted));
            });
        });
        ui.add_space(22.0);

        ui.spacing_mut().item_spacing.y = 4.0;
        for (page, icon, label) in [
            (Page::Home, Icon::Home, "Home"),
            (Page::Gallery, Icon::Image, "Gallery"),
            (Page::Settings, Icon::Settings, "Settings"),
        ] {
            if widgets::nav_item(ui, icon, label, self.page == page).clicked() && self.page != page {
                self.stop_recording();
                self.page = page;
            }
        }
    }

    fn tray_status(&self, ui: &mut Ui) {
        let p = palette(ui);
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, p.success);
                ui.label(RichText::new("Running in tray").size(13.0).color(p.text));
            });
            let hint = match self.settings.close_action {
                CloseAction::HideToTray => "Closing this window keeps CapShotX in the tray.",
                CloseAction::Quit => "Closing this window quits CapShotX.",
            };
            ui.label(RichText::new(hint).size(11.5).color(p.muted));
            if let Some(ShortcutStatus::Active(hotkey)) = self.shortcut_status.get(&Action::OpenApp) {
                ui.label(
                    RichText::new(format!("{hotkey} opens it from anywhere."))
                        .size(11.5)
                        .color(p.muted),
                );
            }
        });
    }

    fn home(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        ui.label(RichText::new("Home").font(semibold(26.0)).color(p.text));
        ui.label(RichText::new("Capture. Edit. Record.").size(14.0).color(p.muted));
        ui.add_space(18.0);

        widgets::section_label(ui, "Capture");
        let captures = [
            (Icon::Region, "Region", "Drag an area, or click a window", Action::CaptureRegion, capture::CaptureMode::Region),
            (Icon::Window, "Window", "Click a window to capture it", Action::CaptureWindow, capture::CaptureMode::Window),
            (Icon::Monitor, "Display", "Capture a whole display", Action::CaptureMonitor, capture::CaptureMode::Monitor),
        ];
        let mut start = None;
        ui.columns(captures.len(), |cols| {
            for (ui, (icon, title, desc, action, mode)) in cols.iter_mut().zip(captures) {
                let shortcut = self.settings.shortcut(action).to_owned();
                let response = widgets::action_card(ui, |ui| {
                    widgets::icon_tile(ui, icon, 38.0);
                    ui.add_space(6.0);
                    ui.label(RichText::new(title).font(semibold(15.5)).color(p.text));
                    ui.label(RichText::new(desc).size(13.0).color(p.muted));
                    ui.add_space(4.0);
                    // Fixed-height footer keeps the three cards the same size
                    // whether they show key chips or plain text.
                    let footer = vec2(ui.available_width(), 28.0);
                    ui.allocate_ui_with_layout(footer, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_size(footer);
                        if shortcut.is_empty() {
                            ui.label(RichText::new("No shortcut").size(12.0).color(p.muted));
                        } else {
                            let keys: Vec<&str> = shortcut.split('+').collect();
                            widgets::shortcut(ui, &keys);
                        }
                    });
                });
                if response.clicked() {
                    start = Some(mode);
                }
            }
        });
        if let Some(mode) = start {
            self.start_capture(ui.ctx(), mode, true);
        }

        ui.add_space(18.0);
        widgets::section_label(ui, "Record");
        let recording = self.screen.is_busy();
        let shortcut = self.settings.shortcut(Action::RecordToggle).to_owned();
        let mut toggle = false;
        ui.columns(3, |cols| {
            let ui = &mut cols[0];
            let (title, desc) = if recording {
                ("Stop recording", "Finish and save the recording")
            } else {
                ("Record screen", "Drag a region, click a window, or press F for full screen")
            };
            let response = widgets::action_card(ui, |ui| {
                widgets::icon_tile(ui, Icon::Record, 38.0);
                ui.add_space(6.0);
                ui.label(RichText::new(title).font(semibold(15.5)).color(p.text));
                ui.label(RichText::new(desc).size(13.0).color(p.muted));
                ui.add_space(4.0);
                let footer = vec2(ui.available_width(), 28.0);
                ui.allocate_ui_with_layout(footer, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_size(footer);
                    if shortcut.is_empty() {
                        ui.label(RichText::new("No shortcut").size(12.0).color(p.muted));
                    } else {
                        let keys: Vec<&str> = shortcut.split('+').collect();
                        widgets::shortcut(ui, &keys);
                    }
                });
            });
            toggle = response.clicked();
        });
        if toggle {
            if recording {
                self.toggle_screen_recording(ui.ctx());
            } else {
                // Hide this window so it's neither in the picker nor recorded.
                ui.ctx().send_viewport_cmd(ViewportCommand::Visible(false));
                let (tx, ctx) = (self.commands_tx.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    std::thread::sleep(HIDE_BEFORE_CAPTURE);
                    let _ = tx.send(AppCommand::RecordToggle);
                    ctx.request_repaint();
                });
            }
        }

        ui.add_space(18.0);
        if self.gallery.recent_ui(ui) {
            self.page = Page::Gallery;
        }
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        self.capture_shortcut(ui);
        self.refresh_monitors();

        ui.label(RichText::new("Settings").font(semibold(26.0)).color(p.text));
        ui.label(RichText::new("Changes are saved automatically.").size(14.0).color(p.muted));
        ui.add_space(18.0);

        {
            let s = &mut self.settings;
            widgets::section_label(ui, "Window & tray");
            widgets::card(ui, |ui| {
                widgets::setting_row(ui, "Close button", "What happens when you close this window", |ui| {
                    widgets::segmented(
                        ui,
                        &mut s.close_action,
                        &[(CloseAction::HideToTray, "Hide to tray"), (CloseAction::Quit, "Quit")],
                    );
                });
                ui.separator();
                widgets::setting_row(ui, "Tray left-click", "Right-click always opens the tray menu", |ui| {
                    widgets::segmented(
                        ui,
                        &mut s.tray_left_click,
                        &[(TrayLeftClick::OpenApp, "Open CapShotX"), (TrayLeftClick::Nothing, "Do nothing")],
                    );
                });
                ui.separator();
                widgets::setting_row(ui, "Start hidden", "Launch straight to the tray without showing this window", |ui| {
                    widgets::toggle(ui, &mut s.start_hidden);
                });
            });
        }

        ui.add_space(12.0);
        widgets::section_label(ui, "Capture");
        widgets::card(ui, |ui| self.capture_card(ui));

        ui.add_space(12.0);
        widgets::section_label(ui, "Recording");
        let recording = self.screen.is_busy();
        widgets::card(ui, |ui| self.record_panel.ui(ui, &mut self.settings.record, recording));

        ui.add_space(12.0);
        widgets::section_label(ui, "Shortcuts");
        widgets::card(ui, |ui| self.shortcuts_card(ui));

        ui.add_space(12.0);
        widgets::section_label(ui, "Appearance");
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, "Theme", "Follow Windows or choose a fixed theme", |ui| {
                widgets::segmented(
                    ui,
                    &mut self.settings.theme,
                    &[(ThemeChoice::Dark, "Dark"), (ThemeChoice::Light, "Light"), (ThemeChoice::System, "System")],
                );
            });
        });

        ui.add_space(12.0);
        widgets::section_label(ui, "Displays");
        widgets::card(ui, |ui| self.displays_card(ui));

        ui.add_space(12.0);
        widgets::section_label(ui, "About");
        widgets::card(ui, |ui| {
            let settings_path = Settings::path();
            widgets::setting_row(
                ui,
                &format!("CapShotX {}", env!("CARGO_PKG_VERSION")),
                &settings_path
                    .as_ref()
                    .map_or("No settings folder".into(), |p| p.display().to_string()),
                |ui| {
                    if let Some(dir) = Settings::logs_dir()
                        && ui.button("Open logs").clicked()
                    {
                        open_folder(&dir);
                    }
                    if let Some(dir) = settings_path.as_ref().and_then(|p| p.parent())
                        && ui.button("Open settings folder").clicked()
                    {
                        open_folder(dir);
                    }
                },
            );
            if let Some(err) = &self.settings_error {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Could not save settings: {err}"))
                        .size(12.5)
                        .color(p.danger),
                );
            }
        });
    }

    fn capture_card(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let c = &mut self.settings.capture;
        widgets::setting_row(ui, "After capture", "What happens when you finish selecting", |ui| {
            widgets::segmented(
                ui,
                &mut c.after,
                &[
                    (AfterCapture::Copy, "Copy"),
                    (AfterCapture::CopyAndSave, "Copy & save"),
                    (AfterCapture::Ask, "Ask each time"),
                ],
            );
        });
        ui.separator();
        widgets::setting_row(ui, "Magnifier", "Pixel zoom and colour under the cursor · press Z while capturing", |ui| {
            widgets::toggle(ui, &mut c.magnifier);
        });
        ui.separator();
        widgets::setting_row(ui, "Capture sound", "A soft shutter click when a screenshot is taken", |ui| {
            if widgets::toggle(ui, &mut c.sound).changed() && c.sound {
                platform_windows::sound::play_shutter();
            }
        });
        ui.separator();
        widgets::setting_row(ui, "Highlight colour", "Selection border and handles while capturing", |ui| {
            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut c.highlight);
            for preset in HIGHLIGHT_PRESETS.iter().rev() {
                if widgets::swatch(ui, *preset, c.highlight == *preset).clicked() {
                    c.highlight = *preset;
                }
            }
        });
        ui.separator();
        let folder = c.save_folder();
        widgets::setting_row(ui, "Save folder", &folder.display().to_string(), |ui| {
            if ui.button("Open").clicked() {
                let _ = std::fs::create_dir_all(&folder);
                platform_windows::shell::open(&folder);
            }
            if ui.button("Change…").clicked()
                && let Some(picked) = platform_windows::shell::pick_folder(Some(&folder))
            {
                c.save_folder = Some(picked);
            }
        });
        ui.separator();

        let preview = capture::render_template(&c.file_template, platform_windows::shell::local_time());
        let (description, color) = match &preview {
            Ok(path) => (format!("{}.png", path.display()), p.muted),
            Err(err) => (err.clone(), p.danger),
        };
        let is_default = c.file_template == capture::DEFAULT_FILE_TEMPLATE;
        widgets::setting_row(ui, "File name", "", |ui| {
            if !is_default
                && ui
                    .add(egui::Button::new(RichText::new("Reset").size(12.5)).frame(false))
                    .clicked()
            {
                c.file_template = capture::DEFAULT_FILE_TEMPLATE.to_owned();
            }
            ui.add(
                egui::TextEdit::singleline(&mut c.file_template)
                    .desired_width(300.0)
                    .font(egui::TextStyle::Monospace),
            )
            .on_hover_text("Tokens: {date} {time} {year} {month} {day} {hour} {minute} {second} {ms}. Use / for folders.");
        });
        ui.label(RichText::new(description).size(12.5).color(color));
    }

    fn shortcuts_card(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        for (i, &action) in Action::ALL.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            let recording = self.recording.as_ref().filter(|r| r.action == action);
            let (description, color) = match (recording, self.shortcut_status.get(&action)) {
                (Some(r), _) => (
                    r.hint.unwrap_or("Press the new shortcut · Esc cancels · Backspace clears"),
                    if r.hint.is_some() { p.danger } else { p.accent },
                ),
                (None, Some(ShortcutStatus::Failed(err))) => (err.as_str(), p.danger),

                _ => (action.description(), p.muted),
            };
            let is_recording = recording.is_some();
            let current = self.settings.shortcut(action).to_owned();
            let is_default = current == action.default_shortcut();

            let mut clicked = false;
            let mut reset = false;
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(action.label()).size(14.5).color(p.text));
                    ui.label(RichText::new(description).size(12.5).color(color));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clicked = shortcut_field(ui, &current, is_recording).clicked();
                    if !is_default && !is_recording {
                        reset = ui
                            .add(egui::Button::new(RichText::new("Reset").size(12.5)).frame(false))
                            .on_hover_text(format!("Restore {}", action.default_shortcut()))
                            .clicked();
                    }
                });
            });

            if clicked {
                if is_recording {
                    self.stop_recording();
                } else {
                    self.start_recording(action);
                }
            }
            if reset {
                self.settings.set_shortcut(action, action.default_shortcut().to_owned());
                self.apply_shortcut(action);
            }
        }
    }

    fn displays_card(&self, ui: &mut Ui) {
        let p = palette(ui);
        if self.monitors.is_empty() {
            ui.label(RichText::new("No displays detected").color(p.muted));
            return;
        }
        for (i, m) in self.monitors.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            let detail = format!(
                "{} × {} px · {:.0}% scale · position ({}, {})",
                m.bounds.width(),
                m.bounds.height(),
                m.scale() * 100.0,
                m.bounds.left,
                m.bounds.top,
            );
            widgets::setting_row(ui, m.name(), &detail, |ui| {
                if m.primary {
                    widgets::badge(ui, "Primary", p.accent, p.accent_soft);
                }
            });
        }
    }
}

impl eframe::App for CapShotApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(cmd) = self.commands.try_recv() {
            match cmd {
                AppCommand::TrayLeftClick => {
                    if self.settings.tray_left_click == TrayLeftClick::OpenApp {
                        Self::show_window(ctx);
                    }
                }
                AppCommand::Open => Self::show_window(ctx),
                AppCommand::OpenSettings => {
                    self.page = Page::Settings;
                    Self::show_window(ctx);
                }
                AppCommand::Quit => {
                    if self.screen.is_recording() {
                        // Finalise the file first; quit when it's saved.
                        self.quit_after_saving = true;
                        self.screen.stop();
                    } else {
                        self.quitting = true;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                AppCommand::RecordToggle => self.toggle_screen_recording(ctx),
                AppCommand::RecordingStop => self.screen.stop(),
                AppCommand::RecordingStarted(result) => {
                    self.screen.on_started(result, ctx.theme() == egui::Theme::Dark);
                    self.tray.set_recording(self.screen.is_busy());
                }
                AppCommand::RecordingFinished(result) => {
                    if result.is_ok() {
                        self.gallery.request_rescan();
                    }
                    self.screen.on_finished(result, ctx.theme() == egui::Theme::Dark);
                    self.tray.set_recording(false);
                    if self.quit_after_saving {
                        self.quitting = true;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                AppCommand::Hotkey(action) => self.run_action(ctx, action),
                AppCommand::CaptureFinished { finished, restore_window } => {
                    self.on_capture_finished(ctx, finished, restore_window)
                }
            }
        }

        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.quitting && self.settings.close_action == CloseAction::Quit && self.screen.is_recording() {
            // Don't kill a recording mid-write: save it, then quit.
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.quit_after_saving = true;
            self.screen.stop();
        }
        if close_requested && !self.quitting && self.settings.close_action == CloseAction::HideToTray {
            self.stop_recording();
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.record_panel.release();
        }
        // Audio level monitors only run while Settings is on screen.
        if self.page != Page::Settings {
            self.record_panel.release();
        }

        self.persist_settings(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let p = palette(ui);

        egui::Panel::left("nav")
            .exact_size(232.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(14, 18)))
            .show(ui, |ui| self.sidebar(ui));

        egui::CentralPanel::no_frame()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin::symmetric(32, 28)))
            .show(ui, |ui| {
                self.gallery.tick();
                if self.page == Page::Gallery {
                    // The gallery scrolls (and virtualises) its own grid.
                    self.gallery.page_ui(ui);
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Leave a gutter so the scrollbar never covers card edges.
                    Frame::new()
                        .inner_margin(Margin { right: 14, ..Margin::ZERO })
                        .show(ui, |ui| match self.page {
                            Page::Home => self.home(ui),
                            Page::Settings => self.settings_page(ui),
                            Page::Gallery => {}
                        });
                });
            });

        if self.page == Page::Settings {
            // Keeps the Displays list current without a display-change hook.
            ui.ctx().request_repaint_after(MONITOR_REFRESH);
        }
    }
}

/// Clickable field showing a shortcut as key chips.
fn shortcut_field(ui: &mut Ui, shortcut: &str, recording: bool) -> egui::Response {
    let p = palette(ui);
    let response = ui
        .scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
            let hovered = ui.response().hovered();
            Frame::new()
                .fill(if recording { p.accent_soft } else if hovered { p.control_hover } else { p.control })
                .stroke(Stroke::new(1.0, if recording { p.accent } else { p.stroke }))
                .corner_radius(8)
                .inner_margin(Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.set_min_width(120.0);
                    ui.horizontal(|ui| {
                        if recording {
                            ui.label(RichText::new("Press keys…").size(13.0).color(p.accent));
                        } else if shortcut.is_empty() {
                            ui.label(RichText::new("Not set").size(13.0).color(p.muted));
                        } else {
                            let keys: Vec<&str> = shortcut.split('+').collect();
                            widgets::shortcut(ui, &keys);
                        }
                    });
                });
        })
        .response;
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn combo_text(modifiers: Modifiers, key: Key) -> String {
    let mut text = String::new();
    for (on, name) in [(modifiers.ctrl, "Ctrl"), (modifiers.shift, "Shift"), (modifiers.alt, "Alt")] {
        if on {
            text.push_str(name);
            text.push('+');
        }
    }
    text.push_str(key.name());
    text
}

/// Keys that are reasonable shortcuts without a modifier.
fn is_standalone_key(vk: u16) -> bool {
    matches!(vk, 0x70..=0x87 | 0x2C | 0x13)
}

fn open_folder(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}

fn apply_theme(ctx: &egui::Context, choice: ThemeChoice) {
    let (preference, system) = match choice {
        ThemeChoice::System => (ThemePreference::System, SystemTheme::SystemDefault),
        ThemeChoice::Light => (ThemePreference::Light, SystemTheme::Light),
        ThemeChoice::Dark => (ThemePreference::Dark, SystemTheme::Dark),
    };
    ctx.set_theme(preference);
    // Keeps the native title bar in step with the app theme.
    ctx.send_viewport_cmd(ViewportCommand::SetTheme(system));
}
