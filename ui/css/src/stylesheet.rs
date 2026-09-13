//! Parsing a stylesheet into rules this engine can match and apply.
//!
//! lightningcss owns the grammar: tokenizing, error recovery, shorthand expansion, colour
//! and `calc()` arithmetic. This module owns the translation from what lightningcss
//! produces into what the cascade needs — selectors re-parsed into our own dialect (D-20),
//! shorthands flattened to longhands, and everything the engine does not implement dropped
//! with a warning rather than silently.

use lightningcss::printer::PrinterOptions;
use lightningcss::properties::Property;
use lightningcss::rules::CssRule;
use lightningcss::stylesheet::{MinifyOptions, ParserOptions, StyleSheet};
use lightningcss::targets::{Features, Targets};
use lightningcss::traits::ToCss;
// `into_owned` lifts a borrowed property to `'static` so a rule outlives the source text.
use static_self::IntoOwned;

use crate::matching::{SelectorParseError, parse_selector_list};
use crate::selector::SelectorList;

/// Where a rule came from, which is the first thing the cascade sorts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// The engine's own defaults. Loses to everything.
    UserAgent,
    /// The application's stylesheets.
    Author,
    /// A `style` attribute. Beats any author rule, however specific.
    ///
    /// Last in the enum because the cascade sorts on the derived `Ord`: an inline
    /// declaration outranks an author one without having to carry a specificity that could
    /// be out-argued. That is the CSS rule too — inline style is its own origin, not a
    /// very specific selector.
    Inline,
}

/// One `selector { declarations }` rule.
#[derive(Debug)]
pub struct StyleRule {
    /// The selectors that make this rule apply.
    pub selectors: SelectorList,
    /// Longhand declarations in source order, each with its `!important` flag.
    ///
    /// Shorthands are expanded here rather than at apply time, so the cascade compares
    /// like with like: `margin: 0` and `margin-top: 4px` have to be able to override each
    /// other property by property, which they cannot do while one is still a shorthand.
    pub declarations: Vec<(Property<'static>, bool)>,
    /// Position in the stylesheet. The cascade's last tiebreak.
    pub source_order: u32,
}

/// A parsed stylesheet.
#[derive(Debug)]
pub struct Stylesheet {
    /// Whether any selector in here uses `+` or `~`.
    ///
    /// Invalidation reads this. A state or attribute change on a node can affect its
    /// following siblings *only* through those two combinators (D-38), so a sheet without
    /// them makes the sibling half of that walk provably dead work — and on a long list it
    /// is the expensive half. Computed once at parse rather than asked per invalidation,
    /// because it is a property of the sheet and the sheet does not change.
    pub uses_sibling_combinators: bool,
    /// The rules, in source order.
    pub rules: Vec<StyleRule>,
    /// Where these rules came from.
    pub origin: Origin,
    /// Everything that was dropped, and why.
    pub warnings: Vec<Warning>,
}

/// Something in the source that was skipped.
///
/// Warnings rather than errors because CSS error recovery is a feature: one unsupported
/// rule must not cost the author the rest of the stylesheet. They are collected rather than
/// logged so that `crisol doctor` can report them (ROADMAP §3.3 applies the same idea to
/// JavaScript).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// What was skipped.
    pub message: String,
    /// One-based line in the source.
    pub line: u32,
}

/// A stylesheet that could not be parsed at all.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("could not parse the stylesheet: {message}")]
pub struct StylesheetParseError {
    /// What lightningcss reported.
    pub message: String,
}

impl Stylesheet {
    /// Parses `source` as an author stylesheet.
    ///
    /// # Errors
    ///
    /// Returns [`StylesheetParseError`] only when the source cannot be tokenized at all.
    /// A rule that is individually unsupported becomes a [`Warning`] and is skipped, which
    /// is what CSS error recovery means.
    pub fn parse(source: &str) -> Result<Self, StylesheetParseError> {
        Self::parse_with_origin(source, Origin::Author)
    }

    /// Parses `source` with an explicit origin.
    ///
    /// # Errors
    ///
    /// As [`Self::parse`].
    pub fn parse_with_origin(source: &str, origin: Origin) -> Result<Self, StylesheetParseError> {
        let flattened;
        let source = match Self::flatten_nesting(source)? {
            Some(text) => {
                flattened = text;
                flattened.as_str()
            }
            None => source,
        };

        let parsed = StyleSheet::parse(source, ParserOptions::default()).map_err(|error| {
            StylesheetParseError {
                message: error.to_string(),
            }
        })?;

        let mut sheet = Self {
            rules: Vec::new(),
            origin,
            warnings: Vec::new(),
            uses_sibling_combinators: false,
        };
        let mut source_order = 0;
        for rule in &parsed.rules.0 {
            sheet.collect(rule, &mut source_order);
        }
        sheet.uses_sibling_combinators = sheet
            .rules
            .iter()
            .any(|rule| selectors_use_sibling_combinators(&rule.selectors));
        Ok(sheet)
    }

    /// Rewrites `source` with nested rules lifted to the top level, or returns `None` when
    /// there is no nesting and the original text will do.
    ///
    /// lightningcss implements this transform, but in its *printer* rather than in the rule
    /// tree: after `minify`, a nested rule is still a child of its parent. Rather than write
    /// a nesting resolver — which means reimplementing what `&` means for specificity, and
    /// getting it wrong inside `:is()` — the sheet is printed with nesting lowered and read
    /// back.
    ///
    /// The extra parse is paid only by stylesheets that actually nest, which is why the
    /// check comes first.
    ///
    /// # Errors
    ///
    /// Returns [`StylesheetParseError`] when the source cannot be tokenized, or when
    /// lightningcss cannot print what it just parsed — which would be a bug in lightningcss,
    /// not in the source.
    fn flatten_nesting(source: &str) -> Result<Option<String>, StylesheetParseError> {
        let mut parsed = StyleSheet::parse(source, ParserOptions::default()).map_err(|error| {
            StylesheetParseError {
                message: error.to_string(),
            }
        })?;

        if !parsed.rules.0.iter().any(has_nested_rules) {
            return Ok(None);
        }

        // `include` means "lower this regardless of what any browser supports".
        let targets = Targets {
            browsers: None,
            include: Features::Nesting,
            exclude: Features::empty(),
        };
        parsed
            .minify(MinifyOptions {
                targets,
                ..MinifyOptions::default()
            })
            .map_err(|error| StylesheetParseError {
                message: error.to_string(),
            })?;
        let printed = parsed
            .to_css(PrinterOptions {
                targets,
                ..PrinterOptions::default()
            })
            .map_err(|error| StylesheetParseError {
                message: error.to_string(),
            })?;
        Ok(Some(printed.code))
    }

    fn collect(&mut self, rule: &CssRule<'_>, source_order: &mut u32) {
        match rule {
            CssRule::Style(style) => {
                let line = style.loc.line + 1;
                let selector_text = match style.selectors.to_css_string(PrinterOptions::default()) {
                    Ok(text) => text,
                    Err(error) => {
                        self.warn(line, format!("could not re-read the selector: {error}"));
                        return;
                    }
                };
                match parse_selector_list(&selector_text) {
                    Ok(selectors) => {
                        let declarations = flatten(&style.declarations);
                        if !declarations.is_empty() {
                            self.rules.push(StyleRule {
                                selectors,
                                declarations,
                                source_order: *source_order,
                            });
                            *source_order += 1;
                        }
                    }
                    Err(SelectorParseError { message, .. }) => {
                        self.warn(line, format!("skipped `{selector_text}`: {message}"));
                    }
                }

                // `flatten_nesting` lifted these to the top level before parsing, so there
                // should be none left. Walking them anyway costs nothing and means a change
                // in lightningcss's printer cannot silently drop a rule.
                for nested in &style.rules.0 {
                    self.collect(nested, source_order);
                }
            }
            // Everything else — @media, @supports, @font-face, @keyframes — is a milestone
            // of its own. Saying so is better than dropping it silently.
            other => {
                self.warn(0, format!("skipped {}", describe(other)));
            }
        }
    }

    fn warn(&mut self, line: u32, message: String) {
        self.warnings.push(Warning { message, line });
    }
}

/// Whether any selector in `list` can reach across a sibling boundary.
///
/// Walks the components in match order and looks for the two combinators that do it. `:is()`
/// and `:where()` hold nested selector lists, so those are walked too — a `+` inside one is
/// still a `+`, and missing it would make the invalidation narrower than the dialect allows,
/// which is the one direction that turns a performance change into a correctness bug.
fn selectors_use_sibling_combinators(list: &SelectorList) -> bool {
    use selectors::parser::{Combinator, Component};

    fn selector_does(selector: &crate::selector::Selector) -> bool {
        selector
            .iter_raw_match_order()
            .any(|component| match component {
                Component::Combinator(Combinator::NextSibling | Combinator::LaterSibling) => true,
                Component::Is(list) | Component::Where(list) | Component::Negation(list) => {
                    list.slice().iter().any(selector_does)
                }
                _ => false,
            })
    }

    list.slice().iter().any(selector_does)
}

/// Parses the body of a `style` attribute: a declaration list, with no selector or braces.
///
/// Returns an empty list for anything it cannot parse, on the same reasoning as the
/// stylesheet's warnings — one bad declaration must not cost the element the rest of them.
/// lightningcss already recovers per declaration inside the block, so this only discards the
/// whole thing when the attribute is not a declaration list at all.
///
/// Parsing lives here, rather than the parsed form living on the element, because
/// `crisol-css` depends on `crisol-tree` and not the other way round. The element stores the
/// string it was given; the cascade turns it into declarations when it needs them.
#[must_use]
pub fn parse_inline_style(source: &str) -> Vec<(Property<'static>, bool)> {
    lightningcss::declaration::DeclarationBlock::parse_string(source, ParserOptions::default())
        .as_ref()
        .map(flatten)
        .unwrap_or_default()
}

/// Flattens a declaration block to longhands, `!important` declarations last.
///
/// Order matters and is not the source order: within one rule, an `!important` declaration
/// beats a normal one regardless of where it appears. Putting them last means the cascade
/// can apply declarations in order and let later ones win, without a second pass.
fn flatten(
    block: &lightningcss::declaration::DeclarationBlock<'_>,
) -> Vec<(Property<'static>, bool)> {
    let mut out = Vec::with_capacity(block.declarations.len() + block.important_declarations.len());
    for property in &block.declarations {
        expand(property, false, &mut out);
    }
    for property in &block.important_declarations {
        expand(property, true, &mut out);
    }
    out
}

/// Expands one declaration into longhands.
///
/// Recursive because shorthands nest: `border` expands to `border-width`, `border-style` and
/// `border-color`, each of which is itself a shorthand over four edges.
fn expand(property: &Property<'_>, important: bool, out: &mut Vec<(Property<'static>, bool)>) {
    let id = property.property_id();
    let Some(longhands) = id.longhands() else {
        out.push((property.clone().into_owned(), important));
        return;
    };
    for longhand_id in longhands {
        if let Some(value) = property.longhand(&longhand_id) {
            expand(&value, important, out);
        }
    }
}

/// Whether this rule, or anything inside it, has a nested rule.
fn has_nested_rules(rule: &CssRule<'_>) -> bool {
    match rule {
        CssRule::Style(style) => {
            !style.rules.0.is_empty() || style.rules.0.iter().any(has_nested_rules)
        }
        _ => false,
    }
}

fn describe(rule: &CssRule<'_>) -> &'static str {
    match rule {
        CssRule::Media(_) => "@media (not implemented; the property subset comes first)",
        CssRule::Supports(_) => "@supports (not implemented)",
        CssRule::FontFace(_) => "@font-face (fonts arrive at M4)",
        CssRule::Keyframes(_) => "@keyframes (animation is not in M3's subset)",
        CssRule::Import(_) => "@import (no loader yet)",
        CssRule::Page(_) => "@page (paged media is out of scope)",
        CssRule::Namespace(_) => "@namespace (there are no namespaces)",
        _ => "an unsupported at-rule",
    }
}
