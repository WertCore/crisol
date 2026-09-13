//! The bits `apply_state` writes, and whether the cascade ever sees them.
//!
//! `apply_state` is the seam between input and style: it writes `:hover`, `:focus` and
//! `:active` onto elements, and the cascade matches against them. Both halves can be correct
//! while the seam is not — the element's state says hovered, the computed style says
//! otherwise, and nothing reports an error. This asserts across the seam rather than on
//! either side of it.

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::{Point, Size};
use crisol_events::{EventSystem, pointer_at};
use crisol_layout::layout;
use crisol_style::values::Color;
use crisol_style::{StyleEngine, StyleMap};
use crisol_text::FontSystem;

const VIEWPORT: Size = Size {
    width: 200.0,
    height: 100.0,
};
const RED: Color = Color::rgba(255, 0, 0, 255);

#[test]
fn a_hover_reaches_the_cascade_through_an_incremental_restyle() {
    let mut document = crisol_html::parse("<body><div class=box></div></body>");
    let css = ".box { width: 50px; height: 50px; color: blue } .box:hover { color: red }";
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(css).unwrap());

    // First pass, nothing hovered.
    let (mut styles, _) = engine.restyle_incremental(&mut document.tree, &StyleMap::default());
    layout(
        &mut document.tree,
        &styles,
        &mut FontSystem::empty(),
        VIEWPORT,
    );

    let box_node = {
        let mut stack = vec![document.tree.root().unwrap()];
        loop {
            let id = stack.pop().expect("no .box");
            if document
                .tree
                .element(id)
                .is_some_and(|d| d.has_class("box", true))
            {
                break id;
            }
            for c in document.tree.children(id) {
                stack.push(c);
            }
        }
    };
    assert_ne!(styles.get(box_node).unwrap().color, RED, "not hovered yet");

    // Hover it the way an application would.
    let mut events = EventSystem::new();
    events.pointer_moved(&document.tree, pointer_at(Point::new(5.0, 5.0)), &());
    let changed = events.apply_state(&mut document.tree);
    assert!(changed > 0, "apply_state reported a change");

    // Then restyle incrementally, exactly as the doc comment says to.
    styles = engine.restyle_incremental(&mut document.tree, &styles).0;
    assert_eq!(
        styles.get(box_node).unwrap().color,
        RED,
        "the :hover rule should have applied after apply_state marked it"
    );
}

#[test]
fn hovering_does_not_restyle_the_whole_document() {
    // The fix that makes `:hover` reach the cascade marks the changed nodes dirty, and
    // over-marking would quietly undo M6: `restyle_incremental` only earns its name if a
    // pointer move costs a few elements rather than the document.
    //
    // Moving *within* an already-hovered chain is the case that has to stay cheap. The
    // ancestors keep their bit, so they are not re-marked; only the nodes that actually
    // enter or leave are.
    let mut document = crisol_html::parse(
        "<body><ul>\
           <li class=row><span>one</span></li>\
           <li class=row><span>two</span></li>\
           <li class=row><span>three</span></li>\
           <li class=row><span>four</span></li>\
         </ul></body>",
    );
    let css = ".row { width: 100px; height: 20px } .row:hover { color: red }";
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(css).unwrap());
    let (mut styles, _) = engine.restyle_incremental(&mut document.tree, &StyleMap::default());
    layout(
        &mut document.tree,
        &styles,
        &mut FontSystem::empty(),
        VIEWPORT,
    );

    let mut events = EventSystem::new();
    // Into the first row, then a nudge within it.
    events.pointer_moved(&document.tree, pointer_at(Point::new(5.0, 5.0)), &());
    events.apply_state(&mut document.tree);
    styles = engine.restyle_incremental(&mut document.tree, &styles).0;

    events.pointer_moved(&document.tree, pointer_at(Point::new(9.0, 9.0)), &());
    let changed = events.apply_state(&mut document.tree);
    let (_, stats) = engine.restyle_incremental(&mut document.tree, &styles);

    assert_eq!(
        changed, 0,
        "the chain did not change, so nothing was re-marked"
    );
    assert_eq!(
        stats.elements, 0,
        "and so nothing was recomputed: a nudge inside one row is free"
    );
}

/// A 200-row list, and the two rows the pointer crosses between.
fn rows_and_engine(css: &str) -> (crisol_html::Document, StyleEngine) {
    let rows: String = (0..200)
        .map(|i| format!("<li class=row><span>{i}</span></li>"))
        .collect();
    let document = crisol_html::parse(&format!("<body><ul>{rows}</ul></body>"));
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(css).unwrap());
    (document, engine)
}

#[test]
fn crossing_rows_restyles_only_the_rows_that_changed() {
    // D-38 invalidates a state change to the node, its descendants and its *following
    // siblings*, because `.row:hover + .row` is expressible. When no rule in the sheet
    // actually expresses it, that last part is provably dead work — and it is the expensive
    // part, because each marked row marks the rest of the list.
    //
    // This sheet has no `+` or `~`, so the engine tells the tree so and the walk stops at
    // the two rows that really changed. It was 400 of 604 before the narrowing.
    let (mut document, mut engine) =
        rows_and_engine(".row { width: 100px; height: 20px } .row:hover { color: red }");
    let (mut styles, _) = engine.restyle_incremental(&mut document.tree, &StyleMap::default());
    layout(
        &mut document.tree,
        &styles,
        &mut FontSystem::empty(),
        Size {
            width: 200.0,
            height: 4200.0,
        },
    );

    let mut events = EventSystem::new();
    events.pointer_moved(&document.tree, pointer_at(Point::new(5.0, 10.0)), &());
    events.apply_state(&mut document.tree);
    styles = engine.restyle_incremental(&mut document.tree, &styles).0;

    events.pointer_moved(&document.tree, pointer_at(Point::new(5.0, 30.0)), &());
    events.apply_state(&mut document.tree);
    let (_, stats) = engine.restyle_incremental(&mut document.tree, &styles);

    assert_eq!(
        stats.elements, 4,
        "the row left and the row entered, each with its span — and nothing else"
    );
}

#[test]
fn a_sheet_that_uses_a_sibling_combinator_still_gets_the_wide_walk() {
    // The guard on the optimisation: narrowing is sound only while nothing can match across
    // a sibling boundary, so a sheet that can must go back to paying for it. Asserted by the
    // rule *working* rather than by counting elements — cost is the means, the rule applying
    // is what would actually be broken.
    //
    // The isolation matters and the first version of this test did not have it. Entering the
    // document hovers the whole chain up to `html`, and marking `html` dirties everything —
    // so a sibling rule appears to work even with the sibling walk disabled entirely. The
    // move below is the *second* one: `html` and `body` keep their bit and are not re-marked,
    // so row 2 can only be reached by walking siblings from row 1.
    let (mut document, mut engine) =
        rows_and_engine(".row { width: 100px; height: 20px } .row:hover + .row { color: red }");
    let (mut styles, _) = engine.restyle_incremental(&mut document.tree, &StyleMap::default());
    layout(
        &mut document.tree,
        &styles,
        &mut FontSystem::empty(),
        Size {
            width: 200.0,
            height: 4200.0,
        },
    );

    let rows: Vec<_> = {
        let list = document
            .tree
            .children(document.tree.root().unwrap())
            .flat_map(|body| document.tree.children(body))
            .next()
            .expect("the ul");
        document.tree.children(list).collect()
    };

    let mut events = EventSystem::new();
    // First move: settles the ancestors, so they are not re-marked by the next one.
    events.pointer_moved(&document.tree, pointer_at(Point::new(5.0, 10.0)), &());
    events.apply_state(&mut document.tree);
    styles = engine.restyle_incremental(&mut document.tree, &styles).0;
    assert_eq!(styles.get(rows[1]).unwrap().color, RED, "row 0 hovered");

    // Second move, row 0 -> row 1. Only those two change state.
    events.pointer_moved(&document.tree, pointer_at(Point::new(5.0, 30.0)), &());
    events.apply_state(&mut document.tree);
    styles = engine.restyle_incremental(&mut document.tree, &styles).0;

    assert_eq!(
        styles.get(rows[2]).unwrap().color,
        RED,
        "row 1 is hovered, so `+` must colour row 2 — reachable only by walking siblings"
    );
    assert_ne!(
        styles.get(rows[1]).unwrap().color,
        RED,
        "and row 1 must have stopped being the one after the hovered row"
    );
}
