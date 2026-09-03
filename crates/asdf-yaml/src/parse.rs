//! Building a [`Document`] from `saphyr-parser`'s event stream.
//!
//! We consume events rather than `saphyr`'s own `Yaml` tree because the tree
//! type flattens the anchor graph -- its `Yaml::Alias` is documented as "not
//! fully supported" -- while ASDF needs aliases to stay distinct from their
//! targets. The event stream carries anchor ids and tags, which is everything
//! required to rebuild the graph faithfully.

use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScanError, Span as SapSpan, SpannedEventReceiver};

use crate::document::{Document, YamlVersion};
use crate::node::{CollectionStyle, Entry, Node, NodeData, NodeId, ScalarStyle, Span};
use crate::tag::{Tag, TagHandle};

/// An error encountered while parsing a YAML document.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The underlying YAML scanner rejected the input.
    #[error("YAML parse error: {0}")]
    Scan(#[from] ScanError),
    /// The document was empty where a document was required.
    #[error("no YAML document found")]
    Empty,
    /// The event stream was not well-formed. Reaching this indicates a bug
    /// rather than bad input, since the scanner validates structure first.
    #[error("malformed YAML event stream: {0}")]
    Malformed(&'static str),
}

/// What the builder is part-way through assembling.
enum Frame {
    Sequence { items: Vec<NodeId>, node: NodeId, start: usize },
    Mapping { entries: Vec<Entry>, pending_key: Option<NodeId>, node: NodeId, start: usize },
}

#[derive(Default)]
struct Builder {
    doc: Document,
    stack: Vec<Frame>,
    /// Maps a parser anchor id to the node that defined it.
    anchors: HashMap<usize, NodeId>,
    /// Anchor ids seen, in first-seen order, so we can name them on emit.
    anchor_order: Vec<usize>,
    root: Option<NodeId>,
    done: bool,
    error: Option<ParseError>,
}

fn convert_style(style: saphyr_parser::ScalarStyle) -> ScalarStyle {
    use saphyr_parser::ScalarStyle as S;
    match style {
        S::Plain => ScalarStyle::Plain,
        S::SingleQuoted => ScalarStyle::SingleQuoted,
        S::DoubleQuoted => ScalarStyle::DoubleQuoted,
        S::Literal => ScalarStyle::Literal,
        S::Folded => ScalarStyle::Folded,
    }
}

fn convert_span(span: SapSpan) -> Span {
    Span { start: span.start.index(), end: span.end.index() }
}

impl Builder {
    /// Record a completed node into whatever container is currently open.
    fn place(&mut self, id: NodeId) {
        match self.stack.last_mut() {
            None => {
                if self.root.is_none() {
                    self.root = Some(id);
                }
            }
            Some(Frame::Sequence { items, .. }) => items.push(id),
            Some(Frame::Mapping { entries, pending_key, .. }) => match pending_key.take() {
                None => *pending_key = Some(id),
                Some(key) => entries.push(Entry { key, value: id }),
            },
        }
    }

    /// Register an anchor definition, if the event carried one.
    ///
    /// Anchor id 0 is the parser's "no anchor" sentinel.
    fn note_anchor(&mut self, anchor_id: usize, node: NodeId) {
        if anchor_id == 0 {
            return;
        }
        if self.anchors.insert(anchor_id, node).is_none() {
            self.anchor_order.push(anchor_id);
        }
        // Name the anchor positionally; the source name is not surfaced by the
        // parser, and ASDF assigns no meaning to anchor names.
        let position = self
            .anchor_order
            .iter()
            .position(|a| *a == anchor_id)
            .unwrap_or(0);
        self.doc.node_mut(node).anchor = Some(format!("anc{}", position));
    }

    fn open(&mut self, frame: Frame) {
        self.stack.push(frame);
    }

    fn close(&mut self) -> Result<(), ParseError> {
        let frame = self
            .stack
            .pop()
            .ok_or(ParseError::Malformed("collection end without a matching start"))?;

        let (node, data, start) = match frame {
            Frame::Sequence { items, node, start } => (
                node,
                NodeData::Sequence { items, style: CollectionStyle::Auto },
                start,
            ),
            Frame::Mapping { entries, pending_key, node, start } => {
                if pending_key.is_some() {
                    return Err(ParseError::Malformed("mapping ended with a dangling key"));
                }
                (
                    node,
                    NodeData::Mapping { entries, style: CollectionStyle::Auto },
                    start,
                )
            }
        };

        let n = self.doc.node_mut(node);
        n.data = data;
        if let Some(span) = n.span.as_mut() {
            span.start = start;
        }
        self.place(node);
        Ok(())
    }

    fn handle(&mut self, ev: Event<'_>, span: SapSpan) -> Result<(), ParseError> {
        // Only the first document in the stream is retained; ASDF permits
        // exactly one, and a block index is parsed separately.
        if self.done {
            return Ok(());
        }

        match ev {
            Event::StreamStart | Event::StreamEnd | Event::Nothing => {}
            Event::DocumentStart(_) => {}
            Event::DocumentEnd => {
                if self.root.is_some() {
                    self.done = true;
                }
            }

            Event::Scalar(value, style, anchor_id, tag) => {
                let mut node = Node::scalar_styled(value.into_owned(), convert_style(style));
                node.tag = tag.map(|t| Tag::new(t.handle.clone(), t.suffix.clone()));
                node.span = Some(convert_span(span));
                let id = self.doc.add(node);
                self.note_anchor(anchor_id, id);
                self.place(id);
            }

            Event::Alias(anchor_id) => {
                let target = self.anchors.get(&anchor_id).copied().ok_or(
                    ParseError::Malformed("alias refers to an anchor that was never defined"),
                )?;
                let mut node = Node::new(NodeData::Alias(target));
                node.span = Some(convert_span(span));
                let id = self.doc.add(node);
                self.place(id);
            }

            Event::SequenceStart(anchor_id, tag) => {
                // The node is allocated up front so that an alias appearing
                // inside it can already refer to it.
                let mut node = Node::sequence();
                node.tag = tag.map(|t| Tag::new(t.handle.clone(), t.suffix.clone()));
                node.span = Some(convert_span(span));
                let id = self.doc.add(node);
                self.note_anchor(anchor_id, id);
                self.open(Frame::Sequence {
                    items: Vec::new(),
                    node: id,
                    start: span.start.index(),
                });
            }

            Event::MappingStart(anchor_id, tag) => {
                let mut node = Node::mapping();
                node.tag = tag.map(|t| Tag::new(t.handle.clone(), t.suffix.clone()));
                node.span = Some(convert_span(span));
                let id = self.doc.add(node);
                self.note_anchor(anchor_id, id);
                self.open(Frame::Mapping {
                    entries: Vec::new(),
                    pending_key: None,
                    node: id,
                    start: span.start.index(),
                });
            }

            Event::SequenceEnd | Event::MappingEnd => self.close()?,
        }
        Ok(())
    }
}

impl<'i> SpannedEventReceiver<'i> for Builder {
    fn on_event(&mut self, ev: Event<'i>, span: SapSpan) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = self.handle(ev, span) {
            self.error = Some(e);
        }
    }
}

/// Read the `%YAML` and `%TAG` directives from the head of a document.
///
/// `saphyr-parser` consumes directives internally and surfaces no event for
/// them -- it hands back tags with their handle already expanded -- so they
/// are scanned from the source instead. Without this the emitter could not
/// reproduce the `%TAG ! tag:stsci.edu:asdf/` line that lets an ASDF tree
/// write `!core/ndarray-1.1.0` rather than the full URI.
fn scan_directives(input: &str, doc: &mut Document) {
    for line in input.lines() {
        let line = line.trim_end();
        if line.starts_with("---") || line.starts_with("...") {
            break;
        }
        if let Some(rest) = line.strip_prefix("%YAML ") {
            let mut parts = rest.trim().split('.');
            if let (Some(major), Some(minor)) = (parts.next(), parts.next())
                && let (Ok(major), Ok(minor)) = (major.parse(), minor.parse())
            {
                doc.version = Some(YamlVersion { major, minor });
            }
        } else if let Some(rest) = line.strip_prefix("%TAG ") {
            let mut parts = rest.split_whitespace();
            if let (Some(handle), Some(prefix)) = (parts.next(), parts.next()) {
                doc.tag_handles.push(TagHandle {
                    handle: handle.to_string(),
                    prefix: prefix.to_string(),
                });
            }
        }
    }
}

/// Parse a YAML document into a [`Document`].
///
/// Only the first document in the stream is returned. Anchors and aliases are
/// preserved as distinct nodes rather than being expanded, and the `%YAML`
/// and `%TAG` directives are recorded so the document can be re-emitted with
/// them intact.
pub fn parse_document(input: &str) -> Result<Document, ParseError> {
    let mut builder = Builder::default();
    let parse_result = Parser::new_from_str(input).load(&mut builder, true);

    if let Some(err) = builder.error {
        return Err(err);
    }
    parse_result?;

    let root = builder.root.ok_or(ParseError::Empty)?;
    let mut doc = builder.doc;
    doc.set_root(root);
    scan_directives(input, &mut doc);
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag::ASDF_STANDARD_TAG_PREFIX;

    #[test]
    fn parses_a_minimal_asdf_tree() {
        let doc = parse_document(
            "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\nfoo: 42\n...\n",
        )
        .unwrap();

        let root = doc.root().unwrap();
        assert!(doc.node(root).is_mapping());

        let tag = doc.tag_of(root).unwrap();
        assert_eq!(tag.handle(), ASDF_STANDARD_TAG_PREFIX);
        assert_eq!(tag.suffix(), "core/asdf-1.1.0");

        let foo = doc.mapping_get(root, "foo").unwrap();
        assert_eq!(doc.node(foo).as_str(), Some("42"));
    }

    #[test]
    fn preserves_scalar_style() {
        let doc = parse_document("a: yes\nb: \"yes\"\nc: 'yes'\n").unwrap();
        let root = doc.root().unwrap();

        let a = doc.node(doc.mapping_get(root, "a").unwrap());
        let b = doc.node(doc.mapping_get(root, "b").unwrap());
        let c = doc.node(doc.mapping_get(root, "c").unwrap());

        assert_eq!(a.scalar_style(), Some(ScalarStyle::Plain));
        assert_eq!(b.scalar_style(), Some(ScalarStyle::DoubleQuoted));
        assert_eq!(c.scalar_style(), Some(ScalarStyle::SingleQuoted));
        // all three carry the same text; only the style distinguishes them
        assert_eq!(a.as_str(), Some("yes"));
        assert_eq!(b.as_str(), Some("yes"));
    }

    #[test]
    fn aliases_share_their_target() {
        let doc = parse_document("shared: &a {x: 1}\nother: *a\n").unwrap();
        let root = doc.root().unwrap();

        let shared = doc.mapping_get(root, "shared").unwrap();
        let other = doc.mapping_get(root, "other").unwrap();

        assert!(!doc.node(shared).is_alias());
        assert!(doc.node(other).is_alias(), "alias must stay distinct from its target");
        assert_eq!(doc.resolve(other), shared, "alias must resolve to the same node");
        assert!(doc.node(shared).anchor.is_some());

        // reading through the alias sees the shared value
        let x = doc.mapping_get(other, "x").unwrap();
        assert_eq!(doc.node(x).as_str(), Some("1"));
    }

    #[test]
    fn alias_to_a_sequence_element() {
        let doc = parse_document("- &v 7\n- *v\n").unwrap();
        let root = doc.root().unwrap();
        let items = doc.sequence_items(root).unwrap().to_vec();
        assert_eq!(items.len(), 2);
        assert!(doc.node(items[1]).is_alias());
        assert_eq!(doc.resolved(items[1]).as_str(), Some("7"));
    }

    #[test]
    fn nested_tags_are_kept() {
        let doc = parse_document(
            "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
             data: !core/ndarray-1.1.0\n  source: 0\n  shape: [8]\n...\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let data = doc.mapping_get(root, "data").unwrap();
        assert_eq!(
            doc.tag_of(data).unwrap().full(),
            "tag:stsci.edu:asdf/core/ndarray-1.1.0"
        );
        let shape = doc.mapping_get(data, "shape").unwrap();
        assert_eq!(doc.container_len(shape), Some(1));
    }

    #[test]
    fn empty_input_is_an_error() {
        assert!(matches!(parse_document(""), Err(ParseError::Empty)));
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        assert!(parse_document("a: [1, 2\nb: 3\n").is_err());
    }

    #[test]
    fn spans_locate_nodes_in_the_source() {
        let src = "foo: 42\n";
        let doc = parse_document(src).unwrap();
        let root = doc.root().unwrap();
        let foo = doc.mapping_get(root, "foo").unwrap();
        let span = doc.node(foo).span.unwrap();
        assert_eq!(&src[span.start..span.end], "42");
    }
}
