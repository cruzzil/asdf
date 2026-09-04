//! Tier 3: compare `asdf verify-checksums --verbose` against libasdf's
//! committed expected files.
//!
//! `tests/fixtures/verify-checksums/*.txt` upstream are exact captures of
//! what the sub-command prints. They pin the wording and the layout, and one
//! of the three fixtures has a deliberately wrong digest, so the failure
//! path is captured as precisely as the success one.

use std::path::PathBuf;
use std::process::Command;

fn libasdf_dir() -> Option<PathBuf> {
    let path = std::env::var_os("LIBASDF_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("code/libasdf")))?;
    path.is_dir().then_some(path)
}

/// The `asdf` binary this crate builds.
fn cli() -> Option<PathBuf> {
    // The test binary lives at <target>/<profile>/deps/<name>-<hash>.
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    let path = dir.join("asdf");
    path.is_file().then_some(path)
}

/// The files upstream's `test-verify-checksums.sh` runs, all with
/// `--verbose`.
const CASES: &[&str] = &["255-2-blocks", "255-invalid-checksum", "compressed"];

#[test]
fn verify_checksums_output_matches_the_committed_goldens() {
    let Some(root) = libasdf_dir() else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let Some(cli) = cli() else {
        eprintln!("skipping: the asdf binary is not built; run `cargo build` first");
        return;
    };
    let goldens = root.join("tests/fixtures/verify-checksums");
    if !goldens.is_dir() {
        eprintln!("skipping: no verify-checksums goldens found");
        return;
    }

    let mut matched = Vec::new();
    let mut differed = Vec::new();

    for name in CASES {
        let golden = goldens.join(format!("{name}.verify-checksums.txt"));
        let input = root.join("tests/fixtures").join(format!("{name}.asdf"));
        let (Ok(expected), true) = (std::fs::read_to_string(&golden), input.is_file()) else {
            eprintln!("skipping {name}: fixture or golden missing");
            continue;
        };

        let out = Command::new(&cli)
            .args(["verify-checksums", "--verbose"])
            .arg(&input)
            .output()
            .expect("run the CLI");
        let actual = String::from_utf8_lossy(&out.stdout).into_owned();

        if actual == expected {
            matched.push(*name);
        } else {
            differed.push((*name, actual, expected));
        }
    }

    eprintln!("verify-checksums goldens: {} matched, {} differed", matched.len(), differed.len());
    for (name, actual, expected) in &differed {
        eprintln!("  {name}:\n    ours: {actual:?}\n    want: {expected:?}");
    }
    assert!(differed.is_empty(), "{} golden(s) differ from upstream's output", differed.len());
}

/// A file with a bad digest must fail, and one without must not.
#[test]
fn a_bad_checksum_sets_a_failing_exit_status() {
    let Some(root) = libasdf_dir() else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let Some(cli) = cli() else {
        eprintln!("skipping: the asdf binary is not built");
        return;
    };

    for (name, expect_success) in [("255-2-blocks", true), ("255-invalid-checksum", false)] {
        let input = root.join("tests/fixtures").join(format!("{name}.asdf"));
        if !input.is_file() {
            continue;
        }
        let out =
            Command::new(&cli).arg("verify-checksums").arg(&input).output().expect("run the CLI");
        assert_eq!(out.status.success(), expect_success, "{name}");

        // Without --verbose only the failures are reported, and on stderr.
        assert!(out.stdout.is_empty(), "{name}: a quiet run prints nothing to stdout");
        assert_eq!(
            !out.stderr.is_empty(),
            !expect_success,
            "{name}: failures and only failures go to stderr"
        );
    }
}
