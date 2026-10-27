use std::path::PathBuf;
use std::sync::Arc;

use egui::{
    Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Theme,
    Visuals, vec2,
};

/// Colours used by custom-painted widgets, per light/dark mode.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub card: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_strong: Color32,
    pub stroke: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub success: Color32,
    pub danger: Color32,
}

impl Palette {
    pub const DARK: Self = Self {
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
        danger: Color32::from_rgb(248, 113, 113),
    };

    pub const LIGHT: Self = Self {
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

    pub fn of(dark_mode: bool) -> Self {
        if dark_mode { Self::DARK } else { Self::LIGHT }
    }
}

const SEMIBOLD: &str = "semibold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn install(ctx: &Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(Theme::Dark, visuals(Palette::DARK, Visuals::dark()));
    ctx.set_visuals_of(Theme::Light, visuals(Palette::LIGHT, Visuals::light()));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 8.0);
        style.spacing.button_padding = vec2(14.0, 7.0);
        style.spacing.interact_size.y = 32.0;
        for (text_style, font) in style.text_styles.iter_mut() {
            font.size = match text_style {
                egui::TextStyle::Heading => 24.0,
                egui::TextStyle::Small => 12.0,
                _ => 14.0,
            };
        }
    });
}

fn visuals(p: Palette, mut v: Visuals) -> Visuals {
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = p.control;
    v.faint_bg_color = p.card;
    v.window_stroke = Stroke::new(1.0, p.stroke);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.hyperlink_color = p.accent;
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
    w.inactive.bg_stroke = Stroke::NONE;
    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.control_strong);
    w.active.bg_fill = p.control_strong;
    w.active.weak_bg_fill = p.control_strong;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    v
}

/// Uses Segoe UI when available, keeping egui's bundled fonts as fallbacks
/// (they provide the symbol glyphs).
fn install_fonts(ctx: &Context) {
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
    let has_regular = load("segoe-ui", "segoeui.ttf");
    let has_semibold = load("segoe-ui-semibold", "seguisb.ttf");

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    if has_regular {
        proportional.insert(0, "segoe-ui".to_owned());
    }
    let mut semibold = proportional.clone();
    if has_semibold {
        semibold.insert(0, "segoe-ui-semibold".to_owned());
    }
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semibold);

    ctx.set_fonts(fonts);
}
