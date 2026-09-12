//! Hit testing, capture/bubble, focus, IME.

#![doc(html_root_url = "https://docs.rs/crisol-events/0.0.0")]

pub mod dispatch;
pub mod event;
pub mod focus;
pub mod hit;

pub use dispatch::{EventSystem, Listener, When, pointer_at};
pub use event::{
    Dispatch, Event, Key, KeyEvent, Modifiers, Outcome, Phase, PointerButton, PointerEvent,
    PointerId, PointerKind,
};
pub use focus::{Direction as FocusDirection, Focusable, focusable, next_focus, tab_order};
pub use hit::{Hit, TextLookup, hit_test, hit_test_with_text, path_to};
