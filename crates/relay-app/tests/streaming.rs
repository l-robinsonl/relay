//! Opening a stream through the service, as the interface will.
//!
//! The engine has its own tests for reading events off a socket.
//! These are about the part the interface depends on: that opening
//! returns immediately rather than blocking until a stream that never
//! ends has ended, that events arrive as they happen, and that the
//! stop button stops it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::sse::{Event, Sse};
use axum::{Router, routing::get};
use relay_app::streams::{StreamEvent, StreamSink};
use relay_app::{MemorySecretStore, RelayService};
use relay_http::HttpEngine;
use relay_models::{ExecutionId, RequestKind};
use relay_storage::Database;

/// Collects what the interface would be shown.
#[derive(Default)]
struct Collector(Mutex<Vec<StreamEvent>>);

impl StreamSink for Collector {
    fn emit(&self, event: StreamEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl Collector {
    /// What was said, in order, with the direction written in so a
    /// websocket transcript can be asserted on as a conversation.
    fn transcript(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Message { message, .. } => Some(format!(
                    "{}{}",
                    match message.direction {
                        relay_models::StreamDirection::Inbound => "< ",
                        relay_models::StreamDirection::Outbound => "> ",
                    },
                    message.data
                )),
                _ => None,
            })
            .collect()
    }

    fn messages(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Message { message, .. } => Some(message.data.clone()),
                _ => None,
            })
            .collect()
    }

    fn closed(&self) -> Option<Option<String>> {
        self.0.lock().unwrap().iter().find_map(|e| match e {
            StreamEvent::Closed { error, .. } => Some(error.clone()),
            _ => None,
        })
    }
}

/// An endpoint that emits `count` events, one every `gap`.
async fn server(count: usize, gap: Duration) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/events",
        get(move || async move {
            let items = futures_util::stream::iter(0..count).then(move |n| async move {
                tokio::time::sleep(gap).await;
                Ok::<_, std::convert::Infallible>(Event::default().data(n.to_string()))
            });
            Sse::new(items)
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

fn service() -> RelayService {
    RelayService::new(
        Database::open_in_memory().unwrap(),
        HttpEngine::with_defaults(),
        Arc::new(MemorySecretStore::new()),
    )
}

async fn a_stream(service: &RelayService, url: String) -> relay_models::RequestDoc {
    let workspace = service.active_workspace().await.unwrap();
    let mut doc = service
        .request_create(workspace.id.clone(), "Prices".into(), None)
        .await
        .unwrap();
    doc.kind = RequestKind::Sse;
    doc.url = url;
    doc
}

use futures_util::StreamExt;

#[tokio::test]
async fn opening_returns_before_the_stream_ends() {
    // A stream does not finish. If opening waited for it to, the
    // interface would freeze the moment somebody pressed send.
    let (base, _handle) = server(3, Duration::from_millis(80)).await;
    let service = service();
    let doc = a_stream(&service, format!("{base}/events")).await;
    let events = Arc::new(Collector::default());

    let started = std::time::Instant::now();
    service
        .stream_open(ExecutionId::new(), doc, None, events.clone())
        .await
        .unwrap();

    assert!(
        started.elapsed() < Duration::from_millis(60),
        "opening blocked for {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn events_arrive_as_they_happen() {
    let (base, _handle) = server(3, Duration::from_millis(30)).await;
    let service = service();
    let doc = a_stream(&service, format!("{base}/events")).await;
    let events = Arc::new(Collector::default());

    service
        .stream_open(ExecutionId::new(), doc, None, events.clone())
        .await
        .unwrap();

    // Long enough for all three, short enough to fail if they only
    // arrive when the connection closes.
    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(events.messages(), vec!["0", "1", "2"]);
    assert_eq!(events.closed(), Some(None), "it did not report closing");
}

#[tokio::test]
async fn closing_stops_a_stream_that_would_keep_going() {
    // The stop button. Without it a subscription to something busy
    // runs until the application quits.
    let (base, _handle) = server(50, Duration::from_millis(20)).await;
    let service = service();
    let doc = a_stream(&service, format!("{base}/events")).await;
    let events = Arc::new(Collector::default());
    let id = ExecutionId::new();

    service
        .stream_open(id.clone(), doc, None, events.clone())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(service.stream_close(&id), "nothing was open to close");

    tokio::time::sleep(Duration::from_millis(200)).await;
    let after_close = events.messages().len();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        events.messages().len(),
        after_close,
        "events kept arriving after it was closed"
    );
    assert!(after_close < 50, "it read the whole stream anyway");
    // Being stopped on purpose is not an error.
    assert_eq!(events.closed(), Some(None));
}

#[tokio::test]
async fn closing_something_already_finished_is_harmless() {
    let (base, _handle) = server(1, Duration::from_millis(10)).await;
    let service = service();
    let doc = a_stream(&service, format!("{base}/events")).await;
    let events = Arc::new(Collector::default());
    let id = ExecutionId::new();

    service
        .stream_open(id.clone(), doc, None, events.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(!service.stream_close(&id), "it was still registered");
}

/// An echo socket, for the two-way half.
async fn socket_server() -> (String, tokio::task::JoinHandle<()>) {
    use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};

    async fn echo(mut socket: WebSocket) {
        while let Some(Ok(message)) = socket.recv().await {
            if let Message::Text(text) = message {
                let _ = socket.send(Message::text(format!("echo:{text}"))).await;
            }
        }
    }

    let app = Router::new().route(
        "/socket",
        get(|ws: WebSocketUpgrade| async move { ws.on_upgrade(echo) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("ws://{addr}/socket"), handle)
}

async fn a_socket(service: &RelayService, url: String) -> relay_models::RequestDoc {
    let mut doc = a_stream(service, url).await;
    doc.kind = RequestKind::WebSocket;
    doc
}

#[tokio::test]
async fn a_websocket_is_a_conversation_and_the_log_shows_both_halves() {
    let (url, _handle) = socket_server().await;
    let service = service();
    let doc = a_socket(&service, url).await;
    let events = Arc::new(Collector::default());
    let id = ExecutionId::new();

    service
        .stream_open(id.clone(), doc, None, events.clone())
        .await
        .unwrap();

    // The socket is opened by a task, so there is a moment before it
    // can carry anything. This is the one place a test has to wait
    // for that rather than for a message.
    tokio::time::sleep(Duration::from_millis(200)).await;
    service.stream_send(&id, "hello".into()).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(events.transcript(), ["> hello", "< echo:hello"]);

    service.stream_close(&id);
}

#[tokio::test]
async fn sending_on_server_sent_events_is_refused_rather_than_swallowed() {
    // There is no channel back on an SSE stream. A caller told
    // nothing would wait for a reply that cannot come.
    let (base, _handle) = server(50, Duration::from_millis(50)).await;
    let service = service();
    let doc = a_stream(&service, format!("{base}/events")).await;
    let events = Arc::new(Collector::default());
    let id = ExecutionId::new();

    service
        .stream_open(id.clone(), doc, None, events.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let error = service
        .stream_send(&id, "hello".into())
        .expect_err("a one-way stream accepted a message");
    assert!(error.to_string().contains("one-way"), "{error}");

    service.stream_close(&id);
}

#[tokio::test]
async fn sending_on_nothing_says_so() {
    let service = service();
    assert!(service.stream_send(&ExecutionId::new(), "x".into()).is_err());
}

#[tokio::test]
async fn an_ordinary_request_is_not_opened_as_a_stream() {
    // Otherwise a GET would be opened as server-sent events and sit
    // there reading a body that is not one.
    let service = service();
    let mut doc = a_stream(&service, "https://example.com".into()).await;
    doc.kind = RequestKind::Http;

    let error = service
        .stream_open(ExecutionId::new(), doc, None, Arc::new(Collector::default()))
        .await
        .expect_err("an HTTP request was opened as a stream");
    assert!(error.to_string().contains("not a stream"), "{error}");
}

#[tokio::test]
async fn an_endpoint_that_is_not_there_reports_why() {
    let service = service();
    let doc = a_stream(&service, "http://127.0.0.1:1/events".into()).await;
    let events = Arc::new(Collector::default());

    service
        .stream_open(ExecutionId::new(), doc, None, events.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    match events.closed() {
        Some(Some(message)) => assert!(!message.is_empty()),
        other => panic!("expected a reported failure, got {other:?}"),
    }
}
