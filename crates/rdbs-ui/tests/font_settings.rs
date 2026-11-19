//! Headless test of Tools > Fonts in the real app (demo mode). Its own test binary: font
//! sizes are process-wide, so other UI tests must not run next to it.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use rdbs_core::MemoryCredentials;
use rdbs_ui::{AppConfig, StudioApp};

struct TestStorage(HashMap<String, String>);

impl eframe::Storage for TestStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

fn demo_app() -> Harness<'static, StudioApp> {
    let storage: &'static TestStorage = Box::leak(Box::new(TestStorage(HashMap::from([(
        "rdbs.update.check_on_start".to_owned(),
        "false".to_owned(),
    )]))));
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_eframe(move |cc| {
            cc.storage = Some(storage);
            StudioApp::new(
                cc,
                AppConfig {
                    factory: Arc::new(rdbs_adapter_mock::MockAdapterFactory::instant()),
                    credentials: Arc::new(MemoryCredentials::default()),
                    demo_profiles: Some(rdbs_adapter_mock::sample_profiles()),
                    system_fonts: false,
                },
            )
            .expect("demo app starts")
        });
    harness.run_steps(4);
    harness
}

fn open_fonts(harness: &mut Harness<'_, StudioApp>) {
    harness.get_by_label("Tools").click();
    harness.run_steps(3);
    harness.get_by_label_contains("Fonts…").click();
    harness.run_steps(4);
}

fn menu_height(harness: &Harness<'_, StudioApp>) -> f32 {
    harness.get_by_label("Help").rect().height()
}

#[test]
fn interface_size_applies_on_ok_and_cancel_restores() {
    let mut harness = demo_app();
    let before = menu_height(&harness);

    open_fonts(&mut harness);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
    for label in [
        "Fonts",
        "Show settings for:",
        "Size (pt)",
        "Sample",
        "Cancel",
    ] {
        let rect = harness.get_by_label(label).rect();
        assert!(
            window.contains_rect(rect),
            "{label:?} at {rect:?} is outside"
        );
    }
    // The dialog opens on the editor; switch to the interface and pick 14 pt.
    harness
        .get_all_by_role(egui::accesskit::Role::ComboBox)
        .last()
        .expect("item picker")
        .click();
    harness.run_steps(3);
    harness
        .get_by_label("Environment (menus, dialogs, explorer)")
        .click();
    harness.run_steps(3);
    harness.get_by_label("14").click();
    harness.run_steps(4);
    let previewed = menu_height(&harness);
    assert!(
        previewed > before + 3.0,
        "live preview: {before} -> {previewed}"
    );

    // Cancel restores the size the dialog opened with.
    harness.get_by_label("Cancel").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Show settings for:").is_none());
    assert!((menu_height(&harness) - before).abs() < 0.5);

    // OK keeps it.
    open_fonts(&mut harness);
    harness
        .get_all_by_role(egui::accesskit::Role::ComboBox)
        .last()
        .expect("item picker")
        .click();
    harness.run_steps(3);
    harness
        .get_by_label("Environment (menus, dialogs, explorer)")
        .click();
    harness.run_steps(3);
    harness.get_by_label("12").click();
    harness.run_steps(3);
    harness.get_by_label_contains("OK").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Show settings for:").is_none());
    let after = menu_height(&harness);
    assert!(
        after > before + 1.0,
        "12 pt is larger than 10 pt: {before} -> {after}"
    );
}
