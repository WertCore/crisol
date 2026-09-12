//! Deciding which declarations win, and applying them.

use std::sync::Arc;

use crisol_css::matching::MatchCaches;
use crisol_css::stylesheet::{Origin, Stylesheet};
use crisol_css::{ElementRef, SelectorList};
use crisol_tree::{DirtyFlags, NodeId, NodeMap, Tree};

use crate::apply::apply;
use crate::computed::ComputedStyle;
use crate::intern::StyleInterner;

/// A declaration's position in the cascade, most significant field first.
///
/// Derived `Ord` does the work: comparing two of these compares `important`, then `origin`,
/// then `specificity`, then `source_order`, in that order — which is the cascade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Precedence {
    /// `!important` beats everything below it.
    important: bool,
    /// Author beats user agent.
    origin: Origin,
    /// The selector's specificity, as `selectors` computes it.
    specificity: u32,
    /// Position in the stylesheet. The last tiebreak, and the reason two equally specific
    /// rules resolve to the one written later.
    source_order: u32,
}

/// Per-restyle counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StyleStats {
    /// Elements that were styled.
    pub elements: usize,
    /// Rules tested against an element, summed over all elements.
    pub rules_tested: u64,
    /// Rules that matched.
    pub rules_matched: u64,
    /// Styles that reused an existing allocation.
    pub interner_hits: u64,
    /// Styles that allocated.
    pub interner_misses: u64,
    /// Elements whose style was reused from the previous pass without being recomputed.
    ///
    /// The number M6's acceptance is about: after one text edit in a large document, this
    /// should be nearly every element.
    pub reused: usize,
}

/// Holds the stylesheets and computes styles for a tree.
#[derive(Debug, Default)]
pub struct StyleEngine {
    stylesheets: Vec<Stylesheet>,
    interner: StyleInterner,
}

/// The computed style of every element in a tree.
///
/// A side table rather than a field on the node, so that `crisol-tree` does not have to know
/// what a computed style is. Text and custom nodes have no entry: they inherit from their
/// parent element, and layout reads the parent's.
pub type StyleMap = NodeMap<Arc<ComputedStyle>>;

impl StyleEngine {
    /// An engine with the user-agent stylesheet loaded.
    ///
    /// The default rather than an opt-in because forgetting it produces a layout that is
    /// subtly wrong — a root box shorter than the window — rather than one that obviously
    /// fails. See [`crate::user_agent`].
    ///
    /// # Panics
    ///
    /// Cannot panic: the user-agent stylesheet is a constant and is covered by a test.
    #[must_use]
    pub fn new() -> Self {
        let mut engine = Self::default();
        engine.add_stylesheet(
            Stylesheet::parse_with_origin(crate::user_agent::STYLESHEET, Origin::UserAgent)
                .expect("the user-agent stylesheet must parse"),
        );
        engine
    }

    /// An engine with nothing loaded at all, for tests that want to see raw behaviour.
    #[must_use]
    pub fn without_user_agent_styles() -> Self {
        Self::default()
    }

    /// Adds a stylesheet. Later sheets of the same origin win ties.
    pub fn add_stylesheet(&mut self, stylesheet: Stylesheet) {
        self.stylesheets.push(stylesheet);
    }

    /// The interner, for inspecting sharing.
    #[must_use]
    pub fn interner(&self) -> &StyleInterner {
        &self.interner
    }

    /// Computes the style of every element in `tree`, from scratch.
    ///
    /// Walks in document order so that a node's parent is always styled before it, which is
    /// what inheritance requires. Returns the styles and the pass's counters.
    ///
    /// Use [`Self::restyle_incremental`] after the first pass: it reuses the styles of
    /// subtrees nothing has invalidated, which is most of a document after most edits.
    pub fn restyle(&mut self, tree: &Tree) -> (StyleMap, StyleStats) {
        let mut styles = StyleMap::with_capacity(tree.len());
        let mut stats = StyleStats::default();
        let Some(root) = tree.root() else {
            return (styles, stats);
        };

        let mut caches = MatchCaches::new();
        let initial = Arc::new(ComputedStyle::default());
        // (node, style its parent computed to)
        let mut stack = vec![(root, Arc::clone(&initial))];

        while let Some((id, parent_style)) = stack.pop() {
            let inherited = if tree.element(id).is_some() {
                let computed = self.compute(tree, id, &parent_style, &mut caches, &mut stats);
                styles.insert(id, Arc::clone(&computed));
                stats.elements += 1;
                computed
            } else {
                // A text node has no declarations of its own, but it very much has a
                // computed style: the font, size and line height it inherited are exactly
                // what the shaper needs. Storing the parent's style under it lets layout ask
                // one question — "what is this node's style?" — regardless of node kind.
                styles.insert(id, Arc::clone(&parent_style));
                parent_style
            };

            let mut child = tree.get(id).and_then(crisol_tree::Node::last_child);
            while let Some(node) = child {
                stack.push((node, Arc::clone(&inherited)));
                child = tree.get(node).and_then(crisol_tree::Node::prev_sibling);
            }
        }

        stats.interner_hits = self.interner.hits();
        stats.interner_misses = self.interner.misses();
        (styles, stats)
    }

    /// Recomputes only what `DirtyFlags::STYLE` says has changed, reusing the rest.
    ///
    /// `previous` is the map the last pass returned. A subtree with no style bit set anywhere
    /// in it keeps the styles it already had, which after a text edit in a large document is
    /// essentially all of it.
    ///
    /// **Inheritance is what makes this subtle.** A subtree can be clean itself and still
    /// need recomputing, because its parent's style changed and half the properties inherit.
    /// So the walk carries whether the inherited style actually changed, and only stops
    /// descending when *both* the subtree is clean and its inherited style is the same
    /// allocation as last time — which the interner makes a pointer comparison (D-21).
    ///
    /// Clears the dirty flags it consumed. Nodes it skipped had none to clear.
    pub fn restyle_incremental(
        &mut self,
        tree: &mut Tree,
        previous: &StyleMap,
    ) -> (StyleMap, StyleStats) {
        let mut styles = StyleMap::with_capacity(tree.len());
        let mut stats = StyleStats::default();
        let Some(root) = tree.root() else {
            return (styles, stats);
        };

        let mut caches = MatchCaches::new();
        let initial = Arc::new(ComputedStyle::default());
        // (node, style its parent computed to, whether that differed from last time)
        //
        // The root inherits from the initial style, which is a constant — so nothing came
        // down from above unless this is the first pass and there is nothing to reuse.
        let root_inherited_changed = previous.get(root).is_none();
        let mut stack = vec![(root, Arc::clone(&initial), root_inherited_changed)];

        while let Some((id, parent_style, inherited_changed)) = stack.pop() {
            let dirty_here = tree.dirty(id).contains(DirtyFlags::STYLE);
            let subtree_dirty = tree.subtree_needs(id, DirtyFlags::STYLE);

            // Nothing in here changed and nothing came down from above: keep what we had.
            // `copy_subtree` fails when some node in it has no previous style, which means
            // the subtree is new and has to be computed rather than copied.
            if !inherited_changed && !subtree_dirty && copy_subtree(tree, id, previous, &mut styles)
            {
                stats.reused += count_elements(tree, id);
                continue;
            }

            let computed = if tree.element(id).is_some() {
                if dirty_here || inherited_changed || previous.get(id).is_none() {
                    let style = self.compute(tree, id, &parent_style, &mut caches, &mut stats);
                    stats.elements += 1;
                    style
                } else {
                    stats.reused += 1;
                    Arc::clone(previous.get(id).expect("checked above"))
                }
            } else {
                Arc::clone(&parent_style)
            };

            let changed = previous
                .get(id)
                .is_none_or(|old| !Arc::ptr_eq(old, &computed));
            styles.insert(id, Arc::clone(&computed));

            // A style *may* have changed is not the same as it *did*. Marking layout here,
            // where the interner makes the question a pointer comparison (D-21), is what
            // stops a structural insert from relaying out every sibling: adding one row to
            // a list marks all of them for restyle, because `:nth-child` could have moved,
            // but almost none of them actually compute to a different style.
            if changed {
                tree.mark_dirty(id, DirtyFlags::LAYOUT);
            }

            let mut child = tree.get(id).and_then(crisol_tree::Node::last_child);
            while let Some(node) = child {
                stack.push((node, Arc::clone(&computed), changed));
                child = tree.get(node).and_then(crisol_tree::Node::prev_sibling);
            }
        }

        clear_style_flags(tree);
        stats.interner_hits = self.interner.hits();
        stats.interner_misses = self.interner.misses();
        (styles, stats)
    }

    fn compute(
        &mut self,
        tree: &Tree,
        id: NodeId,
        parent: &ComputedStyle,
        caches: &mut MatchCaches,
        stats: &mut StyleStats,
    ) -> Arc<ComputedStyle> {
        let mut style = ComputedStyle::inherited_from(parent);

        let Some(element) = ElementRef::new(tree, id) else {
            return self.interner.intern(style);
        };

        // Collect every winning declaration with its precedence, then sort. Sorting
        // declarations rather than rules is what makes `margin: 0` in a later rule lose to
        // `margin-top: 4px` in an earlier, more specific one.
        let mut declarations = Vec::new();
        for sheet in &self.stylesheets {
            for rule in &sheet.rules {
                stats.rules_tested += 1;
                let Some(specificity) = matching_specificity(&rule.selectors, element, caches)
                else {
                    continue;
                };
                stats.rules_matched += 1;
                for (property, important) in &rule.declarations {
                    declarations.push((
                        Precedence {
                            important: *important,
                            origin: sheet.origin,
                            specificity,
                            source_order: rule.source_order,
                        },
                        property,
                    ));
                }
            }
        }

        // A stable sort, so that two declarations with identical precedence — the same
        // property twice in one rule — keep their source order and the later one wins.
        declarations.sort_by_key(|(precedence, _)| *precedence);

        for (_, property) in declarations {
            apply(&mut style, property, parent);
        }

        self.interner.intern(style)
    }
}

/// Copies a whole subtree's styles across from the previous pass.
///
/// Returns `false` when any node in it had no previous style, which means the subtree is new
/// and has to be computed rather than copied.
fn copy_subtree(tree: &Tree, root: NodeId, previous: &StyleMap, out: &mut StyleMap) -> bool {
    let mut stack = vec![root];
    let mut copied = Vec::new();
    while let Some(id) = stack.pop() {
        let Some(style) = previous.get(id) else {
            return false;
        };
        copied.push((id, Arc::clone(style)));
        let mut child = tree.first_child(id);
        while let Some(node) = child {
            stack.push(node);
            child = tree.next_sibling(node);
        }
    }
    for (id, style) in copied {
        out.insert(id, style);
    }
    true
}

/// How many elements a subtree contains, for the reuse counter.
fn count_elements(tree: &Tree, root: NodeId) -> usize {
    let mut count = 0;
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if tree.element(id).is_some() {
            count += 1;
        }
        let mut child = tree.first_child(id);
        while let Some(node) = child {
            stack.push(node);
            child = tree.next_sibling(node);
        }
    }
    count
}

/// Clears every style bit, leaving layout and paint alone.
///
/// The style pass consumed them; layout and paint have their own to consume.
fn clear_style_flags(tree: &mut Tree) {
    let Some(root) = tree.root() else {
        return;
    };
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        tree.clear_dirty(id, DirtyFlags::STYLE | DirtyFlags::SUBTREE_STYLE);
        let mut child = tree.first_child(id);
        while let Some(node) = child {
            stack.push(node);
            child = tree.next_sibling(node);
        }
    }
}

/// The specificity of the most specific selector in `list` that matches, or `None`.
///
/// CSS says a rule's weight is that of the selector that matched, and with a list like
/// `h1, .title` those differ. Taking the maximum is what makes `.title` win over `h1` when
/// both are in the same rule.
fn matching_specificity(
    list: &SelectorList,
    element: ElementRef<'_>,
    caches: &mut MatchCaches,
) -> Option<u32> {
    let mut best: Option<u32> = None;
    for selector in list.slice() {
        if caches.matches(selector, element) {
            let specificity = selector.specificity();
            best = Some(best.map_or(specificity, |current: u32| current.max(specificity)));
        }
    }
    best
}
