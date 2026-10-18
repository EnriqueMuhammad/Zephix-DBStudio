//! Dark theme: palette, typography, spacing and widget visuals.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme, ThemePreference,
    Vec2,
};
use rdbs_core::sql::TokenKind;
use rdbs_core::{DatabaseKind, Environment};

/// Application colour palette.
pub mod color {
    use egui::Color32;

    /// Window/app background behind panels.
    pub const BG: Color32 = Color32::from_rgb(0x17, 0x19, 0x1E);
    /// Default panel fill (menu, toolbar, status bar).
    pub const PANEL: Color32 = Color32::from_rgb(0x1D, 0x20, 0x26);
    /// Sidebar fill.
    pub const SIDEBAR: Color32 = Color32::from_rgb(0x1A, 0x1D, 0x22);
    /// Editor and grid background.
    pub const EDITOR: Color32 = Color32::from_rgb(0x14, 0x16, 0x1A);
    /// Raised surface (inputs, inactive buttons).
    pub const SURFACE: Color32 = Color32::from_rgb(0x25, 0x29, 0x31);
    /// Hovered surface.
    pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x2E, 0x33, 0x3D);
    /// Pressed surface.
    pub const SURFACE_ACTIVE: Color32 = Color32::from_rgb(0x36, 0x3C, 0x48);
    /// Hairline borders and separators.
    pub const BORDER: Color32 = Color32::from_rgb(0x2C, 0x31, 0x3A);
    /// Primary text.
    pub const TEXT: Color32 = Color32::from_rgb(0xD6, 0xDA, 0xE1);
    /// Secondary text.
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8A, 0x92, 0xA0);
    /// Tertiary text (placeholders, NULL).
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5E, 0x66, 0x73);
    /// Brand accent.
    pub const ACCENT: Color32 = Color32::from_rgb(0x3D, 0x8B, 0xFD);
    /// Accent used for selection backgrounds.
    pub const SELECTION: Color32 = Color32::from_rgba_premultiplied(0x1D, 0x42, 0x79, 0xB0);
    /// Success state.
    pub const SUCCESS: Color32 = Color32::from_rgb(0x3F, 0xB9, 0x50);
    /// Warning state.
    pub const WARNING: Color32 = Color32::from_rgb(0xD2, 0x99, 0x22);
    /// Error state.
    pub const DANGER: Color32 = Color32::from_rgb(0xF8, 0x51, 0x49);
}

/// Font size used by the SQL editor and data grid.
pub const MONO_SIZE: f32 = 13.0;

/// Monospace font for code and data.
pub fn mono() -> FontId {
    FontId::new(MONO_SIZE, FontFamily::Monospace)
}

/// Applies fonts, dark visuals and spacing to the context.
pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    ctx.set_theme(ThemePreference::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
        use FontFamily::{Monospace, Proportional};
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, Proportional)),
            (TextStyle::Body, FontId::new(13.5, Proportional)),
            (TextStyle::Button, FontId::new(13.5, Proportional)),
            (TextStyle::Heading, FontId::new(18.0, Proportional)),
            (TextStyle::Monospace, FontId::new(MONO_SIZE, Monospace)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(6.0, 4.0);
        s.button_padding = Vec2::new(8.0, 3.0);
        s.interact_size.y = 22.0;
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(12);
        s.indent = 14.0;
        s.combo_width = 160.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = color::PANEL;
        v.window_fill = color::PANEL;
        v.window_stroke = Stroke::new(1.0, color::BORDER);
        v.window_corner_radius = CornerRadius::same(8);
        v.menu_corner_radius = CornerRadius::same(6);
        v.extreme_bg_color = color::EDITOR;
        v.faint_bg_color = Color32::from_rgb(0x19, 0x1C, 0x21);
        v.code_bg_color = color::EDITOR;
        v.text_edit_bg_color = Some(color::EDITOR);
        v.hyperlink_color = color::ACCENT;
        v.warn_fg_color = color::WARNING;
        v.error_fg_color = color::DANGER;
        v.selection.bg_fill = color::SELECTION;
        v.selection.stroke = Stroke::new(1.0, color::ACCENT);
        v.striped = true;

        let radius = CornerRadius::same(4);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = color::PANEL;
        w.noninteractive.weak_bg_fill = color::PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, color::BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, color::TEXT);
        w.noninteractive.corner_radius = radius;
        for (state, fill) in [
            (&mut w.inactive, color::SURFACE),
            (&mut w.hovered, color::SURFACE_HOVER),
            (&mut w.active, color::SURFACE_ACTIVE),
            (&mut w.open, color::SURFACE_HOVER),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, color::TEXT);
        w.hovered.bg_stroke = Stroke::new(1.0, color::BORDER);
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.active.bg_stroke = Stroke::new(1.0, color::ACCENT);
        w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    });
}

/// Badge colour for an engine (generic hues, not vendor branding).
pub fn kind_color(kind: DatabaseKind) -> Color32 {
    match kind {
        DatabaseKind::PostgreSql => Color32::from_rgb(0x4F, 0x8E, 0xF7),
        DatabaseKind::MySql => Color32::from_rgb(0xE0, 0x8A, 0x1E),
        DatabaseKind::SqlServer => Color32::from_rgb(0xD0, 0x45, 0x4C),
        DatabaseKind::MariaDb => Color32::from_rgb(0xB0, 0x7A, 0x5B),
        DatabaseKind::Sqlite => Color32::from_rgb(0x4F, 0xB3, 0xBF),
        DatabaseKind::Oracle => Color32::from_rgb(0xC7, 0x4B, 0x2E),
        DatabaseKind::MongoDb => Color32::from_rgb(0x4C, 0xAF, 0x50),
        DatabaseKind::Redis => Color32::from_rgb(0xD8, 0x3A, 0x34),
    }
}

/// Marker colour for an environment.
pub fn env_color(env: Environment) -> Color32 {
    match env {
        Environment::Development => color::SUCCESS,
        Environment::Staging => color::WARNING,
        Environment::Production => color::DANGER,
    }
}

/// Syntax highlighting colour for a token kind.
pub fn token_color(kind: TokenKind) -> Color32 {
    match kind {
        TokenKind::Keyword => Color32::from_rgb(0x6F, 0xA8, 0xFF),
        TokenKind::String => Color32::from_rgb(0x9E, 0xCE, 0x6A),
        TokenKind::Number => Color32::from_rgb(0xFF, 0x9E, 0x64),
        TokenKind::Comment => Color32::from_rgb(0x5C, 0x67, 0x73),
        TokenKind::QuotedIdentifier => Color32::from_rgb(0xE0, 0xAF, 0x68),
        TokenKind::Parameter => Color32::from_rgb(0xC0, 0x9B, 0xF7),
        TokenKind::Operator => Color32::from_rgb(0x89, 0xDD, 0xFF),
        TokenKind::Punctuation | TokenKind::Whitespace => color::TEXT_DIM,
        TokenKind::Identifier | TokenKind::Unknown => color::TEXT,
    }
}
