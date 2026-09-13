//! Promise ordering.
//!
//! §M12: *"job queue ordering must match spec or async code misbehaves in ways that look like
//! race conditions."* Nothing here is concurrent — every job runs to completion on one thread —
//! so the bugs look like races only because the order is observable and the code depending on
//! it never says so.
//!
//! These record the order as a list rather than asserting "it happened", because a test that
//! only checks the end state passes on every wrong ordering.

use std::cell::RefCell;
use std::rc::Rc;

use crisol_builtins::{Agent, Outcome, State};
use crisol_value::Value;

/// A shared log the reactions write to.
type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

fn read(log: &Log) -> Vec<String> {
    log.borrow().clone()
}

/// A reaction that records `name` and passes the value on.
fn note(log: &Log, name: &'static str) -> crisol_builtins::Reaction {
    let log = Rc::clone(log);
    Box::new(move |_, value| {
        log.borrow_mut().push(name.to_owned());
        Outcome::Fulfilled(value)
    })
}

// ---- always asynchronous ---------------------------------------------------------------

#[test]
fn then_on_a_settled_promise_still_waits_for_a_microtask() {
    // `Promise.resolve(1).then(f)` must not call `f` before `then` returns. Code that relied
    // on the synchronous case would work until the promise happened to be pending, which is
    // precisely the intermittent failure §M12 warns about.
    let log = log();
    let mut agent = Agent::new();
    let settled = agent.resolved(Value::number(1.0));
    agent.then(settled, Some(note(&log, "handler")), None);

    assert!(
        read(&log).is_empty(),
        "nothing runs before the queue is drained"
    );
    assert_eq!(agent.queued(), 1);
    agent.run_microtasks();
    assert_eq!(read(&log), ["handler"]);
}

#[test]
fn a_handler_attached_before_settling_also_waits() {
    let log = log();
    let mut agent = Agent::new();
    let promise = agent.promise();
    agent.then(promise, Some(note(&log, "handler")), None);
    assert_eq!(agent.queued(), 0, "nothing to run while pending");

    agent.fulfil(promise, Value::number(1.0));
    assert!(read(&log).is_empty(), "settling queues, it does not call");
    agent.run_microtasks();
    assert_eq!(read(&log), ["handler"]);
}

// ---- the interleaving that catches ordering bugs ---------------------------------------

#[test]
fn two_chains_interleave_step_by_step() {
    // The canonical one:
    //   Promise.resolve().then(a1).then(a2)
    //   Promise.resolve().then(b1).then(b2)
    // gives a1, b1, a2, b2 — not a1, a2, b1, b2. A queue that ran a chain to completion
    // before starting the next would produce the second, and every `await`-heavy program
    // would subtly change behaviour.
    let log = log();
    let mut agent = Agent::new();

    let a = agent.resolved(Value::UNDEFINED);
    let a1 = agent.then(a, Some(note(&log, "a1")), None);
    agent.then(a1, Some(note(&log, "a2")), None);

    let b = agent.resolved(Value::UNDEFINED);
    let b1 = agent.then(b, Some(note(&log, "b1")), None);
    agent.then(b1, Some(note(&log, "b2")), None);

    agent.run_microtasks();
    assert_eq!(read(&log), ["a1", "b1", "a2", "b2"]);
}

#[test]
fn jobs_queued_by_jobs_run_in_the_same_drain() {
    // "Microtasks run to completion" — which is also why an endless `.then` chain starves the
    // event loop rather than yielding to it. Specified behaviour, not an oversight.
    let log = log();
    let mut agent = Agent::new();
    let start = agent.resolved(Value::UNDEFINED);

    let inner = Rc::clone(&log);
    agent.then(
        start,
        Some(Box::new(move |agent, value| {
            inner.borrow_mut().push("outer".to_owned());
            let nested = agent.resolved(Value::UNDEFINED);
            let deeper = Rc::clone(&inner);
            agent.then(
                nested,
                Some(Box::new(move |_, value| {
                    deeper.borrow_mut().push("inner".to_owned());
                    Outcome::Fulfilled(value)
                })),
                None,
            );
            Outcome::Fulfilled(value)
        })),
        None,
    );

    let ran = agent.run_microtasks();
    assert_eq!(read(&log), ["outer", "inner"]);
    assert!(ran >= 2, "both jobs ran in one drain, not {ran}");
    assert_eq!(agent.queued(), 0);
}

// ---- pass-through, which is where a rejection gets lost ---------------------------------

#[test]
fn a_rejection_passes_through_a_then_that_only_handles_fulfilment() {
    // The bug this guards: forwarding a rejection as a *fulfilment* means
    // `p.then(onFulfilled).catch(handler)` never reaches the catch, and the program carries on
    // with an Error where it expected data.
    let log = log();
    let mut agent = Agent::new();
    let failed = agent.rejected(Value::number(7.0));

    let forwarded = agent.then(failed, Some(note(&log, "should-not-run")), None);
    let caught = Rc::clone(&log);
    let end = agent.then(
        forwarded,
        None,
        Some(Box::new(move |_, reason| {
            caught.borrow_mut().push("caught".to_owned());
            Outcome::Fulfilled(reason)
        })),
    );

    agent.run_microtasks();
    assert_eq!(read(&log), ["caught"], "the fulfil handler must be skipped");
    assert_eq!(agent.state(end), State::Fulfilled(Value::number(7.0)));
}

#[test]
fn a_fulfilment_passes_through_a_catch_that_only_handles_rejection() {
    let log = log();
    let mut agent = Agent::new();
    let ok = agent.resolved(Value::number(1.0));

    let through = agent.then(ok, None, Some(note(&log, "should-not-run")));
    let end = agent.then(through, Some(note(&log, "then")), None);

    agent.run_microtasks();
    assert_eq!(read(&log), ["then"]);
    assert_eq!(agent.state(end), State::Fulfilled(Value::number(1.0)));
}

#[test]
fn a_handler_that_rejects_rejects_the_derived_promise() {
    // What `throw` inside a `.then` does.
    let mut agent = Agent::new();
    let ok = agent.resolved(Value::number(1.0));
    let derived = agent.then(
        ok,
        Some(Box::new(|_, _| Outcome::Rejected(Value::number(9.0)))),
        None,
    );
    agent.run_microtasks();
    assert_eq!(agent.state(derived), State::Rejected(Value::number(9.0)));
}

// ---- settling once ----------------------------------------------------------------------

#[test]
fn settling_is_one_shot() {
    // What makes it safe to hand both `resolve` and `reject` to code that might call either
    // twice, which is most of what a promise is for.
    let mut agent = Agent::new();
    let promise = agent.promise();
    agent.fulfil(promise, Value::number(1.0));
    agent.fulfil(promise, Value::number(2.0));
    agent.reject(promise, Value::number(3.0));
    assert_eq!(agent.state(promise), State::Fulfilled(Value::number(1.0)));
}

#[test]
fn a_rejection_first_wins_too() {
    let mut agent = Agent::new();
    let promise = agent.promise();
    agent.reject(promise, Value::number(1.0));
    agent.fulfil(promise, Value::number(2.0));
    assert_eq!(agent.state(promise), State::Rejected(Value::number(1.0)));
}

#[test]
fn a_handler_runs_once_even_if_settled_twice() {
    let log = log();
    let mut agent = Agent::new();
    let promise = agent.promise();
    agent.then(promise, Some(note(&log, "once")), None);
    agent.fulfil(promise, Value::UNDEFINED);
    agent.fulfil(promise, Value::UNDEFINED);
    agent.run_microtasks();
    assert_eq!(read(&log), ["once"]);
}

// ---- adoption --------------------------------------------------------------------------

#[test]
fn adopting_another_promise_follows_its_eventual_state() {
    let mut agent = Agent::new();
    let inner = agent.promise();
    let outer = agent.promise();
    agent.adopt(outer, inner);

    agent.run_microtasks();
    assert_eq!(agent.state(outer), State::Pending, "inner has not settled");

    agent.fulfil(inner, Value::number(5.0));
    agent.run_microtasks();
    assert_eq!(agent.state(outer), State::Fulfilled(Value::number(5.0)));
}

#[test]
fn adopting_a_rejection_rejects() {
    let mut agent = Agent::new();
    let inner = agent.promise();
    let outer = agent.promise();
    agent.adopt(outer, inner);
    agent.reject(inner, Value::number(4.0));
    agent.run_microtasks();
    assert_eq!(agent.state(outer), State::Rejected(Value::number(4.0)));
}

#[test]
fn adopting_costs_a_tick_so_it_lands_after_a_plain_value() {
    // Returning a promise from `.then` settles later than returning a value. This is the
    // mechanism behind the famous `await` ordering puzzles — and the reason a refactor that
    // changes `return x` to `return Promise.resolve(x)` can reorder unrelated code.
    let log = log();
    let mut agent = Agent::new();

    let slow_source = agent.resolved(Value::UNDEFINED);
    let slow = agent.then(
        slow_source,
        Some(Box::new(move |agent, _| {
            let inner = agent.resolved(Value::UNDEFINED);
            Outcome::Adopt(inner)
        })),
        None,
    );
    agent.then(slow, Some(note(&log, "adopted")), None);

    let fast = agent.resolved(Value::UNDEFINED);
    let fast1 = agent.then(fast, Some(note(&log, "plain-1")), None);
    agent.then(fast1, Some(note(&log, "plain-2")), None);

    agent.run_microtasks();
    let order = read(&log);
    let adopted = order
        .iter()
        .position(|name| name == "adopted")
        .expect("ran");
    let plain2 = order
        .iter()
        .position(|name| name == "plain-2")
        .expect("ran");
    assert!(
        adopted > plain2,
        "adoption costs at least one extra tick, so it lands after the plain chain: {order:?}"
    );
}

#[test]
fn a_promise_cannot_adopt_itself() {
    // The spec makes this a TypeError. Without it, the promise simply never settles — silent,
    // and indistinguishable from a pending network call.
    let mut agent = Agent::new();
    let promise = agent.promise();
    agent.adopt(promise, promise);
    agent.run_microtasks();
    assert!(
        matches!(agent.state(promise), State::Rejected(_)),
        "resolving a promise with itself must fail loudly"
    );
}

// ---- the queue itself --------------------------------------------------------------------

#[test]
fn the_queue_can_be_drained_a_step_at_a_time() {
    let log = log();
    let mut agent = Agent::new();
    let start = agent.resolved(Value::UNDEFINED);
    let first = agent.then(start, Some(note(&log, "one")), None);
    agent.then(first, Some(note(&log, "two")), None);

    assert_eq!(agent.run_microtasks_up_to(1), 1);
    assert_eq!(read(&log), ["one"]);
    agent.run_microtasks();
    assert_eq!(read(&log), ["one", "two"]);
}

#[test]
fn draining_an_empty_queue_does_nothing() {
    let mut agent = Agent::new();
    assert_eq!(agent.run_microtasks(), 0);
}
