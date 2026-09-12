//! Side tables keyed by [`NodeId`].

use crate::id::NodeId;

/// A sparse map from nodes to values, stored as a vector indexed by arena slot.
///
/// Passes that need per-node state — computed style, layout caches, accessibility nodes —
/// keep it here rather than on the `Node`. That keeps `crisol-tree` free of every
/// downstream crate's dependencies: layout can hold a `taffy` cache per node without the
/// tree having to know what `taffy` is.
///
/// Entries carry the generation of the handle that wrote them, so a value left behind by a
/// removed node is never returned to a *different* node that reused the slot. That is the
/// guarantee that matters, and it is the one a pass can rely on.
///
/// What this does **not** do is notice that the tree removed a node. A handle to a removed
/// node still reads back whatever was stored under it, until the slot is reused or the map
/// is cleared, because the map has no way to hear about the removal without being coupled
/// to the tree. Passes that care should clear the map, or ask the tree whether the node is
/// alive first — which they generally do anyway, since they need the node.
#[derive(Clone, Debug)]
pub struct NodeMap<T> {
    slots: Vec<Option<(u32, T)>>,
}

impl<T> Default for NodeMap<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> NodeMap<T> {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// An empty map sized for `capacity` nodes.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
        }
    }

    /// The value for `id`, or `None` when there is none or the handle is stale.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&T> {
        let (generation, value) = self.slots.get(id.index())?.as_ref()?;
        (*generation == id.generation().get()).then_some(value)
    }

    /// The value for `id`, mutably.
    #[must_use]
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut T> {
        let (generation, value) = self.slots.get_mut(id.index())?.as_mut()?;
        (*generation == id.generation().get()).then_some(value)
    }

    /// Stores `value` for `id`, returning what was there for the same generation.
    pub fn insert(&mut self, id: NodeId, value: T) -> Option<T> {
        if self.slots.len() <= id.index() {
            self.slots.resize_with(id.index() + 1, || None);
        }
        let slot = &mut self.slots[id.index()];
        let previous = match slot.take() {
            // A value from an earlier occupant of this slot is not this node's.
            Some((generation, old)) if generation == id.generation().get() => Some(old),
            _ => None,
        };
        *slot = Some((id.generation().get(), value));
        previous
    }

    /// Removes and returns the value for `id`.
    pub fn remove(&mut self, id: NodeId) -> Option<T> {
        let slot = self.slots.get_mut(id.index())?;
        match slot {
            Some((generation, _)) if *generation == id.generation().get() => {
                slot.take().map(|(_, value)| value)
            }
            _ => None,
        }
    }

    /// Drops every entry, keeping the allocation.
    pub fn clear(&mut self) {
        self.slots.clear();
    }

    /// Number of entries, including ones whose node has been removed.
    ///
    /// A stale entry is invisible to [`Self::get`] but still occupies a slot until it is
    /// overwritten or [`Self::clear`]ed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// True when nothing is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tree;

    #[test]
    fn values_round_trip() {
        let mut tree = Tree::new();
        let a = tree.create_element("div");
        let b = tree.create_element("span");
        let mut map = NodeMap::new();

        assert_eq!(map.insert(a, 1), None);
        assert_eq!(map.insert(b, 2), None);
        assert_eq!(map.get(a), Some(&1));
        assert_eq!(map.get(b), Some(&2));
        assert_eq!(map.insert(a, 3), Some(1));
        assert_eq!(map.get(a), Some(&3));
        assert_eq!(map.remove(a), Some(3));
        assert_eq!(map.get(a), None);
    }

    #[test]
    fn a_recycled_slot_does_not_inherit_the_previous_nodes_value() {
        let mut tree = Tree::new();
        let first = tree.create_element("div");
        let mut map = NodeMap::new();
        map.insert(first, 7);

        tree.remove_subtree(first);
        let second = tree.create_element("span");
        assert_eq!(first.debug_parts().0, second.debug_parts().0, "slot reused");

        assert_eq!(
            map.get(second),
            None,
            "a new node must not see the value its slot's previous occupant left"
        );
        assert_eq!(
            map.get(first),
            Some(&7),
            "the map does not hear about tree removals: the stale handle still reads its \
             own entry until the slot is reused. Documented, and why passes clear the map."
        );

        // And once the slot really is reused, the old entry is gone for good.
        map.insert(second, 8);
        assert_eq!(map.get(first), None);
        assert_eq!(map.get(second), Some(&8));
    }

    #[test]
    fn a_stale_handle_cannot_overwrite_a_live_entry() {
        let mut tree = Tree::new();
        let first = tree.create_element("div");
        tree.remove_subtree(first);
        let second = tree.create_element("span");

        let mut map = NodeMap::new();
        map.insert(second, 1);
        map.insert(first, 99);
        assert_eq!(
            map.get(second),
            None,
            "the stale write replaced the slot; it must not be readable as the live node's"
        );
    }
}
