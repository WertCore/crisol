//! The type lattice's laws.
//!
//! Asserted as laws rather than as a table of examples, because a lattice that is merely
//! *mostly* a lattice produces an analysis that depends on which order the passes visited
//! things — and that bug looks like a miscompilation weeks later, not like a failing test here.

use crisol_ir::Type;
use crisol_value::{PropertyKey, Shapes};

/// Every type, including two distinct object shapes.
fn every_type() -> Vec<Type> {
    let mut shapes = Shapes::new();
    let one = shapes.add(shapes.root(), &PropertyKey::new("x"));
    let two = shapes.add(shapes.root(), &PropertyKey::new("y"));
    vec![
        Type::Never,
        Type::Undefined,
        Type::Null,
        Type::Bool,
        Type::Number,
        Type::String,
        Type::Object(None),
        Type::object(one),
        Type::object(two),
        Type::Unknown,
    ]
}

#[test]
fn join_is_idempotent() {
    for ty in every_type() {
        assert_eq!(ty.join(ty), ty, "{ty} joined with itself");
    }
}

#[test]
fn join_is_commutative() {
    for a in every_type() {
        for b in every_type() {
            assert_eq!(a.join(b), b.join(a), "{a} and {b}");
        }
    }
}

#[test]
fn join_is_associative() {
    for a in every_type() {
        for b in every_type() {
            for c in every_type() {
                assert_eq!(
                    a.join(b).join(c),
                    a.join(b.join(c)),
                    "{a}, {b}, {c} — without this the answer depends on pass order"
                );
            }
        }
    }
}

#[test]
fn never_is_the_bottom_and_unknown_is_the_top() {
    for ty in every_type() {
        assert_eq!(Type::Never.join(ty), ty, "never joined with {ty}");
        assert_eq!(Type::Unknown.join(ty), Type::Unknown, "unknown with {ty}");
        assert!(Type::Never.is_subtype_of(ty), "never ⊑ {ty}");
        assert!(ty.is_subtype_of(Type::Unknown), "{ty} ⊑ unknown");
    }
}

#[test]
fn the_join_is_an_upper_bound_of_both() {
    for a in every_type() {
        for b in every_type() {
            let joined = a.join(b);
            assert!(a.is_subtype_of(joined), "{a} ⊑ {a} ⊔ {b} = {joined}");
            assert!(b.is_subtype_of(joined), "{b} ⊑ {a} ⊔ {b} = {joined}");
        }
    }
}

#[test]
fn subtyping_and_join_agree() {
    // `a ⊑ b` and `a ⊔ b == b` are two ways of saying the same thing, and a lattice where they
    // disagree will infer one answer and check another.
    for a in every_type() {
        for b in every_type() {
            assert_eq!(
                a.is_subtype_of(b),
                a.join(b) == b,
                "{a} ⊑ {b} should match {a} ⊔ {b} == {b}"
            );
        }
    }
}

#[test]
fn two_different_shapes_join_to_an_object_of_unknown_shape() {
    let mut shapes = Shapes::new();
    let one = shapes.add(shapes.root(), &PropertyKey::new("x"));
    let two = shapes.add(shapes.root(), &PropertyKey::new("y"));

    let joined = Type::object(one).join(Type::object(two));
    assert_eq!(
        joined,
        Type::Object(None),
        "still an object, but which one is no longer known"
    );
    assert_eq!(
        joined.shape(),
        None,
        "and picking one would read a property from the wrong slot"
    );
    assert!(joined.is_object());
}

#[test]
fn the_same_shape_joins_to_itself() {
    let mut shapes = Shapes::new();
    let shape = shapes.add(shapes.root(), &PropertyKey::new("x"));
    assert_eq!(
        Type::object(shape).join(Type::object(shape)),
        Type::object(shape),
        "specialisation survives a merge where both sides agree"
    );
}

#[test]
fn a_number_and_a_string_have_nothing_in_common() {
    assert_eq!(Type::Number.join(Type::String), Type::Unknown);
    assert!(!Type::Number.is_subtype_of(Type::String));
}

#[test]
fn only_objects_carry_a_shape() {
    let mut shapes = Shapes::new();
    let shape = shapes.add(shapes.root(), &PropertyKey::new("x"));
    assert_eq!(Type::object(shape).shape(), Some(shape));
    for ty in [Type::Number, Type::String, Type::Unknown, Type::Never] {
        assert_eq!(ty.shape(), None, "{ty}");
        assert!(!ty.is_object(), "{ty}");
    }
}
