//! The dependency set is pinned, so growing it is a deliberate act.
//!
//! Two things ride on this list staying small and known.
//!
//! **Licensing.** Everything written here is MIT (the vendored upstream
//! headers under `crates/libasdf-rs/include/` are the one exception, and stay
//! BSD-3-Clause). That story only holds while every dependency is permissive.
//! A new transitive crate under a copyleft licence would silently break it,
//! and nothing else in the build would notice.
//!
//! **Scope.** The engine is deliberately free of a regex engine and a
//! JSON-Schema validator; see "No schema validation" in `KNOWN-DIVERGENCES.md`
//! for why. Both would arrive as dependencies, so this gate is where that
//! decision is actually enforced rather than merely written down.
//!
//! Adding a dependency is fine. Add it here too, having checked its licence
//! and its own transitive additions.

use std::path::{Path, PathBuf};

/// The workspace members, which are not third-party.
const OURS: &[&str] = &["asdf", "asdf-cli", "asdf-core", "asdf-yaml", "libasdf-rs"];

/// Every external crate in the dependency graph, direct and transitive.
///
/// All are permissive: MIT or Apache-2.0 for most, plus Zlib (`zlib-rs`,
/// `simd-adler32`), 0BSD (`adler2`), BSD-2 (`arraydeque`, `foldhash`), the
/// bzip2 licence (`libbz2-rs-sys`) and Unicode-3.0 (`unicode-ident`).
const EXTERNAL: &[&str] = &[
    "adler2",
    "arraydeque",
    "block-buffer",
    "bzip2",
    "cc",
    "cfg-if",
    "crc32fast",
    "crunchy",
    "crypto-common",
    "digest",
    "find-msvc-tools",
    "flate2",
    "foldhash",
    "generic-array",
    "half",
    "hashbrown",
    "hashlink",
    "libbz2-rs-sys",
    "libc",
    "lz4_flex",
    "md-5",
    "memmap2",
    "miniz_oxide",
    "proc-macro2",
    "quote",
    "saphyr-parser",
    "shlex",
    "simd-adler32",
    "syn",
    "thiserror",
    "thiserror-impl",
    "twox-hash",
    "typenum",
    "unicode-ident",
    "version_check",
    "zerocopy",
    "zerocopy-derive",
    "zlib-rs",
];

/// `Cargo.lock` lives at the workspace root, above this crate's manifest.
fn workspace_lockfile() -> PathBuf {
    let mut dir: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.is_file() {
            return candidate;
        }
        dir = dir.parent().expect("Cargo.lock above the manifest directory");
    }
}

/// Package names in `Cargo.lock`, which lists each one exactly once.
fn locked_packages() -> Vec<String> {
    let lock = std::fs::read_to_string(workspace_lockfile()).expect("read Cargo.lock");
    lock.lines()
        .filter_map(|line| line.strip_prefix("name = \""))
        .filter_map(|rest| rest.strip_suffix('"'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_dependency_set_is_the_pinned_one() {
    let mut locked: Vec<String> = locked_packages();
    locked.sort();
    locked.dedup();

    let mut expected: Vec<String> =
        OURS.iter().chain(EXTERNAL.iter()).map(|s| (*s).to_owned()).collect();
    expected.sort();

    let added: Vec<&String> = locked.iter().filter(|p| !expected.contains(p)).collect();
    let removed: Vec<&String> = expected.iter().filter(|p| !locked.contains(p)).collect();

    assert!(
        added.is_empty() && removed.is_empty(),
        "the dependency set moved.\n  \
         added:   {added:?}\n  \
         removed: {removed:?}\n\n\
         Check the licence of anything added -- the MIT story in LICENSE depends \
         on every dependency being permissive -- then update EXTERNAL in this file."
    );
}

/// The two dependencies whose absence is a design decision rather than an
/// accident, named so a reader of a failure knows which rule was broken.
#[test]
fn no_regex_or_json_schema_engine_is_linked() {
    let locked = locked_packages();

    for banned in ["regex", "regex-automata", "jsonschema", "valico", "boon"] {
        assert!(
            !locked.iter().any(|p| p == banned),
            "`{banned}` entered the dependency graph.\n\n\
             Schema validation is deliberately left to callers -- resolving a tag \
             to a schema is manifest *data*, published with the extension that \
             defines the tag, not logic this engine can carry. See \
             \"No schema validation\" in KNOWN-DIVERGENCES.md. If that decision \
             has been revisited, delete this test in the same commit."
        );
    }
}
