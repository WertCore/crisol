//! The iterator protocol.
//!
//! An iterator is any object with a `next` that returns `{ value, done }`. Three details in
//! that sentence do real work and each is a place implementations diverge.
//!
//! # `done` is coerced, not compared
//!
//! `IteratorComplete` calls `ToBoolean` on `result.done`. So:
//!
//! | `done` | means |
//! |---|---|
//! | `undefined` (absent) | not done |
//! | `0`, `""`, `NaN` | **not done** |
//! | `"false"` | **done** |
//! | `[]`, `{}` | **done** |
//!
//! `{ done: "false" }` ending the loop is not a joke — `"false"` is a non-empty string and
//! therefore truthy (D-65). An implementation comparing `done === true` would loop forever on
//! an iterator that returns `{ done: 1 }`, and one comparing `done == true` would disagree in
//! different places again.
//!
//! # `value` is `undefined` when absent, including on the last step
//!
//! `{ done: true }` with no `value` is the normal way to finish. The final result's `value` is
//! the *return value* of a generator, not an element — which is why `for…of` never sees it and
//! `yield*` does.
//!
//! # Closing
//!
//! Leaving a loop early — `break`, `return`, or a throw — calls the iterator's `return` method
//! if it has one. That is how a generator's `finally` runs and how a file handle behind an
//! iterator gets closed. Skipping it leaks, and the leak is invisible because the happy path
//! never exercises it.

use crisol_value::Value;

use crate::convert::to_boolean;

/// One step's result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    /// The element, or the return value on the final step.
    pub value: Value,
    /// Whether iteration has finished.
    pub done: bool,
}

impl Step {
    /// An element.
    #[must_use]
    pub const fn item(value: Value) -> Self {
        Self { value, done: false }
    }

    /// The end, carrying a return value.
    #[must_use]
    pub const fn finished(value: Value) -> Self {
        Self { value, done: true }
    }

    /// The end, carrying nothing.
    #[must_use]
    pub const fn done() -> Self {
        Self {
            value: Value::UNDEFINED,
            done: true,
        }
    }
}

/// Reads a raw `{ value, done }` pair the way `IteratorComplete` and `IteratorValue` do.
///
/// `done` goes through `ToBoolean`, which is the whole point: `{ done: "false" }` is finished
/// and `{ done: 0 }` is not.
#[must_use]
pub fn read_step(value: Value, done: Value) -> Step {
    Step {
        value,
        done: to_boolean(done),
    }
}

/// An iterator whose steps come from Rust.
///
/// A real iterator's `next` is a JavaScript function, which this crate cannot call — the same
/// limit as [`crate::Got::Getter`]. What is worth having now is the *protocol*: exhaustion,
/// closing, and the rule that a finished iterator stays finished. Those are testable with
/// steps supplied from here and do not change when the caller becomes an interpreter.
#[derive(Debug)]
pub struct StepIterator {
    steps: Vec<Step>,
    at: usize,
    /// Set once a `done` step has been produced or `return` has been called.
    finished: bool,
    /// How many times `return` was called. A closed iterator should be closed once.
    closes: usize,
}

impl StepIterator {
    /// An iterator over `values`, finishing after the last.
    #[must_use]
    pub fn over(values: &[Value]) -> Self {
        Self {
            steps: values.iter().map(|value| Step::item(*value)).collect(),
            at: 0,
            finished: false,
            closes: 0,
        }
    }

    /// An iterator producing exactly these steps, however malformed.
    ///
    /// For the cases a well-behaved iterator would not produce — a `done` step followed by more
    /// elements, say — which is exactly what the protocol has to survive.
    #[must_use]
    pub fn of_steps(steps: Vec<Step>) -> Self {
        Self {
            steps,
            at: 0,
            finished: false,
            closes: 0,
        }
    }

    /// The protocol's `next()`.
    ///
    /// Named `step` rather than `next` on purpose: Rust's `Iterator::next` returns
    /// `Option<T>`, where `None` is the end. This returns a `{ value, done }` pair where the
    /// final step **carries a value** — a generator's return value — and the two are not
    /// interchangeable. A reader who saw `next` here would reasonably assume otherwise.
    ///
    /// **A finished iterator stays finished.** Once a `done` step has come out, every later
    /// call reports done again, even if the underlying sequence has more in it. Without that,
    /// an iterator that was exhausted and then asked again would restart — and the loop
    /// protocol has no way to notice.
    pub fn step(&mut self) -> Step {
        if self.finished {
            return Step::done();
        }
        let Some(step) = self.steps.get(self.at).copied() else {
            self.finished = true;
            return Step::done();
        };
        self.at += 1;
        if step.done {
            self.finished = true;
        }
        step
    }

    /// `iterator.return()` — what leaving a loop early calls.
    ///
    /// Idempotent: closing an already-closed iterator is not an error and does not run the
    /// cleanup twice.
    pub fn close(&mut self) -> Step {
        if !self.finished {
            self.finished = true;
            self.closes += 1;
        }
        Step::done()
    }

    /// How many times cleanup actually ran.
    #[must_use]
    pub const fn closes(&self) -> usize {
        self.closes
    }

    /// Whether it has finished, by exhaustion or closing.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }
}

/// Drains an iterator into a list, the way spreading does.
#[must_use]
pub fn collect(iterator: &mut StepIterator) -> Vec<Value> {
    let mut out = Vec::new();
    loop {
        let step = iterator.step();
        if step.done {
            // The final `value` is a return value, not an element. `for…of` and spread both
            // discard it; `yield*` is the one place it is visible.
            return out;
        }
        out.push(step.value);
    }
}

/// Takes at most `count` elements and then **closes** the iterator.
///
/// The `break` path. Closing here is what runs a generator's `finally` and releases whatever
/// the iterator was holding; skipping it leaks, and the leak is invisible because the happy
/// path never exercises it.
pub fn take(iterator: &mut StepIterator, count: usize) -> Vec<Value> {
    let mut out = Vec::new();
    while out.len() < count {
        let step = iterator.step();
        if step.done {
            return out;
        }
        out.push(step.value);
    }
    iterator.close();
    out
}

/// An async iterator: every step is a promise of a [`Step`].
///
/// `for await (const x of it)` awaits each result, so the protocol is the synchronous one with
/// a promise wrapped around every answer. Two consequences that are easy to miss:
///
/// - **`done` is still coerced**, and it is coerced *after* the promise settles. A promise that
///   fulfils with `{ done: "false" }` finishes the loop.
/// - **A rejected step ends the iteration**, and the rejection propagates rather than being
///   treated as "no more items". Swallowing it would turn a failed network page into a quietly
///   truncated list, which is the failure that looks like success.
///
/// Steps come from Rust for the same reason as [`StepIterator`] — a real one's `next` is a
/// JavaScript function. What is worth having now is the ordering, which does not change.
#[derive(Debug)]
pub struct AsyncStepIterator {
    inner: StepIterator,
}

impl AsyncStepIterator {
    /// An async iterator over `values`.
    #[must_use]
    pub fn over(values: &[Value]) -> Self {
        Self {
            inner: StepIterator::over(values),
        }
    }

    /// From explicit steps.
    #[must_use]
    pub fn of_steps(steps: Vec<Step>) -> Self {
        Self {
            inner: StepIterator::of_steps(steps),
        }
    }

    /// The protocol's `next()`, as a promise that is already fulfilled.
    ///
    /// Already-fulfilled and **still asynchronous**: the handler attached to it runs as a
    /// microtask, not before `next` returns (D-61). That is what makes `for await` yield to the
    /// queue on every iteration even when nothing actually waits.
    pub fn step(&mut self, agent: &mut crate::promise::Agent) -> crate::promise::PromiseId {
        let step = self.inner.step();
        // The value is carried on the promise; `done` is read from the step it settles with.
        agent.resolved(step.value)
    }

    /// The step behind the promise, for a caller that has already awaited it.
    pub fn step_value(&mut self) -> Step {
        self.inner.step()
    }

    /// `asyncIterator.return()` — what leaving a `for await` early calls.
    pub fn close(&mut self) -> Step {
        self.inner.close()
    }

    /// How many times cleanup ran.
    #[must_use]
    pub const fn closes(&self) -> usize {
        self.inner.closes()
    }

    /// Whether it has finished.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }
}
