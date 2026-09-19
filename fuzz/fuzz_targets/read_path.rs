//! Everything a caller can reach from a file's bytes.
//!
//! The shape of this target is taken from the lesson in
//! `docs/SECURITY-REVIEW.md`: the suite that was supposed to catch these
//! stopped at the block layer, so it never decoded an array. Three of the
//! five findings lived past that line. This walks all the way to the
//! elements.
//!
//! What counts as a failure here is wider than a panic. libFuzzer's
//! `-rss_limit_mb` turns a runaway allocation into a reported crash, and an
//! overflowed stack is a crash by itself -- which matters, because both of
//! those are aborts that no `catch_unwind` would have seen.

#![no_main]

use libfuzzer_sys::fuzz_target;

use asdf_core::Reader;
use asdf_core::core::elements::{decode_all, decode_inline};
use asdf_core::core::ndarray::{Ndarray, Source};
use asdf_core::layout::scan;

fuzz_target!(|data: &[u8]| {
    let _ = scan(data);

    let Ok(reader) = Reader::from_bytes(data.to_vec()) else { return };

    for index in 0..reader.block_count() {
        let _ = reader.block(index);
        let _ = reader.block_raw(index);
        let _ = reader.block_compression(index);
        let _ = reader.block_data(index);
        let _ = reader.verify_block_checksum(index);
    }
    let _ = reader.has_python_checksum_bug();
    let _ = reader.tree_inlined();

    let Ok(Some(doc)) = reader.tree() else { return };
    let Some(root) = doc.root() else { return };

    // A bounded walk: the tree may alias back on itself, and exploring a
    // cycle for ever would look like a hang rather than a finding.
    let mut stack = vec![root];
    let mut visited = 0usize;

    while let Some(id) = stack.pop() {
        visited += 1;
        if visited > 4096 {
            return;
        }

        if let Ok(nd) = Ndarray::parse(&doc, id) {
            let block_bytes = match nd.source {
                Source::Block(i) => reader.block_data(i).ok().map(|d| d.len() as u64),
                _ => None,
            };
            let _ = nd.len(block_bytes);
            let _ = nd.nbytes(block_bytes);

            if let Ok(shape) = nd.resolved_shape(block_bytes) {
                match nd.source {
                    Source::Block(i) => {
                        if let Ok(bytes) = reader.block_data(i) {
                            let _ = decode_all(&nd, &shape, &bytes);
                        }
                    }
                    Source::Inline(_) => {
                        let _ = decode_inline(&doc, &nd, &shape);
                    }
                    // Resolving one would read a neighbouring file, which is
                    // not this target's business.
                    Source::External(_) | Source::LastBlock => {}
                }
            }
        }

        match &doc.node(doc.resolve(id)).data {
            asdf_core::yaml::NodeData::Mapping { entries, .. } => {
                stack.extend(entries.iter().map(|e| e.value));
            }
            asdf_core::yaml::NodeData::Sequence { items, .. } => {
                stack.extend(items.iter().copied());
            }
            _ => {}
        }
    }
});
