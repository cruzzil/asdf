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
| Commit | `56d24aa11b3013c362a485b25c2f51db35622d0e` |
| Describe | `0.1.0rc2-3-g56d24aa` |
| Package version | 0.1.0rc2 |
| Vendored on | 2026-09-03 |
| Licence | BSD-3-Clause (see upstream LICENSE) |

`asdf/config.h.in` is **not** vendored; `build.rs` generates `asdf/config.h` for the
target instead.

## Rules

1. **Do not hand-edit these files.** Re-vendor from a pinned upstream commit instead, and
   update the table above.
2. Re-vendoring is a deliberate act: it can change the ABI. Run the layout-assertion and
   symbol-manifest checks afterwards and review any diff.
