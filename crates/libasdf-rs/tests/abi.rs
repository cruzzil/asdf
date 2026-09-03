//! ABI conformance gates.
//!
//! These are the tests that Rust-side unit tests structurally cannot replace:
//! they compile real C against the vendored headers, link the real shared
//! library, and check the things a C caller depends on -- struct layouts,
//! enum discriminants, and the exported symbol namespace.
//!
//! They are wired up in phase 0, long before the API is complete, so that
//! every later phase gets this feedback from its first commit rather than
//! discovering layout problems at integration time.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where cargo put the built artifacts.
fn target_dir() -> PathBuf {
    // The test binary lives at <target>/<profile>/deps/<name>-<hash>.
    let exe = std::env::current_exe().expect("current exe");
    exe.parent()
        .and_then(Path::parent)
        .expect("target/<profile>")
        .to_path_buf()
}

/// The include directories build.rs exported.
fn include_dirs() -> Vec<PathBuf> {
    env!("ASDF_INCLUDE_DIRS")
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn shared_library() -> Option<PathBuf> {
    let dir = target_dir();
    for name in ["libasdf.so", "libasdf.dylib"] {
        let path = dir.join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn c_compiler() -> String {
    std::env::var("CC").unwrap_or_else(|_| "cc".to_string())
}

fn have_c_compiler() -> bool {
    Command::new(c_compiler())
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Compile and run a C program against the vendored headers.
///
/// Returns the program's stdout. `link` controls whether the shared library
/// is linked in -- a pure compile-time assertion program does not need it.
fn compile_and_run(name: &str, source: &str, link: bool) -> Result<String, String> {
    let out_dir = target_dir().join("abi-tests");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    let src = out_dir.join(format!("{name}.c"));
    let bin = out_dir.join(name);
    std::fs::write(&src, source).map_err(|e| e.to_string())?;

    let mut cmd = Command::new(c_compiler());
    cmd.arg("-std=c11").arg("-Wall").arg("-Wextra").arg("-Werror");
    for dir in include_dirs() {
        cmd.arg("-I").arg(dir);
    }
    cmd.arg(&src).arg("-o").arg(&bin);

    if link {
        let lib = shared_library().ok_or("no shared library built")?;
        let libdir = lib.parent().unwrap();
        cmd.arg("-L").arg(libdir).arg("-lasdf");
        cmd.arg(format!("-Wl,-rpath,{}", libdir.display()));
    }

    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "compiling {name} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let run = Command::new(&bin).output().map_err(|e| e.to_string())?;
    if !run.status.success() {
        return Err(format!(
            "running {name} failed ({}):\n{}{}",
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&run.stdout).into_owned())
}

/// The vendored headers must be self-contained and warning-clean.
///
/// This catches a bad re-vendoring or a broken generated `config.h` before
/// anything else has a chance to fail confusingly.
#[test]
fn headers_compile_standalone() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let src = r#"
#include <asdf.h>
#include <asdf/block.h>
#include <asdf/emitter.h>
#include <asdf/error.h>
#include <asdf/event.h>
#include <asdf/extension.h>
#include <asdf/extension_util.h>
#include <asdf/file.h>
#include <asdf/log.h>
#include <asdf/parser.h>
#include <asdf/util.h>
#include <asdf/value.h>
#include <asdf/version.h>
#include <asdf/yaml.h>
#include <asdf/core/asdf.h>
#include <asdf/core/datatype.h>
#include <asdf/core/extension_metadata.h>
#include <asdf/core/history_entry.h>
#include <asdf/core/ndarray.h>
#include <asdf/core/software.h>
#include <asdf/core/time.h>

int main(void) { return 0; }
"#;
    compile_and_run("headers_standalone", src, false).unwrap();
}

/// Enum discriminants are baked into compiled C callers, so a reorder is an
/// ABI break. This asserts the values the headers define, from C.
#[test]
fn enum_discriminants_are_stable() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let src = r#"
#include <assert.h>
#include <asdf.h>

int main(void) {
    /* asdf_error_code_t */
    _Static_assert(ASDF_ERR_NONE == 0, "");
    _Static_assert(ASDF_ERR_UNKNOWN_STATE == 1, "");
    _Static_assert(ASDF_ERR_OUT_OF_MEMORY == 10, "");
    _Static_assert(ASDF_ERR_SYSTEM == 11, "");
    _Static_assert(ASDF_ERR_OVER_LIMIT == 16, "");

    /* asdf_value_err_t runs negative through positive */
    _Static_assert(ASDF_VALUE_ERR_UNKNOWN == -2, "");
    _Static_assert(ASDF_VALUE_ERR_NOT_FOUND == -1, "");
    _Static_assert(ASDF_VALUE_OK == 0, "");
    _Static_assert(ASDF_VALUE_ERR_TYPE_MISMATCH == 1, "");

    /* asdf_value_type_t */
    _Static_assert(ASDF_VALUE_UNKNOWN == 0, "");
    _Static_assert(ASDF_VALUE_SEQUENCE == 1, "");
    _Static_assert(ASDF_VALUE_MAPPING == 2, "");
    _Static_assert(ASDF_VALUE_SCALAR == 3, "");
    _Static_assert(ASDF_VALUE_STRING == 4, "");

    /* asdf_byteorder_t uses character literals, not a sequence */
    _Static_assert(ASDF_BYTEORDER_INVALID == -1, "");
    _Static_assert(ASDF_BYTEORDER_DEFAULT == 0, "");
    _Static_assert(ASDF_BYTEORDER_BIG == '>', "");
    _Static_assert(ASDF_BYTEORDER_LITTLE == '<', "");

    /* asdf_scalar_datatype_t */
    _Static_assert(ASDF_DATATYPE_UNKNOWN == 0, "");
    _Static_assert(ASDF_DATATYPE_INT8 == 1, "");
    _Static_assert(ASDF_DATATYPE_STRUCTURED == 17, "");

    /* Log levels */
    _Static_assert(ASDF_LOG_NONE == 0, "");
    _Static_assert(ASDF_LOG_FATAL == 6, "");

    /* Flag enums are bit positions, not sequences */
    _Static_assert(ASDF_PARSER_OPT_EMIT_YAML_EVENTS == 1, "");
    _Static_assert(ASDF_PARSER_OPT_BUFFER_TREE == 2, "");
    _Static_assert(ASDF_EMITTER_OPT_EMIT_EMPTY == 2, "");

    /* Block header constants from the specification */
    _Static_assert(ASDF_BLOCK_CHECKSUM_DIGEST_SIZE == 16, "");

    return 0;
}
"#;
    compile_and_run("enum_discriminants", src, false).unwrap();
}

/// Public, non-opaque struct layouts, printed from C and compared against
/// what the Rust side believes.
///
/// `asdf_version_t` is the one fully implemented so far; the others are added
/// as their phases land.
#[test]
fn public_struct_layouts_match() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let src = r#"
#include <stddef.h>
#include <stdio.h>
#include <asdf.h>
#include <asdf/version.h>

int main(void) {
    printf("asdf_version_t.size=%zu\n", sizeof(asdf_version_t));
    printf("asdf_version_t.align=%zu\n", _Alignof(asdf_version_t));
    printf("asdf_version_t.version=%zu\n", offsetof(asdf_version_t, version));
    printf("asdf_version_t.major=%zu\n", offsetof(asdf_version_t, major));
    printf("asdf_version_t.minor=%zu\n", offsetof(asdf_version_t, minor));
    printf("asdf_version_t.patch=%zu\n", offsetof(asdf_version_t, patch));
    printf("asdf_version_t.extra=%zu\n", offsetof(asdf_version_t, extra));
    return 0;
}
"#;
    let out = compile_and_run("struct_layouts", src, false).unwrap();

    let mut got = std::collections::HashMap::new();
    for line in out.lines() {
        if let Some((k, v)) = line.split_once('=') {
            got.insert(k.to_string(), v.parse::<usize>().unwrap());
        }
    }

    // The lib is named `asdf` so the cdylib links as `libasdf.so`.
    use asdf::asdf_version_t;
    use std::mem::{align_of, offset_of, size_of};

    assert_eq!(got["asdf_version_t.size"], size_of::<asdf_version_t>());
    assert_eq!(got["asdf_version_t.align"], align_of::<asdf_version_t>());
    assert_eq!(
        got["asdf_version_t.version"],
        offset_of!(asdf_version_t, version)
    );
    assert_eq!(got["asdf_version_t.major"], offset_of!(asdf_version_t, major));
    assert_eq!(got["asdf_version_t.minor"], offset_of!(asdf_version_t, minor));
    assert_eq!(got["asdf_version_t.patch"], offset_of!(asdf_version_t, patch));
    assert_eq!(got["asdf_version_t.extra"], offset_of!(asdf_version_t, extra));
}

/// A real C program calling into the library, end to end.
#[test]
fn c_caller_can_use_the_library() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    if shared_library().is_none() {
        eprintln!("skipping: shared library not built (run `cargo build` first)");
        return;
    }

    let src = r#"
#include <stdio.h>
#include <string.h>
#include <asdf/version.h>

int main(void) {
    asdf_version_t *v = asdf_version_parse("1.6.0");
    if (!v) { fprintf(stderr, "parse returned NULL\n"); return 1; }
    if (v->major != 1 || v->minor != 6 || v->patch != 0) {
        fprintf(stderr, "wrong version: %u.%u.%u\n", v->major, v->minor, v->patch);
        return 1;
    }
    if (strcmp(v->version, "1.6.0") != 0) {
        fprintf(stderr, "wrong version string: %s\n", v->version);
        return 1;
    }
    if (v->extra != NULL) { fprintf(stderr, "unexpected extra\n"); return 1; }

    /* The PEP-440 style suffix upstream documents. */
    asdf_version_t *d = asdf_version_parse("0.1.0.dev4");
    if (!d || !d->extra || strcmp(d->extra, "dev4") != 0) {
        fprintf(stderr, "extra not parsed\n");
        return 1;
    }

    /* A deep copy must survive the original being freed. */
    asdf_version_t *c = asdf_version_copy(d);
    asdf_version_destroy(d);
    if (!c || strcmp(c->version, "0.1.0.dev4") != 0) {
        fprintf(stderr, "copy not independent\n");
        return 1;
    }

    asdf_version_destroy(c);
    asdf_version_destroy(v);
    asdf_version_destroy(NULL);  /* must be a no-op */

    printf("ok\n");
    return 0;
}
"#;
    let out = compile_and_run("c_caller", src, true).unwrap();
    assert_eq!(out.trim(), "ok");
}

/// Upstream's `tests/test-symbol-leakage.sh`, ported.
///
/// The shared library must export nothing outside libasdf's own namespace.
/// Upstream added this after a vendored third-party copy clobbered
/// libasdf-gwcs in the same process.
#[test]
fn exports_only_the_asdf_namespace() {
    let Some(lib) = shared_library() else {
        eprintln!("skipping: shared library not built (run `cargo build` first)");
        return;
    };

    let out = match Command::new("nm").arg("-D").arg("--defined-only").arg(&lib).output() {
        Ok(o) if o.status.success() => o,
        _ => {
            eprintln!("skipping: nm unavailable or failed");
            return;
        }
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let mut leaked = Vec::new();
    let mut exported = 0;

    for line in text.lines() {
        let Some(sym) = line.split_whitespace().last() else { continue };
        // Strip the version tag nm appends, and the Mach-O leading underscore.
        let sym = sym.split("@@").next().unwrap_or(sym);
        let sym = sym.strip_prefix('_').unwrap_or(sym);
        if sym.is_empty() {
            continue;
        }
        exported += 1;
        if !(sym.starts_with("asdf_") || sym.starts_with("ASDF_") || sym.starts_with("libasdf_")) {
            leaked.push(sym.to_string());
        }
    }

    assert!(exported > 0, "nm reported no exported symbols");
    leaked.sort();
    leaked.dedup();
    assert!(
        leaked.is_empty(),
        "{} exports {} symbols outside the asdf_ namespace:\n    {}",
        lib.display(),
        leaked.len(),
        leaked.join("\n    ")
    );
    eprintln!("{exported} exported symbols, all inside the asdf_ namespace");
}

/// The variadic and `_Float16` entry points must survive linking.
///
/// They live in `shim.c` and nothing in the Rust code references them, so
/// without `+whole-archive` and our own version script the linker drops them
/// silently. This test is what catches that regression.
#[test]
fn shim_entry_points_are_exported() {
    let Some(lib) = shared_library() else {
        eprintln!("skipping: shared library not built");
        return;
    };
    let out = match Command::new("nm").arg("-D").arg("--defined-only").arg(&lib).output() {
        Ok(o) if o.status.success() => o,
        _ => {
            eprintln!("skipping: nm unavailable");
            return;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);

    let mut required = vec![
        "asdf_file_log",
        "asdf_file_error_common",
        "asdf_value_error_common",
        "asdf_file_error_oom",
        "asdf_value_error_oom",
        "asdf_file_error_system",
        "asdf_value_error_system",
    ];
    // Declared only when the target supports the type.
    if cfg!(asdf_have_float16) {
        required.push("asdf_ndarray_read_float16_at");
    }

    for sym in required {
        assert!(
            text.contains(sym),
            "{sym} is missing from the shared library's dynamic symbols"
        );
    }
}
