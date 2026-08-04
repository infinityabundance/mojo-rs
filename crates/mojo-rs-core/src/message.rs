//! The core message model.
//!
//! A message is an owned payload plus owned attached handles. Handle transfer
//! semantics: writing a message MOVES the attached handles into the message;
//! reading moves them out. At this layer the payload is an opaque byte buffer
//! (the C API contract); wire-format interpretation is the bindings layer's
//! job (mojo-rs-wire).

use crate::handle::Handle;

/// A message in transit on a message pipe.
#[derive(Debug)]
pub struct Message {
    data: Vec<u8>,
    handles: Vec<Handle>,
}

impl Message {
    /// Create a message from payload bytes and attached handles.
    pub fn new(data: Vec<u8>, handles: Vec<Handle>) -> Message {
        Message { data, handles }
    }

    /// An empty message with no handles.
    pub fn empty() -> Message {
        Message::new(Vec::new(), Vec::new())
    }

    /// The payload bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Consume the message, returning its payload.
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// The attached handles (borrowed).
    pub fn handles(&self) -> &[Handle] {
        &self.handles
    }

    /// Consume the message, returning its attached handles.
    pub fn into_handles(self) -> Vec<Handle> {
        self.handles
    }

    /// Consume the message entirely.
    pub fn into_parts(self) -> (Vec<u8>, Vec<Handle>) {
        (self.data, self.handles)
    }
}

/// A message payload plus the raw bytes for C-API reads.
#[derive(Debug, Default)]
pub struct MessageBody {
    /// Bytes copied out (for `MojoReadMessage` with `MOJO_READ_MESSAGE_FLAG_MAY_DISCARD`,
    /// this is the full payload; otherwise at most `max_num_bytes`).
    pub data: Vec<u8>,
    /// Extracted handles.
    pub handles: Vec<Handle>,
    /// Whether the full message was consumed (payload larger than
    /// `max_num_bytes` leaves the message queued).
    pub consumed: bool,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn message_roundtrip() {
        let m = Message::new(vec![1, 2, 3], vec![]);
        assert_eq!(m.data(), &[1, 2, 3]);
        let (data, handles) = m.into_parts();
        assert_eq!(data, vec![1, 2, 3]);
        assert!(handles.is_empty());
    }
}
