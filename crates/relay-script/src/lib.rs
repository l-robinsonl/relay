//! A sandboxed JavaScript runtime for request assertions.
//!
//! Scripts attached to a request run here after its response arrives.
//! The sandbox is QuickJS, chosen because it has no JIT — so no
//! executable pages are generated at runtime — and because its core has
//! no I/O at all. There is no `fetch`, no `require`, no filesystem and
//! no process: not because they are blocked, but because nothing ever
//! put them there.
//!
//! What a script *can* reach is a fixed set of plain data: the response,
//! a redacted copy of the request, and an explicit variable API. It
//! cannot read a stored secret, open a socket or touch a file.
//!
//! On top of that, three limits apply: a wall-clock deadline enforced by
//! an interrupt handler, a heap ceiling, and a stack ceiling. A script
//! that loops forever is stopped; one that allocates without bound is
//! stopped; neither takes the application with it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod runtime;
mod value;

pub use relay_models::RequestPatch;
pub use runtime::{ScriptEngine, ScriptLimits, ScriptPhase};
pub use value::{
    ConsoleLevel, ConsoleLine, ScriptOutcome, ScriptRequest, ScriptResponse, TestCase,
};
