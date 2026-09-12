//! Shared fixture for the layout snapshot suite.
//!
//! A case is a tiny document plus a stylesheet, laid out into a viewport, described as an
//! indented tree of boxes. Comparing the description rather than poking at individual
//! numbers means a case that breaks shows *what* moved, which is the difference between a
//! failing assertion and a useful one.

#![allow(dead_code, reason = "each test binary uses a subset of the harness")]

use std::fmt::Write as _;

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::Size;
use crisol_layout::layout;
use crisol_style::StyleEngine;
use crisol_text::FontSystem;
use crisol_tree::{NodeId, Tree};

/// A document built from a very small nested syntax, so a case is one readable literal.
///
/// `div.card > div.title` builds two nested elements; siblings are separated by `|`.
/// Deliberately not HTML: html5ever arrives at M4, and a parser dependency in the layout
/// tests would make a layout failure look like a parsing one.
pub struct Doc {
    /// The document.
    pub tree: Tree,
    /// The `<body>` element every case hangs off.
    pub root: NodeId,
}

impl Default for Doc {
    fn default() -> Self {
        Self::new()
    }
}

impl Doc {
    /// Builds `<body>` with no children.
    pub fn new() -> Self {
        let mut tree = Tree::new();
        let root = tree.create_element("body");
        tree.set_root(root).unwrap();
        Self { tree, root }
    }

    /// Appends an element to `parent`, returning it.
    ///
    /// `spec` is a tag optionally followed by `.class` and `#id` chunks in any order:
    /// `div`, `p.lede`, `section.card.wide#main`. An empty tag means `div`.
    pub fn add(&mut self, parent: NodeId, spec: &str) -> NodeId {
        let split = spec.find(['.', '#']).unwrap_or(spec.len());
        let (tag, rest) = spec.split_at(split);
        let id = self
            .tree
            .create_element(if tag.is_empty() { "div" } else { tag });
        self.tree.append_child(parent, id).unwrap();

        let data = self.tree.element_mut(id).unwrap();
        let mut classes = Vec::new();
        let mut chunk = rest;
        while let Some(marker) = chunk.chars().next() {
            let body = &chunk[marker.len_utf8()..];
            let next = body.find(['.', '#']).unwrap_or(body.len());
            let (name, remainder) = body.split_at(next);
            if marker == '#' {
                data.id = Some(name.into());
            } else {
                classes.push(name);
            }
            chunk = remainder;
        }
        if !classes.is_empty() {
            data.set_class(&classes.join(" "));
        }
        id
    }

    /// Appends a text node, which lays out as an empty leaf until M4.
    pub fn text(&mut self, parent: NodeId, content: &str) -> NodeId {
        let id = self.tree.create_text(content);
        self.tree.append_child(parent, id).unwrap();
        id
    }

    /// Styles and lays the document out, returning the box tree as text.
    ///
    /// Uses an empty font system: these cases are about boxes, and a machine's installed
    /// fonts must not change where a box lands. The cases that *are* about text say so by
    /// calling [`Self::snapshot_with_fonts`].
    pub fn snapshot(&mut self, css: &str, viewport: Size) -> String {
        self.snapshot_with(css, viewport, &mut FontSystem::empty())
    }

    /// As [`Self::snapshot`], with the system's fonts loaded.
    pub fn snapshot_with_fonts(&mut self, css: &str, viewport: Size) -> String {
        self.snapshot_with(css, viewport, &mut FontSystem::new())
    }

    fn snapshot_with(&mut self, css: &str, viewport: Size, fonts: &mut FontSystem) -> String {
        let mut engine = StyleEngine::new();
        engine.add_stylesheet(Stylesheet::parse(css).expect("the case's CSS must parse"));
        let (styles, _) = engine.restyle(&self.tree);
        layout(&mut self.tree, &styles, fonts, viewport);
        self.describe()
    }

    /// The box tree, one node per line, indented by depth.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        self.describe_node(self.root, 0, &mut out);
        out
    }

    fn describe_node(&self, id: NodeId, depth: usize, out: &mut String) {
        let node = self.tree.node(id);
        let rect = node.layout;
        let label = match &node.kind {
            crisol_tree::NodeKind::Element(data) => {
                let mut label = data.tag.to_string();
                for class in &data.classes {
                    let _ = write!(label, ".{class}");
                }
                label
            }
            crisol_tree::NodeKind::Text(_) => "#text".to_owned(),
            crisol_tree::NodeKind::Custom(custom) => {
                format!("{}<{}>", custom.data.tag, custom.node.debug_name())
            }
        };
        let _ = writeln!(
            out,
            "{:indent$}{label} {} {} {} {}",
            "",
            round(rect.origin.x),
            round(rect.origin.y),
            round(rect.size.width),
            round(rect.size.height),
            indent = depth * 2,
        );
        for child in self.tree.children(id) {
            self.describe_node(child, depth + 1, out);
        }
    }
}

/// Trims the trailing `.0` off whole numbers so a snapshot reads as `100` not `100.0`.
fn round(value: f32) -> String {
    if (value - value.round()).abs() < 1e-4 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.2}")
    }
}

/// Asserts a snapshot, normalising the indentation of the expected literal.
#[track_caller]
pub fn assert_layout(actual: &str, expected: &str) {
    let expected: String = expected
        .trim_matches('\n')
        .lines()
        .map(|line| format!("{}\n", line.strip_prefix("        ").unwrap_or(line)))
        .collect();
    assert_eq!(
        actual.trim_end(),
        expected.trim_end(),
        "\n--- actual ---\n{actual}\n--- expected ---\n{expected}"
    );
}

/// The viewport every case uses unless it says otherwise.
pub const VIEWPORT: Size = Size {
    width: 200.0,
    height: 100.0,
};
