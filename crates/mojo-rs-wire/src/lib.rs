//! # mojo-rs-wire
//!
//! Wire-format encoding, decoding, and validation for Chromium Mojo messages
//! (compatibility epoch 1: Chromium `151.0.7922.105`, CoreIpcz architecture).
//!
//! Ground truth for all layouts is `atlas/reference/wire/*` (pinned headers).
//! This crate is dependency-free by design: every check is explicit, checked
//! arithmetic only, no panics on malformed input, no unbounded allocation
//! from attacker-controlled lengths.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod error;
pub mod layout;
pub mod message;
pub mod pack;
pub mod pointer;
pub mod validate;
pub mod value;
