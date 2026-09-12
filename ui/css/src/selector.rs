//! The selector dialect Crisol supports.
//!
//! This is not the CSS selector grammar. ROADMAP §1 says the project is not a browser and
//! owes nothing to the spec, and nowhere is that easier to act on than here: every
//! pseudo-class we accept is one the engine has to be able to answer, on every element, on
//! every restyle. Listing them explicitly means an unsupported one is a parse error with a
//! source location rather than a selector that silently never matches.
//!
//! The list is the state an interface actually has. `:visited` is absent because there is
//! no history (ROADMAP §1). `::before` and `::after` are absent because generated content
//! has no box in the M2 tree; adding them is a tree change, not a parser change.

use std::fmt;

use cssparser::{CowRcStr, SourceLocation, ToCss};
use selectors::parser::{NonTSPseudoClass, PseudoElement, SelectorParseErrorKind};

use crate::ident::CssIdent;

/// The [`selectors::SelectorImpl`] Crisol matches with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrisolSelectors;

impl selectors::SelectorImpl for CrisolSelectors {
    type ExtraMatchingData<'a> = ();
    type AttrValue = CssIdent;
    type Identifier = CssIdent;
    type LocalName = CssIdent;
    type NamespaceUrl = CssIdent;
    type NamespacePrefix = CssIdent;
    type BorrowedNamespaceUrl = str;
    type BorrowedLocalName = str;
    type NonTSPseudoClass = CrisolPseudoClass;
    type PseudoElement = CrisolPseudoElement;
}

/// A pseudo-class that is not a tree-structural one.
///
/// Tree-structural pseudo-classes (`:first-child`, `:nth-child`, `:root`, `:empty`) are
/// handled by the `selectors` crate itself from the tree shape, so they are not listed
/// here — they work without any of this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrisolPseudoClass {
    /// The pointer is over the element.
    Hover,
    /// The element is being activated — a pointer is held down on it.
    Active,
    /// The element has keyboard focus.
    Focus,
    /// The element, or something inside it, has keyboard focus.
    FocusWithin,
    /// The element has keyboard focus and the focus ring should be shown. Distinct from
    /// `:focus` because clicking a button focuses it without wanting a ring.
    FocusVisible,
    /// The element does not accept input.
    Disabled,
    /// The element accepts input. The complement of [`Self::Disabled`], spelled out because
    /// `:not(:disabled)` also matches elements that cannot be disabled at all.
    Enabled,
    /// A checkbox or radio is checked.
    Checked,
    /// A control's value fails its constraints.
    Invalid,
}

impl CrisolPseudoClass {
    /// Looks up a pseudo-class by name. Names are ASCII case-insensitive.
    fn parse(name: &str) -> Option<Self> {
        // Allocates only for a name that is not already lowercase, which no real stylesheet
        // writes.
        let lowered = if name.bytes().any(|b| b.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(name.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(name)
        };
        Some(match &*lowered {
            "hover" => Self::Hover,
            "active" => Self::Active,
            "focus" => Self::Focus,
            "focus-within" => Self::FocusWithin,
            "focus-visible" => Self::FocusVisible,
            "disabled" => Self::Disabled,
            "enabled" => Self::Enabled,
            "checked" => Self::Checked,
            "invalid" => Self::Invalid,
            _ => return None,
        })
    }

    /// The CSS name, with the leading colon.
    #[must_use]
    pub fn as_css(self) -> &'static str {
        match self {
            Self::Hover => ":hover",
            Self::Active => ":active",
            Self::Focus => ":focus",
            Self::FocusWithin => ":focus-within",
            Self::FocusVisible => ":focus-visible",
            Self::Disabled => ":disabled",
            Self::Enabled => ":enabled",
            Self::Checked => ":checked",
            Self::Invalid => ":invalid",
        }
    }
}

impl ToCss for CrisolPseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(self.as_css())
    }
}

impl NonTSPseudoClass for CrisolPseudoClass {
    type Impl = CrisolSelectors;

    fn is_active_or_hover(&self) -> bool {
        matches!(self, Self::Active | Self::Hover)
    }

    fn is_user_action_state(&self) -> bool {
        matches!(
            self,
            Self::Active | Self::Hover | Self::Focus | Self::FocusWithin | Self::FocusVisible
        )
    }
}

/// A pseudo-element.
///
/// Uninhabited: there are none yet. Generated content needs a box in the tree, which is an
/// M2-tree change rather than a parser change, so `::before` and friends are a parse error
/// with a source location instead of a selector that quietly never matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrisolPseudoElement {}

impl ToCss for CrisolPseudoElement {
    fn to_css<W: fmt::Write>(&self, _dest: &mut W) -> fmt::Result {
        match *self {}
    }
}

impl PseudoElement for CrisolPseudoElement {
    type Impl = CrisolSelectors;
}

/// Parses the selector dialect above.
#[derive(Clone, Copy, Debug, Default)]
pub struct CrisolSelectorParser;

impl<'i> selectors::parser::Parser<'i> for CrisolSelectorParser {
    type Impl = CrisolSelectors;
    type Error = SelectorParseErrorKind<'i>;

    fn parse_is_and_where(&self) -> bool {
        // `:is()` and `:where()` are worth having: they are how a stylesheet avoids the
        // combinatorial explosion that makes large selector lists slow to match.
        true
    }

    fn parse_nth_child_of(&self) -> bool {
        false
    }

    fn parse_has(&self) -> bool {
        // `:has()` forces the matcher to look *down* the tree, which turns invalidation
        // from an ancestor walk into a subtree scan. Revisit at M6, when invalidation is
        // real and the cost can be measured rather than guessed at.
        false
    }

    fn parse_part(&self) -> bool {
        false
    }

    fn parse_slotted(&self) -> bool {
        false
    }

    fn parse_host(&self) -> bool {
        false
    }

    fn parse_non_ts_pseudo_class(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<CrisolPseudoClass, cssparser::ParseError<'i, Self::Error>> {
        CrisolPseudoClass::parse(&name).ok_or_else(|| {
            location.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(
                name,
            ))
        })
    }

    fn parse_pseudo_element(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<CrisolPseudoElement, cssparser::ParseError<'i, Self::Error>> {
        Err(
            location.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(
                name,
            )),
        )
    }
}

/// A parsed selector list.
pub type SelectorList = selectors::SelectorList<CrisolSelectors>;

/// A single parsed selector.
pub type Selector = selectors::parser::Selector<CrisolSelectors>;
