//! Allocating objects and giving them properties, from the side compiled code calls.
//!
//! These go through the `extern "C"` surface rather than the `Heap` API underneath it, because
//! the surface is what generated machine code reaches and the layer below is already tested.
//! A helper that works when called as a Rust function and passes the wrong slot when called
//! with raw pointers is exactly the failure this file exists to catch.

use crisol_abi::{crisol_create_object, crisol_property_load, crisol_property_store};
use crisol_value::Value;

/// Calls the store helper the way compiled code does — a pointer and a length, not a `&str`.
fn store(object: u64, key: &str, value: u64) {
    // SAFETY: `key` is a live Rust string, so its pointer and length describe readable UTF-8.
    unsafe { crisol_property_store(object, key.as_ptr(), key.len() as u64, value) }
}

fn load(object: u64, key: &str) -> Value {
    // SAFETY: as above.
    Value::from_bits(unsafe { crisol_property_load(object, key.as_ptr(), key.len() as u64) })
}

#[test]
fn a_fresh_object_is_an_object_with_no_properties() {
    let object = crisol_create_object();
    assert_eq!(Value::from_bits(object).kind(), crisol_value::Kind::Object);
    assert_eq!(load(object, "anything"), Value::UNDEFINED);
}

#[test]
fn a_stored_property_reads_back() {
    let object = crisol_create_object();
    store(object, "a", Value::number(1.0).to_bits());
    assert_eq!(load(object, "a"), Value::number(1.0));
}

/// The case `Heap::alloc` alone could not express: an object allocated empty gaining several
/// properties, each one moving it to a wider shape.
#[test]
fn several_properties_each_get_their_own_slot() {
    let object = crisol_create_object();
    for (index, key) in ["a", "b", "c"].iter().enumerate() {
        store(object, key, Value::number(index as f64).to_bits());
    }
    for (index, key) in ["a", "b", "c"].iter().enumerate() {
        assert_eq!(
            load(object, key),
            Value::number(index as f64),
            "{key} must not share a slot with another property"
        );
    }
}

/// How wide `object`'s shape is — how many slots it actually occupies.
fn width(object: u64) -> u32 {
    let address = Value::from_bits(object)
        .as_address()
        .expect("an object carries an address");
    let handle = crisol_gc::GcRef::from_address(address);
    crisol_abi::with_runtime(|runtime| {
        let shape = runtime
            .heap
            .shape_of(handle)
            .expect("a live object has a shape");
        runtime.shapes.borrow().len(shape)
    })
}

/// Reassignment is not a transition.
///
/// Reading the value back is **not** a test of this. If every store added a shape, `x` would
/// simply move to a new slot each time and still read back correctly — the object would just
/// grow a slot per iteration until it exhausted memory. The width is the only thing that
/// changes, so the width is what this asserts.
#[test]
fn reassigning_a_property_does_not_give_it_a_second_slot() {
    let object = crisol_create_object();
    store(object, "x", Value::number(1.0).to_bits());
    store(object, "y", Value::number(9.0).to_bits());
    assert_eq!(width(object), 2);

    for round in 0..10 {
        store(object, "x", Value::number(f64::from(round)).to_bits());
    }
    assert_eq!(width(object), 2, "ten assignments to x must add no slots");
    assert_eq!(load(object, "x"), Value::number(9.0));
    assert_eq!(
        load(object, "y"),
        Value::number(9.0),
        "y must not have moved"
    );
}

#[test]
fn two_objects_do_not_share_state() {
    let first = crisol_create_object();
    let second = crisol_create_object();
    store(first, "a", Value::number(1.0).to_bits());
    assert_eq!(load(second, "a"), Value::UNDEFINED);
}

/// A store through a non-object is ignored rather than faulting. The specification says
/// `TypeError`, which needs an unwinding path M13 does not have; what must not happen is a
/// write through a bit pattern that is not an address.
#[test]
fn a_store_through_a_non_object_is_ignored() {
    store(Value::number(3.0).to_bits(), "a", Value::TRUE.to_bits());
    store(Value::UNDEFINED.to_bits(), "a", Value::TRUE.to_bits());
    assert_eq!(load(Value::number(3.0).to_bits(), "a"), Value::UNDEFINED);
}

/// A malformed call must yield a missing property, not a read past the end of the pointer.
#[test]
fn a_null_or_invalid_key_is_not_dereferenced() {
    let object = crisol_create_object();
    // SAFETY: a null pointer is the case being tested; the helper checks before reading.
    let loaded = unsafe { crisol_property_load(object, std::ptr::null(), 4) };
    assert_eq!(Value::from_bits(loaded), Value::UNDEFINED);

    let invalid = [0xFF_u8, 0xFE];
    // SAFETY: two readable bytes that are not UTF-8 — the length is honest.
    let loaded = unsafe { crisol_property_load(object, invalid.as_ptr(), 2) };
    assert_eq!(Value::from_bits(loaded), Value::UNDEFINED);
}
