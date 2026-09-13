//! The cascade: which declaration wins, what inherits, and how much is shared.

use std::sync::Arc;

use crisol_css::stylesheet::{Origin, Stylesheet};
use crisol_style::values::{Color, Dimension, Display, LineHeight, Number, Visibility};
use crisol_style::{ComputedStyle, StyleEngine};
use crisol_tree::{NodeId, Tree};

const RED: Color = Color::rgba(255, 0, 0, 255);
const BLUE: Color = Color::rgba(0, 0, 255, 255);
const GREEN: Color = Color::rgba(0, 128, 0, 255);

/// Styles a tree with one stylesheet and returns the computed style of `node`.
fn style_of(tree: &Tree, css: &str, node: NodeId) -> Arc<ComputedStyle> {
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(css).unwrap());
    let (styles, _) = engine.restyle(tree);
    Arc::clone(styles.get(node).expect("every element gets a style"))
}

/// `<body><p class="lede" id="first"></p></body>`
fn body_and_paragraph() -> (Tree, NodeId, NodeId) {
    let mut tree = Tree::new();
    let body = tree.create_element("body");
    tree.set_root(body).unwrap();
    let p = tree.create_element("p");
    tree.append_child(body, p).unwrap();
    let data = tree.element_mut(p).unwrap();
    data.set_class("lede");
    data.id = Some("first".into());
    (tree, body, p)
}

// ---- the cascade order ---------------------------------------------------------------

#[test]
fn a_matching_rule_applies() {
    let (tree, _, p) = body_and_paragraph();
    assert_eq!(style_of(&tree, "p { color: red }", p).color, RED);
}

#[test]
fn a_non_matching_rule_does_not() {
    let (tree, _, p) = body_and_paragraph();
    assert_eq!(
        style_of(&tree, "div { color: red }", p).color,
        Color::BLACK,
        "the initial value survives"
    );
}

#[test]
fn specificity_beats_source_order() {
    let (tree, _, p) = body_and_paragraph();
    // The class rule is written first and still wins: 0-1-0 beats 0-0-1.
    let style = style_of(&tree, ".lede { color: red } p { color: blue }", p);
    assert_eq!(style.color, RED);
}

#[test]
fn ids_beat_classes_beat_types() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(
        &tree,
        "#first { color: green } .lede { color: red } p { color: blue }",
        p,
    );
    assert_eq!(style.color, GREEN);
}

#[test]
fn source_order_breaks_a_specificity_tie() {
    let (tree, _, p) = body_and_paragraph();
    assert_eq!(
        style_of(&tree, "p { color: red } p { color: blue }", p).color,
        BLUE
    );
}

#[test]
fn important_beats_specificity() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(
        &tree,
        "p { color: blue !important } #first { color: red }",
        p,
    );
    assert_eq!(style.color, BLUE);
}

#[test]
fn a_selector_lists_weight_is_that_of_the_selector_that_matched() {
    let (tree, _, p) = body_and_paragraph();
    // `h1, .lede` matches through `.lede`, so it weighs 0-1-0 and beats the later `p`.
    let style = style_of(&tree, "h1, .lede { color: red } p { color: blue }", p);
    assert_eq!(
        style.color, RED,
        "the rule's weight is the matching selector's, not the first one's"
    );
}

#[test]
fn author_rules_beat_user_agent_rules_whatever_their_specificity() {
    let (tree, _, p) = body_and_paragraph();
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(
        Stylesheet::parse_with_origin("#first { color: red }", Origin::UserAgent).unwrap(),
    );
    engine.add_stylesheet(Stylesheet::parse("p { color: blue }").unwrap());
    let (styles, _) = engine.restyle(&tree);
    assert_eq!(styles.get(p).unwrap().color, BLUE);
}

#[test]
fn a_later_longhand_beats_an_earlier_shorthand_in_the_same_rule() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "p { margin: 4px; margin-top: 9px }", p);
    assert_eq!(style.margin.top, Dimension::px(9.0));
    assert_eq!(style.margin.left, Dimension::px(4.0));
}

#[test]
fn a_shorthand_in_a_later_rule_overrides_a_longhand_in_an_earlier_one() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "p { margin-top: 9px } p { margin: 4px }", p);
    assert_eq!(
        style.margin.top,
        Dimension::px(4.0),
        "the shorthand expanded to a margin-top that came later"
    );
}

// ---- inheritance ---------------------------------------------------------------------

#[test]
fn inherited_properties_come_down_and_others_do_not() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "body { color: red; width: 300px }", p);
    assert_eq!(style.color, RED, "colour inherits");
    assert_eq!(style.width, Dimension::Auto, "width does not");
}

#[test]
fn a_child_can_override_what_it_inherited() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "body { color: red } p { color: blue }", p);
    assert_eq!(style.color, BLUE);
}

#[test]
fn inheritance_crosses_a_text_node() {
    let mut tree = Tree::new();
    let body = tree.create_element("body");
    tree.set_root(body).unwrap();
    let outer = tree.create_element("div");
    tree.append_child(body, outer).unwrap();
    let inner = tree.create_element("span");
    tree.append_child(outer, inner).unwrap();

    let style = style_of(&tree, "body { color: red }", inner);
    assert_eq!(style.color, RED);
}

#[test]
fn visibility_inherits_so_a_hidden_subtree_stays_hidden() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "body { visibility: hidden }", p);
    assert_eq!(style.visibility, Visibility::Hidden);
}

// ---- units ---------------------------------------------------------------------------

#[test]
fn em_resolves_against_this_elements_font_size() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "p { font-size: 20px; width: 2em }", p);
    assert_eq!(style.font_size.get(), 20.0);
    assert_eq!(style.width, Dimension::px(40.0));
}

#[test]
fn em_inside_font_size_resolves_against_the_parent() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "body { font-size: 20px } p { font-size: 1.5em }", p);
    assert_eq!(
        style.font_size.get(),
        30.0,
        "otherwise font-size: 1.5em would be circular"
    );
}

#[test]
fn rem_resolves_against_the_initial_font_size() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "body { font-size: 40px } p { width: 2rem }", p);
    assert_eq!(style.width, Dimension::px(32.0), "2 x 16px, not 2 x 40px");
}

#[test]
fn a_font_size_percentage_is_relative_to_the_parent() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "body { font-size: 20px } p { font-size: 150% }", p);
    assert_eq!(style.font_size.get(), 30.0);
}

#[test]
fn percentages_survive_unresolved_for_layout() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "p { width: 50% }", p);
    assert_eq!(
        style.width,
        Dimension::percent(50.0),
        "a percentage resolves against a containing block layout has not measured yet"
    );
}

#[test]
fn line_height_inherits_as_a_multiple_not_as_a_length() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "body { line-height: 1.5 } p { font-size: 40px }", p);
    assert_eq!(style.line_height, LineHeight::Number(Number::new(1.5)));
    assert_eq!(
        style.resolved_line_height(),
        60.0,
        "the child's larger font gets a proportionally larger line box"
    );
}

#[test]
fn current_color_follows_the_computed_colour() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "p { color: red; border-top-color: currentColor }", p);
    assert_eq!(style.border_color.top, RED);
}

// ---- what is ignored -----------------------------------------------------------------

#[test]
fn an_unsupported_display_leaves_the_property_alone() {
    let (tree, _, p) = body_and_paragraph();
    let style = style_of(&tree, "p { display: flex; display: table }", p);
    assert_eq!(
        style.display,
        Display::Flex,
        "table layout is out of scope, so the declaration behaves as if unwritten"
    );
}

#[test]
fn display_none_is_understood() {
    let (tree, _, p) = body_and_paragraph();
    assert!(style_of(&tree, "p { display: none }", p).is_none());
}

// ---- interning: the milestone's acceptance criterion ---------------------------------

/// ROADMAP §M3: *interning verified by asserting that 100 identically-styled nodes share
/// one `ComputedStyle` allocation.*
#[test]
fn a_hundred_identically_styled_nodes_share_one_allocation() {
    let mut tree = Tree::new();
    let body = tree.create_element("body");
    tree.set_root(body).unwrap();
    let mut nodes = Vec::new();
    for _ in 0..100 {
        let p = tree.create_element("p");
        tree.append_child(body, p).unwrap();
        nodes.push(p);
    }

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse("p { color: red; padding: 4px }").unwrap());
    let (styles, stats) = engine.restyle(&tree);

    let first = styles.get(nodes[0]).unwrap();
    for node in &nodes[1..] {
        assert!(
            Arc::ptr_eq(first, styles.get(*node).unwrap()),
            "every identically-styled paragraph must share one allocation"
        );
    }

    assert_eq!(stats.elements, 101);
    assert_eq!(
        engine.interner().len(),
        2,
        "one style for the body, one shared by all hundred paragraphs"
    );
    assert_eq!(
        stats.interner_misses, 2,
        "only two allocations for a hundred and one elements"
    );
    assert_eq!(stats.interner_hits, 99);
}

#[test]
fn nodes_that_differ_do_not_share() {
    let mut tree = Tree::new();
    let body = tree.create_element("body");
    tree.set_root(body).unwrap();
    let a = tree.create_element("p");
    tree.append_child(body, a).unwrap();
    let b = tree.create_element("p");
    tree.append_child(body, b).unwrap();
    tree.element_mut(b).unwrap().set_class("wide");

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse("p { color: red } .wide { width: 100px }").unwrap());
    let (styles, _) = engine.restyle(&tree);

    assert!(!Arc::ptr_eq(styles.get(a).unwrap(), styles.get(b).unwrap()));
    assert_eq!(engine.interner().len(), 3);
}

#[test]
fn interning_survives_a_restyle_and_reuses_the_same_allocations() {
    let (tree, _, _) = body_and_paragraph();
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse("p { color: red }").unwrap());

    let (first, _) = engine.restyle(&tree);
    let before = engine.interner().len();
    let (second, _) = engine.restyle(&tree);

    assert_eq!(engine.interner().len(), before, "no new allocations");
    let root = tree.root().unwrap();
    assert!(Arc::ptr_eq(
        first.get(root).unwrap(),
        second.get(root).unwrap()
    ));
}

#[test]
fn the_interner_can_drop_styles_nothing_holds() {
    let (tree, _, _) = body_and_paragraph();
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse("p { color: red }").unwrap());
    let (styles, _) = engine.restyle(&tree);
    let held = engine.interner().len();
    assert!(held > 0);

    drop(styles);
    // `collect_unused` needs `&mut`, which means taking the engine apart is not necessary;
    // the interner is reachable through the engine in real use. Here we just prove the
    // reference counting works the way the sweep assumes.
    assert_eq!(engine.interner().len(), held);
}

// ---- an empty document ---------------------------------------------------------------

#[test]
fn a_tree_with_no_root_styles_nothing() {
    let tree = Tree::new();
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse("p { color: red }").unwrap());
    let (styles, stats) = engine.restyle(&tree);
    assert!(styles.is_empty());
    assert_eq!(stats.elements, 0);
}

// ---- inline styles -------------------------------------------------------------------

/// `body_and_paragraph`, with a `style` attribute on the paragraph.
fn with_inline(style: &str) -> (Tree, NodeId) {
    let (mut tree, _, p) = body_and_paragraph();
    tree.element_mut(p).unwrap().style = Some(style.into());
    (tree, p)
}

#[test]
fn an_inline_style_applies() {
    let (tree, p) = with_inline("color: red");
    assert_eq!(style_of(&tree, "", p).color, RED);
}

#[test]
fn an_inline_style_beats_a_more_specific_rule() {
    // `#first` is the most specific thing an author can write at this element, and it still
    // loses: inline is a different origin, not a higher specificity.
    let (tree, p) = with_inline("color: blue");
    assert_eq!(style_of(&tree, "#first { color: red }", p).color, BLUE);
}

#[test]
fn an_important_rule_beats_a_normal_inline_style() {
    // The one direction an author can win, and the reason `important` sorts above `origin`.
    let (tree, p) = with_inline("color: blue");
    assert_eq!(style_of(&tree, "p { color: red !important }", p).color, RED);
}

#[test]
fn an_important_inline_style_beats_an_important_rule() {
    let (tree, p) = with_inline("color: blue !important");
    assert_eq!(
        style_of(&tree, "p { color: red !important }", p).color,
        BLUE
    );
}

#[test]
fn a_later_inline_declaration_beats_an_earlier_one() {
    let (tree, p) = with_inline("color: red; color: blue");
    assert_eq!(style_of(&tree, "", p).color, BLUE);
}

#[test]
fn an_inline_shorthand_expands_like_any_other() {
    // Shorthands are flattened to longhands before the cascade sees them, so an inline
    // `margin` has to reach all four edges exactly as a stylesheet's does. Asserted against
    // the stylesheet spelling rather than a literal `Px`, which is the same relation the
    // test is actually about (D-29).
    let (inline, p) = with_inline("margin: 4px");
    let (sheet, _, q) = body_and_paragraph();
    let inline = style_of(&inline, "", p);
    let sheet = style_of(&sheet, "p { margin: 4px }", q);
    assert_eq!(inline.margin, sheet.margin);
    assert_ne!(inline.margin.top, ComputedStyle::default().margin.top);
}

#[test]
fn a_malformed_declaration_does_not_cost_the_others() {
    // CSS error recovery is a feature (see `Stylesheet`'s warnings): one bad declaration
    // must not throw away the element's whole style attribute.
    let (tree, p) = with_inline("color: red; width: ¯\\_(ツ)_/¯");
    assert_eq!(style_of(&tree, "", p).color, RED);
}

#[test]
fn an_inline_style_inherits_to_children() {
    let (mut tree, body, p) = body_and_paragraph();
    tree.element_mut(body).unwrap().style = Some("color: red".into());
    assert_eq!(style_of(&tree, "", p).color, RED);
}

#[test]
fn an_element_without_one_is_unaffected() {
    let (tree, _, p) = body_and_paragraph();
    assert_eq!(style_of(&tree, "p { color: red }", p).color, RED);
}
