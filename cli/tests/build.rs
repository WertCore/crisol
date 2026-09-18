//! §M13's acceptance path: source in, native binary out, correct output.
//!
//! These build and **run** real programs. That is the only evidence that matters for a claim
//! phrased as "compiles to a standalone binary that runs and produces correct output" — a test
//! that checked the binary existed would pass for a binary that printed nothing.
//!
//! **These link a prebuilt `libcrisol_abi.a`, which `cargo test -p crisol` does not rebuild.**
//! Changing the runtime and re-running the tests therefore proves nothing until
//! `cargo build -p crisol-abi` has run — a mutation to the runtime silently keeps passing
//! otherwise, which is the shape of false negative this file is least able to notice.
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

    Some(execute(name, &binary, false))
}

/// Runs a built program, optionally collecting on every allocation.
fn execute(name: &str, binary: &Path, stress: bool) -> String {
    let mut command = Command::new(binary);
    if stress {
        command.env("CRISOL_GC_STRESS", "1");
    }
    let output = command.output().expect("run the binary");
    assert!(
        output.status.success(),
        "{name} exited with {:?}{}",
        output.status.code(),
        if stress { " under GC stress" } else { "" }
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Builds `source`, runs it **twice**, and requires the same answer both times.
///
/// The second run collects on every allocation. That is not extra caution — it is the only
/// thing that tests rooting at all, and it has now caught two bugs that every ordinary run
/// passed: stack map offsets read from the wrong end of the frame (D-93), and temporaries that
/// never reach a slot being invisible to the collector (D-99).
///
/// Running *every* program both ways rather than writing separate stress tests, because the
/// bugs it finds are not in the programs that look like they exercise the collector. `[{v: 1}]`
/// does not look like a GC test.
fn check(name: &str, source: &str, expected: &str) {
    let Some(runtime) = runtime() else {
        return;
    };
    let directory = std::env::temp_dir().join(format!("crisol-acceptance-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a working directory");

    let file = directory.join("main.js");
    std::fs::write(&file, source).expect("write the source");
    let binary = directory.join("main");
    crisol::build::build(&file, &binary, &runtime)
        .unwrap_or_else(|error| panic!("{name} should build: {error}"));

    assert_eq!(execute(name, &binary, false), expected, "{name}: {source}");
    assert_eq!(
        execute(name, &binary, true),
        expected,
        "{name} under GC stress: {source}"
    );
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

/// §M13 asks for a GC stress mode. This is what it is for.
///
/// `CRISOL_GC_STRESS` collects on **every** allocation, so an object that the collector cannot
/// see is freed before the next line rather than surviving until memory runs low. Each `{...}`
/// below is an allocation, so by the last one every earlier object has been through several
/// collections while live only in a compiled frame.
///
/// If the stack map table were empty, or registered too late, or read at the wrong frame
/// offset, this prints garbage or crashes. Without stress mode it would pass either way, which
/// is exactly why the mode exists.
#[test]
fn objects_survive_a_collection_at_every_allocation() {
    let source = "let inner = {v: 4}; let outer = {}; outer.i = inner; \
                  let a = {x: 1}; let b = {y: 2}; return outer.i.v + a.x + b.y;";
    let Some(relaxed) = build_and_run("gc-stress-off", source) else {
        return;
    };
    assert_eq!(relaxed, "7", "the answer without stress mode");

    let Some(runtime) = runtime() else { return };
    let directory = std::env::temp_dir().join("crisol-acceptance-gc-stress-on");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a working directory");
    let file = directory.join("main.js");
    std::fs::write(&file, source).expect("write the source");
    let binary = directory.join("main");
    crisol::build::build(&file, &binary, &runtime).expect("it should build");

    let output = Command::new(&binary)
        .env("CRISOL_GC_STRESS", "1")
        .output()
        .expect("run the binary");
    assert!(output.status.success(), "it must not crash under stress");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "7",
        "an object held only by a compiled frame must survive collection"
    );
}

#[test]
fn this_at_the_top_level_is_undefined() {
    // A module's `this` is `undefined`, not the global object. The entry point passes it
    // explicitly, so this checks the value actually arrives rather than defaulting.
    check("this-toplevel", "return this;", "undefined");
}

// ---- closures and calls ----------------------------------------------------------------

#[test]
fn a_function_can_be_called() {
    check(
        "call-simple",
        "let f = function (a, b) { return a + b; }; return f(2, 3);",
        "5",
    );
}

#[test]
fn a_closure_reads_what_it_captured() {
    // The capture path end to end: `n` lives in the enclosing frame, is copied into the
    // closure at creation, and is read back out through the callee's prologue.
    check(
        "call-capture",
        "let n = 10; let add = function (x) { return x + n; }; return add(5);",
        "15",
    );
}

#[test]
fn a_missing_argument_is_undefined_not_an_error() {
    // The guarded load in the prologue. `b` was never passed, so `a + b` is `NaN` — which is
    // the specification's answer, and is what distinguishes it from reading stack garbage.
    check(
        "call-missing-arg",
        "let f = function (a, b) { return a + b; }; return f(1);",
        "NaN",
    );
}

#[test]
fn extra_arguments_are_ignored() {
    check(
        "call-extra-args",
        "let f = function (a) { return a; }; return f(7, 8, 9);",
        "7",
    );
}

#[test]
fn calling_something_that_is_not_a_function_does_not_crash() {
    // `5()` is a TypeError, which needs a throw path M13 does not have. What must not happen
    // is a jump through a null pointer, so the runtime hands back a real fallback instead.
    check("call-non-function", "let x = 5; return x();", "undefined");
}

#[test]
fn a_callback_passed_as_a_value_is_reached_indirectly() {
    // The case the whole convention exists for: `apply` has no idea which function it holds.
    check(
        "call-callback",
        "let twice = function (f, v) { return f(f(v)); }; \
         let inc = function (x) { return x + 1; }; return twice(inc, 5);",
        "7",
    );
}

/// Closures allocate, so every one of them is a collection under stress — and a closure is
/// reachable only from a compiled frame and from its own captures. If the captures were not
/// traced, or the closure itself were not rooted, this returns garbage or crashes.
#[test]
fn closures_and_captures_survive_a_collection_at_every_allocation() {
    let source = "let n = 10; \
                  let add = function (x) { return x + n; }; \
                  let twice = function (f, v) { return f(f(v)); }; \
                  return twice(add, 1);";
    let Some(relaxed) = build_and_run("closure-stress-off", source) else {
        return;
    };
    assert_eq!(relaxed, "21");

    let Some(runtime) = runtime() else { return };
    let directory = std::env::temp_dir().join("crisol-acceptance-closure-stress-on");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a working directory");
    let file = directory.join("main.js");
    std::fs::write(&file, source).expect("write the source");
    let binary = directory.join("main");
    crisol::build::build(&file, &binary, &runtime).expect("it should build");

    let output = Command::new(&binary)
        .env("CRISOL_GC_STRESS", "1")
        .output()
        .expect("run the binary");
    assert!(output.status.success(), "it must not crash under stress");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "21",
        "a captured value must survive collection"
    );
}

// ---- classes ----------------------------------------------------------------------------

#[test]
fn a_class_constructs_and_its_field_reads_back() {
    check(
        "class-field",
        "class Point { constructor(x) { this.x = x; } } let p = new Point(4); return p.x;",
        "4",
    );
}

/// Methods live on one shared prototype, not on each instance, so this only works if the
/// property lookup walks the chain.
#[test]
fn a_method_is_found_through_the_prototype() {
    check(
        "class-method",
        "class Box { constructor(v) { this.v = v; } get() { return this.v; } } \
         let b = new Box(9); return b.get();",
        "9",
    );
}

/// `this` inside a method is the receiver. Losing it is silent — the call still returns
/// something, it is only `this` that is wrong.
#[test]
fn this_inside_a_method_is_the_receiver() {
    check(
        "class-this",
        "class Sum { constructor(a, b) { this.a = a; this.b = b; } total() { return this.a + this.b; } } \
         let s = new Sum(2, 3); return s.total();",
        "5",
    );
}

/// **A constructor returning an object replaces `this`; one returning a primitive does not.**
/// Both halves, because a lowering that ignored the rule passes the second test.
#[test]
fn a_constructor_returning_a_primitive_still_yields_the_instance() {
    check(
        "class-return-primitive",
        "class C { constructor() { this.x = 1; return 42; } } return new C().x;",
        "1",
    );
}

#[test]
fn a_constructor_returning_an_object_replaces_the_instance() {
    check(
        "class-return-object",
        "class C { constructor() { this.x = 1; return {x: 7}; } } return new C().x;",
        "7",
    );
}

#[test]
fn two_instances_share_a_prototype_but_not_their_fields() {
    check(
        "class-two-instances",
        "class P { constructor(n) { this.n = n; } get() { return this.n; } } \
         let a = new P(1); let b = new P(2); return a.get() + b.get();",
        "3",
    );
}

/// Storing a property on a function must not break calling it.
///
/// A closure keeps its function index and captures as engine-private state. They used to live
/// in the property slots, and a shape numbers properties from zero — so the first property
/// stored on a function overwrote the index and the function silently stopped being callable.
/// `class C {}` does exactly that to its own constructor, via `prototype`.
#[test]
fn a_property_on_a_function_does_not_break_calling_it() {
    check(
        "function-property",
        "let f = function () { return 1; }; f.x = 5; return f();",
        "1",
    );
    check(
        "function-property-read",
        "let f = function () { return 1; }; f.x = 5; return f.x;",
        "5",
    );
}

// ---- captured variables are shared, not copied (D-97) ----------------------------------

/// JavaScript captures the **binding**, not the value. A closure that copied what it captured
/// would pass every read-only test and give a plausible wrong answer the moment anything wrote.
#[test]
fn a_write_inside_a_closure_is_seen_outside_it() {
    check(
        "capture-write",
        "let n = 0; let f = function () { n = 1; }; f(); return n;",
        "1",
    );
}

#[test]
fn a_write_outside_a_closure_is_seen_inside_it() {
    // The other direction, and the one a snapshot-at-creation implementation gets wrong even
    // if writes from inside somehow worked.
    check(
        "capture-read-after",
        "let n = 1; let f = function () { return n; }; n = 2; return f();",
        "2",
    );
}

#[test]
fn a_counter_in_a_closure_accumulates() {
    check(
        "capture-counter",
        "let total = 0; let add = function (x) { total = total + x; }; \
         add(1); add(2); add(3); return total;",
        "6",
    );
}

#[test]
fn two_closures_over_one_variable_see_each_other() {
    check(
        "capture-shared",
        "let n = 0; let set = function (v) { n = v; }; let get = function () { return n; }; \
         set(7); return get();",
        "7",
    );
}

/// A shared *parameter* has no cell to arrive in — the caller passes a plain value — so the
/// callee wraps it at entry. Without that, this reads the unwrapped argument as a cell.
#[test]
fn a_captured_parameter_is_shared_too() {
    check(
        "capture-parameter",
        "let outer = function (n) { let bump = function () { n = n + 1; }; bump(); return n; }; \
         return outer(5);",
        "6",
    );
}

/// A variable nobody assigns must stay a plain value, or every closure pays for a cell.
#[test]
fn a_captured_but_never_assigned_variable_still_reads_correctly() {
    check(
        "capture-readonly",
        "let n = 10; let f = function (x) { return x + n; }; return f(5);",
        "15",
    );
}

// ---- arrays -----------------------------------------------------------------------------

#[test]
fn an_array_literal_indexes_and_reports_its_length() {
    check("array-index", "let a = [10, 20, 30]; return a[1];", "20");
    check(
        "array-length",
        "let a = [10, 20, 30]; return a.length;",
        "3",
    );
    check("array-empty", "let a = []; return a.length;", "0");
}

#[test]
fn an_element_can_be_written() {
    check(
        "array-write",
        "let a = [1, 2, 3]; a[0] = 9; return a[0];",
        "9",
    );
}

/// `a[5] = 1` on a shorter array grows it, and the gap reads as `undefined`.
#[test]
fn writing_past_the_end_grows_the_array() {
    check("array-grow", "let a = [1]; a[3] = 7; return a.length;", "4");
    check(
        "array-gap",
        "let a = [1]; a[3] = 7; return a[2];",
        "undefined",
    );
}

#[test]
fn an_index_past_the_end_is_undefined() {
    check("array-oob", "let a = [1, 2]; return a[9];", "undefined");
}

/// A computed key that is not an index is an ordinary property — `a["x"]` is not an element,
/// and neither is `a[1.5]`.
#[test]
fn a_non_index_key_is_a_property_not_an_element() {
    check(
        "array-non-index",
        "let a = [1, 2]; a[1.5] = 8; return a.length;",
        "2",
    );
    check(
        "array-non-index-read",
        "let a = [1, 2]; return a[1.5];",
        "undefined",
    );
}

/// Element access on an object is property access: `o[0]` and `o["0"]` name the same thing.
#[test]
fn a_computed_key_on_an_object_is_a_property() {
    check("object-computed", "let o = {}; o[0] = 5; return o[0];", "5");
}

#[test]
fn an_element_can_hold_an_object_and_be_reached_through_it() {
    check(
        "array-of-objects",
        "let a = [{v: 1}, {v: 2}]; return a[0].v + a[1].v;",
        "3",
    );
}

/// Arrays allocate, and every element is a reference the collector must trace.
#[test]
fn arrays_survive_a_collection_at_every_allocation() {
    let source = "let a = [{v: 1}, {v: 2}, {v: 3}]; \
                  let total = 0; \
                  total = total + a[0].v; total = total + a[1].v; total = total + a[2].v; \
                  return total;";
    let Some(relaxed) = build_and_run("array-stress-off", source) else {
        return;
    };
    assert_eq!(relaxed, "6");

    let Some(runtime) = runtime() else { return };
    let directory = std::env::temp_dir().join("crisol-acceptance-array-stress-on");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a working directory");
    let file = directory.join("main.js");
    std::fs::write(&file, source).expect("write the source");
    let binary = directory.join("main");
    crisol::build::build(&file, &binary, &runtime).expect("it should build");

    let output = Command::new(&binary)
        .env("CRISOL_GC_STRESS", "1")
        .output()
        .expect("run the binary");
    assert!(output.status.success(), "it must not crash under stress");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "6");
}

// ---- array methods (§M13's last acceptance item) ----------------------------------------

#[test]
fn map_builds_a_new_array_from_a_callback() {
    check(
        "array-map",
        "let a = [1, 2, 3]; let b = a.map(function (x) { return x * 2; }); return b[1];",
        "4",
    );
    check(
        "array-map-length",
        "let a = [1, 2, 3]; return a.map(function (x) { return x; }).length;",
        "3",
    );
}

/// The callback gets `(element, index, array)`. Code that passes a method as a callback
/// depends on the extra arguments arriving.
#[test]
fn a_callback_receives_the_index() {
    check(
        "array-map-index",
        "let a = [10, 20, 30]; let b = a.map(function (x, i) { return i; }); return b[2];",
        "2",
    );
}

#[test]
fn filter_keeps_what_the_callback_accepts() {
    check(
        "array-filter",
        "let a = [1, 2, 3, 4]; let b = a.filter(function (x) { return x > 2; }); return b.length;",
        "2",
    );
    check(
        "array-filter-values",
        "let a = [1, 2, 3, 4]; let b = a.filter(function (x) { return x > 2; }); return b[0];",
        "3",
    );
}

#[test]
fn for_each_runs_for_its_effects() {
    check(
        "array-foreach",
        "let total = 0; [1, 2, 3].forEach(function (x) { total = total + x; }); return total;",
        "6",
    );
}

/// **Without an initial value the first element is the seed**, not `undefined` — otherwise
/// `[1, 2].reduce(add)` is `NaN` rather than `3`.
#[test]
fn reduce_seeds_from_the_first_element_when_given_no_initial_value() {
    check(
        "array-reduce",
        "return [1, 2, 3].reduce(function (a, b) { return a + b; });",
        "6",
    );
    check(
        "array-reduce-seed",
        "return [1, 2, 3].reduce(function (a, b) { return a + b; }, 10);",
        "16",
    );
}

#[test]
fn push_appends_and_answers_the_new_length() {
    check("array-push", "let a = [1]; a.push(2); return a[1];", "2");
    check(
        "array-push-length",
        "let a = [1]; return a.push(2, 3);",
        "3",
    );
}

#[test]
fn index_of_finds_an_element_or_reports_minus_one() {
    check("array-indexof", "return [5, 6, 7].indexOf(6);", "1");
    check(
        "array-indexof-missing",
        "return [5, 6, 7].indexOf(9);",
        "-1",
    );
}

/// The chained case, which is what "array methods" means in practice — and every intermediate
/// array is a temporary nothing else holds.
#[test]
fn methods_chain() {
    check(
        "array-chain",
        "return [1, 2, 3, 4] \
           .filter(function (x) { return x > 1; }) \
           .map(function (x) { return x * 10; }) \
           .reduce(function (a, b) { return a + b; });",
        "90",
    );
}
