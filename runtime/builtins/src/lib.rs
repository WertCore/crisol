//! Object, Array, String, Promise, RegExp and friends.
//!
//! ROADMAP §M12. The object model comes first because everything else stands on it:
//! `Object.defineProperty`, `Object.freeze`, getters, `Proxy` and `Reflect` are all
//! restatements of the internal methods in [`Realm`].

#![doc(html_root_url = "https://docs.rs/crisol-builtins/0.0.0")]

mod descriptor;
mod object;
mod promise;

pub use descriptor::{Entry, PartialDescriptor, Property, Rejected, validate_and_apply};
pub use object::{Got, ObjectId, OrdinaryObject, Realm, Setting};
pub use promise::{Agent, Outcome, PromiseId, Reaction, State};
