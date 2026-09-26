//! Handles to heap objects.

use crisol_value::{Address, Value};

/// Bits of a handle given to the slot index.
///
/// Thirty-one, not thirty-two: BigInt's tag (D-248) narrowed a `Value`'s payload from 48 bits
/// to 47, and the generation keeps its full sixteen — halving stale-handle detection would be
/// the worse trade — so the slot gives up the bit. Two billion live slots remains more than any
/// reachable heap holds.
const SLOT_BITS: u32 = 31;

/// The low `SLOT_BITS` of a packed handle: the slot index.
const SLOT_MASK: u64 = (1 << SLOT_BITS) - 1;

/// A reference to an object on the heap.
///
/// **Forty-seven bits, because that is what a [`Value`] can carry.** A `Value`'s payload is 47
/// bits (D-53, narrowed by D-248), so a handle that did not fit would have to be boxed, and
/// every object reference in the language would cost an indirection. Thirty-one bits of slot
/// and sixteen of generation is the split that fits: two billion live objects, which is more
/// than the address space allows anyway, and sixty-five thousand reuses of a slot before the
/// generation wraps.
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

    /// Packs the handle into the 47 bits a [`Value`] carries.
    ///
    /// A slot wider than [`SLOT_BITS`] would overlap the generation and silently name the wrong
    /// object; it cannot happen — two billion slots is more memory than exists — but the debug
    /// assertion says so out loud rather than leaving a truncation to be discovered downstream.
    #[must_use]
    pub fn to_address(self) -> Address {
        debug_assert!(
            u64::from(self.slot) <= SLOT_MASK,
            "slot index exceeds the 31 bits a handle reserves for it"
        );
        let packed = u64::from(self.slot) | (u64::from(self.generation) << SLOT_BITS);
        Address::new(packed).expect("47 bits by construction")
    }

    /// Unpacks a handle written by [`GcRef::to_address`].
    #[must_use]
    pub fn from_address(address: Address) -> Self {
        let raw = address.get();
        Self {
            slot: u32::try_from(raw & SLOT_MASK).expect("masked to 31 bits"),
            generation: u16::try_from((raw >> SLOT_BITS) & 0xFFFF).expect("masked to 16 bits"),
        }
    }

    /// The handle as an object [`Value`].
    #[must_use]
    pub fn to_value(self) -> Value {
        Value::object(self.to_address())
    }
}
