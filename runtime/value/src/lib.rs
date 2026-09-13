//! NaN-boxed values and the hidden-class shape system.
//!
//! ROADMAP §M9. The shape system is not here yet; [`Value`] is, and it is the thing the IR
//! and the calling convention are built on, which is why §M9 says to do this before the IR.

#![doc(html_root_url = "https://docs.rs/crisol-value/0.0.0")]

mod value;

pub use value::{ADDRESS_BITS, Address, Kind, Value};
