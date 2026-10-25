//! What the assistant can do, and what it must ask about first.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use ts_rs::TS;
use serde::{Deserialize, Serialize};

use crate::error::{AgentError, AgentResult};

/// How much a tool can cost you if the model is wrong.
///
/// The assistant is given real capabilities over the user's work and,
/// through `Send`, over other people's servers. The interface decides
/// what to confirm from this, so getting a tool's classification right
/// matters more than most things in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ToolRisk {
    /// Looks at what is already there. Runs without asking.
    Read,
    /// Changes something in the local workspace. Undoable — every write
    /// goes through the same tombstoned, revisioned storage as a human
    /// edit — so it runs without asking by default.
    Write,
    /// Destroys something, or does something a person would want to see
    /// coming. Always confirmed.
    Destructive,
    /// Reaches a server that is not ours, with the user's credentials
    /// attached. Always confirmed by default: a request Relay sends is
    /// indistinguishable at the far end from one the user sent, and
    /// "list the open invoices" and "delete the open invoices" differ
    /// by one word in a prompt.
    Send,
}

impl ToolRisk {
    /// Whether the interface should confirm before running this.
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Self::Destructive | Self::Send)
    }
}

/// One capability offered to the model.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// The name the model calls it by. Stable: it appears in saved
    /// transcripts.
    fn name(&self) -> &str;

    /// What it does, written for the model. This is the only thing
    /// telling it when to reach for this rather than something else, so
    /// it is prompt text, not documentation.
    fn description(&self) -> &str;

    /// A JSON Schema for the arguments.
    fn input_schema(&self) -> serde_json::Value;

    /// How much it can cost you.
    fn risk(&self) -> ToolRisk;

    /// Runs it.
    async fn run(&self, input: serde_json::Value) -> AgentResult<ToolOutcome>;
}

/// What a tool produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolOutcome {
    /// What the model is told. Keep it small: this goes back into the
    /// context on every subsequent turn of the conversation.
    pub content: String,
    /// True when the tool failed. The model is told either way — it
    /// often recovers, and hiding the failure just makes it retry
    /// blindly.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolOutcome {
    /// A successful result.
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    /// A failure the model should see and work around.
    pub fn failed(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// A call the model asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolCall {
    /// The provider's id for this call, echoed back with the result.
    pub id: String,
    /// Which tool.
    pub name: String,
    /// Its arguments, as the model produced them.
    #[ts(type = "unknown")]
    pub input: serde_json::Value,
}

/// A call paired with what it produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolResult {
    /// Which call this answers.
    pub call_id: String,
    /// What happened.
    pub outcome: ToolOutcome,
}

/// Everything the assistant can do in one conversation.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ToolRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a tool, replacing any of the same name.
    pub fn add(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }

    /// Looks one up.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// How risky a named tool is, if it exists.
    pub fn risk_of(&self, name: &str) -> Option<ToolRisk> {
        self.tools.get(name).map(|t| t.risk())
    }

    /// Every tool, in a stable order.
    ///
    /// Stable because the tool list is part of the cached prompt prefix:
    /// a set that reordered between turns would invalidate the cache on
    /// every request and quietly multiply the bill.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.values()
    }

    /// How many are registered.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True when nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Runs a call, or says why it could not.
    pub async fn dispatch(&self, call: &ToolCall) -> AgentResult<ToolOutcome> {
        let tool = self
            .get(&call.name)
            .ok_or_else(|| AgentError::UnknownTool(call.name.clone()))?;

        tool.run(call.input.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub(&'static str, ToolRisk);

    #[async_trait]
    impl Tool for Stub {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "a stub"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        fn risk(&self) -> ToolRisk {
            self.1
        }
        async fn run(&self, input: serde_json::Value) -> AgentResult<ToolOutcome> {
            Ok(ToolOutcome::ok(input.to_string()))
        }
    }

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry
            .add(Arc::new(Stub("send_request", ToolRisk::Send)))
            .add(Arc::new(Stub("list_requests", ToolRisk::Read)))
            .add(Arc::new(Stub("delete_request", ToolRisk::Destructive)));
        registry
    }

    #[test]
    fn reaching_other_peoples_servers_is_confirmed_and_reading_is_not() {
        assert!(ToolRisk::Send.needs_confirmation());
        assert!(ToolRisk::Destructive.needs_confirmation());
        assert!(!ToolRisk::Read.needs_confirmation());
        // A local write goes through the same revisioned, tombstoned
        // storage as a human edit, so it can be undone.
        assert!(!ToolRisk::Write.needs_confirmation());
    }

    #[test]
    fn the_tool_order_is_stable_across_registries() {
        // The tool list is part of the cached prompt prefix. A set that
        // reordered between turns would invalidate the cache every time.
        let names = |r: &ToolRegistry| r.iter().map(|t| t.name().to_string()).collect::<Vec<_>>();

        let mut other = ToolRegistry::new();
        other
            .add(Arc::new(Stub("delete_request", ToolRisk::Destructive)))
            .add(Arc::new(Stub("send_request", ToolRisk::Send)))
            .add(Arc::new(Stub("list_requests", ToolRisk::Read)));

        assert_eq!(names(&registry()), names(&other));
    }

    #[tokio::test]
    async fn an_unknown_tool_is_named_rather_than_silently_ignored() {
        let call = ToolCall {
            id: "t1".into(),
            name: "rm_rf".into(),
            input: serde_json::json!({}),
        };

        let error = registry().dispatch(&call).await.unwrap_err();
        assert!(matches!(error, AgentError::UnknownTool(name) if name == "rm_rf"));
    }

    #[tokio::test]
    async fn a_registered_tool_runs_with_its_arguments() {
        let call = ToolCall {
            id: "t1".into(),
            name: "list_requests".into(),
            input: serde_json::json!({ "q": "health" }),
        };

        let outcome = registry().dispatch(&call).await.unwrap();
        assert!(outcome.content.contains("health"));
        assert!(!outcome.is_error);
    }

    #[test]
    fn risk_is_reported_for_the_interface_to_act_on() {
        let registry = registry();
        assert_eq!(registry.risk_of("send_request"), Some(ToolRisk::Send));
        assert_eq!(registry.risk_of("nope"), None);
    }
}
