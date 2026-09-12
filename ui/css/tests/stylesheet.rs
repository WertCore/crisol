//! Turning CSS source into rules the cascade can apply.

use crisol_css::stylesheet::{Origin, Stylesheet};

/// The longhand name and serialized value of every declaration a rule produced.
fn values(sheet: &Stylesheet, rule: usize) -> Vec<(String, String)> {
    use lightningcss::printer::PrinterOptions;
    sheet.rules[rule]
        .declarations
        .iter()
        .map(|(property, _)| {
            (
                property.property_id().name().to_string(),
                property
                    .value_to_css_string(PrinterOptions::default())
                    .expect("a parsed value must serialize"),
            )
        })
        .collect()
}

fn value_of<'a>(values: &'a [(String, String)], name: &str) -> Option<&'a str> {
    values
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// The longhand names a rule produced, in order, with `!` on the important ones.
fn longhands(sheet: &Stylesheet, rule: usize) -> Vec<String> {
    sheet.rules[rule]
        .declarations
        .iter()
        .map(|(property, important)| {
            let name = property.property_id().name().to_string();
            if *important { format!("{name}!") } else { name }
        })
        .collect()
}

#[test]
fn a_simple_rule_keeps_its_selector_and_declarations() {
    let sheet = Stylesheet::parse("p { color: red; opacity: 0.5 }").unwrap();
    assert_eq!(sheet.rules.len(), 1);
    assert_eq!(sheet.origin, Origin::Author);
    assert_eq!(longhands(&sheet, 0), vec!["color", "opacity"]);
    assert!(sheet.warnings.is_empty(), "{:?}", sheet.warnings);
}

#[test]
fn rules_keep_their_source_order() {
    let sheet = Stylesheet::parse("a { color: red } b { color: blue } i { color: teal }").unwrap();
    let orders: Vec<_> = sheet.rules.iter().map(|rule| rule.source_order).collect();
    assert_eq!(orders, vec![0, 1, 2]);
}

// ---- shorthand expansion -------------------------------------------------------------

#[test]
fn a_shorthand_becomes_its_longhands() {
    let sheet = Stylesheet::parse("p { margin: 4px }").unwrap();
    assert_eq!(
        longhands(&sheet, 0),
        vec!["margin-top", "margin-right", "margin-bottom", "margin-left"],
        "the cascade has to compare like with like, so margin cannot stay a shorthand"
    );
}

#[test]
fn nested_shorthands_expand_all_the_way_down() {
    // `border` is a shorthand over width, style and colour, each of which is itself a
    // shorthand over four edges.
    let sheet = Stylesheet::parse("p { border: 1px solid red }").unwrap();
    let names = longhands(&sheet, 0);
    assert!(names.contains(&"border-top-width".to_string()), "{names:?}");
    assert!(
        names.contains(&"border-left-color".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"border-bottom-style".to_string()),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "border"),
        "no shorthand should survive: {names:?}"
    );
}

#[test]
fn a_later_longhand_overrides_an_earlier_shorthand() {
    // Within one rule, source order decides. The assertion is on the resulting values
    // rather than on how many declarations survive, because lightningcss is free to merge
    // the two into one shorthand and does.
    let sheet = Stylesheet::parse("p { margin: 4px; margin-top: 9px }").unwrap();
    let values = values(&sheet, 0);
    assert_eq!(value_of(&values, "margin-top"), Some("9px"));
    assert_eq!(value_of(&values, "margin-left"), Some("4px"));
    assert_eq!(value_of(&values, "margin-bottom"), Some("4px"));
}

#[test]
fn important_declarations_sort_after_normal_ones_within_a_rule() {
    let sheet = Stylesheet::parse("p { color: red !important; color: blue }").unwrap();
    assert_eq!(
        longhands(&sheet, 0),
        vec!["color", "color!"],
        "important has to win regardless of where it was written"
    );
}

// ---- error recovery ------------------------------------------------------------------

#[test]
fn an_unsupported_selector_costs_its_rule_and_nothing_else() {
    let sheet = Stylesheet::parse("p::before { color: red } p { color: blue }").unwrap();
    assert_eq!(sheet.rules.len(), 1, "the second rule must survive");
    assert_eq!(longhands(&sheet, 0), vec!["color"]);
    assert_eq!(sheet.warnings.len(), 1);
    assert!(
        sheet.warnings[0].message.contains("before"),
        "{:?}",
        sheet.warnings[0]
    );
    assert_eq!(sheet.warnings[0].line, 1);
}

#[test]
fn at_rules_are_reported_rather_than_dropped_silently() {
    let sheet = Stylesheet::parse("@media print { p { color: red } } p { color: blue }").unwrap();
    assert_eq!(sheet.rules.len(), 1);
    assert_eq!(sheet.warnings.len(), 1);
    assert!(sheet.warnings[0].message.contains("@media"));
}

#[test]
fn an_empty_rule_is_dropped() {
    let sheet = Stylesheet::parse("p { } a { color: red }").unwrap();
    assert_eq!(sheet.rules.len(), 1);
    assert_eq!(
        sheet.rules[0].source_order, 0,
        "source order skips dropped rules"
    );
}

#[test]
fn an_unknown_property_does_not_cost_the_rule() {
    let sheet = Stylesheet::parse("p { -x-nonsense: 1; color: red }").unwrap();
    assert_eq!(sheet.rules.len(), 1);
    let names = longhands(&sheet, 0);
    assert!(names.contains(&"color".to_string()), "{names:?}");
}

#[test]
fn a_comment_only_stylesheet_produces_nothing() {
    let sheet = Stylesheet::parse("/* nothing here */").unwrap();
    assert!(sheet.rules.is_empty());
    assert!(sheet.warnings.is_empty());
}

// ---- nesting -------------------------------------------------------------------------

#[test]
fn nested_rules_are_flattened_with_their_selectors_resolved() {
    let sheet = Stylesheet::parse(".card { color: red; & .title { color: blue } }").unwrap();
    assert_eq!(sheet.rules.len(), 2, "{:?}", sheet.warnings);
    assert_eq!(longhands(&sheet, 0), vec!["color"]);
    assert_eq!(longhands(&sheet, 1), vec!["color"]);
}

// ---- origin --------------------------------------------------------------------------

#[test]
fn the_origin_is_carried_for_the_cascade_to_sort_on() {
    let sheet = Stylesheet::parse_with_origin("p { color: red }", Origin::UserAgent).unwrap();
    assert_eq!(sheet.origin, Origin::UserAgent);
    assert!(Origin::UserAgent < Origin::Author, "author rules must win");
}
