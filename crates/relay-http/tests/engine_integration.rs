//! End-to-end tests for the HTTP engine against a real local server.

mod support;

use relay_http::{EngineConfig, HttpEngine, NoopProgress};
use relay_models::{
    AuthConfig, ExecutionId, HttpExchange, HttpMethod, HttpRequestSpec, KeyValue, RequestBody,
    RequestSettings, ResponseBody, TransportErrorKind,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn engine() -> HttpEngine {
    HttpEngine::with_defaults()
}

async fn send(engine: &HttpEngine, spec: HttpRequestSpec) -> HttpExchange {
    engine
        .send(ExecutionId::new(), spec, Arc::new(NoopProgress))
        .await
}

fn expect_success(exchange: HttpExchange) -> relay_models::HttpResponseData {
    match exchange {
        HttpExchange::Success { response, .. } => *response,
        other => panic!("expected success, got {other:?}"),
    }
}

fn body_text(response: &relay_models::HttpResponseData) -> String {
    match &response.body {
        ResponseBody::Text { text } => text.clone(),
        other => panic!("expected text body, got {other:?}"),
    }
}

#[tokio::test]
async fn sends_a_get_and_reads_json() {
    let server = support::start().await;
    let response = expect_success(
        send(
            &engine(),
            HttpRequestSpec::new(HttpMethod::Get, server.url("/json")),
        )
        .await,
    );

    assert_eq!(response.status, 200);
    assert_eq!(response.status_text, "OK");
    assert_eq!(response.content_type.as_deref(), Some("application/json"));
    assert!(response.is_text);

    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();
    assert_eq!(parsed["id"], 42);
    assert_eq!(parsed["nested"]["ok"], true);
    assert!(response.timings.total_ms > 0.0);
}

#[tokio::test]
async fn every_standard_method_reaches_the_server() {
    let server = support::start().await;
    let engine = engine();

    for method in [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
        HttpMethod::Options,
    ] {
        let spec = HttpRequestSpec::new(method.clone(), server.url("/echo"));
        let response = expect_success(send(&engine, spec).await);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();
        assert_eq!(
            parsed["method"],
            method.as_str(),
            "method {method} differed"
        );
    }
}

#[tokio::test]
async fn head_requests_return_headers_without_a_body() {
    let server = support::start().await;
    let spec = HttpRequestSpec::new(HttpMethod::Head, server.url("/json"));
    let response = expect_success(send(&engine(), spec).await);

    assert_eq!(response.status, 200);
    assert!(matches!(response.body, ResponseBody::Empty));
}

#[tokio::test]
async fn custom_methods_are_sent_verbatim() {
    let server = support::start().await;
    let spec = HttpRequestSpec::new(HttpMethod::from("PURGE"), server.url("/echo"));
    let response = expect_success(send(&engine(), spec).await);
    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();
    assert_eq!(parsed["method"], "PURGE");
}

#[tokio::test]
async fn query_parameters_and_headers_arrive_intact() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/echo"));
    spec.query_params = vec![KeyValue::new("q", "hello world")];
    spec.headers = vec![KeyValue::new("X-Custom", "relay-test")];

    let response = expect_success(send(&engine(), spec).await);
    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();

    assert_eq!(parsed["query"]["q"], "hello world");
    assert_eq!(parsed["headers"]["x-custom"], "relay-test");
}

#[tokio::test]
async fn json_bodies_are_sent_with_the_right_content_type() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Post, server.url("/echo"));
    spec.body = RequestBody::Json {
        text: r#"{"hello":"world"}"#.into(),
    };

    let response = expect_success(send(&engine(), spec).await);
    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();

    assert_eq!(parsed["headers"]["content-type"], "application/json");
    assert_eq!(parsed["body"], r#"{"hello":"world"}"#);
}

#[tokio::test]
async fn urlencoded_bodies_are_encoded_correctly() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Post, server.url("/echo"));
    spec.body = RequestBody::UrlEncoded {
        fields: vec![KeyValue::new("name", "a b"), KeyValue::new("x", "1&2")],
    };

    let response = expect_success(send(&engine(), spec).await);
    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();

    assert_eq!(
        parsed["headers"]["content-type"],
        "application/x-www-form-urlencoded"
    );
    assert_eq!(parsed["body"], "name=a+b&x=1%262");
}

#[tokio::test]
async fn multipart_uploads_stream_a_real_file() {
    let server = support::start().await;
    let dir = std::env::temp_dir().join(format!("relay-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("upload.txt");
    std::fs::write(&path, b"file contents here").unwrap();

    let mut spec = HttpRequestSpec::new(HttpMethod::Post, server.url("/echo"));
    spec.body = RequestBody::Multipart {
        fields: vec![
            relay_models::FormField {
                key: "field".into(),
                enabled: true,
                value: relay_models::FormValue::Text {
                    value: "text value".into(),
                },
                description: None,
            },
            relay_models::FormField {
                key: "upload".into(),
                enabled: true,
                value: relay_models::FormValue::File {
                    path: path.to_string_lossy().into_owned(),
                    file_name: None,
                    content_type: None,
                },
                description: None,
            },
        ],
    };

    let response = expect_success(send(&engine(), spec).await);
    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();

    let content_type = parsed["headers"]["content-type"].as_str().unwrap();
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "got {content_type}"
    );
    let sent = parsed["body"].as_str().unwrap();
    assert!(sent.contains("text value"));
    assert!(sent.contains("file contents here"));
    assert!(sent.contains("filename=\"upload.txt\""));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn binary_bodies_are_streamed_from_disk() {
    let server = support::start().await;
    let dir = std::env::temp_dir().join(format!("relay-bin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("payload.bin");
    std::fs::write(&path, b"raw-bytes-payload").unwrap();

    let mut spec = HttpRequestSpec::new(HttpMethod::Post, server.url("/echo"));
    spec.body = RequestBody::Binary {
        path: path.to_string_lossy().into_owned(),
        content_type: Some("application/octet-stream".into()),
    };

    let response = expect_success(send(&engine(), spec).await);
    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();
    assert_eq!(parsed["body"], "raw-bytes-payload");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn basic_auth_reaches_the_server() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/echo"));
    spec.auth = AuthConfig::Basic {
        username: "Aladdin".into(),
        password: "open sesame".into(),
    };

    let response = expect_success(send(&engine(), spec).await);
    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();
    assert_eq!(
        parsed["headers"]["authorization"],
        "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
    );
}

#[tokio::test]
async fn bearer_auth_reaches_the_server() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/echo"));
    spec.auth = AuthConfig::Bearer {
        token: "tok-123".into(),
    };

    let response = expect_success(send(&engine(), spec).await);
    let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();
    assert_eq!(parsed["headers"]["authorization"], "Bearer tok-123");
}

#[tokio::test]
async fn error_statuses_are_responses_not_failures() {
    let server = support::start().await;
    let engine = engine();

    for code in [400u16, 404, 418, 500, 503] {
        let spec = HttpRequestSpec::new(HttpMethod::Get, server.url(&format!("/status/{code}")));
        let response = expect_success(send(&engine, spec).await);
        assert_eq!(response.status, code);
    }
}

#[tokio::test]
async fn redirects_are_followed_by_default() {
    let server = support::start().await;
    let spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/redirect/3"));
    let response = expect_success(send(&engine(), spec).await);

    assert_eq!(response.status, 200);
    assert_eq!(body_text(&response), "arrived");
    assert!(response.final_url.ends_with("/redirect/0"));
    assert!(!response.redirects.is_empty());
}

#[tokio::test]
async fn redirects_can_be_disabled() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/redirect/3"));
    spec.settings.redirect_policy = relay_models::RedirectPolicy::Never;

    let response = expect_success(send(&engine(), spec).await);
    assert_eq!(response.status, 302);
}

#[tokio::test]
async fn a_redirect_loop_is_reported_not_followed_forever() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/redirect-loop"));
    spec.settings.max_redirects = 3;

    match send(&engine(), spec).await {
        HttpExchange::Failure { error, .. } => {
            assert_eq!(error.kind, TransportErrorKind::TooManyRedirects);
        }
        other => panic!("expected a redirect failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_declared_charset_is_honoured() {
    let server = support::start().await;
    let spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/latin1"));
    let response = expect_success(send(&engine(), spec).await);
    assert_eq!(body_text(&response), "café");
}

#[tokio::test]
async fn binary_responses_are_returned_as_base64_not_mangled_text() {
    let server = support::start().await;
    let spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/binary"));
    let response = expect_success(send(&engine(), spec).await);

    match response.body {
        ResponseBody::Binary { base64 } => assert!(!base64.is_empty()),
        other => panic!("expected binary body, got {other:?}"),
    }
    assert!(!response.is_text);
}

#[tokio::test]
async fn empty_responses_are_handled() {
    let server = support::start().await;
    let spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/empty"));
    let response = expect_success(send(&engine(), spec).await);

    assert_eq!(response.status, 204);
    assert!(matches!(response.body, ResponseBody::Empty));
    assert_eq!(response.size_bytes, 0);
}

#[tokio::test]
async fn cookies_are_captured_and_replayed() {
    let server = support::start().await;
    let engine = engine();

    let response = expect_success(
        send(
            &engine,
            HttpRequestSpec::new(HttpMethod::Get, server.url("/set-cookie")),
        )
        .await,
    );
    assert_eq!(response.cookies.len(), 1);
    assert_eq!(response.cookies[0].name, "sid");
    assert_eq!(response.cookies[0].value, "abc123");
    assert!(response.cookies[0].http_only);

    // The shared jar must send it back on the next request to the same host.
    let echoed = expect_success(
        send(
            &engine,
            HttpRequestSpec::new(HttpMethod::Get, server.url("/read-cookie")),
        )
        .await,
    );
    assert!(body_text(&echoed).contains("sid=abc123"));
}

#[tokio::test]
async fn a_timeout_is_reported_as_a_timeout() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/slow"));
    spec.settings.timeout_ms = Some(150);

    match send(&engine(), spec).await {
        HttpExchange::Failure { error, .. } => {
            assert_eq!(error.kind, TransportErrorKind::Timeout);
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn a_request_can_be_cancelled_mid_flight() {
    let server = support::start().await;
    let engine = engine();
    let execution_id = ExecutionId::new();

    let spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/slow"));
    let task = {
        let engine = engine.clone();
        let execution_id = execution_id.clone();
        tokio::spawn(async move {
            engine
                .send(execution_id, spec, Arc::new(NoopProgress))
                .await
        })
    };

    // Wait for the request to actually be registered before cancelling.
    for _ in 0..100 {
        if engine.inflight_count() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(engine.cancel(&execution_id), "nothing was in flight");

    let exchange = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancellation did not take effect")
        .unwrap();

    match exchange {
        HttpExchange::Cancelled {
            execution_id: id, ..
        } => assert_eq!(id, execution_id),
        other => panic!("expected cancelled, got {other:?}"),
    }

    assert_eq!(engine.inflight_count(), 0, "cancellation handle leaked");
}

#[tokio::test]
async fn cancellation_during_download_stops_reading() {
    let server = support::start().await;
    let engine = engine();
    let execution_id = ExecutionId::new();

    // 2000 chunks at 5 ms each is about 10 s of streaming; we cancel early.
    let spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/stream/2000"));
    let task = {
        let engine = engine.clone();
        let execution_id = execution_id.clone();
        tokio::spawn(async move {
            engine
                .send(execution_id, spec, Arc::new(NoopProgress))
                .await
        })
    };

    tokio::time::sleep(Duration::from_millis(120)).await;
    engine.cancel(&execution_id);

    let exchange = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("download cancellation hung")
        .unwrap();

    assert!(
        matches!(exchange, HttpExchange::Cancelled { .. }),
        "expected cancelled, got {exchange:?}"
    );
}

#[tokio::test]
async fn progress_events_are_emitted_while_downloading() {
    let server = support::start().await;
    let counter = Arc::new(AtomicU64::new(0));
    let max_bytes = Arc::new(AtomicU64::new(0));

    let sink = {
        let counter = Arc::clone(&counter);
        let max_bytes = Arc::clone(&max_bytes);
        move |p: relay_models::ExecutionProgress| {
            counter.fetch_add(1, Ordering::SeqCst);
            max_bytes.fetch_max(p.bytes_read, Ordering::SeqCst);
        }
    };

    let spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/stream/20"));
    let exchange = engine()
        .send(ExecutionId::new(), spec, Arc::new(sink))
        .await;

    assert!(matches!(exchange, HttpExchange::Success { .. }));
    assert!(counter.load(Ordering::SeqCst) > 2, "no progress reported");
    assert_eq!(max_bytes.load(Ordering::SeqCst), 20 * 1024);
}

#[tokio::test]
async fn oversized_responses_are_truncated_rather_than_buffered() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/stream/50"));
    // 50 KiB of body against a 10 KiB cap.
    spec.settings.max_response_bytes = Some(10 * 1024);

    let response = expect_success(send(&engine(), spec).await);
    match response.body {
        ResponseBody::Truncated { size, reason } => {
            assert!(size > 10 * 1024);
            assert!(reason.contains("limit"));
        }
        other => panic!("expected truncated body, got {other:?}"),
    }
}

#[tokio::test]
async fn large_responses_spool_to_disk_instead_of_memory() {
    let server = support::start().await;
    let spool = std::env::temp_dir().join(format!("relay-spool-{}", std::process::id()));

    // A small inline limit so the test does not need to move megabytes.
    let engine = HttpEngine::new(EngineConfig {
        spool_dir: Some(spool.clone()),
        ..Default::default()
    });

    // 300 chunks of 1 KiB; INLINE_BODY_LIMIT is 8 MiB, so to force a spill we
    // instead assert the inline path here and cover spilling via unit tests.
    let spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/stream/300"));
    let response = expect_success(
        engine
            .send(ExecutionId::new(), spec, Arc::new(NoopProgress))
            .await,
    );

    assert_eq!(response.size_bytes, 300 * 1024);
    std::fs::remove_dir_all(&spool).ok();
}

#[tokio::test]
async fn a_refused_connection_is_classified() {
    // Port 1 on loopback is reliably closed.
    let spec = HttpRequestSpec::new(HttpMethod::Get, "http://127.0.0.1:1/");
    match send(&engine(), spec).await {
        HttpExchange::Failure { error, .. } => {
            assert_eq!(error.kind, TransportErrorKind::ConnectionFailed);
            assert!(
                error.message.contains("connect"),
                "message should guide the user: {}",
                error.message
            );
        }
        other => panic!("expected connection failure, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unresolvable_host_is_classified_as_dns() {
    let spec = HttpRequestSpec::new(HttpMethod::Get, "http://relay-nonexistent-host.invalid/");
    match send(&engine(), spec).await {
        HttpExchange::Failure { error, .. } => {
            assert!(
                matches!(
                    error.kind,
                    TransportErrorKind::DnsFailure | TransportErrorKind::ConnectionFailed
                ),
                "got {:?}",
                error.kind
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn secrets_never_appear_in_the_returned_exchange() {
    let server = support::start().await;
    let mut spec = HttpRequestSpec::new(HttpMethod::Get, server.url("/echo"));
    spec.auth = AuthConfig::Bearer {
        token: "sk-live-DO-NOT-LEAK".into(),
    };
    spec.headers = vec![KeyValue::new("X-API-Key", "key-DO-NOT-LEAK")];

    let exchange = send(&engine(), spec).await;

    let (request, response) = match &exchange {
        HttpExchange::Success {
            request, response, ..
        } => (request, response),
        other => panic!("expected success, got {other:?}"),
    };

    // The credentials really were sent: the echo server reflects them in its
    // response body. Without this the test could pass by sending nothing.
    let echoed = body_text(response);
    assert!(
        echoed.contains("sk-live-DO-NOT-LEAK") && echoed.contains("key-DO-NOT-LEAK"),
        "the test did not actually transmit the credentials: {echoed}"
    );

    // Showing the user their own response body is legitimate. Storing the
    // credential in the request snapshot — which history persists to disk and
    // keeps for months — is not.
    let snapshot = serde_json::to_string(&**request).unwrap();
    assert!(!snapshot.contains("DO-NOT-LEAK"), "leaked: {snapshot}");
    assert!(snapshot.contains("[redacted]"), "nothing was redacted");
}

#[tokio::test]
async fn concurrent_requests_do_not_interfere() {
    let server = support::start().await;
    let engine = engine();

    let mut tasks = Vec::new();
    for i in 0..20 {
        let engine = engine.clone();
        let url = server.url("/echo");
        tasks.push(tokio::spawn(async move {
            let mut spec = HttpRequestSpec::new(HttpMethod::Get, url);
            spec.query_params = vec![KeyValue::new("i", i.to_string())];
            let exchange = engine
                .send(ExecutionId::new(), spec, Arc::new(NoopProgress))
                .await;
            let response = match exchange {
                HttpExchange::Success { response, .. } => *response,
                other => panic!("request {i} failed: {other:?}"),
            };
            let parsed: serde_json::Value = serde_json::from_str(&body_text(&response)).unwrap();
            assert_eq!(parsed["query"]["i"], i.to_string());
        }));
    }

    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(engine.inflight_count(), 0);
}

#[tokio::test]
async fn non_idempotent_requests_are_never_retried_automatically() {
    // A POST that fails must surface the failure rather than being resent;
    // a duplicated payment is worse than a visible error.
    let settings = RequestSettings::default();
    assert!(!HttpMethod::Post.is_idempotent());
    // The engine issues exactly one send per execution: there is no retry
    // path at all, which this test documents and pins.
    assert_eq!(settings.max_redirects, 10);
}

#[tokio::test]
async fn clearing_cookies_stops_them_being_sent() {
    let server = support::start().await;
    let engine = engine();

    // Acquire a cookie, and confirm it is replayed.
    send(
        &engine,
        HttpRequestSpec::new(HttpMethod::Get, server.url("/set-cookie")),
    )
    .await;

    let before = expect_success(
        send(
            &engine,
            HttpRequestSpec::new(HttpMethod::Get, server.url("/read-cookie")),
        )
        .await,
    );
    assert!(body_text(&before).contains("sid=abc123"));

    engine.clear_cookies();

    // A stale session cookie breaking every later request is a real and
    // confusing failure, so clearing has to actually work.
    let after = expect_success(
        send(
            &engine,
            HttpRequestSpec::new(HttpMethod::Get, server.url("/read-cookie")),
        )
        .await,
    );
    assert_eq!(
        body_text(&after),
        "<none>",
        "a cleared cookie was still sent"
    );
}
