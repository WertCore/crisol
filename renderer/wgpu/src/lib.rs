//! winit window, wgpu surface, display-list rasterization.

#![doc(html_root_url = "https://docs.rs/crisol-render-wgpu/0.0.0")]

pub mod gpu;
pub mod headless;
pub mod renderer;
pub mod surface;
pub mod testing;
mod texture;

pub use gpu::{Gpu, GpuError, SharedGpu};
pub use headless::{HEADLESS_FORMAT, HeadlessTarget, Pixels};
pub use renderer::{FrameStats, FrameTarget, Renderer};
pub use surface::{AcquiredFrame, WindowSurface};
