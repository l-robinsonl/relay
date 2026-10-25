//! Reassembling a reply that arrives in pieces.
//!
//! Every provider streams as server-sent events, and both shapes have
//! the same awkwardness: a tool call's arguments arrive as JSON split
//! across any number of fragments, so nothing can be parsed until the
//! block ends. The accumulators here hold that partial state and hand
//! back the same [`CompletionResponse`] the non-streaming path
//! produces — which is what lets the loop be indifferent to which was
//! used.

use futures_util::StreamExt;
use serde_json::Value;

use super::{CompletionResponse, StopReason};
use crate::error::{AgentError, AgentResult};
use crate::tool::ToolCall;

/// A tool call being built from fragments.
#[derive(Debug, Default, Clone)]
struct PartialCall {
    id: String,
    name: String,
    /// The arguments, as JSON text so far.
    json: String,
}

impl PartialCall {
    fn finish(self) -> ToolCall {
        ToolCall {
            id: self.id,
            name: self.name,
            // An empty argument object is the common case for a tool
            // that takes nothing, and arrives as no fragments at all.
            input: if self.json.trim().is_empty() {
                Value::Object(serde_json::Map::new())
            } else {
                // A model that produced unparseable arguments is the
                // tool's problem to report, not a reason to throw away
                // a turn that may have done work already. `null` fails
                // the tool's own validation with a message the model
                // can read.
                serde_json::from_str(&self.json).unwrap_or(Value::Null)
            },
        }
    }
}

/// Builds a reply from Anthropic's `messages` stream.
#[derive(Debug, Default)]
pub struct AnthropicAccumulator {
    text: String,
    /// Keyed by the block index the events carry, because blocks are
    /// interleaved and a tool call's fragments are only identifiable
    /// by it.
    blocks: std::collections::BTreeMap<u64, PartialCall>,
    stop_reason: StopReason,
    input_tokens: u32,
    output_tokens: u32,
}

impl AnthropicAccumulator {
    /// A fresh accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one event. Returns any prose it carried, for the caller
    /// to pass straight on to the interface.
    pub fn push(&mut self, event: &str, data: &Value) -> Option<String> {
        match event {
            "message_start" => {
                // Cached reads are charged differently but are still
                // input the person is paying for, so they count.
                let usage = &data["message"]["usage"];
                self.input_tokens += number(&usage["input_tokens"])
                    + number(&usage["cache_creation_input_tokens"])
                    + number(&usage["cache_read_input_tokens"]);
            }
            "content_block_start" => {
                let block = &data["content_block"];
                if block["type"] == "tool_use" {
                    self.blocks.insert(
                        data["index"].as_u64().unwrap_or_default(),
                        PartialCall {
                            id: text_of(&block["id"]),
                            name: text_of(&block["name"]),
                            json: String::new(),
                        },
                    );
                }
            }
            "content_block_delta" => {
                let delta = &data["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let fragment = text_of(&delta["text"]);
                        if !fragment.is_empty() {
                            self.text.push_str(&fragment);
                            return Some(fragment);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(call) =
                            self.blocks.get_mut(&data["index"].as_u64().unwrap_or_default())
                        {
                            call.json.push_str(&text_of(&delta["partial_json"]));
                        }
                    }
                    // Thinking deltas are not shown: the panel renders
                    // the answer, and a summary of reasoning arriving
                    // in the same place would read as the answer.
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                    self.stop_reason = stop_reason(reason);
                }
                self.output_tokens += number(&data["usage"]["output_tokens"]);
            }
            // `error` arrives mid-stream when the provider gives up
            // partway. Handled by the caller, which can report the
            // detail; recording it here would need a second channel.
            _ => {}
        }
        None
    }

    /// The reply, as though it had arrived in one piece.
    pub fn finish(self) -> CompletionResponse {
        CompletionResponse {
            text: self.text,
            tool_calls: self.blocks.into_values().map(PartialCall::finish).collect(),
            stop_reason: self.stop_reason,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        }
    }
}

/// Builds a reply from OpenAI's chat-completions stream.
#[derive(Debug, Default)]
pub struct OpenAiAccumulator {
    text: String,
    /// Keyed by the `index` on each tool-call delta, which is how
    /// OpenAI identifies which call a fragment belongs to.
    calls: std::collections::BTreeMap<u64, PartialCall>,
    stop_reason: StopReason,
    input_tokens: u32,
    output_tokens: u32,
}

impl OpenAiAccumulator {
    /// A fresh accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one `data:` payload. Returns any prose it carried.
    pub fn push(&mut self, data: &Value) -> Option<String> {
        // Sent only on the last chunk, when asked for.
        if let Some(usage) = data.get("usage").filter(|u| !u.is_null()) {
            self.input_tokens = number(&usage["prompt_tokens"]);
            self.output_tokens = number(&usage["completion_tokens"]);
        }

        let choice = &data["choices"][0];
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.stop_reason = match reason {
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refused,
                _ => StopReason::Done,
            };
        }

        let delta = &choice["delta"];

        for call in delta["tool_calls"].as_array().unwrap_or(&Vec::new()) {
            let entry = self
                .calls
                .entry(call["index"].as_u64().unwrap_or_default())
                .or_default();

            // Only the first fragment of a call carries its identity;
            // the rest carry argument text alone.
            if let Some(id) = call["id"].as_str() {
                entry.id = id.to_string();
            }
            if let Some(name) = call["function"]["name"].as_str() {
                entry.name = name.to_string();
            }
            entry
                .json
                .push_str(&text_of(&call["function"]["arguments"]));
        }

        let fragment = text_of(&delta["content"]);
        if fragment.is_empty() {
            return None;
        }
        self.text.push_str(&fragment);
        Some(fragment)
    }

    /// The reply, as though it had arrived in one piece.
    pub fn finish(self) -> CompletionResponse {
        let tool_calls: Vec<ToolCall> =
            self.calls.into_values().map(PartialCall::finish).collect();

        CompletionResponse {
            text: self.text,
            // Some gateways omit `finish_reason` entirely. Calls
            // present means the model wants them run, whatever it
            // said about why it stopped.
            stop_reason: if !tool_calls.is_empty() && self.stop_reason == StopReason::Done {
                StopReason::ToolUse
            } else {
                self.stop_reason
            },
            tool_calls,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        }
    }
}

/// Reads a response as server-sent events, calling `on_event` for each.
///
/// The parser is relay-http's, so chunk boundaries landing mid-line —
/// which they do under load, which is when it matters — are already
/// handled. `on_event` gets the event name, empty when the provider
/// does not send one, and the parsed `data:` payload. A payload that
/// is not JSON is skipped rather than fatal: `[DONE]` is one, and a
/// comment keep-alive is another.
pub(super) async fn read_events<F>(
    provider: &'static str,
    response: reqwest::Response,
    mut on_event: F,
) -> AgentResult<()>
where
    F: FnMut(&str, &Value) -> AgentResult<()>,
{
    let mut parser = relay_http::sse::EventParser::new();
    let mut body = response.bytes_stream();

    let mut handle = |event: relay_models::ServerEvent| -> AgentResult<()> {
        let payload = event.data.trim();
        if payload.is_empty() || payload == "[DONE]" {
            return Ok(());
        }
        let Ok(data) = serde_json::from_str::<Value>(payload) else {
            return Ok(());
        };
        // A provider that fails partway says so in the stream rather
        // than in the status, which has long since been 200.
        if event.event == "error" || data["type"] == "error" {
            return Err(AgentError::Provider {
                provider: provider.into(),
                status: 200,
                detail: data["error"]["message"]
                    .as_str()
                    .unwrap_or("the provider ended the stream with an error")
                    .to_string(),
            });
        }
        on_event(&event.event, &data)
    };

    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| AgentError::Transport {
            provider: provider.into(),
            detail: e.to_string(),
        })?;

        for event in parser.push(&String::from_utf8_lossy(&chunk)) {
            handle(event)?;
        }
    }

    // A stream that ended without its trailing blank line still sent
    // its last event, and for OpenAI that is the one carrying usage.
    if let Some(last) = parser.finish() {
        handle(last)?;
    }

    Ok(())
}

/// Anthropic's `stop_reason`, shared with the non-streaming path.
pub(super) fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refused,
        _ => StopReason::Done,
    }
}

fn text_of(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn number(value: &Value) -> u32 {
    value.as_u64().unwrap_or(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn anthropic(events: &[(&str, Value)]) -> (String, CompletionResponse) {
        let mut acc = AnthropicAccumulator::new();
        let mut shown = String::new();
        for (name, data) in events {
            if let Some(fragment) = acc.push(name, data) {
                shown.push_str(&fragment);
            }
        }
        (shown, acc.finish())
    }

    #[test]
    fn anthropic_text_arrives_in_pieces_and_adds_up() {
        let (shown, response) = anthropic(&[
            ("message_start", json!({"message": {"usage": {"input_tokens": 12}}})),
            ("content_block_start", json!({"index": 0, "content_block": {"type": "text"}})),
            ("content_block_delta", json!({"index": 0, "delta": {"type": "text_delta", "text": "Hello"}})),
            ("content_block_delta", json!({"index": 0, "delta": {"type": "text_delta", "text": ", world"}})),
            ("message_delta", json!({"delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}})),
        ]);

        // What was shown as it arrived and what is persisted afterwards
        // must be the same thing, or the panel and the history differ.
        assert_eq!(shown, "Hello, world");
        assert_eq!(response.text, "Hello, world");
        assert_eq!(response.stop_reason, StopReason::Done);
        assert_eq!(response.input_tokens, 12);
        assert_eq!(response.output_tokens, 5);
    }

    #[test]
    fn anthropic_cached_input_still_counts_as_input() {
        // It is charged at a different rate, not at nothing, and the
        // cost line that ignored it would understate every turn after
        // the first.
        let (_, response) = anthropic(&[(
            "message_start",
            json!({"message": {"usage": {
                "input_tokens": 10,
                "cache_creation_input_tokens": 100,
                "cache_read_input_tokens": 1000,
            }}}),
        )]);

        assert_eq!(response.input_tokens, 1110);
    }

    #[test]
    fn anthropic_tool_arguments_are_parsed_once_whole() {
        // The JSON is split across fragments at arbitrary points, so
        // parsing before the block ends is guaranteed to fail.
        let (shown, response) = anthropic(&[
            ("content_block_start", json!({"index": 0, "content_block": {
                "type": "tool_use", "id": "call_1", "name": "get_request",
            }})),
            ("content_block_delta", json!({"index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"id\":"}})),
            ("content_block_delta", json!({"index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"r1\"}"}})),
            ("message_delta", json!({"delta": {"stop_reason": "tool_use"}})),
        ]);

        assert_eq!(shown, "", "a tool call is not prose");
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "get_request");
        assert_eq!(response.tool_calls[0].input, json!({"id": "r1"}));
    }

    #[test]
    fn anthropic_keeps_prose_and_tool_calls_from_the_same_reply() {
        let (shown, response) = anthropic(&[
            ("content_block_start", json!({"index": 0, "content_block": {"type": "text"}})),
            ("content_block_delta", json!({"index": 0, "delta": {"type": "text_delta", "text": "Let me look."}})),
            ("content_block_start", json!({"index": 1, "content_block": {
                "type": "tool_use", "id": "c1", "name": "list_requests",
            }})),
            ("content_block_delta", json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "{}"}})),
            ("message_delta", json!({"delta": {"stop_reason": "tool_use"}})),
        ]);

        assert_eq!(shown, "Let me look.");
        assert_eq!(response.text, "Let me look.");
        assert_eq!(response.tool_calls.len(), 1);
    }

    #[test]
    fn anthropic_several_tool_calls_keep_their_own_arguments() {
        // Interleaved by index: fragments for call 1 can arrive
        // between two fragments for call 0.
        let (_, response) = anthropic(&[
            ("content_block_start", json!({"index": 0, "content_block": {"type": "tool_use", "id": "a", "name": "one"}})),
            ("content_block_start", json!({"index": 1, "content_block": {"type": "tool_use", "id": "b", "name": "two"}})),
            ("content_block_delta", json!({"index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"x\":1}"}})),
            ("content_block_delta", json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"y\":2}"}})),
        ]);

        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].input, json!({"x": 1}));
        assert_eq!(response.tool_calls[1].input, json!({"y": 2}));
    }

    #[test]
    fn anthropic_a_tool_taking_nothing_gets_an_empty_object() {
        // No argument fragments arrive at all, and `null` would fail
        // a schema that expects an object.
        let (_, response) = anthropic(&[(
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "tool_use", "id": "a", "name": "list_workspaces"}}),
        )]);

        assert_eq!(response.tool_calls[0].input, json!({}));
    }

    #[test]
    fn anthropic_thinking_is_not_shown_as_the_answer() {
        let (shown, response) = anthropic(&[
            ("content_block_delta", json!({"index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}})),
            ("content_block_delta", json!({"index": 1, "delta": {"type": "text_delta", "text": "Done."}})),
        ]);

        assert_eq!(shown, "Done.");
        assert_eq!(response.text, "Done.");
    }

    fn openai(chunks: &[Value]) -> (String, CompletionResponse) {
        let mut acc = OpenAiAccumulator::new();
        let mut shown = String::new();
        for chunk in chunks {
            if let Some(fragment) = acc.push(chunk) {
                shown.push_str(&fragment);
            }
        }
        (shown, acc.finish())
    }

    #[test]
    fn openai_text_arrives_in_pieces_and_adds_up() {
        let (shown, response) = openai(&[
            json!({"choices": [{"delta": {"content": "Hel"}}]}),
            json!({"choices": [{"delta": {"content": "lo"}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 7, "completion_tokens": 2}}),
        ]);

        assert_eq!(shown, "Hello");
        assert_eq!(response.text, "Hello");
        assert_eq!(response.stop_reason, StopReason::Done);
        assert_eq!(response.input_tokens, 7);
        assert_eq!(response.output_tokens, 2);
    }

    #[test]
    fn openai_tool_arguments_accumulate_by_index() {
        let (_, response) = openai(&[
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "get_request", "arguments": ""}}
            ]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"id\":"}}
            ]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "\"r1\"}"}}
            ]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ]);

        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call_1");
        assert_eq!(response.tool_calls[0].name, "get_request");
        assert_eq!(response.tool_calls[0].input, json!({"id": "r1"}));
    }

    #[test]
    fn openai_calls_without_a_finish_reason_are_still_calls() {
        // Some gateways never send one. Without this the loop would
        // treat a reply full of tool calls as a finished answer and
        // stop.
        let (_, response) = openai(&[json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "c", "function": {"name": "n", "arguments": "{}"}}
        ]}}]})]);

        assert_eq!(response.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn unparseable_arguments_do_not_lose_the_rest_of_the_turn() {
        // The tool reports it to the model, which usually retries. A
        // hard error here would throw away work already done.
        let (_, response) = openai(&[json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "c", "function": {"name": "n", "arguments": "{not json"}}
        ]}}]})]);

        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].input, Value::Null);
    }
}
