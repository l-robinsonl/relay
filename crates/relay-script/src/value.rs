//! What goes into a script, and what comes back out.

use relay_models::{HttpRequestSpec, HttpResponseData, ResponseBody};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The response, as a script sees it.
///
/// A plain data structure rather than a live handle: a script gets a
/// snapshot it can read, not an object that can reach back into the
/// application.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptResponse {
    /// The HTTP status code.
    pub status: u16,
    /// Its reason phrase, e.g. `OK`.
    pub status_text: String,
    /// True for 2xx, matching what `fetch` calls `ok`.
    pub ok: bool,
    /// Header names lower-cased, because HTTP header names are
    /// case-insensitive and a script should not have to guess.
    pub headers: serde_json::Map<String, serde_json::Value>,
    /// The body as text, empty when it was binary or spilled to disk.
    pub body: String,
    /// Round-trip time in milliseconds.
    pub time: f64,
    /// Bytes received.
    pub size: u64,
    /// The URL that produced it, after any redirects.
    pub url: String,
}

impl ScriptResponse {
    /// Builds the script's view of a response.
    pub fn from_response(response: &HttpResponseData) -> Self {
        let mut headers = serde_json::Map::new();
        for header in &response.headers {
            headers.insert(
                header.key.to_ascii_lowercase(),
                serde_json::Value::String(header.value.clone()),
            );
        }

        // A binary or spooled body is not given to the script. Base64 in
        // a test assertion helps nobody, and a spooled body is on disk
        // precisely because it was too big to hold in memory.
        let body = match &response.body {
            ResponseBody::Text { text } => text.clone(),
            ResponseBody::File { preview, .. } => preview.clone(),
            _ => String::new(),
        };

        Self {
            status: response.status,
            status_text: response.status_text.clone(),
            ok: (200..300).contains(&response.status),
            headers,
            body,
            time: response.timings.total_ms,
            size: response.size_bytes,
            url: response.final_url.clone(),
        }
    }
}

/// The request, as a script sees it.
///
/// Built from the already-redacted spec, so a script cannot read the
/// bearer token that was sent even though it can see the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptRequest {
    /// The method that was sent.
    pub method: String,
    /// The URL that was requested.
    pub url: String,
    /// Header names lower-cased.
    pub headers: serde_json::Map<String, serde_json::Value>,
}

impl ScriptRequest {
    /// Builds the script's view of a request.
    pub fn from_spec(spec: &HttpRequestSpec) -> Self {
        let redacted = spec.redacted();
        let mut headers = serde_json::Map::new();
        for header in redacted.headers.iter().filter(|h| h.is_active()) {
            headers.insert(
                header.key.to_ascii_lowercase(),
                serde_json::Value::String(header.value.clone()),
            );
        }

        Self {
            method: redacted.method.to_string(),
            url: redacted.url,
            headers,
        }
    }
}

/// One assertion's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Assertion {
    /// Whether it held.
    pub passed: bool,
    /// Why it failed; empty when it passed.
    pub message: String,
}

/// A named group of assertions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TestCase {
    /// What the author called it.
    pub name: String,
    /// Its assertions, in order.
    pub assertions: Vec<Assertion>,
    /// Set when the body threw something that was not an assertion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TestCase {
    /// True when every assertion held and nothing threw.
    pub fn passed(&self) -> bool {
        self.error.is_none() && self.assertions.iter().all(|a| a.passed)
    }

    /// The first failure, for a one-line summary.
    pub fn first_failure(&self) -> Option<&str> {
        if let Some(error) = &self.error {
            return Some(error);
        }
        self.assertions
            .iter()
            .find(|a| !a.passed)
            .map(|a| a.message.as_str())
    }
}

/// How noisy a console line was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ConsoleLevel {
    /// console.log, info or debug.
    Log,
    /// console.warn.
    Warn,
    /// console.error.
    Error,
}

/// One line a script printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConsoleLine {
    /// How noisy it was.
    pub level: ConsoleLevel,
    /// What was printed.
    pub text: String,
}

/// Everything a script run produced.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptOutcome {
    /// Named test groups, in the order they ran.
    /// Named groups, in the order they ran.
    pub tests: Vec<TestCase>,
    /// Assertions made outside any test group.
    /// Assertions made outside any group.
    pub loose_assertions: Vec<Assertion>,
    /// Anything the script printed.
    /// Anything printed.
    pub console: Vec<ConsoleLine>,
    /// Variables the script set, to be written back by the caller.
    /// Variables the script asked to set.
    pub variables: std::collections::BTreeMap<String, String>,
    /// Set when the script itself failed — a syntax error, a timeout, or
    /// an exception outside any test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How long it ran, in milliseconds.
    /// How long the run took.
    pub duration_ms: f64,
}

impl ScriptOutcome {
    /// True when nothing failed.
    pub fn passed(&self) -> bool {
        self.error.is_none()
            && self.tests.iter().all(TestCase::passed)
            && self.loose_assertions.iter().all(|a| a.passed)
    }

    /// How many assertions held.
    pub fn passed_count(&self) -> usize {
        self.tests
            .iter()
            .flat_map(|t| &t.assertions)
            .chain(&self.loose_assertions)
            .filter(|a| a.passed)
            .count()
    }

    /// How many did not.
    pub fn failed_count(&self) -> usize {
        self.tests
            .iter()
            .flat_map(|t| &t.assertions)
            .chain(&self.loose_assertions)
            .filter(|a| !a.passed)
            .count()
            + self.tests.iter().filter(|t| t.error.is_some()).count()
    }

    /// True when the script did nothing worth reporting.
    pub fn is_empty(&self) -> bool {
        self.tests.is_empty()
            && self.loose_assertions.is_empty()
            && self.console.is_empty()
            && self.error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_with_a_failed_assertion_has_not_passed() {
        let case = TestCase {
            name: "x".into(),
            assertions: vec![
                Assertion {
                    passed: true,
                    message: String::new(),
                },
                Assertion {
                    passed: false,
                    message: "expected 200 but got 404".into(),
                },
            ],
            error: None,
        };

        assert!(!case.passed());
        assert_eq!(case.first_failure(), Some("expected 200 but got 404"));
    }

    #[test]
    fn a_thrown_error_counts_as_a_failure_even_with_no_assertions() {
        let case = TestCase {
            name: "x".into(),
            assertions: vec![],
            error: Some("response.json is not a function".into()),
        };
        assert!(!case.passed());
        assert!(case.first_failure().unwrap().contains("not a function"));
    }

    #[test]
    fn an_outcome_counts_assertions_inside_and_outside_tests() {
        let outcome = ScriptOutcome {
            tests: vec![TestCase {
                name: "grouped".into(),
                assertions: vec![
                    Assertion {
                        passed: true,
                        message: String::new(),
                    },
                    Assertion {
                        passed: false,
                        message: "no".into(),
                    },
                ],
                error: None,
            }],
            loose_assertions: vec![Assertion {
                passed: true,
                message: String::new(),
            }],
            ..Default::default()
        };

        assert_eq!(outcome.passed_count(), 2);
        assert_eq!(outcome.failed_count(), 1);
        assert!(!outcome.passed());
    }

    #[test]
    fn an_empty_run_is_empty_and_passing() {
        let outcome = ScriptOutcome::default();
        assert!(outcome.is_empty());
        assert!(outcome.passed());
    }
}
