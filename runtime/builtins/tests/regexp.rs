//! `RegExp`, and the mutable cursor that makes it surprising.
//!
//! Most of these are about `lastIndex`. `regress` does the matching; what a JavaScript engine
//! adds is a **cursor that lives on the object**, and that cursor is the reason a regex literal
//! with `g` behaves differently on its second call.

use crisol_builtins::{Flags, JsRegExp};

fn re(pattern: &str, flags: &str) -> JsRegExp {
    JsRegExp::new(pattern, Flags::parse(flags).expect("valid flags")).expect("valid pattern")
}

// ---- the cursor ------------------------------------------------------------------------------

#[test]
fn a_global_regex_alternates_across_repeated_tests() {
    // The canonical surprise:
    //   const r = /a/g;
    //   r.test("a")  // true  — lastIndex is now 1
    //   r.test("a")  // false — searching from 1 finds nothing, and resets to 0
    //   r.test("a")  // true  — again
    let mut r = re("a", "g");
    assert!(r.test("a"), "first call matches");
    assert_eq!(r.last_index(), 1, "and advances the cursor");
    assert!(!r.test("a"), "second call searches from 1 and misses");
    assert_eq!(r.last_index(), 0, "the miss resets the cursor");
    assert!(r.test("a"), "so the third call matches again");
}

#[test]
fn test_mutates_exactly_as_much_as_exec() {
    // `test` is `exec` with the result discarded. Implementing it as a stateless search would
    // make it disagree with `exec` on the same object.
    let mut by_test = re("a", "g");
    let mut by_exec = re("a", "g");
    by_test.test("aa");
    by_exec.exec("aa");
    assert_eq!(by_test.last_index(), by_exec.last_index());
}

#[test]
fn a_regex_without_g_or_y_ignores_the_cursor_entirely() {
    // `lastIndex` is inert without one of those flags: it can be assigned and it changes
    // nothing, which is its own source of confusion.
    let mut r = re("a", "");
    r.set_last_index(5);
    let found = r.exec("a").expect("matched despite lastIndex 5");
    assert_eq!(found.start, 0);
    assert_eq!(r.last_index(), 5, "and the assignment survives untouched");
}

#[test]
fn a_global_regex_walks_through_a_string() {
    let mut r = re("a", "g");
    assert_eq!(r.exec("aXa").expect("first").start, 0);
    assert_eq!(r.last_index(), 1);
    assert_eq!(r.exec("aXa").expect("second").start, 2);
    assert_eq!(r.last_index(), 3);
    assert!(r.exec("aXa").is_none(), "exhausted");
    assert_eq!(r.last_index(), 0, "and reset");
}

#[test]
fn sticky_anchors_at_the_cursor_rather_than_searching_from_it() {
    // The difference between `y` and `g`: a match found later in the string is not a match.
    let mut sticky = re("a", "y");
    assert!(!sticky.test("Xa"), "there is an `a`, but not at index 0");

    let mut global = re("a", "g");
    assert!(global.test("Xa"), "`g` searches forward and finds it");
}

#[test]
fn sticky_matches_when_the_cursor_points_at_it() {
    let mut r = re("a", "y");
    r.set_last_index(1);
    assert!(r.test("Xa"));
    assert_eq!(r.last_index(), 2);
}

#[test]
fn a_cursor_past_the_end_misses_and_resets() {
    let mut r = re("a", "g");
    r.set_last_index(99);
    assert!(!r.test("a"));
    assert_eq!(r.last_index(), 0);
}

// ---- flags ------------------------------------------------------------------------------------

#[test]
fn flags_report_in_the_specifications_order_not_the_source_order() {
    // `/x/yg.flags` is "gy". Echoing source order would make two equivalent regexes compare
    // unequal as strings.
    assert_eq!(Flags::parse("yg").expect("valid").to_text(), "gy");
    assert_eq!(Flags::parse("smi").expect("valid").to_text(), "ims");
    assert_eq!(Flags::parse("dgimsuy").expect("valid").to_text(), "dgimsuy");
}

#[test]
fn a_repeated_flag_is_refused() {
    // `/x/gg` is a SyntaxError, not something to ignore — accepting it lets a typo through.
    assert_eq!(Flags::parse("gg"), Err('g'));
    assert_eq!(Flags::parse("gig"), Err('g'));
}

#[test]
fn an_unknown_flag_is_refused() {
    assert_eq!(Flags::parse("q"), Err('q'));
    assert_eq!(Flags::parse("gq"), Err('q'));
}

#[test]
fn only_g_and_y_make_the_cursor_live() {
    assert!(Flags::parse("g").expect("valid").uses_last_index());
    assert!(Flags::parse("y").expect("valid").uses_last_index());
    assert!(!Flags::parse("i").expect("valid").uses_last_index());
    assert!(!Flags::parse("").expect("valid").uses_last_index());
}

// ---- matching ------------------------------------------------------------------------------------

#[test]
fn case_insensitivity_is_a_flag_not_a_pattern_rewrite() {
    assert!(re("abc", "i").test("ABC"));
    assert!(!re("abc", "").test("ABC"));
}

#[test]
fn dot_all_decides_whether_a_dot_crosses_a_line_break() {
    assert!(re("a.b", "s").test("a\nb"));
    assert!(!re("a.b", "").test("a\nb"));
}

#[test]
fn multiline_moves_the_anchors_to_line_boundaries() {
    assert!(re("^b", "m").test("a\nb"));
    assert!(!re("^b", "").test("a\nb"));
}

#[test]
fn a_group_that_did_not_participate_is_none_not_empty() {
    // `undefined` versus `""` in `exec`'s result — a real difference that a caller branches on.
    let mut r = re("(a)|(b)", "");
    let found = r.exec("a").expect("matched");
    assert_eq!(found.groups.len(), 2);
    assert!(found.groups[0].is_some(), "the taken branch captured");
    assert_eq!(found.groups[1], None, "the untaken branch did not");
}

#[test]
fn backreferences_work_which_is_why_regress_is_here() {
    // Rust's `regex` crate deliberately omits these. Real code uses them, so a "close enough"
    // engine would reject patterns that work in every browser.
    assert!(re(r"(\w)\1", "").test("aa"));
    assert!(!re(r"(\w)\1", "").test("ab"));
}

#[test]
fn lookahead_works_too() {
    assert!(re("a(?=b)", "").test("ab"));
    assert!(!re("a(?=b)", "").test("ac"));
    assert!(re("a(?!b)", "").test("ac"));
}

#[test]
fn an_invalid_pattern_is_reported_rather_than_panicking() {
    assert!(JsRegExp::new("(", Flags::default()).is_err());
    assert!(JsRegExp::new("a{2,1}", Flags::default()).is_err());
}

// ---- iterating every match -------------------------------------------------------------------------

#[test]
fn all_matches_walks_the_whole_string() {
    let mut r = re("a", "g");
    let found = r.all_matches("aXaXa");
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].start, 0);
    assert_eq!(found[1].start, 2);
    assert_eq!(found[2].start, 4);
}

#[test]
fn an_empty_match_advances_rather_than_looping_forever() {
    // `/(?:)/g` has an empty match at every position. Without the bump the cursor never moves.
    let mut r = re("", "g");
    let found = r.all_matches("ab");
    assert!(
        found.len() <= 3,
        "one per position plus the end, not an infinite list: {}",
        found.len()
    );
    assert!(!found.is_empty());
}

#[test]
fn the_advance_is_by_character_so_it_cannot_split_a_code_point() {
    // A byte-wise bump would land inside a multi-byte sequence and panic.
    let mut r = re("", "g");
    let found = r.all_matches("\u{1F600}b");
    assert!(!found.is_empty());
    for capture in &found {
        assert!(
            "\u{1F600}b".is_char_boundary(capture.start),
            "match at {} is not on a boundary",
            capture.start
        );
    }
}
