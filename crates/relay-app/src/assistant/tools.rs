//! What the assistant can do to a Relay workspace.
//!
//! One rule governs this file: **the assistant never sees a secret
//! variable's value.** Scripts have been held to that since they
//! existed, and an agent that can be steered by whatever a web page
//! said is a weaker vessel than a script the user wrote. Every tool
//! that touches variables reports names and whether each is secret,
//! never the value — and the values still reach the wire, because
//! resolution happens below this layer at send time.

use std::sync::Arc;

use async_trait::async_trait;
use relay_agent::tool::{Tool, ToolOutcome, ToolRegistry, ToolRisk};
use relay_agent::{AgentError, AgentResult};
use relay_models::{
    CollectionId, EnvironmentId, ExecutionId, HttpExchange, HttpMethod, KeyValue, RequestBody,
    RequestDoc, RequestId, ResponseBody, WorkspaceId,
};
use serde_json::{Value, json};

use crate::service::RelayService;

/// How much of a response body the model is shown.
///
/// A 2 MB JSON payload would fill the context and be charged for on
/// every subsequent turn. The model is told it was truncated and can
/// ask for a particular part.
const BODY_PREVIEW: usize = 4_000;

/// Everything the assistant can do, bound to one workspace.
pub fn registry(service: RelayService, workspace: WorkspaceId) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    let at = |t: Arc<dyn Tool>| t;

    tools
        .add(at(Arc::new(ListRequests {
            service: service.clone(),
            workspace: workspace.clone(),
        })))
        .add(at(Arc::new(GetRequest {
            service: service.clone(),
        })))
        .add(at(Arc::new(CreateRequest {
            service: service.clone(),
            workspace: workspace.clone(),
        })))
        .add(at(Arc::new(UpdateRequest {
            service: service.clone(),
        })))
        .add(at(Arc::new(DeleteRequest {
            service: service.clone(),
        })))
        .add(at(Arc::new(SendRequest {
            service: service.clone(),
        })))
        .add(at(Arc::new(ListCollections {
            service: service.clone(),
            workspace: workspace.clone(),
        })))
        .add(at(Arc::new(ListEnvironments {
            service: service.clone(),
            workspace: workspace.clone(),
        })))
        .add(at(Arc::new(ListHistory { service, workspace })));

    tools
}

/// Reads a required string argument.
fn arg<'a>(input: &'a Value, tool: &str, name: &str) -> AgentResult<&'a str> {
    input
        .get(name)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AgentError::BadToolInput {
            tool: tool.into(),
            detail: format!("'{name}' is required"),
        })
}

/// Renders a request compactly. The model does not need every field.
fn summarise(doc: &RequestDoc) -> Value {
    json!({
        "id": doc.id.as_str(),
        "name": doc.name,
        "method": doc.method.as_str(),
        "url": doc.url,
        "collectionId": doc.collection_id.as_ref().map(|c| c.as_str()),
    })
}

// --- reading ---------------------------------------------------------------

struct ListRequests {
    service: RelayService,
    workspace: WorkspaceId,
}

#[async_trait]
impl Tool for ListRequests {
    fn name(&self) -> &str {
        "list_requests"
    }
    fn description(&self) -> &str {
        "List every saved request in this workspace, with its id, name, method and URL. \
         Start here when the user refers to a request by name rather than by id."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }
    async fn run(&self, _input: Value) -> AgentResult<ToolOutcome> {
        let requests = self
            .service
            .request_list(self.workspace.clone())
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        Ok(ToolOutcome::ok(
            json!({
                "count": requests.len(),
                "requests": requests.iter().map(summarise).collect::<Vec<_>>(),
            })
            .to_string(),
        ))
    }
}

struct GetRequest {
    service: RelayService,
}

#[async_trait]
impl Tool for GetRequest {
    fn name(&self) -> &str {
        "get_request"
    }
    fn description(&self) -> &str {
        "Read one saved request in full: headers, query parameters, body, auth type and \
         scripts. Variable references like {{baseUrl}} are returned as written, not resolved."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string", "description": "The request id." } },
            "required": ["id"],
            "additionalProperties": false,
        })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }
    async fn run(&self, input: Value) -> AgentResult<ToolOutcome> {
        let id = arg(&input, "get_request", "id")?;
        let doc = self
            .service
            .request_get(RequestId::from(id))
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        // A header may hold a credential typed in directly rather than
        // referenced as a variable, so the same names history redacts
        // are redacted here.
        const SENSITIVE: [&str; 4] = ["authorization", "cookie", "x-api-key", "x-auth-token"];
        let headers: Vec<Value> = doc
            .headers
            .iter()
            .map(|h| {
                let hidden = SENSITIVE.contains(&h.key.to_ascii_lowercase().as_str());
                json!({
                    "key": h.key,
                    "value": if hidden { relay_models::REDACTED } else { h.value.as_str() },
                    "enabled": h.enabled,
                })
            })
            .collect();

        Ok(ToolOutcome::ok(
            json!({
                "id": doc.id.as_str(),
                "name": doc.name,
                "method": doc.method.as_str(),
                "url": doc.url,
                "headers": headers,
                "queryParams": doc.query_params,
                "body": doc.body,
                "authType": format!("{:?}", doc.auth).split(['(', ' ']).next().unwrap_or("None").to_string(),
                "preRequestScript": doc.scripts.pre_request,
                "testScript": doc.scripts.post_response,
            })
            .to_string(),
        ))
    }
}

struct ListCollections {
    service: RelayService,
    workspace: WorkspaceId,
}

#[async_trait]
impl Tool for ListCollections {
    fn name(&self) -> &str {
        "list_collections"
    }
    fn description(&self) -> &str {
        "List the folders and collections in this workspace, with their ids and parents."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }
    async fn run(&self, _input: Value) -> AgentResult<ToolOutcome> {
        let collections = self
            .service
            .collection_list(self.workspace.clone())
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        Ok(ToolOutcome::ok(
            json!({
                "collections": collections.iter().map(|c| json!({
                    "id": c.id.as_str(),
                    "name": c.name,
                    "parentId": c.parent_id.as_ref().map(|p| p.as_str()),
                })).collect::<Vec<_>>(),
            })
            .to_string(),
        ))
    }
}

struct ListEnvironments {
    service: RelayService,
    workspace: WorkspaceId,
}

#[async_trait]
impl Tool for ListEnvironments {
    fn name(&self) -> &str {
        "list_environments"
    }
    fn description(&self) -> &str {
        "List the environments and the variables each defines. Variable VALUES are only \
         returned for non-secret variables; a secret's value is never readable, by you or \
         by any script. Secrets are still substituted when a request is actually sent."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }
    async fn run(&self, _input: Value) -> AgentResult<ToolOutcome> {
        let environments = self
            .service
            .environment_list(self.workspace.clone())
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        Ok(ToolOutcome::ok(
            json!({
                "environments": environments.iter().map(|environment| json!({
                    "id": environment.id.as_str(),
                    "name": environment.name,
                    "variables": environment.variables.iter().map(|variable| json!({
                        "key": variable.key,
                        "secret": variable.secret,
                        "enabled": variable.enabled,
                        // The whole point. A secret's value does not
                        // leave the credential store for anything but
                        // an outgoing request.
                        "value": if variable.secret { Value::Null } else { json!(variable.value) },
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
            .to_string(),
        ))
    }
}

struct ListHistory {
    service: RelayService,
    workspace: WorkspaceId,
}

#[async_trait]
impl Tool for ListHistory {
    fn name(&self) -> &str {
        "list_history"
    }
    fn description(&self) -> &str {
        "The most recent requests actually sent, with status and timing. Useful for \
         'why did that fail' and 'what did I just send'."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "How many, newest first. Default 20.", "minimum": 1, "maximum": 100 }
            },
            "additionalProperties": false,
        })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }
    async fn run(&self, input: Value) -> AgentResult<ToolOutcome> {
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .min(100) as u32;

        let entries = self
            .service
            .history_list(self.workspace.clone(), limit, 0)
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        Ok(ToolOutcome::ok(
            json!({
                "entries": entries.iter().map(|entry| json!({
                    "method": entry.method.as_str(),
                    "url": entry.url,
                    "status": entry.status,
                    "durationMs": entry.duration_ms,
                    "error": entry.error,
                    "executedAt": entry.executed_at.as_millis(),
                })).collect::<Vec<_>>(),
            })
            .to_string(),
        ))
    }
}

// --- writing ---------------------------------------------------------------

struct CreateRequest {
    service: RelayService,
    workspace: WorkspaceId,
}

#[async_trait]
impl Tool for CreateRequest {
    fn name(&self) -> &str {
        "create_request"
    }
    fn description(&self) -> &str {
        "Create a saved request. Use {{variableName}} in the URL, headers or body to \
         reference a workspace or environment variable rather than hard-coding a value."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "method": { "type": "string", "description": "GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS or any custom method." },
                "url": { "type": "string" },
                "collectionId": { "type": "string", "description": "Optional folder to put it in." },
                "headers": {
                    "type": "object",
                    "description": "Header name to value.",
                    "additionalProperties": { "type": "string" },
                },
                "body": { "type": "string", "description": "Request body as text. JSON should be sent as a JSON string." },
                "contentType": { "type": "string", "description": "json, text, xml or none. Defaults to json when a body is given." },
            },
            "required": ["name", "method", "url"],
            "additionalProperties": false,
        })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Write
    }
    async fn run(&self, input: Value) -> AgentResult<ToolOutcome> {
        let name = arg(&input, "create_request", "name")?;
        let method = arg(&input, "create_request", "method")?;
        let url = arg(&input, "create_request", "url")?;

        let collection = input
            .get("collectionId")
            .and_then(Value::as_str)
            .filter(|c| !c.trim().is_empty())
            .map(CollectionId::from);

        let mut doc = self
            .service
            .request_create(self.workspace.clone(), name.to_string(), collection)
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        doc.method = HttpMethod::from(method.to_ascii_uppercase().as_str());
        doc.url = url.to_string();
        apply_headers(&mut doc, &input);
        apply_body(&mut doc, &input);

        let saved = self
            .service
            .request_save(doc)
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        Ok(ToolOutcome::ok(summarise(&saved).to_string()))
    }
}

struct UpdateRequest {
    service: RelayService,
}

#[async_trait]
impl Tool for UpdateRequest {
    fn name(&self) -> &str {
        "update_request"
    }
    fn description(&self) -> &str {
        "Change a saved request. Only the fields you give are altered; everything else is \
         left alone. Headers given replace headers of the same name and leave the rest."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "name": { "type": "string" },
                "method": { "type": "string" },
                "url": { "type": "string" },
                "headers": { "type": "object", "additionalProperties": { "type": "string" } },
                "body": { "type": "string" },
                "contentType": { "type": "string" },
            },
            "required": ["id"],
            "additionalProperties": false,
        })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Write
    }
    async fn run(&self, input: Value) -> AgentResult<ToolOutcome> {
        let id = arg(&input, "update_request", "id")?;
        let mut doc = self
            .service
            .request_get(RequestId::from(id))
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        if let Some(name) = input.get("name").and_then(Value::as_str) {
            doc.name = name.to_string();
        }
        if let Some(method) = input.get("method").and_then(Value::as_str) {
            doc.method = HttpMethod::from(method.to_ascii_uppercase().as_str());
        }
        if let Some(url) = input.get("url").and_then(Value::as_str) {
            doc.url = url.to_string();
        }
        apply_headers(&mut doc, &input);
        apply_body(&mut doc, &input);

        let saved = self
            .service
            .request_save(doc)
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        Ok(ToolOutcome::ok(summarise(&saved).to_string()))
    }
}

/// Merges headers from tool input, replacing by name and keeping the rest.
fn apply_headers(doc: &mut RequestDoc, input: &Value) {
    let Some(headers) = input.get("headers").and_then(Value::as_object) else {
        return;
    };

    for (name, value) in headers {
        let Some(value) = value.as_str() else {
            continue;
        };
        match doc
            .headers
            .iter_mut()
            .find(|h| h.key.eq_ignore_ascii_case(name))
        {
            Some(existing) => {
                existing.value = value.to_string();
                existing.enabled = true;
            }
            None => doc.headers.push(KeyValue::new(name, value)),
        }
    }
}

/// Sets the body, in whichever mode was asked for.
fn apply_body(doc: &mut RequestDoc, input: &Value) {
    let Some(text) = input.get("body").and_then(Value::as_str) else {
        return;
    };

    let mode = input
        .get("contentType")
        .and_then(Value::as_str)
        .unwrap_or("json")
        .to_ascii_lowercase();

    doc.body = match mode.as_str() {
        "none" | "" => RequestBody::None,
        "text" => RequestBody::Text { text: text.into() },
        "xml" => RequestBody::Xml { text: text.into() },
        _ => RequestBody::Json { text: text.into() },
    };
}

// --- destroying ------------------------------------------------------------

struct DeleteRequest {
    service: RelayService,
}

#[async_trait]
impl Tool for DeleteRequest {
    fn name(&self) -> &str {
        "delete_request"
    }
    fn description(&self) -> &str {
        "Delete a saved request. The person is asked to confirm before this runs."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"],
            "additionalProperties": false,
        })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Destructive
    }
    async fn run(&self, input: Value) -> AgentResult<ToolOutcome> {
        let id = arg(&input, "delete_request", "id")?;
        self.service
            .request_delete(RequestId::from(id))
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        Ok(ToolOutcome::ok(format!("Deleted {id}.")))
    }
}

// --- sending ---------------------------------------------------------------

struct SendRequest {
    service: RelayService,
}

#[async_trait]
impl Tool for SendRequest {
    fn name(&self) -> &str {
        "send_request"
    }
    fn description(&self) -> &str {
        "Actually send a saved request and return the response. This reaches a real server \
         with the user's credentials, so they are asked to confirm first. Long response \
         bodies are truncated; say so if you need a particular part."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "environmentId": { "type": "string", "description": "Which environment's variables to resolve with." },
            },
            "required": ["id"],
            "additionalProperties": false,
        })
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Send
    }
    async fn run(&self, input: Value) -> AgentResult<ToolOutcome> {
        let id = arg(&input, "send_request", "id")?;
        let doc = self
            .service
            .request_get(RequestId::from(id))
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        let environment = input
            .get("environmentId")
            .and_then(Value::as_str)
            .filter(|e| !e.trim().is_empty())
            .map(EnvironmentId::from);

        let exchange = self
            .service
            .request_send(
                ExecutionId::new(),
                doc,
                environment,
                Arc::new(relay_http::NoopProgress),
            )
            .await
            .map_err(|e| AgentError::Internal(e.message))?;

        Ok(ToolOutcome::ok(describe(&exchange)))
    }
}

/// Renders an exchange for the model, bounded.
fn describe(exchange: &HttpExchange) -> String {
    match exchange {
        HttpExchange::Success { response, .. } => {
            let (body, truncated) = match &response.body {
                ResponseBody::Text { text } if text.len() > BODY_PREVIEW => {
                    (text.chars().take(BODY_PREVIEW).collect::<String>(), true)
                }
                ResponseBody::Text { text } => (text.clone(), false),
                other => (format!("<{} body, not shown>", kind_of(other)), false),
            };

            json!({
                "status": response.status,
                "statusText": response.status_text,
                "durationMs": response.timings.total_ms,
                "sizeBytes": response.size_bytes,
                "body": body,
                "truncated": truncated,
            })
            .to_string()
        }
        HttpExchange::Failure { error, .. } => {
            json!({ "failed": true, "error": error.message }).to_string()
        }
        HttpExchange::Cancelled { .. } => json!({ "cancelled": true }).to_string(),
    }
}

fn kind_of(body: &ResponseBody) -> &'static str {
    match body {
        ResponseBody::Text { .. } => "text",
        ResponseBody::Binary { .. } => "binary",
        ResponseBody::File { .. } => "file on disk",
        ResponseBody::Truncated { .. } => "truncated",
        ResponseBody::Empty => "empty",
    }
}
