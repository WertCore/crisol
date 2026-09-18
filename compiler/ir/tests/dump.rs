//! The text dump.
//!
//! §M11's acceptance asks for a dump that is "stable and reviewable". Both halves are tested
//! here: stable by dumping the same function twice and comparing, reviewable by pinning the
//! exact text — if a change to the format is an improvement, the diff in this file is where
//! someone gets to agree with that.

use crisol_ir::{
    Block, BlockId, CompareOp, Constant, Function, Instruction, Op, Safepoint, Terminator, Type,
    ValueId,
};
use crisol_value::{PropertyKey, Shapes};

fn number(result: ValueId, value: f64) -> Instruction {
    Instruction {
        result: Some(result),
        ty: Type::Number,
        op: Op::Const(Constant::Number(value)),
        safepoint: None,
    }
}

#[test]
fn a_function_dumps_the_way_it_is_expected_to() {
    let mut shapes = Shapes::new();
    let shape = shapes.add(shapes.root(), &PropertyKey::new("x"));

    let mut function = Function::new("example");
    let one = function.value();
    let two = function.value();
    let less = function.value();
    let object = function.value();
    let merged = function.value();

    let join = function.block(Block {
        params: vec![(merged, Type::Number)],
        instructions: Vec::new(),
        terminator: Terminator::Return(Some(merged)),
    });
    let allocating = function.block(Block {
        params: Vec::new(),
        instructions: vec![
            Instruction {
                result: Some(object),
                ty: Type::object(shape),
                op: Op::CreateObject { shape },
                safepoint: Some(Safepoint {
                    live: vec![one, two],
                }),
            },
            Instruction {
                result: None,
                ty: Type::Undefined,
                op: Op::PropertyStore {
                    object,
                    key: PropertyKey::new("x"),
                    value: one,
                },
                safepoint: Some(Safepoint { live: vec![object] }),
            },
        ],
        terminator: Terminator::Jump {
            target: join,
            args: vec![one],
        },
    });

    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![
        number(one, 1.0),
        number(two, 2.0),
        Instruction {
            result: Some(less),
            ty: Type::Bool,
            op: Op::Compare {
                op: CompareOp::Less,
                left: one,
                right: two,
            },
            safepoint: None,
        },
    ];
    entry.terminator = Terminator::Branch {
        condition: less,
        then_block: allocating,
        then_args: Vec::new(),
        else_block: join,
        else_args: vec![two],
    };

    let expected = "\
function example {
bb0:  ; entry
    v0: number = const 1.0
    v1: number = const 2.0
    v2: bool = lt v0, v1
    branch v2 -> bb2, bb1(v1)
bb1(v4: number):
    return v4
bb2:
    v3: object#2 = object #2  ; safepoint [v0, v1]
    set v3.\"x\" = v0  ; safepoint [v3]
    jump bb1(v0)
}
";
    assert_eq!(function.to_string(), expected, "\n--- got ---\n{function}");
}

#[test]
fn the_dump_is_stable() {
    // Nothing printed may come from a hash map's iteration order or an address.
    let mut function = Function::new("stable");
    let value = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![number(value, 1.5)];
    entry.terminator = Terminator::Return(Some(value));

    let once = function.to_string();
    for _ in 0..20 {
        assert_eq!(function.to_string(), once);
    }
}

#[test]
fn a_whole_number_still_prints_as_a_double() {
    // JavaScript has one number type, and a dump that printed `1` would invite the reader to
    // believe there is an integer in there.
    let mut function = Function::new("doubles");
    let value = function.value();
    let entry = function.get_mut(BlockId::ENTRY).expect("entry");
    entry.instructions = vec![number(value, 1.0)];
    entry.terminator = Terminator::Return(Some(value));
    assert!(function.to_string().contains("const 1.0"), "{function}");
}

#[test]
fn every_operation_prints() {
    // Not an assertion about the text, just that nothing panics or prints empty. A dump is
    // most wanted when something is wrong, which is exactly when an unprintable operation
    // would be worst.
    let mut shapes = Shapes::new();
    let shape = shapes.add(shapes.root(), &PropertyKey::new("x"));
    let value = ValueId::from_index(0);
    let ops = vec![
        Op::Const(Constant::Undefined),
        Op::Const(Constant::Null),
        Op::Const(Constant::Bool(true)),
        Op::Const(Constant::String("hello".to_owned())),
        Op::Load { slot: 3 },
        Op::Store { slot: 3, value },
        Op::Call {
            callee: value,
            this_value: value,
            args: vec![value],
        },
        Op::PropertyLoad {
            object: value,
            key: PropertyKey::new("k"),
        },
        Op::CreateObject { shape },
        Op::CreateArray {
            elements: vec![value],
        },
        Op::Closure {
            function: crisol_ir::FunctionId(7),
            captures: vec![value],
        },
        Op::Await { value },
        Op::Compare {
            op: CompareOp::GreaterEqual,
            left: value,
            right: value,
        },
    ];

    for op in ops {
        let mut function = Function::new("each");
        let result = function.value();
        let safepoint = op.can_collect().then(Safepoint::default);
        let entry = function.get_mut(BlockId::ENTRY).expect("entry");
        entry.instructions = vec![Instruction {
            result: Some(result),
            ty: Type::Unknown,
            op,
            safepoint,
        }];
        let text = function.to_string();
        assert!(text.contains("v0"), "{text}");
    }
}
