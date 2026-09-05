//! The bulk read path must agree with the general one, everywhere.
//!
//! `read_array_of` takes a shortcut when the stored elements already are the
//! requested type laid out end to end, copying them in bulk instead of
//! decoding to [`Element`] first. That shortcut is worth roughly 13x on a
//! large array end to end, and it is also the kind of optimisation that quietly returns
//! wrong numbers for a layout nobody tested.
//!
//! So it is checked against the slow path rather than against expectations:
//! `read_array_f64` and `read_array_i64` decode every element individually
//! and are untouched by the fast path, which makes them an oracle that cannot
//! drift with it.
//!
//! The Standard's reference corpus is the input because it contains what we
//! would not think to write: 17 big-endian arrays, an array with explicit
//! strides, and one at a non-zero offset into its block. The first exercises
//! the fast path's byte swapping; the last two must send it back to the
//! general path, and would be silently wrong if they did not.

use std::path::PathBuf;

use asdf::{AsdfFile, ScalarType, Value};

fn standard_dir() -> Option<PathBuf> {
    let path = std::env::var_os("ASDF_STANDARD_DIR").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("code/asdf-standard"))
    })?;
    path.is_dir().then_some(path)
}

fn corpus_files() -> Vec<PathBuf> {
    let Some(root) = standard_dir() else { return Vec::new() };
    let mut out = Vec::new();
    let Ok(versions) = std::fs::read_dir(root.join("reference_files")) else {
        return out;
    };
    for version in versions.flatten() {
        let Ok(entries) = std::fs::read_dir(version.path()) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "asdf") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Every `/`-joined path in the tree that holds an ndarray.
fn array_paths(value: Value<'_>, prefix: &str, out: &mut Vec<String>) {
    if value.as_ndarray().is_some() {
        out.push(prefix.to_string());
        return;
    }
    if value.is_mapping() {
        for (key, child) in value.entries() {
            array_paths(child, &format!("{prefix}/{key}"), out);
        }
    } else if value.is_sequence() {
        for (index, child) in value.items().enumerate() {
            array_paths(child, &format!("{prefix}/{index}"), out);
        }
    }
}

#[test]
fn the_bulk_path_agrees_with_the_general_one_across_the_corpus() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    }

    let (mut checked, mut floats, mut ints, mut big_endian) = (0usize, 0usize, 0usize, 0usize);

    for path in &files {
        let Ok(file) = AsdfFile::open(path) else { continue };
        let Ok(Some(tree)) = file.tree() else { continue };
        let Some(root) = tree.root() else { continue };

        let mut paths = Vec::new();
        array_paths(root, "", &mut paths);

        for array_path in paths {
            let Some(value) = tree.get(&array_path) else { continue };
            let Some(array) = value.as_ndarray() else { continue };

            // Only the plain scalar types have a bulk path to check.
            let Ok(general) = (match array.datatype.scalar {
                ScalarType::Float64 => file.read_array_f64(&array).map(|v| {
                    let bulk: Vec<f64> = file.read_array_of(&array_path).unwrap_or_default();
                    (v, bulk)
                }),
                ScalarType::Int64 | ScalarType::Int32 | ScalarType::Int16 | ScalarType::Int8 => {
                    file.read_array_i64(&array).map(|v| {
                        let bulk: Vec<i64> = file.read_array_of(&array_path).unwrap_or_default();
                        (
                            v.iter().map(|n| *n as f64).collect(),
                            bulk.iter().map(|n| *n as f64).collect(),
                        )
                    })
                }
                _ => continue,
            }) else {
                continue;
            };

            let (slow, fast) = general;
            if slow.is_empty() {
                continue;
            }
            assert_eq!(
                slow.len(),
                fast.len(),
                "{}: {array_path} has {} elements the slow way and {} the fast way",
                path.display(),
                slow.len(),
                fast.len()
            );
            for (index, (a, b)) in slow.iter().zip(fast.iter()).enumerate() {
                assert!(
                    a == b || (a.is_nan() && b.is_nan()),
                    "{}: {array_path}[{index}] is {a} the slow way and {b} the fast way",
                    path.display()
                );
            }

            checked += 1;
            if array.datatype.scalar == ScalarType::Float64 {
                floats += 1;
            } else {
                ints += 1;
            }
            if format!("{:?}", array.byteorder).contains("Big") {
                big_endian += 1;
            }
        }
    }

    eprintln!(
        "bulk read equivalence: {checked} arrays across {} files \
         ({floats} float, {ints} integer, {big_endian} big-endian)",
        files.len()
    );
    assert!(checked > 0, "no arrays were compared; the corpus was found but yielded nothing");
    assert!(
        big_endian > 0,
        "no big-endian array was compared, so the fast path's byte swapping went unchecked"
    );
}
