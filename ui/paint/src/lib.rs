//! Styled tree to display list.
//!
//! Paint is the boundary between the document and the renderer. Above it everything is
//! nodes, style and layout; below it everything is a flat list of draw commands in
//! painter's order, and nothing knows what a node is (DECISIONS D-13).
//!
//! The walk is iterative. A four-hundred-page document is not allowed to overflow the Rust
//! stack merely by being deep.
//!
//! ```
//! use crisol_display_list::{Color, Rect, Size};
//! use crisol_tree::{BoxStyle, Tree};
//!
//! let mut tree = Tree::new();
//! let root = tree.create_element("div");
//! tree.set_root(root).unwrap();
//! tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 100.0, 100.0);
//! tree.node_mut(root).style = BoxStyle::filled(Color::BLACK);
//!
//! let list = crisol_paint::paint(&tree, &crisol_paint::PaintOptions::new(Size::new(100.0, 100.0)));
//! assert_eq!(list.len(), 1);
//! ```

#![doc(html_root_url = "https://docs.rs/crisol-paint/0.0.0")]

use crisol_display_list::{
    Clip, Color, DisplayList, DisplayListBuilder, Point, Rect, RectCommand, Size, TextCommand,
    TextId,
};
use crisol_tree::{BoxStyle, NodeId, NodeKind, Tree};

/// How a tree is painted.
///
/// A struct rather than arguments because M6 adds a damage rectangle here and device pixel
/// snapping after that, and neither should be a breaking change for callers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintOptions {
    /// Size of the surface being painted, in logical pixels.
    pub viewport: Size,
    /// Colour the surface is cleared to before anything is drawn.
    pub background: Color,
}

impl PaintOptions {
    /// Paints a `viewport`-sized surface on a white background.
    #[must_use]
    pub fn new(viewport: Size) -> Self {
        Self {
            viewport,
            background: Color::WHITE,
        }
    }

    /// The same options with a different clear colour.
    #[must_use]
    pub fn with_background(self, background: Color) -> Self {
        Self { background, ..self }
    }
}

/// Counters for one paint pass.
///
/// Instrumentation rather than diagnostics: M6's acceptance test is a statement about these
/// numbers, so they are part of the interface and not a debugging aid to be removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaintStats {
    /// Nodes the walk descended into.
    pub nodes_visited: usize,
    /// Nodes that emitted a box.
    pub boxes_emitted: usize,
    /// Custom nodes given a chance to paint themselves.
    pub custom_nodes: usize,
    /// Clip groups opened.
    pub clips: usize,
    /// Text blocks referred to.
    pub text_runs: usize,
}

/// Paints the tree's root into a fresh display list.
///
/// An empty tree, or a tree with no root, produces a list that clears and draws nothing.
#[must_use]
pub fn paint(tree: &Tree, options: &PaintOptions) -> DisplayList {
    let mut builder = DisplayListBuilder::new(options.viewport);
    builder.set_background(options.background);
    if let Some(root) = tree.root() {
        paint_subtree(tree, root, Point::ZERO, &mut builder);
    }
    builder.build()
}

/// As [`paint`], but also returns the pass's counters.
#[must_use]
pub fn paint_with_stats(tree: &Tree, options: &PaintOptions) -> (DisplayList, PaintStats) {
    let mut builder = DisplayListBuilder::new(options.viewport);
    builder.set_background(options.background);
    let stats = match tree.root() {
        Some(root) => paint_subtree(tree, root, Point::ZERO, &mut builder),
        None => PaintStats::default(),
    };
    (builder.build(), stats)
}

/// One step of the walk.
///
/// The `PopClip` marker is what lets an explicit stack do what recursion would do on the
/// way back up. Without it the walk would have to remember, for every node it pushed,
/// whether that node had opened a clip.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Paint this node's box and queue its children.
    Enter {
        id: NodeId,
        /// Absolute origin of this node's *parent*, in logical pixels.
        parent_origin: Point,
    },
    /// Close the clip a node opened.
    PopClip,
}

/// Paints the subtree rooted at `root`, positioning it as if its parent's border box
/// started at `origin`.
///
/// Exposed because M6 repaints a damaged subtree without rebuilding the whole list, and
/// because a custom node that hosts engine-laid-out content needs the same entry point.
pub fn paint_subtree(
    tree: &Tree,
    root: NodeId,
    origin: Point,
    builder: &mut DisplayListBuilder,
) -> PaintStats {
    let mut stats = PaintStats::default();
    let mut stack = vec![Step::Enter {
        id: root,
        parent_origin: origin,
    }];

    while let Some(step) = stack.pop() {
        let Step::Enter { id, parent_origin } = step else {
            builder.pop_clip();
            continue;
        };

        let Some(node) = tree.get(id) else {
            // A handle can go stale between layout and paint if something mutated the tree
            // in between. Skipping is the only safe answer; panicking here would turn a
            // logic error somewhere else into a crash in the frame loop.
            continue;
        };
        // `display: none` generates no box: the subtree is not laid out, not painted, not
        // hit tested and not in the tab order. Skipping it here is cheaper than walking it to
        // emit nothing, which is what happened before the flag existed.
        if !node.style.generates_box {
            continue;
        }
        stats.nodes_visited += 1;

        let bounds = node.layout.translate(parent_origin);

        if node.style.visible {
            match &node.kind {
                NodeKind::Custom(custom) => {
                    // The engine, not the node, owns the clip. A custom node that draws
                    // outside its box is clipped rather than trusted (DECISIONS D-19).
                    if emit_box(builder, bounds, &node.style) {
                        stats.boxes_emitted += 1;
                    }
                    // The node is clipped to its own border box, corners included: a rounded
                    // PDF page must not paint into the corners it does not own.
                    builder.push_rounded_clip(Clip::rounded(bounds, node.style.radii));
                    custom.node.paint(bounds, builder);
                    builder.pop_clip();
                    stats.custom_nodes += 1;
                    stats.clips += 1;
                }
                NodeKind::Element(_) => {
                    if emit_box(builder, bounds, &node.style) {
                        stats.boxes_emitted += 1;
                    }
                }
                NodeKind::Text(text) if !text.is_empty() => {
                    // The renderer holds the shaped text; the list refers to it by the same
                    // handle the node has, so paint does not need to know how it was shaped.
                    if builder.push_text(TextCommand {
                        text: TextId(id.to_bits()),
                        origin: bounds.origin,
                        color: node.style.text_color,
                    }) {
                        stats.text_runs += 1;
                    }
                }
                NodeKind::Text(_) => {}
            }
        }

        // `visibility: hidden` hides this node's own box but not its descendants: a child
        // whose computed visibility is `visible` still paints. After M3 the cascade will
        // have resolved inheritance, so a subtree that should be invisible arrives here
        // already marked invisible node by node.
        // `overflow: hidden` clips to the *padding* box in CSS, but the difference only
        // shows under a translucent border; clipping to the border box keeps the common case
        // — a rounded card — exactly right and costs nothing.
        if node.style.clips_children {
            builder.push_rounded_clip(Clip::rounded(bounds, node.style.radii));
            stats.clips += 1;
            stack.push(Step::PopClip);
        }

        // Children are painted over their parent and in document order, so they are pushed
        // last-first onto a stack that pops in reverse.
        let mut child = node.last_child();
        while let Some(current) = child {
            stack.push(Step::Enter {
                id: current,
                parent_origin: bounds.origin,
            });
            child = tree.get(current).and_then(crisol_tree::Node::prev_sibling);
        }
    }

    stats
}

/// Emits a node's background and border. Returns whether the builder kept the command.
fn emit_box(builder: &mut DisplayListBuilder, bounds: Rect, style: &BoxStyle) -> bool {
    builder.push_rect(RectCommand {
        rect: bounds,
        radii: style.radii,
        fill: style.background,
        border_color: style.border_color,
        border_width: style.border_width,
    })
}
