//! Relay's native HTTP execution engine.
//!
//! Every request a user sends goes through this crate, never through the
//! webview's `fetch`. That is what allows arbitrary methods and headers,
//! proper cookie control, client certificates, streaming bodies, real
//! cancellation, and accurate timings — none of which a browser will permit.
//!
//! The engine takes an [`relay_models::HttpRequestSpec`] in which every
//! variable has already been resolved, and returns an
//! [`relay_models::HttpExchange`] describing what happened.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod build;
mod client;
mod engine;
mod error;
mod response;

pub use build::{PreparedBody, apply_query_auth, build_headers, prepare_body, resolve_url};
pub use client::ClientPool;
pub use engine::{EngineConfig, HttpEngine, NoopProgress, ProgressSink};
pub use error::EngineError;
pub use response::{
    DEFAULT_MAX_RESPONSE_BYTES, INLINE_BODY_LIMIT, SpoolTarget, decode_text, essence,
    is_textual_content_type,
};
