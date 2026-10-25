//! The agent loop: ask, run what it asked for, ask again.

use std::sync::Arc;

use crate::error::{AgentError, AgentResult};
use crate::provider::{CompletionRequest, Message, ModelProvider, StopReason};
use crate::tool::{Tool, ToolCall, ToolRegistry, ToolResult, ToolRisk};

/// How many model turns one request may take.
///
/// A ceiling rather than a budget: the loop exists to let the model work
/// through a task, and a task that has not converged in this many turns
/// is looping, not working.
pub const MAX_STEPS: usize = 40;

/// Whether a tool call may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// Run it.
    Allow,
    /// Do not, and tell the model why.
    Deny,
}

/// Asked before anything consequential happens.
///
/// A trait rather than a callback because the answer comes from a
/// person: the desktop layer shows the call and waits.
#[async_trait::async_trait]
pub trait Approver: Send + Sync + 'static {
    /// May this run?
    async fn approve(&self, call: &ToolCall, risk: ToolRisk) -> Approval;
}

/// Approves everything. For tests, and for a user who has turned
/// confirmation off for a session.
#[derive(Debug, Default, Clone, Copy)]
pub struct ApproveEverything;

#[async_trait::async_trait]
impl Approver for ApproveEverything {
    async fn approve(&self, _call: &ToolCall, _risk: ToolRisk) -> Approval {
        Approval::Allow
    }
}

/// Reports progress as the loop runs, so the interface is not a
/// spinner for two minutes.
pub trait Progress: Send + Sync + 'static {
    /// A fragment of the answer, as it is written.
    ///
    /// Called many times per message. Every fragment concatenated is
    /// exactly the text that `said` is later called with, so an
    /// implementation uses one or the other and never both.
    fn delta(&self, _text: &str) {}
    /// The model said something, complete.
    fn said(&self, _text: &str) {}
    /// A tool is about to run.
    fn running(&self, _call: &ToolCall) {}
    /// A tool finished.
    fn ran(&self, _result: &ToolResult) {}
}

/// Reports nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct Silent;
impl Progress for Silent {}

/// What one turn of the conversation produced.
#[derive(Debug, Clone)]
pub struct Turn {
    /// Everything added to the conversation, in order.
    pub messages: Vec<Message>,
    /// Tokens read across every step of this turn.
    pub input_tokens: u32,
    /// Tokens written.
    pub output_tokens: u32,
    /// How many times the model was called.
    pub steps: usize,
}

impl Turn {
    /// The assistant's final prose, which is what the user reads.
    pub fn reply(&self) -> &str {
        self.messages
            .iter()
            .rev()
            .find(|m| !m.text.trim().is_empty())
            .map(|m| m.text.as_str())
            .unwrap_or_default()
    }
}

/// Runs the conversation until the model is finished.
pub struct Agent {
    provider: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
    approver: Arc<dyn Approver>,
    progress: Arc<dyn Progress>,
    model: String,
    system: String,
    max_tokens: u32,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("provider", &self.provider.name())
            .field("model", &self.model)
            .field("tools", &self.tools.len())
            .finish()
    }
}

impl Agent {
    /// Builds an agent.
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        tools: ToolRegistry,
        system: impl Into<String>,
    ) -> Self {
        let model = provider.default_model().to_string();
        Self {
            provider,
            tools,
            approver: Arc::new(ApproveEverything),
            progress: Arc::new(Silent),
            model,
            system: system.into(),
            max_tokens: 16_000,
        }
    }

    /// Uses a particular model rather than the provider's default.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let model = model.into();
        if !model.trim().is_empty() {
            self.model = model;
        }
        self
    }

    /// Asks before running anything consequential.
    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = approver;
        self
    }

    /// Reports progress as it goes.
    pub fn with_progress(mut self, progress: Arc<dyn Progress>) -> Self {
        self.progress = progress;
        self
    }

    /// Runs one turn: everything from the user's message to the
    /// model's final answer, however many tool calls that takes.
    ///
    /// `history` is the conversation so far and is not modified; what
    /// this turn added comes back in [`Turn::messages`] for the caller
    /// to persist.
    pub async fn run(&self, history: &[Message], ask: Message) -> AgentResult<Turn> {
        let mut conversation: Vec<Message> = history.to_vec();
        conversation.push(ask.clone());

        let mut added = vec![ask];
        let mut turn = Turn {
            messages: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            steps: 0,
        };

        let tools: Vec<&dyn Tool> = self.tools.iter().map(|t| t.as_ref() as &dyn Tool).collect();

        for _ in 0..MAX_STEPS {
            turn.steps += 1;

            let progress = self.progress.clone();
            let response = self
                .provider
                .complete_streaming(
                    CompletionRequest {
                        model: &self.model,
                        system: &self.system,
                        messages: &conversation,
                        tools: &tools,
                        max_tokens: self.max_tokens,
                    },
                    &move |fragment| progress.delta(fragment),
                )
                .await?;

            turn.input_tokens += response.input_tokens;
            turn.output_tokens += response.output_tokens;

            if !response.text.trim().is_empty() {
                self.progress.said(&response.text);
            }

            let assistant = Message {
                role: crate::provider::Role::Assistant,
                text: response.text,
                tool_calls: response.tool_calls.clone(),
                tool_results: Vec::new(),
            };
            conversation.push(assistant.clone());
            added.push(assistant);

            if response.stop_reason != StopReason::ToolUse || response.tool_calls.is_empty() {
                turn.messages = added;
                return Ok(turn);
            }

            // Every call in one assistant message is answered in one
            // user message. Splitting them teaches the model to stop
            // asking for several at once.
            let mut results = Vec::with_capacity(response.tool_calls.len());

            for call in &response.tool_calls {
                let risk = self.tools.risk_of(&call.name);

                let outcome = match risk {
                    None => crate::tool::ToolOutcome::failed(format!(
                        "There is no tool called '{}'.",
                        call.name
                    )),
                    Some(risk) if risk.needs_confirmation() => {
                        match self.approver.approve(call, risk).await {
                            Approval::Deny => crate::tool::ToolOutcome::failed(
                                "The person declined this. Do not try it again; \
                                 ask them what they would rather do."
                                    .to_string(),
                            ),
                            Approval::Allow => {
                                self.progress.running(call);
                                self.dispatch(call).await
                            }
                        }
                    }
                    Some(_) => {
                        self.progress.running(call);
                        self.dispatch(call).await
                    }
                };

                let result = ToolResult {
                    call_id: call.id.clone(),
                    outcome,
                };
                self.progress.ran(&result);
                results.push(result);
            }

            let message = Message::results(results);
            conversation.push(message.clone());
            added.push(message);
        }

        Err(AgentError::TooManySteps(MAX_STEPS))
    }

    /// Runs one call, turning an error into something the model can read.
    ///
    /// A tool that fails is not a failed turn: the model usually
    /// recovers, and propagating the error would throw away the work it
    /// has already done this turn.
    async fn dispatch(&self, call: &ToolCall) -> crate::tool::ToolOutcome {
        match self.tools.dispatch(call).await {
            Ok(outcome) => outcome,
            Err(error) => crate::tool::ToolOutcome::failed(error.to_string()),
        }
    }
}
