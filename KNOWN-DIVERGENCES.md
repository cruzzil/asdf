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
