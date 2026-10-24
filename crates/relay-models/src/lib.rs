//! Shared domain types for Relay.
//!
//! This crate is pure data. It performs no I/O, depends on no other Relay
//! crate, and is compiled into the desktop app, the sync server and the
//! TypeScript bindings alike — which is what keeps the wire format from
//! drifting between them.
//!
//! The TypeScript definitions in `packages/shared-types` are generated from
//! these types; run `cargo test -p relay-models export_bindings` to refresh
//! them.

#![forbid(unsafe_code)]
// Every type and module here carries a doc comment explaining what it is for.
// `missing_docs` is not enabled because these are wire-contract structs whose
// fields (`key`, `value`, `enabled`, `status`) are self-describing: 200-odd
// comments restating the field name would bury the explanations that matter.
// The logic crates — relay-http, relay-storage, relay-core — do enforce it.
#![warn(missing_debug_implementations)]

pub mod common;
pub mod error;
pub mod http;
pub mod ids;
pub mod request;
pub mod workspace;

pub use common::{KeyValue, Revision, Timestamp};
pub use error::{ErrorKind, RelayError, RelayResult};
pub use http::{
    ExecutionPhase, ExecutionProgress, HistoryEntry, HttpExchange, HttpRequestSpec,
    HttpResponseData, REDACTED, RedirectHop, RequestPatch, ResponseBody, ResponseCookie,
    ServerEvent, Timings, TransportError, TransportErrorKind,
};
pub use ids::{CollectionId, EnvironmentId, ExecutionId, HistoryId, RequestId, TabId, WorkspaceId};
pub use request::{
    ApiKeyLocation, AuthConfig, ClientAuthStyle, ClientCertificate, FormField, FormValue,
    HttpMethod, OAuth2Config, OAuth2Grant, ProxyConfig, RedirectPolicy, RequestBody, RequestDoc,
    RequestKind, RequestScripts, RequestSettings,
};
pub use workspace::{Collection, Environment, SyncConfig, TreeNode, Variable, Workspace};

/// The synchronisation protocol version this build speaks.
///
/// Sent as `/v1` in every sync URL. A breaking change to the object model
/// bumps this, and a server may support several at once.
pub const SYNC_PROTOCOL_VERSION: u32 = 1;
