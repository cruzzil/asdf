//! Block compression.
//!
//! The standard defines `zlib` and `bzp2` and says implementations should
//! support both. `lz4` is a de-facto extension that Python asdf and libasdf
//! both implement, with a framing of their own that this module reproduces
//! exactly -- see [`lz4`].
//!
//! The name is stored in a four-byte field in the block header, so every
//! identifier is at most four bytes and an all-zero field means uncompressed.

use crate::error::{Result, err};

/// A compression method understood by this library.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compression {
    /// No compression; the header's compression field is all zeros.
    None,
    /// zlib, as defined by the standard.
    Zlib,
    /// bzip2, as defined by the standard.
    Bzp2,
    /// LZ4, a de-facto extension shared with Python asdf.
    Lz4,
}

impl Compression {
    /// Parse the four-byte identifier.
    ///
    /// An empty name means no compression, matching the all-zero field.
    pub fn from_name(name: &str) -> Result<Self> {
        match name {
            "" => Ok(Compression::None),
            "zlib" => Ok(Compression::Zlib),
            "bzp2" => Ok(Compression::Bzp2),
            "lz4" => Ok(Compression::Lz4),
            other => Err(err!(UnknownCompression, "unknown compression type: {other}")),
        }
    }

    /// The identifier as written to the block header.
    pub fn name(self) -> &'static str {
        match self {
            Compression::None => "",
            Compression::Zlib => "zlib",
            Compression::Bzp2 => "bzp2",
            Compression::Lz4 => "lz4",
        }
    }

    /// Whether support for this method was compiled in.
    pub fn is_available(self) -> bool {
        match self {
            Compression::None => true,
            Compression::Zlib => cfg!(feature = "zlib"),
            Compression::Bzp2 => cfg!(feature = "bzp2"),
            Compression::Lz4 => cfg!(feature = "lz4"),
        }
    }

    /// Decompress `data`, which is expected to expand to `expected_size` bytes.
    pub fn decompress(self, data: &[u8], expected_size: usize) -> Result<Vec<u8>> {
        match self {
            Compression::None => Ok(data.to_vec()),
            Compression::Zlib => zlib::decompress(data, expected_size),
            Compression::Bzp2 => bzp2::decompress(data, expected_size),
            Compression::Lz4 => lz4::decompress(data, expected_size),
        }
    }

    /// Compress `data`.
    pub fn compress(self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            Compression::None => Ok(data.to_vec()),
            Compression::Zlib => zlib::compress(data),
            Compression::Bzp2 => bzp2::compress(data),
            Compression::Lz4 => lz4::compress(data),
        }
    }
}

/// Every method this build supports, for reporting.
pub fn available() -> Vec<Compression> {
    [Compression::Zlib, Compression::Bzp2, Compression::Lz4]
        .into_iter()
        .filter(|c| c.is_available())
        .collect()
}

/// Guard against a corrupt header claiming an absurd decompressed size.
///
/// The standard's own limit is the 64-bit size field, but a claim far beyond
/// the input's plausible expansion is a sign of corruption rather than a
/// legitimate very large block, and allocating on it is a denial-of-service
/// waiting to happen.
const MAX_EXPANSION_RATIO: usize = 4096;

fn check_expected_size(compressed_len: usize, expected: usize) -> Result<()> {
    let ceiling = compressed_len
        .saturating_mul(MAX_EXPANSION_RATIO)
        .max(1 << 20);
    if expected > ceiling {
        return Err(err!(
            CompressionFailed,
            "block claims to decompress {expected} bytes from {compressed_len}, \
             beyond the {MAX_EXPANSION_RATIO}x sanity limit"
        ));
    }
    Ok(())
}

mod zlib {
    use super::*;

    #[cfg(feature = "zlib")]
    pub fn decompress(data: &[u8], expected: usize) -> Result<Vec<u8>> {
        use std::io::Read;
        check_expected_size(data.len(), expected)?;
        let mut out = Vec::with_capacity(expected);
        flate2::read::ZlibDecoder::new(data)
            .read_to_end(&mut out)
            .map_err(|e| err!(CompressionFailed, "zlib decompression failed: {e}"))?;
        Ok(out)
    }

    #[cfg(feature = "zlib")]
    pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
        use std::io::Write;
        let mut enc =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data)
            .and_then(|()| enc.finish())
            .map_err(|e| err!(CompressionFailed, "zlib compression failed: {e}"))
    }

    #[cfg(not(feature = "zlib"))]
    pub fn decompress(_data: &[u8], _expected: usize) -> Result<Vec<u8>> {
        Err(err!(UnknownCompression, "zlib support was not compiled in"))
    }

    #[cfg(not(feature = "zlib"))]
    pub fn compress(_data: &[u8]) -> Result<Vec<u8>> {
        Err(err!(UnknownCompression, "zlib support was not compiled in"))
    }
}

mod bzp2 {
    use super::*;

    #[cfg(feature = "bzp2")]
    pub fn decompress(data: &[u8], expected: usize) -> Result<Vec<u8>> {
        use std::io::Read;
        check_expected_size(data.len(), expected)?;
        let mut out = Vec::with_capacity(expected);
        bzip2::read::BzDecoder::new(data)
            .read_to_end(&mut out)
            .map_err(|e| err!(CompressionFailed, "bzip2 decompression failed: {e}"))?;
        Ok(out)
    }

    #[cfg(feature = "bzp2")]
    pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
        use std::io::Write;
        // Upstream uses block size 9 and work factor 30.
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
        enc.write_all(data)
            .and_then(|()| enc.finish())
            .map_err(|e| err!(CompressionFailed, "bzip2 compression failed: {e}"))
    }

    #[cfg(not(feature = "bzp2"))]
    pub fn decompress(_data: &[u8], _expected: usize) -> Result<Vec<u8>> {
        Err(err!(UnknownCompression, "bzip2 support was not compiled in"))
    }

    #[cfg(not(feature = "bzp2"))]
    pub fn compress(_data: &[u8]) -> Result<Vec<u8>> {
        Err(err!(UnknownCompression, "bzip2 support was not compiled in"))
    }
}

/// ASDF's LZ4 framing.
///
/// This is *not* the LZ4 frame format. Both libasdf and Python asdf write a
/// sequence of chunks, each laid out as:
///
/// ```text
///   [u32 big-endian]     length of everything that follows for this chunk
///   [u32 little-endian]  the chunk's decompressed size
///   [bytes]              a raw LZ4 block
/// ```
///
/// The big-endian length *includes* the four-byte little-endian size, which
/// is python-lz4's own header. The inner pair is therefore exactly
/// `lz4_flex`'s "size prepended" block format. Chunks are 4 MiB of input
/// each, matching both existing implementations.
pub mod lz4 {
    use super::*;

    /// The uncompressed chunk size both other implementations use.
    pub const CHUNK_SIZE: usize = 1 << 22;

    /// The per-chunk framing overhead: a big-endian length and a
    /// little-endian decompressed size.
    pub const CHUNK_HEADER_SIZE: usize = 8;

    #[cfg(feature = "lz4")]
    pub fn decompress(data: &[u8], expected: usize) -> Result<Vec<u8>> {
        check_expected_size(data.len(), expected)?;
        let mut out = Vec::with_capacity(expected);
        let mut pos = 0usize;

        while pos < data.len() {
            if pos + 4 > data.len() {
                return Err(err!(
                    CompressionFailed,
                    "lz4 stream truncated in a chunk length at offset {pos}"
                ));
            }
            let framed_len = u32::from_be_bytes([
                data[pos],
                data[pos + 1],
                data[pos + 2],
                data[pos + 3],
            ]) as usize;
            pos += 4;

            if framed_len < 4 || pos + framed_len > data.len() {
                return Err(err!(
                    CompressionFailed,
                    "lz4 chunk at offset {pos} claims {framed_len} bytes, \
                     past the end of the {} byte stream",
                    data.len()
                ));
            }

            // The framed length covers python-lz4's little-endian size header
            // plus the block, which together are what `decompress_size_prepended`
            // expects.
            let chunk = &data[pos..pos + framed_len];
            let decoded = lz4_flex::block::decompress_size_prepended(chunk)
                .map_err(|e| err!(CompressionFailed, "lz4 decompression failed: {e}"))?;
            out.extend_from_slice(&decoded);
            pos += framed_len;
        }
        Ok(out)
    }

    #[cfg(feature = "lz4")]
    pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        // An empty input produces an empty stream, as upstream's loop does.
        for chunk in data.chunks(CHUNK_SIZE) {
            let framed = lz4_flex::block::compress_prepend_size(chunk);
            let len = u32::try_from(framed.len())
                .map_err(|_| err!(CompressionFailed, "lz4 chunk too large to frame"))?;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(&framed);
        }
        Ok(out)
    }

    #[cfg(not(feature = "lz4"))]
    pub fn decompress(_data: &[u8], _expected: usize) -> Result<Vec<u8>> {
        Err(err!(UnknownCompression, "lz4 support was not compiled in"))
    }

    #[cfg(not(feature = "lz4"))]
    pub fn compress(_data: &[u8]) -> Result<Vec<u8>> {
        Err(err!(UnknownCompression, "lz4 support was not compiled in"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    /// A counter array: realistic ndarray data, but nearly incompressible
    /// for a fast low-ratio codec like LZ4.
    fn counter_payload() -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..10_000u32 {
            v.extend_from_slice(&i.to_le_bytes());
        }
        v
    }

    /// Data with real redundancy, of the kind every codec should shrink.
    fn compressible_payload() -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..10_000u32 {
            v.extend_from_slice(&(i % 16).to_le_bytes());
        }
        v
    }

    #[test]
    fn names_round_trip() {
        for c in [
            Compression::None,
            Compression::Zlib,
            Compression::Bzp2,
            Compression::Lz4,
        ] {
            assert_eq!(Compression::from_name(c.name()).unwrap(), c);
            // Every identifier must fit the header's four-byte field.
            assert!(c.name().len() <= 4, "{:?} name too long", c);
        }
    }

    #[test]
    fn unknown_names_are_rejected() {
        let e = Compression::from_name("zstd").unwrap_err();
        assert_eq!(e.code(), ErrorCode::UnknownCompression);
    }

    #[test]
    fn round_trips_through_every_method() {
        for data in [counter_payload(), compressible_payload()] {
            for c in available() {
                let packed = c.compress(&data).unwrap_or_else(|e| panic!("{:?}: {e}", c));
                let unpacked = c
                    .decompress(&packed, data.len())
                    .unwrap_or_else(|e| panic!("{:?}: {e}", c));
                assert_eq!(unpacked, data, "{:?} did not round trip", c);
            }
        }
    }

    #[test]
    fn every_method_shrinks_redundant_data() {
        // Deliberately separate from the round-trip test: LZ4 trades ratio
        // for speed and does not shrink a counter array, so asserting a ratio
        // on arbitrary data would be wrong rather than a real failure.
        let data = compressible_payload();
        for c in available() {
            let packed = c.compress(&data).unwrap();
            assert!(
                packed.len() < data.len(),
                "{:?} grew {} bytes to {}",
                c,
                data.len(),
                packed.len()
            );
        }
    }

    #[test]
    fn round_trips_empty_and_tiny_inputs() {
        for c in available() {
            for data in [vec![], vec![0u8], vec![7u8; 3]] {
                let packed = c.compress(&data).unwrap();
                let unpacked = c.decompress(&packed, data.len()).unwrap();
                assert_eq!(unpacked, data, "{:?} failed on {} bytes", c, data.len());
            }
        }
    }

    #[test]
    fn none_is_a_passthrough() {
        let data = b"unchanged".to_vec();
        assert_eq!(Compression::None.compress(&data).unwrap(), data);
        assert_eq!(
            Compression::None.decompress(&data, data.len()).unwrap(),
            data
        );
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_uses_the_asdf_chunk_framing() {
        // The framing is shared with Python asdf and libasdf, so its shape is
        // a compatibility contract, not an implementation detail.
        let data = vec![0xABu8; 1000];
        let packed = lz4::compress(&data).unwrap();

        assert!(packed.len() > lz4::CHUNK_HEADER_SIZE);
        let framed_len =
            u32::from_be_bytes([packed[0], packed[1], packed[2], packed[3]]) as usize;
        assert_eq!(
            framed_len,
            packed.len() - 4,
            "the big-endian length must cover the rest of the chunk"
        );

        let decompressed_size =
            u32::from_le_bytes([packed[4], packed[5], packed[6], packed[7]]) as usize;
        assert_eq!(
            decompressed_size,
            data.len(),
            "the little-endian header must carry the decompressed size"
        );
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_splits_large_inputs_into_chunks() {
        // Just over one chunk, so the stream must contain two frames.
        let data = vec![0x5Au8; lz4::CHUNK_SIZE + 1024];
        let packed = lz4::compress(&data).unwrap();
        let unpacked = lz4::decompress(&packed, data.len()).unwrap();
        assert_eq!(unpacked.len(), data.len());
        assert_eq!(unpacked, data);

        // Walk the frames to confirm there really are two.
        let mut pos = 0;
        let mut frames = 0;
        while pos < packed.len() {
            let len = u32::from_be_bytes([
                packed[pos],
                packed[pos + 1],
                packed[pos + 2],
                packed[pos + 3],
            ]) as usize;
            pos += 4 + len;
            frames += 1;
        }
        assert_eq!(frames, 2, "a 4 MiB + 1 KiB input should make two chunks");
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn truncated_lz4_streams_are_rejected() {
        let data = vec![0x11u8; 5000];
        let packed = lz4::compress(&data).unwrap();

        // Cut inside the compressed body.
        let e = lz4::decompress(&packed[..packed.len() - 10], data.len()).unwrap_err();
        assert_eq!(e.code(), ErrorCode::CompressionFailed);

        // Cut inside a length field.
        let e = lz4::decompress(&packed[..2], data.len()).unwrap_err();
        assert_eq!(e.code(), ErrorCode::CompressionFailed);
    }

    #[test]
    fn corrupt_input_is_an_error_not_a_panic() {
        let garbage = vec![0xFFu8; 64];
        for c in available() {
            let r = c.decompress(&garbage, 1024);
            // Either it errors, or it happens to decode something; it must
            // never panic or hang.
            if let Ok(v) = r {
                assert!(v.len() <= 1 << 20);
            }
        }
    }

    #[test]
    fn absurd_expected_sizes_are_refused() {
        // A corrupt header claiming a huge decompressed size must not cause a
        // huge allocation.
        let small = vec![0u8; 16];
        for c in available() {
            let e = c.decompress(&small, usize::MAX / 2);
            assert!(e.is_err(), "{:?} accepted an absurd size", c);
        }
    }

    #[test]
    fn available_reports_compiled_features() {
        let names: Vec<_> = available().iter().map(|c| c.name()).collect();
        // The standard requires both of these, and the default build has them.
        #[cfg(feature = "zlib")]
        assert!(names.contains(&"zlib"));
        #[cfg(feature = "bzp2")]
        assert!(names.contains(&"bzp2"));
        let _ = names;
    }
}
