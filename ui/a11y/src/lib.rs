//! accesskit bridge.
//!
//! ROADMAP §M5 puts accessibility here rather than in year three, and the reason is in the
//! milestone's own note: *it constrains the tree, focus model, and event system, and
//! retrofitting means restructuring.* By the time this file exists the constraints have
//! already been paid — the focus order is document order (D-34), `display: none` really
//! removes a node, and the tree is walked in reading order — so what remains is a
//! translation rather than a redesign.
//!
//! What a screen reader is given is a *second* tree: smaller than the box tree, made of the
//! things a user can perceive and act on. A `<div>` used for spacing is not one of those, and
//! a tree that announced every one of them would be unusable.

#![doc(html_root_url = "https://docs.rs/crisol-a11y/0.0.0")]

use accesskit::{Node, NodeId as AccessNodeId, Rect, Role, TreeUpdate};
use crisol_tree::{ElementState, NodeId, NodeKind, Tree};

/// Builds the accessibility tree a screen reader reads.
#[derive(Debug, Default)]
pub struct AccessibilityTree {
    /// Nodes that were in the last update, so a caller can tell what changed.
    last: Vec<AccessNodeId>,
}

impl AccessibilityTree {
    /// An empty bridge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many nodes the last update contained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.last.len()
    }

    /// Whether the last update was empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.last.is_empty()
    }

    /// Builds a complete update for `tree`, with `focus` as the focused node.
    ///
    /// A full update every time. Incremental updates are what M6 is for, and doing it now
    /// would mean maintaining a diff against a tree whose invalidation story does not exist
    /// yet — the classic way to ship a cache that is wrong in ways nobody notices until a
    /// screen reader user reports that a button announces the wrong label.
    pub fn update(&mut self, tree: &Tree, focus: Option<NodeId>) -> TreeUpdate {
        let mut nodes = Vec::new();
        let root = tree.root();

        if let Some(root) = root {
            build(tree, root, &mut nodes);
        }

        self.last = nodes.iter().map(|(id, _)| *id).collect();

        let root_id = root.map_or(AccessNodeId(0), to_access_id);
        // accesskit requires a focused node. Falling back to the root means "the window has
        // focus but nothing in it does", which is true and is what a screen reader expects
        // before the user has tabbed anywhere.
        let focus = focus
            .filter(|node| tree.is_alive(*node))
            .map_or(root_id, to_access_id);

        TreeUpdate {
            // One tree, not a graft into someone else's. Subtree ids are for embedding one
            // accessibility tree inside another, which an engine that owns its whole window
            // has no use for.
            tree_id: accesskit::TreeId::ROOT,
            nodes,
            tree: Some(accesskit::TreeInfo::new(root_id)),
            focus,
        }
    }
}

/// Converts a tree handle to accesskit's.
///
/// The same packed form taffy takes and JavaScript will get at M16 (D-17). One integer
/// representation of a handle, used by everything that needs one.
#[must_use]
pub fn to_access_id(node: NodeId) -> AccessNodeId {
    AccessNodeId(node.to_bits())
}

/// Converts an accesskit handle back, for routing an action to a node.
#[must_use]
pub fn from_access_id(id: AccessNodeId) -> Option<NodeId> {
    NodeId::from_bits(id.0)
}

/// Walks the box tree in reading order, emitting the nodes worth announcing.
///
/// Children are emitted before their parent so that a parent's child list can refer to nodes
/// that already exist, which is what accesskit expects of an update.
fn build(tree: &Tree, node: NodeId, out: &mut Vec<(AccessNodeId, Node)>) {
    if !is_perceivable(tree, node) {
        // A node that generates no box is not in the document, and a hidden one is present
        // but not perceivable. Neither is announced, and neither are its descendants — a
        // screen reader reading out a `display: none` dialog is the accessibility equivalent
        // of it being drawn on screen.
        return;
    }

    for child in tree.children(node) {
        build(tree, child, out);
    }

    let Some(mut access) = describe(tree, node) else {
        // Skipped. Its children were already emitted and float up to take its place, which
        // is what keeps a `<div>` used for spacing out of the announced tree without taking
        // its contents with it.
        return;
    };

    access.set_children(
        tree.children(node)
            .flat_map(|child| access_children_of(tree, child))
            .collect::<Vec<_>>(),
    );

    let rect = tree
        .absolute_rect(node)
        .unwrap_or(crisol_display_list::Rect::ZERO);
    access.set_bounds(Rect {
        x0: f64::from(rect.min_x()),
        y0: f64::from(rect.min_y()),
        x1: f64::from(rect.max_x()),
        y1: f64::from(rect.max_y()),
    });

    out.push((to_access_id(node), access));
}

/// Whether a node is present and visible, and therefore announceable at all.
fn is_perceivable(tree: &Tree, node: NodeId) -> bool {
    tree.get(node)
        .is_some_and(|entry| entry.style.generates_box && entry.style.visible)
}

/// The accessibility nodes a subtree contributes at its own level.
///
/// A node that is announced contributes itself. One that is skipped contributes whatever its
/// children contribute, which is how a `<div>` wrapper disappears without taking its contents
/// with it.
fn access_children_of(tree: &Tree, node: NodeId) -> Vec<AccessNodeId> {
    if !is_perceivable(tree, node) {
        return Vec::new();
    }
    if describe(tree, node).is_some() {
        return vec![to_access_id(node)];
    }
    tree.children(node)
        .flat_map(|child| access_children_of(tree, child))
        .collect()
}

/// What to announce a node as, or `None` when it should not be announced at all.
fn describe(tree: &Tree, node: NodeId) -> Option<Node> {
    let entry = tree.get(node)?;

    if let NodeKind::Text(text) = &entry.kind {
        if text.trim().is_empty() {
            return None;
        }
        let mut access = Node::new(Role::TextRun);
        access.set_value(text.clone());
        return Some(access);
    }

    let data = entry.kind.element()?;
    let role = match data.tag.as_str() {
        "html" => Role::Window,
        "button" => Role::Button,
        "a" if data.has_attribute("href") => Role::Link,
        "input" => match data.attribute("type").as_deref() {
            Some("checkbox") => Role::CheckBox,
            Some("radio") => Role::RadioButton,
            _ => Role::TextInput,
        },
        "textarea" => Role::TextInput,
        "select" => Role::ComboBox,
        "img" => Role::Image,
        "p" => Role::Paragraph,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Role::Heading,
        "ul" | "ol" => Role::List,
        "li" => Role::ListItem,
        "nav" => Role::Navigation,
        "main" => Role::Main,
        "header" => Role::Header,
        "footer" => Role::Footer,
        "form" => Role::Form,
        "label" => Role::Label,
        // Everything else is layout. A screen reader that announced every `<div>` and
        // `<span>` would drown the content in structure; their children are announced in
        // their place.
        _ => return None,
    };

    let mut access = Node::new(role);
    if let Some(label) = data.attribute("aria-label") {
        access.set_label(label.into_owned());
    }
    if data.state.contains(ElementState::DISABLED) {
        access.set_disabled();
    }
    if data.state.contains(ElementState::CHECKED) {
        access.set_toggled(accesskit::Toggled::True);
    }
    Some(access)
}
