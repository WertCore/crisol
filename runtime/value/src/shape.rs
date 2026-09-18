//! Hidden classes.
//!
//! An object does not carry its own property names. It carries a [`ShapeId`] and a flat array
//! of values, and the shape says which name lives in which slot. Objects built the same way
//! share a shape, so the names are stored once for all of them rather than once each — which
//! is the same argument as `crisol-style`'s interner (D-21), applied to a different kind of
//! repetition.
//!
//! # Shapes form a tree, not a set
//!
//! Adding a property to a shape *transitions* to another shape, and the transition is
//! remembered. So `{}` → `.x` → `.y` is walked once and reused forever: every object literal
//! `{x: 1, y: 2}` in a program arrives at the same [`ShapeId`] without comparing any names.
//!
//! The tree is why property *order* is part of a shape's identity. `{x, y}` and `{y, x}` are
//! different shapes, which is not an implementation artefact — JavaScript specifies insertion
//! order for string keys, and `Object.keys` has to produce it.
//!
//! # What lookup costs
//!
//! A shape stores only the property it adds, so finding a name walks the chain to the root:
//! O(properties). Storing a flat map per shape would make lookup O(1) and the memory O(n²)
//! across a transition chain, which is the wrong trade for the objects programs actually
//! build.
//!
//! ROADMAP §3.4 is explicit that the generic path is expected to be the common one in v1 and
//! that the answer is a per-site monomorphic cache: a call site remembers the last shape it
//! saw and the slot it resolved to, so the walk happens once per site rather than once per
//! access. That cache belongs to the IR (M11), not here. What belongs here is a lookup whose
//! answer is stable enough to cache, which is why [`ShapeId`] is dense and comparable.

use std::collections::HashMap;

use crate::key::PropertyKey;

/// Identifies a shape.
///
/// Dense and `Copy` so a per-site cache can hold one and compare it with an integer compare
/// (ROADMAP §3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShapeId(u32);

impl ShapeId {
    /// The index, for a caller keeping its own side table.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// What a property permits, beyond holding a value.
///
/// **Assignment and `defineProperty` default to opposite ends of this.** `o.x = 1` creates a
/// property that is writable, enumerable and configurable; `Object.defineProperty(o, "x", {})`
/// creates one that is none of those. Getting that backwards makes a defined property behave
/// like an assigned one, which every test of the difference catches and nothing else does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Attributes {
    /// Whether a write is allowed. A write to a non-writable property is silently ignored
    /// outside strict mode, which is why it is not an error here.
    pub writable: bool,
    /// Whether `Object.keys` and `for-in` see it.
    pub enumerable: bool,
    /// Whether it can be deleted or redefined.
    pub configurable: bool,
}

impl Attributes {
    /// What `o.x = 1` creates: everything permitted.
    pub const DATA: Self = Self {
        writable: true,
        enumerable: true,
        configurable: true,
    };

    /// What `Object.defineProperty` creates when the descriptor says nothing: nothing
    /// permitted.
    pub const DEFINED: Self = Self {
        writable: false,
        enumerable: false,
        configurable: false,
    };
}

impl Default for Attributes {
    fn default() -> Self {
        Self::DATA
    }
}

/// Where a property's value lives in an object's slot array.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slot(u32);

impl Slot {
    /// The index into the object's values.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// One shape: what it adds to its parent, and where it can go next.
#[derive(Debug)]
struct ShapeData {
    /// The shape this one adds a property to. `None` only for a root.
    parent: Option<ShapeId>,
    /// The property this shape adds, and its slot. `None` only for a root.
    added: Option<(PropertyKey, Slot)>,
    /// How many properties this shape has in total.
    count: u32,
    /// Shapes reached by adding one more property.
    ///
    /// This is what makes the second object of a given literal free: the names were compared
    /// when the first one was built, and never again.
    transitions: HashMap<PropertyKey, ShapeId>,
    /// Whether property access on this object has to take the slow path (ROADMAP §3.2).
    exotic: bool,
}

/// Every shape in a runtime.
///
/// Owns the tree. Shapes are never removed: a program's set of shapes is bounded by its
/// source text rather than by its data, so it reaches a fixed point early and stays there.
#[derive(Debug)]
pub struct Shapes {
    shapes: Vec<ShapeData>,
    ordinary_root: ShapeId,
    exotic_root: ShapeId,
}

impl Shapes {
    /// A table with the two roots in it.
    #[must_use]
    pub fn new() -> Self {
        let shapes = vec![ShapeData::root(false), ShapeData::root(true)];
        Self {
            shapes,
            ordinary_root: ShapeId(0),
            exotic_root: ShapeId(1),
        }
    }

    /// The shape of `{}`.
    #[must_use]
    pub const fn root(&self) -> ShapeId {
        self.ordinary_root
    }

    /// The shape of an exotic empty object — a `Proxy`, or anything else whose property
    /// access cannot be specialised (ROADMAP §3.2).
    #[must_use]
    pub const fn exotic_root(&self) -> ShapeId {
        self.exotic_root
    }

    /// Whether access on this shape must take the slow path.
    ///
    /// One field read and one branch, which is the whole of §3.2's resolution: programs that
    /// never construct a `Proxy` pay one predictable branch rather than a check per access.
    #[must_use]
    pub fn is_exotic(&self, shape: ShapeId) -> bool {
        self.data(shape).exotic
    }

    /// How many properties a shape has.
    #[must_use]
    pub fn len(&self, shape: ShapeId) -> u32 {
        self.data(shape).count
    }

    /// Whether a shape has no properties.
    #[must_use]
    pub fn is_empty(&self, shape: ShapeId) -> bool {
        self.len(shape) == 0
    }

    /// The shape that results from adding `key` to `shape`.
    ///
    /// Adding a property the shape already has is not a transition — it is an assignment, and
    /// assignment does not change an object's shape. Returning the same id for that case is
    /// what stops `for (…) obj.x = i` from growing the tree once per iteration.
    pub fn add(&mut self, shape: ShapeId, key: &PropertyKey) -> ShapeId {
        if self.lookup(shape, key).is_some() {
            return shape;
        }
        if let Some(existing) = self.data(shape).transitions.get(key) {
            return *existing;
        }

        let parent = self.data(shape);
        let slot = Slot(parent.count);
        let count = parent.count + 1;
        let exotic = parent.exotic;

        let id = ShapeId(u32::try_from(self.shapes.len()).expect("shapes fit in u32"));
        self.shapes.push(ShapeData {
            parent: Some(shape),
            added: Some((key.clone(), slot)),
            count,
            transitions: HashMap::new(),
            exotic,
        });
        self.shapes[shape.0 as usize]
            .transitions
            .insert(key.clone(), id);
        id
    }

    /// The slot `key` occupies in `shape`, if it has one.
    ///
    /// Walks to the root. See the module docs for why that is the right cost here and where
    /// the cache that hides it belongs.
    #[must_use]
    pub fn lookup(&self, shape: ShapeId, key: &PropertyKey) -> Option<Slot> {
        let mut current = Some(shape);
        while let Some(id) = current {
            let data = self.data(id);
            if let Some((name, slot)) = &data.added
                && name == key
            {
                return Some(*slot);
            }
            current = data.parent;
        }
        None
    }

    /// Every property of `shape`, in insertion order.
    ///
    /// Insertion order because JavaScript specifies it for string keys and `Object.keys` has
    /// to produce it. The walk is rootward, so the result is reversed before it is returned
    /// rather than left for each caller to remember.
    #[must_use]
    pub fn properties(&self, shape: ShapeId) -> Vec<(PropertyKey, Slot)> {
        let mut out = Vec::with_capacity(self.len(shape) as usize);
        let mut current = Some(shape);
        while let Some(id) = current {
            let data = self.data(id);
            if let Some((name, slot)) = &data.added {
                out.push((name.clone(), *slot));
            }
            current = data.parent;
        }
        out.reverse();
        out
    }

    /// How many shapes exist. For tests and for a runtime that wants to report its own size.
    #[must_use]
    pub fn count(&self) -> usize {
        self.shapes.len()
    }

    fn data(&self, shape: ShapeId) -> &ShapeData {
        &self.shapes[shape.0 as usize]
    }
}

impl Default for Shapes {
    fn default() -> Self {
        Self::new()
    }
}

impl ShapeData {
    fn root(exotic: bool) -> Self {
        Self {
            parent: None,
            added: None,
            count: 0,
            transitions: HashMap::new(),
            exotic,
        }
    }
}
