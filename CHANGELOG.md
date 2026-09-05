# Changelog

Notable changes to this project. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it
reaches 1.0.

Two version numbers matter here and they are not the same thing:

- the **crate version** below, which is ours;
- the **libasdf commit** whose ABI we implement, recorded in
  [`SYNC_COMMIT.md`](SYNC_COMMIT.md). A change to that is an ABI event and is
  always called out.

## [0.1.1] - 2026-09-05

### Fixed

- **`libasdf-rs` could not be linked on Linux aarch64.** The version script
  the build emitted to export the C shim's symbols is a second one, and GNU ld
  rejects that: "anonymous version tag cannot be combined with other version
  tags". The public names are now naked tail-call trampolines in Rust, so
  rustc exports them and no version script is needed.
- **Four exported symbols were missing on macOS** -- `asdf_file_log`,
  `asdf_file_error_common`, `asdf_value_error_common` and
  `asdf_ndarray_read_float16_at`. Mach-O has no version script, so the
  workaround above never applied there and a C caller using `ASDF_LOG` or
  `ASDF_ERROR` failed to link. Same fix.
- Eight symbols carried an `@@LIBASDF` version tag. Upstream's are
  unversioned, so a program built against one library and run against the
  other met a version mismatch that need not exist.
- `strerror_r` is POSIX and absent from the Windows CRT; it is now behind
  `cfg(unix)`.
- The pre-`main` extension-registry constructor was spelled only for
  GCC/Clang; MSVC's `.CRT$XCU` form is now there too.
- `asdf-core` reads a file whole rather than mapping it under `cfg(miri)`, so
  dependants can run Miri.

## [Unreleased]

Nothing released yet. Everything below is the initial development series and
is listed here so the first release notes are not written from scratch.

### Added

- **A C ABI drop-in for libasdf.** All 376 symbols the upstream headers
  declare are exported and implemented, checked against the preprocessed
  headers rather than a hand-kept list. Struct layouts, enum discriminants and
  the `_Generic` macros are verified from real C programs compiled against the
  vendored headers.
- **`asdf-rs`, an idiomatic Rust API** — borrowed data, `Result`, iterators,
  no `unsafe`. Reading, writing, editing in place, typed array access, and
  accessors for the core schema types.
- **`asdf-core`, the engine** both faces are projections of: file layout,
  blocks, compression (zlib, bzip2, lz4), block indices, checksums, ndarray
  decoding, the info renderer and the event stream.
- **`asdf-yaml`**, the ASDF YAML layer: document model, parser over
  `saphyr-parser`, and an emitter that keeps the anchors, aliases and
  directives ASDF mandates.
- **The `asdf` command-line tool**: `info`, `dd`, `events` and
  `verify-checksums`, matching upstream's options and output byte for byte.
- **Beyond upstream parity**, in `asdf-core` and the Rust API rather than the
  C surface: external array sources (exploded form), the `core/complex` tag,
  and reading inline array data back out.

### Testing

- Upstream libasdf's own C test suite, compiled against the vendored headers
  and linked against our `libasdf.so`: **498 of 501 pass** across eleven
  suites, with every suite's count pinned in both directions. The remaining
  three compare emitted YAML against fixtures naming libasdf as the writing
  library, which no other implementation can match.
- The ASDF Standard's reference corpus: **105 of 105** files match.
- Differential tests against the Python `asdf` implementation, both
  directions, across every compression method.
- Robustness tests over truncation at every length, single-byte corruption,
  corrupt block headers and indices, and absurd declared sizes.
- Miri over the whole FFI layer, in CI.

### Fixed

- `asdf_ndarray_data` returned a buffer aligned to 1, which every C caller
  casts to its element type before dereferencing — undefined behaviour, and a
  bus error on a strict-alignment target. Found by Miri; the buffer is now
  16-byte aligned as `malloc` would give.
- `asdf_tree_info_t.buf` pointed at an allocation invalidated by moving its
  owning `CString`. Also found by Miri.

### Changed

- The idiomatic crate is published as **`asdf-rs`**, since `asdf` on crates.io
  is taken by an unrelated 2017 project. The library target keeps the plain
  name, so dependants still write `use asdf::...`.
- Relicensed to **MIT**. The vendored upstream headers stay BSD-3-Clause; see
  [`README.md`](README.md#licence).

### Not implemented

- Schema validation against the ASDF Standard's JSON schemas, which upstream
  libasdf does not do either. The reasoning is in
  [`KNOWN-DIVERGENCES.md`](KNOWN-DIVERGENCES.md).

[Unreleased]: https://github.com/cruzzil/asdf/commits/main
