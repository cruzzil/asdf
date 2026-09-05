//! Error and log plumbing shared with `shim.c`.
//!
//! The `asdf_shim_*` functions below are the Rust side of that split. They
//! are exported because `shim.c` resolves them by name, and they sit inside
//! the `asdf_` namespace so the symbol-leakage gate accepts them. They are
//! internal plumbing, not public API -- upstream does the same for the
//! `asdf_file_error_common` family, which its headers call out as "exported
//! for the macros, but intentionally left undocumented".
//!
//! The C API reports errors as a code plus a message string owned by the
//! handle they were recorded against, so the handle keeps the `CString`
//! alive until the next error replaces it.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;

use asdf_core::ErrorCode;

/// The per-code `printf` format strings, matching `src/error.c` upstream.
///
/// The parameter counts are part of the documented contract for
/// `ASDF_ERROR_COMMON`, so these strings must keep their conversions exactly.
const ERROR_FORMATS: &[Option<&CStr>] = &[
    None,                                                   // NONE
    Some(c"unknown parser state"),                          // UNKNOWN_STATE
    Some(c"failed to initialize stream"),                   // STREAM_INIT_FAILED
    Some(c"cannot write to a read-only stream or file"),    // STREAM_READ_ONLY
    Some(c"invalid ASDF header"),                           // INVALID_ASDF_HEADER
    Some(c"unexpected end of file"),                        // UNEXPECTED_EOF
    Some(c"invalid block header"),                          // INVALID_BLOCK_HEADER
    Some(c"block magic mismatch"),                          // BLOCK_MAGIC_MISMATCH
    Some(c"YAML parser initialization failed"),             // YAML_PARSER_INIT_FAILED
    Some(c"YAML parsing failed"),                           // YAML_PARSE_FAILED
    Some(c"out of memory"),                                 // OUT_OF_MEMORY
    None,                                                   // SYSTEM (from strerror)
    Some(c"invalid argument for %s: %s"),                   // INVALID_ARGUMENT
    Some(c"unknown compression type: %s"),                  // UNKNOWN_COMPRESSION
    Some(c"compression error: %s"),                         // COMPRESSION_FAILED
    Some(c"no serializer registered for the %s extension"), // EXTENSION_NOT_FOUND
    Some(c"over limit: %s"),                                // OVER_LIMIT
];

/// The severity each error code is logged at, matching upstream.
const ERROR_LOG_LEVELS: &[LogLevel] = &[
    LogLevel::None,  // NONE
    LogLevel::Error, // UNKNOWN_STATE
    LogLevel::Error, // STREAM_INIT_FAILED
    LogLevel::Error, // STREAM_READ_ONLY
    LogLevel::Error, // INVALID_ASDF_HEADER
    LogLevel::Error, // UNEXPECTED_EOF
    LogLevel::Error, // INVALID_BLOCK_HEADER
    LogLevel::Error, // BLOCK_MAGIC_MISMATCH
    LogLevel::Fatal, // YAML_PARSER_INIT_FAILED
    LogLevel::Error, // YAML_PARSE_FAILED
    LogLevel::Fatal, // OUT_OF_MEMORY
    LogLevel::Error, // SYSTEM
    LogLevel::Error, // INVALID_ARGUMENT
    LogLevel::Error, // UNKNOWN_COMPRESSION
    LogLevel::Error, // COMPRESSION_FAILED
    LogLevel::Warn,  // EXTENSION_NOT_FOUND
    LogLevel::Error, // OVER_LIMIT
];

/// Severity levels, matching `asdf_log_level_t`.
///
/// Zero is `None`, which is what a caller's zeroed `asdf_log_cfg_t` carries
/// and what upstream reads as "unset, use the default".
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
#[repr(i32)]
pub enum LogLevel {
    /// Emit nothing, and what an unset configuration field holds.
    #[default]
    None = 0,
    /// Fine-grained tracing.
    Trace,
    /// Debugging messages.
    Debug,
    /// Informational messages.
    Info,
    /// Recoverable problems.
    Warn,
    /// Errors.
    Error,
    /// Unrecoverable errors.
    Fatal,
}

impl LogLevel {
    /// Parse the `ASDF_LOG_LEVEL` environment variable's spelling.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "NONE" => Some(LogLevel::None),
            "TRACE" => Some(LogLevel::Trace),
            "DEBUG" => Some(LogLevel::Debug),
            "INFO" => Some(LogLevel::Info),
            "WARN" => Some(LogLevel::Warn),
            "ERROR" => Some(LogLevel::Error),
            "FATAL" => Some(LogLevel::Fatal),
            _ => None,
        }
    }

    /// The name used in log output.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::None => "NONE",
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Fatal => "FATAL",
        }
    }

    fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(LogLevel::None),
            1 => Some(LogLevel::Trace),
            2 => Some(LogLevel::Debug),
            3 => Some(LogLevel::Info),
            4 => Some(LogLevel::Warn),
            5 => Some(LogLevel::Error),
            6 => Some(LogLevel::Fatal),
            _ => None,
        }
    }
}

/// The error state a file or value handle carries.
///
/// `asdf_error` hands out a borrowed pointer into `message`, so the string
/// must outlive the call and stay put until the next error is recorded.
#[derive(Default, Debug)]
pub struct ErrorState {
    inner: Mutex<ErrorStateInner>,
}

#[derive(Default, Debug)]
struct ErrorStateInner {
    code: i32,
    errno: i32,
    message: Option<CString>,
}

impl ErrorState {
    /// Record an error.
    pub fn set(&self, code: i32, message: impl Into<Vec<u8>>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.code = code;
        inner.errno = 0;
        inner.message = CString::new(message).ok();
    }

    /// Record an OS-level error, deriving the message from `strerror`.
    pub fn set_system(&self, errnum: i32) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.code = ErrorCode::System as i32;
        inner.errno = errnum;
        inner.message = CString::new(strerror(errnum)).ok();
    }

    /// Record an engine error.
    pub fn set_error(&self, err: &asdf_core::Error) {
        match err.errno() {
            Some(n) => {
                self.set_system(n);
                // Keep the engine's richer message rather than bare strerror.
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.message = CString::new(err.message()).ok();
            }
            None => self.set(err.code() as i32, err.message()),
        }
    }

    /// Clear any recorded error.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *inner = ErrorStateInner::default();
    }

    /// The recorded code.
    pub fn code(&self) -> i32 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).code
    }

    /// The recorded `errno`, meaningful only for [`ErrorCode::System`].
    pub fn errno(&self) -> i32 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).errno
    }

    /// A pointer to the recorded message, valid until the next error is set.
    ///
    /// # Safety
    /// The returned pointer borrows from `self` and is invalidated by any
    /// later `set`/`clear` on the same state.
    pub fn message_ptr(&self) -> *const c_char {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &inner.message {
            Some(s) => s.as_ptr(),
            None => std::ptr::null(),
        }
    }
}

/// The message C's `strerror` gives for an `errno`.
///
/// Rust's own `io::Error` display appends " (os error N)", which is helpful
/// in a Rust message and wrong here: `asdf_error` hands this to a C caller
/// that expects exactly what `strerror` would have said, and upstream's own
/// tests compare it against that string.
///
/// `strerror_r` rather than `strerror`, since the message must not be
/// clobbered by another thread between formatting and use.
#[cfg(unix)]
fn strerror(errnum: i32) -> String {
    let mut buffer = [0 as c_char; 256];
    // SAFETY: the buffer is ours and its length is passed correctly.
    let rc = unsafe { libc::strerror_r(errnum, buffer.as_mut_ptr(), buffer.len()) };
    if rc != 0 {
        // The XSI form failed; fall back to something truthful.
        return format!("errno {errnum}");
    }
    unsafe { crate::ffi::c_string_lossy(buffer.as_ptr()) }.unwrap_or_default()
}

/// The Windows CRT has no `strerror_r`; its thread-safe spelling is
/// `strerror_s`, which the `libc` crate does not bind either. `strerror`
/// itself is what remains, and on the Windows CRT it returns a pointer into
/// per-thread storage rather than a shared static, so the race the POSIX
/// branch avoids does not arise here.
#[cfg(not(unix))]
fn strerror(errnum: i32) -> String {
    // SAFETY: `strerror` never returns null, and on this CRT the storage is
    // per-thread, so it is stable until this thread calls `strerror` again.
    unsafe { crate::ffi::c_string_lossy(libc::strerror(errnum)) }
        .unwrap_or_else(|| format!("errno {errnum}"))
}

/// The format string for an error code, or null.
///
/// Called by `shim.c` to drive `vsnprintf` over the caller's varargs.
///
/// # Safety
/// The returned pointer refers to a `'static` string and is always valid.
#[unsafe(no_mangle)]
pub extern "C" fn asdf_shim_error_format(code: c_int) -> *const c_char {
    let idx = match usize::try_from(code) {
        Ok(i) => i,
        Err(_) => return std::ptr::null(),
    };
    match ERROR_FORMATS.get(idx) {
        Some(Some(s)) => s.as_ptr(),
        _ => std::ptr::null(),
    }
}

/// The severity a given error code is logged at.
pub fn error_log_level(code: i32) -> LogLevel {
    usize::try_from(code)
        .ok()
        .and_then(|i| ERROR_LOG_LEVELS.get(i).copied())
        .unwrap_or(LogLevel::Error)
}

/// Record an already-formatted error against a handle.
///
/// # Safety
/// `obj` must be null, or a valid `asdf_file_t *` when `is_value` is 0, or a
/// valid `asdf_value_t *` when it is 1. `msg` must be a valid NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_shim_error_set(
    obj: *mut c_void,
    is_value: c_int,
    code: c_int,
    src_file: *const c_char,
    lineno: c_int,
    msg: *const c_char,
) {
    crate::panic::guard("asdf_shim_error_set", (), || {
        let text = unsafe { crate::ffi::c_string_lossy(msg) }.unwrap_or_default();
        let text = if text.is_empty() {
            usize::try_from(code)
                .ok()
                .and_then(|i| ERROR_FORMATS.get(i).copied().flatten())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown error".to_string())
        } else {
            text
        };

        if let Some(state) = unsafe { state_for(obj, is_value) } {
            state.set(code, text.clone());
        }
        unsafe { emit_log(obj, is_value, error_log_level(code), src_file, lineno, &text) };
    });
}

/// Record an OS-level error against a handle.
///
/// # Safety
/// Same requirements as [`asdf_shim_error_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_shim_error_set_system(
    obj: *mut c_void,
    is_value: c_int,
    errnum: c_int,
    src_file: *const c_char,
    lineno: c_int,
) {
    crate::panic::guard("asdf_shim_error_set_system", (), || {
        let text = strerror(errnum);
        if let Some(state) = unsafe { state_for(obj, is_value) } {
            state.set_system(errnum);
        }
        unsafe { emit_log(obj, is_value, LogLevel::Error, src_file, lineno, &text) };
    });
}

/// Emit an already-formatted log message.
///
/// # Safety
/// `file` must be null or a valid `asdf_file_t *`; the string arguments must
/// be valid NUL-terminated strings or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_shim_log_message(
    file: *const c_void,
    level: c_int,
    src_file: *const c_char,
    lineno: c_int,
    msg: *const c_char,
) {
    crate::panic::guard("asdf_shim_log_message", (), || {
        let Some(level) = LogLevel::from_i32(level) else { return };
        let text = unsafe { crate::ffi::c_string_lossy(msg) }.unwrap_or_default();
        unsafe { emit_log(file.cast_mut(), 0, level, src_file, lineno, &text) };
    });
}

/// Resolve a handle to its error state.
///
/// # Safety
/// See [`asdf_shim_error_set`].
unsafe fn state_for(obj: *mut c_void, is_value: c_int) -> Option<&'static ErrorState> {
    if obj.is_null() {
        return None;
    }
    // A value's errors belong to its file: `ASDF_ERROR_COMMON(value, ..)`
    // followed by `asdf_error_code(file)` is how upstream's own tests read
    // them back.
    let file = if is_value != 0 {
        crate::file_ffi::value_file(obj.cast::<crate::file_ffi::AsdfValue>())?
    } else {
        obj.cast::<crate::file_ffi::AsdfFile>()
    };
    crate::file_ffi::error_state(file)
}

/// Write a log line if it meets the active threshold.
///
/// # Safety
/// See [`asdf_shim_log_message`].
unsafe fn emit_log(
    _obj: *mut c_void,
    _is_value: c_int,
    level: LogLevel,
    src_file: *const c_char,
    lineno: c_int,
    msg: &str,
) {
    if level == LogLevel::None || level < default_log_level() {
        return;
    }
    // A null `src_file` is what the shim passes when it has no source
    // location to report, and upstream prints `?` for it.
    let src = unsafe { crate::ffi::c_string_lossy(src_file) }.unwrap_or_else(|| "?".into());
    eprintln!("{} libasdf {}:{}: {}", level.as_str(), src, lineno, msg);
}

/// Emit a log line against a file, honouring its own log configuration.
///
/// A file opened with an `asdf_config_t` may name its own stream and level,
/// which is how a caller captures warnings; without one this falls back to
/// the process-wide default.
pub(crate) fn log_to_file(file: *mut crate::file_ffi::AsdfFile, level: LogLevel, msg: &str) {
    let config = crate::file_ffi::file_config(file).unwrap_or_default();
    let threshold =
        if config.log_level == LogLevel::None { default_log_level() } else { config.log_level };
    if level == LogLevel::None || level < threshold {
        return;
    }

    let line = format!("{} libasdf: {msg}\n", level.as_str());
    if config.log_stream.is_null() {
        eprint!("{line}");
        return;
    }
    // SAFETY: the caller's `asdf_config_t` named this stream and, by the C
    // contract, keeps it open for the file's lifetime.
    unsafe {
        libc::fwrite(
            line.as_ptr().cast::<c_void>(),
            1,
            line.len(),
            config.log_stream.cast::<libc::FILE>(),
        );
        libc::fflush(config.log_stream.cast::<libc::FILE>());
    }
}

/// The threshold used when a file carries no explicit configuration.
///
/// Taken from `ASDF_LOG_LEVEL`, defaulting to `WARN`, as upstream documents.
pub fn default_log_level() -> LogLevel {
    static CACHED: std::sync::OnceLock<LogLevel> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("ASDF_LOG_LEVEL")
            .ok()
            .and_then(|v| LogLevel::from_name(&v))
            .unwrap_or(LogLevel::Warn)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_code_has_a_table_entry() {
        // The tables are indexed by code, so a gap would be a silent
        // out-of-bounds read on the C side.
        assert_eq!(ERROR_FORMATS.len(), 17);
        assert_eq!(ERROR_LOG_LEVELS.len(), 17);
        assert_eq!(ERROR_FORMATS.len(), ERROR_LOG_LEVELS.len());
    }

    #[test]
    fn format_strings_match_upstream_conversions() {
        // The number of %s conversions is the documented contract for
        // ASDF_ERROR_COMMON's variadic arguments.
        let invalid = ERROR_FORMATS[ErrorCode::InvalidArgument as usize].unwrap();
        assert_eq!(invalid.to_str().unwrap().matches("%s").count(), 2);

        for code in [
            ErrorCode::UnknownCompression,
            ErrorCode::CompressionFailed,
            ErrorCode::ExtensionNotFound,
            ErrorCode::OverLimit,
        ] {
            let f = ERROR_FORMATS[code as usize].unwrap();
            assert_eq!(f.to_str().unwrap().matches("%s").count(), 1, "{code:?}");
        }
    }

    #[test]
    fn system_and_none_have_no_format() {
        assert!(ERROR_FORMATS[ErrorCode::None as usize].is_none());
        assert!(ERROR_FORMATS[ErrorCode::System as usize].is_none());
    }

    #[test]
    fn error_format_lookup_is_bounds_safe() {
        assert!(!asdf_shim_error_format(ErrorCode::UnexpectedEof as i32).is_null());
        assert!(asdf_shim_error_format(ErrorCode::None as i32).is_null());
        assert!(asdf_shim_error_format(9999).is_null());
        assert!(asdf_shim_error_format(-1).is_null());
    }

    #[test]
    fn log_levels_match_upstream() {
        assert_eq!(error_log_level(ErrorCode::OutOfMemory as i32), LogLevel::Fatal);
        assert_eq!(error_log_level(ErrorCode::YamlParserInitFailed as i32), LogLevel::Fatal);
        assert_eq!(error_log_level(ErrorCode::ExtensionNotFound as i32), LogLevel::Warn);
        assert_eq!(error_log_level(ErrorCode::UnexpectedEof as i32), LogLevel::Error);
    }

    #[test]
    fn error_state_round_trips() {
        let s = ErrorState::default();
        assert_eq!(s.code(), 0);
        assert!(s.message_ptr().is_null());

        s.set(ErrorCode::UnexpectedEof as i32, "truncated");
        assert_eq!(s.code(), ErrorCode::UnexpectedEof as i32);
        let msg = unsafe { CStr::from_ptr(s.message_ptr()) };
        assert_eq!(msg.to_str().unwrap(), "truncated");

        s.clear();
        assert_eq!(s.code(), 0);
    }

    #[test]
    fn system_errors_carry_errno() {
        let s = ErrorState::default();
        s.set_system(2);
        assert_eq!(s.code(), ErrorCode::System as i32);
        assert_eq!(s.errno(), 2);
        assert!(!s.message_ptr().is_null());
    }

    #[test]
    fn messages_with_interior_nul_do_not_panic() {
        let s = ErrorState::default();
        s.set(1, "bad\0message");
        // CString rejects it; the state simply reports no message.
        assert!(s.message_ptr().is_null());
    }

    #[test]
    fn log_level_names_round_trip() {
        for level in [
            LogLevel::None,
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
            LogLevel::Fatal,
        ] {
            assert_eq!(LogLevel::from_name(level.as_str()), Some(level));
            assert_eq!(LogLevel::from_i32(level as i32), Some(level));
        }
        // The environment variable is documented as case-insensitive.
        assert_eq!(LogLevel::from_name("warn"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_name("nonsense"), None);
    }
}
