//! The type lattice.
//!
//! ```text
//!                      Unknown            ⊤  — could be anything
//!        ┌────────┬────────┬────────┬────────┐
//!    Undefined   Null    Bool    Number   String   Object(?)
//!                                                  ┌────┴────┐
//!                                            Object(s₁)  Object(s₂)
//!        └────────┴────────┴────────┴────────┘
//!                       Never              ⊥  — no value reaches here
//! ```
//!
//! Deliberately shallow. A lattice with more structure infers more, and every level of it is
//! another place for the analysis to be subtly wrong in a way that produces *faster incorrect
//! code*. This one answers the question codegen actually asks — "can I skip the check?" — and
//! says `Unknown` whenever it cannot be sure, which is always safe and merely slow.
//!
//! `Object` carries an optional shape because that is where specialisation comes from (D-54):
//! a property access on `Object(Some(s))` resolves to a slot at compile time, and one on
//! `Object(None)` does not. Joining two different shapes has to forget both — the alternative
//! is picking one, which is how a field is read from the wrong offset.

use std::fmt;

use crisol_value::ShapeId;

/// What a value can be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// No value reaches here. The lattice's bottom, and what an unreachable block's
    /// parameters have.
    Never,
    /// `undefined`.
    Undefined,
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool,
    /// A double.
    Number,
    /// A string.
    String,
    /// An object. `Some(shape)` when every value here has that shape and property access can
    /// be resolved to a slot; `None` when it is an object but which one is not known.
    Object(Option<ShapeId>),
    /// Could be anything. The lattice's top.
    Unknown,
}

impl Type {
    /// An object of known shape.
    #[must_use]
    pub const fn object(shape: ShapeId) -> Self {
        Self::Object(Some(shape))
    }

    /// Whether every value of this type is also a value of `other`.
    ///
    /// The lattice's ordering. `Never ⊑ t ⊑ Unknown` for every `t`.
    #[must_use]
    pub fn is_subtype_of(self, other: Self) -> bool {
        match (self, other) {
            (Self::Never, _) | (_, Self::Unknown) => true,
            (Self::Object(Some(_)), Self::Object(None)) => true,
            _ => self == other,
        }
    }

    /// The least type that contains both.
    ///
    /// Used where control flow merges: a value that is a `Number` on one path and a `String`
    /// on another is `Unknown` afterwards, because nothing weaker is true of it.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        if self == other {
            return self;
        }
        match (self, other) {
            (Self::Never, t) | (t, Self::Never) => t,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            // Two objects are still an object. Which one is no longer known, and guessing
            // would put a property read at the wrong slot.
            (Self::Object(_), Self::Object(_)) => Self::Object(None),
            _ => Self::Unknown,
        }
    }

    /// Whether a value of this type is known to be an object.
    #[must_use]
    pub const fn is_object(self) -> bool {
        matches!(self, Self::Object(_))
    }

    /// The shape, when one is known.
    ///
    /// `Some` is the whole of specialisation: it is what lets a property access become a slot
    /// index instead of a lookup.
    #[must_use]
    pub const fn shape(self) -> Option<ShapeId> {
        match self {
            Self::Object(shape) => shape,
            _ => None,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Never => f.write_str("never"),
            Self::Undefined => f.write_str("undefined"),
            Self::Null => f.write_str("null"),
            Self::Bool => f.write_str("bool"),
            Self::Number => f.write_str("number"),
            Self::String => f.write_str("string"),
            Self::Object(None) => f.write_str("object"),
            Self::Object(Some(shape)) => write!(f, "object#{}", shape.index()),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}
