//! Tray and menu bitmaps (docs/07 §7, docs/design/brand/README.md).
//!
//! * The state icons are the generated `icons/tray/<state>-<light|dark>.ico` files
//!   (M2-E1-T22), embedded with `include_bytes!`. Each holds 16/20/24/32 px 32-bit DIBs; the
//!   tray picks the size from the tray DPI ([`icon_size_for_scale`]) and the variant from
//!   `SystemAppearance.taskbar_light`. A remote context composites `remote-<theme>.ico` over
//!   the state icon (alpha "source over", same size).
//! * The menu glyphs are monochrome Lucide-like strokes (24-unit grid, 2-unit stroke)
//!   rasterized here at 16 px with analytic anti-aliasing, in the menu's foreground color.
//!   The status line dot is drawn the same way in the state color.
//!
//! Pure code: no Win32, no Tauri types; [`Rgba`] converts to a `tauri::image::Image` in
//! the native layer.

use super::model::{Glyph, TrayState};

/// An RGBA bitmap, rows top to bottom (what `tauri::image::Image::new_owned` takes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgba {
    pub fn transparent(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
        }
    }

    /// The `[r, g, b, a]` of a pixel.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Pixels with any opacity.
    pub fn opaque_count(&self) -> usize {
        self.pixels.chunks_exact(4).filter(|p| p[3] > 0).count()
    }

    /// Composites `top` over `self` ("source over", straight alpha). Sizes must match.
    pub fn draw_over(&mut self, top: &Rgba) {
        if top.width != self.width || top.height != self.height {
            return;
        }
        for (dst, src) in self
            .pixels
            .chunks_exact_mut(4)
            .zip(top.pixels.chunks_exact(4))
        {
            let sa = f32::from(src[3]) / 255.0;
            if sa <= 0.0 {
                continue;
            }
            let da = f32::from(dst[3]) / 255.0;
            let out_a = sa + da * (1.0 - sa);
            for c in 0..3 {
                let value = (f32::from(src[c]) * sa + f32::from(dst[c]) * da * (1.0 - sa)) / out_a;
                dst[c] = value.round().clamp(0.0, 255.0) as u8;
            }
            dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Sizes every tray `.ico` holds (100/125/150/200 %).
pub const TRAY_SIZES: [u32; 4] = [16, 20, 24, 32];

/// Size of a menu item bitmap (muda draws them in a 16×16 cell on Windows).
pub const MENU_GLYPH_SIZE: u32 = 16;

/// Frames of the `starting` ring.
pub const STARTING_FRAMES: u8 = 8;

/// The tray icon size for a display scale factor (1.0 = 96 DPI): 16, 20, 24 or 32 px.
pub fn icon_size_for_scale(scale: f64) -> u32 {
    if !scale.is_finite() || scale < 1.125 {
        16
    } else if scale < 1.375 {
        20
    } else if scale < 1.75 {
        24
    } else {
        32
    }
}

/// Which bitmap the tray shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IconKey {
    pub state: TrayState,
    /// Frame of the `starting` ring (0..8); ignored by the other states.
    pub frame: u8,
    /// Light taskbar (`-light` variant) or dark (`-dark`).
    pub taskbar_light: bool,
    /// The remote-context overlay (docs/13 §5).
    pub remote: bool,
    /// 16, 20, 24 or 32.
    pub size: u32,
}

macro_rules! tray_ico {
    ($name:literal) => {
        (
            include_bytes!(concat!("../../icons/tray/", $name, "-light.ico")).as_slice(),
            include_bytes!(concat!("../../icons/tray/", $name, "-dark.ico")).as_slice(),
        )
    };
}

/// `(light, dark)` bytes of every embedded tray file.
const BASE: (&[u8], &[u8]) = tray_ico!("base");
const RUNNING: (&[u8], &[u8]) = tray_ico!("running");
const STOPPED: (&[u8], &[u8]) = tray_ico!("stopped");
const UPDATING: (&[u8], &[u8]) = tray_ico!("updating");
const WARNING: (&[u8], &[u8]) = tray_ico!("warning");
const ERROR: (&[u8], &[u8]) = tray_ico!("error");
const REMOTE: (&[u8], &[u8]) = tray_ico!("remote");
const STARTING: [(&[u8], &[u8]); STARTING_FRAMES as usize] = [
    tray_ico!("starting-0"),
    tray_ico!("starting-1"),
    tray_ico!("starting-2"),
    tray_ico!("starting-3"),
    tray_ico!("starting-4"),
    tray_ico!("starting-5"),
    tray_ico!("starting-6"),
    tray_ico!("starting-7"),
];

/// The app icon (`icons/icon.ico`, the header of the menu).
const APP_ICON: &[u8] = include_bytes!("../../icons/icon.ico");

fn pick((light, dark): (&'static [u8], &'static [u8]), taskbar_light: bool) -> &'static [u8] {
    if taskbar_light { light } else { dark }
}

/// The embedded `.ico` of a state (and frame) for a taskbar theme.
pub fn state_ico(state: TrayState, frame: u8, taskbar_light: bool) -> &'static [u8] {
    let pair = match state {
        TrayState::Base => BASE,
        TrayState::Running => RUNNING,
        TrayState::Starting => STARTING[usize::from(frame % STARTING_FRAMES)],
        TrayState::Stopped => STOPPED,
        TrayState::Updating => UPDATING,
        TrayState::Warning => WARNING,
        TrayState::Error => ERROR,
    };
    pick(pair, taskbar_light)
}

/// The remote overlay for a taskbar theme.
pub fn remote_ico(taskbar_light: bool) -> &'static [u8] {
    pick(REMOTE, taskbar_light)
}

/// The tray bitmap for `key`: the state icon at its size, plus the remote overlay.
pub fn tray_icon(key: IconKey) -> Rgba {
    let size = if TRAY_SIZES.contains(&key.size) {
        key.size
    } else {
        16
    };
    let mut image = decode_ico(state_ico(key.state, key.frame, key.taskbar_light), size)
        .unwrap_or_else(|| Rgba::transparent(size, size));
    if key.remote {
        if let Some(overlay) = decode_ico(remote_ico(key.taskbar_light), size) {
            image.draw_over(&overlay);
        }
    }
    image
}

/// The app icon at `size` (16 by default: the menu cell).
pub fn app_icon(size: u32) -> Rgba {
    decode_ico(APP_ICON, size).unwrap_or_else(|| Rgba::transparent(size, size))
}

/// Decodes the `size`×`size` entry of an `.ico` holding 32-bit DIB entries (what the brand
/// generator writes below 256 px). `None` when there is no such entry or it is not a 32-bit
/// DIB.
pub fn decode_ico(bytes: &[u8], size: u32) -> Option<Rgba> {
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None;
    }
    let count = usize::from(u16_at(4)?);
    for i in 0..count {
        let entry = 6 + 16 * i;
        let width = match *bytes.get(entry)? {
            0 => 256,
            w => u32::from(w),
        };
        if width != size {
            continue;
        }
        let length = u32_at(entry + 8)? as usize;
        let offset = u32_at(entry + 12)? as usize;
        let data = bytes.get(offset..offset.checked_add(length)?)?;
        return decode_dib(data, size);
    }
    None
}

/// A `BITMAPINFOHEADER` + 32-bit BGRA rows (bottom-up) + the 1-bit AND mask.
fn decode_dib(data: &[u8], size: u32) -> Option<Rgba> {
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
    };
    let header = u32_at(0)? as usize;
    if header < 40 || u16_at(14)? != 32 {
        return None;
    }
    let width = u32_at(4)?;
    if width != size {
        return None;
    }
    let (w, h) = (size as usize, size as usize);
    let pixels_at = header;
    let mask_at = pixels_at + w * h * 4;
    let mask_stride = w.div_ceil(32) * 4;
    let mut out = Rgba::transparent(size, size);
    let mut any_alpha = false;
    for row in 0..h {
        // DIB rows are stored bottom-up.
        let src_row = h - 1 - row;
        for col in 0..w {
            let at = pixels_at + (src_row * w + col) * 4;
            let bgra = data.get(at..at + 4)?;
            let dst = (row * w + col) * 4;
            out.pixels[dst] = bgra[2];
            out.pixels[dst + 1] = bgra[1];
            out.pixels[dst + 2] = bgra[0];
            out.pixels[dst + 3] = bgra[3];
            any_alpha |= bgra[3] != 0;
        }
    }
    if !any_alpha {
        // An old-style entry: transparency comes from the AND mask.
        for row in 0..h {
            let src_row = h - 1 - row;
            for col in 0..w {
                let byte = data
                    .get(mask_at + src_row * mask_stride + col / 8)
                    .copied()
                    .unwrap_or(0);
                let masked = byte & (0x80 >> (col % 8)) != 0;
                out.pixels[(row * w + col) * 4 + 3] = if masked { 0 } else { 255 };
            }
        }
    }
    Some(out)
}

// --- menu glyphs ----------------------------------------------------------------------

/// Foreground of the menu glyphs: the menu text color of each Windows menu theme.
pub fn glyph_color(dark_menu: bool) -> [u8; 3] {
    if dark_menu {
        [0xF3, 0xF3, 0xF3]
    } else {
        [0x1F, 0x1F, 0x1F]
    }
}

/// Colors of the status dot (docs/design/brand: the badge colors).
pub fn state_color(state: TrayState) -> [u8; 3] {
    match state {
        TrayState::Running => [0x28, 0xC0, 0x4C],
        TrayState::Starting | TrayState::Updating => [0x16, 0x68, 0xF0],
        TrayState::Warning => [0xF5, 0xA0, 0x0B],
        TrayState::Error | TrayState::Stopped => [0xE0, 0x26, 0x2B],
        TrayState::Base => [0x8A, 0x8A, 0x8A],
    }
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    /// A segment from `(x1, y1)` to `(x2, y2)`.
    Line(f32, f32, f32, f32),
    /// A circle outline: center and radius.
    Circle(f32, f32, f32),
    /// An arc: center, radius, start and end angle in degrees (clockwise, 0 = +x, y down).
    Arc(f32, f32, f32, f32, f32),
    /// A rounded rectangle outline: x, y, width, height, corner radius.
    Rect(f32, f32, f32, f32, f32),
    /// An ellipse outline: center and radii.
    Ellipse(f32, f32, f32, f32),
    /// A filled disc: center and radius (drawn without stroke).
    Disc(f32, f32, f32),
}

/// Lucide-like strokes of each glyph (24-unit grid, as in the reference mockup).
fn shapes(glyph: Glyph) -> Vec<Shape> {
    use Shape::*;
    match glyph {
        // app-window
        Glyph::Open => vec![
            Rect(2.0, 4.0, 20.0, 16.0, 2.0),
            Line(2.0, 8.0, 22.0, 8.0),
            Line(6.0, 4.0, 6.0, 8.0),
            Line(10.0, 4.0, 10.0, 8.0),
        ],
        // server
        Glyph::Services => vec![
            Rect(2.0, 2.0, 20.0, 8.0, 2.0),
            Rect(2.0, 14.0, 20.0, 8.0, 2.0),
            Disc(6.0, 6.0, 1.1),
            Disc(6.0, 18.0, 1.1),
        ],
        // globe
        Glyph::Sites => vec![
            Circle(12.0, 12.0, 10.0),
            Ellipse(12.0, 12.0, 4.5, 10.0),
            Line(2.0, 12.0, 22.0, 12.0),
        ],
        // code
        Glyph::Php => vec![
            Line(16.0, 18.0, 22.0, 12.0),
            Line(22.0, 12.0, 16.0, 6.0),
            Line(8.0, 6.0, 2.0, 12.0),
            Line(2.0, 12.0, 8.0, 18.0),
        ],
        // play
        Glyph::Play => vec![
            Line(6.0, 3.0, 20.0, 12.0),
            Line(20.0, 12.0, 6.0, 21.0),
            Line(6.0, 21.0, 6.0, 3.0),
        ],
        // rotate-cw
        Glyph::Restart => vec![
            Arc(12.0, 12.0, 9.0, -40.0, 270.0),
            Line(21.0, 3.0, 21.0, 8.0),
            Line(21.0, 8.0, 16.0, 8.0),
        ],
        // square
        Glyph::Stop => vec![Rect(4.0, 4.0, 16.0, 16.0, 2.0)],
        // file-text
        Glyph::Logs => vec![
            Line(6.0, 2.0, 14.0, 2.0),
            Line(14.0, 2.0, 20.0, 8.0),
            Line(20.0, 8.0, 20.0, 22.0),
            Line(20.0, 22.0, 4.0, 22.0),
            Line(4.0, 22.0, 4.0, 2.0),
            Line(4.0, 2.0, 6.0, 2.0),
            Line(14.0, 2.0, 14.0, 8.0),
            Line(14.0, 8.0, 20.0, 8.0),
            Line(8.0, 13.0, 16.0, 13.0),
            Line(8.0, 17.0, 16.0, 17.0),
        ],
        // mail
        Glyph::Mail => vec![
            Rect(2.0, 4.0, 20.0, 16.0, 2.0),
            Line(2.0, 7.0, 12.0, 13.0),
            Line(12.0, 13.0, 22.0, 7.0),
        ],
        // terminal
        Glyph::Terminal => vec![
            Line(4.0, 17.0, 10.0, 11.0),
            Line(10.0, 11.0, 4.0, 5.0),
            Line(12.0, 19.0, 20.0, 19.0),
        ],
        // settings-2 (sliders)
        Glyph::Settings => vec![
            Line(20.0, 7.0, 11.0, 7.0),
            Line(14.0, 17.0, 4.0, 17.0),
            Circle(7.0, 7.0, 3.0),
            Circle(17.0, 17.0, 3.0),
        ],
        // power
        Glyph::Quit => vec![
            Line(12.0, 2.0, 12.0, 12.0),
            Arc(12.0, 13.0, 9.0, -45.0, 225.0),
        ],
        // A dot centered in the cell (status line, active context).
        Glyph::Dot(_) => vec![Disc(12.0, 12.0, 5.5)],
        Glyph::App => Vec::new(),
    }
}

fn distance(shape: Shape, px: f32, py: f32) -> f32 {
    fn segment(px: f32, py: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len2 = dx * dx + dy * dy;
        let t = if len2 == 0.0 {
            0.0
        } else {
            (((px - x1) * dx + (py - y1) * dy) / len2).clamp(0.0, 1.0)
        };
        let (cx, cy) = (x1 + t * dx, y1 + t * dy);
        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
    }
    match shape {
        Shape::Line(x1, y1, x2, y2) => segment(px, py, x1, y1, x2, y2),
        Shape::Circle(cx, cy, r) => (((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - r).abs(),
        Shape::Arc(cx, cy, r, from, to) => {
            let angle = (py - cy).atan2(px - cx).to_degrees();
            let inside = |a: f32| {
                let span = to - from;
                let rel = (a - from).rem_euclid(360.0);
                rel <= span
            };
            if inside(angle) {
                (((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - r).abs()
            } else {
                let end = |deg: f32| {
                    let rad = deg.to_radians();
                    let (ex, ey) = (cx + r * rad.cos(), cy + r * rad.sin());
                    ((px - ex).powi(2) + (py - ey).powi(2)).sqrt()
                };
                end(from).min(end(to))
            }
        }
        Shape::Rect(x, y, w, h, r) => {
            // |signed distance| of a rounded box.
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            let qx = (px - cx).abs() - (w / 2.0 - r);
            let qy = (py - cy).abs() - (h / 2.0 - r);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
            (outside + qx.max(qy).min(0.0) - r).abs()
        }
        Shape::Ellipse(cx, cy, rx, ry) => {
            // First-order distance to an ellipse outline: |f| / |grad f|.
            let (dx, dy) = (px - cx, py - cy);
            let f = (dx / rx).powi(2) + (dy / ry).powi(2) - 1.0;
            let gx = 2.0 * dx / (rx * rx);
            let gy = 2.0 * dy / (ry * ry);
            let grad = (gx * gx + gy * gy).sqrt().max(1e-3);
            (f / grad).abs()
        }
        Shape::Disc(cx, cy, r) => {
            // Negative inside: the coverage below fills it.
            ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - r - 1.0
        }
    }
}

/// Rasterizes a glyph at `size` px in `color` (2-unit stroke on the 24-unit grid). The app
/// icon is the decoded `icon.ico`; `Dot` uses the state color.
pub fn glyph(glyph: Glyph, size: u32, dark_menu: bool) -> Rgba {
    if glyph == Glyph::App {
        return app_icon(size);
    }
    let color = match glyph {
        Glyph::Dot(state) => state_color(state),
        _ => glyph_color(dark_menu),
    };
    let shapes = shapes(glyph);
    let scale = size as f32 / 24.0;
    let half_width = 1.0; // grid units (stroke width 2)
    let mut out = Rgba::transparent(size, size);
    // 4×4 supersampling of the stroke coverage.
    const SAMPLES: u32 = 4;
    for y in 0..size {
        for x in 0..size {
            let mut covered = 0u32;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let gx = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / scale;
                    let gy = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / scale;
                    let d = shapes
                        .iter()
                        .map(|s| distance(*s, gx, gy))
                        .fold(f32::INFINITY, f32::min);
                    if d <= half_width {
                        covered += 1;
                    }
                }
            }
            if covered > 0 {
                let i = ((y * size + x) * 4) as usize;
                out.pixels[i..i + 3].copy_from_slice(&color);
                out.pixels[i + 3] = ((covered * 255) / (SAMPLES * SAMPLES)) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_follow_the_tray_dpi() {
        assert_eq!(icon_size_for_scale(1.0), 16);
        assert_eq!(icon_size_for_scale(1.25), 20);
        assert_eq!(icon_size_for_scale(1.5), 24);
        assert_eq!(icon_size_for_scale(1.75), 32);
        assert_eq!(icon_size_for_scale(2.0), 32);
        assert_eq!(icon_size_for_scale(3.0), 32);
        assert_eq!(icon_size_for_scale(f64::NAN), 16);
    }

    #[test]
    fn every_embedded_icon_decodes_at_every_size() {
        let states = [
            TrayState::Base,
            TrayState::Running,
            TrayState::Starting,
            TrayState::Stopped,
            TrayState::Updating,
            TrayState::Warning,
            TrayState::Error,
        ];
        for light in [true, false] {
            for size in TRAY_SIZES {
                for state in states {
                    for frame in 0..STARTING_FRAMES {
                        let image = decode_ico(state_ico(state, frame, light), size)
                            .unwrap_or_else(|| panic!("{state:?} {frame} {size} {light}"));
                        assert_eq!((image.width, image.height), (size, size));
                        assert!(image.opaque_count() > (size * size / 4) as usize);
                    }
                }
                assert!(decode_ico(remote_ico(light), size).is_some());
            }
        }
        assert!(app_icon(16).opaque_count() > 64);
        assert!(decode_ico(APP_ICON, 17).is_none());
        assert!(decode_ico(b"not an icon", 16).is_none());
    }

    #[test]
    fn variants_and_states_differ() {
        let key = |state, light| IconKey {
            state,
            frame: 0,
            taskbar_light: light,
            remote: false,
            size: 16,
        };
        assert_ne!(
            tray_icon(key(TrayState::Running, true)),
            tray_icon(key(TrayState::Running, false))
        );
        assert_ne!(
            tray_icon(key(TrayState::Running, true)),
            tray_icon(key(TrayState::Error, true))
        );
        let frames: Vec<Rgba> = (0..STARTING_FRAMES)
            .map(|frame| {
                tray_icon(IconKey {
                    frame,
                    ..key(TrayState::Starting, true)
                })
            })
            .collect();
        assert_ne!(frames[0], frames[1]);
    }

    #[test]
    fn the_remote_overlay_marks_the_top_left_corner() {
        let local = tray_icon(IconKey {
            state: TrayState::Running,
            frame: 0,
            taskbar_light: false,
            remote: false,
            size: 32,
        });
        let remote = tray_icon(IconKey {
            remote: true,
            ..IconKey {
                state: TrayState::Running,
                frame: 0,
                taskbar_light: false,
                remote: false,
                size: 32,
            }
        });
        assert_ne!(local, remote);
        // Only the top-left quadrant changes.
        for y in 0..32 {
            for x in 0..32 {
                if x >= 16 || y >= 16 {
                    assert_eq!(local.pixel(x, y), remote.pixel(x, y), "({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn glyphs_are_monochrome_and_follow_the_menu_theme() {
        for g in [
            Glyph::Open,
            Glyph::Services,
            Glyph::Sites,
            Glyph::Php,
            Glyph::Play,
            Glyph::Restart,
            Glyph::Stop,
            Glyph::Logs,
            Glyph::Mail,
            Glyph::Terminal,
            Glyph::Settings,
            Glyph::Quit,
        ] {
            for dark in [true, false] {
                let image = glyph(g, MENU_GLYPH_SIZE, dark);
                let painted = image.opaque_count();
                assert!(painted > 20 && painted < 200, "{g:?}: {painted}");
                let color = glyph_color(dark);
                for p in image.pixels.chunks_exact(4).filter(|p| p[3] > 0) {
                    assert_eq!(&p[..3], &color, "{g:?}");
                }
            }
        }
        let dot = glyph(Glyph::Dot(TrayState::Running), 16, false);
        assert_eq!(&dot.pixel(8, 8)[..3], &state_color(TrayState::Running));
        assert_eq!(dot.pixel(8, 8)[3], 255);
        assert_eq!(dot.pixel(0, 0)[3], 0);
    }

    #[test]
    fn source_over_keeps_the_bottom_where_the_top_is_clear() {
        let mut bottom = Rgba::transparent(1, 2);
        bottom.pixels = vec![10, 20, 30, 255, 10, 20, 30, 255];
        let mut top = Rgba::transparent(1, 2);
        top.pixels = vec![0, 0, 0, 0, 200, 100, 0, 255];
        bottom.draw_over(&top);
        assert_eq!(bottom.pixel(0, 0), [10, 20, 30, 255]);
        assert_eq!(bottom.pixel(0, 1), [200, 100, 0, 255]);
    }
}
