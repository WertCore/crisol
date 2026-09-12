//! The arena and the mutation API.

use core::num::NonZeroU32;

use crisol_display_list::Rect;

use crate::atom::Atom;
use crate::custom::CustomNode;
use crate::dirty::DirtyFlags;
use crate::id::NodeId;
use crate::node::{ElementData, Node, NodeKind};

/// Why a tree mutation was refused.
///
/// These are returned rather than panicked on because at M16 they become DOM exceptions,
/// and JavaScript is allowed to attempt all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    /// A handle referred to a node that has been removed, or to a slot that has since been
    /// reused by a different node.
    #[error("node {0} is not alive")]
    StaleHandle(NodeId),

    /// The operation would have made a node its own ancestor.
    #[error("node {child} is an ancestor of {parent}, so it cannot be inserted into it")]
    WouldCycle {
        /// The intended parent.
        parent: NodeId,
        /// The node that would have been inserted.
        child: NodeId,
    },

    /// A reference node was given that is not a child of the parent.
    #[error("node {reference} is not a child of {parent}")]
    NotAChild {
        /// The intended parent.
        parent: NodeId,
        /// The node that was supposed to be one of its children.
        reference: NodeId,
    },

    /// A node was asked to be its own parent.
    #[error("node {0} cannot be its own parent")]
    SelfParent(NodeId),
}

#[derive(Debug)]
struct Slot {
    /// Bumped every time the slot is freed, so handles to the old occupant stop resolving.
    generation: NonZeroU32,
    node: Option<Node>,
}

/// Counters for the instrumentation M6's acceptance test needs.
///
/// Cheap enough to keep on in release: six increments on paths that already touch memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeStats {
    /// Nodes created since the tree was made.
    pub created: u64,
    /// Nodes freed since the tree was made.
    pub freed: u64,
    /// Calls to [`Tree::mark_dirty`].
    pub dirty_marks: u64,
    /// Ancestor links walked while propagating subtree dirty bits.
    pub dirty_walk_steps: u64,
}

/// An arena of nodes.
///
/// Nodes are addressed by [`NodeId`] rather than by reference, which is what allows the
/// tree to be mutated while handles to it are held — by layout, by the event system, and
/// eventually by JavaScript.
#[derive(Debug, Default)]
pub struct Tree {
    slots: Vec<Slot>,
    /// Indices of freed slots, reused newest-first so hot nodes stay near each other.
    free: Vec<u32>,
    root: Option<NodeId>,
    stats: TreeStats,
}

impl Tree {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty tree with room for `capacity` nodes.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            ..Self::default()
        }
    }

    // ---- creation -------------------------------------------------------------------

    /// Creates a detached element node.
    ///
    /// The tag is lowercased, because HTML tag names are ASCII case-insensitive and a
    /// hand-built tree must match the same selectors a parsed one does.
    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.insert(NodeKind::Element(ElementData::new(Atom::lowercase(tag))))
    }

    /// The element data for `id`, or `None` when it is not an element.
    #[must_use]
    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        self.get(id)?.kind.element()
    }

    /// The element data for `id`, mutably.
    #[must_use]
    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        self.get_mut(id)?.kind.element_mut()
    }

    /// The nearest ancestor of `id` that is an element, skipping text and custom nodes.
    ///
    /// Selector matching walks the *element* tree, not the node tree.
    #[must_use]
    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        let mut current = self.parent(id);
        while let Some(node) = current {
            if self.element(node).is_some() {
                return Some(node);
            }
            current = self.parent(node);
        }
        None
    }

    /// The previous sibling of `id` that is an element.
    #[must_use]
    pub fn prev_sibling_element(&self, id: NodeId) -> Option<NodeId> {
        let mut current = self.get(id)?.prev_sibling();
        while let Some(node) = current {
            if self.element(node).is_some() {
                return Some(node);
            }
            current = self.get(node)?.prev_sibling();
        }
        None
    }

    /// The next sibling of `id` that is an element.
    #[must_use]
    pub fn next_sibling_element(&self, id: NodeId) -> Option<NodeId> {
        let mut current = self.next_sibling(id);
        while let Some(node) = current {
            if self.element(node).is_some() {
                return Some(node);
            }
            current = self.next_sibling(node);
        }
        None
    }

    /// The first child of `id` that is an element.
    #[must_use]
    pub fn first_child_element(&self, id: NodeId) -> Option<NodeId> {
        let mut current = self.first_child(id);
        while let Some(node) = current {
            if self.element(node).is_some() {
                return Some(node);
            }
            current = self.next_sibling(node);
        }
        None
    }

    /// Creates a detached text node.
    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.insert(NodeKind::Text(text.into()))
    }

    /// Creates a detached custom node (DECISIONS D-06).
    pub fn create_custom(&mut self, custom: impl CustomNode) -> NodeId {
        self.insert(NodeKind::Custom(Box::new(custom)))
    }

    fn insert(&mut self, kind: NodeKind) -> NodeId {
        self.stats.created += 1;
        let node = Node::new(kind);

        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.node.is_none(), "free list held an occupied slot");
            slot.node = Some(node);
            return NodeId::new(index, slot.generation);
        }

        let index = u32::try_from(self.slots.len()).expect("more than 4 billion nodes");
        let generation = NonZeroU32::MIN;
        self.slots.push(Slot {
            generation,
            node: Some(node),
        });
        NodeId::new(index, generation)
    }

    // ---- access ---------------------------------------------------------------------

    /// The root, if one has been set.
    #[must_use]
    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    /// Sets the root. The node must be alive and must have no parent.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::StaleHandle`] when `id` is not alive.
    pub fn set_root(&mut self, id: NodeId) -> Result<(), TreeError> {
        self.check_alive(id)?;
        self.root = Some(id);
        Ok(())
    }

    /// Whether the handle still refers to the node it was issued for.
    #[must_use]
    pub fn is_alive(&self, id: NodeId) -> bool {
        self.slot(id).is_some()
    }

    fn slot(&self, id: NodeId) -> Option<&Slot> {
        let slot = self.slots.get(id.index())?;
        if slot.generation != id.generation() || slot.node.is_none() {
            return None;
        }
        Some(slot)
    }

    fn check_alive(&self, id: NodeId) -> Result<(), TreeError> {
        if self.is_alive(id) {
            Ok(())
        } else {
            Err(TreeError::StaleHandle(id))
        }
    }

    /// The node, or `None` when the handle is stale.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.slot(id)?.node.as_ref()
    }

    /// The node, mutably.
    #[must_use]
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        let slot = self.slots.get_mut(id.index())?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.node.as_mut()
    }

    /// The node.
    ///
    /// # Panics
    ///
    /// Panics when the handle is stale. Use [`Tree::get`] where a stale handle is expected.
    #[must_use]
    pub fn node(&self, id: NodeId) -> &Node {
        self.get(id)
            .unwrap_or_else(|| panic!("node {id} is not alive"))
    }

    /// The node, mutably.
    ///
    /// # Panics
    ///
    /// Panics when the handle is stale.
    #[must_use]
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.get_mut(id)
            .unwrap_or_else(|| panic!("node {id} is not alive"))
    }

    /// Number of live nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    /// True when there are no live nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Counters since the tree was created.
    #[must_use]
    pub fn stats(&self) -> TreeStats {
        self.stats
    }

    // ---- navigation -----------------------------------------------------------------

    /// The parent.
    #[must_use]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.get(id)?.parent()
    }

    /// The first child.
    #[must_use]
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.get(id)?.first_child()
    }

    /// The next sibling.
    #[must_use]
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.get(id)?.next_sibling()
    }

    /// Iterates the children in order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            tree: self,
            next: self.first_child(id),
        }
    }

    /// Whether `ancestor` is `id` or one of its ancestors.
    #[must_use]
    pub fn is_inclusive_ancestor(&self, ancestor: NodeId, id: NodeId) -> bool {
        let mut current = Some(id);
        while let Some(node) = current {
            if node == ancestor {
                return true;
            }
            current = self.parent(node);
        }
        false
    }

    /// The node's border box in absolute coordinates, by summing ancestor origins.
    ///
    /// O(depth). Paint accumulates offsets as it descends instead of calling this per node;
    /// this exists for one-off queries such as "where is the focused element".
    #[must_use]
    pub fn absolute_rect(&self, id: NodeId) -> Option<Rect> {
        let mut rect = self.get(id)?.layout;
        let mut current = self.parent(id);
        while let Some(node) = current {
            let parent = self.get(node)?;
            rect = rect.translate(parent.layout.origin);
            current = parent.parent();
        }
        Some(rect)
    }

    // ---- mutation -------------------------------------------------------------------

    /// Appends `child` to `parent`, detaching it from its current parent first.
    ///
    /// # Errors
    ///
    /// Fails on a stale handle, on a self-parent, or when the insertion would create a
    /// cycle.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        self.prepare_insert(parent, child)?;

        let previous_last = self.node(parent).last_child;
        {
            let node = self.node_mut(child);
            node.parent = Some(parent);
            node.prev_sibling = previous_last;
            node.next_sibling = None;
        }
        match previous_last {
            Some(last) => self.node_mut(last).next_sibling = Some(child),
            None => self.node_mut(parent).first_child = Some(child),
        }
        self.node_mut(parent).last_child = Some(child);

        self.mark_dirty(parent, DirtyFlags::LAYOUT);
        self.mark_dirty(child, DirtyFlags::ALL_SELF);
        Ok(())
    }

    /// Inserts `child` immediately before `reference`, which must be a child of `parent`.
    ///
    /// This is the operation a `Vec<NodeId>` would make O(n) and a sibling list makes O(1)
    /// (DECISIONS D-17).
    ///
    /// # Errors
    ///
    /// Fails on a stale handle, a self-parent, a cycle, or when `reference` is not a child
    /// of `parent`.
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        child: NodeId,
        reference: NodeId,
    ) -> Result<(), TreeError> {
        self.check_alive(reference)?;
        if self.parent(reference) != Some(parent) {
            return Err(TreeError::NotAChild { parent, reference });
        }
        if child == reference {
            return Ok(());
        }
        self.prepare_insert(parent, child)?;

        // Re-read after the detach in `prepare_insert`: it may have moved `reference`'s
        // neighbours if `child` was one of them.
        let before = self.node(reference).prev_sibling;
        {
            let node = self.node_mut(child);
            node.parent = Some(parent);
            node.prev_sibling = before;
            node.next_sibling = Some(reference);
        }
        self.node_mut(reference).prev_sibling = Some(child);
        match before {
            Some(previous) => self.node_mut(previous).next_sibling = Some(child),
            None => self.node_mut(parent).first_child = Some(child),
        }

        self.mark_dirty(parent, DirtyFlags::LAYOUT);
        self.mark_dirty(child, DirtyFlags::ALL_SELF);
        Ok(())
    }

    /// Detaches `child` from `parent`, leaving it alive and parentless.
    ///
    /// # Errors
    ///
    /// Fails on a stale handle or when `child` is not a child of `parent`.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        self.check_alive(parent)?;
        self.check_alive(child)?;
        if self.parent(child) != Some(parent) {
            return Err(TreeError::NotAChild {
                parent,
                reference: child,
            });
        }
        self.detach(child);
        Ok(())
    }

    /// Replaces `old` with `new` in `old`'s parent.
    ///
    /// # Errors
    ///
    /// Fails on a stale handle, when `old` has no parent, or when the insertion would
    /// create a cycle.
    pub fn replace_child(&mut self, old: NodeId, new: NodeId) -> Result<(), TreeError> {
        self.check_alive(old)?;
        let parent = self.parent(old).ok_or(TreeError::NotAChild {
            parent: old,
            reference: old,
        })?;
        self.insert_before(parent, new, old)?;
        self.detach(old);
        Ok(())
    }

    /// Detaches `id` from its parent, if it has one. The node stays alive.
    pub fn detach(&mut self, id: NodeId) {
        let Some(node) = self.get(id) else {
            return;
        };
        let (parent, previous, next) = (node.parent, node.prev_sibling, node.next_sibling);
        let Some(parent) = parent else {
            return;
        };

        match previous {
            Some(previous) => self.node_mut(previous).next_sibling = next,
            None => self.node_mut(parent).first_child = next,
        }
        match next {
            Some(next) => self.node_mut(next).prev_sibling = previous,
            None => self.node_mut(parent).last_child = previous,
        }
        {
            let node = self.node_mut(id);
            node.parent = None;
            node.prev_sibling = None;
            node.next_sibling = None;
        }
        // The parent lost a child, so its layout changed even though nothing about the
        // parent itself did.
        self.mark_dirty(parent, DirtyFlags::LAYOUT);
    }

    /// Removes `id` and everything beneath it, freeing their slots.
    ///
    /// Every handle into the removed subtree becomes stale. Returns the number of nodes
    /// freed.
    pub fn remove_subtree(&mut self, id: NodeId) -> usize {
        if !self.is_alive(id) {
            return 0;
        }
        self.detach(id);
        if self.root == Some(id) {
            self.root = None;
        }

        // Iterative rather than recursive: a deep document must not be able to overflow the
        // Rust stack just by being deleted.
        let mut freed = 0;
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            let mut child = self.first_child(current);
            while let Some(node) = child {
                child = self.next_sibling(node);
                stack.push(node);
            }
            self.free_slot(current);
            freed += 1;
        }
        freed
    }

    fn free_slot(&mut self, id: NodeId) {
        let Some(slot) = self.slots.get_mut(id.index()) else {
            return;
        };
        if slot.generation != id.generation() || slot.node.is_none() {
            return;
        }
        slot.node = None;
        // Saturating rather than wrapping: a slot that has been recycled four billion times
        // stops being reusable instead of silently validating a stale handle.
        slot.generation = slot.generation.checked_add(1).unwrap_or(NonZeroU32::MAX);
        if slot.generation != NonZeroU32::MAX {
            self.free.push(id.index() as u32);
        }
        self.stats.freed += 1;
    }

    /// Detaches `child` and checks the insertion is legal.
    fn prepare_insert(&mut self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        self.check_alive(parent)?;
        self.check_alive(child)?;
        if parent == child {
            return Err(TreeError::SelfParent(parent));
        }
        // A childless node cannot be an ancestor of anything, so the O(depth) walk is only
        // needed when the child actually has a subtree. That short-circuit matters: a parser
        // appends millions of freshly created leaves, and without it building a document
        // costs O(nodes x depth).
        if self.first_child(child).is_some() && self.is_inclusive_ancestor(child, parent) {
            return Err(TreeError::WouldCycle { parent, child });
        }
        self.detach(child);
        Ok(())
    }

    // ---- invalidation ---------------------------------------------------------------

    /// Marks `id` dirty and tells its ancestors something beneath them changed.
    ///
    /// `flags` is expanded first, so marking `STYLE` also marks `LAYOUT` and `PAINT`
    /// (DECISIONS D-18). The ancestor walk stops as soon as it reaches a node that already
    /// carries the subtree bits, which is what keeps the steady state cheap: the second
    /// keystroke in a paragraph walks one level, not four hundred pages.
    pub fn mark_dirty(&mut self, id: NodeId, flags: DirtyFlags) {
        if !self.is_alive(id) {
            return;
        }
        self.stats.dirty_marks += 1;

        let flags = flags.expanded();
        let node = self.node_mut(id);
        node.dirty |= flags;

        let subtree = flags.as_subtree();
        let mut current = self.parent(id);
        while let Some(ancestor) = current {
            self.stats.dirty_walk_steps += 1;
            let node = self.node_mut(ancestor);
            if node.dirty.contains(subtree) {
                break;
            }
            node.dirty |= subtree;
            current = node.parent;
        }
    }

    /// This node's pending invalidation.
    #[must_use]
    pub fn dirty(&self, id: NodeId) -> DirtyFlags {
        self.get(id).map_or(DirtyFlags::empty(), Node::dirty)
    }

    /// Clears `flags` on `id` only. Subtree bits are cleared by whoever walked the subtree.
    pub fn clear_dirty(&mut self, id: NodeId, flags: DirtyFlags) {
        if let Some(node) = self.get_mut(id) {
            node.dirty.remove(flags);
        }
    }

    /// Clears every dirty bit in the tree.
    ///
    /// What a full pass calls when it is done. M6 replaces this with per-pass clearing that
    /// only touches what it visited.
    pub fn clear_all_dirty(&mut self) {
        for slot in &mut self.slots {
            if let Some(node) = slot.node.as_mut() {
                node.dirty = DirtyFlags::empty();
            }
        }
    }
}

/// Iterator over a node's children, in order.
#[derive(Debug)]
pub struct Children<'a> {
    tree: &'a Tree,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.tree.next_sibling(current);
        Some(current)
    }
}
