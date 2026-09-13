//! Precise mark-sweep collector, shadow stack, rooting.
//!
//! ROADMAP §M9. §3.1 names the GC/FFI boundary as this milestone's risk — every host function
//! that touches a JS value participates in rooting, and getting it wrong produces
//! use-after-free bugs that appear only under memory pressure. The answer here is a scope
//! guard that cannot be forgotten and a stress mode that collects on every allocation.

#![doc(html_root_url = "https://docs.rs/crisol-gc/0.0.0")]

mod handle;
mod heap;

pub use handle::GcRef;
pub use heap::{Collected, Heap, Rooted, Scope, Stats};
