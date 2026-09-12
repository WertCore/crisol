//! M2 acceptance: a hand-built nested tree paints to the right commands.
//!
//! These assert on the display list rather than on pixels, so they run without a GPU. The
//! pixel half of the milestone is `tests/render.rs`.

use crisol_display_list::{Color, Corners, DrawCommand, Edges, Edges4, Point, Rect, Size};
use crisol_paint::{PaintOptions, paint, paint_with_stats};
use crisol_tree::{BoxStyle, ColorBox, NodeId, Tree};

const RED: Color = Color::rgb(1.0, 0.0, 0.0);
const GREEN: Color = Color::rgb(0.0, 1.0, 0.0);
const BLUE: Color = Color::rgb(0.0, 0.0, 1.0);

fn options() -> PaintOptions {
    PaintOptions::new(Size::new(200.0, 200.0))
}

/// `root(0,0,200,200 red) > mid(20,20,120,120 green) > leaf(10,10,40,40 blue)`
///
/// The three levels the milestone asks for, with each level offset from its parent so that
/// a bug in offset accumulation shows up as a wrong absolute position rather than as
/// nothing at all.
fn nested() -> (Tree, [NodeId; 3]) {
    let mut tree = Tree::new();

    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);
    tree.node_mut(root).style = BoxStyle::filled(RED);

    let mid = tree.create_element("div");
    tree.append_child(root, mid).unwrap();
    tree.node_mut(mid).layout = Rect::from_xywh(20.0, 20.0, 120.0, 120.0);
    tree.node_mut(mid).style = BoxStyle::filled(GREEN);

    let leaf = tree.create_element("div");
    tree.append_child(mid, leaf).unwrap();
    tree.node_mut(leaf).layout = Rect::from_xywh(10.0, 10.0, 40.0, 40.0);
    tree.node_mut(leaf).style = BoxStyle::filled(BLUE);

    (tree, [root, mid, leaf])
}

fn rects(list: &crisol_display_list::DisplayList) -> Vec<(Rect, Color)> {
    list.commands()
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Rect(rect) => Some((rect.rect, rect.fill)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_three_level_tree_paints_at_absolute_positions() {
    let (tree, _) = nested();
    let list = paint(&tree, &options());

    assert_eq!(
        rects(&list),
        vec![
            (Rect::from_xywh(0.0, 0.0, 200.0, 200.0), RED),
            (Rect::from_xywh(20.0, 20.0, 120.0, 120.0), GREEN),
            // 20 + 10 on both axes: the leaf's origin is relative to its parent.
            (Rect::from_xywh(30.0, 30.0, 40.0, 40.0), BLUE),
        ]
    );
}

/// The second half of M2's acceptance: remove a node, paint again, get the expected output.
#[test]
fn removing_a_node_removes_it_and_its_subtree_from_the_next_paint() {
    let (mut tree, [_, mid, _]) = nested();

    assert_eq!(rects(&paint(&tree, &options())).len(), 3);

    tree.remove_subtree(mid);

    assert_eq!(
        rects(&paint(&tree, &options())),
        vec![(Rect::from_xywh(0.0, 0.0, 200.0, 200.0), RED)],
        "removing the middle node must take the leaf with it"
    );
}

#[test]
fn detaching_a_node_removes_it_from_paint_but_keeps_it_alive() {
    let (mut tree, [root, mid, leaf]) = nested();
    tree.detach(mid);

    assert_eq!(rects(&paint(&tree, &options())).len(), 1);
    assert!(tree.is_alive(mid) && tree.is_alive(leaf));

    // And re-attaching brings the whole subtree back.
    tree.append_child(root, mid).unwrap();
    assert_eq!(rects(&paint(&tree, &options())).len(), 3);
}

#[test]
fn siblings_paint_in_document_order() {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 100.0, 100.0);

    for (i, color) in [RED, GREEN, BLUE].into_iter().enumerate() {
        let child = tree.create_element("div");
        tree.append_child(root, child).unwrap();
        tree.node_mut(child).layout = Rect::from_xywh(i as f32 * 10.0, 0.0, 10.0, 10.0);
        tree.node_mut(child).style = BoxStyle::filled(color);
    }

    let list = paint(&tree, &options());
    let colors: Vec<_> = rects(&list).into_iter().map(|(_, c)| c).collect();
    assert_eq!(colors, vec![RED, GREEN, BLUE]);
}

#[test]
fn a_transparent_node_emits_nothing_but_still_positions_its_children() {
    let (mut tree, [_, mid, _]) = nested();
    tree.node_mut(mid).style = BoxStyle::default();

    let painted = rects(&paint(&tree, &options()));
    assert_eq!(painted.len(), 2);
    // The leaf is still offset by the invisible parent's origin.
    assert_eq!(painted[1].0, Rect::from_xywh(30.0, 30.0, 40.0, 40.0));
}

#[test]
fn an_invisible_node_hides_its_own_box_only() {
    let (mut tree, [_, mid, _]) = nested();
    tree.node_mut(mid).style.visible = false;

    let painted = rects(&paint(&tree, &options()));
    assert_eq!(
        painted,
        vec![
            (Rect::from_xywh(0.0, 0.0, 200.0, 200.0), RED),
            (Rect::from_xywh(30.0, 30.0, 40.0, 40.0), BLUE),
        ],
        "visibility hides a node's box; a descendant that is still visible still paints"
    );
}

#[test]
fn overflow_hidden_opens_and_closes_a_clip_around_the_subtree() {
    let (mut tree, [_, mid, _]) = nested();
    tree.node_mut(mid).style.clips_children = true;

    let list = paint(&tree, &options());
    let kinds: Vec<_> = list
        .commands()
        .iter()
        .map(|command| match command {
            DrawCommand::Rect(_) => "rect",
            DrawCommand::Image(_) => "image",
            DrawCommand::PushClip(_) => "push",
            DrawCommand::PopClip => "pop",
            DrawCommand::Text(_) => "text",
        })
        .collect();
    assert_eq!(kinds, vec!["rect", "rect", "push", "rect", "pop"]);

    let DrawCommand::PushClip(clip) = list.commands()[2] else {
        panic!("expected a clip");
    };
    assert_eq!(clip.rect, Rect::from_xywh(20.0, 20.0, 120.0, 120.0));
}

#[test]
fn a_child_entirely_outside_its_clipping_parent_is_dropped() {
    let (mut tree, [_, mid, leaf]) = nested();
    tree.node_mut(mid).style.clips_children = true;
    // Push the leaf far past the parent's 120x120 box.
    tree.node_mut(leaf).layout = Rect::from_xywh(400.0, 400.0, 40.0, 40.0);

    let list = paint(&tree, &options());
    assert_eq!(
        rects(&list).len(),
        2,
        "the builder should cull a command that cannot intersect the clip"
    );
}

#[test]
fn borders_and_radii_survive_the_walk() {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(4.0, 6.0, 50.0, 50.0);
    tree.node_mut(root).style = BoxStyle {
        background: RED,
        border_color: Edges4::all(BLUE),
        border_width: Edges {
            top: 1.0,
            right: 2.0,
            bottom: 3.0,
            left: 4.0,
        },
        radii: Corners::all(6.0),
        ..BoxStyle::default()
    };

    let list = paint(&tree, &options());
    let DrawCommand::Rect(command) = list.commands()[0] else {
        panic!("expected a rect");
    };
    assert_eq!(command.rect, Rect::from_xywh(4.0, 6.0, 50.0, 50.0));
    assert_eq!(command.border_width.left, 4.0);
    assert_eq!(command.radii, Corners::all(6.0));
    assert_eq!(command.border_color, Edges4::all(BLUE));
}

// ---- custom nodes --------------------------------------------------------------------

#[test]
fn a_custom_node_paints_itself_inside_an_engine_owned_clip() {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);

    let custom = tree.create_custom("canvas", ColorBox::new(Size::new(40.0, 40.0), GREEN));
    tree.append_child(root, custom).unwrap();
    tree.node_mut(custom).layout = Rect::from_xywh(25.0, 35.0, 40.0, 40.0);

    let (list, stats) = paint_with_stats(&tree, &options());
    assert_eq!(stats.custom_nodes, 1);

    let kinds: Vec<_> = list
        .commands()
        .iter()
        .map(|command| match command {
            DrawCommand::Rect(r) => format!("rect {:?}", r.rect),
            DrawCommand::PushClip(clip) => format!("push {:?}", clip.rect),
            DrawCommand::PopClip => "pop".to_owned(),
            DrawCommand::Image(_) => "image".to_owned(),
            DrawCommand::Text(_) => "text".to_owned(),
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            format!("push {:?}", Rect::from_xywh(25.0, 35.0, 40.0, 40.0)),
            format!("rect {:?}", Rect::from_xywh(25.0, 35.0, 40.0, 40.0)),
            "pop".to_owned(),
        ],
        "a custom node is given its absolute bounds, inside a clip the engine owns"
    );
}

/// DECISIONS D-19: a custom node that draws outside its box must be clipped rather than
/// trusted. This is what makes a PDF page safe to embed.
#[test]
fn a_custom_node_cannot_paint_outside_its_bounds() {
    #[derive(Debug)]
    struct Overreaching;

    impl crisol_tree::CustomNode for Overreaching {
        fn measure(&mut self, _: crisol_tree::MeasureConstraints) -> Size {
            Size::new(10.0, 10.0)
        }

        fn paint(&self, bounds: Rect, builder: &mut crisol_display_list::DisplayListBuilder) {
            builder.fill_rect(bounds.inflate(1000.0), RED);
        }
    }

    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);

    let custom = tree.create_custom("canvas", Overreaching);
    tree.append_child(root, custom).unwrap();
    tree.node_mut(custom).layout = Rect::from_xywh(50.0, 50.0, 10.0, 10.0);

    let list = paint(&tree, &options());
    let DrawCommand::PushClip(clip) = list.commands()[0] else {
        panic!("expected the engine's clip first");
    };
    assert_eq!(clip.rect, Rect::from_xywh(50.0, 50.0, 10.0, 10.0));

    // The oversized fill is still in the list, but the clip in force bounds it. The
    // renderer's scissor is what makes that true on the GPU; the list records the intent.
    let DrawCommand::Rect(command) = list.commands()[1] else {
        panic!("expected the custom node's rect");
    };
    assert!(command.rect.width() > 1000.0);
}

#[test]
fn a_custom_node_can_have_a_background_of_its_own() {
    let mut tree = Tree::new();
    let root = tree.create_custom("canvas", ColorBox::new(Size::new(10.0, 10.0), GREEN));
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
    tree.node_mut(root).style = BoxStyle::filled(RED);

    let list = paint(&tree, &options());
    let painted = rects(&list);
    assert_eq!(
        painted,
        vec![
            (Rect::from_xywh(0.0, 0.0, 10.0, 10.0), RED),
            (Rect::from_xywh(0.0, 0.0, 10.0, 10.0), GREEN),
        ],
        "the node's own background paints first, then the custom content over it"
    );
}

// ---- shape of the walk ---------------------------------------------------------------

#[test]
fn an_empty_tree_paints_a_clear_and_nothing_else() {
    let tree = Tree::new();
    let list = paint(&tree, &options().with_background(BLUE));
    assert!(list.is_empty());
    assert_eq!(list.background, BLUE);
    assert_eq!(list.viewport, Size::new(200.0, 200.0));
}

#[test]
fn a_text_node_emits_a_text_command_at_its_box() {
    let mut tree = Tree::new();
    let root = tree.create_element("p");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(4.0, 6.0, 100.0, 20.0);
    let text = tree.create_text("hello");
    tree.append_child(root, text).unwrap();
    tree.node_mut(text).layout = Rect::from_xywh(2.0, 1.0, 40.0, 16.0);
    tree.node_mut(text).style.text_color = RED;

    let (list, stats) = paint_with_stats(&tree, &options());
    assert_eq!(stats.nodes_visited, 2);
    assert_eq!(stats.boxes_emitted, 0, "a text node has no box of its own");
    assert_eq!(stats.text_runs, 1);

    let DrawCommand::Text(command) = list.commands()[0] else {
        panic!("expected a text command, got {:?}", list.commands());
    };
    assert_eq!(
        command.origin,
        Point::new(6.0, 7.0),
        "positioned at its absolute box, like everything else"
    );
    assert_eq!(command.color, RED);
    assert_eq!(
        command.text,
        crisol_display_list::TextId(text.to_bits()),
        "referred to by the node's own handle, which is how the renderer finds the layout"
    );
}

#[test]
fn an_empty_text_node_emits_nothing() {
    let mut tree = Tree::new();
    let root = tree.create_element("p");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 100.0, 20.0);
    let text = tree.create_text("");
    tree.append_child(root, text).unwrap();

    let (list, stats) = paint_with_stats(&tree, &options());
    assert_eq!(stats.text_runs, 0);
    assert!(list.is_empty());
}

#[test]
fn invisible_text_is_not_drawn() {
    let mut tree = Tree::new();
    let root = tree.create_element("p");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 100.0, 20.0);
    let text = tree.create_text("hello");
    tree.append_child(root, text).unwrap();
    tree.node_mut(text).style.visible = false;

    assert_eq!(paint_with_stats(&tree, &options()).1.text_runs, 0);
}

#[test]
fn a_deep_tree_does_not_overflow_the_stack() {
    let depth = 50_000;
    let mut tree = Tree::with_capacity(depth + 1);
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);

    let mut current = root;
    for _ in 0..depth {
        let child = tree.create_element("div");
        tree.append_child(current, child).unwrap();
        tree.node_mut(child).layout = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        tree.node_mut(child).style = BoxStyle::filled(RED);
        current = child;
    }

    let (_, stats) = paint_with_stats(&tree, &options());
    assert_eq!(stats.nodes_visited, depth + 1);
    assert_eq!(stats.boxes_emitted, depth);
}

#[test]
fn paint_subtree_positions_a_fragment_relative_to_a_given_origin() {
    let (tree, [_, mid, _]) = nested();
    let mut builder = crisol_display_list::DisplayListBuilder::new(Size::new(200.0, 200.0));
    crisol_paint::paint_subtree(&tree, mid, Point::new(100.0, 0.0), &mut builder);

    assert_eq!(
        rects(&builder.build()),
        vec![
            (Rect::from_xywh(120.0, 20.0, 120.0, 120.0), GREEN),
            (Rect::from_xywh(130.0, 30.0, 40.0, 40.0), BLUE),
        ]
    );
}

#[test]
fn overflow_hidden_on_a_rounded_box_produces_a_rounded_clip() {
    // The defect this pins: a scissor rectangle alone leaves square corners, so a rounded
    // card's content showed through them.
    let (mut tree, [_, mid, _]) = nested();
    tree.node_mut(mid).style.clips_children = true;
    tree.node_mut(mid).style.radii = Corners::all(8.0);

    let list = paint(&tree, &options());
    let DrawCommand::PushClip(clip) = list.commands()[2] else {
        panic!("expected a clip");
    };
    assert_eq!(clip.rect, Rect::from_xywh(20.0, 20.0, 120.0, 120.0));
    assert_eq!(clip.radii, Corners::all(8.0));
    assert!(clip.is_rounded());
}

#[test]
fn a_rounded_clip_keeps_its_own_corners_when_intersected() {
    // The radii belong to the box they were declared on. Clipping a rounded card to a
    // smaller ancestor must not move its corners.
    let mut builder = crisol_display_list::DisplayListBuilder::new(Size::new(200.0, 200.0));
    builder.push_clip(Rect::from_xywh(0.0, 0.0, 50.0, 200.0));
    builder.push_rounded_clip(crisol_display_list::Clip::rounded(
        Rect::from_xywh(10.0, 10.0, 100.0, 100.0),
        Corners::all(12.0),
    ));
    let clip = builder.current_clip().unwrap();
    assert_eq!(
        clip.rect,
        Rect::from_xywh(10.0, 10.0, 40.0, 100.0),
        "bounds intersect"
    );
    assert_eq!(
        clip.radii_rect,
        Rect::from_xywh(10.0, 10.0, 100.0, 100.0),
        "but the corners still belong to the box they were written for"
    );
    builder.pop_clip();
    builder.pop_clip();
}

#[test]
fn a_custom_node_is_clipped_to_its_own_rounded_box() {
    let mut tree = Tree::new();
    let root = tree.create_custom("canvas", ColorBox::new(Size::new(10.0, 10.0), GREEN));
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 40.0, 40.0);
    tree.node_mut(root).style.radii = Corners::all(6.0);

    let list = paint(&tree, &options());
    let DrawCommand::PushClip(clip) = list.commands()[0] else {
        panic!("expected the engine's clip first");
    };
    assert_eq!(
        clip.radii,
        Corners::all(6.0),
        "a rounded PDF page must not paint into corners it does not own"
    );
}

// ---- damage culling ---------------------------------------------------------------------

#[test]
fn a_damaged_paint_skips_subtrees_that_cannot_touch_it() {
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);

    // Twenty stacked boxes; only one of them is damaged.
    let mut boxes = Vec::new();
    for i in 0..20 {
        let child = tree.create_element("div");
        tree.append_child(root, child).unwrap();
        tree.node_mut(child).layout = Rect::from_xywh(0.0, i as f32 * 10.0, 200.0, 10.0);
        tree.node_mut(child).style = BoxStyle::filled(RED);
        boxes.push(child);
    }

    let (list, stats) = paint_with_stats(
        &tree,
        &options().with_damage(Rect::from_xywh(0.0, 50.0, 200.0, 10.0)),
    );

    assert!(
        stats.subtrees_culled >= 15,
        "most of the document should be skipped, culled {}",
        stats.subtrees_culled
    );
    // The clip, the background fill, and the one box that was damaged.
    let rect_count = list
        .commands()
        .iter()
        .filter(|c| matches!(c, DrawCommand::Rect(_)))
        .count();
    assert!(
        rect_count <= 3,
        "only the damaged box should be drawn: {rect_count}"
    );
}

#[test]
fn a_damaged_paint_clips_and_repaints_the_background_itself() {
    // The renderer *loads* rather than clears on a damaged frame, because a clear ignores
    // the scissor and would wipe the region being kept. So paint has to put the background
    // back inside the damage before anything is drawn over it.
    let (tree, _) = nested();
    let damage = Rect::from_xywh(10.0, 10.0, 20.0, 20.0);
    let list = paint(&tree, &options().with_background(BLUE).with_damage(damage));

    let DrawCommand::PushClip(clip) = list.commands()[0] else {
        panic!("a damaged paint should clip to the damage first");
    };
    assert_eq!(clip.rect, damage);

    let DrawCommand::Rect(background) = list.commands()[1] else {
        panic!("and then repaint the background inside it");
    };
    assert_eq!(background.rect, damage);
    assert_eq!(background.fill, BLUE);
}

#[test]
fn a_child_overflowing_its_parent_is_not_culled_with_it() {
    // `overflow: visible` is the initial value, so a child outside its parent's box is the
    // common case rather than an exotic one. Culling on the parent's box alone loses it.
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);

    let parent = tree.create_element("div");
    tree.append_child(root, parent).unwrap();
    tree.node_mut(parent).layout = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);

    let overflowing = tree.create_element("div");
    tree.append_child(parent, overflowing).unwrap();
    // Sticks out well past the parent, into the damaged region.
    tree.node_mut(overflowing).layout = Rect::from_xywh(0.0, 100.0, 50.0, 50.0);
    tree.node_mut(overflowing).style = BoxStyle::filled(RED);

    let (list, _) = paint_with_stats(
        &tree,
        &options().with_damage(Rect::from_xywh(0.0, 120.0, 50.0, 10.0)),
    );
    assert!(
        rects(&list).iter().any(|(_, color)| *color == RED),
        "the overflowing child touches the damage and must be painted"
    );
}

#[test]
fn a_clipping_parent_that_misses_the_damage_takes_its_children_with_it() {
    // The other side of the same coin: a node with `overflow: hidden` confines its
    // descendants, so if its box misses there is nothing beneath it to find.
    let mut tree = Tree::new();
    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 200.0, 200.0);

    let parent = tree.create_element("div");
    tree.append_child(root, parent).unwrap();
    tree.node_mut(parent).layout = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
    tree.node_mut(parent).style.clips_children = true;

    let child = tree.create_element("div");
    tree.append_child(parent, child).unwrap();
    tree.node_mut(child).layout = Rect::from_xywh(0.0, 100.0, 50.0, 50.0);
    tree.node_mut(child).style = BoxStyle::filled(RED);

    let (list, stats) = paint_with_stats(
        &tree,
        &options().with_damage(Rect::from_xywh(0.0, 120.0, 50.0, 10.0)),
    );
    assert!(stats.subtrees_culled > 0);
    assert!(
        !rects(&list).iter().any(|(_, color)| *color == RED),
        "the child is clipped away by its parent, so it cannot reach the damage"
    );
}

#[test]
fn painting_without_damage_is_unchanged() {
    let (tree, _) = nested();
    let (list, stats) = paint_with_stats(&tree, &options());
    assert_eq!(stats.subtrees_culled, 0);
    assert_eq!(rects(&list).len(), 3, "the whole document, as before");
}
