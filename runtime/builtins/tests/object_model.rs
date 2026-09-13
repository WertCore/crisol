//! The object model, against what ECMA-262 actually says.
//!
//! Most of these are rules that do not follow from intuition, which is the reason they are
//! tests rather than comments: `Object.freeze` needing two bits, a frozen *prototype* blocking
//! assignment on a child, and `Object.keys` reordering integer-like keys are all things a
//! reasonable implementation gets wrong by being reasonable.

use crisol_builtins::{
    Got, PartialDescriptor, Property, Realm, Rejected, Setting, validate_and_apply,
};
use crisol_value::{PropertyKey, Value};

fn key(name: &str) -> PropertyKey {
    PropertyKey::new(name)
}

fn number(value: f64) -> Value {
    Value::number(value)
}

/// What assignment would create: writable, enumerable, configurable.
///
/// Spelled out because `PartialDescriptor::value` defaults every attribute to `false`, which
/// is what the spec says `Object.defineProperty` does — and is emphatically not what a
/// "normal" property looks like. Two tests here were written with `value()` meaning "an
/// ordinary property" and failed, which is the asymmetry proving itself.
fn permissive(value: Value) -> PartialDescriptor {
    PartialDescriptor {
        value: Some(value),
        writable: Some(true),
        enumerable: Some(true),
        configurable: Some(true),
        ..PartialDescriptor::default()
    }
}

// ---- validate_and_apply: the asymmetries -----------------------------------------------

#[test]
fn freezing_takes_two_bits_not_one() {
    // Non-configurable but still writable: the value may change. This is the one people
    // expect `configurable: false` alone to prevent, and it does not.
    let writable = Property::Data {
        value: number(1.0),
        writable: true,
        enumerable: true,
        configurable: false,
    };
    assert!(
        validate_and_apply(
            Some(&writable),
            &PartialDescriptor::value(number(2.0)),
            true
        )
        .is_ok(),
        "non-configurable but writable still accepts a new value"
    );

    let frozen = Property::Data {
        value: number(1.0),
        writable: false,
        enumerable: true,
        configurable: false,
    };
    assert_eq!(
        validate_and_apply(Some(&frozen), &PartialDescriptor::value(number(2.0)), true),
        Err(Rejected::NotConfigurable),
        "both bits off is what freezing means"
    );
}

#[test]
fn a_frozen_property_may_be_set_to_the_value_it_already_has() {
    let frozen = Property::Data {
        value: number(1.0),
        writable: false,
        enumerable: false,
        configurable: false,
    };
    assert!(
        validate_and_apply(Some(&frozen), &PartialDescriptor::value(number(1.0)), true).is_ok(),
        "the spec asks for SameValue, and this is the same value"
    );
}

#[test]
fn same_value_means_nan_is_allowed_and_negative_zero_is_not() {
    // The spec says SameValue, which is `Object.is` — and `Value`'s derived equality is
    // exactly that (D-53), which is why this works rather than needing a special case.
    let nan = Property::Data {
        value: number(f64::NAN),
        writable: false,
        enumerable: false,
        configurable: false,
    };
    assert!(
        validate_and_apply(
            Some(&nan),
            &PartialDescriptor::value(number(f64::NAN)),
            true
        )
        .is_ok(),
        "Object.is(NaN, NaN) is true, so this is not a change"
    );

    let zero = Property::Data {
        value: number(0.0),
        writable: false,
        enumerable: false,
        configurable: false,
    };
    assert_eq!(
        validate_and_apply(Some(&zero), &PartialDescriptor::value(number(-0.0)), true),
        Err(Rejected::NotConfigurable),
        "Object.is(0, -0) is false, so this is a change and is refused"
    );
}

#[test]
fn writability_may_be_turned_off_but_not_back_on() {
    let base = |writable: bool| Property::Data {
        value: number(1.0),
        writable,
        enumerable: false,
        configurable: false,
    };
    let off = PartialDescriptor {
        writable: Some(false),
        ..PartialDescriptor::default()
    };
    let on = PartialDescriptor {
        writable: Some(true),
        ..PartialDescriptor::default()
    };

    assert!(
        validate_and_apply(Some(&base(true)), &off, true).is_ok(),
        "one way"
    );
    assert_eq!(
        validate_and_apply(Some(&base(false)), &on, true),
        Err(Rejected::NotConfigurable),
        "and not back"
    );
}

#[test]
fn changing_kind_needs_configurable() {
    let data = Property::Data {
        value: number(1.0),
        writable: true,
        enumerable: true,
        configurable: false,
    };
    let to_accessor = PartialDescriptor {
        get: Some(Value::NULL),
        ..PartialDescriptor::default()
    };
    assert_eq!(
        validate_and_apply(Some(&data), &to_accessor, true),
        Err(Rejected::NotConfigurable)
    );

    let accessor = Property::Accessor {
        get: Value::NULL,
        set: Value::UNDEFINED,
        enumerable: true,
        configurable: false,
    };
    assert_eq!(
        validate_and_apply(
            Some(&accessor),
            &PartialDescriptor::value(number(1.0)),
            true
        ),
        Err(Rejected::NotConfigurable),
        "and not the other way either"
    );
}

#[test]
fn a_descriptor_that_asks_for_nothing_is_always_allowed() {
    // Even on a frozen property, and even on a non-extensible object: asking for no change is
    // not a change.
    let frozen = Property::Data {
        value: number(1.0),
        writable: false,
        enumerable: false,
        configurable: false,
    };
    assert!(validate_and_apply(Some(&frozen), &PartialDescriptor::default(), false).is_ok());
}

#[test]
fn a_new_property_needs_an_extensible_object() {
    assert_eq!(
        validate_and_apply(None, &PartialDescriptor::value(number(1.0)), false),
        Err(Rejected::NotExtensible)
    );
    assert!(validate_and_apply(None, &PartialDescriptor::value(number(1.0)), true).is_ok());
}

#[test]
fn define_property_defaults_to_locked_down_and_assignment_does_not() {
    // `Object.defineProperty(o, "x", {})` and `o.x = undefined` produce different properties,
    // which is a real asymmetry and a common surprise.
    let defined = validate_and_apply(None, &PartialDescriptor::default(), true).expect("allowed");
    assert_eq!(
        defined,
        Property::Data {
            value: Value::UNDEFINED,
            writable: false,
            enumerable: false,
            configurable: false,
        },
        "defineProperty defaults every attribute to false"
    );
    assert_eq!(
        Property::data(Value::UNDEFINED),
        Property::Data {
            value: Value::UNDEFINED,
            writable: true,
            enumerable: true,
            configurable: true,
        },
        "assignment creates a fully-permissive property"
    );
}

// ---- the prototype chain ------------------------------------------------------------

#[test]
fn get_walks_the_prototype_chain() {
    let mut realm = Realm::new();
    let parent = realm.create(None);
    let child = realm.create(Some(parent));
    realm
        .define_own_property(parent, key("x"), &PartialDescriptor::value(number(1.0)))
        .expect("defined");

    assert_eq!(realm.get(child, &key("x")), Got::Value(number(1.0)));
    assert!(realm.has_property(child, &key("x")));
    assert!(
        realm.own_property(child, &key("x")).is_none(),
        "inherited, not owned"
    );
}

#[test]
fn a_missing_property_is_undefined_rather_than_an_error() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    assert_eq!(
        realm.get(object, &key("nothing")),
        Got::Value(Value::UNDEFINED)
    );
    assert!(!realm.has_property(object, &key("nothing")));
}

#[test]
fn a_frozen_prototype_blocks_assignment_on_the_child() {
    // The surprising one. `Object.freeze(proto)` stops `child.x = 1` from creating an *own*
    // property on the child, because `[[Set]]` consults the chain before deciding where to
    // write.
    let mut realm = Realm::new();
    let parent = realm.create(None);
    let child = realm.create(Some(parent));
    realm
        .define_own_property(
            parent,
            key("x"),
            &PartialDescriptor {
                value: Some(number(1.0)),
                writable: Some(false),
                ..PartialDescriptor::default()
            },
        )
        .expect("defined");

    assert_eq!(realm.set(child, &key("x"), number(2.0)), Setting::Refused);
    assert!(
        realm.own_property(child, &key("x")).is_none(),
        "and no own property was created"
    );
}

#[test]
fn a_writable_prototype_property_is_shadowed_rather_than_changed() {
    let mut realm = Realm::new();
    let parent = realm.create(None);
    let child = realm.create(Some(parent));
    realm
        .define_own_property(parent, key("x"), &permissive(number(1.0)))
        .expect("defined");

    assert_eq!(realm.set(child, &key("x"), number(2.0)), Setting::Done);
    assert_eq!(realm.get(child, &key("x")), Got::Value(number(2.0)));
    assert_eq!(
        realm.get(parent, &key("x")),
        Got::Value(number(1.0)),
        "the prototype is untouched"
    );
}

#[test]
fn an_accessor_is_handed_back_rather_than_called() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    let getter = Value::number(42.0);
    realm
        .define_own_property(
            object,
            key("x"),
            &PartialDescriptor {
                get: Some(getter),
                ..PartialDescriptor::default()
            },
        )
        .expect("defined");

    assert_eq!(realm.get(object, &key("x")), Got::Getter(getter));
    assert_eq!(
        realm.set(object, &key("x"), number(1.0)),
        Setting::Setter(Value::UNDEFINED),
        "no setter means undefined, not a refusal — calling it is what fails"
    );
}

#[test]
fn a_prototype_cycle_is_refused() {
    // Without this every lookup that misses would hang.
    let mut realm = Realm::new();
    let a = realm.create(None);
    let b = realm.create(Some(a));
    assert!(!realm.set_prototype_of(a, Some(b)), "would close a loop");
    assert!(!realm.set_prototype_of(a, Some(a)), "and so would this");
    assert_eq!(realm.prototype_of(a), None);
}

#[test]
fn a_non_extensible_object_keeps_its_prototype() {
    let mut realm = Realm::new();
    let parent = realm.create(None);
    let object = realm.create(None);
    realm.prevent_extensions(object);

    assert!(!realm.set_prototype_of(object, Some(parent)));
    assert!(
        realm.set_prototype_of(object, None),
        "setting it to what it already is is a no-op and allowed"
    );
}

// ---- delete -------------------------------------------------------------------------

#[test]
fn deleting_something_that_was_never_there_succeeds() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    assert!(
        realm.delete(object, &key("nothing")),
        "`delete obj.nothing` is true"
    );
}

#[test]
fn a_non_configurable_property_cannot_be_deleted() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    realm
        .define_own_property(object, key("x"), &PartialDescriptor::value(number(1.0)))
        .expect("defined");
    assert!(!realm.delete(object, &key("x")));
    assert!(realm.own_property(object, &key("x")).is_some());
}

// ---- key order, which is observable ---------------------------------------------------

#[test]
fn integer_like_keys_come_first_in_ascending_order() {
    // `Object.keys({b: 1, 2: 2, a: 3, 1: 4})` is `["1", "2", "b", "a"]`. Code that renders a
    // keyed list depends on this, and it is not insertion order.
    let mut realm = Realm::new();
    let object = realm.create(None);
    for name in ["b", "2", "a", "1"] {
        realm
            .define_own_property(object, key(name), &PartialDescriptor::value(number(0.0)))
            .expect("defined");
    }
    let keys: Vec<String> = realm
        .own_keys(object)
        .into_iter()
        .map(|key| key.to_string())
        .collect();
    assert_eq!(keys, ["1", "2", "b", "a"]);
}

#[test]
fn only_canonical_indices_count_as_integer_keys() {
    // `"01"`, `"1.0"` and `"-0"` are ordinary string keys. Treating them as numeric would
    // reorder `Object.keys` in a way no engine does.
    let mut realm = Realm::new();
    let object = realm.create(None);
    for name in ["01", "1.0", "-0", "2"] {
        realm
            .define_own_property(object, key(name), &PartialDescriptor::value(number(0.0)))
            .expect("defined");
    }
    let keys: Vec<String> = realm
        .own_keys(object)
        .into_iter()
        .map(|key| key.to_string())
        .collect();
    assert_eq!(keys, ["2", "01", "1.0", "-0"]);
}

#[test]
fn redefining_a_property_does_not_move_it() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    for name in ["a", "b"] {
        realm
            .define_own_property(object, key(name), &permissive(number(0.0)))
            .expect("defined");
    }
    realm
        .define_own_property(object, key("a"), &PartialDescriptor::value(number(9.0)))
        .expect("redefined");

    let keys: Vec<String> = realm
        .own_keys(object)
        .into_iter()
        .map(|key| key.to_string())
        .collect();
    assert_eq!(keys, ["a", "b"], "still first");
}

#[test]
fn object_keys_shows_only_enumerable_own_properties() {
    let mut realm = Realm::new();
    let parent = realm.create(None);
    let object = realm.create(Some(parent));
    realm
        .define_own_property(
            parent,
            key("inherited"),
            &PartialDescriptor::value(number(0.0)),
        )
        .expect("defined");
    realm
        .define_own_property(
            object,
            key("hidden"),
            &PartialDescriptor {
                value: Some(number(0.0)),
                enumerable: Some(false),
                ..PartialDescriptor::default()
            },
        )
        .expect("defined");
    realm
        .define_own_property(
            object,
            key("shown"),
            &PartialDescriptor {
                value: Some(number(0.0)),
                enumerable: Some(true),
                ..PartialDescriptor::default()
            },
        )
        .expect("defined");

    let keys: Vec<String> = realm
        .enumerable_keys(object)
        .into_iter()
        .map(|key| key.to_string())
        .collect();
    assert_eq!(keys, ["shown"]);
}

#[test]
fn a_non_extensible_object_refuses_new_properties_but_not_changes_to_old_ones() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    realm
        .define_own_property(
            object,
            key("x"),
            &PartialDescriptor {
                value: Some(number(1.0)),
                writable: Some(true),
                configurable: Some(true),
                ..PartialDescriptor::default()
            },
        )
        .expect("defined");
    realm.prevent_extensions(object);

    assert_eq!(realm.set(object, &key("y"), number(1.0)), Setting::Refused);
    assert_eq!(realm.set(object, &key("x"), number(2.0)), Setting::Done);
    assert_eq!(realm.get(object, &key("x")), Got::Value(number(2.0)));
}
