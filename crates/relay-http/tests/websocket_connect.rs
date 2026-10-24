//! Talking to a real websocket server.
//!
//! The unit tests cover URL handling and what the transport refuses.
//! These are about the conversation: that what the person types
//! reaches the far end, that both halves of it land in the log facing
//! the right way, that the request's own authentication is carried
//! through the handshake, and that stopping actually stops.

use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::{Router, routing::get};
use relay_models::{AuthConfig, HttpMethod, HttpRequestSpec, StreamDirection, StreamMessage};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// An endpoint that echoes, but only for a caller holding the token.
///
/// Rejecting at the handshake rather than after it is the point: a
/// websocket's authentication happens in the HTTP request that opens
/// it, so this is what proves the spec's auth was applied.
async fn server() -> String {
    async fn upgrade(headers: axum::http::HeaderMap, ws: WebSocketUpgrade) -> Response {
        let authorised = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("sekrit"));

        if !authorised {
            return axum::http::Response::builder()
                .status(axum::http::StatusCode::UNAUTHORIZED)
                .body(axum::body::Body::empty())
                .unwrap();
        }

        ws.on_upgrade(echo)
    }

    async fn echo(mut socket: WebSocket) {
        // Something unprompted, so a test can prove inbound frames
        // arrive without having to send one first.
        let _ = socket.send(Message::text("hello")).await;

        while let Some(Ok(message)) = socket.recv().await {
            match message {
                Message::Text(text) => {
                    if text.as_str() == "bye" {
                        let _ = socket.send(Message::text("closing")).await;
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                    if text.as_str() == "bytes" {
                        let _ = socket.send(Message::binary(vec![1u8, 2, 255])).await;
                        continue;
                    }
                    let _ = socket.send(Message::text(format!("echo:{text}"))).await;
                }
                Message::Close(_) => return,
                _ => {}
            }
        }
    }

    let app = Router::new().route("/socket", get(upgrade));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("ws://{addr}/socket")
}

fn spec(url: String, token: &str) -> HttpRequestSpec {
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, url);
    spec.auth = AuthConfig::Bearer {
        token: token.into(),
    };
    spec
}

/// Runs a connection, collecting the log, until it ends.
async fn run(
    spec: HttpRequestSpec,
    sends: Vec<&str>,
    cancel: CancellationToken,
) -> (Vec<StreamMessage>, Result<(), String>) {
    let (tx, rx) = mpsc::unbounded_channel();
    for text in sends {
        tx.send(text.to_string()).unwrap();
    }

    let log = Arc::new(Mutex::new(Vec::new()));
    let collector = log.clone();

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        relay_http::websocket::connect(
            &spec,
            rx,
            move |message| collector.lock().unwrap().push(message),
            cancel,
        ),
    )
    .await
    .expect("the connection never finished");

    let messages = log.lock().unwrap().clone();
    (messages, outcome.map_err(|e| e.to_string()))
}

fn texts(messages: &[StreamMessage], direction: StreamDirection) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.direction == direction && m.name == "text")
        .map(|m| m.data.clone())
        .collect()
}

#[tokio::test]
async fn a_message_goes_out_and_the_answer_comes_back() {
    let url = server().await;
    let (log, outcome) = run(
        spec(url, "sekrit"),
        vec!["ping me", "bye"],
        CancellationToken::new(),
    )
    .await;

    assert!(outcome.is_ok(), "{outcome:?}");

    // What we said is in the log too, or it reads as a monologue.
    assert_eq!(texts(&log, StreamDirection::Outbound), ["ping me", "bye"]);

    let inbound = texts(&log, StreamDirection::Inbound);
    assert!(inbound.contains(&"hello".to_string()), "{inbound:?}");
    assert!(inbound.contains(&"echo:ping me".to_string()), "{inbound:?}");
}

#[tokio::test]
async fn the_requests_own_authentication_opens_the_socket() {
    // A websocket authenticates in the HTTP request that upgrades it,
    // so a transport that dropped the spec's auth would fail here and
    // nowhere else.
    let url = server().await;
    let (_log, outcome) = run(spec(url, "wrong"), vec![], CancellationToken::new()).await;

    let error = outcome.expect_err("an unauthorised handshake was accepted");
    assert!(error.contains("401") || error.to_lowercase().contains("unauthorized"), "{error}");
}

#[tokio::test]
async fn the_far_end_closing_is_logged_with_its_reason() {
    let url = server().await;
    let (log, outcome) = run(spec(url, "sekrit"), vec!["bye"], CancellationToken::new()).await;

    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        log.iter().any(|m| m.name == "close"),
        "a server-initiated close left no trace: {log:?}"
    );
}

#[tokio::test]
async fn a_binary_frame_is_logged_as_binary_with_its_length() {
    let url = server().await;
    let (log, _) = run(
        spec(url, "sekrit"),
        vec!["bytes", "bye"],
        CancellationToken::new(),
    )
    .await;

    let binary = log
        .iter()
        .find(|m| m.name == "binary")
        .expect("a binary frame was not logged");
    assert_eq!(binary.binary_bytes, Some(3));
    assert_eq!(binary.data, "01 02 ff");
}

#[tokio::test]
async fn cancelling_ends_a_connection_that_would_otherwise_stay_open() {
    // The echo server never closes on its own, so without the token
    // being honoured this test hangs until its timeout.
    let url = server().await;
    let cancel = CancellationToken::new();

    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        stopper.cancel();
    });

    let (log, outcome) = run(spec(url, "sekrit"), vec!["still here"], cancel).await;

    assert!(outcome.is_ok(), "{outcome:?}");
    // And it got far enough to have talked before being stopped.
    assert!(!log.is_empty());
}
