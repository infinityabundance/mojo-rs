//! # mojo-rs-oracle
//!
//! Oracle-side harness tooling. This crate contains no candidate runtime code
//! and must never depend on mojo-rs-core/wire/system/platform/c-api: the
//! oracle stays physically and logically separate.
#![deny(missing_docs)]

pub mod driver;
pub mod protocol;
