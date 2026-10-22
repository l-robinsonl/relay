//! Variable resolution.
//!
//! Templates use `{{name}}`. Resolution walks a scope chain, and the result
//! records which variables could not be resolved rather than quietly leaving
//! the braces in the URL — a request sent to `{{baseUrl}}/users` fails in a
//! confusing way, so the editor needs to say so before it is sent.

use relay_models::Variable;
use std::collections::{BTreeMap, BTreeSet};

/// The precedence order of a variable's source. Higher wins.
///
/// Documented precedence, lowest to highest: workspace, collection (outermost
/// to innermost folder), environment, then request-level overrides. The
/// environment beats collections so that switching from Staging to Production
/// actually changes the host even when a collection has defined one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Workspace-wide variables.
    Workspace,
    /// A collection or folder, with depth: deeper folders win.
    Collection(u8),
    /// The active environment.
    Environment,
    /// Set on the request itself, or by a pre-request script.
    Request,
}

/// A resolved variable and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The value to substitute.
    pub value: String,
    /// Which scope supplied it.
    pub scope: Scope,
    /// True when the value came from the OS credential store.
    pub secret: bool,
    /// True when the value is substituted as-is, without being expanded.
    ///
    /// Only a pre-request script's variables are literal. A script may not
    /// read a secret, so it must not be able to set a variable whose value
    /// is a reference to one and have resolution fetch it on the script's
    /// behalf — that would be a way round the rule rather than an
    /// exception to it.
    pub literal: bool,
}

/// A layered set of variable bindings.
#[derive(Debug, Clone, Default)]
pub struct VariableContext {
    bindings: BTreeMap<String, Binding>,
}

impl VariableContext {
    /// An empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds variables from a scope, overriding only lower-precedence ones.
    pub fn extend(&mut self, scope: Scope, variables: &[Variable]) {
        for variable in variables.iter().filter(|v| v.enabled) {
            let key = variable.key.trim();
            if key.is_empty() {
                continue;
            }
            let replace = match self.bindings.get(key) {
                Some(existing) => scope >= existing.scope,
                None => true,
            };
            if replace {
                self.bindings.insert(
                    key.to_string(),
                    Binding {
                        value: variable.value.clone(),
                        scope,
                        secret: variable.secret,
                        literal: false,
                    },
                );
            }
        }
    }

    /// Supplies a secret value fetched from the credential store.
    pub fn set_secret(&mut self, key: &str, value: impl Into<String>) {
        self.bindings.insert(
            key.trim().to_string(),
            Binding {
                value: value.into(),
                scope: Scope::Environment,
                secret: true,
                literal: false,
            },
        );
    }

    /// Sets a value at request scope.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.bindings.insert(
            key.trim().to_string(),
            Binding {
                value: value.into(),
                scope: Scope::Request,
                secret: false,
                literal: false,
            },
        );
    }

    /// Sets a value a pre-request script produced.
    ///
    /// Substituted verbatim: whatever the script wrote is what goes into
    /// the request, with no second pass that might turn `{{apiSecret}}`
    /// into the secret the script is not allowed to read.
    pub fn set_literal(&mut self, key: &str, value: impl Into<String>) {
        self.bindings.insert(
            key.trim().to_string(),
            Binding {
                value: value.into(),
                scope: Scope::Request,
                secret: false,
                literal: true,
            },
        );
    }

    /// Looks up a binding.
    pub fn get(&self, key: &str) -> Option<&Binding> {
        self.bindings.get(key.trim())
    }

    /// Every bound name, for autocomplete.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.bindings.keys().map(String::as_str)
    }

    /// Number of bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// True when nothing is bound.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// The outcome of interpolating a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interpolated {
    /// The substituted text. Unresolved references are left verbatim.
    pub text: String,
    /// Names that had no binding, in the order first encountered.
    pub missing: Vec<String>,
}

impl Interpolated {
    /// True when every reference resolved.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// How many times a value may itself expand before giving up.
const MAX_DEPTH: usize = 10;

/// Substitutes `{{name}}` references using `context`.
///
/// Values may themselves contain references — `{{baseUrl}}` resolving to
/// `{{scheme}}://{{host}}` is common — so expansion recurses, bounded by
/// [`MAX_DEPTH`] and a visited set so a cycle terminates instead of hanging
/// the UI thread.
pub fn interpolate(template: &str, context: &VariableContext) -> Interpolated {
    let mut missing = Vec::new();
    let mut visiting = BTreeSet::new();
    let text = expand(template, context, &mut missing, &mut visiting, 0);
    Interpolated { text, missing }
}

fn expand(
    template: &str,
    context: &VariableContext,
    missing: &mut Vec<String>,
    visiting: &mut BTreeSet<String>,
    depth: usize,
) -> String {
    if depth > MAX_DEPTH || !template.contains("{{") {
        return template.to_string();
    }

    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'{'
            && let Some(end) = template[i + 2..].find("}}")
        {
            let raw = &template[i + 2..i + 2 + end];
            let name = raw.trim();

            if name.is_empty() || name.contains('{') {
                // Not a reference: emit the braces verbatim.
                out.push_str(&template[i..i + 2]);
                i += 2;
                continue;
            }

            match context.get(name) {
                Some(binding) => {
                    if binding.literal {
                        // Substituted as written. See `set_literal`.
                        out.push_str(&binding.value);
                    } else if visiting.contains(name) {
                        // A cycle. Leave the reference in place so the
                        // user can see which variable is self-referential.
                        out.push_str(&template[i..i + 4 + end]);
                    } else {
                        visiting.insert(name.to_string());
                        out.push_str(&expand(
                            &binding.value,
                            context,
                            missing,
                            visiting,
                            depth + 1,
                        ));
                        visiting.remove(name);
                    }
                }
                None => {
                    if !missing.iter().any(|m| m == name) {
                        missing.push(name.to_string());
                    }
                    out.push_str(&template[i..i + 4 + end]);
                }
            }

            i += 4 + end;
            continue;
        }

        // Advance by one full character so multi-byte text is not split.
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&template[i..(i + ch_len).min(template.len())]);
        i += ch_len;
    }

    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// Every `{{name}}` referenced by a template, in order of first appearance.
pub fn referenced_names(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim();
        if !name.is_empty() && !name.contains('{') && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
        rest = &after[end + 2..];
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(pairs: &[(&str, &str)]) -> VariableContext {
        let mut c = VariableContext::new();
        let vars: Vec<Variable> = pairs.iter().map(|(k, v)| Variable::new(*k, *v)).collect();
        c.extend(Scope::Environment, &vars);
        c
    }

    #[test]
    fn substitutes_a_single_variable() {
        let result = interpolate("{{baseUrl}}/users", &ctx(&[("baseUrl", "https://api.x")]));
        assert_eq!(result.text, "https://api.x/users");
        assert!(result.is_complete());
    }

    #[test]
    fn substitutes_several_variables() {
        let c = ctx(&[("base", "https://api.x"), ("id", "42")]);
        let result = interpolate("{{base}}/users/{{id}}/posts", &c);
        assert_eq!(result.text, "https://api.x/users/42/posts");
    }

    #[test]
    fn tolerates_whitespace_inside_braces() {
        let result = interpolate("{{ baseUrl }}/x", &ctx(&[("baseUrl", "https://api.x")]));
        assert_eq!(result.text, "https://api.x/x");
    }

    #[test]
    fn reports_missing_variables_instead_of_silently_emptying_them() {
        let result = interpolate(
            "{{baseUrl}}/users/{{userId}}",
            &ctx(&[("baseUrl", "https://x")]),
        );
        assert_eq!(result.missing, vec!["userId"]);
        // The reference is left in place so the user sees what went wrong.
        assert_eq!(result.text, "https://x/users/{{userId}}");
        assert!(!result.is_complete());
    }

    #[test]
    fn a_missing_variable_is_reported_once() {
        let result = interpolate("{{a}}/{{a}}/{{a}}", &ctx(&[]));
        assert_eq!(result.missing, vec!["a"]);
    }

    #[test]
    fn nested_variables_expand() {
        let c = ctx(&[
            ("baseUrl", "{{scheme}}://{{host}}"),
            ("scheme", "https"),
            ("host", "api.example.com"),
        ]);
        assert_eq!(
            interpolate("{{baseUrl}}/v1", &c).text,
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn a_self_referential_variable_terminates() {
        let c = ctx(&[("loop", "{{loop}}")]);
        let result = interpolate("{{loop}}", &c);
        // Must not hang or stack-overflow; the reference is left visible.
        assert!(result.text.contains("loop"));
    }

    #[test]
    fn a_mutual_cycle_terminates() {
        let c = ctx(&[("a", "{{b}}"), ("b", "{{a}}")]);
        let result = interpolate("{{a}}", &c);
        assert!(result.text.contains("{{"));
    }

    #[test]
    fn text_without_variables_is_returned_unchanged() {
        let result = interpolate("https://api.example.com/users", &ctx(&[]));
        assert_eq!(result.text, "https://api.example.com/users");
        assert!(result.is_complete());
    }

    #[test]
    fn unmatched_braces_are_left_alone() {
        for input in ["{{unclosed", "}}stray", "{single}", "{{}}"] {
            let result = interpolate(input, &ctx(&[]));
            assert_eq!(result.text, input, "mangled {input}");
        }
    }

    #[test]
    fn multibyte_text_is_not_corrupted() {
        let c = ctx(&[("name", "Ada")]);
        let result = interpolate("héllo {{name}} — ✓ 日本語", &c);
        assert_eq!(result.text, "héllo Ada — ✓ 日本語");
    }

    #[test]
    fn a_variable_resolving_to_empty_is_not_missing() {
        // An explicitly empty value is a choice, not an error.
        let result = interpolate("{{prefix}}/users", &ctx(&[("prefix", "")]));
        assert_eq!(result.text, "/users");
        assert!(result.is_complete());
    }

    #[test]
    fn environment_overrides_collection_which_overrides_workspace() {
        let mut c = VariableContext::new();
        c.extend(Scope::Workspace, &[Variable::new("host", "workspace")]);
        c.extend(Scope::Collection(0), &[Variable::new("host", "collection")]);
        c.extend(Scope::Environment, &[Variable::new("host", "environment")]);
        assert_eq!(interpolate("{{host}}", &c).text, "environment");
    }

    #[test]
    fn deeper_folders_win_over_shallower_ones() {
        let mut c = VariableContext::new();
        c.extend(Scope::Collection(0), &[Variable::new("v", "outer")]);
        c.extend(Scope::Collection(1), &[Variable::new("v", "inner")]);
        assert_eq!(interpolate("{{v}}", &c).text, "inner");
    }

    #[test]
    fn a_lower_scope_cannot_override_a_higher_one() {
        let mut c = VariableContext::new();
        c.extend(Scope::Environment, &[Variable::new("v", "environment")]);
        c.extend(Scope::Workspace, &[Variable::new("v", "workspace")]);
        assert_eq!(interpolate("{{v}}", &c).text, "environment");
    }

    #[test]
    fn request_scope_beats_everything() {
        let mut c = VariableContext::new();
        c.extend(Scope::Environment, &[Variable::new("v", "environment")]);
        c.set("v", "request");
        assert_eq!(interpolate("{{v}}", &c).text, "request");
    }

    #[test]
    fn disabled_variables_are_ignored() {
        let mut c = VariableContext::new();
        let mut disabled = Variable::new("v", "ignored");
        disabled.enabled = false;
        c.extend(Scope::Environment, &[disabled]);
        assert_eq!(interpolate("{{v}}", &c).missing, vec!["v"]);
    }

    #[test]
    fn secret_values_come_from_the_credential_store_not_the_variable_row() {
        let mut c = VariableContext::new();
        // A secret variable is persisted with an empty value...
        c.extend(
            Scope::Environment,
            &[Variable::new("token", "").into_secret()],
        );
        // ...and the real value is injected at send time.
        c.set_secret("token", "sk-live-real");
        assert_eq!(
            interpolate("Bearer {{token}}", &c).text,
            "Bearer sk-live-real"
        );
        assert!(c.get("token").unwrap().secret);
    }

    #[test]
    fn referenced_names_lists_every_distinct_reference() {
        let names = referenced_names("{{a}}/{{ b }}/{{a}}/plain/{{c}}");
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn referenced_names_handles_text_with_no_references() {
        assert!(referenced_names("https://example.com").is_empty());
    }
}
