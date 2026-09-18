//! The ABI helpers compiled code calls.
//!
//! Most of these are about `ToInt32`, because its modular wrap is the reason the bitwise
//! operators are calls rather than machine instructions. A saturating conversion — which is
//! what the hardware offers — gets every case here wrong while still producing a number.

use crisol_abi::{
    crisol_add, crisol_bit_and, crisol_bit_or, crisol_divide, crisol_exponent, crisol_multiply,
    crisol_remainder, crisol_shift_left, crisol_shift_right, crisol_subtract,
    crisol_unsigned_shift_right, to_int32, to_uint32,
};
use crisol_value::Value;

fn n(value: f64) -> u64 {
    Value::number(value).to_bits()
}

fn number(bits: u64) -> f64 {
    Value::from_bits(bits).as_number().expect("a number")
}

// ---- ToInt32, and what saturation would do instead ------------------------------------------

#[test]
fn to_int32_wraps_rather_than_saturating() {
    // The case that decided these are calls. `1e10 | 0` is 1410065408 in JavaScript; the
    // hardware's saturating conversion gives `i32::MAX`. Both are numbers, and only one is
    // right.
    assert_eq!(to_int32(1e10), 1_410_065_408);

    #[expect(
        clippy::cast_possible_truncation,
        reason = "demonstrating the wrong answer on purpose"
    )]
    let saturating = 1e10_f64 as i32;
    assert_eq!(
        saturating,
        i32::MAX,
        "which is what the instruction would give"
    );
    assert_ne!(saturating, to_int32(1e10), "and it is not the answer");
}

#[test]
fn two_to_the_thirty_first_becomes_negative() {
    assert_eq!(to_int32(2_147_483_648.0), i32::MIN);
    assert_eq!(to_int32(4_294_967_295.0), -1, "2^32 - 1 is -1");
    assert_eq!(to_int32(4_294_967_296.0), 0, "2^32 wraps to zero");
}

#[test]
fn the_non_finite_values_and_zero_are_all_zero() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -0.0] {
        assert_eq!(to_int32(value), 0, "{value}");
    }
}

#[test]
fn truncation_is_toward_zero_not_floor() {
    assert_eq!(to_int32(1.9), 1);
    assert_eq!(to_int32(-1.9), -1, "toward zero, so -1 rather than -2");
}

#[test]
fn to_uint32_reinterprets_the_same_bits() {
    assert_eq!(to_uint32(-1.0), u32::MAX);
    assert_eq!(to_uint32(1e10), 1_410_065_408);
}

// ---- the operators -----------------------------------------------------------------------------

#[test]
fn arithmetic_works() {
    assert!((number(crisol_add(n(1.0), n(2.0))) - 3.0).abs() < f64::EPSILON);
    assert!((number(crisol_subtract(n(5.0), n(3.0))) - 2.0).abs() < f64::EPSILON);
    assert!((number(crisol_multiply(n(4.0), n(3.0))) - 12.0).abs() < f64::EPSILON);
    assert!((number(crisol_divide(n(6.0), n(2.0))) - 3.0).abs() < f64::EPSILON);
}

#[test]
fn remainder_takes_the_sign_of_the_dividend() {
    // `-5 % 3` is -2, not 1. A mathematical modulo gives the other answer, and the difference
    // is invisible until an operand goes negative.
    assert!((number(crisol_remainder(n(-5.0), n(3.0))) + 2.0).abs() < f64::EPSILON);
    assert!((number(crisol_remainder(n(5.0), n(-3.0))) - 2.0).abs() < f64::EPSILON);
}

#[test]
fn division_by_zero_is_infinity_not_an_error() {
    assert_eq!(number(crisol_divide(n(1.0), n(0.0))), f64::INFINITY);
    assert!(number(crisol_divide(n(0.0), n(0.0))).is_nan());
}

#[test]
fn exponent_follows_the_specification() {
    assert!((number(crisol_exponent(n(2.0), n(8.0))) - 256.0).abs() < f64::EPSILON);
    assert!((number(crisol_exponent(n(2.0), n(0.0))) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn the_bitwise_operators_go_through_int32() {
    assert!((number(crisol_bit_and(n(6.0), n(3.0))) - 2.0).abs() < f64::EPSILON);
    assert!((number(crisol_bit_or(n(6.0), n(3.0))) - 7.0).abs() < f64::EPSILON);

    // The wrap showing through an operator rather than in isolation.
    assert!(
        (number(crisol_bit_or(n(1e10), n(0.0))) - 1_410_065_408.0).abs() < f64::EPSILON,
        "1e10 | 0"
    );
}

#[test]
fn the_shift_count_is_masked_to_five_bits() {
    // `1 << 32` is 1, not 0 — the count wraps. Rust's `<<` panics on an over-wide shift in
    // debug builds, so the mask is doing real work rather than matching the hardware.
    assert!((number(crisol_shift_left(n(1.0), n(32.0))) - 1.0).abs() < f64::EPSILON);
    assert!((number(crisol_shift_left(n(1.0), n(33.0))) - 2.0).abs() < f64::EPSILON);
    assert!((number(crisol_shift_left(n(1.0), n(4.0))) - 16.0).abs() < f64::EPSILON);
}

#[test]
fn the_unsigned_shift_is_the_only_one_that_reads_as_unsigned() {
    // `-1 >>> 0` is 4294967295; `-1 >> 0` is -1. That difference is why it cannot be folded in
    // with the other two shifts.
    assert!(
        (number(crisol_unsigned_shift_right(n(-1.0), n(0.0))) - 4_294_967_295.0).abs()
            < f64::EPSILON
    );
    assert!((number(crisol_shift_right(n(-1.0), n(0.0))) + 1.0).abs() < f64::EPSILON);
}

#[test]
fn a_shift_left_that_overflows_int32_wraps() {
    // `1 << 31` is negative, because the result is read as a signed 32-bit value.
    assert!(number(crisol_shift_left(n(1.0), n(31.0))) < 0.0);
}

// ---- what is not implemented yet ---------------------------------------------------------------

#[test]
fn a_non_numeric_operand_gives_nan_rather_than_a_plausible_number() {
    // `+` concatenates when either operand is a string, and strings need the runtime's string
    // table. Until then a string operand gives NaN — **not** zero, which would make `"5" * 2`
    // evaluate to 0 instead of 10 and look like arithmetic rather than a gap.
    let undefined = Value::UNDEFINED.to_bits();
    assert!(number(crisol_add(n(1.0), undefined)).is_nan());
    assert!(number(crisol_multiply(n(5.0), undefined)).is_nan());
}
