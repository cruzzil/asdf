//! Tier 6: differential testing against Python asdf.
//!
//! The two implementations are independent, so agreeing with each other is
//! much stronger evidence than either agreeing with itself. Both directions
//! are checked, because they can fail independently:
//!
//! - **we write, Python reads** — catches anything wrong in the emitter, the
//!   block writer, or checksum generation;
//! - **Python writes, we read** — catches anything wrong in the scanner, the
//!   YAML model, or element decoding.
//!
//! Python asdf is not a build dependency. The tests look for an interpreter
//! that can import it and skip with a note when there is none. Point them at
//! one with `ASDF_PYTHON=/path/to/python`, or create the default venv:
//!
//! ```console
//! $ python3 -m venv /tmp/asdf-venv && /tmp/asdf-venv/bin/pip install asdf
//! ```

use std::path::PathBuf;
use std::process::Command;

use asdf_core::compression::Compression;
use asdf_core::reader::{ChecksumStatus, Reader};
use asdf_core::writer::{PendingBlock, Writer};

/// An interpreter that can `import asdf`, if there is one.
fn python() -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(explicit) = std::env::var("ASDF_PYTHON") {
        candidates.push(explicit);
    }
    candidates.push("/tmp/asdf-venv/bin/python".to_string());
    candidates.push("python3".to_string());

    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .args(["-c", "import asdf"])
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// Run a Python snippet, returning its stdout.
fn run_python(interpreter: &str, script: &str) -> Result<String, String> {
    let out =
        Command::new(interpreter).arg("-c").arg(script).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "python failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("asdf-differential-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Build a file with our writer that exercises the tree and a block.
fn build_our_file(compression: Compression) -> Vec<u8> {
    let tree = asdf_yaml::parse_document(
        "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
         name: Dennis Richie\n\
         count: 42\n\
         negative: -7\n\
         ratio: 1.5\n\
         flag: true\n\
         nothing: null\n\
         quoted: '99'\n\
         nested:\n  inner: deep\n\
         list: [1, 2, 3]\n\
         squares: !core/ndarray-1.1.0\n  \
         source: 0\n  datatype: int64\n  byteorder: little\n  shape: [10]\n\
         ...\n",
    )
    .unwrap();

    let squares: Vec<i64> = (0..10i64).map(|i| i * i).collect();
    let mut writer = Writer::from_document(tree);
    writer.add_block(PendingBlock::compressed(
        squares.iter().flat_map(|v| v.to_le_bytes()).collect(),
        compression,
    ));
    writer.to_bytes().unwrap()
}

#[test]
fn python_reads_what_we_write() {
    let Some(interpreter) = python() else {
        eprintln!("skipping: no Python with asdf available");
        return;
    };

    let dir = scratch_dir();
    let path = dir.join("ours.asdf");
    std::fs::write(&path, build_our_file(Compression::None)).unwrap();

    let script = format!(
        r#"
import asdf, json
with asdf.open({path:?}) as af:
    out = {{
        "name": af["name"],
        "count": int(af["count"]),
        "negative": int(af["negative"]),
        "ratio": float(af["ratio"]),
        "flag": bool(af["flag"]),
        "nothing_is_none": af["nothing"] is None,
        "quoted": af["quoted"],
        "quoted_is_str": isinstance(af["quoted"], str),
        "inner": af["nested"]["inner"],
        "list": [int(x) for x in af["list"]],
        "squares": [int(x) for x in af["squares"]],
    }}
print(json.dumps(out))
"#,
        path = path.to_string_lossy()
    );

    let output = run_python(&interpreter, &script)
        .unwrap_or_else(|e| panic!("Python could not read our file:\n{e}"));
    let parsed: serde_free::Json = serde_free::parse(&output).expect("python emitted JSON");

    assert_eq!(parsed.string("name"), Some("Dennis Richie".to_string()));
    assert_eq!(parsed.number("count"), Some(42.0));
    assert_eq!(parsed.number("negative"), Some(-7.0));
    assert_eq!(parsed.number("ratio"), Some(1.5));
    assert_eq!(parsed.bool("flag"), Some(true));
    assert_eq!(parsed.bool("nothing_is_none"), Some(true));
    // The important one: a quoted numeric must arrive as a string, not an int.
    assert_eq!(
        parsed.bool("quoted_is_str"),
        Some(true),
        "a quoted '99' must read as a string in Python too"
    );
    assert_eq!(parsed.string("quoted"), Some("99".to_string()));
    assert_eq!(parsed.string("inner"), Some("deep".to_string()));
    assert_eq!(parsed.numbers("list"), Some(vec![1.0, 2.0, 3.0]));
    assert_eq!(
        parsed.numbers("squares"),
        Some((0..10).map(|i| f64::from(i * i)).collect::<Vec<_>>()),
        "the array in our binary block must decode in Python"
    );
}

#[test]
fn python_reads_our_compressed_blocks() {
    let Some(interpreter) = python() else {
        eprintln!("skipping: no Python with asdf available");
        return;
    };
    let dir = scratch_dir();

    for compression in asdf_core::compression::available() {
        let path = dir.join(format!("ours-{}.asdf", compression.name()));
        std::fs::write(&path, build_our_file(compression)).unwrap();

        let script = format!(
            r#"
import asdf, json
with asdf.open({path:?}) as af:
    print(json.dumps({{"squares": [int(x) for x in af["squares"]]}}))
"#,
            path = path.to_string_lossy()
        );

        let output = run_python(&interpreter, &script).unwrap_or_else(|e| {
            panic!("Python could not read our {} file:\n{e}", compression.name())
        });
        let parsed = serde_free::parse(&output).expect("python emitted JSON");
        assert_eq!(
            parsed.numbers("squares"),
            Some((0..10).map(|i| f64::from(i * i)).collect::<Vec<_>>()),
            "{} block did not decode in Python",
            compression.name()
        );
    }
}

/// The Python script that writes a file for us to read.
fn writer_script(path: &str, compression: &str) -> String {
    let all = if compression == "none" { "None".to_string() } else { format!("{compression:?}") };
    format!(
        r#"
import asdf, numpy as np
tree = {{
    "name": "written by python",
    "count": 4242,
    "negative": -12345,
    "ratio": 2.25,
    "flag": False,
    "nothing": None,
    "nested": {{"inner": "deep"}},
    "list": [1, 2, 3],
    "int8s": np.arange(-4, 4, dtype=np.int8),
    "uint16s": np.arange(0, 8, dtype=np.uint16),
    "floats": np.linspace(0.0, 1.0, 8, dtype=np.float64),
    "float32s": np.linspace(0.0, 1.0, 8, dtype=np.float32),
    "big_endian": np.arange(0, 8, dtype=">i4"),
    "matrix": np.arange(12, dtype=np.int64).reshape(3, 4),
}}
af = asdf.AsdfFile(tree)
af.write_to({path:?}, all_array_compression={all})
"#
    )
}

#[test]
fn we_read_what_python_writes() {
    let Some(interpreter) = python() else {
        eprintln!("skipping: no Python with asdf available");
        return;
    };

    let dir = scratch_dir();
    let path = dir.join("theirs.asdf");
    run_python(&interpreter, &writer_script(&path.to_string_lossy(), "none"))
        .unwrap_or_else(|e| panic!("Python could not write a file:\n{e}"));

    let reader = Reader::open(&path).expect("our reader should open a Python-written file");
    let tree = reader.tree().unwrap().expect("a tree");
    let root = tree.root().unwrap();

    let text = |key: &str| {
        tree.mapping_get(root, key).and_then(|id| tree.resolved(id).as_str().map(str::to_string))
    };
    assert_eq!(text("name").as_deref(), Some("written by python"));
    assert_eq!(text("count").as_deref(), Some("4242"));
    assert_eq!(text("negative").as_deref(), Some("-12345"));
    assert_eq!(text("ratio").as_deref(), Some("2.25"));

    // Every array must decode to the values Python put there.
    let inlined = reader.tree_inlined().unwrap().expect("a tree");
    let (doc, not_inlined) = inlined;
    assert!(not_inlined.is_empty(), "arrays left un-inlined: {not_inlined:?}");
    let root = doc.root().unwrap();

    let numbers = |key: &str| -> Vec<f64> {
        let node = doc.mapping_get(root, key).unwrap_or_else(|| panic!("no {key}"));
        let data = doc.mapping_get(node, "data").unwrap_or_else(|| panic!("{key} was not inlined"));
        doc.sequence_items(data)
            .unwrap_or(&[])
            .iter()
            .map(|id| doc.resolved(*id).as_str().unwrap().parse::<f64>().unwrap())
            .collect()
    };

    assert_eq!(numbers("int8s"), (-4..4).map(f64::from).collect::<Vec<_>>());
    assert_eq!(numbers("uint16s"), (0..8).map(f64::from).collect::<Vec<_>>());

    // A big-endian array is the case a byte-order bug would show up in.
    assert_eq!(
        numbers("big_endian"),
        (0..8).map(f64::from).collect::<Vec<_>>(),
        "big-endian data decoded incorrectly"
    );

    let floats = numbers("floats");
    assert_eq!(floats.len(), 8);
    assert!((floats[0] - 0.0).abs() < 1e-12);
    assert!((floats[7] - 1.0).abs() < 1e-12);

    let float32s = numbers("float32s");
    assert_eq!(float32s.len(), 8);
    assert!((float32s[7] - 1.0).abs() < 1e-6);

    // A two-dimensional array must come back nested, not flattened.
    let matrix = doc.mapping_get(root, "matrix").unwrap();
    let data = doc.mapping_get(matrix, "data").unwrap();
    assert_eq!(doc.container_len(data), Some(3), "matrix should have 3 rows");
    let first_row = doc.sequence_get(data, 0).unwrap();
    assert_eq!(doc.container_len(first_row), Some(4), "each row has 4 columns");
}

#[test]
fn we_read_python_compressed_blocks() {
    let Some(interpreter) = python() else {
        eprintln!("skipping: no Python with asdf available");
        return;
    };
    let dir = scratch_dir();

    // The names Python asdf uses; ours must match, since they go in the
    // four-byte header field.
    for name in ["zlib", "bzp2", "lz4"] {
        let path = dir.join(format!("theirs-{name}.asdf"));
        if run_python(&interpreter, &writer_script(&path.to_string_lossy(), name)).is_err() {
            eprintln!("  skipping {name}: Python cannot write it");
            continue;
        }

        let reader = Reader::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(reader.block_count() > 0, "{name}: no blocks");

        for index in 0..reader.block_count() {
            let compression = reader.block_compression(index).unwrap_or_else(|e| {
                panic!("{name}: block {index} has a compression we do not know: {e}")
            });
            assert_eq!(compression.name(), name, "{name}: block {index}");

            // The real test: our decompressor must reproduce what theirs
            // compressed. The lz4 framing in particular is a private
            // convention shared between the implementations.
            let data = reader
                .block_data(index)
                .unwrap_or_else(|e| panic!("{name}: block {index} would not decompress: {e}"));
            let header = &reader.block(index).unwrap().header;
            assert_eq!(
                data.len() as u64,
                header.data_size,
                "{name}: block {index} inflated to the wrong size"
            );
        }
        eprintln!("  {name}: {} blocks decoded", reader.block_count());
    }
}

/// Establish which form the installed Python asdf checksums, rather than
/// assuming.
///
/// libasdf works around asdf#2015 -- versions that checksum a compressed
/// block's *uncompressed* data -- by treating any `asdf` at major version 5
/// or below as affected. That test turns out to be too broad: 5.3.1 records
/// the digest of the stored bytes, i.e. correctly. The workaround is still
/// sound, because it only *retries* against the decompressed bytes when the
/// stored ones do not match, so a corrected file verifies on the first
/// attempt and never reaches it.
///
/// This test asserts the property that actually matters -- the block
/// verifies -- and reports which form matched, so a future change on either
/// side is visible rather than silent.
#[test]
fn python_written_checksums_verify() {
    let Some(interpreter) = python() else {
        eprintln!("skipping: no Python with asdf available");
        return;
    };

    let version =
        run_python(&interpreter, "import asdf; print(asdf.__version__)").unwrap_or_default();
    let version = version.trim().to_string();
    eprintln!("  testing against Python asdf {version}");

    let dir = scratch_dir();
    let path = dir.join("checksum.asdf");
    run_python(&interpreter, &writer_script(&path.to_string_lossy(), "zlib"))
        .unwrap_or_else(|e| panic!("Python could not write a compressed file:\n{e}"));

    let reader = Reader::open(&path).unwrap();
    // The version test libasdf uses, reported for context.
    eprintln!(
        "  libasdf's version test calls this writer {}",
        if reader.has_python_checksum_bug() { "affected" } else { "unaffected" }
    );

    let mut compressed = 0;
    let mut stored_form = 0;
    let mut uncompressed_form = 0;

    for index in 0..reader.block_count() {
        if reader.block_compression(index).unwrap_or(Compression::None) == Compression::None {
            continue;
        }
        compressed += 1;

        // What actually matters: the block verifies.
        let (status, _) = reader.verify_block_checksum(index).unwrap();
        assert_eq!(status, ChecksumStatus::Valid, "block {index} did not verify");

        // And which form the digest covers, so a change on either side shows.
        let recorded = reader.block(index).unwrap().header.checksum;
        if md5_of(reader.block_raw(index).unwrap()) == recorded {
            stored_form += 1;
        } else if md5_of(&reader.block_data(index).unwrap()) == recorded {
            uncompressed_form += 1;
        } else {
            panic!("block {index}: neither form matched the recorded digest");
        }
    }

    assert!(compressed > 0, "the file had no compressed blocks to check");
    eprintln!(
        "  asdf {version}: {stored_form} block(s) checksum the stored bytes \
         (correct), {uncompressed_form} the uncompressed bytes (asdf#2015)"
    );
}

fn md5_of(data: &[u8]) -> [u8; 16] {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// A file we write, read by Python, written back by Python, and read by us
/// again — so a difference anywhere in the loop shows up.
#[test]
fn a_file_survives_a_round_trip_through_python() {
    let Some(interpreter) = python() else {
        eprintln!("skipping: no Python with asdf available");
        return;
    };

    let dir = scratch_dir();
    let ours = dir.join("loop-ours.asdf");
    let theirs = dir.join("loop-theirs.asdf");
    std::fs::write(&ours, build_our_file(Compression::None)).unwrap();

    let script = format!(
        r#"
import asdf
with asdf.open({input:?}) as af:
    af.write_to({output:?})
"#,
        input = ours.to_string_lossy(),
        output = theirs.to_string_lossy()
    );
    run_python(&interpreter, &script)
        .unwrap_or_else(|e| panic!("Python could not rewrite our file:\n{e}"));

    let reader = Reader::open(&theirs).unwrap();
    let (doc, not_inlined) = reader.tree_inlined().unwrap().expect("a tree");
    assert!(not_inlined.is_empty());
    let root = doc.root().unwrap();

    let text = |key: &str| {
        doc.mapping_get(root, key).and_then(|id| doc.resolved(id).as_str().map(str::to_string))
    };
    assert_eq!(text("name").as_deref(), Some("Dennis Richie"));
    assert_eq!(text("count").as_deref(), Some("42"));

    // The array must survive both writers.
    let squares = doc.mapping_get(root, "squares").unwrap();
    let data = doc.mapping_get(squares, "data").unwrap();
    let values: Vec<i64> = doc
        .sequence_items(data)
        .unwrap()
        .iter()
        .map(|id| doc.resolved(*id).as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(values, (0..10i64).map(|i| i * i).collect::<Vec<_>>());
}

/// A very small JSON reader, so these tests need no serde dependency.
mod serde_free {
    /// A parsed top-level JSON object, kept as raw text per key.
    pub struct Json {
        fields: std::collections::HashMap<String, String>,
    }

    impl Json {
        pub fn string(&self, key: &str) -> Option<String> {
            let raw = self.fields.get(key)?;
            let trimmed = raw.trim();
            let inner = trimmed.strip_prefix('"')?.strip_suffix('"')?;
            Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
        }

        pub fn number(&self, key: &str) -> Option<f64> {
            self.fields.get(key)?.trim().parse().ok()
        }

        pub fn bool(&self, key: &str) -> Option<bool> {
            match self.fields.get(key)?.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }
        }

        pub fn numbers(&self, key: &str) -> Option<Vec<f64>> {
            let raw = self.fields.get(key)?.trim();
            let inner = raw.strip_prefix('[')?.strip_suffix(']')?;
            if inner.trim().is_empty() {
                return Some(Vec::new());
            }
            inner.split(',').map(|part| part.trim().parse().ok()).collect()
        }
    }

    /// Split a flat JSON object into its top-level key/value pairs.
    ///
    /// Only handles what the test scripts emit: one object whose values are
    /// strings, numbers, booleans, or arrays of numbers.
    pub fn parse(text: &str) -> Option<Json> {
        let text = text.trim();
        let body = text.strip_prefix('{')?.strip_suffix('}')?;

        let mut fields = std::collections::HashMap::new();
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        let mut current = String::new();
        let mut parts = Vec::new();

        for ch in body.chars() {
            if escaped {
                current.push(ch);
                escaped = false;
                continue;
            }
            match ch {
                '\\' if in_string => {
                    current.push(ch);
                    escaped = true;
                }
                '"' => {
                    in_string = !in_string;
                    current.push(ch);
                }
                '[' | '{' if !in_string => {
                    depth += 1;
                    current.push(ch);
                }
                ']' | '}' if !in_string => {
                    depth -= 1;
                    current.push(ch);
                }
                ',' if !in_string && depth == 0 => {
                    parts.push(std::mem::take(&mut current));
                }
                _ => current.push(ch),
            }
        }
        if !current.trim().is_empty() {
            parts.push(current);
        }

        for part in parts {
            let (key, value) = split_pair(&part)?;
            fields.insert(key, value);
        }
        Some(Json { fields })
    }

    /// Split `"key": value` at the colon that follows the key's closing quote.
    fn split_pair(part: &str) -> Option<(String, String)> {
        let part = part.trim();
        let rest = part.strip_prefix('"')?;
        let close = rest.find('"')?;
        let key = rest[..close].to_string();
        let after = rest[close + 1..].trim_start().strip_prefix(':')?;
        Some((key, after.trim().to_string()))
    }
}

/// Our `repr` for floats and complex numbers must be CPython's, exactly.
///
/// The `core/complex` schema leaves the spelling open, so the corpus's
/// spelling is whatever Python produced. A unit test can only pin the values
/// someone thought to write down; this asks the interpreter itself, across
/// the values where the shortest-repr and fixed/exponent rules actually bite.
#[test]
fn float_and_complex_spellings_match_python() {
    use asdf_core::core::pyrepr::{repr_complex, repr_f64};

    let Some(interpreter) = python() else {
        eprintln!("skipping: no Python with asdf available");
        return;
    };

    // Chosen for the boundaries: the fixed/exponent switch at both ends, the
    // signed zeros, the subnormals, and the extremes.
    let floats: Vec<f64> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        0.1,
        1.0 / 3.0,
        1e-4,
        1e-5,
        9.999999999999999e-5,
        1e15,
        1e16,
        1e17,
        1.2345678901234567e16,
        f64::MAX,
        -f64::MAX,
        f64::MIN_POSITIVE,
        5e-324,
        f64::EPSILON,
        123456789012345678.0,
        -2.5e-300,
        6.02214076e23,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];

    // Passed as hex bit patterns so the transfer itself cannot round.
    let bits: Vec<String> = floats.iter().map(|v| format!("{:016x}", v.to_bits())).collect();
    let script = format!(
        r#"
import struct
bits = {bits:?}
for b in bits:
    v = struct.unpack('>d', bytes.fromhex(b))[0]
    print(repr(v))
    print(repr(complex(0.0, v)))
    print(repr(complex(v, v)))
    print(repr(complex(-0.0, v)))
"#
    );
    let out = run_python(&interpreter, &script).expect("python repr");
    let mut lines = out.lines();

    let mut checked = 0;
    for v in &floats {
        for (ours, label) in [
            (repr_f64(*v), "float"),
            (repr_complex(0.0, *v), "complex(0, v)"),
            (repr_complex(*v, *v), "complex(v, v)"),
            (repr_complex(-0.0, *v), "complex(-0, v)"),
        ] {
            let theirs = lines.next().expect("a line per value");
            assert_eq!(ours, theirs, "{label} for {v:?} (bits {:016x})", v.to_bits());
            checked += 1;
        }
    }
    assert_eq!(lines.next(), None, "python produced more lines than we consumed");
    eprintln!("{checked} float and complex spellings match CPython exactly");
}
