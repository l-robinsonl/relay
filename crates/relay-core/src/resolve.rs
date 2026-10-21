//! Assembling a sendable request from a saved document.
//!
//! This is the step between "what the user typed" and "what goes on the
//! wire": variables are substituted, inherited auth and headers are merged
//! from the folder chain, and disabled rows are dropped.

use relay_models::{
    AuthConfig, Collection, Environment, HttpRequestSpec, KeyValue, RequestBody, RequestDoc,
    RequestSettings, Workspace,
};

use crate::interpolate::{Interpolated, Scope, VariableContext, interpolate};

/// Everything needed to resolve one request.
#[derive(Debug, Clone, Default)]
pub struct ResolutionInput<'a> {
    /// The workspace the request belongs to.
    pub workspace: Option<&'a Workspace>,
    /// The folder chain from the outermost collection inwards.
    pub folder_chain: Vec<&'a Collection>,
    /// The active environment, if one is selected.
    pub environment: Option<&'a Environment>,
}

/// A resolved request plus anything the user should be told about it.
#[derive(Debug, Clone)]
pub struct Resolution {
    /// The request, ready for the HTTP engine.
    pub spec: HttpRequestSpec,
    /// Variable names that could not be resolved anywhere.
    pub missing_variables: Vec<String>,
    /// Secret variable names the request needs, for the credential store.
    pub required_secrets: Vec<String>,
}

impl Resolution {
    /// True when every variable resolved.
    pub fn is_complete(&self) -> bool {
        self.missing_variables.is_empty()
    }
}

/// Builds the variable context for a request from its scope chain.
pub fn build_context(input: &ResolutionInput<'_>) -> VariableContext {
    let mut context = VariableContext::new();

    if let Some(workspace) = input.workspace {
        context.extend(Scope::Workspace, &workspace.variables);
    }

    for (depth, collection) in input.folder_chain.iter().enumerate() {
        context.extend(
            Scope::Collection(depth.min(u8::MAX as usize) as u8),
            &collection.variables,
        );
    }

    if let Some(environment) = input.environment {
        context.extend(Scope::Environment, &environment.variables);
    }

    context
}

/// Resolves a saved request into a sendable spec.
pub fn resolve(
    doc: &RequestDoc,
    input: &ResolutionInput<'_>,
    context: &VariableContext,
) -> Resolution {
    let mut missing: Vec<String> = Vec::new();

    let mut take = |text: &str| -> String {
        let Interpolated {
            text,
            missing: names,
        } = interpolate(text, context);
        for name in names {
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
        text
    };

    let url = take(&doc.url);

    // Headers: the folder chain contributes first, outermost to innermost, so
    // a header set on a specific folder overrides the collection default. The
    // request's own headers come last and win outright.
    let mut headers: Vec<KeyValue> = Vec::new();
    for collection in &input.folder_chain {
        for header in collection.headers.iter().filter(|h| h.is_active()) {
            upsert_header(
                &mut headers,
                KeyValue {
                    key: take(&header.key),
                    value: take(&header.value),
                    enabled: true,
                    description: header.description.clone(),
                },
            );
        }
    }
    for header in doc.headers.iter().filter(|h| h.is_active()) {
        upsert_header(
            &mut headers,
            KeyValue {
                key: take(&header.key),
                value: take(&header.value),
                enabled: true,
                description: header.description.clone(),
            },
        );
    }

    let query_params = doc
        .query_params
        .iter()
        .filter(|p| p.is_active())
        .map(|p| KeyValue {
            key: take(&p.key),
            value: take(&p.value),
            enabled: true,
            description: p.description.clone(),
        })
        .collect();

    let body = resolve_body(&doc.body, &mut take);
    let auth = resolve_auth(doc, input, &mut take);
    let settings = resolve_settings(doc, input);

    let required_secrets = collect_required_secrets(doc, context);

    Resolution {
        spec: HttpRequestSpec {
            method: doc.method.clone(),
            url,
            headers,
            query_params,
            body,
            auth,
            settings,
        },
        missing_variables: missing,
        required_secrets,
    }
}

/// Replaces a header with the same name, or appends it.
///
/// Header names are case-insensitive, so `content-type` from a collection and
/// `Content-Type` on the request are the same header and the request must win
/// rather than both being sent.
fn upsert_header(headers: &mut Vec<KeyValue>, header: KeyValue) {
    let lowered = header.key.to_ascii_lowercase();
    match headers
        .iter_mut()
        .find(|h| h.key.to_ascii_lowercase() == lowered)
    {
        Some(existing) => *existing = header,
        None => headers.push(header),
    }
}

fn resolve_body(body: &RequestBody, take: &mut impl FnMut(&str) -> String) -> RequestBody {
    match body {
        RequestBody::None => RequestBody::None,
        RequestBody::Json { text } => RequestBody::Json { text: take(text) },
        RequestBody::Text { text } => RequestBody::Text { text: take(text) },
        RequestBody::Xml { text } => RequestBody::Xml { text: take(text) },
        RequestBody::Raw { text, content_type } => RequestBody::Raw {
            text: take(text),
            content_type: take(content_type),
        },
        RequestBody::UrlEncoded { fields } => RequestBody::UrlEncoded {
            fields: fields
                .iter()
                .filter(|f| f.is_active())
                .map(|f| KeyValue {
                    key: take(&f.key),
                    value: take(&f.value),
                    enabled: true,
                    description: f.description.clone(),
                })
                .collect(),
        },
        RequestBody::Multipart { fields } => RequestBody::Multipart {
            fields: fields
                .iter()
                .filter(|f| f.enabled)
                .map(|f| relay_models::FormField {
                    key: take(&f.key),
                    enabled: true,
                    value: match &f.value {
                        relay_models::FormValue::Text { value } => {
                            relay_models::FormValue::Text { value: take(value) }
                        }
                        relay_models::FormValue::File {
                            path,
                            file_name,
                            content_type,
                        } => relay_models::FormValue::File {
                            path: take(path),
                            file_name: file_name.clone(),
                            content_type: content_type.clone(),
                        },
                    },
                    description: f.description.clone(),
                })
                .collect(),
        },
        RequestBody::Binary { path, content_type } => RequestBody::Binary {
            path: take(path),
            content_type: content_type.clone(),
        },
        RequestBody::GraphQl { query, variables } => RequestBody::GraphQl {
            query: take(query),
            variables: take(variables),
        },
    }
}

/// Resolves auth, walking up the folder chain when the request inherits.
fn resolve_auth(
    doc: &RequestDoc,
    input: &ResolutionInput<'_>,
    take: &mut impl FnMut(&str) -> String,
) -> AuthConfig {
    let effective = match &doc.auth {
        AuthConfig::Inherit => input
            .folder_chain
            .iter()
            .rev()
            .map(|c| &c.auth)
            .find(|auth| !matches!(auth, AuthConfig::Inherit | AuthConfig::None))
            .cloned()
            .unwrap_or(AuthConfig::None),
        other => other.clone(),
    };

    match effective {
        AuthConfig::Basic { username, password } => AuthConfig::Basic {
            username: take(&username),
            password: take(&password),
        },
        AuthConfig::Bearer { token } => AuthConfig::Bearer {
            token: take(&token),
        },
        AuthConfig::ApiKey {
            key,
            value,
            location,
        } => AuthConfig::ApiKey {
            key: take(&key),
            value: take(&value),
            location,
        },
        AuthConfig::OAuth2(config) => {
            let mut resolved = *config;
            resolved.client_id = take(&resolved.client_id);
            resolved.client_secret = take(&resolved.client_secret);
            resolved.token_url = take(&resolved.token_url);
            resolved.auth_url = take(&resolved.auth_url);
            resolved.scope = take(&resolved.scope);
            resolved.audience = take(&resolved.audience);
            resolved.username = take(&resolved.username);
            resolved.password = take(&resolved.password);
            AuthConfig::OAuth2(Box::new(resolved))
        }
        other => other,
    }
}

/// Settings are taken from the request; the workspace supplies no overrides
/// today, but the hook exists so adding them later does not change callers.
fn resolve_settings(doc: &RequestDoc, _input: &ResolutionInput<'_>) -> RequestSettings {
    doc.settings.clone()
}

/// Names of secret variables this request references.
///
/// The caller fetches these from the OS credential store and injects them
/// with [`VariableContext::set_secret`] immediately before sending, so a
/// secret exists in memory for the duration of one request and never touches
/// the database.
fn collect_required_secrets(doc: &RequestDoc, context: &VariableContext) -> Vec<String> {
    let mut sources = vec![doc.url.clone()];
    sources.extend(
        doc.headers
            .iter()
            .flat_map(|h| [h.key.clone(), h.value.clone()]),
    );
    sources.extend(
        doc.query_params
            .iter()
            .flat_map(|p| [p.key.clone(), p.value.clone()]),
    );

    match &doc.auth {
        AuthConfig::Basic { username, password } => {
            sources.push(username.clone());
            sources.push(password.clone());
        }
        AuthConfig::Bearer { token } => sources.push(token.clone()),
        AuthConfig::ApiKey { key, value, .. } => {
            sources.push(key.clone());
            sources.push(value.clone());
        }
        AuthConfig::OAuth2(config) => {
            sources.push(config.client_id.clone());
            sources.push(config.client_secret.clone());
            sources.push(config.username.clone());
            sources.push(config.password.clone());
        }
        _ => {}
    }

    match &doc.body {
        RequestBody::Json { text }
        | RequestBody::Text { text }
        | RequestBody::Xml { text }
        | RequestBody::Raw { text, .. } => sources.push(text.clone()),
        RequestBody::UrlEncoded { fields } => {
            sources.extend(fields.iter().map(|f| f.value.clone()));
        }
        RequestBody::GraphQl { query, variables } => {
            sources.push(query.clone());
            sources.push(variables.clone());
        }
        _ => {}
    }

    let mut secrets = Vec::new();
    for source in &sources {
        for name in crate::interpolate::referenced_names(source) {
            if context.get(&name).is_some_and(|b| b.secret) && !secrets.contains(&name) {
                secrets.push(name);
            }
        }
    }
    secrets
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_models::{HttpMethod, Variable, WorkspaceId};

    fn workspace_with(vars: &[(&str, &str)]) -> Workspace {
        let mut ws = Workspace::new("W");
        ws.variables = vars.iter().map(|(k, v)| Variable::new(*k, *v)).collect();
        ws
    }

    #[test]
    fn resolves_url_variables_from_the_environment() {
        let ws = workspace_with(&[]);
        let mut env = Environment::new(ws.id.clone(), "Dev");
        env.variables = vec![Variable::new("baseUrl", "http://localhost:3000")];

        let mut doc = RequestDoc::new(ws.id.clone(), "Get users");
        doc.url = "{{baseUrl}}/users".into();

        let input = ResolutionInput {
            workspace: Some(&ws),
            folder_chain: vec![],
            environment: Some(&env),
        };
        let context = build_context(&input);
        let resolved = resolve(&doc, &input, &context);

        assert_eq!(resolved.spec.url, "http://localhost:3000/users");
        assert!(resolved.is_complete());
    }

    #[test]
    fn missing_variables_are_reported_not_silently_dropped() {
        let ws = workspace_with(&[]);
        let mut doc = RequestDoc::new(ws.id.clone(), "Broken");
        doc.url = "{{baseUrl}}/users/{{id}}".into();

        let input = ResolutionInput {
            workspace: Some(&ws),
            ..Default::default()
        };
        let resolved = resolve(&doc, &input, &build_context(&input));

        assert_eq!(resolved.missing_variables, vec!["baseUrl", "id"]);
        assert!(!resolved.is_complete());
    }

    #[test]
    fn variables_resolve_in_headers_query_and_body() {
        let ws = workspace_with(&[("token", "abc"), ("v", "2"), ("name", "Ada")]);
        let mut doc = RequestDoc::new(ws.id.clone(), "R");
        doc.url = "https://x".into();
        doc.headers = vec![KeyValue::new("Authorization", "Bearer {{token}}")];
        doc.query_params = vec![KeyValue::new("version", "{{v}}")];
        doc.body = RequestBody::Json {
            text: r#"{"name":"{{name}}"}"#.into(),
        };

        let input = ResolutionInput {
            workspace: Some(&ws),
            ..Default::default()
        };
        let resolved = resolve(&doc, &input, &build_context(&input));

        assert_eq!(resolved.spec.headers[0].value, "Bearer abc");
        assert_eq!(resolved.spec.query_params[0].value, "2");
        match resolved.spec.body {
            RequestBody::Json { text } => assert_eq!(text, r#"{"name":"Ada"}"#),
            other => panic!("expected json body, got {other:?}"),
        }
    }

    #[test]
    fn disabled_rows_are_dropped_before_sending() {
        let ws = workspace_with(&[]);
        let mut doc = RequestDoc::new(ws.id.clone(), "R");
        doc.url = "https://x".into();
        doc.headers = vec![
            KeyValue::new("Keep", "1"),
            KeyValue {
                key: "Drop".into(),
                value: "1".into(),
                enabled: false,
                description: None,
            },
        ];

        let input = ResolutionInput {
            workspace: Some(&ws),
            ..Default::default()
        };
        let resolved = resolve(&doc, &input, &build_context(&input));

        assert_eq!(resolved.spec.headers.len(), 1);
        assert_eq!(resolved.spec.headers[0].key, "Keep");
    }

    #[test]
    fn collection_headers_are_merged_and_overridden_case_insensitively() {
        let ws = workspace_with(&[]);
        let mut collection = Collection::new(ws.id.clone(), "API");
        collection.headers = vec![
            KeyValue::new("Accept", "application/json"),
            KeyValue::new("content-type", "text/plain"),
        ];

        let mut doc = RequestDoc::new(ws.id.clone(), "R");
        doc.url = "https://x".into();
        doc.headers = vec![KeyValue::new("Content-Type", "application/json")];

        let input = ResolutionInput {
            workspace: Some(&ws),
            folder_chain: vec![&collection],
            environment: None,
        };
        let resolved = resolve(&doc, &input, &build_context(&input));

        assert_eq!(resolved.spec.headers.len(), 2, "the header was duplicated");
        let content_type = resolved
            .spec
            .headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("content-type"))
            .unwrap();
        assert_eq!(content_type.value, "application/json");
    }

    #[test]
    fn inherited_auth_comes_from_the_nearest_folder_that_defines_it() {
        let ws = workspace_with(&[]);
        let mut outer = Collection::new(ws.id.clone(), "Outer");
        outer.auth = AuthConfig::Bearer {
            token: "outer-token".into(),
        };
        let mut inner = Collection::new(ws.id.clone(), "Inner");
        inner.auth = AuthConfig::Bearer {
            token: "inner-token".into(),
        };

        let mut doc = RequestDoc::new(ws.id.clone(), "R");
        doc.url = "https://x".into();
        doc.auth = AuthConfig::Inherit;

        let input = ResolutionInput {
            workspace: Some(&ws),
            folder_chain: vec![&outer, &inner],
            environment: None,
        };
        let resolved = resolve(&doc, &input, &build_context(&input));

        assert_eq!(
            resolved.spec.auth,
            AuthConfig::Bearer {
                token: "inner-token".into()
            }
        );
    }

    #[test]
    fn inheritance_skips_folders_that_define_no_auth() {
        let ws = workspace_with(&[]);
        let mut outer = Collection::new(ws.id.clone(), "Outer");
        outer.auth = AuthConfig::Bearer {
            token: "outer-token".into(),
        };
        let inner = Collection::new(ws.id.clone(), "Inner"); // defaults to None

        let mut doc = RequestDoc::new(ws.id.clone(), "R");
        doc.auth = AuthConfig::Inherit;

        let input = ResolutionInput {
            workspace: Some(&ws),
            folder_chain: vec![&outer, &inner],
            environment: None,
        };
        let resolved = resolve(&doc, &input, &build_context(&input));

        assert_eq!(
            resolved.spec.auth,
            AuthConfig::Bearer {
                token: "outer-token".into()
            }
        );
    }

    #[test]
    fn an_explicit_auth_overrides_inheritance() {
        let ws = workspace_with(&[]);
        let mut collection = Collection::new(ws.id.clone(), "C");
        collection.auth = AuthConfig::Bearer {
            token: "inherited".into(),
        };

        let mut doc = RequestDoc::new(ws.id.clone(), "R");
        doc.auth = AuthConfig::None;

        let input = ResolutionInput {
            workspace: Some(&ws),
            folder_chain: vec![&collection],
            environment: None,
        };
        let resolved = resolve(&doc, &input, &build_context(&input));
        assert_eq!(resolved.spec.auth, AuthConfig::None);
    }

    #[test]
    fn required_secrets_are_identified_for_just_in_time_injection() {
        let ws = workspace_with(&[]);
        let mut env = Environment::new(ws.id.clone(), "Prod");
        env.variables = vec![
            Variable::new("baseUrl", "https://api.x"),
            Variable::new("apiToken", "").into_secret(),
        ];

        let mut doc = RequestDoc::new(ws.id.clone(), "R");
        doc.url = "{{baseUrl}}/users".into();
        doc.auth = AuthConfig::Bearer {
            token: "{{apiToken}}".into(),
        };

        let input = ResolutionInput {
            workspace: Some(&ws),
            folder_chain: vec![],
            environment: Some(&env),
        };
        let resolved = resolve(&doc, &input, &build_context(&input));

        assert_eq!(resolved.required_secrets, vec!["apiToken"]);
        // Not yet substituted: the value is empty until the keyring supplies it.
        assert_eq!(
            resolved.spec.auth,
            AuthConfig::Bearer {
                token: String::new()
            }
        );
    }

    #[test]
    fn method_and_settings_pass_through_unchanged() {
        let ws = workspace_with(&[]);
        let mut doc = RequestDoc::new(ws.id.clone(), "R");
        doc.method = HttpMethod::Patch;
        doc.settings.timeout_ms = Some(1234);
        doc.settings.verify_tls = false;

        let input = ResolutionInput {
            workspace: Some(&ws),
            ..Default::default()
        };
        let resolved = resolve(&doc, &input, &build_context(&input));

        assert_eq!(resolved.spec.method, HttpMethod::Patch);
        assert_eq!(resolved.spec.settings.timeout_ms, Some(1234));
        assert!(!resolved.spec.settings.verify_tls);
    }

    #[test]
    fn a_request_with_no_workspace_still_resolves() {
        let doc = RequestDoc::new(WorkspaceId::new(), "R");
        let input = ResolutionInput::default();
        let resolved = resolve(&doc, &input, &build_context(&input));
        assert_eq!(resolved.spec.url, "");
    }
}
