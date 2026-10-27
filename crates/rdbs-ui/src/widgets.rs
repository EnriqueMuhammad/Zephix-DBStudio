//! Small reusable widgets and formatting helpers.

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Response, RichText, Sense, Stroke, Ui, Vec2, Widget,
};
use rdbs_core::sql::{self, TokenKind};
use rdbs_core::{DatabaseKind, Environment};

use crate::theme::{self, color};

/// Paints the two-letter engine badge and returns its response.
pub fn kind_badge(ui: &mut Ui, kind: DatabaseKind, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_kind_badge(ui, rect, kind);
    response
}

/// Paints an engine badge into `rect`.
pub fn paint_kind_badge(ui: &Ui, rect: egui::Rect, kind: DatabaseKind) {
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), theme::kind_color(kind));
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        kind.badge(),
        FontId::proportional(rect.height() * 0.48),
        Color32::WHITE,
    );
}

/// Small coloured environment label, e.g. "PROD". Nothing is shown for Development.
pub fn env_pill(ui: &mut Ui, env: Environment) {
    let text = match env {
        Environment::Development => "DEV",
        Environment::Staging => "STAGING",
        Environment::Production => "PROD",
    };
    let c = theme::env_color(env);
    egui::Frame::new()
        .fill(c.gamma_multiply(0.18))
        .stroke(Stroke::new(1.0, c.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(egui::Margin::symmetric(4, 0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.0).strong().color(c));
        });
}

/// A ribbon-style toolbar button: icon above a short label.
pub struct ToolbarButton<'a> {
    icon: &'a str,
    label: &'a str,
    enabled: bool,
}

impl<'a> ToolbarButton<'a> {
    /// Creates an enabled button.
    pub fn new(icon: &'a str, label: &'a str) -> Self {
        Self {
            icon,
            label,
            enabled: true,
        }
    }

    /// Sets whether the button reacts to clicks.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

impl Widget for ToolbarButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = Vec2::new(64.0, 46.0);
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(size, sense);
        let painter = ui.painter();
        if self.enabled && response.hovered() {
            let fill = if response.is_pointer_button_down_on() {
                color::surface_active()
            } else {
                color::surface_hover()
            };
            painter.rect_filled(rect, CornerRadius::same(5), fill);
        }
        let fg = if self.enabled {
            color::text()
        } else {
            color::text_faint()
        };
        let icon_color = if self.enabled { color::accent() } else { fg };
        painter.text(
            rect.center_top() + Vec2::new(0.0, 15.0),
            Align2::CENTER_CENTER,
            self.icon,
            FontId::proportional(20.0),
            icon_color,
        );
        painter.text(
            rect.center_bottom() - Vec2::new(0.0, 10.0),
            Align2::CENTER_CENTER,
            self.label,
            FontId::proportional(11.5),
            fg,
        );
        response
    }
}

/// Formats an integer with thousands separators: `1234567` → `1,234,567`.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Formats a duration compactly: `850 ms`, `1.25 s`, `2m 03s`.
pub fn format_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.2} s", d.as_secs_f64())
    } else {
        let secs = d.as_secs();
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// Builds a syntax-highlighted layout job for SQL text.
pub fn highlight_sql(text: &str, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    for token in sql::tokenize(text) {
        let mut format = TextFormat::simple(font.clone(), theme::token_color(token.kind));
        format.italics = token.kind == TokenKind::Comment;
        job.append(&text[token.range], 0.0, format);
    }
    job
}

/// Converts a char-index range (as reported by `TextEdit`) into a byte range of `text`.
pub fn char_range_to_bytes(text: &str, start: usize, end: usize) -> std::ops::Range<usize> {
    let byte_at = |char_idx: usize| {
        text.char_indices()
            .nth(char_idx)
            .map_or(text.len(), |(b, _)| b)
    };
    byte_at(start)..byte_at(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn durations_are_compact() {
        use std::time::Duration;
        assert_eq!(format_duration(Duration::from_millis(42)), "42 ms");
        assert_eq!(format_duration(Duration::from_millis(1_250)), "1.25 s");
        assert_eq!(format_duration(Duration::from_secs(123)), "2m 03s");
    }

    #[test]
    fn highlighting_preserves_text() {
        let sql = "SELECT 'x' -- c\nFROM t";
        assert_eq!(highlight_sql(sql, &theme::mono()).text, sql);
    }

    #[test]
    fn char_ranges_map_to_utf8_bytes() {
        let text = "héllo wörld";
        let r = char_range_to_bytes(text, 6, 11);
        assert_eq!(&text[r], "wörld");
        assert_eq!(char_range_to_bytes(text, 0, 99), 0..text.len());
    }
}
