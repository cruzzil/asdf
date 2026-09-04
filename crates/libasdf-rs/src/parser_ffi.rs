//! `asdf/parser.h`, `asdf/event.h` and `asdf/yaml.h`: the low-level
//! event-based parser.
//!
//! This is libasdf's streaming interface: rather than building a tree, it
//! walks a file and reports what it finds -- the version headers, any
//! comments, the block index, the tree's extent, optionally the YAML events
//! inside it, then each block, then the end.
//!
//! # Ownership
//!
//! The header's contract is unusual and is reproduced rather than
//! simplified:
//!
//! - `asdf_parser_parse` hands out an event the *caller* releases with
//!   `asdf_event_free`.
//! - `asdf_event_iterate` releases the event it handed out last time before
//!   producing the next, so a loop over it frees nothing by hand.
//! - Anything still outstanding is released by `asdf_parser_destroy`, so a
//!   caller that abandons a parse leaks nothing.
//!
//! An event's strings belong to the event and stay valid until it is freed.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

use asdf_core::layout::{self, Layout};
use asdf_core::yaml::{YamlEvent, YamlEventKind};

use asdf_core::ErrorCode;

use crate::error_ffi::ErrorState;
use crate::panic::guard;
use crate::types::{
    AsdfEventType, AsdfParserOptFlags, AsdfYamlEventType, asdf_block_header_t, asdf_block_info_t,
    asdf_parser_cfg_t, asdf_tree_info_t, parser_opt,
};

/// What an event carries beyond its type.
#[derive(Debug)]
enum Payload {
    /// Nothing; the type says all there is to say.
    None,
    /// An `#ASDF` or `#ASDF_STANDARD` version, as written.
    Version(CString),
    /// A comment line, without its leading `#`.
    Comment(CString),
    /// The tree's extent, and its text when buffering was asked for.
    Tree(Box<asdf_tree_info_t>, Option<CString>),
    /// A YAML sub-event.
    Yaml(YamlSub),
    /// A block's position and header.
    Block(Box<asdf_block_info_t>),
    /// The offsets listed in the file's block index.
    BlockIndex(Vec<i64>),
}

/// A YAML sub-event, with its strings owned.
#[derive(Debug)]
struct YamlSub {
    kind: AsdfYamlEventType,
    tag: Option<CString>,
    value: Option<CString>,
}

/// A parser event. Opaque to C.
///
/// The type sits first so that code built against upstream's internal header
/// -- which is how its own test suite reads events -- sees it where it
/// expects.
#[repr(C)]
#[derive(Debug)]
pub struct AsdfEvent {
    event_type: AsdfEventType,
    payload: Payload,
}

/// The plan for one event, before its C-visible form is built.
///
/// The whole stream is worked out when the input is set, because the layout
/// scan reads the file end-to-end anyway; each entry is then turned into an
/// `AsdfEvent` on demand.
#[derive(Debug)]
enum Step {
    Version(bool, String),
    Comment(String),
    BlockIndex(Vec<u64>),
    TreeStart(usize),
    Yaml(YamlEvent),
    TreeEnd(usize, usize),
    Block(usize),
    End,
}

/// A parser handle. Opaque to C.
#[derive(Debug)]
pub struct AsdfParser {
    flags: AsdfParserOptFlags,
    error: ErrorState,
    /// The file's bytes, once an input is set.
    buffer: Vec<u8>,
    /// The name reported in messages, when the input came from a path.
    filename: Option<CString>,
    /// The scan of the file, once an input is set.
    layout: Option<Layout>,
    /// The remaining events to produce.
    steps: std::collections::VecDeque<Step>,
    /// Events handed out and not yet freed.
    live: Vec<*mut AsdfEvent>,
    /// The event `asdf_event_iterate` produced last, which it frees on the
    /// next call.
    iterated: *mut AsdfEvent,
    /// Set once the stream has run out.
    finished: bool,
}

impl AsdfParser {
    fn new(flags: AsdfParserOptFlags) -> Self {
        Self {
            flags,
            error: ErrorState::default(),
            buffer: Vec::new(),
            filename: None,
            layout: None,
            steps: std::collections::VecDeque::new(),
            live: Vec::new(),
            iterated: std::ptr::null_mut(),
            finished: false,
        }
    }

    fn emits_yaml(&self) -> bool {
        self.flags & parser_opt::EMIT_YAML_EVENTS != 0
    }

    fn buffers_tree(&self) -> bool {
        self.flags & parser_opt::BUFFER_TREE != 0
    }

    /// Scan the buffer and lay out the whole event stream.
    ///
    /// Returns 0 on success and -1 on failure, which is what the
    /// `asdf_parser_set_input_*` functions report.
    fn ingest(&mut self, buffer: Vec<u8>) -> c_int {
        self.buffer = buffer;
        self.steps.clear();
        self.finished = false;

        let layout = match layout::scan(&self.buffer) {
            Ok(l) => l,
            Err(e) => {
                self.error.set_error(&e);
                return -1;
            }
        };

        self.steps.push_back(Step::Version(false, layout.format_version.to_string()));
        if let Some(standard) = &layout.standard_version {
            self.steps.push_back(Step::Version(true, standard.to_string()));
        }
        for comment in &layout.comments {
            self.steps.push_back(Step::Comment(comment.clone()));
        }
        if layout.block_index_pos.is_some() && !layout.block_index_offsets.is_empty() {
            self.steps.push_back(Step::BlockIndex(layout.block_index_offsets.clone()));
        }
        if let Some(tree) = layout.tree.clone() {
            self.steps.push_back(Step::TreeStart(tree.start));
            if self.emits_yaml()
                && let Some(text) = layout.tree_str(&self.buffer)
                && let Ok(events) = asdf_core::yaml::scan_events(text)
            {
                self.steps.extend(events.into_iter().map(Step::Yaml));
            }
            self.steps.push_back(Step::TreeEnd(tree.start, tree.end));
        }
        for index in 0..layout.blocks.len() {
            self.steps.push_back(Step::Block(index));
        }
        self.steps.push_back(Step::End);

        self.layout = Some(layout);
        self.error.clear();
        0
    }

    /// Turn the next planned step into an event the caller owns.
    fn next_event(&mut self) -> *mut AsdfEvent {
        let Some(step) = self.steps.pop_front() else {
            self.finished = true;
            return std::ptr::null_mut();
        };
        let event = match step {
            Step::Version(is_standard, text) => {
                let kind = if is_standard {
                    AsdfEventType::StandardVersion
                } else {
                    AsdfEventType::AsdfVersion
                };
                let Ok(owned) = CString::new(text) else {
                    return std::ptr::null_mut();
                };
                AsdfEvent { event_type: kind, payload: Payload::Version(owned) }
            }
            Step::Comment(text) => {
                let Ok(owned) = CString::new(text) else {
                    return std::ptr::null_mut();
                };
                AsdfEvent { event_type: AsdfEventType::Comment, payload: Payload::Comment(owned) }
            }
            Step::BlockIndex(offsets) => AsdfEvent {
                event_type: AsdfEventType::BlockIndex,
                payload: Payload::BlockIndex(
                    offsets.iter().map(|o| i64::try_from(*o).unwrap_or(i64::MAX)).collect(),
                ),
            },
            Step::TreeStart(start) => {
                let info = Box::new(asdf_tree_info_t { start, end: 0, buf: std::ptr::null() });
                AsdfEvent {
                    event_type: AsdfEventType::TreeStart,
                    payload: Payload::Tree(info, None),
                }
            }
            Step::TreeEnd(start, end) => {
                let text = self
                    .buffers_tree()
                    .then(|| {
                        self.buffer.get(start..end).and_then(|b| CString::new(b.to_vec()).ok())
                    })
                    .flatten();
                let buf = text.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
                let info = Box::new(asdf_tree_info_t { start, end, buf });
                AsdfEvent { event_type: AsdfEventType::TreeEnd, payload: Payload::Tree(info, text) }
            }
            Step::Yaml(event) => AsdfEvent {
                event_type: AsdfEventType::Yaml,
                payload: Payload::Yaml(yaml_sub(&event)),
            },
            Step::Block(index) => {
                let Some(location) = self.layout.as_ref().and_then(|l| l.blocks.get(index)) else {
                    return std::ptr::null_mut();
                };
                let header = &location.header;
                let info = Box::new(asdf_block_info_t {
                    index: location.index,
                    header_pos: i64::try_from(location.header_pos).unwrap_or(i64::MAX),
                    data_pos: i64::try_from(location.data_pos).unwrap_or(i64::MAX),
                    header: asdf_block_header_t {
                        header_size: header.header_size,
                        flags: header.flags,
                        compression: header.compression,
                        allocated_size: header.allocated_size,
                        used_size: header.used_size,
                        data_size: header.data_size,
                        checksum: header.checksum,
                    },
                });
                AsdfEvent { event_type: AsdfEventType::Block, payload: Payload::Block(info) }
            }
            Step::End => AsdfEvent { event_type: AsdfEventType::End, payload: Payload::None },
        };

        let handle = Box::into_raw(Box::new(event));
        self.live.push(handle);
        handle
    }

    /// Drop an event this parser handed out. Unknown pointers are ignored.
    fn release(&mut self, event: *mut AsdfEvent) {
        let Some(position) = self.live.iter().position(|e| *e == event) else {
            return;
        };
        self.live.remove(position);
        if self.iterated == event {
            self.iterated = std::ptr::null_mut();
        }
        drop(unsafe { Box::from_raw(event) });
    }
}

impl Drop for AsdfParser {
    fn drop(&mut self) {
        for event in std::mem::take(&mut self.live) {
            drop(unsafe { Box::from_raw(event) });
        }
    }
}

/// Convert an engine YAML event into its C-visible form.
fn yaml_sub(event: &YamlEvent) -> YamlSub {
    let kind = match event.kind {
        YamlEventKind::StreamStart => AsdfYamlEventType::StreamStart,
        YamlEventKind::StreamEnd => AsdfYamlEventType::StreamEnd,
        YamlEventKind::DocumentStart => AsdfYamlEventType::DocumentStart,
        YamlEventKind::DocumentEnd => AsdfYamlEventType::DocumentEnd,
        YamlEventKind::MappingStart => AsdfYamlEventType::MappingStart,
        YamlEventKind::MappingEnd => AsdfYamlEventType::MappingEnd,
        YamlEventKind::SequenceStart => AsdfYamlEventType::SequenceStart,
        YamlEventKind::SequenceEnd => AsdfYamlEventType::SequenceEnd,
        YamlEventKind::Scalar => AsdfYamlEventType::Scalar,
        YamlEventKind::Alias => AsdfYamlEventType::Alias,
    };
    YamlSub {
        kind,
        tag: event.tag.as_ref().and_then(|t| CString::new(t.as_str()).ok()),
        value: event.value.as_ref().and_then(|v| CString::new(v.as_str()).ok()),
    }
}

fn parser_ref<'a>(parser: *const AsdfParser) -> Option<&'a AsdfParser> {
    (!parser.is_null()).then(|| unsafe { &*parser })
}

fn parser_mut<'a>(parser: *mut AsdfParser) -> Option<&'a mut AsdfParser> {
    (!parser.is_null()).then(|| unsafe { &mut *parser })
}

fn event_ref<'a>(event: *const AsdfEvent) -> Option<&'a AsdfEvent> {
    (!event.is_null()).then(|| unsafe { &*event })
}

// ---- Parser lifecycle ------------------------------------------------

/// Create a parser.
///
/// `config` may be null, which selects the defaults: no YAML events and no
/// tree buffering.
///
/// # Safety
/// `config` must be null or point to a valid `asdf_parser_cfg_t`. The result
/// must be released with [`asdf_parser_destroy`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_create(config: *const asdf_parser_cfg_t) -> *mut AsdfParser {
    guard("asdf_parser_create", std::ptr::null_mut(), || {
        let flags = if config.is_null() { 0 } else { unsafe { &*config }.flags };
        Box::into_raw(Box::new(AsdfParser::new(flags)))
    })
}

/// Release a parser and everything it handed out.
///
/// # Safety
/// `parser` must be null or a handle from [`asdf_parser_create`] that has not
/// already been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_destroy(parser: *mut AsdfParser) {
    guard("asdf_parser_destroy", (), || {
        if parser.is_null() {
            return;
        }
        drop(unsafe { Box::from_raw(parser) });
    })
}

/// Read a file and lay out its event stream. Returns 0 on success.
///
/// # Safety
/// `parser` must be a valid handle and `filename` a valid string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_set_input_file(
    parser: *mut AsdfParser,
    filename: *const c_char,
) -> c_int {
    guard("asdf_parser_set_input_file", -1, || {
        let Some(state) = parser_mut(parser) else {
            return -1;
        };
        if filename.is_null() {
            state.error.set(ErrorCode::InvalidArgument as i32, "no filename given");
            return -1;
        }
        let name = unsafe { CStr::from_ptr(filename) };
        let path = std::path::PathBuf::from(name.to_string_lossy().into_owned());
        match std::fs::read(&path) {
            Ok(bytes) => {
                state.filename = Some(name.to_owned());
                state.ingest(bytes)
            }
            Err(e) => {
                state.error.set_system(e.raw_os_error().unwrap_or(0));
                -1
            }
        }
    })
}

/// Read an already-open stream. Returns 0 on success.
///
/// The whole stream is read up front, so `fp` may be closed once this
/// returns. `filename` is optional and is used only in messages.
///
/// # Safety
/// `fp` must be a `FILE *` open for reading, positioned where the ASDF file
/// begins.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_set_input_fp(
    parser: *mut AsdfParser,
    fp: *mut c_void,
    filename: *const c_char,
) -> c_int {
    guard("asdf_parser_set_input_fp", -1, || {
        let Some(state) = parser_mut(parser) else {
            return -1;
        };
        if fp.is_null() {
            state.error.set(ErrorCode::InvalidArgument as i32, "no stream given");
            return -1;
        }
        if !filename.is_null() {
            state.filename = Some(unsafe { CStr::from_ptr(filename) }.to_owned());
        }
        match read_stream(fp) {
            Some(bytes) => state.ingest(bytes),
            None => {
                state.error.set(ErrorCode::System as i32, "could not read the stream");
                -1
            }
        }
    })
}

/// Read a buffer. Returns 0 on success.
///
/// The bytes are copied, so `buf` need not outlive the call.
///
/// # Safety
/// `buf` must point to at least `size` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_set_input_mem(
    parser: *mut AsdfParser,
    buf: *const c_void,
    size: usize,
) -> c_int {
    guard("asdf_parser_set_input_mem", -1, || {
        let Some(state) = parser_mut(parser) else {
            return -1;
        };
        if buf.is_null() {
            state.error.set(ErrorCode::InvalidArgument as i32, "no buffer given");
            return -1;
        }
        let bytes = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size) }.to_vec();
        state.ingest(bytes)
    })
}

/// Read a whole `FILE *` through `fread`.
fn read_stream(fp: *mut c_void) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read =
            unsafe { libc::fread(chunk.as_mut_ptr().cast::<c_void>(), 1, chunk.len(), fp.cast()) };
        if read > 0 {
            out.extend_from_slice(&chunk[..read]);
        }
        if read < chunk.len() {
            // Short read: either the end, or an error worth reporting.
            if unsafe { libc::ferror(fp.cast()) } != 0 {
                return None;
            }
            return Some(out);
        }
    }
}

/// Produce the next event, or null at the end of the stream.
///
/// The caller owns the event and releases it with [`asdf_event_free`].
///
/// # Safety
/// `parser` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_parse(parser: *mut AsdfParser) -> *mut AsdfEvent {
    guard("asdf_parser_parse", std::ptr::null_mut(), || match parser_mut(parser) {
        Some(state) => state.next_event(),
        None => std::ptr::null_mut(),
    })
}

// ---- Parser errors ---------------------------------------------------

/// Whether the parser has recorded an error.
///
/// # Safety
/// `parser` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_has_error(parser: *const AsdfParser) -> bool {
    guard("asdf_parser_has_error", false, || {
        parser_ref(parser).is_some_and(|p| p.error.code() != 0)
    })
}

/// The recorded error message, or null.
///
/// # Safety
/// `parser` must be null or a valid handle. The string is owned by the parser
/// and is invalidated by the next error it records.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_get_error(parser: *const AsdfParser) -> *const c_char {
    guard("asdf_parser_get_error", std::ptr::null(), || match parser_ref(parser) {
        Some(p) => p.error.message_ptr(),
        None => std::ptr::null(),
    })
}

/// The recorded error code.
///
/// # Safety
/// `parser` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_error_code(parser: *const AsdfParser) -> c_int {
    guard("asdf_parser_error_code", 0, || parser_ref(parser).map_or(0, |p| p.error.code()))
}

/// The recorded `errno`, meaningful only for a system error.
///
/// # Safety
/// `parser` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_parser_error_errno(parser: *const AsdfParser) -> c_int {
    guard("asdf_parser_error_errno", 0, || parser_ref(parser).map_or(0, |p| p.error.errno()))
}

// ---- Events ----------------------------------------------------------

/// An event's type; `ASDF_NONE_EVENT` for a null event.
///
/// # Safety
/// `event` must be null or a valid event handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_event_type(event: *mut AsdfEvent) -> AsdfEventType {
    guard("asdf_event_type", AsdfEventType::None, || match event_ref(event) {
        Some(e) => e.event_type,
        None => AsdfEventType::None,
    })
}

/// The name of an event type, as its enum member is spelled.
///
/// # Safety
/// Always safe; the result is a `'static` string.
#[unsafe(no_mangle)]
pub extern "C" fn asdf_event_type_name(event_type: c_int) -> *const c_char {
    // Taken as an `int`: C may pass any value, and holding one outside the
    // enum's range in a Rust enum is undefined behaviour.
    match AsdfEventType::from_i32(event_type) {
        Some(known) => known.name().as_ptr(),
        None => c"ASDF_UNKNOWN_EVENT".as_ptr(),
    }
}

/// A comment event's text, without its leading `#`, or null.
///
/// # Safety
/// `event` must be null or a valid event handle. The string is owned by the
/// event.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_event_comment(event: *const AsdfEvent) -> *const c_char {
    guard("asdf_event_comment", std::ptr::null(), || match event_ref(event).map(|e| &e.payload) {
        Some(Payload::Comment(text)) => text.as_ptr(),
        _ => std::ptr::null(),
    })
}

/// A tree event's extent, or null for any other event.
///
/// # Safety
/// `event` must be null or a valid event handle. The struct is owned by the
/// event.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_event_tree_info(event: *const AsdfEvent) -> *const asdf_tree_info_t {
    guard("asdf_event_tree_info", std::ptr::null(), || match event_ref(event).map(|e| &e.payload) {
        Some(Payload::Tree(info, _)) => std::ptr::from_ref::<asdf_tree_info_t>(info),
        _ => std::ptr::null(),
    })
}

/// A block event's position and header, or null for any other event.
///
/// # Safety
/// `event` must be null or a valid event handle. The struct is owned by the
/// event.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_event_block_info(
    event: *const AsdfEvent,
) -> *const asdf_block_info_t {
    guard("asdf_event_block_info", std::ptr::null(), || {
        match event_ref(event).map(|e| &e.payload) {
            Some(Payload::Block(info)) => std::ptr::from_ref::<asdf_block_info_t>(info),
            _ => std::ptr::null(),
        }
    })
}

/// Produce the next event, releasing the one this call produced last.
///
/// This is the loop-friendly form: nothing needs freeing by hand.
///
/// # Safety
/// `parser` must be null or a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_event_iterate(parser: *mut AsdfParser) -> *mut AsdfEvent {
    guard("asdf_event_iterate", std::ptr::null_mut(), || {
        let Some(state) = parser_mut(parser) else {
            return std::ptr::null_mut();
        };
        if !state.iterated.is_null() {
            let previous = state.iterated;
            state.release(previous);
        }
        let event = state.next_event();
        state.iterated = event;
        event
    })
}

/// Release an event from [`asdf_parser_parse`].
///
/// # Safety
/// `parser` must be the parser that produced `event`, which must not already
/// have been freed. Both may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_event_free(parser: *mut AsdfParser, event: *mut AsdfEvent) {
    guard("asdf_event_free", (), || {
        if let Some(state) = parser_mut(parser) {
            state.release(event);
        }
    })
}

/// Print an event, in the format `asdf info --events` uses.
///
/// # Safety
/// `event` must be a valid event handle and `file` a `FILE *` open for
/// writing; a null `file` selects `stdout`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_event_print(
    event: *const AsdfEvent,
    file: *mut c_void,
    verbose: bool,
) {
    guard("asdf_event_print", (), || {
        let Some(state) = event_ref(event) else {
            return;
        };
        let mut text = format!("Event: {}\n", state.event_type.name().to_string_lossy());
        if verbose {
            text.push_str(&describe(state));
        }
        write_c_stream(file, &text);
    })
}

/// The verbose body of [`asdf_event_print`], for one event.
fn describe(event: &AsdfEvent) -> String {
    match &event.payload {
        Payload::Version(text) => {
            let label = match event.event_type {
                AsdfEventType::StandardVersion => "Standard Version",
                _ => "ASDF Version",
            };
            format!("  {label}: {}\n", text.to_string_lossy())
        }
        Payload::Comment(text) => format!("  Comment: {}\n", text.to_string_lossy()),
        Payload::Yaml(sub) => {
            let mut out = format!("  Type: {}\n", sub.kind.text().to_string_lossy());
            if let Some(tag) = &sub.tag {
                out.push_str(&format!("  Tag: {}\n", tag.to_string_lossy()));
            }
            if let Some(value) = &sub.value
                && !value.as_bytes().is_empty()
            {
                out.push_str(&format!("  Value: {}\n", value.to_string_lossy()));
            }
            out
        }
        Payload::Tree(info, text) => {
            if event.event_type == AsdfEventType::TreeStart {
                format!("  Tree start position: {} (0x{:x})\n", info.start, info.start)
            } else {
                let mut out = format!("  Tree end position: {} (0x{:x})\n", info.end, info.end);
                if let Some(text) = text {
                    out.push_str(&format!("{}\n", text.to_string_lossy()));
                }
                out
            }
        }
        Payload::Block(info) => {
            let header = &info.header;
            let mut out = String::new();
            out.push_str(&format!(
                "  Header position: {} (0x{:x})\n",
                info.header_pos, info.header_pos
            ));
            out.push_str(&format!("  Data position: {} (0x{:x})\n", info.data_pos, info.data_pos));
            out.push_str(&format!(
                "  Allocated size: {} (0x{:x})\n",
                header.allocated_size, header.allocated_size
            ));
            out.push_str(&format!(
                "  Used size: {} (0x{:x})\n",
                header.used_size, header.used_size
            ));
            out.push_str(&format!(
                "  Data size: {} (0x{:x})\n",
                header.data_size, header.data_size
            ));
            if header.compression[0] != 0 {
                let name = String::from_utf8_lossy(&header.compression);
                out.push_str(&format!("  Compression: {name}\n"));
            }
            out.push_str("  Checksum: ");
            for byte in header.checksum {
                out.push_str(&format!("{byte:02x}"));
            }
            out.push('\n');
            out
        }
        Payload::BlockIndex(offsets) => {
            let listed: Vec<String> = offsets.iter().map(i64::to_string).collect();
            format!("  Offsets: {}\n", listed.join(", "))
        }
        Payload::None => String::new(),
    }
}

/// Write to a C `FILE *`, falling back to `stdout` when it is null.
fn write_c_stream(file: *mut c_void, text: &str) {
    let stream = if file.is_null() { unsafe { stdout_stream() } } else { file.cast() };
    if stream.is_null() {
        return;
    }
    unsafe {
        libc::fwrite(text.as_ptr().cast::<c_void>(), 1, text.len(), stream);
    }
}

/// The process's `stdout`, as a `FILE *`.
unsafe fn stdout_stream() -> *mut libc::FILE {
    // `stdout` is a macro in C; libc exposes it as a function on every
    // platform this builds for.
    unsafe { libc::fdopen(1, c"w".as_ptr()) }
}

// ---- YAML sub-events -------------------------------------------------

/// The YAML sub-event type, or `ASDF_YAML_NONE_EVENT`.
///
/// # Safety
/// `event` must be null or a valid event handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_yaml_event_type(event: *const AsdfEvent) -> AsdfYamlEventType {
    guard("asdf_yaml_event_type", AsdfYamlEventType::None, || {
        match event_ref(event).map(|e| &e.payload) {
            Some(Payload::Yaml(sub)) => sub.kind,
            _ => AsdfYamlEventType::None,
        }
    })
}

/// A human-readable name for the YAML sub-event type.
///
/// Empty for an event that carries none, as upstream's does.
///
/// # Safety
/// `event` must be null or a valid event handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_yaml_event_type_text(event: *const AsdfEvent) -> *const c_char {
    guard("asdf_yaml_event_type_text", c"".as_ptr(), || {
        match event_ref(event).map(|e| &e.payload) {
            Some(Payload::Yaml(sub)) => sub.kind.text().as_ptr(),
            _ => c"".as_ptr(),
        }
    })
}

/// A scalar sub-event's raw text, or null.
///
/// # Safety
/// `event` must be null or a valid event handle and `lenp` writable or null.
/// The string is owned by the event.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_yaml_event_scalar_value(
    event: *const AsdfEvent,
    lenp: *mut usize,
) -> *const c_char {
    guard("asdf_yaml_event_scalar_value", std::ptr::null(), || {
        let text = match event_ref(event).map(|e| &e.payload) {
            Some(Payload::Yaml(sub)) => sub.value.as_ref(),
            _ => None,
        };
        match text {
            Some(value) => {
                if !lenp.is_null() {
                    unsafe { *lenp = value.as_bytes().len() };
                }
                value.as_ptr()
            }
            None => {
                if !lenp.is_null() {
                    unsafe { *lenp = 0 };
                }
                std::ptr::null()
            }
        }
    })
}

/// A YAML sub-event's tag, or null.
///
/// # Safety
/// See [`asdf_yaml_event_scalar_value`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_yaml_event_tag(
    event: *const AsdfEvent,
    lenp: *mut usize,
) -> *const c_char {
    guard("asdf_yaml_event_tag", std::ptr::null(), || {
        let tag = match event_ref(event).map(|e| &e.payload) {
            Some(Payload::Yaml(sub)) => sub.tag.as_ref(),
            _ => None,
        };
        match tag {
            Some(value) => {
                if !lenp.is_null() {
                    unsafe { *lenp = value.as_bytes().len() };
                }
                value.as_ptr()
            }
            None => {
                if !lenp.is_null() {
                    unsafe { *lenp = 0 };
                }
                std::ptr::null()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::parser_opt;

    /// A small file with a tree and one block-free body.
    fn sample() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n#a note\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(b"n: 1\nname: probe\n");
        buf.extend_from_slice(b"...\n");
        buf
    }

    struct Parser(*mut AsdfParser);
    impl Drop for Parser {
        fn drop(&mut self) {
            unsafe { asdf_parser_destroy(self.0) };
        }
    }

    fn parse(bytes: &[u8], flags: AsdfParserOptFlags) -> Parser {
        let cfg = asdf_parser_cfg_t { flags, log: std::ptr::null_mut() };
        let parser = unsafe { asdf_parser_create(&cfg) };
        assert!(!parser.is_null());
        assert_eq!(
            unsafe { asdf_parser_set_input_mem(parser, bytes.as_ptr().cast(), bytes.len()) },
            0
        );
        Parser(parser)
    }

    /// Every event type in order, walked with `asdf_event_iterate`.
    fn event_types(parser: &Parser) -> Vec<AsdfEventType> {
        let mut out = Vec::new();
        loop {
            let event = unsafe { asdf_event_iterate(parser.0) };
            if event.is_null() {
                return out;
            }
            out.push(unsafe { asdf_event_type(event) });
        }
    }

    #[test]
    fn reports_the_expected_event_sequence() {
        let bytes = sample();
        let parser = parse(&bytes, 0);
        assert_eq!(
            event_types(&parser),
            vec![
                AsdfEventType::AsdfVersion,
                AsdfEventType::StandardVersion,
                AsdfEventType::Comment,
                AsdfEventType::TreeStart,
                AsdfEventType::TreeEnd,
                AsdfEventType::End,
            ]
        );
    }

    #[test]
    fn yaml_events_appear_only_when_asked_for() {
        let bytes = sample();
        let quiet = parse(&bytes, 0);
        assert!(!event_types(&quiet).contains(&AsdfEventType::Yaml));

        let loud = parse(&bytes, parser_opt::EMIT_YAML_EVENTS);
        let types = event_types(&loud);
        assert!(types.contains(&AsdfEventType::Yaml));
        // Stream start/end, document start/end, mapping start/end, and two
        // key/value pairs.
        assert_eq!(types.iter().filter(|t| **t == AsdfEventType::Yaml).count(), 10);
    }

    #[test]
    fn comment_text_drops_its_leading_hash() {
        let bytes = sample();
        let parser = parse(&bytes, 0);
        loop {
            let event = unsafe { asdf_event_iterate(parser.0) };
            assert!(!event.is_null(), "no comment event in the stream");
            if unsafe { asdf_event_type(event) } == AsdfEventType::Comment {
                let text = unsafe { asdf_event_comment(event) };
                assert_eq!(unsafe { CStr::from_ptr(text) }, c"a note");
                return;
            }
        }
    }

    #[test]
    fn tree_info_brackets_the_yaml() {
        let bytes = sample();
        let parser = parse(&bytes, 0);
        let mut start = None;
        let mut end = None;
        loop {
            let event = unsafe { asdf_event_iterate(parser.0) };
            if event.is_null() {
                break;
            }
            match unsafe { asdf_event_type(event) } {
                AsdfEventType::TreeStart => {
                    let info = unsafe { &*asdf_event_tree_info(event) };
                    start = Some(info.start);
                }
                AsdfEventType::TreeEnd => {
                    let info = unsafe { &*asdf_event_tree_info(event) };
                    end = Some(info.end);
                }
                _ => {}
            }
        }
        let (start, end) = (start.expect("tree start"), end.expect("tree end"));
        // The tree starts at the `%YAML` directive, just past the two `#`
        // header lines and the comment.
        assert_eq!(&bytes[start..start + 5], b"%YAML");
        assert!(end > start && end <= bytes.len());
    }

    #[test]
    fn buffered_tree_hands_back_its_text() {
        let bytes = sample();
        let parser = parse(&bytes, parser_opt::BUFFER_TREE);
        loop {
            let event = unsafe { asdf_event_iterate(parser.0) };
            assert!(!event.is_null());
            if unsafe { asdf_event_type(event) } == AsdfEventType::TreeEnd {
                let info = unsafe { &*asdf_event_tree_info(event) };
                assert!(!info.buf.is_null());
                let text = unsafe { CStr::from_ptr(info.buf) }.to_string_lossy().into_owned();
                assert!(text.starts_with("%YAML 1.1"), "unexpected tree text: {text}");
                assert!(text.contains("name: probe"));
                return;
            }
        }
    }

    #[test]
    fn accessors_reject_events_of_the_wrong_type() {
        let bytes = sample();
        let parser = parse(&bytes, 0);
        let event = unsafe { asdf_event_iterate(parser.0) };
        assert_eq!(unsafe { asdf_event_type(event) }, AsdfEventType::AsdfVersion);
        assert!(unsafe { asdf_event_comment(event) }.is_null());
        assert!(unsafe { asdf_event_tree_info(event) }.is_null());
        assert!(unsafe { asdf_event_block_info(event) }.is_null());
        assert_eq!(unsafe { asdf_yaml_event_type(event) }, AsdfYamlEventType::None);
    }

    #[test]
    fn parse_hands_out_events_the_caller_frees() {
        let bytes = sample();
        let parser = parse(&bytes, 0);
        // Unlike `iterate`, `parse` keeps every event alive until freed, so
        // two of them may be held at once.
        let first = unsafe { asdf_parser_parse(parser.0) };
        let second = unsafe { asdf_parser_parse(parser.0) };
        assert!(!first.is_null() && !second.is_null());
        assert_eq!(unsafe { asdf_event_type(first) }, AsdfEventType::AsdfVersion);
        assert_eq!(unsafe { asdf_event_type(second) }, AsdfEventType::StandardVersion);
        unsafe { asdf_event_free(parser.0, first) };
        unsafe { asdf_event_free(parser.0, second) };
        // Anything left outstanding is released with the parser.
        let _ = unsafe { asdf_parser_parse(parser.0) };
    }

    #[test]
    fn event_type_names_match_the_enum_spelling() {
        let name = asdf_event_type_name(AsdfEventType::BlockIndex as c_int);
        assert_eq!(unsafe { CStr::from_ptr(name) }, c"ASDF_BLOCK_INDEX_EVENT");
        // A value C could pass but the enum does not name is reported as
        // unknown rather than indexing past the end of the table.
        assert_eq!(unsafe { CStr::from_ptr(asdf_event_type_name(99)) }, c"ASDF_UNKNOWN_EVENT");
        assert_eq!(unsafe { CStr::from_ptr(asdf_event_type_name(-1)) }, c"ASDF_UNKNOWN_EVENT");
    }

    #[test]
    fn a_bad_buffer_is_reported_not_fatal() {
        let cfg = asdf_parser_cfg_t { flags: 0, log: std::ptr::null_mut() };
        let parser = Parser(unsafe { asdf_parser_create(&cfg) });
        let junk = b"not an asdf file at all\n";
        assert_eq!(
            unsafe { asdf_parser_set_input_mem(parser.0, junk.as_ptr().cast(), junk.len()) },
            -1
        );
        assert!(unsafe { asdf_parser_has_error(parser.0) });
        assert!(!unsafe { asdf_parser_get_error(parser.0) }.is_null());
        assert_ne!(unsafe { asdf_parser_error_code(parser.0) }, 0);
    }

    #[test]
    fn null_handles_are_tolerated() {
        assert!(unsafe { asdf_parser_parse(std::ptr::null_mut()) }.is_null());
        assert!(unsafe { asdf_event_iterate(std::ptr::null_mut()) }.is_null());
        assert!(!unsafe { asdf_parser_has_error(std::ptr::null()) });
        assert_eq!(unsafe { asdf_event_type(std::ptr::null_mut()) }, AsdfEventType::None);
        assert!(unsafe { asdf_event_comment(std::ptr::null()) }.is_null());
        unsafe { asdf_parser_destroy(std::ptr::null_mut()) };
        unsafe { asdf_event_free(std::ptr::null_mut(), std::ptr::null_mut()) };
        let mut len = 7usize;
        assert!(unsafe { asdf_yaml_event_tag(std::ptr::null(), &mut len) }.is_null());
        assert_eq!(len, 0, "the length must be cleared even when there is no tag");
    }

    #[test]
    fn missing_input_files_are_reported() {
        let cfg = asdf_parser_cfg_t { flags: 0, log: std::ptr::null_mut() };
        let parser = Parser(unsafe { asdf_parser_create(&cfg) });
        let path = CString::new("/nonexistent/definitely-not-here.asdf").unwrap();
        assert_eq!(unsafe { asdf_parser_set_input_file(parser.0, path.as_ptr()) }, -1);
        assert!(unsafe { asdf_parser_has_error(parser.0) });
        assert_ne!(unsafe { asdf_parser_error_errno(parser.0) }, 0);
    }
}
