//! The in-process memory probe must agree with the tool macOS ships.
//!
//! Its own integration test, and that is the point: `cargo test` runs unit tests as threads
//! in one process, and `measure`'s own unit tests allocate 64 MiB to prove the probe moves.
//! Run alongside those, this compared a reading taken at one instant with `vmmap`'s view at
//! another and reported a 63 MiB disagreement that was entirely the neighbouring test's
//! allocation. An integration test gets a process to itself, so the only thing changing
//! memory here is this file.
//!
//! Without this check the probe is a plausible number from a struct offset nobody verified
//! against the platform — and a memory instrument that is quietly wrong is worse than none,
//! because the figure still gets quoted.

#![cfg(all(feature = "measure", target_os = "macos"))]

use crisol_ui::measure::{Metric, current};

#[test]
fn the_reading_agrees_with_vmmap() {
    let Some(mine) = current() else {
        panic!("macOS is a platform this is implemented for");
    };
    assert_eq!(mine.metric, Metric::MachPhysFootprint);

    let Ok(output) = std::process::Command::new("/usr/bin/vmmap")
        .args(["--summary", &std::process::id().to_string()])
        .output()
    else {
        // `vmmap` needs privileges that are not guaranteed on a build machine. A test that
        // could not run is not a test that failed.
        eprintln!("vmmap could not be run; skipping");
        return;
    };
    if !output.status.success() {
        eprintln!("vmmap refused; skipping");
        return;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let Some(theirs) = text
        .lines()
        .find_map(|line| line.strip_prefix("Physical footprint:"))
        .and_then(|value| parse_vmmap_size(value.trim()))
    else {
        eprintln!("vmmap printed no footprint line; skipping");
        return;
    };

    // Take a second reading afterwards: running `vmmap` itself allocates, so the honest
    // comparison is against the range this process occupied across the call rather than
    // against a single instant on one side of it.
    let after = current().expect("still macOS").bytes;
    let low = mine.bytes.min(after);
    let high = mine.bytes.max(after);

    // Distance outside the bracket, zero when vmmap's figure falls inside it.
    let distance = low.saturating_sub(theirs).max(theirs.saturating_sub(high));
    // 5%, or 512 KiB for a process too small for a percentage to mean anything.
    //
    // The first version allowed a quarter, with a 4 MiB floor — and 4 MiB is larger than this
    // whole test process, which weighs about 1.7 MiB. Any wrong field reading near zero sat
    // comfortably inside that, and swapping `phys_footprint` for its neighbour
    // `compressed_lifetime` passed. A tolerance wider than the quantity is not a tolerance.
    //
    // Measured, the two readings agree to within a few hundred bytes, so this is still three
    // orders of magnitude of slack for the fact that they are not simultaneous.
    let tolerance = (high / 20).max(512 * 1024);
    assert!(
        distance <= tolerance,
        "in-process {mine} is outside vmmap's {:.1} MiB by {:.1} MiB",
        mib(theirs),
        mib(distance),
    );
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Turns vmmap's `26.2M` into bytes.
fn parse_vmmap_size(text: &str) -> Option<u64> {
    let (number, scale) = match text.as_bytes().last()? {
        b'K' => (&text[..text.len() - 1], 1024.0),
        b'M' => (&text[..text.len() - 1], 1024.0 * 1024.0),
        b'G' => (&text[..text.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (text, 1.0),
    };
    let value: f64 = number.trim().parse().ok()?;
    if value < 0.0 {
        return None;
    }
    // A size is never negative and never larger than a machine's address space, so the cast
    // cannot be lossy in a way that matters here.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((value * scale) as u64)
}
