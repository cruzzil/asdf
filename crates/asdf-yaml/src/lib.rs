//! ASDF-flavoured YAML: document model, parser and emitter.
//!
//! ASDF stores its tree as a YAML 1.1 document with three requirements that
//! general-purpose Rust YAML crates do not currently meet together:
//!
//! - **Tags are the type system.** `!core/ndarray-1.1.0` is what makes a
//!   mapping an array, so tags must survive parsing and be re-emitted.
//! - **Anchors and aliases are load-bearing.** Files in the standard's own
//!   reference corpus share nodes through aliases, and a round trip must not
//!   silently duplicate or drop them.
//! - **Scalar style carries meaning.** A quoted `"123"` is a string where an
//!   unquoted `123` is an integer.
//!
//! This crate builds on `saphyr-parser`'s event stream, which surfaces tags,
//! anchor ids and styles, and adds the document model and emitter on top.

#![forbid(unsafe_code)]

pub mod document;
pub mod node;
pub mod parse;
pub mod scalar;
pub mod tag;

pub use document::{Document, YamlVersion};
pub use node::{CollectionStyle, Entry, Node, NodeData, NodeId, ScalarStyle, Span};
pub use parse::{ParseError, parse_document};
pub use scalar::{Resolved, Schema, ValueType};
pub use tag::{ASDF_CORE_TAG_PREFIX, ASDF_STANDARD_TAG_PREFIX, Tag, TagHandle};
