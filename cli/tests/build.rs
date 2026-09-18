//! §M13's acceptance path: source in, native binary out, correct output.
//!
//! These build and **run** real programs. That is the only evidence that matters for a claim
//! phrased as "compiles to a standalone binary that runs and produces correct output" — a test
//! that checked the binary existed would pass for a binary that printed nothing.
//!
//! They skip when the runtime archive is absent rather than failing, because `cargo test`
//! builds test binaries before it builds the `staticlib` a compiled program links against.
//! `CRISOL_REQUIRE_BUILD=1` turns the absence into a failure, so CI cannot pass by finding
//! nothing — the same arrangement as `CRISOL_REQUIRE_GPU`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The runtime archive, if it has been built.
fn runtime() -> Option<PathBuf> {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
    for profile in ["debug", "release"] {
        let candidate = target.join(profile).join("libcrisol_abi.a");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    assert!(
        std::env::var("CRISOL_REQUIRE_BUILD").is_err(),
        "CRISOL_REQUIRE_BUILD is set and libcrisol_abi.a was not found. \
         Build it first: cargo build -p crisol-abi"
    );
    None
}

/// Compiles `source` and returns what the resulting binary printed.
fn build_and_run(name: &str, source: &str) -> Option<String> {
    let runtime = runtime()?;
    let directory = std::env::temp_dir().join(format!("crisol-acceptance-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a working directory");

    let file = directory.join("main.js");
    std::fs::write(&file, source).expect("write the source");
    let binary = directory.join("main");

    // The CLI is invoked through the library rather than as a subprocess: a subprocess would
    // need the `crisol` binary already built, which is the same ordering problem as the
    // archive and would make the test depend on how `cargo test` schedules its work.
    crisol::build::build(&file, &binary, &runtime)
        .unwrap_or_else(|error| panic!("{name} should build: {error}"));

    let output = Command::new(&binary).output().expect("run the binary");
    assert!(
        output.status.success(),
        "{name} exited with {:?}",
        output.status.code()
    );
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn check(name: &str, source: &str, expected: &str) {
    let Some(actual) = build_and_run(name, source) else {
        return;
    };
    assert_eq!(actual, expected, "{name}: {source}");
}

#[test]
fn arithmetic_compiles_links_and_runs() {
    check("subtract", "return 6 - 4;", "2");
    check("multiply", "return 6 * 4;", "24");
    check("divide", "return 6 / 4;", "1.5");
    check("negate", "return -5;", "-5");
}

#[test]
fn the_operators_that_call_the_runtime_link_against_it() {
    // These need position-independent code, because the object has to *call* out. Before
    // `is_pic` was set, a program using only `-` linked and one using `+` did not.
    check("add", "return 2 + 3;", "5");
    check("exponent", "return 2 ** 8;", "256");
}

#[test]
fn the_int32_wrap_survives_into_a_real_binary() {
    // The assertion the call-not-instruction decision exists for (D-87), made against a
    // compiled program rather than a JIT. A saturating conversion would print 2147483647.
    check("wrap", "return 1e10 | 0;", "1410065408");
    check("unsigned", "return -1 >>> 0;", "4294967295");
}

#[test]
fn remainder_keeps_the_dividends_sign_end_to_end() {
    check("remainder", "return -5 % 3;", "-2");
}

#[test]
fn control_flow_picks_the_right_branch() {
    // The other arm returns a number too, so branching wrongly gives a plausible answer.
    check("conditional", "let a = 1; return a < 2 ? 10 : 20;", "10");
    check("comparison", "return 1 < 2;", "true");
    check("not", "return !(1 < 2);", "false");
}

#[test]
fn locals_round_trip() {
    check("locals", "let a = 6; let b = 4; return a - b;", "2");
}

#[test]
fn a_program_with_no_return_prints_undefined() {
    check("empty", "let a = 1;", "undefined");
    check("void", "return void 0;", "undefined");
}

#[test]
fn a_construct_the_compiler_cannot_handle_is_refused_rather_than_miscompiled() {
    let Some(runtime) = runtime() else { return };
    let directory = std::env::temp_dir().join("crisol-acceptance-refused");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a working directory");
    let file = directory.join("main.js");
    // A `for` loop still lowers to a recorded gap rather than to nothing.
    std::fs::write(&file, "for (;;) { } return 1;").expect("write");

    let error =
        crisol::build::build(&file, &directory.join("main"), &runtime).expect_err("should refuse");
    // The stage matters: this is a gap in the compiler, not a mistake in the program, and the
    // two need different responses from whoever reads it.
    assert!(
        matches!(error, crisol::build::BuildError::Unsupported { .. }),
        "{error:?}"
    );
}

// ---- objects ---------------------------------------------------------------------------

#[test]
fn an_object_literal_allocates_and_its_property_reads_back() {
    check("object-property", "let o = {a: 7}; return o.a;", "7");
}

#[test]
fn a_property_added_after_allocation_reads_back() {
    // The path `Heap::alloc` alone could not express: the object is allocated empty and gains
    // a slot when the property is stored (D-92).
    check("object-grow", "let o = {}; o.a = 5; return o.a;", "5");
}

#[test]
fn several_properties_do_not_share_a_slot() {
    check(
        "object-several",
        "let o = {}; o.a = 1; o.b = 2; o.c = 3; return o.a + o.b + o.c;",
        "6",
    );
}

#[test]
fn a_missing_property_is_undefined() {
    check("object-missing", "let o = {a: 1}; return o.b;", "undefined");
}

#[test]
fn a_property_holding_an_object_can_be_reached_through_it() {
    check(
        "object-nested",
        "let inner = {v: 4}; let outer = {}; outer.i = inner; return outer.i.v;",
        "4",
    );
}
