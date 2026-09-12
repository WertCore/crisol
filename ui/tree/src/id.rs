//! Node handles.

use core::num::NonZeroU32;

/// A handle to a node in a [`Tree`](crate::Tree).
///
/// Carries a generation counter alongside the arena index. When a node is removed its slot
/// is reused, and any handle still pointing at the old occupant fails its liveness check
/// instead of silently addressing the new one (DECISIONS D-17).
///
/// That matters more than it looks. At M16 JavaScript holds these as opaque DOM handles,
/// and a stale handle that resolves to *a* node is a bug that reproduces once a week and
/// never in a debugger.
///
/// `NodeId` is 8 bytes and `Option<NodeId>` is also 8, because the generation is non-zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId {
    index: u32,
    generation: NonZeroU32,
}

impl NodeId {
    pub(crate) fn new(index: u32, generation: NonZeroU32) -> Self {
        Self { index, generation }
    }

    pub(crate) fn index(self) -> usize {
        self.index as usize
    }

    pub(crate) fn generation(self) -> NonZeroU32 {
        self.generation
    }

    /// The raw index and generation, for diagnostics and test output only.
    ///
    /// Not an address and not stable across runs. Do not key anything on it.
    #[must_use]
    pub fn debug_parts(self) -> (u32, u32) {
        (self.index, self.generation.get())
    }

    /// Packs the handle into a single integer, losslessly.
    ///
    /// The value is opaque: it is a handle, not an index, and arithmetic on it is
    /// meaningless. It exists because handles have to cross boundaries that only carry
    /// integers — `taffy`'s node ids at M3, and the DOM handles JavaScript is given at M16,
    /// which ROADMAP §3.1 requires be opaque integers rather than pointers.
    ///
    /// Round-trips through [`Self::from_bits`] for as long as the node is alive, and fails
    /// the liveness check afterwards exactly as the original would.
    #[must_use]
    pub fn to_bits(self) -> u64 {
        (u64::from(self.generation.get()) << 32) | u64::from(self.index)
    }

    /// Unpacks a handle produced by [`Self::to_bits`].
    ///
    /// Returns `None` for a value that was never a handle — specifically one with a zero
    /// generation, which no live handle has. This does *not* check that the node is alive;
    /// ask the tree for that.
    #[must_use]
    pub fn from_bits(bits: u64) -> Option<Self> {
        let generation = NonZeroU32::new((bits >> 32) as u32)?;
        #[allow(clippy::cast_possible_truncation, reason = "the low half is the index")]
        Some(Self::new(bits as u32, generation))
    }
}

impl core::fmt::Display for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "#{}v{}", self.index, self.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_round_trip_through_bits() {
        let id = NodeId::new(7, NonZeroU32::new(3).unwrap());
        assert_eq!(NodeId::from_bits(id.to_bits()), Some(id));
    }

    #[test]
    fn the_first_slot_of_the_first_generation_round_trips() {
        // Index 0, generation 1 is the very first node a tree hands out, and packing it
        // naively into the low bits makes it indistinguishable from "no handle".
        let id = NodeId::new(0, NonZeroU32::MIN);
        assert_ne!(id.to_bits(), 0);
        assert_eq!(NodeId::from_bits(id.to_bits()), Some(id));
    }

    #[test]
    fn a_value_that_was_never_a_handle_is_rejected() {
        assert_eq!(NodeId::from_bits(0), None);
        assert_eq!(
            NodeId::from_bits(42),
            None,
            "a bare index has no generation"
        );
    }

    #[test]
    fn distinct_handles_pack_distinctly() {
        let a = NodeId::new(1, NonZeroU32::MIN);
        let b = NodeId::new(2, NonZeroU32::MIN);
        let c = NodeId::new(1, NonZeroU32::new(2).unwrap());
        assert_ne!(a.to_bits(), b.to_bits());
        assert_ne!(a.to_bits(), c.to_bits());
    }
}
