//! Handles to heap objects.

use crisol_value::{Address, Value};

/// Bits of a handle given to the slot index.
const SLOT_BITS: u32 = 32;

/// A reference to an object on the heap.
///
/// **Forty-eight bits, because that is what a [`Value`] can carry.** A `Value`'s payload is 48
/// bits (D-53), so a handle that did not fit would have to be boxed, and every object
/// reference in the language would cost an indirection. Thirty-two bits of slot and sixteen of
/// generation is the split that fits: four billion live objects, which is more than the
/// address space allows anyway, and sixty-five thousand reuses of a slot before the generation
/// wraps.
///
/// **The generation is what makes a stale handle detectable.** Reading through a handle whose
/// object has been collected fails its liveness check rather than resolving to whatever now
/// occupies the slot — the same reasoning as `crisol-tree`'s `NodeId` (D-17), and for a
/// collector it is the difference between a caught error and the use-after-free ROADMAP §3.1
/// says is the worst failure mode to debug.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct GcRef {
    slot: u32,
    generation: u16,
}

impl GcRef {
    pub(crate) const fn new(slot: u32, generation: u16) -> Self {
        Self { slot, generation }
    }

    /// Which slot of the heap this handle names.
    ///
    /// Public because a stale handle and a live one for the same slot are a thing worth being
    /// able to see; it exposes no invariant, since reading through a handle is checked.
    #[must_use]
    pub const fn slot(self) -> u32 {
        self.slot
    }

    /// How many times this slot had been freed when the handle was made.
    #[must_use]
    pub const fn generation(self) -> u16 {
        self.generation
    }

    /// Packs the handle into the 48 bits a [`Value`] carries.
    #[must_use]
    pub fn to_address(self) -> Address {
        let packed = u64::from(self.slot) | (u64::from(self.generation) << SLOT_BITS);
        Address::new(packed).expect("48 bits by construction")
    }

    /// Unpacks a handle written by [`GcRef::to_address`].
    #[must_use]
    pub fn from_address(address: Address) -> Self {
        let raw = address.get();
        Self {
            slot: u32::try_from(raw & 0xFFFF_FFFF).expect("masked to 32 bits"),
            generation: u16::try_from((raw >> SLOT_BITS) & 0xFFFF).expect("masked to 16 bits"),
        }
    }

    /// The handle as an object [`Value`].
    #[must_use]
    pub fn to_value(self) -> Value {
        Value::object(self.to_address())
    }
}
