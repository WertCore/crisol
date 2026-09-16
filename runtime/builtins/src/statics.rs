//! `Object.*` and `Number.*` statics.
//!
//! # `Object.isFrozen` is vacuously true more often than people expect
//!
//! `Object.isFrozen(Object.preventExtensions({}))` is **true**. Freeze was never called; the
//! object simply has no properties that could be changed and cannot gain any, so every
//! condition in the definition holds over an empty set. The same goes for `isSealed`.
//!
//! That is not a curiosity — code that branches on `isFrozen` to decide whether it may mutate
//! will take the frozen path for an object nobody froze, and an implementation that "corrects"
//! this by tracking a `frozen` flag would disagree with every engine.
//!
//! # `seal` and `freeze` differ by one bit
//!
//! `seal` makes every own property non-configurable and stops extensions. `freeze` does that
//! **and** makes data properties non-writable. So a sealed object's values can still change;
//! only its shape is fixed. Conflating them gives an object that reports sealed and silently
//! accepts writes, or one that rejects writes nobody asked it to reject.
//!
//! # `Number.isNaN` and the global `isNaN` are different functions
//!
//! | | `"NaN"` | `"1"` | `undefined` |
//! |---|---|---|---|
//! | `Number.isNaN` | `false` | `false` | `false` |
//! | global `isNaN` | **`true`** | `false` | **`true`** |
//!
//! The global coerces first; the `Number` one does not. The same split exists for
//! `Number.isFinite` versus global `isFinite`. Reaching for whichever is in scope is how a
//! string that looks numeric passes a guard meant to reject it.

use crisol_value::{PropertyKey, Value};

use crate::convert::to_number;
use crate::descriptor::{PartialDescriptor, Property};
use crate::object::{ObjectId, Realm};

/// `Object.keys` — own, enumerable, string-keyed.
#[must_use]
pub fn keys(realm: &Realm, object: ObjectId) -> Vec<PropertyKey> {
    realm.enumerable_keys(object)
}

/// `Object.values`.
///
/// Reads through `[[Get]]`, so an accessor is *invoked*. Here that means an accessor's value
/// cannot be produced, and the key is skipped rather than reported as `undefined` — reporting
/// `undefined` would be indistinguishable from a getter that really returns it.
#[must_use]
pub fn values(realm: &Realm, object: ObjectId) -> Vec<Value> {
    keys(realm, object)
        .into_iter()
        .filter_map(|key| match realm.own_property(object, &key) {
            Some(Property::Data { value, .. }) => Some(*value),
            _ => None,
        })
        .collect()
}

/// `Object.entries`.
#[must_use]
pub fn entries(realm: &Realm, object: ObjectId) -> Vec<(PropertyKey, Value)> {
    keys(realm, object)
        .into_iter()
        .filter_map(|key| match realm.own_property(object, &key) {
            Some(Property::Data { value, .. }) => Some((key, *value)),
            _ => None,
        })
        .collect()
}

/// `Object.getOwnPropertyNames` — own and string-keyed, **including non-enumerable**.
///
/// The difference from `Object.keys` is the whole reason both exist: `keys` is what you
/// iterate, `getOwnPropertyNames` is what you inspect.
#[must_use]
pub fn own_property_names(realm: &Realm, object: ObjectId) -> Vec<PropertyKey> {
    realm.own_keys(object)
}

/// `Object.assign(target, source)`.
///
/// Copies own **enumerable** properties. Shallow, and non-enumerable properties are skipped —
/// which is why assigning from a class instance does not bring its methods, and why this is
/// not a clone.
pub fn assign(realm: &mut Realm, target: ObjectId, source: ObjectId) {
    for key in realm.enumerable_keys(source) {
        let Some(Property::Data { value, .. }) = realm.own_property(source, &key) else {
            continue;
        };
        let value = *value;
        realm.set(target, &key, value);
    }
}

/// `Object.fromEntries`.
pub fn from_entries(realm: &mut Realm, pairs: &[(PropertyKey, Value)]) -> ObjectId {
    let object = realm.create(None);
    for (key, value) in pairs {
        let _ = realm.define_own_property(
            object,
            key.clone(),
            &PartialDescriptor {
                value: Some(*value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..PartialDescriptor::default()
            },
        );
    }
    object
}

/// `Object.seal` — no new properties, and no existing one may be reconfigured or deleted.
///
/// Values may still change. That one bit is the whole difference from [`freeze`].
pub fn seal(realm: &mut Realm, object: ObjectId) {
    realm.prevent_extensions(object);
    for key in realm.own_keys(object) {
        let _ = realm.define_own_property(
            object,
            key,
            &PartialDescriptor {
                configurable: Some(false),
                ..PartialDescriptor::default()
            },
        );
    }
}

/// `Object.freeze` — [`seal`], plus data properties become non-writable.
pub fn freeze(realm: &mut Realm, object: ObjectId) {
    realm.prevent_extensions(object);
    for key in realm.own_keys(object) {
        let is_data = realm
            .own_property(object, &key)
            .is_some_and(Property::is_data);
        let _ = realm.define_own_property(
            object,
            key,
            &PartialDescriptor {
                // An accessor has no `writable`, and asking for one on it would be rejected.
                writable: is_data.then_some(false),
                configurable: Some(false),
                ..PartialDescriptor::default()
            },
        );
    }
}

/// `Object.isSealed`.
///
/// **Vacuously true for a non-extensible object with no properties.** Every condition holds
/// over an empty set, and no engine treats that differently.
#[must_use]
pub fn is_sealed(realm: &Realm, object: ObjectId) -> bool {
    if realm.is_extensible(object) {
        return false;
    }
    realm.own_keys(object).into_iter().all(|key| {
        realm
            .own_property(object, &key)
            .is_none_or(|property| !property.configurable())
    })
}

/// `Object.isFrozen` — sealed, and every data property non-writable.
#[must_use]
pub fn is_frozen(realm: &Realm, object: ObjectId) -> bool {
    if realm.is_extensible(object) {
        return false;
    }
    realm.own_keys(object).into_iter().all(|key| {
        realm.own_property(object, &key).is_none_or(|property| {
            !property.configurable()
                && match property {
                    Property::Data { writable, .. } => !writable,
                    // An accessor has nothing writable about it, so it does not block freezing.
                    Property::Accessor { .. } => true,
                }
        })
    })
}

// ---- Number ----------------------------------------------------------------------------

/// The largest integer that `f64` represents exactly: `2^53 - 1`.
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `Number.isInteger`.
///
/// True for `5.0`, because every JavaScript number is a double and `5` *is* `5.0`. There is no
/// separate integer type for this to distinguish.
#[must_use]
pub fn is_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0
}

/// `Number.isSafeInteger` — an integer `f64` represents exactly.
///
/// Beyond `2^53 - 1` the doubles are further apart than 1, so `2^53` and `2^53 + 1` are the
/// same value. An id past this point silently collides with its neighbour, which is why JSON
/// APIs that use 64-bit ids send them as strings.
#[must_use]
pub fn is_safe_integer(value: f64) -> bool {
    is_integer(value) && value.abs() <= MAX_SAFE_INTEGER
}

/// `Number.isNaN` — **no coercion**.
///
/// `Number.isNaN("NaN")` is `false`, because a string is not the number `NaN`. The global
/// `isNaN` coerces first and says `true`. See [`global_is_nan`].
#[must_use]
pub fn number_is_nan(value: Value) -> bool {
    value.as_number().is_some_and(f64::is_nan)
}

/// `Number.isFinite` — no coercion. `Number.isFinite("1")` is `false`.
#[must_use]
pub fn number_is_finite(value: Value) -> bool {
    value.as_number().is_some_and(f64::is_finite)
}

/// The **global** `isNaN`, which coerces first.
///
/// `isNaN("NaN")` is `true` and `isNaN(undefined)` is `true`, because `ToNumber` makes both
/// `NaN`. Kept next to [`number_is_nan`] so the difference is visible at the point where
/// someone picks one.
#[must_use]
pub fn global_is_nan(value: Value) -> bool {
    to_number(value).is_none_or(f64::is_nan)
}

/// The **global** `isFinite`, which coerces first. `isFinite("1")` is `true`.
#[must_use]
pub fn global_is_finite(value: Value) -> bool {
    to_number(value).is_some_and(f64::is_finite)
}
