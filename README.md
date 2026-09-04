# libasdf-rs

A Rust implementation of [ASDF](https://www.asdf-format.org/) (Advanced
Scientific Data Format), providing two things from one engine:

- **a drop-in replacement for [libasdf](https://github.com/asdf-format/libasdf)**,
  exposing the same C ABI so existing C code recompiles and relinks unchanged;
- **an idiomatic Rust library**, with borrowed data, `Result`, iterators and no
  `unsafe`.

ASDF is a hybrid format: a YAML tree describing the data, followed by binary
blocks holding it. It is the native format of the Nancy Grace Roman Space
Telescope.

## Layout

| Crate | What it is |
|---|---|
| `asdf-core` | The engine. File layout, blocks, compression, ndarray, rendering. All the behaviour lives here. |
| `asdf-yaml` | The ASDF YAML layer: document model, parser and emitter. |
| `libasdf-rs` | The C ABI. Builds `libasdf.so`; every entry point is panic-guarded. |
| `asdf` | The idiomatic Rust API. |
| `asdf-cli` | The `asdf` command-line tool. |

`libasdf-rs` and `asdf` are both thin projections of `asdf-core`, so the two
public faces cannot drift apart in semantics.

## Using it from Rust

```rust
use asdf::{AsdfBuilder, AsdfFile};

let mut builder = AsdfBuilder::new();
builder.set_str("name", "Dennis Richie")?;
let squares: Vec<u64> = (0..100).map(|i| i * i).collect();
builder.set_array("powers/squares", &squares)?;
builder.write_to_path("out.asdf")?;

let file = AsdfFile::open("out.asdf")?;
let tree = file.tree()?.expect("a tree");
println!("{:?}", tree.get("name").and_then(|v| v.as_str()));

// Arrays read back as whatever scalar type they fit.
let values: Vec<u64> = file.read_array_of("powers/squares")?;
```

Editing an existing file goes through `edit`, which carries the tree and the
blocks over so every `source: N` still points where it did:

```rust
use asdf::{AsdfFile, Compression};

let file = AsdfFile::open("observation.asdf")?;
let mut edited = file.edit()?;
edited.set_str("meta/observer", "M. Curie")?;
edited.recompress(Compression::Zlib).write_to_path("observation.asdf")?;
```

## Using it from C

The public headers are vendored from upstream, so C source compiles against
them unchanged:

```c
#include <asdf.h>

asdf_file_t *file = asdf_open(NULL);
asdf_set_string0(file, "name", "Dennis Richie");
asdf_set_int64(file, "foo", 42);
asdf_write_to(file, "out.asdf");
asdf_close(file);
```

## Building

```console
$ cargo build --release      # builds target/release/libasdf.so
$ cargo test --workspace
```

Tests read two external corpora when they are present, and skip with a note
when they are not. Point them somewhere other than `~/code` with:

```console
$ ASDF_STANDARD_DIR=/path/to/asdf-standard LIBASDF_DIR=/path/to/libasdf cargo test
```

## How correctness is judged

Three independent oracles already exist for this format, and all three are
wired into the test suite rather than reasoned about:

- **libasdf's own C test suite** — the tests upstream wrote for its own
  implementation, compiled against the vendored headers and linked against
  the built `libasdf.so`. **498 of 501 pass**, across eleven of its
  twenty-one suites; the other ten reach into libasdf's private headers and
  cannot run against a different implementation by construction. This is the
  strongest evidence the project can produce, and every suite's pass count is
  pinned so it cannot drift.
- **The ASDF Standard's reference corpus** — 105 `.asdf` files paired with the
  YAML they should read as, across seven standard versions. Following the
  corpus README's procedure (inline every array, dereference aliases, compare
  at the value level), **all 105 match exactly**.
- **libasdf's committed command-line captures** — 17 for `asdf info`, 4 for
  `asdf events` and 3 for `asdf verify-checksums`, all reproduced byte for
  byte, ANSI styling included.
- **A C ABI conformance harness** — real C programs compiled against the
  vendored headers and linked against the built library, covering the
  `_Generic` macros, struct layouts, enum discriminants, a third-party
  extension registering before `main`, and the exported symbol namespace.
  Every symbol the headers declare is checked to exist: 376 of 376, read out
  of the preprocessed headers rather than a list kept by hand.
- **Differential tests against Python asdf** — files written here are read by
  the reference implementation and vice versa, across every compression
  method, so the two are checked against each other rather than only against
  themselves. They skip when Python asdf is not installed.

YAML output is compared at the **value level, never byte for byte** — YAML
admits many spellings of the same value, and the standard's own corpus says
as much. The binary layer is the exception and is byte-exact by
specification. See `KNOWN-DIVERGENCES.md` for the deliberate differences from
upstream, each with a test pinning it.

## Status

Feature-complete against upstream libasdf, and past it in a few places.

- Every symbol upstream's headers declare is exported and implemented.
- All seven core-schema extensions, and the extension registry third-party
  extensions register into before `main`.
- Both the read and the write path, including compression (zlib, bzip2, lz4),
  block indices, checksums and inline array storage.
- The `asdf` command-line tool: `info`, `dd`, `events` and
  `verify-checksums`, with upstream's options and output.
- Past parity, in `asdf-core` and the idiomatic API rather than the C
  surface: external array sources (exploded form), the `core/complex` tag,
  and reading inline array data back out.

Not implemented: schema validation against the ASDF Standard's JSON schemas,
which upstream libasdf does not do either.

See `CONFORMANCE.md` for the ABI baseline and the gates, and
`KNOWN-DIVERGENCES.md` for the deliberate differences from upstream.

## Licence

BSD-3-Clause, matching upstream libasdf.
