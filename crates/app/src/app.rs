use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use egui::{
    Event, Frame, Key, Margin, Modifiers, RichText, Sense, Stroke, SystemTheme, TextureHandle,
    ThemePreference, Ui, UiBuilder, ViewportCommand, vec2,
};
use platform_windows::hotkeys::{Hotkey, HotkeyService, KeyCapture, current_modifiers, is_vk_down};
use platform_windows::monitors::{self, Monitor};
use platform_windows::single_instance::SingleInstance;

use crate::commands::{Action, AppCommand, Waker};
use crate::editor_addon::{self, EditorLaunchTarget};
use crate::gallery_view::{GalleryCache, GalleryView};
use crate::record_settings::RecordPanel;
use crate::screen_record::ScreenRecorder;
use crate::settings::{AfterCapture, CloseAction, RecordStartMode, Settings, ThemeChoice, TrayLeftClick};
use crate::theme::{self, semibold};
use crate::tray::{self, Tray};
use crate::widgets::{self, Icon, palette};

const MONITOR_REFRESH: Duration = Duration::from_secs(2);
/// Time for the main window to disappear before freezing the screen.
const HIDE_BEFORE_CAPTURE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Home,
    Gallery,
    Addons,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    General,
    Capture,
    Recording,
    Hotkeys,
    About,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayStep {
    StayInTray,
    OpenWindow,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiExit {
    HideToTray,
    Quit,
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
    key_capture: KeyCapture,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(platform_windows::updater::ReleaseInfo),
    Downloading(platform_windows::updater::ReleaseInfo),
    Error(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum EditorAddonState {
    #[default]
    Idle,
    PromptOpen,
    Downloading,
    Error(String),
}

/// Persistent lightweight process state (~a few KB) that lives across UI
/// sessions while CapShotX sits in the tray with zero GPU/renderer overhead.
pub struct CoreState {
    pub settings: Settings,
    saved_settings: Settings,
    settings_error: Option<String>,
    startup_error: Option<String>,
    is_elevated: bool,
    sync_startup_enabled: bool,
    last_saved_folder: Option<std::path::PathBuf>,
    pub(crate) page: Page,
    pub(crate) settings_tab: SettingsTab,
    pub(crate) update_state: UpdateState,
    pub(crate) editor_addon_state: EditorAddonState,
    pub(crate) pending_editor_target: Option<EditorLaunchTarget>,
    pub window_size: [f32; 2],
    pub window_pos_px: Option<[f32; 2]>,
    pub window_maximized: bool,
    waker: Waker,
    commands: Receiver<AppCommand>,
    hotkeys: HotkeyService,
    shortcut_status: BTreeMap<Action, ShortcutStatus>,
    gallery_cache: GalleryCache,
    screen: ScreenRecorder,
    /// Quit was requested mid-recording; quit once the file is saved.
    quit_after_saving: bool,
    tray: Tray,
    _instance: SingleInstance,
}

impl CoreState {
    pub fn new(
        settings: Settings,
        instance: SingleInstance,
        sync_startup_enabled: bool,
    ) -> anyhow::Result<Self> {
        platform_windows::updater::cleanup_old_exe();
        let (tx, commands) = mpsc::channel();
        let waker = Waker::new(
            tx,
            platform_windows::shell::current_thread_id(),
            settings.theme,
            settings.low_memory_mode,
        );
        let tray = tray::install(waker.clone())?;
        {
            let waker = waker.clone();
            instance.listen_for_activation(move || {
                waker.send(AppCommand::Open);
            });
        }
        let hotkeys = {
            let waker = waker.clone();
            HotkeyService::start(move |id| {
                if let Some(action) = Action::from_hotkey_id(id) {
                    waker.send(AppCommand::Hotkey(action));
                }
            })?
        };

        for m in monitors::enumerate() {
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

        {
            let (folder, waker) = (settings.capture.save_folder(), waker.clone());
            let _ = std::thread::Builder::new()
                .name("recover-partials".into())
                .spawn(move || {
                    let recovered = recorder::recover_partials(&folder);
                    if !recovered.is_empty() {
                        waker.send(AppCommand::RecoveredRecordings(recovered));
                    }
                });
        }

        let check_on_startup = settings.check_updates_on_startup;
        let mut core = Self {
            saved_settings: settings.clone(),
            page: if settings.sidebar.home {
                Page::Home
            } else if settings.sidebar.gallery {
                Page::Gallery
            } else if settings.sidebar.addons {
                Page::Addons
            } else {
                Page::Settings
            },
            settings,
            settings_error: None,
            startup_error: None,
            is_elevated: platform_windows::shell::is_elevated(),
            sync_startup_enabled,
            last_saved_folder: None,
            settings_tab: SettingsTab::General,
            update_state: UpdateState::Idle,
            editor_addon_state: EditorAddonState::Idle,
            pending_editor_target: None,
            window_size: [1040.0, 680.0],
            window_pos_px: None,
            window_maximized: false,
            waker: waker.clone(),
            commands,
            hotkeys,
            shortcut_status: BTreeMap::new(),
            gallery_cache: GalleryCache::default(),
            screen: ScreenRecorder::new(waker),
            quit_after_saving: false,
            tray,
            _instance: instance,
        };
        for &action in Action::ALL {
            core.apply_shortcut(action);
        }
        core.sync_startup();
        if check_on_startup {
            core.check_for_updates(false);
        }
        Ok(core)
    }

    pub fn check_for_updates(&mut self, manual: bool) {
        if matches!(self.update_state, UpdateState::Checking | UpdateState::Downloading(_)) {
            return;
        }
        self.update_state = UpdateState::Checking;
        let waker = self.waker.clone();
        let _ = std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let result =
                    platform_windows::updater::check_for_update(env!("CARGO_PKG_VERSION"));
                waker.send(AppCommand::UpdateCheckFinished { result, manual });
            });
    }

    pub fn install_update(&mut self, release: platform_windows::updater::ReleaseInfo) {
        if matches!(self.update_state, UpdateState::Downloading(_)) {
            return;
        }
        self.persist_settings_headless();
        self.update_state = UpdateState::Downloading(release.clone());
        let waker = self.waker.clone();
        let _ = std::thread::Builder::new()
            .name("update-install".into())
            .spawn(move || {
                let res = platform_windows::updater::download_and_install_update(&release);
                waker.send(AppCommand::UpdateInstallFinished(res));
            });
    }

    fn on_update_check_finished(
        &mut self,
        result: Result<Option<platform_windows::updater::ReleaseInfo>, String>,
        manual: bool,
    ) {
        match result {
            Ok(Some(info)) => {
                tracing::info!("update available on GitHub: v{}", info.version);
                self.update_state = UpdateState::Available(info);
            }
            Ok(None) => {
                self.update_state = UpdateState::UpToDate;
            }
            Err(err) => {
                tracing::debug!("update check failed (manual={manual}): {err}");
                if manual {
                    self.update_state = UpdateState::Error(err);
                } else {
                    self.update_state = UpdateState::Idle;
                }
            }
        }
    }

    /// Launches `CapShotX_Editor.exe` with `target` if installed. If not installed yet,
    /// records `target` as pending, opens the one-click download prompt, and returns `false`
    /// so the caller can show the main window.
    pub fn request_editor(&mut self, target: EditorLaunchTarget) -> bool {
        if editor_addon::launch_if_installed(&target) {
            self.pending_editor_target = None;
            return true;
        }
        if let Some(EditorLaunchTarget::TempBgra(old_temp)) =
            self.pending_editor_target.replace(target)
        {
            let _ = std::fs::remove_file(old_temp);
        }
        if !matches!(self.editor_addon_state, EditorAddonState::Downloading) {
            self.editor_addon_state = EditorAddonState::PromptOpen;
        }
        false
    }

    pub fn start_editor_download(&mut self) {
        if matches!(self.editor_addon_state, EditorAddonState::Downloading) {
            return;
        }
        let Some(target_path) = editor_addon::editor_exe_path() else {
            self.editor_addon_state = EditorAddonState::Error(
                "Could not determine install path for CapShotX_Editor.exe".into(),
            );
            return;
        };
        self.editor_addon_state = EditorAddonState::Downloading;
        let waker = self.waker.clone();
        let _ = std::thread::Builder::new()
            .name("editor-download".into())
            .spawn(move || {
                let res = platform_windows::updater::download_and_install_editor(&target_path);
                waker.send(AppCommand::EditorInstallFinished(res));
            });
    }

    pub fn cancel_editor_prompt(&mut self) {
        if let Some(EditorLaunchTarget::TempBgra(temp)) = self.pending_editor_target.take() {
            let _ = std::fs::remove_file(temp);
        }
        self.editor_addon_state = EditorAddonState::Idle;
    }

    fn on_editor_install_finished(&mut self, res: Result<String, String>) -> bool {
        match res {
            Ok(version) => {
                tracing::info!("CapShotX_Editor.exe v{version} installed");
                self.editor_addon_state = EditorAddonState::Idle;
                self.apply_shortcut(Action::CaptureEdit);
                if let Some(target) = self.pending_editor_target.take() {
                    return editor_addon::launch_if_installed(&target);
                }
                false
            }
            Err(err) => {
                tracing::warn!("CapShotX_Editor.exe download failed: {err}");
                self.editor_addon_state = EditorAddonState::Error(err);
                false
            }
        }
    }

    fn sync_startup(&mut self) {
        if !self.sync_startup_enabled {
            return;
        }
        match platform_windows::startup::sync(
            self.settings.launch_at_startup,
            self.settings.run_as_admin,
        ) {
            Ok(()) => self.startup_error = None,
            Err(err) => {
                tracing::warn!("startup registration failed: {err}");
                self.startup_error = Some(err);
            }
        }
    }

    fn open_captures_folder(&self) {
        open_folder(&self.settings.capture.save_folder());
    }

    fn open_latest_capture_folder(&self) {
        if let Some(dir) = self.last_saved_folder.as_deref().filter(|d| d.is_dir()) {
            open_folder(dir);
        } else {
            self.open_captures_folder();
        }
    }

    pub fn trim_if_idle(&self) {
        if !self.screen.is_busy() {
            platform_windows::shell::trim_working_set();
        }
    }

    /// Drains queued commands while no UI window is open.
    pub fn poll_tray(&mut self) -> TrayStep {
        while let Ok(cmd) = self.commands.try_recv() {
            match cmd {
                AppCommand::TrayLeftClick => match self.settings.tray_left_click {
                    TrayLeftClick::OpenApp => return TrayStep::OpenWindow,
                    TrayLeftClick::OpenGallery => {
                        self.page = Page::Gallery;
                        return TrayStep::OpenWindow;
                    }
                    TrayLeftClick::OpenSettings => {
                        self.page = Page::Settings;
                        return TrayStep::OpenWindow;
                    }
                    TrayLeftClick::OpenLatestCaptureFolder => {
                        self.open_latest_capture_folder();
                        self.trim_if_idle();
                    }
                    TrayLeftClick::OpenCapturesFolder => {
                        self.open_captures_folder();
                        self.trim_if_idle();
                    }
                    TrayLeftClick::CaptureRegion => {
                        self.start_capture_headless(capture::CaptureMode::Region);
                    }
                    TrayLeftClick::RecordRegion => self.toggle_screen_recording(),
                    TrayLeftClick::FastRecord => self.start_screen_recording(true, false),
                    TrayLeftClick::Nothing => {}
                },
                AppCommand::Open => return TrayStep::OpenWindow,
                AppCommand::OpenEditor => {
                    if !self.request_editor(EditorLaunchTarget::Empty) {
                        return TrayStep::OpenWindow;
                    }
                    self.trim_if_idle();
                }
                AppCommand::OpenSettings => {
                    self.page = Page::Settings;
                    return TrayStep::OpenWindow;
                }
                AppCommand::Quit => {
                    if self.screen.is_recording() {
                        self.quit_after_saving = true;
                        self.screen.stop();
                    } else {
                        self.screen.cancel_starting();
                        return TrayStep::Quit;
                    }
                }
                AppCommand::Hotkey(Action::OpenApp) => return TrayStep::OpenWindow,
                AppCommand::Hotkey(action) => self.run_background_action(action),
                AppCommand::RecordToggle => self.toggle_screen_recording(),
                AppCommand::RecordStart { fast, from_app } => {
                    self.start_screen_recording(fast, from_app);
                }
                AppCommand::RecordSyncAudio { desktop_audio, microphone } => {
                    self.settings.record.desktop_audio = desktop_audio;
                    self.settings.record.microphone = microphone;
                    self.persist_settings_headless();
                }
                AppCommand::RecordingStop => self.screen.stop(),
                AppCommand::RecordingStarted(result) => {
                    let restore = self.screen.on_started(result, self.waker.is_dark());
                    self.tray.set_recording(self.screen.is_busy());
                    if restore && !self.quit_after_saving {
                        return TrayStep::OpenWindow;
                    }
                }
                AppCommand::RecordingFinished(result) => {
                    if result.is_ok() {
                        self.gallery_cache.dirty = true;
                    }
                    if let Some(saved) = self.screen.on_finished(result, self.waker.is_dark()) {
                        self.last_saved_folder = saved.parent().map(std::path::Path::to_path_buf);
                    }
                    self.tray.set_recording(false);
                    if self.quit_after_saving {
                        return TrayStep::Quit;
                    }
                    self.trim_if_idle();
                }
                AppCommand::RecoveredRecordings(paths) => {
                    self.gallery_cache.dirty = true;
                    crate::screen_record::notify_recovered(paths, self.waker.is_dark());
                    self.trim_if_idle();
                }
                AppCommand::CaptureFinished { finished, restore_window } => {
                    let (saved, opened_editor) = self.finish_capture_state(finished);
                    if saved {
                        self.gallery_cache.dirty = true;
                    }
                    self.persist_settings_headless();
                    let need_prompt = matches!(
                        self.editor_addon_state,
                        EditorAddonState::PromptOpen | EditorAddonState::Downloading | EditorAddonState::Error(_)
                    );
                    if (restore_window || need_prompt) && !opened_editor && !self.quit_after_saving {
                        return TrayStep::OpenWindow;
                    }
                    self.trim_if_idle();
                }
                AppCommand::UpdateCheckFinished { result, manual } => {
                    self.on_update_check_finished(result, manual);
                    self.trim_if_idle();
                }
                AppCommand::UpdateInstallFinished(Ok(())) => {
                    return TrayStep::Quit;
                }
                AppCommand::UpdateInstallFinished(Err(err)) => {
                    tracing::warn!("update install failed: {err}");
                    self.update_state = UpdateState::Error(err);
                }
                AppCommand::EditorInstallFinished(res) => {
                    self.on_editor_install_finished(res);
                    self.trim_if_idle();
                }
            }
        }
        TrayStep::StayInTray
    }

    fn run_background_action(&mut self, action: Action) {
        tracing::debug!("shortcut (tray): {action:?}");
        match action {
            Action::OpenApp => {}
            Action::CaptureRegion => self.start_capture_headless(capture::CaptureMode::Region),
            Action::CaptureEdit => {
                self.start_capture_headless_with_after(
                    capture::CaptureMode::Region,
                    capture::AfterCapture::Edit,
                );
            }
            Action::CaptureWindow => self.start_capture_headless(capture::CaptureMode::Window),
            Action::CaptureMonitor => self.start_capture_headless(capture::CaptureMode::Monitor),
            Action::RecordToggle => self.toggle_screen_recording(),
            Action::RecordFast => self.start_screen_recording(true, false),
            Action::RecordPause => self.screen.toggle_pause(),
        }
    }

    fn toggle_screen_recording(&mut self) {
        let dark = self.waker.is_dark();
        self.screen.toggle(&self.settings, dark);
        self.tray.set_recording(self.screen.is_busy());
    }

    fn start_screen_recording(&mut self, fast: bool, from_app: bool) {
        let dark = self.waker.is_dark();
        self.screen.start_or_toggle(&self.settings, dark, fast, from_app);
        self.tray.set_recording(self.screen.is_busy());
    }

    fn capture_options(&self, mode: capture::CaptureMode) -> capture::CaptureOptions {
        let c = &self.settings.capture;
        capture::CaptureOptions {
            mode,
            after: c.after.into(),
            magnifier: c.magnifier,
            save_folder: c.save_folder(),
            file_template: c.file_template.clone(),
            highlight: c.highlight,
            border_width: c.border_width,
            sound: c.sound,
            show_notification: c.show_notification,
            dark: self.waker.is_dark(),
            editor_installed: editor_addon::is_installed(),
        }
    }

    fn start_capture_headless(&mut self, mode: capture::CaptureMode) {
        let after = self.settings.capture.after.into();
        self.start_capture_headless_with_after(mode, after);
    }

    fn start_capture_headless_with_after(
        &mut self,
        mode: capture::CaptureMode,
        after: capture::AfterCapture,
    ) {
        let mut options = self.capture_options(mode);
        options.after = after;
        let waker = self.waker.clone();
        let on_finish = move |finished| {
            waker.send(AppCommand::CaptureFinished { finished, restore_window: false });
        };
        if capture::start(options, on_finish).is_err() {
            tracing::debug!("capture already in progress; ignoring");
        }
    }

    /// Updates magnifier preference from a finished capture; returns `(saved, open_editor)`.
    fn finish_capture_state(&mut self, finished: capture::Finished) -> (bool, bool) {
        let mut saved = false;
        let mut open_editor = false;
        match finished.outcome {
            capture::CaptureOutcome::Failed(err) => tracing::warn!("capture failed: {err}"),
            capture::CaptureOutcome::Captured {
                saved: Some(path), ..
            } => {
                self.last_saved_folder = path.parent().map(std::path::Path::to_path_buf);
                saved = true;
            }
            capture::CaptureOutcome::Edit {
                width,
                height,
                bgra,
            } => {
                let target = editor_addon::write_temp_bgra(width, height, &bgra)
                    .map_or(EditorLaunchTarget::Empty, EditorLaunchTarget::TempBgra);
                open_editor = self.request_editor(target);
            }
            outcome => tracing::debug!("capture finished: {outcome:?}"),
        }
        self.settings.capture.magnifier = finished.magnifier;
        (saved, open_editor)
    }

    /// (Re-)registers the global shortcut for `action` from settings.
    fn apply_shortcut(&mut self, action: Action) {
        let id = action.hotkey_id();
        if action == Action::CaptureEdit && !editor_addon::is_installed() {
            self.hotkeys.unregister(id);
            self.shortcut_status.insert(action, ShortcutStatus::Disabled);
            return;
        }
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

    fn persist_settings_headless(&mut self) {
        if self.settings == self.saved_settings {
            return;
        }
        self.waker.set_theme_choice(self.settings.theme);
        self.waker.set_low_memory_mode(self.settings.low_memory_mode);
        if self.settings.capture.save_folder != self.saved_settings.capture.save_folder {
            self.gallery_cache = GalleryCache::default();
        }
        if self.settings.record.show_outline != self.saved_settings.record.show_outline {
            self.screen.set_show_outline(self.settings.record.show_outline);
        }
        if self.settings.launch_at_startup != self.saved_settings.launch_at_startup
            || self.settings.run_as_admin != self.saved_settings.run_as_admin
        {
            self.sync_startup();
        }
        self.settings_error = self.settings.save().err().map(|err| format!("{err:#}"));
        if let Some(err) = &self.settings_error {
            tracing::error!("saving settings failed: {err}");
        }
        self.saved_settings = self.settings.clone();
    }
}

pub struct CapShotApp<'a> {
    core: &'a mut CoreState,
    exit_out: &'a mut UiExit,
    exit: UiExit,
    /// Window is hidden briefly for an in-app screenshot or recording bounds pick.
    hidden_temporarily: bool,
    hidden_for_record: bool,
    logo: TextureHandle,
    recording: Option<Recording>,
    settings_search: String,
    monitors: Vec<Monitor>,
    monitors_read_at: Instant,
    gallery: GalleryView,
    record_panel: RecordPanel,
}

impl<'a> CapShotApp<'a> {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        core: &'a mut CoreState,
        exit_out: &'a mut UiExit,
        initial_visible: bool,
    ) -> anyhow::Result<Self> {
        let ctx = &cc.egui_ctx;
        theme::install(ctx);
        apply_theme(ctx, core.settings.theme);
        core.waker.attach_ctx(ctx.clone());
        ctx.request_repaint();

        let logo = ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], &crate::icon::rgba(64)),
            egui::TextureOptions::LINEAR,
        );
        let gallery = GalleryView::new(
            ctx,
            core.settings.capture.save_folder(),
            Settings::thumbnail_cache_dir(),
            &mut core.gallery_cache,
        );

        Ok(Self {
            core,
            exit_out,
            exit: UiExit::HideToTray,
            hidden_temporarily: !initial_visible,
            hidden_for_record: false,
            logo,
            recording: None,
            settings_search: String::new(),
            monitors: monitors::enumerate(),
            monitors_read_at: Instant::now(),
            gallery,
            record_panel: RecordPanel::default(),
        })
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden_temporarily = false;
        self.hidden_for_record = false;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn run_action(&mut self, ctx: &egui::Context, action: Action) {
        tracing::debug!("shortcut: {action:?}");
        match action {
            Action::OpenApp => self.show_window(ctx),
            Action::CaptureRegion => self.start_capture(ctx, capture::CaptureMode::Region, false),
            Action::CaptureEdit => {
                self.start_capture_with_after(
                    ctx,
                    capture::CaptureMode::Region,
                    capture::AfterCapture::Edit,
                    false,
                );
            }
            Action::CaptureWindow => self.start_capture(ctx, capture::CaptureMode::Window, false),
            Action::CaptureMonitor => self.start_capture(ctx, capture::CaptureMode::Monitor, false),
            Action::RecordToggle => self.core.toggle_screen_recording(),
            Action::RecordFast => self.core.start_screen_recording(true, false),
            Action::RecordPause => self.core.screen.toggle_pause(),
        }
    }

    /// Opens the capture overlay. From the Home page the main window is
    /// hidden first (and restored afterwards) so it isn't in the shot.
    fn start_capture(&mut self, ctx: &egui::Context, mode: capture::CaptureMode, from_app: bool) {
        let after = self.core.settings.capture.after.into();
        self.start_capture_with_after(ctx, mode, after, from_app);
    }

    fn start_capture_with_after(
        &mut self,
        ctx: &egui::Context,
        mode: capture::CaptureMode,
        after: capture::AfterCapture,
        from_app: bool,
    ) {
        let mut options = self.core.capture_options(mode);
        options.after = after;
        let waker = self.core.waker.clone();
        let on_finish = move |finished| {
            waker.send(AppCommand::CaptureFinished { finished, restore_window: from_app });
        };
        if from_app {
            self.hidden_temporarily = true;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            let waker = self.core.waker.clone();
            std::thread::spawn(move || {
                std::thread::sleep(HIDE_BEFORE_CAPTURE);
                if capture::start(options, on_finish).is_err() {
                    waker.send(AppCommand::Open);
                }
            });
        } else if capture::start(options, on_finish).is_err() {
            tracing::debug!("capture already in progress; ignoring");
        }
    }

    fn on_capture_finished(&mut self, ctx: &egui::Context, finished: capture::Finished, restore: bool) {
        let (saved, opened_editor) = self.core.finish_capture_state(finished);
        if saved {
            self.gallery.request_rescan();
        }
        let need_prompt = matches!(
            self.core.editor_addon_state,
            EditorAddonState::PromptOpen | EditorAddonState::Downloading | EditorAddonState::Error(_)
        );
        if (restore && !opened_editor) || need_prompt {
            self.show_window(ctx);
        }
    }

    fn start_recording(&mut self, action: Action) {
        self.stop_recording();
        // Temporarily unregister all CapShotX global hotkeys so pressing any
        // chord (even one currently bound to another action) reaches this window.
        for &a in Action::ALL {
            self.core.hotkeys.unregister(a.hotkey_id());
        }
        self.recording = Some(Recording {
            action,
            hint: None,
            key_capture: KeyCapture::new(),
        });
    }

    fn stop_recording(&mut self) {
        if self.recording.take().is_some() {
            for &a in Action::ALL {
                self.core.apply_shortcut(a);
            }
        }
    }

    /// Consumes the next key press while recording a shortcut.
    ///
    /// Uses both Win32 `GetKeyboardState` (`KeyCapture`) and `egui` event
    /// fallbacks (`Event::Cut`/`Copy`/`Paste`/`Key`) so combinations like
    /// `Ctrl+X`, `Ctrl+C`, `Ctrl+V`, `Win+...`, Numpad keys, and single keys
    /// all record cleanly.
    fn capture_shortcut(&mut self, ui: &mut Ui) {
        let Some(recording) = &mut self.recording else { return };

        let (focused, egui_fallback) = ui.input_mut(|i| {
            let mut fallback = None;
            i.events.retain(|event| match event {
                Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let chosen = physical_key.unwrap_or(*key);
                    if !is_modifier_key(chosen) && fallback.is_none() {
                        fallback = egui_key_to_hotkey(chosen, *modifiers);
                    }
                    false
                }
                Event::Key { .. } | Event::Text(_) => false,
                Event::Cut => {
                    if fallback.is_none() {
                        let (ctrl, shift, alt, win) = current_modifiers();
                        let vk = if shift && !ctrl && is_vk_down(0x2E) {
                            0x2E // Shift+Delete
                        } else {
                            0x58 // Ctrl+X
                        };
                        fallback = Some(Hotkey {
                            ctrl: ctrl || i.modifiers.ctrl || i.modifiers.command,
                            shift: shift || i.modifiers.shift,
                            alt: alt || i.modifiers.alt,
                            win,
                            vk,
                        });
                    }
                    false
                }
                Event::Copy => {
                    if fallback.is_none() {
                        let (ctrl, shift, alt, win) = current_modifiers();
                        let vk = if is_vk_down(0x2D) { 0x2D } else { 0x43 };
                        fallback = Some(Hotkey {
                            ctrl: ctrl || i.modifiers.ctrl || i.modifiers.command,
                            shift: shift || i.modifiers.shift,
                            alt: alt || i.modifiers.alt,
                            win,
                            vk,
                        });
                    }
                    false
                }
                Event::Paste(_) | Event::PasteImage(_) => {
                    if fallback.is_none() {
                        let (ctrl, shift, alt, win) = current_modifiers();
                        let vk = if is_vk_down(0x2D) { 0x2D } else { 0x56 };
                        fallback = Some(Hotkey {
                            ctrl: ctrl || i.modifiers.ctrl || i.modifiers.command,
                            shift: shift || i.modifiers.shift,
                            alt: alt || i.modifiers.alt,
                            win,
                            vk,
                        });
                    }
                    false
                }
                _ => true,
            });
            (i.focused, fallback)
        });

        let win32_hotkey = focused.then(|| recording.key_capture.poll()).flatten();
        let Some(hotkey) = win32_hotkey.or(egui_fallback) else {
            return;
        };
        let action = recording.action;

        if hotkey.vk == 0x1B && !hotkey.has_modifier() {
            // Bare Esc cancels recording.
            self.stop_recording();
        } else if hotkey.vk == 0x08 && !hotkey.has_modifier() {
            // Bare Backspace clears the shortcut.
            self.core.settings.set_shortcut(action, String::new());
            self.stop_recording();
        } else {
            // Unbind any other CapShotX action that previously used this exact
            // chord so the newly edited action takes ownership cleanly.
            for &other in Action::ALL {
                if other != action
                    && self
                        .core
                        .settings
                        .shortcut(other)
                        .parse::<Hotkey>()
                        .is_ok_and(|h| h == hotkey)
                {
                    self.core.settings.set_shortcut(other, String::new());
                }
            }
            self.core.settings.set_shortcut(action, hotkey.to_string());
            self.stop_recording();
        }
    }

    fn refresh_monitors(&mut self) {
        if self.monitors_read_at.elapsed() >= MONITOR_REFRESH {
            self.monitors = monitors::enumerate();
            self.monitors_read_at = Instant::now();
        }
    }

    fn persist_settings(&mut self, ctx: &egui::Context) {
        if self.core.settings == self.core.saved_settings {
            return;
        }
        if self.core.settings.theme != self.core.saved_settings.theme {
            apply_theme(ctx, self.core.settings.theme);
        }
        if self.core.settings.capture.save_folder != self.core.saved_settings.capture.save_folder {
            self.gallery.set_folder(self.core.settings.capture.save_folder());
        }
        self.core.persist_settings_headless();
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
        let shown = self.core.settings.sidebar.clone();
        for (page, icon, label, visible) in [
            (Page::Home, Icon::Home, "Home", shown.home),
            (Page::Gallery, Icon::Image, "Gallery", shown.gallery),
            (Page::Addons, Icon::Addons, "Add-ons", shown.addons),
            (Page::Settings, Icon::Settings, "Settings", true),
        ] {
            if visible
                && widgets::nav_item(ui, icon, label, self.core.page == page).clicked() && self.core.page != page {
                self.stop_recording();
                self.core.page = page;
            }
        }
        if shown.editor
            && editor_addon::is_installed()
            && widgets::nav_item(ui, Icon::Edit, "Editor ↗", false).clicked()
        {
            self.stop_recording();
            self.core.request_editor(EditorLaunchTarget::Empty);
        }
    }

    fn tray_status(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        if let UpdateState::Available(info) = &self.core.update_state {
            let ver = info.version.clone();
            let clicked = ui
                .scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
                    let hovered = ui.response().hovered();
                    Frame::new()
                        .fill(if hovered { p.control_hover } else { p.accent_soft })
                        .stroke(Stroke::new(1.0, p.accent))
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                RichText::new(format!("Update v{ver} available"))
                                    .font(semibold(12.5))
                                    .color(p.accent),
                            );
                            ui.label(
                                RichText::new("Click to view & install")
                                    .size(11.5)
                                    .color(p.muted),
                            );
                        });
                })
                .response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked();
            if clicked {
                self.stop_recording();
                self.core.page = Page::Settings;
                self.core.settings_tab = SettingsTab::About;
            }
            ui.add_space(8.0);
        }
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, p.success);
                ui.label(RichText::new("Running in tray").size(13.0).color(p.text));
            });
            let hint = match self.core.settings.close_action {
                CloseAction::HideToTray => "Closing this window keeps CapShotX in the tray.",
                CloseAction::Quit => "Closing this window quits CapShotX.",
            };
            ui.label(RichText::new(hint).size(11.5).color(p.muted));
            if let Some(ShortcutStatus::Active(hotkey)) = self.core.shortcut_status.get(&Action::OpenApp) {
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
        let editor_installed = editor_addon::is_installed();
        ui.label(RichText::new("Home").font(semibold(26.0)).color(p.text));
        ui.label(
            RichText::new(if editor_installed {
                "Capture. Edit. Record."
            } else {
                "Capture. Record."
            })
            .size(14.0)
            .color(p.muted),
        );
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
                let shortcut = self.core.settings.shortcut(action).to_owned();
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

        if editor_installed {
            ui.add_space(18.0);
            widgets::section_label(ui, "Edit");
            let edit_sc = self.core.settings.shortcut(Action::CaptureEdit).to_owned();
            let mut edit_action: Option<u8> = None; // 0 = capture & edit, 1 = open file, 2 = paste
            ui.columns(3, |cols| {
                let r0 = widgets::action_card(&mut cols[0], |ui| {
                    widgets::icon_tile(ui, Icon::Edit, 38.0);
                    ui.add_space(6.0);
                    ui.label(RichText::new("Capture & edit").font(semibold(15.5)).color(p.text));
                    ui.label(RichText::new("Take a screenshot and open it in the Editor").size(13.0).color(p.muted));
                    ui.add_space(4.0);
                    shortcut_footer(ui, &edit_sc);
                });
                if r0.clicked() {
                    edit_action = Some(0);
                }

                let r1 = widgets::action_card(&mut cols[1], |ui| {
                    widgets::icon_tile(ui, Icon::Image, 38.0);
                    ui.add_space(6.0);
                    ui.label(RichText::new("Open image").font(semibold(15.5)).color(p.text));
                    ui.label(RichText::new("Annotate, crop, redact, or frame an existing image").size(13.0).color(p.muted));
                    ui.add_space(4.0);
                    shortcut_footer(ui, "Ctrl+O");
                });
                if r1.clicked() {
                    edit_action = Some(1);
                }

                let r2 = widgets::action_card(&mut cols[2], |ui| {
                    widgets::icon_tile(ui, Icon::Edit, 38.0);
                    ui.add_space(6.0);
                    ui.label(RichText::new("Paste into Editor").font(semibold(15.5)).color(p.text));
                    ui.label(RichText::new("Edit an image currently on your clipboard").size(13.0).color(p.muted));
                    ui.add_space(4.0);
                    shortcut_footer(ui, "Ctrl+V");
                });
                if r2.clicked() {
                    edit_action = Some(2);
                }
            });
            match edit_action {
                Some(0) => {
                    self.start_capture_with_after(
                        ui.ctx(),
                        capture::CaptureMode::Region,
                        capture::AfterCapture::Edit,
                        true,
                    );
                }
                Some(1) => {
                    let dir = self.core.settings.capture.save_folder();
                    if let Some(path) = platform_windows::shell::pick_open_image(Some(&dir)) {
                        self.core.request_editor(editor_addon::target_for_path(&path));
                    }
                }
                Some(2) => {
                    self.core.request_editor(EditorLaunchTarget::Paste);
                }
                _ => {}
            }
        }

        ui.add_space(18.0);
        widgets::section_label(ui, "Record");
        let recording = self.core.screen.is_busy();
        let toggle_sc = self.core.settings.shortcut(Action::RecordToggle).to_owned();
        let fast_sc = self.core.settings.shortcut(Action::RecordFast).to_owned();
        let default_fast = self.core.settings.record.start_mode == RecordStartMode::Fast;
        let mut record_click: Option<Option<bool>> = None; // Some(None) = stop/toggle, Some(Some(fast)) = start
        ui.columns(3, |cols| {
            if recording {
                let ui = &mut cols[0];
                let response = widgets::action_card(ui, |ui| {
                    widgets::icon_tile(ui, Icon::Record, 38.0);
                    ui.add_space(6.0);
                    ui.label(RichText::new("Stop recording").font(semibold(15.5)).color(p.text));
                    ui.label(RichText::new("Finish and save the recording").size(13.0).color(p.muted));
                    ui.add_space(4.0);
                    shortcut_footer(ui, &toggle_sc);
                });
                if response.clicked() {
                    record_click = Some(None);
                }
            } else {
                let cards = [
                    (
                        "Record screen",
                        "Pick bounds, adjust on screen, then click Start",
                        if !default_fast { toggle_sc.as_str() } else { "" },
                        false,
                    ),
                    (
                        "Fast record",
                        "Pick an area or window and record immediately",
                        if !fast_sc.is_empty() {
                            fast_sc.as_str()
                        } else if default_fast {
                            toggle_sc.as_str()
                        } else {
                            ""
                        },
                        true,
                    ),
                ];
                for (ui, (title, desc, sc, fast)) in cols.iter_mut().zip(cards) {
                    let response = widgets::action_card(ui, |ui| {
                        widgets::icon_tile(ui, Icon::Record, 38.0);
                        ui.add_space(6.0);
                        ui.label(RichText::new(title).font(semibold(15.5)).color(p.text));
                        ui.label(RichText::new(desc).size(13.0).color(p.muted));
                        ui.add_space(4.0);
                        shortcut_footer(ui, sc);
                    });
                    if response.clicked() {
                        record_click = Some(Some(fast));
                    }
                }
            }
        });
        match record_click {
            Some(None) => self.core.toggle_screen_recording(),
            Some(Some(fast)) => {
                // Hide this window so it's neither in the picker nor recorded.
                self.hidden_temporarily = true;
                self.hidden_for_record = true;
                ui.ctx().send_viewport_cmd(ViewportCommand::Visible(false));
                let waker = self.core.waker.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(HIDE_BEFORE_CAPTURE);
                    waker.send(AppCommand::RecordStart { fast, from_app: true });
                });
            }
            None => {}
        }
    }

    fn addons_page(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        ui.label(RichText::new("Add-ons").font(semibold(26.0)).color(p.text));
        ui.label(
            RichText::new("Optional tools and extensions kept separate so CapShotX stays lightweight.")
                .size(14.0)
                .color(p.muted),
        );
        ui.add_space(18.0);

        widgets::section_label(ui, "Optional Add-on · Image Editor");
        widgets::card(ui, |ui| self.editor_addon_card(ui));
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        self.capture_shortcut(ui);
        self.refresh_monitors();

        let search_id = egui::Id::unique("settings-search");
        // Type-to-search only when no input box (text field, slider value,
        // colour hex, …) already has keyboard focus.
        if self.recording.is_none()
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| {
                i.events.iter().any(|e| match e {
                    Event::Text(t) => !t.is_empty() && !t.chars().all(char::is_control),
                    _ => false,
                })
            })
        {
            ui.memory_mut(|m| m.request_focus(search_id));
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), search_id) {
                let end = egui::text::CCursor::new(self.settings_search.chars().count());
                state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                state.store(ui.ctx(), search_id);
            }
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Settings").font(semibold(26.0)).color(p.text));
                ui.label(RichText::new("Changes are saved automatically.").size(14.0).color(p.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings_search)
                        .id(search_id)
                        .hint_text("Search settings…")
                        .desired_width(220.0),
                );
                if !self.settings_search.is_empty()
                    && ui
                        .add(egui::Button::new(RichText::new("Clear").size(12.5)).frame(false))
                        .clicked()
                {
                    self.settings_search.clear();
                }
            });
        });
        ui.add_space(14.0);

        let prev_tab = self.core.settings_tab;
        widgets::segmented(
            ui,
            &mut self.core.settings_tab,
            &[
                (SettingsTab::General, "General"),
                (SettingsTab::Capture, "Capture"),
                (SettingsTab::Recording, "Recording"),
                (SettingsTab::Hotkeys, "Hotkeys"),
                (SettingsTab::About, "Displays & About"),
            ],
        );
        if self.core.settings_tab != prev_tab {
            self.stop_recording();
            self.settings_search.clear();
        }

        let query = self.settings_search.trim().to_ascii_lowercase();
        let searching = !query.is_empty();
        let tab = self.core.settings_tab;

        let mut any_section = false;
        let space_section = |ui: &mut Ui, any: &mut bool| {
            if *any {
                ui.add_space(12.0);
            } else {
                ui.add_space(10.0);
            }
            *any = true;
        };

        // --- General (Window & tray + Appearance) ---
        if (searching && Self::general_matches(&query)) || (!searching && tab == SettingsTab::General) {
            if Self::window_tray_matches(&query) {
                space_section(ui, &mut any_section);
                widgets::section_label(ui, "Window & tray");
                self.window_tray_card(ui, &query);
            }
            if Self::appearance_matches(&query) {
                space_section(ui, &mut any_section);
                widgets::section_label(ui, "Appearance");
                widgets::card(ui, |ui| {
                    widgets::setting_row(ui, "Theme", "Follow Windows or choose a fixed theme", |ui| {
                        widgets::segmented(
                            ui,
                            &mut self.core.settings.theme,
                            &[(ThemeChoice::Dark, "Dark"), (ThemeChoice::Light, "Light"), (ThemeChoice::System, "System")],
                        );
                    });
                });
            }
            if Self::sidebar_matches(&query) {
                space_section(ui, &mut any_section);
                widgets::section_label(ui, "Sidebar");
                let s = &mut self.core.settings.sidebar;
                widgets::card(ui, |ui| {
                    let rows: [(&str, &str, &mut bool); 4] = [
                        ("Home", "Quick capture and record actions", &mut s.home),
                        ("Gallery", "Browse your screenshots and recordings", &mut s.gallery),
                        ("Add-ons", "Optional downloads such as the Image Editor", &mut s.addons),
                        ("Editor", "Opens the Image Editor when the add-on is installed", &mut s.editor),
                    ];
                    for (i, (title, desc, on)) in rows.into_iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        widgets::setting_row(ui, title, desc, |ui| {
                            widgets::toggle(ui, on);
                        });
                    }
                    ui.separator();
                    ui.label(RichText::new("Settings is always shown.").size(12.0).color(p.muted));
                });
            }
        }

        // --- Capture ---
        if (searching && Self::capture_matches(&query)) || (!searching && tab == SettingsTab::Capture) {
            space_section(ui, &mut any_section);
            widgets::section_label(ui, "Capture");
            widgets::card(ui, |ui| self.capture_card(ui, &query));
        }

        // --- Recording ---
        if (searching && RecordPanel::matches_query(&query)) || (!searching && tab == SettingsTab::Recording) {
            space_section(ui, &mut any_section);
            widgets::section_label(ui, "Recording");
            let recording = self.core.screen.is_busy();
            widgets::card(ui, |ui| {
                self.record_panel
                    .ui_filtered(ui, &mut self.core.settings.record, recording, &query);
            });
        }

        // --- Hotkeys ---
        if (searching && self.shortcuts_match(&query)) || (!searching && tab == SettingsTab::Hotkeys) {
            space_section(ui, &mut any_section);
            ui.horizontal(|ui| {
                widgets::section_label(ui, "Hotkeys");
                let any_custom = Action::ALL
                    .iter()
                    .any(|&a| self.core.settings.shortcut(a) != a.default_shortcut());
                if any_custom {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(RichText::new("Reset all to defaults").size(12.0)).frame(false))
                            .clicked()
                        {
                            self.stop_recording();
                            for &a in Action::ALL {
                                self.core.settings.set_shortcut(a, a.default_shortcut().to_owned());
                                self.core.apply_shortcut(a);
                            }
                        }
                    });
                }
            });
            widgets::card(ui, |ui| self.shortcuts_card(ui, &query));
        }

        // --- Displays & About ---
        if (searching && Self::about_matches(&query)) || (!searching && tab == SettingsTab::About) {
            if Self::displays_matches(&query) {
                space_section(ui, &mut any_section);
                widgets::section_label(ui, "Displays");
                widgets::card(ui, |ui| self.displays_card(ui));
            }
            if Self::updates_matches(&query) {
                space_section(ui, &mut any_section);
                widgets::section_label(ui, "Updates");
                widgets::card(ui, |ui| self.updates_card(ui));
            }
            if Self::about_card_matches(&query) {
                space_section(ui, &mut any_section);
                widgets::section_label(ui, "About");
                widgets::card(ui, |ui| self.about_card(ui));
            }
        }

        if searching && !any_section {
            ui.add_space(18.0);
            widgets::card(ui, |ui| {
                ui.label(
                    RichText::new(format!("No settings match \"{}\"", self.settings_search.trim()))
                        .font(semibold(14.5))
                        .color(p.text),
                );
                ui.label(
                    RichText::new("Try searching for hotkey, update, tray, admin, folder, audio, fps, or theme.")
                        .size(12.5)
                        .color(p.muted),
                );
            });
        }
    }

    fn window_tray_matches(q: &str) -> bool {
        const H: &str = "general window tray close button hide to tray quit tray left-click open app latest capture folder gallery settings launch at windows startup sign in autostart start hidden low memory mode standard windows menu capture admin windows administrator elevated task manager uac automatic update checks github releases";
        q.is_empty() || H.contains(q)
    }

    fn appearance_matches(q: &str) -> bool {
        const H: &str = "general appearance theme dark light system mode colour color";
        q.is_empty() || H.contains(q)
    }

    fn sidebar_matches(q: &str) -> bool {
        const H: &str = "general sidebar menu navigation nav hide show pages home gallery add-ons addons editor";
        q.is_empty() || H.contains(q)
    }

    fn general_matches(q: &str) -> bool {
        Self::window_tray_matches(q) || Self::appearance_matches(q) || Self::sidebar_matches(q)
    }

    fn capture_matches(q: &str) -> bool {
        const H: &str = "capture screenshot after capture copy save ask magnifier zoom pixel colour sound shutter click notification toast popup preview bottom-right highlight colour border thickness width save folder directory path file name template year month day date time";
        q.is_empty() || H.contains(q)
    }

    fn shortcuts_match(&self, q: &str) -> bool {
        if q.is_empty() || "hotkeys shortcuts keybinds keys combo".contains(q) {
            return true;
        }
        Action::ALL.iter().any(|&a| self.action_matches_query(a, q))
    }

    fn action_matches_query(&self, action: Action, q: &str) -> bool {
        if action == Action::CaptureEdit && !editor_addon::is_installed() {
            return false;
        }
        if q.is_empty() || "hotkeys shortcuts keybinds".contains(q) {
            return true;
        }
        action.label().to_ascii_lowercase().contains(q)
            || action.description().to_ascii_lowercase().contains(q)
            || self.core.settings.shortcut(action).to_ascii_lowercase().contains(q)
    }

    fn displays_matches(q: &str) -> bool {
        const H: &str = "displays monitors screens resolution scale dpi primary";
        q.is_empty() || H.contains(q)
    }

    fn updates_matches(q: &str) -> bool {
        const H: &str = "updates automatic update check upgrade version github release download install";
        q.is_empty() || H.contains(q)
    }

    fn about_card_matches(q: &str) -> bool {
        const H: &str = "about version logs folder settings json config path";
        q.is_empty() || H.contains(q)
    }

    fn about_matches(q: &str) -> bool {
        Self::displays_matches(q)
            || Self::updates_matches(q)
            || Self::about_card_matches(q)
    }

    fn window_tray_card(&mut self, ui: &mut Ui, q: &str) {
        let p = palette(ui);
        let show = |kw: &str| q.is_empty() || q == "general" || q == "window" || q == "tray" || kw.contains(q);
        let mut any = false;
        let sep = |ui: &mut Ui, any: &mut bool| {
            if *any {
                ui.separator();
            }
            *any = true;
        };

        let mut restart_admin = false;
        let mut toggled_admin_on = false;
        let mut trigger_update_check = false;
        {
            let is_elevated = self.core.is_elevated;
            let recording_busy = self.core.screen.is_busy();
            let startup_err = self.core.startup_error.clone();
            let s = &mut self.core.settings;
            widgets::card(ui, |ui| {
                if show("close button hide to tray quit exit window") {
                    sep(ui, &mut any);
                    widgets::setting_row(ui, "Close button", "What happens when you close this window", |ui| {
                        widgets::segmented(
                            ui,
                            &mut s.close_action,
                            &[(CloseAction::HideToTray, "Hide to tray"), (CloseAction::Quit, "Quit")],
                        );
                    });
                }
                if show("tray left-click left click action open app latest capture folder region record gallery settings") {
                    sep(ui, &mut any);
                    let left_desc = s.tray_left_click.description();
                    widgets::setting_row(ui, "Tray left-click", left_desc, |ui| {
                        let slot = vec2(236.0, 28.0);
                        ui.allocate_ui_with_layout(
                            slot,
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                egui::ComboBox::from_id_salt("tray-left-click")
                                    .width(236.0)
                                    .selected_text(s.tray_left_click.label())
                                    .show_ui(ui, |ui| {
                                        for &opt in TrayLeftClick::ALL {
                                            ui.selectable_value(&mut s.tray_left_click, opt, opt.label());
                                        }
                                    });
                            },
                        );
                    });
                }
                if show("launch at windows startup sign in boot autostart tray") {
                    sep(ui, &mut any);
                    widgets::setting_row(
                        ui,
                        "Launch at Windows startup",
                        "Start CapShotX in the tray automatically when you sign in to Windows",
                        |ui| {
                            widgets::toggle(ui, &mut s.launch_at_startup);
                        },
                    );
                    if let Some(err) = &startup_err {
                        ui.label(RichText::new(err).size(12.0).color(p.danger));
                    }
                }
                if show("start hidden tray background launch window") {
                    sep(ui, &mut any);
                    widgets::setting_row(ui, "Start hidden", "Launch straight to the tray without showing this window", |ui| {
                        widgets::toggle(ui, &mut s.start_hidden);
                    });
                }
                if show("low memory mode ram gpu standard windows tray context menu") {
                    sep(ui, &mut any);
                    widgets::setting_row(
                        ui,
                        "Low memory mode",
                        "Free window & GPU memory while in the tray and use the standard Windows tray menu",
                        |ui| {
                            widgets::toggle(ui, &mut s.low_memory_mode);
                        },
                    );
                }
                if show("automatic update checks github releases startup version") {
                    sep(ui, &mut any);
                    widgets::setting_row(
                        ui,
                        "Automatic update checks",
                        "Check GitHub Releases for new CapShotX versions when starting up",
                        |ui| {
                            if widgets::toggle(ui, &mut s.check_updates_on_startup).changed()
                                && s.check_updates_on_startup
                            {
                                trigger_update_check = true;
                            }
                        },
                    );
                }
                if show("capture admin windows administrator elevated task manager uac permissions") {
                    sep(ui, &mut any);
                    let admin_desc = if s.run_as_admin && !is_elevated {
                        "Not currently elevated — click Restart as admin to capture Task Manager and elevated apps"
                    } else {
                        "Opt-in: run as administrator so shortcuts and capture work over Task Manager and elevated apps"
                    };
                    widgets::setting_row(ui, "Capture admin windows", admin_desc, |ui| {
                        if widgets::toggle(ui, &mut s.run_as_admin).changed()
                            && s.run_as_admin
                            && !is_elevated
                            && !recording_busy
                        {
                            restart_admin = true;
                            toggled_admin_on = true;
                        }
                        if s.run_as_admin
                            && !is_elevated
                            && ui
                                .add_enabled(!recording_busy, egui::Button::new("Restart as admin"))
                                .clicked()
                        {
                            restart_admin = true;
                        }
                    });
                }
            });
        }
        if trigger_update_check {
            self.core.check_for_updates(false);
        }
        if restart_admin {
            self.core.persist_settings_headless();
            if platform_windows::shell::relaunch_as_admin(&["--wait-restart", "--open-settings"]).is_ok() {
                self.exit = UiExit::Quit;
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            } else if toggled_admin_on {
                // User cancelled the UAC prompt: snap the toggle back to off so
                // CapShotX won't prompt on next launch.
                self.core.settings.run_as_admin = false;
                self.core.persist_settings_headless();
            }
        }
    }

    fn updates_card(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let current = env!("CARGO_PKG_VERSION");
        let recording_busy = self.core.screen.is_busy();
        let state = self.core.update_state.clone();

        let (title, subtitle, sub_color) = match &state {
            UpdateState::Idle => (
                format!("Installed version: v{current}"),
                "Updates are downloaded directly from GitHub Releases and verified with SHA-256.".to_owned(),
                p.muted,
            ),
            UpdateState::Checking => (
                "Checking for updates…".to_owned(),
                format!("Querying github.com/{}/releases", platform_windows::updater::GITHUB_REPO),
                p.accent,
            ),
            UpdateState::UpToDate => (
                format!("You're up to date (v{current})"),
                "CapShotX is running the latest release on GitHub.".to_owned(),
                p.success,
            ),
            UpdateState::Available(info) => {
                let mb = info.exe_size as f64 / (1024.0 * 1024.0);
                let size_str = if mb > 0.1 {
                    format!(" ({mb:.1} MB)")
                } else {
                    String::new()
                };
                (
                    format!("CapShotX v{} is available{size_str}", info.version),
                    "Click Update & Restart to download, verify SHA-256, and upgrade in place.".to_owned(),
                    p.accent,
                )
            }
            UpdateState::Downloading(info) => (
                format!("Downloading & installing CapShotX v{}…", info.version),
                "Verifying SHA-256 digest and replacing executable — CapShotX will restart automatically.".to_owned(),
                p.accent,
            ),
            UpdateState::Error(err) => (
                "Update check or install failed".to_owned(),
                err.clone(),
                p.danger,
            ),
        };

        let mut do_check = false;
        let mut do_install = None;
        let mut open_url = None;

        let text_max_w = (ui.available_width() - 270.0).max(180.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width(text_max_w);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).size(14.5).color(p.text));
                ui.label(RichText::new(subtitle).size(12.5).color(sub_color));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &state {
                    UpdateState::Available(info) => {
                        if ui
                            .add_enabled(!recording_busy, egui::Button::new("Update & Restart"))
                            .on_disabled_hover_text("Stop the active screen recording before updating")
                            .clicked()
                        {
                            do_install = Some(info.clone());
                        }
                        if ui.button("Release notes").clicked() {
                            open_url = Some(info.html_url.clone());
                        }
                    }
                    UpdateState::Checking | UpdateState::Downloading(_) => {
                        ui.add_enabled(false, egui::Button::new("Working…"));
                    }
                    UpdateState::Idle | UpdateState::UpToDate | UpdateState::Error(_) => {
                        if ui.button("Check for updates").clicked() {
                            do_check = true;
                        }
                        if ui.button("GitHub Releases").clicked() {
                            open_url = Some(format!(
                                "https://github.com/{}/releases",
                                platform_windows::updater::GITHUB_REPO
                            ));
                        }
                    }
                }
            });
        });

        ui.separator();
        widgets::setting_row(
            ui,
            "Check for updates at startup",
            "Automatically check GitHub Releases in the background when CapShotX starts",
            |ui| {
                if widgets::toggle(ui, &mut self.core.settings.check_updates_on_startup).changed()
                    && self.core.settings.check_updates_on_startup
                {
                    do_check = true;
                }
            },
        );

        if do_check {
            self.core.check_for_updates(true);
        }
        if let Some(info) = do_install {
            self.core.install_update(info);
        }
        if let Some(url) = open_url {
            platform_windows::shell::open(std::path::Path::new(&url));
        }
    }

    fn editor_addon_card(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let installed = editor_addon::is_installed();
        let size_mb = editor_addon::installed_size_bytes()
            .map(|b| b as f64 / (1024.0 * 1024.0));
        let downloading = matches!(self.core.editor_addon_state, EditorAddonState::Downloading);

        let (title, subtitle, sub_color) = if downloading {
            (
                "Downloading CapShotX_Editor.exe from GitHub Releases…".to_owned(),
                "Verifying SHA-256 digest and installing alongside CapShotX.exe.".to_owned(),
                p.accent,
            )
        } else if let EditorAddonState::Error(err) = &self.core.editor_addon_state {
            (
                "Image Editor download failed".to_owned(),
                err.clone(),
                p.danger,
            )
        } else if installed {
            let size_badge = size_mb
                .map(|mb| format!(" ({mb:.1} MB)"))
                .unwrap_or_default();
            (
                format!("CapShotX Image Editor — Installed{size_badge}"),
                "Standalone pop-out editor (CapShotX_Editor.exe). Kept separate so CapShotX.exe stays ultra-lightweight.".to_owned(),
                p.success,
            )
        } else {
            (
                "CapShotX Image Editor — Not installed (Optional add-on)".to_owned(),
                "Download CapShotX_Editor.exe on demand from GitHub Releases whenever you want to annotate, crop, redact, or pixelate screenshots.".to_owned(),
                p.muted,
            )
        };

        let mut do_download = false;
        let mut do_open = false;
        let mut do_uninstall = false;

        let reserved_right = if downloading {
            150.0
        } else if installed {
            355.0
        } else {
            210.0
        };
        let text_max_w = (ui.available_width() - reserved_right).max(160.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width(text_max_w);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).size(14.5).color(p.text));
                ui.label(RichText::new(subtitle).size(12.5).color(sub_color));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if downloading {
                    ui.add_enabled(false, egui::Button::new("Downloading…"));
                } else if installed {
                    if ui
                        .button(RichText::new("Uninstall").color(p.danger))
                        .on_hover_text("Remove CapShotX_Editor.exe from disk")
                        .clicked()
                    {
                        do_uninstall = true;
                    }
                    if ui
                        .button("Reinstall / Update")
                        .on_hover_text("Re-download the latest CapShotX_Editor.exe from GitHub Releases")
                        .clicked()
                    {
                        do_download = true;
                    }
                    if ui.button("Open Editor").clicked() {
                        do_open = true;
                    }
                } else if ui.button("Download Image Editor").clicked() {
                    do_download = true;
                }
            });
        });

        if do_open {
            self.core.request_editor(EditorLaunchTarget::Empty);
        }
        if do_download {
            self.core.start_editor_download();
        }
        if do_uninstall {
            match editor_addon::uninstall_editor() {
                Ok(()) => {
                    if self.core.settings.capture.after == AfterCapture::Edit {
                        self.core.settings.capture.after = AfterCapture::Copy;
                    }
                    self.core.apply_shortcut(Action::CaptureEdit);
                }
                Err(err) => {
                    self.core.editor_addon_state = EditorAddonState::Error(err);
                }
            }
        }
    }

    fn editor_addon_modal(&mut self, ctx: &egui::Context) {
        if matches!(self.core.editor_addon_state, EditorAddonState::Idle) {
            return;
        }
        let downloading = matches!(self.core.editor_addon_state, EditorAddonState::Downloading);
        if downloading {
            ctx.request_repaint_after(Duration::from_millis(80));
        }

        let mut do_download = false;
        let mut do_cancel = false;

        egui::Window::new("Optional Add-on — Image Editor")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([460.0, 220.0])
            .show(ctx, |ui| {
                let p = palette(ui);
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.label(
                    RichText::new("Download the CapShotX Image Editor?")
                        .font(semibold(17.0))
                        .color(p.text),
                );
                ui.label(
                    RichText::new(
                        "To keep CapShotX ultra-lightweight, the full Image Editor (CapShotX_Editor.exe) is a separate optional download (~3 MB) from GitHub Releases.",
                    )
                    .size(13.0)
                    .color(p.muted),
                );

                if self.core.pending_editor_target.is_some() {
                    Frame::new()
                        .fill(p.accent_soft)
                        .stroke(Stroke::new(1.0, p.accent))
                        .corner_radius(8)
                        .inner_margin(Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(
                                    "Your screenshot is ready and will open automatically in the Editor as soon as the download finishes.",
                                )
                                .size(12.5)
                                .color(p.accent),
                            );
                        });
                }

                if let EditorAddonState::Error(err) = &self.core.editor_addon_state {
                    Frame::new()
                        .fill(p.control)
                        .stroke(Stroke::new(1.0, p.danger))
                        .corner_radius(8)
                        .inner_margin(Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.label(RichText::new(err).size(12.0).color(p.danger));
                        });
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if downloading {
                        ui.add_enabled(
                            false,
                            egui::Button::new(
                                RichText::new("Downloading CapShotX_Editor.exe…")
                                    .font(semibold(13.0)),
                            ),
                        );
                    } else {
                        let btn_label = if matches!(self.core.editor_addon_state, EditorAddonState::Error(_)) {
                            "Retry Download"
                        } else {
                            "Download & Open Image Editor"
                        };
                        let btn = egui::Button::new(
                            RichText::new(btn_label)
                                .font(semibold(13.0))
                                .color(egui::Color32::WHITE),
                        )
                        .fill(p.accent)
                        .corner_radius(8);
                        if ui.add(btn).clicked() {
                            do_download = true;
                        }
                        if ui.button("Not now").clicked() {
                            do_cancel = true;
                        }
                    }
                });
            });

        if do_download {
            self.core.start_editor_download();
        }
        if do_cancel {
            self.core.cancel_editor_prompt();
        }
    }

    fn about_card(&self, ui: &mut Ui) {
        let p = palette(ui);
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
        if let Some(err) = &self.core.settings_error {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Could not save settings: {err}"))
                    .size(12.5)
                    .color(p.danger),
            );
        }
    }

    fn capture_card(&mut self, ui: &mut Ui, q: &str) {
        let p = palette(ui);
        let c = &mut self.core.settings.capture;
        let show = |kw: &str| q.is_empty() || q == "capture" || kw.contains(q);
        let mut any = false;
        let sep = |ui: &mut Ui, any: &mut bool| {
            if *any {
                ui.separator();
            }
            *any = true;
        };

        if show("after capture copy save edit ask") {
            sep(ui, &mut any);
            let editor_installed = editor_addon::is_installed();
            if !editor_installed && c.after == AfterCapture::Edit {
                c.after = AfterCapture::Copy;
            }
            widgets::setting_row(ui, "After capture", "What happens when you finish selecting", |ui| {
                if editor_installed {
                    widgets::segmented(
                        ui,
                        &mut c.after,
                        &[
                            (AfterCapture::Copy, "Copy"),
                            (AfterCapture::CopyAndSave, "Copy & save"),
                            (AfterCapture::Edit, "Open in Editor"),
                            (AfterCapture::Ask, "Ask each time"),
                        ],
                    );
                } else {
                    widgets::segmented(
                        ui,
                        &mut c.after,
                        &[
                            (AfterCapture::Copy, "Copy"),
                            (AfterCapture::CopyAndSave, "Copy & save"),
                            (AfterCapture::Ask, "Ask each time"),
                        ],
                    );
                }
            });
        }
        if show("magnifier pixel zoom colour cursor") {
            sep(ui, &mut any);
            widgets::setting_row(ui, "Magnifier", "Pixel zoom and colour under the cursor · press Z while capturing", |ui| {
                widgets::toggle(ui, &mut c.magnifier);
            });
        }
        if show("capture sound shutter click audio") {
            sep(ui, &mut any);
            widgets::setting_row(ui, "Capture sound", "A soft shutter click when a screenshot is taken", |ui| {
                if widgets::toggle(ui, &mut c.sound).changed() && c.sound {
                    platform_windows::sound::play_shutter();
                }
            });
        }
        if show("capture notification screenshot toast popup preview bottom-right") {
            sep(ui, &mut any);
            widgets::setting_row(
                ui,
                "Capture notification",
                "Show a small preview notification in the bottom-right corner after capturing",
                |ui| {
                    widgets::toggle(ui, &mut c.show_notification);
                },
            );
        }
        if show("highlight colour color border selection handles") {
            sep(ui, &mut any);
            widgets::setting_row(ui, "Highlight colour", "Screenshot selection border and handles", |ui| {
                egui::widgets::color_picker::color_edit_button_srgb(ui, &mut c.highlight);
                for preset in crate::settings::HIGHLIGHT_PRESETS.iter().rev() {
                    if widgets::swatch(ui, *preset, c.highlight == *preset).clicked() {
                        c.highlight = *preset;
                    }
                }
            });
        }
        if show("border thickness width selection outline") {
            sep(ui, &mut any);
            widgets::setting_row(ui, "Border thickness", "Width of the screenshot selection border", |ui| {
                ui.add(egui::Slider::new(&mut c.border_width, crate::settings::BORDER_WIDTH_RANGE).suffix(" px"));
            });
        }
        if show("save folder directory path location pictures") {
            sep(ui, &mut any);
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
        }
        if show("file name template format year month day date time") {
            sep(ui, &mut any);
            let now = platform_windows::shell::local_time();
            let img_preview = capture::render_output_path(&c.file_template, capture::OutputKind::Image, now);
            let vid_preview = capture::render_output_path(&c.file_template, capture::OutputKind::Video, now);
            let (description, color) = match (&img_preview, &vid_preview) {
                (Ok(img), Ok(vid)) => (
                    format!("Images: {}   ·   Videos: {}", img.display(), vid.display()),
                    p.muted,
                ),
                (Err(err), _) | (_, Err(err)) => (err.clone(), p.danger),
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
                        .id(egui::Id::unique("capture-file-template"))
                        .desired_width(300.0)
                        .font(egui::TextStyle::Monospace),
                )
                .on_hover_text("Tokens: {type} {date} {time} {year} {month} {day} {hour} {minute} {second} {ms}. Use / for folders.");
            });
            ui.label(RichText::new(description).size(12.5).color(color));
        }
    }

    fn shortcuts_card(&mut self, ui: &mut Ui, q: &str) {
        let p = palette(ui);
        let mut shown = 0usize;
        for &action in Action::ALL {
            if !self.action_matches_query(action, q) {
                continue;
            }
            if shown > 0 {
                ui.separator();
            }
            shown += 1;

            let recording = self.recording.as_ref().filter(|r| r.action == action);
            let held_mods = recording
                .map(|r| r.key_capture.held_modifiers())
                .unwrap_or_default();
            let (description, color) = match (recording, self.core.shortcut_status.get(&action)) {
                (Some(r), _) => (
                    r.hint.unwrap_or("Press any key or combo (e.g. Ctrl+X) · Esc cancels · Backspace clears"),
                    if r.hint.is_some() { p.danger } else { p.accent },
                ),
                (None, Some(ShortcutStatus::Failed(err))) => (err.as_str(), p.danger),

                _ => (action.description(), p.muted),
            };
            let is_recording = recording.is_some();
            let current = self.core.settings.shortcut(action).to_owned();
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
                    clicked = shortcut_field(ui, &current, is_recording, &held_mods).clicked();
                    if !is_default && !is_recording {
                        let tip = if action.default_shortcut().is_empty() {
                            "Clear shortcut".to_owned()
                        } else {
                            format!("Restore {}", action.default_shortcut())
                        };
                        reset = ui
                            .add(egui::Button::new(RichText::new("Reset").size(12.5)).frame(false))
                            .on_hover_text(tip)
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
                self.core.settings.set_shortcut(action, action.default_shortcut().to_owned());
                self.core.apply_shortcut(action);
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

impl eframe::App for CapShotApp<'_> {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.hidden_temporarily {
            ctx.input(|i| {
                let vp = i.viewport();
                let minimized = vp.minimized == Some(true);
                let maximized = vp.maximized == Some(true);
                if !minimized {
                    self.core.window_maximized = maximized;
                    if !maximized {
                        if let Some(inner) = vp.inner_rect
                            && inner.width() >= 400.0
                            && inner.height() >= 300.0
                        {
                            self.core.window_size = [inner.width(), inner.height()];
                        }
                        if let Some(outer) = vp.outer_rect {
                            let ppp = vp.native_pixels_per_point.unwrap_or(1.0);
                            self.core.window_pos_px = Some([outer.min.x * ppp, outer.min.y * ppp]);
                        }
                    }
                }
            });
        }

        while let Ok(cmd) = self.core.commands.try_recv() {
            match cmd {
                AppCommand::TrayLeftClick => match self.core.settings.tray_left_click {
                    TrayLeftClick::OpenApp => self.show_window(ctx),
                    TrayLeftClick::OpenGallery => {
                        self.core.page = Page::Gallery;
                        self.show_window(ctx);
                    }
                    TrayLeftClick::OpenSettings => {
                        self.core.page = Page::Settings;
                        self.show_window(ctx);
                    }
                    TrayLeftClick::OpenLatestCaptureFolder => self.core.open_latest_capture_folder(),
                    TrayLeftClick::OpenCapturesFolder => self.core.open_captures_folder(),
                    TrayLeftClick::CaptureRegion => {
                        self.start_capture(ctx, capture::CaptureMode::Region, false);
                    }
                    TrayLeftClick::RecordRegion => self.core.toggle_screen_recording(),
                    TrayLeftClick::FastRecord => self.core.start_screen_recording(true, false),
                    TrayLeftClick::Nothing => {}
                },
                AppCommand::Open => self.show_window(ctx),
                AppCommand::OpenEditor => {
                    if !self.core.request_editor(EditorLaunchTarget::Empty) {
                        self.show_window(ctx);
                    }
                }
                AppCommand::OpenSettings => {
                    self.core.page = Page::Settings;
                    self.show_window(ctx);
                }
                AppCommand::Quit => {
                    if self.core.screen.is_recording() {
                        // Finalise the file first; quit when it's saved.
                        self.core.quit_after_saving = true;
                        self.core.screen.stop();
                    } else {
                        self.core.screen.cancel_starting();
                        self.exit = UiExit::Quit;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                AppCommand::RecordToggle => self.core.toggle_screen_recording(),
                AppCommand::RecordStart { fast, from_app } => {
                    self.core.start_screen_recording(fast, from_app);
                }
                AppCommand::RecordSyncAudio { desktop_audio, microphone } => {
                    self.core.settings.record.desktop_audio = desktop_audio;
                    self.core.settings.record.microphone = microphone;
                }
                AppCommand::RecordingStop => self.core.screen.stop(),
                AppCommand::RecordingStarted(result) => {
                    let ok = result.is_ok();
                    let dark = ctx.theme() == egui::Theme::Dark;
                    if self.core.screen.on_started(result, dark) && self.exit != UiExit::Quit {
                        self.show_window(ctx);
                    } else if ok && self.hidden_for_record && self.exit != UiExit::Quit {
                        self.hidden_for_record = false;
                        if self.core.settings.low_memory_mode {
                            // Recording is now running from Home while the window is hidden;
                            // tear down the UI session so recording runs without GPU UI overhead.
                            self.exit = UiExit::HideToTray;
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                    }
                    self.core.tray.set_recording(self.core.screen.is_busy());
                }
                AppCommand::RecordingFinished(result) => {
                    if result.is_ok() {
                        self.gallery.request_rescan();
                    }
                    if let Some(saved) =
                        self.core.screen.on_finished(result, ctx.theme() == egui::Theme::Dark)
                    {
                        self.core.last_saved_folder = saved.parent().map(std::path::Path::to_path_buf);
                    }
                    self.core.tray.set_recording(false);
                    if self.core.quit_after_saving {
                        self.exit = UiExit::Quit;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                AppCommand::RecoveredRecordings(paths) => {
                    self.gallery.request_rescan();
                    crate::screen_record::notify_recovered(paths, ctx.theme() == egui::Theme::Dark);
                }
                AppCommand::Hotkey(action) => self.run_action(ctx, action),
                AppCommand::CaptureFinished { finished, restore_window } => {
                    self.on_capture_finished(ctx, finished, restore_window);
                }
                AppCommand::UpdateCheckFinished { result, manual } => {
                    self.core.on_update_check_finished(result, manual);
                }
                AppCommand::UpdateInstallFinished(Ok(())) => {
                    self.exit = UiExit::Quit;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                AppCommand::UpdateInstallFinished(Err(err)) => {
                    tracing::warn!("update install failed: {err}");
                    self.core.update_state = UpdateState::Error(err);
                }
                AppCommand::EditorInstallFinished(res) => {
                    self.core.on_editor_install_finished(res);
                }
            }
        }

        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && self.exit != UiExit::Quit {
            match self.core.settings.close_action {
                CloseAction::Quit if self.core.screen.is_recording() => {
                    // Don't kill a recording mid-write: save it, then quit.
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    self.core.quit_after_saving = true;
                    self.core.screen.stop();
                }
                CloseAction::Quit => {
                    self.core.screen.cancel_starting();
                    self.exit = UiExit::Quit;
                }
                CloseAction::HideToTray if self.core.settings.low_memory_mode => {
                    // Let eframe close and destroy the window + GL stack;
                    // main() resumes the zero-GPU Win32 tray loop.
                    self.stop_recording();
                    self.record_panel.release();
                    self.exit = UiExit::HideToTray;
                }
                CloseAction::HideToTray => {
                    // Keep the window and GL context warm in the background.
                    self.stop_recording();
                    self.record_panel.release();
                    self.gallery.release_preview();
                    self.hidden_temporarily = true;
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                }
            }
        }

        // Audio level monitors and full-resolution previews only stay alive on their pages/tabs.
        let rec_tab_active = self.core.page == Page::Settings
            && (self.core.settings_tab == SettingsTab::Recording
                || !self.settings_search.trim().is_empty());
        if !rec_tab_active {
            self.record_panel.release();
        }
        if self.core.page != Page::Gallery {
            self.gallery.release_preview();
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
                if self.core.page == Page::Gallery {
                    // The gallery scrolls (and virtualises) its own grid.
                    if let Some(edit_path) = self.gallery.page_ui(ui) {
                        self.core.request_editor(editor_addon::target_for_path(&edit_path));
                    }
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Leave a gutter so the scrollbar never covers card edges.
                    Frame::new()
                        .inner_margin(Margin { right: 14, ..Margin::ZERO })
                        .show(ui, |ui| match self.core.page {
                            Page::Home => self.home(ui),
                            Page::Addons => self.addons_page(ui),
                            Page::Settings => self.settings_page(ui),
                            Page::Gallery => {}
                        });
                });
            });

        self.editor_addon_modal(ui.ctx());

        if self.core.page == Page::Settings {
            // Keeps the Displays list current without a display-change hook.
            ui.ctx().request_repaint_after(MONITOR_REFRESH);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_recording();
        self.record_panel.release();
        self.gallery.save_cache(&mut self.core.gallery_cache);
        self.core.persist_settings_headless();
        self.core.waker.detach_ctx();
        *self.exit_out = self.exit;
    }
}

impl Drop for CapShotApp<'_> {
    fn drop(&mut self) {
        self.core.waker.detach_ctx();
    }
}

/// Clickable field showing a shortcut as key chips.
fn shortcut_field(
    ui: &mut Ui,
    shortcut: &str,
    recording: bool,
    held_modifiers: &[&str],
) -> egui::Response {
    let p = palette(ui);
    if recording {
        ui.ctx().request_repaint_after(Duration::from_millis(30));
    }
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
                            if held_modifiers.is_empty() {
                                ui.label(RichText::new("Press keys…").size(13.0).color(p.accent));
                            } else {
                                widgets::shortcut(ui, held_modifiers);
                                ui.label(RichText::new("+ …").size(13.0).color(p.accent));
                            }
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

fn is_modifier_key(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

fn egui_key_to_hotkey(key: Key, modifiers: Modifiers) -> Option<Hotkey> {
    let (w_ctrl, w_shift, w_alt, w_win) = current_modifiers();
    let vk = platform_windows::hotkeys::vk_from_name(key.name())?;
    Some(Hotkey {
        ctrl: modifiers.ctrl || modifiers.command || w_ctrl,
        shift: modifiers.shift || w_shift,
        alt: modifiers.alt || w_alt,
        win: w_win,
        vk,
    })
}

fn open_folder(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}

fn shortcut_footer(ui: &mut Ui, shortcut: &str) {
    let p = palette(ui);
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
