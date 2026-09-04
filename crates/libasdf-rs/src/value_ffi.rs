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
pub(crate) fn make_value(file: *mut AsdfFile, node: NodeId) -> *mut AsdfValue {
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
pub unsafe extern "C" fn asdf_value_is_type(value: *mut AsdfValue, value_type: c_int) -> bool {
    guard("asdf_value_is_type", false, || {
        // Taken as an `int`: C may pass anything, and an out-of-range value
        // in a Rust enum is undefined behaviour. See `AsdfValueType::from_i32`.
        let Some(wanted) = AsdfValueType::from_i32(value_type) else {
            return false;
        };
        match wanted {
            // `Unknown` names no type, so nothing is of it.
            AsdfValueType::Unknown => false,
            // `Scalar` is the category, not a resolution: a string, a
            // boolean and an integer are all scalars.
            AsdfValueType::Scalar => unsafe { asdf_value_is_scalar(value) },
            AsdfValueType::Mapping => unsafe { asdf_value_is_mapping(value) },
            AsdfValueType::Sequence => unsafe { asdf_value_is_sequence(value) },
            AsdfValueType::Bool => unsafe { asdf_value_is_bool(value) },
            AsdfValueType::Int8 => unsafe { asdf_value_is_int8(value) },
            AsdfValueType::Int16 => unsafe { asdf_value_is_int16(value) },
            AsdfValueType::Int32 => unsafe { asdf_value_is_int32(value) },
            AsdfValueType::Int64 => unsafe { asdf_value_is_int64(value) },
            AsdfValueType::Uint8 => unsafe { asdf_value_is_uint8(value) },
            AsdfValueType::Uint16 => unsafe { asdf_value_is_uint16(value) },
            AsdfValueType::Uint32 => unsafe { asdf_value_is_uint32(value) },
            AsdfValueType::Uint64 => unsafe { asdf_value_is_uint64(value) },
            other => {
                let actual = unsafe { crate::file_ffi::asdf_value_get_type(value) };
                actual == other
            }
        }
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
        /// Whether the value is an integer that this type can hold.
        ///
        /// Not "whose inferred type is exactly this": an `int8` of `-127`
        /// *is* an `int16`, and a caller asking whether it can read one is
        /// asking whether the value fits, not how it was spelled.
        ///
        /// # Safety
        /// `value` must be null or a valid value handle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $is(value: *mut AsdfValue) -> bool {
            guard(stringify!($is), false, || match resolved_of(value) {
                Some(Resolved::Uint(v, _)) => <$ty>::try_from(v).is_ok(),
                Some(Resolved::Int(v, _)) => <$ty>::try_from(v).is_ok(),
                _ => false,
            })
        }

        /// Read the value as this type.
        ///
        /// A value too large for the type is still written, truncated to the
        /// type's width as a C cast would, *and* reported as an overflow --
        /// the caller decides whether the truncation is acceptable. Only a
        /// value that is not an integer at all leaves `out` untouched.
        ///
        /// # Safety
        /// `value` must be null or a valid value handle; `out` writable or null.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $as(value: *mut AsdfValue, out: *mut $ty) -> AsdfValueErr {
            guard(stringify!($as), AsdfValueErr::Unknown, || {
                let Some(resolved) = resolved_of(value) else {
                    return AsdfValueErr::TypeMismatch;
                };
                let (truncated, fits): ($ty, bool) = match resolved {
                    Resolved::Uint(v, _) => (v as $ty, <$ty>::try_from(v).is_ok()),
                    Resolved::Int(v, _) => (v as $ty, <$ty>::try_from(v).is_ok()),
                    // The text is an integer, just not one any type holds,
                    // so this is an overflow rather than a type mismatch.
                    Resolved::IntOverflow => return AsdfValueErr::Overflow,
                    _ => return AsdfValueErr::TypeMismatch,
                };
                if !out.is_null() {
                    unsafe { *out = truncated };
                }
                if fits { AsdfValueErr::Ok } else { AsdfValueErr::Overflow }
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
        let narrow = wide as f32;
        if !out.is_null() {
            unsafe { *out = narrow };
        }
        // A finite `double` with no `float` becomes an infinity. The value
        // is still handed over -- the caller may not care -- but the loss is
        // reported. A value that was already infinite loses nothing.
        if wide.is_finite() && narrow.is_infinite() {
            return AsdfValueErr::Overflow;
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
    guard("asdf_value_is_bool", false, || bool_of(value).is_some())
}

/// A value's boolean reading, if it has one.
///
/// libasdf's boolean parser accepts `0` and `1`, but tries integers first,
/// so a bare `1` *resolves* as `uint8` while still reading as `true`. Both
/// halves are the contract: the reported type is the integer one, and asking
/// for a boolean succeeds.
fn bool_of(value: *mut AsdfValue) -> Option<bool> {
    match resolved_of(value)? {
        Resolved::Bool(v) => Some(v),
        Resolved::Uint(0, _) => Some(false),
        Resolved::Uint(1, _) => Some(true),
        _ => None,
    }
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
        let Some(converted) = bool_of(value) else {
            return AsdfValueErr::TypeMismatch;
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
        // A null handle is not a value of the wrong type; there is no value.
        if value.is_null() {
            return AsdfValueErr::Unknown;
        }
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
    /// The children, each with its key (for a mapping) and the position it
    /// holds in the container -- which is what `index` reports, counting
    /// from the container's own start even when iterating in reverse.
    entries: Vec<(Option<String>, NodeId, c_int)>,
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

    let mut numbered: Vec<(Option<String>, NodeId, c_int)> = entries
        .drain(..)
        .enumerate()
        .map(|(index, (key, node))| (key, node, c_int::try_from(index).unwrap_or(c_int::MAX)))
        .collect();
    if reverse {
        numbered.reverse();
    }

    let iter = Box::new(ContainerIter {
        public: asdf_container_iter_t {
            key: std::ptr::null(),
            index: -1,
            value: std::ptr::null_mut(),
        },
        file,
        entries: numbered,
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

        let (key, node, index) = iter.entries[iter.position].clone();
        iter.position += 1;

        // A mapping's entries are numbered too: the index is the position in
        // the container, which a caller can use to address the entry either
        // way round.
        if iter.is_mapping {
            iter.current_key = key.and_then(|k| CString::new(k).ok());
            iter.public.key = iter.current_key.as_ref().map_or(std::ptr::null(), |k| k.as_ptr());
        } else {
            iter.current_key = None;
            iter.public.key = std::ptr::null();
        }
        iter.public.index = index;

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

// ---- Typed setters on containers -------------------------------------

/// Generate `asdf_mapping_set_<type>` and `asdf_sequence_append_<type>` for
/// one scalar type, along with `asdf_sequence_of_<type>`.
///
/// Each is the composition of the matching `asdf_value_of_<type>` with
/// `asdf_mapping_set` / `asdf_sequence_append`, which is how the C header
/// documents them.
macro_rules! container_setters {
    ($set:ident, $append:ident, $of:ident, $ctor:ident, $ty:ty) => {
        /// Put a scalar into a mapping under `key`.
        ///
        /// # Safety
        /// `mapping` must be a valid handle and `key` a valid string.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $set(
            mapping: *mut AsdfMapping,
            key: *const c_char,
            value: $ty,
        ) -> AsdfValueErr {
            guard(stringify!($set), AsdfValueErr::Unknown, || {
                let Some(file) = value_file(mapping) else {
                    return AsdfValueErr::Unknown;
                };
                let node = unsafe { $ctor(file, value) };
                if node.is_null() {
                    return AsdfValueErr::Unknown;
                }
                let result = unsafe { asdf_mapping_set(mapping, key, node) };
                unsafe { crate::file_ffi::asdf_value_destroy(node) };
                result
            })
        }

        /// Append a scalar to a sequence.
        ///
        /// # Safety
        /// `sequence` must be a valid handle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $append(sequence: *mut AsdfSequence, value: $ty) -> AsdfValueErr {
            guard(stringify!($append), AsdfValueErr::Unknown, || {
                let Some(file) = value_file(sequence) else {
                    return AsdfValueErr::Unknown;
                };
                let node = unsafe { $ctor(file, value) };
                if node.is_null() {
                    return AsdfValueErr::Unknown;
                }
                let result = unsafe { asdf_sequence_append(sequence, node) };
                unsafe { crate::file_ffi::asdf_value_destroy(node) };
                result
            })
        }

        /// Build a sequence from a C array of scalars.
        ///
        /// # Safety
        /// `file` must be a valid file handle and `arr` must point to at
        /// least `size` readable values. The result must be released with
        /// `asdf_sequence_destroy`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $of(
            file: *mut AsdfFile,
            arr: *const $ty,
            size: c_int,
        ) -> *mut AsdfSequence {
            guard(stringify!($of), std::ptr::null_mut(), || {
                if arr.is_null() || size < 0 {
                    return std::ptr::null_mut();
                }
                let sequence = unsafe { asdf_sequence_create(file) };
                if sequence.is_null() {
                    return std::ptr::null_mut();
                }
                let items = unsafe { std::slice::from_raw_parts(arr, size as usize) };
                for value in items {
                    if unsafe { $append(sequence, *value) } != AsdfValueErr::Ok {
                        unsafe { asdf_sequence_destroy(sequence) };
                        return std::ptr::null_mut();
                    }
                }
                sequence
            })
        }
    };
}

container_setters!(
    asdf_mapping_set_int8,
    asdf_sequence_append_int8,
    asdf_sequence_of_int8,
    asdf_value_of_int8,
    i8
);
container_setters!(
    asdf_mapping_set_int16,
    asdf_sequence_append_int16,
    asdf_sequence_of_int16,
    asdf_value_of_int16,
    i16
);
container_setters!(
    asdf_mapping_set_int32,
    asdf_sequence_append_int32,
    asdf_sequence_of_int32,
    asdf_value_of_int32,
    i32
);
container_setters!(
    asdf_mapping_set_int64,
    asdf_sequence_append_int64,
    asdf_sequence_of_int64,
    asdf_value_of_int64,
    i64
);
container_setters!(
    asdf_mapping_set_uint8,
    asdf_sequence_append_uint8,
    asdf_sequence_of_uint8,
    asdf_value_of_uint8,
    u8
);
container_setters!(
    asdf_mapping_set_uint16,
    asdf_sequence_append_uint16,
    asdf_sequence_of_uint16,
    asdf_value_of_uint16,
    u16
);
container_setters!(
    asdf_mapping_set_uint32,
    asdf_sequence_append_uint32,
    asdf_sequence_of_uint32,
    asdf_value_of_uint32,
    u32
);
container_setters!(
    asdf_mapping_set_uint64,
    asdf_sequence_append_uint64,
    asdf_sequence_of_uint64,
    asdf_value_of_uint64,
    u64
);
container_setters!(
    asdf_mapping_set_float,
    asdf_sequence_append_float,
    asdf_sequence_of_float,
    asdf_value_of_float,
    f32
);
container_setters!(
    asdf_mapping_set_double,
    asdf_sequence_append_double,
    asdf_sequence_of_double,
    asdf_value_of_double,
    f64
);
container_setters!(
    asdf_mapping_set_bool,
    asdf_sequence_append_bool,
    asdf_sequence_of_bool,
    asdf_value_of_bool,
    bool
);

/// Put a NUL-terminated string into a mapping.
///
/// # Safety
/// `mapping` must be a valid handle; `key` and `value` valid strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_set_string0(
    mapping: *mut AsdfMapping,
    key: *const c_char,
    value: *const c_char,
) -> AsdfValueErr {
    guard("asdf_mapping_set_string0", AsdfValueErr::Unknown, || {
        let Some(file) = value_file(mapping) else {
            return AsdfValueErr::Unknown;
        };
        let node = unsafe { asdf_value_of_string0(file, value) };
        if node.is_null() {
            return AsdfValueErr::Unknown;
        }
        let result = unsafe { asdf_mapping_set(mapping, key, node) };
        unsafe { crate::file_ffi::asdf_value_destroy(node) };
        result
    })
}

/// Put a counted string into a mapping.
///
/// # Safety
/// `value` must point to at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_set_string(
    mapping: *mut AsdfMapping,
    key: *const c_char,
    value: *const c_char,
    len: usize,
) -> AsdfValueErr {
    guard("asdf_mapping_set_string", AsdfValueErr::Unknown, || {
        let Some(file) = value_file(mapping) else {
            return AsdfValueErr::Unknown;
        };
        let node = unsafe { asdf_value_of_string(file, value, len) };
        if node.is_null() {
            return AsdfValueErr::Unknown;
        }
        let result = unsafe { asdf_mapping_set(mapping, key, node) };
        unsafe { crate::file_ffi::asdf_value_destroy(node) };
        result
    })
}

/// Put a null into a mapping.
///
/// # Safety
/// `mapping` must be a valid handle and `key` a valid string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_set_null(
    mapping: *mut AsdfMapping,
    key: *const c_char,
) -> AsdfValueErr {
    guard("asdf_mapping_set_null", AsdfValueErr::Unknown, || {
        let Some(file) = value_file(mapping) else {
            return AsdfValueErr::Unknown;
        };
        let node = unsafe { asdf_value_of_null(file) };
        if node.is_null() {
            return AsdfValueErr::Unknown;
        }
        let result = unsafe { asdf_mapping_set(mapping, key, node) };
        unsafe { crate::file_ffi::asdf_value_destroy(node) };
        result
    })
}

/// Put a nested mapping into a mapping.
///
/// # Safety
/// Both handles must belong to the same file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_set_mapping(
    mapping: *mut AsdfMapping,
    key: *const c_char,
    value: *mut AsdfMapping,
) -> AsdfValueErr {
    unsafe { asdf_mapping_set(mapping, key, value) }
}

/// Put a sequence into a mapping.
///
/// # Safety
/// Both handles must belong to the same file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_set_sequence(
    mapping: *mut AsdfMapping,
    key: *const c_char,
    value: *mut AsdfSequence,
) -> AsdfValueErr {
    unsafe { asdf_mapping_set(mapping, key, value) }
}

/// Append a NUL-terminated string to a sequence.
///
/// # Safety
/// `sequence` must be a valid handle and `value` a valid string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_append_string0(
    sequence: *mut AsdfSequence,
    value: *const c_char,
) -> AsdfValueErr {
    guard("asdf_sequence_append_string0", AsdfValueErr::Unknown, || {
        let Some(file) = value_file(sequence) else {
            return AsdfValueErr::Unknown;
        };
        let node = unsafe { asdf_value_of_string0(file, value) };
        if node.is_null() {
            return AsdfValueErr::Unknown;
        }
        let result = unsafe { asdf_sequence_append(sequence, node) };
        unsafe { crate::file_ffi::asdf_value_destroy(node) };
        result
    })
}

/// Append a counted string to a sequence.
///
/// # Safety
/// `value` must point to at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_append_string(
    sequence: *mut AsdfSequence,
    value: *const c_char,
    len: usize,
) -> AsdfValueErr {
    guard("asdf_sequence_append_string", AsdfValueErr::Unknown, || {
        let Some(file) = value_file(sequence) else {
            return AsdfValueErr::Unknown;
        };
        let node = unsafe { asdf_value_of_string(file, value, len) };
        if node.is_null() {
            return AsdfValueErr::Unknown;
        }
        let result = unsafe { asdf_sequence_append(sequence, node) };
        unsafe { crate::file_ffi::asdf_value_destroy(node) };
        result
    })
}

/// Append a null to a sequence.
///
/// # Safety
/// `sequence` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_append_null(sequence: *mut AsdfSequence) -> AsdfValueErr {
    guard("asdf_sequence_append_null", AsdfValueErr::Unknown, || {
        let Some(file) = value_file(sequence) else {
            return AsdfValueErr::Unknown;
        };
        let node = unsafe { asdf_value_of_null(file) };
        if node.is_null() {
            return AsdfValueErr::Unknown;
        }
        let result = unsafe { asdf_sequence_append(sequence, node) };
        unsafe { crate::file_ffi::asdf_value_destroy(node) };
        result
    })
}

/// Append a mapping to a sequence.
///
/// # Safety
/// Both handles must belong to the same file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_append_mapping(
    sequence: *mut AsdfSequence,
    value: *mut AsdfMapping,
) -> AsdfValueErr {
    unsafe { asdf_sequence_append(sequence, value) }
}

/// Append a nested sequence to a sequence.
///
/// # Safety
/// Both handles must belong to the same file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_append_sequence(
    sequence: *mut AsdfSequence,
    value: *mut AsdfSequence,
) -> AsdfValueErr {
    unsafe { asdf_sequence_append(sequence, value) }
}

/// Build a sequence of nulls.
///
/// # Safety
/// `file` must be a valid file handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_of_null(
    file: *mut AsdfFile,
    size: c_int,
) -> *mut AsdfSequence {
    guard("asdf_sequence_of_null", std::ptr::null_mut(), || {
        if size < 0 {
            return std::ptr::null_mut();
        }
        let sequence = unsafe { asdf_sequence_create(file) };
        if sequence.is_null() {
            return std::ptr::null_mut();
        }
        for _ in 0..size {
            if unsafe { asdf_sequence_append_null(sequence) } != AsdfValueErr::Ok {
                unsafe { asdf_sequence_destroy(sequence) };
                return std::ptr::null_mut();
            }
        }
        sequence
    })
}

/// Build a sequence from an array of NUL-terminated strings.
///
/// # Safety
/// `arr` must point to at least `size` valid string pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_of_string0(
    file: *mut AsdfFile,
    arr: *const *const c_char,
    size: c_int,
) -> *mut AsdfSequence {
    guard("asdf_sequence_of_string0", std::ptr::null_mut(), || {
        if arr.is_null() || size < 0 {
            return std::ptr::null_mut();
        }
        let sequence = unsafe { asdf_sequence_create(file) };
        if sequence.is_null() {
            return std::ptr::null_mut();
        }
        for index in 0..size as isize {
            let text = unsafe { *arr.offset(index) };
            if unsafe { asdf_sequence_append_string0(sequence, text) } != AsdfValueErr::Ok {
                unsafe { asdf_sequence_destroy(sequence) };
                return std::ptr::null_mut();
            }
        }
        sequence
    })
}

/// Build a sequence from an array of counted strings.
///
/// # Safety
/// `arr` and `lens` must each point to at least `size` readable entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_sequence_of_string(
    file: *mut AsdfFile,
    arr: *const *const c_char,
    lens: *const usize,
    size: c_int,
) -> *mut AsdfSequence {
    guard("asdf_sequence_of_string", std::ptr::null_mut(), || {
        if arr.is_null() || lens.is_null() || size < 0 {
            return std::ptr::null_mut();
        }
        let sequence = unsafe { asdf_sequence_create(file) };
        if sequence.is_null() {
            return std::ptr::null_mut();
        }
        for index in 0..size as isize {
            let text = unsafe { *arr.offset(index) };
            let len = unsafe { *lens.offset(index) };
            if unsafe { asdf_sequence_append_string(sequence, text, len) } != AsdfValueErr::Ok {
                unsafe { asdf_sequence_destroy(sequence) };
                return std::ptr::null_mut();
            }
        }
        sequence
    })
}

/// Copy a mapping's entries into a new mapping.
///
/// A shallow copy: the entries refer to the same value nodes.
///
/// # Safety
/// `mapping` must be a valid handle. The result must be released with
/// `asdf_mapping_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_copy(mapping: *mut AsdfMapping) -> *mut AsdfMapping {
    guard("asdf_mapping_copy", std::ptr::null_mut(), || {
        let (Some(file), Some(source)) = (value_file(mapping), value_node(mapping)) else {
            return std::ptr::null_mut();
        };
        let handle = unsafe { &mut *file };
        let Some(doc) = handle.document_for_values() else {
            return std::ptr::null_mut();
        };
        let Some(entries) = doc.mapping_entries(source).map(<[_]>::to_vec) else {
            return std::ptr::null_mut();
        };
        let pairs: Vec<_> = entries.iter().map(|e| (e.key, e.value)).collect();
        let fresh = doc.add_mapping(pairs);
        make_value(file, fresh)
    })
}

/// Merge one mapping's entries into another.
///
/// Existing keys are replaced and new ones appended, in the update's order.
///
/// # Safety
/// Both handles must be valid and belong to the same file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_mapping_update(
    mapping: *mut AsdfMapping,
    update: *mut AsdfMapping,
) -> AsdfValueErr {
    guard("asdf_mapping_update", AsdfValueErr::Unknown, || {
        let (Some(file), Some(target)) = (value_file(mapping), value_node(mapping)) else {
            return AsdfValueErr::Unknown;
        };
        let Some(source) = value_node(update) else {
            return AsdfValueErr::Unknown;
        };
        let handle = unsafe { &mut *file };
        let Some(doc) = handle.document_for_values() else {
            return AsdfValueErr::Unknown;
        };
        if !doc.resolved(target).is_mapping() || !doc.resolved(source).is_mapping() {
            return AsdfValueErr::TypeMismatch;
        }

        let entries = doc.mapping_entries(source).map(<[_]>::to_vec).unwrap_or_default();
        for entry in entries {
            let Some(key) = doc.resolved(entry.key).as_str().map(str::to_string) else {
                continue;
            };
            doc.mapping_set(target, &key, entry.value);
        }
        AsdfValueErr::Ok
    })
}

// ---- Counted-string and generic accessors ----------------------------

/// Hand out a scalar's text and its length.
fn scalar_with_len(
    value: *mut AsdfValue,
    out: *mut *const c_char,
    out_len: *mut usize,
) -> AsdfValueErr {
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
    if !out_len.is_null() {
        unsafe { *out_len = text.len() };
    }
    AsdfValueErr::Ok
}

/// Read the value as a counted string.
///
/// The text is NUL-terminated as well, so `out_len` is a convenience rather
/// than the only way to know where it ends.
///
/// # Safety
/// `value` must be null or a valid value handle; `out` and `out_len` writable
/// or null. The string is owned by the value's file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_string(
    value: *mut AsdfValue,
    out: *mut *const c_char,
    out_len: *mut usize,
) -> AsdfValueErr {
    guard("asdf_value_as_string", AsdfValueErr::Unknown, || {
        if !matches!(resolved_of(value), Some(Resolved::String)) {
            return AsdfValueErr::TypeMismatch;
        }
        scalar_with_len(value, out, out_len)
    })
}

/// Read a scalar's raw text and length, whatever its resolved type.
///
/// # Safety
/// See [`asdf_value_as_string`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_scalar(
    value: *mut AsdfValue,
    out: *mut *const c_char,
    out_len: *mut usize,
) -> AsdfValueErr {
    guard("asdf_value_as_scalar", AsdfValueErr::Unknown, || {
        // See `asdf_value_as_scalar0`.
        if value.is_null() {
            return AsdfValueErr::Unknown;
        }
        if !unsafe { asdf_value_is_scalar(value) } {
            return AsdfValueErr::TypeMismatch;
        }
        scalar_with_len(value, out, out_len)
    })
}

/// Read a value as the type named by `value_type`.
///
/// `out` points at storage of the C type matching `value_type`; for
/// `ASDF_VALUE_STRING` and `ASDF_VALUE_SCALAR` that is a `const char *`,
/// holding a NUL-terminated string.
///
/// # Safety
/// `out` must point at writable storage of the right type and size for
/// `value_type`, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_type(
    value: *mut AsdfValue,
    value_type: c_int,
    out: *mut std::ffi::c_void,
) -> AsdfValueErr {
    guard("asdf_value_as_type", AsdfValueErr::Unknown, || unsafe {
        // See `asdf_value_is_type` on why this arrives as an `int`.
        let Some(value_type) = AsdfValueType::from_i32(value_type) else {
            return AsdfValueErr::TypeMismatch;
        };
        // `Unknown` names no type, so the request is for the value itself:
        // hand back a copy the caller destroys.
        if value_type == AsdfValueType::Unknown {
            if value.is_null() {
                return AsdfValueErr::Unknown;
            }
            let copy = asdf_value_copy(value);
            if copy.is_null() {
                return AsdfValueErr::Oom;
            }
            if out.is_null() {
                crate::file_ffi::asdf_value_destroy(copy);
            } else {
                *out.cast::<*mut AsdfValue>() = copy;
            }
            return AsdfValueErr::Ok;
        }
        match value_type {
            AsdfValueType::Int8 => asdf_value_as_int8(value, out.cast()),
            AsdfValueType::Int16 => asdf_value_as_int16(value, out.cast()),
            AsdfValueType::Int32 => asdf_value_as_int32(value, out.cast()),
            AsdfValueType::Int64 => asdf_value_as_int64(value, out.cast()),
            AsdfValueType::Uint8 => asdf_value_as_uint8(value, out.cast()),
            AsdfValueType::Uint16 => asdf_value_as_uint16(value, out.cast()),
            AsdfValueType::Uint32 => asdf_value_as_uint32(value, out.cast()),
            AsdfValueType::Uint64 => asdf_value_as_uint64(value, out.cast()),
            AsdfValueType::Float => asdf_value_as_float(value, out.cast()),
            AsdfValueType::Double => asdf_value_as_double(value, out.cast()),
            AsdfValueType::Bool => asdf_value_as_bool(value, out.cast()),
            AsdfValueType::String => asdf_value_as_string0(value, out.cast()),
            AsdfValueType::Scalar => asdf_value_as_scalar0(value, out.cast()),
            AsdfValueType::Mapping => asdf_value_as_mapping(value, out.cast()),
            AsdfValueType::Sequence => asdf_value_as_sequence(value, out.cast()),
            // Null carries no data: report only whether it matches.
            AsdfValueType::Null => {
                if asdf_value_is_null(value) {
                    AsdfValueErr::Ok
                } else {
                    AsdfValueErr::TypeMismatch
                }
            }
            AsdfValueType::Unknown | AsdfValueType::Extension => AsdfValueErr::TypeMismatch,
        }
    })
}

/// View a mapping as a generic value.
///
/// The two share a representation, so this is the identity; it exists for
/// type-checking on the C side.
///
/// # Safety
/// `mapping` must be null or a valid mapping handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_mapping(mapping: *mut AsdfMapping) -> *mut AsdfValue {
    mapping
}

/// View a sequence as a generic value. See [`asdf_value_of_mapping`].
///
/// # Safety
/// `sequence` must be null or a valid sequence handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_sequence(sequence: *mut AsdfSequence) -> *mut AsdfValue {
    sequence
}

/// The YAML-pointer path of a value within its document.
///
/// Null for a value that is not reachable from the root -- one built with
/// `asdf_value_of_*` and not yet attached, for instance.
///
/// # Safety
/// `value` must be null or a valid value handle. The string is owned by the
/// value's file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_path(value: *mut AsdfValue) -> *const c_char {
    guard("asdf_value_path", std::ptr::null(), || {
        let (Some(doc), Some(node), Some(file)) =
            (value_document(value), value_node(value), value_file(value))
        else {
            return std::ptr::null();
        };
        match doc.path_of(node) {
            Some(path) => unsafe { &*file }.intern(&path),
            None => std::ptr::null(),
        }
    })
}

/// The container holding a value, or null for the root or a detached value.
///
/// # Safety
/// `value` must be null or a valid value handle. The result must be released
/// with `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_parent(value: *mut AsdfValue) -> *mut AsdfValue {
    guard("asdf_value_parent", std::ptr::null_mut(), || {
        let (Some(doc), Some(node), Some(file)) =
            (value_document(value), value_node(value), value_file(value))
        else {
            return std::ptr::null_mut();
        };
        match doc.parent_of(node) {
            Some(parent) => make_value(file, parent),
            None => std::ptr::null_mut(),
        }
    })
}

// ---- Tree traversal --------------------------------------------------

/// A predicate over a value, as C passes it in.
pub type AsdfValuePred = Option<unsafe extern "C" fn(*mut AsdfValue) -> bool>;

/// A find iterator. The public head must stay first; see [`MappingIter`].
#[repr(C)]
struct FindIter {
    /// The public head, which C casts to.
    public: crate::types::asdf_find_iter_t,
    file: *mut AsdfFile,
    /// Nodes still to visit, each with the depth at which it was reached.
    queue: std::collections::VecDeque<(NodeId, isize)>,
    pred: AsdfValuePred,
    descend_pred: AsdfValuePred,
    depth_first: bool,
    max_depth: isize,
    /// Nodes already queued, so an aliased subtree is visited once.
    seen: Vec<NodeId>,
}

impl FindIter {
    /// Push a node's children in the order the traversal wants them.
    fn enqueue_children(&mut self, doc: &asdf_core::yaml::Document, node: NodeId, depth: isize) {
        // `max_depth` counts containers entered *below* the root, so a
        // container at depth `d` may be opened while `d <= max_depth`: with
        // a limit of 1 the root's children are visited and one container
        // among them is entered, but nothing inside that one.
        if self.max_depth >= 0 && depth > self.max_depth {
            return;
        }
        let resolved = doc.resolve(node);
        let mut children: Vec<NodeId> = Vec::new();
        match &doc.node(resolved).data {
            NodeData::Mapping { entries, .. } => {
                children.extend(entries.iter().map(|e| e.value));
            }
            NodeData::Sequence { items, .. } => children.extend(items.iter().copied()),
            _ => return,
        }
        if self.depth_first {
            // Pushed onto the front, so reverse to keep document order.
            for child in children.into_iter().rev() {
                self.queue.push_front((child, depth + 1));
            }
        } else {
            for child in children {
                self.queue.push_back((child, depth + 1));
            }
        }
    }

    /// Whether the traversal should descend into this container.
    ///
    /// The search's own root is always descended: `descend_pred` selects
    /// which containers *found along the way* are entered, and refusing the
    /// root would make a search from a mapping with
    /// `asdf_find_descend_sequence_only` find nothing at all.
    fn should_descend(&self, node: NodeId, is_root: bool) -> bool {
        if is_root {
            return true;
        }
        let Some(pred) = self.descend_pred else {
            return true;
        };
        // The predicate takes a value handle, so one is made for the call and
        // released straight after.
        let handle = make_value(self.file, node);
        let verdict = unsafe { pred(handle) };
        unsafe { crate::file_ffi::asdf_value_destroy(handle) };
        verdict
    }

    /// Release the value handed out for the previous step.
    fn clear_current(&mut self) {
        if !self.public.value.is_null() {
            unsafe { crate::file_ffi::asdf_value_destroy(self.public.value.cast::<AsdfValue>()) };
            self.public.value = std::ptr::null_mut();
        }
    }

    /// Advance to the next match, or `false` when the traversal is done.
    fn step(&mut self) -> bool {
        self.clear_current();
        let Some(doc) = crate::file_ffi::file_document(self.file) else {
            return false;
        };
        while let Some((node, depth)) = self.queue.pop_front() {
            let resolved = doc.resolve(node);
            if self.seen.contains(&resolved) {
                continue;
            }
            self.seen.push(resolved);

            let is_container = doc.node(resolved).is_mapping() || doc.node(resolved).is_sequence();
            if is_container && self.should_descend(node, depth == 0) {
                self.enqueue_children(doc, node, depth);
            }

            let matched = match self.pred {
                Some(pred) => {
                    let handle = make_value(self.file, node);
                    let verdict = unsafe { pred(handle) };
                    if verdict {
                        self.public.value = handle.cast();
                        return true;
                    }
                    unsafe { crate::file_ffi::asdf_value_destroy(handle) };
                    false
                }
                // A null predicate matches everything, as C's convention has
                // it for an omitted filter.
                None => {
                    self.public.value = make_value(self.file, node).cast();
                    return true;
                }
            };
            let _ = matched;
        }
        false
    }
}

/// Build a find iterator over `root`.
fn find_iter_new(
    root: *mut AsdfValue,
    pred: AsdfValuePred,
    depth_first: bool,
    descend_pred: AsdfValuePred,
    max_depth: isize,
) -> *mut FindIter {
    let (Some(file), Some(node)) = (value_file(root), value_node(root)) else {
        return std::ptr::null_mut();
    };
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((node, 0isize));
    Box::into_raw(Box::new(FindIter {
        public: crate::types::asdf_find_iter_t { value: std::ptr::null_mut() },
        file,
        queue,
        pred,
        descend_pred,
        depth_first,
        max_depth,
        seen: Vec::new(),
    }))
}

/// Find the first value at or below `root` matching `pred`, breadth-first.
///
/// # Safety
/// `root` must be a valid value handle. The result must be released with
/// `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_find(
    root: *mut AsdfValue,
    pred: AsdfValuePred,
) -> *mut AsdfValue {
    unsafe { asdf_value_find_ex(root, pred, false, None, -1) }
}

/// Find the first match with control over traversal order and depth.
///
/// `depth_first` selects the order, `descend_pred` filters which containers
/// are entered (null enters all), and `max_depth` of -1 means no limit.
///
/// # Safety
/// See [`asdf_value_find`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_find_ex(
    root: *mut AsdfValue,
    pred: AsdfValuePred,
    depth_first: bool,
    descend_pred: AsdfValuePred,
    max_depth: isize,
) -> *mut AsdfValue {
    guard("asdf_value_find_ex", std::ptr::null_mut(), || {
        let iter = find_iter_new(root, pred, depth_first, descend_pred, max_depth);
        if iter.is_null() {
            return std::ptr::null_mut();
        }
        let mut boxed = unsafe { Box::from_raw(iter) };
        if !boxed.step() {
            return std::ptr::null_mut();
        }
        // Hand the match to the caller rather than letting the drop free it.
        let found = boxed.public.value.cast::<AsdfValue>();
        boxed.public.value = std::ptr::null_mut();
        found
    })
}

/// Start a breadth-first search yielding every value matching `pred`.
///
/// # Safety
/// `root` must be a valid container value handle. The iterator is released by
/// running it to exhaustion or with [`asdf_find_iter_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_find_iter_init(
    root: *mut AsdfValue,
    pred: AsdfValuePred,
) -> *mut crate::types::asdf_find_iter_t {
    unsafe { asdf_find_iter_init_ex(root, pred, false, None, -1) }
}

/// Start a search with control over traversal order and depth.
///
/// # Safety
/// See [`asdf_find_iter_init`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_find_iter_init_ex(
    root: *mut AsdfValue,
    pred: AsdfValuePred,
    depth_first: bool,
    descend_pred: AsdfValuePred,
    max_depth: isize,
) -> *mut crate::types::asdf_find_iter_t {
    guard("asdf_find_iter_init_ex", std::ptr::null_mut(), || {
        find_iter_new(root, pred, depth_first, descend_pred, max_depth)
            .cast::<crate::types::asdf_find_iter_t>()
    })
}

/// Advance a find iterator.
///
/// On exhaustion the iterator is freed and `*iter` set to null, so the
/// trailing `destroy` only matters when the loop breaks early.
///
/// # Safety
/// `iter` must point at a handle from [`asdf_find_iter_init`], or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_find_iter_next(
    iter: *mut *mut crate::types::asdf_find_iter_t,
) -> bool {
    guard("asdf_value_find_iter_next", false, || {
        if iter.is_null() {
            return false;
        }
        let current = unsafe { *iter };
        if current.is_null() {
            return false;
        }
        let state = unsafe { &mut *current.cast::<FindIter>() };
        if state.step() {
            return true;
        }
        unsafe { asdf_find_iter_destroy(current) };
        unsafe { *iter = std::ptr::null_mut() };
        false
    })
}

/// Release an iterator abandoned before exhaustion.
///
/// # Safety
/// `iter` must be null or a handle from [`asdf_find_iter_init`] that has not
/// already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_find_iter_destroy(iter: *mut crate::types::asdf_find_iter_t) {
    guard("asdf_find_iter_destroy", (), || {
        if iter.is_null() {
            return;
        }
        let mut boxed = unsafe { Box::from_raw(iter.cast::<FindIter>()) };
        boxed.clear_current();
    })
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

    /// A predicate for the find tests: matches any string scalar.
    unsafe extern "C" fn is_a_string(value: *mut AsdfValue) -> bool {
        unsafe { asdf_value_is_string(value) }
    }

    /// Matches the scalar `deep`, which sits two levels down.
    unsafe extern "C" fn is_deep(value: *mut AsdfValue) -> bool {
        let mut out = std::ptr::null();
        if unsafe { asdf_value_as_string0(value, &mut out) } != AsdfValueErr::Ok {
            return false;
        }
        let text = unsafe { CStr::from_ptr(out) };
        text == c"deep"
    }

    #[test]
    fn find_walks_breadth_first_by_default() {
        let h = open();
        let root = value_at(&h, "");
        // `b: two` is at depth 1; `nested/inner: deep` at depth 2. A
        // breadth-first walk reaches the shallower one first.
        let found = unsafe { asdf_value_find(root, Some(is_a_string)) };
        assert!(!found.is_null());
        let mut text = std::ptr::null();
        assert_eq!(unsafe { asdf_value_as_string0(found, &mut text) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { CStr::from_ptr(text) }, c"two");
        unsafe { asdf_value_destroy(found) };
        unsafe { asdf_value_destroy(root) };
    }

    /// `max_depth` counts containers entered *below* the root: with a limit
    /// of 0 the root's own children are visited and nothing under them.
    #[test]
    fn find_respects_max_depth() {
        let h = open();
        let root = value_at(&h, "");

        // `deep` lives inside `nested`, so entering `nested` is the one
        // descent a limit of 0 forbids.
        let shallow = unsafe { asdf_value_find_ex(root, Some(is_deep), false, None, 0) };
        assert!(shallow.is_null());

        // A limit of 1 allows exactly that descent.
        let found = unsafe { asdf_value_find_ex(root, Some(is_deep), false, None, 1) };
        assert!(!found.is_null());
        unsafe { asdf_value_destroy(found) };

        let deep = unsafe { asdf_value_find_ex(root, Some(is_deep), false, None, -1) };
        assert!(!deep.is_null());
        unsafe { asdf_value_destroy(deep) };
        unsafe { asdf_value_destroy(root) };
    }

    #[test]
    fn find_iterates_every_match() {
        let h = open();
        let root = value_at(&h, "");

        let mut iter = unsafe { asdf_find_iter_init(root, Some(is_a_string)) };
        let mut seen = Vec::new();
        while unsafe { asdf_value_find_iter_next(&mut iter) } {
            let current = unsafe { &*iter }.value.cast::<AsdfValue>();
            let mut text = std::ptr::null();
            assert_eq!(unsafe { asdf_value_as_string0(current, &mut text) }, AsdfValueErr::Ok);
            seen.push(unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned());
        }
        // The iterator frees itself on exhaustion.
        assert!(iter.is_null());

        // Keys are not visited, only values: `two` and `deep`.
        assert_eq!(seen, vec!["two".to_string(), "deep".to_string()]);
        unsafe { asdf_value_destroy(root) };
    }

    #[test]
    fn abandoning_a_find_iterator_is_safe() {
        let h = open();
        let root = value_at(&h, "");
        let mut iter = unsafe { asdf_find_iter_init(root, None) };
        assert!(unsafe { asdf_value_find_iter_next(&mut iter) });
        // Break out early, which is exactly when `destroy` has work to do.
        unsafe { asdf_find_iter_destroy(iter) };
        unsafe { asdf_value_destroy(root) };
    }

    #[test]
    fn values_report_their_path_and_parent() {
        let h = open();
        let inner = value_at(&h, "nested/inner");
        // Paths are absolute, as libasdf reports them.
        let path = unsafe { asdf_value_path(inner) };
        assert!(!path.is_null());
        assert_eq!(unsafe { CStr::from_ptr(path) }, c"/nested/inner");

        let parent = unsafe { asdf_value_parent(inner) };
        assert!(!parent.is_null());
        let parent_path = unsafe { asdf_value_path(parent) };
        assert_eq!(unsafe { CStr::from_ptr(parent_path) }, c"/nested");

        // The root has no parent, and its path is `/`.
        let root = value_at(&h, "");
        assert!(unsafe { asdf_value_parent(root) }.is_null());
        assert_eq!(unsafe { CStr::from_ptr(asdf_value_path(root)) }, c"/");

        for v in [inner, parent, root] {
            unsafe { asdf_value_destroy(v) };
        }
    }

    /// A sequence index is written plainly, not bracketed: that is the form
    /// libasdf reports, and it reads back as an index because the container
    /// it addresses is a sequence.
    #[test]
    fn sequence_elements_report_plain_indices() {
        let h = open();
        let item = value_at(&h, "list/1");
        assert_eq!(unsafe { CStr::from_ptr(asdf_value_path(item)) }, c"/list/1");

        // And the reported path finds the same value again.
        let again = value_at(&h, "/list/1");
        assert_eq!(value_node(again), value_node(item));

        unsafe { asdf_value_destroy(again) };
        unsafe { asdf_value_destroy(item) };
    }

    #[test]
    fn counted_string_accessors_report_lengths() {
        let h = open();
        let b = value_at(&h, "b");
        let mut text = std::ptr::null();
        let mut len = 0usize;
        assert_eq!(unsafe { asdf_value_as_string(b, &mut text, &mut len) }, AsdfValueErr::Ok);
        assert_eq!(len, 3);
        assert_eq!(unsafe { CStr::from_ptr(text) }, c"two");

        // `as_scalar` works on a value that is not a string, `as_string` does
        // not.
        let a = value_at(&h, "a");
        assert_eq!(
            unsafe { asdf_value_as_string(a, &mut text, &mut len) },
            AsdfValueErr::TypeMismatch
        );
        assert_eq!(unsafe { asdf_value_as_scalar(a, &mut text, &mut len) }, AsdfValueErr::Ok);
        assert_eq!(len, 1);
        assert_eq!(unsafe { CStr::from_ptr(text) }, c"1");

        for v in [a, b] {
            unsafe { asdf_value_destroy(v) };
        }
    }

    #[test]
    fn as_type_dispatches_on_the_requested_type() {
        let h = open();
        let a = value_at(&h, "a");
        let mut narrow: i32 = 0;
        assert_eq!(
            unsafe {
                asdf_value_as_type(
                    a,
                    AsdfValueType::Int32 as c_int,
                    std::ptr::from_mut(&mut narrow).cast(),
                )
            },
            AsdfValueErr::Ok
        );
        assert_eq!(narrow, 1);

        // A string request against an integer is a type mismatch.
        let mut text = std::ptr::null::<c_char>();
        assert_eq!(
            unsafe {
                asdf_value_as_type(
                    a,
                    AsdfValueType::String as c_int,
                    std::ptr::from_mut(&mut text).cast(),
                )
            },
            AsdfValueErr::TypeMismatch
        );

        let nothing = value_at(&h, "nothing");
        assert_eq!(
            unsafe {
                asdf_value_as_type(nothing, AsdfValueType::Null as c_int, std::ptr::null_mut())
            },
            AsdfValueErr::Ok
        );

        for v in [a, nothing] {
            unsafe { asdf_value_destroy(v) };
        }
    }

    #[test]
    fn typed_container_setters_build_a_tree() {
        use crate::file_ffi::asdf_value_destroy as destroy;

        // `asdf_open(NULL)` -- a new, empty file open for writing.
        let file = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        assert!(!file.is_null());
        let handle = Handle(file);

        let mapping = unsafe { asdf_mapping_create(handle.0) };
        assert!(!mapping.is_null());
        let key = CString::new("count").unwrap();
        assert_eq!(unsafe { asdf_mapping_set_int32(mapping, key.as_ptr(), -7) }, AsdfValueErr::Ok);
        let name = CString::new("name").unwrap();
        let value = CString::new("probe").unwrap();
        assert_eq!(
            unsafe { asdf_mapping_set_string0(mapping, name.as_ptr(), value.as_ptr()) },
            AsdfValueErr::Ok
        );
        let missing = CString::new("missing").unwrap();
        assert_eq!(unsafe { asdf_mapping_set_null(mapping, missing.as_ptr()) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { asdf_mapping_size(mapping) }, 3);

        let read_back = unsafe { asdf_mapping_get(mapping, key.as_ptr()) };
        assert!(!read_back.is_null());
        let mut got: i32 = 0;
        assert_eq!(unsafe { asdf_value_as_int32(read_back, &mut got) }, AsdfValueErr::Ok);
        assert_eq!(got, -7);
        unsafe { destroy(read_back) };

        let numbers: [f64; 3] = [1.5, 2.5, 3.5];
        let sequence = unsafe { asdf_sequence_of_double(handle.0, numbers.as_ptr(), 3) };
        assert!(!sequence.is_null());
        assert_eq!(unsafe { asdf_sequence_size(sequence) }, 3);
        let second = unsafe { asdf_sequence_get(sequence, 1) };
        let mut d = 0.0f64;
        assert_eq!(unsafe { asdf_value_as_double(second, &mut d) }, AsdfValueErr::Ok);
        assert!((d - 2.5).abs() < f64::EPSILON);
        unsafe { destroy(second) };

        assert_eq!(unsafe { asdf_sequence_append_bool(sequence, true) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { asdf_sequence_size(sequence) }, 4);

        unsafe { destroy(sequence) };
        unsafe { destroy(mapping) };
    }

    #[test]
    fn mapping_update_merges_and_replaces() {
        use crate::file_ffi::asdf_value_destroy as destroy;

        let file = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let handle = Handle(file);

        let target = unsafe { asdf_mapping_create(handle.0) };
        let update = unsafe { asdf_mapping_create(handle.0) };
        let shared = CString::new("shared").unwrap();
        let only_target = CString::new("target-only").unwrap();
        let only_update = CString::new("update-only").unwrap();

        unsafe { asdf_mapping_set_int32(target, shared.as_ptr(), 1) };
        unsafe { asdf_mapping_set_int32(target, only_target.as_ptr(), 2) };
        unsafe { asdf_mapping_set_int32(update, shared.as_ptr(), 99) };
        unsafe { asdf_mapping_set_int32(update, only_update.as_ptr(), 3) };

        assert_eq!(unsafe { asdf_mapping_update(target, update) }, AsdfValueErr::Ok);
        assert_eq!(unsafe { asdf_mapping_size(target) }, 3);

        let merged = unsafe { asdf_mapping_get(target, shared.as_ptr()) };
        let mut got: i32 = 0;
        assert_eq!(unsafe { asdf_value_as_int32(merged, &mut got) }, AsdfValueErr::Ok);
        assert_eq!(got, 99, "the update's value should replace the target's");
        unsafe { destroy(merged) };

        // A shallow copy carries the same entries.
        let copy = unsafe { asdf_mapping_copy(target) };
        assert!(!copy.is_null());
        assert_eq!(unsafe { asdf_mapping_size(copy) }, 3);

        for v in [copy, update, target] {
            unsafe { destroy(v) };
        }
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
            // A mapping's entries are numbered too: the index is the
            // position in the container, not a sequence-only field.
            assert_eq!(head.index, seen.len() as c_int);
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
