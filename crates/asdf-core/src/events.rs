//! The low-level event stream: what a file contains, in the order it appears.
//!
//! This is the engine behind libasdf's event-based parser and behind the
//! `asdf events` command. Rather than building a tree, it reports what the
//! file holds: the version headers, any comments, the block index, the
//! tree's extent, optionally the YAML events inside it, then each block,
//! then the end.
//!
//! The order is upstream's, which is not quite the file's own: the block
//! index is reported *before* the tree, because it is found by reading back
//! from the end of the file and knowing it early is what lets the blocks be
//! located without a scan.

use asdf_yaml::YamlEvent;

use crate::error::Result;
use crate::layout::{BlockLocation, Layout, scan};

/// One event from the stream.
#[derive(Clone, Debug)]
pub enum Event {
    /// The `#ASDF` line's version.
    AsdfVersion(String),
    /// The `#ASDF_STANDARD` line's version.
    StandardVersion(String),
    /// A comment line, without its leading `#`.
    Comment(String),
    /// The offsets listed in the file's block index.
    ///
    /// Reported as the file states them, whether or not they check out;
    /// judging the index is [`Layout`]'s job, not the stream's.
    BlockIndex(Vec<u64>),
    /// The YAML tree begins here.
    TreeStart { start: usize },
    /// One YAML event from inside the tree.
    Yaml(YamlEvent),
    /// The YAML tree ends here.
    TreeEnd { start: usize, end: usize },
    /// A binary block.
    Block(BlockLocation),
    /// The end of the file.
    End,
}

/// What to include in the stream.
#[derive(Clone, Copy, Default, Debug)]
pub struct EventOptions {
    /// Report the YAML events inside the tree, not just its extent.
    pub yaml: bool,
}

/// The event stream for a file already in memory.
///
/// A YAML tree that will not parse yields no [`Event::Yaml`] events rather
/// than an error: the rest of the file is still worth reporting, and that is
/// what makes the command useful on a damaged file.
pub fn events(buf: &[u8], options: EventOptions) -> Result<Vec<Event>> {
    let layout = scan(buf)?;
    Ok(events_from(buf, &layout, options))
}

/// The event stream for a file whose layout has already been scanned.
pub fn events_from(buf: &[u8], layout: &Layout, options: EventOptions) -> Vec<Event> {
    let mut out = Vec::new();

    out.push(Event::AsdfVersion(layout.format_version.to_string()));
    if let Some(standard) = &layout.standard_version {
        out.push(Event::StandardVersion(standard.to_string()));
    }
    for comment in &layout.comments {
        out.push(Event::Comment(comment.clone()));
    }
    if layout.block_index_pos.is_some() && !layout.block_index_offsets.is_empty() {
        out.push(Event::BlockIndex(layout.block_index_offsets.clone()));
    }
    if let Some(tree) = layout.tree.clone() {
        out.push(Event::TreeStart { start: tree.start });
        if options.yaml
            && let Some(text) = layout.tree_str(buf)
            && let Ok(parsed) = asdf_yaml::scan_events(text)
        {
            out.extend(parsed.into_iter().map(Event::Yaml));
        }
        out.push(Event::TreeEnd { start: tree.start, end: tree.end });
    }
    for block in &layout.blocks {
        out.push(Event::Block(block.clone()));
    }
    out.push(Event::End);
    out
}

impl Event {
    /// The name libasdf reports for this event's type.
    pub fn type_name(&self) -> &'static str {
        match self {
            Event::AsdfVersion(_) => "ASDF_ASDF_VERSION_EVENT",
            Event::StandardVersion(_) => "ASDF_STANDARD_VERSION_EVENT",
            Event::Comment(_) => "ASDF_COMMENT_EVENT",
            Event::BlockIndex(_) => "ASDF_BLOCK_INDEX_EVENT",
            Event::TreeStart { .. } => "ASDF_TREE_START_EVENT",
            Event::Yaml(_) => "ASDF_YAML_EVENT",
            Event::TreeEnd { .. } => "ASDF_TREE_END_EVENT",
            Event::Block(_) => "ASDF_BLOCK_EVENT",
            Event::End => "ASDF_END_EVENT",
        }
    }
}

/// The name libfyaml gives a YAML event, which libasdf passes straight
/// through.
///
/// These are the YAML test suite's event notation rather than anything
/// spelled out in a header, so they are pinned here and checked against
/// upstream's committed `events` fixtures.
pub fn yaml_event_name(event: &YamlEvent) -> &'static str {
    use asdf_yaml::YamlEventKind as K;
    match event.kind {
        K::StreamStart => "+STR",
        K::StreamEnd => "-STR",
        K::DocumentStart => "+DOC",
        K::DocumentEnd => "-DOC",
        K::MappingStart => "+MAP",
        K::MappingEnd => "-MAP",
        K::SequenceStart => "+SEQ",
        K::SequenceEnd => "-SEQ",
        K::Scalar => "=VAL",
        K::Alias => "=ALI",
    }
}

/// Render an event the way `asdf_event_print` does.
///
/// The `verbose` body is upstream's format character for character, which is
/// what its committed `events` fixtures pin.
pub fn render_event(event: &Event, verbose: bool) -> String {
    let mut out = format!("Event: {}\n", event.type_name());
    if !verbose {
        return out;
    }

    match event {
        Event::AsdfVersion(v) => out.push_str(&format!("  ASDF Version: {v}\n")),
        Event::StandardVersion(v) => out.push_str(&format!("  Standard Version: {v}\n")),
        Event::Comment(text) => out.push_str(&format!("  Comment: {text}\n")),
        Event::BlockIndex(offsets) => {
            let listed: Vec<String> = offsets.iter().map(u64::to_string).collect();
            out.push_str(&format!("  Offsets: {}\n", listed.join(", ")));
        }
        Event::TreeStart { start } => {
            out.push_str(&format!("  Tree start position: {start} (0x{start:x})\n"));
        }
        Event::TreeEnd { end, .. } => {
            out.push_str(&format!("  Tree end position: {end} (0x{end:x})\n"));
        }
        Event::Yaml(yaml) => {
            out.push_str(&format!("  Type: {}\n", yaml_event_name(yaml)));
            if let Some(tag) = &yaml.tag {
                out.push_str(&format!("  Tag: {tag}\n"));
            }
            if let Some(value) = &yaml.value
                && !value.is_empty()
            {
                out.push_str(&format!("  Value: {value}\n"));
            }
        }
        Event::Block(block) => {
            let header = &block.header;
            out.push_str(&format!(
                "  Header position: {} (0x{:x})\n",
                block.header_pos, block.header_pos
            ));
            out.push_str(&format!(
                "  Data position: {} (0x{:x})\n",
                block.data_pos, block.data_pos
            ));
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
            // The field is four bytes, `\0`-padded; upstream prints `%.4s`,
            // which stops at the first NUL.
            if header.compression[0] != 0 {
                let name = header.compression.split(|b| *b == 0).next().unwrap_or(&[]);
                out.push_str(&format!("  Compression: {}\n", String::from_utf8_lossy(name)));
            }
            out.push_str("  Checksum: ");
            for byte in header.checksum {
                out.push_str(&format!("{byte:02x}"));
            }
            out.push('\n');
        }
        Event::End => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n#a note\n");
        buf.extend_from_slice(b"%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n");
        buf.extend_from_slice(b"n: 1\nlist: [a]\n");
        buf.extend_from_slice(b"...\n");
        buf
    }

    fn names(options: EventOptions) -> Vec<&'static str> {
        events(&sample(), options).unwrap().iter().map(Event::type_name).collect()
    }

    #[test]
    fn the_stream_follows_upstreams_order() {
        assert_eq!(
            names(EventOptions::default()),
            vec![
                "ASDF_ASDF_VERSION_EVENT",
                "ASDF_STANDARD_VERSION_EVENT",
                "ASDF_COMMENT_EVENT",
                "ASDF_TREE_START_EVENT",
                "ASDF_TREE_END_EVENT",
                "ASDF_END_EVENT",
            ]
        );
    }

    #[test]
    fn yaml_events_are_opt_in() {
        let quiet = names(EventOptions::default());
        assert!(!quiet.contains(&"ASDF_YAML_EVENT"));

        let loud = names(EventOptions { yaml: true });
        // +STR +DOC +MAP =VAL(n) =VAL(1) =VAL(list) +SEQ =VAL(a) -SEQ -MAP
        // -DOC -STR.
        assert_eq!(loud.iter().filter(|n| **n == "ASDF_YAML_EVENT").count(), 12);
    }

    #[test]
    fn comments_lose_their_leading_hash() {
        let stream = events(&sample(), EventOptions::default()).unwrap();
        let comment = stream
            .iter()
            .find_map(|e| match e {
                Event::Comment(text) => Some(text.clone()),
                _ => None,
            })
            .expect("a comment event");
        assert_eq!(comment, "a note");
    }

    #[test]
    fn rendering_is_the_header_line_alone_unless_verbose() {
        let stream = events(&sample(), EventOptions { yaml: true }).unwrap();
        for event in &stream {
            let terse = render_event(event, false);
            assert_eq!(terse, format!("Event: {}\n", event.type_name()));
            assert!(render_event(event, true).starts_with(&terse));
        }
    }

    #[test]
    fn a_damaged_tree_still_reports_the_rest_of_the_file() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
        buf.extend_from_slice(b"%YAML 1.1\n--- !core/asdf-1.1.0\n");
        // A mapping value that never closes.
        buf.extend_from_slice(b"a: [1, 2\n");
        buf.extend_from_slice(b"...\n");

        let stream = events(&buf, EventOptions { yaml: true }).unwrap();
        let names: Vec<&str> = stream.iter().map(Event::type_name).collect();
        assert!(names.contains(&"ASDF_TREE_START_EVENT"));
        assert!(names.contains(&"ASDF_TREE_END_EVENT"));
        assert_eq!(names.last(), Some(&"ASDF_END_EVENT"));
    }
}
