//! The provenance schemas: `core/software`, `core/history_entry`,
//! `core/extension_metadata` and the `core/asdf` root.
//!
//! These are what a file says about itself -- what wrote it, what extensions
//! it needs, what was done to it. Readers act on them: the workaround for the
//! Python checksum bug keys off `asdf_library`, and a reader that cannot
//! honour a listed extension knows so before it reaches the value.

use asdf_yaml::{Document, NodeId, ScalarStyle, Tag};

use crate::core::time::Time;
use crate::error::{Result, err};

/// The tag a `core/software` record carries.
pub const SOFTWARE_TAG: &str = "tag:stsci.edu:asdf/core/software-1.0.0";

/// A `core/software` record: a piece of software, named and versioned.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Software {
    /// The software's name.
    pub name: String,
    /// Its version, as written -- not necessarily semantic.
    pub version: String,
    /// Who wrote it.
    pub author: Option<String>,
    /// Where to find it.
    pub homepage: Option<String>,
}

impl Software {
    /// This library's own identity, for stamping files it writes.
    pub fn this_library() -> Self {
        Self {
            name: "libasdf-rs".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            author: Some("The libasdf-rs Developers".to_string()),
            homepage: Some("https://github.com/cruzzil/libasdf-rs".to_string()),
        }
    }

    /// Read a `core/software` record from the tree.
    pub fn parse(doc: &Document, id: NodeId) -> Result<Self> {
        if !doc.resolved(id).is_mapping() {
            return Err(err!(InvalidArgument, "core/software must be a mapping"));
        }
        let field = |key: &str| -> Option<String> {
            doc.mapping_get(id, key).and_then(|n| doc.resolved(n).as_str().map(str::to_string))
        };
        // `name` and `version` are the schema's required properties.
        let (Some(name), Some(version)) = (field("name"), field("version")) else {
            return Err(err!(InvalidArgument, "core/software needs a name and a version"));
        };
        Ok(Self { name, version, author: field("author"), homepage: field("homepage") })
    }

    /// Build the tree node for this record, tagged `core/software`.
    pub fn to_node(&self, doc: &mut Document) -> NodeId {
        let mut pairs = Vec::new();
        let mut put = |doc: &mut Document, key: &str, value: &str| {
            let k = doc.add_scalar(key);
            // Quoted where a bare version like `1.0` would read as a number.
            let v = doc.add_scalar_styled(value, string_style(value));
            pairs.push((k, v));
        };
        put(doc, "name", &self.name);
        put(doc, "version", &self.version);
        if let Some(author) = &self.author {
            put(doc, "author", author);
        }
        if let Some(homepage) = &self.homepage {
            put(doc, "homepage", homepage);
        }
        let node = doc.add_mapping(pairs);
        doc.node_mut(node).tag = Some(Tag::parse(SOFTWARE_TAG));
        node
    }
}

/// Plain style unless the text would read back as something other than a
/// string.
fn string_style(text: &str) -> ScalarStyle {
    match asdf_yaml::resolve(text, ScalarStyle::Plain, asdf_yaml::Schema::Libasdf) {
        asdf_yaml::Resolved::String => ScalarStyle::Plain,
        _ => ScalarStyle::SingleQuoted,
    }
}

/// Read a `software` key that may hold one record or a sequence of them.
fn software_list(doc: &Document, id: NodeId, key: &str) -> Vec<Software> {
    let Some(entry) = doc.mapping_get(id, key) else {
        return Vec::new();
    };
    match doc.sequence_items(doc.resolve(entry)) {
        Some(items) => items.iter().filter_map(|n| Software::parse(doc, *n).ok()).collect(),
        None => Software::parse(doc, entry).into_iter().collect(),
    }
}

/// A `core/history_entry` record: something that was done to the file.
#[derive(Clone, PartialEq, Debug)]
pub struct HistoryEntry {
    /// What was done.
    pub description: Option<String>,
    /// When, if the writer recorded it.
    pub time: Option<Time>,
    /// What did it. The schema allows one record or several.
    pub software: Vec<Software>,
}

impl HistoryEntry {
    /// Read a `core/history_entry` record from the tree.
    pub fn parse(doc: &Document, id: NodeId) -> Result<Self> {
        if !doc.resolved(id).is_mapping() {
            return Err(err!(InvalidArgument, "core/history_entry must be a mapping"));
        }
        let description = doc
            .mapping_get(id, "description")
            .and_then(|n| doc.resolved(n).as_str().map(str::to_string));
        let time = doc.mapping_get(id, "time").and_then(|n| Time::parse(doc, n).ok());
        Ok(Self { description, time, software: software_list(doc, id, "software") })
    }
}

/// A `core/extension_metadata` record: an extension the file was written
/// with, and which a reader needs to make sense of its tagged values.
#[derive(Clone, PartialEq, Debug)]
pub struct ExtensionMetadata {
    /// The extension class that wrote the values.
    pub extension_class: String,
    /// The URI identifying the extension.
    pub extension_uri: Option<String>,
    /// The package providing it.
    ///
    /// Only the `package` key: `software` and `manifest_software` are
    /// different things, and reading either here would make a file that
    /// names no package look as though it does.
    pub package: Option<Software>,
    /// The software that wrote the values.
    pub software: Option<Software>,
    /// The software providing the manifest the extension follows.
    pub manifest_software: Option<Software>,
}

impl ExtensionMetadata {
    /// Read a `core/extension_metadata` record from the tree.
    pub fn parse(doc: &Document, id: NodeId) -> Result<Self> {
        if !doc.resolved(id).is_mapping() {
            return Err(err!(InvalidArgument, "core/extension_metadata must be a mapping"));
        }
        let Some(extension_class) = doc
            .mapping_get(id, "extension_class")
            .and_then(|n| doc.resolved(n).as_str().map(str::to_string))
        else {
            return Err(err!(InvalidArgument, "core/extension_metadata needs an extension_class"));
        };
        let one = |key: &str| doc.mapping_get(id, key).and_then(|n| Software::parse(doc, n).ok());
        Ok(Self {
            extension_class,
            extension_uri: doc
                .mapping_get(id, "extension_uri")
                .and_then(|n| doc.resolved(n).as_str().map(str::to_string)),
            package: one("package"),
            software: one("software"),
            manifest_software: one("manifest_software"),
        })
    }
}

/// A file's `history`: the extensions it needs and what was done to it.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct History {
    /// The extensions the file was written with.
    pub extensions: Vec<ExtensionMetadata>,
    /// What was done to the file, oldest first.
    pub entries: Vec<HistoryEntry>,
}

/// The `core/asdf` root's own metadata.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Meta {
    /// What wrote the file.
    pub asdf_library: Option<Software>,
    /// The file's history.
    pub history: History,
}

impl Meta {
    /// Read the metadata from a `core/asdf` root.
    ///
    /// Everything is optional: a file that says nothing about itself yields
    /// an empty `Meta` rather than an error.
    pub fn parse(doc: &Document, id: NodeId) -> Result<Self> {
        if !doc.resolved(id).is_mapping() {
            return Err(err!(InvalidArgument, "the ASDF root must be a mapping"));
        }
        let asdf_library =
            doc.mapping_get(id, "asdf_library").and_then(|n| Software::parse(doc, n).ok());

        // `history` is a mapping of extensions and entries in the 1.1.0
        // form, and a bare sequence of entries in the older one.
        let mut history = History::default();
        if let Some(node) = doc.mapping_get(id, "history") {
            let node = doc.resolve(node);
            if doc.resolved(node).is_mapping() {
                if let Some(list) = doc.mapping_get(node, "extensions") {
                    history.extensions = read_list(doc, list, ExtensionMetadata::parse);
                }
                if let Some(list) = doc.mapping_get(node, "entries") {
                    history.entries = read_list(doc, list, HistoryEntry::parse);
                }
            } else {
                history.entries = read_list(doc, node, HistoryEntry::parse);
            }
        }
        Ok(Self { asdf_library, history })
    }
}

/// Read a sequence of records, or a single record not in a sequence.
fn read_list<T>(doc: &Document, id: NodeId, parse: fn(&Document, NodeId) -> Result<T>) -> Vec<T> {
    match doc.sequence_items(doc.resolve(id)) {
        Some(items) => items.iter().filter_map(|n| parse(doc, *n).ok()).collect(),
        None => parse(doc, id).into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asdf_yaml::parse_document;

    fn tree(yaml: &str) -> Document {
        parse_document(&format!(
            "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\n{yaml}"
        ))
        .unwrap()
    }

    #[test]
    fn software_needs_a_name_and_a_version() {
        let doc = tree("s: !core/software-1.0.0 {name: asdf, version: 4.1.0}\n");
        let root = doc.root().unwrap();
        let s = Software::parse(&doc, doc.mapping_get(root, "s").unwrap()).unwrap();
        assert_eq!(s.name, "asdf");
        assert_eq!(s.version, "4.1.0");
        assert_eq!(s.author, None);

        let doc = tree("s: !core/software-1.0.0 {name: asdf}\n");
        let root = doc.root().unwrap();
        assert!(Software::parse(&doc, doc.mapping_get(root, "s").unwrap()).is_err());
    }

    #[test]
    fn software_round_trips_through_the_tree() {
        let original = Software::this_library();
        let mut doc = Document::new_asdf();
        let node = original.to_node(&mut doc);

        assert_eq!(doc.tag_of(node).map(Tag::full).as_deref(), Some(SOFTWARE_TAG));
        assert_eq!(Software::parse(&doc, node).unwrap(), original);
    }

    /// A version like `1.0` would read back as a number if written plainly.
    #[test]
    fn a_numeric_looking_version_stays_a_string() {
        let original = Software {
            name: "asdf".to_string(),
            version: "1.0".to_string(),
            author: None,
            homepage: None,
        };
        let mut doc = Document::new_asdf();
        let node = original.to_node(&mut doc);
        assert_eq!(Software::parse(&doc, node).unwrap().version, "1.0");
    }

    #[test]
    fn a_history_entry_carries_its_time_and_software() {
        let doc = tree(
            "e: !core/history_entry-1.0.0\n  \
             description: did a thing\n  \
             time: !<tag:stsci.edu:asdf/time/time-1.4.0> '2025-07-23 11:56:15+00:00'\n  \
             software: !core/software-1.0.0 {name: asdf, version: 4.1.0}\n",
        );
        let root = doc.root().unwrap();
        let entry = HistoryEntry::parse(&doc, doc.mapping_get(root, "e").unwrap()).unwrap();

        assert_eq!(entry.description.as_deref(), Some("did a thing"));
        assert_eq!(entry.software.len(), 1);
        assert_eq!(entry.software[0].name, "asdf");
        let time = entry.time.expect("a time");
        assert_eq!(time.civil.unwrap().unix_seconds, 1_753_271_775);
    }

    /// The schema allows one software record or a list of them.
    #[test]
    fn a_history_entrys_software_may_be_one_or_many() {
        let doc = tree(
            "e: !core/history_entry-1.0.0\n  \
             description: two of them\n  \
             software:\n  \
             - !core/software-1.0.0 {name: a, version: '1'}\n  \
             - !core/software-1.0.0 {name: b, version: '2'}\n",
        );
        let root = doc.root().unwrap();
        let entry = HistoryEntry::parse(&doc, doc.mapping_get(root, "e").unwrap()).unwrap();
        assert_eq!(entry.software.len(), 2);
        assert_eq!(entry.software[1].name, "b");
    }

    /// `package` is the package; `software` and `manifest_software` are not.
    #[test]
    fn extension_metadata_keeps_its_three_software_records_apart() {
        let doc = tree(
            "x: !core/extension_metadata-1.0.0\n  \
             extension_class: asdf.extension._manifest.ManifestExtension\n  \
             extension_uri: asdf://asdf-format.org/core/extensions/core-1.6.0\n  \
             manifest_software: !core/software-1.0.0 {name: asdf_standard, version: 1.1.1}\n  \
             software: !core/software-1.0.0 {name: asdf, version: 4.1.0}\n",
        );
        let root = doc.root().unwrap();
        let x = ExtensionMetadata::parse(&doc, doc.mapping_get(root, "x").unwrap()).unwrap();

        assert_eq!(x.extension_class, "asdf.extension._manifest.ManifestExtension");
        assert_eq!(
            x.extension_uri.as_deref(),
            Some("asdf://asdf-format.org/core/extensions/core-1.6.0")
        );
        assert!(x.package.is_none(), "this record names no package");
        assert_eq!(x.software.unwrap().name, "asdf");
        assert_eq!(x.manifest_software.unwrap().name, "asdf_standard");
    }

    #[test]
    fn meta_reads_the_whole_provenance_block() {
        let doc = tree(
            "asdf_library: !core/software-1.0.0 {name: asdf, version: 4.1.0}\n\
             history:\n  \
             extensions:\n  \
             - !core/extension_metadata-1.0.0\n    \
             extension_class: asdf.extension._manifest.ManifestExtension\n  \
             entries:\n  \
             - !core/history_entry-1.0.0 {description: first}\n  \
             - !core/history_entry-1.0.0 {description: second}\n",
        );
        let meta = Meta::parse(&doc, doc.root().unwrap()).unwrap();

        assert_eq!(meta.asdf_library.unwrap().name, "asdf");
        assert_eq!(meta.history.extensions.len(), 1);
        assert_eq!(meta.history.entries.len(), 2);
        assert_eq!(meta.history.entries[1].description.as_deref(), Some("second"));
    }

    /// The older form puts the entries directly under `history`.
    #[test]
    fn the_pre_1_1_0_history_form_is_a_bare_sequence() {
        let doc = tree(
            "history:\n\
             - !core/history_entry-1.0.0 {description: only}\n",
        );
        let meta = Meta::parse(&doc, doc.root().unwrap()).unwrap();
        assert!(meta.history.extensions.is_empty());
        assert_eq!(meta.history.entries.len(), 1);
        assert_eq!(meta.history.entries[0].description.as_deref(), Some("only"));
    }

    #[test]
    fn a_file_that_says_nothing_about_itself_is_not_an_error() {
        let doc = tree("x: 1\n");
        let meta = Meta::parse(&doc, doc.root().unwrap()).unwrap();
        assert_eq!(meta, Meta::default());
    }
}
