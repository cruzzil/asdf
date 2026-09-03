//! `asdf/core/ndarray.h`.
//!
//! Only the `float16` accessor exists so far, because `shim.c` references it
//! and an undefined symbol in the shared library breaks every C link against
//! it. The rest of the ndarray surface lands in phase 4.

use std::ffi::{c_int, c_void};

use crate::panic::guard;

/// Error codes matching `asdf_ndarray_err_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum NdarrayErr {
    /// Read successfully.
    Ok = 0,
    /// Read beyond the bounds of the array.
    OutOfBounds,
    /// Allocation failure.
    Oom,
    /// An argument was invalid.
    Inval,
    /// A value did not fit the requested type.
    Overflow,
    /// An element could not be converted to the requested type.
    Conversion,
}

/// Read one `float16` element, returning its raw bit pattern.
///
/// Called only by `shim.c`, which reinterprets the bits as `_Float16` and
/// returns that to the caller. The conversion has to happen on the C side:
/// `_Float16` and `uint16_t` do not share a return ABI (on x86-64 SysV one
/// returns in `xmm0`, the other in `rax`), so returning the bits from Rust
/// and reinterpreting in C is the only way to get the value into the right
/// register without unstable Rust.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t *`, `indices` must be
/// null or point to at least `ndim` values, and `err` must be null or point
/// to a writable `int`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_shim_ndarray_read_float16_bits_at(
    ndarray: *mut c_void,
    indices: *const u64,
    err: *mut c_int,
) -> u16 {
    guard("asdf_shim_ndarray_read_float16_bits_at", 0u16, || {
        // TODO(phase 4): read through the engine once ndarray lands.
        let _ = (ndarray, indices);
        if !err.is_null() {
            unsafe { *err = NdarrayErr::Inval as c_int };
        }
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn err_discriminants_match_the_c_abi() {
        assert_eq!(NdarrayErr::Ok as i32, 0);
        assert_eq!(NdarrayErr::OutOfBounds as i32, 1);
        assert_eq!(NdarrayErr::Oom as i32, 2);
        assert_eq!(NdarrayErr::Inval as i32, 3);
        assert_eq!(NdarrayErr::Overflow as i32, 4);
        assert_eq!(NdarrayErr::Conversion as i32, 5);
    }

    #[test]
    fn float16_stub_reports_an_error_rather_than_a_wrong_value() {
        let mut err: c_int = -1;
        let bits = unsafe {
            asdf_shim_ndarray_read_float16_bits_at(std::ptr::null_mut(), std::ptr::null(), &mut err)
        };
        assert_eq!(bits, 0);
        assert_eq!(err, NdarrayErr::Inval as c_int);
    }

    #[test]
    fn a_null_error_pointer_is_accepted() {
        let _ = unsafe {
            asdf_shim_ndarray_read_float16_bits_at(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
    }
}
