//! The app icon, rendered procedurally so no binary assets are needed yet:
//! purple capture-frame corners around a lightning bolt on a white tile.

const PURPLE: [f32; 3] = [159.0, 87.0, 245.0];

/// Bolt outline in unit coordinates (clockwise from the top tip).
const BOLT: [(f32, f32); 6] = [
    (0.590, 0.240),
    (0.549, 0.479),
    (0.657, 0.479),
    (0.435, 0.752),
    (0.488, 0.567),
    (0.345, 0.567),
];

/// Renders the icon as straight-alpha RGBA8 at `size`×`size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);

    // Frame: rounded-square outline, with the middle of each side cut away.
    let half = 0.29;
    let radius = 0.075;
    let stroke = 0.05f32.max(1.6 / s); // stays legible in the tray
    let gap = 0.09; // arms stop this far from the centre line

    for y in 0..size {
        for x in 0..size {
            // Unit coordinates, centred.
            let (u, v) = ((x as f32 + 0.5) / s - 0.5, (y as f32 + 0.5) / s - 0.5);

            let tile = coverage(rounded_square_sdf(u, v, 0.5, 0.22) * s);

            let outline = rounded_square_sdf(u, v, half, radius).abs() - stroke / 2.0;
            let mut frame = outline.max(gap - u.abs().min(v.abs()));
            // Round caps at the arm ends.
            for (cx, cy) in [(gap, half), (half, gap)] {
                let d = ((u.abs() - cx).powi(2) + (v.abs() - cy).powi(2)).sqrt() - stroke / 2.0;
                frame = frame.min(d);
            }

            let bolt = polygon_sdf(u + 0.5, v + 0.5, &BOLT) - 0.004;
            let ink = coverage(frame.min(bolt) * s);

            let mut rgb = [0u8; 3];
            for i in 0..3 {
                rgb[i] = (255.0 + (PURPLE[i] - 255.0) * ink).round() as u8;
            }
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (tile * 255.0).round() as u8]);
        }
    }
    out
}

/// Signed distance to a rounded square centred on the origin.
fn rounded_square_sdf(x: f32, y: f32, half: f32, radius: f32) -> f32 {
    let qx = x.abs() - (half - radius);
    let qy = y.abs() - (half - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

/// Signed distance to a simple polygon (negative inside).
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

/// Anti-aliased coverage for a signed distance in pixels (negative = inside).
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    /// Writes the icon at several sizes for a visual check:
    /// `cargo test -p capshotx icon -- --ignored`.
    #[test]
    #[ignore]
    fn render_preview() {
        for size in [256u32, 64, 32, 16] {
            let px = super::rgba(size);
            let path = std::env::temp_dir().join(format!("capshotx-icon-{size}.png"));
            let file = std::fs::File::create(&path).unwrap();
            let mut enc = png::Encoder::new(file, size, size);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&px).unwrap();
            println!("{}", path.display());
        }
    }
}
