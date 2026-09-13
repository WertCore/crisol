//! Promises, and the microtask queue that orders them.
//!
//! ROADMAP §M12 singles this out: *"`Promise` semantics are subtle — job queue ordering must
//! match spec or async code misbehaves in ways that look like race conditions."* That is the
//! reason this is a separate module with its own tests rather than one builtin among many.
//!
//! # What "looks like a race condition" means
//!
//! Nothing here is concurrent. Every job runs to completion on one thread, in a fixed order.
//! The bugs look like races because the *order* is observable and the code that depends on it
//! never says so: two `.then` chains interleave in a particular way, and a program that assumed
//! otherwise fails intermittently on a different engine, or after an unrelated refactor moves
//! an `await`.
//!
//! So the tests here are almost all about order, and they record the order as a list rather
//! than asserting "it happened".
//!
//! # Reactions are Rust closures for now
//!
//! A reaction is ultimately a JavaScript function, and nothing in this crate can call one yet
//! (the same reason [`crate::Got::Getter`] hands getters back instead of invoking them). Here
//! that would make the module untestable rather than merely incomplete, because *ordering* is
//! the whole content — so a reaction is a Rust closure and the queue really runs it.
//!
//! When the interpreter lands, a job becomes "call this function" and the ordering logic below
//! does not change. That is the part worth getting right now.

use std::collections::VecDeque;

use crisol_value::Value;

/// Identifies a promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PromiseId(u32);

impl PromiseId {
    /// The index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// What a promise has settled to, if anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    /// Not settled.
    Pending,
    /// Settled with a value.
    Fulfilled(Value),
    /// Settled with a reason.
    Rejected(Value),
}

/// What a reaction produced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// Fulfil the derived promise.
    Fulfilled(Value),
    /// Reject it — what a `throw` inside a `.then` handler does.
    Rejected(Value),
    /// Adopt another promise's eventual state, as returning a promise from `.then` does.
    Adopt(PromiseId),
}

/// A handler, run as a microtask.
pub type Reaction = Box<dyn FnOnce(&mut Agent, Value) -> Outcome>;

struct Record {
    state: State,
    /// Handlers waiting on fulfilment, with the promise each one settles.
    on_fulfilled: Vec<(Option<Reaction>, PromiseId)>,
    /// Handlers waiting on rejection.
    on_rejected: Vec<(Option<Reaction>, PromiseId)>,
}

type Job = Box<dyn FnOnce(&mut Agent)>;

/// The agent: every promise, and the queue of jobs waiting to run.
///
/// One queue, drained to empty. There is no macrotask queue here — that is the host's (M15's),
/// and the ordering rule that matters between them is that **microtasks drain completely
/// before control returns**, which [`Agent::run_microtasks`] does by construction.
#[derive(Default)]
pub struct Agent {
    promises: Vec<Record>,
    queue: VecDeque<Job>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("promises", &self.promises.len())
            .field("queued", &self.queue.len())
            .finish()
    }
}

impl Agent {
    /// An agent with nothing in it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A new pending promise.
    pub fn promise(&mut self) -> PromiseId {
        let id = PromiseId(u32::try_from(self.promises.len()).expect("promises fit in u32"));
        self.promises.push(Record {
            state: State::Pending,
            on_fulfilled: Vec::new(),
            on_rejected: Vec::new(),
        });
        id
    }

    /// A promise already fulfilled with `value`, as `Promise.resolve` gives.
    pub fn resolved(&mut self, value: Value) -> PromiseId {
        let id = self.promise();
        self.fulfil(id, value);
        id
    }

    /// A promise already rejected with `reason`.
    pub fn rejected(&mut self, reason: Value) -> PromiseId {
        let id = self.promise();
        self.reject(id, reason);
        id
    }

    /// What a promise has settled to.
    #[must_use]
    pub fn state(&self, promise: PromiseId) -> State {
        self.promises
            .get(promise.0 as usize)
            .map_or(State::Pending, |record| record.state)
    }

    /// How many jobs are waiting.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Fulfils a promise, queueing its reactions.
    ///
    /// **Settling is one-shot.** A second call does nothing — not an error, nothing. That is
    /// what makes it safe to hand both `resolve` and `reject` to code that might call either
    /// twice, which is most of what a promise is for.
    pub fn fulfil(&mut self, promise: PromiseId, value: Value) {
        self.settle(promise, State::Fulfilled(value));
    }

    /// Rejects a promise, queueing its reactions.
    pub fn reject(&mut self, promise: PromiseId, reason: Value) {
        self.settle(promise, State::Rejected(reason));
    }

    fn settle(&mut self, promise: PromiseId, state: State) {
        let Some(record) = self.promises.get_mut(promise.0 as usize) else {
            return;
        };
        if !matches!(record.state, State::Pending) {
            return;
        }
        record.state = state;
        let (waiting, value) = match state {
            State::Fulfilled(value) => (std::mem::take(&mut record.on_fulfilled), value),
            State::Rejected(reason) => (std::mem::take(&mut record.on_rejected), reason),
            State::Pending => unreachable!("just set to a settled state"),
        };
        // The handlers for the *other* outcome are dropped: they will never run, and holding
        // them would keep whatever they close over alive for the life of the promise.
        if let Some(record) = self.promises.get_mut(promise.0 as usize) {
            record.on_fulfilled = Vec::new();
            record.on_rejected = Vec::new();
        }
        let rejected = matches!(state, State::Rejected(_));
        for (reaction, derived) in waiting {
            self.enqueue_reaction(reaction, value, derived, rejected);
        }
    }

    /// `promise.then(on_fulfilled, on_rejected)`, returning the derived promise.
    ///
    /// **Always asynchronous.** Attaching a handler to an already-settled promise queues a job
    /// rather than running it — `Promise.resolve(1).then(f)` does not call `f` before `then`
    /// returns. Code that relied on the synchronous case would work until the promise happened
    /// to be pending, which is the intermittent failure §M12 warns about.
    pub fn then(
        &mut self,
        promise: PromiseId,
        on_fulfilled: Option<Reaction>,
        on_rejected: Option<Reaction>,
    ) -> PromiseId {
        let derived = self.promise();
        match self.state(promise) {
            State::Pending => {
                if let Some(record) = self.promises.get_mut(promise.0 as usize) {
                    record.on_fulfilled.push((on_fulfilled, derived));
                    record.on_rejected.push((on_rejected, derived));
                }
            }
            State::Fulfilled(value) => self.enqueue_reaction(on_fulfilled, value, derived, false),
            State::Rejected(reason) => self.enqueue_reaction(on_rejected, reason, derived, true),
        }
        derived
    }

    /// Queues one reaction, or the pass-through when there is no handler.
    ///
    /// `rejected` says which list this came from, and it decides what a *missing* handler
    /// does. Passing a rejection through as a fulfilment is the bug that makes
    /// `p.then(onlyOnFulfilled).catch(handler)` never reach the `catch` — the rejection would
    /// arrive at the derived promise as a fulfilment carrying the error as its value, and the
    /// program would carry on with an `Error` where it expected data.
    fn enqueue_reaction(
        &mut self,
        reaction: Option<Reaction>,
        value: Value,
        derived: PromiseId,
        rejected: bool,
    ) {
        self.queue.push_back(Box::new(move |agent| match reaction {
            Some(handler) => match handler(agent, value) {
                Outcome::Fulfilled(value) => agent.fulfil(derived, value),
                Outcome::Rejected(reason) => agent.reject(derived, reason),
                Outcome::Adopt(other) => agent.adopt(derived, other),
            },
            // No handler: the settlement passes through *as it was*. This is what makes a
            // `.then(onFulfilled)` in the middle of a chain transparent to an error, and a
            // `.catch` transparent when nothing threw.
            None if rejected => agent.reject(derived, value),
            None => agent.fulfil(derived, value),
        }));
    }

    /// Makes `derived` follow `other`.
    ///
    /// What returning a promise from a `.then` handler does. It costs an extra tick — the
    /// adoption itself is a job — which is why `return promise` resolves one microtask later
    /// than `return value`, and why interleaving two chains is not as simple as it looks.
    pub fn adopt(&mut self, derived: PromiseId, other: PromiseId) {
        if derived == other {
            // Resolving a promise with itself is a `TypeError` in the spec. Without this it is
            // a promise that can never settle, which is worse: silent, and indistinguishable
            // from a pending network call.
            self.reject(derived, Value::UNDEFINED);
            return;
        }
        self.then(
            other,
            Some(Box::new(move |agent, value| {
                agent.fulfil(derived, value);
                Outcome::Fulfilled(value)
            })),
            Some(Box::new(move |agent, reason| {
                agent.reject(derived, reason);
                Outcome::Fulfilled(reason)
            })),
        );
    }

    /// Runs queued jobs until there are none, returning how many ran.
    ///
    /// Jobs queued *by* jobs run in the same drain, which is what "microtasks run to
    /// completion" means and is why an infinite `.then` chain starves the event loop rather
    /// than yielding to it. That is the specified behaviour, not an oversight.
    pub fn run_microtasks(&mut self) -> usize {
        let mut ran = 0;
        while let Some(job) = self.queue.pop_front() {
            job(self);
            ran += 1;
        }
        ran
    }

    /// Runs at most `limit` jobs, for tests that want to look at a half-drained queue.
    pub fn run_microtasks_up_to(&mut self, limit: usize) -> usize {
        let mut ran = 0;
        while ran < limit {
            let Some(job) = self.queue.pop_front() else {
                break;
            };
            job(self);
            ran += 1;
        }
        ran
    }
}
