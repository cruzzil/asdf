//! The `core/datatype` schema: how an ndarray's elements are laid out.
//!
//! A datatype is one of three shapes:
//!
//! - a **scalar**, named by a string such as `int64` or `float32`;
//! - a **fixed-length string**, written as a two-element sequence such as
//!   `['ascii', 32]` or `['ucs4', 8]`, where the length is in *characters*
//!   (so a `ucs4` element occupies four times that many bytes);
//! - a **compound** type, a sequence of fields, each of which may itself name
//!   a datatype, a byte order, and a sub-array shape.

use asdf_yaml::{Document, NodeData, NodeId};

use crate::error::{Result, err};

/// The scalar element types, mirroring `asdf_scalar_datatype_t`.
///
/// Discriminants are part of the C ABI and must not be reordered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum ScalarType {
    /// Unsupported or unrecognised.
    #[default]
    Unknown = 0,
    /// Signed 8-bit integer.
    Int8,
    /// Unsigned 8-bit integer.
    Uint8,
    /// Signed 16-bit integer.
    Int16,
    /// Unsigned 16-bit integer.
    Uint16,
    /// Signed 32-bit integer.
    Int32,
    /// Unsigned 32-bit integer.
    Uint32,
    /// Signed 64-bit integer.
    Int64,
    /// Unsigned 64-bit integer.
    Uint64,
    /// Half-precision float.
    Float16,
    /// Single-precision float.
    Float32,
    /// Double-precision float.
    Float64,
    /// A pair of `float32`s.
    Complex64,
    /// A pair of `float64`s.
    Complex128,
    /// An 8-bit boolean.
    Bool8,
    /// Fixed-length ASCII text, one byte per character.
    Ascii,
    /// Fixed-length UCS-4 text, four bytes per character.
    Ucs4,
    /// A compound or otherwise non-scalar type.
    Structured,
}

impl ScalarType {
    /// Parse the schema's name for a scalar type.
    pub fn from_name(name: &str) -> Self {
        match name {
            "int8" => ScalarType::Int8,
            "uint8" => ScalarType::Uint8,
            "int16" => ScalarType::Int16,
            "uint16" => ScalarType::Uint16,
            "int32" => ScalarType::Int32,
            "uint32" => ScalarType::Uint32,
            "int64" => ScalarType::Int64,
            "uint64" => ScalarType::Uint64,
            "float16" => ScalarType::Float16,
            "float32" => ScalarType::Float32,
            "float64" => ScalarType::Float64,
            "complex64" => ScalarType::Complex64,
            "complex128" => ScalarType::Complex128,
            "bool8" => ScalarType::Bool8,
            "ascii" => ScalarType::Ascii,
            "ucs4" => ScalarType::Ucs4,
            _ => ScalarType::Unknown,
        }
    }

    /// The schema's name for this type.
    pub fn name(self) -> &'static str {
        match self {
            ScalarType::Unknown => "unknown",
            ScalarType::Int8 => "int8",
            ScalarType::Uint8 => "uint8",
            ScalarType::Int16 => "int16",
            ScalarType::Uint16 => "uint16",
            ScalarType::Int32 => "int32",
            ScalarType::Uint32 => "uint32",
            ScalarType::Int64 => "int64",
            ScalarType::Uint64 => "uint64",
            ScalarType::Float16 => "float16",
            ScalarType::Float32 => "float32",
            ScalarType::Float64 => "float64",
            ScalarType::Complex64 => "complex64",
            ScalarType::Complex128 => "complex128",
            ScalarType::Bool8 => "bool8",
            ScalarType::Ascii => "ascii",
            ScalarType::Ucs4 => "ucs4",
            ScalarType::Structured => "structured",
        }
    }

    /// Bytes per element for a numeric type.
    ///
    /// Returns 0 for the string and non-scalar types, whose width depends on
    /// a length the datatype must carry separately.
    pub fn size(self) -> u64 {
        match self {
            ScalarType::Int8 | ScalarType::Uint8 | ScalarType::Bool8 => 1,
            ScalarType::Int16 | ScalarType::Uint16 | ScalarType::Float16 => 2,
            ScalarType::Int32 | ScalarType::Uint32 | ScalarType::Float32 => 4,
            ScalarType::Int64
            | ScalarType::Uint64
            | ScalarType::Float64
            | ScalarType::Complex64 => 8,
            ScalarType::Complex128 => 16,
            ScalarType::Ascii | ScalarType::Ucs4 | ScalarType::Structured | ScalarType::Unknown => {
                0
            }
        }
    }

    /// Whether this is a numeric type this library can convert between.
    pub fn is_numeric(self) -> bool {
        !matches!(
            self,
            ScalarType::Unknown
                | ScalarType::Ascii
                | ScalarType::Ucs4
                | ScalarType::Structured
                | ScalarType::Complex64
                | ScalarType::Complex128
        )
    }

    /// Whether this is one of the fixed-length string types.
    pub fn is_string(self) -> bool {
        matches!(self, ScalarType::Ascii | ScalarType::Ucs4)
    }

    /// Bytes per character, for the string types.
    pub fn bytes_per_char(self) -> u64 {
        match self {
            ScalarType::Ascii => 1,
            ScalarType::Ucs4 => 4,
            _ => 0,
        }
    }
}

/// Byte order, mirroring `asdf_byteorder_t`.
///
/// The discriminants are the ASCII codes for `>` and `<`, which is part of
/// the C ABI.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum ByteOrder {
    /// Not a valid byte order.
    Invalid = -1,
    /// Unspecified; the containing array's order applies.
    #[default]
    Default = 0,
    /// Big-endian.
    Big = b'>' as i32,
    /// Little-endian.
    Little = b'<' as i32,
}

impl ByteOrder {
    /// Parse the schema's spelling.
    pub fn from_name(name: &str) -> Self {
        match name {
            "big" => ByteOrder::Big,
            "little" => ByteOrder::Little,
            _ => ByteOrder::Invalid,
        }
    }

    /// The schema's spelling.
    pub fn name(self) -> &'static str {
        match self {
            ByteOrder::Big => "big",
            ByteOrder::Little => "little",
            ByteOrder::Default => "",
            ByteOrder::Invalid => "invalid",
        }
    }

    /// This machine's byte order.
    pub fn native() -> Self {
        if cfg!(target_endian = "big") { ByteOrder::Big } else { ByteOrder::Little }
    }

    /// Whether reading this order on this machine needs a byte swap.
    pub fn needs_swap(self) -> bool {
        matches!(self, ByteOrder::Big | ByteOrder::Little) && self != ByteOrder::native()
    }
}

/// One field of a compound datatype.
#[derive(Clone, PartialEq, Debug)]
pub struct Field {
    /// The field's name, if it was given one.
    pub name: Option<String>,
    /// The field's own datatype.
    pub datatype: Datatype,
}

/// A complete datatype.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Datatype {
    /// The scalar type, or [`ScalarType::Structured`] for a compound type.
    pub scalar: ScalarType,
    /// Element size in bytes. Computed for numeric types; carried explicitly
    /// for strings, where it is the length in characters times the character
    /// width.
    pub size: u64,
    /// Byte order for this type's elements, where it differs from the array's.
    pub byteorder: ByteOrder,
    /// Sub-array shape, for a field that is itself an array.
    pub shape: Vec<u64>,
    /// Fields, for a compound type.
    pub fields: Vec<Field>,
}

impl Datatype {
    /// A plain scalar datatype.
    pub fn scalar(scalar: ScalarType) -> Self {
        Self { scalar, size: scalar.size(), ..Default::default() }
    }

    /// A fixed-length string datatype, sized in characters.
    pub fn string(scalar: ScalarType, length: u64) -> Self {
        Self { scalar, size: length * scalar.bytes_per_char(), ..Default::default() }
    }

    /// Whether this is a compound type.
    pub fn is_structured(&self) -> bool {
        self.scalar == ScalarType::Structured || !self.fields.is_empty()
    }

    /// The size of one element in bytes.
    ///
    /// For a compound type this is the sum of its fields; for a field with a
    /// sub-array shape, the element size times the number of elements.
    pub fn item_size(&self) -> u64 {
        let base = if self.is_structured() {
            self.fields.iter().map(|f| f.datatype.item_size()).sum()
        } else if self.size != 0 {
            self.size
        } else {
            self.scalar.size()
        };
        base * self.shape.iter().product::<u64>().max(1)
    }

    /// Parse a datatype from a tree node.
    ///
    /// Accepts all three shapes the schema allows: a name, a
    /// `[kind, length]` string pair, and a sequence of fields.
    pub fn parse(doc: &Document, id: NodeId) -> Result<Self> {
        let node = doc.resolved(id);
        match &node.data {
            NodeData::Scalar { value, .. } => {
                let scalar = ScalarType::from_name(value);
                if scalar == ScalarType::Unknown {
                    return Err(err!(InvalidArgument, "unknown datatype: {value}"));
                }
                if scalar.is_string() {
                    // A bare `ascii` with no length is a zero-length string,
                    // matching libasdf's treatment of size 0.
                    return Ok(Datatype::string(scalar, 0));
                }
                Ok(Datatype::scalar(scalar))
            }

            NodeData::Sequence { items, .. } => {
                // `['ascii', 32]` is a string type; anything else is compound.
                if items.len() == 2
                    && let Some(name) = doc.resolved(items[0]).as_str()
                    && ScalarType::from_name(name).is_string()
                    && let Some(len_text) = doc.resolved(items[1]).as_str()
                    && let Ok(length) = len_text.parse::<u64>()
                {
                    return Ok(Datatype::string(ScalarType::from_name(name), length));
                }

                let mut fields = Vec::with_capacity(items.len());
                for item in items {
                    fields.push(Self::parse_field(doc, *item)?);
                }
                Ok(Datatype {
                    scalar: ScalarType::Structured,
                    size: 0,
                    byteorder: ByteOrder::Default,
                    shape: Vec::new(),
                    fields,
                })
            }

            _ => Err(err!(InvalidArgument, "datatype must be a string or a sequence")),
        }
    }

    /// Parse one entry of a compound datatype's field list.
    fn parse_field(doc: &Document, id: NodeId) -> Result<Field> {
        let node = doc.resolved(id);

        // A field may be given as a bare datatype rather than a mapping.
        if !matches!(node.data, NodeData::Mapping { .. }) {
            return Ok(Field { name: None, datatype: Self::parse(doc, id)? });
        }

        let name =
            doc.mapping_get(id, "name").and_then(|n| doc.resolved(n).as_str().map(str::to_string));

        let inner = doc
            .mapping_get(id, "datatype")
            .ok_or_else(|| err!(InvalidArgument, "compound datatype field has no datatype"))?;
        let mut datatype = Self::parse(doc, inner)?;

        if let Some(bo) = doc.mapping_get(id, "byteorder")
            && let Some(text) = doc.resolved(bo).as_str()
        {
            datatype.byteorder = ByteOrder::from_name(text);
        }
        if let Some(shape) = doc.mapping_get(id, "shape") {
            datatype.shape = parse_shape(doc, shape)?;
        }

        Ok(Field { name, datatype })
    }
}

/// Parse a `shape` sequence.
///
/// The schema allows the first entry to be `*` for a streamed array, whose
/// length is determined from the block's size instead. That is reported as
/// `None` for that dimension by [`parse_shape_with_star`]; this stricter
/// form rejects it.
pub fn parse_shape(doc: &Document, id: NodeId) -> Result<Vec<u64>> {
    parse_shape_with_star(doc, id)?
        .into_iter()
        .map(|d| d.ok_or_else(|| err!(InvalidArgument, "'*' is not allowed in this shape")))
        .collect()
}

/// Parse a `shape` sequence, allowing `*` in the first position.
pub fn parse_shape_with_star(doc: &Document, id: NodeId) -> Result<Vec<Option<u64>>> {
    let items =
        doc.sequence_items(id).ok_or_else(|| err!(InvalidArgument, "shape must be a sequence"))?;

    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let text = doc
            .resolved(*item)
            .as_str()
            .ok_or_else(|| err!(InvalidArgument, "shape entry {idx} is not a scalar"))?;
        if text == "*" {
            if idx != 0 {
                return Err(err!(
                    InvalidArgument,
                    "'*' may only appear as the first shape entry, found it at {idx}"
                ));
            }
            out.push(None);
            continue;
        }
        let dim = text
            .parse::<u64>()
            .map_err(|_| err!(InvalidArgument, "shape entry {idx} is not an integer: {text}"))?;
        out.push(Some(dim));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asdf_yaml::parse_document;

    fn parse_dt(yaml: &str) -> Result<Datatype> {
        let doc = parse_document(yaml).unwrap();
        let root = doc.root().unwrap();
        let dt = doc.mapping_get(root, "datatype").unwrap();
        Datatype::parse(&doc, dt)
    }

    #[test]
    fn scalar_names_round_trip() {
        for name in [
            "int8",
            "uint8",
            "int16",
            "uint16",
            "int32",
            "uint32",
            "int64",
            "uint64",
            "float16",
            "float32",
            "float64",
            "complex64",
            "complex128",
            "bool8",
            "ascii",
            "ucs4",
        ] {
            let t = ScalarType::from_name(name);
            assert_ne!(t, ScalarType::Unknown, "{name}");
            assert_eq!(t.name(), name);
        }
        assert_eq!(ScalarType::from_name("int128"), ScalarType::Unknown);
        assert_eq!(ScalarType::from_name(""), ScalarType::Unknown);
    }

    #[test]
    fn scalar_sizes_match_the_schema() {
        assert_eq!(ScalarType::Int8.size(), 1);
        assert_eq!(ScalarType::Bool8.size(), 1);
        assert_eq!(ScalarType::Float16.size(), 2);
        assert_eq!(ScalarType::Uint32.size(), 4);
        assert_eq!(ScalarType::Float64.size(), 8);
        assert_eq!(ScalarType::Complex64.size(), 8);
        assert_eq!(ScalarType::Complex128.size(), 16);
        // Strings carry their width separately.
        assert_eq!(ScalarType::Ascii.size(), 0);
        assert_eq!(ScalarType::Ucs4.size(), 0);
    }

    #[test]
    fn byteorder_discriminants_are_the_ascii_codes() {
        // Part of the C ABI: the header defines these as '>' and '<'.
        assert_eq!(ByteOrder::Big as i32, 62);
        assert_eq!(ByteOrder::Little as i32, 60);
        assert_eq!(ByteOrder::Invalid as i32, -1);
        assert_eq!(ByteOrder::Default as i32, 0);
    }

    #[test]
    fn byteorder_names_round_trip() {
        assert_eq!(ByteOrder::from_name("big"), ByteOrder::Big);
        assert_eq!(ByteOrder::from_name("little"), ByteOrder::Little);
        assert_eq!(ByteOrder::from_name("middle"), ByteOrder::Invalid);
        assert_eq!(ByteOrder::Big.name(), "big");
    }

    #[test]
    fn native_order_needs_no_swap() {
        assert!(!ByteOrder::native().needs_swap());
        let other = if ByteOrder::native() == ByteOrder::Little {
            ByteOrder::Big
        } else {
            ByteOrder::Little
        };
        assert!(other.needs_swap());
        // An unspecified order is not a swap instruction.
        assert!(!ByteOrder::Default.needs_swap());
    }

    #[test]
    fn parses_a_scalar_datatype() {
        let dt = parse_dt("datatype: float64\n").unwrap();
        assert_eq!(dt.scalar, ScalarType::Float64);
        assert_eq!(dt.item_size(), 8);
        assert!(!dt.is_structured());
    }

    #[test]
    fn rejects_an_unknown_datatype() {
        assert!(parse_dt("datatype: float128\n").is_err());
    }

    #[test]
    fn parses_fixed_length_strings() {
        // ascii is one byte per character...
        let dt = parse_dt("datatype: ['ascii', 32]\n").unwrap();
        assert_eq!(dt.scalar, ScalarType::Ascii);
        assert_eq!(dt.item_size(), 32);

        // ...and ucs4 is four, so the byte width is 4x the character count.
        let dt = parse_dt("datatype: ['ucs4', 8]\n").unwrap();
        assert_eq!(dt.scalar, ScalarType::Ucs4);
        assert_eq!(dt.item_size(), 32);
    }

    #[test]
    fn parses_a_compound_datatype() {
        // The example from the ndarray schema: a coordinate and a kernel.
        let yaml = "\
datatype:
  - name: coordinate
    datatype:
      - name: ra
        datatype: float64
      - name: dec
        datatype: float64
  - name: kernel
    datatype: float32
    shape: [3, 3]
";
        let dt = parse_dt(yaml).unwrap();
        assert!(dt.is_structured());
        assert_eq!(dt.fields.len(), 2);

        let coord = &dt.fields[0];
        assert_eq!(coord.name.as_deref(), Some("coordinate"));
        assert!(coord.datatype.is_structured());
        assert_eq!(coord.datatype.fields.len(), 2);
        assert_eq!(coord.datatype.item_size(), 16);

        let kernel = &dt.fields[1];
        assert_eq!(kernel.name.as_deref(), Some("kernel"));
        assert_eq!(kernel.datatype.shape, vec![3, 3]);
        // A 3x3 sub-array of float32 is 36 bytes.
        assert_eq!(kernel.datatype.item_size(), 36);

        // The record as a whole is the sum of its fields.
        assert_eq!(dt.item_size(), 16 + 36);
    }

    #[test]
    fn compound_fields_may_carry_a_byteorder() {
        let yaml = "\
datatype:
  - name: a
    datatype: int32
    byteorder: big
  - name: b
    datatype: int32
    byteorder: little
";
        let dt = parse_dt(yaml).unwrap();
        assert_eq!(dt.fields[0].datatype.byteorder, ByteOrder::Big);
        assert_eq!(dt.fields[1].datatype.byteorder, ByteOrder::Little);
    }

    #[test]
    fn compound_fields_may_be_bare_datatypes() {
        // The schema allows a field to be just a datatype, with no name.
        let dt = parse_dt("datatype: [int32, float64]\n").unwrap();
        assert!(dt.is_structured());
        assert_eq!(dt.fields.len(), 2);
        assert!(dt.fields[0].name.is_none());
        assert_eq!(dt.item_size(), 4 + 8);
    }

    #[test]
    fn parses_shapes() {
        let doc = parse_document("shape: [1024, 768]\n").unwrap();
        let root = doc.root().unwrap();
        let s = doc.mapping_get(root, "shape").unwrap();
        assert_eq!(parse_shape(&doc, s).unwrap(), vec![1024, 768]);
    }

    #[test]
    fn a_star_shape_is_allowed_only_first_and_only_where_permitted() {
        let doc = parse_document("shape: ['*', 4]\n").unwrap();
        let root = doc.root().unwrap();
        let s = doc.mapping_get(root, "shape").unwrap();

        // The streaming form: first dimension determined from the block size.
        assert_eq!(parse_shape_with_star(&doc, s).unwrap(), vec![None, Some(4)]);
        // The strict form rejects it.
        assert!(parse_shape(&doc, s).is_err());

        // A star anywhere else is malformed.
        let doc = parse_document("shape: [4, '*']\n").unwrap();
        let root = doc.root().unwrap();
        let s = doc.mapping_get(root, "shape").unwrap();
        assert!(parse_shape_with_star(&doc, s).is_err());
    }

    #[test]
    fn malformed_shapes_are_rejected() {
        let doc = parse_document("shape: [1, abc]\n").unwrap();
        let root = doc.root().unwrap();
        let s = doc.mapping_get(root, "shape").unwrap();
        assert!(parse_shape(&doc, s).is_err());

        let doc = parse_document("shape: 5\n").unwrap();
        let root = doc.root().unwrap();
        let s = doc.mapping_get(root, "shape").unwrap();
        assert!(parse_shape(&doc, s).is_err());
    }

    #[test]
    fn numeric_classification() {
        assert!(ScalarType::Int32.is_numeric());
        assert!(ScalarType::Float64.is_numeric());
        assert!(ScalarType::Bool8.is_numeric());
        // Not yet convertible, so not "numeric" for conversion purposes.
        assert!(!ScalarType::Complex64.is_numeric());
        assert!(!ScalarType::Ascii.is_numeric());
        assert!(ScalarType::Ascii.is_string());
        assert!(ScalarType::Ucs4.is_string());
    }
}
