//! `String`, and the places UTF-16 shows through.
//!
//! The recurring theme: **length counts code units, iteration yields code points**, and every
//! index-taking method works in code units. Slicing at an odd boundary splits an emoji in half
//! and that is specified, not a bug to route around.

use crisol_builtins::JsString;

/// The grinning face, U+1F600 — two UTF-16 code units.
const EMOJI: &str = "\u{1F600}";

fn s(text: &str) -> JsString {
    JsString::from(text)
}

// ---- what a string is ------------------------------------------------------------------------

#[test]
fn length_counts_code_units_not_characters() {
    assert_eq!(s("abc").length(), 3);
    assert_eq!(s(EMOJI).length(), 2, "one character, two code units");
    assert_eq!(s("e\u{301}").length(), 2, "e plus a combining accent");
}

#[test]
fn iteration_yields_code_points() {
    // The split that catches everyone: `"emoji".length` is 2 and `[..."emoji"].length` is 1.
    let emoji = s(EMOJI);
    assert_eq!(emoji.length(), 2);
    assert_eq!(emoji.code_points(), [0x1_F600], "one code point");
}

#[test]
fn splitting_on_empty_string_tears_a_surrogate_pair_in_half() {
    // `"emoji".split("")` gives two broken halves. Specified, and the reason `[...string]`
    // exists as a separate way to take a string apart.
    let halves = s(EMOJI).split_units();
    assert_eq!(halves.len(), 2);
    assert!(
        !halves[0].is_well_formed(),
        "the first half is a lone high surrogate"
    );
    assert!(!halves[1].is_well_formed());
    assert_eq!(halves[0].to_rust(), None, "and cannot become Rust text");
}

#[test]
fn a_lone_surrogate_is_a_perfectly_ordinary_string() {
    // Rust's `String` cannot hold this at all, which is the whole reason the storage is
    // `Vec<u16>`: rejecting it or replacing it with U+FFFD corrupts data at the boundary.
    let lone = JsString::from_units(vec![0xD800]);
    assert_eq!(lone.length(), 1);
    assert!(!lone.is_well_formed());
    assert_eq!(lone.to_rust(), None, "no lossy substitution");
    assert_eq!(
        lone.to_rust_lossy(),
        "\u{FFFD}",
        "only the diagnostic path replaces it"
    );
}

#[test]
fn code_unit_and_code_point_differ_at_a_surrogate_pair() {
    let emoji = s(EMOJI);
    assert_eq!(emoji.code_unit_at(0), Some(0xD83D), "the high half alone");
    assert_eq!(emoji.code_point_at(0), Some(0x1_F600), "both halves joined");
    assert_eq!(
        emoji.code_point_at(1),
        Some(0xDE00),
        "starting mid-pair gives the low half, not the character"
    );
}

// ---- slice versus substring ---------------------------------------------------------------------

#[test]
fn slice_does_not_swap_its_arguments_and_substring_does() {
    // Two differences that make the pair a reliable source of bugs when one is substituted for
    // the other.
    let text = s("abcdef");
    assert_eq!(text.slice(4, 1).to_rust().as_deref(), Some(""));
    assert_eq!(text.substring(4, 1).to_rust().as_deref(), Some("bcd"));
}

#[test]
fn slice_counts_negatives_from_the_end_and_substring_clamps_them() {
    let text = s("abcdef");
    assert_eq!(text.slice(-3, 6).to_rust().as_deref(), Some("def"));
    assert_eq!(
        text.substring(-3, 6).to_rust().as_deref(),
        Some("abcdef"),
        "a negative clamps to zero rather than counting back"
    );
}

#[test]
fn slicing_mid_pair_produces_a_lone_surrogate() {
    // An implementation that snapped indices to code-point boundaries would return different
    // strings than every engine, and `slice` would stop composing with `indexOf`.
    let text = s(EMOJI);
    let half = text.slice(0, 1);
    assert_eq!(half.length(), 1);
    assert!(!half.is_well_formed());
}

#[test]
fn at_takes_negative_indices() {
    let text = s("abc");
    assert_eq!(text.at(0), Some(u16::from(b'a')));
    assert_eq!(text.at(-1), Some(u16::from(b'c')));
    assert_eq!(text.at(3), None);
    assert_eq!(text.at(-4), None, "past the front");
}

// ---- searching -----------------------------------------------------------------------------------

#[test]
fn index_of_reports_absence_as_none_rather_than_minus_one() {
    // JavaScript returns -1; `None` is used here so a caller cannot use the sentinel as an
    // index by accident.
    let text = s("hello world");
    assert_eq!(text.index_of(&s("world")), Some(6));
    assert_eq!(text.index_of(&s("nope")), None);
    assert_eq!(text.index_of(&s("")), Some(0), "the empty needle is at 0");
}

#[test]
fn prefix_and_suffix_checks() {
    let text = s("hello");
    assert!(text.starts_with(&s("he")));
    assert!(text.ends_with(&s("lo")));
    assert!(text.includes(&s("ell")));
    assert!(!text.starts_with(&s("lo")));
    assert!(text.starts_with(&s("")), "everything starts with nothing");
}

// ---- building ------------------------------------------------------------------------------------

#[test]
fn repeat_refuses_a_negative_count() {
    // A RangeError, not an empty string: returning "" would turn a caller's arithmetic mistake
    // into silently missing output.
    assert_eq!(
        s("ab").repeat(3).and_then(|r| r.to_rust()).as_deref(),
        Some("ababab")
    );
    assert_eq!(
        s("ab").repeat(0).and_then(|r| r.to_rust()).as_deref(),
        Some("")
    );
    assert_eq!(s("ab").repeat(-1), None);
}

#[test]
fn pad_start_truncates_the_filler_and_never_trims() {
    let text = s("5");
    assert_eq!(
        text.pad_start(4, &s("ab")).to_rust().as_deref(),
        Some("aba5"),
        "the filler is cut mid-pattern rather than dropped"
    );
    assert_eq!(
        text.pad_start(0, &s("x")).to_rust().as_deref(),
        Some("5"),
        "a shorter target leaves the string alone"
    );
    assert_eq!(
        text.pad_start(4, &s("")).to_rust().as_deref(),
        Some("5"),
        "an empty pad cannot fill anything"
    );
}

#[test]
fn concat_joins_code_units() {
    assert_eq!(s("ab").concat(&s("cd")).to_rust().as_deref(), Some("abcd"));

    // Two lone surrogates that happen to pair up become one real character.
    let high = JsString::from_units(vec![0xD83D]);
    let low = JsString::from_units(vec![0xDE00]);
    let joined = high.concat(&low);
    assert!(joined.is_well_formed());
    assert_eq!(joined.code_points(), [0x1_F600]);
}

// ---- trimming ------------------------------------------------------------------------------------

#[test]
fn trim_removes_the_specifications_whitespace_set() {
    assert_eq!(s("  a  ").trim().to_rust().as_deref(), Some("a"));
    assert_eq!(
        s("\t\n\r a \u{000B}\u{000C}").trim().to_rust().as_deref(),
        Some("a")
    );
    assert_eq!(
        s("\u{00A0}a\u{3000}").trim().to_rust().as_deref(),
        Some("a")
    );
    assert_eq!(s("   ").trim().to_rust().as_deref(), Some(""), "all space");
    assert_eq!(s("").trim().to_rust().as_deref(), Some(""));
}

#[test]
fn trim_removes_the_byte_order_mark() {
    // The one people miss: Unicode does not classify U+FEFF as whitespace and the spec trims it
    // anyway. A file that begins with one otherwise leaves an invisible character on the front
    // of the first field.
    assert_eq!(s("\u{FEFF}a").trim().to_rust().as_deref(), Some("a"));
    assert_eq!(s("\u{FEFF}").trim().length(), 0);
}

#[test]
fn trim_leaves_interior_space_alone() {
    assert_eq!(s(" a b ").trim().to_rust().as_deref(), Some("a b"));
}
