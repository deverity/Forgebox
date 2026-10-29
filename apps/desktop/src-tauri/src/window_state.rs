//! Window placement per monitor (docs/07 §4, [ADR-0035]): `window-state.json` in the GUI
//! data dir.
//!
//! One placement is kept per monitor (identified by its name and physical bounds), most
//! recent first. At start the most recent placement whose monitor is still connected wins;
//! otherwise the placement saved for another connected monitor; otherwise the default size
//! centered by Tauri. A placement is always fitted into its monitor's work area, so a
//! window never comes back off-screen or larger than the screen (e.g. after a resolution or
//! scaling change).
//!
//! `tauri-plugin-window-state` is not used: it always creates Tauri's `app_config_dir`
//! (`%APPDATA%\dev.forgebox.app`) when saving, which breaks portable mode (docs/08 §2.2),
//! and it keeps one placement per window rather than per monitor.
//!
//! Positions are physical pixels (outer position), sizes physical inner sizes, as Tauri
//! reports them; `scale` rescales a size when the monitor's scaling changed since.
//!
//! [ADR-0035]: ../../../../docs/adr/0035-window-chrome-materials.md

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Version of the file format.
pub const FORMAT_VERSION: u32 = 1;
/// Monitors remembered at most.
pub const MAX_PLACEMENTS: usize = 8;

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    fn right(&self) -> i64 {
        i64::from(self.x) + i64::from(self.width)
    }

    fn bottom(&self) -> i64 {
        i64::from(self.y) + i64::from(self.height)
    }
}

/// Identity of a monitor: its device name and physical bounds (a monitor whose resolution
/// changed is "another monitor": its old placement no longer fits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorId {
    pub name: String,
    pub bounds: Rect,
}

/// A connected monitor, as Tauri reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    pub work_area: Rect,
    pub scale: f64,
}

impl MonitorInfo {
    /// [`min_logical_size`] of this monitor, in physical pixels.
    pub fn min_physical_size(&self) -> (u32, u32) {
        let (w, h) = min_logical_size(self.work_area, self.scale);
        let scale = if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        };
        ((w * scale).floor() as u32, (h * scale).floor() as u32)
    }
}

/// Where the window was on one monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub monitor: MonitorId,
    /// Outer position and inner size of the normal (not maximized) window.
    pub rect: Rect,
    /// Scale factor of the monitor when saved.
    pub scale: f64,
    pub maximized: bool,
}

/// Content of `window-state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowStateFile {
    pub version: u32,
    /// Most recent first, one per monitor.
    pub placements: Vec<Placement>,
}

impl Default for WindowStateFile {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            placements: Vec::new(),
        }
    }
}

/// What to do at start.
#[derive(Debug, Clone, PartialEq)]
pub struct Restore {
    /// Fitted rectangle (physical).
    pub rect: Rect,
    pub maximized: bool,
}

impl WindowStateFile {
    /// Parses the file; anything unreadable or of another version is "nothing saved".
    pub fn parse(text: &str) -> Self {
        match serde_json::from_str::<WindowStateFile>(text) {
            Ok(file) if file.version == FORMAT_VERSION => file,
            _ => Self::default(),
        }
    }

    /// Reads `path` (missing or invalid → empty). Blocking.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Records `placement` as the most recent one, replacing the one of its monitor.
    pub fn record(&mut self, placement: Placement) {
        self.placements.retain(|p| p.monitor != placement.monitor);
        self.placements.insert(0, placement);
        self.placements.truncate(MAX_PLACEMENTS);
    }

    /// The placement to restore among `monitors` (see the module docs), fitted into its
    /// monitor's work area with at least the minimum size when the area allows it.
    pub fn restore<'m>(&self, monitors: &'m [MonitorInfo]) -> Option<(Restore, &'m MonitorInfo)> {
        self.placements.iter().find_map(|placement| {
            let monitor = monitors.iter().find(|m| m.id == placement.monitor)?;
            let rect = rescale(placement.rect, placement.scale, monitor.scale);
            let restore = Restore {
                rect: fit(rect, monitor.work_area, monitor.min_physical_size()),
                maximized: placement.maximized,
            };
            Some((restore, monitor))
        })
    }

    /// Writes the file atomically (`.tmp` + rename). Blocking.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp: PathBuf = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, path)
    }
}

/// Scales the size of `rect` from `from` to `to` scale factor (position unchanged).
fn rescale(rect: Rect, from: f64, to: f64) -> Rect {
    if !(from.is_finite() && to.is_finite()) || from <= 0.0 || to <= 0.0 || from == to {
        return rect;
    }
    let factor = to / from;
    let scale = |v: u32| {
        ((f64::from(v) * factor)
            .round()
            .clamp(1.0, f64::from(u32::MAX))) as u32
    };
    Rect {
        width: scale(rect.width),
        height: scale(rect.height),
        ..rect
    }
}

/// Fits `rect` into `area`: never larger than the area, at least `min` when the area
/// allows it, and moved (not resized further) until it lies fully inside.
pub fn fit(rect: Rect, area: Rect, min: (u32, u32)) -> Rect {
    let width = rect.width.max(min.0).min(area.width).max(1);
    let height = rect.height.max(min.1).min(area.height).max(1);
    let clamp_axis = |pos: i32, len: u32, start: i32, span: u32| -> i32 {
        let max = i64::from(start) + i64::from(span) - i64::from(len);
        i64::from(pos).clamp(i64::from(start), max.max(i64::from(start))) as i32
    };
    let fitted = Rect {
        x: clamp_axis(rect.x, width, area.x, area.width),
        y: clamp_axis(rect.y, height, area.y, area.height),
        width,
        height,
    };
    debug_assert!(fitted.right() <= area.right() && fitted.bottom() <= area.bottom());
    fitted
}

/// Minimum window size in logical pixels: 960×600, or the work area when smaller
/// (decision "min(960x600, work area)", docs/07 §4).
pub fn min_logical_size(work_area: Rect, scale: f64) -> (f64, f64) {
    const MIN: (f64, f64) = (960.0, 600.0);
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let area_w = f64::from(work_area.width) / scale;
    let area_h = f64::from(work_area.height) / scale;
    (MIN.0.min(area_w).floor(), MIN.1.min(area_h).floor())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn monitor(name: &str, bounds: Rect, scale: f64) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId {
                name: name.to_owned(),
                bounds,
            },
            // A 48 px taskbar at the bottom.
            work_area: Rect {
                height: bounds.height - 48,
                ..bounds
            },
            scale,
        }
    }

    fn placement(m: &MonitorInfo, r: Rect, maximized: bool) -> Placement {
        Placement {
            monitor: m.id.clone(),
            rect: r,
            scale: m.scale,
            maximized,
        }
    }

    #[test]
    fn the_most_recent_connected_monitor_wins() {
        let laptop = monitor(r"\\.\DISPLAY1", rect(0, 0, 1920, 1080), 1.25);
        let external = monitor(r"\\.\DISPLAY2", rect(1920, 0, 2560, 1440), 1.0);
        let mut file = WindowStateFile::default();
        file.record(placement(&laptop, rect(100, 100, 1200, 800), false));
        file.record(placement(&external, rect(2100, 50, 1600, 1000), true));

        let both = [laptop.clone(), external.clone()];
        let (r, m) = file.restore(&both).unwrap();
        assert_eq!(r.rect, rect(2100, 50, 1600, 1000));
        assert!(r.maximized);
        assert_eq!(m.id, external.id);

        // Undocked: the laptop's own placement comes back instead of the default.
        let (r, _) = file.restore(std::slice::from_ref(&laptop)).unwrap();
        assert_eq!(r.rect, rect(100, 100, 1200, 800));
        assert!(!r.maximized);

        // An unknown monitor set: nothing to restore (Tauri centers the default size).
        let other = monitor(r"\\.\DISPLAY9", rect(0, 0, 1366, 768), 1.0);
        assert_eq!(file.restore(&[other]), None);
    }

    #[test]
    fn one_placement_per_monitor_most_recent_first_and_bounded() {
        let mut file = WindowStateFile::default();
        let a = monitor("A", rect(0, 0, 1920, 1080), 1.0);
        file.record(placement(&a, rect(0, 0, 1000, 700), false));
        file.record(placement(&a, rect(10, 10, 1100, 700), false));
        assert_eq!(file.placements.len(), 1);
        assert_eq!(file.placements[0].rect.x, 10);
        for i in 0..20 {
            let m = monitor(&format!("M{i}"), rect(0, 0, 1920, 1080), 1.0);
            file.record(placement(&m, rect(0, 0, 1000, 700), false));
        }
        assert_eq!(file.placements.len(), MAX_PLACEMENTS);
        assert_eq!(file.placements[0].monitor.name, "M19");
    }

    #[test]
    fn placements_are_fitted_into_the_work_area() {
        let area = rect(0, 0, 1366, 720);
        // Too big: shrunk to the area.
        assert_eq!(
            fit(rect(0, 0, 2000, 1200), area, (960, 600)),
            rect(0, 0, 1366, 720)
        );
        // Off to the right and below: moved back inside, size kept.
        assert_eq!(
            fit(rect(1000, 600, 1000, 650), area, (960, 600)),
            rect(366, 70, 1000, 650)
        );
        // Off to the left/top (negative coordinates of a monitor that is gone).
        assert_eq!(
            fit(rect(-3000, -50, 1000, 650), area, (960, 600)),
            rect(0, 0, 1000, 650)
        );
        // Smaller than the minimum: grown to it.
        assert_eq!(
            fit(rect(10, 10, 300, 200), area, (960, 600)),
            rect(10, 10, 960, 600)
        );
        // A work area smaller than the minimum caps it (min(960x600, work area)).
        let tiny = rect(0, 0, 800, 560);
        assert_eq!(
            fit(rect(0, 0, 1200, 800), tiny, (960, 600)),
            rect(0, 0, 800, 560)
        );
        // Secondary monitor with negative origin.
        let left = rect(-1920, 0, 1920, 1032);
        assert_eq!(
            fit(rect(-100, 10, 1200, 800), left, (960, 600)),
            rect(-1200, 10, 1200, 800)
        );
    }

    #[test]
    fn a_changed_scale_rescales_the_size() {
        let mut m = monitor("A", rect(0, 0, 3840, 2160), 1.5);
        let mut file = WindowStateFile::default();
        file.record(placement(&m, rect(100, 100, 1800, 1200), false));
        m.scale = 2.0;
        let monitors = [m];
        let (r, _) = file.restore(&monitors).unwrap();
        assert_eq!(r.rect, rect(100, 100, 2400, 1600));

        // A placement smaller than the minimum of the (scaled) monitor grows to it.
        let mut file = WindowStateFile::default();
        file.record(placement(&monitors[0], rect(0, 0, 400, 300), false));
        let (r, _) = file.restore(&monitors).unwrap();
        assert_eq!((r.rect.width, r.rect.height), (1920, 1200));
    }

    #[test]
    fn the_minimum_size_is_capped_by_the_work_area() {
        assert_eq!(
            min_logical_size(rect(0, 0, 1920, 1032), 1.0),
            (960.0, 600.0)
        );
        assert_eq!(
            monitor("A", rect(0, 0, 1920, 1080), 1.25).min_physical_size(),
            (1200, 750)
        );
        assert_eq!(
            monitor("B", rect(0, 0, 1366, 768), 1.5).min_physical_size(),
            (1365, 720)
        );
        // 1366x768 at 150 %: 910x480 logical of work area.
        assert_eq!(min_logical_size(rect(0, 0, 1366, 720), 1.5), (910.0, 480.0));
        assert_eq!(
            min_logical_size(rect(0, 0, 1280, 720), f64::NAN),
            (960.0, 600.0)
        );
    }

    #[test]
    fn the_file_round_trips_and_rejects_other_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui").join("window-state.json");
        assert_eq!(WindowStateFile::load(&path), WindowStateFile::default());
        let mut file = WindowStateFile::default();
        let m = monitor("A", rect(0, 0, 1920, 1080), 1.0);
        file.record(placement(&m, rect(1, 2, 1000, 700), true));
        file.save(&path).unwrap();
        assert_eq!(WindowStateFile::load(&path), file);
        assert!(!path.with_extension("json.tmp").exists());

        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"version\": 1", "\"version\": 2");
        assert_eq!(WindowStateFile::parse(&text), WindowStateFile::default());
        assert_eq!(WindowStateFile::parse("{"), WindowStateFile::default());
    }
}
