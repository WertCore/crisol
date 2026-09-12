//! Invalidation flags.

use bitflags::bitflags;

bitflags! {
    /// What has changed about a node, and what has changed somewhere beneath it.
    ///
    /// Marking a node dirty sets its own bit and walks to the root setting the matching
    /// `SUBTREE_` bit. A pass can then skip a clean subtree in constant time instead of
    /// visiting every node in it — the difference between a keystroke costing twenty nodes
    /// and costing four hundred pages (DECISIONS D-18).
    ///
    /// The walk is O(depth) and happens on mutation, because in a document editor's steady
    /// state mutation is the rare event and traversal is the frequent one.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct DirtyFlags: u8 {
        /// The node's computed style may have changed.
        const STYLE = 1 << 0;
        /// The node's box may have changed size or position.
        const LAYOUT = 1 << 1;
        /// The node's appearance may have changed, but not its box.
        const PAINT = 1 << 2;

        /// Some descendant has `STYLE` set.
        const SUBTREE_STYLE = 1 << 3;
        /// Some descendant has `LAYOUT` set.
        const SUBTREE_LAYOUT = 1 << 4;
        /// Some descendant has `PAINT` set.
        const SUBTREE_PAINT = 1 << 5;

        /// Everything about this node.
        const ALL_SELF = Self::STYLE.bits() | Self::LAYOUT.bits() | Self::PAINT.bits();
        /// Everything about the subtree.
        const ALL_SUBTREE =
            Self::SUBTREE_STYLE.bits() | Self::SUBTREE_LAYOUT.bits() | Self::SUBTREE_PAINT.bits();
    }
}

impl DirtyFlags {
    /// The `SUBTREE_` bits an ancestor must carry to account for these bits.
    #[must_use]
    pub fn as_subtree(self) -> Self {
        let mut out = Self::empty();
        if self.intersects(Self::STYLE | Self::SUBTREE_STYLE) {
            out |= Self::SUBTREE_STYLE;
        }
        if self.intersects(Self::LAYOUT | Self::SUBTREE_LAYOUT) {
            out |= Self::SUBTREE_LAYOUT;
        }
        if self.intersects(Self::PAINT | Self::SUBTREE_PAINT) {
            out |= Self::SUBTREE_PAINT;
        }
        out
    }

    /// Expands a request into everything it implies.
    ///
    /// A box change changes the pixels, so `LAYOUT` implies `PAINT`. Callers ask for the
    /// narrowest thing that is true and get the correct closure, instead of every call site
    /// having to remember the implication order.
    ///
    /// **`STYLE` does not imply `LAYOUT`**, though a style change often does change the box.
    /// The implication is real but it is not knowable here: `mark_dirty` is told a style
    /// *may* have changed, and assuming it did costs a relayout of every node whose
    /// selectors merely needed re-evaluating. Inserting one row into a thousand-row list
    /// marks every sibling for restyle, because `:nth-child` could have moved — and none of
    /// their boxes change. The style pass resolves it instead, marking `LAYOUT` on the nodes
    /// that genuinely computed to a different style, which interning makes a pointer
    /// comparison.
    #[must_use]
    pub fn expanded(self) -> Self {
        let mut out = self;
        if out.contains(Self::LAYOUT) {
            out |= Self::PAINT;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_implies_paint_but_style_implies_neither() {
        assert_eq!(
            DirtyFlags::LAYOUT.expanded(),
            DirtyFlags::LAYOUT | DirtyFlags::PAINT
        );
        // A style change usually does move the box, but `mark_dirty` is only told it *may*
        // have changed. Assuming it did relaid out every sibling of an inserted list row,
        // because a structural change marks them all for `:nth-child`. The style pass marks
        // `LAYOUT` on the ones whose style genuinely differs.
        assert_eq!(DirtyFlags::STYLE.expanded(), DirtyFlags::STYLE);
    }

    #[test]
    fn paint_implies_nothing_else() {
        assert_eq!(DirtyFlags::PAINT.expanded(), DirtyFlags::PAINT);
    }

    #[test]
    fn subtree_bits_propagate_transitively() {
        // A node that is itself clean but has a dirty descendant still has to tell *its*
        // ancestors something, or the walk stops one level too early.
        assert_eq!(
            DirtyFlags::SUBTREE_LAYOUT.as_subtree(),
            DirtyFlags::SUBTREE_LAYOUT
        );
        assert_eq!(DirtyFlags::LAYOUT.as_subtree(), DirtyFlags::SUBTREE_LAYOUT);
        assert!(DirtyFlags::empty().as_subtree().is_empty());
    }
}
