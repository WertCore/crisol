//! Ordinary objects: prototype chains, and the internal methods on them.
//!
//! ECMA-262 10.1. These are the `[[Get]]`, `[[Set]]`, `[[Delete]]` and friends that every
//! object has unless it is exotic, and getting them right is what makes `Object.freeze`,
//! `Object.keys` and prototype inheritance behave.
//!
//! # Where this sits relative to shapes and the heap
//!
//! [`crisol_value::Shapes`] is the *fast* path: a data property in a slot, resolved at compile
//! time. This is the *general* path, where a property can be an accessor, non-enumerable or
//! frozen. Real engines keep both and spill from the first to the second; marrying them is the
//! next piece of work and is why this crate does not reach into [`crisol_gc`] yet — the join
//! is a design decision that deserves its own diff rather than being smuggled in underneath a
//! descriptor implementation.
//!
//! # Getters are returned, not called
//!
//! `[[Get]]` on an accessor has to call a function, and nothing in this crate can call one.
//! Rather than pretend, [`OrdinaryObject::get`] returns [`Got::Getter`] and the caller — which
//! will be the runtime, once there is one — performs the call. That keeps the property lookup
//! honest about what it did and did not do.

use crisol_value::{PropertyKey, Value};

use crate::descriptor::{PartialDescriptor, Property, Rejected};

/// Identifies an object in a [`Realm`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(u32);

impl ObjectId {
    /// The index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// One ordinary object.
#[derive(Clone, Debug, Default)]
pub struct OrdinaryObject {
    /// Properties, in insertion order. A `Vec` rather than a map because insertion order is
    /// observable — `Object.keys` has to produce it — and these are small.
    properties: Vec<(PropertyKey, Property)>,
    /// `[[Prototype]]`. `None` is `null`, which is what `Object.create(null)` makes.
    prototype: Option<ObjectId>,
    /// `[[Extensible]]`.
    extensible: bool,
}

/// What `[[Get]]` found.
#[derive(Clone, Debug, PartialEq)]
pub enum Got {
    /// The value. Also what a missing property gives, as `undefined`.
    Value(Value),
    /// An accessor. The caller has to call this with the receiver.
    Getter(Value),
}

/// What `[[Set]]` decided.
#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    /// Done.
    Done,
    /// Refused — a non-writable property, or a non-extensible object. In strict mode this is
    /// a `TypeError`; in sloppy mode it is silently ignored, which is why the decision is
    /// returned rather than made here.
    Refused,
    /// An accessor. The caller has to call this setter with the receiver and the value.
    Setter(Value),
}

/// Every object, and the chains between them.
#[derive(Debug, Default)]
pub struct Realm {
    objects: Vec<OrdinaryObject>,
}

impl Realm {
    /// An empty realm.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an object with the given prototype.
    pub fn create(&mut self, prototype: Option<ObjectId>) -> ObjectId {
        let id = ObjectId(u32::try_from(self.objects.len()).expect("objects fit in u32"));
        self.objects.push(OrdinaryObject {
            properties: Vec::new(),
            prototype,
            extensible: true,
        });
        id
    }

    /// `[[GetPrototypeOf]]`.
    #[must_use]
    pub fn prototype_of(&self, object: ObjectId) -> Option<ObjectId> {
        self.get_object(object).and_then(|object| object.prototype)
    }

    /// `[[SetPrototypeOf]]`, returning whether it was allowed.
    ///
    /// Refused on a non-extensible object unless it is a no-op, and refused when it would make
    /// a cycle — `a.__proto__ = b; b.__proto__ = a` would otherwise make every lookup hang.
    pub fn set_prototype_of(&mut self, object: ObjectId, prototype: Option<ObjectId>) -> bool {
        if self.prototype_of(object) == prototype {
            return true;
        }
        if !self.is_extensible(object) {
            return false;
        }
        // Walk up from the proposed prototype: if we meet ourselves, this would close a loop.
        let mut walk = prototype;
        while let Some(current) = walk {
            if current == object {
                return false;
            }
            walk = self.prototype_of(current);
        }
        if let Some(target) = self.objects.get_mut(object.0 as usize) {
            target.prototype = prototype;
            return true;
        }
        false
    }

    /// `[[IsExtensible]]`.
    #[must_use]
    pub fn is_extensible(&self, object: ObjectId) -> bool {
        self.get_object(object)
            .is_some_and(|object| object.extensible)
    }

    /// `[[PreventExtensions]]`. One way only: nothing turns it back on.
    pub fn prevent_extensions(&mut self, object: ObjectId) -> bool {
        if let Some(target) = self.objects.get_mut(object.0 as usize) {
            target.extensible = false;
            return true;
        }
        false
    }

    /// `[[GetOwnProperty]]` — this object only, no prototype chain.
    #[must_use]
    pub fn own_property(&self, object: ObjectId, key: &PropertyKey) -> Option<&Property> {
        self.get_object(object)?
            .properties
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, property)| property)
    }

    /// `[[DefineOwnProperty]]`.
    ///
    /// # Errors
    ///
    /// [`Rejected`] when the change is not permitted.
    pub fn define_own_property(
        &mut self,
        object: ObjectId,
        key: PropertyKey,
        descriptor: &PartialDescriptor,
    ) -> Result<(), Rejected> {
        let extensible = self.is_extensible(object);
        let current = self.own_property(object, &key).cloned();
        let next = crate::descriptor::validate_and_apply(current.as_ref(), descriptor, extensible)?;

        let Some(target) = self.objects.get_mut(object.0 as usize) else {
            return Err(Rejected::NotExtensible);
        };
        match target.properties.iter_mut().find(|(name, _)| *name == key) {
            // Replaced in place, so redefining a property does not move it to the end of
            // `Object.keys`.
            Some(slot) => slot.1 = next,
            None => target.properties.push((key, next)),
        }
        Ok(())
    }

    /// `[[HasProperty]]` — this object and its prototype chain.
    #[must_use]
    pub fn has_property(&self, object: ObjectId, key: &PropertyKey) -> bool {
        let mut current = Some(object);
        while let Some(id) = current {
            if self.own_property(id, key).is_some() {
                return true;
            }
            current = self.prototype_of(id);
        }
        false
    }

    /// `[[Get]]`, walking the prototype chain.
    ///
    /// A missing property is `undefined` rather than an error — which is why a typo in a
    /// property name is a runtime surprise in JavaScript rather than a compile-time one.
    #[must_use]
    pub fn get(&self, object: ObjectId, key: &PropertyKey) -> Got {
        let mut current = Some(object);
        while let Some(id) = current {
            if let Some(property) = self.own_property(id, key) {
                return match property {
                    Property::Data { value, .. } => Got::Value(*value),
                    Property::Accessor { get, .. } => Got::Getter(*get),
                };
            }
            current = self.prototype_of(id);
        }
        Got::Value(Value::UNDEFINED)
    }

    /// `[[Set]]`.
    ///
    /// The subtle part is that a **non-writable data property on the prototype blocks
    /// assignment on the receiver**. `Object.freeze(proto)` therefore stops `child.x = 1` from
    /// creating an own property on the child, which surprises people and is what the spec
    /// says.
    pub fn set(&mut self, object: ObjectId, key: &PropertyKey, value: Value) -> Setting {
        let mut current = Some(object);
        while let Some(id) = current {
            match self.own_property(id, key) {
                Some(Property::Accessor { set, .. }) => return Setting::Setter(*set),
                Some(Property::Data { writable, .. }) => {
                    if !*writable {
                        return Setting::Refused;
                    }
                    break;
                }
                None => current = self.prototype_of(id),
            }
        }

        // Either an own writable data property, or nothing anywhere on the chain. Both end up
        // writing an own property on the receiver.
        if self.own_property(object, key).is_some() {
            let descriptor = PartialDescriptor::value(value);
            return match self.define_own_property(object, key.clone(), &descriptor) {
                Ok(()) => Setting::Done,
                Err(_) => Setting::Refused,
            };
        }
        if !self.is_extensible(object) {
            return Setting::Refused;
        }
        if let Some(target) = self.objects.get_mut(object.0 as usize) {
            // Assignment creates a fully-permissive property, unlike `defineProperty` with an
            // empty descriptor, which creates a locked-down one.
            target.properties.push((key.clone(), Property::data(value)));
            return Setting::Done;
        }
        Setting::Refused
    }

    /// `[[Delete]]`, returning whether the property is gone afterwards.
    ///
    /// Deleting something that was never there succeeds, which is why `delete obj.nothing` is
    /// `true`.
    pub fn delete(&mut self, object: ObjectId, key: &PropertyKey) -> bool {
        match self.own_property(object, key) {
            None => true,
            Some(property) if !property.configurable() => false,
            Some(_) => {
                if let Some(target) = self.objects.get_mut(object.0 as usize) {
                    target.properties.retain(|(name, _)| name != key);
                }
                true
            }
        }
    }

    /// `[[OwnPropertyKeys]]`.
    ///
    /// **Array indices first, in ascending numeric order, then strings in insertion order.**
    /// Not an implementation detail: `Object.keys({b: 1, 2: 2, a: 3, 1: 4})` is
    /// `["1", "2", "b", "a"]`, and code that renders a keyed list depends on it.
    #[must_use]
    pub fn own_keys(&self, object: ObjectId) -> Vec<PropertyKey> {
        let Some(target) = self.get_object(object) else {
            return Vec::new();
        };
        let mut indices: Vec<(u32, PropertyKey)> = Vec::new();
        let mut strings: Vec<PropertyKey> = Vec::new();
        for (key, _) in &target.properties {
            match array_index(key.as_str()) {
                Some(index) => indices.push((index, key.clone())),
                None => strings.push(key.clone()),
            }
        }
        indices.sort_by_key(|(index, _)| *index);
        indices
            .into_iter()
            .map(|(_, key)| key)
            .chain(strings)
            .collect()
    }

    /// The keys `Object.keys` returns: own, enumerable, string-keyed.
    #[must_use]
    pub fn enumerable_keys(&self, object: ObjectId) -> Vec<PropertyKey> {
        self.own_keys(object)
            .into_iter()
            .filter(|key| {
                self.own_property(object, key)
                    .is_some_and(Property::enumerable)
            })
            .collect()
    }

    fn get_object(&self, object: ObjectId) -> Option<&OrdinaryObject> {
        self.objects.get(object.0 as usize)
    }
}

/// Whether a key is an array index, per the spec's definition.
///
/// A canonical decimal in `0..2^32-1`. `"01"` is not one, and neither is `"1.0"` or `"-0"` —
/// they are ordinary string keys, and putting them in the numeric group would reorder
/// `Object.keys` in a way no engine does.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index: u32 = key.parse().ok()?;
    (index != u32::MAX).then_some(index)
}
