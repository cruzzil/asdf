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
