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
use crisol_ir::verify;

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
        if let Err(errors) = verify(&lowered.function) {
            panic!(
                "{name} produced a malformed graph: {errors:?}\n{}",
                lowered.function
            );
        }

        out.push_str(&format!("=== {name} ===\n"));
        out.push_str("--- source\n");
        out.push_str(source);
        if !source.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("--- ir\n");
        out.push_str(&lowered.function.to_string());
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
        assert_eq!(
            verify(&lowered.function),
            Ok(()),
            "{name}\n{}",
            lowered.function
        );
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
        ("let a = 1 + 2;", "binary operator +"),
        ("for (;;) { }", "for statement"),
        ("function f() { }", "function declaration"),
        ("let a = [1];", "array literal"),
        ("let a = () => 1;", "arrow function"),
        ("let o = {}; let a = o[1];", "computed member access"),
        ("let a = !true;", "unary expression"),
        ("let a = true && false;", "logical expression"),
        ("let a = true ? 1 : 2;", "conditional expression"),
        ("try { } catch (e) { }", "try statement"),
        ("let [a] = [1];", "destructuring declaration"),
        ("let o = { ...{} };", "object spread"),
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
    let dump = lowered.function.to_string();
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
fn a_program_that_does_not_parse_is_refused() {
    assert!(lower("broken", "let = ;").is_err());
}
