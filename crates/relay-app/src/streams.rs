//! Connections that stay open.
//!
//! An ordinary request answers once and is done, so the service can
//! hand back a value and forget it. A stream does not: it runs until
//! the server stops or the person does, and in the meantime the
//! interface needs to be told things. So a stream is a task with a
//! handle, and the handle is how it gets closed.
//!
//! One registry for the application rather than one per window: the
//! command that closes a stream arrives on its own, with an id to go
//! on, and ids are unique.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use relay_models::{ExecutionId, ServerEvent};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

/// Something that happened on a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum StreamEvent {
    /// The connection is up.
    #[serde(rename_all = "camelCase")]
    Open {
        /// Which stream, so the right tab shows it.
        execution_id: ExecutionId,
    },
    /// An event arrived.
    #[serde(rename_all = "camelCase")]
    Message {
        /// Which stream it came from.
        execution_id: ExecutionId,
        /// The event itself, as the server sent it.
        event: ServerEvent,
        /// When it arrived, so the log can be read as a timeline.
        #[ts(type = "number")]
        at: i64,
    },
    /// The connection ended, by either side.
    #[serde(rename_all = "camelCase")]
    Closed {
        /// Which stream ended.
        execution_id: ExecutionId,
        /// Why, when it was not simply the far end finishing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Where stream events go.
pub trait StreamSink: Send + Sync + 'static {
    /// Called for each event, on the task's thread.
    fn emit(&self, event: StreamEvent);
}

/// A sink that drops everything, for tests and for headless use.
pub struct NoStreamEvents;

impl StreamSink for NoStreamEvents {
    fn emit(&self, _event: StreamEvent) {}
}

/// The streams currently open.
#[derive(Clone, Default)]
pub struct Streams {
    open: Arc<Mutex<HashMap<ExecutionId, CancellationToken>>>,
}

impl std::fmt::Debug for Streams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Streams")
            .field("open", &self.count())
            .finish()
    }
}

impl Streams {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a stream, returning the token that closes it.
    pub fn register(&self, id: ExecutionId) -> CancellationToken {
        let token = CancellationToken::new();
        if let Ok(mut open) = self.open.lock() {
            // Replacing an id closes whatever was there, rather than
            // leaking a task nothing can reach any more.
            if let Some(previous) = open.insert(id, token.clone()) {
                previous.cancel();
            }
        }
        token
    }

    /// Closes one, if it is open. Says whether it was.
    pub fn close(&self, id: &ExecutionId) -> bool {
        match self.open.lock() {
            Ok(mut open) => match open.remove(id) {
                Some(token) => {
                    token.cancel();
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }

    /// Forgets one without cancelling, for a task that has ended on
    /// its own.
    pub fn forget(&self, id: &ExecutionId) {
        if let Ok(mut open) = self.open.lock() {
            open.remove(id);
        }
    }

    /// Closes every stream. Called when a workspace closes.
    pub fn close_all(&self) {
        if let Ok(mut open) = self.open.lock() {
            for (_, token) in open.drain() {
                token.cancel();
            }
        }
    }

    /// How many are open.
    pub fn count(&self) -> usize {
        self.open.lock().map(|open| open.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_cancels_the_token_the_task_is_watching() {
        let streams = Streams::new();
        let id = ExecutionId::new();
        let token = streams.register(id.clone());

        assert!(!token.is_cancelled());
        assert!(streams.close(&id));
        assert!(token.is_cancelled());
    }

    #[test]
    fn closing_something_that_is_not_open_says_so() {
        let streams = Streams::new();
        assert!(!streams.close(&ExecutionId::new()));
    }

    #[test]
    fn reusing_an_id_closes_what_was_there() {
        // Otherwise the first task runs on with nothing able to
        // reach it, writing into a log nobody is reading.
        let streams = Streams::new();
        let id = ExecutionId::new();

        let first = streams.register(id.clone());
        let second = streams.register(id.clone());

        assert!(first.is_cancelled(), "the replaced stream was left running");
        assert!(!second.is_cancelled());
        assert_eq!(streams.count(), 1);
    }

    #[test]
    fn a_task_that_ended_is_forgotten_without_being_cancelled() {
        let streams = Streams::new();
        let id = ExecutionId::new();
        let token = streams.register(id.clone());

        streams.forget(&id);

        assert_eq!(streams.count(), 0);
        assert!(!token.is_cancelled());
        // And closing it afterwards is a no-op rather than a panic.
        assert!(!streams.close(&id));
    }

    #[test]
    fn closing_everything_leaves_nothing_running() {
        let streams = Streams::new();
        let tokens: Vec<_> = (0..3)
            .map(|_| streams.register(ExecutionId::new()))
            .collect();

        streams.close_all();

        assert_eq!(streams.count(), 0);
        assert!(tokens.iter().all(|t| t.is_cancelled()));
    }
}
