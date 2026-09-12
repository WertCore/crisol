//! The component abstraction, and the bindings that make one reactive.
//!
//! A component here runs **once**. It builds its nodes, registers effects that keep specific
//! attributes and text in sync, and returns. Nothing re-runs it — that is what makes "no
//! full-tree rebuilds" the default rather than an optimisation, and it is why ROADMAP §M7's
//! acceptance is about node counts.

use crisol_tree::NodeId;

use crate::{Cx, Scope, Track};

/// Something that can build a subtree.
///
/// Implemented for any `FnOnce(&mut Cx) -> NodeId`, so a plain function is a component and
/// the trait exists for the cases that need state or a name — including, at M16, a component
/// whose body lives in JavaScript.
pub trait View {
    /// Builds the subtree and returns its root. Called once.
    fn build(self, cx: &mut Cx<'_, '_>) -> NodeId;
}

impl<F> View for F
where
    F: FnOnce(&mut Cx<'_, '_>) -> NodeId,
{
    fn build(self, cx: &mut Cx<'_, '_>) -> NodeId {
        self(cx)
    }
}

/// A mounted component: its root node, and the scope that owns everything it created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mounted {
    /// The root of what was built.
    pub node: NodeId,
    /// Disposing this undoes the component's effects. It does not remove `node`.
    pub scope: Scope,
}

impl Mounted {
    /// Disposes the component and removes its nodes.
    pub fn unmount(self, cx: &mut Cx<'_, '_>) {
        cx.runtime().dispose(self.scope, cx.dom);
        cx.dom.remove_subtree(self.node);
    }
}

/// Builds `view` in a fresh scope and appends it to `parent`.
pub fn mount(cx: &mut Cx<'_, '_>, parent: NodeId, view: impl View) -> Mounted {
    let runtime = cx.runtime();
    let (scope, node) = runtime.scope(|_| view.build(cx));
    cx.dom
        .append_child(parent, node)
        .expect("parent is alive and node is fresh");
    Mounted { node, scope }
}

/// Creates a detached element.
pub fn element(cx: &mut Cx<'_, '_>, tag: &str) -> NodeId {
    cx.dom.create_element(tag)
}

/// Creates an element with a class, the shape most call sites want.
pub fn element_with_class(cx: &mut Cx<'_, '_>, tag: &str, class: &str) -> NodeId {
    let node = cx.dom.create_element(tag);
    cx.dom.set_attribute(node, "class", class);
    node
}

/// Creates a detached text node.
pub fn text(cx: &mut Cx<'_, '_>, value: &str) -> NodeId {
    cx.dom.create_text(value)
}

/// Appends `child` to `parent`.
///
/// # Panics
///
/// If `parent` is dead or the insertion would make a cycle — both caller bugs while
/// building a fresh subtree.
pub fn append(cx: &mut Cx<'_, '_>, parent: NodeId, child: NodeId) {
    cx.dom
        .append_child(parent, child)
        .expect("appending a fresh child to a live parent");
}

/// Keeps a text node's content equal to `value`.
///
/// The effect re-runs when what `value` read changes, and writes one text node. An edit to
/// one item's label costs one `set_text` and relayout of that item's ancestor chain — which
/// is the number ROADMAP §M6 made measurable.
pub fn bind_text(
    cx: &mut Cx<'_, '_>,
    node: NodeId,
    mut value: impl FnMut(&Track<'_>) -> String + 'static,
) {
    let runtime = cx.runtime();
    runtime.effect(cx.dom, move |cx| {
        let next = value(cx);
        cx.dom.set_text(node, &next);
    });
}

/// Keeps an attribute equal to `value`. `None` removes it.
pub fn bind_attribute(
    cx: &mut Cx<'_, '_>,
    node: NodeId,
    name: &str,
    mut value: impl FnMut(&Track<'_>) -> Option<String> + 'static,
) {
    let runtime = cx.runtime();
    let name = name.to_owned();
    runtime.effect(cx.dom, move |cx| match value(cx) {
        Some(next) => {
            cx.dom.set_attribute(node, &name, &next);
        }
        None => {
            cx.dom.remove_attribute(node, &name);
        }
    });
}

/// Keeps a single class present or absent.
///
/// Toggling one class rather than rewriting `class` wholesale is what lets the style engine
/// invalidate just this node and its following siblings instead of the subtree.
pub fn bind_class(
    cx: &mut Cx<'_, '_>,
    node: NodeId,
    class: &str,
    mut present: impl FnMut(&Track<'_>) -> bool + 'static,
) {
    let runtime = cx.runtime();
    let class = class.to_owned();
    runtime.effect(cx.dom, move |cx| {
        let next = present(cx);
        cx.dom.toggle_class(node, &class, next);
    });
}
