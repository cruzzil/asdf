//! `asdf/util.h`: the one entry point that header declares.
//!
//! # Why a free function is part of the ABI
//!
//! Four entry points hand the caller a buffer the library allocated:
//! `asdf_write_to_mem`, `asdf_ndarray_read_all`, `asdf_ndarray_read_tile_ndim`
//! and `asdf_ndarray_read_tile_2d`, each when asked to allocate the
//! destination itself. Before libasdf 0.2.0 the headers told the caller to
//! release those with `free()`, which made the C runtime's allocator part of
//! the interface -- an implementation could not use any other one, and on
//! Windows a caller linked against a different CRT would free into the wrong
//! heap. Raised as [libasdf#250]; `asdf_free` is the answer.
//!
//! It does not change what this crate allocates with. `CMallocBuf` still uses
//! `malloc`, because callers written against the older headers are still
//! calling `free()` on these buffers and must keep working.
//!
//! [libasdf#250]: https://github.com/asdf-format/libasdf/issues/250

use core::ffi::c_void;

use crate::panic::guard;

/// Release a buffer libasdf allocated on the caller's behalf.
///
/// Only for the four allocating entry points listed in `asdf/util.h`. Every
/// other pointer the library hands out has its own destructor and must not
/// come here.
///
/// # Safety
/// `buf` must be null, or a pointer returned by one of those entry points and
/// not yet released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_free(buf: *mut c_void) {
    guard("asdf_free", (), || {
        if buf.is_null() {
            return;
        }
        // SAFETY: the caller's contract says this came from `CMallocBuf`,
        // which allocates with `malloc` and never freed it.
        unsafe { libc::free(buf) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::CMallocBuf;

    #[test]
    fn a_null_buffer_is_a_no_op() {
        unsafe { asdf_free(core::ptr::null_mut()) };
    }

    #[test]
    fn a_library_buffer_round_trips_through_asdf_free() {
        let raw = CMallocBuf::copy_from(&[1u8, 2, 3]).expect("malloc").into_raw();
        unsafe { asdf_free(raw) };
    }
}
