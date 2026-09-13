//! `Map` and `Set`.
//!
//! # Their key equality is not the same as anything else in this crate
//!
//! Three equalities are in play and they disagree in different places:
//!
//! | | `NaN` vs `NaN` | `0` vs `-0` |
//! |---|---|---|
//! | `===` (strict) | different | same |
//! | `Object.is` (SameValue) | same | different |
//! | **`Map`/`Set` (SameValueZero)** | **same** | **same** |
//!
//! `Value`'s derived equality is `Object.is` (D-53), which is exactly right for property
//! descriptors (D-60) and **wrong here**: a `Map` with `0` as a key must be found by `-0`.
//! Using `Value`'s own `==` for map keys would give a map with two entries that print
//! identically and neither of which `get(0)` finds — a bug that survives every casual test,
//! because nobody writes `-0` on purpose.
//!
//! So keys go through [`Key`], which normalises `-0` to `0` and relies on NaN already being
//! canonical from M9.
//!
//! # Iteration order, and mutating while iterating
//!
//! Insertion order, and the spec is specific about what happens when the map changes during
//! iteration: an entry **deleted** before the iterator reaches it is not visited, and an entry
//! **added** during iteration *is*. Storing entries in a `Vec` with tombstones gives both for
//! free, which is the reason it is a `Vec` rather than only a hash map.

use std::collections::HashMap;

use crisol_value::Value;

/// A map key under SameValueZero.
///
/// Wraps a [`Value`]'s bits with `-0` folded into `0`. NaN needs no handling because M9
/// canonicalises it on the way into a `Value` (D-53) — which is the second time that decision
/// has paid for itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key(u64);

impl Key {
    fn new(value: Value) -> Self {
        // `-0.0 == 0.0` is true for `f64`, so this catches negative zero and nothing else.
        if let Some(number) = value.as_number()
            && number == 0.0
        {
            return Self(Value::number(0.0).to_bits());
        }
        Self(value.to_bits())
    }
}

/// A JavaScript `Map`.
#[derive(Debug, Default)]
pub struct JsMap {
    /// Insertion order, with `None` where an entry was deleted.
    entries: Vec<Option<(Value, Value)>>,
    /// Key to index in `entries`.
    index: HashMap<Key, usize>,
    live: usize,
}

impl JsMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `map.size`.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.live
    }

    /// Whether it holds nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// `map.set(key, value)`.
    ///
    /// Setting an existing key replaces the value and **keeps its original position** — a
    /// `Map` used as an LRU by re-setting would not work, and that is the specified behaviour.
    pub fn set(&mut self, key: Value, value: Value) {
        let lookup = Key::new(key);
        // The stored key is left alone: `set(-0, …)` on a map keyed by `0` updates the value
        // and does not replace the key, which is what the spec says and what `map.keys()`
        // then reports.
        if let Some(at) = self.index.get(&lookup)
            && let Some(Some((_, existing))) = self.entries.get_mut(*at)
        {
            *existing = value;
            return;
        }
        self.index.insert(lookup, self.entries.len());
        self.entries.push(Some((key, value)));
        self.live += 1;
    }

    /// `map.get(key)`.
    #[must_use]
    pub fn get(&self, key: Value) -> Option<Value> {
        let at = *self.index.get(&Key::new(key))?;
        self.entries.get(at)?.as_ref().map(|(_, value)| *value)
    }

    /// `map.has(key)`.
    #[must_use]
    pub fn has(&self, key: Value) -> bool {
        self.get(key).is_some()
    }

    /// `map.delete(key)`, returning whether anything was removed.
    pub fn delete(&mut self, key: Value) -> bool {
        let lookup = Key::new(key);
        let Some(at) = self.index.remove(&lookup) else {
            return false;
        };
        if let Some(slot) = self.entries.get_mut(at)
            && slot.take().is_some()
        {
            self.live -= 1;
            return true;
        }
        false
    }

    /// `map.clear()`.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
        self.live = 0;
    }

    /// Entries in insertion order.
    pub fn entries(&self) -> impl Iterator<Item = (Value, Value)> + '_ {
        self.entries.iter().filter_map(|slot| *slot)
    }

    /// Keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = Value> + '_ {
        self.entries().map(|(key, _)| key)
    }

    /// Values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = Value> + '_ {
        self.entries().map(|(_, value)| value)
    }

    /// The entry at a position in the insertion sequence, skipping deleted ones.
    ///
    /// What an iterator that survives mutation needs: it holds a position in `entries`, not a
    /// borrow, so the map can be changed between steps. An entry deleted before the iterator
    /// reaches it is skipped; one appended during iteration is reached.
    #[must_use]
    pub fn at(&self, position: usize) -> Option<(Value, Value)> {
        self.entries.get(position).copied().flatten()
    }

    /// How many positions there are, including deleted ones.
    #[must_use]
    pub const fn positions(&self) -> usize {
        self.entries.len()
    }
}

/// A JavaScript `Set`.
///
/// A [`JsMap`] whose values are its keys, which is what the specification says it is.
#[derive(Debug, Default)]
pub struct JsSet {
    map: JsMap,
}

impl JsSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `set.size`.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.map.size()
    }

    /// Whether it holds nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `set.add(value)`.
    pub fn add(&mut self, value: Value) {
        self.map.set(value, value);
    }

    /// `set.has(value)`.
    #[must_use]
    pub fn has(&self, value: Value) -> bool {
        self.map.has(value)
    }

    /// `set.delete(value)`.
    pub fn delete(&mut self, value: Value) -> bool {
        self.map.delete(value)
    }

    /// `set.clear()`.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = Value> + '_ {
        self.map.keys()
    }

    /// The value at a position, skipping deleted ones. See [`JsMap::at`].
    #[must_use]
    pub fn at(&self, position: usize) -> Option<Value> {
        self.map.at(position).map(|(value, _)| value)
    }

    /// How many positions there are, including deleted ones.
    #[must_use]
    pub const fn positions(&self) -> usize {
        self.map.positions()
    }
}
