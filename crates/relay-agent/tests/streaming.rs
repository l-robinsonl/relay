//! Streaming a reply off a real socket.
//!
//! The accumulators have unit tests for every event shape. These are
//! about the wiring: that fragments reach the caller while the
//! response is still open rather than all at the end, that a tool
//! call survives the trip, and that a provider which gives up partway
//! — after the status line has already said 200 — is reported rather
//! than returned as a short answer.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::response::sse::{Event, Sse};
use axum::{Router, routing::post};
use futures_util::stream;
use relay_agent::provider::{
    AnthropicProvider, CompletionRequest, Message, ModelProvider, OpenAiProvider, StopReason,
};

/// Serves a fixed list of `(event, data)` pairs, slowly.
///
/// The gap is what makes "arrived while open" testable: without it a
/// provider that buffered the whole body would pass.
async fn server(path: &'static str, events: Vec<(&'static str, String)>) -> String {
    let app = Router::new().route(
        path,
        post(move || {
            let events = events.clone();
            async move {
                let items = stream::iter(events).then(|(name, data)| async move {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    Ok::<_, std::convert::Infallible>(Event::default().event(name).data(data))
                });
                Sse::new(items)
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

use futures_util::StreamExt;

/// Each fragment, with how long after the start it arrived.
type Timeline = Arc<Mutex<Vec<(String, Duration)>>>;

fn recorder() -> (Timeline, Instant) {
    (Arc::new(Mutex::new(Vec::new())), Instant::now())
}

fn request<'a>(messages: &'a [Message], model: &'a str) -> CompletionRequest<'a> {
    CompletionRequest {
        model,
        system: "You are a test.",
        messages,
        tools: &[],
        max_tokens: 1024,
    }
}

fn anthropic_events() -> Vec<(&'static str, String)> {
    vec![
        ("message_start", r#"{"message":{"usage":{"input_tokens":11}}}"#.into()),
        ("content_block_start", r#"{"index":0,"content_block":{"type":"text"}}"#.into()),
        ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"One "}}"#.into()),
        ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"two "}}"#.into()),
        ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"three"}}"#.into()),
        ("message_delta", r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#.into()),
        ("message_stop", "{}".into()),
    ]
}

#[tokio::test]
async fn claude_text_arrives_while_the_response_is_still_open() {
    let base = server("/v1/messages", anthropic_events()).await;
    let provider = AnthropicProvider::with_base_url("test-key", &base).unwrap();
    let (seen, started) = recorder();

    let collector = seen.clone();
    let response = provider
        .complete_streaming(
            request(&[Message::user("count")], "claude-opus-5-5"),
            &move |fragment: &str| {
                collector
                    .lock()
                    .unwrap()
                    .push((fragment.to_string(), started.elapsed()));
            },
        )
        .await
        .expect("the stream failed");

    let seen = seen.lock().unwrap().clone();
    let fragments: Vec<&str> = seen.iter().map(|(text, _)| text.as_str()).collect();
    assert_eq!(fragments, ["One ", "two ", "three"]);

    // The point of the whole exercise: the first words were on their
    // way before the last ones existed.
    let (_, first) = &seen[0];
    let (_, last) = &seen[seen.len() - 1];
    assert!(
        *last > *first + Duration::from_millis(30),
        "every fragment arrived at once — this is buffering, not streaming: {seen:?}"
    );

    // And what was shown equals what gets persisted.
    assert_eq!(response.text, "One two three");
    assert_eq!(response.stop_reason, StopReason::Done);
    assert_eq!(response.input_tokens, 11);
    assert_eq!(response.output_tokens, 3);
}

#[tokio::test]
async fn claude_streams_a_tool_call_back_whole() {
    let base = server(
        "/v1/messages",
        vec![
            ("content_block_start", r#"{"index":0,"content_block":{"type":"tool_use","id":"c1","name":"get_request"}}"#.into()),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"id\""}}"#.into()),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":":\"r1\"}"}}"#.into()),
            ("message_delta", r#"{"delta":{"stop_reason":"tool_use"}}"#.into()),
        ],
    )
    .await;
    let provider = AnthropicProvider::with_base_url("test-key", &base).unwrap();

    let response = provider
        .complete_streaming(request(&[Message::user("look")], "claude-opus-5-5"), &|_| {})
        .await
        .unwrap();

    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "get_request");
    assert_eq!(
        response.tool_calls[0].input,
        serde_json::json!({"id": "r1"})
    );
}

#[tokio::test]
async fn a_provider_that_gives_up_mid_stream_is_reported() {
    // The status line said 200 long before this happened, so nothing
    // upstream of the body can notice it. Returning the half-written
    // answer as though it were complete is the failure to avoid.
    let base = server(
        "/v1/messages",
        vec![
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"Half an ans"}}"#.into()),
            ("error", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#.into()),
        ],
    )
    .await;
    let provider = AnthropicProvider::with_base_url("test-key", &base).unwrap();

    let error = provider
        .complete_streaming(request(&[Message::user("hi")], "claude-opus-5-5"), &|_| {})
        .await
        .expect_err("a truncated answer was returned as a whole one");

    assert!(error.to_string().contains("Overloaded"), "{error}");
}

#[tokio::test]
async fn openai_text_arrives_in_pieces_and_usage_comes_last() {
    let base = server(
        "/v1/chat/completions",
        vec![
            ("message", r#"{"choices":[{"delta":{"content":"Al"}}]}"#.into()),
            ("message", r#"{"choices":[{"delta":{"content":"pha"}}]}"#.into()),
            ("message", r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.into()),
            ("message", r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":2}}"#.into()),
            ("message", "[DONE]".into()),
        ],
    )
    .await;
    let provider = OpenAiProvider::with_base_url("test-key", format!("{base}/v1"), "OpenAI").unwrap();
    let (seen, started) = recorder();

    let collector = seen.clone();
    let response = provider
        .complete_streaming(
            request(&[Message::user("say alpha")], "gpt-4o"),
            &move |fragment: &str| {
                collector
                    .lock()
                    .unwrap()
                    .push((fragment.to_string(), started.elapsed()));
            },
        )
        .await
        .expect("the stream failed");

    let fragments: Vec<String> = seen.lock().unwrap().iter().map(|(t, _)| t.clone()).collect();
    assert_eq!(fragments, ["Al", "pha"]);
    assert_eq!(response.text, "Alpha");
    // Without `stream_options.include_usage` these are both zero and
    // every streamed turn reports costing nothing.
    assert_eq!(response.input_tokens, 9);
    assert_eq!(response.output_tokens, 2);
}

#[tokio::test]
async fn a_reply_split_across_awkward_chunks_still_reads_correctly() {
    // Not a contrived case: a fragment containing a newline is split
    // at the line break by any sane writer, and an event spanning two
    // TCP segments is normal under load.
    let base = server(
        "/v1/messages",
        vec![
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"line one\nline two"}}"#.into()),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"text_delta","text":"\n\nand three"}}"#.into()),
        ],
    )
    .await;
    let provider = AnthropicProvider::with_base_url("test-key", &base).unwrap();
    let seen = Arc::new(Mutex::new(String::new()));

    let collector = seen.clone();
    let response = provider
        .complete_streaming(
            request(&[Message::user("lines")], "claude-opus-5-5"),
            &move |fragment: &str| collector.lock().unwrap().push_str(fragment),
        )
        .await
        .unwrap();

    assert_eq!(response.text, "line one\nline two\n\nand three");
    assert_eq!(*seen.lock().unwrap(), response.text);
}
