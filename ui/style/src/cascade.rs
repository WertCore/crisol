//! Deciding which declarations win, and applying them.

use std::sync::Arc;

use crisol_css::matching::MatchCaches;
use crisol_css::stylesheet::{Origin, Stylesheet};
use crisol_css::{ElementRef, SelectorList};
use crisol_tree::{NodeId, NodeMap, Tree};

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

    /// Computes the style of every element in `tree`.
    ///
    /// Walks in document order so that a node's parent is always styled before it, which is
    /// what inheritance requires. Returns the styles and the pass's counters.
    ///
    /// This restyles everything. M6 replaces it with invalidation driven by
    /// `DirtyFlags::STYLE`; the signature is the same either way, which is why the
    /// stats are here now.
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
