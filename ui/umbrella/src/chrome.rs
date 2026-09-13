//! Window chrome an application draws itself.
//!
//! A window with decorations turned off has no title bar and no resize border, which is what
//! an application wants when its own interface should look the same on all three platforms.
//! What it gives up is the two things the system title bar was doing: moving the window, and
//! resizing it from an edge.
//!
//! Moving is the application's to decide, because only it knows which of its elements is a
//! title bar. A hit test against its own tree answers that.
//!
//! Resizing is geometry, and it is the same geometry everywhere, so it is here.
//!
//! Like [`crate::platform_cursor`], this names its own vocabulary rather than a windowing
//! crate's. Nothing above the renderer depends on `winit` — an application maps [`ResizeEdge`]
//! to whatever its window library calls the same eight directions, which is a `match` it
//! writes once.

use crisol_display_list::{Point, Size};

/// An edge or corner a borderless window can be resized from.
///
/// Compass names because that is what every windowing library calls them, so the mapping an
/// application writes is a rename rather than a translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResizeEdge {
    /// The top edge.
    North,
    /// The top-right corner.
    NorthEast,
    /// The right edge.
    East,
    /// The bottom-right corner.
    SouthEast,
    /// The bottom edge.
    South,
    /// The bottom-left corner.
    SouthWest,
    /// The left edge.
    West,
    /// The top-left corner.
    NorthWest,
}

/// Which edge or corner `point` falls in, or `None` for the interior.
///
/// `border` is how far in from an edge still counts, in the same logical pixels as `point`
/// and `size`. Corners win over edges — a point in the top-left square is `NorthWest` rather
/// than whichever of north and west happened to be tested first, which is what lets a corner
/// drag in two axes at once.
///
/// The band sits inside the window rather than straddling its boundary. A borderless window
/// has nothing outside it to hit, and a band that hung over the edge would claim pixels the
/// window does not own.
///
/// ```
/// use crisol_display_list::{Point, Size};
/// use crisol_ui::chrome::{ResizeEdge, resize_edge};
///
/// let size = Size { width: 100.0, height: 80.0 };
/// assert_eq!(resize_edge(Point::new(2.0, 2.0), size, 4.0), Some(ResizeEdge::NorthWest));
/// assert_eq!(resize_edge(Point::new(50.0, 2.0), size, 4.0), Some(ResizeEdge::North));
/// assert_eq!(resize_edge(Point::new(50.0, 40.0), size, 4.0), None);
/// ```
#[must_use]
pub fn resize_edge(point: Point, size: Size, border: f32) -> Option<ResizeEdge> {
    // Outside the window is not an edge. A drag that leaves the window keeps delivering
    // moves, and treating those as an edge would start a resize from wherever the pointer
    // happened to cross the boundary.
    if point.x < 0.0 || point.y < 0.0 || point.x > size.width || point.y > size.height {
        return None;
    }

    // A window can be narrower than two borders, mid-resize or just small. Splitting the
    // difference stops the two bands overlapping, which would make one edge shadow the other
    // and leave a window that had been shrunk impossible to grow again.
    let horizontal = border.min(size.width / 2.0);
    let vertical = border.min(size.height / 2.0);

    let west = point.x < horizontal;
    let east = point.x > size.width - horizontal;
    let north = point.y < vertical;
    let south = point.y > size.height - vertical;

    Some(match (north, south, west, east) {
        (true, _, true, _) => ResizeEdge::NorthWest,
        (true, _, _, true) => ResizeEdge::NorthEast,
        (_, true, true, _) => ResizeEdge::SouthWest,
        (_, true, _, true) => ResizeEdge::SouthEast,
        (true, ..) => ResizeEdge::North,
        (_, true, ..) => ResizeEdge::South,
        (_, _, true, _) => ResizeEdge::West,
        (_, _, _, true) => ResizeEdge::East,
        _ => return None,
    })
}
