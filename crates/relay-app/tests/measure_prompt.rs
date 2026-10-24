//! Not a test of behaviour — a measurement, for pricing a hosted
//! assistant. Prints what one turn sends before the conversation even
//! starts: the system prompt plus every tool definition, which is paid
//! for on every single request unless it is cached.
//!
//! Skipped unless RELAY_MEASURE=1.

use std::sync::Arc;

use relay_app::{MemorySecretStore, RelayService};
use relay_http::HttpEngine;
use relay_storage::Database;

#[tokio::test]
async fn what_one_turn_sends() {
    if std::env::var("RELAY_MEASURE").ok().as_deref() != Some("1") {
        return;
    }

    let service = RelayService::new(
        Database::open_in_memory().unwrap(),
        HttpEngine::with_defaults(),
        Arc::new(MemorySecretStore::new()),
    );
    let workspace = service.active_workspace().await.unwrap();
    let registry = relay_app::assistant::tools::registry(service.clone(), workspace.id.clone());

    let specs: Vec<_> = registry
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name(),
                "description": tool.description(),
                "input_schema": tool.input_schema(),
            })
        })
        .collect();

    let tools_json = serde_json::to_string(&specs).unwrap();
    println!("MEASURE tool_count={}", specs.len());
    println!("MEASURE tools_chars={}", tools_json.len());
    for spec in &specs {
        println!(
            "MEASURE   {:24} {} chars",
            spec["name"].as_str().unwrap(),
            serde_json::to_string(spec).unwrap().len()
        );
    }
}
