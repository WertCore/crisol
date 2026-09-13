//! `Array`: holes, and what `length` does to them.
//!
//! Two things here are easy to get wrong by being reasonable. A **hole is not `undefined`** —
//! both read as `undefined` and only one answers `in`. And **truncation can fail halfway** —
//! `a.length = 0` on an array with one frozen element shrinks it as far as that element and
//! reports failure, rather than doing nothing or doing everything.

use crisol_builtins::{JsArray, MAX_LENGTH, Property, array_index};
use crisol_value::Value;

fn n(value: f64) -> Value {
    Value::number(value)
}

fn frozen(value: Value) -> Property {
    Property::Data {
        value,
        writable: false,
        enumerable: true,
        configurable: false,
    }
}

// ---- holes --------------------------------------------------------------------------------

#[test]
fn a_hole_is_not_a_property_holding_undefined() {
    // `[, 1]` versus `[undefined, 1]`. Both read `undefined` at index 0; only one answers
    // `0 in a` with true, and every iterating method has to pick which it means.
    let mut with_hole = JsArray::with_length(2);
    with_hole.set(1, n(1.0));

    let with_undefined = JsArray::from_values(&[Value::UNDEFINED, n(1.0)]);

    assert_eq!(with_hole.get(0), None, "a hole has no element");
    assert!(!with_hole.has(0), "and `0 in a` is false");

    assert_eq!(
        with_undefined.get(0),
        Some(Value::UNDEFINED),
        "a property holding undefined has an element"
    );
    assert!(with_undefined.has(0), "and `0 in b` is true");

    assert_eq!(with_hole.length(), with_undefined.length(), "same length");
}

#[test]
fn deleting_leaves_a_hole_and_does_not_shorten_the_array() {
    // The difference between `delete` and `pop`, and the reason `delete` on an array is
    // almost always a mistake.
    let mut array = JsArray::from_values(&[n(1.0), n(2.0), n(3.0)]);
    assert!(array.delete(2));
    assert_eq!(array.length(), 3, "still three long");
    assert!(!array.has(2), "with a hole at the end");
    assert_eq!(array.get(2), None);
}

#[test]
fn indices_skips_holes_and_slots_does_not() {
    let mut array = JsArray::with_length(4);
    array.set(0, n(10.0));
    array.set(3, n(40.0));

    let present: Vec<u32> = array.indices().collect();
    assert_eq!(present, [0, 3], "Object.keys sees only the elements");

    let all: Vec<Option<Value>> = array.slots().collect();
    assert_eq!(
        all,
        [Some(n(10.0)), None, None, Some(n(40.0))],
        "and JSON.stringify sees the holes, which it writes as null"
    );
}

// ---- length -------------------------------------------------------------------------------

#[test]
fn writing_past_the_end_grows_the_length() {
    let mut array = JsArray::new();
    assert_eq!(array.length(), 0);
    assert!(array.set(4, n(1.0)));
    assert_eq!(array.length(), 5, "index 4 means length 5");
    assert!(!array.has(0), "and everything below it is a hole");
    assert!(array.has(4));
}

#[test]
fn growing_the_length_adds_holes_not_undefined() {
    let mut array = JsArray::from_values(&[n(1.0)]);
    assert!(array.set_length(3));
    assert_eq!(array.length(), 3);
    assert!(!array.has(1), "grown space is holes");
    assert!(!array.has(2));
}

#[test]
fn shrinking_the_length_deletes_the_elements_above_it() {
    let mut array = JsArray::from_values(&[n(1.0), n(2.0), n(3.0)]);
    assert!(array.set_length(1));
    assert_eq!(array.length(), 1);
    assert_eq!(array.get(0), Some(n(1.0)));
    assert!(!array.has(1));
}

#[test]
fn truncation_stops_at_an_element_it_cannot_delete() {
    // ArraySetLength deletes from the top down and stops at the first refusal, leaving length
    // one above it. Partial success — an implementation that treated this as atomic would be
    // wrong in both directions.
    let mut array = JsArray::from_values(&[n(0.0), n(1.0), n(2.0), n(3.0)]);
    array.define(1, frozen(n(1.0)));

    assert!(!array.set_length(0), "it could not do all of it");
    assert_eq!(array.length(), 2, "stopped one above the frozen element");
    assert_eq!(array.get(1), Some(n(1.0)), "which is still there");
    assert!(!array.has(2), "and everything above it went");
    assert!(!array.has(3));
}

#[test]
fn truncation_below_a_frozen_element_still_succeeds() {
    // Stopping is only for elements that are actually in the way.
    let mut array = JsArray::from_values(&[n(0.0), n(1.0), n(2.0)]);
    array.define(0, frozen(n(0.0)));
    assert!(array.set_length(1), "nothing above index 0 refused");
    assert_eq!(array.length(), 1);
}

#[test]
fn a_frozen_length_refuses_both_growth_and_truncation() {
    let mut array = JsArray::from_values(&[n(1.0)]);
    array.freeze_length();
    assert!(!array.length_writable());
    assert!(!array.set_length(5));
    assert!(!array.set_length(0));
    assert_eq!(array.length(), 1);
    assert!(
        !array.set(1, n(2.0)),
        "and writing past the end, because that would write length"
    );
    assert!(
        array.set(0, n(9.0)),
        "but writing an existing element is still fine"
    );
}

// ---- push and pop ---------------------------------------------------------------------------

#[test]
fn push_and_pop_move_the_end() {
    let mut array = JsArray::new();
    assert_eq!(array.push(n(1.0)), Some(1));
    assert_eq!(array.push(n(2.0)), Some(2));
    assert_eq!(array.pop(), Some(n(2.0)));
    assert_eq!(array.length(), 1);
    assert_eq!(array.pop(), Some(n(1.0)));
    assert_eq!(array.pop(), None, "empty");
    assert!(array.is_empty());
}

#[test]
fn popping_a_hole_still_shortens_the_array() {
    // `pop` reports `undefined` for both an empty array and a trailing hole, but only one of
    // those changes the length.
    let mut array = JsArray::with_length(2);
    array.set(0, n(1.0));
    assert_eq!(array.pop(), None, "the hole reads as nothing");
    assert_eq!(array.length(), 1, "and the array still got shorter");
    assert_eq!(array.pop(), Some(n(1.0)));
}

#[test]
fn a_frozen_array_refuses_a_push() {
    let mut array = JsArray::from_values(&[n(1.0)]);
    array.freeze_length();
    assert_eq!(array.push(n(2.0)), None);
    assert_eq!(array.length(), 1);
}

// ---- writability ----------------------------------------------------------------------------

#[test]
fn a_non_writable_element_refuses_assignment() {
    let mut array = JsArray::from_values(&[n(1.0)]);
    array.define(0, frozen(n(1.0)));
    assert!(!array.set(0, n(2.0)));
    assert_eq!(array.get(0), Some(n(1.0)));
}

#[test]
fn a_non_configurable_element_refuses_deletion() {
    let mut array = JsArray::from_values(&[n(1.0)]);
    array.define(0, frozen(n(1.0)));
    assert!(!array.delete(0));
    assert!(array.has(0));
}

// ---- what counts as an index -----------------------------------------------------------------

#[test]
fn only_canonical_decimals_below_the_maximum_are_indices() {
    assert_eq!(array_index("0"), Some(0));
    assert_eq!(array_index("42"), Some(42));
    assert_eq!(
        array_index("4294967294"),
        Some(MAX_LENGTH - 1),
        "the largest"
    );

    for key in ["01", "1.0", "-1", "+1", "", "a", " 1", "1 ", "1e2"] {
        assert_eq!(array_index(key), None, "{key:?} is an ordinary string key");
    }
    assert_eq!(
        array_index("4294967295"),
        None,
        "2^32-1 is a valid length but not a valid index"
    );
}

#[test]
fn an_index_at_the_maximum_cannot_be_written() {
    // Writing it would make `length` 2^32, which it cannot be.
    let mut array = JsArray::new();
    assert!(!array.set(MAX_LENGTH, n(1.0)));
    assert_eq!(array.length(), 0);
}
