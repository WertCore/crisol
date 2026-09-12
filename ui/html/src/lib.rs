//! html5ever integration: HTML source to node tree.
//!
//! The parser is html5ever's; this crate is the `TreeSink` that drives it into a
//! [`crisol_tree::Tree`]. Nothing here re-implements HTML — the tree building algorithm,
//! implied tags, error recovery and encoding are all upstream's job (DECISIONS D-04).
//!
//! What *is* decided here is which parts of the DOM this engine keeps. Comments, processing
//! instructions, doctypes and templates have no box and no effect on layout, so they are
//! dropped rather than stored — the tree is what the engine renders, not an archive of the
//! source.
//!
//! ```
//! let document = crisol_html::parse("<p class=lede>hello</p>");
//! let root = document.tree.root().unwrap();
//! assert_eq!(document.tree.node(root).kind.tag(), Some("html"));
//! ```

#![doc(html_root_url = "https://docs.rs/crisol-html/0.0.0")]

mod sink;

use crisol_tree::Tree;

pub use sink::ParseError;

/// A parsed document.
#[derive(Debug)]
pub struct Document {
    /// The tree, rooted at `<html>`.
    pub tree: Tree,
    /// Everything html5ever complained about.
    ///
    /// Collected rather than logged. HTML has no fatal parse errors — the algorithm always
    /// produces a tree — so these are advisory, and `crisol doctor` is a better home for
    /// them than a log nobody reads.
    pub errors: Vec<ParseError>,
    /// Whether the source triggered quirks mode.
    ///
    /// Recorded and then ignored: ROADMAP §1 puts quirks mode permanently out of scope. It
    /// is here so that "this page renders oddly" has an answer other than a shrug.
    pub quirks: bool,
}

/// Parses a complete HTML document.
///
/// Never fails. html5ever's tree builder recovers from everything, inserting the implied
/// `<html>`, `<head>` and `<body>` as needed, so even an empty string produces a usable
/// tree. Anything it objected to along the way is in [`Document::errors`].
#[must_use]
pub fn parse(source: &str) -> Document {
    sink::parse_document(source)
}

/// Parses a fragment in the context of a `<div>`, as `innerHTML` does.
///
/// The tree is rooted at a synthetic `<div>` holding the fragment's top-level nodes, because
/// a fragment has no single root of its own and the engine's tree does.
#[must_use]
pub fn parse_fragment(source: &str) -> Document {
    sink::parse_fragment(source)
}
