//! Turning a scroll gesture into movement.

use std::collections::VecDeque;
use std::time::Duration;

use crisol_display_list::{Point, Rect};
use crisol_tree::{NodeId, Tree};

use crate::hit::hit_test;

/// What a scroll gesture did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrolled {
    /// The innermost node that moved.
    pub node: NodeId,
    /// How far content moved in total, summed over every node in the chain.
    pub applied: Point,
    /// What has to be repainted: the box of the outermost node that moved.
    ///
    /// One rectangle rather than several, because an outer scroll container's box contains
    /// every inner one that could also have moved.
    pub damage: Rect,
}

/// Applies a scroll gesture at `point`, chaining outward through ancestors.
///
/// Starts at the node under the pointer and walks up. Each scroll container takes what it
/// can and passes the rest outward, so a list that has hit its end lets the page underneath
/// keep moving instead of swallowing the gesture — which is the behaviour every platform has
/// and the one users notice only when it is missing.
///
/// `delta` is in logical pixels, positive down and right. Returns `None` when nothing moved.
pub fn scroll_at(tree: &mut Tree, point: Point, delta: Point) -> Option<Scrolled> {
    let start = hit_test(tree, point)
        .map(|hit| hit.node)
        .or_else(|| tree.root())?;
    scroll_from(tree, start, delta)
}

/// As [`scroll_at`], but starting from a known node rather than a pointer position.
///
/// The form a keyboard scroll wants: page-down acts on what is focused, and there may be no
/// pointer anywhere.
pub fn scroll_from(tree: &mut Tree, start: NodeId, delta: Point) -> Option<Scrolled> {
    let mut remaining = delta;
    let mut applied = Point::ZERO;
    let mut innermost = None;
    let mut outermost = None;

    let mut current = Some(start);
    while let Some(node) = current {
        if remaining.x == 0.0 && remaining.y == 0.0 {
            break;
        }
        let moved = tree.scroll_by(node, remaining);
        if moved.x != 0.0 || moved.y != 0.0 {
            applied = applied + moved;
            remaining = remaining - moved;
            innermost.get_or_insert(node);
            outermost = Some(node);
        }
        current = tree.parent(node);
    }

    let node = innermost?;
    let damage = tree
        .absolute_rect(outermost.unwrap_or(node))
        .unwrap_or(Rect::ZERO);
    Some(Scrolled {
        node,
        applied,
        damage,
    })
}

// ---- momentum ----------------------------------------------------------------------------

/// Fraction of a fling's speed that survives one second, matching the rate the platforms
/// settle on for a normal flick.
///
/// Expressed per second rather than per frame so the result does not depend on the frame
/// rate — a fling that travelled further on a 120Hz display would be a bug people feel
/// without being able to name.
const DECELERATION: f32 = 0.135;

/// Below this, in logical pixels per second, a fling is over. At sixty frames a second this
/// is a fifth of a pixel per frame: still moving, arithmetically, and not moving at all as
/// far as anyone can see.
const STOP_BELOW: f32 = 12.0;

/// The speed under which a release is a tap with a wobble rather than a flick.
const FLING_ABOVE: f32 = 50.0;

/// A decaying scroll velocity: what a flick leaves behind.
///
/// **Not for wheel events.** A trackpad on macOS has already been through the system's own
/// momentum by the time winit reports it, and the events keep arriving after the fingers
/// lift. Running this on top would integrate the same flick twice. This is for a drag the
/// application tracked itself — touch, pen, or a grabbed scrollbar.
#[derive(Clone, Copy, Debug)]
pub struct Fling {
    node: NodeId,
    velocity: Point,
}

impl Fling {
    /// Starts a fling on `node` at `velocity`, in logical pixels per second.
    ///
    /// `None` when the release was too slow to be a flick, which is most of them: a drag
    /// that ends by stopping should stop.
    #[must_use]
    pub fn new(node: NodeId, velocity: Point) -> Option<Self> {
        let speed = velocity.x.hypot(velocity.y);
        (speed >= FLING_ABOVE).then_some(Self { node, velocity })
    }

    /// The node this fling is scrolling.
    #[must_use]
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// The current velocity, in logical pixels per second.
    #[must_use]
    pub fn velocity(&self) -> Point {
        self.velocity
    }

    /// Whether there is nothing left to do.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.velocity == Point::ZERO
    }

    /// Advances the fling by `elapsed` and applies it. `None` once it has stopped.
    ///
    /// Deliberately does **not** chain outward: a flick belongs to the list it started in,
    /// and one that leapt to the page behind it on reaching the end would feel broken. So
    /// running out of room ends the fling.
    pub fn advance(&mut self, tree: &mut Tree, elapsed: Duration) -> Option<Scrolled> {
        if self.is_finished() {
            return None;
        }
        let seconds = elapsed.as_secs_f32();
        let decay = DECELERATION.powf(seconds);
        // Distance is the integral of the decaying velocity over the interval, not
        // `velocity * elapsed`: sampling the speed at one end of the frame and holding it
        // makes the whole fling frame-rate dependent.
        let travelled = (decay - 1.0) / DECELERATION.ln();
        let delta = Point {
            x: self.velocity.x * travelled,
            y: self.velocity.y * travelled,
        };

        self.velocity = Point {
            x: self.velocity.x * decay,
            y: self.velocity.y * decay,
        };
        if self.velocity.x.hypot(self.velocity.y) < STOP_BELOW {
            self.velocity = Point::ZERO;
        }

        let moved = tree.scroll_by(self.node, delta);
        if moved == Point::ZERO {
            self.velocity = Point::ZERO;
            return None;
        }
        Some(Scrolled {
            node: self.node,
            applied: moved,
            damage: tree.absolute_rect(self.node).unwrap_or(Rect::ZERO),
        })
    }
}

/// Estimates how fast a drag was going when it ended.
///
/// Takes the oldest sample still inside a short window and the newest, rather than the last
/// two: consecutive pointer samples are noisy, and a finger that paused for one frame before
/// lifting would otherwise read as a flick at whatever speed that single frame implied.
#[derive(Clone, Debug, Default)]
pub struct VelocityTracker {
    samples: VecDeque<(Duration, Point)>,
}

/// How far back a velocity estimate looks.
const WINDOW: Duration = Duration::from_millis(100);

impl VelocityTracker {
    /// An empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records where the pointer was at `time`, measured from any fixed origin.
    pub fn sample(&mut self, time: Duration, position: Point) {
        self.samples.push_back((time, position));
        while let Some(&(oldest, _)) = self.samples.front() {
            if self.samples.len() > 2 && time.saturating_sub(oldest) > WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// The estimated velocity in logical pixels per second, or zero if there is not enough
    /// to go on.
    #[must_use]
    pub fn velocity(&self) -> Point {
        let (Some(&(first, start)), Some(&(last, end))) =
            (self.samples.front(), self.samples.back())
        else {
            return Point::ZERO;
        };
        let seconds = last.saturating_sub(first).as_secs_f32();
        if seconds <= 0.0 {
            return Point::ZERO;
        }
        Point {
            x: (end.x - start.x) / seconds,
            y: (end.y - start.y) / seconds,
        }
    }

    /// Forgets everything, for the start of a new drag.
    pub fn clear(&mut self) {
        self.samples.clear();
    }
}
