//! Object, Array, String, Promise, RegExp and friends.
//!
//! ROADMAP §M12. The object model comes first because everything else stands on it:
//! `Object.defineProperty`, `Object.freeze`, getters, `Proxy` and `Reflect` are all
//! restatements of the internal methods in [`Realm`].

#![doc(html_root_url = "https://docs.rs/crisol-builtins/0.0.0")]

mod array;
mod collections;
mod convert;
mod descriptor;
mod error;
mod json;
mod object;
mod promise;
mod symbol;

pub use array::{JsArray, MAX_LENGTH, array_index};
pub use collections::{JsMap, JsSet};
pub use convert::{
    number_to_string, string_to_boolean, string_to_number, to_boolean, to_number, to_string,
};
pub use descriptor::{Entry, PartialDescriptor, Property, Rejected, validate_and_apply};
pub use error::{ErrorKind, JsError};
pub use json::{Json, ParseError, parse, stringify};
pub use object::{Got, ObjectId, OrdinaryObject, Realm, Setting};
pub use promise::{Agent, Outcome, PromiseId, Reaction, State};
pub use symbol::{SymbolId, Symbols, WellKnown};
