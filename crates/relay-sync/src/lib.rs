//! Relay's synchronisation protocol.
//!
//! These types are the contract between the desktop client and any server —
//! the self-hosted SQLite one and the managed Postgres one alike. Both
//! depend on this crate, so the two cannot drift apart.
//!
//! # The model
//!
//! Each workspace on the server carries a monotonically increasing
//! `change_seq`. Every accepted mutation bumps it and stamps the affected
//! object with `rev = change_seq`. A client remembers the highest `rev` it
//! has seen.
//!
//! **Pulling** asks for everything with `rev > since`, in revision order,
//! tombstones included. **Pushing** submits mutations each carrying the
//! `base_rev` they were edited against; the server accepts a mutation only
//! if that still matches, and reports a conflict otherwise. Nothing is
//! overwritten on the strength of a timestamp, and nothing is discarded
//! silently.
//!
//! Both directions are idempotent. A pull is driven entirely by `since`, so
//! repeating it converges. A push carries a client-generated `mutation_id`
//! that the server deduplicates, so a batch re-sent after a dropped
//! connection applies once.

#![forbid(unsafe_code)]
// Every type and module here is documented. `missing_docs` is off for the
// same reason as in relay-models: these are wire-contract structs whose
// fields (`id`, `rev`, `deleted`, `email`) restate their own names, and
// commenting each one would bury the explanations that matter. The logic
// crates — relay-server among them — do enforce it.
#![warn(missing_debug_implementations)]

pub mod auth;
pub mod protocol;

pub use auth::{Invitation, LoginRequest, LoginResponse, Membership, Role, UserProfile};
pub use protocol::{
    ApplyResult, ChangePage, EntityKind, Mutation, MutationBatch, MutationOutcome, SyncError,
    SyncObject,
};

/// The protocol version this build speaks, as it appears in the URL path.
pub const PROTOCOL_VERSION: &str = "v1";

/// The largest mutation batch a server will accept, in bytes.
pub const MAX_BATCH_BYTES: usize = 2 * 1024 * 1024;

/// The largest single object document, in bytes.
pub const MAX_OBJECT_BYTES: usize = 256 * 1024;

/// How many objects a single pull returns at most.
pub const MAX_PAGE_SIZE: u32 = 500;
