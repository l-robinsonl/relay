//! The assistant on Relay's key, through a subscriber's sync server.
//!
//! The same loop, the same tools, the same machine — the only
//! difference is who holds the provider key. Somebody who does not
//! want to think about API keys subscribes instead, and the server
//! forwards the request on their behalf.
//!
//! Authentication is the sync session token the client already has.
//! No provider key ever reaches this process, which is the point.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{CompletionRequest, CompletionResponse, ModelProvider, StopReason};
use crate::error::{AgentError, AgentResult};
use crate::tool::ToolCall;

/// The assistant hosted by a Relay server.
pub struct RelayProvider {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

impl std::fmt::Debug for RelayProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No token: a session token is a credential like any other.
        f.debug_struct("RelayProvider")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl RelayProvider {
    /// Builds a provider against a sync server the user is signed in
    /// to.
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> AgentResult<Self> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(AgentError::NoApiKey {
                provider: "Relay".into(),
            });
        }

        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .build()
                .map_err(|e| AgentError::Transport {
                    provider: "Relay".into(),
                    detail: e.to_string(),
                })?,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
        })
    }
}

impl RelayProvider {
    /// The request body, shared by both paths.
    ///
    /// Model and token ceiling are deliberately absent: the server
    /// sets both, and would ignore them anyway.
    fn body_for(request: &CompletionRequest<'_>) -> Value {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "input_schema": tool.input_schema(),
                    "strict": true,
                })
            })
            .collect();

        json!({
            "system": request.system,
            "messages": super::anthropic::encode_messages(request.messages),
            "tools": tools,
        })
    }

    /// Posts to one of the two assistant endpoints and checks the status.
    async fn send(&self, path: &str, body: &Value) -> AgentResult<reqwest::Response> {
        let response = self
            .client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| AgentError::Transport {
                provider: "Relay".into(),
                detail: e.to_string(),
            })?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        {
            let detail = response.text().await.unwrap_or_default();
            let message = serde_json::from_str::<Value>(&detail)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_string))
                .unwrap_or(detail);

            Err(match status.as_u16() {
                401 | 403 => AgentError::BadApiKey {
                    provider: "Relay".into(),
                },
                // Out of included turns, or on a plan that does not
                // include the assistant. The server's own wording
                // says what to do about it, and it is addressed to
                // the person rather than to this code — so it is
                // passed through rather than replaced.
                402 => AgentError::Provider {
                    provider: "Relay".into(),
                    status: 402,
                    detail: message,
                },
                429 => AgentError::RateLimited {
                    provider: "Relay".into(),
                    retry_after: None,
                },
                other => AgentError::Provider {
                    provider: "Relay".into(),
                    status: other,
                    detail: message,
                },
            })
        }
    }
}

#[async_trait]
impl ModelProvider for RelayProvider {
    fn name(&self) -> &'static str {
        "Relay"
    }

    fn default_model(&self) -> &'static str {
        // The server decides, and says so in its own settings. Naming
        // one here would be a claim this process cannot keep.
        "chosen by the server"
    }

    async fn complete(&self, request: CompletionRequest<'_>) -> AgentResult<CompletionResponse> {
        let response = self
            .send("/v1/assistant/messages", &Self::body_for(&request))
            .await?;
        let status = response.status();

        let body: Value = response.json().await.map_err(|e| AgentError::Provider {
            provider: "Relay".into(),
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
                _ => {}
            }
        }

        Ok(CompletionResponse {
            text,
            tool_calls,
            // camelCase: the server returns its own type, not
            // Anthropic's envelope.
            stop_reason: match body["stopReason"].as_str() {
                Some("tool_use") => StopReason::ToolUse,
                Some("max_tokens") => StopReason::MaxTokens,
                Some("refusal") => StopReason::Refused,
                _ => StopReason::Done,
            },
            input_tokens: body["usage"]["inputTokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: body["usage"]["outputTokens"].as_u64().unwrap_or(0) as u32,
        })
    }

    async fn complete_streaming(
        &self,
        request: CompletionRequest<'_>,
        on_delta: super::OnDelta<'_>,
    ) -> AgentResult<CompletionResponse> {
        // The server forwards the provider's events untouched, so the
        // same accumulator reads them. A subscriber and somebody on
        // their own key see the answer arrive identically, which is
        // the point of the tier costing more rather than less.
        let response = self
            .send("/v1/assistant/messages/stream", &Self::body_for(&request))
            .await?;

        let mut accumulator = super::stream::AnthropicAccumulator::new();
        super::stream::read_events("Relay", response, |event, data| {
            if let Some(fragment) = accumulator.push(event, data) {
                on_delta(&fragment);
            }
            Ok(())
        })
        .await?;

        Ok(accumulator.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_token_is_required() {
        assert!(matches!(
            RelayProvider::new("https://relay.test", "  "),
            Err(AgentError::NoApiKey { .. })
        ));
        assert!(RelayProvider::new("https://relay.test", "tok").is_ok());
    }

    #[test]
    fn debug_output_does_not_contain_the_token() {
        let provider = RelayProvider::new("https://relay.test", "very-secret-token").unwrap();
        let printed = format!("{provider:?}");
        assert!(!printed.contains("very-secret"), "{printed}");
    }

    #[test]
    fn a_trailing_slash_does_not_double_up() {
        let provider = RelayProvider::new("https://relay.test/", "tok").unwrap();
        assert_eq!(provider.base_url, "https://relay.test");
    }
}
