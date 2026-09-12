//! Selector matching against a Crisol tree.
//!
//! These are the tests that say what the selector dialect *is*: which selectors work, which
//! are rejected at parse time, and what the deliberate departures from browser behaviour
//! are.

use crisol_css::matching::{MatchCaches, parse_selector_list};
use crisol_css::{ElementRef, matching};
use crisol_tree::{Attribute, ElementState, NodeId, Tree};

/// Builds a tree from a tiny nested description and returns it with the named nodes.
///
/// ```text
/// <body>
///   <header id="top"><h1 class="title">…</h1></header>
///   <main class="content wide">
///     <p class="lede">…</p>
///     <p>…</p>
///     <a href="/x">…</a>
///     <button disabled>…</button>
///   </main>
/// </body>
/// ```
struct Fixture {
    tree: Tree,
    body: NodeId,
    header: NodeId,
    h1: NodeId,
    main: NodeId,
    lede: NodeId,
    second_p: NodeId,
    link: NodeId,
    button: NodeId,
}

impl Fixture {
    fn new() -> Self {
        let mut tree = Tree::new();
        let body = tree.create_element("body");
        tree.set_root(body).unwrap();

        let header = tree.create_element("header");
        tree.append_child(body, header).unwrap();
        tree.element_mut(header).unwrap().id = Some("top".into());

        let h1 = tree.create_element("h1");
        tree.append_child(header, h1).unwrap();
        tree.element_mut(h1).unwrap().set_class("title");
        let heading_text = tree.create_text("Crisol");
        tree.append_child(h1, heading_text).unwrap();

        let main = tree.create_element("main");
        tree.append_child(body, main).unwrap();
        tree.element_mut(main).unwrap().set_class("content wide");

        let lede = tree.create_element("p");
        tree.append_child(main, lede).unwrap();
        tree.element_mut(lede).unwrap().set_class("lede");

        let second_p = tree.create_element("p");
        tree.append_child(main, second_p).unwrap();

        let link = tree.create_element("a");
        tree.append_child(main, link).unwrap();
        tree.element_mut(link)
            .unwrap()
            .attributes
            .push(Attribute::new("href", "/x"));

        let button = tree.create_element("button");
        tree.append_child(main, button).unwrap();
        tree.element_mut(button).unwrap().state = ElementState::DISABLED;

        Self {
            tree,
            body,
            header,
            h1,
            main,
            lede,
            second_p,
            link,
            button,
        }
    }

    fn matches(&self, selector: &str, node: NodeId) -> bool {
        let list = parse_selector_list(selector)
            .unwrap_or_else(|e| panic!("could not parse {selector:?}: {e}"));
        let element = ElementRef::new(&self.tree, node).expect("not an element");
        matching::matches_any(&list, element)
    }

    /// Every element the selector matches, in document order, described by tag and class.
    fn all(&self, selector: &str) -> Vec<String> {
        let list = parse_selector_list(selector)
            .unwrap_or_else(|e| panic!("could not parse {selector:?}: {e}"));
        let mut caches = MatchCaches::new();
        let mut found = Vec::new();
        let mut stack = vec![self.tree.root().unwrap()];
        while let Some(id) = stack.pop() {
            let mut child = self.tree.get(id).and_then(crisol_tree::Node::last_child);
            while let Some(node) = child {
                stack.push(node);
                child = self
                    .tree
                    .get(node)
                    .and_then(crisol_tree::Node::prev_sibling);
            }
            let Some(element) = ElementRef::new(&self.tree, id) else {
                continue;
            };
            if caches.matches_any(&list, element) {
                let data = element.data();
                found.push(match data.classes.first() {
                    Some(class) => format!("{}.{}", data.tag, class),
                    None => data.tag.to_string(),
                });
            }
        }
        found
    }
}

// ---- the basics --------------------------------------------------------------------

#[test]
fn type_selectors_match_the_tag() {
    let f = Fixture::new();
    assert!(f.matches("main", f.main));
    assert!(!f.matches("main", f.body));
}

#[test]
fn tag_names_are_ascii_case_insensitive() {
    let f = Fixture::new();
    assert!(f.matches("MAIN", f.main));
    assert!(f.matches("MaIn", f.main));
}

#[test]
fn the_universal_selector_matches_every_element_and_no_text() {
    let f = Fixture::new();
    assert_eq!(
        f.all("*").len(),
        8,
        "body, header, h1, main, two p, a, button — and the text nodes are not elements"
    );
}

#[test]
fn class_selectors_match_any_of_several_classes() {
    let f = Fixture::new();
    assert!(f.matches(".content", f.main));
    assert!(f.matches(".wide", f.main));
    assert!(!f.matches(".narrow", f.main));
}

#[test]
fn class_selectors_are_case_sensitive() {
    let f = Fixture::new();
    assert!(
        !f.matches(".CONTENT", f.main),
        "class names are not case-folded"
    );
}

#[test]
fn id_selectors_match_the_id_attribute() {
    let f = Fixture::new();
    assert!(f.matches("#top", f.header));
    assert!(!f.matches("#top", f.body));
}

#[test]
fn compound_selectors_require_every_part() {
    let f = Fixture::new();
    assert!(f.matches("main.content.wide", f.main));
    assert!(!f.matches("main.content.narrow", f.main));
    assert!(!f.matches("section.content", f.main));
}

// ---- combinators -------------------------------------------------------------------

#[test]
fn descendant_combinator_crosses_generations() {
    let f = Fixture::new();
    assert!(f.matches("body h1", f.h1));
    assert!(f.matches("header h1", f.h1));
    assert!(!f.matches("main h1", f.h1));
}

#[test]
fn child_combinator_does_not() {
    let f = Fixture::new();
    assert!(f.matches("header > h1", f.h1));
    assert!(!f.matches("body > h1", f.h1), "h1 is a grandchild of body");
}

#[test]
fn sibling_combinators_walk_the_element_list() {
    let f = Fixture::new();
    assert!(f.matches("p + p", f.second_p));
    assert!(!f.matches("p + p", f.lede), "the lede has no preceding p");
    assert!(f.matches("p ~ button", f.button));
    assert!(f.matches("p ~ a", f.link));
}

#[test]
fn combinators_skip_text_nodes() {
    // The h1 has a text child. A text node between two elements must not break `+`.
    let mut f = Fixture::new();
    let text = f.tree.create_text("   ");
    f.tree.insert_before(f.main, text, f.second_p).unwrap();
    assert!(
        f.matches("p + p", f.second_p),
        "sibling combinators walk elements, not nodes"
    );
}

// ---- attributes --------------------------------------------------------------------

#[test]
fn attribute_selectors_cover_the_operators() {
    let f = Fixture::new();
    assert!(f.matches("[href]", f.link));
    assert!(f.matches("[href=\"/x\"]", f.link));
    assert!(f.matches("[href^=\"/\"]", f.link));
    assert!(f.matches("[href$=\"x\"]", f.link));
    assert!(f.matches("[href*=\"/x\"]", f.link));
    assert!(!f.matches("[href=\"/y\"]", f.link));
    assert!(!f.matches("[href]", f.main));
}

#[test]
fn id_and_class_are_reachable_as_attributes() {
    // They are stored in their own fields for matching speed; a selector that goes through
    // the attribute syntax must still find them.
    let f = Fixture::new();
    assert!(f.matches("[id=\"top\"]", f.header));
    assert!(f.matches("[class]", f.main));
}

// ---- structural pseudo-classes -----------------------------------------------------

#[test]
fn structural_pseudo_classes_work_off_the_tree_shape() {
    let f = Fixture::new();
    assert!(f.matches(":root", f.body));
    assert!(!f.matches(":root", f.main));
    assert!(f.matches("p:first-child", f.lede));
    assert!(f.matches("button:last-child", f.button));
    assert!(f.matches("p:nth-child(2)", f.second_p));
    assert!(f.matches("p:first-of-type", f.lede));
}

#[test]
fn empty_is_about_content_not_about_elements() {
    let f = Fixture::new();
    assert!(f.matches(":empty", f.second_p), "no children at all");
    assert!(
        !f.matches(":empty", f.h1),
        "an element with a text child is not empty, even with no element children"
    );
}

#[test]
fn not_and_is_compose() {
    let f = Fixture::new();
    assert!(f.matches("p:not(.lede)", f.second_p));
    assert!(!f.matches("p:not(.lede)", f.lede));
    assert!(f.matches(":is(main, header)", f.main));
    assert!(f.matches(":where(main, header)", f.header));
}

// ---- state pseudo-classes ----------------------------------------------------------

#[test]
fn state_pseudo_classes_read_element_state() {
    let mut f = Fixture::new();
    assert!(!f.matches("a:hover", f.link));

    f.tree.element_mut(f.link).unwrap().state = ElementState::HOVER | ElementState::FOCUS;
    assert!(f.matches("a:hover", f.link));
    assert!(f.matches("a:focus", f.link));
    assert!(f.matches("a:hover:focus", f.link));
    assert!(!f.matches("a:active", f.link));
}

#[test]
fn disabled_matches_and_enabled_is_not_its_inverse() {
    let f = Fixture::new();
    assert!(f.matches(":disabled", f.button));
    assert!(!f.matches(":enabled", f.button));
    // The crucial difference: a div is neither enabled nor disabled, so `:not(:disabled)`
    // matches it and `:enabled` does not.
    assert!(!f.matches(":enabled", f.main));
    assert!(f.matches(":not(:disabled)", f.main));
}

#[test]
fn an_anchor_without_an_href_is_not_a_link() {
    // There is no `:link` or `:any-link` in the dialect — a document with no history has
    // nothing to say about visitedness (ROADMAP §1). The distinction is still reachable
    // through the attribute selector, which is what an author would use here anyway.
    let mut f = Fixture::new();
    let bare = f.tree.create_element("a");
    f.tree.append_child(f.main, bare).unwrap();
    assert!(f.matches("a[href]", f.link));
    assert!(!f.matches("a[href]", bare));
    assert!(parse_selector_list(":any-link").is_err());
}

// ---- what the dialect deliberately refuses -----------------------------------------

#[test]
fn unsupported_pseudo_classes_are_a_parse_error_not_a_silent_non_match() {
    for selector in [":visited", ":target", ":lang(en)", ":has(p)"] {
        let error = parse_selector_list(selector).unwrap_err();
        assert_eq!(error.source_text, selector);
        assert!(
            !error.message.is_empty(),
            "{selector} should report why it was rejected"
        );
    }
}

#[test]
fn pseudo_elements_are_rejected() {
    for selector in ["p::before", "p::after", "p::first-line"] {
        assert!(
            parse_selector_list(selector).is_err(),
            "{selector} has no box in the tree, so it must not parse"
        );
    }
}

#[test]
fn malformed_selectors_report_a_position() {
    let error = parse_selector_list("div >").unwrap_err();
    assert_eq!(error.line, 1);
    assert!(error.column > 0);
}

// ---- document-order queries --------------------------------------------------------

#[test]
fn matching_a_whole_tree_returns_document_order() {
    let f = Fixture::new();
    assert_eq!(f.all("p"), vec!["p.lede", "p"]);
    assert_eq!(f.all(".content p"), vec!["p.lede", "p"]);
    assert_eq!(f.all("body > *"), vec!["header", "main.content"]);
}

#[test]
fn a_selector_list_matches_the_union() {
    let f = Fixture::new();
    assert_eq!(f.all("h1, button"), vec!["h1.title", "button"]);
}

#[test]
fn reusing_caches_gives_the_same_answers() {
    let f = Fixture::new();
    let list = parse_selector_list("p:nth-child(2)").unwrap();
    let mut caches = MatchCaches::new();
    let element = ElementRef::new(&f.tree, f.second_p).unwrap();
    for _ in 0..4 {
        assert!(caches.matches_any(&list, element));
    }
    let lede = ElementRef::new(&f.tree, f.lede).unwrap();
    assert!(!caches.matches_any(&list, lede));
}

#[test]
fn a_text_node_is_not_an_element() {
    let mut f = Fixture::new();
    let text = f.tree.create_text("hello");
    f.tree.append_child(f.main, text).unwrap();
    assert!(ElementRef::new(&f.tree, text).is_none());
}
