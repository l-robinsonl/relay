//! Websockets.
//!
//! A connection rather than a call: both ends may speak at any time,
//! and it lasts until somebody closes it. Like server-sent events it
//! takes a built `HttpRequestSpec`, so the URL, headers, query
//! parameters and authentication all come along — a socket behind a
//! bearer token is the ordinary case.
//!
//! What this does *not* carry across from the HTTP engine is the proxy
//! and the TLS verification toggle. Both are refused loudly rather
//! than ignored: a setting that is silently dropped is worse than one
//! that is missing, because the person believes it took effect.

use futures_util::{SinkExt, StreamExt};
use relay_models::{HttpRequestSpec, StreamDirection, StreamMessage};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::EngineError;

/// Turns the request's URL into a websocket one.
///
/// `ws` and `wss` are accepted as written. `http` and `https` are
/// accepted too and upgraded, because that is what an API's
/// documentation usually prints and retyping the scheme to satisfy a
/// tool is the tool's fault. A bare host defaults to `wss`.
fn websocket_url(spec: &HttpRequestSpec) -> Result<Url, EngineError> {
    let raw = spec.url.trim();

    // Resolution — query parameters, the default scheme, the host
    // check — is the HTTP builder's, so a websocket URL is parsed by
    // exactly the same rules as every other. It only speaks http and
    // https, hence the round trip through them.
    let (as_http, secure) = match raw.split_once("://") {
        Some(("ws", rest)) => (format!("http://{rest}"), false),
        Some(("wss", rest)) => (format!("https://{rest}"), true),
        Some(("http", _)) => (raw.to_string(), false),
        Some((other, _)) if other != "https" => {
            return Err(EngineError::InvalidUrl(format!(
                "unsupported scheme '{other}' — a websocket URL is ws:// or wss://"
            )));
        }
        // https, or no scheme at all, which resolves to https.
        _ => (raw.to_string(), true),
    };

    let mut probe = spec.clone();
    probe.url = as_http;
    let mut url = crate::build::resolve_url(&probe)?;
    crate::build::apply_query_auth(&spec.auth, &mut url);

    url.set_scheme(if secure { "wss" } else { "ws" })
        .map_err(|()| EngineError::InvalidUrl(format!("{raw}: not a websocket URL")))?;
    Ok(url)
}

/// Settings the HTTP engine honours that this transport cannot.
///
/// Reported before connecting, so the person is told rather than left
/// to find out from a certificate error they had asked to ignore.
fn unsupported(spec: &HttpRequestSpec) -> Option<&'static str> {
    if !spec.settings.verify_tls {
        return Some(
            "turning off certificate verification is not supported for websockets yet — \
             the connection was not opened",
        );
    }
    if spec.settings.proxy.as_ref().is_some_and(|p| !p.url.trim().is_empty()) {
        return Some("a proxy is not supported for websockets yet — the connection was not opened");
    }
    None
}

/// Opens a websocket and pumps it until it closes.
///
/// `outbound` carries what the person types; every frame in either
/// direction is handed to `on_message`, including the ones we sent, so
/// the log reads as a conversation. `cancel` closes the socket
/// politely — a close frame, not a dropped TCP connection, because the
/// far end is entitled to know the difference.
pub async fn connect<F>(
    spec: &HttpRequestSpec,
    mut outbound: mpsc::UnboundedReceiver<String>,
    mut on_message: F,
    cancel: CancellationToken,
) -> Result<(), EngineError>
where
    F: FnMut(StreamMessage) + Send,
{
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    if let Some(reason) = unsupported(spec) {
        return Err(EngineError::Stream(reason.to_string()));
    }

    let url = websocket_url(spec)?;

    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| EngineError::InvalidUrl(format!("{url}: {e}")))?;

    // The handshake is an HTTP request, so the spec's headers and
    // authentication apply — minus the ones tungstenite computes,
    // which must not be second-guessed.
    let headers = crate::build::build_headers(spec)?;
    for (name, value) in &headers {
        if is_handshake_header(name.as_str()) {
            continue;
        }
        request.headers_mut().insert(name, value.clone());
    }

    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| EngineError::Stream(e.to_string()))?;

    let (mut writer, mut reader) = socket.split();

    loop {
        tokio::select! {
            // Biased so that a close requested while frames are
            // pouring in is acted on, rather than starved by a busy
            // socket.
            biased;

            () = cancel.cancelled() => {
                let _ = writer.send(Message::Close(None)).await;
                return Ok(());
            }

            sending = outbound.recv() => {
                let Some(text) = sending else {
                    // Nothing can send any more: the session is gone.
                    let _ = writer.send(Message::Close(None)).await;
                    return Ok(());
                };
                writer
                    .send(Message::text(text.clone()))
                    .await
                    .map_err(|e| EngineError::Stream(e.to_string()))?;
                on_message(StreamMessage::text(StreamDirection::Outbound, text));
            }

            frame = reader.next() => {
                let Some(frame) = frame else { return Ok(()) };
                let frame = frame.map_err(|e| EngineError::Stream(e.to_string()))?;

                match frame {
                    Message::Text(text) => {
                        on_message(StreamMessage::text(
                            StreamDirection::Inbound,
                            text.as_str(),
                        ));
                    }
                    Message::Binary(bytes) => {
                        on_message(StreamMessage::binary(StreamDirection::Inbound, &bytes));
                    }
                    // Tungstenite answers pings itself; showing them
                    // is how somebody diagnoses a socket that looks
                    // idle but is not.
                    Message::Ping(_) => on_message(named(StreamDirection::Inbound, "ping")),
                    Message::Pong(_) => on_message(named(StreamDirection::Inbound, "pong")),
                    Message::Close(frame) => {
                        let detail = frame
                            .map(|f| {
                                let reason = f.reason.trim();
                                if reason.is_empty() {
                                    format!("{}", f.code)
                                } else {
                                    format!("{} {reason}", f.code)
                                }
                            })
                            .unwrap_or_default();
                        let mut message = named(StreamDirection::Inbound, "close");
                        message.data = detail;
                        on_message(message);
                        return Ok(());
                    }
                    Message::Frame(_) => {}
                }
            }
        }
    }
}

/// A frame worth logging that carries nothing to show.
fn named(direction: StreamDirection, name: &str) -> StreamMessage {
    StreamMessage {
        direction,
        name: name.to_string(),
        data: String::new(),
        id: None,
        binary_bytes: None,
    }
}

/// Headers that belong to the handshake and are tungstenite's to set.
///
/// A user who pastes a captured request often brings these along, and
/// overwriting the computed `Sec-WebSocket-Key` with a stale one makes
/// the server reject the connection for no visible reason.
fn is_handshake_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection" | "upgrade" | "sec-websocket-key" | "sec-websocket-version" | "host"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_models::{HttpMethod, KeyValue, ProxyConfig};

    fn spec(url: &str) -> HttpRequestSpec {
        HttpRequestSpec::new(HttpMethod::Get, url)
    }

    #[test]
    fn ws_and_wss_are_taken_as_written() {
        assert_eq!(websocket_url(&spec("ws://example.com/s")).unwrap().as_str(), "ws://example.com/s");
        assert_eq!(
            websocket_url(&spec("wss://example.com/s")).unwrap().as_str(),
            "wss://example.com/s"
        );
    }

    #[test]
    fn http_urls_are_upgraded_rather_than_refused() {
        // What an API's documentation prints is usually https.
        assert_eq!(
            websocket_url(&spec("https://example.com/s")).unwrap().as_str(),
            "wss://example.com/s"
        );
        assert_eq!(
            websocket_url(&spec("http://localhost:8080/s")).unwrap().as_str(),
            "ws://localhost:8080/s"
        );
    }

    #[test]
    fn a_bare_host_defaults_to_the_secure_scheme() {
        assert_eq!(
            websocket_url(&spec("example.com/s")).unwrap().as_str(),
            "wss://example.com/s"
        );
    }

    #[test]
    fn query_parameters_are_applied_as_they_are_for_a_request() {
        let mut s = spec("wss://example.com/s");
        s.query_params = vec![KeyValue::new("token", "abc")];

        assert_eq!(
            websocket_url(&s).unwrap().as_str(),
            "wss://example.com/s?token=abc"
        );
    }

    #[test]
    fn another_scheme_is_named_in_the_message() {
        let error = websocket_url(&spec("ftp://example.com/s")).unwrap_err().to_string();
        assert!(error.contains("ftp"), "{error}");
        assert!(error.contains("ws://"), "{error}");
    }

    #[test]
    fn an_empty_url_is_refused() {
        assert!(websocket_url(&spec("   ")).is_err());
    }

    #[test]
    fn settings_this_transport_cannot_honour_are_refused_not_ignored() {
        // Connecting anyway would verify a certificate the person had
        // explicitly asked Relay not to check.
        let mut s = spec("wss://example.com/s");
        s.settings.verify_tls = false;
        assert!(unsupported(&s).is_some());

        let mut s = spec("wss://example.com/s");
        s.settings.proxy = Some(ProxyConfig {
            url: "http://127.0.0.1:8888".to_string(),
            username: None,
            password: None,
            bypass: Vec::new(),
        });
        assert!(unsupported(&s).is_some());

        assert!(unsupported(&spec("wss://example.com/s")).is_none());
    }

    #[test]
    fn the_handshake_headers_are_left_to_the_library() {
        for name in ["Connection", "upgrade", "Sec-WebSocket-Key", "Host"] {
            assert!(is_handshake_header(name), "{name} would be overwritten");
        }
        assert!(!is_handshake_header("authorization"));
        assert!(!is_handshake_header("origin"));
    }

    #[test]
    fn a_binary_frame_is_shown_as_hex_with_its_real_length() {
        let message = StreamMessage::binary(StreamDirection::Inbound, &[0x00, 0xff, 0x10]);

        assert_eq!(message.data, "00 ff 10");
        assert_eq!(message.binary_bytes, Some(3));
    }

    #[test]
    fn a_large_binary_frame_is_truncated_but_still_reports_its_size() {
        let bytes = vec![0xab; 1000];
        let message = StreamMessage::binary(StreamDirection::Inbound, &bytes);

        assert!(message.data.ends_with('…'));
        assert_eq!(message.binary_bytes, Some(1000));
    }
}
