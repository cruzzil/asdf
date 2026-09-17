# Security review: handling untrusted input

Conducted 2026-09-16 against the tree at the libasdf 0.2.0 sync, by reading
the read path and then trying to break it. Every finding below was
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

All five are fixed. Severities are as assessed at the time of the review.

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

### 5. External sources could be escaped with a symlink — Low

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

## Not findings

Checked and found sound, recorded so the next reviewer can skip them:

- **Block header parsing.** Bounds-checked, validated for internal
  consistency, `saturating_add` for the end offset, and every size field
  converted with `try_from`. The existing truncation and corruption suites
  cover it well.
- **YAML nesting depth.** saphyr enforces a recursion limit; a million
  brackets is refused at the parser, not at the stack.
- **`tree_inlined` alias handling.** Does not expand aliases eagerly, so the
  bomb in finding 3 does not reach it.
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

## Worth doing next

- **A `cargo-fuzz` target**, seeded from the reference corpus and from the
  five files in these findings. `robustness.rs` says in its own header that
  one belongs beside it. Every finding here was reachable in a handful of
  mutations; a fuzzer would have found them unaided.
- **`clippy::arithmetic_side_effects`**, denied on the modules that compute
  sizes from file-controlled numbers. Four of the five findings were an
  unchecked multiply, and the lint names them all.
- **Run the robustness suite under a memory cap in CI** (`ulimit -v`), so a
  regression of finding 2 fails the build in seconds rather than swapping the
  runner to death.
