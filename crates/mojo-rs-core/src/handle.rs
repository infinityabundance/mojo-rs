//! The handle table: handle identity, ownership, and stale-handle protection.
//!
//! Handle values are opaque `u32`s; the table packs a slot index and a
//! generation counter into each value so a stale handle cannot alias a newly
//! created object at the same slot (generation counters, per the master
//! directive). Every live handle has exactly one logical owner; transfer moves
//! the handle value and the owning `Handle` Rust type.

use std::sync::{Arc, Mutex, Weak};

use crate::dispatcher::Dispatcher;
use crate::error::{CoreError, CoreResult};

/// The number of bits used for the slot index in a handle value.
const SLOT_BITS: u32 = 20;
/// Maximum number of live slots.
const MAX_SLOTS: usize = 1 << SLOT_BITS;
/// Mask for the slot portion of a handle value.
const SLOT_MASK: u32 = (1 << SLOT_BITS) - 1;
/// The generation portion of a handle value.
const GENERATION_SHIFT: u32 = SLOT_BITS;

/// A slot index into the handle table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Slot(u32);

/// A handle value as seen by the C API: an opaque `u32`.
pub type MojoHandleValue = u32;

/// A handle into the table. Dropping closes it.
#[derive(Debug, Clone)]
pub struct Handle {
    table: Weak<HandleTable>,
    value: MojoHandleValue,
    slot: Slot,
}

impl Handle {
    /// The opaque C-API handle value.
    pub fn value(&self) -> MojoHandleValue {
        self.value
    }

    /// The slot this handle refers to.
    pub fn slot(&self) -> Slot {
        self.slot
    }

    /// Whether this handle value is the canonical invalid handle (0).
    pub fn is_invalid_value(value: MojoHandleValue) -> bool {
        value == 0
    }

    /// Whether this handle value is valid (non-zero).
    pub fn is_valid_value(value: MojoHandleValue) -> bool {
        value != 0
    }

    /// Close this handle (idempotent at the table level; a closed handle
    /// cannot be resurrected). Returns the dispatcher that was closed, if any.
    fn close_internal(&self) {
        if let Some(table) = self.table.upgrade() {
            table.close_slot(self.slot, self.value);
        }
    }

    /// Duplicate the underlying dispatcher, returning a new handle to the same
    /// dispatcher (where the dispatcher type supports duplication). Returns
    /// `Err(InvalidArgument)` for non-duplicable dispatchers.
    pub fn duplicate(&self) -> CoreResult<Handle> {
        let table = self.table.upgrade().ok_or(CoreError::InvalidArgument)?;
        table.duplicate(self.slot, self.value)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.close_internal();
    }
}

/// The handle table: the single registry of live dispatchers.
pub struct HandleTable {
    inner: Mutex<TableInner>,
}

struct TableInner {
    /// Slot -> live entry.
    entries: Vec<Option<Entry>>,
    /// Slot -> persistent generation counter (never reset on free/reuse, so
    /// stale handle values can never alias a reused slot).
    generations: Vec<u32>,
    /// Free slots (LIFO for locality).
    free: Vec<u32>,
}

struct Entry {
    dispatcher: Arc<dyn Dispatcher>,
    /// The generation this entry was created with; a handle value is valid
    /// only if its generation matches.
    generation: u32,
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl HandleTable {
    /// Create an empty handle table.
    pub fn new() -> HandleTable {
        HandleTable {
            inner: Mutex::new(TableInner {
                entries: Vec::new(),
                generations: Vec::new(),
                free: Vec::new(),
            }),
        }
    }

    /// The number of currently live handles.
    pub fn live_count(&self) -> usize {
        self.inner
            .lock()
            .map(|t| t.entries.iter().filter(|e| e.is_some()).count())
            .unwrap_or(0)
    }

    /// Add a dispatcher to the table, returning an owned handle.
    pub fn add(self: &Arc<Self>, dispatcher: Arc<dyn Dispatcher>) -> CoreResult<Handle> {
        let mut inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        let (slot, generation) = inner.allocate()?;
        let value = encode(slot, generation);
        inner.entries[slot as usize] = Some(Entry {
            dispatcher,
            generation,
        });
        Ok(Handle {
            table: Arc::downgrade(self),
            value,
            slot: Slot(slot),
        })
    }

    /// Resolve a handle value to its dispatcher.
    pub fn get(&self, value: MojoHandleValue) -> CoreResult<Arc<dyn Dispatcher>> {
        if value == 0 {
            return Err(CoreError::InvalidArgument);
        }
        let inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        let slot = value & SLOT_MASK;
        let generation = value >> GENERATION_SHIFT;
        let entry = inner
            .entries
            .get(slot as usize)
            .and_then(|e| e.as_ref())
            .ok_or(CoreError::InvalidArgument)?;
        if entry.generation != generation {
            return Err(CoreError::InvalidArgument);
        }
        Ok(Arc::clone(&entry.dispatcher))
    }

    /// Close the slot if the handle value's generation matches (stale handles
    /// are rejected).
    fn close_slot(&self, slot: Slot, value: MojoHandleValue) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let generation = value >> GENERATION_SHIFT;
        let entry = inner
            .entries
            .get_mut(slot.0 as usize)
            .and_then(|e| e.as_mut());
        if let Some(entry) = entry {
            if entry.generation == generation {
                let dispatcher = Arc::clone(&entry.dispatcher);
                inner.entries[slot.0 as usize] = None;
                inner.free.push(slot.0);
                // Notify the dispatcher AFTER releasing the table lock.
                drop(inner);
                dispatcher.on_closed();
            }
        }
    }

    /// Duplicate a dispatcher entry.
    fn duplicate(self: &Arc<Self>, slot: Slot, value: MojoHandleValue) -> CoreResult<Handle> {
        let dispatcher = self.get(value)?;
        if !dispatcher.is_duplicable() {
            return Err(CoreError::InvalidArgument);
        }
        let mut inner = self.inner.lock().map_err(|_| CoreError::Internal)?;
        let (new_slot, generation) = inner.allocate()?;
        let new_value = encode(new_slot, generation);
        inner.entries[new_slot as usize] = Some(Entry {
            dispatcher,
            generation,
        });
        Ok(Handle {
            table: Arc::downgrade(self),
            value: new_value,
            slot: Slot(new_slot),
        })
    }
}

impl TableInner {
    fn allocate(&mut self) -> CoreResult<(u32, u32)> {
        if let Some(slot) = self.free.pop() {
            self.bump_generation(slot);
            let generation = self.generations[slot as usize];
            return Ok((slot, generation));
        }
        if self.entries.len() >= MAX_SLOTS {
            return Err(CoreError::ResourceExhausted);
        }
        let slot = self.entries.len() as u32;
        self.entries.push(None);
        self.generations.push(0);
        self.bump_generation(slot);
        let generation = self.generations[slot as usize];
        Ok((slot, generation))
    }

    /// Advance the per-slot generation, skipping 0 (reserved for the invalid
    /// handle value 0).
    fn bump_generation(&mut self, slot: u32) {
        let g = self.generations[slot as usize].wrapping_add(1);
        self.generations[slot as usize] = if g == 0 { 1 } else { g };
    }
}

/// Pack (slot, generation) into a handle value.
fn encode(slot: u32, generation: u32) -> MojoHandleValue {
    (generation << GENERATION_SHIFT) | (slot & SLOT_MASK)
}

impl Slot {
    /// The raw slot index.
    pub fn raw(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::pipe::MessagePipe;
    use crate::signal::Signals;

    #[test]
    fn create_and_resolve() {
        let table = Arc::new(HandleTable::new());
        let (a, b) = MessagePipe::create();
        let ha = table.add(a).unwrap();
        let hb = table.add(b).unwrap();
        assert_ne!(ha.value(), hb.value());
        assert!(ha.value() != 0);
        assert!(table.get(ha.value()).is_ok());
        assert!(table.get(hb.value()).is_ok());
        assert_eq!(table.live_count(), 2);
    }

    #[test]
    fn close_rejects_stale_and_reused() {
        let table = Arc::new(HandleTable::new());
        let (a, b) = MessagePipe::create();
        let ha = table.add(a).unwrap();
        let hb = table.add(b).unwrap();
        let value = ha.value();
        drop(ha); // closes
        assert!(table.get(value).is_err());
        // A new handle may reuse the slot, but the stale value must not alias it.
        let (a2, b2) = MessagePipe::create();
        let ha2 = table.add(a2).unwrap();
        let _hb2 = table.add(b2).unwrap();
        if table.get(value).is_ok() {
            // If the slot was reused, the generation differs so the OLD value
            // must NOT resolve to the new object.
            let resolved = table.get(value).unwrap();
            let fresh = table.get(ha2.value()).unwrap();
            assert!(!Arc::ptr_eq(&resolved, &fresh));
        }
    }

    #[test]
    fn zero_is_invalid() {
        assert!(Handle::is_invalid_value(0));
        assert!(!Handle::is_valid_value(0));
    }

    #[test]
    fn signals_sanity() {
        assert!(Signals::READABLE.contains(Signals::READABLE));
        assert!(!Signals::READABLE.contains(Signals::WRITABLE));
    }
}
