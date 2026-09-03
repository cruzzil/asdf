//! The core-schema extensions.
//!
//! `ASDF_REGISTER_EXTENSION` generates eleven public functions for each
//! extension type. C gets them from the macro; here they are produced by
//! [`declare_extension`], which mirrors the macro's naming and semantics
//! exactly so a caller cannot tell the difference:
//!
//! `asdf_get_<name>`, `asdf_set_<name>`, `asdf_is_<name>`,
//! `asdf_value_is_<name>`, `asdf_value_as_<name>`, `asdf_value_of_<name>`,
//! `asdf_<name>_copy`, `asdf_<name>_copy_into`, `asdf_<name>_array_copy`,
//! `asdf_<name>_deinit` and `asdf_<name>_destroy`.
//!
//! The C types they work with are plain structs of borrowed pointers, so
//! each has a `deinit` that frees the fields without freeing the struct —
//! the split libasdf's headers call out, because an object may be embedded,
//! an array element, or static.

use std::ffi::{CStr, CString, c_char};

use asdf_core::yaml::{Document, NodeId, Tag};

use crate::extension_ffi::{asdf_software_t, asdf_tag_t};
use crate::file_ffi::{AsdfFile, AsdfValue, value_document, value_node};
use crate::panic::guard;
use crate::types::AsdfValueErr;
use crate::version_ffi::{asdf_version_parse, asdf_version_t};

/// Allocate a C string, or null when the text contains an interior NUL.
fn to_c_string(text: &str) -> *const c_char {
    CString::new(text).map_or(std::ptr::null(), |c| c.into_raw().cast_const())
}

/// Free a string produced by [`to_c_string`].
unsafe fn free_c_string(ptr: *const c_char) {
    if !ptr.is_null() {
        drop(unsafe { CString::from_raw(ptr.cast_mut()) });
    }
}

/// Copy a C string, or null.
unsafe fn clone_c_string(ptr: *const c_char) -> *const c_char {
    if ptr.is_null() {
        return std::ptr::null();
    }
    let text = unsafe { CStr::from_ptr(ptr) };
    CString::new(text.to_bytes()).map_or(std::ptr::null(), |c| c.into_raw().cast_const())
}

/// Read a mapping's string entry.
fn string_field(doc: &Document, node: NodeId, key: &str) -> *const c_char {
    doc.mapping_get(node, key)
        .and_then(|id| doc.resolved(id).as_str().map(to_c_string))
        .unwrap_or(std::ptr::null())
}

/// Read a mapping's entry as a parsed version.
fn version_field(doc: &Document, node: NodeId, key: &str) -> *const asdf_version_t {
    let Some(text) =
        doc.mapping_get(node, key).and_then(|id| doc.resolved(id).as_str().map(str::to_string))
    else {
        return std::ptr::null();
    };
    let Ok(c) = CString::new(text) else {
        return std::ptr::null();
    };
    unsafe { asdf_version_parse(c.as_ptr()) }.cast_const()
}

/// Generate the eleven functions `ASDF_REGISTER_EXTENSION` produces.
///
/// `$deserialize` fills a zeroed object from a value; `$serialize` builds a
/// value from an object; `$deinit` frees the object's fields; `$copy`
/// deep-copies into pre-zeroed storage.
macro_rules! declare_extension {
    (
        name: $name:ident,
        ty: $ty:ty,
        tag: $tag:expr,
        deserialize: $deserialize:path,
        serialize: $serialize:path,
        deinit: $deinit:path,
        copy: $copy:path,
        is_fn: $is_fn:ident,
        value_is_fn: $value_is_fn:ident,
        value_as_fn: $value_as_fn:ident,
        value_of_fn: $value_of_fn:ident,
        get_fn: $get_fn:ident,
        set_fn: $set_fn:ident,
        copy_fn: $copy_fn:ident,
        copy_into_fn: $copy_into_fn:ident,
        array_copy_fn: $array_copy_fn:ident,
        deinit_fn: $deinit_fn:ident,
        destroy_fn: $destroy_fn:ident,
    ) => {
        /// Whether a value carries this extension's tag.
        ///
        /// # Safety
        /// `value` must be null or a valid value handle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $value_is_fn(value: *mut AsdfValue) -> bool {
            guard(stringify!($value_is_fn), false, || {
                let (Some(doc), Some(node)) = (value_document(value), value_node(value)) else {
                    return false;
                };
                doc.tag_of(node).is_some_and(|t| t.full() == $tag)
            })
        }

        /// Whether the value at `path` carries this extension's tag.
        ///
        /// # Safety
        /// `file` must be a valid file handle and `path` a valid string or
        /// null.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $is_fn(file: *mut AsdfFile, path: *const c_char) -> bool {
            guard(stringify!($is_fn), false, || {
                let value = unsafe { crate::file_ffi::asdf_get_value(file, path) };
                if value.is_null() {
                    return false;
                }
                let matched = unsafe { $value_is_fn(value) };
                unsafe { crate::file_ffi::asdf_value_destroy(value) };
                matched
            })
        }

        /// Read a value as this extension's type.
        ///
        /// # Safety
        /// `value` must be a valid value handle and `out` writable. The
        /// result must be released with the matching `destroy`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $value_as_fn(
            value: *mut AsdfValue,
            out: *mut *mut $ty,
        ) -> AsdfValueErr {
            guard(stringify!($value_as_fn), AsdfValueErr::Unknown, || {
                if out.is_null() {
                    return AsdfValueErr::Unknown;
                }
                if !unsafe { $value_is_fn(value) } {
                    return AsdfValueErr::TypeMismatch;
                }
                let (Some(doc), Some(node)) = (value_document(value), value_node(value)) else {
                    return AsdfValueErr::Unknown;
                };
                let boxed: Box<$ty> = Box::new(<$ty>::zeroed());
                let raw = Box::into_raw(boxed);
                match $deserialize(doc, node, raw) {
                    AsdfValueErr::Ok => {
                        unsafe { *out = raw };
                        AsdfValueErr::Ok
                    }
                    err => {
                        unsafe { $destroy_fn(raw) };
                        err
                    }
                }
            })
        }

        /// Read the value at `path` as this extension's type.
        ///
        /// # Safety
        /// See the value-level reader.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $get_fn(
            file: *mut AsdfFile,
            path: *const c_char,
            out: *mut *mut $ty,
        ) -> AsdfValueErr {
            guard(stringify!($get_fn), AsdfValueErr::Unknown, || {
                let value = unsafe { crate::file_ffi::asdf_get_value(file, path) };
                if value.is_null() {
                    return AsdfValueErr::NotFound;
                }
                let result = unsafe { $value_as_fn(value, out) };
                unsafe { crate::file_ffi::asdf_value_destroy(value) };
                result
            })
        }

        /// Build a value from an object of this extension's type.
        ///
        /// # Safety
        /// `file` must be a valid file handle and `obj` a valid object. The
        /// result must be released with `asdf_value_destroy`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $value_of_fn(
            file: *mut AsdfFile,
            obj: *const $ty,
        ) -> *mut AsdfValue {
            guard(stringify!($value_of_fn), std::ptr::null_mut(), || {
                if file.is_null() || obj.is_null() {
                    return std::ptr::null_mut();
                }
                let handle = unsafe { &mut *file };
                let Some(doc) = handle.document_for_values() else {
                    return std::ptr::null_mut();
                };
                let Some(node) = $serialize(doc, unsafe { &*obj }) else {
                    return std::ptr::null_mut();
                };
                doc.node_mut(node).tag = Some(Tag::parse($tag));
                Box::into_raw(Box::new(AsdfValue::new(file, node)))
            })
        }

        /// Write an object of this extension's type at `path`.
        ///
        /// # Safety
        /// See the value constructor; `path` must be a valid string or null.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $set_fn(
            file: *mut AsdfFile,
            path: *const c_char,
            obj: *const $ty,
        ) -> AsdfValueErr {
            guard(stringify!($set_fn), AsdfValueErr::Unknown, || {
                let value = unsafe { $value_of_fn(file, obj) };
                if value.is_null() {
                    return AsdfValueErr::EmitFailure;
                }
                let result = unsafe { crate::file_ffi::set_value_at(file, path, value) };
                unsafe { crate::file_ffi::asdf_value_destroy(value) };
                result
            })
        }

        /// Free the object's fields without freeing the object itself.
        ///
        /// The split matters because an object may be embedded, an array
        /// element, or static, so its own storage is not always ours to free.
        ///
        /// # Safety
        /// `obj` must be null or a valid object of this type; it must be safe
        /// to call on a zeroed or partially-initialised one.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $deinit_fn(obj: *mut $ty) {
            guard(stringify!($deinit_fn), (), || {
                if !obj.is_null() {
                    unsafe { $deinit(obj) };
                }
            })
        }

        /// De-initialise and free an object.
        ///
        /// # Safety
        /// `obj` must be null or have come from this extension, and must not
        /// be used afterwards.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $destroy_fn(obj: *mut $ty) {
            guard(stringify!($destroy_fn), (), || {
                if obj.is_null() {
                    return;
                }
                unsafe { $deinit(obj) };
                drop(unsafe { Box::from_raw(obj) });
            })
        }

        /// Deep-copy an object into caller-provided storage.
        ///
        /// `dst` is zeroed first, and de-initialised on failure, matching the
        /// generated wrapper's contract.
        ///
        /// # Safety
        /// `src` and `dst` must be valid objects of this type.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $copy_into_fn(
            file: *mut AsdfFile,
            src: *const $ty,
            dst: *mut $ty,
        ) -> bool {
            guard(stringify!($copy_into_fn), false, || {
                let _ = file;
                if src.is_null() || dst.is_null() {
                    return false;
                }
                unsafe { std::ptr::write(dst, <$ty>::zeroed()) };
                if unsafe { $copy(&*src, dst) } {
                    true
                } else {
                    unsafe { $deinit(dst) };
                    false
                }
            })
        }

        /// Deep-copy an object into fresh storage.
        ///
        /// # Safety
        /// `src` must be a valid object of this type. The result must be
        /// released with the matching `destroy`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $copy_fn(file: *mut AsdfFile, src: *const $ty) -> *mut $ty {
            guard(stringify!($copy_fn), std::ptr::null_mut(), || {
                if src.is_null() {
                    return std::ptr::null_mut();
                }
                let raw = Box::into_raw(Box::new(<$ty>::zeroed()));
                if unsafe { $copy_into_fn(file, src, raw) } {
                    raw
                } else {
                    drop(unsafe { Box::from_raw(raw) });
                    std::ptr::null_mut()
                }
            })
        }

        /// Deep-copy a null-terminated array of objects.
        ///
        /// # Safety
        /// `src` must be a null-terminated array of valid objects.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $array_copy_fn(
            file: *mut AsdfFile,
            src: *mut *const $ty,
        ) -> *mut *mut $ty {
            guard(stringify!($array_copy_fn), std::ptr::null_mut(), || {
                if src.is_null() {
                    return std::ptr::null_mut();
                }
                let mut count = 0isize;
                while !unsafe { *src.offset(count) }.is_null() {
                    count += 1;
                }

                let mut copies: Vec<*mut $ty> = Vec::with_capacity(count as usize + 1);
                for index in 0..count {
                    let element = unsafe { *src.offset(index) };
                    let copy = unsafe { $copy_fn(file, element) };
                    if copy.is_null() {
                        // Unwind the copies made so far rather than leaking.
                        for made in copies {
                            unsafe { $destroy_fn(made) };
                        }
                        return std::ptr::null_mut();
                    }
                    copies.push(copy);
                }
                copies.push(std::ptr::null_mut());
                copies.shrink_to_fit();
                let boxed = copies.into_boxed_slice();
                Box::into_raw(boxed).cast::<*mut $ty>()
            })
        }
    };
}

// ---- core/software ---------------------------------------------------

/// The tag for `core/software`.
pub const SOFTWARE_TAG: &str = "tag:stsci.edu:asdf/core/software-1.0.0";

impl asdf_software_t {
    /// A zeroed instance, matching what the generated wrappers assume.
    fn zeroed() -> Self {
        Self {
            name: std::ptr::null(),
            version: std::ptr::null(),
            author: std::ptr::null(),
            homepage: std::ptr::null(),
        }
    }
}

fn software_deserialize(doc: &Document, node: NodeId, out: *mut asdf_software_t) -> AsdfValueErr {
    // `name` and `version` are required by the schema.
    let name = string_field(doc, node, "name");
    let version = version_field(doc, node, "version");
    if name.is_null() || version.is_null() {
        unsafe { free_c_string(name) };
        if !version.is_null() {
            unsafe { crate::version_ffi::asdf_version_destroy(version.cast_mut()) };
        }
        return AsdfValueErr::ParseFailure;
    }

    unsafe {
        (*out).name = name;
        (*out).version = version;
        (*out).author = string_field(doc, node, "author");
        (*out).homepage = string_field(doc, node, "homepage");
    }
    AsdfValueErr::Ok
}

fn software_serialize(doc: &mut Document, obj: &asdf_software_t) -> Option<NodeId> {
    let mut pairs = Vec::new();

    let mut put = |doc: &mut Document, key: &str, ptr: *const c_char| {
        if ptr.is_null() {
            return;
        }
        let text = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        let k = doc.add_scalar(key);
        let v = doc.add_scalar_styled(text, asdf_core::yaml::ScalarStyle::SingleQuoted);
        pairs.push((k, v));
    };

    put(doc, "name", obj.name);
    if !obj.version.is_null() {
        let version = unsafe { &*obj.version };
        put(doc, "version", version.version);
    }
    put(doc, "author", obj.author);
    put(doc, "homepage", obj.homepage);

    (!pairs.is_empty()).then(|| doc.add_mapping(pairs))
}

unsafe fn software_deinit(obj: *mut asdf_software_t) {
    let software = unsafe { &mut *obj };
    unsafe { free_c_string(software.name) };
    unsafe { free_c_string(software.author) };
    unsafe { free_c_string(software.homepage) };
    if !software.version.is_null() {
        unsafe { crate::version_ffi::asdf_version_destroy(software.version.cast_mut()) };
    }
    *software = asdf_software_t::zeroed();
}

unsafe fn software_copy(src: &asdf_software_t, dst: *mut asdf_software_t) -> bool {
    let out = unsafe { &mut *dst };
    out.name = unsafe { clone_c_string(src.name) };
    out.author = unsafe { clone_c_string(src.author) };
    out.homepage = unsafe { clone_c_string(src.homepage) };
    out.version = if src.version.is_null() {
        std::ptr::null()
    } else {
        unsafe { crate::version_ffi::asdf_version_copy(src.version) }.cast_const()
    };
    // Only a failed allocation of a field that was present is a failure.
    !(out.name.is_null() && !src.name.is_null())
}

declare_extension! {
    name: software,
    ty: asdf_software_t,
    tag: SOFTWARE_TAG,
    deserialize: software_deserialize,
    serialize: software_serialize,
    deinit: software_deinit,
    copy: software_copy,
    is_fn: asdf_is_software,
    value_is_fn: asdf_value_is_software,
    value_as_fn: asdf_value_as_software,
    value_of_fn: asdf_value_of_software,
    get_fn: asdf_get_software,
    set_fn: asdf_set_software,
    copy_fn: asdf_software_copy,
    copy_into_fn: asdf_software_copy_into,
    array_copy_fn: asdf_software_array_copy,
    deinit_fn: asdf_software_deinit,
    destroy_fn: asdf_software_destroy,
}

// ---- core/extension_metadata ----------------------------------------

/// The tag for `core/extension_metadata`.
pub const EXTENSION_METADATA_TAG: &str = "tag:stsci.edu:asdf/core/extension_metadata-1.0.0";

/// Mirror of `asdf_extension_metadata_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_extension_metadata_t {
    /// The extension class that wrote the file.
    pub extension_class: *const c_char,
    /// The package providing it.
    pub package: *const asdf_software_t,
    /// Any further metadata, as a mapping value.
    pub metadata: *mut AsdfValue,
}

impl asdf_extension_metadata_t {
    fn zeroed() -> Self {
        Self {
            extension_class: std::ptr::null(),
            package: std::ptr::null(),
            metadata: std::ptr::null_mut(),
        }
    }
}

fn extension_metadata_deserialize(
    doc: &Document,
    node: NodeId,
    out: *mut asdf_extension_metadata_t,
) -> AsdfValueErr {
    let class = string_field(doc, node, "extension_class");
    if class.is_null() {
        return AsdfValueErr::ParseFailure;
    }

    // The schema calls the package `software` in some versions and
    // `package` in others; accept either.
    let package = ["package", "software"]
        .iter()
        .find_map(|key| doc.mapping_get(node, key))
        .map(|id| {
            let raw = Box::into_raw(Box::new(asdf_software_t::zeroed()));
            if software_deserialize(doc, id, raw) == AsdfValueErr::Ok {
                raw.cast_const()
            } else {
                drop(unsafe { Box::from_raw(raw) });
                std::ptr::null()
            }
        })
        .unwrap_or(std::ptr::null());

    unsafe {
        (*out).extension_class = class;
        (*out).package = package;
        (*out).metadata = std::ptr::null_mut();
    }
    AsdfValueErr::Ok
}

fn extension_metadata_serialize(
    doc: &mut Document,
    obj: &asdf_extension_metadata_t,
) -> Option<NodeId> {
    let mut pairs = Vec::new();
    if !obj.extension_class.is_null() {
        let text = unsafe { CStr::from_ptr(obj.extension_class) }.to_string_lossy().into_owned();
        let k = doc.add_scalar("extension_class");
        let v = doc.add_scalar_styled(text, asdf_core::yaml::ScalarStyle::SingleQuoted);
        pairs.push((k, v));
    }
    if !obj.package.is_null()
        && let Some(node) = software_serialize(doc, unsafe { &*obj.package })
    {
        doc.node_mut(node).tag = Some(Tag::parse(SOFTWARE_TAG));
        let k = doc.add_scalar("software");
        pairs.push((k, node));
    }
    (!pairs.is_empty()).then(|| doc.add_mapping(pairs))
}

unsafe fn extension_metadata_deinit(obj: *mut asdf_extension_metadata_t) {
    let metadata = unsafe { &mut *obj };
    unsafe { free_c_string(metadata.extension_class) };
    if !metadata.package.is_null() {
        unsafe { asdf_software_destroy(metadata.package.cast_mut()) };
    }
    if !metadata.metadata.is_null() {
        unsafe { crate::file_ffi::asdf_value_destroy(metadata.metadata) };
    }
    *metadata = asdf_extension_metadata_t::zeroed();
}

unsafe fn extension_metadata_copy(
    src: &asdf_extension_metadata_t,
    dst: *mut asdf_extension_metadata_t,
) -> bool {
    let out = unsafe { &mut *dst };
    out.extension_class = unsafe { clone_c_string(src.extension_class) };
    out.package = if src.package.is_null() {
        std::ptr::null()
    } else {
        unsafe { asdf_software_copy(std::ptr::null_mut(), src.package) }.cast_const()
    };
    out.metadata = std::ptr::null_mut();
    true
}

declare_extension! {
    name: extension_metadata,
    ty: asdf_extension_metadata_t,
    tag: EXTENSION_METADATA_TAG,
    deserialize: extension_metadata_deserialize,
    serialize: extension_metadata_serialize,
    deinit: extension_metadata_deinit,
    copy: extension_metadata_copy,
    is_fn: asdf_is_extension_metadata,
    value_is_fn: asdf_value_is_extension_metadata,
    value_as_fn: asdf_value_as_extension_metadata,
    value_of_fn: asdf_value_of_extension_metadata,
    get_fn: asdf_get_extension_metadata,
    set_fn: asdf_set_extension_metadata,
    copy_fn: asdf_extension_metadata_copy,
    copy_into_fn: asdf_extension_metadata_copy_into,
    array_copy_fn: asdf_extension_metadata_array_copy,
    deinit_fn: asdf_extension_metadata_deinit,
    destroy_fn: asdf_extension_metadata_destroy,
}

/// Override the `asdf_library` metadata written to a file.
///
/// # Safety
/// `file` must be a valid file handle and `software` a valid object, which is
/// copied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_library_set(file: *mut AsdfFile, software: *const asdf_software_t) {
    guard("asdf_library_set", (), || {
        if file.is_null() || software.is_null() {
            return;
        }
        let path = c"asdf_library";
        unsafe { asdf_set_software(file, path.as_ptr(), software) };
    })
}

/// Override only the version of the `asdf_library` metadata.
///
/// # Safety
/// `file` must be a valid file handle and `version` a valid NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_library_set_version(file: *mut AsdfFile, version: *const c_char) {
    guard("asdf_library_set_version", (), || {
        if file.is_null() || version.is_null() {
            return;
        }
        let path = c"asdf_library/version";
        let value = unsafe { crate::value_ffi::asdf_value_of_string0(file, version) };
        if value.is_null() {
            return;
        }
        unsafe { crate::file_ffi::set_value_at(file, path.as_ptr(), value) };
        unsafe { crate::file_ffi::asdf_value_destroy(value) };
    })
}

/// Parse a tag string, exposed for extension authors.
///
/// # Safety
/// See [`crate::extension_ffi::asdf_tag_parse`], which this forwards to.
pub unsafe fn parse_tag(tag: *const c_char) -> *mut asdf_tag_t {
    unsafe { crate::extension_ffi::asdf_tag_parse(tag) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_ffi::{asdf_close, asdf_open_mem_ex, asdf_write_to_mem};

    struct Handle(*mut AsdfFile);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { asdf_close(self.0) };
        }
    }

    fn sample() -> Handle {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(
            b"asdf_library: !core/software-1.0.0\n  \
              author: The ASDF Developers\n  \
              homepage: 'http://github.com/asdf-format/asdf'\n  \
              name: asdf\n  version: 4.1.0\n\
              history:\n  extensions:\n  - !core/extension_metadata-1.0.0\n    \
              extension_class: asdf.extension._manifest.ManifestExtension\n    \
              software: !core/software-1.0.0 {name: asdf, version: 4.1.0}\n",
        );
        buf.extend_from_slice(b"...\n");
        let f = unsafe { asdf_open_mem_ex(buf.as_ptr().cast(), buf.len(), std::ptr::null_mut()) };
        assert!(!f.is_null());
        Handle(f)
    }

    #[test]
    fn reads_the_asdf_library_software() {
        let h = sample();
        let path = c"asdf_library";
        assert!(unsafe { asdf_is_software(h.0, path.as_ptr()) });

        let mut software: *mut asdf_software_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_software(h.0, path.as_ptr(), &mut software) },
            AsdfValueErr::Ok
        );
        assert!(!software.is_null());

        let view = unsafe { &*software };
        assert_eq!(unsafe { CStr::from_ptr(view.name) }.to_str().unwrap(), "asdf");
        assert!(!view.version.is_null());
        let version = unsafe { &*view.version };
        assert_eq!((version.major, version.minor, version.patch), (4, 1, 0));
        assert_eq!(unsafe { CStr::from_ptr(view.author) }.to_str().unwrap(), "The ASDF Developers");

        unsafe { asdf_software_destroy(software) };
    }

    #[test]
    fn a_wrong_tag_is_a_mismatch() {
        let h = sample();
        let path = c"history";
        assert!(!unsafe { asdf_is_software(h.0, path.as_ptr()) });

        let mut software: *mut asdf_software_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_software(h.0, path.as_ptr(), &mut software) },
            AsdfValueErr::TypeMismatch
        );
        assert!(software.is_null());
    }

    #[test]
    fn a_missing_path_is_not_found() {
        let h = sample();
        let path = c"nope";
        let mut software: *mut asdf_software_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_software(h.0, path.as_ptr(), &mut software) },
            AsdfValueErr::NotFound
        );
    }

    #[test]
    fn software_round_trips_through_a_written_file() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let h = Handle(f);

        let version = unsafe { asdf_version_parse(c"2.3.4".as_ptr()) };
        let software = asdf_software_t {
            name: c"my-writer".as_ptr(),
            version: version.cast_const(),
            author: c"Someone".as_ptr(),
            homepage: c"https://example.com".as_ptr(),
        };

        let path = c"asdf_library";
        assert_eq!(unsafe { asdf_set_software(h.0, path.as_ptr(), &software) }, AsdfValueErr::Ok);

        let mut buf: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut size = 0usize;
        assert_eq!(unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) }, 0);

        let reopened = unsafe { asdf_open_mem_ex(buf, size, std::ptr::null_mut()) };
        let r = Handle(reopened);

        let mut read_back: *mut asdf_software_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_software(r.0, path.as_ptr(), &mut read_back) },
            AsdfValueErr::Ok
        );
        let view = unsafe { &*read_back };
        assert_eq!(unsafe { CStr::from_ptr(view.name) }.to_str().unwrap(), "my-writer");
        assert_eq!(unsafe { &*view.version }.minor, 3);

        unsafe { asdf_software_destroy(read_back) };
        unsafe { libc::free(buf) };
        unsafe { crate::version_ffi::asdf_version_destroy(version) };
    }

    #[test]
    fn copies_are_independent() {
        let h = sample();
        let path = c"asdf_library";
        let mut software: *mut asdf_software_t = std::ptr::null_mut();
        unsafe { asdf_get_software(h.0, path.as_ptr(), &mut software) };

        let copy = unsafe { asdf_software_copy(h.0, software) };
        assert!(!copy.is_null());
        // Distinct allocations for every owned field.
        unsafe {
            assert_ne!((*copy).name, (*software).name);
            assert_ne!((*copy).version, (*software).version);
        }

        // Freeing the original must leave the copy intact.
        unsafe { asdf_software_destroy(software) };
        assert_eq!(unsafe { CStr::from_ptr((*copy).name) }.to_str().unwrap(), "asdf");
        unsafe { asdf_software_destroy(copy) };
    }

    #[test]
    fn deinit_is_safe_on_a_zeroed_object() {
        // The header requires this: the generated copy wrapper zeroes `dst`
        // and de-initialises it on failure.
        let mut zeroed = asdf_software_t::zeroed();
        unsafe { asdf_software_deinit(&mut zeroed) };
        unsafe { asdf_software_deinit(&mut zeroed) };
        unsafe { asdf_software_deinit(std::ptr::null_mut()) };
    }

    #[test]
    fn copy_into_zeroes_the_destination_first() {
        let h = sample();
        let path = c"asdf_library";
        let mut software: *mut asdf_software_t = std::ptr::null_mut();
        unsafe { asdf_get_software(h.0, path.as_ptr(), &mut software) };

        let mut destination = asdf_software_t::zeroed();
        assert!(unsafe { asdf_software_copy_into(h.0, software, &mut destination) });
        assert_eq!(unsafe { CStr::from_ptr(destination.name) }.to_str().unwrap(), "asdf");

        unsafe { asdf_software_deinit(&mut destination) };
        unsafe { asdf_software_destroy(software) };
    }

    #[test]
    fn arrays_of_objects_copy() {
        let h = sample();
        let path = c"asdf_library";
        let mut software: *mut asdf_software_t = std::ptr::null_mut();
        unsafe { asdf_get_software(h.0, path.as_ptr(), &mut software) };

        let mut array: [*const asdf_software_t; 2] = [software, std::ptr::null()];
        let copies = unsafe { asdf_software_array_copy(h.0, array.as_mut_ptr()) };
        assert!(!copies.is_null());

        let first = unsafe { *copies };
        assert!(!first.is_null());
        assert_eq!(unsafe { CStr::from_ptr((*first).name) }.to_str().unwrap(), "asdf");
        // Null-terminated, as the C helper produces.
        assert!(unsafe { *copies.offset(1) }.is_null());

        unsafe { asdf_software_destroy(first) };
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(copies, 2)) });
        unsafe { asdf_software_destroy(software) };
    }

    #[test]
    fn reads_extension_metadata() {
        let h = sample();
        let path = c"history/extensions/0";
        assert!(unsafe { asdf_is_extension_metadata(h.0, path.as_ptr()) });

        let mut metadata: *mut asdf_extension_metadata_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_extension_metadata(h.0, path.as_ptr(), &mut metadata) },
            AsdfValueErr::Ok
        );
        let view = unsafe { &*metadata };
        assert_eq!(
            unsafe { CStr::from_ptr(view.extension_class) }.to_str().unwrap(),
            "asdf.extension._manifest.ManifestExtension"
        );
        assert!(!view.package.is_null(), "the nested software must be read");
        assert_eq!(unsafe { CStr::from_ptr((*view.package).name) }.to_str().unwrap(), "asdf");

        unsafe { asdf_extension_metadata_destroy(metadata) };
    }

    #[test]
    fn null_handles_are_tolerated() {
        let mut out: *mut asdf_software_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_value_as_software(std::ptr::null_mut(), &mut out) },
            AsdfValueErr::TypeMismatch
        );
        assert!(
            unsafe { asdf_value_of_software(std::ptr::null_mut(), std::ptr::null()) }.is_null()
        );
        assert!(unsafe { asdf_software_copy(std::ptr::null_mut(), std::ptr::null()) }.is_null());
        assert!(!unsafe {
            asdf_software_copy_into(std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut())
        });
        unsafe { asdf_software_destroy(std::ptr::null_mut()) };
        unsafe { asdf_library_set(std::ptr::null_mut(), std::ptr::null()) };
        unsafe { asdf_library_set_version(std::ptr::null_mut(), std::ptr::null()) };
    }
}
