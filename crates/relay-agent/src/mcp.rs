//! Tools from the user's own MCP servers.
//!
//! Relay is a client here, never a server. A person points this at
//! something they run — a database inspector, their company's internal
//! tooling, a filesystem bridge — and its tools appear beside Relay's
//! own in the same list, with the same confirmation rules.
//!
//! Two transports: a child process over stdio for something local, and
//! streamable HTTP for something remote. Both come from the official
//! SDK rather than being hand-rolled, because JSON-RPC framing and the
//! initialize handshake are protocol nobody should write twice.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use serde::{Deserialize, Serialize};

use crate::error::{AgentError, AgentResult};
use crate::tool::{Tool, ToolOutcome, ToolRisk};

/// How to reach one MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "camelCase")]
pub enum McpServer {
    /// A program on this machine, spoken to over its stdin and stdout.
    #[serde(rename_all = "camelCase")]
    Stdio {
        /// A name for it, used to prefix its tools.
        name: String,
        /// The program to run.
        command: String,
        /// Its arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Extra environment for the child.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// A server reached over HTTP.
    ///
    /// Accepted and stored, but not yet connectable: the SDK's
    /// reqwest-backed transport will not resolve its trait bound
    /// against this workspace's feature set, and shipping a transport
    /// that silently half-works is worse than saying so. Local servers
    /// over stdio — which is most of them — work today.
    #[serde(rename_all = "camelCase")]
    Http {
        /// A name for it, used to prefix its tools.
        name: String,
        /// Where it lives.
        url: String,
    },
}

impl McpServer {
    /// What this server is called.
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio { name, .. } | Self::Http { name, .. } => name,
        }
    }
}

/// A connected server and the tools it offered.
pub struct McpConnection {
    name: String,
    service: Arc<RunningService<rmcp::RoleClient, ()>>,
    tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for McpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConnection")
            .field("name", &self.name)
            .field("tools", &self.tools.len())
            .finish()
    }
}

impl McpConnection {
    /// The server's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The tools it offered, ready to register.
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// Shuts the connection down, stopping a child process.
    pub async fn close(self) {
        if let Some(service) = Arc::into_inner(self.service) {
            let _ = service.cancel().await;
        }
    }
}

/// Connects to a server and lists what it offers.
pub async fn connect(server: &McpServer) -> AgentResult<McpConnection> {
    let name = server.name().to_string();

    let service: RunningService<rmcp::RoleClient, ()> = match server {
        McpServer::Stdio {
            command, args, env, ..
        } => {
            let mut process = tokio::process::Command::new(command);
            process.args(args);
            for (key, value) in env {
                process.env(key, value);
            }

            let transport = rmcp::transport::TokioChildProcess::new(process).map_err(|e| {
                AgentError::Internal(format!("could not start the MCP server '{name}': {e}"))
            })?;

            ().serve(transport).await.map_err(|e| {
                AgentError::Internal(format!("'{name}' did not complete the handshake: {e}"))
            })?
        }
        McpServer::Http { url, .. } => {
            let _ = url;
            return Err(AgentError::Internal(format!(
                "'{name}' is an HTTP MCP server, which Relay cannot reach yet. \
                 Local servers over stdio work; see docs/ASSISTANT.md."
            )));
        }
    };

    let listed = service
        .list_tools(None)
        .await
        .map_err(|e| AgentError::Internal(format!("'{name}' would not list its tools: {e}")))?;

    let service = Arc::new(service);
    let tools: Vec<Arc<dyn Tool>> = listed
        .tools
        .into_iter()
        .map(|tool| {
            Arc::new(McpTool {
                // Prefixed, so two servers offering `search` do not
                // collide and so the person can see where a tool came
                // from in the transcript.
                qualified: format!("{name}__{}", tool.name),
                remote: tool.name.to_string(),
                description: tool
                    .description
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| format!("A tool from the {name} server.")),
                schema: serde_json::Value::Object((*tool.input_schema).clone()),
                service: service.clone(),
            }) as Arc<dyn Tool>
        })
        .collect();

    Ok(McpConnection {
        name,
        service,
        tools,
    })
}

/// One tool, living on someone else's server.
struct McpTool {
    qualified: String,
    remote: String,
    description: String,
    schema: serde_json::Value,
    service: Arc<RunningService<rmcp::RoleClient, ()>>,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.qualified
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    /// Always confirmed.
    ///
    /// Relay cannot know what someone else's tool does. The protocol
    /// has hints — `readOnlyHint` and friends — but they are written by
    /// the same server being asked to behave, so trusting them to
    /// decide whether to ask the user is trusting the wrong party. A
    /// person who finds this tedious for a particular server can allow
    /// it for the session.
    fn risk(&self) -> ToolRisk {
        ToolRisk::Send
    }

    async fn run(&self, input: serde_json::Value) -> AgentResult<ToolOutcome> {
        let arguments = match input {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => {
                return Err(AgentError::BadToolInput {
                    tool: self.qualified.clone(),
                    detail: format!("expected an object, got {other}"),
                });
            }
        };

        let result = self
            .service
            // Built through the constructor: the struct is
            // non-exhaustive, and a literal would not compile against
            // the next version that adds a field.
            .call_tool({
                let mut params = CallToolRequestParams::new(self.remote.clone());
                params.arguments = arguments;
                params
            })
            .await
            .map_err(|e| AgentError::Internal(format!("{} failed: {e}", self.qualified)))?;

        // The protocol allows structured content and several blocks;
        // the model reads text, so anything else is described rather
        // than dropped silently.
        let mut text = String::new();
        for block in result.content.iter() {
            match block.as_text() {
                Some(part) => text.push_str(&part.text),
                None => text.push_str("<non-text content, not shown>"),
            }
        }

        if text.is_empty() {
            text.push_str("The tool returned nothing.");
        }

        Ok(if result.is_error.unwrap_or(false) {
            ToolOutcome::failed(text)
        } else {
            ToolOutcome::ok(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_definition_round_trips_through_its_tagged_form() {
        let stdio = McpServer::Stdio {
            name: "files".into(),
            command: "mcp-filesystem".into(),
            args: vec!["/home/me/notes".into()],
            env: BTreeMap::new(),
        };

        let json = serde_json::to_value(&stdio).unwrap();
        assert_eq!(json["transport"], "stdio");
        assert_eq!(json["command"], "mcp-filesystem");
        assert_eq!(serde_json::from_value::<McpServer>(json).unwrap(), stdio);
    }

    #[test]
    fn an_http_server_is_a_different_shape_with_the_same_name_field() {
        let http = McpServer::Http {
            name: "internal".into(),
            url: "https://tools.example.com/mcp".into(),
        };

        let json = serde_json::to_value(&http).unwrap();
        assert_eq!(json["transport"], "http");
        assert_eq!(http.name(), "internal");
    }
}
