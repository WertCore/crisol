//! The test262 harness.
//!
//! §M12's acceptance is *"the relevant `test262` subset passes at >80% for implemented
//! builtins"*. This is the machinery for that, and it reports an uncomfortable fact rather than
//! hiding it:
//!
//! > **Every test262 test is a JavaScript program that must be executed, and crisol cannot
//! > execute JavaScript yet.**
//!
//! `compiler/codegen` is a stub scheduled for M13 and there is no interpreter. So the pass rate
//! is not 0% — it is *undefined*, because nothing has been attempted. Reporting "0 of 12,719
//! passing" would imply the tests ran and failed, which is a different and less accurate claim
//! than "there is no way to run them".
//!
//! What this harness does now is **measure the acceptance**: discover every applicable case,
//! parse its metadata, and report what running them would require. That turns "the acceptance
//! is not met" from an admission into a number, and the discovery and parsing are real work
//! that M13 does not have to redo.
//!
//! # Getting the suite
//!
//! ```text
//! git clone --depth 1 --filter=blob:none --sparse https://github.com/tc39/test262.git
//! cd test262 && git sparse-checkout set harness test/built-ins/Object test/built-ins/Array …
//! ```
//!
//! Point `CRISOL_TEST262_ROOT` at it. Absent, these tests **skip** — the same arrangement as
//! `CRISOL_REQUIRE_GPU` and `CRISOL_REQUIRE_NODE_MODULES` — and `CRISOL_REQUIRE_TEST262=1`
//! turns the absence into a failure so CI cannot pass by finding nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a test262 file declares about itself.
#[derive(Debug, Default, PartialEq, Eq)]
struct Metadata {
    /// Required. Every file in the corpus has one.
    description: String,
    /// Harness files that must be loaded first, beyond the always-included ones.
    includes: Vec<String>,
    /// `onlyStrict`, `noStrict`, `module`, `raw`, `async`, `CanBlockIsFalse`, …
    flags: Vec<String>,
    /// Language features the test needs, which is how a runner decides to skip.
    features: Vec<String>,
    /// Present when the test is *expected* to fail, with the phase and error type.
    negative: Option<(String, String)>,
}

/// Parses the `/*--- … ---*/` block.
///
/// Hand-written rather than pulling in a YAML crate: the block is a **restricted** subset —
/// scalars, `|` block text, and `[a, b]` flow sequences — and a general parser would accept
/// documents test262 does not contain while still needing this much glue. The risk is the
/// reverse (silently mis-parsing something unusual), which is why
/// `every_file_in_the_corpus_parses` runs over all 12,000-odd files rather than a sample.
fn parse(source: &str) -> Option<Metadata> {
    let start = source.find("/*---")? + 5;
    let end = source[start..].find("---*/")? + start;
    let block = &source[start..end];

    let mut meta = Metadata::default();
    let mut lines = block.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_end();
        // Only column-zero keys are fields; anything indented belongs to a block scalar above.
        let Some((key, rest)) = trimmed.split_once(':') else {
            continue;
        };
        if key.starts_with(char::is_whitespace) || key.is_empty() {
            continue;
        }
        let value = rest.trim();
        match key {
            "description" => meta.description = value.to_owned(),
            "includes" => meta.includes = flow_sequence(value),
            "flags" => meta.flags = flow_sequence(value),
            "features" => meta.features = flow_sequence(value),
            "negative" => {
                // A nested mapping over the next lines.
                let mut phase = String::new();
                let mut kind = String::new();
                while let Some(next) = lines.peek() {
                    if !next.starts_with(char::is_whitespace) {
                        break;
                    }
                    let inner = lines.next().unwrap_or_default();
                    if let Some((name, val)) = inner.trim().split_once(':') {
                        match name.trim() {
                            "phase" => phase = val.trim().to_owned(),
                            "type" => kind = val.trim().to_owned(),
                            _ => {}
                        }
                    }
                }
                meta.negative = Some((phase, kind));
            }
            _ => {}
        }
    }
    Some(meta)
}

/// `[a, b, c]`, or a bare scalar, or a following block list.
fn flow_sequence(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Where the suite is, if it is anywhere.
fn suite_root() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("CRISOL_TEST262_ROOT") {
        let path = PathBuf::from(root);
        return path.join("test").is_dir().then_some(path);
    }
    // The conventional sibling checkout, so a developer who cloned it where the docs say does
    // not also have to set a variable.
    let sibling = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../test262");
    sibling.join("test").is_dir().then_some(sibling)
}

/// Every `.js` case under `test/built-ins`, excluding fixtures.
///
/// `_FIXTURE.js` files are *imported by* other tests and are not cases themselves. Counting
/// them would inflate the denominator, which is the wrong direction to be wrong in for a
/// percentage-based acceptance.
fn discover(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.join("test/built-ins")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "js")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("_FIXTURE"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn skip_or_root() -> Option<PathBuf> {
    if let Some(root) = suite_root() {
        return Some(root);
    }
    assert!(
        std::env::var("CRISOL_REQUIRE_TEST262").is_err(),
        "CRISOL_REQUIRE_TEST262 is set and no suite was found — see this file's docs. \
         CI must not pass by finding nothing."
    );
    None
}

#[test]
fn every_file_in_the_corpus_parses() {
    // Over the whole corpus rather than a sample, because the parser is hand-written against a
    // restricted YAML subset and the risk it carries is silently mis-parsing something unusual.
    let Some(root) = skip_or_root() else {
        return;
    };
    let cases = discover(&root);
    assert!(
        cases.len() > 1000,
        "suspiciously few cases: {}",
        cases.len()
    );

    let mut failures = Vec::new();
    for case in &cases {
        let Ok(source) = std::fs::read_to_string(case) else {
            failures.push(format!("{}: unreadable", case.display()));
            continue;
        };
        match parse(&source) {
            None => failures.push(format!("{}: no frontmatter block", case.display())),
            Some(meta) if meta.description.is_empty() => {
                failures.push(format!("{}: no description", case.display()));
            }
            Some(_) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} files did not parse:\n{}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_acceptance_is_measured_rather_than_asserted() {
    // §M12 wants >80%. Nothing can be run, so the honest output is a census of what running
    // would involve — not a pass rate, and not "0 passing", which would imply the tests ran.
    let Some(root) = skip_or_root() else {
        return;
    };
    let cases = discover(&root);

    let mut needs_includes = 0;
    let mut negative = 0;
    let mut module = 0;
    let mut only_strict = 0;
    let mut is_async = 0;
    let mut features: BTreeMap<String, usize> = BTreeMap::new();

    for case in &cases {
        let Ok(source) = std::fs::read_to_string(case) else {
            continue;
        };
        let Some(meta) = parse(&source) else { continue };
        if !meta.includes.is_empty() {
            needs_includes += 1;
        }
        if meta.negative.is_some() {
            negative += 1;
        }
        if meta.flags.iter().any(|flag| flag == "module") {
            module += 1;
        }
        if meta.flags.iter().any(|flag| flag == "onlyStrict") {
            only_strict += 1;
        }
        if meta.flags.iter().any(|flag| flag == "async") {
            is_async += 1;
        }
        for feature in meta.features {
            *features.entry(feature).or_default() += 1;
        }
    }

    let mut common: Vec<(&String, &usize)> = features.iter().collect();
    common.sort_by(|a, b| b.1.cmp(a.1));

    println!("\n--- test262 census for the implemented builtins ---");
    println!("cases discovered:        {}", cases.len());
    println!("  need extra includes:   {needs_includes}");
    println!("  expect failure:        {negative}");
    println!("  module goal:           {module}");
    println!("  strict only:           {only_strict}");
    println!("  async:                 {is_async}");
    println!("  distinct features:     {}", features.len());
    println!("  most common features:");
    for (name, count) in common.iter().take(8) {
        println!("    {count:5}  {name}");
    }
    println!("runnable today:          0  (no execution engine; codegen is M13)");
    println!("pass rate:               undefined, not 0% — nothing has been attempted\n");

    // The only assertion that means anything today: the harness found a real corpus and read
    // it. The pass rate becomes assertable when there is something to run the cases with.
    assert!(cases.len() > 1000);
    assert!(!features.is_empty(), "features should have been parsed");
}

#[test]
fn the_parser_handles_the_shapes_the_corpus_uses() {
    // Unit coverage for the parser itself, so a corpus-wide failure can be localised rather
    // than bisected across 12,000 files.
    let simple = parse("/*---\ndescription: a thing\n---*/\n").expect("parsed");
    assert_eq!(simple.description, "a thing");
    assert!(simple.includes.is_empty());

    let rich = parse(
        "/*---\nesid: sec-x\ndescription: another\nfeatures: [Proxy, Reflect]\n\
         flags: [onlyStrict]\nincludes: [compareArray.js]\n---*/\n",
    )
    .expect("parsed");
    assert_eq!(rich.features, ["Proxy", "Reflect"]);
    assert_eq!(rich.flags, ["onlyStrict"]);
    assert_eq!(rich.includes, ["compareArray.js"]);

    let with_block = parse(
        "/*---\ndescription: has info\ninfo: |\n    description: not a field\n    features: [nope]\n\
         features: [real]\n---*/\n",
    )
    .expect("parsed");
    assert_eq!(
        with_block.description, "has info",
        "an indented line inside a block scalar must not be read as a field"
    );
    assert_eq!(with_block.features, ["real"]);

    let failing = parse(
        "/*---\ndescription: expected to fail\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/\n",
    )
    .expect("parsed");
    assert_eq!(
        failing.negative,
        Some(("parse".to_owned(), "SyntaxError".to_owned()))
    );

    assert_eq!(parse("no frontmatter here"), None);
}
