//! The document: a node arena plus the directives that introduced it.

use crate::node::{CollectionStyle, Entry, Node, NodeData, NodeId, ScalarStyle};
use crate::tag::{Tag, TagHandle};

/// The `%YAML` version directive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct YamlVersion {
    /// Major version.
    pub major: u32,
    /// Minor version.
    pub minor: u32,
}

impl YamlVersion {
    /// The version ASDF mandates in the tree.
    pub const V1_1: YamlVersion = YamlVersion { major: 1, minor: 1 };
}

impl Default for YamlVersion {
    fn default() -> Self {
        Self::V1_1
    }
}

/// A parsed YAML document: an arena of nodes reachable from a single root.
#[derive(Clone, Debug)]
pub struct Document {
    nodes: Vec<Node>,
    root: Option<NodeId>,
    /// The `%YAML` directive, if the document carried one.
    pub version: Option<YamlVersion>,
    /// The `%TAG` directives, in the order they appeared.
    pub tag_handles: Vec<TagHandle>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// An empty document with no root.
    pub fn new() -> Self {
        Self { nodes: Vec::new(), root: None, version: None, tag_handles: Vec::new() }
    }

    /// An empty document carrying the directives ASDF conventionally writes.
    pub fn new_asdf() -> Self {
        Self {
            nodes: Vec::new(),
            root: None,
            version: Some(YamlVersion::V1_1),
            tag_handles: vec![TagHandle::asdf_default()],
        }
    }

    /// The number of nodes in the arena.
    ///
    /// This counts every node including those only reachable through an
    /// alias, so it is not the same as the number of distinct tree positions.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The document's root node, if it has one.
    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    /// Set the document's root node.
    pub fn set_root(&mut self, id: NodeId) {
        self.root = Some(id);
    }

    /// Add a node to the arena and return its id.
    pub fn add(&mut self, node: Node) -> NodeId {
        let id = NodeId(u32::try_from(self.nodes.len()).expect("node arena overflow"));
        self.nodes.push(node);
        id
    }

    /// Borrow a node.
    ///
    /// # Panics
    /// Panics if `id` did not come from this document.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    /// Mutably borrow a node.
    ///
    /// # Panics
    /// Panics if `id` did not come from this document.
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    /// Borrow a node if the id is in range.
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index())
    }

    /// Follow aliases until reaching a non-alias node.
    ///
    /// Alias chains are bounded by the arena size, so a cycle -- which a
    /// conforming YAML document cannot contain, but a hand-edited one might --
    /// terminates rather than looping forever.
    pub fn resolve(&self, mut id: NodeId) -> NodeId {
        for _ in 0..=self.nodes.len() {
            match self.nodes.get(id.index()).map(|n| &n.data) {
                Some(NodeData::Alias(target)) => id = *target,
                _ => return id,
            }
        }
        id
    }

    /// Borrow a node, following aliases first.
    pub fn resolved(&self, id: NodeId) -> &Node {
        self.node(self.resolve(id))
    }

    /// The effective tag of a node, following aliases.
    ///
    /// An alias node carries no tag of its own, so the tag of its target is
    /// what callers mean when they ask.
    pub fn tag_of(&self, id: NodeId) -> Option<&Tag> {
        self.resolved(id).tag.as_ref()
    }

    // ---- convenience constructors -------------------------------------

    /// Add a plain scalar node.
    pub fn add_scalar(&mut self, value: impl Into<String>) -> NodeId {
        self.add(Node::scalar(value))
    }

    /// Add a scalar node with an explicit style.
    pub fn add_scalar_styled(&mut self, value: impl Into<String>, style: ScalarStyle) -> NodeId {
        self.add(Node::scalar_styled(value, style))
    }

    /// Add a sequence node built from existing nodes.
    pub fn add_sequence(&mut self, items: Vec<NodeId>) -> NodeId {
        self.add(Node::new(NodeData::Sequence { items, style: CollectionStyle::Auto }))
    }

    /// Add a mapping node built from existing key/value node pairs.
    pub fn add_mapping(&mut self, pairs: Vec<(NodeId, NodeId)>) -> NodeId {
        let entries = pairs.into_iter().map(|(key, value)| Entry { key, value }).collect();
        self.add(Node::new(NodeData::Mapping { entries, style: CollectionStyle::Auto }))
    }

    // ---- accessors ----------------------------------------------------

    /// The items of a sequence node, following aliases.
    pub fn sequence_items(&self, id: NodeId) -> Option<&[NodeId]> {
        match &self.resolved(id).data {
            NodeData::Sequence { items, .. } => Some(items),
            _ => None,
        }
    }

    /// The entries of a mapping node, following aliases.
    pub fn mapping_entries(&self, id: NodeId) -> Option<&[Entry]> {
        match &self.resolved(id).data {
            NodeData::Mapping { entries, .. } => Some(entries),
            _ => None,
        }
    }

    /// Look up a mapping value by string key, following aliases.
    ///
    /// Where a key appears more than once -- which YAML permits and ASDF
    /// files occasionally contain -- the first occurrence wins, matching how
    /// a streaming reader would see it.
    pub fn mapping_get(&self, id: NodeId, key: &str) -> Option<NodeId> {
        let entries = self.mapping_entries(id)?;
        entries.iter().find(|e| self.resolved(e.key).as_str() == Some(key)).map(|e| e.value)
    }

    /// Insert or replace a mapping entry by string key.
    ///
    /// Returns the previous value node when the key was already present.
    pub fn mapping_set(&mut self, id: NodeId, key: &str, value: NodeId) -> Option<NodeId> {
        let target = self.resolve(id);

        let existing = self.mapping_entries(target).and_then(|entries| {
            entries.iter().position(|e| self.resolved(e.key).as_str() == Some(key))
        });

        match existing {
            Some(pos) => {
                let NodeData::Mapping { entries, .. } = &mut self.node_mut(target).data else {
                    return None;
                };
                Some(std::mem::replace(&mut entries[pos].value, value))
            }
            None => {
                let key_id = self.add_scalar(key);
                let NodeData::Mapping { entries, .. } = &mut self.node_mut(target).data else {
                    return None;
                };
                entries.push(Entry { key: key_id, value });
                None
            }
        }
    }

    /// Remove a mapping entry by string key, returning the value node.
    pub fn mapping_remove(&mut self, id: NodeId, key: &str) -> Option<NodeId> {
        let target = self.resolve(id);
        let pos = self
            .mapping_entries(target)?
            .iter()
            .position(|e| self.resolved(e.key).as_str() == Some(key))?;
        let NodeData::Mapping { entries, .. } = &mut self.node_mut(target).data else {
            return None;
        };
        Some(entries.remove(pos).value)
    }

    /// Index into a sequence, following aliases and accepting negative
    /// indices that count back from the end.
    pub fn sequence_get(&self, id: NodeId, index: i64) -> Option<NodeId> {
        let items = self.sequence_items(id)?;
        let len = i64::try_from(items.len()).ok()?;
        let idx = if index < 0 { len + index } else { index };
        if idx < 0 || idx >= len {
            return None;
        }
        items.get(usize::try_from(idx).ok()?).copied()
    }

    /// The number of children of a container node, or `None` for scalars.
    pub fn container_len(&self, id: NodeId) -> Option<usize> {
        match &self.resolved(id).data {
            NodeData::Sequence { items, .. } => Some(items.len()),
            NodeData::Mapping { entries, .. } => Some(entries.len()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> (Document, NodeId) {
        let mut doc = Document::new();
        let v1 = doc.add_scalar("1");
        let v2 = doc.add_scalar("2");
        let k1 = doc.add_scalar("a");
        let k2 = doc.add_scalar("b");
        let map = doc.add_mapping(vec![(k1, v1), (k2, v2)]);
        doc.set_root(map);
        (doc, map)
    }

    #[test]
    fn mapping_lookup_and_order() {
        let (doc, map) = tiny();
        assert_eq!(doc.container_len(map), Some(2));
        let a = doc.mapping_get(map, "a").unwrap();
        assert_eq!(doc.node(a).as_str(), Some("1"));
        assert!(doc.mapping_get(map, "missing").is_none());

        let keys: Vec<_> = doc
            .mapping_entries(map)
            .unwrap()
            .iter()
            .map(|e| doc.node(e.key).as_str().unwrap())
            .collect();
        assert_eq!(keys, ["a", "b"], "insertion order must be preserved");
    }

    #[test]
    fn mapping_set_replaces_in_place() {
        let (mut doc, map) = tiny();
        let three = doc.add_scalar("3");
        let old = doc.mapping_set(map, "a", three);
        assert!(old.is_some());
        assert_eq!(doc.node(doc.mapping_get(map, "a").unwrap()).as_str(), Some("3"));
        // replacing must not reorder or grow the mapping
        assert_eq!(doc.container_len(map), Some(2));
        let keys: Vec<_> = doc
            .mapping_entries(map)
            .unwrap()
            .iter()
            .map(|e| doc.node(e.key).as_str().unwrap())
            .collect();
        assert_eq!(keys, ["a", "b"]);
    }

    #[test]
    fn mapping_set_appends_new_key() {
        let (mut doc, map) = tiny();
        let v = doc.add_scalar("9");
        assert!(doc.mapping_set(map, "c", v).is_none());
        assert_eq!(doc.container_len(map), Some(3));
        assert_eq!(doc.node(doc.mapping_get(map, "c").unwrap()).as_str(), Some("9"));
    }

    #[test]
    fn mapping_remove_works() {
        let (mut doc, map) = tiny();
        let removed = doc.mapping_remove(map, "a").unwrap();
        assert_eq!(doc.node(removed).as_str(), Some("1"));
        assert_eq!(doc.container_len(map), Some(1));
        assert!(doc.mapping_get(map, "a").is_none());
    }

    #[test]
    fn negative_sequence_indices_count_from_end() {
        let mut doc = Document::new();
        let b = doc.add_scalar("b");
        let c = doc.add_scalar("c");
        let seq = doc.add_sequence(vec![b, c]);

        assert_eq!(doc.node(doc.sequence_get(seq, 0).unwrap()).as_str(), Some("b"));
        assert_eq!(doc.node(doc.sequence_get(seq, -1).unwrap()).as_str(), Some("c"));
        assert_eq!(doc.node(doc.sequence_get(seq, -2).unwrap()).as_str(), Some("b"));
        assert!(doc.sequence_get(seq, 2).is_none());
        assert!(doc.sequence_get(seq, -3).is_none());
    }

    #[test]
    fn aliases_resolve_through() {
        let mut doc = Document::new();
        let target = doc.add_scalar("shared");
        doc.node_mut(target).anchor = Some("anc".into());
        let alias = doc.add(Node::new(NodeData::Alias(target)));

        assert!(doc.node(alias).is_alias());
        assert_eq!(doc.resolve(alias), target);
        assert_eq!(doc.resolved(alias).as_str(), Some("shared"));
    }

    #[test]
    fn alias_cycle_terminates() {
        // A conforming document cannot express this, but a corrupt one might;
        // resolution must not hang.
        let mut doc = Document::new();
        let a = doc.add(Node::new(NodeData::Alias(NodeId(1))));
        let b = doc.add(Node::new(NodeData::Alias(NodeId(0))));
        let _ = doc.resolve(a);
        let _ = doc.resolve(b);
    }
}
