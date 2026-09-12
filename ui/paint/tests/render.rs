//! M2 acceptance, end to end: a hand-built nested tree reaches the framebuffer.
//!
//! `tests/paint.rs` proves the display list is right. This proves the pixels are, which is
//! what the milestone actually asks for — and it is the first test in the project that
//! exercises tree, paint, display list and renderer together.

use crisol_display_list::{Color, Size};
use crisol_paint::{PaintOptions, paint};
use crisol_render_wgpu::testing::with_gpu;
use crisol_render_wgpu::{FrameTarget, Gpu, HEADLESS_FORMAT, HeadlessTarget, Pixels, Renderer};
use crisol_tree::{BoxStyle, ColorBox, NodeId, Tree};

const TOLERANCE: u8 = 2;
const RED: Color = Color::rgb(1.0, 0.0, 0.0);
const GREEN: Color = Color::rgb(0.0, 1.0, 0.0);
const BLUE: Color = Color::rgb(0.0, 0.0, 1.0);

const VIEWPORT: Size = Size {
    width: 200.0,
    height: 200.0,
};

fn draw(gpu: &Gpu, tree: &Tree, scale: f32) -> Pixels {
    let width = (VIEWPORT.width * scale) as u32;
    let height = (VIEWPORT.height * scale) as u32;

    let list = paint(
        tree,
        &PaintOptions::new(VIEWPORT).with_background(Color::WHITE),
    );
    let mut renderer = Renderer::new(gpu, HEADLESS_FORMAT);
    let target = HeadlessTarget::new(gpu, width, height);
    renderer.render(
        FrameTarget {
            view: target.view(),
            width,
            height,
            scale_factor: scale,
        },
        &list,
    );
    target.read_pixels(gpu)
}

/// `root(0,0,200,200 red) > mid(20,20,120,120 green) > leaf(10,10,40,40 blue)`
fn nested() -> (Tree, [NodeId; 3]) {
    let mut tree = Tree::new();

    let root = tree.create_element("div");
    tree.set_root(root).unwrap();
    tree.node_mut(root).layout = crisol_display_list::Rect::from_xywh(0.0, 0.0, 200.0, 200.0);
    tree.node_mut(root).style = BoxStyle::filled(RED);

    let mid = tree.create_element("div");
    tree.append_child(root, mid).unwrap();
    tree.node_mut(mid).layout = crisol_display_list::Rect::from_xywh(20.0, 20.0, 120.0, 120.0);
    tree.node_mut(mid).style = BoxStyle::filled(GREEN);

    let leaf = tree.create_element("div");
    tree.append_child(mid, leaf).unwrap();
    tree.node_mut(leaf).layout = crisol_display_list::Rect::from_xywh(10.0, 10.0, 40.0, 40.0);
    tree.node_mut(leaf).style = BoxStyle::filled(BLUE);

    (tree, [root, mid, leaf])
}

#[test]
fn a_three_level_nested_tree_renders_at_the_right_positions() {
    with_gpu(|gpu| {
        let (tree, _) = nested();
        let pixels = draw(&gpu, &tree, 1.0);

        // Root, outside the middle box.
        pixels.assert_pixel(5, 5, RED, TOLERANCE);
        pixels.assert_pixel(180, 180, RED, TOLERANCE);
        // Middle box, outside the leaf.
        pixels.assert_pixel(25, 100, GREEN, TOLERANCE);
        pixels.assert_pixel(130, 25, GREEN, TOLERANCE);
        // Leaf: absolute (30,30)-(70,70).
        pixels.assert_pixel(32, 32, BLUE, TOLERANCE);
        pixels.assert_pixel(68, 68, BLUE, TOLERANCE);
        // Just outside the leaf on each side is the middle box, not the root.
        pixels.assert_pixel(28, 50, GREEN, TOLERANCE);
        pixels.assert_pixel(72, 50, GREEN, TOLERANCE);
    });
}

#[test]
fn removing_a_node_and_re_rendering_produces_the_expected_output() {
    with_gpu(|gpu| {
        let (mut tree, [_, mid, _]) = nested();

        let before = draw(&gpu, &tree, 1.0);
        before.assert_pixel(32, 32, BLUE, TOLERANCE);
        before.assert_pixel(25, 100, GREEN, TOLERANCE);

        tree.remove_subtree(mid);

        let after = draw(&gpu, &tree, 1.0);
        // Both the removed node and its child are gone, and the root shows through.
        after.assert_pixel(32, 32, RED, TOLERANCE);
        after.assert_pixel(25, 100, RED, TOLERANCE);
        after.assert_pixel(5, 5, RED, TOLERANCE);
    });
}

#[test]
fn a_tree_renders_identically_at_2x() {
    with_gpu(|gpu| {
        let (tree, _) = nested();
        let at_1x = draw(&gpu, &tree, 1.0);
        let at_2x = draw(&gpu, &tree, 2.0);

        assert_eq!((at_2x.width(), at_2x.height()), (400, 400));
        for (x, y) in [(5, 5), (25, 100), (32, 32), (68, 68), (130, 25)] {
            let expected = at_1x.at(x, y);
            let actual = at_2x.at(x * 2 + 1, y * 2 + 1);
            assert!(
                expected
                    .iter()
                    .zip(actual.iter())
                    .all(|(a, b)| a.abs_diff(*b) <= TOLERANCE),
                "1x ({x}, {y}) is {expected:?} but 2x is {actual:?}"
            );
        }
    });
}

#[test]
fn a_clipping_parent_cuts_its_child_off_in_the_framebuffer() {
    with_gpu(|gpu| {
        let (mut tree, [_, mid, leaf]) = nested();
        tree.node_mut(mid).style.clips_children = true;
        // Hang the leaf off the bottom-right of its 120x120 parent.
        tree.node_mut(leaf).layout = crisol_display_list::Rect::from_xywh(100.0, 100.0, 60.0, 60.0);

        let pixels = draw(&gpu, &tree, 1.0);
        // Inside the parent: the leaf shows.
        pixels.assert_pixel(125, 125, BLUE, TOLERANCE);
        // Past the parent's edge at 140: clipped away, root shows through.
        pixels.assert_pixel(150, 150, RED, TOLERANCE);
        pixels.assert_pixel(145, 125, RED, TOLERANCE);
    });
}

#[test]
fn a_custom_node_reaches_the_framebuffer() {
    with_gpu(|gpu| {
        let mut tree = Tree::new();
        let root = tree.create_element("div");
        tree.set_root(root).unwrap();
        tree.node_mut(root).layout = crisol_display_list::Rect::from_xywh(0.0, 0.0, 200.0, 200.0);
        tree.node_mut(root).style = BoxStyle::filled(RED);

        let custom = tree.create_custom("canvas", ColorBox::new(Size::new(40.0, 40.0), GREEN));
        tree.append_child(root, custom).unwrap();
        tree.node_mut(custom).layout = crisol_display_list::Rect::from_xywh(80.0, 80.0, 40.0, 40.0);

        let pixels = draw(&gpu, &tree, 1.0);
        pixels.assert_pixel(100, 100, GREEN, TOLERANCE);
        pixels.assert_pixel(78, 100, RED, TOLERANCE);
        pixels.assert_pixel(122, 100, RED, TOLERANCE);
    });
}
