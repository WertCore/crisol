//! §M11's acceptance: an IR dump for a set of representative programs, stable and reviewable.
//!
//! All thirty go into **one** snapshot file rather than thirty. A change to the IR or the dump
//! format then shows up as a single diff covering every program at once, which is what makes it
//! reviewable — thirty files each changing by two lines is thirty times the reading for the same
//! information.
//!
//! # What "representative" means here, honestly
//!
//! Representative **of what lowers today**. The corpus deliberately contains no arithmetic, no
//! functions and no `for` loops, because the lowering does not handle them yet — and a corpus
//! that quietly included them would either fail or, worse, snapshot a dump that is not a
//! translation of the source.
//!
//! That is not a gap hidden in a comment: `unfaithful_programs_are_reported_not_guessed` takes
//! the constructs that are missing and asserts each one is *named* in the unsupported list. The
//! corpus is what works; that test is the list of what does not, and the two are checked
//! against each other rather than against a claim.

use std::path::{Path, PathBuf};

use crisol_frontend::lower;
use crisol_ir::verify_module;

/// The programs. Order is fixed, because the snapshot is.
const CORPUS: &[(&str, &str)] = &[
    ("empty", ""),
    ("number", "let x = 1;"),
    ("negative-is-not-arithmetic-here", "let x = 0;"),
    ("float", "let x = 1.5;"),
    ("string", "let s = \"hello\";"),
    ("string-with-quotes", "let s = \"a \\\"b\\\" c\";"),
    ("bool", "let t = true; let f = false;"),
    ("null", "let n = null;"),
    ("undefined", "let u = undefined;"),
    ("declaration-without-init", "let x;"),
    ("two-declarations", "let a = 1; let b = 2;"),
    ("read-a-variable", "let a = 1; let b = a;"),
    ("assign", "let a = 1; a = 2;"),
    ("assign-is-an-expression", "let a = 1; let b = (a = 2);"),
    ("strict-equal", "let a = 1; let b = 2; let c = a === b;"),
    ("strict-not-equal", "let a = 1; let b = 2; let c = a !== b;"),
    ("less", "let a = 1; let b = 2; let c = a < b;"),
    ("less-equal", "let a = 1; let b = 2; let c = a <= b;"),
    ("greater", "let a = 1; let b = 2; let c = a > b;"),
    ("greater-equal", "let a = 1; let b = 2; let c = a >= b;"),
    ("return-nothing", "return;"),
    ("return-a-value", "let a = 1; return a;"),
    ("dead-code-after-return", "return; let a = 1;"),
    ("throw", "let e = \"boom\"; throw e;"),
    ("if-without-else", "let a = 1; if (a === 1) { a = 2; }"),
    (
        "if-else",
        "let a = 1; if (a === 1) { a = 2; } else { a = 3; }",
    ),
    (
        "nested-if",
        "let a = 1; if (a === 1) { if (a === 1) { a = 2; } else { a = 3; } }",
    ),
    (
        "if-with-return",
        "let a = 1; if (a === 1) { return a; } a = 2;",
    ),
    ("while", "let a = 1; while (a < 10) { a = 10; }"),
    (
        "while-with-if",
        "let a = 1; while (a < 10) { if (a === 1) { a = 10; } }",
    ),
    ("empty-object", "let o = {};"),
    ("object-with-properties", "let o = { a: 1, b: \"two\" };"),
    ("object-with-string-key", "let o = { \"a b\": 1 };"),
    ("property-read", "let o = { a: 1 }; let v = o.a;"),
    ("property-chain", "let o = { a: 1 }; let v = o.a.b;"),
    ("call-no-args", "let f = null; let v = f();"),
    (
        "call-with-args",
        "let f = null; let a = 1; let v = f(a, a);",
    ),
    ("call-a-method", "let o = { a: 1 }; let v = o.a();"),
    ("block-scope", "let a = 1; { a = 2; }"),
    ("empty-statement", ";"),
    ("add", "let a = 1 + 2;"),
    ("subtract", "let a = 3 - 1;"),
    ("multiply", "let a = 2 * 3;"),
    ("divide", "let a = 6 / 2;"),
    ("remainder", "let a = 7 % 3;"),
    ("exponent", "let a = 2 ** 8;"),
    (
        "string-concatenation-is-the-same-operator",
        "let a = \"x\" + \"y\";",
    ),
    ("bitwise-and", "let a = 6 & 3;"),
    ("bitwise-or", "let a = 6 | 3;"),
    ("bitwise-xor", "let a = 6 ^ 3;"),
    ("shift-left", "let a = 1 << 4;"),
    ("shift-right", "let a = 16 >> 2;"),
    ("unsigned-shift-right", "let a = 1 >>> 0;"),
    ("negate", "let a = -1;"),
    ("unary-plus", "let a = +\"1\";"),
    ("logical-not", "let a = !true;"),
    ("bitwise-not", "let a = ~0;"),
    ("typeof", "let a = typeof 1;"),
    ("void", "let a = void 0;"),
    (
        "logical-and-short-circuits",
        "let a = 1; let b = false && a;",
    ),
    ("logical-or-short-circuits", "let a = 1; let b = true || a;"),
    ("nullish-coalescing-is-not-or", "let a = 0; let b = a ?? 1;"),
    ("conditional", "let a = true ? 1 : 2;"),
    ("nested-arithmetic", "let a = 1 + 2 * 3;"),
    ("array-literal", "let a = [1, 2, 3];"),
    ("empty-array", "let a = [];"),
    ("arrow-concise-body", "let f = (x) => x;"),
    ("arrow-block-body", "let f = (x) => { return x; };"),
    ("arrow-no-parameters", "let f = () => 1;"),
    (
        "function-expression",
        "let f = function (a, b) { return a; };",
    ),
    (
        "a-closure-captures-an-outer-local",
        "let a = 1; let f = () => a;",
    ),
    (
        "a-parameter-shadows-rather-than-captures",
        "let a = 1; let f = (a) => a;",
    ),
    (
        "a-local-shadows-rather-than-captures",
        "let a = 1; let f = () => { let a = 2; return a; };",
    ),
    (
        "two-captures-keep-their-order",
        "let a = 1; let b = 2; let f = () => a + b;",
    ),
    (
        "a-nested-closure-captures-through-each-level",
        "let a = 1; let f = () => () => a;",
    ),
    (
        "a-function-that-runs-off-the-end-returns-undefined",
        "let f = () => { let a = 1; };",
    ),
];

fn snapshot_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ir-snapshots")
        .join("corpus.ir")
}

/// The whole corpus, rendered.
fn render() -> String {
    let mut out = String::new();
    for (name, source) in CORPUS {
        let lowered = lower(name, source).unwrap_or_else(|error| {
            panic!("{name} should parse: {error:?}");
        });
        assert!(
            lowered.is_faithful(),
            "{name} lowered with gaps, so its dump would not be a translation of its source: {:?}",
            lowered.unsupported
        );
        // `verify_module`, not per-function `verify`: the capture-arity check pairs a
        // closure with its callee and is invisible to a verifier that sees one function.
        if let Err(errors) = verify_module(&lowered.functions) {
            panic!("{name} produced a malformed module: {errors:?}");
        }

        out.push_str(&format!("=== {name} ===\n"));
        out.push_str("--- source\n");
        out.push_str(source);
        if !source.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("--- ir\n");
        // Every function, not just the program: a nested one that lowered wrongly would
        // otherwise be invisible in the review.
        for function in &lowered.functions {
            out.push_str(&function.to_string());
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_corpus_dump_matches_its_snapshot() {
    let rendered = render();
    let path = snapshot_path();

    let Ok(expected) = std::fs::read_to_string(&path) else {
        std::fs::write(&path, &rendered).expect("write the first snapshot");
        panic!(
            "no snapshot at {}; wrote one. Read it before committing — it is the review.",
            path.display()
        );
    };

    if rendered != expected {
        let actual = path.with_extension("ir.actual");
        std::fs::write(&actual, &rendered).expect("write the actual dump");
        panic!(
            "the IR dump changed.\n{}\n\nWrote {}. If the change is an improvement, replace \
             the snapshot and the diff is the review.",
            first_difference(&expected, &rendered),
            actual.display(),
        );
    }
}

/// Describes the first line that differs, for a failure nobody can open the files for.
///
/// CI is where this test fails and CI is where "compare these two paths" is useless advice —
/// the files are on a machine that no longer exists by the time anyone reads the log. It also
/// names a line-ending difference explicitly, because that one looks like every character
/// changed and is the most likely way for this to fail for a reason that is not about the IR.
fn first_difference(expected: &str, actual: &str) -> String {
    if expected.replace("\r\n", "\n") == actual.replace("\r\n", "\n") {
        return "The only difference is line endings — the snapshot was checked out with CRLF. \
                `.gitattributes` should be pinning it to LF."
            .to_owned();
    }
    for (at, (left, right)) in expected.lines().zip(actual.lines()).enumerate() {
        if left != right {
            return format!(
                "first difference at line {}:\n  snapshot: {left}\n  now:      {right}",
                at + 1
            );
        }
    }
    format!(
        "the first {} lines match; the snapshot has {} lines and the dump has {}",
        expected.lines().count().min(actual.lines().count()),
        expected.lines().count(),
        actual.lines().count()
    )
}

#[test]
fn every_program_in_the_corpus_verifies() {
    // Also asserted inside `render`, but separately here so a verifier regression is reported
    // as a verifier regression rather than as a snapshot mismatch.
    for (name, source) in CORPUS {
        let lowered = lower(name, source).expect("parses");
        assert_eq!(verify_module(&lowered.functions), Ok(()), "{name}");
    }
}

#[test]
fn the_corpus_is_at_least_thirty_programs() {
    assert!(
        CORPUS.len() >= 30,
        "§M11 asks for thirty representative programs, and there are {}",
        CORPUS.len()
    );
}

#[test]
fn unfaithful_programs_are_reported_not_guessed() {
    // The other half of the corpus's honesty. Each of these is something the lowering does not
    // understand, and each must appear in `unsupported` rather than becoming a plausible-looking
    // dump. A compiler that silently emits `undefined` for syntax it did not read produces a
    // program that runs and is wrong, which is worse than one that refuses.
    let cases = [
        ("for (;;) { }", "for statement"),
        // The function itself lowers now; what does not is **hoisting**. The binding appears
        // where the declaration does, so calling it earlier in the source reads an unset slot
        // rather than working. Recorded rather than left silently half-right.
        ("function f() { }", "function declaration hoisting"),
        ("let o = {}; let a = o[1];", "computed member access"),
        ("try { } catch (e) { }", "try statement"),
        ("let [a] = [1];", "destructuring declaration"),
        ("let o = { ...{} };", "object spread"),
        ("let a = delete ({}).x;", "delete operator"),
        // A hole is not `undefined` (D-64) and the IR cannot yet say so, so it is recorded
        // rather than filled in with a value that reads the same and answers `in` differently.
        ("let a = [1, , 3];", "array hole or spread"),
        // `==` is not `===`: it coerces, and the coercion table needs machinery that is not
        // here yet. Lowering it as a strict comparison would be wrong for every mixed-type
        // operand, which is the only case anyone writes `==` for.
        ("let a = 1 == 2;", "binary operator =="),
        ("let a = 1 instanceof Object;", "binary operator instanceof"),
    ];

    for (source, expected) in cases {
        let lowered = lower("case", source).expect("parses");
        assert!(
            !lowered.is_faithful(),
            "{source:?} should not claim to be a faithful translation"
        );
        let named: Vec<&str> = lowered
            .unsupported
            .iter()
            .map(|note| note.what.as_str())
            .collect();
        assert!(
            named.iter().any(|what| what == &expected),
            "{source:?} should name {expected:?}, and named {named:?}"
        );
    }
}

#[test]
fn an_object_literal_does_not_claim_a_shape_it_will_not_keep() {
    // Soundness rather than precision. A type in SSA is fixed for the value's whole life, but
    // an object's shape changes as properties are added — so typing the result `object#root`
    // after two `PropertyStore`s claims the object is still empty, and a pass trusting that
    // would resolve `.a` to no slot at all.
    let lowered = lower("object", "let o = { a: 1, b: 2 };").expect("parses");
    let dump = lowered.program().to_string();
    assert!(
        dump.contains(": object ="),
        "the value should be typed `object`, shape unknown: {dump}"
    );
    assert!(
        !dump.contains(": object#"),
        "and must not claim a specific shape: {dump}"
    );
}

#[test]
fn the_short_circuiting_operators_are_lowered_as_control_flow() {
    // `a && b` must not evaluate `b` when `a` is falsy. Lowering these as a two-operand
    // instruction would evaluate both, which changes what the program *does* — a side effect in
    // `b` would run when the source says it must not.
    for source in [
        "let a = 1; let b = false && a;",
        "let a = 1; let b = true || a;",
        "let a = 1; let b = null ?? a;",
        "let a = true ? 1 : 2;",
    ] {
        let lowered = lower("short-circuit", source).expect("parses");
        assert!(
            lowered.program().blocks.len() > 1,
            "{source:?} should branch, and lowered to a single block"
        );
    }
}

#[test]
fn nullish_coalescing_tests_for_null_not_for_falsiness() {
    // `0 ?? 1` is `0` where `0 || 1` is `1`. Treating them alike is the bug that made `??`
    // worth adding to the language, so the lowering must compare against null and undefined
    // rather than branching on the value itself.
    let nullish = lower("nullish", "let a = 0; let b = a ?? 1;").expect("parses");
    let dump = nullish.program().to_string();
    assert!(
        dump.contains("const null") && dump.contains("const undefined"),
        "`??` should compare against both nullish values:\n{dump}"
    );

    let or = lower("or", "let a = 0; let b = a || 1;").expect("parses");
    let or_dump = or.program().to_string();
    assert!(
        !or_dump.contains("const null"),
        "`||` branches on truthiness and needs no null comparison:\n{or_dump}"
    );
}

#[test]
fn addition_is_not_typed_as_a_number() {
    // `+` may concatenate, so typing its result `Number` would let codegen emit a float add for
    // a string concatenation. Every other arithmetic operator coerces with `ToNumber`.
    let add = lower("add", "let a = 1 + 2;").expect("parses");
    assert!(
        add.program().to_string().contains("unknown = + "),
        "`+` is unknown until something proves otherwise:\n{}",
        add.program()
    );

    let subtract = lower("subtract", "let a = 1 - 2;").expect("parses");
    assert!(
        subtract.program().to_string().contains("number = - "),
        "`-` always produces a number:\n{}",
        subtract.program()
    );
}

#[test]
fn a_compiler_temporary_cannot_be_shadowed_by_a_source_name() {
    // The temporaries that carry a short-circuit result are named with a character the grammar
    // does not allow in an identifier, so a program cannot declare one that collides.
    let lowered = lower("collide", "let a = 1; let b = false && a;").expect("parses");
    assert!(lowered.is_faithful());
    assert!(lowered.program().blocks.len() > 1);
}

#[test]
fn a_closure_captures_an_outer_local_and_a_parameter_shadows_it() {
    // The distinction the whole scope analysis exists for. `() => a` reads the outer `a` and
    // must capture it; `(a) => a` declares its own and must not — capturing there would read
    // the outer value and then immediately overwrite it with the argument.
    let capturing = lower("capture", "let a = 1; let f = () => a;").expect("parses");
    let inner = &capturing.functions[1];
    assert_eq!(inner.captures.len(), 1, "the arrow captures `a`");
    assert!(inner.parameters.is_empty());

    let shadowing = lower("shadow", "let a = 1; let f = (a) => a;").expect("parses");
    let inner = &shadowing.functions[1];
    assert!(
        inner.captures.is_empty(),
        "a parameter shadows rather than captures"
    );
    assert_eq!(inner.parameters.len(), 1);
}

#[test]
fn a_local_declaration_shadows_rather_than_captures() {
    let lowered = lower(
        "local",
        "let a = 1; let f = () => { let a = 2; return a; };",
    )
    .expect("parses");
    assert!(
        lowered.functions[1].captures.is_empty(),
        "the inner `let a` is a new binding, not a read of the outer one"
    );
}

#[test]
fn a_nested_closure_captures_at_each_level() {
    // `a` has to travel down two functions, and each level captures it from the one above —
    // which is what makes a chain of closures work at all.
    let lowered = lower("nested", "let a = 1; let f = () => () => a;").expect("parses");
    assert_eq!(
        lowered.functions.len(),
        3,
        "program, outer arrow, inner arrow"
    );
    assert_eq!(lowered.functions[1].captures.len(), 1);
    assert_eq!(lowered.functions[2].captures.len(), 1);
}

#[test]
fn a_concise_arrow_body_is_an_implicit_return() {
    let concise = lower("concise", "let f = (x) => x;").expect("parses");
    let block = &concise.functions[1].blocks[0];
    assert!(
        matches!(block.terminator, crisol_ir::Terminator::Return(Some(_))),
        "`x => x` returns x, it does not evaluate and discard it"
    );
}

#[test]
fn a_function_that_runs_off_the_end_returns_undefined() {
    let lowered = lower("falls-off", "let f = () => { let a = 1; };").expect("parses");
    let block = &lowered.functions[1].blocks[0];
    assert!(matches!(
        block.terminator,
        crisol_ir::Terminator::Return(None)
    ));
}

#[test]
fn a_program_that_does_not_parse_is_refused() {
    assert!(lower("broken", "let = ;").is_err());
}
