//! HTML source to node tree.
//!
//! These do not re-test html5ever — the tree building algorithm is upstream's. They test the
//! translation: which nodes this engine keeps, what happens to the ones it drops, and that
//! the tree a parse produces is the same shape the cascade and layout already expect.

use crisol_html::{parse, parse_fragment};
use crisol_tree::{NodeId, NodeKind, Tree};

/// The tree as an indented outline: `tag`, `tag.class`, `tag#id`, or `"text"`.
fn outline(tree: &Tree) -> String {
    fn walk(tree: &Tree, id: NodeId, depth: usize, out: &mut String) {
        use std::fmt::Write as _;
        let node = tree.node(id);
        let label = match &node.kind {
            NodeKind::Element(data) | NodeKind::Custom(crisol_tree::CustomElement { data, .. }) => {
                let mut label = data.tag.to_string();
                if let Some(id) = &data.id {
                    let _ = write!(label, "#{id}");
                }
                for class in &data.classes {
                    let _ = write!(label, ".{class}");
                }
                label
            }
            NodeKind::Text(text) => format!("{text:?}"),
        };
        let _ = writeln!(out, "{:indent$}{label}", "", indent = depth * 2);
        for child in tree.children(id) {
            walk(tree, child, depth + 1, out);
        }
    }
    let mut out = String::new();
    if let Some(root) = tree.root() {
        walk(tree, root, 0, &mut out);
    }
    out
}

#[track_caller]
fn assert_outline(tree: &Tree, expected: &str) {
    let expected: String = expected
        .trim_matches('\n')
        .lines()
        .map(|line| format!("{}\n", line.strip_prefix("        ").unwrap_or(line)))
        .collect();
    assert_eq!(
        outline(tree).trim_end(),
        expected.trim_end(),
        "\n--- actual ---\n{}\n--- expected ---\n{expected}",
        outline(tree)
    );
}

// ---- the shape a parse produces ------------------------------------------------------

#[test]
fn the_tree_is_rooted_at_html_not_at_a_document_node() {
    // CSS's `:root` means `<html>`. A document node above it would make `:root` match
    // something with no box.
    let document = parse("<p>hi</p>");
    let root = document.tree.root().unwrap();
    assert_eq!(document.tree.node(root).kind.tag(), Some("html"));
    assert!(document.tree.parent(root).is_none());
}

#[test]
fn implied_html_head_and_body_are_inserted() {
    assert_outline(
        &parse("<p>hi</p>").tree,
        r#"
        html
          head
          body
            p
              "hi"
        "#,
    );
}

#[test]
fn an_empty_document_still_produces_a_usable_tree() {
    assert_outline(
        &parse("").tree,
        "
        html
          head
          body
        ",
    );
}

#[test]
fn tag_names_are_lowercased() {
    assert_outline(
        &parse("<DIV><SPAN>x</SPAN></DIV>").tree,
        r#"
        html
          head
          body
            div
              span
                "x"
        "#,
    );
}

// ---- attributes ----------------------------------------------------------------------

#[test]
fn id_and_class_land_in_their_own_fields() {
    let document = parse(r#"<p id="lede" class="a b">x</p>"#);
    let p = find(&document.tree, "p");
    let data = document.tree.element(p).unwrap();
    assert_eq!(
        data.id.as_ref().map(ToString::to_string),
        Some("lede".into())
    );
    assert_eq!(data.classes.len(), 2);
    assert!(data.has_class("a", true) && data.has_class("b", true));
}

#[test]
fn other_attributes_are_kept_in_document_order() {
    let document = parse(r#"<a href="/x" title="T" data-n="1">x</a>"#);
    let a = find(&document.tree, "a");
    let data = document.tree.element(a).unwrap();
    let names: Vec<_> = data
        .attributes
        .iter()
        .map(|attribute| attribute.name.to_string())
        .collect();
    assert_eq!(names, vec!["href", "title", "data-n"]);
    assert_eq!(data.attribute("href").as_deref(), Some("/x"));
}

#[test]
fn an_unquoted_attribute_parses() {
    let document = parse("<p class=lede>x</p>");
    assert!(
        document
            .tree
            .element(find(&document.tree, "p"))
            .unwrap()
            .has_class("lede", true)
    );
}

// ---- text ----------------------------------------------------------------------------

#[test]
fn a_run_of_text_split_by_an_entity_becomes_one_node() {
    // The tokenizer emits text in whatever chunks it produced; an entity reference splits a
    // run in two. One node per run is what the shaper wants at M4 and what the DOM says.
    let document = parse("<p>a&amp;b</p>");
    let p = find(&document.tree, "p");
    let children: Vec<_> = document.tree.children(p).collect();
    assert_eq!(children.len(), 1, "expected one merged text node");
    assert_eq!(document.tree.node(children[0]).kind.text(), Some("a&b"));
}

#[test]
fn entities_are_decoded() {
    let document = parse("<p>&lt;tag&gt; &#169; caf&eacute;</p>");
    let p = find(&document.tree, "p");
    let text = document.tree.first_child(p).unwrap();
    assert_eq!(document.tree.node(text).kind.text(), Some("<tag> © café"));
}

#[test]
fn whitespace_between_elements_is_preserved() {
    // Whitespace is text, and collapsing it is the text layer's decision at M4, not the
    // parser's. Throwing it away here would make that decision unavailable.
    let document = parse("<body><span>a</span> <span>b</span></body>");
    let body = find(&document.tree, "body");
    let kinds: Vec<_> = document
        .tree
        .children(body)
        .map(|id| match &document.tree.node(id).kind {
            NodeKind::Text(text) => format!("{text:?}"),
            other => other.tag().unwrap_or("?").to_owned(),
        })
        .collect();
    assert_eq!(kinds, vec!["span", "\" \"", "span"]);
}

// ---- what is dropped -------------------------------------------------------------------

#[test]
fn comments_do_not_reach_the_tree() {
    assert_outline(
        &parse("<p>a<!-- note -->b</p>").tree,
        r#"
        html
          head
          body
            p
              "ab"
        "#,
    );
}

#[test]
fn a_doctype_does_not_reach_the_tree_but_sets_quirks() {
    let standards = parse("<!DOCTYPE html><p>x</p>");
    assert!(!standards.quirks);
    assert_eq!(
        standards
            .tree
            .node(standards.tree.root().unwrap())
            .kind
            .tag(),
        Some("html")
    );

    let quirks = parse("<p>x</p>");
    assert!(quirks.quirks, "a missing doctype is quirks mode");
}

// ---- error recovery ---------------------------------------------------------------------

#[test]
fn unclosed_tags_are_recovered_and_reported() {
    let document = parse("<p>a<p>b");
    assert_outline(
        &document.tree,
        r#"
        html
          head
          body
            p
              "a"
            p
              "b"
        "#,
    );
}

#[test]
fn mis_nested_tags_are_repaired() {
    let document = parse("<b><i>x</b></i>");
    // html5ever's adoption agency algorithm reparents; the point is that it produces a tree
    // rather than failing, and that the sink survives the reparenting.
    assert!(!document.tree.is_empty());
    assert!(
        !document.errors.is_empty(),
        "the mis-nesting should be reported"
    );
}

#[test]
fn parse_errors_are_collected_rather_than_thrown_away() {
    let document = parse("<p><div></p>");
    assert!(
        !document.errors.is_empty(),
        "html5ever objects to this; the objection should be reachable"
    );
    assert!(!document.errors[0].message.is_empty());
}

#[test]
fn a_stray_end_tag_does_not_break_the_parse() {
    let document = parse("</div><p>x</p>");
    assert_eq!(
        document.tree.node(find(&document.tree, "p")).kind.tag(),
        Some("p")
    );
}

// ---- fragments ---------------------------------------------------------------------------

#[test]
fn a_fragment_is_rooted_at_its_context_element() {
    let document = parse_fragment("<span>a</span><span>b</span>");
    assert_outline(
        &document.tree,
        r#"
        div
          span
            "a"
          span
            "b"
        "#,
    );
}

#[test]
fn a_fragment_does_not_gain_an_implied_body() {
    let document = parse_fragment("<p>x</p>");
    let root = document.tree.root().unwrap();
    assert_eq!(document.tree.node(root).kind.tag(), Some("div"));
    assert!(
        document
            .tree
            .children(root)
            .all(|id| document.tree.node(id).kind.tag() != Some("body"))
    );
}

// ---- the tree the rest of the engine expects ---------------------------------------------

#[test]
fn a_parsed_tree_is_selectable_and_stylable() {
    // The point of the whole crate: what comes out is the same tree the cascade and layout
    // already work on, with no adapter in between.
    use crisol_css::stylesheet::Stylesheet;
    use crisol_style::StyleEngine;

    let document = parse(r#"<body><p class="lede">hello</p></body>"#);
    let p = find(&document.tree, "p");

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(".lede { color: red }").unwrap());
    let (styles, _) = engine.restyle(&document.tree);

    assert_eq!(
        styles.get(p).unwrap().color,
        crisol_style::values::Color::rgba(255, 0, 0, 255)
    );
}

#[test]
fn a_parsed_tree_lays_out() {
    use crisol_css::stylesheet::Stylesheet;
    use crisol_display_list::Size;
    use crisol_style::StyleEngine;

    let mut document = parse("<body><div></div></body>");
    let div = find(&document.tree, "div");

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse("div { height: 20px }").unwrap());
    let (styles, _) = engine.restyle(&document.tree);
    crisol_layout::layout(
        &mut document.tree,
        &styles,
        &mut crisol_text::FontSystem::empty(),
        Size::new(100.0, 50.0),
    );

    assert_eq!(
        document.tree.node(div).layout,
        crisol_display_list::Rect::from_xywh(0.0, 0.0, 100.0, 20.0)
    );
}

/// The first element with the given tag, in document order.
fn find(tree: &Tree, tag: &str) -> NodeId {
    let mut stack = vec![tree.root().expect("a parsed tree has a root")];
    while let Some(id) = stack.pop() {
        if tree.node(id).kind.tag() == Some(tag) {
            return id;
        }
        let mut child = tree.get(id).and_then(crisol_tree::Node::last_child);
        while let Some(node) = child {
            stack.push(node);
            child = tree.get(node).and_then(crisol_tree::Node::prev_sibling);
        }
    }
    panic!("no <{tag}> in the tree");
}
