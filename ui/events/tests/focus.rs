//! Focus order and keyboard navigation.
//!
//! ROADMAP §M5's acceptance is *a form with three text inputs is fully keyboard-navigable*.
//! `tabbing_through_a_form_reaches_every_input_and_comes_back` is that, literally.

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::Size;
use crisol_events::{EventSystem, FocusDirection, Focusable, focusable, next_focus, tab_order};
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

/// The `name` attribute of each node, which is how these tests identify controls.
fn names(tree: &Tree, nodes: &[NodeId]) -> Vec<String> {
    nodes
        .iter()
        .map(|id| {
            tree.element(*id)
                .and_then(|data| data.attribute("name").map(|value| value.to_string()))
                .unwrap_or_else(|| tree.node(*id).kind.tag().unwrap_or("?").to_owned())
        })
        .collect()
}

fn by_name(tree: &Tree, name: &str) -> NodeId {
    let mut stack = vec![tree.root().unwrap()];
    while let Some(id) = stack.pop() {
        if tree
            .element(id)
            .and_then(|data| data.attribute("name"))
            .as_deref()
            == Some(name)
        {
            return id;
        }
        for child in tree.children(id) {
            stack.push(child);
        }
    }
    panic!("no control named {name}");
}

// ---- M5's acceptance ----------------------------------------------------------------------

/// *A form with three text inputs is fully keyboard-navigable.*
#[test]
fn tabbing_through_a_form_reaches_every_input_and_comes_back() {
    let tree = page(
        r#"<body><form>
             <input name=one><input name=two><input name=three>
           </form></body>"#,
        "input { width: 40px; height: 20px }",
    );

    let mut events = EventSystem::new();
    let mut visited = Vec::new();
    let mut focus = None;

    // Four presses: three inputs, then back to the first.
    for _ in 0..4 {
        focus = next_focus(&tree, focus, FocusDirection::Forward);
        events.set_focus(focus);
        visited.push(focus.expect("something focusable"));
    }

    assert_eq!(
        names(&tree, &visited),
        vec!["one", "two", "three", "one"],
        "Tab visits every input in document order and wraps"
    );

    // And back the other way.
    let mut backwards = Vec::new();
    let mut focus = Some(by_name(&tree, "one"));
    for _ in 0..3 {
        focus = next_focus(&tree, focus, FocusDirection::Backward);
        backwards.push(focus.unwrap());
    }
    assert_eq!(names(&tree, &backwards), vec!["three", "two", "one"]);
}

#[test]
fn focus_state_follows_the_keyboard() {
    let mut tree = page(
        r#"<body><form><input name=one><input name=two></form></body>"#,
        "input { width: 40px; height: 20px }",
    );
    let mut events = EventSystem::new();

    let first = next_focus(&tree, None, FocusDirection::Forward).unwrap();
    events.set_focus(Some(first));
    events.apply_state(&mut tree);
    assert!(
        tree.element(first)
            .unwrap()
            .state
            .contains(ElementState::FOCUS),
        ":focus should match the focused input"
    );

    let second = next_focus(&tree, Some(first), FocusDirection::Forward).unwrap();
    events.set_focus(Some(second));
    events.apply_state(&mut tree);
    assert!(
        !tree
            .element(first)
            .unwrap()
            .state
            .contains(ElementState::FOCUS)
    );
    assert!(
        tree.element(second)
            .unwrap()
            .state
            .contains(ElementState::FOCUS)
    );
}

// ---- what is focusable ----------------------------------------------------------------------

#[test]
fn native_controls_are_focusable_and_plain_boxes_are_not() {
    let tree = page(
        r#"<body>
             <div name=box></div>
             <button name=btn></button>
             <input name=inp>
             <select name=sel></select>
             <textarea name=area></textarea>
           </body>"#,
        "* { height: 10px }",
    );
    assert_eq!(focusable(&tree, by_name(&tree, "box")), Focusable::No);
    for control in ["btn", "inp", "sel", "area"] {
        assert_eq!(
            focusable(&tree, by_name(&tree, control)),
            Focusable::Yes,
            "{control} should be focusable"
        );
    }
}

#[test]
fn a_link_is_focusable_only_if_it_goes_somewhere() {
    let tree = page(
        r#"<body><a name=real href="/x"></a><a name=anchor></a></body>"#,
        "a { height: 10px }",
    );
    assert_eq!(focusable(&tree, by_name(&tree, "real")), Focusable::Yes);
    assert_eq!(focusable(&tree, by_name(&tree, "anchor")), Focusable::No);
}

#[test]
fn a_disabled_control_is_skipped() {
    let mut tree = page(
        r#"<body><input name=one><input name=two><input name=three></body>"#,
        "input { height: 10px }",
    );
    let two = by_name(&tree, "two");
    tree.element_mut(two).unwrap().state |= ElementState::DISABLED;

    assert_eq!(focusable(&tree, two), Focusable::No);
    assert_eq!(names(&tree, &tab_order(&tree)), vec!["one", "three"]);
}

#[test]
fn tabindex_zero_makes_anything_focusable() {
    let tree = page(
        r#"<body><div name=box tabindex="0"></div></body>"#,
        "div { height: 10px }",
    );
    assert_eq!(focusable(&tree, by_name(&tree, "box")), Focusable::Yes);
    assert_eq!(names(&tree, &tab_order(&tree)), vec!["box"]);
}

#[test]
fn a_negative_tabindex_is_reachable_by_pointer_but_not_by_tab() {
    let tree = page(
        r#"<body><input name=one><div name=panel tabindex="-1"></div></body>"#,
        "* { height: 10px }",
    );
    assert_eq!(
        focusable(&tree, by_name(&tree, "panel")),
        Focusable::Programmatic
    );
    assert_eq!(
        names(&tree, &tab_order(&tree)),
        vec!["one"],
        "Tab skips it; script and clicks can still focus it"
    );
}

/// A positive `tabindex` reorders the tab sequence independently of the document, which is
/// the single most reliable way to make an interface unusable with a keyboard. The engine
/// declines and treats it as an ordinary `0`.
#[test]
fn a_positive_tabindex_does_not_reorder_anything() {
    let tree = page(
        r#"<body>
             <input name=one>
             <div name=jumper tabindex="5"></div>
             <input name=two>
           </body>"#,
        "* { height: 10px }",
    );
    assert_eq!(
        names(&tree, &tab_order(&tree)),
        vec!["one", "jumper", "two"],
        "document order, not the author's numbering"
    );
}

// ---- what is skipped -------------------------------------------------------------------------

#[test]
fn a_hidden_control_is_not_in_the_tab_order() {
    // A focus ring on empty space is worse than no focus ring.
    let tree = page(
        r#"<body><input name=one><input name=hidden><input name=two></body>"#,
        "input { height: 10px } input[name=hidden] { visibility: hidden }",
    );
    assert_eq!(names(&tree, &tab_order(&tree)), vec!["one", "two"]);
}

#[test]
fn a_control_inside_a_display_none_subtree_is_not_reachable() {
    let tree = page(
        r#"<body><input name=one><div class=gone><input name=buried></div></body>"#,
        "input { height: 10px } .gone { display: none }",
    );
    assert_eq!(
        names(&tree, &tab_order(&tree)),
        vec!["one"],
        "a box that is not there has nothing to focus"
    );
}

#[test]
fn a_control_inside_a_hidden_subtree_is_not_reachable() {
    let tree = page(
        r#"<body><input name=one><div class=gone><input name=buried></div></body>"#,
        "input { height: 10px } .gone { visibility: hidden }",
    );
    assert_eq!(names(&tree, &tab_order(&tree)), vec!["one"]);
}

// ---- edges ------------------------------------------------------------------------------------

#[test]
fn a_document_with_nothing_focusable_has_nowhere_to_go() {
    let tree = page("<body><div></div></body>", "div { height: 10px }");
    assert!(tab_order(&tree).is_empty());
    assert_eq!(next_focus(&tree, None, FocusDirection::Forward), None);
}

#[test]
fn tabbing_from_something_outside_the_order_starts_at_the_beginning() {
    let tree = page(
        r#"<body><div name=box></div><input name=one></body>"#,
        "* { height: 10px }",
    );
    let box_node = by_name(&tree, "box");
    assert_eq!(
        next_focus(&tree, Some(box_node), FocusDirection::Forward),
        Some(by_name(&tree, "one")),
        "focus on an unfocusable node should not strand the keyboard"
    );
}

#[test]
fn shift_tab_from_nothing_starts_at_the_end() {
    let tree = page(
        r#"<body><input name=one><input name=two></body>"#,
        "input { height: 10px }",
    );
    assert_eq!(
        next_focus(&tree, None, FocusDirection::Backward),
        Some(by_name(&tree, "two"))
    );
}

#[test]
fn focus_order_is_document_order_not_visual_order() {
    // `position: absolute` can put a control anywhere on screen. Tab follows the document,
    // which is what a screen reader reads and what a keyboard user predicts.
    let tree = page(
        r#"<body><input name=first><input name=second></body>"#,
        "input { position: absolute; height: 10px; width: 10px }
         input[name=first] { left: 100px } input[name=second] { left: 0 }",
    );
    assert_eq!(
        names(&tree, &tab_order(&tree)),
        vec!["first", "second"],
        "the one on the right is still first, because it is first in the document"
    );
}
