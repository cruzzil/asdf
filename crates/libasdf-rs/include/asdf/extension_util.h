/**
 * Utilities for extension / schema parsing
 *
 * Provides helper functions and macros for use by libasdf extension authors.
 */
#ifndef ASDF_EXTENSION_UTIL_H
#define ASDF_EXTENSION_UTIL_H

#include <asdf/util.h>
#include <asdf/value.h>


#define ASDF_IS_OK(err) (ASDF_VALUE_OK == (err))

#define ASDF_IS_ERR(err) (!ASDF_IS_OK((err)))

#define ASDF_IS_OPTIONAL_OK(err) (ASDF_VALUE_OK == (err) || ASDF_VALUE_ERR_NOT_FOUND == (err))


ASDF_BEGIN_DECLS

/**
 * Look up a required property in a mapping.
 *
 * Logs a warning if the property is missing or has the wrong type/tag.
 * Returns ASDF_VALUE_OK on success, or an error code on failure.
 */
ASDF_EXPORT asdf_value_err_t asdf_get_required_property(
    asdf_mapping_t *mapping,
    const char *name,
    asdf_value_type_t type,
    const char *tag,
    void *out);

/**
 * Look up an optional property in a mapping.
 *
 * Like asdf_get_required_property but returns ASDF_VALUE_ERR_NOT_FOUND
 * (rather than an error) if the property is absent.  If the property is
 * present, type checking is still performed.
 */
ASDF_EXPORT asdf_value_err_t asdf_get_optional_property(
    asdf_mapping_t *mapping,
    const char *name,
    asdf_value_type_t type,
    const char *tag,
    void *out);

ASDF_END_DECLS

#endif /* ASDF_EXTENSION_UTIL_H */
