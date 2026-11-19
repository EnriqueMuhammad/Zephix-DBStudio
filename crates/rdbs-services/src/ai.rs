//! Sends AI assistant requests (ADR 0014).
//!
//! Blocking HTTP with `ureq` on Tokio's blocking pool, like the update check. Answers stream
//! as server-sent events; each text piece is handed to a callback, so the UI can show the
//! reply while it is being written.

use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use rdbs_core::CancelHandle;
use rdbs_core::ai::{
    AiProtocol, HttpRequest, StreamPiece, describe_http_error, parse_complete, parse_models,
    parse_stream_data,
};

/// Connecting (DNS, TCP, TLS) gives up after this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for the response headers, and for each piece of a streamed answer. Reasoning
/// models can be quiet for a while before the first token.
const READ_TIMEOUT: Duration = Duration::from_secs(300);
/// A model list is small: the whole request gives up after this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);

fn agent(read_timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(read_timeout))
        .timeout_per_read(Some(read_timeout))
        // Error bodies explain what is wrong (bad key, unknown model): read them.
        .http_status_as_error(false)
        .user_agent(format!("Zephix/{}", env!("CARGO_PKG_VERSION")))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(crate::update::root_certs())
                .build(),
        )
        .build();
    ureq::Agent::new_with_config(config)
}

fn describe_error(err: &ureq::Error) -> String {
    match err {
        ureq::Error::Timeout(_) => "The provider did not answer in time.".to_owned(),
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            "Nothing is listening at the base URL (is the local server running?).".to_owned()
        }
        other => format!("Could not reach the provider: {other}"),
    }
}

/// Error text for a non-2xx answer (reads at most 64 KiB of the body).
fn status_error(status: u16, body: impl Read) -> String {
    let mut text = String::new();
    let _ = body.take(64 * 1024).read_to_string(&mut text);
    describe_http_error(status, &text)
}

/// Sends a chat request and calls `on_text` for every piece of the answer.
///
/// Returns `Err` with one sentence when the request fails or is cancelled. A cancel is seen
/// between two pieces of the stream.
pub(crate) fn chat(
    request: &HttpRequest,
    protocol: AiProtocol,
    cancel: &CancelHandle,
    mut on_text: impl FnMut(String),
) -> Result<(), String> {
    let mut call = agent(READ_TIMEOUT).post(&request.url);
    for (name, value) in &request.headers {
        call = call.header(*name, value);
    }
    let response = call
        .send(request.body.as_str())
        .map_err(|e| describe_error(&e))?;
    let status = response.status().as_u16();
    let is_stream = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    let reader = response.into_body().into_reader();
    if !(200..300).contains(&status) {
        return Err(status_error(status, reader));
    }
    if cancel.is_cancelled() {
        return Err("Cancelled.".to_owned());
    }
    if !is_stream {
        // The server ignored "stream": true and sent the whole answer at once.
        let mut body = String::new();
        reader
            .take(16 * 1024 * 1024)
            .read_to_string(&mut body)
            .map_err(|e| format!("Could not read the answer: {e}"))?;
        on_text(parse_complete(protocol, &body)?);
        return Ok(());
    }
    let mut got_text = false;
    for line in BufReader::new(reader).lines() {
        if cancel.is_cancelled() {
            return Err("Cancelled.".to_owned());
        }
        let line = line.map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => "The provider stopped answering.".to_owned(),
            _ => format!("The answer was cut off: {e}"),
        })?;
        // `data:` lines carry the payload; `event:`, `id:` and comments are not needed.
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        match parse_stream_data(protocol, data) {
            StreamPiece::Text(text) => {
                got_text = true;
                on_text(text);
            }
            StreamPiece::Done => return Ok(()),
            StreamPiece::Error(message) => return Err(format!("The provider failed: {message}")),
            StreamPiece::Skip => {}
        }
    }
    if got_text {
        // Some servers close the stream without a final marker.
        Ok(())
    } else {
        Err("The provider closed the answer without any text.".to_owned())
    }
}

/// Fetches the model ids a provider offers.
pub(crate) fn list_models(request: &HttpRequest) -> Result<Vec<String>, String> {
    let mut call = agent(LIST_TIMEOUT).get(&request.url);
    for (name, value) in &request.headers {
        call = call.header(*name, value);
    }
    let response = call.call().map_err(|e| describe_error(&e))?;
    let status = response.status().as_u16();
    let reader = response.into_body().into_reader();
    if !(200..300).contains(&status) {
        return Err(status_error(status, reader));
    }
    let mut body = String::new();
    reader
        .take(8 * 1024 * 1024)
        .read_to_string(&mut body)
        .map_err(|e| format!("Could not read the model list: {e}"))?;
    parse_models(&body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use rdbs_core::ai::{AiMessage, AiProvider, chat_request, models_request};

    use super::*;

    /// A one-shot HTTP server on 127.0.0.1 that answers every request with `status`,
    /// `content_type` and `body`, and hands back the request it received.
    pub(crate) fn serve(
        status: u16,
        content_type: &'static str,
        body: String,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            // Read headers, then as much body as Content-Length says.
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
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (base, handle)
    }

    pub(crate) fn local_provider(base: &str, protocol: AiProtocol) -> AiProvider {
        AiProvider {
            protocol,
            base_url: base.to_owned(),
            model: "test-model".into(),
            ..AiProvider::from_preset(&rdbs_core::ai::PRESETS[0])
        }
    }

    #[test]
    fn streams_openai_compatible_answer() {
        let sse = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"SELECT \"}}]}\n\n\
                   : keep-alive\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"1;\"}}]}\n\n\
                   data: [DONE]\n\n"
            .to_owned();
        let (base, server) = serve(200, "text/event-stream", sse);
        let p = local_provider(&base, AiProtocol::OpenAiCompatible);
        let req = chat_request(&p, Some("sk-test"), "sys", &[AiMessage::user("q")]);
        let mut text = String::new();
        chat(&req, p.protocol, &CancelHandle::new(), |t| {
            text.push_str(&t)
        })
        .unwrap();
        assert_eq!(text, "SELECT 1;");
        let seen = server.join().unwrap();
        assert!(seen.starts_with("POST /v1/chat/completions"), "{seen}");
        assert!(
            seen.to_ascii_lowercase()
                .contains("authorization: bearer sk-test")
        );
        assert!(seen.contains("\"model\":\"test-model\""));
    }

    #[test]
    fn streams_anthropic_answer() {
        let sse = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n\
                   event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
                   event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            .to_owned();
        let (base, server) = serve(200, "text/event-stream", sse);
        let p = local_provider(&base, AiProtocol::Anthropic);
        let req = chat_request(&p, Some("k"), "sys", &[AiMessage::user("q")]);
        let mut text = String::new();
        chat(&req, p.protocol, &CancelHandle::new(), |t| {
            text.push_str(&t)
        })
        .unwrap();
        assert_eq!(text, "Hi");
        let seen = server.join().unwrap().to_ascii_lowercase();
        assert!(seen.starts_with("post /v1/messages"));
        assert!(seen.contains("x-api-key: k") && seen.contains("anthropic-version:"));
    }

    #[test]
    fn non_streamed_json_answer_is_accepted() {
        let (base, server) = serve(
            200,
            "application/json",
            r#"{"choices":[{"message":{"content":"whole"}}]}"#.to_owned(),
        );
        let p = local_provider(&base, AiProtocol::OpenAiCompatible);
        let req = chat_request(&p, None, "s", &[AiMessage::user("q")]);
        let mut text = String::new();
        chat(&req, p.protocol, &CancelHandle::new(), |t| {
            text.push_str(&t)
        })
        .unwrap();
        assert_eq!(text, "whole");
        server.join().unwrap();
    }

    #[test]
    fn http_errors_carry_the_provider_message() {
        let (base, server) = serve(
            401,
            "application/json",
            r#"{"error":{"message":"Incorrect API key"}}"#.to_owned(),
        );
        let p = local_provider(&base, AiProtocol::OpenAiCompatible);
        let req = chat_request(&p, Some("bad"), "s", &[AiMessage::user("q")]);
        let err = chat(&req, p.protocol, &CancelHandle::new(), |_| {}).unwrap_err();
        assert!(
            err.contains("401") && err.contains("Incorrect API key"),
            "{err}"
        );
        server.join().unwrap();
    }

    #[test]
    fn refused_connection_is_explained() {
        // Bind and drop: nothing listens on that port any more.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let p = local_provider(
            &format!("http://127.0.0.1:{port}/v1"),
            AiProtocol::OpenAiCompatible,
        );
        let req = chat_request(&p, None, "s", &[AiMessage::user("q")]);
        let err = chat(&req, p.protocol, &CancelHandle::new(), |_| {}).unwrap_err();
        assert!(!err.is_empty());
    }

    /// The whole path through the worker: key from the keychain, chunks, then finished.
    #[test]
    fn worker_streams_chunks_with_the_saved_key() {
        use std::sync::Arc;

        use rdbs_core::{Command, CredentialStore, Event, MemoryCredentials, SecretKind};

        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n\
                   data: [DONE]\n\n"
            .to_owned();
        let (base, server) = serve(200, "text/event-stream", sse);
        let provider = local_provider(&base, AiProtocol::OpenAiCompatible);
        let credentials = Arc::new(MemoryCredentials::default());
        credentials
            .set(
                provider.keychain_id(),
                SecretKind::AiApiKey,
                "from-keychain",
            )
            .unwrap();
        let h = crate::ServiceHandle::spawn(
            Arc::new(rdbs_adapter_mock::MockAdapterFactory::new()),
            credentials,
            || {},
        )
        .unwrap();
        let id = h.next_request_id();
        h.send(Command::AiChat {
            request_id: id,
            provider: provider.clone(),
            api_key: None,
            system: "s".into(),
            messages: vec![AiMessage::user("q")],
        });
        let mut text = String::new();
        let finished = loop {
            match h.recv_timeout(Duration::from_secs(10)).expect("an event") {
                Event::AiChunk {
                    request_id,
                    text: t,
                } if request_id == id => text.push_str(&t),
                Event::AiFinished { request_id, result } if request_id == id => break result,
                _ => {}
            }
        };
        assert_eq!(finished, Ok(()));
        assert_eq!(text, "ab");
        let seen = server.join().unwrap().to_ascii_lowercase();
        assert!(seen.contains("authorization: bearer from-keychain"));

        // Without a saved key the request is not sent at all.
        let other = AiProvider {
            id: AiProvider::from_preset(&rdbs_core::ai::PRESETS[0]).id,
            ..provider
        };
        let id = h.next_request_id();
        h.send(Command::AiChat {
            request_id: id,
            provider: other,
            api_key: None,
            system: "s".into(),
            messages: vec![AiMessage::user("q")],
        });
        let result = loop {
            if let Some(Event::AiFinished { request_id, result }) =
                h.recv_timeout(Duration::from_secs(10))
                && request_id == id
            {
                break result;
            }
        };
        assert!(result.unwrap_err().contains("No API key"));
    }

    #[test]
    fn lists_models() {
        let (base, server) = serve(
            200,
            "application/json",
            r#"{"data":[{"id":"b-model"},{"id":"a-model"}]}"#.to_owned(),
        );
        let p = local_provider(&base, AiProtocol::Anthropic);
        let ids = list_models(&models_request(&p, Some("k"))).unwrap();
        assert_eq!(ids, ["a-model", "b-model"]);
        assert!(server.join().unwrap().starts_with("GET /v1/models"));
    }
}
