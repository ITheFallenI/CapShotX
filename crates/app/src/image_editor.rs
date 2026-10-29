//! CapShotX Image Editor shell: a standalone pop-out window that loads an
//! image (file, clipboard or fresh capture), shows it on a zoomable canvas and
//! can copy or save it. Editing tools are intentionally absent; this is the
//! host for the editor UI that will replace it.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align, Color32, CursorIcon, Frame, Key, Layout, Margin, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Ui, Vec2, vec2,
};
use image::{ImageFormat, RgbaImage};
use platform_windows::{clipboard, shell, toast};

use crate::theme::{Palette, semibold};

pub struct ImageEditor {
    image: Arc<RgbaImage>,
    source_path: Option<PathBuf>,
    texture: Option<TextureHandle>,
    /// Viewport zoom (`1.0` = 100%) and pan offset in screen points.
    zoom: f32,
    pan: Vec2,
    fit_requested: bool,
    status: Option<(String, bool, Instant)>,
}

impl ImageEditor {
    /// An empty dark canvas shown when the editor opens without an image.
    pub fn new_placeholder() -> Self {
        let img = RgbaImage::from_pixel(960, 540, image::Rgba([30, 33, 44, 255]));
        Self::from_rgba_image(img, None)
    }

    /// Loads a top-down BGRA8 buffer (such as a fresh capture).
    pub fn from_bgra(width: u32, height: u32, bgra: &[u8]) -> Self {
        let mut rgba = Vec::with_capacity(bgra.len());
        for px in bgra.chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        let img = RgbaImage::from_raw(width.max(1), height.max(1), rgba)
            .unwrap_or_else(|| RgbaImage::new(1, 1));
        Self::from_rgba_image(img, None)
    }

    /// Loads an image from a file on disk (`PNG`, `JPG`, `BMP`, `WebP`).
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
        let decoded = image::load_from_memory(&bytes)
            .map_err(|e| format!("Couldn't decode {}: {e}", path.display()))?
            .to_rgba8();
        Ok(Self::from_rgba_image(decoded, Some(path.to_path_buf())))
    }

    /// Loads a temporary BGRA capture buffer and deletes the temporary file.
    pub fn from_temp_bgra_file(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("Couldn't read temp capture buffer {}: {e}", path.display()))?;
        let _ = std::fs::remove_file(path);
        if bytes.len() < 8 {
            return Err("Temp capture buffer is truncated".into());
        }
        let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let expected = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if width == 0 || height == 0 || bytes.len() < 8 + expected {
            return Err("Temp capture buffer dimensions are invalid".into());
        }
        Ok(Self::from_bgra(width, height, &bytes[8..8 + expected]))
    }

    pub fn from_rgba_image(img: RgbaImage, source_path: Option<PathBuf>) -> Self {
        Self {
            image: Arc::new(img),
            source_path,
            texture: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit_requested: true,
            status: None,
        }
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.image.width(), self.image.height())
    }

    fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some((text.into(), error, Instant::now()));
    }

    /// Replaces the image, keeping the status message.
    fn replace(&mut self, next: Self) {
        let status = self.status.take();
        *self = next;
        self.status = status;
    }

    pub fn paste_image_from_clipboard(&mut self) -> bool {
        let Some(clip) = clipboard::get_image() else {
            self.set_status("No image found on the clipboard", true);
            return false;
        };
        let next = match clip {
            clipboard::ClipboardImage::Png(bytes) => match image::load_from_memory(&bytes) {
                Ok(decoded) => Self::from_rgba_image(decoded.to_rgba8(), None),
                Err(e) => {
                    self.set_status(format!("Couldn't decode clipboard PNG: {e}"), true);
                    return false;
                }
            },
            clipboard::ClipboardImage::Bgra { width, height, bgra } => {
                Self::from_bgra(width, height, &bgra)
            }
        };
        let (w, h) = next.dimensions();
        self.replace(next);
        self.set_status(format!("Pasted {w} × {h} px image"), false);
        true
    }

    fn open_dialog(&mut self) {
        let initial = self.source_path.as_deref().and_then(Path::parent);
        let Some(path) = shell::pick_open_image(initial) else {
            return;
        };
        match Self::from_path(&path) {
            Ok(next) => self.replace(next),
            Err(e) => self.set_status(e, true),
        }
    }

    fn encode_png(&self) -> Result<Vec<u8>, String> {
        let mut png = Vec::new();
        self.image
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .map_err(|e| format!("Couldn't encode PNG: {e}"))?;
        Ok(png)
    }

    fn bgra(&self) -> Vec<u8> {
        let mut bgra = Vec::with_capacity(self.image.as_raw().len());
        for px in self.image.as_raw().chunks_exact(4) {
            bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        bgra
    }

    pub fn copy_to_clipboard(&mut self, sound: bool) {
        let png = match self.encode_png() {
            Ok(png) => png,
            Err(e) => return self.set_status(e, true),
        };
        let (w, h) = self.dimensions();
        match clipboard::set_image(w, h, &self.bgra(), &png) {
            Ok(()) => {
                if sound {
                    platform_windows::sound::play_shutter();
                }
                self.set_status(format!("Copied {w} × {h} px image"), false);
            }
            Err(e) => self.set_status(format!("Clipboard error: {e}"), true),
        }
    }

    pub fn save_as_dialog(&mut self, default_folder: &Path, file_template: &str, settings: &SaveSettings) {
        let default_name = capture::render_output_path(
            file_template,
            capture::OutputKind::Image,
            shell::local_time(),
        )
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "CapShotX_Edited".into());
        let folder = self
            .source_path
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(default_folder);
        let Some(path) = shell::pick_save_image(Some(folder), &format!("{default_name}.png"), false)
        else {
            return;
        };
        self.save_to(&path, settings);
    }

    fn save_to(&mut self, path: &Path, settings: &SaveSettings) {
        let is_jpg = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"));
        let encoded = if is_jpg {
            let rgb = image::DynamicImage::ImageRgba8((*self.image).clone()).to_rgb8();
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92)
                .encode_image(&rgb)
                .map(|()| out)
                .map_err(|e| format!("Encode failed: {e}"))
        } else {
            self.encode_png()
        };
        let encoded = match encoded {
            Ok(bytes) => bytes,
            Err(e) => return self.set_status(e, true),
        };
        if let Err(e) = std::fs::write(path, &encoded) {
            return self.set_status(format!("Couldn't write {}: {e}", path.display()), true);
        }
        if settings.sound {
            platform_windows::sound::play_shutter();
        }
        let fname = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        self.set_status(format!("Saved {fname}"), false);
        self.source_path = Some(path.to_path_buf());
        if settings.show_notification {
            let (width, height) = self.dimensions();
            let _ = toast::show_toast(toast::Toast {
                title: "Image saved".into(),
                subtitle: fname,
                open_on_click: Some(path.to_path_buf()),
                image: Some(toast::ToastImage { width, height, bgra: self.bgra() }),
                error: false,
                dark: settings.dark,
            });
        }
    }

    fn ui(&mut self, ui: &mut Ui, save_folder: &Path, file_template: &str, settings: &SaveSettings) {
        let p = Palette::of(ui.visuals().dark_mode);

        let (open, paste, copy, save) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, Key::O),
                i.consume_key(egui::Modifiers::COMMAND, Key::V),
                i.consume_key(egui::Modifiers::COMMAND, Key::C),
                i.consume_key(egui::Modifiers::COMMAND, Key::S),
            )
        });

        let mut actions = (open, paste, copy, save);
        egui::Panel::top("editor_toolbar")
            .frame(Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(12, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Editor").font(semibold(15.0)).color(p.text));
                    ui.add_space(12.0);
                    actions.0 |= ui.button("Open…").on_hover_text("Ctrl+O").clicked();
                    actions.1 |= ui.button("Paste").on_hover_text("Ctrl+V").clicked();
                    ui.separator();
                    actions.2 |= ui.button("Copy").on_hover_text("Ctrl+C").clicked();
                    actions.3 |= ui.button("Save as…").on_hover_text("Ctrl+S").clicked();
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Fit").clicked() {
                            self.fit_requested = true;
                        }
                        if ui.button("100%").clicked() {
                            self.zoom = 1.0;
                            self.pan = Vec2::ZERO;
                        }
                        ui.label(RichText::new(format!("{:.0}%", self.zoom * 100.0)).color(p.muted));
                    });
                });
            });

        egui::Panel::bottom("editor_status")
            .frame(Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(12, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (w, h) = self.dimensions();
                    ui.label(RichText::new(format!("{w} × {h} px")).size(12.0).color(p.muted));
                    if let Some(path) = &self.source_path {
                        ui.label(RichText::new(path.display().to_string()).size(12.0).color(p.muted));
                    }
                    if let Some((text, error, at)) = &self.status {
                        if at.elapsed() < Duration::from_secs(4) {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let color = if *error { p.danger } else { p.text };
                                ui.label(RichText::new(text).size(12.0).color(color));
                            });
                            ui.ctx().request_repaint_after(Duration::from_millis(500));
                        }
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.bg))
            .show(ui, |ui| self.canvas(ui, &p));

        let (open, paste, copy, save) = actions;
        if open {
            self.open_dialog();
        }
        if paste {
            self.paste_image_from_clipboard();
        }
        if copy {
            self.copy_to_clipboard(settings.sound);
        }
        if save {
            self.save_as_dialog(save_folder, file_template, settings);
        }
    }

    fn canvas(&mut self, ui: &mut Ui, p: &Palette) {
        let area = ui.max_rect();
        let response = ui.allocate_rect(area, Sense::click_and_drag());
        let (w, h) = self.dimensions();
        let size = vec2(w as f32, h as f32);

        if self.fit_requested && area.width() > 0.0 {
            let pad = 48.0;
            self.zoom = ((area.width() - pad) / size.x)
                .min((area.height() - pad) / size.y)
                .clamp(0.05, 1.0);
            self.pan = Vec2::ZERO;
            self.fit_requested = false;
        }

        // Ctrl/plain wheel zooms around the cursor; drag pans.
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let old = self.zoom;
                self.zoom = (self.zoom * (scroll * 0.0015).exp()).clamp(0.05, 16.0);
                if let Some(cursor) = response.hover_pos() {
                    let from_center = cursor - area.center() - self.pan;
                    self.pan -= from_center * (self.zoom / old - 1.0);
                }
            }
        }
        if response.dragged() {
            self.pan += response.drag_delta();
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        if response.double_clicked() {
            self.fit_requested = true;
        }

        let texture = self.texture.get_or_insert_with(|| {
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                self.image.as_raw(),
            );
            ui.ctx().load_texture("editor_image", color, TextureOptions::LINEAR)
        });

        let rect = Rect::from_center_size(area.center() + self.pan, size * self.zoom);
        let painter = ui.painter_at(area);
        painter.rect_filled(rect.expand(1.0), 0.0, Color32::from_black_alpha(60));
        painter.image(
            texture.id(),
            rect,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, p.stroke), StrokeKind::Outside);
    }
}

/// Output preferences shared with the main app's capture settings.
pub struct SaveSettings {
    pub sound: bool,
    pub show_notification: bool,
    pub dark: bool,
}

struct EditorWindowApp {
    settings: crate::settings::Settings,
    editor: ImageEditor,
    last_title: String,
}

impl eframe::App for EditorWindowApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (w, h) = self.editor.dimensions();
        let title = format!("CapShotX - Editor - {w} × {h}");
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let save = SaveSettings {
            sound: self.settings.capture.sound,
            show_notification: self.settings.capture.show_notification,
            dark: ui.visuals().dark_mode,
        };
        let folder = self.settings.capture.save_folder();
        let template = self.settings.capture.file_template.clone();
        self.editor.ui(ui, &folder, &template, &save);
    }
}

/// Runs the standalone CapShotX Image Editor window (`CapShotX_Editor.exe`).
pub fn run_editor_window(
    settings: crate::settings::Settings,
    editor: ImageEditor,
) -> anyhow::Result<()> {
    let (w, h) = editor.dimensions();
    let title = format!("CapShotX - Editor - {w} × {h}");
    let win_size = [1280.0f32, 800.0f32];

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(title.clone())
        .with_app_id("CapShotX.Editor")
        .with_resizable(true)
        .with_inner_size(win_size)
        .with_min_inner_size([640.0, 420.0])
        .with_icon(Arc::new(egui::IconData {
            rgba: crate::icon::rgba(64),
            width: 64,
            height: 64,
        }));

    // Open centred on the monitor under the cursor.
    let (cx, cy) = platform_windows::shell::cursor_pos();
    let monitors = platform_windows::monitors::enumerate();
    if let Some(m) = monitors
        .iter()
        .find(|m| m.bounds.contains(cx, cy))
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())
    {
        let scale = m.scale().max(0.5);
        let mw = m.bounds.width() as f32 / scale;
        let mh = m.bounds.height() as f32 / scale;
        let mx = m.bounds.left as f32 / scale;
        let my = m.bounds.top as f32 / scale;
        let px = mx + ((mw - win_size[0]) * 0.5).max(20.0);
        let py = my + ((mh - win_size[1]) * 0.5).max(20.0);
        viewport = viewport.with_position([px, py]);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "CapShotX - Editor",
        options,
        Box::new(move |cc| {
            crate::theme::install(&cc.egui_ctx);
            let (preference, system) = match settings.theme {
                crate::settings::ThemeChoice::System => {
                    (egui::ThemePreference::System, egui::SystemTheme::SystemDefault)
                }
                crate::settings::ThemeChoice::Light => {
                    (egui::ThemePreference::Light, egui::SystemTheme::Light)
                }
                crate::settings::ThemeChoice::Dark => {
                    (egui::ThemePreference::Dark, egui::SystemTheme::Dark)
                }
            };
            cc.egui_ctx.set_theme(preference);
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(system));
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            Ok(Box::new(EditorWindowApp { settings, editor, last_title: title }))
        }),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_buffers_load_as_rgba() {
        let editor = ImageEditor::from_bgra(2, 1, &[1, 2, 3, 0, 4, 5, 6, 0]);
        assert_eq!(editor.dimensions(), (2, 1));
        assert_eq!(editor.image.as_raw(), &[3, 2, 1, 255, 6, 5, 4, 255]);
        assert_eq!(editor.bgra(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }
}
