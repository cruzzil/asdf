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
    assert!(
        files.len() >= 100,
        "expected the full reference corpus, found {} files",
        files.len()
    );

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
                    failures.push(format!(
                        "{}: block index rejected: {reason:?}",
                        rel(&refs, path)
                    ));
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
            let layout = scan(&buf)
                .unwrap_or_else(|e| panic!("{}: {e}", rel(&refs, path)));
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
    let insert_at = buf
        .windows(6)
        .position(|w| w == b"%YAML ")
        .expect("tree directive");
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
    assert!(matches!(
        layout.index_rejection,
        Some(IndexRejection::FirstOffsetMismatch { .. })
    ));
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
