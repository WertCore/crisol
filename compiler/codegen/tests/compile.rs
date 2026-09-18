//! Compiling IR to object code.
//!
//! The assertions are about **what the backend refuses** as much as what it emits. A code
//! generator that guessed at an operation it had not implemented would produce machine code that
//! runs and is wrong — and that cannot be distinguished from correct code by testing the
//! compiler, only by running the program and noticing the answer.

use crisol_codegen::{Backend, CodegenError, Cranelift};
use crisol_ir::{
    BinaryOp, BlockId, CompareOp, Constant, Function, Instruction, Op, Safepoint, Terminator, Type,
    ValueId,
};

/// The host, so the tests exercise a real ISA rather than a cross-target stub.
fn host() -> Cranelift {
    Cranelift::new(current_platform_triple()).expect("the host is a supported target")
}

fn current_platform_triple() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

fn number(result: ValueId, value: f64) -> Instruction {
    Instruction {
        result: Some(result),
        ty: Type::Number,
        op: Op::Const(Constant::Number(value)),
        safepoint: None,
    }
}

/// `function f() { return <value>; }`
fn returns_constant(value: f64) -> Function {
    let mut function = Function::new("returns_constant");
    let id = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![number(id, value)];
    entry.terminator = Terminator::Return(Some(id));
    function
}

/// Two constants combined with `op`.
fn arithmetic(op: BinaryOp) -> Function {
    let mut function = Function::new("arithmetic");
    let left = function.value();
    let right = function.value();
    let result = function.value();
    let numeric = op.is_always_numeric();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        number(left, 6.0),
        number(right, 3.0),
        Instruction {
            result: Some(result),
            ty: if numeric { Type::Number } else { Type::Unknown },
            op: Op::Binary { op, left, right },
            // `+` can collect, because ToPrimitive calls user code (D-79).
            safepoint: (!numeric).then(Safepoint::default),
        },
    ];
    entry.terminator = Terminator::Return(Some(result));
    function
}

// ---- what it emits -----------------------------------------------------------------------

#[test]
fn a_function_returning_a_constant_compiles() {
    let mut backend = host();
    let _ = backend.compile(&returns_constant(42.0)).expect("compiles");
    let object = backend.finish().expect("emits");
    assert!(
        object.len() > 64,
        "an object file of {} bytes",
        object.len()
    );
}

#[test]
fn the_always_numeric_operators_become_native_instructions() {
    for op in [BinaryOp::Subtract, BinaryOp::Multiply, BinaryOp::Divide] {
        let mut backend = host();
        let _ = backend
            .compile(&arithmetic(op))
            .unwrap_or_else(|error| panic!("{} should compile: {error}", op.symbol()));
        assert!(backend.finish().expect("emits").len() > 64);
    }
}

#[test]
fn addition_becomes_a_call_and_not_a_float_add() {
    // D-79 arriving in the machine code. `+` may concatenate, so the IR types it `Unknown`,
    // and an `Unknown` cannot be a float add — the operands might be strings. A call to the
    // runtime is the correct lowering, not a fallback.
    //
    // Asserted by looking for the imported symbol in the object file: if this were lowered to
    // an `fadd` there would be no relocation to find.
    let mut backend = host();
    backend
        .compile(&arithmetic(BinaryOp::Add))
        .expect("compiles");
    let object = backend.finish().expect("emits");
    let bytes = object.as_slice();
    let needle = b"crisol_add";
    assert!(
        bytes.windows(needle.len()).any(|window| window == needle),
        "the object file should reference the runtime helper"
    );
}

#[test]
fn control_flow_compiles() {
    // A comparison, a branch, and two returns — the shape every `if` lowers to.
    let mut function = Function::new("branching");
    let left = function.value();
    let right = function.value();
    let condition = function.value();

    let yes = function.block(crisol_ir::Block {
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: Terminator::Return(Some(left)),
    });
    let no = function.block(crisol_ir::Block {
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: Terminator::Return(Some(right)),
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

    assert_eq!(crisol_ir::verify(&function), Ok(()), "{function}");
    let mut backend = host();
    let _ = backend.compile(&function).expect("compiles");
    assert!(backend.finish().expect("emits").len() > 64);
}

#[test]
fn slots_become_variables() {
    // Cranelift's variables do the `mem2reg` this IR has not done yet (D-59), which is why
    // slot-based IR can be lowered directly instead of needing SSA construction first.
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

    let mut backend = host();
    let _ = backend.compile(&function).expect("compiles");
    assert!(backend.finish().expect("emits").len() > 64);
}

// ---- what it refuses ---------------------------------------------------------------------

#[test]
fn an_operator_needing_int32_coercion_is_refused_not_approximated() {
    // `%`, `**` and the bitwise operators need int32 coercion or a libm call. Guessing at
    // either would produce plausible wrong numbers, which is worse than a refusal because the
    // compiler's own tests cannot see it.
    for op in [
        BinaryOp::Remainder,
        BinaryOp::Exponent,
        BinaryOp::BitAnd,
        BinaryOp::ShiftLeft,
        BinaryOp::UnsignedShiftRight,
    ] {
        let mut backend = host();
        let error = backend
            .compile(&arithmetic(op))
            .expect_err("should be refused");
        assert!(
            matches!(error, CodegenError::Unsupported { .. }),
            "{}: {error:?}",
            op.symbol()
        );
    }
}

#[test]
fn strict_equality_is_refused_because_nan_and_negative_zero_are_the_point() {
    // `===` on boxed values is a bit comparison *except* for NaN and ±0, and getting that
    // wrong is D-53's whole subject. Refused rather than approximated.
    let mut function = Function::new("eq");
    let left = function.value();
    let right = function.value();
    let result = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        number(left, 1.0),
        number(right, 1.0),
        Instruction {
            result: Some(result),
            ty: Type::Bool,
            op: Op::Compare {
                op: CompareOp::StrictEqual,
                left,
                right,
            },
            safepoint: None,
        },
    ];
    entry.terminator = Terminator::Return(Some(result));

    let mut backend = host();
    assert!(matches!(
        backend.compile(&function).expect_err("refused"),
        CodegenError::Unsupported { .. }
    ));
}

#[test]
fn a_safepoint_produces_stack_map_entries() {
    // §M13's deliverable is "stack map emission at safepoints", and a test that only checks
    // the function compiled verifies nothing about emission. §M11 made safepoints carry an
    // explicit live set precisely so this step had something to hand over — without it there
    // would be nothing to declare, and a collection during an allocation would miss live
    // references (§3.1).
    let mut function = Function::new("with_safepoint");
    let kept = function.value();
    let allocated = function.value();
    let after = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        number(kept, 1.0),
        Instruction {
            result: Some(allocated),
            ty: Type::Unknown,
            op: Op::Binary {
                op: BinaryOp::Add,
                left: kept,
                right: kept,
            },
            // `kept` must survive the call, so it is live across the safepoint.
            safepoint: Some(Safepoint { live: vec![kept] }),
        },
        // **`kept` has to be used *after* the call**, or it is not live across it and
        // Cranelift is right to record nothing. The first version of this test returned
        // `allocated` and asserted one entry: the count was 0, and the test was wrong rather
        // than the backend — a value whose last use *is* the call argument does not need to
        // survive the collection.
        Instruction {
            result: Some(after),
            ty: Type::Number,
            op: Op::Binary {
                op: BinaryOp::Multiply,
                left: kept,
                right: allocated,
            },
            safepoint: None,
        },
    ];
    entry.terminator = Terminator::Return(Some(after));

    let mut backend = host();
    let report = backend.compile(&function).expect("compiles");
    assert!(
        report.stack_map_entries >= 1,
        "a value live across the safepoint should be declared, got {}",
        report.stack_map_entries
    );
}

#[test]
fn a_function_without_safepoints_declares_none() {
    // The other direction: a count that was always non-zero would prove nothing.
    let mut backend = host();
    let report = backend.compile(&returns_constant(1.0)).expect("compiles");
    assert_eq!(report.stack_map_entries, 0);
}

#[test]
fn an_unknown_target_is_refused() {
    let error = Cranelift::new("not-a-real-triple").expect_err("refused");
    assert!(
        matches!(error, CodegenError::UnknownTarget { .. }),
        "{error:?}"
    );
}

#[test]
fn every_listed_target_can_be_constructed() {
    // §M13 names four. A backend that could only build for the host would pass every test here
    // and fail the acceptance, which is explicitly about all four.
    for triple in [
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
    ] {
        assert!(
            Cranelift::new(triple).is_ok(),
            "{triple} should be constructible"
        );
    }
}
