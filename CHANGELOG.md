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

### Security

- **An unchecked `.product()` sized a `'*'` dimension.** `shape: ['*', 1<<62, 4]`
  panicked in debug and wrapped in release, resolving the streamed dimension
  to a number bearing no relation to the block. Four more of the same shape
  were in `Datatype::item_size`, `asdf-rs::set_array_shaped` and the C ABI's
  inline-storage path; all five are now checked, saturating where the
  signature has no error channel.

  Worth noting where it survived: the function already carried
  `#[deny(clippy::arithmetic_side_effects)]`, added a fortnight ago as the
  mitigation for exactly this class. **The lint flags operators, and
  `.product()` is a method call.** Every place the arithmetic wore an operator
  was fixed then; not one place where it wore a method's clothes was.

### Added

- **A third fuzz target, `structured`.** The other two mutate file bytes, so
  most of their budget goes on getting past the header. This one mutates a
  *description* of a file and renders a valid one, putting every input into
  the shape, stride, datatype and alias code where the findings actually live.
  It reached 7,090 coverage edges from three random seeds in 5,859 runs --
  `read_path` needed a 1,600-file corpus and 145,000 executions for 7,508 --
  and found the defect above in its first minutes. CI builds and replays it.

## [0.2.3] - 2026-09-19

`asdf-core` 0.2.3 and `asdf-yaml` 0.2.1. The other three crates are unchanged
and ask for `^0.2.0`, which matches both, so they pick these up without
releases of their own.

A dependency release: no code changed, and no API with it.

### Removed

- **`asdf-yaml` no longer depends on `hashlink`.** It was declared and never
  used -- it appears nowhere in the source, and the crate builds and passes
  its suite without it. Anyone depending on `asdf-yaml`, and so on
  `asdf-core`, now compiles one crate fewer.

### Changed

- **`md-5` 0.10 -> 0.11** and **`lz4_flex` 0.11 -> 0.14**, both major bumps of
  crates that handle bytes out of an untrusted file. Neither appears in any
  public signature, so neither is breaking for a consumer. Verified rather
  than assumed: all 305 checksums across the reference corpus still verify
  under the new digest, and all 17 compressed blocks -- including a real lz4
  one -- still decode, with 105 of 105 reference pairs matching and the
  round-trip against Python `asdf` still passing.

## [0.2.2] - 2026-09-19

`asdf-core` only. The other four crates are unchanged and stay where they are;
`^0.2.0` matches this, so every one of them picks it up without a release of
its own.

No API changed.

### Security

- **Rendering a tree bounded memory but not work.** 0.2.0 capped `asdf info`'s
  output at 64 MiB, which stops the memory blowup -- but *reaching* a 64 MiB
  cap means formatting 64 MiB first. A **573-byte** file cost **375 ms and
  64 MB** per render, a hundred-thousandfold amplification: merely annoying in
  a CLI, a denial of service in anything rendering files in a loop. Now
  bounded by nodes visited, scaled to the document's own node count, with the
  byte budget lowered to 8 MiB -- the largest tree in the reference corpus
  renders in single-digit kilobytes, so 64 MiB was never headroom, just a big
  number. **375 ms -> 2.8 ms, 64 MiB -> 650 KB**, with all 24 golden captures
  unchanged.

  Found by a one-hour fuzz campaign, as a *slow unit* rather than a crash.
  The existing regression test asserted the output was under 128 MiB, which a
  64 MiB blowup passes comfortably; it now asserts the output lands well under
  the byte budget -- which is what shows the visit budget stopped the walk --
  plus a wall-clock bound.

### Changed

- **A 57-entry fuzzing dictionary** (`fuzz/asdf.dict`), used by CI's campaign
  too. A raw-byte mutator will not invent `#ASDF 1.0.0` or `\xd3BLK`, so
  without one almost every generated input dies in the first twelve bytes.
  With it, an hour's campaign reached ~800 more coverage edges per target than
  the undicted runs.
- CI's fuzz campaign passes `-malloc_limit_mb` below its RSS limit, so an
  out-of-memory report names the allocating line rather than only the process.

## [0.2.1] - 2026-09-19

`asdf-core` and `libasdf-rs`. The other three crates are unchanged and stay at
0.2.0; both fixed crates are a patch bump, so a dependant already asking for
`^0.2.0` picks them up without needing a release of its own.

A security release. Everything here is a fix or the machinery that found it --
no API changed.

### Security

Two more findings, both from the `cargo-fuzz` targets that [0.2.0]'s review
recommended and this change adds. Both are the same mistake in different
places, and worth naming as a shape: **a bound placed after the allocation it
is meant to prevent is not a bound.**

- **An LZ4 chunk allocated from its own four-byte header.** 0.2.0 bounded
  decompression by the block's declared `data_size`, accumulating chunk by
  chunk -- but python-lz4's framing has each chunk declare its own
  decompressed size, and `lz4_flex` allocates from that declaration before it
  decodes anything, so the accumulation check ran one line too late. Four
  bytes of `0xff` in a 4 KB file asked for **4.28 GB**. The chunk's claim is
  now checked against the remaining budget before the decoder sees it.
- **The alias bomb had a second route, through inline arrays.** 0.2.0 fixed
  alias expansion in `asdf info` with a rendering budget. The fuzz target then
  hung for fifteen minutes on the same input in a path that never calls
  `info::render`: `Ndarray::parse` treats a bare nested sequence as an inline
  array and surveys every element to infer its datatype, following aliases
  with no memo and no depth bound. Two neighbours came out with it --
  `infer_inline_shape` looping for ever on `a: &a [*a]`, ten bytes, and
  `collect_inline` materialising the expansion. All three are now bounded by
  the document's node count, exactly as a block-backed array is bounded by its
  block's length.

Two smaller ones from auditing for the same shape afterwards:
`inline_ndarray` reserved from a caller-supplied shape before checking it
described the elements given, and `asdf_block_create` aborted on a size the
allocator could not satisfy where `block.h` promises `NULL`.

### Added

- **`fuzz/`, two `cargo-fuzz` targets** and the guide in
  [`docs/FUZZING.md`](docs/FUZZING.md). `read_path` covers the read path
  through to an array's decoded elements; `render_tree` covers the renderers
  and the alias graph. The committed corpus is small and deliberate -- every
  file in it once broke something.
- **A CI `fuzz` job**: builds both targets, replays the committed corpus, and
  runs a two-minute campaign. That catches the targets rotting and the corpus
  regressing; it is not long enough to find anything new, and does not pretend
  to be.

### Changed

- **`clippy::arithmetic_side_effects` is denied on the functions that turn
  file-controlled numbers into an extent.** The review had suggested whole
  modules; measured, that was 39 hits across five files, nearly all loop
  indices, and that volume of `#[allow]` teaches a reader to add another
  without thinking. Scoped to the extent-computing functions it is 4 hits, one
  of which was a real bug -- `strides[dim] * idx` in `decode_all`, both
  operands straight from the file. Now checked.
- **CI runs the robustness suite capped (`ulimit -v` 4 GB) and in release**,
  so a regression of the allocation kind fails in seconds instead of swapping
  the runner, and one of the arithmetic kind fails at all.

## [0.2.0] - 2026-09-17

**All five crates.** Two things happened at once: the sync to upstream libasdf
0.2.0, and a security review of how untrusted files are handled that found
five reproducible ways to abort the process.

This is a **minor bump, not a patch**, because `Ndarray::c_strides` now returns
`Option<Vec<i64>>` where it returned `Vec<i64>` -- it refuses a shape too large
to stride rather than handing back a wrapped stride that silently addresses the
wrong element. That is the only source-breaking change. Note what it means for
the fixes below: `0.1.x` is not semver-compatible with `0.2.0`, so `cargo
update` alone will not pick them up.

### Changed

- **Synced to libasdf `4be9e73` (0.2.0)**, up from `cff7ab0` (0.1.0). Three
  new exported symbols and two new `_Generic` macros; no struct layout, enum
  discriminant or existing signature moved, so this is additive and upstream's
  own `SONAME` stays `libasdf.so.0`. The surface is now **379 declared
  exports**, up from 376, and upstream's C suite still passes **498 of 501**
  -- against its *new* suite, which gained the tests below.
- **`.inf` and `.nan` now resolve as floats, and bare `inf` / `nan` no longer
  do.** This was a documented divergence: libasdf *emitted* `.inf` and `.nan`
  but its `strtod`-based reader read them back as strings, while accepting the
  bare `inf`, `nan` and `infinity` that `strtod` takes and YAML does not.
  Upstream closed the round trip in 0.2.0 and we follow, under both schemas --
  `Schema::Yaml11` also stops accepting a signed `-.nan`, which PyYAML rejects.
  Verified spelling by spelling against PyYAML itself. The entry in
  `KNOWN-DIVERGENCES.md` is gone, because the divergence is.
- **`libasdf_version` reports `0.2.0`.** That static is the upstream ABI
  version implemented, not this crate's own version, which has not moved.

### Security

A review of how untrusted files are handled found **five reproducible ways to
abort the process**, four of them from files under 500 bytes. All are fixed,
each with a regression test; [`docs/SECURITY-REVIEW.md`](docs/SECURITY-REVIEW.md)
has the detail.

The common thread is worth stating plainly: this crate's safety property is
that *panics* never cross the C boundary, and that held throughout. But an
abort is not a panic -- a failed allocation and an overflowed stack do not
unwind, so `panic::guard` cannot catch either, and three of these took the
caller's process down straight through it. `CONFORMANCE.md` and
`docs/DEVELOPING.md` now say so.

- **An ndarray's element count was allocated before it was validated.** A
  225-byte file with `shape: [100000000, 100000000]` asked for 320 petabytes,
  through the C ABI, and aborted. The shape's product was also unchecked, so a
  crafted shape could wrap to a small count. `decode_all` now checks the count
  against the block's real length *before* reserving, `element_count` refuses
  a product that does not fit, and `c_strides` returns `None` rather than a
  wrapped stride that would silently address the wrong element.
- **A decompression bomb evaded the ratio guard by understating `data_size`.**
  The guard bounded the *declared* size; nothing bounded what the codec
  produced. 40 KiB of zlib expanded to 40 MiB with `data_size: 8`.
  Decompression is now bounded by the declared size in both directions, which
  is the model upstream libasdf already used.
- **`asdf info` expanded YAML aliases without bound.** 444 bytes of nested
  aliases drove a 6.4 GB string. Rendering now has a 64 MiB budget; real trees
  render in kilobytes and the 17 golden captures are unchanged.
- **A self-referential alias recursed until the stack overflowed.** `a: &a\n
  b: *a`, 96 bytes. The tree walker now carries its ancestor path and renders
  a repeat as `(...)`.
- **An external `source` could leave its directory through a symlink.** The
  lexical check caught `..` and absolute paths but not a symlink, which is
  neither. Both ends are now canonicalised and compared.

`asdf-core`'s `robustness` suite is why these went unnoticed: it stopped at
the block layer, so it never decoded an array or rendered a tree, and it only
ran in debug, where an arithmetic overflow panics instead of wrapping. It now
does both, and asserts what is *refused* rather than only that nothing
panicked -- an abort passes a "did not panic" test perfectly well.

### Added

- **`asdf_free`**, for the four entry points that hand back a buffer the
  library allocated (`asdf_write_to_mem`, `asdf_ndarray_read_all`,
  `asdf_ndarray_read_tile_ndim`, `asdf_ndarray_read_tile_2d`). Upstream's
  headers used to tell the caller to use `free()`, which made the C runtime's
  allocator part of the interface -- raised as [libasdf#250] and now answered.
  What this crate allocates with is unchanged: callers written against the
  older headers are still calling `free()` on these buffers and still work.
- **`asdf_file_find` and `asdf_file_find_ex`**, and the `asdf_find` /
  `asdf_find_ex` `_Generic` macros that dispatch on `asdf_file_t *` or
  `asdf_value_t *`. Searching from a file's root no longer needs the caller to
  fetch and destroy a root handle.
- A C gate for both of the above, in `tests/abi.rs`. Upstream covers them in
  `tests/test-file.c`, which includes libasdf's private `file.h` and so cannot
  run in the upstream-suite gate; and the `asdf_file_t *` arm of a `_Generic`
  macro exists only in the header, where no Rust test can reach it.
- [`docs/UPSTREAM-SYNC.md`](docs/UPSTREAM-SYNC.md) -- what a sync to a new
  upstream libasdf actually involves. The half the ABI gates check is the easy
  half; the half that bites is that upstream's C suite only covers upstream's
  C ABI, so a change to scalar resolution or the block layer reaches our gates
  only by luck. The commit log has to be read, not only run.

### Removed

- The generated MSVC `sys/time.h` stand-in. `asdf/core/time.h` included
  `<sys/time.h>` for a `struct timespec` that `<time.h>` already provides, and
  MSVC ships no `sys/` directory; upstream dropped the include in 0.2.0
  ([libasdf#261]), so `build.rs` no longer has to supply one.

[libasdf#250]: https://github.com/asdf-format/libasdf/issues/250
[libasdf#261]: https://github.com/asdf-format/libasdf/issues/261

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
  which [libasdf#251] had blocked. Three things beyond the headers had to
  give: `asdf/core/time.h` pulls in `<sys/time.h>` for a `struct timespec`
  that `<time.h>` already provides, so `build.rs` generates a shim rather
  than editing a vendored header; the include-path list was colon-joined,
  which a `C:\` path breaks; and the `.CRT$XCU` constructor pointer in
  `shim.c` was `static`, so `/include:` could not resolve it and the DLL
  failed to link -- that branch had never been compiled before. The
  ABI conformance harness stays Unix-only: it drives `cc` and `nm`, which
  MSVC does not provide. Porting it is worth doing, since Windows is where
  struct layouts differ, and it is not done here.
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

[Unreleased]: https://github.com/cruzzil/asdf/compare/v0.2.3...HEAD
[0.2.3]: https://github.com/cruzzil/asdf/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/cruzzil/asdf/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/cruzzil/asdf/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/cruzzil/asdf/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/cruzzil/asdf/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/cruzzil/asdf/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/cruzzil/asdf/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cruzzil/asdf/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cruzzil/asdf/releases/tag/v0.1.0
