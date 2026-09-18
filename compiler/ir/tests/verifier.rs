//! What the verifier accepts, and what it refuses.
//!
//! §M11's acceptance is that it "rejects malformed graphs", so every rejection here is a test
//! that builds the malformed graph rather than asserting a message. Several kinds of
//! malformed are *absent* on purpose — a block with two terminators, or an operation after
//! one, cannot be constructed, because `Terminator` is a field rather than an `Op` variant.
//! Those are not missing tests; they are things the type system already refuses.

use crisol_ir::{
    Block, BlockId, CompareOp, Constant, Function, FunctionId, Instruction, Op, Safepoint,
    Terminator, Type, ValueId, VerifyError, verify, verify_module,
};
use crisol_value::{PropertyKey, Shapes};

/// `v = const n`, which cannot collect and so takes no safepoint.
fn constant(result: ValueId, number: f64) -> Instruction {
    Instruction {
        result: Some(result),
        ty: Type::Number,
        op: Op::Const(Constant::Number(number)),
        safepoint: None,
    }
}

/// An allocation, which can collect and so must carry one.
fn allocate(result: ValueId, shapes: &mut Shapes, live: &[ValueId]) -> Instruction {
    let shape = shapes.add(shapes.root(), &PropertyKey::new("x"));
    Instruction {
        result: Some(result),
        ty: Type::object(shape),
        op: Op::CreateObject { shape },
        safepoint: Some(Safepoint {
            live: live.to_vec(),
        }),
    }
}

fn errors(function: &Function) -> Vec<VerifyError> {
    verify(function).err().unwrap_or_default()
}

// ---- what it accepts ------------------------------------------------------------------

#[test]
fn a_straight_line_function_verifies() {
    let mut shapes = Shapes::new();
    let mut function = Function::new("straight");
    let a = function.value();
    let object = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![constant(a, 1.0), allocate(object, &mut shapes, &[a])];
    entry.terminator = Terminator::Return(Some(object));

    assert_eq!(verify(&function), Ok(()), "{function}");
}

#[test]
fn a_diamond_with_block_parameters_verifies() {
    // bb0: branch -> bb1(1.0), bb2(2.0); bb1/bb2 jump to bb3(v), bb3 returns it.
    // The shape a phi node would have, written as a parameter.
    let mut function = Function::new("diamond");
    let condition = function.value();
    let left = function.value();
    let right = function.value();
    let merged = function.value();

    let join = function.block(Block {
        params: vec![(merged, Type::Number)],
        instructions: Vec::new(),
        terminator: Terminator::Return(Some(merged)),
    });
    let then_block = function.block(Block {
        params: Vec::new(),
        instructions: vec![constant(left, 1.0)],
        terminator: Terminator::Jump {
            target: join,
            args: vec![left],
        },
    });
    let else_block = function.block(Block {
        params: Vec::new(),
        instructions: vec![constant(right, 2.0)],
        terminator: Terminator::Jump {
            target: join,
            args: vec![right],
        },
    });

    let zero = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        constant(zero, 0.0),
        Instruction {
            result: Some(condition),
            ty: Type::Bool,
            op: Op::Compare {
                op: CompareOp::Less,
                left: zero,
                right: zero,
            },
            safepoint: None,
        },
    ];
    entry.terminator = Terminator::Branch {
        condition,
        then_block,
        then_args: Vec::new(),
        else_block,
        else_args: Vec::new(),
    };

    assert_eq!(verify(&function), Ok(()), "{function}");
}

// ---- what it refuses ------------------------------------------------------------------

#[test]
fn a_value_that_is_never_defined_is_refused() {
    let mut function = Function::new("undefined");
    let ghost = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.terminator = Terminator::Return(Some(ghost));
    // `ghost` was allocated an id but never defined by an instruction or a parameter.
    assert!(errors(&function).contains(&VerifyError::Undefined {
        value: ghost,
        at: BlockId::ENTRY
    }));
}

#[test]
fn using_a_value_before_it_is_defined_in_the_same_block_is_refused() {
    let mut function = Function::new("too-early");
    let first = function.value();
    let second = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        Instruction {
            result: Some(first),
            ty: Type::Bool,
            op: Op::Compare {
                op: CompareOp::StrictEqual,
                left: second,
                right: second,
            },
            safepoint: None,
        },
        constant(second, 1.0),
    ];
    assert!(errors(&function).contains(&VerifyError::NotDominated {
        value: second,
        at: BlockId::ENTRY
    }));
}

#[test]
fn a_value_from_the_other_arm_of_a_branch_is_refused() {
    // The case that needs dominance rather than ordering: `left` is defined in bb1 and used in
    // bb2, and bb1 does not dominate bb2 even though it comes first.
    let mut function = Function::new("cross-branch");
    let condition = function.value();
    let left = function.value();

    let then_block = function.block(Block {
        params: Vec::new(),
        instructions: vec![constant(left, 1.0)],
        terminator: Terminator::Return(None),
    });
    let else_block = function.block(Block {
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: Terminator::Return(Some(left)),
    });
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![Instruction {
        result: Some(condition),
        ty: Type::Bool,
        op: Op::Const(Constant::Bool(true)),
        safepoint: None,
    }];
    entry.terminator = Terminator::Branch {
        condition,
        then_block,
        then_args: Vec::new(),
        else_block,
        else_args: Vec::new(),
    };

    assert!(
        errors(&function).contains(&VerifyError::NotDominated {
            value: left,
            at: else_block
        }),
        "{function}"
    );
}

#[test]
fn the_wrong_number_of_branch_arguments_is_refused() {
    let mut function = Function::new("arity");
    let value = function.value();
    let param = function.value();
    let target = function.block(Block {
        params: vec![(param, Type::Number)],
        instructions: Vec::new(),
        terminator: Terminator::Return(Some(param)),
    });
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![constant(value, 1.0)];
    entry.terminator = Terminator::Jump {
        target,
        args: Vec::new(),
    };

    assert!(
        errors(&function).contains(&VerifyError::WrongArgumentCount {
            from: BlockId::ENTRY,
            target,
            passed: 0,
            expected: 1
        })
    );
}

#[test]
fn a_branch_argument_of_the_wrong_type_is_refused() {
    let mut function = Function::new("types");
    let text = function.value();
    let param = function.value();
    let target = function.block(Block {
        params: vec![(param, Type::Number)],
        instructions: Vec::new(),
        terminator: Terminator::Return(Some(param)),
    });
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![Instruction {
        result: Some(text),
        ty: Type::String,
        op: Op::Const(Constant::String("no".to_owned())),
        safepoint: None,
    }];
    entry.terminator = Terminator::Jump {
        target,
        args: vec![text],
    };

    assert!(errors(&function).contains(&VerifyError::WrongArgumentType {
        from: BlockId::ENTRY,
        target,
        index: 0,
        passed: Type::String,
        expected: Type::Number
    }));
}

#[test]
fn a_jump_to_a_block_that_does_not_exist_is_refused() {
    let mut function = Function::new("nowhere");
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.terminator = Terminator::Jump {
        target: BlockId::from_index(99),
        args: Vec::new(),
    };
    assert!(errors(&function).contains(&VerifyError::NoSuchBlock {
        from: BlockId::ENTRY,
        target: BlockId::from_index(99)
    }));
}

// ---- safepoints, which §M11 singles out ------------------------------------------------

#[test]
fn an_allocation_without_a_safepoint_is_refused() {
    // §M11: "the IR must represent safepoints explicitly or the GC integration in M13 will not
    // work". A missing one is §3.1's use-after-free-under-pressure, so it is refused here.
    let mut shapes = Shapes::new();
    let shape = shapes.add(shapes.root(), &PropertyKey::new("x"));
    let mut function = Function::new("no-safepoint");
    let object = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![Instruction {
        result: Some(object),
        ty: Type::object(shape),
        op: Op::CreateObject { shape },
        safepoint: None,
    }];
    entry.terminator = Terminator::Return(Some(object));

    assert!(errors(&function).contains(&VerifyError::MissingSafepoint {
        at: BlockId::ENTRY,
        index: 0
    }));
}

#[test]
fn a_safepoint_on_something_that_cannot_collect_is_refused() {
    // Rejected rather than ignored: it means whoever built this did not know which operations
    // collect, and the ones they missed are the dangerous half.
    let mut function = Function::new("extra-safepoint");
    let value = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![Instruction {
        result: Some(value),
        ty: Type::Number,
        op: Op::Const(Constant::Number(1.0)),
        safepoint: Some(Safepoint::default()),
    }];
    entry.terminator = Terminator::Return(Some(value));

    assert!(
        errors(&function).contains(&VerifyError::UnexpectedSafepoint {
            at: BlockId::ENTRY,
            index: 0
        })
    );
}

#[test]
fn a_property_load_needs_a_safepoint_because_a_getter_is_a_call() {
    let mut shapes = Shapes::new();
    let shape = shapes.add(shapes.root(), &PropertyKey::new("x"));
    let mut function = Function::new("getter");
    let object = function.value();
    let read = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        Instruction {
            result: Some(object),
            ty: Type::object(shape),
            op: Op::CreateObject { shape },
            safepoint: Some(Safepoint::default()),
        },
        Instruction {
            result: Some(read),
            ty: Type::Unknown,
            op: Op::PropertyLoad {
                object,
                key: PropertyKey::new("x"),
            },
            safepoint: None,
        },
    ];
    entry.terminator = Terminator::Return(Some(read));

    assert!(
        errors(&function).contains(&VerifyError::MissingSafepoint {
            at: BlockId::ENTRY,
            index: 1
        }),
        "a getter runs user code, and an exotic shape makes the lookup itself a call"
    );
}

#[test]
fn a_safepoint_naming_an_undefined_value_is_refused() {
    let mut shapes = Shapes::new();
    let mut function = Function::new("bad-live-set");
    let object = function.value();
    let ghost = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![allocate(object, &mut shapes, &[ghost])];
    entry.terminator = Terminator::Return(Some(object));

    assert!(errors(&function).contains(&VerifyError::Undefined {
        value: ghost,
        at: BlockId::ENTRY
    }));
}

#[test]
fn every_problem_is_reported_not_just_the_first() {
    // A malformed graph usually has one cause and several symptoms, and stopping at the first
    // makes the cause the hardest one to see.
    let mut function = Function::new("several");
    let ghost = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.terminator = Terminator::Jump {
        target: BlockId::from_index(42),
        args: vec![ghost],
    };
    let found = errors(&function);
    assert!(found.len() >= 2, "expected several problems, got {found:?}");
}

// ---- the check that needs more than one function ------------------------------------------

/// A function taking `count` captured values.
fn callee(count: usize) -> Function {
    let mut function = Function::new("callee");
    function.captures = (0..u32::try_from(count).expect("small")).collect();
    function
}

/// A program whose entry block closes over `@1` with `count` values.
fn caller(count: usize) -> Function {
    let mut function = Function::new("caller");
    let mut captures = Vec::new();
    for value in 0..count {
        let id = function.value();
        #[expect(clippy::cast_precision_loss, reason = "test values are tiny")]
        let literal = Constant::Number(value as f64);
        function
            .get_mut(BlockId::ENTRY)
            .expect("entry")
            .instructions
            .push(Instruction {
                result: Some(id),
                ty: Type::Number,
                op: Op::Const(literal),
                safepoint: None,
            });
        captures.push(id);
    }
    let result = function.value();
    function
        .get_mut(BlockId::ENTRY)
        .expect("entry")
        .instructions
        .push(Instruction {
            result: Some(result),
            ty: Type::Object(None),
            op: Op::Closure {
                function: FunctionId(1),
                captures,
            },
            safepoint: Some(Safepoint::default()),
        });
    function
}

#[test]
fn a_matching_closure_verifies() {
    assert_eq!(verify_module(&[caller(2), callee(2)]), Ok(()));
    assert_eq!(verify_module(&[caller(0), callee(0)]), Ok(()));
}

#[test]
fn passing_the_wrong_number_of_captures_is_refused() {
    // The pairing is positional, so a mismatch means the callee reads a slot nobody filled —
    // uninitialised, and plausible. Invisible to a verifier that sees one function at a time,
    // which is why `verify_module` exists.
    let errors = verify_module(&[caller(1), callee(2)]).expect_err("refused");
    assert!(
        errors.contains(&VerifyError::WrongCaptureCount {
            at: BlockId::ENTRY,
            function: 1,
            passed: 1,
            expected: 2,
        }),
        "{errors:?}"
    );

    let too_many = verify_module(&[caller(3), callee(2)]).expect_err("refused");
    assert!(!too_many.is_empty(), "too many is as wrong as too few");
}

#[test]
fn closing_over_a_function_that_does_not_exist_is_refused() {
    let errors = verify_module(&[caller(0)]).expect_err("refused");
    assert!(
        errors.contains(&VerifyError::NoSuchFunction {
            at: BlockId::ENTRY,
            function: 1,
        }),
        "{errors:?}"
    );
}

#[test]
fn a_module_reports_problems_from_every_function() {
    // One malformed closure must not hide a malformed body elsewhere.
    let mut broken = Function::new("broken");
    let ghost = broken.value();
    broken.get_mut(BlockId::ENTRY).expect("entry").terminator = Terminator::Return(Some(ghost));
    let errors = verify_module(&[caller(1), callee(2), broken]).expect_err("refused");
    assert!(
        errors.len() >= 2,
        "both the arity mismatch and the undefined value: {errors:?}"
    );
}
