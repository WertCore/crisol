//! Per-node memory.
//!
//! M8's acceptance is an RSS number, which makes the size of a `Node` a tracked figure
//! rather than an implementation detail. A field added without noticing costs a megabyte
//! every three thousand nodes.
//!
//! **Arena is not the memory story, and the figure below should not be read as one.** A node
//! that is styled, laid out and shaped costs about **16 KB** once taffy's box tree and its
//! shaped text are counted (`ui/umbrella/examples/bigresponse.rs`); the arena is roughly 6% of
//! that. D-47 sized a response from this number alone and came out 24x low. What this test
//! guards is drift in a field nobody costed — not the budget.

use std::mem::size_of;

use crisol_tree::{Atom, Attribute, BoxStyle, ElementData, Node, NodeId, NodeKind};

/// What a node may grow to before somebody has to justify it.
///
/// Not a tight bound — a bound that has to move on every change teaches nothing. This one
/// leaves headroom over the measured size (328 bytes when it was set; 344 since the `style`
/// attribute's slot, D-50), so it catches a field nobody costed rather than ordinary drift.
const BUDGET: usize = 400;

#[test]
fn a_node_stays_within_its_memory_budget() {
    let node = size_of::<Node>();
    println!(
        "\n  Node {node}  =  NodeKind {} + BoxStyle {} + links {} + geometry {} + flags\n\
           \x20 100k nodes: {:.1} MB of arena, which is about 6% of what they cost once\n\
           \x20 styled, laid out and shaped — see examples/bigresponse.rs\n",
        size_of::<NodeKind>(),
        size_of::<BoxStyle>(),
        5 * size_of::<Option<NodeId>>(),
        size_of::<crisol_display_list::Rect>()
            + size_of::<crisol_display_list::Size>()
            + size_of::<crisol_display_list::Point>(),
        100_000.0 * node as f64 / 1_048_576.0,
    );
    assert!(
        node <= BUDGET,
        "a Node is {node} bytes, over the {BUDGET} budget. \
         Either shrink it or move the budget deliberately."
    );
}

/// Where the bytes are, so the next person shrinking this knows where to look.
#[test]
fn the_largest_parts_of_a_node_are_the_ones_worth_shrinking() {
    // `BoxStyle` is the biggest single piece, and most of it is colour: a `Color` is four
    // f32s, and `border_color` holds four of them. Three quarters of `BoxStyle` is colour
    // that is identical on almost every node in a document.
    assert!(size_of::<BoxStyle>() >= size_of::<ElementData>() / 2);
    // An `Atom` is a pointer plus a cached hash. `ElementData` holds one outright, one
    // optional, and two vectors of them.
    assert_eq!(size_of::<Atom>(), 24);
    assert_eq!(size_of::<Attribute>(), 2 * size_of::<Atom>());
    // Generational, and still no bigger than a pointer — the niche in the generation makes
    // `Option<NodeId>` free, which matters five times over in every node.
    assert_eq!(size_of::<NodeId>(), 8);
    assert_eq!(size_of::<Option<NodeId>>(), size_of::<NodeId>());
}
