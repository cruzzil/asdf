//! Public names for the four entry points whose bodies must be C.
//!
//! # Why this file exists
//!
//! Four entry points cannot be written in Rust on stable: three take C
//! varargs (`c_variadic` is unstable) and one returns `_Float16` (`f16` is
//! unstable). Their bodies live in `shim.c`.
//!
//! The obvious arrangement -- let the C definitions carry the public names --
//! does not survive linking. rustc gives a `cdylib` its own version script
//! listing the Rust `#[no_mangle]` symbols and ending `local: *`, so a symbol
//! defined in C is hidden from the finished library. Every way of adding them
//! back is worse than it looks:
//!
//! - A second `--version-script` works on some GNU ld builds and not others:
//!   "anonymous version tag cannot be combined with other version tags". It
//!   also stamps a version tag onto the symbols, which upstream's are without,
//!   so a program linked against one library and run against the other meets a
//!   version mismatch that need not exist.
//! - `--export-dynamic-symbol` and `--dynamic-list` both lose to the version
//!   script's `local: *`. Measured, not assumed.
//! - Mach-O has no version script at all, so on macOS the symbols were simply
//!   absent, and upstream's own `test-error` suite failed to link against us.
//!
//! What does work is giving rustc the symbol. A `#[naked]` function *is* a
//! `#[no_mangle]` Rust item, so it lands in rustc's export list on every
//! platform, and its body is a single jump.
//!
//! # Why a jump is ABI-transparent
//!
//! A tail `jmp`/`b` transfers control without touching a register, the stack,
//! or the return address, so the callee sees exactly the state the caller
//! established. That is what makes it safe for signatures Rust cannot even
//! spell: the varargs registers, x86-64's `al` vector-register count, and the
//! `_Float16` return in `xmm0` all pass through untouched, because nothing
//! here observes them.
//!
//! The Rust signatures below are therefore fiction -- deliberately so. They
//! take no arguments because a naked body cannot read arguments anyway, and
//! declaring the real ones would suggest a Rust caller could use these. None
//! can; the true signatures are in the vendored headers, which is what C
//! compiles against.

use std::arch::naked_asm;

/// One public symbol, defined as a jump to its `asdf_shim_*` implementation.
///
/// The implementations are hidden (`ASDF_LOCAL`), so they bind locally and
/// the jump is direct rather than through the PLT.
macro_rules! trampoline {
    ($(#[$meta:meta])* $public:ident => $target:ident) => {
        unsafe extern "C" {
            fn $target();
        }

        $(#[$meta])*
        ///
        /// # Safety
        /// Not callable from Rust. The declared signature is a placeholder --
        /// the real one is in the vendored header, and the jump passes
        /// whatever the caller set up straight through. A C caller must
        /// satisfy the header's contract; a Rust caller cannot satisfy this
        /// one, because the arguments it would need to pass cannot be spelled
        /// here.
        #[unsafe(naked)]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $public() {
            #[cfg(target_arch = "x86_64")]
            naked_asm!("jmp {}", sym $target);
            #[cfg(target_arch = "aarch64")]
            naked_asm!("b {}", sym $target);
        }
    };
}

trampoline! {
    /// `asdf_file_error_common` -- variadic; the per-code format string
    /// decides how many arguments are consumed.
    asdf_file_error_common => asdf_shim_file_error_common
}

trampoline! {
    /// `asdf_value_error_common` -- variadic, as above, for a value handle.
    asdf_value_error_common => asdf_shim_value_error_common
}

trampoline! {
    /// `asdf_file_log` -- variadic; the caller's format string and arguments
    /// go to `vsnprintf`.
    asdf_file_log => asdf_shim_file_log_v
}

#[cfg(asdf_have_float16)]
trampoline! {
    /// `asdf_ndarray_read_float16_at` -- returns `_Float16`, which on x86-64
    /// SysV comes back in `xmm0` where a `uint16_t` would come back in `rax`.
    /// Only defined when the C compiler has the type, matching the header,
    /// which leaves the declaration out otherwise.
    asdf_ndarray_read_float16_at => asdf_shim_read_float16_at
}
