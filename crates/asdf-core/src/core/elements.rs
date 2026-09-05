//! Decoding an ndarray's bytes into typed elements, and inlining them back
//! into the tree.
//!
//! Inlining is what the ASDF Standard's reference corpus asks a reader to do
//! before comparing a `.asdf` file against its expected `.yaml`: turn every
//! block-backed array into the nested sequences the expected output carries.

use asdf_yaml::{CollectionStyle, Document, Node, NodeData, NodeId, ScalarStyle};

use crate::core::datatype::{ByteOrder, Datatype, ScalarType};
use crate::core::ndarray::{Ndarray, Source};
use crate::error::{Result, err};

/// One decoded array element.
#[derive(Clone, PartialEq, Debug)]
pub enum Element {
    /// A signed integer.
    Int(i64),
    /// An unsigned integer.
    Uint(u64),
    /// A float, including the half- and single-precision types widened to
    /// `f64`.
    Float(f64),
    /// A boolean, from `bool8`.
    Bool(bool),
    /// Fixed-length text, with trailing NULs trimmed.
    Text(String),
    /// A complex number.
    Complex(f64, f64),
    /// One record of a compound array.
    Record(Vec<Element>),
}

/// Read a big- or little-endian integer of `n` bytes.
fn read_uint(bytes: &[u8], order: ByteOrder) -> u64 {
    let mut acc = 0u64;
    if order == ByteOrder::Big {
        for b in bytes {
            acc = (acc << 8) | u64::from(*b);
        }
    } else {
        for b in bytes.iter().rev() {
            acc = (acc << 8) | u64::from(*b);
        }
    }
    acc
}

/// Sign-extend an `n`-byte two's-complement value.
fn sign_extend(value: u64, bytes: usize) -> i64 {
    let bits = bytes * 8;
    if bits >= 64 {
        return value as i64;
    }
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

/// The order to actually use for a field: its own if set, else the array's.
fn effective_order(field: ByteOrder, array: ByteOrder) -> ByteOrder {
    match field {
        ByteOrder::Big | ByteOrder::Little => field,
        // The schema's default when nothing says otherwise.
        _ => match array {
            ByteOrder::Big | ByteOrder::Little => array,
            _ => ByteOrder::Little,
        },
    }
}

/// Decode a single element of `datatype` from the front of `bytes`.
fn decode_one(datatype: &Datatype, bytes: &[u8], array_order: ByteOrder) -> Result<Element> {
    if datatype.is_structured() {
        let mut fields = Vec::with_capacity(datatype.fields.len());
        let mut offset = 0usize;
        for field in &datatype.fields {
            let width = field.datatype.item_size() as usize;
            let slice = bytes.get(offset..offset + width).ok_or_else(|| {
                err!(UnexpectedEof, "compound element truncated at field offset {offset}")
            })?;
            fields.push(decode_one(&field.datatype, slice, array_order)?);
            offset += width;
        }
        return Ok(Element::Record(fields));
    }

    let order = effective_order(datatype.byteorder, array_order);
    let width = datatype.item_size() as usize;
    let raw = bytes.get(..width).ok_or_else(|| {
        err!(UnexpectedEof, "element needs {width} bytes, {} available", bytes.len())
    })?;

    Ok(match datatype.scalar {
        ScalarType::Bool8 => Element::Bool(raw[0] != 0),

        ScalarType::Uint8 | ScalarType::Uint16 | ScalarType::Uint32 | ScalarType::Uint64 => {
            Element::Uint(read_uint(raw, order))
        }

        ScalarType::Int8 | ScalarType::Int16 | ScalarType::Int32 | ScalarType::Int64 => {
            Element::Int(sign_extend(read_uint(raw, order), width))
        }

        ScalarType::Float16 => {
            let bits = read_uint(raw, order) as u16;
            Element::Float(f64::from(half::f16::from_bits(bits)))
        }
        ScalarType::Float32 => {
            let bits = read_uint(raw, order) as u32;
            Element::Float(f64::from(f32::from_bits(bits)))
        }
        ScalarType::Float64 => Element::Float(f64::from_bits(read_uint(raw, order))),

        ScalarType::Complex64 => {
            let re = f32::from_bits(read_uint(&raw[..4], order) as u32);
            let im = f32::from_bits(read_uint(&raw[4..], order) as u32);
            Element::Complex(f64::from(re), f64::from(im))
        }
        ScalarType::Complex128 => {
            let re = f64::from_bits(read_uint(&raw[..8], order));
            let im = f64::from_bits(read_uint(&raw[8..], order));
            Element::Complex(re, im)
        }

        ScalarType::Ascii => {
            // Fixed-length text is NUL-padded to its declared width.
            let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            Element::Text(String::from_utf8_lossy(&raw[..end]).into_owned())
        }
        ScalarType::Ucs4 => {
            let mut out = String::new();
            let (quads, _) = raw.as_chunks::<4>();
            for chunk in quads {
                let cp = read_uint(chunk, order) as u32;
                if cp == 0 {
                    break;
                }
                out.push(char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            Element::Text(out)
        }

        ScalarType::Unknown | ScalarType::Structured => {
            return Err(err!(
                InvalidArgument,
                "cannot decode a {} element",
                datatype.scalar.name()
            ));
        }
    })
}

/// Decode every element of an array, in C order.
///
/// `shape` must be fully resolved. Strides, when present, are honoured, so a
/// tile view or a Fortran-order array reads correctly.
pub fn decode_all(nd: &Ndarray, shape: &[u64], bytes: &[u8]) -> Result<Vec<Element>> {
    let item = nd.datatype.item_size();
    if item == 0 {
        return Err(err!(InvalidArgument, "cannot decode elements of zero width"));
    }

    let count: u64 = shape.iter().product();
    let count = usize::try_from(count)
        .map_err(|_| err!(OverLimit, "array has too many elements for this platform"))?;

    let strides = match &nd.strides {
        Some(s) if s.len() == shape.len() => s.clone(),
        Some(s) => {
            return Err(err!(
                InvalidArgument,
                "strides have {} entries but the shape has {}",
                s.len(),
                shape.len()
            ));
        }
        None => Ndarray::c_strides(shape, item),
    };

    let base = usize::try_from(nd.offset)
        .map_err(|_| err!(InvalidArgument, "ndarray offset overflows this platform"))?;

    let mut out = Vec::with_capacity(count);
    let mut index = vec![0u64; shape.len()];

    for _ in 0..count {
        // Byte position of this element, from the per-dimension strides.
        let mut pos = base as i64;
        for (dim, idx) in index.iter().enumerate() {
            pos += strides[dim] * (*idx as i64);
        }
        let pos = usize::try_from(pos)
            .map_err(|_| err!(InvalidArgument, "strides address a negative offset"))?;

        let slice = bytes.get(pos..).ok_or_else(|| {
            err!(UnexpectedEof, "element at byte {pos} is past the end of the block")
        })?;
        out.push(decode_one(&nd.datatype, slice, nd.byteorder)?);

        // Odometer step, last dimension fastest.
        for dim in (0..shape.len()).rev() {
            index[dim] += 1;
            if index[dim] < shape[dim] {
                break;
            }
            index[dim] = 0;
        }
    }
    Ok(out)
}

/// Decode an array whose data is inline in the tree rather than in a block.
///
/// The elements are already text in the document, so this is the mirror of
/// [`inline_ndarray`]: walk the nested sequences under `data` in row-major
/// order and read each scalar as the array's datatype.
///
/// The descent is driven by `shape`, not by how deeply the sequences happen
/// to nest. That distinction matters for a compound array, whose innermost
/// "sequence" is one record's fields rather than another dimension.
///
/// A `mask`ed value written as `null` decodes to the datatype's zero, since
/// [`Element`] carries no missing-value marker; `mask` itself is available on
/// the [`Ndarray`] for a caller that needs to know which those were.
pub fn decode_inline(doc: &Document, array: &Ndarray, shape: &[u64]) -> Result<Vec<Element>> {
    let Source::Inline(root) = array.source else {
        return Err(err!(InvalidArgument, "this array's data is not inline"));
    };

    let expected: u64 = shape.iter().copied().product();
    let mut out = Vec::with_capacity(usize::try_from(expected).unwrap_or(0));
    collect_inline(doc, root, &array.datatype, shape, &mut out)?;

    if out.len() as u64 != expected {
        return Err(err!(
            InvalidArgument,
            "inline data holds {} elements but the shape calls for {expected}",
            out.len()
        ));
    }
    Ok(out)
}

/// Walk `shape.len()` levels of sequences, reading each leaf as `datatype`.
fn collect_inline(
    doc: &Document,
    node: NodeId,
    datatype: &Datatype,
    shape: &[u64],
    out: &mut Vec<Element>,
) -> Result<()> {
    let resolved = doc.resolve(node);

    let Some((dim, rest)) = shape.split_first() else {
        // Past the last dimension: this is one element.
        out.push(leaf_element(doc, resolved, datatype)?);
        return Ok(());
    };

    let items = doc.sequence_items(resolved).map(<[_]>::to_vec).ok_or_else(|| {
        err!(InvalidArgument, "inline array data is not nested {} deep", shape.len())
    })?;
    if items.len() as u64 != *dim {
        return Err(err!(
            InvalidArgument,
            "inline dimension holds {} entries but the shape calls for {dim}",
            items.len()
        ));
    }
    for item in items {
        collect_inline(doc, item, datatype, rest, out)?;
    }
    Ok(())
}

/// Read one element: a scalar, or a record for a compound datatype.
fn leaf_element(doc: &Document, node: NodeId, datatype: &Datatype) -> Result<Element> {
    if !datatype.fields.is_empty() {
        let items = doc.sequence_items(node).map(<[_]>::to_vec).ok_or_else(|| {
            err!(InvalidArgument, "a compound element must be a sequence of its fields")
        })?;
        if items.len() != datatype.fields.len() {
            return Err(err!(
                InvalidArgument,
                "a compound element holds {} values but the datatype has {} fields",
                items.len(),
                datatype.fields.len()
            ));
        }
        let mut record = Vec::with_capacity(items.len());
        for (item, field) in items.iter().zip(datatype.fields.iter()) {
            record.push(leaf_element(doc, doc.resolve(*item), &field.datatype)?);
        }
        return Ok(Element::Record(record));
    }

    let text = doc
        .resolved(node)
        .as_str()
        .ok_or_else(|| err!(InvalidArgument, "inline array data holds a non-scalar leaf"))?;
    scalar_element(text, datatype.scalar)
}

/// Read one inline scalar as `scalar`.
fn scalar_element(text: &str, scalar: ScalarType) -> Result<Element> {
    use ScalarType as S;

    // A masked element is written `null`; there is no missing marker in
    // `Element`, so it reads as the type's zero.
    if matches!(text, "null" | "~" | "") {
        return Ok(match scalar {
            S::Float16 | S::Float32 | S::Float64 => Element::Float(0.0),
            S::Complex64 | S::Complex128 => Element::Complex(0.0, 0.0),
            S::Bool8 => Element::Bool(false),
            S::Ascii | S::Ucs4 => Element::Text(String::new()),
            S::Uint8 | S::Uint16 | S::Uint32 | S::Uint64 => Element::Uint(0),
            _ => Element::Int(0),
        });
    }

    let bad = |what: &str| err!(InvalidArgument, "inline {what} value {text:?} does not parse");
    match scalar {
        S::Uint8 | S::Uint16 | S::Uint32 | S::Uint64 => {
            Ok(Element::Uint(text.parse::<u64>().map_err(|_| bad("unsigned"))?))
        }
        S::Int8 | S::Int16 | S::Int32 | S::Int64 => {
            Ok(Element::Int(text.parse::<i64>().map_err(|_| bad("integer"))?))
        }
        S::Float16 | S::Float32 | S::Float64 => Ok(Element::Float(parse_inline_float(text)?)),
        S::Complex64 | S::Complex128 => {
            let (re, im) = parse_inline_complex(text)?;
            Ok(Element::Complex(re, im))
        }
        S::Bool8 => Ok(Element::Bool(matches!(text, "true" | "True" | "1"))),
        S::Ascii | S::Ucs4 => Ok(Element::Text(text.to_string())),
        S::Unknown | S::Structured => {
            Err(err!(InvalidArgument, "inline data needs a known scalar datatype"))
        }
    }
}

/// Parse a float, accepting YAML's non-finite spellings and Python's.
fn parse_inline_float(text: &str) -> Result<f64> {
    match text {
        ".nan" | ".NaN" | ".NAN" | "nan" => return Ok(f64::NAN),
        ".inf" | ".Inf" | ".INF" | "inf" => return Ok(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" | "-inf" => return Ok(f64::NEG_INFINITY),
        _ => {}
    }
    text.parse::<f64>()
        .map_err(|_| err!(InvalidArgument, "inline float value {text:?} does not parse"))
}

/// Parse a complex number in the spellings the `core/complex` schema allows.
///
/// Both `(1+2j)` and `1+2j` are valid, as is a pure imaginary `3j` or a pure
/// real `1`. The imaginary unit may be `i` or `j`, either case.
fn parse_inline_complex(text: &str) -> Result<(f64, f64)> {
    let body = text.trim();
    let body = body.strip_prefix('(').map_or(body, |rest| rest.strip_suffix(')').unwrap_or(rest));

    let imaginary_unit = |c: char| matches!(c, 'i' | 'I' | 'j' | 'J');
    let Some(unit) = body.char_indices().rev().find(|(_, c)| imaginary_unit(*c)) else {
        // No imaginary part at all: a plain real number.
        return Ok((parse_inline_float(body)?, 0.0));
    };
    // The unit must be last; anything after it is not a complex number.
    if unit.0 + unit.1.len_utf8() != body.len() {
        return Err(err!(InvalidArgument, "inline complex value {text:?} does not parse"));
    }
    let without_unit = &body[..unit.0];

    // Split off the imaginary part at the sign that separates the two, which
    // is the last `+`/`-` not part of an exponent and not the leading sign.
    let split = without_unit
        .char_indices()
        .rev()
        .find(|(index, c)| {
            (*c == '+' || *c == '-')
                && *index > 0
                && !matches!(without_unit.as_bytes()[index - 1], b'e' | b'E')
        })
        .map(|(index, _)| index);

    match split {
        None => Ok((0.0, parse_inline_float(without_unit)?)),
        Some(index) => {
            let (real, imaginary) = without_unit.split_at(index);
            // The imaginary part keeps its sign; a bare sign means one.
            let imaginary = match imaginary {
                "+" => "1",
                "-" => "-1",
                other => other,
            };
            Ok((parse_inline_float(real)?, parse_inline_float(imaginary)?))
        }
    }
}

/// The tag Python asdf puts on every inline complex value.
const COMPLEX_TAG: &str = "tag:stsci.edu:asdf/core/complex-1.0.0";

/// Format a float the way libasdf's emitter does.
///
/// `%.17g` for doubles, with YAML's own spellings for the non-finite values.
pub fn format_float(value: f64) -> String {
    if value.is_nan() {
        return ".nan".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() { "-.inf".into() } else { ".inf".into() };
    }
    // Shortest representation that round-trips, which is what Rust's
    // formatter gives and what a reader will parse back identically.
    let mut s = format!("{value}");
    if !s.contains('.') && !s.contains('e') && !s.contains("inf") && !s.contains("nan") {
        s.push_str(".0");
    }
    s
}

/// Render one element as a tree node.
fn element_to_node(doc: &mut Document, element: &Element) -> NodeId {
    match element {
        Element::Int(v) => doc.add_scalar(v.to_string()),
        Element::Uint(v) => doc.add_scalar(v.to_string()),
        Element::Bool(v) => doc.add_scalar(if *v { "true" } else { "false" }),
        Element::Float(v) => doc.add_scalar(format_float(*v)),
        // Text is quoted so it round-trips as a string rather than being
        // re-resolved as a number.
        Element::Text(s) => doc.add_scalar_styled(s.clone(), ScalarStyle::SingleQuoted),
        Element::Complex(re, im) => {
            // The core/complex schema leaves the spelling open; Python asdf
            // writes CPython's `repr(complex)` and tags each value, so that
            // is the canonical form to reproduce.
            let node = Node::scalar(crate::core::pyrepr::repr_complex(*re, *im))
                .with_tag(asdf_yaml::Tag::parse(COMPLEX_TAG));
            doc.add(node)
        }
        Element::Record(fields) => {
            let items: Vec<NodeId> = fields.iter().map(|f| element_to_node(doc, f)).collect();
            doc.add_sequence(items)
        }
    }
}

/// Build nested sequences for `elements` laid out in `shape`.
pub fn nest(doc: &mut Document, elements: &[Element], shape: &[u64]) -> NodeId {
    fn build(
        doc: &mut Document,
        elements: &[Element],
        shape: &[u64],
        cursor: &mut usize,
    ) -> NodeId {
        match shape.split_first() {
            None => {
                let node = element_to_node(doc, &elements[*cursor]);
                *cursor += 1;
                node
            }
            Some((dim, rest)) => {
                let mut items = Vec::with_capacity(*dim as usize);
                for _ in 0..*dim {
                    items.push(build(doc, elements, rest, cursor));
                }
                let id = doc.add_sequence(items);
                // Inline data reads better in flow style, which is how both
                // other implementations write it.
                if let NodeData::Sequence { style, .. } = &mut doc.node_mut(id).data {
                    *style = CollectionStyle::Flow;
                }
                id
            }
        }
    }

    let mut cursor = 0;
    build(doc, elements, shape, &mut cursor)
}

/// Replace an ndarray's `source` with the inline `data` it stands for.
///
/// This is the transformation the reference corpus prescribes. The array's
/// `byteorder`, `offset` and `strides` describe how bytes sit in a block and
/// become meaningless once the data is inline, so they are removed too.
pub fn inline_ndarray(
    doc: &mut Document,
    id: NodeId,
    elements: &[Element],
    shape: &[u64],
) -> Result<()> {
    let data = nest(doc, elements, shape);
    let target = doc.resolve(id);

    if !doc.node(target).is_mapping() {
        // The bare-sequence shorthand is already inline.
        return Ok(());
    }

    doc.mapping_remove(target, "source");
    for key in ["byteorder", "offset", "strides"] {
        doc.mapping_remove(target, key);
    }
    // A compound datatype's fields carry their own byteorder, which is just
    // as meaningless once the data is inline.
    if let Some(dt) = doc.mapping_get(target, "datatype")
        && let Some(fields) = doc.sequence_items(dt).map(<[_]>::to_vec)
    {
        for field in fields {
            let field = doc.resolve(field);
            if doc.node(field).is_mapping() {
                doc.mapping_remove(field, "byteorder");
            }
        }
    }
    doc.mapping_set(target, "data", data);

    // Record the shape the data actually has, replacing any '*'.
    let dims: Vec<NodeId> = shape.iter().map(|d| doc.add_scalar(d.to_string())).collect();
    let shape_node = doc.add_sequence(dims);
    if let NodeData::Sequence { style, .. } = &mut doc.node_mut(shape_node).data {
        *style = CollectionStyle::Flow;
    }
    doc.mapping_set(target, "shape", shape_node);
    Ok(())
}

/// Build a node holding an element, for tests and callers wanting one value.
pub fn element_node(doc: &mut Document, element: &Element) -> NodeId {
    element_to_node(doc, element)
}

/// A node with a tag applied.
pub fn tagged(doc: &mut Document, node: Node, tag: asdf_yaml::Tag) -> NodeId {
    doc.add(node.with_tag(tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use asdf_yaml::parse_document;

    fn ndarray(yaml: &str) -> Ndarray {
        let doc = parse_document(yaml).unwrap();
        let root = doc.root().unwrap();
        Ndarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap()
    }

    #[test]
    fn inline_integers_decode_from_the_tree() {
        let doc = parse_document(
            "a:\n  data: [[1, 2, 3], [4, 5, 6]]\n  datatype: int32\n  shape: [2, 3]\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let nd = Ndarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();
        let shape = nd.resolved_shape(None).unwrap();
        assert_eq!(shape, vec![2, 3]);

        let els = decode_inline(&doc, &nd, &shape).unwrap();
        assert_eq!(
            els,
            (1..=6).map(Element::Int).collect::<Vec<_>>(),
            "row-major order, flattened"
        );
    }

    #[test]
    fn inline_floats_accept_yamls_non_finite_spellings() {
        let doc = parse_document(
            "a:\n  data: [1.5, .inf, -.inf, .nan]\n  datatype: float64\n  shape: [4]\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let nd = Ndarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();
        let els = decode_inline(&doc, &nd, &[4]).unwrap();

        assert_eq!(els[0], Element::Float(1.5));
        assert_eq!(els[1], Element::Float(f64::INFINITY));
        assert_eq!(els[2], Element::Float(f64::NEG_INFINITY));
        let Element::Float(nan) = els[3] else { panic!("{:?}", els[3]) };
        assert!(nan.is_nan());
    }

    /// The schema allows a family of spellings; all of them must read.
    #[test]
    fn inline_complex_accepts_every_spelling_the_schema_allows() {
        let cases = [
            ("0j", (0.0, 0.0)),
            ("(1+2j)", (1.0, 2.0)),
            ("1+2j", (1.0, 2.0)),
            ("(1-2j)", (1.0, -2.0)),
            ("-1j", (0.0, -1.0)),
            ("(-0+0j)", (-0.0, 0.0)),
            ("3", (3.0, 0.0)),
            ("2i", (0.0, 2.0)),
            ("(1.5e-3+2.5e+4j)", (1.5e-3, 2.5e4)),
            // A bare sign before the unit means one.
            ("(1+j)", (1.0, 1.0)),
            ("(1-j)", (1.0, -1.0)),
        ];
        for (text, (re, im)) in cases {
            let got = parse_inline_complex(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(got.0, re, "real part of {text}");
            assert_eq!(got.1, im, "imaginary part of {text}");
        }

        // The non-finites, which the corpus does carry.
        let (re, im) = parse_inline_complex("(nan-infj)").unwrap();
        assert!(re.is_nan());
        assert_eq!(im, f64::NEG_INFINITY);
    }

    /// Everything we write must read back, which is the property that
    /// matters for a round trip through inline form.
    #[test]
    fn complex_spellings_round_trip_through_the_parser() {
        let values = [
            (0.0, 0.0),
            (-0.0, 0.0),
            (1.0, 2.0),
            (1.0, -2.0),
            (0.0, -1.0),
            (1.5e-3, 2.5e4),
            (f64::MAX, f64::MIN_POSITIVE),
        ];
        for (re, im) in values {
            let text = crate::core::pyrepr::repr_complex(re, im);
            let (back_re, back_im) = parse_inline_complex(&text).unwrap();
            assert_eq!(back_re.to_bits(), re.to_bits(), "{text}");
            assert_eq!(back_im.to_bits(), im.to_bits(), "{text}");
        }
    }

    #[test]
    fn inline_compound_records_stay_grouped() {
        let doc = parse_document(
            "a:\n  data: [[1, 2.5], [3, 4.5]]\n  shape: [2]\n  datatype:\n  \
             - {name: n, datatype: int32}\n  - {name: x, datatype: float64}\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let nd = Ndarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();
        let els = decode_inline(&doc, &nd, &[2]).unwrap();
        assert_eq!(
            els,
            vec![
                Element::Record(vec![Element::Int(1), Element::Float(2.5)]),
                Element::Record(vec![Element::Int(3), Element::Float(4.5)]),
            ]
        );
    }

    #[test]
    fn inline_data_must_match_the_declared_shape() {
        let doc =
            parse_document("a:\n  data: [1, 2, 3]\n  datatype: int32\n  shape: [4]\n").unwrap();
        let root = doc.root().unwrap();
        let nd = Ndarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();
        let err = decode_inline(&doc, &nd, &[4]).unwrap_err();
        assert!(err.message().contains("shape calls for 4"), "{err}");
    }

    /// A block-backed array decoded and re-read through inline form must
    /// come back the same, which is the property `tree_inlined` relies on.
    #[test]
    fn a_block_array_survives_a_trip_through_inline_form() {
        let nd =
            ndarray("a:\n  source: 0\n  shape: [5]\n  datatype: float64\n  byteorder: little\n");
        let values = [1.5f64, -2.25, 0.0, f64::MAX, -0.125];
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let original = decode_all(&nd, &[5], &bytes).unwrap();

        let mut doc = parse_document(
            "a:\n  source: 0\n  shape: [5]\n  datatype: float64\n  byteorder: little\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let node = doc.mapping_get(root, "a").unwrap();
        inline_ndarray(&mut doc, node, &original, &[5]).unwrap();

        let inlined = Ndarray::parse(&doc, node).unwrap();
        let read_back = decode_inline(&doc, &inlined, &[5]).unwrap();
        assert_eq!(read_back, original);
    }

    #[test]
    fn decodes_little_endian_integers() {
        let nd = ndarray("a:\n  source: 0\n  shape: [4]\n  datatype: int32\n  byteorder: little\n");
        let mut bytes = Vec::new();
        for v in [1i32, -1, 256, i32::MIN] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let els = decode_all(&nd, &[4], &bytes).unwrap();
        assert_eq!(
            els,
            vec![
                Element::Int(1),
                Element::Int(-1),
                Element::Int(256),
                Element::Int(i64::from(i32::MIN)),
            ]
        );
    }

    #[test]
    fn decodes_big_endian_integers() {
        let nd = ndarray("a:\n  source: 0\n  shape: [3]\n  datatype: int16\n  byteorder: big\n");
        let mut bytes = Vec::new();
        for v in [1i16, -2, 1000] {
            bytes.extend_from_slice(&v.to_be_bytes());
        }
        let els = decode_all(&nd, &[3], &bytes).unwrap();
        assert_eq!(els, vec![Element::Int(1), Element::Int(-2), Element::Int(1000)]);
    }

    #[test]
    fn byte_order_actually_changes_the_value() {
        let bytes = [0x01u8, 0x00];
        let le =
            ndarray("a:\n  source: 0\n  shape: [1]\n  datatype: uint16\n  byteorder: little\n");
        let be = ndarray("a:\n  source: 0\n  shape: [1]\n  datatype: uint16\n  byteorder: big\n");
        assert_eq!(decode_all(&le, &[1], &bytes).unwrap(), vec![Element::Uint(1)]);
        assert_eq!(decode_all(&be, &[1], &bytes).unwrap(), vec![Element::Uint(256)]);
    }

    #[test]
    fn decodes_floats_of_every_width() {
        let nd =
            ndarray("a:\n  source: 0\n  shape: [2]\n  datatype: float64\n  byteorder: little\n");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1.5f64.to_le_bytes());
        bytes.extend_from_slice(&(-0.25f64).to_le_bytes());
        assert_eq!(
            decode_all(&nd, &[2], &bytes).unwrap(),
            vec![Element::Float(1.5), Element::Float(-0.25)]
        );

        let nd =
            ndarray("a:\n  source: 0\n  shape: [1]\n  datatype: float32\n  byteorder: little\n");
        assert_eq!(
            decode_all(&nd, &[1], &2.5f32.to_le_bytes()).unwrap(),
            vec![Element::Float(2.5)]
        );

        let nd =
            ndarray("a:\n  source: 0\n  shape: [1]\n  datatype: float16\n  byteorder: little\n");
        let h = half::f16::from_f32(0.5);
        assert_eq!(
            decode_all(&nd, &[1], &h.to_bits().to_le_bytes()).unwrap(),
            vec![Element::Float(0.5)]
        );
    }

    #[test]
    fn decodes_bools_and_text() {
        let nd = ndarray("a:\n  source: 0\n  shape: [2]\n  datatype: bool8\n  byteorder: little\n");
        assert_eq!(
            decode_all(&nd, &[2], &[0u8, 1]).unwrap(),
            vec![Element::Bool(false), Element::Bool(true)]
        );

        // Fixed-length ASCII is NUL-padded and trimmed on the way out.
        let nd = ndarray(
            "a:\n  source: 0\n  shape: [2]\n  datatype: ['ascii', 4]\n  byteorder: little\n",
        );
        let bytes = b"M31\0Cas\0";
        assert_eq!(
            decode_all(&nd, &[2], bytes).unwrap(),
            vec![Element::Text("M31".into()), Element::Text("Cas".into())]
        );
    }

    #[test]
    fn decodes_ucs4_text() {
        let nd = ndarray(
            "a:\n  source: 0\n  shape: [1]\n  datatype: ['ucs4', 3]\n  byteorder: little\n",
        );
        let mut bytes = Vec::new();
        for cp in ['a' as u32, 0x00E9 /* é */, 0] {
            bytes.extend_from_slice(&cp.to_le_bytes());
        }
        assert_eq!(decode_all(&nd, &[1], &bytes).unwrap(), vec![Element::Text("aé".into())]);
    }

    #[test]
    fn honours_offset() {
        let nd = ndarray(
            "a:\n  source: 0\n  shape: [2]\n  datatype: uint8\n  byteorder: little\n  offset: 3\n",
        );
        let bytes = [9u8, 9, 9, 1, 2];
        assert_eq!(
            decode_all(&nd, &[2], &bytes).unwrap(),
            vec![Element::Uint(1), Element::Uint(2)]
        );
    }

    #[test]
    fn honours_strides_for_a_fortran_order_array() {
        // A 2x3 array stored column-major: strides are [1, 2] elements.
        let nd = ndarray(
            "a:\n  source: 0\n  shape: [2, 3]\n  datatype: uint8\n  byteorder: little\n  \
             strides: [1, 2]\n",
        );
        // Column-major layout of [[1,2,3],[4,5,6]].
        let bytes = [1u8, 4, 2, 5, 3, 6];
        let els = decode_all(&nd, &[2, 3], &bytes).unwrap();
        let values: Vec<u64> = els
            .iter()
            .map(|e| match e {
                Element::Uint(v) => *v,
                _ => unreachable!(),
            })
            .collect();
        // Read back in C order it must be the logical array.
        assert_eq!(values, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn honours_strides_for_a_tile_view() {
        // A 2x2 tile of a 4x4 uint8 image, starting at row 1 column 1.
        let nd = ndarray(
            "a:\n  source: 0\n  shape: [2, 2]\n  datatype: uint8\n  byteorder: little\n  \
             strides: [4, 1]\n  offset: 5\n",
        );
        let bytes: Vec<u8> = (0..16).collect();
        let els = decode_all(&nd, &[2, 2], &bytes).unwrap();
        let values: Vec<u64> = els
            .iter()
            .map(|e| match e {
                Element::Uint(v) => *v,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(values, vec![5, 6, 9, 10]);
    }

    #[test]
    fn decodes_compound_records() {
        let nd = ndarray(
            "a:\n  source: 0\n  shape: [2]\n  byteorder: little\n  \
             datatype:\n    - name: id\n      datatype: uint16\n    \
             - name: value\n      datatype: float32\n",
        );
        let mut bytes = Vec::new();
        for (id, value) in [(1u16, 1.5f32), (2, -2.5)] {
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let els = decode_all(&nd, &[2], &bytes).unwrap();
        assert_eq!(
            els,
            vec![
                Element::Record(vec![Element::Uint(1), Element::Float(1.5)]),
                Element::Record(vec![Element::Uint(2), Element::Float(-2.5)]),
            ]
        );
    }

    #[test]
    fn truncated_data_is_an_error_not_a_panic() {
        let nd = ndarray("a:\n  source: 0\n  shape: [4]\n  datatype: int64\n  byteorder: little\n");
        assert!(decode_all(&nd, &[4], &[0u8; 8]).is_err());
    }

    #[test]
    fn nesting_reproduces_the_shape() {
        let mut doc = Document::new();
        let els: Vec<Element> = (0..6).map(Element::Uint).collect();
        let node = nest(&mut doc, &els, &[2, 3]);
        doc.set_root(node);

        assert_eq!(doc.container_len(node), Some(2));
        let first = doc.sequence_get(node, 0).unwrap();
        assert_eq!(doc.container_len(first), Some(3));
        assert_eq!(doc.resolved(doc.sequence_get(first, 2).unwrap()).as_str(), Some("2"));
    }

    #[test]
    fn float_formatting_uses_yaml_spellings() {
        assert_eq!(format_float(f64::NAN), ".nan");
        assert_eq!(format_float(f64::INFINITY), ".inf");
        assert_eq!(format_float(f64::NEG_INFINITY), "-.inf");
        // A whole float keeps a decimal point so it does not read as an int.
        assert_eq!(format_float(1.0), "1.0");
        assert_eq!(format_float(1.5), "1.5");
    }

    #[test]
    fn inlining_replaces_source_with_data() {
        let mut doc = parse_document(
            "a:\n  source: 0\n  shape: [4]\n  datatype: uint8\n  byteorder: little\n  offset: 0\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let nd_id = doc.mapping_get(root, "a").unwrap();

        let els: Vec<Element> = (0..4).map(Element::Uint).collect();
        inline_ndarray(&mut doc, nd_id, &els, &[4]).unwrap();

        assert!(doc.mapping_get(nd_id, "source").is_none(), "source must be removed");
        assert!(doc.mapping_get(nd_id, "byteorder").is_none(), "byteorder is meaningless inline");
        assert!(doc.mapping_get(nd_id, "offset").is_none(), "offset is meaningless inline");

        let data = doc.mapping_get(nd_id, "data").expect("data must be added");
        assert_eq!(doc.container_len(data), Some(4));
        // The datatype survives; it still describes the values.
        assert!(doc.mapping_get(nd_id, "datatype").is_some());
    }
}
