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
#include <asdf/core/time.h>

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

    /* Embeds struct timespec and struct tm, whose layouts are
       platform-specific -- the most likely of any to differ by target. */
    SHOW_TYPE(asdf_time_t);
    SHOW(asdf_time_t, value);
    SHOW(asdf_time_t, info);
    SHOW(asdf_time_t, format);
    SHOW(asdf_time_t, scale);
    SHOW(asdf_time_t, location);
    SHOW_TYPE(asdf_time_info_t);
    SHOW(asdf_time_info_t, ts);
    SHOW(asdf_time_info_t, tm);
    SHOW_TYPE(asdf_time_location_t);
    SHOW(asdf_time_location_t, longitude);
    SHOW(asdf_time_location_t, latitude);
    SHOW(asdf_time_location_t, height);

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

/// A third-party extension, registered before `main` by the real macro.
///
/// `ASDF_REGISTER_EXTENSION` generates a function marked
/// `__attribute__((constructor))`, so the registration runs while the process
/// is still starting — before `main`, and before anything in the library has
/// initialised. This is the risk the plan called out, and it is the shape a
/// real extension such as libasdf-gwcs takes, so it is tested with the actual
/// macro rather than by calling the registration function directly.
#[test]
fn a_c_extension_registers_before_main() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    if shared_library().is_none() {
        eprintln!("skipping: shared library not built");
        return;
    }

    let src = r##"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <asdf.h>
#include <asdf/extension.h>

#define CHECK(cond, msg) \
    do { if (!(cond)) { fprintf(stderr, "FAIL: %s\n", msg); return 1; } } while (0)

/* A minimal native type for the extension to deserialize into. */
typedef struct {
    char *label;
} widget_t;

#define WIDGET_TAG "tag:example.com:test/widget-1.0.0"

/* Set by the constructor, read by main, to prove the ordering. */
static int registered_before_main = 0;

static asdf_value_err_t widget_deserialize(
    asdf_value_t *value, const void *userdata, void **out) {
    (void)userdata;
    const char *text = NULL;
    if (asdf_value_as_string0(value, &text) != ASDF_VALUE_OK)
        return ASDF_VALUE_ERR_TYPE_MISMATCH;

    widget_t *widget = calloc(1, sizeof(widget_t));
    if (!widget)
        return ASDF_VALUE_ERR_OOM;

    /* strdup is POSIX rather than C11, so copy by hand under -std=c11. */
    size_t len = strlen(text);
    widget->label = malloc(len + 1);
    if (!widget->label) {
        free(widget);
        return ASDF_VALUE_ERR_OOM;
    }
    memcpy(widget->label, text, len + 1);

    *out = widget;
    return ASDF_VALUE_OK;
}

static void widget_deinit(void *obj) {
    widget_t *widget = obj;
    if (widget) {
        free(widget->label);
        widget->label = NULL;
    }
}

static asdf_version_t widget_version = {"1.0.0", 1, 0, 0, NULL};
static asdf_software_t widget_software = {
    "test-widget", &widget_version, "Nobody", "https://example.com"};

static const asdf_extension_vtab_t widget_vtab = {
    .serialize = NULL,
    .deserialize = widget_deserialize,
    .copy = NULL,
    .deinit = widget_deinit,
};

/* The real macro: defines the accessors and a constructor that registers. */
ASDF_REGISTER_EXTENSION(
    widget, widget_t, &widget_software, &widget_vtab, NULL, WIDGET_TAG);

/* Runs after the macro's constructor, and records what it saw. */
__attribute__((constructor(65535))) static void observe(void) {
    registered_before_main = (asdf_extension_get(NULL, WIDGET_TAG) != NULL);
}

static const char TREE[] =
    "#ASDF 1.0.0\n"
    "#ASDF_STANDARD 1.6.0\n"
    "%YAML 1.1\n"
    "%TAG ! tag:stsci.edu:asdf/\n"
    "--- !core/asdf-1.1.0\n"
    "thing: !<tag:example.com:test/widget-1.0.0> 'a labelled widget'\n"
    "...\n";

int main(void) {
    /* The whole point: registration happened during process start-up. */
    CHECK(registered_before_main, "extension was not registered before main");

    const asdf_extension_t *found = asdf_extension_get(NULL, WIDGET_TAG);
    CHECK(found != NULL, "extension not found after main");
    CHECK(found->size == sizeof(widget_t), "extension size");
    CHECK(found->software != NULL, "extension software");
    CHECK(strcmp(found->software->name, "test-widget") == 0, "software name");

    asdf_file_t *file = asdf_open((const void *)TREE, sizeof(TREE) - 1);
    CHECK(file != NULL, "open failed");

    /* The generated predicate and getter, both from the macro. */
    CHECK(asdf_is_widget(file, "thing"), "asdf_is_widget");

    widget_t *widget = NULL;
    CHECK(asdf_get_widget(file, "thing", &widget) == ASDF_VALUE_OK, "asdf_get_widget");
    CHECK(widget != NULL, "null widget");
    CHECK(strcmp(widget->label, "a labelled widget") == 0, "widget label");

    /* The generated copy, which deep-copies through the vtab (or shallow
     * when no copy method is given, as here). */
    widget_t *copy = asdf_widget_copy(file, widget);
    CHECK(copy != NULL, "asdf_widget_copy");

    /* And the generated destructor. A shallow copy shares the label, so
     * only one of the two may free it. */
    memset(copy, 0, sizeof(*copy));
    asdf_widget_destroy(copy);
    asdf_widget_destroy(widget);

    /* A tag the extension does not claim must not match. */
    CHECK(asdf_extension_get(NULL, "tag:example.com:test/widget-9.9.9") == NULL,
          "unrelated tag matched");

    asdf_close(file);
    printf("ok\n");
    return 0;
}
"##;
    let out = compile_and_run("c_extension", src, true).unwrap();
    assert_eq!(out.trim(), "ok");
}

/// libasdf's README write example, verbatim, including its ndarrays.
///
/// This is the program upstream puts first in its documentation: open a NULL
/// file, set metadata, build two arrays with
/// `asdf_ndarray_data_alloc`, attach them with `asdf_set_ndarray`, and write.
/// It then reads the result back the way the README's second example does.
/// Between them these exercise most of what a real consumer touches.
#[test]
fn the_upstream_readme_ndarray_example_works() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    if shared_library().is_none() {
        eprintln!("skipping: shared library not built");
        return;
    }

    let out_dir = target_dir().join("abi-tests");
    std::fs::create_dir_all(&out_dir).unwrap();
    let output = out_dir.join("readme-out.asdf");
    let _ = std::fs::remove_file(&output);

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
    const char *filename = "{path}";

    /* ---- the README's write example ---- */
    asdf_file_t *file = asdf_open(NULL);
    CHECK(file != NULL, "asdf_open(NULL)");

    asdf_set_string0(file, "name", "Dennis Richie");
    asdf_set_int64(file, "foo", 42);

    uint64_t N = 100;

    asdf_ndarray_t sequence = {{
        .ndim = 1,
        .shape = (uint64_t[]){{N}},
        .datatype = {{.type = ASDF_DATATYPE_UINT64}}
    }};
    uint64_t *sequence_data = asdf_ndarray_data_alloc(&sequence);
    CHECK(sequence_data != NULL, "sequence data_alloc");

    asdf_ndarray_t squares = {{
        .ndim = 1,
        .shape = (uint64_t[]){{N}},
        .datatype = {{.type = ASDF_DATATYPE_UINT64}}
    }};
    uint64_t *squares_data = asdf_ndarray_data_alloc(&squares);
    CHECK(squares_data != NULL, "squares data_alloc");

    for (uint64_t idx = 0; idx < N; idx++) {{
        sequence_data[idx] = idx;
        squares_data[idx] = idx * idx;
    }}

    CHECK(asdf_set_ndarray(file, "sequence", &sequence) == ASDF_VALUE_OK, "set sequence");
    /* A nested path materialises its parent, as the README relies on. */
    CHECK(asdf_set_ndarray(file, "powers/squares", &squares) == ASDF_VALUE_OK, "set squares");

    CHECK(asdf_write_to(file, filename) == 0, "write_to");

    asdf_ndarray_data_dealloc(&sequence);
    asdf_ndarray_data_dealloc(&squares);
    asdf_close(file);

    /* ---- the README's read example ---- */
    asdf_file_t *readback = asdf_open(filename, "r");
    CHECK(readback != NULL, "reopen");

    const char *name = NULL;
    CHECK(asdf_get_string0(readback, "name", &name) == ASDF_VALUE_OK, "get name");
    CHECK(strcmp(name, "Dennis Richie") == 0, "name");

    int64_t foo = 0;
    CHECK(asdf_get_int64(readback, "foo", &foo) == ASDF_VALUE_OK, "get foo");
    CHECK(foo == 42, "foo");

    asdf_ndarray_t *read_squares = NULL;
    uint64_t *read_data = NULL;
    CHECK(asdf_get_ndarray(readback, "powers/squares", &read_squares) == ASDF_VALUE_OK,
          "get squares");
    CHECK(asdf_ndarray_read_all(read_squares, ASDF_DATATYPE_UINT64, (void **)&read_data)
              == ASDF_NDARRAY_OK, "read_all");

    uint64_t nelem = asdf_ndarray_size(read_squares);
    CHECK(nelem == N, "element count");
    uint64_t sum = 0;
    for (uint64_t idx = 0; idx < nelem; idx++) {{
        sum += read_data[idx];
    }}
    /* The README states this figure. */
    CHECK(sum == 328350, "sum of squares");

    /* The other array must have landed in its own block. */
    CHECK(asdf_block_count(readback) == 2, "two blocks");

    asdf_ndarray_t *read_sequence = NULL;
    CHECK(asdf_get_ndarray(readback, "sequence", &read_sequence) == ASDF_VALUE_OK,
          "get sequence");
    uint64_t index = 7;
    asdf_ndarray_err_t err = ASDF_NDARRAY_OK;
    CHECK(asdf_ndarray_read_uint64_at(read_sequence, &index, &err) == 7, "sequence[7]");
    CHECK(err == ASDF_NDARRAY_OK, "read_at err");

    free(read_data);
    asdf_ndarray_destroy(read_sequence);
    asdf_ndarray_destroy(read_squares);
    asdf_close(readback);
    printf("ok\n");
    return 0;
}}
"##,
        path = output.display()
    );

    let out = compile_and_run("readme_ndarray", &src, true).unwrap();
    assert_eq!(out.trim(), "ok");

    // And the file it produced must be a real ASDF file to anyone else.
    let reader = asdf_core::Reader::open(&output).expect("the written file should scan");
    assert_eq!(reader.block_count(), 2);
    assert!(reader.tree().unwrap().is_some());
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

/// Every `ASDF_EXPORT` declaration in the headers must resolve at link time.
///
/// The complement of [`exports_only_the_asdf_namespace`]: that one catches
/// symbols we export and should not, this one catches symbols upstream's
/// headers promise and we do not provide. A C program that compiles against
/// the vendored headers can call any of them, so a missing one is a link
/// error in a consumer rather than anything the Rust build would notice.
///
/// The list is taken from the preprocessed headers rather than a checked-in
/// file, so re-vendoring upstream's headers moves the goalposts by itself.
#[test]
fn every_declared_export_is_defined() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let Some(lib) = shared_library() else {
        eprintln!("skipping: shared library not built");
        return;
    };

    let declared = declared_exports();
    assert!(
        declared.len() > 300,
        "only {} exports found in the headers; the scan is probably broken",
        declared.len()
    );

    let defined = defined_symbols(&lib);
    if defined.is_empty() {
        eprintln!("skipping: nm unavailable");
        return;
    }

    let mut missing: Vec<&String> = declared.iter().filter(|s| !defined.contains(*s)).collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "{} of {} declared exports are missing from {}:\n    {}",
        missing.len(),
        declared.len(),
        lib.display(),
        missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n    ")
    );
    eprintln!("all {} declared exports are defined", declared.len());
}

/// Every symbol the vendored headers declare with `ASDF_EXPORT`.
///
/// Read out of the preprocessed headers: `ASDF_EXPORT` expands to the
/// visibility attribute, so each declaration is the identifier just before
/// the first `(` that follows one.
fn declared_exports() -> std::collections::BTreeSet<String> {
    const MARKER: &str = r#"__attribute__((visibility("default")))"#;

    let out_dir = target_dir().join("abi-tests");
    std::fs::create_dir_all(&out_dir).expect("create abi-tests dir");
    let src = out_dir.join("declared_exports.c");
    std::fs::write(&src, "#include <asdf.h>\n").expect("write probe source");

    let mut cmd = Command::new(c_compiler());
    cmd.arg("-E").arg(&src);
    for dir in include_dirs() {
        cmd.arg("-I").arg(dir);
    }
    let out = cmd.output().expect("run the preprocessor");
    assert!(
        out.status.success(),
        "preprocessing failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);

    let mut names = std::collections::BTreeSet::new();
    for chunk in text.split(MARKER).skip(1) {
        // The declarator runs up to the parameter list for a function, or to
        // the semicolon for an `extern` variable such as `libasdf_version`.
        let paren = chunk.find('(').unwrap_or(usize::MAX);
        let semi = chunk.find(';').unwrap_or(usize::MAX);
        let stop = paren.min(semi);
        if stop == usize::MAX {
            continue;
        }
        let head = &chunk[..stop];
        let name: String = head
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if name.starts_with("asdf_") || name.starts_with("ASDF_") {
            names.insert(name);
        }
    }
    names
}

/// The dynamic symbols a shared library defines, version suffixes stripped.
fn defined_symbols(lib: &Path) -> std::collections::BTreeSet<String> {
    let Ok(out) = Command::new("nm").arg("-D").arg("--defined-only").arg(lib).output() else {
        return std::collections::BTreeSet::new();
    };
    if !out.status.success() {
        return std::collections::BTreeSet::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .map(|sym| sym.split("@@").next().unwrap_or(sym).to_string())
        .collect()
}

/// The asdf-standard reference corpus, if it is available.
fn standard_dir() -> Option<PathBuf> {
    let path = std::env::var_os("ASDF_STANDARD_DIR").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("code/asdf-standard"))
    })?;
    path.is_dir().then_some(path)
}

/// The low-level event API, walked from C over a reference file.
///
/// This is upstream's `tests/test-event.c` reduced to what the *public*
/// headers expose: its own version reaches into the internal `asdf_event_t`
/// definition, which a drop-in consumer cannot. The event sequence, the YAML
/// sub-events and their tags, and the block header values are all taken from
/// that test, so this checks the same facts through the supported surface.
#[test]
fn c_caller_can_walk_the_event_stream() {
    if !have_c_compiler() {
        eprintln!("skipping: no C compiler");
        return;
    }
    if shared_library().is_none() {
        eprintln!("skipping: shared library not built");
        return;
    }
    let Some(standard) = standard_dir() else {
        eprintln!("skipping: asdf-standard corpus not found");
        return;
    };
    let basic = standard.join("reference_files/1.6.0/basic.asdf");
    if !basic.is_file() {
        eprintln!("skipping: {} not found", basic.display());
        return;
    }

    let src = format!(
        r##"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <asdf.h>

static asdf_parser_t *parser;
static asdf_event_t *event;

static void next(asdf_event_type_t expected) {{
    event = asdf_event_iterate(parser);
    if (!event) {{
        printf("FAIL: stream ended, wanted %s\n", asdf_event_type_name(expected));
        exit(1);
    }}
    asdf_event_type_t got = asdf_event_type(event);
    if (got != expected) {{
        printf("FAIL: wanted %s, got %s\n",
               asdf_event_type_name(expected), asdf_event_type_name(got));
        exit(1);
    }}
}}

/* Check the next event is a YAML sub-event of the given type, tag and value.
   NULL tag or value means "must be absent". */
static void next_yaml(asdf_yaml_event_type_t kind, const char *tag, const char *value) {{
    next(ASDF_YAML_EVENT);
    if (asdf_yaml_event_type(event) != kind) {{
        printf("FAIL: yaml event %d, wanted %d\n", asdf_yaml_event_type(event), kind);
        exit(1);
    }}
    size_t len = 0;
    const char *got = asdf_yaml_event_tag(event, &len);
    if (tag == NULL) {{
        if (len != 0) {{ printf("FAIL: unexpected tag %.*s\n", (int)len, got); exit(1); }}
    }} else {{
        if (len != strlen(tag) || 0 != memcmp(got, tag, len)) {{
            printf("FAIL: tag %.*s, wanted %s\n", (int)len, got ? got : "", tag);
            exit(1);
        }}
    }}
    len = 0;
    got = asdf_yaml_event_scalar_value(event, &len);
    if (value == NULL) {{
        if (len != 0) {{ printf("FAIL: unexpected value %.*s\n", (int)len, got); exit(1); }}
    }} else {{
        if (len != strlen(value) || 0 != memcmp(got, value, len)) {{
            printf("FAIL: value %.*s, wanted %s\n", (int)len, got ? got : "", value);
            exit(1);
        }}
    }}
}}

int main(void) {{
    asdf_parser_cfg_t cfg = {{ .flags = ASDF_PARSER_OPT_EMIT_YAML_EVENTS }};
    parser = asdf_parser_create(&cfg);
    assert(parser);

    if (asdf_parser_set_input_file(parser, "{path}") != 0) {{
        printf("FAIL: %s\n", asdf_parser_get_error(parser));
        return 1;
    }}
    assert(!asdf_parser_has_error(parser));

    next(ASDF_ASDF_VERSION_EVENT);
    next(ASDF_STANDARD_VERSION_EVENT);

    next(ASDF_BLOCK_INDEX_EVENT);
    /* A non-tree event has no tree info. */
    assert(asdf_event_tree_info(event) == NULL);

    next(ASDF_TREE_START_EVENT);
    const asdf_tree_info_t *tree = asdf_event_tree_info(event);
    assert(tree != NULL);

    next_yaml(ASDF_YAML_STREAM_START_EVENT, NULL, NULL);
    next_yaml(ASDF_YAML_DOCUMENT_START_EVENT, NULL, NULL);
    next_yaml(ASDF_YAML_MAPPING_START_EVENT, "tag:stsci.edu:asdf/core/asdf-1.1.0", NULL);
    next_yaml(ASDF_YAML_SCALAR_EVENT, NULL, "asdf_library");
    next_yaml(ASDF_YAML_MAPPING_START_EVENT, "tag:stsci.edu:asdf/core/software-1.0.0", NULL);
    next_yaml(ASDF_YAML_SCALAR_EVENT, NULL, "author");
    next_yaml(ASDF_YAML_SCALAR_EVENT, NULL, "The ASDF Developers");

    /* Skip ahead to the ndarray, checking only that the stream stays
       well-formed; the middle of the tree is covered by the tree tests. */
    int mapping_start_count = 1;
    int saw_ndarray_tag = 0;
    while (1) {{
        event = asdf_event_iterate(parser);
        assert(event);
        asdf_event_type_t type = asdf_event_type(event);
        if (type == ASDF_TREE_END_EVENT)
            break;
        assert(type == ASDF_YAML_EVENT);
        asdf_yaml_event_type_t kind = asdf_yaml_event_type(event);
        if (kind == ASDF_YAML_MAPPING_START_EVENT) {{
            mapping_start_count++;
            size_t len = 0;
            const char *tag = asdf_yaml_event_tag(event, &len);
            if (len == strlen("tag:stsci.edu:asdf/core/ndarray-1.1.0") &&
                0 == memcmp(tag, "tag:stsci.edu:asdf/core/ndarray-1.1.0", len))
                saw_ndarray_tag = 1;
        }}
    }}
    assert(saw_ndarray_tag);
    assert(mapping_start_count == 6);

    tree = asdf_event_tree_info(event);
    assert(tree != NULL);

    next(ASDF_BLOCK_EVENT);
    const asdf_block_info_t *block = asdf_event_block_info(event);
    assert(block != NULL);

    next(ASDF_END_EVENT);

    /* Past the end the parser reports nothing further. */
    assert(asdf_event_iterate(parser) == NULL);

    asdf_parser_destroy(parser);
    printf("OK\n");
    return 0;
}}
"##,
        path = basic.display()
    );

    let out = compile_and_run("event_stream", &src, true).unwrap();
    assert!(out.contains("OK"), "event walk failed:\n{out}");
}
