//! M6's acceptance, and the invalidation it rests on.
//!
//! > In a 10,000-node tree, mutating one text node's content triggers relayout of fewer than
//! > 20 nodes and repaints only the damaged rectangle. Instrumented counters prove it.
//!
//! The counters are the point. A test that only asserted the *result* was right would pass
//! just as well against an engine that recomputed everything — which is precisely the engine
//! this milestone exists to stop being.

mod harness;

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::Size;
use crisol_layout::{LayoutCache, LayoutContext};
use crisol_style::{StyleEngine, StyleMap};
use crisol_text::FontSystem;
use crisol_tree::{DirtyFlags, NodeId, Tree};

const VIEWPORT: Size = Size {
    width: 400.0,
    height: 600.0,
};

const CSS: &str = "p { height: 20px; font-size: 12px }";

/// A document of `paragraphs` paragraphs, each holding one text node.
///
/// Built by hand rather than parsed, because `crisol-html` already dev-depends on this crate
/// and the reverse would be a dependency cycle.
fn document(paragraphs: usize) -> (Tree, Vec<NodeId>) {
    let mut tree = Tree::with_capacity(paragraphs * 3 + 4);
    let root = tree.create_element("html");
    tree.set_root(root).unwrap();
    let body = tree.create_element("body");
    tree.append_child(root, body).unwrap();

    let mut texts = Vec::with_capacity(paragraphs);
    for i in 0..paragraphs {
        let p = tree.create_element("p");
        tree.append_child(body, p).unwrap();
        let text = tree.create_text(format!("paragraph number {i}"));
        tree.append_child(p, text).unwrap();
        texts.push(text);
    }
    (tree, texts)
}

fn styles(tree: &Tree) -> StyleMap {
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).unwrap());
    engine.restyle(tree).0
}

// ---- M6's acceptance -----------------------------------------------------------------------

#[test]
fn one_text_edit_in_a_ten_thousand_node_document_relayouts_a_handful_of_nodes() {
    let paragraphs = 5_000;
    let (mut tree, texts) = document(paragraphs);
    let node_count = tree.len();
    assert!(node_count >= 10_000, "fixture is only {node_count} nodes");

    let map = styles(&tree);
    let mut fonts = FontSystem::empty();
    // The cache the frame loop owns. Everything below depends on it surviving the edit,
    // which is why it lives out here rather than inside the context.
    let mut cache = LayoutCache::new();

    let first = {
        let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
        context.run(VIEWPORT);
        context.stats()
    };
    assert!(
        first.nodes_laid_out > 5_000,
        "the first pass really does lay everything out: {}",
        first.nodes_laid_out
    );

    // One edit, halfway down.
    tree.set_text(texts[paragraphs / 2], "paragraph number 2500 edited");

    let second = {
        let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
        context.run(VIEWPORT);
        context.stats()
    };

    assert!(
        second.caches_invalidated < 20,
        "one text edit should invalidate the edited node and its ancestors — got {} of {} nodes",
        second.caches_invalidated,
        node_count
    );
    assert!(
        second.nodes_laid_out < 20,
        "and taffy should recompute only those — got {} of {} nodes",
        second.nodes_laid_out,
        node_count
    );
    assert!(
        second.unchanged > paragraphs,
        "nearly every box should come out identical: {} unchanged",
        second.unchanged
    );
}

#[test]
fn the_second_pass_repaints_only_the_damaged_rectangle() {
    // Real fonts, so the edited text genuinely changes width and there is damage to measure.
    // With an empty font system every string measures to nothing and this passes vacuously.
    let mut fonts = FontSystem::new();
    if fonts.is_empty() {
        eprintln!("skipping: no fonts installed");
        return;
    }

    let paragraphs = 200;
    let (mut tree, texts) = document(paragraphs);
    let map = styles(&tree);
    let mut cache = LayoutCache::new();

    {
        let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
        context.run(VIEWPORT);
    }

    // A paragraph in the middle. Its height is fixed by CSS, so its box does not move and
    // neither does anything after it — only the text inside it changes width.
    let index = 100;
    tree.set_text(
        texts[index],
        "a substantially longer line than the one that was here",
    );

    let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
    context.run(VIEWPORT);

    let damage = context
        .damage()
        .expect("the text changed width, so something moved");
    assert!(
        damage.height() <= 20.0,
        "the damage should be the one paragraph that changed, not the document: {damage:?}"
    );

    // And it should be *that* paragraph, not some other one.
    let expected_top = index as f32 * 20.0;
    assert!(
        (damage.min_y() - expected_top).abs() < 0.01,
        "the damage should sit at the edited paragraph ({expected_top}), got {damage:?}"
    );
}

#[test]
fn a_pass_with_no_changes_costs_nothing_and_damages_nothing() {
    let (mut tree, _) = document(500);
    let map = styles(&tree);
    let mut fonts = FontSystem::empty();
    let mut cache = LayoutCache::new();

    {
        let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
        context.run(VIEWPORT);
    }

    let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
    context.run(VIEWPORT);
    let stats = context.stats();

    assert_eq!(stats.caches_invalidated, 0, "nothing was dirty");
    assert_eq!(stats.nodes_laid_out, 0, "so taffy recomputed nothing");
    assert_eq!(context.damage(), None, "and there is nothing to repaint");
}

#[test]
fn the_damage_of_a_shrinking_box_covers_where_it_used_to_be() {
    // A box that shrank leaves pixels behind that have to be painted over, so the damage is
    // the union of the old box and the new one. Taking only the new one leaves a ghost.
    let (mut tree, _) = document(1);
    let mut fonts = FontSystem::empty();
    let mut cache = LayoutCache::new();

    let mut tall = StyleEngine::new();
    tall.add_stylesheet(Stylesheet::parse("p { height: 100px }").unwrap());
    let map = tall.restyle(&tree).0;
    {
        let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
        context.run(VIEWPORT);
    }

    let mut short = StyleEngine::new();
    short.add_stylesheet(Stylesheet::parse("p { height: 10px }").unwrap());
    let map = short.restyle(&tree).0;
    tree.mark_subtree_dirty(tree.root().unwrap(), DirtyFlags::LAYOUT);

    let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
    context.run(VIEWPORT);

    let damage = context.damage().expect("the box shrank");
    assert!(
        damage.height() >= 100.0,
        "the damage must cover where the box used to be, not only where it is: {damage:?}"
    );
}

// ---- what marks what --------------------------------------------------------------------------

#[test]
fn setting_the_same_text_is_not_a_change() {
    let (mut tree, texts) = document(3);
    tree.clear_all_dirty();
    let existing = tree.node(texts[0]).kind.text().unwrap().to_owned();
    tree.set_text(texts[0], existing);
    assert!(
        !tree.dirty(texts[0]).contains(DirtyFlags::LAYOUT),
        "an idempotent write should not cost a relayout"
    );
}

#[test]
fn editing_text_marks_the_node_and_its_ancestors_and_nothing_else() {
    let (mut tree, texts) = document(5);
    tree.clear_all_dirty();
    tree.set_text(texts[2], "changed");

    assert!(tree.dirty(texts[2]).contains(DirtyFlags::LAYOUT));
    assert!(tree.subtree_needs(tree.root().unwrap(), DirtyFlags::LAYOUT));
    assert!(
        !tree.dirty(texts[3]).contains(DirtyFlags::LAYOUT),
        "a sibling paragraph has not changed"
    );
}

/// `:empty` matches on whether an element has content, so text becoming empty changes whether
/// its *parent* matches — a style change, not a layout one.
#[test]
fn text_becoming_empty_invalidates_the_parents_style() {
    let (mut tree, texts) = document(3);
    tree.clear_all_dirty();
    let parent = tree.parent(texts[0]).unwrap();
    tree.set_text(texts[0], "");
    assert!(tree.dirty(parent).contains(DirtyFlags::STYLE));
}

#[test]
fn an_ordinary_text_edit_invalidates_no_style_at_all() {
    let (mut tree, texts) = document(3);
    tree.clear_all_dirty();
    tree.set_text(texts[0], "different but still not empty");
    let parent = tree.parent(texts[0]).unwrap();
    assert!(!tree.dirty(parent).contains(DirtyFlags::STYLE));
    assert!(!tree.dirty(texts[0]).contains(DirtyFlags::STYLE));
}

#[test]
fn adding_a_child_invalidates_its_siblings_style_but_not_their_subtrees() {
    let (mut tree, texts) = document(3);
    let body = tree.parent(tree.parent(texts[0]).unwrap()).unwrap();
    let existing: Vec<_> = tree.children(body).collect();
    tree.clear_all_dirty();

    let inserted = tree.create_element("p");
    tree.append_child(body, inserted).unwrap();

    for sibling in &existing {
        assert!(
            tree.dirty(*sibling).contains(DirtyFlags::STYLE),
            "a sibling's :nth-child may have changed"
        );
    }
    assert!(
        !tree.dirty(texts[0]).contains(DirtyFlags::STYLE),
        "but a grandchild's position among its own siblings did not"
    );
}

#[test]
fn a_class_change_invalidates_descendants_and_following_siblings_only() {
    let (mut tree, texts) = document(4);
    let paragraphs: Vec<_> = texts.iter().map(|t| tree.parent(*t).unwrap()).collect();
    tree.clear_all_dirty();

    let middle = paragraphs[1];
    tree.element_mut(middle).unwrap().set_class("open");
    tree.mark_selector_state_changed(middle);

    assert!(tree.dirty(middle).contains(DirtyFlags::STYLE), "itself");
    assert!(
        tree.dirty(texts[1]).contains(DirtyFlags::STYLE),
        "its descendants, through `.open .panel`"
    );
    assert!(
        tree.dirty(paragraphs[2]).contains(DirtyFlags::STYLE),
        "its following siblings, through `.open + .panel`"
    );
    assert!(
        !tree.dirty(paragraphs[0]).contains(DirtyFlags::STYLE),
        "but not the ones before it: no combinator in the dialect looks backwards"
    );
}

// ---- incremental restyle ------------------------------------------------------------------------

#[test]
fn a_clean_document_restyles_nothing() {
    let (mut tree, _) = document(50);
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).unwrap());

    let (previous, first) = engine.restyle(&tree);
    assert!(first.elements > 50);
    tree.clear_all_dirty();

    let (_, second) = engine.restyle_incremental(&mut tree, &previous);
    assert_eq!(second.elements, 0, "nothing changed, so nothing recomputed");
    assert!(second.reused > 50, "and everything reused");
}

#[test]
fn a_text_edit_restyles_nothing_at_all() {
    // Text content is invisible to every selector in the dialect except `:empty`, so an
    // ordinary edit costs zero style work.
    let (mut tree, texts) = document(50);
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).unwrap());
    let (previous, _) = engine.restyle(&tree);
    tree.clear_all_dirty();

    tree.set_text(texts[25], "edited");

    let (_, stats) = engine.restyle_incremental(&mut tree, &previous);
    assert_eq!(stats.elements, 0, "a text edit is not a style change");
}

#[test]
fn every_element_keeps_a_style_across_an_incremental_pass() {
    let (mut tree, _) = document(5);
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).unwrap());
    let (previous, _) = engine.restyle(&tree);
    tree.clear_all_dirty();
    tree.mark_dirty(tree.root().unwrap(), DirtyFlags::STYLE);

    let (after, stats) = engine.restyle_incremental(&mut tree, &previous);
    assert!(stats.elements > 0, "the root was recomputed");

    let mut stack = vec![tree.root().unwrap()];
    while let Some(id) = stack.pop() {
        if tree.element(id).is_some() {
            assert!(after.get(id).is_some(), "every element keeps a style");
        }
        for child in tree.children(id) {
            stack.push(child);
        }
    }
}

#[test]
fn restyling_clears_the_flags_it_consumed_and_leaves_layouts_alone() {
    let (mut tree, texts) = document(3);
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).unwrap());
    let (previous, _) = engine.restyle(&tree);
    tree.clear_all_dirty();

    tree.set_text(texts[0], "");
    let parent = tree.parent(texts[0]).unwrap();
    assert!(tree.dirty(parent).contains(DirtyFlags::STYLE));

    engine.restyle_incremental(&mut tree, &previous);
    assert!(
        !tree.dirty(parent).contains(DirtyFlags::STYLE),
        "the style pass consumed it"
    );
    assert!(
        tree.dirty(texts[0]).contains(DirtyFlags::LAYOUT),
        "and left layout's alone for layout to consume"
    );
}

#[test]
fn a_newly_added_node_is_styled_rather_than_skipped() {
    let (mut tree, _) = document(3);
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).unwrap());
    let (previous, _) = engine.restyle(&tree);
    tree.clear_all_dirty();

    let added = tree.create_element("div");
    tree.append_child(tree.root().unwrap(), added).unwrap();

    let (after, _) = engine.restyle_incremental(&mut tree, &previous);
    assert!(
        after.get(added).is_some(),
        "a node with no previous style has to be computed, not reused"
    );
}

/// Prints the numbers M6's acceptance is about, so a reader can see the shape of the win
/// rather than only that an assertion held.
#[test]
fn the_incremental_win_in_numbers() {
    let paragraphs = 5_000;
    let (mut tree, texts) = document(paragraphs);
    let map = styles(&tree);
    let mut fonts = FontSystem::empty();
    let mut cache = LayoutCache::new();

    let first = {
        let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
        context.run(VIEWPORT);
        context.stats()
    };

    tree.set_text(texts[paragraphs / 2], "edited");

    let (second, damage) = {
        let mut context = LayoutContext::new(&mut tree, &map, &mut fonts, &mut cache);
        context.run(VIEWPORT);
        (context.stats(), context.damage())
    };

    eprintln!(
        "\n{} nodes; one text edit\n  first pass:  {} laid out\n  second pass: {} caches \
         invalidated, {} laid out, {} boxes unchanged\n  damage: {:?}\n",
        tree.len(),
        first.nodes_laid_out,
        second.caches_invalidated,
        second.nodes_laid_out,
        second.unchanged,
        damage,
    );
    assert!(second.nodes_laid_out < 20);
}
