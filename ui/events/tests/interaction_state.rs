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

#[test]
fn crossing_rows_pays_the_sibling_combinator_and_this_is_what_it_costs() {
    // Not a regression and not an accident: D-38 invalidates a state change conservatively —
    // the node, its descendants, and its *following siblings*, because `.row:hover + .row`
    // is expressible. Marking row 1 therefore marks rows 2..n, and marking row 2 marks
    // 3..n, so crossing one boundary in a long list touches most of the list.
    //
    // Before `apply_state` marked anything this cost nothing, because `:hover` never
    // applied at all. Making it correct is what makes the price visible, and the price is
    // worth writing down: a hover-heavy list pays it on every row boundary the pointer
    // crosses, which is the one input that happens continuously.
    //
    // The obvious way out is not to fix the invalidation but to narrow it: if no rule in
    // any loaded stylesheet uses `+` or `~`, the sibling half is pure waste and the engine
    // knows its own stylesheets. Left alone until something needs it — this test is the
    // baseline it would be measured against.
    let rows: String = (0..200)
        .map(|i| format!("<li class=row><span>{i}</span></li>"))
        .collect();
    let mut document = crisol_html::parse(&format!("<body><ul>{rows}</ul></body>"));
    let css = ".row { width: 100px; height: 20px } .row:hover { color: red }";
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(css).unwrap());
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

    let total = document.tree.len();
    assert!(
        stats.elements > total / 4,
        "measured at 400 of 604 when written; if this has dropped, the invalidation was \
         narrowed and the comment above needs revisiting rather than the number"
    );
}
