//! Keeps the window usable on small or high-DPI screens.

use egui::{Context, Vec2, ViewportCommand, vec2};

/// Room the taskbar takes at the edge of the monitor, in points.
const TASKBAR: f32 = 48.0;
/// Size of a restored window when the monitor has room for it, in points.
const PREFERRED: Vec2 = vec2(1360.0, 840.0);

/// Keeps the window on its monitor; call it every frame.
#[derive(Debug, Default)]
pub struct WindowFit {
    /// The size at start-up was checked.
    started: bool,
    /// Monitor and window size of the previous frame: sizes are acted on once they are stable,
    /// since they are briefly wrong while the window opens, moves to another monitor or
    /// changes its DPI.
    last: Option<(Vec2, Vec2)>,
}

impl WindowFit {
    /// A window larger than its monitor (the default size on a small or highly scaled screen,
    /// or a size remembered from a larger monitor) gets a restored size that fits. At start-up
    /// it also opens maximized; later, e.g. after the restore button, it only shrinks.
    pub fn update(&mut self, ctx: &Context) {
        let (monitor, outer, maximized, minimized, fullscreen) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.monitor_size,
                v.outer_rect,
                v.maximized,
                v.minimized,
                v.fullscreen,
            )
        });
        let (Some(monitor), Some(outer)) = (monitor, outer) else {
            return;
        };
        if minimized == Some(true) || fullscreen == Some(true) {
            self.last = None;
            return;
        }
        let reading = (monitor, outer.size());
        if self.last != Some(reading) {
            self.last = Some(reading);
            ctx.request_repaint();
            return;
        }
        if maximized == Some(true) || fits(outer.size(), monitor) {
            self.started = true;
            return;
        }
        let size = restored_size(monitor);
        tracing::info!(
            "window {:.0}x{:.0} does not fit the {:.0}x{:.0} monitor; resizing to {:.0}x{:.0}",
            outer.width(),
            outer.height(),
            monitor.x,
            monitor.y,
            size.x,
            size.y
        );
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        if !self.started {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
        }
        self.started = true;
        self.last = None;
    }
}

/// Whether a window of `outer` size fits a monitor of `monitor` size next to the taskbar.
fn fits(outer: Vec2, monitor: Vec2) -> bool {
    outer.x <= monitor.x && outer.y <= monitor.y - TASKBAR
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
    use egui::vec2;

    use super::{fits, restored_size};

    #[test]
    fn small_screens_get_a_smaller_window() {
        // 2880x1620 at 200 % = 1440x810 points; the default window is larger than that.
        assert!(!fits(vec2(1293.0, 815.5), vec2(1440.0, 810.0)));
        // Remembered from a larger monitor.
        assert!(!fits(vec2(1733.0, 1076.0), vec2(1440.0, 810.0)));
        // A 1920x1080 monitor at 100 % has room for it.
        assert!(fits(vec2(1373.0, 855.5), vec2(1920.0, 1080.0)));

        let size = restored_size(vec2(1440.0, 810.0));
        assert!(fits(size + vec2(13.0, 40.0), vec2(1440.0, 810.0)));
        assert_eq!(restored_size(vec2(1920.0, 1080.0)), vec2(1360.0, 840.0));
    }
}
