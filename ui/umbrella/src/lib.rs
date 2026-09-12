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
pub use crisol_dom as dom;
pub use crisol_events as events;
pub use crisol_html as html;
pub use crisol_layout as layout;
pub use crisol_paint as paint;
pub use crisol_reactive as reactive;
pub use crisol_style as style;
pub use crisol_text as text;
pub use crisol_tree as tree;

#[cfg(feature = "render")]
pub use crisol_render_wgpu as render;

/// The platform pointer shape for a computed [`style::CursorIcon`], or `None` to hide it.
///
/// The mapping lives here rather than in `crisol-style`, which must not know that windows
/// exist, or in `crisol-render-wgpu`, which must not know that a cascade does. This crate is
/// where Track A is assembled, so it is where the two vocabularies meet.
///
/// [`cursor_icon::CursorIcon`] is what `winit::window::Window::set_cursor` accepts, so the
/// result goes straight there.
///
/// ```
/// use crisol_ui::{platform_cursor, style::CursorIcon};
///
/// assert_eq!(
///     platform_cursor(CursorIcon::Pointer),
///     Some(cursor_icon::CursorIcon::Pointer)
/// );
/// // `auto` has to be resolved against what is under the pointer before it gets here.
/// assert_eq!(
///     platform_cursor(CursorIcon::Auto.resolve(true)),
///     Some(cursor_icon::CursorIcon::Text)
/// );
/// assert_eq!(platform_cursor(CursorIcon::None), None);
/// ```
#[must_use]
pub fn platform_cursor(icon: style::CursorIcon) -> Option<cursor_icon::CursorIcon> {
    use cursor_icon::CursorIcon as Platform;
    use style::CursorIcon as Icon;
    Some(match icon {
        // `none` is not a shape, so it cannot be one here. A caller hides the pointer.
        Icon::None => return None,
        // Unresolved `auto` reaching this point means the caller did not ask what is under
        // the pointer; an arrow is what every platform means by the bare default.
        Icon::Auto | Icon::Default => Platform::Default,
        Icon::ContextMenu => Platform::ContextMenu,
        Icon::Help => Platform::Help,
        Icon::Pointer => Platform::Pointer,
        Icon::Progress => Platform::Progress,
        Icon::Wait => Platform::Wait,
        Icon::Cell => Platform::Cell,
        Icon::Crosshair => Platform::Crosshair,
        Icon::Text => Platform::Text,
        Icon::VerticalText => Platform::VerticalText,
        Icon::Alias => Platform::Alias,
        Icon::Copy => Platform::Copy,
        Icon::Move => Platform::Move,
        Icon::NoDrop => Platform::NoDrop,
        Icon::NotAllowed => Platform::NotAllowed,
        Icon::Grab => Platform::Grab,
        Icon::Grabbing => Platform::Grabbing,
        Icon::EResize => Platform::EResize,
        Icon::NResize => Platform::NResize,
        Icon::NeResize => Platform::NeResize,
        Icon::NwResize => Platform::NwResize,
        Icon::SResize => Platform::SResize,
        Icon::SeResize => Platform::SeResize,
        Icon::SwResize => Platform::SwResize,
        Icon::WResize => Platform::WResize,
        Icon::EwResize => Platform::EwResize,
        Icon::NsResize => Platform::NsResize,
        Icon::NeswResize => Platform::NeswResize,
        Icon::NwseResize => Platform::NwseResize,
        Icon::ColResize => Platform::ColResize,
        Icon::RowResize => Platform::RowResize,
        Icon::AllScroll => Platform::AllScroll,
        Icon::ZoomIn => Platform::ZoomIn,
        Icon::ZoomOut => Platform::ZoomOut,
    })
}
