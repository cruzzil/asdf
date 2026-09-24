//! A generator of *well-formed* ASDF files with hostile contents.
//!
//! The other two targets mutate raw bytes. Even with `asdf.dict` that spends
//! most of its budget on the first few dozen bytes of the file: the header
//! has to be right, then the YAML has to parse, then the block headers have
//! to be self-consistent, and only then does anything interesting run. The
//! parts of this library with the most logic in them -- shape and stride
//! arithmetic, datatype conversion, the alias graph, block framing -- sit
//! behind all of that.
//!
//! So this target does not mutate the file. It mutates a *description* of a
//! file and then renders a valid one, which means every input reaches the
//! code the previous eight findings lived in. The framing is always
//! plausible; what varies is the part a real writer would get right and an
//! attacker would not.
//!
//! The values it draws from are deliberately weighted towards edges --
//! shapes whose product wraps, strides that address backwards, offsets past
//! the block, sizes that disagree with each other -- because a uniform
//! `u64` is never `1 << 61` in practice.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use asdf_core::Reader;
use asdf_core::core::elements::{decode_all, decode_inline};
use asdf_core::core::ndarray::{Ndarray, Source};
use asdf_core::info::{InfoOptions, render};

/// A dimension, drawn so that the interesting ones come up often.
#[derive(Arbitrary, Debug)]
enum Dim {
    Small(u8),
    /// The values around which `u64` multiplication wraps.
    Boundary(BoundaryDim),
    Raw(u64),
}

#[derive(Arbitrary, Debug)]
enum BoundaryDim {
    Zero,
    One,
    Pow31,
    Pow32,
    Pow61,
    Pow62,
    Pow63,
    Pow63Plus1,
    Max,
}

impl Dim {
    fn value(&self) -> u64 {
        match self {
            Dim::Small(n) => u64::from(*n),
            Dim::Raw(n) => *n,
            Dim::Boundary(b) => match b {
                BoundaryDim::Zero => 0,
                BoundaryDim::One => 1,
                BoundaryDim::Pow31 => 1 << 31,
                BoundaryDim::Pow32 => 1 << 32,
                BoundaryDim::Pow61 => 1 << 61,
                BoundaryDim::Pow62 => 1 << 62,
                BoundaryDim::Pow63 => 1 << 63,
                BoundaryDim::Pow63Plus1 => (1 << 63) + 1,
                BoundaryDim::Max => u64::MAX,
            },
        }
    }
}

#[derive(Arbitrary, Debug)]
enum Datatype {
    Int8,
    Int16,
    Int32,
    Int64,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Float16,
    Float32,
    Float64,
    Complex64,
    Complex128,
    Bool8,
    /// A width the reader has to reject rather than trust.
    Bogus,
}

impl Datatype {
    fn name(&self) -> &'static str {
        match self {
            Datatype::Int8 => "int8",
            Datatype::Int16 => "int16",
            Datatype::Int32 => "int32",
            Datatype::Int64 => "int64",
            Datatype::Uint8 => "uint8",
            Datatype::Uint16 => "uint16",
            Datatype::Uint32 => "uint32",
            Datatype::Uint64 => "uint64",
            Datatype::Float16 => "float16",
            Datatype::Float32 => "float32",
            Datatype::Float64 => "float64",
            Datatype::Complex64 => "complex64",
            Datatype::Complex128 => "complex128",
            Datatype::Bool8 => "bool8",
            Datatype::Bogus => "not_a_datatype",
        }
    }
}

#[derive(Arbitrary, Debug)]
enum Compression {
    None,
    Zlib,
    Bzp2,
    Lz4,
    /// A name no codec answers to.
    Unknown,
}

impl Compression {
    fn field(&self) -> &'static [u8] {
        match self {
            Compression::None => b"",
            Compression::Zlib => b"zlib",
            Compression::Bzp2 => b"bzp2",
            Compression::Lz4 => b"lz4",
            Compression::Unknown => b"zzzz",
        }
    }
}

/// One binary block, with every size field independently chosen: a real
/// writer keeps them consistent, so disagreement is the whole point.
#[derive(Arbitrary, Debug)]
struct Block {
    payload: Vec<u8>,
    compression: Compression,
    /// Compress the payload for real before writing it, so the codecs get
    /// streams they can actually start decoding.
    really_compress: bool,
    allocated: Option<u32>,
    used: Option<u32>,
    data_size: Option<u64>,
    streamed: bool,
    checksum: bool,
}

/// What the tree says about an array.
#[derive(Arbitrary, Debug)]
struct ArrayDecl {
    shape: Vec<Dim>,
    datatype: Datatype,
    big_endian: bool,
    offset: Option<u64>,
    strides: Option<Vec<i64>>,
    /// Point at a block index, or inline the data, or name a `*` dimension.
    source: SourceKind,
    mask_value: bool,
}

#[derive(Arbitrary, Debug)]
enum SourceKind {
    Block(u8),
    Inline(Vec<i8>),
    Star,
}

/// The tree's shape, including the alias graph that three findings lived in.
#[derive(Arbitrary, Debug)]
enum TreeNode {
    Scalar(ScalarKind),
    Array(ArrayDecl),
    Seq(Vec<TreeNode>),
    Map(Vec<(String, TreeNode)>),
    /// Define an anchor over a node.
    Anchor(u8, Box<TreeNode>),
    /// Refer to one, including one that may not exist or may be an ancestor.
    Alias(u8),
}

#[derive(Arbitrary, Debug)]
enum ScalarKind {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Inf,
    NegInf,
    Nan,
    BareInf,
    Text(String),
    Empty,
}

#[derive(Arbitrary, Debug)]
struct File {
    tree: TreeNode,
    blocks: Vec<Block>,
    write_index: bool,
    /// A block index whose offsets are wrong, which the standard tells a
    /// reader to detect rather than trust.
    corrupt_index: bool,
}

/// Render a scalar as the text a writer would emit.
fn scalar_text(s: &ScalarKind) -> String {
    match s {
        ScalarKind::Null => "null".into(),
        ScalarKind::Bool(b) => b.to_string(),
        ScalarKind::Int(i) => i.to_string(),
        ScalarKind::Float(f) if f.is_finite() => format!("{f:?}"),
        ScalarKind::Float(_) => ".nan".into(),
        ScalarKind::Inf => ".inf".into(),
        ScalarKind::NegInf => "-.inf".into(),
        ScalarKind::Nan => ".nan".into(),
        ScalarKind::BareInf => "inf".into(),
        // Keep it on one line and free of the characters that would make the
        // document unparseable; the point is a hostile *tree*, not hostile
        // YAML lexing, which the byte-level targets already cover.
        ScalarKind::Text(t) => {
            let cleaned: String =
                t.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').take(40).collect();
            format!("\"{cleaned}\"")
        }
        ScalarKind::Empty => "\"\"".into(),
    }
}

fn write_array(out: &mut String, indent: usize, a: &ArrayDecl) {
    let pad = "  ".repeat(indent);
    out.push_str("!core/ndarray-1.1.0\n");
    match &a.source {
        SourceKind::Block(i) => {
            out.push_str(&format!("{pad}source: {i}\n"));
        }
        SourceKind::Inline(values) => {
            let items: Vec<String> =
                values.iter().take(32).map(std::string::ToString::to_string).collect();
            out.push_str(&format!("{pad}data: [{}]\n", items.join(", ")));
        }
        SourceKind::Star => {
            out.push_str(&format!("{pad}source: 0\n"));
        }
    }
    out.push_str(&format!("{pad}datatype: {}\n", a.datatype.name()));
    out.push_str(&format!("{pad}byteorder: {}\n", if a.big_endian { "big" } else { "little" }));

    let mut dims: Vec<String> = a.shape.iter().take(8).map(|d| d.value().to_string()).collect();
    if matches!(a.source, SourceKind::Star) && !dims.is_empty() {
        dims[0] = "'*'".into();
    }
    if !dims.is_empty() {
        out.push_str(&format!("{pad}shape: [{}]\n", dims.join(", ")));
    }
    if let Some(o) = a.offset {
        out.push_str(&format!("{pad}offset: {o}\n"));
    }
    if let Some(s) = &a.strides
        && !s.is_empty()
    {
        let items: Vec<String> = s.iter().take(8).map(std::string::ToString::to_string).collect();
        out.push_str(&format!("{pad}strides: [{}]\n", items.join(", ")));
    }
    if a.mask_value {
        out.push_str(&format!("{pad}mask: 0\n"));
    }
}

fn write_node(out: &mut String, indent: usize, node: &TreeNode, budget: &mut u32) {
    if *budget == 0 {
        out.push_str("null\n");
        return;
    }
    *budget -= 1;
    let pad = "  ".repeat(indent);

    match node {
        TreeNode::Scalar(s) => {
            out.push_str(&scalar_text(s));
            out.push('\n');
        }
        TreeNode::Array(a) => write_array(out, indent + 1, a),
        TreeNode::Alias(n) => {
            out.push_str(&format!("*a{}\n", n % 8));
        }
        TreeNode::Anchor(n, inner) => {
            out.push_str(&format!("&a{} ", n % 8));
            write_node(out, indent, inner, budget);
        }
        TreeNode::Seq(items) => {
            if items.is_empty() {
                out.push_str("[]\n");
                return;
            }
            out.push('\n');
            for item in items.iter().take(8) {
                out.push_str(&format!("{pad}  - "));
                write_node(out, indent + 2, item, budget);
            }
        }
        TreeNode::Map(entries) => {
            if entries.is_empty() {
                out.push_str("{}\n");
                return;
            }
            out.push('\n');
            for (index, (key, value)) in entries.iter().take(8).enumerate() {
                let safe: String = key
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .take(16)
                    .collect();
                let safe = if safe.is_empty() { format!("k{index}") } else { safe };
                out.push_str(&format!("{pad}  {safe}: "));
                write_node(out, indent + 2, value, budget);
            }
        }
    }
}

/// Assemble the bytes of a file that a writer could plausibly have produced.
fn render_file(spec: &File) -> Vec<u8> {
    let mut tree = String::new();
    let mut budget = 200u32;
    tree.push_str("--- !core/asdf-1.1.0\n");
    // The root must be a mapping for the tree to be an ASDF tree at all.
    tree.push_str("root: ");
    write_node(&mut tree, 0, &spec.tree, &mut budget);

    let mut out =
        b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n".to_vec();
    out.extend_from_slice(tree.as_bytes());
    out.extend_from_slice(b"...\n");

    let mut offsets = Vec::new();
    for block in spec.blocks.iter().take(4) {
        offsets.push(out.len() as u64);

        let stored: Vec<u8> = if block.really_compress {
            let comp = match block.compression {
                Compression::Zlib => asdf_core::compression::Compression::Zlib,
                Compression::Bzp2 => asdf_core::compression::Compression::Bzp2,
                Compression::Lz4 => asdf_core::compression::Compression::Lz4,
                _ => asdf_core::compression::Compression::None,
            };
            comp.compress(&block.payload).unwrap_or_else(|_| block.payload.clone())
        } else {
            block.payload.clone()
        };

        let mut header = [0u8; 48];
        let flags: u32 = if block.streamed { 1 } else { 0 };
        header[0..4].copy_from_slice(&flags.to_be_bytes());
        let name = block.compression.field();
        header[4..4 + name.len()].copy_from_slice(name);

        let allocated = block.allocated.map_or(stored.len() as u64, u64::from);
        let used = block.used.map_or(stored.len() as u64, u64::from);
        let data_size = block.data_size.unwrap_or(block.payload.len() as u64);
        header[8..16].copy_from_slice(&allocated.to_be_bytes());
        header[16..24].copy_from_slice(&used.to_be_bytes());
        header[24..32].copy_from_slice(&data_size.to_be_bytes());
        if block.checksum {
            header[32..48].copy_from_slice(&[0xAB; 16]);
        }

        out.extend_from_slice(b"\xd3BLK\x00\x30");
        out.extend_from_slice(&header);
        out.extend_from_slice(&stored);
    }

    if spec.write_index && !offsets.is_empty() {
        out.extend_from_slice(b"#ASDF BLOCK INDEX\n%YAML 1.1\n---\n");
        for offset in &offsets {
            let value = if spec.corrupt_index { offset.wrapping_add(7) } else { *offset };
            out.extend_from_slice(format!("- {value}\n").as_bytes());
        }
        out.extend_from_slice(b"...\n");
    }

    out
}

fuzz_target!(|spec: File| {
    let bytes = render_file(&spec);

    let Ok(reader) = Reader::from_bytes(bytes) else { return };

    for index in 0..reader.block_count() {
        let _ = reader.block_raw(index);
        let _ = reader.block_data(index);
        let _ = reader.verify_block_checksum(index);
    }
    let _ = reader.tree_inlined();
    let _ = render(&reader, InfoOptions::default());

    let Ok(Some(doc)) = reader.tree() else { return };
    let Some(root) = doc.root() else { return };

    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(id) = stack.pop() {
        visited += 1;
        if visited > 2048 {
            return;
        }

        if let Ok(nd) = Ndarray::parse(&doc, id) {
            let block_bytes = match nd.source {
                Source::Block(i) => reader.block_data(i).ok().map(|d| d.len() as u64),
                _ => None,
            };
            let _ = nd.len(block_bytes);
            let _ = nd.nbytes(block_bytes);
            if let Ok(shape) = nd.resolved_shape(block_bytes) {
                match nd.source {
                    Source::Block(i) => {
                        if let Ok(data) = reader.block_data(i) {
                            let _ = decode_all(&nd, &shape, &data);
                        }
                    }
                    Source::Inline(_) => {
                        let _ = decode_inline(&doc, &nd, &shape);
                    }
                    Source::External(_) | Source::LastBlock => {}
                }
            }
        }

        match &doc.node(doc.resolve(id)).data {
            asdf_core::yaml::NodeData::Mapping { entries, .. } => {
                stack.extend(entries.iter().map(|e| e.value));
            }
            asdf_core::yaml::NodeData::Sequence { items, .. } => {
                stack.extend(items.iter().copied());
            }
            _ => {}
        }
    }
});
