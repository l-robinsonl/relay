//! What can go wrong talking gRPC.

/// A gRPC call or reflection attempt that did not work.
#[derive(Debug, thiserror::Error)]
pub enum GrpcError {
    /// The URL was not one we can connect to.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// The connection could not be made.
    #[error("could not connect: {0}")]
    Connect(String),

    /// The server does not answer reflection.
    ///
    /// Its own message is kept: "unimplemented" means reflection is
    /// switched off, which is a different problem from the server
    /// being unreachable, and the person can fix one and not the
    /// other.
    #[error("{0}")]
    NoReflection(String),

    /// The descriptors came back but did not make sense together.
    #[error("the server's descriptors could not be read: {0}")]
    BadDescriptors(String),

    /// No such service or method on this server.
    #[error("{0}")]
    NoSuchMethod(String),

    /// The request body was not valid JSON for the method's type.
    #[error("the request does not match {message}: {detail}")]
    BadRequest {
        /// The protobuf type the body had to match.
        message: String,
        /// What was wrong with it.
        detail: String,
    },

    /// The call was made and the server refused it.
    ///
    /// Carries gRPC's own status, which is the thing somebody
    /// testing a service actually wants to see.
    #[error("{code}: {message}")]
    Status {
        /// The `grpc-status` name, such as `NotFound`.
        code: String,
        /// The `grpc-message`.
        message: String,
    },

    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// A gRPC result.
pub type GrpcResult<T> = Result<T, GrpcError>;

impl From<tonic::Status> for GrpcError {
    fn from(status: tonic::Status) -> Self {
        Self::Status {
            code: format!("{:?}", status.code()),
            message: status.message().to_string(),
        }
    }
}
