//! Binary block headers.
//!
//! Unlike the YAML tree, this layer is byte-exact by specification. Every
//! field is big-endian, and `header_size` is authoritative: the standard
//! requires readers to obey it rather than assume 48, because a writer may
//! enlarge the header to align block data to a filesystem boundary.

use crate::error::{Error, ErrorCode, Result, err};

/// The bytes that introduce every block: `0xd3` followed by `BLK`.
pub const BLOCK_MAGIC: &[u8; 4] = b"\xd3BLK";

/// Length of [`BLOCK_MAGIC`].
pub const BLOCK_MAGIC_SIZE: usize = 4;

/// The smallest header the standard permits, measured the way `header_size`
/// measures it: excluding the magic and the `header_size` field itself.
pub const BLOCK_HEADER_SIZE: usize = 48;

/// Size of the full fixed prologue: magic, `header_size`, and a minimal header.
pub const BLOCK_HEADER_FULL_SIZE: usize = BLOCK_HEADER_SIZE + BLOCK_MAGIC_SIZE + 2;

/// Size of the compression name field.
pub const COMPRESSION_FIELD_SIZE: usize = 4;

/// Size of the MD5 digest stored in the header.
pub const CHECKSUM_SIZE: usize = 16;

/// The maximum a block header may occupy, per the standard's stated limits.
pub const MAX_BLOCK_HEADER_SIZE: usize = 65536;

// Field offsets, measured from just after `header_size`.
const OFF_FLAGS: usize = 0;
const OFF_COMPRESSION: usize = 4;
const OFF_ALLOCATED_SIZE: usize = 8;
const OFF_USED_SIZE: usize = 16;
const OFF_DATA_SIZE: usize = 24;
const OFF_CHECKSUM: usize = 32;

/// Set when the block extends to the end of the file.
///
/// A streamed block ignores the three size fields, must be the last block in
/// the file, and forbids a block index.
pub const FLAG_STREAMED: u32 = 0x1;

/// A decoded block header.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockHeader {
    /// The header size as recorded in the file, excluding the magic and this
    /// field. Preserved so a re-emitted block keeps any padding it had.
    pub header_size: u16,
    /// Flag bits; see [`FLAG_STREAMED`].
    pub flags: u32,
    /// The compression name, `\0`-padded to four bytes in the file.
    pub compression: [u8; COMPRESSION_FIELD_SIZE],
    /// Space reserved for the block's data, excluding the header.
    pub allocated_size: u64,
    /// Bytes actually used on disk, excluding the header.
    pub used_size: u64,
    /// Size of the data once decoded. Equal to `used_size` when uncompressed.
    pub data_size: u64,
    /// MD5 of the used data. All-zero means "do not verify".
    pub checksum: [u8; CHECKSUM_SIZE],
}

impl Default for BlockHeader {
    fn default() -> Self {
        Self {
            header_size: BLOCK_HEADER_SIZE as u16,
            flags: 0,
            compression: [0; COMPRESSION_FIELD_SIZE],
            allocated_size: 0,
            used_size: 0,
            data_size: 0,
            checksum: [0; CHECKSUM_SIZE],
        }
    }
}

/// Does this buffer start with the block magic?
pub fn is_block_magic(buf: &[u8]) -> bool {
    buf.len() >= BLOCK_MAGIC_SIZE && &buf[..BLOCK_MAGIC_SIZE] == BLOCK_MAGIC
}

fn be_u16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
fn be_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

impl BlockHeader {
    /// Decode a header from a buffer positioned at the block magic.
    ///
    /// Returns the header and the number of bytes it occupied, so the caller
    /// can find the data that follows.
    pub fn parse(buf: &[u8]) -> Result<(Self, usize)> {
        if buf.len() < BLOCK_MAGIC_SIZE + 2 {
            return Err(err!(UnexpectedEof, "truncated block header"));
        }
        if !is_block_magic(buf) {
            return Err(Error::new(
                ErrorCode::BlockMagicMismatch,
                "block magic bytes did not match",
            ));
        }

        let header_size = be_u16(&buf[BLOCK_MAGIC_SIZE..]);
        let hs = usize::from(header_size);

        if hs < BLOCK_HEADER_SIZE {
            return Err(err!(
                InvalidBlockHeader,
                "block header_size {hs} is below the {BLOCK_HEADER_SIZE}-byte minimum"
            ));
        }
        if hs > MAX_BLOCK_HEADER_SIZE {
            return Err(err!(
                InvalidBlockHeader,
                "block header_size {hs} exceeds the {MAX_BLOCK_HEADER_SIZE}-byte limit"
            ));
        }

        let total = BLOCK_MAGIC_SIZE + 2 + hs;
        if buf.len() < total {
            return Err(err!(UnexpectedEof, "truncated block header: need {total} bytes"));
        }

        // Fields are read at their fixed offsets within the declared header;
        // anything beyond `OFF_CHECKSUM + CHECKSUM_SIZE` is padding.
        let f = &buf[BLOCK_MAGIC_SIZE + 2..total];

        let mut compression = [0u8; COMPRESSION_FIELD_SIZE];
        compression.copy_from_slice(&f[OFF_COMPRESSION..OFF_COMPRESSION + COMPRESSION_FIELD_SIZE]);

        let mut checksum = [0u8; CHECKSUM_SIZE];
        checksum.copy_from_slice(&f[OFF_CHECKSUM..OFF_CHECKSUM + CHECKSUM_SIZE]);

        let header = BlockHeader {
            header_size,
            flags: be_u32(&f[OFF_FLAGS..]),
            compression,
            allocated_size: be_u64(&f[OFF_ALLOCATED_SIZE..]),
            used_size: be_u64(&f[OFF_USED_SIZE..]),
            data_size: be_u64(&f[OFF_DATA_SIZE..]),
            checksum,
        };

        header.validate()?;
        Ok((header, total))
    }

    /// Check the internal consistency the standard requires.
    fn validate(&self) -> Result<()> {
        if self.is_streamed() {
            // The size fields are explicitly ignored for a streamed block.
            return Ok(());
        }
        if self.compression_name().is_empty() && self.data_size != self.used_size {
            return Err(err!(
                InvalidBlockHeader,
                "uncompressed block has data_size {} but used_size {}",
                self.data_size,
                self.used_size
            ));
        }
        if self.allocated_size < self.used_size {
            return Err(err!(
                InvalidBlockHeader,
                "block allocated_size {} is smaller than used_size {}",
                self.allocated_size,
                self.used_size
            ));
        }
        Ok(())
    }

    /// Encode the header, including magic, into `out`.
    ///
    /// Any `header_size` beyond the fields is written as zero padding, so a
    /// block that was read with an enlarged header re-emits at the same size.
    pub fn write(&self, out: &mut Vec<u8>) {
        let hs = usize::from(self.header_size).max(BLOCK_HEADER_SIZE);
        out.extend_from_slice(BLOCK_MAGIC);
        out.extend_from_slice(&(hs as u16).to_be_bytes());

        let start = out.len();
        out.resize(start + hs, 0);
        let f = &mut out[start..start + hs];

        f[OFF_FLAGS..OFF_FLAGS + 4].copy_from_slice(&self.flags.to_be_bytes());
        f[OFF_COMPRESSION..OFF_COMPRESSION + COMPRESSION_FIELD_SIZE]
            .copy_from_slice(&self.compression);
        f[OFF_ALLOCATED_SIZE..OFF_ALLOCATED_SIZE + 8]
            .copy_from_slice(&self.allocated_size.to_be_bytes());
        f[OFF_USED_SIZE..OFF_USED_SIZE + 8].copy_from_slice(&self.used_size.to_be_bytes());
        f[OFF_DATA_SIZE..OFF_DATA_SIZE + 8].copy_from_slice(&self.data_size.to_be_bytes());
        f[OFF_CHECKSUM..OFF_CHECKSUM + CHECKSUM_SIZE].copy_from_slice(&self.checksum);
    }

    /// The number of bytes this header occupies in the file.
    pub fn on_disk_size(&self) -> usize {
        BLOCK_MAGIC_SIZE + 2 + usize::from(self.header_size)
    }

    /// Whether the streamed flag is set.
    pub fn is_streamed(&self) -> bool {
        self.flags & FLAG_STREAMED != 0
    }

    /// The compression name with its `\0` padding removed.
    ///
    /// An empty name means the block is uncompressed.
    pub fn compression_name(&self) -> &str {
        let end = self
            .compression
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(COMPRESSION_FIELD_SIZE);
        std::str::from_utf8(&self.compression[..end]).unwrap_or("")
    }

    /// Set the compression name, which must fit in four bytes.
    pub fn set_compression(&mut self, name: &str) -> Result<()> {
        let bytes = name.as_bytes();
        if bytes.len() > COMPRESSION_FIELD_SIZE {
            return Err(err!(
                UnknownCompression,
                "compression name {name:?} exceeds {COMPRESSION_FIELD_SIZE} bytes"
            ));
        }
        self.compression = [0; COMPRESSION_FIELD_SIZE];
        self.compression[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Whether a checksum is recorded. All-zero means "do not verify".
    pub fn has_checksum(&self) -> bool {
        self.checksum.iter().any(|b| *b != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BlockHeader {
        let mut h = BlockHeader { allocated_size: 64, used_size: 64, data_size: 64, ..Default::default() };
        h.set_compression("").unwrap();
        h
    }

    #[test]
    fn magic_is_the_documented_byte_sequence() {
        assert_eq!(BLOCK_MAGIC, &[0xd3, 0x42, 0x4c, 0x4b]);
        assert_eq!(BLOCK_MAGIC, b"\xd3BLK");
    }

    #[test]
    fn round_trips_through_bytes() {
        let h = sample();
        let mut buf = Vec::new();
        h.write(&mut buf);
        assert_eq!(buf.len(), BLOCK_HEADER_FULL_SIZE);

        let (parsed, consumed) = BlockHeader::parse(&buf).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(consumed, BLOCK_HEADER_FULL_SIZE);
    }

    #[test]
    fn fields_are_big_endian_at_documented_offsets() {
        let mut h = sample();
        h.flags = 0x0000_0001;
        h.allocated_size = 0x0102_0304_0506_0708;
        let mut buf = Vec::new();
        h.write(&mut buf);

        assert_eq!(&buf[0..4], BLOCK_MAGIC);
        assert_eq!(&buf[4..6], &[0x00, 0x30]); // header_size == 48
        assert_eq!(&buf[6..10], &[0, 0, 0, 1]); // flags, big-endian
        assert_eq!(
            &buf[14..22],
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
    }

    #[test]
    fn honours_an_enlarged_header_size() {
        // A writer may pad the header to align block data; readers must obey
        // header_size rather than assume 48.
        let mut h = sample();
        h.header_size = 96;
        let mut buf = Vec::new();
        h.write(&mut buf);
        assert_eq!(buf.len(), BLOCK_MAGIC_SIZE + 2 + 96);

        let (parsed, consumed) = BlockHeader::parse(&buf).unwrap();
        assert_eq!(parsed.header_size, 96);
        assert_eq!(consumed, BLOCK_MAGIC_SIZE + 2 + 96);
        assert_eq!(parsed.allocated_size, 64, "fields still read at fixed offsets");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = Vec::new();
        sample().write(&mut buf);
        buf[1] = b'X';
        let e = BlockHeader::parse(&buf).unwrap_err();
        assert_eq!(e.code(), ErrorCode::BlockMagicMismatch);
    }

    #[test]
    fn rejects_undersized_header_size() {
        let mut buf = Vec::new();
        sample().write(&mut buf);
        buf[4..6].copy_from_slice(&40u16.to_be_bytes());
        let e = BlockHeader::parse(&buf).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidBlockHeader);
    }

    #[test]
    fn rejects_truncated_input() {
        let mut buf = Vec::new();
        sample().write(&mut buf);
        buf.truncate(20);
        let e = BlockHeader::parse(&buf).unwrap_err();
        assert_eq!(e.code(), ErrorCode::UnexpectedEof);
    }

    #[test]
    fn uncompressed_block_must_have_matching_sizes() {
        let mut h = sample();
        h.data_size = 99;
        let mut buf = Vec::new();
        h.write(&mut buf);
        let e = BlockHeader::parse(&buf).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidBlockHeader);
    }

    #[test]
    fn compressed_block_may_have_differing_sizes() {
        let mut h = BlockHeader { allocated_size: 20, used_size: 20, data_size: 64, ..Default::default() };
        h.set_compression("zlib").unwrap();
        let mut buf = Vec::new();
        h.write(&mut buf);
        let (parsed, _) = BlockHeader::parse(&buf).unwrap();
        assert_eq!(parsed.compression_name(), "zlib");
        assert_eq!(parsed.data_size, 64);
    }

    #[test]
    fn streamed_blocks_skip_size_validation() {
        // The standard says the size fields are ignored when STREAMED is set.
        let mut h = BlockHeader { flags: FLAG_STREAMED, ..Default::default() };
        h.data_size = 12345;
        h.used_size = 0;
        h.allocated_size = 0;
        let mut buf = Vec::new();
        h.write(&mut buf);
        let (parsed, _) = BlockHeader::parse(&buf).unwrap();
        assert!(parsed.is_streamed());
    }

    #[test]
    fn compression_names_pad_and_trim() {
        let mut h = sample();
        h.set_compression("lz4").unwrap();
        assert_eq!(h.compression, [b'l', b'z', b'4', 0]);
        assert_eq!(h.compression_name(), "lz4");

        h.set_compression("bzp2").unwrap();
        assert_eq!(h.compression_name(), "bzp2");

        assert!(h.set_compression("toolong").is_err());
    }

    #[test]
    fn checksum_presence() {
        let mut h = sample();
        assert!(!h.has_checksum());
        h.checksum[0] = 1;
        assert!(h.has_checksum());
    }

    #[test]
    fn allocated_size_must_cover_used_size() {
        let mut h = BlockHeader { allocated_size: 8, used_size: 64, data_size: 64, ..Default::default() };
        h.set_compression("").unwrap();
        let mut buf = Vec::new();
        h.write(&mut buf);
        assert_eq!(
            BlockHeader::parse(&buf).unwrap_err().code(),
            ErrorCode::InvalidBlockHeader
        );
    }
}
