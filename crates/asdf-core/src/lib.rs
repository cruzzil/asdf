//! The ASDF engine: file layout, binary blocks, and the tree.
//!
//! This crate holds all the behaviour. The C ABI shim (`libasdf-rs`) and the
//! idiomatic Rust API (`asdf`) are both thin projections of it, so the two
//! public faces cannot drift apart in semantics. Nothing here knows about C.

#![forbid(unsafe_code)]

pub mod block;
pub mod error;
pub mod layout;
pub mod version;

pub use error::{Error, ErrorCode, Result};
pub use layout::{Layout, scan};
pub use version::Version;
