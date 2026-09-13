//! That a value survives being one `u64`.
//!
//! The interesting cases are all doubles that look like something else. A NaN is a tagged
//! value's bit pattern with the tag bits happening to be set, and an infinity is one
//! mantissa bit away from that — so the tests that matter are the ones that push a number
//! into the space the tags live in and check it comes back a number.

use crisol_value::{ADDRESS_BITS, Address, Kind, Value};

// ---- numbers ------------------------------------------------------------------------

#[test]
fn every_ordinary_double_round_trips() {
    for value in [
        0.0,
        1.0,
        -1.0,
        0.5,
        -0.5,
        std::f64::consts::PI,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::EPSILON,
        -f64::MIN_POSITIVE,
        1e308,
        -1e308,
        1e-308,
    ] {
        let boxed = Value::number(value);
        assert_eq!(boxed.kind(), Kind::Number, "{value} should be a number");
        assert_eq!(boxed.as_number(), Some(value), "{value} should come back");
    }
}

#[test]
fn the_edges_of_the_exponent_are_numbers_not_tags() {
    // Infinities have the exponent all ones, which is half of what makes a tag. They must
    // still be numbers: only the two mantissa bits beside it decide.
    for value in [f64::INFINITY, f64::NEG_INFINITY] {
        let boxed = Value::number(value);
        assert_eq!(boxed.kind(), Kind::Number, "{value} is a number");
        assert_eq!(boxed.as_number(), Some(value));
    }
}

#[test]
fn subnormals_round_trip() {
    let subnormal = f64::from_bits(1);
    assert!(subnormal.is_subnormal());
    assert_eq!(Value::number(subnormal).as_number(), Some(subnormal));
}

#[test]
fn negative_zero_keeps_its_sign() {
    let negative = Value::number(-0.0);
    let positive = Value::number(0.0);
    assert_eq!(negative.as_number(), Some(-0.0));
    assert!(
        negative.as_number().unwrap().is_sign_negative(),
        "the sign bit is the only thing that distinguishes them, and it has to survive"
    );
    assert!(positive.as_number().unwrap().is_sign_positive());
}

// ---- NaN, which is where this representation can go wrong ---------------------------

#[test]
fn a_nan_is_a_number() {
    let boxed = Value::number(f64::NAN);
    assert_eq!(boxed.kind(), Kind::Number);
    assert!(boxed.as_number().expect("a number").is_nan());
}

#[test]
fn a_nan_with_the_tag_bits_set_is_still_a_number() {
    // This is the case the whole canonicalisation exists for. Hand-built NaNs — from bit
    // manipulation, or from a producer that does not canonicalise — can have the tag bits
    // set. Without the rewrite in `Value::number` this comes back as an Object whose
    // address is the mantissa, and the first thing that dereferences it is a crash a long
    // way from here.
    for payload in [
        0x0003_DEAD_BEEF_CAFE_u64,
        0x0001_0000_0000_0001,
        0x0002_FFFF_FFFF_FFFF,
    ] {
        let hostile = f64::from_bits(0x7FFC_0000_0000_0000 | payload);
        assert!(hostile.is_nan(), "the fixture has to actually be a NaN");

        let boxed = Value::number(hostile);
        assert_eq!(
            boxed.kind(),
            Kind::Number,
            "a NaN with tag bits set must not be read as a pointer"
        );
        assert!(boxed.as_number().expect("a number").is_nan());
        assert_eq!(boxed.as_address(), None, "and it must carry no address");
    }
}

#[test]
fn all_nans_canonicalise_to_the_same_value() {
    // JavaScript cannot tell two NaNs apart, so this loses nothing — and it is what makes
    // the equality below behave like `Object.is`.
    let one = Value::number(f64::NAN);
    let another = Value::number(f64::from_bits(0x7FFC_0000_0000_0001));
    let negative = Value::number(-f64::NAN);
    assert_eq!(one, another);
    assert_eq!(one, negative);
}

// ---- the singletons ------------------------------------------------------------------

#[test]
fn the_singletons_are_distinct_and_know_themselves() {
    assert_eq!(Value::UNDEFINED.kind(), Kind::Undefined);
    assert_eq!(Value::NULL.kind(), Kind::Null);
    assert_eq!(Value::TRUE.kind(), Kind::Boolean);
    assert_eq!(Value::FALSE.kind(), Kind::Boolean);

    assert!(Value::UNDEFINED.is_undefined());
    assert!(Value::NULL.is_null());
    assert!(Value::UNDEFINED.is_nullish() && Value::NULL.is_nullish());
    assert!(!Value::FALSE.is_nullish(), "false is not nullish");

    assert_eq!(Value::TRUE.as_boolean(), Some(true));
    assert_eq!(Value::FALSE.as_boolean(), Some(false));
    assert_eq!(Value::boolean(true), Value::TRUE);
    assert_eq!(Value::boolean(false), Value::FALSE);

    let all = [Value::UNDEFINED, Value::NULL, Value::TRUE, Value::FALSE];
    for (at, one) in all.iter().enumerate() {
        for other in &all[at + 1..] {
            assert_ne!(one, other, "the singletons must not collide");
        }
    }
}

#[test]
fn a_singleton_is_not_a_number_and_holds_no_address() {
    for value in [Value::UNDEFINED, Value::NULL, Value::TRUE, Value::FALSE] {
        assert_eq!(value.as_number(), None);
        assert_eq!(value.as_address(), None);
    }
}

#[test]
fn undefined_is_the_default() {
    assert_eq!(Value::default(), Value::UNDEFINED);
}

// ---- addresses -----------------------------------------------------------------------

#[test]
fn an_address_round_trips_through_every_pointer_kind() {
    let address = Address::new(0x0000_7F1B_2C3D_4E50).expect("a plausible heap address");
    for (make, kind) in [
        (Value::object as fn(Address) -> Value, Kind::Object),
        (Value::string as fn(Address) -> Value, Kind::String),
        (Value::symbol as fn(Address) -> Value, Kind::Symbol),
    ] {
        let value = make(address);
        assert_eq!(value.kind(), kind);
        assert_eq!(value.as_address(), Some(address));
        assert_eq!(value.as_number(), None, "{kind:?} is not a number");
    }
}

#[test]
fn the_ends_of_the_address_range_round_trip() {
    for raw in [0, 1, Address::MAX - 1, Address::MAX] {
        let address = Address::new(raw).expect("within range");
        assert_eq!(Value::object(address).as_address(), Some(address));
    }
}

#[test]
fn an_address_too_wide_is_refused_rather_than_truncated() {
    assert_eq!(Address::MAX, (1_u64 << ADDRESS_BITS) - 1);
    assert!(Address::new(1 << ADDRESS_BITS).is_none());
    assert!(Address::new(u64::MAX).is_none());
    // The point: truncating would give a pointer that is wrong rather than obviously
    // invalid, and the crash would happen somewhere else entirely.
    assert!(Address::new(Address::MAX).is_some());
}

#[test]
fn the_same_address_with_different_kinds_is_different_values() {
    let address = Address::new(0x1234_5678).expect("in range");
    assert_ne!(Value::object(address), Value::string(address));
    assert_ne!(Value::string(address), Value::symbol(address));
}

// ---- the representation itself -------------------------------------------------------

#[test]
fn a_value_is_eight_bytes_and_aligned_like_one() {
    assert_eq!(
        size_of::<Value>(),
        8,
        "the calling convention depends on this"
    );
    assert_eq!(align_of::<Value>(), 8);
    assert_eq!(
        size_of::<Option<Value>>(),
        16,
        "no niche: every pattern is a value"
    );
}

#[test]
fn bits_round_trip() {
    for value in [
        Value::UNDEFINED,
        Value::NULL,
        Value::TRUE,
        Value::FALSE,
        Value::number(1.5),
        Value::number(f64::NAN),
        Value::object(Address::new(0xABCD).expect("in range")),
    ] {
        assert_eq!(Value::from_bits(value.to_bits()), value);
    }
}

#[test]
fn kind_is_total_over_every_bit_pattern() {
    // `from_bits` is reachable from generated code, so a pattern no safe constructor
    // produces still must not panic. Walks one bit at a time plus a spread of hostile
    // patterns rather than all 2^64.
    for bit in 0..64 {
        let value = Value::from_bits(1_u64 << bit);
        let _ = value.kind();
        let _ = value.as_number();
        let _ = value.as_address();
        let _ = value.as_boolean();
    }
    for bits in [
        0,
        u64::MAX,
        0x7FFC_0000_0000_0000,
        0xFFFF_FFFF_FFFF_FFFF,
        0x7FFC_0000_0000_00FF,
    ] {
        let value = Value::from_bits(bits);
        let _ = value.kind();
        let _ = value.as_address();
    }
}

/// Derived equality is `Object.is`, not `===`, and that is worth knowing before someone
/// reaches for `==` expecting the other one.
#[test]
fn equality_is_same_value_not_strict_equals() {
    // `Object.is(NaN, NaN)` is true. `NaN === NaN` is false.
    assert_eq!(
        Value::number(f64::NAN),
        Value::number(f64::NAN),
        "canonicalisation is what makes this hold"
    );
    // `Object.is(0, -0)` is false. `0 === -0` is true.
    assert_ne!(Value::number(0.0), Value::number(-0.0));
    // Everything else agrees.
    assert_eq!(Value::number(1.0), Value::number(1.0));
    assert_ne!(Value::number(1.0), Value::number(2.0));
    assert_ne!(Value::NULL, Value::UNDEFINED);
}
