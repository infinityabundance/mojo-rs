//! # mojo-rs-casefile
//!
//! Casefile model, replay protocol, harness events, and differential
//! comparison for the forensic courts. This crate MUST NOT depend on the
//! candidate runtime; it is shared by the oracle side and the courts.
#![deny(missing_docs)]

pub mod casefile;
pub mod compare;
pub mod events;
pub mod normalizers;

pub use casefile::{Casefile, Operation, Process, ProcessName};
pub use events::Event;
