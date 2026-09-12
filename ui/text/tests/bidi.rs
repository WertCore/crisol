//! Bidirectional text: visual order, carets at a direction boundary, and split selections.
//!
//! ROADMAP §M4 shipped the API carrying `Direction` and `Affinity` while saying bidi layout
//! was M8's. This is M8's.
//!
//! As in `text.rs`, these assert *relations* rather than pixel positions, because the fonts
//! differ between a developer's machine and a runner. What they do pin is which side of a
//! boundary a caret lands on, and that a selection does not highlight text nobody selected.

use crisol_display_list::Point;
use crisol_text::{
    Affinity, Cursor, Direction, FontSystem, TextLayout, TextStyle, Wrapping, shape,
};

/// `ab שלום cd` — Latin, Hebrew, Latin. Byte offsets:
///
/// ```text
///   a b ␣ ש  ל  ו  ם  ␣  c  d
///   0 1 2 3  5  7  9  11 12 13   (14 = end)
/// ```
const MIXED: &str = "ab שלום cd";
const HEBREW: &str = "שלום";
/// Where the Latin ends and the Hebrew begins.
const LTR_TO_RTL: usize = 3;
/// Where the Hebrew ends and the space before `cd` begins.
const RTL_TO_LTR: usize = 11;

const REQUIRE_FONTS: &str = "CRISOL_REQUIRE_FONTS";

/// Shapes `text`, or `None` after saying why nothing was checked.
///
/// A machine with no Hebrew font shapes the string to nothing and every assertion below
/// passes vacuously, which is indistinguishable from the engine being correct. So when
/// `CRISOL_REQUIRE_FONTS=1` this is a failure rather than a skip — the same bargain the rest
/// of the suite makes, extended to *coverage* and not merely to the presence of some font.
fn rtl(text: &str) -> Option<(FontSystem, TextLayout)> {
    let mut fonts = FontSystem::new();
    let required = std::env::var(REQUIRE_FONTS).is_ok_and(|value| value == "1");
    if fonts.is_empty() {
        assert!(!required, "{REQUIRE_FONTS}=1 but no fonts are installed");
        eprintln!("skipping: no fonts installed");
        return None;
    }
    let layout = shape(
        &mut fonts,
        text,
        &TextStyle::default(),
        None,
        Wrapping::Word,
    );
    let has_rtl = layout
        .runs()
        .iter()
        .any(|run| run.direction == Direction::Rtl);
    if !has_rtl {
        assert!(
            !required,
            "{REQUIRE_FONTS}=1 but no installed font covers Hebrew, so the bidi tests \
             would pass without testing anything"
        );
        eprintln!("skipping: no font covers Hebrew on this machine");
        return None;
    }
    Some((fonts, layout))
}

/// The visual span of a glyph, left edge first whichever way its run runs.
fn span(glyph: &crisol_text::Glyph) -> (f32, f32) {
    let a = glyph.position.x;
    let b = glyph.position.x + glyph.advance;
    (a.min(b), a.max(b))
}

// ---- visual order --------------------------------------------------------------------------

#[test]
fn right_to_left_text_starts_at_the_right() {
    let Some((_, layout)) = rtl(HEBREW) else {
        return;
    };
    let glyphs = layout.glyphs();

    // The first character logically is the rightmost visually. That is the whole of what
    // "right to left" means, and it is the thing an engine that ignored `level` would get
    // backwards while still drawing every glyph.
    let first = span(&glyphs[0]);
    let last = span(&glyphs[glyphs.len() - 1]);
    assert!(
        first.0 > last.0,
        "the first cluster should be further right than the last: {first:?} vs {last:?}"
    );
    assert!(
        layout
            .runs()
            .iter()
            .all(|run| run.direction == Direction::Rtl),
        "an all-Hebrew line is one right-to-left run"
    );
}

#[test]
fn a_mixed_line_is_laid_out_in_visual_order() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };

    // Glyphs come out in the order they are drawn, left to right, whatever the logical
    // order was. A renderer walks this list and a hit test scans it; both break if the
    // reordering has not happened by now.
    let mut previous = f32::NEG_INFINITY;
    for glyph in layout.glyphs() {
        let (left, right) = span(glyph);
        assert!(left >= previous - 0.01, "glyphs are not in visual order");
        previous = right;
    }

    let directions: Vec<_> = layout.runs().iter().map(|run| run.direction).collect();
    assert_eq!(
        directions,
        [Direction::Ltr, Direction::Rtl, Direction::Ltr],
        "three runs: Latin, Hebrew, Latin"
    );
}

// ---- carets --------------------------------------------------------------------------------

#[test]
fn a_caret_at_a_direction_boundary_has_two_places_and_affinity_chooses() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };

    // One byte offset, two places on screen — the same thing affinity answers at a soft
    // wrap, which is why there is only one flag for both.
    let after_latin = layout.cursor_to_point(Cursor::upstream(LTR_TO_RTL));
    let before_hebrew = layout.cursor_to_point(Cursor::new(LTR_TO_RTL));

    assert!(
        before_hebrew.position.x > after_latin.position.x,
        "reading Hebrew starts at its right-hand end, which is further right than the \
         space it follows: {} vs {}",
        before_hebrew.position.x,
        after_latin.position.x
    );
    assert_eq!(after_latin.direction, Direction::Ltr);
    assert_eq!(before_hebrew.direction, Direction::Rtl);
}

#[test]
fn the_other_boundary_works_the_same_way_round() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };

    let end_of_hebrew = layout.cursor_to_point(Cursor::upstream(RTL_TO_LTR));
    let start_of_latin = layout.cursor_to_point(Cursor::new(RTL_TO_LTR));

    assert_eq!(end_of_hebrew.direction, Direction::Rtl);
    assert_eq!(start_of_latin.direction, Direction::Ltr);
    assert!(
        end_of_hebrew.position.x < start_of_latin.position.x,
        "the Hebrew ends at its left-hand end; the Latin after it begins to the right"
    );
}

#[test]
fn caret_direction_is_the_run_it_is_in_not_the_line_it_is_on() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };

    // Taking the direction from the line's first run reports Ltr everywhere on this line,
    // which tells a renderer to draw every caret leaning the wrong way.
    assert_eq!(
        layout.cursor_to_point(Cursor::new(1)).direction,
        Direction::Ltr
    );
    assert_eq!(
        layout.cursor_to_point(Cursor::new(5)).direction,
        Direction::Rtl,
        "inside the Hebrew"
    );
    assert_eq!(
        layout.cursor_to_point(Cursor::new(13)).direction,
        Direction::Ltr
    );
}

#[test]
fn affinity_still_means_nothing_away_from_a_boundary() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };
    for index in [1, 5, 7, 13] {
        let downstream = layout.cursor_to_point(Cursor::new(index)).position.x;
        let upstream = layout.cursor_to_point(Cursor::upstream(index)).position.x;
        // Within a run the two are the trailing edge of one glyph and the leading edge of
        // the next, which are the same place reached by two different sums. Asserting bit
        // equality tests the font's floating point rather than the engine, and the runners
        // disagree with this machine by a hundredth of a pixel.
        assert!(
            (downstream - upstream).abs() < 0.05,
            "index {index} is not a boundary, so both sides should be the same place: \
             {downstream} vs {upstream}"
        );
    }
}

// ---- hit testing ---------------------------------------------------------------------------

#[test]
fn clicking_inside_a_run_lands_in_that_run() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };

    // The two runs meeting at a boundary offer the same x for two different byte offsets.
    // Without preferring the run the point is in, a click just inside the Hebrew resolves
    // to the offset at the end of the Latin.
    let hebrew_run = layout.runs()[1].clone();
    let glyphs = layout.run_glyphs(&hebrew_run);
    let left = glyphs
        .iter()
        .map(|g| span(g).0)
        .fold(f32::INFINITY, f32::min);
    let right = glyphs
        .iter()
        .map(|g| span(g).1)
        .fold(f32::NEG_INFINITY, f32::max);

    for at in [left + 1.0, (left + right) * 0.5, right - 1.0] {
        let cursor = layout.point_to_cursor(Point::new(at, 1.0));
        assert!(
            (LTR_TO_RTL..=RTL_TO_LTR).contains(&cursor.index),
            "a click at x={at} is inside the Hebrew but resolved to byte {}",
            cursor.index
        );
    }
}

#[test]
fn a_click_round_trips_back_to_where_it_was_clicked() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };

    // The affinity a click returns is what makes this hold at a boundary: dropping it sends
    // the caret to the other place the same offset sits, which is a visible jump.
    let width = layout.size().width;
    let mut at = 1.0;
    while at < width {
        let cursor = layout.point_to_cursor(Point::new(at, 1.0));
        let back = layout.cursor_to_point(cursor);
        assert!(
            (back.position.x - at).abs() <= 12.0,
            "clicked at {at}, caret came back at {} (byte {}, {:?})",
            back.position.x,
            cursor.index,
            cursor.affinity
        );
        at += 3.0;
    }
}

// ---- selection -----------------------------------------------------------------------------

#[test]
fn a_selection_that_is_visually_split_is_drawn_split() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };

    // `b`, the space, and the first Hebrew letter. The first letter of the Hebrew is drawn
    // at its *right-hand* end, so three characters that are contiguous in the string are
    // two pieces on screen with four unselected characters between them.
    let rects = layout.selection_rects(1..5);
    assert_eq!(
        rects.len(),
        2,
        "taking the leftmost to the rightmost would highlight the four letters in between"
    );
    assert!(
        rects[0].max_x() < rects[1].min_x(),
        "and they do not overlap"
    );
}

#[test]
fn a_selection_inside_one_direction_stays_one_rectangle() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };
    assert_eq!(layout.selection_rects(0..2).len(), 1, "within the Latin");
    assert_eq!(
        layout.selection_rects(LTR_TO_RTL..RTL_TO_LTR).len(),
        1,
        "the whole Hebrew run is contiguous on screen"
    );
    assert_eq!(
        layout.selection_rects(12..14).len(),
        1,
        "the trailing Latin"
    );
}

#[test]
fn selecting_everything_covers_the_line_without_gaps() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };
    let rects = layout.selection_rects(0..MIXED.len());
    assert_eq!(
        rects.len(),
        1,
        "every glyph is selected, so there is no gap"
    );
    assert!(rects[0].width() >= layout.size().width - 0.5);
}

#[test]
fn an_empty_selection_draws_nothing() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };
    assert!(layout.selection_rects(5..5).is_empty());
}

// ---- movement ------------------------------------------------------------------------------

#[test]
fn cursor_movement_is_logical_and_not_visual() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };

    // Pressing right-arrow in most editors moves through the string, not across the screen.
    // The distinction matters here because the two disagree inside the Hebrew.
    let mut index = 0;
    let mut visited = vec![0];
    while index < MIXED.len() {
        index = layout.next_cluster(index);
        visited.push(index);
    }
    assert_eq!(visited, [0, 1, 2, 3, 5, 7, 9, 11, 12, 13, 14]);

    let mut back = vec![MIXED.len()];
    let mut index = MIXED.len();
    while index > 0 {
        index = layout.prev_cluster(index);
        back.push(index);
    }
    back.reverse();
    assert_eq!(visited, back, "and it is reversible");
}

#[test]
fn every_cluster_boundary_is_a_char_boundary() {
    let Some((_, layout)) = rtl(MIXED) else {
        return;
    };
    for index in 0..=MIXED.len() {
        if layout.is_cluster_boundary(index) {
            assert!(
                MIXED.is_char_boundary(index),
                "byte {index} is offered as a caret position but splits a character"
            );
        }
    }
    let _ = Affinity::Downstream;
}
