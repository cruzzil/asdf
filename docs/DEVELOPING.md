# Developer guide

How this project is put together, and the things that are not obvious from
reading the code. For the rules a patch has to satisfy, see
[`CONTRIBUTING.md`](../CONTRIBUTING.md).

## The shape of it

Everything the library actually *does* lives in `asdf-core`. The two public
faces are thin projections of it:

```
                 ┌─────────────┐        ┌──────────┐
   C callers ───▶│ libasdf-rs  │        │ asdf-cli │
                 │  (C ABI)    │        └────┬─────┘
                 └──────┬──────┘             │
                        │      ┌─────────┐   │
   Rust callers ───────▶│      │ asdf-rs │───┤
                        │      └────┬────┘   │
                        ▼           ▼        ▼
                     ┌──────────────────────────┐
                     │        asdf-core         │  the engine
                     └────────────┬─────────────┘
                                  ▼
                     ┌──────────────────────────┐
                     │        asdf-yaml         │  document model
                     └──────────────────────────┘
```

This is the single most important structural decision: because both faces are
projections of one engine, they cannot drift apart in semantics. A bug fixed
for a C caller is fixed for a Rust caller by construction. **New behaviour
belongs in `asdf-core`**, with whatever thin plumbing each face needs.

`asdf-yaml` is separate from `asdf-core` because ASDF's YAML is not quite
anyone's YAML: scalar resolution follows libasdf's C `strtoull`/`strtod`
rather than the YAML spec, and the emitter has to preserve anchors, aliases
and `%TAG` directives that general-purpose emitters drop.

## The corpora

Several test suites read external checkouts. They **skip with a printed note**
when those are absent, so a green `cargo test` on a bare clone is weaker than
it looks. Get both:

```console
$ git clone https://github.com/asdf-format/asdf-standard ~/code/asdf-standard
$ git clone --recurse-submodules https://github.com/asdf-format/libasdf ~/code/libasdf
```

`--recurse-submodules` matters: upstream's C suite needs `tests/munit` and
`third_party/STC`, and without them the `upstream_suite` gate silently skips.

They default to `~/code`; point elsewhere with:

```console
$ ASDF_STANDARD_DIR=/path/to/asdf-standard LIBASDF_DIR=/path/to/libasdf cargo test
```

The differential tests additionally want the Python implementation
(`pip install asdf lz4`) and skip without it.

## The gates

Correctness here is judged against oracles other people wrote, not against our
own expectations. In rough order of how much each is worth:

| Gate | What it proves |
|---|---|
| `-p libasdf-rs --test upstream_suite` | Upstream's own C tests, compiled against the vendored headers and linked against our `libasdf.so`. 498/501. |
| `-p asdf-core --test corpus` / `reference_pairs` | The ASDF Standard's 105 reference files read as the YAML they document. |
| `-p libasdf-rs --test abi` | Exported symbols, struct layouts, enum discriminants, `_Generic` macros, pre-`main` extension registration — from real C programs. |
| `-p asdf-core --test differential` | Round-trips against the Python implementation, both directions. |
| `--test *_goldens` | Upstream's committed CLI captures, byte for byte, ANSI included. |
| `-p asdf-core --test robustness` | Truncation, corruption and absurd sizes never panic. |
| `cargo +nightly miri test -p libasdf-rs --lib` | Undefined behaviour in the unsafe layer. |

### Pass counts are pinned in both directions

`upstream_suite.rs` records what each upstream suite scores. A change that
makes *more* tests pass still fails the build until the table is updated. That
is deliberate: an improvement should be recorded, not absorbed silently.

### Miri, and why it is not optional

Every unsafe block in the workspace is in `libasdf-rs`. Miri found two real
defects there that all the gates above passed, both latent on glibc/x86-64 —
see the "Undefined behaviour" section of [`CONFORMANCE.md`](../CONFORMANCE.md).
Run it after any change to the FFI layer:

```console
$ MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-ignore-leaks" \
      cargo +nightly miri test -p libasdf-rs --lib
```

`-Zmiri-ignore-leaks` because the extension registry is populated before
`main` and never torn down, matching upstream's constructor-built one.
`Reader::open` reads the file whole under `cfg(miri)` rather than mapping it,
since Miri has no `mmap`.

## Working on the C ABI layer

### Raw pointers go through `crate::ffi`

A C ABI is a pointer ABI, so unsafe cannot leave this crate — but each
judgement is made once, in `crates/libasdf-rs/src/ffi.rs`, rather than
re-asserted at hundreds of call sites. Use `write_out`, `c_str`,
`c_string_lossy`, `as_ref`, `as_mut` and `CMallocBuf` in preference to
open-coding.

Two conventions there are easy to get wrong:

- **Out-parameters use `ptr::write`, not `Option<&mut T>`.** The reference
  form looks more idiomatic and is ABI-identical, but C calls these with an
  *uninitialised* destination, and a reference must always point at a valid
  value of its type. `Option<&T>` is right for inputs.
- **`malloc`/`free` is confined to `CMallocBuf`.** Four upstream entry points
  document the caller freeing the buffer with `free()`, which makes the
  allocator part of the ABI — we cannot substitute Rust's. Raised upstream as
  [libasdf#250](https://github.com/asdf-format/libasdf/issues/250).

### Enums crossing the boundary are `c_int`

C may pass any integer. Holding an out-of-range value in a `#[repr(i32)]` Rust
enum is undefined behaviour, so parameters are taken as `c_int` and converted
with a checked `from_i32`. Never take a `#[repr]` enum by value from C.

### Panics never cross the boundary

Unwinding out of an `extern "C"` function is undefined behaviour. Every entry
point wraps its body in `panic::guard`, which catches, reports once, and
returns a caller-supplied fallback.

### What lives in `shim.c`, and why

Only two categories, and nothing else should join them:

1. **Variadic functions.** Defining a variadic `extern "C"` fn in Rust needs
   the unstable `c_variadic` feature.
2. **`_Float16` returns.** Substituting `u16` is not ABI-equivalent — on
   x86-64 SysV a `_Float16` returns in `xmm0`, a `u16` in `rax`.

Plus the one pre-`main` constructor, since Rust has no stable equivalent of
`__attribute__((constructor))`. It is spelled twice: the GCC/Clang attribute,
and a `.CRT$XCU` section pointer for MSVC.

## Re-vendoring the headers

`crates/libasdf-rs/include/` holds upstream's public headers copied verbatim.
They cannot be regenerated from the Rust source, because several public API
entry points exist *only* in the headers — `asdf_open` and friends are
`_Generic` macros, `asdf_open_file` and `asdf_scalar_datatype_size` are
`static inline`, and `ASDF_REGISTER_EXTENSION` is a code-generating macro
third-party extensions depend on.

Re-vendoring can change the ABI, so it is a deliberate act:

1. Copy the headers from a pinned upstream commit. Do not hand-edit them.
   `asdf/config.h.in` is *not* vendored; `build.rs` generates `config.h`.
2. Run `cargo test -p libasdf-rs --test abi -- --nocapture` and
   `--test upstream_suite -- --nocapture`.
3. Read the diff. A changed struct layout or enum discriminant is an ABI
   break and needs saying so.
4. Update [`SYNC_COMMIT.md`](../SYNC_COMMIT.md) and
   `crates/libasdf-rs/include/PROVENANCE.md` in the same commit.

## Divergences are recorded, not discovered

Where our behaviour differs from upstream libasdf or from Python `asdf`,
[`KNOWN-DIVERGENCES.md`](../KNOWN-DIVERGENCES.md) says what each does, why,
and which test pins it. If you find behaviour that differs and is not in that
file, it is a bug in one of the two — not a thing to preserve quietly.

The standing rule on output: **YAML is compared at the value level, never byte
for byte.** YAML admits many spellings of the same value and the Standard's
own corpus says as much. The binary layer is the exception and is byte-exact.
