//! The YAML Pointer path syntax used to address values in the tree.
//!
//! This is libfyaml's native lookup syntax, which libasdf inherits and which
//! is only informally specified. The rules, as libasdf's own documentation
//! sets them out:
//!
//! - Components are separated by `/`.
//! - A non-numeric component is always a mapping key.
//! - A numeric component depends on context: a mapping key when the parent is
//!   a mapping, a sequence index when the parent is a sequence. Sequence
//!   indices may be negative, counting back from the end.
//! - `[N]` is *always* a sequence index, and fails against a mapping.
//! - `'a'` or `"a"` is *always* a mapping key, so `'0'` is the string key
//!   `0` rather than an index.
//! - Escaping uses backslashes rather than JSON Pointer's `~`. The characters
//!   needing escape are ``/{}[].&*\``; inside quotes none of them do.

use crate::document::Document;
use crate::node::{NodeData, NodeId};

/// One step of a path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Component {
    /// A mapping key, stated unambiguously by quoting.
    Key(String),
    /// A sequence index, stated unambiguously by brackets. Negative values
    /// count back from the end.
    Index(i64),
    /// A bare component, resolved against whatever the parent turns out to be.
    Bare(String),
}

/// The characters that must be escaped outside quotes.
pub const ESCAPE_CHARS: &[char] = &['/', '{', '}', '[', ']', '.', '&', '*', '\\'];

/// An error in a path's syntax.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PathError {
    /// A quoted component was never closed.
    #[error("unterminated quote in path component {0:?}")]
    UnterminatedQuote(String),
    /// A bracketed component was never closed.
    #[error("unterminated bracket in path component {0:?}")]
    UnterminatedBracket(String),
    /// The contents of `[...]` were not an integer.
    #[error("bracketed path component {0:?} is not an integer")]
    NonIntegerIndex(String),
    /// A trailing backslash with nothing to escape.
    #[error("path ends with a dangling escape")]
    DanglingEscape,
}

/// A parsed path.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Path {
    components: Vec<Component>,
}

impl Path {
    /// Parse a path string.
    ///
    /// A leading `/` is optional and ignored, so `a/b` and `/a/b` are the
    /// same path. An empty path addresses the root.
    pub fn parse(path: &str) -> Result<Self, PathError> {
        let mut components = Vec::new();
        let mut rest = path.strip_prefix('/').unwrap_or(path);

        if rest.is_empty() {
            return Ok(Path { components });
        }

        while !rest.is_empty() {
            let (component, remainder) = parse_component(rest)?;
            components.push(component);
            rest = match remainder.strip_prefix('/') {
                Some(r) => r,
                None => {
                    debug_assert!(remainder.is_empty());
                    ""
                }
            };
            // A trailing slash addresses the same node, matching how a
            // directory-style path reads.
            if rest.is_empty() {
                break;
            }
        }
        Ok(Path { components })
    }

    /// The components, in order.
    pub fn components(&self) -> &[Component] {
        &self.components
    }

    /// Whether this path addresses the root.
    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }
}

/// Split one component off the front of `s`, returning it and the remainder.
fn parse_component(s: &str) -> Result<(Component, &str), PathError> {
    let mut chars = s.char_indices().peekable();

    match chars.peek() {
        // Bracketed: always a sequence index.
        Some((_, '[')) => {
            let close = s.find(']').ok_or_else(|| {
                PathError::UnterminatedBracket(s.to_string())
            })?;
            let body = &s[1..close];
            let index = body
                .trim()
                .parse::<i64>()
                .map_err(|_| PathError::NonIntegerIndex(body.to_string()))?;
            Ok((Component::Index(index), &s[close + 1..]))
        }

        // Quoted: always a mapping key, with no escaping inside.
        Some((_, quote @ ('\'' | '"'))) => {
            let quote = *quote;
            let close = s[1..]
                .find(quote)
                .ok_or_else(|| PathError::UnterminatedQuote(s.to_string()))?
                + 1;
            let key = s[1..close].to_string();
            Ok((Component::Key(key), &s[close + 1..]))
        }

        // Bare: runs to the next unescaped separator.
        _ => {
            let mut out = String::new();
            let mut escaped = false;
            let mut end = s.len();

            for (idx, ch) in s.char_indices() {
                if escaped {
                    out.push(ch);
                    escaped = false;
                    continue;
                }
                match ch {
                    '\\' => escaped = true,
                    '/' => {
                        end = idx;
                        break;
                    }
                    _ => out.push(ch),
                }
            }
            if escaped {
                return Err(PathError::DanglingEscape);
            }
            Ok((Component::Bare(out), &s[end..]))
        }
    }
}

/// Escape a string so it survives a round trip through [`Path::parse`] as a
/// single bare component.
pub fn escape_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ESCAPE_CHARS.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

impl Document {
    /// Resolve a path against a starting node.
    ///
    /// Aliases are followed at each step, so a path may descend through a
    /// shared node.
    pub fn lookup_from(&self, start: NodeId, path: &Path) -> Option<NodeId> {
        let mut current = start;

        for component in path.components() {
            let resolved = self.resolve(current);
            current = match component {
                Component::Key(key) => self.mapping_get(resolved, key)?,
                Component::Index(index) => self.sequence_get(resolved, *index)?,
                Component::Bare(text) => match &self.node(resolved).data {
                    // A bare component is a key when the parent is a mapping,
                    // whether or not it looks numeric.
                    NodeData::Mapping { .. } => self.mapping_get(resolved, text)?,
                    NodeData::Sequence { .. } => {
                        let index = text.parse::<i64>().ok()?;
                        self.sequence_get(resolved, index)?
                    }
                    _ => return None,
                },
            };
        }
        Some(current)
    }

    /// Resolve a path against the document root.
    pub fn lookup(&self, path: &Path) -> Option<NodeId> {
        self.lookup_from(self.root()?, path)
    }

    /// Parse and resolve a path in one step.
    pub fn lookup_str(&self, path: &str) -> Option<NodeId> {
        self.lookup(&Path::parse(path).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_document;

    fn doc() -> Document {
        parse_document(
            "a:\n  b: 1\n  '0': zero-as-key\nseq: [x, y, z]\nnested:\n  - k: v\n  - k: w\n",
        )
        .unwrap()
    }

    fn get<'a>(d: &'a Document, path: &str) -> Option<&'a str> {
        d.lookup_str(path).map(|id| d.resolved(id).as_str().unwrap_or("<container>"))
    }

    #[test]
    fn parses_simple_paths() {
        let p = Path::parse("a/b").unwrap();
        assert_eq!(
            p.components(),
            [Component::Bare("a".into()), Component::Bare("b".into())]
        );
        // A leading slash is optional.
        assert_eq!(Path::parse("/a/b").unwrap(), p);
    }

    #[test]
    fn empty_path_is_the_root() {
        assert!(Path::parse("").unwrap().is_root());
        assert!(Path::parse("/").unwrap().is_root());
    }

    #[test]
    fn looks_up_nested_mappings() {
        let d = doc();
        assert_eq!(get(&d, "a/b"), Some("1"));
        assert_eq!(get(&d, "/a/b"), Some("1"));
        assert!(get(&d, "a/missing").is_none());
    }

    #[test]
    fn numeric_component_indexes_a_sequence() {
        let d = doc();
        assert_eq!(get(&d, "seq/0"), Some("x"));
        assert_eq!(get(&d, "seq/2"), Some("z"));
        assert!(get(&d, "seq/3").is_none());
    }

    #[test]
    fn negative_indices_count_from_the_end() {
        let d = doc();
        assert_eq!(get(&d, "seq/-1"), Some("z"));
        assert_eq!(get(&d, "seq/-3"), Some("x"));
        assert!(get(&d, "seq/-4").is_none());
    }

    #[test]
    fn numeric_component_is_a_key_under_a_mapping() {
        // The context rule: '0' addresses the *key* "0", not an index.
        let d = doc();
        assert_eq!(get(&d, "a/0"), Some("zero-as-key"));
    }

    #[test]
    fn brackets_force_a_sequence_index() {
        let d = doc();
        assert_eq!(get(&d, "seq/[1]"), Some("y"));
        assert_eq!(get(&d, "seq/[-1]"), Some("z"));
        // Against a mapping it must fail, even though the key "0" exists.
        assert!(get(&d, "a/[0]").is_none());
    }

    #[test]
    fn quotes_force_a_mapping_key() {
        let d = doc();
        assert_eq!(get(&d, "a/'0'"), Some("zero-as-key"));
        assert_eq!(get(&d, "a/\"0\""), Some("zero-as-key"));
        // Against a sequence a quoted component must fail: it is a string key.
        assert!(get(&d, "seq/'0'").is_none());
    }

    #[test]
    fn descends_through_sequences_of_mappings() {
        let d = doc();
        assert_eq!(get(&d, "nested/0/k"), Some("v"));
        assert_eq!(get(&d, "nested/1/k"), Some("w"));
        assert_eq!(get(&d, "nested/-1/k"), Some("w"));
    }

    #[test]
    fn backslash_escapes_a_separator() {
        let d = parse_document("a/b: slashed\n").unwrap();
        assert_eq!(get(&d, "a\\/b"), Some("slashed"));
        // Without the escape it is two components and finds nothing.
        assert!(get(&d, "a/b").is_none());
    }

    #[test]
    fn quotes_avoid_the_need_to_escape() {
        let d = parse_document("a.b: dotted\n").unwrap();
        assert_eq!(get(&d, "'a.b'"), Some("dotted"));
        assert_eq!(get(&d, "a\\.b"), Some("dotted"));
    }

    #[test]
    fn escape_round_trips() {
        for raw in ["a/b", "a.b", "a[0]", "a&b", "plain"] {
            let escaped = escape_component(raw);
            let parsed = Path::parse(&escaped).unwrap();
            assert_eq!(
                parsed.components(),
                [Component::Bare(raw.to_string())],
                "{raw:?} escaped to {escaped:?}"
            );
        }
    }

    #[test]
    fn paths_follow_aliases() {
        let d = parse_document("target: &a {x: 1}\nalias: *a\n").unwrap();
        assert_eq!(get(&d, "alias/x"), Some("1"));
        assert_eq!(get(&d, "target/x"), Some("1"));
    }

    #[test]
    fn descending_into_a_scalar_fails() {
        let d = doc();
        assert!(get(&d, "a/b/c").is_none());
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert_eq!(
            Path::parse("'unterminated"),
            Err(PathError::UnterminatedQuote("'unterminated".into()))
        );
        assert_eq!(
            Path::parse("[1"),
            Err(PathError::UnterminatedBracket("[1".into()))
        );
        assert_eq!(
            Path::parse("[abc]"),
            Err(PathError::NonIntegerIndex("abc".into()))
        );
        assert_eq!(Path::parse("a\\"), Err(PathError::DanglingEscape));
    }

    #[test]
    fn trailing_slash_is_ignored() {
        let d = doc();
        assert_eq!(get(&d, "a/b/"), Some("1"));
    }
}
