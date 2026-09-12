//! The adapter that lets the `selectors` matcher walk a Crisol tree.
//!
//! `selectors` is written against a trait rather than a concrete DOM, so this file is the
//! whole of the integration. Most of it is answering "no" to questions a browser would care
//! about and this engine does not have — shadow roots, slots, parts, custom states
//! (ROADMAP §1).

use std::fmt;

use crisol_tree::{ElementData, ElementState, NodeId, Tree};
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::MatchingContext;
use selectors::{Element, OpaqueElement};

use crate::ident::CssIdent;
use crate::selector::{CrisolPseudoClass, CrisolPseudoElement, CrisolSelectors};

/// A node in a tree, viewed as something the selector matcher can query.
///
/// `Copy` and two words wide, because the matcher clones it constantly as it walks
/// ancestors and siblings.
#[derive(Clone, Copy)]
pub struct ElementRef<'a> {
    tree: &'a Tree,
    id: NodeId,
}

impl<'a> ElementRef<'a> {
    /// Views `id` as an element, or returns `None` when it is a text or custom node.
    ///
    /// Returning `None` rather than a ref that answers nothing is deliberate: selector
    /// matching is defined over elements, and a text node silently failing every selector
    /// is indistinguishable from a stylesheet bug.
    #[must_use]
    pub fn new(tree: &'a Tree, id: NodeId) -> Option<Self> {
        tree.element(id)?;
        Some(Self { tree, id })
    }

    /// The node this refers to.
    #[must_use]
    pub fn id(self) -> NodeId {
        self.id
    }

    /// The tree this refers into.
    #[must_use]
    pub fn tree(self) -> &'a Tree {
        self.tree
    }

    /// The element data.
    ///
    /// # Panics
    ///
    /// Cannot panic: [`Self::new`] checked, and a `NodeId` cannot change kind.
    #[must_use]
    pub fn data(self) -> &'a ElementData {
        self.tree
            .element(self.id)
            .expect("ElementRef was constructed over an element")
    }

    fn wrap(self, id: Option<NodeId>) -> Option<Self> {
        Self::new(self.tree, id?)
    }
}

impl fmt::Debug for ElementRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let data = self.data();
        write!(f, "<{}", data.tag)?;
        if let Some(id) = &data.id {
            write!(f, " id={id}")?;
        }
        for class in &data.classes {
            write!(f, " .{class}")?;
        }
        write!(f, "> {}", self.id)
    }
}

impl Element for ElementRef<'_> {
    type Impl = CrisolSelectors;

    fn opaque(&self) -> OpaqueElement {
        // Identity, used by the matcher to detect cycles and to key caches. The arena index
        // is stable for as long as the node is alive, which is longer than any match.
        OpaqueElement::new(&self.tree.node(self.id).kind)
    }

    fn parent_element(&self) -> Option<Self> {
        self.wrap(self.tree.parent_element(self.id))
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.wrap(self.tree.prev_sibling_element(self.id))
    }

    fn next_sibling_element(&self) -> Option<Self> {
        self.wrap(self.tree.next_sibling_element(self.id))
    }

    fn first_element_child(&self) -> Option<Self> {
        self.wrap(self.tree.first_child_element(self.id))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        // There is one document language and it is HTML-shaped. Saying so is what makes
        // `div` match a `<div>` case-insensitively, which is what an author expects.
        true
    }

    fn has_local_name(&self, local_name: &str) -> bool {
        self.data().tag.as_str() == local_name
    }

    fn has_namespace(&self, ns: &str) -> bool {
        // No namespaces (ROADMAP §1), so every element is in the null namespace and only an
        // empty namespace constraint can match.
        ns.is_empty()
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.data().tag == other.data().tag
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&CssIdent>,
        local_name: &CssIdent,
        operation: &AttrSelectorOperation<&CssIdent>,
    ) -> bool {
        match ns {
            NamespaceConstraint::Any => {}
            NamespaceConstraint::Specific(url) if url.is_empty() => {}
            // A namespaced attribute selector cannot match in a document with no
            // namespaces.
            NamespaceConstraint::Specific(_) => return false,
        }
        self.data()
            .attribute(local_name.as_str())
            .is_some_and(|value| operation.eval_str(&value))
    }

    fn has_attr_in_no_namespace(&self, local_name: &CssIdent) -> bool {
        self.data().has_attribute(local_name.as_str())
    }

    fn match_non_ts_pseudo_class(
        &self,
        pseudo: &CrisolPseudoClass,
        _context: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        let state = self.data().state;
        match pseudo {
            CrisolPseudoClass::Hover => state.contains(ElementState::HOVER),
            CrisolPseudoClass::Active => state.contains(ElementState::ACTIVE),
            CrisolPseudoClass::Focus => state.contains(ElementState::FOCUS),
            CrisolPseudoClass::FocusWithin => state.contains(ElementState::FOCUS_WITHIN),
            CrisolPseudoClass::FocusVisible => state.contains(ElementState::FOCUS_VISIBLE),
            CrisolPseudoClass::Disabled => state.contains(ElementState::DISABLED),
            // Not simply the inverse: `:enabled` matches only elements that *could* be
            // disabled, which is why `:not(:disabled)` is not a synonym for it.
            CrisolPseudoClass::Enabled => {
                is_disableable(self.data()) && !state.contains(ElementState::DISABLED)
            }
            CrisolPseudoClass::Checked => state.contains(ElementState::CHECKED),
            CrisolPseudoClass::Invalid => state.contains(ElementState::INVALID),
        }
    }

    fn match_pseudo_element(
        &self,
        pseudo: &CrisolPseudoElement,
        _context: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        match *pseudo {}
    }

    fn apply_selector_flags(&self, _flags: selectors::matching::ElementSelectorFlags) {
        // These tell a browser which elements need re-matching when a sibling changes.
        // M6 owns invalidation and will want them; recording them now, against a tree the
        // matcher only borrows, would mean interior mutability for a consumer that does not
        // exist yet.
    }

    fn is_link(&self) -> bool {
        let data = self.data();
        data.tag.as_str() == "a" && data.has_attribute("href")
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &CssIdent, case_sensitivity: CaseSensitivity) -> bool {
        self.data()
            .id
            .as_ref()
            .is_some_and(|own| case_sensitivity.eq(own.as_str().as_bytes(), id.as_str().as_bytes()))
    }

    fn has_class(&self, name: &CssIdent, case_sensitivity: CaseSensitivity) -> bool {
        self.data().has_class(
            name.as_str(),
            case_sensitivity == CaseSensitivity::CaseSensitive,
        )
    }

    fn has_custom_state(&self, _name: &CssIdent) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssIdent) -> Option<CssIdent> {
        None
    }

    fn is_part(&self, _name: &CssIdent) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        // `:empty` is about content, not about elements: a node with a text child is not
        // empty even though it has no element children.
        let mut child = self.tree.first_child(self.id);
        while let Some(node) = child {
            match &self.tree.node(node).kind {
                crisol_tree::NodeKind::Text(text) if text.is_empty() => {}
                _ => return false,
            }
            child = self.tree.next_sibling(node);
        }
        true
    }

    fn is_root(&self) -> bool {
        self.tree.root() == Some(self.id)
    }

    fn add_element_unique_hashes(&self, filter: &mut BloomFilter) -> bool {
        // Seeds the ancestor bloom filter the matcher uses to reject descendant selectors
        // without walking. Returning `false` would be correct but would turn every
        // `.a .b` into a full ancestor walk per candidate.
        let data = self.data();
        filter.insert_hash(data.tag.cached_hash());
        if let Some(id) = &data.id {
            filter.insert_hash(id.cached_hash());
        }
        for class in &data.classes {
            filter.insert_hash(class.cached_hash());
        }
        true
    }
}

/// Whether `:enabled` and `:disabled` apply to this element at all.
///
/// The list is short because the engine has few native controls. It is a list rather than
/// "anything with a `disabled` attribute" so that `:enabled` does not match every `<div>`
/// in the document.
fn is_disableable(data: &ElementData) -> bool {
    matches!(
        data.tag.as_str(),
        "button" | "input" | "select" | "textarea" | "option" | "optgroup" | "fieldset"
    )
}
