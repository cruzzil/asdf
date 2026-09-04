//! The C ABI for libasdf, implemented in Rust.
//!
//! This crate is a thin projection of [`asdf_core`] onto libasdf's exported
//! C interface. It holds no behaviour of its own: everything here converts
//! between C representations and the engine's, and every entry point is
//! wrapped so that a panic can never unwind into a C caller.
//!
//! # What lives here versus in C
//!
//! Almost all of the ABI is expressed directly in Rust. Two things cannot be,
//! and live in `shim.c` instead:
//!
//! - the three variadic entry points (`asdf_file_log`,
//!   `asdf_file_error_common`, `asdf_value_error_common`), because defining
//!   variadic `extern "C"` functions needs unstable Rust; and
//! - `asdf_ndarray_read_float16_at`, because `_Float16` is unstable and its
//!   return ABI differs from `uint16_t`'s.
//!
//! # Headers
//!
//! The public headers are vendored verbatim from upstream rather than
//! generated, because several API entry points exist only as `_Generic`
//! macros or `static inline` functions. See `include/PROVENANCE.md`.

#![allow(non_camel_case_types)]

pub mod block_ffi;
pub mod core_ext;
pub mod error_ffi;
pub mod extension_ffi;
/// Internal only: the raw-pointer helpers every entry point is built on.
pub(crate) mod ffi;
pub mod file_ffi;
pub mod ndarray_ffi;
pub mod panic;
pub mod parser_ffi;
pub mod time_ffi;
pub mod types;
pub mod value_ffi;
pub mod version_ffi;

pub use error_ffi::LogLevel;
pub use file_ffi::{AsdfFile, AsdfValue};
pub use types::{AsdfValueErr, AsdfValueType, asdf_config_t};
pub use version_ffi::asdf_version_t;

/// The library's own version, exported as `libasdf_version`.
///
/// Reported in the `asdf_library` metadata of files this library writes.
pub const LIBASDF_RS_VERSION: &str = env!("CARGO_PKG_VERSION");
