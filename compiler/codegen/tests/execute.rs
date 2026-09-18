//! Running the code the backend generates.
//!
//! Every other test in this crate checks that a function **compiles**. None of them checks that
//! it computes the right answer — `6 / 3` compiling says nothing about whether it yields `2`.
//! These JIT-compile a function and call it.
//!
//! The helper symbols are registered explicitly from `crisol-abi` rather than resolved from the
//! host process, so the contract is visible here rather than implicit in a link order.

use crisol_codegen::{Jit, Report};
use crisol_ir::{
    BinaryOp, Block, BlockId, CompareOp, Constant, Function, Instruction, Op, Terminator, Type,
    ValueId,
};
use crisol_value::Value;

/// The runtime the generated code calls into.
fn helpers() -> Vec<(&'static str, *const u8)> {
    vec![
        ("crisol_add", crisol_abi::crisol_add as *const u8),
        (
            "crisol_remainder",
            crisol_abi::crisol_remainder as *const u8,
        ),
        ("crisol_exponent", crisol_abi::crisol_exponent as *const u8),
        ("crisol_bit_and", crisol_abi::crisol_bit_and as *const u8),
        ("crisol_bit_or", crisol_abi::crisol_bit_or as *const u8),
        ("crisol_bit_xor", crisol_abi::crisol_bit_xor as *const u8),
        (
            "crisol_shift_left",
            crisol_abi::crisol_shift_left as *const u8,
        ),
        (
            "crisol_shift_right",
            crisol_abi::crisol_shift_right as *const u8,
        ),
        (
            "crisol_unsigned_shift_right",
            crisol_abi::crisol_unsigned_shift_right as *const u8,
        ),
    ]
}

fn number(result: ValueId, value: f64) -> Instruction {
    Instruction {
        result: Some(result),
        ty: Type::Number,
        op: Op::Const(Constant::Number(value)),
        safepoint: None,
    }
}

/// Compiles `function` and calls it with no arguments, returning the boxed result.
fn run(function: &Function) -> Value {
    let mut jit = Jit::new(&helpers()).expect("a JIT for the host");
    let _: Report = jit.compile(function).expect("compiles");
    let address = jit.address(&function.name).expect("compiled");
    // SAFETY: the function was compiled from a verified IR function whose signature is
    // `() -> i64` when it has no parameters or captures, which every caller below satisfies.
    // The JIT owns the code for as long as it lives, and it outlives this call.
    let callable: extern "C" fn() -> u64 = unsafe { std::mem::transmute(address) };
    Value::from_bits(callable())
}

/// `function f() { return <left> <op> <right>; }`
fn binary(op: BinaryOp, left_value: f64, right_value: f64) -> Function {
    let mut function = Function::new("binary");
    let left = function.value();
    let right = function.value();
    let result = function.value();
    let numeric = matches!(
        op,
        BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
    );
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        number(left, left_value),
        number(right, right_value),
        Instruction {
            result: Some(result),
            ty: if numeric { Type::Number } else { Type::Unknown },
            op: Op::Binary { op, left, right },
            safepoint: (!numeric).then(crisol_ir::Safepoint::default),
        },
    ];
    entry.terminator = Terminator::Return(Some(result));
    function
}

fn as_number(value: Value) -> f64 {
    value.as_number().expect("a number")
}

// ---- the answers ---------------------------------------------------------------------------

#[test]
fn a_constant_comes_back_unchanged() {
    let mut function = Function::new("constant");
    let id = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![number(id, 42.5)];
    entry.terminator = Terminator::Return(Some(id));
    assert!((as_number(run(&function)) - 42.5).abs() < f64::EPSILON);
}

#[test]
fn the_native_instructions_compute_the_right_answer() {
    assert!((as_number(run(&binary(BinaryOp::Subtract, 6.0, 4.0))) - 2.0).abs() < f64::EPSILON);
    assert!((as_number(run(&binary(BinaryOp::Multiply, 6.0, 4.0))) - 24.0).abs() < f64::EPSILON);
    assert!((as_number(run(&binary(BinaryOp::Divide, 6.0, 4.0))) - 1.5).abs() < f64::EPSILON);
}

#[test]
fn the_helper_calls_reach_the_runtime() {
    // The symbol contract, exercised rather than compared: if the backend emitted a name the
    // runtime does not define, this would fail to resolve rather than produce a wrong number.
    assert!((as_number(run(&binary(BinaryOp::Add, 1.0, 2.0))) - 3.0).abs() < f64::EPSILON);
    assert!((as_number(run(&binary(BinaryOp::Exponent, 2.0, 8.0))) - 256.0).abs() < f64::EPSILON);
}

#[test]
fn remainder_keeps_the_dividends_sign_through_the_whole_pipeline() {
    // `-5 % 3` is -2. Asserted end to end rather than only against the helper, because the
    // operand order surviving the lowering is part of the claim.
    assert!((as_number(run(&binary(BinaryOp::Remainder, -5.0, 3.0))) + 2.0).abs() < f64::EPSILON);
}

#[test]
fn the_int32_wrap_survives_the_whole_pipeline() {
    // `1e10 | 0` is 1410065408. If this had been lowered to a saturating conversion it would
    // be `i32::MAX` — a number, and the wrong one. This is the assertion the whole
    // call-not-instruction decision exists for, and it can only be made by *running* the code.
    let answer = as_number(run(&binary(BinaryOp::BitOr, 1e10, 0.0)));
    assert!(
        (answer - 1_410_065_408.0).abs() < f64::EPSILON,
        "got {answer}, saturation would give {}",
        f64::from(i32::MAX)
    );
}

#[test]
fn the_unsigned_shift_reads_as_unsigned() {
    // `-1 >>> 0` is 4294967295, not -1.
    let answer = as_number(run(&binary(BinaryOp::UnsignedShiftRight, -1.0, 0.0)));
    assert!(
        (answer - 4_294_967_295.0).abs() < f64::EPSILON,
        "got {answer}"
    );
}

#[test]
fn slots_round_trip_through_variables() {
    let mut function = Function::new("slots");
    let value = function.value();
    let loaded = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        number(value, 7.0),
        Instruction {
            result: None,
            ty: Type::Undefined,
            op: Op::Store { slot: 0, value },
            safepoint: None,
        },
        Instruction {
            result: Some(loaded),
            ty: Type::Number,
            op: Op::Load { slot: 0 },
            safepoint: None,
        },
    ];
    entry.terminator = Terminator::Return(Some(loaded));
    assert!((as_number(run(&function)) - 7.0).abs() < f64::EPSILON);
}

#[test]
fn a_branch_takes_the_right_arm() {
    // `1 < 2 ? 10 : 20` — and the *other* arm would also return a number, so a backend that
    // branched the wrong way would still produce a plausible answer.
    let mut function = Function::new("branching");
    let left = function.value();
    let right = function.value();
    let condition = function.value();
    let taken = function.value();
    let missed = function.value();

    let yes = function.block(Block {
        params: Vec::new(),
        instructions: vec![number(taken, 10.0)],
        terminator: Terminator::Return(Some(taken)),
    });
    let no = function.block(Block {
        params: Vec::new(),
        instructions: vec![number(missed, 20.0)],
        terminator: Terminator::Return(Some(missed)),
    });

    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        number(left, 1.0),
        number(right, 2.0),
        Instruction {
            result: Some(condition),
            ty: Type::Bool,
            op: Op::Compare {
                op: CompareOp::Less,
                left,
                right,
            },
            safepoint: None,
        },
    ];
    entry.terminator = Terminator::Branch {
        condition,
        then_block: yes,
        then_args: Vec::new(),
        else_block: no,
        else_args: Vec::new(),
    };

    assert!((as_number(run(&function)) - 10.0).abs() < f64::EPSILON);
}

#[test]
fn strict_equality_on_numbers_agrees_with_javascript_about_nan_and_zero() {
    // The two cases that make `===` interesting, checked by running rather than by reasoning
    // about which instruction was emitted.
    let nan = run(&compare(CompareOp::StrictEqual, f64::NAN, f64::NAN));
    assert_eq!(nan, Value::FALSE, "NaN === NaN is false");

    let zeroes = run(&compare(CompareOp::StrictEqual, 0.0, -0.0));
    assert_eq!(zeroes, Value::TRUE, "+0 === -0 is true");
}

/// `function f() { return <left> === <right>; }`
fn compare(op: CompareOp, left_value: f64, right_value: f64) -> Function {
    let mut function = Function::new("compare");
    let left = function.value();
    let right = function.value();
    let result = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        number(left, left_value),
        number(right, right_value),
        Instruction {
            result: Some(result),
            ty: Type::Bool,
            op: Op::Compare { op, left, right },
            safepoint: None,
        },
    ];
    entry.terminator = Terminator::Return(Some(result));
    function
}

#[test]
fn a_function_that_returns_nothing_returns_undefined() {
    let function = Function::new("empty");
    assert_eq!(run(&function), Value::UNDEFINED);
}
