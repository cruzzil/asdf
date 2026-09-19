# Fuzzing

`fuzz/` holds two `cargo-fuzz` targets. They exist because
[`SECURITY-REVIEW.md`](SECURITY-REVIEW.md) ended with the observation that
every finding in it was within a handful of mutations of the reference
corpus — a fuzzer would have found them unaided.

That turned out to be literally true. The first corpus replay, before either
target had generated a single input of its own, found a sixth defect the
review had missed.

## Running one

```console
$ cd fuzz
$ cargo +nightly fuzz run read_path -- \
>     -dict=asdf.dict -rss_limit_mb=2048 -timeout=25
```

Nightly is required; `cargo-fuzz` builds with `-Zsanitizer=address`. Stop it
with Ctrl-C. A finding is written to `fuzz/artifacts/<target>/` and replayed
with:

```console
$ cargo +nightly fuzz run read_path fuzz/artifacts/read_path/crash-<hash>
```

`cargo fuzz tmin` shrinks it before you go looking for the cause.

### The flags are not optional

- **`-rss_limit_mb`** is what turns a runaway allocation into a *reported*
  finding. Without it libFuzzer's default still applies, but stating it keeps
  the number the same between a laptop and CI.
- **`-timeout`** is what turns a hang into one. This matters more here than
  it looks: finding 5 was a hang, not a crash, and a target without a timeout
  simply appears to be working hard.
- **`-malloc_limit_mb`**, lower than the RSS limit, makes libFuzzer print the
  *allocation's* stack trace rather than only the process's. That is the
  difference between "something asked for 4 GB" and knowing which line did.
  It is how finding 6 was located.
- **`-dict=asdf.dict`** is worth more here than in most projects. A raw-byte
  mutator will not invent `#ASDF 1.0.0` or `\xd3BLK`, so without a dictionary
  almost every generated input dies in the first twelve bytes and the campaign
  explores the header rather than the format. The dictionary carries the
  magic strings, the tags, the datatype names, the compression names, and the
  YAML spellings that drive scalar resolution.

### Running longer

libFuzzer parallelises itself. On a 24-core machine:

```console
$ cargo +nightly fuzz run read_path <corpus-dir> -- \
>     -workers=6 -jobs=6 -max_total_time=3600 \
>     -dict=asdf.dict -rss_limit_mb=1536 -malloc_limit_mb=1024 -timeout=25
```

Point it at a **working corpus outside the repo**. The committed one is a
regression set, not a place to accumulate; pointing a campaign at it fills
the working tree with thousands of generated files.

## The targets

| | |
|---|---|
| `read_path` | The whole read path, from `scan` through to an array's decoded elements. Driven by the block layer and the datatypes. |
| `render_tree` | `info::render` and the event stream. Driven by the tree's anchor and alias graph. |

They are split because they explore different things. Both findings in the
alias graph came from inputs that a block-layer-driven target would take a
long time to reach, and vice versa.

`read_path` is deliberately wider than the API most callers use. That is the
lesson of the review: `robustness.rs` stopped at the block layer, so it never
decoded an array, and three of five findings lived past that line. **A target
that stops where the old test stopped would have found none of them.**

## The corpus

Two corpora, and only one of them is committed.

**Committed** — `fuzz/corpus/<target>/`, about 76 KB. Every file is there
because it once broke something: the hand-crafted attacks from
[`SECURITY-REVIEW.md`](SECURITY-REVIEW.md), and `regress-lz4-chunk-oom`,
which is the fuzzer's own output from finding 6. Replaying it is a regression
test with better provenance than anything written by hand, and it stays small
enough that CI replays it in under a second.

**Local** — a working corpus for actually fuzzing, which after an hour is tens
of megabytes and has no business in git. Seed it from the checkouts
[`DEVELOPING.md`](DEVELOPING.md#the-corpora) describes:

```console
$ find ~/code/asdf-standard ~/code/libasdf/tests/fixtures -name '*.asdf' \
>     -exec cp {} fuzz/corpus/read_path/ \;
```

Run `cargo fuzz cmin <target>` when it gets unwieldy — it prunes to a minimal
set with the same coverage. **Then put the committed set back**, or a
`git add` sweeps thousands of generated files into the repo. Only promote a
generated input to the committed corpus when it is a finding, and give it a
name that says what it is.

The reference files are not committed here because they already exist in the
`asdf-standard` checkout the rest of the suite needs; duplicating them would
be 700 KB of the same bytes.

## What CI does, and does not, do

CI builds both targets, replays the committed corpus, and runs a two-minute
campaign. That is enough to catch the targets rotting when an API they drive
changes, and enough to catch the corpus regressing. **It is not enough to find
anything new.** A real campaign is hours on one machine, and belongs off the
pull-request path.

## Watch the slow units, not just the crashes

libFuzzer reports three kinds of finding, and only one of them looks like a
bug at a glance:

- a **crash** — a signal or an abort;
- an **OOM** — an allocation past `-rss_limit_mb`;
- a **slow unit** — an input that simply took a long time.

The third is the one that gets ignored, and it is where finding 7 came from:
a 573-byte file that made `asdf info` spend 375 ms and 64 MB. Nothing crashed.
The budget added two days earlier was working exactly as written — it capped
the *output* at 64 MiB, and reaching that cap meant formatting 64 MiB first.

A slow unit is a statement that some input costs wildly more than its size
justifies. In a format whose whole job is to be handed files by other people,
that is a finding.

## What it found

Three defects, on top of the five the review had found by reading. All three
are in [`SECURITY-REVIEW.md`](SECURITY-REVIEW.md), as findings 5, 6 and 7, and
they are one lesson in three costumes:

> A bound has to name the dimension that actually runs away, and sit where the
> running-away happens.

| finding | bounded | left open |
|---|---|---|
| 5 | one function's walk | the same bomb through another function |
| 6 | the accumulated total | the single allocation before the total |
| 7 | memory | time |

Two of the three were holes in fixes written days earlier while looking
directly at the code they were in. That is the argument for fuzzing in one
sentence.

The first is worth reading in full as a lesson about targeted fixes:

The review found that `asdf info` expanded YAML aliases without bound, and
fixed it with a rendering budget. The fuzzer then hung for fifteen minutes
on the *same input* in `read_path`, which never calls `info::render` at all.
The alias bomb had a second route: `Ndarray::parse` treats a bare nested
sequence as an inline array and calls `survey_inline` to infer its datatype,
which walks every element following aliases, with no memo and no depth bound.
Same 10^10 nodes, different function, untouched by the fix.

Two more of its kind were next to it once the first was found —
`infer_inline_shape` looping for ever on `a: &a [*a]`, and `collect_inline`
materialising the expansion — so the fix bounds all three: inline arrays are
now bounded by the document's node count, exactly as block-backed arrays are
bounded by their block's length.

The moral is not that the first fix was wrong. It is that a fix aimed at one
function is a fix to one function, and only something that explores the whole
input space knows how many other ways in there are.

The second finding made the same point about the *shape* of a fix rather than
its location. Finding 1 had bounded LZ4 decompression by accumulating each
chunk's output against the block's declared size — correct, and one line too
late, because python-lz4's framing has each chunk declare its own size and
`lz4_flex` allocates from that declaration before decoding. Four bytes asked
for 4.28 GB.
