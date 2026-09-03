/**
 * YAML-specific types and utilities
 *
 * This header provides types and accessor functions that relate specifically to
 * the YAML layer of ASDF.  It covers node style hints for serialization, the
 * YAML event types surfaced through the event-based API, and functions for
 * extracting YAML-specific fields from `asdf_event_t` objects.
 */

//

#ifndef ASDF_YAML_H
#define ASDF_YAML_H

#include <asdf/event.h>
#include <asdf/util.h>

ASDF_BEGIN_DECLS


/**
 * YAML node style hint used when serializing mappings and sequences
 */
typedef enum {
    /** Let libfyaml choose the style */
    ASDF_YAML_NODE_STYLE_AUTO = 0,
    /** Emit inline using ``{...}`` / ``[...]`` notation */
    ASDF_YAML_NODE_STYLE_FLOW,
    /** Emit in indented block notation */
    ASDF_YAML_NODE_STYLE_BLOCK
} asdf_yaml_node_style_t;


/**
 * YAML event types returned inside an `asdf_event_t` of type
 * ``ASDF_YAML_EVENT``
 *
 * These mirror the libfyaml event model and are extracted via
 * `asdf_yaml_event_type`.
 */
typedef enum {
    /** Sentinel: the enclosing `asdf_event_t` does not carry a YAML sub-event */
    ASDF_YAML_NONE_EVENT = 0,
    /** Start of the YAML stream */
    ASDF_YAML_STREAM_START_EVENT,
    /** End of the YAML stream */
    ASDF_YAML_STREAM_END_EVENT,
    /** Start of a YAML document (``---``) */
    ASDF_YAML_DOCUMENT_START_EVENT,
    /** End of a YAML document (``...``) */
    ASDF_YAML_DOCUMENT_END_EVENT,
    /** Start of a YAML mapping node */
    ASDF_YAML_MAPPING_START_EVENT,
    /** End of a YAML mapping node */
    ASDF_YAML_MAPPING_END_EVENT,
    /** Start of a YAML sequence node */
    ASDF_YAML_SEQUENCE_START_EVENT,
    /** End of a YAML sequence node */
    ASDF_YAML_SEQUENCE_END_EVENT,
    /** A YAML scalar node */
    ASDF_YAML_SCALAR_EVENT,
    /** A YAML alias node (reference to an anchor) */
    ASDF_YAML_ALIAS_EVENT
} asdf_yaml_event_type_t;


/**
 * A YAML tag directive associating a handle (e.g. ``"!!"`` ) with a prefix
 * (e.g. ``"tag:yaml.org,2002:"`` )
 */
typedef struct {
    const char *handle;
    const char *prefix;
} asdf_yaml_tag_handle_t;


/**
 * Return the raw scalar string from a ``ASDF_YAML_SCALAR_EVENT``
 *
 * The string is owned by ``event`` and must not be freed by the caller.
 * Returns ``NULL`` if ``event`` does not carry a scalar sub-event.
 *
 * :param event: The `asdf_event_t *` to query
 * :param lenp: If non-NULL, receives the byte length of the returned string
 * :return: Pointer to the scalar value, or ``NULL``
 */
ASDF_EXPORT const char *asdf_yaml_event_scalar_value(const asdf_event_t *event, size_t *lenp);

/**
 * Return the YAML tag string from a YAML event, if any
 *
 * The string is owned by ``event`` and must not be freed by the caller.
 * Returns ``NULL`` if the event carries no tag.
 *
 * :param event: The `asdf_event_t *` to query
 * :param lenp: If non-NULL, receives the byte length of the returned string
 * :return: Pointer to the tag string, or ``NULL``
 */
ASDF_EXPORT const char *asdf_yaml_event_tag(const asdf_event_t *event, size_t *lenp);

/**
 * Return the `asdf_yaml_event_type_t` of the YAML sub-event carried by
 * ``event``
 *
 * Returns ``ASDF_YAML_NONE_EVENT`` if ``event`` is not of type
 * ``ASDF_YAML_EVENT``.
 *
 * :param event: The `asdf_event_t *` to query
 * :return: The `asdf_yaml_event_type_t` of the YAML sub-event
 */
ASDF_EXPORT asdf_yaml_event_type_t asdf_yaml_event_type(const asdf_event_t *event);

/**
 * Return a human-readable name for the YAML event type of ``event``
 *
 * :param event: The `asdf_event_t *` to query
 * :return: A static string such as ``"ASDF_YAML_SCALAR_EVENT"``
 */
ASDF_EXPORT const char *asdf_yaml_event_type_text(const asdf_event_t *event);

ASDF_END_DECLS

#endif /* ASDF_YAML_H */
