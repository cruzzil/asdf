//! Tier 3: compare `asdf events --verbose` against libasdf's committed
//! expected files.
//!
//! `tests/fixtures/events/*.events.txt` upstream are exact captures of the
//! low-level event stream. They pin two things nothing else does: the *order*
//! events come in -- which is not the file's own order, since the block index
//! is reported before the tree -- and the names libfyaml gives YAML events,
//! which are the YAML test suite's notation (`+MAP`, `=VAL`, `-SEQ`) and
//! appear in no header.

use std::path::PathBuf;

use asdf_core::events::{EventOptions, events, render_event};

fn libasdf_dir() -> Option<PathBuf> {
    let path = std::env::var_os("LIBASDF_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("code/libasdf")))?;
    path.is_dir().then_some(path)
}

fn standard_dir() -> Option<PathBuf> {
    let path = std::env::var_os("ASDF_STANDARD_DIR").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("code/asdf-standard"))
    })?;
    path.is_dir().then_some(path)
}

/// The files upstream's `test-events.sh` renders, all from the 1.6.0
/// reference corpus and all with `--verbose`.
const CASES: &[&str] = &["basic", "complex", "compressed", "int"];

#[test]
fn event_output_matches_the_committed_goldens() {
    let (Some(root), Some(standard)) = (libasdf_dir(), standard_dir()) else {
        eprintln!("skipping: LIBASDF_DIR or ASDF_STANDARD_DIR not found");
        return;
    };
    let goldens = root.join("tests/fixtures/events");
    if !goldens.is_dir() {
        eprintln!("skipping: no event goldens found");
        return;
    }

    let mut matched = Vec::new();
    let mut differed = Vec::new();
    let mut missing = 0;

    for name in CASES {
        let golden_path = goldens.join(format!("{name}.events.txt"));
        let input = standard.join("reference_files/1.6.0").join(format!("{name}.asdf"));
        let (Ok(expected), Ok(buf)) =
            (std::fs::read_to_string(&golden_path), std::fs::read(&input))
        else {
            missing += 1;
            continue;
        };

        // `test-events.sh` passes `--verbose`, and YAML events are on by
        // default.
        let stream = match events(&buf, EventOptions { yaml: true }) {
            Ok(stream) => stream,
            Err(e) => {
                differed.push((*name, format!("could not read: {e}")));
                continue;
            }
        };
        let actual: String = stream.iter().map(|e| render_event(e, true)).collect();

        if actual == expected {
            matched.push(*name);
            continue;
        }

        let mut detail = format!(
            "output agrees line for line but lengths differ ({} vs {} lines)",
            actual.lines().count(),
            expected.lines().count()
        );
        for (index, (ours, want)) in actual.lines().zip(expected.lines()).enumerate() {
            if ours != want {
                detail = format!("line {}:\n        ours: {ours}\n        want: {want}", index + 1);
                break;
            }
        }
        differed.push((*name, detail));
    }

    eprintln!(
        "event goldens: {} matched, {} differed, {missing} unavailable",
        matched.len(),
        differed.len()
    );
    if !matched.is_empty() {
        eprintln!("  matched: {}", matched.join(", "));
    }
    for (name, detail) in &differed {
        eprintln!("  {name}: {detail}");
    }

    assert!(differed.is_empty(), "{} golden(s) differ from upstream's output", differed.len());
    assert!(matched.len() + missing == CASES.len());
}
