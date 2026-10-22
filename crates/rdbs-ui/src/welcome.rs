//! Welcome tab with quick actions, recent connections and shortcuts.

use egui::{Grid, RichText, Ui};
use egui_phosphor::regular as icon;

use crate::model::UiAction;
use crate::theme::color;
use crate::widgets;
use crate::workspace::TabContext;

/// Keyboard shortcuts listed on the welcome page.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+N", "New connection"),
    ("Ctrl+T", "New query tab"),
    ("Ctrl+Enter / Ctrl+R", "Run query or selection"),
    ("Ctrl+Space", "Autocomplete"),
    ("Ctrl+W", "Close tab"),
    ("Ctrl+Shift+W", "Close all tabs"),
    ("Ctrl+B", "Toggle sidebar"),
    ("Ctrl+F", "Filter explorer"),
    ("F5", "Refresh selected connection"),
];

/// Renders the welcome tab.
pub fn show(ui: &mut Ui, cx: &mut TabContext<'_>) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(icon::DATABASE)
                        .size(44.0)
                        .color(color::accent()),
                );
                ui.label(RichText::new(rdbs_core::brand::APP_NAME).size(26.0).strong());
                ui.label(
                    RichText::new(rdbs_core::brand::APP_TAGLINE)
                        .size(15.0)
                        .color(color::text()),
                );
                ui.label(
                    RichText::new(
                        "PostgreSQL · MySQL / MariaDB · SQL Server · SQLite · Oracle · MongoDB · Redis",
                    )
                    .color(color::text_dim()),
                );
                ui.label(
                    RichText::new(format!("Created by {}", rdbs_core::brand::APP_AUTHOR))
                        .small()
                        .color(color::text_faint()),
                );
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    // Center the two buttons.
                    ui.add_space((ui.available_width() - 330.0).max(0.0) / 2.0);
                    if ui
                        .add(
                            egui::Button::new(format!("{}  New Connection", icon::PLUS))
                                .min_size([160.0, 32.0].into()),
                        )
                        .clicked()
                    {
                        cx.actions.push(UiAction::NewConnection);
                    }
                    if ui
                        .add(
                            egui::Button::new(format!("{}  New Query", icon::FILE_SQL))
                                .min_size([160.0, 32.0].into()),
                        )
                        .clicked()
                    {
                        cx.actions.push(UiAction::NewQuery {
                            profile_id: cx.profiles.first().map(|p| p.id),
                            sql: None,
                            title: None,
                            run: false,
                        });
                    }
                });
            });

            ui.add_space(28.0);
            // Keep the two columns away from the tab edges on wide windows.
            let side = ((ui.available_width() - 900.0) / 2.0).clamp(24.0, 120.0); // `Margin` uses i8
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(side as i8, 0))
                .show(ui, |ui| {
                    ui.columns(2, |cols| {
                        let [left, right] = cols else { return };
                        left.vertical(|ui| {
                            ui.label(
                                RichText::new("CONNECTIONS")
                                    .size(11.0)
                                    .strong()
                                    .color(color::text_dim()),
                            );
                            ui.add_space(4.0);
                            for p in cx.profiles {
                                ui.horizontal(|ui| {
                                    widgets::kind_badge(ui, p.kind, 18.0);
                                    let link = ui.link(&p.name).on_hover_text(p.endpoint());
                                    widgets::env_pill(ui, p.environment);
                                    if link.clicked() {
                                        cx.actions.push(UiAction::Connect(p.id));
                                        cx.actions.push(UiAction::NewQuery {
                                            profile_id: Some(p.id),
                                            sql: None,
                                            title: None,
                                            run: false,
                                        });
                                    }
                                });
                            }
                        });
                        right.vertical(|ui| {
                            ui.label(
                                RichText::new("KEYBOARD")
                                    .size(11.0)
                                    .strong()
                                    .color(color::text_dim()),
                            );
                            ui.add_space(4.0);
                            Grid::new("shortcuts").spacing([16.0, 6.0]).show(ui, |ui| {
                                for (keys, what) in SHORTCUTS {
                                    ui.label(RichText::new(*keys).monospace().color(color::accent()));
                                    ui.label(*what);
                                    ui.end_row();
                                }
                            });
                        });
                    })
                });
        });
}
