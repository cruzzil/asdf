//! Nodes in an ASDF YAML document.

use crate::tag::Tag;

/// An index into a [`Document`](crate::Document)'s node arena.
///
/// Nodes are addressed by index rather than by reference so that aliases can
/// genuinely share a target, and so that a handle to a node (which the C API
/// exposes as `asdf_value_t`) stays valid while the document is mutated.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    /// The raw arena index.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// How a scalar was written in the source document.
///
/// This is preserved because ASDF's type resolution depends on it: a quoted
/// `"123"` is a string, an unquoted `123` is an integer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ScalarStyle {
    /// Unquoted.
    #[default]
    Plain,
    /// Wrapped in `'`.
    SingleQuoted,
    /// Wrapped in `"`.
    DoubleQuoted,
    /// A `|` block scalar.
    Literal,
    /// A `>` block scalar.
    Folded,
}

impl ScalarStyle {
    /// Whether this style forces the scalar to be a string regardless of
    /// its content.
    ///
    /// Mirrors libasdf: a scalar is a string if it is explicitly quoted, or
    /// uses a literal or folded representation.
    pub fn is_quoted(self) -> bool {
        !matches!(self, ScalarStyle::Plain)
    }
}

/// Whether a collection was written inline or as an indented block.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CollectionStyle {
    /// Let the emitter choose.
    #[default]
    Auto,
    /// `{...}` / `[...]`.
    Flow,
    /// Indented block notation.
    Block,
}

/// A key/value pair in a mapping.
///
/// Keys are nodes rather than strings so that complex keys round-trip, even
/// though ASDF trees use string keys almost exclusively.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The key node.
    pub key: NodeId,
    /// The value node.
    pub value: NodeId,
}

/// The payload of a node.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeData {
    /// A scalar, kept in its source representation. Type resolution happens on
    /// demand rather than at parse time, so that the raw text is never lost.
    Scalar {
        /// The scalar text, with escapes already processed by the parser.
        value: String,
        /// How it was written.
        style: ScalarStyle,
    },
    /// An ordered list of nodes.
    Sequence {
        /// The items.
        items: Vec<NodeId>,
        /// How it was written.
        style: CollectionStyle,
    },
    /// An ordered list of key/value pairs.
    Mapping {
        /// The entries, in document order.
        entries: Vec<Entry>,
        /// How it was written.
        style: CollectionStyle,
    },
    /// A reference to an anchored node elsewhere in the document.
    ///
    /// Kept distinct from its target so that the alias survives a round trip
    /// as `*anchor` rather than being expanded into a copy.
    Alias(NodeId),
}

/// A node in the document tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// The node's explicit tag, if it carried one.
    pub tag: Option<Tag>,
    /// The anchor name this node was defined under, if any.
    pub anchor: Option<String>,
    /// The node's payload.
    pub data: NodeData,
    /// Byte range in the source document, when the node was parsed rather
    /// than constructed.
    pub span: Option<Span>,
}

/// A byte range in the source document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset one past the last byte.
    pub end: usize,
}

impl Node {
    /// A node with the given payload and nothing else set.
    pub fn new(data: NodeData) -> Self {
        Self { tag: None, anchor: None, data, span: None }
    }

    /// A plain scalar node.
    pub fn scalar(value: impl Into<String>) -> Self {
        Self::new(NodeData::Scalar { value: value.into(), style: ScalarStyle::Plain })
    }

    /// A scalar node with an explicit style.
    pub fn scalar_styled(value: impl Into<String>, style: ScalarStyle) -> Self {
        Self::new(NodeData::Scalar { value: value.into(), style })
    }

    /// An empty sequence node.
    pub fn sequence() -> Self {
        Self::new(NodeData::Sequence { items: Vec::new(), style: CollectionStyle::Auto })
    }

    /// An empty mapping node.
    pub fn mapping() -> Self {
        Self::new(NodeData::Mapping { entries: Vec::new(), style: CollectionStyle::Auto })
    }

    /// Attach a tag, builder-style.
    pub fn with_tag(mut self, tag: Tag) -> Self {
        self.tag = Some(tag);
        self
    }

    /// Whether this node is a scalar.
    pub fn is_scalar(&self) -> bool {
        matches!(self.data, NodeData::Scalar { .. })
    }

    /// Whether this node is a sequence.
    pub fn is_sequence(&self) -> bool {
        matches!(self.data, NodeData::Sequence { .. })
    }

    /// Whether this node is a mapping.
    pub fn is_mapping(&self) -> bool {
        matches!(self.data, NodeData::Mapping { .. })
    }

    /// Whether this node is an alias to another node.
    pub fn is_alias(&self) -> bool {
        matches!(self.data, NodeData::Alias(_))
    }

    /// The scalar text, if this is a scalar.
    pub fn as_str(&self) -> Option<&str> {
        match &self.data {
            NodeData::Scalar { value, .. } => Some(value),
            _ => None,
        }
    }

    /// The scalar style, if this is a scalar.
    pub fn scalar_style(&self) -> Option<ScalarStyle> {
        match &self.data {
            NodeData::Scalar { style, .. } => Some(*style),
            _ => None,
        }
    }
}
