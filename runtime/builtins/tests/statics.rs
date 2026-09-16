//! `Object.*` and `Number.*` statics.
//!
//! The two groups have a trap each: `isFrozen` is vacuously true for an object nobody froze,
//! and `Number.isNaN` is a different function from the global `isNaN`.

use crisol_builtins::{
    MAX_SAFE_INTEGER, PartialDescriptor, Realm, assign, entries, freeze, from_entries,
    global_is_finite, global_is_nan, is_frozen, is_integer, is_safe_integer, is_sealed, keys,
    number_is_finite, number_is_nan, own_property_names, seal, values,
};
use crisol_value::{PropertyKey, Value};

fn key(name: &str) -> PropertyKey {
    PropertyKey::new(name)
}

fn n(value: f64) -> Value {
    Value::number(value)
}

/// What assignment creates: writable, enumerable, configurable.
fn permissive(value: Value) -> PartialDescriptor {
    PartialDescriptor {
        value: Some(value),
        writable: Some(true),
        enumerable: Some(true),
        configurable: Some(true),
        ..PartialDescriptor::default()
    }
}

fn hidden(value: Value) -> PartialDescriptor {
    PartialDescriptor {
        value: Some(value),
        writable: Some(true),
        enumerable: Some(false),
        configurable: Some(true),
        ..PartialDescriptor::default()
    }
}

/// An object with `a` and `b`, both ordinary.
fn pair(realm: &mut Realm) -> crisol_builtins::ObjectId {
    let object = realm.create(None);
    realm
        .define_own_property(object, key("a"), &permissive(n(1.0)))
        .expect("defined");
    realm
        .define_own_property(object, key("b"), &permissive(n(2.0)))
        .expect("defined");
    object
}

// ---- sealing and freezing -------------------------------------------------------------------

#[test]
fn an_object_nobody_froze_can_report_frozen() {
    // `Object.isFrozen(Object.preventExtensions({}))` is true. Every condition in the
    // definition holds over an empty set of properties. Code branching on `isFrozen` to decide
    // whether it may mutate will take the frozen path here, and no engine differs.
    let mut realm = Realm::new();
    let empty = realm.create(None);
    assert!(!is_frozen(&realm, empty), "extensible, so not frozen");

    realm.prevent_extensions(empty);
    assert!(is_frozen(&realm, empty), "vacuously frozen");
    assert!(is_sealed(&realm, empty), "and vacuously sealed");
}

#[test]
fn seal_and_freeze_differ_by_one_bit() {
    // A sealed object's values may still change; only its shape is fixed.
    let mut realm = Realm::new();
    let sealed = pair(&mut realm);
    seal(&mut realm, sealed);

    assert!(is_sealed(&realm, sealed));
    assert!(!is_frozen(&realm, sealed), "writable data is not frozen");
    assert_eq!(
        realm.set(sealed, &key("a"), n(9.0)),
        crisol_builtins::Setting::Done,
        "a sealed object still accepts a write"
    );

    let frozen = pair(&mut realm);
    freeze(&mut realm, frozen);
    assert!(is_sealed(&realm, frozen), "freezing implies sealing");
    assert!(is_frozen(&realm, frozen));
    assert_eq!(
        realm.set(frozen, &key("a"), n(9.0)),
        crisol_builtins::Setting::Refused
    );
}

#[test]
fn sealing_stops_new_properties_and_deletion() {
    let mut realm = Realm::new();
    let object = pair(&mut realm);
    seal(&mut realm, object);

    assert!(!realm.is_extensible(object));
    assert!(!realm.delete(object, &key("a")), "non-configurable now");
    assert_eq!(
        realm.set(object, &key("new"), n(1.0)),
        crisol_builtins::Setting::Refused
    );
}

#[test]
fn freezing_an_accessor_does_not_need_it_to_be_writable() {
    // An accessor has no `writable` attribute, so asking for one would be rejected — and an
    // accessor does not block the object from being frozen.
    let mut realm = Realm::new();
    let object = realm.create(None);
    realm
        .define_own_property(
            object,
            key("x"),
            &PartialDescriptor {
                get: Some(Value::NULL),
                enumerable: Some(true),
                configurable: Some(true),
                ..PartialDescriptor::default()
            },
        )
        .expect("defined");

    freeze(&mut realm, object);
    assert!(is_frozen(&realm, object));
}

// ---- which keys each static sees --------------------------------------------------------------

#[test]
fn keys_skips_non_enumerable_and_own_property_names_does_not() {
    // The whole reason both exist: `keys` is what you iterate, `getOwnPropertyNames` is what
    // you inspect.
    let mut realm = Realm::new();
    let object = realm.create(None);
    realm
        .define_own_property(object, key("shown"), &permissive(n(1.0)))
        .expect("defined");
    realm
        .define_own_property(object, key("hidden"), &hidden(n(2.0)))
        .expect("defined");

    let visible: Vec<String> = keys(&realm, object)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(visible, ["shown"]);

    let all: Vec<String> = own_property_names(&realm, object)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(all, ["shown", "hidden"]);
}

#[test]
fn assign_copies_only_enumerable_properties() {
    // Which is why assigning from a class instance does not bring its methods, and why this is
    // not a clone.
    let mut realm = Realm::new();
    let source = realm.create(None);
    realm
        .define_own_property(source, key("shown"), &permissive(n(1.0)))
        .expect("defined");
    realm
        .define_own_property(source, key("hidden"), &hidden(n(2.0)))
        .expect("defined");

    let target = realm.create(None);
    assign(&mut realm, target, source);

    let copied: Vec<String> = keys(&realm, target)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(copied, ["shown"]);
    assert!(realm.own_property(target, &key("hidden")).is_none());
}

#[test]
fn entries_and_from_entries_round_trip() {
    let mut realm = Realm::new();
    let object = pair(&mut realm);
    let pairs = entries(&realm, object);
    assert_eq!(pairs.len(), 2);

    let rebuilt = from_entries(&mut realm, &pairs);
    assert_eq!(
        entries(&realm, rebuilt),
        pairs,
        "same keys, same values, same order"
    );
}

#[test]
fn values_follows_keys() {
    let mut realm = Realm::new();
    let object = pair(&mut realm);
    assert_eq!(values(&realm, object), [n(1.0), n(2.0)]);
}

// ---- Number, and the globals it is confused with -----------------------------------------------

#[test]
fn number_is_nan_does_not_coerce_and_the_global_does() {
    // Reaching for whichever is in scope is how a string that looks numeric passes a guard
    // meant to reject it.
    assert!(number_is_nan(n(f64::NAN)));
    assert!(
        !number_is_nan(Value::UNDEFINED),
        "undefined is not the number NaN"
    );

    assert!(
        global_is_nan(Value::UNDEFINED),
        "but ToNumber(undefined) is NaN"
    );
    assert!(!global_is_nan(Value::NULL), "and ToNumber(null) is 0");
    assert!(!global_is_nan(n(1.0)));
}

#[test]
fn number_is_finite_does_not_coerce_and_the_global_does() {
    assert!(number_is_finite(n(1.0)));
    assert!(!number_is_finite(n(f64::INFINITY)));
    assert!(
        !number_is_finite(Value::TRUE),
        "true is not a finite number, it is a boolean"
    );
    assert!(global_is_finite(Value::TRUE), "but ToNumber(true) is 1");
    assert!(!global_is_finite(Value::UNDEFINED));
}

#[test]
fn every_whole_double_is_an_integer() {
    // There is no separate integer type for this to distinguish: `5` *is* `5.0`.
    assert!(is_integer(5.0));
    assert!(is_integer(-0.0));
    assert!(!is_integer(5.5));
    assert!(!is_integer(f64::NAN));
    assert!(!is_integer(f64::INFINITY), "infinity is not an integer");
}

#[test]
fn safety_stops_at_two_to_the_fifty_three() {
    // Beyond this the doubles are further apart than 1, so an id past the boundary silently
    // collides with its neighbour — which is why APIs with 64-bit ids send them as strings.
    assert!(is_safe_integer(MAX_SAFE_INTEGER));
    assert!(!is_safe_integer(MAX_SAFE_INTEGER + 1.0));
    assert!(is_safe_integer(-MAX_SAFE_INTEGER));
    assert!(!is_safe_integer(1.5));

    // The collision itself, which is what the boundary is protecting against.
    let beyond = MAX_SAFE_INTEGER + 1.0;
    assert_eq!(
        beyond,
        beyond + 1.0,
        "past the boundary, two different integers are the same double"
    );
}
