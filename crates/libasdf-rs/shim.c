/*
 * C shim for the parts of libasdf's ABI that Rust cannot express on stable.
 *
 * Two categories land here, and only these two:
 *
 *   1. Variadic functions. `asdf_file_log`, `asdf_file_error_common` and
 *      `asdf_value_error_common` take `...`. Defining a variadic
 *      `extern "C"` function in Rust requires the unstable `c_variadic`
 *      feature, so the varargs are formatted here with `vsnprintf` and the
 *      finished string is handed to a non-variadic Rust entry point.
 *
 *   2. `_Float16` returns. `asdf_ndarray_read_float16_at` returns
 *      `_Float16`, which is still unstable in Rust. Substituting `uint16_t`
 *      is *not* ABI-equivalent -- on x86-64 SysV a `_Float16` returns in
 *      `xmm0` where a `uint16_t` returns in `rax` -- so the conversion has
 *      to happen in C, where the compiler places the value correctly.
 *
 * Everything else is implemented directly in Rust.
 */

#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <asdf/core/ndarray.h>
#include <asdf/error.h>
#include <asdf/log.h>

/* Formatted messages are truncated at this length, as libasdf's own are. */
#define ASDF_SHIM_MSG_MAX 1024

/* ---- Implemented in Rust ------------------------------------------- */

/* Returns the printf-style format string for an error code, or NULL. */
extern const char *asdf_shim_error_format(int code);

/* Records an already-formatted error against a file or value handle. */
extern void asdf_shim_error_set(
    void *obj, int is_value, int code, const char *src_file, int lineno, const char *msg);

/* Records an OS-level error. */
extern void asdf_shim_error_set_system(
    void *obj, int is_value, int errnum, const char *src_file, int lineno);

/* Emits an already-formatted log message. */
extern void asdf_shim_log_message(
    const void *file, int level, const char *src_file, int lineno, const char *msg);

/* Reads a float16 element, returning its raw bit pattern. */
extern uint16_t asdf_shim_ndarray_read_float16_bits_at(
    void *ndarray, const uint64_t *indices, int *err);

/* ---- Variadic entry points ----------------------------------------- */

static void shim_format_error(
    void *obj, int is_value, int code, const char *src_file, int lineno, va_list args) {
    const char *fmt = asdf_shim_error_format(code);
    char msg[ASDF_SHIM_MSG_MAX];

    if (fmt == NULL) {
        msg[0] = '\0';
    } else {
        /* The per-code format string decides how many varargs are consumed. */
        vsnprintf(msg, sizeof(msg), fmt, args);
    }

    asdf_shim_error_set(obj, is_value, code, src_file, lineno, msg);
}

void asdf_file_error_common(
    asdf_file_t *file, asdf_error_code_t code, const char *src_file, int lineno, ...) {
    va_list args;
    va_start(args, lineno);
    shim_format_error((void *)file, 0, (int)code, src_file, lineno, args);
    va_end(args);
}

void asdf_value_error_common(
    asdf_value_t *value, asdf_error_code_t code, const char *src_file, int lineno, ...) {
    va_list args;
    va_start(args, lineno);
    shim_format_error((void *)value, 1, (int)code, src_file, lineno, args);
    va_end(args);
}

void asdf_file_log(
    const asdf_file_t *file,
    asdf_log_level_t level,
    const char *src_file,
    int lineno,
    const char *fmt,
    ...) {
    char msg[ASDF_SHIM_MSG_MAX];
    va_list args;

    va_start(args, fmt);
    vsnprintf(msg, sizeof(msg), fmt == NULL ? "" : fmt, args);
    va_end(args);

    asdf_shim_log_message((const void *)file, (int)level, src_file, lineno, msg);
}

/* ---- Non-variadic helpers that share the same paths ---------------- */

void asdf_file_error_oom(asdf_file_t *file, const char *src_file, int lineno) {
    asdf_shim_error_set((void *)file, 0, ASDF_ERR_OUT_OF_MEMORY, src_file, lineno, "out of memory");
}

void asdf_value_error_oom(asdf_value_t *value, const char *src_file, int lineno) {
    asdf_shim_error_set((void *)value, 1, ASDF_ERR_OUT_OF_MEMORY, src_file, lineno, "out of memory");
}

void asdf_file_error_system(asdf_file_t *file, int errnum, const char *src_file, int lineno) {
    asdf_shim_error_set_system((void *)file, 0, errnum, src_file, lineno);
}

void asdf_value_error_system(asdf_value_t *value, int errnum, const char *src_file, int lineno) {
    asdf_shim_error_set_system((void *)value, 1, errnum, src_file, lineno);
}

/* ---- _Float16 ------------------------------------------------------ */

#ifdef ASDF_HAVE_FLOAT16
_Float16 asdf_ndarray_read_float16_at(
    asdf_ndarray_t *ndarray, const uint64_t *indices, asdf_ndarray_err_t *err) {
    int local_err = 0;
    uint16_t bits = asdf_shim_ndarray_read_float16_bits_at((void *)ndarray, indices, &local_err);
    _Float16 out;

    /* Reinterpreting here, in C, is what puts the value in the register the
     * caller expects for a _Float16 return. */
    memcpy(&out, &bits, sizeof(out));

    if (err != NULL)
        *err = (asdf_ndarray_err_t)local_err;

    return out;
}
#endif /* ASDF_HAVE_FLOAT16 */

/*
 * Register the core-schema extensions before main.
 *
 * ASDF_REGISTER_EXTENSION gives each of upstream's core extensions a
 * __attribute__((constructor)), so they are in the registry before any
 * caller runs. Rust has no equivalent attribute on stable, so the one
 * constructor lives here and calls into the Rust side.
 */
extern void asdf_shim_register_core_extensions(void);

__attribute__((constructor)) static void asdf_shim_register_core(void) {
    asdf_shim_register_core_extensions();
}
