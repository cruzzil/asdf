//! Version strings, as they appear on the `#ASDF` header line, in tags, and
//! in `core/software` metadata.

use std::fmt;

/// A parsed version string.
///
/// Mirrors `asdf_version_t`, including its tolerance for versions that are not
/// `MAJOR.MINOR.PATCH`: those keep the full string and leave the numeric
/// fields at zero.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Version {
    /// The full, unparsed version string.
    pub version: String,
    /// Major version, or 0 if the string was not `X.Y.Z`.
    pub major: u32,
    /// Minor version, or 0.
    pub minor: u32,
    /// Patch version, or 0.
    pub patch: u32,
    /// Trailing version information, if any.
    ///
    /// A separator following a complete `X.Y.Z` is dropped, so `1.2.3-rc1`
    /// and `1.2.3.dev4` both yield `rc1` / `dev4`. A separator appearing
    /// earlier is kept verbatim, matching upstream.
    pub extra: Option<String>,
}

/// Consume a run of ASCII digits, returning the value and the rest.
fn take_u32(s: &str) -> Option<(u32, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    // Saturate rather than fail: C's strtoul clamps at ULONG_MAX, and a
    // version component that large is malformed either way.
    let value = s[..end].parse::<u32>().unwrap_or(u32::MAX);
    Some((value, &s[end..]))
}

impl Version {
    /// Parse a version string.
    ///
    /// This never fails; an unparseable string is preserved verbatim with
    /// zeroed numeric fields, exactly as `asdf_version_parse` behaves.
    pub fn parse(version: &str) -> Self {
        let mut out = Version { version: version.to_string(), ..Default::default() };

        // Not a semver at all; keep the string and stop.
        let Some((major, rest)) = take_u32(version) else {
            return out;
        };
        out.major = major;

        let Some(rest) = rest.strip_prefix('.') else {
            if !rest.is_empty() {
                out.extra = Some(rest.to_string());
            }
            return out;
        };

        let Some((minor, rest)) = take_u32(rest) else {
            out.extra = Some(rest.to_string());
            return out;
        };
        out.minor = minor;

        let Some(rest) = rest.strip_prefix('.') else {
            if !rest.is_empty() {
                out.extra = Some(rest.to_string());
            }
            return out;
        };

        let Some((patch, rest)) = take_u32(rest) else {
            out.extra = Some(rest.to_string());
            return out;
        };
        out.patch = patch;

        // Only after a complete X.Y.Z is a single separator absorbed.
        if !rest.is_empty() {
            let tail = rest.strip_prefix(['.', '-']).unwrap_or(rest);
            if !tail.is_empty() {
                out.extra = Some(tail.to_string());
            }
        }
        out
    }

    /// Build a version from its numeric components.
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            version: format!("{major}.{minor}.{patch}"),
            major,
            minor,
            patch,
            extra: None,
        }
    }

    /// The `(major, minor, patch)` triple, for ordering comparisons.
    pub fn triple(&self) -> (u32, u32, u32) {
        (self.major, self.minor, self.patch)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.version)
    }
}

/// The ASDF low-level file format version this library writes.
pub const ASDF_FORMAT_VERSION: &str = "1.0.0";

/// The ASDF Standard version this library writes by default.
pub const ASDF_STANDARD_VERSION: &str = "1.6.0";

/// The ASDF Standard versions this library knows about.
pub const SUPPORTED_STANDARD_VERSIONS: &[&str] =
    &["1.0.0", "1.1.0", "1.2.0", "1.3.0", "1.4.0", "1.5.0", "1.6.0"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_semver() {
        let v = Version::parse("1.6.0");
        assert_eq!(v.triple(), (1, 6, 0));
        assert_eq!(v.extra, None);
        assert_eq!(v.version, "1.6.0");
    }

    #[test]
    fn separator_after_patch_is_absorbed() {
        // Both PEP-440's dot and semver's hyphen are dropped after X.Y.Z.
        let v = Version::parse("0.1.0.dev4");
        assert_eq!(v.triple(), (0, 1, 0));
        assert_eq!(v.extra.as_deref(), Some("dev4"));

        let v = Version::parse("1.2.3-rc1");
        assert_eq!(v.triple(), (1, 2, 3));
        assert_eq!(v.extra.as_deref(), Some("rc1"));
    }

    #[test]
    fn other_suffixes_are_kept_verbatim() {
        // No separator to absorb, so the suffix arrives whole.
        let v = Version::parse("0.1.0rc2");
        assert_eq!(v.triple(), (0, 1, 0));
        assert_eq!(v.extra.as_deref(), Some("rc2"));
    }

    #[test]
    fn separator_before_patch_is_kept() {
        // Upstream only strips the separator after a *complete* X.Y.Z, so a
        // hyphen appearing earlier survives into `extra`.
        let v = Version::parse("1.2-foo");
        assert_eq!(v.triple(), (1, 2, 0));
        assert_eq!(v.extra.as_deref(), Some("-foo"));
    }

    #[test]
    fn truncated_versions_zero_the_rest() {
        let v = Version::parse("1");
        assert_eq!(v.triple(), (1, 0, 0));
        assert_eq!(v.extra, None);

        let v = Version::parse("1.2");
        assert_eq!(v.triple(), (1, 2, 0));
        assert_eq!(v.extra, None);
    }

    #[test]
    fn non_numeric_is_preserved_verbatim() {
        let v = Version::parse("not-a-version");
        assert_eq!(v.triple(), (0, 0, 0));
        assert_eq!(v.version, "not-a-version");
        assert_eq!(v.extra, None);
    }

    #[test]
    fn missing_component_becomes_extra() {
        let v = Version::parse("1.x");
        assert_eq!(v.triple(), (1, 0, 0));
        assert_eq!(v.extra.as_deref(), Some("x"));
    }

    #[test]
    fn empty_string() {
        let v = Version::parse("");
        assert_eq!(v.triple(), (0, 0, 0));
        assert_eq!(v.version, "");
    }

    #[test]
    fn display_round_trips_the_original() {
        for s in ["1.6.0", "0.1.0rc2", "not-a-version", "1.2.3-rc1"] {
            assert_eq!(Version::parse(s).to_string(), s);
        }
    }
}
