//! Dark and light themes: palettes, typography, spacing and widget visuals.

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme, ThemePreference,
    Vec2,
};
use rdbs_core::sql::TokenKind;
use rdbs_core::{DatabaseKind, Environment};

/// Colours of one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window/app background behind panels.
    pub bg: Color32,
    /// Default panel fill (menu, toolbar, status bar).
    pub panel: Color32,
    /// Sidebar fill.
    pub sidebar: Color32,
    /// Editor and grid background.
    pub editor: Color32,
    /// Raised surface (inputs, inactive buttons).
    pub surface: Color32,
    /// Hovered surface.
    pub surface_hover: Color32,
    /// Pressed surface.
    pub surface_active: Color32,
    /// Hairline borders and separators.
    pub border: Color32,
    /// Primary text.
    pub text: Color32,
    /// Emphasised text (hovered widgets, focused tab).
    pub text_strong: Color32,
    /// Secondary text.
    pub text_dim: Color32,
    /// Tertiary text (placeholders, NULL).
    pub text_faint: Color32,
    /// Brand accent.
    pub accent: Color32,
    /// Accent used for selection backgrounds.
    pub selection: Color32,
    /// Success state.
    pub success: Color32,
    /// Warning state.
    pub warning: Color32,
    /// Error state.
    pub danger: Color32,
    /// Striped rows and other faint backgrounds.
    pub faint_bg: Color32,
    /// Syntax colours: keyword, string, number, comment, quoted identifier, parameter, operator.
    pub syntax: [Color32; 7],
}

/// The dark theme (default).
pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x17, 0x19, 0x1E),
    panel: Color32::from_rgb(0x1D, 0x20, 0x26),
    sidebar: Color32::from_rgb(0x1A, 0x1D, 0x22),
    editor: Color32::from_rgb(0x14, 0x16, 0x1A),
    surface: Color32::from_rgb(0x25, 0x29, 0x31),
    surface_hover: Color32::from_rgb(0x2E, 0x33, 0x3D),
    surface_active: Color32::from_rgb(0x36, 0x3C, 0x48),
    border: Color32::from_rgb(0x2C, 0x31, 0x3A),
    text: Color32::from_rgb(0xD6, 0xDA, 0xE1),
    text_strong: Color32::WHITE,
    text_dim: Color32::from_rgb(0x8A, 0x92, 0xA0),
    text_faint: Color32::from_rgb(0x5E, 0x66, 0x73),
    accent: Color32::from_rgb(0x3D, 0x8B, 0xFD),
    selection: Color32::from_rgba_premultiplied(0x1D, 0x42, 0x79, 0xB0),
    success: Color32::from_rgb(0x3F, 0xB9, 0x50),
    warning: Color32::from_rgb(0xD2, 0x99, 0x22),
    danger: Color32::from_rgb(0xF8, 0x51, 0x49),
    faint_bg: Color32::from_rgb(0x19, 0x1C, 0x21),
    syntax: [
        Color32::from_rgb(0x6F, 0xA8, 0xFF),
        Color32::from_rgb(0x9E, 0xCE, 0x6A),
        Color32::from_rgb(0xFF, 0x9E, 0x64),
        Color32::from_rgb(0x5C, 0x67, 0x73),
        Color32::from_rgb(0xE0, 0xAF, 0x68),
        Color32::from_rgb(0xC0, 0x9B, 0xF7),
        Color32::from_rgb(0x89, 0xDD, 0xFF),
    ],
};

/// The light theme.
pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xEC, 0xEE, 0xF1),
    panel: Color32::from_rgb(0xF4, 0xF5, 0xF7),
    sidebar: Color32::from_rgb(0xF0, 0xF2, 0xF4),
    editor: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    surface: Color32::from_rgb(0xE6, 0xE9, 0xEE),
    surface_hover: Color32::from_rgb(0xDB, 0xE0, 0xE7),
    surface_active: Color32::from_rgb(0xCD, 0xD4, 0xDE),
    border: Color32::from_rgb(0xD3, 0xD8, 0xDF),
    text: Color32::from_rgb(0x1F, 0x23, 0x28),
    text_strong: Color32::from_rgb(0x0B, 0x0D, 0x10),
    text_dim: Color32::from_rgb(0x57, 0x60, 0x6A),
    text_faint: Color32::from_rgb(0x8C, 0x95, 0x9F),
    accent: Color32::from_rgb(0x1F, 0x6F, 0xEB),
    selection: Color32::from_rgba_premultiplied(0x0A, 0x23, 0x4A, 0x50),
    success: Color32::from_rgb(0x1A, 0x7F, 0x37),
    warning: Color32::from_rgb(0x9A, 0x67, 0x00),
    danger: Color32::from_rgb(0xCF, 0x22, 0x2E),
    faint_bg: Color32::from_rgb(0xF6, 0xF7, 0xF9),
    syntax: [
        Color32::from_rgb(0x05, 0x50, 0xAE),
        Color32::from_rgb(0x11, 0x63, 0x29),
        Color32::from_rgb(0x95, 0x38, 0x00),
        Color32::from_rgb(0x6E, 0x77, 0x81),
        Color32::from_rgb(0x7D, 0x4E, 0x00),
        Color32::from_rgb(0x82, 0x50, 0xDF),
        Color32::from_rgb(0x09, 0x69, 0xDA),
    ],
};

/// Whether the light theme is active.
static LIGHT_MODE: AtomicBool = AtomicBool::new(false);

/// Whether the light theme is active.
pub fn is_light() -> bool {
    LIGHT_MODE.load(Ordering::Relaxed)
}

/// Palette of the active theme.
pub fn palette() -> &'static Palette {
    if is_light() { &LIGHT } else { &DARK }
}

/// Icon hues are tuned for the dark theme; on the light theme they are darkened for contrast.
pub fn icon_tint(c: Color32) -> Color32 {
    if is_light() {
        let dark = |v: u8| u8::try_from(u16::from(v) * 7 / 10).unwrap_or(v);
        Color32::from_rgb(dark(c.r()), dark(c.g()), dark(c.b()))
    } else {
        c
    }
}

/// Colours of the active theme.
pub mod color {
    use egui::Color32;

    use super::palette;

    macro_rules! colors {
        ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
            $(
                $(#[$doc])*
                pub fn $name() -> Color32 {
                    palette().$name
                }
            )*
        };
    }

    colors!(
        /// Window/app background behind panels.
        bg,
        /// Default panel fill (menu, toolbar, status bar).
        panel,
        /// Sidebar fill.
        sidebar,
        /// Editor and grid background.
        editor,
        /// Raised surface (inputs, inactive buttons).
        surface,
        /// Hovered surface.
        surface_hover,
        /// Pressed surface.
        surface_active,
        /// Hairline borders and separators.
        border,
        /// Primary text.
        text,
        /// Emphasised text.
        text_strong,
        /// Secondary text.
        text_dim,
        /// Tertiary text (placeholders, NULL).
        text_faint,
        /// Brand accent.
        accent,
        /// Accent used for selection backgrounds.
        selection,
        /// Success state.
        success,
        /// Warning state.
        warning,
        /// Error state.
        danger,
    );
}

/// Font size used by the SQL editor and data grid.
pub const MONO_SIZE: f32 = 13.0;

/// Monospace font for code and data.
pub fn mono() -> FontId {
    FontId::new(MONO_SIZE, FontFamily::Monospace)
}

/// Applies fonts, both themes' visuals and spacing to the context, then selects a theme.
pub fn apply(ctx: &egui::Context, light: bool) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    ctx.style_mut_of(Theme::Dark, |style| configure(style, &DARK, true));
    ctx.style_mut_of(Theme::Light, |style| configure(style, &LIGHT, false));
    set_light(ctx, light);
}

/// Switches between the light and the dark theme.
pub fn set_light(ctx: &egui::Context, light: bool) {
    LIGHT_MODE.store(light, Ordering::Relaxed);
    ctx.set_theme(if light {
        ThemePreference::Light
    } else {
        ThemePreference::Dark
    });
}

/// Typography, spacing and widget visuals of one theme.
fn configure(style: &mut egui::Style, p: &Palette, dark: bool) {
    {
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
        v.dark_mode = dark;
        v.panel_fill = p.panel;
        v.window_fill = p.panel;
        v.window_stroke = Stroke::new(1.0, p.border);
        v.window_corner_radius = CornerRadius::same(8);
        v.menu_corner_radius = CornerRadius::same(6);
        v.extreme_bg_color = p.editor;
        v.faint_bg_color = p.faint_bg;
        v.code_bg_color = p.editor;
        v.text_edit_bg_color = Some(p.editor);
        v.hyperlink_color = p.accent;
        v.warn_fg_color = p.warning;
        v.error_fg_color = p.danger;
        v.selection.bg_fill = p.selection;
        v.selection.stroke = Stroke::new(1.0, p.accent);
        v.striped = true;

        let radius = CornerRadius::same(4);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = p.panel;
        w.noninteractive.weak_bg_fill = p.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        w.noninteractive.corner_radius = radius;
        for (state, fill) in [
            (&mut w.inactive, p.surface),
            (&mut w.hovered, p.surface_hover),
            (&mut w.active, p.surface_active),
            (&mut w.open, p.surface_hover),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, p.text);
        w.hovered.bg_stroke = Stroke::new(1.0, p.border);
        w.hovered.fg_stroke = Stroke::new(1.5, p.text_strong);
        w.active.bg_stroke = Stroke::new(1.0, p.accent);
        w.active.fg_stroke = Stroke::new(1.5, p.text_strong);
    }
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
        Environment::Development => color::success(),
        Environment::Staging => color::warning(),
        Environment::Production => color::danger(),
    }
}

/// Syntax highlighting colour for a token kind.
pub fn token_color(kind: TokenKind) -> Color32 {
    let [
        keyword,
        string,
        number,
        comment,
        quoted,
        parameter,
        operator,
    ] = palette().syntax;
    match kind {
        TokenKind::Keyword => keyword,
        TokenKind::String => string,
        TokenKind::Number => number,
        TokenKind::Comment => comment,
        TokenKind::QuotedIdentifier => quoted,
        TokenKind::Parameter => parameter,
        TokenKind::Operator => operator,
        TokenKind::Punctuation | TokenKind::Whitespace => color::text_dim(),
        TokenKind::Identifier | TokenKind::Unknown => color::text(),
    }
}
