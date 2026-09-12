//! M7's acceptance.
//!
//! > A Rust-only todo app with add/remove/filter/edit runs with no full-tree rebuilds.
//!
//! "No full-tree rebuilds" is a claim about counters, not about pixels. An engine that threw
//! the list away and rebuilt it would render the same thing, so the assertions below are
//! about how many nodes were created, moved and laid out — and about node *identity*, which
//! is the part a rebuild cannot fake.
//!
//! The app is driven through the whole pipeline: reactive writes, then the DOM mutation API,
//! then M6's incremental restyle and relayout. Counting only DOM mutations would leave open
//! the possibility that the engine behind them redid everything anyway.

use std::cell::Cell;
use std::rc::Rc;

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::Size;
use crisol_dom::{Dom, DomStats};
use crisol_layout::{LayoutCache, LayoutContext, LayoutStats};
use crisol_reactive::{
    Cx, Keyed, ListStats, Memo, Runtime, Scope, Signal, append, bind_class, bind_text,
    element_with_class, text,
};
use crisol_style::{StyleEngine, StyleMap, StyleStats};
use crisol_text::FontSystem;
use crisol_tree::{NodeId, Tree};

const VIEWPORT: Size = Size {
    width: 400.0,
    height: 600.0,
};

const CSS: &str = "
    ul.todo-list { display: block; width: 100% }
    li.todo { display: block; height: 24px }
    li.done span.label { color: rgb(128, 128, 128) }
    span.label { display: block }
    p.count { display: block; height: 20px }
";

// ---- the app ------------------------------------------------------------------------------

/// One todo. Its text and its done-ness are signals the item's view binds to directly, so
/// editing one todo wakes one effect rather than every item in the list.
///
/// Keyed by `id`: two todos with the same text are still two todos.
#[derive(Clone, Copy, Debug)]
struct Todo {
    id: u32,
    label: Signal<String>,
    done: Signal<bool>,
    /// Owns `label` and `done`. Separate from the view's scope because the data outlives a
    /// filter that hides it.
    scope: Scope,
}

impl PartialEq for Todo {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Todo {}

impl std::hash::Hash for Todo {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Done,
}

struct App {
    todos: Signal<Vec<Todo>>,
    filter: Signal<Filter>,
    /// The filtered list the view renders. Public so a test can read it directly rather
    /// than inferring it from the tree.
    visible: Memo<Vec<Todo>>,
    list: NodeId,
    /// What the last reconcile pass did.
    last: Rc<Cell<ListStats>>,
    next_id: Cell<u32>,
}

fn build(runtime: &Runtime, dom: &mut Dom<'_>) -> App {
    let root = dom.create_element("html");
    dom.set_root(root);
    let body = dom.create_element("body");
    dom.append_child(root, body).unwrap();

    let todos = runtime.signal(Vec::<Todo>::new());
    let filter = runtime.signal(Filter::All);

    // Reading each todo's `done` from inside a read of the list is the nested read the
    // runtime has to support; it is also why the filter is a memo and not a plain closure.
    let visible = runtime.memo(move |track| {
        let filter = track.get(filter);
        track
            .with(todos, |todos| {
                todos
                    .iter()
                    .copied()
                    .filter(|todo| match filter {
                        Filter::All => true,
                        Filter::Active => !track.get(todo.done),
                        Filter::Done => track.get(todo.done),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });

    let list;
    {
        let mut cx = Cx::new(runtime, dom);
        list = element_with_class(&mut cx, "ul", "todo-list");
        append(&mut cx, body, list);

        let footer = element_with_class(&mut cx, "p", "count");
        append(&mut cx, body, footer);
        let remaining = text(&mut cx, "");
        append(&mut cx, footer, remaining);
        bind_text(&mut cx, remaining, move |track| {
            let left = track
                .with(todos, |todos| {
                    todos.iter().filter(|todo| !track.get(todo.done)).count()
                })
                .unwrap_or_default();
            format!("{left} items left")
        });
    }

    let last = Rc::new(Cell::new(ListStats::default()));
    let recorder = Rc::clone(&last);
    let mut keyed = Keyed::new(list);
    runtime.effect(dom, move |cx| {
        let keys = cx.memo(visible);
        let stats = keyed.reconcile(cx, &keys, |cx, todo| item(cx, *todo));
        recorder.set(stats);
    });

    App {
        todos,
        filter,
        visible,
        list,
        last,
        next_id: Cell::new(0),
    }
}

/// One item's view. Runs once per todo; nothing re-runs it.
fn item(cx: &mut Cx<'_, '_>, todo: Todo) -> NodeId {
    let row = element_with_class(cx, "li", "todo");
    let label = element_with_class(cx, "span", "label");
    append(cx, row, label);
    let content = text(cx, "");
    append(cx, label, content);

    bind_text(cx, content, move |track| track.get(todo.label));
    // One class, not a rewritten `class` attribute: the style engine can then invalidate
    // this node and its following siblings rather than the subtree.
    bind_class(cx, row, "done", move |track| track.get(todo.done));
    row
}

// ---- operations ---------------------------------------------------------------------------

fn add(runtime: &Runtime, app: &App, label: &str) -> Todo {
    let id = app.next_id.get();
    app.next_id.set(id + 1);
    let (scope, (label, done)) =
        runtime.scope(|_| (runtime.signal(label.to_owned()), runtime.signal(false)));
    let todo = Todo {
        id,
        label,
        done,
        scope,
    };
    runtime.update(app.todos, |todos| todos.push(todo));
    todo
}

fn remove(runtime: &Runtime, dom: &mut Dom<'_>, app: &App, todo: Todo) {
    runtime.update(app.todos, |todos| todos.retain(|other| other.id != todo.id));
    runtime.flush(dom);
    // Only now. The item's effects read `label` and `done`, and they are disposed by the
    // reconciler during the flush above — freeing the signals first would leave those
    // effects reading handles whose slots had been reused.
    runtime.dispose(todo.scope, dom);
}

fn edit(runtime: &Runtime, todo: Todo, label: &str) {
    runtime.set(todo.label, label.to_owned());
}

fn toggle(runtime: &Runtime, todo: Todo) {
    let now = runtime.peek(todo.done).unwrap_or_default();
    runtime.set(todo.done, !now);
}

// ---- the pipeline behind it -----------------------------------------------------------------

/// Style and lay out, the way a frame would.
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
            fonts: FontSystem::new(),
            cache: LayoutCache::new(),
            styles: StyleMap::default(),
        }
    }

    fn run(&mut self, tree: &mut Tree) -> (StyleStats, LayoutStats) {
        let (styles, style_stats) = self.engine.restyle_incremental(tree, &self.styles);
        self.styles = styles;
        let mut context = LayoutContext::new(tree, &self.styles, &mut self.fonts, &mut self.cache);
        context.run(VIEWPORT);
        (style_stats, context.stats())
    }
}

/// The text of every label in the list, read straight out of the tree.
fn labels(dom: &Dom<'_>, app: &App) -> Vec<String> {
    dom.children(app.list)
        .into_iter()
        .map(|row| {
            let label = dom.first_child(row).expect("li > span");
            let content = dom.first_child(label).expect("span > text");
            dom.tree()
                .get(content)
                .and_then(|node| node.kind.text())
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

// ---- M7's acceptance -------------------------------------------------------------------------

const ITEMS: usize = 1_000;

/// Runs one user action, then a frame, and reports what each layer did.
fn step<R>(
    runtime: &Runtime,
    tree: &mut Tree,
    frame: &mut Frame,
    app: &App,
    name: &str,
    operation: impl FnOnce(&Runtime, &mut Dom<'_>, &App) -> R,
) -> (R, DomStats, ListStats, LayoutStats) {
    runtime.reset_stats();
    // Cleared so that "the list effect never ran" is visible as zeroes rather than as the
    // previous step's numbers.
    app.last.set(ListStats::default());

    let (result, dom_stats) = {
        let mut dom = Dom::new(tree);
        let result = operation(runtime, &mut dom, app);
        runtime.flush(&mut dom);
        (result, dom.stats())
    };
    let list = app.last.get();
    let (_, layout) = frame.run(tree);

    println!(
        "  {name:<22} dom: {created:>2} created {inserted:>2} inserted {removed:>2} removed \
         {text:>2} text {attributes:>2} attrs {no_ops:>2} no-ops | list: {lc} new {lm} moved \
         {lr} gone {lk} kept | effects: {effects:>2} | layout: {invalidated:>4} invalidated {laid:>4} laid out {unchanged:>4} unchanged of {total}",
        created = dom_stats.created,
        inserted = dom_stats.inserted,
        removed = dom_stats.removed,
        text = dom_stats.text_set,
        attributes = dom_stats.attributes_set,
        no_ops = dom_stats.no_ops,
        lc = list.created,
        lm = list.moved,
        lr = list.removed,
        lk = list.kept,
        effects = runtime.stats().effects_run,
        invalidated = layout.caches_invalidated,
        laid = layout.nodes_laid_out,
        unchanged = layout.unchanged,
        total = tree.len(),
    );
    (result, dom_stats, list, layout)
}

#[test]
fn a_todo_app_adds_removes_filters_and_edits_without_rebuilding_the_tree() {
    let runtime = Runtime::new();
    let mut tree = Tree::new();
    let mut frame = Frame::new();

    let app = {
        let mut dom = Dom::new(&mut tree);
        let app = build(&runtime, &mut dom);
        for index in 0..ITEMS {
            add(&runtime, &app, &format!("todo number {index}"));
        }
        runtime.flush(&mut dom);
        app
    };
    let (_, first) = frame.run(&mut tree);
    println!("\n{ITEMS} todos; {} nodes\n", tree.len());
    assert!(
        first.nodes_laid_out > ITEMS,
        "the first pass really does lay everything out: {}",
        first.nodes_laid_out
    );

    // The identity check that a rebuild cannot fake. Every assertion below compares against
    // these handles: a generational id reused for a freshly created node would not match.
    let before = Dom::new(&mut tree).children(app.list);
    assert_eq!(before.len(), ITEMS);

    // ---- add ---------------------------------------------------------------------------
    let (added, dom, list, layout) = step(
        &runtime,
        &mut tree,
        &mut frame,
        &app,
        "add",
        |runtime, _, app| add(runtime, app, "a new todo"),
    );

    assert_eq!(
        (list.created, list.moved, list.removed, list.kept),
        (1, 0, 0, ITEMS),
        "one item built; every existing item stayed exactly where it was"
    );
    assert_eq!(dom.removed, 0, "nothing was torn down to add one row");
    assert_eq!(
        dom.created, 3,
        "li, span and the text node — and nothing else"
    );
    assert!(
        layout.nodes_laid_out < 40,
        "appending to a {ITEMS}-item list relaid {} nodes",
        layout.nodes_laid_out
    );

    let after_add = Dom::new(&mut tree).children(app.list);
    assert_eq!(
        after_add[..ITEMS],
        before[..],
        "every original row is the same node it was"
    );

    // ---- edit --------------------------------------------------------------------------
    let middle = ITEMS / 2;
    let (_, dom, list, layout) = step(
        &runtime,
        &mut tree,
        &mut frame,
        &app,
        "edit (item 500)",
        |runtime, _, app| {
            let todo = runtime
                .peek(app.todos)
                .expect("todos")
                .into_iter()
                .find(|todo| todo.id == middle as u32)
                .expect("item 500");
            edit(runtime, todo, "edited in place");
        },
    );

    assert_eq!(dom.text_set, 1, "one text node written, out of {ITEMS}");
    assert_eq!(
        (dom.created, dom.inserted, dom.removed, dom.attributes_set),
        (0, 0, 0, 0),
        "an edit touches no structure at all"
    );
    assert_eq!(
        runtime.stats().effects_run,
        1,
        "and wakes exactly one effect: the label binding for that todo"
    );
    assert_eq!(
        (list.created, list.moved, list.removed),
        (0, 0, 0),
        "the list effect did not even run — the filter does not read labels"
    );
    assert!(layout.nodes_laid_out < 20, "{}", layout.nodes_laid_out);

    let dom_view = Dom::new(&mut tree);
    assert_eq!(labels(&dom_view, &app)[middle], "edited in place");
    assert_eq!(dom_view.children(app.list)[..ITEMS], before[..]);

    // ---- toggle, which the filter does read ----------------------------------------------
    let (_, dom, list, _) = step(
        &runtime,
        &mut tree,
        &mut frame,
        &app,
        "toggle (item 500)",
        |runtime, _, app| {
            let todo = runtime
                .peek(app.todos)
                .expect("todos")
                .into_iter()
                .find(|todo| todo.id == middle as u32)
                .expect("item 500");
            toggle(runtime, todo);
        },
    );

    assert_eq!(dom.attributes_set, 1, "one class toggled");
    assert_eq!(dom.text_set, 1, "and the footer count rewritten");
    assert_eq!(
        (dom.created, dom.inserted, dom.removed),
        (0, 0, 0),
        "no structure changes when an item is ticked"
    );
    assert_eq!(
        (list.created, list.moved, list.removed, list.kept),
        (0, 0, 0, 0),
        "the list effect did not run at all: under `All` the filter never reads `done`, so \
         it never subscribed to it. Dependencies are what a closure actually read on its \
         last run, not what it might read."
    );
    assert_eq!(
        runtime.stats().effects_run,
        2,
        "exactly two: this row's class binding and the footer count"
    );

    // ---- filter ------------------------------------------------------------------------
    let (_, dom, list, layout) = step(
        &runtime,
        &mut tree,
        &mut frame,
        &app,
        "filter (active)",
        |runtime, _, app| {
            runtime.set(app.filter, Filter::Active);
        },
    );

    assert_eq!(
        (list.created, list.moved, list.removed, list.kept),
        (0, 0, 1, ITEMS),
        "hiding one row moves none of the others"
    );
    assert_eq!(dom.removed, 3, "the row's li, span and text");
    assert_eq!(dom.created, 0);
    assert!(layout.nodes_laid_out < 40, "{}", layout.nodes_laid_out);

    let (_, dom, list, _) = step(
        &runtime,
        &mut tree,
        &mut frame,
        &app,
        "filter (all)",
        |runtime, _, app| {
            runtime.set(app.filter, Filter::All);
        },
    );

    assert_eq!(
        (list.created, list.moved, list.removed, list.kept),
        (1, 0, 0, ITEMS),
        "restoring the row rebuilds one item and moves nothing"
    );
    assert_eq!(dom.created, 3);

    let restored = Dom::new(&mut tree).children(app.list);
    assert_eq!(restored.len(), ITEMS + 1);
    assert_ne!(
        restored[middle], before[middle],
        "the hidden row was genuinely destroyed and rebuilt, not parked somewhere"
    );
    assert_eq!(
        restored[..middle],
        before[..middle],
        "and it came back in the right place, with its neighbours untouched"
    );

    // ---- remove ------------------------------------------------------------------------
    let scopes_before = runtime.stats().scopes_disposed;
    let (_, dom, list, layout) = step(
        &runtime,
        &mut tree,
        &mut frame,
        &app,
        "remove (item 100)",
        |runtime, dom, app| {
            let todo = runtime
                .peek(app.todos)
                .expect("todos")
                .into_iter()
                .find(|todo| todo.id == 100)
                .expect("item 100");
            remove(runtime, dom, app, todo);
        },
    );

    assert_eq!(
        (list.created, list.moved, list.removed, list.kept),
        (0, 0, 1, ITEMS),
        "one row gone; the other thousand did not move"
    );
    assert_eq!(dom.removed, 3);
    assert_eq!(dom.created, 0, "removing builds nothing");
    assert!(layout.nodes_laid_out < 40, "{}", layout.nodes_laid_out);
    assert!(
        runtime.stats().scopes_disposed > scopes_before,
        "the removed item's effects and signals were freed, not leaked"
    );

    // ---- the filter that empties most of the list ------------------------------------------
    let (_, dom, list, _) = step(
        &runtime,
        &mut tree,
        &mut frame,
        &app,
        "filter (done)",
        |runtime, _, app| {
            runtime.set(app.filter, Filter::Done);
        },
    );

    // Nothing forbids work proportional to what actually changed — only work proportional
    // to the document. Hiding 999 rows costs 999 removals, and that is the right answer.
    assert_eq!(
        (list.created, list.moved, list.removed, list.kept),
        (0, 0, ITEMS - 1, 1),
        "one item is done; the rest are gone, and the survivor did not move"
    );
    assert_eq!(dom.created, 0, "emptying a list builds nothing");

    {
        let mut dom = Dom::new(&mut tree);
        let cx = Cx::new(&runtime, &mut dom);
        let visible = cx.memo(app.visible);
        assert_eq!(visible.len(), 1);
        assert_eq!(
            runtime.peek(visible[0].label).as_deref(),
            Some("edited in place")
        );
    }
    assert_eq!(labels(&Dom::new(&mut tree), &app), vec!["edited in place"]);

    let (_, _, list, _) = step(
        &runtime,
        &mut tree,
        &mut frame,
        &app,
        "filter (all again)",
        |runtime, _, app| {
            runtime.set(app.filter, Filter::All);
        },
    );
    assert_eq!(
        (list.created, list.moved, list.removed, list.kept),
        (ITEMS - 1, 0, 0, 1),
        "the survivor anchors the rebuild, so nothing has to move around it"
    );

    // ---- and the result is actually right ------------------------------------------------
    let dom_view = Dom::new(&mut tree);
    let labels = labels(&dom_view, &app);
    assert_eq!(labels.len(), ITEMS);
    assert_eq!(labels[0], "todo number 0");
    assert_eq!(labels[99], "todo number 99");
    assert_eq!(labels[100], "todo number 101", "101 closed the gap");
    assert_eq!(labels[middle - 1], "edited in place");
    assert_eq!(labels[ITEMS - 1], "a new todo");
    assert_eq!(
        runtime.peek(added.label).as_deref(),
        Some("a new todo"),
        "the handle returned at add time still points at the right todo"
    );

    // A final full restyle must agree with what the incremental passes produced. If any
    // step above had skipped an invalidation, the tree would have been quietly wrong for
    // every frame since.
    // The same engine, so its interner is shared and pointer equality is meaningful — which
    // also checks that equal styles really did come back as one allocation (D-21).
    let expected = frame.engine.restyle(&tree).0;
    let mut pending = vec![tree.root().expect("root")];
    while let Some(node) = pending.pop() {
        pending.extend(tree.children(node));
        assert_eq!(
            expected.get(node).map(std::sync::Arc::as_ptr),
            frame.styles.get(node).map(std::sync::Arc::as_ptr),
            "node {node:?} was styled differently by the incremental path"
        );
    }
    println!();
}
