//! Calling a service nobody compiled against.
//!
//! Two steps, and the first is why this is possible at all: ask the
//! server for its descriptors, then use them to build a request, send
//! it, and read the answer back as JSON.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MethodDescriptor, SerializeOptions};
use tonic::transport::{Channel, Endpoint};

use crate::error::{GrpcError, GrpcResult};
use crate::reflection::{MessageRequest, MessageResponse, ServerReflectionRequest};
use crate::schema::GrpcSchema;

/// How long to wait for a connection.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A connection to one server.
pub struct GrpcClient {
    channel: Channel,
}

impl std::fmt::Debug for GrpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcClient").finish_non_exhaustive()
    }
}

/// Turns what somebody typed into something tonic will dial.
///
/// `grpc://` and `grpcs://` are accepted because that is what a lot
/// of documentation prints, and a bare `host:port` because that is
/// what `grpcurl` takes and what people have in their fingers.
pub fn endpoint_url(raw: &str) -> GrpcResult<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(GrpcError::InvalidUrl("the URL is empty".into()));
    }

    let normalised = match raw.split_once("://") {
        Some(("grpc", rest)) => format!("http://{rest}"),
        Some(("grpcs", rest)) => format!("https://{rest}"),
        Some(("http" | "https", _)) => raw.to_string(),
        Some((other, _)) => {
            return Err(GrpcError::InvalidUrl(format!(
                "unsupported scheme '{other}' — a gRPC URL is grpc:// or grpcs://"
            )));
        }
        // No scheme. Plain HTTP/2 is the usual case for a local
        // server, and guessing TLS would fail confusingly against
        // one.
        None => format!("http://{raw}"),
    };

    Ok(normalised.trim_end_matches('/').to_string())
}

impl GrpcClient {
    /// Opens a connection.
    pub async fn connect(url: &str) -> GrpcResult<Self> {
        let endpoint = Endpoint::from_shared(endpoint_url(url)?)
            .map_err(|e| GrpcError::InvalidUrl(e.to_string()))?
            .connect_timeout(CONNECT_TIMEOUT)
            // A server that never answers should not hold a window
            // open forever; a streaming call sets its own.
            .timeout(std::time::Duration::from_secs(120));

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| GrpcError::Connect(e.to_string()))?;

        Ok(Self { channel })
    }

    /// Asks the server what it serves.
    ///
    /// Tries the standardised path first and the pre-2023 one after,
    /// because plenty of running servers only answer the latter and
    /// a client that only spoke `v1` would call them unreachable.
    pub async fn reflect(&self) -> GrpcResult<(DescriptorPool, GrpcSchema)> {
        let mut last = None;
        for path in [crate::reflection::V1, crate::reflection::V1ALPHA] {
            match self.reflect_at(path).await {
                Ok(pool) => {
                    let schema = GrpcSchema::from_pool(&pool);
                    return Ok((pool, schema));
                }
                Err(error) => last = Some(error),
            }
        }

        Err(last.unwrap_or_else(|| {
            GrpcError::NoReflection("the server did not answer reflection".into())
        }))
    }

    async fn reflect_at(&self, path: &'static str) -> GrpcResult<DescriptorPool> {
        let services = self.list_services(path).await?;

        let mut pool = DescriptorPool::new();
        for service in &services {
            if service.starts_with("grpc.reflection.") {
                continue;
            }
            for bytes in self.files_for(path, service).await? {
                let file = prost_types::FileDescriptorProto::decode(bytes.as_slice())
                    .map_err(|e| GrpcError::BadDescriptors(e.to_string()))?;
                // Already present: reflection returns a file once per
                // service that depends on it, so most are repeats.
                if pool.get_file_by_name(file.name()).is_some() {
                    continue;
                }
                pool.add_file_descriptor_proto(file)
                    .map_err(|e| GrpcError::BadDescriptors(e.to_string()))?;
            }
        }

        Ok(pool)
    }

    async fn list_services(&self, path: &'static str) -> GrpcResult<Vec<String>> {
        let response = self
            .reflection_call(
                path,
                MessageRequest::ListServices(String::new()),
            )
            .await?;

        match response {
            MessageResponse::ListServicesResponse(list) => {
                Ok(list.service.into_iter().map(|s| s.name).collect())
            }
            MessageResponse::ErrorResponse(error) => {
                Err(GrpcError::NoReflection(error.error_message))
            }
            _ => Err(GrpcError::NoReflection(
                "the server answered reflection with something unexpected".into(),
            )),
        }
    }

    async fn files_for(&self, path: &'static str, symbol: &str) -> GrpcResult<Vec<Vec<u8>>> {
        let response = self
            .reflection_call(
                path,
                MessageRequest::FileContainingSymbol(symbol.to_string()),
            )
            .await?;

        match response {
            MessageResponse::FileDescriptorResponse(files) => Ok(files.file_descriptor_proto),
            MessageResponse::ErrorResponse(error) => {
                Err(GrpcError::NoReflection(error.error_message))
            }
            _ => Err(GrpcError::NoReflection(
                "the server answered reflection with something unexpected".into(),
            )),
        }
    }

    /// One request down the reflection stream, one response back.
    async fn reflection_call(
        &self,
        path: &'static str,
        request: MessageRequest,
    ) -> GrpcResult<MessageResponse> {
        use futures_util::StreamExt;

        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready()
            .await
            .map_err(|e| GrpcError::Connect(e.to_string()))?;

        let outgoing = futures_util::stream::iter(vec![ServerReflectionRequest {
            host: String::new(),
            message_request: Some(request),
        }]);

        let mut stream: tonic::Streaming<crate::reflection::ServerReflectionResponse> = grpc
            .streaming(
                tonic::Request::new(outgoing),
                http::uri::PathAndQuery::from_static(path),
                tonic_prost::ProstCodec::default(),
            )
            .await?
            .into_inner();

        let message = stream
            .next()
            .await
            .transpose()?
            .ok_or_else(|| GrpcError::NoReflection("reflection answered nothing".into()))?;

        message.message_response.ok_or_else(|| {
            GrpcError::NoReflection("reflection answered with an empty response".into())
        })
    }

    /// Calls a unary method, JSON in and JSON out.
    pub async fn unary(
        &self,
        pool: &DescriptorPool,
        service: &str,
        method: &str,
        body: &str,
        metadata: &[(String, String)],
    ) -> GrpcResult<String> {
        let descriptor = find_method(pool, service, method)?;
        if descriptor.is_client_streaming() || descriptor.is_server_streaming() {
            return Err(GrpcError::NoSuchMethod(format!(
                "{service}/{method} is a streaming method — open it as a stream rather than \
                 sending it as a single call"
            )));
        }

        let request = build_request(&descriptor, body)?;

        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready()
            .await
            .map_err(|e| GrpcError::Connect(e.to_string()))?;

        let mut outgoing = tonic::Request::new(request);
        apply_metadata(&mut outgoing, metadata)?;

        let path = method_path(service, method)?;
        let response = grpc
            .unary(
                outgoing,
                path,
                crate::codec::DynamicCodec::new(descriptor.output()),
            )
            .await?;

        to_json(response.into_inner())
    }
}

/// Builds the path a method lives at.
fn method_path(service: &str, method: &str) -> GrpcResult<http::uri::PathAndQuery> {
    format!("/{service}/{method}")
        .parse()
        .map_err(|_| GrpcError::NoSuchMethod(format!("{service}/{method} is not a valid path")))
}

/// Looks a method up, saying what is actually there when it is not.
fn find_method(
    pool: &DescriptorPool,
    service: &str,
    method: &str,
) -> GrpcResult<MethodDescriptor> {
    let found = pool.get_service_by_name(service).ok_or_else(|| {
        let known: Vec<String> =
            pool.services().map(|s| s.full_name().to_string()).collect();
        GrpcError::NoSuchMethod(format!(
            "no service called '{service}'. This server has: {}",
            known.join(", ")
        ))
    })?;

    found
        .methods()
        .find(|m| m.name() == method)
        .ok_or_else(|| {
            let known: Vec<String> = found.methods().map(|m| m.name().to_string()).collect();
            GrpcError::NoSuchMethod(format!(
                "'{service}' has no method '{method}'. It has: {}",
                known.join(", ")
            ))
        })
}

/// Turns a JSON body into the method's request message.
fn build_request(method: &MethodDescriptor, body: &str) -> GrpcResult<DynamicMessage> {
    let input = method.input();
    let text = body.trim();
    // An empty editor means an empty message, which plenty of methods
    // take. Refusing it would make the common case the awkward one.
    if text.is_empty() {
        return Ok(DynamicMessage::new(input));
    }

    let mut deserializer = serde_json::Deserializer::from_str(text);
    DynamicMessage::deserialize(input.clone(), &mut deserializer).map_err(|e| {
        GrpcError::BadRequest {
            message: input.full_name().to_string(),
            detail: e.to_string(),
        }
    })
}

/// Renders a response message as JSON.
pub fn to_json(message: DynamicMessage) -> GrpcResult<String> {
    let mut buffer = Vec::new();
    let mut serializer = serde_json::Serializer::pretty(&mut buffer);
    message
        .serialize_with_options(
            &mut serializer,
            // Fields left at their default are still part of the
            // answer: "balance: 0" and "no balance field" are
            // different things to somebody testing a service.
            &SerializeOptions::new().skip_default_fields(false),
        )
        .map_err(|e| GrpcError::Other(e.to_string()))?;

    String::from_utf8(buffer).map_err(|e| GrpcError::Other(e.to_string()))
}

/// Puts the request's headers on the call as gRPC metadata.
fn apply_metadata<T>(
    request: &mut tonic::Request<T>,
    metadata: &[(String, String)],
) -> GrpcResult<()> {
    for (name, value) in metadata {
        let key: tonic::metadata::MetadataKey<_> = name
            .to_ascii_lowercase()
            .parse()
            .map_err(|_| GrpcError::Other(format!("'{name}' is not a valid metadata name")))?;
        let value = value
            .parse()
            .map_err(|_| GrpcError::Other(format!("'{name}' has a value gRPC cannot carry")))?;
        request.metadata_mut().insert(key, value);
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schemes_people_actually_type_are_accepted() {
        assert_eq!(endpoint_url("grpc://host:50051").unwrap(), "http://host:50051");
        assert_eq!(endpoint_url("grpcs://host:443").unwrap(), "https://host:443");
        assert_eq!(endpoint_url("http://host:50051").unwrap(), "http://host:50051");
        // What grpcurl takes, and so what is in people's fingers.
        assert_eq!(endpoint_url("localhost:50051").unwrap(), "http://localhost:50051");
    }

    #[test]
    fn another_scheme_is_named_in_the_message() {
        let error = endpoint_url("ftp://host").unwrap_err().to_string();
        assert!(error.contains("ftp"), "{error}");
        assert!(error.contains("grpc://"), "{error}");
    }

    #[test]
    fn an_empty_url_is_refused() {
        assert!(endpoint_url("   ").is_err());
    }

    #[test]
    fn a_method_path_is_built_the_way_grpc_wants_it() {
        let path = method_path("helloworld.Greeter", "SayHello").unwrap();
        assert_eq!(path.path(), "/helloworld.Greeter/SayHello");
    }
}
