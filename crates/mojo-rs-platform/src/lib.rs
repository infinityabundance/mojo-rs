//! # mojo-rs-platform
//!
//! Platform layer for mojo-rs. Linux first; interfaces never leak Unix
//! semantics into the core. Unsafe code is confined to `sys` modules with
//! documented `SAFETY:` invariants, wrapped by safe ownership types.
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

#[cfg(unix)]
pub mod fd;
#[cfg(unix)]
pub mod shm;
#[cfg(unix)]
pub mod socket;
#[cfg(unix)]
pub mod sys;
pub mod transport;
