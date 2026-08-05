//! RingBuffer: a circular byte buffer over a shared-memory mapping.
//!
//! Mirrors the pinned epoch's `mojo/core/ipcz_driver/ring_buffer.{h,cc}`
//! (Chromium 151.0.7922.105, CoreIpcz architecture) operation-for-operation:
//! `Range` offsets are circular within the physical buffer; a range is mapped
//! to one or two contiguous spans (`MapRange`); writes extend the data range
//! from its end while reads/discards advance its offset. The buffer is not
//! internally synchronized — the owning `DataPipe` pair serializes all access
//! (the official implementation guards each endpoint with its own lock; the
//! in-process court observes identical behavior through the pair lock, and the
//! cross-process path (Phase 5) replaces the pair lock with shared-memory
//! ownership plus portal control messages).
//!
//! All sizes are `usize`; every offset arithmetic is checked against
//! `capacity` before slicing the mapping.

use std::ops::DerefMut;

use mojo_rs_platform::shm::Mapping;

/// A circular range of bytes within the physical buffer.
///
/// `offset` is the buffer offset of the first byte; `size` is the number of
/// bytes (no larger than the buffer size). `offset + size` may exceed
/// `capacity`, in which case the range wraps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// The buffer offset of the first byte in the range.
    pub offset: usize,
    /// The size of the range in bytes.
    pub size: usize,
}

impl Range {
    /// A range at offset 0 of size 0.
    pub fn empty() -> Range {
        Range { offset: 0, size: 0 }
    }

    /// The offset just past the end of the range, wrapped into `capacity`.
    pub fn end(&self, capacity: usize) -> usize {
        let end = self.offset + self.size;
        if end >= capacity { end - capacity } else { end }
    }
}

/// A captured contiguous span of the ring buffer for two-phase operations.
///
/// The span is only valid while the owning two-phase operation is active (the
/// mapping outlives the pipe and is never resized or unmapped during its
/// lifetime, so the pointer remains valid). The `len` bounds every commit.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: a Span is only ever constructed from a live mapping owned by a
// RingBuffer that outlives the span (begin..end of a two-phase operation), and
// the mapping is never resized or unmapped during that window. Sending the
// span across threads is safe because the pointer targets process-shared
// memory; the owning pipe serializes mutation with its lock.
unsafe impl Send for Span {}
// SAFETY: shared access to the span's bytes (`as_slice`) never mutates the
// underlying memory; mutation requires `&mut Span` through `as_mut_slice`,
// whose callers must uphold no-aliasing (the owning pipe serializes two-phase
// operations with its lock).
unsafe impl Sync for Span {}

impl Span {
    fn new(ptr: *mut u8, len: usize) -> Span {
        Span { ptr, len }
    }

    /// The number of bytes in the span.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the span is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The start address of the span (for the harness, which writes content
    /// into a two-phase write buffer or reads a two-phase read buffer).
    ///
    /// SAFETY: the address is valid for `len()` bytes while the two-phase
    /// operation is active.
    pub fn address(&self) -> usize {
        self.ptr as usize
    }

    /// A shared slice view of the span.
    ///
    /// SAFETY: the span is live for `len` bytes (see the type invariant).
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the span is live and owned for `len` bytes.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// A mutable slice view of the span.
    ///
    /// SAFETY: the span is live for `len` bytes; the caller must not create
    /// any other mutable (or, while the `&mut [u8]` is alive, shared) access
    /// to the same bytes. The span type is `Copy`, so aliasing the same span
    /// into two mutable slices is the caller's responsibility to avoid.
    pub unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the span is live and owned for `len` bytes; the caller has
        // upheld the no-aliasing precondition.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

/// A DirectWriter exposes the first contiguous span of available capacity
/// within a RingBuffer for direct writing (official `RingBuffer::DirectWriter`).
#[derive(Debug, Clone, Copy)]
pub struct DirectWriter {
    span: Span,
}

impl DirectWriter {
    fn new(span: Span) -> DirectWriter {
        DirectWriter { span }
    }

    /// The span of bytes available for writing.
    pub fn span(&self) -> Span {
        self.span
    }

    /// Commits the first `n` bytes of the span into the buffer and consumes
    /// the writer. Returns false if `n` exceeds the span size.
    pub fn commit(self, n: usize, ring: &mut RingBuffer) -> bool {
        n <= self.span.len && ring.extend_data_range(n)
    }
}

/// A DirectReader exposes the first contiguous span of data within a
/// RingBuffer for direct reading (official `RingBuffer::DirectReader`).
#[derive(Debug, Clone, Copy)]
pub struct DirectReader {
    span: Span,
}

impl DirectReader {
    fn new(span: Span) -> DirectReader {
        DirectReader { span }
    }

    /// The span of bytes available for reading.
    pub fn span(&self) -> Span {
        self.span
    }

    /// Consumes `n` bytes from the front of the span. Returns false if `n`
    /// exceeds the span size.
    pub fn consume(self, n: usize, ring: &mut RingBuffer) -> bool {
        n <= self.span.len && ring.discard_all(n)
    }
}

/// The serialized state of a RingBuffer (official
/// `RingBuffer::SerializedState`, 8 bytes on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SerializedState {
    /// The data range offset.
    pub offset: u32,
    /// The data range size.
    pub size: u32,
}

/// A circular data buffer over a shared-memory mapping.
pub struct RingBuffer {
    mapping: Mapping,
    data_range: Range,
}

impl RingBuffer {
    /// Construct a new empty RingBuffer over `mapping` (must cover the whole
    /// region used by the pipe; the capacity is the mapping size).
    pub fn new(mapping: Mapping) -> RingBuffer {
        RingBuffer {
            mapping,
            data_range: Range::empty(),
        }
    }

    /// The total capacity of this RingBuffer in bytes.
    pub fn capacity(&self) -> usize {
        self.mapping.len()
    }

    /// The number of bytes of data currently in the RingBuffer.
    pub fn data_size(&self) -> usize {
        self.data_range.size
    }

    /// The number of bytes of available capacity in the RingBuffer.
    pub fn available_capacity(&self) -> usize {
        self.capacity() - self.data_size()
    }

    /// The current data range.
    pub fn data_range(&self) -> Range {
        self.data_range
    }

    /// Attempts to append `source` to the buffer. Returns the number of bytes
    /// written, which may be less than the length of `source` if there was
    /// insufficient capacity available.
    pub fn write(&mut self, source: &[u8]) -> usize {
        let (first_len, second_len) = self.free_chunks(source.len());
        let written = first_len + second_len;
        let cap = self.capacity();
        let free_offset = self.data_range.end(cap);
        let bytes = self.mapping.deref_mut();
        bytes[free_offset..free_offset + first_len].copy_from_slice(&source[..first_len]);
        if second_len > 0 {
            bytes[..second_len].copy_from_slice(&source[first_len..first_len + second_len]);
        }
        let ok = self.extend_data_range(written);
        debug_assert!(ok);
        written
    }

    /// Like `write` but only writes data if there's enough room for all of
    /// `source`. Returns true if the write happened and false otherwise.
    pub fn write_all(&mut self, source: &[u8]) -> bool {
        if source.len() > self.available_capacity() {
            return false;
        }
        let n = self.write(source);
        debug_assert_eq!(n, source.len());
        true
    }

    /// Attempts to copy bytes from the front of the buffer and into `target`,
    /// discarding them from the buffer. May consume less than the length of
    /// `target` if the buffer doesn't have enough data to read.
    pub fn read(&mut self, target: &mut [u8]) -> usize {
        let n = self.peek(target);
        let ok = self.discard_all(n);
        debug_assert!(ok);
        n
    }

    /// Like `read` but only reads data if there's enough to fill `target`.
    pub fn read_all(&mut self, target: &mut [u8]) -> bool {
        if target.len() > self.data_size() {
            return false;
        }
        let n = self.read(target);
        debug_assert_eq!(n, target.len());
        true
    }

    /// Same semantics as `read` but no data is discarded from the buffer.
    pub fn peek(&self, target: &mut [u8]) -> usize {
        let (first_len, second_len) = self.data_chunks(target.len());
        let offset = self.data_range.offset;
        let bytes = &self.mapping[..];
        target[..first_len].copy_from_slice(&bytes[offset..offset + first_len]);
        if second_len > 0 {
            target[first_len..first_len + second_len].copy_from_slice(&bytes[..second_len]);
        }
        first_len + second_len
    }

    /// Like `peek` but only reads data if there's enough to fill `target`.
    pub fn peek_all(&self, target: &mut [u8]) -> bool {
        if target.len() > self.data_size() {
            return false;
        }
        let n = self.peek(target);
        debug_assert_eq!(n, target.len());
        true
    }

    /// Attempts to discard `n` bytes from the front of the buffer. Returns the
    /// number of bytes discarded, which may be smaller than `n`.
    pub fn discard(&mut self, n: usize) -> usize {
        let num_bytes = std::cmp::min(n, self.data_size());
        let ok = self.discard_all(num_bytes);
        debug_assert!(ok);
        num_bytes
    }

    /// Like `discard` but only discards data if there are `n` bytes to
    /// discard.
    pub fn discard_all(&mut self, n: usize) -> bool {
        if n > self.data_size() {
            return false;
        }
        let Some(new_offset) = self.data_range.offset.checked_add(n) else {
            return false;
        };
        let cap = self.capacity();
        self.data_range = Range {
            offset: new_offset % cap,
            size: self.data_size() - n,
        };
        true
    }

    /// Attempts to extend the range of readable data by `n` bytes, implying
    /// that the data has already been populated within the buffer immediately
    /// following any currently readable data. Returns false if `n` exceeds the
    /// available capacity.
    pub fn extend_data_range(&mut self, n: usize) -> bool {
        if n > self.available_capacity() {
            return false;
        }
        self.data_range.size += n;
        true
    }

    /// The first contiguous span of available capacity, starting from the
    /// first byte of available capacity (official
    /// `GetAvailableCapacityView`). Used to seed a two-phase write.
    pub fn available_capacity_view(&mut self) -> Span {
        let free = self.complement_range(self.data_range);
        let cap = self.capacity();
        let first_chunk_size = std::cmp::min(free.size, cap - free.offset);
        let base = self.mapping.address();
        // SAFETY: base + free.offset is within the mapping (free.offset < cap
        // by construction), and the mapping is owned by this RingBuffer for
        // its whole capacity; the span is captured for the two-phase window.
        Span::new((base + free.offset) as *mut u8, first_chunk_size)
    }

    /// The first contiguous span of readable data (official
    /// `GetReadableDataView`). Used to seed a two-phase read.
    pub fn readable_data_view(&self) -> Span {
        let cap = self.capacity();
        let first_chunk_size = std::cmp::min(self.data_range.size, cap - self.data_range.offset);
        let base = self.mapping.address();
        // SAFETY: base + data_range.offset is within the mapping
        // (data_range.offset < cap by construction); the mapping is owned by
        // this RingBuffer for its whole capacity.
        Span::new((base + self.data_range.offset) as *mut u8, first_chunk_size)
    }

    /// A two-phase writer over the first contiguous available-capacity span.
    pub fn begin_write(&mut self) -> DirectWriter {
        DirectWriter::new(self.available_capacity_view())
    }

    /// A two-phase reader over the first contiguous readable-data span.
    pub fn begin_read(&self) -> DirectReader {
        DirectReader::new(self.readable_data_view())
    }

    /// The serialized state of the ring (offset + size as u32).
    pub fn serialize(&self) -> SerializedState {
        SerializedState {
            offset: self.data_range.offset as u32,
            size: self.data_range.size as u32,
        }
    }

    /// Restore state from a serialized state. Returns false if the state is
    /// invalid for this capacity.
    pub fn deserialize(&mut self, state: SerializedState) -> bool {
        let data_offset = state.offset as usize;
        let data_size = state.size as usize;
        if data_offset >= self.capacity() || data_size > self.capacity() {
            return false;
        }
        self.data_range = Range {
            offset: data_offset,
            size: data_size,
        };
        true
    }

    /// The sizes of the two chunks of the free region (the complement of the
    /// data range) for a write of `len` bytes: `(first_chunk, second_chunk)`
    /// capped at `len`. The first chunk is capped by both the free range size
    /// and the distance to the physical end of the buffer (official
    /// `MapRange`); the second chunk is the remainder, so no arithmetic can
    /// underflow.
    fn free_chunks(&self, len: usize) -> (usize, usize) {
        let free = self.complement_range(self.data_range);
        let cap = self.capacity();
        let first_chunk_size = std::cmp::min(len, std::cmp::min(free.size, cap - free.offset));
        let second_chunk_size = std::cmp::min(len - first_chunk_size, free.size - first_chunk_size);
        (first_chunk_size, second_chunk_size)
    }

    /// The sizes of the two chunks of the data region for a read of `len`
    /// bytes, capped at `len` (official `MapRange` semantics).
    fn data_chunks(&self, len: usize) -> (usize, usize) {
        let cap = self.capacity();
        let first_chunk_size = std::cmp::min(
            len,
            std::cmp::min(self.data_range.size, cap - self.data_range.offset),
        );
        let second_chunk_size = std::cmp::min(
            len - first_chunk_size,
            self.data_range.size - first_chunk_size,
        );
        (first_chunk_size, second_chunk_size)
    }

    /// The complement of `range` within the underlying buffer: the range which
    /// includes exactly all bytes NOT in `range`.
    fn complement_range(&self, range: Range) -> Range {
        let cap = self.capacity();
        debug_assert!(range.offset <= cap);
        debug_assert!(range.size <= cap);
        Range {
            offset: range.end(cap),
            size: cap - range.size,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn ring(capacity: usize) -> RingBuffer {
        let mem = mojo_rs_platform::shm::SharedMemory::create("mojo-ring-test", capacity).unwrap();
        let m = mem
            .map(0, capacity, mojo_rs_platform::shm::Access::ReadWrite)
            .unwrap();
        RingBuffer::new(m)
    }

    #[test]
    fn write_read_roundtrip() {
        let mut r = ring(16);
        assert_eq!(r.write(&[1, 2, 3]), 3);
        assert_eq!(r.data_size(), 3);
        let mut out = [0u8; 3];
        assert_eq!(r.read(&mut out), 3);
        assert_eq!(out, [1, 2, 3]);
        assert_eq!(r.data_size(), 0);
    }

    #[test]
    fn wrap_around_offsets() {
        let mut r = ring(8);
        // Fill the buffer fully.
        assert!(r.write_all(&[0, 1, 2, 3, 4, 5, 6, 7]));
        // Read the front half: data range (0,8) -> (4,4).
        let mut out = [0u8; 4];
        assert_eq!(r.read(&mut out), 4);
        assert_eq!(out, [0, 1, 2, 3]);
        assert_eq!(r.data_range(), Range { offset: 4, size: 4 });
        // Write 4 more: they land at [4..8) after the existing data at
        // [4..8)?? No: existing data is at [4..8); the free region is
        // [0..4), which is the complement and does not wrap.
        assert!(r.write_all(&[8, 9, 10, 11]));
        assert_eq!(r.data_range(), Range { offset: 4, size: 8 });
        // Now the data range wraps: bytes at [4..8) and [0..4).
        let mut all = [0u8; 8];
        assert_eq!(r.read(&mut all), 8);
        assert_eq!(all, [4, 5, 6, 7, 8, 9, 10, 11]);
    }

    #[test]
    fn peek_does_not_consume() {
        let mut r = ring(16);
        r.write_all(&[9, 8, 7]);
        let mut out = [0u8; 3];
        assert_eq!(r.peek(&mut out), 3);
        assert_eq!(out, [9, 8, 7]);
        assert_eq!(r.data_size(), 3);
        assert_eq!(r.read(&mut out), 3);
        assert_eq!(r.data_size(), 0);
    }

    #[test]
    fn all_or_none_semantics() {
        let mut r = ring(4);
        assert!(!r.write_all(&[1, 2, 3, 4, 5])); // too big
        assert_eq!(r.data_size(), 0);
        assert!(r.write_all(&[1, 2, 3, 4]));
        assert!(!r.write_all(&[5])); // full
        assert_eq!(r.data_size(), 4);
        let mut out = [0u8; 5];
        assert!(!r.read_all(&mut out)); // not enough data
        assert_eq!(r.data_size(), 4);
        let mut out4 = [0u8; 4];
        assert!(r.read_all(&mut out4));
        assert_eq!(out4, [1, 2, 3, 4]);
    }

    #[test]
    fn discard_semantics() {
        let mut r = ring(16);
        r.write_all(&[1, 2, 3, 4, 5]);
        assert_eq!(r.discard(2), 2);
        assert_eq!(r.data_size(), 3);
        assert!(!r.discard_all(4)); // too many
        assert_eq!(r.data_size(), 3);
        assert!(r.discard_all(3));
        assert_eq!(r.data_size(), 0);
    }

    #[test]
    fn two_phase_write_and_read() {
        let mut r = ring(16);
        let w = r.begin_write();
        assert_eq!(w.span().len(), 16);
        // Write through the raw span (only the first 4 bytes).
        let mut span = w.span();
        // SAFETY: the span is the writer's captured available-capacity view;
        // it is live for the two-phase window and this is the only access to
        // those bytes.
        let s = unsafe { span.as_mut_slice() };
        s[..4].copy_from_slice(&[1, 2, 3, 4]);
        assert!(w.commit(4, &mut r));
        assert_eq!(r.data_size(), 4);

        let rd = r.begin_read();
        assert_eq!(rd.span().len(), 4);
        assert_eq!(rd.span().as_slice(), &[1, 2, 3, 4]);
        assert!(rd.consume(4, &mut r));
        assert_eq!(r.data_size(), 0);
    }

    #[test]
    fn two_phase_commit_bounds() {
        let mut r = ring(8);
        let w = r.begin_write();
        let len = w.span().len();
        assert_eq!(len, 8);
        assert!(!w.commit(len + 1, &mut r)); // exceeds the span
        assert_eq!(r.data_size(), 0);
    }

    #[test]
    fn two_phase_writer_wraps() {
        let mut r = ring(8);
        r.write_all(&[1, 2, 3, 4, 5, 6]);
        let mut out = [0u8; 4];
        r.read(&mut out); // data range (0,6) -> (4,2)
        assert_eq!(out, [1, 2, 3, 4]);
        // Data at [4..6). Free = complement(4,2) = (6, 6) -> [6..8)+[0..4).
        // The available-capacity view is the FIRST contiguous chunk: [6..8).
        let w = r.begin_write();
        assert_eq!(w.span().len(), 2);
        let mut span = w.span();
        // SAFETY: the span is the captured available-capacity view, live for
        // the two-phase window; exclusive access is held here.
        let s = unsafe { span.as_mut_slice() };
        s.copy_from_slice(&[7, 8]);
        assert!(w.commit(2, &mut r));
        assert_eq!(r.data_range(), Range { offset: 4, size: 4 });
        // Remaining data [4..6) then [6..8) then [0..2) of the next write...
        let mut out2 = [0u8; 4];
        assert_eq!(r.read(&mut out2), 4);
        assert_eq!(out2, [5, 6, 7, 8]);
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        // The SerializedState describes the RANGE metadata; the bytes live in
        // the shared memory, so a peer endpoint (duplicated descriptor)
        // restores the same range over the same pages.
        let mem = mojo_rs_platform::shm::SharedMemory::create("mojo-ring-serde", 16).unwrap();
        let peer_mem = mem.duplicate().unwrap();
        let mut r = RingBuffer::new(
            mem.map(0, 16, mojo_rs_platform::shm::Access::ReadWrite)
                .unwrap(),
        );
        let mut r2 = RingBuffer::new(
            peer_mem
                .map(0, 16, mojo_rs_platform::shm::Access::ReadWrite)
                .unwrap(),
        );
        r.write_all(&[1, 2, 3, 4]);
        let mut out = [0u8; 2];
        r.read(&mut out);
        assert_eq!(out, [1, 2]);
        let st = r.serialize();
        assert_eq!(st, SerializedState { offset: 2, size: 2 });
        assert!(r2.deserialize(st));
        assert_eq!(r2.data_size(), 2);
        let mut out2 = [0u8; 2];
        assert_eq!(r2.read(&mut out2), 2);
        assert_eq!(out2, [3, 4]);
    }

    #[test]
    fn deserialize_rejects_invalid_state() {
        let mut r = ring(16);
        assert!(!r.deserialize(SerializedState {
            offset: 16,
            size: 0
        })); // offset == cap
        assert!(!r.deserialize(SerializedState {
            offset: 0,
            size: 17
        })); // size > cap
    }
}
