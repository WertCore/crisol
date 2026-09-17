//! Object, Array, String, Promise, RegExp and friends.
//!
//! ROADMAP §M12. The object model comes first because everything else stands on it:
//! `Object.defineProperty`, `Object.freeze`, getters, `Proxy` and `Reflect` are all
//! restatements of the internal methods in [`Realm`].

#![doc(html_root_url = "https://docs.rs/crisol-builtins/0.0.0")]

mod array;
mod collections;
mod convert;
mod date;
mod descriptor;
mod error;
mod iterator;
mod json;
mod object;
mod promise;
mod proxy;
mod reflect;
mod regexp;
mod statics;
mod string;
mod symbol;

pub use array::{JsArray, MAX_LENGTH, array_index};
pub use collections::{JsMap, JsSet};
pub use convert::{
    number_to_string, string_to_boolean, string_to_number, to_boolean, to_number, to_string,
};
pub use date::{
    Fields, INVALID_DATE, MAX_TIME, MS_PER_DAY, civil_from_days, day_from_time, days_from_civil,
    days_in_month, fields, is_leap_year, make_date, make_time, time_clip, time_from_civil,
    time_within_day, to_iso_string, week_day,
};
pub use descriptor::{Entry, PartialDescriptor, Property, Rejected, validate_and_apply};
pub use error::{ErrorKind, JsError};
pub use iterator::{AsyncStepIterator, Step, StepIterator, collect, read_step, take};
pub use json::{Json, ParseError, parse, stringify};
pub use object::{Got, ObjectId, OrdinaryObject, Realm, Setting};
pub use promise::{Agent, Outcome, PromiseId, Reaction, State};
pub use proxy::{Proxy, ProxyError, Target, Trap};
pub use reflect::{BooleanObject, boolean};
pub use regexp::{Captured, Flags, JsRegExp};

/// `Reflect` — the non-throwing mirror of the `Object` statics.
pub mod reflect_ns {
    pub use crate::reflect::{
        define_property, delete_property, get, has, is_extensible, own_keys,
        own_property_descriptor, prevent_extensions, prototype_of, set, set_prototype_of,
    };
}
pub use statics::{
    MAX_SAFE_INTEGER, assign, entries, freeze, from_entries, global_is_finite, global_is_nan,
    is_frozen, is_integer, is_safe_integer, is_sealed, keys, number_is_finite, number_is_nan,
    own_property_names, seal, values,
};
pub use string::JsString;
pub use symbol::{SymbolId, Symbols, WellKnown};
