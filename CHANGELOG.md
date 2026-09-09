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

## [Unreleased]

## [0.1.4] - 2026-09-09

`libasdf-rs` only. The four other crates are unchanged.

Prompted by a question that turned out to have a sharp answer: can a real
third-party extension use this library? [libasdf-gwcs] now builds against it
and passes all 35 of its tests -- reading, writing, and evaluating a Roman L2
WCS against both AST and Python gwcs -- with no valgrind errors and nothing
leaked, matching upstream libasdf exactly. Getting there took three fixes.

### Fixed

- **`asdf_get_required_property` and `asdf_get_optional_property` returned a
  freed handle for a mapping or a sequence.** Both destroyed the value they
  looked up before returning, which is right for a scalar -- the C type is
  copied out -- but wrong for a container, where what lands in `*out` *is*
  that value. Callers got `ASDF_VALUE_OK` and a dangling pointer. Ownership
  now passes to the caller, as `value.h` says. This is the first thing
  libasdf-gwcs does on every transform it reads, so it segfaulted immediately.
- **The insertion entry points leaked the handle they were given.**
  `asdf_mapping_set`, `asdf_sequence_append` and their four container variants
  are documented to consume the value on success; they linked the node into
  the document but never released the handle box that named it.
- **`asdf_value_of_ndarray` stranded the array's internal state.**
  `ndarray.h` has it transfer ownership of the data to the file, and callers
  build the `asdf_ndarray_t` as a stack literal -- upstream's own write
  example does -- so they have nowhere to call `asdf_ndarray_deinit` from.
  About 1.2 KB went missing per array written.

Together these were 23 KB leaked across a single run of libasdf-gwcs's suite,
against upstream's zero.

### Changed

- **Synced to libasdf `cff7ab0` (0.1.0)**, up from `56d24aa` (0.1.0rc2). The
  headers carry the two fixes for [libasdf#251]: `asdf_value_find_ex` takes an
  `asdf_depth_t` (`int64_t`) where it took a POSIX-only `ssize_t`, and the
  option-flag enums shift `1ULL` rather than `1UL`, which overflowed on LLP64.
  No struct layout, enum discriminant or exported symbol moved; all 376
  exports and 58 struct layouts still agree, and upstream's C suite still
  passes 498 of 501.
- **The C ABI crate is built and tested on Windows again**, x64 and arm64,
  which [libasdf#251] had blocked. One gratuitous include remained --
  `asdf/core/time.h` pulls in `<sys/time.h>` for a `struct timespec` that
  `<time.h>` already provides -- and `build.rs` answers it with a generated
  shim rather than editing a vendored header.
- The README explains how to build a third-party extension against this
  library, including the `pkg-config` prefix cargo does not produce.

[libasdf-gwcs]: https://github.com/asdf-format/libasdf-gwcs
[libasdf#251]: https://github.com/asdf-format/libasdf/issues/251

## [0.1.3] - 2026-09-07

`asdf-cli` is unchanged and stays at 0.1.2.

### Changed

- The five crates now carry **independent versions** rather than a shared one,
  so a release bumps only what changed. The release procedure is in
  [`docs/DEVELOPING.md`](docs/DEVELOPING.md#releasing).
- `clippy::std_instead_of_core` and `clippy::std_instead_of_alloc` are denied
  workspace-wide, and every path now names the narrowest crate that defines
  the item -- `core::fmt`, `core::ffi::CStr`, `alloc::ffi::CString` and so on.
  Nothing here is `no_std` and no API changed: `core::fmt::Display` *is*
  `std::fmt::Display`. It keeps a future `no_std` build a small step away.
- CI's clippy step now runs with `-D warnings`, so a warning fails the build
  rather than scrolling past.

## [0.1.2] - 2026-09-05

### Fixed

- **`read_array_of` was twelve times slower than the reference
  implementation** on large arrays: 153 MB/s against Python asdf's 1806 MB/s.
  It decoded every element into an `Element` enum and then converted that,
  costing three passes and an intermediate several times the size of the data,
  to deliver bytes already laid out correctly in the mapping. It now takes a
  bulk path when the stored type already is the requested one, and falls back
  for anything else. See [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md).
- `set_array` allocated a `Vec<u8>` per element while encoding.

### Added

- Benchmarks, in `crates/asdf/benches/throughput.rs`.
- `asdf-rs` re-exports `ByteOrder`, `Datatype`, `Element`, `Field`, `Mask`,
  `Ndarray`, `ScalarType` and `Source`. They already appeared in its public
  signatures, so a caller could not name what they were given without also
  depending on `asdf-core`.

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

[Unreleased]: https://github.com/cruzzil/asdf/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/cruzzil/asdf/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/cruzzil/asdf/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/cruzzil/asdf/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cruzzil/asdf/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cruzzil/asdf/releases/tag/v0.1.0
