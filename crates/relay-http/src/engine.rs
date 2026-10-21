//! The HTTP execution engine.

use relay_models::{
    ExecutionId, ExecutionPhase, ExecutionProgress, HttpExchange, HttpRequestSpec,
    HttpResponseData, RedirectHop, Timings,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::build::{self, PreparedBody};
use crate::client::ClientPool;
use crate::error::EngineError;
use crate::response::{self, DEFAULT_MAX_RESPONSE_BYTES, SpoolTarget};

/// Receives progress events while a request runs.
///
/// A trait rather than a channel so the engine can be driven from tests with
/// no runtime plumbing, and from Tauri by forwarding to the webview.
pub trait ProgressSink: Send + Sync + 'static {
    /// Called as the request advances. Must not block.
    fn emit(&self, progress: ExecutionProgress);
}

/// A sink that discards everything.
pub struct NoopProgress;

impl ProgressSink for NoopProgress {
    fn emit(&self, _progress: ExecutionProgress) {}
}

impl<F> ProgressSink for F
where
    F: Fn(ExecutionProgress) + Send + Sync + 'static,
{
    fn emit(&self, progress: ExecutionProgress) {
        self(progress)
    }
}

/// Configuration for the engine.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Applied when a request does not set its own timeout.
    pub default_timeout: Duration,
    /// Ceiling on a response body when the request does not set one.
    pub default_max_response_bytes: u64,
    /// Where oversized response bodies are spooled.
    pub spool_dir: Option<std::path::PathBuf>,
    /// Sent as `User-Agent` unless the request overrides it.
    pub user_agent: String,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
            default_max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            spool_dir: None,
            user_agent: concat!("Relay/", env!("CARGO_PKG_VERSION")).to_string(),
        }
    }
}

/// Executes HTTP requests and tracks the ones in flight so they can be
/// cancelled.
#[derive(Clone)]
pub struct HttpEngine {
    pool: ClientPool,
    config: Arc<EngineConfig>,
    /// Cancellation handles for in-flight executions, keyed by execution id.
    inflight: Arc<Mutex<HashMap<ExecutionId, CancellationToken>>>,
}

impl HttpEngine {
    /// Builds an engine with the given configuration.
    pub fn new(config: EngineConfig) -> Self {
        Self {
            pool: ClientPool::new(config.default_timeout, config.user_agent.clone()),
            config: Arc::new(config),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Builds an engine with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(EngineConfig::default())
    }

    /// Cancels an in-flight execution. Returns true if one was running.
    ///
    /// Cancellation drops the response stream, which closes the connection, so
    /// the remote stops sending and no further bytes are buffered.
    pub fn cancel(&self, execution_id: &ExecutionId) -> bool {
        let token = self
            .inflight
            .lock()
            .ok()
            .and_then(|mut guard| guard.remove(execution_id));

        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// The number of executions currently in flight.
    pub fn inflight_count(&self) -> usize {
        self.inflight.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Clears pooled connections and cached clients.
    pub fn reset_connections(&self) {
        self.pool.clear();
    }

    /// Executes a fully resolved request.
    ///
    /// Always returns an [`HttpExchange`] rather than an `Err`: a 500 response
    /// and a DNS failure are both outcomes the UI must render, and collapsing
    /// them into `Result` would push that distinction into the caller.
    pub async fn send(
        &self,
        execution_id: ExecutionId,
        spec: HttpRequestSpec,
        progress: Arc<dyn ProgressSink>,
    ) -> HttpExchange {
        let token = CancellationToken::new();
        if let Ok(mut guard) = self.inflight.lock() {
            guard.insert(execution_id.clone(), token.clone());
        }

        let started = Instant::now();
        let result = self
            .execute(&execution_id, &spec, progress, token.clone())
            .await;

        if let Ok(mut guard) = self.inflight.lock() {
            guard.remove(&execution_id);
        }

        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        match result {
            Ok(response) => HttpExchange::Success {
                execution_id,
                request: Box::new(spec.redacted()),
                response: Box::new(response),
            },
            Err(err) if token.is_cancelled() => {
                tracing::debug!(%execution_id, "execution cancelled: {err}");
                HttpExchange::Cancelled {
                    execution_id,
                    elapsed_ms,
                }
            }
            Err(err) => {
                // The spec is redacted before it is attached, so an error
                // surfaced in the UI or written to history carries no secrets.
                tracing::debug!(%execution_id, "execution failed: {err}");
                HttpExchange::Failure {
                    execution_id,
                    request: Box::new(spec.redacted()),
                    error: err.to_transport_error(),
                    elapsed_ms,
                }
            }
        }
    }

    async fn execute(
        &self,
        execution_id: &ExecutionId,
        spec: &HttpRequestSpec,
        progress: Arc<dyn ProgressSink>,
        token: CancellationToken,
    ) -> Result<HttpResponseData, EngineError> {
        let emit = |phase: ExecutionPhase, bytes: u64| {
            progress.emit(ExecutionProgress {
                execution_id: execution_id.clone(),
                phase,
                bytes_read: bytes,
                total_bytes: None,
            });
        };

        emit(ExecutionPhase::Preparing, 0);

        let mut url = build::resolve_url(spec)?;
        build::apply_query_auth(&spec.auth, &mut url);
        let headers = build::build_headers(spec)?;
        let body = build::prepare_body(&spec.body).await?;

        let client = self.pool.get(&spec.settings)?;
        let method = reqwest::Method::from_bytes(spec.method.as_str().as_bytes())
            .map_err(|e| EngineError::InvalidUrl(format!("invalid HTTP method: {e}")))?;

        let mut builder = client.request(method, url.clone()).headers(headers);

        builder = match body {
            PreparedBody::None => builder,
            PreparedBody::Bytes(bytes) => builder.body(bytes),
            PreparedBody::Multipart(form) => builder.multipart(*form),
            PreparedBody::File { path, len } => {
                let file = tokio::fs::File::open(&path).await.map_err(|e| {
                    EngineError::RequestBody(format!("cannot open {}: {e}", path.display()))
                })?;
                let stream = tokio_util::io::ReaderStream::new(file);
                builder
                    .header(reqwest::header::CONTENT_LENGTH, len)
                    .body(reqwest::Body::wrap_stream(stream))
            }
        };

        emit(ExecutionPhase::Sending, 0);

        let timeout = spec
            .settings
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(self.config.default_timeout);

        let send_started = Instant::now();

        // Three outcomes race: the response, the deadline, and the user
        // pressing Cancel. Whichever wins, the other futures are dropped,
        // which tears down the connection and frees its buffers.
        let response = tokio::select! {
            biased;

            () = token.cancelled() => {
                return Err(EngineError::RequestBody("cancelled".into()));
            }
            result = tokio::time::timeout(timeout, builder.send()) => {
                match result {
                    Ok(Ok(response)) => response,
                    Ok(Err(e)) => return Err(EngineError::Transport(e)),
                    Err(_) => return Err(EngineError::Timeout(timeout.as_millis() as u64)),
                }
            }
        };

        let ttfb_ms = send_started.elapsed().as_secs_f64() * 1000.0;
        emit(ExecutionPhase::Downloading, 0);

        let status = response.status();
        let http_version = format!("{:?}", response.version());
        let final_url = response.url().to_string();
        let header_map = response.headers().clone();
        let content_type = header_map
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let redirects = redirect_hops(&url, &final_url);

        let max_bytes = spec
            .settings
            .max_response_bytes
            .unwrap_or(self.config.default_max_response_bytes);

        let spool = self.config.spool_dir.as_ref().map(|dir| SpoolTarget {
            dir: dir.clone(),
            stem: execution_id.to_string(),
        });

        let download_started = Instant::now();

        let progress_for_body = Arc::clone(&progress);
        let exec_for_body = execution_id.clone();
        let read = response::read_body(
            response,
            max_bytes,
            spool.as_ref(),
            content_type.as_deref(),
            move |bytes| {
                progress_for_body.emit(ExecutionProgress {
                    execution_id: exec_for_body.clone(),
                    phase: ExecutionPhase::Downloading,
                    bytes_read: bytes,
                    total_bytes: None,
                });
            },
        );

        let (body, size_bytes) = tokio::select! {
            biased;

            () = token.cancelled() => {
                return Err(EngineError::RequestBody("cancelled".into()));
            }
            result = read => result?,
        };

        let download_ms = download_started.elapsed().as_secs_f64() * 1000.0;
        emit(ExecutionPhase::Done, size_bytes);

        let is_text = matches!(body, relay_models::ResponseBody::Text { .. });

        Ok(HttpResponseData {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("Unknown").to_string(),
            http_version,
            headers: response::collect_headers(&header_map),
            cookies: response::collect_cookies(&header_map),
            body,
            size_bytes,
            decoded_size_bytes: None,
            timings: Timings {
                total_ms: send_started.elapsed().as_secs_f64() * 1000.0,
                time_to_first_byte_ms: ttfb_ms,
                download_ms,
            },
            redirects,
            final_url,
            content_type: response::essence(content_type.as_deref()),
            is_text,
        })
    }
}

/// Records that a redirect happened, when the final URL differs from the sent
/// one. `reqwest` follows redirects internally and does not expose each hop,
/// so this reports the net effect rather than inventing detail it does not have.
fn redirect_hops(requested: &url::Url, final_url: &str) -> Vec<RedirectHop> {
    if requested.as_str() == final_url {
        Vec::new()
    } else {
        vec![RedirectHop {
            status: 0,
            url: requested.to_string(),
            location: final_url.to_string(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_models::HttpMethod;

    #[tokio::test]
    async fn cancelling_an_unknown_execution_reports_false() {
        let engine = HttpEngine::with_defaults();
        assert!(!engine.cancel(&ExecutionId::from("never-started")));
    }

    #[tokio::test]
    async fn an_invalid_url_fails_without_touching_the_network() {
        let engine = HttpEngine::with_defaults();
        let spec = HttpRequestSpec::new(HttpMethod::Get, "file:///etc/passwd");
        let exchange = engine
            .send(ExecutionId::new(), spec, Arc::new(NoopProgress))
            .await;

        match exchange {
            HttpExchange::Failure { error, .. } => {
                assert_eq!(error.kind, relay_models::TransportErrorKind::InvalidUrl);
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_inflight_table_is_emptied_after_a_request() {
        let engine = HttpEngine::with_defaults();
        let spec = HttpRequestSpec::new(HttpMethod::Get, "");
        let _ = engine
            .send(ExecutionId::new(), spec, Arc::new(NoopProgress))
            .await;
        assert_eq!(engine.inflight_count(), 0, "cancellation handle leaked");
    }
}
