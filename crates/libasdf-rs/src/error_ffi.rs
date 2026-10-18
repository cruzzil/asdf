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

use alloc::ffi::CString;
use core::ffi::{CStr, c_char, c_int, c_void};
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
/// Zero is `Default`, which is what a caller's zeroed `asdf_log_cfg_t`
/// carries and which selects the `ASDF_LOG_LEVEL` environment variable.
/// `None` sits above every severity, so as a threshold it emits nothing.
/// Neither is a severity a message can be logged at.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
#[repr(i32)]
pub enum LogLevel {
    /// Use the default threshold. What an unset configuration field holds.
    #[default]
    Default = 0,
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
    /// Emit nothing; a threshold only.
    None,
}

impl LogLevel {
    /// Parse the `ASDF_LOG_LEVEL` environment variable's spelling.
    ///
    /// `DEFAULT` is deliberately not a spelling: upstream only matches the
    /// names of real severities, plus `NONE`.
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
            LogLevel::Default => "DEFAULT",
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Fatal => "FATAL",
            LogLevel::None => "NONE",
        }
    }

    /// Whether a message can be logged at this level.
    pub fn is_severity(self) -> bool {
        (LogLevel::Trace..=LogLevel::Fatal).contains(&self)
    }

    /// The colour upstream gives this level's name.
    fn color(self) -> &'static str {
        match self {
            LogLevel::Trace => "\x1b[94m",
            LogLevel::Debug => "\x1b[36m",
            LogLevel::Info => "\x1b[32m",
            LogLevel::Warn => "\x1b[33m",
            LogLevel::Error => "\x1b[31m",
            LogLevel::Fatal => "\x1b[35m",
            LogLevel::Default | LogLevel::None => "",
        }
    }

    fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(LogLevel::Default),
            1 => Some(LogLevel::Trace),
            2 => Some(LogLevel::Debug),
            3 => Some(LogLevel::Info),
            4 => Some(LogLevel::Warn),
            5 => Some(LogLevel::Error),
            6 => Some(LogLevel::Fatal),
            7 => Some(LogLevel::None),
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
            None => core::ptr::null(),
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
        Err(_) => return core::ptr::null(),
    };
    match ERROR_FORMATS.get(idx) {
        Some(Some(s)) => s.as_ptr(),
        _ => core::ptr::null(),
    }
}

/// The severity a given error code is logged at.
pub fn error_log_level(code: i32) -> LogLevel {
    usize::try_from(code)
        .ok()
        .and_then(|i| ERROR_LOG_LEVELS.get(i).copied())
        .unwrap_or(LogLevel::Error)
}

// ---- The non-variadic error entry points -----------------------------
//
// These four were in `shim.c` because they sit beside the variadic ones, not
// because they had to be. Being C cost them their export: rustc gives a
// cdylib its own symbol list, naming only Rust `#[no_mangle]` symbols, and a
// C symbol not in that list is localised. On Linux the version script this
// crate's `build.rs` emits puts them back; Mach-O has no version script, so
// on macOS they simply were not in the dylib, and a C caller using
// `ASDF_ERROR_OOM` failed to link. Defining them in Rust makes rustc export
// them on every platform, which is the fix rather than a workaround.

/// Record an out-of-memory error against a file handle.
///
/// # Safety
/// `file` must be null or a valid file handle; `src_file` a C string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_file_error_oom(
    file: *mut crate::file_ffi::AsdfFile,
    src_file: *const c_char,
    lineno: c_int,
) {
    unsafe { asdf_shim_error_set(file.cast(), 0, OOM_CODE, src_file, lineno, OOM_MESSAGE.as_ptr()) }
}

/// Record an out-of-memory error against a value handle.
///
/// # Safety
/// As [`asdf_file_error_oom`], for a value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_error_oom(
    value: *mut crate::file_ffi::AsdfValue,
    src_file: *const c_char,
    lineno: c_int,
) {
    unsafe {
        asdf_shim_error_set(value.cast(), 1, OOM_CODE, src_file, lineno, OOM_MESSAGE.as_ptr())
    }
}

/// Record an OS-level error against a file handle.
///
/// # Safety
/// As [`asdf_file_error_oom`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_file_error_system(
    file: *mut crate::file_ffi::AsdfFile,
    errnum: c_int,
    src_file: *const c_char,
    lineno: c_int,
) {
    unsafe { asdf_shim_error_set_system(file.cast(), 0, errnum, src_file, lineno) }
}

/// Record an OS-level error against a value handle.
///
/// # Safety
/// As [`asdf_file_error_oom`], for a value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_error_system(
    value: *mut crate::file_ffi::AsdfValue,
    errnum: c_int,
    src_file: *const c_char,
    lineno: c_int,
) {
    unsafe { asdf_shim_error_set_system(value.cast(), 1, errnum, src_file, lineno) }
}

/// `ASDF_ERR_OUT_OF_MEMORY`, and the message upstream's shim passed with it.
const OOM_CODE: c_int = ErrorCode::OutOfMemory as c_int;
const OOM_MESSAGE: &CStr = c"out of memory";

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
        // A level that is not a real severity is dropped, never looked up.
        let Some(level) = LogLevel::from_i32(level) else { return };
        if file.is_null() {
            let src = unsafe { crate::ffi::c_string_lossy(src_file) }.unwrap_or_else(|| "?".into());
            LogSettings::fallback().log(
                LogLevel::Fatal,
                &src,
                lineno,
                "logging context not initialized",
            );
            return;
        }
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

/// Log a message against a handle, honouring its file's log settings.
///
/// # Safety
/// See [`asdf_shim_error_set`].
unsafe fn emit_log(
    obj: *mut c_void,
    is_value: c_int,
    level: LogLevel,
    src_file: *const c_char,
    lineno: c_int,
    msg: &str,
) {
    let file = if obj.is_null() {
        None
    } else if is_value != 0 {
        crate::file_ffi::value_file(obj.cast::<crate::file_ffi::AsdfValue>())
    } else {
        Some(obj.cast::<crate::file_ffi::AsdfFile>())
    };
    let settings =
        file.and_then(|f| crate::file_ffi::file_config(f)).map(|c| c.log).unwrap_or_default();
    // A null `src_file` is what the shim passes when it has no source
    // location to report, and upstream prints `?` for it.
    let src = unsafe { crate::ffi::c_string_lossy(src_file) }.unwrap_or_else(|| "?".into());
    settings.log(level, &src, lineno, msg);
}

/// Log a message the library raises itself against a file.
///
/// `src_file` and `lineno` are where in this library the message comes from,
/// as `file!()` and `line!()` give them, standing where upstream's own
/// `__FILE__` and `__LINE__` would.
pub(crate) fn log_to_file(
    file: *mut crate::file_ffi::AsdfFile,
    level: LogLevel,
    src_file: &str,
    lineno: u32,
    msg: &str,
) {
    let settings = crate::file_ffi::file_config(file).map(|c| c.log).unwrap_or_default();
    settings.log(level, src_file, c_int::try_from(lineno).unwrap_or(c_int::MAX), msg);
}

/// The threshold an unset level selects: `ASDF_LOG_LEVEL`, else `WARN`.
///
/// Read afresh on every call, because upstream reads it each time a file is
/// opened: a caller may change it between one open and the next.
pub fn default_log_level() -> LogLevel {
    std::env::var("ASDF_LOG_LEVEL")
        .ok()
        .and_then(|v| LogLevel::from_name(&v))
        .unwrap_or(LogLevel::Warn)
}

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_DIM_GREY: &str = "\x1b[90m";

/// A file's log settings, resolved once when it is opened.
///
/// Upstream folds its `asdf_log_cfg_t` into the file's context at open,
/// reading `ASDF_LOG_LEVEL` and `ASDF_LOG_COLOR` and probing the stream for a
/// terminal then, and never again. This is that resolved form.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LogSettings {
    /// Where output goes; null means `stderr`.
    stream: *mut c_void,
    /// Messages below this are dropped.
    ///
    /// Kept as the raw `asdf_log_level_t` the caller passed, because upstream
    /// compares it as an integer: anything above `ASDF_LOG_NONE` drops every
    /// message too, and is not a value a [`LogLevel`] may hold.
    threshold: c_int,
    /// Which of the `ASDF_LOG_FIELD_*` parts each line carries; never zero.
    fields: u64,
    /// Whether to write ANSI colour codes.
    color: bool,
}

impl Default for LogSettings {
    /// The settings of a file opened without a configuration.
    fn default() -> Self {
        Self::resolve(None)
    }
}

impl LogSettings {
    /// Resolve a caller's log configuration, filling in every unset field.
    pub(crate) fn resolve(config: Option<&crate::types::asdf_log_cfg_t>) -> Self {
        let (stream, level, fields, no_color) = config
            .map_or((core::ptr::null_mut(), 0, 0, false), |c| {
                (c.stream, c.level, c.fields, c.no_color)
            });
        let threshold =
            if level == LogLevel::Default as c_int { default_log_level() as c_int } else { level };
        Self {
            stream,
            threshold,
            fields: if fields == 0 { crate::types::log_field::ALL } else { fields },
            color: use_color(stream, no_color),
        }
    }

    /// What upstream logs with when there is no context to consult: every
    /// field, to `stderr`, at any level.
    fn fallback() -> Self {
        Self {
            stream: core::ptr::null_mut(),
            threshold: LogLevel::Trace as c_int,
            fields: crate::types::log_field::ALL,
            color: use_color(core::ptr::null_mut(), false),
        }
    }

    /// Whether a message at `level` is written: it must be a real severity
    /// at or above the threshold.
    fn emits(&self, level: LogLevel) -> bool {
        level.is_severity() && self.threshold <= level as c_int
    }

    /// Write one message, if [`emits`](Self::emits) allows it.
    pub(crate) fn log(&self, level: LogLevel, src_file: &str, lineno: c_int, msg: &str) {
        if !self.emits(level) {
            return;
        }
        let line = format_line(level, self.fields, self.color, src_file, lineno, msg);
        if self.stream.is_null() {
            use std::io::Write as _;
            let _ = std::io::stderr().write_all(line.as_bytes());
            return;
        }
        // SAFETY: the caller's `asdf_config_t` named this stream and, by the
        // C contract, keeps it open for the file's lifetime. Upstream flushes
        // after every message so none is lost or delayed in a buffer.
        unsafe {
            let stream = self.stream.cast::<libc::FILE>();
            libc::fwrite(line.as_ptr().cast::<c_void>(), 1, line.len(), stream);
            libc::fflush(stream);
        }
    }
}

/// Whether output to `stream` is colourised, as upstream decides it.
///
/// An explicit `no_color` wins outright, since the caller may be parsing the
/// output. Then `ASDF_LOG_COLOR` forces it on (`1`) or off (`0`), ignoring
/// any other value. Otherwise only a terminal gets colour; a file, a pipe or
/// a stream with no descriptor at all would carry the escape codes verbatim.
fn use_color(stream: *mut c_void, no_color: bool) -> bool {
    if no_color {
        return false;
    }
    match std::env::var("ASDF_LOG_COLOR").as_deref() {
        Ok("1") => return true,
        Ok("0") => return false,
        _ => {}
    }
    let fd = if stream.is_null() {
        2
    } else {
        // SAFETY: as in `LogSettings::log`, the stream is the caller's open
        // `FILE *`.
        unsafe { libc::fileno(stream.cast::<libc::FILE>()) }
    };
    // SAFETY: `isatty` accepts any descriptor, reporting a bad one as false.
    fd >= 0 && unsafe { libc::isatty(fd) } == 1
}

/// Lay out one log line exactly as upstream's `asdf_log_impl` does.
///
/// Each field present is separated from the one before by a single space,
/// and a line always ends in a newline, even when it carries no message.
fn format_line(
    level: LogLevel,
    fields: u64,
    color: bool,
    src_file: &str,
    lineno: c_int,
    msg: &str,
) -> String {
    use crate::types::log_field;
    use core::fmt::Write as _;

    let mut line = String::new();
    let mut sep = false;
    if fields & log_field::LEVEL != 0 {
        let name = level.as_str();
        if color {
            let _ = write!(
                line,
                "{ANSI_DIM}[{ANSI_RESET}{}{name}{ANSI_RESET}{ANSI_DIM}]{ANSI_RESET}",
                level.color()
            );
        } else {
            let _ = write!(line, "[{name:<5}]");
        }
        sep = true;
    }
    if fields & (log_field::PACKAGE | log_field::FILE | log_field::LINE) != 0 {
        if sep {
            line.push(' ');
        }
        if color {
            line.push_str(ANSI_DIM_GREY);
        }
        if fields & log_field::PACKAGE != 0 {
            line.push_str("(libasdf)");
        }
        if fields & log_field::FILE != 0 {
            let _ = write!(line, "{src_file}:");
        }
        if fields & log_field::LINE != 0 {
            let _ = write!(line, "{lineno}:");
        }
        if color {
            line.push_str(ANSI_RESET);
        }
        sep = true;
    }
    if fields & log_field::MSG != 0 {
        if sep {
            line.push(' ');
        }
        line.push_str(msg);
    }
    line.push('\n');
    line
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
        // `DEFAULT` is the zero value, not something `ASDF_LOG_LEVEL` names.
        assert_eq!(LogLevel::from_i32(0), Some(LogLevel::Default));
        assert_eq!(LogLevel::from_name("DEFAULT"), None);
        assert_eq!(LogLevel::from_i32(8), None);
    }

    #[test]
    fn log_levels_match_the_header() {
        // `ASDF_LOG_NONE` moved from 0 to above `ASDF_LOG_FATAL` in 0.2.1+.
        assert_eq!(LogLevel::Default as i32, 0);
        assert_eq!(LogLevel::Trace as i32, 1);
        assert_eq!(LogLevel::Fatal as i32, 6);
        assert_eq!(LogLevel::None as i32, 7);
        assert!(!LogLevel::Default.is_severity());
        assert!(!LogLevel::None.is_severity());
        assert!(LogLevel::Trace.is_severity() && LogLevel::Fatal.is_severity());
    }

    #[test]
    fn log_lines_separate_fields_with_one_space() {
        // The cases upstream's `test-log.c` pins in `fields_format`.
        use crate::types::log_field::*;
        let cases = [
            (ALL, "[ERROR] (libasdf)src.c:12: message\n"),
            (LEVEL | MSG, "[ERROR] message\n"),
            (FILE | LINE | MSG, "src.c:12: message\n"),
            (MSG, "message\n"),
            (LEVEL | FILE, "[ERROR] src.c:\n"),
        ];
        for (fields, expected) in cases {
            let line = format_line(LogLevel::Error, fields, false, "src.c", 12, "message");
            assert_eq!(line, expected, "fields {fields:#b}");
        }
        // Short names are padded to the width of the longest.
        assert_eq!(format_line(LogLevel::Warn, LEVEL, false, "", 0, ""), "[WARN ]\n");
    }

    #[test]
    fn colored_log_lines_match_upstream() {
        let line = format_line(LogLevel::Warn, crate::types::log_field::ALL, true, "src.c", 3, "m");
        assert_eq!(
            line,
            "\x1b[2m[\x1b[0m\x1b[33mWARN\x1b[0m\x1b[2m]\x1b[0m \
             \x1b[90m(libasdf)src.c:3:\x1b[0m m\n"
        );
    }

    #[test]
    fn thresholds_compare_as_integers() {
        let at = |threshold: c_int| LogSettings {
            stream: core::ptr::null_mut(),
            threshold,
            fields: crate::types::log_field::ALL,
            color: false,
        };
        let passes = |s: &LogSettings, level| s.emits(level);
        // `NONE`, and anything a caller stores above it, drops even `FATAL`.
        assert!(!passes(&at(LogLevel::None as c_int), LogLevel::Fatal));
        assert!(!passes(&at(99), LogLevel::Fatal));
        // An explicit `WARN` is a threshold like any other, not "unset".
        assert!(passes(&at(LogLevel::Warn as c_int), LogLevel::Warn));
        assert!(!passes(&at(LogLevel::Warn as c_int), LogLevel::Info));
        // Levels that are not severities are never written.
        assert!(!passes(&at(LogLevel::Trace as c_int), LogLevel::Default));
        assert!(!passes(&at(LogLevel::Trace as c_int), LogLevel::None));
    }

    #[test]
    fn every_severity_has_upstreams_color() {
        let expected = [
            (LogLevel::Trace, "\x1b[94m"),
            (LogLevel::Debug, "\x1b[36m"),
            (LogLevel::Info, "\x1b[32m"),
            (LogLevel::Warn, "\x1b[33m"),
            (LogLevel::Error, "\x1b[31m"),
            (LogLevel::Fatal, "\x1b[35m"),
        ];
        for (level, color) in expected {
            let line = format_line(level, crate::types::log_field::LEVEL, true, "", 0, "");
            assert!(line.contains(&format!("{color}{}", level.as_str())), "{level:?}: {line:?}");
        }
        // The two thresholds are never printed, so they carry no colour.
        assert_eq!(LogLevel::Default.color(), "");
        assert_eq!(LogLevel::None.color(), "");
        assert_eq!(LogLevel::Default.as_str(), "DEFAULT");
    }

    /// Everything written so far to a `tmpfile()` stream.
    fn read_back(stream: *mut libc::FILE) -> String {
        let mut out = Vec::new();
        let mut chunk = [0u8; 256];
        unsafe { libc::rewind(stream) };
        loop {
            let n = unsafe { libc::fread(chunk.as_mut_ptr().cast(), 1, chunk.len(), stream) };
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8(out).unwrap()
    }

    /// Every path that logs against a file -- `asdf_file_log`, an error
    /// recorded against the file or against one of its values, and a
    /// message the library raises itself -- goes to the stream, level and
    /// fields that file was opened with.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot run tmpfile")]
    fn a_file_logs_to_its_configured_stream() {
        use crate::file_ffi::{asdf_close, asdf_get_value, asdf_open_mem_ex, asdf_value_destroy};
        use crate::types::log_field::{LEVEL, MSG};

        let stream = unsafe { libc::tmpfile() };
        assert!(!stream.is_null());
        // SAFETY: every field of `asdf_config_t` is an integer, a pointer, a
        // `bool` or an enum whose zero is a variant, so all-zero is valid and
        // is what a C caller's `= {0}` gives.
        let mut config: crate::types::asdf_config_t = unsafe { core::mem::zeroed() };
        config.log.stream = stream.cast();
        config.log.level = LogLevel::Info as c_int;
        config.log.fields = LEVEL | MSG;
        let file = unsafe { asdf_open_mem_ex(core::ptr::null(), 0, &mut config) };
        assert!(!file.is_null());

        unsafe {
            asdf_shim_log_message(
                file.cast(),
                LogLevel::Debug as c_int,
                c"a.c".as_ptr(),
                1,
                c"dropped".as_ptr(),
            );
            asdf_shim_log_message(
                file.cast(),
                LogLevel::Info as c_int,
                c"a.c".as_ptr(),
                2,
                c"kept".as_ptr(),
            );
            asdf_shim_error_set(
                file.cast(),
                0,
                ErrorCode::InvalidArgument as c_int,
                core::ptr::null(),
                3,
                c"on the file".as_ptr(),
            );
        }
        let root = unsafe { asdf_get_value(file, c"".as_ptr()) };
        assert!(!root.is_null());
        unsafe {
            asdf_shim_error_set(
                root.cast(),
                1,
                ErrorCode::ExtensionNotFound as c_int,
                c"a.c".as_ptr(),
                4,
                c"on a value".as_ptr(),
            );
        }
        log_to_file(file, LogLevel::Warn, file!(), line!(), "raised here");

        assert_eq!(
            read_back(stream),
            "[INFO ] kept\n[ERROR] on the file\n[WARN ] on a value\n[WARN ] raised here\n"
        );
        unsafe {
            asdf_value_destroy(root);
            asdf_close(file);
            libc::fclose(stream);
        }
    }

    /// `asdf_file_log` on a null file reports that, the way upstream's
    /// fallback does, rather than dereferencing it. The line goes to
    /// `stderr`, so all this can check is that it returns.
    #[test]
    fn logging_without_a_file_falls_back_to_stderr() {
        unsafe {
            asdf_shim_log_message(
                core::ptr::null(),
                LogLevel::Error as c_int,
                core::ptr::null(),
                5,
                c"unused".as_ptr(),
            );
        }
        let fallback = LogSettings::fallback();
        assert!(fallback.stream.is_null());
        assert_eq!(fallback.fields, crate::types::log_field::ALL);
        assert!(fallback.emits(LogLevel::Trace));
    }

    #[test]
    fn explicit_levels_and_fields_survive_resolution() {
        let config = crate::types::asdf_log_cfg_t {
            stream: core::ptr::null_mut(),
            level: LogLevel::None as c_int,
            fields: 0,
            no_color: true,
        };
        let settings = LogSettings::resolve(Some(&config));
        assert_eq!(settings.threshold, LogLevel::None as c_int);
        assert_eq!(settings.fields, crate::types::log_field::ALL);
        assert!(!settings.color);
    }
}
