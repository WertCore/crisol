//! `Reflect`, and the `Boolean` wrapper.
//!
//! # `Reflect` is the non-throwing mirror of the `Object` statics
//!
//! That is the whole reason it exists, and it is one line in the spec that changes how error
//! handling is written:
//!
//! | | on failure |
//! |---|---|
//! | `Object.defineProperty` | **throws** a `TypeError` |
//! | `Reflect.defineProperty` | returns **`false`** |
//!
//! The same split runs through `set`, `deleteProperty`, `preventExtensions` and
//! `setPrototypeOf`. Code that wants to *attempt* an operation and branch on the outcome has to
//! wrap the `Object` form in a `try`, which conflates "this was not allowed" with "something
//! else went wrong inside a getter". `Reflect` separates them.
//!
//! An implementation that made `Reflect.defineProperty` throw would still pass every test that
//! defines a property successfully — the difference only shows on the failure path, which is
//! the path people write least and rely on most.
//!
//! `Reflect.ownKeys` also differs from `Object.keys`: it reports **non-enumerable** properties
//! too, because it mirrors the internal method rather than the iteration helper.
//!
//! # `new Boolean(false)` is truthy
//!
//! `Boolean(x)` is `ToBoolean` and returns a primitive. `new Boolean(x)` returns an **object**,
//! and every object is truthy — so `new Boolean(false)` is truthy, and
//! `if (new Boolean(false))` takes the branch.
//!
//! This is not a quirk to paper over: it follows from objects being truthy, which is the rule
//! that makes `if (obj)` a null check. The wrapper is why `Boolean` should be called without
//! `new`, and there is a test rather than a comment because the assertion reads as a mistake.

use crisol_value::{PropertyKey, Value};

use crate::convert::to_boolean;
use crate::descriptor::{PartialDescriptor, Property};
use crate::object::{Got, ObjectId, Realm, Setting};

/// `Reflect.get`.
///
/// Returns the accessor rather than calling it, for the reason [`Got`] exists.
#[must_use]
pub fn get(realm: &Realm, object: ObjectId, key: &PropertyKey) -> Got {
    realm.get(object, key)
}

/// `Reflect.set`, returning **whether it worked** rather than the value assigned.
///
/// `Object`'s assignment form evaluates to the value, so it cannot report failure at all —
/// in sloppy mode a refused write is silent. This is the version that tells you.
pub fn set(realm: &mut Realm, object: ObjectId, key: &PropertyKey, value: Value) -> bool {
    matches!(realm.set(object, key, value), Setting::Done)
}

/// `Reflect.has` — the `in` operator as a function, prototype chain included.
#[must_use]
pub fn has(realm: &Realm, object: ObjectId, key: &PropertyKey) -> bool {
    realm.has_property(object, key)
}

/// `Reflect.deleteProperty`.
pub fn delete_property(realm: &mut Realm, object: ObjectId, key: &PropertyKey) -> bool {
    realm.delete(object, key)
}

/// `Reflect.defineProperty` — **returns `false` where `Object.defineProperty` throws**.
///
/// The difference only shows on the failure path, which is the path people write least and
/// rely on most.
pub fn define_property(
    realm: &mut Realm,
    object: ObjectId,
    key: PropertyKey,
    descriptor: &PartialDescriptor,
) -> bool {
    realm.define_own_property(object, key, descriptor).is_ok()
}

/// `Reflect.getOwnPropertyDescriptor`.
#[must_use]
pub fn own_property_descriptor(
    realm: &Realm,
    object: ObjectId,
    key: &PropertyKey,
) -> Option<Property> {
    realm.own_property(object, key).cloned()
}

/// `Reflect.ownKeys`.
///
/// **Includes non-enumerable properties**, unlike `Object.keys` — it mirrors the internal
/// method, not the iteration helper.
#[must_use]
pub fn own_keys(realm: &Realm, object: ObjectId) -> Vec<PropertyKey> {
    realm.own_keys(object)
}

/// `Reflect.getPrototypeOf`.
#[must_use]
pub fn prototype_of(realm: &Realm, object: ObjectId) -> Option<ObjectId> {
    realm.prototype_of(object)
}

/// `Reflect.setPrototypeOf`, returning whether it was allowed.
pub fn set_prototype_of(realm: &mut Realm, object: ObjectId, prototype: Option<ObjectId>) -> bool {
    realm.set_prototype_of(object, prototype)
}

/// `Reflect.isExtensible`.
#[must_use]
pub fn is_extensible(realm: &Realm, object: ObjectId) -> bool {
    realm.is_extensible(object)
}

/// `Reflect.preventExtensions`.
pub fn prevent_extensions(realm: &mut Realm, object: ObjectId) -> bool {
    realm.prevent_extensions(object)
}

// ---- Boolean ---------------------------------------------------------------------------------

/// `Boolean(value)` — called **without** `new`, giving a primitive.
#[must_use]
pub fn boolean(value: Value) -> bool {
    to_boolean(value)
}

/// A `Boolean` wrapper object, as `new Boolean(value)` gives.
///
/// Exists so the truthiness rule can be stated in the type: a wrapper is an object, every
/// object is truthy, therefore **`new Boolean(false)` is truthy**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BooleanObject {
    /// The wrapped primitive, which `valueOf` returns.
    pub value: bool,
}

impl BooleanObject {
    /// `new Boolean(value)`.
    #[must_use]
    pub fn new(value: Value) -> Self {
        Self {
            value: to_boolean(value),
        }
    }

    /// `booleanObject.valueOf()` — the primitive back out.
    #[must_use]
    pub const fn value_of(self) -> bool {
        self.value
    }

    /// Whether `if (wrapper)` takes the branch.
    ///
    /// **Always true.** It follows from objects being truthy, which is the same rule that makes
    /// `if (obj)` a null check — not a special case for `Boolean`.
    #[must_use]
    pub const fn is_truthy(self) -> bool {
        true
    }
}
