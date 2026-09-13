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

/// An object: its shape, and one value per slot the shape names.
#[derive(Debug)]
struct Object {
    shape: ShapeId,
    slots: Vec<Value>,
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
            let children: Vec<GcRef> = object
                .slots
                .iter()
                .filter_map(|value| value.as_address().map(GcRef::from_address))
                .collect();
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

    fn allocate(&self, shape: ShapeId, slots: usize) -> GcRef {
        if self.stress.get() {
            self.collect();
        }

        let object = Object {
            shape,
            slots: vec![Value::UNDEFINED; slots],
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
        let handle = self.heap.allocate(shape, slots);
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
