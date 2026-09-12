//! Layout snapshots: flex layout, custom nodes, and the paint projection.
//!
//! The other half of M3's 40+ case suite; block layout and the box model are in `block.rs`.

mod harness;

use crisol_display_list::{Color, Size};
use crisol_style::values::Overflow;
use crisol_tree::ColorBox;
use harness::{Doc, VIEWPORT, assert_layout};

/// A row of three fixed-width boxes inside a flex container.
fn row() -> Doc {
    let mut doc = Doc::new();
    let container = doc.add(doc.root, "div.row");
    for name in ["a", "b", "c"] {
        doc.add(container, &format!("div.{name}"));
    }
    doc
}

// ---- direction and order -------------------------------------------------------------

#[test]
fn flex_items_lay_out_along_the_row() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; height: 30px } .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 30
            div.a 0 0 20 30
            div.b 20 0 20 30
            div.c 40 0 20 30
        ",
    );
}

#[test]
fn row_reverse_starts_from_the_other_end() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; flex-direction: row-reverse; height: 30px }
             .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 30
            div.a 180 0 20 30
            div.b 160 0 20 30
            div.c 140 0 20 30
        ",
    );
}

#[test]
fn a_column_stacks_items_vertically() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; flex-direction: column; width: 40px }
             .row > div { height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 40 30
            div.a 0 0 40 10
            div.b 0 10 40 10
            div.c 0 20 40 10
        ",
    );
}

// ---- growing and shrinking -----------------------------------------------------------

#[test]
fn flex_grow_shares_out_the_free_space() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; height: 10px }
             .a { flex-grow: 1 } .b { flex-grow: 1 } .c { width: 100px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 0 0 50 10
            div.b 50 0 50 10
            div.c 100 0 100 10
        ",
    );
}

#[test]
fn flex_grow_shares_in_proportion() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; height: 10px }
             .a { flex-grow: 1 } .b { flex-grow: 3 } .c { display: none }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 0 0 50 10
            div.b 50 0 150 10
            div.c 0 0 0 0
        ",
    );
}

#[test]
fn flex_shrink_takes_the_overflow_back() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; height: 10px }
             .row > div { width: 100px } .c { flex-shrink: 0 }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 0 0 50 10
            div.b 50 0 50 10
            div.c 100 0 100 10
        ",
    );
}

#[test]
fn flex_basis_is_the_starting_size() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; height: 10px }
             .a { flex-basis: 60px } .b { flex-basis: 40px } .c { display: none }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 0 0 60 10
            div.b 60 0 40 10
            div.c 0 0 0 0
        ",
    );
}

// ---- distribution --------------------------------------------------------------------

#[test]
fn justify_content_center_packs_the_items_together() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; justify-content: center; height: 10px }
             .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 70 0 20 10
            div.b 90 0 20 10
            div.c 110 0 20 10
        ",
    );
}

#[test]
fn justify_content_flex_end_pushes_them_right() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; justify-content: flex-end; height: 10px }
             .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 140 0 20 10
            div.b 160 0 20 10
            div.c 180 0 20 10
        ",
    );
}

#[test]
fn space_between_puts_the_gap_in_the_middle() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; justify-content: space-between; height: 10px }
             .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 0 0 20 10
            div.b 90 0 20 10
            div.c 180 0 20 10
        ",
    );
}

#[test]
fn space_around_gives_each_item_equal_surrounding_space() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; justify-content: space-around; height: 10px }
             .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 23 0 20 10
            div.b 90 0 20 10
            div.c 157 0 20 10
        ",
    );
}

// ---- cross axis ----------------------------------------------------------------------

#[test]
fn align_items_stretch_is_the_default() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; height: 40px } .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 40
            div.a 0 0 20 40
            div.b 20 0 20 40
            div.c 40 0 20 40
        ",
    );
}

#[test]
fn align_items_center_centres_on_the_cross_axis() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; align-items: center; height: 40px }
             .row > div { width: 20px; height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 40
            div.a 0 15 20 10
            div.b 20 15 20 10
            div.c 40 15 20 10
        ",
    );
}

#[test]
fn align_self_overrides_the_containers_align_items() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; align-items: flex-start; height: 40px }
             .row > div { width: 20px; height: 10px }
             .b { align-self: flex-end }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 40
            div.a 0 0 20 10
            div.b 20 30 20 10
            div.c 40 0 20 10
        ",
    );
}

// ---- gaps and wrapping ---------------------------------------------------------------

#[test]
fn column_gap_separates_items_in_a_row() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; column-gap: 10px; height: 10px } .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 10
            div.a 0 0 20 10
            div.b 30 0 20 10
            div.c 60 0 20 10
        ",
    );
}

#[test]
fn the_gap_shorthand_sets_both_axes() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; flex-direction: column; gap: 6px; width: 30px }
             .row > div { height: 10px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 30 42
            div.a 0 0 30 10
            div.b 0 16 30 10
            div.c 0 32 30 10
        ",
    );
}

#[test]
fn flex_wrap_moves_overflow_onto_a_second_line() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; flex-wrap: wrap; width: 100px; height: 40px }
             .row > div { width: 60px; height: 20px; flex-shrink: 0 }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 100 40
            div.a 0 0 60 20
            div.b 0 20 60 20
            div.c 0 40 60 20
        ",
    );
}

// ---- nesting -------------------------------------------------------------------------

#[test]
fn a_flex_item_can_itself_be_a_flex_container() {
    let mut doc = Doc::new();
    let outer = doc.add(doc.root, "div.outer");
    let inner = doc.add(outer, "div.inner");
    doc.add(inner, "div.leaf");
    doc.add(inner, "div.leaf");
    assert_layout(
        &doc.snapshot(
            ".outer { display: flex; height: 20px }
             .inner { display: flex; flex-grow: 1 }
             .leaf { width: 30px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.outer 0 0 200 20
            div.inner 0 0 200 20
              div.leaf 0 0 30 20
              div.leaf 30 0 30 20
        ",
    );
}

#[test]
fn padding_on_a_flex_container_insets_its_items() {
    let mut doc = row();
    assert_layout(
        &doc.snapshot(
            ".row { display: flex; padding: 5px; height: 30px } .row > div { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 30
            div.a 5 5 20 20
            div.b 25 5 20 20
            div.c 45 5 20 20
        ",
    );
}

// ---- custom nodes --------------------------------------------------------------------

#[test]
fn a_custom_node_is_sized_by_its_own_measure() {
    let mut doc = Doc::new();
    let custom = doc
        .tree
        .create_custom("canvas", ColorBox::new(Size::new(37.0, 21.0), Color::BLACK));
    doc.tree.append_child(doc.root, custom).unwrap();

    assert_layout(
        &doc.snapshot("", VIEWPORT),
        "
        body 0 0 200 100
          canvas<ColorBox> 0 0 37 21
        ",
    );
}

#[test]
fn a_custom_node_participates_in_flex_layout() {
    let mut doc = Doc::new();
    let container = doc.add(doc.root, "div.row");
    doc.add(container, "div.a");
    let custom = doc
        .tree
        .create_custom("canvas", ColorBox::new(Size::new(40.0, 15.0), Color::BLACK));
    doc.tree.append_child(container, custom).unwrap();

    assert_layout(
        &doc.snapshot(
            ".row { display: flex; align-items: flex-start; height: 30px } .a { width: 20px }",
            VIEWPORT,
        ),
        "
        body 0 0 200 100
          div.row 0 0 200 30
            div.a 0 0 20 0
            canvas<ColorBox> 20 0 40 15
        ",
    );
}

#[test]
fn css_can_override_a_custom_nodes_measured_size() {
    // The engine has the final word on the box; `measure` is a request, not a decision.
    let mut doc = Doc::new();
    let custom = doc
        .tree
        .create_custom("canvas", ColorBox::new(Size::new(37.0, 21.0), Color::BLACK));
    doc.tree.append_child(doc.root, custom).unwrap();

    let mut engine = crisol_style::StyleEngine::new();
    engine.add_stylesheet(
        crisol_css::stylesheet::Stylesheet::parse("body > * { width: 90px }").unwrap(),
    );
    let (styles, _) = engine.restyle(&doc.tree);
    crisol_layout::layout(
        &mut doc.tree,
        &styles,
        &mut crisol_text::FontSystem::empty(),
        VIEWPORT,
    );

    assert_layout(
        &doc.describe(),
        "
        body 0 0 200 100
          canvas<ColorBox> 0 0 90 21
        ",
    );
}

#[test]
fn a_custom_nodes_children_are_invisible_to_layout() {
    // A custom node lays its own interior out. Letting taffy at its children would be the
    // opposite of what the escape hatch promises.
    let mut doc = Doc::new();
    let custom = doc
        .tree
        .create_custom("canvas", ColorBox::new(Size::new(30.0, 30.0), Color::BLACK));
    doc.tree.append_child(doc.root, custom).unwrap();
    let child = doc.tree.create_element("div");
    doc.tree.append_child(custom, child).unwrap();

    assert_layout(
        &doc.snapshot("div { height: 999px }", VIEWPORT),
        "
        body 0 0 200 100
          canvas<ColorBox> 0 0 30 30
            div 0 0 0 0
        ",
    );
}

// ---- the projection onto paint -------------------------------------------------------

#[test]
fn layout_writes_the_paint_style_onto_the_node() {
    let mut doc = Doc::new();
    let card = doc.add(doc.root, "div.card");
    doc.snapshot(
        ".card { width: 50px; height: 20px; background-color: #ff0000;
                 border: 2px solid #0000ff; border-radius: 4px; overflow: hidden }",
        VIEWPORT,
    );

    let style = doc.tree.node(card).style;
    assert_eq!(style.background, Color::from_rgba8(255, 0, 0, 255));
    assert_eq!(
        style.border_color,
        crisol_display_list::Edges4::all(Color::from_rgba8(0, 0, 255, 255))
    );
    assert_eq!(style.border_width.top, 2.0);
    assert_eq!(style.radii.top_left, 4.0);
    assert!(style.clips_children);
    assert!(style.visible);
}

#[test]
fn a_percentage_radius_resolves_against_the_laid_out_box() {
    let mut doc = Doc::new();
    let card = doc.add(doc.root, "div.card");
    doc.snapshot(
        ".card { width: 80px; height: 20px; border-radius: 25%; background-color: black }",
        VIEWPORT,
    );
    assert_eq!(
        doc.tree.node(card).style.radii.top_left,
        20.0,
        "25% of the 80px border box, which only exists after layout"
    );
}

#[test]
fn visibility_hidden_reaches_paint() {
    let mut doc = Doc::new();
    let card = doc.add(doc.root, "div.card");
    doc.snapshot(".card { visibility: hidden; height: 10px }", VIEWPORT);
    assert!(!doc.tree.node(card).style.visible);
}

#[test]
fn overflow_scroll_clips_like_hidden_does() {
    let mut doc = Doc::new();
    let card = doc.add(doc.root, "div.card");
    doc.snapshot(".card { overflow: scroll; height: 10px }", VIEWPORT);
    assert!(doc.tree.node(card).style.clips_children);
    assert_eq!(Overflow::Scroll.clips(), Overflow::Clip.clips());
}
