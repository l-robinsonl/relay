//! Calling a real gRPC server that Relay was never compiled against.
//!
//! The server here is built by hand — descriptors constructed in
//! Rust, framing written out — for one reason: `protoc` must not be
//! a requirement for building Relay, and tonic's generated server
//! would make it one. The cost is this file; the benefit is that the
//! whole path, reflection through to a decoded answer, is exercised
//! in CI on three platforms.

use std::convert::Infallible;

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::{Router, routing::post};
use http_body_util::StreamBody;
use hyper::body::Frame;
use prost::Message as _;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{
    DescriptorProto, FieldDescriptorProto, FileDescriptorProto, MethodDescriptorProto,
    ServiceDescriptorProto,
};
use relay_grpc::GrpcClient;
use relay_grpc::reflection::{
    FileDescriptorResponse, ListServiceResponse, MessageRequest, MessageResponse,
    ServerReflectionRequest, ServerReflectionResponse, ServiceResponse,
};

/// The service the test server offers:
///
/// - `package relay.test;`
/// - `message Ping { string text = 1; int32 count = 2; }`
/// - `message Pong { string text = 1; }`
/// - `service Echo { rpc Say(Ping) returns (Pong); }`
fn descriptor() -> FileDescriptorProto {
    fn field(name: &str, number: i32, kind: Type) -> FieldDescriptorProto {
        FieldDescriptorProto {
            name: Some(name.to_string()),
            number: Some(number),
            label: Some(Label::Optional as i32),
            r#type: Some(kind as i32),
            json_name: Some(name.to_string()),
            ..Default::default()
        }
    }

    FileDescriptorProto {
        name: Some("relay/test.proto".into()),
        package: Some("relay.test".into()),
        syntax: Some("proto3".into()),
        message_type: vec![
            DescriptorProto {
                name: Some("Ping".into()),
                field: vec![field("text", 1, Type::String), field("count", 2, Type::Int32)],
                ..Default::default()
            },
            DescriptorProto {
                name: Some("Pong".into()),
                field: vec![field("text", 1, Type::String)],
                ..Default::default()
            },
        ],
        service: vec![ServiceDescriptorProto {
            name: Some("Echo".into()),
            method: vec![
                MethodDescriptorProto {
                    name: Some("Say".into()),
                    input_type: Some(".relay.test.Ping".into()),
                    output_type: Some(".relay.test.Pong".into()),
                    ..Default::default()
                },
                // Exists, and refuses. A method that is not in the
                // descriptor at all is a different test: the client
                // stops that one before it reaches the wire.
                MethodDescriptorProto {
                    name: Some("Fail".into()),
                    input_type: Some(".relay.test.Ping".into()),
                    output_type: Some(".relay.test.Pong".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Wraps a message in gRPC's length-prefixed framing.
fn frame(message: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(message.len() + 5);
    out.push(0); // not compressed
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    Bytes::from(out)
}

/// Unwraps one, ignoring anything after the first.
fn unframe(body: &[u8]) -> Vec<u8> {
    if body.len() < 5 {
        return Vec::new();
    }
    let len = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    body[5..5 + len.min(body.len() - 5)].to_vec()
}

/// A gRPC response: framed body, then the trailers carrying the status.
fn respond(message: Bytes, status: &str, detail: &str) -> Response {
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", HeaderValue::from_str(status).unwrap());
    trailers.insert("grpc-message", HeaderValue::from_str(detail).unwrap());

    let frames = futures_util::stream::iter(vec![
        Ok::<_, Infallible>(Frame::data(message)),
        Ok(Frame::trailers(trailers)),
    ]);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .body(axum::body::Body::new(StreamBody::new(frames)))
        .unwrap()
}

fn pool() -> prost_reflect::DescriptorPool {
    let mut pool = prost_reflect::DescriptorPool::new();
    pool.add_file_descriptor_proto(descriptor()).unwrap();
    pool
}

/// Starts the server. Returns its address.
async fn server() -> String {
    async fn reflect(body: Bytes) -> Response {
        let Ok(request) = ServerReflectionRequest::decode(unframe(&body).as_slice()) else {
            return respond(Bytes::new(), "13", "unreadable request");
        };

        let answer = match request.message_request {
            Some(MessageRequest::ListServices(_)) => {
                MessageResponse::ListServicesResponse(ListServiceResponse {
                    service: vec![ServiceResponse { name: "relay.test.Echo".into() }],
                })
            }
            Some(MessageRequest::FileContainingSymbol(_)) => {
                MessageResponse::FileDescriptorResponse(FileDescriptorResponse {
                    file_descriptor_proto: vec![descriptor().encode_to_vec()],
                })
            }
            _ => return respond(Bytes::new(), "12", "unimplemented"),
        };

        let response = ServerReflectionResponse {
            valid_host: String::new(),
            message_response: Some(answer),
        };
        respond(frame(&response.encode_to_vec()), "0", "")
    }

    async fn say(body: Bytes) -> Response {
        // Decoded and re-encoded with the same descriptors the client
        // discovered, which is what makes this a real round trip
        // rather than a fixture.
        let pool = pool();
        let ping = pool.get_message_by_name("relay.test.Ping").unwrap();
        let pong = pool.get_message_by_name("relay.test.Pong").unwrap();

        let Ok(message) = prost_reflect::DynamicMessage::decode(ping, unframe(&body).as_slice())
        else {
            return respond(Bytes::new(), "3", "could not read the request");
        };

        let text = message
            .get_field_by_name("text")
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .unwrap_or_default();

        let mut answer = prost_reflect::DynamicMessage::new(pong);
        answer.set_field_by_name("text", prost_reflect::Value::String(format!("you said {text}")));

        respond(frame(&answer.encode_to_vec()), "0", "")
    }

    async fn refuse() -> Response {
        respond(Bytes::new(), "5", "no such customer")
    }

    let app = Router::new()
        .route("/grpc.reflection.v1.ServerReflection/ServerReflectionInfo", post(reflect))
        .route("/relay.test.Echo/Say", post(say))
        .route("/relay.test.Echo/Fail", post(refuse));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

#[tokio::test]
async fn a_server_is_asked_what_it_serves() {
    let client = GrpcClient::connect(&server().await).await.unwrap();
    let (_pool, schema) = client.reflect().await.expect("reflection failed");

    assert_eq!(schema.services.len(), 1);
    let service = &schema.services[0];
    assert_eq!(service.name, "relay.test.Echo");
    assert_eq!(service.methods.len(), 2);

    let method = &service.methods[0];
    assert_eq!(method.name, "Say");
    assert_eq!(method.input_type, "relay.test.Ping");
    assert_eq!(method.output_type, "relay.test.Pong");
    assert!(!method.client_streaming);
    assert!(!method.server_streaming);
}

#[tokio::test]
async fn the_example_shows_what_the_method_wants() {
    // Somebody facing an empty editor and a method called `Say` has
    // no way to know it takes a `text` and a `count`.
    let client = GrpcClient::connect(&server().await).await.unwrap();
    let (_pool, schema) = client.reflect().await.unwrap();

    let example = &schema.services[0].methods[0].example;
    let parsed: serde_json::Value = serde_json::from_str(example).expect("not valid JSON");

    assert_eq!(parsed["text"], "");
    assert_eq!(parsed["count"], 0);
}

#[tokio::test]
async fn a_method_is_called_with_json_and_answers_with_json() {
    // The whole point: no generated code anywhere, the message types
    // learned a moment ago over the wire.
    let client = GrpcClient::connect(&server().await).await.unwrap();
    let (pool, _) = client.reflect().await.unwrap();

    let answer = client
        .unary(&pool, "relay.test.Echo", "Say", r#"{"text": "hello"}"#, &[])
        .await
        .expect("the call failed");

    let parsed: serde_json::Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(parsed["text"], "you said hello");
}

#[tokio::test]
async fn an_empty_body_is_an_empty_message_rather_than_an_error() {
    // Plenty of methods take nothing, and an empty editor is how that
    // looks.
    let client = GrpcClient::connect(&server().await).await.unwrap();
    let (pool, _) = client.reflect().await.unwrap();

    let answer = client
        .unary(&pool, "relay.test.Echo", "Say", "   ", &[])
        .await
        .expect("an empty body was refused");

    assert!(answer.contains("you said"));
}

#[tokio::test]
async fn a_field_the_message_does_not_have_is_reported_before_sending() {
    let client = GrpcClient::connect(&server().await).await.unwrap();
    let (pool, _) = client.reflect().await.unwrap();

    let error = client
        .unary(&pool, "relay.test.Echo", "Say", r#"{"nonsense": 1}"#, &[])
        .await
        .expect_err("a field that does not exist was accepted");

    let text = error.to_string();
    assert!(text.contains("relay.test.Ping"), "{text}");
}

#[tokio::test]
async fn the_servers_own_status_is_what_comes_back() {
    // `NotFound` with the server's message is the thing somebody
    // testing a service wants to see, not "the call failed".
    let client = GrpcClient::connect(&server().await).await.unwrap();
    let (pool, _) = client.reflect().await.unwrap();

    let error = client
        .unary(&pool, "relay.test.Echo", "Fail", "{}", &[])
        .await
        .expect_err("a refusal was read as success");

    let text = error.to_string();
    assert!(text.contains("NotFound"), "{text}");
    assert!(text.contains("no such customer"), "{text}");
}

#[tokio::test]
async fn asking_for_a_method_that_is_not_there_lists_what_is() {
    let client = GrpcClient::connect(&server().await).await.unwrap();
    let (pool, _) = client.reflect().await.unwrap();

    let error = client
        .unary(&pool, "relay.test.Echo", "Shout", "{}", &[])
        .await
        .expect_err("a method that does not exist was called");

    let text = error.to_string();
    assert!(text.contains("Shout"), "{text}");
    // And says what it could have called instead.
    assert!(text.contains("Say"), "{text}");
}

#[tokio::test]
async fn a_server_that_is_not_there_says_so() {
    let error = GrpcClient::connect("http://127.0.0.1:1").await.unwrap_err();
    assert!(error.to_string().contains("connect"), "{error}");
}
