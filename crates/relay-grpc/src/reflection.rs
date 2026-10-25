//! Asking a server what it serves.
//!
//! The gRPC equivalent of GraphQL introspection: a well-known service
//! that hands back the `.proto` descriptors for everything else the
//! server exposes. With those, Relay can call a method it was never
//! compiled against.
//!
//! The message types here are written out by hand rather than
//! generated. Generating them would mean `protoc` on every machine
//! that builds Relay and in CI, for four small messages whose shape
//! has not changed since 2016.

use prost::Message;

/// What we ask for.
#[derive(Clone, PartialEq, Message)]
pub struct ServerReflectionRequest {
    #[prost(string, tag = "1")]
    pub host: String,
    #[prost(oneof = "MessageRequest", tags = "3, 4, 6, 7")]
    pub message_request: Option<MessageRequest>,
}

/// The one question this request carries.
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum MessageRequest {
    /// Everything describing one file.
    #[prost(string, tag = "3")]
    FileByFilename(String),
    /// Everything describing whatever declares this symbol.
    #[prost(string, tag = "4")]
    FileContainingSymbol(String),
    /// The extension numbers of a type.
    #[prost(message, tag = "6")]
    AllExtensionNumbersOfType(String),
    /// Every service on the server. The value is ignored.
    #[prost(string, tag = "7")]
    ListServices(String),
}

/// What comes back.
#[derive(Clone, PartialEq, Message)]
pub struct ServerReflectionResponse {
    #[prost(string, tag = "1")]
    pub valid_host: String,
    #[prost(oneof = "MessageResponse", tags = "4, 6, 7")]
    pub message_response: Option<MessageResponse>,
}

/// The one answer a response carries.
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum MessageResponse {
    /// Serialised `FileDescriptorProto`s, including dependencies.
    #[prost(message, tag = "4")]
    FileDescriptorResponse(FileDescriptorResponse),
    /// Extension numbers.
    #[prost(message, tag = "5")]
    AllExtensionNumbersResponse(ExtensionNumberResponse),
    /// The services on offer.
    #[prost(message, tag = "6")]
    ListServicesResponse(ListServiceResponse),
    /// Why not.
    #[prost(message, tag = "7")]
    ErrorResponse(ErrorResponse),
}

/// Descriptors, one encoded `FileDescriptorProto` per entry.
#[derive(Clone, PartialEq, Message)]
pub struct FileDescriptorResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub file_descriptor_proto: Vec<Vec<u8>>,
}

/// Extension numbers of a type. Unused, but part of the oneof.
#[derive(Clone, PartialEq, Message)]
pub struct ExtensionNumberResponse {
    #[prost(string, tag = "1")]
    pub base_type_name: String,
    #[prost(int32, repeated, tag = "2")]
    pub extension_number: Vec<i32>,
}

/// Every service the server exposes.
#[derive(Clone, PartialEq, Message)]
pub struct ListServiceResponse {
    #[prost(message, repeated, tag = "1")]
    pub service: Vec<ServiceResponse>,
}

/// One service's fully-qualified name.
#[derive(Clone, PartialEq, Message)]
pub struct ServiceResponse {
    #[prost(string, tag = "1")]
    pub name: String,
}

/// A refusal, in gRPC's own status vocabulary.
#[derive(Clone, PartialEq, Message)]
pub struct ErrorResponse {
    #[prost(int32, tag = "1")]
    pub error_code: i32,
    #[prost(string, tag = "2")]
    pub error_message: String,
}

/// The reflection service's two paths.
///
/// `v1` was standardised in 2023 and `v1alpha` is what everything
/// built before then serves — including, at the time of writing,
/// plenty of production servers. Trying one and falling back to the
/// other is the only way to reach both.
pub const V1: &str = "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo";
/// The older path, served by anything predating the v1 standard.
pub const V1ALPHA: &str =
    "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_services_request_round_trips() {
        // Hand-written descriptors are the kind of thing that is
        // wrong by one tag number and fails only against a real
        // server, so the encoding is checked here.
        let request = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::ListServices(String::new())),
        };

        let bytes = request.encode_to_vec();
        let back = ServerReflectionRequest::decode(bytes.as_slice()).unwrap();

        assert!(matches!(
            back.message_request,
            Some(MessageRequest::ListServices(_))
        ));
    }

    #[test]
    fn a_symbol_request_keeps_its_symbol() {
        let request = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::FileContainingSymbol(
                "helloworld.Greeter".into(),
            )),
        };

        let back = ServerReflectionRequest::decode(request.encode_to_vec().as_slice()).unwrap();

        assert!(matches!(
            back.message_request,
            Some(MessageRequest::FileContainingSymbol(s)) if s == "helloworld.Greeter"
        ));
    }

    #[test]
    fn a_list_response_is_read_back() {
        let response = ServerReflectionResponse {
            valid_host: String::new(),
            message_response: Some(MessageResponse::ListServicesResponse(ListServiceResponse {
                service: vec![
                    ServiceResponse { name: "helloworld.Greeter".into() },
                    ServiceResponse { name: "grpc.reflection.v1.ServerReflection".into() },
                ],
            })),
        };

        let back = ServerReflectionResponse::decode(response.encode_to_vec().as_slice()).unwrap();

        let Some(MessageResponse::ListServicesResponse(list)) = back.message_response else {
            panic!("the oneof did not survive the round trip");
        };
        assert_eq!(list.service.len(), 2);
        assert_eq!(list.service[0].name, "helloworld.Greeter");
    }
}
