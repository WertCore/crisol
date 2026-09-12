//! The reactive graph: signals, memos, effects and the scopes that own them.

use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::ops::Deref;

use crisol_dom::Dom;

/// A handle into the reactive arena. Generational for the same reason node ids are: a
/// foreign caller will hold one past its disposal, and that must be an error rather than a
/// write to whatever took the slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Slot {
    index: u32,
    generation: u32,
    /// Which runtime issued this handle.
    ///
    /// The price of D-43's decision. A `Signal` is an index, and an index means something
    /// different in every runtime — so handing window A's signal to window B's runtime would
    /// silently read whatever B has in that slot. An ambient thread-local cannot be confused
    /// that way, so a design that rejected one for being unsafe has to answer for it.
    ///
    /// Four bytes on a `Copy` handle, checked on every access, in exchange for a class of bug
    /// that would look like one window's state leaking into another's.
    runtime: RuntimeId,
}

/// Identifies a [`Runtime`], so a handle cannot be used with the wrong one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct RuntimeId(u32);

impl RuntimeId {
    fn next() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(1);
        // Relaxed is enough: the only requirement is that two runtimes never agree, and a
        // fetch_add gives that regardless of ordering between threads.
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A reactive value.
///
/// `Copy` regardless of `T`, because it is an id and not the value — passing one into a
/// closure does not move the data, which is what makes the closures below composable.
pub struct Signal<T> {
    slot: Slot,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Signal<T> {}

impl<T> std::fmt::Debug for Signal<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Signal({}v{})", self.slot.index, self.slot.generation)
    }
}

impl<T> PartialEq for Signal<T> {
    fn eq(&self, other: &Self) -> bool {
        self.slot == other.slot
    }
}

impl<T> Eq for Signal<T> {}

/// A value derived from other reactive values.
///
/// Recomputed lazily on read, not eagerly on write, so a memo nothing is looking at costs
/// nothing. It reads as a [`Signal`] and cannot be written.
pub struct Memo<T> {
    slot: Slot,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Memo<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Memo<T> {}

impl<T> std::fmt::Debug for Memo<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Memo({}v{})", self.slot.index, self.slot.generation)
    }
}

/// An ownership region. Disposing one drops everything created inside it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Scope(Slot);

type MemoFn = Box<dyn FnMut(&Track<'_>) -> Box<dyn Any>>;
type EffectFn = Box<dyn FnMut(&mut Cx<'_, '_>)>;
type CleanupFn = Box<dyn FnOnce(&mut Dom<'_>)>;

enum Kind {
    Signal,
    /// `None` while the closure is out being run — taken so the arena is not borrowed
    /// across a call that will re-enter it.
    Memo(Option<MemoFn>),
    Effect(Option<EffectFn>),
    Free,
}

struct Node {
    generation: u32,
    kind: Kind,
    value: Option<Box<dyn Any>>,
    /// What this node read last time it ran. Cleared and rebuilt on every run, so a branch
    /// that stops reading a signal stops depending on it.
    dependencies: Vec<u32>,
    subscribers: Vec<u32>,
    /// A memo whose inputs changed and which must recompute before it is next read.
    stale: bool,
    queued: bool,
    owner: Option<u32>,
}

impl Node {
    fn free(generation: u32) -> Self {
        Self {
            generation,
            kind: Kind::Free,
            value: None,
            dependencies: Vec::new(),
            subscribers: Vec::new(),
            stale: false,
            queued: false,
            owner: None,
        }
    }
}

struct ScopeData {
    generation: u32,
    alive: bool,
    parent: Option<u32>,
    children: Vec<u32>,
    nodes: Vec<u32>,
    cleanups: Vec<CleanupFn>,
}

#[derive(Default)]
struct Inner {
    nodes: Vec<Node>,
    free: Vec<u32>,
    scopes: Vec<ScopeData>,
    free_scopes: Vec<u32>,
    /// Who is reading, innermost last. A read registers against the top.
    observers: Vec<u32>,
    /// Which scope owns things created right now, innermost last.
    owners: Vec<u32>,
    queue: Vec<u32>,
}

/// Counters describing what the graph did, for tests and for the acceptance measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeStats {
    /// Effect bodies run.
    pub effects_run: usize,
    /// Memo bodies evaluated.
    pub memos_computed: usize,
    /// Scopes disposed.
    pub scopes_disposed: usize,
}

/// Owns the reactive graph.
///
/// Deliberately an ordinary value rather than an ambient thread-local (DECISIONS D-43): a
/// second window means a second runtime, and the JS runtime at M16 needs to say which one it
/// is driving rather than inherit it from whichever thread it happens to be on.
///
/// Handles carry the identity of the runtime that issued them, so using one with a different
/// runtime fails rather than reading whatever that runtime has in the same slot.
pub struct Runtime {
    id: RuntimeId,
    inner: RefCell<Inner>,
    stats: std::cell::Cell<RuntimeStats>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            id: RuntimeId::next(),
            inner: RefCell::default(),
            stats: std::cell::Cell::default(),
        }
    }
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.borrow();
        f.debug_struct("Runtime")
            .field("nodes", &(inner.nodes.len() - inner.free.len()))
            .field("queued", &inner.queue.len())
            .finish()
    }
}

impl Runtime {
    /// An empty runtime.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What the graph has done so far.
    #[must_use]
    pub fn stats(&self) -> RuntimeStats {
        self.stats.get()
    }

    /// Resets the counters, so a caller can measure one update rather than all of them.
    pub fn reset_stats(&self) {
        self.stats.set(RuntimeStats::default());
    }

    /// How many effects are waiting to run.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.inner.borrow().queue.len()
    }

    // ---- arena ------------------------------------------------------------------------

    fn alloc(&self, kind: Kind, value: Option<Box<dyn Any>>) -> Slot {
        let mut inner = self.inner.borrow_mut();
        let owner = inner.owners.last().copied();
        let index = match inner.free.pop() {
            Some(index) => {
                let node = &mut inner.nodes[index as usize];
                node.kind = kind;
                node.value = value;
                node.stale = false;
                node.queued = false;
                node.owner = owner;
                index
            }
            None => {
                let index = u32::try_from(inner.nodes.len()).expect("arena fits in u32");
                inner.nodes.push(Node {
                    generation: 0,
                    kind,
                    value,
                    dependencies: Vec::new(),
                    subscribers: Vec::new(),
                    stale: false,
                    queued: false,
                    owner,
                });
                index
            }
        };
        let generation = inner.nodes[index as usize].generation;
        if let Some(owner) = owner {
            inner.scopes[owner as usize].nodes.push(index);
        }
        Slot {
            index,
            generation,
            runtime: self.id,
        }
    }

    fn live(&self, inner: &Inner, slot: Slot) -> Option<u32> {
        // A handle from another runtime names a slot that exists here and means something
        // else. Rejecting it is the difference between "this signal is not mine" and one
        // window quietly reading another's state.
        if slot.runtime != self.id {
            return None;
        }
        let node = inner.nodes.get(slot.index as usize)?;
        (node.generation == slot.generation && !matches!(node.kind, Kind::Free))
            .then_some(slot.index)
    }

    // ---- signals ----------------------------------------------------------------------

    /// Creates a signal holding `value`, owned by the innermost open scope.
    pub fn signal<T: 'static>(&self, value: T) -> Signal<T> {
        Signal {
            slot: self.alloc(Kind::Signal, Some(Box::new(value))),
            marker: PhantomData,
        }
    }

    /// Creates a memo. The closure may read signals and other memos; it must not touch the
    /// DOM, which is why it is handed a [`Track`] and not a [`Cx`].
    ///
    /// Purity here is not a style preference. A memo runs lazily, at an unpredictable point
    /// inside somebody else's read, and possibly not at all — a DOM write from there would
    /// land at a time no caller could reason about.
    pub fn memo<T: 'static>(&self, mut compute: impl FnMut(&Track<'_>) -> T + 'static) -> Memo<T> {
        let slot = self.alloc(
            Kind::Memo(Some(Box::new(move |track| Box::new(compute(track))))),
            None,
        );
        self.inner.borrow_mut().nodes[slot.index as usize].stale = true;
        Memo {
            slot,
            marker: PhantomData,
        }
    }

    /// Reads a signal without subscribing to it.
    ///
    /// The escape hatch for an effect that needs a value but must not re-run when it
    /// changes. Using it by accident is how a UI stops updating, so it is named to be
    /// conspicuous at the call site.
    #[must_use]
    pub fn peek<T: Clone + 'static>(&self, signal: Signal<T>) -> Option<T> {
        let index = self.live(&self.inner.borrow(), signal.slot)?;
        self.read_value(index, T::clone)
    }

    /// Writes a signal and queues whatever depended on it.
    ///
    /// Notifies unconditionally; it does not compare against the old value, because `T` need
    /// not be comparable. A redundant write costs an effect run, not a DOM write — the
    /// mutation API drops writes that change nothing. Use [`Self::set_if_changed`] to skip
    /// even the effect.
    ///
    /// Returns whether the signal was still alive.
    pub fn set<T: 'static>(&self, signal: Signal<T>, value: T) -> bool {
        let index = {
            let mut inner = self.inner.borrow_mut();
            let Some(index) = self.live(&inner, signal.slot) else {
                return false;
            };
            inner.nodes[index as usize].value = Some(Box::new(value));
            index
        };
        self.notify(index);
        true
    }

    /// Writes a signal only if the value differs. Returns whether anything changed.
    pub fn set_if_changed<T: PartialEq + 'static>(&self, signal: Signal<T>, value: T) -> bool {
        {
            let inner = self.inner.borrow();
            let Some(index) = self.live(&inner, signal.slot) else {
                return false;
            };
            if inner.nodes[index as usize]
                .value
                .as_ref()
                .and_then(|existing| existing.downcast_ref::<T>())
                == Some(&value)
            {
                return false;
            }
        }
        self.set(signal, value)
    }

    /// Mutates a signal in place and queues whatever depended on it.
    ///
    /// The only way to change a large value — a `Vec` of a thousand todos — without cloning
    /// it to change one field.
    pub fn update<T: 'static, R>(
        &self,
        signal: Signal<T>,
        edit: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        let (index, result) = {
            let mut inner = self.inner.borrow_mut();
            let index = self.live(&inner, signal.slot)?;
            let value = inner.nodes[index as usize]
                .value
                .as_mut()?
                .downcast_mut::<T>()?;
            (index, edit(value))
        };
        self.notify(index);
        Some(result)
    }

    /// Marks everything downstream of `index` as needing attention.
    ///
    /// Memos are marked stale rather than recomputed, so a memo nobody reads is never run.
    /// Effects are queued, because something has to do the work eventually.
    fn notify(&self, index: u32) {
        let mut inner = self.inner.borrow_mut();
        let mut stack = inner.nodes[index as usize].subscribers.clone();
        while let Some(current) = stack.pop() {
            let node = &mut inner.nodes[current as usize];
            match node.kind {
                Kind::Memo(_) => {
                    if !node.stale {
                        node.stale = true;
                        stack.extend(node.subscribers.iter().copied());
                    }
                }
                Kind::Effect(_) => {
                    if !node.queued {
                        node.queued = true;
                        inner.queue.push(current);
                    }
                }
                Kind::Signal | Kind::Free => {}
            }
        }
    }

    /// Reads a node's value without holding the arena borrow while `read` runs.
    ///
    /// The borrow matters: a closure that reads a second signal — filtering a list of todos
    /// by a flag each one owns, say — would otherwise panic inside `RefCell`. An API whose
    /// reads cannot nest is not one a foreign caller can drive, and M16's caller will nest
    /// them without asking.
    fn read_value<T: 'static, R>(&self, index: u32, read: impl FnOnce(&T) -> R) -> Option<R> {
        let (taken, generation) = {
            let mut inner = self.inner.borrow_mut();
            let node = &mut inner.nodes[index as usize];
            (node.value.take()?, node.generation)
        };
        let result = taken.downcast_ref::<T>().map(read);

        let mut inner = self.inner.borrow_mut();
        let node = &mut inner.nodes[index as usize];
        // Do not resurrect a value the closure overwrote, or one whose slot was freed and
        // handed to somebody else while we were out.
        if node.generation == generation && node.value.is_none() {
            node.value = Some(taken);
        }
        result
    }

    fn track_read(&self, index: u32) {
        let mut inner = self.inner.borrow_mut();
        let Some(&observer) = inner.observers.last() else {
            return;
        };
        if observer == index {
            return;
        }
        if !inner.nodes[observer as usize].dependencies.contains(&index) {
            inner.nodes[observer as usize].dependencies.push(index);
            inner.nodes[index as usize].subscribers.push(observer);
        }
    }

    /// Drops every dependency edge into `index`, so a re-run can rebuild them from scratch.
    fn clear_dependencies(inner: &mut Inner, index: u32) {
        let dependencies = std::mem::take(&mut inner.nodes[index as usize].dependencies);
        for dependency in dependencies {
            inner.nodes[dependency as usize]
                .subscribers
                .retain(|&subscriber| subscriber != index);
        }
    }

    /// Recomputes a memo if its inputs changed since it was last read.
    fn refresh(&self, index: u32) {
        let mut compute = {
            let mut inner = self.inner.borrow_mut();
            let node = &mut inner.nodes[index as usize];
            if !node.stale {
                return;
            }
            match &mut node.kind {
                Kind::Memo(slot) => match slot.take() {
                    // Re-entered while already computing: a cycle. Leave the stale value
                    // rather than recursing forever.
                    None => return,
                    Some(compute) => compute,
                },
                _ => return,
            }
        };

        {
            let mut inner = self.inner.borrow_mut();
            Self::clear_dependencies(&mut inner, index);
            inner.observers.push(index);
        }
        let value = compute(&Track { runtime: self });
        let mut stats = self.stats.get();
        stats.memos_computed += 1;
        self.stats.set(stats);

        let mut inner = self.inner.borrow_mut();
        inner.observers.pop();
        let node = &mut inner.nodes[index as usize];
        node.value = Some(value);
        node.stale = false;
        node.kind = Kind::Memo(Some(compute));
    }

    // ---- effects ----------------------------------------------------------------------

    /// Creates an effect and runs it once, so it can build what it is responsible for and
    /// record what it read.
    ///
    /// The first run is not an optimisation detail: an effect that has never run has no
    /// dependencies, and would never run again.
    ///
    /// An effect is stopped by disposing the [`Scope`] that owns it, so there is no handle to
    /// return — one that could not be used for anything would only suggest otherwise.
    pub fn effect(&self, dom: &mut Dom<'_>, body: impl FnMut(&mut Cx<'_, '_>) + 'static) {
        let slot = self.alloc(Kind::Effect(Some(Box::new(body))), None);
        self.run_effect(slot.index, dom);
    }

    fn run_effect(&self, index: u32, dom: &mut Dom<'_>) {
        let mut body = {
            let mut inner = self.inner.borrow_mut();
            let node = &mut inner.nodes[index as usize];
            node.queued = false;
            match &mut node.kind {
                Kind::Effect(slot) => match slot.take() {
                    None => return,
                    Some(body) => body,
                },
                _ => return,
            }
        };

        {
            let mut inner = self.inner.borrow_mut();
            Self::clear_dependencies(&mut inner, index);
            inner.observers.push(index);
        }
        body(&mut Cx {
            track: Track { runtime: self },
            dom,
        });
        let mut stats = self.stats.get();
        stats.effects_run += 1;
        self.stats.set(stats);

        let mut inner = self.inner.borrow_mut();
        inner.observers.pop();
        // The effect may have been disposed by its own body; only restore it if the slot
        // is still the one we took from.
        if let Kind::Effect(slot @ None) = &mut inner.nodes[index as usize].kind {
            *slot = Some(body);
        }
    }

    /// Runs every queued effect, and everything their writes queue in turn.
    ///
    /// This is the point where reactivity touches the tree. Nothing mutates the DOM between
    /// flushes, so a frame sees one consistent state rather than a half-applied update.
    pub fn flush(&self, dom: &mut Dom<'_>) {
        // A bound, not a policy: an effect that writes a signal it reads is a cycle, and
        // the alternative to stopping is hanging.
        let mut budget = 1_000_000_usize;
        loop {
            let Some(index) = self.inner.borrow_mut().queue.pop() else {
                return;
            };
            self.run_effect(index, dom);
            budget = budget.saturating_sub(1);
            assert!(
                budget > 0,
                "reactive update did not settle: a cycle between an effect and a signal it reads"
            );
        }
    }

    // ---- scopes -----------------------------------------------------------------------

    /// Opens a scope, runs `body`, and closes it. Everything created inside belongs to the
    /// returned scope and dies with it.
    pub fn scope<R>(&self, body: impl FnOnce(Scope) -> R) -> (Scope, R) {
        let index = {
            let mut inner = self.inner.borrow_mut();
            let parent = inner.owners.last().copied();
            let index = match inner.free_scopes.pop() {
                Some(index) => {
                    let scope = &mut inner.scopes[index as usize];
                    scope.alive = true;
                    scope.parent = parent;
                    index
                }
                None => {
                    let index = u32::try_from(inner.scopes.len()).expect("arena fits in u32");
                    inner.scopes.push(ScopeData {
                        generation: 0,
                        alive: true,
                        parent,
                        children: Vec::new(),
                        nodes: Vec::new(),
                        cleanups: Vec::new(),
                    });
                    index
                }
            };
            if let Some(parent) = parent {
                inner.scopes[parent as usize].children.push(index);
            }
            inner.owners.push(index);
            index
        };

        let generation = self.inner.borrow().scopes[index as usize].generation;
        let scope = Scope(Slot {
            index,
            generation,
            runtime: self.id,
        });
        let result = body(scope);
        self.inner.borrow_mut().owners.pop();
        (scope, result)
    }

    /// Registers work to do when the innermost open scope is disposed.
    ///
    /// This is how a list item removes its own nodes: the reconciler does not need to know
    /// what a component built, only that disposing it undoes it.
    pub fn on_cleanup(&self, cleanup: impl FnOnce(&mut Dom<'_>) + 'static) -> bool {
        let mut inner = self.inner.borrow_mut();
        let Some(&owner) = inner.owners.last() else {
            return false;
        };
        inner.scopes[owner as usize]
            .cleanups
            .push(Box::new(cleanup));
        true
    }

    /// Disposes a scope: runs its cleanups innermost-first, then frees everything it owned.
    ///
    /// Returns whether the scope was alive.
    pub fn dispose(&self, scope: Scope, dom: &mut Dom<'_>) -> bool {
        let index = {
            if scope.0.runtime != self.id {
                return false;
            }
            let inner = self.inner.borrow();
            let Some(data) = inner.scopes.get(scope.0.index as usize) else {
                return false;
            };
            if !data.alive || data.generation != scope.0.generation {
                return false;
            }
            scope.0.index
        };

        // Detach from the parent first, so a parent disposed later does not walk into it.
        {
            let mut inner = self.inner.borrow_mut();
            if let Some(parent) = inner.scopes[index as usize].parent.take() {
                inner.scopes[parent as usize]
                    .children
                    .retain(|&child| child != index);
            }
        }
        self.dispose_inner(index, dom);
        true
    }

    fn dispose_inner(&self, index: u32, dom: &mut Dom<'_>) {
        let children = std::mem::take(&mut self.inner.borrow_mut().scopes[index as usize].children);
        for child in children {
            self.dispose_inner(child, dom);
        }

        // Cleanups run outside the borrow: they take `&mut Dom` and may create or dispose
        // further scopes.
        let cleanups = std::mem::take(&mut self.inner.borrow_mut().scopes[index as usize].cleanups);
        for cleanup in cleanups {
            cleanup(dom);
        }

        let mut inner = self.inner.borrow_mut();
        let nodes = std::mem::take(&mut inner.scopes[index as usize].nodes);
        for node in nodes {
            Self::clear_dependencies(&mut inner, node);
            let subscribers = std::mem::take(&mut inner.nodes[node as usize].subscribers);
            for subscriber in subscribers {
                inner.nodes[subscriber as usize]
                    .dependencies
                    .retain(|&dependency| dependency != node);
            }
            inner.queue.retain(|&queued| queued != node);
            let generation = inner.nodes[node as usize].generation.wrapping_add(1);
            inner.nodes[node as usize] = Node::free(generation);
            inner.free.push(node);
        }

        let scope = &mut inner.scopes[index as usize];
        scope.alive = false;
        scope.generation = scope.generation.wrapping_add(1);
        inner.free_scopes.push(index);
        drop(inner);

        let mut stats = self.stats.get();
        stats.scopes_disposed += 1;
        self.stats.set(stats);
    }
}

/// A read-only reactive context: reading through it subscribes whoever is running.
#[derive(Clone, Copy)]
pub struct Track<'a> {
    runtime: &'a Runtime,
}

impl<'a> Track<'a> {
    /// The runtime behind this context.
    #[must_use]
    pub fn runtime(&self) -> &'a Runtime {
        self.runtime
    }

    /// Reads a signal, subscribing to it.
    ///
    /// # Panics
    ///
    /// If the signal was disposed. Holding a handle past its scope is a caller bug, and
    /// silently returning a default would hide it; [`Self::try_get`] is the checked form.
    #[must_use]
    pub fn get<T: Clone + 'static>(&self, signal: Signal<T>) -> T {
        self.try_get(signal)
            .expect("signal is disposed, or is already being read further up the stack")
    }

    /// Reads a signal, subscribing to it. `None` if it was disposed.
    #[must_use]
    pub fn try_get<T: Clone + 'static>(&self, signal: Signal<T>) -> Option<T> {
        self.with(signal, Clone::clone)
    }

    /// Reads a signal by reference, subscribing to it, without cloning.
    #[must_use]
    pub fn with<T: 'static, R>(&self, signal: Signal<T>, read: impl FnOnce(&T) -> R) -> Option<R> {
        let index = self.runtime.live(&self.runtime.inner.borrow(), signal.slot)?;
        self.runtime.track_read(index);
        self.runtime.read_value(index, read)
    }

    /// Reads a memo, recomputing it first if its inputs changed.
    ///
    /// # Panics
    ///
    /// If the memo was disposed.
    #[must_use]
    pub fn memo<T: Clone + 'static>(&self, memo: Memo<T>) -> T {
        self.with_memo(memo, Clone::clone)
            .expect("memo is disposed, or is already being read further up the stack")
    }

    /// Reads a memo by reference, recomputing it first if its inputs changed.
    #[must_use]
    pub fn with_memo<T: 'static, R>(&self, memo: Memo<T>, read: impl FnOnce(&T) -> R) -> Option<R> {
        let index = self.runtime.live(&self.runtime.inner.borrow(), memo.slot)?;
        // Freshen before subscribing, so the memo's own reads register against the memo and
        // not against whoever is asking for it.
        self.runtime.refresh(index);
        self.runtime.track_read(index);
        self.runtime.read_value(index, read)
    }
}

/// What an effect body is handed: reactive reads, plus the DOM to write.
pub struct Cx<'a, 'd> {
    track: Track<'a>,
    /// The tree, for mutation.
    pub dom: &'a mut Dom<'d>,
}

impl<'a> Deref for Cx<'a, '_> {
    type Target = Track<'a>;

    fn deref(&self) -> &Self::Target {
        &self.track
    }
}

impl<'a, 'd> Cx<'a, 'd> {
    /// Borrows a runtime and a DOM as an effect context, for code that builds a view
    /// outside any effect.
    pub fn new(runtime: &'a Runtime, dom: &'a mut Dom<'d>) -> Self {
        Self {
            track: Track { runtime },
            dom,
        }
    }
}
