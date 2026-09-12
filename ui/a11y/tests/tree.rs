//! The tree a screen reader is given.
//!
//! ROADMAP §M5's acceptance is *VoiceOver/NVDA/Orca announce the tree correctly*, which
//! cannot be asserted from a test process — no CI runner has a screen reader attached. What
//! *can* be asserted is the thing those readers consume: the `TreeUpdate` handed to
//! accesskit. If the roles, the labels, the nesting and the focus are right, the announcement
//! is the platform's problem rather than the engine's.

use accesskit::{Role, TreeUpdate};
use crisol_a11y::{AccessibilityTree, from_access_id, to_access_id};
use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::Size;
use crisol_layout::layout;
use crisol_style::StyleEngine;
use crisol_text::FontSystem;
use crisol_tree::{ElementState, NodeId, Tree};

const VIEWPORT: Size = Size {
    width: 200.0,
    height: 100.0,
};

fn page(html: &str, css: &str) -> Tree {
    let mut document = crisol_html::parse(html);
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(css).unwrap());
    let (styles, _) = engine.restyle(&document.tree);
    layout(
        &mut document.tree,
        &styles,
        &mut FontSystem::empty(),
        VIEWPORT,
    );
    document.tree
}

/// Every role in the update, in the order the nodes were emitted.
fn roles(update: &TreeUpdate) -> Vec<Role> {
    update.nodes.iter().map(|(_, node)| node.role()).collect()
}

fn find(tree: &Tree, tag: &str) -> NodeId {
    let mut stack = vec![tree.root().unwrap()];
    while let Some(id) = stack.pop() {
        if tree.node(id).kind.tag() == Some(tag) {
            return id;
        }
        for child in tree.children(id) {
            stack.push(child);
        }
    }
    panic!("no <{tag}>");
}

// ---- what is announced --------------------------------------------------------------------

#[test]
fn controls_get_the_roles_a_screen_reader_expects() {
    let tree = page(
        r#"<body>
             <h1>Title</h1>
             <p>Body</p>
             <button>Go</button>
             <a href="/x">Link</a>
             <input type=checkbox>
           </body>"#,
        "* { height: 10px }",
    );
    let update = AccessibilityTree::new().update(&tree, None);
    let found = roles(&update);

    for expected in [
        Role::Heading,
        Role::Paragraph,
        Role::Button,
        Role::Link,
        Role::CheckBox,
        Role::Window,
    ] {
        assert!(
            found.contains(&expected),
            "expected a {expected:?} in {found:?}"
        );
    }
}

#[test]
fn an_input_type_decides_between_text_checkbox_and_radio() {
    let tree = page(
        r#"<body><input><input type=checkbox><input type=radio></body>"#,
        "input { height: 10px }",
    );
    let update = AccessibilityTree::new().update(&tree, None);
    let found = roles(&update);
    assert!(found.contains(&Role::TextInput));
    assert!(found.contains(&Role::CheckBox));
    assert!(found.contains(&Role::RadioButton));
}

#[test]
fn text_is_announced_as_its_content() {
    let tree = page("<body><p>Hello there</p></body>", "p { height: 10px }");
    let update = AccessibilityTree::new().update(&tree, None);
    let text = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextRun)
        .expect("a text run");
    assert_eq!(text.1.value(), Some("Hello there"));
}

#[test]
fn an_aria_label_overrides_what_would_be_read() {
    let tree = page(
        r#"<body><button aria-label="Close dialog">x</button></body>"#,
        "button { height: 10px }",
    );
    let update = AccessibilityTree::new().update(&tree, None);
    let button = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Button)
        .expect("a button");
    assert_eq!(button.1.label(), Some("Close dialog"));
}

#[test]
fn state_reaches_the_screen_reader() {
    let mut tree = page(
        r#"<body><button>Go</button><input type=checkbox></body>"#,
        "* { height: 10px }",
    );
    let button = find(&tree, "button");
    tree.element_mut(button).unwrap().state |= ElementState::DISABLED;
    let checkbox = find(&tree, "input");
    tree.element_mut(checkbox).unwrap().state |= ElementState::CHECKED;

    let update = AccessibilityTree::new().update(&tree, None);
    let disabled = update
        .nodes
        .iter()
        .find(|(id, _)| *id == to_access_id(button))
        .unwrap();
    assert!(
        disabled.1.is_disabled(),
        "a disabled button must announce as such"
    );

    let checked = update
        .nodes
        .iter()
        .find(|(id, _)| *id == to_access_id(checkbox))
        .unwrap();
    assert_eq!(checked.1.toggled(), Some(accesskit::Toggled::True));
}

// ---- what is not announced ------------------------------------------------------------------

/// A tree that announced every `<div>` would drown the content in structure.
#[test]
fn layout_boxes_do_not_appear_but_their_contents_do() {
    let tree = page(
        "<body><div><div><button>Go</button></div></div></body>",
        "* { height: 10px }",
    );
    let update = AccessibilityTree::new().update(&tree, None);
    let found = roles(&update);
    assert!(found.contains(&Role::Button), "the button survives");
    assert!(
        !found.contains(&Role::GenericContainer),
        "the two wrapper divs do not: {found:?}"
    );
}

#[test]
fn a_skipped_wrapper_does_not_orphan_its_children() {
    // `<body>` is a layout box and is skipped, so the button has to become a child of
    // `<html>` rather than of nothing.
    let tree = page(
        "<body><div><button>Go</button></div></body>",
        "* { height: 10px }",
    );
    let update = AccessibilityTree::new().update(&tree, None);

    let root = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Window)
        .expect("a root");
    let button = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Button)
        .expect("a button");
    assert!(
        root.1.children().contains(&button.0),
        "the button should have floated up to the root"
    );
}

#[test]
fn a_hidden_subtree_is_not_announced() {
    // A screen reader reading out a hidden dialog is the accessibility equivalent of it
    // being drawn on screen.
    let tree = page(
        "<body><button>Visible</button><div class=gone><button>Hidden</button></div></body>",
        "* { height: 10px } .gone { visibility: hidden }",
    );
    let update = AccessibilityTree::new().update(&tree, None);
    assert_eq!(
        roles(&update)
            .iter()
            .filter(|role| **role == Role::Button)
            .count(),
        1
    );
}

#[test]
fn a_display_none_subtree_is_not_announced() {
    let tree = page(
        "<body><button>Visible</button><div class=gone><button>Gone</button></div></body>",
        "* { height: 10px } .gone { display: none }",
    );
    let update = AccessibilityTree::new().update(&tree, None);
    assert_eq!(
        roles(&update)
            .iter()
            .filter(|role| **role == Role::Button)
            .count(),
        1
    );
}

#[test]
fn whitespace_only_text_is_not_announced() {
    let tree = page(
        "<body><button>Go</button>   <button>Stop</button></body>",
        "* { height: 10px }",
    );
    let update = AccessibilityTree::new().update(&tree, None);
    assert_eq!(
        roles(&update)
            .iter()
            .filter(|role| **role == Role::TextRun)
            .count(),
        2,
        "the two labels, and not the whitespace between them"
    );
}

// ---- focus and geometry ----------------------------------------------------------------------

#[test]
fn the_focused_node_is_reported() {
    let tree = page("<body><input></body>", "input { height: 10px }");
    let input = find(&tree, "input");
    let update = AccessibilityTree::new().update(&tree, Some(input));
    assert_eq!(update.focus, to_access_id(input));
}

#[test]
fn nothing_focused_reports_the_root() {
    // accesskit requires a focus. The root means "the window has focus but nothing in it
    // does", which is true before the user has tabbed anywhere.
    let tree = page("<body><input></body>", "input { height: 10px }");
    let update = AccessibilityTree::new().update(&tree, None);
    assert_eq!(update.focus, to_access_id(tree.root().unwrap()));
}

#[test]
fn focus_on_a_removed_node_falls_back_to_the_root() {
    let mut tree = page("<body><input></body>", "input { height: 10px }");
    let input = find(&tree, "input");
    tree.remove_subtree(input);
    let update = AccessibilityTree::new().update(&tree, Some(input));
    assert_eq!(update.focus, to_access_id(tree.root().unwrap()));
}

#[test]
fn nodes_carry_their_screen_position() {
    let tree = page(
        "<body><button class=go></button></body>",
        ".go { position: absolute; left: 20px; top: 30px; width: 40px; height: 10px }",
    );
    let update = AccessibilityTree::new().update(&tree, None);
    let button = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Button)
        .unwrap();
    let bounds = button
        .1
        .bounds()
        .expect("a button has a position on screen");
    assert!((bounds.x0 - 20.0).abs() < 0.01, "{bounds:?}");
    assert!((bounds.y0 - 30.0).abs() < 0.01, "{bounds:?}");
    assert!((bounds.x1 - 60.0).abs() < 0.01, "{bounds:?}");
}

// ---- handles --------------------------------------------------------------------------------

#[test]
fn handles_round_trip_so_an_action_can_be_routed_back() {
    // A screen reader sends "activate this node" back by id. If the mapping is not
    // reversible the action goes nowhere.
    let tree = page("<body><button></button></body>", "button { height: 10px }");
    let button = find(&tree, "button");
    assert_eq!(from_access_id(to_access_id(button)), Some(button));
}

#[test]
fn an_empty_document_still_produces_a_valid_update() {
    let tree = Tree::new();
    let update = AccessibilityTree::new().update(&tree, None);
    assert!(update.nodes.is_empty());
    assert!(
        update.tree.is_some(),
        "accesskit needs tree info even for nothing"
    );
}

// ---- reading order ----------------------------------------------------------------------------

#[test]
fn children_are_emitted_before_their_parent() {
    // accesskit expects a parent's child list to refer to nodes that already exist in the
    // update.
    let tree = page("<body><p>Hello</p></body>", "p { height: 10px }");
    let update = AccessibilityTree::new().update(&tree, None);

    let mut seen = Vec::new();
    for (id, node) in &update.nodes {
        for child in node.children() {
            assert!(
                seen.contains(child),
                "node {id:?} refers to a child that has not been emitted yet"
            );
        }
        seen.push(*id);
    }
}
