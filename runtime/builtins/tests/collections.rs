//! `Map` and `Set`, and the equality they use.
//!
//! The point of most of these is that `Map` keys use **SameValueZero**, which agrees with
//! neither `===` nor `Object.is`:
//!
//! | | `NaN` vs `NaN` | `0` vs `-0` |
//! |---|---|---|
//! | `===` | different | same |
//! | `Object.is` | same | different |
//! | `Map`/`Set` | same | same |
//!
//! `Value`'s derived equality is `Object.is` (D-53) — right for descriptors, wrong for these.

use crisol_builtins::{JsMap, JsSet};
use crisol_value::Value;

fn n(value: f64) -> Value {
    Value::number(value)
}

// ---- SameValueZero -----------------------------------------------------------------------

#[test]
fn zero_and_negative_zero_are_the_same_key() {
    // The bug this guards: using `Value`'s own equality gives a map with two entries that
    // print identically and neither of which `get(0)` reliably finds. It survives every casual
    // test because nobody writes `-0` on purpose — it arrives from arithmetic.
    let mut map = JsMap::new();
    map.set(n(0.0), n(1.0));
    assert_eq!(map.get(n(-0.0)), Some(n(1.0)), "-0 finds the 0 entry");
    map.set(n(-0.0), n(2.0));
    assert_eq!(map.size(), 1, "and does not add a second one");
    assert_eq!(map.get(n(0.0)), Some(n(2.0)));
}

#[test]
fn the_value_type_itself_still_distinguishes_them() {
    // The contrast that makes the wrapper necessary rather than decorative.
    assert_ne!(
        n(0.0),
        n(-0.0),
        "Value equality is Object.is, which separates them"
    );
}

#[test]
fn nan_is_a_usable_key() {
    // `===` says NaN !== NaN, so a naive map cannot find it. SameValueZero says it can, and
    // M9's canonicalisation is what makes one NaN hash like another.
    let mut map = JsMap::new();
    map.set(n(f64::NAN), n(1.0));
    assert_eq!(map.get(n(f64::NAN)), Some(n(1.0)));
    assert!(map.has(n(f64::NAN)));
    map.set(n(f64::NAN), n(2.0));
    assert_eq!(map.size(), 1);
}

#[test]
fn resetting_a_key_with_negative_zero_keeps_the_original_key() {
    // `map.keys()` reports the key as first inserted, not as last used.
    let mut map = JsMap::new();
    map.set(n(0.0), n(1.0));
    map.set(n(-0.0), n(2.0));
    let keys: Vec<Value> = map.keys().collect();
    assert_eq!(keys, [n(0.0)]);
    assert!(
        keys[0].as_number().expect("a number").is_sign_positive(),
        "the stored key is still +0"
    );
}

#[test]
fn different_kinds_are_different_keys() {
    let mut map = JsMap::new();
    map.set(Value::UNDEFINED, n(1.0));
    map.set(Value::NULL, n(2.0));
    map.set(Value::TRUE, n(3.0));
    map.set(Value::FALSE, n(4.0));
    assert_eq!(map.size(), 4);
    assert_eq!(map.get(Value::NULL), Some(n(2.0)));
    assert_eq!(map.get(Value::UNDEFINED), Some(n(1.0)));
}

// ---- order -------------------------------------------------------------------------------

#[test]
fn iteration_is_in_insertion_order() {
    let mut map = JsMap::new();
    for value in [3.0, 1.0, 2.0] {
        map.set(n(value), n(value));
    }
    let keys: Vec<f64> = map.keys().filter_map(Value::as_number).collect();
    assert_eq!(keys, [3.0, 1.0, 2.0], "not sorted, and not a hash order");
}

#[test]
fn resetting_a_key_keeps_its_position() {
    // A `Map` used as an LRU by re-setting would not work, and that is specified.
    let mut map = JsMap::new();
    for value in [1.0, 2.0, 3.0] {
        map.set(n(value), n(value));
    }
    map.set(n(1.0), n(99.0));
    let keys: Vec<f64> = map.keys().filter_map(Value::as_number).collect();
    assert_eq!(keys, [1.0, 2.0, 3.0], "still first");
    assert_eq!(map.get(n(1.0)), Some(n(99.0)));
}

#[test]
fn deleting_and_re_adding_moves_a_key_to_the_end() {
    let mut map = JsMap::new();
    for value in [1.0, 2.0, 3.0] {
        map.set(n(value), n(value));
    }
    assert!(map.delete(n(1.0)));
    map.set(n(1.0), n(1.0));
    let keys: Vec<f64> = map.keys().filter_map(Value::as_number).collect();
    assert_eq!(keys, [2.0, 3.0, 1.0]);
}

// ---- mutation during iteration -----------------------------------------------------------

#[test]
fn an_entry_deleted_before_it_is_reached_is_not_visited() {
    // Specified: the iterator holds a position, not a snapshot.
    let mut map = JsMap::new();
    for value in [1.0, 2.0, 3.0] {
        map.set(n(value), n(value));
    }

    let mut seen = Vec::new();
    let mut position = 0;
    while position < map.positions() {
        if let Some((key, _)) = map.at(position) {
            let number = key.as_number().expect("a number");
            seen.push(number);
            if (number - 1.0).abs() < f64::EPSILON {
                map.delete(n(2.0));
            }
        }
        position += 1;
    }
    assert_eq!(seen, [1.0, 3.0], "2 was removed before the walk reached it");
}

#[test]
fn an_entry_added_during_iteration_is_visited() {
    // The other half, and the reason an iterator over a `Map` can loop forever if the body
    // keeps adding. Also specified.
    let mut map = JsMap::new();
    map.set(n(1.0), n(1.0));

    let mut seen = Vec::new();
    let mut position = 0;
    while position < map.positions() {
        if let Some((key, _)) = map.at(position) {
            let number = key.as_number().expect("a number");
            seen.push(number);
            if (number - 1.0).abs() < f64::EPSILON {
                map.set(n(2.0), n(2.0));
            }
        }
        position += 1;
    }
    assert_eq!(seen, [1.0, 2.0], "the appended entry is reached");
}

// ---- the rest ----------------------------------------------------------------------------

#[test]
fn delete_reports_whether_anything_went() {
    let mut map = JsMap::new();
    map.set(n(1.0), n(1.0));
    assert!(map.delete(n(1.0)));
    assert!(!map.delete(n(1.0)), "already gone");
    assert!(!map.has(n(1.0)));
    assert_eq!(map.size(), 0);
}

#[test]
fn clear_empties_it() {
    let mut map = JsMap::new();
    for value in [1.0, 2.0] {
        map.set(n(value), n(value));
    }
    map.clear();
    assert!(map.is_empty());
    assert_eq!(map.keys().count(), 0);
    map.set(n(3.0), n(3.0));
    assert_eq!(map.size(), 1, "and is usable afterwards");
}

#[test]
fn a_missing_key_is_none_rather_than_undefined() {
    // `map.get` returns `undefined` in JavaScript, but at this layer the difference between
    // "absent" and "present and undefined" still matters — `has` is what distinguishes them.
    let mut map = JsMap::new();
    map.set(n(1.0), Value::UNDEFINED);
    assert_eq!(
        map.get(n(1.0)),
        Some(Value::UNDEFINED),
        "present, holding undefined"
    );
    assert!(map.has(n(1.0)));
    assert_eq!(map.get(n(2.0)), None, "absent");
    assert!(!map.has(n(2.0)));
}

// ---- Set ---------------------------------------------------------------------------------

#[test]
fn a_set_is_a_map_whose_values_are_its_keys() {
    let mut set = JsSet::new();
    set.add(n(1.0));
    set.add(n(2.0));
    set.add(n(1.0));
    assert_eq!(set.size(), 2, "adding again is not a second entry");
    let values: Vec<f64> = set.values().filter_map(Value::as_number).collect();
    assert_eq!(values, [1.0, 2.0]);
}

#[test]
fn a_set_uses_the_same_equality() {
    let mut set = JsSet::new();
    set.add(n(0.0));
    assert!(set.has(n(-0.0)));
    set.add(n(f64::NAN));
    set.add(n(f64::NAN));
    assert_eq!(set.size(), 2, "0/-0 are one, and both NaNs are one");
}

#[test]
fn a_set_deletes_and_clears() {
    let mut set = JsSet::new();
    set.add(n(1.0));
    assert!(set.delete(n(1.0)));
    assert!(!set.delete(n(1.0)));
    set.add(n(2.0));
    set.clear();
    assert!(set.is_empty());
    assert_eq!(set.at(0), None);
}
