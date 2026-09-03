/** Version parsing utils */

#ifndef ASDF_VERSION_H
#define ASDF_VERSION_H

#include <asdf/util.h>

ASDF_BEGIN_DECLS

/**
 * Struct representing a version string from an ASDF file
 *
 * This is used forthe ASDF format version, ASDF Standard version,
 * tag versions, etc. where applicable.
 */
typedef struct {
    /** The full, unparsed version string */
    const char *version;
    /** The major version number, if the version was in X.Y.Z format */
    unsigned int major;
    /** The minor version number, if the version was in X.Y.Z format */
    unsigned int minor;
    /** The patch version number, if the version was in X.Y.Z format */
    unsigned int patch;
    /**
     * Extra version info
     *
     * Any trailing version info following parsed X.Y.Z versions; if there
     * was an additional dot as in PEP-440 .devN versions, the dot is omitted.
     * Likewise if the extra version info was separated from the main version
     * by a hyphen ``-`` that is omitted.  Any other less common version
     * suffixes are returned here verbatim.
     */
    const char *extra;
} asdf_version_t;


/**
 * Parse a version string into an `asdf_version_t`
 *
 * If the version is not in MAJOR.MINOR.PATCH format an `asdf_version_t *` is
 * still returned but with the full version string copied verbatim into the
 * ``version`` field, and the major/minor/patch fields set to 0.
 *
 * The `asdf_version_t` allocated by this function must be freed with
 * `asdf_version_destroy`.
 *
 * :param version: The version string to parse
 * :return: A heap-allocated `asdf_version_t` into which the version was parsed
 */
ASDF_EXPORT asdf_version_t *asdf_version_parse(const char *version);


/**
 * Deep copy of an `asdf_version_t *`
 *
 * :param version: The `asdf_version_t *` to copy -- all fields are deep-copied
 *   as well
 */
ASDF_EXPORT asdf_version_t *asdf_version_copy(const asdf_version_t *version);

/**
 * Free memory allocated for an `asdf_version_t` by `asdf_version_parse`
 *
 * :param version: The `asdf_version_t *` to free; the memory it points to is
 *   also zeroed out.
 */
ASDF_EXPORT void asdf_version_destroy(asdf_version_t *version);


ASDF_END_DECLS

#endif /* ASDF_VERSION_H */
