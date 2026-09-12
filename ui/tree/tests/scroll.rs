//! Scrolling: what it moves, what it marks, and what it refuses.

use crisol_display_list::{Point, Rect, Size};
use crisol_tree::{DirtyFlags, NodeId, Tree};

/// A `viewport` box containing one tall child, scrollable by `max`.
fn scroller(max: Size) -> (Tree, NodeId, NodeId) {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    let child = tree.create_element("div");
    tree.append_child(root, child).unwrap();

    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 100.0, 100.0);
    tree.node_mut(root).style.scrolls = true;
    tree.node_mut(root).style.clips_children = true;
    tree.node_mut(root).scroll_max = max;
    tree.node_mut(child).layout = Rect::from_xywh(0.0, 0.0, 100.0, 100.0 + max.height);
    tree.clear_all_dirty();
    (tree, root, child)
}

#[test]
fn a_scroll_marks_paint_and_never_layout() {
    let (mut tree, root, _) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });

    tree.scroll_by(root, Point { x: 0.0, y: 50.0 });

    // The whole point. Scrolling moves where content is drawn; not one box changed, and
    // treating it as a layout change would relayout the document sixty times a second
    // through a fling.
    assert_eq!(tree.dirty(root), DirtyFlags::PAINT);
    assert!(!tree.dirty(root).contains(DirtyFlags::LAYOUT));
    assert!(!tree.dirty(root).contains(DirtyFlags::STYLE));
}

#[test]
fn scrolling_stops_at_the_end_of_the_content() {
    let (mut tree, root, _) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });

    let moved = tree.scroll_by(root, Point { x: 0.0, y: 250.0 });
    assert_eq!(moved, Point { x: 0.0, y: 250.0 });
    assert_eq!(tree.scroll_offset(root).y, 250.0);

    // Asking for more than is left returns only what was taken. The remainder is what a
    // caller hands outward, so getting this wrong breaks scroll chaining rather than
    // showing up here.
    let moved = tree.scroll_by(root, Point { x: 0.0, y: 400.0 });
    assert_eq!(moved, Point { x: 0.0, y: 150.0 });
    assert_eq!(tree.scroll_offset(root).y, 400.0);

    assert_eq!(tree.scroll_by(root, Point { x: 0.0, y: 10.0 }), Point::ZERO);
}

#[test]
fn scrolling_never_goes_above_the_top() {
    let (mut tree, root, _) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });
    assert_eq!(
        tree.scroll_by(root, Point { x: 0.0, y: -10.0 }),
        Point::ZERO
    );

    tree.scroll_by(root, Point { x: 0.0, y: 100.0 });
    let moved = tree.scroll_by(root, Point { x: 0.0, y: -250.0 });
    assert_eq!(moved, Point { x: 0.0, y: -100.0 });
    assert_eq!(tree.scroll_offset(root).y, 0.0);
}

#[test]
fn an_axis_with_nowhere_to_go_does_not_move() {
    let (mut tree, root, _) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });
    // A vertical list dragged diagonally scrolls down, not sideways.
    let moved = tree.scroll_by(root, Point { x: 30.0, y: 30.0 });
    assert_eq!(moved, Point { x: 0.0, y: 30.0 });
}

#[test]
fn a_node_that_clips_but_does_not_scroll_refuses() {
    let (mut tree, root, _) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });
    // `overflow: clip` does both halves of `hidden` except the scrolling.
    tree.node_mut(root).style.scrolls = false;

    assert_eq!(tree.scroll_by(root, Point { x: 0.0, y: 50.0 }), Point::ZERO);
    assert!(!tree.set_scroll(root, Point { x: 0.0, y: 50.0 }));
    assert!(!tree.is_scrollable(root));
    assert_eq!(tree.dirty(root), DirtyFlags::empty());
}

#[test]
fn content_shrinking_pulls_the_offset_back_into_range() {
    let (mut tree, root, _) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });
    tree.scroll_by(root, Point { x: 0.0, y: 400.0 });
    tree.clear_all_dirty();

    // Layout reports less content than before — items were removed, or the box grew. An
    // offset left where it was would be looking at blank space past the end.
    tree.set_scroll_max(
        root,
        Size {
            width: 0.0,
            height: 120.0,
        },
    );
    assert_eq!(tree.scroll_offset(root).y, 120.0);
    assert!(tree.dirty(root).contains(DirtyFlags::PAINT));
}

#[test]
fn content_growing_leaves_the_offset_alone() {
    let (mut tree, root, _) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });
    tree.scroll_by(root, Point { x: 0.0, y: 200.0 });
    tree.clear_all_dirty();

    tree.set_scroll_max(
        root,
        Size {
            width: 0.0,
            height: 900.0,
        },
    );
    assert_eq!(
        tree.scroll_offset(root).y,
        200.0,
        "a list that jumped when something loaded below it would be unusable"
    );
    assert_eq!(tree.dirty(root), DirtyFlags::empty(), "nothing moved");
}

#[test]
fn absolute_position_is_where_the_node_is_drawn() {
    let (mut tree, root, child) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });
    assert_eq!(tree.absolute_rect(child).unwrap().min_y(), 0.0);

    tree.scroll_by(root, Point { x: 0.0, y: 75.0 });

    // Not where it was laid out. A screen reader told otherwise points at the wrong place,
    // and `absolute_rect` is what feeds accesskit.
    assert_eq!(tree.absolute_rect(child).unwrap().min_y(), -75.0);
    assert_eq!(
        tree.absolute_rect(root).unwrap().min_y(),
        0.0,
        "the container itself did not move"
    );
}

#[test]
fn setting_an_offset_outright_clamps_and_reports_whether_it_moved() {
    let (mut tree, root, _) = scroller(Size {
        width: 0.0,
        height: 400.0,
    });
    assert!(tree.set_scroll(root, Point { x: 0.0, y: 9_999.0 }));
    assert_eq!(tree.scroll_offset(root).y, 400.0);
    assert!(
        !tree.set_scroll(root, Point { x: 0.0, y: 500.0 }),
        "already clamped there, so nothing changed"
    );
}
