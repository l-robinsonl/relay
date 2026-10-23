//! Claude, through the Messages API.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{CompletionRequest, CompletionResponse, Message, ModelProvider, Role, StopReason};
use crate::error::{AgentError, AgentResult};
use crate::tool::ToolCall;

/// The default model.
///
/// Opus 5.5 rather than something cheaper: this agent edits the user's
/// work and sends requests on their behalf, and the cost of it
/// misreading an instruction is higher than the cost of the tokens.
/// The user can choose otherwise in settings.
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// The API version header every request carries.
const API_VERSION: &str = "2023-06-01";

/// Claude.
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No api_key field. This struct ends up in spans and panics.
        f.debug_struct("AnthropicProvider")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// Builds a provider around the user's key.
    pub fn new(api_key: impl Into<String>) -> AgentResult<Self> {
        Self::with_base_url(api_key, "https://api.anthropic.com")
    }

    /// As [`Self::new`], against another host. For tests, and for
    /// whatever gateway an organisation puts in front of the API.
    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> AgentResult<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(AgentError::NoApiKey {
                provider: "Anthropic".into(),
            });
        }

        Ok(Self {
            client: reqwest::Client::builder()
                // Generous: a long agentic turn genuinely takes minutes.
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .map_err(|e| AgentError::Internal(e.to_string()))?,
            api_key,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }

    /// Turns the conversation into the Messages API's shape.
    fn encode_messages(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|message| {
                let mut content: Vec<Value> = Vec::new();

                if !message.text.trim().is_empty() {
                    content.push(json!({ "type": "text", "text": message.text }));
                }

                for call in &message.tool_calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.input,
                    }));
                }

                for result in &message.tool_results {
                    content.push(json!({
                        "type": "tool_result",
                        "tool_use_id": result.call_id,
                        "content": result.outcome.content,
                        "is_error": result.outcome.is_error,
                    }));
                }

                json!({
                    "role": match message.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    "content": content,
                })
            })
            .collect()
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "Anthropic"
    }

    fn default_model(&self) -> &'static str {
        DEFAULT_MODEL
    }

    async fn complete(&self, request: CompletionRequest<'_>) -> AgentResult<CompletionResponse> {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "input_schema": tool.input_schema(),
                    // Guarantees the arguments validate against the
                    // schema, so a tool never has to defend against a
                    // shape the model invented.
                    "strict": true,
                })
            })
            .collect();

        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            // Cached: the instructions and the tool list do not change
            // between turns, and without this every turn re-reads the
            // whole prefix at full price.
            "system": [{
                "type": "text",
                "text": request.system,
                "cache_control": { "type": "ephemeral" },
            }],
            "messages": Self::encode_messages(request.messages),
            // Deciding which of a dozen tools to use, against a person's
            // own data, is exactly the kind of thing thinking helps with.
            "thinking": { "type": "adaptive" },
        });

        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::Transport {
                provider: "Anthropic".into(),
                detail: e.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            let detail = response.text().await.unwrap_or_default();

            return Err(match status.as_u16() {
                401 | 403 => AgentError::BadApiKey {
                    provider: "Anthropic".into(),
                },
                429 => AgentError::RateLimited {
                    provider: "Anthropic".into(),
                    retry_after,
                },
                other => AgentError::Provider {
                    provider: "Anthropic".into(),
                    status: other,
                    detail,
                },
            });
        }

        let body: Value = response.json().await.map_err(|e| AgentError::Provider {
            provider: "Anthropic".into(),
            status: status.as_u16(),
            detail: e.to_string(),
        })?;

        let mut text = String::new();
        let mut tool_calls = Vec::new();

        for block in body["content"].as_array().unwrap_or(&Vec::new()) {
            match block["type"].as_str() {
                Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
                Some("tool_use") => tool_calls.push(ToolCall {
                    id: block["id"].as_str().unwrap_or_default().to_string(),
                    name: block["name"].as_str().unwrap_or_default().to_string(),
                    input: block["input"].clone(),
                }),
                // Thinking blocks are summaries at best and are not
                // shown; the raw reasoning is never returned anyway.
                _ => {}
            }
        }

        Ok(CompletionResponse {
            text,
            tool_calls,
            stop_reason: match body["stop_reason"].as_str() {
                Some("tool_use") => StopReason::ToolUse,
                Some("max_tokens") => StopReason::MaxTokens,
                Some("refusal") => StopReason::Refused,
                _ => StopReason::Done,
            },
            input_tokens: body["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: body["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{ToolOutcome, ToolResult};

    #[test]
    fn a_key_is_required() {
        assert!(matches!(
            AnthropicProvider::new("  "),
            Err(AgentError::NoApiKey { .. })
        ));
        assert!(AnthropicProvider::new("sk-ant-test").is_ok());
    }

    #[test]
    fn debug_output_does_not_contain_the_key() {
        let provider = AnthropicProvider::new("sk-ant-very-secret").unwrap();
        let printed = format!("{provider:?}");
        assert!(!printed.contains("very-secret"), "{printed}");
    }

    #[test]
    fn a_tool_call_and_its_result_round_trip_into_the_wire_shape() {
        let messages = vec![
            Message::user("send the health check"),
            Message {
                role: Role::Assistant,
                text: "Sending it.".into(),
                tool_calls: vec![ToolCall {
                    id: "toolu_1".into(),
                    name: "send_request".into(),
                    input: serde_json::json!({ "id": "r1" }),
                }],
                tool_results: Vec::new(),
            },
            Message::results(vec![ToolResult {
                call_id: "toolu_1".into(),
                outcome: ToolOutcome::ok("200 OK"),
            }]),
        ];

        let encoded = AnthropicProvider::encode_messages(&messages);

        assert_eq!(encoded[1]["content"][0]["type"], "text");
        assert_eq!(encoded[1]["content"][1]["type"], "tool_use");
        assert_eq!(encoded[1]["content"][1]["id"], "toolu_1");

        // The result comes back as a user turn, keyed by the call id.
        assert_eq!(encoded[2]["role"], "user");
        assert_eq!(encoded[2]["content"][0]["type"], "tool_result");
        assert_eq!(encoded[2]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(encoded[2]["content"][0]["is_error"], false);
    }

    #[test]
    fn an_empty_text_block_is_not_sent() {
        // A turn that is only tool results must not carry an empty text
        // block beside them; the API rejects it.
        let messages = vec![Message::results(vec![ToolResult {
            call_id: "toolu_1".into(),
            outcome: ToolOutcome::ok("done"),
        }])];

        let encoded = AnthropicProvider::encode_messages(&messages);
        assert_eq!(encoded[0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(encoded[0]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn a_failed_tool_is_reported_to_the_model_as_a_failure() {
        // Hiding it would just make the model retry the same thing.
        let messages = vec![Message::results(vec![ToolResult {
            call_id: "toolu_1".into(),
            outcome: ToolOutcome::failed("no such request"),
        }])];

        let encoded = AnthropicProvider::encode_messages(&messages);
        assert_eq!(encoded[0]["content"][0]["is_error"], true);
    }
}
