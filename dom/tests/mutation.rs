//! The mutation API's contract: every write marks what a selector could notice, and a write
//! that changes nothing costs nothing.

use crisol_dom::{Dom, NodeType};
use crisol_tree::{DirtyFlags, Tree};

/// `div#root > span.a, span.b, span.c`
fn fixture() -> (Tree, [crisol_tree::NodeId; 4]) {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    let a = tree.create_element("span");
    let b = tree.create_element("span");
    let c = tree.create_element("span");
    for child in [a, b, c] {
        tree.append_child(root, child).unwrap();
    }
    tree.clear_all_dirty();
    (tree, [root, a, b, c])
}

#[test]
fn a_class_write_marks_the_node_and_its_following_siblings() {
    let (mut tree, [root, a, b, c]) = fixture();
    let mut dom = Dom::new(&mut tree);
    assert!(dom.add_class(a, "on"));

    // The node itself, and the siblings after it — a sibling combinator or `:nth-child`
    // can see the change from there. `root` is marked as an ancestor of a dirty node.
    assert!(tree.dirty(a).contains(DirtyFlags::STYLE), "the node itself");
    assert!(
        tree.dirty(b).contains(DirtyFlags::STYLE),
        "following sibling"
    );
    assert!(
        tree.dirty(c).contains(DirtyFlags::STYLE),
        "following sibling"
    );
    assert!(
        tree.dirty(root).contains(DirtyFlags::SUBTREE_STYLE),
        "the ancestor chain must lead the restyle walk to the change"
    );
}

#[test]
fn a_write_that_changes_nothing_marks_nothing() {
    let (mut tree, [_, a, b, _]) = fixture();
    let mut dom = Dom::new(&mut tree);
    dom.add_class(a, "on");
    tree.clear_all_dirty();

    let mut dom = Dom::new(&mut tree);
    assert!(!dom.add_class(a, "on"), "already present");
    assert!(!dom.set_attribute(a, "class", "on"), "same value");
    assert!(!dom.remove_class(a, "absent"), "was never there");
    assert_eq!(dom.stats().no_ops, 3);
    assert_eq!(dom.stats().attributes_set, 0);

    assert!(
        tree.dirty(a).is_empty() && tree.dirty(b).is_empty(),
        "a no-op write must not cost a restyle; a reactive system makes them constantly"
    );
}

#[test]
fn setting_the_same_text_is_not_a_relayout() {
    let mut tree = Tree::new();
    let text = tree.create_text("hello");
    tree.clear_all_dirty();

    let mut dom = Dom::new(&mut tree);
    assert!(!dom.set_text(text, "hello"));
    assert_eq!(dom.stats().no_ops, 1);
    assert!(tree.dirty(text).is_empty());

    let mut dom = Dom::new(&mut tree);
    assert!(dom.set_text(text, "goodbye"));
    assert_eq!(dom.stats().text_set, 1);
    assert!(tree.dirty(text).contains(DirtyFlags::LAYOUT));
}

#[test]
fn id_and_class_are_routed_to_the_fields_the_matcher_reads() {
    let (mut tree, [_, a, ..]) = fixture();
    let mut dom = Dom::new(&mut tree);

    dom.set_attribute(a, "id", "main");
    dom.set_attribute(a, "class", "x y");
    // Not the generic attribute bag: `#main` and `.x` look at `ElementData::id`/`classes`.
    assert_eq!(
        tree.element(a)
            .unwrap()
            .id
            .as_ref()
            .map(crisol_tree::Atom::as_str),
        Some("main")
    );
    assert_eq!(tree.element(a).unwrap().classes.len(), 2);
    assert!(tree.element(a).unwrap().attributes.is_empty());

    // ...and they still read back through the generic accessor.
    let dom = Dom::new(&mut tree);
    assert_eq!(dom.attribute(a, "id").as_deref(), Some("main"));
    assert_eq!(dom.attribute(a, "class").as_deref(), Some("x y"));
}

#[test]
fn a_mixed_case_attribute_is_findable_by_the_name_it_was_stored_under() {
    let (mut tree, [_, a, ..]) = fixture();
    let mut dom = Dom::new(&mut tree);

    dom.set_attribute(a, "dataFoo", "1");
    assert_eq!(dom.attribute(a, "datafoo").as_deref(), Some("1"));
    // The same write again is a no-op, which it would not be if the lookup missed.
    assert!(!dom.set_attribute(a, "dataFoo", "1"));
    assert!(dom.remove_attribute(a, "DATAFOO"));
    assert_eq!(dom.attribute(a, "datafoo"), None);
}

#[test]
fn toggle_class_reports_only_real_changes() {
    let (mut tree, [_, a, ..]) = fixture();
    let mut dom = Dom::new(&mut tree);

    assert!(dom.toggle_class(a, "on", true));
    assert!(!dom.toggle_class(a, "on", true), "already on");
    assert!(dom.toggle_class(a, "on", false));
    assert!(!dom.toggle_class(a, "on", false), "already off");
    assert_eq!(dom.stats().attributes_set, 2);
    assert_eq!(dom.stats().no_ops, 2);
}

#[test]
fn structure_changes_are_counted_and_queryable() {
    let (mut tree, [root, a, b, c]) = fixture();
    let mut dom = Dom::new(&mut tree);

    assert_eq!(dom.children(root), vec![a, b, c]);
    assert_eq!(dom.parent(a), Some(root));
    assert_eq!(dom.next_sibling(a), Some(b));
    assert_eq!(dom.node_type(a), Some(NodeType::Element));
    assert_eq!(dom.tag_name(a).unwrap(), "span");

    let inserted = dom.create_element("em");
    dom.insert_before(root, inserted, b).unwrap();
    assert_eq!(dom.children(root), vec![a, inserted, b, c]);

    dom.remove_child(root, c).unwrap();
    assert_eq!(dom.children(root), vec![a, inserted, b]);
    assert!(dom.is_alive(c), "removing detaches; it stays re-insertable");

    dom.append_child(root, c).unwrap();
    assert_eq!(dom.children(root), vec![a, inserted, b, c]);

    let stats = dom.stats();
    assert_eq!((stats.created, stats.inserted, stats.removed), (1, 2, 1));
}

#[test]
fn removing_a_subtree_frees_every_handle_in_it() {
    let (mut tree, [root, a, ..]) = fixture();
    let mut dom = Dom::new(&mut tree);
    let text = dom.create_text("gone");
    dom.append_child(a, text).unwrap();
    dom.reset_stats();

    assert_eq!(dom.remove_subtree(a), 2, "the span and its text");
    assert!(!dom.is_alive(a) && !dom.is_alive(text));
    assert_eq!(dom.children(root).len(), 2);
    assert_eq!(dom.stats().removed, 2);
}

#[test]
fn mutating_through_a_stale_handle_fails_rather_than_corrupting() {
    let (mut tree, [root, a, ..]) = fixture();
    let mut dom = Dom::new(&mut tree);
    dom.remove_subtree(a);

    // A generational id makes this an error rather than a write to whoever got the slot —
    // which is what a foreign caller holding a detached node will eventually do.
    assert!(dom.append_child(root, a).is_err());
    assert!(!dom.set_text(a, "x"));
    assert!(!dom.set_attribute(a, "class", "x"));
    assert_eq!(dom.node_type(a), None);
}
