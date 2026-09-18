//! M7's todo app, in a window: `cargo run -p crisol-ui --example todo`
//!
//! The whole of Track A end to end — reactive state, the DOM mutation API, the cascade,
//! layout, text shaping, paint, and the GPU. Nothing above the engine and nothing beside it.
//!
//! The point is what does *not* happen. A component runs once; changing state wakes an
//! effect that writes one text node or toggles one class. Adding a todo appends three nodes
//! to a list of any length and moves none of the others. The counters print to stdout on
//! every change, so the claim is checkable while the thing is running.
//!
//! ```text
//!   enter      add what you have typed
//!   f2         edit the selected todo; enter commits
//!   up/down    move the selection
//!   tab        tick the selected todo
//!   delete     remove the selected todo
//!   left/right change the filter
//!   escape     quit
//! ```

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use crisol_ui::chrome::{ResizeEdge, resize_edge};
use crisol_ui::css::stylesheet::Stylesheet;
use crisol_ui::display_list::{Color, DisplayList, Point};
use crisol_ui::dom::Dom;
use crisol_ui::events::{
    DragEvent, EventSystem, Modifiers, hit_test, pointer_at, scroll_at, scroll_from,
};
use crisol_ui::layout::{LayoutCache, LayoutContext, ShapedText};
use crisol_ui::paint::{PaintOptions, paint};
use crisol_ui::platform_cursor;
use crisol_ui::reactive::{
    Cx, Keyed, ListStats, Memo, Runtime, Scope, Signal, append, bind_class, bind_text,
    element_with_class, text,
};
use crisol_ui::render::{AcquiredFrame, FrameTarget, Renderer, WindowSurface};
use crisol_ui::style::{CursorIcon, StyleEngine, StyleMap};
use crisol_ui::text::FontSystem;
use crisol_ui::tree::{NodeId, NodeKind, Tree};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{WindowAttributes, WindowId};

const CSS: &str = "
    body {
        display: flex;
        flex-direction: column;
        height: 100%;
        padding-top: 20px; padding-left: 24px; padding-right: 24px;
        background-color: rgb(14, 16, 20);
        color: rgb(226, 232, 240);
        font-size: 15px;
        line-height: 22px;
    }
    h1 { display: block; font-size: 22px; line-height: 34px; color: rgb(97, 175, 239) }
    .draft {
        cursor: text;
        display: block; height: 30px; line-height: 30px;
        padding-left: 10px; padding-right: 10px;
        margin-bottom: 10px;
        background-color: rgb(24, 28, 34);
        border-bottom-width: 2px; border-bottom-color: rgb(97, 175, 239);
    }
    .draft.editing { border-bottom-color: rgb(229, 192, 123) }
    ul.list {
        display: flex; flex-direction: column;
        flex-grow: 1;
        /* Without this a flex item refuses to shrink below its content, so a long list
           would grow the body rather than scrolling inside it. */
        min-height: 0;
        overflow: scroll;
    }
    li.todo {
        cursor: pointer;
        display: flex; flex-direction: row;
        height: 26px; line-height: 26px;
        padding-left: 8px; padding-right: 8px;
    }
    li.selected { background-color: rgb(34, 40, 49) }
    /* The reason `apply_state` exists. Nothing in this example styled an interaction
       pseudo-class until the event system was wired up, which is how a `:hover` that never
       reached the cascade went unnoticed since M5. */
    li.todo:hover { background-color: rgb(28, 33, 41) }
    li.todo.drop-target { background-color: rgb(50, 62, 44) }
    li.done .label { color: rgb(106, 115, 125) }
    .mark { cursor: cell; display: block; width: 30px; color: rgb(152, 195, 121) }
    .label { display: block; flex-grow: 1 }
    p.status {
        display: block; height: 24px; line-height: 24px;
        margin-top: 8px;
        color: rgb(106, 115, 125); font-size: 12px;
    }
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `--headless` drives the same app through a scripted sequence and prints the counters,
    // with no window and no GPU. It exists so the interaction paths are exercised somewhere
    // that does not need a display — which is also the only way CI ever sees them.
    if std::env::args().any(|argument| argument == "--headless") {
        headless();
        return Ok(());
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(Shell::default())?;
    Ok(())
}

/// The application's native menu bar, on the platforms that have one.
///
/// Native on purpose, and the one part of this example that is *meant* to look different on
/// each platform. Everything else here is drawn by the engine so that it does not — but a
/// menu bar belongs to the desktop rather than to the document, and a hand-drawn one is
/// immediately wrong: on macOS it lives at the top of the screen rather than in the window,
/// it carries the application menu, and it answers to the system's own keyboard handling.
///
/// Linux has no entry here. muda drives GTK there, which this engine does not otherwise
/// depend on and will not acquire for a menu bar — winit speaks X11 and Wayland directly.
/// Linux applications conventionally put their menus inside the window, which crisol can
/// already draw.
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod menu {
    use muda::{Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

    /// Ids the event loop matches on. Strings because that is what muda carries.
    pub const ADD: &str = "todo.add";
    pub const CLEAR_DONE: &str = "todo.clear-done";

    /// Builds the bar. Held by the caller: dropping a `Menu` unhooks it.
    pub fn build() -> muda::Result<Menu> {
        let bar = Menu::new();

        // macOS insists the first submenu is the application menu, and puts the application
        // name on it whatever this says. Quit has to be the predefined item rather than an
        // ordinary one, or the system does not treat it as quitting.
        let app = Submenu::new("Crisol", true);
        app.append_items(&[
            &PredefinedMenuItem::about(None, None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ])?;

        let todo = Submenu::new("&Todo", true);
        todo.append_items(&[
            &MenuItem::with_id(MenuId::new(ADD), "&Add a todo", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(MenuId::new(CLEAR_DONE), "&Clear completed", true, None),
        ])?;

        bar.append_items(&[&app, &todo])?;
        Ok(bar)
    }
}

/// How far in from an edge counts as the resize band, in logical pixels.
const RESIZE_BORDER: f32 = 6.0;

/// Maps the engine's edge vocabulary to winit's.
///
/// `crisol-ui` names its own directions rather than depending on a windowing crate — nothing
/// above the renderer does (see its `Cargo.toml`). This is the rename that costs, written
/// once by the application that needed it.
fn to_winit(edge: ResizeEdge) -> winit::window::ResizeDirection {
    use winit::window::ResizeDirection as D;
    match edge {
        ResizeEdge::North => D::North,
        ResizeEdge::NorthEast => D::NorthEast,
        ResizeEdge::East => D::East,
        ResizeEdge::SouthEast => D::SouthEast,
        ResizeEdge::South => D::South,
        ResizeEdge::SouthWest => D::SouthWest,
        ResizeEdge::West => D::West,
        ResizeEdge::NorthWest => D::NorthWest,
    }
}

/// The pointer shape for an edge, which winit already knows.
fn resize_cursor(edge: ResizeEdge) -> cursor_icon::CursorIcon {
    to_winit(edge).into()
}

/// The font system, honouring the same `CRISOL_REQUIRE_FONTS` the tests use.
///
/// Without it a machine with no fonts renders an empty window and reports success, which is
/// indistinguishable from working.
fn fonts() -> FontSystem {
    let fonts = FontSystem::new();
    if fonts.is_empty() {
        assert!(
            std::env::var_os("CRISOL_REQUIRE_FONTS").is_none(),
            "CRISOL_REQUIRE_FONTS is set and no fonts were found"
        );
        eprintln!("warning: no fonts found, so nothing will be legible");
    }
    fonts
}

fn headless() {
    use crisol_ui::display_list::Size;

    let viewport = Size {
        width: 600.0,
        height: 460.0,
    };
    let mut tree = Tree::new();
    let runtime = Runtime::new();
    let app = {
        let mut dom = Dom::new(&mut tree);
        let app = build(&runtime, &mut dom);
        for seed in ["read the roadmap", "ship M7", "measure idle RSS"] {
            app.add(&runtime, seed.to_owned());
        }
        // Enough to overflow the list, so the scroll steps below have somewhere to go.
        for index in 0..24 {
            app.add(&runtime, format!("filler {index}"));
        }
        runtime.flush(&mut dom);
        app
    };

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).expect("the example's own stylesheet"));
    let mut styles = StyleMap::default();
    let mut fonts = fonts();
    let mut cache = LayoutCache::new();

    let typing = |word: &str| -> Vec<Key> {
        word.chars()
            .map(|character| Key::Character(character.to_string().into()))
            .collect()
    };

    let mut script: Vec<(&str, Vec<Key>)> = vec![
        ("type a new todo", typing("write it down")),
        ("add it", vec![Key::Named(NamedKey::Enter)]),
        ("select the second", vec![Key::Named(NamedKey::ArrowDown)]),
        ("tick it", vec![Key::Named(NamedKey::Tab)]),
        ("filter to active", vec![Key::Named(NamedKey::ArrowRight)]),
        ("filter to done", vec![Key::Named(NamedKey::ArrowRight)]),
        ("back to all", vec![Key::Named(NamedKey::ArrowRight)]),
        ("start editing", vec![Key::Named(NamedKey::F2)]),
        ("clear it", vec![Key::Named(NamedKey::Backspace); 64]),
    ];
    script.push(("retype it", typing("edited in place")));
    script.push(("commit the edit", vec![Key::Named(NamedKey::Enter)]));
    script.push(("remove it", vec![Key::Named(NamedKey::Delete)]));

    println!("\n{} nodes\n", tree.len());
    for (name, keys) in script {
        runtime.reset_stats();
        app.last.set(ListStats::default());
        let dom_stats = {
            let mut dom = Dom::new(&mut tree);
            dom.reset_stats();
            for key in keys {
                app.key(&runtime, &mut dom, key);
            }
            runtime.flush(&mut dom);
            dom.stats()
        };

        // Once, not twice: the pass consumes the style flags it acted on, so a second call
        // sees a clean tree, reuses the previous map wholesale and hands back stale styles.
        styles = engine.restyle_incremental(&mut tree, &styles).0;
        let laid_out = {
            let mut context = LayoutContext::new(&mut tree, &styles, &mut fonts, &mut cache);
            context.run(viewport);
            context.stats().nodes_laid_out
        };
        let list = app.last.get();
        println!(
            "  {name:<18} dom: {:>2} created {:>2} removed {:>2} text {:>2} attrs | \
             list: {} new {} moved {} gone {} kept | effects: {:>2} | layout: {laid_out:>3} of {}",
            dom_stats.created,
            dom_stats.removed,
            dom_stats.text_set,
            dom_stats.attributes_set,
            list.created,
            list.moved,
            list.removed,
            list.kept,
            runtime.stats().effects_run,
            tree.len(),
        );
    }

    // ---- scrolling -----------------------------------------------------------------
    //
    // The list has to have become a scroll container for any of this to mean anything,
    // which is a statement about the stylesheet above as much as about the engine.
    assert!(
        tree.is_scrollable(app.list),
        "the list should overflow with {} rows in it",
        runtime
            .peek(app.todos)
            .map(|todos| todos.len())
            .unwrap_or(0)
    );
    let reach = tree.scroll_max(app.list).height;

    let scrolled = scroll_from(&mut tree, app.list, Point::new(0.0, 60.0)).expect("moved");
    assert_eq!(scrolled.applied, Point::new(0.0, 60.0));
    assert_eq!(tree.scroll_offset(app.list).y, 60.0);
    println!("  scrolled 60 of {reach:.0} available");

    // Past the end takes only what is left, which is what a caller hands outward.
    let rest = scroll_from(&mut tree, app.list, Point::new(0.0, 10_000.0)).expect("moved");
    assert_eq!(rest.applied.y, reach - 60.0);
    assert_eq!(tree.scroll_offset(app.list).y, reach);
    assert!(scroll_from(&mut tree, app.list, Point::new(0.0, 1.0)).is_none());

    // Nothing about scrolling is allowed to move a box.
    let before = tree.get(app.list).map(|node| node.layout);
    let laid_out = {
        let mut context = LayoutContext::new(&mut tree, &styles, &mut fonts, &mut cache);
        context.run(viewport);
        context.stats().nodes_laid_out
    };
    assert_eq!(tree.get(app.list).map(|node| node.layout), before);
    println!("  a frame after scrolling laid out {laid_out} nodes");
    assert_eq!(
        laid_out, 0,
        "scrolling marks paint, never layout: a fling must not relayout the document"
    );

    // ---- the pointer shape -----------------------------------------------------------
    //
    // `li.todo { cursor: pointer }` has to reach the label inside the row. If it did not,
    // the pointer would flicker to an arrow as it crossed each piece of text, which is the
    // whole reason CSS makes the property inherited.
    // Back to the top first: the scroll steps above left the list at its end, so the first
    // row is clipped out of view and a hit test correctly misses it.
    tree.set_scroll(app.list, Point::ZERO);
    let row = tree.first_child(app.list).expect("a row");
    let label = tree
        .first_child(row)
        .and_then(|m| tree.next_sibling(m))
        .expect("the label");
    let text = tree.first_child(label).expect("the label's text");
    let box_of = tree.absolute_rect(text).expect("laid out");
    let at = Point::new(box_of.min_x() + 1.0, box_of.min_y() + box_of.height() * 0.5);

    let hit = hit_test(&tree, at).expect("something under the pointer");
    assert_eq!(hit.node, text, "the point should land on the label's text");
    let over_text = tree
        .get(hit.node)
        .is_some_and(|node| matches!(node.kind, NodeKind::Text(_)));
    assert!(over_text);
    let icon = styles
        .get(hit.node)
        .expect("styled")
        .cursor
        .resolve(over_text);
    assert_eq!(
        icon,
        CursorIcon::Pointer,
        "an explicit keyword beats `auto`'s I-beam even over text"
    );
    assert_eq!(
        platform_cursor(icon),
        Some(cursor_icon::CursorIcon::Pointer)
    );
    println!("  pointer over a row label: {icon:?}");

    // ---- dispatch: what a pointer leaves behind, and what a drag must not ------------
    //
    // `EventSystem` is the layer an application drives input through, and until now nothing
    // outside its own unit tests did. Exercising it here puts the interaction-state pipeline
    // on all three platforms in CI: `:hover` is matched by the cascade against bits that
    // `apply_state` writes, so a break in it is invisible until something reads them.
    //
    // `ElementState` is qualified because winit exports that name too, and the two mean
    // entirely different things.
    let mut events = EventSystem::new();
    events.pointer_moved(&tree, pointer_at(at), &());
    let touched = events.apply_state(&mut tree);
    assert!(touched > 0, "the pointer should have marked its chain");
    let hovered = |tree: &Tree, node| {
        tree.element(node)
            .is_some_and(|data| data.state.contains(crisol_ui::tree::ElementState::HOVER))
    };
    assert!(hovered(&tree, label), "the label under the pointer");
    assert!(
        hovered(&tree, row),
        "and its row, because :hover is a chain"
    );

    // A drag over the very same point. It must land on the same node and must *not* leave
    // the row looking hovered: the OS owns the cursor while a drag is in flight, so a file
    // passing over a button is not a finger about to press it, and nothing would ever turn
    // that highlight off again.
    let mut events = EventSystem::new();
    let drag = DragEvent {
        position: at,
        modifiers: Modifiers::default(),
    };
    events.drag_moved(&tree, drag, &());
    events.apply_state(&mut tree);
    assert!(
        !hovered(&tree, row),
        "a drag is not a pointer, and must not leave :hover behind"
    );
    assert_eq!(
        events.drag_dropped(&tree, drag, &()),
        Some(text),
        "the drop lands on the node under it"
    );
    println!("  hover set on the row, and a drag over it left none");

    // Printed numbers are not a check. What the script should have left behind:
    let labels = runtime.peek(app.todos).expect("todos");
    let text = |todo: &Todo| runtime.peek(todo.label).unwrap_or_default();
    // The selection is an index into the *filtered* list, so the three filter steps leave it
    // on the first visible row rather than where it started. That row is what gets edited and
    // then removed: "read the roadmap".
    let written: Vec<_> = labels.iter().map(text).collect();
    assert_eq!(
        [
            written.first().map(String::as_str),
            written.get(1).map(String::as_str),
            written.last().map(String::as_str),
        ],
        [
            Some("ship M7"),
            Some("measure idle RSS"),
            Some("write it down")
        ],
        "one todo added at the end, the first edited, and the edited one removed"
    );
    assert_eq!(
        written.len(),
        27,
        "3 seeds + 24 fillers + 1 added - 1 removed"
    );
    assert_eq!(
        runtime.peek(app.filter),
        Some(Filter::All),
        "three filter steps return to where they started"
    );
    assert_eq!(
        runtime.peek(app.editing).flatten(),
        None,
        "the edit committed"
    );
    assert_eq!(
        runtime.peek(app.draft).as_deref(),
        Some(""),
        "and the draft was cleared"
    );

    println!("  checked: 27 todos, filter all, edit committed, draft empty");

    // ---- the menu's two actions ------------------------------------------------------
    //
    // The bar itself is macOS/Windows only and needs a window, so it cannot be driven from
    // here. The pair of actions behind it can be, which is why they live on `App` rather
    // than on the window's `State` — otherwise they would be the one interaction path in
    // this example that CI never executes on any platform.
    //
    // Counts are taken from the list rather than written down, so that editing the script
    // above cannot quietly turn these into assertions about the wrong thing.
    {
        let mut dom = Dom::new(&mut tree);
        let label_of = |todo: &Todo| runtime.peek(todo.label).unwrap_or_default();
        let base = runtime.peek(app.todos).expect("todos").len();

        // An empty draft still adds something.
        assert_eq!(
            runtime.peek(app.draft).as_deref(),
            Some(""),
            "the script left the draft empty"
        );
        app.add_draft(&runtime, &mut dom);
        let added = runtime.peek(app.todos).expect("todos");
        assert_eq!(added.len(), base + 1, "an empty draft adds the placeholder");
        assert_eq!(
            label_of(added.last().expect("the new row")),
            "a new todo",
            "and the placeholder is what `add` would otherwise have dropped"
        );

        // A draft with something in it is used verbatim, and is cleared behind itself so
        // that the next invocation does not repeat it.
        runtime.set(app.draft, "from the menu".to_owned());
        app.add_draft(&runtime, &mut dom);
        let added = runtime.peek(app.todos).expect("todos");
        assert_eq!(added.len(), base + 2);
        assert_eq!(
            label_of(added.last().expect("the new row")),
            "from the menu"
        );
        assert_eq!(
            runtime.peek(app.draft).as_deref(),
            Some(""),
            "the draft is cleared, or the next add repeats it"
        );

        // Clearing takes every completed row and leaves the rest. How many the script
        // already left ticked is deliberately not assumed.
        for todo in added.iter().take(2) {
            runtime.set(todo.done, true);
        }
        runtime.flush(&mut dom);
        let before = runtime.peek(app.todos).expect("todos");
        let done = before
            .iter()
            .filter(|todo| runtime.peek(todo.done).unwrap_or_default())
            .count();
        assert!(done >= 2, "two rows were just ticked");
        app.clear_done(&runtime, &mut dom);
        let left = runtime.peek(app.todos).expect("todos");
        assert_eq!(
            left.len(),
            before.len() - done,
            "every completed row went, and only those"
        );
        assert!(
            left.iter()
                .all(|todo| !runtime.peek(todo.done).unwrap_or_default()),
            "nothing completed is left behind"
        );
        println!("  menu: added a placeholder and a draft, then cleared {done} completed");
    }

    // The acceptance figure, taken here rather than sampled from outside.
    //
    // This point in the run is a stated one: the whole scripted sequence has happened, the
    // tree is built, styled and laid out, and nothing is in flight. A sampler outside the
    // process cannot name a moment like that, which is how the earlier readings ended up
    // describing a process that had not drawn anything yet.
    //
    // CI runs this on macOS, Linux and Windows, so the three numbers are comparable runs of
    // the same script — but not the same quantity: see `Metric`.
    #[cfg(feature = "measure")]
    {
        // The profile is part of the reading, not a footnote. A debug build carries overflow
        // checks and unoptimised layout, and D-47's table was taken in release — quoting one
        // beside the other without saying which is how a memory claim stops meaning anything.
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        match crisol_ui::measure::current() {
            Some(reading) => println!("  memory after the scripted run: {reading}, {profile}"),
            None => println!("  memory after the scripted run: unmeasured on this platform"),
        }
    }
    println!();
}

// ---- the app ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Todo {
    id: u32,
    label: Signal<String>,
    done: Signal<bool>,
    /// Owns `label` and `done`. Separate from the row's scope, because the data outlives a
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

impl Filter {
    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Active => "active",
            Self::Done => "done",
        }
    }

    fn shifted(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::All, true) | (Self::Done, false) => Self::Active,
            (Self::Active, true) | (Self::All, false) => Self::Done,
            (Self::Done, true) | (Self::Active, false) => Self::All,
        }
    }
}

struct App {
    /// The `<ul>` the rows live in, so a headless run can scroll it without a pointer.
    list: NodeId,
    todos: Signal<Vec<Todo>>,
    filter: Signal<Filter>,
    visible: Memo<Vec<Todo>>,
    /// Index into `visible`, not into `todos`: the selection follows what is on screen.
    selected: Signal<usize>,
    draft: Signal<String>,
    /// The todo being edited, if any. `f2` starts it; enter commits.
    editing: Signal<Option<Todo>>,
    next_id: Cell<u32>,
    last: Rc<Cell<ListStats>>,
}

fn build(runtime: &Runtime, dom: &mut Dom<'_>) -> App {
    let root = dom.create_element("html");
    dom.set_root(root);
    let body = dom.create_element("body");
    dom.append_child(root, body).unwrap();

    let todos = runtime.signal(Vec::<Todo>::new());
    let filter = runtime.signal(Filter::All);
    let selected = runtime.signal(0_usize);
    let draft = runtime.signal(String::new());
    let editing = runtime.signal(None::<Todo>);

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

        let title = element_with_class(&mut cx, "h1", "title");
        append(&mut cx, body, title);
        let title_text = text(&mut cx, "todos");
        append(&mut cx, title, title_text);

        let draft_row = element_with_class(&mut cx, "div", "draft");
        append(&mut cx, body, draft_row);
        let draft_text = text(&mut cx, "");
        append(&mut cx, draft_row, draft_text);
        bind_text(&mut cx, draft_text, move |track| {
            let typed = track.get(draft);
            match track.get(editing) {
                Some(_) => format!("editing: {typed}_"),
                None => format!("> {typed}_"),
            }
        });
        bind_class(&mut cx, draft_row, "editing", move |track| {
            track.get(editing).is_some()
        });

        list = element_with_class(&mut cx, "ul", "list");
        append(&mut cx, body, list);

        let status = element_with_class(&mut cx, "p", "status");
        append(&mut cx, body, status);
        let status_text = text(&mut cx, "");
        append(&mut cx, status, status_text);
        bind_text(&mut cx, status_text, move |track| {
            let left = track
                .with(todos, |todos| {
                    todos.iter().filter(|todo| !track.get(todo.done)).count()
                })
                .unwrap_or_default();
            let filter = track.get(filter);
            format!(
                "{left} left  ·  filter: {}  ·  enter add   f2 edit   tab tick   del remove   \
                 arrows select/filter   esc quit",
                filter.name()
            )
        });
    }

    let last = Rc::new(Cell::new(ListStats::default()));
    let recorder = Rc::clone(&last);
    let mut keyed = Keyed::new(list);
    runtime.effect(dom, move |cx| {
        let rows = cx.memo(visible);
        let stats = keyed.reconcile(cx, &rows, |cx, todo| row(cx, *todo, visible, selected));
        recorder.set(stats);
    });

    App {
        list,
        todos,
        filter,
        visible,
        selected,
        draft,
        editing,
        next_id: Cell::new(0),
        last,
    }
}

/// One row. Runs once per todo; nothing re-runs it.
fn row(
    cx: &mut Cx<'_, '_>,
    todo: Todo,
    visible: Memo<Vec<Todo>>,
    selected: Signal<usize>,
) -> NodeId {
    let node = element_with_class(cx, "li", "todo");

    let mark = element_with_class(cx, "span", "mark");
    append(cx, node, mark);
    let mark_text = text(cx, "");
    append(cx, mark, mark_text);
    bind_text(cx, mark_text, move |track| {
        if track.get(todo.done) { "[x]" } else { "[ ]" }.to_owned()
    });

    let label = element_with_class(cx, "span", "label");
    append(cx, node, label);
    let label_text = text(cx, "");
    append(cx, label, label_text);
    bind_text(cx, label_text, move |track| track.get(todo.label));

    bind_class(cx, node, "done", move |track| track.get(todo.done));

    // Reads `visible` so the highlight follows the filtered order rather than the raw list.
    //
    // **This binding is O(rows) per selection change**, and deliberately left that way. Every
    // row subscribes to `selected`, so an arrow key wakes all of them — one to gain the class
    // and one to lose it, and the rest to write nothing. The DOM layer absorbs the writes, so
    // the cost is closure calls rather than relayouts, but it is still linear.
    //
    // That is not a limit of the engine; it is what asking "am *I* the selected one?" once
    // per row costs. A list long enough to care would keep the previously selected row and
    // toggle exactly two, which is bookkeeping this example is clearer without.
    bind_class(cx, node, "selected", move |track| {
        let index = track.get(selected);
        track
            .with_memo(visible, |rows| rows.get(index) == Some(&todo))
            .unwrap_or(false)
    });
    node
}

impl App {
    fn add(&self, runtime: &Runtime, label: String) {
        if label.trim().is_empty() {
            return;
        }
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let (scope, (label, done)) =
            runtime.scope(|_| (runtime.signal(label), runtime.signal(false)));
        runtime.update(self.todos, |todos| {
            todos.push(Todo {
                id,
                label,
                done,
                scope,
            });
        });
    }

    fn selected_todo(&self, runtime: &Runtime, dom: &mut Dom<'_>) -> Option<Todo> {
        let index = runtime.peek(self.selected)?;
        let cx = Cx::new(runtime, dom);
        cx.with_memo(self.visible, |rows| rows.get(index).copied())?
    }

    fn remove(&self, runtime: &Runtime, dom: &mut Dom<'_>, todo: Todo) {
        runtime.update(self.todos, |todos| {
            todos.retain(|other| other.id != todo.id);
        });
        // Flush before disposing: the row's effects read `label` and `done`, and the
        // reconciler disposes them during the flush. Freeing the signals first would leave
        // those effects reading handles whose slots had been reused.
        runtime.flush(dom);
        runtime.dispose(todo.scope, dom);
        self.clamp_selection(runtime, dom);
    }

    /// Adds whatever is in the draft, or a placeholder when the draft is empty.
    ///
    /// The placeholder is the point: `add` ignores an empty label, so without it the menu
    /// item would do nothing whenever the draft happened to be empty, which reads as broken
    /// rather than as deliberate.
    fn add_draft(&self, runtime: &Runtime, dom: &mut Dom<'_>) {
        let draft = runtime.peek(self.draft).unwrap_or_default();
        let label = if draft.trim().is_empty() {
            "a new todo".to_owned()
        } else {
            draft
        };
        self.add(runtime, label);
        runtime.set(self.draft, String::new());
        runtime.flush(dom);
    }

    /// Removes every completed todo.
    fn clear_done(&self, runtime: &Runtime, dom: &mut Dom<'_>) {
        let todos = runtime.peek(self.todos).unwrap_or_default();
        let done: Vec<_> = todos
            .into_iter()
            .filter(|todo| runtime.peek(todo.done).unwrap_or_default())
            .collect();
        for todo in done {
            self.remove(runtime, dom, todo);
        }
    }

    fn clamp_selection(&self, runtime: &Runtime, dom: &mut Dom<'_>) {
        let cx = Cx::new(runtime, dom);
        let count = cx.with_memo(self.visible, Vec::len).unwrap_or(0);
        let index = runtime.peek(self.selected).unwrap_or(0);
        runtime.set_if_changed(self.selected, index.min(count.saturating_sub(1)));
    }

    fn move_selection(&self, runtime: &Runtime, dom: &mut Dom<'_>, down: bool) {
        let cx = Cx::new(runtime, dom);
        let count = cx.with_memo(self.visible, Vec::len).unwrap_or(0);
        if count == 0 {
            return;
        }
        let index = runtime.peek(self.selected).unwrap_or(0);
        let next = if down {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        runtime.set_if_changed(self.selected, next);
    }

    /// Applies one key. Returns whether anything might have changed.
    fn key(&self, runtime: &Runtime, dom: &mut Dom<'_>, key: Key) -> bool {
        match key.as_ref() {
            Key::Named(NamedKey::Enter) => {
                let typed = runtime.peek(self.draft).unwrap_or_default();
                match runtime.peek(self.editing).flatten() {
                    // Committing an edit writes one signal, which wakes one effect, which
                    // writes one text node. No row is rebuilt.
                    Some(todo) if !typed.trim().is_empty() => {
                        runtime.set(todo.label, typed);
                        runtime.set(self.editing, None);
                    }
                    Some(_) => {
                        runtime.set(self.editing, None);
                    }
                    None => self.add(runtime, typed),
                }
                runtime.set(self.draft, String::new());
            }
            Key::Named(NamedKey::Backspace) => {
                runtime.update(self.draft, |draft| {
                    draft.pop();
                });
            }
            Key::Named(NamedKey::F2) => {
                let Some(todo) = self.selected_todo(runtime, dom) else {
                    return false;
                };
                runtime.set(self.draft, runtime.peek(todo.label).unwrap_or_default());
                runtime.set(self.editing, Some(todo));
            }
            Key::Named(NamedKey::Tab) => {
                let Some(todo) = self.selected_todo(runtime, dom) else {
                    return false;
                };
                let done = runtime.peek(todo.done).unwrap_or_default();
                runtime.set(todo.done, !done);
                self.clamp_selection(runtime, dom);
            }
            Key::Named(NamedKey::Delete) => {
                let Some(todo) = self.selected_todo(runtime, dom) else {
                    return false;
                };
                self.remove(runtime, dom, todo);
            }
            Key::Named(NamedKey::ArrowDown) => self.move_selection(runtime, dom, true),
            Key::Named(NamedKey::ArrowUp) => self.move_selection(runtime, dom, false),
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowLeft) => {
                let forward = matches!(key.as_ref(), Key::Named(NamedKey::ArrowRight));
                let next = runtime
                    .peek(self.filter)
                    .unwrap_or(Filter::All)
                    .shifted(forward);
                runtime.set(self.filter, next);
                self.clamp_selection(runtime, dom);
            }
            // No arm for space: it arrives as `Key::Character(" ")` now, and the arm
            // below already appends it.
            Key::Character(typed) => {
                runtime.update(self.draft, |draft| draft.push_str(typed));
            }
            _ => return false,
        }
        true
    }
}

// ---- the frame ----------------------------------------------------------------------------

struct State {
    pointer: Point,
    /// Input goes through this rather than around it, which is what makes `:hover` work and
    /// what a drop is routed by.
    events: EventSystem,
    /// The last position a drag reported. `DragDropped` carries none of its own, so the drop
    /// has to be told where it happened, and this is the only thing that knows.
    drag: Option<Point>,
    /// What the window is currently showing, so a mouse move that changes nothing does not
    /// talk to the window server sixty times a second.
    cursor: Option<cursor_icon::CursorIcon>,
    surface: WindowSurface,
    renderer: Renderer,
    tree: Tree,
    runtime: Runtime,
    app: App,
    engine: StyleEngine,
    styles: StyleMap,
    fonts: FontSystem,
    cache: LayoutCache,
}

impl State {
    fn new(surface: WindowSurface) -> Self {
        let renderer = Renderer::new(surface.gpu(), surface.format());
        let mut tree = Tree::new();
        let runtime = Runtime::new();
        let app = {
            let mut dom = Dom::new(&mut tree);
            let app = build(&runtime, &mut dom);
            for seed in ["read the roadmap", "ship M7", "measure idle RSS"] {
                app.add(&runtime, seed.to_owned());
            }
            runtime.flush(&mut dom);
            app
        };

        let mut engine = StyleEngine::new();
        engine.add_stylesheet(Stylesheet::parse(CSS).expect("the example's own stylesheet"));

        let fonts = fonts();

        Self {
            pointer: Point::ZERO,
            events: EventSystem::new(),
            drag: None,
            cursor: Some(cursor_icon::CursorIcon::Default),
            surface,
            renderer,
            tree,
            runtime,
            app,
            engine,
            styles: StyleMap::default(),
            fonts,
            cache: LayoutCache::new(),
        }
    }

    fn key(&mut self, key: Key) -> bool {
        let mut dom = Dom::new(&mut self.tree);
        dom.reset_stats();
        self.runtime.reset_stats();
        self.app.last.set(ListStats::default());

        if !self.app.key(&self.runtime, &mut dom, key) {
            return false;
        }
        self.runtime.flush(&mut dom);

        let dom_stats = dom.stats();
        let list = self.app.last.get();
        println!(
            "dom: {} created, {} removed, {} text, {} attrs ({} no-ops)  |  \
             list: {} new, {} moved, {} gone, {} kept  |  effects: {}",
            dom_stats.created,
            dom_stats.removed,
            dom_stats.text_set,
            dom_stats.attributes_set,
            dom_stats.no_ops,
            list.created,
            list.moved,
            list.removed,
            list.kept,
            self.runtime.stats().effects_run,
        );
        true
    }

    /// Points the mouse cursor at whatever is under it.
    ///
    /// `cursor` is inherited, so the value on the text inside a row is the row's — without
    /// that the pointer would flicker back to an arrow as it crossed each label.
    /// Routes the pointer through the event system so `:hover` is matched against something
    /// true. Returns whether anything changed, which is whether a frame is worth drawing.
    fn hover(&mut self) -> bool {
        self.events
            .pointer_moved(&self.tree, pointer_at(self.pointer), &());
        self.events.apply_state(&mut self.tree) > 0
    }

    /// Moves a drag to a position reported by the platform, in physical pixels.
    fn drag_to(&mut self, position: winit::dpi::PhysicalPosition<f64>) {
        let scale = self.surface.scale_factor();
        let at = Point::new(position.x as f32 / scale, position.y as f32 / scale);
        self.drag = Some(at);
        let drag = DragEvent {
            position: at,
            modifiers: Modifiers::default(),
        };
        let over = self.events.drag_moved(&self.tree, drag, &());
        self.mark_drop_target(over);
    }

    /// Drops on whatever the drag was last over.
    ///
    /// The payload is not here. winit hands dragged data over asynchronously by
    /// `DataTransferId`, so an application that wants the file asks for it separately; this
    /// example only needs to show that the drop found the right row.
    fn drop_it(&mut self) {
        let Some(at) = self.drag.take() else {
            return;
        };
        let drag = DragEvent {
            position: at,
            modifiers: Modifiers::default(),
        };
        if let Some(node) = self.events.drag_dropped(&self.tree, drag, &()) {
            // Selecting the row is the visible proof that the drop landed on a particular
            // one rather than on the window. `list`'s children are the visible rows in
            // order, which is exactly what `selected` indexes.
            if let Some(row) = self.row_of(node)
                && let Some(index) = self.tree.children(self.app.list).position(|id| id == row)
            {
                self.runtime.set(self.app.selected, index);
                let mut dom = Dom::new(&mut self.tree);
                self.runtime.flush(&mut dom);
            }
        }
        self.mark_drop_target(None);
    }

    /// Ends a drag that left without dropping.
    fn drag_cancelled(&mut self) {
        self.drag = None;
        self.events.drag_left(DragEvent {
            position: Point::ZERO,
            modifiers: Modifiers::default(),
        });
        self.mark_drop_target(None);
    }

    /// Puts `drop-target` on the row under the drag and takes it off every other.
    fn mark_drop_target(&mut self, under: Option<NodeId>) {
        let wanted = under.and_then(|node| self.row_of(node));
        let rows: Vec<_> = self.tree.children(self.app.list).collect();
        let mut dom = Dom::new(&mut self.tree);
        for row in rows {
            if Some(row) == wanted {
                dom.add_class(row, "drop-target");
            } else {
                dom.remove_class(row, "drop-target");
            }
        }
    }

    /// The `li.todo` at or above `node`, if there is one.
    fn row_of(&self, node: NodeId) -> Option<NodeId> {
        let mut at = Some(node);
        while let Some(id) = at {
            if self.tree.parent(id) == Some(self.app.list) {
                return Some(id);
            }
            at = self.tree.parent(id);
        }
        None
    }

    /// Handles a press that might be on the window's own chrome rather than its content.
    ///
    /// An edge starts a resize, the title starts a move, and anything else is left alone for
    /// the application to treat as a click. Order matters: the edge band overlaps the title
    /// at the top corners, and a corner that moved the window instead of resizing it would
    /// be the more surprising of the two.
    fn press(&mut self, at: Point) {
        if let Some(edge) = resize_edge(at, self.surface.logical_size(), RESIZE_BORDER) {
            let _ = self.surface.window().drag_resize_window(to_winit(edge));
            return;
        }
        if self.over_title(at) {
            let _ = self.surface.window().drag_window();
        }
    }

    /// Whether `at` is over the heading, which is this application's title bar.
    ///
    /// Which element counts is the application's to decide — the engine knows where the
    /// boxes are, not which of them the user thinks of as somewhere to grab the window by.
    fn over_title(&self, at: Point) -> bool {
        let Some(hit) = hit_test(&self.tree, at) else {
            return false;
        };
        let mut node = Some(hit.node);
        while let Some(id) = node {
            if self.tree.node(id).kind.tag() == Some("h1") {
                return true;
            }
            node = self.tree.parent(id);
        }
        false
    }

    /// Adds whatever is in the draft, or a placeholder when it is empty.
    ///
    /// Reached from the menu rather than the keyboard, which is the whole point of having
    /// one: the same application state, a second way in. The work is `App`'s so that the
    /// headless run can exercise it, since the menu itself cannot be driven without a window.
    ///
    /// Gated with its caller: where there is no menu bar there is nothing to call this, and
    /// the example is built with `-D warnings`.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn menu_add(&mut self) {
        let mut dom = Dom::new(&mut self.tree);
        self.app.add_draft(&self.runtime, &mut dom);
    }

    /// Removes every completed todo.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn menu_clear_done(&mut self) {
        let mut dom = Dom::new(&mut self.tree);
        self.app.clear_done(&self.runtime, &mut dom);
    }

    fn update_cursor(&mut self) {
        // An edge wins over whatever the cascade says, because the resize is what a press
        // there would actually do, and a cursor that disagrees with that is a lie.
        if let Some(edge) = resize_edge(self.pointer, self.surface.logical_size(), RESIZE_BORDER) {
            let icon = resize_cursor(edge);
            if self.cursor != Some(icon) {
                self.cursor = Some(icon);
                self.surface.window().set_cursor(icon.into());
            }
            return;
        }
        let icon = hit_test(&self.tree, self.pointer)
            .and_then(|hit| {
                let over_text = self
                    .tree
                    .get(hit.node)
                    .is_some_and(|node| matches!(node.kind, NodeKind::Text(_)));
                self.styles
                    .get(hit.node)
                    .map(|style| style.cursor.resolve(over_text))
            })
            .unwrap_or(CursorIcon::Default);

        let shape = platform_cursor(icon);
        if shape == self.cursor {
            return;
        }
        self.cursor = shape;
        match shape {
            Some(shape) => {
                self.surface.window().set_cursor(shape.into());
                self.surface.window().set_cursor_visible(true);
            }
            // `cursor: none` is not a shape to fall back from — the pointer goes away.
            None => self.surface.window().set_cursor_visible(false),
        }
    }

    /// Applies a scroll gesture at the pointer. Returns whether anything moved.
    fn scroll(&mut self, delta: Point) -> bool {
        let Some(scrolled) = scroll_at(&mut self.tree, self.pointer, delta) else {
            return false;
        };
        println!(
            "scroll: {:.0},{:.0} on {:?}  (repaint {:.0}x{:.0})",
            scrolled.applied.x,
            scrolled.applied.y,
            scrolled.node,
            scrolled.damage.width(),
            scrolled.damage.height(),
        );
        true
    }

    fn draw(&mut self) {
        let AcquiredFrame::Frame(frame) = self.surface.acquire() else {
            return;
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let viewport = self.surface.logical_size();

        // Anything queued by a key press has already been flushed; this catches work an
        // effect queued on the way out.
        {
            let mut dom = Dom::new(&mut self.tree);
            self.runtime.flush(&mut dom);
        }

        let (styles, _) = self
            .engine
            .restyle_incremental(&mut self.tree, &self.styles);
        self.styles = styles;
        {
            let mut context = LayoutContext::new(
                &mut self.tree,
                &self.styles,
                &mut self.fonts,
                &mut self.cache,
            );
            context.run(viewport);
        }

        let list: DisplayList = paint(
            &self.tree,
            &PaintOptions::new(viewport).with_background(Color::rgb(0.055, 0.063, 0.078)),
        );
        self.renderer.render_text(
            FrameTarget {
                view: &view,
                width: self.surface.width(),
                height: self.surface.height(),
                scale_factor: self.surface.scale_factor(),
                damage: None,
            },
            &list,
            &mut self.fonts,
            &ShapedText(self.cache.text()),
        );
        self.surface.present(frame);
    }
}

#[derive(Default)]
struct Shell {
    state: Option<State>,
    /// Held, not just built. Dropping a `Menu` takes it back off the application.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    menu: Option<muda::Menu>,
}

impl ApplicationHandler for Shell {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let window = Arc::from(
            event_loop
                .create_window(
                    WindowAttributes::default()
                        .with_title("Crisol — M7: signals, effects, components")
                        // M8's window chrome: no system title bar, so what the window shows
                        // is what the engine drew, the same on all three platforms. The cost
                        // is that moving and resizing become the application's problem —
                        // handled in `PointerButton` below.
                        .with_decorations(false)
                        .with_surface_size(winit::dpi::LogicalSize::new(600.0, 460.0)),
                )
                .expect("could not create a window"),
        );
        let surface = WindowSurface::new(window).expect("could not create a surface");

        // After the window, deliberately. On macOS the menu attaches to the `NSApplication`,
        // which winit has only finished creating by the time this callback runs; on Windows
        // it attaches to the window itself and so needs one to exist.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            match menu::build() {
                Ok(bar) => {
                    #[cfg(target_os = "macos")]
                    bar.init_for_nsapp();
                    #[cfg(target_os = "windows")]
                    {
                        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        if let Ok(handle) = surface.window().window_handle()
                            && let RawWindowHandle::Win32(win32) = handle.as_raw()
                        {
                            // SAFETY: the handle comes from the window created just above,
                            // which `State` owns for as long as the application runs. It
                            // therefore outlives both this call and the menu hung on it,
                            // which is what `init_for_hwnd` requires.
                            let _ = unsafe { bar.init_for_hwnd(win32.hwnd.get()) };
                        }
                    }
                    self.menu = Some(bar);
                }
                // A menu bar that will not build is not a reason to refuse to start. The
                // application is entirely usable from the keyboard without it.
                Err(error) => eprintln!("warning: no native menu ({error})"),
            }
        }

        self.state = Some(State::new(surface));
    }

    /// Menu clicks do not arrive as window events — muda posts them to its own channel, so
    /// they are drained here, once per turn of the loop.
    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            let Some(state) = self.state.as_mut() else {
                continue;
            };
            match event.id().as_ref() {
                menu::ADD => state.menu_add(),
                menu::CLEAR_DONE => state.menu_clear_done(),
                _ => continue,
            }
            state.surface.window().request_redraw();
        }
        let _ = event_loop;
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::SurfaceResized(size) => {
                state.surface.resize(size.width, size.height);
                // The viewport changed, so every box has to be measured against it again.
                state.tree.mark_subtree_dirty(
                    state.tree.root().expect("a root"),
                    crisol_ui::tree::DirtyFlags::LAYOUT,
                );
                state.surface.window().request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                state.surface.refresh();
                state.surface.window().request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if matches!(event.logical_key.as_ref(), Key::Named(NamedKey::Escape)) {
                    event_loop.exit();
                    return;
                }
                if state.key(event.logical_key) {
                    state.surface.window().request_redraw();
                }
            }
            WindowEvent::PointerMoved { position, .. } => {
                let scale = state.surface.scale_factor();
                state.pointer = Point::new(position.x as f32 / scale, position.y as f32 / scale);
                state.update_cursor();
                if state.hover() {
                    state.surface.window().request_redraw();
                }
            }
            // ---- drag and drop ---------------------------------------------------------
            //
            // winit 0.31 reports a drag's position (D-51); 0.30 did not, which is why this
            // could not be written before. `DragEntered` may or may not carry one depending
            // on the platform, `DragPosition` always does, and `DragDropped` never does —
            // so the drop is told where it happened from whatever the last two recorded.
            WindowEvent::DragEntered {
                position: Some(position),
                ..
            } => {
                state.drag_to(position);
                state.surface.window().request_redraw();
            }
            WindowEvent::DragPosition { position, .. } => {
                state.drag_to(position);
                state.surface.window().request_redraw();
            }
            WindowEvent::DragDropped { .. } => {
                state.drop_it();
                state.surface.window().request_redraw();
            }
            WindowEvent::DragLeft { .. } => {
                state.drag_cancelled();
                state.surface.window().request_redraw();
            }
            WindowEvent::PointerButton {
                state: ElementState::Pressed,
                position,
                ..
            } => {
                let scale = state.surface.scale_factor();
                let at = Point::new(position.x as f32 / scale, position.y as f32 / scale);
                state.press(at);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // A trackpad on macOS has already been through the system's own momentum by
                // the time this arrives, which is why the engine's `Fling` is for drags the
                // application tracked itself and must not be layered on top of these.
                let delta = match delta {
                    MouseScrollDelta::LineDelta(x, y) => Point::new(x * -40.0, y * -40.0),
                    MouseScrollDelta::PixelDelta(position) => {
                        Point::new(-position.x as f32, -position.y as f32)
                    }
                    // `MouseScrollDelta` is `#[non_exhaustive]` as of winit 0.31. A kind this
                    // build does not know how to read is not a scroll of zero — it is a scroll
                    // of unknown size, and inventing a distance for it would be worse than
                    // ignoring it.
                    _ => return,
                };
                if state.scroll(delta) {
                    state.surface.window().request_redraw();
                }
            }
            WindowEvent::RedrawRequested => state.draw(),
            _ => {}
        }
    }
}
