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
| Commit | `4be9e73` |
| Describe | `0.2.0` |
| Package version | 0.2.0 |
| Vendored on | 2026-09-16 |
| Licence | BSD-3-Clause — the upstream text is vendored alongside, as `LICENSE` |

`asdf/config.h.in` is **not** vendored; `build.rs` generates `asdf/config.h` for the
target instead.

Up to 0.1.0 it also generated an empty `sys/time.h` for MSVC, which has no such
header, because `asdf/core/time.h` included it while needing only `struct timespec`.
Upstream dropped that include in 0.2.0 ([gh-261]), so the shim is gone.

[gh-261]: https://github.com/asdf-format/libasdf/issues/261

## Rules

1. **Do not hand-edit these files.** Re-vendor from a pinned upstream commit instead, and
   update the table above.
2. Re-vendoring is a deliberate act: it can change the ABI. Run the layout-assertion and
   symbol-manifest checks afterwards and review any diff. The procedure is
   [`docs/UPSTREAM-SYNC.md`](../../../docs/UPSTREAM-SYNC.md).
