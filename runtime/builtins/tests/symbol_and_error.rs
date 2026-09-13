//! `Symbol` and the `Error` hierarchy.

use crisol_builtins::{ErrorKind, JsError, Symbols, WellKnown};
use crisol_value::Value;

// ---- Symbol ------------------------------------------------------------------------------

#[test]
fn two_symbols_with_the_same_description_are_different() {
    // The whole point of a symbol: the description is a label, not an identity.
    let mut symbols = Symbols::new();
    let one = symbols.create(Some("x".to_owned()));
    let other = symbols.create(Some("x".to_owned()));
    assert_ne!(one, other);
    assert_eq!(symbols.description(one), Some("x"));
    assert_eq!(symbols.description(other), Some("x"));
}

#[test]
fn no_description_is_not_the_same_as_an_empty_one() {
    // `Symbol().description` is `undefined`; `Symbol("").description` is `""`. Frequently
    // conflated, and the difference shows in `String(symbol)`.
    let mut symbols = Symbols::new();
    let bare = symbols.create(None);
    let empty = symbols.create(Some(String::new()));
    assert_eq!(symbols.description(bare), None);
    assert_eq!(symbols.description(empty), Some(""));
    assert_eq!(symbols.to_display(bare), "Symbol()");
    assert_eq!(symbols.to_display(empty), "Symbol()");
}

#[test]
fn the_registry_returns_the_same_symbol_for_the_same_key() {
    let mut symbols = Symbols::new();
    let first = symbols.for_key("shared");
    let again = symbols.for_key("shared");
    assert_eq!(first, again, "Symbol.for is idempotent");
    assert_eq!(symbols.key_for(first), Some("shared"));
}

#[test]
fn an_unregistered_symbol_has_no_key() {
    let mut symbols = Symbols::new();
    let private = symbols.create(Some("shared".to_owned()));
    let registered = symbols.for_key("shared");
    assert_ne!(private, registered, "the description does not register it");
    assert_eq!(symbols.key_for(private), None);
}

#[test]
fn well_known_symbols_are_shared_without_being_registered() {
    // The row that catches people. They are shared across realms, which looks like registry
    // behaviour, but `Symbol.keyFor(Symbol.iterator)` is `undefined` — and putting them in the
    // registry would make `Symbol.for("Symbol.iterator")` return the real one, which is the
    // collision the separate namespace exists to prevent.
    let mut symbols = Symbols::new();
    let iterator = symbols.well_known(WellKnown::Iterator);
    assert_eq!(symbols.key_for(iterator), None, "not in the registry");

    let impostor = symbols.for_key("Symbol.iterator");
    assert_ne!(impostor, iterator, "and the registry cannot reach it");
}

#[test]
fn every_well_known_symbol_is_distinct_and_stable() {
    let symbols = Symbols::new();
    let mut seen = Vec::new();
    for known in WellKnown::ALL {
        let id = symbols.well_known(known);
        assert!(!seen.contains(&id), "{known:?} collides with another");
        seen.push(id);
        assert_eq!(
            symbols.well_known(known),
            id,
            "{known:?} must not move between lookups"
        );
        assert_eq!(symbols.description(id), Some(known.description()));
    }
    assert_eq!(seen.len(), 13);
}

// ---- Error -------------------------------------------------------------------------------

#[test]
fn every_error_inherits_from_error() {
    // Why `catch (e) { if (e instanceof Error) }` catches all of them. An implementation that
    // gave each kind an independent prototype would pass every construction test and fail
    // every real catch block.
    for kind in ErrorKind::ALL {
        assert!(kind.is_a(ErrorKind::Error), "{kind:?} is an Error");
    }
}

#[test]
fn error_is_not_a_type_error() {
    // The relation only goes one way.
    assert!(!ErrorKind::Error.is_a(ErrorKind::Type));
    assert!(ErrorKind::Type.is_a(ErrorKind::Type));
    assert!(
        !ErrorKind::Range.is_a(ErrorKind::Type),
        "siblings are unrelated"
    );
}

#[test]
fn the_names_are_the_specified_ones() {
    assert_eq!(ErrorKind::Error.name(), "Error");
    assert_eq!(ErrorKind::Type.name(), "TypeError");
    assert_eq!(ErrorKind::Range.name(), "RangeError");
    assert_eq!(ErrorKind::Reference.name(), "ReferenceError");
    assert_eq!(ErrorKind::Syntax.name(), "SyntaxError");
    assert_eq!(ErrorKind::Eval.name(), "EvalError");
    // Capitalised URI, not Uri — it is an initialism in the spec and in every engine.
    assert_eq!(ErrorKind::Uri.name(), "URIError");
    assert_eq!(ErrorKind::Aggregate.name(), "AggregateError");
}

#[test]
fn to_string_joins_name_and_message_only_when_both_exist() {
    // The separator appears only when both halves do, which is what every stack trace shows.
    let with = JsError::new(ErrorKind::Type, "boom");
    assert_eq!(with.to_display(), "TypeError: boom");

    let without = JsError::new(ErrorKind::Type, "");
    assert_eq!(without.to_display(), "TypeError", "no trailing colon");
}

#[test]
fn message_is_an_own_property_only_when_it_is_not_empty() {
    // `new Error()` inherits `""` from the prototype rather than owning one, which shows up in
    // `Object.keys`.
    assert!(!JsError::new(ErrorKind::Error, "").has_own_message());
    assert!(JsError::new(ErrorKind::Error, "x").has_own_message());
}

#[test]
fn cause_is_absent_unless_it_was_given() {
    // `{ cause: undefined }` does create the property, which is why this is an Option rather
    // than a Value defaulting to undefined.
    let plain = JsError::new(ErrorKind::Error, "x");
    assert_eq!(plain.cause, None);

    let caused = JsError::new(ErrorKind::Error, "x").with_cause(Value::UNDEFINED);
    assert_eq!(
        caused.cause,
        Some(Value::UNDEFINED),
        "given explicitly, even as undefined"
    );
}

#[test]
fn an_aggregate_carries_its_errors_and_others_do_not() {
    let inner = vec![Value::number(1.0), Value::number(2.0)];
    let aggregate = JsError::aggregate(inner.clone(), "several");
    assert_eq!(aggregate.kind, ErrorKind::Aggregate);
    assert_eq!(aggregate.errors, inner);
    assert!(aggregate.is_a(ErrorKind::Error));

    assert!(JsError::new(ErrorKind::Type, "x").errors.is_empty());
}

#[test]
fn an_error_displays_through_the_standard_traits() {
    let error = JsError::new(ErrorKind::Range, "out of range");
    assert_eq!(error.to_string(), "RangeError: out of range");
    let _: &dyn std::error::Error = &error;
}
