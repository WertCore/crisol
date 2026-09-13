//! Property descriptors, and the rules for changing one.
//!
//! This is the spec's `ValidateAndApplyPropertyDescriptor` (ECMA-262 10.1.6.3) written out. It
//! looks like bookkeeping and is not: it is the single place that decides whether
//! `Object.defineProperty` is allowed to do what it was asked, and getting it wrong means
//! either rejecting legal programs or letting a frozen object be modified.
//!
//! # Why it is a table, not a few `if`s
//!
//! The rules are asymmetric in ways that do not follow from intuition:
//!
//! - A **non-configurable** property can still have its value changed, if it is also
//!   **writable**. Freezing requires both bits.
//! - A non-configurable, **non-writable** property can be "changed" to the value it already
//!   has. The spec says *SameValue*, so `NaN` to `NaN` is allowed and `0` to `-0` is not —
//!   which is why [`crisol_value::Value`]'s derived equality being `Object.is` (D-53) is load
//!   bearing here rather than a curiosity.
//! - A non-configurable property may go from writable to non-writable — one way only.
//! - Turning a data property into an accessor, or back, needs `configurable`.
//!
//! Each of those is a line in the spec and a test below. The shape of the code follows the
//! shape of the specification on purpose: when the two disagree, the diff should be obvious.

use crisol_value::{PropertyKey, Value};

/// A property, fully specified.
///
/// The spec's "complete property descriptor": every attribute has a value, unlike
/// [`PartialDescriptor`], where absent means "leave alone".
#[derive(Clone, Debug, PartialEq)]
pub enum Property {
    /// A value, and whether it can be written.
    Data {
        /// The value.
        value: Value,
        /// Whether assignment can change it.
        writable: bool,
        /// Whether it shows up in `for…in` and `Object.keys`.
        enumerable: bool,
        /// Whether its attributes can change, or it can be deleted.
        configurable: bool,
    },
    /// A getter and/or setter.
    Accessor {
        /// Called on read. `undefined` when there is none.
        get: Value,
        /// Called on write. `undefined` when there is none.
        set: Value,
        /// Whether it shows up in `for…in` and `Object.keys`.
        enumerable: bool,
        /// Whether its attributes can change, or it can be deleted.
        configurable: bool,
    },
}

impl Property {
    /// A plain `{ value, writable: true, enumerable: true, configurable: true }`.
    ///
    /// What assignment to a new property creates.
    #[must_use]
    pub const fn data(value: Value) -> Self {
        Self::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
        }
    }

    /// Whether attributes may still be changed.
    #[must_use]
    pub const fn configurable(&self) -> bool {
        match self {
            Self::Data { configurable, .. } | Self::Accessor { configurable, .. } => *configurable,
        }
    }

    /// Whether it appears in `Object.keys`.
    #[must_use]
    pub const fn enumerable(&self) -> bool {
        match self {
            Self::Data { enumerable, .. } | Self::Accessor { enumerable, .. } => *enumerable,
        }
    }

    /// Whether this is a data property.
    #[must_use]
    pub const fn is_data(&self) -> bool {
        matches!(self, Self::Data { .. })
    }
}

/// A descriptor where absent attributes mean "leave whatever is there".
///
/// What `Object.defineProperty` is handed: `{ value: 1 }` says nothing about `enumerable`, and
/// what that means depends on whether the property already exists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartialDescriptor {
    /// The new value.
    pub value: Option<Value>,
    /// The new getter.
    pub get: Option<Value>,
    /// The new setter.
    pub set: Option<Value>,
    /// The new writability.
    pub writable: Option<bool>,
    /// The new enumerability.
    pub enumerable: Option<bool>,
    /// The new configurability.
    pub configurable: Option<bool>,
}

impl PartialDescriptor {
    /// `{ value }` and nothing else.
    #[must_use]
    pub fn value(value: Value) -> Self {
        Self {
            value: Some(value),
            ..Self::default()
        }
    }

    /// Whether this mentions anything only a data property has.
    #[must_use]
    pub const fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    /// Whether this mentions anything only an accessor has.
    #[must_use]
    pub const fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    /// Whether this says nothing at all, which is always allowed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.value.is_none()
            && self.get.is_none()
            && self.set.is_none()
            && self.writable.is_none()
            && self.enumerable.is_none()
            && self.configurable.is_none()
    }
}

/// Why a define was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// The object is not extensible and the property does not exist.
    NotExtensible,
    /// The property is non-configurable and the change is not one of the permitted ones.
    NotConfigurable,
}

/// The spec's `ValidateAndApplyPropertyDescriptor`, for an ordinary object.
///
/// `current` is the property as it is, or `None` if there is none. Returns what the property
/// should become, or why the change is refused.
///
/// # Errors
///
/// [`Rejected`] naming which rule refused it.
pub fn validate_and_apply(
    current: Option<&Property>,
    descriptor: &PartialDescriptor,
    extensible: bool,
) -> Result<Property, Rejected> {
    let Some(current) = current else {
        // Step 2: creating a property needs an extensible object.
        if !extensible {
            return Err(Rejected::NotExtensible);
        }
        return Ok(create(descriptor));
    };

    // Step 3: a descriptor with no fields asks for nothing, and nothing is always allowed —
    // even on a non-configurable property, and even on a frozen object.
    if descriptor.is_empty() {
        return Ok(current.clone());
    }

    if !current.configurable() {
        // Step 4: the changes a non-configurable property refuses.
        if descriptor.configurable == Some(true) {
            return Err(Rejected::NotConfigurable);
        }
        if let Some(enumerable) = descriptor.enumerable
            && enumerable != current.enumerable()
        {
            return Err(Rejected::NotConfigurable);
        }
        // Changing kind needs configurable, in either direction.
        if descriptor.is_accessor() && current.is_data() {
            return Err(Rejected::NotConfigurable);
        }
        if descriptor.is_data() && !current.is_data() {
            return Err(Rejected::NotConfigurable);
        }

        match current {
            Property::Data {
                value, writable, ..
            } => {
                if !*writable {
                    // Non-configurable *and* non-writable: frozen. Writability may not go
                    // back up, and the value may only be "changed" to the one it already has.
                    if descriptor.writable == Some(true) {
                        return Err(Rejected::NotConfigurable);
                    }
                    if let Some(new) = descriptor.value
                        && new != *value
                    {
                        // `Value`'s equality is `Object.is` (D-53), which is exactly what the
                        // spec asks for here: NaN to NaN is allowed, 0 to -0 is not.
                        return Err(Rejected::NotConfigurable);
                    }
                }
                // Writable but non-configurable: the value may change freely, and writability
                // may be turned off. That asymmetry is why freezing needs both bits.
            }
            Property::Accessor { get, set, .. } => {
                if let Some(new) = descriptor.get
                    && new != *get
                {
                    return Err(Rejected::NotConfigurable);
                }
                if let Some(new) = descriptor.set
                    && new != *set
                {
                    return Err(Rejected::NotConfigurable);
                }
            }
        }
    }

    Ok(apply(current, descriptor))
}

/// A brand new property from a partial descriptor.
///
/// Absent attributes default to `false` and absent values to `undefined` — the spec's
/// "generic" completion. Note this is the opposite of what assignment produces: `obj.x = 1`
/// creates a fully-permissive property, `Object.defineProperty(obj, "x", {})` creates a
/// locked-down one.
fn create(descriptor: &PartialDescriptor) -> Property {
    if descriptor.is_accessor() {
        return Property::Accessor {
            get: descriptor.get.unwrap_or(Value::UNDEFINED),
            set: descriptor.set.unwrap_or(Value::UNDEFINED),
            enumerable: descriptor.enumerable.unwrap_or(false),
            configurable: descriptor.configurable.unwrap_or(false),
        };
    }
    Property::Data {
        value: descriptor.value.unwrap_or(Value::UNDEFINED),
        writable: descriptor.writable.unwrap_or(false),
        enumerable: descriptor.enumerable.unwrap_or(false),
        configurable: descriptor.configurable.unwrap_or(false),
    }
}

/// Applies a validated descriptor to an existing property.
fn apply(current: &Property, descriptor: &PartialDescriptor) -> Property {
    let enumerable = descriptor.enumerable.unwrap_or(current.enumerable());
    let configurable = descriptor.configurable.unwrap_or(current.configurable());

    // Changing kind: the attributes the old kind had that the new one does not are dropped,
    // and the new kind's own attributes default rather than being carried over.
    if descriptor.is_accessor() && current.is_data() {
        return Property::Accessor {
            get: descriptor.get.unwrap_or(Value::UNDEFINED),
            set: descriptor.set.unwrap_or(Value::UNDEFINED),
            enumerable,
            configurable,
        };
    }
    if descriptor.is_data() && !current.is_data() {
        return Property::Data {
            value: descriptor.value.unwrap_or(Value::UNDEFINED),
            writable: descriptor.writable.unwrap_or(false),
            enumerable,
            configurable,
        };
    }

    match current {
        Property::Data {
            value, writable, ..
        } => Property::Data {
            value: descriptor.value.unwrap_or(*value),
            writable: descriptor.writable.unwrap_or(*writable),
            enumerable,
            configurable,
        },
        Property::Accessor { get, set, .. } => Property::Accessor {
            get: descriptor.get.unwrap_or(*get),
            set: descriptor.set.unwrap_or(*set),
            enumerable,
            configurable,
        },
    }
}

/// A key and the property it names, for [`crate::Realm::own_keys`].
pub type Entry = (PropertyKey, Property);
