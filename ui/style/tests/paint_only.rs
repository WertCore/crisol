//! Whether a style change costs a relayout or only a repaint.
//!
//! The restyle pass marks `LAYOUT` on nodes whose computed style genuinely changed (D-45).
//! That is right as far as it goes, and for a while it went too far: *any* difference marked
//! `LAYOUT`, including one that only altered a colour. Toggling `.done` on a todo row, where
//! the rule is `li.done span.label { color: … }`, relaid out the row rather than repainting
//! it — issue #20.
//!
//! These tests pin both directions, because only one of them is the interesting one. That a
//! colour change repaints is the fix; that a width change still relays out is the thing the
//! fix must not have broken, and a change that simply stopped marking `LAYOUT` would pass the
//! first test and fail the second.

use crisol_css::stylesheet::Stylesheet;
use crisol_style::{StyleEngine, StyleMap};
use crisol_tree::{DirtyFlags, NodeId, Tree};

/// `<body><p class="before"></p></body>`, styled once, with the flags that first pass set
/// cleared again — so what a later pass marks is only ever about that pass.
fn styled(css: &str) -> (StyleEngine, Tree, NodeId, StyleMap) {
    let mut tree = Tree::new();
    let body = tree.create_element("body");
    tree.set_root(body).unwrap();
    let p = tree.create_element("p");
    tree.append_child(body, p).unwrap();
    tree.element_mut(p).unwrap().set_class("before");

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(css).unwrap());
    let (styles, _) = engine.restyle(&tree);
    clear(&mut tree, body);
    clear(&mut tree, p);
    (engine, tree, p, styles)
}

fn clear(tree: &mut Tree, id: NodeId) {
    tree.clear_dirty(id, DirtyFlags::all());
}

/// Swaps the class and restyles, returning what the pass marked on `p`.
fn reclass(
    engine: &mut StyleEngine,
    tree: &mut Tree,
    p: NodeId,
    class: &str,
    previous: &StyleMap,
) -> (DirtyFlags, StyleMap) {
    tree.element_mut(p).unwrap().set_class(class);
    // By hand: `Tree` cannot enforce invalidation, which is why all mutation goes through
    // `crisol-dom` in real use (D-42). The test is below that layer on purpose — it is the
    // style pass being examined, not the DOM's marking.
    tree.mark_dirty(p, DirtyFlags::STYLE);
    let (styles, _) = engine.restyle_incremental(tree, previous);
    (tree.dirty(p), styles)
}

#[test]
fn a_colour_only_change_repaints_rather_than_relaying_out() {
    let (mut engine, mut tree, p, styles) = styled(
        "p { width: 100px; color: red }
         p.after { color: blue }",
    );
    let (marked, _) = reclass(&mut engine, &mut tree, p, "after", &styles);

    assert!(
        marked.contains(DirtyFlags::PAINT),
        "a colour change has to repaint: {marked:?}"
    );
    assert!(
        !marked.contains(DirtyFlags::LAYOUT),
        "and it must not relay out — the box did not move or change size: {marked:?}"
    );
}

#[test]
fn a_size_change_still_relays_out() {
    let (mut engine, mut tree, p, styles) = styled(
        "p { width: 100px; color: red }
         p.after { width: 200px }",
    );
    let (marked, _) = reclass(&mut engine, &mut tree, p, "after", &styles);

    assert!(
        marked.contains(DirtyFlags::LAYOUT),
        "a width change is a layout change: {marked:?}"
    );
}

#[test]
fn a_font_change_relays_out_because_text_measures_differently() {
    let (mut engine, mut tree, p, styles) = styled(
        "p { font-size: 13px }
         p.after { font-size: 26px }",
    );
    let (marked, _) = reclass(&mut engine, &mut tree, p, "after", &styles);

    assert!(
        marked.contains(DirtyFlags::LAYOUT),
        "font size decides how wide the text is, so it is not paint-only: {marked:?}"
    );
}

#[test]
fn overflow_relays_out_because_it_decides_whether_the_box_scrolls() {
    let (mut engine, mut tree, p, styles) = styled(
        "p { overflow: visible }
         p.after { overflow: scroll }",
    );
    let (marked, _) = reclass(&mut engine, &mut tree, p, "after", &styles);

    assert!(
        marked.contains(DirtyFlags::LAYOUT),
        "becoming a scroll container is a layout question, not a paint one: {marked:?}"
    );
}

#[test]
fn a_style_that_did_not_change_marks_nothing() {
    let (mut engine, mut tree, p, styles) = styled(
        "p { width: 100px; color: red }
         p.after { color: red }",
    );
    // `after` sets the colour it already had, so the interned style is the same allocation
    // and the pass should conclude nothing happened.
    let (marked, _) = reclass(&mut engine, &mut tree, p, "after", &styles);

    assert!(
        !marked.contains(DirtyFlags::LAYOUT) && !marked.contains(DirtyFlags::PAINT),
        "re-deriving the same style is not a change: {marked:?}"
    );
}
