//! YAML tags, and the ASDF conventions layered on top of them.

use std::fmt;

/// The tag prefix all ASDF Standard tags share.
pub const ASDF_STANDARD_TAG_PREFIX: &str = "tag:stsci.edu:asdf/";

/// The tag prefix for the ASDF core schemas.
pub const ASDF_CORE_TAG_PREFIX: &str = "tag:stsci.edu:asdf/core/";

/// The prefix for YAML's own built-in tags.
pub const YAML_TAG_PREFIX: &str = "tag:yaml.org,2002:";

/// A fully-resolved YAML tag.
///
/// `saphyr-parser` hands us tags already split into an expanded `handle` (the
/// prefix a `%TAG` directive mapped the shorthand to) and a `suffix`. We keep
/// the two apart because emitting needs to re-shorten the tag against the
/// document's directives, which requires knowing where the split was.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag {
    handle: String,
    suffix: String,
}

impl Tag {
    /// Build a tag from an already-expanded handle and suffix.
    pub fn new(handle: impl Into<String>, suffix: impl Into<String>) -> Self {
        Self { handle: handle.into(), suffix: suffix.into() }
    }

    /// Build a tag from a full tag string, splitting it at the ASDF prefix
    /// when it has one so that it re-shortens the way it arrived.
    pub fn parse(full: &str) -> Self {
        for prefix in [ASDF_STANDARD_TAG_PREFIX, YAML_TAG_PREFIX] {
            if let Some(rest) = full.strip_prefix(prefix) {
                return Self::new(prefix, rest);
            }
        }
        Self::new(full, "")
    }

    /// The expanded prefix this tag's shorthand resolved to.
    pub fn handle(&self) -> &str {
        &self.handle
    }

    /// The part of the tag following the handle.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// The full tag: handle and suffix concatenated.
    pub fn full(&self) -> String {
        format!("{}{}", self.handle, self.suffix)
    }

    /// Whether this is a tag from the ASDF Standard.
    pub fn is_asdf(&self) -> bool {
        self.handle == ASDF_STANDARD_TAG_PREFIX
            || self.full().starts_with(ASDF_STANDARD_TAG_PREFIX)
    }

    /// Whether this is one of YAML's own built-in tags (`!!str`, `!!int`, ...).
    pub fn is_yaml_builtin(&self) -> bool {
        self.handle == YAML_TAG_PREFIX || self.full().starts_with(YAML_TAG_PREFIX)
    }

    /// The tag with the ASDF prefix stripped, e.g. `core/ndarray-1.1.0`.
    ///
    /// Returns `None` for tags outside the ASDF Standard.
    pub fn asdf_name(&self) -> Option<&str> {
        if self.handle == ASDF_STANDARD_TAG_PREFIX {
            Some(&self.suffix)
        } else {
            None
        }
    }

    /// Split an ASDF tag into its schema name and version, e.g.
    /// `core/ndarray-1.1.0` into `("core/ndarray", "1.1.0")`.
    ///
    /// This mirrors `asdf_tag_parse`. A tag with no parseable trailing version
    /// yields the whole name and `None`.
    pub fn split_version(&self) -> (&str, Option<&str>) {
        let name = self.asdf_name().unwrap_or(&self.suffix);
        split_tag_version(name)
    }
}

/// Split a `name-X.Y.Z` string into its name and version parts.
///
/// Only a trailing component that looks like a version (starts with a digit
/// after the final `-`) is split off, so `history_entry-1.0.0` splits but a
/// hypothetical `some-name` does not.
pub fn split_tag_version(name: &str) -> (&str, Option<&str>) {
    match name.rfind('-') {
        Some(idx) => {
            let (head, tail) = name.split_at(idx);
            let version = &tail[1..];
            if version.starts_with(|c: char| c.is_ascii_digit()) {
                (head, Some(version))
            } else {
                (name, None)
            }
        }
        None => (name, None),
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.handle, self.suffix)
    }
}

impl fmt::Debug for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tag({}{})", self.handle, self.suffix)
    }
}

/// A `%TAG` directive: a shorthand handle bound to a prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagHandle {
    /// The shorthand, including its delimiting `!` characters, e.g. `!` or `!x!`.
    pub handle: String,
    /// The prefix the shorthand expands to.
    pub prefix: String,
}

impl TagHandle {
    /// The `%TAG ! tag:stsci.edu:asdf/` directive ASDF files conventionally use.
    pub fn asdf_default() -> Self {
        Self { handle: "!".into(), prefix: ASDF_STANDARD_TAG_PREFIX.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_asdf_tag() {
        let t = Tag::parse("tag:stsci.edu:asdf/core/ndarray-1.1.0");
        assert_eq!(t.handle(), ASDF_STANDARD_TAG_PREFIX);
        assert_eq!(t.suffix(), "core/ndarray-1.1.0");
        assert!(t.is_asdf());
        assert_eq!(t.asdf_name(), Some("core/ndarray-1.1.0"));
    }

    #[test]
    fn splits_version() {
        let t = Tag::parse("tag:stsci.edu:asdf/core/ndarray-1.1.0");
        assert_eq!(t.split_version(), ("core/ndarray", Some("1.1.0")));

        let t = Tag::parse("tag:stsci.edu:asdf/core/history_entry-1.0.0");
        assert_eq!(t.split_version(), ("core/history_entry", Some("1.0.0")));
    }

    #[test]
    fn hyphen_without_version_is_not_split() {
        assert_eq!(split_tag_version("some-name"), ("some-name", None));
        assert_eq!(split_tag_version("plain"), ("plain", None));
    }

    #[test]
    fn recognises_yaml_builtins() {
        let t = Tag::parse("tag:yaml.org,2002:int");
        assert!(t.is_yaml_builtin());
        assert!(!t.is_asdf());
        assert_eq!(t.asdf_name(), None);
    }

    #[test]
    fn full_round_trips() {
        for s in [
            "tag:stsci.edu:asdf/core/asdf-1.1.0",
            "tag:yaml.org,2002:str",
            "!local",
        ] {
            assert_eq!(Tag::parse(s).full(), s);
        }
    }
}
