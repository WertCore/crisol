//! What an event is, and how it travels.
//!
//! The shape here is the DOM's, because M16 has to present exactly that to JavaScript and a
//! different model underneath would mean translating on every dispatch. Where the DOM has
//! accumulated three ways to say the same thing, only one is here.
//!
//! **Pointers, not mice.** ROADMAP §3.6 requires touch and gesture input designed into the
//! event model at M5 rather than bolted on for M22, and this is what that means concretely:
//! there is no mouse event. A finger, a stylus and a mouse all produce [`PointerEvent`]s
//! that differ by [`PointerKind`], so an application written against a mouse today is
//! already written against a finger.

use crisol_display_list::Point;
use crisol_tree::NodeId;

/// What kind of device produced a pointer event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PointerKind {
    /// A mouse or trackpad.
    #[default]
    Mouse,
    /// A finger.
    Touch,
    /// A pen or stylus.
    Pen,
}

/// Which button, for devices that have them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// Left, or a finger's only button.
    #[default]
    Primary,
    /// Right.
    Secondary,
    /// Middle.
    Auxiliary,
}

/// Identifies one pointer among several.
///
/// Touch is multi-pointer by nature, so this exists from the start rather than being added
/// when the phones arrive. A mouse is pointer zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PointerId(pub u32);

/// A pointer moving, pressing or releasing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    /// Which pointer.
    pub id: PointerId,
    /// What kind of device.
    pub kind: PointerKind,
    /// Which button, for press and release.
    pub button: PointerButton,
    /// Position in the layout's coordinate space, in logical pixels.
    pub position: Point,
    /// Modifier keys held at the time.
    pub modifiers: Modifiers,
}

/// A key going down or up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// The key, by what it means rather than where it sits.
    pub key: Key,
    /// Whether this is a repeat from the key being held.
    pub repeat: bool,
    /// Modifier keys held at the time.
    pub modifiers: Modifiers,
}

/// A key, named by meaning.
///
/// Not a scancode: an application wants to know the user pressed Enter, not that they pressed
/// the key in position 28. Physical positions matter for games and are not what this engine
/// is for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that produces text, as the text it produces.
    Character(String),
    /// Move focus forward, or backward with shift.
    Tab,
    /// Activate.
    Enter,
    /// Cancel, close, or clear a selection.
    Escape,
    /// Insert a space.
    Space,
    /// Delete backwards.
    Backspace,
    /// Delete forwards.
    Delete,
    /// Arrow keys.
    ArrowLeft,
    /// Arrow keys.
    ArrowRight,
    /// Arrow keys.
    ArrowUp,
    /// Arrow keys.
    ArrowDown,
    /// Start of line.
    Home,
    /// End of line.
    End,
    /// A key this engine has no name for.
    Unidentified,
}

bitflags::bitflags! {
    /// Modifier keys held during an event.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Modifiers: u8 {
        /// Shift.
        const SHIFT = 1 << 0;
        /// Control.
        const CONTROL = 1 << 1;
        /// Alt, or Option.
        const ALT = 1 << 2;
        /// The platform's command key: Super on Linux, Windows key, or Command on macOS.
        const META = 1 << 3;
    }
}

impl Modifiers {
    /// The modifier this platform uses for shortcuts — Command on macOS, Control elsewhere.
    ///
    /// A method rather than a constant because getting it wrong means every shortcut in the
    /// application is wrong on one platform, and that is worth having exactly one answer to.
    #[must_use]
    pub fn shortcut() -> Self {
        if cfg!(target_os = "macos") {
            Self::META
        } else {
            Self::CONTROL
        }
    }

    /// Whether the platform's shortcut modifier is held.
    #[must_use]
    pub fn has_shortcut(self) -> bool {
        self.contains(Self::shortcut())
    }
}

/// Everything that can happen.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A pointer was pressed.
    PointerDown(PointerEvent),
    /// A pointer was released.
    PointerUp(PointerEvent),
    /// A pointer moved.
    PointerMove(PointerEvent),
    /// A pointer entered a node's box.
    PointerEnter(PointerEvent),
    /// A pointer left a node's box.
    PointerLeave(PointerEvent),
    /// The system took the pointer away — a gesture was recognised, a call arrived, the
    /// window lost focus mid-drag.
    ///
    /// Not the same as [`Self::PointerUp`], and conflating them is why so many interfaces
    /// leave a button stuck in its pressed state after an interruption. ROADMAP §3.6 names
    /// pointer cancellation specifically.
    PointerCancel(PointerEvent),
    /// A scroll gesture.
    Wheel {
        /// Horizontal distance in logical pixels.
        delta_x: f32,
        /// Vertical distance in logical pixels.
        delta_y: f32,
        /// Where the pointer was.
        position: Point,
        /// Modifier keys held.
        modifiers: Modifiers,
    },
    /// A key went down.
    KeyDown(KeyEvent),
    /// A key came up.
    KeyUp(KeyEvent),
    /// Text was committed — by a keystroke, a paste, or an IME.
    ///
    /// Separate from [`Self::KeyDown`] because the two are not the same question. A key
    /// press may produce no text, and text may arrive with no key press at all, which is
    /// exactly what an IME does.
    TextInput(String),
    /// A node gained focus.
    Focus,
    /// A node lost focus.
    Blur,
}

impl Event {
    /// The pointer this event came from, if any.
    #[must_use]
    pub fn pointer(&self) -> Option<&PointerEvent> {
        match self {
            Self::PointerDown(p)
            | Self::PointerUp(p)
            | Self::PointerMove(p)
            | Self::PointerEnter(p)
            | Self::PointerLeave(p)
            | Self::PointerCancel(p) => Some(p),
            _ => None,
        }
    }

    /// Whether this event propagates through the tree at all.
    ///
    /// Enter and leave do not: they are delivered to exactly the nodes they concern, which
    /// is what makes `:hover` on a parent and on a child both work without a `:hover` on
    /// every node in between firing as well.
    #[must_use]
    pub fn bubbles(&self) -> bool {
        !matches!(
            self,
            Self::PointerEnter(_) | Self::PointerLeave(_) | Self::Focus | Self::Blur
        )
    }
}

/// Which leg of the journey an event is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Travelling down from the root towards the target.
    Capture,
    /// At the target.
    Target,
    /// Travelling back up from the target to the root.
    Bubble,
}

/// An event in flight, with the flags a listener can set on it.
#[derive(Debug)]
pub struct Dispatch<'a> {
    /// The event.
    pub event: &'a Event,
    /// The node the event is aimed at.
    pub target: NodeId,
    /// The node currently being offered the event.
    pub current: NodeId,
    /// Which leg of the journey.
    pub phase: Phase,
    stopped: bool,
    stopped_immediately: bool,
    default_prevented: bool,
}

impl<'a> Dispatch<'a> {
    pub(crate) fn new(event: &'a Event, target: NodeId) -> Self {
        Self {
            event,
            target,
            current: target,
            phase: Phase::Target,
            stopped: false,
            stopped_immediately: false,
            default_prevented: false,
        }
    }

    /// Stops the event travelling to any further node.
    ///
    /// Listeners already registered on *this* node still run. That is the DOM's rule and it
    /// is the useful one: two listeners on the same node rarely know about each other, so
    /// one silencing the other is almost always a bug.
    pub fn stop_propagation(&mut self) {
        self.stopped = true;
    }

    /// Stops the event reaching any further listener, including on this node.
    pub fn stop_immediate_propagation(&mut self) {
        self.stopped = true;
        self.stopped_immediately = true;
    }

    /// Tells the engine not to do whatever it would normally have done.
    ///
    /// What suppresses the caret moving on a click, the scroll on a wheel, or the focus
    /// change on a Tab.
    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }

    /// Whether propagation has been stopped.
    #[must_use]
    pub fn is_propagation_stopped(&self) -> bool {
        self.stopped
    }

    /// Whether the remaining listeners on this node have been stopped too.
    #[must_use]
    pub fn is_immediately_stopped(&self) -> bool {
        self.stopped_immediately
    }

    /// Whether the engine's default action was suppressed.
    #[must_use]
    pub fn is_default_prevented(&self) -> bool {
        self.default_prevented
    }
}

/// What a dispatch left behind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Nodes the event actually reached, in the order they saw it.
    ///
    /// The whole journey, capture then target then bubble, which is what makes a test able
    /// to assert the order rather than merely the destination.
    pub visited: Vec<(NodeId, Phase)>,
    /// Whether a listener asked the engine not to act.
    pub default_prevented: bool,
    /// Whether propagation was stopped before the event reached the root.
    pub propagation_stopped: bool,
}
