//! Reading an ASDF file: its tree, and the data in its binary blocks.

use std::borrow::Cow;
use std::path::Path;

use asdf_yaml::{Document, parse_document};

use crate::block::header::CHECKSUM_SIZE;
use crate::compression::Compression;
use crate::error::{Result, err};
use crate::layout::{BlockLocation, Layout, scan};

/// Where a reader's bytes come from.
enum Source {
    /// A memory-mapped file. Block data is read straight out of the mapping,
    /// so a large array costs no copy until it is decompressed or converted.
    Mapped(memmap2::Mmap),
    /// An in-memory buffer.
    Owned(Vec<u8>),
}

impl std::ops::Deref for Source {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Source::Mapped(m) => m,
            Source::Owned(v) => v,
        }
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Source::Mapped(_) => "Mapped",
            Source::Owned(_) => "Owned",
        };
        write!(f, "{kind}({} bytes)", self.len())
    }
}

/// The outcome of verifying a block's checksum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChecksumStatus {
    /// The header records no checksum; the all-zero value means "do not
    /// verify", so this is not a failure.
    Absent,
    /// The recorded digest matches.
    Valid,
    /// The recorded digest does not match.
    Invalid,
}

impl ChecksumStatus {
    /// Whether this status should be treated as a failure.
    ///
    /// An absent checksum is not one: the standard makes it optional.
    pub fn is_failure(self) -> bool {
        self == ChecksumStatus::Invalid
    }
}

/// An open ASDF file.
#[derive(Debug)]
pub struct Reader {
    source: Source,
    layout: Layout,
}

impl Reader {
    /// Open and scan a file from disk.
    ///
    /// The file is memory-mapped, so block data is not read until it is used.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)?;

        // SAFETY: the only unsafe operation in the engine. Mapping is unsafe
        // because another process truncating the file can turn a later read
        // into SIGBUS. That hazard is inherent to memory-mapping and is the
        // same one libasdf accepts; ASDF files are written whole rather than
        // modified in place, so a concurrent truncation is not a case the
        // format contemplates. Mapping is what lets a multi-gigabyte array be
        // read without loading the file into memory, which is the point of
        // the format.
        #[allow(unsafe_code)]
        let mapped = unsafe { memmap2::Mmap::map(&file) }?;
        let layout = scan(&mapped)?;
        Ok(Self { source: Source::Mapped(mapped), layout })
    }

    /// Scan an in-memory file.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let layout = scan(&bytes)?;
        Ok(Self { source: Source::Owned(bytes), layout })
    }

    /// The whole file's bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.source
    }

    /// The scanned layout.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The YAML tree's text, if the file has a tree.
    pub fn tree_text(&self) -> Option<&str> {
        self.layout.tree_str(&self.source)
    }

    /// Parse the YAML tree.
    ///
    /// A file with no tree -- legitimate in exploded form -- yields `None`.
    pub fn tree(&self) -> Result<Option<Document>> {
        match self.tree_text() {
            None => Ok(None),
            Some(text) => Ok(Some(parse_document(text)?)),
        }
    }

    /// The number of binary blocks.
    pub fn block_count(&self) -> usize {
        self.layout.blocks.len()
    }

    /// A block's location and header.
    pub fn block(&self, index: usize) -> Result<&BlockLocation> {
        self.layout.blocks.get(index).ok_or_else(|| {
            err!(
                InvalidArgument,
                "block index {index} is out of range; the file has {} blocks",
                self.layout.blocks.len()
            )
        })
    }

    /// A block's bytes exactly as stored, without decompressing.
    ///
    /// For an uncompressed block this is the data itself; for a compressed
    /// one it is the compressed form.
    pub fn block_raw(&self, index: usize) -> Result<&[u8]> {
        let block = self.block(index)?;
        let start = usize::try_from(block.data_pos)
            .map_err(|_| err!(UnexpectedEof, "block {index} data offset overflows"))?;

        let len = if block.header.is_streamed() {
            // A streamed block runs to the end of the file; its size fields
            // are meaningless.
            self.source.len().saturating_sub(start)
        } else {
            usize::try_from(block.header.used_size)
                .map_err(|_| err!(UnexpectedEof, "block {index} used_size overflows"))?
        };

        self.source
            .get(start..start + len)
            .ok_or_else(|| err!(UnexpectedEof, "block {index} extends past the end of the file"))
    }

    /// The compression method a block uses.
    pub fn block_compression(&self, index: usize) -> Result<Compression> {
        Compression::from_name(self.block(index)?.header.compression_name())
    }

    /// A block's data, decompressed if necessary.
    ///
    /// An uncompressed block borrows straight from the file with no copy.
    pub fn block_data(&self, index: usize) -> Result<Cow<'_, [u8]>> {
        let raw = self.block_raw(index)?;
        let compression = self.block_compression(index)?;
        if compression == Compression::None {
            return Ok(Cow::Borrowed(raw));
        }
        let expected = usize::try_from(self.block(index)?.header.data_size)
            .map_err(|_| err!(UnexpectedEof, "block {index} data_size overflows"))?;
        Ok(Cow::Owned(compression.decompress(raw, expected)?))
    }

    /// Verify a block's MD5 checksum.
    ///
    /// The returned digest is what the data actually hashes to, which is
    /// useful for reporting a mismatch.
    ///
    /// # The Python asdf compatibility case
    ///
    /// For a *compressed* block, the specification means the checksum to
    /// cover the bytes as stored. Python asdf 5.x and earlier instead
    /// checksum the *uncompressed* data
    /// ([asdf#2015](https://github.com/asdf-format/asdf/issues/2015)).
    /// libasdf works around this by consulting the file's `asdf_library`
    /// metadata, and so do we: see [`Reader::has_python_checksum_bug`]. A
    /// compressed block whose stored bytes do not match is therefore retried
    /// against the decompressed bytes when the writer is known to be affected.
    pub fn verify_block_checksum(
        &self,
        index: usize,
    ) -> Result<(ChecksumStatus, [u8; CHECKSUM_SIZE])> {
        let header = &self.block(index)?.header;
        if !header.has_checksum() {
            return Ok((ChecksumStatus::Absent, [0; CHECKSUM_SIZE]));
        }
        let expected = header.checksum;

        let raw_digest = md5_of(self.block_raw(index)?);
        if raw_digest == expected {
            return Ok((ChecksumStatus::Valid, raw_digest));
        }

        // Only compressed blocks are affected, and only when the writer is
        // one of the versions known to be wrong.
        if self.block_compression(index)? != Compression::None && self.has_python_checksum_bug() {
            let decompressed = md5_of(&self.block_data(index)?);
            if decompressed == expected {
                return Ok((ChecksumStatus::Valid, decompressed));
            }
        }

        Ok((ChecksumStatus::Invalid, raw_digest))
    }

    /// Whether this file was written by a Python asdf version that
    /// checksums compressed blocks incorrectly.
    ///
    /// Matches libasdf's test: the `asdf_library` name is `asdf` and its
    /// major version is 5 or below.
    pub fn has_python_checksum_bug(&self) -> bool {
        const BUGGY_THROUGH_MAJOR: u32 = 5;

        let Ok(Some(doc)) = self.tree() else { return false };
        let Some(root) = doc.root() else { return false };
        let Some(library) = doc.mapping_get(root, "asdf_library") else {
            return false;
        };

        let name = doc
            .mapping_get(library, "name")
            .and_then(|id| doc.resolved(id).as_str().map(str::to_string));
        if name.as_deref() != Some("asdf") {
            return false;
        }

        doc.mapping_get(library, "version")
            .and_then(|id| doc.resolved(id).as_str().map(crate::Version::parse))
            .is_some_and(|v| v.major <= BUGGY_THROUGH_MAJOR)
    }
}

/// MD5 of a buffer.
fn md5_of(data: &[u8]) -> [u8; CHECKSUM_SIZE] {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::header::BlockHeader;
    use crate::layout::write_block_index;

    /// Build a file with one block, optionally compressed and checksummed.
    fn build(payload: &[u8], compression: Compression, checksum_over: Option<&[u8]>) -> Vec<u8> {
        let stored = compression.compress(payload).unwrap();
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(
            b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\nx: 1\n...\n",
        );

        let mut header = BlockHeader {
            allocated_size: stored.len() as u64,
            used_size: stored.len() as u64,
            data_size: payload.len() as u64,
            ..Default::default()
        };
        header.set_compression(compression.name()).unwrap();
        if let Some(over) = checksum_over {
            header.checksum = md5_of(over);
        }

        let offset = buf.len() as u64;
        header.write(&mut buf);
        buf.extend_from_slice(&stored);
        buf.extend_from_slice(&write_block_index(&[offset]));
        buf
    }

    #[test]
    fn reads_tree_and_block_data() {
        let payload = b"hello block data".to_vec();
        let file = build(&payload, Compression::None, None);
        let r = Reader::from_bytes(file).unwrap();

        assert_eq!(r.block_count(), 1);
        assert_eq!(&*r.block_data(0).unwrap(), &payload[..]);

        let doc = r.tree().unwrap().unwrap();
        let root = doc.root().unwrap();
        assert!(doc.mapping_get(root, "x").is_some());
    }

    #[test]
    fn uncompressed_data_is_borrowed_not_copied() {
        let file = build(b"borrow me", Compression::None, None);
        let r = Reader::from_bytes(file).unwrap();
        assert!(matches!(r.block_data(0).unwrap(), Cow::Borrowed(_)));
    }

    #[test]
    fn compressed_data_round_trips() {
        let payload = vec![7u8; 4096];
        for c in crate::compression::available() {
            let file = build(&payload, c, None);
            let r = Reader::from_bytes(file).unwrap();
            assert_eq!(r.block_compression(0).unwrap(), c);
            assert_eq!(&*r.block_data(0).unwrap(), &payload[..], "{c:?}");
            // The raw form is the compressed bytes.
            assert!(r.block_raw(0).unwrap().len() < payload.len(), "{c:?}");
        }
    }

    #[test]
    fn valid_checksums_verify() {
        let payload = b"checksum me".to_vec();
        let file = build(&payload, Compression::None, Some(&payload));
        let r = Reader::from_bytes(file).unwrap();
        let (status, _) = r.verify_block_checksum(0).unwrap();
        assert_eq!(status, ChecksumStatus::Valid);
    }

    #[test]
    fn invalid_checksums_are_reported() {
        let payload = b"checksum me".to_vec();
        let file = build(&payload, Compression::None, Some(b"something else"));
        let r = Reader::from_bytes(file).unwrap();
        let (status, computed) = r.verify_block_checksum(0).unwrap();
        assert_eq!(status, ChecksumStatus::Invalid);
        assert_eq!(computed, md5_of(&payload), "the digest of the real data is reported");
    }

    #[test]
    fn an_absent_checksum_is_not_a_failure() {
        let file = build(b"no checksum", Compression::None, None);
        let r = Reader::from_bytes(file).unwrap();
        let (status, _) = r.verify_block_checksum(0).unwrap();
        assert_eq!(status, ChecksumStatus::Absent);
        assert!(!status.is_failure());
    }

    #[cfg(feature = "zlib")]
    #[test]
    fn compressed_checksums_cover_the_stored_bytes() {
        // What the specification means: the digest is over the data as stored.
        let payload = vec![3u8; 2048];
        let stored = Compression::Zlib.compress(&payload).unwrap();
        let file = build(&payload, Compression::Zlib, Some(&stored));
        let r = Reader::from_bytes(file).unwrap();
        assert_eq!(r.verify_block_checksum(0).unwrap().0, ChecksumStatus::Valid);
    }

    /// Python asdf 5.x and earlier checksum the *uncompressed* data for a
    /// compressed block. libasdf detects those writers from `asdf_library`
    /// and verifies against the decompressed bytes instead; so do we.
    #[cfg(feature = "zlib")]
    #[test]
    fn the_python_checksum_bug_is_worked_around() {
        let payload = vec![9u8; 2048];
        let stored = Compression::Zlib.compress(&payload).unwrap();

        let make = |library_version: &str| {
            let mut buf = Vec::new();
            buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
            buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
            buf.extend_from_slice(
                format!(
                    "asdf_library: !core/software-1.0.0 {{name: asdf, version: {library_version}}}\n"
                )
                .as_bytes(),
            );
            buf.extend_from_slice(b"...\n");

            let mut header = BlockHeader {
                allocated_size: stored.len() as u64,
                used_size: stored.len() as u64,
                data_size: payload.len() as u64,
                // The bug: digest taken over the *uncompressed* payload.
                checksum: md5_of(&payload),
                ..Default::default()
            };
            header.set_compression("zlib").unwrap();
            header.write(&mut buf);
            buf.extend_from_slice(&stored);
            buf
        };

        // A writer known to be affected: accepted via the workaround.
        let r = Reader::from_bytes(make("4.1.0")).unwrap();
        assert!(r.has_python_checksum_bug());
        assert_eq!(
            r.verify_block_checksum(0).unwrap().0,
            ChecksumStatus::Valid,
            "an affected writer's checksum should verify against the uncompressed data"
        );

        // A writer past the fix: the same file is genuinely invalid.
        let r = Reader::from_bytes(make("6.0.0")).unwrap();
        assert!(!r.has_python_checksum_bug());
        assert_eq!(
            r.verify_block_checksum(0).unwrap().0,
            ChecksumStatus::Invalid,
            "the workaround must not apply to writers that are not affected"
        );
    }

    #[test]
    fn out_of_range_block_indices_error() {
        let file = build(b"one block", Compression::None, None);
        let r = Reader::from_bytes(file).unwrap();
        assert!(r.block(1).is_err());
        assert!(r.block_data(99).is_err());
    }

    #[test]
    fn a_file_without_a_tree_reads_cleanly() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        let header = BlockHeader {
            allocated_size: 4,
            used_size: 4,
            data_size: 4,
            ..Default::default()
        };
        header.write(&mut buf);
        buf.extend_from_slice(b"data");

        let r = Reader::from_bytes(buf).unwrap();
        assert!(r.tree().unwrap().is_none());
        assert_eq!(&*r.block_data(0).unwrap(), b"data");
    }
}
