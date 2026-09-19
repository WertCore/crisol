//! Running test262, rather than only counting it.
//!
//! §M12's acceptance is *"the relevant `test262` subset passes at >80% for implemented
//! builtins"*. Until M13 there was no way to attempt a single case, so the companion harness in
//! `crisol-builtins` reports a census instead — what running *would* involve. Compiled programs
//! now run, so this attempts them and reports what actually happens.
//!
//! # What "passing" means here
//!
//! A test262 case is a program that throws on failure and returns normally on success. crisol
//! has no throw path yet, so **a case that runs to completion has not necessarily passed** — it
//! may have reached an assertion it could not signal. This runner therefore reports three
//! numbers and does not collapse them:
//!
//! - **refused** — the compiler said what it could not handle. The honest majority today.
//! - **crashed** — it built and then died, which is a bug here rather than a missing feature.
//! - **ran** — it built and exited cleanly.
//!
//! `ran` is an *upper bound* on the pass rate, not the pass rate. Reporting it as a pass rate
//! would be the same overclaim the census harness exists to avoid, one stage later.
//!
//! Point `CRISOL_TEST262_ROOT` at a checkout. Absent, this skips; `CRISOL_REQUIRE_TEST262=1`
//! turns the absence into a failure so CI cannot pass by finding nothing.

#[path = "../../tests/support/test262.rs"]
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use support::{Metadata, discover, parse, skip_or_root};

/// How many cases to attempt unless told otherwise.
///
/// Every case that compiles also links and runs, which costs a `cc` invocation — so the whole
/// corpus is minutes rather than seconds. The default is a sample large enough for the
/// proportions to mean something; `CRISOL_TEST262_ALL=1` runs all of them.
///
/// **The cost grows as the engine improves**, which is not obvious: a refused case costs
/// milliseconds and a compiled one costs a `cc` invocation, so the run gets slower every time
/// something stops being refused. `CRISOL_TEST262_SAMPLE` exists for that — a smaller sample
/// while iterating, with the default kept for any number that gets reported, because two
/// sample sizes are two different measurements and comparing them says nothing.
const SAMPLE: usize = 400;

/// The sample size to use, honouring `CRISOL_TEST262_SAMPLE`.
fn sample_size() -> usize {
    std::env::var("CRISOL_TEST262_SAMPLE")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(SAMPLE)
}

/// What happened to one case.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    /// The compiler named something it does not handle.
    Refused(String),
    /// It built and then threw — which is how a test262 case reports failure. Carries what
    /// was thrown, because the *reason* is the list worth reading once cases start running.
    Failed(String),
    /// It built and then died on a signal, which is a bug here rather than a wrong answer.
    Crashed,
    /// It built, ran and returned normally. **This is a pass.**
    Ran,
}

fn runtime_archive() -> Option<PathBuf> {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
    for profile in ["debug", "release"] {
        let candidate = target.join(profile).join("libcrisol_abi.a");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The source a case runs as: the always-included harness, its own includes, then the test.
fn assemble(root: &Path, case: &Path, metadata: &Metadata) -> Option<String> {
    let body = std::fs::read_to_string(case).ok()?;
    if metadata.flags.iter().any(|flag| flag == "raw") {
        return Some(body);
    }
    let mut source = String::new();
    for include in ["assert.js", "sta.js"]
        .iter()
        .map(|name| (*name).to_owned())
        .chain(metadata.includes.iter().cloned())
    {
        let path = root.join("harness").join(&include);
        source.push_str(&std::fs::read_to_string(path).ok()?);
        source.push('\n');
    }
    source.push_str(&body);
    Some(source)
}

fn attempt(root: &Path, case: &Path, work: &Path, index: usize) -> Option<Outcome> {
    let metadata = parse(&std::fs::read_to_string(case).ok()?)?;
    // Module and async cases need machinery no part of this pipeline has; counting them as
    // refusals would inflate the refusal reasons with something never attempted.
    if metadata
        .flags
        .iter()
        .any(|flag| flag == "module" || flag == "async")
    {
        return None;
    }
    let source = assemble(root, case, &metadata)?;

    let directory = work.join(format!("case{index}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).ok()?;
    let file = directory.join("main.js");
    std::fs::write(&file, &source).ok()?;
    let binary = directory.join("main");

    let outcome = match crisol::build::build(&file, &binary, &runtime_archive()?) {
        Err(error) => Outcome::Refused(stage_of(&error)),
        Ok(()) => match Command::new(&binary).output() {
            Ok(output) if output.status.success() => Outcome::Ran,
            // Exit 1 is the entry point reporting an uncaught throw, which is exactly how a
            // case signals a failed assertion. Anything else — a signal, a panic — is ours.
            Ok(output) if output.status.code() == Some(1) => {
                Outcome::Failed(thrown_reason(&String::from_utf8_lossy(&output.stderr)))
            }
            _ => Outcome::Crashed,
        },
    };
    let _ = std::fs::remove_dir_all(&directory);
    Some(outcome)
}

/// Which built-in a case is testing, from its path.
///
/// The thrown message says `is not a function` without naming what was called — the callee is a
/// value, and by the time the call fails nothing holds the name it was read from. The path does
/// know, and it is what turns 90 identical failures into a list of what to write.
fn area_of(case: &Path) -> String {
    let parts: Vec<&str> = case
        .iter()
        .filter_map(|part| part.to_str())
        .skip_while(|part| *part != "built-ins")
        .skip(1)
        .collect();
    match parts.as_slice() {
        [] => "unknown".to_owned(),
        [one] => (*one).to_owned(),
        [one, two, ..] if *two == "prototype" => format!("{one}.prototype"),
        [one, ..] => (*one).to_owned(),
    }
}

/// What a failing case threw, reduced to something worth counting.
///
/// The entry point prints `uncaught: …`. A test262 assertion message names the value it saw, so
/// the messages are nearly all distinct — grouping needs the *shape* of the complaint rather
/// than its text, or every case is its own row and the list says nothing.
fn thrown_reason(stderr: &str) -> String {
    let line = stderr
        .lines()
        .find(|line| line.starts_with("uncaught: "))
        .map_or("nothing", |line| &line["uncaught: ".len()..])
        .trim();
    // Grouped by the *shape* of the complaint, not its text. test262's messages name the value
    // they saw — "Expected SameValue(«undefined», «1») to be true" — so keeping them whole puts
    // every case in its own row and the ranking says nothing.
    let trimmed = line.trim_start_matches("Test262Error: ");
    let shape: String = trimmed
        .split_whitespace()
        .take_while(|word| !word.starts_with('\u{ab}'))
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    if shape.is_empty() {
        trimmed.chars().take(60).collect()
    } else {
        shape
    }
}

/// Which stage refused, and the first construct it named.
///
/// Grouped by construct rather than by message, because the useful output is a ranked list of
/// what to implement next — not a histogram of error strings.
fn stage_of(error: &crisol::build::BuildError) -> String {
    match error {
        crisol::build::BuildError::Unsupported { constructs } => constructs.first().map_or_else(
            || "unsupported".to_owned(),
            |first| first.split(" (byte").next().unwrap_or(first).to_owned(),
        ),
        crisol::build::BuildError::Parse { .. } => "parse".to_owned(),
        crisol::build::BuildError::Malformed { .. } => "malformed IR".to_owned(),
        // The backend's own message, not just "codegen": it names the construct it could not
        // lower, and that is the list worth reading. The function name prefix is stripped so
        // the same refusal from different functions groups together.
        crisol::build::BuildError::Codegen { message } => {
            let reason = message.rsplit(": ").next().unwrap_or(message);
            // Every distinct string literal would otherwise be its own row, which buries the
            // one fact worth reading: string constants are unsupported, once.
            if reason.starts_with("cannot compile Const(String(") {
                return "codegen: string constant".to_owned();
            }
            format!("codegen: {reason}")
        }
        crisol::build::BuildError::Link { .. } => "link".to_owned(),
        crisol::build::BuildError::Unreadable { .. } => "unreadable".to_owned(),
    }
}

#[test]
fn the_suite_is_attempted_and_the_result_reported() {
    let Some(root) = skip_or_root() else {
        return;
    };
    assert!(
        runtime_archive().is_some() || std::env::var("CRISOL_REQUIRE_TEST262").is_err(),
        "CRISOL_REQUIRE_TEST262 is set and libcrisol_abi.a was not found. \
         Build it first: cargo build -p crisol-abi"
    );
    if runtime_archive().is_none() {
        return;
    }

    let cases = discover(&root);
    assert!(!cases.is_empty(), "the suite root has no cases");
    let limit = if std::env::var("CRISOL_TEST262_ALL").is_ok() {
        cases.len()
    } else {
        sample_size().min(cases.len())
    };
    // Evenly spaced rather than the first N: the corpus is ordered by directory, so a prefix
    // is entirely `Array` and says nothing about the rest.
    let step = cases.len().div_ceil(limit);
    let work = std::env::temp_dir().join("crisol-test262");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("a working directory");

    let mut attempted = 0usize;
    let mut ran = 0usize;
    let mut failed = 0usize;
    let mut crashed = Vec::new();
    let mut refusals: BTreeMap<String, usize> = BTreeMap::new();
    let mut passing = Vec::new();
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut areas: BTreeMap<String, usize> = BTreeMap::new();
    let mut examples: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();

    for (index, case) in cases.iter().step_by(step).enumerate() {
        let Some(outcome) = attempt(&root, case, &work, index) else {
            continue;
        };
        attempted += 1;
        match outcome {
            Outcome::Ran => {
                ran += 1;
                passing.push(case.clone());
            }
            Outcome::Failed(reason) => {
                failed += 1;
                let reason_key = reason.clone();
                *failures.entry(reason).or_default() += 1;
                *areas.entry(area_of(case)).or_default() += 1;
                examples.entry(reason_key).or_default().push(case.clone());
            }
            Outcome::Crashed => crashed.push(case.clone()),
            Outcome::Refused(reason) => *refusals.entry(reason).or_default() += 1,
        }
    }
    let _ = std::fs::remove_dir_all(&work);

    let mut ranked: Vec<(&String, &usize)> = refusals.iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));

    println!(
        "test262: {attempted} attempted of {} discovered",
        cases.len()
    );
    println!("  passed:   {ran}");
    println!("  failed:   {failed}");
    println!("  crashed:  {}", crashed.len());
    println!("  refused:  {}", attempted - ran - failed - crashed.len());
    println!("what the compiler refused, most common first:");
    for (reason, count) in ranked.iter().take(20) {
        println!("  {count:>5}  {reason}");
    }
    let mut by_reason: Vec<(&String, &usize)> = failures.iter().collect();
    by_reason.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
    println!("what ran and then threw, most common first:");
    for (reason, count) in by_reason.iter().take(20) {
        println!("  {count:>5}  {reason}");
    }
    let mut by_area: Vec<(&String, &usize)> = areas.iter().collect();
    by_area.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
    // A count says how much and a path says what. The thrown message cannot name the method
    // that was missing — the callee is a value by then — so the cases themselves have to.
    if let Some((reason, _)) = by_reason.first()
        && let Some(cases) = examples.get(*reason)
    {
        println!("examples of the most common failure ({reason}):");
        for case in cases.iter().take(8) {
            println!("  {}", case.display());
        }
    }
    println!("which built-ins those failures are testing:");
    for (area, count) in by_area.iter().take(20) {
        println!("  {count:>5}  {area}");
    }
    // Named rather than counted, so a fall in the number can be read as "these stopped
    // passing" rather than taken on trust. A fix that makes a case *correctly* fail looks
    // exactly like a regression in the total.
    println!("what passed:");
    for case in &passing {
        println!("  {}", case.display());
    }
    for case in crashed.iter().take(10) {
        println!("crashed: {}", case.display());
    }

    // **A case that built and then died is a bug here**, not a missing feature, and is the one
    // outcome that must not be allowed to accumulate quietly.
    assert!(
        crashed.is_empty(),
        "{} case(s) compiled and then crashed",
        crashed.len()
    );
}
