//! Scroll gestures: where they land, and where they go when the inner list runs out.

use crisol_display_list::{Point, Rect, Size};
use crisol_events::{hit_test, scroll_at};
use crisol_tree::{NodeId, Tree};

/// An outer scroller containing an inner scroller containing three rows.
///
/// ```text
///   outer  0,0   200x200,  scrollable to 300
///     inner  0,0 200x100,  scrollable to 200
///       row0   0,0   200x100
///       row1   0,100 200x100
///       row2   0,200 200x100
/// ```
fn nested() -> (Tree, NodeId, NodeId, [NodeId; 3]) {
    let mut tree = Tree::new();
    let outer = tree.create_element("div");
    tree.set_root(outer).unwrap();
    let inner = tree.create_element("div");
    tree.append_child(outer, inner).unwrap();

    let mut rows = [NodeId::from_bits(0).unwrap_or(inner); 3];
    for (index, row) in rows.iter_mut().enumerate() {
        let node = tree.create_element("div");
        tree.append_child(inner, node).unwrap();
        tree.node_mut(node).layout = Rect::from_xywh(0.0, index as f32 * 100.0, 200.0, 100.0);
        *row = node;
    }

    tree.node_mut(outer).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);
    tree.node_mut(outer).style.scrolls = true;
    tree.node_mut(outer).style.clips_children = true;
    tree.node_mut(outer).scroll_max = Size {
        width: 0.0,
        height: 300.0,
    };

    tree.node_mut(inner).layout = Rect::from_xywh(0.0, 0.0, 200.0, 100.0);
    tree.node_mut(inner).style.scrolls = true;
    tree.node_mut(inner).style.clips_children = true;
    tree.node_mut(inner).scroll_max = Size {
        width: 0.0,
        height: 200.0,
    };

    tree.clear_all_dirty();
    (tree, outer, inner, rows)
}

#[test]
fn the_innermost_scroller_under_the_pointer_takes_the_gesture() {
    let (mut tree, outer, inner, _) = nested();

    let scrolled = scroll_at(&mut tree, Point::new(50.0, 50.0), Point::new(0.0, 60.0))
        .expect("something moved");

    assert_eq!(scrolled.node, inner);
    assert_eq!(scrolled.applied, Point::new(0.0, 60.0));
    assert_eq!(tree.scroll_offset(inner).y, 60.0);
    assert_eq!(
        tree.scroll_offset(outer).y,
        0.0,
        "the outer container must not move while the inner one still has room"
    );
}

#[test]
fn a_gesture_chains_outward_when_the_inner_list_runs_out() {
    let (mut tree, outer, inner, _) = nested();
    tree.set_scroll(inner, Point::new(0.0, 180.0));

    // 20 left inside, so 50 splits 20/30. Swallowing the remainder is the behaviour people
    // notice only when it is missing: the page stops dead at the bottom of a small list.
    let scrolled =
        scroll_at(&mut tree, Point::new(50.0, 50.0), Point::new(0.0, 50.0)).expect("moved");

    assert_eq!(tree.scroll_offset(inner).y, 200.0, "inner hit its end");
    assert_eq!(tree.scroll_offset(outer).y, 30.0, "outer took the rest");
    assert_eq!(scrolled.applied, Point::new(0.0, 50.0));
    assert_eq!(scrolled.node, inner, "the innermost that moved");
}

#[test]
fn a_gesture_over_an_exhausted_chain_moves_nothing() {
    let (mut tree, outer, inner, _) = nested();
    tree.set_scroll(inner, Point::new(0.0, 200.0));
    tree.set_scroll(outer, Point::new(0.0, 300.0));

    assert!(scroll_at(&mut tree, Point::new(50.0, 50.0), Point::new(0.0, 40.0)).is_none());
}

#[test]
fn a_gesture_where_nothing_scrolls_moves_nothing() {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);

    assert!(scroll_at(&mut tree, Point::new(50.0, 50.0), Point::new(0.0, 40.0)).is_none());
}

#[test]
fn hit_testing_follows_the_content_rather_than_the_layout() {
    let (mut tree, _, inner, rows) = nested();

    // Before scrolling, the top of the viewport is the top of the first row.
    assert_eq!(
        hit_test(&tree, Point::new(50.0, 10.0)).unwrap().node,
        rows[0]
    );

    tree.set_scroll(inner, Point::new(0.0, 100.0));

    // The same pixel is now over the second row. A hit test reading the laid-out position
    // would still say the first, and would click whatever used to be under the pointer.
    assert_eq!(
        hit_test(&tree, Point::new(50.0, 10.0)).unwrap().node,
        rows[1]
    );

    // ...and the local coordinate is measured from where the row is drawn, not where it
    // was laid out, or a text caret would land a hundred pixels off.
    let hit = hit_test(&tree, Point::new(50.0, 10.0)).unwrap();
    assert_eq!(hit.local.y, 10.0);
}

#[test]
fn content_scrolled_out_of_the_clip_is_not_hit() {
    let (mut tree, _, inner, rows) = nested();
    tree.set_scroll(inner, Point::new(0.0, 200.0));

    // Row 0 is now 200px above the clip. Nothing in the viewport belongs to it.
    for y in [1.0, 50.0, 99.0] {
        let hit = hit_test(&tree, Point::new(50.0, y)).unwrap();
        assert_ne!(hit.node, rows[0], "row 0 is scrolled out of view");
    }
}

#[test]
fn a_scroll_reports_the_area_that_has_to_be_repainted() {
    let (mut tree, outer, inner, _) = nested();

    let scrolled =
        scroll_at(&mut tree, Point::new(50.0, 50.0), Point::new(0.0, 40.0)).expect("moved");
    assert_eq!(
        scrolled.damage,
        tree.absolute_rect(inner).unwrap(),
        "only the inner container's contents moved"
    );

    // When the chain reaches the outer one, the damage has to cover it too — and its box
    // contains every inner container that could also have moved, so one rectangle is enough.
    tree.set_scroll(inner, Point::new(0.0, 200.0));
    let scrolled =
        scroll_at(&mut tree, Point::new(50.0, 50.0), Point::new(0.0, 40.0)).expect("moved");
    assert_eq!(scrolled.damage, tree.absolute_rect(outer).unwrap());
}

// ---- momentum ----------------------------------------------------------------------------

use std::time::Duration;

use crisol_events::{Fling, VelocityTracker};

const FRAME: Duration = Duration::from_micros(16_667);

#[test]
fn a_slow_release_is_not_a_flick() {
    let (_, _, inner, _) = nested();
    assert!(Fling::new(inner, Point::new(0.0, 20.0)).is_none());
    assert!(Fling::new(inner, Point::new(0.0, 400.0)).is_some());
}

#[test]
fn a_fling_decays_to_a_stop() {
    let (mut tree, _, inner, _) = nested();
    let mut fling = Fling::new(inner, Point::new(0.0, 600.0)).unwrap();

    let mut frames = 0;
    let mut travelled = 0.0;
    while let Some(step) = fling.advance(&mut tree, FRAME) {
        travelled += step.applied.y;
        frames += 1;
        assert!(
            frames < 600,
            "a fling that never ends is a spinning frame loop"
        );
    }

    assert!(fling.is_finished());
    assert!(travelled > 0.0, "it went somewhere: {travelled}");
    assert!(
        (2..=180).contains(&frames),
        "a flick should settle in well under three seconds, took {frames} frames"
    );
}

#[test]
fn distance_does_not_depend_on_the_frame_rate() {
    let total = Duration::from_millis(300);
    let at = |frames: u32| {
        let (mut tree, _, inner, _) = nested();
        let mut fling = Fling::new(inner, Point::new(0.0, 500.0)).unwrap();
        let step = total / frames;
        for _ in 0..frames {
            fling.advance(&mut tree, step);
        }
        tree.scroll_offset(inner).y
    };

    // Integrating the decay over the interval rather than holding the speed for the frame
    // is what makes these agree. A fling that travelled further on a 120Hz display is a bug
    // people feel without being able to name.
    let sixty = at(18);
    let hundred_and_twenty = at(36);
    assert!(
        (sixty - hundred_and_twenty).abs() < 1.0,
        "60Hz went {sixty}, 120Hz went {hundred_and_twenty}"
    );
}

#[test]
fn a_fling_stops_at_the_end_rather_than_chaining_outward() {
    let (mut tree, outer, inner, _) = nested();
    tree.set_scroll(inner, Point::new(0.0, 190.0));

    let mut fling = Fling::new(inner, Point::new(0.0, 3_000.0)).unwrap();
    while fling.advance(&mut tree, FRAME).is_some() {}

    assert_eq!(tree.scroll_offset(inner).y, 200.0);
    assert_eq!(
        tree.scroll_offset(outer).y,
        0.0,
        "a flick belongs to the list it started in; leaping to the page behind would feel broken"
    );
}

#[test]
fn velocity_is_measured_over_a_window_not_the_last_two_samples() {
    let mut tracker = VelocityTracker::new();
    // A steady 600 px/s drag downward, sampled every frame.
    for frame in 0..10 {
        let time = FRAME * frame;
        tracker.sample(time, Point::new(0.0, time.as_secs_f32() * 600.0));
    }
    assert!((tracker.velocity().y - 600.0).abs() < 1.0);

    // The finger pauses for one frame before lifting. Reading only the last two samples
    // would call that a dead stop and swallow the flick.
    tracker.sample(
        FRAME * 10,
        Point::new(0.0, FRAME.as_secs_f32() * 9.0 * 600.0),
    );
    assert!(
        tracker.velocity().y > 300.0,
        "one still frame should not erase the gesture: {:?}",
        tracker.velocity()
    );
}

#[test]
fn a_tracker_with_nothing_in_it_reports_no_movement() {
    let mut tracker = VelocityTracker::new();
    assert_eq!(tracker.velocity(), Point::ZERO);

    tracker.sample(Duration::ZERO, Point::new(0.0, 10.0));
    assert_eq!(
        tracker.velocity(),
        Point::ZERO,
        "one sample is not a velocity"
    );

    tracker.clear();
    assert_eq!(tracker.velocity(), Point::ZERO);
}
