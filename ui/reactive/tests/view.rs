//! The binding helpers, and what mounting owns.

use crisol_dom::Dom;
use crisol_reactive::{
    Cx, Runtime, append, bind_attribute, bind_class, bind_text, element, element_with_class, mount,
    text,
};
use crisol_tree::{NodeId, Tree};

fn text_of(dom: &Dom<'_>, node: NodeId) -> String {
    dom.tree()
        .get(node)
        .and_then(|node| node.kind.text())
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn a_bound_attribute_follows_its_signal_and_is_removed_by_none() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let root = dom.create_element("div");
    dom.set_root(root);

    let title = runtime.signal(Some(String::from("first")));
    let node = {
        let mut cx = Cx::new(&runtime, &mut dom);
        let node = element(&mut cx, "span");
        append(&mut cx, root, node);
        bind_attribute(&mut cx, node, "title", move |track| track.get(title));
        node
    };
    assert_eq!(dom.attribute(node, "title").as_deref(), Some("first"));

    runtime.set(title, Some(String::from("second")));
    runtime.flush(&mut dom);
    assert_eq!(dom.attribute(node, "title").as_deref(), Some("second"));

    // `None` is a removal, not the string "none" — the distinction a boolean attribute
    // like `disabled` depends on.
    runtime.set(title, None);
    runtime.flush(&mut dom);
    assert_eq!(dom.attribute(node, "title"), None);
}

#[test]
fn a_bound_class_toggles_without_disturbing_the_others() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let root = dom.create_element("div");
    dom.set_root(root);

    let active = runtime.signal(false);
    let node = {
        let mut cx = Cx::new(&runtime, &mut dom);
        let node = element_with_class(&mut cx, "li", "row wide");
        append(&mut cx, root, node);
        bind_class(&mut cx, node, "active", move |track| track.get(active));
        node
    };
    assert_eq!(dom.attribute(node, "class").as_deref(), Some("row wide"));

    runtime.set(active, true);
    runtime.flush(&mut dom);
    assert_eq!(
        dom.attribute(node, "class").as_deref(),
        Some("row wide active"),
        "the author's classes survive"
    );

    runtime.set(active, false);
    runtime.flush(&mut dom);
    assert_eq!(dom.attribute(node, "class").as_deref(), Some("row wide"));
}

#[test]
fn a_redundant_write_reaches_no_further_than_the_dom() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let root = dom.create_element("div");
    dom.set_root(root);

    let label = runtime.signal(String::from("same"));
    let node = {
        let mut cx = Cx::new(&runtime, &mut dom);
        let node = text(&mut cx, "");
        append(&mut cx, root, node);
        bind_text(&mut cx, node, move |track| track.get(label));
        node
    };
    dom.reset_stats();
    runtime.reset_stats();

    // The signal notifies unconditionally, so the effect runs — but the value is identical,
    // and the mutation API drops it. That division of labour is why `set` does not require
    // `PartialEq` on every signal.
    runtime.set(label, String::from("same"));
    runtime.flush(&mut dom);
    assert_eq!(runtime.stats().effects_run, 1, "the effect did run");
    assert_eq!(dom.stats().text_set, 0, "and wrote nothing");
    assert_eq!(dom.stats().no_ops, 1);
    assert_eq!(text_of(&dom, node), "same");
}

#[test]
fn unmounting_removes_the_nodes_and_stops_the_effects() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let root = dom.create_element("div");
    dom.set_root(root);

    let label = runtime.signal(String::from("hello"));
    let mounted = {
        let mut cx = Cx::new(&runtime, &mut dom);
        mount(&mut cx, root, move |cx: &mut Cx<'_, '_>| {
            let paragraph = element(cx, "p");
            let content = text(cx, "");
            append(cx, paragraph, content);
            bind_text(cx, content, move |track| track.get(label));
            paragraph
        })
    };
    assert_eq!(dom.children(root), vec![mounted.node]);

    runtime.set(label, String::from("goodbye"));
    runtime.flush(&mut dom);
    let content = dom.first_child(mounted.node).unwrap();
    assert_eq!(text_of(&dom, content), "goodbye");

    {
        let mut cx = Cx::new(&runtime, &mut dom);
        mounted.unmount(&mut cx);
    }
    assert!(dom.children(root).is_empty());
    assert!(!dom.is_alive(mounted.node) && !dom.is_alive(content));

    // The signal outlives the component that displayed it; writing it must not wake an
    // effect that would now be writing into a freed node.
    runtime.reset_stats();
    runtime.set(label, String::from("nobody is listening"));
    runtime.flush(&mut dom);
    assert_eq!(runtime.stats().effects_run, 0);
}

#[test]
fn a_component_can_mount_another_component_and_own_it() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let root = dom.create_element("div");
    dom.set_root(root);

    let label = runtime.signal(String::from("inner"));
    let outer = {
        let mut cx = Cx::new(&runtime, &mut dom);
        mount(&mut cx, root, move |cx: &mut Cx<'_, '_>| {
            let section = element(cx, "section");
            mount(cx, section, move |cx: &mut Cx<'_, '_>| {
                let child = element(cx, "p");
                let content = text(cx, "");
                append(cx, child, content);
                bind_text(cx, content, move |track| track.get(label));
                child
            });
            section
        })
    };

    let inner = dom.first_child(outer.node).expect("section > p");
    let content = dom.first_child(inner).expect("p > text");
    assert_eq!(text_of(&dom, content), "inner");

    // Disposing the outer scope has to reach the nested one, or the inner effect keeps
    // running against nodes the outer component removed.
    {
        let mut cx = Cx::new(&runtime, &mut dom);
        outer.unmount(&mut cx);
    }
    runtime.reset_stats();
    runtime.set(label, String::from("still here?"));
    runtime.flush(&mut dom);
    assert_eq!(runtime.stats().effects_run, 0);
    assert!(!dom.is_alive(inner));
}

#[test]
fn clearing_a_list_disposes_every_item() {
    use crisol_reactive::Keyed;

    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let list = dom.create_element("ul");
    dom.set_root(list);

    let labels: Vec<_> = (0..4)
        .map(|i| runtime.signal(format!("item {i}")))
        .collect();
    let keys: Vec<u32> = (0..4).collect();
    let mut keyed = Keyed::new(list);
    {
        let mut cx = Cx::new(&runtime, &mut dom);
        keyed.reconcile(&mut cx, &keys, |cx, &key| {
            let row = element(cx, "li");
            let content = text(cx, "");
            append(cx, row, content);
            let label = labels[key as usize];
            bind_text(cx, content, move |track| track.get(label));
            row
        });
    }
    let rows = keyed.nodes();
    assert_eq!(rows.len(), 4);
    assert_eq!(dom.children(list), rows, "the reconciler owns every child");
    assert!(!keyed.is_empty() && keyed.len() == 4);

    {
        let mut cx = Cx::new(&runtime, &mut dom);
        keyed.clear(&mut cx);
    }
    assert!(keyed.is_empty());
    assert!(dom.children(list).is_empty());
    assert!(rows.iter().all(|&row| !dom.is_alive(row)));

    // Every item's effects went with it: writing the signals they read wakes nothing.
    runtime.reset_stats();
    for (index, label) in labels.iter().enumerate() {
        runtime.set(*label, format!("changed {index}"));
    }
    runtime.flush(&mut dom);
    assert_eq!(runtime.stats().effects_run, 0);
}
