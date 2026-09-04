//! `asdf/core/ndarray.h` and `asdf/core/datatype.h`.
//!
//! # Why the layouts matter here
//!
//! `asdf_ndarray_t` and `asdf_datatype_t` are **not** opaque. Callers read
//! `array->ndim` and `array->shape[0]` directly, and libasdf's own write
//! example builds an `asdf_ndarray_t` as a stack literal. So both layouts are
//! reproduced field for field, and the trailing `_reserved` pointer is where
//! this implementation keeps the state it needs.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

use asdf_core::compression::Compression;
use asdf_core::core::datatype::{Datatype, ScalarType};
use asdf_core::core::elements::{Element, decode_all};
use asdf_core::core::ndarray::{Ndarray, Source};

use crate::panic::guard;
use crate::types::AsdfArrayStorage;

/// Error codes matching `asdf_ndarray_err_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum NdarrayErr {
    /// Read successfully.
    Ok = 0,
    /// Read beyond the bounds of the array.
    OutOfBounds,
    /// Allocation failure.
    Oom,
    /// An argument was invalid.
    Inval,
    /// A value did not fit the requested type.
    Overflow,
    /// An element could not be converted to the requested type.
    Conversion,
}

/// Mirror of `asdf_datatype_t`.
///
/// Field order and widths must match `include/asdf/core/datatype.h`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_datatype_t {
    /// The scalar type, or `STRUCTURED` for a compound type.
    pub type_: ScalarTypeAbi,
    /// Element size in bytes. May be left 0 for numeric types.
    pub size: u64,
    /// Optional field name, for a compound type's member.
    pub name: *const c_char,
    /// Byte order of the elements.
    pub byteorder: ByteOrderAbi,
    /// Number of sub-array dimensions, 0 for a scalar.
    pub ndim: u32,
    /// The sub-array shape, `ndim` entries.
    pub shape: *const u64,
    /// Number of fields, for a compound type.
    pub nfields: u32,
    /// The fields, `nfields` entries.
    pub fields: *const asdf_datatype_t,
}

/// `asdf_scalar_datatype_t` as it crosses the boundary.
pub type ScalarTypeAbi = i32;
/// `asdf_byteorder_t` as it crosses the boundary.
pub type ByteOrderAbi = i32;

/// Mirror of `asdf_ndarray_t`.
///
/// The header notes that these fields are public "for now" and may not stay
/// ABI-stable; reproducing them exactly is what makes this a drop-in today.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_ndarray_t {
    /// Index of the block holding the data.
    pub source: usize,
    /// Number of dimensions.
    pub ndim: u32,
    /// The shape, `ndim` entries.
    pub shape: *const u64,
    /// The element type.
    pub datatype: asdf_datatype_t,
    /// Byte order of the array data.
    pub byteorder: ByteOrderAbi,
    /// Offset into the block where the data starts.
    pub offset: u64,
    /// Strides in bytes, `ndim` entries, or null for C-contiguous.
    pub strides: *const i64,
    /// Reserved for the implementation. This is where our state lives.
    pub _reserved: *mut c_void,
}

/// The state hanging off `_reserved`.
///
/// It owns every buffer the public struct points at, so the pointers stay
/// valid for as long as the array does and are freed exactly once.
struct NdarrayState {
    shape: Vec<u64>,
    strides: Option<Vec<i64>>,
    /// Field descriptors for a compound datatype, kept alive for `fields`.
    fields: Vec<asdf_datatype_t>,
    /// Field names, kept alive for each field's `name`.
    field_names: Vec<CString>,
    /// Per-field sub-array shapes.
    field_shapes: Vec<Vec<u64>>,
    /// The engine's own view of the array.
    parsed: Ndarray,
    /// Data read from the file, cached for `asdf_ndarray_data`.
    data: Option<Vec<u8>>,
    /// A buffer from `asdf_ndarray_data_alloc`, owned until dealloc.
    allocated: Option<Vec<u8>>,
    /// Compression to use when the array is written.
    compression: Compression,
    /// Where the data will be written.
    storage: AsdfArrayStorage,
    /// The file the array was read from, for `asdf_ndarray_block`.
    file: *mut crate::file_ffi::AsdfFile,
    /// The index of the block holding the data, when it is not inline.
    block_index: Option<usize>,
    /// The block view handed out by `asdf_ndarray_block`, owned here.
    block: *mut crate::block_ffi::AsdfBlock,
}

fn scalar_abi(t: ScalarType) -> ScalarTypeAbi {
    t as i32
}

/// The scalar type for an ABI discriminant, for other modules.
pub(crate) fn scalar_from_abi_public(v: ScalarTypeAbi) -> ScalarType {
    scalar_from_abi(v)
}

fn scalar_from_abi(v: ScalarTypeAbi) -> ScalarType {
    match v {
        1 => ScalarType::Int8,
        2 => ScalarType::Uint8,
        3 => ScalarType::Int16,
        4 => ScalarType::Uint16,
        5 => ScalarType::Int32,
        6 => ScalarType::Uint32,
        7 => ScalarType::Int64,
        8 => ScalarType::Uint64,
        9 => ScalarType::Float16,
        10 => ScalarType::Float32,
        11 => ScalarType::Float64,
        12 => ScalarType::Complex64,
        13 => ScalarType::Complex128,
        14 => ScalarType::Bool8,
        15 => ScalarType::Ascii,
        16 => ScalarType::Ucs4,
        17 => ScalarType::Structured,
        _ => ScalarType::Unknown,
    }
}

/// Build the C datatype view for `datatype`, parking owned storage in `state`.
fn build_datatype(datatype: &Datatype, state: &mut NdarrayState) -> asdf_datatype_t {
    for field in &datatype.fields {
        let name = field.name.as_deref().and_then(|n| CString::new(n).ok()).unwrap_or_default();
        state.field_names.push(name);
        state.field_shapes.push(field.datatype.shape.clone());
    }

    let base = state.fields.len();
    for (index, field) in datatype.fields.iter().enumerate() {
        let name_ptr = state.field_names[base + index].as_ptr();
        let shape = &state.field_shapes[base + index];
        state.fields.push(asdf_datatype_t {
            type_: scalar_abi(field.datatype.scalar),
            size: field.datatype.item_size(),
            name: name_ptr,
            byteorder: field.datatype.byteorder as i32,
            ndim: u32::try_from(shape.len()).unwrap_or(0),
            shape: if shape.is_empty() { std::ptr::null() } else { shape.as_ptr() },
            nfields: 0,
            fields: std::ptr::null(),
        });
    }

    asdf_datatype_t {
        type_: scalar_abi(datatype.scalar),
        size: datatype.item_size(),
        name: std::ptr::null(),
        byteorder: datatype.byteorder as i32,
        ndim: 0,
        shape: std::ptr::null(),
        nfields: u32::try_from(datatype.fields.len()).unwrap_or(0),
        fields: if state.fields.is_empty() {
            std::ptr::null()
        } else {
            state.fields[base..].as_ptr()
        },
    }
}

/// Build a public ndarray handle from the engine's parsed form.
pub(crate) fn make_ndarray(parsed: Ndarray, shape: Vec<u64>) -> *mut asdf_ndarray_t {
    let mut state = Box::new(NdarrayState {
        shape,
        strides: parsed.strides.clone(),
        fields: Vec::new(),
        field_names: Vec::new(),
        field_shapes: Vec::new(),
        parsed: parsed.clone(),
        data: None,
        allocated: None,
        compression: Compression::None,
        storage: AsdfArrayStorage::Internal,
        file: std::ptr::null_mut(),
        block_index: None,
        block: std::ptr::null_mut(),
    });

    // The compound-field storage must be sized before any pointer into it is
    // taken, or a later push would reallocate and dangle it.
    state.fields.reserve(parsed.datatype.fields.len());
    state.field_names.reserve(parsed.datatype.fields.len());
    state.field_shapes.reserve(parsed.datatype.fields.len());
    let datatype = build_datatype(&parsed.datatype, &mut state);

    let source = match parsed.source {
        Source::Block(index) => index,
        _ => 0,
    };

    let shape_ptr = if state.shape.is_empty() { std::ptr::null() } else { state.shape.as_ptr() };
    let strides_ptr = state.strides.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());

    let array = Box::new(asdf_ndarray_t {
        source,
        ndim: u32::try_from(state.shape.len()).unwrap_or(0),
        shape: shape_ptr,
        datatype,
        byteorder: parsed.byteorder as i32,
        offset: parsed.offset,
        strides: strides_ptr,
        _reserved: Box::into_raw(state).cast::<c_void>(),
    });
    Box::into_raw(array)
}

fn state_of<'a>(array: *mut asdf_ndarray_t) -> Option<&'a mut NdarrayState> {
    if array.is_null() {
        return None;
    }
    let reserved = unsafe { &*array }._reserved;
    (!reserved.is_null()).then(|| unsafe { &mut *reserved.cast::<NdarrayState>() })
}

/// The state behind an array, creating it from the public fields if absent.
///
/// A caller may build an `asdf_ndarray_t` as a stack literal with
/// `_reserved` left zero -- libasdf's own README write example does exactly
/// that, then calls `asdf_ndarray_data_alloc` on it. So anything needing
/// state has to bring it into being rather than assume it is already there.
///
/// The caller then owns that allocation, and releases it with
/// `asdf_ndarray_deinit` or `asdf_ndarray_destroy`.
fn ensure_state<'a>(array: *mut asdf_ndarray_t) -> Option<&'a mut NdarrayState> {
    if array.is_null() {
        return None;
    }
    if state_of(array).is_some() {
        return state_of(array);
    }

    let view = unsafe { &*array };
    let shape: Vec<u64> = if view.shape.is_null() || view.ndim == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(view.shape, view.ndim as usize) }.to_vec()
    };
    let strides: Option<Vec<i64>> = (!view.strides.is_null() && view.ndim > 0)
        .then(|| unsafe { std::slice::from_raw_parts(view.strides, view.ndim as usize) }.to_vec());

    // Rebuild the engine's view from what the caller filled in.
    let scalar = scalar_from_abi(view.datatype.type_);
    let mut datatype = Datatype::scalar(scalar);
    if view.datatype.size != 0 {
        datatype.size = view.datatype.size;
    }
    let parsed = Ndarray {
        source: Source::Block(view.source),
        shape: shape.iter().map(|d| Some(*d)).collect(),
        datatype,
        byteorder: match view.byteorder {
            62 => asdf_core::core::datatype::ByteOrder::Big,
            60 => asdf_core::core::datatype::ByteOrder::Little,
            _ => asdf_core::core::datatype::ByteOrder::native(),
        },
        offset: view.offset,
        strides: strides.clone(),
        mask: None,
    };

    let state = Box::new(NdarrayState {
        shape,
        strides,
        fields: Vec::new(),
        field_names: Vec::new(),
        field_shapes: Vec::new(),
        parsed,
        data: None,
        allocated: None,
        compression: Compression::None,
        storage: AsdfArrayStorage::Internal,
        file: std::ptr::null_mut(),
        block_index: None,
        block: std::ptr::null_mut(),
    });
    unsafe { (*array)._reserved = Box::into_raw(state).cast::<c_void>() };
    state_of(array)
}

/// The number of elements.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_size(ndarray: *const asdf_ndarray_t) -> u64 {
    guard("asdf_ndarray_size", 0, || {
        if ndarray.is_null() {
            return 0;
        }
        let array = unsafe { &*ndarray };
        if array.shape.is_null() || array.ndim == 0 {
            return 0;
        }
        let shape = unsafe { std::slice::from_raw_parts(array.shape, array.ndim as usize) };
        shape.iter().product()
    })
}

/// The number of bytes the elements occupy.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_nbytes(ndarray: *const asdf_ndarray_t) -> u64 {
    guard("asdf_ndarray_nbytes", 0, || {
        if ndarray.is_null() {
            return 0;
        }
        let count = unsafe { asdf_ndarray_size(ndarray) };
        count * unsafe { asdf_datatype_size(&raw const (*ndarray).datatype as *mut _) }
    })
}

/// The size of one element of a datatype, computing it when left at zero.
///
/// # Safety
/// `datatype` must be null or a valid `asdf_datatype_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_datatype_size(datatype: *mut asdf_datatype_t) -> u64 {
    guard("asdf_datatype_size", 0, || {
        if datatype.is_null() {
            return 0;
        }
        let dt = unsafe { &mut *datatype };
        if dt.size != 0 {
            return dt.size;
        }
        // A string type must carry its own size; zero there means an empty
        // string, as the header documents. Numeric types are computed and
        // written back.
        let scalar = scalar_from_abi(dt.type_);
        let computed = scalar.size();
        dt.size = computed;
        computed
    })
}

/// The scalar type named by a string, or `UNKNOWN`.
///
/// # Safety
/// `name` must be a valid NUL-terminated string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_scalar_datatype_from_string(name: *const c_char) -> ScalarTypeAbi {
    guard("asdf_scalar_datatype_from_string", 0, || {
        if name.is_null() {
            return 0;
        }
        let text = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        scalar_abi(ScalarType::from_name(&text))
    })
}

/// The string naming a scalar type.
///
/// # Safety
/// The returned pointer refers to a `'static` string.
#[unsafe(no_mangle)]
pub extern "C" fn asdf_scalar_datatype_to_string(datatype: ScalarTypeAbi) -> *const c_char {
    // Static names, so no allocation and no lifetime question.
    let name: &'static CStr = match scalar_from_abi(datatype) {
        ScalarType::Int8 => c"int8",
        ScalarType::Uint8 => c"uint8",
        ScalarType::Int16 => c"int16",
        ScalarType::Uint16 => c"uint16",
        ScalarType::Int32 => c"int32",
        ScalarType::Uint32 => c"uint32",
        ScalarType::Int64 => c"int64",
        ScalarType::Uint64 => c"uint64",
        ScalarType::Float16 => c"float16",
        ScalarType::Float32 => c"float32",
        ScalarType::Float64 => c"float64",
        ScalarType::Complex64 => c"complex64",
        ScalarType::Complex128 => c"complex128",
        ScalarType::Bool8 => c"bool8",
        ScalarType::Ascii => c"ascii",
        ScalarType::Ucs4 => c"ucs4",
        ScalarType::Structured => c"structured",
        ScalarType::Unknown => c"unknown",
    };
    name.as_ptr()
}

/// Allocate a data buffer sized for the array.
///
/// Repeated calls return the same buffer. It is released by
/// [`asdf_ndarray_data_dealloc`], not automatically.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_data_alloc(ndarray: *mut asdf_ndarray_t) -> *mut c_void {
    guard("asdf_ndarray_data_alloc", std::ptr::null_mut(), || {
        let nbytes = unsafe { asdf_ndarray_nbytes(ndarray) };
        let Some(state) = ensure_state(ndarray) else {
            return std::ptr::null_mut();
        };
        let Ok(len) = usize::try_from(nbytes) else {
            return std::ptr::null_mut();
        };
        if state.allocated.is_none() {
            state.allocated = Some(vec![0u8; len]);
        }
        state.allocated.as_mut().map_or(std::ptr::null_mut(), |b| b.as_mut_ptr().cast::<c_void>())
    })
}

/// Free a buffer from [`asdf_ndarray_data_alloc`].
///
/// Calling this without a prior allocation is a no-op.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_data_dealloc(ndarray: *mut asdf_ndarray_t) {
    guard("asdf_ndarray_data_dealloc", (), || {
        if let Some(state) = state_of(ndarray) {
            state.allocated = None;
        }
    })
}

/// Allocate the array's buffer and copy `src` into it.
///
/// # Safety
/// `src` must point to at least `asdf_ndarray_nbytes` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_data_copy(
    ndarray: *mut asdf_ndarray_t,
    src: *const c_void,
) -> NdarrayErr {
    guard("asdf_ndarray_data_copy", NdarrayErr::Inval, || {
        if ndarray.is_null() || src.is_null() {
            return NdarrayErr::Inval;
        }
        let nbytes = unsafe { asdf_ndarray_nbytes(ndarray) };
        let Ok(len) = usize::try_from(nbytes) else {
            return NdarrayErr::Inval;
        };
        let destination = unsafe { asdf_ndarray_data_alloc(ndarray) };
        if destination.is_null() {
            return NdarrayErr::Oom;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(src.cast::<u8>(), destination.cast::<u8>(), len);
        }
        NdarrayErr::Ok
    })
}

/// The array's data, decompressed if needed.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`; `size` writable or
/// null. The pointer is owned by the array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_data(
    ndarray: *mut asdf_ndarray_t,
    size: *mut usize,
) -> *const c_void {
    guard("asdf_ndarray_data", std::ptr::null(), || {
        let Some(state) = state_of(ndarray) else {
            if !size.is_null() {
                unsafe { *size = 0 };
            }
            return std::ptr::null();
        };
        // A buffer the caller built takes precedence: it is the array's data.
        let bytes = match (&state.allocated, &state.data) {
            (Some(buffer), _) => buffer,
            (None, Some(data)) => data,
            (None, None) => {
                if !size.is_null() {
                    unsafe { *size = 0 };
                }
                return std::ptr::null();
            }
        };
        if !size.is_null() {
            unsafe { *size = bytes.len() };
        }
        bytes.as_ptr().cast::<c_void>()
    })
}

/// The array's data as stored, without decompressing.
///
/// # Safety
/// See [`asdf_ndarray_data`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_data_raw(
    ndarray: *mut asdf_ndarray_t,
    size: *mut usize,
) -> *const c_void {
    // The engine decompresses on read, so the two coincide for arrays we
    // hand out; a caller wanting the stored form uses the block API.
    unsafe { asdf_ndarray_data(ndarray, size) }
}

/// Set the compression used when the array is written.
///
/// # Safety
/// `compression` must be a valid NUL-terminated string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_compression_set(
    ndarray: *mut asdf_ndarray_t,
    compression: *const c_char,
) -> c_int {
    guard("asdf_ndarray_compression_set", -1, || {
        let Some(state) = ensure_state(ndarray) else { return -1 };
        let name = if compression.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(compression) }.to_string_lossy().into_owned()
        };
        let Ok(method) = Compression::from_name(&name) else {
            return -1;
        };
        if !method.is_available() {
            return -1;
        }
        state.compression = method;
        0
    })
}

/// Where the array's data will be written.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_storage(ndarray: *mut asdf_ndarray_t) -> AsdfArrayStorage {
    guard("asdf_ndarray_storage", AsdfArrayStorage::Default, || {
        state_of(ndarray).map_or(AsdfArrayStorage::Default, |s| s.storage)
    })
}

/// Set where the array's data will be written.
///
/// `External` is not yet supported and leaves the setting unchanged, as
/// upstream does.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_storage_set(
    ndarray: *mut asdf_ndarray_t,
    storage: AsdfArrayStorage,
) {
    guard("asdf_ndarray_storage_set", (), || {
        if storage == AsdfArrayStorage::External {
            return;
        }
        if let Some(state) = ensure_state(ndarray) {
            state.storage = storage;
        }
    })
}

/// Convert one element to a destination scalar type, writing it into `out`.
fn write_converted(element: &Element, target: ScalarType, out: &mut [u8]) -> NdarrayErr {
    macro_rules! put {
        ($ty:ty, $value:expr) => {{
            let bytes = (<$ty>::from($value)).to_ne_bytes();
            if out.len() < bytes.len() {
                return NdarrayErr::Inval;
            }
            out[..bytes.len()].copy_from_slice(&bytes);
            NdarrayErr::Ok
        }};
    }
    macro_rules! put_int {
        ($ty:ty) => {{
            let wide: i128 = match element {
                Element::Int(v) => i128::from(*v),
                Element::Uint(v) => i128::from(*v),
                Element::Bool(v) => i128::from(*v),
                Element::Float(v) if v.fract() == 0.0 => *v as i128,
                _ => return NdarrayErr::Conversion,
            };
            match <$ty>::try_from(wide) {
                Ok(v) => {
                    let bytes = v.to_ne_bytes();
                    if out.len() < bytes.len() {
                        return NdarrayErr::Inval;
                    }
                    out[..bytes.len()].copy_from_slice(&bytes);
                    NdarrayErr::Ok
                }
                Err(_) => NdarrayErr::Overflow,
            }
        }};
    }

    match target {
        ScalarType::Int8 => put_int!(i8),
        ScalarType::Int16 => put_int!(i16),
        ScalarType::Int32 => put_int!(i32),
        ScalarType::Int64 => put_int!(i64),
        ScalarType::Uint8 => put_int!(u8),
        ScalarType::Uint16 => put_int!(u16),
        ScalarType::Uint32 => put_int!(u32),
        ScalarType::Uint64 => put_int!(u64),
        ScalarType::Bool8 => {
            let value = match element {
                Element::Bool(v) => u8::from(*v),
                Element::Int(v) => u8::from(*v != 0),
                Element::Uint(v) => u8::from(*v != 0),
                _ => return NdarrayErr::Conversion,
            };
            put!(u8, value)
        }
        ScalarType::Float32 => {
            let value = match element {
                Element::Float(v) => *v as f32,
                Element::Int(v) => *v as f32,
                Element::Uint(v) => *v as f32,
                _ => return NdarrayErr::Conversion,
            };
            let bytes = value.to_ne_bytes();
            if out.len() < bytes.len() {
                return NdarrayErr::Inval;
            }
            out[..bytes.len()].copy_from_slice(&bytes);
            NdarrayErr::Ok
        }
        ScalarType::Float64 => {
            let value = match element {
                Element::Float(v) => *v,
                Element::Int(v) => *v as f64,
                Element::Uint(v) => *v as f64,
                _ => return NdarrayErr::Conversion,
            };
            let bytes = value.to_ne_bytes();
            if out.len() < bytes.len() {
                return NdarrayErr::Inval;
            }
            out[..bytes.len()].copy_from_slice(&bytes);
            NdarrayErr::Ok
        }
        ScalarType::Float16 => {
            let value = match element {
                Element::Float(v) => half::f16::from_f64(*v),
                Element::Int(v) => half::f16::from_f64(*v as f64),
                Element::Uint(v) => half::f16::from_f64(*v as f64),
                _ => return NdarrayErr::Conversion,
            };
            let bytes = value.to_bits().to_ne_bytes();
            if out.len() < bytes.len() {
                return NdarrayErr::Inval;
            }
            out[..bytes.len()].copy_from_slice(&bytes);
            NdarrayErr::Ok
        }
        _ => NdarrayErr::Conversion,
    }
}

/// Decode the array's elements, using the cached data.
fn elements_of(state: &NdarrayState) -> Option<Vec<Element>> {
    let data = state.allocated.as_ref().or(state.data.as_ref())?;
    decode_all(&state.parsed, &state.shape, data).ok()
}

/// Read the whole array, converting to `dst_t`.
///
/// With `dst` pointing at a null pointer, a buffer is allocated and the
/// caller frees it with `free`. Passing `ASDF_DATATYPE_SOURCE` (which is
/// `UNKNOWN`) keeps the array's own type.
///
/// # Safety
/// `ndarray` must be a valid `asdf_ndarray_t`; `dst` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_read_all(
    ndarray: *mut asdf_ndarray_t,
    dst_t: ScalarTypeAbi,
    dst: *mut *mut c_void,
) -> NdarrayErr {
    guard("asdf_ndarray_read_all", NdarrayErr::Inval, || {
        let Some(state) = state_of(ndarray) else {
            return NdarrayErr::Inval;
        };
        let Some(elements) = elements_of(state) else {
            return NdarrayErr::Inval;
        };

        let target = match scalar_from_abi(dst_t) {
            // ASDF_DATATYPE_SOURCE is an alias for UNKNOWN and means
            // "keep the source type".
            ScalarType::Unknown => state.parsed.datatype.scalar,
            other => other,
        };
        let width = target.size();
        if width == 0 {
            return NdarrayErr::Conversion;
        }
        let Ok(width) = usize::try_from(width) else {
            return NdarrayErr::Inval;
        };

        let total = elements.len() * width;
        let mut buffer = vec![0u8; total];
        for (index, element) in elements.iter().enumerate() {
            let slot = &mut buffer[index * width..(index + 1) * width];
            let err = write_converted(element, target, slot);
            if err != NdarrayErr::Ok {
                return err;
            }
        }

        if dst.is_null() {
            return NdarrayErr::Inval;
        }
        let existing = unsafe { *dst };
        if existing.is_null() {
            // Allocate with malloc, since the caller frees it with free().
            let allocation = unsafe { libc::malloc(total.max(1)) };
            if allocation.is_null() {
                return NdarrayErr::Oom;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(buffer.as_ptr(), allocation.cast::<u8>(), total);
                *dst = allocation;
            }
        } else {
            unsafe {
                std::ptr::copy_nonoverlapping(buffer.as_ptr(), existing.cast::<u8>(), total);
            }
        }
        NdarrayErr::Ok
    })
}

/// The flat offset of an element, or `None` if the indices are out of range.
fn flat_index(shape: &[u64], indices: &[u64]) -> Option<usize> {
    if shape.len() != indices.len() {
        return None;
    }
    let mut flat = 0u64;
    for (dim, index) in shape.iter().zip(indices.iter()) {
        if index >= dim {
            return None;
        }
        flat = flat.checked_mul(*dim)?.checked_add(*index)?;
    }
    usize::try_from(flat).ok()
}

/// Generate a typed single-element accessor.
macro_rules! read_at {
    ($name:ident, $ty:ty, $convert:expr) => {
        /// Read one element, converted to this type.
        ///
        /// # Safety
        /// `ndarray` must be a valid `asdf_ndarray_t`; `indices` must point
        /// to `ndim` values; `err` writable or null.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            ndarray: *mut asdf_ndarray_t,
            indices: *const u64,
            err: *mut c_int,
        ) -> $ty {
            guard(stringify!($name), <$ty>::default(), || {
                let set = |code: NdarrayErr| {
                    if !err.is_null() {
                        unsafe { *err = code as c_int };
                    }
                };
                let Some(state) = state_of(ndarray) else {
                    set(NdarrayErr::Inval);
                    return <$ty>::default();
                };
                if indices.is_null() {
                    set(NdarrayErr::Inval);
                    return <$ty>::default();
                }
                let idx = unsafe { std::slice::from_raw_parts(indices, state.shape.len()) };
                let Some(flat) = flat_index(&state.shape, idx) else {
                    set(NdarrayErr::OutOfBounds);
                    return <$ty>::default();
                };
                let Some(elements) = elements_of(state) else {
                    set(NdarrayErr::Inval);
                    return <$ty>::default();
                };
                let Some(element) = elements.get(flat) else {
                    set(NdarrayErr::OutOfBounds);
                    return <$ty>::default();
                };
                #[allow(clippy::redundant_closure_call)]
                match ($convert)(element) {
                    Some(v) => {
                        set(NdarrayErr::Ok);
                        v
                    }
                    None => {
                        set(NdarrayErr::Conversion);
                        <$ty>::default()
                    }
                }
            })
        }
    };
}

/// Convert an element to an integer type, or `None`.
macro_rules! to_int {
    ($ty:ty) => {
        |element: &Element| -> Option<$ty> {
            let wide: i128 = match element {
                Element::Int(v) => i128::from(*v),
                Element::Uint(v) => i128::from(*v),
                Element::Bool(v) => i128::from(*v),
                Element::Float(v) if v.fract() == 0.0 => *v as i128,
                _ => return None,
            };
            <$ty>::try_from(wide).ok()
        }
    };
}

read_at!(asdf_ndarray_read_int8_at, i8, to_int!(i8));
read_at!(asdf_ndarray_read_int16_at, i16, to_int!(i16));
read_at!(asdf_ndarray_read_int32_at, i32, to_int!(i32));
read_at!(asdf_ndarray_read_int64_at, i64, to_int!(i64));
read_at!(asdf_ndarray_read_uint8_at, u8, to_int!(u8));
read_at!(asdf_ndarray_read_uint16_at, u16, to_int!(u16));
read_at!(asdf_ndarray_read_uint32_at, u32, to_int!(u32));
read_at!(asdf_ndarray_read_uint64_at, u64, to_int!(u64));

read_at!(asdf_ndarray_read_float32_at, f32, |element: &Element| {
    match element {
        Element::Float(v) => Some(*v as f32),
        Element::Int(v) => Some(*v as f32),
        Element::Uint(v) => Some(*v as f32),
        _ => None,
    }
});
read_at!(asdf_ndarray_read_float64_at, f64, |element: &Element| {
    match element {
        Element::Float(v) => Some(*v),
        Element::Int(v) => Some(*v as f64),
        Element::Uint(v) => Some(*v as f64),
        _ => None,
    }
});

/// Read one `float16` element, returning its raw bit pattern.
///
/// Called only by `shim.c`, which reinterprets the bits as `_Float16`. The
/// conversion has to happen on the C side: `_Float16` and `uint16_t` do not
/// share a return ABI -- on x86-64 SysV one returns in `xmm0`, the other in
/// `rax` -- so returning bits from Rust and reinterpreting in C is the only
/// way to place the value correctly without unstable Rust.
///
/// # Safety
/// See the other `read_*_at` accessors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_shim_ndarray_read_float16_bits_at(
    ndarray: *mut c_void,
    indices: *const u64,
    err: *mut c_int,
) -> u16 {
    guard("asdf_shim_ndarray_read_float16_bits_at", 0u16, || {
        let value =
            unsafe { asdf_ndarray_read_float64_at(ndarray.cast::<asdf_ndarray_t>(), indices, err) };
        half::f16::from_f64(value).to_bits()
    })
}

/// Free an ndarray handle and everything it owns.
///
/// # Safety
/// `ndarray` must be null or have come from the library, and must not be used
/// afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_destroy(ndarray: *mut asdf_ndarray_t) {
    guard("asdf_ndarray_destroy", (), || {
        if ndarray.is_null() {
            return;
        }
        let boxed = unsafe { Box::from_raw(ndarray) };
        if !boxed._reserved.is_null() {
            drop(unsafe { Box::from_raw(boxed._reserved.cast::<NdarrayState>()) });
        }
    })
}

/// Attach data read from a file to an array handle.
pub(crate) fn set_data(array: *mut asdf_ndarray_t, data: Vec<u8>) {
    if let Some(state) = state_of(array) {
        state.data = Some(data);
    }
}

/// Build an ndarray handle for a value, reading its block data.
fn ndarray_from_value(value: *mut crate::file_ffi::AsdfValue) -> *mut asdf_ndarray_t {
    use crate::file_ffi::{file_reader, value_document, value_file, value_node};

    let (Some(doc), Some(node)) = (value_document(value), value_node(value)) else {
        return std::ptr::null_mut();
    };
    let Ok(parsed) = Ndarray::parse(doc, node) else {
        return std::ptr::null_mut();
    };

    // Read the block up front, as libasdf does, so the array's data pointer
    // is usable for as long as the handle is.
    let mut data = None;
    let mut block_len = None;
    let mut block_index = None;
    if let Some(file) = value_file(value)
        && let Some(reader) = file_reader(file)
    {
        let index = match parsed.source {
            Source::Block(index) => Some(index),
            Source::LastBlock => reader.block_count().checked_sub(1),
            _ => None,
        };
        block_index = index;
        if let Some(index) = index
            && let Ok(bytes) = reader.block_data(index)
        {
            block_len = Some(bytes.len() as u64);
            data = Some(bytes.into_owned());
        }
    }

    let Ok(shape) = parsed.resolved_shape(block_len) else {
        return std::ptr::null_mut();
    };
    let array = make_ndarray(parsed, shape);
    if let Some(bytes) = data {
        set_data(array, bytes);
    }
    // Remember where the data came from so `asdf_ndarray_block` can hand
    // back a view of the underlying block.
    if let Some(state) = state_of(array) {
        state.file = value_file(value).unwrap_or(std::ptr::null_mut());
        state.block_index = block_index;
    }
    array
}

/// Read the array at `path`.
///
/// # Safety
/// `file` must be a valid file handle, `path` a valid NUL-terminated string
/// or null, and `out` writable. The result must be released with
/// [`asdf_ndarray_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_get_ndarray(
    file: *mut crate::file_ffi::AsdfFile,
    path: *const c_char,
    out: *mut *mut asdf_ndarray_t,
) -> crate::types::AsdfValueErr {
    use crate::types::AsdfValueErr;

    guard("asdf_get_ndarray", AsdfValueErr::Unknown, || {
        let value = unsafe { crate::file_ffi::asdf_get_value(file, path) };
        if value.is_null() {
            return AsdfValueErr::NotFound;
        }
        let array = ndarray_from_value(value);
        unsafe { crate::file_ffi::asdf_value_destroy(value) };

        if array.is_null() {
            return AsdfValueErr::TypeMismatch;
        }
        if !out.is_null() {
            unsafe { *out = array };
        } else {
            unsafe { asdf_ndarray_destroy(array) };
        }
        AsdfValueErr::Ok
    })
}

/// Interpret a value as an array.
///
/// # Safety
/// `value` must be a valid value handle and `out` writable. The result must
/// be released with [`asdf_ndarray_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_as_ndarray(
    value: *mut crate::file_ffi::AsdfValue,
    out: *mut *mut asdf_ndarray_t,
) -> crate::types::AsdfValueErr {
    use crate::types::AsdfValueErr;

    guard("asdf_value_as_ndarray", AsdfValueErr::Unknown, || {
        let array = ndarray_from_value(value);
        if array.is_null() {
            return AsdfValueErr::TypeMismatch;
        }
        if !out.is_null() {
            unsafe { *out = array };
        } else {
            unsafe { asdf_ndarray_destroy(array) };
        }
        AsdfValueErr::Ok
    })
}

/// Whether the value at `path` is an ndarray.
///
/// # Safety
/// `file` must be a valid file handle and `path` a valid string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_is_ndarray(
    file: *mut crate::file_ffi::AsdfFile,
    path: *const c_char,
) -> bool {
    guard("asdf_is_ndarray", false, || {
        let value = unsafe { crate::file_ffi::asdf_get_value(file, path) };
        if value.is_null() {
            return false;
        }
        let is_array = unsafe { asdf_value_is_ndarray(value) };
        unsafe { crate::file_ffi::asdf_value_destroy(value) };
        is_array
    })
}

/// Whether a value is an ndarray, by its tag.
///
/// # Safety
/// `value` must be null or a valid value handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_is_ndarray(value: *mut crate::file_ffi::AsdfValue) -> bool {
    guard("asdf_value_is_ndarray", false, || {
        use crate::file_ffi::{value_document, value_node};
        let (Some(doc), Some(node)) = (value_document(value), value_node(value)) else {
            return false;
        };
        doc.tag_of(node).is_some_and(|t| t.split_version().0 == "core/ndarray")
    })
}

// ---- The rest of the generated extension family ----------------------

/// Free an ndarray's fields without freeing the struct.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`; safe on a zeroed one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_deinit(ndarray: *mut asdf_ndarray_t) {
    guard("asdf_ndarray_deinit", (), || {
        if ndarray.is_null() {
            return;
        }
        let array = unsafe { &mut *ndarray };
        if !array._reserved.is_null() {
            let state = unsafe { Box::from_raw(array._reserved.cast::<NdarrayState>()) };
            if !state.block.is_null() {
                unsafe { crate::block_ffi::asdf_block_close(state.block) };
            }
            drop(state);
            array._reserved = std::ptr::null_mut();
        }
        // The public pointers all borrowed from the state that just went.
        array.shape = std::ptr::null();
        array.strides = std::ptr::null();
        array.datatype.fields = std::ptr::null();
        array.datatype.nfields = 0;
        array.ndim = 0;
    })
}

/// Deep-copy an ndarray into caller-provided storage.
///
/// The copy owns its own data, so it may outlive the original and be written
/// to a different file.
///
/// # Safety
/// `src` and `dst` must be valid `asdf_ndarray_t` values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_copy_into(
    file: *mut crate::file_ffi::AsdfFile,
    src: *const asdf_ndarray_t,
    dst: *mut asdf_ndarray_t,
) -> bool {
    guard("asdf_ndarray_copy_into", false, || {
        let _ = file;
        if src.is_null() || dst.is_null() {
            return false;
        }
        let Some(state) = state_of(src.cast_mut()) else {
            return false;
        };

        // Rebuild from the engine's own view, so every buffer is fresh.
        let rebuilt = make_ndarray(state.parsed.clone(), state.shape.clone());
        if rebuilt.is_null() {
            return false;
        }
        if let Some(data) = state.allocated.as_ref().or(state.data.as_ref()) {
            set_data(rebuilt, data.clone());
        }
        if let Some(fresh) = state_of(rebuilt) {
            fresh.compression = state.compression;
            fresh.storage = state.storage;
        }

        // Move the rebuilt value into the caller's storage.
        let boxed = unsafe { Box::from_raw(rebuilt) };
        unsafe { std::ptr::write(dst, *boxed) };
        true
    })
}

/// Deep-copy an ndarray into fresh storage.
///
/// # Safety
/// `src` must be a valid `asdf_ndarray_t`. The result must be released with
/// [`asdf_ndarray_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_copy(
    file: *mut crate::file_ffi::AsdfFile,
    src: *const asdf_ndarray_t,
) -> *mut asdf_ndarray_t {
    guard("asdf_ndarray_copy", std::ptr::null_mut(), || {
        if src.is_null() {
            return std::ptr::null_mut();
        }
        let raw = Box::into_raw(Box::new(asdf_ndarray_t {
            source: 0,
            ndim: 0,
            shape: std::ptr::null(),
            datatype: asdf_datatype_t {
                type_: 0,
                size: 0,
                name: std::ptr::null(),
                byteorder: 0,
                ndim: 0,
                shape: std::ptr::null(),
                nfields: 0,
                fields: std::ptr::null(),
            },
            byteorder: 0,
            offset: 0,
            strides: std::ptr::null(),
            _reserved: std::ptr::null_mut(),
        }));
        if unsafe { asdf_ndarray_copy_into(file, src, raw) } {
            raw
        } else {
            drop(unsafe { Box::from_raw(raw) });
            std::ptr::null_mut()
        }
    })
}

/// Deep-copy a null-terminated array of ndarrays.
///
/// # Safety
/// `src` must be a null-terminated array of valid `asdf_ndarray_t` pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_array_copy(
    file: *mut crate::file_ffi::AsdfFile,
    src: *mut *const asdf_ndarray_t,
) -> *mut *mut asdf_ndarray_t {
    guard("asdf_ndarray_array_copy", std::ptr::null_mut(), || {
        if src.is_null() {
            return std::ptr::null_mut();
        }
        let mut count = 0isize;
        while !unsafe { *src.offset(count) }.is_null() {
            count += 1;
        }

        let mut copies: Vec<*mut asdf_ndarray_t> = Vec::with_capacity(count as usize + 1);
        for index in 0..count {
            let copy = unsafe { asdf_ndarray_copy(file, *src.offset(index)) };
            if copy.is_null() {
                // Unwind rather than leak the copies already made.
                for made in copies {
                    unsafe { asdf_ndarray_destroy(made) };
                }
                return std::ptr::null_mut();
            }
            copies.push(copy);
        }
        copies.push(std::ptr::null_mut());
        Box::into_raw(copies.into_boxed_slice()).cast::<*mut asdf_ndarray_t>()
    })
}

/// Build a value for an ndarray, writing its data into a new block.
///
/// The array's `source` in the tree is the index of the block appended to
/// `file`, so the value is only meaningful once written with that file.
///
/// # Safety
/// `file` must be a file handle open for writing and `obj` a valid
/// `asdf_ndarray_t`. The result must be released with `asdf_value_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_value_of_ndarray(
    file: *mut crate::file_ffi::AsdfFile,
    obj: *const asdf_ndarray_t,
) -> *mut crate::file_ffi::AsdfValue {
    use asdf_core::yaml::{CollectionStyle, NodeData, Tag};

    guard("asdf_value_of_ndarray", std::ptr::null_mut(), || {
        if file.is_null() || obj.is_null() {
            return std::ptr::null_mut();
        }
        let array = unsafe { &*obj };

        // The shape and datatype come from the public fields, so an array
        // built as a C stack literal works -- which is what libasdf's own
        // write example does.
        let shape: Vec<u64> = if array.shape.is_null() || array.ndim == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(array.shape, array.ndim as usize) }.to_vec()
        };
        let scalar = scalar_from_abi(array.datatype.type_);
        let item_size = if array.datatype.size != 0 { array.datatype.size } else { scalar.size() };
        if item_size == 0 {
            return std::ptr::null_mut();
        }

        // The data is whatever the caller allocated or we read.
        let element_count: u64 = shape.iter().product::<u64>().max(1);
        let expected = (element_count * item_size) as usize;
        let payload: Vec<u8> = match ensure_state(obj.cast_mut()) {
            Some(state) => state
                .allocated
                .as_ref()
                .or(state.data.as_ref())
                .cloned()
                .unwrap_or_else(|| vec![0u8; expected]),
            None => vec![0u8; expected],
        };
        let compression =
            state_of(obj.cast_mut()).map(|s| s.compression).unwrap_or(Compression::None);

        // Append the block, then reference it by index.
        let Some(blocks) = crate::file_ffi::file_blocks_mut(file) else {
            return std::ptr::null_mut();
        };
        blocks.push(asdf_core::PendingBlock::compressed(payload, compression));
        let index = blocks.len() - 1;

        let handle = unsafe { &mut *file };
        let Some(doc) = handle.document_for_values() else {
            return std::ptr::null_mut();
        };

        let source = doc.add_scalar(index.to_string());
        let datatype = doc.add_scalar(scalar.name());
        let order = match array.byteorder {
            62 => "big",
            60 => "little",
            // An unspecified order means this machine's.
            _ => ByteOrderNative,
        };
        let byteorder = doc.add_scalar(order);

        let dims: Vec<_> = shape.iter().map(|d| doc.add_scalar(d.to_string())).collect();
        let shape_node = doc.add_sequence(dims);
        if let NodeData::Sequence { style, .. } = &mut doc.node_mut(shape_node).data {
            *style = CollectionStyle::Flow;
        }

        let keys: Vec<_> = ["source", "datatype", "byteorder", "shape"]
            .iter()
            .map(|k| doc.add_scalar(*k))
            .collect();
        let node = doc.add_mapping(vec![
            (keys[0], source),
            (keys[1], datatype),
            (keys[2], byteorder),
            (keys[3], shape_node),
        ]);
        doc.node_mut(node).tag = Some(Tag::parse("tag:stsci.edu:asdf/core/ndarray-1.1.0"));

        Box::into_raw(Box::new(crate::file_ffi::AsdfValue::new(file, node)))
    })
}

/// This machine's byte order, as the schema spells it.
#[allow(non_upper_case_globals)]
const ByteOrderNative: &str = if cfg!(target_endian = "big") { "big" } else { "little" };

/// Write an ndarray at `path`, appending its data as a new block.
///
/// # Safety
/// See [`asdf_value_of_ndarray`]; `path` must be a valid string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_set_ndarray(
    file: *mut crate::file_ffi::AsdfFile,
    path: *const c_char,
    obj: *const asdf_ndarray_t,
) -> crate::types::AsdfValueErr {
    use crate::types::AsdfValueErr;

    guard("asdf_set_ndarray", AsdfValueErr::Unknown, || {
        let value = unsafe { asdf_value_of_ndarray(file, obj) };
        if value.is_null() {
            return AsdfValueErr::EmitFailure;
        }
        let result = unsafe { crate::file_ffi::set_value_at(file, path, value) };
        unsafe { crate::file_ffi::asdf_value_destroy(value) };
        result
    })
}

// ---- Blocks and tiles ------------------------------------------------

/// The block underlying an array, or null when its data is inline.
///
/// The view is opened on first use and owned by the array, so it must not be
/// closed by the caller; it is released with the array.
///
/// # Safety
/// `ndarray` must be null or a valid `asdf_ndarray_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_block(
    ndarray: *mut asdf_ndarray_t,
) -> *mut crate::block_ffi::AsdfBlock {
    guard("asdf_ndarray_block", std::ptr::null_mut(), || {
        let Some(state) = state_of(ndarray) else {
            return std::ptr::null_mut();
        };
        if !state.block.is_null() {
            return state.block;
        }
        let (Some(index), false) = (state.block_index, state.file.is_null()) else {
            return std::ptr::null_mut();
        };
        state.block = unsafe { crate::block_ffi::asdf_block_open(state.file, index) };
        state.block
    })
}

/// Copy `count` elements starting at `flat` into `dst`, converting to `target`.
fn write_elements(
    elements: &[Element],
    flat: usize,
    count: usize,
    target: ScalarType,
    width: usize,
    dst: &mut [u8],
) -> NdarrayErr {
    for step in 0..count {
        let Some(element) = elements.get(flat + step) else {
            return NdarrayErr::OutOfBounds;
        };
        let slot = &mut dst[step * width..(step + 1) * width];
        let err = write_converted(element, target, slot);
        if err != NdarrayErr::Ok {
            return err;
        }
    }
    NdarrayErr::Ok
}

/// Hand a buffer back through `dst`, allocating with `malloc` if asked.
///
/// libasdf lets the caller either supply storage or take a fresh allocation
/// by pointing `dst` at a null pointer, in which case the caller frees it
/// with `free` -- so `malloc` rather than Rust's allocator.
fn deliver(buffer: &[u8], dst: *mut *mut c_void) -> NdarrayErr {
    if dst.is_null() {
        return NdarrayErr::Inval;
    }
    let existing = unsafe { *dst };
    if existing.is_null() {
        let allocation = unsafe { libc::malloc(buffer.len().max(1)) };
        if allocation.is_null() {
            return NdarrayErr::Oom;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(buffer.as_ptr(), allocation.cast::<u8>(), buffer.len());
            *dst = allocation;
        }
    } else {
        unsafe {
            std::ptr::copy_nonoverlapping(buffer.as_ptr(), existing.cast::<u8>(), buffer.len());
        }
    }
    NdarrayErr::Ok
}

/// Read one element, converting to `dst_t`.
///
/// # Safety
/// `ndarray` must be a valid `asdf_ndarray_t`; `indices` must point to `ndim`
/// values; `dst` must have room for one value of `dst_t`. `dst` need not be
/// aligned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_read_at(
    ndarray: *mut asdf_ndarray_t,
    indices: *const u64,
    dst_t: ScalarTypeAbi,
    dst: *mut c_void,
) -> NdarrayErr {
    guard("asdf_ndarray_read_at", NdarrayErr::Inval, || {
        let Some(state) = state_of(ndarray) else {
            return NdarrayErr::Inval;
        };
        if indices.is_null() || dst.is_null() {
            return NdarrayErr::Inval;
        }
        let idx = unsafe { std::slice::from_raw_parts(indices, state.shape.len()) };
        let Some(flat) = flat_index(&state.shape, idx) else {
            return NdarrayErr::OutOfBounds;
        };
        let Some(elements) = elements_of(state) else {
            return NdarrayErr::Inval;
        };
        let Some(element) = elements.get(flat) else {
            return NdarrayErr::OutOfBounds;
        };

        let target = match scalar_from_abi(dst_t) {
            ScalarType::Unknown => state.parsed.datatype.scalar,
            other => other,
        };
        let Ok(width) = usize::try_from(target.size()) else {
            return NdarrayErr::Inval;
        };
        if width == 0 {
            return NdarrayErr::Conversion;
        }
        // Written into a local first because `dst` carries no alignment
        // guarantee, then copied out byte by byte.
        let mut scratch = vec![0u8; width];
        let err = write_converted(element, target, &mut scratch);
        if err != NdarrayErr::Ok {
            return err;
        }
        unsafe { std::ptr::copy_nonoverlapping(scratch.as_ptr(), dst.cast::<u8>(), width) };
        NdarrayErr::Ok
    })
}

/// Read an N-dimensional tile, converting to `dst_t`.
///
/// # Safety
/// `origin` and `shape` must each point to `ndim` values; `dst` must be
/// writable, pointing either at storage large enough for the tile or at a
/// null pointer, in which case a buffer is allocated for the caller to
/// `free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_ndarray_read_tile_ndim(
    ndarray: *mut asdf_ndarray_t,
    origin: *const u64,
    shape: *const u64,
    dst_t: ScalarTypeAbi,
    dst: *mut *mut c_void,
) -> NdarrayErr {
    guard("asdf_ndarray_read_tile_ndim", NdarrayErr::Inval, || {
        let Some(state) = state_of(ndarray) else {
            return NdarrayErr::Inval;
        };
        if origin.is_null() || shape.is_null() {
            return NdarrayErr::Inval;
        }
        let ndim = state.shape.len();
        if ndim == 0 {
            return NdarrayErr::Inval;
        }
        let origin = unsafe { std::slice::from_raw_parts(origin, ndim) }.to_vec();
        let tile = unsafe { std::slice::from_raw_parts(shape, ndim) }.to_vec();

        // Every corner of the tile has to land inside the array.
        for axis in 0..ndim {
            let Some(end) = origin[axis].checked_add(tile[axis]) else {
                return NdarrayErr::OutOfBounds;
            };
            if end > state.shape[axis] {
                return NdarrayErr::OutOfBounds;
            }
        }

        let target = match scalar_from_abi(dst_t) {
            ScalarType::Unknown => state.parsed.datatype.scalar,
            other => other,
        };
        let Ok(width) = usize::try_from(target.size()) else {
            return NdarrayErr::Inval;
        };
        if width == 0 {
            return NdarrayErr::Conversion;
        }

        let mut count: u64 = 1;
        for extent in &tile {
            let Some(next) = count.checked_mul(*extent) else {
                return NdarrayErr::Inval;
            };
            count = next;
        }
        let Ok(count) = usize::try_from(count) else {
            return NdarrayErr::Inval;
        };
        if count == 0 {
            return deliver(&[], dst);
        }

        let Some(elements) = elements_of(state) else {
            return NdarrayErr::Inval;
        };

        // The tile is contiguous along the last axis only, so copy it one
        // run at a time and step the outer indices by hand.
        let run = tile[ndim - 1] as usize;
        let mut buffer = vec![0u8; count * width];
        let mut cursor = origin.clone();
        let mut written = 0usize;
        loop {
            let Some(flat) = flat_index(&state.shape, &cursor) else {
                return NdarrayErr::OutOfBounds;
            };
            let slice = &mut buffer[written * width..(written + run) * width];
            let err = write_elements(&elements, flat, run, target, width, slice);
            if err != NdarrayErr::Ok {
                return err;
            }
            written += run;

            // Advance the outer axes odometer-style; the last is the run.
            let mut axis = ndim as isize - 2;
            loop {
                if axis < 0 {
                    return deliver(&buffer, dst);
                }
                let a = axis as usize;
                cursor[a] += 1;
                if cursor[a] < origin[a] + tile[a] {
                    break;
                }
                cursor[a] = origin[a];
                axis -= 1;
            }
        }
    })
}

/// Read a 2-D tile, converting to `dst_t`.
///
/// For an array of more than two dimensions, `plane_origin` gives the
/// `ndim - 2` outer coordinates; null selects the first plane. `x`/`width`
/// index the last axis and `y`/`height` the one before it.
///
/// # Safety
/// See [`asdf_ndarray_read_tile_ndim`]; `plane_origin`, when not null, must
/// point to `ndim - 2` values.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn asdf_ndarray_read_tile_2d(
    ndarray: *mut asdf_ndarray_t,
    x: u64,
    y: u64,
    width: u64,
    height: u64,
    plane_origin: *const u64,
    dst_t: ScalarTypeAbi,
    dst: *mut *mut c_void,
) -> NdarrayErr {
    guard("asdf_ndarray_read_tile_2d", NdarrayErr::Inval, || {
        let Some(state) = state_of(ndarray) else {
            return NdarrayErr::Inval;
        };
        let ndim = state.shape.len();
        if ndim < 2 {
            return NdarrayErr::Inval;
        }
        let planes = ndim - 2;

        let mut origin = vec![0u64; ndim];
        let mut tile = vec![1u64; ndim];
        if planes > 0 && !plane_origin.is_null() {
            let outer = unsafe { std::slice::from_raw_parts(plane_origin, planes) };
            origin[..planes].copy_from_slice(outer);
        }
        origin[ndim - 2] = y;
        origin[ndim - 1] = x;
        tile[ndim - 2] = height;
        tile[ndim - 1] = width;

        unsafe { asdf_ndarray_read_tile_ndim(ndarray, origin.as_ptr(), tile.as_ptr(), dst_t, dst) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use asdf_core::core::datatype::ByteOrder;
    use asdf_core::core::ndarray::Ndarray as CoreNdarray;
    use asdf_core::yaml::parse_document;

    /// Build a handle over a little-endian int32 array of the given values.
    fn int32_array(values: &[i32]) -> *mut asdf_ndarray_t {
        let doc = parse_document(&format!(
            "a:\n  source: 0\n  shape: [{}]\n  datatype: int32\n  byteorder: little\n",
            values.len()
        ))
        .unwrap();
        let root = doc.root().unwrap();
        let parsed = CoreNdarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();

        let array = make_ndarray(parsed, vec![values.len() as u64]);
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        set_data(array, bytes);
        array
    }

    /// A 3x4 little-endian int32 array holding 0..12 in row-major order.
    fn int32_grid() -> *mut asdf_ndarray_t {
        let doc = parse_document(
            "a:\n  source: 0\n  shape: [3, 4]\n  datatype: int32\n  byteorder: little\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let parsed = CoreNdarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();
        let array = make_ndarray(parsed, vec![3, 4]);
        let bytes: Vec<u8> = (0i32..12).flat_map(i32::to_le_bytes).collect();
        set_data(array, bytes);
        array
    }

    #[test]
    fn reads_one_element_at_indices() {
        let array = int32_grid();
        let indices: [u64; 2] = [2, 1];
        let mut out: i32 = 0;
        assert_eq!(
            unsafe {
                asdf_ndarray_read_at(
                    array,
                    indices.as_ptr(),
                    ScalarType::Int32 as ScalarTypeAbi,
                    std::ptr::from_mut(&mut out).cast(),
                )
            },
            NdarrayErr::Ok
        );
        assert_eq!(out, 9, "row 2, column 1 of a 3x4 array holding 0..12");

        // Converting on the way out is allowed.
        let mut wide: f64 = 0.0;
        assert_eq!(
            unsafe {
                asdf_ndarray_read_at(
                    array,
                    indices.as_ptr(),
                    ScalarType::Float64 as ScalarTypeAbi,
                    std::ptr::from_mut(&mut wide).cast(),
                )
            },
            NdarrayErr::Ok
        );
        assert!((wide - 9.0).abs() < f64::EPSILON);

        let past_end: [u64; 2] = [3, 0];
        assert_eq!(
            unsafe {
                asdf_ndarray_read_at(
                    array,
                    past_end.as_ptr(),
                    ScalarType::Int32 as ScalarTypeAbi,
                    std::ptr::from_mut(&mut out).cast(),
                )
            },
            NdarrayErr::OutOfBounds
        );
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn reads_a_2d_tile() {
        let array = int32_grid();
        // A 2x2 tile whose origin is (x=1, y=1): rows 1-2, columns 1-2.
        let mut buffer = [0i32; 4];
        let mut dst = buffer.as_mut_ptr().cast::<c_void>();
        assert_eq!(
            unsafe {
                asdf_ndarray_read_tile_2d(
                    array,
                    1,
                    1,
                    2,
                    2,
                    std::ptr::null(),
                    ScalarType::Int32 as ScalarTypeAbi,
                    &mut dst,
                )
            },
            NdarrayErr::Ok
        );
        assert_eq!(buffer, [5, 6, 9, 10]);
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn a_tile_may_not_run_past_the_edge() {
        let array = int32_grid();
        let mut buffer = [0i32; 4];
        let mut dst = buffer.as_mut_ptr().cast::<c_void>();
        assert_eq!(
            unsafe {
                asdf_ndarray_read_tile_2d(
                    array,
                    3,
                    1,
                    2,
                    2,
                    std::ptr::null(),
                    ScalarType::Int32 as ScalarTypeAbi,
                    &mut dst,
                )
            },
            NdarrayErr::OutOfBounds
        );
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn an_ndim_tile_can_allocate_its_own_buffer() {
        let array = int32_grid();
        let origin: [u64; 2] = [0, 2];
        let shape: [u64; 2] = [3, 2];
        // A null destination asks the library to allocate.
        let mut dst: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                asdf_ndarray_read_tile_ndim(
                    array,
                    origin.as_ptr(),
                    shape.as_ptr(),
                    ScalarType::Int32 as ScalarTypeAbi,
                    &mut dst,
                )
            },
            NdarrayErr::Ok
        );
        assert!(!dst.is_null());
        let got = unsafe { std::slice::from_raw_parts(dst.cast::<i32>(), 6) };
        assert_eq!(got, [2, 3, 6, 7, 10, 11]);
        unsafe { libc::free(dst) };
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn an_inline_array_has_no_block() {
        let array = int32_grid();
        // Built without a file behind it, so there is no block to hand back.
        assert!(unsafe { asdf_ndarray_block(array) }.is_null());
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn reports_shape_and_sizes_through_the_public_fields() {
        let array = int32_array(&[1, 2, 3, 4]);
        let view = unsafe { &*array };

        assert_eq!(view.ndim, 1);
        assert_eq!(view.source, 0);
        assert!(!view.shape.is_null());
        assert_eq!(unsafe { *view.shape }, 4);
        assert_eq!(view.byteorder, ByteOrder::Little as i32);

        assert_eq!(unsafe { asdf_ndarray_size(array) }, 4);
        assert_eq!(unsafe { asdf_ndarray_nbytes(array) }, 16);

        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn datatype_size_is_computed_when_left_zero() {
        let mut dt = asdf_datatype_t {
            type_: scalar_abi(ScalarType::Float64),
            size: 0,
            name: std::ptr::null(),
            byteorder: 0,
            ndim: 0,
            shape: std::ptr::null(),
            nfields: 0,
            fields: std::ptr::null(),
        };
        assert_eq!(unsafe { asdf_datatype_size(&mut dt) }, 8);
        // ...and written back, as the header documents.
        assert_eq!(dt.size, 8);
    }

    #[test]
    fn scalar_type_names_round_trip() {
        for name in ["int8", "uint64", "float32", "complex128", "bool8", "ascii", "ucs4"] {
            let c = CString::new(name).unwrap();
            let code = unsafe { asdf_scalar_datatype_from_string(c.as_ptr()) };
            assert_ne!(code, 0, "{name}");
            let back = unsafe { CStr::from_ptr(asdf_scalar_datatype_to_string(code)) };
            assert_eq!(back.to_str().unwrap(), name);
        }
        // An unknown name is UNKNOWN, not a crash.
        let bogus = CString::new("float128").unwrap();
        assert_eq!(unsafe { asdf_scalar_datatype_from_string(bogus.as_ptr()) }, 0);
    }

    #[test]
    fn reads_the_whole_array_in_its_own_type() {
        let array = int32_array(&[10, -20, 30]);
        let mut dst: *mut c_void = std::ptr::null_mut();
        // ASDF_DATATYPE_SOURCE is UNKNOWN, meaning "keep the source type".
        assert_eq!(unsafe { asdf_ndarray_read_all(array, 0, &mut dst) }, NdarrayErr::Ok);
        assert!(!dst.is_null());
        let values = unsafe { std::slice::from_raw_parts(dst.cast::<i32>(), 3) };
        assert_eq!(values, [10, -20, 30]);

        unsafe { libc::free(dst) };
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn reads_the_whole_array_converted() {
        let array = int32_array(&[1, 2, 3]);
        let mut dst: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_ndarray_read_all(array, scalar_abi(ScalarType::Float64), &mut dst) },
            NdarrayErr::Ok
        );
        let values = unsafe { std::slice::from_raw_parts(dst.cast::<f64>(), 3) };
        assert_eq!(values, [1.0, 2.0, 3.0]);
        unsafe { libc::free(dst) };
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn read_all_fills_a_caller_supplied_buffer() {
        let array = int32_array(&[7, 8]);
        let mut buffer = [0i32; 2];
        let mut dst: *mut c_void = buffer.as_mut_ptr().cast();
        assert_eq!(unsafe { asdf_ndarray_read_all(array, 0, &mut dst) }, NdarrayErr::Ok);
        assert_eq!(buffer, [7, 8]);
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn converting_a_value_that_does_not_fit_overflows() {
        let array = int32_array(&[1000]);
        let mut dst: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            unsafe { asdf_ndarray_read_all(array, scalar_abi(ScalarType::Int8), &mut dst) },
            NdarrayErr::Overflow
        );
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn reads_single_elements_by_index() {
        let array = int32_array(&[5, 6, 7]);

        for (index, expected) in [(0u64, 5i64), (1, 6), (2, 7)] {
            let mut err: c_int = -1;
            let value = unsafe { asdf_ndarray_read_int64_at(array, &index, &mut err) };
            assert_eq!(err, NdarrayErr::Ok as c_int);
            assert_eq!(value, expected);
        }
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn out_of_bounds_reads_are_reported() {
        let array = int32_array(&[1, 2]);
        let index = 5u64;
        let mut err: c_int = -1;
        let value = unsafe { asdf_ndarray_read_int64_at(array, &index, &mut err) };
        assert_eq!(err, NdarrayErr::OutOfBounds as c_int);
        assert_eq!(value, 0, "a failed read returns a zero value");
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn single_element_reads_convert_and_overflow() {
        let array = int32_array(&[300]);
        let index = 0u64;

        let mut err: c_int = -1;
        let wide = unsafe { asdf_ndarray_read_float64_at(array, &index, &mut err) };
        assert_eq!(err, NdarrayErr::Ok as c_int);
        assert_eq!(wide, 300.0);

        let mut err: c_int = -1;
        let narrow = unsafe { asdf_ndarray_read_uint8_at(array, &index, &mut err) };
        assert_eq!(err, NdarrayErr::Conversion as c_int);
        assert_eq!(narrow, 0);

        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn float16_reads_go_through_the_bit_pattern() {
        let array = int32_array(&[3]);
        let index = 0u64;
        let mut err: c_int = -1;
        let bits =
            unsafe { asdf_shim_ndarray_read_float16_bits_at(array.cast(), &index, &mut err) };
        assert_eq!(err, NdarrayErr::Ok as c_int);
        assert_eq!(half::f16::from_bits(bits).to_f64(), 3.0);
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn data_alloc_is_idempotent_and_freeable() {
        let array = int32_array(&[0; 4]);
        let first = unsafe { asdf_ndarray_data_alloc(array) };
        let second = unsafe { asdf_ndarray_data_alloc(array) };
        assert!(!first.is_null());
        assert_eq!(first, second, "repeated calls return the same buffer");

        unsafe { asdf_ndarray_data_dealloc(array) };
        // Deallocating twice must be a no-op, as the header documents.
        unsafe { asdf_ndarray_data_dealloc(array) };
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn data_copy_fills_the_allocated_buffer() {
        let array = int32_array(&[0; 3]);
        let source: Vec<i32> = vec![11, 22, 33];
        assert_eq!(
            unsafe { asdf_ndarray_data_copy(array, source.as_ptr().cast()) },
            NdarrayErr::Ok
        );

        let mut size = 0usize;
        let data = unsafe { asdf_ndarray_data(array, &mut size) };
        assert_eq!(size, 12);
        let values = unsafe { std::slice::from_raw_parts(data.cast::<i32>(), 3) };
        assert_eq!(values, [11, 22, 33]);

        unsafe { asdf_ndarray_data_dealloc(array) };
        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn compression_and_storage_settings() {
        let array = int32_array(&[1]);

        let zlib = CString::new("zlib").unwrap();
        assert_eq!(unsafe { asdf_ndarray_compression_set(array, zlib.as_ptr()) }, 0);
        let bogus = CString::new("zstd").unwrap();
        assert_eq!(unsafe { asdf_ndarray_compression_set(array, bogus.as_ptr()) }, -1);

        // Internal is the default when nothing has been set.
        assert_eq!(unsafe { asdf_ndarray_storage(array) }, AsdfArrayStorage::Internal);
        unsafe { asdf_ndarray_storage_set(array, AsdfArrayStorage::Inline) };
        assert_eq!(unsafe { asdf_ndarray_storage(array) }, AsdfArrayStorage::Inline);

        // External is not supported and must leave the setting alone.
        unsafe { asdf_ndarray_storage_set(array, AsdfArrayStorage::External) };
        assert_eq!(unsafe { asdf_ndarray_storage(array) }, AsdfArrayStorage::Inline);

        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn a_compound_datatype_exposes_its_fields() {
        let doc = parse_document(
            "a:\n  source: 0\n  shape: [2]\n  byteorder: little\n  \
             datatype:\n    - name: x\n      datatype: float64\n    \
             - name: y\n      datatype: int32\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let parsed = CoreNdarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();
        let array = make_ndarray(parsed, vec![2]);

        let view = unsafe { &*array };
        assert_eq!(view.datatype.nfields, 2);
        assert!(!view.datatype.fields.is_null());

        let fields = unsafe { std::slice::from_raw_parts(view.datatype.fields, 2) };
        assert_eq!(fields[0].type_, scalar_abi(ScalarType::Float64));
        assert_eq!(fields[0].size, 8);
        assert_eq!(unsafe { CStr::from_ptr(fields[0].name) }.to_str().unwrap(), "x");
        assert_eq!(fields[1].type_, scalar_abi(ScalarType::Int32));
        assert_eq!(unsafe { CStr::from_ptr(fields[1].name) }.to_str().unwrap(), "y");

        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn multi_dimensional_indexing() {
        let doc = parse_document(
            "a:\n  source: 0\n  shape: [2, 3]\n  datatype: uint8\n  byteorder: little\n",
        )
        .unwrap();
        let root = doc.root().unwrap();
        let parsed = CoreNdarray::parse(&doc, doc.mapping_get(root, "a").unwrap()).unwrap();
        let array = make_ndarray(parsed, vec![2, 3]);
        set_data(array, vec![1, 2, 3, 4, 5, 6]);

        // Row-major: [1][2] is the sixth element.
        let indices = [1u64, 2];
        let mut err: c_int = -1;
        let value = unsafe { asdf_ndarray_read_uint8_at(array, indices.as_ptr(), &mut err) };
        assert_eq!(err, NdarrayErr::Ok as c_int);
        assert_eq!(value, 6);

        // Out of range in the second dimension only.
        let bad = [0u64, 3];
        let mut err: c_int = -1;
        unsafe { asdf_ndarray_read_uint8_at(array, bad.as_ptr(), &mut err) };
        assert_eq!(err, NdarrayErr::OutOfBounds as c_int);

        unsafe { asdf_ndarray_destroy(array) };
    }

    #[test]
    fn err_discriminants_match_the_c_abi() {
        assert_eq!(NdarrayErr::Ok as i32, 0);
        assert_eq!(NdarrayErr::OutOfBounds as i32, 1);
        assert_eq!(NdarrayErr::Oom as i32, 2);
        assert_eq!(NdarrayErr::Inval as i32, 3);
        assert_eq!(NdarrayErr::Overflow as i32, 4);
        assert_eq!(NdarrayErr::Conversion as i32, 5);
    }

    #[test]
    fn null_handles_are_tolerated() {
        let null: *mut asdf_ndarray_t = std::ptr::null_mut();
        assert_eq!(unsafe { asdf_ndarray_size(null) }, 0);
        assert_eq!(unsafe { asdf_ndarray_nbytes(null) }, 0);
        assert!(unsafe { asdf_ndarray_data_alloc(null) }.is_null());
        unsafe { asdf_ndarray_data_dealloc(null) };
        assert_eq!(unsafe { asdf_ndarray_data_copy(null, std::ptr::null()) }, NdarrayErr::Inval);
        assert_eq!(unsafe { asdf_datatype_size(std::ptr::null_mut()) }, 0);
        unsafe { asdf_ndarray_destroy(null) };

        let mut size = 99usize;
        assert!(unsafe { asdf_ndarray_data(null, &mut size) }.is_null());
        assert_eq!(size, 0);
    }
}
