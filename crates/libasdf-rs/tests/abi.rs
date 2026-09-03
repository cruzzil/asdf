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
    exe.parent().and_then(Path::parent).expect("target/<profile>").to_path_buf()
}

/// The include directories build.rs exported.
fn include_dirs() -> Vec<PathBuf> {
    env!("ASDF_INCLUDE_DIRS").split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect()
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
    Command::new(c_compiler()).arg("--version").output().is_ok_and(|o| o.status.success())
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
        return Err(format!("compiling {name} failed:\n{}", String::from_utf8_lossy(&out.stderr)));
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

#define SHOW(type, field) printf(#type "." #field "=%zu\n", offsetof(type, field))
#define SHOW_TYPE(type) \
    printf(#type ".size=%zu\n", sizeof(type)); \
    printf(#type ".align=%zu\n", _Alignof(type))

int main(void) {
    SHOW_TYPE(asdf_version_t);
    SHOW(asdf_version_t, version);
    SHOW(asdf_version_t, major);
    SHOW(asdf_version_t, minor);
    SHOW(asdf_version_t, patch);
    SHOW(asdf_version_t, extra);

    /* Non-opaque and read directly by callers, so every offset matters. */
    SHOW_TYPE(asdf_ndarray_t);
    SHOW(asdf_ndarray_t, source);
    SHOW(asdf_ndarray_t, ndim);
    SHOW(asdf_ndarray_t, shape);
    SHOW(asdf_ndarray_t, datatype);
    SHOW(asdf_ndarray_t, byteorder);
    SHOW(asdf_ndarray_t, offset);
    SHOW(asdf_ndarray_t, strides);

    SHOW_TYPE(asdf_datatype_t);
    SHOW(asdf_datatype_t, type);
    SHOW(asdf_datatype_t, size);
    SHOW(asdf_datatype_t, name);
    SHOW(asdf_datatype_t, byteorder);
    SHOW(asdf_datatype_t, ndim);
    SHOW(asdf_datatype_t, shape);
    SHOW(asdf_datatype_t, nfields);
    SHOW(asdf_datatype_t, fields);

    /* Public iterator heads that the implementation casts to and from. */
    SHOW_TYPE(asdf_mapping_iter_t);
    SHOW(asdf_mapping_iter_t, key);
    SHOW(asdf_mapping_iter_t, value);
    SHOW_TYPE(asdf_sequence_iter_t);
    SHOW(asdf_sequence_iter_t, index);
    SHOW(asdf_sequence_iter_t, value);
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
    assert_eq!(got["asdf_version_t.version"], offset_of!(asdf_version_t, version));
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

/// A C program using the header's `_Generic` macros, not just its symbols.
///
/// `asdf_open` and the `ASDF_ERROR_*` family exist only as macros, and
/// `asdf_open_file` / `_fp` / `_mem` only as `static inline` wrappers. They are
/// part of the API a drop-in consumer compiles against, so they need a test
/// that goes through them rather than calling the underlying symbols directly.
#[test]
fn c_caller_can_use_the_header_macros() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    if shared_library().is_none() {
        eprintln!("skipping: shared library not built");
        return;
    }

    let src = r##"
#include <inttypes.h>
#include <stdio.h>
#include <string.h>
#include <asdf.h>

static const char TREE[] =
    "#ASDF 1.0.0\n"
    "#ASDF_STANDARD 1.6.0\n"
    "%YAML 1.1\n"
    "%TAG ! tag:stsci.edu:asdf/\n"
    "--- !core/asdf-1.1.0\n"
    "name: Dennis Richie\n"
    "foo: 42\n"
    "big: 5000000000\n"
    "flag: true\n"
    "nested:\n"
    "  inner: deep\n"
    "list: [a, b, c]\n"
    "...\n";

#define CHECK(cond, msg) \
    do { if (!(cond)) { fprintf(stderr, "FAIL: %s\n", msg); return 1; } } while (0)

int main(void) {
    /* asdf_open is a _Generic macro; this exercises its void* form. */
    asdf_file_t *file = asdf_open((const void *)TREE, sizeof(TREE) - 1);
    CHECK(file != NULL, "asdf_open returned NULL");

    const char *name = NULL;
    CHECK(asdf_get_string0(file, "name", &name) == ASDF_VALUE_OK, "get name");
    CHECK(strcmp(name, "Dennis Richie") == 0, "name value");

    int64_t foo = 0;
    CHECK(asdf_get_int64(file, "foo", &foo) == ASDF_VALUE_OK, "get foo");
    CHECK(foo == 42, "foo value");

    /* A value too large for the requested type must overflow, not truncate. */
    uint8_t small = 0;
    CHECK(asdf_get_uint8(file, "big", &small) == ASDF_VALUE_ERR_OVERFLOW, "overflow");

    bool flag = false;
    CHECK(asdf_get_bool(file, "flag", &flag) == ASDF_VALUE_OK, "get flag");
    CHECK(flag, "flag value");

    const char *inner = NULL;
    CHECK(asdf_get_string0(file, "nested/inner", &inner) == ASDF_VALUE_OK, "nested");
    CHECK(strcmp(inner, "deep") == 0, "nested value");

    const char *second = NULL;
    CHECK(asdf_get_string0(file, "list/1", &second) == ASDF_VALUE_OK, "indexed");
    CHECK(strcmp(second, "b") == 0, "indexed value");

    /* A missing path is NOT_FOUND, distinct from a type mismatch. */
    int64_t missing = 0;
    CHECK(asdf_get_int64(file, "nope", &missing) == ASDF_VALUE_ERR_NOT_FOUND, "missing");

    CHECK(asdf_is_mapping(file, "nested"), "nested is a mapping");
    CHECK(asdf_is_sequence(file, "list"), "list is a sequence");
    CHECK(asdf_is_string(file, "name"), "name is a string");

    asdf_value_t *root = asdf_get_value(file, "");
    CHECK(root != NULL, "get root value");
    CHECK(asdf_value_get_type(root) == ASDF_VALUE_MAPPING, "root is a mapping");
    const char *tag = asdf_value_tag(root);
    CHECK(tag != NULL, "root has a tag");
    CHECK(strcmp(tag, "tag:stsci.edu:asdf/core/asdf-1.1.0") == 0, "root tag");
    asdf_value_destroy(root);

    CHECK(asdf_error_code(file) == ASDF_ERR_NONE, "no error recorded");
    CHECK(asdf_block_count(file) == 0, "no blocks");

    asdf_close(file);
    printf("ok\n");
    return 0;
}
"##;
    let out = compile_and_run("c_macros", src, true).unwrap();
    assert_eq!(out.trim(), "ok");
}

/// libasdf's own README example, written in C against our headers.
///
/// Upstream documents this as the way to use the library: open a NULL file
/// for writing, set some metadata, write it out, then read it back. It goes
/// through `asdf_open`'s and `asdf_write_to`'s `_Generic` macros, so it is
/// about as close to a real consumer as a test can get.
#[test]
fn the_upstream_readme_example_works() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    if shared_library().is_none() {
        eprintln!("skipping: shared library not built");
        return;
    }

    let src = r##"
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <asdf.h>

#define CHECK(cond, msg) \
    do { if (!(cond)) { fprintf(stderr, "FAIL: %s\n", msg); return 1; } } while (0)

int main(void) {
    /* Open a "NULL" file for writing, as the README does. */
    asdf_file_t *file = asdf_open(NULL);
    CHECK(file != NULL, "asdf_open(NULL) returned NULL");

    CHECK(asdf_set_string0(file, "name", "Dennis Richie") == ASDF_VALUE_OK, "set name");
    CHECK(asdf_set_int64(file, "foo", 42) == ASDF_VALUE_OK, "set foo");

    /* A nested path materialises its parent mapping. */
    CHECK(asdf_set_uint64(file, "powers/squares", 1764) == ASDF_VALUE_OK, "set nested");
    CHECK(asdf_set_bool(file, "flag", true) == ASDF_VALUE_OK, "set flag");
    CHECK(asdf_set_double(file, "ratio", 1.5) == ASDF_VALUE_OK, "set ratio");
    CHECK(asdf_set_null(file, "nothing") == ASDF_VALUE_OK, "set null");

    /* asdf_write_to is a _Generic macro; this is its buffer form. */
    void *buf = NULL;
    size_t size = 0;
    CHECK(asdf_write_to(file, &buf, &size) == 0, "write to memory");
    CHECK(buf != NULL && size > 0, "empty output");

    /* What we wrote must be a well-formed ASDF file. */
    CHECK(memcmp(buf, "#ASDF ", 6) == 0, "missing ASDF header");
    asdf_close(file);

    /* Read it back through the same API. */
    asdf_file_t *readback = asdf_open((const void *)buf, size);
    CHECK(readback != NULL, "reopen failed");

    const char *name = NULL;
    CHECK(asdf_get_string0(readback, "name", &name) == ASDF_VALUE_OK, "get name");
    CHECK(strcmp(name, "Dennis Richie") == 0, "name round trip");

    int64_t foo = 0;
    CHECK(asdf_get_int64(readback, "foo", &foo) == ASDF_VALUE_OK, "get foo");
    CHECK(foo == 42, "foo round trip");

    uint64_t squares = 0;
    CHECK(asdf_get_uint64(readback, "powers/squares", &squares) == ASDF_VALUE_OK, "get nested");
    CHECK(squares == 1764, "nested round trip");

    bool flag = false;
    CHECK(asdf_get_bool(readback, "flag", &flag) == ASDF_VALUE_OK, "get flag");
    CHECK(flag, "flag round trip");

    double ratio = 0.0;
    CHECK(asdf_get_double(readback, "ratio", &ratio) == ASDF_VALUE_OK, "get ratio");
    CHECK(ratio == 1.5, "ratio round trip");

    CHECK(asdf_is_null(readback, "nothing"), "null round trip");

    /* The root must carry the core/asdf tag a valid tree requires. */
    asdf_value_t *root = asdf_get_value(readback, "");
    CHECK(root != NULL, "root value");
    const char *tag = asdf_value_tag(root);
    CHECK(tag != NULL && strncmp(tag, "tag:stsci.edu:asdf/core/asdf-", 29) == 0, "root tag");
    asdf_value_destroy(root);

    asdf_close(readback);
    free(buf);
    printf("ok\n");
    return 0;
}
"##;
    let out = compile_and_run("readme_example", src, true).unwrap();
    assert_eq!(out.trim(), "ok");
}

/// Reading a real array from C, the way libasdf's README read example does.
///
/// Writes a file with the Rust API, then reads its array back through the C
/// ndarray surface: `asdf_get_ndarray`, the public struct fields, the typed
/// element accessors and `asdf_ndarray_read_all`.
#[test]
fn c_caller_can_read_an_ndarray() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    if shared_library().is_none() {
        eprintln!("skipping: shared library not built");
        return;
    }

    // Build the input with the engine directly. Going through the `asdf`
    // crate would be more natural, but this package's own lib is also named
    // `asdf` (so the cdylib links as libasdf.so), and depending on both makes
    // the name ambiguous.
    let squares: Vec<u64> = (0..100u64).map(|i| i * i).collect();
    let tree = asdf_core::yaml::parse_document(
        "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n\
         name: Dennis Richie\n\
         powers:\n  squares: !core/ndarray-1.1.0\n    source: 0\n    \
         datatype: uint64\n    byteorder: little\n    shape: [100]\n...\n",
    )
    .unwrap();
    let mut writer = asdf_core::Writer::from_document(tree);
    writer.add_block(asdf_core::PendingBlock::new(
        squares.iter().flat_map(|v| v.to_le_bytes()).collect(),
    ));
    let bytes = writer.to_bytes().unwrap();

    let out_dir = target_dir().join("abi-tests");
    std::fs::create_dir_all(&out_dir).unwrap();
    let input = out_dir.join("squares.asdf");
    std::fs::write(&input, &bytes).unwrap();

    let src = format!(
        r##"
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <asdf.h>

#define CHECK(cond, msg) \
    do {{ if (!(cond)) {{ fprintf(stderr, "FAIL: %s\n", msg); return 1; }} }} while (0)

int main(void) {{
    asdf_file_t *file = asdf_open("{path}", "r");
    CHECK(file != NULL, "open failed");

    const char *name = NULL;
    CHECK(asdf_get_string0(file, "name", &name) == ASDF_VALUE_OK, "get name");
    CHECK(strcmp(name, "Dennis Richie") == 0, "name value");

    CHECK(asdf_is_ndarray(file, "powers/squares"), "is_ndarray");

    asdf_ndarray_t *squares = NULL;
    CHECK(asdf_get_ndarray(file, "powers/squares", &squares) == ASDF_VALUE_OK, "get ndarray");
    CHECK(squares != NULL, "null ndarray");

    /* The public struct fields a caller reads directly. */
    CHECK(squares->ndim == 1, "ndim");
    CHECK(squares->shape != NULL, "shape pointer");
    CHECK(squares->shape[0] == 100, "shape[0]");
    CHECK(squares->datatype.type == ASDF_DATATYPE_UINT64, "datatype");
    CHECK(asdf_ndarray_size(squares) == 100, "size");
    CHECK(asdf_ndarray_nbytes(squares) == 800, "nbytes");

    /* One element at a time. */
    uint64_t index = 10;
    asdf_ndarray_err_t err = ASDF_NDARRAY_OK;
    uint64_t value = asdf_ndarray_read_uint64_at(squares, &index, &err);
    CHECK(err == ASDF_NDARRAY_OK, "read_at error");
    CHECK(value == 100, "read_at value");

    /* Out of range must be reported, not guessed at. */
    index = 1000;
    (void)asdf_ndarray_read_uint64_at(squares, &index, &err);
    CHECK(err == ASDF_NDARRAY_ERR_OUT_OF_BOUNDS, "out of bounds");

    /* The whole array, as in the README's read example. */
    uint64_t *data = NULL;
    CHECK(asdf_ndarray_read_all(squares, ASDF_DATATYPE_UINT64, (void **)&data)
              == ASDF_NDARRAY_OK, "read_all");
    uint64_t sum = 0;
    for (uint64_t i = 0; i < asdf_ndarray_size(squares); i++) {{
        sum += data[i];
    }}
    /* Sum of i*i for i in 0..99 */
    CHECK(sum == 328350, "sum of squares");

    /* And converted to another type. */
    double *as_double = NULL;
    CHECK(asdf_ndarray_read_all(squares, ASDF_DATATYPE_FLOAT64, (void **)&as_double)
              == ASDF_NDARRAY_OK, "read_all converted");
    CHECK(as_double[10] == 100.0, "converted value");

    free(as_double);
    free(data);
    asdf_ndarray_destroy(squares);
    asdf_close(file);
    printf("ok\n");
    return 0;
}}
"##,
        path = input.display()
    );

    let out = compile_and_run("c_ndarray", &src, true).unwrap();
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
        assert!(text.contains(sym), "{sym} is missing from the shared library's dynamic symbols");
    }
}
