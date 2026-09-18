//! The heap, the collector, and the rooting API.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;

use crisol_value::{ShapeId, Value};

use crate::handle::GcRef;

/// What one collection did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Collected {
    /// Objects reachable from a root, which survived.
    pub marked: usize,
    /// Objects reclaimed.
    pub swept: usize,
    /// Slots retired because their generation could not advance. See [`GcRef`].
    pub retired: usize,
}

/// How the heap has been used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Allocations since the heap was created.
    pub allocated: u64,
    /// Collections run.
    pub collections: u64,
    /// Objects reclaimed over all collections.
    pub swept: u64,
}

/// An object: its shape, one value per slot the shape names, and what it inherits from.
#[derive(Debug)]
struct Object {
    shape: ShapeId,
    slots: Vec<Value>,
    /// Its `[[Prototype]]`, or `None` for an object at the end of the chain.
    ///
    /// A `GcRef` rather than a `Value` because a prototype is an object or nothing — there is
    /// no boxed form to get wrong — and because the collector has to trace it, which is easier
    /// to not forget when the type says it is a reference.
    prototype: Option<GcRef>,
    /// Engine-private state: a closure's function index and its captured values.
    ///
    /// **Separate from `slots`, and that separation is the whole point.** Properties are
    /// addressed by *shape*, and a shape assigns slot numbers from zero — so a closure keeping
    /// its function index in slot zero lost it the moment anything stored a property on the
    /// function. `class C {}` does exactly that: it stores `prototype` on the constructor,
    /// which took slot zero and overwrote the index, and the constructor silently stopped
    /// being callable.
    ///
    /// Traced like any other reference, because captures are values the program can still
    /// reach.
    internals: Vec<Value>,
}

#[derive(Debug)]
enum State {
    Live { object: Object, marked: bool },
    Free,
}

#[derive(Debug)]
struct Entry {
    generation: u16,
    state: State,
}

/// A source of roots the collector cannot find by itself.
///
/// A newtype purely because `dyn Fn` has no `Debug`, and a `Heap` that could not be printed
/// would be harder to debug than one whose provider prints as a placeholder.
#[derive(Default)]
struct ExtraRoots(Option<Box<dyn Fn() -> Vec<GcRef>>>);

impl std::fmt::Debug for ExtraRoots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Whether one is installed is the part worth seeing. A closure's address is not, and
        // calling it to print the roots would collect-time work in a `Debug` impl.
        f.debug_tuple("ExtraRoots")
            .field(&if self.0.is_some() {
                "installed"
            } else {
                "none"
            })
            .finish()
    }
}

/// A garbage-collected heap.
///
/// # Rooting
///
/// ROADMAP §3.1 calls the GC/FFI boundary the risk of this milestone: Rust code holding a JS
/// value must not hide it from the collector, and getting it wrong produces use-after-free
/// bugs that appear only under memory pressure. §M9 says to make the API hard to misuse and
/// prefers a scope guard to manual push/pop, so that is what this is:
///
/// ```
/// # use crisol_gc::Heap;
/// # use crisol_value::Shapes;
/// let mut shapes = Shapes::new();
/// let heap = Heap::new();
/// let scope = heap.scope();
/// let object = scope.alloc(shapes.root(), 0);
/// // `object` is rooted for as long as `scope` lives, and cannot outlive it.
/// assert!(heap.is_live(object.handle()));
/// ```
///
/// There is no way to obtain a [`Rooted`] without a [`Scope`], and a `Rooted` borrows its
/// scope, so the compiler rejects the mistake rather than the collector discovering it. What
/// remains possible is holding a bare [`GcRef`] across a collection, which is why reading
/// through one is checked rather than assumed — see [`Heap::is_live`].
///
/// # Interior mutability
///
/// `alloc` and `collect` take `&self`. They have to: a scope guard that restored the root
/// stack on drop while allocation held `&mut self` would make two live scopes impossible, and
/// nested scopes are the normal shape of a call stack.
#[derive(Debug, Default)]
pub struct Heap {
    /// A source of roots the shadow stack does not hold — compiled frames, once a program has
    /// registered its stack maps.
    ///
    /// `None` means there are none, which is the truth for an embedder running no compiled
    /// code. It is *not* a default that silently loses roots: a compiled program installs one
    /// before it runs anything, and `collect` refuses when compiled frames exist without it.
    extra_roots: RefCell<ExtraRoots>,
    cells: RefCell<Vec<Entry>>,
    free: RefCell<Vec<u32>>,
    roots: RefCell<Vec<GcRef>>,
    stress: Cell<bool>,
    stats: Cell<Stats>,
}

impl Heap {
    /// An empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs a source of roots the shadow stack cannot see.
    ///
    /// A compiled program calls this before running anything, passing something that walks its
    /// native frames. Without it the collector sees only what Rust code has rooted, and every
    /// value held by compiled code looks like garbage.
    ///
    /// Replacing an existing provider is allowed and takes effect on the next collection; there
    /// is no way to *remove* one, because a program that stopped reporting its compiled roots
    /// midway would be strictly worse than one that never reported them.
    pub fn set_extra_roots(&self, provider: Box<dyn Fn() -> Vec<GcRef>>) {
        self.extra_roots.borrow_mut().0 = Some(provider);
    }

    /// Whether a source of compiled roots has been installed.
    #[must_use]
    pub fn has_extra_roots(&self) -> bool {
        self.extra_roots.borrow().0.is_some()
    }

    /// Collect on every allocation.
    ///
    /// ROADMAP §3.1 asks for exactly this, and it is not a debugging convenience — it is how a
    /// missing root is turned from a bug that appears under memory pressure into one that
    /// appears on the next line. Anything not rooted dies immediately.
    pub fn set_stress(&self, stress: bool) {
        self.stress.set(stress);
    }

    /// Whether stress mode is on.
    #[must_use]
    pub fn stress(&self) -> bool {
        self.stress.get()
    }

    /// Opens a rooting scope.
    ///
    /// Everything allocated through it stays reachable until the scope is dropped, and drop
    /// order is lexical, so nested scopes unwind in the order a call stack does.
    #[must_use]
    pub fn scope(&self) -> Scope<'_> {
        Scope {
            heap: self,
            depth: self.roots.borrow().len(),
        }
    }

    /// How the heap has been used.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    /// How many objects are live.
    #[must_use]
    pub fn live(&self) -> usize {
        self.cells
            .borrow()
            .iter()
            .filter(|cell| matches!(cell.state, State::Live { .. }))
            .count()
    }

    /// Whether a handle still refers to the object it was made for.
    ///
    /// False for a handle whose object has been collected, *including* when the slot has since
    /// been reused — that is what the generation is for.
    #[must_use]
    pub fn is_live(&self, handle: GcRef) -> bool {
        self.cells
            .borrow()
            .get(handle.slot() as usize)
            .is_some_and(|cell| {
                cell.generation == handle.generation() && matches!(cell.state, State::Live { .. })
            })
    }

    /// The shape of the object a handle refers to.
    #[must_use]
    pub fn shape_of(&self, handle: GcRef) -> Option<ShapeId> {
        let cells = self.cells.borrow();
        let cell = cells.get(handle.slot() as usize)?;
        if cell.generation != handle.generation() {
            return None;
        }
        match &cell.state {
            State::Live { object, .. } => Some(object.shape),
            State::Free => None,
        }
    }

    /// Reads a slot.
    #[must_use]
    pub fn get(&self, handle: GcRef, slot: u32) -> Option<Value> {
        let cells = self.cells.borrow();
        let cell = cells.get(handle.slot() as usize)?;
        if cell.generation != handle.generation() {
            return None;
        }
        match &cell.state {
            State::Live { object, .. } => object.slots.get(slot as usize).copied(),
            State::Free => None,
        }
    }

    /// Writes a slot, returning whether it existed.
    pub fn set(&self, handle: GcRef, slot: u32, value: Value) -> bool {
        let mut cells = self.cells.borrow_mut();
        let Some(cell) = cells.get_mut(handle.slot() as usize) else {
            return false;
        };
        if cell.generation != handle.generation() {
            return false;
        }
        match &mut cell.state {
            State::Live { object, .. } => match object.slots.get_mut(slot as usize) {
                Some(existing) => {
                    *existing = value;
                    true
                }
                None => false,
            },
            State::Free => false,
        }
    }

    /// Moves `handle` to `shape`, growing it to `slots` values.
    ///
    /// An object literal is lowered as an empty allocation followed by one `PropertyStore` per
    /// property, and a shape names the properties an object has — so storing a *new* property
    /// has to move the object to the shape that includes it. Without this, an object allocated
    /// at the root shape could never gain a property, which is every object literal.
    ///
    /// Returns whether it happened. It refuses two things:
    ///
    /// - a stale handle, like every other accessor here;
    /// - **any request that would shrink the object**, because the slots past the new end hold
    ///   values, and dropping them would make the collector stop tracing references that the
    ///   object still logically owns. A transition that removes a property has to be written as
    ///   an explicit rebuild, so that the values being discarded are discarded *visibly*.
    ///
    /// New slots arrive as `undefined` rather than uninitialised: a slot holding a plausible
    /// bit pattern is the worst case for a precise collector, which would read it as a
    /// reference and follow it.
    pub fn transition(&self, handle: GcRef, shape: ShapeId, slots: usize) -> bool {
        let mut cells = self.cells.borrow_mut();
        let Some(cell) = cells.get_mut(handle.slot() as usize) else {
            return false;
        };
        if cell.generation != handle.generation() {
            return false;
        }
        match &mut cell.state {
            State::Live { object, .. } => {
                if slots < object.slots.len() {
                    return false;
                }
                object.slots.resize(slots, Value::UNDEFINED);
                object.shape = shape;
                true
            }
            State::Free => false,
        }
    }

    /// Reads engine-private state, which no property access can reach.
    #[must_use]
    pub fn internal(&self, handle: GcRef, index: u32) -> Option<Value> {
        let cells = self.cells.borrow();
        let cell = cells.get(handle.slot() as usize)?;
        if cell.generation != handle.generation() {
            return None;
        }
        match &cell.state {
            State::Live { object, .. } => object.internals.get(index as usize).copied(),
            State::Free => None,
        }
    }

    /// Writes engine-private state, returning whether the index existed.
    pub fn set_internal(&self, handle: GcRef, index: u32, value: Value) -> bool {
        let mut cells = self.cells.borrow_mut();
        let Some(cell) = cells.get_mut(handle.slot() as usize) else {
            return false;
        };
        if cell.generation != handle.generation() {
            return false;
        }
        match &mut cell.state {
            State::Live { object, .. } => match object.internals.get_mut(index as usize) {
                Some(existing) => {
                    *existing = value;
                    true
                }
                None => false,
            },
            State::Free => false,
        }
    }

    /// What `handle` inherits from, if anything.
    #[must_use]
    pub fn prototype_of(&self, handle: GcRef) -> Option<GcRef> {
        let cells = self.cells.borrow();
        let cell = cells.get(handle.slot() as usize)?;
        if cell.generation != handle.generation() {
            return None;
        }
        match &cell.state {
            State::Live { object, .. } => object.prototype,
            State::Free => None,
        }
    }

    /// Sets what `handle` inherits from, returning whether it happened.
    ///
    /// No cycle check. `a.__proto__ = b; b.__proto__ = a` would make a property lookup loop
    /// forever, and the specification forbids it — but the check belongs where prototypes are
    /// *assigned* from source, not here, because the constructor path cannot produce a cycle
    /// and would pay for the walk on every `new`.
    pub fn set_prototype(&self, handle: GcRef, prototype: Option<GcRef>) -> bool {
        let mut cells = self.cells.borrow_mut();
        let Some(cell) = cells.get_mut(handle.slot() as usize) else {
            return false;
        };
        if cell.generation != handle.generation() {
            return false;
        }
        match &mut cell.state {
            State::Live { object, .. } => {
                object.prototype = prototype;
                true
            }
            State::Free => false,
        }
    }

    /// Runs a collection.
    ///
    /// Mark from the roots, sweep what was not reached. Precise rather than conservative: the
    /// roots are exactly the shadow stack, and an object's outgoing references are exactly the
    /// slot values that carry an address, so nothing is retained because an integer happened
    /// to look like a pointer.
    pub fn collect(&self) -> Collected {
        let marked = self.mark();
        let (swept, retired) = self.sweep();

        let mut stats = self.stats.get();
        stats.collections += 1;
        stats.swept += swept as u64;
        self.stats.set(stats);

        Collected {
            marked,
            swept,
            retired,
        }
    }

    /// Marks everything reachable from the shadow stack, returning how many survived.
    fn mark(&self) -> usize {
        let mut worklist: Vec<GcRef> = self.roots.borrow().clone();
        // Roots the shadow stack cannot see. Compiled machine code holds values in registers
        // and frame slots and pushes nothing, so without this every one of them looks like
        // garbage — and marking would free values a running program is still using (§3.1).
        //
        // A callback rather than a direct call into the runtime: the collector defines the
        // hole and whoever knows how to walk a native stack fills it. Reversing that would
        // make this crate depend on the ABI crate, which depends on this one to allocate.
        if let Some(extra) = self.extra_roots.borrow().0.as_ref() {
            worklist.extend(extra());
        }
        let mut marked = 0;

        while let Some(handle) = worklist.pop() {
            let mut cells = self.cells.borrow_mut();
            let Some(cell) = cells.get_mut(handle.slot() as usize) else {
                continue;
            };
            if cell.generation != handle.generation() {
                continue;
            }
            let State::Live {
                object,
                marked: bit,
            } = &mut cell.state
            else {
                continue;
            };
            if *bit {
                // Already seen. This is what stops a cycle from looping forever, and it is
                // the whole reason a cyclic graph is collectable at all.
                continue;
            }
            *bit = true;
            marked += 1;

            // Collected before releasing the borrow: the worklist cannot be extended while
            // `cells` is held, and re-borrowing per child would be a borrow error rather
            // than merely slow.
            // The prototype is traced like any slot. Missing it would free a class's shared
            // prototype the moment nothing else referred to it — and every instance would go
            // on pointing at a reclaimed object, which is the use-after-free §3.1 names.
            let mut children: Vec<GcRef> = object
                .slots
                .iter()
                .filter_map(|value| value.as_address().map(GcRef::from_address))
                .collect();
            children.extend(object.prototype);
            children.extend(
                object
                    .internals
                    .iter()
                    .filter_map(|value| value.as_address().map(GcRef::from_address)),
            );
            drop(cells);
            worklist.extend(children);
        }
        marked
    }

    /// Frees everything unmarked, clearing the mark bits as it goes.
    fn sweep(&self) -> (usize, usize) {
        let mut cells = self.cells.borrow_mut();
        let mut free = self.free.borrow_mut();
        let mut swept = 0;
        let mut retired = 0;

        for (index, cell) in cells.iter_mut().enumerate() {
            let State::Live { marked, .. } = &mut cell.state else {
                continue;
            };
            if *marked {
                *marked = false;
                continue;
            }
            cell.state = State::Free;
            swept += 1;
            match cell.generation.checked_add(1) {
                Some(next) => {
                    cell.generation = next;
                    free.push(u32::try_from(index).expect("slots fit in u32"));
                }
                // The generation cannot advance, so a new handle for this slot would be
                // indistinguishable from an old one. Retiring the slot leaks it — bounded
                // by the number of slots reused 65,536 times — and the alternative is an
                // ABA bug that reads one object through another's handle.
                None => retired += 1,
            }
        }
        (swept, retired)
    }

    fn allocate(&self, shape: ShapeId, slots: usize, internals: usize) -> GcRef {
        if self.stress.get() {
            self.collect();
        }

        let object = Object {
            shape,
            slots: vec![Value::UNDEFINED; slots],
            prototype: None,
            internals: vec![Value::UNDEFINED; internals],
        };

        let handle = match self.free.borrow_mut().pop() {
            Some(index) => {
                let mut cells = self.cells.borrow_mut();
                let cell = &mut cells[index as usize];
                cell.state = State::Live {
                    object,
                    marked: false,
                };
                GcRef::new(index, cell.generation)
            }
            None => {
                let mut cells = self.cells.borrow_mut();
                let index = u32::try_from(cells.len()).expect("slots fit in u32");
                cells.push(Entry {
                    generation: 0,
                    state: State::Live {
                        object,
                        marked: false,
                    },
                });
                GcRef::new(index, 0)
            }
        };

        let mut stats = self.stats.get();
        stats.allocated += 1;
        self.stats.set(stats);
        handle
    }
}

/// A rooting scope.
///
/// Everything allocated or rooted through it is reachable until it is dropped, at which point
/// the shadow stack is cut back to where it was. Dropping is the only way to unroot, which is
/// the point: there is no `pop` to forget to call.
#[derive(Debug)]
pub struct Scope<'heap> {
    heap: &'heap Heap,
    depth: usize,
}

impl<'heap> Scope<'heap> {
    /// Allocates an object with `slots` slots, rooted for this scope.
    pub fn alloc(&self, shape: ShapeId, slots: usize) -> Rooted<'_> {
        let handle = self.heap.allocate(shape, slots, 0);
        self.root(handle)
    }

    /// Allocates with room for engine-private state as well.
    ///
    /// A closure needs this: its function index and captures must not live in the property
    /// slots, because a shape assigns those from zero and would hand slot zero to the first
    /// property stored on the function.
    pub fn alloc_with_internals(
        &self,
        shape: ShapeId,
        slots: usize,
        internals: usize,
    ) -> Rooted<'_> {
        let handle = self.heap.allocate(shape, slots, internals);
        self.root(handle)
    }

    /// Roots an existing handle for this scope.
    ///
    /// The other half of §3.1: a host function handed a `Value` by compiled code holds
    /// something the collector cannot see until it says so.
    pub fn root(&self, handle: GcRef) -> Rooted<'_> {
        self.heap.roots.borrow_mut().push(handle);
        Rooted {
            handle,
            scope: PhantomData,
        }
    }

    /// The heap this scope belongs to.
    #[must_use]
    pub const fn heap(&self) -> &'heap Heap {
        self.heap
    }

    /// How many roots this scope holds.
    #[must_use]
    pub fn rooted(&self) -> usize {
        self.heap.roots.borrow().len() - self.depth
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        self.heap.roots.borrow_mut().truncate(self.depth);
    }
}

/// A handle that is rooted for as long as its scope lives.
///
/// Carries no data beyond the handle; the lifetime is the whole point. It cannot outlive the
/// [`Scope`] that produced it, so it cannot name an object the collector has been allowed to
/// free.
#[derive(Clone, Copy, Debug)]
pub struct Rooted<'scope> {
    handle: GcRef,
    scope: PhantomData<&'scope ()>,
}

impl Rooted<'_> {
    /// The handle.
    ///
    /// Bare, and so no longer protected by anything: holding one of these across a collection
    /// is the mistake the type system stops being able to catch. [`Heap::is_live`] is how a
    /// caller checks, and every read through a handle checks anyway.
    #[must_use]
    pub const fn handle(self) -> GcRef {
        self.handle
    }

    /// The handle as a [`Value`].
    #[must_use]
    pub fn to_value(self) -> Value {
        self.handle.to_value()
    }
}
