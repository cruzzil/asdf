# Syncing to a new upstream libasdf

`libasdf-rs` is a drop-in replacement for a *specific* commit of upstream
libasdf, recorded in [`SYNC_COMMIT.md`](../SYNC_COMMIT.md). Moving that pin is
a deliberate piece of work with a fixed shape. This is that shape.

## Why it needs a procedure at all

The obvious half of a sync — copy the headers, watch the ABI gates — is the
half the tooling already checks for you. The half that bites is the other one:

**Upstream's C test suite only covers upstream's C ABI.** Ten of its
twenty-one suites cannot run here at all, because they include libasdf's
private headers. A fix to scalar resolution, to the block layer or to the
emitter reaches our gates only if upstream happened to write a *public-header*
test for it. So the commit log has to be **read**, not only run.

And upstream's own tests are a moving target in both directions: a new test
can fail here because we have a real gap, *or* because upstream changed a
behaviour we faithfully reproduced. Telling those apart is the work.

## 0. Get a clean checkout at the new commit

Work from a detached worktree, not from the shared checkout — that one is
probably on a branch with local work on it, and the upstream-suite gate
compiles whatever `LIBASDF_DIR` points at.

```console
$ cd ~/code/libasdf && git fetch --all --tags
$ git worktree add --detach /tmp/libasdf-new <commit>
$ cd /tmp/libasdf-new && git submodule update --init tests/munit third_party/STC
```

Both submodules matter: without them the `upstream_suite` gate silently skips,
and a skipped gate looks exactly like a passing one.

From here on, every command below assumes `LIBASDF_DIR=/tmp/libasdf-new`.

## 1. Record the baseline *before* changing anything

```console
$ LIBASDF_DIR=/tmp/libasdf-new cargo test -p libasdf-rs --test upstream_suite -- --nocapture
```

This is the single most informative step in the whole procedure, and it is the
one that is easiest to skip. Running the *new* suite against the *unchanged*
library tells you, for free, exactly which upstream changes have observable
consequences here: every new failure is either a feature to implement or a
behaviour that changed under us. Do it first, and keep the output.

## 2. Read the log

```console
$ git -C /tmp/libasdf-new log --oneline <old>..<new>
$ git -C /tmp/libasdf-new diff --stat <old>..<new>
```

Then read the diff of each of these, in this order:

| Path | What you are looking for |
|---|---|
| `include/` | The ABI. New `ASDF_EXPORT`s, new macros, changed signatures, changed structs or enums. |
| `CHANGES.rst`, `changes/` | Upstream's own account of what it thinks it changed. |
| `configure.ac` | `LIBASDF_VERSION_INFO` and its history comment — upstream's own statement of whether the ABI broke. |
| `src/` | **Behaviour.** This is the part no gate will tell you about. |
| `tests/` | New or changed expectations, including fixtures. A changed fixture is a changed behaviour. |

Ignore `.github/`, `cmake/`, `docs/` and `Makefile.am` unless something else
points at them.

## 3. Re-vendor the headers

Copy, never edit:

```console
$ for f in $(cd crates/libasdf-rs/include && find . -name '*.h' | sed 's|^\./||'); do
>     cp "/tmp/libasdf-new/include/$f" "crates/libasdf-rs/include/$f"
> done
$ cp /tmp/libasdf-new/LICENSE crates/libasdf-rs/include/LICENSE
$ git diff --stat -- crates/libasdf-rs/include
```

The diff here should be *identical* to upstream's own `include/` diff from
step 2. If it is not, a header was added or removed and the loop above missed
it — it only copies files we already have.

Then check whether anything in `build.rs` existed to work around a header
problem upstream has now fixed. The `sys/time.h` shim was exactly that, and
sat there dead for one release before anyone looked.

## 4. Implement

New behaviour goes in **`asdf-core`**, with thin plumbing in each face — see
[`DEVELOPING.md`](DEVELOPING.md). A new C entry point is usually the only
thing that belongs in `libasdf-rs` itself.

For each item from step 1, decide which it is:

- **a gap of ours** — implement it;
- **a behaviour upstream changed** — change ours to match, and delete the
  entry in [`KNOWN-DIVERGENCES.md`](../KNOWN-DIVERGENCES.md) if one existed
  for it. A divergence that upstream has closed is not a divergence any more,
  and leaving the entry in place makes the file lie;
- **a behaviour we deliberately do not share** — say so in
  `KNOWN-DIVERGENCES.md`, with the test that pins it.

Anything that changes what `libasdf_version` reports goes in
`crates/libasdf-rs/src/extension_ffi.rs`. That static is the *upstream* version
implemented, not the crate's own.

## 5. Cover what the upstream suite cannot reach

A new entry point declared in a header the suite can't compile against — one
that `tests/test-file.c` covers, say — has no gate at all until you write one.
Two places to put it:

- a Rust unit test in the relevant `*_ffi.rs`, which also puts it under Miri;
- a C program in `crates/libasdf-rs/tests/abi.rs`, which is the *only* way to
  reach a `_Generic` macro or a `static inline`, since neither is a symbol.

## 6. Run every gate

```console
$ cargo fmt --all && cargo clippy --workspace --all-targets
$ LIBASDF_DIR=/tmp/libasdf-new cargo test --workspace
$ LIBASDF_DIR=/tmp/libasdf-new cargo test -p libasdf-rs --test upstream_suite -- --nocapture
$ cargo test -p libasdf-rs --test abi -- --nocapture
$ MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-ignore-leaks" \
>     cargo +nightly miri test -p libasdf-rs --lib
```

Watch for gates that **skip**. `cargo test --workspace` on a machine without
the corpora, or without Python `asdf` installed, is green and means much less
than it looks. The differential suite prints `skipping: no Python with asdf
available` and still reports `ok`.

Pass counts in `upstream_suite.rs` are pinned in both directions, so a suite
that now passes *more* fails the build until the number is raised. That is
deliberate: an improvement should be recorded, not absorbed silently.

## 7. Update the written record

Every one of these, in the same commit as the code:

| File | What moves |
|---|---|
| [`SYNC_COMMIT.md`](../SYNC_COMMIT.md) | Commit, describe, package version, interface version, date. |
| `crates/libasdf-rs/include/PROVENANCE.md` | The same table, plus any build.rs workaround that went away. |
| [`CONFORMANCE.md`](../CONFORMANCE.md) | Pinned commit, the exported-symbol count, the per-suite pass table, and anything in "Platforms" that upstream has unblocked. |
| [`KNOWN-DIVERGENCES.md`](../KNOWN-DIVERGENCES.md) | Entries upstream has closed; entries the sync opened. |
| [`CHANGELOG.md`](../CHANGELOG.md) | Under `Unreleased`. A sync is always called out as an ABI event. |
| [`README.md`](../README.md) | It quotes the pass counts and the symbol count too. |

The counts in those files are not decoration — they are the figures the gates
print, and a reader uses them to tell whether the document was written against
this version or a previous one. Take them from the test output, not from the
previous version of the document.

## 8. Releasing is a separate decision

A sync lands on `main` under `Unreleased`. Cutting a release is its own act
with its own checklist, in [`DEVELOPING.md`](DEVELOPING.md#releasing). The two
version numbers stay independent: ours, and the upstream ABI we implement.

## Worked example: 0.1.0 → 0.2.0

What the procedure actually turned up, as a sense of the ratio:

- **From the ABI gates, for free:** three new exports (`asdf_free`,
  `asdf_file_find`, `asdf_file_find_ex`) and two new `_Generic` macros.
- **From step 1's baseline run:** `test-ndarray` no longer linked at all
  (it now frees through `asdf_free`), and `test-value` dropped three tests.
- **From reading `src/`:** the reason for those three — upstream had reversed
  its treatment of `.inf` / `.nan`, which we had a documented divergence
  reproducing. The divergence was deleted, not preserved.
- **Found only by reading, with no gate anywhere:** `asdf/core/time.h` no
  longer includes `<sys/time.h>`, which made a `build.rs` workaround dead
  code; and upstream's `asdf_value_as_float` overflow rule changed to the one
  this library already had.
