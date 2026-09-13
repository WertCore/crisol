//! What the collector keeps and what it reclaims.
//!
//! ROADMAP §M9's acceptance is at the top: a cyclic object graph whose roots are dropped is
//! reclaimed. A collector that leaks cycles is the failure this milestone is most likely to
//! ship by accident, because everything *else* works — refcounting passes every test here
//! except that one.

use crisol_gc::{GcRef, Heap};
use crisol_value::{Address, PropertyKey, Shapes, Value};

/// `{next: …}` — one slot, so objects can point at each other.
fn linked(shapes: &mut Shapes) -> crisol_value::ShapeId {
    shapes.add(shapes.root(), &PropertyKey::new("next"))
}

// ---- §M9's acceptance -----------------------------------------------------------------

#[test]
fn a_cycle_whose_roots_are_dropped_is_reclaimed() {
    let mut shapes = Shapes::new();
    let shape = linked(&mut shapes);
    let heap = Heap::new();

    let (first, second) = {
        let scope = heap.scope();
        let a = scope.alloc(shape, 1);
        let b = scope.alloc(shape, 1);
        // a -> b -> a. Nothing outside the cycle refers to either.
        heap.set(a.handle(), 0, b.to_value());
        heap.set(b.handle(), 0, a.to_value());

        assert_eq!(heap.collect().swept, 0, "both are rooted here");
        assert_eq!(heap.live(), 2);
        (a.handle(), b.handle())
    };
    // The scope is gone, so the shadow stack is empty and the only references left are the
    // ones the two objects hold to each other.

    let collected = heap.collect();
    assert_eq!(
        collected.swept, 2,
        "a cycle is garbage once nothing else names it"
    );
    assert_eq!(collected.marked, 0);
    assert_eq!(heap.live(), 0);
    assert!(!heap.is_live(first));
    assert!(!heap.is_live(second));
}

#[test]
fn a_longer_cycle_is_reclaimed_too() {
    let mut shapes = Shapes::new();
    let shape = linked(&mut shapes);
    let heap = Heap::new();

    {
        let scope = heap.scope();
        let ring: Vec<GcRef> = (0..8).map(|_| scope.alloc(shape, 1).handle()).collect();
        for (at, handle) in ring.iter().enumerate() {
            let next = ring[(at + 1) % ring.len()];
            heap.set(*handle, 0, next.to_value());
        }
    }

    assert_eq!(heap.collect().swept, 8);
    assert_eq!(heap.live(), 0);
}

// ---- reachability ----------------------------------------------------------------------

#[test]
fn a_rooted_object_survives() {
    let shapes = Shapes::new();
    let heap = Heap::new();
    let scope = heap.scope();
    let object = scope.alloc(shapes.root(), 0);

    let collected = heap.collect();
    assert_eq!(collected.marked, 1);
    assert_eq!(collected.swept, 0);
    assert!(heap.is_live(object.handle()));
}

#[test]
fn an_unrooted_object_does_not() {
    let shapes = Shapes::new();
    let heap = Heap::new();
    let handle = {
        let scope = heap.scope();
        scope.alloc(shapes.root(), 0).handle()
    };
    assert_eq!(heap.collect().swept, 1);
    assert!(!heap.is_live(handle));
}

#[test]
fn reachability_is_transitive() {
    let mut shapes = Shapes::new();
    let shape = linked(&mut shapes);
    let heap = Heap::new();

    let scope = heap.scope();
    let a = scope.alloc(shape, 1);
    // b and c are allocated in an inner scope that ends, so only the chain from `a` keeps
    // them alive.
    let (b, c) = {
        let inner = heap.scope();
        let b = inner.alloc(shape, 1);
        let c = inner.alloc(shape, 1);
        heap.set(a.handle(), 0, b.to_value());
        heap.set(b.handle(), 0, c.to_value());
        (b.handle(), c.handle())
    };

    let collected = heap.collect();
    assert_eq!(collected.marked, 3, "a keeps b, and b keeps c");
    assert_eq!(collected.swept, 0);
    assert!(heap.is_live(b) && heap.is_live(c));
}

#[test]
fn breaking_the_chain_releases_the_tail() {
    let mut shapes = Shapes::new();
    let shape = linked(&mut shapes);
    let heap = Heap::new();

    let scope = heap.scope();
    let a = scope.alloc(shape, 1);
    let b = {
        let inner = heap.scope();
        let b = inner.alloc(shape, 1);
        heap.set(a.handle(), 0, b.to_value());
        b.handle()
    };
    assert_eq!(heap.collect().swept, 0);

    heap.set(a.handle(), 0, Value::NULL);
    assert_eq!(
        heap.collect().swept,
        1,
        "b is unreachable once a stops naming it"
    );
    assert!(!heap.is_live(b));
    assert!(heap.is_live(a.handle()));
}

// ---- scopes ----------------------------------------------------------------------------

#[test]
fn a_scope_unroots_exactly_what_it_rooted() {
    let shapes = Shapes::new();
    let heap = Heap::new();

    let outer = heap.scope();
    let kept = outer.alloc(shapes.root(), 0);
    assert_eq!(outer.rooted(), 1);

    {
        let inner = heap.scope();
        inner.alloc(shapes.root(), 0);
        inner.alloc(shapes.root(), 0);
        assert_eq!(inner.rooted(), 2);
        assert_eq!(
            outer.rooted(),
            3,
            "the outer scope sees the stack, not its own share"
        );
    }

    assert_eq!(
        outer.rooted(),
        1,
        "the inner scope took its two back with it"
    );
    assert_eq!(heap.collect().swept, 2);
    assert!(heap.is_live(kept.handle()));
}

#[test]
fn rooting_an_existing_handle_keeps_it() {
    // The other half of §3.1: a host function handed a value by compiled code holds something
    // the collector cannot see until it says so.
    let shapes = Shapes::new();
    let heap = Heap::new();
    let handle = {
        let scope = heap.scope();
        scope.alloc(shapes.root(), 0).handle()
    };

    let scope = heap.scope();
    let rooted = scope.root(handle);
    assert_eq!(heap.collect().swept, 0, "re-rooted before the collection");
    assert!(heap.is_live(rooted.handle()));
}

// ---- stale handles ---------------------------------------------------------------------

#[test]
fn a_stale_handle_does_not_resolve_to_whatever_took_its_slot() {
    // The D-17 argument, applied to a collector. Without the generation this is a
    // use-after-free that reads one object through another's handle, and §3.1 calls that
    // the worst failure mode to debug.
    let shapes = Shapes::new();
    let heap = Heap::new();

    let stale = {
        let scope = heap.scope();
        scope.alloc(shapes.root(), 1).handle()
    };
    assert_eq!(heap.collect().swept, 1);

    let scope = heap.scope();
    let fresh = scope.alloc(shapes.root(), 1);
    assert_eq!(
        fresh.handle().slot(),
        stale.slot(),
        "the fixture needs the slot to actually be reused, or this proves nothing"
    );

    assert!(!heap.is_live(stale));
    assert_eq!(heap.get(stale, 0), None, "reading through it fails");
    assert!(!heap.set(stale, 0, Value::TRUE), "and so does writing");
    assert_eq!(
        heap.get(fresh.handle(), 0),
        Some(Value::UNDEFINED),
        "while the new object is perfectly readable"
    );
}

#[test]
fn a_slot_is_reused_rather_than_leaked() {
    let shapes = Shapes::new();
    let heap = Heap::new();
    for _ in 0..100 {
        let scope = heap.scope();
        scope.alloc(shapes.root(), 0);
        drop(scope);
        heap.collect();
    }
    assert_eq!(heap.stats().allocated, 100);
    assert!(
        heap.live() <= 1,
        "at most the last one, and the rest reused one slot"
    );
}

// ---- stress mode -------------------------------------------------------------------------

#[test]
fn stress_mode_collects_on_every_allocation() {
    let shapes = Shapes::new();
    let heap = Heap::new();
    heap.set_stress(true);
    assert!(heap.stress());

    let scope = heap.scope();
    let first = scope.alloc(shapes.root(), 0);
    let second = scope.alloc(shapes.root(), 0);

    assert!(heap.stats().collections >= 2, "one per allocation");
    assert!(
        heap.is_live(first.handle()) && heap.is_live(second.handle()),
        "rooted objects survive a collection that happens between allocations"
    );
}

#[test]
fn stress_mode_reclaims_an_unrooted_object_on_the_very_next_allocation() {
    // This is what stress mode is for: it turns "a missing root is a bug under memory
    // pressure" into "a missing root is a bug on the next line".
    let shapes = Shapes::new();
    let heap = Heap::new();
    let unrooted = {
        let scope = heap.scope();
        scope.alloc(shapes.root(), 0).handle()
    };
    heap.set_stress(true);

    let scope = heap.scope();
    scope.alloc(shapes.root(), 0);
    assert!(
        !heap.is_live(unrooted),
        "gone at the next allocation, not eventually"
    );
}

// ---- handles and values -----------------------------------------------------------------

#[test]
fn a_handle_round_trips_through_a_value() {
    let shapes = Shapes::new();
    let heap = Heap::new();
    let scope = heap.scope();
    let object = scope.alloc(shapes.root(), 0);

    let value = object.to_value();
    let address = value.as_address().expect("an object value carries one");
    assert_eq!(GcRef::from_address(address), object.handle());
}

#[test]
fn a_handle_fits_the_forty_eight_bits_a_value_carries() {
    // The coupling between D-53 and this crate: a handle that did not fit would have to be
    // boxed, and every object reference in the language would cost an indirection.
    let handle = GcRef::from_address(Address::new(Address::MAX).expect("in range"));
    assert_eq!(handle.to_address().get(), Address::MAX);
    assert_eq!(handle.slot(), u32::MAX);
    assert_eq!(handle.generation(), u16::MAX);
}

#[test]
fn an_object_knows_its_shape() {
    let mut shapes = Shapes::new();
    let shape = linked(&mut shapes);
    let heap = Heap::new();
    let scope = heap.scope();
    let object = scope.alloc(shape, 1);
    assert_eq!(heap.shape_of(object.handle()), Some(shape));
}

#[test]
fn slots_start_undefined_and_hold_what_is_written() {
    let mut shapes = Shapes::new();
    let shape = linked(&mut shapes);
    let heap = Heap::new();
    let scope = heap.scope();
    let object = scope.alloc(shape, 1);

    assert_eq!(heap.get(object.handle(), 0), Some(Value::UNDEFINED));
    assert!(heap.set(object.handle(), 0, Value::number(42.0)));
    assert_eq!(heap.get(object.handle(), 0), Some(Value::number(42.0)));
    assert_eq!(
        heap.get(object.handle(), 1),
        None,
        "there is no second slot"
    );
    assert!(!heap.set(object.handle(), 1, Value::TRUE));
}

#[test]
fn a_number_in_a_slot_is_not_followed_as_a_pointer() {
    // Precise, not conservative: an object is retained because something points at it, never
    // because an integer happened to look like an address.
    let mut shapes = Shapes::new();
    let shape = linked(&mut shapes);
    let heap = Heap::new();

    let victim = {
        let scope = heap.scope();
        scope.alloc(shape, 1).handle()
    };
    let scope = heap.scope();
    let holder = scope.alloc(shape, 1);
    // The victim's handle, as a *number* rather than as an object reference.
    let as_number = Value::number(f64::from_bits(victim.to_address().get()));
    heap.set(holder.handle(), 0, as_number);

    assert_eq!(heap.collect().swept, 1, "the number does not keep it alive");
    assert!(!heap.is_live(victim));
}

// ---- the stress half of §M9's acceptance -------------------------------------------------

/// A mixed workload with stress mode on: allocate, link, unlink, drop scopes, collect.
///
/// §M9's acceptance asks for "stress mode runs the full suite with zero use-after-free under
/// ASAN". **That clause is vacuous for this design rather than satisfied by it**, and saying
/// so is more useful than a green ASAN run would be: there is no `unsafe` in `crisol-gc` or
/// `crisol-value`, so a use-after-free in the sense ASAN detects is not expressible. A handle
/// whose object was collected fails its generation check and returns `None`.
///
/// That is stronger than the acceptance asks for, and it is not free: it holds because
/// objects live in a slab behind checked handles rather than behind raw pointers. When
/// compiled code dereferences objects directly (M13), the checks stop being free and ASAN
/// starts having something to look at. The trade is recorded in D-55 rather than assumed to
/// last.
///
/// What this test does cover is the part that is real either way: that a workload which
/// allocates, links and unlinks under collection-on-every-allocation neither loses a rooted
/// object nor retains an unrooted one.
#[test]
fn a_mixed_workload_under_stress_mode_keeps_exactly_what_is_reachable() {
    let mut shapes = Shapes::new();
    let shape = linked(&mut shapes);
    let heap = Heap::new();
    heap.set_stress(true);

    let outer = heap.scope();
    let anchor = outer.alloc(shape, 1);
    let mut expected_tail: Option<GcRef> = None;

    for round in 0..50 {
        let inner = heap.scope();
        let a = inner.alloc(shape, 1);
        let b = inner.alloc(shape, 1);
        heap.set(a.handle(), 0, b.to_value());

        if round % 2 == 0 {
            // Reachable from the anchor, so it must survive the inner scope ending.
            heap.set(anchor.handle(), 0, a.to_value());
            expected_tail = Some(b.handle());
        }

        // Everything allocated in `inner` that the anchor does not reach goes here.
        drop(inner);
        heap.collect();

        assert!(
            heap.is_live(anchor.handle()),
            "the anchor is rooted throughout"
        );
        if let Some(tail) = expected_tail {
            assert!(
                heap.is_live(tail),
                "round {round}: anchor -> a -> b, so b is reachable and must not be swept"
            );
        }
    }

    // Cut the chain and everything behind it goes.
    heap.set(anchor.handle(), 0, Value::NULL);
    heap.collect();
    assert!(heap.is_live(anchor.handle()));
    assert_eq!(heap.live(), 1, "only the anchor is reachable now");
    assert!(heap.stats().collections >= 50);
}
