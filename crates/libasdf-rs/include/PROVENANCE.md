# Vendored libasdf public headers

These headers are copied **verbatim** from the upstream libasdf C implementation and are
part of the ABI contract that `libasdf-rs` must satisfy.

They cannot be regenerated from the Rust source, because several public API entry points
exist *only* in the headers:

- `asdf_open`, `asdf_open_ex`, `asdf_write_to` and the `ASDF_ERROR_*` family are
  `_Generic` macros.
- `asdf_open_file`, `asdf_open_fp`, `asdf_open_mem` and `asdf_scalar_datatype_size`
  are `static inline` functions.
- `ASDF_REGISTER_EXTENSION` / `ASDF_DECLARE_EXTENSION` are code-generating macros that
  third-party extensions rely on.

## Provenance

| | |
|---|---|
| Upstream | https://github.com/asdf-format/libasdf |
| Commit | `cff7ab0cc3a33673666f9013d92f4cc50edf2b19` |
| Describe | `0.1.0-4-gcff7ab0` |
| Package version | 0.1.0 |
| Vendored on | 2026-09-09 |
| Licence | BSD-3-Clause — the upstream text is vendored alongside, as `LICENSE` |

`asdf/config.h.in` is **not** vendored; `build.rs` generates `asdf/config.h` for the
target instead. It also generates an empty `sys/time.h` on MSVC, which has no such
header: `asdf/core/time.h` includes it but needs only `struct timespec`, which
`<time.h>` provides. Supplying a header the compiler asks for is not editing a
vendored one.

## Rules

1. **Do not hand-edit these files.** Re-vendor from a pinned upstream commit instead, and
   update the table above.
2. Re-vendoring is a deliberate act: it can change the ABI. Run the layout-assertion and
   symbol-manifest checks afterwards and review any diff.
