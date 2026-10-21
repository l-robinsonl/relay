//! Pre-request scripts: what they may change, and what they may not.

use relay_script::{ScriptEngine, ScriptRequest};
use std::collections::BTreeMap;

fn request() -> ScriptRequest {
    ScriptRequest {
        method: "GET".into(),
        url: "https://api.example.com/users".into(),
        headers: serde_json::Map::new(),
    }
}

fn run(source: &str) -> relay_script::ScriptOutcome {
    ScriptEngine::new().run_pre_request(source, Some(&request()), &BTreeMap::new())
}

#[test]
fn a_header_can_be_added() {
    let outcome = run("relay.setHeader('X-Request-Id', 'abc123');");

    assert_eq!(outcome.error, None);
    assert_eq!(
        outcome.request_patch.headers.get("X-Request-Id"),
        Some(&"abc123".to_string())
    );
}

#[test]
fn a_signature_can_be_computed_from_the_request() {
    // The reason this feature exists: signing a request needs the method
    // and path, and the result has to go in a header.
    let outcome = run("const stamp = '1700000000';
         relay.setHeader('X-Timestamp', stamp);
         relay.setHeader('X-Signature', `${request.method}:${request.url}:${stamp}`);");

    assert_eq!(outcome.error, None);
    assert_eq!(
        outcome.request_patch.headers.get("X-Signature"),
        Some(&"GET:https://api.example.com/users:1700000000".to_string())
    );
}

#[test]
fn a_header_can_be_removed() {
    let outcome = run("relay.removeHeader('User-Agent');");

    assert_eq!(outcome.error, None);
    assert_eq!(outcome.request_patch.removed_headers, vec!["User-Agent"]);
}

#[test]
fn removing_a_header_cancels_setting_it() {
    // Last word wins, rather than both instructions being applied in an
    // order the author cannot see.
    let outcome = run("relay.setHeader('X-Try', '1'); relay.removeHeader('X-Try');");

    assert!(outcome.request_patch.headers.is_empty());
    assert_eq!(outcome.request_patch.removed_headers, vec!["X-Try"]);
}

#[test]
fn the_url_method_body_and_query_can_be_set() {
    let outcome = run("relay.setMethod('post');
         relay.setUrl('https://api.example.com/v2/users');
         relay.setBody('{\"ok\":true}');
         relay.setQueryParam('page', '2');
         relay.removeQueryParam('debug');");

    let patch = outcome.request_patch;
    assert_eq!(patch.method.as_deref(), Some("POST"));
    assert_eq!(
        patch.url.as_deref(),
        Some("https://api.example.com/v2/users")
    );
    assert_eq!(patch.body.as_deref(), Some("{\"ok\":true}"));
    assert_eq!(patch.query.get("page"), Some(&"2".to_string()));
    assert_eq!(patch.removed_query, vec!["debug"]);
}

#[test]
fn a_variable_can_be_set_for_the_request_to_use() {
    let outcome = run("relay.setVariable('nonce', 'n-42');");

    assert_eq!(outcome.variables.get("nonce"), Some(&"n-42".to_string()));
}

#[test]
fn there_is_no_response_to_read() {
    let outcome = run("if (typeof response !== 'undefined') { throw new Error('leaked'); }");
    assert_eq!(outcome.error, None);
}

#[test]
fn the_authorization_header_already_on_the_request_stays_hidden() {
    // A script may construct a credential; it may not read one that is
    // already there.
    let mut spec = relay_models::HttpRequestSpec::new(
        relay_models::HttpMethod::Get,
        "https://api.example.com/users",
    );
    spec.auth = relay_models::AuthConfig::Bearer {
        token: "real-token".into(),
    };
    spec.headers = vec![relay_models::KeyValue::new(
        "Authorization",
        "Bearer real-token",
    )];

    let outcome = ScriptEngine::new().run_pre_request(
        "relay.setHeader('X-Echo', String(request.headers['authorization']));",
        Some(&ScriptRequest::from_spec(&spec)),
        &BTreeMap::new(),
    );

    let echoed = outcome
        .request_patch
        .headers
        .get("X-Echo")
        .cloned()
        .unwrap_or_default();
    assert!(
        !echoed.contains("real-token"),
        "the token was readable: {echoed}"
    );
}

#[test]
fn a_post_response_script_is_told_why_it_cannot_change_the_request() {
    let outcome = ScriptEngine::new().run(
        "relay.setHeader('X-Too-Late', '1');",
        None,
        Some(&request()),
        &BTreeMap::new(),
    );

    let error = outcome.error.unwrap_or_default();
    assert!(error.contains("pre-request"), "{error}");
    assert!(outcome.request_patch.is_empty());
}

#[test]
fn a_runaway_script_cannot_fill_memory_through_the_patch() {
    let outcome = run("for (let i = 0; i < 100000; i++) relay.setHeader('H' + i, 'x');");

    assert!(
        outcome.request_patch.headers.len() <= 200,
        "unbounded: {}",
        outcome.request_patch.headers.len()
    );
}

#[test]
fn a_failing_script_still_reports_what_it_managed_to_set() {
    // Partial work is visible, so an author can see how far it got.
    let outcome = run("relay.setHeader('X-One', '1'); throw new Error('stopped here');");

    assert!(outcome.error.unwrap_or_default().contains("stopped here"));
    assert_eq!(
        outcome.request_patch.headers.get("X-One"),
        Some(&"1".to_string())
    );
}
