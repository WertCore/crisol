//! Where a borderless window's resize band is.

use crisol_display_list::{Point, Size};
use crisol_ui::chrome::{ResizeEdge, resize_edge};

const SIZE: Size = Size {
    width: 100.0,
    height: 80.0,
};
const BORDER: f32 = 6.0;

fn at(x: f32, y: f32) -> Option<ResizeEdge> {
    resize_edge(Point::new(x, y), SIZE, BORDER)
}

#[test]
fn each_edge_has_its_own_band() {
    assert_eq!(at(50.0, 1.0), Some(ResizeEdge::North));
    assert_eq!(at(50.0, 79.0), Some(ResizeEdge::South));
    assert_eq!(at(1.0, 40.0), Some(ResizeEdge::West));
    assert_eq!(at(99.0, 40.0), Some(ResizeEdge::East));
}

#[test]
fn a_corner_beats_both_edges_it_is_made_of() {
    // The point of testing all four: an implementation that checks north before west gets
    // three of these right and calls the fourth `North`, and a corner that only resizes in
    // one axis feels broken in a way that is hard to name.
    assert_eq!(at(1.0, 1.0), Some(ResizeEdge::NorthWest));
    assert_eq!(at(99.0, 1.0), Some(ResizeEdge::NorthEast));
    assert_eq!(at(1.0, 79.0), Some(ResizeEdge::SouthWest));
    assert_eq!(at(99.0, 79.0), Some(ResizeEdge::SouthEast));
}

#[test]
fn the_interior_is_not_a_resize() {
    assert_eq!(at(50.0, 40.0), None);
    // Just inside the band's inner edge, which is where an off-by-one would show.
    assert_eq!(at(BORDER, BORDER), None);
    assert_eq!(at(SIZE.width - BORDER, SIZE.height - BORDER), None);
}

#[test]
fn outside_the_window_is_not_an_edge() {
    // A drag that leaves the window keeps reporting moves. Treating those as an edge would
    // start a resize from wherever the pointer crossed the boundary.
    assert_eq!(at(-1.0, 40.0), None);
    assert_eq!(at(50.0, -1.0), None);
    assert_eq!(at(101.0, 40.0), None);
    assert_eq!(at(50.0, 81.0), None);
}

#[test]
fn a_window_narrower_than_two_borders_still_has_two_edges() {
    // Mid-resize a window can be a few pixels wide. If both bands claimed the full border
    // they would overlap, one would shadow the other, and a window shrunk to nothing could
    // never be grown back — the failure is permanent, which is why it is worth a test.
    let thin = Size {
        width: 8.0,
        height: 8.0,
    };
    assert_eq!(
        resize_edge(Point::new(1.0, 4.0), thin, 20.0),
        Some(ResizeEdge::West)
    );
    assert_eq!(
        resize_edge(Point::new(7.0, 4.0), thin, 20.0),
        Some(ResizeEdge::East),
        "the far side must still be reachable"
    );
}
