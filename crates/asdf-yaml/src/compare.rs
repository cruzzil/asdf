//! Comparing two documents at the value level.
//!
//! Byte-level parity between YAML writers is a poor correctness criterion:
//! flow versus block, quoting style, line breaking and scalar spelling all
//! vary freely without changing a single value, so byte equality both fails
//! on correct output and passes on incorrect output. The ASDF Standard's own
//! reference corpus says as much -- its files "do not need to be
//! byte-for-byte identical, but should represent the same values at the YAML
//! level".
//!
//! This module is that comparison. It ignores presentation and is strict
//! about meaning: tags, resolved scalar values, sequence order and the set of
//! mapping keys.

use std::collections::HashSet;
use std::fmt;

use crate::document::Document;
use crate::node::{NodeData, NodeId};
use crate::scalar::{Resolved, Schema, resolve};
use crate::tag::Tag;

/// Which document a one-sided difference came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// The left-hand document.
    Left,
    /// The right-hand document.
    Right,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::Left => "left",
            Side::Right => "right",
        })
    }
}

/// One way in which two documents differ.
#[derive(Clone, PartialEq, Debug)]
pub enum Difference {
    /// A mapping key present on one side only.
    MissingKey {
        /// Where in the tree.
        path: String,
        /// The key that is absent from the other side.
        key: String,
        /// The side that *has* the key.
        present_in: Side,
    },
    /// The two nodes are different kinds of thing.
    KindMismatch {
        /// Where in the tree.
        path: String,
        /// The left node's kind.
        left: &'static str,
        /// The right node's kind.
        right: &'static str,
    },
    /// Two scalars resolve to different values.
    ValueMismatch {
        /// Where in the tree.
        path: String,
        /// The left value, as written.
        left: String,
        /// The right value, as written.
        right: String,
    },
    /// The nodes carry different tags.
    TagMismatch {
        /// Where in the tree.
        path: String,
        /// The left tag, if any.
        left: Option<String>,
        /// The right tag, if any.
        right: Option<String>,
    },
    /// Two sequences have different lengths.
    LengthMismatch {
        /// Where in the tree.
        path: String,
        /// The left length.
        left: usize,
        /// The right length.
        right: usize,
    },
}

impl Difference {
    /// Where in the tree this difference is.
    pub fn path(&self) -> &str {
        match self {
            Difference::MissingKey { path, .. }
            | Difference::KindMismatch { path, .. }
            | Difference::ValueMismatch { path, .. }
            | Difference::TagMismatch { path, .. }
            | Difference::LengthMismatch { path, .. } => path,
        }
    }
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Difference::MissingKey { path, key, present_in } => {
                let absent = if *present_in == Side::Left { Side::Right } else { Side::Left };
                write!(f, "{path}: key {key:?} is in the {present_in} document but not the {absent}")
            }
            Difference::KindMismatch { path, left, right } => {
                write!(f, "{path}: left is a {left}, right is a {right}")
            }
            Difference::ValueMismatch { path, left, right } => {
                write!(f, "{path}: {left} != {right}")
            }
            Difference::TagMismatch { path, left, right } => {
                let show = |t: &Option<String>| t.clone().unwrap_or_else(|| "<untagged>".into());
                write!(f, "{path}: tag {} != {}", show(left), show(right))
            }
            Difference::LengthMismatch { path, left, right } => {
                write!(f, "{path}: sequence length {left} != {right}")
            }
        }
    }
}

/// How strict a comparison to make.
#[derive(Clone, Copy, Debug)]
pub struct CompareOptions {
    /// Treat mappings as unordered. YAML assigns no meaning to key order, and
    /// two writers routinely disagree about it, so this is on by default.
    pub ignore_key_order: bool,
    /// Require nodes to carry the same tag. On by default: in ASDF the tag is
    /// the type, so ignoring it would let an ndarray compare equal to a plain
    /// mapping.
    pub compare_tags: bool,
    /// Which scalar-resolution rules to apply to both sides.
    pub schema: Schema,
    /// Relative tolerance for comparing floats. `None` requires bit-equal
    /// values (with NaN equal to NaN, which `==` would not give).
    pub float_tolerance: Option<f64>,
    /// Stop after this many differences, so a wholly unrelated pair of
    /// documents does not produce an unreadable report.
    pub max_differences: usize,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            ignore_key_order: true,
            compare_tags: true,
            schema: Schema::default(),
            float_tolerance: None,
            max_differences: 50,
        }
    }
}

/// The result of a comparison.
#[derive(Clone, Debug, Default)]
pub struct Comparison {
    /// Every difference found, in tree order.
    pub differences: Vec<Difference>,
    /// Whether the report was cut short by `max_differences`.
    pub truncated: bool,
}

impl Comparison {
    /// Whether the two documents represent the same values.
    pub fn is_equal(&self) -> bool {
        self.differences.is_empty()
    }
}

impl fmt::Display for Comparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.differences.is_empty() {
            return f.write_str("documents are equal");
        }
        writeln!(f, "{} difference(s):", self.differences.len())?;
        for d in &self.differences {
            writeln!(f, "  {d}")?;
        }
        if self.truncated {
            write!(f, "  ... (stopped early; more differences remain)")?;
        }
        Ok(())
    }
}

/// The kind of a node, for reporting.
fn kind_of(doc: &Document, id: NodeId) -> &'static str {
    match &doc.resolved(id).data {
        NodeData::Scalar { .. } => "scalar",
        NodeData::Sequence { .. } => "sequence",
        NodeData::Mapping { .. } => "mapping",
        NodeData::Alias(_) => "alias",
    }
}

fn tag_string(tag: Option<&Tag>) -> Option<String> {
    tag.map(|t| t.full())
}

/// Are two resolved scalars the same value?
fn scalars_equal(left: Resolved, right: Resolved, tolerance: Option<f64>) -> bool {
    use Resolved::*;
    match (left, right) {
        (Null, Null) | (String, String) => true,
        (Bool(a), Bool(b)) => a == b,

        // Compare integers by value, not by the width they happened to
        // narrow to: 42 is 42 whether it landed in uint8 or int64.
        (Uint(a, _), Uint(b, _)) => a == b,
        (Int(a, _), Int(b, _)) => a == b,
        (Uint(a, _), Int(b, _)) | (Int(b, _), Uint(a, _)) => {
            i128::from(a) == i128::from(b)
        }

        (Double(a), Double(b)) => floats_equal(a, b, tolerance),

        // A whole-valued float and an integer are the same number, and the
        // two sides may legitimately have written it either way.
        (Double(d), Uint(u, _)) | (Uint(u, _), Double(d)) => {
            d.fract() == 0.0 && d >= 0.0 && (d as u64) == u && (u as f64) == d
        }
        (Double(d), Int(i, _)) | (Int(i, _), Double(d)) => {
            d.fract() == 0.0 && (d as i64) == i && (i as f64) == d
        }

        _ => false,
    }
}

fn floats_equal(a: f64, b: f64, tolerance: Option<f64>) -> bool {
    // Two NaNs represent the same thing here, though `==` says otherwise.
    if a.is_nan() && b.is_nan() {
        return true;
    }
    match tolerance {
        None => a == b,
        Some(tol) => {
            if a == b {
                return true;
            }
            if !a.is_finite() || !b.is_finite() {
                return false;
            }
            let scale = a.abs().max(b.abs());
            (a - b).abs() <= tol * scale.max(1.0)
        }
    }
}

struct Comparer<'a> {
    left: &'a Document,
    right: &'a Document,
    options: CompareOptions,
    out: Comparison,
    /// Node pairs already compared, so shared and aliased structure is not
    /// walked repeatedly and cycles cannot loop forever.
    seen: HashSet<(NodeId, NodeId)>,
}

impl Comparer<'_> {
    fn full(&self) -> bool {
        self.out.differences.len() >= self.options.max_differences
    }

    fn push(&mut self, d: Difference) {
        if self.full() {
            self.out.truncated = true;
            return;
        }
        self.out.differences.push(d);
    }

    fn compare(&mut self, path: &str, left: NodeId, right: NodeId) {
        if self.full() {
            self.out.truncated = true;
            return;
        }

        // Aliases are dereferenced, as the reference corpus prescribes.
        let l = self.left.resolve(left);
        let r = self.right.resolve(right);
        if !self.seen.insert((l, r)) {
            return;
        }

        if self.options.compare_tags {
            let lt = tag_string(self.left.node(l).tag.as_ref());
            let rt = tag_string(self.right.node(r).tag.as_ref());
            if lt != rt {
                self.push(Difference::TagMismatch {
                    path: path.to_string(),
                    left: lt,
                    right: rt,
                });
            }
        }

        match (&self.left.node(l).data, &self.right.node(r).data) {
            (
                NodeData::Scalar { value: lv, style: ls },
                NodeData::Scalar { value: rv, style: rs },
            ) => {
                let lr = resolve(lv, *ls, self.options.schema);
                let rr = resolve(rv, *rs, self.options.schema);
                let same = if matches!(lr, Resolved::String) && matches!(rr, Resolved::String) {
                    // Both are strings: compare the text itself.
                    lv == rv
                } else {
                    scalars_equal(lr, rr, self.options.float_tolerance)
                };
                if !same {
                    self.push(Difference::ValueMismatch {
                        path: path.to_string(),
                        left: format!("{lv:?}"),
                        right: format!("{rv:?}"),
                    });
                }
            }

            (NodeData::Sequence { items: li, .. }, NodeData::Sequence { items: ri, .. }) => {
                let (li, ri) = (li.clone(), ri.clone());
                if li.len() != ri.len() {
                    self.push(Difference::LengthMismatch {
                        path: path.to_string(),
                        left: li.len(),
                        right: ri.len(),
                    });
                }
                for (idx, (a, b)) in li.iter().zip(ri.iter()).enumerate() {
                    let child = format!("{path}/{idx}");
                    self.compare(&child, *a, *b);
                }
            }

            (NodeData::Mapping { entries: le, .. }, NodeData::Mapping { entries: re, .. }) => {
                let le = le.clone();
                let re = re.clone();

                let key_of = |doc: &Document, id: NodeId| -> String {
                    doc.resolved(id).as_str().unwrap_or("<complex key>").to_string()
                };

                let left_keys: Vec<String> =
                    le.iter().map(|e| key_of(self.left, e.key)).collect();
                let right_keys: Vec<String> =
                    re.iter().map(|e| key_of(self.right, e.key)).collect();

                for (idx, key) in left_keys.iter().enumerate() {
                    let child = format!("{path}/{key}");
                    match right_keys.iter().position(|k| k == key) {
                        Some(pos) => {
                            if !self.options.ignore_key_order && pos != idx {
                                self.push(Difference::MissingKey {
                                    path: path.to_string(),
                                    key: format!("{key} (at position {idx} vs {pos})"),
                                    present_in: Side::Left,
                                });
                            }
                            self.compare(&child, le[idx].value, re[pos].value);
                        }
                        None => self.push(Difference::MissingKey {
                            path: path.to_string(),
                            key: key.clone(),
                            present_in: Side::Left,
                        }),
                    }
                }
                for key in &right_keys {
                    if !left_keys.contains(key) {
                        self.push(Difference::MissingKey {
                            path: path.to_string(),
                            key: key.clone(),
                            present_in: Side::Right,
                        });
                    }
                }
            }

            _ => self.push(Difference::KindMismatch {
                path: path.to_string(),
                left: kind_of(self.left, l),
                right: kind_of(self.right, r),
            }),
        }
    }
}

/// Compare two subtrees.
pub fn compare_from(
    left: &Document,
    left_root: NodeId,
    right: &Document,
    right_root: NodeId,
    options: CompareOptions,
) -> Comparison {
    let mut c = Comparer {
        left,
        right,
        options,
        out: Comparison::default(),
        seen: HashSet::new(),
    };
    c.compare("", left_root, right_root);
    c.out
}

/// Compare two documents from their roots.
///
/// A document with no root compares unequal to one with a root.
pub fn compare(left: &Document, right: &Document, options: CompareOptions) -> Comparison {
    match (left.root(), right.root()) {
        (Some(l), Some(r)) => compare_from(left, l, right, r, options),
        (None, None) => Comparison::default(),
        (l, _) => Comparison {
            differences: vec![Difference::KindMismatch {
                path: String::new(),
                left: if l.is_some() { "document" } else { "empty" },
                right: if l.is_some() { "empty" } else { "document" },
            }],
            truncated: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_document;

    fn cmp(a: &str, b: &str) -> Comparison {
        let (da, db) = (parse_document(a).unwrap(), parse_document(b).unwrap());
        compare(&da, &db, CompareOptions::default())
    }

    fn cmp_with(a: &str, b: &str, options: CompareOptions) -> Comparison {
        let (da, db) = (parse_document(a).unwrap(), parse_document(b).unwrap());
        compare(&da, &db, options)
    }

    #[test]
    fn identical_documents_are_equal() {
        assert!(cmp("a: 1\nb: two\n", "a: 1\nb: two\n").is_equal());
    }

    #[test]
    fn presentation_differences_are_ignored() {
        // Flow versus block is the canonical case: the same values, written
        // two ways, must compare equal.
        let r = cmp("a: {x: 1, y: 2}\n", "a:\n  x: 1\n  y: 2\n");
        assert!(r.is_equal(), "{r}");

        let r = cmp("s: [1, 2, 3]\n", "s:\n  - 1\n  - 2\n  - 3\n");
        assert!(r.is_equal(), "{r}");
    }

    #[test]
    fn integer_width_is_not_part_of_the_value() {
        // 42 narrows to uint8 and 300 to uint16; the widths must not matter,
        // only the numbers.
        assert!(cmp("a: 42\n", "a: 42\n").is_equal());
        assert!(cmp("a: 0x2a\n", "a: 42\n").is_equal(), "hex and decimal 42");
    }

    #[test]
    fn quoted_and_unquoted_numbers_differ() {
        // This is a real difference: one is an integer, the other a string.
        let r = cmp("a: 1\n", "a: '1'\n");
        assert!(!r.is_equal());
        assert!(matches!(r.differences[0], Difference::ValueMismatch { .. }));
    }

    #[test]
    fn key_order_is_ignored_by_default() {
        assert!(cmp("a: 1\nb: 2\n", "b: 2\na: 1\n").is_equal());
    }

    #[test]
    fn key_order_can_be_enforced() {
        let options = CompareOptions { ignore_key_order: false, ..Default::default() };
        let r = cmp_with("a: 1\nb: 2\n", "b: 2\na: 1\n", options);
        assert!(!r.is_equal(), "order-sensitive comparison should notice");
    }

    #[test]
    fn missing_keys_are_reported_from_both_sides() {
        let r = cmp("a: 1\nb: 2\n", "a: 1\nc: 3\n");
        assert_eq!(r.differences.len(), 2);
        assert!(r.differences.iter().any(|d| matches!(
            d,
            Difference::MissingKey { key, present_in: Side::Left, .. } if key == "b"
        )));
        assert!(r.differences.iter().any(|d| matches!(
            d,
            Difference::MissingKey { key, present_in: Side::Right, .. } if key == "c"
        )));
    }

    #[test]
    fn tags_are_compared() {
        let a = "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\nx: 1\n...\n";
        let b = "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.0.0\nx: 1\n...\n";
        let r = cmp(a, b);
        assert!(!r.is_equal(), "differing tags must be reported");
        assert!(matches!(r.differences[0], Difference::TagMismatch { .. }));
    }

    #[test]
    fn tag_comparison_can_be_disabled() {
        let a = "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.1.0\nx: 1\n...\n";
        let b = "%YAML 1.1\n%TAG ! tag:stsci.edu:asdf/\n--- !core/asdf-1.0.0\nx: 1\n...\n";
        let options = CompareOptions { compare_tags: false, ..Default::default() };
        assert!(cmp_with(a, b, options).is_equal());
    }

    #[test]
    fn aliases_are_dereferenced() {
        // The reference corpus prescribes dereferencing aliases before
        // comparing, so an aliased document equals its expanded form.
        let aliased = "shared: &a {x: 1}\nother: *a\n";
        let expanded = "shared: {x: 1}\nother: {x: 1}\n";
        let r = cmp(aliased, expanded);
        assert!(r.is_equal(), "{r}");
    }

    #[test]
    fn sequence_length_and_order_matter() {
        let r = cmp("s: [1, 2, 3]\n", "s: [1, 2]\n");
        assert!(r.differences.iter().any(|d| matches!(d, Difference::LengthMismatch { .. })));

        // Sequence order *is* semantic, unlike mapping order.
        let r = cmp("s: [1, 2]\n", "s: [2, 1]\n");
        assert!(!r.is_equal());
    }

    #[test]
    fn kind_mismatches_are_reported() {
        let r = cmp("a: 1\n", "a: [1]\n");
        assert!(matches!(r.differences[0], Difference::KindMismatch { .. }));
    }

    #[test]
    fn nan_equals_nan() {
        // Bare `nan` resolves as a double under libasdf's rules.
        assert!(cmp("a: nan\n", "a: nan\n").is_equal());
    }

    #[test]
    fn float_tolerance_is_respected() {
        let strict = cmp("a: 1.0000001\n", "a: 1.0000002\n");
        assert!(!strict.is_equal(), "exact comparison should differ");

        let options = CompareOptions { float_tolerance: Some(1e-6), ..Default::default() };
        assert!(cmp_with("a: 1.0000001\n", "a: 1.0000002\n", options).is_equal());
    }

    #[test]
    fn whole_floats_equal_integers() {
        // Two writers may spell the same number differently.
        assert!(cmp("a: 1.0\n", "a: 1\n").is_equal());
        assert!(cmp("a: -2.0\n", "a: -2\n").is_equal());
        assert!(!cmp("a: 1.5\n", "a: 1\n").is_equal());
    }

    #[test]
    fn paths_locate_the_difference() {
        let r = cmp("a:\n  b:\n    c: 1\n", "a:\n  b:\n    c: 2\n");
        assert_eq!(r.differences.len(), 1);
        assert_eq!(r.differences[0].path(), "/a/b/c");
    }

    #[test]
    fn reports_are_bounded() {
        let mut a = String::new();
        let mut b = String::new();
        for i in 0..200 {
            a.push_str(&format!("k{i}: 1\n"));
            b.push_str(&format!("k{i}: 2\n"));
        }
        let r = cmp(&a, &b);
        assert!(r.truncated, "an unbounded report would be unreadable");
        assert!(r.differences.len() <= CompareOptions::default().max_differences);
    }

    #[test]
    fn display_is_readable() {
        let r = cmp("a: 1\n", "a: 2\n");
        let text = r.to_string();
        assert!(text.contains("/a"), "{text}");
        assert!(text.contains("1 difference"), "{text}");
    }

    #[test]
    fn shared_structure_does_not_loop() {
        // Both sides alias the same node repeatedly; comparison must
        // terminate rather than re-walking it.
        let doc = "a: &x {p: 1}\nb: *x\nc: *x\nd: *x\n";
        assert!(cmp(doc, doc).is_equal());
    }
}
