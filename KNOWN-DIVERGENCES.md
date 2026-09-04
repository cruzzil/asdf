# Known divergences

Deliberate, tested differences between `libasdf-rs`, upstream libasdf, and the
Python `asdf` library. Each is covered by a test that will fail if the
behaviour drifts.

## Scalar resolution: `.inf` / `.nan`

**Upstream behaviour.** libasdf resolves plain scalars with C's `strtoull`,
`strtoll` and `strtod` at base 0, requiring the whole string to be consumed.
`strtod` does not accept YAML's `.inf` / `-.inf` / `.nan` spellings, so libasdf
reads them back as **strings** — even though its own emitter *writes* those
spellings for non-finite floats (`src/value.h`,
`ASDF_NODE_OF_FLOAT_VALUE_TYPE`). This is a genuine round-trip asymmetry in
upstream, verified against the C library's `strtod`.

**What we do.** `Schema::Libasdf` (the default) reproduces it exactly, so
drop-in parity holds. `Schema::Yaml11` resolves them as floats, which is what
YAML 1.1 and 1.2 both specify and what Python asdf does.

Covered by `scalar::tests::yaml_infinity_spellings_diverge_between_schemas`.

## Scalar resolution: integers before booleans

libasdf tries integer parsing *before* boolean parsing, and its boolean parser
accepts `0` and `1`. The consequence is that an untagged `1` resolves as
`uint8`, never as a boolean; the `0`/`1` boolean spellings only surface under
an explicit `!!bool` tag. We reproduce this ordering.

Covered by `scalar::tests::int_is_tried_before_bool`.

## Scalar resolution: radix prefixes

Because libasdf uses base 0, a bare leading zero means **octal** in C's sense
(`010` is 8) and `0x` means hex. YAML 1.2's `0o` prefix is *not* recognised and
falls through to a string. YAML 1.1's `0o`, `0b`, underscores and sexagesimals
are recognised only under `Schema::Yaml11`.

Covered by `scalar::tests::base0_radix_matches_c_not_yaml` and
`scalar::tests::yaml11_underscores_and_sexagesimals`.

## YAML version

The ASDF Standard mandates `%YAML 1.1`. libasdf, via libfyaml, effectively
applies YAML 1.2 core-schema resolution regardless; Python asdf, via PyYAML,
applies the genuine 1.1 resolver. The three already disagree on unquoted
`yes` / `no` / `on` / `off`, sexagesimals, and leading-zero octals. We default
to libasdf's behaviour and offer `Schema::Yaml11` for reading Python-written
files.

## Output formatting

Byte-level parity with libasdf's emitted YAML is a nice-to-have, never a gate.
Equality is judged at the YAML-value level. The binary block layer is exempt:
block headers, checksums, padding and index offsets are byte-exact by
specification.

## Block checksums on compressed blocks

**The specification** means the block's MD5 to cover the used data as stored,
i.e. the compressed bytes for a compressed block.

**Python asdf 5.x and earlier** instead checksum the *uncompressed* data
([asdf#2015](https://github.com/asdf-format/asdf/issues/2015)). libasdf works
around this by reading the file's `asdf_library` metadata and, when the writer
is `asdf` at major version 5 or below, verifying against the decompressed
bytes instead.

**What we do.** The same workaround, with the same version test. It is not a
theoretical concern: every one of the 17 compressed blocks across the ASDF
Standard reference corpus and libasdf's fixtures is written by an affected
version, so without the workaround all 17 would fail verification.

Covered by `reader::tests::the_python_checksum_bug_is_worked_around`, which
pins both directions -- an affected writer verifies, an unaffected one does
not -- and by `corpus::every_checksummed_block_verifies`.

**A correction to libasdf's version test.** libasdf treats any `asdf` at
major version 5 or below as affected. Measured against a real install, that
is too broad: **asdf 5.3.1 records the digest of the stored bytes, i.e.
correctly.** Applying the workaround on the strength of the version alone
would therefore mislabel a correct file, and libasdf logs a warning saying so.

We keep the same version test but use it only as a *gate on a fallback*: the
stored bytes are checked first, and the decompressed bytes are tried only if
that fails. A file from a corrected writer verifies on the first attempt and
never reaches the workaround, so the too-broad version test costs nothing.

Measured by `differential::python_written_checksums_verify`, which reports
which form the installed Python asdf actually used rather than assuming.

## Five extra exported symbols: `asdf_shim_*`

Two parts of the C ABI cannot be written in stable Rust -- the three variadic
entry points and `asdf_ndarray_read_float16_at`, whose `_Float16` return uses
a different register class from `uint16_t`. They live in `shim.c`, which calls
back into Rust through five helpers: `asdf_shim_error_set`,
`asdf_shim_error_format`, `asdf_shim_error_set_system`, `asdf_shim_log_message`
and `asdf_shim_ndarray_read_float16_bits_at`.

Upstream declares no such symbols, so the shared library exports five names
upstream's does not. The version script cannot hide them: GNU ld resolves a
symbol matching wildcards in both `global` and `local` in favour of the global
one, and they have to match `asdf_*` to satisfy the symbol-leakage test that
reserves that namespace for libasdf. Renaming them outside the namespace would
trade one problem for a worse one, since the version script is not applied on
macOS at all.

They collide with nothing: they sit inside the namespace upstream reserves for
itself, and a program that defines its own `asdf_shim_*` is already in
violation of that reservation. Nothing in the public headers references them.

Covered by `abi::every_declared_export_is_defined`, which checks the other
direction -- every symbol the vendored headers declare is defined -- and by
`abi::exports_only_the_asdf_namespace`.

## External array sources are read by the engine, not by the C ABI

An `ndarray`'s `source` may be a string naming another file rather than a
block index; that is how the standard's *exploded* form stores data. Upstream
libasdf does not read them. `src/core/ndarray.c` asks for `source` as a
`uint64` and, on a type mismatch, logs

> currently only internal binary block sources are supported; ndarray at %s
> has an unsupported source and will not be read

and hands back nothing.

**What we do.** `asdf_core` resolves them, so the idiomatic API and the CLI
read exploded files; `libasdf-rs` deliberately does not, so a C caller sees
what upstream's would. This is the split the project is built around: missing
functionality goes in the engine, and the C surface stays at parity.

Resolution is deliberately narrow. The URI must be a relative path with no
`..` component and no scheme, so a tree can only name files beneath its own
directory -- a tree is untrusted input, and following an arbitrary path out of
it would let a crafted file name anything on the machine. A file read from
memory has no directory to resolve against and resolves nothing.

Covered by `reader::tests::external_source_uris_may_not_escape_the_directory`,
`reader::tests::an_external_source_is_read_from_the_neighbouring_file` and
`reference_pairs::inlined_reference_trees_equal_their_expected_yaml`, which
now covers `exploded.asdf`.

## Complex numbers are spelled the way CPython spells them

The `core/complex-1.0.0` schema accepts a family of spellings and pins none of
them. Every complex value in the reference corpus was written by Python asdf,
so the canonical text is CPython's `complex.__repr__`: `0j`, `(-0+0j)`,
`(nan-infj)`, `-1.7976931348623157e+308j`. Note what that is *not* -- it is
not `(0.0+0.0j)`, because CPython formats the parts without forcing a decimal
point, and it is not `1.7976931348623157e308`, because the exponent carries a
sign and at least two digits.

`asdf_core::core::pyrepr` reproduces it, including the shortest-round-trip
digits and the switch to exponent notation when the decimal point would fall
outside `-4 < decpt <= 16`. Upstream libasdf has no complex support at all, so
there is nothing to be at parity with; matching Python is what makes the
corpus compare equal.

Covered by `pyrepr::tests` and, against the interpreter itself rather than
written-down expectations, by
`differential::float_and_complex_spellings_match_python`.
