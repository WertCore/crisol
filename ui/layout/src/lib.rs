//! taffy integration and the custom-node layout protocol.
//!
//! Layout is the pass between style and paint: it turns a styled tree into boxes, writing
//! each node's border box into `Node::layout` and projecting its computed style onto the
//! `BoxStyle` that paint reads.

#![doc(html_root_url = "https://docs.rs/crisol-layout/0.0.0")]

pub mod style_adapter;
pub mod tree_adapter;

use crisol_display_list::Size;
use crisol_style::StyleMap;
use crisol_tree::Tree;

pub use style_adapter::StyleRef;
pub use tree_adapter::{LayoutContext, LayoutStats};

/// Lays out `tree` into a `viewport`-sized area and writes the boxes back onto the nodes.
///
/// Convenience for a one-off pass. Hold a [`LayoutContext`] instead when laying out
/// repeatedly, so taffy's measurement caches survive between passes.
pub fn layout(tree: &mut Tree, styles: &StyleMap, viewport: Size) -> LayoutStats {
    let mut context = LayoutContext::new(tree, styles);
    context.run(viewport);
    context.stats()
}
