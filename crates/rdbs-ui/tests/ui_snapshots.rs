//! Headless UI tests of the whole app in demo mode (mock engine, in-memory profiles).
//!
//! Each snapshot is the AccessKit tree (role, label, value) written as text, so it does not
//! depend on a GPU or font rendering. Refresh the files after an intended UI change with
//! `UPDATE_SNAPSHOTS=1 cargo test -p rdbs-ui --test ui_snapshots`.

#![allow(clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use rdbs_core::MemoryCredentials;
use rdbs_ui::{AppConfig, StudioApp};

/// Persisted settings seen by the app at start: no update check (it would use the network).
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

/// Steps the app until a node with `label` exists; the worker answers on another thread.
fn wait_for_label(harness: &mut Harness<'_, StudioApp>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while harness.query_by_label_contains(label).is_none() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label:?}:\n{}",
            tree_text(harness)
        );
        std::thread::sleep(Duration::from_millis(20));
        harness.step();
    }
    harness.run_steps(2);
}

/// The AccessKit tree as indented text: every node with a label or a value.
fn tree_text(harness: &Harness<'_, StudioApp>) -> String {
    fn walk<'t, N: NodeT<'t> + 't>(node: &N, depth: usize, out: &mut String) {
        let ak = node.accesskit_node();
        let label = ak.label();
        let value = ak.value();
        let shown = label.is_some() || value.is_some();
        if shown {
            out.push_str(&"  ".repeat(depth));
            out.push_str(&format!("{:?}", ak.role()));
            if let Some(label) = label {
                out.push_str(&format!(" {label:?}"));
            }
            if let Some(value) = value {
                out.push_str(&format!(" = {value:?}"));
            }
            if ak.is_disabled() {
                out.push_str(" (disabled)");
            }
            out.push('\n');
        }
        for child in node.children() {
            walk(&child, depth + usize::from(shown), out);
        }
    }
    let mut out = String::new();
    walk(&harness.root(), 0, &mut out);
    // A version bump alone should not change the snapshots.
    out.replace(concat!("v", env!("CARGO_PKG_VERSION")), "v{version}")
}

fn check_snapshot(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("snapshot dir")).expect("create dir");
        std::fs::write(&path, actual).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "missing {} ({e}); run with UPDATE_SNAPSHOTS=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    if expected != actual {
        let first = expected
            .lines()
            .zip(actual.lines())
            .position(|(e, a)| e != a)
            .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
        panic!(
            "snapshot {name} differs at line {}:\n  expected: {:?}\n  actual:   {:?}\n\
             Run with UPDATE_SNAPSHOTS=1 if the change is intended.",
            first + 1,
            expected.lines().nth(first),
            actual.lines().nth(first),
        );
    }
}

#[test]
fn start_page() {
    let harness = demo_app();
    harness.get_by_label_contains("New query tab");
    check_snapshot("start_page", &tree_text(&harness));
}

#[test]
fn new_query_tab_shows_the_editor() {
    let mut harness = demo_app();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::T);
    harness.run_steps(4);
    check_snapshot("new_query_tab", &tree_text(&harness));
}

#[test]
fn file_menu() {
    let mut harness = demo_app();
    harness.get_by_label("File").click();
    harness.run_steps(4);
    check_snapshot("file_menu", &tree_text(&harness));
}

#[test]
fn edit_menu_with_a_query_tab() {
    let mut harness = demo_app();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::T);
    harness.run_steps(4);
    harness.get_by_label("Edit").click();
    harness.run_steps(4);
    check_snapshot("edit_menu", &tree_text(&harness));
}

#[test]
fn query_runs_against_the_demo_engine() {
    let mut harness = demo_app();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::T);
    harness.run_steps(4);
    harness.get_by_role(egui::accesskit::Role::ComboBox).click();
    harness.run_steps(2);
    harness.get_by_label_contains("Local PostgreSQL").click();
    harness.run_steps(2);
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    harness.run_steps(2);
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("SELECT 1 AS answer");
    harness.run_steps(2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    // The mock engine answers any SELECT with its sample customer table.
    wait_for_label(&mut harness, "1,000 rows");
    harness.get_by_label("full_name");
    harness.get_by_label("Putri Saputra");
}
