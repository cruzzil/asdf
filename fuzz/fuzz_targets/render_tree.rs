//! The renderers, which follow YAML aliases and so can be made to recurse
//! for ever or to expand exponentially.
//!
//! Split from `read_path` because it explores a different thing: that target
//! is driven by the block layer and the datatypes, this one by the tree's
//! anchor and alias graph. Two of the five findings in
//! `docs/SECURITY-REVIEW.md` were here, and both were aborts.

#![no_main]

use libfuzzer_sys::fuzz_target;

use asdf_core::Reader;
use asdf_core::events::{EventOptions, events, render_event};
use asdf_core::info::{InfoOptions, render};

fuzz_target!(|data: &[u8]| {
    // The event walk reads the raw bytes, so it runs whether or not the
    // layout scan liked what it saw.
    let options = EventOptions { yaml: true, buffer_tree: true };
    if let Ok(evs) = events(data, options) {
        for event in evs.iter().take(4096) {
            let _ = render_event(event, true);
        }
    }

    let Ok(reader) = Reader::from_bytes(data.to_vec()) else { return };
    let _ = render(&reader, InfoOptions::default());
    let _ = render(
        &reader,
        InfoOptions { print_tree: true, print_blocks: true, verify_checksums: true },
    );
});
