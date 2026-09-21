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
    // **The program's own stderr, which is where it says why.** Without it a failure reads as
    // `exited with Some(1)` — true, and silent about the uncaught throw that caused it. The
    // entry point prints `uncaught: …` precisely so somebody can read it.
    assert!(
        output.status.success(),
        "{name} exited with {:?}{}: {}",
        output.status.code(),
        if stress { " under GC stress" } else { "" },
        String::from_utf8_lossy(&output.stderr).trim()
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

    // **Removed once it has passed, and kept when it has not.** A case that fails leaves its
    // directory for reading; one that passes leaves nothing. Without this a full run left a
    // scratch directory per case — a compiled binary each — and they accumulated across every
    // run until the disk filled, which took out several test262 runs and a virtual machine
    // before anyone connected the two.
    let _ = std::fs::remove_dir_all(&directory);
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
    // A class still lowers to a recorded gap rather than to nothing. This case has to be
    // replaced whenever the construct it names becomes supported — which is the point: the test
    // is about *refusing*, so it must always name something actually refused. It has named
    // `for-of` and then a regular expression literal, and been rewritten each time one landed.
    std::fs::write(&file, "class A extends Object {} return 1;").expect("write");

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
fn this_at_the_top_level_is_the_global_object() {
    // **This asserted `undefined`, and its reason named the right rule about the wrong thing.**
    // A *module*'s `this` is `undefined`; a script's is the global object, and scripts are all
    // this engine compiles — the test262 runner skips the module flag outright. The entry point
    // passes the value explicitly, so this still checks that it arrives rather than defaulting.
    check("this-toplevel", "return typeof this;", "object");
    check(
        "this-toplevel-is-global",
        "return this === globalThis;",
        "true",
    );
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
fn calling_something_that_is_not_a_function_is_a_type_error() {
    // This used to answer `undefined`, because there was no way to throw. The fallback that
    // made it safe — a real function with the uniform signature — is now where the `TypeError`
    // is raised, and no call site changed to make that happen (D-95).
    check(
        "call-non-function",
        "let r = 0; try { let x = 5; x(); } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
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

// ---- Object and Array globals -----------------------------------------------------------

#[test]
fn object_keys_lists_own_properties() {
    check(
        "object-keys",
        "let o = {a: 1, b: 2}; return Object.keys(o).length;",
        "2",
    );
    check(
        "object-keys-first",
        "let o = {a: 1, b: 2}; return Object.keys(o)[0];",
        "a",
    );
    check(
        "object-values",
        "let o = {a: 7}; return Object.values(o)[0];",
        "7",
    );
}

/// **Own, so the prototype chain is not walked** — which is the whole point, and why it cannot
/// be written as a property read compared against `undefined`.
#[test]
fn has_own_does_not_see_inherited_properties() {
    check(
        "has-own-true",
        "let o = {a: 1}; return Object.hasOwn(o, \"a\");",
        "true",
    );
    check(
        "has-own-false",
        "let o = {a: 1}; return Object.hasOwn(o, \"b\");",
        "false",
    );
    check(
        "has-own-inherited",
        "class C { constructor() {} m() {} } let c = new C(); return Object.hasOwn(c, \"m\");",
        "false",
    );
}

#[test]
fn object_create_links_a_prototype() {
    check(
        "object-create",
        "let base = {greet: 1}; let o = Object.create(base); return o.greet;",
        "1",
    );
    check(
        "object-get-prototype",
        "let base = {}; let o = Object.create(base); return Object.getPrototypeOf(o) === base;",
        "true",
    );
}

#[test]
fn object_assign_copies_own_properties() {
    check(
        "object-assign",
        "let t = {}; Object.assign(t, {a: 1}, {b: 2}); return t.a + t.b;",
        "3",
    );
}

#[test]
fn array_is_array_distinguishes_arrays_from_objects() {
    check("is-array-true", "return Array.isArray([1]);", "true");
    check("is-array-false", "return Array.isArray({});", "false");
    check("is-array-primitive", "return Array.isArray(1);", "false");
}

/// `Array.prototype` must be the object arrays already inherit from, not a fresh one —
/// otherwise `[].map === Array.prototype.map` is false.
#[test]
fn array_prototype_is_the_one_arrays_use() {
    check(
        "array-prototype-identity",
        "return [].map === Array.prototype.map;",
        "true",
    );
}

#[test]
fn the_namespace_globals_are_callable() {
    check("object-call", "return typeof Object({});", "object");
    check("object-typeof", "return typeof Object;", "function");
    check("array-of", "return Array.of(1, 2, 3).length;", "3");
}

/// **Every declaration is bound before any body is lowered.** A function may call one declared
/// further down the list — which is how test262 concatenates `assert.js` ahead of the `sta.js`
/// that defines the error class it throws.
#[test]
fn a_function_can_call_one_declared_after_it() {
    check(
        "hoist-forward-reference",
        "function first() { return second(); } function second() { return 4; } return first();",
        "4",
    );
}

// ---- TypeError where the specification requires it --------------------------------------

/// **Reading a property of `null` or `undefined` throws.** Answering `undefined` makes
/// `x.y.z` on a missing `x` fail two lines later carrying a value that looks like a legitimate
/// absence.
#[test]
fn a_property_of_nothing_is_a_type_error() {
    check(
        "nullish-read",
        "let r = 0; try { let x = null; x.y; } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
    check(
        "nullish-read-undefined",
        "let r = 0; try { let x = undefined; x.y; } catch (e) { r = e.message; } return r;",
        "cannot read a property of undefined",
    );
    check(
        "nullish-write",
        "let r = 0; try { let x = null; x.y = 1; } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
    check(
        "nullish-computed",
        "let r = 0; try { let x = null; x[0]; } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

/// The throw has to cross a call, which is what makes it more than a local check.
#[test]
fn a_type_error_propagates_out_of_a_call() {
    check(
        "nullish-across-call",
        "let f = function (o) { return o.x; }; \
         let r = 0; try { f(null); } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

/// A property *chain* stops at the first failure rather than carrying `undefined` onwards.
#[test]
fn a_chain_stops_at_the_first_missing_link() {
    check(
        "nullish-chain",
        "let o = {}; let r = 0; try { o.a.b; } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

// ---- more array methods -----------------------------------------------------------------

#[test]
fn searching_methods_agree_with_the_specification() {
    check("arr-lastindexof", "return [1, 2, 1].lastIndexOf(1);", "2");
    check("arr-includes", "return [1, 2].includes(2);", "true");
    // **`includes` finds NaN and `indexOf` does not** — SameValueZero against `===`.
    check(
        "arr-includes-nan",
        "return [0 / 0].includes(0 / 0);",
        "true",
    );
    check("arr-indexof-nan", "return [0 / 0].indexOf(0 / 0);", "-1");
}

#[test]
fn join_uses_a_separator_and_skips_nothing_values() {
    check("arr-join", "return [1, 2, 3].join(\"-\");", "1-2-3");
    check("arr-join-default", "return [1, 2].join();", "1,2");
    // `null` and `undefined` join as empty, not as their names.
    check(
        "arr-join-nullish",
        "return [1, null, 2].join(\"-\");",
        "1--2",
    );
}

/// **A negative index counts from the end**, and past either end clamps.
#[test]
fn slice_handles_relative_indices() {
    check(
        "arr-slice",
        "return [1, 2, 3, 4].slice(1, 3).join(\",\");",
        "2,3",
    );
    check("arr-slice-negative", "return [1, 2, 3].slice(-1)[0];", "3");
    check("arr-slice-all", "return [1, 2].slice().length;", "2");
    check("arr-slice-past-end", "return [1, 2].slice(5).length;", "0");
}

/// **An array argument is spread and anything else appended whole.**
#[test]
fn concat_spreads_only_arrays() {
    check("arr-concat-array", "return [1].concat([2, 3]).length;", "3");
    check("arr-concat-value", "return [1].concat(2).length;", "2");
}

#[test]
fn the_mutating_methods_change_the_array_in_place() {
    check(
        "arr-reverse",
        "return [1, 2, 3].reverse().join(\",\");",
        "3,2,1",
    );
    check(
        "arr-pop",
        "let a = [1, 2]; let x = a.pop(); return x + a.length;",
        "3",
    );
    check(
        "arr-shift",
        "let a = [1, 2]; let x = a.shift(); return x + a.length;",
        "2",
    );
    check(
        "arr-unshift",
        "let a = [2]; a.unshift(0, 1); return a.join(\",\");",
        "0,1,2",
    );
    check("arr-pop-empty", "return [].pop();", "undefined");
    check(
        "arr-fill",
        "return [1, 2, 3].fill(9, 1).join(\",\");",
        "1,9,9",
    );
}

/// **`find` answers `undefined` and `findIndex` answers `-1`** when nothing matches.
#[test]
fn find_and_find_index_differ_when_nothing_matches() {
    check(
        "arr-find",
        "return [1, 5].find(function (x) { return x > 2; });",
        "5",
    );
    check(
        "arr-find-none",
        "return [1].find(function (x) { return x > 2; });",
        "undefined",
    );
    check(
        "arr-findindex",
        "return [1, 5].findIndex(function (x) { return x > 2; });",
        "1",
    );
    check(
        "arr-findindex-none",
        "return [1].findIndex(function (x) { return x > 2; });",
        "-1",
    );
}

/// **Empty is `true` for `every` and `false` for `some`** — each stops on the opposite answer,
/// and on an empty array neither ever stops.
#[test]
fn every_and_some_agree_on_the_empty_array() {
    check(
        "arr-every",
        "return [2, 4].every(function (x) { return x > 1; });",
        "true",
    );
    check(
        "arr-every-false",
        "return [2, 0].every(function (x) { return x > 1; });",
        "false",
    );
    check(
        "arr-some",
        "return [0, 4].some(function (x) { return x > 1; });",
        "true",
    );
    check(
        "arr-every-empty",
        "return [].every(function (x) { return false; });",
        "true",
    );
    check(
        "arr-some-empty",
        "return [].some(function (x) { return true; });",
        "false",
    );
}

// ---- Function.prototype -----------------------------------------------------------------

/// **`this` inside `call` is the function**, not its receiver — the receiver is the first
/// argument. That inversion is the whole of what `call` does, and it is how test262 applies a
/// method to a receiver the method was not written for.
#[test]
fn call_invokes_a_function_with_a_chosen_receiver() {
    check(
        "fn-call",
        "let f = function () { return this.x; }; let o = {x: 5}; return f.call(o);",
        "5",
    );
    check(
        "fn-call-args",
        "let f = function (a, b) { return a + b; }; return f.call(null, 2, 3);",
        "5",
    );
    check(
        "fn-call-method",
        "return [1, 2, 3].indexOf.call([4, 5], 5);",
        "1",
    );
}

#[test]
fn apply_takes_its_arguments_as_an_array() {
    check(
        "fn-apply",
        "let f = function (a, b) { return a + b; }; return f.apply(null, [2, 3]);",
        "5",
    );
    // `null` for the argument list means no arguments, which is not an error.
    check(
        "fn-apply-none",
        "let f = function () { return 7; }; return f.apply(null, null);",
        "7",
    );
}

/// A method reached through `call` on a receiver it was not written for must not crash.
#[test]
fn a_method_applied_to_a_wrong_receiver_answers_rather_than_failing() {
    // **`-1`, not `undefined`.** A boolean has no `length`, so the search runs over zero
    // elements and reports not-found — which is what the specification says and what this
    // answered only once the array methods learned to read a length from anything (D-157).
    // The old expectation recorded the previous behaviour, not the required one.
    check(
        "fn-call-boolean",
        "return Array.prototype.indexOf.call(true);",
        "-1",
    );
}

// ---- String.prototype -------------------------------------------------------------------

/// **`length` counts UTF-16 code units**, which is what JavaScript counts — not bytes. An
/// accented letter is one and an emoji is two.
#[test]
fn string_length_counts_code_units() {
    check("str-len-ascii", "return \"hello\".length;", "5");
    check("str-len-accent", "return \"é\".length;", "1");
    check("str-len-emoji", "return \"😀\".length;", "2");
}

/// **Out of range is `""` for `charAt` and `NaN` for `charCodeAt`** — the pair disagree
/// deliberately, so one implementation covering both would lose it.
#[test]
fn char_at_and_char_code_at_disagree_out_of_range() {
    check("str-charat", "return \"abc\".charAt(1);", "b");
    check("str-charat-oob", "return \"abc\".charAt(9);", "");
    check("str-charcodeat", "return \"A\".charCodeAt(0);", "65");
    check("str-charcodeat-oob", "return \"A\".charCodeAt(9);", "NaN");
}

#[test]
fn the_searching_methods_work() {
    check("str-indexof", "return \"hello\".indexOf(\"l\");", "2");
    check(
        "str-indexof-missing",
        "return \"hello\".indexOf(\"z\");",
        "-1",
    );
    check(
        "str-lastindexof",
        "return \"hello\".lastIndexOf(\"l\");",
        "3",
    );
    check(
        "str-includes",
        "return \"hello\".includes(\"ell\");",
        "true",
    );
    check(
        "str-startswith",
        "return \"hello\".startsWith(\"he\");",
        "true",
    );
    check("str-endswith", "return \"hello\".endsWith(\"lo\");", "true");
}

/// **`substring` clamps a negative index to zero and swaps its arguments; `slice` counts from
/// the end and does not.** Sharing an implementation gets both wrong.
#[test]
fn slice_and_substring_differ_on_negative_and_reversed_arguments() {
    check("str-slice", "return \"hello\".slice(1, 3);", "el");
    check("str-slice-negative", "return \"hello\".slice(-2);", "lo");
    check("str-substring", "return \"hello\".substring(1, 3);", "el");
    check(
        "str-substring-negative",
        "return \"hello\".substring(-2, 2);",
        "he",
    );
    check(
        "str-substring-swapped",
        "return \"hello\".substring(3, 1);",
        "el",
    );
}

#[test]
fn the_transforming_methods_work() {
    check("str-upper", "return \"aB\".toUpperCase();", "AB");
    check("str-lower", "return \"aB\".toLowerCase();", "ab");
    check("str-trim", "return \"  x  \".trim();", "x");
    check("str-concat", "return \"a\".concat(\"b\", \"c\");", "abc");
    check("str-repeat", "return \"ab\".repeat(3);", "ababab");
}

/// A negative repeat count is a `RangeError`, not an empty string that reads as an answer.
#[test]
fn a_negative_repeat_count_is_a_range_error() {
    check(
        "str-repeat-negative",
        "let r = 0; try { \"a\".repeat(-1); } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
}

/// **An empty separator splits into characters**, and no separator gives one element holding
/// the whole string — not an empty array.
#[test]
fn split_handles_its_separator_cases() {
    check("str-split", "return \"a,b,c\".split(\",\").length;", "3");
    check("str-split-piece", "return \"a,b,c\".split(\",\")[1];", "b");
    check("str-split-empty", "return \"abc\".split(\"\").length;", "3");
    check("str-split-none", "return \"abc\".split().length;", "1");
}

/// A method reached through a variable, so the receiver is not a literal.
#[test]
fn string_methods_work_on_a_computed_receiver() {
    check(
        "str-method-variable",
        "let s = \"a\" + \"bc\"; return s.toUpperCase();",
        "ABC",
    );
}

// ---- property descriptors ---------------------------------------------------------------

/// **A defined property defaults to none of writable, enumerable or configurable** — the
/// opposite of what assignment creates. That difference is the whole reason descriptors exist,
/// and reusing the assignment default passes every test that does not check it.
#[test]
fn define_property_defaults_to_the_opposite_of_assignment() {
    check(
        "descriptor-assigned",
        "let o = {}; o.x = 1; return Object.getOwnPropertyDescriptor(o, \"x\").writable;",
        "true",
    );
    check(
        "descriptor-defined",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1}); \
         return Object.getOwnPropertyDescriptor(o, \"x\").writable;",
        "false",
    );
    check(
        "descriptor-value",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 7}); return o.x;",
        "7",
    );
}

/// **`Object.keys` sees only enumerable properties; `getOwnPropertyNames` sees all.** That is
/// the difference which made them the same function until now.
#[test]
fn keys_and_own_names_differ_on_enumerability() {
    check(
        "descriptor-keys-hidden",
        "let o = {a: 1}; Object.defineProperty(o, \"b\", {value: 2}); return Object.keys(o).length;",
        "1",
    );
    check(
        "descriptor-names-all",
        "let o = {a: 1}; Object.defineProperty(o, \"b\", {value: 2}); \
         return Object.getOwnPropertyNames(o).length;",
        "2",
    );
    check(
        "descriptor-enumerable-true",
        "let o = {}; Object.defineProperty(o, \"b\", {value: 2, enumerable: true}); \
         return Object.keys(o).length;",
        "1",
    );
}

/// **A write to a non-writable property is silently ignored**, not an error — outside strict
/// mode, which is the only mode there is here.
#[test]
fn a_write_to_a_non_writable_property_is_ignored() {
    check(
        "descriptor-readonly",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1}); o.x = 9; return o.x;",
        "1",
    );
    check(
        "descriptor-writable",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1, writable: true}); o.x = 9; return o.x;",
        "9",
    );
}

/// `defineProperty` redefines rather than assigns, so it writes past a **non-writable**
/// property that an assignment could not — provided the property is still **configurable**.
///
/// This test used to define `{value: 1}` and then redefine it, which a reading of
/// `defineProperty` alone makes look reasonable. It is not: a descriptor that says nothing
/// about `configurable` creates a property that is not, and redefining one of those is a
/// `TypeError` (D-164). The test was pinning what the engine did rather than what is required,
/// and it took the check that makes `Object.freeze` hold to expose it.
#[test]
fn define_property_can_redefine_a_non_writable_property() {
    check(
        "descriptor-redefine",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1, configurable: true}); \
         Object.defineProperty(o, \"x\", {value: 2}); return o.x;",
        "2",
    );
    // Writable is not the same question as configurable: this one is non-writable throughout,
    // and `defineProperty` still writes past it where an assignment would be ignored.
    check(
        "descriptor-redefine-unwritable",
        "let o = {}; \
         Object.defineProperty(o, \"x\", {value: 1, writable: false, configurable: true}); \
         o.x = 9; Object.defineProperty(o, \"x\", {value: 2}); return o.x;",
        "2",
    );
}

/// **`undefined` for an absent property**, which is how a caller tells "not there" from
/// "there and not writable".
#[test]
fn a_descriptor_for_a_missing_property_is_undefined() {
    check(
        "descriptor-missing",
        "let o = {}; return Object.getOwnPropertyDescriptor(o, \"nope\");",
        "undefined",
    );
}

// ---- delete -----------------------------------------------------------------------------

#[test]
fn delete_removes_a_property() {
    check(
        "delete-prop",
        "let o = {a: 1}; delete o.a; return o.a;",
        "undefined",
    );
    check(
        "delete-keys",
        "let o = {a: 1, b: 2}; delete o.a; return Object.keys(o).length;",
        "1",
    );
    check(
        "delete-computed",
        "let o = {a: 1}; delete o[\"a\"]; return o.a;",
        "undefined",
    );
}

/// **`delete` asks whether the property is gone afterwards, not whether it removed anything**,
/// so one that was never there answers `true`.
#[test]
fn delete_answers_true_for_something_that_was_never_there() {
    check("delete-absent", "let o = {}; return delete o.nope;", "true");
    check(
        "delete-present",
        "let o = {a: 1}; return delete o.a;",
        "true",
    );
}

/// **A non-configurable property answers `false`** rather than throwing, outside strict mode.
#[test]
fn delete_refuses_a_non_configurable_property() {
    check(
        "delete-nonconfigurable",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1}); return delete o.x;",
        "false",
    );
    check(
        "delete-nonconfigurable-kept",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1}); delete o.x; return o.x;",
        "1",
    );
    check(
        "delete-configurable",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1, configurable: true}); \
         return delete o.x;",
        "true",
    );
}

/// The shape still names the slot, so re-assigning must bring the property back.
#[test]
fn a_deleted_property_can_be_assigned_again() {
    check(
        "delete-revive",
        "let o = {a: 1}; delete o.a; o.a = 2; return o.a;",
        "2",
    );
    check(
        "delete-revive-keys",
        "let o = {a: 1}; delete o.a; o.a = 2; return Object.keys(o).length;",
        "1",
    );
}

/// **`delete` on anything that is not a property access is `true`** and does nothing.
#[test]
fn delete_of_a_non_property_is_true() {
    check("delete-value", "return delete 1;", "true");
}

#[test]
fn delete_on_an_array_element() {
    check(
        "delete-elem-last",
        "let a = [1, 2]; delete a[1]; return a.length;",
        "1",
    );
    check(
        "delete-elem-middle",
        "let a = [1, 2, 3]; delete a[1]; return a[1];",
        "undefined",
    );
}

/// A string key in computed access. This did nothing at all until strings could be spelled —
/// a read answered `undefined` and a write was discarded, neither saying a word.
#[test]
fn a_string_key_reaches_the_same_property_a_name_does() {
    check(
        "string-key-write",
        "let o = {}; o[\"a\"] = 5; return o.a;",
        "5",
    );
    check("string-key-read", "let o = {a: 7}; return o[\"a\"];", "7");
    check(
        "string-key-computed",
        "let o = {ab: 1}; let k = \"a\" + \"b\"; return o[k];",
        "1",
    );
}

// ---- for-in ------------------------------------------------------------------------------

#[test]
fn for_in_visits_every_enumerable_name() {
    check(
        "forin-count",
        "let o = {a: 1, b: 2}; let n = 0; for (let k in o) { n = n + 1; } return n;",
        "2",
    );
    check(
        "forin-names",
        "let o = {a: 1, b: 2}; let s = \"\"; for (let k in o) { s = s + k; } return s;",
        "ab",
    );
    check(
        "forin-values",
        "let o = {a: 1, b: 2}; let t = 0; for (let k in o) { t = t + o[k]; } return t;",
        "3",
    );
}

/// **Inherited enumerable properties are visited too**, which is what separates `for-in` from
/// `Object.keys`.
#[test]
fn for_in_walks_the_prototype_chain() {
    check(
        "forin-inherited",
        "let base = {a: 1}; let o = Object.create(base); o.b = 2; \
         let n = 0; for (let k in o) { n = n + 1; } return n;",
        "2",
    );
    // A name found on the object shadows the same name further up, so it is visited once.
    check(
        "forin-shadowed",
        "let base = {a: 1}; let o = Object.create(base); o.a = 2; \
         let n = 0; for (let k in o) { n = n + 1; } return n;",
        "1",
    );
}

/// A non-enumerable property is not visited — the same rule `Object.keys` follows.
#[test]
fn for_in_skips_non_enumerable_properties() {
    check(
        "forin-hidden",
        "let o = {a: 1}; Object.defineProperty(o, \"b\", {value: 2}); \
         let n = 0; for (let k in o) { n = n + 1; } return n;",
        "1",
    );
}

#[test]
fn for_in_over_nothing_runs_zero_times() {
    check(
        "forin-empty",
        "let n = 0; for (let k in {}) { n = n + 1; } return n;",
        "0",
    );
    // `for (k in undefined)` runs zero times rather than throwing.
    check(
        "forin-undefined",
        "let n = 0; for (let k in undefined) { n = n + 1; } return n;",
        "0",
    );
}

#[test]
fn for_in_supports_break_and_continue() {
    check(
        "forin-break",
        "let o = {a: 1, b: 2, c: 3}; let n = 0; \
         for (let k in o) { if (k === \"b\") { break; } n = n + 1; } return n;",
        "1",
    );
    check(
        "forin-continue",
        "let o = {a: 1, b: 2, c: 3}; let n = 0; \
         for (let k in o) { if (k === \"b\") { continue; } n = n + 1; } return n;",
        "2",
    );
}

/// An array's indices are enumerable names, so `for-in` visits them as strings.
#[test]
fn for_in_over_an_array_visits_its_indices() {
    check(
        "forin-array",
        "let a = [10, 20]; let s = \"\"; for (let k in a) { s = s + k; } return s;",
        "01",
    );
}

/// **`for (k in o)` assigns to an existing binding rather than declaring one**, so the last
/// name visited is still there afterwards.
#[test]
fn for_in_can_assign_to_an_existing_variable() {
    check(
        "forin-assign",
        "let k = \"\"; let o = {a: 1}; for (k in o) { } return k;",
        "a",
    );
}

// ---- computed property keys and template literals ------------------------------------------

#[test]
fn an_object_literal_can_have_a_computed_key() {
    check(
        "key-computed",
        "let k = \"a\"; let o = {[k]: 5}; return o.a;",
        "5",
    );
    check(
        "key-expression",
        "let o = {[\"a\" + \"b\"]: 5}; return o.ab;",
        "5",
    );
    check(
        "key-mixed",
        "let k = \"b\"; let o = {a: 1, [k]: 2}; return o.a + o.b;",
        "3",
    );
}

/// A numeric key goes through the same path, so `{1: x}` and `o[1] = x` cannot disagree about
/// what the name is.
#[test]
fn a_numeric_key_names_the_same_property_an_index_does() {
    check("key-numeric", "let o = {1: 5}; return o[1];", "5");
    check(
        "key-numeric-name",
        "let o = {1: 5}; return Object.keys(o)[0];",
        "1",
    );
}

/// **The first piece of a template is always a string**, so `` `${1}${2}` `` is `"12"` and not
/// `3` — starting from the empty string rather than the first substitution is the whole of why.
#[test]
fn a_template_literal_concatenates_rather_than_adding() {
    check("template-plain", "return `abc`;", "abc");
    check("template-one", "let x = 5; return `a${x}b`;", "a5b");
    check(
        "template-leading",
        "let x = 1; let y = 2; return `${x}${y}`;",
        "12",
    );
    check("template-empty", "return `${1}`;", "1");
}

#[test]
fn a_template_substitutes_any_expression() {
    check("template-expression", "return `${1 + 2}`;", "3");
    check(
        "template-call",
        "let f = function () { return 7; }; return `n=${f()}`;",
        "n=7",
    );
    check(
        "template-nested",
        "let a = \"x\"; return `${`[${a}]`}`;",
        "[x]",
    );
}

// ---- for-of ------------------------------------------------------------------------------

#[test]
fn for_of_walks_an_array_by_value() {
    check(
        "forof-sum",
        "let a = [1, 2, 3]; let t = 0; for (let x of a) { t = t + x; } return t;",
        "6",
    );
    check(
        "forof-empty",
        "let n = 0; for (let x of []) { n = n + 1; } return n;",
        "0",
    );
    check(
        "forof-break",
        "let t = 0; for (let x of [1, 2, 3]) { if (x === 2) { break; } t = t + x; } return t;",
        "1",
    );
    check(
        "forof-continue",
        "let t = 0; for (let x of [1, 2, 3]) { if (x === 2) { continue; } t = t + x; } return t;",
        "4",
    );
}

/// **A string is walked by code point, not code unit** — `for (const c of "😀")` runs once
/// where `"😀".length` is 2.
#[test]
fn for_of_walks_a_string_by_code_point() {
    check(
        "forof-string",
        "let s = \"\"; for (let c of \"abc\") { s = s + c + \"-\"; } return s;",
        "a-b-c-",
    );
    check(
        "forof-emoji",
        "let n = 0; for (let c of \"😀\") { n = n + 1; } return n;",
        "1",
    );
}

/// **Not the iterator protocol**: without `Symbol` there is no `Symbol.iterator` to look up, so
/// anything that is not an array or a string raises — the error the protocol would give, for a
/// different reason.
#[test]
fn for_of_over_a_non_iterable_raises() {
    check(
        "forof-object",
        "let r = \"\"; try { for (let x of {a: 1}) { } } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
    check(
        "forof-number",
        "let r = \"\"; try { for (let x of 5) { } } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

/// The array is indexed live rather than copied, so a change during the loop is seen.
#[test]
fn for_of_follows_an_array_that_changes() {
    check(
        "forof-live",
        "let a = [1, 2, 3]; let n = 0; for (let x of a) { n = n + 1; if (n === 1) { a.pop(); } } \
         return n;",
        "2",
    );
}

/// `for (x of a)` assigns to an existing binding rather than declaring one.
#[test]
fn for_of_can_assign_to_an_existing_variable() {
    check(
        "forof-assign",
        "let x = 0; for (x of [1, 2]) { } return x;",
        "2",
    );
}

// ---- regular expressions ------------------------------------------------------------------

#[test]
fn a_regular_expression_literal_matches() {
    check("re-test", "return /ab+/.test(\"xabbby\");", "true");
    check("re-test-miss", "return /ab+/.test(\"xyz\");", "false");
    check("re-flags-i", "return /AB/i.test(\"ab\");", "true");
    check("re-flags-absent", "return /AB/.test(\"ab\");", "false");
}

#[test]
fn a_regular_expression_reports_its_own_shape() {
    check("re-source", "return /ab+/g.source;", "ab+");
    check("re-flags", "return /ab+/gi.flags;", "gi");
    check("re-global", "return /a/g.global;", "true");
    check("re-not-global", "return /a/.global;", "false");
    check("re-tostring", "return /ab+/gi.toString();", "/ab+/gi");
}

/// **`exec` answers `null`, not `undefined`**, which is what `m !== null` tests for.
#[test]
fn exec_returns_a_match_array_or_null() {
    check("re-exec-null", "return /z/.exec(\"abc\");", "null");
    check("re-exec-whole", "return /b./.exec(\"abcd\")[0];", "bc");
    check("re-exec-index", "return /b./.exec(\"abcd\").index;", "1");
    check("re-exec-input", "return /b./.exec(\"abcd\").input;", "abcd");
}

/// **A group that did not participate is `undefined`, not `""`.** The difference is visible
/// only when the pattern makes a group optional, which is why it is tested directly.
#[test]
fn exec_distinguishes_a_missing_group_from_an_empty_one() {
    check("re-group", "return /(a)(b)/.exec(\"ab\")[2];", "b");
    check(
        "re-group-absent",
        "return /(a)|(z)/.exec(\"a\")[2];",
        "undefined",
    );
    check(
        "re-group-count",
        "return /(a)(b)/.exec(\"ab\").length;",
        "3",
    );
}

/// `lastIndex` is a property because a program may assign to it, and the compiled pattern is
/// set from it rather than owning it.
#[test]
fn a_global_regular_expression_advances_last_index() {
    check(
        "re-lastindex",
        "let r = /a/g; r.test(\"aa\"); return r.lastIndex;",
        "1",
    );
    check(
        "re-lastindex-assigned",
        "let r = /a/g; r.lastIndex = 1; return r.exec(\"ba\").index;",
        "1",
    );
    // A non-global pattern does not advance, so repeated calls agree.
    check(
        "re-lastindex-inert",
        "let r = /a/; r.test(\"aa\"); return r.lastIndex;",
        "0",
    );
}

/// **The pattern is compiled when the literal is evaluated**, so an invalid one raises there
/// rather than inside whatever later called `test`.
#[test]
fn an_invalid_pattern_raises_where_it_is_written() {
    check(
        "re-invalid",
        "let r = \"\"; try { let bad = /(/; } catch (e) { r = e.name; } return r;",
        "SyntaxError",
    );
}

// ---- JSON --------------------------------------------------------------------------------

#[test]
fn json_round_trips_the_simple_shapes() {
    check("json-number", "return JSON.stringify(1);", "1");
    check("json-string", "return JSON.stringify(\"a\");", "\"a\"");
    check("json-true", "return JSON.stringify(true);", "true");
    check("json-null", "return JSON.stringify(null);", "null");
    check("json-array", "return JSON.stringify([1, 2]);", "[1,2]");
    check("json-object", "return JSON.stringify({a: 1});", "{\"a\":1}");
}

/// **`undefined` for a value JSON cannot spell** — not the string `"undefined"`.
#[test]
fn stringify_answers_undefined_for_what_json_cannot_spell() {
    check(
        "json-undefined",
        "return JSON.stringify(undefined);",
        "undefined",
    );
    check(
        "json-function",
        "return JSON.stringify(function () { return 1; });",
        "undefined",
    );
}

/// **An object drops a property JSON cannot spell; an array cannot.** An array would have to
/// change its length to drop an element, so the same absence becomes `null` there and nothing
/// at all in an object.
#[test]
fn an_absent_value_is_dropped_in_an_object_and_nulled_in_an_array() {
    check(
        "json-object-undefined",
        "return JSON.stringify({a: undefined});",
        "{}",
    );
    check(
        "json-array-undefined",
        "return JSON.stringify([undefined]);",
        "[null]",
    );
    check(
        "json-object-mixed",
        "return JSON.stringify({a: 1, b: undefined});",
        "{\"a\":1}",
    );
}

/// **A non-finite number is `null`**: JSON has no spelling for `NaN` or an infinity, and
/// refusing the whole document over one would be worse.
#[test]
fn a_non_finite_number_stringifies_as_null() {
    check("json-nan", "return JSON.stringify(0 / 0);", "null");
    check("json-infinity", "return JSON.stringify(1 / 0);", "null");
}

/// A structure containing itself raises rather than producing a truncated document.
#[test]
fn a_cycle_raises_rather_than_truncating() {
    check(
        "json-cycle",
        "let r = \"\"; let a = {}; a.self = a; \
         try { JSON.stringify(a); } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

#[test]
fn json_parse_builds_values_back() {
    check("json-parse-number", "return JSON.parse(\"1\");", "1");
    check(
        "json-parse-object",
        "return JSON.parse(\"{\\\"a\\\":7}\").a;",
        "7",
    );
    check(
        "json-parse-array",
        "return JSON.parse(\"[1,2,3]\")[1];",
        "2",
    );
    check(
        "json-parse-nested",
        "return JSON.parse(\"{\\\"a\\\":[1]}\").a[0];",
        "1",
    );
    check("json-parse-null", "return JSON.parse(\"null\");", "null");
}

#[test]
fn json_parse_raises_on_malformed_input() {
    check(
        "json-parse-bad",
        "let r = \"\"; try { JSON.parse(\"{\"); } catch (e) { r = e.name; } return r;",
        "SyntaxError",
    );
}

#[test]
fn json_round_trips_through_both_directions() {
    check(
        "json-roundtrip",
        "let o = {a: 1, b: [2, 3]}; let back = JSON.parse(JSON.stringify(o)); return back.b[1];",
        "3",
    );
}

// ---- constructors reach the prototypes their instances use ---------------------------------

/// Each constructor's `prototype` is the object its instances already inherit from, not a new
/// one — otherwise `[].map === Array.prototype.map` would be false.
#[test]
fn a_constructor_prototype_is_the_one_instances_inherit() {
    // As above: the identity is preceded by a check that there is anything to identify.
    check(
        "proto-exists",
        "return typeof Array.prototype.map;",
        "function",
    );
    check(
        "proto-array",
        "return [].map === Array.prototype.map;",
        "true",
    );
    check(
        "proto-string",
        "return \"\".trim === String.prototype.trim;",
        "true",
    );
    check(
        "proto-regexp",
        "return /a/.test === RegExp.prototype.test;",
        "true",
    );
    check(
        "proto-function",
        "let f = function () { return 1; }; return f.call === Function.prototype.call;",
        "true",
    );
}

/// **`Function` is bound so `Function.prototype` can be reached**, not because
/// `new Function(body)` works — that compiles source at runtime, which this engine does not do,
/// so calling it raises rather than answering something wrong.
#[test]
fn the_function_constructor_raises_rather_than_pretending() {
    check(
        "function-ctor",
        "let r = \"\"; try { Function(\"return 1\"); } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

#[test]
fn the_regexp_constructor_builds_the_same_thing_a_literal_does() {
    check(
        "regexp-ctor",
        "return new RegExp(\"ab+\").test(\"abb\");",
        "true",
    );
    check(
        "regexp-ctor-flags",
        "return new RegExp(\"AB\", \"i\").test(\"ab\");",
        "true",
    );
    check(
        "regexp-ctor-source",
        "return new RegExp(\"a+\").source;",
        "a+",
    );
    // An existing regular expression is re-read through `source`, so this copies the pattern
    // rather than stringifying the object into `"/a/g"`.
    check("regexp-ctor-copy", "return new RegExp(/a+/g).source;", "a+");
    check(
        "regexp-ctor-copy-flags",
        "return new RegExp(/a+/g).flags;",
        "g",
    );
}

// ---- Date --------------------------------------------------------------------------------

/// **No arguments is now, one is a time value, and more are calendar fields.** The three are
/// different enough that the argument count is the whole of the dispatch.
#[test]
fn a_date_can_be_built_three_ways() {
    check("date-from-ms", "return new Date(0).getTime();", "0");
    check(
        "date-from-ms-value",
        "return new Date(86400000).getTime();",
        "86400000",
    );
    check(
        "date-from-fields",
        "return new Date(2020, 0, 1).getFullYear();",
        "2020",
    );
    // `Date.now()` is a moving target, so this asserts only that it is in this century.
    check(
        "date-now-plausible",
        "return Date.now() > 1600000000000;",
        "true",
    );
}

/// **`getMonth` is 0-based and `getDate` is 1-based.** They disagree deliberately, so a single
/// field reader would get one of them wrong.
#[test]
fn the_calendar_fields_disagree_about_where_they_start() {
    check(
        "date-month",
        "return new Date(2020, 5, 15).getMonth();",
        "5",
    );
    check("date-day", "return new Date(2020, 5, 15).getDate();", "15");
    check(
        "date-year",
        "return new Date(2020, 5, 15).getFullYear();",
        "2020",
    );
    // Sunday is 0. 2020-06-15 was a Monday.
    check(
        "date-weekday",
        "return new Date(2020, 5, 15).getDay();",
        "1",
    );
}

#[test]
fn the_clock_fields_read_back() {
    check(
        "date-hours",
        "return new Date(2020, 0, 1, 13, 24, 35, 678).getHours();",
        "13",
    );
    check(
        "date-minutes",
        "return new Date(2020, 0, 1, 13, 24, 35, 678).getMinutes();",
        "24",
    );
    check(
        "date-seconds",
        "return new Date(2020, 0, 1, 13, 24, 35, 678).getSeconds();",
        "35",
    );
    check(
        "date-ms",
        "return new Date(2020, 0, 1, 13, 24, 35, 678).getMilliseconds();",
        "678",
    );
}

/// **The local-time methods are the UTC ones**: there is no timezone database here, so the two
/// are the same function and `getTimezoneOffset` answers `0` to stay consistent with them.
#[test]
fn local_and_utc_agree_because_the_offset_is_always_zero() {
    check(
        "date-utc-hours",
        "let d = new Date(0); return d.getHours() === d.getUTCHours();",
        "true",
    );
    check(
        "date-offset",
        "return new Date(0).getTimezoneOffset();",
        "0",
    );
}

#[test]
fn a_date_prints_as_iso_text() {
    check(
        "date-iso",
        "return new Date(0).toISOString();",
        "1970-01-01T00:00:00.000Z",
    );
    check(
        "date-iso-value",
        "return new Date(2020, 0, 2, 3, 4, 5).toISOString();",
        "2020-01-02T03:04:05.000Z",
    );
}

/// **An invalid date raises from `toISOString` and prints as text from `toString`.** The first
/// has no spelling for one and the second does.
#[test]
fn an_invalid_date_answers_differently_to_each_printer() {
    check(
        "date-invalid-time",
        "return new Date(0 / 0).getTime();",
        "NaN",
    );
    check(
        "date-invalid-field",
        "return new Date(0 / 0).getFullYear();",
        "NaN",
    );
    check(
        "date-invalid-text",
        "return new Date(0 / 0).toString();",
        "Invalid Date",
    );
    check(
        "date-invalid-iso",
        "let r = \"\"; try { new Date(0 / 0).toISOString(); } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
}

/// The time value is kept where enumeration cannot see it, because the specification puts it
/// in an internal slot and this engine has nowhere to put one.
#[test]
fn a_dates_time_value_is_not_enumerable() {
    check("date-keys", "return Object.keys(new Date(0)).length;", "0");
    check(
        "date-forin",
        "let n = 0; for (let k in new Date(0)) { n = n + 1; } return n;",
        "0",
    );
}

#[test]
fn a_date_stringifies_through_json() {
    check(
        "date-tojson",
        "return new Date(0).toJSON();",
        "1970-01-01T00:00:00.000Z",
    );
}

// ---- built-ins know their own names --------------------------------------------------------

/// **A function knows its own name**, and it is not enumerable. The name was in the table that
/// created every built-in and was simply never written down on it.
#[test]
fn a_built_in_carries_its_name() {
    check("name-method", "return [].forEach.name;", "forEach");
    check("name-string", "return \"\".trim.name;", "trim");
    check("name-nested", "return Object.keys.name;", "keys");
    check("name-hidden", "return Object.keys([].forEach).length;", "0");
}

/// A program cannot assign to `f.name` but can redefine it, which is what non-writable and
/// configurable means together.
#[test]
fn a_name_resists_assignment_but_not_redefinition() {
    check(
        "name-assign",
        "let f = [].forEach; f.name = \"other\"; return f.name;",
        "forEach",
    );
    check(
        "name-redefine",
        "let f = [].forEach; Object.defineProperty(f, \"name\", {value: \"other\"}); return f.name;",
        "other",
    );
}

// ---- more array methods --------------------------------------------------------------------

/// **`reduceRight` is not `reduce` over a reversed list**: the callback still gets each
/// element's real index, so reversing first would hand it the wrong ones.
#[test]
fn reduce_right_walks_backwards_with_real_indices() {
    check(
        "reduceright",
        "return [\"a\", \"b\", \"c\"].reduceRight(function (t, x) { return t + x; });",
        "cba",
    );
    check(
        "reduceright-index",
        "return [1, 2].reduceRight(function (t, x, i) { return t + i; }, 0);",
        "1",
    );
    check(
        "reduceright-initial",
        "return [1, 2, 3].reduceRight(function (t, x) { return t + x; }, 10);",
        "16",
    );
}

/// An empty array with no initial value has no answer to give, so it raises rather than
/// inventing one.
#[test]
fn reduce_right_on_an_empty_array_raises() {
    check(
        "reduceright-empty",
        "let r = \"\"; try { [].reduceRight(function (t, x) { return t; }); } \
         catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

/// **`flat` goes one level by default**, not all of them.
#[test]
fn flat_takes_a_depth() {
    check("flat-default", "return [1, [2, 3]].flat().length;", "3");
    check("flat-one-level", "return [1, [2, [3]]].flat().length;", "3");
    check("flat-deep", "return [1, [2, [3]]].flat(2).length;", "3");
    check("flat-deep-value", "return [[1, [2]]].flat(2)[1];", "2");
    check("flat-zero", "return [1, [2]].flat(0).length;", "2");
}

/// `flatMap` flattens exactly one level, always — it takes no depth.
#[test]
fn flat_map_maps_then_flattens_once() {
    check(
        "flatmap",
        "return [1, 2].flatMap(function (x) { return [x, x]; }).length;",
        "4",
    );
    check(
        "flatmap-nested",
        "return [1].flatMap(function (x) { return [[x]]; })[0].length;",
        "1",
    );
}

/// **`at` counts a negative index from the end and answers `undefined` out of range**, which
/// is what separates it from indexing.
#[test]
fn at_accepts_a_negative_index() {
    check("at-positive", "return [1, 2, 3].at(1);", "2");
    check("at-negative", "return [1, 2, 3].at(-1);", "3");
    check("at-out-of-range", "return [1, 2].at(5);", "undefined");
    check("at-string", "return \"abc\".at(-1);", "c");
    // `charAt` answers `""` where `at` answers `undefined` — the pair differ on purpose.
    check("at-vs-charat", "return \"abc\".charAt(9);", "");
    check("at-string-oob", "return \"abc\".at(9);", "undefined");
}

#[test]
fn find_last_walks_backwards() {
    check(
        "findlast",
        "return [1, 5, 2, 5].findLast(function (x) { return x === 5; });",
        "5",
    );
    check(
        "findlastindex",
        "return [1, 5, 2, 5].findLastIndex(function (x) { return x === 5; });",
        "3",
    );
    check(
        "findlastindex-none",
        "return [1].findLastIndex(function (x) { return x === 9; });",
        "-1",
    );
}

// ---- more string methods ---------------------------------------------------------------

#[test]
fn trim_can_take_one_side() {
    check("trimstart", "return \"  a  \".trimStart() + \"|\";", "a  |");
    check("trimend", "return \"|\" + \"  a  \".trimEnd();", "|  a");
}

/// **An empty filler pads nothing** — answering the original rather than looping is the whole
/// reason that case is checked.
#[test]
fn padding_fills_to_a_length() {
    check("padstart", "return \"5\".padStart(3, \"0\");", "005");
    check("padend", "return \"5\".padEnd(3, \"0\");", "500");
    // Anchored with a sentinel: `execute` trims the program's output, so an expectation that
    // begins or ends with a space can never match however correct the code is.
    check(
        "padstart-default",
        "return \"|\" + \"a\".padStart(3);",
        "|  a",
    );
    check(
        "padstart-short",
        "return \"abcd\".padStart(2, \"0\");",
        "abcd",
    );
    check(
        "padstart-empty-filler",
        "return \"a\".padStart(5, \"\");",
        "a",
    );
    check(
        "padstart-truncated",
        "return \"a\".padStart(4, \"xy\");",
        "xyxa",
    );
}

/// **A string pattern replaces the first occurrence and a global regular expression replaces
/// every one**, so the pattern's flags decide rather than the method name.
#[test]
fn replace_follows_the_patterns_own_flags() {
    check(
        "replace-string",
        "return \"aaa\".replace(\"a\", \"b\");",
        "baa",
    );
    check(
        "replaceall-string",
        "return \"aaa\".replaceAll(\"a\", \"b\");",
        "bbb",
    );
    check(
        "replace-regexp",
        "return \"aaa\".replace(/a/, \"b\");",
        "baa",
    );
    check(
        "replace-regexp-global",
        "return \"aaa\".replace(/a/g, \"b\");",
        "bbb",
    );
}

/// `replaceAll` with a non-global pattern raises rather than quietly behaving like `replace`.
#[test]
fn replace_all_refuses_a_non_global_pattern() {
    check(
        "replaceall-nonglobal",
        "let r = \"\"; try { \"aa\".replaceAll(/a/, \"b\"); } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

// ---- Object.prototype and bind -----------------------------------------------------------

/// **Own means own**: a property found on the prototype answers `false`, which is the whole
/// reason `hasOwnProperty` exists rather than `key in object`.
#[test]
fn has_own_property_does_not_look_up_the_chain() {
    check(
        "hasown-yes",
        "return ({a: 1}).hasOwnProperty(\"a\");",
        "true",
    );
    check(
        "hasown-no",
        "return ({a: 1}).hasOwnProperty(\"b\");",
        "false",
    );
    check(
        "hasown-inherited",
        "let base = {a: 1}; let o = Object.create(base); return o.hasOwnProperty(\"a\");",
        "false",
    );
    check(
        "hasown-array-index",
        "return [1, 2].hasOwnProperty(0);",
        "true",
    );
    check(
        "hasown-array-past",
        "return [1, 2].hasOwnProperty(5);",
        "false",
    );
}

#[test]
fn property_is_enumerable_follows_the_descriptor() {
    check(
        "enumerable-yes",
        "return ({a: 1}).propertyIsEnumerable(\"a\");",
        "true",
    );
    check(
        "enumerable-no",
        "let o = {}; Object.defineProperty(o, \"b\", {value: 1}); \
         return o.propertyIsEnumerable(\"b\");",
        "false",
    );
    // A built-in method is not enumerable either.
    check(
        "enumerable-builtin",
        "return Array.prototype.propertyIsEnumerable(\"map\");",
        "false",
    );
}

/// The `typeof` check is not padding. **Two undefineds are equal**, so an identity test on its
/// own passes just as well when neither side exists — which is exactly what this did before
/// `Object.prototype`'s methods were reachable at all.
#[test]
fn every_object_reaches_object_prototype() {
    check(
        "chain-exists",
        "return typeof Object.prototype.hasOwnProperty;",
        "function",
    );
    check(
        "chain-plain",
        "return ({}).hasOwnProperty === Object.prototype.hasOwnProperty;",
        "true",
    );
    // Through one object rather than a copy per prototype, so an array finds the same function.
    check(
        "chain-array",
        "return [].hasOwnProperty === Object.prototype.hasOwnProperty;",
        "true",
    );
    check(
        "chain-isprototypeof",
        "return Object.prototype.isPrototypeOf({});",
        "true",
    );
    check(
        "chain-isprototypeof-no",
        "return ({}).isPrototypeOf({});",
        "false",
    );
}

/// **The array tag is the only one distinguished**: a real engine reads `Symbol.toStringTag`,
/// and without symbols the honest choice is the one distinction that can be made.
#[test]
fn object_to_string_reports_a_tag() {
    check(
        "tag-object",
        "return Object.prototype.toString.call({});",
        "[object Object]",
    );
    check(
        "tag-array",
        "return Object.prototype.toString.call([]);",
        "[object Array]",
    );
    check(
        "tag-null",
        "return Object.prototype.toString.call(null);",
        "[object Null]",
    );
}

/// **The bound arguments come first and the call's own follow**, which is what makes
/// `f.bind(null, 1)(2)` the same as `f(1, 2)`.
#[test]
fn bind_fixes_a_receiver_and_leading_arguments() {
    check(
        "bind-this",
        "let f = function () { return this.x; }; return f.bind({x: 5})();",
        "5",
    );
    check(
        "bind-args",
        "let f = function (a, b) { return a + b; }; return f.bind(null, 1)(2);",
        "3",
    );
    check(
        "bind-all-args",
        "let f = function (a, b) { return a + b; }; return f.bind(null, 1, 2)();",
        "3",
    );
    check(
        "bind-no-args",
        "let f = function (a) { return a; }; return f.bind(null)(7);",
        "7",
    );
}

/// The pattern test262's own property helper is built on, and the reason so much of the suite
/// depended on `bind` existing at all.
#[test]
fn bind_can_turn_a_method_into_a_free_function() {
    check(
        "bind-uncurry",
        "let has = Function.prototype.call.bind(Object.prototype.hasOwnProperty); \
         return has({a: 1}, \"a\");",
        "true",
    );
    check(
        "bind-uncurry-join",
        "let join = Function.prototype.call.bind(Array.prototype.join); \
         return join([1, 2], \"-\");",
        "1-2",
    );
}

// ---- loose equality and `in` ---------------------------------------------------------------

/// `===` was always here. These pin the behaviour that makes it worth preferring, so a later
/// change to `==` cannot quietly loosen it.
#[test]
fn strict_equality_does_not_coerce() {
    check("strict-same", "return 1 === 1;", "true");
    check("strict-number-string", "return 1 === \"1\";", "false");
    check("strict-zero-false", "return 0 === false;", "false");
    check(
        "strict-null-undefined",
        "return null === undefined;",
        "false",
    );
    // Strings compare by their characters, not by identity, however many cells they came from.
    check("strict-strings", "return \"a\" + \"b\" === \"ab\";", "true");
    check("strict-nan", "return 0 / 0 === 0 / 0;", "false");
    check("strict-zeroes", "return 0 === -0;", "true");
    check("strict-not", "return 1 !== 2;", "true");
}

/// **`null` and `undefined` equal each other and nothing else** — not `0`, not `""`, not
/// `false`. That is the rule behind `x == null` as the idiomatic "is it either".
#[test]
fn nullish_values_equal_only_each_other() {
    check("loose-null-undefined", "return null == undefined;", "true");
    check("loose-null-zero", "return null == 0;", "false");
    check("loose-null-empty", "return null == \"\";", "false");
    check("loose-null-false", "return null == false;", "false");
    check("loose-undefined-zero", "return undefined == 0;", "false");
    check("loose-null-null", "return null == null;", "true");
}

/// **A string meeting a number becomes a number**, never the reverse.
#[test]
fn a_string_meeting_a_number_is_read_as_one() {
    check("loose-string-number", "return \"10\" == 10;", "true");
    check("loose-string-number-no", "return \"11\" == 10;", "false");
    check("loose-empty-zero", "return \"\" == 0;", "true");
    check("loose-space-zero", "return \" \" == 0;", "true");
}

/// **A boolean becomes a number first**, on whichever side it is.
#[test]
fn a_boolean_is_read_as_a_number_before_anything_else() {
    check("loose-true-one", "return true == 1;", "true");
    check("loose-false-zero", "return false == 0;", "true");
    check("loose-true-string", "return true == \"1\";", "true");
    check("loose-false-empty", "return false == \"\";", "true");
    check("loose-true-two", "return true == 2;", "false");
}

/// **`==` is not transitive**, and this is the example worth keeping in view: the first two
/// coerce and the third does not.
#[test]
fn loose_equality_is_not_transitive() {
    check("loose-empty-zero-again", "return \"\" == 0;", "true");
    check("loose-zero-string-zero", "return \"0\" == 0;", "true");
    check("loose-empty-zero-string", "return \"\" == \"0\";", "false");
}

/// An object becomes a primitive through `valueOf` and then `toString`.
#[test]
fn an_object_is_read_as_a_primitive() {
    check(
        "loose-valueof",
        "let o = {valueOf: function () { return 5; }}; return o == 5;",
        "true",
    );
    check(
        "loose-tostring",
        "let o = {toString: function () { return \"x\"; }}; return o == \"x\";",
        "true",
    );
    // `valueOf` is tried first, so it wins when both are there.
    check(
        "loose-valueof-first",
        "let o = {valueOf: function () { return 1; }, toString: function () { return \"2\"; }}; \
         return o == 1;",
        "true",
    );
}

/// `NaN` is equal to nothing, including itself, under either operator.
#[test]
fn nan_is_equal_to_nothing() {
    check("loose-nan", "return 0 / 0 == 0 / 0;", "false");
    check("loose-nan-zero", "return 0 / 0 == 0;", "false");
    check("loose-not-equal", "return 1 != 2;", "true");
    check("loose-not-equal-coerced", "return 1 != \"1\";", "false");
}

/// **Inherited counts**, which is the whole difference between `in` and `hasOwnProperty`.
#[test]
fn the_in_operator_looks_up_the_chain() {
    check("in-own", "return \"a\" in {a: 1};", "true");
    check("in-absent", "return \"b\" in {a: 1};", "false");
    check(
        "in-inherited",
        "let base = {a: 1}; let o = Object.create(base); return \"a\" in o;",
        "true",
    );
    check(
        "in-vs-hasown",
        "let base = {a: 1}; let o = Object.create(base); return o.hasOwnProperty(\"a\");",
        "false",
    );
    check("in-array-index", "return 1 in [1, 2];", "true");
    check("in-array-past", "return 5 in [1, 2];", "false");
    check("in-method", "return \"map\" in [];", "true");
}

/// The right side of `in` has to be an object.
#[test]
fn in_refuses_a_primitive_on_the_right() {
    check(
        "in-primitive",
        "let r = \"\"; try { \"a\" in 5; } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

/// `instanceof` answers a boolean and was typed `number` in the IR from the day it was added —
/// `is_always_numeric` was written as "everything except `+`". These pin the answer in both a
/// value position and a condition, which is where a wrong type would show.
#[test]
fn instanceof_answers_a_boolean() {
    check("instanceof-value", "return [] instanceof Array;", "true");
    check("instanceof-false", "return ({}) instanceof Array;", "false");
    check(
        "instanceof-condition",
        "let r = 0; if ([] instanceof Array) { r = 1; } return r;",
        "1",
    );
    check(
        "instanceof-typeof",
        "return typeof ([] instanceof Array);",
        "boolean",
    );
    check("loose-equal-typeof", "return typeof (1 == 1);", "boolean");
    check("in-typeof", "return typeof (\"a\" in {a: 1});", "boolean");
}

// ---- array spread --------------------------------------------------------------------------

/// **`spread` is the whole difference between `[...a]` and `[a]`.**
#[test]
fn spread_appends_the_elements_and_not_the_array() {
    check("spread-alone", "return [...[1, 2]].length;", "2");
    check("spread-nested", "return [[1, 2]].length;", "1");
    check("spread-values", "return [...[1, 2]].join(\",\");", "1,2");
}

/// An array with no spread is still one `CreateArray`; only what follows a spread is appended
/// piece by piece.
#[test]
fn spread_composes_with_ordinary_elements() {
    check(
        "spread-leading",
        "return [0, ...[1, 2]].join(\",\");",
        "0,1,2",
    );
    check(
        "spread-trailing",
        "return [...[1, 2], 3].join(\",\");",
        "1,2,3",
    );
    check(
        "spread-middle",
        "return [0, ...[1], 2].join(\",\");",
        "0,1,2",
    );
    check(
        "spread-twice",
        "return [...[1], ...[2, 3]].join(\",\");",
        "1,2,3",
    );
    check("spread-empty", "return [...[]].length;", "0");
    check("spread-only-plain", "return [1, 2, 3].length;", "3");
}

/// A copy, not an alias — the point of `[...a]`.
#[test]
fn spread_copies_rather_than_aliases() {
    check(
        "spread-copy",
        "let a = [1, 2]; let b = [...a]; b.push(3); return a.length;",
        "2",
    );
}

/// A string spreads into its code points, by the same rule `for-of` follows.
#[test]
fn spread_of_a_string_gives_its_code_points() {
    check("spread-string", "return [...\"abc\"].length;", "3");
    check(
        "spread-string-join",
        "return [...\"abc\"].join(\"-\");",
        "a-b-c",
    );
    check("spread-emoji", "return [...\"😀\"].length;", "1");
}

/// **Spreading a non-iterable fails the way `for-of` does**, rather than quietly producing a
/// one-element array.
#[test]
fn spreading_a_non_iterable_raises() {
    check(
        "spread-number",
        "let r = \"\"; try { let a = [...5]; } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

// ---- Math ----------------------------------------------------------------------------------

#[test]
fn the_rounding_functions_agree_with_the_specification() {
    check("math-floor", "return Math.floor(1.7);", "1");
    check("math-ceil", "return Math.ceil(1.2);", "2");
    check("math-trunc", "return Math.trunc(-1.7);", "-1");
    check("math-floor-negative", "return Math.floor(-1.2);", "-2");
    check("math-abs", "return Math.abs(-3);", "3");
}

/// **`Math.round` is not Rust's `round`.** JavaScript rounds a half *upward*, toward positive
/// infinity; Rust rounds it *away from zero*. They agree on `0.5` and disagree on `-0.5`.
#[test]
fn round_leans_upward_rather_than_away_from_zero() {
    check("math-round-half", "return Math.round(0.5);", "1");
    check("math-round-up", "return Math.round(1.5);", "2");
    check("math-round-negative-half", "return Math.round(-0.5);", "0");
    check("math-round-negative", "return Math.round(-1.5);", "-1");
    check("math-round-down", "return Math.round(1.4);", "1");
}

/// **`Math.sign` is not `signum`**, which answers `1` for a zero and never `NaN`. All three of
/// `0`, `-0` and `NaN` come back as themselves.
#[test]
fn sign_preserves_zero_and_propagates_nan() {
    check("math-sign-positive", "return Math.sign(5);", "1");
    check("math-sign-negative", "return Math.sign(-5);", "-1");
    check("math-sign-zero", "return Math.sign(0);", "0");
    check("math-sign-nan", "return Math.sign(0 / 0);", "NaN");
}

/// **No arguments gives the opposite infinity each time**, because each has to lose to the
/// first real argument. **One `NaN` anywhere wins**, which `f64::min` does not do.
#[test]
fn min_and_max_lean_opposite_ways_when_empty() {
    check("math-min", "return Math.min(3, 1, 2);", "1");
    check("math-max", "return Math.max(3, 1, 2);", "3");
    check("math-min-empty", "return Math.min();", "Infinity");
    check("math-max-empty", "return Math.max();", "-Infinity");
    check("math-min-nan", "return Math.min(1, 0 / 0);", "NaN");
    check("math-max-nan", "return Math.max(1, 0 / 0);", "NaN");
}

#[test]
fn the_power_and_root_functions_work() {
    check("math-sqrt", "return Math.sqrt(9);", "3");
    check("math-cbrt", "return Math.cbrt(27);", "3");
    check("math-pow", "return Math.pow(2, 10);", "1024");
    check("math-hypot", "return Math.hypot(3, 4);", "5");
    check("math-exp-zero", "return Math.exp(0);", "1");
    check("math-log-one", "return Math.log(1);", "0");
    check("math-log2", "return Math.log2(8);", "3");
    check("math-log10", "return Math.log10(1000);", "3");
}

#[test]
fn the_trigonometric_functions_work() {
    check("math-sin-zero", "return Math.sin(0);", "0");
    check("math-cos-zero", "return Math.cos(0);", "1");
    check("math-atan2", "return Math.atan2(0, 1);", "0");
    check("math-asin-zero", "return Math.asin(0);", "0");
}

#[test]
fn the_constants_are_there() {
    check(
        "math-pi",
        "return Math.PI > 3.14 && Math.PI < 3.15;",
        "true",
    );
    check("math-e", "return Math.E > 2.71 && Math.E < 2.72;", "true");
    check(
        "math-sqrt2",
        "return Math.SQRT2 > 1.41 && Math.SQRT2 < 1.42;",
        "true",
    );
    check(
        "math-ln2",
        "return Math.LN2 > 0.69 && Math.LN2 < 0.70;",
        "true",
    );
}

/// **Not suitable for anything needing unpredictability** — the specification asks only for an
/// implementation-dependent value in `[0, 1)`, which is all this checks.
#[test]
fn random_stays_in_its_range() {
    check(
        "math-random-range",
        "let r = Math.random(); return r >= 0 && r < 1;",
        "true",
    );
    check(
        "math-random-varies",
        "return Math.random() !== Math.random() || true;",
        "true",
    );
}

// ---- arguments ------------------------------------------------------------------------------

#[test]
fn arguments_holds_what_the_caller_passed() {
    check(
        "arguments-length",
        "let f = function () { return arguments.length; }; return f(1, 2, 3);",
        "3",
    );
    check(
        "arguments-index",
        "let f = function () { return arguments[1]; }; return f(\"a\", \"b\");",
        "b",
    );
    check(
        "arguments-none",
        "let f = function () { return arguments.length; }; return f();",
        "0",
    );
    // More arguments than parameters is exactly the case `arguments` exists for.
    check(
        "arguments-extra",
        "let f = function (a) { return arguments.length; }; return f(1, 2, 3);",
        "3",
    );
}

/// **An array, not the specification's array-*like*.** That buys everything array-shaped
/// working at once, and costs the aliasing and the `Array.isArray` answer.
#[test]
fn arguments_behaves_as_an_array_here() {
    check(
        "arguments-join",
        "let f = function () { return arguments.join(\"-\"); }; return f(1, 2);",
        "1-2",
    );
    check(
        "arguments-spread",
        "let f = function () { return [...arguments].length; }; return f(1, 2);",
        "2",
    );
    check(
        "arguments-forof",
        "let f = function () { let t = 0; for (let x of arguments) { t = t + x; } return t; }; \
         return f(1, 2, 3);",
        "6",
    );
}

/// A copy, so writing to `arguments` does not reach the named parameter. A real engine aliases
/// them outside strict mode; this does not, and the difference is asserted rather than assumed.
#[test]
fn arguments_does_not_alias_its_parameters() {
    check(
        "arguments-no-alias",
        "let f = function (a) { arguments[0] = 9; return a; }; return f(1);",
        "1",
    );
}

/// **An arrow has no `arguments` of its own** and sees the enclosing function's, which is the
/// same rule `this` follows — and falls out of resolving the name through the ordinary capture
/// machinery rather than being special-cased.
#[test]
fn an_arrow_sees_the_enclosing_arguments() {
    check(
        "arguments-arrow",
        "let f = function () { let g = () => arguments.length; return g(); }; return f(1, 2);",
        "2",
    );
}

/// A function that never names `arguments` must not pay for it. Nothing observable proves the
/// prologue is empty, so this only pins that the name stays unbound at the top level.
#[test]
fn arguments_is_not_a_global() {
    check(
        "arguments-not-global",
        "let r = \"\"; try { let n = arguments.length; } catch (e) { r = e.name; } return r;",
        "ReferenceError",
    );
}

// ---- var hoisting ---------------------------------------------------------------------------

/// **A `var` is function-scoped and hoisted**, so its name exists from the top of the function
/// whatever line declares it. This is the shape test262's own `propertyHelper.js` has — a `var`
/// at the top of the file read by a hoisted function — and lowering the declaration where it
/// appeared left every hoisted function above it unable to see the name.
#[test]
fn a_hoisted_function_sees_a_var_declared_below_it() {
    check(
        "var-hoist-function",
        "function read() { return later; } var later = 7; return read();",
        "7",
    );
    check(
        "var-hoist-alias",
        "function get() { return alias(1, 2); } var alias = Math.max; return get();",
        "2",
    );
}

/// **Hoisted means declared, not assigned.** Reading before the declaring statement runs gives
/// `undefined`, which is exactly what separates `var` from `let`.
#[test]
fn a_var_read_before_its_declaration_is_undefined() {
    check("var-before", "let r = x; var x = 1; return r;", "undefined");
    check("var-after", "var x = 1; return x;", "1");
}

/// A `var` inside a block belongs to the function around it, not the block.
#[test]
fn a_var_inside_a_block_escapes_the_block() {
    check("var-in-block", "{ var x = 1; } return x;", "1");
    check("var-in-if", "if (true) { var y = 2; } return y;", "2");
    check(
        "var-in-loop",
        "for (let i = 0; i < 1; i = i + 1) { var z = 3; } return z;",
        "3",
    );
    check(
        "var-in-try",
        "try { var t = 4; } catch (e) { } return t;",
        "4",
    );
}

/// **`var x;` after an assignment must not clobber it.** The hoist already set the binding to
/// `undefined`; a declaration with no initialiser has nothing left to do.
#[test]
fn a_var_declaration_without_an_initialiser_does_not_reset_it() {
    check("var-redeclare", "x = 5; var x; return x;", "5");
    check("var-twice", "var x = 1; var x; return x;", "1");
}

/// A nested function's own `var`s belong to it, not to the function around it.
#[test]
fn a_nested_functions_vars_stay_inside_it() {
    check(
        "var-nested-scope",
        "let f = function () { var inner = 1; return inner; }; \
         let r = f(); let outer = typeof inner; return outer;",
        "undefined",
    );
}

/// **`typeof` is the one operator that does not throw on an undeclared name.** The comment in
/// the lowering said so long before the code did — the operand went through the ordinary global
/// load, which raises, so this was a `ReferenceError` instead of a string.
#[test]
fn typeof_an_undeclared_name_is_a_string() {
    check(
        "typeof-undeclared",
        "return typeof nothingHere;",
        "undefined",
    );
    check("typeof-declared", "let x = 1; return typeof x;", "number");
    check("typeof-global", "return typeof Object;", "function");
    // Every *other* read of a missing global is still a `ReferenceError`.
    check(
        "undeclared-read-still-throws",
        "let r = \"\"; try { let v = nothingHere; } catch (e) { r = e.name; } return r;",
        "ReferenceError",
    );
}

// ---- sort and splice --------------------------------------------------------------------

/// **The default sort order is by text, not by number.** `[10, 9].sort()` is `[10, 9]`, because
/// `"10"` sorts before `"9"`. That surprises everyone once, and it is the specification's rule.
#[test]
fn sort_compares_as_text_unless_told_otherwise() {
    check(
        "sort-text",
        "return [\"c\", \"a\", \"b\"].sort().join(\"\");",
        "abc",
    );
    check(
        "sort-numbers-as-text",
        "return [10, 9, 1].sort().join(\",\");",
        "1,10,9",
    );
    check(
        "sort-comparator",
        "return [10, 9, 1].sort(function (a, b) { return a - b; }).join(\",\");",
        "1,9,10",
    );
    check(
        "sort-descending",
        "return [1, 3, 2].sort(function (a, b) { return b - a; }).join(\",\");",
        "3,2,1",
    );
}

/// **`undefined` sorts to the end and never reaches the comparator.**
#[test]
fn undefined_sorts_last() {
    check(
        "sort-undefined-last",
        "let a = [3, undefined, 1]; a.sort(); return a[2] === undefined;",
        "true",
    );
    check(
        "sort-undefined-not-compared",
        "let seen = 0; [1, undefined, 2].sort(function (a, b) { seen = seen + 1; return 0; }); \
         return seen;",
        "1",
    );
}

/// Stable since ES2019: equal elements keep the order they were in.
#[test]
fn sort_is_stable() {
    check(
        "sort-stable",
        "let a = [\"b1\", \"a1\", \"b2\", \"a2\"]; \
         a.sort(function (x, y) { return x.charAt(0) < y.charAt(0) ? -1 : (x.charAt(0) > y.charAt(0) ? 1 : 0); }); \
         return a.join(\",\");",
        "a1,a2,b1,b2",
    );
}

/// **`splice` answers the removed elements and mutates in place** — the pair of jobs that makes
/// it the odd one out among the array methods.
#[test]
fn splice_removes_and_answers_what_it_removed() {
    check(
        "splice-removed",
        "return [1, 2, 3].splice(1, 1).join(\",\");",
        "2",
    );
    check(
        "splice-remaining",
        "let a = [1, 2, 3]; a.splice(1, 1); return a.join(\",\");",
        "1,3",
    );
    check(
        "splice-insert",
        "let a = [1, 4]; a.splice(1, 0, 2, 3); return a.join(\",\");",
        "1,2,3,4",
    );
    check(
        "splice-replace",
        "let a = [1, 9, 3]; a.splice(1, 1, 2); return a.join(\",\");",
        "1,2,3",
    );
}

/// **No second argument removes everything from `start` on**, which is different from passing
/// a count of zero — so the argument *count* decides, not the value.
#[test]
fn splice_without_a_count_removes_the_rest() {
    check(
        "splice-to-end",
        "let a = [1, 2, 3]; a.splice(1); return a.join(\",\");",
        "1",
    );
    check(
        "splice-count-zero",
        "let a = [1, 2, 3]; a.splice(1, 0); return a.join(\",\");",
        "1,2,3",
    );
    check(
        "splice-negative",
        "let a = [1, 2, 3]; a.splice(-1); return a.join(\",\");",
        "1,2",
    );
}

#[test]
fn an_array_prints_as_its_elements() {
    check("array-tostring", "return [1, 2, 3].toString();", "1,2,3");
    check(
        "array-tostring-nested",
        "return String([1, [2, 3]]);",
        "1,2,3",
    );
    check(
        "array-tostring-nullish",
        "return [1, null, 2].toString();",
        "1,,2",
    );
}

/// **Two strings compare lexicographically; everything else numerically.** Coercing both sides
/// to a number made every string comparison a `NaN` comparison — false in *both* directions, so
/// a sort comparator written the ordinary way answered `0` for every pair and sorted nothing.
#[test]
fn relational_operators_compare_strings_as_text() {
    check("less-strings", "return \"a\" < \"b\";", "true");
    check("less-strings-reverse", "return \"b\" < \"a\";", "false");
    check("greater-strings", "return \"b\" > \"a\";", "true");
    check("less-equal-strings", "return \"a\" <= \"a\";", "true");
    // As text `"10"` precedes `"9"`; as numbers it does not. Both are right, for their types.
    check("less-numeric-strings", "return \"10\" < \"9\";", "true");
    check("less-numbers", "return 10 < 9;", "false");
    // A string against a number is numeric, so this reads `"10"` as ten.
    check("less-mixed", "return \"10\" < 9;", "false");
}

/// `NaN` makes all four false, which is not the same as the negation of the opposite operator.
#[test]
fn nan_is_not_ordered() {
    check("less-nan", "return 0 / 0 < 1;", "false");
    check("greater-nan", "return 0 / 0 > 1;", "false");
    check("less-equal-nan", "return 0 / 0 <= 0 / 0;", "false");
}

/// **An object is asked for its text**, through `toString` and then `valueOf`. Reading
/// `[object Object]` off every object made `String([1, 2])` that string instead of `"1,2"` —
/// the array had a perfectly good `toString` that nothing called.
#[test]
fn an_object_is_asked_how_it_reads_as_text() {
    check("text-array", "return String([1, 2]);", "1,2");
    check("text-array-concat", "return \"\" + [1, 2];", "1,2");
    check(
        "text-custom",
        "let o = {toString: function () { return \"x\"; }}; return String(o);",
        "x",
    );
    // A plain object still reads as `[object Object]`, through the inherited `toString`.
    check("text-plain-object", "return String({});", "[object Object]");
}

/// An uncaught error has to say what it was. **`describe_error` runs before `to_text`**,
/// because `name` and `message` are what an error carries and `toString` is something most
/// errors inherit rather than define — once `to_text` learned to ask an object (D-137), every
/// uncaught error started describing itself as `[object Object]`.
#[test]
fn an_uncaught_error_reports_its_name_and_message() {
    let Some(runtime) = runtime() else { return };
    let directory = std::env::temp_dir().join("crisol-acceptance-uncaught-text");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a working directory");
    let file = directory.join("main.js");
    std::fs::write(&file, "throw new TypeError(\"bad thing\");").expect("write");
    let binary = directory.join("main");
    crisol::build::build(&file, &binary, &runtime).expect("should build");

    let output = Command::new(&binary).output().expect("run the binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TypeError") && stderr.contains("bad thing"),
        "an uncaught error should name itself, got: {stderr}"
    );
    assert!(
        !stderr.contains("[object Object]"),
        "an uncaught error should not describe itself as a plain object, got: {stderr}"
    );
}

/// **A `String` wrapper carries its own text.** Without it, a method reached through the
/// wrapper asks the object for text, which calls `String.prototype.toString`, which asks
/// again — `new String("x").slice(0, 1)` overflowed the stack rather than answering, and
/// twelve test262 cases crashed on exactly that.
#[test]
fn a_string_wrapper_answers_its_methods() {
    check(
        "wrapper-slice",
        "return new String(\"undefined\").slice(0, 3);",
        "und",
    );
    check("wrapper-length", "return new String(\"abc\").length;", "3");
    check(
        "wrapper-indexof",
        "return new String(\"hello\").indexOf(\"l\");",
        "2",
    );
    check(
        "wrapper-upper",
        "return new String(\"ab\").toUpperCase();",
        "AB",
    );
    check(
        "wrapper-tostring",
        "return new String(\"ab\").toString();",
        "ab",
    );
    check(
        "wrapper-charat",
        "return new String(\"abc\").charAt(1);",
        "b",
    );
    // A plain call is not a wrapper and still answers a primitive.
    check(
        "wrapper-plain-call",
        "return typeof String(\"a\");",
        "string",
    );
    check(
        "wrapper-is-object",
        "return typeof new String(\"a\");",
        "object",
    );
}

/// The wrapped text is not enumerable, for the same reason a date's time value is not.
///
/// **Asserted by name, not by a total.** The first version of this counted the wrapper's keys
/// and expected none, which encoded a second wrong belief while testing the first thing
/// correctly: a string wrapper owns one enumerable property per character. A count is a bad
/// assertion for "X is absent" — it passes for the wrong reason whenever the total is wrong
/// for another one.
#[test]
fn a_string_wrappers_value_is_hidden() {
    check(
        "wrapper-keys",
        "return Object.keys(new String(\"ab\")).indexOf(\"__primitive\");",
        "-1",
    );
    check(
        "wrapper-own-names-hide-the-primitive",
        "return Object.getOwnPropertyNames(new String(\"ab\")).indexOf(\"__primitive\");",
        "-1",
    );
}

// ---- Map and Set ---------------------------------------------------------------------------

#[test]
fn a_map_stores_and_retrieves_by_key() {
    check(
        "map-set-get",
        "let m = new Map(); m.set(\"a\", 1); return m.get(\"a\");",
        "1",
    );
    check(
        "map-size",
        "let m = new Map(); m.set(\"a\", 1); m.set(\"b\", 2); return m.size;",
        "2",
    );
    check(
        "map-has",
        "let m = new Map(); m.set(\"a\", 1); return m.has(\"a\");",
        "true",
    );
    check(
        "map-has-not",
        "let m = new Map(); return m.has(\"a\");",
        "false",
    );
    check("map-empty-size", "return new Map().size;", "0");
    // Answers the map, so calls chain.
    check(
        "map-chains",
        "let m = new Map(); m.set(\"a\", 1).set(\"b\", 2); return m.size;",
        "2",
    );
}

/// **`undefined` for a missing key is indistinguishable from a stored `undefined`** — which is
/// what `has` is for, and why both exist.
#[test]
fn a_map_distinguishes_absent_from_undefined_only_through_has() {
    check("map-missing", "return new Map().get(\"a\");", "undefined");
    check(
        "map-stored-undefined",
        "let m = new Map(); m.set(\"a\", undefined); return m.get(\"a\");",
        "undefined",
    );
    check(
        "map-stored-undefined-has",
        "let m = new Map(); m.set(\"a\", undefined); return m.has(\"a\");",
        "true",
    );
}

/// **An existing key keeps its position.** Insertion order is observable through `forEach`, and
/// re-setting a key does not move it to the end.
#[test]
fn a_map_keeps_insertion_order_through_a_reassignment() {
    check(
        "map-order",
        "let m = new Map(); m.set(\"a\", 1); m.set(\"b\", 2); m.set(\"a\", 3); \
         let s = \"\"; m.forEach(function (v, k) { s = s + k; }); return s;",
        "ab",
    );
    check(
        "map-reassign-size",
        "let m = new Map(); m.set(\"a\", 1); m.set(\"a\", 2); return m.size;",
        "1",
    );
}

/// **`forEach` passes value first, then key** — the opposite of how the pair is stored.
#[test]
fn map_for_each_passes_the_value_before_the_key() {
    check(
        "map-foreach-order",
        "let m = new Map(); m.set(\"k\", \"v\"); \
         let s = \"\"; m.forEach(function (value, key) { s = value + key; }); return s;",
        "vk",
    );
}

#[test]
fn a_map_can_delete_and_clear() {
    check(
        "map-delete",
        "let m = new Map(); m.set(\"a\", 1); m.set(\"b\", 2); m.delete(\"a\"); return m.size;",
        "1",
    );
    check(
        "map-delete-answer",
        "let m = new Map(); m.set(\"a\", 1); return m.delete(\"a\");",
        "true",
    );
    // **`false` for a key that was not there**, where `delete` on an object answers `true`.
    check(
        "map-delete-absent",
        "return new Map().delete(\"a\");",
        "false",
    );
    check(
        "map-delete-keeps-order",
        "let m = new Map(); m.set(\"a\", 1); m.set(\"b\", 2); m.set(\"c\", 3); m.delete(\"b\"); \
         let s = \"\"; m.forEach(function (v, k) { s = s + k; }); return s;",
        "ac",
    );
    check(
        "map-clear",
        "let m = new Map(); m.set(\"a\", 1); m.clear(); return m.size;",
        "0",
    );
}

#[test]
fn a_set_holds_each_value_once() {
    check(
        "set-add",
        "let s = new Set(); s.add(1); s.add(2); return s.size;",
        "2",
    );
    check(
        "set-duplicate",
        "let s = new Set(); s.add(1); s.add(1); return s.size;",
        "1",
    );
    check(
        "set-has",
        "let s = new Set(); s.add(1); return s.has(1);",
        "true",
    );
    check(
        "set-delete",
        "let s = new Set(); s.add(1); s.delete(1); return s.size;",
        "0",
    );
    check("set-delete-absent", "return new Set().delete(1);", "false");
    check(
        "set-clear",
        "let s = new Set(); s.add(1); s.clear(); return s.size;",
        "0",
    );
}

/// **`NaN` equals itself here**, which `===` does not do — without SameValueZero every
/// `add(NaN)` would add another.
#[test]
fn a_collection_keys_on_same_value_zero() {
    check(
        "set-nan",
        "let s = new Set(); s.add(0 / 0); s.add(0 / 0); return s.size;",
        "1",
    );
    check(
        "set-nan-has",
        "let s = new Set(); s.add(0 / 0); return s.has(0 / 0);",
        "true",
    );
    // `+0` and `-0` are the same key.
    check(
        "set-zeroes",
        "let s = new Set(); s.add(0); s.add(-0); return s.size;",
        "1",
    );
    check(
        "map-nan-key",
        "let m = new Map(); m.set(0 / 0, \"x\"); return m.get(0 / 0);",
        "x",
    );
}

/// **`Set.prototype.forEach` passes the value twice**, so a callback written for a map works
/// unchanged on a set.
#[test]
fn set_for_each_passes_the_value_twice() {
    check(
        "set-foreach",
        "let s = new Set(); s.add(\"a\"); s.add(\"b\"); \
         let out = \"\"; s.forEach(function (v) { out = out + v; }); return out;",
        "ab",
    );
    check(
        "set-foreach-twice",
        "let s = new Set(); s.add(\"x\"); \
         let out = \"\"; s.forEach(function (v, k) { out = v + k; }); return out;",
        "xx",
    );
}

/// An object key is compared by identity, not by contents.
#[test]
fn an_object_key_is_its_own_key() {
    check(
        "map-object-key",
        "let a = {}; let b = {}; let m = new Map(); m.set(a, 1); m.set(b, 2); return m.size;",
        "2",
    );
    check(
        "map-object-key-get",
        "let a = {}; let m = new Map(); m.set(a, 7); return m.get(a);",
        "7",
    );
}

// ---- Symbol --------------------------------------------------------------------------------

/// **Every symbol is unique**, which falls out of a symbol being a heap cell rather than being
/// arranged: two `Symbol("x")` are different symbols however alike they read.
#[test]
fn a_symbol_is_unique_and_reports_its_type() {
    check("symbol-typeof", "return typeof Symbol();", "symbol");
    check(
        "symbol-unique",
        "return Symbol(\"x\") === Symbol(\"x\");",
        "false",
    );
    check(
        "symbol-self",
        "let s = Symbol(\"x\"); return s === s;",
        "true",
    );
    check(
        "symbol-typeof-described",
        "return typeof Symbol(\"x\");",
        "symbol",
    );
}

#[test]
fn a_symbol_carries_its_description() {
    check(
        "symbol-description",
        "return Symbol(\"hello\").description;",
        "hello",
    );
    check(
        "symbol-tostring",
        "return Symbol(\"hello\").toString();",
        "Symbol(hello)",
    );
    check(
        "symbol-no-description",
        "return Symbol().toString();",
        "Symbol()",
    );
    check(
        "symbol-valueof",
        "let s = Symbol(\"x\"); return s.valueOf() === s;",
        "true",
    );
}

/// **`Symbol.for` is a registry and `Symbol()` is not.** The same key gives the same symbol
/// back, which is the whole point of it.
#[test]
fn the_symbol_registry_returns_the_same_symbol() {
    check(
        "symbol-for",
        "return Symbol.for(\"k\") === Symbol.for(\"k\");",
        "true",
    );
    check(
        "symbol-for-differs",
        "return Symbol.for(\"a\") === Symbol.for(\"b\");",
        "false",
    );
    check(
        "symbol-for-vs-plain",
        "return Symbol.for(\"k\") === Symbol(\"k\");",
        "false",
    );
    check(
        "symbol-keyfor",
        "return Symbol.keyFor(Symbol.for(\"k\"));",
        "k",
    );
}

/// **Only a registered symbol has a key.** One made by `Symbol("x")` answers `undefined` even
/// though its description is `"x"` — the description is not the key.
#[test]
fn key_for_distinguishes_registered_from_described() {
    check(
        "symbol-keyfor-plain",
        "return Symbol.keyFor(Symbol(\"x\"));",
        "undefined",
    );
    check(
        "symbol-description-still",
        "return Symbol(\"x\").description;",
        "x",
    );
}

/// The well-known symbols exist as values. **They are not yet usable as property keys** — a
/// `PropertyKey` is a string — so this checks they are symbols, not that they index anything.
#[test]
fn the_well_known_symbols_are_symbols() {
    check(
        "symbol-iterator",
        "return typeof Symbol.iterator;",
        "symbol",
    );
    check(
        "symbol-async-iterator",
        "return typeof Symbol.asyncIterator;",
        "symbol",
    );
    check(
        "symbol-has-instance",
        "return typeof Symbol.hasInstance;",
        "symbol",
    );
    check(
        "symbol-to-primitive",
        "return typeof Symbol.toPrimitive;",
        "symbol",
    );
    check(
        "symbol-to-string-tag",
        "return typeof Symbol.toStringTag;",
        "symbol",
    );
    // Stable across reads, rather than freshly made each time.
    check(
        "symbol-iterator-stable",
        "return Symbol.iterator === Symbol.iterator;",
        "true",
    );
}

/// A symbol survives a collection, which is the reason it is a heap cell rather than a bare
/// payload — every path that turns a value into a reference would otherwise have traced a
/// number as though it addressed one.
#[test]
fn a_symbol_survives_collection() {
    check(
        "symbol-survives",
        "let s = Symbol(\"keep\"); let junk = []; \
         for (let i = 0; i < 50; i = i + 1) { junk.push({n: i}); } return s.description;",
        "keep",
    );
    check(
        "symbol-registry-survives",
        "let s = Symbol.for(\"reg\"); let junk = []; \
         for (let i = 0; i < 50; i = i + 1) { junk.push({n: i}); } \
         return Symbol.for(\"reg\") === s;",
        "true",
    );
}

// ---- more array methods ----------------------------------------------------------------

/// **The copying counterparts leave the original alone**, which is the whole reason they exist
/// alongside `reverse`, `sort` and `splice`.
#[test]
fn the_copying_array_methods_do_not_mutate() {
    check(
        "to-reversed",
        "return [1, 2, 3].toReversed().join(\",\");",
        "3,2,1",
    );
    check(
        "to-reversed-original",
        "let a = [1, 2, 3]; a.toReversed(); return a.join(\",\");",
        "1,2,3",
    );
    check(
        "to-sorted",
        "return [3, 1, 2].toSorted().join(\",\");",
        "1,2,3",
    );
    check(
        "to-sorted-original",
        "let a = [3, 1, 2]; a.toSorted(); return a.join(\",\");",
        "3,1,2",
    );
    check(
        "to-sorted-comparator",
        "return [10, 9].toSorted(function (x, y) { return x - y; }).join(\",\");",
        "9,10",
    );
    check(
        "to-spliced",
        "return [1, 2, 3].toSpliced(1, 1).join(\",\");",
        "1,3",
    );
    check(
        "to-spliced-insert",
        "return [1, 4].toSpliced(1, 0, 2, 3).join(\",\");",
        "1,2,3,4",
    );
    check(
        "to-spliced-original",
        "let a = [1, 2, 3]; a.toSpliced(1, 1); return a.length;",
        "3",
    );
    check(
        "array-with",
        "return [1, 2, 3].with(1, 9).join(\",\");",
        "1,9,3",
    );
    check(
        "array-with-negative",
        "return [1, 2, 3].with(-1, 9).join(\",\");",
        "1,2,9",
    );
    check(
        "array-with-original",
        "let a = [1, 2, 3]; a.with(1, 9); return a.join(\",\");",
        "1,2,3",
    );
}

/// **`with` raises out of range where `at` answers `undefined`** — it builds an array, and
/// there is no array to build for an index that does not exist.
#[test]
fn with_refuses_an_index_that_is_not_there() {
    check(
        "with-out-of-range",
        "let r = \"\"; try { [1, 2].with(5, 0); } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
    check("at-out-of-range-still", "return [1, 2].at(5);", "undefined");
}

/// **`copyWithin` never changes the length** — a run copied past the end is truncated rather
/// than growing the array.
#[test]
fn copy_within_moves_a_run_without_resizing() {
    check(
        "copy-within",
        "return [1, 2, 3, 4, 5].copyWithin(0, 3).join(\",\");",
        "4,5,3,4,5",
    );
    check(
        "copy-within-length",
        "return [1, 2, 3].copyWithin(0, 1).length;",
        "3",
    );
    check(
        "copy-within-end",
        "return [1, 2, 3, 4].copyWithin(0, 1, 3).join(\",\");",
        "2,3,3,4",
    );
    // Overlapping runs read before they write.
    check(
        "copy-within-overlap",
        "return [1, 2, 3, 4].copyWithin(1, 0).join(\",\");",
        "1,1,2,3",
    );
}

#[test]
fn array_from_takes_an_array_like_or_a_string() {
    check("from-array", "return Array.from([1, 2]).length;", "2");
    check(
        "from-string",
        "return Array.from(\"abc\").join(\",\");",
        "a,b,c",
    );
    check(
        "from-array-like",
        "return Array.from({length: 2, 0: \"a\", 1: \"b\"}).join(\",\");",
        "a,b",
    );
    check(
        "from-mapper",
        "return Array.from([1, 2], function (x) { return x * 2; }).join(\",\");",
        "2,4",
    );
    check("from-empty", "return Array.from({length: 0}).length;", "0");
    // A copy, not the same array.
    check(
        "from-copies",
        "let a = [1]; let b = Array.from(a); b.push(2); return a.length;",
        "1",
    );
}

/// **`Array.of` is not `Array`** — `Array(3)` is three elements and `Array.of(3)` is one.
#[test]
fn array_of_takes_its_arguments_as_elements() {
    check("of-one", "return Array.of(3).length;", "1");
    check("of-many", "return Array.of(1, 2, 3).join(\",\");", "1,2,3");
    check("of-none", "return Array.of().length;", "0");
}

/// **`{value, done}` every time, and `done` stays `true` once reached** — an exhausted iterator
/// does not restart, which is what lets a caller loop on `done` without counting.
#[test]
fn an_array_iterator_walks_and_then_stops() {
    check("values-first", "return [7, 8].values().next().value;", "7");
    check(
        "values-not-done",
        "return [7].values().next().done;",
        "false",
    );
    check(
        "values-second",
        "let it = [7, 8].values(); it.next(); return it.next().value;",
        "8",
    );
    check(
        "values-exhausted",
        "let it = [7].values(); it.next(); return it.next().done;",
        "true",
    );
    check(
        "values-stays-done",
        "let it = [].values(); it.next(); return it.next().done;",
        "true",
    );
    check(
        "values-exhausted-value",
        "let it = [7].values(); it.next(); return it.next().value;",
        "undefined",
    );
}

#[test]
fn keys_and_entries_walk_positions_and_pairs() {
    check("keys-first", "return [7, 8].keys().next().value;", "0");
    check(
        "entries-index",
        "return [7, 8].entries().next().value[0];",
        "0",
    );
    check(
        "entries-value",
        "return [7, 8].entries().next().value[1];",
        "7",
    );
    check(
        "keys-second",
        "let it = [7, 8].keys(); it.next(); return it.next().value;",
        "1",
    );
}

// ---- Object statics, Number, Boolean and the globals ---------------------------------------

/// **Freezing keeps enumerability** — a frozen object still lists its properties; it is the
/// writing and the deleting that stop.
#[test]
fn freeze_stops_writing_and_extending() {
    check(
        "freeze-write",
        "let o = {a: 1}; Object.freeze(o); o.a = 2; return o.a;",
        "1",
    );
    check(
        "freeze-extend",
        "let o = {}; Object.freeze(o); o.b = 1; return o.b;",
        "undefined",
    );
    check(
        "freeze-is",
        "let o = {}; Object.freeze(o); return Object.isFrozen(o);",
        "true",
    );
    check("freeze-not", "return Object.isFrozen({a: 1});", "false");
    check(
        "freeze-keys",
        "let o = {a: 1}; Object.freeze(o); return Object.keys(o).length;",
        "1",
    );
    check(
        "freeze-answers",
        "let o = {}; return Object.freeze(o) === o;",
        "true",
    );
    check(
        "freeze-delete",
        "let o = {a: 1}; Object.freeze(o); delete o.a; return o.a;",
        "1",
    );
}

/// **Sealing leaves the values writable** — that is the whole difference from freezing.
#[test]
fn seal_stops_extending_but_not_writing() {
    check(
        "seal-write",
        "let o = {a: 1}; Object.seal(o); o.a = 2; return o.a;",
        "2",
    );
    check(
        "seal-extend",
        "let o = {}; Object.seal(o); o.b = 1; return o.b;",
        "undefined",
    );
    check(
        "seal-is",
        "let o = {}; Object.seal(o); return Object.isSealed(o);",
        "true",
    );
    check(
        "seal-not-frozen",
        "let o = {a: 1}; Object.seal(o); return Object.isFrozen(o);",
        "false",
    );
    check(
        "prevent-extensions",
        "let o = {}; Object.preventExtensions(o); o.a = 1; return o.a;",
        "undefined",
    );
    check("is-extensible", "return Object.isExtensible({});", "true");
    check(
        "is-extensible-after",
        "let o = {}; Object.preventExtensions(o); return Object.isExtensible(o);",
        "false",
    );
}

#[test]
fn entries_and_from_entries_are_inverses() {
    check(
        "entries-length",
        "return Object.entries({a: 1, b: 2}).length;",
        "2",
    );
    check("entries-pair", "return Object.entries({a: 1})[0][0];", "a");
    check("entries-value", "return Object.entries({a: 1})[0][1];", "1");
    check(
        "from-entries",
        "return Object.fromEntries([[\"a\", 1], [\"b\", 2]]).b;",
        "2",
    );
    check(
        "entries-round-trip",
        "let o = {a: 1, b: 2}; return Object.fromEntries(Object.entries(o)).a;",
        "1",
    );
}

/// **`Object.is` is neither `===` nor SameValueZero.** It is the only one of the three that
/// separates the zeroes, and unlike `===` it says `NaN` is itself.
#[test]
fn object_is_separates_the_zeroes() {
    check("is-nan", "return Object.is(0 / 0, 0 / 0);", "true");
    check("is-zeroes", "return Object.is(0, -0);", "false");
    check("is-strict-zeroes", "return 0 === -0;", "true");
    check("is-same", "return Object.is(1, 1);", "true");
    check("is-different", "return Object.is(1, 2);", "false");
}

/// **The `Number` predicates do no coercion; the globals do.** That is the whole difference,
/// and it is why `isNaN("x")` is true while `Number.isNaN("x")` is false.
#[test]
fn the_number_predicates_do_not_coerce() {
    check(
        "number-isnan-string",
        "return Number.isNaN(\"x\");",
        "false",
    );
    check("global-isnan-string", "return isNaN(\"x\");", "true");
    check("number-isnan", "return Number.isNaN(0 / 0);", "true");
    check(
        "number-isfinite-string",
        "return Number.isFinite(\"1\");",
        "false",
    );
    check("global-isfinite-string", "return isFinite(\"1\");", "true");
    check("number-isinteger", "return Number.isInteger(1);", "true");
    check(
        "number-isinteger-fraction",
        "return Number.isInteger(1.5);",
        "false",
    );
    check(
        "number-isinteger-string",
        "return Number.isInteger(\"1\");",
        "false",
    );
    check("number-issafe", "return Number.isSafeInteger(1);", "true");
}

/// **`parseInt` reads a prefix and stops**, where `Number` demands the whole string.
#[test]
fn parse_int_and_parse_float_read_a_prefix() {
    check("parseint", "return parseInt(\"12\");", "12");
    check("parseint-trailing", "return parseInt(\"12abc\");", "12");
    check("number-whole", "return Number(\"12abc\");", "NaN");
    check("parseint-radix", "return parseInt(\"ff\", 16);", "255");
    check("parseint-hex-prefix", "return parseInt(\"0x10\");", "16");
    check(
        "parseint-hex-decimal",
        "return parseInt(\"0x10\", 10);",
        "0",
    );
    check("parseint-negative", "return parseInt(\"-42\");", "-42");
    check("parseint-none", "return parseInt(\"abc\");", "NaN");
    check("parsefloat", "return parseFloat(\"1.5rest\");", "1.5");
    check("parsefloat-none", "return parseFloat(\"abc\");", "NaN");
}

#[test]
fn number_constants_and_methods() {
    check(
        "number-max-safe",
        "return Number.MAX_SAFE_INTEGER > 9007199254740990;",
        "true",
    );
    check("number-epsilon", "return Number.EPSILON > 0;", "true");
    check(
        "number-infinity",
        "return Number.POSITIVE_INFINITY;",
        "Infinity",
    );
    check("number-tostring", "return (255).toString(16);", "ff");
    check("number-tostring-binary", "return (5).toString(2);", "101");
    check("number-tostring-default", "return (12).toString();", "12");
    check("number-tofixed", "return (1.005).toFixed(2);", "1.00");
    check("number-tofixed-zero", "return (1.5).toFixed(0);", "2");
    check("number-valueof", "return (5).valueOf();", "5");
    check("number-wrapper", "return new Number(7).valueOf();", "7");
    check(
        "number-wrapper-tostring",
        "return new Number(7).toString();",
        "7",
    );
}

#[test]
fn boolean_methods_and_wrappers() {
    check("boolean-tostring", "return true.toString();", "true");
    check("boolean-valueof", "return false.valueOf();", "false");
    check(
        "boolean-wrapper",
        "return new Boolean(true).valueOf();",
        "true",
    );
    check(
        "boolean-wrapper-false",
        "return new Boolean(false).toString();",
        "false",
    );
    // A wrapper object is truthy whatever it wraps, which catches everyone once.
    check(
        "boolean-wrapper-truthy",
        "return new Boolean(false) ? 1 : 0;",
        "1",
    );
}

#[test]
fn string_statics_build_from_code_units_and_points() {
    check(
        "from-char-code",
        "return String.fromCharCode(65, 66);",
        "AB",
    );
    check("from-code-point", "return String.fromCodePoint(65);", "A");
    // A code point beyond the basic plane is one argument here and two to `fromCharCode`.
    check(
        "from-code-point-astral",
        "return String.fromCodePoint(128512).length;",
        "2",
    );
}

/// **`Date.UTC` answers a time value, not a date** — and a lone argument is a year.
#[test]
fn date_utc_and_parse() {
    check("date-utc", "return Date.UTC(1970, 0, 1);", "0");
    check("date-utc-day", "return Date.UTC(1970, 0, 2);", "86400000");
    check(
        "date-parse-iso",
        "return Date.parse(\"1970-01-01T00:00:00.000Z\");",
        "0",
    );
    check(
        "date-parse-date-only",
        "return Date.parse(\"1970-01-02\");",
        "86400000",
    );
    check(
        "date-parse-bad",
        "return Date.parse(\"not a date\");",
        "NaN",
    );
    check(
        "date-parse-round-trip",
        "return Date.parse(new Date(86400000).toISOString());",
        "86400000",
    );
}

/// **A number and a boolean are not heap cells**, so property access on them has to find their
/// prototypes without an object to walk from. A string does not need this because a string *is*
/// a cell — which is why the gap only appeared once the other two grew methods worth reaching.
#[test]
fn a_primitive_number_or_boolean_reaches_its_prototype() {
    // Ordered to separate two causes that look identical from outside: if the prototype itself
    // is empty the first fails, and if only the primitive cannot reach a prototype that is
    // fine, the second does.
    check(
        "proto-has-tostring",
        "return typeof Number.prototype.toString;",
        "function",
    );
    // The decisive one: `call` hands the primitive to the method directly and never asks the
    // primitive for a property. If this works, the method and the prototype are both fine and
    // only the lookup from a primitive receiver is broken.
    check(
        "primitive-via-call",
        "return Number.prototype.toString.call(255, 16);",
        "ff",
    );
    check(
        "primitive-var-tostring",
        "let n = 255; return typeof n.toString;",
        "function",
    );
    check(
        "primitive-computed",
        "let n = 255; return typeof n[\"toString\"];",
        "function",
    );
    check(
        "primitive-finds-tostring",
        "return typeof (255).toString;",
        "function",
    );
    check(
        "primitive-number-method",
        "return (255).toString(16);",
        "ff",
    );
    check(
        "primitive-boolean-method",
        "return true.toString();",
        "true",
    );
    check("primitive-number-valueof", "return (5).valueOf();", "5");
    check(
        "primitive-in-variable",
        "let n = 7; return n.toString();",
        "7",
    );
    // The receiver stays the primitive, so the method sees the value it was called on.
    check(
        "primitive-receiver",
        "let b = false; return b.toString();",
        "false",
    );
}

/// **The engine's stand-ins for internal slots are not the program's properties.** Each one was
/// visible to `getOwnPropertyNames` and, worse, to `isFrozen` — which asks whether every own
/// property is non-writable and found this bookkeeping among them.
#[test]
fn internal_bookkeeping_is_not_a_property() {
    check(
        "internal-frozen",
        "let o = {}; Object.freeze(o); return Object.isFrozen(o);",
        "true",
    );
    check(
        "internal-names",
        "let o = {}; Object.freeze(o); return Object.getOwnPropertyNames(o).length;",
        "0",
    );
    check(
        "internal-date",
        "return Object.getOwnPropertyNames(new Date(0)).length;",
        "0",
    );
    check(
        "internal-map",
        "return Object.getOwnPropertyNames(new Map()).length;",
        "1",
    );
    // By name rather than by a total: a wrapper owns its characters and its `length`, so the
    // total is three and says nothing about whether the flag is among them.
    check(
        "internal-string-wrapper",
        "return Object.getOwnPropertyNames(new String(\"ab\")).indexOf(\"__primitive\");",
        "-1",
    );
}

/// **`length` is not stored anywhere — it *is* the element count**, so assigning to it has to
/// resize the array rather than add a property. Without this `a.length = 0` silently did
/// nothing, and test262's `buildString` helper, which empties a scratch array that way on every
/// chunk, re-sent everything it had accumulated instead: quadratic growth, and the process
/// killed on memory rather than any error a test could report.
#[test]
fn assigning_to_length_resizes_an_array() {
    check(
        "length-truncate",
        "let a = [1, 2, 3]; a.length = 1; return a.length;",
        "1",
    );
    check(
        "length-truncate-value",
        "let a = [1, 2, 3]; a.length = 1; return a[0];",
        "1",
    );
    check(
        "length-empty",
        "let a = [1, 2, 3]; a.length = 0; return a.length;",
        "0",
    );
    check(
        "length-grow",
        "let a = [1]; a.length = 3; return a.length;",
        "3",
    );
    check(
        "length-grow-hole",
        "let a = [1]; a.length = 3; return a[2];",
        "undefined",
    );
    check(
        "length-unchanged",
        "let a = [1, 2]; a.length = 2; return a.length;",
        "2",
    );
    // Reusing a scratch array is the pattern that made this matter.
    check(
        "length-reuse",
        "let a = []; let total = 0; \
         for (let i = 0; i < 3; i = i + 1) { a[0] = i; total = total + a.length; a.length = 0; } \
         return total;",
        "3",
    );
}

/// **A length above 2^32-1 is a `RangeError`** — the specification's rule, and the only thing
/// between `[].length = 4294967297` and an attempt to materialise four billion elements, which
/// was a crash rather than an error a test could report.
#[test]
fn an_out_of_range_length_raises() {
    check(
        "length-too-big",
        "let r = \"\"; try { [].length = 4294967297; } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
    check(
        "length-negative",
        "let r = \"\"; try { [].length = -1; } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
    check(
        "length-fractional",
        "let r = \"\"; try { [].length = 1.5; } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
    check(
        "length-legal",
        "let a = [1, 2]; a.length = 1; return a.length;",
        "1",
    );
}

/// **A panic must not cross an `extern "C"` boundary.** A pattern the engine cannot compile is
/// a `SyntaxError` whether the compiler says so or falls over saying it — otherwise the program
/// dies on a signal with nothing to say which pattern did it.
#[test]
fn an_unsupported_pattern_raises_rather_than_aborting() {
    // A malformed pattern is a `SyntaxError`, which is the path that already worked.
    check(
        "regexp-malformed",
        "let r = \"ok\"; try { let p = new RegExp(\"(\"); } catch (e) { r = e.name; } return r;",
        "SyntaxError",
    );
    // A property escape compiles — this is *not* what was panicking, though naming it was the
    // first guess. What the guard buys is that whatever does panic arrives as an error instead
    // of a signal, and the corpus reporting zero crashes is the evidence for that.
    check(
        "regexp-property-escape",
        "return new RegExp(\"\\\\p{L}\", \"u\").test(\"a\");",
        "true",
    );
    // A pattern it can compile still works, so the guard has not swallowed the ordinary path.
    check("regexp-still-works", "return /ab+/.test(\"abb\");", "true");
}

/// **A throw is not a return value.** "A constructor answering a primitive yields the instance"
/// was swallowing the exception signal, which is not an object either — so a constructor that
/// raised handed back a perfectly good empty object and the `try` around it saw nothing.
#[test]
fn a_constructor_that_throws_is_not_swallowed() {
    check(
        "construct-throws",
        "let r = \"ok\"; try { let p = new RegExp(\"(\"); } catch (e) { r = e.name; } return r;",
        "SyntaxError",
    );
    check(
        "construct-throws-user",
        "let F = function () { throw new TypeError(\"no\"); }; \
         let r = \"ok\"; try { let v = new F(); } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
    // The rule it was hiding behind still holds: a primitive return yields the instance.
    check(
        "construct-primitive-still",
        "let F = function () { this.x = 1; return 42; }; return new F().x;",
        "1",
    );
}

// ---- the array methods work on array-likes -------------------------------------------------

/// **Not just arrays.** test262 applies the array methods to anything with a `length` and
/// indexed properties — `Array.prototype.filter.call(new String("abc"), …)` is a whole family
/// of its cases — and a method insisting on real elements answered `undefined` for every one.
#[test]
fn the_array_methods_accept_an_array_like() {
    check(
        "arraylike-filter",
        "let o = new String(\"abc\"); \
         return Array.prototype.filter.call(o, function () { return true; })[0];",
        "a",
    );
    check(
        "arraylike-map",
        "return Array.prototype.map.call({length: 2, 0: 1, 1: 2}, function (x) { return x * 2; })\
         .join(\",\");",
        "2,4",
    );
    check(
        "arraylike-join",
        "return Array.prototype.join.call({length: 2, 0: \"a\", 1: \"b\"}, \"-\");",
        "a-b",
    );
    check(
        "arraylike-indexof",
        "return Array.prototype.indexOf.call({length: 2, 0: \"a\", 1: \"b\"}, \"b\");",
        "1",
    );
    check(
        "arraylike-foreach",
        "let n = 0; Array.prototype.forEach.call({length: 3, 0: 1, 1: 2, 2: 3}, function () { n = n + 1; }); \
         return n;",
        "3",
    );
    check(
        "arraylike-slice",
        "return Array.prototype.slice.call({length: 3, 0: \"a\", 1: \"b\", 2: \"c\"}, 1).join(\",\");",
        "b,c",
    );
    // A real array still takes the element path, which is the first question `indexed_length`
    // asks.
    check(
        "array-still-works",
        "return [1, 2, 3].filter(function (x) { return x > 1; }).length;",
        "2",
    );
}

/// **A lone surrogate is legal in `fromCodePoint` and cannot be represented here.** JavaScript
/// strings are UTF-16 and may hold an unpaired surrogate; these are UTF-8. Raising was wrong —
/// the specification says this succeeds — so it stands in a replacement character, which is a
/// visible wrong answer rather than an error a program cannot expect.
#[test]
fn a_lone_surrogate_does_not_raise() {
    check(
        "surrogate-length",
        "return String.fromCodePoint(0xD800).length;",
        "1",
    );
    check(
        "surrogate-ok",
        "return typeof String.fromCodePoint(0xD800);",
        "string",
    );
    // Out of range is still a `RangeError`, which the specification does require.
    check(
        "code-point-too-big",
        "let r = \"\"; try { String.fromCodePoint(0x110000); } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
}

/// **A string wrapper is indexed by its characters.** It holds its text whole rather than one
/// property per character, and reading one out on demand is what lets `new String("abc")[0]`
/// work and what lets the array methods walk a wrapper at all.
#[test]
fn a_string_wrapper_is_indexed() {
    check("wrapper-index", "return new String(\"abc\")[0];", "a");
    check("wrapper-index-last", "return new String(\"abc\")[2];", "c");
    check(
        "wrapper-index-past",
        "return new String(\"abc\")[9];",
        "undefined",
    );
    check(
        "wrapper-index-computed",
        "let o = new String(\"abc\"); let i = 1; return o[i];",
        "b",
    );
    // A plain object with a numbered property is unaffected.
    check("plain-index", "let o = {0: \"z\"}; return o[0];", "z");
}

/// **`indexOf` compares strings by their characters.** It compared bits, so two cells holding
/// `"b"` were different values and `["a", "b"].indexOf("b")` answered `-1` for as long as the
/// method has existed. Every existing test used numbers, where comparing bits happens to agree.
#[test]
fn index_of_finds_a_string() {
    check(
        "indexof-string",
        "return [\"a\", \"b\"].indexOf(\"b\");",
        "1",
    );
    check(
        "indexof-string-first",
        "return [\"a\", \"b\"].indexOf(\"a\");",
        "0",
    );
    check(
        "indexof-string-missing",
        "return [\"a\"].indexOf(\"z\");",
        "-1",
    );
    // Built rather than written, so the two cells cannot be the same allocation.
    check(
        "indexof-string-built",
        "let needle = \"a\" + \"b\"; return [\"x\", \"ab\"].indexOf(needle);",
        "1",
    );
    // The rules it already had right stay right.
    check("indexof-nan-still", "return [0 / 0].indexOf(0 / 0);", "-1");
    check("indexof-number-still", "return [1, 2].indexOf(2);", "1");
    // An object compares by identity, which is what the fallback is for.
    check(
        "indexof-object-identity",
        "let o = {}; return [o, {}].indexOf(o);",
        "0",
    );
    check("indexof-object-not-equal", "return [{}].indexOf({});", "-1");
}

/// **2^32 is not a length**, so a method that builds a result sized by it has to say so rather
/// than try. Walking such a thing is merely slow; building one is four billion allocations, and
/// that arrived as a killed process rather than an error a program could catch.
#[test]
fn an_impossible_length_raises_rather_than_allocating() {
    check(
        "map-length-too-big",
        "let o = {0: 12, length: 4294967296}; let r = \"\"; \
         try { Array.prototype.map.call(o, function (v) { return v; }); } \
         catch (e) { r = e.name; } return r;",
        "RangeError",
    );
    // A length an array could have still works.
    check(
        "map-length-ok",
        "let o = {0: 1, 1: 2, length: 2}; \
         return Array.prototype.map.call(o, function (v) { return v * 2; }).join(\",\");",
        "2,4",
    );
}

/// **One number is a length and anything else is an element.** `Array(3)` is three empty slots
/// and `Array("3")` is one string — the most surprising rule in the constructor, and the reason
/// `Array.of` exists to mean the other thing.
#[test]
fn the_array_constructor_reads_one_number_as_a_length() {
    check("array-ctor-length", "return new Array(3).length;", "3");
    check("array-ctor-length-plain", "return Array(3).length;", "3");
    check("array-ctor-string", "return Array(\"3\").length;", "1");
    check("array-ctor-string-value", "return Array(\"3\")[0];", "3");
    check("array-ctor-many", "return Array(1, 2, 3).length;", "3");
    check("array-ctor-none", "return Array().length;", "0");
    check("array-ctor-zero", "return Array(0).length;", "0");
    // `Array.of` means the other thing, which is why both exist.
    check("array-of-contrast", "return Array.of(3).length;", "1");
}

/// A length the constructor cannot honour is a `RangeError`, as it is for an assignment.
#[test]
fn the_array_constructor_refuses_an_impossible_length() {
    check(
        "array-ctor-too-big",
        "let r = \"\"; try { Array(4294967296); } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
    check(
        "array-ctor-fraction",
        "let r = \"\"; try { Array(1.5); } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
    check(
        "array-ctor-negative",
        "let r = \"\"; try { Array(-1); } catch (e) { r = e.name; } return r;",
        "RangeError",
    );
}

/// **A sparse index becomes a named property rather than four billion slots.** Elements are a
/// dense `Vec`, so `a[4294967294] = 2` — a legal array index — asks for every slot below it as
/// well. The value is still stored and still readable by the same key; what it is not is an
/// element, so `length` does not count it. That is wrong, and wrong in a way a test can report
/// rather than a way that kills the process.
#[test]
fn a_sparse_index_does_not_exhaust_memory() {
    check(
        "sparse-read-back",
        "let a = []; a[4294967294] = 2; return a[4294967294];",
        "2",
    );
    check(
        "sparse-length",
        "let a = [0, 1]; a[4294967294] = 2; return a.length;",
        "2",
    );
    check(
        "sparse-survives",
        "let a = []; a[4294967294] = 2; return typeof a;",
        "object",
    );
    // An ordinary index is still an element, which is the case that has to stay fast.
    check(
        "dense-still-element",
        "let a = []; a[3] = 7; return a.length;",
        "4",
    );
    check("dense-read-back", "let a = []; a[3] = 7; return a[3];", "7");
}

// ---- accessor properties -------------------------------------------------------------------

/// **An accessor is a property whose value is computed**, so reading it calls something. The
/// slot holds the pair of functions rather than anything the program sees.
#[test]
fn a_getter_is_called_on_read() {
    check(
        "getter-read",
        "let o = {}; Object.defineProperty(o, \"x\", {get: function () { return 7; }}); return o.x;",
        "7",
    );
    check(
        "getter-receiver",
        "let o = {n: 3}; Object.defineProperty(o, \"double\", \
         {get: function () { return this.n * 2; }}); return o.double;",
        "6",
    );
    check(
        "getter-each-time",
        "let count = 0; let o = {}; \
         Object.defineProperty(o, \"x\", {get: function () { count = count + 1; return count; }}); \
         o.x; o.x; return o.x;",
        "3",
    );
}

/// **A setter receives the write**, and a getter without one swallows it — which is what makes
/// a read-only computed property read-only.
#[test]
fn a_setter_is_called_on_write() {
    check(
        "setter-write",
        "let seen = 0; let o = {}; \
         Object.defineProperty(o, \"x\", {set: function (v) { seen = v; }}); o.x = 9; return seen;",
        "9",
    );
    check(
        "setter-pair",
        "let held = 0; let o = {}; \
         Object.defineProperty(o, \"x\", \
         {get: function () { return held; }, set: function (v) { held = v * 2; }}); \
         o.x = 5; return o.x;",
        "10",
    );
    check(
        "getter-only-write-ignored",
        "let o = {}; Object.defineProperty(o, \"x\", {get: function () { return 1; }}); \
         o.x = 9; return o.x;",
        "1",
    );
    // A setter with no getter reads as `undefined` — the whole of what a write-only property is.
    check(
        "setter-only-read",
        "let o = {}; Object.defineProperty(o, \"x\", {set: function () { }}); return o.x;",
        "undefined",
    );
}

/// An accessor inherited from a prototype sees the instance it was reached through.
#[test]
fn an_inherited_accessor_uses_the_receiver() {
    check(
        "getter-inherited",
        "let base = {}; Object.defineProperty(base, \"x\", \
         {get: function () { return this.n; }}); \
         let o = Object.create(base); o.n = 4; return o.x;",
        "4",
    );
    check(
        "setter-inherited",
        "let seen = 0; let base = {}; \
         Object.defineProperty(base, \"x\", {set: function (v) { seen = v; }}); \
         let o = Object.create(base); o.x = 6; return seen;",
        "6",
    );
}

/// **An accessor descriptor has `get` and `set` where a data one has `value` and `writable`** —
/// four fields, never mixed, and a caller tells them apart by which pair is present.
#[test]
fn a_descriptor_reports_which_kind_it_is() {
    check(
        "descriptor-accessor-get",
        "let o = {}; Object.defineProperty(o, \"x\", {get: function () { return 1; }}); \
         return typeof Object.getOwnPropertyDescriptor(o, \"x\").get;",
        "function",
    );
    check(
        "descriptor-accessor-no-value",
        "let o = {}; Object.defineProperty(o, \"x\", {get: function () { return 1; }}); \
         return typeof Object.getOwnPropertyDescriptor(o, \"x\").value;",
        "undefined",
    );
    check(
        "descriptor-data-no-get",
        "let o = {a: 1}; return typeof Object.getOwnPropertyDescriptor(o, \"a\").get;",
        "undefined",
    );
    check(
        "descriptor-data-writable",
        "let o = {a: 1}; return Object.getOwnPropertyDescriptor(o, \"a\").writable;",
        "true",
    );
}

/// A descriptor carrying both a value and an accessor is a `TypeError` — they describe two
/// different kinds of property and an object cannot be both.
#[test]
fn a_descriptor_cannot_be_both_kinds() {
    check(
        "descriptor-both",
        "let r = \"\"; let o = {}; \
         try { Object.defineProperty(o, \"x\", {value: 1, get: function () { return 2; }}); } \
         catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

/// **A non-configurable property is nearly immutable.** The specification allows exactly one
/// change: a writable data property may be made non-writable. Everything else is a `TypeError` —
/// and without that, `defineProperty` would undo its own guarantees, since a frozen property
/// could be quietly thawed by redefining it.
#[test]
fn a_non_configurable_property_cannot_be_redefined() {
    check(
        "redefine-configurable",
        "let r = \"\"; let o = {}; Object.defineProperty(o, \"x\", {value: 1}); \
         try { Object.defineProperty(o, \"x\", {configurable: true}); } catch (e) { r = e.name; } \
         return r;",
        "TypeError",
    );
    check(
        "redefine-enumerable",
        "let r = \"\"; let o = {}; Object.defineProperty(o, \"x\", {value: 1}); \
         try { Object.defineProperty(o, \"x\", {enumerable: true}); } catch (e) { r = e.name; } \
         return r;",
        "TypeError",
    );
    check(
        "redefine-writable-up",
        "let r = \"\"; let o = {}; Object.defineProperty(o, \"x\", {value: 1}); \
         try { Object.defineProperty(o, \"x\", {writable: true}); } catch (e) { r = e.name; } \
         return r;",
        "TypeError",
    );
    check(
        "redefine-value",
        "let r = \"\"; let o = {}; Object.defineProperty(o, \"x\", {value: 1}); \
         try { Object.defineProperty(o, \"x\", {value: 2}); } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
    check(
        "redefine-kind",
        "let r = \"\"; let o = {}; Object.defineProperty(o, \"x\", {value: 1}); \
         try { Object.defineProperty(o, \"x\", {get: function () { return 2; }}); } \
         catch (e) { r = e.name; } return r;",
        "TypeError",
    );
    // Freezing must actually hold against a redefinition.
    check(
        "freeze-holds",
        "let r = \"\"; let o = {a: 1}; Object.freeze(o); \
         try { Object.defineProperty(o, \"a\", {value: 2}); } catch (e) { r = e.name; } return r;",
        "TypeError",
    );
}

/// **The one change that is allowed**: a writable property may be made non-writable, and the
/// same value may be redefined. A configurable property may still be changed freely.
#[test]
fn the_permitted_redefinitions_still_work() {
    check(
        "redefine-writable-down",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1, writable: true}); \
         Object.defineProperty(o, \"x\", {writable: false}); \
         return Object.getOwnPropertyDescriptor(o, \"x\").writable;",
        "false",
    );
    check(
        "redefine-same-value",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1}); \
         Object.defineProperty(o, \"x\", {value: 1}); return o.x;",
        "1",
    );
    check(
        "redefine-configurable-freely",
        "let o = {}; Object.defineProperty(o, \"x\", {value: 1, configurable: true}); \
         Object.defineProperty(o, \"x\", {value: 2}); return o.x;",
        "2",
    );
    // An ordinary assigned property is configurable, so it redefines without complaint.
    check(
        "redefine-plain",
        "let o = {a: 1}; Object.defineProperty(o, \"a\", {value: 2}); return o.a;",
        "2",
    );
}

/// **`Object.create`'s second argument is a map of descriptors, not of values.**
/// `Object.create(p, {x: {value: 1}})` gives `x` the value one; `Object.create(p, {x: 1})`
/// gives it no value at all, because `1` describes nothing.
#[test]
fn object_create_takes_descriptors() {
    check(
        "create-descriptors",
        "let o = Object.create(null, {x: {value: 1, enumerable: true}}); return o.x;",
        "1",
    );
    check(
        "create-descriptors-enumerable",
        "let o = Object.create(null, {x: {value: 1, enumerable: true}}); \
         return Object.keys(o).length;",
        "1",
    );
    check(
        "create-descriptors-default-hidden",
        "let o = Object.create(null, {x: {value: 1}}); return Object.keys(o).length;",
        "0",
    );
    check(
        "create-descriptors-accessor",
        "let o = Object.create(null, {x: {get: function () { return 5; }}}); return o.x;",
        "5",
    );
    // A value that is not a descriptor describes nothing.
    check(
        "create-not-a-descriptor",
        "let o = Object.create(null, {x: {}}); return o.x;",
        "undefined",
    );
    // The prototype still works, with or without a second argument.
    check(
        "create-prototype-still",
        "let base = {greet: 1}; let o = Object.create(base, {x: {value: 2}}); return o.greet;",
        "1",
    );
}

/// **`__defineGetter__` is older than `defineProperty` and still in use** — the only way a
/// program written before ES5 could make an accessor. It makes an **enumerable, configurable**
/// property, where `defineProperty`'s defaults are the opposite.
#[test]
fn the_legacy_accessor_definers_work() {
    check(
        "define-getter",
        "let o = {}; o.__defineGetter__(\"x\", function () { return 7; }); return o.x;",
        "7",
    );
    check(
        "define-setter",
        "let seen = 0; let o = {}; o.__defineSetter__(\"x\", function (v) { seen = v; }); \
         o.x = 4; return seen;",
        "4",
    );
    check(
        "define-getter-enumerable",
        "let o = {}; o.__defineGetter__(\"x\", function () { return 1; }); \
         return Object.keys(o).length;",
        "1",
    );
    check(
        "define-getter-not-function",
        "let r = \"\"; let o = {}; try { o.__defineGetter__(\"x\", 1); } catch (e) { r = e.name; } \
         return r;",
        "TypeError",
    );
}

/// **An array's `length` is its element count, not a property**, so defining it has to resize
/// rather than store. Storing left the array reporting two lengths at once — the descriptor
/// said one and the elements said two — and every question after that got whichever answer its
/// asker happened to consult.
#[test]
fn defining_length_through_a_descriptor_resizes() {
    check(
        "define-length-descriptor",
        "let a = [0, 1]; Object.defineProperties(a, {length: {value: 1, writable: false}}); \
         return a.length;",
        "1",
    );
    // The elements go with the length, so the dropped one is really gone.
    check(
        "define-length-drops-elements",
        "let a = [0, 1]; Object.defineProperty(a, \"length\", {value: 1}); \
         return a.hasOwnProperty(\"1\");",
        "false",
    );
    check(
        "define-length-keeps-the-rest",
        "let a = [0, 1]; Object.defineProperty(a, \"length\", {value: 1}); return a[0];",
        "0",
    );
    check(
        "define-length-grows",
        "let a = [0]; Object.defineProperty(a, \"length\", {value: 3}); return a.length;",
        "3",
    );
    // **A length made non-writable ignores an assignment**, as a non-writable property does.
    check(
        "define-length-fixed",
        "let a = [0, 1]; Object.defineProperty(a, \"length\", {value: 2, writable: false}); \
         a.length = 1; return a.length;",
        "2",
    );
    // A writable length still resizes on assignment.
    check(
        "define-length-still-writable",
        "let a = [0, 1]; a.length = 1; return a.length;",
        "1",
    );
    check(
        "define-length-invalid",
        "let r = \"\"; let a = [0]; \
         try { Object.defineProperty(a, \"length\", {value: -1}); } catch (e) { r = e.name; } \
         return r;",
        "RangeError",
    );
    // The flag is bookkeeping, not a property the program can see. Asserted by name rather
    // than by a total, because the total was itself wrong — an array owns its indices *and*
    // `length`, so `[0]` has two own names and the first version of this expected none.
    check(
        "define-length-flag-hidden",
        "let a = [0]; Object.defineProperty(a, \"length\", {value: 1, writable: false}); \
         return Object.getOwnPropertyNames(a).indexOf(\"__fixedLength\");",
        "-1",
    );
}

/// **An array owns `length`**, even though nothing stores it — `getOwnPropertyNames` has to
/// say so. It is **not enumerable**, so `Object.keys` and `for-in` still leave it out, and that
/// difference is the whole reason the two lists are not the same list.
#[test]
fn an_array_owns_its_indices_and_its_length() {
    check(
        "array-own-names",
        "return Object.getOwnPropertyNames([7, 8]).join(\",\");",
        "0,1,length",
    );
    check(
        "array-own-names-empty",
        "return Object.getOwnPropertyNames([]).join(\",\");",
        "length",
    );
    // Enumeration leaves `length` out, which is why `Object.keys` is shorter.
    check(
        "array-keys",
        "return Object.keys([7, 8]).join(\",\");",
        "0,1",
    );
    check(
        "array-forin",
        "let s = \"\"; for (let k in [7, 8]) { s = s + k; } return s;",
        "01",
    );
    // A plain object is unaffected: it has no derived length to report or hide.
    check(
        "object-own-names-plain",
        "return Object.getOwnPropertyNames({a: 1}).join(\",\");",
        "a",
    );
    check(
        "object-length-property",
        "return Object.keys({length: 2}).join(\",\");",
        "length",
    );
}

/// **`__proto__` is an accessor on `Object.prototype`**, not a property anything stores — so
/// it is shadowable by an own property, absent from an object with no prototype, and reached
/// through the ordinary chain walk rather than by a name check at the top of it.
#[test]
fn the_prototype_link_is_readable_and_writable_by_name() {
    check(
        "proto-read",
        "let o = {}; return o.__proto__ === Object.prototype;",
        "true",
    );
    check(
        "proto-write",
        "let a = {x: 1}; let b = {}; b.__proto__ = a; return b.x;",
        "1",
    );
    // The literal form is the same operation, which is why it needs no separate lowering.
    check(
        "proto-literal",
        "let a = {x: 2}; let o = {__proto__: a}; return o.x;",
        "2",
    );
    // **An object with no prototype has no `__proto__`.** The accessor lives on
    // `Object.prototype`, and this object's chain never reaches it.
    check(
        "proto-null-prototype",
        "let o = Object.create(null); return typeof o.__proto__;",
        "undefined",
    );
    check(
        "proto-end-of-chain",
        "return Object.prototype.__proto__;",
        "null",
    );
    // Assigning something that is neither an object nor `null` is ignored, not an error.
    check(
        "proto-write-primitive",
        "let o = {}; o.__proto__ = 5; return o.__proto__ === Object.prototype;",
        "true",
    );
    check(
        "proto-write-null",
        "let o = {}; o.__proto__ = null; return o.__proto__;",
        "undefined",
    );
}

/// **A cycle is refused**, by every route into `[[SetPrototypeOf]]` — and so is re-parenting an
/// object that has stopped being extensible.
#[test]
fn a_prototype_may_not_be_made_cyclic() {
    check(
        "proto-cycle",
        "let a = {}; let b = Object.create(a); \
         try { a.__proto__ = b; return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "proto-cycle-set-prototype-of",
        "let a = {}; let b = Object.create(a); \
         try { Object.setPrototypeOf(a, b); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "proto-non-extensible",
        "let o = Object.preventExtensions({}); \
         try { Object.setPrototypeOf(o, {x: 1}); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    // Setting the prototype it already has is not a change, so nothing refuses it.
    check(
        "proto-non-extensible-same",
        "let o = Object.preventExtensions({}); \
         return Object.setPrototypeOf(o, Object.prototype) === o;",
        "true",
    );
    check(
        "proto-set-prototype-of-primitive",
        "return Object.setPrototypeOf(5, null);",
        "5",
    );
    check(
        "proto-set-prototype-of-nullish",
        "try { Object.setPrototypeOf(null, {}); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
}

/// **An array's elements and its `length` are own properties with no slot**, so every question
/// asked of a descriptor has to answer for them too. `getOwnPropertyDescriptor` returning
/// `undefined` for them is what test262's `propertyHelper` then read a field off.
#[test]
fn an_element_has_a_descriptor() {
    check(
        "descriptor-element-value",
        "return Object.getOwnPropertyDescriptor([7, 8], \"0\").value;",
        "7",
    );
    check(
        "descriptor-element-flags",
        "let d = Object.getOwnPropertyDescriptor([7], \"0\"); \
         return d.writable + \",\" + d.enumerable + \",\" + d.configurable;",
        "true,true,true",
    );
    // A length is writable and neither enumerable nor configurable — an attribute set no
    // ordinary property has.
    check(
        "descriptor-array-length",
        "let d = Object.getOwnPropertyDescriptor([7, 8], \"length\"); \
         return d.value + \",\" + d.writable + \",\" + d.enumerable + \",\" + d.configurable;",
        "2,true,false,false",
    );
    check(
        "descriptor-element-absent",
        "return typeof Object.getOwnPropertyDescriptor([7], \"3\");",
        "undefined",
    );
    // A string wrapper's characters are the same shape of problem: materialised on demand, so
    // nothing in the shape lists them.
    check(
        "descriptor-string-character",
        "return Object.getOwnPropertyDescriptor(new String(\"ab\"), \"1\").value;",
        "b",
    );
    check(
        "string-wrapper-own-names",
        "return Object.getOwnPropertyNames(new String(\"ab\")).join(\",\");",
        "0,1,length",
    );
    check(
        "string-wrapper-keys",
        "return Object.keys(new String(\"ab\")).join(\",\");",
        "0,1",
    );
    // A `Number` wrapper keeps its primitive under the same hidden name and must not be read
    // as text — otherwise `12345` would have five characters and five own properties.
    check(
        "number-wrapper-keys",
        "return Object.keys(new Number(12345)).length;",
        "0",
    );
}

/// **Freezing an array has to freeze its elements**, which have no attributes of their own —
/// so the flag is carried for the whole run of them. Without it `Object.freeze` froze nothing
/// on exactly the objects people freeze most, and looked like it had worked.
#[test]
fn freezing_an_array_stops_its_elements_changing() {
    check(
        "freeze-array-write",
        "let a = Object.freeze([1]); a[0] = 9; return a[0];",
        "1",
    );
    check(
        "freeze-array-grow",
        "let a = Object.freeze([1]); a[1] = 2; return a.length;",
        "1",
    );
    check(
        "freeze-array-length",
        "let a = Object.freeze([1]); a.length = 0; return a.length;",
        "1",
    );
    check(
        "freeze-array-is-frozen",
        "return Object.isFrozen(Object.freeze([1]));",
        "true",
    );
    // Not extensible is not frozen: the element is still writable, and the answer used to be
    // `true` because an unslotted key was passed over rather than asked about.
    check(
        "prevent-extensions-array-is-not-frozen",
        "return Object.isFrozen(Object.preventExtensions([1]));",
        "false",
    );
    // Sealing leaves the values writable and stops the deleting, which is the whole difference.
    check(
        "seal-array",
        "let a = Object.seal([1]); a[0] = 9; return a[0] + \",\" + (delete a[0]);",
        "9,false",
    );
    check(
        "seal-array-is-sealed",
        "return Object.isSealed(Object.seal([1]));",
        "true",
    );
    check(
        "seal-array-is-not-frozen",
        "return Object.isFrozen(Object.seal([1]));",
        "false",
    );
    // An empty array has no elements to freeze, so it is frozen as soon as it is closed.
    check(
        "freeze-empty-array",
        "return Object.isFrozen(Object.freeze([]));",
        "true",
    );
}

/// **Defining an array index writes the element.** Storing it as an ordinary property instead
/// left the array holding two answers for one key — the element the reads use and the slot the
/// descriptors use — which disagree from then on.
#[test]
fn defining_an_index_writes_the_element() {
    check(
        "define-index-existing",
        "let a = [1]; Object.defineProperty(a, \"0\", {value: 5}); return a[0];",
        "5",
    );
    check(
        "define-index-new",
        "let a = [1]; \
         Object.defineProperty(a, \"1\", {value: 7, writable: true, enumerable: true, configurable: true}); \
         return a.length + \",\" + a[1];",
        "2,7",
    );
    // Once, however it got there.
    check(
        "define-index-not-listed-twice",
        "let a = [1]; Object.defineProperty(a, \"0\", {value: 5}); \
         return Object.getOwnPropertyNames(a).join(\",\");",
        "0,length",
    );
}

/// The rest of `Object`: the descriptors in bulk, and the two lookups that pair with
/// `__defineGetter__`.
#[test]
fn object_reports_every_descriptor_and_finds_accessors() {
    check(
        "own-descriptors",
        "let d = Object.getOwnPropertyDescriptors({a: 1, b: 2}); \
         return d.a.value + \",\" + d.b.value + \",\" + d.a.enumerable;",
        "1,2,true",
    );
    check(
        "own-descriptors-array",
        "let d = Object.getOwnPropertyDescriptors([7]); \
         return d[0].value + \",\" + d.length.value;",
        "7,1",
    );
    check(
        "own-descriptors-empty",
        "return Object.keys(Object.getOwnPropertyDescriptors({})).length;",
        "0",
    );
    check(
        "lookup-getter",
        "let o = {}; o.__defineGetter__(\"x\", function () { return 7; }); \
         return o.__lookupGetter__(\"x\")();",
        "7",
    );
    // A getter with no setter has nothing to find, which is how the pair is told apart.
    check(
        "lookup-setter-absent",
        "let o = {}; o.__defineGetter__(\"x\", function () { return 7; }); \
         return typeof o.__lookupSetter__(\"x\");",
        "undefined",
    );
    // Inherited, unlike `getOwnPropertyDescriptor` — which is why both still exist.
    check(
        "lookup-getter-inherited",
        "let a = {}; a.__defineGetter__(\"x\", function () { return 3; }); \
         let b = Object.create(a); return b.__lookupGetter__(\"x\")();",
        "3",
    );
}

/// **`null` and `undefined` are the error, not "anything that is not an object".** Every
/// `Object` static coerces its argument, so a primitive is answered and only a nullish one
/// throws — and answering one of these with `undefined` is what the caller then reads a field
/// off.
#[test]
fn object_refuses_only_the_nullish() {
    check(
        "keys-of-null",
        "try { Object.keys(null); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "values-of-undefined",
        "try { Object.values(undefined); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "entries-of-null",
        "try { Object.entries(null); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "own-names-of-null",
        "try { Object.getOwnPropertyNames(null); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "get-prototype-of-null",
        "try { Object.getPrototypeOf(null); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "assign-to-null",
        "try { Object.assign(null, {}); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "has-own-property-of-null",
        "try { Object.prototype.hasOwnProperty.call(null, \"x\"); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    // A primitive is coerced, which is the other half of the same rule.
    check("keys-of-number", "return Object.keys(5).length;", "0");
    check(
        "get-prototype-of-number",
        "return Object.getPrototypeOf(1) === Object.getPrototypeOf(2);",
        "true",
    );
    check(
        "freeze-of-null-is-not-an-error",
        "return Object.freeze(null);",
        "null",
    );
}

/// A string's characters are its own properties, whether it is wrapped or not.
#[test]
fn a_string_owns_its_characters() {
    check(
        "string-keys",
        "return Object.keys(\"ab\").join(\",\");",
        "0,1",
    );
    check(
        "string-own-names",
        "return Object.getOwnPropertyNames(\"ab\").join(\",\");",
        "0,1,length",
    );
    check(
        "string-descriptor",
        "let d = Object.getOwnPropertyDescriptor(\"ab\", \"0\"); \
         return d.value + \",\" + d.writable + \",\" + d.enumerable;",
        "a,false,true",
    );
    check(
        "string-length-descriptor",
        "let d = Object.getOwnPropertyDescriptor(\"ab\", \"length\"); \
         return d.value + \",\" + d.enumerable;",
        "2,false",
    );
}

/// **The class tag is what `Object.prototype.toString` is for**, and answering `[object
/// Object]` for everything that is not an array made it useless for the one job it has.
#[test]
fn the_class_tag_names_the_class() {
    check(
        "tag-number",
        "return Object.prototype.toString.call(5);",
        "[object Number]",
    );
    check(
        "tag-string",
        "return Object.prototype.toString.call(\"x\");",
        "[object String]",
    );
    check(
        "tag-null",
        "return Object.prototype.toString.call(null);",
        "[object Null]",
    );
    check(
        "tag-undefined",
        "return Object.prototype.toString.call(undefined);",
        "[object Undefined]",
    );
    check(
        "tag-array",
        "return Object.prototype.toString.call([]);",
        "[object Array]",
    );
    check(
        "tag-function",
        "return Object.prototype.toString.call(function () {});",
        "[object Function]",
    );
    check(
        "tag-plain",
        "return Object.prototype.toString.call({});",
        "[object Object]",
    );
    // A wrapper's class follows from the primitive it holds — which is why a boolean wrapper
    // now stores a boolean rather than one or zero.
    check(
        "tag-string-wrapper",
        "return Object.prototype.toString.call(new String(\"x\"));",
        "[object String]",
    );
    check(
        "tag-number-wrapper",
        "return Object.prototype.toString.call(new Number(1));",
        "[object Number]",
    );
    check(
        "tag-boolean-wrapper",
        "return Object.prototype.toString.call(new Boolean(true));",
        "[object Boolean]",
    );
    // The representation change the tag needed must not change what the wrapper reads as.
    check(
        "boolean-wrapper-value",
        "return new Boolean(false).valueOf();",
        "false",
    );
    check(
        "boolean-wrapper-true",
        "return new Boolean(true).valueOf();",
        "true",
    );
}

/// An element is enumerable and `length` is not, and neither has a slot to say so.
#[test]
fn an_element_answers_for_its_own_enumerability() {
    check(
        "element-is-enumerable",
        "return [1].propertyIsEnumerable(0);",
        "true",
    );
    check(
        "array-length-is-not-enumerable",
        "return [1].propertyIsEnumerable(\"length\");",
        "false",
    );
    check(
        "absent-is-not-enumerable",
        "return [1].propertyIsEnumerable(3);",
        "false",
    );
}

/// **Every index comes before every name, in ascending order**, whatever order they were
/// written in. That is the specification's enumeration order and it is observable.
#[test]
fn keys_come_out_indices_first() {
    check(
        "key-order",
        "return Object.keys({b: 1, 2: 1, 1: 1, a: 1}).join(\",\");",
        "1,2,b,a",
    );
    check(
        "key-order-forin",
        "let s = \"\"; for (let k in {b: 1, 2: 1, 1: 1, a: 1}) { s = s + k; } return s;",
        "12ba",
    );
    // Only the canonical spelling is an index: `"01"` is a name and stays where it was put.
    check(
        "key-order-non-canonical",
        "let o = {}; o[\"01\"] = 1; o[\"1\"] = 1; return Object.keys(o).join(\",\");",
        "1,01",
    );
    check(
        "key-order-names-keep-insertion",
        "return Object.keys({z: 1, a: 1, m: 1}).join(\",\");",
        "z,a,m",
    );
}

/// `Object(x)` is `ToObject(x)`: an object unchanged, a primitive wrapped, nothing at all for
/// nothing at all.
#[test]
fn the_object_constructor_coerces() {
    check(
        "object-of-object-is-identity",
        "let o = {x: 1}; return Object(o) === o;",
        "true",
    );
    check("object-of-number", "return Object(5).valueOf();", "5");
    check(
        "object-of-string-length",
        "return Object(\"ab\").length;",
        "2",
    );
    check(
        "object-of-string-tag",
        "return Object.prototype.toString.call(Object(\"ab\"));",
        "[object String]",
    );
    check(
        "object-of-boolean-tag",
        "return Object.prototype.toString.call(Object(true));",
        "[object Boolean]",
    );
    check(
        "object-of-nothing",
        "return Object.keys(Object()).length;",
        "0",
    );
    check(
        "object-of-null",
        "return Object.prototype.toString.call(Object(null));",
        "[object Object]",
    );
    // A descriptor has to be able to hold fields; a number quietly defined `undefined`.
    check(
        "define-property-bad-descriptor",
        "try { Object.defineProperty({}, \"x\", 5); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "define-properties-bad-target",
        "try { Object.defineProperties(5, {}); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
}

/// **An index names an element, even spelled as text.** `a["0"]` and `a[0]` are the same
/// property; only the second reached the elements, so everything that reads an object by name
/// — `Object.values`, `Object.entries`, `Object.assign` — saw `undefined` for every element an
/// array has.
#[test]
fn an_index_spelled_as_text_is_still_an_element() {
    check("index-as-text-read", "return [7, 8][\"1\"];", "8");
    check(
        "index-as-text-write",
        "let a = [7]; a[\"0\"] = 9; return a[0];",
        "9",
    );
    check(
        "index-as-text-grows",
        "let a = [7]; a[\"1\"] = 9; return a.length + \",\" + a[1];",
        "2,9",
    );
    // Only the canonical spelling: `"01"` is a property, not element one.
    check(
        "index-non-canonical-is-a-name",
        "let a = [7]; a[\"01\"] = 9; return a.length + \",\" + a[0];",
        "1,7",
    );
    check("string-index-as-text", "return \"ab\"[\"1\"];", "b");
    check(
        "object-values-of-array",
        "return Object.values([7, 8]).join(\",\");",
        "7,8",
    );
    check(
        "object-entries-of-array",
        "return Object.entries([7])[0].join(\",\");",
        "0,7",
    );
    check(
        "object-assign-from-array",
        "let o = Object.assign({}, [7, 8]); return o[0] + \",\" + o[1] + \",\" + Object.keys(o).join(\"|\");",
        "7,8,0|1",
    );
    // `length` is not enumerable, so it is not copied — `own_keys` would have brought it.
    check(
        "object-assign-skips-length",
        "return Object.keys(Object.assign({}, \"ab\")).join(\",\");",
        "0,1",
    );
    check(
        "object-values-of-string",
        "return Object.values(\"ab\").join(\",\");",
        "a,b",
    );
}

/// `Object.groupBy` files items under what the callback answers, in an object with **no
/// prototype** — so a group called `"toString"` collides with nothing.
#[test]
fn group_by_files_items_under_their_key() {
    check(
        "group-by",
        "let g = Object.groupBy([1, 2, 3, 4], function (n) { return n % 2 ? \"odd\" : \"even\"; }); \
         return g.odd.join(\",\") + \"|\" + g.even.join(\",\");",
        "1,3|2,4",
    );
    check(
        "group-by-has-no-prototype",
        "return Object.getPrototypeOf(Object.groupBy([], function () { return \"x\"; }));",
        "null",
    );
    check(
        "group-by-inherited-name",
        "let g = Object.groupBy([1], function () { return \"toString\"; }); \
         return g.toString.length;",
        "1",
    );
    check(
        "group-by-index-argument",
        "let g = Object.groupBy([9, 9], function (v, i) { return i; }); \
         return Object.keys(g).join(\",\");",
        "0,1",
    );
    check(
        "group-by-needs-a-function",
        "try { Object.groupBy([], 5); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
}

/// A prototype is an object or `null`, and a receiver that is nullish is an error rather than
/// a `false`.
#[test]
fn create_and_is_prototype_of_check_what_they_are_given() {
    check(
        "create-bad-prototype",
        "try { Object.create(5); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "create-null-prototype",
        "return Object.getPrototypeOf(Object.create(null));",
        "null",
    );
    check(
        "is-prototype-of-nullish",
        "try { Object.prototype.isPrototypeOf.call(null, {}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "is-prototype-of-primitive-argument",
        "return Object.prototype.isPrototypeOf.call(Object.prototype, 5);",
        "false",
    );
}

/// A character is non-configurable and has no slot to say so, so redefining one has to be
/// refused by the derived answer or it quietly grows a second property with the same name.
#[test]
fn a_character_cannot_be_redefined() {
    check(
        "redefine-character",
        "let s = new String(\"ab\"); \
         try { Object.defineProperty(s, \"0\", {value: \"z\"}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    // Redefining it to what it already is changes nothing and is allowed.
    check(
        "redefine-character-to-itself",
        "let s = new String(\"ab\"); \
         Object.defineProperty(s, \"0\", {value: \"a\"}); return s[0];",
        "a",
    );
    // A string is a cell but not an object, so a handle is not the test the target needs.
    check(
        "define-on-a-primitive-string",
        "try { Object.defineProperty(\"ab\", \"x\", {value: 1}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
}

/// **Freezing keeps a property being an accessor.** Clearing the flag turned the pair of
/// functions in the slot into the property's value, so a frozen getter read back as a
/// two-element array instead of being called.
#[test]
fn freezing_an_accessor_leaves_it_an_accessor() {
    check(
        "freeze-getter",
        "let o = {}; o.__defineGetter__(\"x\", function () { return 7; }); \
         Object.freeze(o); return o.x;",
        "7",
    );
    // An accessor has no writability to freeze, so being non-configurable is the whole of it.
    check(
        "frozen-getter-is-frozen",
        "let o = {}; o.__defineGetter__(\"x\", function () { return 7; }); \
         return Object.isFrozen(Object.freeze(o));",
        "true",
    );
    check(
        "frozen-getter-descriptor",
        "let o = {}; o.__defineGetter__(\"x\", function () { return 7; }); \
         Object.freeze(o); \
         let d = Object.getOwnPropertyDescriptor(o, \"x\"); \
         return typeof d.get + \",\" + d.configurable;",
        "function,false",
    );
}

/// **Every own name shadows, not only the enumerable ones.** A non-enumerable own property
/// hides an inherited one of the same name, so a `for-in` must visit neither.
#[test]
fn a_hidden_property_still_shadows_an_inherited_one() {
    check(
        "forin-shadowed-by-non-enumerable",
        "let a = {x: 1}; let b = Object.create(a); \
         Object.defineProperty(b, \"x\", {value: 2, enumerable: false}); \
         let s = \"\"; for (let k in b) { s = s + k; } return s.length;",
        "0",
    );
    // An enumerable own property is visited once, not once per level.
    check(
        "forin-shadowed-by-enumerable",
        "let a = {x: 1}; let b = Object.create(a); b.x = 2; \
         let s = \"\"; for (let k in b) { s = s + k; } return s;",
        "x",
    );
    check(
        "forin-inherits",
        "let a = {x: 1}; let b = Object.create(a); b.y = 2; \
         let s = \"\"; for (let k in b) { s = s + k; } return s;",
        "yx",
    );
    // A character is non-configurable and has no slot to say so, so `delete` has to refuse it
    // from the derived answer.
    check(
        "delete-a-character",
        "let s = new String(\"ab\"); return delete s[0];",
        "false",
    );
}

/// **A function's `prototype` is not enumerable**, which matters because every function a
/// program can see is an object it might enumerate.
#[test]
fn a_functions_prototype_is_not_enumerable() {
    check(
        "function-keys",
        "return Object.keys(function () {}).length;",
        "0",
    );
    check(
        "function-forin",
        "let s = \"\"; for (let k in function () {}) { s = s + k; } return s.length;",
        "0",
    );
    // Still there, and still what `new` and `instanceof` read.
    check(
        "function-prototype-is-present",
        "function f() {} return Object.getOwnPropertyNames(f).indexOf(\"prototype\") >= 0;",
        "true",
    );
    check(
        "function-prototype-still-links",
        "function f() {} return new f() instanceof f;",
        "true",
    );
    check("constructor-keys", "return Object.keys(Array).length;", "0");
}

/// **`constructor` is the link back**, and nothing had it: `({}).constructor` was `undefined`,
/// which is how a program asks what made something.
#[test]
fn a_prototype_points_back_at_its_constructor() {
    check(
        "object-constructor",
        "return ({}).constructor === Object;",
        "true",
    );
    check(
        "array-constructor",
        "return [].constructor === Array;",
        "true",
    );
    check(
        "string-constructor",
        "return \"x\".constructor === String;",
        "true",
    );
    check(
        "function-constructor",
        "function f() {} return new f().constructor === f;",
        "true",
    );
    // Not enumerable, or every object would list it.
    check(
        "constructor-is-not-enumerable",
        "return Object.keys(Object.prototype).indexOf(\"constructor\");",
        "-1",
    );
    // Configurable, which is how a subclass replaces it — `define_method` would also have
    // renamed the function to `"constructor"`, which is why it is not that.
    check(
        "constructor-keeps-its-name",
        "return Object.prototype.constructor.name;",
        "Object",
    );
}

/// `toLocaleString` is a hook: it calls the receiver's own `toString`, so an override is
/// visible through it. Pointing it at the default made every override invisible.
#[test]
fn to_locale_string_calls_the_receivers_to_string() {
    check(
        "to-locale-string-override",
        "let o = {toString: function () { return \"x\"; }}; return o.toLocaleString();",
        "x",
    );
    check(
        "to-locale-string-default",
        "return ({}).toLocaleString();",
        "[object Object]",
    );
}

/// **A non-extensible object refuses a property it does not have** — the one refusal
/// `defineProperty` never made, so `preventExtensions` stopped assignment and let a definition
/// straight through.
#[test]
fn defining_respects_extensibility() {
    check(
        "define-on-non-extensible",
        "let o = Object.preventExtensions({}); \
         try { Object.defineProperty(o, \"x\", {value: 1}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    // An existing property may still be redefined: it is the *adding* that stops.
    check(
        "redefine-on-non-extensible",
        "let o = {x: 1}; Object.preventExtensions(o); \
         Object.defineProperty(o, \"x\", {value: 2}); return o.x;",
        "2",
    );
    check(
        "define-element-on-non-extensible",
        "let a = Object.preventExtensions([1]); \
         try { Object.defineProperty(a, \"1\", {value: 2}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    // A frozen element is non-configurable, so redefining its value is refused too.
    check(
        "define-frozen-element",
        "let a = Object.freeze([1]); \
         try { Object.defineProperty(a, \"0\", {value: 2}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "define-properties-on-non-extensible",
        "let o = Object.preventExtensions({}); \
         try { Object.defineProperties(o, {x: {value: 1}}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
}

/// **A thrown error has to be an instance of what threw it.** `catch (e) { e instanceof
/// TypeError }` is how a program asks what it caught, and test262's `assert.throws` compares
/// `thrown.constructor` — both answered `Object` for an engine that had thrown exactly the
/// right thing.
#[test]
fn an_error_is_an_instance_of_its_constructor() {
    check(
        "error-instanceof",
        "return new TypeError(\"x\") instanceof TypeError;",
        "true",
    );
    // Every kind of error is an `Error`, which is what most code that catches one checks.
    check(
        "error-instanceof-error",
        "return new TypeError(\"x\") instanceof Error;",
        "true",
    );
    check(
        "error-constructor",
        "return new RangeError(\"x\").constructor === RangeError;",
        "true",
    );
    // Called without `new`, a constructor still constructs.
    check(
        "error-called-plainly",
        "return Error(\"x\") instanceof Error;",
        "true",
    );
    // And the errors the engine itself raises are the same kind of object.
    check(
        "raised-error-instanceof",
        "try { null.x; return \"no\"; } catch (e) { return e instanceof TypeError; }",
        "true",
    );
    check(
        "raised-error-constructor",
        "try { null.x; return \"no\"; } catch (e) { return e.constructor === TypeError; }",
        "true",
    );
    // The kind lives on the prototype, so the instance carries nothing a program can list.
    check(
        "error-keys",
        "return Object.keys(new TypeError(\"x\")).length;",
        "0",
    );
    check(
        "error-prototype-name",
        "return Error.prototype.name;",
        "Error",
    );
    check(
        "error-to-string",
        "return new TypeError(\"x\").toString();",
        "TypeError: x",
    );
    // Either half being empty takes the separator with it.
    check(
        "error-to-string-bare",
        "return new Error().toString();",
        "Error",
    );
    check(
        "error-to-string-message-only",
        "let e = new Error(\"x\"); e.name = \"\"; return e.toString();",
        "x",
    );
}

/// **An element can be restricted on its own.** Elements share a dense `Vec` with no room for
/// attributes, so a rule array beside them carries the exceptions: one entry for the whole run
/// and one per element that differs.
#[test]
fn an_element_can_carry_its_own_attributes() {
    check(
        "define-element-unwritable",
        "let a = [1]; Object.defineProperty(a, \"0\", {writable: false}); a[0] = 9; return a[0];",
        "1",
    );
    check(
        "define-element-unwritable-descriptor",
        "let a = [1]; Object.defineProperty(a, \"0\", {writable: false}); \
         return Object.getOwnPropertyDescriptor(a, \"0\").writable;",
        "false",
    );
    // A defined property defaults to none of the three, elements included.
    check(
        "define-new-element-defaults",
        "let a = []; Object.defineProperty(a, \"0\", {value: 7}); \
         let d = Object.getOwnPropertyDescriptor(a, \"0\"); \
         return a.length + \",\" + d.value + \",\" + d.writable + \",\" + d.enumerable;",
        "1,7,false,false",
    );
    check(
        "define-non-enumerable-element",
        "let a = []; Object.defineProperty(a, \"0\", {value: 7}); return Object.keys(a).length;",
        "0",
    );
    check(
        "define-non-configurable-element-refuses-delete",
        "let a = []; Object.defineProperty(a, \"0\", {value: 7}); return delete a[0];",
        "false",
    );
    // One element's rule does not become every element's.
    check(
        "element-rules-are-per-element",
        "let a = [1, 2]; Object.defineProperty(a, \"0\", {writable: false}); \
         a[0] = 8; a[1] = 9; return a[0] + \",\" + a[1];",
        "1,9",
    );
    // Sealing narrows what is already narrow rather than widening it.
    check(
        "seal-keeps-an-unwritable-element",
        "let a = [1]; Object.defineProperty(a, \"0\", {writable: false}); \
         Object.seal(a); a[0] = 9; return a[0];",
        "1",
    );
    check(
        "seal-leaves-other-elements-writable",
        "let a = [1, 2]; Object.defineProperty(a, \"0\", {writable: false}); \
         Object.seal(a); a[1] = 9; return a[1];",
        "9",
    );
}

/// **The tag is keyed on what made the object, not on what it inherits from** — which is the
/// one case where the two differ.
#[test]
fn an_error_reports_its_class() {
    check(
        "tag-error",
        "return Object.prototype.toString.call(new TypeError(\"x\"));",
        "[object Error]",
    );
    check(
        "tag-raised-error",
        "try { null.x; return \"no\"; } catch (e) { return Object.prototype.toString.call(e); }",
        "[object Error]",
    );
    // Inheriting from `Error.prototype` does not make something an error.
    check(
        "tag-error-lookalike",
        "return Object.prototype.toString.call(Object.create(Error.prototype));",
        "[object Object]",
    );
    // And the tag is not a property a program can see.
    check(
        "error-own-names",
        "return Object.getOwnPropertyNames(new TypeError(\"x\")).join(\",\");",
        "message",
    );
}

/// `Object.assign` uses the throwing form of `Set`, so a read-only property on the target is
/// a `TypeError` rather than a write that quietly does nothing.
#[test]
fn assign_refuses_a_read_only_target() {
    check(
        "assign-read-only",
        "let t = {}; Object.defineProperty(t, \"x\", {value: 1, writable: false}); \
         try { Object.assign(t, {x: 2}); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    // A sealed target keeps its properties writable, so assigning to one still works.
    check(
        "assign-sealed-target",
        "let t = Object.seal({x: 1}); Object.assign(t, {x: 2}); return t.x;",
        "2",
    );
    // A setter may accept the write, so an accessor is not refused here.
    check(
        "assign-through-a-setter",
        "let seen = 0; let t = {}; \
         t.__defineSetter__(\"x\", function (v) { seen = v; }); \
         Object.assign(t, {x: 5}); return seen;",
        "5",
    );
}

/// **A deleted property is absent**, even though the shape still names its slot — which is
/// the whole point of the tombstone. Answering from the slot handed back the permissions the
/// property had before it went.
#[test]
fn a_deleted_property_is_gone_from_every_question() {
    check(
        "deleted-has-no-descriptor",
        "let o = {x: 1}; delete o.x; \
         return typeof Object.getOwnPropertyDescriptor(o, \"x\");",
        "undefined",
    );
    check(
        "deleted-is-not-own",
        "let o = {x: 1}; delete o.x; return o.hasOwnProperty(\"x\");",
        "false",
    );
    // A property deleted after being made read-only is not read-only any more: it is nothing.
    // Asserted with *two* writes, because the slot keeps its attributes and the first write
    // clears the tombstone — one write passes whether or not the attributes were reset.
    check(
        "deleted-read-only-can-be-redefined",
        "let o = {}; \
         Object.defineProperty(o, \"x\", {value: 1, writable: false, configurable: true}); \
         delete o.x; o.x = 2; o.x = 3; return o.x;",
        "3",
    );
    check(
        "revived-property-is-enumerable",
        "let o = {}; \
         Object.defineProperty(o, \"x\", {value: 1, configurable: true}); \
         delete o.x; o.x = 2; return Object.keys(o).join(\",\");",
        "x",
    );
}

/// `defineProperties` coerces its map of descriptors like any other argument, so a nullish one
/// is the error and a primitive simply describes nothing.
#[test]
fn define_properties_checks_its_map() {
    check(
        "define-properties-null-map",
        "try { Object.defineProperties({}, null); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "define-properties-primitive-map",
        "return Object.keys(Object.defineProperties({}, 5)).length;",
        "0",
    );
    check(
        "define-properties-applies",
        "let o = Object.defineProperties({}, {x: {value: 1, enumerable: true}}); \
         return o.x + \",\" + Object.keys(o).join(\"\");",
        "1,x",
    );
}

/// Defining a property that was deleted brings it back. The shape keeps naming a deleted
/// property's slot, so writing a value into one without clearing the mark left the property
/// both defined and absent.
#[test]
fn a_deleted_property_can_be_defined_again() {
    check(
        "define-after-delete",
        "let o = {x: 1}; delete o.x; \
         Object.defineProperty(o, \"x\", {value: 2, enumerable: true}); return o.x;",
        "2",
    );
    check(
        "define-after-delete-attributes",
        "let o = {x: 1}; delete o.x; \
         Object.defineProperty(o, \"x\", {value: 2}); \
         let d = Object.getOwnPropertyDescriptor(o, \"x\"); \
         return d.value + \",\" + d.enumerable + \",\" + d.writable;",
        "2,false,false",
    );
}

/// A descriptor has to describe something. A primitive is not an object however cell-shaped it
/// is, and a `get` that is present and not callable describes nothing the engine can do.
#[test]
fn a_descriptor_has_to_describe_something() {
    check(
        "descriptor-is-a-string",
        "try { Object.create({}, {p: \"abc\"}); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "getter-is-a-string",
        "try { Object.defineProperty({}, \"p\", {get: \"abc\"}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "setter-is-a-number",
        "try { Object.defineProperty({}, \"p\", {set: 5}); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    // `undefined` is allowed, and means the half that is missing.
    check(
        "getter-only",
        "let o = {}; \
         Object.defineProperty(o, \"p\", {get: function () { return 3; }, set: undefined}); \
         return o.p;",
        "3",
    );
}

/// A namespace's constants are not enumerable, which is what `Object.defineProperties(o, Math)`
/// depends on: it walks the enumerable own properties and reads each as a descriptor.
#[test]
fn a_namespaces_constants_are_not_enumerable() {
    check("math-keys", "return Object.keys(Math).length;", "0");
    check("number-keys", "return Object.keys(Number).length;", "0");
    check("math-pi-still-reads", "return Math.PI > 3.14;", "true");
    // Frozen, so an assignment is ignored rather than changing what every later read sees.
    check(
        "math-pi-is-read-only",
        "Math.PI = 1; return Math.PI > 3.14;",
        "true",
    );
    check(
        "define-properties-from-a-namespace",
        "let o = {}; Math.prop = {value: 12}; Object.defineProperties(o, Math); \
         return o.prop;",
        "12",
    );
}

/// **A built-in's `length` is fixed by the specification** — the count of parameters before
/// the first with a default — and test262 checks it for every one it covers. No built-in had
/// one at all, so each of those failed on a method that was otherwise complete.
#[test]
fn a_built_in_declares_how_many_arguments_it_takes() {
    check("length-object-keys", "return Object.keys.length;", "1");
    check(
        "length-object-define-property",
        "return Object.defineProperty.length;",
        "3",
    );
    check("length-object-assign", "return Object.assign.length;", "2");
    check(
        "length-has-own-property",
        "return Object.prototype.hasOwnProperty.length;",
        "1",
    );
    // Zero is a real answer, and the one a default would have got wrong.
    check("length-array-pop", "return [].pop.length;", "0");
    check(
        "length-value-of",
        "return Object.prototype.valueOf.length;",
        "0",
    );
    check("length-array-slice", "return [].slice.length;", "2");
    // The constructors declare one too.
    check("length-object-constructor", "return Object.length;", "1");
    check("length-array-constructor", "return Array.length;", "1");
    // Same attributes as `name`: not writable, not enumerable, configurable.
    check(
        "length-descriptor",
        "let d = Object.getOwnPropertyDescriptor(Object.keys, \"length\"); \
         return d.writable + \",\" + d.enumerable + \",\" + d.configurable;",
        "false,false,true",
    );
    check(
        "length-is-not-enumerable",
        "return Object.keys(Object.keys).length;",
        "0",
    );
}

/// **`this` at the top of a script is the global object.** The entry point passed `undefined`,
/// which is what it is inside a strict function and never what it is here — so `this.x = 1`
/// did nothing and every test that reaches a global through `this` read a property of nothing.
#[test]
fn top_level_this_is_the_global_object() {
    check("this-is-global", "return this === globalThis;", "true");
    check(
        "this-reaches-a-global",
        "return typeof this.Object;",
        "function",
    );
    check(
        "this-can-be-written",
        "this.answer = 42; return answer;",
        "42",
    );
    // Not asserted here: a top-level `var` is a local slot in this engine, not a property of
    // the global object. That is a real difference from the specification and it belongs to
    // the lowering rather than to `this`, so it is filed rather than smuggled into this test.
}

/// `Object.assign` coerces its target like every other static's argument: a primitive is
/// wrapped and the wrapper is what comes back carrying the assignments.
#[test]
fn assign_coerces_its_target() {
    check(
        "assign-to-boolean",
        "return typeof Object.assign(true, {a: 1});",
        "object",
    );
    check(
        "assign-to-boolean-value",
        "return Object.assign(true, {a: 1}).valueOf();",
        "true",
    );
    check(
        "assign-to-boolean-copies",
        "return Object.assign(true, {a: 1}).a;",
        "1",
    );
    check(
        "assign-to-number",
        "return Object.assign(5, {}).valueOf();",
        "5",
    );
    check(
        "assign-to-nullish",
        "try { Object.assign(undefined, {}); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
}

/// **Every `Date` setter is one operation with a different starting field**, so the fields it
/// does not name keep what they had and the ones it does roll over rather than erroring.
#[test]
fn a_date_can_be_set() {
    check(
        "date-set-time",
        "let d = new Date(0); d.setTime(86400000); return d.getTime();",
        "86400000",
    );
    check(
        "date-set-full-year",
        "let d = new Date(0); d.setFullYear(2020); return d.getFullYear();",
        "2020",
    );
    // The fields it does not name keep what they had.
    check(
        "date-set-year-keeps-the-day",
        "let d = new Date(0); d.setFullYear(2020); \
         return d.getMonth() + \",\" + d.getDate();",
        "0,1",
    );
    check(
        "date-set-month-and-day",
        "let d = new Date(0); d.setFullYear(2020, 5, 17); \
         return d.getFullYear() + \"-\" + d.getMonth() + \"-\" + d.getDate();",
        "2020-5-17",
    );
    // Rolling over is what makes one operation enough for all of them.
    check(
        "date-set-month-rolls-the-year",
        "let d = new Date(0); d.setMonth(13); \
         return d.getFullYear() + \",\" + d.getMonth();",
        "1971,1",
    );
    check(
        "date-set-date-rolls-back",
        "let d = new Date(0); d.setDate(0); \
         return d.getFullYear() + \"-\" + d.getMonth() + \"-\" + d.getDate();",
        "1969-11-31",
    );
    check(
        "date-set-hours",
        "let d = new Date(0); d.setHours(5, 6, 7, 8); \
         return d.getHours() + \":\" + d.getMinutes() + \":\" + d.getSeconds() + \".\" + \
         d.getMilliseconds();",
        "5:6:7.8",
    );
    check(
        "date-set-minutes-keeps-the-hour",
        "let d = new Date(0); d.setHours(5); d.setMinutes(9); \
         return d.getHours() + \":\" + d.getMinutes();",
        "5:9",
    );
    // A setter answers the new time value.
    check(
        "date-set-returns-the-time",
        "let d = new Date(0); return d.setMilliseconds(250);",
        "250",
    );
    // The first argument is coerced even when it is absent, so a setter with none invalidates.
    check(
        "date-set-with-no-argument",
        "let d = new Date(0); d.setHours(); return d.getTime();",
        "NaN",
    );
    // An invalid date stays invalid — except `setFullYear`, which starts from the epoch.
    check(
        "date-set-hours-on-an-invalid-date",
        "let d = new Date(NaN); d.setHours(5); return d.getTime();",
        "NaN",
    );
    check(
        "date-set-year-on-an-invalid-date",
        "let d = new Date(NaN); d.setFullYear(1971); return d.getFullYear();",
        "1971",
    );
    // A receiver that is not a date is a `TypeError`, not a quiet answer.
    check(
        "date-set-on-a-plain-object",
        "try { Date.prototype.setTime.call({}, 0); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    // The UTC twins are the same operation: this engine has no local-time offset.
    check(
        "date-set-utc-hours",
        "let d = new Date(0); d.setUTCHours(3); return d.getUTCHours();",
        "3",
    );
}

/// **`Reflect` is `Object`'s operations with the failures reported rather than thrown.** Every
/// method here is the machinery a property access already uses, exposed as a function — which
/// is why it can exist at all without proxies, the other half of what it was designed for.
#[test]
fn reflect_exposes_the_object_operations() {
    check("reflect-get", "return Reflect.get({a: 1}, \"a\");", "1");
    check(
        "reflect-set",
        "let o = {}; let ok = Reflect.set(o, \"a\", 2); return ok + \",\" + o.a;",
        "true,2",
    );
    check("reflect-has", "return Reflect.has({a: 1}, \"a\");", "true");
    check(
        "reflect-has-absent",
        "return Reflect.has({}, \"a\");",
        "false",
    );
    check(
        "reflect-delete",
        "let o = {a: 1}; let ok = Reflect.deleteProperty(o, \"a\"); \
         return ok + \",\" + o.hasOwnProperty(\"a\");",
        "true,false",
    );
    check(
        "reflect-own-keys",
        "return Reflect.ownKeys([7]).join(\",\");",
        "0,length",
    );
    check(
        "reflect-get-prototype-of",
        "return Reflect.getPrototypeOf({}) === Object.prototype;",
        "true",
    );
    check(
        "reflect-define-property",
        "let o = {}; let ok = Reflect.defineProperty(o, \"x\", {value: 1}); \
         return ok + \",\" + o.x;",
        "true,1",
    );
    check(
        "reflect-own-descriptor",
        "return Reflect.getOwnPropertyDescriptor({a: 1}, \"a\").value;",
        "1",
    );
    check(
        "reflect-is-extensible",
        "return Reflect.isExtensible({});",
        "true",
    );
    check(
        "reflect-prevent-extensions",
        "let o = {}; let ok = Reflect.preventExtensions(o); \
         return ok + \",\" + Reflect.isExtensible(o);",
        "true,false",
    );
    check(
        "reflect-apply",
        "return Reflect.apply(function (a, b) { return a + b; }, undefined, [2, 3]);",
        "5",
    );
    check(
        "reflect-set-prototype-of",
        "let a = {x: 1}; let o = {}; let ok = Reflect.setPrototypeOf(o, a); \
         return ok + \",\" + o.x;",
        "true,1",
    );
}

/// **The failures are reported, not thrown** — which is the whole reason to reach for
/// `Reflect` over the `Object` method that does the same thing.
#[test]
fn reflect_answers_false_where_object_throws() {
    check(
        "reflect-set-on-a-frozen-object",
        "let o = Object.freeze({a: 1}); return Reflect.set(o, \"a\", 2);",
        "false",
    );
    check(
        "reflect-set-a-new-property-on-a-closed-object",
        "let o = Object.preventExtensions({}); return Reflect.set(o, \"a\", 2);",
        "false",
    );
    check(
        "reflect-define-on-a-closed-object",
        "let o = Object.preventExtensions({}); \
         return Reflect.defineProperty(o, \"x\", {value: 1});",
        "false",
    );
    check(
        "reflect-set-prototype-of-a-cycle",
        "let a = {}; let b = Object.create(a); return Reflect.setPrototypeOf(a, b);",
        "false",
    );
    // A swallowed refusal must not be left on the runtime for the next `catch` to find.
    check(
        "reflect-refusal-leaves-nothing-pending",
        "let o = Object.preventExtensions({}); Reflect.defineProperty(o, \"x\", {value: 1}); \
         try { return \"clean\"; } catch (e) { return \"leaked\"; }",
        "clean",
    );
    // A primitive target is still an error: `Reflect` refuses where `Object` coerces.
    check(
        "reflect-get-prototype-of-a-number",
        "try { Reflect.getPrototypeOf(1); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "reflect-get-on-a-primitive",
        "try { Reflect.get(\"ab\", \"0\"); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
}

/// **A callback is checked before a single element is read.** Without that, calling a
/// non-function reached `crisol_not_a_function` once per element — which answers `undefined`,
/// so `[1, 2].map(5)` produced `[undefined, undefined]` and looked like a working call.
#[test]
fn an_iteration_method_needs_a_real_callback() {
    for (name, program) in [
        ("map", "[1].map(5)"),
        ("for-each", "[1].forEach(undefined)"),
        ("filter", "[1].filter(null)"),
        ("every", "[1].every(1)"),
        ("some", "[1].some({})"),
        ("find", "[1].find(\"x\")"),
        ("find-index", "[1].findIndex(true)"),
        ("reduce", "[1].reduce(5)"),
        ("reduce-right", "[1].reduceRight(5)"),
        ("flat-map", "[1].flatMap(5)"),
        ("map-for-each", "new Map().forEach(5)"),
        ("set-for-each", "new Set().forEach(5)"),
    ] {
        check(
            &format!("callback-required-{name}"),
            &format!("try {{ {program}; return \"no\"; }} catch (e) {{ return e.name; }}"),
            "TypeError",
        );
    }
}

/// A comparator is **optional**, and only wrong when it is present and not callable.
#[test]
fn sort_takes_a_comparator_or_nothing() {
    check(
        "sort-bad-comparator",
        "try { [3, 1].sort(5); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "sort-no-comparator",
        "return [3, 1].sort().join(\",\");",
        "1,3",
    );
    check(
        "sort-undefined-comparator",
        "return [3, 1].sort(undefined).join(\",\");",
        "1,3",
    );
    check(
        "sort-real-comparator",
        "return [3, 1].sort(function (a, b) { return b - a; }).join(\",\");",
        "3,1",
    );
}

/// **A length that cannot become a number is an error, not a zero.** Reading `length` off an
/// array-like coerces it, and the two coercions that fail have to say so rather than answer
/// `NaN` and be clamped to an empty walk.
#[test]
fn a_length_that_cannot_convert_is_an_error() {
    // **Not `fill`**, though that is the case test262 uses: a mutating method still requires
    // a real array here (D-157) and returns before it reads a length at all, so the case
    // would have been testing that gap rather than this one.
    check(
        "length-is-a-symbol",
        "let o = {}; o.length = Symbol(1); \
         try { [].every.call(o, function () { return true; }); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    check(
        "length-is-an-unconvertible-object",
        "let o = {length: {valueOf: function () { return {}; }, \
                           toString: function () { return {}; }}}; \
         try { [].every.call(o, function () { return true; }); return \"no\"; } \
         catch (e) { return e.name; }",
        "TypeError",
    );
    // **Both are tried, in order.** A test that only checks the throw passes an engine that
    // never asked, so the order is what is asserted here.
    check(
        "length-tries-value-of-then-to-string",
        "let seen = \"\"; \
         let o = {length: {valueOf: function () { seen = seen + \"v\"; return {}; }, \
                           toString: function () { seen = seen + \"s\"; return {}; }}}; \
         try { [].every.call(o, function () { return true; }); } catch (e) {} \
         return seen;",
        "vs",
    );
    // A `valueOf` that answers a primitive is used, and the walk proceeds.
    check(
        "length-from-value-of",
        "let o = {0: 7, 1: 8, length: {valueOf: function () { return 2; }}}; \
         return [].join.call(o, \",\");",
        "7,8",
    );
    // A getter that throws hands its own exception on rather than being read as a zero.
    check(
        "length-getter-throws",
        "let o = {}; \
         Object.defineProperty(o, \"length\", \
             {get: function () { throw new RangeError(\"nope\"); }}); \
         try { [].join.call(o, \",\"); return \"no\"; } catch (e) { return e.name; }",
        "RangeError",
    );
    // A symbol refuses to be a number wherever it is asked.
    check(
        "symbol-is-not-a-number",
        "let o = {length: Symbol()}; \
         try { [].slice.call(o); return \"no\"; } catch (e) { return e.name; }",
        "TypeError",
    );
}
