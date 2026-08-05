//! Idiomatic safe Rust data-pipe API (Phase 4).
//!
//! Wraps the core `DataPipe` endpoints with ownership-enforcing types:
//!
//! * A `DataPipeProducer`/`DataPipeConsumer` owns one endpoint handle; the
//!   peer closure is observed through the signal state.
//! * One-phase writes/reads are safe slice operations.
//! * Two-phase access is an RAII transaction: the writable/readable span is
//!   only exposed through the transaction, which commits (or, on drop,
//!   cancels with a zero-length commit) exactly once. The span cannot outlive
//!   the transaction, so a stale two-phase span can never be used after the
//!   operation ends.
//!
//! The official error codes surface through `SystemError`; the semantics
//! (element-size alignment, ALL_OR_NONE, BUSY while a two-phase operation is
//! active, peer-closed FAILED_PRECONDITION) match the core exactly.

use std::sync::Arc;

use mojo_rs_core::data_pipe::{DataPipe, DataPipeEnd, ReadFlags as CoreReadFlags};
use mojo_rs_core::ring_buffer::Span;
use mojo_rs_core::signal::SignalsState;

use crate::error::{SystemError, SystemResult};

/// Options for creating a data pipe pair (official `MojoCreateDataPipeOptions`
/// defaults when omitted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataPipeOptions {
    /// The size of each element in bytes. Must be non-zero.
    pub element_size: u32,
    /// The total byte capacity. Zero selects the system default (64 KiB).
    pub capacity: u32,
}

impl Default for DataPipeOptions {
    fn default() -> DataPipeOptions {
        DataPipeOptions {
            element_size: 1,
            capacity: 0,
        }
    }
}

/// A data pipe producer endpoint: owns the write side.
pub struct DataPipeProducer {
    pipe: Arc<DataPipe>,
}

/// A data pipe consumer endpoint: owns the read side.
pub struct DataPipeConsumer {
    pipe: Arc<DataPipe>,
}

/// A created data pipe pair.
pub struct DataPipePair {
    /// The producer endpoint.
    pub producer: DataPipeProducer,
    /// The consumer endpoint.
    pub consumer: DataPipeConsumer,
}

/// Create a data pipe pair.
pub fn create(options: &DataPipeOptions) -> SystemResult<DataPipePair> {
    let capacity = if options.capacity == 0 {
        64 * 1024
    } else {
        options.capacity
    };
    let (producer, consumer) = DataPipe::create_pair(options.element_size, capacity)?;
    Ok(DataPipePair {
        producer: DataPipeProducer { pipe: producer },
        consumer: DataPipeConsumer { pipe: consumer },
    })
}

/// Read flags for one-phase reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadFlags {
    /// Require all requested bytes (`MOJO_READ_DATA_FLAG_ALL_OR_NONE`).
    pub all_or_none: bool,
    /// Query available bytes without reading (`MOJO_READ_DATA_FLAG_QUERY`).
    pub query: bool,
    /// Discard bytes without copying (`MOJO_READ_DATA_FLAG_DISCARD`).
    pub discard: bool,
    /// Read bytes without consuming (`MOJO_READ_DATA_FLAG_PEEK`).
    pub peek: bool,
}

impl From<ReadFlags> for CoreReadFlags {
    fn from(f: ReadFlags) -> CoreReadFlags {
        CoreReadFlags {
            all_or_none: f.all_or_none,
            query: f.query,
            discard: f.discard,
            peek: f.peek,
        }
    }
}

impl DataPipeProducer {
    /// The element size of this pipe.
    pub fn element_size(&self) -> u32 {
        self.pipe.element_size()
    }

    /// The current signal state (WRITABLE/PEER_CLOSED/PEER_REMOTE).
    pub fn signals(&self) -> SignalsState {
        self.pipe.query_signals()
    }

    /// One-phase write. `all_or_none` requires that all of `data` fits;
    /// otherwise up to the available capacity is written. Returns the number
    /// of bytes written.
    pub fn write(&self, data: &[u8], all_or_none: bool) -> SystemResult<usize> {
        let num_bytes = u32::try_from(data.len()).map_err(|_| SystemError::InvalidArgument)?;
        Ok(self.pipe.write(data, num_bytes, all_or_none)? as usize)
    }

    /// Begin a two-phase write. The returned transaction exposes the writable
    /// span; dropping it without committing cancels the two-phase operation
    /// (a zero-length commit).
    pub fn begin_write(&mut self) -> SystemResult<WriteTransaction<'_>> {
        let span = self.pipe.begin_write()?;
        Ok(WriteTransaction {
            producer: self,
            span,
            committed: false,
        })
    }
}

/// An active two-phase write. The span is valid until the transaction is
/// committed (or dropped).
pub struct WriteTransaction<'a> {
    producer: &'a mut DataPipeProducer,
    span: Span,
    committed: bool,
}

impl<'a> WriteTransaction<'a> {
    /// The writable bytes of the transaction.
    ///
    /// The borrow is tied to `&mut self`, and the transaction is the only
    /// access path to the span (the pipe rejects concurrent writes with
    /// BUSY, and the peer's reads never touch the free region), so this is
    /// sound.
    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the span is live for the transaction's lifetime (the pipe
        // keeps its mapping alive); `&mut self` is the sole access path to the
        // span's bytes, so no aliasing mutable reference can exist.
        unsafe { self.span.as_mut_slice() }
    }

    /// The length of the writable span.
    pub fn len(&self) -> usize {
        self.span.len()
    }

    /// Whether the span is empty.
    pub fn is_empty(&self) -> bool {
        self.span.is_empty()
    }

    /// Commit `n` bytes (must be a multiple of the element size and no larger
    /// than the span). Consumes the transaction.
    pub fn commit(mut self, n: usize) -> SystemResult<()> {
        let produced = u32::try_from(n).map_err(|_| SystemError::InvalidArgument)?;
        self.producer.pipe.end_write(produced)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for WriteTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // Cancel the two-phase operation with a zero-length commit.
            let _ = self.producer.pipe.end_write(0);
        }
    }
}

impl DataPipeConsumer {
    /// The element size of this pipe.
    pub fn element_size(&self) -> u32 {
        self.pipe.element_size()
    }

    /// The current signal state (READABLE/NEW_DATA_READABLE/PEER_CLOSED/
    /// PEER_REMOTE).
    pub fn signals(&self) -> SignalsState {
        self.pipe.query_signals()
    }

    /// One-phase read into `buffer`. Returns the bytes read (or peeked), and
    /// the number consumed for discard/query modes.
    pub fn read(&self, buffer: &mut [u8], flags: ReadFlags) -> SystemResult<ReadOutcome> {
        let num_bytes = u32::try_from(buffer.len()).map_err(|_| SystemError::InvalidArgument)?;
        let elements = if flags.query || flags.discard {
            None
        } else {
            Some(buffer)
        };
        let outcome = self.pipe.read(elements, num_bytes, flags.into())?;
        Ok(ReadOutcome {
            num_bytes: outcome.num_bytes,
            data: outcome.data,
        })
    }

    /// Begin a two-phase read. The returned transaction exposes the readable
    /// span; dropping it without consuming cancels the two-phase operation.
    pub fn begin_read(&mut self) -> SystemResult<ReadTransaction<'_>> {
        let span = self.pipe.begin_read()?;
        Ok(ReadTransaction {
            consumer: self,
            span,
            consumed: false,
        })
    }
}

/// The outcome of a one-phase read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutcome {
    /// The number of bytes read/peeked/discarded (or the available size for a
    /// query).
    pub num_bytes: u32,
    /// The bytes read (or peeked); empty for query and discard.
    pub data: Vec<u8>,
}

/// An active two-phase read.
pub struct ReadTransaction<'a> {
    consumer: &'a mut DataPipeConsumer,
    span: Span,
    consumed: bool,
}

impl<'a> ReadTransaction<'a> {
    /// The readable bytes of the transaction (shared view).
    pub fn bytes(&self) -> &[u8] {
        self.span.as_slice()
    }

    /// The length of the readable span.
    pub fn len(&self) -> usize {
        self.span.len()
    }

    /// Whether the span is empty.
    pub fn is_empty(&self) -> bool {
        self.span.is_empty()
    }

    /// Consume `n` bytes (must be a multiple of the element size and no larger
    /// than the span). Consumes the transaction.
    pub fn consume(mut self, n: usize) -> SystemResult<()> {
        let consumed = u32::try_from(n).map_err(|_| SystemError::InvalidArgument)?;
        self.consumer.pipe.end_read(consumed)?;
        self.consumed = true;
        Ok(())
    }
}

impl Drop for ReadTransaction<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            let _ = self.consumer.pipe.end_read(0);
        }
    }
}

/// Close the producer endpoint (the consumer observes PEER_CLOSED).
pub fn close_producer(p: &DataPipeProducer) {
    p.pipe.close();
}

/// Close the consumer endpoint (the producer observes PEER_CLOSED).
pub fn close_consumer(c: &DataPipeConsumer) {
    c.pipe.close();
}

impl Drop for DataPipeProducer {
    fn drop(&mut self) {
        self.pipe.close();
    }
}

impl Drop for DataPipeConsumer {
    fn drop(&mut self) {
        self.pipe.close();
    }
}

// Ensure the endpoints are usable across threads (the core is internally
// synchronized).
fn _assert_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DataPipeProducer>();
    assert_send_sync::<DataPipeConsumer>();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn one_phase_roundtrip() {
        let pair = create(&DataPipeOptions::default()).unwrap();
        assert_eq!(pair.producer.write(&[1, 2, 3], false).unwrap(), 3);
        let mut buf = [0u8; 8];
        let out = pair
            .consumer
            .read(&mut buf[..3], ReadFlags::default())
            .unwrap();
        assert_eq!(out.data, vec![1, 2, 3]);
    }

    #[test]
    fn two_phase_raii() {
        let mut pair = create(&DataPipeOptions::default()).unwrap();
        {
            let mut tx = pair.producer.begin_write().unwrap();
            assert_eq!(tx.len(), 64 * 1024);
            tx.bytes()[..3].copy_from_slice(&[9, 8, 7]);
            tx.commit(3).unwrap();
        }
        {
            let tx = pair.consumer.begin_read().unwrap();
            assert_eq!(tx.bytes(), &[9, 8, 7]);
            tx.consume(3).unwrap();
        }
        // Dropped without commit cancels (zero-length end): no data added.
        {
            let _tx = pair.producer.begin_write().unwrap();
        }
        let st = pair.consumer.signals();
        assert!(
            !st.satisfied
                .contains(mojo_rs_core::signal::Signals::READABLE)
        );
    }

    #[test]
    fn drop_closes_endpoint() {
        let pair = create(&DataPipeOptions::default()).unwrap();
        drop(pair.producer);
        let st = pair.consumer.signals();
        assert!(
            st.satisfied
                .contains(mojo_rs_core::signal::Signals::PEER_CLOSED)
        );
    }

    #[test]
    fn endpoint_type_is_correct() {
        let pair = create(&DataPipeOptions::default()).unwrap();
        assert_eq!(pair.producer.pipe.end(), DataPipeEnd::Producer);
        assert_eq!(pair.consumer.pipe.end(), DataPipeEnd::Consumer);
    }
}
