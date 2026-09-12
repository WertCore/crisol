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
    /// A style change can change the box, and a box change changes the pixels. Callers ask
    /// for the narrowest thing that is true and get the correct closure, instead of every
    /// call site having to remember the implication order.
    #[must_use]
    pub fn expanded(self) -> Self {
        let mut out = self;
        if out.contains(Self::STYLE) {
            out |= Self::LAYOUT;
        }
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
    fn style_implies_layout_and_paint() {
        assert_eq!(DirtyFlags::STYLE.expanded(), DirtyFlags::ALL_SELF);
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
