//! Tray icons — the PortZero brand mark (the same navy-ring-and-cyan-"P" favicon
//! used on portzero.net) with a small filled status dot badged in the corner, so
//! the tray colour always matches [`Health`] exactly while still reading as the
//! product icon rather than a bare traffic light.
//!
//! The renderer produces a backend-neutral [`RgbaImage`]; each platform converts
//! it to its native icon type (`tray_icon::Icon` on Windows/macOS, `ksni::Icon`
//! on Linux) so this module depends on no GUI toolkit.

use crate::state::Health;

const SIZE: u32 = 32;

/// The brand mark, pre-rendered to a `SIZE x SIZE` straight-alpha RGBA buffer
/// (generated from the same mark used for `portzero.net`'s favicon and the
/// desktop app's window/taskbar icon — see `client/crates/app/icons/`).
const MARK: &[u8] = include_bytes!("assets/mark_32.rgba");

/// A raw RGBA (8-bit, non-premultiplied, R,G,B,A byte order) image.
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// RGB status colours, chosen for contrast on both light and dark menu bars.
fn color(health: Health) -> (u8, u8, u8) {
    match health {
        Health::Ok => (46, 160, 67),        // green
        Health::Degraded => (210, 153, 34), // amber
        Health::Down => (218, 54, 51),      // red
    }
}

/// Build the tray icon for a given health state: the brand mark with a status
/// badge dot overlaid in the bottom-right corner.
pub fn image_for_health(health: Health) -> RgbaImage {
    let mut buf = MARK.to_vec();
    draw_badge(&mut buf, color(health));
    RgbaImage {
        width: SIZE,
        height: SIZE,
        rgba: buf,
    }
}

/// Paint a small filled disc with a soft 1px darker rim over `buf` (a `SIZE x
/// SIZE` RGBA buffer), badged into the bottom-right corner. The badge is fully
/// opaque so it cleanly overwrites the mark pixels beneath it.
fn draw_badge(buf: &mut [u8], (r, g, b): (u8, u8, u8)) {
    let n = SIZE as i32;
    const RADIUS: f32 = 7.0;
    // Centered a radius-and-a-bit in from the bottom-right edge.
    let center = n as f32 - 1.0 - RADIUS;

    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            // Antialias the last pixel of the edge: full alpha inside `RADIUS`,
            // fading to 0 across one pixel, so the dot doesn't look jagged.
            let alpha = if dist <= RADIUS {
                1.0
            } else if dist <= RADIUS + 1.0 {
                RADIUS + 1.0 - dist
            } else {
                0.0
            };
            if alpha <= 0.0 {
                continue;
            }
            // Darken the outer rim slightly for definition on light backgrounds.
            let rim = dist > RADIUS - 1.5;
            let scale = if rim { 0.78 } else { 1.0 };
            let idx = ((y * n + x) * 4) as usize;
            let (br, bg_, bb) = (
                (r as f32 * scale) as u8,
                (g as f32 * scale) as u8,
                (b as f32 * scale) as u8,
            );
            // Blend the badge over whatever the mark drew at this pixel.
            buf[idx] = lerp(buf[idx], br, alpha);
            buf[idx + 1] = lerp(buf[idx + 1], bg_, alpha);
            buf[idx + 2] = lerp(buf[idx + 2], bb, alpha);
            buf[idx + 3] = lerp(buf[idx + 3], 255, alpha);
        }
    }
}

/// Linear-interpolate a single channel from `under` to `over` by `alpha` (0..=1).
fn lerp(under: u8, over: u8, alpha: f32) -> u8 {
    (under as f32 * (1.0 - alpha) + over as f32 * alpha) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_health_maps_to_a_distinct_colour() {
        let ok = color(Health::Ok);
        let degraded = color(Health::Degraded);
        let down = color(Health::Down);
        assert_ne!(ok, degraded);
        assert_ne!(degraded, down);
        assert_ne!(ok, down);
    }

    #[test]
    fn image_has_expected_size() {
        let image = image_for_health(Health::Ok);
        assert_eq!(image.width, SIZE);
        assert_eq!(image.height, SIZE);
        assert_eq!(image.rgba.len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn badge_center_is_fully_opaque_and_matches_health_colour() {
        let (r, g, b) = color(Health::Down);
        let image = image_for_health(Health::Down);
        let n = SIZE as i32;
        const RADIUS: f32 = 7.0;
        let center = (n as f32 - 1.0 - RADIUS).round() as usize;
        let idx = (center * SIZE as usize + center) * 4;
        assert_eq!(image.rgba[idx + 3], 255);
        // The badge center isn't rim-darkened, so it matches the health colour exactly.
        assert_eq!(image.rgba[idx], r);
        assert_eq!(image.rgba[idx + 1], g);
        assert_eq!(image.rgba[idx + 2], b);
    }

    #[test]
    fn top_left_corner_stays_transparent() {
        // The mark leaves a margin before the circle starts, and the badge is
        // in the opposite (bottom-right) corner, so top-left stays untouched.
        let image = image_for_health(Health::Degraded);
        assert_eq!(image.rgba[3], 0);
    }
}
