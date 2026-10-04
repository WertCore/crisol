//! `ToBoolean`, `ToNumber`, `ToString` — the coercions everything else is built on.
//!
//! These are short algorithms with a very high density of surprises, and almost every one of
//! them is a place where "obviously it does X" is wrong:
//!
//! - `Boolean("0")` is **true**. So is `Boolean("false")`, `Boolean([])` and `Boolean({})`.
//!   Only `undefined`, `null`, `false`, `±0`, `NaN` and `""` are falsy, and that list is
//!   closed — an implementation that "helpfully" added `"0"` would break every truthiness
//!   check on a string.
//! - `Number("")` is **0**, and so is `Number("   ")`. `Number("abc")` is `NaN`. The empty
//!   string being zero is why `+[]` is `0`.
//! - `Number("0x10")` is **16**, but `parseInt`-style trailing junk is not allowed:
//!   `Number("10abc")` is `NaN` where `parseInt("10abc")` is `10`. They are different
//!   operations and mixing them up gives a number where there should be a failure.
//! - `String(-0)` is **`"0"`**. The sign is observable through `Object.is` and not through
//!   text, which is the mirror image of the `Map` key rule (D-62).
//! - `String(1e21)` is `"1e+21"` and `String(1e20)` is `"100000000000000000000"`. The
//!   changeover is exactly at 1e21, and it is specified rather than a float-printing
//!   accident.

use crisol_value::{Kind, Value};

/// `ToBoolean`.
///
/// The falsy list is closed: `undefined`, `null`, `false`, `+0`, `-0`, `NaN`, `""`. Everything
/// else — including `"0"`, `"false"`, `[]` and `{}` — is true.
#[must_use]
pub fn to_boolean(value: Value) -> bool {
    match value.kind() {
        Kind::Undefined | Kind::Null => false,
        Kind::Boolean => value.as_boolean().unwrap_or(false),
        Kind::Number => value.as_number().is_some_and(|number| {
            // `NaN` fails both comparisons, and `-0 != 0.0` is false, so both zeros land here.
            number != 0.0 && !number.is_nan()
        }),
        // A string's truthiness is its *length*, not its content. `"false"` is true. A BigInt is
        // truthy unless it is `0n`, which the value layer cannot tell apart from here — the
        // heap-aware caller in the ABI refines it, exactly as it does for a string.
        Kind::String | Kind::Symbol | Kind::BigInt | Kind::Object => true,
    }
}

/// `ToBoolean` for a string, which the [`Value`] layer cannot see into yet.
#[must_use]
pub const fn string_to_boolean(text: &str) -> bool {
    !text.is_empty()
}

/// `ToNumber` applied to a string — the `StringNumericLiteral` grammar.
///
/// Not `parseInt`. Trailing junk is a failure here and a truncation there:
/// `Number("10abc")` is `NaN`, `parseInt("10abc")` is `10`. Reaching for the lenient one
/// because it "usually works" turns malformed input into a plausible number.
#[must_use]
pub fn string_to_number(text: &str) -> f64 {
    // The spec trims `StrWhiteSpace`, which includes Unicode space separators and the BOM —
    // not just ASCII whitespace. `char::is_whitespace` covers the space separators; the BOM is
    // listed explicitly because Unicode does not classify it as whitespace and the spec does.
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');

    if trimmed.is_empty() {
        // `Number("")` and `Number("   ")` are both 0. This is why `+[]` is 0.
        return 0.0;
    }

    if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return radix(rest, 16);
    }
    if let Some(rest) = trimmed
        .strip_prefix("0o")
        .or_else(|| trimmed.strip_prefix("0O"))
    {
        return radix(rest, 8);
    }
    if let Some(rest) = trimmed
        .strip_prefix("0b")
        .or_else(|| trimmed.strip_prefix("0B"))
    {
        return radix(rest, 2);
    }

    match trimmed {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }

    // Rust's parser is close to the grammar but not identical, so the cases where they differ
    // are excluded first rather than hoped over:
    //   - `inf`, `infinity`, `nan` in any case are Rust literals and not JavaScript ones.
    //   - `1_000` is a Rust literal; numeric separators are not part of `ToNumber`.
    //   - a trailing or leading `.` alone (`"."`) is neither.
    let lowered = trimmed.to_ascii_lowercase();
    if lowered.contains('_')
        || lowered.contains("inf")
        || lowered.contains("nan")
        || trimmed == "."
        || trimmed == "+"
        || trimmed == "-"
    {
        return f64::NAN;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

fn radix(digits: &str, base: u32) -> f64 {
    if digits.is_empty() {
        return f64::NAN;
    }
    let mut total = 0.0_f64;
    for character in digits.chars() {
        let Some(digit) = character.to_digit(base) else {
            return f64::NAN;
        };
        total = total.mul_add(f64::from(base), f64::from(digit));
    }
    total
}

/// `ToString` applied to a number — `Number::toString` with radix 10.
///
/// The rules that matter:
/// - `NaN` is `"NaN"`, the infinities are `"Infinity"` and `"-Infinity"`.
/// - **`-0` is `"0"`.** The sign survives `Object.is` and not text.
/// - Integers below `1e21` print in full; at `1e21` and above the form becomes exponential.
///   The changeover is specified, not a float-printing accident.
/// - Small magnitudes go exponential below `1e-6`: `0.000001` prints in full and `0.0000001`
///   prints as `1e-7`.
#[must_use]
pub fn number_to_string(number: f64) -> String {
    if number.is_nan() {
        return "NaN".to_owned();
    }
    if number == 0.0 {
        // Catches `-0.0` too, because `-0.0 == 0.0`.
        return "0".to_owned();
    }
    if number.is_infinite() {
        return if number > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .to_owned();
    }

    let magnitude = number.abs();
    if magnitude >= 1e21 {
        return exponential(number);
    }
    if magnitude < 1e-6 {
        return exponential(number);
    }
    if number.fract() == 0.0 {
        return format!("{number:.0}");
    }
    // Rust's shortest round-trip representation agrees with the spec's "shortest decimal that
    // round-trips" for this range, which is the only part of the algorithm both share by
    // construction rather than by coincidence.
    format!("{number}")
}

/// The `1e+21` form: a mantissa, `e`, an explicit sign, and no zero padding on the exponent.
fn exponential(number: f64) -> String {
    let formatted = format!("{number:e}");
    // Rust writes `1e21`; JavaScript writes `1e+21`. The sign is not optional there.
    match formatted.split_once('e') {
        Some((mantissa, exponent)) if !exponent.starts_with('-') => {
            format!("{mantissa}e+{exponent}")
        }
        _ => formatted,
    }
}

/// `ToString` for the values this layer can see whole.
///
/// Strings and objects are not here: a string's text lives behind a handle the runtime owns,
/// and an object's `ToString` calls `toString` or `valueOf`, which needs a caller. Returning
/// `None` says so rather than inventing `"[object Object]"` in the crate least able to know.
#[must_use]
pub fn to_string(value: Value) -> Option<String> {
    match value.kind() {
        Kind::Undefined => Some("undefined".to_owned()),
        Kind::Null => Some("null".to_owned()),
        Kind::Boolean => value
            .as_boolean()
            .map(|flag| if flag { "true" } else { "false" }.to_owned()),
        Kind::Number => value.as_number().map(number_to_string),
        // `String(Symbol())` is a TypeError — only `String()` itself is allowed to describe
        // one, and implicit coercion must fail. Reported as "not here" rather than as text. A
        // BigInt's digits live behind a handle too, so it defers the same way; the ABI formats
        // them without the trailing `n`.
        Kind::String | Kind::Symbol | Kind::BigInt | Kind::Object => None,
    }
}

/// `ToNumber` for the values this layer can see whole.
#[must_use]
pub fn to_number(value: Value) -> Option<f64> {
    match value.kind() {
        // `Number(undefined)` is NaN and `Number(null)` is 0. Not the same, and the difference
        // is why `null >= 0` is true while `undefined >= 0` is false.
        Kind::Undefined => Some(f64::NAN),
        Kind::Null => Some(0.0),
        Kind::Boolean => value.as_boolean().map(|flag| if flag { 1.0 } else { 0.0 }),
        Kind::Number => value.as_number(),
        // A BigInt is *not* here even though its value is numeric: `Number(1n)` works but the
        // implicit `ToNumber(1n)` is a TypeError, and only the ABI can tell which caller it is.
        Kind::String | Kind::Symbol | Kind::BigInt | Kind::Object => None,
    }
}
