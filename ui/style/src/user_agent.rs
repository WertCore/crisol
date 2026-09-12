//! The engine's own stylesheet.
//!
//! Deliberately tiny. A browser's user-agent stylesheet is hundreds of rules because it has
//! to make a document written in 1998 render sensibly; this engine renders applications and
//! owes nothing to that (ROADMAP §1). Every rule here has to earn its place by describing
//! something that is true of *this* engine rather than of HTML.

/// The rules the engine applies before any author stylesheet.
///
/// One rule, and the reason is the difference between a document and an application. In a
/// browser the root element has `height: auto` and shrinks to its content, with the viewport
/// merely being what you see of it. In an application the root *is* the window: a layout
/// where the root box is shorter than the window has no way to express "fill the space",
/// and every author would open with this rule anyway.
///
/// An author who genuinely wants a content-sized root can say so — `:root { height: auto }`
/// wins, because author rules beat user-agent rules regardless of specificity.
pub const STYLESHEET: &str = ":root { width: 100%; height: 100% }";
