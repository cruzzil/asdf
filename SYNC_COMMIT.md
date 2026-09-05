# Upstream sync point

The commit of [libasdf](https://github.com/asdf-format/libasdf) that this
implementation is synchronised with.

| | |
|---|---|
| **Commit** | `56d24aa11b3013c362a485b25c2f51db35622d0e` |
| **Describe** | `0.1.0rc2-3-g56d24aa` |
| **Subject** | Merge pull request #247 from embray/stc-symbol-leakage |
| **Package version** | 0.1.0rc2 |
| **Synced on** | 2026-09-03 |

## What "synced" means here

Three things are pinned to this commit, and all three have to move together:

1. **The vendored public headers** in `crates/libasdf-rs/include/`, copied
   verbatim. They are the ABI contract, and `crates/libasdf-rs/tests/abi.rs`
   reads the exported symbol list and the struct layouts straight out of them
   rather than out of a list kept by hand.
2. **The C test suite** in `crates/libasdf-rs/tests/upstream_suite.rs`, which
   compiles upstream's own `tests/test-*.c` from a checkout at this commit and
   links them against our `libasdf.so`. Each suite's pass count is pinned in
   both directions.
3. **The committed CLI captures** the golden tests reproduce byte for byte.

## Updating it

Re-vendoring is a deliberate act: it can change the ABI. The procedure is in
[`docs/DEVELOPING.md`](docs/DEVELOPING.md#re-vendoring-the-headers). In short —
copy the headers, run the ABI and upstream-suite gates, read the diff, and
update this file and `crates/libasdf-rs/include/PROVENANCE.md` in the same
commit.

To see what has landed upstream since this point:

```console
$ git -C /path/to/libasdf log --oneline 56d24aa11b3013c362a485b25c2f51db35622d0e..origin/main
```
