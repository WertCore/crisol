//! A JavaScript value in 64 bits.
//!
//! Every value is one `u64`. Numbers are IEEE-754 doubles stored as themselves; everything
//! else hides in the space doubles do not use. That space exists because a double has 2^52
//! distinct NaN bit patterns and JavaScript can observe only one of them, so all but one are
//! free real estate.
//!
//! # The layout
//!
//! ```text
//! 63          51                                              0
//! ┌─┬───────────┬─┬─┬──┬──────────────────────────────────────┐
//! │s│ exponent  │q│e│tg│              payload                 │
//! └─┴───────────┴─┴─┴──┴──────────────────────────────────────┘
//! ```
//!
//! A value is a **number** unless its exponent is all ones *and* both `q` (bit 51, the quiet
//! bit) and `e` (bit 50) are set. That pair is [`TAG_BASE`]. Reserving `e` as well as `q` is
//! what leaves the canonical quiet NaN — `0x7FF8…`, the one every FPU produces — on the
//! number side of the line, so arithmetic that overflows into NaN needs no special handling
//! at the point it happens.
//!
//! `tg` is two bits saying what the remaining 48 hold, and 48 bits is not a guess: every
//! platform this engine targets gives user space a 48-bit virtual address, so a heap pointer
//! fits with nothing to spare and nothing wasted. [`Address`] is the type that refuses
//! anything larger rather than truncating it.
//!
//! # Why not tag numbers instead
//!
//! The other arrangement — tag the pointers' space and leave doubles boxed — costs an
//! allocation and an indirection on every arithmetic result. JavaScript's only number type is
//! the double, so that is the hot path by definition, and making it the slow one to keep
//! pointer handling tidy is the wrong way round.

use std::fmt;

/// Bits that are set on every value that is not a number.
///
/// Exponent all ones, plus the quiet bit, plus one more mantissa bit. See the module docs for
/// why the extra bit is there.
const TAG_BASE: u64 = 0x7FFC_0000_0000_0000;

/// Selects the two bits that say what a non-number value is.
const TAG_MASK: u64 = 0x0003_0000_0000_0000;

/// The 48 bits a non-number value carries.
const PAYLOAD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

const TAG_SINGLETON: u64 = 0x0000_0000_0000_0000;
const TAG_OBJECT: u64 = 0x0001_0000_0000_0000;
const TAG_STRING: u64 = 0x0002_0000_0000_0000;
const TAG_SYMBOL: u64 = 0x0003_0000_0000_0000;

const SINGLETON_UNDEFINED: u64 = 0;
const SINGLETON_NULL: u64 = 1;
const SINGLETON_FALSE: u64 = 2;
const SINGLETON_TRUE: u64 = 3;

/// The NaN every value that is not a number canonicalises to.
///
/// `f64::NAN`'s own bits, which is the pattern hardware produces. Any *other* NaN is rewritten
/// to this on the way in — see [`Value::number`].
const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

/// How many bits of a pointer a [`Value`] can carry.
pub const ADDRESS_BITS: u32 = 48;

/// A heap address small enough to live inside a [`Value`].
///
/// Constructed through [`Address::new`], which rejects anything wider than
/// [`ADDRESS_BITS`] rather than truncating it. Truncation here would produce a pointer that
/// is merely *wrong* rather than obviously invalid, and the failure would surface as a
/// corrupt read somewhere else entirely.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address(u64);

impl Address {
    /// The largest address that fits.
    pub const MAX: u64 = PAYLOAD_MASK;

    /// Wraps `raw`, or `None` if it does not fit in [`ADDRESS_BITS`].
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        if raw > Self::MAX {
            return None;
        }
        Some(Self(raw))
    }

    /// The address as an integer.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({:#014x})", self.0)
    }
}

/// What a [`Value`] is, once asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `undefined`.
    Undefined,
    /// `null`.
    Null,
    /// `true` or `false`.
    Boolean,
    /// Any double, including infinities and NaN.
    Number,
    /// A string, by reference.
    String,
    /// A symbol, by reference.
    Symbol,
    /// An object, by reference.
    Object,
}

/// A JavaScript value.
///
/// `Copy`, eight bytes, and cheap to pass in a register — which is the entire point, because
/// this type is the calling convention as much as it is a value (ROADMAP §M9).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Value {
    bits: u64,
}

impl Value {
    /// `undefined`.
    pub const UNDEFINED: Self = Self::singleton(SINGLETON_UNDEFINED);
    /// `null`.
    pub const NULL: Self = Self::singleton(SINGLETON_NULL);
    /// `true`.
    pub const TRUE: Self = Self::singleton(SINGLETON_TRUE);
    /// `false`.
    pub const FALSE: Self = Self::singleton(SINGLETON_FALSE);

    const fn singleton(which: u64) -> Self {
        Self {
            bits: TAG_BASE | TAG_SINGLETON | which,
        }
    }

    const fn pointer(tag: u64, address: Address) -> Self {
        Self {
            bits: TAG_BASE | tag | address.0,
        }
    }

    /// A number.
    ///
    /// **NaN is canonicalised.** A NaN carrying an arbitrary payload — which bit manipulation
    /// or a foreign producer can hand over, even though arithmetic will not — could have the
    /// tag bits set and would then be read back as an object, with the mantissa as its
    /// address. Rewriting it to one known NaN costs a predictable branch and removes the
    /// entire class of confusion. JavaScript cannot tell two NaNs apart, so nothing is lost.
    #[must_use]
    pub fn number(value: f64) -> Self {
        if value.is_nan() {
            return Self {
                bits: CANONICAL_NAN,
            };
        }
        Self {
            bits: value.to_bits(),
        }
    }

    /// `true` or `false`.
    #[must_use]
    pub const fn boolean(value: bool) -> Self {
        if value { Self::TRUE } else { Self::FALSE }
    }

    /// An object, by address.
    #[must_use]
    pub const fn object(address: Address) -> Self {
        Self::pointer(TAG_OBJECT, address)
    }

    /// A string, by address.
    #[must_use]
    pub const fn string(address: Address) -> Self {
        Self::pointer(TAG_STRING, address)
    }

    /// A symbol, by address.
    #[must_use]
    pub const fn symbol(address: Address) -> Self {
        Self::pointer(TAG_SYMBOL, address)
    }

    /// Whether this is a number.
    ///
    /// The whole discrimination, and it is one mask and one compare. Every other predicate
    /// below is this question answered first.
    #[must_use]
    pub const fn is_number(self) -> bool {
        self.bits & TAG_BASE != TAG_BASE
    }

    /// What this value is.
    #[must_use]
    pub const fn kind(self) -> Kind {
        if self.is_number() {
            return Kind::Number;
        }
        match self.bits & TAG_MASK {
            TAG_OBJECT => Kind::Object,
            TAG_STRING => Kind::String,
            TAG_SYMBOL => Kind::Symbol,
            // Singleton. The payload says which, and anything unrecognised is `undefined`
            // rather than a panic: this is reached from compiled code, and a value that
            // cannot be constructed through the safe API should not be able to abort a
            // program either.
            _ => match self.bits & PAYLOAD_MASK {
                SINGLETON_NULL => Kind::Null,
                SINGLETON_TRUE | SINGLETON_FALSE => Kind::Boolean,
                _ => Kind::Undefined,
            },
        }
    }

    /// The number, if this is one.
    #[must_use]
    pub fn as_number(self) -> Option<f64> {
        self.is_number().then(|| f64::from_bits(self.bits))
    }

    /// The boolean, if this is one.
    #[must_use]
    pub const fn as_boolean(self) -> Option<bool> {
        if self.bits == Self::TRUE.bits {
            Some(true)
        } else if self.bits == Self::FALSE.bits {
            Some(false)
        } else {
            None
        }
    }

    /// The address, if this value holds one.
    ///
    /// Objects, strings and symbols all do; nothing else does. A collector walks these and
    /// only these, which is why the question is asked once here rather than three times at
    /// every call site.
    #[must_use]
    pub const fn as_address(self) -> Option<Address> {
        if self.is_number() {
            return None;
        }
        match self.bits & TAG_MASK {
            TAG_OBJECT | TAG_STRING | TAG_SYMBOL => Some(Address(self.bits & PAYLOAD_MASK)),
            _ => None,
        }
    }

    /// Whether this is `undefined`.
    #[must_use]
    pub const fn is_undefined(self) -> bool {
        self.bits == Self::UNDEFINED.bits
    }

    /// Whether this is `null`.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.bits == Self::NULL.bits
    }

    /// Whether this is `null` or `undefined`, which JavaScript treats together more often
    /// than it treats them apart.
    #[must_use]
    pub const fn is_nullish(self) -> bool {
        self.is_null() || self.is_undefined()
    }

    /// The raw bits, for a collector or a code generator that needs them.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        self.bits
    }

    /// Rebuilds a value from [`Value::to_bits`].
    ///
    /// Any `u64` is a valid `Value` — every pattern is either a double or a tagged value with
    /// some payload — so this cannot fail. It can still produce a value that no safe
    /// constructor would, which is why [`Value::kind`] has a total answer for every pattern
    /// rather than a panic for the ones it does not expect.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self { bits }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            Kind::Undefined => f.write_str("undefined"),
            Kind::Null => f.write_str("null"),
            Kind::Boolean => write!(f, "{}", self.as_boolean().unwrap_or_default()),
            Kind::Number => write!(f, "{}", self.as_number().unwrap_or(f64::NAN)),
            Kind::String => write!(f, "String({:?})", self.as_address()),
            Kind::Symbol => write!(f, "Symbol({:?})", self.as_address()),
            Kind::Object => write!(f, "Object({:?})", self.as_address()),
        }
    }
}

impl Default for Value {
    /// `undefined`, which is what an uninitialised binding holds.
    fn default() -> Self {
        Self::UNDEFINED
    }
}
