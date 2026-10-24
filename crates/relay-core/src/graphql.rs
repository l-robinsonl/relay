//! Reading a GraphQL schema out of an endpoint.
//!
//! Relay already sends GraphQL — a query, variables, posted as JSON.
//! What it could not do was tell you what the server accepts, so
//! writing a query meant having the documentation open beside it.
//!
//! Introspection is a GraphQL query like any other, which is the
//! useful part: it goes out through the ordinary request path, with
//! whatever authentication, headers and TLS settings that request
//! already has. A schema browser that could not see a private API
//! would be a schema browser for public demos.
//!
//! Parsing only. Sending lives in `relay-app`, because this crate
//! does no I/O.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// The introspection query.
///
/// Deliberately not the full one from the specification: that pulls
/// every directive, every deprecation reason and the whole type tree
/// to seven levels, and on a large schema the response is megabytes.
/// This asks for what a person writing a query needs — the operations,
/// their arguments, and the shape of what comes back.
pub const INTROSPECTION_QUERY: &str = r#"
query RelayIntrospection {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types {
      kind
      name
      description
      fields(includeDeprecated: false) {
        name
        description
        args { name description type { ...TypeRef } }
        type { ...TypeRef }
      }
      inputFields { name description type { ...TypeRef } }
      enumValues(includeDeprecated: false) { name description }
    }
  }
}

fragment TypeRef on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType { kind name }
    }
  }
}
"#;

/// A field a caller can ask for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Field {
    /// Its name.
    pub name: String,
    /// What the schema says it is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The type it returns, rendered the way it is written in a
    /// query — `[Invoice!]!` rather than a nest of wrappers.
    pub type_name: String,
    /// Arguments it takes.
    #[serde(default)]
    pub args: Vec<Argument>,
}

/// One argument to a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Argument {
    /// Its name.
    pub name: String,
    /// Its type, as written.
    pub type_name: String,
    /// What the schema says it is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A named type in the schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NamedType {
    /// Its name.
    pub name: String,
    /// `OBJECT`, `INPUT_OBJECT`, `ENUM`, and so on.
    pub kind: String,
    /// What the schema says it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its fields, for an object or input object.
    #[serde(default)]
    pub fields: Vec<Field>,
    /// Its values, for an enum.
    #[serde(default)]
    pub enum_values: Vec<String>,
}

/// What an endpoint accepts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Schema {
    /// The queries it offers.
    #[serde(default)]
    pub queries: Vec<Field>,
    /// The mutations it offers.
    #[serde(default)]
    pub mutations: Vec<Field>,
    /// The subscriptions it offers.
    #[serde(default)]
    pub subscriptions: Vec<Field>,
    /// Every named type, for looking up what a field returns.
    /// Built-in scalars and introspection's own `__` types are left
    /// out: nobody is browsing for `String`.
    #[serde(default)]
    pub types: Vec<NamedType>,
}

impl Schema {
    /// Reads a schema out of an introspection response.
    ///
    /// Takes the whole response body, `{"data": {"__schema": …}}`, or
    /// the `__schema` object on its own — a server behind a gateway
    /// sometimes unwraps it, and failing on that would be a puzzle
    /// for whoever hit it.
    pub fn parse(body: &Value) -> Result<Self, String> {
        if let Some(errors) = body["errors"].as_array()
            && !errors.is_empty()
        {
            let first = errors[0]["message"]
                .as_str()
                .unwrap_or("the server rejected the introspection query");
            return Err(first.to_string());
        }

        let schema = match (&body["data"]["__schema"], &body["__schema"]) {
            (Value::Object(_), _) => &body["data"]["__schema"],
            (_, Value::Object(_)) => &body["__schema"],
            _ => {
                return Err(
                    "no schema in the response — the endpoint may have introspection \
                     turned off, which many do in production"
                        .into(),
                );
            }
        };

        let root = |key: &str| schema[key]["name"].as_str().unwrap_or_default().to_string();
        let (query_root, mutation_root, subscription_root) = (
            root("queryType"),
            root("mutationType"),
            root("subscriptionType"),
        );

        let mut parsed = Self::default();

        for raw in schema["types"].as_array().unwrap_or(&Vec::new()) {
            let name = raw["name"].as_str().unwrap_or_default().to_string();
            // Introspection describes itself, which nobody wants to
            // browse.
            if name.is_empty() || name.starts_with("__") {
                continue;
            }

            let fields: Vec<Field> = raw["fields"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(parse_field)
                .collect();

            if name == query_root {
                parsed.queries = fields.clone();
            } else if name == mutation_root {
                parsed.mutations = fields.clone();
            } else if name == subscription_root {
                parsed.subscriptions = fields.clone();
            }

            let kind = raw["kind"].as_str().unwrap_or_default().to_string();
            // Built-in scalars carry no information worth a row.
            if kind == "SCALAR" && is_builtin_scalar(&name) {
                continue;
            }

            let input_fields: Vec<Field> = raw["inputFields"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(parse_field)
                .collect();

            parsed.types.push(NamedType {
                name,
                kind,
                description: description(&raw["description"]),
                fields: if fields.is_empty() {
                    input_fields
                } else {
                    fields
                },
                enum_values: raw["enumValues"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .filter_map(|v| v["name"].as_str().map(str::to_string))
                    .collect(),
            });
        }

        parsed.types.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(parsed)
    }
}

fn parse_field(raw: &Value) -> Field {
    Field {
        name: raw["name"].as_str().unwrap_or_default().to_string(),
        description: description(&raw["description"]),
        type_name: render_type(&raw["type"]),
        args: raw["args"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|arg| Argument {
                name: arg["name"].as_str().unwrap_or_default().to_string(),
                type_name: render_type(&arg["type"]),
                description: description(&arg["description"]),
            })
            .collect(),
    }
}

/// Blank descriptions are noise; `None` renders as nothing.
fn description(raw: &Value) -> Option<String> {
    raw.as_str()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
}

/// Renders a type reference the way it is written in a query.
///
/// Introspection returns `NON_NULL(LIST(NON_NULL(Invoice)))`; a person
/// writing a query sees `[Invoice!]!`, and showing them the nest
/// would be showing them the wire format.
fn render_type(raw: &Value) -> String {
    match raw["kind"].as_str() {
        Some("NON_NULL") => format!("{}!", render_type(&raw["ofType"])),
        Some("LIST") => format!("[{}]", render_type(&raw["ofType"])),
        _ => raw["name"].as_str().unwrap_or("Unknown").to_string(),
    }
}

fn is_builtin_scalar(name: &str) -> bool {
    matches!(name, "String" | "Int" | "Float" | "Boolean" | "ID")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response() -> Value {
        json!({
            "data": { "__schema": {
                "queryType": { "name": "Query" },
                "mutationType": { "name": "Mutation" },
                "subscriptionType": null,
                "types": [
                    {
                        "kind": "OBJECT", "name": "Query", "description": null,
                        "fields": [{
                            "name": "invoices",
                            "description": "Every invoice, newest first.",
                            "args": [{
                                "name": "limit", "description": null,
                                "type": { "kind": "SCALAR", "name": "Int" }
                            }],
                            "type": { "kind": "NON_NULL", "name": null, "ofType": {
                                "kind": "LIST", "name": null, "ofType": {
                                    "kind": "NON_NULL", "name": null,
                                    "ofType": { "kind": "OBJECT", "name": "Invoice" }
                                }
                            }}
                        }]
                    },
                    {
                        "kind": "OBJECT", "name": "Mutation", "description": null,
                        "fields": [{
                            "name": "voidInvoice", "description": null, "args": [],
                            "type": { "kind": "OBJECT", "name": "Invoice" }
                        }]
                    },
                    {
                        "kind": "OBJECT", "name": "Invoice", "description": "A bill.",
                        "fields": [{
                            "name": "id", "description": null, "args": [],
                            "type": { "kind": "NON_NULL", "name": null,
                                      "ofType": { "kind": "SCALAR", "name": "ID" } }
                        }]
                    },
                    { "kind": "SCALAR", "name": "String", "description": null },
                    { "kind": "OBJECT", "name": "__Type", "description": null }
                ]
            }}
        })
    }

    #[test]
    fn the_operations_come_out_separated() {
        let schema = Schema::parse(&response()).unwrap();

        assert_eq!(schema.queries.len(), 1);
        assert_eq!(schema.queries[0].name, "invoices");
        assert_eq!(schema.mutations.len(), 1);
        assert_eq!(schema.mutations[0].name, "voidInvoice");
        assert!(schema.subscriptions.is_empty());
    }

    #[test]
    fn a_type_reads_as_it_is_written_in_a_query() {
        // Introspection nests wrappers; nobody writes those.
        let schema = Schema::parse(&response()).unwrap();
        assert_eq!(schema.queries[0].type_name, "[Invoice!]!");
    }

    #[test]
    fn arguments_and_descriptions_survive() {
        let schema = Schema::parse(&response()).unwrap();
        let invoices = &schema.queries[0];

        assert_eq!(
            invoices.description.as_deref(),
            Some("Every invoice, newest first.")
        );
        assert_eq!(invoices.args.len(), 1);
        assert_eq!(invoices.args[0].name, "limit");
        assert_eq!(invoices.args[0].type_name, "Int");
    }

    #[test]
    fn nothing_introspective_or_built_in_is_listed() {
        // Nobody browses a schema looking for `String`.
        let schema = Schema::parse(&response()).unwrap();
        let names: Vec<&str> = schema.types.iter().map(|t| t.name.as_str()).collect();

        assert!(names.contains(&"Invoice"));
        assert!(!names.contains(&"String"), "{names:?}");
        assert!(!names.iter().any(|n| n.starts_with("__")), "{names:?}");
    }

    #[test]
    fn a_rejected_query_says_why() {
        // The common case by far: introspection is turned off in
        // production, and the server says so.
        let refused = json!({
            "errors": [{ "message": "GraphQL introspection is not allowed" }]
        });

        let error = Schema::parse(&refused).unwrap_err();
        assert!(error.contains("not allowed"), "{error}");
    }

    #[test]
    fn a_response_with_no_schema_explains_the_likeliest_reason() {
        let error = Schema::parse(&json!({ "data": {} })).unwrap_err();
        assert!(error.contains("introspection"), "{error}");
    }

    #[test]
    fn an_unwrapped_schema_is_accepted_too() {
        // Some gateways return the schema without the data envelope.
        let unwrapped = response()["data"].clone();
        let schema = Schema::parse(&unwrapped).unwrap();

        assert_eq!(schema.queries.len(), 1);
    }
}
