//! A drag crossing the tree: who is told, in what order, and what it does not disturb.

use std::cell::RefCell;
use std::rc::Rc;

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::{Point, Size};
use crisol_events::{DragEvent, Event, EventSystem, Modifiers, When};
use crisol_layout::layout;
use crisol_style::StyleEngine;
use crisol_text::FontSystem;
use crisol_tree::{ElementState, NodeId, Tree};

const VIEWPORT: Size = Size {
    width: 200.0,
    height: 100.0,
};

/// `.outer` fills the left half, `.inner` sits in its top-left corner.
fn page() -> (Tree, NodeId, NodeId) {
    let mut document =
        crisol_html::parse("<body><div class=outer><div class=inner></div></div></body>");
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(
        Stylesheet::parse(
            ".outer { width: 50px; height: 50px } .inner { width: 20px; height: 20px }",
        )
        .unwrap(),
    );
    let (styles, _) = engine.restyle(&document.tree);
    layout(
        &mut document.tree,
        &styles,
        &mut FontSystem::empty(),
        VIEWPORT,
    );
    let find = |class: &str| {
        let mut stack = vec![document.tree.root().unwrap()];
        while let Some(id) = stack.pop() {
            if document
                .tree
                .element(id)
                .is_some_and(|data| data.has_class(class, true))
            {
                return id;
            }
            for child in document.tree.children(id) {
                stack.push(child);
            }
        }
        panic!("no .{class}");
    };
    let (outer, inner) = (find("outer"), find("inner"));
    (document.tree, outer, inner)
}

fn at(x: f32, y: f32) -> DragEvent {
    DragEvent {
        position: Point::new(x, y),
        modifiers: Modifiers::default(),
    }
}

/// Records the event name and node of everything it is offered.
type Log = Rc<RefCell<Vec<(&'static str, NodeId)>>>;

fn recorder(log: &Log) -> impl FnMut(&mut crisol_events::Dispatch<'_>) + use<> {
    let log = Rc::clone(log);
    move |dispatch| {
        let name = match dispatch.event {
            Event::DragEnter(_) => "enter",
            Event::DragOver(_) => "over",
            Event::DragLeave(_) => "leave",
            Event::Drop(_) => "drop",
            _ => "other",
        };
        log.borrow_mut().push((name, dispatch.current));
    }
}

#[test]
fn entering_tells_each_node_once_shallowest_first() {
    let (tree, outer, inner) = page();
    let log: Log = Log::default();
    let mut events = EventSystem::new();
    for node in [outer, inner] {
        events.add_listener(node, When::Bubble, recorder(&log));
    }

    events.drag_moved(&tree, at(5.0, 5.0), &());
    // A second move inside the same box must not re-enter anything.
    events.drag_moved(&tree, at(9.0, 9.0), &());

    let entered: Vec<_> = log
        .borrow()
        .iter()
        .filter(|(name, _)| *name == "enter")
        .map(|(_, node)| *node)
        .collect();
    assert_eq!(entered, vec![outer, inner], "shallowest first, once each");
}

#[test]
fn leaving_tells_the_child_before_the_parent() {
    let (tree, outer, inner) = page();
    let log: Log = Log::default();
    let mut events = EventSystem::new();
    for node in [outer, inner] {
        events.add_listener(node, When::Bubble, recorder(&log));
    }

    events.drag_moved(&tree, at(5.0, 5.0), &());
    log.borrow_mut().clear();
    events.drag_moved(&tree, at(150.0, 90.0), &());

    let left: Vec<_> = log
        .borrow()
        .iter()
        .filter(|(name, _)| *name == "leave")
        .map(|(_, node)| *node)
        .collect();
    assert_eq!(left, vec![inner, outer], "deepest first on the way out");
}

#[test]
fn moving_within_a_box_still_reports_over() {
    let (tree, _outer, inner) = page();
    let log: Log = Log::default();
    let mut events = EventSystem::new();
    events.add_listener(inner, When::Bubble, recorder(&log));

    events.drag_moved(&tree, at(5.0, 5.0), &());
    events.drag_moved(&tree, at(9.0, 9.0), &());

    let overs = log.borrow().iter().filter(|(n, _)| *n == "over").count();
    assert_eq!(
        overs, 2,
        "every move reports over, even without re-entering"
    );
}

#[test]
fn over_bubbles_but_enter_and_leave_do_not() {
    // The same distinction the pointer makes: `:hover` on an ancestor and on a child both
    // work because enter is delivered, not propagated. A drop target that highlights on
    // `over` needs the opposite, so these must not be the same.
    assert!(Event::DragOver(at(0.0, 0.0)).bubbles());
    assert!(Event::Drop(at(0.0, 0.0)).bubbles());
    assert!(!Event::DragEnter(at(0.0, 0.0)).bubbles());
    assert!(!Event::DragLeave(at(0.0, 0.0)).bubbles());
}

#[test]
fn a_drop_lands_on_the_node_under_it_and_unwinds_the_drag() {
    let (tree, outer, inner) = page();
    let log: Log = Log::default();
    let mut events = EventSystem::new();
    for node in [outer, inner] {
        events.add_listener(node, When::Bubble, recorder(&log));
    }

    events.drag_moved(&tree, at(5.0, 5.0), &());
    log.borrow_mut().clear();
    let landed = events.drag_dropped(&tree, at(5.0, 5.0), &());

    assert_eq!(landed, Some(inner), "the deepest node under the drop");
    let names: Vec<_> = log.borrow().iter().map(|(name, _)| *name).collect();
    assert!(names.contains(&"drop"), "the drop was delivered");
    assert!(
        names.contains(&"leave"),
        "and the drag was unwound, so a stale chain cannot leak into the next one"
    );

    // Nothing is left to leave.
    log.borrow_mut().clear();
    events.drag_left(at(5.0, 5.0));
    assert!(log.borrow().is_empty(), "the drop already unwound it");
}

#[test]
fn a_drop_at_an_unreported_position_still_retargets() {
    // The last motion before a release is not guaranteed to reach us, so a drop has to hit
    // test its own position rather than trust the chain the last move left behind.
    let (tree, outer, inner) = page();
    let log: Log = Log::default();
    let mut events = EventSystem::new();
    for node in [outer, inner] {
        events.add_listener(node, When::Bubble, recorder(&log));
    }

    events.drag_moved(&tree, at(40.0, 40.0), &());
    assert_eq!(
        events.drag_dropped(&tree, at(5.0, 5.0), &()),
        Some(inner),
        "the drop is where the drop says it is"
    );
}

#[test]
fn a_cancelled_drag_leaves_without_dropping() {
    let (tree, outer, inner) = page();
    let log: Log = Log::default();
    let mut events = EventSystem::new();
    for node in [outer, inner] {
        events.add_listener(node, When::Bubble, recorder(&log));
    }

    events.drag_moved(&tree, at(5.0, 5.0), &());
    log.borrow_mut().clear();
    events.drag_left(at(5.0, 5.0));

    let names: Vec<_> = log.borrow().iter().map(|(name, _)| *name).collect();
    assert!(!names.contains(&"drop"), "a cancel is not a drop");
    assert_eq!(
        log.borrow().iter().filter(|(n, _)| *n == "leave").count(),
        2,
        "both nodes were told"
    );
}

#[test]
fn a_drag_does_not_make_anything_hovered() {
    // The load-bearing separation: on every platform the OS owns the cursor for the duration
    // of a drag, so the pointer chain and the drag chain are different questions. If a drag
    // set `:hover`, a file passing over a button would light it up as though the user were
    // about to click it, and nothing would turn it off.
    let (mut tree, _outer, _inner) = page();
    let mut events = EventSystem::new();

    events.drag_moved(&tree, at(5.0, 5.0), &());
    events.apply_state(&mut tree);

    let hovered = tree
        .element(_inner_of(&tree))
        .is_some_and(|data| data.state.contains(ElementState::HOVER));
    assert!(!hovered, "a drag is not a pointer");
}

/// The `.inner` node, found again from the tree alone.
fn _inner_of(tree: &Tree) -> NodeId {
    let mut stack = vec![tree.root().unwrap()];
    while let Some(id) = stack.pop() {
        if tree
            .element(id)
            .is_some_and(|data| data.has_class("inner", true))
        {
            return id;
        }
        for child in tree.children(id) {
            stack.push(child);
        }
    }
    panic!("no .inner");
}
