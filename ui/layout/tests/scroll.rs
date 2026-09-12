//! Scroll extent comes out of layout, and the offset survives it.

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::{Point, Size};
use crisol_layout::{LayoutCache, LayoutContext};
use crisol_style::{StyleEngine, StyleMap};
use crisol_text::FontSystem;
use crisol_tree::{NodeId, Tree};

const VIEWPORT: Size = Size {
    width: 200.0,
    height: 400.0,
};

const CSS: &str = "
    .scroller { display: block; overflow: scroll; width: 200px; height: 100px }
    .clipper  { display: block; overflow: clip;   width: 200px; height: 100px }
    .item     { display: block; height: 60px }
";

/// A scroll container holding `items` sixty-pixel rows.
fn document(class: &str, items: usize) -> (Tree, NodeId, Vec<NodeId>) {
    let mut tree = Tree::new();
    let root = tree.create_element("html");
    tree.set_root(root).unwrap();
    let scroller = tree.create_element("div");
    tree.element_mut(scroller).unwrap().set_class(class);
    tree.append_child(root, scroller).unwrap();

    let rows = (0..items)
        .map(|_| {
            let row = tree.create_element("div");
            tree.element_mut(row).unwrap().set_class("item");
            tree.append_child(scroller, row).unwrap();
            row
        })
        .collect();
    (tree, scroller, rows)
}

struct Frame {
    engine: StyleEngine,
    fonts: FontSystem,
    cache: LayoutCache,
    styles: StyleMap,
}

impl Frame {
    fn new() -> Self {
        let mut engine = StyleEngine::new();
        engine.add_stylesheet(Stylesheet::parse(CSS).unwrap());
        Self {
            engine,
            fonts: FontSystem::empty(),
            cache: LayoutCache::new(),
            styles: StyleMap::default(),
        }
    }

    fn run(&mut self, tree: &mut Tree) {
        self.styles = self.engine.restyle_incremental(tree, &self.styles).0;
        let mut context = LayoutContext::new(tree, &self.styles, &mut self.fonts, &mut self.cache);
        context.run(VIEWPORT);
    }
}

#[test]
fn overflow_scroll_makes_a_box_scrollable_by_its_excess_content() {
    // Five 60px rows in a 100px box: 300 of content, 200 of it out of reach.
    let (mut tree, scroller, _) = document("scroller", 5);
    Frame::new().run(&mut tree);

    assert!(tree.get(scroller).unwrap().style.scrolls);
    assert_eq!(tree.scroll_max(scroller).height, 200.0);
    assert!(tree.is_scrollable(scroller));
}

#[test]
fn content_that_fits_is_not_scrollable() {
    let (mut tree, scroller, _) = document("scroller", 1);
    Frame::new().run(&mut tree);

    assert!(
        tree.get(scroller).unwrap().style.scrolls,
        "it is a scroll container"
    );
    assert_eq!(tree.scroll_max(scroller).height, 0.0, "with nowhere to go");
    assert!(
        !tree.is_scrollable(scroller),
        "so a gesture over it should chain outward rather than being swallowed"
    );
}

#[test]
fn overflow_clip_is_never_scrollable_however_much_it_hides() {
    let (mut tree, clipper, _) = document("clipper", 5);
    Frame::new().run(&mut tree);

    assert!(tree.get(clipper).unwrap().style.clips_children);
    assert!(!tree.get(clipper).unwrap().style.scrolls);
    assert!(!tree.is_scrollable(clipper));
    assert_eq!(tree.scroll_by(clipper, Point::new(0.0, 50.0)), Point::ZERO);
}

#[test]
fn an_offset_survives_a_relayout_that_did_not_shrink_the_content() {
    let (mut tree, scroller, _) = document("scroller", 5);
    let mut frame = Frame::new();
    frame.run(&mut tree);

    tree.set_scroll(scroller, Point::new(0.0, 150.0));

    // Something elsewhere changed and the frame laid out again.
    let root = tree.root().unwrap();
    let extra = tree.create_element("div");
    tree.element_mut(extra).unwrap().set_class("item");
    tree.append_child(root, extra).unwrap();
    frame.run(&mut tree);

    assert_eq!(
        tree.scroll_offset(scroller).y,
        150.0,
        "a list that jumped to the top on every relayout would be unusable"
    );
}

#[test]
fn removing_content_pulls_the_offset_back_to_the_new_end() {
    let (mut tree, scroller, rows) = document("scroller", 5);
    let mut frame = Frame::new();
    frame.run(&mut tree);

    tree.set_scroll(scroller, Point::new(0.0, 200.0));
    assert_eq!(tree.scroll_offset(scroller).y, 200.0);

    // Down to two rows: 120 of content in a 100 box, so 20 is all that is left.
    for row in &rows[2..] {
        tree.remove_subtree(*row);
    }
    frame.run(&mut tree);

    assert_eq!(tree.scroll_max(scroller).height, 20.0);
    assert_eq!(
        tree.scroll_offset(scroller).y,
        20.0,
        "an offset left where it was would be looking past the end of the content"
    );
}
