//! Small custom-painted widgets for the CapShotX look.

use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, Painter, Pos2, Rect, Response,
    RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::theme::{Palette, semibold};

pub fn palette(ui: &Ui) -> Palette {
    Palette::of(ui.visuals().dark_mode)
}

/// A bordered surface filling the available width.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.stroke))
        .corner_radius(14)
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

/// A card that highlights on hover and reports clicks anywhere on it.
pub fn action_card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) -> Response {
    let p = palette(ui);
    ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        let hovered = ui.response().hovered();
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0, if hovered { p.accent } else { p.stroke }))
            .corner_radius(14)
            .inner_margin(Margin::same(18))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add_contents(ui);
            });
    })
    .response
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn section_label(ui: &mut Ui, text: &str) {
    let p = palette(ui);
    ui.add_space(6.0);
    ui.label(RichText::new(text.to_uppercase()).font(semibold(11.5)).color(p.muted));
    ui.add_space(2.0);
}

/// Sidebar navigation entry.
pub fn nav_item(ui: &mut Ui, icon: Icon, label: &str, selected: bool) -> Response {
    let p = palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    let painter = ui.painter();

    let fill = if selected {
        p.accent_soft
    } else if response.hovered() {
        p.control
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, 9, fill);
    if selected {
        let bar = Rect::from_min_size(
            pos2(rect.left(), rect.center().y - 9.0),
            vec2(3.0, 18.0),
        );
        painter.rect_filled(bar, 2, p.accent);
    }

    let color = if selected { p.text } else { p.muted };
    let icon_rect = Rect::from_center_size(pos2(rect.left() + 24.0, rect.center().y), Vec2::splat(18.0));
    icon.paint(painter, icon_rect, if selected { p.accent } else { color });
    painter.text(
        pos2(rect.left() + 44.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        if selected { semibold(14.0) } else { FontId::proportional(14.0) },
        color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// An iOS-style switch. Returns a response marked changed on toggle.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let p = palette(ui);
    let (rect, mut response) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = p.control_strong.lerp_to_gamma(p.accent, t);
    let painter = ui.painter();
    painter.rect_filled(rect, 11, track);
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    painter.circle_filled(pos2(x, rect.center().y), 8.0, Color32::WHITE);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A pill-shaped segmented control for picking one of a few options.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, options: &[(T, &str)]) {
    const PAD: f32 = 3.0;
    const GAP: f32 = 2.0;
    let p = palette(ui);

    // Laid out manually so the order and size don't depend on the parent layout.
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label)| {
            ui.painter().layout_no_wrap(
                (*label).to_owned(),
                FontId::proportional(13.5),
                Color32::PLACEHOLDER,
            )
        })
        .collect();
    let seg_sizes: Vec<Vec2> = galleys.iter().map(|g| g.size() + vec2(24.0, 12.0)).collect();
    let height = seg_sizes.iter().map(|s| s.y).fold(0.0, f32::max) + 2.0 * PAD;
    let width = seg_sizes.iter().map(|s| s.x).sum::<f32>()
        + GAP * (options.len().saturating_sub(1)) as f32
        + 2.0 * PAD;

    let (outer, outer_response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    ui.painter().rect_filled(outer, 10, p.control);

    let mut x = outer.left() + PAD;
    for (i, ((value, _), galley)) in options.iter().zip(galleys).enumerate() {
        let rect = Rect::from_min_size(pos2(x, outer.top() + PAD), vec2(seg_sizes[i].x, height - 2.0 * PAD));
        x += seg_sizes[i].x + GAP;

        let response = ui.interact(rect, outer_response.id.with(i), Sense::click());
        if response.clicked() {
            *current = *value;
        }
        let (fill, text) = if *current == *value {
            (p.accent, Color32::WHITE)
        } else if response.hovered() {
            (p.control_hover, p.text)
        } else {
            (Color32::TRANSPARENT, p.muted)
        };
        ui.painter().rect_filled(rect, 8, fill);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, text);
        response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
}

/// A settings line: title and description on the left, control on the right.
pub fn setting_row(ui: &mut Ui, title: &str, description: &str, control: impl FnOnce(&mut Ui)) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).size(14.5).color(p.text));
            ui.label(RichText::new(description).size(12.5).color(p.muted));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}

/// Keyboard shortcut chips, e.g. `Ctrl` `Shift` `2`.
pub fn shortcut(ui: &mut Ui, keys: &[&str]) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // Inside right-aligned rows `horizontal` lays out right-to-left;
        // reverse so the chips still read Ctrl, Shift, key.
        let rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
        let ordered: Vec<&&str> = if rtl { keys.iter().rev().collect() } else { keys.iter().collect() };
        for key in ordered {
            Frame::new()
                .fill(p.control)
                .stroke(Stroke::new(1.0, p.stroke))
                .corner_radius(5)
                .inner_margin(Margin::symmetric(7, 2))
                .show(ui, |ui| {
                    ui.label(RichText::new(*key).size(12.0).color(p.muted));
                });
        }
    });
}

/// A round colour swatch; ringed when selected.
pub fn swatch(ui: &mut Ui, rgb: [u8; 3], selected: bool) -> Response {
    let p = palette(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let painter = ui.painter();
    if selected {
        painter.circle_stroke(rect.center(), 10.5, Stroke::new(2.0, p.text));
    } else if response.hovered() {
        painter.circle_stroke(rect.center(), 10.5, Stroke::new(1.0, p.control_strong));
    }
    painter.circle_filled(rect.center(), 7.5, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small rounded status badge.
pub fn badge(ui: &mut Ui, text: &str, color: Color32, fill: Color32) {
    Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(semibold(11.5)).color(color));
        });
}

/// Line icons painted with vector shapes so they stay crisp at any DPI.
#[derive(Clone, Copy)]
pub enum Icon {
    Home,
    Settings,
    Region,
    Window,
    Monitor,
    Image,
    Record,
}

impl Icon {
    pub fn paint(self, painter: &Painter, rect: Rect, color: Color32) {
        let s = rect.width();
        let stroke = Stroke::new((s / 11.0).max(1.4), color);
        let at = |x: f32, y: f32| -> Pos2 { rect.min + vec2(x * s, y * s) };

        match self {
            Icon::Home => {
                painter.add(Shape::line(
                    vec![at(0.12, 0.48), at(0.5, 0.14), at(0.88, 0.48)],
                    stroke,
                ));
                painter.add(Shape::line(
                    vec![at(0.22, 0.4), at(0.22, 0.86), at(0.78, 0.86), at(0.78, 0.4)],
                    stroke,
                ));
            }
            Icon::Settings => {
                let c = rect.center();
                painter.circle_stroke(c, s * 0.17, stroke);
                for i in 0..8 {
                    let a = i as f32 * std::f32::consts::TAU / 8.0;
                    let dir = vec2(a.cos(), a.sin());
                    painter.line_segment([c + dir * s * 0.3, c + dir * s * 0.44], stroke);
                }
                painter.circle_stroke(c, s * 0.3, Stroke::new(stroke.width * 0.8, color));
            }
            Icon::Region => {
                let l = 0.3;
                for (x, y, dx, dy) in [
                    (0.1, 0.1, 1.0, 1.0),
                    (0.9, 0.1, -1.0, 1.0),
                    (0.1, 0.9, 1.0, -1.0),
                    (0.9, 0.9, -1.0, -1.0),
                ] {
                    painter.add(Shape::line(
                        vec![at(x + dx * l, y), at(x, y), at(x, y + dy * l)],
                        stroke,
                    ));
                }
                painter.circle_filled(rect.center(), s * 0.06, color);
            }
            Icon::Window => {
                let r = Rect::from_min_max(at(0.1, 0.16), at(0.9, 0.84));
                painter.rect_stroke(r, s * 0.1, stroke, StrokeKind::Middle);
                painter.line_segment([at(0.1, 0.36), at(0.9, 0.36)], stroke);
            }
            Icon::Monitor => {
                let r = Rect::from_min_max(at(0.08, 0.12), at(0.92, 0.68));
                painter.rect_stroke(r, s * 0.08, stroke, StrokeKind::Middle);
                painter.line_segment([at(0.5, 0.68), at(0.5, 0.86)], stroke);
                painter.line_segment([at(0.3, 0.88), at(0.7, 0.88)], stroke);
            }
            Icon::Image => {
                let r = Rect::from_min_max(at(0.08, 0.14), at(0.92, 0.86));
                painter.rect_stroke(r, s * 0.1, stroke, StrokeKind::Middle);
                painter.circle_stroke(at(0.68, 0.36), s * 0.08, stroke);
                painter.add(Shape::line(
                    vec![at(0.14, 0.78), at(0.4, 0.5), at(0.6, 0.7), at(0.72, 0.6), at(0.88, 0.76)],
                    stroke,
                ));
            }
            Icon::Record => {
                // Video camera: body plus lens wedge, with a filled record dot.
                let body = Rect::from_min_max(at(0.08, 0.26), at(0.66, 0.74));
                painter.rect_stroke(body, s * 0.1, stroke, StrokeKind::Middle);
                painter.add(Shape::closed_line(vec![at(0.7, 0.42), at(0.92, 0.3), at(0.92, 0.7), at(0.7, 0.58)], stroke));
                painter.circle_filled(at(0.37, 0.5), s * 0.09, Color32::from_rgb(239, 68, 68));
            }
        }
    }
}

/// An icon sitting in a soft accent-coloured tile.
pub fn icon_tile(ui: &mut Ui, icon: Icon, size: f32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().rect_filled(rect, size * 0.28, p.accent_soft);
    icon.paint(ui.painter(), rect.shrink(size * 0.26), p.accent);
}
