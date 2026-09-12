//! Backend-independent draw command list and the geometry types shared across the engine.
//!
//! Paint (`crisol-paint`) turns a laid-out tree into a [`DisplayList`]; a renderer
//! (`crisol-render-wgpu`) turns a [`DisplayList`] into pixels. Neither knows about the
//! other, which is what makes the GPU backend swappable and render snapshot tests possible
//! without constructing a document.

#![doc(html_root_url = "https://docs.rs/crisol-display-list/0.0.0")]

pub mod command;
pub mod geom;

pub use command::{
    DisplayList, DisplayListBuilder, DrawCommand, ImageCommand, ImageId, RectCommand,
};
pub use geom::{Color, Corners, Edges, Point, Rect, Size};
