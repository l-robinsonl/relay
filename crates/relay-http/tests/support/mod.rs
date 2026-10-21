//! A local HTTP server for integration tests.
//!
//! Tests run against a real server on a real socket rather than a mocked
//! transport, because most of what can go wrong in an HTTP client — header
//! encoding, redirects, chunked bodies, slow responses, early disconnects —
//! only goes wrong at the socket.

use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

/// A running test server. Dropping it shuts the server down.
pub struct TestServer {
    pub addr: SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// The absolute URL of `path` on this server.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Starts the test server on an ephemeral port.
pub async fn start() -> TestServer {
    let app = Router::new()
        .route("/echo", any(echo))
        .route("/status/{code}", any(status_code))
        .route("/json", get(json_body))
        .route("/text", get(text_body))
        .route("/binary", get(binary_body))
        .route("/latin1", get(latin1_body))
        .route("/empty", get(empty_body))
        .route("/slow", get(slow_body))
        .route("/stream/{count}", get(stream_body))
        .route("/redirect/{n}", get(redirect_chain))
        .route("/redirect-loop", get(redirect_loop))
        .route("/set-cookie", get(set_cookie))
        .route("/read-cookie", get(read_cookie));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    TestServer { addr, handle }
}

/// Echoes back the method, headers, query and body it received.
async fn echo(
    method: axum::http::Method,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let header_map: HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or("<binary>").to_string(),
            )
        })
        .collect();

    Json(serde_json::json!({
        "method": method.as_str(),
        "query": query,
        "headers": header_map,
        "body": String::from_utf8_lossy(&body),
        "bodyLen": body.len(),
    }))
}

async fn status_code(Path(code): Path<u16>) -> impl IntoResponse {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, format!("status {code}"))
}

async fn json_body() -> impl IntoResponse {
    Json(serde_json::json!({ "id": 42, "name": "relay", "nested": { "ok": true } }))
}

async fn text_body() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "hello ✓",
    )
}

async fn binary_body() -> impl IntoResponse {
    // A PNG header: valid bytes, definitely not text.
    let bytes = vec![0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x00, 0x1A];
    ([(header::CONTENT_TYPE, "image/png")], bytes)
}

async fn latin1_body() -> impl IntoResponse {
    // "café" in ISO-8859-1; invalid UTF-8, so the charset must be honoured.
    let bytes = vec![0x63u8, 0x61, 0x66, 0xE9];
    (
        [(header::CONTENT_TYPE, "text/plain; charset=iso-8859-1")],
        bytes,
    )
}

async fn empty_body() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

/// Holds the response open, so timeout and cancellation can be tested.
async fn slow_body() -> impl IntoResponse {
    tokio::time::sleep(Duration::from_secs(30)).await;
    "too late"
}

/// Streams `count` chunks of 1 KiB, slowly enough to cancel mid-flight.
async fn stream_body(Path(count): Path<usize>) -> impl IntoResponse {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
    tokio::spawn(async move {
        for _ in 0..count {
            tokio::time::sleep(Duration::from_millis(5)).await;
            if tx
                .send(Ok(axum::body::Bytes::from(vec![b'x'; 1024])))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
        .unwrap()
}

/// Redirects `n` times, then returns 200.
async fn redirect_chain(Path(n): Path<u32>) -> Response {
    if n == 0 {
        return (StatusCode::OK, "arrived").into_response();
    }
    (
        StatusCode::FOUND,
        [(header::LOCATION, format!("/redirect/{}", n - 1))],
    )
        .into_response()
}

async fn redirect_loop() -> impl IntoResponse {
    (StatusCode::FOUND, [(header::LOCATION, "/redirect-loop")])
}

async fn set_cookie() -> impl IntoResponse {
    (
        [(
            header::SET_COOKIE,
            "sid=abc123; Path=/; HttpOnly; SameSite=Lax",
        )],
        "cookie set",
    )
}

async fn read_cookie(headers: HeaderMap) -> impl IntoResponse {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("<none>")
        .to_string()
}
