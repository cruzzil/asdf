//! Binary blocks: the format's byte-exact layer.

pub mod header;

pub use header::{
    BLOCK_HEADER_FULL_SIZE, BLOCK_HEADER_SIZE, BLOCK_MAGIC, BLOCK_MAGIC_SIZE, BlockHeader,
    CHECKSUM_SIZE, FLAG_STREAMED, is_block_magic,
};
