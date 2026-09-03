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
            v.extend(std::iter::repeat_n(b'[', 5000));
            v.extend(std::iter::repeat_n(b']', 5000));
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
