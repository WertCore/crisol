//! `Proxy` and `Reflect`.
//!
//! ROADMAP §3.2 makes `Proxy` a named product risk: it cannot be rejected if the ecosystem is a
//! goal, because Vue 3's reactivity, MobX, Immer, Valtio and Solid stores all depend on it. The
//! resolution there is about *cost* — shapes carry an `is_exotic` bit (D-54) so the fast path
//! branches once. This module is about *correctness*, which is a separate and larger problem.
//!
//! # The traps are not the hard part; the invariants are
//!
//! A trap is a function call. What makes `Proxy` safe to have in a language is that the spec
//! **checks the trap's answer against the target** and throws when they disagree in specific
//! ways. Without those checks, a proxy could report that a frozen property has a different
//! value than it does — and every piece of code that reasoned about `Object.freeze`, including
//! the engine's own optimiser, would be reasoning about a lie.
//!
//! So the invariant checks are the content here, and each one has a test that constructs a
//! lying trap and asserts it is refused:
//!
//! - `get` on a non-configurable, non-writable data property must report **that** value.
//! - `has` may not report `false` for a non-configurable own property.
//! - `deleteProperty` may not report `true` for a non-configurable one.
//! - `isExtensible` must agree with the target exactly.
//! - `getOwnPropertyDescriptor` may not report `undefined` for a non-configurable property.
//!
//! An implementation with the traps and without the checks passes every test that *uses* a
//! proxy and fails the ones that try to break one — which is the direction real code exercises
//! only after someone has shipped a bug.
//!
//! # Revocation
//!
//! A revoked proxy throws on every operation, including `typeof`-adjacent ones. It is the
//! mechanism behind handing out a capability you can take back, and it has to be checked before
//! anything else — a revoked proxy must not consult its handler, because the handler is exactly
//! what revocation is meant to detach.

use crisol_value::{PropertyKey, Value};

use crate::descriptor::Property;

/// Which trap a handler is being asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Trap {
    /// `get`.
    Get,
    /// `set`.
    Set,
    /// `has`.
    Has,
    /// `deleteProperty`.
    DeleteProperty,
    /// `getOwnPropertyDescriptor`.
    GetOwnPropertyDescriptor,
    /// `defineProperty`.
    DefineProperty,
    /// `ownKeys`.
    OwnKeys,
    /// `isExtensible`.
    IsExtensible,
    /// `preventExtensions`.
    PreventExtensions,
    /// `getPrototypeOf`.
    GetPrototypeOf,
    /// `setPrototypeOf`.
    SetPrototypeOf,
}

impl Trap {
    /// The name the handler property has.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Set => "set",
            Self::Has => "has",
            Self::DeleteProperty => "deleteProperty",
            Self::GetOwnPropertyDescriptor => "getOwnPropertyDescriptor",
            Self::DefineProperty => "defineProperty",
            Self::OwnKeys => "ownKeys",
            Self::IsExtensible => "isExtensible",
            Self::PreventExtensions => "preventExtensions",
            Self::GetPrototypeOf => "getPrototypeOf",
            Self::SetPrototypeOf => "setPrototypeOf",
        }
    }
}

/// Why a proxy operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyError {
    /// The proxy has been revoked.
    Revoked,
    /// A trap's answer contradicts the target in a way the spec forbids.
    InvariantViolated {
        /// Which trap lied.
        trap: Trap,
        /// What the contradiction was.
        detail: String,
    },
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Revoked => f.write_str("cannot perform any operation on a revoked proxy"),
            Self::InvariantViolated { trap, detail } => {
                write!(f, "'{}' on proxy: {detail}", trap.name())
            }
        }
    }
}

impl std::error::Error for ProxyError {}

/// What a target looks like to the invariant checks.
///
/// A trait rather than a concrete type because the checks are about the *relationship* between
/// a trap's answer and a target's state, and they should be testable against a hand-built
/// target without a whole realm behind it.
pub trait Target {
    /// The target's own property, if it has one.
    fn own_property(&self, key: &PropertyKey) -> Option<Property>;
    /// Whether the target is extensible.
    fn is_extensible(&self) -> bool;
}

/// A proxy: a target, and whether it is still live.
///
/// The handler is not stored here. Its traps are supplied per call, because calling a
/// JavaScript function is not something this crate can do (the same reason [`crate::Got`] hands
/// getters back) — and the part worth having now is the checking, not the dispatch.
#[derive(Debug)]
pub struct Proxy<T> {
    target: T,
    revoked: bool,
}

impl<T: Target> Proxy<T> {
    /// `new Proxy(target, handler)`.
    pub const fn new(target: T) -> Self {
        Self {
            target,
            revoked: false,
        }
    }

    /// `Proxy.revocable(...).revoke()`.
    pub const fn revoke(&mut self) {
        self.revoked = true;
    }

    /// Whether it has been revoked.
    #[must_use]
    pub const fn is_revoked(&self) -> bool {
        self.revoked
    }

    /// The target, for the checks and for `Reflect`.
    pub const fn target(&self) -> &T {
        &self.target
    }

    /// Checks a `get` trap's answer.
    ///
    /// # Errors
    ///
    /// [`ProxyError::Revoked`], or a violation when the target has a non-configurable,
    /// non-writable data property and the trap reported something else. That property is a
    /// promise the target made; a proxy is not allowed to break it.
    pub fn checked_get(&self, key: &PropertyKey, reported: Value) -> Result<Value, ProxyError> {
        self.alive()?;
        if let Some(Property::Data {
            value,
            writable: false,
            configurable: false,
            ..
        }) = self.target.own_property(key)
            && reported != value
        {
            return Err(ProxyError::InvariantViolated {
                trap: Trap::Get,
                detail: format!(
                    "property {key:?} is non-configurable and non-writable, so it must be \
                     reported as the value the target holds"
                ),
            });
        }
        Ok(reported)
    }

    /// Checks a `set` trap's answer.
    ///
    /// # Errors
    ///
    /// A violation when the trap claims success on a non-configurable, non-writable property
    /// whose value would change.
    pub fn checked_set(
        &self,
        key: &PropertyKey,
        written: Value,
        reported: bool,
    ) -> Result<bool, ProxyError> {
        self.alive()?;
        if reported
            && let Some(Property::Data {
                value,
                writable: false,
                configurable: false,
                ..
            }) = self.target.own_property(key)
            && written != value
        {
            return Err(ProxyError::InvariantViolated {
                trap: Trap::Set,
                detail: format!("cannot change non-configurable, non-writable property {key:?}"),
            });
        }
        Ok(reported)
    }

    /// Checks a `has` trap's answer.
    ///
    /// # Errors
    ///
    /// A violation when the trap hides a non-configurable own property. Hiding one would let a
    /// proxy deny the existence of something that provably cannot be removed.
    pub fn checked_has(&self, key: &PropertyKey, reported: bool) -> Result<bool, ProxyError> {
        self.alive()?;
        if !reported
            && let Some(property) = self.target.own_property(key)
            && !property.configurable()
        {
            return Err(ProxyError::InvariantViolated {
                trap: Trap::Has,
                detail: format!("cannot report non-configurable property {key:?} as absent"),
            });
        }
        Ok(reported)
    }

    /// Checks a `deleteProperty` trap's answer.
    ///
    /// # Errors
    ///
    /// A violation when the trap claims to have deleted a non-configurable property.
    pub fn checked_delete(&self, key: &PropertyKey, reported: bool) -> Result<bool, ProxyError> {
        self.alive()?;
        if reported
            && let Some(property) = self.target.own_property(key)
            && !property.configurable()
        {
            return Err(ProxyError::InvariantViolated {
                trap: Trap::DeleteProperty,
                detail: format!("cannot report non-configurable property {key:?} as deleted"),
            });
        }
        Ok(reported)
    }

    /// Checks a `getOwnPropertyDescriptor` trap's answer.
    ///
    /// # Errors
    ///
    /// A violation when the trap reports `undefined` for a non-configurable property.
    pub fn checked_descriptor(
        &self,
        key: &PropertyKey,
        reported: Option<Property>,
    ) -> Result<Option<Property>, ProxyError> {
        self.alive()?;
        if reported.is_none()
            && let Some(property) = self.target.own_property(key)
            && !property.configurable()
        {
            return Err(ProxyError::InvariantViolated {
                trap: Trap::GetOwnPropertyDescriptor,
                detail: format!("cannot report non-configurable property {key:?} as non-existent"),
            });
        }
        Ok(reported)
    }

    /// Checks an `isExtensible` trap's answer.
    ///
    /// # Errors
    ///
    /// A violation on any disagreement. This one has **no latitude at all** — unlike the
    /// property traps, where a proxy may invent properties the target does not have,
    /// extensibility must match exactly. `Object.isExtensible` is what code uses to decide
    /// whether a shape can still change, and a proxy that lied would invalidate that reasoning
    /// everywhere including inside the engine.
    pub fn checked_is_extensible(&self, reported: bool) -> Result<bool, ProxyError> {
        self.alive()?;
        let actual = self.target.is_extensible();
        if reported != actual {
            return Err(ProxyError::InvariantViolated {
                trap: Trap::IsExtensible,
                detail: format!("must report {actual}, which is what the target is"),
            });
        }
        Ok(reported)
    }

    /// Checks an `ownKeys` trap's answer.
    ///
    /// # Errors
    ///
    /// A violation when a non-configurable key is missing, or when the target is not
    /// extensible and the list does not match its keys exactly.
    pub fn checked_own_keys(
        &self,
        reported: &[PropertyKey],
        target_keys: &[PropertyKey],
    ) -> Result<(), ProxyError> {
        self.alive()?;
        for key in target_keys {
            let non_configurable = self
                .target
                .own_property(key)
                .is_some_and(|property| !property.configurable());
            if non_configurable && !reported.contains(key) {
                return Err(ProxyError::InvariantViolated {
                    trap: Trap::OwnKeys,
                    detail: format!("non-configurable key {key:?} must be listed"),
                });
            }
        }
        if !self.target.is_extensible() {
            // Nothing can be added and nothing can be removed, so the lists must match.
            for key in target_keys {
                if !reported.contains(key) {
                    return Err(ProxyError::InvariantViolated {
                        trap: Trap::OwnKeys,
                        detail: format!("{key:?} is missing and the target is not extensible"),
                    });
                }
            }
            for key in reported {
                if !target_keys.contains(key) {
                    return Err(ProxyError::InvariantViolated {
                        trap: Trap::OwnKeys,
                        detail: format!("{key:?} is invented and the target is not extensible"),
                    });
                }
            }
        }
        Ok(())
    }

    fn alive(&self) -> Result<(), ProxyError> {
        // Checked before anything else, and before the handler is consulted: revocation exists
        // precisely to detach the handler, so asking it first would defeat it.
        if self.revoked {
            return Err(ProxyError::Revoked);
        }
        Ok(())
    }
}
