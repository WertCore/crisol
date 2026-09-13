//! `JSON.parse` and `JSON.stringify`.
//!
//! Most of these are about what parsing **refuses**. JSON looks like a JavaScript literal and
//! is not, and every difference is a place where being lenient turns a clear error at the
//! boundary into corrupt data further in.
//!
//! Non-ASCII appears as escapes rather than as literal characters, so that a diff of this file
//! is readable in a terminal that has opinions about fonts.

use crisol_builtins::{Json, parse, stringify};

fn round_trip(text: &str) -> String {
    stringify(&parse(text).expect("parses"), 0)
}

// ---- what it accepts --------------------------------------------------------------------

#[test]
fn scalars_round_trip() {
    for text in ["null", "true", "false", "0", "-1", "1.5", "1e3", "\"x\""] {
        let value = parse(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        let _ = stringify(&value, 0);
    }
    assert_eq!(round_trip("null"), "null");
    assert_eq!(round_trip("true"), "true");
    assert_eq!(round_trip("-1"), "-1");
}

#[test]
fn structures_round_trip_and_keep_their_order() {
    // A round trip must not reorder a document. Sorting keys would quietly rewrite every file
    // that passed through.
    let text = r#"{"b":1,"a":2,"c":{"z":[1,2,3],"y":{}}}"#;
    assert_eq!(round_trip(text), text);
}

#[test]
fn empty_containers() {
    assert_eq!(round_trip("[]"), "[]");
    assert_eq!(round_trip("{}"), "{}");
    assert_eq!(round_trip("[[],{}]"), "[[],{}]");
}

#[test]
fn whitespace_between_tokens_is_allowed() {
    assert_eq!(round_trip("  {\n\t\"a\" :\r 1 }  "), r#"{"a":1}"#);
}

#[test]
fn a_repeated_key_keeps_the_last_value_in_the_first_position() {
    assert_eq!(round_trip(r#"{"a":1,"b":2,"a":3}"#), r#"{"a":3,"b":2}"#);
}

// ---- what it refuses ---------------------------------------------------------------------

#[test]
fn javascript_literal_syntax_that_json_does_not_allow_is_refused() {
    let cases = [
        ("[1,]", "trailing comma in an array"),
        ("{\"a\":1,}", "trailing comma in an object"),
        ("{a:1}", "unquoted key"),
        ("{'a':1}", "single-quoted key"),
        ("'x'", "single-quoted string"),
        ("[1] // comment", "a comment"),
        ("+1", "leading plus"),
        ("01", "leading zero"),
        ("1.", "trailing decimal point"),
        (".5", "leading decimal point"),
        ("0x10", "hexadecimal"),
        ("NaN", "NaN"),
        ("Infinity", "Infinity"),
        ("undefined", "undefined"),
        ("[1 2]", "missing comma"),
        ("{\"a\" 1}", "missing colon"),
        ("", "empty input"),
        ("[1][2]", "trailing content"),
    ];
    for (text, why) in cases {
        assert!(
            parse(text).is_err(),
            "{text:?} should be refused ({why}), and was accepted"
        );
    }
}

#[test]
fn a_raw_control_character_in_a_string_is_refused() {
    // A literal newline between the quotes is an error, not a newline.
    assert!(parse("\"a\nb\"").is_err());
    assert!(parse("\"a\tb\"").is_err());
    assert!(parse("\"a\\nb\"").is_ok(), "the escape is fine");
}

#[test]
fn an_unterminated_string_is_refused() {
    assert!(parse("\"abc").is_err());
    assert!(parse("\"abc\\").is_err());
}

// ---- escapes -----------------------------------------------------------------------------

#[test]
fn every_escape_is_understood() {
    let parsed = parse(r#""\" \\ \/ \b \f \n \r \t""#).expect("parses");
    assert_eq!(
        parsed,
        Json::String("\" \\ / \u{8} \u{c} \n \r \t".to_owned())
    );
}

#[test]
fn a_unicode_escape_becomes_its_character() {
    assert_eq!(
        parse(r#""A""#).expect("parses"),
        Json::String("A".to_owned())
    );
    assert_eq!(
        parse(r#""é""#).expect("parses"),
        Json::String("\u{e9}".to_owned())
    );
}

#[test]
fn a_surrogate_pair_is_joined_into_one_character() {
    // Without joining, the two halves become replacement characters and the text is silently
    // corrupted -- an emoji turns into two question marks somewhere downstream.
    let parsed = parse(r#""😀""#).expect("parses");
    assert_eq!(parsed, Json::String("\u{1F600}".to_owned()));
}

#[test]
fn a_lone_surrogate_is_refused() {
    assert!(
        parse(r#""\ud83d""#).is_err(),
        "a high half with nothing after"
    );
    assert!(parse(r#""\ude00""#).is_err(), "a low half on its own");
    assert!(
        parse(r#""\ud83dA""#).is_err(),
        "a high half followed by something that is not its partner"
    );
}

#[test]
fn a_truncated_unicode_escape_is_refused() {
    assert!(parse(r#""\u12""#).is_err());
    assert!(parse(r#""\uZZZZ""#).is_err());
}

// ---- output ------------------------------------------------------------------------------

#[test]
fn control_characters_are_escaped_on_the_way_out() {
    let value = Json::String("a\u{1}b\nc".to_owned());
    // U+0001 has no short escape, so it takes the long form; newline has one and uses it.
    assert_eq!(stringify(&value, 0), "\"a\\u0001b\\nc\"");
}

#[test]
fn a_forward_slash_is_not_escaped() {
    // Legal either way, and escaping it makes output differ from every other implementation
    // for no benefit.
    assert_eq!(stringify(&Json::String("a/b".to_owned()), 0), r#""a/b""#);
}

#[test]
fn whole_numbers_print_without_a_decimal_point() {
    assert_eq!(stringify(&Json::Number(1.0), 0), "1");
    assert_eq!(stringify(&Json::Number(-42.0), 0), "-42");
    assert_eq!(stringify(&Json::Number(1.5), 0), "1.5");
}

#[test]
fn negative_zero_becomes_zero_and_the_sign_is_lost() {
    // JSON has no negative zero, so a round trip loses it. Specified, and a genuine
    // information loss worth knowing about rather than discovering.
    assert_eq!(stringify(&Json::Number(-0.0), 0), "0");
}

#[test]
fn not_a_number_and_the_infinities_become_null() {
    // JSON cannot write them, and emitting `NaN` would produce output no other parser accepts.
    assert_eq!(stringify(&Json::Number(f64::NAN), 0), "null");
    assert_eq!(stringify(&Json::Number(f64::INFINITY), 0), "null");
    assert_eq!(stringify(&Json::Number(f64::NEG_INFINITY), 0), "null");
}

#[test]
fn indentation_nests() {
    let value = parse(r#"{"a":[1,2],"b":{}}"#).expect("parses");
    let expected = "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}";
    assert_eq!(stringify(&value, 2), expected);
}

#[test]
fn indented_output_parses_back_to_the_same_thing() {
    let original = parse(r#"{"a":[1,{"b":"c"}],"d":null}"#).expect("parses");
    let text = stringify(&original, 4);
    assert_eq!(parse(&text).expect("re-parses"), original);
}

// ---- accessors ---------------------------------------------------------------------------

#[test]
fn a_document_can_be_looked_into() {
    let value = parse(r#"{"a":{"b":[10,20]}}"#).expect("parses");
    let inner = value
        .get("a")
        .and_then(|value| value.get("b"))
        .expect("found");
    assert_eq!(inner.at(1), Some(&Json::Number(20.0)));
    assert_eq!(value.get("missing"), None);
    assert_eq!(value.at(0), None, "an object has no index");
}

#[test]
fn an_error_says_where() {
    let error = parse("[1, 2, oops]").expect_err("refused");
    assert!(
        error.at > 0,
        "the offset should point at the problem: {error}"
    );
    assert!(!error.message.is_empty());
}
