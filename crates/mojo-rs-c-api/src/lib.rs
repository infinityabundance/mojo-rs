//! # mojo-rs-c-api
//!
//! Export of the compatible public Mojo C System ABI (epoch 1). No panic may
//! cross the ABI boundary; every export is wrapped in panic containment.
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

pub mod containment;
pub mod symbols;
pub mod types;
pub mod versioning;
