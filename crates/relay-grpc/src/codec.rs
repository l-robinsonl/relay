//! Encoding messages Relay has no Rust type for.
//!
//! Every other gRPC client in Rust compiles a `.proto` into structs
//! before it runs. Relay cannot: the service belongs to whoever is
//! being tested, and is only described at the moment somebody asks
//! for it. So the message type is a runtime value — a
//! `MessageDescriptor` — and the message itself is a
//! `DynamicMessage`, which this teaches tonic to put on the wire.

use prost::Message;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

/// A codec that decodes into a type discovered at runtime.
#[derive(Debug, Clone)]
pub struct DynamicCodec {
    response: MessageDescriptor,
}

impl DynamicCodec {
    /// A codec for one method's response type.
    ///
    /// Only the response: the request is already a `DynamicMessage`
    /// by the time it gets here and knows its own shape, while a
    /// response arrives as bytes that mean nothing without one.
    pub fn new(response: MessageDescriptor) -> Self {
        Self { response }
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder(self.response.clone())
    }
}

/// Writes a dynamic message into tonic's buffer.
///
/// Carries no descriptor: a `DynamicMessage` already holds the one it
/// was built from, and a second copy here could only ever disagree
/// with it.
#[derive(Debug)]
pub struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        item.encode(buf)
            .map_err(|e| tonic::Status::internal(format!("could not encode the request: {e}")))
    }
}

/// Reads a dynamic message out of tonic's buffer.
#[derive(Debug)]
pub struct DynamicDecoder(MessageDescriptor);

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        // An empty frame is a legitimate message of a type with no
        // set fields, not an absent one, so this never yields `None`
        // for a frame tonic handed over.
        DynamicMessage::decode(self.0.clone(), buf)
            .map(Some)
            .map_err(|e| tonic::Status::internal(format!("could not read the response: {e}")))
    }
}
