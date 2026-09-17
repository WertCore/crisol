//! `Reflect`, the `Boolean` wrapper, and async iteration.
//!
//! The theme of the `Reflect` tests is the **failure path**: an implementation that made
//! `Reflect.defineProperty` throw would still pass every test that defines a property
//! successfully. The difference only shows where the operation is refused, which is the path
//! people write least and rely on most.

use crisol_builtins::{
    Agent, AsyncStepIterator, BooleanObject, Got, PartialDescriptor, Realm, State, Step, boolean,
    reflect_ns as reflect,
};
use crisol_value::{PropertyKey, Value};

fn key(name: &str) -> PropertyKey {
    PropertyKey::new(name)
}

fn n(value: f64) -> Value {
    Value::number(value)
}

fn permissive(value: Value) -> PartialDescriptor {
    PartialDescriptor {
        value: Some(value),
        writable: Some(true),
        enumerable: Some(true),
        configurable: Some(true),
        ..PartialDescriptor::default()
    }
}

// ---- Reflect reports failure instead of throwing ------------------------------------------------

#[test]
fn define_property_reports_false_where_object_would_throw() {
    // The whole reason `Reflect` exists. Code that wants to *attempt* an operation and branch
    // on the outcome would otherwise have to wrap the `Object` form in a `try`, conflating
    // "not allowed" with "something went wrong inside a getter".
    let mut realm = Realm::new();
    let object = realm.create(None);
    assert!(reflect::define_property(
        &mut realm,
        object,
        key("x"),
        &permissive(n(1.0))
    ));

    realm.prevent_extensions(object);
    assert!(
        !reflect::define_property(&mut realm, object, key("new"), &permissive(n(2.0))),
        "refused, and says so rather than throwing"
    );
}

#[test]
fn set_reports_whether_it_worked_rather_than_the_value() {
    // Plain assignment evaluates to the value, so it cannot report failure at all — in sloppy
    // mode a refused write is silent.
    let mut realm = Realm::new();
    let object = realm.create(None);
    assert!(reflect::set(&mut realm, object, &key("x"), n(1.0)));

    realm.prevent_extensions(object);
    assert!(
        !reflect::set(&mut realm, object, &key("y"), n(2.0)),
        "a refused write is visible here"
    );
}

#[test]
fn delete_property_reports_a_refusal() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    // `PartialDescriptor::value` defaults every attribute to false, so this is non-configurable.
    realm
        .define_own_property(object, key("locked"), &PartialDescriptor::value(n(1.0)))
        .expect("defined");

    assert!(!reflect::delete_property(
        &mut realm,
        object,
        &key("locked")
    ));
    assert!(
        reflect::delete_property(&mut realm, object, &key("absent")),
        "deleting what was never there succeeds"
    );
}

#[test]
fn set_prototype_of_reports_a_refused_cycle() {
    let mut realm = Realm::new();
    let a = realm.create(None);
    let b = realm.create(Some(a));
    assert!(!reflect::set_prototype_of(&mut realm, a, Some(b)));
    assert!(
        reflect::set_prototype_of(&mut realm, a, None),
        "a no-op is allowed"
    );
}

// ---- Reflect mirrors the internal methods, not the helpers ----------------------------------------

#[test]
fn own_keys_includes_non_enumerable_properties() {
    // Unlike `Object.keys` — `Reflect.ownKeys` mirrors the internal method, not the iteration
    // helper.
    let mut realm = Realm::new();
    let object = realm.create(None);
    realm
        .define_own_property(object, key("shown"), &permissive(n(1.0)))
        .expect("defined");
    realm
        .define_own_property(
            object,
            key("hidden"),
            &PartialDescriptor {
                value: Some(n(2.0)),
                enumerable: Some(false),
                ..PartialDescriptor::default()
            },
        )
        .expect("defined");

    let all: Vec<String> = reflect::own_keys(&realm, object)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(all, ["shown", "hidden"]);

    let enumerable: Vec<String> = realm
        .enumerable_keys(object)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(enumerable, ["shown"], "Object.keys sees only one");
}

#[test]
fn has_walks_the_prototype_chain_like_the_in_operator() {
    let mut realm = Realm::new();
    let parent = realm.create(None);
    let child = realm.create(Some(parent));
    realm
        .define_own_property(parent, key("x"), &permissive(n(1.0)))
        .expect("defined");

    assert!(reflect::has(&realm, child, &key("x")), "inherited counts");
    assert!(
        reflect::own_property_descriptor(&realm, child, &key("x")).is_none(),
        "but it is not an own property"
    );
}

#[test]
fn get_hands_back_an_accessor_rather_than_calling_it() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    realm
        .define_own_property(
            object,
            key("x"),
            &PartialDescriptor {
                get: Some(Value::NULL),
                ..PartialDescriptor::default()
            },
        )
        .expect("defined");
    assert_eq!(
        reflect::get(&realm, object, &key("x")),
        Got::Getter(Value::NULL)
    );
}

#[test]
fn extensibility_round_trips() {
    let mut realm = Realm::new();
    let object = realm.create(None);
    assert!(reflect::is_extensible(&realm, object));
    assert!(reflect::prevent_extensions(&mut realm, object));
    assert!(!reflect::is_extensible(&realm, object));
    assert_eq!(reflect::prototype_of(&realm, object), None);
}

// ---- the Boolean wrapper -------------------------------------------------------------------------

#[test]
fn new_boolean_false_is_truthy() {
    // It follows from objects being truthy — the same rule that makes `if (obj)` a null check —
    // not from a special case for `Boolean`. The assertion reads as a mistake, which is exactly
    // why it is a test rather than a comment.
    let wrapper = BooleanObject::new(Value::FALSE);
    assert!(!wrapper.value_of(), "the wrapped primitive is false");
    assert!(wrapper.is_truthy(), "and the wrapper is still truthy");
}

#[test]
fn boolean_called_without_new_gives_the_primitive() {
    assert!(!boolean(Value::FALSE));
    assert!(!boolean(Value::UNDEFINED));
    assert!(!boolean(Value::number(0.0)));
    assert!(boolean(Value::TRUE));
    assert!(boolean(Value::number(1.0)));
}

// ---- async iteration ------------------------------------------------------------------------------

#[test]
fn every_async_step_is_a_promise_that_settles_on_the_queue() {
    // Already-fulfilled and still asynchronous: the handler runs as a microtask, not before
    // `step` returns (D-61). That is what makes `for await` yield on every iteration even when
    // nothing actually waits.
    let mut agent = Agent::new();
    let mut iterator = AsyncStepIterator::over(&[n(1.0), n(2.0)]);

    let promise = iterator.step(&mut agent);
    assert_eq!(agent.state(promise), State::Fulfilled(n(1.0)));
    assert_eq!(
        agent.queued(),
        0,
        "nothing queued until a handler is attached"
    );

    let second = iterator.step(&mut agent);
    assert_eq!(agent.state(second), State::Fulfilled(n(2.0)));
}

#[test]
fn an_async_iterator_finishes_and_stays_finished() {
    let mut iterator = AsyncStepIterator::over(&[n(1.0)]);
    assert!(!iterator.step_value().done);
    assert!(iterator.step_value().done);
    assert!(iterator.step_value().done, "and stays finished");
    assert!(iterator.is_finished());
}

#[test]
fn leaving_a_for_await_early_closes_it() {
    let mut iterator = AsyncStepIterator::over(&[n(1.0), n(2.0), n(3.0)]);
    let _ = iterator.step_value();
    iterator.close();
    assert_eq!(iterator.closes(), 1);
    assert!(iterator.is_finished());
}

#[test]
fn an_async_done_step_still_carries_its_value() {
    let mut iterator =
        AsyncStepIterator::of_steps(vec![Step::item(n(1.0)), Step::finished(n(9.0))]);
    assert_eq!(iterator.step_value().value, n(1.0));
    let last = iterator.step_value();
    assert!(last.done);
    assert_eq!(last.value, n(9.0), "the return value, not an element");
}
