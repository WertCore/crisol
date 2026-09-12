//! M2: the node tree's structural guarantees.

use crisol_display_list::{Color, Point, Rect, Size};
use crisol_tree::{
    BoxStyle, ColorBox, CustomNode, DirtyFlags, MeasureConstraints, Tree, TreeError,
};

/// Builds `root > a > (b, c)` and returns the ids in that order.
fn three_levels() -> (Tree, [crisol_tree::NodeId; 4]) {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    let a = tree.create_element("div");
    let b = tree.create_element("div");
    let c = tree.create_element("div");
    tree.append_child(root, a).unwrap();
    tree.append_child(a, b).unwrap();
    tree.append_child(a, c).unwrap();
    (tree, [root, a, b, c])
}

#[test]
fn children_iterate_in_insertion_order() {
    let (tree, [root, a, b, c]) = three_levels();
    assert_eq!(tree.children(root).collect::<Vec<_>>(), vec![a]);
    assert_eq!(tree.children(a).collect::<Vec<_>>(), vec![b, c]);
    assert_eq!(tree.parent(c), Some(a));
    assert_eq!(tree.parent(root), None);
}

#[test]
fn insert_before_splices_into_the_middle() {
    let (mut tree, [_, a, b, c]) = three_levels();
    let d = tree.create_element("span");
    tree.insert_before(a, d, c).unwrap();
    assert_eq!(tree.children(a).collect::<Vec<_>>(), vec![b, d, c]);

    // And at the front, which is the case that has to update `first_child`.
    let e = tree.create_element("span");
    tree.insert_before(a, e, b).unwrap();
    assert_eq!(tree.children(a).collect::<Vec<_>>(), vec![e, b, d, c]);
    assert_eq!(tree.first_child(a), Some(e));
}

#[test]
fn appending_a_node_that_already_has_a_parent_moves_it() {
    let (mut tree, [root, a, b, c]) = three_levels();
    tree.append_child(root, b).unwrap();
    assert_eq!(tree.children(a).collect::<Vec<_>>(), vec![c]);
    assert_eq!(tree.children(root).collect::<Vec<_>>(), vec![a, b]);
    assert_eq!(tree.parent(b), Some(root));
}

#[test]
fn inserting_before_a_sibling_that_is_the_node_itself_is_a_no_op() {
    let (mut tree, [_, a, b, c]) = three_levels();
    tree.insert_before(a, c, c).unwrap();
    assert_eq!(tree.children(a).collect::<Vec<_>>(), vec![b, c]);
}

#[test]
fn a_node_cannot_be_inserted_into_its_own_descendant() {
    let (mut tree, [root, a, b, _]) = three_levels();
    assert_eq!(
        tree.append_child(b, root),
        Err(TreeError::WouldCycle {
            parent: b,
            child: root
        })
    );
    assert_eq!(tree.append_child(a, a), Err(TreeError::SelfParent(a)));
    // The failed insertion changed nothing.
    assert_eq!(tree.parent(root), None);
    assert_eq!(tree.children(a).count(), 2);
}

#[test]
fn insert_before_rejects_a_reference_that_is_not_a_child() {
    let (mut tree, [root, a, b, _]) = three_levels();
    let stranger = tree.create_element("span");
    assert_eq!(
        tree.insert_before(root, stranger, b),
        Err(TreeError::NotAChild {
            parent: root,
            reference: b
        })
    );
    assert_eq!(tree.children(a).count(), 2);
}

#[test]
fn replace_child_swaps_in_place() {
    let (mut tree, [_, a, b, c]) = three_levels();
    let d = tree.create_element("span");
    tree.replace_child(b, d).unwrap();
    assert_eq!(tree.children(a).collect::<Vec<_>>(), vec![d, c]);
    // The replaced node is detached but still alive: the DOM lets you re-insert it.
    assert!(tree.is_alive(b));
    assert_eq!(tree.parent(b), None);
}

#[test]
fn removing_a_subtree_frees_every_node_in_it() {
    let (mut tree, [root, a, b, c]) = three_levels();
    assert_eq!(tree.len(), 4);

    assert_eq!(tree.remove_subtree(a), 3);

    assert_eq!(tree.len(), 1);
    assert!(tree.is_alive(root));
    assert!(!tree.is_alive(a));
    assert!(!tree.is_alive(b));
    assert!(!tree.is_alive(c));
    assert_eq!(tree.children(root).count(), 0);
}

/// The reason for the generation counter (DECISIONS D-17). A recycled slot must not make a
/// stale handle resolve to whatever moved in.
#[test]
fn a_stale_handle_does_not_resolve_to_the_node_that_reused_its_slot() {
    let mut tree = Tree::new();
    let first = tree.create_element("div");
    tree.remove_subtree(first);

    let second = tree.create_element("span");
    // The slot really was reused — otherwise this test proves nothing.
    assert_eq!(first.debug_parts().0, second.debug_parts().0);
    assert_ne!(first.debug_parts().1, second.debug_parts().1);

    assert!(!tree.is_alive(first));
    assert!(tree.get(first).is_none());
    assert!(tree.is_alive(second));
    assert_eq!(
        tree.set_root(first),
        Err(TreeError::StaleHandle(first)),
        "a stale handle must not be usable as a root"
    );
}

#[test]
fn removing_the_root_clears_it() {
    let (mut tree, [root, ..]) = three_levels();
    tree.remove_subtree(root);
    assert_eq!(tree.root(), None);
    assert!(tree.is_empty());
}

#[test]
fn deleting_a_deep_chain_does_not_overflow_the_stack() {
    // 100k deep is far past anything a document produces and well past the default stack
    // depth a recursive free would survive.
    let mut tree = Tree::with_capacity(100_001);
    let root = tree.create_element("div");
    let mut current = root;
    for _ in 0..100_000 {
        let child = tree.create_element("div");
        tree.append_child(current, child).unwrap();
        current = child;
    }
    assert_eq!(tree.remove_subtree(root), 100_001);
    assert!(tree.is_empty());
}

// ---- dirty tracking ------------------------------------------------------------------

#[test]
fn marking_a_leaf_dirty_tells_its_ancestors() {
    let (mut tree, [root, a, b, c]) = three_levels();
    tree.clear_all_dirty();

    tree.mark_dirty(b, DirtyFlags::PAINT);

    assert_eq!(tree.dirty(b), DirtyFlags::PAINT);
    assert_eq!(tree.dirty(a), DirtyFlags::SUBTREE_PAINT);
    assert_eq!(tree.dirty(root), DirtyFlags::SUBTREE_PAINT);
    // The sibling is untouched, which is the whole point.
    assert_eq!(tree.dirty(c), DirtyFlags::empty());
}

#[test]
fn a_style_change_implies_layout_and_paint() {
    let (mut tree, [root, a, b, _]) = three_levels();
    tree.clear_all_dirty();

    tree.mark_dirty(b, DirtyFlags::STYLE);

    assert_eq!(tree.dirty(b), DirtyFlags::ALL_SELF);
    assert_eq!(tree.dirty(a), DirtyFlags::ALL_SUBTREE);
    assert_eq!(tree.dirty(root), DirtyFlags::ALL_SUBTREE);
}

/// The property M6's acceptance test will lean on: re-marking an already-dirty subtree
/// stops at the first ancestor that already knows, so the walk does not repeat itself.
#[test]
fn the_ancestor_walk_stops_at_an_already_marked_ancestor() {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    let mut current = root;
    for _ in 0..64 {
        let child = tree.create_element("div");
        tree.append_child(current, child).unwrap();
        current = child;
    }
    tree.clear_all_dirty();

    let before = tree.stats().dirty_walk_steps;
    tree.mark_dirty(current, DirtyFlags::PAINT);
    let first_walk = tree.stats().dirty_walk_steps - before;
    assert_eq!(first_walk, 64, "the first mark has to reach the root");

    let before = tree.stats().dirty_walk_steps;
    tree.mark_dirty(current, DirtyFlags::PAINT);
    let second_walk = tree.stats().dirty_walk_steps - before;
    assert_eq!(
        second_walk, 1,
        "the second mark should stop at the first ancestor that already knows"
    );
}

#[test]
fn a_new_node_starts_fully_dirty() {
    let mut tree = Tree::new();
    let id = tree.create_element("div");
    assert_eq!(tree.dirty(id), DirtyFlags::ALL_SELF);
}

#[test]
fn detaching_a_child_dirties_the_parents_layout() {
    let (mut tree, [root, a, b, _]) = three_levels();
    tree.clear_all_dirty();
    tree.remove_child(a, b).unwrap();
    assert!(tree.dirty(a).contains(DirtyFlags::LAYOUT));
    assert!(tree.dirty(root).contains(DirtyFlags::SUBTREE_LAYOUT));
}

#[test]
fn marking_a_dead_node_is_a_no_op_rather_than_a_panic() {
    let mut tree = Tree::new();
    let id = tree.create_element("div");
    tree.remove_subtree(id);
    tree.mark_dirty(id, DirtyFlags::PAINT);
    assert_eq!(tree.dirty(id), DirtyFlags::empty());
}

// ---- geometry ------------------------------------------------------------------------

#[test]
fn absolute_rect_sums_ancestor_origins() {
    let (mut tree, [root, a, b, _]) = three_levels();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);
    tree.node_mut(a).layout = Rect::from_xywh(10.0, 20.0, 100.0, 100.0);
    tree.node_mut(b).layout = Rect::from_xywh(5.0, 5.0, 20.0, 20.0);

    assert_eq!(
        tree.absolute_rect(b),
        Some(Rect::from_xywh(15.0, 25.0, 20.0, 20.0))
    );
}

// ---- custom nodes --------------------------------------------------------------------

#[test]
fn a_custom_node_reports_its_own_size_and_hit() {
    let mut tree = Tree::new();
    let mut stub = ColorBox::new(Size::new(64.0, 48.0), Color::BLACK);
    assert_eq!(
        stub.measure(MeasureConstraints::definite_width(1000.0)),
        Size::new(64.0, 48.0),
        "a fixed-size custom node ignores the constraints it is given"
    );
    assert_eq!(
        stub.hit_test(Point::new(4.0, 4.0)).map(|hit| hit.local),
        Some(Point::new(4.0, 4.0))
    );

    let id = tree.create_custom(stub);
    assert!(matches!(
        tree.node(id).kind,
        crisol_tree::NodeKind::Custom(_)
    ));
}

#[test]
fn box_style_defaults_to_an_invisible_but_present_box() {
    let style = BoxStyle::default();
    assert!(style.visible);
    assert!(style.background.is_transparent());
    assert!(!style.clips_children);
}

/// The cycle check short-circuits on childless nodes, which is what keeps building a
/// document linear rather than quadratic in its depth. Asserted because the guard is
/// invisible from the outside until someone deletes it and the parser gets slow.
#[test]
fn building_a_deep_tree_is_linear_not_quadratic() {
    let depth = 50_000;
    let mut tree = Tree::with_capacity(depth + 1);
    let root = tree.create_element("div");
    let mut current = root;
    let start = std::time::Instant::now();
    for _ in 0..depth {
        let child = tree.create_element("div");
        tree.append_child(current, child).unwrap();
        current = child;
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "appending {depth} leaves took {elapsed:?}; the cycle check is walking ancestors \
         for nodes that cannot possibly be ancestors"
    );

    // And the check still fires when it should.
    assert!(matches!(
        tree.append_child(current, root),
        Err(TreeError::WouldCycle { .. })
    ));
}
