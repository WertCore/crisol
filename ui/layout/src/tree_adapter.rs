//! Driving `taffy` over a Crisol tree.
//!
//! `taffy` 0.14 can lay out a tree it does not own, through a set of traits. That is what
//! this file implements, so there is no second tree to keep in sync — the thing layout
//! measures is the same arena HTML built, the cascade styled, and paint will walk.
//!
//! The custom-node protocol lands here: a node that opts out of CSS layout (DECISIONS D-06,
//! D-19) is a leaf as far as taffy is concerned, and its measure function is its own
//! [`CustomNode::measure`](crisol_tree::CustomNode::measure).

use crisol_display_list::{Rect, Size as CrisolSize};
use crisol_style::values::Display;
use crisol_style::{ComputedStyle, StyleMap};
use crisol_text::{FontSystem, TextLayout, Wrapping};
use crisol_tree::{DirtyFlags, MeasureConstraints, NodeId, NodeKind, NodeMap, Tree};
use taffy::geometry::Size;
use taffy::style::AvailableSpace;
use taffy::tree::{Cache, Layout, LayoutInput, LayoutOutput, NodeId as TaffyId, RunMode};
use taffy::{
    CacheTree, LayoutBlockContainer, LayoutFlexboxContainer, LayoutPartialTree, RoundTree,
    TraversePartialTree, TraverseTree,
};

use crate::style_adapter::StyleRef;

/// Per-node scratch that only layout needs. Kept in a side table so `crisol-tree` never has
/// to know what `taffy` is.
#[derive(Debug, Default)]
struct NodeLayout {
    cache: Cache,
    unrounded: Layout,
    final_layout: Layout,
}

/// Counters for one layout pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutStats {
    /// Nodes taffy computed a layout for, cache misses only.
    pub nodes_laid_out: usize,
    /// Custom nodes whose `measure` was called.
    pub custom_measures: usize,
    /// Text nodes that were shaped.
    pub text_shaped: usize,
    /// Nodes whose box was written back to the tree.
    pub nodes_written: usize,
    /// Nodes whose cached measurement was thrown away because something changed.
    ///
    /// The number M6's acceptance is about: after one text edit in a large document this
    /// should be the edited node and its ancestors, and nothing else.
    pub caches_invalidated: usize,
    /// Nodes whose box came out the same as last time.
    pub unchanged: usize,
}

/// The shaped text of every text node in a tree.
///
/// A side table for the same reason computed style is one (DECISIONS D-21): `crisol-tree`
/// has no business knowing what a shaped glyph is. Paint reads it rather than reshaping,
/// which is the whole point of keeping it.
pub type TextMap = NodeMap<TextLayout>;

/// Everything layout remembers between passes.
///
/// Owned by the caller rather than by [`LayoutContext`], because a frame loop has to lay
/// out, *mutate the tree*, and lay out again — and a context holding `&mut Tree` makes the
/// middle step impossible. Keeping the caches out here is what lets them survive an edit,
/// which is the entire premise of M6.
#[derive(Debug, Default)]
pub struct LayoutCache {
    nodes: NodeMap<NodeLayout>,
    text: TextMap,
}

impl LayoutCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The shaped text from the last pass, which paint reads.
    #[must_use]
    pub fn text(&self) -> &TextMap {
        &self.text
    }

    /// Takes the shaped text out, leaving the cache without it.
    #[must_use]
    pub fn take_text(&mut self) -> TextMap {
        std::mem::take(&mut self.text)
    }

    /// Throws everything away, forcing the next pass to be a full one.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.text.clear();
    }
}

/// Borrows a tree, its styles and a cache for one layout pass.
pub struct LayoutContext<'a> {
    tree: &'a mut Tree,
    styles: &'a StyleMap,
    fonts: &'a mut FontSystem,
    cache: &'a mut LayoutCache,
    /// The union of every box that moved or resized this pass, in logical pixels.
    damage: Option<Rect>,
    /// Text and custom nodes have no style of their own and lay out with initial values.
    fallback: ComputedStyle,
    stats: LayoutStats,
}

impl std::fmt::Debug for LayoutContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutContext")
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// Looks up a style without borrowing the whole context, so the tree stays free for the
/// mutable borrow a custom node's `measure` needs.
fn style_of<'s>(
    styles: &'s StyleMap,
    fallback: &'s ComputedStyle,
    id: NodeId,
) -> &'s ComputedStyle {
    styles.get(id).map_or(fallback, |style| &**style)
}

/// Whether this node measures itself rather than being sized by its container.
///
/// Text as well as custom nodes. Without inline layout, a text node is the closest thing the
/// engine has to a replaced element: its box should be the extent of its glyphs, so that
/// `align-items: center` centres the text rather than a full-width box containing it. The
/// container's width still reaches the shaper — as the available space to wrap within —
/// which is the part that matters.
fn is_replaced(tree: &Tree, id: NodeId) -> bool {
    matches!(tree.node(id).kind, NodeKind::Custom(_) | NodeKind::Text(_))
}

/// The taffy view of a node's style, including the replaced bit the cascade cannot know.
fn style_ref<'s>(
    tree: &Tree,
    styles: &'s StyleMap,
    fallback: &'s ComputedStyle,
    id: NodeId,
) -> StyleRef<'s> {
    let style = style_of(styles, fallback, id);
    if is_replaced(tree, id) {
        StyleRef::replaced(style)
    } else {
        StyleRef::new(style)
    }
}

fn crisol_id(node: TaffyId) -> NodeId {
    NodeId::from_bits(u64::from(node)).expect("taffy only sees ids this adapter gave it")
}

fn taffy_id(id: NodeId) -> TaffyId {
    TaffyId::from(id.to_bits())
}

fn definite(space: AvailableSpace) -> Option<f32> {
    match space {
        AvailableSpace::Definite(value) => Some(value),
        AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
    }
}

/// Collects every node whose layout is dirty, plus every ancestor of one.
///
/// Returns them deepest-last, which does not matter for clearing a cache but makes the list
/// readable when a test prints it.
fn collect_dirty(tree: &Tree, root: NodeId, out: &mut Vec<NodeId>) -> bool {
    let Some(node) = tree.get(root) else {
        return false;
    };
    // Nothing in this subtree wants layout: skip it whole, which is the entire point of the
    // subtree bits (DECISIONS D-17).
    if !node
        .dirty()
        .intersects(DirtyFlags::LAYOUT | DirtyFlags::SUBTREE_LAYOUT)
    {
        return false;
    }

    let mut any = node.dirty().contains(DirtyFlags::LAYOUT);
    let mut child = tree.first_child(root);
    while let Some(current) = child {
        any |= collect_dirty(tree, current, out);
        child = tree.next_sibling(current);
    }
    if any {
        out.push(root);
    }
    any
}

/// Clears the layout bits the pass just consumed.
fn clear_layout_flags(tree: &mut Tree, root: NodeId) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        tree.clear_dirty(id, DirtyFlags::LAYOUT | DirtyFlags::SUBTREE_LAYOUT);
        let mut child = tree.first_child(id);
        while let Some(node) = child {
            stack.push(node);
            child = tree.next_sibling(node);
        }
    }
}

/// Whether a node lays its own children out, or is a leaf as far as taffy is concerned.
///
/// A custom node is always a leaf: it opted out of CSS layout, and letting taffy lay out
/// children it does not know about would be the opposite of what ROADMAP §2.6 promises.
fn is_leaf(tree: &Tree, id: NodeId) -> bool {
    match &tree.node(id).kind {
        NodeKind::Custom(_) | NodeKind::Text(_) => true,
        NodeKind::Element(_) => tree.first_child(id).is_none(),
    }
}

impl<'a> LayoutContext<'a> {
    /// Prepares a pass over `tree` using `styles`, reusing whatever `cache` remembers.
    #[must_use]
    pub fn new(
        tree: &'a mut Tree,
        styles: &'a StyleMap,
        fonts: &'a mut FontSystem,
        cache: &'a mut LayoutCache,
    ) -> Self {
        Self {
            tree,
            styles,
            fonts,
            cache,
            damage: None,
            fallback: ComputedStyle::default(),
            stats: LayoutStats::default(),
        }
    }

    /// The shaped text produced by this pass.
    #[must_use]
    pub fn text(&self) -> &TextMap {
        &self.cache.text
    }

    /// The rectangle covering everything that moved or resized in the last pass.
    ///
    /// `None` when nothing did, which is the answer a frame with no changes wants: there is
    /// nothing to repaint. The union of the old and new boxes of each changed node, because
    /// a box that shrank leaves behind pixels that have to be painted over.
    #[must_use]
    pub fn damage(&self) -> Option<Rect> {
        self.damage
    }

    /// Throws away the cached measurement of every node whose layout is dirty, and of every
    /// ancestor of one.
    ///
    /// The ancestors matter and are easy to forget: a node's size feeds its parent's, so a
    /// paragraph growing a line changes the height of everything containing it. Stopping at
    /// the dirty node leaves taffy serving a stale size to its parent.
    ///
    /// Everything else keeps its cache, and taffy then does no work for it at all — which is
    /// what makes a one-word edit in a four-hundred-page document cost a handful of nodes
    /// rather than all of them.
    fn invalidate_dirty(&mut self, root: NodeId) -> usize {
        let mut invalidated = Vec::new();
        collect_dirty(self.tree, root, &mut invalidated);
        let count = invalidated.len();
        for node in invalidated {
            self.entry(node).cache.clear();
        }
        count
    }

    /// This context's counters, accumulated across passes.
    #[must_use]
    pub fn stats(&self) -> LayoutStats {
        self.stats
    }

    /// The final layout computed for a node, in its parent's coordinate space.
    #[must_use]
    pub fn layout_of(&self, id: NodeId) -> Option<Layout> {
        self.cache.nodes.get(id).map(|node| node.final_layout)
    }

    /// Lays out the tree's root into `available`, then writes the results back.
    ///
    /// Returns `false` when the tree has no root. `available` is the viewport in logical
    /// pixels.
    pub fn run(&mut self, available: CrisolSize) -> bool {
        let Some(root) = self.tree.root() else {
            return false;
        };

        self.damage = None;
        self.stats.caches_invalidated += self.invalidate_dirty(root);

        taffy::compute_root_layout(
            self,
            taffy_id(root),
            Size {
                width: AvailableSpace::Definite(available.width),
                height: AvailableSpace::Definite(available.height),
            },
        );
        // Snap boxes to whole pixels. Without it, a column of 1/3-height rows accumulates
        // fractional offsets and adjacent borders land on different physical pixels.
        taffy::round_layout(self, taffy_id(root));
        self.write_back(root);
        true
    }

    /// Copies taffy's boxes onto the nodes, and projects computed style onto `BoxStyle`.
    ///
    /// Both happen here rather than in two passes because the projection needs the box:
    /// a percentage `border-radius` is only a number of pixels once layout has decided the
    /// border box.
    fn write_back(&mut self, root: NodeId) {
        // (node, absolute origin of its parent). Boxes are stored relative to the parent,
        // which is what makes moving a subtree one write — but damage has to be absolute,
        // because the renderer scissors with it.
        let mut stack = vec![(root, crisol_display_list::Point::ZERO)];
        while let Some((id, parent_origin)) = stack.pop() {
            let Some(layout) = self.cache.nodes.get(id).map(|node| node.final_layout) else {
                continue;
            };
            let rect = Rect::from_xywh(
                layout.location.x,
                layout.location.y,
                layout.size.width,
                layout.size.height,
            );
            let absolute = rect.translate(parent_origin);
            let box_style = style_of(self.styles, &self.fallback, id).to_box_style(rect.size);

            if let Some(node) = self.tree.get_mut(id) {
                if node.layout == rect && node.style == box_style {
                    self.stats.unchanged += 1;
                } else {
                    // Both boxes are damaged: the new one has to be drawn, and the old one
                    // has to be painted over, which a box that shrank or moved leaves behind.
                    // In absolute coordinates, because that is the space the renderer clips
                    // in — unioning parent-relative rectangles produces a region that means
                    // nothing.
                    let was = node.layout.translate(parent_origin);
                    node.layout = rect;
                    node.style = box_style;
                    self.damage = Some(match self.damage {
                        Some(current) => current.union(was).union(absolute),
                        None => was.union(absolute),
                    });
                }
            }
            self.stats.nodes_written += 1;

            // How far this node's content can scroll, which taffy derives from the
            // scrollable overflow rectangle. Clamps the existing offset if the content
            // shrank beneath it.
            self.tree.set_scroll_max(
                id,
                CrisolSize {
                    width: layout.scroll_width(),
                    height: layout.scroll_height(),
                },
            );

            // A custom node gets told the box it was given, so it can lay out its interior
            // now that the size is settled (DECISIONS D-19).
            self.tree.layout_custom(id, rect.size);

            // Children descend from where they are *painted*, not from where they were laid
            // out. The two differ inside a scroll container, and the origin threaded here is
            // what damage rectangles are built from — so ignoring the scroll would damage
            // the place a box would have been if nobody had scrolled.
            let child_origin = absolute.origin - self.tree.scroll_offset(id);
            let mut child = self.tree.first_child(id);
            while let Some(node) = child {
                stack.push((node, child_origin));
                child = self.tree.next_sibling(node);
            }
        }

        // Layout consumed them; paint has its own.
        clear_layout_flags(self.tree, root);
    }

    fn entry(&mut self, id: NodeId) -> &mut NodeLayout {
        if self.cache.nodes.get(id).is_none() {
            self.cache.nodes.insert(id, NodeLayout::default());
        }
        self.cache
            .nodes
            .get_mut(id)
            .expect("inserted above when it was missing")
    }
}

/// Iterates a node's children as taffy ids.
pub struct ChildIter<'a> {
    tree: &'a Tree,
    next: Option<NodeId>,
}

impl Iterator for ChildIter<'_> {
    type Item = TaffyId;

    fn next(&mut self) -> Option<TaffyId> {
        let current = self.next?;
        self.next = self.tree.next_sibling(current);
        Some(taffy_id(current))
    }
}

impl TraversePartialTree for LayoutContext<'_> {
    type ChildIter<'b>
        = ChildIter<'b>
    where
        Self: 'b;

    fn child_ids(&self, parent: TaffyId) -> Self::ChildIter<'_> {
        let id = crisol_id(parent);
        ChildIter {
            tree: self.tree,
            next: if is_leaf(self.tree, id) {
                None
            } else {
                self.tree.first_child(id)
            },
        }
    }

    fn child_count(&self, parent: TaffyId) -> usize {
        self.child_ids(parent).count()
    }

    fn get_child_id(&self, parent: TaffyId, index: usize) -> TaffyId {
        self.child_ids(parent)
            .nth(index)
            .expect("taffy asked for a child index it got from child_count")
    }
}

impl TraverseTree for LayoutContext<'_> {}

impl LayoutPartialTree for LayoutContext<'_> {
    type CoreContainerStyle<'b>
        = StyleRef<'b>
    where
        Self: 'b;
    type CustomIdent = String;

    fn get_core_container_style(&self, node: TaffyId) -> Self::CoreContainerStyle<'_> {
        style_ref(self.tree, self.styles, &self.fallback, crisol_id(node))
    }

    fn set_unrounded_layout(&mut self, node: TaffyId, layout: &Layout) {
        let id = crisol_id(node);
        self.entry(id).unrounded = *layout;
    }

    fn compute_child_layout(&mut self, node: TaffyId, inputs: LayoutInput) -> LayoutOutput {
        // An ancestor is `display: none`, so this node is hidden whatever its own display
        // says. The check has to come before the cache: a hidden pass must not be served
        // from, or stored into, the cache of a visible one.
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return taffy::compute_hidden_layout(self, node);
        }

        taffy::compute_cached_layout(self, node, inputs, |context, node, inputs| {
            let id = crisol_id(node);
            context.stats.nodes_laid_out += 1;

            let display = style_of(context.styles, &context.fallback, id).display;
            if display == Display::None {
                return taffy::compute_hidden_layout(context, node);
            }

            if is_leaf(context.tree, id) {
                // Split the borrows by field: the style comes from `styles`/`fallback`,
                // while the measure closure needs `tree` and `stats` mutably. Going through
                // `self.method()` would borrow all of `context` and make this impossible.
                let LayoutContext {
                    tree,
                    styles,
                    fonts,
                    cache,
                    fallback,
                    stats,
                    ..
                } = context;
                let text = &mut cache.text;
                let computed = style_of(styles, fallback, id);
                let style = if is_replaced(tree, id) {
                    StyleRef::replaced(computed)
                } else {
                    StyleRef::new(computed)
                };
                return taffy::compute_leaf_layout(
                    inputs,
                    &style,
                    |_, _| 0.0,
                    |known, available| {
                        measure_leaf(tree, fonts, text, computed, stats, id, known, available)
                    },
                );
            }

            match display {
                Display::Flex => taffy::compute_flexbox_layout(context, node, inputs),
                // Block is the default, and `Display::None` was handled above.
                _ => taffy::compute_block_layout(context, node, inputs, None),
            }
        })
    }
}

/// Measures a leaf: a custom node measures itself, a text node is shaped, and an empty
/// element is empty.
#[allow(
    clippy::too_many_arguments,
    reason = "the borrows are split by field on purpose"
)]
fn measure_leaf(
    tree: &mut Tree,
    fonts: &mut FontSystem,
    text_map: &mut TextMap,
    style: &ComputedStyle,
    stats: &mut LayoutStats,
    id: NodeId,
    known: Size<Option<f32>>,
    available: Size<AvailableSpace>,
) -> Size<f32> {
    let constraints = MeasureConstraints {
        width: known.width,
        height: known.height,
        available_width: definite(available.width),
        available_height: definite(available.height),
    };
    if let Some(size) = tree.measure_custom(id, constraints) {
        stats.custom_measures += 1;
        return Size {
            width: size.width,
            height: size.height,
        };
    }

    let Some(content) = tree.get(id).and_then(|node| node.kind.text()) else {
        // An empty element.
        return Size::ZERO;
    };
    if content.is_empty() {
        return Size::ZERO;
    }

    // A definite width wins over the available space; an unbounded axis means "as wide as
    // it wants", which is what taffy asks for during intrinsic sizing.
    let width = known.width.or_else(|| definite(available.width));
    let layout = crisol_text::shape(
        fonts,
        content,
        &style.to_text_style(),
        width,
        // Text in a box wraps. A node that must not wrap says so with `white-space`, which
        // is not in M3's property subset yet.
        Wrapping::Word,
    );
    let size = layout.size();
    text_map.insert(id, layout);
    stats.text_shaped += 1;

    Size {
        // Report the glyph extent, not the width that was offered: a short line of text in a
        // wide box is a narrow box, and reporting otherwise makes `align-items: center` put
        // it in the wrong place.
        width: size.width,
        height: size.height,
    }
}

impl CacheTree for LayoutContext<'_> {
    fn cache_get(&mut self, node: TaffyId, input: &LayoutInput) -> Option<LayoutOutput> {
        self.entry(crisol_id(node)).cache.get(input)
    }

    fn cache_store(&mut self, node: TaffyId, input: &LayoutInput, output: LayoutOutput) {
        let id = crisol_id(node);
        self.entry(id).cache.store(input, output);
    }

    fn cache_clear(&mut self, node: TaffyId) {
        let id = crisol_id(node);
        self.entry(id).cache.clear();
    }
}

impl RoundTree for LayoutContext<'_> {
    fn get_unrounded_layout(&self, node: TaffyId) -> Layout {
        self.cache
            .nodes
            .get(crisol_id(node))
            .map_or_else(Layout::new, |node| node.unrounded)
    }

    fn set_final_layout(&mut self, node: TaffyId, layout: &Layout) {
        let id = crisol_id(node);
        self.entry(id).final_layout = *layout;
    }
}

impl LayoutFlexboxContainer for LayoutContext<'_> {
    type FlexboxContainerStyle<'b>
        = StyleRef<'b>
    where
        Self: 'b;
    type FlexboxItemStyle<'b>
        = StyleRef<'b>
    where
        Self: 'b;

    fn get_flexbox_container_style(&self, node: TaffyId) -> Self::FlexboxContainerStyle<'_> {
        style_ref(self.tree, self.styles, &self.fallback, crisol_id(node))
    }

    fn get_flexbox_child_style(&self, child: TaffyId) -> Self::FlexboxItemStyle<'_> {
        style_ref(self.tree, self.styles, &self.fallback, crisol_id(child))
    }
}

impl LayoutBlockContainer for LayoutContext<'_> {
    type BlockContainerStyle<'b>
        = StyleRef<'b>
    where
        Self: 'b;
    type BlockItemStyle<'b>
        = StyleRef<'b>
    where
        Self: 'b;

    fn get_block_container_style(&self, node: TaffyId) -> Self::BlockContainerStyle<'_> {
        style_ref(self.tree, self.styles, &self.fallback, crisol_id(node))
    }

    fn get_block_child_style(&self, child: TaffyId) -> Self::BlockItemStyle<'_> {
        style_ref(self.tree, self.styles, &self.fallback, crisol_id(child))
    }
}
