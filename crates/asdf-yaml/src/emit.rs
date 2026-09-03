//! Emitting a [`Document`] back to YAML.
//!
//! This exists because no Rust YAML crate can write what ASDF requires.
//! Both `saphyr` and `yaml-rust2` drop aliases silently -- their emitters
//! match `Yaml::Alias(_) => Ok(())` -- never write anchors, and emit no
//! `%YAML` or `%TAG` directives. ASDF mandates all of those: the tree is a
//! tagged document introduced by directives, and the standard's own reference
//! corpus shares nodes through aliases.
//!
//! Output is judged at the value level rather than byte for byte (see
//! `KNOWN-DIVERGENCES.md`), so this aims at clean, conventional YAML rather
//! than at reproducing libfyaml's exact line breaking.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::document::Document;
use crate::node::{CollectionStyle, NodeData, NodeId, ScalarStyle};
use crate::tag::TagHandle;

/// How to lay out the emitted document.
#[derive(Clone, Debug)]
pub struct EmitOptions {
    /// Spaces per indentation level.
    pub indent: usize,
    /// Write the `%YAML` and `%TAG` directives.
    pub directives: bool,
    /// Write the `---` document start marker.
    pub explicit_start: bool,
    /// Write the `...` document end marker, which ASDF requires so a reader
    /// can find where the tree ends.
    pub explicit_end: bool,
    /// Emit sequences of scalars inline when they fit within this many
    /// characters. Zero disables it.
    pub flow_seq_max_width: usize,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            indent: 2,
            directives: true,
            explicit_start: true,
            explicit_end: true,
            flow_seq_max_width: 72,
        }
    }
}

/// A problem that prevented emission.
#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    /// The document has no root node.
    #[error("document has no root node")]
    NoRoot,
    /// The document contains a reference cycle, which YAML cannot represent
    /// without an anchor on the cycle's entry point.
    #[error("cycle detected at node {0:?}")]
    Cycle(NodeId),
}

/// Which nodes need an anchor written, and under what name.
///
/// Only a node that at least one alias actually points at gets one; an
/// anchor nothing refers to is noise.
fn collect_anchors(doc: &Document) -> HashMap<NodeId, String> {
    let mut targets = HashSet::new();
    for index in 0..doc.node_count() {
        let id = NodeId(index as u32);
        if let Some(node) = doc.get(id)
            && let NodeData::Alias(target) = node.data
        {
            targets.insert(doc.resolve(target));
        }
    }

    let mut out = HashMap::new();
    let mut counter = 0;
    for index in 0..doc.node_count() {
        let id = NodeId(index as u32);
        if !targets.contains(&id) {
            continue;
        }
        let name = doc.get(id).and_then(|n| n.anchor.clone()).unwrap_or_else(|| {
            counter += 1;
            format!("id{counter:03}")
        });
        out.insert(id, name);
    }
    out
}

/// Can this text be written as a plain scalar without changing meaning?
///
/// This is only about *structural* safety. Whether a plain `42` would be
/// re-read as an integer rather than a string is decided by the node's
/// [`ScalarStyle`], which the emitter preserves.
fn plain_is_safe(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    // Leading or trailing space would be lost.
    if text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace) {
        return false;
    }
    // Control characters and line breaks cannot appear in a plain scalar.
    if text.chars().any(|c| c.is_control()) {
        return false;
    }
    // A leading indicator would be read as structure.
    let first = text.chars().next().unwrap_or(' ');
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        // `-`, `?` and `:` are only indicators when followed by a space.
        let followed_by_space =
            text.len() == 1 || text[first.len_utf8()..].starts_with(char::is_whitespace);
        if !matches!(first, '-' | '?' | ':') || followed_by_space {
            return false;
        }
    }
    // `: ` starts a mapping value and ` #` starts a comment.
    if text.contains(": ") || text.contains(" #") || text.ends_with(':') {
        return false;
    }
    true
}

/// Write a single-quoted scalar, doubling any embedded quote.
fn write_single_quoted(out: &mut String, text: &str) {
    out.push('\'');
    for ch in text.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
}

/// Write a double-quoted scalar with the escapes YAML defines.
fn write_double_quoted(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Render a scalar in the style its node carries.
fn write_scalar(out: &mut String, text: &str, style: ScalarStyle) {
    match style {
        // A plain scalar that would be misread structurally falls back to
        // quoting rather than producing a broken document.
        ScalarStyle::Plain if plain_is_safe(text) => out.push_str(text),
        ScalarStyle::Plain | ScalarStyle::SingleQuoted => {
            if text.chars().any(char::is_control) {
                // Single quotes cannot carry escapes.
                write_double_quoted(out, text);
            } else {
                write_single_quoted(out, text);
            }
        }
        ScalarStyle::DoubleQuoted | ScalarStyle::Literal | ScalarStyle::Folded => {
            write_double_quoted(out, text)
        }
    }
}

struct Emitter<'a> {
    doc: &'a Document,
    options: EmitOptions,
    anchors: HashMap<NodeId, String>,
    out: String,
    /// Nodes on the current path, to detect a cycle rather than recurse
    /// until the stack runs out.
    in_progress: HashSet<NodeId>,
}

impl Emitter<'_> {
    /// The tag to write for a node, shortened against the document's `%TAG`
    /// directives so `tag:stsci.edu:asdf/core/ndarray-1.1.0` comes out as
    /// `!core/ndarray-1.1.0`.
    fn tag_prefix(&self, id: NodeId) -> Option<String> {
        let tag = self.doc.node(id).tag.as_ref()?;
        let full = tag.full();
        for handle in &self.doc.tag_handles {
            if let Some(rest) = full.strip_prefix(&handle.prefix) {
                return Some(format!("{}{}", handle.handle, rest));
            }
        }
        Some(format!("!<{full}>"))
    }

    /// The `&anchor` and `!tag` properties that precede a node's content.
    fn properties(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        if let Some(name) = self.anchors.get(&id) {
            parts.push(format!("&{name}"));
        }
        if let Some(tag) = self.tag_prefix(id) {
            parts.push(tag);
        }
        parts.join(" ")
    }

    /// Would this node emit as a single line?
    fn is_short(&self, id: NodeId) -> bool {
        let resolved = self.doc.resolve(id);
        matches!(self.doc.node(resolved).data, NodeData::Scalar { .. })
            && self.doc.node(resolved).tag.is_none()
            && !self.anchors.contains_key(&resolved)
    }

    /// Try to render a sequence of plain scalars inline.
    fn try_flow_sequence(&self, items: &[NodeId]) -> Option<String> {
        if self.options.flow_seq_max_width == 0 || items.is_empty() {
            return None;
        }
        let mut parts = Vec::with_capacity(items.len());
        for item in items {
            if self.doc.node(*item).is_alias() || !self.is_short(*item) {
                return None;
            }
            let node = self.doc.resolved(*item);
            let NodeData::Scalar { value, style } = &node.data else {
                return None;
            };
            let mut piece = String::new();
            write_scalar(&mut piece, value, *style);
            parts.push(piece);
        }
        let rendered = format!("[{}]", parts.join(", "));
        (rendered.len() <= self.options.flow_seq_max_width).then_some(rendered)
    }

    /// Emit `id` as the value of a mapping key or sequence item.
    ///
    /// `indent` is the column its children start at. `inline` is written
    /// before the value on the same line as its key.
    fn emit_value(&mut self, id: NodeId, indent: usize, inline: bool) -> Result<(), EmitError> {
        // An alias is written as a reference, never expanded.
        if let NodeData::Alias(target) = self.doc.node(id).data {
            let resolved = self.doc.resolve(target);
            let name = self.anchors.get(&resolved).cloned().unwrap_or_default();
            if inline {
                self.out.push(' ');
            }
            let _ = write!(self.out, "*{name}");
            self.out.push('\n');
            return Ok(());
        }

        if !self.in_progress.insert(id) {
            return Err(EmitError::Cycle(id));
        }

        let props = self.properties(id);
        let pad = " ".repeat(indent);

        // Cloning the children keeps the borrow of `doc` from outliving the
        // recursive calls below.
        let data = self.doc.node(id).data.clone();
        match data {
            NodeData::Scalar { value, style } => {
                if inline {
                    self.out.push(' ');
                }
                if !props.is_empty() {
                    let _ = write!(self.out, "{props} ");
                }
                write_scalar(&mut self.out, &value, style);
                self.out.push('\n');
            }

            NodeData::Sequence { items, style } => {
                let flow = if style == CollectionStyle::Block {
                    None
                } else {
                    self.try_flow_sequence(&items)
                };

                if let Some(rendered) = flow {
                    if inline {
                        self.out.push(' ');
                    }
                    if !props.is_empty() {
                        let _ = write!(self.out, "{props} ");
                    }
                    self.out.push_str(&rendered);
                    self.out.push('\n');
                } else if items.is_empty() {
                    if inline {
                        self.out.push(' ');
                    }
                    if !props.is_empty() {
                        let _ = write!(self.out, "{props} ");
                    }
                    self.out.push_str("[]\n");
                } else {
                    if !props.is_empty() {
                        if inline {
                            self.out.push(' ');
                        }
                        self.out.push_str(&props);
                    }
                    if inline || !props.is_empty() {
                        self.out.push('\n');
                    }
                    for item in items {
                        let _ = write!(self.out, "{pad}-");
                        self.emit_value(item, indent + self.options.indent, true)?;
                    }
                }
            }

            NodeData::Mapping { entries, .. } => {
                if entries.is_empty() {
                    if inline {
                        self.out.push(' ');
                    }
                    if !props.is_empty() {
                        let _ = write!(self.out, "{props} ");
                    }
                    self.out.push_str("{}\n");
                } else {
                    if !props.is_empty() {
                        if inline {
                            self.out.push(' ');
                        }
                        self.out.push_str(&props);
                    }
                    if inline || !props.is_empty() {
                        self.out.push('\n');
                    }
                    for entry in entries {
                        let key =
                            self.doc.resolved(entry.key).as_str().unwrap_or_default().to_string();
                        let _ = write!(self.out, "{pad}");
                        // Keys are quoted on the same rules as any scalar.
                        write_scalar(&mut self.out, &key, ScalarStyle::Plain);
                        self.out.push(':');
                        self.emit_value(entry.value, indent + self.options.indent, true)?;
                    }
                }
            }

            NodeData::Alias(_) => unreachable!("handled above"),
        }

        self.in_progress.remove(&id);
        Ok(())
    }
}

/// Emit a document with the default options.
pub fn emit(doc: &Document) -> Result<String, EmitError> {
    emit_with(doc, &EmitOptions::default())
}

/// Emit a document.
pub fn emit_with(doc: &Document, options: &EmitOptions) -> Result<String, EmitError> {
    let root = doc.root().ok_or(EmitError::NoRoot)?;

    let mut emitter = Emitter {
        doc,
        options: options.clone(),
        anchors: collect_anchors(doc),
        out: String::new(),
        in_progress: HashSet::new(),
    };

    if options.directives {
        if let Some(version) = doc.version {
            let _ = writeln!(emitter.out, "%YAML {}.{}", version.major, version.minor);
        }
        for handle in &doc.tag_handles {
            let _ = writeln!(emitter.out, "%TAG {} {}", handle.handle, handle.prefix);
        }
    }

    if options.explicit_start {
        emitter.out.push_str("---");
        // The root's own properties sit on the `---` line, which is how ASDF
        // files carry the `!core/asdf-1.1.0` tag.
        let props = emitter.properties(root);
        if !props.is_empty() {
            let _ = write!(emitter.out, " {props}");
        }
        emitter.out.push('\n');

        // The root's content follows at column zero; its properties are
        // already written, so emit the body only.
        emit_root_body(&mut emitter, root)?;
    } else {
        emitter.emit_value(root, 0, false)?;
    }

    if options.explicit_end {
        emitter.out.push_str("...\n");
    }
    Ok(emitter.out)
}

/// Emit the root's children, its properties having gone on the `---` line.
fn emit_root_body(emitter: &mut Emitter<'_>, root: NodeId) -> Result<(), EmitError> {
    let data = emitter.doc.node(root).data.clone();
    let indent = emitter.options.indent;

    match data {
        NodeData::Mapping { entries, .. } if !entries.is_empty() => {
            for entry in entries {
                let key = emitter.doc.resolved(entry.key).as_str().unwrap_or_default().to_string();
                write_scalar(&mut emitter.out, &key, ScalarStyle::Plain);
                emitter.out.push(':');
                emitter.emit_value(entry.value, indent, true)?;
            }
            Ok(())
        }
        NodeData::Sequence { items, .. } if !items.is_empty() => {
            for item in items {
                emitter.out.push('-');
                emitter.emit_value(item, indent, true)?;
            }
            Ok(())
        }
        NodeData::Mapping { .. } => {
            emitter.out.push_str("{}\n");
            Ok(())
        }
        NodeData::Sequence { .. } => {
            emitter.out.push_str("[]\n");
            Ok(())
        }
        NodeData::Scalar { value, style } => {
            write_scalar(&mut emitter.out, &value, style);
            emitter.out.push('\n');
            Ok(())
        }
        NodeData::Alias(_) => emitter.emit_value(root, 0, false),
    }
}

/// The `%TAG` directive ASDF files conventionally carry.
pub fn asdf_tag_handles() -> Vec<TagHandle> {
    vec![TagHandle::asdf_default()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{CompareOptions, compare};
    use crate::parse::parse_document;

    /// Emit a document and read it back.
    fn round_trip(source: &str) -> (String, Document) {
        let doc = parse_document(source).unwrap();
        let text = emit(&doc).unwrap();
        let reparsed = parse_document(&text)
            .unwrap_or_else(|e| panic!("emitted document does not parse: {e}\n{text}"));
        (text, reparsed)
    }

    /// Assert that a document survives emission unchanged at the value level.
    fn assert_round_trips(source: &str) -> String {
        let original = parse_document(source).unwrap();
        let (text, reparsed) = round_trip(source);
        let result = compare(&original, &reparsed, CompareOptions::default());
        assert!(
            result.is_equal(),
            "round trip changed the document:\n{result}\n--- emitted ---\n{text}"
        );
        text
    }

    #[test]
    fn emits_directives_and_markers() {
        let text = assert_round_trips(
            "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\nfoo: 42\n...\n",
        );
        assert!(text.starts_with("%YAML 1.1\n"), "{text}");
        assert!(text.contains("%TAG ! tag:stsci.edu:asdf/\n"), "{text}");
        assert!(text.contains("--- !core/asdf-1.1.0\n"), "{text}");
        assert!(text.ends_with("...\n"), "{text}");
    }

    #[test]
    fn shortens_tags_against_the_directives() {
        let text = assert_round_trips(
            "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
             data: !core/ndarray-1.1.0\n  source: 0\n...\n",
        );
        // The tag must come back shortened, not as a full URI.
        assert!(text.contains("!core/ndarray-1.1.0"), "{text}");
        assert!(!text.contains("tag:stsci.edu:asdf/core/ndarray"), "{text}");
    }

    #[test]
    fn writes_anchors_and_aliases() {
        // The gap that makes a hand-written emitter necessary: saphyr and
        // yaml-rust2 both drop these.
        let text = assert_round_trips("shared: &a {x: 1}\nother: *a\n");
        assert!(text.contains('&'), "no anchor was written:\n{text}");
        assert!(text.contains('*'), "no alias was written:\n{text}");

        // ...and the alias must still resolve to the same node.
        let reparsed = parse_document(&text).unwrap();
        let root = reparsed.root().unwrap();
        let shared = reparsed.mapping_get(root, "shared").unwrap();
        let other = reparsed.mapping_get(root, "other").unwrap();
        assert!(reparsed.node(other).is_alias());
        assert_eq!(reparsed.resolve(other), shared);
    }

    #[test]
    fn an_anchor_nothing_references_is_not_written() {
        let mut doc = parse_document("a: 1\n").unwrap();
        let root = doc.root().unwrap();
        let value = doc.mapping_get(root, "a").unwrap();
        doc.node_mut(value).anchor = Some("unused".into());

        let text = emit(&doc).unwrap();
        assert!(!text.contains("&unused"), "an unreferenced anchor is noise:\n{text}");
    }

    #[test]
    fn nests_mappings_and_sequences() {
        let text = assert_round_trips("a:\n  b:\n    c: 1\nlist:\n  - x: 1\n  - x: 2\n");
        assert!(text.contains("a:\n"), "{text}");
        assert!(text.contains("  b:\n"), "{text}");
        assert!(text.contains("    c: 1\n"), "{text}");
    }

    #[test]
    fn short_scalar_sequences_go_inline() {
        let text = assert_round_trips("shape: [1024, 1024]\n");
        assert!(text.contains("shape: [1024, 1024]"), "{text}");
    }

    #[test]
    fn long_sequences_break_into_block_style() {
        let mut source = String::from("data: [");
        for i in 0..200 {
            if i > 0 {
                source.push_str(", ");
            }
            let _ = write!(source, "{i}");
        }
        source.push_str("]\n");

        let text = assert_round_trips(&source);
        assert!(
            text.contains("\n  - 0\n"),
            "long sequence should break:\n{}",
            &text[..200.min(text.len())]
        );
    }

    #[test]
    fn quoting_is_preserved_so_types_survive() {
        // The important case: a quoted "42" must stay a string.
        let text = assert_round_trips("a: '42'\nb: 42\nc: 'true'\nd: true\n");
        assert!(text.contains("a: '42'"), "{text}");
        assert!(text.contains("b: 42\n"), "{text}");

        let reparsed = parse_document(&text).unwrap();
        let root = reparsed.root().unwrap();
        let a = reparsed.node(reparsed.mapping_get(root, "a").unwrap());
        assert!(a.scalar_style().unwrap().is_quoted(), "quoting lost");
    }

    #[test]
    fn structurally_unsafe_plain_scalars_are_quoted() {
        // Each of these would be misread if written plain.
        for value in [
            "",
            " leading",
            "trailing ",
            "has: colon",
            "- dash",
            "#hash",
            "[bracket",
            "{brace",
            "&anchor",
            "*alias",
            "!tag",
            "ends:",
        ] {
            let mut doc = Document::new_asdf();
            let k = doc.add_scalar("k");
            let v = doc.add_scalar(value);
            let root = doc.add_mapping(vec![(k, v)]);
            doc.set_root(root);

            let text = emit(&doc).unwrap();
            let reparsed = parse_document(&text)
                .unwrap_or_else(|e| panic!("{value:?} produced unparseable output: {e}\n{text}"));
            let back = reparsed
                .mapping_get(reparsed.root().unwrap(), "k")
                .and_then(|id| reparsed.resolved(id).as_str().map(str::to_string));
            assert_eq!(back.as_deref(), Some(value), "{value:?} did not survive:\n{text}");
        }
    }

    #[test]
    fn control_characters_survive_via_double_quoting() {
        let mut doc = Document::new_asdf();
        let k = doc.add_scalar("k");
        let v = doc.add_scalar("line one\nline two\ttabbed");
        let root = doc.add_mapping(vec![(k, v)]);
        doc.set_root(root);

        let text = emit(&doc).unwrap();
        let reparsed = parse_document(&text).unwrap();
        let back = reparsed
            .mapping_get(reparsed.root().unwrap(), "k")
            .and_then(|id| reparsed.resolved(id).as_str().map(str::to_string));
        assert_eq!(back.as_deref(), Some("line one\nline two\ttabbed"));
    }

    #[test]
    fn single_quotes_are_doubled() {
        let mut doc = Document::new_asdf();
        let k = doc.add_scalar("k");
        let v = doc.add_scalar_styled("it's", ScalarStyle::SingleQuoted);
        let root = doc.add_mapping(vec![(k, v)]);
        doc.set_root(root);

        let text = emit(&doc).unwrap();
        assert!(text.contains("'it''s'"), "{text}");
        let reparsed = parse_document(&text).unwrap();
        let back = reparsed
            .mapping_get(reparsed.root().unwrap(), "k")
            .and_then(|id| reparsed.resolved(id).as_str().map(str::to_string));
        assert_eq!(back.as_deref(), Some("it's"));
    }

    #[test]
    fn empty_containers_use_flow_form() {
        let text = assert_round_trips("a: {}\nb: []\n");
        assert!(text.contains("a: {}"), "{text}");
        assert!(text.contains("b: []"), "{text}");
    }

    #[test]
    fn a_document_with_no_root_is_an_error() {
        let doc = Document::new();
        assert!(matches!(emit(&doc), Err(EmitError::NoRoot)));
    }

    #[test]
    fn unicode_survives() {
        let text = assert_round_trips("greeting: héllo wörld\nemoji: 🔭\n");
        assert!(text.contains('🔭'), "{text}");
    }

    #[test]
    fn deeply_nested_documents_round_trip() {
        let mut source = String::new();
        for depth in 0..20 {
            let _ = writeln!(source, "{}k{depth}:", "  ".repeat(depth));
        }
        let _ = writeln!(source, "{}leaf", "  ".repeat(20));
        assert_round_trips(&source);
    }

    #[test]
    fn plain_safety_rules() {
        assert!(plain_is_safe("hello"));
        assert!(plain_is_safe("42"));
        assert!(plain_is_safe("a-b"));
        // `-` alone or followed by a space is a sequence indicator.
        assert!(!plain_is_safe("- x"));
        assert!(!plain_is_safe(""));
        assert!(!plain_is_safe(" x"));
        assert!(!plain_is_safe("x "));
        assert!(!plain_is_safe("a: b"));
        assert!(!plain_is_safe("a #b"));
        assert!(!plain_is_safe("ends:"));
        assert!(!plain_is_safe("with\nnewline"));
    }
}
