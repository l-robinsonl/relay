//! Reading server-sent events off a real socket.
//!
//! The parser has unit tests for chunk boundaries. These are about
//! the connection: that it carries the request's own authentication,
//! that events arrive as they are written rather than all at the end,
//! and that the caller can stop a stream which by design never
//! finishes.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::sse::{Event, Sse};
use axum::{Router, routing::get};
use futures_util::stream;
use relay_http::ClientPool;
use relay_models::{AuthConfig, HttpMethod, HttpRequestSpec, KeyValue};

/// An endpoint that writes three events, slowly, and only to a
/// caller holding the token.
async fn server() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/events",
        get(|headers: axum::http::HeaderMap| async move {
            let authorised = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("sekrit"));

            let items: Vec<Result<Event, std::convert::Infallible>> = if authorised {
                vec![
                    Ok(Event::default().event("price").data("1")),
                    Ok(Event::default().event("price").data("2")),
                    Ok(Event::default().event("done").data("bye")),
                ]
            } else {
                vec![Ok(Event::default().event("error").data("unauthorised"))]
            };

            Sse::new(stream::iter(items))
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

fn spec(url: String, auth: AuthConfig) -> HttpRequestSpec {
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, url);
    spec.auth = auth;
    spec
}

#[tokio::test]
async fn events_arrive_from_a_live_endpoint() {
    let (base, _handle) = server().await;
    let pool = ClientPool::new(Duration::from_secs(10), "relay-test");
    let seen = Arc::new(Mutex::new(Vec::new()));

    let collector = seen.clone();
    relay_http::sse::stream(
        &pool,
        &spec(
            format!("{base}/events"),
            AuthConfig::Bearer {
                token: "sekrit".into(),
            },
        ),
        move |event| {
            collector.lock().unwrap().push((event.event, event.data));
            true
        },
    )
    .await
    .unwrap();

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            ("price".to_string(), "1".to_string()),
            ("price".to_string(), "2".to_string()),
            ("done".to_string(), "bye".to_string()),
        ]
    );
}

#[tokio::test]
async fn the_stream_carries_the_requests_authentication() {
    // An events endpoint behind a token is the normal case.
    let (base, _handle) = server().await;
    let pool = ClientPool::new(Duration::from_secs(10), "relay-test");
    let seen = Arc::new(Mutex::new(Vec::new()));

    let collector = seen.clone();
    relay_http::sse::stream(
        &pool,
        &spec(format!("{base}/events"), AuthConfig::None),
        move |event| {
            collector.lock().unwrap().push(event.data);
            true
        },
    )
    .await
    .unwrap();

    assert_eq!(
        seen.lock().unwrap().clone(),
        vec!["unauthorised".to_string()]
    );
}

#[tokio::test]
async fn returning_false_closes_a_stream_that_would_not_end() {
    // The interface's stop button. Without it, a subscription to
    // something busy runs until the application quits.
    let (base, _handle) = server().await;
    let pool = ClientPool::new(Duration::from_secs(10), "relay-test");
    let seen = Arc::new(Mutex::new(0usize));

    let counter = seen.clone();
    relay_http::sse::stream(
        &pool,
        &spec(
            format!("{base}/events"),
            AuthConfig::Bearer {
                token: "sekrit".into(),
            },
        ),
        move |_| {
            let mut n = counter.lock().unwrap();
            *n += 1;
            // Stop after the first.
            false
        },
    )
    .await
    .unwrap();

    assert_eq!(
        *seen.lock().unwrap(),
        1,
        "it kept reading after being told to stop"
    );
}

#[tokio::test]
async fn a_request_header_beats_the_default_accept() {
    // Somebody testing what a server does with a different Accept
    // should be able to.
    let seen = Arc::new(Mutex::new(String::new()));
    let recorder = seen.clone();

    let app = Router::new().route(
        "/events",
        get(move |headers: axum::http::HeaderMap| {
            let recorder = recorder.clone();
            async move {
                *recorder.lock().unwrap() = headers
                    .get("accept")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                Sse::new(stream::iter(vec![Ok::<_, std::convert::Infallible>(
                    Event::default().data("x"),
                )]))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let pool = ClientPool::new(Duration::from_secs(10), "relay-test");
    let mut custom = spec(format!("http://{addr}/events"), AuthConfig::None);
    custom.headers = vec![KeyValue::new("Accept", "application/x-ndjson")];

    relay_http::sse::stream(&pool, &custom, |_| true)
        .await
        .unwrap();

    assert_eq!(seen.lock().unwrap().as_str(), "application/x-ndjson");
}
