//! Relay domain logic.
//!
//! This crate sits between the stored documents a user edits and the wire
//! requests the engine sends. It resolves variables, merges inherited
//! settings, and converts to and from the formats people exchange requests in.
//!
//! It depends on no database, no HTTP client and no Tauri, which is what makes
//! it cheap to test exhaustively.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod curl;
mod error;
pub mod interpolate;
pub mod resolve;

pub use curl::{export_curl, import_curl};
pub use error::{CoreError, CoreResult};
pub use interpolate::{
    Binding, Interpolated, Scope, VariableContext, interpolate, referenced_names,
};
pub use resolve::{Resolution, ResolutionInput, build_context, resolve};
