//! Signals, effects and a component model over the crisol DOM.
//!
//! ROADMAP §M7 asks for *signals, effects, a component abstraction, and the mutation API
//! that a foreign caller (the JS runtime, later) will drive*, and to *design it as if an
//! external consumer exists, because one will*. The mutation API is [`crisol_dom`]; this
//! crate is the reactive layer over it, and its first consumer.
//!
//! # The shape
//!
//! A component runs **once**. It builds nodes and registers effects that keep specific
//! pieces of those nodes in sync with specific signals. Changing state re-runs an effect,
//! not a component — so an edit writes one text node rather than rebuilding a subtree and
//! diffing it. Lists are the exception that needs real work, and [`Keyed`] does it by key.
//!
//! ```
//! use crisol_dom::Dom;
//! use crisol_reactive::{Cx, Runtime, bind_text, element, mount, text};
//! use crisol_tree::Tree;
//!
//! let mut tree = Tree::new();
//! let runtime = Runtime::new();
//! let mut dom = Dom::new(&mut tree);
//! let root = dom.create_element("div");
//! dom.set_root(root);
//!
//! let name = runtime.signal(String::from("world"));
//! let mut cx = Cx::new(&runtime, &mut dom);
//! mount(&mut cx, root, |cx: &mut Cx<'_, '_>| {
//!     let paragraph = element(cx, "p");
//!     let label = text(cx, "");
//!     crisol_reactive::append(cx, paragraph, label);
//!     bind_text(cx, label, move |track| format!("hello {}", track.get(name)));
//!     paragraph
//! });
//!
//! runtime.set(name, String::from("crisol"));
//! runtime.flush(&mut dom);
//! # let _ = root;
//! ```
//!
//! # Where the runtime lives
//!
//! [`Runtime`] is an ordinary value passed by reference, not an ambient thread-local
//! (DECISIONS D-43). Two windows means two runtimes, and the JS runtime at M16 has to say
//! which one it is driving rather than inherit whichever thread it happens to be on.

#![doc(html_root_url = "https://docs.rs/crisol-reactive/0.0.0")]

mod list;
mod runtime;
mod view;

pub use list::{Keyed, ListStats};
pub use runtime::{Cx, Effect, Memo, Runtime, RuntimeStats, Scope, Signal, Track};
pub use view::{
    Mounted, View, append, bind_attribute, bind_class, bind_text, element, element_with_class,
    mount, text,
};
