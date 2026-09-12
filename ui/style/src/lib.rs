//! Cascade, inheritance, and interned computed style.

#![doc(html_root_url = "https://docs.rs/crisol-style/0.0.0")]

mod apply;
pub mod cascade;
pub mod computed;
pub mod intern;
pub mod values;

pub use cascade::{StyleEngine, StyleMap, StyleStats};
pub use computed::{ComputedStyle, INITIAL_FONT_SIZE, NORMAL_LINE_HEIGHT_RATIO};
pub use intern::StyleInterner;
pub use values::{
    AlignItems, Color, CornerRadii, Dimension, Display, FlexDirection, FlexWrap, FontStyle,
    JustifyContent, LengthPercentage, LineHeight, Number, Overflow, Position, Px, Sides,
    Visibility,
};
