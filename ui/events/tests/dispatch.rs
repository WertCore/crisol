//! Sending events through the tree, and the state selectors read.

use std::cell::RefCell;
use std::rc::Rc;

use crisol_css::stylesheet::Stylesheet;
use crisol_display_list::{Point, Size};
use crisol_events::{
    Event, EventSystem, Modifiers, Phase, PointerButton, PointerEvent, PointerId, PointerKind,
    When, hit_test, pointer_at,
};
use crisol_layout::layout;
use crisol_style::StyleEngine;
use crisol_text::FontSystem;
use crisol_tree::{ElementState, NodeId, Tree};

const VIEWPORT: Size = Size {
    width: 200.0,
    height: 100.0,
};

struct Page {
    tree: Tree,
}

impl Page {
    fn new(html: &str, css: &str) -> Self {
        let mut document = crisol_html::parse(html);
        let mut engine = StyleEngine::new();
        engine.add_stylesheet(Stylesheet::parse(css).unwrap());
        let (styles, _) = engine.restyle(&document.tree);
        layout(
            &mut document.tree,
            &styles,
            &mut FontSystem::empty(),
            VIEWPORT,
        );
        Self {
            tree: document.tree,
        }
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

    fn by_class(&self, class: &str) -> NodeId {
        let mut stack = vec![self.tree.root().unwrap()];
        while let Some(id) = stack.pop() {
            if self
                .tree
                .element(id)
                .is_some_and(|data| data.has_class(class, true))
            {
                return id;
            }
            for child in self.tree.children(id) {
                stack.push(child);
            }
        }
        panic!("no .{class}");
    }
}

/// A listener that records the nodes and phases it saw.
type Log = Rc<RefCell<Vec<(NodeId, Phase)>>>;

fn recorder(log: &Log) -> impl FnMut(&mut crisol_events::Dispatch<'_>) + use<> {
    let log = Rc::clone(log);
    move |dispatch| log.borrow_mut().push((dispatch.current, dispatch.phase))
}

fn click() -> Event {
    Event::PointerDown(pointer_at(Point::new(5.0, 5.0)))
}

// ---- propagation ------------------------------------------------------------------------

#[test]
fn an_event_captures_down_then_bubbles_up() {
    let page = Page::new(
        "<body><div class=outer><div class=inner></div></div></body>",
        ".outer { width: 50px; height: 50px } .inner { width: 20px; height: 20px }",
    );
    let mut events = EventSystem::new();
    let outcome = events.dispatch(&page.tree, page.by_class("inner"), &click());

    let names: Vec<_> = outcome
        .visited
        .iter()
        .map(|(id, phase)| {
            let tag = page.tree.node(*id).kind.tag().unwrap_or("?").to_owned();
            let class = page
                .tree
                .element(*id)
                .and_then(|data| data.classes.first().map(ToString::to_string));
            (class.unwrap_or(tag), *phase)
        })
        .collect();

    assert_eq!(
        names,
        vec![
            ("html".to_owned(), Phase::Capture),
            ("body".to_owned(), Phase::Capture),
            ("outer".to_owned(), Phase::Capture),
            ("inner".to_owned(), Phase::Target),
            ("outer".to_owned(), Phase::Bubble),
            ("body".to_owned(), Phase::Bubble),
            ("html".to_owned(), Phase::Bubble),
        ]
    );
}

#[test]
fn a_capture_listener_sees_the_event_before_the_target() {
    let page = Page::new(
        "<body><div class=inner></div></body>",
        ".inner { width: 20px; height: 20px }",
    );
    let log: Log = Rc::default();
    let mut events = EventSystem::new();
    events.add_listener(page.find("body"), When::Capture, recorder(&log));
    events.add_listener(page.by_class("inner"), When::Bubble, recorder(&log));

    events.dispatch(&page.tree, page.by_class("inner"), &click());

    let seen = log.borrow();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0].1,
        Phase::Capture,
        "the ancestor's capture listener first"
    );
    assert_eq!(seen[1].1, Phase::Target);
}

#[test]
fn a_bubble_listener_on_an_ancestor_does_not_fire_during_capture() {
    let page = Page::new(
        "<body><div class=inner></div></body>",
        ".inner { width: 20px; height: 20px }",
    );
    let log: Log = Rc::default();
    let mut events = EventSystem::new();
    events.add_listener(page.find("body"), When::Bubble, recorder(&log));

    events.dispatch(&page.tree, page.by_class("inner"), &click());

    assert_eq!(log.borrow().len(), 1);
    assert_eq!(log.borrow()[0].1, Phase::Bubble);
}

#[test]
fn stop_propagation_halts_the_journey_but_not_this_node() {
    let page = Page::new(
        "<body><div class=inner></div></body>",
        ".inner { width: 20px; height: 20px }",
    );
    let log: Log = Rc::default();
    let mut events = EventSystem::new();
    let inner = page.by_class("inner");

    events.add_listener(
        inner,
        When::Bubble,
        |dispatch: &mut crisol_events::Dispatch<'_>| {
            dispatch.stop_propagation();
        },
    );
    events.add_listener(inner, When::Bubble, recorder(&log));
    events.add_listener(page.find("body"), When::Bubble, recorder(&log));

    let outcome = events.dispatch(&page.tree, inner, &click());

    assert!(outcome.propagation_stopped);
    assert_eq!(
        log.borrow().len(),
        1,
        "the second listener on the same node still runs; the ancestor does not"
    );
}

#[test]
fn stop_immediate_propagation_halts_this_node_too() {
    let page = Page::new(
        "<body><div class=inner></div></body>",
        ".inner { width: 20px; height: 20px }",
    );
    let log: Log = Rc::default();
    let mut events = EventSystem::new();
    let inner = page.by_class("inner");

    events.add_listener(
        inner,
        When::Bubble,
        |dispatch: &mut crisol_events::Dispatch<'_>| {
            dispatch.stop_immediate_propagation();
        },
    );
    events.add_listener(inner, When::Bubble, recorder(&log));

    events.dispatch(&page.tree, inner, &click());
    assert!(log.borrow().is_empty());
}

#[test]
fn prevent_default_is_reported_to_the_caller() {
    let page = Page::new("<body></body>", "");
    let mut events = EventSystem::new();
    let body = page.find("body");
    events.add_listener(
        body,
        When::Bubble,
        |dispatch: &mut crisol_events::Dispatch<'_>| {
            dispatch.prevent_default();
        },
    );

    let outcome = events.dispatch(&page.tree, body, &click());
    assert!(
        outcome.default_prevented,
        "the engine should not do its default thing"
    );
}

#[test]
fn focus_and_blur_do_not_propagate() {
    let page = Page::new(
        "<body><div class=inner></div></body>",
        ".inner { width: 20px; height: 20px }",
    );
    let mut events = EventSystem::new();
    let outcome = events.dispatch(&page.tree, page.by_class("inner"), &Event::Focus);
    assert_eq!(
        outcome.visited.len(),
        1,
        "focus is delivered to the node it concerns and nowhere else"
    );
}

// ---- hover ------------------------------------------------------------------------------

#[test]
fn moving_the_pointer_enters_and_leaves_the_right_nodes() {
    let page = Page::new(
        "<body><div class=a></div><div class=b></div></body>",
        "div { position: absolute; top: 0; width: 20px; height: 20px }
         .a { left: 0 } .b { left: 40px }",
    );
    let log: Log = Rc::default();
    let mut events = EventSystem::new();
    let a = page.by_class("a");
    let b = page.by_class("b");
    events.add_listener(a, When::Bubble, recorder(&log));
    events.add_listener(b, When::Bubble, recorder(&log));

    assert_eq!(
        events.pointer_moved(&page.tree, pointer_at(Point::new(5.0, 5.0)), &()),
        Some(a)
    );
    assert_eq!(log.borrow().len(), 1, "entered a");

    assert_eq!(
        events.pointer_moved(&page.tree, pointer_at(Point::new(45.0, 5.0)), &()),
        Some(b)
    );
    assert_eq!(log.borrow().len(), 3, "left a and entered b");
}

#[test]
fn staying_within_a_node_does_not_re_enter_it() {
    let page = Page::new(
        "<body><div class=a></div></body>",
        ".a { width: 50px; height: 50px }",
    );
    let log: Log = Rc::default();
    let mut events = EventSystem::new();
    events.add_listener(page.by_class("a"), When::Bubble, recorder(&log));

    events.pointer_moved(&page.tree, pointer_at(Point::new(5.0, 5.0)), &());
    events.pointer_moved(&page.tree, pointer_at(Point::new(9.0, 9.0)), &());
    assert_eq!(log.borrow().len(), 1, "one enter, no spurious second");
}

// ---- the bits selectors read --------------------------------------------------------------

/// The payoff: M3 put `ElementState` on the node and the cascade has been matching `:hover`
/// against it ever since, always getting `false`. This is what writes it.
#[test]
fn hovering_sets_the_state_the_cascade_reads() {
    let mut page = Page::new(
        "<body><div class=card></div></body>",
        ".card { width: 50px; height: 50px }",
    );
    let card = page.by_class("card");
    let mut events = EventSystem::new();

    assert_eq!(
        page.tree.element(card).unwrap().state,
        ElementState::empty()
    );

    events.pointer_moved(&page.tree, pointer_at(Point::new(10.0, 10.0)), &());
    assert!(events.apply_state(&mut page.tree) > 0);

    assert!(
        page.tree
            .element(card)
            .unwrap()
            .state
            .contains(ElementState::HOVER)
    );
    // And the ancestors, because `:hover` on a container is a real thing.
    let body = page.find("body");
    assert!(
        page.tree
            .element(body)
            .unwrap()
            .state
            .contains(ElementState::HOVER)
    );
}

#[test]
fn a_hover_selector_matches_once_the_state_is_applied() {
    // End to end: move the pointer, apply the state, restyle, and see the rule take effect.
    let mut page = Page::new(
        "<body><div class=card></div></body>",
        ".card { width: 50px; height: 50px; background-color: white }
         .card:hover { background-color: red }",
    );
    let card = page.by_class("card");

    let restyle = |tree: &Tree| {
        let mut engine = StyleEngine::new();
        engine.add_stylesheet(
            Stylesheet::parse(
                ".card { width: 50px; height: 50px; background-color: white }
                 .card:hover { background-color: red }",
            )
            .unwrap(),
        );
        let (styles, _) = engine.restyle(tree);
        styles
    };

    let before = restyle(&page.tree);
    assert_eq!(
        before.get(card).unwrap().background_color,
        crisol_style::values::Color::WHITE
    );

    let mut events = EventSystem::new();
    events.pointer_moved(&page.tree, pointer_at(Point::new(10.0, 10.0)), &());
    events.apply_state(&mut page.tree);

    let after = restyle(&page.tree);
    assert_eq!(
        after.get(card).unwrap().background_color,
        crisol_style::values::Color::rgba(255, 0, 0, 255),
        ":hover should match now that something writes the state"
    );
}

#[test]
fn focus_sets_focus_and_focus_within_up_the_chain() {
    let mut page = Page::new(
        "<body><div class=form><input class=field></div></body>",
        ".field { width: 20px; height: 20px }",
    );
    let field = page.by_class("field");
    let form = page.by_class("form");

    let mut events = EventSystem::new();
    events.set_focus(Some(field));
    events.apply_state(&mut page.tree);

    let field_state = page.tree.element(field).unwrap().state;
    assert!(field_state.contains(ElementState::FOCUS));
    assert!(field_state.contains(ElementState::FOCUS_WITHIN));

    let form_state = page.tree.element(form).unwrap().state;
    assert!(
        !form_state.contains(ElementState::FOCUS),
        "the container does not have focus itself"
    );
    assert!(
        form_state.contains(ElementState::FOCUS_WITHIN),
        "but it does contain it"
    );
}

#[test]
fn pressing_sets_active_and_releasing_clears_it() {
    let mut page = Page::new(
        "<body><button class=go></button></body>",
        ".go { width: 40px; height: 20px }",
    );
    let button = page.by_class("go");
    let mut events = EventSystem::new();

    events.pointer_pressed(button);
    events.apply_state(&mut page.tree);
    assert!(
        page.tree
            .element(button)
            .unwrap()
            .state
            .contains(ElementState::ACTIVE)
    );

    events.pointer_released();
    events.apply_state(&mut page.tree);
    assert!(
        !page
            .tree
            .element(button)
            .unwrap()
            .state
            .contains(ElementState::ACTIVE)
    );
}

/// ROADMAP §3.6 names pointer cancellation specifically. A cancelled press is not a click,
/// and conflating the two is why interfaces leave buttons stuck looking pressed.
#[test]
fn a_cancelled_pointer_clears_active_without_being_a_release() {
    let mut page = Page::new(
        "<body><button class=go></button></body>",
        ".go { width: 40px; height: 20px }",
    );
    let button = page.by_class("go");
    let mut events = EventSystem::new();

    events.pointer_pressed(button);
    assert_eq!(events.pointer_cancelled(), Some(button));
    assert_eq!(events.pressed(), None);

    events.apply_state(&mut page.tree);
    assert!(
        !page
            .tree
            .element(button)
            .unwrap()
            .state
            .contains(ElementState::ACTIVE)
    );
}

#[test]
fn applying_state_leaves_bits_that_are_not_ours_alone() {
    let mut page = Page::new(
        "<body><button class=go></button></body>",
        ".go { width: 40px; height: 20px }",
    );
    let button = page.by_class("go");
    page.tree.element_mut(button).unwrap().state = ElementState::DISABLED | ElementState::CHECKED;

    EventSystem::new().apply_state(&mut page.tree);

    let state = page.tree.element(button).unwrap().state;
    assert!(
        state.contains(ElementState::DISABLED),
        "not the event system's to clear"
    );
    assert!(state.contains(ElementState::CHECKED));
}

#[test]
fn state_referring_to_removed_nodes_is_forgotten() {
    let mut page = Page::new(
        "<body><div class=card></div></body>",
        ".card { width: 50px; height: 50px }",
    );
    let card = page.by_class("card");
    let mut events = EventSystem::new();
    events.pointer_moved(&page.tree, pointer_at(Point::new(10.0, 10.0)), &());
    events.set_focus(Some(card));
    assert_eq!(events.hovered(), Some(card));

    page.tree.remove_subtree(card);
    events.forget_dead(&page.tree);

    assert_ne!(events.hovered(), Some(card));
    assert_eq!(
        events.focused(),
        None,
        "focus on a removed node is no focus"
    );
}

// ---- the vocabulary -----------------------------------------------------------------------

/// ROADMAP §3.6: touch designed into the event model now, not bolted on at M22.
#[test]
fn a_touch_and_a_mouse_are_the_same_kind_of_event() {
    let touch = PointerEvent {
        id: PointerId(3),
        kind: PointerKind::Touch,
        button: PointerButton::Primary,
        position: Point::new(10.0, 10.0),
        modifiers: Modifiers::empty(),
    };
    let event = Event::PointerDown(touch);
    assert_eq!(event.pointer().map(|p| p.kind), Some(PointerKind::Touch));
    assert!(event.bubbles());

    // And the engine can tell them apart when it needs to — a stylus gets hover, a finger
    // does not.
    let mouse = pointer_at(Point::new(10.0, 10.0));
    assert_eq!(mouse.kind, PointerKind::Mouse);
    assert_ne!(mouse.id, touch.id);
}

#[test]
fn the_shortcut_modifier_is_the_platforms() {
    let expected = if cfg!(target_os = "macos") {
        Modifiers::META
    } else {
        Modifiers::CONTROL
    };
    assert_eq!(Modifiers::shortcut(), expected);
    assert!(expected.has_shortcut());
    assert!(!Modifiers::SHIFT.has_shortcut());
}

#[test]
fn a_click_lands_on_what_hit_testing_found() {
    let page = Page::new(
        "<body><div class=card></div></body>",
        ".card { width: 50px; height: 50px }",
    );
    let hit = hit_test(&page.tree, Point::new(10.0, 10.0)).unwrap();
    let mut events = EventSystem::new();
    let outcome = events.dispatch(&page.tree, hit.node, &click());
    assert_eq!(outcome.visited.last().map(|(id, _)| *id), page.tree.root());
}
