//! §M10's acceptance: a real `node_modules` tree containing React, resolved and parsed, with
//! no unresolved imports.
//!
//! **Real, not a fixture.** A hand-built tree would satisfy the sentence and miss the point:
//! what makes this an acceptance is `package.json` `exports` maps, conditions, CJS entry
//! points and a dependency that lives in another package — none of which anyone writes into a
//! fixture the way npm actually lays them out.
//!
//! # Skipping is a failure when it matters
//!
//! `CRISOL_NODE_MODULES_ROOT` names a directory containing `node_modules`. Without it this
//! test skips, because a contributor should not need an npm install to run `cargo test`. But
//! `CRISOL_REQUIRE_NODE_MODULES=1` makes its absence a *failure*, and CI sets both — the same
//! arrangement as `CRISOL_REQUIRE_GPU` and `CRISOL_REQUIRE_FONTS`, and for the reason the
//! workflow already gives: a run where everything skipped is indistinguishable from a run
//! where everything passed.

use std::path::MAIN_SEPARATOR as MAIN_SEP;
use std::path::PathBuf;

use crisol_frontend::Loader;

/// The tree to test against, or `None` when there is none and none was required.
fn tree() -> Option<PathBuf> {
    match std::env::var("CRISOL_NODE_MODULES_ROOT") {
        Ok(root) => {
            let root = PathBuf::from(root);
            assert!(
                root.join("node_modules").is_dir(),
                "CRISOL_NODE_MODULES_ROOT={} has no node_modules in it",
                root.display()
            );
            Some(root)
        }
        Err(_) => {
            assert!(
                std::env::var("CRISOL_REQUIRE_NODE_MODULES").is_err(),
                "CRISOL_REQUIRE_NODE_MODULES is set and CRISOL_NODE_MODULES_ROOT is not, so the \
                 acceptance would have silently skipped"
            );
            None
        }
    }
}

fn names(loader: &Loader) -> Vec<String> {
    (0..loader.graph().len())
        .filter_map(|raw| {
            let id = crisol_frontend::ModuleId::from_index(u32::try_from(raw).ok()?);
            loader.graph().specifier(id).map(str::to_owned)
        })
        .collect()
}

#[test]
fn a_real_react_tree_resolves_and_parses_with_no_unresolved_imports() {
    let Some(root) = tree() else {
        return;
    };

    // `react-dom/client` rather than `react-dom`: it is what an application actually imports,
    // it exercises a subpath in the `exports` map rather than the bare package, and it is the
    // entry that reaches `scheduler` — the top-level one does not, which is a fact about React
    // rather than about this loader and was checked before being asserted.
    let mut loader = Loader::new();
    let entry = loader
        .load_specifier(&root, "react-dom/client")
        .expect("react-dom/client should resolve from a tree that has it");

    // ---- the acceptance itself ----
    assert!(
        loader.unresolved().is_empty(),
        "every specifier must resolve, and these did not: {:?}",
        loader.unresolved()
    );

    let graph = loader.graph();
    assert!(
        graph.len() >= 5,
        "a react-dom tree is more than a handful of files: {} found",
        graph.len()
    );
    assert_eq!(
        loader.parsed(),
        graph.len(),
        "every module in the graph should have been parsed"
    );
    assert_eq!(
        graph.evaluation_order(entry).len(),
        graph.len(),
        "everything in the graph is reachable from the entry, because that is how it got there"
    );

    // ---- it really crossed package boundaries ----
    let found = names(&loader);
    let has = |needle: &str| found.iter().any(|name| name.contains(needle));
    assert!(has("react-dom"), "the entry package");
    assert!(
        has(&format!("node_modules{}react{}", MAIN_SEP, MAIN_SEP)),
        "react itself, reached from react-dom by bare specifier: {found:#?}"
    );
    assert!(
        has("scheduler"),
        "and scheduler, which only react-dom's internals ask for: {found:#?}"
    );

    // ---- CJS was actually followed ----
    //
    // React 19 is CommonJS throughout: `module.exports = require('./cjs/react.production.js')`.
    // A loader that understood only `import` would find no edges here at all and then report a
    // complete graph with no unresolved imports — passing this test by doing nothing. Asserting
    // the cjs files are present is what makes that impossible.
    assert!(
        has("cjs"),
        "the cjs bundles are where React's code actually is: {found:#?}"
    );

    // ---- §3.5, visible in the graph ----
    //
    // `if (process.env.NODE_ENV === 'production') require(A) else require(B)` puts *both*
    // bundles in the graph, which is correct for a graph — it records what could be imported.
    // §3.5 is about the build defining NODE_ENV and eliminating one of them, and that is an
    // optimisation pass's job (M12), not this one's. Recorded here because the graph is where
    // the two branches first become visible.
    assert!(
        has("production") && has("development"),
        "both NODE_ENV branches should be in the graph: {found:#?}"
    );
}

#[test]
fn react_alone_resolves_too() {
    let Some(root) = tree() else {
        return;
    };
    let mut loader = Loader::new();
    let entry = loader
        .load_specifier(&root, "react")
        .expect("react should resolve");
    assert!(loader.unresolved().is_empty(), "{:?}", loader.unresolved());
    assert!(
        loader.graph().len() >= 3,
        "index.js plus both NODE_ENV bundles"
    );
    assert!(!loader.graph().evaluation_order(entry).is_empty());
}
