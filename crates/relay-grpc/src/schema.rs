//! What a server said it serves.
//!
//! The shape the interface shows, kept deliberately small: a person
//! picking a method needs its name, whether it streams, and what it
//! takes. The full descriptor pool stays in Rust, where the call is
//! made.

use prost_reflect::{DescriptorPool, MethodDescriptor};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Everything a server exposes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GrpcSchema {
    /// The services, reflection's own excluded.
    pub services: Vec<GrpcService>,
}

/// One service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GrpcService {
    /// Fully qualified, as the path needs it.
    pub name: String,
    /// What it offers.
    pub methods: Vec<GrpcMethod>,
}

/// One method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GrpcMethod {
    /// Just the method name; the service carries the rest.
    pub name: String,
    /// The request type's fully qualified name.
    pub input_type: String,
    /// The response type's.
    pub output_type: String,
    /// Whether the caller sends many messages.
    pub client_streaming: bool,
    /// Whether the server answers with many.
    pub server_streaming: bool,
    /// A request body with every field present, as JSON.
    ///
    /// Somebody facing an empty editor and a method called
    /// `CreateSubscription` needs to know what it wants. Generated
    /// from the descriptor rather than guessed.
    pub example: String,
}

impl GrpcSchema {
    /// Reads a pool into the shape the interface shows.
    pub fn from_pool(pool: &DescriptorPool) -> Self {
        let mut services: Vec<GrpcService> = pool
            .services()
            // Reflection is how we got here; listing it invites
            // somebody to call it by hand and learn nothing.
            .filter(|service| !service.full_name().starts_with("grpc.reflection."))
            .map(|service| GrpcService {
                name: service.full_name().to_string(),
                methods: service.methods().map(describe).collect(),
            })
            .collect();

        // Reflection returns them in whatever order the server's
        // files happen to be in, which differs between runs on some
        // servers and makes the list jump about.
        services.sort_by(|a, b| a.name.cmp(&b.name));
        Self { services }
    }
}

fn describe(method: MethodDescriptor) -> GrpcMethod {
    GrpcMethod {
        name: method.name().to_string(),
        input_type: method.input().full_name().to_string(),
        output_type: method.output().full_name().to_string(),
        client_streaming: method.is_client_streaming(),
        server_streaming: method.is_server_streaming(),
        example: example_for(&method.input()),
    }
}

/// A JSON skeleton of a message type.
///
/// Nested messages recurse, up to a depth, because a type that
/// contains itself — a tree node, a linked list — would otherwise
/// generate forever.
fn example_for(message: &prost_reflect::MessageDescriptor) -> String {
    serde_json::to_string_pretty(&skeleton(message, 0)).unwrap_or_else(|_| "{}".to_string())
}

fn skeleton(message: &prost_reflect::MessageDescriptor, depth: usize) -> serde_json::Value {
    use prost_reflect::Kind;
    use serde_json::{Map, Value, json};

    const MAX_DEPTH: usize = 4;

    let mut object = Map::new();
    for field in message.fields() {
        let value = match field.kind() {
            Kind::Message(nested) if depth >= MAX_DEPTH => {
                json!(format!("<{}>", nested.full_name()))
            }
            Kind::Message(nested) => skeleton(&nested, depth + 1),
            Kind::Enum(values) => values
                .values()
                .next()
                .map_or(Value::Null, |v| json!(v.name())),
            Kind::Bool => json!(false),
            Kind::String => json!(""),
            Kind::Bytes => json!(""),
            Kind::Double | Kind::Float => json!(0.0),
            // 64-bit integers are strings in protobuf's JSON mapping,
            // because they do not survive a double.
            Kind::Int64 | Kind::Uint64 | Kind::Sint64 | Kind::Fixed64 | Kind::Sfixed64 => {
                json!("0")
            }
            _ => json!(0),
        };

        object.insert(
            field.json_name().to_string(),
            if field.is_list() {
                json!([value])
            } else {
                value
            },
        );
    }

    Value::Object(object)
}
