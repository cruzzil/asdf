# Conformance baseline

`libasdf-rs` is a drop-in replacement for a *specific* upstream libasdf. That
target is pinned here so the ABI has a fixed definition rather than a moving
one, since upstream is at `0.1.0rc2` and still changing.

| | |
|---|---|
| Upstream repository | https://github.com/asdf-format/libasdf |
| Pinned commit | `56d24aa11b3013c362a485b25c2f51db35622d0e` |
| Describe | `0.1.0rc2-3-g56d24aa` |
| Package version | `0.1.0rc2` |
| Shared library `SOVERSION` | `0.0.0` |
| ASDF Standard | 1.6.0 (reading 1.0.0 through 1.6.0) |
| ASDF file format | 1.0.0 |

Re-basing onto a newer upstream is deliberate work, not a routine update: it
can change the ABI. The procedure is to re-vendor the headers (see
`crates/libasdf-rs/include/PROVENANCE.md`), update the table above, and then
review the diff reported by the ABI gates below.

## The exported surface

**376 distinct symbols**, every one of them declared `ASDF_EXPORT` in the
vendored headers. That figure is not maintained by hand: the
`every_declared_export_is_defined` gate preprocesses `asdf.h`, reads the
declarations out of the result, and fails if the shared library is missing any
of them. Re-vendoring upstream's headers therefore moves the target by itself.

Of those, **77** come from `ASDF_DECLARE_EXTENSION`: eleven functions for each
of the seven core extensions (`meta`, `datatype`, `ndarray`, `software`,
`history_entry`, `extension_metadata`, `time`). Two are data rather than
functions: `libasdf_version` and `libasdf_software`.

The count assumes `ASDF_HAVE_FLOAT16`. Where the target's C compiler lacks
`_Float16`, upstream's headers leave `asdf_ndarray_read_float16_at`
undeclared and the surface is 375.

The library additionally exports the five `asdf_shim_*` helpers that `shim.c`
calls back into; see KNOWN-DIVERGENCES.md for why they cannot be hidden.

Several API entry points are **not** symbols and exist only in the headers,
which is why the headers are vendored rather than generated:

- `asdf_open`, `asdf_open_ex`, `asdf_write_to`, and the `ASDF_ERROR_COMMON` /
  `ASDF_ERROR_OOM` / `ASDF_ERROR_SYSTEM` family are `_Generic` macros.
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

A second family of gates lives in `asdf-core`'s test suite and compares
rendered output against upstream's committed fixtures byte for byte:

| Gate | What it proves |
|---|---|
| `info_goldens` | `asdf info` reproduces all 17 of upstream's `.info.txt` captures, ANSI styling included. |
| `event_goldens` | `asdf events --verbose` reproduces all 4 of upstream's `.events.txt` captures. These pin the event *order*, which is not the file's own — the block index is reported before the tree — and the names libfyaml gives YAML events (`+MAP`, `=VAL`, `-SEQ`), which appear in no header. |

## Upstream's own C test suite

The strongest conformance evidence the project can produce: the tests
*upstream wrote for its own implementation*, compiled against the vendored
headers and linked against our shared library.

Run with `cargo test -p libasdf-rs --test upstream_suite`. It needs a libasdf
checkout with its munit submodule initialised:

```console
$ cd ~/code/libasdf && git submodule update --init tests/munit
```

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

## Not yet wired up
- **Differential testing against the real libasdf.** Blocked on building the C
  library locally, which needs `libfyaml`, `cmake`, `libbz2`, `liblz4` and
  `libmd`, plus `git submodule update --init` in the libasdf checkout.
  Differential testing against Python asdf *is* wired up, in
  `crates/asdf-core/tests/differential.rs`.
