//! `asdf/file.h`: opening, closing and reading values from a file.
//!
//! # Handle model
//!
//! `asdf_file_t` and `asdf_value_t` are opaque to C, so they are ordinary
//! Rust types here. The one thing the C contract forces on their design is
//! string lifetime: `asdf_error` and `asdf_get_string0` hand back a
//! `const char *` the caller does not own and does not free. The engine's
//! strings are Rust `String`s, which are not NUL-terminated, so each one
//! handed out is interned into an arena owned by the file and freed when the
//! file is closed. That matches libasdf, where such pointers are owned by the
//! file and invalidated by `asdf_close`.

use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
use std::sync::Mutex;

use asdf_core::yaml::{
    self as asdf_yaml, Document, NodeId, Resolved, ScalarStyle, Schema, Tag, resolve,
};
use asdf_core::{PendingBlock, Reader, Writer};

use crate::error_ffi::ErrorState;
use crate::panic::guard;
use crate::types::{AsdfValueErr, AsdfValueType, asdf_config_t};

/// How a file was opened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FileMode {
    /// Backed by an existing file or buffer.
    ReadOnly,
    /// Created empty for writing.
    Write,
}

/// An open ASDF file. Opaque to C.
#[derive(Debug)]
pub struct AsdfFile {
    reader: Option<Reader>,
    document: Option<Document>,
    mode: FileMode,
    /// Blocks queued for writing.
    blocks: Vec<PendingBlock>,
    error: ErrorState,
    /// C strings handed out to callers, kept alive until the file is closed.
    ///
    /// Buffers from `asdf_write_to_mem` are deliberately *not* tracked here:
    /// that function allocates with `malloc` and the caller frees them, which
    /// is the contract libasdf documents.
    interned: Mutex<Vec<CString>>,
}

/// A handle to one value in a file's tree. Opaque to C.
#[derive(Debug)]
pub struct AsdfValue {
    file: *mut AsdfFile,
    node: NodeId,
}

impl AsdfValue {
    /// Build a handle for a node of `file`.
    pub(crate) fn new(file: *mut AsdfFile, node: NodeId) -> Self {
        Self { file, node }
    }
}

/// The file a value belongs to.
pub(crate) fn value_file(value: *mut AsdfValue) -> Option<*mut AsdfFile> {
    if value.is_null() {
        return None;
    }
    let file = unsafe { &*value }.file;
    (!file.is_null()).then_some(file)
}

/// The node a value refers to.
pub(crate) fn value_node(value: *mut AsdfValue) -> Option<NodeId> {
    if value.is_null() {
        return None;
    }
    Some(unsafe { &*value }.node)
}

/// The reader backing a file, for the block API.
pub(crate) fn file_reader(file: *mut AsdfFile) -> Option<&'static Reader> {
    if file.is_null() {
        return None;
    }
    unsafe { &*file }.reader.as_ref()
}

/// The queued blocks of a file open for writing.
pub(crate) fn file_blocks_mut(file: *mut AsdfFile) -> Option<&'static mut Vec<PendingBlock>> {
    if file.is_null() {
        return None;
    }
    let handle = unsafe { &mut *file };
    (handle.mode == FileMode::Write).then_some(&mut handle.blocks)
}

/// The document a value belongs to.
pub(crate) fn value_document(value: *mut AsdfValue) -> Option<&'static Document> {
    let file = value_file(value)?;
    // The C contract has the file outlive every value taken from it.
    unsafe { &*file }.document()
}

impl AsdfFile {
    fn new(mode: FileMode) -> Self {
        Self {
            reader: None,
            document: None,
            mode,
            blocks: Vec::new(),
            error: ErrorState::default(),
            interned: Mutex::new(Vec::new()),
        }
    }

    /// The tree, creating an empty one if the file does not have it yet.
    ///
    /// A file opened for writing starts with no tree; the first `asdf_set_*`
    /// call brings one into being, as libasdf's own write example expects.
    fn document_for_write(&mut self) -> Option<&mut Document> {
        if self.mode != FileMode::Write {
            return None;
        }
        if self.document.is_none() {
            let mut doc = Document::new_asdf();
            let root = doc.add(asdf_core::yaml::Node::mapping());
            // Every ASDF tree's root carries the core/asdf tag.
            doc.node_mut(root).tag = Some(Tag::parse(ASDF_ROOT_TAG));
            doc.set_root(root);
            self.document = Some(doc);
        }
        self.document.as_mut()
    }

    /// Intern a string and return a pointer valid until the file is closed.
    pub(crate) fn intern(&self, s: &str) -> *const c_char {
        let Ok(c) = CString::new(s) else {
            return std::ptr::null();
        };
        let mut arena = self.interned.lock().unwrap_or_else(|e| e.into_inner());
        arena.push(c);
        arena.last().map_or(std::ptr::null(), |c| c.as_ptr())
    }

    /// The file's parsed tree, if it has one.
    fn document(&self) -> Option<&Document> {
        self.document.as_ref()
    }
}

/// The tag every ASDF tree's root carries.
const ASDF_ROOT_TAG: &str = "tag:stsci.edu:asdf/core/asdf-1.1.0";

/// Build a file handle around a reader.
fn open_reader(reader: Reader) -> *mut AsdfFile {
    let mut file = AsdfFile::new(FileMode::ReadOnly);
    match reader.tree() {
        Ok(doc) => file.document = doc,
        Err(e) => {
            // A tree that will not parse is reported, but the file still
            // opens so that its blocks remain reachable.
            file.error.set_error(&e);
        }
    }
    file.reader = Some(reader);
    Box::into_raw(Box::new(file))
}

/// Open a file by path.
///
/// # Safety
/// `filename` and `mode` must be valid NUL-terminated strings or null.
/// `config` must be null or point to a valid `asdf_config_t`. The result must
/// be released with [`asdf_close`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_open_file_ex(
    filename: *const c_char,
    mode: *const c_char,
    config: *mut asdf_config_t,
) -> *mut AsdfFile {
    guard("asdf_open_file_ex", std::ptr::null_mut(), || {
        let _ = config;
        if filename.is_null() {
            return std::ptr::null_mut();
        }
        // Only reading is supported so far; writing arrives in phase 5.
        if !mode.is_null() {
            let mode = unsafe { CStr::from_ptr(mode) }.to_string_lossy();
            if !mode.contains('r') {
                return std::ptr::null_mut();
            }
        }
        let path = unsafe { CStr::from_ptr(filename) }.to_string_lossy().into_owned();
        match Reader::open(&path) {
            Ok(reader) => open_reader(reader),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

/// Open a file from an in-memory buffer.
///
/// # Safety
/// `buf` must point to at least `size` readable bytes, or be null with a
/// `size` of 0. The result must be released with [`asdf_close`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_open_mem_ex(
    buf: *const c_void,
    size: usize,
    config: *mut asdf_config_t,
) -> *mut AsdfFile {
    guard("asdf_open_mem_ex", std::ptr::null_mut(), || {
        let _ = config;
        // `asdf_open(NULL)` expands to `asdf_open_mem(NULL, 0)`, which is how
        // the C API asks for a new, empty file to write into.
        if buf.is_null() || size == 0 {
            return Box::into_raw(Box::new(AsdfFile::new(FileMode::Write)));
        }
        let bytes = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size) }.to_vec();
        match Reader::from_bytes(bytes) {
            Ok(reader) => open_reader(reader),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

/// Open a file from an already-open `FILE *`.
///
/// The stream is read to its end; the caller keeps ownership of it.
///
/// # Safety
/// `fp` must be a `FILE *` open for reading, or null. `filename` is only used
/// in messages and may be null. The result must be released with
/// [`asdf_close`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_open_fp_ex(
    fp: *mut c_void,
    filename: *const c_char,
    config: *mut asdf_config_t,
) -> *mut AsdfFile {
    guard("asdf_open_fp_ex", std::ptr::null_mut(), || {
        let _ = (config, filename);
        if fp.is_null() {
            return std::ptr::null_mut();
        }

        // Read the stream in whole chunks through libc, since the caller owns
        // the FILE and may have already consumed part of it.
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let read = unsafe {
                libc::fread(
                    chunk.as_mut_ptr().cast::<c_void>(),
                    1,
                    chunk.len(),
                    fp.cast::<libc::FILE>(),
                )
            };
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
        if bytes.is_empty() {
            return std::ptr::null_mut();
        }
        match Reader::from_bytes(bytes) {
            Ok(reader) => open_reader(reader),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

/// Close a file and release everything it owns.
///
/// Any `const char *` obtained from this file becomes invalid.
///
/// # Safety
/// `file` must be null or have come from one of the openers, and must not be
/// used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_close(file: *mut AsdfFile) {
    guard("asdf_close", (), || {
        if !file.is_null() {
            drop(unsafe { Box::from_raw(file) });
        }
    })
}

/// The most recent error message, or null if there is none.
///
/// # Safety
/// `file` must be null or a valid file handle. The returned pointer is owned
/// by the file and is invalidated by the next error or by `asdf_close`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_error(file: *mut AsdfFile) -> *const c_char {
    guard("asdf_error", std::ptr::null(), || {
        if file.is_null() {
            return std::ptr::null();
        }
        unsafe { &*file }.error.message_ptr()
    })
}

/// The most recent error code.
///
/// # Safety
/// `file` must be null or a valid file handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_error_code(file: *mut AsdfFile) -> c_int {
    guard("asdf_error_code", 0, || {
        if file.is_null() {
            return 0;
        }
        unsafe { &*file }.error.code()
    })
}

/// The OS `errno` behind the most recent error, when it was a system error.
///
/// # Safety
/// `file` must be null or a valid file handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_error_errno(file: *mut AsdfFile) -> c_int {
    guard("asdf_error_errno", 0, || {
        if file.is_null() {
            return 0;
        }
        unsafe { &*file }.error.errno()
    })
}

/// Look up a node by path.
fn lookup(file: *mut AsdfFile, path: *const c_char) -> Option<(&'static Document, NodeId)> {
    if file.is_null() {
        return None;
    }
    // The handle outlives every value taken from it, by the C contract.
    let f: &'static AsdfFile = unsafe { &*file };
    let doc = f.document()?;
    let path = if path.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(path) }.to_string_lossy().into_owned()
    };
    let node = doc.lookup_str(&path)?;
    Some((doc, node))
}

/// Get a handle to the value at `path`.
///
/// # Safety
/// `file` must be a valid file handle and `path` a valid NUL-terminated
/// string or null. The result must be released with `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_value(
    file: *mut AsdfFile,
    path: *const c_char,
) -> *mut AsdfValue {
    guard("asdf_get_value", std::ptr::null_mut(), || match lookup(file, path) {
        Some((_, node)) => Box::into_raw(Box::new(AsdfValue { file, node })),
        None => std::ptr::null_mut(),
    })
}

/// Release a value handle.
///
/// This does not free anything the value refers to; the file owns that.
///
/// # Safety
/// `value` must be null or have come from a value-producing call, and must
/// not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_destroy(value: *mut AsdfValue) {
    guard("asdf_value_destroy", (), || {
        if !value.is_null() {
            drop(unsafe { Box::from_raw(value) });
        }
    })
}

/// Borrow a value's document and node.
fn value_parts(value: *mut AsdfValue) -> Option<(&'static AsdfFile, &'static Document, NodeId)> {
    if value.is_null() {
        return None;
    }
    let v = unsafe { &*value };
    if v.file.is_null() {
        return None;
    }
    let f: &'static AsdfFile = unsafe { &*v.file };
    let doc = f.document()?;
    Some((f, doc, v.node))
}

/// The resolved type of a value.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_get_type(value: *mut AsdfValue) -> AsdfValueType {
    guard("asdf_value_get_type", AsdfValueType::Unknown, || {
        let Some((_, doc, node)) = value_parts(value) else {
            return AsdfValueType::Unknown;
        };
        AsdfValueType::from(node_type(doc, node))
    })
}

/// The value type of a node, applying libasdf's resolution rules.
fn node_type(doc: &Document, node: NodeId) -> asdf_yaml::ValueType {
    use asdf_yaml::{NodeData, ValueType};
    let resolved = doc.resolved(node);
    match &resolved.data {
        NodeData::Mapping { .. } => ValueType::Mapping,
        NodeData::Sequence { .. } => ValueType::Sequence,
        NodeData::Scalar { value, style } => {
            // An explicit YAML common-schema tag short-circuits inference.
            if let Some(tag) = doc.tag_of(node)
                && tag.is_yaml_builtin()
                && let Some(r) =
                    asdf_yaml::scalar::resolve_tagged(value, tag.suffix(), Schema::Libasdf)
            {
                return r.value_type();
            }
            resolve(value, *style, Schema::Libasdf).value_type()
        }
        NodeData::Alias(_) => ValueType::Unknown,
    }
}

/// The tag on a value, or null if it has none.
///
/// # Safety
/// `value` must be null or a valid value handle. The returned pointer is
/// owned by the file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_tag(value: *mut AsdfValue) -> *const c_char {
    guard("asdf_value_tag", std::ptr::null(), || {
        let Some((file, doc, node)) = value_parts(value) else {
            return std::ptr::null();
        };
        match doc.tag_of(node) {
            Some(tag) => file.intern(&tag.full()),
            None => std::ptr::null(),
        }
    })
}

/// The name libasdf reports for a value type.
///
/// # Safety
/// Always safe; the returned pointer refers to a `'static` string.
#[unsafe(no_mangle)]
pub extern "C" fn asdf_value_type_string(value_type: AsdfValueType) -> *const c_char {
    // Static NUL-terminated names, so no allocation and no lifetime concern.
    let s: &'static CStr = match value_type {
        AsdfValueType::Unknown => c"<unknown>",
        AsdfValueType::Sequence => c"sequence",
        AsdfValueType::Mapping => c"mapping",
        AsdfValueType::Scalar => c"scalar",
        AsdfValueType::String => c"string",
        AsdfValueType::Bool => c"bool",
        AsdfValueType::Null => c"null",
        AsdfValueType::Int8 => c"int8",
        AsdfValueType::Int16 => c"int16",
        AsdfValueType::Int32 => c"int32",
        AsdfValueType::Int64 => c"int64",
        AsdfValueType::Uint8 => c"uint8",
        AsdfValueType::Uint16 => c"uint16",
        AsdfValueType::Uint32 => c"uint32",
        AsdfValueType::Uint64 => c"uint64",
        AsdfValueType::Float => c"float",
        AsdfValueType::Double => c"double",
        AsdfValueType::Extension => c"<extension>",
    };
    s.as_ptr()
}

/// Resolve a path to a scalar and its resolution.
fn resolve_at(file: *mut AsdfFile, path: *const c_char) -> Option<(Resolved, String, ScalarStyle)> {
    let (doc, node) = lookup(file, path)?;
    let resolved_node = doc.resolved(node);
    let (text, style) = match &resolved_node.data {
        asdf_yaml::NodeData::Scalar { value, style } => (value.clone(), *style),
        _ => return None,
    };
    // An explicit common-schema tag wins over inference.
    if let Some(tag) = doc.tag_of(node)
        && tag.is_yaml_builtin()
        && let Some(r) = asdf_yaml::scalar::resolve_tagged(&text, tag.suffix(), Schema::Libasdf)
    {
        return Some((r, text, style));
    }
    Some((resolve(&text, style, Schema::Libasdf), text, style))
}

/// Generate a typed integer getter matching libasdf's semantics.
macro_rules! int_getter {
    ($name:ident, $ty:ty) => {
        /// Read the value at `path` as this integer type.
        ///
        /// Returns `Overflow` when the value is numeric but does not fit, and
        /// `TypeMismatch` when it is not an integer at all.
        ///
        /// # Safety
        /// `file` must be a valid file handle, `path` a valid string or null,
        /// and `out` a writable pointer or null.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            file: *mut AsdfFile,
            path: *const c_char,
            out: *mut $ty,
        ) -> AsdfValueErr {
            guard(stringify!($name), AsdfValueErr::Unknown, || {
                let Some((resolved, _, _)) = resolve_at(file, path) else {
                    return AsdfValueErr::NotFound;
                };
                let narrowed: Option<$ty> = match resolved {
                    Resolved::Uint(v, _) => <$ty>::try_from(v).ok(),
                    Resolved::Int(v, _) => <$ty>::try_from(v).ok(),
                    _ => return AsdfValueErr::TypeMismatch,
                };
                match narrowed {
                    Some(v) => {
                        if !out.is_null() {
                            unsafe { *out = v };
                        }
                        AsdfValueErr::Ok
                    }
                    None => AsdfValueErr::Overflow,
                }
            })
        }
    };
}

int_getter!(asdf_get_int8, i8);
int_getter!(asdf_get_int16, i16);
int_getter!(asdf_get_int32, i32);
int_getter!(asdf_get_int64, i64);
int_getter!(asdf_get_uint8, u8);
int_getter!(asdf_get_uint16, u16);
int_getter!(asdf_get_uint32, u32);
int_getter!(asdf_get_uint64, u64);

/// Read the value at `path` as a `double`.
///
/// Integers are accepted, since a whole number is a valid double.
///
/// # Safety
/// See the integer getters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_double(
    file: *mut AsdfFile,
    path: *const c_char,
    out: *mut c_double,
) -> AsdfValueErr {
    guard("asdf_get_double", AsdfValueErr::Unknown, || {
        let Some((resolved, _, _)) = resolve_at(file, path) else {
            return AsdfValueErr::NotFound;
        };
        let value = match resolved {
            Resolved::Double(d) => d,
            Resolved::Uint(v, _) => v as f64,
            Resolved::Int(v, _) => v as f64,
            _ => return AsdfValueErr::TypeMismatch,
        };
        if !out.is_null() {
            unsafe { *out = value };
        }
        AsdfValueErr::Ok
    })
}

/// Read the value at `path` as a `float`.
///
/// # Safety
/// See the integer getters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_float(
    file: *mut AsdfFile,
    path: *const c_char,
    out: *mut f32,
) -> AsdfValueErr {
    guard("asdf_get_float", AsdfValueErr::Unknown, || {
        let Some((resolved, _, _)) = resolve_at(file, path) else {
            return AsdfValueErr::NotFound;
        };
        let value = match resolved {
            Resolved::Double(d) => d,
            Resolved::Uint(v, _) => v as f64,
            Resolved::Int(v, _) => v as f64,
            _ => return AsdfValueErr::TypeMismatch,
        };
        if !out.is_null() {
            unsafe { *out = value as f32 };
        }
        AsdfValueErr::Ok
    })
}

/// Read the value at `path` as a boolean.
///
/// # Safety
/// See the integer getters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_bool(
    file: *mut AsdfFile,
    path: *const c_char,
    out: *mut bool,
) -> AsdfValueErr {
    guard("asdf_get_bool", AsdfValueErr::Unknown, || {
        let Some((resolved, text, _)) = resolve_at(file, path) else {
            return AsdfValueErr::NotFound;
        };
        // libasdf resolves integers before booleans, so a bare 0 or 1 arrives
        // here as an integer; its documented behaviour is to accept those two
        // as booleans when read as one.
        let value = match resolved {
            Resolved::Bool(b) => b,
            Resolved::Uint(0, _) => false,
            Resolved::Uint(1, _) => true,
            _ => {
                let _ = text;
                return AsdfValueErr::TypeMismatch;
            }
        };
        if !out.is_null() {
            unsafe { *out = value };
        }
        AsdfValueErr::Ok
    })
}

/// Read the value at `path` as a NUL-terminated string.
///
/// # Safety
/// `file` must be a valid file handle, `path` a valid string or null, and
/// `out` a writable pointer or null. The string is owned by the file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_string0(
    file: *mut AsdfFile,
    path: *const c_char,
    out: *mut *const c_char,
) -> AsdfValueErr {
    guard("asdf_get_string0", AsdfValueErr::Unknown, || {
        let Some((resolved, text, _)) = resolve_at(file, path) else {
            return AsdfValueErr::NotFound;
        };
        if !matches!(resolved, Resolved::String) {
            return AsdfValueErr::TypeMismatch;
        }
        let ptr = unsafe { &*file }.intern(&text);
        if ptr.is_null() {
            return AsdfValueErr::Oom;
        }
        if !out.is_null() {
            unsafe { *out = ptr };
        }
        AsdfValueErr::Ok
    })
}

/// Whether the value at `path` is null.
///
/// # Safety
/// See the integer getters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_is_null(file: *mut AsdfFile, path: *const c_char) -> bool {
    guard("asdf_is_null", false, || matches!(resolve_at(file, path), Some((Resolved::Null, _, _))))
}

/// Generate a predicate over a value's resolved type.
macro_rules! type_predicate {
    ($name:ident, $variant:ident) => {
        /// Whether the value at `path` has this type.
        ///
        /// # Safety
        /// See the integer getters.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(file: *mut AsdfFile, path: *const c_char) -> bool {
            guard(stringify!($name), false, || match lookup(file, path) {
                Some((doc, node)) => {
                    AsdfValueType::from(node_type(doc, node)) == AsdfValueType::$variant
                }
                None => false,
            })
        }
    };
}

type_predicate!(asdf_is_mapping, Mapping);
type_predicate!(asdf_is_sequence, Sequence);
type_predicate!(asdf_is_string, String);
type_predicate!(asdf_is_bool, Bool);

/// The number of blocks in the file.
///
/// # Safety
/// `file` must be null or a valid file handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_count(file: *mut AsdfFile) -> usize {
    guard("asdf_block_count", 0, || {
        if file.is_null() {
            return 0;
        }
        let handle = unsafe { &*file };
        // A file opened for writing has no reader; its blocks are the ones
        // queued so far.
        match &handle.reader {
            Some(reader) => reader.block_count(),
            None => handle.blocks.len(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(
            b"name: Dennis Richie\nfoo: 42\nbig: 5000000000\nneg: -7\n\
              pi: 3.5\nyes_flag: true\nnothing: null\nquoted: '1'\n\
              nested:\n  inner: deep\nlist: [a, b, c]\n",
        );
        buf.extend_from_slice(b"...\n");
        buf
    }

    struct Handle(*mut AsdfFile);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { asdf_close(self.0) };
        }
    }

    fn open() -> Handle {
        let bytes = sample();
        let f =
            unsafe { asdf_open_mem_ex(bytes.as_ptr().cast(), bytes.len(), std::ptr::null_mut()) };
        assert!(!f.is_null());
        Handle(f)
    }

    fn cpath(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    #[test]
    fn opens_and_closes_a_memory_buffer() {
        let h = open();
        assert_eq!(unsafe { asdf_error_code(h.0) }, 0);
        assert!(unsafe { asdf_error(h.0) }.is_null());
    }

    #[test]
    fn rejects_bad_arguments_without_crashing() {
        assert!(
            unsafe { asdf_open_file_ex(std::ptr::null(), std::ptr::null(), std::ptr::null_mut()) }
                .is_null()
        );
        // Closing null must be a no-op, as it is upstream.
        unsafe { asdf_close(std::ptr::null_mut()) };
        assert_eq!(unsafe { asdf_error_code(std::ptr::null_mut()) }, 0);
    }

    /// `asdf_open(NULL)` expands to `asdf_open_mem(NULL, 0)`, which the C API
    /// defines as "give me a new, empty file to write into" -- not as an
    /// error. libasdf's own write example opens a file that way.
    #[test]
    fn opening_a_null_buffer_creates_a_writable_file() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        assert!(!f.is_null());
        let h = Handle(f);

        let path = cpath("foo");
        assert_eq!(unsafe { asdf_set_int64(h.0, path.as_ptr(), 42) }, AsdfValueErr::Ok);
    }

    #[test]
    fn writing_to_a_read_only_file_is_refused() {
        let h = open();
        let path = cpath("foo");
        assert_eq!(unsafe { asdf_set_int64(h.0, path.as_ptr(), 1) }, AsdfValueErr::ReadOnly);
    }

    #[test]
    fn a_written_file_reads_back() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let h = Handle(f);

        let name = cpath("name");
        let value = CString::new("Dennis Richie").unwrap();
        assert_eq!(
            unsafe { asdf_set_string0(h.0, name.as_ptr(), value.as_ptr()) },
            AsdfValueErr::Ok
        );
        let foo = cpath("foo");
        assert_eq!(unsafe { asdf_set_int64(h.0, foo.as_ptr(), 42) }, AsdfValueErr::Ok);
        // Intermediate mappings are materialised.
        let nested = cpath("powers/squares");
        assert_eq!(unsafe { asdf_set_uint64(h.0, nested.as_ptr(), 1764) }, AsdfValueErr::Ok);

        let mut buf: *mut c_void = std::ptr::null_mut();
        let mut size: usize = 0;
        assert_eq!(unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) }, 0);
        assert!(!buf.is_null() && size > 0);

        // Read the bytes back through the same API.
        let reopened = unsafe { asdf_open_mem_ex(buf, size, std::ptr::null_mut()) };
        assert!(!reopened.is_null());
        let r = Handle(reopened);

        let mut out: *const c_char = std::ptr::null();
        assert_eq!(unsafe { asdf_get_string0(r.0, name.as_ptr(), &mut out) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { CStr::from_ptr(out) }.to_str().unwrap(), "Dennis Richie");

        let mut v: i64 = 0;
        assert_eq!(unsafe { asdf_get_int64(r.0, foo.as_ptr(), &mut v) }, AsdfValueErr::Ok);
        assert_eq!(v, 42);

        let mut u: u64 = 0;
        assert_eq!(unsafe { asdf_get_uint64(r.0, nested.as_ptr(), &mut u) }, AsdfValueErr::Ok);
        assert_eq!(u, 1764);

        unsafe { libc::free(buf) };
    }

    /// A string of digits must survive as a string, not become an integer.
    #[test]
    fn string_setters_preserve_stringness() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let h = Handle(f);

        let key = cpath("version");
        let value = CString::new("42").unwrap();
        unsafe { asdf_set_string0(h.0, key.as_ptr(), value.as_ptr()) };

        let mut buf: *mut c_void = std::ptr::null_mut();
        let mut size: usize = 0;
        unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) };
        let reopened = unsafe { asdf_open_mem_ex(buf, size, std::ptr::null_mut()) };
        let r = Handle(reopened);

        let mut out: *const c_char = std::ptr::null();
        assert_eq!(
            unsafe { asdf_get_string0(r.0, key.as_ptr(), &mut out) },
            AsdfValueErr::Ok,
            "a quoted numeric string must read back as a string"
        );
        assert_eq!(unsafe { CStr::from_ptr(out) }.to_str().unwrap(), "42");
        unsafe { libc::free(buf) };
    }

    #[test]
    fn a_missing_file_returns_null() {
        let name = cpath("/definitely/not/here.asdf");
        let mode = cpath("r");
        let f = unsafe { asdf_open_file_ex(name.as_ptr(), mode.as_ptr(), std::ptr::null_mut()) };
        assert!(f.is_null());
    }

    #[test]
    fn reads_a_string() {
        let h = open();
        let mut out: *const c_char = std::ptr::null();
        let path = cpath("name");
        assert_eq!(unsafe { asdf_get_string0(h.0, path.as_ptr(), &mut out) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { CStr::from_ptr(out) }.to_str().unwrap(), "Dennis Richie");
    }

    #[test]
    fn reads_integers_at_every_width() {
        let h = open();
        let path = cpath("foo");

        let mut v8: i8 = 0;
        assert_eq!(unsafe { asdf_get_int8(h.0, path.as_ptr(), &mut v8) }, AsdfValueErr::Ok);
        assert_eq!(v8, 42);

        let mut v64: i64 = 0;
        assert_eq!(unsafe { asdf_get_int64(h.0, path.as_ptr(), &mut v64) }, AsdfValueErr::Ok);
        assert_eq!(v64, 42);

        let mut u8v: u8 = 0;
        assert_eq!(unsafe { asdf_get_uint8(h.0, path.as_ptr(), &mut u8v) }, AsdfValueErr::Ok);
        assert_eq!(u8v, 42);
    }

    #[test]
    fn too_small_a_type_overflows_rather_than_truncating() {
        let h = open();
        let path = cpath("big");
        let mut v: u8 = 0;
        assert_eq!(unsafe { asdf_get_uint8(h.0, path.as_ptr(), &mut v) }, AsdfValueErr::Overflow);
        // The wide type still works.
        let mut w: u64 = 0;
        assert_eq!(unsafe { asdf_get_uint64(h.0, path.as_ptr(), &mut w) }, AsdfValueErr::Ok);
        assert_eq!(w, 5_000_000_000);
    }

    #[test]
    fn a_negative_value_does_not_read_as_unsigned() {
        let h = open();
        let path = cpath("neg");
        let mut v: u32 = 0;
        assert_eq!(unsafe { asdf_get_uint32(h.0, path.as_ptr(), &mut v) }, AsdfValueErr::Overflow);
        let mut s: i32 = 0;
        assert_eq!(unsafe { asdf_get_int32(h.0, path.as_ptr(), &mut s) }, AsdfValueErr::Ok);
        assert_eq!(s, -7);
    }

    #[test]
    fn reads_floats_and_accepts_integers_as_doubles() {
        let h = open();
        let mut d: f64 = 0.0;
        let pi = cpath("pi");
        assert_eq!(unsafe { asdf_get_double(h.0, pi.as_ptr(), &mut d) }, AsdfValueErr::Ok);
        assert_eq!(d, 3.5);

        let foo = cpath("foo");
        assert_eq!(unsafe { asdf_get_double(h.0, foo.as_ptr(), &mut d) }, AsdfValueErr::Ok);
        assert_eq!(d, 42.0);
    }

    #[test]
    fn reads_booleans() {
        let h = open();
        let mut b = false;
        let path = cpath("yes_flag");
        assert_eq!(unsafe { asdf_get_bool(h.0, path.as_ptr(), &mut b) }, AsdfValueErr::Ok);
        assert!(b);
    }

    #[test]
    fn a_quoted_number_is_a_string_not_an_integer() {
        let h = open();
        let path = cpath("quoted");

        let mut v: i64 = 0;
        assert_eq!(
            unsafe { asdf_get_int64(h.0, path.as_ptr(), &mut v) },
            AsdfValueErr::TypeMismatch
        );

        let mut s: *const c_char = std::ptr::null();
        assert_eq!(unsafe { asdf_get_string0(h.0, path.as_ptr(), &mut s) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { CStr::from_ptr(s) }.to_str().unwrap(), "1");
    }

    #[test]
    fn a_missing_path_is_not_found() {
        let h = open();
        let path = cpath("nope");
        let mut v: i64 = 0;
        assert_eq!(unsafe { asdf_get_int64(h.0, path.as_ptr(), &mut v) }, AsdfValueErr::NotFound);
    }

    #[test]
    fn nested_and_indexed_paths_resolve() {
        let h = open();
        let mut out: *const c_char = std::ptr::null();
        let path = cpath("nested/inner");
        assert_eq!(unsafe { asdf_get_string0(h.0, path.as_ptr(), &mut out) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { CStr::from_ptr(out) }.to_str().unwrap(), "deep");

        let path = cpath("list/1");
        assert_eq!(unsafe { asdf_get_string0(h.0, path.as_ptr(), &mut out) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { CStr::from_ptr(out) }.to_str().unwrap(), "b");
    }

    #[test]
    fn nulls_and_type_predicates() {
        let h = open();
        let nothing = cpath("nothing");
        assert!(unsafe { asdf_is_null(h.0, nothing.as_ptr()) });

        let nested = cpath("nested");
        assert!(unsafe { asdf_is_mapping(h.0, nested.as_ptr()) });
        assert!(!unsafe { asdf_is_sequence(h.0, nested.as_ptr()) });

        let list = cpath("list");
        assert!(unsafe { asdf_is_sequence(h.0, list.as_ptr()) });

        let name = cpath("name");
        assert!(unsafe { asdf_is_string(h.0, name.as_ptr()) });
    }

    #[test]
    fn value_handles_report_type_and_tag() {
        let h = open();
        let root = cpath("");
        let v = unsafe { asdf_get_value(h.0, root.as_ptr()) };
        assert!(!v.is_null());
        assert_eq!(unsafe { asdf_value_get_type(v) }, AsdfValueType::Mapping);

        let tag = unsafe { asdf_value_tag(v) };
        assert!(!tag.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(tag) }.to_str().unwrap(),
            "tag:stsci.edu:asdf/core/asdf-1.1.0"
        );
        unsafe { asdf_value_destroy(v) };
        unsafe { asdf_value_destroy(std::ptr::null_mut()) };
    }

    #[test]
    fn type_names_match_libasdf() {
        let name = |t| unsafe { CStr::from_ptr(asdf_value_type_string(t)) }.to_str().unwrap();
        assert_eq!(name(AsdfValueType::Uint8), "uint8");
        assert_eq!(name(AsdfValueType::Mapping), "mapping");
        assert_eq!(name(AsdfValueType::Unknown), "<unknown>");
        assert_eq!(name(AsdfValueType::Extension), "<extension>");
    }

    #[test]
    fn interned_strings_stay_valid_while_the_file_is_open() {
        let h = open();
        let mut first: *const c_char = std::ptr::null();
        let path = cpath("name");
        unsafe { asdf_get_string0(h.0, path.as_ptr(), &mut first) };

        // Reading many more strings must not invalidate the first, which the
        // C contract guarantees until asdf_close.
        for _ in 0..100 {
            let mut other: *const c_char = std::ptr::null();
            unsafe { asdf_get_string0(h.0, path.as_ptr(), &mut other) };
        }
        assert_eq!(unsafe { CStr::from_ptr(first) }.to_str().unwrap(), "Dennis Richie");
    }

    #[test]
    fn null_out_pointers_are_accepted() {
        let h = open();
        let path = cpath("foo");
        // A caller may pass NULL to test for existence without reading.
        assert_eq!(
            unsafe { asdf_get_int64(h.0, path.as_ptr(), std::ptr::null_mut()) },
            AsdfValueErr::Ok
        );
    }

    #[test]
    fn block_count_is_reported() {
        let h = open();
        assert_eq!(unsafe { asdf_block_count(h.0) }, 0);
        assert_eq!(unsafe { asdf_block_count(std::ptr::null_mut()) }, 0);
    }
}

// ---- Writing --------------------------------------------------------

/// Resolve a file handle for mutation.
fn write_target(file: *mut AsdfFile) -> Option<&'static mut AsdfFile> {
    if file.is_null() {
        return None;
    }
    Some(unsafe { &mut *file })
}

/// Set a node at `path`, creating intermediate mappings as needed.
fn set_node(
    file: *mut AsdfFile,
    path: *const c_char,
    make: impl FnOnce(&mut Document) -> NodeId,
) -> AsdfValueErr {
    let Some(handle) = write_target(file) else {
        return AsdfValueErr::Unknown;
    };
    if handle.mode != FileMode::Write {
        // libasdf reports a write to a read-only file distinctly from a
        // type problem, so a caller can tell the two apart.
        return AsdfValueErr::ReadOnly;
    }
    let path = if path.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(path) }.to_string_lossy().into_owned()
    };
    let Some(doc) = handle.document_for_write() else {
        return AsdfValueErr::Unknown;
    };
    let node = make(doc);
    match doc.insert_at_str(&path, node) {
        Ok(_) => AsdfValueErr::Ok,
        Err(_) => AsdfValueErr::Unknown,
    }
}

/// Generate a scalar setter.
macro_rules! scalar_setter {
    ($name:ident, $ty:ty) => {
        /// Set the value at `path`.
        ///
        /// Intermediate mappings are created as needed, so setting
        /// `powers/squares` in an empty tree also creates `powers`.
        ///
        /// # Safety
        /// `file` must be a file handle opened for writing and `path` a valid
        /// NUL-terminated string or null.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            file: *mut AsdfFile,
            path: *const c_char,
            value: $ty,
        ) -> AsdfValueErr {
            guard(stringify!($name), AsdfValueErr::Unknown, || {
                set_node(file, path, |doc| doc.add_scalar(value.to_string()))
            })
        }
    };
}

scalar_setter!(asdf_set_int8, i8);
scalar_setter!(asdf_set_int16, i16);
scalar_setter!(asdf_set_int32, i32);
scalar_setter!(asdf_set_int64, i64);
scalar_setter!(asdf_set_uint8, u8);
scalar_setter!(asdf_set_uint16, u16);
scalar_setter!(asdf_set_uint32, u32);
scalar_setter!(asdf_set_uint64, u64);

/// Set a NUL-terminated string at `path`.
///
/// The value is written quoted, so a string of digits reads back as a string
/// rather than as a number.
///
/// # Safety
/// `file` must be a file handle opened for writing; `path` and `value` must
/// be valid NUL-terminated strings or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_set_string0(
    file: *mut AsdfFile,
    path: *const c_char,
    value: *const c_char,
) -> AsdfValueErr {
    guard("asdf_set_string0", AsdfValueErr::Unknown, || {
        if value.is_null() {
            return AsdfValueErr::Unknown;
        }
        let text = unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned();
        set_node(file, path, |doc| {
            // Plain style is fine for text that cannot be mistaken for
            // another type; anything else is quoted so it stays a string.
            let style = match asdf_yaml::resolve(&text, ScalarStyle::Plain, Schema::Libasdf) {
                Resolved::String => ScalarStyle::Plain,
                _ => ScalarStyle::SingleQuoted,
            };
            doc.add_scalar_styled(text, style)
        })
    })
}

/// Set a boolean at `path`.
///
/// # Safety
/// See the integer setters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_set_bool(
    file: *mut AsdfFile,
    path: *const c_char,
    value: bool,
) -> AsdfValueErr {
    guard("asdf_set_bool", AsdfValueErr::Unknown, || {
        set_node(file, path, |doc| doc.add_scalar(if value { "true" } else { "false" }))
    })
}

/// Set a null at `path`.
///
/// # Safety
/// See the integer setters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_set_null(file: *mut AsdfFile, path: *const c_char) -> AsdfValueErr {
    guard("asdf_set_null", AsdfValueErr::Unknown, || {
        set_node(file, path, |doc| doc.add_scalar("null"))
    })
}

/// Set a `double` at `path`.
///
/// # Safety
/// See the integer setters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_set_double(
    file: *mut AsdfFile,
    path: *const c_char,
    value: c_double,
) -> AsdfValueErr {
    guard("asdf_set_double", AsdfValueErr::Unknown, || {
        set_node(file, path, |doc| doc.add_scalar(asdf_core::core::elements::format_float(value)))
    })
}

/// Set a `float` at `path`.
///
/// # Safety
/// See the integer setters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_set_float(
    file: *mut AsdfFile,
    path: *const c_char,
    value: f32,
) -> AsdfValueErr {
    guard("asdf_set_float", AsdfValueErr::Unknown, || {
        set_node(file, path, |doc| {
            doc.add_scalar(asdf_core::core::elements::format_float(f64::from(value)))
        })
    })
}

/// Assemble the file's bytes.
fn serialize(handle: &AsdfFile) -> Result<Vec<u8>, asdf_core::Error> {
    let mut writer = match &handle.document {
        Some(doc) => Writer::from_document(doc.clone()),
        None => Writer::new(),
    };
    for block in &handle.blocks {
        writer.add_block(block.clone());
    }
    writer.to_bytes()
}

/// Write the file to a filesystem path.
///
/// # Safety
/// `file` must be a valid file handle and `filename` a valid NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_write_to_file(file: *mut AsdfFile, filename: *const c_char) -> c_int {
    guard("asdf_write_to_file", -1, || {
        if file.is_null() || filename.is_null() {
            return -1;
        }
        let handle = unsafe { &*file };
        let path = unsafe { CStr::from_ptr(filename) }.to_string_lossy().into_owned();

        match serialize(handle).and_then(|bytes| Ok(std::fs::write(&path, bytes)?)) {
            Ok(()) => 0,
            Err(e) => {
                handle.error.set_error(&e);
                -1
            }
        }
    })
}

/// Write the file to an open `FILE *`.
///
/// # Safety
/// `file` must be a valid file handle and `fp` a `FILE *` open for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_write_to_fp(file: *mut AsdfFile, fp: *mut c_void) -> c_int {
    guard("asdf_write_to_fp", -1, || {
        if file.is_null() || fp.is_null() {
            return -1;
        }
        let handle = unsafe { &*file };
        let bytes = match serialize(handle) {
            Ok(b) => b,
            Err(e) => {
                handle.error.set_error(&e);
                return -1;
            }
        };
        let written = unsafe {
            libc::fwrite(bytes.as_ptr().cast::<c_void>(), 1, bytes.len(), fp.cast::<libc::FILE>())
        };
        if written == bytes.len() { 0 } else { -1 }
    })
}

/// Write the file into a freshly allocated buffer.
///
/// The buffer is allocated with `malloc`, so the caller frees it with
/// `free`. This matches libasdf, whose callers own the result.
///
/// # Safety
/// `file` must be a valid file handle; `buf` and `size` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_write_to_mem(
    file: *mut AsdfFile,
    buf: *mut *mut c_void,
    size: *mut usize,
) -> c_int {
    guard("asdf_write_to_mem", -1, || {
        if file.is_null() || buf.is_null() || size.is_null() {
            return -1;
        }
        let handle = unsafe { &*file };
        let bytes = match serialize(handle) {
            Ok(b) => b,
            Err(e) => {
                handle.error.set_error(&e);
                return -1;
            }
        };

        // Allocated with malloc rather than Rust's allocator, since the C
        // caller frees it with free().
        let allocation = unsafe { libc::malloc(bytes.len().max(1)) };
        if allocation.is_null() {
            return -1;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocation.cast::<u8>(), bytes.len());
            *buf = allocation;
            *size = bytes.len();
        }
        0
    })
}
