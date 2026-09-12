//! Parsing selectors and testing them against a tree.

use cssparser::{Parser as CssParser, ParserInput};
use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};
use selectors::parser::ParseRelative;

use crate::element::ElementRef;
use crate::selector::{CrisolSelectorParser, Selector, SelectorList};

/// A selector that could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{source_text}: {message} (line {line}, column {column})")]
pub struct SelectorParseError {
    /// The selector text that failed.
    pub source_text: String,
    /// What was wrong with it.
    pub message: String,
    /// One-based line within the selector text.
    pub line: u32,
    /// One-based column.
    pub column: u32,
}

/// Parses a comma-separated selector list.
///
/// # Errors
///
/// Returns [`SelectorParseError`] for a selector that is malformed, or that uses a feature
/// this engine deliberately does not support — see [`crate::selector`] for the list. An
/// unsupported pseudo-class is an error here rather than a selector that never matches,
/// because the second is impossible to debug from the outside.
pub fn parse_selector_list(source: &str) -> Result<SelectorList, SelectorParseError> {
    let mut input = ParserInput::new(source);
    let mut parser = CssParser::new(&mut input);
    SelectorList::parse(
        &CrisolSelectorParser,
        &mut parser,
        // No nesting: `&` is only meaningful inside a rule, and rules are parsed by
        // `crate::stylesheet`, which resolves nesting before it gets here.
        ParseRelative::No,
    )
    .map_err(|error| SelectorParseError {
        source_text: source.to_owned(),
        message: format!("{:?}", error.kind),
        line: error.location.line + 1,
        column: error.location.column,
    })
}

/// A scratch buffer the matcher reuses across calls.
///
/// `selectors` keeps `:nth-child` index caches here. Constructing one per match is correct
/// but throws away the caching, which is the difference between `:nth-child` being O(1) and
/// O(siblings) per element.
#[derive(Default)]
pub struct MatchCaches {
    caches: SelectorCaches,
}

impl std::fmt::Debug for MatchCaches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatchCaches").finish_non_exhaustive()
    }
}

impl MatchCaches {
    /// An empty set of caches.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Tests one selector against one element.
    pub fn matches(&mut self, selector: &Selector, element: ElementRef<'_>) -> bool {
        let mut context = self.context();
        selectors::matching::matches_selector(selector, 0, None, &element, &mut context)
    }

    /// Tests a whole selector list, stopping at the first that matches.
    pub fn matches_any(&mut self, list: &SelectorList, element: ElementRef<'_>) -> bool {
        let mut context = self.context();
        selectors::matching::matches_selector_list(list, &element, &mut context)
    }

    fn context(&mut self) -> MatchingContext<'_, crate::selector::CrisolSelectors> {
        MatchingContext::new(
            // Normal: match the element itself, not a pseudo-element on it.
            MatchingMode::Normal,
            // No bloom filter. It is a fast reject for deep descendant selectors and only
            // pays off when the caller can keep one alive across a whole subtree, which is
            // the cascade's job at M3b, not this entry point's.
            None,
            &mut self.caches,
            // Standards mode. There is no quirks mode (ROADMAP §1).
            QuirksMode::NoQuirks,
            // Selector flags record which elements need re-matching when a sibling changes.
            // `ElementRef` only borrows the tree, so it cannot record them; M6 owns
            // invalidation and will revisit this.
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        )
    }
}

/// Tests one selector against one element, without reusable caches.
///
/// Convenient for a one-off question. Use [`MatchCaches`] when matching many elements.
#[must_use]
pub fn matches(selector: &Selector, element: ElementRef<'_>) -> bool {
    MatchCaches::new().matches(selector, element)
}

/// Tests a selector list against one element, without reusable caches.
#[must_use]
pub fn matches_any(list: &SelectorList, element: ElementRef<'_>) -> bool {
    MatchCaches::new().matches_any(list, element)
}
