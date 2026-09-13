//! NaN-boxed values and the hidden-class shape system.
//!
//! ROADMAP §M9. [`Value`] is the thing the IR and the calling convention are built on, which
//! is why §M9 says to do this before the IR; [`Shapes`] is how objects avoid carrying their
//! own property names.

#![doc(html_root_url = "https://docs.rs/crisol-value/0.0.0")]

mod key;
mod shape;
mod value;

pub use key::PropertyKey;
pub use shape::{ShapeId, Shapes, Slot};
pub use value::{ADDRESS_BITS, Address, Kind, Value};
