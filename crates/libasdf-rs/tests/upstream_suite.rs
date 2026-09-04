//! Tier 4: upstream libasdf's own C test suite, run against this library.
//!
//! This is the strongest conformance evidence the project can produce.
//! Everything else compares our output to a fixture we chose to compare
//! against; this compiles the tests *upstream wrote for its own
//! implementation*, links them against our shared library, and runs them.
//!
//! # What can and cannot run
//!
//! Upstream's tests fall into two groups. Most include only public
//! `asdf/*.h` headers and so link against any implementation of the ABI;
//! those are listed in [`SUITES`]. The rest reach into libasdf's private
//! headers -- `event.h`, `parser.h`, `stream.h`, `compat/numeric.h`, and
//! `libfyaml.h` itself -- to poke at internals that are implementation
//! detail rather than interface. Those cannot run against a different
//! implementation by construction, and are listed in [`INTERNAL_ONLY`] with
//! the header that rules each one out.
//!
//! # Requirements
//!
//! A libasdf checkout with its `tests/munit` submodule initialised:
//!
//! ```console
//! $ cd ~/code/libasdf && git submodule update --init tests/munit
//! ```
//!
//! Point at it with `LIBASDF_DIR`, and at the reference corpus with
//! `ASDF_STANDARD_DIR`. Without them the test skips with a note.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A suite that can be built against the public headers alone.
struct Suite {
    /// Basename of the `test-*.c` file.
    name: &'static str,
    /// Tests expected to pass today. Raise it as gaps close; a suite that
    /// passes *more* than this fails too, so the number stays honest.
    expect_pass: usize,
    /// Total tests in the suite, as munit counts them.
    total: usize,
}

/// The suites that build against the public ABI.
///
/// The counts are what this library achieves now, not what upstream's own
/// build achieves -- the point of pinning them is that a change which loses
/// ground fails here rather than going unnoticed.
const SUITES: &[Suite] = &[
    Suite { name: "test-version", expect_pass: 4, total: 4 },
    Suite { name: "test-tag", expect_pass: 1, total: 1 },
    Suite { name: "test-tests", expect_pass: 3, total: 3 },
    Suite { name: "test-error", expect_pass: 0, total: 3 },
    Suite { name: "test-time", expect_pass: 10, total: 17 },
    Suite { name: "test-core-extensions", expect_pass: 8, total: 16 },
    Suite { name: "test-extension", expect_pass: 7, total: 13 },
    Suite { name: "test-reference-files", expect_pass: 1, total: 113 },
];

/// Suites that cannot run against another implementation, and why.
const INTERNAL_ONLY: &[(&str, &str)] = &[
    ("test-block", "includes file.h, libasdf's private file struct"),
    ("test-compression", "includes compression/compression.h"),
    ("test-emitter", "includes emitter.h, file.h and stream.h"),
    ("test-event", "includes event.h and parser.h, the private event struct"),
    ("test-file", "includes file.h"),
    ("test-malloc-fail", "includes event.h, file.h, parser.h, tag.h, value.h"),
    ("test-ndarray", "includes compat/numeric.h"),
    ("test-parse-util", "includes parse_util.h and yaml.h"),
    ("test-parser", "includes event.h and parser.h"),
    ("test-stream", "includes stream.h and stream_intern.h"),
    ("test-value", "includes libfyaml.h directly"),
    ("test-value-util", "includes value_util.h"),
    ("test-yaml", "includes yaml.h"),
];

fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    exe.parent().and_then(Path::parent).expect("target/<profile>").to_path_buf()
}

fn include_dirs() -> Vec<PathBuf> {
    env!("ASDF_INCLUDE_DIRS").split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect()
}

fn shared_library_dir() -> Option<PathBuf> {
    let dir = target_dir();
    ["libasdf.so", "libasdf.dylib"].iter().any(|n| dir.join(n).is_file()).then_some(dir)
}

fn env_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    let path = std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))?;
    path.is_dir().then_some(path)
}

fn c_compiler() -> String {
    std::env::var("CC").unwrap_or_else(|_| "cc".to_string())
}

/// How many of a suite's tests munit reported as successful.
///
/// The summary line is `N of M (P%) tests successful, ...`.
fn parse_summary(output: &str) -> Option<(usize, usize)> {
    let line = output.lines().rev().find(|l| l.contains(") tests successful"))?;
    let mut words = line.split_whitespace();
    let passed: usize = words.next()?.parse().ok()?;
    words.next()?; // "of"
    let total: usize = words.next()?.parse().ok()?;
    Some((passed, total))
}

#[test]
fn upstream_c_test_suite_runs_against_this_library() {
    let Some(root) = env_dir("LIBASDF_DIR", "code/libasdf") else {
        eprintln!("skipping: LIBASDF_DIR not found");
        return;
    };
    let Some(standard) = env_dir("ASDF_STANDARD_DIR", "code/asdf-standard") else {
        eprintln!("skipping: ASDF_STANDARD_DIR not found");
        return;
    };
    let Some(lib_dir) = shared_library_dir() else {
        eprintln!("skipping: shared library not built");
        return;
    };
    let munit = root.join("tests/munit/munit.c");
    if !munit.is_file() {
        eprintln!(
            "skipping: {} not found; run `git submodule update --init tests/munit` in {}",
            munit.display(),
            root.display()
        );
        return;
    }

    let build = target_dir().join("upstream-suite");
    let temp = build.join("tmp");
    std::fs::create_dir_all(&temp).expect("create build dir");

    // `util.c` includes libasdf's build-time `config.h`, of which it reads
    // only `HAVE_STATGRAB`; leaving it undefined selects the portable path.
    std::fs::write(
        build.join("config.h"),
        "/* Stand-in for libasdf's build config. Only HAVE_STATGRAB is read,\n\
           and leaving it undefined selects util.c's portable path. */\n",
    )
    .expect("write config.h");

    let tests = root.join("tests");
    let mut cflags: Vec<String> = vec!["-std=gnu11".into()];
    for dir in [build.clone(), tests.clone()].into_iter().chain(include_dirs()) {
        cflags.push("-I".into());
        cflags.push(dir.display().to_string());
    }
    cflags
        .push(format!("-DREFERENCE_FILES_DIR=\"{}\"", standard.join("reference_files").display()));
    cflags.push(format!("-DFIXTURES_DIR=\"{}\"", tests.join("fixtures").display()));
    cflags.push(format!("-DTEMP_DIR=\"{}\"", temp.display()));

    let compile = |source: &Path, object: &Path| -> Result<(), String> {
        let out = Command::new(c_compiler())
            .args(&cflags)
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(object)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    };

    // The harness objects, shared by every suite.
    let munit_o = build.join("munit.o");
    let util_o = build.join("util.o");
    if let Err(e) = compile(&munit, &munit_o) {
        eprintln!("skipping: munit did not compile:\n{e}");
        return;
    }
    compile(&tests.join("util.c"), &util_o).expect("upstream's test util.c must compile");

    let mut results = Vec::new();
    let mut failures = Vec::new();

    for suite in SUITES {
        let object = build.join(format!("{}.o", suite.name));
        let binary = build.join(suite.name);
        if let Err(e) = compile(&tests.join(format!("{}.c", suite.name)), &object) {
            failures.push(format!("{}: did not compile:\n{e}", suite.name));
            continue;
        }

        let link = Command::new(c_compiler())
            .arg(&object)
            .arg(&munit_o)
            .arg(&util_o)
            .arg("-o")
            .arg(&binary)
            .arg("-L")
            .arg(&lib_dir)
            .arg("-lasdf")
            .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
            .output()
            .expect("run the linker");
        if !link.status.success() {
            failures.push(format!(
                "{}: did not link:\n{}",
                suite.name,
                String::from_utf8_lossy(&link.stderr)
            ));
            continue;
        }

        // munit forks per test, so one failing test does not stop the rest.
        let run = Command::new(&binary).current_dir(&build).output().expect("run the suite");
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        let Some((passed, total)) = parse_summary(&output) else {
            failures.push(format!(
                "{}: no munit summary in its output:\n{}",
                suite.name,
                output.lines().rev().take(5).collect::<Vec<_>>().join("\n")
            ));
            continue;
        };

        results.push((suite.name, passed, total));
        if total != suite.total {
            failures.push(format!(
                "{}: upstream now has {total} tests, not {}; update the table",
                suite.name, suite.total
            ));
        } else if passed < suite.expect_pass {
            failures.push(format!(
                "{}: {passed} of {total} pass, down from {}",
                suite.name, suite.expect_pass
            ));
        } else if passed > suite.expect_pass {
            failures.push(format!(
                "{}: {passed} of {total} pass, up from {} -- raise expect_pass",
                suite.name, suite.expect_pass
            ));
        }
    }

    let passed: usize = results.iter().map(|(_, p, _)| p).sum();
    let total: usize = results.iter().map(|(_, _, t)| t).sum();
    eprintln!("upstream C suite: {passed} of {total} tests pass across {} suites", results.len());
    for (name, p, t) in &results {
        eprintln!("  {name}: {p}/{t}");
    }
    eprintln!(
        "  ({} further suites need libasdf's private headers and cannot run \
         against another implementation)",
        INTERNAL_ONLY.len()
    );

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
