//! Read and write ASDF files from Rust.
//!
//! [ASDF](https://www.asdf-format.org/) (Advanced Scientific Data Format) is
//! a hybrid format: a YAML tree describing the data, followed by binary
//! blocks holding it. It is the native format of the Nancy Grace Roman Space
//! Telescope and is widely used across astronomy.
//!
//! This is the idiomatic Rust face of the library. It borrows rather than
//! copies wherever the format allows, returns [`Result`] rather than error
//! codes, and needs no `unsafe`. For C interoperability use the `libasdf-rs`
//! crate instead, which exposes the same engine through libasdf's C ABI.
//!
//! # Reading
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use asdf::AsdfFile;
//!
//! let file = AsdfFile::open("observation.asdf")?;
//! let tree = file.tree()?.expect("a tree");
//!
//! // Values are addressed by path.
//! if let Some(name) = tree.get("meta/instrument/name").and_then(|v| v.as_str()) {
//!     println!("instrument: {name}");
//! }
//!
//! // Arrays are read through their block.
//! if let Some(array) = tree.get("data").and_then(|v| v.as_ndarray()) {
//!     let values = file.read_array_f64(&array)?;
//!     println!("{} elements", values.len());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Writing
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use asdf::AsdfBuilder;
//!
//! let mut builder = AsdfBuilder::new();
//! builder.set_str("name", "Dennis Richie")?;
//! builder.set_i64("foo", 42)?;
//!
//! // An array's data goes in a binary block, referenced from the tree.
//! let squares: Vec<u64> = (0..100).map(|i| i * i).collect();
//! builder.set_array_u64("powers/squares", &squares)?;
//!
//! builder.write_to_path("out.asdf")?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::borrow::Cow;
use std::path::Path;

use asdf_core::core::datatype::{ByteOrder, Datatype, ScalarType};
use asdf_core::core::elements::{Element, decode_all};
use asdf_core::core::ndarray::{Ndarray, Source};
use asdf_core::yaml::{
    self, CompareOptions, Document, NodeData, NodeId, Resolved, ScalarStyle, Schema, Tag,
};
use asdf_core::{PendingBlock, Reader, Writer};

pub use asdf_core::ChecksumStatus;
pub use asdf_core::compression::Compression;
pub use asdf_core::error::{Error, ErrorCode};
pub use asdf_core::version::Version;

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// An ASDF file opened for reading.
#[derive(Debug)]
pub struct AsdfFile {
    reader: Reader,
}

impl AsdfFile {
    /// Open a file from disk.
    ///
    /// The file is memory-mapped, so a large array costs nothing until it is
    /// actually read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self { reader: Reader::open(path)? })
    }

    /// Open a file already held in memory.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Ok(Self { reader: Reader::from_bytes(bytes)? })
    }

    /// The ASDF file-format version from the header line.
    pub fn format_version(&self) -> &Version {
        &self.reader.layout().format_version
    }

    /// The ASDF Standard version, if the file records one.
    pub fn standard_version(&self) -> Option<&Version> {
        self.reader.layout().standard_version.as_ref()
    }

    /// The YAML tree.
    ///
    /// A file in exploded form may legitimately have none, hence the
    /// [`Option`].
    pub fn tree(&self) -> Result<Option<Tree>> {
        Ok(self.reader.tree()?.map(|document| Tree { document }))
    }

    /// The tree with every block-backed array replaced by inline data.
    ///
    /// This is the transformation the ASDF Standard's reference corpus
    /// prescribes before comparing files. Arrays whose data lives outside
    /// this file are left alone and named in the returned list.
    pub fn tree_inlined(&self) -> Result<Option<(Tree, Vec<String>)>> {
        Ok(self.reader.tree_inlined()?.map(|(document, skipped)| (Tree { document }, skipped)))
    }

    /// The number of binary blocks.
    pub fn block_count(&self) -> usize {
        self.reader.block_count()
    }

    /// A block's data, decompressed if it needs to be.
    ///
    /// An uncompressed block borrows directly from the mapped file.
    pub fn block_data(&self, index: usize) -> Result<Cow<'_, [u8]>> {
        self.reader.block_data(index)
    }

    /// A block's bytes exactly as stored, without decompressing.
    pub fn block_raw(&self, index: usize) -> Result<&[u8]> {
        self.reader.block_raw(index)
    }

    /// How a block is compressed.
    pub fn block_compression(&self, index: usize) -> Result<Compression> {
        self.reader.block_compression(index)
    }

    /// Verify a block's MD5 checksum.
    ///
    /// An absent checksum is reported as [`ChecksumStatus::Absent`] rather
    /// than as a failure: the standard makes it optional.
    pub fn verify_block(&self, index: usize) -> Result<ChecksumStatus> {
        Ok(self.reader.verify_block_checksum(index)?.0)
    }

    /// Resolve an array's source to a block index in this file.
    fn block_for(&self, array: &Ndarray) -> Result<usize> {
        match &array.source {
            Source::Block(index) => Ok(*index),
            Source::LastBlock => self
                .reader
                .block_count()
                .checked_sub(1)
                .ok_or_else(|| Error::new(ErrorCode::InvalidArgument, "the file has no blocks")),
            Source::External(uri) => Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("array data lives in another file: {uri}"),
            )),
            Source::Inline(_) => {
                Err(Error::new(ErrorCode::InvalidArgument, "array data is inline, not in a block"))
            }
        }
    }

    /// Read every element of a block-backed or external array.
    ///
    /// An array whose `source` names another file -- the standard's exploded
    /// form -- is followed, provided this file was opened from a path and the
    /// name resolves to a file beneath its directory.
    ///
    /// An array whose data is *inline* in the tree carries no block, so it is
    /// an error here; read one with [`Tree::read_array`], which has the tree
    /// the values live in.
    pub fn read_array(&self, array: &Ndarray) -> Result<Vec<Element>> {
        if let Source::External(uri) = &array.source {
            let data = self.reader.external_block(uri)?;
            let shape = array.resolved_shape(Some(data.len() as u64))?;
            return decode_all(array, &shape, &data);
        }
        let index = self.block_for(array)?;
        let data = self.block_data(index)?;
        let shape = array.resolved_shape(Some(data.len() as u64))?;
        decode_all(array, &shape, &data)
    }

    /// Read every element of the array at `path`, wherever its data lives.
    ///
    /// The one call that covers all four cases: a block in this file, the
    /// last block, another file, or inline in the tree. It parses the tree
    /// each time, so a loop over many arrays is better served by holding a
    /// [`Tree`] and using [`Tree::read_array`] or [`AsdfFile::read_array`].
    pub fn read_array_at(&self, path: &str) -> Result<Vec<Element>> {
        let tree = self.tree()?.ok_or_else(|| {
            Error::new(ErrorCode::InvalidArgument, "this file has no tree to look in")
        })?;
        let value = tree.get(path).ok_or_else(|| {
            Error::new(ErrorCode::InvalidArgument, format!("no value at {path:?}"))
        })?;
        let array = value.as_ndarray().ok_or_else(|| {
            Error::new(ErrorCode::InvalidArgument, format!("the value at {path:?} is not an array"))
        })?;
        match array.source {
            Source::Inline(_) => tree.read_array(&array),
            _ => self.read_array(&array),
        }
    }

    /// Read an array converted to `f64`.
    ///
    /// Every numeric type converts; a string or compound array does not.
    pub fn read_array_f64(&self, array: &Ndarray) -> Result<Vec<f64>> {
        as_f64(self.read_array(array)?)
    }

    /// Read an array converted to `i64`.
    ///
    /// A float with a fractional part is an error rather than being
    /// truncated silently.
    pub fn read_array_i64(&self, array: &Ndarray) -> Result<Vec<i64>> {
        as_i64(self.read_array(array)?)
    }

    /// [`AsdfFile::read_array_at`] converted to `f64`.
    pub fn read_array_f64_at(&self, path: &str) -> Result<Vec<f64>> {
        as_f64(self.read_array_at(path)?)
    }

    /// [`AsdfFile::read_array_at`] converted to `i64`.
    pub fn read_array_i64_at(&self, path: &str) -> Result<Vec<i64>> {
        as_i64(self.read_array_at(path)?)
    }
}

/// Convert decoded elements to `f64`.
fn as_f64(elements: Vec<Element>) -> Result<Vec<f64>> {
    elements
        .into_iter()
        .map(|element| match element {
            Element::Float(v) => Ok(v),
            Element::Int(v) => Ok(v as f64),
            Element::Uint(v) => Ok(v as f64),
            Element::Bool(v) => Ok(if v { 1.0 } else { 0.0 }),
            other => Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("{other:?} cannot be read as a number"),
            )),
        })
        .collect()
}

/// Convert decoded elements to `i64`.
fn as_i64(elements: Vec<Element>) -> Result<Vec<i64>> {
    elements
        .into_iter()
        .map(|element| match element {
            Element::Int(v) => Ok(v),
            Element::Uint(v) => i64::try_from(v).map_err(|_| {
                Error::new(ErrorCode::InvalidArgument, format!("{v} does not fit an i64"))
            }),
            Element::Bool(v) => Ok(i64::from(v)),
            Element::Float(v) if v.fract() == 0.0 => Ok(v as i64),
            other => Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("{other:?} cannot be read as an integer"),
            )),
        })
        .collect()
}

/// A parsed ASDF tree.
#[derive(Clone, Debug)]
pub struct Tree {
    document: Document,
}

impl Tree {
    /// The root value.
    pub fn root(&self) -> Option<Value<'_>> {
        self.document.root().map(|node| Value { document: &self.document, node })
    }

    /// The value at a path, using ASDF's YAML Pointer syntax.
    ///
    /// A numeric component indexes a sequence or names a mapping key
    /// depending on what its parent is; negative indices count from the end.
    pub fn get(&self, path: &str) -> Option<Value<'_>> {
        self.document.lookup_str(path).map(|node| Value { document: &self.document, node })
    }

    /// Read every element of an array whose data is inline in this tree.
    ///
    /// Inline data needs no file: the values are already here. An array
    /// backed by a block is read through [`AsdfFile::read_array`] instead,
    /// and is an error here.
    pub fn read_array(&self, array: &Ndarray) -> Result<Vec<Element>> {
        let shape = array.resolved_shape(None)?;
        asdf_core::core::decode_inline(&self.document, array, &shape)
    }

    /// The underlying document, for callers needing the lower-level model.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Whether two trees represent the same values.
    ///
    /// Presentation -- flow versus block, quoting, integer width -- is
    /// ignored; tags, values, sequence order and the set of keys are not.
    pub fn value_eq(&self, other: &Tree) -> bool {
        yaml::compare(&self.document, &other.document, CompareOptions::default()).is_equal()
    }

    /// Render the tree back to YAML.
    pub fn to_yaml(&self) -> Result<String> {
        yaml::emit(&self.document)
            .map_err(|e| Error::new(ErrorCode::YamlParseFailed, e.to_string()))
    }
}

/// One value in a tree.
#[derive(Clone, Copy, Debug)]
pub struct Value<'a> {
    document: &'a Document,
    node: NodeId,
}

impl<'a> Value<'a> {
    /// The value's YAML tag, which in ASDF is what gives it its type.
    pub fn tag(&self) -> Option<&'a Tag> {
        self.document.tag_of(self.node)
    }

    /// Whether the tag names this ASDF schema, ignoring its version.
    ///
    /// So `has_tag("core/ndarray")` matches both `core/ndarray-1.0.0` and
    /// `core/ndarray-1.1.0`.
    pub fn has_tag(&self, name: &str) -> bool {
        self.tag().is_some_and(|t| t.split_version().0 == name)
    }

    /// The raw scalar text, whatever its type.
    pub fn as_raw_str(&self) -> Option<&'a str> {
        self.document.resolved(self.node).as_str()
    }

    /// The value as a string, if it is one.
    ///
    /// A quoted `"42"` is a string; an unquoted `42` is not.
    pub fn as_str(&self) -> Option<&'a str> {
        let node = self.document.resolved(self.node);
        let NodeData::Scalar { value, style } = &node.data else {
            return None;
        };
        matches!(yaml::resolve(value, *style, Schema::Libasdf), Resolved::String)
            .then_some(value.as_str())
    }

    /// The value as a signed integer.
    pub fn as_i64(&self) -> Option<i64> {
        match self.resolved()? {
            Resolved::Int(v, _) => Some(v),
            Resolved::Uint(v, _) => i64::try_from(v).ok(),
            _ => None,
        }
    }

    /// The value as an unsigned integer.
    pub fn as_u64(&self) -> Option<u64> {
        match self.resolved()? {
            Resolved::Uint(v, _) => Some(v),
            Resolved::Int(v, _) => u64::try_from(v).ok(),
            _ => None,
        }
    }

    /// The value as a float. Integers convert.
    pub fn as_f64(&self) -> Option<f64> {
        match self.resolved()? {
            Resolved::Double(v) => Some(v),
            Resolved::Int(v, _) => Some(v as f64),
            Resolved::Uint(v, _) => Some(v as f64),
            _ => None,
        }
    }

    /// The value as a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self.resolved()? {
            Resolved::Bool(v) => Some(v),
            _ => None,
        }
    }

    /// Whether the value is null.
    pub fn is_null(&self) -> bool {
        matches!(self.resolved(), Some(Resolved::Null))
    }

    fn resolved(&self) -> Option<Resolved> {
        let node = self.document.resolved(self.node);
        let NodeData::Scalar { value, style } = &node.data else {
            return None;
        };
        Some(yaml::resolve(value, *style, Schema::Libasdf))
    }

    /// Whether this is a mapping.
    pub fn is_mapping(&self) -> bool {
        self.document.resolved(self.node).is_mapping()
    }

    /// Whether this is a sequence.
    pub fn is_sequence(&self) -> bool {
        self.document.resolved(self.node).is_sequence()
    }

    /// The number of children, for a mapping or sequence.
    pub fn len(&self) -> Option<usize> {
        self.document.container_len(self.node)
    }

    /// Whether this container has no children.
    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|n| n == 0)
    }

    /// A mapping entry by key.
    pub fn get(&self, key: &str) -> Option<Value<'a>> {
        self.document
            .mapping_get(self.node, key)
            .map(|node| Value { document: self.document, node })
    }

    /// A sequence element, with negative indices counting from the end.
    pub fn at(&self, index: i64) -> Option<Value<'a>> {
        self.document
            .sequence_get(self.node, index)
            .map(|node| Value { document: self.document, node })
    }

    /// A value further down, by path.
    pub fn path(&self, path: &str) -> Option<Value<'a>> {
        let parsed = yaml::Path::parse(path).ok()?;
        self.document
            .lookup_from(self.node, &parsed)
            .map(|node| Value { document: self.document, node })
    }

    /// Iterate a mapping's entries in document order.
    pub fn entries(&self) -> impl Iterator<Item = (&'a str, Value<'a>)> + 'a {
        let document = self.document;
        let entries = document.mapping_entries(self.node).unwrap_or(&[]);
        entries.iter().map(move |entry| {
            let key = document.resolved(entry.key).as_str().unwrap_or_default();
            (key, Value { document, node: entry.value })
        })
    }

    /// Iterate a sequence's items.
    pub fn items(&self) -> impl Iterator<Item = Value<'a>> + 'a {
        let document = self.document;
        let items = document.sequence_items(self.node).unwrap_or(&[]);
        items.iter().map(move |node| Value { document, node: *node })
    }

    /// Interpret this value as an ndarray.
    ///
    /// Returns `None` when it is not one; the array's data is then read
    /// through [`AsdfFile::read_array`].
    pub fn as_ndarray(&self) -> Option<Ndarray> {
        Ndarray::parse(self.document, self.node).ok()
    }

    /// Whether this value is an alias to another node.
    pub fn is_alias(&self) -> bool {
        self.document.node(self.node).is_alias()
    }
}

/// Builds an ASDF file.
#[derive(Debug)]
pub struct AsdfBuilder {
    document: Document,
    blocks: Vec<PendingBlock>,
    compression: Compression,
}

impl Default for AsdfBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AsdfBuilder {
    /// A builder for a new, empty file.
    pub fn new() -> Self {
        let mut document = Document::new_asdf();
        let root = document.add(yaml::Node::mapping());
        document.node_mut(root).tag = Some(Tag::parse("tag:stsci.edu:asdf/core/asdf-1.1.0"));
        document.set_root(root);
        Self { document, blocks: Vec::new(), compression: Compression::None }
    }

    /// Compress every array written from here on.
    pub fn with_compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    /// The tree being built, for direct manipulation.
    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }

    fn insert(&mut self, path: &str, node: NodeId) -> Result<()> {
        self.document
            .insert_at_str(path, node)
            .map(|_| ())
            .map_err(|e| Error::new(ErrorCode::InvalidArgument, e.to_string()))
    }

    /// Set a string. It is quoted where needed so it reads back as a string.
    pub fn set_str(&mut self, path: &str, value: &str) -> Result<()> {
        let style = match yaml::resolve(value, ScalarStyle::Plain, Schema::Libasdf) {
            Resolved::String => ScalarStyle::Plain,
            _ => ScalarStyle::SingleQuoted,
        };
        let node = self.document.add_scalar_styled(value, style);
        self.insert(path, node)
    }

    /// Set a signed integer.
    pub fn set_i64(&mut self, path: &str, value: i64) -> Result<()> {
        let node = self.document.add_scalar(value.to_string());
        self.insert(path, node)
    }

    /// Set an unsigned integer.
    pub fn set_u64(&mut self, path: &str, value: u64) -> Result<()> {
        let node = self.document.add_scalar(value.to_string());
        self.insert(path, node)
    }

    /// Set a float.
    pub fn set_f64(&mut self, path: &str, value: f64) -> Result<()> {
        let node = self.document.add_scalar(asdf_core::core::elements::format_float(value));
        self.insert(path, node)
    }

    /// Set a boolean.
    pub fn set_bool(&mut self, path: &str, value: bool) -> Result<()> {
        let node = self.document.add_scalar(if value { "true" } else { "false" });
        self.insert(path, node)
    }

    /// Set a null.
    pub fn set_null(&mut self, path: &str) -> Result<()> {
        let node = self.document.add_scalar("null");
        self.insert(path, node)
    }

    /// Write an array into a binary block and reference it from the tree.
    fn set_array(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
        shape: &[u64],
        scalar: ScalarType,
    ) -> Result<()> {
        let index = self.blocks.len();
        self.blocks.push(PendingBlock::compressed(bytes, self.compression));

        // Build the core/ndarray mapping the schema defines.
        let source = self.document.add_scalar(index.to_string());
        let datatype = self.document.add_scalar(scalar.name());
        let byteorder = self.document.add_scalar(ByteOrder::native().name());

        let dims: Vec<NodeId> =
            shape.iter().map(|d| self.document.add_scalar(d.to_string())).collect();
        let shape_node = self.document.add_sequence(dims);
        if let NodeData::Sequence { style, .. } = &mut self.document.node_mut(shape_node).data {
            *style = yaml::CollectionStyle::Flow;
        }

        let keys: Vec<NodeId> = ["source", "datatype", "byteorder", "shape"]
            .iter()
            .map(|k| self.document.add_scalar(*k))
            .collect();
        let array = self.document.add_mapping(vec![
            (keys[0], source),
            (keys[1], datatype),
            (keys[2], byteorder),
            (keys[3], shape_node),
        ]);
        self.document.node_mut(array).tag =
            Some(Tag::parse("tag:stsci.edu:asdf/core/ndarray-1.1.0"));

        self.insert(path, array)
    }

    /// Write a one-dimensional `u64` array.
    pub fn set_array_u64(&mut self, path: &str, values: &[u64]) -> Result<()> {
        let bytes = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
        self.set_array(path, bytes, &[values.len() as u64], ScalarType::Uint64)
    }

    /// Write a one-dimensional `i64` array.
    pub fn set_array_i64(&mut self, path: &str, values: &[i64]) -> Result<()> {
        let bytes = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
        self.set_array(path, bytes, &[values.len() as u64], ScalarType::Int64)
    }

    /// Write a one-dimensional `f64` array.
    pub fn set_array_f64(&mut self, path: &str, values: &[f64]) -> Result<()> {
        let bytes = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
        self.set_array(path, bytes, &[values.len() as u64], ScalarType::Float64)
    }

    /// Write a multi-dimensional `f64` array.
    ///
    /// The data is taken in C order, and its length must match the shape.
    pub fn set_array_f64_shaped(
        &mut self,
        path: &str,
        values: &[f64],
        shape: &[u64],
    ) -> Result<()> {
        let expected: u64 = shape.iter().product();
        if expected != values.len() as u64 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("shape {shape:?} needs {expected} values, got {}", values.len()),
            ));
        }
        let bytes = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
        self.set_array(path, bytes, shape, ScalarType::Float64)
    }

    /// Add a raw binary block, returning its index.
    pub fn add_block(&mut self, data: Vec<u8>) -> usize {
        self.blocks.push(PendingBlock::compressed(data, self.compression));
        self.blocks.len() - 1
    }

    fn writer(&self) -> Writer {
        let mut writer = Writer::from_document(self.document.clone());
        for block in &self.blocks {
            writer.add_block(block.clone());
        }
        writer
    }

    /// Assemble the file in memory.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.writer().to_bytes()
    }

    /// Write the file to a path.
    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        self.writer().write_to_path(path)
    }

    /// Write the file to a stream.
    pub fn write_to(&self, sink: &mut impl std::io::Write) -> Result<()> {
        self.writer().write_to(sink)
    }
}

/// The default datatype for an array element, matching this machine.
pub fn native_byte_order() -> ByteOrder {
    ByteOrder::native()
}

/// A datatype for one of the scalar types.
pub fn scalar_datatype(scalar: ScalarType) -> Datatype {
    Datatype::scalar(scalar)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(builder: &AsdfBuilder) -> AsdfFile {
        AsdfFile::from_bytes(builder.to_bytes().unwrap()).unwrap()
    }

    #[test]
    fn an_inline_array_is_read_from_the_tree() {
        // Inline data needs no block, so it reads without a file behind it.
        let bytes = b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n\
%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
grid: !core/ndarray-1.1.0\n  data: [[1, 2, 3], [4, 5, 6]]\n  datatype: int32\n  shape: [2, 3]\n\
...\n"
            .to_vec();
        let file = AsdfFile::from_bytes(bytes).unwrap();
        let tree = file.tree().unwrap().unwrap();
        let array = tree.get("grid").unwrap().as_ndarray().unwrap();

        let elements = tree.read_array(&array).unwrap();
        assert_eq!(elements, (1..=6).map(Element::Int).collect::<Vec<_>>());

        // Through the file it is an error, since there is no block to read.
        assert!(file.read_array(&array).is_err());

        // `read_array_at` dispatches for the caller.
        assert_eq!(file.read_array_at("grid").unwrap().len(), 6);
    }

    #[test]
    fn read_array_at_covers_a_block_backed_array() {
        let values: Vec<i64> = vec![3, 1, 4, 1, 5];
        let mut builder = AsdfBuilder::new();
        builder.set_array_i64("data", &values).unwrap();
        let file = round_trip(&builder);

        assert_eq!(
            file.read_array_at("data").unwrap(),
            values.iter().map(|v| Element::Int(*v)).collect::<Vec<_>>()
        );
        assert!(file.read_array_at("missing").is_err());
    }

    #[test]
    fn an_external_array_is_followed_to_the_neighbouring_file() {
        let dir = std::env::temp_dir().join(format!("asdf-api-exploded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // The data file, written with our own builder.
        let values: Vec<i64> = vec![10, 20, 30, 40];
        let mut holder = AsdfBuilder::new();
        holder.set_array_i64("data", &values).unwrap();
        holder.write_to_path(dir.join("split0000.asdf")).unwrap();

        // The referring file, whose array names it.
        let referring = format!(
            "#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n\
%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
data: !core/ndarray-1.1.0\n  source: split0000.asdf\n  datatype: int64\n  \
byteorder: little\n  shape: [{}]\n...\n",
            values.len()
        );
        let path = dir.join("split.asdf");
        std::fs::write(&path, referring).unwrap();

        let file = AsdfFile::open(&path).unwrap();
        assert_eq!(file.block_count(), 0, "the referring file has no blocks of its own");
        assert_eq!(file.read_array_i64_at("data").unwrap(), values);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_external_array_read_from_memory_is_refused() {
        // A file held in memory has no directory to resolve the name
        // against, so following it would mean guessing at the working
        // directory. The error says so rather than reporting "not found".
        let bytes = b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n\
%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
data: !core/ndarray-1.1.0\n  source: elsewhere.asdf\n  datatype: int64\n  shape: [2]\n\
...\n"
            .to_vec();
        let file = AsdfFile::from_bytes(bytes).unwrap();
        let err = file.read_array_at("data").unwrap_err();
        assert!(err.message().contains("not read from disk"), "{}", err.message());
    }

    #[test]
    fn an_external_array_may_not_escape_its_directory() {
        let dir = std::env::temp_dir().join(format!("asdf-api-escape-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nosy.asdf");
        std::fs::write(
            &path,
            "#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n\
%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
data: !core/ndarray-1.1.0\n  source: ../../../etc/passwd\n  datatype: int64\n  shape: [2]\n\
...\n",
        )
        .unwrap();

        let file = AsdfFile::open(&path).unwrap();
        let err = file.read_array_at("data").unwrap_err();
        assert!(err.message().contains("climbs out"), "{}", err.message());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writes_and_reads_scalars() {
        let mut builder = AsdfBuilder::new();
        builder.set_str("name", "Dennis Richie").unwrap();
        builder.set_i64("foo", 42).unwrap();
        builder.set_u64("big", 5_000_000_000).unwrap();
        builder.set_f64("ratio", 1.5).unwrap();
        builder.set_bool("flag", true).unwrap();
        builder.set_null("nothing").unwrap();

        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();

        assert_eq!(tree.get("name").unwrap().as_str(), Some("Dennis Richie"));
        assert_eq!(tree.get("foo").unwrap().as_i64(), Some(42));
        assert_eq!(tree.get("big").unwrap().as_u64(), Some(5_000_000_000));
        assert_eq!(tree.get("ratio").unwrap().as_f64(), Some(1.5));
        assert_eq!(tree.get("flag").unwrap().as_bool(), Some(true));
        assert!(tree.get("nothing").unwrap().is_null());
        assert!(tree.get("missing").is_none());
    }

    #[test]
    fn a_numeric_string_stays_a_string() {
        let mut builder = AsdfBuilder::new();
        builder.set_str("version", "42").unwrap();

        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();
        let value = tree.get("version").unwrap();
        assert_eq!(value.as_str(), Some("42"), "quoting was lost");
        assert_eq!(value.as_i64(), None, "a string must not read as an integer");
    }

    #[test]
    fn nested_paths_are_materialised() {
        let mut builder = AsdfBuilder::new();
        builder.set_i64("meta/observation/exposure", 300).unwrap();

        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();
        assert_eq!(tree.get("meta/observation/exposure").unwrap().as_i64(), Some(300));
        assert!(tree.get("meta").unwrap().is_mapping());
    }

    #[test]
    fn writes_and_reads_arrays() {
        let squares: Vec<u64> = (0..100u64).map(|i| i * i).collect();
        let mut builder = AsdfBuilder::new();
        builder.set_array_u64("powers/squares", &squares).unwrap();

        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();

        let value = tree.get("powers/squares").unwrap();
        assert!(value.has_tag("core/ndarray"));

        let array = value.as_ndarray().unwrap();
        let read_back = file.read_array_i64(&array).unwrap();
        assert_eq!(read_back.len(), 100);
        assert_eq!(read_back[10], 100);
        assert_eq!(read_back.iter().sum::<i64>(), squares.iter().sum::<u64>() as i64);
    }

    #[test]
    fn writes_and_reads_float_arrays() {
        let values: Vec<f64> = (0..50).map(|i| f64::from(i) * 0.25).collect();
        let mut builder = AsdfBuilder::new();
        builder.set_array_f64("data", &values).unwrap();

        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();
        let array = tree.get("data").unwrap().as_ndarray().unwrap();
        assert_eq!(file.read_array_f64(&array).unwrap(), values);
    }

    #[test]
    fn multi_dimensional_arrays_keep_their_shape() {
        let values: Vec<f64> = (0..12).map(f64::from).collect();
        let mut builder = AsdfBuilder::new();
        builder.set_array_f64_shaped("image", &values, &[3, 4]).unwrap();

        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();
        let array = tree.get("image").unwrap().as_ndarray().unwrap();

        assert_eq!(array.resolved_shape(None).unwrap(), vec![3, 4]);
        assert_eq!(file.read_array_f64(&array).unwrap(), values);
    }

    #[test]
    fn a_shape_that_does_not_match_the_data_is_refused() {
        let mut builder = AsdfBuilder::new();
        let err = builder.set_array_f64_shaped("image", &[1.0, 2.0], &[3, 4]).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn arrays_can_be_compressed() {
        for compression in asdf_core::compression::available() {
            let values: Vec<u64> = (0..1000u64).map(|i| i % 7).collect();
            let mut builder = AsdfBuilder::new().with_compression(compression);
            builder.set_array_u64("data", &values).unwrap();

            let file = round_trip(&builder);
            assert_eq!(file.block_compression(0).unwrap(), compression);
            assert_eq!(file.verify_block(0).unwrap(), ChecksumStatus::Valid);

            let tree = file.tree().unwrap().unwrap();
            let array = tree.get("data").unwrap().as_ndarray().unwrap();
            let read_back = file.read_array_i64(&array).unwrap();
            assert_eq!(read_back.len(), values.len(), "{compression:?}");
            assert_eq!(read_back[3], 3, "{compression:?}");
        }
    }

    #[test]
    fn iterates_mappings_and_sequences() {
        let mut builder = AsdfBuilder::new();
        builder.set_i64("a", 1).unwrap();
        builder.set_i64("b", 2).unwrap();
        builder.set_i64("c", 3).unwrap();

        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();
        let root = tree.root().unwrap();

        // `asdf_library` is appended by the writer, stamping the file with
        // what wrote it, so it comes last.
        let keys: Vec<&str> = root.entries().map(|(k, _)| k).collect();
        assert_eq!(keys, ["a", "b", "c", "asdf_library"], "insertion order must survive");

        let values: Vec<i64> = root.entries().filter_map(|(_, v)| v.as_i64()).collect();
        assert_eq!(values, [1, 2, 3]);
    }

    /// Every file we write says what wrote it. Readers act on that: the
    /// workaround for the Python checksum bug keys off exactly this field.
    #[test]
    fn written_files_record_what_wrote_them() {
        let builder = AsdfBuilder::new();
        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();

        let library = tree.get("asdf_library").expect("asdf_library");
        assert!(library.has_tag("core/software"));
        assert_eq!(library.get("name").and_then(|v| v.as_str()), Some("libasdf-rs"));
        assert!(library.get("version").and_then(|v| v.as_str()).is_some());
        assert!(library.get("homepage").and_then(|v| v.as_str()).is_some());
    }

    /// A tree that already names its writer keeps it -- rewriting someone
    /// else's file must not claim authorship of it.
    #[test]
    fn an_existing_asdf_library_is_left_alone() {
        let source = "#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n\
%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
asdf_library: !core/software-1.0.0 {name: asdf, version: 4.1.0}\n\
x: 1\n...\n";
        let original = AsdfFile::from_bytes(source.as_bytes().to_vec()).unwrap();
        let tree = original.tree().unwrap().unwrap();

        let mut builder = AsdfBuilder::new();
        *builder.document_mut() = tree.document().clone();
        let rewritten = round_trip(&builder);

        let tree = rewritten.tree().unwrap().unwrap();
        let library = tree.get("asdf_library").unwrap();
        assert_eq!(library.get("name").and_then(|v| v.as_str()), Some("asdf"));
    }

    #[test]
    fn sequences_index_forwards_and_backwards() {
        let doc = yaml::parse_document("s: [10, 20, 30]\n").unwrap();
        let tree = Tree { document: doc };
        let seq = tree.get("s").unwrap();

        assert_eq!(seq.len(), Some(3));
        assert_eq!(seq.at(0).unwrap().as_i64(), Some(10));
        assert_eq!(seq.at(-1).unwrap().as_i64(), Some(30));
        assert!(seq.at(3).is_none());

        let all: Vec<i64> = seq.items().filter_map(|v| v.as_i64()).collect();
        assert_eq!(all, [10, 20, 30]);
    }

    #[test]
    fn aliases_are_visible_and_resolve() {
        let doc = yaml::parse_document("shared: &a {x: 1}\nother: *a\n").unwrap();
        let tree = Tree { document: doc };

        let other = tree.get("other").unwrap();
        assert!(other.is_alias());
        // Reading through the alias sees the shared value.
        assert_eq!(other.get("x").unwrap().as_i64(), Some(1));
        assert_eq!(tree.get("other/x").unwrap().as_i64(), Some(1));
    }

    #[test]
    fn tags_are_matched_without_their_version() {
        let doc = yaml::parse_document(
            "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
             d: !core/ndarray-1.0.0\n  source: 0\n...\n",
        )
        .unwrap();
        let tree = Tree { document: doc };
        let value = tree.get("d").unwrap();
        assert!(value.has_tag("core/ndarray"));
        assert!(!value.has_tag("core/software"));
        assert_eq!(value.tag().unwrap().full(), "tag:stsci.edu:asdf/core/ndarray-1.0.0");
    }

    #[test]
    fn trees_render_back_to_yaml() {
        let mut builder = AsdfBuilder::new();
        builder.set_i64("foo", 42).unwrap();

        let file = round_trip(&builder);
        let tree = file.tree().unwrap().unwrap();
        let text = tree.to_yaml().unwrap();
        assert!(text.contains("foo: 42"), "{text}");
        assert!(text.starts_with("%YAML 1.1"), "{text}");
    }

    #[test]
    fn value_equality_ignores_presentation() {
        let a = Tree { document: yaml::parse_document("a: {x: 1, y: 2}\n").unwrap() };
        let b = Tree { document: yaml::parse_document("a:\n  x: 1\n  y: 2\n").unwrap() };
        assert!(a.value_eq(&b));

        let c = Tree { document: yaml::parse_document("a: {x: 1, y: 3}\n").unwrap() };
        assert!(!a.value_eq(&c));
    }

    #[test]
    fn versions_are_reported() {
        let builder = AsdfBuilder::new();
        let file = round_trip(&builder);
        assert_eq!(file.format_version().triple(), (1, 0, 0));
        assert_eq!(file.standard_version().unwrap().triple(), (1, 6, 0));
    }

    #[test]
    fn raw_blocks_round_trip() {
        let mut builder = AsdfBuilder::new();
        let index = builder.add_block(b"arbitrary bytes".to_vec());
        assert_eq!(index, 0);

        let file = round_trip(&builder);
        assert_eq!(file.block_count(), 1);
        assert_eq!(&*file.block_data(0).unwrap(), b"arbitrary bytes");
    }
}
