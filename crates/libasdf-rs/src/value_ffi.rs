//! `asdf/value.h`: working with values, mappings and sequences.
//!
//! # Iterator contract
//!
//! libasdf's iterators are unusual and the shape is part of the ABI:
//!
//! ```c
//! asdf_mapping_iter_t *iter = asdf_mapping_iter_init(mapping);
//! while (asdf_mapping_iter_next(&iter)) {
//!     use(iter->key, iter->value);
//! }
//! asdf_mapping_iter_destroy(iter);
//! ```
//!
//! `next` takes a *pointer to* the iterator pointer, and on reaching the end
//! it destroys the iterator and sets the caller's pointer to `NULL` -- so the
//! trailing `destroy` is a no-op in the normal case and only matters when the
//! loop breaks early. Each step also frees the previous `value`, which the
//! iterator owns.

use std::ffi::{CStr, CString, c_char, c_int};

use asdf_core::yaml::{NodeData, NodeId, Resolved, ScalarStyle, Schema, resolve};

use crate::file_ffi::{AsdfFile, AsdfValue, value_document, value_file, value_node};
use crate::panic::guard;
use crate::types::{
    AsdfValueErr, AsdfValueType, asdf_container_iter_t, asdf_mapping_iter_t, asdf_sequence_iter_t,
};

/// A mapping handle. In libasdf this is a value known to be a mapping, and
/// the two are freely cast between, so they share a representation here too.
pub type AsdfMapping = AsdfValue;

/// A sequence handle. See [`AsdfMapping`].
pub type AsdfSequence = AsdfValue;

/// The resolution of a value's scalar, if it has one.
fn resolved_of(value: *mut AsdfValue) -> Option<Resolved> {
    let doc = value_document(value)?;
    let node = value_node(value)?;
    let resolved = doc.resolved(node);
    let NodeData::Scalar { value: text, style } = &resolved.data else {
        return None;
    };
    // An explicit YAML common-schema tag wins over inference.
    if let Some(tag) = doc.tag_of(node)
        && tag.is_yaml_builtin()
        && let Some(r) = asdf_core::yaml::resolve_tagged(text, tag.suffix(), Schema::Libasdf)
    {
        return Some(r);
    }
    Some(resolve(text, *style, Schema::Libasdf))
}

/// Build a value handle for a node of the same file.
fn make_value(file: *mut AsdfFile, node: NodeId) -> *mut AsdfValue {
    Box::into_raw(Box::new(AsdfValue::new(file, node)))
}

// ---- Generic value accessors ----------------------------------------

/// Duplicate a value handle.
///
/// The copy refers to the same node; it does not copy the value itself.
///
/// # Safety
/// `value` must be null or a valid value handle. The result must be released
/// with `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_copy(value: *mut AsdfValue) -> *mut AsdfValue {
    guard("asdf_value_copy", std::ptr::null_mut(), || {
        let (Some(file), Some(node)) = (value_file(value), value_node(value)) else {
            return std::ptr::null_mut();
        };
        make_value(file, node)
    })
}

/// The file a value belongs to.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_file(value: *mut AsdfValue) -> *mut AsdfFile {
    guard("asdf_value_file", std::ptr::null_mut(), || {
        value_file(value).unwrap_or(std::ptr::null_mut())
    })
}

/// Whether a value is a mapping or a sequence.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_container(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_container", false, || {
        let (Some(doc), Some(node)) = (value_document(value), value_node(value)) else {
            return false;
        };
        let resolved = doc.resolved(node);
        resolved.is_mapping() || resolved.is_sequence()
    })
}

/// The number of children of a container, or `-1` if it is not one.
///
/// # Safety
/// `container` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_container_size(container: *mut AsdfValue) -> c_int {
    guard("asdf_container_size", -1, || {
        let (Some(doc), Some(node)) = (value_document(container), value_node(container)) else {
            return -1;
        };
        doc.container_len(node).and_then(|n| c_int::try_from(n).ok()).unwrap_or(-1)
    })
}

/// Whether a value matches a given type.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_type(
    value: *mut AsdfValue,
    value_type: AsdfValueType,
) -> bool {
    guard("asdf_value_is_type", false, || {
        let actual = unsafe { crate::file_ffi::asdf_value_get_type(value) };
        actual == value_type
    })
}

// ---- Mappings --------------------------------------------------------

/// Whether a value is a mapping.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_mapping(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_mapping", false, || {
        value_document(value)
            .zip(value_node(value))
            .is_some_and(|(doc, node)| doc.resolved(node).is_mapping())
    })
}

/// View a value as a mapping.
///
/// # Safety
/// `value` must be a valid value handle and `out` writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_mapping(
    value: *mut AsdfValue,
    out: *mut *mut AsdfMapping,
) -> AsdfValueErr {
    guard("asdf_value_as_mapping", AsdfValueErr::Unknown, || {
        if !unsafe { asdf_value_is_mapping(value) } {
            return AsdfValueErr::TypeMismatch;
        }
        if !out.is_null() {
            unsafe { *out = value };
        }
        AsdfValueErr::Ok
    })
}

/// The number of entries in a mapping, or `-1` if it is not one.
///
/// # Safety
/// `mapping` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_size(mapping: *mut AsdfMapping) -> c_int {
    guard("asdf_mapping_size", -1, || {
        if !unsafe { asdf_value_is_mapping(mapping) } {
            return -1;
        }
        unsafe { asdf_container_size(mapping) }
    })
}

/// Look up a mapping entry by key.
///
/// # Safety
/// `mapping` must be a valid handle and `key` a valid NUL-terminated string.
/// The result must be released with `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_get(
    mapping: *mut AsdfMapping,
    key: *const c_char,
) -> *mut AsdfValue {
    guard("asdf_mapping_get", std::ptr::null_mut(), || {
        if key.is_null() {
            return std::ptr::null_mut();
        }
        let (Some(doc), Some(node), Some(file)) =
            (value_document(mapping), value_node(mapping), value_file(mapping))
        else {
            return std::ptr::null_mut();
        };
        let key = unsafe { CStr::from_ptr(key) }.to_string_lossy().into_owned();
        match doc.mapping_get(node, &key) {
            Some(found) => make_value(file, found),
            None => std::ptr::null_mut(),
        }
    })
}

/// A mapping iterator, in file order or reversed.
///
/// `repr(C)` is load-bearing, not decoration: C casts a
/// `*mut asdf_mapping_iter_t` to and from this, so `public` must genuinely
/// sit at offset 0. Without it Rust may reorder the fields and the cast
/// reads whatever happens to be first.
#[repr(C)]
struct MappingIter {
    /// The public head, which C casts to. Must stay first.
    public: asdf_mapping_iter_t,
    file: *mut AsdfFile,
    entries: Vec<(Option<String>, NodeId)>,
    position: usize,
    /// The key string handed out for the current entry, kept alive for the
    /// duration of the step.
    current_key: Option<CString>,
    /// The value handle handed out for the current entry, which the iterator
    /// owns and frees on the next step.
    current_value: *mut AsdfValue,
}

fn mapping_iter_init(mapping: *mut AsdfMapping, reverse: bool) -> *mut asdf_mapping_iter_t {
    let (Some(doc), Some(node), Some(file)) =
        (value_document(mapping), value_node(mapping), value_file(mapping))
    else {
        return std::ptr::null_mut();
    };
    let Some(entries) = doc.mapping_entries(node) else {
        return std::ptr::null_mut();
    };

    let mut collected: Vec<(Option<String>, NodeId)> = entries
        .iter()
        .map(|entry| {
            // A non-scalar key is reported as NULL, as libasdf does: ASDF
            // does not allow them, but the value is still yielded.
            let key = doc.resolved(entry.key).as_str().map(str::to_string);
            (key, entry.value)
        })
        .collect();
    if reverse {
        collected.reverse();
    }

    let iter = Box::new(MappingIter {
        public: asdf_mapping_iter_t { key: std::ptr::null(), value: std::ptr::null_mut() },
        file,
        entries: collected,
        position: 0,
        current_key: None,
        current_value: std::ptr::null_mut(),
    });
    // The public head is the first field, so the pointers are interchangeable.
    Box::into_raw(iter).cast::<asdf_mapping_iter_t>()
}

/// Start iterating a mapping in document order.
///
/// # Safety
/// `mapping` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_iter_init(
    mapping: *mut AsdfMapping,
) -> *mut asdf_mapping_iter_t {
    guard("asdf_mapping_iter_init", std::ptr::null_mut(), || mapping_iter_init(mapping, false))
}

/// Start iterating a mapping in reverse.
///
/// # Safety
/// `mapping` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_reverse_iter_init(
    mapping: *mut AsdfMapping,
) -> *mut asdf_mapping_iter_t {
    guard("asdf_mapping_reverse_iter_init", std::ptr::null_mut(), || {
        mapping_iter_init(mapping, true)
    })
}

/// Advance a mapping iterator.
///
/// Returns `false` at the end, having destroyed the iterator and set
/// `*iter_ptr` to `NULL` -- so a `while` loop over this needs no cleanup of
/// its own, and the trailing `destroy` only matters on an early break.
///
/// # Safety
/// `iter_ptr` must be null or point to an iterator obtained from one of the
/// init functions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_iter_next(iter_ptr: *mut *mut asdf_mapping_iter_t) -> bool {
    guard("asdf_mapping_iter_next", false, || {
        if iter_ptr.is_null() {
            return false;
        }
        let raw = unsafe { *iter_ptr };
        if raw.is_null() {
            return false;
        }
        let iter = unsafe { &mut *raw.cast::<MappingIter>() };

        // Each step releases the handle the previous step handed out.
        if !iter.current_value.is_null() {
            drop(unsafe { Box::from_raw(iter.current_value) });
            iter.current_value = std::ptr::null_mut();
        }

        if iter.position >= iter.entries.len() {
            unsafe { asdf_mapping_iter_destroy(raw) };
            unsafe { *iter_ptr = std::ptr::null_mut() };
            return false;
        }

        let (key, node) = iter.entries[iter.position].clone();
        iter.position += 1;

        iter.current_key = key.and_then(|k| CString::new(k).ok());
        iter.public.key = iter.current_key.as_ref().map_or(std::ptr::null(), |k| k.as_ptr());

        iter.current_value = make_value(iter.file, node);
        iter.public.value = iter.current_value.cast();
        true
    })
}

/// Release a mapping iterator.
///
/// # Safety
/// `iter` must be null or an iterator that has not already been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_iter_destroy(iter: *mut asdf_mapping_iter_t) {
    guard("asdf_mapping_iter_destroy", (), || {
        if iter.is_null() {
            return;
        }
        let mut boxed = unsafe { Box::from_raw(iter.cast::<MappingIter>()) };
        if !boxed.current_value.is_null() {
            drop(unsafe { Box::from_raw(boxed.current_value) });
            boxed.current_value = std::ptr::null_mut();
        }
    })
}

// ---- Sequences -------------------------------------------------------

/// Whether a value is a sequence.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_sequence(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_sequence", false, || {
        value_document(value)
            .zip(value_node(value))
            .is_some_and(|(doc, node)| doc.resolved(node).is_sequence())
    })
}

/// View a value as a sequence.
///
/// # Safety
/// `value` must be a valid value handle and `out` writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_sequence(
    value: *mut AsdfValue,
    out: *mut *mut AsdfSequence,
) -> AsdfValueErr {
    guard("asdf_value_as_sequence", AsdfValueErr::Unknown, || {
        if !unsafe { asdf_value_is_sequence(value) } {
            return AsdfValueErr::TypeMismatch;
        }
        if !out.is_null() {
            unsafe { *out = value };
        }
        AsdfValueErr::Ok
    })
}

/// The number of items in a sequence, or `-1` if it is not one.
///
/// # Safety
/// `sequence` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_size(sequence: *mut AsdfSequence) -> c_int {
    guard("asdf_sequence_size", -1, || {
        if !unsafe { asdf_value_is_sequence(sequence) } {
            return -1;
        }
        unsafe { asdf_container_size(sequence) }
    })
}

/// Index into a sequence. Negative indices count from the end.
///
/// # Safety
/// `sequence` must be a valid handle. The result must be released with
/// `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_get(
    sequence: *mut AsdfSequence,
    index: c_int,
) -> *mut AsdfValue {
    guard("asdf_sequence_get", std::ptr::null_mut(), || {
        let (Some(doc), Some(node), Some(file)) =
            (value_document(sequence), value_node(sequence), value_file(sequence))
        else {
            return std::ptr::null_mut();
        };
        match doc.sequence_get(node, i64::from(index)) {
            Some(found) => make_value(file, found),
            None => std::ptr::null_mut(),
        }
    })
}

/// A sequence iterator. See [`MappingIter`] on why this is `repr(C)`.
#[repr(C)]
struct SequenceIter {
    /// The public head, which C casts to. Must stay first.
    public: asdf_sequence_iter_t,
    file: *mut AsdfFile,
    items: Vec<NodeId>,
    position: usize,
    /// Index reported for the current item, which for a reversed iterator
    /// still counts from the sequence's start.
    indices: Vec<c_int>,
    current_value: *mut AsdfValue,
}

fn sequence_iter_init(sequence: *mut AsdfSequence, reverse: bool) -> *mut asdf_sequence_iter_t {
    let (Some(doc), Some(node), Some(file)) =
        (value_document(sequence), value_node(sequence), value_file(sequence))
    else {
        return std::ptr::null_mut();
    };
    let Some(items) = doc.sequence_items(node) else {
        return std::ptr::null_mut();
    };

    let mut items = items.to_vec();
    let mut indices: Vec<c_int> =
        (0..items.len()).map(|i| c_int::try_from(i).unwrap_or(c_int::MAX)).collect();
    if reverse {
        items.reverse();
        indices.reverse();
    }

    let iter = Box::new(SequenceIter {
        public: asdf_sequence_iter_t { index: -1, value: std::ptr::null_mut() },
        file,
        items,
        position: 0,
        indices,
        current_value: std::ptr::null_mut(),
    });
    Box::into_raw(iter).cast::<asdf_sequence_iter_t>()
}

/// Start iterating a sequence.
///
/// # Safety
/// `sequence` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_iter_init(
    sequence: *mut AsdfSequence,
) -> *mut asdf_sequence_iter_t {
    guard("asdf_sequence_iter_init", std::ptr::null_mut(), || sequence_iter_init(sequence, false))
}

/// Start iterating a sequence in reverse.
///
/// # Safety
/// `sequence` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_reverse_iter_init(
    sequence: *mut AsdfSequence,
) -> *mut asdf_sequence_iter_t {
    guard("asdf_sequence_reverse_iter_init", std::ptr::null_mut(), || {
        sequence_iter_init(sequence, true)
    })
}

/// Advance a sequence iterator. See [`asdf_mapping_iter_next`] for the
/// contract, which is the same.
///
/// # Safety
/// `iter_ptr` must be null or point to an iterator from one of the init
/// functions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_iter_next(iter_ptr: *mut *mut asdf_sequence_iter_t) -> bool {
    guard("asdf_sequence_iter_next", false, || {
        if iter_ptr.is_null() {
            return false;
        }
        let raw = unsafe { *iter_ptr };
        if raw.is_null() {
            return false;
        }
        let iter = unsafe { &mut *raw.cast::<SequenceIter>() };

        if !iter.current_value.is_null() {
            drop(unsafe { Box::from_raw(iter.current_value) });
            iter.current_value = std::ptr::null_mut();
        }

        if iter.position >= iter.items.len() {
            unsafe { asdf_sequence_iter_destroy(raw) };
            unsafe { *iter_ptr = std::ptr::null_mut() };
            return false;
        }

        let node = iter.items[iter.position];
        iter.public.index = iter.indices[iter.position];
        iter.position += 1;

        iter.current_value = make_value(iter.file, node);
        iter.public.value = iter.current_value.cast();
        true
    })
}

/// Release a sequence iterator.
///
/// # Safety
/// `iter` must be null or an iterator that has not already been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_iter_destroy(iter: *mut asdf_sequence_iter_t) {
    guard("asdf_sequence_iter_destroy", (), || {
        if iter.is_null() {
            return;
        }
        let mut boxed = unsafe { Box::from_raw(iter.cast::<SequenceIter>()) };
        if !boxed.current_value.is_null() {
            drop(unsafe { Box::from_raw(boxed.current_value) });
            boxed.current_value = std::ptr::null_mut();
        }
    })
}

// ---- Typed accessors on a value --------------------------------------

/// Generate `asdf_value_is_<type>` and `asdf_value_as_<type>` for an integer.
macro_rules! value_int_accessors {
    ($is:ident, $as:ident, $ty:ty, $variant:ident) => {
        /// Whether the value has this type.
        ///
        /// # Safety
        /// `value` must be null or a valid value handle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $is(value: *mut AsdfValue) -> bool {
            guard(stringify!($is), false, || {
                // Bound rather than compared inline: an `unsafe { .. }` block
                // in expression position followed by `==` parses as a
                // statement.
                let actual = unsafe { crate::file_ffi::asdf_value_get_type(value) };
                actual == AsdfValueType::$variant
            })
        }

        /// Read the value as this type.
        ///
        /// # Safety
        /// `value` must be null or a valid value handle; `out` writable or null.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $as(value: *mut AsdfValue, out: *mut $ty) -> AsdfValueErr {
            guard(stringify!($as), AsdfValueErr::Unknown, || {
                let Some(resolved) = resolved_of(value) else {
                    return AsdfValueErr::TypeMismatch;
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

value_int_accessors!(asdf_value_is_int8, asdf_value_as_int8, i8, Int8);
value_int_accessors!(asdf_value_is_int16, asdf_value_as_int16, i16, Int16);
value_int_accessors!(asdf_value_is_int32, asdf_value_as_int32, i32, Int32);
value_int_accessors!(asdf_value_is_int64, asdf_value_as_int64, i64, Int64);
value_int_accessors!(asdf_value_is_uint8, asdf_value_as_uint8, u8, Uint8);
value_int_accessors!(asdf_value_is_uint16, asdf_value_as_uint16, u16, Uint16);
value_int_accessors!(asdf_value_is_uint32, asdf_value_as_uint32, u32, Uint32);
value_int_accessors!(asdf_value_is_uint64, asdf_value_as_uint64, u64, Uint64);

/// Whether the value is any integer type.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_int(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_int", false, || {
        matches!(resolved_of(value), Some(Resolved::Int(..) | Resolved::Uint(..)))
    })
}

/// Read the value as a `double`.
///
/// # Safety
/// `value` must be null or a valid value handle; `out` writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_double(
    value: *mut AsdfValue,
    out: *mut f64,
) -> AsdfValueErr {
    guard("asdf_value_as_double", AsdfValueErr::Unknown, || {
        let converted = match resolved_of(value) {
            Some(Resolved::Double(d)) => d,
            Some(Resolved::Uint(v, _)) => v as f64,
            Some(Resolved::Int(v, _)) => v as f64,
            _ => return AsdfValueErr::TypeMismatch,
        };
        if !out.is_null() {
            unsafe { *out = converted };
        }
        AsdfValueErr::Ok
    })
}

/// Read the value as a `float`.
///
/// # Safety
/// See [`asdf_value_as_double`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_float(value: *mut AsdfValue, out: *mut f32) -> AsdfValueErr {
    guard("asdf_value_as_float", AsdfValueErr::Unknown, || {
        let mut wide = 0f64;
        let err = unsafe { asdf_value_as_double(value, &mut wide) };
        if err != AsdfValueErr::Ok {
            return err;
        }
        if !out.is_null() {
            unsafe { *out = wide as f32 };
        }
        AsdfValueErr::Ok
    })
}

/// Whether the value is a `double`.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_double(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_double", false, || matches!(resolved_of(value), Some(Resolved::Double(_))))
}

/// Whether the value is a float. libasdf resolves every float as a double,
/// so this matches the same values.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_float(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_float", false, || unsafe { asdf_value_is_double(value) })
}

/// Whether the value is a boolean.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_bool(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_bool", false, || matches!(resolved_of(value), Some(Resolved::Bool(_))))
}

/// Read the value as a boolean.
///
/// As libasdf documents, the integers 0 and 1 are accepted here even though
/// they resolve as integers, because integers are resolved before booleans.
///
/// # Safety
/// `value` must be null or a valid value handle; `out` writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_bool(value: *mut AsdfValue, out: *mut bool) -> AsdfValueErr {
    guard("asdf_value_as_bool", AsdfValueErr::Unknown, || {
        let converted = match resolved_of(value) {
            Some(Resolved::Bool(b)) => b,
            Some(Resolved::Uint(0, _)) => false,
            Some(Resolved::Uint(1, _)) => true,
            _ => return AsdfValueErr::TypeMismatch,
        };
        if !out.is_null() {
            unsafe { *out = converted };
        }
        AsdfValueErr::Ok
    })
}

/// Whether the value is null.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_null(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_null", false, || matches!(resolved_of(value), Some(Resolved::Null)))
}

/// Whether the value is a string.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_string(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_string", false, || matches!(resolved_of(value), Some(Resolved::String)))
}

/// Read the value as a NUL-terminated string.
///
/// # Safety
/// `value` must be null or a valid value handle; `out` writable or null. The
/// string is owned by the value's file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_string0(
    value: *mut AsdfValue,
    out: *mut *const c_char,
) -> AsdfValueErr {
    guard("asdf_value_as_string0", AsdfValueErr::Unknown, || {
        if !matches!(resolved_of(value), Some(Resolved::String)) {
            return AsdfValueErr::TypeMismatch;
        }
        intern_scalar(value, out)
    })
}

/// Whether the value is a scalar of any kind.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_scalar(value: *mut AsdfValue) -> bool {
    guard("asdf_value_is_scalar", false, || {
        value_document(value)
            .zip(value_node(value))
            .is_some_and(|(doc, node)| doc.resolved(node).is_scalar())
    })
}

/// Read a scalar's raw text, whatever its resolved type.
///
/// # Safety
/// `value` must be null or a valid value handle; `out` writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_scalar0(
    value: *mut AsdfValue,
    out: *mut *const c_char,
) -> AsdfValueErr {
    guard("asdf_value_as_scalar0", AsdfValueErr::Unknown, || {
        if !unsafe { asdf_value_is_scalar(value) } {
            return AsdfValueErr::TypeMismatch;
        }
        intern_scalar(value, out)
    })
}

/// Hand out a scalar's text, interned in the file so it stays valid.
fn intern_scalar(value: *mut AsdfValue, out: *mut *const c_char) -> AsdfValueErr {
    let (Some(doc), Some(node), Some(file)) =
        (value_document(value), value_node(value), value_file(value))
    else {
        return AsdfValueErr::Unknown;
    };
    let Some(text) = doc.resolved(node).as_str() else {
        return AsdfValueErr::TypeMismatch;
    };
    let ptr = unsafe { &*file }.intern(text);
    if ptr.is_null() {
        return AsdfValueErr::Oom;
    }
    if !out.is_null() {
        unsafe { *out = ptr };
    }
    AsdfValueErr::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_ffi::{asdf_close, asdf_get_value, asdf_open_mem_ex, asdf_value_destroy};

    fn sample() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(
            b"a: 1\nb: two\nc: 3.5\nflag: true\nnothing: null\n\
              list: [10, 20, 30]\nnested:\n  inner: deep\n",
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

    fn value_at(h: &Handle, path: &str) -> *mut AsdfValue {
        let c = CString::new(path).unwrap();
        let v = unsafe { asdf_get_value(h.0, c.as_ptr()) };
        assert!(!v.is_null(), "no value at {path}");
        v
    }

    #[test]
    fn recognises_containers() {
        let h = open();
        let root = value_at(&h, "");
        assert!(unsafe { asdf_value_is_mapping(root) });
        assert!(unsafe { asdf_value_is_container(root) });
        assert_eq!(unsafe { asdf_mapping_size(root) }, 7);

        let list = value_at(&h, "list");
        assert!(unsafe { asdf_value_is_sequence(list) });
        assert_eq!(unsafe { asdf_sequence_size(list) }, 3);

        let scalar = value_at(&h, "a");
        assert!(!unsafe { asdf_value_is_container(scalar) });
        assert_eq!(unsafe { asdf_mapping_size(scalar) }, -1);
        assert_eq!(unsafe { asdf_sequence_size(scalar) }, -1);

        for v in [root, list, scalar] {
            unsafe { asdf_value_destroy(v) };
        }
    }

    #[test]
    fn mapping_lookup_and_typed_reads() {
        let h = open();
        let root = value_at(&h, "");

        let key = CString::new("a").unwrap();
        let a = unsafe { asdf_mapping_get(root, key.as_ptr()) };
        assert!(!a.is_null());
        let mut n: i64 = 0;
        assert_eq!(unsafe { asdf_value_as_int64(a, &mut n) }, AsdfValueErr::Ok);
        assert_eq!(n, 1);
        assert!(unsafe { asdf_value_is_int(a) });

        let missing = CString::new("nope").unwrap();
        assert!(unsafe { asdf_mapping_get(root, missing.as_ptr()) }.is_null());

        unsafe { asdf_value_destroy(a) };
        unsafe { asdf_value_destroy(root) };
    }

    #[test]
    fn iterates_a_mapping_in_order() {
        let h = open();
        let root = value_at(&h, "");

        let mut iter = unsafe { asdf_mapping_iter_init(root) };
        assert!(!iter.is_null());

        let mut keys = Vec::new();
        while unsafe { asdf_mapping_iter_next(&mut iter) } {
            let head = unsafe { &*iter };
            assert!(!head.key.is_null());
            keys.push(unsafe { CStr::from_ptr(head.key) }.to_str().unwrap().to_string());
            assert!(!head.value.is_null());
        }
        // The loop's end nulls the caller's pointer, so cleanup is a no-op.
        assert!(iter.is_null(), "the iterator must null itself at the end");
        unsafe { asdf_mapping_iter_destroy(iter) };

        assert_eq!(keys, ["a", "b", "c", "flag", "nothing", "list", "nested"]);
        unsafe { asdf_value_destroy(root) };
    }

    #[test]
    fn iterates_a_mapping_in_reverse() {
        let h = open();
        let root = value_at(&h, "");

        let mut iter = unsafe { asdf_mapping_reverse_iter_init(root) };
        let mut keys = Vec::new();
        while unsafe { asdf_mapping_iter_next(&mut iter) } {
            let head = unsafe { &*iter };
            keys.push(unsafe { CStr::from_ptr(head.key) }.to_str().unwrap().to_string());
        }
        assert_eq!(keys, ["nested", "list", "nothing", "flag", "c", "b", "a"]);
        unsafe { asdf_value_destroy(root) };
    }

    #[test]
    fn iterates_a_sequence_with_indices() {
        let h = open();
        let list = value_at(&h, "list");

        let mut iter = unsafe { asdf_sequence_iter_init(list) };
        let mut seen = Vec::new();
        while unsafe { asdf_sequence_iter_next(&mut iter) } {
            let head = unsafe { &*iter };
            let mut n: i64 = 0;
            unsafe { asdf_value_as_int64(head.value.cast(), &mut n) };
            seen.push((head.index, n));
        }
        assert_eq!(seen, [(0, 10), (1, 20), (2, 30)]);
        assert!(iter.is_null());
        unsafe { asdf_value_destroy(list) };
    }

    #[test]
    fn a_reversed_sequence_keeps_original_indices() {
        let h = open();
        let list = value_at(&h, "list");

        let mut iter = unsafe { asdf_sequence_reverse_iter_init(list) };
        let mut seen = Vec::new();
        while unsafe { asdf_sequence_iter_next(&mut iter) } {
            let head = unsafe { &*iter };
            seen.push(head.index);
        }
        assert_eq!(seen, [2, 1, 0]);
        unsafe { asdf_value_destroy(list) };
    }

    #[test]
    fn breaking_out_of_a_loop_leaves_the_iterator_to_destroy() {
        let h = open();
        let root = value_at(&h, "");

        let mut iter = unsafe { asdf_mapping_iter_init(root) };
        let mut count = 0;
        while unsafe { asdf_mapping_iter_next(&mut iter) } {
            count += 1;
            if count == 2 {
                break;
            }
        }
        // The early break leaves a live iterator; destroying it must be
        // clean, including the value handle it still owns.
        assert!(!iter.is_null());
        unsafe { asdf_mapping_iter_destroy(iter) };
        unsafe { asdf_value_destroy(root) };
    }

    #[test]
    fn sequence_indexing_accepts_negatives() {
        let h = open();
        let list = value_at(&h, "list");

        let last = unsafe { asdf_sequence_get(list, -1) };
        assert!(!last.is_null());
        let mut n: i64 = 0;
        unsafe { asdf_value_as_int64(last, &mut n) };
        assert_eq!(n, 30);

        assert!(unsafe { asdf_sequence_get(list, 3) }.is_null());
        unsafe { asdf_value_destroy(last) };
        unsafe { asdf_value_destroy(list) };
    }

    #[test]
    fn typed_predicates_and_reads() {
        let h = open();

        let b = value_at(&h, "b");
        assert!(unsafe { asdf_value_is_string(b) });
        let mut s: *const c_char = std::ptr::null();
        assert_eq!(unsafe { asdf_value_as_string0(b, &mut s) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { CStr::from_ptr(s) }.to_str().unwrap(), "two");

        let c = value_at(&h, "c");
        assert!(unsafe { asdf_value_is_double(c) });
        let mut d = 0f64;
        assert_eq!(unsafe { asdf_value_as_double(c, &mut d) }, AsdfValueErr::Ok);
        assert_eq!(d, 3.5);

        let flag = value_at(&h, "flag");
        assert!(unsafe { asdf_value_is_bool(flag) });
        let mut bl = false;
        assert_eq!(unsafe { asdf_value_as_bool(flag, &mut bl) }, AsdfValueErr::Ok);
        assert!(bl);

        let nothing = value_at(&h, "nothing");
        assert!(unsafe { asdf_value_is_null(nothing) });

        for v in [b, c, flag, nothing] {
            unsafe { asdf_value_destroy(v) };
        }
    }

    #[test]
    fn reading_the_wrong_type_is_a_mismatch_not_a_guess() {
        let h = open();
        let b = value_at(&h, "b");
        let mut n: i64 = 0;
        assert_eq!(unsafe { asdf_value_as_int64(b, &mut n) }, AsdfValueErr::TypeMismatch);
        unsafe { asdf_value_destroy(b) };
    }

    #[test]
    fn as_mapping_and_as_sequence_check_the_type() {
        let h = open();
        let root = value_at(&h, "");
        let list = value_at(&h, "list");

        let mut out: *mut AsdfMapping = std::ptr::null_mut();
        assert_eq!(unsafe { asdf_value_as_mapping(root, &mut out) }, AsdfValueErr::Ok);
        assert_eq!(out, root);
        assert_eq!(unsafe { asdf_value_as_mapping(list, &mut out) }, AsdfValueErr::TypeMismatch);

        let mut seq: *mut AsdfSequence = std::ptr::null_mut();
        assert_eq!(unsafe { asdf_value_as_sequence(list, &mut seq) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { asdf_value_as_sequence(root, &mut seq) }, AsdfValueErr::TypeMismatch);

        unsafe { asdf_value_destroy(root) };
        unsafe { asdf_value_destroy(list) };
    }

    #[test]
    fn copies_are_independent_handles_to_the_same_node() {
        let h = open();
        let a = value_at(&h, "a");
        let copy = unsafe { asdf_value_copy(a) };
        assert!(!copy.is_null());
        assert_ne!(copy, a);

        // Destroying one must leave the other usable.
        unsafe { asdf_value_destroy(a) };
        let mut n: i64 = 0;
        assert_eq!(unsafe { asdf_value_as_int64(copy, &mut n) }, AsdfValueErr::Ok);
        assert_eq!(n, 1);
        unsafe { asdf_value_destroy(copy) };
    }

    #[test]
    fn null_handles_are_tolerated_everywhere() {
        let null = std::ptr::null_mut();
        assert!(!unsafe { asdf_value_is_mapping(null) });
        assert!(!unsafe { asdf_value_is_sequence(null) });
        assert!(!unsafe { asdf_value_is_container(null) });
        assert_eq!(unsafe { asdf_container_size(null) }, -1);
        assert_eq!(unsafe { asdf_mapping_size(null) }, -1);
        assert_eq!(unsafe { asdf_sequence_size(null) }, -1);
        assert!(unsafe { asdf_mapping_iter_init(null) }.is_null());
        assert!(unsafe { asdf_sequence_iter_init(null) }.is_null());
        assert!(!unsafe { asdf_mapping_iter_next(std::ptr::null_mut()) });
        assert!(!unsafe { asdf_sequence_iter_next(std::ptr::null_mut()) });
        unsafe { asdf_mapping_iter_destroy(std::ptr::null_mut()) };
        unsafe { asdf_sequence_iter_destroy(std::ptr::null_mut()) };
        assert!(unsafe { asdf_value_copy(null) }.is_null());
        assert!(unsafe { asdf_value_file(null) }.is_null());
    }

    /// The cast between the public head and the implementation is only
    /// sound while the head sits at offset 0. Pin it, so removing the
    /// `repr(C)` fails here rather than corrupting a C caller's read.
    #[test]
    fn iterator_public_heads_sit_at_offset_zero() {
        use std::mem::offset_of;
        assert_eq!(offset_of!(MappingIter, public), 0);
        assert_eq!(offset_of!(SequenceIter, public), 0);
    }
}

// ---- Container iteration --------------------------------------------

/// An iterator over either kind of container.
///
/// `repr(C)` for the same reason as the other two: C casts a
/// `*mut asdf_container_iter_t` to and from this.
#[repr(C)]
struct ContainerIter {
    /// The public head, which C casts to. Must stay first.
    public: asdf_container_iter_t,
    file: *mut AsdfFile,
    /// The children, with a key for a mapping and none for a sequence.
    entries: Vec<(Option<String>, NodeId)>,
    position: usize,
    current_key: Option<CString>,
    current_value: *mut AsdfValue,
    is_mapping: bool,
}

fn container_iter_init(container: *mut AsdfValue, reverse: bool) -> *mut asdf_container_iter_t {
    let (Some(doc), Some(node), Some(file)) =
        (value_document(container), value_node(container), value_file(container))
    else {
        return std::ptr::null_mut();
    };

    let resolved = doc.resolved(node);
    let is_mapping = resolved.is_mapping();

    let mut entries: Vec<(Option<String>, NodeId)> = if is_mapping {
        doc.mapping_entries(node)
            .unwrap_or(&[])
            .iter()
            .map(|e| (doc.resolved(e.key).as_str().map(str::to_string), e.value))
            .collect()
    } else if resolved.is_sequence() {
        doc.sequence_items(node).unwrap_or(&[]).iter().map(|n| (None, *n)).collect()
    } else {
        return std::ptr::null_mut();
    };

    let mut indices: Vec<c_int> =
        (0..entries.len()).map(|i| c_int::try_from(i).unwrap_or(c_int::MAX)).collect();
    if reverse {
        entries.reverse();
        indices.reverse();
    }

    let iter = Box::new(ContainerIter {
        public: asdf_container_iter_t {
            key: std::ptr::null(),
            // A mapping reports -1 for the index, as libasdf does.
            index: -1,
            value: std::ptr::null_mut(),
        },
        file,
        entries: entries
            .into_iter()
            .zip(indices)
            .map(|((key, node), index)| {
                // Stash the reported index alongside the key by encoding it
                // in the entry order; sequences use their position.
                let _ = index;
                (key, node)
            })
            .collect(),
        position: 0,
        current_key: None,
        current_value: std::ptr::null_mut(),
        is_mapping,
    });
    Box::into_raw(iter).cast::<asdf_container_iter_t>()
}

/// Start iterating a mapping or sequence.
///
/// # Safety
/// `container` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_container_iter_init(
    container: *mut AsdfValue,
) -> *mut asdf_container_iter_t {
    guard("asdf_container_iter_init", std::ptr::null_mut(), || {
        container_iter_init(container, false)
    })
}

/// Start iterating a container in reverse.
///
/// # Safety
/// `container` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_container_reverse_iter_init(
    container: *mut AsdfValue,
) -> *mut asdf_container_iter_t {
    guard("asdf_container_reverse_iter_init", std::ptr::null_mut(), || {
        container_iter_init(container, true)
    })
}

/// Advance a container iterator. Same contract as
/// [`asdf_mapping_iter_next`].
///
/// # Safety
/// `iter_ptr` must be null or point to an iterator from one of the init
/// functions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_container_iter_next(
    iter_ptr: *mut *mut asdf_container_iter_t,
) -> bool {
    guard("asdf_container_iter_next", false, || {
        if iter_ptr.is_null() {
            return false;
        }
        let raw = unsafe { *iter_ptr };
        if raw.is_null() {
            return false;
        }
        let iter = unsafe { &mut *raw.cast::<ContainerIter>() };

        if !iter.current_value.is_null() {
            drop(unsafe { Box::from_raw(iter.current_value) });
            iter.current_value = std::ptr::null_mut();
        }

        if iter.position >= iter.entries.len() {
            unsafe { asdf_container_iter_destroy(raw) };
            unsafe { *iter_ptr = std::ptr::null_mut() };
            return false;
        }

        let (key, node) = iter.entries[iter.position].clone();
        let position = iter.position;
        iter.position += 1;

        if iter.is_mapping {
            iter.current_key = key.and_then(|k| CString::new(k).ok());
            iter.public.key = iter.current_key.as_ref().map_or(std::ptr::null(), |k| k.as_ptr());
            iter.public.index = -1;
        } else {
            iter.current_key = None;
            iter.public.key = std::ptr::null();
            iter.public.index = c_int::try_from(position).unwrap_or(c_int::MAX);
        }

        iter.current_value = make_value(iter.file, node);
        iter.public.value = iter.current_value.cast();
        true
    })
}

/// Release a container iterator.
///
/// # Safety
/// `iter` must be null or an iterator that has not already been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_container_iter_destroy(iter: *mut asdf_container_iter_t) {
    guard("asdf_container_iter_destroy", (), || {
        if iter.is_null() {
            return;
        }
        let mut boxed = unsafe { Box::from_raw(iter.cast::<ContainerIter>()) };
        if !boxed.current_value.is_null() {
            drop(unsafe { Box::from_raw(boxed.current_value) });
            boxed.current_value = std::ptr::null_mut();
        }
    })
}

// ---- Building values -------------------------------------------------

/// Allocate a node in a file's document, creating the document if needed.
fn add_node(
    file: *mut AsdfFile,
    make: impl FnOnce(&mut asdf_core::yaml::Document) -> NodeId,
) -> *mut AsdfValue {
    if file.is_null() {
        return std::ptr::null_mut();
    }
    let handle = unsafe { &mut *file };
    let Some(doc) = handle.document_for_values() else {
        return std::ptr::null_mut();
    };
    let node = make(doc);
    make_value(file, node)
}

/// Generate `asdf_value_of_<type>` for a scalar.
macro_rules! value_of {
    ($name:ident, $ty:ty) => {
        /// Build a value holding this scalar.
        ///
        /// # Safety
        /// `file` must be a valid file handle. The result must be released
        /// with `asdf_value_destroy`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(file: *mut AsdfFile, value: $ty) -> *mut AsdfValue {
            guard(stringify!($name), std::ptr::null_mut(), || {
                add_node(file, |doc| doc.add_scalar(value.to_string()))
            })
        }
    };
}

value_of!(asdf_value_of_int8, i8);
value_of!(asdf_value_of_int16, i16);
value_of!(asdf_value_of_int32, i32);
value_of!(asdf_value_of_int64, i64);
value_of!(asdf_value_of_uint8, u8);
value_of!(asdf_value_of_uint16, u16);
value_of!(asdf_value_of_uint32, u32);
value_of!(asdf_value_of_uint64, u64);

/// Build a value holding a `double`.
///
/// # Safety
/// See the integer constructors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_double(file: *mut AsdfFile, value: f64) -> *mut AsdfValue {
    guard("asdf_value_of_double", std::ptr::null_mut(), || {
        add_node(file, |doc| doc.add_scalar(asdf_core::core::elements::format_float(value)))
    })
}

/// Build a value holding a `float`.
///
/// # Safety
/// See the integer constructors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_float(file: *mut AsdfFile, value: f32) -> *mut AsdfValue {
    guard("asdf_value_of_float", std::ptr::null_mut(), || {
        add_node(file, |doc| {
            doc.add_scalar(asdf_core::core::elements::format_float(f64::from(value)))
        })
    })
}

/// Build a value holding a boolean.
///
/// # Safety
/// See the integer constructors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_bool(file: *mut AsdfFile, value: bool) -> *mut AsdfValue {
    guard("asdf_value_of_bool", std::ptr::null_mut(), || {
        add_node(file, |doc| doc.add_scalar(if value { "true" } else { "false" }))
    })
}

/// Build a null value.
///
/// # Safety
/// See the integer constructors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_null(file: *mut AsdfFile) -> *mut AsdfValue {
    guard("asdf_value_of_null", std::ptr::null_mut(), || {
        add_node(file, |doc| doc.add_scalar("null"))
    })
}

/// Build a value holding a NUL-terminated string.
///
/// The string is quoted where needed so it reads back as a string rather
/// than as a number or boolean.
///
/// # Safety
/// `value` must be a valid NUL-terminated string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_string0(
    file: *mut AsdfFile,
    value: *const c_char,
) -> *mut AsdfValue {
    guard("asdf_value_of_string0", std::ptr::null_mut(), || {
        if value.is_null() {
            return std::ptr::null_mut();
        }
        let text = unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned();
        add_node(file, |doc| {
            let style = match resolve(&text, ScalarStyle::Plain, Schema::Libasdf) {
                Resolved::String => ScalarStyle::Plain,
                _ => ScalarStyle::SingleQuoted,
            };
            doc.add_scalar_styled(text, style)
        })
    })
}

/// Build a value holding a string of `len` bytes.
///
/// # Safety
/// `value` must point to at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_string(
    file: *mut AsdfFile,
    value: *const c_char,
    len: usize,
) -> *mut AsdfValue {
    guard("asdf_value_of_string", std::ptr::null_mut(), || {
        if value.is_null() {
            return std::ptr::null_mut();
        }
        let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) };
        let text = String::from_utf8_lossy(bytes).into_owned();
        add_node(file, |doc| {
            let style = match resolve(&text, ScalarStyle::Plain, Schema::Libasdf) {
                Resolved::String => ScalarStyle::Plain,
                _ => ScalarStyle::SingleQuoted,
            };
            doc.add_scalar_styled(text, style)
        })
    })
}

/// Create an empty mapping.
///
/// # Safety
/// `file` must be a valid file handle. The result must be released with
/// `asdf_mapping_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_create(file: *mut AsdfFile) -> *mut AsdfMapping {
    guard("asdf_mapping_create", std::ptr::null_mut(), || {
        add_node(file, |doc| doc.add(asdf_core::yaml::Node::mapping()))
    })
}

/// Create an empty sequence.
///
/// # Safety
/// `file` must be a valid file handle. The result must be released with
/// `asdf_sequence_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_create(file: *mut AsdfFile) -> *mut AsdfSequence {
    guard("asdf_sequence_create", std::ptr::null_mut(), || {
        add_node(file, |doc| doc.add(asdf_core::yaml::Node::sequence()))
    })
}

/// Release a mapping handle.
///
/// # Safety
/// See `asdf_value_destroy`, which this is equivalent to.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_destroy(mapping: *mut AsdfMapping) {
    unsafe { crate::file_ffi::asdf_value_destroy(mapping) }
}

/// Release a sequence handle.
///
/// # Safety
/// See `asdf_value_destroy`, which this is equivalent to.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_destroy(sequence: *mut AsdfSequence) {
    unsafe { crate::file_ffi::asdf_value_destroy(sequence) }
}

/// Set how a mapping is written: inline, block, or the emitter's choice.
///
/// # Safety
/// `mapping` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_set_style(
    mapping: *mut AsdfMapping,
    style: crate::types::AsdfYamlNodeStyle,
) {
    guard("asdf_mapping_set_style", (), || set_collection_style(mapping, style))
}

/// Set how a sequence is written.
///
/// # Safety
/// `sequence` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_set_style(
    sequence: *mut AsdfSequence,
    style: crate::types::AsdfYamlNodeStyle,
) {
    guard("asdf_sequence_set_style", (), || set_collection_style(sequence, style))
}

fn set_collection_style(value: *mut AsdfValue, style: crate::types::AsdfYamlNodeStyle) {
    use crate::types::AsdfYamlNodeStyle;
    use asdf_core::yaml::CollectionStyle;

    let Some(file) = value_file(value) else { return };
    let Some(node) = value_node(value) else { return };
    let handle = unsafe { &mut *file };
    let Some(doc) = handle.document_for_values() else { return };

    let target = doc.resolve(node);
    let wanted = match style {
        AsdfYamlNodeStyle::Auto => CollectionStyle::Auto,
        AsdfYamlNodeStyle::Flow => CollectionStyle::Flow,
        AsdfYamlNodeStyle::Block => CollectionStyle::Block,
    };
    match &mut doc.node_mut(target).data {
        NodeData::Mapping { style, .. } | NodeData::Sequence { style, .. } => *style = wanted,
        _ => {}
    }
}

/// Put a value into a mapping under `key`.
///
/// # Safety
/// `mapping` and `value` must be valid handles from the same file; `key` a
/// valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_set(
    mapping: *mut AsdfMapping,
    key: *const c_char,
    value: *mut AsdfValue,
) -> AsdfValueErr {
    guard("asdf_mapping_set", AsdfValueErr::Unknown, || {
        if key.is_null() {
            return AsdfValueErr::Unknown;
        }
        let (Some(file), Some(target)) = (value_file(mapping), value_node(mapping)) else {
            return AsdfValueErr::Unknown;
        };
        let Some(child) = value_node(value) else {
            return AsdfValueErr::Unknown;
        };
        let key = unsafe { CStr::from_ptr(key) }.to_string_lossy().into_owned();

        let handle = unsafe { &mut *file };
        let Some(doc) = handle.document_for_values() else {
            return AsdfValueErr::Unknown;
        };
        if !doc.resolved(target).is_mapping() {
            return AsdfValueErr::TypeMismatch;
        }
        doc.mapping_set(target, &key, child);
        AsdfValueErr::Ok
    })
}

/// Remove an entry from a mapping, returning it.
///
/// # Safety
/// `mapping` must be a valid handle and `key` a valid NUL-terminated string.
/// The result must be released with `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_pop(
    mapping: *mut AsdfMapping,
    key: *const c_char,
) -> *mut AsdfValue {
    guard("asdf_mapping_pop", std::ptr::null_mut(), || {
        if key.is_null() {
            return std::ptr::null_mut();
        }
        let (Some(file), Some(target)) = (value_file(mapping), value_node(mapping)) else {
            return std::ptr::null_mut();
        };
        let key = unsafe { CStr::from_ptr(key) }.to_string_lossy().into_owned();

        let handle = unsafe { &mut *file };
        let Some(doc) = handle.document_for_values() else {
            return std::ptr::null_mut();
        };
        match doc.mapping_remove(target, &key) {
            Some(node) => make_value(file, node),
            None => std::ptr::null_mut(),
        }
    })
}

/// Append a value to a sequence.
///
/// # Safety
/// `sequence` and `value` must be valid handles from the same file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_append(
    sequence: *mut AsdfSequence,
    value: *mut AsdfValue,
) -> AsdfValueErr {
    guard("asdf_sequence_append", AsdfValueErr::Unknown, || {
        let (Some(file), Some(target)) = (value_file(sequence), value_node(sequence)) else {
            return AsdfValueErr::Unknown;
        };
        let Some(child) = value_node(value) else {
            return AsdfValueErr::Unknown;
        };
        let handle = unsafe { &mut *file };
        let Some(doc) = handle.document_for_values() else {
            return AsdfValueErr::Unknown;
        };

        let resolved = doc.resolve(target);
        if !doc.node(resolved).is_sequence() {
            return AsdfValueErr::TypeMismatch;
        }
        match &mut doc.node_mut(resolved).data {
            NodeData::Sequence { items, .. } => {
                items.push(child);
                AsdfValueErr::Ok
            }
            _ => AsdfValueErr::TypeMismatch,
        }
    })
}

/// Remove an item from a sequence, returning it.
///
/// # Safety
/// `sequence` must be a valid handle. The result must be released with
/// `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_pop(
    sequence: *mut AsdfSequence,
    index: c_int,
) -> *mut AsdfValue {
    guard("asdf_sequence_pop", std::ptr::null_mut(), || {
        let (Some(file), Some(target)) = (value_file(sequence), value_node(sequence)) else {
            return std::ptr::null_mut();
        };
        let handle = unsafe { &mut *file };
        let Some(doc) = handle.document_for_values() else {
            return std::ptr::null_mut();
        };
        match doc.sequence_remove(target, i64::from(index)) {
            Some(node) => make_value(file, node),
            None => std::ptr::null_mut(),
        }
    })
}

#[cfg(test)]
mod build_tests {
    use super::*;
    use crate::file_ffi::{asdf_close, asdf_open_mem_ex, asdf_value_destroy, asdf_write_to_mem};
    use crate::types::AsdfYamlNodeStyle;
    use std::ffi::c_void;

    struct Handle(*mut AsdfFile);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { asdf_close(self.0) };
        }
    }

    fn writable() -> Handle {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        assert!(!f.is_null());
        Handle(f)
    }

    fn cstr(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    #[test]
    fn builds_a_mapping_from_values() {
        let h = writable();

        let mapping = unsafe { asdf_mapping_create(h.0) };
        assert!(!mapping.is_null());
        assert!(unsafe { asdf_value_is_mapping(mapping) });
        assert_eq!(unsafe { asdf_mapping_size(mapping) }, 0);

        let n = unsafe { asdf_value_of_int64(h.0, 42) };
        let key = cstr("answer");
        assert_eq!(unsafe { asdf_mapping_set(mapping, key.as_ptr(), n) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { asdf_mapping_size(mapping) }, 1);

        let found = unsafe { asdf_mapping_get(mapping, key.as_ptr()) };
        let mut value: i64 = 0;
        assert_eq!(unsafe { asdf_value_as_int64(found, &mut value) }, AsdfValueErr::Ok);
        assert_eq!(value, 42);

        unsafe { asdf_value_destroy(found) };
        unsafe { asdf_value_destroy(n) };
        unsafe { asdf_mapping_destroy(mapping) };
    }

    #[test]
    fn builds_a_sequence_by_appending() {
        let h = writable();
        let sequence = unsafe { asdf_sequence_create(h.0) };
        assert!(unsafe { asdf_value_is_sequence(sequence) });

        for value in [10i64, 20, 30] {
            let item = unsafe { asdf_value_of_int64(h.0, value) };
            assert_eq!(unsafe { asdf_sequence_append(sequence, item) }, AsdfValueErr::Ok);
            unsafe { asdf_value_destroy(item) };
        }
        assert_eq!(unsafe { asdf_sequence_size(sequence) }, 3);

        let second = unsafe { asdf_sequence_get(sequence, 1) };
        let mut value: i64 = 0;
        unsafe { asdf_value_as_int64(second, &mut value) };
        assert_eq!(value, 20);

        unsafe { asdf_value_destroy(second) };
        unsafe { asdf_sequence_destroy(sequence) };
    }

    #[test]
    fn popping_removes_and_returns() {
        let h = writable();

        let mapping = unsafe { asdf_mapping_create(h.0) };
        let n = unsafe { asdf_value_of_int64(h.0, 7) };
        let key = cstr("gone");
        unsafe { asdf_mapping_set(mapping, key.as_ptr(), n) };

        let popped = unsafe { asdf_mapping_pop(mapping, key.as_ptr()) };
        assert!(!popped.is_null());
        let mut value: i64 = 0;
        unsafe { asdf_value_as_int64(popped, &mut value) };
        assert_eq!(value, 7);
        assert_eq!(unsafe { asdf_mapping_size(mapping) }, 0);
        // Popping again finds nothing.
        assert!(unsafe { asdf_mapping_pop(mapping, key.as_ptr()) }.is_null());

        let sequence = unsafe { asdf_sequence_create(h.0) };
        for value in [1i64, 2] {
            let item = unsafe { asdf_value_of_int64(h.0, value) };
            unsafe { asdf_sequence_append(sequence, item) };
            unsafe { asdf_value_destroy(item) };
        }
        let first = unsafe { asdf_sequence_pop(sequence, 0) };
        assert!(!first.is_null());
        assert_eq!(unsafe { asdf_sequence_size(sequence) }, 1);

        for v in [popped, first, n, mapping, sequence] {
            unsafe { asdf_value_destroy(v) };
        }
    }

    #[test]
    fn a_built_tree_writes_and_reads_back() {
        let h = writable();

        // Build `meta: {name: 'obs', frames: [1, 2, 3]}` from values, then
        // attach it at a path and write the file.
        let meta = unsafe { asdf_mapping_create(h.0) };
        let name = unsafe { asdf_value_of_string0(h.0, cstr("obs").as_ptr()) };
        let name_key = cstr("name");
        unsafe { asdf_mapping_set(meta, name_key.as_ptr(), name) };

        let frames = unsafe { asdf_sequence_create(h.0) };
        for value in [1i64, 2, 3] {
            let item = unsafe { asdf_value_of_int64(h.0, value) };
            unsafe { asdf_sequence_append(frames, item) };
            unsafe { asdf_value_destroy(item) };
        }
        let frames_key = cstr("frames");
        unsafe { asdf_mapping_set(meta, frames_key.as_ptr(), frames) };

        let path = cstr("meta");
        assert_eq!(
            unsafe { crate::file_ffi::set_value_at(h.0, path.as_ptr(), meta) },
            AsdfValueErr::Ok
        );

        let mut buf: *mut c_void = std::ptr::null_mut();
        let mut size = 0usize;
        assert_eq!(unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) }, 0);

        let reopened = unsafe { asdf_open_mem_ex(buf, size, std::ptr::null_mut()) };
        let r = Handle(reopened);

        let inner = cstr("meta/name");
        let mut text: *const c_char = std::ptr::null();
        assert_eq!(
            unsafe { crate::file_ffi::asdf_get_string0(r.0, inner.as_ptr(), &mut text) },
            AsdfValueErr::Ok
        );
        assert_eq!(unsafe { CStr::from_ptr(text) }.to_str().unwrap(), "obs");

        let third = cstr("meta/frames/2");
        let mut value: i64 = 0;
        assert_eq!(
            unsafe { crate::file_ffi::asdf_get_int64(r.0, third.as_ptr(), &mut value) },
            AsdfValueErr::Ok
        );
        assert_eq!(value, 3);

        unsafe { libc::free(buf) };
        for v in [meta, name, frames] {
            unsafe { asdf_value_destroy(v) };
        }
    }

    #[test]
    fn a_numeric_string_value_stays_a_string() {
        let h = writable();
        let value = unsafe { asdf_value_of_string0(h.0, cstr("42").as_ptr()) };
        assert!(unsafe { asdf_value_is_string(value) });
        let mut n: i64 = 0;
        assert_eq!(unsafe { asdf_value_as_int64(value, &mut n) }, AsdfValueErr::TypeMismatch);
        unsafe { asdf_value_destroy(value) };
    }

    #[test]
    fn scalar_constructors_produce_the_right_types() {
        let h = writable();

        let cases: Vec<(*mut AsdfValue, AsdfValueType)> = vec![
            (unsafe { asdf_value_of_bool(h.0, true) }, AsdfValueType::Bool),
            (unsafe { asdf_value_of_null(h.0) }, AsdfValueType::Null),
            (unsafe { asdf_value_of_double(h.0, 1.5) }, AsdfValueType::Double),
            // Small positives narrow to uint8, as libasdf resolves them.
            (unsafe { asdf_value_of_int64(h.0, 7) }, AsdfValueType::Uint8),
            (unsafe { asdf_value_of_int64(h.0, -7) }, AsdfValueType::Int8),
        ];
        for (value, expected) in cases {
            assert!(!value.is_null());
            assert_eq!(unsafe { crate::file_ffi::asdf_value_get_type(value) }, expected);
            unsafe { asdf_value_destroy(value) };
        }
    }

    #[test]
    fn iterates_either_container_kind() {
        let h = writable();

        let mapping = unsafe { asdf_mapping_create(h.0) };
        for (key, value) in [("a", 1i64), ("b", 2)] {
            let item = unsafe { asdf_value_of_int64(h.0, value) };
            unsafe { asdf_mapping_set(mapping, cstr(key).as_ptr(), item) };
            unsafe { asdf_value_destroy(item) };
        }

        let mut iter = unsafe { asdf_container_iter_init(mapping) };
        let mut seen = Vec::new();
        while unsafe { asdf_container_iter_next(&mut iter) } {
            let head = unsafe { &*iter };
            assert!(!head.key.is_null(), "a mapping must report keys");
            assert_eq!(head.index, -1, "a mapping reports -1 for the index");
            seen.push(unsafe { CStr::from_ptr(head.key) }.to_str().unwrap().to_string());
        }
        assert_eq!(seen, ["a", "b"]);
        assert!(iter.is_null());

        let sequence = unsafe { asdf_sequence_create(h.0) };
        for value in [10i64, 20] {
            let item = unsafe { asdf_value_of_int64(h.0, value) };
            unsafe { asdf_sequence_append(sequence, item) };
            unsafe { asdf_value_destroy(item) };
        }
        let mut iter = unsafe { asdf_container_iter_init(sequence) };
        let mut indices = Vec::new();
        while unsafe { asdf_container_iter_next(&mut iter) } {
            let head = unsafe { &*iter };
            assert!(head.key.is_null(), "a sequence reports no key");
            indices.push(head.index);
        }
        assert_eq!(indices, [0, 1]);

        unsafe { asdf_mapping_destroy(mapping) };
        unsafe { asdf_sequence_destroy(sequence) };
    }

    #[test]
    fn container_iteration_can_be_reversed() {
        let h = writable();
        let sequence = unsafe { asdf_sequence_create(h.0) };
        for value in [1i64, 2, 3] {
            let item = unsafe { asdf_value_of_int64(h.0, value) };
            unsafe { asdf_sequence_append(sequence, item) };
            unsafe { asdf_value_destroy(item) };
        }

        let mut iter = unsafe { asdf_container_reverse_iter_init(sequence) };
        let mut values = Vec::new();
        while unsafe { asdf_container_iter_next(&mut iter) } {
            let head = unsafe { &*iter };
            let mut n: i64 = 0;
            unsafe { asdf_value_as_int64(head.value.cast(), &mut n) };
            values.push(n);
        }
        assert_eq!(values, [3, 2, 1]);
        unsafe { asdf_sequence_destroy(sequence) };
    }

    #[test]
    fn styles_can_be_set() {
        let h = writable();
        let sequence = unsafe { asdf_sequence_create(h.0) };
        // Setting a style must not disturb the contents.
        unsafe { asdf_sequence_set_style(sequence, AsdfYamlNodeStyle::Flow) };
        unsafe { asdf_sequence_set_style(sequence, AsdfYamlNodeStyle::Block) };
        assert_eq!(unsafe { asdf_sequence_size(sequence) }, 0);

        let mapping = unsafe { asdf_mapping_create(h.0) };
        unsafe { asdf_mapping_set_style(mapping, AsdfYamlNodeStyle::Flow) };
        assert_eq!(unsafe { asdf_mapping_size(mapping) }, 0);

        unsafe { asdf_sequence_destroy(sequence) };
        unsafe { asdf_mapping_destroy(mapping) };
    }

    #[test]
    fn setting_the_wrong_kind_is_a_mismatch() {
        let h = writable();
        let sequence = unsafe { asdf_sequence_create(h.0) };
        let item = unsafe { asdf_value_of_int64(h.0, 1) };

        // A sequence is not a mapping, and vice versa.
        assert_eq!(
            unsafe { asdf_mapping_set(sequence, cstr("k").as_ptr(), item) },
            AsdfValueErr::TypeMismatch
        );

        let mapping = unsafe { asdf_mapping_create(h.0) };
        assert_eq!(unsafe { asdf_sequence_append(mapping, item) }, AsdfValueErr::TypeMismatch);

        unsafe { asdf_value_destroy(item) };
        unsafe { asdf_sequence_destroy(sequence) };
        unsafe { asdf_mapping_destroy(mapping) };
    }

    #[test]
    fn null_handles_are_tolerated() {
        assert!(unsafe { asdf_mapping_create(std::ptr::null_mut()) }.is_null());
        assert!(unsafe { asdf_sequence_create(std::ptr::null_mut()) }.is_null());
        assert!(unsafe { asdf_value_of_int64(std::ptr::null_mut(), 0) }.is_null());
        assert!(unsafe { asdf_value_of_null(std::ptr::null_mut()) }.is_null());
        assert!(unsafe { asdf_value_of_string0(std::ptr::null_mut(), std::ptr::null()) }.is_null());
        assert!(unsafe { asdf_container_iter_init(std::ptr::null_mut()) }.is_null());
        assert!(!unsafe { asdf_container_iter_next(std::ptr::null_mut()) });
        unsafe { asdf_container_iter_destroy(std::ptr::null_mut()) };
    }
}
