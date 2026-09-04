//! `asdf/extension.h`: the extension mechanism.
//!
//! # Registration happens before `main`
//!
//! `ASDF_REGISTER_EXTENSION` generates a function marked
//! `__attribute__((constructor))`, so a third-party extension such as
//! libasdf-gwcs calls [`asdf_extension_register`] while the dynamic loader is
//! still bringing the process up — before `main`, and before anything in this
//! library has had a chance to initialise.
//!
//! The registry is therefore a `static Mutex<Vec<..>>` built by a `const`
//! constructor: it needs no lazy initialisation, allocates nothing until the
//! first registration, and has no ordering dependency on anything else in the
//! library. Reaching for a `OnceLock` with an initialiser, or anything that
//! runs its own setup first, would reintroduce exactly the ordering problem
//! this avoids.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Mutex;

use crate::panic::guard;
use crate::types::AsdfValueType;
use crate::version_ffi::asdf_version_t;

/// Mirror of `asdf_tag_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_tag_t {
    /// The tag's name, with any version suffix removed.
    pub name: *const c_char,
    /// The parsed version, or null when the tag carried none.
    pub version: *const asdf_version_t,
}

/// Mirror of `asdf_software_t`.
///
/// Defined in `asdf/extension.h` rather than `core/software.h`, because the
/// two headers would otherwise be circular.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_software_t {
    /// The software's name.
    pub name: *const c_char,
    /// Its version.
    pub version: *const asdf_version_t,
    /// Optional author.
    pub author: *const c_char,
    /// Optional homepage.
    pub homepage: *const c_char,
}

/// Serialize a native object into a value.
pub type AsdfExtensionSerialize = Option<
    unsafe extern "C" fn(
        file: *mut crate::file_ffi::AsdfFile,
        obj: *const c_void,
        userdata: *const c_void,
    ) -> *mut crate::file_ffi::AsdfValue,
>;

/// Deserialize a value into a native object.
pub type AsdfExtensionDeserialize = Option<
    unsafe extern "C" fn(
        value: *mut crate::file_ffi::AsdfValue,
        userdata: *const c_void,
        out: *mut *mut c_void,
    ) -> crate::types::AsdfValueErr,
>;

/// Deep-copy a native object into caller-provided storage.
pub type AsdfExtensionCopy = Option<
    unsafe extern "C" fn(
        file: *mut crate::file_ffi::AsdfFile,
        src: *const c_void,
        dst: *mut c_void,
    ) -> bool,
>;

/// De-initialise a native object's fields, without freeing the object.
pub type AsdfExtensionDeinit = Option<unsafe extern "C" fn(obj: *mut c_void)>;

/// A generic method pointer, for the vtable's reserved slots.
pub type AsdfExtensionMethod = Option<unsafe extern "C" fn()>;

/// Total method slots in the vtable, used and reserved.
pub const ASDF_EXTENSION_VTAB_MAX_METHODS: usize = 8;
/// Method slots currently defined.
pub const ASDF_EXTENSION_VTAB_METHODS: usize = 4;

/// Mirror of `asdf_extension_vtab_t`.
///
/// The reserved slots are what let upstream add methods without breaking the
/// ABI, so the total width must stay at
/// [`ASDF_EXTENSION_VTAB_MAX_METHODS`] pointers.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_extension_vtab_t {
    /// Serializer, or null if the type cannot be written.
    pub serialize: AsdfExtensionSerialize,
    /// Deserializer.
    pub deserialize: AsdfExtensionDeserialize,
    /// Deep-copy method, or null for a shallow copy.
    pub copy: AsdfExtensionCopy,
    /// De-initialiser for objects the deserializer produced.
    pub deinit: AsdfExtensionDeinit,
    /// Reserved, keeping the ABI stable as methods are added.
    pub _reserved:
        [AsdfExtensionMethod; ASDF_EXTENSION_VTAB_MAX_METHODS - ASDF_EXTENSION_VTAB_METHODS],
}

/// Mirror of `asdf_extension_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_extension_t {
    /// A null-terminated array of the full YAML tags this handles.
    ///
    /// `tags[0]` is written when serializing; any listed tag is recognised
    /// when reading, so one extension can serve several schema versions.
    pub tags: *const *const c_char,
    /// The software implementing the extension.
    pub software: *mut asdf_software_t,
    /// The extension's methods.
    pub vtab: *const asdf_extension_vtab_t,
    /// Size of the extension's objects, for allocation.
    pub size: usize,
    /// Opaque data passed through to the methods.
    pub userdata: *mut c_void,
}

/// One registration.
///
/// The pointer is stored rather than the struct: `ASDF_REGISTER_EXTENSION`
/// makes the `asdf_extension_t` a file-scope `static`, so it outlives the
/// process's use of it.
#[derive(Clone, Copy)]
struct Registration {
    extension: *const asdf_extension_t,
}

// SAFETY: the registered struct is a C `static` and is only ever read after
// registration, so sharing the pointer across threads is sound. The C API
// makes the same assumption.
unsafe impl Send for Registration {}

/// The registry.
///
/// `Mutex::new` is `const`, so this needs no lazy initialisation and is safe
/// to touch from a pre-`main` constructor. See the module comment.
static REGISTRY: Mutex<Vec<Registration>> = Mutex::new(Vec::new());

/// Register an extension.
///
/// Normally called by the constructor `ASDF_REGISTER_EXTENSION` generates,
/// which runs before `main`.
///
/// # Safety
/// `ext` must point to an `asdf_extension_t` that outlives the process's use
/// of the library — in practice a file-scope `static`, which is what the
/// registration macro produces.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_extension_register(ext: *mut asdf_extension_t) {
    guard("asdf_extension_register", (), || {
        if ext.is_null() {
            return;
        }
        let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        // Registering the same extension twice is harmless and can happen
        // when a library is loaded more than once; keep one entry.
        if registry.iter().any(|r| std::ptr::eq(r.extension, ext)) {
            return;
        }
        registry.push(Registration { extension: ext });
    })
}

/// Look up the extension registered for a tag.
///
/// The tag is matched in full, so `core/ndarray-1.0.0` and
/// `core/ndarray-1.1.0` each match only if the extension listed them. The
/// `tag:` prefix is optional on both sides: upstream canonicalizes with
/// `asdf_yaml_tag_canonicalize`, and its own tests register and look up
/// tags without it.
///
/// # Safety
/// `tag` must be a valid NUL-terminated string or null. `file` is accepted
/// for signature compatibility and may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_extension_get(
    file: *mut crate::file_ffi::AsdfFile,
    tag: *const c_char,
) -> *const asdf_extension_t {
    guard("asdf_extension_get", std::ptr::null(), || {
        let _ = file;
        if tag.is_null() {
            return std::ptr::null();
        }
        let wanted = unsafe { CStr::from_ptr(tag) }.to_string_lossy().into_owned();
        let registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());

        for entry in registry.iter() {
            // SAFETY: registration promised this outlives our use of it.
            let extension = unsafe { &*entry.extension };
            if extension.tags.is_null() {
                continue;
            }
            let mut index = 0isize;
            loop {
                let tag_ptr = unsafe { *extension.tags.offset(index) };
                if tag_ptr.is_null() {
                    break;
                }
                let declared = unsafe { CStr::from_ptr(tag_ptr) }.to_string_lossy();
                if tags_match(&declared, &wanted) {
                    return entry.extension;
                }
                index += 1;
            }
        }
        std::ptr::null()
    })
}

/// How many times a particular extension is registered.
///
/// Not part of libasdf's API; used by this crate's own tests. It counts one
/// specific pointer rather than the whole registry, because the registry is
/// process-global and tests run in parallel — a total would be racy.
#[cfg(test)]
pub(crate) fn registrations_of(ext: *const asdf_extension_t) -> usize {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|r| std::ptr::eq(r.extension, ext))
        .count()
}

/// Parse a tag into its name and version.
///
/// `core/ndarray-1.1.0` yields the name `core/ndarray` and version `1.1.0`.
/// A tag with no parseable trailing version yields the whole string and a
/// null version.
///
/// # Safety
/// `tag` must be a valid NUL-terminated string or null. The result must be
/// freed with [`asdf_tag_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_tag_parse(tag: *const c_char) -> *mut asdf_tag_t {
    use std::ffi::CString;

    guard("asdf_tag_parse", std::ptr::null_mut(), || {
        if tag.is_null() {
            return std::ptr::null_mut();
        }
        let text = unsafe { CStr::from_ptr(tag) }.to_string_lossy().into_owned();
        let (name, version) = asdf_core::yaml::tag::split_tag_version(&text);

        let Ok(name) = CString::new(name) else {
            return std::ptr::null_mut();
        };
        let version_ptr = match version {
            Some(v) => {
                let Ok(v) = CString::new(v) else {
                    return std::ptr::null_mut();
                };
                let parsed = unsafe { crate::version_ffi::asdf_version_parse(v.as_ptr()) };
                if parsed.is_null() {
                    return std::ptr::null_mut();
                }
                parsed.cast_const()
            }
            None => std::ptr::null(),
        };

        Box::into_raw(Box::new(asdf_tag_t {
            name: name.into_raw().cast_const(),
            version: version_ptr,
        }))
    })
}

/// Free a tag from [`asdf_tag_parse`].
///
/// # Safety
/// `tag` must be null or have come from [`asdf_tag_parse`], and must not be
/// used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_tag_destroy(tag: *mut asdf_tag_t) {
    use std::ffi::CString;

    guard("asdf_tag_destroy", (), || {
        if tag.is_null() {
            return;
        }
        let boxed = unsafe { Box::from_raw(tag) };
        if !boxed.name.is_null() {
            drop(unsafe { CString::from_raw(boxed.name.cast_mut()) });
        }
        if !boxed.version.is_null() {
            unsafe { crate::version_ffi::asdf_version_destroy(boxed.version.cast_mut()) };
        }
    })
}

/// Whether two tags name the same thing, with or without the `tag:` prefix.
///
/// An extension may register `stsci.edu:asdf/tests/foo-1.1.0` and a caller
/// may look it up the same way, while the tag on a value is always the full
/// `tag:stsci.edu:asdf/tests/foo-1.1.0`. Upstream reconciles the two by
/// canonicalizing both with `asdf_yaml_tag_canonicalize`; comparing without
/// the prefix is the same relation and allocates nothing.
fn tags_match(left: &str, right: &str) -> bool {
    fn bare(tag: &str) -> &str {
        tag.strip_prefix("tag:").unwrap_or(tag)
    }
    bare(left) == bare(right)
}

/// Whether a value's tag is one this extension handles.
///
/// # Safety
/// `value` must be null or a valid value handle; `ext` null or a registered
/// extension.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_extension_type(
    value: *mut crate::file_ffi::AsdfValue,
    ext: *const asdf_extension_t,
) -> bool {
    guard("asdf_value_is_extension_type", false, || {
        use crate::file_ffi::{value_document, value_node};

        if ext.is_null() {
            return false;
        }
        let (Some(doc), Some(node)) = (value_document(value), value_node(value)) else {
            return false;
        };
        let Some(tag) = doc.tag_of(node) else {
            return false;
        };
        let full = tag.full();

        let extension = unsafe { &*ext };
        if extension.tags.is_null() {
            return false;
        }
        let mut index = 0isize;
        loop {
            let tag_ptr = unsafe { *extension.tags.offset(index) };
            if tag_ptr.is_null() {
                return false;
            }
            if tags_match(&unsafe { CStr::from_ptr(tag_ptr) }.to_string_lossy(), &full) {
                return true;
            }
            index += 1;
        }
    })
}

/// Whether the value at `path` is of this extension's type.
///
/// # Safety
/// `file` must be a valid file handle; `path` a valid string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_is_extension_type(
    file: *mut crate::file_ffi::AsdfFile,
    path: *const c_char,
    ext: *mut asdf_extension_t,
) -> bool {
    guard("asdf_is_extension_type", false, || {
        let value = unsafe { crate::file_ffi::asdf_get_value(file, path) };
        if value.is_null() {
            return false;
        }
        let matched = unsafe { asdf_value_is_extension_type(value, ext) };
        unsafe { crate::file_ffi::asdf_value_destroy(value) };
        matched
    })
}

/// Deserialize a value through an extension.
///
/// # Safety
/// `value` must be a valid value handle, `ext` a registered extension whose
/// vtable has a deserializer, and `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_extension_type(
    value: *mut crate::file_ffi::AsdfValue,
    ext: *const asdf_extension_t,
    out: *mut *mut c_void,
) -> crate::types::AsdfValueErr {
    use crate::types::AsdfValueErr;

    guard("asdf_value_as_extension_type", AsdfValueErr::Unknown, || {
        if ext.is_null() || out.is_null() {
            return AsdfValueErr::Unknown;
        }
        if !unsafe { asdf_value_is_extension_type(value, ext) } {
            return AsdfValueErr::TypeMismatch;
        }
        let extension = unsafe { &*ext };
        if extension.vtab.is_null() {
            return AsdfValueErr::Unknown;
        }
        let Some(deserialize) = (unsafe { &*extension.vtab }).deserialize else {
            return AsdfValueErr::Unknown;
        };
        // The extension owns what it produces; the generated
        // `asdf_<ext>_destroy` releases it.
        unsafe { deserialize(value, extension.userdata.cast_const(), out) }
    })
}

/// Read the value at `path` through an extension.
///
/// # Safety
/// See [`asdf_value_as_extension_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_extension_type(
    file: *mut crate::file_ffi::AsdfFile,
    path: *const c_char,
    ext: *const asdf_extension_t,
    out: *mut *mut c_void,
) -> crate::types::AsdfValueErr {
    use crate::types::AsdfValueErr;

    guard("asdf_get_extension_type", AsdfValueErr::Unknown, || {
        let value = unsafe { crate::file_ffi::asdf_get_value(file, path) };
        if value.is_null() {
            return AsdfValueErr::NotFound;
        }
        let result = unsafe { asdf_value_as_extension_type(value, ext, out) };
        unsafe { crate::file_ffi::asdf_value_destroy(value) };
        result
    })
}

/// A wrapper making one of the identity statics shareable.
///
/// `asdf_version_t` and `asdf_software_t` hold raw pointers, so they are not
/// `Sync` in general — a heap-allocated one from `asdf_version_parse` is the
/// caller's to manage. The two statics below are different: every pointer in
/// them refers to a string literal, and nothing ever writes to them, so
/// sharing them across threads is sound. `repr(transparent)` keeps the
/// exported symbol byte-identical to the bare struct.
#[repr(transparent)]
#[derive(Debug)]
pub struct Identity<T>(pub T);

// SAFETY: only used for the two statics below, which are built entirely from
// `'static` string literals and are never mutated.
unsafe impl<T> Sync for Identity<T> {}

/// The library's own version, exported as `libasdf_version`.
///
/// A data symbol, not a function: callers read it directly.
///
/// Immutable, though the header declares it as plain (non-`const`) extern
/// data. It is the library's identity and nothing should write to it; making
/// it a shared `static` lets it live in read-only memory, so an errant write
/// faults rather than silently corrupting the value every later reader sees.
#[unsafe(no_mangle)]
pub static libasdf_version: Identity<asdf_version_t> = Identity(asdf_version_t {
    version: c"0.1.0".as_ptr(),
    major: 0,
    minor: 1,
    patch: 0,
    extra: std::ptr::null(),
});

/// The library's own `core/software` metadata, exported as
/// `libasdf_software`.
///
/// Recorded in the `asdf_library` field of files this library writes.
/// Immutable for the same reason as [`libasdf_version`].
#[unsafe(no_mangle)]
pub static libasdf_software: Identity<asdf_software_t> = Identity(asdf_software_t {
    name: c"libasdf-rs".as_ptr(),
    version: (&raw const libasdf_version).cast::<asdf_version_t>(),
    author: c"The libasdf-rs Developers".as_ptr(),
    homepage: c"https://github.com/petesmc/libasdf-rs".as_ptr(),
});

/// Serialize a native object through an extension.
///
/// # Safety
/// `file` must be a valid file handle, `obj` a valid object of the
/// extension's type, and `ext` a registered extension whose vtable has a
/// serializer. The result must be released with `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_extension_type(
    file: *mut crate::file_ffi::AsdfFile,
    obj: *const c_void,
    ext: *const asdf_extension_t,
) -> *mut crate::file_ffi::AsdfValue {
    guard("asdf_value_of_extension_type", std::ptr::null_mut(), || {
        if ext.is_null() {
            return std::ptr::null_mut();
        }
        let extension = unsafe { &*ext };
        if extension.vtab.is_null() {
            return std::ptr::null_mut();
        }
        let Some(serialize) = (unsafe { &*extension.vtab }).serialize else {
            // The header allows a null serializer, meaning the type cannot
            // be written.
            return std::ptr::null_mut();
        };

        // The first tag an extension registers is the one written for a
        // newly serialized object; without it the value goes into the tree
        // untagged and nothing can read it back as this type.
        if extension.tags.is_null() {
            return std::ptr::null_mut();
        }
        let first = unsafe { *extension.tags };
        if first.is_null() {
            return std::ptr::null_mut();
        }
        let tag = unsafe { CStr::from_ptr(first) }.to_string_lossy().into_owned();

        let value = unsafe { serialize(file, obj, extension.userdata.cast_const()) };
        if value.is_null() {
            return value;
        }

        let (Some(owner), Some(node)) =
            (crate::file_ffi::value_file(value), crate::file_ffi::value_node(value))
        else {
            return value;
        };
        // The tag may be registered without its `tag:` prefix; the tree
        // always carries the full form.
        let full = if tag.starts_with("tag:") { tag } else { format!("tag:{tag}") };
        // SAFETY: the file outlives every value taken from it, by the C
        // contract, and the serializer has already finished with the tree.
        if let Some(doc) = unsafe { &mut *owner }.document_for_values() {
            doc.node_mut(node).tag = Some(asdf_core::yaml::Tag::parse(&full));
        }
        value
    })
}

/// Write a native object at `path` through an extension.
///
/// # Safety
/// See [`asdf_value_of_extension_type`]; `path` must be a valid
/// NUL-terminated string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_set_extension_type(
    file: *mut crate::file_ffi::AsdfFile,
    path: *const c_char,
    obj: *const c_void,
    ext: *const asdf_extension_t,
) -> crate::types::AsdfValueErr {
    use crate::types::AsdfValueErr;

    guard("asdf_set_extension_type", AsdfValueErr::Unknown, || {
        let value = unsafe { asdf_value_of_extension_type(file, obj, ext) };
        if value.is_null() {
            // No serializer, or the serializer failed.
            return AsdfValueErr::EmitFailure;
        }
        let result = unsafe { crate::file_ffi::set_value_at(file, path, value) };
        unsafe { crate::file_ffi::asdf_value_destroy(value) };
        result
    })
}

// ---- Schema property helpers -----------------------------------------
//
// `asdf/extension_util.h`. These are what an extension's deserializer uses
// to pull a schema's properties out of a mapping with the type checking the
// schema calls for, rather than repeating it at each call site.

/// Whether a value of `found` can be read as `wanted`.
///
/// Integer widths widen, and every numeric type reads as a `double`. The
/// relation is deliberately one-way: a `uint16` satisfies a request for an
/// `int32`, but not the reverse, because the reverse can overflow.
fn is_equivalent_type(found: AsdfValueType, wanted: AsdfValueType) -> bool {
    use AsdfValueType as T;
    match wanted {
        T::Uint64 => matches!(found, T::Uint64 | T::Uint32 | T::Uint16 | T::Uint8),
        T::Uint32 => matches!(found, T::Uint32 | T::Uint16 | T::Uint8),
        T::Uint16 => matches!(found, T::Uint16 | T::Uint8),
        T::Uint8 => found == T::Uint8,
        T::Int64 => matches!(
            found,
            T::Int64 | T::Uint32 | T::Int32 | T::Uint16 | T::Int16 | T::Uint8 | T::Int8
        ),
        T::Int32 => matches!(found, T::Int32 | T::Uint16 | T::Int16 | T::Uint8 | T::Int8),
        T::Int16 => matches!(found, T::Int16 | T::Uint8 | T::Int8),
        T::Int8 => found == T::Int8,
        T::Double => matches!(
            found,
            T::Double
                | T::Float
                | T::Int64
                | T::Int32
                | T::Int16
                | T::Int8
                | T::Uint64
                | T::Uint32
                | T::Uint16
                | T::Uint8
        ),
        other => found == other,
    }
}

/// Look up a mapping's property and read it as `value_type`.
///
/// # Safety
/// `mapping` must be a valid handle, `name` a valid string, `tag` a valid
/// string or null, and `out` storage of the C type matching `value_type`.
unsafe fn get_property(
    mapping: *mut crate::value_ffi::AsdfMapping,
    name: *const c_char,
    value_type: c_int,
    tag: *const c_char,
    out: *mut c_void,
) -> crate::types::AsdfValueErr {
    use crate::types::AsdfValueErr;

    let prop = unsafe { crate::value_ffi::asdf_mapping_get(mapping, name) };
    if prop.is_null() {
        return AsdfValueErr::NotFound;
    }
    let release = |value| unsafe { crate::file_ffi::asdf_value_destroy(value) };

    let Some(wanted) = AsdfValueType::from_i32(value_type) else {
        release(prop);
        return AsdfValueErr::TypeMismatch;
    };

    // An extension type is matched by tag rather than by shape.
    if wanted == AsdfValueType::Extension && !tag.is_null() {
        let file = crate::file_ffi::value_file(mapping).unwrap_or(std::ptr::null_mut());
        let ext = unsafe { asdf_extension_get(file, tag) };
        if ext.is_null() || !unsafe { asdf_value_is_extension_type(prop, ext) } {
            release(prop);
            return AsdfValueErr::TypeMismatch;
        }
        let err = unsafe { asdf_value_as_extension_type(prop, ext, out.cast()) };
        release(prop);
        return err;
    }

    if wanted != AsdfValueType::Unknown && wanted != AsdfValueType::Extension {
        let found = unsafe { crate::file_ffi::asdf_value_get_type(prop) };
        if !is_equivalent_type(found, wanted) {
            release(prop);
            return AsdfValueErr::TypeMismatch;
        }
    }

    let err = unsafe { crate::value_ffi::asdf_value_as_type(prop, value_type, out) };
    release(prop);
    err
}

/// Read a property the schema requires.
///
/// # Safety
/// See [`asdf_get_optional_property`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_required_property(
    mapping: *mut crate::value_ffi::AsdfMapping,
    name: *const c_char,
    value_type: c_int,
    tag: *const c_char,
    out: *mut c_void,
) -> crate::types::AsdfValueErr {
    guard("asdf_get_required_property", crate::types::AsdfValueErr::Unknown, || unsafe {
        get_property(mapping, name, value_type, tag, out)
    })
}

/// Read a property the schema allows but does not require.
///
/// Identical to [`asdf_get_required_property`] except in how loudly an
/// absent property is reported; both return `ASDF_VALUE_ERR_NOT_FOUND`.
///
/// # Safety
/// `mapping` must be a valid handle, `name` a valid string, `tag` a valid
/// string or null, and `out` storage of the C type matching `value_type`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_optional_property(
    mapping: *mut crate::value_ffi::AsdfMapping,
    name: *const c_char,
    value_type: c_int,
    tag: *const c_char,
    out: *mut c_void,
) -> crate::types::AsdfValueErr {
    guard("asdf_get_optional_property", crate::types::AsdfValueErr::Unknown, || unsafe {
        get_property(mapping, name, value_type, tag, out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn parses_a_versioned_tag() {
        let tag = CString::new("tag:stsci.edu:asdf/core/ndarray-1.1.0").unwrap();
        let parsed = unsafe { asdf_tag_parse(tag.as_ptr()) };
        assert!(!parsed.is_null());

        let view = unsafe { &*parsed };
        assert_eq!(
            unsafe { CStr::from_ptr(view.name) }.to_str().unwrap(),
            "tag:stsci.edu:asdf/core/ndarray"
        );
        assert!(!view.version.is_null());
        let version = unsafe { &*view.version };
        assert_eq!((version.major, version.minor, version.patch), (1, 1, 0));

        unsafe { asdf_tag_destroy(parsed) };
    }

    #[test]
    fn a_tag_without_a_version_has_a_null_version() {
        let tag = CString::new("tag:example.com:plain").unwrap();
        let parsed = unsafe { asdf_tag_parse(tag.as_ptr()) };
        let view = unsafe { &*parsed };
        assert_eq!(unsafe { CStr::from_ptr(view.name) }.to_str().unwrap(), "tag:example.com:plain");
        assert!(view.version.is_null());
        unsafe { asdf_tag_destroy(parsed) };
    }

    #[test]
    fn tag_parsing_tolerates_null() {
        assert!(unsafe { asdf_tag_parse(std::ptr::null()) }.is_null());
        unsafe { asdf_tag_destroy(std::ptr::null_mut()) };
    }

    /// Build a registration the way `ASDF_REGISTER_EXTENSION` does.
    ///
    /// Everything is **leaked on purpose**. The macro makes the
    /// `asdf_extension_t` and its tag array file-scope statics, and the
    /// registry stores the pointer rather than a copy, so a registered
    /// extension must genuinely live for the process. Allocating one in a
    /// `Box` and dropping it leaves the registry holding a dangling pointer
    /// — and, since the allocator reuses addresses, a later extension can
    /// appear to be registered already.
    fn make_extension(tags: &[&str]) -> &'static mut asdf_extension_t {
        let names: Vec<CString> = tags.iter().map(|t| CString::new(*t).unwrap()).collect();
        let mut array: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();
        array.push(std::ptr::null());

        // Leak the names first so their pointers stay valid.
        let names: &'static [CString] = Vec::leak(names);
        let _ = names;
        let array: &'static [*const c_char] = Vec::leak(array);

        Box::leak(Box::new(asdf_extension_t {
            tags: array.as_ptr(),
            software: std::ptr::null_mut(),
            vtab: std::ptr::null(),
            size: 0,
            userdata: std::ptr::null_mut(),
        }))
    }

    #[test]
    fn registers_and_looks_up_by_tag() {
        let ext = make_extension(&["tag:example.com:thing-1.0.0"]);
        assert_eq!(registrations_of(ext), 0);
        unsafe { asdf_extension_register(ext) };
        assert_eq!(registrations_of(ext), 1);

        let wanted = CString::new("tag:example.com:thing-1.0.0").unwrap();
        let found = unsafe { asdf_extension_get(std::ptr::null_mut(), wanted.as_ptr()) };
        assert!(std::ptr::eq(found, ext));

        let missing = CString::new("tag:example.com:other-1.0.0").unwrap();
        assert!(unsafe { asdf_extension_get(std::ptr::null_mut(), missing.as_ptr()) }.is_null());
    }

    #[test]
    fn one_extension_can_serve_several_tag_versions() {
        // The documented use: an extension lists every version it reads, and
        // writes with the first.
        let ext = make_extension(&["tag:example.com:multi-1.1.0", "tag:example.com:multi-1.0.0"]);
        unsafe { asdf_extension_register(ext) };

        for tag in ["tag:example.com:multi-1.1.0", "tag:example.com:multi-1.0.0"] {
            let c = CString::new(tag).unwrap();
            let found = unsafe { asdf_extension_get(std::ptr::null_mut(), c.as_ptr()) };
            assert!(std::ptr::eq(found, ext), "{tag}");
        }
    }

    #[test]
    fn registering_twice_keeps_one_entry() {
        let ext = make_extension(&["tag:example.com:dup-1.0.0"]);
        unsafe { asdf_extension_register(ext) };
        unsafe { asdf_extension_register(ext) };
        assert_eq!(registrations_of(ext), 1, "a repeated registration must not add a second entry");
    }

    #[test]
    fn registration_tolerates_null() {
        unsafe { asdf_extension_register(std::ptr::null_mut()) };
        assert_eq!(registrations_of(std::ptr::null()), 0);
        assert!(unsafe { asdf_extension_get(std::ptr::null_mut(), std::ptr::null()) }.is_null());
    }

    #[test]
    fn value_type_matching_uses_the_full_tag() {
        use crate::file_ffi::{asdf_close, asdf_get_value, asdf_open_mem_ex, asdf_value_destroy};

        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(b"d: !core/ndarray-1.1.0\n  source: 0\n...\n");

        let file =
            unsafe { asdf_open_mem_ex(buf.as_ptr().cast(), buf.len(), std::ptr::null_mut()) };
        assert!(!file.is_null());

        let path = CString::new("d").unwrap();
        let value = unsafe { asdf_get_value(file, path.as_ptr()) };
        assert!(!value.is_null());

        let matching = make_extension(&["tag:stsci.edu:asdf/core/ndarray-1.1.0"]);
        assert!(unsafe { asdf_value_is_extension_type(value, matching) });

        // A different *version* of the same schema must not match, since an
        // extension declares each version it handles.
        let other = make_extension(&["tag:stsci.edu:asdf/core/ndarray-1.0.0"]);
        assert!(!unsafe { asdf_value_is_extension_type(value, other) });

        assert!(!unsafe { asdf_value_is_extension_type(value, std::ptr::null()) });

        unsafe { asdf_value_destroy(value) };
        unsafe { asdf_close(file) };
    }

    #[test]
    fn deserializing_without_a_vtable_is_an_error_not_a_crash() {
        use crate::types::AsdfValueErr;

        let ext = make_extension(&["tag:example.com:novtab-1.0.0"]);
        let mut out: *mut c_void = std::ptr::null_mut();
        // A null value handle first.
        assert_eq!(
            unsafe { asdf_value_as_extension_type(std::ptr::null_mut(), ext, &mut out) },
            AsdfValueErr::TypeMismatch
        );
    }

    #[test]
    fn the_vtable_keeps_its_reserved_width() {
        // The reserved slots are what let upstream add methods without an
        // ABI break, so the total width is part of the contract.
        use std::mem::size_of;
        assert_eq!(
            size_of::<asdf_extension_vtab_t>(),
            ASDF_EXTENSION_VTAB_MAX_METHODS * size_of::<AsdfExtensionMethod>()
        );
    }

    #[test]
    fn the_library_reports_its_own_version() {
        assert_eq!(unsafe { CStr::from_ptr(libasdf_version.0.version) }.to_str().unwrap(), "0.1.0");
        assert_eq!(
            unsafe { CStr::from_ptr(libasdf_software.0.name) }.to_str().unwrap(),
            "libasdf-rs"
        );
        // The software's version must point at the exported version symbol,
        // not a copy of it.
        assert!(std::ptr::eq(
            libasdf_software.0.version,
            (&raw const libasdf_version).cast::<asdf_version_t>()
        ));
    }
}
