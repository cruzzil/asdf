//! Tier 3: compare `info` output against libasdf's committed expected files.
//!
//! `tests/fixtures/info/*.info.txt` upstream are exact captures of what
//! `asdf info` prints, ANSI styling and all. They are the tightest available
//! check on the whole read path's *presentation*, and the only place in this
//! project where byte-for-byte equality is the criterion -- because here the
//! bytes are the product, not a serialization of it.

use std::path::PathBuf;

use asdf_core::info::{InfoOptions, render};
use asdf_core::reader::Reader;

fn libasdf_dir() -> Option<PathBuf> {
    let path = std::env::var_os("LIBASDF_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("code/libasdf")))?;
    path.is_dir().then_some(path)
}

/// Where each golden's input file lives, and what options produced it.
///
/// Upstream's `test-info.sh` drives these; the mapping is reproduced here.
struct Case {
    /// Basename of the `.info.txt` golden.
    golden: &'static str,
    /// The `.asdf` file it was rendered from.
    input: &'static str,
    /// Whether the golden includes block tables.
    blocks: bool,
}

const CASES: &[Case] = &[
    Case { golden: "basic", input: "basic.asdf", blocks: true },
    Case { golden: "int", input: "int.asdf", blocks: true },
    Case { golden: "float", input: "float.asdf", blocks: true },
    Case { golden: "ascii", input: "ascii.asdf", blocks: true },
    Case { golden: "complex", input: "complex.asdf", blocks: true },
    Case { golden: "endian", input: "endian.asdf", blocks: true },
    Case { golden: "shared", input: "shared.asdf", blocks: true },
    Case { golden: "scalars", input: "scalars.asdf", blocks: true },
    Case { golden: "structured", input: "structured.asdf", blocks: true },
    Case { golden: "unicode_bmp", input: "unicode_bmp.asdf", blocks: true },
    Case { golden: "unicode_spp", input: "unicode_spp.asdf", blocks: true },
    Case { golden: "stream", input: "stream.asdf", blocks: true },
    Case { golden: "compressed", input: "compressed.asdf", blocks: true },
    Case { golden: "exploded", input: "exploded.asdf", blocks: true },
    Case { golden: "anchor", input: "anchor.asdf", blocks: true },
    Case { golden: "exploded0000", input: "exploded0000.asdf", blocks: true },
    Case { golden: "roman_l2_wcs", input: "roman_l2_wcs.asdf", blocks: true },
];

/// Find a golden's input.
///
/// Upstream's `test-info.sh` renders `asdf-standard/reference_files/1.6.0/*.asdf`
/// plus `fixtures/roman_l2_wcs.asdf`, so the reference corpus is searched
/// first. The order matters: libasdf's own fixtures directory contains
/// same-named files written by a *different* asdf version, whose
/// `asdf_library` metadata does not match the goldens.
fn find_input(root: &std::path::Path, name: &str) -> Option<PathBuf> {
    let refs = std::env::var_os("ASDF_STANDARD_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("code/asdf-standard")))
        .map(|r| r.join("reference_files/1.6.0").join(name));
    if let Some(refs) = refs
        && refs.is_file()
    {
        return Some(refs);
    }
    let direct = root.join("tests/fixtures").join(name);
    direct.is_file().then_some(direct)
}

/// Render each golden's input and report which match.
///
/// Rather than failing on the first mismatch, this reports the whole picture:
/// how many match, and the first differing line of each that does not. That
/// makes it usable as a progress signal while the renderer is completed.
#[test]
fn info_output_matches_the_committed_goldens() {
    let Some(root) = libasdf_dir() else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let goldens = root.join("tests/fixtures/info");
    if !goldens.is_dir() {
        eprintln!("skipping: no info goldens found");
        return;
    }

    let mut matched = Vec::new();
    let mut differed = Vec::new();
    let mut missing = 0;

    for case in CASES {
        let golden_path = goldens.join(format!("{}.info.txt", case.golden));
        let Ok(expected) = std::fs::read_to_string(&golden_path) else {
            missing += 1;
            continue;
        };
        let Some(input) = find_input(&root, case.input) else {
            missing += 1;
            continue;
        };
        let Ok(reader) = Reader::open(&input) else {
            differed.push((case.golden, "could not open input".to_string()));
            continue;
        };

        let options = InfoOptions {
            print_tree: true,
            print_blocks: case.blocks,
            verify_checksums: false,
        };
        let Ok(actual) = render(&reader, options) else {
            differed.push((case.golden, "render failed".to_string()));
            continue;
        };

        if actual == expected {
            matched.push(case.golden);
            continue;
        }

        // Report the first line that differs, with escapes made visible.
        let show = |s: &str| s.replace('\x1b', "\\e");
        let mut detail = String::from("output matches until EOF but lengths differ");
        for (idx, (a, e)) in actual.lines().zip(expected.lines()).enumerate() {
            if a != e {
                detail = format!(
                    "line {}:\n        ours: {}\n        want: {}",
                    idx + 1,
                    show(a),
                    show(e)
                );
                break;
            }
        }
        differed.push((case.golden, detail));
    }

    eprintln!(
        "info goldens: {} matched, {} differed, {missing} unavailable",
        matched.len(),
        differed.len()
    );
    if !matched.is_empty() {
        eprintln!("  matched: {}", matched.join(", "));
    }
    for (name, detail) in &differed {
        eprintln!("  {name}: {detail}");
    }

    assert!(
        matched.len() + missing == CASES.len(),
        "{} golden(s) differ from upstream's output",
        differed.len()
    );
}
