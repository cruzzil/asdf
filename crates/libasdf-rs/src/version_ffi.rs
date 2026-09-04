//! `asdf/version.h`: parsing, copying and freeing `asdf_version_t`.
//!
//! `asdf_version_t` is a public, non-opaque struct, so its layout is part of
//! the ABI: callers read `.major` and friends directly.

use std::ffi::{CStr, CString, c_char, c_uint};

use asdf_core::Version;

use crate::panic::guard;

/// Mirror of `asdf_version_t`.
///
/// Field order and types must match `include/asdf/version.h` exactly.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_version_t {
    /// The full, unparsed version string. Owned; freed by
    /// `asdf_version_destroy`.
    pub version: *const c_char,
    /// Major version, or 0.
    pub major: c_uint,
    /// Minor version, or 0.
    pub minor: c_uint,
    /// Patch version, or 0.
    pub patch: c_uint,
    /// Trailing version information, or null.
    pub extra: *const c_char,
}

/// Allocate a C string, or null if it contains an interior NUL.
fn into_c_string(s: &str) -> *const c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw().cast_const(),
        Err(_) => std::ptr::null(),
    }
}

/// Free a string previously produced by [`into_c_string`].
///
/// # Safety
/// `p` must be null or have come from `CString::into_raw`.
unsafe fn free_c_string(p: *const c_char) {
    if !p.is_null() {
        drop(unsafe { CString::from_raw(p.cast_mut()) });
    }
}

/// Build the C struct for a parsed version.
fn to_ffi(v: &Version) -> *mut asdf_version_t {
    let version = into_c_string(&v.version);
    if version.is_null() && !v.version.is_empty() {
        return std::ptr::null_mut();
    }
    let extra = match &v.extra {
        Some(e) => into_c_string(e),
        None => std::ptr::null(),
    };

    Box::into_raw(Box::new(asdf_version_t {
        version,
        major: v.major,
        minor: v.minor,
        patch: v.patch,
        extra,
    }))
}

/// Parse a version string into a heap-allocated `asdf_version_t`.
///
/// A string that is not `MAJOR.MINOR.PATCH` still yields a struct, with the
/// original copied verbatim into `version` and the numeric fields zeroed.
///
/// # Safety
/// `version` must be null or a valid NUL-terminated string. The result must
/// be freed with [`asdf_version_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_version_parse(version: *const c_char) -> *mut asdf_version_t {
    guard("asdf_version_parse", std::ptr::null_mut(), || {
        if version.is_null() {
            return std::ptr::null_mut();
        }
        let text = unsafe { CStr::from_ptr(version) }.to_string_lossy().into_owned();
        to_ffi(&Version::parse(&text))
    })
}

/// Deep-copy an `asdf_version_t`.
///
/// # Safety
/// `version` must be null or point to a valid `asdf_version_t`. The result
/// must be freed with [`asdf_version_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_version_copy(version: *const asdf_version_t) -> *mut asdf_version_t {
    guard("asdf_version_copy", std::ptr::null_mut(), || {
        if version.is_null() {
            return std::ptr::null_mut();
        }
        let src = unsafe { &*version };

        let version_str = if src.version.is_null() {
            std::ptr::null()
        } else {
            let s = unsafe { CStr::from_ptr(src.version) };
            match CString::new(s.to_bytes()) {
                Ok(c) => c.into_raw().cast_const(),
                Err(_) => return std::ptr::null_mut(),
            }
        };
        let extra_str = if src.extra.is_null() {
            std::ptr::null()
        } else {
            let s = unsafe { CStr::from_ptr(src.extra) };
            match CString::new(s.to_bytes()) {
                Ok(c) => c.into_raw().cast_const(),
                Err(_) => {
                    unsafe { free_c_string(version_str) };
                    return std::ptr::null_mut();
                }
            }
        };

        Box::into_raw(Box::new(asdf_version_t {
            version: version_str,
            major: src.major,
            minor: src.minor,
            patch: src.patch,
            extra: extra_str,
        }))
    })
}

/// Free an `asdf_version_t` and zero its storage.
///
/// # Safety
/// `version` must be null or have come from [`asdf_version_parse`] or
/// [`asdf_version_copy`], and must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_version_destroy(version: *mut asdf_version_t) {
    guard("asdf_version_destroy", (), || {
        if version.is_null() {
            return;
        }
        let boxed = unsafe { Box::from_raw(version) };
        unsafe { free_c_string(boxed.version) };
        unsafe { free_c_string(boxed.extra) };
        // Upstream zeroes the struct before freeing; the Box drop releases
        // the storage, and the strings above are already released.
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> *mut asdf_version_t {
        let c = CString::new(s).unwrap();
        unsafe { asdf_version_parse(c.as_ptr()) }
    }

    fn version_str(v: *const asdf_version_t) -> Option<String> {
        let v = unsafe { &*v };
        (!v.version.is_null()).then(|| {
            unsafe { crate::ffi::c_str(v.version) }
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
        })
    }

    fn extra_str(v: *const asdf_version_t) -> Option<String> {
        let v = unsafe { &*v };
        (!v.extra.is_null()).then(|| {
            unsafe { crate::ffi::c_str(v.extra) }
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
        })
    }

    #[test]
    fn parses_and_frees() {
        let v = parse("1.6.0");
        assert!(!v.is_null());
        unsafe {
            assert_eq!((*v).major, 1);
            assert_eq!((*v).minor, 6);
            assert_eq!((*v).patch, 0);
            assert!((*v).extra.is_null());
        }
        assert_eq!(version_str(v).as_deref(), Some("1.6.0"));
        unsafe { asdf_version_destroy(v) };
    }

    #[test]
    fn carries_the_extra_field() {
        let v = parse("0.1.0.dev4");
        assert_eq!(extra_str(v).as_deref(), Some("dev4"));
        assert_eq!(version_str(v).as_deref(), Some("0.1.0.dev4"));
        unsafe { asdf_version_destroy(v) };
    }

    #[test]
    fn non_semver_is_preserved() {
        let v = parse("not-a-version");
        unsafe {
            assert_eq!((*v).major, 0);
            assert_eq!((*v).minor, 0);
            assert_eq!((*v).patch, 0);
        }
        assert_eq!(version_str(v).as_deref(), Some("not-a-version"));
        unsafe { asdf_version_destroy(v) };
    }

    #[test]
    fn copy_is_deep() {
        let a = parse("1.2.3-rc1");
        let b = unsafe { asdf_version_copy(a) };
        assert!(!b.is_null());

        // Distinct allocations, equal contents.
        unsafe {
            assert_ne!((*a).version, (*b).version);
            assert_ne!((*a).extra, (*b).extra);
        }
        assert_eq!(version_str(a), version_str(b));
        assert_eq!(extra_str(a), extra_str(b));

        // Freeing one must leave the other intact.
        unsafe { asdf_version_destroy(a) };
        assert_eq!(version_str(b).as_deref(), Some("1.2.3-rc1"));
        unsafe { asdf_version_destroy(b) };
    }

    #[test]
    fn null_arguments_are_handled() {
        assert!(unsafe { asdf_version_parse(std::ptr::null()) }.is_null());
        assert!(unsafe { asdf_version_copy(std::ptr::null()) }.is_null());
        // Freeing null must be a no-op, as it is upstream.
        unsafe { asdf_version_destroy(std::ptr::null_mut()) };
    }

    #[test]
    fn struct_layout_matches_the_header() {
        use std::mem::{align_of, offset_of, size_of};
        // Two pointers, three unsigned ints, padded to pointer alignment.
        assert_eq!(offset_of!(asdf_version_t, version), 0);
        assert_eq!(offset_of!(asdf_version_t, major), size_of::<*const c_char>());
        assert_eq!(
            offset_of!(asdf_version_t, minor),
            size_of::<*const c_char>() + size_of::<c_uint>()
        );
        assert_eq!(
            offset_of!(asdf_version_t, patch),
            size_of::<*const c_char>() + 2 * size_of::<c_uint>()
        );
        assert_eq!(align_of::<asdf_version_t>(), align_of::<*const c_char>());
    }
}
