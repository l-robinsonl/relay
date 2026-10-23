//! The MCP client, against a real server over a real pipe.
//!
//! The server is a few lines of Python speaking JSON-RPC on stdin and
//! stdout. A mocked transport would prove the types line up; this
//! proves the handshake, the framing and a round trip.

use relay_agent::mcp::{McpServer, connect};
use relay_agent::tool::ToolRisk;
use std::collections::BTreeMap;

/// Writes a minimal MCP server and returns its path.
fn write_server() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("relay-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server.py");

    std::fs::write(
        &path,
        r#"
import json, sys

TOOLS = [{
    "name": "echo",
    "description": "Repeats what it is given.",
    "inputSchema": {
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
    },
}]

def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    request = json.loads(line)
    method, rid = request.get("method"), request.get("id")

    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": request["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "probe", "version": "0"},
        }})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": TOOLS}})
    elif method == "tools/call":
        text = request["params"].get("arguments", {}).get("text", "")
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": f"you said: {text}"}],
            "isError": False,
        }})
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid,
              "error": {"code": -32601, "message": method}})
"#,
    )
    .unwrap();

    path
}

fn server() -> McpServer {
    McpServer::Stdio {
        name: "probe".into(),
        command: "python3".into(),
        args: vec![write_server().to_string_lossy().into_owned()],
        env: BTreeMap::new(),
    }
}

#[tokio::test]
async fn a_local_server_offers_its_tools() {
    let connection = connect(&server()).await.expect("could not connect");

    assert_eq!(connection.name(), "probe");
    assert_eq!(connection.tools().len(), 1);

    let tool = &connection.tools()[0];
    // Prefixed with the server's name, so two servers offering the same
    // tool do not collide and the transcript says where it came from.
    assert_eq!(tool.name(), "probe__echo");
    assert_eq!(tool.description(), "Repeats what it is given.");
    assert_eq!(tool.input_schema()["properties"]["text"]["type"], "string");

    connection.close().await;
}

#[tokio::test]
async fn someone_elses_tool_is_always_confirmed() {
    // Relay cannot know what another server's tool does. The protocol
    // has hints, but they are written by the same server being asked to
    // behave, so trusting them to decide whether to ask the person is
    // trusting the wrong party.
    let connection = connect(&server()).await.expect("could not connect");
    assert_eq!(connection.tools()[0].risk(), ToolRisk::Send);
    assert!(connection.tools()[0].risk().needs_confirmation());
    connection.close().await;
}

#[tokio::test]
async fn calling_it_returns_what_the_server_said() {
    let connection = connect(&server()).await.expect("could not connect");

    let outcome = connection.tools()[0]
        .run(serde_json::json!({ "text": "hello" }))
        .await
        .expect("the call failed");

    assert_eq!(outcome.content, "you said: hello");
    assert!(!outcome.is_error);

    connection.close().await;
}

#[tokio::test]
async fn a_server_that_will_not_start_is_reported_by_name() {
    let missing = McpServer::Stdio {
        name: "ghost".into(),
        command: "definitely-not-a-real-program-xyz".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };

    let error = connect(&missing).await.unwrap_err().to_string();
    assert!(error.contains("ghost"), "{error}");
}

#[tokio::test]
async fn an_http_server_says_it_is_not_supported_rather_than_failing_obscurely() {
    let http = McpServer::Http {
        name: "remote".into(),
        url: "https://tools.example.com/mcp".into(),
    };

    let error = connect(&http).await.unwrap_err().to_string();
    assert!(error.contains("cannot reach yet"), "{error}");
    assert!(
        error.contains("stdio"),
        "it should say what does work: {error}"
    );
}
