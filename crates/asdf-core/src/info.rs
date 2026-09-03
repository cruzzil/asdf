//! Rendering a human-readable view of an ASDF file.
//!
//! This reproduces the output of libasdf's `asdf info` byte for byte,
//! including its ANSI styling and box drawing, so the two tools can be
//! compared directly and upstream's committed expected-output fixtures serve
//! as tests.

use std::fmt::Write as _;

use asdf_yaml::{Document, NodeData, NodeId};

use crate::reader::{ChecksumStatus, Reader};

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_DIM: &str = "\x1b[2m";
const COLOR_GREEN: &str = "\x1b[32m";
const COLOR_RED: &str = "\x1b[31m";

/// Display columns of a scalar shown in the tree preview.
///
/// Scalars may be arbitrarily long and may contain newlines; printed verbatim
/// they would wrap the terminal and break the tree drawing.
const SCALAR_PREVIEW_MAX: usize = 64;

/// Total width of the block table, including both borders.
const BOX_WIDTH: usize = 50;

/// What to include in the rendering.
#[derive(Clone, Copy, Debug)]
pub struct InfoOptions {
    /// Render the YAML tree.
    pub print_tree: bool,
    /// Render a table for each binary block.
    pub print_blocks: bool,
    /// Verify each block's checksum and mark it in the table.
    pub verify_checksums: bool,
}

impl Default for InfoOptions {
    fn default() -> Self {
        Self { print_tree: true, print_blocks: false, verify_checksums: false }
    }
}

/// Which border row to draw.
#[derive(Clone, Copy)]
enum Border {
    Top,
    Middle,
    Bottom,
}

/// How to place a field's text in the box.
#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
}

/// The display width of a string, ignoring ANSI escapes and counting a UTF-8
/// character as one column.
fn visible_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut len = 0;
    let mut idx = 0;
    while idx < bytes.len() {
        // Skip an escape sequence up to its terminating 'm'.
        if bytes[idx] == 0x1b && idx + 1 < bytes.len() && bytes[idx + 1] == b'[' {
            idx += 2;
            while idx < bytes.len() && bytes[idx] != b'm' {
                idx += 1;
            }
            if idx < bytes.len() {
                idx += 1;
            }
            continue;
        }
        // Count leading bytes only, so a multi-byte character is one column.
        if bytes[idx] & 0xc0 != 0x80 {
            len += 1;
        }
        idx += 1;
    }
    len
}

fn write_border(out: &mut String, border: Border) {
    out.push_str(ANSI_DIM);
    let (left, right) = match border {
        Border::Top => ("┌", "┐"),
        Border::Middle => ("├", "┤"),
        Border::Bottom => ("└", "┘"),
    };
    out.push_str(left);
    for _ in 1..BOX_WIDTH - 1 {
        out.push('─');
    }
    out.push_str(right);
    out.push('\n');
    out.push_str(ANSI_RESET);
}

fn write_field(out: &mut String, align: Align, text: &str) {
    let len = visible_len(text);
    let _ = write!(out, "{ANSI_DIM}│{ANSI_RESET}");
    match align {
        Align::Left => {
            // One leading space, then pad to the inner width.
            let pad = BOX_WIDTH.saturating_sub(len + 3);
            let _ = write!(out, " {text}{:pad$}", "", pad = pad);
        }
        Align::Center => {
            let left = (BOX_WIDTH.saturating_sub(len)) / 2 - 1;
            let right = BOX_WIDTH.saturating_sub(len + left + 2);
            let _ = write!(out, "{:left$}{text}{:right$}", "", "", left = left, right = right);
        }
    }
    let _ = writeln!(out, "{ANSI_DIM}│{ANSI_RESET}");
}

/// A single-line, column-limited preview of a scalar.
///
/// Runs of control characters collapse to one space, and the text is cut at
/// [`SCALAR_PREVIEW_MAX`] columns with an ellipsis.
fn scalar_preview(value: &str) -> String {
    let mut out = String::from(": ");
    let mut cols = 0usize;
    let mut pending_space = false;
    let mut any = false;

    for ch in value.chars() {
        if (ch as u32) < 0x20 || ch as u32 == 0x7f {
            // A run before any real output is dropped entirely.
            if any {
                pending_space = true;
            }
            continue;
        }
        if cols >= SCALAR_PREVIEW_MAX {
            out.push_str("...");
            return out;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
            cols += 1;
            if cols >= SCALAR_PREVIEW_MAX {
                // A real character still follows, so content is being dropped.
                out.push_str("...");
                return out;
            }
        }
        out.push(ch);
        cols += 1;
        any = true;
    }
    out
}

/// The label shown in parentheses after a node's name.
fn node_label(doc: &Document, id: NodeId) -> String {
    if let Some(tag) = doc.tag_of(id) {
        return tag.full();
    }
    match &doc.resolved(id).data {
        NodeData::Mapping { .. } => "mapping".into(),
        NodeData::Sequence { .. } => "sequence".into(),
        _ => "scalar".into(),
    }
}

/// State carried down the tree walk.
struct TreeState {
    /// Whether each ancestor level still has siblings to come, deciding
    /// between a continuing `│ ` and a blank `  `.
    active: Vec<bool>,
}

fn write_indent(out: &mut String, state: &TreeState, depth: usize, is_leaf: bool) {
    if depth < 1 {
        return;
    }
    out.push_str(ANSI_DIM);
    for idx in 0..depth {
        if idx == depth - 1 {
            out.push_str(if is_leaf { "└─" } else { "├─" });
        } else if state.active.get(idx).copied().unwrap_or(false) {
            out.push_str("│ ");
        } else {
            out.push_str("  ");
        }
    }
    out.push_str(ANSI_RESET);
}

/// How a node is identified by its parent.
enum NodeIndex<'a> {
    Key(&'a str),
    Index(usize),
}

fn write_node(
    out: &mut String,
    doc: &Document,
    id: NodeId,
    index: &NodeIndex<'_>,
    depth: usize,
    is_leaf: bool,
    state: &mut TreeState,
) {
    let label = node_label(doc, id);
    write_indent(out, state, depth, is_leaf);

    match index {
        NodeIndex::Key(key) => {
            let _ = write!(out, "{ANSI_BOLD}{key}{ANSI_RESET} ({label})");
        }
        NodeIndex::Index(idx) => {
            let _ = write!(
                out,
                "{ANSI_DIM}[{ANSI_RESET}{ANSI_BOLD}{idx}{ANSI_RESET}{ANSI_DIM}]{ANSI_RESET} ({label})"
            );
        }
    }

    let resolved = doc.resolve(id);
    let node = doc.node(resolved);

    // A scalar, or an alias to one, ends the line with its value.
    if !node.is_mapping() && !node.is_sequence() {
        out.push_str(&scalar_preview(node.as_str().unwrap_or("")));
        out.push('\n');
        return;
    }
    out.push('\n');

    if state.active.len() <= depth {
        state.active.resize(depth + 1, false);
    }
    state.active[depth] = true;

    match &node.data {
        NodeData::Mapping { entries, .. } => {
            let entries = entries.clone();
            let last = entries.len().saturating_sub(1);
            for (position, entry) in entries.iter().enumerate() {
                let leaf = position == last;
                if leaf {
                    state.active[depth] = false;
                }
                let key = doc.resolved(entry.key).as_str().unwrap_or("<complex key>").to_string();
                write_node(out, doc, entry.value, &NodeIndex::Key(&key), depth + 1, leaf, state);
            }
        }
        NodeData::Sequence { items, .. } => {
            let items = items.clone();
            let last = items.len().saturating_sub(1);
            for (position, item) in items.iter().enumerate() {
                let leaf = position == last;
                if leaf {
                    state.active[depth] = false;
                }
                write_node(out, doc, *item, &NodeIndex::Index(position), depth + 1, leaf, state);
            }
        }
        _ => {}
    }
}

/// Render one block's table.
fn write_block(out: &mut String, reader: &Reader, index: usize, verify: bool) {
    let Ok(block) = reader.block(index) else { return };
    let header = &block.header;

    write_border(out, Border::Top);
    write_field(out, Align::Center, &format!("Block #{index}"));
    write_border(out, Border::Middle);
    write_field(out, Align::Left, &format!("flags: 0x{:08x}", header.flags));
    write_border(out, Border::Middle);

    // Upstream prints this with `%.*s` over the four-byte field, and printf
    // stops at the first NUL -- so an uncompressed block shows `""` rather
    // than four padding bytes.
    write_field(out, Align::Left, &format!("compression: \"{}\"", header.compression_name()));
    write_border(out, Border::Middle);

    write_field(out, Align::Left, &format!("allocated_size: {}", header.allocated_size));
    write_border(out, Border::Middle);
    write_field(out, Align::Left, &format!("used_size: {}", header.used_size));
    write_border(out, Border::Middle);
    write_field(out, Align::Left, &format!("data_size: {}", header.data_size));
    write_border(out, Border::Middle);

    let checksum: String = header.checksum.iter().map(|b| format!("{b:02x}")).collect();
    let mark = if verify {
        match reader.verify_block_checksum(index) {
            Ok((ChecksumStatus::Valid, _)) => format!(" {COLOR_GREEN}✓{ANSI_RESET}"),
            Ok((ChecksumStatus::Absent, _)) => String::new(),
            _ => format!(" {COLOR_RED}✗{ANSI_RESET}"),
        }
    } else {
        String::new()
    };
    write_field(out, Align::Left, &format!("checksum: {checksum}{mark}"));
    write_border(out, Border::Bottom);
}

/// Render a file's information as a string.
pub fn render(reader: &Reader, options: InfoOptions) -> crate::Result<String> {
    let mut out = String::new();

    if options.print_tree
        && let Some(doc) = reader.tree()?
        && let Some(root) = doc.root()
    {
        let mut state = TreeState { active: vec![false; 16] };
        write_node(&mut out, &doc, root, &NodeIndex::Key("root"), 0, true, &mut state);
    }

    if options.print_blocks {
        for index in 0..reader.block_count() {
            write_block(&mut out, reader, index, options.verify_checksums);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip ANSI escapes, for assertions about structure rather than styling.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let bytes = s.as_bytes();
        let mut idx = 0;
        while idx < bytes.len() {
            if bytes[idx] == 0x1b && idx + 1 < bytes.len() && bytes[idx + 1] == b'[' {
                idx += 2;
                while idx < bytes.len() && bytes[idx] != b'm' {
                    idx += 1;
                }
                idx += 1;
                continue;
            }
            let start = idx;
            idx += 1;
            while idx < bytes.len() && bytes[idx] & 0xc0 == 0x80 {
                idx += 1;
            }
            out.push_str(std::str::from_utf8(&bytes[start..idx]).unwrap_or("?"));
        }
        out
    }

    fn build_file(tree: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(tree.as_bytes());
        buf.extend_from_slice(b"...\n");
        buf
    }

    #[test]
    fn visible_len_ignores_escapes_and_counts_characters() {
        assert_eq!(visible_len("abc"), 3);
        assert_eq!(visible_len("\x1b[1mabc\x1b[0m"), 3);
        // A multi-byte character is one column.
        assert_eq!(visible_len("✓"), 1);
        assert_eq!(visible_len("\x1b[32m✓\x1b[0m"), 1);
    }

    #[test]
    fn box_rows_are_all_the_same_visible_width() {
        let mut out = String::new();
        write_border(&mut out, Border::Top);
        write_field(&mut out, Align::Center, "Block #0");
        write_border(&mut out, Border::Middle);
        write_field(&mut out, Align::Left, "flags: 0x00000000");
        write_border(&mut out, Border::Bottom);

        // Upstream emits the newline *before* the trailing reset, so each
        // reset lands at the start of the following line. That is part of the
        // byte-exact output, so the check tolerates it rather than "fixing" it.
        for line in out.lines() {
            let line = line.strip_prefix(ANSI_RESET).unwrap_or(line);
            // The final reset trails the last newline, leaving an empty tail.
            if line.is_empty() {
                continue;
            }
            assert_eq!(
                visible_len(line),
                BOX_WIDTH,
                "row {line:?} is not {BOX_WIDTH} columns wide"
            );
        }
    }

    #[test]
    fn scalar_previews_collapse_control_characters() {
        assert_eq!(scalar_preview("hello"), ": hello");
        assert_eq!(scalar_preview("a\nb"), ": a b");
        assert_eq!(scalar_preview("a\n\n\tb"), ": a b");
        // A leading run is dropped rather than turned into a space.
        assert_eq!(scalar_preview("\n\nabc"), ": abc");
    }

    #[test]
    fn long_scalars_are_truncated() {
        let long = "x".repeat(100);
        let preview = scalar_preview(&long);
        assert!(preview.ends_with("..."));
        assert_eq!(preview.len(), 2 + SCALAR_PREVIEW_MAX + 3);
    }

    #[test]
    fn renders_a_simple_tree() {
        let file = build_file("a: 1\nb:\n  c: two\n");
        let reader = Reader::from_bytes(file).unwrap();
        let out = render(&reader, InfoOptions::default()).unwrap();
        let text = plain(&out);

        assert!(text.starts_with("root (tag:stsci.edu:asdf/core/asdf-1.1.0)\n"), "{text}");
        assert!(text.contains("├─a (scalar): 1\n"), "{text}");
        assert!(text.contains("└─b (mapping)\n"), "{text}");
        // The last child of the last child uses the corner and a blank
        // continuation from its parent.
        assert!(text.contains("  └─c (scalar): two\n"), "{text}");
    }

    #[test]
    fn continuation_bars_track_remaining_siblings() {
        let file = build_file("a:\n  x: 1\n  y: 2\nb: 3\n");
        let reader = Reader::from_bytes(file).unwrap();
        let text = plain(&render(&reader, InfoOptions::default()).unwrap());

        // `a` still has sibling `b` to come, so its children carry `│ `.
        assert!(text.contains("│ ├─x (scalar): 1\n"), "{text}");
        assert!(text.contains("│ └─y (scalar): 2\n"), "{text}");
        assert!(text.contains("└─b (scalar): 3\n"), "{text}");
    }

    #[test]
    fn sequences_are_indexed() {
        let file = build_file("s: [10, 20]\n");
        let reader = Reader::from_bytes(file).unwrap();
        let text = plain(&render(&reader, InfoOptions::default()).unwrap());
        assert!(text.contains("├─[0] (scalar): 10\n"), "{text}");
        assert!(text.contains("└─[1] (scalar): 20\n"), "{text}");
    }

    #[test]
    fn tagged_nodes_show_their_tag() {
        let file = build_file("d: !core/ndarray-1.1.0\n  source: 0\n");
        let reader = Reader::from_bytes(file).unwrap();
        let text = plain(&render(&reader, InfoOptions::default()).unwrap());
        assert!(text.contains("d (tag:stsci.edu:asdf/core/ndarray-1.1.0)"), "{text}");
    }

    #[test]
    fn the_tree_can_be_suppressed() {
        let file = build_file("a: 1\n");
        let reader = Reader::from_bytes(file).unwrap();
        let options = InfoOptions { print_tree: false, ..Default::default() };
        assert!(render(&reader, options).unwrap().is_empty());
    }
}
