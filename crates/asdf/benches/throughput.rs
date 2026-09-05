//! Throughput of the paths that handle bulk data.
//!
//! Run with `cargo bench -p asdf-rs`, or one group with
//! `cargo bench -p asdf-rs -- read`.
//!
//! # What is worth measuring here
//!
//! ASDF is a YAML tree in front of binary blocks, and the two halves have
//! entirely different cost profiles. The tree is small and its cost is fixed
//! per file -- opening and parsing a reference file is tens of microseconds,
//! and no realistic workload notices. The blocks are where the bytes are, and
//! a file holding a few hundred megabytes of `float64` is ordinary in this
//! format's world.
//!
//! So these benchmarks are sized in megabytes and report bytes per second.
//! The per-file constants are measured too, but as a floor to check they stay
//! negligible rather than as something to tune.
//!
//! # The regression these exist to catch
//!
//! `read_array_of` once decoded every element into an `Element` enum and then
//! converted that to the requested type: three passes and an intermediate
//! several times the size of the data, to deliver bytes that were already
//! contiguous and correctly laid out in the mapping. It ran at 153 MB/s
//! against the reference implementation's 1806 MB/s -- twelve times slower
//! than Python.
//!
//! Nothing failed. Every test passed, the C suite passed, the corpus matched.
//! A benchmark is the only kind of test that would have noticed, which is the
//! argument for keeping these.

use asdf::{AsdfBuilder, AsdfFile, Compression};

fn main() {
    divan::main();
}

/// Four million `f64`, or 32 MB -- large enough that per-call overhead
/// disappears and small enough to run in a few seconds.
const N: usize = 4_000_000;
const BYTES: usize = N * size_of::<f64>();

fn sample() -> Vec<f64> {
    (0..N).map(|i| i as f64 * 1.5).collect()
}

/// A file in a temporary directory, deleted when the benchmark ends.
fn write_sample(compression: Compression) -> tempfile_lite::TempPath {
    let mut builder = AsdfBuilder::new().with_compression(compression);
    builder.set_array("big", &sample()).expect("set_array");
    let path = tempfile_lite::TempPath::new(&format!("bench-{compression:?}.asdf"));
    builder.write_to_path(path.as_path()).expect("write");
    path
}

// ---- Reading -------------------------------------------------------

#[divan::bench_group(name = "read")]
mod read {
    use super::*;

    /// The whole point: bytes already laid out as `f64` reach the caller
    /// without a per-element decode.
    #[divan::bench(bytes_count = BYTES)]
    fn array_f64(bencher: divan::Bencher) {
        let path = write_sample(Compression::None);
        let file = AsdfFile::open(path.as_path()).expect("open");
        bencher.bench(|| file.read_array_of::<f64>("big").expect("read").len());
    }

    /// The general path, kept for comparison: every element becomes an
    /// `Element` first. This is what the typed read used to cost.
    #[divan::bench(bytes_count = BYTES)]
    fn array_elements(bencher: divan::Bencher) {
        let path = write_sample(Compression::None);
        let file = AsdfFile::open(path.as_path()).expect("open");
        let tree = file.tree().expect("tree").expect("a tree");
        let array = tree.get("big").and_then(|v| v.as_ndarray()).expect("ndarray");
        bencher.bench(|| file.read_array(&array).expect("read").len());
    }

    /// Raw block bytes. Should be a pointer into the mapping, so this is the
    /// floor everything else is measured against.
    #[divan::bench(bytes_count = BYTES)]
    fn block_bytes(bencher: divan::Bencher) {
        let path = write_sample(Compression::None);
        let file = AsdfFile::open(path.as_path()).expect("open");
        bencher.bench(|| file.block_data(0).expect("block").len());
    }

    #[divan::bench(bytes_count = BYTES, args = [Compression::Zlib, Compression::Lz4])]
    fn compressed(bencher: divan::Bencher, compression: Compression) {
        let path = write_sample(compression);
        let file = AsdfFile::open(path.as_path()).expect("open");
        bencher.bench(|| file.read_array_of::<f64>("big").expect("read").len());
    }

    #[divan::bench(bytes_count = BYTES)]
    fn verify_checksum(bencher: divan::Bencher) {
        let path = write_sample(Compression::None);
        let file = AsdfFile::open(path.as_path()).expect("open");
        bencher.bench(|| file.verify_block(0).expect("verify"));
    }
}

// ---- Writing -------------------------------------------------------

#[divan::bench_group(name = "write")]
mod write {
    use super::*;

    /// Encoding values into the block's bytes, without assembling a file.
    #[divan::bench(bytes_count = BYTES)]
    fn encode(bencher: divan::Bencher) {
        let values = sample();
        bencher.bench(|| {
            let mut builder = AsdfBuilder::new();
            builder.set_array("big", &values).expect("set_array");
            builder
        });
    }

    /// The whole file: tree, block headers, checksums and block index.
    #[divan::bench(bytes_count = BYTES)]
    fn whole_file(bencher: divan::Bencher) {
        let values = sample();
        let mut builder = AsdfBuilder::new();
        builder.set_array("big", &values).expect("set_array");
        bencher.bench(|| builder.to_bytes().expect("to_bytes").len());
    }

    #[divan::bench(bytes_count = BYTES, args = [Compression::Zlib, Compression::Lz4])]
    fn compressed(bencher: divan::Bencher, compression: Compression) {
        let values = sample();
        let mut builder = AsdfBuilder::new().with_compression(compression);
        builder.set_array("big", &values).expect("set_array");
        bencher.bench(|| builder.to_bytes().expect("to_bytes").len());
    }
}

// ---- Per-file constants --------------------------------------------

/// These are the costs a caller pays once per file rather than per megabyte.
/// They are here to stay negligible, not to be tuned.
#[divan::bench_group(name = "per_file")]
mod per_file {
    use super::*;

    fn small_file() -> tempfile_lite::TempPath {
        let mut builder = AsdfBuilder::new();
        builder.set_str("meta/observer", "M. Curie").expect("set");
        builder.set_array("data", &(0..64u64).collect::<Vec<_>>()).expect("set");
        let path = tempfile_lite::TempPath::new("bench-small.asdf");
        builder.write_to_path(path.as_path()).expect("write");
        path
    }

    /// Memory-mapping the file and scanning its block headers.
    #[divan::bench]
    fn open_and_scan(bencher: divan::Bencher) {
        let path = small_file();
        bencher.bench(|| AsdfFile::open(path.as_path()).expect("open"));
    }

    /// Parsing the YAML tree.
    #[divan::bench]
    fn parse_tree(bencher: divan::Bencher) {
        let path = small_file();
        let file = AsdfFile::open(path.as_path()).expect("open");
        bencher.bench(|| file.tree().expect("tree").is_some());
    }
}

/// A temporary file that removes itself, so the benchmarks need no
/// dev-dependency on a tempfile crate for the one thing they want from it.
mod tempfile_lite {
    use std::path::{Path, PathBuf};

    pub struct TempPath(PathBuf);

    impl TempPath {
        pub fn new(name: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("libasdf-rs-{}-{name}", std::process::id()));
            Self(path)
        }

        pub fn as_path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}
