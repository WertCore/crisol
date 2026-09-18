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
    // `for-of` still lowers to a recorded gap rather than to nothing. This case has to be
    // replaced whenever the construct it names becomes supported — which is the point: the
    // test is about *refusing*, so it must always name something actually refused.
    std::fs::write(&file, "for (let x of [1]) { } return 1;").expect("write");

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

// ---- hoisting and switch ----------------------------------------------------------------

/// **A function declaration is usable above its own text.** Every test262 case depends on it —
/// the suite's own `assert.js` defines helpers below the code that calls them.
#[test]
fn a_function_declaration_is_callable_before_it_appears() {
    check(
        "hoist-call-before",
        "let r = f(); function f() { return 7; } return r;",
        "7",
    );
}

#[test]
fn hoisting_works_inside_a_function_too() {
    check(
        "hoist-nested",
        "function outer() { let r = inner(); function inner() { return 3; } return r; } return outer();",
        "3",
    );
}

#[test]
fn a_switch_picks_the_matching_case() {
    check(
        "switch-match",
        "let x = 2; let r = 0; switch (x) { case 1: r = 10; break; case 2: r = 20; break; } return r;",
        "20",
    );
}

/// **Cases fall through without `break`**, which is what makes a switch more than nested ifs.
#[test]
fn a_case_without_break_falls_through() {
    check(
        "switch-fallthrough",
        "let r = 0; switch (1) { case 1: r = r + 1; case 2: r = r + 10; case 3: r = r + 100; } return r;",
        "111",
    );
}

#[test]
fn break_stops_the_fall_through() {
    check(
        "switch-break",
        "let r = 0; switch (1) { case 1: r = r + 1; break; case 2: r = r + 10; } return r;",
        "1",
    );
}

#[test]
fn default_runs_when_nothing_matches() {
    check(
        "switch-default",
        "let r = 0; switch (9) { case 1: r = 1; break; default: r = 5; } return r;",
        "5",
    );
}

/// **`default` is tested last but runs in its source position.** With a match it is skipped
/// entirely; without one, control enters it and then falls through into what follows.
#[test]
fn default_before_a_case_still_falls_through_into_it() {
    check(
        "switch-default-first",
        "let r = 0; switch (9) { default: r = r + 1; case 1: r = r + 10; } return r;",
        "11",
    );
    check(
        "switch-default-first-match",
        "let r = 0; switch (1) { default: r = r + 1; case 1: r = r + 10; } return r;",
        "10",
    );
}

/// The discriminant is evaluated once, so `switch (f())` does not call `f` per case.
#[test]
fn the_discriminant_is_evaluated_once() {
    check(
        "switch-once",
        "let calls = 0; let f = function () { calls = calls + 1; return 3; }; \
         switch (f()) { case 1: break; case 2: break; case 3: break; } return calls;",
        "1",
    );
}

// ---- exceptions -------------------------------------------------------------------------

#[test]
fn a_thrown_value_is_caught() {
    check(
        "throw-catch",
        "let r = 0; try { throw 5; } catch (e) { r = e; } return r;",
        "5",
    );
}

/// The point of propagation: a throw crosses a call boundary to reach the handler.
#[test]
fn a_throw_inside_a_call_reaches_the_callers_handler() {
    check(
        "throw-across-call",
        "let f = function () { throw 7; }; let r = 0; try { f(); } catch (e) { r = e; } return r;",
        "7",
    );
}

#[test]
fn a_throw_crosses_several_frames() {
    check(
        "throw-deep",
        "let inner = function () { throw 3; }; \
         let middle = function () { inner(); return 99; }; \
         let outer = function () { middle(); return 98; }; \
         let r = 0; try { outer(); } catch (e) { r = e; } return r;",
        "3",
    );
}

/// **Statements after a throwing call must not run.** A propagation that reached the handler
/// but also continued would give the right caught value and the wrong everything else.
#[test]
fn nothing_after_a_throwing_call_runs() {
    check(
        "throw-skips-rest",
        "let f = function () { throw 1; }; let r = 0; \
         try { f(); r = 100; } catch (e) { r = r + 10; } return r;",
        "10",
    );
}

#[test]
fn a_try_that_does_not_throw_skips_the_handler() {
    check(
        "try-no-throw",
        "let r = 0; try { r = 1; } catch (e) { r = 2; } return r;",
        "1",
    );
}

#[test]
fn a_caught_exception_can_be_an_object() {
    check(
        "throw-object",
        "let r = 0; try { throw {code: 4}; } catch (e) { r = e.code; } return r;",
        "4",
    );
}

/// The handler is the *innermost* one, and an outer `try` is unaffected.
#[test]
fn nested_handlers_catch_at_the_innermost() {
    check(
        "throw-nested",
        "let r = 0; \
         try { try { throw 1; } catch (e) { r = r + 1; } r = r + 10; } catch (e) { r = r + 100; } \
         return r;",
        "11",
    );
}

#[test]
fn a_throw_from_a_catch_reaches_the_outer_handler() {
    check(
        "throw-rethrow",
        "let r = 0; \
         try { try { throw 1; } catch (e) { throw 2; } } catch (e) { r = e; } \
         return r;",
        "2",
    );
}

// ---- loops ------------------------------------------------------------------------------

#[test]
fn a_for_loop_runs_its_body_and_updates() {
    check(
        "for-sum",
        "let t = 0; for (let i = 0; i < 4; i = i + 1) { t = t + i; } return t;",
        "6",
    );
}

/// **`continue` goes to the update, not the test.** Sharing a block for them makes this hang
/// rather than answer wrongly, and only when a `continue` is present.
#[test]
fn continue_still_runs_the_update() {
    check(
        "for-continue",
        "let t = 0; for (let i = 0; i < 4; i = i + 1) { if (i === 2) { continue; } t = t + i; } return t;",
        "4",
    );
}

#[test]
fn break_leaves_a_loop() {
    check(
        "for-break",
        "let t = 0; for (let i = 0; i < 10; i = i + 1) { if (i === 3) { break; } t = t + 1; } return t;",
        "3",
    );
}

#[test]
fn a_while_loop_can_break_and_continue() {
    check(
        "while-break",
        "let i = 0; let t = 0; while (true) { i = i + 1; if (i > 5) { break; } t = t + i; } return t;",
        "15",
    );
}

/// `do … while` runs its body before testing anything, which is the whole difference.
#[test]
fn a_do_while_runs_once_even_when_the_test_is_false() {
    check(
        "do-while-once",
        "let t = 0; do { t = t + 1; } while (false); return t;",
        "1",
    );
}

#[test]
fn a_loop_can_throw_out_of_itself() {
    check(
        "loop-throw",
        "let r = 0; try { for (let i = 0; i < 10; i = i + 1) { if (i === 2) { throw i; } } } \
         catch (e) { r = e; } return r;",
        "2",
    );
}

/// `instanceof` walks the prototype chain for the constructor's `prototype`.
#[test]
fn instanceof_recognises_an_instance_of_its_class() {
    check(
        "instanceof-true",
        "class C { constructor() {} } let c = new C(); return c instanceof C;",
        "true",
    );
}

#[test]
fn instanceof_rejects_an_unrelated_object_and_a_primitive() {
    check(
        "instanceof-other",
        "class C { constructor() {} } class D { constructor() {} } \
         let d = new D(); return d instanceof C;",
        "false",
    );
    // `1 instanceof C` is `false`, not an error — a primitive has no chain to walk.
    check(
        "instanceof-primitive",
        "class C { constructor() {} } return 1 instanceof C;",
        "false",
    );
}

// ---- strings ----------------------------------------------------------------------------

#[test]
fn a_string_literal_prints() {
    check("string-literal", "return \"hello\";", "hello");
}

/// **Strings compare by their characters, not by identity.** Each literal allocates a fresh
/// cell today, so an identity comparison would make this false.
#[test]
fn strings_compare_by_value() {
    check("string-eq", "return \"a\" === \"a\";", "true");
    check("string-ne", "return \"a\" === \"b\";", "false");
    check(
        "string-eq-built",
        "let a = \"foo\"; let b = \"fo\" + \"o\"; return a === b;",
        "true",
    );
}

/// **`+` concatenates when either operand is a string**, and adds otherwise — `1 + "2"` is
/// `"12"`, not `3`.
#[test]
fn plus_concatenates_with_a_string_operand() {
    check("string-concat", "return \"a\" + \"b\";", "ab");
    check("string-number-right", "return \"n=\" + 2;", "n=2");
    check("string-number-left", "return 1 + \"2\";", "12");
    check("number-plus-number", "return 1 + 2;", "3");
}

#[test]
fn a_string_has_a_length() {
    check("string-length", "return \"hello\".length;", "5");
    check("string-length-empty", "return \"\".length;", "0");
}

#[test]
fn a_string_can_be_a_property_value_and_an_element() {
    check("string-in-object", "let o = {s: \"hi\"}; return o.s;", "hi");
    check(
        "string-in-array",
        "let a = [\"x\", \"y\"]; return a[1];",
        "y",
    );
}

/// Strings allocate, so they are collected like anything else.
#[test]
fn strings_survive_a_collection() {
    check(
        "string-stress",
        "let a = \"one\"; let b = \"two\"; let c = a + b; let o = {v: c}; return o.v;",
        "onetwo",
    );
}

// ---- unary operators on values of unknown type ------------------------------------------

#[test]
fn negation_coerces_before_negating() {
    check("unary-negate", "let x = 3; return -x;", "-3");
    check("unary-negate-string", "let s = \"4\"; return -s;", "-4");
}

#[test]
fn unary_plus_is_to_number() {
    check("unary-plus", "let s = \"5\"; return +s;", "5");
}

#[test]
fn not_applies_to_boolean_conversion() {
    check("unary-not-zero", "let x = 0; return !x;", "true");
    check("unary-not-object", "let o = {}; return !o;", "false");
}

/// **`typeof null` is `"object"`** — a bug old enough to be part of the language — and a
/// function reports `"function"` although it is an object, so neither can be read off the
/// value's kind alone.
#[test]
fn typeof_reports_the_specified_names() {
    check("typeof-number", "return typeof 1;", "number");
    check("typeof-string", "return typeof \"a\";", "string");
    check("typeof-boolean", "return typeof true;", "boolean");
    check("typeof-undefined", "return typeof undefined;", "undefined");
    check("typeof-null", "return typeof null;", "object");
    check("typeof-object", "return typeof {};", "object");
    check(
        "typeof-function",
        "let f = function () {}; return typeof f;",
        "function",
    );
}

/// The three things a bit comparison gets wrong, on values the lattice knows nothing about.
#[test]
fn strict_equality_on_unknown_values_follows_the_specification() {
    // `NaN === NaN` is false, and two NaNs have identical bits.
    check(
        "eq-nan",
        "let a = 0 / 0; let b = 0 / 0; return a === b;",
        "false",
    );
    // `+0 === -0` is true, and their bits differ.
    check(
        "eq-zeroes",
        "let a = 0; let b = -0; return a === b;",
        "true",
    );
    // Different types are never equal, whatever the payloads.
    check(
        "eq-types",
        "let a = 0; let b = \"0\"; return a === b;",
        "false",
    );
}

// ---- truthiness -------------------------------------------------------------------------

/// **A branch is not a bit comparison against `true`.** Every truthy value that is not
/// literally `true` would take the false path, so these all tested backwards.
#[test]
fn a_branch_on_a_non_boolean_follows_to_boolean() {
    check("truthy-string", "if (\"a\") { return 1; } return 2;", "1");
    check(
        "truthy-empty-string",
        "if (\"\") { return 1; } return 2;",
        "2",
    );
    check("truthy-number", "if (3) { return 1; } return 2;", "1");
    check("truthy-zero", "if (0) { return 1; } return 2;", "2");
    check("truthy-object", "if ({}) { return 1; } return 2;", "1");
    check(
        "truthy-undefined",
        "if (undefined) { return 1; } return 2;",
        "2",
    );
    check("truthy-null", "if (null) { return 1; } return 2;", "2");
}

/// `||` and `&&` are branches too, and this is the shape that made every test262 error lose
/// its message: `this.message = message || ""` assigned `""` whatever it was given.
#[test]
fn or_returns_the_first_truthy_operand() {
    check("or-string", "let m = \"boom\"; return m || \"\";", "boom");
    check(
        "or-empty",
        "let m = \"\"; return m || \"fallback\";",
        "fallback",
    );
    check(
        "or-undefined",
        "let m = undefined; return m || \"fallback\";",
        "fallback",
    );
    check(
        "and-string",
        "let m = \"boom\"; return m && \"second\";",
        "second",
    );
    check("and-empty", "let m = \"\"; return m && \"second\";", "");
}

#[test]
fn a_loop_condition_is_also_to_boolean() {
    check(
        "truthy-while",
        "let n = 3; let t = 0; while (n) { t = t + n; n = n - 1; } return t;",
        "6",
    );
}

/// **Every function has a `prototype` object**, not only a class. `new f()` links an instance
/// to it and `instanceof` looks for it, so a function without one answers `false` for an object
/// its own constructor just made.
#[test]
fn a_plain_function_is_a_constructor_too() {
    check(
        "function-prototype",
        "function E(m) { this.message = m; } let e = new E(\"boom\"); return e instanceof E;",
        "true",
    );
    check(
        "function-prototype-field",
        "function E(m) { this.message = m; } return new E(\"boom\").message;",
        "boom",
    );
    // The guard test262's own error class uses, which recursed forever without a prototype.
    check(
        "function-instanceof-guard",
        "function E(m) { if (!(this instanceof E)) { return new E(m); } this.message = m || \"\"; } \
         return new E(\"x\").message;",
        "x",
    );
}

// ---- globals ----------------------------------------------------------------------------

/// **A name that resolves to no binding is a global, not a fresh local.** Reading it as a local
/// is what made every builtin compare equal to `undefined`.
#[test]
fn a_missing_global_is_a_reference_error() {
    check(
        "global-missing",
        "let r = 0; try { nosuchthing; } catch (e) { r = e.name; } return r;",
        "ReferenceError",
    );
    check(
        "global-missing-message",
        "let r = 0; try { nosuchthing; } catch (e) { r = e.message; } return r;",
        "nosuchthing is not defined",
    );
}

#[test]
fn the_error_constructors_exist_and_carry_their_name() {
    check(
        "global-typeerror",
        "return new TypeError(\"x\").name;",
        "TypeError",
    );
    check(
        "global-typeerror-message",
        "return new TypeError(\"x\").message;",
        "x",
    );
    check(
        "global-rangeerror",
        "return new RangeError(\"y\").name;",
        "RangeError",
    );
    // Each is the same code with a different binding, so they must not share a name.
    check(
        "global-distinct",
        "return new TypeError(\"a\").name === new RangeError(\"b\").name;",
        "false",
    );
}

/// A thrown error is caught and read like any other object.
#[test]
fn a_constructed_error_can_be_thrown_and_caught() {
    check(
        "global-throw-error",
        "let r = 0; try { throw new TypeError(\"bad\"); } catch (e) { r = e.message; } return r;",
        "bad",
    );
}

#[test]
fn the_conversion_globals_work() {
    check("global-string", "return String(12);", "12");
    check("global-number", "return Number(\"7\");", "7");
    check("global-number-empty", "return Number();", "0");
    check("global-boolean", "return Boolean(\"\");", "false");
}

#[test]
fn global_this_and_the_value_globals_resolve() {
    check("global-undefined", "return undefined;", "undefined");
    check("global-nan", "return NaN;", "NaN");
    check("global-infinity", "return Infinity;", "Infinity");
    check("global-this-exists", "return typeof globalThis;", "object");
}
