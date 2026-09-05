//! Keeping Rust panics from crossing the C boundary.
//!
//! Unwinding out of an `extern "C"` function into C is undefined behaviour.
//! Every entry point in this crate therefore runs its body inside
//! [`guard`], which catches any panic, reports it once, and returns the
//! caller-supplied fallback value instead.
//!
//! This is a backstop, not a design: a panic reaching here is a bug in the
//! engine. It is reported to stderr on first occurrence so the bug is visible
//! rather than silently swallowed.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};

static REPORTED: AtomicBool = AtomicBool::new(false);

/// Run `body`, returning `fallback` if it panics.
///
/// The closure is treated as unwind-safe: state it touches lives behind
/// handles the C caller owns, and a panic leaves that state untouched
/// because the engine itself does not panic on error paths -- it returns
/// `Result`.
pub fn guard<T>(what: &'static str, fallback: T, body: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            report(what, &payload);
            fallback
        }
    }
}

fn report(what: &'static str, payload: &Box<dyn std::any::Any + Send>) {
    // Only the first panic is reported, so a caller looping over a broken
    // file does not flood stderr.
    if REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let msg = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string());

    eprintln!(
        "libasdf-rs: internal error: a panic escaped {what}: {msg}\n\
         libasdf-rs: this is a bug; the call returned a failure value instead.\n\
         libasdf-rs: please report it at https://github.com/cruzzil/asdf/issues"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_value_when_nothing_panics() {
        assert_eq!(guard("test", -1, || 42), 42);
    }

    #[test]
    fn returns_the_fallback_on_panic() {
        // Silence the default hook so the test output stays readable.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let got = guard("test", -1, || panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(got, -1);
    }

    #[test]
    fn null_pointers_are_a_valid_fallback() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let got: *mut u8 = guard("test", std::ptr::null_mut(), || panic!("boom"));
        std::panic::set_hook(prev);
        assert!(got.is_null());
    }
}
