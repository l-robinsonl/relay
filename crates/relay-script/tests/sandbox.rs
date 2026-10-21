//! What a script can and cannot do.
//!
//! The escape tests matter more than the feature tests: a sandbox that
//! runs assertions correctly but lets a script read a file is worse than
//! no sandbox at all, because it looks safe.

use relay_script::{ScriptEngine, ScriptLimits, ScriptRequest, ScriptResponse};
use std::collections::BTreeMap;
use std::time::Duration;

fn response() -> ScriptResponse {
    let mut headers = serde_json::Map::new();
    headers.insert(
        "content-type".into(),
        serde_json::Value::String("application/json".into()),
    );
    headers.insert(
        "x-request-id".into(),
        serde_json::Value::String("abc-123".into()),
    );

    ScriptResponse {
        status: 200,
        status_text: "OK".into(),
        ok: true,
        headers,
        body: r#"{"id":42,"name":"Ada","tags":["a","b"],"nested":{"ok":true}}"#.into(),
        time: 123.4,
        size: 61,
        url: "https://api.example.com/users/42".into(),
    }
}

fn run(source: &str) -> relay_script::ScriptOutcome {
    ScriptEngine::new().run(source, Some(&response()), None, &BTreeMap::new())
}

// --- the sandbox ----------------------------------------------------------

#[test]
fn a_script_cannot_reach_the_filesystem() {
    for source in [
        "require('fs')",
        "import('fs')",
        "new (require('module'))()",
        "std.open('/etc/passwd', 'r')",
        "os.open('/etc/passwd')",
        "Deno.readTextFile('/etc/passwd')",
        "process.binding('fs')",
    ] {
        let outcome = run(source);
        assert!(
            outcome.error.is_some() || !outcome.passed(),
            "'{source}' did not fail — the sandbox may expose it"
        );
    }
}

#[test]
fn a_script_cannot_reach_the_network() {
    for source in [
        "fetch('https://example.com')",
        "new XMLHttpRequest()",
        "new WebSocket('wss://example.com')",
        "navigator.sendBeacon('https://example.com')",
    ] {
        let outcome = run(source);
        assert!(outcome.error.is_some(), "'{source}' did not fail");
    }
}

#[test]
fn a_script_cannot_reach_the_process_or_environment() {
    for source in [
        "process.env.HOME",
        "process.exit(1)",
        "globalThis.process.argv",
        "scriptArgs",
    ] {
        let outcome = run(source);
        assert!(outcome.error.is_some(), "'{source}' did not fail");
    }
}

#[test]
fn an_endless_loop_is_stopped_rather_than_hanging_the_app() {
    let engine = ScriptEngine::with_limits(ScriptLimits {
        timeout: Duration::from_millis(300),
        ..Default::default()
    });

    let started = std::time::Instant::now();
    let outcome = engine.run("while (true) {}", Some(&response()), None, &BTreeMap::new());
    let elapsed = started.elapsed();

    assert!(outcome.error.is_some(), "an infinite loop was not stopped");
    assert!(
        elapsed < Duration::from_secs(5),
        "it took {elapsed:?} to stop"
    );
    assert!(
        outcome.error.as_ref().unwrap().contains("longer than"),
        "got: {:?}",
        outcome.error
    );
}

#[test]
fn unbounded_allocation_is_stopped() {
    let engine = ScriptEngine::with_limits(ScriptLimits {
        memory_bytes: 2 * 1024 * 1024,
        timeout: Duration::from_secs(3),
        ..Default::default()
    });

    let outcome = engine.run(
        "const a = []; while (true) { a.push(new Array(10000).fill('x')); }",
        Some(&response()),
        None,
        &BTreeMap::new(),
    );

    assert!(
        outcome.error.is_some(),
        "runaway allocation was not stopped"
    );
}

#[test]
fn runaway_recursion_is_stopped() {
    let outcome = run("function f() { return f(); } f();");
    assert!(outcome.error.is_some());
}

#[test]
fn console_output_is_bounded() {
    let engine = ScriptEngine::with_limits(ScriptLimits {
        max_console_lines: 10,
        timeout: Duration::from_secs(3),
        ..Default::default()
    });

    let outcome = engine.run(
        "for (let i = 0; i < 1000; i++) console.log('line ' + i);",
        Some(&response()),
        None,
        &BTreeMap::new(),
    );

    // The debugging aid must not become the memory exhaustion route.
    assert_eq!(outcome.console.len(), 10);
}

#[test]
fn a_very_long_console_line_is_truncated() {
    let engine = ScriptEngine::with_limits(ScriptLimits {
        max_console_line: 50,
        ..Default::default()
    });
    let outcome = engine.run(
        "console.log('x'.repeat(10000))",
        Some(&response()),
        None,
        &BTreeMap::new(),
    );

    assert_eq!(outcome.console.len(), 1);
    assert!(outcome.console[0].text.contains("truncated"));
    assert!(outcome.console[0].text.chars().count() < 100);
}

#[test]
fn a_script_sees_only_the_redacted_request() {
    let mut spec = relay_models::HttpRequestSpec::new(
        relay_models::HttpMethod::Get,
        "https://api.example.com",
    );
    spec.headers = vec![relay_models::KeyValue::new(
        "Authorization",
        "Bearer sk-live-MUST-NOT-REACH-SCRIPTS",
    )];
    spec.auth = relay_models::AuthConfig::Bearer {
        token: "sk-live-MUST-NOT-REACH-SCRIPTS".into(),
    };

    let outcome = ScriptEngine::new().run(
        "console.log(JSON.stringify(request))",
        Some(&response()),
        Some(&ScriptRequest::from_spec(&spec)),
        &BTreeMap::new(),
    );

    let printed = outcome
        .console
        .iter()
        .map(|l| l.text.clone())
        .collect::<String>();

    assert!(
        !printed.contains("sk-live-MUST-NOT-REACH-SCRIPTS"),
        "a script could read the bearer token: {printed}"
    );
    assert!(printed.contains("[redacted]"));
}

#[test]
fn a_script_sees_only_the_variables_it_is_given() {
    let mut variables = BTreeMap::new();
    variables.insert("baseUrl".to_string(), "https://api.example.com".to_string());

    let outcome = ScriptEngine::new().run(
        "console.log(relay.getVariable('baseUrl')); \
         console.log(String(relay.getVariable('apiToken')));",
        Some(&response()),
        None,
        &variables,
    );

    let printed: Vec<&str> = outcome.console.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(printed[0], "https://api.example.com");
    // A secret the caller withheld is simply absent.
    assert_eq!(printed[1], "undefined");
}

#[test]
fn variables_a_script_sets_are_reported_not_applied_directly() {
    let outcome = run("relay.setVariable('token', 'abc123')");

    // The engine hands them back; writing them is the caller's decision.
    assert_eq!(
        outcome.variables.get("token").map(String::as_str),
        Some("abc123")
    );
}

#[test]
fn the_number_of_variables_a_script_can_set_is_bounded() {
    let outcome = run("for (let i = 0; i < 5000; i++) relay.setVariable('k' + i, 'v');");
    assert!(
        outcome.variables.len() <= 500,
        "{}",
        outcome.variables.len()
    );
}

#[test]
fn one_script_cannot_see_what_a_previous_one_left_behind() {
    let engine = ScriptEngine::new();
    engine.run(
        "globalThis.leaked = 'from the first script'",
        Some(&response()),
        None,
        &BTreeMap::new(),
    );

    let outcome = engine.run(
        "console.log(String(globalThis.leaked))",
        Some(&response()),
        None,
        &BTreeMap::new(),
    );

    assert_eq!(
        outcome.console[0].text, "undefined",
        "state leaked between runs"
    );
}

// --- assertions -----------------------------------------------------------

#[test]
fn the_example_from_the_documentation_works() {
    let outcome = run("expect(response.status).toBe(200)\n\
         expect(response.json().id).toBeDefined()");

    assert!(outcome.passed(), "{outcome:?}");
    assert_eq!(outcome.passed_count(), 2);
    assert_eq!(outcome.failed_count(), 0);
}

#[test]
fn a_failed_assertion_says_what_it_expected_and_what_it_got() {
    let outcome = run("expect(response.status).toBe(404)");

    assert!(!outcome.passed());
    let message = &outcome.loose_assertions[0].message;
    assert!(message.contains("404"), "{message}");
    assert!(message.contains("200"), "{message}");
}

#[test]
fn tests_group_their_assertions_and_report_individually() {
    let outcome = run(
        "test('it succeeds', () => { expect(response.status).toBe(200) });\n\
         test('it has a name', () => { expect(response.json().name).toBe('Ada') });\n\
         test('this one fails', () => { expect(response.status).toBe(500) });",
    );

    assert_eq!(outcome.tests.len(), 3);
    assert!(outcome.tests[0].passed());
    assert!(outcome.tests[1].passed());
    assert!(!outcome.tests[2].passed());
    assert!(!outcome.passed());
}

#[test]
fn a_failing_test_does_not_stop_the_ones_after_it() {
    let outcome = run("test('first', () => { expect(1).toBe(2) });\n\
         test('second', () => { expect(1).toBe(1) });");

    assert_eq!(outcome.tests.len(), 2);
    assert!(!outcome.tests[0].passed());
    assert!(outcome.tests[1].passed(), "a later test was skipped");
}

#[test]
fn a_test_that_throws_reports_the_error_rather_than_vanishing() {
    let outcome = run("test('broken', () => { response.nope.deeper });");

    assert_eq!(outcome.tests.len(), 1);
    assert!(!outcome.tests[0].passed());
    assert!(outcome.tests[0].error.is_some());
    // And the run as a whole is still readable.
    assert!(outcome.error.is_none());
}

#[test]
fn every_matcher_works() {
    let outcome = run(r#"
        test('matchers', () => {
          expect(response.status).toBe(200)
          expect(response.json()).toEqual({ id: 42, name: 'Ada', tags: ['a','b'], nested: { ok: true } })
          expect(response.json().id).toBeDefined()
          expect(response.json().missing).toBeUndefined()
          expect(response.ok).toBeTruthy()
          expect(false).toBeFalsy()
          expect(response.body).toContain('Ada')
          expect(response.json().tags).toContain('a')
          expect(response.json().tags).toHaveLength(2)
          expect(response.time).toBeGreaterThan(0)
          expect(response.status).toBeLessThan(300)
          expect(response.url).toMatch(/example\.com/)
          expect(response.json()).toHaveProperty('name')
          expect(response.status).not.toBe(404)
        })
        "#);

    assert!(outcome.passed(), "{:#?}", outcome.tests);
    assert_eq!(outcome.tests[0].assertions.len(), 14);
}

#[test]
fn not_inverts_without_recording_the_inner_result() {
    let passing = run("expect(1).not.toBe(2)");
    assert!(passing.passed());
    assert_eq!(
        passing.loose_assertions.len(),
        1,
        "the inner matcher leaked a result"
    );

    let failing = run("expect(1).not.toBe(1)");
    assert!(!failing.passed());
    assert_eq!(failing.loose_assertions.len(), 1);
}

#[test]
fn response_helpers_work() {
    let outcome = run("test('helpers', () => {\n\
           expect(response.header('Content-Type')).toBe('application/json')\n\
           expect(response.header('content-type')).toBe('application/json')\n\
           expect(response.json().nested.ok).toBe(true)\n\
         });");
    assert!(outcome.passed(), "{:#?}", outcome.tests);
}

#[test]
fn a_syntax_error_is_reported_against_the_users_script() {
    let outcome = run("this is not javascript {{{");

    assert!(outcome.error.is_some());
    let error = outcome.error.unwrap();
    assert!(
        !error.contains("test framework"),
        "the prelude was blamed for the user's syntax error: {error}"
    );
}

#[test]
fn an_empty_script_is_not_an_error() {
    let outcome = run("");
    assert!(outcome.passed());
    assert!(outcome.is_empty());
}

#[test]
fn console_output_is_captured_with_its_level() {
    let outcome =
        run("console.log('a log'); console.warn('a warning'); console.error('an error');");

    assert_eq!(outcome.console.len(), 3);
    assert_eq!(outcome.console[0].level, relay_script::ConsoleLevel::Log);
    assert_eq!(outcome.console[1].level, relay_script::ConsoleLevel::Warn);
    assert_eq!(outcome.console[2].level, relay_script::ConsoleLevel::Error);
    assert_eq!(outcome.console[2].text, "an error");
}

#[test]
fn console_renders_values_the_way_a_developer_expects() {
    let outcome = run("console.log(42); console.log({a:1}); console.log([1,2]); \
         console.log(null); console.log(undefined); console.log(true);");

    let lines: Vec<&str> = outcome.console.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        lines,
        ["42", r#"{"a":1}"#, "[1,2]", "null", "undefined", "true"]
    );
}

#[test]
fn a_run_reports_how_long_it_took() {
    let outcome = run("expect(1).toBe(1)");
    assert!(outcome.duration_ms >= 0.0);
    assert!(outcome.duration_ms < 5000.0);
}

#[test]
fn standard_javascript_is_available() {
    // The sandbox removes I/O, not the language.
    let outcome = run("test('language', () => {\n\
           expect(JSON.parse('{\"a\":1}').a).toBe(1)\n\
           expect(Math.max(1, 2)).toBe(2)\n\
           expect([3,1,2].sort().join('')).toBe('123')\n\
           expect('abc'.toUpperCase()).toBe('ABC')\n\
           expect(typeof Date.now()).toBe('number')\n\
           expect([1,2,3].map(n => n * 2)).toEqual([2,4,6])\n\
         });");
    assert!(outcome.passed(), "{:#?}", outcome.tests);
}

#[test]
fn a_dynamic_import_rejects_and_the_rejection_is_reported() {
    // import() does not throw — it returns a rejected promise. Without a
    // rejection tracker that failure is invisible and the script looks
    // like it succeeded, which is the worst possible outcome for a line
    // that was trying to reach the filesystem.
    let outcome = run("import('fs')");

    assert!(
        outcome.error.is_some(),
        "an unhandled rejection was swallowed"
    );
    assert!(
        outcome.error.as_ref().unwrap().contains("promise rejected"),
        "got: {:?}",
        outcome.error
    );
}

#[test]
fn an_async_test_body_actually_runs() {
    // Without pumping the job queue the body never executes and the test
    // silently reports no assertions — passing by doing nothing.
    let outcome = run("test('async work', async () => { \
           const value = await Promise.resolve(200); \
           expect(value).toBe(response.status); \
         });");

    assert_eq!(outcome.tests.len(), 1);
    assert_eq!(
        outcome.tests[0].assertions.len(),
        1,
        "the async body did not run: {:#?}",
        outcome.tests[0]
    );
    assert!(outcome.tests[0].passed());
}

#[test]
fn a_failing_async_assertion_is_reported_rather_than_passing_silently() {
    let outcome = run("test('async failure', async () => { \
           await Promise.resolve(); \
           expect(response.status).toBe(500); \
         });");

    assert_eq!(outcome.tests.len(), 1);
    assert!(!outcome.tests[0].passed(), "{:#?}", outcome.tests[0]);
}

#[test]
fn a_handled_rejection_is_not_reported_as_an_error() {
    let outcome = run("Promise.reject(new Error('caught')).catch(() => {})");
    assert!(outcome.error.is_none(), "got: {:?}", outcome.error);
}
