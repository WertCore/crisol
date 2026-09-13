//! The coercions, and the places "obviously it does X" is wrong.
//!
//! Every case here is one that a reasonable implementation gets wrong by being reasonable, or
//! that a reasonable *reader* gets wrong by being reasonable. They are short algorithms with a
//! very high density of surprises.

use crisol_builtins::{
    number_to_string, string_to_boolean, string_to_number, to_boolean, to_number, to_string,
};
use crisol_value::Value;

fn n(value: f64) -> Value {
    Value::number(value)
}

// ---- ToBoolean ----------------------------------------------------------------------------

#[test]
fn the_falsy_list_is_closed() {
    for value in [
        Value::UNDEFINED,
        Value::NULL,
        Value::FALSE,
        n(0.0),
        n(-0.0),
        n(f64::NAN),
    ] {
        assert!(!to_boolean(value), "{value:?} should be falsy");
    }
    for value in [Value::TRUE, n(1.0), n(-1.0), n(f64::INFINITY)] {
        assert!(to_boolean(value), "{value:?} should be truthy");
    }
}

#[test]
fn a_strings_truthiness_is_its_length_not_its_content() {
    // `Boolean("0")` and `Boolean("false")` are both true. An implementation that
    // "helpfully" treated `"0"` as falsy would break every truthiness check on a string.
    assert!(!string_to_boolean(""));
    assert!(string_to_boolean("0"));
    assert!(string_to_boolean("false"));
    assert!(string_to_boolean(" "), "a space is not empty");
}

// ---- ToNumber on strings --------------------------------------------------------------------

#[test]
fn an_empty_or_blank_string_is_zero() {
    // This is why `+[]` is 0.
    assert_eq!(string_to_number(""), 0.0);
    assert_eq!(string_to_number("   "), 0.0);
    assert_eq!(string_to_number("\t\n\r "), 0.0);
}

#[test]
fn surrounding_whitespace_is_trimmed() {
    assert_eq!(string_to_number("  42  "), 42.0);
    assert_eq!(string_to_number("\n-1\t"), -1.0);
}

#[test]
fn radix_prefixes_are_understood() {
    assert_eq!(string_to_number("0x10"), 16.0);
    assert_eq!(string_to_number("0X1f"), 31.0);
    assert_eq!(string_to_number("0b101"), 5.0);
    assert_eq!(string_to_number("0o17"), 15.0);
    assert!(string_to_number("0x").is_nan(), "a prefix with no digits");
    assert!(string_to_number("0xZ").is_nan());
}

#[test]
fn trailing_junk_is_a_failure_not_a_truncation() {
    // The `Number` / `parseInt` split: `Number("10abc")` is NaN where `parseInt("10abc")` is
    // 10. Reaching for the lenient one because it "usually works" turns malformed input into a
    // plausible number.
    assert!(string_to_number("10abc").is_nan());
    assert!(string_to_number("1 2").is_nan());
    assert!(string_to_number("abc").is_nan());
}

#[test]
fn the_infinities_are_spelled_out_but_rust_spellings_are_not_accepted() {
    assert_eq!(string_to_number("Infinity"), f64::INFINITY);
    assert_eq!(string_to_number("+Infinity"), f64::INFINITY);
    assert_eq!(string_to_number("-Infinity"), f64::NEG_INFINITY);
    // Rust's parser accepts these; JavaScript's grammar does not.
    for text in ["inf", "INF", "infinity", "NaN", "nan"] {
        assert!(string_to_number(text).is_nan(), "{text:?} is not a number");
    }
}

#[test]
fn numeric_separators_are_not_part_of_tonumber() {
    // `1_000` is a valid *literal* in source and not a valid *string* to convert.
    assert!(string_to_number("1_000").is_nan());
}

#[test]
fn decimals_and_exponents_work() {
    assert_eq!(string_to_number("1.5"), 1.5);
    assert_eq!(
        string_to_number(".5"),
        0.5,
        "a leading point is allowed here"
    );
    assert_eq!(string_to_number("1e3"), 1000.0);
    assert_eq!(string_to_number("1E-3"), 0.001);
    assert_eq!(string_to_number("-0"), -0.0);
    assert!(
        string_to_number("-0").is_sign_negative(),
        "the sign survives the conversion even though ToString drops it"
    );
}

#[test]
fn lone_signs_and_points_are_not_numbers() {
    for text in ["+", "-", "."] {
        assert!(string_to_number(text).is_nan(), "{text:?}");
    }
}

// ---- ToString on numbers ----------------------------------------------------------------------

#[test]
fn negative_zero_prints_without_its_sign() {
    // The mirror image of the `Map` key rule: the sign is observable through `Object.is` and
    // not through text.
    assert_eq!(number_to_string(-0.0), "0");
    assert_eq!(number_to_string(0.0), "0");
}

#[test]
fn the_specials_have_names() {
    assert_eq!(number_to_string(f64::NAN), "NaN");
    assert_eq!(number_to_string(f64::INFINITY), "Infinity");
    assert_eq!(number_to_string(f64::NEG_INFINITY), "-Infinity");
}

#[test]
fn whole_numbers_print_without_a_decimal_point() {
    assert_eq!(number_to_string(1.0), "1");
    assert_eq!(number_to_string(-42.0), "-42");
    assert_eq!(number_to_string(1e20), "100000000000000000000");
}

#[test]
fn the_changeover_to_exponential_is_at_1e21() {
    // Specified, not a float-printing accident: 1e20 prints in full and 1e21 does not.
    assert_eq!(number_to_string(1e20), "100000000000000000000");
    assert_eq!(number_to_string(1e21), "1e+21");
    assert_eq!(number_to_string(-1e21), "-1e+21");
}

#[test]
fn small_magnitudes_go_exponential_below_1e_minus_6() {
    assert_eq!(number_to_string(0.000_001), "0.000001");
    assert_eq!(number_to_string(0.000_000_1), "1e-7");
}

#[test]
fn the_exponent_always_carries_its_sign() {
    // Rust writes `1e21`; JavaScript writes `1e+21`, and the plus is not optional.
    assert!(number_to_string(1e21).contains("e+"));
    assert!(number_to_string(1e-7).contains("e-"));
}

#[test]
fn fractions_print_shortest() {
    assert_eq!(number_to_string(1.5), "1.5");
    assert_eq!(number_to_string(0.1), "0.1");
}

// ---- the Value-level entry points --------------------------------------------------------------

#[test]
fn undefined_and_null_convert_differently() {
    // `Number(undefined)` is NaN and `Number(null)` is 0, which is why `null >= 0` is true
    // while `undefined >= 0` is false.
    assert!(to_number(Value::UNDEFINED).expect("known").is_nan());
    assert_eq!(to_number(Value::NULL), Some(0.0));

    assert_eq!(to_string(Value::UNDEFINED).as_deref(), Some("undefined"));
    assert_eq!(to_string(Value::NULL).as_deref(), Some("null"));
}

#[test]
fn booleans_convert_both_ways() {
    assert_eq!(to_number(Value::TRUE), Some(1.0));
    assert_eq!(to_number(Value::FALSE), Some(0.0));
    assert_eq!(to_string(Value::TRUE).as_deref(), Some("true"));
    assert_eq!(to_string(Value::FALSE).as_deref(), Some("false"));
}

#[test]
fn a_value_this_layer_cannot_see_into_reports_nothing() {
    // Strings and objects need the runtime — an object's ToString calls `toString` or
    // `valueOf`. Reporting `None` says so rather than inventing "[object Object]" in the crate
    // least able to know.
    let object = Value::object(crisol_value::Address::new(0x1000).expect("in range"));
    assert_eq!(to_string(object), None);
    assert_eq!(to_number(object), None);
}
