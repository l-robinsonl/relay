//! The assistant: Relay's own capabilities, offered to a model.

pub mod session;
pub mod tools;

pub use session::{Approvals, AskTheUser, AssistantEvent, EventSink, Forwarding, NoEvents};
pub use tools::registry;
