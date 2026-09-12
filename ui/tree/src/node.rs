//! The node itself: what it is, how it looks, and where it ended up.

use std::borrow::Cow;

use crisol_display_list::{Color, Corners, Edges, Edges4, Rect};

use crate::atom::Atom;

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
    /// An element that lays out and paints itself (DECISIONS D-06).
    ///
    /// Still an element: it has a tag, classes and attributes, it matches selectors, and the
    /// cascade styles it. `<canvas>` is the model — an element with a painter attached, not
    /// a thing outside the document. Without that, `overflow: hidden` or a `width` could not
    /// be written against a PDF page, which ROADMAP §2.6 requires.
    Custom(CustomElement),
}

/// An element with a painter attached.
#[derive(Debug)]
pub struct CustomElement {
    /// Everything a selector can ask about it.
    pub data: ElementData,
    /// The painter.
    pub node: Box<dyn CustomNode>,
}

impl NodeKind {
    /// The tag name, for elements.
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        self.element().map(|data| data.tag.as_str())
    }

    /// The element data, for elements — including custom ones.
    #[must_use]
    pub fn element(&self) -> Option<&ElementData> {
        match self {
            Self::Element(data) => Some(data),
            Self::Custom(custom) => Some(&custom.data),
            Self::Text(_) => None,
        }
    }

    /// The element data, mutably.
    #[must_use]
    pub fn element_mut(&mut self) -> Option<&mut ElementData> {
        match self {
            Self::Element(data) => Some(data),
            Self::Custom(custom) => Some(&mut custom.data),
            Self::Text(_) => None,
        }
    }

    /// The painter, for a custom element.
    #[must_use]
    pub fn custom(&self) -> Option<&dyn CustomNode> {
        match self {
            Self::Custom(custom) => Some(&*custom.node),
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

/// Element-specific data: everything a selector can ask about a node.
///
/// `id` and `classes` are separate fields rather than entries in `attributes` because the
/// matcher asks about them far more often than about anything else, and a linear scan of an
/// attribute list per candidate is the difference between matching a stylesheet in
/// microseconds and in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ElementData {
    /// Lowercase tag name.
    pub tag: Atom,
    /// The `id` attribute, if present.
    pub id: Option<Atom>,
    /// The `class` attribute, split on whitespace.
    ///
    /// A `Vec` rather than a set: real elements carry two or three classes, and a linear
    /// scan over three entries beats hashing every one of them.
    pub classes: Vec<Atom>,
    /// Everything else, in document order.
    pub attributes: Vec<Attribute>,
    /// Interaction state the matcher reads for `:hover`, `:focus` and friends.
    ///
    /// Nothing sets these before M5, when there is an event loop to set them from. They
    /// exist now so the selector matcher is complete rather than quietly answering `false`
    /// to half the pseudo-classes it is asked about.
    pub state: ElementState,
}

impl ElementData {
    /// An element with the given tag name and nothing else.
    #[must_use]
    pub fn new(tag: impl Into<Atom>) -> Self {
        Self {
            tag: tag.into(),
            ..Self::default()
        }
    }

    /// The value of an attribute, by name.
    ///
    /// Answers for `id` and `class` too, so a caller that does not care where they are
    /// stored does not have to know. `class` is the only one that can allocate, and only
    /// when an element has two or more classes and someone asks for the joined string —
    /// which is `[class="a b"]`, a rare selector. Presence tests go through
    /// [`Self::has_attribute`] and never allocate.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<Cow<'_, str>> {
        match name {
            "id" => return self.id.as_ref().map(|id| Cow::Borrowed(id.as_str())),
            "class" => {
                return match self.classes.as_slice() {
                    [] => None,
                    [single] => Some(Cow::Borrowed(single.as_str())),
                    many => Some(Cow::Owned(
                        many.iter().map(Atom::as_str).collect::<Vec<_>>().join(" "),
                    )),
                };
            }
            _ => {}
        }
        self.attributes
            .iter()
            .find(|attribute| attribute.name == *name)
            .map(|attribute| Cow::Borrowed(attribute.value.as_str()))
    }

    /// Whether an attribute is present, without reading its value.
    ///
    /// Separate from [`Self::attribute`] because `[class]` asks only about presence, and an
    /// element with several classes should not have to build a string to answer it.
    #[must_use]
    pub fn has_attribute(&self, name: &str) -> bool {
        match name {
            "id" => self.id.is_some(),
            "class" => !self.classes.is_empty(),
            _ => self
                .attributes
                .iter()
                .any(|attribute| attribute.name == *name),
        }
    }

    /// Sets `class`, splitting on ASCII whitespace as HTML does.
    pub fn set_class(&mut self, value: &str) {
        self.classes.clear();
        self.classes
            .extend(value.split_ascii_whitespace().map(Atom::new));
    }

    /// True when `name` is one of this element's classes.
    #[must_use]
    pub fn has_class(&self, name: &str, case_sensitive: bool) -> bool {
        self.classes.iter().any(|class| {
            if case_sensitive {
                class.as_str() == name
            } else {
                class.as_str().eq_ignore_ascii_case(name)
            }
        })
    }
}

/// A name/value pair on an element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    /// Lowercase attribute name.
    pub name: Atom,
    /// The value as written.
    pub value: Atom,
}

impl Attribute {
    /// A new attribute, lowercasing the name as HTML does.
    #[must_use]
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: Atom::lowercase(name),
            value: Atom::new(value),
        }
    }
}

bitflags::bitflags! {
    /// Interaction state a selector can ask about.
    ///
    /// One bitflag rather than separate booleans because M6 has to answer "did anything a
    /// selector cares about change?" in one comparison, and because the set is small and
    /// closed — see `crisol-css`'s pseudo-class list, which this mirrors.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct ElementState: u16 {
        /// The pointer is over the element.
        const HOVER = 1 << 0;
        /// A pointer is held down on the element.
        const ACTIVE = 1 << 1;
        /// The element has keyboard focus.
        const FOCUS = 1 << 2;
        /// The element, or something inside it, has keyboard focus.
        const FOCUS_WITHIN = 1 << 3;
        /// Focus arrived by a route that should show a focus ring.
        const FOCUS_VISIBLE = 1 << 4;
        /// The element does not accept input.
        const DISABLED = 1 << 5;
        /// A checkbox or radio is checked.
        const CHECKED = 1 << 6;
        /// A control's value fails its constraints.
        const INVALID = 1 << 7;
    }
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
    /// Border colour, per edge.
    pub border_color: Edges4<Color>,
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
            border_color: Edges4::all(Color::TRANSPARENT),
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
