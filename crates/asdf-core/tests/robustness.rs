//! Tier 7: the reader must never panic, whatever it is fed.
//!
//! This matters more here than in an ordinary Rust library. A panic that
//! reaches the C boundary is undefined behaviour, and while every FFI entry
//! point catches unwinding as a backstop, the engine reaching that backstop
//! is a bug. The parser is also the attack surface: the specification is
//! explicit that a reader must tolerate a hand-edited tree and detect an
//! invalid block index rather than trusting it.
//!
//! These are deterministic mutation tests rather than a fuzzer, so they run
//! in CI on every change. A real `cargo-fuzz` target belongs alongside them,
//! seeded from the same corpus.
//!
//! # Not panicking is not the whole job
//!
//! A panic unwinds and the C boundary catches it. An *abort* does not, and
//! nothing can catch it: a failed allocation and an overflowed stack both
//! take the caller's process down with them, panic guard or no. Several of
//! the cases below are here because they abort rather than panic, and the
//! only reason they went unnoticed is that `exercise` used to stop at the
//! block layer -- it never asked for an array's elements, and never rendered
//! a tree. Anything a caller can reach belongs in it.
//!
//! # Run this in release too
//!
//! A debug build catches an arithmetic overflow with a panic, which this
//! suite treats as a failure and reports precisely. A release build wraps
//! silently, and the wrapped value is what walks past a size check into an
//! allocation. Both builds are therefore worth running; they fail in
//! different places.

use std::path::PathBuf;

use asdf_core::Reader;
use asdf_core::layout::scan;

fn corpus_files() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join("code/asdf-standard/reference_files/1.6.0"));
        roots.push(PathBuf::from(&home).join("code/libasdf/tests/fixtures"));
    }
    if let Some(dir) = std::env::var_os("ASDF_STANDARD_DIR") {
        roots.push(PathBuf::from(dir).join("reference_files/1.6.0"));
    }
    if let Some(dir) = std::env::var_os("LIBASDF_DIR") {
        roots.push(PathBuf::from(dir).join("tests/fixtures"));
    }

    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "asdf") {
                out.push(path);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Exercise everything a caller might reach for, so a panic anywhere in the
/// read path surfaces.
fn exercise(bytes: &[u8]) {
    let Ok(layout) = scan(bytes) else { return };

    // The tree, if the extent search found one.
    if let Some(text) = layout.tree_str(bytes) {
        let _ = asdf_yaml::parse_document(text);
    }

    let Ok(reader) = Reader::from_bytes(bytes.to_vec()) else { return };
    for index in 0..reader.block_count() {
        let _ = reader.block(index);
        let _ = reader.block_raw(index);
        let _ = reader.block_compression(index);
        let _ = reader.block_data(index);
        let _ = reader.verify_block_checksum(index);
    }
    let _ = reader.tree();
    let _ = reader.tree_inlined();
    let _ = reader.has_python_checksum_bug();

    // Rendering the tree, which follows aliases and so can be made to
    // recurse for ever or to expand exponentially.
    let _ = asdf_core::info::render(&reader, asdf_core::info::InfoOptions::default());

    // And the array path, which is where a shape out of the tree turns into
    // an allocation. Everything above stops at the block's bytes.
    let Ok(Some(doc)) = reader.tree() else { return };
    exercise_arrays(&reader, &doc);
}

/// Decode every `core/ndarray` the tree declares, by every route a caller has.
fn exercise_arrays(reader: &Reader, doc: &asdf_core::yaml::Document) {
    use asdf_core::core::elements::{decode_all, decode_inline};
    use asdf_core::core::ndarray::{Ndarray, Source};

    let Some(root) = doc.root() else { return };
    let mut stack = vec![root];
    let mut seen = 0usize;

    while let Some(id) = stack.pop() {
        // A tree may alias back on itself; a bounded walk is enough here.
        seen += 1;
        if seen > 10_000 {
            return;
        }

        if let Ok(nd) = Ndarray::parse(doc, id) {
            let block_bytes = match nd.source {
                Source::Block(index) => reader.block_data(index).ok().map(|d| d.len() as u64),
                _ => None,
            };

            if let Ok(shape) = nd.resolved_shape(block_bytes) {
                let _ = nd.len(block_bytes);
                let _ = nd.nbytes(block_bytes);
                match nd.source {
                    Source::Block(index) => {
                        if let Ok(data) = reader.block_data(index) {
                            let _ = decode_all(&nd, &shape, &data);
                        }
                    }
                    Source::Inline(_) => {
                        let _ = decode_inline(doc, &nd, &shape);
                    }
                    Source::External(_) | Source::LastBlock => {}
                }
            }
        }

        match &doc.node(doc.resolve(id)).data {
            asdf_core::yaml::NodeData::Mapping { entries, .. } => {
                stack.extend(entries.iter().map(|e| e.value));
            }
            asdf_core::yaml::NodeData::Sequence { items, .. } => {
                stack.extend(items.iter().copied())
            }
            _ => {}
        }
    }
}

#[test]
fn truncation_at_every_length_never_panics() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("skipping: no corpus found");
        return;
    }

    let mut checked = 0;
    for path in files.iter().take(12) {
        let Ok(bytes) = std::fs::read(path) else { continue };
        // Every prefix up to a bound, then a sparser sweep of the rest, so
        // the test stays quick while still reaching the block section.
        for len in 0..bytes.len().min(1500) {
            exercise(&bytes[..len]);
            checked += 1;
        }
        let mut len = 1500;
        while len < bytes.len() {
            exercise(&bytes[..len]);
            checked += 1;
            len += 97; // a prime stride, so offsets do not align with structure
        }
    }
    eprintln!("exercised {checked} truncations");
    assert!(checked > 1000);
}

#[test]
fn single_byte_corruption_never_panics() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("skipping: no corpus found");
        return;
    }

    let mut checked = 0;
    for path in files.iter().take(8) {
        let Ok(original) = std::fs::read(path) else { continue };
        if original.is_empty() {
            continue;
        }

        // Flip one byte at a time across the file, and try several values at
        // each position rather than only the complement.
        let mut offset = 0usize;
        while offset < original.len() {
            for value in [0x00u8, 0xff, 0x0a, 0xd3] {
                let mut mangled = original.clone();
                mangled[offset] = value;
                exercise(&mangled);
                checked += 1;
            }
            // A stride that is coprime with the block header size, so the
            // sweep does not repeatedly hit the same field.
            offset += 31;
        }
    }
    eprintln!("exercised {checked} single-byte corruptions");
    assert!(checked > 500);
}

/// The block header's size fields are the most dangerous input: a corrupt
/// one asks the reader to address memory that is not there.
#[test]
fn corrupt_block_sizes_never_panic() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("skipping: no corpus found");
        return;
    }

    let magic = b"\xd3BLK";
    let mut checked = 0;

    for path in files.iter().take(8) {
        let Ok(original) = std::fs::read(path) else { continue };

        // Find each block header and scribble on its size fields.
        let mut position = 0usize;
        while position + 4 <= original.len() {
            if &original[position..position + 4] != magic {
                position += 1;
                continue;
            }
            // Offsets within the header, measured from the magic:
            // +6 flags, +10 compression, +14 allocated, +22 used, +30 data.
            for field_offset in [4usize, 6, 10, 14, 22, 30] {
                for pattern in
                    [[0xffu8; 8], [0x00; 8], [0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]]
                {
                    let mut mangled = original.clone();
                    let start = position + field_offset;
                    let end = (start + 8).min(mangled.len());
                    if start >= mangled.len() {
                        continue;
                    }
                    mangled[start..end].copy_from_slice(&pattern[..end - start]);
                    exercise(&mangled);
                    checked += 1;
                }
            }
            position += 4;
        }
    }
    eprintln!("exercised {checked} corrupt block headers");
    assert!(checked > 0, "no block headers were found to corrupt");
}

/// A block index is read from the end of the file and points elsewhere in
/// it, so a corrupt one is the clearest way to make a reader address the
/// wrong bytes. The specification calls for conservatism here.
#[test]
fn corrupt_block_indices_never_panic() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("skipping: no corpus found");
        return;
    }
    let header = b"#ASDF BLOCK INDEX";
    let mut checked = 0;

    for path in files.iter().take(8) {
        let Ok(original) = std::fs::read(path) else { continue };
        let Some(position) = original.windows(header.len()).rposition(|w| w == header) else {
            continue;
        };

        // Replace the index body with a series of hostile ones.
        for body in [
            "\n%YAML 1.1\n---\n- 999999999999\n...\n",
            "\n%YAML 1.1\n---\n- -1\n...\n",
            "\n%YAML 1.1\n---\n- 0\n- 0\n- 0\n...\n",
            "\n%YAML 1.1\n---\n[not, integers]\n...\n",
            "\n%YAML 1.1\n---\n",
            "\n",
            "",
        ] {
            let mut mangled = original[..position + header.len()].to_vec();
            mangled.extend_from_slice(body.as_bytes());
            exercise(&mangled);
            checked += 1;
        }
    }
    eprintln!("exercised {checked} corrupt block indices");
    assert!(checked > 0);
}

/// Structured inputs a hostile or careless writer might produce.
#[test]
fn adversarial_inputs_never_panic() {
    let cases: &[(&str, Vec<u8>)] = &[
        ("empty", vec![]),
        ("header only", b"#ASDF 1.0.0\n".to_vec()),
        ("header no newline", b"#ASDF 1.0.0".to_vec()),
        ("no version", b"#ASDF \n".to_vec()),
        ("huge version", format!("#ASDF {}\n", "9".repeat(10_000)).into_bytes()),
        ("only magic", b"\xd3BLK".to_vec()),
        ("magic then nothing", b"#ASDF 1.0.0\n\xd3BLK".to_vec()),
        ("header size zero", {
            let mut v = b"#ASDF 1.0.0\n".to_vec();
            v.extend_from_slice(b"\xd3BLK\x00\x00");
            v
        }),
        ("header size max", {
            let mut v = b"#ASDF 1.0.0\n".to_vec();
            v.extend_from_slice(b"\xd3BLK\xff\xff");
            v
        }),
        ("tree never terminated", b"#ASDF 1.0.0\n%YAML 1.1\n--- !core/asdf-1.1.0\na: 1\n".to_vec()),
        ("deeply nested tree", {
            let mut v = b"#ASDF 1.0.0\n%YAML 1.1\n--- ".to_vec();
            v.extend(core::iter::repeat_n(b'[', 5000));
            v.extend(core::iter::repeat_n(b']', 5000));
            v.extend_from_slice(b"\n...\n");
            v
        }),
        ("many blocks claimed", {
            let mut v = b"#ASDF 1.0.0\n".to_vec();
            for _ in 0..100 {
                v.extend_from_slice(b"\xd3BLK\x00\x30");
                v.extend_from_slice(&[0u8; 48]);
            }
            v
        }),
        ("invalid utf8 tree", {
            let mut v = b"#ASDF 1.0.0\n%YAML 1.1\n--- ".to_vec();
            v.extend_from_slice(&[0xff, 0xfe, 0xfd]);
            v.extend_from_slice(b"\n...\n");
            v
        }),
        ("index without blocks", {
            let mut v = b"#ASDF 1.0.0\n%YAML 1.1\n--- {}\n...\n".to_vec();
            v.extend_from_slice(b"#ASDF BLOCK INDEX\n%YAML 1.1\n---\n- 12\n...\n");
            v
        }),
    ];

    for (name, bytes) in cases {
        // Any panic here fails the test by unwinding out of it, which is the
        // point; the name makes the culprit obvious.
        exercise(bytes);
        eprintln!("  ok: {name}");
    }
}

/// Very large declared sizes must be refused rather than allocated.
#[test]
fn absurd_declared_sizes_do_not_allocate() {
    // A compressed block claiming to inflate to a preposterous size is the
    // classic decompression-bomb shape.
    let mut file = b"#ASDF 1.0.0\n%YAML 1.1\n--- {}\n...\n".to_vec();
    file.extend_from_slice(b"\xd3BLK\x00\x30");
    let mut header = [0u8; 48];
    header[4..8].copy_from_slice(b"zlib");
    // allocated_size and used_size of 16, data_size of ~16 exabytes.
    header[8..16].copy_from_slice(&16u64.to_be_bytes());
    header[16..24].copy_from_slice(&16u64.to_be_bytes());
    header[24..32].copy_from_slice(&u64::MAX.to_be_bytes());
    file.extend_from_slice(&header);
    file.extend_from_slice(&[0u8; 16]);

    let reader = Reader::from_bytes(file).expect("the layout itself is well formed");
    // The read must fail rather than trying to allocate the claimed size.
    assert!(
        reader.block_data(0).is_err(),
        "a block claiming to inflate to u64::MAX must be refused"
    );
}

/// The four inputs that used to take the process down.
///
/// Each of these is an *abort*, not a panic: a failed allocation or an
/// overflowed stack. No `catch_unwind` anywhere -- including the one every
/// `libasdf-rs` entry point wraps itself in -- can turn one back into an
/// error return, so there is nothing to catch and the C caller's process
/// simply dies. They are pinned here as assertions about the refusal rather
/// than as "does not panic", because before the fix they did not panic
/// either.
mod aborts {
    use asdf_core::Reader;
    use asdf_core::core::elements::decode_all;
    use asdf_core::core::ndarray::Ndarray;

    const HEADER: &[u8] =
        b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n\
          --- !core/asdf-1.1.0\n";

    fn file_with(tree: &str, block: &[u8], compression: &[u8], data_size: u64) -> Vec<u8> {
        let mut f = HEADER.to_vec();
        f.extend_from_slice(tree.as_bytes());
        f.extend_from_slice(b"...\n");
        let mut header = [0u8; 48];
        header[4..4 + compression.len()].copy_from_slice(compression);
        header[8..16].copy_from_slice(&(block.len() as u64).to_be_bytes());
        header[16..24].copy_from_slice(&(block.len() as u64).to_be_bytes());
        header[24..32].copy_from_slice(&data_size.to_be_bytes());
        f.extend_from_slice(b"\xd3BLK\x00\x30");
        f.extend_from_slice(&header);
        f.extend_from_slice(block);
        f
    }

    /// A shape the block cannot hold must be refused before it is allocated.
    ///
    /// `[100000000, 100000000]` is 10^16 elements. Decoded, each is an
    /// `Element` several times wider than the stored value, so reserving for
    /// them asks for hundreds of petabytes -- from a file of 225 bytes.
    #[test]
    fn an_impossible_shape_is_refused_before_it_is_allocated() {
        let tree = "arr: !core/ndarray-1.1.0\n  source: 0\n  datatype: float64\n  \
                    byteorder: little\n  shape: [100000000, 100000000]\n";
        let bytes = file_with(tree, &[0u8; 8], b"", 8);
        assert!(bytes.len() < 400, "the whole attack is {} bytes", bytes.len());

        let reader = Reader::from_bytes(bytes).expect("the layout is well formed");
        let doc = reader.tree().unwrap().unwrap();
        let node = doc.mapping_get(doc.root().unwrap(), "arr").unwrap();
        let nd = Ndarray::parse(&doc, node).unwrap();
        let shape = nd.resolved_shape(Some(8)).unwrap();
        let data = reader.block_data(0).unwrap();

        let err = decode_all(&nd, &shape, &data).expect_err("must refuse");
        assert!(
            format!("{err}").contains("block holds"),
            "the refusal should name the mismatch, got: {err}"
        );
    }

    /// A shape whose product wraps must not come back as a small count.
    ///
    /// `(1 << 63) + 1` elements of two bytes is exactly 2 bytes once the
    /// multiplication wraps -- so a size check done in the same arithmetic
    /// passes, and whatever is sized from it is far too small.
    #[test]
    fn a_shape_whose_product_wraps_is_refused() {
        let nelems: u64 = (1 << 63) + 1;
        let tree = format!(
            "arr: !core/ndarray-1.1.0\n  source: 0\n  datatype: int16\n  \
             byteorder: little\n  shape: [{nelems}, 2]\n"
        );
        let reader =
            Reader::from_bytes(file_with(&tree, &[0u8; 64], b"", 64)).expect("well formed");
        let doc = reader.tree().unwrap().unwrap();
        let node = doc.mapping_get(doc.root().unwrap(), "arr").unwrap();
        let nd = Ndarray::parse(&doc, node).unwrap();

        // The count itself must not wrap...
        assert!(nd.len(Some(64)).is_err(), "a wrapping element count must be an error");

        // ...and neither must the decode that would be sized from it.
        let shape = nd.resolved_shape(Some(64)).unwrap();
        let data = reader.block_data(0).unwrap();
        assert!(decode_all(&nd, &shape, &data).is_err(), "must refuse");
    }

    /// A stream that inflates past what its header declares must be refused.
    ///
    /// The ratio guard bounds the *declared* size against the compressed
    /// length, which a bomb evades simply by understating it: declare eight
    /// bytes and let the codec produce as many as it likes.
    #[test]
    fn a_compressed_block_may_not_inflate_past_its_declared_size() {
        let raw = vec![0u8; 8 << 20];
        let comp = asdf_core::compression::Compression::Zlib.compress(&raw).unwrap();
        assert!(comp.len() < 16 << 10, "8 MiB of zeros should compress small");

        let tree = "x: 1\n";
        let reader = Reader::from_bytes(file_with(tree, &comp, b"zlib", 8)).expect("well formed");

        let err = reader.block_data(0).expect_err("must refuse");
        assert!(
            format!("{err}").contains("declares"),
            "the refusal should name the declared size, got: {err}"
        );

        // The honest version of the same block still reads.
        let ok = Reader::from_bytes(file_with(tree, &comp, b"zlib", raw.len() as u64)).unwrap();
        assert_eq!(ok.block_data(0).unwrap().len(), raw.len());
    }

    /// An alias pointing at its own ancestor must not be followed for ever.
    #[test]
    fn a_self_referential_alias_does_not_recurse_for_ever() {
        let mut f = HEADER.to_vec();
        f.extend_from_slice(b"a: &a\n  b: *a\n...\n");
        assert!(f.len() < 128, "the whole attack is {} bytes", f.len());

        let reader = Reader::from_bytes(f).expect("well formed");
        let rendered = asdf_core::info::render(&reader, asdf_core::info::InfoOptions::default())
            .expect("rendering a cyclic tree should succeed, not recurse");
        assert!(rendered.contains("(...)"), "the cycle should be marked, not followed");
    }

    /// Nested aliases must not expand without bound.
    ///
    /// Ten levels of ten-way nesting is 10^10 nodes from a few hundred
    /// bytes. Expanding aliases is what `asdf info` is for, so the answer is
    /// a budget rather than a refusal.
    #[test]
    fn nested_aliases_render_within_a_budget() {
        let mut tree = String::from("a: &a [x,x,x,x,x,x,x,x,x,x]\n");
        let mut prev = 'a';
        for name in "bcdefghij".chars() {
            tree.push_str(&format!("{name}: &{name} ["));
            tree.push_str(&vec![format!("*{prev}"); 10].join(","));
            tree.push_str("]\n");
            prev = name;
        }
        let mut f = HEADER.to_vec();
        f.extend_from_slice(tree.as_bytes());
        f.extend_from_slice(b"...\n");
        assert!(f.len() < 600, "the whole attack is {} bytes", f.len());

        let reader = Reader::from_bytes(f).expect("well formed");
        let rendered = asdf_core::info::render(&reader, asdf_core::info::InfoOptions::default())
            .expect("rendering should complete");
        assert!(
            rendered.len() < 128 << 20,
            "rendering ran past its budget at {} bytes",
            rendered.len()
        );
    }
}
