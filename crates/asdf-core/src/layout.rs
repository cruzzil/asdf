//! Scanning an ASDF file's overall structure.
//!
//! The layout is, in order: a `#ASDF` header line, optional comment lines, an
//! optional YAML tree, zero or more binary blocks, and an optional block
//! index. The tree's length is deliberately not recorded anywhere, so that it
//! can be edited by hand; readers find its end by searching for the document
//! end marker.

use std::ops::Range;

use crate::block::header::{BLOCK_MAGIC, BlockHeader, is_block_magic};
use crate::error::{Result, err};
use crate::version::Version;

/// The token every ASDF file starts with.
pub const ASDF_HEADER_PREFIX: &[u8] = b"#ASDF ";
/// The comment that records the ASDF Standard version.
pub const ASDF_STANDARD_PREFIX: &[u8] = b"#ASDF_STANDARD ";
/// The line that introduces the block index.
pub const BLOCK_INDEX_HEADER: &[u8] = b"#ASDF BLOCK INDEX";
/// The prefix of the YAML version directive.
pub const YAML_DIRECTIVE_PREFIX: &[u8] = b"%YAML ";
/// The YAML document end marker, including its leading newline.
pub const YAML_DOCUMENT_END_MARKER: &[u8] = b"\n...";

/// Where a block lives in the file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockLocation {
    /// Zero-based index of the block in the file.
    pub index: usize,
    /// Byte offset of the block magic.
    pub header_pos: u64,
    /// Byte offset of the first data byte, just past the header.
    pub data_pos: u64,
    /// The decoded header.
    pub header: BlockHeader,
}

impl BlockLocation {
    /// The offset one past the block's allocated space, where the next block
    /// or the block index begins.
    ///
    /// Saturating, because `allocated_size` comes straight from the file and
    /// a corrupt one can be `u64::MAX`. Wrapping would produce an offset
    /// *inside* the file and send the scanner somewhere plausible-looking;
    /// saturating pushes it past the end, where the bounds check catches it.
    pub fn end_pos(&self) -> u64 {
        self.data_pos.saturating_add(self.header.allocated_size)
    }
}

/// Why a block index was rejected.
///
/// The standard tells libraries to be conservative here: addressing the wrong
/// part of a file on the strength of a stale index is worse than rebuilding it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IndexRejection {
    /// The index YAML could not be parsed as a list of integers.
    Unparseable,
    /// The index listed a different number of blocks than the file contains.
    CountMismatch {
        /// Offsets listed in the index.
        listed: usize,
        /// Blocks actually found.
        found: usize,
    },
    /// The first offset did not point at the first block. This catches the
    /// common case of a tree edited by hand without updating the index.
    FirstOffsetMismatch {
        /// What the index claimed.
        listed: u64,
        /// Where the first block actually is.
        actual: u64,
    },
    /// An offset did not point at block magic.
    NotBlockMagic {
        /// The offending offset.
        offset: u64,
    },
    /// Offsets were not monotonically increasing, so the index could not be
    /// rebuilt by skipping along.
    NotMonotonic,
    /// The last block's allocated space was not immediately followed by the
    /// index.
    LastBlockNotAdjacent,
}

/// The result of scanning a file.
#[derive(Clone, Debug)]
pub struct Layout {
    /// The low-level format version from the `#ASDF` line.
    pub format_version: Version,
    /// The standard version from the `#ASDF_STANDARD` comment, if present.
    pub standard_version: Option<Version>,
    /// Any other comment lines between the header and the tree, without their
    /// leading `#` or trailing newline.
    pub comments: Vec<String>,
    /// Byte range of the YAML tree, if the file has one.
    pub tree: Option<Range<usize>>,
    /// The blocks, in file order.
    pub blocks: Vec<BlockLocation>,
    /// Byte offset of the block index header, if one was found.
    pub block_index_pos: Option<u64>,
    /// Why the block index was not used, if it was found but rejected.
    pub index_rejection: Option<IndexRejection>,
}

impl Layout {
    /// Whether the file carries a YAML tree.
    ///
    /// A file in exploded form may legitimately have none.
    pub fn has_tree(&self) -> bool {
        self.tree.is_some()
    }

    /// The tree text, given the buffer it was scanned from.
    pub fn tree_str<'a>(&self, buf: &'a [u8]) -> Option<&'a str> {
        let range = self.tree.clone()?;
        std::str::from_utf8(&buf[range]).ok()
    }

    /// Whether a block index was present and accepted.
    pub fn used_block_index(&self) -> bool {
        self.block_index_pos.is_some() && self.index_rejection.is_none()
    }
}

/// Find the end of a line starting at `pos`, returning the content without
/// its line terminator and the offset of the next line.
fn read_line(buf: &[u8], pos: usize) -> Option<(&[u8], usize)> {
    if pos >= buf.len() {
        return None;
    }
    match buf[pos..].iter().position(|b| *b == b'\n') {
        Some(rel) => {
            let nl = pos + rel;
            // Trim a DOS line ending.
            let end = if nl > pos && buf[nl - 1] == b'\r' { nl - 1 } else { nl };
            Some((&buf[pos..end], nl + 1))
        }
        // A final line with no terminator.
        None => Some((&buf[pos..], buf.len())),
    }
}

/// Scan the header, comments and tree extent, returning the offset where the
/// binary section begins.
fn scan_text_section(buf: &[u8], out: &mut Layout) -> Result<usize> {
    let Some((line, mut pos)) = read_line(buf, 0) else {
        return Err(err!(InvalidAsdfHeader, "file is empty"));
    };

    if !line.starts_with(ASDF_HEADER_PREFIX) {
        return Err(err!(
            InvalidAsdfHeader,
            "file does not begin with the {:?} token",
            String::from_utf8_lossy(ASDF_HEADER_PREFIX)
        ));
    }
    let version = std::str::from_utf8(&line[ASDF_HEADER_PREFIX.len()..])
        .map_err(|_| err!(InvalidAsdfHeader, "ASDF version is not valid UTF-8"))?;
    out.format_version = Version::parse(version.trim());

    // Comment lines, up to the tree or the binary section.
    while let Some((line, next)) = read_line(buf, pos) {
        if !line.starts_with(b"#") {
            break;
        }
        if let Some(rest) = line.strip_prefix(ASDF_STANDARD_PREFIX) {
            if let Ok(s) = std::str::from_utf8(rest) {
                out.standard_version = Some(Version::parse(s.trim()));
            }
        } else {
            out.comments.push(String::from_utf8_lossy(&line[1..]).into_owned());
        }
        pos = next;
    }

    // What follows is either the tree, a block, or nothing.
    if pos >= buf.len() {
        return Ok(pos);
    }
    if is_block_magic(&buf[pos..]) {
        return Ok(pos);
    }
    if !buf[pos..].starts_with(YAML_DIRECTIVE_PREFIX) {
        // Not a directive and not a block: treat the remainder as a tree
        // anyway if it looks like YAML content, otherwise as binary.
        if !buf[pos..].starts_with(b"---") {
            return Ok(pos);
        }
    }

    let tree_start = pos;
    let tree_end = find_document_end(buf, tree_start);
    out.tree = Some(tree_start..tree_end);
    Ok(tree_end)
}

/// Find the end of the YAML document beginning at `start`.
///
/// The standard's recommended search is for `\r?\n...\r?\n`. The returned
/// offset is just past the marker's own line terminator, so the tree range
/// includes the `...` line.
fn find_document_end(buf: &[u8], start: usize) -> usize {
    let mut search = start;
    while let Some(rel) = find_bytes(&buf[search..], YAML_DOCUMENT_END_MARKER) {
        let marker = search + rel;
        let after = marker + YAML_DOCUMENT_END_MARKER.len();
        // The marker must be alone on its line.
        match buf.get(after) {
            None => return buf.len(),
            Some(b'\n') => return after + 1,
            Some(b'\r') if buf.get(after + 1) == Some(&b'\n') => return after + 2,
            _ => search = after,
        }
    }
    // No end marker: the tree runs to the first block, or to EOF.
    match find_bytes(&buf[start..], BLOCK_MAGIC) {
        Some(rel) => start + rel,
        None => buf.len(),
    }
}

/// A plain substring search over bytes.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The same search, from the end.
fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// Walk the blocks from `pos`, following each header's allocated size.
///
/// This is the "skip along" traversal the standard describes, and is what
/// makes the block index optional.
fn scan_blocks(buf: &[u8], mut pos: usize, out: &mut Layout) -> Result<()> {
    // Padding may separate the tree from the first block.
    if !is_block_magic(buf.get(pos..).unwrap_or(&[])) {
        match find_bytes(&buf[pos.min(buf.len())..], BLOCK_MAGIC) {
            Some(rel) => pos += rel,
            None => return Ok(()),
        }
    }

    while pos < buf.len() {
        if !is_block_magic(&buf[pos..]) {
            break;
        }
        let (header, consumed) = BlockHeader::parse(&buf[pos..])?;
        let data_pos = pos + consumed;

        let location = BlockLocation {
            index: out.blocks.len(),
            header_pos: pos as u64,
            data_pos: data_pos as u64,
            header: header.clone(),
        };

        if header.is_streamed() {
            // A streamed block runs to the end of the file, and nothing may
            // follow it.
            out.blocks.push(location);
            return Ok(());
        }

        let end = location.end_pos();
        if end > buf.len() as u64 {
            return Err(err!(
                UnexpectedEof,
                "block {} claims {} bytes but the file ends at {}",
                location.index,
                header.allocated_size,
                buf.len()
            ));
        }
        out.blocks.push(location);
        pos = end as usize;
    }
    Ok(())
}

/// Parse the block index's YAML payload: a flow or block sequence of integers.
fn parse_index_offsets(text: &str) -> Option<Vec<u64>> {
    let mut offsets = Vec::new();
    let mut saw_any = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('%')
            || line == "---"
            || line == "..."
            || line.starts_with('#')
        {
            continue;
        }
        // Block style: "- 1234"
        if let Some(rest) = line.strip_prefix("- ") {
            offsets.push(rest.trim().parse::<u64>().ok()?);
            saw_any = true;
            continue;
        }
        // Flow style, possibly following a document marker: "--- [1, 2]"
        let body = line.strip_prefix("---").unwrap_or(line).trim();
        let body = body.strip_prefix('[')?.strip_suffix(']')?;
        for part in body.split(',') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            offsets.push(p.parse::<u64>().ok()?);
        }
        saw_any = true;
    }

    saw_any.then_some(offsets)
}

/// Look for a block index at the end of the file and check it against the
/// blocks actually found.
fn scan_block_index(buf: &[u8], out: &mut Layout) {
    // The standard says to read backwards from the end.
    let Some(pos) = rfind_bytes(buf, BLOCK_INDEX_HEADER) else {
        return;
    };
    out.block_index_pos = Some(pos as u64);

    let text = String::from_utf8_lossy(&buf[pos + BLOCK_INDEX_HEADER.len()..]);
    let Some(offsets) = parse_index_offsets(&text) else {
        out.index_rejection = Some(IndexRejection::Unparseable);
        return;
    };

    if offsets.windows(2).any(|w| w[1] <= w[0]) {
        out.index_rejection = Some(IndexRejection::NotMonotonic);
        return;
    }
    if offsets.len() != out.blocks.len() {
        out.index_rejection =
            Some(IndexRejection::CountMismatch { listed: offsets.len(), found: out.blocks.len() });
        return;
    }
    if let (Some(first_listed), Some(first_block)) = (offsets.first(), out.blocks.first())
        && *first_listed != first_block.header_pos
    {
        out.index_rejection = Some(IndexRejection::FirstOffsetMismatch {
            listed: *first_listed,
            actual: first_block.header_pos,
        });
        return;
    }
    for off in &offsets {
        let ok = usize::try_from(*off).ok().and_then(|o| buf.get(o..)).is_some_and(is_block_magic);
        if !ok {
            out.index_rejection = Some(IndexRejection::NotBlockMagic { offset: *off });
            return;
        }
    }
    if let Some(last) = out.blocks.last()
        && last.end_pos() != pos as u64
    {
        out.index_rejection = Some(IndexRejection::LastBlockNotAdjacent);
    }
}

/// Scan an in-memory ASDF file.
pub fn scan(buf: &[u8]) -> Result<Layout> {
    let mut out = Layout {
        format_version: Version::default(),
        standard_version: None,
        comments: Vec::new(),
        tree: None,
        blocks: Vec::new(),
        block_index_pos: None,
        index_rejection: None,
    };

    let after_text = scan_text_section(buf, &mut out)?;
    scan_blocks(buf, after_text, &mut out)?;
    scan_block_index(buf, &mut out);
    Ok(out)
}

/// Render a block index for a set of blocks, as written at the end of a file.
pub fn write_block_index(offsets: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(BLOCK_INDEX_HEADER);
    out.push(b'\n');
    out.extend_from_slice(b"%YAML 1.1\n---\n");
    for off in offsets {
        out.extend_from_slice(format!("- {off}\n").as_bytes());
    }
    out.extend_from_slice(b"...\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::header::BLOCK_HEADER_FULL_SIZE;
    use crate::block::header::FLAG_STREAMED;
    use crate::error::ErrorCode;

    /// Build a minimal but well-formed ASDF file.
    fn build(tree: Option<&str>, block_payloads: &[&[u8]], with_index: bool) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        if let Some(t) = tree {
            buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
            buf.extend_from_slice(t.as_bytes());
            buf.extend_from_slice(b"\n...\n");
        }
        let mut offsets = Vec::new();
        for payload in block_payloads {
            offsets.push(buf.len() as u64);
            let h = BlockHeader {
                allocated_size: payload.len() as u64,
                used_size: payload.len() as u64,
                data_size: payload.len() as u64,
                ..Default::default()
            };
            h.write(&mut buf);
            buf.extend_from_slice(payload);
        }
        if with_index && !offsets.is_empty() {
            buf.extend_from_slice(&write_block_index(&offsets));
        }
        buf
    }

    #[test]
    fn reads_header_and_standard_version() {
        let buf = build(Some("foo: 1"), &[], false);
        let l = scan(&buf).unwrap();
        assert_eq!(l.format_version.triple(), (1, 0, 0));
        assert_eq!(l.standard_version.unwrap().triple(), (1, 6, 0));
    }

    #[test]
    fn rejects_a_file_without_the_asdf_token() {
        let e = scan(b"not an asdf file\n").unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidAsdfHeader);
        assert_eq!(scan(b"").unwrap_err().code(), ErrorCode::InvalidAsdfHeader);
    }

    #[test]
    fn finds_the_tree_extent() {
        let buf = build(Some("foo: 1"), &[], false);
        let l = scan(&buf).unwrap();
        let tree = l.tree_str(&buf).unwrap();
        assert!(tree.starts_with("%YAML 1.1\n"));
        assert!(tree.trim_end().ends_with("..."));
        assert!(tree.contains("foo: 1"));
    }

    #[test]
    fn handles_dos_line_endings() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\r\n#ASDF_STANDARD 1.6.0\r\n");
        buf.extend_from_slice(b"%YAML 1.1\r\n--- !core/asdf-1.1.0\r\nfoo: 1\r\n...\r\n");
        let l = scan(&buf).unwrap();
        assert_eq!(l.format_version.triple(), (1, 0, 0));
        assert_eq!(l.standard_version.as_ref().unwrap().triple(), (1, 6, 0));
        assert!(l.has_tree());
        assert!(l.tree_str(&buf).unwrap().contains("foo: 1"));
    }

    #[test]
    fn collects_other_comments() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n# a note\n");
        buf.extend_from_slice(b"%YAML 1.1\n--- !core/asdf-1.1.0\nfoo: 1\n...\n");
        let l = scan(&buf).unwrap();
        assert_eq!(l.comments, [" a note"]);
    }

    #[test]
    fn a_file_may_have_no_tree() {
        // Exploded form: header then straight into blocks.
        let buf = build(None, &[b"abcd"], false);
        let l = scan(&buf).unwrap();
        assert!(!l.has_tree());
        assert_eq!(l.blocks.len(), 1);
    }

    #[test]
    fn walks_blocks_by_skipping_along() {
        let buf = build(Some("x: 1"), &[b"aaaa", b"bbbbbbbb", b"c"], false);
        let l = scan(&buf).unwrap();
        assert_eq!(l.blocks.len(), 3);
        assert_eq!(l.blocks[0].header.used_size, 4);
        assert_eq!(l.blocks[1].header.used_size, 8);
        assert_eq!(l.blocks[2].header.used_size, 1);

        // Each block's data must sit where the header says it does.
        for (b, expect) in l.blocks.iter().zip([&b"aaaa"[..], b"bbbbbbbb", b"c"]) {
            let start = b.data_pos as usize;
            let end = start + b.header.used_size as usize;
            assert_eq!(&buf[start..end], expect);
        }
    }

    #[test]
    fn tolerates_padding_between_tree_and_first_block() {
        let mut buf = build(Some("x: 1"), &[], false);
        buf.extend_from_slice(&[b' '; 64]); // the spec suggests spaces
        let block_at = buf.len() as u64;
        let h = BlockHeader { allocated_size: 4, used_size: 4, data_size: 4, ..Default::default() };
        h.write(&mut buf);
        buf.extend_from_slice(b"data");

        let l = scan(&buf).unwrap();
        assert_eq!(l.blocks.len(), 1);
        assert_eq!(l.blocks[0].header_pos, block_at);
    }

    #[test]
    fn accepts_a_correct_block_index() {
        let buf = build(Some("x: 1"), &[b"aaaa", b"bbbb"], true);
        let l = scan(&buf).unwrap();
        assert_eq!(l.blocks.len(), 2);
        assert!(l.used_block_index(), "index should be accepted: {:?}", l.index_rejection);
    }

    #[test]
    fn rejects_an_index_whose_first_offset_is_stale() {
        // The case the standard singles out: the tree was edited by hand and
        // every offset shifted.
        let mut buf = build(Some("x: 1"), &[b"aaaa"], true);
        let idx = rfind_bytes(&buf, BLOCK_INDEX_HEADER).unwrap();
        let tail = write_block_index(&[9999]);
        buf.truncate(idx);
        buf.extend_from_slice(&tail);

        let l = scan(&buf).unwrap();
        assert!(!l.used_block_index());
        assert!(matches!(l.index_rejection, Some(IndexRejection::FirstOffsetMismatch { .. })));
        // ...but the blocks are still found by skipping along.
        assert_eq!(l.blocks.len(), 1);
    }

    #[test]
    fn rejects_a_non_monotonic_index() {
        let mut buf = build(Some("x: 1"), &[b"aaaa", b"bbbb"], false);
        buf.extend_from_slice(&write_block_index(&[500, 100]));
        let l = scan(&buf).unwrap();
        assert_eq!(l.index_rejection, Some(IndexRejection::NotMonotonic));
    }

    #[test]
    fn rejects_an_index_with_the_wrong_count() {
        let mut buf = build(Some("x: 1"), &[b"aaaa", b"bbbb"], false);
        let first = buf.windows(4).position(|w| w == BLOCK_MAGIC).unwrap() as u64;
        buf.extend_from_slice(&write_block_index(&[first]));
        let l = scan(&buf).unwrap();
        assert!(matches!(
            l.index_rejection,
            Some(IndexRejection::CountMismatch { listed: 1, found: 2 })
        ));
    }

    #[test]
    fn parses_both_index_styles() {
        // Block style, as libasdf writes it.
        assert_eq!(
            parse_index_offsets("%YAML 1.1\n---\n- 901\n- 1024\n...\n"),
            Some(vec![901, 1024])
        );
        // Flow style, as the standard's example shows.
        assert_eq!(
            parse_index_offsets("%YAML 1.1\n--- [2043, 16340]\n...\n"),
            Some(vec![2043, 16340])
        );
    }

    #[test]
    fn streamed_block_ends_the_scan() {
        let mut buf = build(Some("x: 1"), &[], false);
        let h = BlockHeader { flags: FLAG_STREAMED, ..Default::default() };
        h.write(&mut buf);
        buf.extend_from_slice(b"streaming payload, length unknown up front");

        let l = scan(&buf).unwrap();
        assert_eq!(l.blocks.len(), 1);
        assert!(l.blocks[0].header.is_streamed());
    }

    #[test]
    fn block_running_past_eof_is_an_error() {
        let mut buf = build(Some("x: 1"), &[], false);
        let h = BlockHeader {
            allocated_size: 1_000_000,
            used_size: 1_000_000,
            data_size: 1_000_000,
            ..Default::default()
        };
        h.write(&mut buf);
        buf.extend_from_slice(b"short");
        assert_eq!(scan(&buf).unwrap_err().code(), ErrorCode::UnexpectedEof);
    }

    #[test]
    fn index_round_trips() {
        let rendered = write_block_index(&[901, 2048]);
        let text = String::from_utf8(rendered.clone()).unwrap();
        assert!(text.starts_with("#ASDF BLOCK INDEX\n"));
        let body = &text[BLOCK_INDEX_HEADER.len()..];
        assert_eq!(parse_index_offsets(body), Some(vec![901, 2048]));
        assert_eq!(BLOCK_HEADER_FULL_SIZE, 54);
    }
}
