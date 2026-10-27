#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use egui::{
    Align, Button, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    Frame, Layout, Margin, RichText, Stroke, SystemTheme, TextureHandle, Theme, ThemePreference,
    Ui, ViewportCommand, Visuals, vec2,
};
use flate2::read::ZlibDecoder;
use platform_windows::{monitors, setup_ops, shell, startup};

static PAYLOAD_ZLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/CapShotX.exe.zz"));
static PAYLOAD_RAW_LEN_STR: &str = include_str!(concat!(env!("OUT_DIR"), "/payload_size.txt"));

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAIN_EXE_NAME: &str = "CapShotX.exe";
const UNINSTALL_EXE_NAME: &str = "Uninstall CapShotX.exe";
const SHORTCUT_DESC: &str = "CapShotX — Fast native screenshots & screen recording";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallOptions {
    pub install_dir: PathBuf,
    pub create_start_menu: bool,
    pub create_desktop: bool,
    pub launch_at_startup: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            install_dir: setup_ops::default_install_dir(),
            create_start_menu: true,
            // Explicitly unticked by default per product requirements.
            create_desktop: false,
            launch_at_startup: true,
        }
    }
}

#[derive(Clone, Debug)]
struct InstallOutcome {
    install_dir: PathBuf,
    installed_exe: PathBuf,
}

#[derive(Clone, Debug)]
struct CliArgs {
    silent: bool,
    uninstall: bool,
    uninstall_dir: Option<PathBuf>,
    self_temp: bool,
    custom_dir: Option<PathBuf>,
    desktop_icon: bool,
    remove_app_data: bool,
}

impl CliArgs {
    fn parse() -> Self {
        let mut out = Self {
            silent: false,
            uninstall: false,
            uninstall_dir: None,
            self_temp: false,
            custom_dir: None,
            desktop_icon: false,
            remove_app_data: false,
        };

        if let Ok(exe) = std::env::current_exe()
            && let Some(stem) = exe.file_stem().and_then(|s| s.to_str())
            && stem.to_ascii_lowercase().starts_with("uninstall")
        {
            out.uninstall = true;
        }

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--silent" | "-s" | "/S" | "/quiet" => out.silent = true,
                "--uninstall" | "/uninstall" => out.uninstall = true,
                "--self-temp" => out.self_temp = true,
                "--desktop-icon" => out.desktop_icon = true,
                "--purge-data" => out.remove_app_data = true,
                "--uninstall-dir" => {
                    if let Some(dir) = args.next() {
                        out.uninstall = true;
                        out.uninstall_dir = Some(PathBuf::from(dir));
                    }
                }
                "--dir" => {
                    if let Some(dir) = args.next() {
                        out.custom_dir = Some(PathBuf::from(dir));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

fn main() -> anyhow::Result<()> {
    shell::init_process();
    let cli = CliArgs::parse();

    if cli.uninstall {
        return run_uninstaller(cli);
    }

    let mut opts = InstallOptions::default();
    if let Some(dir) = cli.custom_dir {
        opts.install_dir = dir;
    }
    if cli.desktop_icon {
        opts.create_desktop = true;
    }

    if cli.silent {
        perform_install(&opts).map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(());
    }

    run_installer_gui(opts)
}

fn run_uninstaller(cli: CliArgs) -> anyhow::Result<()> {
    let current_exe = std::env::current_exe()?;
    let target_dir = cli
        .uninstall_dir
        .clone()
        .or_else(setup_ops::existing_install_dir)
        .or_else(|| {
            current_exe
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| s.to_ascii_lowercase().starts_with("uninstall"))
                .and_then(|_| current_exe.parent().map(Path::to_path_buf))
        })
        .unwrap_or_else(setup_ops::default_install_dir);

    // If the uninstaller is running from inside the install directory, copy it
    // to %TEMP% and re-exec so the install folder can be deleted cleanly.
    if !cli.self_temp && is_inside_dir(&current_exe, &target_dir) {
        let temp_exe =
            std::env::temp_dir().join(format!("CapShotX-Uninstall-{}.exe", std::process::id()));
        std::fs::copy(&current_exe, &temp_exe)?;
        let mut cmd = Command::new(&temp_exe);
        cmd.arg("--uninstall")
            .arg("--uninstall-dir")
            .arg(&target_dir)
            .arg("--self-temp");
        if cli.silent {
            cmd.arg("--silent");
        }
        if cli.remove_app_data {
            cmd.arg("--purge-data");
        }
        cmd.spawn()?;
        return Ok(());
    }

    if cli.silent {
        perform_uninstall(&target_dir, cli.remove_app_data)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if cli.self_temp {
            setup_ops::schedule_self_cleanup(&current_exe, &target_dir);
        }
        return Ok(());
    }

    let mut did_uninstall = false;
    run_uninstaller_gui(target_dir.clone(), &mut did_uninstall)?;
    if did_uninstall && cli.self_temp {
        setup_ops::schedule_self_cleanup(&current_exe, &target_dir);
    }
    Ok(())
}

fn is_inside_dir(exe: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_ascii_lowercase()
    };
    let e = norm(exe);
    let mut d = norm(dir);
    if !d.ends_with('\\') {
        d.push('\\');
    }
    e.starts_with(&d)
}

fn payload_raw_len() -> usize {
    PAYLOAD_RAW_LEN_STR.trim().parse::<usize>().unwrap_or(0)
}

fn decompress_payload() -> Result<Vec<u8>, String> {
    let expected = payload_raw_len();
    if expected <= 1024 {
        return Err(
            "This development build of CapShotX-Setup.exe was compiled without a release CapShotX.exe payload. Run packaging\\build-installer.ps1 to build the full installer."
                .to_owned(),
        );
    }
    let mut out = Vec::with_capacity(expected);
    let mut dec = ZlibDecoder::new(PAYLOAD_ZLIB);
    dec.read_to_end(&mut out)
        .map_err(|e| format!("Failed to decompress CapShotX.exe payload: {e}"))?;
    if out.len() != expected {
        return Err(format!(
            "Payload size mismatch (expected {expected} bytes, got {} bytes)",
            out.len()
        ));
    }
    Ok(out)
}

fn perform_install(opts: &InstallOptions) -> Result<InstallOutcome, String> {
    let dir_str = opts.install_dir.to_string_lossy();
    if dir_str.trim().is_empty() {
        return Err("Please choose an installation folder.".to_owned());
    }

    let payload = decompress_payload()?;
    setup_ops::stop_running_processes(MAIN_EXE_NAME)?;

    std::fs::create_dir_all(&opts.install_dir).map_err(|e| {
        format!(
            "Could not create installation folder {}: {e}",
            opts.install_dir.display()
        )
    })?;

    let installed_exe = opts.install_dir.join(MAIN_EXE_NAME);
    let tmp_exe = opts.install_dir.join("CapShotX.exe.tmp");
    std::fs::write(&tmp_exe, &payload)
        .map_err(|e| format!("Could not write {}: {e}", tmp_exe.display()))?;
    if installed_exe.exists() {
        std::fs::remove_file(&installed_exe).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_exe);
            format!(
                "Could not replace {} (is CapShotX still open?): {e}",
                installed_exe.display()
            )
        })?;
    }
    std::fs::rename(&tmp_exe, &installed_exe)
        .map_err(|e| format!("Could not finalise {}: {e}", installed_exe.display()))?;

    let uninstaller_exe = opts.install_dir.join(UNINSTALL_EXE_NAME);
    if let Ok(self_exe) = std::env::current_exe()
        && !paths_equal(&self_exe, &uninstaller_exe)
    {
        std::fs::copy(&self_exe, &uninstaller_exe)
            .map_err(|e| format!("Could not install {}: {e}", uninstaller_exe.display()))?;
    }

    if opts.create_start_menu
        && let Some(lnk) = setup_ops::start_menu_shortcut_path()
    {
        setup_ops::create_shortcut(&lnk, &installed_exe, &opts.install_dir, SHORTCUT_DESC)?;
    }
    if opts.create_desktop
        && let Some(lnk) = setup_ops::desktop_shortcut_path()
    {
        setup_ops::create_shortcut(&lnk, &installed_exe, &opts.install_dir, SHORTCUT_DESC)?;
    }

    let existing_run_as_admin = sync_settings_file_startup(opts.launch_at_startup)?;
    startup::sync_for_exe(&installed_exe, opts.launch_at_startup, existing_run_as_admin)?;

    let uninst_size = std::fs::metadata(&uninstaller_exe).map_or(0, |m| m.len());
    let estimated_kb = ((payload.len() as u64 + uninst_size) / 1024).max(1) as u32;
    setup_ops::register_uninstaller(
        &opts.install_dir,
        &installed_exe,
        &uninstaller_exe,
        APP_VERSION,
        estimated_kb,
    )?;

    Ok(InstallOutcome {
        install_dir: opts.install_dir.clone(),
        installed_exe,
    })
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

fn settings_file_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "CapShotX")
        .map(|dirs| dirs.config_dir().join("settings.json"))
}

/// Updates `launch_at_startup` inside `settings.json` while preserving every
/// existing user preference (and keeping `run_as_admin` untouched / `false` by
/// default). Returns the effective `run_as_admin` flag.
fn sync_settings_file_startup(launch_at_startup: bool) -> Result<bool, String> {
    let Some(path) = settings_file_path() else {
        return Ok(false);
    };
    merge_startup_into_settings_file(&path, launch_at_startup)
}

fn merge_startup_into_settings_file(path: &Path, launch_at_startup: bool) -> Result<bool, String> {
    let existing_raw = std::fs::read(path).ok();
    let (merged, run_as_admin) = merge_startup_json(existing_raw.as_deref(), launch_at_startup);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&merged)
        .map_err(|e| format!("Could not serialise settings.json: {e}"))?;
    std::fs::write(&tmp, bytes)
        .map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("Could not save {}: {e}", path.display()))?;
    Ok(run_as_admin)
}

fn merge_startup_json(existing: Option<&[u8]>, launch_at_startup: bool) -> (serde_json::Value, bool) {
    let mut obj = existing
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| match v {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();

    // `run_as_admin` is strictly opt-in (false unless the user previously enabled it).
    let run_as_admin = obj
        .get("run_as_admin")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    obj.insert(
        "launch_at_startup".to_owned(),
        serde_json::Value::Bool(launch_at_startup),
    );
    obj.entry("run_as_admin".to_owned())
        .or_insert(serde_json::Value::Bool(false));

    (serde_json::Value::Object(obj), run_as_admin)
}

fn perform_uninstall(install_dir: &Path, remove_app_data: bool) -> Result<(), String> {
    setup_ops::stop_running_processes(MAIN_EXE_NAME)?;

    if let Some(lnk) = setup_ops::start_menu_shortcut_path()
        && lnk.exists()
    {
        let _ = std::fs::remove_file(lnk);
    }
    if let Some(lnk) = setup_ops::desktop_shortcut_path()
        && lnk.exists()
    {
        let _ = std::fs::remove_file(lnk);
    }

    startup::remove_all();
    setup_ops::unregister_uninstaller();

    let main_exe = install_dir.join(MAIN_EXE_NAME);
    if main_exe.exists() {
        std::fs::remove_file(&main_exe)
            .map_err(|e| format!("Could not remove {}: {e}", main_exe.display()))?;
    }
    let uninst_exe = install_dir.join(UNINSTALL_EXE_NAME);
    if uninst_exe.exists() {
        let _ = std::fs::remove_file(&uninst_exe);
    }
    // Removes install_dir only when empty; never touches extra user files.
    let _ = std::fs::remove_dir(install_dir);

    if remove_app_data
        && let Some(dirs) = directories::ProjectDirs::from("", "", "CapShotX")
    {
        if let Some(roaming_root) = dirs.config_dir().parent() {
            let _ = std::fs::remove_dir_all(roaming_root);
        }
        if let Some(local_root) = dirs.data_local_dir().parent() {
            let _ = std::fs::remove_dir_all(local_root);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// GUI Setup & Uninstaller (eframe / egui)
// ---------------------------------------------------------------------------

fn centered_viewport(title: &str, size: [f32; 2]) -> egui::ViewportBuilder {
    let mut vp = egui::ViewportBuilder::default()
        .with_title(title)
        .with_app_id("CapShotX.Setup")
        .with_inner_size(size)
        .with_min_inner_size(size)
        .with_resizable(false)
        .with_maximize_button(false)
        .with_icon(Arc::new(egui::IconData {
            rgba: icon_rgba(64),
            width: 64,
            height: 64,
        }));

    let displays = monitors::enumerate();
    if let Some(primary) = displays.iter().find(|m| m.primary).or_else(|| displays.first()) {
        let scale = primary.scale().max(0.5);
        let mon_w = primary.bounds.width() as f32 / scale;
        let mon_h = primary.bounds.height() as f32 / scale;
        let x = primary.bounds.left as f32 / scale + ((mon_w - size[0]) * 0.5).max(0.0);
        let y = primary.bounds.top as f32 / scale + ((mon_h - size[1]) * 0.45).max(0.0);
        vp = vp.with_position([x, y]);
    }
    vp
}

fn run_installer_gui(opts: InstallOptions) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: centered_viewport("CapShotX Setup", [520.0, 445.0]),
        ..Default::default()
    };
    let is_upgrade = setup_ops::existing_install_dir().is_some();
    eframe::run_native(
        "CapShotX Setup",
        options,
        Box::new(move |cc| Ok(Box::new(SetupApp::new(cc, opts, is_upgrade)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

fn run_uninstaller_gui(install_dir: PathBuf, did_uninstall: &mut bool) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: centered_viewport("Uninstall CapShotX", [480.0, 295.0]),
        run_and_return: true,
        ..Default::default()
    };
    eframe::run_native("Uninstall CapShotX", options, {
        let flag = &mut *did_uninstall;
        Box::new(move |cc| Ok(Box::new(UninstallApp::new(cc, install_dir, flag))))
    })
    .map_err(|e| anyhow::anyhow!("{e}"))
}

enum SetupStep {
    Configure,
    Installing { rx: Receiver<Result<InstallOutcome, String>> },
    Finished(InstallOutcome),
}

struct SetupApp {
    opts: InstallOptions,
    dir_text: String,
    is_upgrade: bool,
    step: SetupStep,
    error: Option<String>,
    launch_now: bool,
    logo: TextureHandle,
}

impl SetupApp {
    fn new(cc: &eframe::CreationContext<'_>, opts: InstallOptions, is_upgrade: bool) -> Self {
        let ctx = &cc.egui_ctx;
        install_theme(ctx);
        let logo = ctx.load_texture(
            "capshotx-logo",
            egui::ColorImage::from_rgba_unmultiplied([96, 96], &icon_rgba(96)),
            egui::TextureOptions::LINEAR,
        );
        let dir_text = opts.install_dir.to_string_lossy().into_owned();
        Self {
            opts,
            dir_text,
            is_upgrade,
            step: SetupStep::Configure,
            error: None,
            launch_now: true,
            logo,
        }
    }

    fn start_install(&mut self, ctx: &Context) {
        self.error = None;
        self.opts.install_dir = PathBuf::from(self.dir_text.trim());
        let opts = self.opts.clone();
        let ctx_clone = ctx.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = perform_install(&opts);
            let _ = tx.send(res);
            ctx_clone.request_repaint();
        });
        self.step = SetupStep::Installing { rx };
    }
}

impl eframe::App for SetupApp {
    fn logic(&mut self, _ctx: &Context, _frame: &mut eframe::Frame) {
        if let SetupStep::Installing { rx } = &self.step
            && let Ok(result) = rx.try_recv()
        {
            match result {
                Ok(done) => self.step = SetupStep::Finished(done),
                Err(err) => {
                    self.error = Some(err);
                    self.step = SetupStep::Configure;
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let p = Palette::of(ui.visuals().dark_mode);

        egui::Panel::bottom("setup_footer")
            .exact_size(64.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.sidebar)
                    .stroke(Stroke::new(1.0, p.stroke))
                    .inner_margin(Margin::symmetric(24, 14)),
            )
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    match &self.step {
                        SetupStep::Configure => {
                            if secondary_button(ui, p, "Cancel").clicked() {
                                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let label = if self.is_upgrade { "Update" } else { "Install" };
                                if primary_button(ui, p, label, p.accent).clicked() {
                                    let ctx = ui.ctx().clone();
                                    self.start_install(&ctx);
                                }
                            });
                        }
                        SetupStep::Installing { .. } => {
                            ui.label(RichText::new("Installing files…").size(13.0).color(p.muted));
                        }
                        SetupStep::Finished(done) => {
                            let exe = done.installed_exe.clone();
                            let launch = self.launch_now;
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if primary_button(ui, p, "Finish", p.accent).clicked() {
                                    if launch {
                                        let _ = setup_ops::spawn_detached(&exe);
                                    }
                                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                                }
                            });
                        }
                    }
                });
            });

        egui::CentralPanel::no_frame()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin::symmetric(24, 20)))
            .show(ui, |ui| {
                // Header
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::from_texture(&self.logo)
                            .fit_to_exact_size(vec2(46.0, 46.0))
                            .corner_radius(11),
                    );
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("CapShotX Setup")
                                    .font(semibold(20.0))
                                    .color(p.text),
                            );
                            let badge_text = if self.is_upgrade {
                                format!("v{APP_VERSION} • Update")
                            } else {
                                format!("v{APP_VERSION}")
                            };
                            Frame::new()
                                .fill(p.accent_soft)
                                .corner_radius(6)
                                .inner_margin(Margin::symmetric(8, 2))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(badge_text)
                                            .font(semibold(11.5))
                                            .color(p.accent),
                                    );
                                });
                        });
                        ui.label(
                            RichText::new("Native Windows screenshots & screen recording")
                                .size(13.0)
                                .color(p.muted),
                        );
                    });
                });

                ui.add_space(14.0);

                match &self.step {
                    SetupStep::Configure => {
                        card(ui, p, |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.label(
                                RichText::new("Install folder")
                                    .font(semibold(13.5))
                                    .color(p.text),
                            );
                            ui.horizontal(|ui| {
                                let browse_w = 88.0;
                                let text_w = (ui.available_width() - browse_w - 8.0).max(120.0);
                                ui.add_sized(
                                    [text_w, 32.0],
                                    egui::TextEdit::singleline(&mut self.dir_text),
                                );
                                if secondary_button(ui, p, "Browse…").clicked() {
                                    let current = PathBuf::from(self.dir_text.trim());
                                    let start_dir = current
                                        .exists()
                                        .then_some(current.as_path())
                                        .or_else(|| current.parent().filter(|p| p.exists()));
                                    if let Some(chosen) = shell::pick_folder(start_dir) {
                                        let final_dir = chosen
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .is_some_and(|n| n.eq_ignore_ascii_case("CapShotX"))
                                            .then_some(chosen.clone())
                                            .unwrap_or_else(|| chosen.join("CapShotX"));
                                        self.dir_text = final_dir.to_string_lossy().into_owned();
                                    }
                                }
                            });
                            ui.label(
                                RichText::new(
                                    "Per-user install — no administrator permissions required.",
                                )
                                .size(12.0)
                                .color(p.muted),
                            );
                        });

                        ui.add_space(10.0);

                        card(ui, p, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            ui.label(
                                RichText::new("Shortcuts & startup")
                                    .font(semibold(13.5))
                                    .color(p.text),
                            );
                            ui.checkbox(
                                &mut self.opts.create_start_menu,
                                RichText::new("Create a Start Menu shortcut")
                                    .size(13.5)
                                    .color(p.text),
                            );
                            ui.checkbox(
                                &mut self.opts.create_desktop,
                                RichText::new("Create a desktop icon")
                                    .size(13.5)
                                    .color(p.text),
                            );
                            ui.checkbox(
                                &mut self.opts.launch_at_startup,
                                RichText::new("Launch CapShotX when Windows starts")
                                    .size(13.5)
                                    .color(p.text),
                            );
                        });

                        if let Some(err) = &self.error {
                            ui.add_space(8.0);
                            ui.label(RichText::new(err).size(12.5).color(p.danger));
                        }
                    }
                    SetupStep::Installing { .. } => {
                        ui.add_space(36.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Spinner::new().size(28.0).color(p.accent));
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new("Installing CapShotX…")
                                    .font(semibold(15.0))
                                    .color(p.text),
                            );
                            ui.label(
                                RichText::new(self.opts.install_dir.display().to_string())
                                    .size(12.5)
                                    .color(p.muted),
                            );
                        });
                    }
                    SetupStep::Finished(done) => {
                        let installed_dir = done.install_dir.display().to_string();
                        card(ui, p, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            ui.label(
                                RichText::new("CapShotX is ready to use")
                                    .font(semibold(15.5))
                                    .color(p.success),
                            );
                            ui.label(
                                RichText::new(format!("Installed in {installed_dir}"))
                                    .size(13.0)
                                    .color(p.muted),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.launch_now,
                                RichText::new("Launch CapShotX now")
                                    .font(semibold(13.5))
                                    .color(p.text),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(
                                    "You can uninstall anytime from Windows Settings › Apps › Installed apps.",
                                )
                                .size(12.0)
                                .color(p.muted),
                            );
                        });
                    }
                }
            });
    }
}

struct UninstallApp<'a> {
    install_dir: PathBuf,
    remove_app_data: bool,
    done: bool,
    error: Option<String>,
    did_uninstall: &'a mut bool,
    logo: TextureHandle,
}

impl<'a> UninstallApp<'a> {
    fn new(
        cc: &eframe::CreationContext<'_>,
        install_dir: PathBuf,
        did_uninstall: &'a mut bool,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        install_theme(ctx);
        let logo = ctx.load_texture(
            "capshotx-logo",
            egui::ColorImage::from_rgba_unmultiplied([96, 96], &icon_rgba(96)),
            egui::TextureOptions::LINEAR,
        );
        Self {
            install_dir,
            remove_app_data: false,
            done: false,
            error: None,
            did_uninstall,
            logo,
        }
    }
}

impl eframe::App for UninstallApp<'_> {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let p = Palette::of(ui.visuals().dark_mode);

        egui::Panel::bottom("uninstall_footer")
            .exact_size(62.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.sidebar)
                    .stroke(Stroke::new(1.0, p.stroke))
                    .inner_margin(Margin::symmetric(24, 13)),
            )
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if self.done {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if primary_button(ui, p, "Close", p.accent).clicked() {
                                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                            }
                        });
                    } else {
                        if secondary_button(ui, p, "Cancel").clicked() {
                            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if primary_button(ui, p, "Uninstall", p.danger).clicked() {
                                match perform_uninstall(&self.install_dir, self.remove_app_data) {
                                    Ok(()) => {
                                        *self.did_uninstall = true;
                                        self.done = true;
                                        self.error = None;
                                    }
                                    Err(err) => self.error = Some(err),
                                }
                            }
                        });
                    }
                });
            });

        egui::CentralPanel::no_frame()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin::symmetric(24, 18)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::from_texture(&self.logo)
                            .fit_to_exact_size(vec2(42.0, 42.0))
                            .corner_radius(10),
                    );
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(
                            RichText::new("Uninstall CapShotX")
                                .font(semibold(19.0))
                                .color(p.text),
                        );
                        ui.label(
                            RichText::new(self.install_dir.display().to_string())
                                .size(12.0)
                                .color(p.muted),
                        );
                    });
                });

                ui.add_space(12.0);

                if self.done {
                    card(ui, p, |ui| {
                        ui.label(
                            RichText::new("CapShotX has been removed from your computer.")
                                .font(semibold(14.0))
                                .color(p.success),
                        );
                        ui.label(
                            RichText::new("Your saved captures in Pictures\\CapShotX were kept.")
                                .size(12.5)
                                .color(p.muted),
                        );
                    });
                } else {
                    card(ui, p, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.checkbox(
                            &mut self.remove_app_data,
                            RichText::new("Also remove settings, logs, and thumbnail cache")
                                .size(13.5)
                                .color(p.text),
                        );
                        ui.label(
                            RichText::new(
                                "Screenshots and recordings in Pictures\\CapShotX are never deleted.",
                            )
                            .size(12.0)
                            .color(p.muted),
                        );
                    });

                    if let Some(err) = &self.error {
                        ui.add_space(8.0);
                        ui.label(RichText::new(err).size(12.5).color(p.danger));
                    }
                }
            });
    }
}

// ---------------------------------------------------------------------------
// Theme, Widgets, and Procedural App Icon
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Palette {
    bg: Color32,
    sidebar: Color32,
    card: Color32,
    control: Color32,
    control_hover: Color32,
    control_strong: Color32,
    stroke: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    accent_soft: Color32,
    success: Color32,
    danger: Color32,
}

impl Palette {
    const DARK: Self = Self {
        bg: Color32::from_rgb(15, 17, 21),
        sidebar: Color32::from_rgb(20, 23, 28),
        card: Color32::from_rgb(25, 29, 36),
        control: Color32::from_rgb(33, 38, 47),
        control_hover: Color32::from_rgb(42, 48, 59),
        control_strong: Color32::from_rgb(58, 65, 78),
        stroke: Color32::from_rgb(37, 42, 51),
        text: Color32::from_rgb(230, 232, 235),
        muted: Color32::from_rgb(138, 147, 160),
        accent: Color32::from_rgb(110, 123, 255),
        accent_soft: Color32::from_rgb(35, 39, 72),
        success: Color32::from_rgb(52, 199, 123),
        danger: Color32::from_rgb(239, 68, 68),
    };

    const LIGHT: Self = Self {
        bg: Color32::from_rgb(246, 247, 249),
        sidebar: Color32::from_rgb(238, 240, 244),
        card: Color32::WHITE,
        control: Color32::from_rgb(236, 238, 242),
        control_hover: Color32::from_rgb(226, 229, 235),
        control_strong: Color32::from_rgb(200, 205, 214),
        stroke: Color32::from_rgb(227, 230, 235),
        text: Color32::from_rgb(21, 24, 29),
        muted: Color32::from_rgb(107, 114, 128),
        accent: Color32::from_rgb(88, 101, 242),
        accent_soft: Color32::from_rgb(228, 231, 255),
        success: Color32::from_rgb(22, 163, 74),
        danger: Color32::from_rgb(220, 38, 38),
    };

    fn of(dark_mode: bool) -> Self {
        if dark_mode { Self::DARK } else { Self::LIGHT }
    }
}

const SEMIBOLD: &str = "semibold";

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

fn card(ui: &mut Ui, p: Palette, add_contents: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.stroke))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
}

fn primary_button(ui: &mut Ui, _p: Palette, label: &str, fill: Color32) -> egui::Response {
    ui.add_sized(
        [116.0, 34.0],
        Button::new(RichText::new(label).font(semibold(13.5)).color(Color32::WHITE))
            .fill(fill)
            .corner_radius(8),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn secondary_button(ui: &mut Ui, p: Palette, label: &str) -> egui::Response {
    ui.add_sized(
        [88.0, 32.0],
        Button::new(RichText::new(label).size(13.0).color(p.text))
            .fill(p.control)
            .stroke(Stroke::new(1.0, p.stroke))
            .corner_radius(8),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn install_theme(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let mut load = |key: &str, file: &str| -> bool {
        match std::fs::read(dir.join(file)) {
            Ok(bytes) => {
                fonts
                    .font_data
                    .insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let has_reg = load("segoe-ui", "segoeui.ttf");
    let has_semi = load("segoe-ui-semibold", "seguisb.ttf");
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    if has_reg {
        prop.insert(0, "segoe-ui".to_owned());
    }
    let mut semi = prop.clone();
    if has_semi {
        semi.insert(0, "segoe-ui-semibold".to_owned());
    }
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semi);
    ctx.set_fonts(fonts);

    ctx.set_visuals_of(Theme::Dark, make_visuals(Palette::DARK, Visuals::dark()));
    ctx.set_visuals_of(Theme::Light, make_visuals(Palette::LIGHT, Visuals::light()));
    ctx.set_theme(ThemePreference::System);
    ctx.send_viewport_cmd(ViewportCommand::SetTheme(SystemTheme::SystemDefault));
}

fn make_visuals(p: Palette, mut v: Visuals) -> Visuals {
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = p.control;
    v.faint_bg_color = p.card;
    v.window_stroke = Stroke::new(1.0, p.stroke);
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    let w = &mut v.widgets;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.corner_radius = CornerRadius::same(8);
        state.expansion = 0.0;
        state.fg_stroke = Stroke::new(1.0, p.text);
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.muted);
    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.control_strong);
    w.active.bg_fill = p.control_strong;
    w.active.weak_bg_fill = p.control_strong;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    v
}

fn icon_rgba(size: u32) -> Vec<u8> {
    const PURPLE: [f32; 3] = [159.0, 87.0, 245.0];
    const BOLT: [(f32, f32); 6] = [
        (0.590, 0.240),
        (0.549, 0.479),
        (0.657, 0.479),
        (0.435, 0.752),
        (0.488, 0.567),
        (0.345, 0.567),
    ];

    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let half = 0.29;
    let radius = 0.075;
    let stroke = 0.05f32.max(1.6 / s);
    let gap = 0.09;

    for y in 0..size {
        for x in 0..size {
            let (u, v) = ((x as f32 + 0.5) / s - 0.5, (y as f32 + 0.5) / s - 0.5);
            let tile = (0.5 - rounded_square_sdf(u, v, 0.5, 0.22) * s).clamp(0.0, 1.0);
            let outline = rounded_square_sdf(u, v, half, radius).abs() - stroke / 2.0;
            let mut frame = outline.max(gap - u.abs().min(v.abs()));
            for (cx, cy) in [(gap, half), (half, gap)] {
                let d = ((u.abs() - cx).powi(2) + (v.abs() - cy).powi(2)).sqrt() - stroke / 2.0;
                frame = frame.min(d);
            }
            let bolt = polygon_sdf(u + 0.5, v + 0.5, &BOLT) - 0.004;
            let ink = (0.5 - frame.min(bolt) * s).clamp(0.0, 1.0);
            let mut rgb = [0u8; 3];
            for i in 0..3 {
                rgb[i] = (255.0 + (PURPLE[i] - 255.0) * ink).round() as u8;
            }
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (tile * 255.0).round() as u8]);
        }
    }
    out
}

fn rounded_square_sdf(x: f32, y: f32, half: f32, radius: f32) -> f32 {
    let qx = x.abs() - (half - radius);
    let qy = y.abs() - (half - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

fn polygon_sdf(x: f32, y: f32, pts: &[(f32, f32)]) -> f32 {
    let mut d = f32::MAX;
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (ax, ay) = pts[i];
        let (bx, by) = pts[j];
        let (ex, ey) = (bx - ax, by - ay);
        let (wx, wy) = (x - ax, y - ay);
        let t = ((wx * ex + wy * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        d = d.min((wx - ex * t).powi(2) + (wy - ey * t).powi(2));
        if (ay > y) != (by > y) && x < ax + (y - ay) * ex / ey {
            inside = !inside;
        }
        j = i;
    }
    if inside { -d.sqrt() } else { d.sqrt() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_untick_desktop_icon_and_enable_start_menu_and_startup() {
        let opts = InstallOptions::default();
        assert!(!opts.create_desktop, "Create desktop icon MUST be unticked by default");
        assert!(opts.create_start_menu);
        assert!(opts.launch_at_startup);
        assert!(!opts.install_dir.as_os_str().is_empty());
    }

    #[test]
    fn merging_startup_into_settings_preserves_existing_preferences_and_defaults_admin_off() {
        let existing = br#"{
            "tray_left_click": "OpenLatestCaptureFolder",
            "low_memory_mode": true
        }"#;
        let (merged, admin) = merge_startup_json(Some(existing), false);
        assert!(!admin);
        assert_eq!(merged["launch_at_startup"], serde_json::Value::Bool(false));
        assert_eq!(merged["run_as_admin"], serde_json::Value::Bool(false));
        assert_eq!(
            merged["tray_left_click"],
            serde_json::Value::String("OpenLatestCaptureFolder".to_owned())
        );
        assert_eq!(merged["low_memory_mode"], serde_json::Value::Bool(true));
    }

    #[test]
    fn merging_startup_respects_existing_opt_in_admin_setting() {
        let existing = br#"{ "run_as_admin": true }"#;
        let (merged, admin) = merge_startup_json(Some(existing), true);
        assert!(admin);
        assert_eq!(merged["run_as_admin"], serde_json::Value::Bool(true));
        assert_eq!(merged["launch_at_startup"], serde_json::Value::Bool(true));
    }
}
