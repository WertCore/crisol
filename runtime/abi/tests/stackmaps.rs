//! Stack map lookup and native frame walking.
//!
//! The lookup is tested against a hand-built table rather than only through the process-global
//! one a compiled program registers: a lookup reachable only via generated code is a lookup
//! nothing can check.

use crisol_abi::{StackMapRow, live_at, walk_frames};

/// A table for a function starting at `base`.
fn row(base: *const u8, code_offset: u32, frame_offset: u32) -> StackMapRow {
    StackMapRow {
        function: base,
        code_offset,
        frame_offset,
    }
}

/// A stable fake address. Never dereferenced.
fn address(at: usize) -> *const u8 {
    at as *const u8
}

#[test]
fn a_return_address_finds_the_offsets_live_at_it() {
    let base = address(0x1000);
    let rows = [row(base, 0x20, 8), row(base, 0x20, 16), row(base, 0x40, 24)];
    // 0x1000 + 0x20
    let live = live_at(&rows, address(0x1020));
    assert_eq!(live, vec![8, 16], "both values live at that safepoint");
}

#[test]
fn an_address_that_is_not_a_safepoint_yields_nothing() {
    // Not an error: every frame except those at a call into the runtime is in this position.
    let rows = [row(address(0x1000), 0x20, 8)];
    assert!(live_at(&rows, address(0x1030)).is_empty());
    assert!(live_at(&rows, address(0)).is_empty());
}

#[test]
fn the_match_is_exact_rather_than_a_range() {
    // A range match would attribute a return address to the nearest preceding safepoint, which
    // is a different safepoint's live set — plausible offsets naming the wrong slots.
    let base = address(0x1000);
    let rows = [row(base, 0x20, 8)];
    assert!(live_at(&rows, address(0x1021)).is_empty(), "one byte past");
    assert!(
        live_at(&rows, address(0x101F)).is_empty(),
        "one byte before"
    );
    assert_eq!(live_at(&rows, address(0x1020)), vec![8]);
}

#[test]
fn rows_from_different_functions_do_not_collide() {
    // Two functions can have a safepoint at the same *offset*; only the absolute address
    // distinguishes them.
    let rows = [
        row(address(0x1000), 0x20, 8),
        row(address(0x2000), 0x20, 16),
    ];
    assert_eq!(live_at(&rows, address(0x1020)), vec![8]);
    assert_eq!(live_at(&rows, address(0x2020)), vec![16]);
}

#[test]
fn an_empty_table_is_not_a_special_case() {
    assert!(live_at(&[], address(0x1020)).is_empty());
}

#[test]
fn walking_the_stack_terminates_and_stays_bounded() {
    // The walk follows a pointer chain through memory it does not own, so the property that
    // matters is that it *stops*: on a null or unaligned frame pointer, on a chain that does
    // not move outwards, and at the limit. A walker that ran off the end would read unmapped
    // memory rather than return a wrong answer.
    //
    // Rust does not guarantee frame pointers on every target, so this asserts termination and
    // the bound rather than a frame count — a count would be a claim about the host's calling
    // convention, not about this code.
    // SAFETY: this thread's own stack, not stopped at a safepoint but only walked, never
    // written, and the walk is bounded.
    let frames = unsafe { walk_frames(16) };
    assert!(frames.len() <= 16, "the limit is respected");
    for frame in &frames {
        assert!(!frame.return_address.is_null());
        assert!(!frame.base.is_null());
    }
}

#[test]
fn a_zero_limit_walks_nothing() {
    // SAFETY: as above; a zero limit performs no reads at all.
    let frames = unsafe { walk_frames(0) };
    assert!(frames.is_empty());
}

#[test]
fn compiled_roots_are_empty_when_no_program_registered_a_table() {
    // A test binary has no compiled program in it. Returning nothing is right; the alternative
    // would be reading a table that does not exist.
    // SAFETY: no table is registered, so this returns before walking anything.
    let roots = unsafe { crisol_abi::compiled_roots(16) };
    assert!(roots.is_empty());
}

// ---- handing the roots to the collector ------------------------------------------------

/// Installing a root provider must not turn the collector into one that keeps everything.
///
/// This is the failure that would hide the most: a provider that over-reports still passes
/// every "does it survive" test, and the leak shows up only as memory that never comes back.
#[test]
fn installing_the_provider_does_not_root_everything() {
    use crisol_gc::Heap;
    use crisol_value::Shapes;

    let shapes = Shapes::new();
    let heap = Heap::new();
    // SAFETY: nothing compiled is running, so no collection can observe a non-safepoint stack.
    unsafe { crisol_abi::install_compiled_roots(&heap) };
    assert!(heap.has_extra_roots());

    {
        let scope = heap.scope();
        scope.alloc(shapes.root(), 0);
    }
    // No compiled frames exist in this process, so the walk has nothing to report and an
    // unrooted object must still die.
    assert_eq!(heap.collect().swept, 1);
    assert_eq!(heap.live(), 0);
}

/// The walk runs against this test's own native stack, which has no registered stack maps.
///
/// It must come back empty rather than reading whatever the frames happen to contain — the
/// difference between "no roots" and "some stack garbage reinterpreted as handles".
#[test]
fn a_scan_with_no_registered_maps_yields_no_roots() {
    // SAFETY: as above.
    let roots = unsafe { crisol_abi::compiled_roots(crisol_abi::FRAME_LIMIT) };
    assert!(
        roots.is_empty(),
        "unregistered maps must mean no roots, not garbage"
    );
}
