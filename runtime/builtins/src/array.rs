//! `Array`, which is an exotic object for exactly one reason.
//!
//! Everything unusual about an array comes from `length` being a real property whose value is
//! tied to the indices that exist. Two directions:
//!
//! - Writing an index at or beyond `length` **raises** `length` to `index + 1`.
//! - Writing a smaller `length` **deletes** the elements above it.
//!
//! # A hole is not `undefined`
//!
//! `[, 1]` has length 2 and one element. `delete a[0]` makes a hole where a value was. A hole
//! reads as `undefined` — and is *not* the same as a property holding `undefined`:
//!
//! ```text
//! const a = [, 1];   a[0] === undefined   // true
//!                    0 in a               // false
//! const b = [undefined, 1];
//!                    b[0] === undefined   // true
//!                    0 in b               // true
//! ```
//!
//! Every method that iterates has to decide which it means, and they do not all agree — which
//! is why `has` exists separately from `get` here rather than `get` returning `undefined` and
//! leaving the caller to guess.
//!
//! # Truncation can fail halfway
//!
//! `ArraySetLength` (ECMA-262 10.4.2.4) deletes from the top down and **stops at the first
//! element it cannot delete**, leaving `length` one above it and reporting failure. So
//! `Object.freeze`ing one element of an array makes `a.length = 0` shrink the array only as far
//! as that element — and it partially succeeds rather than doing nothing. An implementation
//! that treated the whole operation as atomic would be wrong in both directions: it would
//! either refuse a change the spec allows, or discard elements the spec protects.

use crisol_value::Value;

use crate::descriptor::Property;

/// The largest length an array can have: `2^32 - 1`.
///
/// Not a limit of this implementation — it is the spec's, and it is why array indices stop at
/// `2^32 - 2`.
pub const MAX_LENGTH: u32 = u32::MAX;

/// A JavaScript array.
#[derive(Debug, Default)]
pub struct JsArray {
    /// One slot per index below `length`. `None` is a hole.
    elements: Vec<Option<Property>>,
    /// Whether `length` itself may be written. `Object.freeze` clears it.
    length_writable: bool,
}

impl JsArray {
    /// An empty array.
    #[must_use]
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            length_writable: true,
        }
    }

    /// An array of `length` holes, as `new Array(n)` gives.
    #[must_use]
    pub fn with_length(length: u32) -> Self {
        Self {
            elements: vec![None; length as usize],
            length_writable: true,
        }
    }

    /// From a list of values, as a literal gives.
    #[must_use]
    pub fn from_values(values: &[Value]) -> Self {
        Self {
            elements: values
                .iter()
                .map(|value| Some(Property::data(*value)))
                .collect(),
            length_writable: true,
        }
    }

    /// `array.length`.
    #[must_use]
    pub fn length(&self) -> u32 {
        u32::try_from(self.elements.len()).unwrap_or(MAX_LENGTH)
    }

    /// Whether `length` may be written.
    #[must_use]
    pub const fn length_writable(&self) -> bool {
        self.length_writable
    }

    /// Makes `length` non-writable, as `Object.freeze` does.
    pub fn freeze_length(&mut self) {
        self.length_writable = false;
    }

    /// Whether index `index` has an element — `index in array`.
    ///
    /// False for a hole, true for a property holding `undefined`. This is the distinction the
    /// module docs are about.
    #[must_use]
    pub fn has(&self, index: u32) -> bool {
        self.elements
            .get(index as usize)
            .is_some_and(Option::is_some)
    }

    /// `array[index]`, or `None` for a hole or an index past the end.
    ///
    /// `None` here means "no element", not "the element is `undefined`" — see [`JsArray::has`].
    #[must_use]
    pub fn get(&self, index: u32) -> Option<Value> {
        match self.elements.get(index as usize)?.as_ref()? {
            Property::Data { value, .. } => Some(*value),
            // An accessor on an array index is legal and rare. Reading it needs a call, which
            // this crate cannot do, so it is reported as absent rather than as a wrong value.
            Property::Accessor { .. } => None,
        }
    }

    /// The property at an index, accessors included.
    #[must_use]
    pub fn property(&self, index: u32) -> Option<&Property> {
        self.elements.get(index as usize)?.as_ref()
    }

    /// `array[index] = value`, growing `length` when the index is at or beyond it.
    ///
    /// Returns false when the write was refused: the element is non-writable, or the array
    /// would have to grow and `length` is not writable.
    pub fn set(&mut self, index: u32, value: Value) -> bool {
        if let Some(Some(existing)) = self.elements.get_mut(index as usize) {
            return match existing {
                Property::Data {
                    value: slot,
                    writable,
                    ..
                } => {
                    if !*writable {
                        return false;
                    }
                    *slot = value;
                    true
                }
                Property::Accessor { .. } => false,
            };
        }
        if index as usize >= self.elements.len() {
            if !self.length_writable {
                // Growing means writing `length`, and a frozen `length` forbids it. This is
                // why `Object.freeze(a)` stops `a.push(x)` rather than merely stopping
                // `a[0] = x`.
                return false;
            }
            if index == MAX_LENGTH {
                // `length` would have to become 2^32, which it cannot be.
                return false;
            }
            self.elements.resize(index as usize + 1, None);
        }
        self.elements[index as usize] = Some(Property::data(value));
        true
    }

    /// Defines an index with explicit attributes, as `Object.defineProperty` would.
    ///
    /// The way a non-configurable element gets into an array, which is what makes truncation
    /// able to fail.
    pub fn define(&mut self, index: u32, property: Property) -> bool {
        if index as usize >= self.elements.len() {
            if !self.length_writable || index == MAX_LENGTH {
                return false;
            }
            self.elements.resize(index as usize + 1, None);
        }
        self.elements[index as usize] = Some(property);
        true
    }

    /// `delete array[index]`, leaving a hole.
    ///
    /// Deleting does **not** change `length` — `a = [1,2,3]; delete a[2]` leaves length 3 with
    /// a hole at the end, which is the difference between `delete` and `pop`.
    pub fn delete(&mut self, index: u32) -> bool {
        match self.elements.get_mut(index as usize) {
            None => true,
            Some(slot) => match slot {
                None => true,
                Some(property) if !property.configurable() => false,
                Some(_) => {
                    *slot = None;
                    true
                }
            },
        }
    }

    /// `array.length = new_length`.
    ///
    /// Returns whether it fully succeeded. **A partial truncation is possible**: deletion runs
    /// from the top down and stops at the first element that refuses, leaving `length` one
    /// above it. See the module docs.
    pub fn set_length(&mut self, new_length: u32) -> bool {
        if !self.length_writable {
            return false;
        }
        let current = self.length();
        if new_length >= current {
            self.elements.resize(new_length as usize, None);
            return true;
        }

        let mut index = current;
        while index > new_length {
            index -= 1;
            let refuses = self
                .elements
                .get(index as usize)
                .and_then(Option::as_ref)
                .is_some_and(|property| !property.configurable());
            if refuses {
                // Stop here, keeping this element and everything below it. `length` becomes
                // one past the element that refused, not the value that was asked for.
                self.elements.truncate(index as usize + 1);
                return false;
            }
            self.elements[index as usize] = None;
        }
        self.elements.truncate(new_length as usize);
        true
    }

    /// `array.push(value)`, returning the new length.
    ///
    /// Returns `None` when the push was refused, which is what a frozen array does.
    pub fn push(&mut self, value: Value) -> Option<u32> {
        let index = self.length();
        if !self.set(index, value) {
            return None;
        }
        Some(self.length())
    }

    /// `array.pop()`, returning the value removed.
    ///
    /// `None` for an empty array *and* for a hole at the end — both of which `pop` reports as
    /// `undefined` in JavaScript, and both of which still shorten the array.
    pub fn pop(&mut self) -> Option<Value> {
        if self.elements.is_empty() {
            return None;
        }
        let last = self.elements.len() - 1;
        let value = self
            .elements
            .get(last)
            .and_then(Option::as_ref)
            .and_then(|property| match property {
                Property::Data { value, .. } => Some(*value),
                Property::Accessor { .. } => None,
            });
        self.elements.truncate(last);
        value
    }

    /// The indices that have elements, ascending. Holes are skipped.
    ///
    /// What `Object.keys` on an array reports, and what the iterating methods that skip holes
    /// walk.
    pub fn indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.elements.iter().enumerate().filter_map(|(at, slot)| {
            slot.as_ref()
                .map(|_| u32::try_from(at).unwrap_or(MAX_LENGTH))
        })
    }

    /// Every position, with `None` for holes.
    ///
    /// What the methods that *do not* skip holes walk — `JSON.stringify` among them, which
    /// writes a hole as `null`.
    pub fn slots(&self) -> impl Iterator<Item = Option<Value>> + '_ {
        (0..self.length()).map(|index| self.get(index))
    }

    /// Whether the array has no elements at all. An array of holes is not empty: it has a
    /// length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }
}

/// Whether a property key names an array index.
///
/// The spec's definition: a canonical decimal below `2^32 - 1`. `"01"`, `"1.0"`, `"-1"` and
/// `"4294967295"` are ordinary string keys — the last because it is `2^32 - 1`, which is a
/// valid *length* but not a valid index.
#[must_use]
pub fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index: u32 = key.parse().ok()?;
    (index != MAX_LENGTH).then_some(index)
}
