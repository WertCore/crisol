//! lightningcss parsing and `selectors` matching.
//!
//! ```
//! use crisol_css::{ElementRef, matching};
//! use crisol_tree::Tree;
//!
//! let mut tree = Tree::new();
//! let root = tree.create_element("div");
//! tree.set_root(root).unwrap();
//! tree.element_mut(root).unwrap().set_class("card wide");
//!
//! let list = matching::parse_selector_list("div.card").unwrap();
//! let element = ElementRef::new(&tree, root).unwrap();
//! assert!(matching::matches_any(&list, element));
//! ```

#![doc(html_root_url = "https://docs.rs/crisol-css/0.0.0")]

pub mod element;
pub mod ident;
pub mod matching;
pub mod selector;

pub use element::ElementRef;
pub use ident::CssIdent;
pub use matching::{MatchCaches, SelectorParseError, parse_selector_list};
pub use selector::{
    CrisolPseudoClass, CrisolPseudoElement, CrisolSelectors, Selector, SelectorList,
};
