//! The handful of operations that genuinely need a raw pointer.
//!
//! A C ABI is a pointer ABI, so unsafe code cannot be removed from this crate
//! -- but it can be concentrated. Every entry point takes pointers the caller
//! supplies and must, exactly once, decide what they mean. Doing that inline
//! spread the same three or four judgements across hundreds of `unsafe`
//! blocks, where each one had to be read and trusted on its own.
//!
//! These helpers make each judgement once. What remains at the call sites is
//! ordinary safe Rust over `Option`, so a reviewer's attention goes to the
//! logic rather than to re-checking a null test.
//!
//! # A note on `Option<&mut T>` for out-parameters
//!
//! `Option<&T>` and `Option<&mut T>` are ABI-identical to `*const T` and
//! `*mut T` -- the null pointer optimisation guarantees it -- so they look
//! like the obvious signature for every pointer argument, and for *inputs*
//! they are: C passes a live, initialised object or null.
//!
//! Out-parameters are different, and the difference matters. C writes
//!
//! ```c
//! const char *text;                       /* uninitialised */
//! asdf_value_as_string(value, &text);
//! ```
//!
//! and a `&mut *const c_char` pointing at uninitialised memory is undefined
//! behaviour before we ever write through it: a reference must always point
//! at a valid value of its type, and uninitialised memory is not one. The
//! correct tool is [`std::ptr::write`], which requires the destination to be
//! writable and aligned but *not* initialised. That is what [`write_out`]
//! uses, so it is both safer than the reference form and honest about what a
//! C out-parameter is.

use std::ffi::{CStr, c_char};

/// Write `value` through a C out-parameter, doing nothing if it is null.
///
/// libasdf's convention throughout is that an out-parameter may be null when
/// the caller does not want the value, so the null check is part of the
/// contract rather than defensive programming.
///
/// # Safety
/// `out` must be null or writable and aligned for a `T`. It need not be
/// initialised.
pub(crate) unsafe fn write_out<T>(out: *mut T, value: T) {
    if !out.is_null() {
        // SAFETY: non-null, and the caller guarantees it is writable and
        // aligned. `write` does not read or drop the previous contents,
        // which is what makes it correct over uninitialised storage.
        unsafe { out.write(value) };
    }
}

/// Borrow a C string argument, or `None` if it is null.
///
/// # Safety
/// `ptr` must be null or point at a NUL-terminated string that stays alive
/// and unmodified for `'a`.
pub(crate) unsafe fn c_str<'a>(ptr: *const c_char) -> Option<&'a CStr> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null, and the caller guarantees termination and lifetime.
    Some(unsafe { CStr::from_ptr(ptr) })
}

/// Copy a C string argument into an owned `String`, or `None` if it is null.
///
/// Invalid UTF-8 is replaced rather than rejected: these strings reach us from
/// C callers and end up in log lines and error messages, where losing the
/// message entirely is worse than losing a byte of it.
///
/// # Safety
/// As [`c_str`].
pub(crate) unsafe fn c_string_lossy(ptr: *const c_char) -> Option<String> {
    unsafe { c_str(ptr) }.map(|s| s.to_string_lossy().into_owned())
}

/// Borrow a handle, or `None` if it is null.
///
/// # Safety
/// `ptr` must be null or point at a live `T` that is not mutated for `'a`.
pub(crate) unsafe fn as_ref<'a, T>(ptr: *const T) -> Option<&'a T> {
    // SAFETY: `as_ref` is null-checked; the caller guarantees the rest.
    unsafe { ptr.as_ref() }
}

/// Borrow a handle mutably, or `None` if it is null.
///
/// # Safety
/// `ptr` must be null or point at a live `T` that nothing else touches
/// for `'a`.
pub(crate) unsafe fn as_mut<'a, T>(ptr: *mut T) -> Option<&'a mut T> {
    // SAFETY: `as_mut` is null-checked; the caller guarantees the rest.
    unsafe { ptr.as_mut() }
}

/// A buffer allocated with `malloc`, for handing to a caller who will `free`.
///
/// libasdf's headers specify this and cannot be talked out of it:
///
/// > If ``*buf`` is NULL, a buffer is allocated with `malloc()` [...] The
/// > caller is responsible for freeing the buffer with `free()`.
///
/// So the allocator is part of the ABI. We cannot use Rust's, and a caller
/// cannot use anything but `free`. See `KNOWN-DIVERGENCES.md` for why that is
/// a defect worth raising upstream rather than one we can fix here.
///
/// What we *can* do is confine it. This type is the only place the crate
/// calls `malloc`, and it hands ownership over exactly once, at
/// [`into_raw`](CMallocBuf::into_raw).
pub(crate) struct CMallocBuf {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
}

impl CMallocBuf {
    /// Allocate with `malloc` and copy `bytes` in. `None` if allocation fails.
    ///
    /// A zero-length request still allocates a byte, so the caller receives a
    /// non-null pointer to `free`, as upstream does.
    pub(crate) fn copy_from(bytes: &[u8]) -> Option<Self> {
        // SAFETY: `malloc` with a non-zero size; the result is checked.
        let raw = unsafe { libc::malloc(bytes.len().max(1)) }.cast::<u8>();
        let ptr = std::ptr::NonNull::new(raw)?;
        // SAFETY: `malloc` returned at least `bytes.len()` writable bytes,
        // aligned for any fundamental type, and it cannot overlap `bytes`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr(), bytes.len()) };
        Some(Self { ptr, len: bytes.len() })
    }

    /// The number of bytes copied in, which is what the caller should be told.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Hand the allocation to the caller, who frees it with `free`.
    pub(crate) fn into_raw(self) -> *mut std::ffi::c_void {
        let ptr = self.ptr.as_ptr().cast::<std::ffi::c_void>();
        std::mem::forget(self);
        ptr
    }
}

impl Drop for CMallocBuf {
    /// Only runs when the buffer was *not* handed to the caller -- an error
    /// path between allocating and returning. Without this, every such path
    /// would leak.
    fn drop(&mut self) {
        // SAFETY: allocated by `malloc` here and not yet released, since
        // `into_raw` forgets `self`.
        unsafe { libc::free(self.ptr.as_ptr().cast()) };
    }
}

impl std::fmt::Debug for CMallocBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CMallocBuf").field("len", &self.len).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_out_ignores_a_null_destination() {
        unsafe { write_out(std::ptr::null_mut::<u32>(), 7) };
    }

    #[test]
    fn write_out_does_not_read_the_previous_contents() {
        // The point of `ptr::write`: the destination starts uninitialised,
        // exactly as a C caller's `const char *out;` does.
        let mut slot = std::mem::MaybeUninit::<*const c_char>::uninit();
        unsafe { write_out(slot.as_mut_ptr(), c"hi".as_ptr()) };
        let written = unsafe { slot.assume_init() };
        assert_eq!(unsafe { CStr::from_ptr(written) }, c"hi");
    }

    #[test]
    fn c_str_rejects_null_and_reads_the_rest() {
        assert!(unsafe { c_str(std::ptr::null()) }.is_none());
        assert_eq!(unsafe { c_str(c"abc".as_ptr()) }, Some(c"abc"));
    }

    #[test]
    fn a_malloc_buffer_round_trips_and_is_aligned() {
        let buf = CMallocBuf::copy_from(&[1u8, 2, 3]).expect("malloc");
        assert_eq!(buf.len(), 3);
        let raw = buf.into_raw();
        assert!(!raw.is_null());
        assert_eq!(unsafe { std::slice::from_raw_parts(raw.cast::<u8>(), 3) }, [1, 2, 3]);
        unsafe { libc::free(raw) };
    }

    #[test]
    fn an_empty_buffer_is_still_a_freeable_pointer() {
        let buf = CMallocBuf::copy_from(&[]).expect("malloc");
        assert_eq!(buf.len(), 0);
        unsafe { libc::free(buf.into_raw()) };
    }

    #[test]
    fn dropping_an_unreleased_buffer_frees_it() {
        // Miri's leak check is what makes this test mean anything.
        drop(CMallocBuf::copy_from(&[9u8; 32]).expect("malloc"));
    }
}
