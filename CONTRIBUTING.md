# Contributing

Thanks for looking. This project has an unusual shape — it has to satisfy a C
ABI defined by somebody else *and* read as ordinary Rust — so a few of its
conventions are not the ones you would guess. They are all written down here
and in `docs/DEVELOPING.md`; nothing is folklore.

## Before you start

For anything beyond a typo, open an issue first. The two constraints that most
often sink a well-meant patch are worth checking against your idea early:

- **The C ABI is not ours to change.** `crates/libasdf-rs/include/` is
  upstream libasdf's headers, copied verbatim. If a change would alter a
  struct layout, a function signature, an enum discriminant or an exported
  symbol, it is a change to somebody else's published interface — it belongs
  upstream first. See "Re-vendoring" in `docs/DEVELOPING.md`.
- **Divergence from upstream is a decision, not an accident.** Where behaviour
  differs, `KNOWN-DIVERGENCES.md` records what upstream does, what we do, why,
  and which test pins it. A patch that changes behaviour either preserves what
  is written there or updates it in the same commit.

## Getting set up

```console
$ git clone https://github.com/cruzzil/asdf
$ cd libasdf-rs
$ cargo test --workspace
```

That works with nothing else installed. Several test suites read external
corpora and **skip with a printed note** when they are absent, so a green run
on a bare checkout is weaker evidence than it looks. To run everything, see
"The corpora" in `docs/DEVELOPING.md`.

## What a change needs

Run these before opening a pull request. CI runs all of them.

```console
$ cargo fmt --all
$ cargo clippy --workspace --all-targets      # CI treats warnings as errors
$ cargo test --workspace
$ cargo doc --workspace --no-deps
```

If you touched `crates/libasdf-rs`, also run the three gates that guard the
ABI, and Miri:

```console
$ cargo test -p libasdf-rs --test abi -- --nocapture
$ cargo test -p libasdf-rs --test upstream_suite -- --nocapture
$ MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-ignore-leaks" \
      cargo +nightly miri test -p libasdf-rs --lib
```

### Tests are the deliverable, not the receipt

Every behavioural change needs a test that fails without it. The bar is a
little higher than usual here because three independent oracles for this
format already exist, and a test that checks our behaviour against one of them
is worth more than one that checks our behaviour against our own idea of it:

1. libasdf's own C test suite, compiled against our library;
2. the ASDF Standard's reference corpus, and its committed CLI captures;
3. the Python `asdf` implementation, via the differential tests.

Reach for those before writing a fresh assertion. `docs/DEVELOPING.md`
explains where each lives.

### Pass counts are pinned in both directions

`tests/upstream_suite.rs` records the exact number of upstream C tests each
suite passes, and fails if the count moves **either way**. A fix that makes
more of them pass is good news and still fails the build until you update the
table — that is deliberate, so an improvement is recorded rather than absorbed.

## Style

Match the code around you. Two conventions are specific to this repo:

- **Comments say *why*.** The code already says what. Most comments here exist
  because something is surprising — a C contract, an upstream quirk, a
  deliberate divergence — and that reason is the part worth keeping.
- **Every `unsafe` block carries a `// SAFETY:` note** stating the invariant
  that makes it sound, and reaches for `crate::ffi`'s helpers rather than
  open-coding a pointer conversion. If a new one is genuinely needed, say what
  licenses it.

Commit messages: a short subject line in the imperative, then prose explaining
why. If a commit fixes something subtle, the message is where the next reader
finds out what was actually wrong.

## Licensing

Contributions are accepted under the MIT licence covering the rest of the
project. Do not add a dependency without checking its licence — the MIT story
only holds while the whole graph stays permissive, and
`crates/asdf-core/tests/dependencies.rs` will fail until a new one is added to
its pinned list deliberately.
