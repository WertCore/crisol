//! Evaluation order, and what a cycle does to it.
//!
//! The orders asserted here are the ES module specification's, not this implementation's
//! preference: depth-first post-order, imports in source order, each module once. Where a test
//! pins an order it says which rule produces it, so that changing the walk fails with a reason
//! rather than a diff.

use crisol_frontend::{ModuleGraph, ModuleId};

/// Builds a graph from `(importer, [imported…])` pairs and returns the entry's id.
///
/// The first pair names the entry, because evaluation order is always relative to one.
fn build(edges: &[(&str, &[&str])]) -> (ModuleGraph, ModuleId) {
    let mut graph = ModuleGraph::new();
    let entry = graph.intern(edges[0].0);
    for (from, imports) in edges {
        let from = graph.intern(from);
        for import in *imports {
            let to = graph.intern(import);
            graph.add_import(from, to);
        }
    }
    (graph, entry)
}

fn names(graph: &ModuleGraph, order: &[ModuleId]) -> Vec<String> {
    order
        .iter()
        .map(|id| graph.specifier(*id).unwrap_or("?").to_owned())
        .collect()
}

// ---- ordering ---------------------------------------------------------------------------

#[test]
fn a_chain_evaluates_from_the_bottom_up() {
    let (graph, entry) = build(&[("a", ["b"].as_slice()), ("b", &["c"]), ("c", &[])]);
    assert_eq!(
        names(&graph, &graph.evaluation_order(entry)),
        ["c", "b", "a"],
        "a module's imports evaluate before it does"
    );
}

#[test]
fn imports_evaluate_in_source_order() {
    let (graph, entry) = build(&[("a", ["b", "c"].as_slice()), ("b", &[]), ("c", &[])]);
    assert_eq!(
        names(&graph, &graph.evaluation_order(entry)),
        ["b", "c", "a"]
    );

    // The same modules, imported the other way round, is a different program.
    let (graph, entry) = build(&[("a", ["c", "b"].as_slice()), ("b", &[]), ("c", &[])]);
    assert_eq!(
        names(&graph, &graph.evaluation_order(entry)),
        ["c", "b", "a"],
        "source order decides, so sorting or deduplicating imports would be wrong"
    );
}

#[test]
fn a_diamond_evaluates_its_shared_dependency_once_and_first() {
    let (graph, entry) = build(&[
        ("a", ["b", "c"].as_slice()),
        ("b", &["d"]),
        ("c", &["d"]),
        ("d", &[]),
    ]);
    assert_eq!(
        names(&graph, &graph.evaluation_order(entry)),
        ["d", "b", "c", "a"],
        "d is reached through b first and must not evaluate again through c"
    );
}

#[test]
fn importing_the_same_module_twice_evaluates_it_once() {
    // `import {a} from "./m"; import {b} from "./m"` is two import records, one module.
    let (graph, entry) = build(&[("a", ["m", "m"].as_slice()), ("m", &[])]);
    assert_eq!(graph.imports(entry).len(), 2, "both records are kept");
    assert_eq!(names(&graph, &graph.evaluation_order(entry)), ["m", "a"]);
}

#[test]
fn a_module_nothing_reaches_does_not_evaluate() {
    let mut g = ModuleGraph::new();
    let entry = g.intern("a");
    let b = g.intern("b");
    g.add_import(entry, b);
    g.intern("orphan");

    let order = g.evaluation_order(entry);
    assert_eq!(names(&g, &order), ["b", "a"]);
    assert_eq!(g.len(), 3, "the orphan is in the graph");
}

// ---- cycles, which are legal --------------------------------------------------------------

#[test]
fn a_cycle_is_ordered_rather_than_rejected() {
    // `react` and `react-dom` have shipped cycles for years. A graph that refused one would
    // refuse to build most real programs.
    let (graph, entry) = build(&[("a", ["b"].as_slice()), ("b", &["a"])]);
    let order = graph.evaluation_order(entry);
    assert_eq!(
        names(&graph, &order),
        ["b", "a"],
        "b closes the cycle back to a, which is already being visited, so b finishes first"
    );
    assert_eq!(order.len(), 2, "each module once, even in a cycle");
}

#[test]
fn a_cycle_is_reported() {
    let (graph, _) = build(&[("a", ["b"].as_slice()), ("b", &["a"])]);
    let cycles = graph.cycles();
    assert_eq!(cycles.len(), 1);
    assert_eq!(cycles[0].len(), 2);
    let mut named = names(&graph, &cycles[0]);
    named.sort();
    assert_eq!(named, ["a", "b"]);
}

#[test]
fn a_module_outside_a_cycle_is_not_reported_as_one() {
    // The failure this guards: reporting every strongly connected component, including the
    // components of one that every acyclic module forms, which makes the report useless.
    let (graph, _) = build(&[("a", ["b"].as_slice()), ("b", &["c"]), ("c", &[])]);
    assert!(graph.cycles().is_empty(), "a chain has no cycle");
}

#[test]
fn a_module_that_imports_itself_is_a_cycle() {
    let (graph, entry) = build(&[("a", ["a"].as_slice())]);
    let cycles = graph.cycles();
    assert_eq!(cycles.len(), 1, "a self-import is a cycle of one");
    assert_eq!(cycles[0], [entry]);
    assert!(graph.in_cycle(entry));
    assert_eq!(names(&graph, &graph.evaluation_order(entry)), ["a"]);
}

#[test]
fn only_the_modules_in_a_cycle_are_in_it() {
    // a -> b <-> c, and d off to the side.
    let (graph, _) = build(&[
        ("a", ["b"].as_slice()),
        ("b", &["c"]),
        ("c", &["b"]),
        ("d", &[]),
    ]);
    let cycles = graph.cycles();
    assert_eq!(cycles.len(), 1);
    let mut named = names(&graph, &cycles[0]);
    named.sort();
    assert_eq!(named, ["b", "c"], "a imports the cycle but is not in it");

    let a = graph.cycles().iter().flatten().copied().collect::<Vec<_>>();
    for name in ["a", "d"] {
        let id = (0..graph.len())
            .map(|raw| u32::try_from(raw).expect("fits"))
            .find(|raw| graph.specifier(module_id(&graph, *raw)) == Some(name))
            .map(|raw| module_id(&graph, raw))
            .expect("present");
        assert!(!a.contains(&id), "{name} is not in a cycle");
        assert!(!graph.in_cycle(id));
    }
}

/// The `ModuleId` for a raw index, which only the tests need.
fn module_id(graph: &ModuleGraph, raw: u32) -> ModuleId {
    let _ = graph;
    ModuleId::from_index(raw)
}

#[test]
fn two_separate_cycles_are_two_reports() {
    let mut g = ModuleGraph::new();
    for (from, imports) in [
        ("entry", ["a", "c"].as_slice()),
        ("a", &["b"]),
        ("b", &["a"]),
        ("c", &["d"]),
        ("d", &["c"]),
    ] {
        let from = g.intern(from);
        for import in imports {
            let to = g.intern(import);
            g.add_import(from, to);
        }
    }
    assert_eq!(g.cycles().len(), 2);
}

// ---- the shape of the input --------------------------------------------------------------

#[test]
fn interning_the_same_specifier_gives_the_same_module() {
    let mut g = ModuleGraph::new();
    let first = g.intern("./shared.js");
    let second = g.intern("./shared.js");
    assert_eq!(first, second, "every importer asks for it by name");
    assert_eq!(g.len(), 1);
}

#[test]
fn a_deep_chain_does_not_overflow_the_stack() {
    // The input this guards against is somebody's `node_modules`, whose depth is not this
    // code's to bound — which is why both walks are iterative rather than recursive.
    const DEPTH: usize = 100_000;
    let mut g = ModuleGraph::new();
    let mut previous = g.intern("m0");
    let entry = previous;
    for at in 1..DEPTH {
        let next = g.intern(&format!("m{at}"));
        g.add_import(previous, next);
        previous = next;
    }

    let order = g.evaluation_order(entry);
    assert_eq!(order.len(), DEPTH);
    assert_eq!(g.specifier(order[0]), Some("m99999"), "the deepest first");
    assert_eq!(g.specifier(order[DEPTH - 1]), Some("m0"), "the entry last");
    assert!(g.cycles().is_empty(), "and Tarjan survives it too");
}

#[test]
fn an_empty_graph_is_empty() {
    let g = ModuleGraph::new();
    assert!(g.is_empty());
    assert_eq!(g.len(), 0);
    assert!(g.cycles().is_empty());
}
