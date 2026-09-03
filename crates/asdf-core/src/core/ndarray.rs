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

use crate::core::datatype::{ByteOrder, Datatype, parse_shape_with_star};
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
            return Ok(Ndarray {
                source: Source::Inline(doc.resolve(id)),
                shape: infer_inline_shape(doc, doc.resolve(id)),
                datatype: Datatype::default(),
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
            None => Datatype::default(),
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
                    let row: u64 = self.shape[idx + 1..]
                        .iter()
                        .map(|d| d.unwrap_or(1))
                        .product::<u64>()
                        .max(1);
                    let row_bytes = row.checked_mul(item).filter(|b| *b != 0).ok_or_else(|| {
                        err!(InvalidArgument, "cannot size a '*' dimension with a zero-width row")
                    })?;
                    out.push(bytes / row_bytes);
                }
            }
        }
        Ok(out)
    }

    /// The number of elements, for a fully-known shape.
    pub fn len(&self, block_bytes: Option<u64>) -> Result<u64> {
        Ok(self.resolved_shape(block_bytes)?.iter().product())
    }

    /// Whether the array has no elements.
    pub fn is_empty(&self, block_bytes: Option<u64>) -> Result<bool> {
        Ok(self.len(block_bytes)? == 0)
    }

    /// The number of bytes the elements occupy.
    pub fn nbytes(&self, block_bytes: Option<u64>) -> Result<u64> {
        Ok(self.len(block_bytes)? * self.datatype.item_size())
    }

    /// C-contiguous strides for a shape, in bytes.
    pub fn c_strides(shape: &[u64], item_size: u64) -> Vec<i64> {
        let mut strides = vec![0i64; shape.len()];
        let mut acc = item_size as i64;
        for idx in (0..shape.len()).rev() {
            strides[idx] = acc;
            acc *= shape[idx] as i64;
        }
        strides
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

/// Work out the shape of nested inline sequences.
fn infer_inline_shape(doc: &Document, id: NodeId) -> Vec<Option<u64>> {
    let mut shape = Vec::new();
    let mut current = id;
    // Follow the first element down; a ragged array is not valid ASDF, so the
    // first branch describes the whole.
    while let Some(items) = doc.sequence_items(current) {
        shape.push(Some(items.len() as u64));
        match items.first() {
            Some(first) => current = doc.resolve(*first),
            None => break,
        }
    }
    shape
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(Ndarray::c_strides(&[2, 3], 8), vec![24, 8]);
        assert_eq!(Ndarray::c_strides(&[4], 4), vec![4]);
        assert_eq!(Ndarray::c_strides(&[2, 3, 4], 1), vec![12, 4, 1]);
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
