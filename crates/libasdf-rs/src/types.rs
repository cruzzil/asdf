//! `#[repr(C)]` mirrors of libasdf's public, non-opaque structs.
//!
//! These layouts are load-bearing: callers construct and read them directly,
//! so a wrong field order or width is silent memory corruption in a C caller
//! rather than a compile error. Every one is checked against the vendored
//! headers by the `public_struct_layouts_match` gate in `tests/abi.rs`.
//!
//! Opaque types -- `asdf_file_t`, `asdf_value_t`, `asdf_block_t` and friends --
//! deliberately do *not* appear here. Their contents are private to the
//! implementation, so they are free to be ordinary Rust types.

use std::ffi::{c_char, c_double, c_int, c_void};

use asdf_core::yaml as asdf_yaml;

use crate::error_ffi::LogLevel;

/// Bitmask of parser options, mirroring `asdf_parser_optflags_t`.
pub type AsdfParserOptFlags = u64;

/// Bitmask of emitter options, mirroring `asdf_emitter_optflags_t`.
pub type AsdfEmitterOptFlags = u64;

/// Bitmask of log fields, mirroring `asdf_log_fields_t`.
pub type AsdfLogFields = u64;

/// Parser options. The header defines these as `1 << bit`.
pub mod parser_opt {
    /// Emit YAML sub-events from the parser.
    pub const EMIT_YAML_EVENTS: u64 = 1 << 0;
    /// Buffer the whole tree while parsing.
    pub const BUFFER_TREE: u64 = 1 << 1;
}

/// Emitter options. The header defines these as `1 << bit`.
pub mod emitter_opt {
    /// The default, empty set.
    pub const DEFAULT: u64 = 1 << 0;
    /// Emit empty containers.
    pub const EMIT_EMPTY: u64 = 1 << 1;
    /// Do not write a block checksum.
    pub const NO_BLOCK_CHECKSUM: u64 = 1 << 2;
    /// Do not write a block index.
    pub const NO_BLOCK_INDEX: u64 = 1 << 3;
    /// Write the tree even when it is empty.
    pub const EMIT_EMPTY_TREE: u64 = 1 << 4;
    /// Do not write an empty tree.
    pub const NO_EMIT_EMPTY_TREE: u64 = 1 << 5;
    /// Do not write the `asdf_library` metadata.
    pub const NO_EMIT_ASDF_LIBRARY: u64 = 1 << 6;
    /// Reserved as the last option.
    pub const LAST: u64 = 1 << 62;
}

/// Log field flags. The header defines these as `1 << bit`.
pub mod log_field {
    /// The severity.
    pub const LEVEL: u64 = 1 << 0;
    /// The originating package.
    pub const PACKAGE: u64 = 1 << 1;
    /// The source file.
    pub const FILE: u64 = 1 << 2;
    /// The source line.
    pub const LINE: u64 = 1 << 3;
    /// The message text.
    pub const MSG: u64 = 1 << 4;
    /// Every field.
    pub const ALL: u64 = LEVEL | PACKAGE | FILE | LINE | MSG;
}

/// Where an ndarray's data is written, mirroring `asdf_array_storage_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum AsdfArrayStorage {
    /// Use the file-level setting.
    #[default]
    Default = 0,
    /// Inline in the tree.
    Inline = 1,
    /// In an internal binary block.
    Internal = 2,
    /// In an external file; reserved, not yet supported.
    External = 3,
}

/// When compressed block data is decompressed, mirroring
/// `asdf_block_decomp_mode_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum AsdfBlockDecompMode {
    /// Choose automatically.
    #[default]
    Auto = 0,
    /// Decompress everything on first access.
    Eager = 1,
    /// Decompress on demand where supported, else fall back to eager.
    Lazy = 2,
}

/// A `%TAG` directive, mirroring `asdf_yaml_tag_handle_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_yaml_tag_handle_t {
    /// The shorthand, `!` included.
    pub handle: *const c_char,
    /// The prefix it expands to.
    pub prefix: *const c_char,
}

/// Per-file logging configuration, mirroring `asdf_log_cfg_t`.
///
/// Any zero field is filled in with a default: the stream is `stderr`, the
/// level comes from `ASDF_LOG_LEVEL` or `WARN`, and the fields are all of them.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_log_cfg_t {
    /// Destination stream; `NULL` means `stderr`.
    pub stream: *mut c_void,
    /// Minimum severity to emit.
    pub level: LogLevel,
    /// Which fields the standard formatter includes.
    pub fields: AsdfLogFields,
    /// Suppress colour even where the build supports it.
    pub no_color: bool,
}

/// Low-level parser configuration, mirroring `asdf_parser_cfg_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_parser_cfg_t {
    /// Bitmask of [`parser_opt`] flags.
    pub flags: AsdfParserOptFlags,
    /// Optional logging configuration.
    pub log: *mut asdf_log_cfg_t,
}

/// Low-level emitter configuration, mirroring `asdf_emitter_cfg_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_emitter_cfg_t {
    /// Bitmask of [`emitter_opt`] flags.
    pub flags: AsdfEmitterOptFlags,
    /// `NULL`-terminated array of tag directives to write.
    pub tag_handles: *mut asdf_yaml_tag_handle_t,
    /// Element count above which an inline ndarray logs a warning. Zero
    /// selects the library default of 1024; `SIZE_MAX` suppresses it.
    pub inline_ndarray_warning_thresh: usize,
    /// Override for where all ndarray data is written.
    pub array_storage: AsdfArrayStorage,
}

/// Decompression options.
///
/// This mirrors the *anonymous* struct that forms `asdf_config_t`'s `decomp`
/// field, so it must be laid out as though it were declared inline there.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_decomp_cfg_t {
    /// When to decompress.
    pub mode: AsdfBlockDecompMode,
    /// Decompressed size above which to spill to disk.
    pub max_memory_bytes: usize,
    /// Fraction of system memory above which to spill to disk.
    pub max_memory_threshold: c_double,
    /// Chunk size for lazy decompression; rounded up to a page.
    pub chunk_size: usize,
    /// Directory for temporary files when spilling to disk.
    pub tmp_dir: *const c_char,
}

/// Extended options for opening a file, mirroring `asdf_config_t`.
///
/// The C API copies this on `asdf_open_ex`, so a caller may pass a local and
/// may leave any field zeroed to accept the default.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_config_t {
    /// Parser configuration.
    pub parser: asdf_parser_cfg_t,
    /// Emitter configuration.
    pub emitter: asdf_emitter_cfg_t,
    /// Logging configuration.
    pub log: asdf_log_cfg_t,
    /// Decompression configuration.
    pub decomp: asdf_decomp_cfg_t,
}

/// The value types, mirroring `asdf_value_type_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum AsdfValueType {
    /// Unknown, typically only after a parse error.
    Unknown = 0,
    /// A sequence.
    Sequence,
    /// A mapping.
    Mapping,
    /// A scalar not yet narrowed.
    Scalar,
    /// A string.
    String,
    /// A boolean.
    Bool,
    /// A null.
    Null,
    /// Signed 8-bit integer.
    Int8,
    /// Signed 16-bit integer.
    Int16,
    /// Signed 32-bit integer.
    Int32,
    /// Signed 64-bit integer.
    Int64,
    /// Unsigned 8-bit integer.
    Uint8,
    /// Unsigned 16-bit integer.
    Uint16,
    /// Unsigned 32-bit integer.
    Uint32,
    /// Unsigned 64-bit integer.
    Uint64,
    /// 32-bit float.
    Float,
    /// 64-bit float.
    Double,
    /// A registered extension type.
    Extension,
}

impl From<asdf_yaml::ValueType> for AsdfValueType {
    fn from(v: asdf_yaml::ValueType) -> Self {
        use asdf_yaml::ValueType as V;
        match v {
            V::Unknown => AsdfValueType::Unknown,
            V::Sequence => AsdfValueType::Sequence,
            V::Mapping => AsdfValueType::Mapping,
            V::Scalar => AsdfValueType::Scalar,
            V::String => AsdfValueType::String,
            V::Bool => AsdfValueType::Bool,
            V::Null => AsdfValueType::Null,
            V::Int8 => AsdfValueType::Int8,
            V::Int16 => AsdfValueType::Int16,
            V::Int32 => AsdfValueType::Int32,
            V::Int64 => AsdfValueType::Int64,
            V::Uint8 => AsdfValueType::Uint8,
            V::Uint16 => AsdfValueType::Uint16,
            V::Uint32 => AsdfValueType::Uint32,
            V::Uint64 => AsdfValueType::Uint64,
            V::Float => AsdfValueType::Float,
            V::Double => AsdfValueType::Double,
            V::Extension => AsdfValueType::Extension,
        }
    }
}

/// Return codes for value access, mirroring `asdf_value_err_t`.
///
/// Note the negative discriminants: `OK` is zero with errors on both sides,
/// so a caller testing `err == ASDF_VALUE_OK` is the only correct check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum AsdfValueErr {
    /// An unspecified error.
    Unknown = -2,
    /// The path does not exist in the tree.
    NotFound = -1,
    /// Success.
    Ok = 0,
    /// The value is not of the requested type.
    TypeMismatch = 1,
    /// A tagged value could not be parsed as its tag claims.
    ParseFailure = 2,
    /// A value could not be serialized.
    EmitFailure = 3,
    /// A numeric value does not fit the requested C type.
    Overflow = 4,
    /// Allocation failed.
    Oom = 5,
    /// The file or value is read-only.
    ReadOnly = 6,
}

/// Error codes for ndarray access, mirroring `asdf_ndarray_err_t`.
pub use crate::ndarray_ffi::NdarrayErr as AsdfNdarrayErr;

/// Node style hints, mirroring `asdf_yaml_node_style_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum AsdfYamlNodeStyle {
    /// Let the emitter choose.
    #[default]
    Auto = 0,
    /// `{...}` / `[...]`.
    Flow = 1,
    /// Indented block notation.
    Block = 2,
}

/// The public head of a mapping iterator, mirroring `asdf_mapping_iter_t`.
///
/// The implementation casts between this and its own larger struct, so this
/// must stay at offset 0 of that struct and keep this exact layout.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_mapping_iter_t {
    /// The current entry's key.
    pub key: *const c_char,
    /// The current entry's value.
    pub value: *mut c_void,
}

/// The public head of a sequence iterator, mirroring `asdf_sequence_iter_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_sequence_iter_t {
    /// The current index.
    pub index: c_int,
    /// The current item.
    pub value: *mut c_void,
}

/// The public head of a container iterator, mirroring
/// `asdf_container_iter_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_container_iter_t {
    /// The current entry's key, or `NULL` when iterating a sequence.
    pub key: *const c_char,
    /// The current index, or `-1` when iterating a mapping.
    pub index: c_int,
    /// The current value.
    pub value: *mut c_void,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn value_err_discriminants_span_zero() {
        // The C header runs these from -2 through 6, so a caller checking
        // `err < 0` for "not found" and `err > 0` for a type problem is
        // relying on the exact values.
        assert_eq!(AsdfValueErr::Unknown as i32, -2);
        assert_eq!(AsdfValueErr::NotFound as i32, -1);
        assert_eq!(AsdfValueErr::Ok as i32, 0);
        assert_eq!(AsdfValueErr::TypeMismatch as i32, 1);
        assert_eq!(AsdfValueErr::ReadOnly as i32, 6);
    }

    #[test]
    fn value_type_discriminants_are_sequential_from_zero() {
        assert_eq!(AsdfValueType::Unknown as i32, 0);
        assert_eq!(AsdfValueType::Sequence as i32, 1);
        assert_eq!(AsdfValueType::Mapping as i32, 2);
        assert_eq!(AsdfValueType::Scalar as i32, 3);
        assert_eq!(AsdfValueType::String as i32, 4);
        assert_eq!(AsdfValueType::Extension as i32, 17);
    }

    #[test]
    fn option_flags_are_bit_positions() {
        assert_eq!(parser_opt::EMIT_YAML_EVENTS, 1);
        assert_eq!(parser_opt::BUFFER_TREE, 2);
        assert_eq!(emitter_opt::DEFAULT, 1);
        assert_eq!(emitter_opt::EMIT_EMPTY, 2);
        assert_eq!(emitter_opt::NO_EMIT_ASDF_LIBRARY, 64);
        assert_eq!(log_field::ALL, 31);
    }

    #[test]
    fn storage_and_decomp_modes_match_the_header() {
        assert_eq!(AsdfArrayStorage::Default as i32, 0);
        assert_eq!(AsdfArrayStorage::External as i32, 3);
        assert_eq!(AsdfBlockDecompMode::Auto as i32, 0);
        assert_eq!(AsdfBlockDecompMode::Lazy as i32, 2);
    }

    #[test]
    fn config_nests_its_parts_in_declaration_order() {
        // asdf_config_t is parser, emitter, log, decomp -- in that order.
        assert_eq!(offset_of!(asdf_config_t, parser), 0);
        assert!(offset_of!(asdf_config_t, emitter) >= size_of::<asdf_parser_cfg_t>());
        assert!(offset_of!(asdf_config_t, log) > offset_of!(asdf_config_t, emitter));
        assert!(offset_of!(asdf_config_t, decomp) > offset_of!(asdf_config_t, log));
    }

    #[test]
    fn iterator_heads_start_with_their_public_fields() {
        // The implementation casts its own struct to these, so the public
        // fields must sit at the front.
        assert_eq!(offset_of!(asdf_mapping_iter_t, key), 0);
        assert_eq!(offset_of!(asdf_sequence_iter_t, index), 0);
        assert_eq!(offset_of!(asdf_container_iter_t, key), 0);
        assert_eq!(align_of::<asdf_mapping_iter_t>(), align_of::<*const c_char>());
    }

    #[test]
    fn value_types_convert_from_the_engine() {
        assert_eq!(AsdfValueType::from(asdf_yaml::ValueType::Uint8), AsdfValueType::Uint8);
        assert_eq!(AsdfValueType::from(asdf_yaml::ValueType::Double), AsdfValueType::Double);
        assert_eq!(AsdfValueType::from(asdf_yaml::ValueType::Null), AsdfValueType::Null);
    }
}
