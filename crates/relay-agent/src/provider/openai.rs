//! Anything speaking the OpenAI chat-completions shape.
//!
//! One implementation covers OpenAI, OpenRouter, Together, Groq, and a
//! local Ollama or LM Studio, because they all copied the same wire
//! format. That matters for a local-first tool: somebody who would
//! rather no prompt left their machine points this at `localhost` and
//! nothing else about Relay changes.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{CompletionRequest, CompletionResponse, Message, ModelProvider, Role, StopReason};
use crate::error::{AgentError, AgentResult};
use crate::tool::ToolCall;

/// The model used when the user has not chosen one.
pub const DEFAULT_MODEL: &str = "gpt-5";

/// An OpenAI-compatible endpoint.
pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    label: &'static str,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    /// OpenAI itself.
    pub fn new(api_key: impl Into<String>) -> AgentResult<Self> {
        Self::with_base_url(api_key, "https://api.openai.com/v1", "OpenAI")
    }

    /// Any other endpoint speaking the same shape.
    ///
    /// A local server usually wants no key at all, so an empty one is
    /// allowed here and simply not sent.
    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        label: &'static str,
    ) -> AgentResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .map_err(|e| AgentError::Internal(e.to_string()))?,
            api_key: api_key.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            label,
        })
    }

    /// Turns the conversation into chat-completions messages.
    ///
    /// The shape differs from Anthropic's in a way worth noting: tool
    /// results are their own `role: "tool"` messages rather than blocks
    /// inside a user turn, so one of ours can become several of theirs.
    fn encode_messages(messages: &[Message]) -> Vec<Value> {
        let mut out = Vec::new();

        for message in messages {
            if !message.tool_results.is_empty() {
                for result in &message.tool_results {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": result.call_id,
                        // No is_error field here, so failure has to be
                        // said in words or the model cannot tell.
                        "content": if result.outcome.is_error {
                            format!("Error: {}", result.outcome.content)
                        } else {
                            result.outcome.content.clone()
                        },
                    }));
                }
                continue;
            }

            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };

            if message.tool_calls.is_empty() {
                out.push(json!({ "role": role, "content": message.text }));
                continue;
            }

            out.push(json!({
                "role": role,
                "content": if message.text.is_empty() { Value::Null } else { Value::String(message.text.clone()) },
                "tool_calls": message.tool_calls.iter().map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        // Arguments are a JSON *string* here, not an
                        // object. Sending an object is silently accepted
                        // and then misparsed.
                        "arguments": call.input.to_string(),
                    },
                })).collect::<Vec<_>>(),
            }));
        }

        out
    }
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        self.label
    }

    fn default_model(&self) -> &'static str {
        DEFAULT_MODEL
    }

    async fn complete(&self, request: CompletionRequest<'_>) -> AgentResult<CompletionResponse> {
        let mut messages = vec![json!({ "role": "system", "content": request.system })];
        messages.extend(Self::encode_messages(request.messages));

        let mut body = json!({
            "model": request.model,
            "max_completion_tokens": request.max_tokens,
            "messages": messages,
        });

        if !request.tools.is_empty() {
            body["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": tool.name(),
                                "description": tool.description(),
                                "parameters": tool.input_schema(),
                            },
                        })
                    })
                    .collect(),
            );
        }

        let mut outgoing = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);

        // A local endpoint usually wants no key, and sending an empty
        // bearer token makes some of them refuse outright.
        if !self.api_key.trim().is_empty() {
            outgoing = outgoing.bearer_auth(&self.api_key);
        }

        let response = outgoing.send().await.map_err(|e| AgentError::Transport {
            provider: self.label.into(),
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
                    provider: self.label.into(),
                },
                429 => AgentError::RateLimited {
                    provider: self.label.into(),
                    retry_after,
                },
                other => AgentError::Provider {
                    provider: self.label.into(),
                    status: other,
                    detail,
                },
            });
        }

        let body: Value = response.json().await.map_err(|e| AgentError::Provider {
            provider: self.label.into(),
            status: status.as_u16(),
            detail: e.to_string(),
        })?;

        let choice = &body["choices"][0];
        let tool_calls = choice["message"]["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .map(|call| ToolCall {
                        id: call["id"].as_str().unwrap_or_default().to_string(),
                        name: call["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        // A JSON string on this side. A model that emits
                        // malformed JSON here is common enough that an
                        // empty object beats failing the whole turn —
                        // the tool reports the problem and it retries.
                        input: call["function"]["arguments"]
                            .as_str()
                            .and_then(|raw| serde_json::from_str(raw).ok())
                            .unwrap_or_else(|| json!({})),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(CompletionResponse {
            text: choice["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            stop_reason: match choice["finish_reason"].as_str() {
                Some("tool_calls") => StopReason::ToolUse,
                Some("length") => StopReason::MaxTokens,
                Some("content_filter") => StopReason::Refused,
                _ => StopReason::Done,
            },
            tool_calls,
            input_tokens: body["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: body["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{ToolOutcome, ToolResult};

    #[test]
    fn a_local_endpoint_needs_no_key() {
        // Pointing this at Ollama is the answer for somebody who wants
        // no prompt to leave their machine.
        assert!(OpenAiProvider::with_base_url("", "http://localhost:11434/v1", "Ollama").is_ok());
    }

    #[test]
    fn debug_output_does_not_contain_the_key() {
        let provider = OpenAiProvider::new("sk-very-secret").unwrap();
        assert!(!format!("{provider:?}").contains("very-secret"));
    }

    #[test]
    fn tool_arguments_are_encoded_as_a_json_string() {
        // An object here is accepted and then misparsed, which shows up
        // as the model appearing to call tools with no arguments.
        let messages = vec![Message {
            role: Role::Assistant,
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "send_request".into(),
                input: json!({ "id": "r1" }),
            }],
            tool_results: Vec::new(),
        }];

        let encoded = OpenAiProvider::encode_messages(&messages);
        let arguments = encoded[0]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("arguments must be a string");
        assert_eq!(
            serde_json::from_str::<Value>(arguments).unwrap()["id"],
            "r1"
        );
    }

    #[test]
    fn one_turn_of_results_becomes_one_tool_message_each() {
        let messages = vec![Message::results(vec![
            ToolResult {
                call_id: "call_1".into(),
                outcome: ToolOutcome::ok("200 OK"),
            },
            ToolResult {
                call_id: "call_2".into(),
                outcome: ToolOutcome::failed("no such request"),
            },
        ])];

        let encoded = OpenAiProvider::encode_messages(&messages);
        assert_eq!(encoded.len(), 2);
        assert_eq!(encoded[0]["role"], "tool");
        assert_eq!(encoded[0]["tool_call_id"], "call_1");

        // There is no is_error field in this shape, so the failure has
        // to be in the words or the model cannot tell.
        assert!(
            encoded[1]["content"]
                .as_str()
                .unwrap()
                .starts_with("Error:"),
            "{:?}",
            encoded[1]
        );
    }
}
