//! The loop, against a scripted provider.
//!
//! No network: a provider returning a prepared sequence makes the
//! loop's own behaviour — approval, failure handling, the step ceiling —
//! testable without spending money or depending on what a real model
//! happens to decide today.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use relay_agent::agent_loop::{Approval, Approver, Progress};
use relay_agent::provider::{
    CompletionRequest, CompletionResponse, Message, ModelProvider, StopReason,
};
use relay_agent::tool::{Tool, ToolCall, ToolOutcome, ToolRegistry, ToolResult, ToolRisk};
use relay_agent::{Agent, AgentError, AgentResult};

/// Returns prepared responses in order.
struct Scripted {
    responses: Mutex<Vec<CompletionResponse>>,
}

impl Scripted {
    fn new(responses: Vec<CompletionResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses),
        })
    }
}

#[async_trait]
impl ModelProvider for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }
    fn default_model(&self) -> &'static str {
        "scripted-1"
    }
    async fn complete(&self, _request: CompletionRequest<'_>) -> AgentResult<CompletionResponse> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Ok(done("nothing left to say"));
        }
        Ok(responses.remove(0))
    }
}

fn done(text: &str) -> CompletionResponse {
    CompletionResponse {
        text: text.into(),
        tool_calls: Vec::new(),
        stop_reason: StopReason::Done,
        input_tokens: 10,
        output_tokens: 5,
    }
}

fn calls(name: &str, id: &str) -> CompletionResponse {
    CompletionResponse {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            input: serde_json::json!({}),
        }],
        stop_reason: StopReason::ToolUse,
        input_tokens: 10,
        output_tokens: 5,
    }
}

struct Recording {
    name: &'static str,
    risk: ToolRisk,
    ran: Arc<Mutex<Vec<String>>>,
    fails: bool,
}

#[async_trait]
impl Tool for Recording {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "records that it ran"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn risk(&self) -> ToolRisk {
        self.risk
    }
    async fn run(&self, _input: serde_json::Value) -> AgentResult<ToolOutcome> {
        self.ran.lock().unwrap().push(self.name.to_string());
        if self.fails {
            return Err(AgentError::Internal("the tool broke".into()));
        }
        Ok(ToolOutcome::ok("did the thing"))
    }
}

fn tool(name: &'static str, risk: ToolRisk, ran: Arc<Mutex<Vec<String>>>) -> Arc<dyn Tool> {
    Arc::new(Recording {
        name,
        risk,
        ran,
        fails: false,
    })
}

struct Denying;
#[async_trait]
impl Approver for Denying {
    async fn approve(&self, _call: &ToolCall, _risk: ToolRisk) -> Approval {
        Approval::Deny
    }
}

#[derive(Default)]
struct Watcher {
    events: Mutex<Vec<String>>,
}

impl Progress for Watcher {
    fn said(&self, text: &str) {
        self.events.lock().unwrap().push(format!("said:{text}"));
    }
    fn running(&self, call: &ToolCall) {
        self.events
            .lock()
            .unwrap()
            .push(format!("running:{}", call.name));
    }
    fn ran(&self, result: &ToolResult) {
        self.events
            .lock()
            .unwrap()
            .push(format!("ran:{}", result.call_id));
    }
}

#[tokio::test]
async fn a_plain_answer_needs_one_step() {
    let agent = Agent::new(
        Scripted::new(vec![done("Paris.")]),
        ToolRegistry::new(),
        "be helpful",
    );

    let turn = agent
        .run(&[], Message::user("capital of France?"))
        .await
        .unwrap();

    assert_eq!(turn.steps, 1);
    assert_eq!(turn.reply(), "Paris.");
    assert_eq!(turn.messages.len(), 2);
}

#[tokio::test]
async fn a_tool_call_runs_and_the_model_gets_another_turn() {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools.add(tool("list_requests", ToolRisk::Read, ran.clone()));

    let agent = Agent::new(
        Scripted::new(vec![calls("list_requests", "c1"), done("You have three.")]),
        tools,
        "be helpful",
    );

    let turn = agent
        .run(&[], Message::user("how many requests?"))
        .await
        .unwrap();

    assert_eq!(*ran.lock().unwrap(), vec!["list_requests"]);
    assert_eq!(turn.steps, 2);
    assert_eq!(turn.reply(), "You have three.");
    assert_eq!(turn.input_tokens, 20);
}

#[tokio::test]
async fn reading_runs_without_asking() {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools.add(tool("list_requests", ToolRisk::Read, ran.clone()));

    // Even an approver that refuses everything is never consulted.
    let agent = Agent::new(
        Scripted::new(vec![calls("list_requests", "c1"), done("ok")]),
        tools,
        "be helpful",
    )
    .with_approver(Arc::new(Denying));

    agent.run(&[], Message::user("look")).await.unwrap();
    assert_eq!(*ran.lock().unwrap(), vec!["list_requests"]);
}

#[tokio::test]
async fn sending_a_request_is_not_run_when_declined() {
    // The one that matters. A request Relay sends is indistinguishable
    // at the far end from one the user sent.
    let ran = Arc::new(Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools.add(tool("send_request", ToolRisk::Send, ran.clone()));

    let agent = Agent::new(
        Scripted::new(vec![
            calls("send_request", "c1"),
            done("I did not send it."),
        ]),
        tools,
        "be helpful",
    )
    .with_approver(Arc::new(Denying));

    let turn = agent.run(&[], Message::user("send it")).await.unwrap();

    assert!(ran.lock().unwrap().is_empty(), "it sent the request anyway");
    assert_eq!(turn.reply(), "I did not send it.");
}

#[tokio::test]
async fn a_declined_call_tells_the_model_not_to_retry() {
    // Otherwise it tries again next turn and the person is asked twice,
    // which is how a confirmation becomes noise people click through.
    let mut tools = ToolRegistry::new();
    tools.add(tool(
        "delete_request",
        ToolRisk::Destructive,
        Arc::new(Mutex::new(Vec::new())),
    ));

    let agent = Agent::new(
        Scripted::new(vec![calls("delete_request", "c1"), done("Understood.")]),
        tools,
        "be helpful",
    )
    .with_approver(Arc::new(Denying));

    let turn = agent.run(&[], Message::user("delete it")).await.unwrap();

    let refusal = turn
        .messages
        .iter()
        .flat_map(|m| &m.tool_results)
        .find(|r| r.call_id == "c1")
        .expect("a result for the declined call");

    assert!(refusal.outcome.is_error);
    assert!(
        refusal.outcome.content.contains("Do not try it again"),
        "{refusal:?}"
    );
}

#[tokio::test]
async fn a_tool_that_fails_is_reported_rather_than_ending_the_turn() {
    let mut tools = ToolRegistry::new();
    tools.add(Arc::new(Recording {
        name: "send_request",
        risk: ToolRisk::Read,
        ran: Arc::new(Mutex::new(Vec::new())),
        fails: true,
    }));

    let agent = Agent::new(
        Scripted::new(vec![
            calls("send_request", "c1"),
            done("That failed, here is why."),
        ]),
        tools,
        "be helpful",
    );

    let turn = agent.run(&[], Message::user("send it")).await.unwrap();

    let result = turn
        .messages
        .iter()
        .flat_map(|m| &m.tool_results)
        .next()
        .unwrap();
    assert!(result.outcome.is_error);
    assert!(result.outcome.content.contains("the tool broke"));
    assert_eq!(turn.reply(), "That failed, here is why.");
}

#[tokio::test]
async fn a_tool_the_model_invented_is_reported_to_it() {
    let agent = Agent::new(
        Scripted::new(vec![calls("rm_rf", "c1"), done("Sorry, my mistake.")]),
        ToolRegistry::new(),
        "be helpful",
    );

    let turn = agent.run(&[], Message::user("go")).await.unwrap();
    let result = turn
        .messages
        .iter()
        .flat_map(|m| &m.tool_results)
        .next()
        .unwrap();

    assert!(result.outcome.is_error);
    assert!(result.outcome.content.contains("rm_rf"));
}

#[tokio::test]
async fn a_model_that_never_finishes_is_stopped() {
    let responses = (0..60)
        .map(|i| calls("list_requests", &format!("c{i}")))
        .collect();
    let mut tools = ToolRegistry::new();
    tools.add(tool(
        "list_requests",
        ToolRisk::Read,
        Arc::new(Mutex::new(Vec::new())),
    ));

    let agent = Agent::new(Scripted::new(responses), tools, "be helpful");
    let error = agent
        .run(&[], Message::user("loop forever"))
        .await
        .unwrap_err();

    assert!(matches!(error, AgentError::TooManySteps(_)), "{error:?}");
}

#[tokio::test]
async fn the_interface_is_told_what_is_happening_as_it_happens() {
    // Without this the window is a spinner for however long the turn
    // takes, which on a real model is minutes.
    let watcher = Arc::new(Watcher::default());
    let mut tools = ToolRegistry::new();
    tools.add(tool(
        "list_requests",
        ToolRisk::Read,
        Arc::new(Mutex::new(Vec::new())),
    ));

    let agent = Agent::new(
        Scripted::new(vec![calls("list_requests", "c1"), done("Three.")]),
        tools,
        "be helpful",
    )
    .with_progress(watcher.clone());

    agent.run(&[], Message::user("how many?")).await.unwrap();

    assert_eq!(
        *watcher.events.lock().unwrap(),
        vec!["running:list_requests", "ran:c1", "said:Three."]
    );
}

#[tokio::test]
async fn history_is_carried_in_but_not_returned_again() {
    // The caller persists what comes back; returning the history too
    // would duplicate every earlier message in the database.
    let history = vec![Message::user("hello"), Message::assistant("hello to you")];

    let agent = Agent::new(
        Scripted::new(vec![done("still here")]),
        ToolRegistry::new(),
        "be helpful",
    );
    let turn = agent
        .run(&history, Message::user("still there?"))
        .await
        .unwrap();

    assert_eq!(turn.messages.len(), 2);
    assert_eq!(turn.messages[0].text, "still there?");
}
