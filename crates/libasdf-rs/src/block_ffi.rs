//! `asdf/block.h`: the low-level binary block API.
//!
//! # Ownership
//!
//! The header's ownership rules are intricate and are part of the contract,
//! so they are reproduced rather than simplified:
//!
//! - `asdf_block_create` returns a **detached** handle owning its own state.
//!   It is released with `asdf_block_destroy`.
//! - `asdf_block_append` transfers that state to the file. The handle becomes
//!   a *view* and is then released with `asdf_block_close`, not `destroy`.
//! - `asdf_block_open` returns a view of a block the file already owns, also
//!   released with `asdf_block_close`.
//! - `asdf_block_data_set` **borrows** the caller's buffer, which must stay
//!   valid until the file is written. `asdf_block_data_alloc` allocates a
//!   buffer the block owns instead.

use std::ffi::{CStr, c_char, c_int, c_void};

use asdf_core::block::header::CHECKSUM_SIZE;
use asdf_core::compression::Compression;
use asdf_core::{ChecksumStatus, PendingBlock};

use crate::file_ffi::{AsdfFile, file_blocks_mut, file_reader};
use crate::panic::guard;

/// Where a block's bytes live.
enum BlockData {
    /// Nothing assigned yet.
    Empty,
    /// A buffer the caller owns, borrowed until the file is written.
    Borrowed { ptr: *const u8, len: usize },
    /// A buffer this block owns.
    Owned(Vec<u8>),
}

impl BlockData {
    fn as_slice(&self) -> &[u8] {
        match self {
            BlockData::Empty => &[],
            // SAFETY: the C contract requires the caller to keep this buffer
            // valid until the file is written.
            BlockData::Borrowed { ptr, len } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
            BlockData::Owned(v) => v,
        }
    }

    fn len(&self) -> usize {
        match self {
            BlockData::Empty => 0,
            BlockData::Borrowed { len, .. } => *len,
            BlockData::Owned(v) => v.len(),
        }
    }
}

/// A block handle. Opaque to C.
pub struct AsdfBlock {
    /// The file this block belongs to, if any.
    file: *mut AsdfFile,
    /// The index in the file, for a view onto an existing block.
    index: Option<usize>,
    /// Still detached from any file, so `destroy` rather than `close`.
    detached: bool,
    data: BlockData,
    /// Already-compressed bytes to write verbatim, from
    /// `asdf_block_data_set_compressed`.
    data_is_compressed: bool,
    /// The uncompressed size to record when `data_is_compressed`.
    declared_data_size: u64,
    compression: Compression,
    /// The compression name, NUL-terminated.
    ///
    /// Five bytes, not four: the *header* field is four, but a four-character
    /// name like `bzp2` fills it exactly, leaving no room for a terminator.
    /// `asdf_block_compression` hands this out as a C string, so the extra
    /// byte is what keeps that read in bounds.
    compression_name: [u8; 5],
    allocated_size: u64,
    /// Decompressed bytes, cached on first access as libasdf does.
    decompressed: Option<Vec<u8>>,
}

impl std::fmt::Debug for AsdfBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsdfBlock")
            .field("index", &self.index)
            .field("detached", &self.detached)
            .field("len", &self.data.len())
            .field("compression", &self.compression)
            .finish()
    }
}

impl AsdfBlock {
    fn detached(data: BlockData) -> Self {
        Self {
            file: std::ptr::null_mut(),
            index: None,
            detached: true,
            data,
            data_is_compressed: false,
            declared_data_size: 0,
            compression: Compression::None,
            compression_name: [0; 5],
            allocated_size: 0,
            decompressed: None,
        }
    }

    /// The block as a value the writer can take.
    fn to_pending(&self) -> PendingBlock {
        PendingBlock {
            data: self.data.as_slice().to_vec(),
            compression: self.compression,
            allocated_size: self.allocated_size,
        }
    }
}

fn block_ref<'a>(block: *mut AsdfBlock) -> Option<&'a mut AsdfBlock> {
    (!block.is_null()).then(|| unsafe { &mut *block })
}

/// Open a view of a block the file owns.
///
/// # Safety
/// `file` must be null or a valid file handle. The result must be released
/// with [`asdf_block_close`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_open(file: *mut AsdfFile, index: usize) -> *mut AsdfBlock {
    guard("asdf_block_open", std::ptr::null_mut(), || {
        let Some(reader) = file_reader(file) else {
            return std::ptr::null_mut();
        };
        let Ok(location) = reader.block(index) else {
            return std::ptr::null_mut();
        };
        let header = location.header.clone();
        let compression =
            Compression::from_name(header.compression_name()).unwrap_or(Compression::None);

        let mut block = AsdfBlock {
            file,
            index: Some(index),
            detached: false,
            data: BlockData::Empty,
            data_is_compressed: compression != Compression::None,
            declared_data_size: header.data_size,
            compression,
            compression_name: {
                let mut name = [0u8; 5];
                name[..4].copy_from_slice(&header.compression);
                name
            },
            allocated_size: header.allocated_size,
            decompressed: None,
        };
        // Point at the stored bytes; the file owns them for as long as it is
        // open, which is exactly the lifetime the C contract gives this view.
        if let Ok(raw) = reader.block_raw(index) {
            block.data = BlockData::Borrowed { ptr: raw.as_ptr(), len: raw.len() };
        }
        Box::into_raw(Box::new(block))
    })
}

/// Release a block view.
///
/// # Safety
/// `block` must be null, or a view from [`asdf_block_open`] or an appended
/// handle, and must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_close(block: *mut AsdfBlock) {
    guard("asdf_block_close", (), || {
        if !block.is_null() {
            drop(unsafe { Box::from_raw(block) });
        }
    })
}

/// Create a detached block.
///
/// With `data` non-null the buffer is *borrowed* and must stay valid until
/// the file is written. With `data` null and `size` non-zero, a buffer of
/// that size is allocated for the caller to fill through
/// [`asdf_block_data_alloc`].
///
/// # Safety
/// `data` must point to at least `size` readable bytes, or be null. The
/// result must be released with [`asdf_block_destroy`], or handed to
/// [`asdf_block_append`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_create(data: *const c_void, size: usize) -> *mut AsdfBlock {
    guard("asdf_block_create", std::ptr::null_mut(), || {
        let payload = if !data.is_null() {
            BlockData::Borrowed { ptr: data.cast::<u8>(), len: size }
        } else if size > 0 {
            BlockData::Owned(vec![0u8; size])
        } else {
            BlockData::Empty
        };
        Box::into_raw(Box::new(AsdfBlock::detached(payload)))
    })
}

/// Destroy a detached block that was never appended.
///
/// # Safety
/// `block` must be null or a detached handle from [`asdf_block_create`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_destroy(block: *mut AsdfBlock) {
    guard("asdf_block_destroy", (), || {
        if !block.is_null() {
            drop(unsafe { Box::from_raw(block) });
        }
    })
}

/// Allocate a writable buffer of `size` bytes owned by the block.
///
/// An existing owned buffer of exactly `size` bytes is returned as-is rather
/// than reallocated, so `asdf_block_create(NULL, n)` followed by this call
/// yields the same buffer.
///
/// # Safety
/// `block` must be null or a valid handle. The returned pointer is valid
/// until the block is destroyed or its data replaced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_data_alloc(block: *mut AsdfBlock, size: usize) -> *mut c_void {
    guard("asdf_block_data_alloc", std::ptr::null_mut(), || {
        let Some(block) = block_ref(block) else {
            return std::ptr::null_mut();
        };
        match &mut block.data {
            BlockData::Owned(existing) if existing.len() == size => {
                existing.as_mut_ptr().cast::<c_void>()
            }
            _ => {
                block.data = BlockData::Owned(vec![0u8; size]);
                let BlockData::Owned(buffer) = &mut block.data else {
                    unreachable!("just assigned")
                };
                buffer.as_mut_ptr().cast::<c_void>()
            }
        }
    })
}

/// Point the block at a caller-owned buffer.
///
/// The buffer is borrowed, not copied, and must stay valid until the file is
/// written.
///
/// # Safety
/// `data` must point to at least `size` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_data_set(
    block: *mut AsdfBlock,
    data: *const c_void,
    size: usize,
) -> c_int {
    guard("asdf_block_data_set", -1, || {
        let Some(block) = block_ref(block) else { return -1 };
        if data.is_null() && size > 0 {
            return -1;
        }
        block.data = if data.is_null() {
            BlockData::Empty
        } else {
            BlockData::Borrowed { ptr: data.cast::<u8>(), len: size }
        };
        block.data_is_compressed = false;
        block.decompressed = None;
        0
    })
}

/// Point the block at bytes that are already compressed, to be written
/// verbatim.
///
/// Unlike [`asdf_block_data_set`], these bytes are not compressed again, so a
/// compressed block can be copied without decompressing it.
///
/// # Safety
/// `data` must point to at least `size` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_data_set_compressed(
    block: *mut AsdfBlock,
    data: *const c_void,
    size: usize,
    data_size: u64,
    compression: *const c_char,
) -> c_int {
    guard("asdf_block_data_set_compressed", -1, || {
        let Some(block) = block_ref(block) else { return -1 };
        if data.is_null() && size > 0 {
            return -1;
        }
        let name = if compression.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(compression) }.to_string_lossy().into_owned()
        };
        if name.len() > 4 {
            return -1;
        }

        block.data = if data.is_null() {
            BlockData::Empty
        } else {
            BlockData::Borrowed { ptr: data.cast::<u8>(), len: size }
        };
        block.data_is_compressed = true;
        block.declared_data_size = data_size;
        block.compression = Compression::from_name(&name).unwrap_or(Compression::None);
        block.compression_name = [0; 5];
        block.compression_name[..name.len()].copy_from_slice(name.as_bytes());
        block.decompressed = None;
        0
    })
}

/// Reserve space for the block in the file.
///
/// A value larger than the used size leaves room for the data to grow later
/// without moving everything after it. Zero means "the same as used".
///
/// # Safety
/// `block` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_allocated_size_set(
    block: *mut AsdfBlock,
    allocated_size: u64,
) -> c_int {
    guard("asdf_block_allocated_size_set", -1, || {
        let Some(block) = block_ref(block) else { return -1 };
        block.allocated_size = allocated_size;
        0
    })
}

/// Append a detached block to a file, transferring ownership.
///
/// The handle becomes a view onto the appended block and should afterwards be
/// released with [`asdf_block_close`] rather than
/// [`asdf_block_destroy`]. There is no deduplication: appending the same data
/// twice writes it twice.
///
/// # Safety
/// `file` must be a file handle open for writing, and `block` a detached
/// handle from [`asdf_block_create`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_append(
    file: *mut AsdfFile,
    block: *mut AsdfBlock,
) -> *mut AsdfBlock {
    guard("asdf_block_append", std::ptr::null_mut(), || {
        let Some(handle) = block_ref(block) else {
            return std::ptr::null_mut();
        };
        if !handle.detached {
            return std::ptr::null_mut();
        }
        let Some(blocks) = file_blocks_mut(file) else {
            return std::ptr::null_mut();
        };
        blocks.push(handle.to_pending());

        handle.file = file;
        handle.index = Some(blocks.len() - 1);
        handle.detached = false;
        block
    })
}

/// The uncompressed size of the block's data.
///
/// # Safety
/// `block` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_data_size(block: *mut AsdfBlock) -> usize {
    guard("asdf_block_data_size", 0, || {
        let Some(block) = block_ref(block) else { return 0 };
        if block.data_is_compressed {
            return usize::try_from(block.declared_data_size).unwrap_or(0);
        }
        block.data.len()
    })
}

/// The block's compression name, or the empty string.
///
/// # Safety
/// `block` must be null or a valid handle. The pointer is owned by the block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_compression(block: *mut AsdfBlock) -> *const c_char {
    guard("asdf_block_compression", std::ptr::null(), || {
        let Some(block) = block_ref(block) else {
            return std::ptr::null();
        };
        // Five bytes wide, so even a four-character name is NUL-terminated.
        block.compression_name.as_ptr().cast::<c_char>()
    })
}

/// Set the compression to use when the block is written.
///
/// # Safety
/// `compression` must be a valid NUL-terminated string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_compression_set(
    block: *mut AsdfBlock,
    compression: *const c_char,
) -> c_int {
    guard("asdf_block_compression_set", -1, || {
        let Some(block) = block_ref(block) else { return -1 };
        let name = if compression.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(compression) }.to_string_lossy().into_owned()
        };
        // An unknown compressor is refused rather than silently ignored.
        let Ok(method) = Compression::from_name(&name) else {
            return -1;
        };
        if !method.is_available() {
            return -1;
        }
        block.compression = method;
        block.compression_name = [0; 5];
        block.compression_name[..name.len()].copy_from_slice(name.as_bytes());
        0
    })
}

/// The MD5 digest recorded in the block header, or null.
///
/// # Safety
/// `block` must be null or a valid handle. The pointer is owned by the file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_checksum(block: *mut AsdfBlock) -> *const u8 {
    guard("asdf_block_checksum", std::ptr::null(), || {
        let Some(handle) = block_ref(block) else {
            return std::ptr::null();
        };
        let (Some(reader), Some(index)) = (file_reader(handle.file), handle.index) else {
            return std::ptr::null();
        };
        match reader.block(index) {
            Ok(location) => location.header.checksum.as_ptr(),
            Err(_) => std::ptr::null(),
        }
    })
}

/// Verify the block's MD5 checksum.
///
/// A block with no recorded checksum verifies, since the standard makes it
/// optional and an all-zero digest means "do not check".
///
/// # Safety
/// `block` must be null or a valid handle; `expected` must be writable for
/// 16 bytes, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_checksum_verify(
    block: *mut AsdfBlock,
    expected: *mut u8,
) -> bool {
    guard("asdf_block_checksum_verify", false, || {
        let Some(handle) = block_ref(block) else { return false };
        let (Some(reader), Some(index)) = (file_reader(handle.file), handle.index) else {
            return false;
        };
        let Ok((status, computed)) = reader.verify_block_checksum(index) else {
            return false;
        };
        if !expected.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(computed.as_ptr(), expected, CHECKSUM_SIZE);
            }
        }
        matches!(status, ChecksumStatus::Valid | ChecksumStatus::Absent)
    })
}

/// The block's data, decompressing on first access if needed.
///
/// # Safety
/// `block` must be null or a valid handle; `size` writable or null. The
/// returned pointer is owned by the block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_data(block: *mut AsdfBlock, size: *mut usize) -> *const c_void {
    guard("asdf_block_data", std::ptr::null(), || {
        let Some(handle) = block_ref(block) else {
            if !size.is_null() {
                unsafe { *size = 0 };
            }
            return std::ptr::null();
        };

        if handle.compression == Compression::None {
            let slice = handle.data.as_slice();
            if !size.is_null() {
                unsafe { *size = slice.len() };
            }
            return if slice.is_empty() {
                std::ptr::null()
            } else {
                slice.as_ptr().cast::<c_void>()
            };
        }

        // Cached, as libasdf does, so repeated access does not re-inflate.
        if handle.decompressed.is_none() {
            let expected = usize::try_from(handle.declared_data_size).unwrap_or(0);
            match handle.compression.decompress(handle.data.as_slice(), expected) {
                Ok(bytes) => handle.decompressed = Some(bytes),
                Err(_) => {
                    if !size.is_null() {
                        unsafe { *size = 0 };
                    }
                    return std::ptr::null();
                }
            }
        }
        let bytes = handle.decompressed.as_ref().expect("just decompressed");
        if !size.is_null() {
            unsafe { *size = bytes.len() };
        }
        bytes.as_ptr().cast::<c_void>()
    })
}

/// The block's bytes exactly as stored, without decompressing.
///
/// # Safety
/// See [`asdf_block_data`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_block_data_raw(
    block: *mut AsdfBlock,
    size: *mut usize,
) -> *const c_void {
    guard("asdf_block_data_raw", std::ptr::null(), || {
        let Some(handle) = block_ref(block) else {
            if !size.is_null() {
                unsafe { *size = 0 };
            }
            return std::ptr::null();
        };
        let slice = handle.data.as_slice();
        if !size.is_null() {
            unsafe { *size = slice.len() };
        }
        if slice.is_empty() { std::ptr::null() } else { slice.as_ptr().cast::<c_void>() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_ffi::{asdf_block_count, asdf_close, asdf_open_mem_ex, asdf_write_to_mem};
    use asdf_core::{Writer, writer::PendingBlock as CorePending};
    use std::ffi::CString;

    struct Handle(*mut AsdfFile);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { asdf_close(self.0) };
        }
    }

    /// A file with two blocks, one of them compressed.
    fn sample_file() -> Vec<u8> {
        let doc = asdf_core::yaml::parse_document(
            "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\na: 1\n...\n",
        )
        .unwrap();
        let mut writer = Writer::from_document(doc);
        writer.add_block(CorePending::new((0..=255u8).collect()));
        writer.add_block(CorePending::compressed(vec![7u8; 2048], Compression::Zlib));
        writer.to_bytes().unwrap()
    }

    fn open_sample() -> (Handle, Vec<u8>) {
        let bytes = sample_file();
        let f =
            unsafe { asdf_open_mem_ex(bytes.as_ptr().cast(), bytes.len(), std::ptr::null_mut()) };
        assert!(!f.is_null());
        (Handle(f), bytes)
    }

    #[test]
    fn opens_a_block_and_reads_its_data() {
        let (h, _bytes) = open_sample();
        assert_eq!(unsafe { asdf_block_count(h.0) }, 2);

        let block = unsafe { asdf_block_open(h.0, 0) };
        assert!(!block.is_null());

        let mut size = 0usize;
        let data = unsafe { asdf_block_data(block, &mut size) };
        assert!(!data.is_null());
        assert_eq!(size, 256);
        let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) };
        assert_eq!(slice[0], 0);
        assert_eq!(slice[255], 255);

        unsafe { asdf_block_close(block) };
    }

    #[test]
    fn a_compressed_block_inflates_on_demand() {
        let (h, _bytes) = open_sample();
        let block = unsafe { asdf_block_open(h.0, 1) };

        let name = unsafe { CStr::from_ptr(asdf_block_compression(block)) };
        assert_eq!(name.to_str().unwrap(), "zlib");

        // The raw form is the compressed bytes...
        let mut raw_size = 0usize;
        let raw = unsafe { asdf_block_data_raw(block, &mut raw_size) };
        assert!(!raw.is_null());
        assert!(raw_size < 2048, "raw should be the compressed form");

        // ...and the plain form inflates.
        let mut size = 0usize;
        let data = unsafe { asdf_block_data(block, &mut size) };
        assert_eq!(size, 2048);
        let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) };
        assert!(slice.iter().all(|b| *b == 7));

        // A second call must return the cached buffer, not inflate again.
        let again = unsafe { asdf_block_data(block, &mut size) };
        assert_eq!(again, data);

        unsafe { asdf_block_close(block) };
    }

    #[test]
    fn checksums_verify_through_the_c_api() {
        let (h, _bytes) = open_sample();
        for index in 0..2 {
            let block = unsafe { asdf_block_open(h.0, index) };
            let checksum = unsafe { asdf_block_checksum(block) };
            assert!(!checksum.is_null());

            let mut computed = [0u8; CHECKSUM_SIZE];
            assert!(
                unsafe { asdf_block_checksum_verify(block, computed.as_mut_ptr()) },
                "block {index}"
            );
            assert!(computed.iter().any(|b| *b != 0), "digest was not written out");
            unsafe { asdf_block_close(block) };
        }
    }

    #[test]
    fn out_of_range_indices_return_null() {
        let (h, _bytes) = open_sample();
        assert!(unsafe { asdf_block_open(h.0, 99) }.is_null());
        assert!(unsafe { asdf_block_open(std::ptr::null_mut(), 0) }.is_null());
    }

    #[test]
    fn a_created_block_borrows_the_callers_buffer() {
        let payload: Vec<u8> = (0..64u8).collect();
        let block = unsafe { asdf_block_create(payload.as_ptr().cast(), payload.len()) };
        assert!(!block.is_null());
        assert_eq!(unsafe { asdf_block_data_size(block) }, 64);

        let mut size = 0usize;
        let data = unsafe { asdf_block_data_raw(block, &mut size) };
        assert_eq!(size, 64);
        // Borrowed, not copied: the pointer is the caller's own buffer.
        assert_eq!(data.cast::<u8>(), payload.as_ptr());

        unsafe { asdf_block_destroy(block) };
    }

    #[test]
    fn creating_with_a_null_buffer_allocates_one() {
        let block = unsafe { asdf_block_create(std::ptr::null(), 128) };
        assert_eq!(unsafe { asdf_block_data_size(block) }, 128);

        // The documented shortcut: data_alloc of the same size returns the
        // buffer that create already made.
        let first = unsafe { asdf_block_data_alloc(block, 128) };
        let second = unsafe { asdf_block_data_alloc(block, 128) };
        assert!(!first.is_null());
        assert_eq!(first, second, "an existing buffer of the same size is reused");

        // Filling it through the returned pointer must be visible.
        unsafe { std::ptr::write_bytes(first.cast::<u8>(), 0xAB, 128) };
        let mut size = 0usize;
        let data = unsafe { asdf_block_data_raw(block, &mut size) };
        let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) };
        assert!(slice.iter().all(|b| *b == 0xAB));

        unsafe { asdf_block_destroy(block) };
    }

    #[test]
    fn a_different_size_reallocates() {
        let block = unsafe { asdf_block_create(std::ptr::null(), 16) };
        let _ = unsafe { asdf_block_data_alloc(block, 32) };
        assert_eq!(unsafe { asdf_block_data_size(block) }, 32);
        unsafe { asdf_block_destroy(block) };
    }

    #[test]
    fn appending_transfers_the_block_to_the_file() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let h = Handle(f);

        let payload = b"appended data".to_vec();
        let block = unsafe { asdf_block_create(payload.as_ptr().cast(), payload.len()) };
        let appended = unsafe { asdf_block_append(h.0, block) };
        assert_eq!(appended, block, "append returns the same handle as a view");
        assert_eq!(unsafe { asdf_block_count(h.0) }, 1);

        // A second append of the same handle must be refused: it is no
        // longer detached.
        assert!(unsafe { asdf_block_append(h.0, block) }.is_null());
        unsafe { asdf_block_close(appended) };

        // The data must survive into the written file.
        let mut buf: *mut c_void = std::ptr::null_mut();
        let mut size = 0usize;
        assert_eq!(unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) }, 0);
        let written = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size) }.to_vec();
        unsafe { libc::free(buf) };

        let reader = asdf_core::Reader::from_bytes(written).unwrap();
        assert_eq!(reader.block_count(), 1);
        assert_eq!(&*reader.block_data(0).unwrap(), b"appended data");
    }

    #[test]
    fn compression_can_be_set_and_unknown_names_refused() {
        let block = unsafe { asdf_block_create(std::ptr::null(), 8) };

        let zlib = CString::new("zlib").unwrap();
        assert_eq!(unsafe { asdf_block_compression_set(block, zlib.as_ptr()) }, 0);
        assert_eq!(
            unsafe { CStr::from_ptr(asdf_block_compression(block)) }.to_str().unwrap(),
            "zlib"
        );

        // Unknown compressors are an error, not silently ignored.
        let bogus = CString::new("zstd").unwrap();
        assert_eq!(unsafe { asdf_block_compression_set(block, bogus.as_ptr()) }, -1);
        assert_eq!(
            unsafe { CStr::from_ptr(asdf_block_compression(block)) }.to_str().unwrap(),
            "zlib",
            "a rejected name must not change the setting"
        );

        // The empty string clears it.
        assert_eq!(unsafe { asdf_block_compression_set(block, std::ptr::null()) }, 0);
        assert_eq!(unsafe { CStr::from_ptr(asdf_block_compression(block)) }.to_str().unwrap(), "");

        unsafe { asdf_block_destroy(block) };
    }

    #[test]
    fn an_appended_block_is_compressed_on_write() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let h = Handle(f);

        let payload = vec![3u8; 4096];
        let block = unsafe { asdf_block_create(payload.as_ptr().cast(), payload.len()) };
        let zlib = CString::new("zlib").unwrap();
        unsafe { asdf_block_compression_set(block, zlib.as_ptr()) };
        unsafe { asdf_block_append(h.0, block) };
        unsafe { asdf_block_close(block) };

        let mut buf: *mut c_void = std::ptr::null_mut();
        let mut size = 0usize;
        unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) };
        let written = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size) }.to_vec();
        unsafe { libc::free(buf) };

        let reader = asdf_core::Reader::from_bytes(written).unwrap();
        assert_eq!(reader.block_compression(0).unwrap(), Compression::Zlib);
        assert_eq!(&*reader.block_data(0).unwrap(), &payload[..]);
        assert!(reader.block_raw(0).unwrap().len() < payload.len());
    }

    #[test]
    fn allocated_size_is_honoured_on_write() {
        let f = unsafe { asdf_open_mem_ex(std::ptr::null(), 0, std::ptr::null_mut()) };
        let h = Handle(f);

        let payload = [1u8; 32];
        let block = unsafe { asdf_block_create(payload.as_ptr().cast(), payload.len()) };
        assert_eq!(unsafe { asdf_block_allocated_size_set(block, 1024) }, 0);
        unsafe { asdf_block_append(h.0, block) };
        unsafe { asdf_block_close(block) };

        let mut buf: *mut c_void = std::ptr::null_mut();
        let mut size = 0usize;
        unsafe { asdf_write_to_mem(h.0, &mut buf, &mut size) };
        let written = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size) }.to_vec();
        unsafe { libc::free(buf) };

        let reader = asdf_core::Reader::from_bytes(written).unwrap();
        assert_eq!(reader.block(0).unwrap().header.allocated_size, 1024);
        assert_eq!(reader.block(0).unwrap().header.used_size, 32);
    }

    #[test]
    fn data_set_replaces_the_buffer() {
        let block = unsafe { asdf_block_create(std::ptr::null(), 0) };
        let payload = b"replacement".to_vec();
        assert_eq!(
            unsafe { asdf_block_data_set(block, payload.as_ptr().cast(), payload.len()) },
            0
        );
        assert_eq!(unsafe { asdf_block_data_size(block) }, payload.len());
        unsafe { asdf_block_destroy(block) };
    }

    #[test]
    fn precompressed_data_records_its_uncompressed_size() {
        let raw = vec![9u8; 1000];
        let stored = Compression::Zlib.compress(&raw).unwrap();
        let block = unsafe { asdf_block_create(std::ptr::null(), 0) };
        let zlib = CString::new("zlib").unwrap();
        assert_eq!(
            unsafe {
                asdf_block_data_set_compressed(
                    block,
                    stored.as_ptr().cast(),
                    stored.len(),
                    raw.len() as u64,
                    zlib.as_ptr(),
                )
            },
            0
        );
        // data_size reports the *uncompressed* size, per the header.
        assert_eq!(unsafe { asdf_block_data_size(block) }, 1000);

        // ...and reading it back inflates to the original.
        let mut size = 0usize;
        let data = unsafe { asdf_block_data(block, &mut size) };
        assert_eq!(size, 1000);
        let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) };
        assert!(slice.iter().all(|b| *b == 9));

        unsafe { asdf_block_destroy(block) };
    }

    #[test]
    fn null_handles_are_tolerated() {
        let null = std::ptr::null_mut();
        assert_eq!(unsafe { asdf_block_data_size(null) }, 0);
        assert!(unsafe { asdf_block_compression(null) }.is_null());
        assert_eq!(unsafe { asdf_block_compression_set(null, std::ptr::null()) }, -1);
        assert!(unsafe { asdf_block_checksum(null) }.is_null());
        assert!(!unsafe { asdf_block_checksum_verify(null, std::ptr::null_mut()) });
        assert_eq!(unsafe { asdf_block_allocated_size_set(null, 0) }, -1);
        assert!(unsafe { asdf_block_data_alloc(null, 8) }.is_null());

        let mut size = 123usize;
        assert!(unsafe { asdf_block_data(null, &mut size) }.is_null());
        assert_eq!(size, 0, "size must be zeroed when there is no data");

        unsafe { asdf_block_close(null) };
        unsafe { asdf_block_destroy(null) };
    }
}
