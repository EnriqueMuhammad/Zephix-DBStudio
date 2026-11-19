//! Headless test of the AI assistant in the real app (demo mode) against a local
//! OpenAI-compatible server that streams a canned answer. No network, no API key.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use rdbs_core::MemoryCredentials;
use rdbs_core::ai::{AiProtocol, AiProvider, PRESETS};
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

/// Answers one chat request with a streamed reply and returns the request it got.
fn fake_provider() -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let chunks = [
            r#"Mock answer: the table name is misspelled.\n\n"#,
            r#"```sql\nSELECT 42 AS fixed;\n"#,
            r#"```\n"#,
        ];
        let mut body = String::new();
        for c in chunks {
            body.push_str(&format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{c}\"}}}}]}}\n\n"
            ));
        }
        body.push_str("data: [DONE]\n\n");
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    (base, handle)
}

fn app_with_provider(base: &str) -> Harness<'static, StudioApp> {
    let provider = AiProvider {
        name: "Local mock".into(),
        protocol: AiProtocol::OpenAiCompatible,
        base_url: base.to_owned(),
        model: "mock-model".into(),
        needs_key: false,
        ..AiProvider::from_preset(&PRESETS[0])
    };
    app_with(vec![provider])
}

fn app_with(providers: Vec<AiProvider>) -> Harness<'static, StudioApp> {
    let storage: &'static TestStorage = Box::leak(Box::new(TestStorage(HashMap::from([
        ("rdbs.update.check_on_start".to_owned(), "false".to_owned()),
        (
            "rdbs.ai.providers".to_owned(),
            ron::to_string(&providers).unwrap(),
        ),
    ]))));
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 860.0))
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

fn wait_for(harness: &mut Harness<'_, StudioApp>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while harness.query_by_label_contains(label).is_none() {
        assert!(Instant::now() < deadline, "timed out waiting for {label:?}");
        std::thread::sleep(Duration::from_millis(20));
        harness.step();
    }
    harness.run_steps(2);
}

fn editor_text(harness: &Harness<'_, StudioApp>) -> String {
    harness
        .get_all_by_role(Role::MultilineTextInput)
        .find_map(|n| {
            let v = n.accesskit_node().value()?;
            v.contains("SELECT").then_some(v)
        })
        .unwrap_or_default()
}

#[test]
fn explain_streams_an_answer_and_inserts_its_sql() {
    let (base, server) = fake_provider();
    let mut harness = app_with_provider(&base);

    // A query tab on the demo PostgreSQL connection with some SQL.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::T);
    harness.run_steps(4);
    harness.get_by_role(Role::ComboBox).click();
    harness.run_steps(2);
    harness.get_by_label_contains("Local PostgreSQL").click();
    harness.run_steps(2);
    let editor = harness.get_by_role(Role::MultilineTextInput);
    editor.focus();
    harness.run_steps(2);
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text("SELECT * FROM custmers");
    harness.run_steps(2);

    // Open the panel and ask.
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::I);
    harness.run_steps(4);
    harness.get_by_label_contains("PostgreSQL · ");
    harness.get_by_label("Explain").click();
    wait_for(&mut harness, "Mock answer");
    wait_for(&mut harness, "SELECT 42 AS fixed;");

    // The request carried the engine and the editor text, and no API key.
    let seen = server.join().unwrap();
    assert!(seen.starts_with("POST /v1/chat/completions"), "{seen}");
    assert!(seen.contains("\"model\":\"mock-model\""));
    assert!(seen.contains("Engine: PostgreSQL"), "{seen}");
    assert!(seen.contains("SELECT * FROM custmers"));
    assert!(!seen.to_ascii_lowercase().contains("authorization:"));

    // Insert replaces the whole editor text with the suggestion; nothing runs.
    harness.get_by_label_contains("Replace").click();
    harness.run_steps(4);
    assert_eq!(editor_text(&harness), "SELECT 42 AS fixed;");
    assert!(harness.query_by_label_contains("rows").is_none(), "not run");
}

/// Regression: the provider dialog grew past the window (a vertical separator took the whole
/// screen height), so only an empty white middle was visible.
#[test]
fn provider_dialog_fits_the_window() {
    let mut harness = app_with(Vec::new());
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::I);
    harness.run_steps(4);
    harness.get_by_label_contains("Set Up a Provider").click();
    harness.run_steps(4);
    harness.get_by_label_contains("Add Provider").click();
    harness.run_steps(4);
    harness.get_by_label("Ollama (local)").click();
    harness.run_steps(8);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 860.0));
    let save = harness.get_by_label_contains("  Save").rect();
    assert!(
        window.contains_rect(save),
        "Save at {save:?} is outside the window"
    );
    for label in [
        "AI Assistant Providers",
        "Base URL",
        "Load Models",
        "Cancel",
    ] {
        let rect = harness.get_by_label(label).rect();
        assert!(
            window.contains_rect(rect),
            "{label:?} at {rect:?} is outside the window"
        );
    }
    let dialog_height = harness.get_by_label("Cancel").rect().bottom()
        - harness.get_by_label("AI Assistant Providers").rect().top();
    assert!(dialog_height < 700.0, "dialog is {dialog_height}px tall");
}
