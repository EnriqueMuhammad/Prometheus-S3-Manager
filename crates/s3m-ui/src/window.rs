//! Main window size: opens usable on small or high-DPI screens, and remembers its size.

use std::collections::HashMap;

use egui::{Context, Pos2, Vec2, ViewportCommand, pos2, vec2};
use serde::{Deserialize, Serialize};

/// Room the taskbar takes at the edge of the monitor, in points.
const TASKBAR: f32 = 48.0;
/// Size of a restored window when the monitor has room for it, in points.
pub const PREFERRED: Vec2 = vec2(1360.0, 840.0);
/// Smallest inner size of the window, in points.
pub const MIN_SIZE: Vec2 = vec2(900.0, 560.0);

/// eframe's storage key for the window settings it restores at start-up.
pub const EFRAME_WINDOW_KEY: &str = "window";

/// The window settings eframe restores at start-up (its `WindowSettings`, same field names),
/// without a position.
///
/// eframe's own `persist_window` also saves the position and restores it wrongly when the
/// monitors use different scale factors: points and pixels get mixed up, and the window opens
/// off screen at twice its size. With `persist_window` off, the app writes this instead, so the
/// size and maximized state come back and the window opens centered on the primary monitor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// Inner size of the restored (not maximized) window, in points.
    pub inner_size_points: Option<Vec2>,
    /// Whether the window was maximized.
    pub maximized: bool,
}

/// Drops the position from eframe's window settings in the contents of its `app.ron` (a map
/// of keys to RON text), keeping the size and maximized state. Settings written by earlier
/// builds carry a position; eframe reads them before the app starts, so they are fixed in the
/// file. Returns the new contents, or `None` when nothing needs to change.
pub fn strip_window_position(app_ron: &str) -> Option<String> {
    let mut values: HashMap<String, String> = ron::from_str(app_ron).ok()?;
    let saved = values.get(EFRAME_WINDOW_KEY)?;
    if !saved.contains("position") {
        return None;
    }
    let state: WindowState = ron::from_str(saved).ok()?;
    values.insert(EFRAME_WINDOW_KEY.to_owned(), ron::to_string(&state).ok()?);
    ron::ser::to_string_pretty(&values, ron::ser::PrettyConfig::default()).ok()
}

/// Watches the main window; call [`WindowFit::update`] every frame.
#[derive(Debug, Default)]
pub struct WindowFit {
    /// The size at start-up was checked.
    started: bool,
    /// Monitor and window size of the previous frame: sizes are acted on once they are stable,
    /// since they are briefly wrong while the window opens, moves to another monitor or
    /// changes its DPI.
    last: Option<(Vec2, Vec2)>,
    /// Inner size of the window when it was last neither maximized, minimized nor full screen.
    restored: Option<Vec2>,
    /// Whether the window is maximized.
    maximized: bool,
}

impl WindowFit {
    /// On the first stable frame, a window that does not fit its monitor (the default size on a
    /// small or highly scaled screen, or a size remembered from a larger monitor) gets a smaller
    /// restored size and opens maximized. After that the window is never resized: the size the
    /// user gives it is theirs, so snapping and dragging an edge are not undone.
    pub fn update(&mut self, ctx: &Context) {
        let (monitor, outer, inner, maximized, minimized, fullscreen) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.monitor_size,
                v.outer_rect,
                v.inner_rect,
                v.maximized,
                v.minimized,
                v.fullscreen,
            )
        });
        if minimized == Some(true) || fullscreen == Some(true) {
            self.last = None;
            return;
        }
        self.maximized = maximized == Some(true);
        if !self.maximized
            && let Some(inner) = inner
        {
            self.restored = Some(inner.size());
        }
        if self.started {
            return;
        }
        let (Some(monitor), Some(outer)) = (monitor, outer) else {
            return;
        };
        let reading = (monitor, outer.size());
        if self.last != Some(reading) {
            self.last = Some(reading);
            ctx.request_repaint();
            return;
        }
        self.started = true;
        if self.maximized || fits(outer.size(), monitor) {
            return;
        }
        let size = restored_size(monitor);
        // The window was centered for its old, larger size, so its corner is off screen;
        // the restored window is centered again. (It opens on the primary monitor, at 0,0.)
        let chrome = inner.map_or(Vec2::ZERO, |inner| outer.size() - inner.size());
        let position = centered(monitor, size + chrome);
        tracing::info!(
            "window {:.0}x{:.0} does not fit the {:.0}x{:.0} monitor; opening maximized, restored at {:.0}x{:.0}",
            outer.width(),
            outer.height(),
            monitor.x,
            monitor.y,
            size.x,
            size.y
        );
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(position));
        ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
        self.restored = Some(size);
    }

    /// What to remember for the next run, once the window has been seen.
    pub fn state(&self) -> Option<WindowState> {
        self.restored.map(|size| WindowState {
            inner_size_points: Some(size.max(MIN_SIZE)),
            maximized: self.maximized,
        })
    }
}

/// Whether a window of `outer` size fits a monitor of `monitor` size next to the taskbar.
fn fits(outer: Vec2, monitor: Vec2) -> bool {
    outer.x <= monitor.x && outer.y <= monitor.y - TASKBAR
}

/// Top-left corner that centers a window of `outer` size in the work area of a monitor of
/// `monitor` size, never above or left of the screen.
fn centered(monitor: Vec2, outer: Vec2) -> Pos2 {
    pos2(
        ((monitor.x - outer.x) / 2.0).max(0.0),
        ((monitor.y - TASKBAR - outer.y) / 2.0).max(0.0),
    )
}

/// Inner size of a restored window on a monitor of `monitor` size: the preferred size, or less
/// so the title bar and the taskbar still fit.
fn restored_size(monitor: Vec2) -> Vec2 {
    vec2(
        PREFERRED.x.min(monitor.x * 0.9),
        PREFERRED.y.min((monitor.y - TASKBAR) * 0.9),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui::vec2;

    use super::{
        EFRAME_WINDOW_KEY, MIN_SIZE, PREFERRED, WindowFit, centered, fits, restored_size,
        strip_window_position,
    };

    #[test]
    fn small_screens_get_a_smaller_window() {
        // 2880x1620 at 200 % = 1440x810 points; the default window is larger than that.
        assert!(!fits(vec2(1373.0, 855.5), vec2(1440.0, 810.0)));
        // Remembered from a larger monitor.
        assert!(!fits(vec2(1733.0, 1076.0), vec2(1440.0, 810.0)));
        // A 1920x1080 monitor at 100 % has room for it.
        assert!(fits(vec2(1373.0, 855.5), vec2(1920.0, 1080.0)));

        let size = restored_size(vec2(1440.0, 810.0));
        assert!(fits(size + vec2(13.0, 40.0), vec2(1440.0, 810.0)));
        // Centered on screen, the title bar reachable.
        let corner = centered(vec2(1440.0, 810.0), size + vec2(13.0, 40.0));
        assert!(corner.x > 0.0 && corner.y > 0.0);
        assert_eq!(
            centered(vec2(800.0, 600.0), vec2(1000.0, 700.0)),
            egui::pos2(0.0, 0.0)
        );
        assert_eq!(restored_size(vec2(1920.0, 1080.0)), PREFERRED);
    }

    #[test]
    fn a_large_window_that_fits_the_work_area_is_not_resized() {
        assert!(fits(vec2(1600.0, 900.0), vec2(1920.0, 1200.0)));
        assert!(fits(vec2(1360.0, 840.0), vec2(1920.0, 1080.0)));
    }

    #[test]
    fn saved_settings_have_no_position() {
        let fit = WindowFit {
            restored: Some(vec2(300.0, 200.0)),
            ..WindowFit::default()
        };
        let state = fit.state().expect("state");
        assert_eq!(state.inner_size_points, Some(MIN_SIZE));
        // eframe reads these field names; positions are absent, so it centers the window.
        let text = ron::to_string(&state).expect("ron");
        assert_eq!(
            text,
            "(inner_size_points:Some((x:900.0,y:560.0)),maximized:false)"
        );
        assert_eq!(WindowFit::default().state(), None);
    }

    #[test]
    fn old_settings_lose_their_position_only() {
        // As written with eframe's `persist_window` on a two-monitor setup (200 % and 100 %).
        let old = r#"{
    "window": "(inner_position_pixels:Some((x:2935.0,y:350.0)),outer_position_pixels:Some((x:2927.0,y:319.0)),fullscreen:false,maximized:false,inner_size_points:Some((x:1800.0,y:993.0)))",
    "egui": "(focus:None)",
}"#;
        let fixed = strip_window_position(old).expect("changed");
        let values: HashMap<String, String> = ron::from_str(&fixed).expect("ron");
        assert_eq!(
            values[EFRAME_WINDOW_KEY],
            "(inner_size_points:Some((x:1800.0,y:993.0)),maximized:false)"
        );
        assert_eq!(values["egui"], "(focus:None)");
        // Already clean, missing, or unreadable: left alone.
        assert_eq!(strip_window_position(&fixed), None);
        assert_eq!(strip_window_position(r#"{"egui": "(focus:None)"}"#), None);
        assert_eq!(strip_window_position("not ron"), None);
    }
}
