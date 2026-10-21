//! Live change notifications.
//!
//! Holds a WebSocket to the server and reports when the workspace has
//! moved on, so the client can pull without polling.
//!
//! The socket is strictly an optimisation. Every notification is just a
//! revision number, and the watcher's only job is to say "something
//! changed". If it drops, lags, duplicates or never connects at all,
//! synchronisation is still correct — it merely waits for the next manual
//! sync. Nothing about correctness depends on this staying connected.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use relay_models::WorkspaceId;
use tokio::sync::mpsc;

/// How long to wait before the first reconnection attempt.
const BACKOFF_START: Duration = Duration::from_secs(1);

/// The longest gap between reconnection attempts.
///
/// A minute is frequent enough that a laptop waking from sleep rejoins
/// promptly, and rare enough that a server which is down for an hour is
/// not hammered by every client its users own.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// What the watcher reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEvent {
    /// Connected and listening.
    Connected,
    /// The workspace has advanced to this revision.
    Changed {
        /// The server's new head.
        seq: i64,
    },
    /// The connection dropped; a reconnection is scheduled.
    Disconnected,
}

/// A running watcher. Dropping it closes the socket.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").finish()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}

impl Watcher {
    /// Opens a watcher, reconnecting until it is dropped.
    ///
    /// Events go to `events`; when that receiver is dropped the watcher
    /// stops, so a closed workspace does not leave a socket behind.
    pub fn start(
        websocket_url: String,
        token: String,
        workspace: WorkspaceId,
        events: mpsc::Sender<(WorkspaceId, WatchEvent)>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);

        let task = tokio::spawn(async move {
            let mut backoff = BACKOFF_START;

            while !flag.load(Ordering::Relaxed) {
                match connect(&websocket_url, &token).await {
                    Ok(mut stream) => {
                        backoff = BACKOFF_START;
                        if events
                            .send((workspace.clone(), WatchEvent::Connected))
                            .await
                            .is_err()
                        {
                            return;
                        }

                        while let Some(message) = stream.next().await {
                            if flag.load(Ordering::Relaxed) {
                                return;
                            }

                            let Ok(message) = message else { break };
                            let Ok(text) = message.into_text() else {
                                continue;
                            };
                            let Some(seq) = parse_head(&text) else {
                                continue;
                            };

                            if events
                                .send((workspace.clone(), WatchEvent::Changed { seq }))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }

                        let _ = events
                            .send((workspace.clone(), WatchEvent::Disconnected))
                            .await;
                    }
                    Err(error) => {
                        tracing::debug!(%workspace, "sync watcher could not connect: {error}");
                    }
                }

                if flag.load(Ordering::Relaxed) {
                    return;
                }

                tokio::time::sleep(with_jitter(backoff)).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        });

        Self { stop, task }
    }

    /// Stops the watcher.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}

type Stream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str, token: &str) -> Result<Stream, String> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut request = url
        .into_client_request()
        .map_err(|e| format!("bad websocket url: {e}"))?;

    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| "the token is not a valid header value".to_string())?,
    );

    let (stream, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| e.to_string())?;

    Ok(stream)
}

/// Reads a `{"type":"head","seq":N}` notification.
///
/// Anything else is ignored rather than treated as an error: a future
/// server may send message types this build does not know, and a watcher
/// that disconnected over one would be worse than one that shrugged.
fn parse_head(text: &str) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("type")?.as_str()? != "head" {
        return None;
    }
    value.get("seq")?.as_i64()
}

/// Spreads reconnection attempts out.
///
/// Without jitter, every client of a server that restarts reconnects at
/// the same instant, which is how a recovering server gets knocked over
/// again.
fn with_jitter(base: Duration) -> Duration {
    let millis = base.as_millis() as u64;
    let spread = millis / 4;
    if spread == 0 {
        return base;
    }

    // Cheap, adequate randomness: this picks a delay, not a key.
    let noise = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
        % spread;

    Duration::from_millis(millis - spread / 2 + noise)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_notification_is_read() {
        assert_eq!(parse_head(r#"{"type":"head","seq":42}"#), Some(42));
    }

    #[test]
    fn unknown_message_types_are_ignored_not_fatal() {
        // A future server may send things this build does not know about.
        assert_eq!(parse_head(r#"{"type":"somethingNew","seq":1}"#), None);
        assert_eq!(parse_head(r#"{"type":"head"}"#), None);
        assert_eq!(parse_head("not json"), None);
        assert_eq!(parse_head(""), None);
    }

    #[test]
    fn jitter_stays_close_to_the_requested_delay() {
        let base = Duration::from_secs(8);
        for _ in 0..50 {
            let actual = with_jitter(base);
            assert!(actual >= Duration::from_secs(7), "{actual:?}");
            assert!(actual <= Duration::from_secs(9), "{actual:?}");
        }
    }

    #[test]
    fn jitter_does_not_underflow_a_tiny_delay() {
        assert_eq!(
            with_jitter(Duration::from_millis(1)),
            Duration::from_millis(1)
        );
        assert_eq!(with_jitter(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let mut backoff = BACKOFF_START;
        for _ in 0..20 {
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
        assert_eq!(backoff, BACKOFF_MAX);
    }
}
