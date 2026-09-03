//! The ASDF engine: file layout, binary blocks, and the tree.
//!
//! This crate holds all the behaviour. The C ABI shim (`libasdf-rs`) and the
//! idiomatic Rust API (`asdf`) are both thin projections of it, so the two
//! public faces cannot drift apart in semantics. Nothing here knows about C.

// The engine is safe Rust with exactly one exception: memory-mapping a file
// for reading, which no safe API can express. `deny` rather than `forbid` so
// that one use can be allowed explicitly and reviewed on sight; every
// `#[allow(unsafe_code)]` in this crate must carry a justification.
#![deny(unsafe_code)]

pub mod block;
pub mod compression;
pub mod core;
pub mod error;
pub mod info;
pub mod layout;
pub mod reader;
pub mod version;

/// The ASDF YAML layer: document model, parser and emitter.
///
/// Re-exported so that crates layered on the engine -- the C ABI shim and the
/// idiomatic API -- have a single dependency edge rather than needing to
/// track `asdf-yaml`'s version themselves.
pub use asdf_yaml as yaml;

pub use error::{Error, ErrorCode, Result};
pub use layout::{Layout, scan};
pub use reader::{ChecksumStatus, Reader};
pub use version::Version;
