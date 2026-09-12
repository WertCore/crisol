//! The `TreeSink` html5ever drives.

use std::borrow::Cow;
use std::cell::RefCell;

use crisol_tree::{Attribute as CrisolAttribute, NodeId, Tree};
use html5ever::interface::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{
    Attribute, LocalName, Namespace, ParseOpts, QualName, local_name, ns,
    parse_document as h5_parse,
};

use crate::Document;

/// Something html5ever objected to.
///
/// HTML has no fatal parse errors, so these never stop a parse.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ParseError {
    /// html5ever's description.
    pub message: String,
}

/// An element name the sink can hand back without borrowing from the tree.
///
/// `ElemName` returns references, so the type has to own the atoms. Both are interned and
/// refcounted, which makes cloning one about as cheap as copying a pointer.
#[derive(Clone, Debug)]
pub(crate) struct OwnedName {
    ns: Namespace,
    local: LocalName,
}

impl ElemName for OwnedName {
    fn ns(&self) -> &Namespace {
        &self.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.local
    }
}

/// Builds a [`Tree`] from html5ever's callbacks.
///
/// Everything is behind a `RefCell` because `TreeSink` takes `&self`: html5ever holds the
/// sink for the whole parse and calls into it re-entrantly. The borrows are short and never
/// nested, so a conflict would be a bug here rather than a plausible runtime state.
struct Sink {
    tree: RefCell<Tree>,
    errors: RefCell<Vec<ParseError>>,
    quirks: RefCell<bool>,
    /// Handles for nodes the engine does not keep — comments and processing instructions.
    ///
    /// html5ever needs a handle back from `create_comment` and will later ask to append it.
    /// Recording which handles are placeholders is what lets `append` drop them, instead of
    /// attaching an empty node that splits the text run around it in two.
    discarded: RefCell<std::collections::HashSet<NodeId>>,
    /// The node html5ever treats as the document. Its children are `<html>` and any stray
    /// top-level content.
    document: NodeId,
}

impl Sink {
    fn new() -> Self {
        let mut tree = Tree::new();
        // A placeholder parent for `<html>`. It is discarded in `finish`, because CSS's
        // `:root` means `<html>` and the engine has no use for a node above it.
        let document = tree.create_element("#document");
        Self {
            tree: RefCell::new(tree),
            errors: RefCell::new(Vec::new()),
            quirks: RefCell::new(false),
            discarded: RefCell::new(std::collections::HashSet::new()),
            document,
        }
    }

    /// Appends text to `parent`, merging into a preceding text node when there is one.
    ///
    /// html5ever emits text in whatever chunks the tokenizer produced — an entity reference
    /// splits a run in two. Merging here means one text node per run of text, which is what
    /// the shaper at M4 wants and what the DOM specifies.
    fn append_text(&self, parent: NodeId, text: &str) {
        let mut tree = self.tree.borrow_mut();
        if let Some(last) = tree.get(parent).and_then(crisol_tree::Node::last_child)
            && let Some(crisol_tree::NodeKind::Text(existing)) =
                tree.get_mut(last).map(|node| &mut node.kind)
        {
            existing.push_str(text);
            return;
        }
        let node = tree.create_text(text);
        let _ = tree.append_child(parent, node);
    }

    /// A handle for a node the engine does not keep.
    fn placeholder(&self) -> NodeId {
        let id = self.tree.borrow_mut().create_text("");
        self.discarded.borrow_mut().insert(id);
        id
    }

    fn is_discarded(&self, id: NodeId) -> bool {
        self.discarded.borrow().contains(&id)
    }

    fn append_text_before(&self, sibling: NodeId, text: &str) {
        let mut tree = self.tree.borrow_mut();
        let Some(parent) = tree.parent(sibling) else {
            return;
        };
        let node = tree.create_text(text);
        let _ = tree.insert_before(parent, node, sibling);
    }

    /// Drops the placeholder document node and roots the tree at `<html>`.
    fn finish_tree(self) -> Document {
        let mut tree = self.tree.into_inner();
        // Placeholders were never attached; freeing them keeps the arena the size of the
        // document rather than the size of the source.
        for id in self.discarded.into_inner() {
            tree.remove_subtree(id);
        }
        let html = tree
            .children(self.document)
            .find(|id| tree.node(*id).kind.tag() == Some("html"));

        if let Some(html) = html {
            tree.detach(html);
            let _ = tree.set_root(html);
            tree.remove_subtree(self.document);
        }

        Document {
            tree,
            errors: self.errors.into_inner(),
            quirks: self.quirks.into_inner(),
        }
    }
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Document;
    type ElemName<'a>
        = OwnedName
    where
        Self: 'a;

    fn finish(self) -> Document {
        self.finish_tree()
    }

    fn parse_error(&self, message: Cow<'static, str>) {
        self.errors.borrow_mut().push(ParseError {
            message: message.into_owned(),
        });
    }

    fn get_document(&self) -> NodeId {
        self.document
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> OwnedName {
        // Called constantly, not just for foreign content: the tree builder asks "what is the
        // current open element?" on nearly every token, and answers about implied end tags
        // and scope depend on it. Returning a fixed name here silently mis-nests everything.
        //
        // The name has to be owned rather than borrowed because the tree is behind a
        // `RefCell`. `LocalName` is an interned atom, so reconstructing it from the tag is a
        // hash lookup rather than an allocation.
        let tree = self.tree.borrow();
        let local = tree
            .get(*target)
            .and_then(|node| node.kind.tag().map(LocalName::from))
            .unwrap_or(local_name!(""));
        // There is one namespace: foreign content (SVG, MathML) is out of scope, so every
        // element is an HTML element.
        OwnedName {
            ns: ns!(html),
            local,
        }
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> NodeId {
        let mut tree = self.tree.borrow_mut();
        let id = tree.create_element(&name.local);
        if let Some(data) = tree.element_mut(id) {
            for attribute in attrs {
                let key = attribute.name.local.to_string();
                match key.as_str() {
                    "id" => data.id = Some(attribute.value.as_ref().into()),
                    "class" => data.set_class(&attribute.value),
                    _ => data
                        .attributes
                        .push(CrisolAttribute::new(&key, &attribute.value)),
                }
            }
        }
        id
    }

    fn create_comment(&self, _text: StrTendril) -> NodeId {
        // Comments have no box and no effect on layout or selectors, so the engine does not
        // keep them. html5ever still needs a valid handle back, so a placeholder is created
        // and recorded; `append` drops it rather than attaching it.
        self.placeholder()
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> NodeId {
        // Processing instructions cannot appear in HTML, only in XML. Same treatment.
        self.placeholder()
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        match child {
            NodeOrText::AppendNode(node) => {
                if self.is_discarded(node) {
                    return;
                }
                let mut tree = self.tree.borrow_mut();
                let _ = tree.append_child(*parent, node);
            }
            NodeOrText::AppendText(text) => self.append_text(*parent, &text),
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        // The "foster parenting" case: content that appears where it cannot legally go, such
        // as text directly inside a `<table>`. html5ever asks us to put it before the element
        // when the element has a parent, and inside the fallback otherwise.
        let has_parent = self.tree.borrow().parent(*element).is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        _name: StrTendril,
        _public: StrTendril,
        _system: StrTendril,
    ) {
        // The doctype's only job is deciding quirks mode, which html5ever has already done by
        // the time it tells us, and which ROADMAP §1 puts out of scope anyway.
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        // `<template>` content is inert: not rendered, not matched by selectors. Treating the
        // element as its own content means html5ever parses the children into a subtree that
        // simply never gets laid out, which is the correct visible behaviour.
        *target
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        *self.quirks.borrow_mut() = mode != QuirksMode::NoQuirks;
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        match new_node {
            NodeOrText::AppendNode(node) => {
                if self.is_discarded(node) {
                    return;
                }
                let mut tree = self.tree.borrow_mut();
                if let Some(parent) = tree.parent(*sibling) {
                    let _ = tree.insert_before(parent, node, *sibling);
                }
            }
            NodeOrText::AppendText(text) => self.append_text_before(*sibling, &text),
        }
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut tree = self.tree.borrow_mut();
        let Some(data) = tree.element_mut(*target) else {
            return;
        };
        for attribute in attrs {
            let key = attribute.name.local.to_string();
            match key.as_str() {
                "id" if data.id.is_none() => data.id = Some(attribute.value.as_ref().into()),
                "class" if data.classes.is_empty() => data.set_class(&attribute.value),
                "id" | "class" => {}
                _ if !data.has_attribute(&key) => data
                    .attributes
                    .push(CrisolAttribute::new(&key, &attribute.value)),
                _ => {}
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.tree.borrow_mut().detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let mut tree = self.tree.borrow_mut();
        while let Some(child) = tree.first_child(*node) {
            let _ = tree.append_child(*new_parent, child);
        }
    }
}

pub(crate) fn parse_document(source: &str) -> Document {
    h5_parse(Sink::new(), ParseOpts::default())
        .from_utf8()
        .read_from(&mut source.as_bytes())
        .expect("reading from a &str cannot fail")
}

pub(crate) fn parse_fragment(source: &str) -> Document {
    let context = QualName::new(None, html5ever::ns!(html), html5ever::local_name!("div"));
    let parser = html5ever::driver::parse_fragment(
        Sink::new(),
        ParseOpts::default(),
        context,
        Vec::new(),
        false,
    );
    let mut document = parser
        .from_utf8()
        .read_from(&mut source.as_bytes())
        .expect("reading from a &str cannot fail");

    // A fragment parse puts the fragment's nodes directly under `<html>`. Move them into a
    // `<div>` matching the context element, so the caller gets a root that reflects what they
    // asked to parse rather than a wrapper the algorithm needed internally.
    if let Some(root) = document.tree.root() {
        let holder = document.tree.create_element("div");
        while let Some(child) = document.tree.first_child(root) {
            let _ = document.tree.append_child(holder, child);
        }
        let _ = document.tree.set_root(holder);
        document.tree.remove_subtree(root);
    }
    document
}
