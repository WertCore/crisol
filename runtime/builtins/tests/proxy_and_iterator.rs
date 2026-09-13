//! `Proxy` invariants and the iterator protocol.
//!
//! The proxy tests are all of the same shape: build a trap that **lies about the target** and
//! assert the lie is refused. An implementation with the traps and without the checks passes
//! every test that *uses* a proxy and fails the ones that try to break one — which is the
//! direction real code exercises only after someone has shipped a bug.

use std::collections::HashMap;

use crisol_builtins::{
    Property, Proxy, ProxyError, Step, StepIterator, Target, Trap, collect, read_step, take,
};
use crisol_value::{PropertyKey, Value};

fn key(name: &str) -> PropertyKey {
    PropertyKey::new(name)
}

fn n(value: f64) -> Value {
    Value::number(value)
}

/// A target built by hand, so the invariant checks can be tested without a whole realm.
#[derive(Default)]
struct Fake {
    properties: HashMap<String, Property>,
    extensible: bool,
}

impl Fake {
    fn new() -> Self {
        Self {
            properties: HashMap::new(),
            extensible: true,
        }
    }

    fn with(mut self, name: &str, property: Property) -> Self {
        self.properties.insert(name.to_owned(), property);
        self
    }

    fn sealed(mut self) -> Self {
        self.extensible = false;
        self
    }
}

impl Target for Fake {
    fn own_property(&self, key: &PropertyKey) -> Option<Property> {
        self.properties.get(key.as_str()).cloned()
    }
    fn is_extensible(&self) -> bool {
        self.extensible
    }
}

/// A property nothing may change or remove.
fn locked(value: Value) -> Property {
    Property::Data {
        value,
        writable: false,
        enumerable: true,
        configurable: false,
    }
}

/// Present, but still configurable — a proxy may say what it likes about this one.
fn open(value: Value) -> Property {
    Property::data(value)
}

// ---- the invariants -------------------------------------------------------------------------

#[test]
fn get_may_not_misreport_a_frozen_property() {
    // The target made a promise: this value cannot change. Every piece of code that reasoned
    // about `Object.freeze` — including the engine's own optimiser — depends on a proxy being
    // unable to break it.
    let proxy = Proxy::new(Fake::new().with("x", locked(n(1.0))));
    assert_eq!(proxy.checked_get(&key("x"), n(1.0)), Ok(n(1.0)));

    let lie = proxy.checked_get(&key("x"), n(2.0));
    assert!(
        matches!(
            lie,
            Err(ProxyError::InvariantViolated {
                trap: Trap::Get,
                ..
            })
        ),
        "{lie:?}"
    );
}

#[test]
fn get_may_say_anything_about_a_configurable_property() {
    // The latitude is the point of a proxy. Only the promises are protected.
    let proxy = Proxy::new(Fake::new().with("x", open(n(1.0))));
    assert_eq!(proxy.checked_get(&key("x"), n(99.0)), Ok(n(99.0)));
    assert_eq!(
        proxy.checked_get(&key("invented"), n(7.0)),
        Ok(n(7.0)),
        "and about properties the target does not have at all"
    );
}

#[test]
fn set_may_not_claim_to_have_changed_a_frozen_property() {
    let proxy = Proxy::new(Fake::new().with("x", locked(n(1.0))));
    assert_eq!(
        proxy.checked_set(&key("x"), n(1.0), true),
        Ok(true),
        "writing the value it already has is not a change"
    );
    assert!(matches!(
        proxy.checked_set(&key("x"), n(2.0), true),
        Err(ProxyError::InvariantViolated {
            trap: Trap::Set,
            ..
        })
    ));
    assert_eq!(
        proxy.checked_set(&key("x"), n(2.0), false),
        Ok(false),
        "reporting failure is always allowed"
    );
}

#[test]
fn has_may_not_hide_a_non_configurable_property() {
    // Denying the existence of something that provably cannot be removed.
    let proxy = Proxy::new(Fake::new().with("x", locked(n(1.0))));
    assert!(matches!(
        proxy.checked_has(&key("x"), false),
        Err(ProxyError::InvariantViolated {
            trap: Trap::Has,
            ..
        })
    ));
    assert_eq!(proxy.checked_has(&key("x"), true), Ok(true));

    let loose = Proxy::new(Fake::new().with("x", open(n(1.0))));
    assert_eq!(
        loose.checked_has(&key("x"), false),
        Ok(false),
        "a configurable property may be hidden"
    );
}

#[test]
fn delete_may_not_claim_to_have_removed_a_non_configurable_property() {
    let proxy = Proxy::new(Fake::new().with("x", locked(n(1.0))));
    assert!(matches!(
        proxy.checked_delete(&key("x"), true),
        Err(ProxyError::InvariantViolated {
            trap: Trap::DeleteProperty,
            ..
        })
    ));
    assert_eq!(proxy.checked_delete(&key("x"), false), Ok(false));
}

#[test]
fn a_descriptor_trap_may_not_deny_a_non_configurable_property() {
    let proxy = Proxy::new(Fake::new().with("x", locked(n(1.0))));
    assert!(matches!(
        proxy.checked_descriptor(&key("x"), None),
        Err(ProxyError::InvariantViolated {
            trap: Trap::GetOwnPropertyDescriptor,
            ..
        })
    ));
    assert!(
        proxy
            .checked_descriptor(&key("x"), Some(locked(n(1.0))))
            .is_ok()
    );
}

#[test]
fn is_extensible_has_no_latitude_at_all() {
    // Unlike the property traps, where a proxy may invent properties, this one must match
    // exactly. `Object.isExtensible` is how code decides whether a shape can still change, and
    // a proxy that lied would invalidate that reasoning everywhere, including inside the engine.
    let open_target = Proxy::new(Fake::new());
    assert_eq!(open_target.checked_is_extensible(true), Ok(true));
    assert!(matches!(
        open_target.checked_is_extensible(false),
        Err(ProxyError::InvariantViolated {
            trap: Trap::IsExtensible,
            ..
        })
    ));

    let sealed = Proxy::new(Fake::new().sealed());
    assert_eq!(sealed.checked_is_extensible(false), Ok(false));
    assert!(sealed.checked_is_extensible(true).is_err());
}

#[test]
fn own_keys_must_list_every_non_configurable_key() {
    let proxy = Proxy::new(Fake::new().with("locked", locked(n(1.0))));
    let target_keys = [key("locked")];

    assert!(proxy.checked_own_keys(&target_keys, &target_keys).is_ok());
    assert!(matches!(
        proxy.checked_own_keys(&[], &target_keys),
        Err(ProxyError::InvariantViolated {
            trap: Trap::OwnKeys,
            ..
        })
    ));
}

#[test]
fn own_keys_on_a_sealed_target_must_match_exactly() {
    // Nothing can be added and nothing removed, so invention is as forbidden as omission.
    let proxy = Proxy::new(Fake::new().with("a", open(n(1.0))).sealed());
    let target_keys = [key("a")];

    assert!(proxy.checked_own_keys(&target_keys, &target_keys).is_ok());
    assert!(
        proxy
            .checked_own_keys(&[key("a"), key("invented")], &target_keys)
            .is_err(),
        "invented keys are refused"
    );
    assert!(
        proxy.checked_own_keys(&[], &target_keys).is_err(),
        "and so are missing ones"
    );
}

// ---- revocation ------------------------------------------------------------------------------

#[test]
fn a_revoked_proxy_refuses_everything() {
    let mut proxy = Proxy::new(Fake::new().with("x", open(n(1.0))));
    assert!(proxy.checked_get(&key("x"), n(1.0)).is_ok());

    proxy.revoke();
    assert!(proxy.is_revoked());
    assert_eq!(
        proxy.checked_get(&key("x"), n(1.0)),
        Err(ProxyError::Revoked)
    );
    assert_eq!(proxy.checked_has(&key("x"), true), Err(ProxyError::Revoked));
    assert_eq!(proxy.checked_is_extensible(true), Err(ProxyError::Revoked));
    assert_eq!(
        proxy.checked_delete(&key("x"), true),
        Err(ProxyError::Revoked)
    );
}

#[test]
fn revocation_is_checked_before_the_invariants() {
    // Revocation exists to detach the handler, so a revoked proxy must not reach it — even to
    // report a violation. A revoked proxy with a lying trap is revoked, not invalid.
    let mut proxy = Proxy::new(Fake::new().with("x", locked(n(1.0))));
    proxy.revoke();
    assert_eq!(
        proxy.checked_get(&key("x"), n(999.0)),
        Err(ProxyError::Revoked),
        "not an invariant violation"
    );
}

// ---- the iterator protocol ---------------------------------------------------------------------

#[test]
fn done_is_coerced_not_compared() {
    // `IteratorComplete` calls ToBoolean. `{ done: "false" }` finishes the loop, because
    // "false" is a non-empty string.
    assert!(
        !read_step(n(1.0), Value::UNDEFINED).done,
        "absent means not done"
    );
    assert!(!read_step(n(1.0), n(0.0)).done, "0 is falsy");
    assert!(!read_step(n(1.0), Value::FALSE).done);
    assert!(read_step(n(1.0), Value::TRUE).done);
    assert!(read_step(n(1.0), n(1.0)).done, "1 is truthy");
    assert!(
        read_step(
            n(1.0),
            Value::object(crisol_value::Address::new(8).expect("in range"))
        )
        .done,
        "an object is truthy, so a `done` of {{}} finishes"
    );
}

#[test]
fn a_finished_iterator_stays_finished() {
    // Without this, an exhausted iterator asked again would restart, and the loop protocol has
    // no way to notice.
    let mut iterator =
        StepIterator::of_steps(vec![Step::item(n(1.0)), Step::done(), Step::item(n(2.0))]);
    assert_eq!(iterator.step().value, n(1.0));
    assert!(iterator.step().done);
    assert!(
        iterator.step().done,
        "the element after the done step is unreachable"
    );
    assert!(iterator.is_finished());
}

#[test]
fn draining_discards_the_final_value() {
    // The last result's `value` is a generator's return value, not an element. `for…of` and
    // spread both discard it; `yield*` is the one place it shows.
    let mut iterator = StepIterator::of_steps(vec![
        Step::item(n(1.0)),
        Step::item(n(2.0)),
        Step::finished(n(99.0)),
    ]);
    assert_eq!(collect(&mut iterator), [n(1.0), n(2.0)]);
}

#[test]
fn leaving_early_closes_the_iterator() {
    // The `break` path. Closing is what runs a generator's `finally` and releases whatever the
    // iterator held; skipping it leaks, invisibly, because the happy path never exercises it.
    let mut iterator = StepIterator::over(&[n(1.0), n(2.0), n(3.0)]);
    assert_eq!(take(&mut iterator, 2), [n(1.0), n(2.0)]);
    assert_eq!(iterator.closes(), 1, "cleanup ran");
    assert!(iterator.is_finished());
}

#[test]
fn running_to_the_end_does_not_close() {
    // Exhaustion is not early exit: `return` is called only when the loop leaves early.
    let mut iterator = StepIterator::over(&[n(1.0), n(2.0)]);
    assert_eq!(take(&mut iterator, 5), [n(1.0), n(2.0)]);
    assert_eq!(iterator.closes(), 0);
}

#[test]
fn closing_twice_runs_cleanup_once() {
    let mut iterator = StepIterator::over(&[n(1.0)]);
    iterator.close();
    iterator.close();
    assert_eq!(iterator.closes(), 1);
}

#[test]
fn an_empty_iterator_finishes_immediately() {
    let mut iterator = StepIterator::over(&[]);
    assert!(iterator.step().done);
    assert!(collect(&mut StepIterator::over(&[])).is_empty());
}
