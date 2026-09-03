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
use crate::node::{Node, NodeData, NodeId};

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

impl Document {
    /// Insert `value` at `path`, creating intermediate mappings as needed.
    ///
    /// This mirrors libasdf's `asdf_node_insert_at` with materialisation on:
    /// setting `powers/squares` in an empty tree creates the `powers` mapping
    /// on the way. Returns the node that was replaced, if any.
    ///
    /// Intermediate steps are only ever created as mappings, since a bare
    /// path component gives no way to say "make a sequence here". A component
    /// that names an existing sequence still indexes into it.
    pub fn insert_at(&mut self, path: &Path, value: NodeId) -> Result<Option<NodeId>, PathError> {
        let Some((last, leading)) = path.components().split_last() else {
            // An empty path replaces the root outright.
            let previous = self.root();
            self.set_root(value);
            return Ok(previous);
        };

        // Walk to the parent, materialising mappings that are not there yet.
        let mut current = match self.root() {
            Some(root) => root,
            None => {
                let root = self.add(Node::mapping());
                self.set_root(root);
                root
            }
        };

        for component in leading {
            let resolved = self.resolve(current);
            let existing = match component {
                Component::Key(key) => self.mapping_get(resolved, key),
                Component::Index(index) => self.sequence_get(resolved, *index),
                Component::Bare(text) => match &self.node(resolved).data {
                    NodeData::Sequence { .. } => {
                        text.parse::<i64>().ok().and_then(|i| self.sequence_get(resolved, i))
                    }
                    _ => self.mapping_get(resolved, text),
                },
            };

            current = match existing {
                // Descend through anything that can hold children.
                Some(node) if self.node(self.resolve(node)).is_mapping() => node,
                Some(node) if self.node(self.resolve(node)).is_sequence() => node,
                // A scalar in the way is replaced by a mapping, since the
                // caller has asked for a path through it.
                _ => {
                    let fresh = self.add(Node::mapping());
                    self.set_component(resolved, component, fresh)?;
                    fresh
                }
            };
        }

        let parent = self.resolve(current);
        self.set_component(parent, last, value)
    }

    /// Parse and insert in one step.
    pub fn insert_at_str(
        &mut self,
        path: &str,
        value: NodeId,
    ) -> Result<Option<NodeId>, PathError> {
        let parsed = Path::parse(path)?;
        self.insert_at(&parsed, value)
    }

    /// Set one component of a container, replacing whatever was there.
    fn set_component(
        &mut self,
        parent: NodeId,
        component: &Component,
        value: NodeId,
    ) -> Result<Option<NodeId>, PathError> {
        let is_sequence = self.node(parent).is_sequence();

        match component {
            Component::Key(key) => Ok(self.mapping_set(parent, key, value)),
            Component::Index(index) => Ok(self.sequence_set(parent, *index, value)),
            Component::Bare(text) => {
                if is_sequence
                    && let Ok(index) = text.parse::<i64>()
                {
                    return Ok(self.sequence_set(parent, index, value));
                }
                Ok(self.mapping_set(parent, text, value))
            }
        }
    }

    /// Replace a sequence element, or append when the index is one past the
    /// end. Returns the element that was replaced.
    pub fn sequence_set(&mut self, id: NodeId, index: i64, value: NodeId) -> Option<NodeId> {
        let target = self.resolve(id);
        let len = self.container_len(target)? as i64;
        let idx = if index < 0 { len + index } else { index };

        let NodeData::Sequence { items, .. } = &mut self.node_mut(target).data else {
            return None;
        };
        if idx == len {
            items.push(value);
            return None;
        }
        if idx < 0 || idx > len {
            return None;
        }
        Some(std::mem::replace(&mut items[idx as usize], value))
    }

    /// Remove whatever is at `path`, returning it.
    pub fn remove_at_str(&mut self, path: &str) -> Option<NodeId> {
        let parsed = Path::parse(path).ok()?;
        let (last, leading) = parsed.components().split_last()?;

        let mut current = self.root()?;
        for component in leading {
            let resolved = self.resolve(current);
            current = match component {
                Component::Key(key) => self.mapping_get(resolved, key)?,
                Component::Index(index) => self.sequence_get(resolved, *index)?,
                Component::Bare(text) => match &self.node(resolved).data {
                    NodeData::Sequence { .. } => {
                        self.sequence_get(resolved, text.parse::<i64>().ok()?)?
                    }
                    _ => self.mapping_get(resolved, text)?,
                },
            };
        }

        let parent = self.resolve(current);
        match last {
            Component::Key(key) => self.mapping_remove(parent, key),
            Component::Index(index) => self.sequence_remove(parent, *index),
            Component::Bare(text) => {
                if self.node(parent).is_sequence()
                    && let Ok(index) = text.parse::<i64>()
                {
                    return self.sequence_remove(parent, index);
                }
                self.mapping_remove(parent, text)
            }
        }
    }

    /// Remove a sequence element, returning it.
    pub fn sequence_remove(&mut self, id: NodeId, index: i64) -> Option<NodeId> {
        let target = self.resolve(id);
        let len = self.container_len(target)? as i64;
        let idx = if index < 0 { len + index } else { index };
        if idx < 0 || idx >= len {
            return None;
        }
        let NodeData::Sequence { items, .. } = &mut self.node_mut(target).data else {
            return None;
        };
        Some(items.remove(idx as usize))
    }
}

#[cfg(test)]
mod insert_tests {
    use super::*;
    use crate::node::ScalarStyle;
    use crate::parse::parse_document;

    fn get<'a>(d: &'a Document, path: &str) -> Option<&'a str> {
        d.lookup_str(path).map(|id| d.resolved(id).as_str().unwrap_or("<container>"))
    }

    #[test]
    fn sets_a_top_level_key() {
        let mut doc = parse_document("a: 1\n").unwrap();
        let v = doc.add_scalar("2");
        let previous = doc.insert_at_str("b", v).unwrap();
        assert!(previous.is_none());
        assert_eq!(get(&doc, "b"), Some("2"));
        assert_eq!(get(&doc, "a"), Some("1"));
    }

    #[test]
    fn replaces_an_existing_value() {
        let mut doc = parse_document("a: 1\n").unwrap();
        let v = doc.add_scalar("9");
        let previous = doc.insert_at_str("a", v).unwrap();
        assert!(previous.is_some());
        assert_eq!(get(&doc, "a"), Some("9"));
        assert_eq!(doc.container_len(doc.root().unwrap()), Some(1));
    }

    #[test]
    fn materialises_intermediate_mappings() {
        // The README's case: setting `powers/squares` in a tree that has
        // neither must create `powers` on the way.
        let mut doc = parse_document("a: 1\n").unwrap();
        let v = doc.add_scalar("squared");
        doc.insert_at_str("powers/squares", v).unwrap();

        assert_eq!(get(&doc, "powers/squares"), Some("squared"));
        assert!(doc.node(doc.lookup_str("powers").unwrap()).is_mapping());
    }

    #[test]
    fn materialises_several_levels() {
        let mut doc = Document::new();
        let v = doc.add_scalar("deep");
        doc.insert_at_str("a/b/c/d", v).unwrap();
        assert_eq!(get(&doc, "a/b/c/d"), Some("deep"));
    }

    #[test]
    fn creates_a_root_when_there_is_none() {
        let mut doc = Document::new();
        let v = doc.add_scalar("hello");
        doc.insert_at_str("greeting", v).unwrap();
        assert!(doc.root().is_some());
        assert_eq!(get(&doc, "greeting"), Some("hello"));
    }

    #[test]
    fn an_empty_path_replaces_the_root() {
        let mut doc = parse_document("a: 1\n").unwrap();
        let v = doc.add_scalar("replaced");
        doc.insert_at_str("", v).unwrap();
        assert_eq!(doc.root(), Some(v));
    }

    #[test]
    fn a_scalar_in_the_way_becomes_a_mapping() {
        let mut doc = parse_document("a: scalar\n").unwrap();
        let v = doc.add_scalar("1");
        doc.insert_at_str("a/b", v).unwrap();
        assert_eq!(get(&doc, "a/b"), Some("1"));
        assert!(doc.node(doc.lookup_str("a").unwrap()).is_mapping());
    }

    #[test]
    fn writes_through_a_sequence_index() {
        let mut doc = parse_document("s: [x, y, z]\n").unwrap();
        let v = doc.add_scalar("Y");
        doc.insert_at_str("s/1", v).unwrap();
        assert_eq!(get(&doc, "s/1"), Some("Y"));
        assert_eq!(doc.container_len(doc.lookup_str("s").unwrap()), Some(3));
    }

    #[test]
    fn appending_one_past_the_end_extends_a_sequence() {
        let mut doc = parse_document("s: [x]\n").unwrap();
        let v = doc.add_scalar("y");
        doc.insert_at_str("s/1", v).unwrap();
        assert_eq!(doc.container_len(doc.lookup_str("s").unwrap()), Some(2));
        assert_eq!(get(&doc, "s/1"), Some("y"));
    }

    #[test]
    fn a_quoted_component_makes_a_string_key_even_over_a_sequence() {
        let mut doc = parse_document("a: {}\n").unwrap();
        let v = doc.add_scalar("1");
        doc.insert_at_str("a/'0'", v).unwrap();
        assert_eq!(get(&doc, "a/'0'"), Some("1"));
    }

    #[test]
    fn removes_values() {
        let mut doc = parse_document("a: 1\nb:\n  c: 2\ns: [x, y]\n").unwrap();

        assert!(doc.remove_at_str("a").is_some());
        assert!(doc.lookup_str("a").is_none());

        assert!(doc.remove_at_str("b/c").is_some());
        assert!(doc.lookup_str("b/c").is_none());
        // The parent survives.
        assert!(doc.lookup_str("b").is_some());

        assert!(doc.remove_at_str("s/0").is_some());
        assert_eq!(doc.container_len(doc.lookup_str("s").unwrap()), Some(1));
        assert_eq!(get(&doc, "s/0"), Some("y"));

        assert!(doc.remove_at_str("missing").is_none());
    }

    #[test]
    fn inserted_trees_emit_and_re_read() {
        use crate::emit::emit;
        use crate::parse::parse_document as reparse;

        let mut doc = Document::new_asdf();
        let name = doc.add_scalar_styled("Dennis Richie", ScalarStyle::Plain);
        doc.insert_at_str("name", name).unwrap();
        let foo = doc.add_scalar("42");
        doc.insert_at_str("foo", foo).unwrap();
        let sq = doc.add_scalar("1764");
        doc.insert_at_str("powers/squares", sq).unwrap();

        let text = emit(&doc).unwrap();
        let back = reparse(&text).unwrap();
        assert_eq!(
            back.lookup_str("powers/squares")
                .map(|id| back.resolved(id).as_str().unwrap().to_string()),
            Some("1764".to_string())
        );
    }
}
