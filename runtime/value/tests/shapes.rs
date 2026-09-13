//! That objects built the same way share a shape, and that the tree does not grow when it
//! should not.
//!
//! The sharing is the entire point — it is what lets an object carry a slot array and no
//! names — so most of these are about *identity* rather than about lookup returning the right
//! answer. A shape system that computed correct slots but handed out a fresh shape per object
//! would pass a naive test suite and be worse than no shape system at all.

use crisol_value::{PropertyKey, Shapes};

fn key(name: &str) -> PropertyKey {
    PropertyKey::new(name)
}

/// Builds `{a, b, c, …}` from the ordinary root.
fn object(shapes: &mut Shapes, names: &[&str]) -> crisol_value::ShapeId {
    let mut shape = shapes.root();
    for name in names {
        shape = shapes.add(shape, &key(name));
    }
    shape
}

// ---- sharing, which is the reason this exists ----------------------------------------

#[test]
fn two_objects_built_the_same_way_share_a_shape() {
    let mut shapes = Shapes::new();
    let first = object(&mut shapes, &["x", "y", "z"]);
    let second = object(&mut shapes, &["x", "y", "z"]);
    assert_eq!(
        first, second,
        "every `{{x, y, z}}` in a program must arrive at one shape"
    );
}

#[test]
fn building_the_same_object_again_adds_no_shapes() {
    let mut shapes = Shapes::new();
    object(&mut shapes, &["x", "y", "z"]);
    let after_first = shapes.count();
    for _ in 0..100 {
        object(&mut shapes, &["x", "y", "z"]);
    }
    assert_eq!(
        shapes.count(),
        after_first,
        "the names were compared when the first object was built and must never be again"
    );
}

#[test]
fn a_prefix_is_shared_with_what_extends_it() {
    let mut shapes = Shapes::new();
    let short = object(&mut shapes, &["x", "y"]);
    let before = shapes.count();
    let long = object(&mut shapes, &["x", "y", "z"]);
    assert_ne!(short, long);
    assert_eq!(
        shapes.count(),
        before + 1,
        "only `z` is new: `{{x, y}}` was already on the way to `{{x, y, z}}`"
    );
}

#[test]
fn assigning_a_property_that_already_exists_does_not_reshape() {
    // The `for (…) obj.x = i` case. Without this the tree grows once per iteration, which is
    // a memory leak shaped like a hidden class.
    let mut shapes = Shapes::new();
    let shape = object(&mut shapes, &["x", "y"]);
    let before = shapes.count();
    let again = shapes.add(shape, &key("x"));
    assert_eq!(again, shape, "assignment is not a transition");
    assert_eq!(shapes.count(), before, "and it allocates no shape");
}

// ---- order, which JavaScript can observe ----------------------------------------------

#[test]
fn property_order_is_part_of_a_shapes_identity() {
    let mut shapes = Shapes::new();
    let forwards = object(&mut shapes, &["x", "y"]);
    let backwards = object(&mut shapes, &["y", "x"]);
    assert_ne!(
        forwards, backwards,
        "`Object.keys` has to produce insertion order, so the two cannot share a shape"
    );
}

#[test]
fn properties_come_back_in_insertion_order() {
    let mut shapes = Shapes::new();
    let shape = object(&mut shapes, &["first", "second", "third"]);
    let names: Vec<String> = shapes
        .properties(shape)
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect();
    assert_eq!(names, ["first", "second", "third"]);
}

#[test]
fn slots_are_handed_out_in_insertion_order() {
    let mut shapes = Shapes::new();
    let shape = object(&mut shapes, &["a", "b", "c"]);
    for (at, name) in ["a", "b", "c"].iter().enumerate() {
        let slot = shapes.lookup(shape, &key(name)).expect("present");
        assert_eq!(slot.index() as usize, at, "{name} should be in slot {at}");
    }
}

// ---- lookup ---------------------------------------------------------------------------

#[test]
fn a_name_that_was_never_added_is_not_found() {
    let mut shapes = Shapes::new();
    let shape = object(&mut shapes, &["x", "y"]);
    assert!(shapes.lookup(shape, &key("z")).is_none());
    assert!(shapes.lookup(shapes.root(), &key("x")).is_none());
}

#[test]
fn a_prefix_shape_does_not_see_what_extends_it() {
    // The walk goes rootward only. `{x}` must not find `y` just because `{x, y}` exists.
    let mut shapes = Shapes::new();
    let short = object(&mut shapes, &["x"]);
    let long = object(&mut shapes, &["x", "y"]);
    assert!(shapes.lookup(long, &key("x")).is_some());
    assert!(
        shapes.lookup(short, &key("y")).is_none(),
        "a shape knows its ancestors, not its descendants"
    );
}

#[test]
fn counts_track_the_properties() {
    let mut shapes = Shapes::new();
    assert!(shapes.is_empty(shapes.root()));
    assert_eq!(shapes.len(shapes.root()), 0);
    let shape = object(&mut shapes, &["a", "b", "c"]);
    assert_eq!(shapes.len(shape), 3);
    assert!(!shapes.is_empty(shape));
}

// ---- exotic, which is §3.2's one predictable branch -----------------------------------

#[test]
fn the_two_roots_are_distinct_and_know_which_they_are() {
    let shapes = Shapes::new();
    assert_ne!(shapes.root(), shapes.exotic_root());
    assert!(!shapes.is_exotic(shapes.root()));
    assert!(shapes.is_exotic(shapes.exotic_root()));
}

#[test]
fn exoticness_survives_every_transition() {
    // If it did not, adding a property to a Proxy would quietly turn it into an ordinary
    // object and the fast path would specialise something it must not.
    let mut shapes = Shapes::new();
    let mut shape = shapes.exotic_root();
    for name in ["a", "b", "c"] {
        shape = shapes.add(shape, &key(name));
        assert!(shapes.is_exotic(shape), "still exotic after adding {name}");
    }
    let ordinary = object(&mut shapes, &["a", "b", "c"]);
    assert!(!shapes.is_exotic(ordinary));
    assert_ne!(
        shape, ordinary,
        "the same names from different roots are different shapes"
    );
}

// ---- the key type ----------------------------------------------------------------------

#[test]
fn two_keys_for_the_same_name_are_equal_without_sharing_an_allocation() {
    let one = PropertyKey::new("property");
    let other = PropertyKey::new("property");
    assert_eq!(one, other);
    assert_eq!(one.hash_value(), other.hash_value());
    assert_eq!(one.as_str(), "property");
}

#[test]
fn keys_are_case_sensitive() {
    // `crisol_tree::Atom` has a lowercasing constructor because HTML names are
    // case-insensitive. Applying that here would be a bug: `obj.X` and `obj.x` are
    // different properties.
    assert_ne!(PropertyKey::new("x"), PropertyKey::new("X"));

    let mut shapes = Shapes::new();
    let shape = object(&mut shapes, &["x", "X"]);
    assert_eq!(shapes.len(shape), 2);
    assert_ne!(
        shapes.lookup(shape, &key("x")),
        shapes.lookup(shape, &key("X"))
    );
}

#[test]
fn an_empty_name_is_a_name() {
    // `obj[""]` is legal JavaScript and must not collide with "no property".
    let mut shapes = Shapes::new();
    let shape = shapes.add(shapes.root(), &key(""));
    assert_eq!(shapes.len(shape), 1);
    assert!(shapes.lookup(shape, &key("")).is_some());
    assert!(shapes.lookup(shapes.root(), &key("")).is_none());
}
