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
builder.set_array_u64("powers/squares", &squares)?;
builder.write_to_path("out.asdf")?;

let file = AsdfFile::open("out.asdf")?;
let tree = file.tree()?.expect("a tree");
let array = tree.get("powers/squares").unwrap().as_ndarray().unwrap();
let values = file.read_array_i64(&array)?;
# Ok::<(), asdf::Error>(())
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

- **The ASDF Standard's reference corpus** — 105 `.asdf` files paired with the
  YAML they should read as, across seven standard versions. Following the
  corpus README's procedure (inline every array, dereference aliases, compare
  at the value level), 91 pairs currently match exactly.
- **libasdf's own fixtures and expected outputs** — including 17 committed
  `asdf info` captures, which this implementation reproduces byte for byte.
- **A C ABI conformance harness** — real C programs compiled against the
  vendored headers and linked against the built library, covering the
  `_Generic` macros, struct layouts, enum discriminants, a third-party
  extension registering before `main`, and the exported symbol namespace.
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

The read path and the write path both work end to end. See `CONFORMANCE.md`
for the ABI baseline and gates.

Not yet implemented: the remaining core-schema extensions
(`meta`, `history_entry`, `datatype`, `time`), external array sources
(exploded form), and the `core/complex` tag.

## Licence

BSD-3-Clause, matching upstream libasdf.
