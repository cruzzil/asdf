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

use std::ffi::{CStr, CString, c_char, c_int};

use asdf_core::yaml::{Document, NodeId, Tag};

use crate::extension_ffi::{asdf_software_t, asdf_tag_t};
use crate::file_ffi::{AsdfFile, AsdfValue, value_document, value_file, value_node};
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
    let Some(text) = (unsafe { crate::ffi::c_str(ptr) }) else {
        return std::ptr::null();
    };
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
        tags: $tags:expr,
        ext_build_fn: $ext_build_fn:ident,
        ext_deserialize_fn: $ext_deserialize_fn:ident,
        ext_serialize_fn: $ext_serialize_fn:ident,
        ext_copy_fn: $ext_copy_fn:ident,
        ext_deinit_fn: $ext_deinit_fn:ident,
    ) => {
        /// Whether a value carries one of this extension's tags.
        ///
        /// Every schema version the extension declares counts, not just the
        /// newest: `core/asdf-1.0.0` and `-1.1.0` share a deserializer, and
        /// `time/time` has five versions behind one. Matching only the
        /// newest would leave most of the reference corpus unreadable.
        ///
        /// # Safety
        /// `value` must be null or a valid value handle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $value_is_fn(value: *mut AsdfValue) -> bool {
            guard(stringify!($value_is_fn), false, || {
                let (Some(doc), Some(node)) = (value_document(value), value_node(value)) else {
                    return false;
                };
                doc.tag_of(node).is_some_and(|found| {
                    let found = found.full();
                    let tags: &[&CStr] = $tags;
                    tags.iter().any(|t| t.to_bytes() == found.as_bytes())
                })
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
                // The file goes through too: an extension whose object holds
                // a value -- `extension_metadata`'s spare properties, say --
                // needs one to build a handle against.
                let file = value_file(value).unwrap_or(std::ptr::null_mut());
                let boxed: Box<$ty> = Box::new(<$ty>::zeroed());
                let raw = Box::into_raw(boxed);
                match $deserialize(doc, node, file, raw) {
                    AsdfValueErr::Ok => {
                        unsafe { write_out(out, raw) };
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

        // ---- Registry entry ------------------------------------------
        //
        // `ASDF_REGISTER_EXTENSION` puts each core extension in the
        // process-wide registry, and generic code reaches them only that
        // way: `asdf_extension_get(file, tag)` followed by
        // `asdf_value_as_extension_type`. Upstream's own
        // `test-reference-files` drives every tagged value in the corpus
        // through exactly that path, so the typed functions above are not
        // enough on their own.

        /// Deserialize through the registry's generic entry point.
        ///
        /// Deliberately *not* routed through the typed
        /// `asdf_value_as_<name>`: that one checks the tag first, and a
        /// vtable method must not. The tag check belongs to
        /// `asdf_value_as_extension_type`, which is what lets a caller
        /// deserialize an untagged value -- an ndarray's `datatype`, say --
        /// by reaching for the vtable directly, as upstream's own test does.
        ///
        /// # Safety
        /// `value` must be a valid value handle and `out` writable.
        unsafe extern "C" fn $ext_deserialize_fn(
            value: *mut AsdfValue,
            _userdata: *const std::ffi::c_void,
            out: *mut *mut std::ffi::c_void,
        ) -> AsdfValueErr {
            guard(stringify!($ext_deserialize_fn), AsdfValueErr::Unknown, || {
                if out.is_null() {
                    return AsdfValueErr::Unknown;
                }
                let (Some(doc), Some(node)) = (value_document(value), value_node(value)) else {
                    return AsdfValueErr::Unknown;
                };
                let file = value_file(value).unwrap_or(std::ptr::null_mut());
                let raw = Box::into_raw(Box::new(<$ty>::zeroed()));
                match $deserialize(doc, node, file, raw) {
                    AsdfValueErr::Ok => {
                        unsafe { write_out(out, raw.cast::<std::ffi::c_void>()) };
                        AsdfValueErr::Ok
                    }
                    err => {
                        unsafe { $destroy_fn(raw) };
                        err
                    }
                }
            })
        }

        /// Serialize through the registry's generic entry point.
        ///
        /// # Safety
        /// `obj` must be a valid object of this extension's type.
        unsafe extern "C" fn $ext_serialize_fn(
            file: *mut AsdfFile,
            obj: *const std::ffi::c_void,
            _userdata: *const std::ffi::c_void,
        ) -> *mut AsdfValue {
            unsafe { $value_of_fn(file, obj.cast::<$ty>()) }
        }

        /// Deep-copy through the registry's generic entry point.
        ///
        /// # Safety
        /// `src` and `dst` must be valid objects of this extension's type.
        unsafe extern "C" fn $ext_copy_fn(
            file: *mut AsdfFile,
            src: *const std::ffi::c_void,
            dst: *mut std::ffi::c_void,
        ) -> bool {
            unsafe { $copy_into_fn(file, src.cast::<$ty>(), dst.cast::<$ty>()) }
        }

        /// De-initialise through the registry's generic entry point.
        ///
        /// # Safety
        /// `obj` must be a valid object of this extension's type.
        unsafe extern "C" fn $ext_deinit_fn(obj: *mut std::ffi::c_void) {
            unsafe { $deinit_fn(obj.cast::<$ty>()) };
        }

        /// Build this extension's registry entry.
        ///
        /// The parts are leaked deliberately: `ASDF_REGISTER_EXTENSION`
        /// makes them file-scope `static`s, and the registry stores the
        /// pointer rather than a copy, so they must outlive every use. Seven
        /// of these exist for the life of the process.
        fn $ext_build_fn() -> *mut crate::extension_ffi::asdf_extension_t {
            use crate::extension_ffi::{asdf_extension_t, asdf_extension_vtab_t, libasdf_software};

            let mut tags: Vec<*const c_char> = $tags.iter().map(|t: &&CStr| t.as_ptr()).collect();
            tags.push(std::ptr::null());
            let tags = Box::leak(tags.into_boxed_slice());

            let vtab = Box::leak(Box::new(asdf_extension_vtab_t {
                serialize: Some($ext_serialize_fn),
                deserialize: Some($ext_deserialize_fn),
                copy: Some($ext_copy_fn),
                deinit: Some($ext_deinit_fn),
                _reserved: [None; 4],
            }));

            Box::leak(Box::new(asdf_extension_t {
                tags: tags.as_ptr(),
                software: (&raw const libasdf_software)
                    .cast::<crate::extension_ffi::asdf_software_t>()
                    .cast_mut(),
                vtab: std::ptr::from_ref(vtab),
                size: std::mem::size_of::<$ty>(),
                userdata: std::ptr::null_mut(),
            }))
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

fn software_deserialize(
    doc: &Document,
    node: NodeId,
    _file: *mut AsdfFile,
    out: *mut asdf_software_t,
) -> AsdfValueErr {
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
    // The tag list upstream's `ASDF_REGISTER_EXTENSION` declares.
    tags: &[c"tag:stsci.edu:asdf/core/software-1.0.0"],
    ext_build_fn: build_software_extension,
    ext_deserialize_fn: software_ext_deserialize,
    ext_serialize_fn: software_ext_serialize,
    ext_copy_fn: software_ext_copy,
    ext_deinit_fn: software_ext_deinit,
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
    file: *mut AsdfFile,
    out: *mut asdf_extension_metadata_t,
) -> AsdfValueErr {
    let class = string_field(doc, node, "extension_class");
    if class.is_null() {
        return AsdfValueErr::ParseFailure;
    }

    // Only `package`. The schema also has `software` and
    // `manifest_software` keys, but those are not the package -- reading
    // either into `package` makes a file that has no package look as though
    // it has one, which is exactly what upstream's own test checks against.
    let package = doc
        .mapping_get(node, "package")
        .map(|id| {
            let raw = Box::into_raw(Box::new(asdf_software_t::zeroed()));
            if software_deserialize(doc, id, file, raw) == AsdfValueErr::Ok {
                raw.cast_const()
            } else {
                drop(unsafe { Box::from_raw(raw) });
                std::ptr::null()
            }
        })
        .unwrap_or(std::ptr::null());

    // `metadata` is the whole mapping, so a caller can reach the properties
    // the struct has no field for -- `extension_uri`, `manifest_software`.
    let metadata = if file.is_null() {
        std::ptr::null_mut()
    } else {
        crate::value_ffi::make_value(file, node)
    };

    unsafe {
        (*out).extension_class = class;
        (*out).package = package;
        (*out).metadata = metadata;
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
        // `package`, matching the key the deserializer reads. Writing it as
        // `software` would make the value fail to round-trip through its own
        // reader, which is what upstream's serialize test catches.
        let k = doc.add_scalar("package");
        pairs.push((k, node));
    }

    // Any further properties the struct has no field for -- `extension_uri`,
    // `manifest_software` -- ride along in `metadata`. The two keys with
    // fields of their own are skipped so they cannot be written twice.
    if let Some(node) = value_node(obj.metadata)
        && let Some(entries) = doc.mapping_entries(doc.resolve(node)).map(<[_]>::to_vec)
    {
        for entry in entries {
            let key = doc.resolved(entry.key).as_str().map(str::to_string);
            if matches!(key.as_deref(), Some("extension_class" | "package")) {
                continue;
            }
            pairs.push((entry.key, entry.value));
        }
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
    // The tag list upstream's `ASDF_REGISTER_EXTENSION` declares.
    tags: &[c"tag:stsci.edu:asdf/core/extension_metadata-1.0.0"],
    ext_build_fn: build_extension_metadata_extension,
    ext_deserialize_fn: extension_metadata_ext_deserialize,
    ext_serialize_fn: extension_metadata_ext_serialize,
    ext_copy_fn: extension_metadata_ext_copy,
    ext_deinit_fn: extension_metadata_ext_deinit,
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
              extension_uri: asdf://asdf-format.org/core/extensions/core-1.6.0\n    \
              package: !core/software-1.0.0 {name: asdf_standard, version: 1.1.1}\n    \
              software: !core/software-1.0.0 {name: asdf, version: 4.1.0}\n  \
              - !core/extension_metadata-1.0.0\n    \
              extension_class: some.other.Extension\n    \
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
        // `package` is the `package` key alone. `software` and
        // `manifest_software` are different things, and reading either into
        // `package` would make a file that has none look as though it does.
        assert!(!view.package.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr((*view.package).name) }.to_str().unwrap(),
            "asdf_standard"
        );

        // Everything the struct has no field for stays reachable through
        // `metadata`, which is the whole mapping.
        assert!(!view.metadata.is_null());
        let uri = c"extension_uri";
        let entry = unsafe { crate::value_ffi::asdf_mapping_get(view.metadata, uri.as_ptr()) };
        assert!(!entry.is_null());
        let mut text = std::ptr::null();
        assert_eq!(
            unsafe { crate::value_ffi::asdf_value_as_string0(entry, &mut text) },
            AsdfValueErr::Ok
        );
        assert_eq!(
            unsafe { CStr::from_ptr(text) }.to_str().unwrap(),
            "asdf://asdf-format.org/core/extensions/core-1.6.0"
        );
        unsafe { crate::file_ffi::asdf_value_destroy(entry) };
        unsafe { asdf_extension_metadata_destroy(metadata) };
    }

    /// An entry with no `package` must report none, rather than borrowing
    /// the `software` beside it.
    #[test]
    fn extension_metadata_without_a_package_reports_none() {
        let h = sample();
        let path = c"history/extensions/1";
        let mut metadata: *mut asdf_extension_metadata_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_extension_metadata(h.0, path.as_ptr(), &mut metadata) },
            AsdfValueErr::Ok
        );
        let view = unsafe { &*metadata };
        assert!(view.package.is_null());
        assert!(!view.metadata.is_null(), "the mapping is still reachable");
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

// ---- time/time -------------------------------------------------------

use crate::time_ffi::{
    TIME_TAG, asdf_time_t, time_copy, time_deinit, time_deserialize, time_serialize,
};

declare_extension! {
    name: time,
    ty: asdf_time_t,
    tag: TIME_TAG,
    deserialize: time_deserialize,
    serialize: time_serialize,
    deinit: time_deinit,
    copy: time_copy,
    is_fn: asdf_is_time,
    value_is_fn: asdf_value_is_time,
    value_as_fn: asdf_value_as_time,
    value_of_fn: asdf_value_of_time,
    get_fn: asdf_get_time,
    set_fn: asdf_set_time,
    copy_fn: asdf_time_copy,
    copy_into_fn: asdf_time_copy_into,
    array_copy_fn: asdf_time_array_copy,
    deinit_fn: asdf_time_deinit,
    destroy_fn: asdf_time_destroy,
    // The tag list upstream's `ASDF_REGISTER_EXTENSION` declares.
    tags: &[
        c"tag:stsci.edu:asdf/time/time-1.4.0",
        c"tag:stsci.edu:asdf/time/time-1.3.0",
        c"tag:stsci.edu:asdf/time/time-1.2.0",
        c"tag:stsci.edu:asdf/time/time-1.1.0",
        c"tag:stsci.edu:asdf/time/time-1.0.0",
    ],
    ext_build_fn: build_time_extension,
    ext_deserialize_fn: time_ext_deserialize,
    ext_serialize_fn: time_ext_serialize,
    ext_copy_fn: time_ext_copy,
    ext_deinit_fn: time_ext_deinit,
}

// ---- core/history_entry ----------------------------------------------

/// The tag for `core/history_entry`.
pub const HISTORY_ENTRY_TAG: &str = "tag:stsci.edu:asdf/core/history_entry-1.0.0";

/// Mirror of `asdf_history_entry_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_history_entry_t {
    /// What the entry records.
    pub description: *const c_char,
    /// When it happened, if the entry says.
    pub time: *const asdf_time_t,
    /// A null-terminated array of the software involved.
    pub software: *mut *const asdf_software_t,
}

impl asdf_history_entry_t {
    fn zeroed() -> Self {
        Self {
            description: std::ptr::null(),
            time: std::ptr::null(),
            software: std::ptr::null_mut(),
        }
    }
}

/// Read a null-terminated software array from a mapping's `software` key.
///
/// The schema allows either one object or a sequence of them.
fn read_software_list(
    doc: &Document,
    node: NodeId,
    file: *mut AsdfFile,
) -> *mut *const asdf_software_t {
    let Some(entry) = doc.mapping_get(node, "software") else {
        return std::ptr::null_mut();
    };

    let nodes: Vec<NodeId> = match doc.sequence_items(entry) {
        Some(items) => items.to_vec(),
        // A single object rather than a list.
        None => vec![entry],
    };

    let mut list: Vec<*const asdf_software_t> = Vec::with_capacity(nodes.len() + 1);
    for item in nodes {
        let raw = Box::into_raw(Box::new(asdf_software_t::zeroed()));
        if software_deserialize(doc, item, file, raw) == AsdfValueErr::Ok {
            list.push(raw.cast_const());
        } else {
            drop(unsafe { Box::from_raw(raw) });
        }
    }
    if list.is_empty() {
        return std::ptr::null_mut();
    }
    list.push(std::ptr::null());
    list.shrink_to_fit();
    Box::into_raw(list.into_boxed_slice()).cast::<*const asdf_software_t>()
}

/// Free a software array produced by [`read_software_list`].
unsafe fn free_software_list(list: *mut *const asdf_software_t) {
    if list.is_null() {
        return;
    }
    let mut count = 0isize;
    while !unsafe { *list.offset(count) }.is_null() {
        unsafe { asdf_software_destroy((*list.offset(count)).cast_mut()) };
        count += 1;
    }
    // The array itself was a boxed slice, including its null terminator.
    let slice = std::ptr::slice_from_raw_parts_mut(list, count as usize + 1);
    drop(unsafe { Box::from_raw(slice) });
}

fn history_entry_deserialize(
    doc: &Document,
    node: NodeId,
    file: *mut AsdfFile,
    out: *mut asdf_history_entry_t,
) -> AsdfValueErr {
    let description = string_field(doc, node, "description");

    let time = doc
        .mapping_get(node, "time")
        .map(|id| {
            let raw = Box::into_raw(Box::new(asdf_time_t::zeroed()));
            if time_deserialize(doc, id, file, raw) == AsdfValueErr::Ok {
                raw.cast_const()
            } else {
                drop(unsafe { Box::from_raw(raw) });
                std::ptr::null()
            }
        })
        .unwrap_or(std::ptr::null());

    unsafe {
        (*out).description = description;
        (*out).time = time;
        (*out).software = read_software_list(doc, node, file);
    }
    AsdfValueErr::Ok
}

fn history_entry_serialize(doc: &mut Document, obj: &asdf_history_entry_t) -> Option<NodeId> {
    let mut pairs = Vec::new();

    if !obj.description.is_null() {
        let text = unsafe { CStr::from_ptr(obj.description) }.to_string_lossy().into_owned();
        let key = doc.add_scalar("description");
        let value = doc.add_scalar_styled(text, asdf_core::yaml::ScalarStyle::SingleQuoted);
        pairs.push((key, value));
    }
    if !obj.time.is_null()
        && let Some(node) = time_serialize(doc, unsafe { &*obj.time })
    {
        doc.node_mut(node).tag = Some(Tag::parse(TIME_TAG));
        let key = doc.add_scalar("time");
        pairs.push((key, node));
    }
    if !obj.software.is_null() {
        let mut items = Vec::new();
        let mut index = 0isize;
        while !unsafe { *obj.software.offset(index) }.is_null() {
            let entry = unsafe { *obj.software.offset(index) };
            if let Some(node) = software_serialize(doc, unsafe { &*entry }) {
                doc.node_mut(node).tag = Some(Tag::parse(SOFTWARE_TAG));
                items.push(node);
            }
            index += 1;
        }
        if !items.is_empty() {
            let list = doc.add_sequence(items);
            let key = doc.add_scalar("software");
            pairs.push((key, list));
        }
    }

    (!pairs.is_empty()).then(|| doc.add_mapping(pairs))
}

unsafe fn history_entry_deinit(obj: *mut asdf_history_entry_t) {
    let entry = unsafe { &mut *obj };
    unsafe { free_c_string(entry.description) };
    if !entry.time.is_null() {
        unsafe { asdf_time_destroy(entry.time.cast_mut()) };
    }
    unsafe { free_software_list(entry.software) };
    *entry = asdf_history_entry_t::zeroed();
}

unsafe fn history_entry_copy(src: &asdf_history_entry_t, dst: *mut asdf_history_entry_t) -> bool {
    let out = unsafe { &mut *dst };
    out.description = unsafe { clone_c_string(src.description) };
    out.time = if src.time.is_null() {
        std::ptr::null()
    } else {
        unsafe { asdf_time_copy(std::ptr::null_mut(), src.time) }.cast_const()
    };
    out.software = if src.software.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { asdf_software_array_copy(std::ptr::null_mut(), src.software) }
            .cast::<*const asdf_software_t>()
    };
    true
}

declare_extension! {
    name: history_entry,
    ty: asdf_history_entry_t,
    tag: HISTORY_ENTRY_TAG,
    deserialize: history_entry_deserialize,
    serialize: history_entry_serialize,
    deinit: history_entry_deinit,
    copy: history_entry_copy,
    is_fn: asdf_is_history_entry,
    value_is_fn: asdf_value_is_history_entry,
    value_as_fn: asdf_value_as_history_entry,
    value_of_fn: asdf_value_of_history_entry,
    get_fn: asdf_get_history_entry,
    set_fn: asdf_set_history_entry,
    copy_fn: asdf_history_entry_copy,
    copy_into_fn: asdf_history_entry_copy_into,
    array_copy_fn: asdf_history_entry_array_copy,
    deinit_fn: asdf_history_entry_deinit,
    destroy_fn: asdf_history_entry_destroy,
    // The tag list upstream's `ASDF_REGISTER_EXTENSION` declares.
    tags: &[c"tag:stsci.edu:asdf/core/history_entry-1.0.0"],
    ext_build_fn: build_history_entry_extension,
    ext_deserialize_fn: history_entry_ext_deserialize,
    ext_serialize_fn: history_entry_ext_serialize,
    ext_copy_fn: history_entry_ext_copy,
    ext_deinit_fn: history_entry_ext_deinit,
}

/// Append a history entry to the file.
///
/// # Safety
/// `file` must be a file handle open for writing and `description` a valid
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_history_entry_add(
    file: *mut AsdfFile,
    description: *const c_char,
) -> c_int {
    guard("asdf_history_entry_add", -1, || {
        if file.is_null() || description.is_null() {
            return -1;
        }
        let entry = asdf_history_entry_t {
            description,
            time: std::ptr::null(),
            software: std::ptr::null_mut(),
        };
        let value = unsafe { asdf_value_of_history_entry(file, &entry) };
        if value.is_null() {
            return -1;
        }

        // Entries accumulate under history/entries, which is created on the
        // first call.
        let handle = unsafe { &mut *file };
        let Some(node) = crate::file_ffi::value_node(value) else {
            unsafe { crate::file_ffi::asdf_value_destroy(value) };
            return -1;
        };
        let Some(doc) = handle.document_for_values() else {
            unsafe { crate::file_ffi::asdf_value_destroy(value) };
            return -1;
        };

        let existing = doc.lookup_str("history/entries");
        let list = match existing {
            Some(list) if doc.resolved(list).is_sequence() => doc.resolve(list),
            _ => {
                let fresh = doc.add(asdf_core::yaml::Node::sequence());
                if doc.insert_at_str("history/entries", fresh).is_err() {
                    unsafe { crate::file_ffi::asdf_value_destroy(value) };
                    return -1;
                }
                fresh
            }
        };
        if let asdf_core::yaml::NodeData::Sequence { items, .. } = &mut doc.node_mut(list).data {
            items.push(node);
        }

        unsafe { crate::file_ffi::asdf_value_destroy(value) };
        0
    })
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::file_ffi::{asdf_close, asdf_open_mem_ex, asdf_write_to_mem};
    use crate::time_ffi::asdf_time_t;

    struct Handle(*mut AsdfFile);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { asdf_close(self.0) };
        }
    }

    fn open(tree: &str) -> Handle {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(tree.as_bytes());
        buf.extend_from_slice(b"...\n");
        let f = unsafe { asdf_open_mem_ex(buf.as_ptr().cast(), buf.len(), std::ptr::null_mut()) };
        assert!(!f.is_null());
        Handle(f)
    }

    #[test]
    fn reads_a_history_entry() {
        let h = open(
            "entry: !core/history_entry-1.0.0\n  \
             description: 'reprocessed with a new flat'\n  \
             time: !time/time-1.4.0 '2026-09-04T12:00:00'\n  \
             software:\n  - !core/software-1.0.0 {name: mypipeline, version: 1.2.3}\n",
        );

        let path = c"entry";
        assert!(unsafe { asdf_is_history_entry(h.0, path.as_ptr()) });

        let mut entry: *mut asdf_history_entry_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_history_entry(h.0, path.as_ptr(), &mut entry) },
            AsdfValueErr::Ok
        );
        let view = unsafe { &*entry };
        assert_eq!(
            unsafe { CStr::from_ptr(view.description) }.to_str().unwrap(),
            "reprocessed with a new flat"
        );

        // The nested time, decoded.
        assert!(!view.time.is_null());
        let time = unsafe { &*view.time };
        assert_eq!(time.info.tm.tm_year, 2026 - 1900);
        assert_eq!(time.info.tm.tm_mday, 4);

        // The software list, null-terminated.
        assert!(!view.software.is_null());
        let first = unsafe { *view.software };
        assert!(!first.is_null());
        assert_eq!(unsafe { CStr::from_ptr((*first).name) }.to_str().unwrap(), "mypipeline");
        assert!(unsafe { *view.software.offset(1) }.is_null(), "list must be terminated");

        unsafe { asdf_history_entry_destroy(entry) };
    }

    #[test]
    fn a_single_software_object_is_accepted() {
        // The schema allows one object where a list would also do.
        let h = open(
            "entry: !core/history_entry-1.0.0\n  description: 'x'\n  \
             software: !core/software-1.0.0 {name: solo, version: 0.1.0}\n",
        );
        let mut entry: *mut asdf_history_entry_t = std::ptr::null_mut();
        unsafe { asdf_get_history_entry(h.0, c"entry".as_ptr(), &mut entry) };
        let view = unsafe { &*entry };
        assert!(!view.software.is_null());
        let first = unsafe { *view.software };
        assert_eq!(unsafe { CStr::from_ptr((*first).name) }.to_str().unwrap(), "solo");
        unsafe { asdf_history_entry_destroy(entry) };
    }

    #[test]
    fn history_entries_can_be_added_and_read_back() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let h = Handle(f);

        assert_eq!(unsafe { asdf_history_entry_add(h.0, c"first change".as_ptr()) }, 0);
        assert_eq!(unsafe { asdf_history_entry_add(h.0, c"second change".as_ptr()) }, 0);

        let mut buf: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut size = 0usize;
        assert_eq!(unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) }, 0);

        let reopened = unsafe { asdf_open_mem_ex(buf, size, std::ptr::null_mut()) };
        let r = Handle(reopened);

        // Both entries must be there, in order.
        for (index, expected) in [(0, "first change"), (1, "second change")] {
            let path = CString::new(format!("history/entries/{index}")).unwrap();
            let mut entry: *mut asdf_history_entry_t = std::ptr::null_mut();
            assert_eq!(
                unsafe { asdf_get_history_entry(r.0, path.as_ptr(), &mut entry) },
                AsdfValueErr::Ok,
                "entry {index}"
            );
            assert_eq!(unsafe { CStr::from_ptr((*entry).description) }.to_str().unwrap(), expected);
            unsafe { asdf_history_entry_destroy(entry) };
        }

        unsafe { libc::free(buf) };
    }

    #[test]
    fn a_history_entry_round_trips_through_a_written_file() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let h = Handle(f);

        let value = CString::new("2026-09-04T12:00:00").unwrap();
        let time = asdf_time_t { value: value.as_ptr().cast_mut(), ..asdf_time_t::zeroed() };
        let entry = asdf_history_entry_t {
            description: c"a described change".as_ptr(),
            time: &time,
            software: std::ptr::null_mut(),
        };

        assert_eq!(
            unsafe { asdf_set_history_entry(h.0, c"note".as_ptr(), &entry) },
            AsdfValueErr::Ok
        );

        let mut buf: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut size = 0usize;
        unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) };
        let reopened = unsafe { asdf_open_mem_ex(buf, size, std::ptr::null_mut()) };
        let r = Handle(reopened);

        let mut read_back: *mut asdf_history_entry_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_history_entry(r.0, c"note".as_ptr(), &mut read_back) },
            AsdfValueErr::Ok
        );
        let view = unsafe { &*read_back };
        assert_eq!(
            unsafe { CStr::from_ptr(view.description) }.to_str().unwrap(),
            "a described change"
        );
        assert!(!view.time.is_null(), "the nested time must survive");

        unsafe { asdf_history_entry_destroy(read_back) };
        unsafe { libc::free(buf) };
    }

    #[test]
    fn a_time_value_round_trips() {
        let h = open("t: !time/time-1.4.0 '2026-09-04T12:34:56'\n");
        assert!(unsafe { asdf_is_time(h.0, c"t".as_ptr()) });

        let mut time: *mut asdf_time_t = std::ptr::null_mut();
        assert_eq!(unsafe { asdf_get_time(h.0, c"t".as_ptr(), &mut time) }, AsdfValueErr::Ok);
        let view = unsafe { &*time };
        assert_eq!(unsafe { CStr::from_ptr(view.value) }.to_str().unwrap(), "2026-09-04T12:34:56");
        assert_eq!(view.info.tm.tm_hour, 12);

        // The copy must be independent of the original.
        let copy = unsafe { asdf_time_copy(h.0, time) };
        assert!(!copy.is_null());
        unsafe { asdf_time_destroy(time) };
        assert_eq!(
            unsafe { CStr::from_ptr((*copy).value) }.to_str().unwrap(),
            "2026-09-04T12:34:56"
        );
        unsafe { asdf_time_destroy(copy) };
    }

    #[test]
    fn deinit_is_safe_on_zeroed_objects() {
        let mut entry = asdf_history_entry_t::zeroed();
        unsafe { asdf_history_entry_deinit(&mut entry) };
        unsafe { asdf_history_entry_deinit(&mut entry) };

        let mut time = asdf_time_t::zeroed();
        unsafe { asdf_time_deinit(&mut time) };
        unsafe { asdf_time_deinit(std::ptr::null_mut()) };
    }
}

// ---- core/datatype ---------------------------------------------------

use crate::ffi::write_out;
use crate::ndarray_ffi::asdf_datatype_t;

/// The tag for `core/datatype`.
pub const DATATYPE_TAG: &str = "tag:stsci.edu:asdf/core/datatype-1.0.0";

impl asdf_datatype_t {
    /// A zeroed instance.
    pub(crate) fn zeroed() -> Self {
        Self {
            type_: 0,
            size: 0,
            name: std::ptr::null(),
            byteorder: 0,
            ndim: 0,
            shape: std::ptr::null(),
            nfields: 0,
            fields: std::ptr::null(),
        }
    }
}

fn datatype_deserialize(
    doc: &Document,
    node: NodeId,
    _file: *mut AsdfFile,
    out: *mut asdf_datatype_t,
) -> AsdfValueErr {
    use asdf_core::core::datatype::Datatype;

    let Ok(parsed) = Datatype::parse(doc, node) else {
        return AsdfValueErr::ParseFailure;
    };

    // A datatype read on its own has no enclosing ndarray to inherit a byte
    // order from, and the standard does not say what one means alone. Both
    // libasdf and Python asdf take it as little-endian; see
    // https://github.com/asdf-format/asdf-standard/issues/501
    let default_order = asdf_core::core::datatype::ByteOrder::Little;
    let order_of = |order: asdf_core::core::datatype::ByteOrder| {
        if order == asdf_core::core::datatype::ByteOrder::Default { default_order } else { order }
    };

    // Field descriptors and their names are leaked into owned allocations
    // that `deinit` reclaims, so the pointers stay valid for the object's
    // life.
    let mut fields: Vec<asdf_datatype_t> = Vec::with_capacity(parsed.fields.len());
    for field in &parsed.fields {
        let name = field.name.as_deref().map(to_c_string).unwrap_or(std::ptr::null());
        let shape: Vec<u64> = field.datatype.shape.clone();
        let (shape_ptr, ndim) = if shape.is_empty() {
            (std::ptr::null(), 0)
        } else {
            let boxed = shape.into_boxed_slice();
            let len = boxed.len() as u32;
            (Box::into_raw(boxed).cast::<u64>().cast_const(), len)
        };
        fields.push(asdf_datatype_t {
            type_: field.datatype.scalar as i32,
            size: field.datatype.item_size(),
            name,
            byteorder: order_of(field.datatype.byteorder) as i32,
            ndim,
            shape: shape_ptr,
            nfields: 0,
            fields: std::ptr::null(),
        });
    }

    let (fields_ptr, nfields) = if fields.is_empty() {
        (std::ptr::null(), 0)
    } else {
        let len = fields.len() as u32;
        (Box::into_raw(fields.into_boxed_slice()).cast::<asdf_datatype_t>().cast_const(), len)
    };

    unsafe {
        (*out).type_ = parsed.scalar as i32;
        (*out).size = parsed.item_size();
        (*out).name = std::ptr::null();
        (*out).byteorder = order_of(parsed.byteorder) as i32;
        (*out).ndim = 0;
        (*out).shape = std::ptr::null();
        (*out).nfields = nfields;
        (*out).fields = fields_ptr;
    }
    AsdfValueErr::Ok
}

/// Whether a datatype is a plain scalar that needs no mapping around it.
///
/// Mirrors upstream's `asdf_datatype_is_simple_scalar`: not structured, no
/// name, no shape, no fields, and a byte order that need not be stated.
fn is_simple_scalar(obj: &asdf_datatype_t) -> bool {
    use asdf_core::core::datatype::ByteOrder;
    let scalar = crate::ndarray_ffi::scalar_from_abi_public(obj.type_);
    scalar != asdf_core::core::datatype::ScalarType::Structured
        && (obj.byteorder == 0 || obj.byteorder == ByteOrder::Little as i32)
        && obj.name.is_null()
        && obj.ndim == 0
        && obj.nfields == 0
}

/// Render a scalar datatype: its name, or `[kind, length]` for a string.
fn datatype_serialize_scalar(doc: &mut Document, obj: &asdf_datatype_t) -> Option<NodeId> {
    use asdf_core::core::datatype::ScalarType;

    let scalar = crate::ndarray_ffi::scalar_from_abi_public(obj.type_);
    if scalar.is_string() {
        // A string type is a [kind, length] pair, sized in characters.
        let characters = obj.size / scalar.bytes_per_char().max(1);
        let kind = doc.add_scalar(scalar.name());
        let length = doc.add_scalar(characters.to_string());
        let seq = doc.add_sequence(vec![kind, length]);
        if let asdf_core::yaml::NodeData::Sequence { style, .. } = &mut doc.node_mut(seq).data {
            *style = asdf_core::yaml::CollectionStyle::Flow;
        }
        return Some(seq);
    }
    (scalar != ScalarType::Unknown).then(|| doc.add_scalar(scalar.name()))
}

/// Render one field of a compound datatype as a mapping.
///
/// A field carries what a bare scalar cannot: its name, its own byte order,
/// and a sub-array shape.
fn datatype_serialize_field(doc: &mut Document, field: &asdf_datatype_t) -> Option<NodeId> {
    use asdf_core::core::datatype::ScalarType;

    let scalar = crate::ndarray_ffi::scalar_from_abi_public(field.type_);
    let mut pairs = Vec::new();

    if !field.name.is_null() {
        let text = unsafe { CStr::from_ptr(field.name) }.to_string_lossy().into_owned();
        let key = doc.add_scalar("name");
        let value = doc.add_scalar_styled(text, asdf_core::yaml::ScalarStyle::Plain);
        pairs.push((key, value));
    }

    let inner = if scalar == ScalarType::Structured {
        datatype_serialize_impl(doc, field, false)?
    } else {
        datatype_serialize_scalar(doc, field)?
    };
    let key = doc.add_scalar("datatype");
    pairs.push((key, inner));

    if field.byteorder != 0
        && let Some(order) = byteorder_name(field.byteorder)
    {
        let key = doc.add_scalar("byteorder");
        let value = doc.add_scalar(order);
        pairs.push((key, value));
    }

    if field.ndim > 0 && !field.shape.is_null() {
        let dims = unsafe { std::slice::from_raw_parts(field.shape, field.ndim as usize) };
        let items: Vec<NodeId> = dims.iter().map(|d| doc.add_scalar(d.to_string())).collect();
        let seq = doc.add_sequence(items);
        if let asdf_core::yaml::NodeData::Sequence { style, .. } = &mut doc.node_mut(seq).data {
            *style = asdf_core::yaml::CollectionStyle::Flow;
        }
        let key = doc.add_scalar("shape");
        pairs.push((key, seq));
    }

    let node = doc.add_mapping(pairs);
    // Python asdf writes a plain non-string scalar field inline and anything
    // richer in block style; upstream reproduces that, so we do too.
    if scalar != ScalarType::Structured
        && !scalar.is_string()
        && field.ndim == 0
        && let asdf_core::yaml::NodeData::Mapping { style, .. } = &mut doc.node_mut(node).data
    {
        *style = asdf_core::yaml::CollectionStyle::Flow;
    }
    Some(node)
}

/// The schema's name for a byte order discriminant.
fn byteorder_name(byteorder: i32) -> Option<&'static str> {
    use asdf_core::core::datatype::ByteOrder;
    if byteorder == ByteOrder::Little as i32 {
        Some("little")
    } else if byteorder == ByteOrder::Big as i32 {
        Some("big")
    } else {
        None
    }
}

/// Render a datatype, as a field of a compound type or on its own.
fn datatype_serialize_impl(
    doc: &mut Document,
    obj: &asdf_datatype_t,
    is_field: bool,
) -> Option<NodeId> {
    use asdf_core::core::datatype::ScalarType;

    let scalar = crate::ndarray_ffi::scalar_from_abi_public(obj.type_);

    if is_simple_scalar(obj) {
        return datatype_serialize_scalar(doc, obj);
    }
    if !is_field && scalar != ScalarType::Structured && obj.ndim == 0 {
        // A top-level scalar is written as its name even when its byte order
        // is not the default: the order belongs to the enclosing ndarray's
        // own `byteorder`, not repeated here. As a *field* it would need the
        // mapping form, which carries the per-field order.
        return datatype_serialize_scalar(doc, obj);
    }
    if is_field {
        return datatype_serialize_field(doc, obj);
    }
    if scalar == ScalarType::Structured {
        let fields = if obj.nfields > 0 && !obj.fields.is_null() {
            unsafe { std::slice::from_raw_parts(obj.fields, obj.nfields as usize) }
        } else {
            &[]
        };
        let items: Vec<NodeId> = fields
            .iter()
            .map(|field| datatype_serialize_impl(doc, field, true))
            .collect::<Option<Vec<_>>>()?;
        return Some(doc.add_sequence(items));
    }
    None
}

fn datatype_serialize(doc: &mut Document, obj: &asdf_datatype_t) -> Option<NodeId> {
    datatype_serialize_impl(doc, obj, false)
}

unsafe fn datatype_deinit(obj: *mut asdf_datatype_t) {
    let datatype = unsafe { &mut *obj };
    unsafe { datatype_free_storage(datatype) };
    *datatype = asdf_datatype_t::zeroed();
}

/// Release everything a datatype owns, without zeroing it.
///
/// Recursive, because a field may itself be structured; the fields array is
/// freed after its members, and each member's name and shape after that.
///
/// # Safety
/// `datatype` must own its `name`, `shape` and `fields`, as one produced by
/// the deserializer or by [`datatype_copy`] does.
unsafe fn datatype_free_storage(datatype: &mut asdf_datatype_t) {
    if !datatype.fields.is_null() && datatype.nfields > 0 {
        let count = datatype.nfields as usize;
        let slice = std::ptr::slice_from_raw_parts_mut(datatype.fields.cast_mut(), count);
        for index in 0..count {
            let field = unsafe { &mut *datatype.fields.cast_mut().add(index) };
            unsafe { datatype_free_storage(field) };
        }
        drop(unsafe { Box::from_raw(slice) });
        datatype.fields = std::ptr::null();
        datatype.nfields = 0;
    }
    if !datatype.shape.is_null() && datatype.ndim > 0 {
        let shape =
            std::ptr::slice_from_raw_parts_mut(datatype.shape.cast_mut(), datatype.ndim as usize);
        drop(unsafe { Box::from_raw(shape) });
        datatype.shape = std::ptr::null();
        datatype.ndim = 0;
    }
    unsafe { free_c_string(datatype.name) };
    datatype.name = std::ptr::null();
}

unsafe fn datatype_copy(src: &asdf_datatype_t, dst: *mut asdf_datatype_t) -> bool {
    let out = unsafe { &mut *dst };
    out.type_ = src.type_;
    out.size = src.size;
    out.byteorder = src.byteorder;
    out.name = unsafe { clone_c_string(src.name) };

    // A field's sub-array shape is its own storage, so the copy gets one
    // too: a shallow copy would leave two owners of the same allocation.
    if src.ndim > 0 && !src.shape.is_null() {
        let dims = unsafe { std::slice::from_raw_parts(src.shape, src.ndim as usize) };
        out.ndim = src.ndim;
        out.shape = Box::into_raw(dims.to_vec().into_boxed_slice()).cast::<u64>().cast_const();
    } else {
        out.ndim = 0;
        out.shape = std::ptr::null();
    }

    if src.nfields == 0 || src.fields.is_null() {
        out.nfields = 0;
        out.fields = std::ptr::null();
        return true;
    }

    let source = unsafe { std::slice::from_raw_parts(src.fields, src.nfields as usize) };
    let mut copies: Vec<asdf_datatype_t> = Vec::with_capacity(source.len());
    for field in source {
        let mut copy = asdf_datatype_t::zeroed();
        // Nested fields are one level deep in practice; a deeper nesting
        // recurses through this same path.
        if !unsafe { datatype_copy(field, &mut copy) } {
            return false;
        }
        copies.push(copy);
    }
    out.nfields = src.nfields;
    out.fields = Box::into_raw(copies.into_boxed_slice()).cast::<asdf_datatype_t>().cast_const();
    true
}

declare_extension! {
    name: datatype,
    ty: asdf_datatype_t,
    tag: DATATYPE_TAG,
    deserialize: datatype_deserialize,
    serialize: datatype_serialize,
    deinit: datatype_deinit,
    copy: datatype_copy,
    is_fn: asdf_is_datatype,
    value_is_fn: asdf_value_is_datatype,
    value_as_fn: asdf_value_as_datatype,
    value_of_fn: asdf_value_of_datatype,
    get_fn: asdf_get_datatype,
    set_fn: asdf_set_datatype,
    copy_fn: asdf_datatype_copy,
    copy_into_fn: asdf_datatype_copy_into,
    array_copy_fn: asdf_datatype_array_copy,
    deinit_fn: asdf_datatype_deinit,
    destroy_fn: asdf_datatype_destroy,
    // The tag list upstream's `ASDF_REGISTER_EXTENSION` declares.
    tags: &[c"tag:stsci.edu:asdf/core/datatype-1.0.0"],
    ext_build_fn: build_datatype_extension,
    ext_deserialize_fn: datatype_ext_deserialize,
    ext_serialize_fn: datatype_ext_serialize,
    ext_copy_fn: datatype_ext_copy,
    ext_deinit_fn: datatype_ext_deinit,
}

// ---- core/asdf (the tree's own metadata) -----------------------------

/// The tag for the tree root, `core/asdf`.
pub const META_TAG: &str = "tag:stsci.edu:asdf/core/asdf-1.1.0";

/// Mirror of `asdf_meta_history_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_meta_history_t {
    /// A null-terminated array of the extensions used.
    pub extensions: *mut *const asdf_extension_metadata_t,
    /// A null-terminated array of history entries.
    pub entries: *mut *const asdf_history_entry_t,
}

/// Mirror of `asdf_meta_t`, the `core/asdf` tree root.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_meta_t {
    /// The software that wrote the file.
    pub asdf_library: *mut asdf_software_t,
    /// The file's history.
    pub history: asdf_meta_history_t,
}

impl asdf_meta_t {
    fn zeroed() -> Self {
        Self {
            asdf_library: std::ptr::null_mut(),
            history: asdf_meta_history_t {
                extensions: std::ptr::null_mut(),
                entries: std::ptr::null_mut(),
            },
        }
    }
}

/// Read a null-terminated array of objects from a sequence.
fn read_list<T>(
    doc: &Document,
    node: Option<NodeId>,
    file: *mut AsdfFile,
    zeroed: fn() -> T,
    deserialize: fn(&Document, NodeId, *mut AsdfFile, *mut T) -> AsdfValueErr,
) -> *mut *const T {
    let Some(node) = node else {
        return std::ptr::null_mut();
    };
    let items: Vec<NodeId> = match doc.sequence_items(node) {
        Some(items) => items.to_vec(),
        None => vec![node],
    };

    let mut list: Vec<*const T> = Vec::with_capacity(items.len() + 1);
    for item in items {
        let raw = Box::into_raw(Box::new(zeroed()));
        if deserialize(doc, item, file, raw) == AsdfValueErr::Ok {
            list.push(raw.cast_const());
        } else {
            drop(unsafe { Box::from_raw(raw) });
        }
    }
    if list.is_empty() {
        return std::ptr::null_mut();
    }
    list.push(std::ptr::null());
    Box::into_raw(list.into_boxed_slice()).cast::<*const T>()
}

/// Free a list produced by [`read_list`].
///
/// The destructor is an `extern "C"` function, since these are the same
/// generated `destroy` entry points C callers use.
unsafe fn free_list<T>(list: *mut *const T, destroy: unsafe extern "C" fn(*mut T)) {
    if list.is_null() {
        return;
    }
    let mut count = 0isize;
    while !unsafe { *list.offset(count) }.is_null() {
        unsafe { destroy((*list.offset(count)).cast_mut()) };
        count += 1;
    }
    let slice = std::ptr::slice_from_raw_parts_mut(list, count as usize + 1);
    drop(unsafe { Box::from_raw(slice) });
}

fn meta_deserialize(
    doc: &Document,
    node: NodeId,
    file: *mut AsdfFile,
    out: *mut asdf_meta_t,
) -> AsdfValueErr {
    let library = doc
        .mapping_get(node, "asdf_library")
        .map(|id| {
            let raw = Box::into_raw(Box::new(asdf_software_t::zeroed()));
            if software_deserialize(doc, id, file, raw) == AsdfValueErr::Ok {
                raw
            } else {
                drop(unsafe { Box::from_raw(raw) });
                std::ptr::null_mut()
            }
        })
        .unwrap_or(std::ptr::null_mut());

    // `history` is a mapping of extensions and entries in the 1.1.0 form,
    // and a bare sequence of entries in the older one. Both are accepted.
    let history = doc.mapping_get(node, "history");
    let (extensions_node, entries_node) = match history {
        Some(history) if doc.resolved(history).is_mapping() => {
            (doc.mapping_get(history, "extensions"), doc.mapping_get(history, "entries"))
        }
        Some(history) => (None, Some(history)),
        None => (None, None),
    };

    unsafe {
        (*out).asdf_library = library;
        (*out).history.extensions = read_list(
            doc,
            extensions_node,
            file,
            asdf_extension_metadata_t::zeroed,
            extension_metadata_deserialize,
        );
        (*out).history.entries = read_list(
            doc,
            entries_node,
            file,
            asdf_history_entry_t::zeroed,
            history_entry_deserialize,
        );
    }
    AsdfValueErr::Ok
}

fn meta_serialize(doc: &mut Document, obj: &asdf_meta_t) -> Option<NodeId> {
    let mut pairs = Vec::new();

    if !obj.asdf_library.is_null()
        && let Some(node) = software_serialize(doc, unsafe { &*obj.asdf_library })
    {
        doc.node_mut(node).tag = Some(Tag::parse(SOFTWARE_TAG));
        let key = doc.add_scalar("asdf_library");
        pairs.push((key, node));
    }

    let mut history_pairs = Vec::new();
    if !obj.history.extensions.is_null() {
        let mut items = Vec::new();
        let mut index = 0isize;
        while !unsafe { *obj.history.extensions.offset(index) }.is_null() {
            let entry = unsafe { *obj.history.extensions.offset(index) };
            if let Some(node) = extension_metadata_serialize(doc, unsafe { &*entry }) {
                doc.node_mut(node).tag = Some(Tag::parse(EXTENSION_METADATA_TAG));
                items.push(node);
            }
            index += 1;
        }
        if !items.is_empty() {
            let list = doc.add_sequence(items);
            let key = doc.add_scalar("extensions");
            history_pairs.push((key, list));
        }
    }
    if !obj.history.entries.is_null() {
        let mut items = Vec::new();
        let mut index = 0isize;
        while !unsafe { *obj.history.entries.offset(index) }.is_null() {
            let entry = unsafe { *obj.history.entries.offset(index) };
            if let Some(node) = history_entry_serialize(doc, unsafe { &*entry }) {
                doc.node_mut(node).tag = Some(Tag::parse(HISTORY_ENTRY_TAG));
                items.push(node);
            }
            index += 1;
        }
        if !items.is_empty() {
            let list = doc.add_sequence(items);
            let key = doc.add_scalar("entries");
            history_pairs.push((key, list));
        }
    }
    if !history_pairs.is_empty() {
        let history = doc.add_mapping(history_pairs);
        let key = doc.add_scalar("history");
        pairs.push((key, history));
    }

    Some(doc.add_mapping(pairs))
}

unsafe fn meta_deinit(obj: *mut asdf_meta_t) {
    let meta = unsafe { &mut *obj };
    if !meta.asdf_library.is_null() {
        unsafe { asdf_software_destroy(meta.asdf_library) };
    }
    unsafe { free_list(meta.history.extensions, asdf_extension_metadata_destroy) };
    unsafe { free_list(meta.history.entries, asdf_history_entry_destroy) };
    *meta = asdf_meta_t::zeroed();
}

unsafe fn meta_copy(src: &asdf_meta_t, dst: *mut asdf_meta_t) -> bool {
    let out = unsafe { &mut *dst };
    out.asdf_library = if src.asdf_library.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { asdf_software_copy(std::ptr::null_mut(), src.asdf_library) }
    };
    out.history.extensions = if src.history.extensions.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { asdf_extension_metadata_array_copy(std::ptr::null_mut(), src.history.extensions) }
            .cast::<*const asdf_extension_metadata_t>()
    };
    out.history.entries = if src.history.entries.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { asdf_history_entry_array_copy(std::ptr::null_mut(), src.history.entries) }
            .cast::<*const asdf_history_entry_t>()
    };
    true
}

declare_extension! {
    name: meta,
    ty: asdf_meta_t,
    tag: META_TAG,
    deserialize: meta_deserialize,
    serialize: meta_serialize,
    deinit: meta_deinit,
    copy: meta_copy,
    is_fn: asdf_is_meta,
    value_is_fn: asdf_value_is_meta,
    value_as_fn: asdf_value_as_meta,
    value_of_fn: asdf_value_of_meta,
    get_fn: asdf_get_meta,
    set_fn: asdf_set_meta,
    copy_fn: asdf_meta_copy,
    copy_into_fn: asdf_meta_copy_into,
    array_copy_fn: asdf_meta_array_copy,
    deinit_fn: asdf_meta_deinit,
    destroy_fn: asdf_meta_destroy,
    // The tag list upstream's `ASDF_REGISTER_EXTENSION` declares.
    tags: &[
        c"tag:stsci.edu:asdf/core/asdf-1.1.0",
        c"tag:stsci.edu:asdf/core/asdf-1.0.0",
    ],
    ext_build_fn: build_meta_extension,
    ext_deserialize_fn: meta_ext_deserialize,
    ext_serialize_fn: meta_ext_serialize,
    ext_copy_fn: meta_ext_copy,
    ext_deinit_fn: meta_ext_deinit,
}

#[cfg(test)]
mod meta_tests {
    use super::*;
    use crate::file_ffi::{asdf_close, asdf_open_mem_ex};
    use crate::ndarray_ffi::asdf_datatype_t;

    struct Handle(*mut AsdfFile);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { asdf_close(self.0) };
        }
    }

    /// A tree shaped like a real file's metadata.
    fn open_full() -> Handle {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(
            b"asdf_library: !core/software-1.0.0 {name: asdf, version: 4.1.0}\n\
              history:\n  extensions:\n  - !core/extension_metadata-1.0.0\n    \
              extension_class: asdf.extension._manifest.ManifestExtension\n    \
              software: !core/software-1.0.0 {name: asdf_standard, version: 1.1.1}\n  \
              entries:\n  - !core/history_entry-1.0.0 {description: 'made it'}\n\
              dt: !core/datatype-1.0.0 float64\n\
              compound: !core/datatype-1.0.0\n  - name: x\n    datatype: float64\n  \
              - name: y\n    datatype: int32\n",
        );
        buf.extend_from_slice(b"...\n");
        let f = unsafe { asdf_open_mem_ex(buf.as_ptr().cast(), buf.len(), std::ptr::null_mut()) };
        assert!(!f.is_null());
        Handle(f)
    }

    #[test]
    fn reads_the_tree_metadata() {
        let h = open_full();
        // The root itself carries the core/asdf tag.
        assert!(unsafe { asdf_is_meta(h.0, c"".as_ptr()) });

        let mut meta: *mut asdf_meta_t = std::ptr::null_mut();
        assert_eq!(unsafe { asdf_get_meta(h.0, c"".as_ptr(), &mut meta) }, AsdfValueErr::Ok);
        let view = unsafe { &*meta };

        assert!(!view.asdf_library.is_null());
        assert_eq!(unsafe { CStr::from_ptr((*view.asdf_library).name) }.to_str().unwrap(), "asdf");

        // Extensions and entries both decoded, both null-terminated.
        assert!(!view.history.extensions.is_null());
        let first = unsafe { *view.history.extensions };
        assert_eq!(
            unsafe { CStr::from_ptr((*first).extension_class) }.to_str().unwrap(),
            "asdf.extension._manifest.ManifestExtension"
        );
        assert!(unsafe { *view.history.extensions.offset(1) }.is_null());

        assert!(!view.history.entries.is_null());
        let entry = unsafe { *view.history.entries };
        assert_eq!(unsafe { CStr::from_ptr((*entry).description) }.to_str().unwrap(), "made it");

        unsafe { asdf_meta_destroy(meta) };
    }

    /// The older schema wrote `history` as a bare sequence of entries rather
    /// than a mapping. Both forms must read.
    #[test]
    fn the_legacy_history_form_is_accepted() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(
            b"history:\n- !core/history_entry-1.0.0 {description: 'old style'}\n",
        );
        buf.extend_from_slice(b"...\n");
        let f = unsafe { asdf_open_mem_ex(buf.as_ptr().cast(), buf.len(), std::ptr::null_mut()) };
        let h = Handle(f);

        let mut meta: *mut asdf_meta_t = std::ptr::null_mut();
        assert_eq!(unsafe { asdf_get_meta(h.0, c"".as_ptr(), &mut meta) }, AsdfValueErr::Ok);
        let view = unsafe { &*meta };
        assert!(view.history.extensions.is_null(), "no extensions in the old form");
        assert!(!view.history.entries.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr((**view.history.entries).description) }.to_str().unwrap(),
            "old style"
        );
        unsafe { asdf_meta_destroy(meta) };
    }

    #[test]
    fn metadata_copies_are_independent() {
        let h = open_full();
        let mut meta: *mut asdf_meta_t = std::ptr::null_mut();
        unsafe { asdf_get_meta(h.0, c"".as_ptr(), &mut meta) };

        let copy = unsafe { asdf_meta_copy(h.0, meta) };
        assert!(!copy.is_null());
        unsafe {
            assert_ne!((*copy).asdf_library, (*meta).asdf_library);
            assert_ne!((*copy).history.entries, (*meta).history.entries);
        }

        // Freeing the original must leave the copy whole.
        unsafe { asdf_meta_destroy(meta) };
        assert_eq!(
            unsafe { CStr::from_ptr((*(*copy).asdf_library).name) }.to_str().unwrap(),
            "asdf"
        );
        unsafe { asdf_meta_destroy(copy) };
    }

    #[test]
    fn reads_a_scalar_datatype() {
        let h = open_full();
        assert!(unsafe { asdf_is_datatype(h.0, c"dt".as_ptr()) });

        let mut datatype: *mut asdf_datatype_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_datatype(h.0, c"dt".as_ptr(), &mut datatype) },
            AsdfValueErr::Ok
        );
        let view = unsafe { &*datatype };
        // float64 is discriminant 11, eight bytes wide.
        assert_eq!(view.type_, 11);
        assert_eq!(view.size, 8);
        assert_eq!(view.nfields, 0);
        unsafe { asdf_datatype_destroy(datatype) };
    }

    #[test]
    fn reads_a_compound_datatype_with_named_fields() {
        let h = open_full();
        let mut datatype: *mut asdf_datatype_t = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_get_datatype(h.0, c"compound".as_ptr(), &mut datatype) },
            AsdfValueErr::Ok
        );
        let view = unsafe { &*datatype };
        assert_eq!(view.nfields, 2);
        assert!(!view.fields.is_null());
        // A record of float64 plus int32 is twelve bytes.
        assert_eq!(view.size, 12);

        let fields = unsafe { std::slice::from_raw_parts(view.fields, 2) };
        assert_eq!(unsafe { CStr::from_ptr(fields[0].name) }.to_str().unwrap(), "x");
        assert_eq!(fields[0].size, 8);
        assert_eq!(unsafe { CStr::from_ptr(fields[1].name) }.to_str().unwrap(), "y");
        assert_eq!(fields[1].size, 4);

        // The copy must duplicate the field array, not share it.
        let copy = unsafe { asdf_datatype_copy(h.0, datatype) };
        assert!(!copy.is_null());
        unsafe { assert_ne!((*copy).fields, view.fields) };
        unsafe { asdf_datatype_destroy(datatype) };
        assert_eq!(unsafe { (*copy).nfields }, 2);
        unsafe { asdf_datatype_destroy(copy) };
    }

    #[test]
    fn deinit_is_safe_on_zeroed_objects() {
        let mut meta = asdf_meta_t::zeroed();
        unsafe { asdf_meta_deinit(&mut meta) };
        unsafe { asdf_meta_deinit(&mut meta) };

        let mut datatype = asdf_datatype_t::zeroed();
        unsafe { asdf_datatype_deinit(&mut datatype) };
        unsafe { asdf_datatype_deinit(std::ptr::null_mut()) };
    }
}

// ---- Registering the core schemas ------------------------------------

/// Put the seven core-schema extensions in the process-wide registry.
///
/// `ASDF_REGISTER_EXTENSION` does this with a `__attribute__((constructor))`
/// per extension, so upstream's are in the registry before `main`. Rust has
/// no equivalent attribute, so `shim.c` carries one constructor that calls
/// this — which also keeps the ordering guarantee, since a third-party
/// extension's own constructor may run before or after ours and the registry
/// is const-constructed either way.
///
/// Idempotent: calling it twice registers nothing new.
pub fn register_core_extensions() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::SeqCst) {
        return;
    }

    let extensions = [
        build_meta_extension(),
        build_software_extension(),
        build_extension_metadata_extension(),
        build_history_entry_extension(),
        build_datatype_extension(),
        build_time_extension(),
        crate::ndarray_ffi::build_ndarray_extension(),
    ];
    for extension in extensions {
        // SAFETY: each was just leaked, so it outlives the process's use of
        // the library, which is what registration requires.
        unsafe { crate::extension_ffi::asdf_extension_register(extension) };
    }
}

/// The entry point `shim.c`'s constructor calls.
///
/// # Safety
/// Safe to call at any time, including before `main`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_shim_register_core_extensions() {
    guard("asdf_shim_register_core_extensions", (), register_core_extensions);
}
