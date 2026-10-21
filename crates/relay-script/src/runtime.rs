//! The sandbox itself.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rquickjs::{CatchResultExt, Context, Function, Runtime};

use crate::value::{
    Assertion, ConsoleLevel, ConsoleLine, ScriptOutcome, ScriptRequest, ScriptResponse, TestCase,
};
use relay_models::RequestPatch;

/// The test framework, injected ahead of every script.
const PRELUDE: &str = include_str!("prelude.js");

/// What a script is allowed to consume.
#[derive(Debug, Clone, Copy)]
pub struct ScriptLimits {
    /// Wall-clock deadline. A script that exceeds it is interrupted.
    pub timeout: Duration,
    /// Heap ceiling in bytes.
    pub memory_bytes: usize,
    /// Stack ceiling in bytes, which bounds recursion.
    pub stack_bytes: usize,
    /// Longest console line kept, in characters.
    pub max_console_line: usize,
    /// Most console lines kept.
    pub max_console_lines: usize,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            // Long enough for any reasonable assertion, short enough that
            // an accidental infinite loop is a blip rather than a hang.
            timeout: Duration::from_secs(5),
            memory_bytes: 64 * 1024 * 1024,
            stack_bytes: 1024 * 1024,
            max_console_line: 4 * 1024,
            max_console_lines: 200,
        }
    }
}

/// When a script runs, which decides what it is allowed to touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptPhase {
    /// Before the request is sent: may change it, cannot see a response.
    PreRequest,
    /// After the response arrives: may assert, cannot change the request.
    PostResponse,
}

/// Runs scripts in a fresh sandbox each time.
#[derive(Debug, Clone, Default)]
pub struct ScriptEngine {
    limits: ScriptLimits,
}

impl ScriptEngine {
    /// An engine with the default limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// An engine with custom limits.
    pub fn with_limits(limits: ScriptLimits) -> Self {
        Self { limits }
    }

    /// The limits in force.
    pub fn limits(&self) -> ScriptLimits {
        self.limits
    }

    /// Runs a script against a response.
    ///
    /// Never returns `Err`: a broken script is a result to display, not a
    /// failure of the request that already succeeded. Syntax errors,
    /// exceptions and timeouts all come back inside the outcome.
    pub fn run(
        &self,
        source: &str,
        response: Option<&ScriptResponse>,
        request: Option<&ScriptRequest>,
        variables: &std::collections::BTreeMap<String, String>,
    ) -> ScriptOutcome {
        self.run_phase(
            ScriptPhase::PostResponse,
            source,
            response,
            request,
            variables,
        )
    }

    /// Runs a script before the request goes out.
    ///
    /// The script sees the request as it currently stands and may ask for
    /// changes, which come back in [`ScriptOutcome::request_patch`].
    pub fn run_pre_request(
        &self,
        source: &str,
        request: Option<&ScriptRequest>,
        variables: &std::collections::BTreeMap<String, String>,
    ) -> ScriptOutcome {
        self.run_phase(ScriptPhase::PreRequest, source, None, request, variables)
    }

    fn run_phase(
        &self,
        phase: ScriptPhase,
        source: &str,
        response: Option<&ScriptResponse>,
        request: Option<&ScriptRequest>,
        variables: &std::collections::BTreeMap<String, String>,
    ) -> ScriptOutcome {
        let started = Instant::now();

        let mut outcome = match self.execute(phase, source, response, request, variables, started) {
            Ok(outcome) => outcome,
            Err(message) => ScriptOutcome {
                error: Some(message),
                ..Default::default()
            },
        };

        outcome.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        outcome
    }

    fn execute(
        &self,
        phase: ScriptPhase,
        source: &str,
        response: Option<&ScriptResponse>,
        request: Option<&ScriptRequest>,
        variables: &std::collections::BTreeMap<String, String>,
        started: Instant,
    ) -> Result<ScriptOutcome, String> {
        let runtime = Runtime::new().map_err(|e| format!("could not start the sandbox: {e}"))?;

        runtime.set_memory_limit(self.limits.memory_bytes);
        runtime.set_max_stack_size(self.limits.stack_bytes);

        // QuickJS calls this periodically; returning true aborts the
        // script. It is the only way to stop `while (true) {}`.
        let deadline = self.limits.timeout;
        let timed_out = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&timed_out);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            if started.elapsed() > deadline {
                flag.store(true, Ordering::Relaxed);
                true
            } else {
                false
            }
        })));

        // An unhandled promise rejection would otherwise vanish: nothing
        // observes it, and the script appears to have succeeded. That
        // matters because `import('fs')` rejects rather than throwing,
        // and an author writing an async test deserves to be told their
        // failure was swallowed.
        let rejections: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let rejection_sink = Arc::clone(&rejections);
        runtime.set_host_promise_rejection_tracker(Some(Box::new(
            move |_ctx, _promise, reason, is_handled| {
                let text = reason
                    .as_string()
                    .and_then(|s| s.to_string().ok())
                    .or_else(|| {
                        reason
                            .as_object()
                            .and_then(|o| o.get::<_, String>("message").ok())
                    })
                    .unwrap_or_else(|| "a promise rejected".to_string());

                let Ok(mut sink) = rejection_sink.lock() else {
                    return;
                };

                // QuickJS reports a rejection twice when a handler is
                // attached after the fact: once as unhandled, then again
                // as handled. Only the ones nothing ever handled are a
                // problem, so a later handling retracts the earlier
                // report.
                if is_handled {
                    if let Some(index) = sink.iter().position(|t| *t == text) {
                        sink.remove(index);
                    }
                } else if sink.len() < 20 {
                    sink.push(text);
                }
            },
        )));

        let context =
            Context::full(&runtime).map_err(|e| format!("could not start the sandbox: {e}"))?;

        let console: Arc<std::sync::Mutex<Vec<ConsoleLine>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let written: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>> =
            Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let patch: Arc<std::sync::Mutex<RequestPatch>> =
            Arc::new(std::sync::Mutex::new(RequestPatch::default()));

        let limits = self.limits;
        let result = context.with(|ctx| -> Result<ScriptOutcome, String> {
            let globals = ctx.globals();

            install_console(&ctx, &globals, Arc::clone(&console), limits)?;
            install_variables(&ctx, &globals, variables.clone(), Arc::clone(&written))?;
            install_request_api(&ctx, &globals, phase, Arc::clone(&patch))?;
            install_data(&ctx, &globals, response, request)?;

            ctx.eval::<(), _>(PRELUDE)
                .catch(&ctx)
                .map_err(|e| format!("the test framework failed to load: {e}"))?;

            // The script is evaluated separately from the prelude so a
            // syntax error in it is reported against the user's code.
            if let Err(error) = ctx.eval::<(), _>(source).catch(&ctx) {
                return Ok(ScriptOutcome {
                    error: Some(describe_error(&error.to_string())),
                    ..collect(&ctx).unwrap_or_default()
                });
            }

            // Queued test bodies run here, awaited one at a time.
            if let Err(error) = ctx.eval::<(), _>("globalThis.__relay.run();").catch(&ctx) {
                return Ok(ScriptOutcome {
                    error: Some(describe_error(&error.to_string())),
                    ..collect(&ctx).unwrap_or_default()
                });
            }

            collect(&ctx)
        });

        // Drive any promises the script created. Without this an async
        // test body never runs and silently reports nothing. The loop is
        // bounded, and the interrupt handler still applies, so a promise
        // that reschedules itself forever cannot spin here.
        for _ in 0..10_000 {
            match runtime.execute_pending_job() {
                Ok(true) => continue,
                _ => break,
            }
        }

        // Re-read the results: a job may have completed a test.
        let after_jobs = context.with(|ctx| collect(&ctx)).ok();

        let mut outcome = match result {
            Ok(outcome) => outcome,
            Err(message) => ScriptOutcome {
                error: Some(message),
                ..Default::default()
            },
        };

        if let Some(later) = after_jobs
            && later.tests.len() >= outcome.tests.len()
        {
            outcome.tests = later.tests;
            outcome.loose_assertions = later.loose_assertions;
        }

        if let Ok(unhandled) = rejections.lock()
            && !unhandled.is_empty()
            && outcome.error.is_none()
        {
            outcome.error = Some(format!(
                "a promise rejected and nothing handled it: {}",
                unhandled.join("; ")
            ));
        }

        if timed_out.load(Ordering::Relaxed) {
            outcome.error = Some(format!(
                "the script ran longer than {} seconds and was stopped",
                deadline.as_secs()
            ));
        }

        outcome.console = console.lock().map(|c| c.clone()).unwrap_or_default();
        outcome.variables = written.lock().map(|v| v.clone()).unwrap_or_default();
        outcome.request_patch = patch.lock().map(|p| p.clone()).unwrap_or_default();

        Ok(outcome)
    }
}

/// Turns QuickJS's error text into something worth showing a user.
fn describe_error(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains("out of memory") {
        return "the script ran out of memory and was stopped".to_string();
    }
    if trimmed.contains("interrupted") {
        return "the script was stopped before it finished".to_string();
    }
    if trimmed.contains("stack overflow") {
        return "the script recursed too deeply and was stopped".to_string();
    }
    trimmed.to_string()
}

fn install_console<'js>(
    ctx: &rquickjs::Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    sink: Arc<std::sync::Mutex<Vec<ConsoleLine>>>,
    limits: ScriptLimits,
) -> Result<(), String> {
    let console =
        rquickjs::Object::new(ctx.clone()).map_err(|e| format!("could not create console: {e}"))?;

    for (name, level) in [
        ("log", ConsoleLevel::Log),
        ("info", ConsoleLevel::Log),
        ("debug", ConsoleLevel::Log),
        ("warn", ConsoleLevel::Warn),
        ("error", ConsoleLevel::Error),
    ] {
        let sink = Arc::clone(&sink);
        let function = Function::new(
            ctx.clone(),
            move |args: rquickjs::function::Rest<rquickjs::Value<'_>>| {
                let text = args.0.iter().map(stringify).collect::<Vec<_>>().join(" ");

                let Ok(mut lines) = sink.lock() else { return };

                // Bounded in both directions: a script logging in a loop
                // must not be able to exhaust memory through the sink
                // that was meant to help debug it.
                if lines.len() >= limits.max_console_lines {
                    return;
                }
                let text = if text.chars().count() > limits.max_console_line {
                    let truncated: String = text.chars().take(limits.max_console_line).collect();
                    format!("{truncated}… (truncated)")
                } else {
                    text
                };
                lines.push(ConsoleLine { level, text });
            },
        )
        .map_err(|e| format!("could not create console.{name}: {e}"))?;

        console
            .set(name, function)
            .map_err(|e| format!("could not install console.{name}: {e}"))?;
    }

    globals
        .set("console", console)
        .map_err(|e| format!("could not install console: {e}"))
}

fn install_variables<'js>(
    ctx: &rquickjs::Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    readable: std::collections::BTreeMap<String, String>,
    written: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
) -> Result<(), String> {
    let relay = rquickjs::Object::new(ctx.clone())
        .map_err(|e| format!("could not create the relay object: {e}"))?;

    // Reads see a copy taken before the script started. Secret values are
    // not in it: the caller passes only what a script may know.
    let snapshot = readable.clone();
    let get = Function::new(ctx.clone(), move |name: String| -> Option<String> {
        snapshot.get(&name).cloned()
    })
    .map_err(|e| format!("could not create getVariable: {e}"))?;

    let sink = Arc::clone(&written);
    let set = Function::new(ctx.clone(), move |name: String, value: String| {
        if let Ok(mut map) = sink.lock() {
            // Bounded, so a loop cannot fill memory through this door.
            if map.len() < 500 {
                map.insert(name, value);
            }
        }
    })
    .map_err(|e| format!("could not create setVariable: {e}"))?;

    relay
        .set("getVariable", get)
        .and_then(|()| relay.set("setVariable", set))
        .map_err(|e| format!("could not install the variable API: {e}"))?;

    globals
        .set("relay", relay)
        .map_err(|e| format!("could not install the relay object: {e}"))
}

/// Installs the pre-request mutation API onto the `relay` object.
///
/// In a post-response script the same names exist but throw, because
/// `relay.setHeader` doing nothing after the request has already gone
/// would be a confusing way to spend an afternoon.
fn install_request_api<'js>(
    ctx: &rquickjs::Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    phase: ScriptPhase,
    patch: Arc<std::sync::Mutex<RequestPatch>>,
) -> Result<(), String> {
    let relay: rquickjs::Object<'js> = globals
        .get("relay")
        .map_err(|e| format!("the relay object is missing: {e}"))?;

    if phase == ScriptPhase::PostResponse {
        for name in [
            "setHeader",
            "removeHeader",
            "setQueryParam",
            "removeQueryParam",
            "setUrl",
            "setMethod",
            "setBody",
        ] {
            let message = format!(
                "relay.{name} can only be used in a pre-request script — \
                 the request has already been sent"
            );
            let throwing = Function::new(ctx.clone(), move |ctx: rquickjs::Ctx<'js>| {
                Err::<(), _>(ctx.throw(rquickjs::String::from_str(ctx.clone(), &message)?.into()))
            })
            .map_err(|e| format!("could not create relay.{name}: {e}"))?;

            relay
                .set(name, throwing)
                .map_err(|e| format!("could not install relay.{name}: {e}"))?;
        }
        return Ok(());
    }

    /// A name a script passed in, trimmed and bounded.
    fn clean(value: String) -> String {
        value.trim().chars().take(4096).collect()
    }

    // Bounded, so a loop cannot fill memory through any of these doors.
    const MAX_ENTRIES: usize = 200;

    let sink = Arc::clone(&patch);
    let set_header = Function::new(ctx.clone(), move |name: String, value: String| {
        if let Ok(mut p) = sink.lock() {
            let name = clean(name);
            if !name.is_empty() && p.headers.len() < MAX_ENTRIES {
                p.headers.insert(name, value.chars().take(8192).collect());
            }
        }
    })
    .map_err(|e| format!("could not create relay.setHeader: {e}"))?;

    let sink = Arc::clone(&patch);
    let remove_header = Function::new(ctx.clone(), move |name: String| {
        if let Ok(mut p) = sink.lock() {
            let name = clean(name);
            if !name.is_empty() && p.removed_headers.len() < MAX_ENTRIES {
                p.headers.remove(&name);
                p.removed_headers.push(name);
            }
        }
    })
    .map_err(|e| format!("could not create relay.removeHeader: {e}"))?;

    let sink = Arc::clone(&patch);
    let set_query = Function::new(ctx.clone(), move |name: String, value: String| {
        if let Ok(mut p) = sink.lock() {
            let name = clean(name);
            if !name.is_empty() && p.query.len() < MAX_ENTRIES {
                p.query.insert(name, value.chars().take(8192).collect());
            }
        }
    })
    .map_err(|e| format!("could not create relay.setQueryParam: {e}"))?;

    let sink = Arc::clone(&patch);
    let remove_query = Function::new(ctx.clone(), move |name: String| {
        if let Ok(mut p) = sink.lock() {
            let name = clean(name);
            if !name.is_empty() && p.removed_query.len() < MAX_ENTRIES {
                p.query.remove(&name);
                p.removed_query.push(name);
            }
        }
    })
    .map_err(|e| format!("could not create relay.removeQueryParam: {e}"))?;

    let sink = Arc::clone(&patch);
    let set_url = Function::new(ctx.clone(), move |url: String| {
        if let Ok(mut p) = sink.lock() {
            p.url = Some(clean(url));
        }
    })
    .map_err(|e| format!("could not create relay.setUrl: {e}"))?;

    let sink = Arc::clone(&patch);
    let set_method = Function::new(ctx.clone(), move |method: String| {
        if let Ok(mut p) = sink.lock() {
            p.method = Some(clean(method).to_ascii_uppercase());
        }
    })
    .map_err(|e| format!("could not create relay.setMethod: {e}"))?;

    let sink = Arc::clone(&patch);
    let set_body = Function::new(ctx.clone(), move |body: String| {
        if let Ok(mut p) = sink.lock() {
            p.body = Some(body);
        }
    })
    .map_err(|e| format!("could not create relay.setBody: {e}"))?;

    relay
        .set("setHeader", set_header)
        .and_then(|()| relay.set("removeHeader", remove_header))
        .and_then(|()| relay.set("setQueryParam", set_query))
        .and_then(|()| relay.set("removeQueryParam", remove_query))
        .and_then(|()| relay.set("setUrl", set_url))
        .and_then(|()| relay.set("setMethod", set_method))
        .and_then(|()| relay.set("setBody", set_body))
        .map_err(|e| format!("could not install the request API: {e}"))
}

fn install_data<'js>(
    ctx: &rquickjs::Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    response: Option<&ScriptResponse>,
    request: Option<&ScriptRequest>,
) -> Result<(), String> {
    // Serialised through JSON so the script receives plain objects with
    // no handle back into Rust.
    if let Some(response) = response {
        let json = serde_json::to_string(response)
            .map_err(|e| format!("could not pass the response to the script: {e}"))?;

        let script = format!(
            "globalThis.response = {json};\n\
             globalThis.response.json = function () {{ return JSON.parse(this.body); }};\n\
             globalThis.response.header = function (n) {{ \
                return this.headers[String(n).toLowerCase()]; }};"
        );
        ctx.eval::<(), _>(script)
            .map_err(|e| format!("could not pass the response to the script: {e}"))?;
    } else {
        globals
            .set("response", rquickjs::Undefined)
            .map_err(|e| format!("{e}"))?;
    }

    if let Some(request) = request {
        let json = serde_json::to_string(request)
            .map_err(|e| format!("could not pass the request to the script: {e}"))?;
        ctx.eval::<(), _>(format!("globalThis.request = {json};"))
            .map_err(|e| format!("could not pass the request to the script: {e}"))?;
    }

    Ok(())
}

/// Reads the results the prelude accumulated.
fn collect(ctx: &rquickjs::Ctx<'_>) -> Result<ScriptOutcome, String> {
    let json: String = ctx
        .eval(
            "JSON.stringify({ tests: globalThis.__relay.tests, \
                              loose: globalThis.__relay.loose })",
        )
        .map_err(|e| format!("could not read the test results: {e}"))?;

    #[derive(serde::Deserialize)]
    struct Raw {
        tests: Vec<TestCase>,
        loose: Vec<Assertion>,
    }

    let raw: Raw =
        serde_json::from_str(&json).map_err(|e| format!("could not read the test results: {e}"))?;

    Ok(ScriptOutcome {
        tests: raw.tests,
        loose_assertions: raw.loose,
        ..Default::default()
    })
}

/// Renders a value the way `console.log` would.
fn stringify(value: &rquickjs::Value<'_>) -> String {
    if let Some(text) = value.as_string() {
        return text.to_string().unwrap_or_default();
    }
    if value.is_undefined() {
        return "undefined".into();
    }
    if value.is_null() {
        return "null".into();
    }
    if let Some(flag) = value.as_bool() {
        return flag.to_string();
    }
    if let Some(number) = value.as_number() {
        // Render whole numbers without a trailing .0, as JavaScript does.
        return if number.fract() == 0.0 && number.abs() < 1e15 {
            format!("{}", number as i64)
        } else {
            number.to_string()
        };
    }

    // Objects and arrays go through JSON, which is what a developer
    // expects to see in a log line.
    value
        .ctx()
        .json_stringify(value.clone())
        .ok()
        .flatten()
        .and_then(|s| s.to_string().ok())
        .unwrap_or_else(|| "[object]".into())
}
