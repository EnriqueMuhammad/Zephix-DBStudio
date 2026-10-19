//! Execution plan viewer: an indented operator tree with relative cost bars, estimated vs.
//! actual rows, and the raw plan text.

use egui::{Align, Layout, RichText, ScrollArea, Sense, Ui, Vec2};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use rdbs_core::plan::{ExplainPlan, PlanNode};

use crate::theme::{self, color};
use crate::widgets::format_count;

/// View state of a plan panel.
#[derive(Debug, Default, Clone)]
pub struct PlanViewState {
    /// Show the server's raw output instead of the tree.
    pub raw: bool,
    /// Selected row (index into the flattened tree).
    pub selected: Option<usize>,
}

struct Row<'a> {
    depth: usize,
    node: &'a PlanNode,
}

fn flatten<'a>(nodes: &'a [PlanNode], depth: usize, out: &mut Vec<Row<'a>>) {
    for node in nodes {
        out.push(Row { depth, node });
        flatten(&node.children, depth + 1, out);
    }
}

fn number(value: Option<f64>) -> String {
    match value {
        None => String::new(),
        Some(v) if v.fract() == 0.0 && v.abs() < 1e15 => format_count(v as u64),
        Some(v) if v >= 100.0 => format!("{v:.0}"),
        Some(v) => format!("{v:.2}"),
    }
}

/// Estimates off by 10× or more are highlighted: they usually explain a bad plan.
fn misestimated(node: &PlanNode) -> bool {
    match (node.rows, node.actual_rows) {
        (Some(est), Some(act)) => {
            let (lo, hi) = if est < act { (est, act) } else { (act, est) };
            hi >= 10.0 * lo.max(1.0)
        }
        _ => false,
    }
}

/// Renders a plan.
pub fn show(ui: &mut Ui, plan: &ExplainPlan, state: &mut PlanViewState) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let label = if plan.analyzed {
            format!("{}  Actual plan", icon::LIGHTNING)
        } else {
            format!("{}  Estimated plan", icon::TREE_STRUCTURE)
        };
        ui.label(RichText::new(label).strong());
        for note in &plan.notes {
            ui.label(RichText::new(format!("·  {note}")).color(color::text_dim()));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(8.0);
            if ui.selectable_label(state.raw, "Raw").clicked() {
                state.raw = true;
            }
            if ui.selectable_label(!state.raw, "Tree").clicked() {
                state.raw = false;
            }
            if ui
                .button(format!("{}  Copy", icon::COPY))
                .on_hover_text("Copy the raw plan")
                .clicked()
            {
                ui.ctx().copy_text(plan.raw.clone());
            }
        });
    });
    ui.separator();

    if state.raw {
        let mut text = plan.raw.as_str();
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .font(theme::mono())
                    .desired_width(f32::INFINITY)
                    .frame(egui::Frame::NONE),
            );
        });
        return;
    }

    let mut rows = Vec::new();
    flatten(&plan.roots, 0, &mut rows);
    let max_cost = plan.max_cost();
    let has_actual = rows.iter().any(|r| r.node.actual_rows.is_some());
    let details_height = if state.selected.is_some() { 110.0 } else { 0.0 };
    let table_height = (ui.available_height() - details_height).max(60.0);

    ui.allocate_ui(Vec2::new(ui.available_width(), table_height), |ui| {
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(320.0).at_least(160.0).clip(true))
            .column(Column::initial(200.0).at_least(80.0).clip(true))
            .column(Column::initial(150.0).at_least(80.0))
            .column(Column::initial(90.0).at_least(60.0));
        if has_actual {
            builder = builder
                .column(Column::initial(90.0).at_least(60.0))
                .column(Column::initial(90.0).at_least(60.0));
        }
        builder
            .column(Column::remainder())
            .header(22.0, |mut header| {
                let mut head = |text: &str| {
                    header.col(|ui| {
                        ui.label(RichText::new(text).strong().color(color::text_dim()));
                    });
                };
                head("Operation");
                head("Object");
                head("Cost");
                head("Est. rows");
                if has_actual {
                    head("Actual rows");
                    head("Time (ms)");
                }
                head("");
            })
            .body(|body| {
                body.rows(22.0, rows.len(), |mut row| {
                    let i = row.index();
                    let Row { depth, node } = &rows[i];
                    row.set_selected(state.selected == Some(i));
                    let warn = misestimated(node);
                    row.col(|ui| {
                        ui.add_space(*depth as f32 * 16.0);
                        let glyph = if node.children.is_empty() {
                            icon::DOT_OUTLINE
                        } else {
                            icon::CARET_DOWN
                        };
                        ui.label(RichText::new(glyph).color(color::text_faint()));
                        let response = ui.label(&node.operation);
                        if !node.details.is_empty() {
                            response.on_hover_ui(|ui| {
                                for (k, v) in &node.details {
                                    ui.label(format!("{k}: {v}"));
                                }
                            });
                        }
                    });
                    row.col(|ui| {
                        if let Some(object) = &node.object {
                            ui.label(RichText::new(object).color(color::accent()));
                        }
                    });
                    row.col(|ui| {
                        if let Some(cost) = node.cost {
                            let fraction = if max_cost > 0.0 {
                                (cost / max_cost) as f32
                            } else {
                                0.0
                            };
                            cost_bar(ui, fraction, &number(Some(cost)));
                        }
                    });
                    row.col(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(number(node.rows));
                        });
                    });
                    if has_actual {
                        row.col(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let text = RichText::new(number(node.actual_rows));
                                let response = ui.label(if warn {
                                    text.color(color::warning())
                                } else {
                                    text
                                });
                                if warn {
                                    response.on_hover_text(
                                        "Estimated and actual rows differ by 10× or more; \
                                         statistics may be stale.",
                                    );
                                }
                            });
                        });
                        row.col(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(number(node.actual_ms));
                            });
                        });
                    }
                    row.col(|_| {});
                    if row.response().clicked() {
                        state.selected = (state.selected != Some(i)).then_some(i);
                    }
                });
            });
    });

    if let Some(node) = state.selected.and_then(|i| rows.get(i)).map(|r| r.node) {
        ui.separator();
        ScrollArea::vertical()
            .id_salt("plan-details")
            .max_height(details_height - 10.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&node.operation).strong());
                    if let Some(o) = &node.object {
                        ui.label(RichText::new(o).color(color::accent()));
                    }
                });
                if node.details.is_empty() {
                    ui.label(RichText::new("No further properties").color(color::text_faint()));
                }
                for (k, v) in &node.details {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("{k}:")).color(color::text_dim()));
                        ui.add(
                            egui::Label::new(RichText::new(v).font(theme::mono())).selectable(true),
                        );
                    });
                }
            });
    }
}

fn cost_bar(ui: &mut Ui, fraction: f32, label: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(60.0, 10.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, color::surface());
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    let tint = if fraction > 0.66 {
        color::danger()
    } else if fraction > 0.33 {
        color::warning()
    } else {
        color::accent()
    };
    painter.rect_filled(fill, 2.0, tint.gamma_multiply(0.8));
    ui.label(RichText::new(label).size(12.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_large_misestimates_only() {
        let node = |rows, actual| PlanNode {
            rows: Some(rows),
            actual_rows: Some(actual),
            ..PlanNode::default()
        };
        assert!(misestimated(&node(1.0, 50.0)));
        assert!(misestimated(&node(1000.0, 10.0)));
        assert!(!misestimated(&node(10.0, 30.0)));
        assert!(!misestimated(&node(0.0, 5.0)));
    }

    #[test]
    fn numbers_are_compact() {
        assert_eq!(number(Some(1234.0)), "1,234");
        assert_eq!(number(Some(0.25)), "0.25");
        assert_eq!(number(Some(123.456)), "123");
        assert_eq!(number(None), "");
    }
}
