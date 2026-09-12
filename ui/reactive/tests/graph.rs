//! The reactive graph's contract, independent of any view.

use std::cell::RefCell;
use std::rc::Rc;

use crisol_dom::Dom;
use crisol_reactive::{Cx, Runtime, Track};
use crisol_tree::Tree;

/// A recorder an effect can write to, so a test can see what ran and in what order.
type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

fn taken(log: &Log) -> Vec<String> {
    std::mem::take(&mut log.borrow_mut())
}

#[test]
fn an_effect_runs_once_on_creation_and_again_when_what_it_read_changes() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let count = runtime.signal(0_i32);
    let seen = log();

    let recorder = Rc::clone(&seen);
    runtime.effect(&mut dom, move |cx| {
        recorder
            .borrow_mut()
            .push(format!("count={}", cx.get(count)));
    });
    // The first run is not an optimisation: an effect that has never run has no
    // dependencies and would never run again.
    assert_eq!(taken(&seen), vec!["count=0"]);

    runtime.set(count, 1);
    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), vec!["count=1"]);
}

#[test]
fn nothing_runs_between_flushes() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let count = runtime.signal(0_i32);
    let seen = log();

    let recorder = Rc::clone(&seen);
    runtime.effect(&mut dom, move |cx| {
        recorder.borrow_mut().push(cx.get(count).to_string());
    });
    taken(&seen);

    // Three writes, one flush: a frame sees one consistent state, not three.
    runtime.set(count, 1);
    runtime.set(count, 2);
    runtime.set(count, 3);
    assert_eq!(taken(&seen), Vec::<String>::new());
    assert_eq!(runtime.pending(), 1, "coalesced, not queued three times");

    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), vec!["3"]);
}

#[test]
fn a_memo_is_computed_lazily_and_cached() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let count = runtime.signal(2_i32);
    let doubled = runtime.memo(move |track| track.get(count) * 2);

    // Nobody has read it, so it has never run.
    assert_eq!(runtime.stats().memos_computed, 0);

    let track = Cx::new(&runtime, &mut dom);
    assert_eq!(track.memo(doubled), 4);
    assert_eq!(track.memo(doubled), 4);
    assert_eq!(
        runtime.stats().memos_computed,
        1,
        "cached on the second read"
    );

    runtime.set(count, 5);
    // Still not recomputed: a memo nobody is looking at costs nothing.
    assert_eq!(runtime.stats().memos_computed, 1);
    let track = Cx::new(&runtime, &mut dom);
    assert_eq!(track.memo(doubled), 10);
    assert_eq!(runtime.stats().memos_computed, 2);
}

#[test]
fn an_effect_downstream_of_a_memo_sees_a_fresh_value() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let count = runtime.signal(1_i32);
    let doubled = runtime.memo(move |track| track.get(count) * 2);
    let seen = log();

    let recorder = Rc::clone(&seen);
    runtime.effect(&mut dom, move |cx| {
        recorder.borrow_mut().push(cx.memo(doubled).to_string());
    });
    assert_eq!(taken(&seen), vec!["2"]);

    runtime.set(count, 10);
    runtime.flush(&mut dom);
    // Never an intermediate value: the memo recomputes when the effect reads it, not on a
    // schedule that could interleave with the effect.
    assert_eq!(taken(&seen), vec!["20"]);
}

#[test]
fn a_diamond_runs_its_effect_once() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let source = runtime.signal(1_i32);
    let left = runtime.memo(move |track| track.get(source) + 1);
    let right = runtime.memo(move |track| track.get(source) * 10);
    let seen = log();

    let recorder = Rc::clone(&seen);
    runtime.effect(&mut dom, move |cx| {
        let total = cx.memo(left) + cx.memo(right);
        recorder.borrow_mut().push(total.to_string());
    });
    assert_eq!(taken(&seen), vec!["12"]);

    runtime.set(source, 2);
    runtime.flush(&mut dom);
    // Two paths reach the effect. It must run once with both sides updated, not twice with
    // one side stale — the classic glitch.
    assert_eq!(taken(&seen), vec!["23"]);
}

#[test]
fn dependencies_are_rebuilt_on_every_run() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let use_left = runtime.signal(true);
    let left = runtime.signal(1_i32);
    let right = runtime.signal(100_i32);
    let seen = log();

    let recorder = Rc::clone(&seen);
    runtime.effect(&mut dom, move |cx| {
        let value = if cx.get(use_left) {
            cx.get(left)
        } else {
            cx.get(right)
        };
        recorder.borrow_mut().push(value.to_string());
    });
    assert_eq!(taken(&seen), vec!["1"]);

    // The untaken branch was never read, so writing it must not wake the effect.
    runtime.set(right, 200);
    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), Vec::<String>::new());

    runtime.set(use_left, false);
    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), vec!["200"]);

    // ...and now the roles are reversed: `left` is no longer a dependency.
    runtime.set(left, 2);
    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), Vec::<String>::new());
}

#[test]
fn peek_reads_without_subscribing() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let tick = runtime.signal(0_i32);
    let other = runtime.signal(0_i32);
    let seen = log();

    let recorder = Rc::clone(&seen);
    runtime.effect(&mut dom, move |cx| {
        let watched = cx.get(tick);
        let unwatched = cx.runtime().peek(other).unwrap_or_default();
        recorder.borrow_mut().push(format!("{watched}/{unwatched}"));
    });
    assert_eq!(taken(&seen), vec!["0/0"]);

    runtime.set(other, 9);
    runtime.flush(&mut dom);
    assert_eq!(
        taken(&seen),
        Vec::<String>::new(),
        "peek does not subscribe"
    );

    runtime.set(tick, 1);
    runtime.flush(&mut dom);
    assert_eq!(
        taken(&seen),
        vec!["1/9"],
        "but it does see the current value"
    );
}

#[test]
fn set_if_changed_skips_the_effect_entirely() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let count = runtime.signal(1_i32);

    runtime.effect(&mut dom, move |cx| {
        let _ = cx.get(count);
    });
    runtime.reset_stats();

    assert!(!runtime.set_if_changed(count, 1));
    assert_eq!(runtime.pending(), 0);
    runtime.flush(&mut dom);
    assert_eq!(runtime.stats().effects_run, 0);

    assert!(runtime.set_if_changed(count, 2));
    runtime.flush(&mut dom);
    assert_eq!(runtime.stats().effects_run, 1);
}

#[test]
fn update_mutates_in_place_without_cloning() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let items = runtime.signal(vec![1_i32, 2, 3]);
    let seen = log();

    let recorder = Rc::clone(&seen);
    runtime.effect(&mut dom, move |cx| {
        let len = cx.with(items, Vec::len).unwrap_or_default();
        recorder.borrow_mut().push(len.to_string());
    });
    assert_eq!(taken(&seen), vec!["3"]);

    let pushed = runtime.update(items, |items| {
        items.push(4);
        items.len()
    });
    assert_eq!(pushed, Some(4));
    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), vec!["4"]);
}

#[test]
fn disposing_a_scope_stops_its_effects() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let count = runtime.signal(0_i32);
    let seen = log();

    let recorder = Rc::clone(&seen);
    let (scope, ()) = runtime.scope(|_| {
        runtime.effect(&mut dom, move |cx| {
            recorder.borrow_mut().push(cx.get(count).to_string());
        });
    });
    assert_eq!(taken(&seen), vec!["0"]);

    runtime.set(count, 1);
    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), vec!["1"]);

    assert!(runtime.dispose(scope, &mut dom));
    runtime.set(count, 2);
    runtime.flush(&mut dom);
    assert_eq!(
        taken(&seen),
        Vec::<String>::new(),
        "a disposed effect is gone"
    );
    assert!(
        !runtime.dispose(scope, &mut dom),
        "disposing twice is not an error"
    );
}

#[test]
fn a_signal_queued_and_then_disposed_does_not_run() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let count = runtime.signal(0_i32);
    let seen = log();

    let recorder = Rc::clone(&seen);
    let (scope, ()) = runtime.scope(|_| {
        runtime.effect(&mut dom, move |cx| {
            recorder.borrow_mut().push(cx.get(count).to_string());
        });
    });
    taken(&seen);

    // Queue it, then dispose before flushing. The queue must not hold a dangling slot —
    // which, with slot reuse, would mean running whoever took it.
    runtime.set(count, 1);
    assert_eq!(runtime.pending(), 1);
    runtime.dispose(scope, &mut dom);
    assert_eq!(runtime.pending(), 0);
    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), Vec::<String>::new());
}

#[test]
fn disposing_a_parent_disposes_its_children() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let count = runtime.signal(0_i32);
    let seen = log();

    let recorder = Rc::clone(&seen);
    let (outer, ()) = runtime.scope(|_| {
        runtime.scope(|_| {
            runtime.effect(&mut dom, move |cx| {
                recorder.borrow_mut().push(cx.get(count).to_string());
            });
        });
    });
    taken(&seen);

    runtime.dispose(outer, &mut dom);
    runtime.set(count, 1);
    runtime.flush(&mut dom);
    assert_eq!(taken(&seen), Vec::<String>::new());
}

#[test]
fn cleanups_run_innermost_first_and_can_touch_the_dom() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let root = dom.create_element("div");
    dom.set_root(root);
    let seen = log();

    let outer_log = Rc::clone(&seen);
    let inner_log = Rc::clone(&seen);
    let child = dom.create_element("span");
    dom.append_child(root, child).unwrap();

    let (outer, ()) = runtime.scope(|_| {
        runtime.on_cleanup(move |_| outer_log.borrow_mut().push("outer".into()));
        runtime.scope(|_| {
            runtime.on_cleanup(move |dom| {
                inner_log.borrow_mut().push("inner".into());
                // The node is still alive here, which is the point of running cleanups
                // before anything is freed.
                assert!(dom.is_alive(child));
                dom.remove_subtree(child);
            });
        });
    });

    runtime.dispose(outer, &mut dom);
    assert_eq!(taken(&seen), vec!["inner", "outer"]);
    assert!(!dom.is_alive(child));
}

#[test]
fn a_handle_used_after_disposal_fails_rather_than_hitting_a_reused_slot() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();

    let (scope, stale) = runtime.scope(|_| runtime.signal(1_i32));
    runtime.dispose(scope, &mut dom);

    // The slot is free and the next signal will take it. A generation check is what stops
    // the stale handle from reading the new one's value.
    let fresh = runtime.signal(99_i32);
    assert_eq!(runtime.peek(fresh), Some(99));
    assert_eq!(runtime.peek(stale), None);
    assert!(!runtime.set(stale, 5));

    let track = Cx::new(&runtime, &mut dom);
    assert_eq!(track.try_get(stale), None);
}

#[test]
fn an_effect_writing_a_signal_it_does_not_read_settles() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let source = runtime.signal(1_i32);
    let mirror = runtime.signal(0_i32);
    let seen = log();

    runtime.effect(&mut dom, move |cx| {
        let value = cx.get(source);
        cx.runtime().set(mirror, value * 2);
    });
    let recorder = Rc::clone(&seen);
    runtime.effect(&mut dom, move |cx| {
        recorder.borrow_mut().push(cx.get(mirror).to_string());
    });
    taken(&seen);

    runtime.set(source, 4);
    runtime.flush(&mut dom);
    // The write inside the first effect queues the second within the same flush.
    assert_eq!(taken(&seen), vec!["8"]);
    assert_eq!(runtime.pending(), 0);
}

#[test]
fn a_memo_can_read_another_memo() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let base = runtime.signal(2_i32);
    let doubled = runtime.memo(move |track: &Track<'_>| track.get(base) * 2);
    let quadrupled = runtime.memo(move |track: &Track<'_>| track.memo(doubled) * 2);

    let track = Cx::new(&runtime, &mut dom);
    assert_eq!(track.memo(quadrupled), 8);

    runtime.set(base, 3);
    let track = Cx::new(&runtime, &mut dom);
    assert_eq!(
        track.memo(quadrupled),
        12,
        "staleness propagates through the chain"
    );
}

#[test]
fn a_read_can_nest_inside_another_read() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();

    // The shape a filtered list takes: a signal holding handles, each of which is read
    // while the outer one is still borrowed.
    let flags = vec![
        runtime.signal(true),
        runtime.signal(false),
        runtime.signal(true),
    ];
    let all = runtime.signal(flags.clone());

    let track = Cx::new(&runtime, &mut dom);
    let lit = track
        .with(all, |flags| {
            flags.iter().filter(|&&flag| track.get(flag)).count()
        })
        .unwrap();
    assert_eq!(lit, 2);
}

#[test]
fn a_write_during_a_read_is_not_overwritten_when_the_read_returns() {
    let mut tree = Tree::new();
    let mut dom = Dom::new(&mut tree);
    let runtime = Runtime::new();
    let value = runtime.signal(1_i32);

    // The value is moved out of the arena for the duration of the read; putting it back
    // must not clobber what the closure wrote in the meantime.
    let track = Cx::new(&runtime, &mut dom);
    let observed = track
        .with(value, |seen| {
            runtime.set(value, 99);
            *seen
        })
        .unwrap();
    assert_eq!(observed, 1, "the read sees the value it started with");
    assert_eq!(runtime.peek(value), Some(99), "and the write survives");
}
