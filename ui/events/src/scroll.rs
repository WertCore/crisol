//! Turning a scroll gesture into movement.

use crisol_display_list::{Point, Rect};
use crisol_tree::{NodeId, Tree};

use crate::hit::hit_test;

/// What a scroll gesture did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrolled {
    /// The innermost node that moved.
    pub node: NodeId,
    /// How far content moved in total, summed over every node in the chain.
    pub applied: Point,
    /// What has to be repainted: the box of the outermost node that moved.
    ///
    /// One rectangle rather than several, because an outer scroll container's box contains
    /// every inner one that could also have moved.
    pub damage: Rect,
}

/// Applies a scroll gesture at `point`, chaining outward through ancestors.
///
/// Starts at the node under the pointer and walks up. Each scroll container takes what it
/// can and passes the rest outward, so a list that has hit its end lets the page underneath
/// keep moving instead of swallowing the gesture — which is the behaviour every platform has
/// and the one users notice only when it is missing.
///
/// `delta` is in logical pixels, positive down and right. Returns `None` when nothing moved.
pub fn scroll_at(tree: &mut Tree, point: Point, delta: Point) -> Option<Scrolled> {
    let start = hit_test(tree, point)
        .map(|hit| hit.node)
        .or_else(|| tree.root())?;
    scroll_from(tree, start, delta)
}

/// As [`scroll_at`], but starting from a known node rather than a pointer position.
///
/// The form a keyboard scroll wants: page-down acts on what is focused, and there may be no
/// pointer anywhere.
pub fn scroll_from(tree: &mut Tree, start: NodeId, delta: Point) -> Option<Scrolled> {
    let mut remaining = delta;
    let mut applied = Point::ZERO;
    let mut innermost = None;
    let mut outermost = None;

    let mut current = Some(start);
    while let Some(node) = current {
        if remaining.x == 0.0 && remaining.y == 0.0 {
            break;
        }
        let moved = tree.scroll_by(node, remaining);
        if moved.x != 0.0 || moved.y != 0.0 {
            applied = applied + moved;
            remaining = remaining - moved;
            innermost.get_or_insert(node);
            outermost = Some(node);
        }
        current = tree.parent(node);
    }

    let node = innermost?;
    let damage = tree
        .absolute_rect(outermost.unwrap_or(node))
        .unwrap_or(Rect::ZERO);
    Some(Scrolled {
        node,
        applied,
        damage,
    })
}
