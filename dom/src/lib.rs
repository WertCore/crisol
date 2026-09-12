//! DOM host API over the `crisol-tree`, shared by the AOT and interpreter paths.
//!
//! ROADMAP §M7 asks for *the mutation API that a foreign caller (the JS runtime, later) will
//! drive*, and to *design it as if an external consumer exists, because one will*. This is
//! that API. M16 puts JavaScript behind it; M7's reactive layer is the first caller, and
//! exists partly to find out what the shape is wrong about while it is still cheap to change.
//!
//! **Why this exists rather than callers using [`Tree`] directly.** A mutation has to mark
//! what it invalidates, and the rules are not obvious: changing a class affects the node, its
//! descendants and its *following* siblings; inserting a child affects the parent's other
//! children through `:nth-child`; text becoming empty affects the parent through `:empty`
//! (DECISIONS D-38). `Tree::element_mut` hands out a `&mut` that cannot enforce any of that,
//! and a caller who forgets gets a stale style that looks like a cascade bug.
//!
//! Every mutation here marks correctly. That is the whole point of the layer.
//!
//! ```
//! use crisol_dom::Dom;
//! use crisol_tree::Tree;
//!
//! let mut tree = Tree::new();
//! let mut dom = Dom::new(&mut tree);
//! let root = dom.create_element("div");
//! dom.set_root(root);
//! let text = dom.create_text("hello");
//! dom.append_child(root, text).unwrap();
//! assert_eq!(dom.stats().created, 2);
//! ```

#![doc(html_root_url = "https://docs.rs/crisol-dom/0.0.0")]

use std::borrow::Cow;

use crisol_tree::{Attribute, DirtyFlags, NodeId, NodeKind, Tree, TreeError};

/// Lowercases an attribute name the way [`Attribute::new`] does, without allocating for the
/// overwhelmingly common case of a name that is already lowercase.
fn normalized(name: &str) -> Cow<'_, str> {
    if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

/// What a node is, in the vocabulary JavaScript will be given at M16.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeType {
    /// An element.
    Element,
    /// A text node.
    Text,
}

/// Counters for the mutations a caller made.
///
/// ROADMAP §M7's acceptance is *runs with no full-tree rebuilds*, which is a statement about
/// these numbers. A reconciler that quietly recreated the world would still produce the right
/// pixels; only the counters show the difference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DomStats {
    /// Nodes created.
    pub created: usize,
    /// Nodes inserted into the tree.
    pub inserted: usize,
    /// Nodes detached or removed.
    pub removed: usize,
    /// Text contents replaced.
    pub text_set: usize,
    /// Attribute, class or id writes.
    pub attributes_set: usize,
    /// Writes that changed nothing and were skipped.
    ///
    /// Setting a value to what it already is happens constantly in a reactive system, and
    /// treating it as a change is the difference between an edit costing one relayout and
    /// costing all of them.
    pub no_ops: usize,
}

/// A mutable view of a tree that invalidates correctly.
#[derive(Debug)]
pub struct Dom<'a> {
    tree: &'a mut Tree,
    stats: DomStats,
}

impl<'a> Dom<'a> {
    /// Borrows `tree` for mutation.
    #[must_use]
    pub fn new(tree: &'a mut Tree) -> Self {
        Self {
            tree,
            stats: DomStats::default(),
        }
    }

    /// The mutations made through this view.
    #[must_use]
    pub fn stats(&self) -> DomStats {
        self.stats
    }

    /// Resets the counters, so a caller can measure one update rather than all of them.
    pub fn reset_stats(&mut self) {
        self.stats = DomStats::default();
    }

    /// The tree, for reading.
    #[must_use]
    pub fn tree(&self) -> &Tree {
        self.tree
    }

    // ---- creation --------------------------------------------------------------------

    /// Creates a detached element.
    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.stats.created += 1;
        self.tree.create_element(tag)
    }

    /// Creates a detached text node.
    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.stats.created += 1;
        self.tree.create_text(text)
    }

    /// Makes `node` the document's root.
    pub fn set_root(&mut self, node: NodeId) -> bool {
        self.tree.set_root(node).is_ok()
    }

    /// The root.
    #[must_use]
    pub fn root(&self) -> Option<NodeId> {
        self.tree.root()
    }

    // ---- structure -------------------------------------------------------------------

    /// Appends `child` to `parent`.
    ///
    /// # Errors
    ///
    /// Fails on a stale handle, a self-parent, or an insertion that would create a cycle —
    /// the cases that become DOM exceptions at M16.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        self.tree.append_child(parent, child)?;
        self.stats.inserted += 1;
        Ok(())
    }

    /// Inserts `child` before `reference`, which must be a child of `parent`.
    ///
    /// # Errors
    ///
    /// As [`Self::append_child`], plus when `reference` is not a child of `parent`.
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        child: NodeId,
        reference: NodeId,
    ) -> Result<(), TreeError> {
        self.tree.insert_before(parent, child, reference)?;
        self.stats.inserted += 1;
        Ok(())
    }

    /// Detaches `child` from `parent`, leaving it alive and re-insertable.
    ///
    /// # Errors
    ///
    /// Fails on a stale handle or when `child` is not a child of `parent`.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        self.tree.remove_child(parent, child)?;
        self.stats.removed += 1;
        Ok(())
    }

    /// Replaces `old` with `new` in `old`'s parent.
    ///
    /// # Errors
    ///
    /// Fails on a stale handle, when `old` has no parent, or on a cycle.
    pub fn replace_child(&mut self, old: NodeId, new: NodeId) -> Result<(), TreeError> {
        self.tree.replace_child(old, new)?;
        self.stats.inserted += 1;
        self.stats.removed += 1;
        Ok(())
    }

    /// Removes `node` and everything beneath it, freeing their handles.
    pub fn remove_subtree(&mut self, node: NodeId) -> usize {
        let freed = self.tree.remove_subtree(node);
        self.stats.removed += freed;
        freed
    }

    // ---- content ---------------------------------------------------------------------

    /// Replaces a text node's content.
    ///
    /// Returns whether anything changed. Setting the same text is a no-op rather than a
    /// relayout, which matters because a reactive system writes the same value constantly.
    pub fn set_text(&mut self, node: NodeId, text: &str) -> bool {
        if self.tree.get(node).and_then(|n| n.kind.text()) == Some(text) {
            self.stats.no_ops += 1;
            return false;
        }
        if self.tree.set_text(node, text) {
            self.stats.text_set += 1;
            true
        } else {
            false
        }
    }

    /// Sets an attribute, marking what a selector could notice.
    ///
    /// Returns whether anything changed. `id` and `class` are routed to their own fields,
    /// which is where the matcher looks for them.
    pub fn set_attribute(&mut self, node: NodeId, name: &str, value: &str) -> bool {
        // `Attribute::new` lowercases, as HTML does. Normalising here too keeps a
        // `setAttribute("dataFoo", ..)` findable by the name it was actually stored under.
        let name = normalized(name);
        let name = name.as_ref();
        let Some(data) = self.tree.element(node) else {
            return false;
        };
        if data.attribute(name).as_deref() == Some(value) {
            self.stats.no_ops += 1;
            return false;
        }

        let Some(data) = self.tree.element_mut(node) else {
            return false;
        };
        match name {
            "id" => data.id = Some(value.into()),
            "class" => data.set_class(value),
            _ => match data
                .attributes
                .iter_mut()
                .find(|attribute| attribute.name == *name)
            {
                Some(existing) => existing.value = value.into(),
                None => data.attributes.push(Attribute::new(name, value)),
            },
        }
        self.stats.attributes_set += 1;
        // A selector can see this, so the node, its descendants and its following siblings
        // may all have started or stopped matching (DECISIONS D-38).
        self.tree.mark_selector_state_changed(node);
        true
    }

    /// Removes an attribute. Returns whether it was there.
    pub fn remove_attribute(&mut self, node: NodeId, name: &str) -> bool {
        let name = normalized(name);
        let name = name.as_ref();
        let Some(data) = self.tree.element_mut(node) else {
            return false;
        };
        let had = match name {
            "id" => data.id.take().is_some(),
            "class" => {
                let had = !data.classes.is_empty();
                data.classes.clear();
                had
            }
            _ => {
                let before = data.attributes.len();
                data.attributes.retain(|attribute| attribute.name != *name);
                data.attributes.len() != before
            }
        };
        if had {
            self.stats.attributes_set += 1;
            self.tree.mark_selector_state_changed(node);
        } else {
            self.stats.no_ops += 1;
        }
        had
    }

    /// Adds a class, if it is not already there. Returns whether anything changed.
    pub fn add_class(&mut self, node: NodeId, class: &str) -> bool {
        let Some(data) = self.tree.element(node) else {
            return false;
        };
        if data.has_class(class, true) {
            self.stats.no_ops += 1;
            return false;
        }
        let joined = {
            let data = self.tree.element(node).expect("checked above");
            let mut classes: Vec<_> = data.classes.iter().map(ToString::to_string).collect();
            classes.push(class.to_owned());
            classes.join(" ")
        };
        self.tree
            .element_mut(node)
            .expect("checked above")
            .set_class(&joined);
        self.stats.attributes_set += 1;
        self.tree.mark_selector_state_changed(node);
        true
    }

    /// Removes a class. Returns whether it was there.
    pub fn remove_class(&mut self, node: NodeId, class: &str) -> bool {
        let Some(data) = self.tree.element(node) else {
            return false;
        };
        if !data.has_class(class, true) {
            self.stats.no_ops += 1;
            return false;
        }
        let joined = {
            let data = self.tree.element(node).expect("checked above");
            data.classes
                .iter()
                .filter(|existing| existing.as_str() != class)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        };
        self.tree
            .element_mut(node)
            .expect("checked above")
            .set_class(&joined);
        self.stats.attributes_set += 1;
        self.tree.mark_selector_state_changed(node);
        true
    }

    /// Adds or removes a class to match `present`. Returns whether anything changed.
    pub fn toggle_class(&mut self, node: NodeId, class: &str, present: bool) -> bool {
        if present {
            self.add_class(node, class)
        } else {
            self.remove_class(node, class)
        }
    }

    // ---- queries ---------------------------------------------------------------------

    /// What kind of node this is.
    #[must_use]
    pub fn node_type(&self, node: NodeId) -> Option<NodeType> {
        match self.tree.get(node)?.kind {
            NodeKind::Element(_) | NodeKind::Custom(_) => Some(NodeType::Element),
            NodeKind::Text(_) => Some(NodeType::Text),
        }
    }

    /// The tag name, for elements.
    #[must_use]
    pub fn tag_name(&self, node: NodeId) -> Option<&str> {
        self.tree.get(node)?.kind.tag()
    }

    /// An attribute's value.
    #[must_use]
    pub fn attribute(&self, node: NodeId, name: &str) -> Option<String> {
        self.tree
            .element(node)?
            .attribute(normalized(name).as_ref())
            .map(|value| value.into_owned())
    }

    /// The parent.
    #[must_use]
    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.tree.parent(node)
    }

    /// The first child.
    #[must_use]
    pub fn first_child(&self, node: NodeId) -> Option<NodeId> {
        self.tree.first_child(node)
    }

    /// The next sibling.
    #[must_use]
    pub fn next_sibling(&self, node: NodeId) -> Option<NodeId> {
        self.tree.next_sibling(node)
    }

    /// The children, in order.
    #[must_use]
    pub fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.tree.children(node).collect()
    }

    /// Whether a handle still refers to the node it was issued for.
    #[must_use]
    pub fn is_alive(&self, node: NodeId) -> bool {
        self.tree.is_alive(node)
    }

    /// Marks a node as needing repainting, for a change the tree cannot see.
    ///
    /// A caller that writes `BoxStyle` directly — the reactive layer binding a colour, for
    /// instance — has to say so, because nothing else will.
    pub fn mark_painted(&mut self, node: NodeId) {
        self.tree.mark_dirty(node, DirtyFlags::PAINT);
    }
}
