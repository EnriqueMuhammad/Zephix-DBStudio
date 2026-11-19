//! Headless test of find and replace in the query editor of the real app (demo mode).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable, by};
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

fn editor_text(harness: &Harness<'_, StudioApp>) -> String {
    harness
        .get_by_role(Role::MultilineTextInput)
        .accesskit_node()
        .value()
        .unwrap_or_default()
}

/// A text field of the find bar: by its hint while empty, else by its text.
fn field<'h>(
    harness: &'h Harness<'_, StudioApp>,
    hint: &'static str,
    text: &'static str,
) -> egui_kittest::Node<'h> {
    harness.get(by().role(Role::TextInput).predicate(move |n| {
        let value = n.value().unwrap_or_default();
        if text.is_empty() {
            value.is_empty() && n.placeholder() == Some(hint)
        } else {
            value == text
        }
    }))
}

fn type_text(harness: &mut Harness<'_, StudioApp>, text: &str) {
    harness.event(egui::Event::Text(text.to_owned()));
    harness.run_steps(3);
}

#[test]
fn ctrl_f_finds_steps_and_replaces_in_the_script() {
    let mut harness = demo_app();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::T);
    harness.run_steps(4);
    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run_steps(2);
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text("select a from t;\nSELECT b FROM t2;\nselect selected;");
    harness.run_steps(3);

    // Ctrl+F opens the bar in the query tab (not the explorer filter) and searches as you type.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    harness.run_steps(3);
    assert!(field(&harness, "Find", "").accesskit_node().is_focused());
    type_text(&mut harness, "select");
    harness.get_by_label("1 of 4");

    // Enter and F3 go to the next match, Shift+F3 back.
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    harness.get_by_label("2 of 4");
    harness.key_press(Key::F3);
    harness.run_steps(3);
    harness.get_by_label("3 of 4");
    harness.key_press_modifiers(Modifiers::SHIFT, Key::F3);
    harness.run_steps(3);
    harness.get_by_label("2 of 4");

    // Whole word leaves out "selected"; match case leaves out "SELECT".
    harness.get_by_label("ab").click();
    harness.run_steps(3);
    harness.get_by_label("1 of 3");
    harness.get_by_label("Aa").click();
    harness.run_steps(3);
    harness.get_by_label("1 of 2");

    // Ctrl+H shows the replace row; Replace All changes every match as one step.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::H);
    harness.run_steps(3);
    field(&harness, "Replace", "").focus();
    harness.run_steps(2);
    type_text(&mut harness, "SELECT");
    harness.get_by_label("Replace All").click();
    harness.run_steps(3);
    harness.get_by_label("2 replaced");
    assert_eq!(
        editor_text(&harness),
        "SELECT a from t;\nSELECT b FROM t2;\nSELECT selected;"
    );

    // A regular expression with a group.
    harness.get_by_label("Aa").click();
    harness.get_by_label(".*").click();
    harness.run_steps(2);
    field(&harness, "Find", "select").focus();
    harness.run_steps(2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    type_text(&mut harness, r"from (t\d*)");
    harness.get_by_label("1 of 2");
    field(&harness, "Replace", "SELECT").focus();
    harness.run_steps(2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    type_text(&mut harness, "FROM public.$1");
    harness.get_by_label("Replace All").click();
    harness.run_steps(3);
    assert_eq!(
        editor_text(&harness),
        "SELECT a FROM public.t;\nSELECT b FROM public.t2;\nSELECT selected;"
    );

    // An invalid pattern says why; Esc closes the bar and returns to the editor.
    field(&harness, "Find", r"from (t\d*)").focus();
    harness.run_steps(2);
    harness.key_press(Key::End);
    type_text(&mut harness, "(");
    assert!(harness.query_by_label_contains("unclosed").is_some());
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(harness.query_by_label("Replace All").is_none());
    assert!(
        harness
            .get_by_role(Role::MultilineTextInput)
            .accesskit_node()
            .is_focused()
    );
}

#[test]
fn ctrl_f_without_a_query_tab_filters_the_explorer() {
    let mut harness = demo_app();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    harness.run_steps(3);
    assert!(harness.query_by_label("Aa").is_none());
    let filter = harness.get(by().role(Role::TextInput).predicate(|n| {
        n.placeholder()
            .is_some_and(|hint| hint.contains("Filter objects"))
    }));
    assert!(filter.accesskit_node().is_focused());
}
