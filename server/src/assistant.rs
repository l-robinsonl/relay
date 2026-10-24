//! The hosted assistant: Relay's key, a subscriber's request.
//!
//! The agent loop stays on the person's machine. This is a metered
//! pass-through — it never sees a tool it has to understand, never
//! runs one, and never holds conversation state. Conversations live
//! in the user's own SQLite, which is the answer this product already
//! gives to "where do my conversations live".
//!
//! **It rebuilds the request rather than forwarding it.** A proxy
//! that passed the body through would let a subscriber on a £6 plan
//! name Opus and a 128k output, on Relay's account. Model, token
//! ceiling and caching are decided here; only the conversation
//! crosses over.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::billing::AssistantUsage;
use crate::error::{ServerError, ServerResult};

/// Anthropic's API host.
const API: &str = "https://api.anthropic.com";

/// The version this speaks.
const API_VERSION: &str = "2023-06-01";

/// The most output any one turn may generate.
///
/// A ceiling rather than a setting: output is five times the price of
/// input, and the loop's turns are short. Somebody asking for a
/// 128,000-token essay on Relay's account is not a use case.
const MAX_OUTPUT_TOKENS: u32 = 4096;

/// What the desktop application sends.
///
/// Deliberately narrow. Anything not named here cannot be set by a
/// caller, which is the point — the fields that cost money are not
/// among them.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRequest {
    /// The system prompt.
    #[serde(default)]
    pub system: String,
    /// The conversation so far, in Anthropic's message shape.
    #[serde(default)]
    pub messages: Vec<Value>,
    /// Tool definitions, as the loop would send them.
    #[serde(default)]
    pub tools: Vec<Value>,
}

/// What comes back.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnResponse {
    /// The content blocks, passed through unchanged so the loop reads
    /// them exactly as it would from Anthropic.
    pub content: Value,
    /// Why generation stopped.
    pub stop_reason: Option<String>,
    /// What this turn cost, for the caller to show if it wants to.
    pub usage: AssistantUsage,
}

/// A configured upstream.
pub struct Assistant {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl std::fmt::Debug for Assistant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No api_key field: this ends up in spans and panics.
        f.debug_struct("Assistant")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Assistant {
    /// Builds a client.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> ServerResult<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(ServerError::Config("RELAY_ASSISTANT_KEY is empty".into()));
        }

        Ok(Self {
            client: reqwest::Client::builder()
                // A turn with thinking can take a while. Longer than
                // the Stripe client, shorter than for ever.
                .timeout(Duration::from_secs(120))
                .build()
                .map_err(|e| ServerError::Config(e.to_string()))?,
            api_key,
            model: model.into(),
            base_url: API.into(),
        })
    }

    /// Points it somewhere other than Anthropic, for tests.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Which model it runs.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Builds the upstream request from what the caller sent.
    ///
    /// Separate from sending it so the shape can be asserted without
    /// a network, which is where the cost controls actually live.
    pub fn body_for(&self, turn: &TurnRequest) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            // The prompt and the tool list are identical on every
            // request of every turn, and on a short turn they are
            // about a third of the input cost. Cache reads are a
            // tenth of the price.
            "system": [{
                "type": "text",
                "text": turn.system,
                "cache_control": { "type": "ephemeral" },
            }],
            "messages": turn.messages,
            "thinking": { "type": "adaptive" },
        });

        if !turn.tools.is_empty() {
            let mut tools = turn.tools.clone();
            // Caching is a prefix match over tools, then system, then
            // messages — so the breakpoint goes on the last tool to
            // cover the whole fixed prefix.
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = json!({ "type": "ephemeral" });
            }
            body["tools"] = Value::Array(tools);
        }

        body
    }

    /// Runs one request against the model.
    pub async fn run(&self, turn: &TurnRequest) -> ServerResult<TurnResponse> {
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&self.body_for(turn))
            .send()
            .await
            .map_err(|e| ServerError::Assistant(format!("could not be reached: {e}")))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        if !status.is_success() {
            // The provider's own message is the operator's best clue
            // — an expired key, a model that was retired — and it
            // carries nothing of the conversation.
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(text);
            return Err(ServerError::Assistant(format!("{status}: {detail}")));
        }

        let body: Value = serde_json::from_str(&text)
            .map_err(|e| ServerError::Assistant(format!("sent something unreadable: {e}")))?;

        Ok(TurnResponse {
            content: body["content"].clone(),
            stop_reason: body["stop_reason"].as_str().map(str::to_string),
            usage: read_usage(&body["usage"]),
        })
    }
}

/// Reads what a response cost.
///
/// Cache reads are counted separately because they are a tenth of
/// the price, and a bill that does not distinguish them says nothing
/// about whether caching is working.
pub fn read_usage(usage: &Value) -> AssistantUsage {
    AssistantUsage {
        turns: 1,
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cached_tokens: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn() -> TurnRequest {
        TurnRequest {
            system: "You are the assistant inside Relay.".into(),
            messages: vec![json!({ "role": "user", "content": "hello" })],
            tools: vec![
                json!({ "name": "list_requests", "input_schema": {} }),
                json!({ "name": "send_request", "input_schema": {} }),
            ],
        }
    }

    #[test]
    fn the_caller_does_not_choose_the_model() {
        // The whole reason this rebuilds rather than forwards. A
        // subscriber on £6 a month naming Opus would be spending
        // somebody else's money.
        let assistant = Assistant::new("sk-ant-test", "claude-haiku-5-5").unwrap();
        let body = assistant.body_for(&turn());

        assert_eq!(body["model"], "claude-haiku-5-5");
    }

    #[test]
    fn output_is_capped_whatever_was_asked_for() {
        let assistant = Assistant::new("sk-ant-test", "claude-haiku-5-5").unwrap();
        let body = assistant.body_for(&turn());

        assert_eq!(body["max_tokens"], MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn the_fixed_prefix_is_cached() {
        // About a third of the input cost on a short turn, and it is
        // byte-identical on every request of every turn.
        let assistant = Assistant::new("sk-ant-test", "claude-haiku-5-5").unwrap();
        let body = assistant.body_for(&turn());

        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        // On the *last* tool: caching is a prefix match over tools
        // then system, so the breakpoint has to cover both.
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn a_turn_with_no_tools_sends_none() {
        let assistant = Assistant::new("sk-ant-test", "claude-haiku-5-5").unwrap();
        let body = assistant.body_for(&TurnRequest {
            tools: vec![],
            ..turn()
        });

        assert!(body.get("tools").is_none());
    }

    #[test]
    fn debug_output_does_not_contain_the_key() {
        let assistant = Assistant::new("sk-ant-very-secret", "claude-haiku-5-5").unwrap();
        assert!(!format!("{assistant:?}").contains("very-secret"));
    }

    #[test]
    fn usage_separates_cache_reads_from_fresh_input() {
        let usage = read_usage(&json!({
            "input_tokens": 1200,
            "output_tokens": 300,
            "cache_read_input_tokens": 5000,
        }));

        assert_eq!(usage.turns, 1);
        assert_eq!(usage.input_tokens, 1200);
        assert_eq!(usage.output_tokens, 300);
        assert_eq!(usage.cached_tokens, 5000);
    }

    #[test]
    fn a_response_without_usage_counts_the_turn_anyway() {
        // A turn that happened is a turn, even if the figures are
        // missing. Not counting it would be the wrong way round.
        let usage = read_usage(&json!(null));

        assert_eq!(usage.turns, 1);
        assert_eq!(usage.input_tokens, 0);
    }
}
