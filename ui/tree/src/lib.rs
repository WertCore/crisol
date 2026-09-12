//! Arena-allocated node tree, generational handles, dirty tracking.
//!
//! The tree is the spine of the UI engine: HTML parsing builds it, the cascade styles it,
//! layout measures it, paint walks it, events hit-test it, and from M16 JavaScript mutates
//! it. Everything else in Track A is a pass over this structure.
//!
//! ```
//! use crisol_display_list::{Color, Rect};
//! use crisol_tree::{BoxStyle, Tree};
//!
//! let mut tree = Tree::new();
//! let root = tree.create_element("div");
//! tree.set_root(root).unwrap();
//!
//! let child = tree.create_element("div");
//! tree.append_child(root, child).unwrap();
//!
//! tree.node_mut(child).layout = Rect::from_xywh(8.0, 8.0, 32.0, 32.0);
//! tree.node_mut(child).style = BoxStyle::filled(Color::from_rgba8(97, 175, 239, 255));
//!
//! assert_eq!(tree.children(root).count(), 1);
//! ```

#![doc(html_root_url = "https://docs.rs/crisol-tree/0.0.0")]

pub mod atom;
pub mod custom;
pub mod dirty;
pub mod id;
pub mod map;
pub mod node;
pub mod tree;

pub use atom::Atom;
pub use custom::{ColorBox, CustomHit, CustomNode, MeasureConstraints};
pub use dirty::DirtyFlags;
pub use id::NodeId;
pub use map::NodeMap;
pub use node::{Attribute, BoxStyle, CustomElement, ElementData, ElementState, Node, NodeKind};
pub use tree::{Children, Tree, TreeError, TreeStats};
