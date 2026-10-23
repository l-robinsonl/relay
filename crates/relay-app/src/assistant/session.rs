//! Running a turn, and asking the person when it matters.
//!
//! The loop lives in Rust and the question lives in a window, so the
//! approval round-trip needs a bridge: the approver parks on a channel,
//! the interface is told what is being asked, and a command resolves
//! it. If the window goes away, the parked call is denied rather than
//! hanging — a tool that sends a request must never run because nobody
//! answered.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use relay_agent::agent_loop::{Approval, Approver, Progress};
use relay_agent::tool::{ToolCall, ToolResult, ToolRisk};
use tokio::sync::oneshot;

/// Something the interface should show while a turn runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssistantEvent {
    /// The model said something.
    #[serde(rename_all = "camelCase")]
    Said {
        /// What it said.
        text: String,
    },
    /// A tool is about to run.
    #[serde(rename_all = "camelCase")]
    Running {
        /// Which call.
        call: ToolCall,
    },
    /// A tool finished.
    #[serde(rename_all = "camelCase")]
    Ran {
        /// What it produced.
        result: ToolResult,
    },
    /// The person is being asked whether something may run.
    #[serde(rename_all = "camelCase")]
    Asking {
        /// Which call.
        call: ToolCall,
        /// Why it is being asked.
        risk: ToolRisk,
    },
}

/// Where events go. Implemented by the desktop layer.
pub trait EventSink: Send + Sync + 'static {
    /// Shows one event.
    fn emit(&self, event: AssistantEvent);
}

/// Drops every event. For tests and headless use.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoEvents;
impl EventSink for NoEvents {
    fn emit(&self, _event: AssistantEvent) {}
}

/// The parked approvals for one turn.
#[derive(Default)]
pub struct Approvals {
    waiting: Mutex<HashMap<String, oneshot::Sender<Approval>>>,
}

impl std::fmt::Debug for Approvals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let waiting = self.waiting.lock().map(|w| w.len()).unwrap_or(0);
        f.debug_struct("Approvals")
            .field("waiting", &waiting)
            .finish()
    }
}

impl Approvals {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers a parked call. Returns false if nothing was waiting.
    pub fn resolve(&self, call_id: &str, allow: bool) -> bool {
        let Ok(mut waiting) = self.waiting.lock() else {
            return false;
        };

        match waiting.remove(call_id) {
            Some(sender) => sender
                .send(if allow {
                    Approval::Allow
                } else {
                    Approval::Deny
                })
                .is_ok(),
            None => false,
        }
    }

    /// Denies everything still waiting.
    ///
    /// Called when a turn is cancelled or the window closes. Denying
    /// rather than dropping means the loop continues and tells the
    /// model, instead of parking forever.
    pub fn deny_all(&self) {
        let Ok(mut waiting) = self.waiting.lock() else {
            return;
        };
        for (_, sender) in waiting.drain() {
            let _ = sender.send(Approval::Deny);
        }
    }

    /// How many are parked. For diagnostics and tests.
    pub fn pending(&self) -> usize {
        self.waiting.lock().map(|w| w.len()).unwrap_or(0)
    }
}

/// Asks the interface, and waits.
pub struct AskTheUser {
    approvals: Arc<Approvals>,
    events: Arc<dyn EventSink>,
}

impl std::fmt::Debug for AskTheUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskTheUser").finish_non_exhaustive()
    }
}

impl AskTheUser {
    /// Builds an approver over a shared set of parked calls.
    pub fn new(approvals: Arc<Approvals>, events: Arc<dyn EventSink>) -> Self {
        Self { approvals, events }
    }
}

#[async_trait::async_trait]
impl Approver for AskTheUser {
    async fn approve(&self, call: &ToolCall, risk: ToolRisk) -> Approval {
        let (sender, receiver) = oneshot::channel();

        {
            let Ok(mut waiting) = self.approvals.waiting.lock() else {
                return Approval::Deny;
            };
            waiting.insert(call.id.clone(), sender);
        }

        self.events.emit(AssistantEvent::Asking {
            call: call.clone(),
            risk,
        });

        // A dropped sender means the window went away mid-question.
        // Denying is the only safe reading of silence.
        receiver.await.unwrap_or(Approval::Deny)
    }
}

/// Forwards the loop's progress to the interface.
pub struct Forwarding {
    events: Arc<dyn EventSink>,
}

impl std::fmt::Debug for Forwarding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forwarding").finish_non_exhaustive()
    }
}

impl Forwarding {
    /// Wraps a sink.
    pub fn new(events: Arc<dyn EventSink>) -> Self {
        Self { events }
    }
}

impl Progress for Forwarding {
    fn said(&self, text: &str) {
        self.events.emit(AssistantEvent::Said {
            text: text.to_string(),
        });
    }
    fn running(&self, call: &ToolCall) {
        self.events
            .emit(AssistantEvent::Running { call: call.clone() });
    }
    fn ran(&self, result: &ToolResult) {
        self.events.emit(AssistantEvent::Ran {
            result: result.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_releases_the_parked_call() {
        let approvals = Arc::new(Approvals::new());
        let approver = AskTheUser::new(approvals.clone(), Arc::new(NoEvents));

        let call = ToolCall {
            id: "c1".into(),
            name: "send_request".into(),
            input: serde_json::json!({}),
        };

        let waiting = tokio::spawn(async move { approver.approve(&call, ToolRisk::Send).await });

        // Let the approver park before answering.
        for _ in 0..50 {
            if approvals.pending() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        assert!(approvals.resolve("c1", true));
        assert_eq!(waiting.await.unwrap(), Approval::Allow);
    }

    #[tokio::test]
    async fn a_window_that_goes_away_denies_rather_than_hangs() {
        // A tool that sends a request must never run because nobody
        // answered, and the loop must not park forever either.
        let approvals = Arc::new(Approvals::new());
        let approver = AskTheUser::new(approvals.clone(), Arc::new(NoEvents));

        let call = ToolCall {
            id: "c1".into(),
            name: "send_request".into(),
            input: serde_json::json!({}),
        };

        let waiting = tokio::spawn(async move { approver.approve(&call, ToolRisk::Send).await });

        for _ in 0..50 {
            if approvals.pending() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        approvals.deny_all();
        assert_eq!(waiting.await.unwrap(), Approval::Deny);
    }

    #[test]
    fn answering_something_nobody_asked_is_reported_not_ignored() {
        let approvals = Approvals::new();
        assert!(!approvals.resolve("never-asked", true));
    }

    #[test]
    fn events_serialise_with_a_tag_the_interface_can_switch_on() {
        let json = serde_json::to_value(AssistantEvent::Asking {
            call: ToolCall {
                id: "c1".into(),
                name: "send_request".into(),
                input: serde_json::json!({ "id": "r1" }),
            },
            risk: ToolRisk::Send,
        })
        .unwrap();

        assert_eq!(json["type"], "asking");
        assert_eq!(json["risk"], "send");
        assert_eq!(json["call"]["name"], "send_request");
    }
}
