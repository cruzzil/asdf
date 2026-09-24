//! The `core/ndarray` schema.
//!
//! An array's data lives in one of three places, distinguished by the schema's
//! `source` and `data` keys:
//!
//! - an **internal block**, `source: 0` naming a block by index (negative
//!   indices count back from the last block);
//! - an **external file**, `source: "other.asdf"`, used for exploded form;
//! - **inline** in the tree, under `data`, as nested sequences.

use asdf_yaml::{Document, NodeData, NodeId};

use crate::core::datatype::{ByteOrder, Datatype, ScalarType, parse_shape_with_star};
use crate::error::{Result, err};

/// Where an array's data comes from.
#[derive(Clone, PartialEq, Debug)]
pub enum Source {
    /// A binary block in this file, by index.
    Block(usize),
    /// The last block in this file, written as `source: -1`.
    ///
    /// Kept distinct from a resolved index because a streamed array is
    /// written this way before the block count is known.
    LastBlock,
    /// The first block of another ASDF file, named by URI.
    External(String),
    /// Nested sequences in the tree itself.
    Inline(NodeId),
}

/// What the values in an inline array look like, so a type can be chosen.
#[derive(Default, Debug)]
struct InlineTypes {
    has_string: bool,
    has_float: bool,
    has_signed: bool,
    int_min: i64,
    uint_max: u64,
}

/// The narrowest scalar type that holds every value of an inline array.
///
/// Inline data may carry no `datatype`, in which case the type is whatever
/// the values need: a float if any is fractional, a signed integer if any is
/// negative, and the smallest width that fits otherwise. Strings are not
/// supported inline, and an array of nothing but booleans is `bool8`.
pub fn infer_inline_datatype(doc: &Document, node: NodeId) -> ScalarType {
    let mut seen = InlineTypes::default();
    survey_inline(doc, node, &mut seen);

    if seen.has_string {
        return ScalarType::Unknown;
    }
    if seen.has_float {
        return ScalarType::Float64;
    }
    if !seen.has_signed && seen.uint_max == 0 && seen.int_min == 0 {
        // Nothing numeric at all: the values were booleans or nulls.
        return ScalarType::Bool8;
    }
    if seen.has_signed {
        if seen.int_min >= i64::from(i8::MIN) && seen.uint_max <= i8::MAX as u64 {
            return ScalarType::Int8;
        }
        if seen.int_min >= i64::from(i16::MIN) && seen.uint_max <= i16::MAX as u64 {
            return ScalarType::Int16;
        }
        if seen.int_min >= i64::from(i32::MIN) && seen.uint_max <= i32::MAX as u64 {
            return ScalarType::Int32;
        }
        return ScalarType::Int64;
    }
    if seen.uint_max <= u64::from(u8::MAX) {
        ScalarType::Uint8
    } else if seen.uint_max <= u64::from(u16::MAX) {
        ScalarType::Uint16
    } else if seen.uint_max <= u64::from(u32::MAX) {
        ScalarType::Uint32
    } else {
        ScalarType::Uint64
    }
}

/// Walk an inline array's values, recording what types they need.
fn survey_inline(doc: &Document, node: NodeId, seen: &mut InlineTypes) {
    survey_inline_bounded(doc, node, seen, 0, &mut inline_budget(doc));
}

/// The deepest nesting any of the inline walkers will follow.
///
/// Well past the 32 dimensions numpy allows, and far short of what it takes
/// to overflow a stack.
const MAX_INLINE_DEPTH: usize = 64;

/// How many nodes an inline walk may visit before giving up.
///
/// A YAML alias lets one node stand in for a whole subtree, so the number of
/// nodes a walk *visits* is not bounded by the number the document *holds*:
/// ten anchors each aliasing the one before, ten ways, is 10^10 visits from a
/// few hundred bytes. Real inline data has one node per element, so allowing
/// a generous multiple of the document's size costs a legitimate file
/// nothing and stops that cold.
fn inline_budget(doc: &Document) -> u64 {
    (doc.node_count() as u64).saturating_mul(8).max(1024)
}

fn survey_inline_bounded(
    doc: &Document,
    node: NodeId,
    seen: &mut InlineTypes,
    depth: usize,
    budget: &mut u64,
) {
    if depth > MAX_INLINE_DEPTH || *budget == 0 {
        return;
    }
    *budget -= 1;

    let resolved = doc.resolve(node);
    if let Some(items) = doc.sequence_items(resolved).map(<[_]>::to_vec) {
        for item in items {
            survey_inline_bounded(doc, item, seen, depth + 1, budget);
        }
        return;
    }

    let Some(text) = doc.resolved(resolved).as_str() else {
        return;
    };
    let style = match &doc.resolved(resolved).data {
        NodeData::Scalar { style, .. } => *style,
        _ => return,
    };

    match asdf_yaml::resolve(text, style, asdf_yaml::Schema::Libasdf) {
        asdf_yaml::Resolved::Uint(v, _) => seen.uint_max = seen.uint_max.max(v),
        asdf_yaml::Resolved::Int(v, _) => {
            seen.has_signed = true;
            seen.int_min = seen.int_min.min(v);
            if v > 0 {
                seen.uint_max = seen.uint_max.max(v as u64);
            }
        }
        asdf_yaml::Resolved::Double(_) => seen.has_float = true,
        asdf_yaml::Resolved::String => seen.has_string = true,
        _ => {}
    }
}

/// How missing values are marked.
#[derive(Clone, PartialEq, Debug)]
pub enum Mask {
    /// A sentinel value; elements equal to it are missing.
    Value(String),
    /// Another array of the same shape, non-zero where this array is missing.
    Array(NodeId),
}

/// A parsed `core/ndarray`.
#[derive(Clone, PartialEq, Debug)]
pub struct Ndarray {
    /// Where the data lives.
    pub source: Source,
    /// The array's shape. A leading `None` means the dimension is determined
    /// from the block's size, which the schema allows for streamed arrays.
    pub shape: Vec<Option<u64>>,
    /// The element type.
    pub datatype: Datatype,
    /// Byte order of the elements.
    pub byteorder: ByteOrder,
    /// Offset in bytes into the block where the data starts.
    pub offset: u64,
    /// Bytes to step per dimension. Absent means C-contiguous.
    pub strides: Option<Vec<i64>>,
    /// How missing values are marked, if at all.
    pub mask: Option<Mask>,
}

impl Ndarray {
    /// Parse an ndarray from a tree node.
    pub fn parse(doc: &Document, id: NodeId) -> Result<Self> {
        let node = doc.resolved(id);

        // The schema's shorthand: the whole tagged value is the nested data.
        if matches!(node.data, NodeData::Sequence { .. }) {
            let data = doc.resolve(id);
            return Ok(Ndarray {
                source: Source::Inline(data),
                shape: infer_inline_shape(doc, data),
                // With no `datatype` key there is nothing to state one, so
                // it is read off the values.
                datatype: Datatype::scalar(infer_inline_datatype(doc, data)),
                byteorder: ByteOrder::Default,
                offset: 0,
                strides: None,
                mask: None,
            });
        }

        if !matches!(node.data, NodeData::Mapping { .. }) {
            return Err(err!(InvalidArgument, "ndarray must be a mapping or a sequence"));
        }

        let source = match (doc.mapping_get(id, "source"), doc.mapping_get(id, "data")) {
            (Some(src), _) => parse_source(doc, src)?,
            (None, Some(data)) => Source::Inline(doc.resolve(data)),
            (None, None) => {
                return Err(err!(
                    InvalidArgument,
                    "ndarray has neither a 'source' nor a 'data' key"
                ));
            }
        };

        let shape = match doc.mapping_get(id, "shape") {
            Some(s) => parse_shape_with_star(doc, s)?,
            None => match &source {
                // Inline data carries its shape implicitly.
                Source::Inline(node) => infer_inline_shape(doc, *node),
                _ => Vec::new(),
            },
        };

        let datatype = match doc.mapping_get(id, "datatype") {
            Some(d) => Datatype::parse(doc, d)?,
            // Inline data with no declared type is read off the values, as
            // the shorthand above is.
            None => match &source {
                Source::Inline(node) => Datatype::scalar(infer_inline_datatype(doc, *node)),
                _ => Datatype::default(),
            },
        };

        let byteorder = doc
            .mapping_get(id, "byteorder")
            .and_then(|b| doc.resolved(b).as_str().map(ByteOrder::from_name))
            .unwrap_or(ByteOrder::Default);

        let offset = doc
            .mapping_get(id, "offset")
            .and_then(|o| doc.resolved(o).as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);

        let strides = match doc.mapping_get(id, "strides") {
            None => None,
            Some(s) => {
                let items = doc
                    .sequence_items(s)
                    .ok_or_else(|| err!(InvalidArgument, "strides must be a sequence"))?;
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let text = doc
                        .resolved(*item)
                        .as_str()
                        .ok_or_else(|| err!(InvalidArgument, "stride entry is not a scalar"))?;
                    out.push(text.parse::<i64>().map_err(|_| {
                        err!(InvalidArgument, "stride entry is not an integer: {text}")
                    })?);
                }
                Some(out)
            }
        };

        let mask = doc.mapping_get(id, "mask").map(|m| {
            let n = doc.resolved(m);
            match n.data {
                NodeData::Mapping { .. } | NodeData::Sequence { .. } => Mask::Array(doc.resolve(m)),
                _ => Mask::Value(n.as_str().unwrap_or_default().to_string()),
            }
        });

        Ok(Ndarray { source, shape, datatype, byteorder, offset, strides, mask })
    }

    /// The shape with every dimension known, given the block's byte length.
    ///
    /// A streamed array's first dimension is `*` in the file and is derived
    /// from how many whole rows the block holds.
    #[deny(clippy::arithmetic_side_effects)]
    pub fn resolved_shape(&self, block_bytes: Option<u64>) -> Result<Vec<u64>> {
        let item = self.datatype.item_size();
        let mut out = Vec::with_capacity(self.shape.len());

        for (idx, dim) in self.shape.iter().enumerate() {
            match dim {
                Some(d) => out.push(*d),
                None => {
                    let bytes = block_bytes.ok_or_else(|| {
                        err!(
                            InvalidArgument,
                            "shape dimension {idx} is '*' but no block size is available"
                        )
                    })?;
                    // Checked, and not by the lint on this function: that
                    // sees operators, and `.product()` is a method call, so
                    // an unchecked product sails straight past it. A fuzz
                    // target found this one within minutes of existing.
                    #[allow(
                        clippy::arithmetic_side_effects,
                        reason = "idx indexes self.shape, so idx + 1 is at most its length"
                    )]
                    let tail = &self.shape[idx + 1..];
                    let mut row: u64 = 1;
                    for d in tail {
                        row = row.checked_mul(d.unwrap_or(1)).ok_or_else(|| {
                            err!(
                                OverLimit,
                                "shape {:?} describes a row too large to size a '*' dimension \
                                 against",
                                self.shape
                            )
                        })?;
                    }
                    let row = row.max(1);
                    let row_bytes = row.checked_mul(item).filter(|b| *b != 0).ok_or_else(|| {
                        err!(InvalidArgument, "cannot size a '*' dimension with a zero-width row")
                    })?;
                    #[allow(
                        clippy::arithmetic_side_effects,
                        reason = "row_bytes was filtered non-zero just above"
                    )]
                    out.push(bytes / row_bytes);
                }
            }
        }
        Ok(out)
    }

    /// The number of elements, for a fully-known shape.
    ///
    /// The shape comes from the tree, so the product is checked: a crafted
    /// shape can otherwise wrap to a small number and make a later read
    /// address the wrong bytes, or wrap past a size check into an
    /// allocation nothing justifies.
    pub fn len(&self, block_bytes: Option<u64>) -> Result<u64> {
        element_count(&self.resolved_shape(block_bytes)?)
    }

    /// Whether the array has no elements.
    pub fn is_empty(&self, block_bytes: Option<u64>) -> Result<bool> {
        Ok(self.len(block_bytes)? == 0)
    }

    /// The number of bytes the elements occupy.
    pub fn nbytes(&self, block_bytes: Option<u64>) -> Result<u64> {
        self.len(block_bytes)?
            .checked_mul(self.datatype.item_size())
            .ok_or_else(|| err!(OverLimit, "array's size in bytes does not fit in 64 bits"))
    }

    /// C-contiguous strides for a shape, in bytes.
    ///
    /// `None` when the shape is too large to stride, rather than a wrapped
    /// value: a wrapped stride silently addresses the wrong element, which
    /// for a data format is worse than refusing to read.
    #[deny(clippy::arithmetic_side_effects)]
    pub fn c_strides(shape: &[u64], item_size: u64) -> Option<Vec<i64>> {
        let mut strides = vec![0i64; shape.len()];
        let mut acc = i64::try_from(item_size).ok()?;
        for idx in (0..shape.len()).rev() {
            strides[idx] = acc;
            acc = acc.checked_mul(i64::try_from(shape[idx]).ok()?)?;
        }
        Some(strides)
    }
}

/// Parse the `source` key, which is either a block index or a URI.
fn parse_source(doc: &Document, id: NodeId) -> Result<Source> {
    let node = doc.resolved(id);
    let text =
        node.as_str().ok_or_else(|| err!(InvalidArgument, "ndarray source must be a scalar"))?;

    // A quoted scalar is always a URI, even if it looks numeric.
    let quoted = node.scalar_style().is_some_and(|s| s.is_quoted());
    if !quoted && let Ok(index) = text.parse::<i64>() {
        return Ok(if index == -1 {
            Source::LastBlock
        } else if index < 0 {
            // Other negative indices count back from the end; resolving them
            // needs the block count, so they are rejected here rather than
            // guessed at.
            return Err(err!(
                InvalidArgument,
                "negative ndarray source {index} other than -1 is not supported"
            ));
        } else {
            Source::Block(index as usize)
        });
    }
    Ok(Source::External(text.to_string()))
}

/// The number of elements a shape describes, refusing to wrap.
///
/// Every dimension is a `uint64` read straight out of the tree, so the
/// product is attacker-controlled. `[1 << 61, 8]` wraps to zero and
/// `[(1 << 63) + 1, 2]` wraps to two -- either of which walks straight past
/// a "does the block hold this much?" check that is done in the same
/// arithmetic.
#[deny(clippy::arithmetic_side_effects)]
pub fn element_count(shape: &[u64]) -> Result<u64> {
    let mut count: u64 = 1;
    for dim in shape {
        count = count.checked_mul(*dim).ok_or_else(|| {
            err!(OverLimit, "shape {shape:?} has more elements than 64 bits hold")
        })?;
    }
    Ok(count)
}

/// Work out the shape of nested inline sequences.
fn infer_inline_shape(doc: &Document, id: NodeId) -> Vec<Option<u64>> {
    let mut shape = Vec::new();
    let mut current = id;
    // Follow the first element down; a ragged array is not valid ASDF, so the
    // first branch describes the whole.
    //
    // `a: &a [*a]` makes that descent a cycle -- the first element resolves
    // back to the sequence it came from -- so the walk is bounded as well as
    // followed. Without the bound it pushes a dimension per iteration and
    // never returns.
    while let Some(items) = doc.sequence_items(current) {
        if shape.len() >= MAX_INLINE_DEPTH {
            break;
        }
        shape.push(Some(items.len() as u64));
        match items.first() {
            Some(first) => {
                let next = doc.resolve(*first);
                if next == current {
                    break;
                }
                current = next;
            }
            None => break,
        }
    }
    shape
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inline data with no `datatype` takes the narrowest type that holds
    /// every value, which is what libasdf and Python asdf both do.
    #[test]
    fn an_inline_arrays_datatype_is_inferred_from_its_values() {
        let cases = [
            ("[[0, 1, 2], [3, 4, 5]]", ScalarType::Uint8),
            ("[0, 255]", ScalarType::Uint8),
            ("[0, 256]", ScalarType::Uint16),
            ("[0, 70000]", ScalarType::Uint32),
            ("[0, 5000000000]", ScalarType::Uint64),
            ("[-1, 1]", ScalarType::Int8),
            ("[-200, 1]", ScalarType::Int16),
            ("[-70000, 1]", ScalarType::Int32),
            ("[-5000000000, 1]", ScalarType::Int64),
            // One fractional value makes the whole array a float.
            ("[1, 2.5]", ScalarType::Float64),
            // A signed type still has to hold the largest positive value.
            ("[-1, 200]", ScalarType::Int16),
            // Strings are not supported inline.
            ("['a', 'b']", ScalarType::Unknown),
            ("[true, false]", ScalarType::Bool8),
        ];

        for (data, expected) in cases {
            let doc = asdf_yaml::parse_document(&format!("a: {data}\n")).unwrap();
            let root = doc.root().unwrap();
            let node = doc.mapping_get(root, "a").unwrap();
            assert_eq!(infer_inline_datatype(&doc, node), expected, "{data}");
        }
    }

    /// The bare-sequence shorthand infers its type as well as its shape.
    #[test]
    fn the_shorthand_form_infers_both_shape_and_type() {
        let doc = asdf_yaml::parse_document("a: [[0, 1, 2], [3, 4, 5], [6, 7, 8]]\n").unwrap();
        let root = doc.root().unwrap();
        let nd = Ndarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();

        assert_eq!(nd.resolved_shape(None).unwrap(), vec![3, 3]);
        assert_eq!(nd.datatype.scalar, ScalarType::Uint8);
        assert!(matches!(nd.source, Source::Inline(_)));
    }
    use crate::core::datatype::ScalarType;
    use asdf_yaml::parse_document;

    fn parse_nd(yaml: &str) -> Result<Ndarray> {
        let doc = parse_document(yaml).unwrap();
        let root = doc.root().unwrap();
        let nd = doc.mapping_get(root, "a").unwrap();
        Ndarray::parse(&doc, nd)
    }

    #[test]
    fn parses_a_block_backed_array() {
        let nd = parse_nd(
            "a:\n  source: 0\n  datatype: float64\n  shape: [1024, 1024]\n  byteorder: little\n",
        )
        .unwrap();
        assert_eq!(nd.source, Source::Block(0));
        assert_eq!(nd.datatype.scalar, ScalarType::Float64);
        assert_eq!(nd.byteorder, ByteOrder::Little);
        assert_eq!(nd.resolved_shape(None).unwrap(), vec![1024, 1024]);
        assert_eq!(nd.len(None).unwrap(), 1024 * 1024);
        assert_eq!(nd.nbytes(None).unwrap(), 1024 * 1024 * 8);
    }

    #[test]
    fn parses_a_view_with_offset_and_strides() {
        // The schema's own example: a tile of a larger image.
        let nd = parse_nd(
            "a:\n  source: 0\n  shape: [256, 256]\n  datatype: float64\n  \
             byteorder: little\n  strides: [8192, 8]\n  offset: 2099200\n",
        )
        .unwrap();
        assert_eq!(nd.offset, 2099200);
        assert_eq!(nd.strides, Some(vec![8192, 8]));
    }

    #[test]
    fn parses_inline_data_under_a_data_key() {
        let nd = parse_nd("a:\n  data: [1, 2, 3, 4]\n  datatype: int64\n  shape: [4]\n").unwrap();
        assert!(matches!(nd.source, Source::Inline(_)));
        assert_eq!(nd.resolved_shape(None).unwrap(), vec![4]);
    }

    #[test]
    fn parses_the_bare_sequence_shorthand() {
        // The schema allows the whole tagged value to be the nested data.
        let nd = parse_nd("a: [[1, 0, 0], [0, 1, 0], [0, 0, 1]]\n").unwrap();
        assert!(matches!(nd.source, Source::Inline(_)));
        assert_eq!(nd.resolved_shape(None).unwrap(), vec![3, 3]);
    }

    #[test]
    fn infers_nested_inline_shape() {
        let nd = parse_nd("a:\n  data: [[1, 2, 3], [4, 5, 6]]\n").unwrap();
        assert_eq!(nd.resolved_shape(None).unwrap(), vec![2, 3]);
    }

    #[test]
    fn an_external_source_is_a_uri() {
        let nd = parse_nd(
            "a:\n  source: external.asdf\n  shape: [4]\n  datatype: int8\n  byteorder: little\n",
        )
        .unwrap();
        assert_eq!(nd.source, Source::External("external.asdf".into()));
    }

    #[test]
    fn a_quoted_numeric_source_is_still_a_uri() {
        // Quoting makes it a string, so it names a file rather than a block.
        let nd =
            parse_nd("a:\n  source: '0'\n  shape: [4]\n  datatype: int8\n  byteorder: little\n")
                .unwrap();
        assert_eq!(nd.source, Source::External("0".into()));
    }

    #[test]
    fn source_minus_one_is_the_last_block() {
        let nd =
            parse_nd("a:\n  source: -1\n  shape: ['*']\n  datatype: int64\n  byteorder: little\n")
                .unwrap();
        assert_eq!(nd.source, Source::LastBlock);
    }

    #[test]
    fn a_star_dimension_is_sized_from_the_block() {
        let nd = parse_nd(
            "a:\n  source: -1\n  shape: ['*', 4]\n  datatype: int64\n  byteorder: little\n",
        )
        .unwrap();
        assert_eq!(nd.shape, vec![None, Some(4)]);

        // Each row is 4 int64s, so 32 bytes; 320 bytes is 10 rows.
        assert_eq!(nd.resolved_shape(Some(320)).unwrap(), vec![10, 4]);
        // A partial trailing row is not counted.
        assert_eq!(nd.resolved_shape(Some(330)).unwrap(), vec![10, 4]);
        // Without a block size the dimension cannot be resolved.
        assert!(nd.resolved_shape(None).is_err());
    }

    #[test]
    fn parses_both_mask_forms() {
        let nd = parse_nd(
            "a:\n  source: 0\n  shape: [4]\n  datatype: float64\n  byteorder: little\n  mask: -999\n",
        )
        .unwrap();
        assert_eq!(nd.mask, Some(Mask::Value("-999".into())));

        let nd = parse_nd(
            "a:\n  source: 0\n  shape: [4]\n  datatype: float64\n  byteorder: little\n  \
             mask:\n    source: 1\n    shape: [4]\n    datatype: bool8\n",
        )
        .unwrap();
        assert!(matches!(nd.mask, Some(Mask::Array(_))));
    }

    #[test]
    fn rejects_an_ndarray_with_no_data_at_all() {
        assert!(parse_nd("a:\n  shape: [4]\n  datatype: int8\n").is_err());
    }

    #[test]
    fn c_strides_are_row_major() {
        // A 2x3 array of 8-byte elements: rows are 24 bytes, columns 8.
        assert_eq!(Ndarray::c_strides(&[2, 3], 8), Some(vec![24, 8]));
        assert_eq!(Ndarray::c_strides(&[4], 4), Some(vec![4]));
        assert_eq!(Ndarray::c_strides(&[2, 3, 4], 1), Some(vec![12, 4, 1]));
        // A shape that would wrap gets no strides at all, rather than
        // strides that silently address the wrong element.
        assert_eq!(Ndarray::c_strides(&[u64::MAX / 2, 4, 4], 8), None);
    }

    #[test]
    fn compound_arrays_size_by_record() {
        let nd = parse_nd(
            "a:\n  source: 0\n  shape: [64]\n  byteorder: little\n  \
             datatype:\n    - name: x\n      datatype: float64\n    \
             - name: y\n      datatype: float64\n",
        )
        .unwrap();
        assert!(nd.datatype.is_structured());
        assert_eq!(nd.datatype.item_size(), 16);
        assert_eq!(nd.nbytes(None).unwrap(), 64 * 16);
    }
}
