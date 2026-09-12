//! The `cursor` property: parsed, inherited, and resolved.

use crisol_css::stylesheet::Stylesheet;
use crisol_style::{CursorIcon, StyleEngine, StyleMap};
use crisol_tree::{NodeId, Tree};

const CSS: &str = "
    .link   { cursor: pointer }
    .plain  { cursor: default }
    .gone   { cursor: none }
    .handle { cursor: col-resize }
";

/// `body > div.<class> > (text)`
fn styled(class: &str) -> (Tree, StyleMap, NodeId, NodeId) {
    let mut tree = Tree::new();
    let root = tree.create_element("body");
    tree.set_root(root).unwrap();
    let element = tree.create_element("div");
    tree.element_mut(element).unwrap().set_class(class);
    tree.append_child(root, element).unwrap();
    let text = tree.create_text("label");
    tree.append_child(element, text).unwrap();

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).unwrap());
    let styles = engine.restyle(&tree).0;
    (tree, styles, element, text)
}

fn cursor(styles: &StyleMap, node: NodeId) -> CursorIcon {
    styles.get(node).expect("styled").cursor
}

#[test]
fn the_initial_value_is_auto() {
    let (_, styles, element, _) = styled("unstyled");
    assert_eq!(cursor(&styles, element), CursorIcon::Auto);
}

#[test]
fn a_keyword_is_parsed_rather_than_approximated() {
    let (_, styles, element, _) = styled("link");
    assert_eq!(cursor(&styles, element), CursorIcon::Pointer);

    let (_, styles, element, _) = styled("handle");
    assert_eq!(
        cursor(&styles, element),
        CursorIcon::ColResize,
        "a keyword nobody mapped would silently be an arrow, which is why the conversion is \
         exhaustive rather than defaulted"
    );
}

#[test]
fn it_inherits_to_the_text_inside() {
    // Otherwise the pointer flickers back to an arrow as it crosses a button's own label,
    // which is the whole reason CSS makes this property inherited.
    let (_, styles, element, text) = styled("link");
    assert_eq!(cursor(&styles, element), CursorIcon::Pointer);
    assert_eq!(cursor(&styles, text), CursorIcon::Pointer);
}

#[test]
fn auto_is_an_i_beam_over_text_and_an_arrow_elsewhere() {
    assert_eq!(CursorIcon::Auto.resolve(true), CursorIcon::Text);
    assert_eq!(CursorIcon::Auto.resolve(false), CursorIcon::Default);
}

#[test]
fn an_explicit_keyword_survives_being_over_text() {
    // An author who wrote `cursor: default` meant the arrow, paragraph or not. Only `auto`
    // is context-dependent.
    let (_, styles, _, text) = styled("plain");
    assert_eq!(cursor(&styles, text).resolve(true), CursorIcon::Default);
    assert_eq!(CursorIcon::Pointer.resolve(true), CursorIcon::Pointer);
}

#[test]
fn none_asks_for_the_pointer_to_be_hidden() {
    let (_, styles, element, _) = styled("gone");
    assert_eq!(cursor(&styles, element), CursorIcon::None);
    assert!(cursor(&styles, element).is_hidden());
    assert!(!CursorIcon::Default.is_hidden());
    // Hiding survives resolution; it is not a shape to fall back from.
    assert!(CursorIcon::None.resolve(true).is_hidden());
}
