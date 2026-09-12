//! Focus order and keyboard navigation.
//!
//! ROADMAP §M5's acceptance is *a form with three text inputs is fully keyboard-navigable*,
//! and that is the whole of this file: which nodes can be focused, what order they are in,
//! and what Tab does.
//!
//! Focus order is **document order**, with no `tabindex`. A positive `tabindex` lets an
//! author reorder the sequence independently of the document, which is the single most
//! reliable way to produce an interface that is unusable with a keyboard — every
//! accessibility guide in existence says not to do it. `tabindex="0"` and `tabindex="-1"`
//! are useful and are supported as [`Focusable::Yes`] and [`Focusable::Programmatic`].

use crisol_tree::{ElementState, NodeId, Tree};

/// Whether and how a node can take focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Focusable {
    /// Cannot be focused at all.
    #[default]
    No,
    /// Can be focused by keyboard and by pointer. `tabindex="0"`.
    Yes,
    /// Can be focused by pointer or by script, but Tab skips it. `tabindex="-1"`.
    Programmatic,
}

/// Which way Tab is going.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Tab.
    #[default]
    Forward,
    /// Shift-Tab.
    Backward,
}

/// Whether an element takes focus, by what it is.
///
/// Derived from the tag and attributes rather than stored, so that a tree built by hand and
/// one built by the parser behave alike, and so that nothing has to remember to keep a
/// cached flag in step with an attribute change.
#[must_use]
pub fn focusable(tree: &Tree, node: NodeId) -> Focusable {
    let Some(data) = tree.element(node) else {
        return Focusable::No;
    };

    // An explicit `tabindex` wins, with the two values worth supporting.
    if let Some(value) = data.attribute("tabindex") {
        return match value.trim() {
            "0" => Focusable::Yes,
            // Any negative value means "focusable but not in the tab order". Any positive one
            // means "reorder the tab sequence", which this engine declines to do — see the
            // module comment — and treats as a plain `0`.
            other if other.starts_with('-') => Focusable::Programmatic,
            _ => Focusable::Yes,
        };
    }

    // A disabled control is not focusable, which is most of what disabling means.
    if data.state.contains(ElementState::DISABLED) {
        return Focusable::No;
    }

    match data.tag.as_str() {
        "button" | "input" | "select" | "textarea" => Focusable::Yes,
        // A link is focusable only if it goes somewhere.
        "a" if data.has_attribute("href") => Focusable::Yes,
        _ => Focusable::No,
    }
}

/// Every node Tab will stop at, in order.
///
/// Document order, which is also paint order and reading order. An element inside a
/// `display: none` subtree is skipped, because it is not there.
#[must_use]
pub fn tab_order(tree: &Tree) -> Vec<NodeId> {
    let mut out = Vec::new();
    let Some(root) = tree.root() else {
        return out;
    };
    collect(tree, root, &mut out);
    out
}

fn collect(tree: &Tree, node: NodeId, out: &mut Vec<NodeId>) {
    let Some(entry) = tree.get(node) else {
        return;
    };
    // A hidden subtree contains nothing keyboard-reachable, and one that generates no box is
    // not there at all. An invisible control that Tab still stops at is a focus ring on empty
    // space, which is worse than no focus ring.
    if !entry.style.generates_box || !entry.style.visible {
        return;
    }
    if focusable(tree, node) == Focusable::Yes {
        out.push(node);
    }
    for child in tree.children(node) {
        collect(tree, child, out);
    }
}

/// The next node to focus after `current`, wrapping at the ends.
///
/// `None` when nothing in the tree is focusable. Wrapping rather than stopping because a
/// keyboard user who tabs past the last control expects to come back round, and because an
/// engine that traps focus at the end has no way to say so.
#[must_use]
pub fn next_focus(tree: &Tree, current: Option<NodeId>, direction: Direction) -> Option<NodeId> {
    let order = tab_order(tree);
    if order.is_empty() {
        return None;
    }

    let position = current.and_then(|node| order.iter().position(|id| *id == node));
    let next = match (position, direction) {
        (Some(index), Direction::Forward) => (index + 1) % order.len(),
        (Some(0), Direction::Backward) => order.len() - 1,
        (Some(index), Direction::Backward) => index - 1,
        // Nothing focused, or focus is on something outside the tab order: start at the end
        // the direction comes from.
        (None, Direction::Forward) => 0,
        (None, Direction::Backward) => order.len() - 1,
    };
    order.get(next).copied()
}
