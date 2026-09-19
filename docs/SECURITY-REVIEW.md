# Security review: handling untrusted input

Conducted 2026-09-16 against the tree at the libasdf 0.2.0 sync, by reading
the read path and then trying to break it, and extended on 2026-09-19 by the
fuzz targets in [`FUZZING.md`](FUZZING.md). Every finding below was
**reproduced**, not inferred; each now has a regression test.

## Threat model

An `.asdf` file is untrusted input. It arrives by download, from a shared
filesystem, or attached to a ticket, and someone runs `asdf info` on it or
hands it to a library that reads it. The attacker controls every byte: the
header, the YAML tree, every block header's size fields, and the compressed
bytes.

A C caller is **not** untrusted in the same sense. The C ABI's contract lets a
caller pass a bad pointer and that is the caller's bug, not ours. What follows
is about file content.

## The thing that made these worse than they looked

The project's stated safety property is that **panics never cross the C
boundary**: every entry point wraps its body in `panic::guard`, which catches
the unwind and returns a fallback. That is true and it works.

It is also not the whole guarantee, and the gap is where these findings lived.
**An abort is not a panic.** A failed allocation calls `handle_alloc_error`,
and an overflowed stack calls the runtime's stack-overflow handler; neither
unwinds, so no `catch_unwind` anywhere can intercept either. Three of the five
findings below took the process down *through* the panic guard.

For a library whose whole purpose is to be linked into someone else's
long-lived program, "the caller's process dies" is the outcome that matters,
and the mechanism that was supposed to prevent it never applied.

`CONFORMANCE.md` and `docs/DEVELOPING.md` have been corrected to say so.

## Findings

All seven are fixed. Five came from the review itself; two came from the fuzz
targets the review recommended -- the first before they had generated a single
input of their own, the second after two hours of running. Severities are as
assessed at the time.

### 1. A decompression bomb evaded the guard by lying downwards — High

`compression::check_expected_size` bounded the block header's **declared**
`data_size` against the compressed length, refusing anything claiming to
expand more than 4096×. Nothing bounded what the codec actually produced:
`read_to_end` grew the destination until memory ran out.

So the guard checked the wrong quantity, and the bypass was to understate
`data_size` rather than overstate it. Measured: 40 KiB of zlib expanded to
40 MiB with `data_size: 8`, unimpeded. zlib reaches roughly 1000:1 on
compressible input and bzip2 far more, so a 10 MB file is a 10 GB allocation,
and a file may carry many blocks.

**Fixed** by bounding the *output*, which is what upstream libasdf already
did: decompression now reads through `Read::take(data_size + 1)` and fails if
the stream has more in it than the header accounts for, with the same check
applied per chunk in the LZ4 path. The declared size is now load-bearing in
both directions. The ratio guard stays as the upper bound.

Pinned by `robustness::aborts::a_compressed_block_may_not_inflate_past_its_declared_size`,
which also asserts the *honest* version of the same block still reads — the
point is to refuse the lie, not the size.

### 2. An array's element count was allocated before it was validated — Critical

`elements::decode_all` computed `count = shape.iter().product()` — an
unchecked `u64` product of dimensions read straight from the tree — and then
called `Vec::with_capacity(count)` of `Element`, which is 32 bytes wide.
Nothing had yet checked the count against the number of bytes the block
actually holds.

A **225-byte file** with `shape: [100000000, 100000000]` requested
**320 petabytes**. That is `handle_alloc_error` → `abort()`, so:

```
$ ./cprobe hostile-shape.asdf
opened
root type 2
got ndarray, reading all
memory allocation of 320000000000000000 bytes failed
Segmentation fault (core dumped)
```

— through the C ABI, against the real `libasdf.so`, with the panic guard in
place and unable to do anything about it.

The unchecked product was independently dangerous on its own:
`[(1<<63)+1, 2]` wraps to 2, so a size check performed in the same arithmetic
agrees that two bytes is all that is needed, and whatever is sized from it is
far too small.

**Fixed** in three places:

- `ndarray::element_count` refuses a shape whose product does not fit, and is
  now the only way the count is computed.
- `decode_all` checks `count × item_size + offset` against the block's real
  length and refuses **before** allocating. A file claiming more elements than
  its own block has bytes for is simply wrong, whatever the numbers are.
- `Ndarray::c_strides` returns `Option` rather than wrapping. A wrapped stride
  silently addresses the wrong element, and for a data format quietly wrong is
  worse than refusing.

`decode_inline` no longer reserves from the shape at all, and the four
unchecked multiplies in `ndarray_ffi` are checked. `asdf_ndarray_size` and
`asdf_ndarray_nbytes` saturate: both return a bare `uint64_t` with no error
channel, so `u64::MAX` is the only honest answer for "does not fit" — a
caller's own size check then rejects it, where a wrapped small value sails
through.

Pinned by `aborts::an_impossible_shape_is_refused_before_it_is_allocated` and
`aborts::a_shape_whose_product_wraps_is_refused`.

### 3. Alias expansion in `asdf info` was exponential — High

`info::write_node` follows `Document::resolve`, which follows aliases, with no
memo and no budget. Ten anchors each aliasing the one before, ten ways, is
10^10 nodes. A **444-byte file** drove a 6.4 GB string and aborted.

**Fixed** with a 64 MiB output budget. Refusing to expand aliases was not an
option — expanding them is what `info` is *for*, and upstream expands them
too — so the fix had to leave legitimate output identical. It does: the
largest tree in the reference corpus renders in single-digit kilobytes, and
all 17 golden captures still match byte for byte, ANSI included.

Pinned by `aborts::nested_aliases_render_within_a_budget`.

### 4. A self-referential alias was infinite recursion — High

```yaml
a: &a
  b: *a
```

**96 bytes.** `write_node` descends for ever; the stack overflows; the process
aborts. Nothing catches it.

**Fixed** by carrying the ancestor path and rendering a repeat as `(...)`,
with `TREE_MAX_DEPTH` as an independent second bound. The pattern was already
in the codebase — `value_ffi`'s `FindIter` keeps a `seen` list for exactly
this reason — and simply had not been applied here.

Pinned by `aborts::a_self_referential_alias_does_not_recurse_for_ever`.

### 5. The alias bomb had a second route, through inline arrays — High

**Found by the fuzz target, after the other five were fixed.** Worth the
separate entry, because of how it was missed.

Finding 3 fixed alias expansion in `asdf info` with a rendering budget. The
`read_path` fuzz target then hung for fifteen minutes on the *same input* —
and it never calls `info::render`. The bomb had another way in:
`Ndarray::parse` treats a bare nested sequence as an inline array and calls
`survey_inline` to infer its datatype, which walks every element following
aliases, with no memo and no depth bound. The same 10^10 nodes, in a function
the earlier fix never looked at, and one that *every* caller reading an array
goes through.

Two neighbours came out with it once the first was found:

- `infer_inline_shape` follows the first element down to learn the shape.
  `a: &a [*a]` — **ten bytes** — makes that descent a cycle, and it pushes a
  dimension per iteration for ever.
- `collect_inline` would then faithfully materialise the expansion.

**Fixed** by bounding all three the way `decode_all` is bounded, and on the
same principle: a block-backed array is bounded by its block's length, so an
inline array is bounded by its document's node count. Every element of a real
inline array is a scalar node; a shape calling for more elements than the tree
has nodes can only be alias expansion. `survey_inline` also gets a visit
budget and a depth cap, and `infer_inline_shape` stops when the descent
returns to where it has been.

Pinned by `aborts::nested_aliases_do_not_explode_the_inline_array_walk` and
`aborts::a_cyclic_inline_sequence_does_not_loop`.

The lesson is not that finding 3's fix was wrong. It is that **a fix aimed at
one function fixes one function**, and only something that explores the whole
input space knows how many other ways in there are. See
[`FUZZING.md`](FUZZING.md).

### 6. An LZ4 chunk allocated from its own four-byte header — High

**Found by the fuzz target**, two hours after finding 5, and a repeat of the
same mistake in a different place.

Finding 1 bounded decompression by the block header's declared `data_size`,
and for LZ4 did it by accumulating chunk by chunk:

```rust
let decoded = lz4_flex::block::decompress_size_prepended(chunk)?;  // allocates here
if out.len() + decoded.len() > expected { ... }                    // checks here
```

python-lz4's framing prepends a little-endian decompressed size to every
chunk, and `decompress_size_prepended` allocates from it *before* it decodes
anything. The check is one line too late. Four bytes of `0xff` in a 4 KB file
requested **4.28 GB**, which is what libFuzzer's `-rss_limit_mb` reported.

**Fixed** by reading the chunk's declared size directly and refusing it
against the remaining budget *before* handing the chunk to the decoder, with
the post-decode check kept as a second line in case the decoder does not
honour its own header.

Pinned by `compression::tests::an_lz4_chunk_may_not_allocate_from_its_own_header`,
and the fuzzer's own input is committed at
`fuzz/corpus/read_path/regress-lz4-chunk-oom`.

Note the shape of this, because it is the same shape as finding 5: **a bound
placed after the allocation it is meant to prevent is not a bound.** Both
fixes were written while looking at the right function, and both left a
window open one call deeper.

### 7. External sources could be escaped with a symlink — Low

`reader::external_relative_path` rejects absolute paths, `..` components,
drive and UNC prefixes, and anything with a scheme — a careful lexical check.
A symlink is none of those. `data.bin -> /etc/shadow` is a clean relative name
that `File::open` follows straight out of the directory.

Exploiting it needs the attacker to place a file *beside* the `.asdf`, so it
is a real but narrow escalation rather than a way in.

**Fixed** by canonicalising both the target and the referring file's directory
and requiring the first to sit under the second, which is the only check that
sees through a symlink.

## What let all of this through

`robustness.rs` is the suite whose job this was, and it was green throughout.
Two reasons, both now addressed:

**It stopped at the block layer.** `exercise()` walked the header, the blocks,
the checksums and the tree, and stopped. It never asked an array for its
elements and never rendered a tree — and findings 2, 3 and 4 all live past
that line. It now decodes every `core/ndarray` by every route a caller has,
and renders the tree.

**It only ran in debug.** A debug build catches an arithmetic overflow with a
panic, which the suite reports precisely. A release build wraps silently, and
the wrapped value is what walks past a size check into an allocation. The
suite is now run both ways; they fail in different places.

There is a third reason worth naming, which is that "does not panic" was the
wrong assertion. Every one of these already passed it: an abort is not a
panic, so a test that catches unwinding sees nothing. The new cases assert
what is *refused*, and would have failed before the fix.

A fourth, learned from finding 5: **a hang is a finding too**, and no
assertion catches one. `robustness.rs` would have sat in the alias bomb until
the CI runner timed out, reporting nothing useful. That is what
`-timeout` on a fuzz target is for, and why CI now runs the suite under
`ulimit -v` — both turn "still working" into a failure.

## Not findings

Checked and found sound, recorded so the next reviewer can skip them:

- **Block header parsing.** Bounds-checked, validated for internal
  consistency, `saturating_add` for the end offset, and every size field
  converted with `try_from`. The existing truncation and corruption suites
  cover it well.
- **YAML nesting depth.** saphyr enforces a recursion limit; a million
  brackets is refused at the parser, not at the stack.
- **`tree_inlined` alias handling.** Does not expand aliases eagerly, so the
  bomb in finding 3 does not reach it. Note what this checked and what it did
  not: `tree_inlined` was clean, and the *array* walk one call away was not.
  Finding 5 is what that gap cost.
- **Path traversal, lexically.** The component-by-component check is correct
  and was only missing the symlink case.

## Left alone, deliberately

- **MD5 block checksums are corruption detection, not authenticity.** They are
  what the format specifies and there is nothing to fix, but `verify-checksums`
  passing should not be read as evidence a file has not been tampered with. An
  attacker editing a block simply recomputes the digest.
- **`mmap` and `SIGBUS`.** Documented in `Reader::map`: another process
  truncating the file under us turns a later read into `SIGBUS`. The rationale
  given there assumes a cooperative writer, which is right for the normal case
  and not for a file on a shared filesystem an attacker can also write. Fixing
  it means giving up mapping, which is what makes a multi-gigabyte array
  readable at all. Recorded rather than changed.

## Done since

All three follow-ups the review named, with one revised by measurement.

- **The `cargo-fuzz` targets** are in `fuzz/`, seeded from the reference
  corpus, upstream's fixtures and the attacks above. They found finding 5 on
  the first corpus replay. [`FUZZING.md`](FUZZING.md).
- **`clippy::arithmetic_side_effects`** is denied — but on the *functions*
  that turn file-controlled numbers into an extent, not on whole modules as
  originally suggested. Measured, the module-wide form was 39 hits across
  five files, nearly all of them loop indices; that volume of `#[allow]`
  trains a reader to add another without thinking, which is how the next real
  one gets through. Scoped to the extent-computing functions it is 4 hits,
  **one of which was a real bug** — `strides[dim] * idx` in `decode_all`,
  with both operands straight from the file. Now checked.
- **CI runs the robustness suite capped and in release** (`ulimit -v`
  4 GB), so a regression of finding 2 fails in seconds rather than swapping
  the runner, and one of the arithmetic kind fails at all.

## Worth doing next

- **A longer fuzz campaign, off the pull-request path.** CI's two minutes
  catches rot, not novelty. Hours on one machine is where the next one comes
  from.
- **A structured-input fuzz target.** Both current targets mutate raw bytes,
  so most inputs die at the header. One that generates well-formed files with
  hostile *trees* would reach the parts of the tree walk that raw mutation
  rarely does.
- **Audit every remaining "allocate then check".** Findings 5 and 6 were both
  a bound placed after the allocation it guards. That is a shape worth
  grepping for deliberately rather than waiting to trip over: any
  `with_capacity`, `vec![_; n]` or third-party decoder call whose size comes
  from the file, with its validation on a later line.
