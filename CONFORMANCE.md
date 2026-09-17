# Conformance baseline

`libasdf-rs` is a drop-in replacement for a *specific* upstream libasdf. That
target is pinned here so the ABI has a fixed definition rather than a moving
one, since upstream is at `0.2.0` and still changing.

| | |
|---|---|
| Upstream repository | https://github.com/asdf-format/libasdf |
| Pinned commit | `4be9e73` |
| Describe | `0.2.0` |
| Package version | `0.2.0` |
| Library interface version | `1:0:1` -- `SONAME` `libasdf.so.0` |
| ASDF Standard | 1.6.0 (reading 1.0.0 through 1.6.0) |
| ASDF file format | 1.0.0 |

Upstream's `SONAME` stayed at `libasdf.so.0` across 0.1.0 -> 0.2.0: three
interfaces were added and none removed, so `current` and `age` rose together
and `current - age` did not move. A caller built against 0.1.0 keeps working.

Re-basing onto a newer upstream is deliberate work, not a routine update: it
can change the ABI, and most of what it changes is not in the ABI at all. The
procedure is [`docs/UPSTREAM-SYNC.md`](docs/UPSTREAM-SYNC.md).

## The exported surface

**379 distinct symbols**, every one of them declared `ASDF_EXPORT` in the
vendored headers. That figure is not maintained by hand: the
`every_declared_export_is_defined` gate preprocesses each vendored header,
reads the declarations out of the result, and fails if the shared library is
missing any of them. Re-vendoring upstream's headers therefore moves the target
by itself -- 0.2.0 raised it from 376 by adding `asdf_free`, `asdf_file_find`
and `asdf_file_find_ex`.

Of those, **77** come from `ASDF_DECLARE_EXTENSION`: eleven functions for each
of the seven core extensions (`meta`, `datatype`, `ndarray`, `software`,
`history_entry`, `extension_metadata`, `time`). Two are data rather than
functions: `libasdf_version` and `libasdf_software`.

The count assumes `ASDF_HAVE_FLOAT16`. Where the target's C compiler lacks
`_Float16`, upstream's headers leave `asdf_ndarray_read_float16_at`
undeclared and the surface is 375.

The library additionally exports the six `asdf_shim_*` helpers that `shim.c`
calls back into, for **387 exported symbols in total**; see
KNOWN-DIVERGENCES.md for why they cannot be hidden.

Several API entry points are **not** symbols and exist only in the headers,
which is why the headers are vendored rather than generated:

- `asdf_open`, `asdf_open_ex`, `asdf_write_to`, `asdf_find`, `asdf_find_ex`,
  and the `ASDF_ERROR_COMMON` / `ASDF_ERROR_OOM` / `ASDF_ERROR_SYSTEM` family
  are `_Generic` macros.
- `asdf_open_file`, `asdf_open_fp`, `asdf_open_mem` and
  `asdf_scalar_datatype_size` are `static inline`.
- `ASDF_REGISTER_EXTENSION` and `ASDF_DECLARE_EXTENSION` generate code in the
  caller's translation unit.

## Gates

Run with `cargo test -p libasdf-rs --test abi`. All require a C compiler; they
skip with a note when one is absent.

| Gate | What it proves |
|---|---|
| `headers_compile_standalone` | Every vendored header compiles alone, warning-clean, against the generated `config.h`. Catches a bad re-vendor first. |
| `enum_discriminants_are_stable` | Discriminants match from C. Several are not sequential: `ASDF_BYTEORDER_BIG` is `'>'`, `asdf_value_err_t` runs negative, the option enums are bit positions. |
| `public_struct_layouts_match` | `sizeof`, `_Alignof` and every `offsetof` agree between C and the Rust `#[repr(C)]` mirror. A wrong offset is silent memory corruption in a C caller, not a compile error. |
| `c_caller_can_use_the_library` | A real C program compiles against the headers, links the real `libasdf.so`, and gets correct results. |
| `exports_only_the_asdf_namespace` | Port of upstream's `tests/test-symbol-leakage.sh`. Nothing outside `asdf_` / `ASDF_` / `libasdf_` is exported. |
| `shim_entry_points_are_exported` | The `shim.c` entry points survive linking. Nothing in Rust references them, so without `+whole-archive` and our own version script the linker drops them silently. |
| `every_declared_export_is_defined` | Every `ASDF_EXPORT` declaration in the preprocessed headers resolves to a defined symbol. The complement of the leakage gate: that one catches what we export and should not, this one catches what upstream promises and we do not provide. A miss is a link error in a consumer, invisible to the Rust build. |
| `c_caller_can_walk_the_event_stream` | The low-level event API, walked from C over `basic.asdf`: the event sequence, the YAML sub-events and their expanded tags, and the tree and block accessors. Ported from upstream's `tests/test-event.c`, reduced to what the public headers expose. |
| `a_c_caller_can_use_the_0_2_0_additions` | `asdf_find` / `asdf_find_ex` on both arms of their `_Generic` dispatch, traversal order and depth limit, and `asdf_free` on a buffer `asdf_write_to_mem` allocated. Upstream covers these in `tests/test-file.c`, which the upstream-suite gate cannot compile. |

A second family of gates lives in `asdf-core`'s test suite and compares
rendered output against upstream's committed fixtures byte for byte:

| Gate | What it proves |
|---|---|
| `info_goldens` | `asdf info` reproduces all 17 of upstream's `.info.txt` captures, ANSI styling included. |
| `event_goldens` | `asdf events --verbose` reproduces all 4 of upstream's `.events.txt` captures. These pin the event *order*, which is not the file's own — the block index is reported before the tree — and the names libfyaml gives YAML events (`+MAP`, `=VAL`, `-SEQ`), which appear in no header. |
| `verify_goldens` | `asdf verify-checksums --verbose` reproduces all 3 of upstream's captures, one of which has a deliberately wrong digest — so the failure path is pinned as precisely as the success one. |

## Upstream's own C test suite

The strongest conformance evidence the project can produce: the tests
*upstream wrote for its own implementation*, compiled against the vendored
headers and linked against our shared library.

Run with `cargo test -p libasdf-rs --test upstream_suite`. It needs a libasdf
checkout at the pinned commit with its submodules initialised:

```console
$ cd ~/code/libasdf && git submodule update --init tests/munit third_party/STC
```

Without them the gate **skips**, which looks exactly like passing. Point it
elsewhere with `LIBASDF_DIR`; when syncing, point it at a detached worktree of
the new commit rather than at a checkout carrying local work.

Eleven of upstream's twenty-one suites build against the public ABI. The
other ten include libasdf's private headers -- `event.h`, `parser.h`,
`stream.h`, `compression/compression.h` -- to reach internals that are
implementation detail rather than interface, so they cannot run against a
different implementation by construction. Each is listed in the test with the
header that rules it out.

Three needed a nudge, and none of them stands a private API in:

- `test-ndarray` reaches for `compat/numeric.h`, a type alias for `_Float16`
  and nothing more, so libasdf's `src/` is added to its include path *after*
  the vendored headers and `asdf/*.h` still resolves to ours.
- `test-value` carries a stray `#include <libfyaml.h>` and uses nothing from
  it, so an empty header of that name lets it compile.
- `test-block` includes libasdf's private `file.h` for the *public* types it
  re-exports and touches nothing private, so a stand-in that includes
  `asdf/block.h`, `asdf/emitter.h` and `asdf/file.h` is what it actually
  needs.

Every suite's pass count is pinned. A change that loses ground fails, and so
does one that gains ground without updating the number, so the figure below
cannot drift.

**498 of 501.**

| Suite | | |
|---|---|---|
| `test-version` | 4/4 | |
| `test-tag` | 1/1 | |
| `test-tests` | 3/3 | the harness's own self-test |
| `test-error` | 3/3 | |
| `test-time` | 17/17 | |
| `test-core-extensions` | 16/16 | |
| `test-ndarray` | 257/257 | the numeric conversion matrix, both byte orders deep |
| `test-value` | 66/68 | the value API in full |
| `test-block` | 6/6 | the low-level block API, including verbatim compressed copies |
| `test-extension` | 12/13 | |
| `test-reference-files` | 113/113 | every tagged value in every reference file |

The three that do not pass are all `compare_files` checks of emitted YAML
against fixtures libasdf wrote — whose `asdf_library` names libasdf. They
cannot pass for any other implementation. Byte parity on emitted YAML is a
nice-to-have and never a gate; the binary block layer is where bytes matter,
and there they are exact.

## Undefined behaviour

`cargo +nightly miri test -p libasdf-rs --lib` runs the FFI layer's 202 unit
tests under Miri. That crate holds every unsafe block in the workspace bar one
(`asdf-core`'s memory map, which Miri cannot execute and which `Reader::open`
sidesteps under `cfg(miri)`).

Miri found two defects that the unit tests, the 501-case C suite, the ABI
layout assertions and the reference corpus all passed:

- `asdf_ndarray_data` handed C a buffer aligned to 1, held in a `Vec<u8>`.
  Every caller casts it -- `int32_t *values = asdf_ndarray_data(...)` -- so the
  dereference was undefined behaviour and, on a strict-alignment target, a bus
  error. Upstream gets this right for free by using `malloc`. Fixed by backing
  the buffer with a 16-byte-aligned element type.
- `asdf_tree_info_t.buf` was derived from a `CString` that was then moved into
  the event. Moving a `Box` retags its allocation, invalidating the pointer C
  was left holding. Fixed by filling the field in once the event reaches its
  final home.

Both are latent on glibc/x86-64, which is why they needed a checker rather
than a test. Run with `-Zmiri-ignore-leaks`: the extension registry is
populated before `main` and never torn down, matching upstream's
constructor-built one.

### Panics are caught. Aborts are not.

Every entry point wraps its body in `panic::guard`, so an unwind never reaches
a C caller. That is the guarantee, and it holds.

It is worth being precise about what it does *not* cover, because the
[security review](docs/SECURITY-REVIEW.md) found three ways to take a caller's
process down straight through it. A failed allocation calls
`handle_alloc_error` and an overflowed stack calls the runtime's handler;
neither unwinds, so there is nothing for `catch_unwind` to catch. A shape that
asks for more memory than exists, or a YAML alias pointing at its own
ancestor, aborts -- guard or no guard.

The defence is therefore not the guard but refusing the input before it gets
that far, which is where those fixes live. `asdf-core`'s `robustness` suite
pins them, and asserts what is refused rather than only that nothing panicked
-- an abort passes a "did not panic" test perfectly well.

## Platforms

Linux and macOS, on x86-64 and aarch64, build and test everything.

**Windows builds and tests all five crates**, x64 and arm64, since the fixes
for [libasdf#251] landed upstream: `asdf/value.h` no longer takes a POSIX
`ssize_t`, and the option-flag enums shift `1ULL` rather than a `1UL` that is
32 bits wide on LLP64. 0.2.0 removed the last MSVC workaround on this side too
-- `asdf/core/time.h` no longer includes `<sys/time.h>`, which MSVC does not
ship, so `build.rs` no longer generates a stand-in for it ([libasdf#261]).

**The ABI conformance harness stays Unix-only.** Not the library: the harness.
It drives `cc` with `-std=c11 -I -L -lasdf -Wl,-rpath` and reads symbols with
`nm -D`, none of which MSVC provides -- it wants `cl` flag syntax, an import
`.lib` beside the DLL, and `dumpbin /exports`. The CI step is gated on
`matrix.unix` so the gate is *absent* on Windows rather than quietly reporting
zero tests. Porting it is worth doing, since Windows is where struct layouts
differ, and it is not done.

[libasdf#251]: https://github.com/asdf-format/libasdf/issues/251
[libasdf#261]: https://github.com/asdf-format/libasdf/issues/261

## Not yet wired up
- **Differential testing against the real libasdf.** Blocked on building the C
  library locally, which needs `libfyaml`, `cmake`, `libbz2`, `liblz4` and
  `libmd`, plus `git submodule update --init` in the libasdf checkout.
  Differential testing against Python asdf *is* wired up, in
  `crates/asdf-core/tests/differential.rs`.
