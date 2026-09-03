//! Errors, mapped onto the codes the C API reports.

use std::fmt;

/// The error codes `asdf_error_code` reports.
///
/// The discriminants are part of the C ABI and must not be reordered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum ErrorCode {
    /// No error.
    None = 0,
    /// Unknown parser state.
    UnknownState,
    /// Stream initialization failed.
    StreamInitFailed,
    /// Attempted write to a read-only stream or file.
    StreamReadOnly,
    /// Invalid ASDF file header.
    InvalidAsdfHeader,
    /// Unexpected end of file.
    UnexpectedEof,
    /// Invalid block header.
    InvalidBlockHeader,
    /// Block magic bytes did not match.
    BlockMagicMismatch,
    /// YAML parser initialization failed.
    YamlParserInitFailed,
    /// YAML parsing failed.
    YamlParseFailed,
    /// Out of memory.
    OutOfMemory,
    /// OS-level error; the original `errno` is reported separately.
    System,
    /// Invalid argument.
    InvalidArgument,
    /// Unknown compression type.
    UnknownCompression,
    /// Compression or decompression error.
    CompressionFailed,
    /// No serializer registered for an extension.
    ExtensionNotFound,
    /// A system limit has been reached.
    OverLimit,
}

/// An error from the ASDF engine.
#[derive(Debug)]
pub struct Error {
    code: ErrorCode,
    message: String,
    errno: Option<i32>,
}

impl Error {
    /// Build an error with a code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), errno: None }
    }

    /// Build an error carrying an OS `errno`.
    pub fn system(errno: i32, message: impl Into<String>) -> Self {
        Self { code: ErrorCode::System, message: message.into(), errno: Some(errno) }
    }

    /// The code the C API reports for this error.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The OS `errno`, when [`ErrorCode::System`].
    pub fn errno(&self) -> Option<i32> {
        self.errno
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        let errno = e.raw_os_error();
        match errno {
            Some(n) => Error::system(n, e.to_string()),
            None => Error::new(ErrorCode::System, e.to_string()),
        }
    }
}

impl From<asdf_yaml::ParseError> for Error {
    fn from(e: asdf_yaml::ParseError) -> Self {
        Error::new(ErrorCode::YamlParseFailed, e.to_string())
    }
}

/// The engine's result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Shorthand for building an [`Error`].
macro_rules! err {
    ($code:ident, $($arg:tt)*) => {
        $crate::error::Error::new($crate::error::ErrorCode::$code, format!($($arg)*))
    };
}

pub(crate) use err;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminants_match_the_c_abi() {
        // These values are baked into compiled C callers; a reorder is an
        // ABI break, so pin the ones the header documents explicitly.
        assert_eq!(ErrorCode::None as i32, 0);
        assert_eq!(ErrorCode::UnknownState as i32, 1);
        assert_eq!(ErrorCode::StreamInitFailed as i32, 2);
        assert_eq!(ErrorCode::StreamReadOnly as i32, 3);
        assert_eq!(ErrorCode::InvalidAsdfHeader as i32, 4);
        assert_eq!(ErrorCode::UnexpectedEof as i32, 5);
        assert_eq!(ErrorCode::InvalidBlockHeader as i32, 6);
        assert_eq!(ErrorCode::BlockMagicMismatch as i32, 7);
        assert_eq!(ErrorCode::YamlParserInitFailed as i32, 8);
        assert_eq!(ErrorCode::YamlParseFailed as i32, 9);
        assert_eq!(ErrorCode::OutOfMemory as i32, 10);
        assert_eq!(ErrorCode::System as i32, 11);
        assert_eq!(ErrorCode::InvalidArgument as i32, 12);
        assert_eq!(ErrorCode::UnknownCompression as i32, 13);
        assert_eq!(ErrorCode::CompressionFailed as i32, 14);
        assert_eq!(ErrorCode::ExtensionNotFound as i32, 15);
        assert_eq!(ErrorCode::OverLimit as i32, 16);
    }

    #[test]
    fn io_errors_carry_errno() {
        let io = std::io::Error::from_raw_os_error(2);
        let e = Error::from(io);
        assert_eq!(e.code(), ErrorCode::System);
        assert_eq!(e.errno(), Some(2));
    }
}
