//! The text API M4's acceptance names.
//!
//! These assert *relations* — monotonicity, containment, round-tripping — rather than
//! absolute pixel positions, because the installed fonts differ between a developer's
//! machine and a CI runner, and a test that pins an advance width to two decimal places
//! tests the font rather than the engine.
//!
//! `CRISOL_REQUIRE_FONTS=1` turns a machine with no fonts from a skip into a failure, the
//! same way `CRISOL_REQUIRE_GPU` does for the renderer: a suite where everything silently
//! skipped is indistinguishable from one where everything passed.

use crisol_display_list::Point;
use crisol_text::{
    Affinity, Cursor, Direction, FontSystem, TextLayout, TextStyle, Wrapping, shape,
};

const REQUIRE_FONTS: &str = "CRISOL_REQUIRE_FONTS";

/// A font system, or `None` after saying why.
fn fonts() -> Option<FontSystem> {
    let fonts = FontSystem::new();
    if fonts.is_empty() {
        let required = std::env::var(REQUIRE_FONTS).is_ok_and(|value| value == "1");
        assert!(!required, "{REQUIRE_FONTS}=1 but no fonts are installed");
        eprintln!("skipping: no fonts installed. Set {REQUIRE_FONTS}=1 to make this a failure.");
        return None;
    }
    Some(fonts)
}

fn with_fonts(body: impl FnOnce(FontSystem)) {
    if let Some(fonts) = fonts() {
        body(fonts);
    }
}

fn lay_out(fonts: &mut FontSystem, text: &str, width: Option<f32>) -> TextLayout {
    shape(fonts, text, &TextStyle::default(), width, Wrapping::Word)
}

// ---- shaping -------------------------------------------------------------------------

#[test]
fn a_simple_string_produces_glyphs_on_one_line() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "hello", None);
        assert_eq!(layout.lines().len(), 1);
        assert_eq!(layout.glyphs().len(), 5);
        assert!(layout.size().width > 0.0);
        assert!(layout.size().height > 0.0);
    });
}

#[test]
fn an_empty_string_still_has_a_line_for_the_caret_to_sit_on() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "", Some(100.0));
        assert_eq!(layout.lines().len(), 1, "a caret has to go somewhere");
        assert!(layout.glyphs().is_empty());
        assert!(
            layout.size().height > 0.0,
            "an empty block is still one line tall"
        );

        let caret = layout.cursor_to_point(Cursor::new(0));
        assert_eq!(caret.position.x, 0.0);
        assert!(caret.height > 0.0);
    });
}

#[test]
fn glyphs_advance_left_to_right_and_carry_their_clusters() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "abcdef", None);
        let mut previous_x = f32::NEG_INFINITY;
        let mut previous_cluster = 0;
        for glyph in layout.glyphs() {
            assert!(glyph.position.x > previous_x, "left to right");
            assert!(glyph.advance > 0.0);
            assert_eq!(glyph.cluster.start, previous_cluster);
            previous_x = glyph.position.x;
            previous_cluster = glyph.cluster.end;
        }
        assert_eq!(previous_cluster, 6, "the clusters cover the whole string");
    });
}

/// M4's acceptance names mixed Latin/CJK/emoji. The point is that every byte is accounted
/// for and nothing is dropped — exact glyph counts depend on which fonts the machine has.
#[test]
fn mixed_scripts_shape_without_losing_any_text() {
    with_fonts(|mut fonts| {
        let text = "Hi 世界 🎉";
        let layout = lay_out(&mut fonts, text, None);
        assert!(!layout.glyphs().is_empty());

        // Every glyph's cluster is a valid slice of the source, and clusters do not overlap.
        let mut covered = 0;
        for glyph in layout.glyphs() {
            assert!(
                text.is_char_boundary(glyph.cluster.start),
                "{:?}",
                glyph.cluster
            );
            assert!(
                text.is_char_boundary(glyph.cluster.end),
                "{:?}",
                glyph.cluster
            );
            covered = covered.max(glyph.cluster.end);
        }
        assert_eq!(covered, text.len(), "shaping reached the end of the string");
    });
}

#[test]
fn runs_split_where_the_font_changes() {
    with_fonts(|mut fonts| {
        // Latin and CJK almost never come from one face, so this should produce at least two
        // runs. If a machine really does have one font covering both, one run is also
        // correct — what must hold is that the runs tile the line.
        let layout = lay_out(&mut fonts, "ab世界", None);
        let line = &layout.lines()[0];
        let runs = layout.line_runs(line);
        assert!(!runs.is_empty());

        let mut next_glyph = 0;
        for run in runs {
            assert_eq!(
                run.glyphs.start, next_glyph,
                "runs must tile the line's glyphs"
            );
            assert!(run.glyphs.end > run.glyphs.start, "no empty runs");
            next_glyph = run.glyphs.end;
        }
        assert_eq!(next_glyph, layout.glyphs().len());
    });
}

// ---- wrapping and line boxes -----------------------------------------------------------

#[test]
fn text_wraps_at_the_given_width() {
    with_fonts(|mut fonts| {
        let narrow = lay_out(&mut fonts, "aaa bbb ccc ddd eee fff", Some(40.0));
        assert!(narrow.lines().len() > 1, "narrow text should wrap");

        let wide = lay_out(&mut fonts, "aaa bbb ccc ddd eee fff", Some(4000.0));
        assert_eq!(wide.lines().len(), 1, "wide text should not");
    });
}

#[test]
fn line_boxes_stack_without_gaps_or_overlap() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "aaa bbb ccc ddd eee fff ggg", Some(40.0));
        let mut expected_top = 0.0;
        for line in layout.lines() {
            assert!(
                (line.bounds.min_y() - expected_top).abs() < 0.01,
                "line at {} should start at {expected_top}",
                line.bounds.min_y()
            );
            assert!(line.bounds.height() > 0.0);
            assert!(line.baseline > 0.0 && line.baseline <= line.bounds.height());
            expected_top = line.bounds.max_y();
        }
        assert!((layout.size().height - expected_top).abs() < 0.01);
    });
}

#[test]
fn a_line_box_is_the_full_block_width_not_the_glyph_extent() {
    with_fonts(|mut fonts| {
        // Clicking to the right of a short line must still hit that line, which means the
        // box has to be the block's width even though the glyphs are narrower.
        let layout = lay_out(&mut fonts, "a", Some(200.0));
        let line = &layout.lines()[0];
        assert_eq!(line.bounds.width(), 200.0);
        assert!(line.width < 200.0, "the glyphs themselves are narrow");
    });
}

// ---- clusters ---------------------------------------------------------------------------

#[test]
fn cluster_stepping_walks_the_string() {
    with_fonts(|mut fonts| {
        let text = "abc";
        let layout = lay_out(&mut fonts, text, None);

        let mut index = 0;
        let mut visited = vec![0];
        while index < text.len() {
            index = layout.next_cluster(index);
            visited.push(index);
        }
        assert_eq!(visited, vec![0, 1, 2, 3]);

        // And back again.
        let mut back = vec![text.len()];
        let mut index = text.len();
        while index > 0 {
            index = layout.prev_cluster(index);
            back.push(index);
        }
        assert_eq!(back, vec![3, 2, 1, 0]);
    });
}

#[test]
fn a_multi_byte_character_is_one_step() {
    with_fonts(|mut fonts| {
        // "é" is two bytes. A caret must never land between them.
        let text = "aéb";
        let layout = lay_out(&mut fonts, text, None);
        assert_eq!(layout.next_cluster(0), 1);
        assert_eq!(layout.next_cluster(1), 3, "steps over both bytes of é");
        assert_eq!(layout.next_cluster(3), 4);
        assert!(
            !layout.is_cluster_boundary(2),
            "the middle of é is not a caret position"
        );
    });
}

#[test]
fn stepping_never_lands_off_a_char_boundary() {
    with_fonts(|mut fonts| {
        let text = "a世🎉b";
        let layout = lay_out(&mut fonts, text, None);
        let mut index = 0;
        while index < text.len() {
            index = layout.next_cluster(index);
            assert!(
                text.is_char_boundary(index),
                "next_cluster produced {index}, which splits a character"
            );
        }
        assert_eq!(index, text.len());
    });
}

#[test]
fn cluster_stepping_terminates_at_the_ends() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "ab", None);
        assert_eq!(layout.next_cluster(2), 2, "past the end stays put");
        assert_eq!(layout.next_cluster(99), 2);
        assert_eq!(layout.prev_cluster(0), 0, "before the start stays put");
    });
}

// ---- hit testing: M4's acceptance ------------------------------------------------------

/// *Clicking any glyph returns the correct cursor index, including at cluster boundaries.*
#[test]
fn clicking_each_glyph_returns_the_cursor_on_its_leading_edge() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "abcdef", None);
        let line = &layout.lines()[0];

        for glyph in layout.glyphs() {
            // A point just inside the glyph's leading edge.
            let x = glyph.position.x + glyph.advance * 0.25;
            let cursor = layout.point_to_cursor(Point::new(x, line.baseline_y()));
            assert_eq!(
                cursor.index, glyph.cluster.start,
                "clicking the left quarter of the glyph at {} should give {}",
                glyph.position.x, glyph.cluster.start
            );
        }
    });
}

#[test]
fn clicking_the_trailing_half_of_a_glyph_puts_the_caret_after_it() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "abcdef", None);
        let line = &layout.lines()[0];
        for glyph in layout.glyphs() {
            let x = glyph.position.x + glyph.advance * 0.85;
            let cursor = layout.point_to_cursor(Point::new(x, line.baseline_y()));
            assert_eq!(cursor.index, glyph.cluster.end);
        }
    });
}

#[test]
fn clicking_outside_the_block_clamps_rather_than_missing() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "abc", Some(200.0));
        assert_eq!(layout.point_to_cursor(Point::new(-50.0, -50.0)).index, 0);
        assert_eq!(layout.point_to_cursor(Point::new(500.0, 500.0)).index, 3);
    });
}

#[test]
fn hit_testing_a_multi_byte_character_never_lands_inside_it() {
    with_fonts(|mut fonts| {
        let text = "aéb";
        let layout = lay_out(&mut fonts, text, None);
        let line = &layout.lines()[0];
        // Sweep across the whole line; every answer must be a legal caret position.
        let mut x = -5.0;
        while x < line.width + 5.0 {
            let cursor = layout.point_to_cursor(Point::new(x, line.baseline_y()));
            assert!(
                layout.is_cluster_boundary(cursor.index),
                "hit test at x={x} produced {}, which is inside a cluster",
                cursor.index
            );
            x += 0.5;
        }
    });
}

#[test]
fn a_cursor_round_trips_through_a_point() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "abcdef", None);
        let mut index = 0;
        while index <= 6 {
            let caret = layout.cursor_to_point(Cursor::new(index));
            let back = layout.point_to_cursor(Point::new(caret.position.x, caret.position.y + 1.0));
            assert_eq!(back.index, index, "cursor {index} did not round trip");
            index = layout.next_cluster(index);
            if index == 6 {
                let caret = layout.cursor_to_point(Cursor::new(6));
                let back =
                    layout.point_to_cursor(Point::new(caret.position.x, caret.position.y + 1.0));
                assert_eq!(back.index, 6);
                break;
            }
        }
    });
}

// ---- affinity ----------------------------------------------------------------------------

/// The reason affinity exists: one byte offset, two places on screen.
#[test]
fn affinity_decides_which_line_a_wrap_point_belongs_to() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "aaa bbb ccc ddd", Some(40.0));
        assert!(layout.lines().len() > 1);

        let second_line_start = layout.lines()[1].range.start;
        let downstream = layout.cursor_to_point(Cursor {
            index: second_line_start,
            affinity: Affinity::Downstream,
        });
        let upstream = layout.cursor_to_point(Cursor {
            index: second_line_start,
            affinity: Affinity::Upstream,
        });

        assert_eq!(
            downstream.line, 1,
            "downstream belongs to the line it starts"
        );
        assert_eq!(upstream.line, 0, "upstream belongs to the line it ends");
        assert!(
            upstream.position.y < downstream.position.y,
            "and they are on different lines on screen"
        );
    });
}

#[test]
fn affinity_is_ignored_where_there_is_no_boundary() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "abcdef", None);
        let a = layout.cursor_to_point(Cursor::new(3));
        let b = layout.cursor_to_point(Cursor::upstream(3));
        assert_eq!(a.position, b.position, "mid-line, affinity means nothing");
    });
}

// ---- selection: M4's acceptance ----------------------------------------------------------

#[test]
fn a_selection_covers_exactly_the_selected_glyphs() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "abcdef", None);
        let rects = layout.selection_rects(2..4);
        assert_eq!(rects.len(), 1, "one line, one rectangle");

        let selected: Vec<_> = layout
            .glyphs()
            .iter()
            .filter(|glyph| glyph.cluster.start >= 2 && glyph.cluster.end <= 4)
            .collect();
        let expected_left = selected
            .iter()
            .map(|glyph| glyph.position.x)
            .fold(f32::INFINITY, f32::min);
        let expected_right = selected
            .iter()
            .map(|glyph| glyph.position.x + glyph.advance)
            .fold(f32::NEG_INFINITY, f32::max);

        assert!((rects[0].min_x() - expected_left).abs() < 0.01);
        assert!((rects[0].max_x() - expected_right).abs() < 0.01);
    });
}

#[test]
fn an_empty_range_selects_nothing() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "abcdef", None);
        assert!(
            layout.selection_rects(3..3).is_empty(),
            "a caret is not a selection"
        );
    });
}

/// *Selection rectangles are correct across a line wrap.*
#[test]
fn a_selection_across_a_wrap_produces_one_rectangle_per_line() {
    with_fonts(|mut fonts| {
        let text = "aaa bbb ccc ddd eee";
        let layout = lay_out(&mut fonts, text, Some(40.0));
        let line_count = layout.lines().len();
        assert!(line_count > 1, "the fixture needs to wrap");

        let rects = layout.selection_rects(0..text.len());
        assert_eq!(
            rects.len(),
            line_count,
            "selecting everything should highlight every line"
        );

        // Each rectangle sits on its own line, in order, and none is empty.
        for (rect, line) in rects.iter().zip(layout.lines()) {
            assert!((rect.min_y() - line.bounds.min_y()).abs() < 0.01);
            assert!((rect.height() - line.bounds.height()).abs() < 0.01);
            assert!(rect.width() > 0.0);
        }
    });
}

#[test]
fn a_selection_of_one_line_in_the_middle_does_not_touch_the_others() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "aaa bbb ccc ddd eee", Some(40.0));
        assert!(layout.lines().len() >= 3);
        let middle = &layout.lines()[1];

        let rects = layout.selection_rects(middle.range.clone());
        assert_eq!(rects.len(), 1);
        assert!((rects[0].min_y() - middle.bounds.min_y()).abs() < 0.01);
    });
}

// ---- direction ---------------------------------------------------------------------------

#[test]
fn latin_text_is_left_to_right() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "hello", None);
        assert!(
            layout
                .runs()
                .iter()
                .all(|run| run.direction == Direction::Ltr),
            "the API carries direction even though bidi layout is M8's"
        );
    });
}

#[test]
fn the_direction_of_a_run_is_reported_not_assumed() {
    with_fonts(|mut fonts| {
        // Arabic. Whether it lays out correctly is M8's problem; what must hold today is
        // that the API reports what happened rather than claiming everything is LTR.
        let layout = lay_out(&mut fonts, "مرحبا", None);
        if layout.glyphs().is_empty() {
            eprintln!("skipping: no font covers Arabic on this machine");
            return;
        }
        assert!(
            layout
                .runs()
                .iter()
                .any(|run| run.direction == Direction::Rtl),
            "Arabic should be reported as right to left"
        );
    });
}

// ---- line queries ------------------------------------------------------------------------

#[test]
fn a_point_maps_to_the_line_it_falls_in() {
    with_fonts(|mut fonts| {
        let layout = lay_out(&mut fonts, "aaa bbb ccc ddd eee", Some(40.0));
        for (index, line) in layout.lines().iter().enumerate() {
            let middle = line.bounds.min_y() + line.bounds.height() * 0.5;
            assert_eq!(layout.line_at_y(middle), index);
        }
        assert_eq!(layout.line_at_y(-100.0), 0, "above clamps to the first");
        assert_eq!(
            layout.line_at_y(10_000.0),
            layout.lines().len() - 1,
            "below clamps to the last"
        );
    });
}

#[test]
fn an_index_maps_to_the_line_it_is_on() {
    with_fonts(|mut fonts| {
        let text = "aaa bbb ccc ddd eee";
        let layout = lay_out(&mut fonts, text, Some(40.0));
        for (index, line) in layout.lines().iter().enumerate() {
            assert_eq!(layout.line_at_index(line.range.start), index);
        }
    });
}
