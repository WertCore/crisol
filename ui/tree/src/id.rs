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
}

impl core::fmt::Display for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "#{}v{}", self.index, self.generation)
    }
}
