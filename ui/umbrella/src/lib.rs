//! Crisol UI engine — HTML/CSS to a GPU-rendered native UI tree.
//!
//! This umbrella crate re-exports the Track A crates so the UI engine can be used
//! standalone, independently of the Crisol compiler and JavaScript runtime.
//!
//! ```text
//! html  ->  tree  ->  style  ->  layout  ->  paint  ->  display-list  ->  render-wgpu
//! ```

#![doc(html_root_url = "https://docs.rs/crisol-ui/0.0.0")]

pub use crisol_a11y as a11y;
pub use crisol_css as css;
pub use crisol_display_list as display_list;
pub use crisol_events as events;
pub use crisol_html as html;
pub use crisol_layout as layout;
pub use crisol_paint as paint;
pub use crisol_style as style;
pub use crisol_text as text;
pub use crisol_tree as tree;

#[cfg(feature = "render")]
pub use crisol_render_wgpu as render;
