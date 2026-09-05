/*
 * C shim for the parts of libasdf's ABI that Rust cannot express on stable.
 *
 * None of these carries its public name. Each is `asdf_shim_*` and hidden,
 * and the public symbol is a naked tail-call trampoline on the Rust side --
 * see `trampoline.rs` for why a C definition could not keep the name.
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

ASDF_LOCAL void asdf_shim_file_error_common(
    asdf_file_t *file, asdf_error_code_t code, const char *src_file, int lineno, ...) {
    va_list args;
    va_start(args, lineno);
    shim_format_error((void *)file, 0, (int)code, src_file, lineno, args);
    va_end(args);
}

ASDF_LOCAL void asdf_shim_value_error_common(
    asdf_value_t *value, asdf_error_code_t code, const char *src_file, int lineno, ...) {
    va_list args;
    va_start(args, lineno);
    shim_format_error((void *)value, 1, (int)code, src_file, lineno, args);
    va_end(args);
}

ASDF_LOCAL void asdf_shim_file_log_v(
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

/* ---- _Float16 ------------------------------------------------------ */

#ifdef ASDF_HAVE_FLOAT16
ASDF_LOCAL _Float16 asdf_shim_read_float16_at(
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

static void asdf_shim_register_core(void) {
    asdf_shim_register_core_extensions();
}

#if defined(_MSC_VER)
/*
 * MSVC has no __attribute__((constructor)). Its equivalent is a function
 * pointer placed in the .CRT$XCU section, which the CRT walks before main.
 * The pragma is required: without it the section is not marked as read-only
 * initialised data and the linker discards it.
 */
#pragma section(".CRT$XCU", read)
__declspec(allocate(".CRT$XCU")) static void (*asdf_shim_register_core_ptr)(void) =
    asdf_shim_register_core;
/* Nothing references the pointer, so keep the linker from dropping it. */
#pragma comment(linker, "/include:asdf_shim_register_core_ptr")
#else
__attribute__((constructor)) static void asdf_shim_register_core_ctor(void) {
    asdf_shim_register_core();
}
#endif
