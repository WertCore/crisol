//! PUBLIC text API: shaping, clusters, cursors, selection.
//!
//! ROADMAP §2.5 makes this the core competency rather than a checkbox. The eventual targets
//! — a PDF editor, a document editor — live or die on text, and every serious web editor
//! abandons `contenteditable` and renders text itself. That means the webview was never
//! providing the thing those applications need.
//!
//! So this is a *public API*, not an internal detail: shaped runs, cluster boundaries,
//! cursor affinity, selection rectangles and line box geometry are all supported surface.
//!
//! ```no_run
//! use crisol_text::{FontSystem, TextStyle, Wrapping, shape};
//! use crisol_display_list::Point;
//!
//! let mut fonts = FontSystem::new();
//! let layout = shape(&mut fonts, "hello", &TextStyle::default(), Some(200.0), Wrapping::Word);
//!
//! // Where did the user click?
//! let cursor = layout.point_to_cursor(Point::new(12.0, 4.0));
//! // Where should the caret be drawn?
//! let caret = layout.cursor_to_point(cursor);
//! // What should a selection highlight cover?
//! let rects = layout.selection_rects(0..3);
//! ```

#![doc(html_root_url = "https://docs.rs/crisol-text/0.0.0")]

mod layout;
mod model;
mod shaper;

pub use layout::TextLayout;
pub use model::{Affinity, CaretGeometry, Cursor, Direction, FontId, Glyph, Line, ShapedRun};
pub use shaper::{FontSystem, TextStyle, Wrapping, shape};
