//! The node itself: what it is, how it looks, and where it ended up.

use crisol_display_list::{Color, Corners, Edges, Rect};

use crate::custom::CustomNode;
use crate::dirty::DirtyFlags;
use crate::id::NodeId;

/// What kind of thing a node is.
#[derive(Debug)]
pub enum NodeKind {
    /// An element. Attributes and the full element model arrive with HTML parsing at M4.
    Element(ElementData),
    /// A run of text. Shaping and line breaking arrive at M4; until then the string is
    /// carried but not drawn.
    Text(String),
    /// A node that lays out and paints itself (DECISIONS D-06).
    Custom(Box<dyn CustomNode>),
}

impl NodeKind {
    /// The tag name, for elements.
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Element(data) => Some(&data.tag),
            _ => None,
        }
    }

    /// The text content, for text nodes.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// Element-specific data.
///
/// Deliberately thin at M2. Attributes, id, and class live here from M3, when the cascade
/// needs something to match selectors against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ElementData {
    /// Lowercase tag name.
    pub tag: String,
}

/// Everything paint needs to know about a node's box.
///
/// At M2 this is set directly by whoever builds the tree. From M3 the cascade computes it
/// from CSS and writes it here, so this struct is the contract between style and paint and
/// does not change when the producer does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxStyle {
    /// Background colour, filling the whole border box.
    pub background: Color,
    /// Border colour.
    pub border_color: Color,
    /// Per-edge border widths, drawn inside the border box.
    pub border_width: Edges,
    /// Corner radii.
    pub radii: Corners,
    /// Whether descendants are clipped to this node's border box, as `overflow: hidden`.
    pub clips_children: bool,
    /// Whether this node and its descendants are painted at all, as `visibility: hidden`.
    ///
    /// An invisible node still occupies space and still lays out; it just does not draw.
    /// That is `visibility`, not `display: none`, which removes the box entirely and is a
    /// layout concern rather than a paint one.
    pub visible: bool,
}

impl Default for BoxStyle {
    fn default() -> Self {
        Self {
            background: Color::TRANSPARENT,
            border_color: Color::TRANSPARENT,
            border_width: Edges::ZERO,
            radii: Corners::ZERO,
            clips_children: false,
            visible: true,
        }
    }
}

impl BoxStyle {
    /// A visible box with a solid background and nothing else.
    #[must_use]
    pub fn filled(background: Color) -> Self {
        Self {
            background,
            ..Self::default()
        }
    }
}

/// A node in the tree.
///
/// Children are an intrusive doubly linked list, not a `Vec<NodeId>` on the parent
/// (DECISIONS D-17). Inserting or removing in the middle of a large child list is then O(1)
/// pointer work rather than an O(n) move, which is what M6 needs and what a `Vec` cannot
/// give.
#[derive(Debug)]
pub struct Node {
    pub(crate) parent: Option<NodeId>,
    pub(crate) first_child: Option<NodeId>,
    pub(crate) last_child: Option<NodeId>,
    pub(crate) prev_sibling: Option<NodeId>,
    pub(crate) next_sibling: Option<NodeId>,

    /// What this node is.
    pub kind: NodeKind,
    /// How it is painted.
    pub style: BoxStyle,
    /// The node's border box, with its origin relative to its parent's border box.
    ///
    /// Relative rather than absolute because that is what `taffy` produces at M3 and
    /// because moving a subtree then costs one write instead of one per descendant.
    pub layout: Rect,

    pub(crate) dirty: DirtyFlags,
}

impl Node {
    pub(crate) fn new(kind: NodeKind) -> Self {
        Self {
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            kind,
            style: BoxStyle::default(),
            layout: Rect::ZERO,
            // A brand new node has never been styled, laid out or painted.
            dirty: DirtyFlags::ALL_SELF,
        }
    }

    /// The parent, or `None` for a root or a detached node.
    #[must_use]
    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    /// The first child.
    #[must_use]
    pub fn first_child(&self) -> Option<NodeId> {
        self.first_child
    }

    /// The last child.
    #[must_use]
    pub fn last_child(&self) -> Option<NodeId> {
        self.last_child
    }

    /// The previous sibling.
    #[must_use]
    pub fn prev_sibling(&self) -> Option<NodeId> {
        self.prev_sibling
    }

    /// The next sibling.
    #[must_use]
    pub fn next_sibling(&self) -> Option<NodeId> {
        self.next_sibling
    }

    /// This node's pending invalidation.
    #[must_use]
    pub fn dirty(&self) -> DirtyFlags {
        self.dirty
    }
}
