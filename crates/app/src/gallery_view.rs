//! Gallery UI: recent strip on Home, the full library page and the preview.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, FontId, Id, Key, Margin, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, Ui,
    pos2, vec2,
};
use gallery::{Entry, ThumbKey, Thumbnailer};
use platform_windows::shell;

use crate::screen_record::format_duration;
use crate::theme::semibold;
use crate::widgets::{self, Icon, palette};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaFilter {
    #[default]
    All,
    Images,
    Videos,
}

impl MediaFilter {
    fn matches(self, path: &Path) -> bool {
        match self {
            Self::All => true,
            Self::Images => !gallery::is_video(path),
            Self::Videos => gallery::is_video(path),
        }
    }
}

/// Lightweight metadata kept in memory while the UI window is closed to the
/// tray (~a few KB, zero pixel or GPU data) so reopening Home/Gallery is instant.
pub struct GalleryCache {
    pub entries: Vec<Entry>,
    pub durations: HashMap<PathBuf, Duration>,
    pub filter: MediaFilter,
    pub newest_first: bool,
    pub dirty: bool,
}

impl Default for GalleryCache {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            durations: HashMap::new(),
            filter: MediaFilter::All,
            newest_first: true,
            dirty: true,
        }
    }
}

const RESCAN_EVERY: Duration = Duration::from_secs(10);
/// Thumbnail textures kept in GPU memory (~230 KB each at 320×180).
const MAX_TEXTURES: usize = 48;
const MIN_TILE: f32 = 210.0;
const GAP: f32 = 14.0;
const TILE_FOOTER: f32 = 46.0;
const HEADER_H: f32 = 40.0;
const NOTICE_FOR: Duration = Duration::from_secs(3);
const PREVIEW_MAX_SIDE: u32 = 3840;

enum Slot {
    Pending,
    Ready { texture: TextureHandle, size: [u32; 2], used: u64 },
    Failed,
}

struct Preview {
    path: PathBuf,
    texture: Option<TextureHandle>,
    error: Option<String>,
    loading: Option<Receiver<(Result<(u32, u32, Vec<u8>), String>, Option<Duration>)>>,
}

struct Rename {
    path: PathBuf,
    text: String,
    error: Option<String>,
    focus: bool,
}

struct Notice {
    text: String,
    error: bool,
    at: Instant,
}

enum Job {
    Notice(String, bool),
}

/// What a tile click asks the page to do.
enum TileAction {
    Preview(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
}

pub struct GalleryView {
    ctx: egui::Context,
    folder: PathBuf,
    entries: Vec<Entry>,
    scan: Option<Receiver<Vec<Entry>>>,
    scanned_at: Option<Instant>,
    rescan_pending: bool,
    thumbs: HashMap<ThumbKey, Slot>,
    durations: HashMap<PathBuf, Duration>,
    thumbnailer: Thumbnailer,
    frame: u64,
    wanted: HashSet<ThumbKey>,
    search: String,
    filter: MediaFilter,
    newest_first: bool,
    preview: Option<Preview>,
    rename: Option<Rename>,
    notice: Option<Notice>,
    jobs: (Sender<Job>, Receiver<Job>),
}

impl GalleryView {
    pub fn new(
        ctx: &egui::Context,
        folder: PathBuf,
        cache_dir: Option<PathBuf>,
        cache: &mut GalleryCache,
    ) -> Self {
        let repaint = ctx.clone();
        let has_cached = !cache.entries.is_empty() && !cache.dirty;
        let mut view = Self {
            ctx: ctx.clone(),
            folder,
            entries: std::mem::take(&mut cache.entries),
            scan: None,
            scanned_at: has_cached.then(Instant::now),
            rescan_pending: false,
            thumbs: HashMap::new(),
            durations: std::mem::take(&mut cache.durations),
            thumbnailer: Thumbnailer::new(cache_dir, move || repaint.request_repaint()),
            frame: 0,
            wanted: HashSet::new(),
            search: String::new(),
            filter: cache.filter,
            newest_first: cache.newest_first,
            preview: None,
            rename: None,
            notice: None,
            jobs: mpsc::channel(),
        };
        if !has_cached {
            view.request_rescan();
        }
        view
    }

    /// Saves lightweight index metadata back to `cache` and drops GPU textures
    /// before closing the window to the tray.
    pub fn save_cache(&mut self, cache: &mut GalleryCache) {
        self.preview = None;
        self.thumbs.clear();
        cache.entries = std::mem::take(&mut self.entries);
        cache.durations = std::mem::take(&mut self.durations);
        cache.filter = self.filter;
        cache.newest_first = self.newest_first;
        cache.dirty = self.scan.is_some() || self.rescan_pending;
    }

    /// Frees any full-resolution preview texture when leaving the Gallery page.
    pub fn release_preview(&mut self) {
        self.preview = None;
    }

    pub fn set_folder(&mut self, folder: PathBuf) {
        if folder != self.folder {
            self.folder = folder;
            self.entries.clear();
            self.durations.clear();
            self.thumbs.clear();
            self.preview = None;
            self.request_rescan();
        }
    }

    /// Re-reads the folder in the background (never blocks the UI).
    pub fn request_rescan(&mut self) {
        if self.scan.is_some() {
            self.rescan_pending = true;
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (folder, ctx) = (self.folder.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let entries = gallery::scan(&folder);
            tracing::debug!("scanned {} images in {} ms", entries.len(), started.elapsed().as_millis());
            let _ = tx.send(entries);
            ctx.request_repaint();
        });
        self.scan = Some(rx);
    }

    /// Per-frame housekeeping while the window is visible.
    pub fn tick(&mut self) {
        self.frame += 1;
        if let Some(rx) = &self.scan
            && let Ok(entries) = rx.try_recv()
        {
            self.entries = entries;
            self.scan = None;
            self.scanned_at = Some(Instant::now());
            if std::mem::take(&mut self.rescan_pending) {
                self.request_rescan();
            }
        }
        if self.scan.is_none() && self.scanned_at.is_some_and(|t| t.elapsed() >= RESCAN_EVERY) {
            self.request_rescan();
        }

        for thumb in self.thumbnailer.poll() {
            if let Some(dur) = thumb.duration {
                self.durations.insert(thumb.key.path.clone(), dur);
            }
            let slot = match thumb.image {
                Ok((w, h, rgba)) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    let texture = self.ctx.load_texture(
                        format!("thumb:{}", thumb.key.path.display()),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    Slot::Ready { texture, size: [w, h], used: self.frame }
                }
                Err(err) => {
                    tracing::debug!("thumbnail failed for {}: {err}", thumb.key.path.display());
                    Slot::Failed
                }
            };
            self.thumbs.insert(thumb.key, slot);
        }
        self.evict();

        if let Some(preview) = &mut self.preview
            && let Some(rx) = &preview.loading
            && let Ok((result, dur)) = rx.try_recv()
        {
            preview.loading = None;
            if let Some(d) = dur {
                self.durations.insert(preview.path.clone(), d);
            }
            match result {
                Ok((w, h, rgba)) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    preview.texture = Some(self.ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
                }
                Err(err) => preview.error = Some(err),
            }
        }

        while let Ok(job) = self.jobs.1.try_recv() {
            match job {
                Job::Notice(text, error) => self.notify(text, error),
            }
        }
        if self.notice.as_ref().is_some_and(|n| n.at.elapsed() >= NOTICE_FOR) {
            self.notice = None;
        }
    }

    /// Keeps only the most recently drawn thumbnails in GPU memory.
    fn evict(&mut self) {
        let ready = self.thumbs.values().filter(|s| matches!(s, Slot::Ready { .. })).count();
        if ready <= MAX_TEXTURES {
            return;
        }
        let mut by_age: Vec<(u64, ThumbKey)> = self
            .thumbs
            .iter()
            .filter_map(|(k, s)| match s {
                Slot::Ready { used, .. } if *used < self.frame => Some((*used, k.clone())),
                _ => None,
            })
            .collect();
        by_age.sort_by_key(|(used, _)| *used);
        for (_, key) in by_age.into_iter().take(ready - MAX_TEXTURES) {
            self.thumbs.remove(&key);
        }
    }

    fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(Notice { text: text.into(), error, at: Instant::now() });
        self.ctx.request_repaint_after(NOTICE_FOR);
    }

    // ----- Gallery page ----------------------------------------------------

    pub fn page_ui(&mut self, ui: &mut Ui) {
        if self.preview.is_some() {
            self.preview_ui(ui);
            self.rename_dialog(ui.ctx());
            return;
        }
        let p = palette(ui);

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Gallery").font(semibold(26.0)).color(p.text));
                let count = match self.entries.len() {
                    0 if self.scan.is_some() => "Loading…".to_owned(),
                    1 => "1 capture".to_owned(),
                    n => format!("{n} captures"),
                };
                ui.label(RichText::new(count).size(14.0).color(p.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Open folder").clicked() {
                    let _ = std::fs::create_dir_all(&self.folder);
                    shell::open(&self.folder);
                }
                let sort = if self.newest_first { "Newest first" } else { "Oldest first" };
                if ui.button(sort).on_hover_text("Change sort order").clicked() {
                    self.newest_first = !self.newest_first;
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search file names")
                        .desired_width(185.0),
                );
                widgets::segmented(
                    ui,
                    &mut self.filter,
                    &[
                        (MediaFilter::All, "All"),
                        (MediaFilter::Images, "Images"),
                        (MediaFilter::Videos, "Videos"),
                    ],
                );
            });
        });
        ui.add_space(14.0);

        let needle = self.search.trim().to_lowercase();
        let mut order: Vec<usize> = (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                self.filter.matches(&e.path) && (needle.is_empty() || e.name.to_lowercase().contains(&needle))
            })
            .collect();
        if !self.newest_first {
            order.reverse();
        }

        if order.is_empty() {
            if self.entries.is_empty() {
                self.empty_state(ui, "No captures yet", "Press Ctrl+Shift+2 for a screenshot or Ctrl+Shift+5 to record.");
            } else {
                self.empty_state(ui, "No matches", "Try changing the media filter or file-name search.");
            }
            self.notice_ui(ui);
            return;
        }

        // Rows: a header per day, then tile rows. Laid out up front so only
        // the visible part is drawn (stays fast with thousands of files).
        enum Row {
            Header(String, usize),
            Tiles(Vec<usize>),
        }
        let (cols, tile_w, tile_h) = grid_metrics(ui.available_width() - 14.0);
        let mut rows: Vec<(f32, Row)> = Vec::new();
        let mut y = 0.0;
        let mut start = 0;
        while start < order.len() {
            let day = self.entries[order[start]].day();
            let end = order[start..]
                .iter()
                .position(|&i| self.entries[i].day() != day)
                .map_or(order.len(), |n| start + n);
            let label = gallery::day_label(&self.entries[order[start]].local);
            rows.push((y, Row::Header(label, end - start)));
            y += HEADER_H;
            for chunk in order[start..end].chunks(cols) {
                rows.push((y, Row::Tiles(chunk.to_vec())));
                y += tile_h + GAP;
            }
            y += 8.0;
            start = end;
        }
        let total = y;

        let mut action = None;
        egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, viewport| {
            ui.set_height(total);
            let origin = ui.min_rect().min;
            let visible = viewport.expand2(vec2(0.0, tile_h));
            for (y, row) in &rows {
                let h = match row {
                    Row::Header(..) => HEADER_H,
                    Row::Tiles(_) => tile_h,
                };
                if *y + h < visible.top() || *y > visible.bottom() {
                    continue;
                }
                match row {
                    Row::Header(label, count) => {
                        let at = origin + vec2(0.0, *y + HEADER_H / 2.0 + 4.0);
                        let painter = ui.painter();
                        let r = painter.text(at, Align2::LEFT_CENTER, label, semibold(15.0), p.text);
                        painter.text(
                            pos2(r.right() + 10.0, at.y),
                            Align2::LEFT_CENTER,
                            count.to_string(),
                            FontId::proportional(13.0),
                            p.muted,
                        );
                    }
                    Row::Tiles(items) => {
                        for (col, &i) in items.iter().enumerate() {
                            let rect = Rect::from_min_size(
                                origin + vec2(col as f32 * (tile_w + GAP), *y),
                                vec2(tile_w, tile_h),
                            );
                            if let Some(a) = self.tile(ui, rect, i) {
                                action = Some(a);
                            }
                        }
                    }
                }
            }
        });
        self.finish_frame();
        if let Some(action) = action {
            self.apply(action);
        }
        self.notice_ui(ui);
        self.rename_dialog(ui.ctx());
    }

    /// Drops queued thumbnail work for tiles that scrolled out of view.
    fn finish_frame(&mut self) {
        let wanted = std::mem::take(&mut self.wanted);
        self.thumbnailer.retain(|k| wanted.contains(k));
        self.thumbs.retain(|k, s| !matches!(s, Slot::Pending) || wanted.contains(k));
    }

    fn apply(&mut self, action: TileAction) {
        match action {
            TileAction::Preview(path) => self.open_preview(path),
            TileAction::Rename(path) => {
                let text = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                self.rename = Some(Rename { path, text, error: None, focus: true });
            }
            TileAction::Delete(path) => self.delete(&path),
        }
    }

    fn delete(&mut self, path: &Path) {
        match shell::recycle(path) {
            Ok(()) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                // Step the preview to a neighbour instead of closing it.
                if self.preview.as_ref().is_some_and(|p| p.path == path) {
                    let next = self.neighbour(path, 1).or_else(|| self.neighbour(path, -1));
                    self.preview = None;
                    if let Some(next) = next {
                        self.open_preview(next);
                    }
                }
                self.entries.retain(|e| e.path != path);
                self.notify(format!("Moved {name} to the Recycle Bin"), false);
                self.request_rescan();
            }
            Err(err) => self.notify(err, true),
        }
    }

    fn copy_image(&self, path: PathBuf) {
        let tx = self.jobs.0.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let job = match gallery::copy_image(&path) {
                Ok(()) => Job::Notice("Image copied to the clipboard".into(), false),
                Err(err) => Job::Notice(err, true),
            };
            let _ = tx.send(job);
            ctx.request_repaint();
        });
    }

    /// Draws one tile; returns an action if it was clicked or a menu item chosen.
    fn tile(&mut self, ui: &mut Ui, rect: Rect, index: usize) -> Option<TileAction> {
        let p = palette(ui);
        let entry = &self.entries[index];
        let key = entry.key();
        let path = entry.path.clone();
        let response = ui
            .interact(rect, Id::unique(("tile", &entry.path)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let hovered = response.hovered();
        let painter = ui.painter_at(rect.expand(2.0));

        painter.rect_filled(rect, 12, p.card);
        let image_rect = Rect::from_min_size(rect.min, vec2(rect.width(), rect.height() - TILE_FOOTER));
        let frame = self.frame;
        match self.thumbs.get_mut(&key) {
            Some(Slot::Ready { texture, size, used }) => {
                *used = frame;
                let uv = cover_uv(*size, image_rect.size());
                egui::Image::new((texture.id(), image_rect.size()))
                    .uv(uv)
                    .corner_radius(egui::CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 })
                    .paint_at(ui, image_rect);
            }
            Some(Slot::Failed) => {
                painter.rect_filled(image_rect, egui::CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 }, p.control);
                painter.text(image_rect.center(), Align2::CENTER_CENTER, "Can't preview", FontId::proportional(12.5), p.muted);
            }
            Some(Slot::Pending) => {
                painter.rect_filled(image_rect, egui::CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 }, p.control);
            }
            None => {
                painter.rect_filled(image_rect, egui::CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 }, p.control);
                let need_duration = gallery::is_video(&path) && !self.durations.contains_key(&path);
                self.thumbs.insert(key.clone(), Slot::Pending);
                self.thumbnailer.request(key.clone(), need_duration);
            }
        }
        self.wanted.insert(key);

        if gallery::is_video(&path) {
            // Play badge.
            let c = image_rect.center();
            painter.circle_filled(c, 18.0, Color32::from_black_alpha(140));
            let tri = vec![pos2(c.x - 5.0, c.y - 8.0), pos2(c.x - 5.0, c.y + 8.0), pos2(c.x + 9.0, c.y)];
            painter.add(egui::Shape::convex_polygon(tri, Color32::WHITE, Stroke::NONE));

            if let Some(&dur) = self.durations.get(&path) {
                let text = format_duration(dur);
                let font = semibold(11.5);
                let galley = painter.layout_no_wrap(text, font, Color32::WHITE);
                let pill_size = galley.size() + vec2(12.0, 6.0);
                let pill_rect = Rect::from_min_size(
                    image_rect.max - pill_size - vec2(8.0, 8.0),
                    pill_size,
                );
                painter.rect_filled(pill_rect, 6, Color32::from_black_alpha(185));
                painter.galley(pill_rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
            }
        }

        let entry = &self.entries[index];
        let text_left = rect.left() + 12.0;
        let name = truncate(&entry.name, rect.width() - 24.0, ui);
        painter.text(pos2(text_left, image_rect.bottom() + 15.0), Align2::LEFT_CENTER, name, FontId::proportional(13.0), p.text);
        let meta = format!("{:02}:{:02} · {}", entry.local.hour, entry.local.minute, gallery::human_size(entry.size));
        painter.text(pos2(text_left, image_rect.bottom() + 33.0), Align2::LEFT_CENTER, meta, FontId::proportional(11.5), p.muted);
        painter.rect_stroke(
            rect,
            12,
            Stroke::new(1.0, if hovered { p.accent } else { p.stroke }),
            StrokeKind::Inside,
        );

        let mut action = None;
        if response.clicked() {
            action = Some(TileAction::Preview(path.clone()));
        }
        response.context_menu(|ui| {
            if let Some(a) = self.item_menu(ui, &path) {
                action = Some(a);
            }
        });
        action
    }

    /// Shared actions for the tile context menu and the preview toolbar.
    fn item_menu(&mut self, ui: &mut Ui, path: &Path) -> Option<TileAction> {
        let mut action = None;
        let video = gallery::is_video(path);
        if ui.button(if video { "Play" } else { "Open" }).clicked() {
            shell::open(path);
            ui.close();
        }
        if !video && ui.button("Copy image").clicked() {
            self.copy_image(path.to_path_buf());
            ui.close();
        }
        if ui.button("Copy path").clicked() {
            ui.ctx().copy_text(path.display().to_string());
            self.notify("Path copied", false);
            ui.close();
        }
        if ui.button("Rename…").clicked() {
            action = Some(TileAction::Rename(path.to_path_buf()));
            ui.close();
        }
        if ui.button("Show in Explorer").clicked() {
            shell::reveal(path);
            ui.close();
        }
        ui.separator();
        if ui.button(RichText::new("Move to Recycle Bin").color(palette(ui).danger)).clicked() {
            action = Some(TileAction::Delete(path.to_path_buf()));
            ui.close();
        }
        action
    }

    // ----- Preview ---------------------------------------------------------

    fn open_preview(&mut self, path: PathBuf) {
        let (tx, rx) = mpsc::channel();
        let known_dur = self.durations.get(&path).copied();
        let (p, ctx) = (path.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let dur = known_dur.or_else(|| gallery::video_duration(&p));
            let _ = tx.send((gallery::load_full(&p, PREVIEW_MAX_SIDE), dur));
            ctx.request_repaint();
        });
        self.preview = Some(Preview { path, texture: None, error: None, loading: Some(rx) });
    }

    /// The entry `step` places away from `path` in the current order.
    fn neighbour(&self, path: &Path, step: isize) -> Option<PathBuf> {
        let step = if self.newest_first { step } else { -step };
        let needle = self.search.trim().to_lowercase();
        let order: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| self.filter.matches(&e.path) && (needle.is_empty() || e.name.to_lowercase().contains(&needle)))
            .collect();
        let pos = order.iter().position(|e| e.path == path)? as isize;
        order.get((pos + step) as usize).filter(|_| pos + step >= 0).map(|e| e.path.clone())
    }

    fn preview_ui(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let Some(preview) = &self.preview else { return };
        let path = preview.path.clone();
        let video = gallery::is_video(&path);
        let entry = self.entries.iter().find(|e| e.path == path).cloned();
        let texture_size = preview.texture.as_ref().map(|t| t.size());
        let duration = self.durations.get(&path).copied();
        let mut close = false;
        let mut action = None;

        ui.horizontal(|ui| {
            if ui.button("← Back").clicked() {
                close = true;
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                ui.label(RichText::new(name).font(semibold(15.0)).color(p.text));
                if let Some(e) = &entry {
                    let size = texture_size.map_or(String::new(), |[w, h]| format!("{w} × {h} · "));
                    let dur_str = duration.map_or(String::new(), |d| format!("{} · ", format_duration(d)));
                    ui.label(
                        RichText::new(format!("{size}{dur_str}{} · {}", gallery::day_label(&e.local), gallery::human_size(e.size)))
                            .size(12.5)
                            .color(p.muted),
                    );
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("More…", |ui| {
                    if let Some(a) = self.item_menu(ui, &path) {
                        action = Some(a);
                    }
                });
                if ui.button("Show in Explorer").clicked() {
                    shell::reveal(&path);
                }
                if !video && ui.button("Copy image").clicked() {
                    self.copy_image(path.clone());
                }
                if ui.button(if video { "▶ Play" } else { "Open" }).clicked() {
                    shell::open(&path);
                }
            });
        });
        ui.add_space(12.0);

        let area = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(area, Sense::click());
        ui.painter().rect_filled(area, 12, p.control);
        let preview = self.preview.as_ref().expect("checked above");
        let mut play_clicked = false;
        if let Some(texture) = &preview.texture {
            let [w, h] = texture.size();
            let fit = (area.width() / w as f32).min(area.height() / h as f32).min(1.0);
            let size = vec2(w as f32 * fit, h as f32 * fit);
            let rect = Rect::from_center_size(area.center(), size);
            egui::Image::new((texture.id(), size)).paint_at(ui, rect);
            if video {
                let c = rect.center();
                let play_rect = Rect::from_center_size(c, vec2(72.0, 72.0));
                let play_resp = ui
                    .interact(play_rect, Id::unique("preview-video-play"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                let alpha = if play_resp.hovered() { 185 } else { 145 };
                ui.painter().circle_filled(c, 28.0, Color32::from_black_alpha(alpha));
                let tri = vec![pos2(c.x - 7.0, c.y - 11.0), pos2(c.x - 7.0, c.y + 11.0), pos2(c.x + 12.0, c.y)];
                ui.painter().add(egui::Shape::convex_polygon(tri, Color32::WHITE, Stroke::NONE));
                if play_resp.clicked() {
                    play_clicked = true;
                }
            }
        } else if let Some(err) = &preview.error {
            let prefix = if video { "Can't preview this video" } else { "Can't open this image" };
            ui.painter().text(area.center(), Align2::CENTER_CENTER, format!("{prefix}: {err}"), FontId::proportional(14.0), p.danger);
        } else {
            ui.painter().text(area.center(), Align2::CENTER_CENTER, "Loading…", FontId::proportional(14.0), p.muted);
        }

        // Side arrows on hover, plus ← / → keys.
        let prev = self.neighbour(&path, -1);
        let next = self.neighbour(&path, 1);
        let mut go = None;
        if response.hovered() || ui.rect_contains_pointer(area) {
            for (target, left) in [(&prev, true), (&next, false)] {
                if target.is_none() {
                    continue;
                }
                let center = if left { pos2(area.left() + 34.0, area.center().y) } else { pos2(area.right() - 34.0, area.center().y) };
                let r = Rect::from_center_size(center, vec2(40.0, 40.0));
                let resp = ui.interact(r, Id::unique(("preview-nav", left)), Sense::click());
                let fill = if resp.hovered() { Color32::from_black_alpha(170) } else { Color32::from_black_alpha(110) };
                ui.painter().circle_filled(center, 20.0, fill);
                ui.painter().text(center, Align2::CENTER_CENTER, if left { "‹" } else { "›" }, FontId::proportional(26.0), Color32::WHITE);
                if resp.clicked() {
                    go = target.clone();
                }
            }
        }
        if self.rename.is_none() {
            ui.input(|i| {
                if i.key_pressed(Key::ArrowLeft) {
                    go = go.clone().or(prev.clone());
                }
                if i.key_pressed(Key::ArrowRight) {
                    go = go.clone().or(next.clone());
                }
                if video && (i.key_pressed(Key::Enter) || i.key_pressed(Key::Space)) {
                    play_clicked = true;
                }
                if i.key_pressed(Key::Escape) {
                    close = true;
                }
                if i.key_pressed(Key::Delete) {
                    action = Some(TileAction::Delete(path.clone()));
                }
            });
        }
        if play_clicked {
            shell::open(&path);
        }

        if close {
            self.preview = None;
        } else if let Some(target) = go {
            self.open_preview(target);
        }
        if let Some(a) = action {
            self.apply(a);
        }
        self.notice_ui(ui);
    }

    // ----- Shared bits -----------------------------------------------------

    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some(rename) = &mut self.rename else { return };
        let mut done = None;
        let modal = egui::Modal::new(Id::unique("rename")).show(ctx, |ui| {
            let p = palette(ui);
            ui.set_width(380.0);
            ui.label(RichText::new("Rename").font(semibold(17.0)).color(p.text));
            ui.add_space(8.0);
            let edit = ui.add(egui::TextEdit::singleline(&mut rename.text).desired_width(f32::INFINITY));
            if std::mem::take(&mut rename.focus) {
                edit.request_focus();
            }
            if let Some(err) = &rename.error {
                ui.label(RichText::new(err).size(12.5).color(p.danger));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let submit = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if ui.button("Rename").clicked() || submit {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                });
            });
        });
        if modal.should_close() && done.is_none() {
            done = Some(false);
        }
        match done {
            Some(true) => match gallery::rename(&rename.path, &rename.text) {
                Ok(new_path) => {
                    let old = rename.path.clone();
                    self.rename = None;
                    if let Some(preview) = &mut self.preview
                        && preview.path == old
                    {
                        preview.path = new_path.clone();
                    }
                    if let Some(dur) = self.durations.remove(&old) {
                        self.durations.insert(new_path.clone(), dur);
                    }
                    if let Some(e) = self.entries.iter_mut().find(|e| e.path == old) {
                        e.name = new_path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        e.path = new_path;
                    }
                    self.request_rescan();
                }
                Err(err) => rename.error = Some(err),
            },
            Some(false) => self.rename = None,
            None => {}
        }
    }

    fn empty_state(&self, ui: &mut Ui, title: &str, subtitle: &str) {
        let p = palette(ui);
        widgets::card(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(26.0);
                widgets::icon_tile(ui, Icon::Image, 52.0);
                ui.add_space(8.0);
                ui.label(RichText::new(title).font(semibold(16.0)).color(p.text));
                ui.label(RichText::new(subtitle).size(13.0).color(p.muted));
                ui.add_space(26.0);
            });
        });
    }

    fn notice_ui(&self, ui: &mut Ui) {
        let Some(notice) = &self.notice else { return };
        let p = palette(ui);
        egui::Area::new(Id::unique("gallery-notice"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -20.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(28, 31, 38))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(14, 9))
                    .show(ui, |ui| {
                        let color = if notice.error { p.danger } else { Color32::WHITE };
                        ui.label(RichText::new(&notice.text).size(13.0).color(color));
                    });
            });
    }
}

/// Column count and tile size for a given width.
fn grid_metrics(width: f32) -> (usize, f32, f32) {
    let cols = (((width + GAP) / (MIN_TILE + GAP)).floor() as usize).max(1);
    let tile_w = (width - GAP * (cols - 1) as f32) / cols as f32;
    (cols, tile_w, tile_w * 9.0 / 16.0 + TILE_FOOTER)
}

/// UV rect that centre-crops an image of `size` to fill `target`.
fn cover_uv(size: [u32; 2], target: egui::Vec2) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let image_aspect = w / h;
    let target_aspect = target.x / target.y.max(1.0);
    if image_aspect > target_aspect {
        let visible = target_aspect / image_aspect;
        let x0 = (1.0 - visible) / 2.0;
        Rect::from_min_max(pos2(x0, 0.0), pos2(x0 + visible, 1.0))
    } else {
        let visible = image_aspect / target_aspect;
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, visible))
    }
}

/// Shortens `text` with an ellipsis to fit `width` at 13 pt.
fn truncate(text: &str, width: f32, ui: &Ui) -> String {
    let font = FontId::proportional(13.0);
    let fits = |s: &str| ui.fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE).size().x) <= width;
    if fits(text) {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if fits(&candidate) { lo = mid } else { hi = mid - 1 }
    }
    chars[..lo].iter().collect::<String>() + "…"
}
