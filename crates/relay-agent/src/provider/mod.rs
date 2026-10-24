//! Talking to a model, behind one trait.
//!
//! Relay sends nothing through a server of its own. The key is the
//! user's, the request goes from their machine to the provider, and
//! Relay never sees either.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AgentResult;
use crate::tool::{Tool, ToolCall};

mod anthropic;
mod openai;
mod relay;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;
pub use relay::RelayProvider;

/// Who is speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// The person.
    User,
    /// The model.
    Assistant,
}

/// One turn of a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// Who said it.
    pub role: Role,
    /// What they said. Empty when the turn was only tool calls.
    #[serde(default)]
    pub text: String,
    /// Tools the model asked for, on an assistant turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Results being handed back, on a user turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<crate::tool::ToolResult>,
}

impl Message {
    /// A plain message from the person.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    /// A plain message from the model.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    /// A turn carrying tool results back to the model.
    pub fn results(results: Vec<crate::tool::ToolResult>) -> Self {
        Self {
            role: Role::User,
            text: String::new(),
            tool_calls: Vec::new(),
            tool_results: results,
        }
    }
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// It finished its answer.
    Done,
    /// It wants tools run before it can continue.
    ToolUse,
    /// It hit the output ceiling mid-sentence.
    MaxTokens,
    /// It declined.
    Refused,
}

/// What to ask the model for.
#[derive(Clone)]
pub struct CompletionRequest<'a> {
    /// The model id, as the provider spells it.
    pub model: &'a str,
    /// Standing instructions. Stable across a conversation so it stays
    /// cacheable.
    pub system: &'a str,
    /// The conversation so far.
    pub messages: &'a [Message],
    /// What the model may call.
    pub tools: &'a [&'a dyn Tool],
    /// Ceiling on one reply.
    pub max_tokens: u32,
}

impl std::fmt::Debug for CompletionRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The messages are the user's conversation and the tool list is
        // long; neither belongs in a log line by accident.
        f.debug_struct("CompletionRequest")
            .field("model", &self.model)
            .field("messages", &self.messages.len())
            .field("tools", &self.tools.len())
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionResponse {
    /// Anything it said in prose.
    pub text: String,
    /// Tools it wants run.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Why it stopped.
    pub stop_reason: StopReason,
    /// Tokens in, for the cost line.
    #[serde(default)]
    pub input_tokens: u32,
    /// Tokens out.
    #[serde(default)]
    pub output_tokens: u32,
}

/// Somewhere to send a conversation.
#[async_trait]
pub trait ModelProvider: Send + Sync + 'static {
    /// Which provider this is, for messages and for settings.
    fn name(&self) -> &'static str;

    /// The model used when the user has not chosen one.
    fn default_model(&self) -> &'static str;

    /// Runs one turn.
    async fn complete(&self, request: CompletionRequest<'_>) -> AgentResult<CompletionResponse>;
}
