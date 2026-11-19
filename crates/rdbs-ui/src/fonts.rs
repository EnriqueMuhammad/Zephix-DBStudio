//! Font settings: installed font families and the Tools > Fonts dialog.
//!
//! The installed fonts are listed on a background thread at start-up (reading every font
//! file's name table takes a moment). Until then the built-in fonts are shown; the chosen
//! families are applied as soon as the list is ready.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Id, RichText, Ui};
use egui_phosphor::regular as icon;

use crate::theme::{self, FONT_PT_RANGE, FontChoice, FontSettings, FontTarget, color};

/// Persisted [`FontSettings`].
pub(crate) const FONTS_KEY: &str = "rdbs.fonts";

/// Sizes offered in the size list (any size in [`FONT_PT_RANGE`] can be typed).
const SIZES: [f32; 14] = [
    7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 28.0,
];

/// An installed font family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Family {
    pub name: String,
    /// Every face of the family is fixed-width.
    pub monospaced: bool,
}

/// The fonts installed on this computer.
pub(crate) struct SystemFonts {
    db: fontdb::Database,
    families: Vec<Family>,
}

impl SystemFonts {
    /// Reads the OS font folders (slow: call off the UI thread).
    pub fn scan() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut by_name: std::collections::BTreeMap<String, bool> = Default::default();
        for face in db.faces() {
            let Some((name, _)) = face.families.first() else {
                continue;
            };
            // Skip vertical (CJK "@") and hidden (".") families.
            if name.starts_with('@') || name.starts_with('.') || name.trim().is_empty() {
                continue;
            }
            let entry = by_name.entry(name.clone()).or_insert(true);
            *entry &= face.monospaced;
        }
        let mut families: Vec<Family> = by_name
            .into_iter()
            .map(|(name, monospaced)| Family { name, monospaced })
            .collect();
        families.sort_by_key(|f| f.name.to_lowercase());
        Self { db, families }
    }

    /// Installed families, sorted by name.
    pub fn families(&self) -> &[Family] {
        &self.families
    }

    fn has(&self, name: &str) -> bool {
        self.families.iter().any(|f| f.name == name)
    }

    /// The regular face of `family`: its file contents and face index.
    pub fn load(&self, family: &str) -> Option<(Vec<u8>, u32)> {
        let id = self.db.query(&fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        })?;
        self.db
            .with_face_data(id, |data, index| (data.to_vec(), index))
    }
}

struct Dialog {
    /// Settings when the dialog opened (Cancel goes back to them).
    original: FontSettings,
    target: FontTarget,
    filter: String,
    fixed_only: bool,
    /// Scroll the family list to the chosen family (when the dialog opens or the item
    /// shown changes).
    reveal: bool,
}

/// Fonts in effect, the installed families and the dialog.
pub(crate) struct FontState {
    pub settings: FontSettings,
    system: Option<Arc<SystemFonts>>,
    scanning: Option<Receiver<SystemFonts>>,
    dialog: Option<Dialog>,
}

impl FontState {
    /// `scan` lists the installed fonts on a background thread (off in tests, which keep
    /// the built-in fonts so layouts do not depend on the machine).
    pub fn new(settings: FontSettings, scan: bool) -> Self {
        let scanning = scan.then(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("font-scan".into())
                .spawn(move || {
                    let _ = tx.send(SystemFonts::scan());
                });
            if let Err(e) = spawned {
                tracing::warn!("could not list the installed fonts: {e}");
            }
            rx
        });
        Self {
            settings: settings.sanitized(),
            system: None,
            scanning,
            dialog: None,
        }
    }

    /// Picks up the installed fonts once they are listed and applies the chosen families.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.scanning else {
            return;
        };
        match rx.try_recv() {
            Ok(fonts) => {
                tracing::debug!("{} font families installed", fonts.families.len());
                self.system = Some(Arc::new(fonts));
                self.scanning = None;
                self.apply(ctx);
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Err(TryRecvError::Disconnected) => self.scanning = None,
        }
    }

    /// Applies [`Self::settings`]: families (when the installed fonts are known) and sizes.
    pub fn apply(&self, ctx: &egui::Context) {
        let system = self.system.clone();
        ctx.set_fonts(theme::font_definitions(&self.settings, |family| {
            system.as_ref()?.load(family)
        }));
        theme::apply_font_sizes(ctx, &self.settings);
    }

    /// The dialog is open (it owns the keyboard).
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Opens Tools > Fonts.
    pub fn open_dialog(&mut self) {
        self.dialog = Some(Dialog {
            original: self.settings.clone(),
            target: FontTarget::Editor,
            filter: String::new(),
            fixed_only: false,
            reveal: true,
        });
    }

    /// The font dialog. Changes show at once; Cancel restores the settings it opened with.
    pub fn dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut changed = false;
        let mut ok = false;
        let mut cancel = false;
        let response = egui::Modal::new(Id::new("font-settings")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Fonts");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Show settings for:");
                egui::ComboBox::from_id_salt("font-target")
                    .width(300.0)
                    .selected_text(dialog.target.label())
                    .show_ui(ui, |ui| {
                        for t in FontTarget::ALL {
                            if ui
                                .selectable_value(&mut dialog.target, t, t.label())
                                .clicked()
                            {
                                dialog.reveal = true;
                            }
                        }
                    });
            });
            ui.add_space(6.0);
            let choice = self.settings.get_mut(dialog.target);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(320.0);
                    changed |= family_list(
                        ui,
                        dialog,
                        choice,
                        self.system.as_deref(),
                        self.scanning.is_some(),
                    );
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("Size (pt)").strong());
                    changed |= size_list(ui, choice);
                });
            });
            ui.add_space(8.0);
            ui.label(RichText::new("Sample").strong());
            sample(ui, dialog.target);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Use Defaults")
                    .on_hover_text("Default font and size for the item shown")
                    .clicked()
                {
                    *self.settings.get_mut(dialog.target) =
                        FontSettings::default().get(dialog.target).clone();
                    changed = true;
                }
                if ui
                    .button("Reset All")
                    .on_hover_text("Default fonts and sizes everywhere")
                    .clicked()
                {
                    self.settings = FontSettings::default();
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel = ui.button("Cancel").clicked();
                    ok = ui.button(format!("{}  OK", icon::CHECK)).clicked();
                });
            });
        });
        let cancelled = cancel || (!ok && response.should_close());
        if cancelled {
            self.settings = dialog.original.clone();
            changed = true;
        }
        if ok || cancelled {
            self.dialog = None;
        }
        if changed {
            self.settings = self.settings.clone().sanitized();
            self.apply(ctx);
        }
    }
}

/// The family list with a filter. Fixed-width families are bold. Returns whether the
/// choice changed.
fn family_list(
    ui: &mut Ui,
    dialog: &mut Dialog,
    choice: &mut FontChoice,
    system: Option<&SystemFonts>,
    scanning: bool,
) -> bool {
    let mut changed = false;
    ui.label(RichText::new("Font (fixed-width fonts are bold)").strong());
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut dialog.filter)
                .hint_text(format!("{}  Filter", icon::MAGNIFYING_GLASS))
                .desired_width(180.0),
        );
        ui.checkbox(&mut dialog.fixed_only, "Fixed-width only");
    });
    if let Some(name) = &choice.family
        && system.is_some_and(|s| !s.has(name))
    {
        ui.label(
            RichText::new(format!(
                "\"{name}\" is not installed; the built-in font is used."
            ))
            .small()
            .color(color::warning()),
        );
    }
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, color::border()))
        .corner_radius(4)
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            ui.set_width(310.0);
            egui::ScrollArea::vertical()
                .id_salt("font-families")
                .max_height(220.0)
                .min_scrolled_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if ui
                        .selectable_label(choice.family.is_none(), "Built-in font")
                        .clicked()
                    {
                        choice.family = None;
                        changed = true;
                    }
                    let Some(system) = system else {
                        ui.horizontal(|ui| {
                            if scanning {
                                ui.add(egui::Spinner::new().size(12.0));
                                ui.label("Reading installed fonts…");
                            } else {
                                ui.label("Installed fonts are not available.");
                            }
                        });
                        return;
                    };
                    let filter = dialog.filter.trim().to_lowercase();

                    for family in system.families() {
                        if dialog.fixed_only && !family.monospaced {
                            continue;
                        }
                        if !filter.is_empty() && !family.name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        let selected = choice.family.as_deref() == Some(family.name.as_str());
                        let mut text = RichText::new(&family.name);
                        if family.monospaced {
                            text = text.strong();
                        }
                        let r = ui.selectable_label(selected, text);
                        if selected && dialog.reveal {
                            r.scroll_to_me(Some(egui::Align::Center));
                        }
                        if r.clicked() && !selected {
                            choice.family = Some(family.name.clone());
                            changed = true;
                        }
                    }
                    dialog.reveal = false;
                });
        });
    changed
}

/// The size list plus a field for any size. Returns whether the size changed.
fn size_list(ui: &mut Ui, choice: &mut FontChoice) -> bool {
    let before = choice.size_pt;
    ui.add(
        egui::DragValue::new(&mut choice.size_pt)
            .range(FONT_PT_RANGE)
            .speed(0.25)
            .max_decimals(1)
            .suffix(" pt"),
    );
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, color::border()))
        .corner_radius(4)
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            ui.set_width(90.0);
            for size in SIZES {
                let selected = (choice.size_pt - size).abs() < 0.01;
                if ui.selectable_label(selected, format!("{size}")).clicked() {
                    choice.size_pt = size;
                }
            }
        });
    (choice.size_pt - before).abs() > f32::EPSILON
}

/// Text in the font of `target`.
fn sample(ui: &mut Ui, target: FontTarget) {
    let font = match target {
        FontTarget::Interface => theme::ui_font(13.5),
        FontTarget::Editor => theme::mono(),
        FontTarget::Grid => theme::grid_font(),
    };
    egui::Frame::new()
        .fill(color::editor())
        .stroke(egui::Stroke::new(1.0, color::border()))
        .corner_radius(4)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let lines = match target {
                FontTarget::Editor => {
                    "SELECT c.id, c.full_name, SUM(o.total) AS total\nFROM customers AS c -- 0O 1lI\nWHERE c.created_at >= '2026-01-01';"
                }
                _ => "AaBbCcXxYyZz 0123456789\nConnections · Tables · Results (1,000 rows)",
            };
            ui.label(RichText::new(lines).font(font).color(color::text()));
        });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn definitions_put_chosen_families_first_and_skip_missing_ones() {
        let settings = FontSettings {
            interface: FontChoice {
                family: Some("UiFace".into()),
                size_pt: 10.0,
            },
            editor: FontChoice {
                family: Some("Missing".into()),
                size_pt: 10.0,
            },
            grid: FontChoice {
                family: Some("GridFace".into()),
                size_pt: 9.0,
            },
        };
        // Any bytes do for the definitions; egui parses them only when it lays out text.
        let defs =
            theme::font_definitions(&settings, |f| (f != "Missing").then(|| (vec![0u8; 4], 0)));
        let prop = &defs.families[&egui::FontFamily::Proportional];
        assert_eq!(prop[0], "system:UiFace");
        let grid = &defs.families[&egui::FontFamily::Name(theme::GRID_FAMILY.into())];
        assert_eq!(grid[0], "system:GridFace");
        assert!(
            grid.contains(&"system:UiFace".to_owned()),
            "falls back to the UI font"
        );
        let mono = &defs.families[&egui::FontFamily::Monospace];
        assert!(!mono.iter().any(|k| k.contains("Missing")));
        // Built-in fonts and icons stay as fallbacks.
        assert!(prop.len() > 2);
    }

    #[test]
    fn sanitize_clamps_sizes_and_empty_names() {
        let mut s = FontSettings::default();
        s.editor.size_pt = 400.0;
        s.grid.size_pt = f32::NAN;
        s.interface.family = Some("  ".into());
        let s = s.sanitized();
        assert_eq!(s.editor.size_pt, *FONT_PT_RANGE.end());
        assert!((s.grid.size_pt - 10.0).abs() < f32::EPSILON);
        assert_eq!(s.interface.family, None);
    }

    /// Reads this machine's fonts; on Windows the default families must be found and
    /// loadable, and egui must accept them.
    #[test]
    fn installed_fonts_are_listed_and_loaded() {
        let fonts = SystemFonts::scan();
        if !cfg!(windows) {
            return;
        }
        for family in ["Segoe UI", "Consolas"] {
            assert!(fonts.has(family), "{family} not listed");
            let (bytes, _) = fonts.load(family).unwrap();
            assert!(bytes.len() > 10_000);
        }
        assert!(
            fonts
                .families()
                .iter()
                .any(|f| f.name == "Consolas" && f.monospaced)
        );
        assert!(
            !fonts
                .families()
                .iter()
                .any(|f| f.name == "Segoe UI" && f.monospaced)
        );
        let ctx = egui::Context::default();
        ctx.set_fonts(theme::font_definitions(&FontSettings::default(), |f| {
            fonts.load(f)
        }));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label(RichText::new("SELECT 1").font(theme::mono()));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn points_convert_at_96_dpi() {
        assert!((theme::pt_to_px(9.0) - 12.0).abs() < 0.001);
        assert!((theme::pt_to_px(10.0) - 13.333).abs() < 0.001);
    }
}
