//! Turning a point into a node.

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::{Color, Point, Size};
use crisol_events::{hit_test, hit_test_with_text, path_to};
use crisol_layout::{TextMap, layout};
use crisol_style::StyleEngine;
use crisol_text::FontSystem;
use crisol_tree::{ColorBox, CustomHit, CustomNode, MeasureConstraints, NodeId, Tree};

const VIEWPORT: Size = Size {
    width: 200.0,
    height: 100.0,
};

/// Parses, styles and lays out, so the tests read as documents rather than as box arithmetic.
struct Page {
    tree: Tree,
    text: TextMap,
}

impl Page {
    fn new(html: &str, css: &str) -> Self {
        Self::with_fonts(html, css, FontSystem::empty())
    }

    fn with_fonts(html: &str, css: &str, mut fonts: FontSystem) -> Self {
        let mut document = crisol_html::parse(html);
        let mut engine = StyleEngine::new();
        engine.add_stylesheet(Stylesheet::parse(css).expect("the case's CSS must parse"));
        let (styles, _) = engine.restyle(&document.tree);
        let (text, _) = layout(&mut document.tree, &styles, &mut fonts, VIEWPORT);
        Self {
            tree: document.tree,
            text,
        }
    }

    /// The tag and first class of whatever is at `(x, y)`.
    fn at(&self, x: f32, y: f32) -> Option<String> {
        let hit = hit_test(&self.tree, Point::new(x, y))?;
        Some(describe(&self.tree, hit.node))
    }

    fn find(&self, tag: &str) -> NodeId {
        let mut stack = vec![self.tree.root().unwrap()];
        while let Some(id) = stack.pop() {
            if self.tree.node(id).kind.tag() == Some(tag) {
                return id;
            }
            for child in self.tree.children(id) {
                stack.push(child);
            }
        }
        panic!("no <{tag}>");
    }
}

fn describe(tree: &Tree, id: NodeId) -> String {
    let node = tree.node(id);
    match node.kind.element() {
        Some(data) => match data.classes.first() {
            Some(class) => format!("{}.{}", data.tag, class),
            None => data.tag.to_string(),
        },
        None => "#text".to_owned(),
    }
}

// ---- the basics ------------------------------------------------------------------------

#[test]
fn a_point_finds_the_box_under_it() {
    let page = Page::new(
        "<body><div class=card></div></body>",
        ".card { width: 50px; height: 40px; background-color: red }",
    );
    assert_eq!(page.at(10.0, 10.0).as_deref(), Some("div.card"));
}

#[test]
fn a_point_outside_every_box_still_finds_the_root() {
    // The root fills the viewport (D-25), so a click anywhere in the window lands on it.
    let page = Page::new("<body></body>", "");
    assert_eq!(page.at(150.0, 90.0).as_deref(), Some("html"));
}

#[test]
fn a_point_outside_the_viewport_finds_nothing() {
    let page = Page::new("<body></body>", "");
    assert_eq!(page.at(-5.0, 50.0), None);
    assert_eq!(page.at(50.0, 500.0), None);
}

#[test]
fn the_topmost_box_wins() {
    // Later siblings paint over earlier ones, so they are hit first.
    let page = Page::new(
        "<body><div class=under></div><div class=over></div></body>",
        "div { position: absolute; top: 0; left: 0; width: 50px; height: 50px }",
    );
    assert_eq!(page.at(25.0, 25.0).as_deref(), Some("div.over"));
}

#[test]
fn a_child_is_hit_before_its_parent() {
    let page = Page::new(
        "<body><div class=outer><div class=inner></div></div></body>",
        ".outer { width: 100px; height: 100px } .inner { width: 20px; height: 20px }",
    );
    assert_eq!(page.at(10.0, 10.0).as_deref(), Some("div.inner"));
    assert_eq!(
        page.at(50.0, 50.0).as_deref(),
        Some("div.outer"),
        "outside the child, the parent catches it"
    );
}

#[test]
fn a_transparent_box_still_catches_the_point() {
    // No background is not the same as not there. `pointer-events` is what says otherwise,
    // and it is not in M3's property subset.
    let page = Page::new(
        "<body><div class=ghost></div></body>",
        ".ghost { width: 50px; height: 50px }",
    );
    assert_eq!(page.at(10.0, 10.0).as_deref(), Some("div.ghost"));
}

// ---- what is not hit --------------------------------------------------------------------

#[test]
fn an_invisible_box_is_not_hit() {
    // Half the reason `visibility: hidden` exists is that the button stops being clickable.
    let page = Page::new(
        "<body><div class=gone></div></body>",
        ".gone { width: 50px; height: 50px; visibility: hidden }",
    );
    // Falls through to `<body>`, which the hidden div still gives a height to — hiding a box
    // does not remove it from layout, only from painting and from hit testing.
    assert_eq!(page.at(10.0, 10.0).as_deref(), Some("body"));
}

#[test]
fn a_display_none_box_is_not_hit() {
    let page = Page::new(
        "<body><div class=gone></div></body>",
        ".gone { display: none; width: 50px; height: 50px }",
    );
    // `display: none` removes the box entirely, so `<body>` has no height either and the
    // point falls all the way through to the root.
    assert_eq!(page.at(10.0, 10.0).as_deref(), Some("html"));
}

#[test]
fn an_invisible_parent_hides_its_children_from_hit_testing_too() {
    let page = Page::new(
        "<body><div class=outer><div class=inner></div></div></body>",
        ".outer { visibility: hidden; width: 80px; height: 80px }
         .inner { width: 40px; height: 40px }",
    );
    assert_eq!(
        page.at(10.0, 10.0).as_deref(),
        Some("body"),
        "neither the hidden parent nor its children are hit; `<body>` is what is left"
    );
}

// ---- clipping ---------------------------------------------------------------------------

#[test]
fn a_clipped_away_child_is_not_hit() {
    let page = Page::new(
        "<body><div class=box><div class=tall></div></div></body>",
        ".box { width: 60px; height: 30px; overflow: hidden }
         .tall { width: 60px; height: 200px }",
    );
    assert_eq!(
        page.at(30.0, 15.0).as_deref(),
        Some("div.tall"),
        "inside the clip the child is hit"
    );
    assert_eq!(
        page.at(30.0, 60.0).as_deref(),
        Some("html"),
        "past the clip nothing in that subtree is"
    );
}

#[test]
fn nested_clips_intersect() {
    let page = Page::new(
        "<body><div class=outer><div class=inner><div class=content></div></div></div></body>",
        ".outer { width: 100px; height: 40px; overflow: hidden }
         .inner { width: 40px; height: 100px; overflow: hidden }
         .content { width: 200px; height: 200px }",
    );
    // Only the 40x40 intersection can be hit.
    assert_eq!(page.at(20.0, 20.0).as_deref(), Some("div.content"));
    assert_eq!(
        page.at(60.0, 20.0).as_deref(),
        Some("div.outer"),
        "past the inner clip, but still inside the outer box, which catches it"
    );
    // The outer box is 40px tall, and `<body>` is only as tall as it, so 60px down is past
    // both and lands on the root.
    assert_eq!(
        page.at(20.0, 60.0).as_deref(),
        Some("html"),
        "past the outer clip entirely"
    );
}

// ---- local coordinates ------------------------------------------------------------------

#[test]
fn the_hit_carries_the_point_in_the_nodes_own_space() {
    let page = Page::new(
        "<body><div class=outer><div class=inner></div></div></body>",
        ".outer { width: 100px; height: 100px; padding: 10px }
         .inner { width: 40px; height: 40px }",
    );
    let hit = hit_test(&page.tree, Point::new(15.0, 18.0)).unwrap();
    assert_eq!(describe(&page.tree, hit.node), "div.inner");
    assert_eq!(
        hit.local,
        Point::new(5.0, 8.0),
        "relative to the inner box, which starts at (10, 10)"
    );
    assert_eq!(hit.bounds.origin, Point::new(10.0, 10.0));
}

// ---- custom nodes -------------------------------------------------------------------------

#[test]
fn a_custom_node_resolves_the_point_itself() {
    let mut page = Page::new("<body></body>", "");
    let body = page.find("body");
    let mut stub = ColorBox::new(Size::new(40.0, 40.0), Color::BLACK);
    stub.target = 7;
    let custom = page.tree.create_custom("canvas", stub);
    page.tree.append_child(body, custom).unwrap();
    page.tree.node_mut(custom).layout =
        crisol_display_list::Rect::from_xywh(10.0, 10.0, 40.0, 40.0);

    let hit = hit_test(&page.tree, Point::new(20.0, 25.0)).unwrap();
    assert_eq!(hit.node, custom);
    assert_eq!(
        hit.custom,
        Some(CustomHit {
            target: 7,
            local: Point::new(10.0, 15.0)
        }),
        "the node's own answer, in its own coordinates"
    );
}

/// DECISIONS D-19: returning `None` is how a custom node declares a hole.
#[test]
fn a_custom_node_can_declare_the_point_a_miss() {
    #[derive(Debug)]
    struct Hollow;

    impl CustomNode for Hollow {
        fn measure(&mut self, _: MeasureConstraints) -> Size {
            Size::new(40.0, 40.0)
        }

        fn paint(
            &self,
            _: crisol_display_list::Rect,
            _: &mut crisol_display_list::DisplayListBuilder,
        ) {
        }

        fn hit_test(&self, local: Point) -> Option<CustomHit> {
            // Opaque only on the left half.
            (local.x < 20.0).then_some(CustomHit { target: 1, local })
        }
    }

    let mut page = Page::new("<body></body>", "");
    let body = page.find("body");
    let custom = page.tree.create_custom("canvas", Hollow);
    page.tree.append_child(body, custom).unwrap();
    page.tree.node_mut(custom).layout = crisol_display_list::Rect::from_xywh(0.0, 0.0, 40.0, 40.0);

    assert_eq!(
        hit_test(&page.tree, Point::new(5.0, 5.0)).map(|hit| hit.node),
        Some(custom)
    );
    assert_eq!(
        hit_test(&page.tree, Point::new(30.0, 5.0)).map(|hit| describe(&page.tree, hit.node)),
        Some("html".to_owned()),
        "the hole falls through to what is behind it"
    );
}

// ---- text -------------------------------------------------------------------------------

#[test]
fn hitting_text_resolves_to_a_cursor() {
    let fonts = FontSystem::new();
    if fonts.is_empty() {
        eprintln!("skipping: no fonts installed");
        return;
    }
    let page = Page::with_fonts(
        "<body><p>hello world</p></body>",
        "p { font-size: 20px }",
        fonts,
    );

    struct Lookup<'a>(&'a TextMap);
    impl crisol_events::TextLookup for Lookup<'_> {
        fn text_for(&self, node: NodeId) -> Option<&crisol_text::TextLayout> {
            self.0.get(node)
        }
    }

    let hit = hit_test_with_text(&page.tree, Point::new(2.0, 8.0), &Lookup(&page.text)).unwrap();
    assert_eq!(describe(&page.tree, hit.node), "#text");
    assert_eq!(
        hit.cursor.map(|cursor| cursor.index),
        Some(0),
        "the left edge of the first glyph is cursor 0"
    );

    // Further right is further into the string.
    let later = hit_test_with_text(&page.tree, Point::new(40.0, 8.0), &Lookup(&page.text)).unwrap();
    assert!(later.cursor.unwrap().index > 0);
}

#[test]
fn hitting_text_without_a_lookup_still_finds_the_node() {
    let fonts = FontSystem::new();
    if fonts.is_empty() {
        return;
    }
    let page = Page::with_fonts("<body><p>hello</p></body>", "p { font-size: 20px }", fonts);
    let hit = hit_test(&page.tree, Point::new(2.0, 8.0)).unwrap();
    assert_eq!(describe(&page.tree, hit.node), "#text");
    assert_eq!(hit.cursor, None, "no lookup, no cursor — but still a hit");
}

// ---- the propagation path -----------------------------------------------------------------

#[test]
fn the_path_runs_from_the_root_down_to_the_target() {
    let page = Page::new(
        "<body><div class=outer><div class=inner></div></div></body>",
        ".outer { width: 50px; height: 50px } .inner { width: 20px; height: 20px }",
    );
    let hit = hit_test(&page.tree, Point::new(5.0, 5.0)).unwrap();
    let path: Vec<_> = path_to(&page.tree, hit.node)
        .into_iter()
        .map(|id| describe(&page.tree, id))
        .collect();
    assert_eq!(
        path,
        vec!["html", "body", "div.outer", "div.inner"],
        "capture runs this forwards and bubble runs it backwards"
    );
}

#[test]
fn the_path_to_the_root_is_just_the_root() {
    let page = Page::new("<body></body>", "");
    let root = page.tree.root().unwrap();
    assert_eq!(path_to(&page.tree, root), vec![root]);
}
