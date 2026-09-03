//! Writing ASDF files.
//!
//! The layout is assembled in the order the standard prescribes: the `#ASDF`
//! header line, the `#ASDF_STANDARD` comment, the YAML tree, the binary
//! blocks, and finally the block index.

use std::io::Write;
use std::path::Path;

use asdf_yaml::{Document, EmitOptions, TagHandle, emit_with};

use crate::block::header::{BlockHeader, CHECKSUM_SIZE};
use crate::compression::Compression;
use crate::error::{Result, err};
use crate::layout::write_block_index;
use crate::version::{ASDF_FORMAT_VERSION, ASDF_STANDARD_VERSION};

/// A block queued for writing.
#[derive(Clone, Debug)]
pub struct PendingBlock {
    /// The uncompressed data.
    pub data: Vec<u8>,
    /// How to compress it on the way out.
    pub compression: Compression,
    /// Space to reserve, which may exceed the used size so the block can grow
    /// later without moving everything after it. Zero means "use the used
    /// size".
    pub allocated_size: u64,
}

impl PendingBlock {
    /// A block of uncompressed data.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, compression: Compression::None, allocated_size: 0 }
    }

    /// A block compressed with the given method.
    pub fn compressed(data: Vec<u8>, compression: Compression) -> Self {
        Self { data, compression, allocated_size: 0 }
    }
}

/// How to write a file.
#[derive(Clone, Debug)]
pub struct WriteOptions {
    /// The version written on the `#ASDF` header line.
    pub format_version: String,
    /// The version written on the `#ASDF_STANDARD` comment line.
    pub standard_version: String,
    /// Write a block index after the last block.
    pub write_block_index: bool,
    /// Compute and store each block's MD5 checksum.
    pub write_checksums: bool,
    /// How the YAML tree is laid out.
    pub emit: EmitOptions,
    /// Bytes of padding between the tree and the first block, so the tree can
    /// grow later without rewriting the whole file.
    pub tree_padding: usize,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            format_version: ASDF_FORMAT_VERSION.to_string(),
            standard_version: ASDF_STANDARD_VERSION.to_string(),
            write_block_index: true,
            write_checksums: true,
            emit: EmitOptions::default(),
            tree_padding: 0,
        }
    }
}

/// Assembles an ASDF file.
#[derive(Debug)]
pub struct Writer {
    document: Option<Document>,
    blocks: Vec<PendingBlock>,
    options: WriteOptions,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    /// A writer with no tree and no blocks.
    pub fn new() -> Self {
        Self { document: None, blocks: Vec::new(), options: WriteOptions::default() }
    }

    /// A writer for an existing tree.
    pub fn from_document(document: Document) -> Self {
        Self { document: Some(document), blocks: Vec::new(), options: WriteOptions::default() }
    }

    /// Replace the write options.
    pub fn with_options(mut self, options: WriteOptions) -> Self {
        self.options = options;
        self
    }

    /// Set the tree to write.
    pub fn set_document(&mut self, document: Document) {
        self.document = Some(document);
    }

    /// The tree being written, if any.
    pub fn document(&self) -> Option<&Document> {
        self.document.as_ref()
    }

    /// The tree being written, for mutation.
    pub fn document_mut(&mut self) -> Option<&mut Document> {
        self.document.as_mut()
    }

    /// Queue a block, returning the index it will have in the file.
    ///
    /// That index is what an ndarray's `source` refers to.
    pub fn add_block(&mut self, block: PendingBlock) -> usize {
        self.blocks.push(block);
        self.blocks.len() - 1
    }

    /// The number of blocks queued.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Assemble the whole file in memory.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();

        // Header line, then the standard version as a comment.
        out.extend_from_slice(b"#ASDF ");
        out.extend_from_slice(self.options.format_version.as_bytes());
        out.push(b'\n');
        out.extend_from_slice(b"#ASDF_STANDARD ");
        out.extend_from_slice(self.options.standard_version.as_bytes());
        out.push(b'\n');

        if let Some(doc) = &self.document {
            // A tree needs the directives and the `...` terminator: a reader
            // finds where the tree ends by searching for the latter.
            let mut options = self.options.emit.clone();
            options.directives = true;
            options.explicit_start = true;
            options.explicit_end = true;

            let mut doc = doc.clone();
            if doc.version.is_none() {
                doc.version = Some(asdf_yaml::YamlVersion::V1_1);
            }
            if doc.tag_handles.is_empty() {
                doc.tag_handles.push(TagHandle::asdf_default());
            }

            let text = emit_with(&doc, &options)
                .map_err(|e| err!(YamlParseFailed, "could not emit the tree: {e}"))?;
            out.extend_from_slice(text.as_bytes());
        }

        // Optional padding, so the tree can grow without moving the blocks.
        // The standard recommends spaces, which read as empty space.
        if !self.blocks.is_empty() && self.options.tree_padding > 0 {
            out.resize(out.len() + self.options.tree_padding, b' ');
            out.push(b'\n');
        }

        let mut offsets = Vec::with_capacity(self.blocks.len());
        for block in &self.blocks {
            offsets.push(out.len() as u64);
            self.write_block(&mut out, block)?;
        }

        // The index is forbidden when there are no blocks, and pointless
        // when it was turned off.
        if self.options.write_block_index && !offsets.is_empty() {
            out.extend_from_slice(&write_block_index(&offsets));
        }
        Ok(out)
    }

    /// Append one block's header and data.
    fn write_block(&self, out: &mut Vec<u8>, block: &PendingBlock) -> Result<()> {
        let stored = block.compression.compress(&block.data)?;

        let used_size = stored.len() as u64;
        let allocated_size = block.allocated_size.max(used_size);

        let mut header = BlockHeader {
            allocated_size,
            used_size,
            data_size: block.data.len() as u64,
            ..Default::default()
        };
        header.set_compression(block.compression.name())?;

        if self.options.write_checksums {
            // The specification means the digest to cover the used data as
            // stored, which for a compressed block is the compressed bytes.
            header.checksum = md5_of(&stored);
        }

        header.write(out);
        out.extend_from_slice(&stored);

        // Reserved-but-unused space is left as zeros; the standard does not
        // constrain its contents.
        let padding = allocated_size - used_size;
        out.resize(out.len() + padding as usize, 0);
        Ok(())
    }

    /// Write the file to a stream.
    pub fn write_to(&self, sink: &mut impl Write) -> Result<()> {
        let bytes = self.to_bytes()?;
        sink.write_all(&bytes)?;
        Ok(())
    }

    /// Write the file to a path, replacing anything already there.
    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let bytes = self.to_bytes()?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

fn md5_of(data: &[u8]) -> [u8; CHECKSUM_SIZE] {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{ChecksumStatus, Reader};
    use asdf_yaml::{CompareOptions, Tag, compare, parse_document};

    fn tree(yaml: &str) -> Document {
        parse_document(yaml).unwrap()
    }

    #[test]
    fn writes_a_readable_file() {
        let doc =
            tree("%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\nfoo: 42\n...\n");
        let writer = Writer::from_document(doc);
        let bytes = writer.to_bytes().unwrap();

        let reader = Reader::from_bytes(bytes).unwrap();
        assert_eq!(reader.layout().format_version.triple(), (1, 0, 0));
        assert_eq!(reader.layout().standard_version.as_ref().unwrap().triple(), (1, 6, 0));

        let read_back = reader.tree().unwrap().unwrap();
        let root = read_back.root().unwrap();
        assert_eq!(read_back.tag_of(root).unwrap().full(), "tag:stsci.edu:asdf/core/asdf-1.1.0");
        let foo = read_back.mapping_get(root, "foo").unwrap();
        assert_eq!(read_back.node(foo).as_str(), Some("42"));
    }

    #[test]
    fn a_written_tree_reads_back_equal() {
        let source = "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
                      name: Dennis Richie\nfoo: 42\nnested:\n  a: [1, 2, 3]\n\
                      shared: &x {p: 1}\nalias: *x\n...\n";
        let original = tree(source);
        let bytes = Writer::from_document(original.clone()).to_bytes().unwrap();

        let reader = Reader::from_bytes(bytes).unwrap();
        let read_back = reader.tree().unwrap().unwrap();
        let result = compare(&original, &read_back, CompareOptions::default());
        assert!(result.is_equal(), "{result}");
    }

    #[test]
    fn writes_blocks_that_read_back_byte_for_byte() {
        let mut writer = Writer::from_document(tree("a: 1\n"));
        let first: Vec<u8> = (0..=255u8).collect();
        let second = b"second block".to_vec();

        assert_eq!(writer.add_block(PendingBlock::new(first.clone())), 0);
        assert_eq!(writer.add_block(PendingBlock::new(second.clone())), 1);

        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        assert_eq!(reader.block_count(), 2);
        assert_eq!(&*reader.block_data(0).unwrap(), &first[..]);
        assert_eq!(&*reader.block_data(1).unwrap(), &second[..]);
    }

    #[test]
    fn written_checksums_verify() {
        let mut writer = Writer::from_document(tree("a: 1\n"));
        writer.add_block(PendingBlock::new(vec![7u8; 1024]));

        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        let (status, _) = reader.verify_block_checksum(0).unwrap();
        assert_eq!(status, ChecksumStatus::Valid);
    }

    #[test]
    fn checksums_can_be_turned_off() {
        let options = WriteOptions { write_checksums: false, ..Default::default() };
        let mut writer = Writer::from_document(tree("a: 1\n")).with_options(options);
        writer.add_block(PendingBlock::new(vec![1u8; 32]));

        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        let (status, _) = reader.verify_block_checksum(0).unwrap();
        assert_eq!(status, ChecksumStatus::Absent);
    }

    #[test]
    fn compressed_blocks_round_trip_through_every_method() {
        for compression in crate::compression::available() {
            let payload: Vec<u8> = (0..4096u32).map(|i| (i % 17) as u8).collect();
            let mut writer = Writer::from_document(tree("a: 1\n"));
            writer.add_block(PendingBlock::compressed(payload.clone(), compression));

            let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
            assert_eq!(reader.block_compression(0).unwrap(), compression);
            assert_eq!(&*reader.block_data(0).unwrap(), &payload[..], "{compression:?}");
            assert_eq!(
                reader.verify_block_checksum(0).unwrap().0,
                ChecksumStatus::Valid,
                "{compression:?}"
            );
            // The stored form really is smaller for this redundant payload.
            assert!(reader.block_raw(0).unwrap().len() < payload.len(), "{compression:?}");
        }
    }

    #[test]
    fn a_written_block_index_is_accepted_on_read_back() {
        let mut writer = Writer::from_document(tree("a: 1\n"));
        writer.add_block(PendingBlock::new(vec![1u8; 64]));
        writer.add_block(PendingBlock::new(vec![2u8; 64]));

        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        assert!(
            reader.layout().used_block_index(),
            "index rejected: {:?}",
            reader.layout().index_rejection
        );
    }

    #[test]
    fn no_index_is_written_when_there_are_no_blocks() {
        // The standard forbids an index in a file with no blocks.
        let writer = Writer::from_document(tree("a: 1\n"));
        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        assert!(reader.layout().block_index_pos.is_none());
    }

    #[test]
    fn the_index_can_be_suppressed() {
        let options = WriteOptions { write_block_index: false, ..Default::default() };
        let mut writer = Writer::from_document(tree("a: 1\n")).with_options(options);
        writer.add_block(PendingBlock::new(vec![3u8; 16]));

        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        assert!(reader.layout().block_index_pos.is_none());
        // ...and the block is still found by skipping along.
        assert_eq!(reader.block_count(), 1);
    }

    #[test]
    fn allocated_size_reserves_room_without_breaking_the_read() {
        let mut writer = Writer::from_document(tree("a: 1\n"));
        let data = vec![9u8; 100];
        writer.add_block(PendingBlock {
            data: data.clone(),
            compression: Compression::None,
            allocated_size: 4096,
        });
        writer.add_block(PendingBlock::new(vec![8u8; 10]));

        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        assert_eq!(reader.block(0).unwrap().header.allocated_size, 4096);
        assert_eq!(reader.block(0).unwrap().header.used_size, 100);
        // The second block must be found past the first's reserved space.
        assert_eq!(&*reader.block_data(0).unwrap(), &data[..]);
        assert_eq!(&*reader.block_data(1).unwrap(), &[8u8; 10][..]);
        assert!(reader.layout().used_block_index());
    }

    #[test]
    fn tree_padding_does_not_confuse_the_reader() {
        let options = WriteOptions { tree_padding: 512, ..Default::default() };
        let mut writer = Writer::from_document(tree("a: 1\n")).with_options(options);
        writer.add_block(PendingBlock::new(vec![5u8; 32]));

        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        assert_eq!(reader.block_count(), 1);
        assert_eq!(&*reader.block_data(0).unwrap(), &[5u8; 32][..]);
        assert!(reader.tree().unwrap().is_some());
    }

    #[test]
    fn a_file_with_no_tree_is_valid() {
        // Exploded form: header straight into blocks.
        let mut writer = Writer::new();
        writer.add_block(PendingBlock::new(b"just data".to_vec()));

        let reader = Reader::from_bytes(writer.to_bytes().unwrap()).unwrap();
        assert!(reader.tree().unwrap().is_none());
        assert_eq!(&*reader.block_data(0).unwrap(), b"just data");
    }

    #[test]
    fn directives_are_supplied_when_the_tree_lacks_them() {
        // A document built in memory has no directives; the writer must add
        // them, since the format requires them.
        let mut doc = Document::new();
        let k = doc.add_scalar("foo");
        let v = doc.add_scalar("1");
        let root = doc.add_mapping(vec![(k, v)]);
        doc.node_mut(root).tag = Some(Tag::parse("tag:stsci.edu:asdf/core/asdf-1.1.0"));
        doc.set_root(root);

        let bytes = Writer::from_document(doc).to_bytes().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("%YAML 1.1\n"), "{text}");
        assert!(text.contains("%TAG ! tag:stsci.edu:asdf/\n"), "{text}");
        assert!(text.contains("--- !core/asdf-1.1.0\n"), "{text}");

        let reader = Reader::from_bytes(bytes).unwrap();
        assert!(reader.tree().unwrap().is_some());
    }

    #[test]
    fn writes_to_a_path() {
        let dir = std::env::temp_dir().join(format!("asdf-writer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.asdf");

        let mut writer = Writer::from_document(tree("a: 1\n"));
        writer.add_block(PendingBlock::new(b"payload".to_vec()));
        writer.write_to_path(&path).unwrap();

        let reader = Reader::open(&path).unwrap();
        assert_eq!(&*reader.block_data(0).unwrap(), b"payload");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
