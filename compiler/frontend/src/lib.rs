//! oxc wrapper, module resolution, module graph.
//!
//! Scheduled for **M10**. See `ROADMAP.md` and `STATE.md` for the current position.

#![doc(html_root_url = "https://docs.rs/crisol-frontend/0.0.0")]

mod escape;
mod graph;
mod loader;
mod lower;

pub use graph::{ModuleGraph, ModuleId};
pub use loader::{Loader, Request, Unresolved};
pub use lower::{Lowered, ParseFailed, Unsupported, lower};
