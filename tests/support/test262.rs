//! Reading a test262 case: its metadata, and where the suite is.
//!
//! Shared by two harnesses that cannot be one crate — the census in `crisol-builtins`, which
//! must not drag in the compiler, and the runner in `crisol`, which must. Included with
//! `#[path]` by both rather than copied: a second parser that disagreed with this one would
//! skip a different set of tests and quietly change the number each reports.
#![allow(dead_code, reason = "each harness uses a different part of this")]

use std::path::{Path, PathBuf};

/// What a test262 file declares about itself.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Required. Every file in the corpus has one.
    pub description: String,
    /// Harness files that must be loaded first, beyond the always-included ones.
    pub includes: Vec<String>,
    /// `onlyStrict`, `noStrict`, `module`, `raw`, `async`, `CanBlockIsFalse`, …
    pub flags: Vec<String>,
    /// Language features the test needs, which is how a runner decides to skip.
    pub features: Vec<String>,
    /// Present when the test is *expected* to fail, with the phase and error type.
    pub negative: Option<(String, String)>,
}

/// Parses the `/*--- … ---*/` block.
///
/// Hand-written rather than pulling in a YAML crate: the block is a **restricted** subset —
/// scalars, `|` block text, and `[a, b]` flow sequences — and a general parser would accept
/// documents test262 does not contain while still needing this much glue. The risk is the
/// reverse (silently mis-parsing something unusual), which is why
/// `every_file_in_the_corpus_parses` runs over all 12,000-odd files rather than a sample.
pub fn parse(source: &str) -> Option<Metadata> {
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
pub fn flow_sequence(value: &str) -> Vec<String> {
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
pub fn suite_root() -> Option<PathBuf> {
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
pub fn discover(root: &Path) -> Vec<PathBuf> {
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

pub fn skip_or_root() -> Option<PathBuf> {
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
