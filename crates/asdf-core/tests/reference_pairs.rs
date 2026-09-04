//! Tier 1: compare each reference `.asdf` file's tree against its paired
//! `.yaml` file.
//!
//! The corpus README sets out the procedure: load the `.asdf`, inline every
//! `core/ndarray`, resolve JSON Pointer references, dereference aliases, then
//! compare at the YAML-value level -- explicitly not byte-for-byte.
//!
//! Alias dereferencing and value-level comparison are done. Inlining is not:
//! it needs block reading and datatype decoding, which land in phase 4. So
//! the test asserts the *shape* of the remaining gap instead of ignoring it:
//! every difference must be an ndarray's `source` key standing in for the
//! `data` key it will become. Anything else is a real failure.
//!
//! When phase 4 lands the gap predicates below go away and this becomes a
//! plain equality check.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asdf_core::layout::scan;
use asdf_core::reader::Reader;
use asdf_yaml::compare::{CompareOptions, Difference, Side};
use asdf_yaml::{Document, parse_document};

fn standard_dir() -> Option<PathBuf> {
    let path = std::env::var_os("ASDF_STANDARD_DIR").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("code/asdf-standard"))
    })?;
    path.is_dir().then_some(path)
}

/// Pairs of `<name>.asdf` and `<name>.yaml` under the reference corpus.
fn reference_pairs(refs: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let Ok(versions) = std::fs::read_dir(refs) else { return out };
    for version in versions.flatten() {
        if !version.path().is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(version.path()) else { continue };
        for entry in entries.flatten() {
            let asdf = entry.path();
            if asdf.extension().is_none_or(|e| e != "asdf") {
                continue;
            }
            let yaml = asdf.with_extension("yaml");
            if yaml.is_file() {
                out.push((asdf, yaml));
            }
        }
    }
    out.sort();
    out
}

/// Read a file's tree as a document, with block-backed arrays inlined.
///
/// This is the transformation the corpus README prescribes.
fn load_inlined(path: &Path) -> Option<(Document, Vec<String>)> {
    let reader = Reader::open(path).ok()?;
    reader.tree_inlined().ok()?
}

/// Read a file's tree without transforming it.
fn load_tree(path: &Path) -> Option<Document> {
    let buf = std::fs::read(path).ok()?;
    let layout = scan(&buf).ok()?;
    let text = layout.tree_str(&buf)?;
    parse_document(text).ok()
}

/// Is this difference the known ndarray inlining gap?
///
/// In a `.asdf` file an array is `{source: 0, datatype: ..., shape: [...]}`;
/// in the paired `.yaml` the same array is `{data: [...], ...}`. Until
/// inlining lands, that shows up as `source` present only on the left and
/// `data` present only on the right.
fn is_expected_ndarray_gap(d: &Difference) -> bool {
    match d {
        Difference::MissingKey { key, present_in, .. } => {
            (key == "source" && *present_in == Side::Left)
                || (key == "data" && *present_in == Side::Right)
                // A streamed array's shape carries '*' in the file and a
                // concrete length in the expected output.
                || (key == "shape" && *present_in == Side::Right)
        }
        _ => false,
    }
}

/// Differences that are the inlining gap showing up as a value mismatch
/// rather than a missing key, e.g. a `shape` of `['*']` versus a number.
fn is_expected_shape_gap(d: &Difference) -> bool {
    matches!(d, Difference::ValueMismatch { path, left, .. }
        if path.contains("/shape") && left.contains('*'))
}

#[test]
fn reference_trees_match_their_expected_yaml() {
    let Some(root) = standard_dir() else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let refs = root.join("reference_files");
    let pairs = reference_pairs(&refs);
    assert!(!pairs.is_empty(), "no .asdf/.yaml pairs found under {}", refs.display());

    // `byteorder` and `offset` describe how bytes sit in a block, so they
    // vanish once the data is inline. Ignore them until inlining lands.
    let block_only_keys = ["byteorder", "offset", "strides"];

    let options = CompareOptions {
        // The .yaml is written by a different implementation, so key order
        // routinely differs and carries no meaning.
        ignore_key_order: true,
        compare_tags: true,
        // Float text is written differently by the two writers; compare the
        // numbers.
        float_tolerance: Some(1e-12),
        max_differences: 200,
        ..Default::default()
    };

    let mut unexplained: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut compared = 0;
    let mut equal = 0;

    for (asdf_path, yaml_path) in &pairs {
        let name = asdf_path.strip_prefix(&refs).unwrap_or(asdf_path).display().to_string();

        let (Some(left), Some(right)) = (load_tree(asdf_path), load_tree(yaml_path)) else {
            unexplained.entry(name).or_default().push("failed to load".into());
            continue;
        };
        compared += 1;

        let result = asdf_yaml::compare(&left, &right, options);
        if result.is_equal() {
            equal += 1;
            continue;
        }

        let leftovers: Vec<String> = result
            .differences
            .iter()
            .filter(|d| {
                if is_expected_ndarray_gap(d) || is_expected_shape_gap(d) {
                    return false;
                }
                if let Difference::MissingKey { key, .. } = d
                    && block_only_keys.contains(&key.as_str())
                {
                    return false;
                }
                true
            })
            .map(|d| d.to_string())
            .collect();

        if !leftovers.is_empty() {
            unexplained.entry(name).or_default().extend(leftovers);
        }
    }

    eprintln!(
        "compared {compared} reference pairs; {equal} already identical, \
         {} with only the known ndarray inlining gap",
        compared - equal - unexplained.len()
    );

    if !unexplained.is_empty() {
        let mut report = String::new();
        for (file, diffs) in unexplained.iter().take(10) {
            report.push_str(&format!("\n{file}:\n"));
            for d in diffs.iter().take(6) {
                report.push_str(&format!("    {d}\n"));
            }
        }
        panic!(
            "{} reference pair(s) differ beyond the known ndarray gap:{report}",
            unexplained.len()
        );
    }
}

/// Tier 1, in full: with block-backed arrays inlined, each reference file's
/// tree must equal its expected `.yaml` at the value level.
///
/// This is the corpus README's procedure carried out end to end, and it
/// exercises nearly the whole read path at once: layout scanning, the tree
/// extent search, the YAML document model, tags, anchors and aliases, block
/// location, all three decompressors, datatype parsing, byte-order handling,
/// strides, and element decoding -- judged against files written by a
/// different implementation.
#[test]
fn inlined_reference_trees_equal_their_expected_yaml() {
    let Some(root) = standard_dir() else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let refs = root.join("reference_files");
    let pairs = reference_pairs(&refs);
    assert!(!pairs.is_empty());

    // Features not yet implemented, with the reason. Each would be a real
    // gap, not a tolerated difference -- and the list is now empty: every
    // file in the corpus round-trips. Kept so a corpus update that adds a
    // feature we lack has somewhere to record it rather than being silently
    // dropped from the comparison.
    let unsupported: &[(&str, &str)] = &[];

    let options = CompareOptions {
        ignore_key_order: true,
        compare_tags: true,
        float_tolerance: Some(1e-12),
        max_differences: 20,
        ..Default::default()
    };

    let mut matched = 0;
    let mut skipped = 0;
    let mut failures: Vec<String> = Vec::new();

    for (asdf_path, yaml_path) in &pairs {
        let name = asdf_path.file_name().unwrap().to_string_lossy().into_owned();
        let rel = asdf_path.strip_prefix(&refs).unwrap_or(asdf_path).display().to_string();

        if unsupported.iter().any(|(n, _)| *n == name) {
            skipped += 1;
            continue;
        }

        let (Some((left, not_inlined)), Some(right)) =
            (load_inlined(asdf_path), load_tree(yaml_path))
        else {
            failures.push(format!("{rel}: failed to load"));
            continue;
        };
        if !not_inlined.is_empty() {
            failures.push(format!("{rel}: arrays left un-inlined: {not_inlined:?}"));
            continue;
        }

        let result = asdf_yaml::compare(&left, &right, options);
        if result.is_equal() {
            matched += 1;
        } else {
            let detail: Vec<String> =
                result.differences.iter().take(4).map(|d| d.to_string()).collect();
            failures.push(format!("{rel}:\n      {}", detail.join("\n      ")));
        }
    }

    eprintln!(
        "{matched} of {} reference pairs match exactly after inlining \
         ({skipped} skipped for unimplemented features)",
        pairs.len()
    );

    assert!(
        failures.is_empty(),
        "{} pair(s) differ:\n    {}",
        failures.len(),
        failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n    ")
    );
    assert!(matched > 60, "expected most pairs to match, got {matched}");
}
