//! Tier 1 and 2 of the test plan: scan every file in the ASDF Standard's
//! reference corpus and libasdf's own fixture set.
//!
//! These corpora are the external definition of correctness, so they are
//! wired in from the first phase rather than at the end. Both live outside
//! this repository; set `ASDF_STANDARD_DIR` and `LIBASDF_DIR` to point at
//! them, or rely on the `~/code` defaults. Tests skip with a printed note
//! when a corpus is absent, so a checkout without them still builds green.

use std::path::{Path, PathBuf};

use asdf_core::layout::{IndexRejection, scan};

fn corpus_dir(env: &str, default: &str) -> Option<PathBuf> {
    let path = std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(default)))?;
    path.is_dir().then_some(path)
}

fn standard_dir() -> Option<PathBuf> {
    corpus_dir("ASDF_STANDARD_DIR", "code/asdf-standard")
}

fn libasdf_dir() -> Option<PathBuf> {
    corpus_dir("LIBASDF_DIR", "code/libasdf")
}

/// Every `.asdf` file under `root`, sorted for stable output.
fn asdf_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "asdf") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

#[test]
fn scans_every_standard_reference_file() {
    let Some(root) = standard_dir() else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let refs = root.join("reference_files");
    let files = asdf_files(&refs);
    assert!(files.len() >= 100, "expected the full reference corpus, found {} files", files.len());

    let mut failures = Vec::new();
    let mut with_blocks = 0;
    let mut with_index = 0;
    let mut streamed = 0;

    for path in &files {
        let buf = std::fs::read(path).expect("read reference file");
        match scan(&buf) {
            Ok(layout) => {
                if !layout.blocks.is_empty() {
                    with_blocks += 1;
                }
                if layout.used_block_index() {
                    with_index += 1;
                }
                if layout.blocks.iter().any(|b| b.header.is_streamed()) {
                    streamed += 1;
                }
                // Every block's data must lie inside the file.
                for b in &layout.blocks {
                    if b.header.is_streamed() {
                        continue;
                    }
                    let end = b.end_pos();
                    if end > buf.len() as u64 {
                        failures.push(format!(
                            "{}: block {} ends at {} beyond EOF {}",
                            rel(&refs, path),
                            b.index,
                            end,
                            buf.len()
                        ));
                    }
                }
                // A rejected index is legitimate, but it should not happen
                // across a corpus written by the reference implementation.
                if let Some(reason) = &layout.index_rejection {
                    failures
                        .push(format!("{}: block index rejected: {reason:?}", rel(&refs, path)));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", rel(&refs, path))),
        }
    }

    eprintln!(
        "scanned {} reference files: {with_blocks} with blocks, \
         {with_index} with a valid index, {streamed} streamed",
        files.len()
    );
    assert!(failures.is_empty(), "failures:\n  {}", failures.join("\n  "));
}

#[test]
fn every_standard_version_is_represented() {
    let Some(root) = standard_dir() else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let refs = root.join("reference_files");

    for version in ["1.0.0", "1.1.0", "1.2.0", "1.3.0", "1.4.0", "1.5.0", "1.6.0"] {
        let dir = refs.join(version);
        if !dir.is_dir() {
            continue;
        }
        let files = asdf_files(&dir);
        assert!(!files.is_empty(), "no files for standard {version}");

        for path in &files {
            let buf = std::fs::read(path).unwrap();
            let layout = scan(&buf).unwrap_or_else(|e| panic!("{}: {e}", rel(&refs, path)));
            // The format version on the header line is independent of the
            // standard version and has been 1.0.0 throughout.
            assert_eq!(
                layout.format_version.triple(),
                (1, 0, 0),
                "{}: unexpected format version",
                rel(&refs, path)
            );
        }
    }
}

#[test]
fn tree_extents_are_parseable_yaml() {
    let Some(root) = standard_dir() else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let refs = root.join("reference_files");

    let mut checked = 0;
    let mut failures = Vec::new();
    for path in asdf_files(&refs) {
        let buf = std::fs::read(&path).unwrap();
        let Ok(layout) = scan(&buf) else { continue };
        let Some(text) = layout.tree_str(&buf) else { continue };

        // The extent we found must be a complete, self-contained YAML
        // document -- this is what validates the end-marker search.
        match asdf_yaml::parse_document(text) {
            Ok(doc) => {
                assert!(doc.root().is_some(), "{}: tree has no root", rel(&refs, &path));
                checked += 1;
            }
            Err(e) => failures.push(format!("{}: {e}", rel(&refs, &path))),
        }
    }
    eprintln!("parsed {checked} reference trees");
    assert!(checked > 50, "expected to parse most of the corpus, got {checked}");
    assert!(failures.is_empty(), "failures:\n  {}", failures.join("\n  "));
}

#[test]
fn scans_libasdf_fixtures() {
    let Some(root) = libasdf_dir() else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let fixtures = root.join("tests/fixtures");
    let files = asdf_files(&fixtures);
    if files.is_empty() {
        eprintln!("skipping: no fixtures found");
        return;
    }

    // Every fixture currently scans, including the ones that exercise
    // awkward layouts (no tree padding, no newline before the tree, no
    // block index). A fixture that starts failing is a regression, so
    // there is no allow-list here on purpose.
    let mut ok = 0;
    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let buf = std::fs::read(path).unwrap();

        match scan(&buf) {
            Ok(layout) => {
                ok += 1;
                for b in &layout.blocks {
                    if !b.header.is_streamed() && b.end_pos() > buf.len() as u64 {
                        failures.push(format!("{name}: block {} runs past EOF", b.index));
                    }
                }
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    eprintln!("scanned {} libasdf fixtures, {ok} parsed", files.len());
    assert!(failures.is_empty(), "failures:\n  {}", failures.join("\n  "));
}

/// The fixtures libasdf ships specifically to exercise index handling.
#[test]
fn handles_the_block_index_edge_case_fixtures() {
    let Some(root) = libasdf_dir() else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let fixtures = root.join("tests/fixtures");

    // A file whose index was removed must still yield its blocks, found by
    // skipping along -- the property that makes the index optional.
    let no_index = fixtures.join("255-block-no-index.asdf");
    if no_index.is_file() {
        let buf = std::fs::read(&no_index).unwrap();
        let layout = scan(&buf).unwrap();
        assert!(!layout.blocks.is_empty(), "blocks must be found without an index");
        assert!(layout.block_index_pos.is_none());
    }

    // Two blocks, both discoverable.
    let two = fixtures.join("255-2-blocks.asdf");
    if two.is_file() {
        let buf = std::fs::read(&two).unwrap();
        let layout = scan(&buf).unwrap();
        assert_eq!(layout.blocks.len(), 2);
    }

    // An invalid checksum is a data-level problem, not a layout one: the
    // file must still scan.
    let bad_sum = fixtures.join("255-invalid-checksum.asdf");
    if bad_sum.is_file() {
        let buf = std::fs::read(&bad_sum).unwrap();
        let layout = scan(&buf).unwrap();
        assert!(!layout.blocks.is_empty());
        assert!(layout.blocks[0].header.has_checksum());
    }
}

/// Scanning must never panic, however mangled the input.
#[test]
fn truncations_never_panic() {
    let Some(root) = libasdf_dir() else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let path = root.join("tests/fixtures/255.asdf");
    if !path.is_file() {
        return;
    }
    let buf = std::fs::read(&path).unwrap();

    // Every prefix of a real file, plus every prefix with one byte flipped.
    for len in 0..buf.len().min(2048) {
        let _ = scan(&buf[..len]);
    }
    for cut in [buf.len() / 4, buf.len() / 2, buf.len() * 3 / 4] {
        let mut mangled = buf[..cut].to_vec();
        if let Some(last) = mangled.last_mut() {
            *last ^= 0xff;
        }
        let _ = scan(&mangled);
    }
}

/// A block index that has been tampered with must be rejected, not trusted.
#[test]
fn a_tampered_index_is_rejected() {
    let Some(root) = libasdf_dir() else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let path = root.join("tests/fixtures/255.asdf");
    if !path.is_file() {
        return;
    }
    let buf = std::fs::read(&path).unwrap();
    let layout = scan(&buf).unwrap();
    assert!(layout.used_block_index(), "baseline file should have a good index");

    // Shift the tree by inserting a comment, exactly the hand-edit the
    // standard warns about, and the offsets go stale.
    let insert_at = buf.windows(6).position(|w| w == b"%YAML ").expect("tree directive");
    let mut edited = Vec::new();
    edited.extend_from_slice(&buf[..insert_at]);
    edited.extend_from_slice(b"# an extra comment line\n");
    edited.extend_from_slice(&buf[insert_at..]);

    let layout = scan(&edited).unwrap();
    assert!(
        !layout.used_block_index(),
        "a stale index must be rejected, got {:?}",
        layout.index_rejection
    );
    assert!(matches!(layout.index_rejection, Some(IndexRejection::FirstOffsetMismatch { .. })));
    // ...and the blocks must still be found by skipping along.
    assert!(!layout.blocks.is_empty());
}

/// Every checksummed block across both corpora must verify.
///
/// This is the tier-1 data-level gate: it exercises block location, the
/// stored-versus-decompressed distinction, all three decompressors, and the
/// Python asdf checksum-bug workaround, against files written by two other
/// implementations.
#[test]
fn every_checksummed_block_verifies() {
    use asdf_core::reader::{ChecksumStatus, Reader};

    let mut roots = Vec::new();
    if let Some(r) = standard_dir() {
        roots.push(r.join("reference_files"));
    }
    if let Some(r) = libasdf_dir() {
        roots.push(r.join("tests/fixtures"));
    }
    if roots.is_empty() {
        eprintln!("skipping: no corpora found");
        return;
    }

    // The one fixture upstream ships specifically to *fail* verification.
    let known_bad = ["255-invalid-checksum.asdf"];

    let mut checked = 0;
    let mut absent = 0;
    let mut compressed = 0;
    let mut workarounds = 0;
    let mut failures = Vec::new();

    for root in &roots {
        for path in asdf_files(root) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let Ok(reader) = Reader::open(&path) else { continue };
            let uses_workaround = reader.has_python_checksum_bug();

            for idx in 0..reader.block_count() {
                // A streamed block's size fields are meaningless, so there is
                // nothing well-defined to checksum.
                if reader.block(idx).map(|b| b.header.is_streamed()).unwrap_or(false) {
                    continue;
                }
                let Ok((status, computed)) = reader.verify_block_checksum(idx) else {
                    failures.push(format!("{name}: block {idx}: verification errored"));
                    continue;
                };
                match status {
                    ChecksumStatus::Absent => absent += 1,
                    ChecksumStatus::Valid => {
                        checked += 1;
                        if reader
                            .block_compression(idx)
                            .is_ok_and(|c| c != asdf_core::compression::Compression::None)
                        {
                            compressed += 1;
                            if uses_workaround {
                                workarounds += 1;
                            }
                        }
                    }
                    ChecksumStatus::Invalid => {
                        if !known_bad.contains(&name.as_str()) {
                            failures.push(format!(
                                "{name}: block {idx}: checksum mismatch (computed {})",
                                computed.iter().map(|b| format!("{b:02x}")).collect::<String>()
                            ));
                        }
                    }
                }
            }
        }
    }

    eprintln!(
        "verified {checked} checksums ({compressed} on compressed blocks, \
         {workarounds} via the Python checksum-bug workaround); \
         {absent} blocks carry no checksum"
    );
    assert!(checked > 0, "no checksums were verified at all");
    assert!(failures.is_empty(), "failures:\n  {}", failures.join("\n  "));
}

/// The fixture upstream ships to exercise a *failing* checksum must fail.
#[test]
fn the_invalid_checksum_fixture_is_detected() {
    use asdf_core::reader::{ChecksumStatus, Reader};

    let Some(root) = libasdf_dir() else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let path = root.join("tests/fixtures/255-invalid-checksum.asdf");
    if !path.is_file() {
        eprintln!("skipping: fixture not present");
        return;
    }

    let reader = Reader::open(&path).unwrap();
    let (status, _) = reader.verify_block_checksum(0).unwrap();
    assert_eq!(
        status,
        ChecksumStatus::Invalid,
        "a corrupt checksum must be reported, not silently accepted"
    );
}

/// Block data must be reachable for every block in both corpora.
#[test]
fn all_block_data_is_readable() {
    use asdf_core::reader::Reader;

    let mut roots = Vec::new();
    if let Some(r) = standard_dir() {
        roots.push(r.join("reference_files"));
    }
    if let Some(r) = libasdf_dir() {
        roots.push(r.join("tests/fixtures"));
    }

    let mut blocks = 0;
    let mut bytes = 0usize;
    let mut failures = Vec::new();

    for root in &roots {
        for path in asdf_files(root) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let Ok(reader) = Reader::open(&path) else { continue };
            for idx in 0..reader.block_count() {
                match reader.block_data(idx) {
                    Ok(data) => {
                        blocks += 1;
                        bytes += data.len();
                        // A non-streamed block's decompressed length must
                        // match what its header promised.
                        let header = &reader.block(idx).unwrap().header;
                        if !header.is_streamed() && data.len() as u64 != header.data_size {
                            failures.push(format!(
                                "{name}: block {idx}: got {} bytes, header says {}",
                                data.len(),
                                header.data_size
                            ));
                        }
                    }
                    Err(e) => failures.push(format!("{name}: block {idx}: {e}")),
                }
            }
        }
    }

    eprintln!("read {blocks} blocks totalling {bytes} bytes");
    assert!(blocks > 0, "no blocks were read");
    assert!(failures.is_empty(), "failures:\n  {}", failures.join("\n  "));
}

/// Every reference tree must survive being emitted and read back.
///
/// This is the emitter's real test: 112 documents written by two other
/// implementations, covering tags, anchors and aliases, every scalar style,
/// unicode, and deeply nested structures. Equality is judged at the value
/// level, so formatting may differ freely while meaning may not.
#[test]
fn every_reference_tree_survives_a_round_trip() {
    use asdf_yaml::{CompareOptions, compare, emit, parse_document};

    let Some(root) = standard_dir() else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let refs = root.join("reference_files");

    let mut round_tripped = 0;
    let mut failures = Vec::new();

    for path in asdf_files(&refs) {
        let name = rel(&refs, &path);
        let buf = std::fs::read(&path).unwrap();
        let Ok(layout) = scan(&buf) else { continue };
        let Some(text) = layout.tree_str(&buf) else { continue };
        let Ok(original) = parse_document(text) else { continue };

        let emitted = match emit(&original) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{name}: emit failed: {e}"));
                continue;
            }
        };

        let reparsed = match parse_document(&emitted) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{name}: emitted output does not parse: {e}"));
                continue;
            }
        };

        let result = compare(&original, &reparsed, CompareOptions::default());
        if result.is_equal() {
            round_tripped += 1;
        } else {
            let detail: Vec<String> =
                result.differences.iter().take(3).map(|d| d.to_string()).collect();
            failures.push(format!("{name}:\n      {}", detail.join("\n      ")));
        }
    }

    eprintln!("{round_tripped} reference trees survived an emit/parse round trip");
    assert!(round_tripped > 100, "expected the whole corpus, got {round_tripped}");
    assert!(
        failures.is_empty(),
        "{} tree(s) did not round trip:\n    {}",
        failures.len(),
        failures.iter().take(5).cloned().collect::<Vec<_>>().join("\n    ")
    );
}

/// Emitted trees must also be readable as complete ASDF tree sections.
///
/// A tree the emitter writes has to carry the directives and the `...`
/// terminator, since a reader locates the tree's end by searching for it.
#[test]
fn emitted_trees_carry_the_markers_a_reader_needs() {
    use asdf_yaml::{emit, parse_document};

    let Some(root) = standard_dir() else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let path = root.join("reference_files/1.6.0/basic.asdf");
    if !path.is_file() {
        return;
    }

    let buf = std::fs::read(&path).unwrap();
    let layout = scan(&buf).unwrap();
    let doc = parse_document(layout.tree_str(&buf).unwrap()).unwrap();
    let emitted = emit(&doc).unwrap();

    assert!(emitted.starts_with("%YAML 1.1\n"), "{emitted}");
    assert!(emitted.contains("%TAG ! tag:stsci.edu:asdf/\n"), "{emitted}");
    assert!(emitted.contains("--- !core/asdf-1.1.0\n"), "{emitted}");
    assert!(emitted.ends_with("...\n"), "{emitted}");

    // Reassembled into a file, the layout scanner must find exactly this tree.
    let mut file = Vec::new();
    file.extend_from_slice(b"#ASDF 1.0.0\n#ASDF_STANDARD 1.6.0\n");
    file.extend_from_slice(emitted.as_bytes());
    let relaid = scan(&file).unwrap();
    assert_eq!(
        relaid.tree_str(&file).unwrap(),
        emitted,
        "the scanner must recover exactly the tree that was written"
    );
}

/// Rewrite every reference file with our own writer and read it back.
///
/// This closes the loop on the write path: for each file in the corpus, take
/// its tree and every one of its blocks, assemble a fresh ASDF file, and
/// check that reading that file yields the same tree and byte-identical
/// block data. It exercises the emitter, the block writer, checksum
/// generation and block-index generation against 112 real files.
#[test]
fn every_reference_file_survives_being_rewritten() {
    use asdf_core::compression::Compression;
    use asdf_core::reader::{ChecksumStatus, Reader};
    use asdf_core::writer::{PendingBlock, Writer};
    use asdf_yaml::{CompareOptions, compare};

    let Some(root) = standard_dir() else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let refs = root.join("reference_files");

    let mut rewritten = 0;
    let mut blocks_copied = 0;
    let mut failures = Vec::new();

    for path in asdf_files(&refs) {
        let name = rel(&refs, &path);
        let Ok(source) = Reader::open(&path) else { continue };

        // A streamed block has no well-defined size to copy, so those files
        // are left to phase 9.
        if (0..source.block_count()).any(|i| source.block(i).is_ok_and(|b| b.header.is_streamed()))
        {
            continue;
        }

        let Ok(Some(original_tree)) = source.tree() else { continue };

        let mut writer = Writer::from_document(original_tree.clone());
        let mut expected_blocks = Vec::new();
        let mut ok = true;

        for index in 0..source.block_count() {
            let Ok(data) = source.block_data(index) else {
                ok = false;
                break;
            };
            let compression = source.block_compression(index).unwrap_or(Compression::None);
            expected_blocks.push(data.to_vec());
            writer.add_block(PendingBlock::compressed(data.to_vec(), compression));
        }
        if !ok {
            failures.push(format!("{name}: could not read a block"));
            continue;
        }

        let bytes = match writer.to_bytes() {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{name}: write failed: {e}"));
                continue;
            }
        };

        let rebuilt = match Reader::from_bytes(bytes) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{name}: the file we wrote will not scan: {e}"));
                continue;
            }
        };

        // The tree must survive.
        match rebuilt.tree() {
            Ok(Some(tree)) => {
                let result = compare(&original_tree, &tree, CompareOptions::default());
                if !result.is_equal() {
                    let detail: Vec<String> =
                        result.differences.iter().take(3).map(|d| d.to_string()).collect();
                    failures
                        .push(format!("{name}: tree changed:\n      {}", detail.join("\n      ")));
                    continue;
                }
            }
            _ => {
                failures.push(format!("{name}: the rewritten file has no tree"));
                continue;
            }
        }

        // Every block must come back byte for byte.
        if rebuilt.block_count() != expected_blocks.len() {
            failures.push(format!(
                "{name}: wrote {} blocks, read back {}",
                expected_blocks.len(),
                rebuilt.block_count()
            ));
            continue;
        }
        for (index, expected) in expected_blocks.iter().enumerate() {
            match rebuilt.block_data(index) {
                Ok(actual) if actual.as_ref() == expected.as_slice() => blocks_copied += 1,
                Ok(actual) => failures.push(format!(
                    "{name}: block {index} differs ({} bytes vs {})",
                    actual.len(),
                    expected.len()
                )),
                Err(e) => failures.push(format!("{name}: block {index}: {e}")),
            }
            // And the checksum we generated must verify.
            if let Ok((status, _)) = rebuilt.verify_block_checksum(index)
                && status != ChecksumStatus::Valid
            {
                failures.push(format!("{name}: block {index}: checksum {status:?}"));
            }
        }

        // A written index must be one we would accept on read.
        if rebuilt.block_count() > 0 && !rebuilt.layout().used_block_index() {
            failures.push(format!(
                "{name}: our own block index was rejected: {:?}",
                rebuilt.layout().index_rejection
            ));
        }
        rewritten += 1;
    }

    eprintln!("rewrote and re-read {rewritten} reference files, {blocks_copied} blocks copied");
    assert!(rewritten > 90, "expected most of the corpus, got {rewritten}");
    assert!(
        failures.is_empty(),
        "{} file(s) failed to survive a rewrite:\n    {}",
        failures.len(),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n    ")
    );
}
