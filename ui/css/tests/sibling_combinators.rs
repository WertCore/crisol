//! Whether a sheet can reach across a sibling boundary.
//!
//! Invalidation narrows itself on this answer, so a false negative is not a slower engine but
//! a wrong one: a rule that *can* match a following sibling, in a sheet that claims none can,
//! would stop being applied when the sibling's neighbour changed.

use crisol_css::stylesheet::Stylesheet;

fn uses(css: &str) -> bool {
    Stylesheet::parse(css)
        .expect("parses")
        .uses_sibling_combinators
}

#[test]
fn the_two_sibling_combinators_are_found() {
    assert!(uses(".a + .b { color: red }"), "adjacent");
    assert!(uses(".a ~ .b { color: red }"), "general");
}

#[test]
fn the_combinators_that_do_not_cross_siblings_are_not() {
    assert!(!uses(".a .b { color: red }"), "descendant");
    assert!(!uses(".a > .b { color: red }"), "child");
    assert!(!uses(".a.b { color: red }"), "compound");
    assert!(!uses("div:hover { color: red }"), "a plain state rule");
}

#[test]
fn a_sibling_combinator_nested_in_a_functional_selector_still_counts() {
    // The case that would make the narrowing unsound if it were missed: the `+` is real, it
    // is just wearing brackets.
    assert!(uses(":is(.a + .b) { color: red }"), "inside :is()");
    assert!(uses(":where(.a ~ .b) { color: red }"), "inside :where()");
    assert!(uses(":not(.a + .b) { color: red }"), "inside :not()");
}

#[test]
fn one_rule_among_many_is_enough() {
    assert!(
        uses(".a { color: red } .b { color: blue } .c + .d { color: green }"),
        "the sheet is only as narrow as its widest rule"
    );
}

#[test]
fn an_empty_sheet_claims_nothing() {
    assert!(!uses(""));
}
