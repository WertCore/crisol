//! Layout snapshots: block layout, the box model, sizing and positioning.
//!
//! Part of M3's acceptance — *a layout snapshot suite of 40+ cases passes*. Split across
//! this file and `flex.rs` so a failure names the area it is in.

mod harness;

use crisol_display_list::Size;
use harness::{Doc, VIEWPORT, assert_layout};

// ---- the root ------------------------------------------------------------------------

#[test]
fn the_root_fills_the_viewport() {
    let mut doc = Doc::new();
    assert_layout(
        &doc.snapshot("", VIEWPORT),
        "
        body 0 0 200 100
        ",
    );
}

#[test]
fn an_empty_block_child_is_full_width_and_zero_height() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    assert_layout(
        &doc.snapshot("", VIEWPORT),
        "
        body 0 0 200 100
          div 0 0 200 0
        ",
    );
}

// ---- stacking ------------------------------------------------------------------------

#[test]
fn block_children_stack_vertically() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div.a");
    doc.add(doc.root, "div.b");
    assert_layout(
        &doc.snapshot("div { height: 20px }", VIEWPORT),
        "
        body 0 0 200 100
          div.a 0 0 200 20
          div.b 0 20 200 20
        ",
    );
}

#[test]
fn nested_blocks_offset_from_their_parent() {
    let mut doc = Doc::new();
    let outer = doc.add(doc.root, "div.outer");
    doc.add(outer, "div.inner");
    assert_layout(
        &doc.snapshot(
            ".outer { padding: 10px; height: 60px } .inner { height: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.outer 0 0 200 60
            div.inner 10 10 180 20
        ",
    );
}

// ---- the box model -------------------------------------------------------------------

#[test]
fn padding_insets_the_content() {
    let mut doc = Doc::new();
    let outer = doc.add(doc.root, "div.outer");
    doc.add(outer, "div.inner");
    assert_layout(
        &doc.snapshot(
            ".outer { padding-left: 8px; padding-top: 4px } .inner { height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.outer 0 0 200 14
            div.inner 8 4 192 10
        ",
    );
}

#[test]
fn borders_inset_the_content_too() {
    let mut doc = Doc::new();
    let outer = doc.add(doc.root, "div.outer");
    doc.add(outer, "div.inner");
    assert_layout(
        &doc.snapshot(
            ".outer { border: 5px solid black } .inner { height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.outer 0 0 200 20
            div.inner 5 5 190 10
        ",
    );
}

#[test]
fn margins_push_siblings_apart() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div.a");
    doc.add(doc.root, "div.b");
    assert_layout(
        &doc.snapshot("div { height: 10px } .b { margin-top: 6px }", VIEWPORT),
        "
        body 0 0 200 100
          div.a 0 0 200 10
          div.b 0 16 200 10
        ",
    );
}

#[test]
fn a_horizontal_margin_narrows_the_box() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    assert_layout(
        &doc.snapshot(
            "div { margin-left: 20px; margin-right: 30px; height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div 20 0 150 10
        ",
    );
}

/// Border-box sizing: `width` is the *outer* width, so padding eats into the content rather
/// than growing the box. This is the one place the engine departs from the CSS initial value
/// on purpose — see `CoreStyle::box_sizing` in `style_adapter`.
#[test]
fn width_is_the_border_box_not_the_content_box() {
    let mut doc = Doc::new();
    let outer = doc.add(doc.root, "div.outer");
    doc.add(outer, "div.inner");
    assert_layout(
        &doc.snapshot(
            ".outer { width: 100px; padding: 10px; height: 50px } .inner { height: 5px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.outer 0 0 100 50
            div.inner 10 10 80 5
        ",
    );
}

// ---- sizing --------------------------------------------------------------------------

#[test]
fn an_explicit_width_and_height_are_honoured() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    assert_layout(
        &doc.snapshot("div { width: 40px; height: 25px }", VIEWPORT),
        "
        body 0 0 200 100
          div 0 0 40 25
        ",
    );
}

#[test]
fn percentage_width_resolves_against_the_parent() {
    let mut doc = Doc::new();
    let outer = doc.add(doc.root, "div.outer");
    doc.add(outer, "div.inner");
    assert_layout(
        &doc.snapshot(
            ".outer { width: 100px; height: 40px } .inner { width: 50%; height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.outer 0 0 100 40
            div.inner 0 0 50 10
        ",
    );
}

#[test]
fn min_width_wins_over_a_smaller_width() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    assert_layout(
        &doc.snapshot(
            "div { width: 10px; min-width: 60px; height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div 0 0 60 10
        ",
    );
}

#[test]
fn max_width_wins_over_a_larger_width() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    assert_layout(
        &doc.snapshot(
            "div { width: 150px; max-width: 80px; height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div 0 0 80 10
        ",
    );
}

#[test]
fn min_height_and_max_height_apply_on_the_other_axis() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div.a");
    doc.add(doc.root, "div.b");
    assert_layout(
        &doc.snapshot(
            ".a { height: 4px; min-height: 20px } .b { height: 90px; max-height: 30px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.a 0 0 200 20
          div.b 0 20 200 30
        ",
    );
}

#[test]
fn a_parent_grows_to_fit_its_children() {
    let mut doc = Doc::new();
    let outer = doc.add(doc.root, "div.outer");
    doc.add(outer, "div.a");
    doc.add(outer, "div.b");
    assert_layout(
        &doc.snapshot("div div { height: 15px }", VIEWPORT),
        "
        body 0 0 200 100
          div.outer 0 0 200 30
            div.a 0 0 200 15
            div.b 0 15 200 15
        ",
    );
}

#[test]
fn em_lengths_resolve_against_the_font_size() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    assert_layout(
        &doc.snapshot("div { font-size: 10px; width: 3em; height: 2em }", VIEWPORT),
        "
        body 0 0 200 100
          div 0 0 30 20
        ",
    );
}

#[test]
fn rem_lengths_resolve_against_the_root_font_size() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    assert_layout(
        &doc.snapshot(
            "body { font-size: 40px } div { width: 2rem; height: 1rem }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div 0 0 32 16
        ",
    );
}

// ---- display -------------------------------------------------------------------------

#[test]
fn display_none_removes_the_box_and_its_subtree() {
    let mut doc = Doc::new();
    let hidden = doc.add(doc.root, "div.gone");
    doc.add(hidden, "div.child");
    doc.add(doc.root, "div.after");
    assert_layout(
        &doc.snapshot(
            ".gone { display: none; height: 50px } .after { height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.gone 0 0 0 0
            div.child 0 0 0 0
          div.after 0 0 200 10
        ",
    );
}

#[test]
fn visibility_hidden_keeps_the_box() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div.a");
    doc.add(doc.root, "div.b");
    assert_layout(
        &doc.snapshot("div { height: 10px } .a { visibility: hidden }", VIEWPORT),
        "
        body 0 0 200 100
          div.a 0 0 200 10
          div.b 0 10 200 10
        ",
    );
}

#[test]
fn an_unsupported_display_leaves_the_box_as_it_was() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    // `display: grid` is declined because no grid properties are in M3's subset; the
    // earlier `flex` therefore stands.
    assert_layout(
        &doc.snapshot(
            "div { display: flex; display: grid; height: 10px } div > * { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div 0 0 200 10
        ",
    );
}

// ---- position ------------------------------------------------------------------------

#[test]
fn absolute_positioning_takes_a_box_out_of_flow() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div.floating");
    doc.add(doc.root, "div.after");
    assert_layout(
        &doc.snapshot(
            ".floating { position: absolute; top: 5px; left: 7px; width: 10px; height: 10px }
             .after { height: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.floating 7 5 10 10
          div.after 0 0 200 20
        ",
    );
}

#[test]
fn an_absolute_box_positions_against_its_containing_block() {
    let mut doc = Doc::new();
    let outer = doc.add(doc.root, "div.outer");
    doc.add(outer, "div.pinned");
    assert_layout(
        &doc.snapshot(
            ".outer { position: relative; width: 100px; height: 60px; padding: 10px }
             .pinned { position: absolute; right: 0; bottom: 0; width: 20px; height: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.outer 0 0 100 60
            div.pinned 80 40 20 20
        ",
    );
}

#[test]
fn inset_shifts_a_relatively_positioned_box_without_moving_its_siblings() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div.a");
    doc.add(doc.root, "div.b");
    assert_layout(
        &doc.snapshot(
            "div { height: 10px } .a { position: relative; top: 4px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.a 0 4 200 10
          div.b 0 10 200 10
        ",
    );
}

// ---- text and leaves -----------------------------------------------------------------

#[test]
fn a_text_node_lays_out_as_an_empty_leaf_before_m4() {
    let mut doc = Doc::new();
    let p = doc.add(doc.root, "p");
    doc.text(p, "hello");
    assert_layout(
        &doc.snapshot("p { height: 12px }", VIEWPORT),
        "
        body 0 0 200 100
          p 0 0 200 12
            #text 0 0 200 0
        ",
    );
}

// ---- the viewport --------------------------------------------------------------------

#[test]
fn a_different_viewport_changes_percentage_sizing() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    assert_layout(
        &doc.snapshot("div { width: 50%; height: 10px }", Size::new(80.0, 40.0)),
        "
        body 0 0 80 40
          div 0 0 40 10
        ",
    );
}

#[test]
fn a_zero_sized_viewport_does_not_panic() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div");
    let snapshot = doc.snapshot("div { width: 50%; height: 10px }", Size::new(0.0, 0.0));
    assert!(snapshot.starts_with("body 0 0 0 0"), "{snapshot}");
}

#[test]
fn boxes_are_rounded_to_whole_pixels() {
    let mut doc = Doc::new();
    doc.add(doc.root, "div.a");
    doc.add(doc.root, "div.b");
    doc.add(doc.root, "div.c");
    // Three rows of a third each: without rounding these land on fractional offsets and
    // adjacent borders fall on different physical pixels.
    let snapshot = doc.snapshot("div { height: 33.333px }", VIEWPORT);
    for line in snapshot.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        for value in &fields[1..] {
            assert!(
                !value.contains('.'),
                "every coordinate should be whole after rounding: {line}"
            );
        }
    }
}
