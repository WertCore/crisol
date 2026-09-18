//! The C ABI that compiled code calls into.
//!
//! §M13's backend emits calls to these symbols for every operator it cannot express as a
//! native instruction (D-86). Until they exist, an object file references undefined symbols and
//! nothing links — which is why this crate is the last thing between "compiles" and "runs".
//!
//! # The shape of the boundary
//!
//! Every function takes and returns `u64`: a JavaScript value NaN-boxed into 64 bits (D-53).
//! There is no wrapper type at the boundary because the callee is machine code with no notion
//! of Rust types, and a `#[repr(transparent)]` newtype would be a comment rather than a
//! guarantee. [`crisol_value::Value::from_bits`] is the only thing that gives the bits meaning.
//!
//! # `ToInt32` is the reason the bitwise operators are here
//!
//! The specification's `ToInt32` truncates toward zero and then wraps **modulo 2³²**.
//! Cranelift's float-to-int conversion **saturates**, so `1e10 | 0` lowered as a machine
//! instruction would clamp to `i32::MAX` where JavaScript gives `1410065408`. That is a wrong
//! number that looks entirely plausible, which is why these are calls and not instructions.

#![doc(html_root_url = "https://docs.rs/crisol-abi/0.0.0")]

use crisol_value::Value;

/// Every symbol this crate provides to generated code.
///
/// The backend declares imports by name and this crate defines them by name, and **nothing
/// connects the two until link time** — a typo on either side is silent through every compiler
/// test and fails when someone tries to produce a binary. So the list lives here, on the side
/// that defines them, and `crisol-codegen`'s tests assert that every symbol it emits appears
/// in it.
pub const SYMBOLS: &[&str] = &[
    "crisol_add",
    "crisol_subtract",
    "crisol_multiply",
    "crisol_divide",
    "crisol_remainder",
    "crisol_exponent",
    "crisol_bit_and",
    "crisol_bit_or",
    "crisol_bit_xor",
    "crisol_shift_left",
    "crisol_shift_right",
    "crisol_unsigned_shift_right",
];

/// `ToNumber` for a value that is already a number, and `NaN` otherwise.
///
/// The arithmetic helpers below are reached only for operands the compiler could **not** prove
/// numeric, so each one has to do the coercion itself. Anything that is not a number yields
/// `NaN` for now: strings need the runtime's string table, and objects need `ToPrimitive`,
/// which calls user code. Both are recorded rather than approximated — returning `0` for a
/// string would make `"5" * 2` evaluate to `0` instead of `10`.
fn to_number(bits: u64) -> f64 {
    let value = Value::from_bits(bits);
    value.as_number().unwrap_or(f64::NAN)
}

fn from_number(number: f64) -> u64 {
    Value::number(number).to_bits()
}

/// The specification's `ToInt32`.
///
/// Not a cast. `NaN`, the infinities and `±0` all give `0`; everything else truncates toward
/// zero and wraps modulo 2³², so `2³¹` comes back as `-2³¹` and `1e10` as `1410065408`.
///
/// A saturating conversion — which is what the hardware offers — gets every one of those
/// wrong in a way that still produces a number.
#[must_use]
pub fn to_int32(number: f64) -> i32 {
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let truncated = number.trunc();
    // `rem_euclid` on the 2³² modulus, done in `f64` because the value may be far outside
    // `i64`'s range before reduction — casting first is exactly the saturation this avoids.
    let wrapped = truncated.rem_euclid(4_294_967_296.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rem_euclid put this in 0..2^32, which is exactly u32's range"
    )]
    let unsigned = wrapped as u32;
    #[expect(
        clippy::cast_possible_wrap,
        reason = "the wrap to a signed 32-bit value is the specified behaviour"
    )]
    let signed = unsigned as i32;
    signed
}

/// The specification's `ToUint32`, which differs from [`to_int32`] only in the final step.
#[must_use]
pub fn to_uint32(number: f64) -> u32 {
    #[expect(
        clippy::cast_sign_loss,
        reason = "reinterpreting the same 32 bits is the specified behaviour"
    )]
    let unsigned = to_int32(number) as u32;
    unsigned
}

/// `left + right`.
///
/// # Safety
///
/// Called from generated machine code with two NaN-boxed values. There is nothing unsafe about
/// the body; the `extern "C"` surface is what the backend emits calls to.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_add(left: u64, right: u64) -> u64 {
    // `+` concatenates when either operand is a string after `ToPrimitive`. Strings need the
    // runtime's string table, which does not exist yet, so a string operand gives `NaN` rather
    // than a wrong concatenation or a silent numeric answer.
    from_number(to_number(left) + to_number(right))
}

/// `left - right`, for callers that could not prove both operands numeric.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_subtract(left: u64, right: u64) -> u64 {
    from_number(to_number(left) - to_number(right))
}

/// `left * right`.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_multiply(left: u64, right: u64) -> u64 {
    from_number(to_number(left) * to_number(right))
}

/// `left / right`.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_divide(left: u64, right: u64) -> u64 {
    from_number(to_number(left) / to_number(right))
}

/// `left % right`.
///
/// **The result takes the sign of the dividend**, not the divisor — `-5 % 3` is `-2`, where a
/// mathematical modulo would give `1`. Rust's `%` on `f64` is `fmod` and agrees, which is why
/// this is a one-liner and not a correction.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_remainder(left: u64, right: u64) -> u64 {
    from_number(to_number(left) % to_number(right))
}

/// `left ** right`.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_exponent(left: u64, right: u64) -> u64 {
    from_number(to_number(left).powf(to_number(right)))
}

/// `left & right`, on int32.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_bit_and(left: u64, right: u64) -> u64 {
    from_number(f64::from(
        to_int32(to_number(left)) & to_int32(to_number(right)),
    ))
}

/// `left | right`, on int32.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_bit_or(left: u64, right: u64) -> u64 {
    from_number(f64::from(
        to_int32(to_number(left)) | to_int32(to_number(right)),
    ))
}

/// `left ^ right`, on int32.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_bit_xor(left: u64, right: u64) -> u64 {
    from_number(f64::from(
        to_int32(to_number(left)) ^ to_int32(to_number(right)),
    ))
}

/// `left << right`.
///
/// **The shift count is masked to five bits**, so `1 << 32` is `1` and not `0`. Rust's `<<`
/// panics on an over-wide shift in debug builds, so the mask is doing real work rather than
/// matching what the hardware happens to do.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_shift_left(left: u64, right: u64) -> u64 {
    let count = to_uint32(to_number(right)) & 31;
    from_number(f64::from(to_int32(to_number(left)) << count))
}

/// `left >> right`, sign-propagating.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_shift_right(left: u64, right: u64) -> u64 {
    let count = to_uint32(to_number(right)) & 31;
    from_number(f64::from(to_int32(to_number(left)) >> count))
}

/// `left >>> right`, zero-filling.
///
/// The only shift whose result is read as **unsigned**, so `-1 >>> 0` is `4294967295` rather
/// than `-1`. That is also the reason it cannot be folded in with the other two.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_unsigned_shift_right(left: u64, right: u64) -> u64 {
    let count = to_uint32(to_number(right)) & 31;
    from_number(f64::from(to_uint32(to_number(left)) >> count))
}

/// Prints a value the way a program's result should appear.
///
/// Exists so the C entry point a compiled program links against does not have to understand
/// NaN boxing. Decoding 64 bits into a JavaScript value is this crate's job, and duplicating
/// the tag layout in generated C would be a second place for it to drift.
///
/// # Safety
///
/// Called from the generated entry point with a NaN-boxed value.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_print(bits: u64) {
    let value = Value::from_bits(bits);
    // Deliberately not `Display`: a compiled program's output is a product surface, and
    // `Debug` would print Rust's idea of the value rather than JavaScript's.
    match value.kind() {
        crisol_value::Kind::Undefined => println!("undefined"),
        crisol_value::Kind::Null => println!("null"),
        crisol_value::Kind::Boolean => println!("{}", value.as_boolean().unwrap_or(false)),
        crisol_value::Kind::Number => match value.as_number() {
            Some(number) if number.is_nan() => println!("NaN"),
            Some(number) if number.is_infinite() && number > 0.0 => println!("Infinity"),
            Some(number) if number.is_infinite() => println!("-Infinity"),
            // Whole numbers print without a decimal point, as JavaScript does — `1`, not `1.0`.
            Some(number) if number.fract() == 0.0 && number.abs() < 1e21 => {
                println!("{number:.0}");
            }
            Some(number) => println!("{number}"),
            None => println!("NaN"),
        },
        crisol_value::Kind::String | crisol_value::Kind::Symbol | crisol_value::Kind::Object => {
            // Reaching into the heap needs a runtime this crate does not have. Saying so beats
            // printing a pointer that looks like a number.
            println!("[unprintable: the heap is not wired up yet]");
        }
    }
}
