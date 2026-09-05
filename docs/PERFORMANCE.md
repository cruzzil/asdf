# Performance

Run the benchmarks with:

```console
$ cargo bench -p asdf-rs              # everything
$ cargo bench -p asdf-rs -- read      # one group
```

They live in `crates/asdf/benches/throughput.rs` and use
[divan](https://crates.io/crates/divan) as a dev-dependency, so nothing is
added to what a consumer builds.

## What is worth measuring

ASDF is a YAML tree in front of binary blocks, and the two halves have
entirely different cost profiles.

The tree is small and its cost is fixed per file. Opening a reference file and
parsing its tree takes about **8 µs and 10 µs** respectively — for any file,
because the tree does not grow with the data. No realistic workload notices,
so those are benchmarked as a floor to confirm they stay negligible, not as
something to tune.

The blocks are where the bytes are. A file holding hundreds of megabytes of
`float64` is ordinary in this format's world, so the block benchmarks are
sized in megabytes and report bytes per second.

## Measurements

32 MB of `float64` (4M elements), x86-64, release build, medians:

| | Throughput | |
|---|---|---|
| `block_data` (raw bytes) | 10 ns | a pointer into the mapping; nothing is copied |
| `read_array_of::<f64>` | 10.9 GB/s | bulk path |
| `read_array` (`Vec<Element>`) | 227 MB/s | general path, decodes each element |
| `read_array_of` via lz4 | 943 MB/s | decompression-bound |
| `read_array_of` via zlib | 601 MB/s | decompression-bound |
| `verify_block` (md5) | 673 MB/s | |
| `set_array` (encode) | 4.4 GB/s | |
| `to_bytes` (whole file) | 281 MB/s | |
| `to_bytes` with lz4 | 302 MB/s | 57% of raw size |
| `to_bytes` with zlib | 19 MB/s | 19% of raw size |

## Against the reference implementation

The comparison that matters, since this is a re-implementation. Same machine,
same 32 MB array, against Python `asdf` 5.3.1 with numpy 2.5.2 — measured end
to end from a cold file, which is why the read figure differs from the warm
benchmark above:

| | libasdf-rs | Python `asdf` |
|---|---|---|
| write, uncompressed | 109 ms | 138 ms |
| read array | 17.0 ms | 17.7 ms |
| write, zlib | 1.70 s | 1.50 s |
| read, zlib | 68 ms | 72 ms |

Roughly at parity, which is the right expectation: both are ultimately moving
bytes between a file and memory, and Python's array handling is numpy, which
is C.

## The regression these exist to catch

`read_array_of` used to decode every element into an `Element` enum and then
convert that to the requested type — three passes and an intermediate several
times the size of the data, to deliver bytes that were already contiguous and
correctly laid out in the mapping. It ran at **153 MB/s against Python's 1806
MB/s: twelve times slower than the reference implementation.**

Nothing failed. All 569 tests passed, upstream's C suite passed, the reference
corpus matched. A benchmark is the only kind of test that would have noticed,
which is the whole argument for keeping these.

The fix takes a bulk path when the stored elements already are the requested
type, laid out end to end. It falls back to the general path for anything
else: a different width, a compound type, explicit strides, a non-zero offset
into the block, or a mask. Those conditions are in `bulk_readable`, and each
one is a case the bulk path cannot honour.

Because a fast path that quietly returns wrong numbers is worse than a slow
one, it is pinned against the slow path rather than against expectations:
`crates/asdf/tests/bulk_read_equivalence.rs` reads **91 arrays across the 112
reference files — 42 of them big-endian** — both ways and requires them to
agree. The corpus is the input precisely because it contains what we would not
think to write, including the big-endian arrays that exercise the byte
swapping and the strided and offset arrays that must *not* take the fast path.

## Known costs, not yet addressed

- **Checksums are about 40% of write time.** `to_bytes` runs at 281 MB/s while
  md5 alone runs at 673 MB/s. Upstream has `ASDF_EMITTER_OPT_NO_BLOCK_CHECKSUM`
  for callers who do not want to pay this; the idiomatic Rust API does not
  expose an equivalent yet.
- **zlib writes at 19 MB/s**, roughly 90× slower than writing uncompressed.
  This is flate2's cost at the default level rather than anything in this
  crate — Python pays the same, at 21 MB/s — but a caller choosing zlib should
  know the write side is where it is paid. lz4 costs almost nothing to write
  (302 MB/s) for a weaker ratio.
- **The general read path is 227 MB/s** and is still what `read_array` and the
  `Vec<Element>` API use. It is inherently per-element, and worth revisiting
  only if a caller is found who needs it on large arrays.
