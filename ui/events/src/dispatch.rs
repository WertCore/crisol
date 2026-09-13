//! Sending an event through the tree, and keeping the state selectors read.

use crisol_display_list::Point;
use crisol_tree::{ElementState, NodeId, NodeMap, Tree};

use crate::event::{Dispatch, DragEvent, Event, Outcome, Phase, PointerEvent};
use crate::hit::{TextLookup, hit_test_with_text, path_to};

/// Something that wants to see events.
///
/// A trait rather than a boxed closure because at M16 the listener is a JavaScript function
/// and the runtime, not this crate, decides how to call it. An engine that had baked
/// `Box<dyn FnMut>` into its dispatch would have to be rewritten to get there.
pub trait Listener {
    /// Offers the event. The listener may stop propagation or prevent the default.
    fn handle(&mut self, dispatch: &mut Dispatch<'_>);
}

impl<F> Listener for F
where
    F: FnMut(&mut Dispatch<'_>),
{
    fn handle(&mut self, dispatch: &mut Dispatch<'_>) {
        self(dispatch);
    }
}

/// When a listener wants the event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum When {
    /// On the way down. Rare, and the reason capture exists: an ancestor that wants to
    /// intercept before a descendant can act.
    Capture,
    /// On the way back up, which is what nearly everything wants.
    #[default]
    Bubble,
}

struct Registration {
    when: When,
    listener: Box<dyn Listener>,
}

/// Drives events into a tree and keeps the interaction state selectors read.
///
/// Owns the listener registry and the hover, focus and press state. All of it lives here
/// rather than on the node, for the same reason computed style does (DECISIONS D-21): the
/// tree is below the event system and has to stay usable without it.
#[derive(Default)]
pub struct EventSystem {
    listeners: NodeMap<Vec<Registration>>,
    /// Nodes currently under a pointer, deepest last. Kept as the whole chain rather than
    /// the deepest node so that `:hover` works on an ancestor too, which is what CSS means.
    hovered: Vec<NodeId>,
    /// Nodes currently under a drag, deepest last. Separate from `hovered` because a drag is
    /// not the window's pointer: on every platform the OS owns the cursor for the duration,
    /// so the two chains can disagree and `:hover` must not follow a drag around.
    dragged: Vec<NodeId>,
    focused: Option<NodeId>,
    pressed: Option<NodeId>,
}

impl std::fmt::Debug for EventSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSystem")
            .field("hovered", &self.hovered.len())
            .field("dragged", &self.dragged.len())
            .field("focused", &self.focused)
            .field("pressed", &self.pressed)
            .finish_non_exhaustive()
    }
}

impl EventSystem {
    /// An event system with nothing registered and nothing hovered or focused.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a listener on a node.
    pub fn add_listener(&mut self, node: NodeId, when: When, listener: impl Listener + 'static) {
        let registration = Registration {
            when,
            listener: Box::new(listener),
        };
        match self.listeners.get_mut(node) {
            Some(existing) => existing.push(registration),
            None => {
                self.listeners.insert(node, vec![registration]);
            }
        }
    }

    /// Removes every listener on a node.
    pub fn clear_listeners(&mut self, node: NodeId) {
        self.listeners.remove(node);
    }

    /// The node with keyboard focus.
    #[must_use]
    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// The deepest node under the pointer.
    #[must_use]
    pub fn hovered(&self) -> Option<NodeId> {
        self.hovered.last().copied()
    }

    /// The node a pointer is currently held down on.
    #[must_use]
    pub fn pressed(&self) -> Option<NodeId> {
        self.pressed
    }

    /// Sends an event to `target`, running capture down and bubble back up.
    ///
    /// Returns where it went and what listeners asked for. The tree is passed separately
    /// from the listeners so that a listener can be given `&mut` access to its own state
    /// without the tree being borrowed across the call.
    pub fn dispatch(&mut self, tree: &Tree, target: NodeId, event: &Event) -> Outcome {
        let mut outcome = Outcome::default();
        let mut flight = Dispatch::new(event, target);

        if !event.bubbles() {
            self.offer(target, Phase::Target, &mut flight, &mut outcome);
            outcome.default_prevented = flight.is_default_prevented();
            return outcome;
        }

        let path = path_to(tree, target);

        // Down: every ancestor except the target itself.
        for &node in &path[..path.len().saturating_sub(1)] {
            self.offer(node, Phase::Capture, &mut flight, &mut outcome);
            if flight.is_propagation_stopped() {
                outcome.propagation_stopped = true;
                outcome.default_prevented = flight.is_default_prevented();
                return outcome;
            }
        }

        self.offer(target, Phase::Target, &mut flight, &mut outcome);

        // Back up: everything except the target, in reverse.
        if !flight.is_propagation_stopped() {
            for &node in path[..path.len().saturating_sub(1)].iter().rev() {
                self.offer(node, Phase::Bubble, &mut flight, &mut outcome);
                if flight.is_propagation_stopped() {
                    break;
                }
            }
        }

        outcome.propagation_stopped = flight.is_propagation_stopped();
        outcome.default_prevented = flight.is_default_prevented();
        outcome
    }

    fn offer(
        &mut self,
        node: NodeId,
        phase: Phase,
        flight: &mut Dispatch<'_>,
        outcome: &mut Outcome,
    ) {
        outcome.visited.push((node, phase));
        flight.current = node;
        flight.phase = phase;

        let Some(registrations) = self.listeners.get_mut(node) else {
            return;
        };
        let wanted = match phase {
            Phase::Capture => When::Capture,
            // At the target both kinds run, which is what the DOM does: a node cannot
            // capture from itself.
            Phase::Target | Phase::Bubble => When::Bubble,
        };
        for registration in registrations {
            if phase != Phase::Target && registration.when != wanted {
                continue;
            }
            registration.listener.handle(flight);
            if flight.is_immediately_stopped() {
                return;
            }
        }
    }

    // ---- pointer state --------------------------------------------------------------

    /// Moves the pointer, updating hover and emitting enter and leave along the way.
    ///
    /// Returns the node under the pointer. Enter and leave are dispatched to exactly the
    /// nodes entered and left — not to everything in between — which is what makes `:hover`
    /// on a parent and on a child both behave.
    pub fn pointer_moved(
        &mut self,
        tree: &Tree,
        pointer: PointerEvent,
        text: &impl TextLookup,
    ) -> Option<NodeId> {
        let hit = hit_test_with_text(tree, pointer.position, text);
        let path = hit.map_or_else(Vec::new, |hit| path_to(tree, hit.node));

        // Work out who left and who entered before touching anything, because offering an
        // event borrows the listener registry and the hover list lives next to it.
        let left: Vec<_> = self
            .hovered
            .iter()
            .rev()
            .copied()
            .filter(|node| !path.contains(node))
            .collect();
        let entered: Vec<_> = path
            .iter()
            .copied()
            .filter(|node| !self.hovered.contains(node))
            .collect();

        // Deepest first on the way out, so a listener sees the child leave before its parent.
        let leave = Event::PointerLeave(pointer);
        for node in left {
            let mut flight = Dispatch::new(&leave, node);
            let mut ignored = Outcome::default();
            self.offer(node, Phase::Target, &mut flight, &mut ignored);
        }
        // Shallowest first on the way in.
        let enter = Event::PointerEnter(pointer);
        for node in entered {
            let mut flight = Dispatch::new(&enter, node);
            let mut ignored = Outcome::default();
            self.offer(node, Phase::Target, &mut flight, &mut ignored);
        }

        self.hovered = path;
        self.hovered.last().copied()
    }

    /// Moves a drag over the tree, dispatching enter, leave and over.
    ///
    /// The same shape as [`Self::pointer_moved`] and for the same reasons: leave is offered
    /// deepest first so a child is seen to lose the drag before its parent does, enter
    /// shallowest first on the way back in. `over` then goes to the deepest node and bubbles,
    /// which is what a listener uses to say "yes, drop here".
    ///
    /// Returns the node under the drag.
    pub fn drag_moved(
        &mut self,
        tree: &Tree,
        drag: DragEvent,
        text: &impl TextLookup,
    ) -> Option<NodeId> {
        let path = self.retarget_drag(tree, drag, text);
        let target = path.last().copied();
        if let Some(node) = target {
            let over = Event::DragOver(drag);
            self.dispatch(tree, node, &over);
        }
        self.dragged = path;
        target
    }

    /// Drops a drag on whatever is under it, then forgets the drag.
    ///
    /// Returns the node the drop landed on. The dragged data is not here: the platform hands
    /// that over separately and asynchronously, so correlating the two is the caller's job.
    pub fn drag_dropped(
        &mut self,
        tree: &Tree,
        drag: DragEvent,
        text: &impl TextLookup,
    ) -> Option<NodeId> {
        // Retargeted first, because a drop can arrive at a position no `over` was reported
        // for — the last motion before a release is not guaranteed to reach us.
        let path = self.retarget_drag(tree, drag, text);
        let target = path.last().copied();
        if let Some(node) = target {
            let drop = Event::Drop(drag);
            self.dispatch(tree, node, &drop);
        }
        self.drag_left(drag);
        target
    }

    /// Ends a drag without a drop, telling everything under it that it left.
    ///
    /// Takes no tree: the chain to unwind is the one already recorded, and re-deriving it
    /// from a hit test would be wrong anyway — a cancelled drag is nowhere by then.
    pub fn drag_left(&mut self, drag: DragEvent) {
        let leave = Event::DragLeave(drag);
        for node in std::mem::take(&mut self.dragged).into_iter().rev() {
            let mut flight = Dispatch::new(&leave, node);
            let mut ignored = Outcome::default();
            self.offer(node, Phase::Target, &mut flight, &mut ignored);
        }
    }

    /// The enter/leave half of a drag move, shared by moving and dropping.
    fn retarget_drag(
        &mut self,
        tree: &Tree,
        drag: DragEvent,
        text: &impl TextLookup,
    ) -> Vec<NodeId> {
        let hit = hit_test_with_text(tree, drag.position, text);
        let path = hit.map_or_else(Vec::new, |hit| path_to(tree, hit.node));

        let left: Vec<_> = self
            .dragged
            .iter()
            .rev()
            .copied()
            .filter(|node| !path.contains(node))
            .collect();
        let entered: Vec<_> = path
            .iter()
            .copied()
            .filter(|node| !self.dragged.contains(node))
            .collect();

        let leave = Event::DragLeave(drag);
        for node in left {
            let mut flight = Dispatch::new(&leave, node);
            let mut ignored = Outcome::default();
            self.offer(node, Phase::Target, &mut flight, &mut ignored);
        }
        let enter = Event::DragEnter(drag);
        for node in entered {
            let mut flight = Dispatch::new(&enter, node);
            let mut ignored = Outcome::default();
            self.offer(node, Phase::Target, &mut flight, &mut ignored);
        }
        path
    }

    /// Records a pointer going down on a node.
    pub fn pointer_pressed(&mut self, node: NodeId) {
        self.pressed = Some(node);
    }

    /// Records a pointer coming up, returning the node it went down on.
    pub fn pointer_released(&mut self) -> Option<NodeId> {
        self.pressed.take()
    }

    /// Records the system taking the pointer away.
    ///
    /// Distinct from a release: nothing was clicked, and anything mid-drag should be
    /// abandoned rather than completed (ROADMAP §3.6).
    pub fn pointer_cancelled(&mut self) -> Option<NodeId> {
        self.pressed.take()
    }

    /// Moves focus, returning the node that lost it.
    pub fn set_focus(&mut self, node: Option<NodeId>) -> Option<NodeId> {
        let previous = self.focused;
        self.focused = node;
        previous
    }

    /// Drops any state referring to nodes that are no longer alive.
    ///
    /// Hover and focus are handles, and a handle to a removed node would otherwise keep
    /// `:hover` matching something that is not there.
    pub fn forget_dead(&mut self, tree: &Tree) {
        self.hovered.retain(|node| tree.is_alive(*node));
        self.dragged.retain(|node| tree.is_alive(*node));
        self.focused = self.focused.filter(|node| tree.is_alive(*node));
        self.pressed = self.pressed.filter(|node| tree.is_alive(*node));
    }

    // ---- the bits selectors read ------------------------------------------------------

    /// Writes hover, focus and press state onto the tree's elements.
    ///
    /// The `ElementState` bits M3 put on the node and nothing has written until now — the
    /// cascade has been matching `:hover`, `:focus` and `:active` against them all along and
    /// always getting `false`. Call this after handling input and before restyling.
    ///
    /// Returns the number of nodes whose state changed, which is what tells a caller whether
    /// a restyle is needed at all.
    pub fn apply_state(&self, tree: &mut Tree) -> usize {
        let mut changed = 0;
        let hovered: Vec<_> = self.hovered.clone();
        let focus_path = self
            .focused
            .map_or_else(Vec::new, |node| path_to(tree, node));

        // Collect first, then write: `path_to` borrows the tree.
        let mut wanted: Vec<(NodeId, ElementState)> = Vec::new();
        for node in &hovered {
            wanted.push((*node, ElementState::HOVER));
        }
        if let Some(pressed) = self.pressed {
            wanted.push((pressed, ElementState::ACTIVE));
        }
        for node in &focus_path {
            let bit = if Some(*node) == self.focused {
                // A focused node is also within itself, so it gets both.
                ElementState::FOCUS | ElementState::FOCUS_WITHIN
            } else {
                ElementState::FOCUS_WITHIN
            };
            wanted.push((*node, bit));
        }

        let live: Vec<NodeId> = all_elements(tree);
        for node in live {
            let target = wanted
                .iter()
                .filter(|(id, _)| *id == node)
                .fold(ElementState::empty(), |acc, (_, bit)| acc | *bit);
            let Some(data) = tree.element_mut(node) else {
                continue;
            };
            // Only the interaction bits are ours; `DISABLED`, `CHECKED` and `INVALID` belong
            // to whoever set them.
            let interaction = ElementState::HOVER
                | ElementState::ACTIVE
                | ElementState::FOCUS
                | ElementState::FOCUS_WITHIN;
            let updated = (data.state & !interaction) | target;
            if updated != data.state {
                data.state = updated;
                changed += 1;
            }
        }
        changed
    }
}

/// Every element in the tree, in document order.
fn all_elements(tree: &Tree) -> Vec<NodeId> {
    let mut out = Vec::new();
    let Some(root) = tree.root() else {
        return out;
    };
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if tree.element(id).is_some() {
            out.push(id);
        }
        let mut child = tree.get(id).and_then(crisol_tree::Node::last_child);
        while let Some(node) = child {
            stack.push(node);
            child = tree.get(node).and_then(crisol_tree::Node::prev_sibling);
        }
    }
    out
}

/// A pointer event at a position, with defaults for everything else.
///
/// Convenience for callers and tests that only care where the pointer is.
#[must_use]
pub fn pointer_at(position: Point) -> PointerEvent {
    PointerEvent {
        id: crate::event::PointerId(0),
        kind: crate::event::PointerKind::Mouse,
        button: crate::event::PointerButton::Primary,
        position,
        modifiers: crate::event::Modifiers::empty(),
    }
}
