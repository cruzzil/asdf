# Upstream sync point

The commit of [libasdf](https://github.com/asdf-format/libasdf) that this
implementation is synchronised with.

| | |
|---|---|
| **Commit** | `4be9e73` |
| **Describe** | `0.2.0` |
| **Subject** | bump version 0.1.0 -> 0.2.0 |
| **Package version** | 0.2.0 |
| **Library interface version** | `1:0:1` — `SONAME` `libasdf.so.0`, filename `libasdf.so.0.1.0` |
| **Synced on** | 2026-09-16 |

## What "synced" means here

Four things are pinned to this commit, and all four have to move together:

1. **The vendored public headers** in `crates/libasdf-rs/include/`, copied
   verbatim. They are the ABI contract, and `crates/libasdf-rs/tests/abi.rs`
   reads the exported symbol list and the struct layouts straight out of them
   rather than out of a list kept by hand.
2. **The C test suite** in `crates/libasdf-rs/tests/upstream_suite.rs`, which
   compiles upstream's own `tests/test-*.c` from a checkout at this commit and
   links them against our `libasdf.so`. Each suite's pass count is pinned in
   both directions.
3. **The committed CLI captures** the golden tests reproduce byte for byte.
4. **The behaviour upstream's suite does not reach**, which is most of the
   engine. Upstream's C tests cover the C ABI; a fix to scalar resolution or
   to the block layer shows up there only if upstream happened to write a
   test for it, so the commit log has to be read rather than only run.

## Updating it

Re-vendoring is a deliberate act: it can change the ABI. The full procedure is
[`docs/UPSTREAM-SYNC.md`](docs/UPSTREAM-SYNC.md).

To see what has landed upstream since this point:

```console
$ git -C /path/to/libasdf log --oneline 4be9e73..origin/main
```
