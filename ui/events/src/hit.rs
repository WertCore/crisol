//! Turning a point into a node.
//!
//! The first thing every other part of M5 needs: a pointer event has to know what it landed
//! on before anything can be dispatched, focused or announced.
//!
//! Hit testing walks the tree in *reverse paint order* — last sibling first, children before
//! their parent — because the last thing painted is the top thing on screen and is what the
//! user believes they clicked.

use crisol_display_list::{Point, Rect};
use crisol_text::{Cursor, TextLayout};
use crisol_tree::{CustomHit, NodeId, NodeKind, Tree};

/// What the point landed on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// The topmost node under the point.
    pub node: NodeId,
    /// The point in that node's own coordinate space, with the origin at its border box's
    /// top-left. What a custom node, a text query or a drag handle all want.
    pub local: Point,
    /// The node's border box in absolute coordinates, so a caller does not have to walk the
    /// ancestors again to recover it.
    pub bounds: Rect,
    /// What a custom node made of the point, when the node was one (DECISIONS D-19).
    pub custom: Option<CustomHit>,
    /// Where in the text the point landed, when the node was a text node and shaped text was
    /// available to ask.
    pub cursor: Option<Cursor>,
}

/// Somewhere to look up shaped text while hit testing.
///
/// A trait so `crisol-events` does not have to depend on whatever produced the text —
/// `crisol-layout` returns a `NodeMap`, a test may have a single layout, and an application
/// may have its own store. Pass `&()` when the caller does not have text or does not care
/// where in it the point landed.
pub trait TextLookup {
    /// The shaped text for a node.
    fn text_for(&self, node: NodeId) -> Option<&TextLayout>;
}

impl TextLookup for () {
    fn text_for(&self, _node: NodeId) -> Option<&TextLayout> {
        None
    }
}

impl<F> TextLookup for F
where
    F: Fn(NodeId) -> Option<&'static TextLayout>,
{
    fn text_for(&self, node: NodeId) -> Option<&TextLayout> {
        self(node)
    }
}

/// The topmost node at `point`, or `None` when the point is outside the tree entirely.
///
/// `point` is in the same space as the display list: logical pixels, origin at the top-left
/// of the block the tree was laid out into.
#[must_use]
pub fn hit_test(tree: &Tree, point: Point) -> Option<Hit> {
    hit_test_with_text(tree, point, &())
}

/// As [`hit_test`], but also resolving a text node to a cursor position.
#[must_use]
pub fn hit_test_with_text(tree: &Tree, point: Point, text: &impl TextLookup) -> Option<Hit> {
    let root = tree.root()?;
    descend(tree, root, Point::ZERO, None, point, text)
}

/// Depth-first, last child first.
///
/// Recursive rather than iterative, unlike paint and subtree removal. Hit testing descends
/// only into the subtree containing the point, so its depth is the depth of one *chain* of
/// boxes rather than of the document — a four-hundred-page document is still only a handful
/// of nested boxes deep at any given pixel.
fn descend(
    tree: &Tree,
    id: NodeId,
    parent_origin: Point,
    clip: Option<Rect>,
    point: Point,
    text: &impl TextLookup,
) -> Option<Hit> {
    let node = tree.get(id)?;
    let bounds = node.layout.translate(parent_origin);

    // `visibility: hidden` removes a box from hit testing as well as from painting: an
    // invisible button must not be clickable, which is half the reason the property exists.
    if !node.style.visible {
        return None;
    }

    // A clipped-out subtree cannot contain the point, whatever its boxes say. Checking here
    // rather than per node is what makes a scrolled-away list cost nothing to miss.
    let clip = if node.style.clips_children {
        Some(match clip {
            Some(current) => current.intersection(bounds)?,
            None => bounds,
        })
    } else {
        clip
    };
    if let Some(clip) = clip
        && !clip.contains(point)
    {
        return None;
    }

    // Children first, last to first: the last painted is the topmost.
    let mut child = node.last_child();
    while let Some(current) = child {
        if let Some(hit) = descend(tree, current, bounds.origin, clip, point, text) {
            return Some(hit);
        }
        child = tree.get(current).and_then(crisol_tree::Node::prev_sibling);
    }

    if !bounds.contains(point) {
        return None;
    }

    let local = Point::new(point.x - bounds.min_x(), point.y - bounds.min_y());

    // A custom node gets to say what the point means inside it, and to say the point misses
    // entirely — which is how it declares holes (DECISIONS D-19).
    if let NodeKind::Custom(custom) = &node.kind {
        let custom_hit = custom.node.hit_test(local)?;
        return Some(Hit {
            node: id,
            local,
            bounds,
            custom: Some(custom_hit),
            cursor: None,
        });
    }

    let cursor = match &node.kind {
        NodeKind::Text(_) => text
            .text_for(id)
            .map(|layout| layout.point_to_cursor(local)),
        _ => None,
    };

    Some(Hit {
        node: id,
        local,
        bounds,
        custom: None,
        cursor,
    })
}

/// The chain from the root down to `node`, inclusive.
///
/// What event dispatch propagates along: capture runs it forwards, bubble backwards
/// (ROADMAP §M5). Returned as a vector rather than an iterator because both directions are
/// walked and the tree must not be borrowed across a listener call.
#[must_use]
pub fn path_to(tree: &Tree, node: NodeId) -> Vec<NodeId> {
    let mut path = Vec::new();
    let mut current = Some(node);
    while let Some(id) = current {
        path.push(id);
        current = tree.parent(id);
    }
    path.reverse();
    path
}
