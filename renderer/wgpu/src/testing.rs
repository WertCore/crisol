//! Helpers for render tests.
//!
//! Public rather than `#[cfg(test)]` because the crates above this one — paint at M2, text
//! at M4 — need the same "get a device or say why not" logic for their own snapshot tests.

use crate::gpu::Gpu;

/// Set this to `1` to make a missing GPU a test failure instead of a skip.
///
/// CI sets it: a run where every render test silently skipped is indistinguishable from a
/// run where they all passed, and that is exactly the failure mode a graphics test suite
/// drifts into.
pub const REQUIRE_GPU_ENV: &str = "CRISOL_REQUIRE_GPU";

/// Acquires a device, or returns `None` after printing why.
///
/// # Panics
///
/// Panics instead of returning `None` when `CRISOL_REQUIRE_GPU=1`.
#[must_use]
pub fn gpu_or_skip() -> Option<Gpu> {
    match Gpu::headless() {
        Ok(gpu) => Some(gpu),
        Err(error) => {
            let required = std::env::var(REQUIRE_GPU_ENV).is_ok_and(|v| v == "1");
            assert!(
                !required,
                "{REQUIRE_GPU_ENV}=1 but no GPU adapter is available: {error}"
            );
            eprintln!(
                "skipping: no GPU adapter ({error}). \
                 Set {REQUIRE_GPU_ENV}=1 to make this a failure."
            );
            None
        }
    }
}

/// Runs `body` with a device, or skips when there is none.
///
/// ```no_run
/// crisol_render_wgpu::testing::with_gpu(|gpu| {
///     let target = crisol_render_wgpu::HeadlessTarget::new(&gpu, 8, 8);
///     assert_eq!(target.width(), 8);
/// });
/// ```
pub fn with_gpu(body: impl FnOnce(Gpu)) {
    if let Some(gpu) = gpu_or_skip() {
        body(gpu);
    }
}

/// A checkerboard of two colours, as straight-alpha sRGB RGBA bytes.
///
/// Useful as a test texture: it makes both the sampler's addressing and the destination
/// rectangle's orientation visible in a readback, which a flat colour would not.
#[must_use]
pub fn checkerboard(width: u32, height: u32, cell: u32, a: [u8; 4], b: [u8; 4]) -> Vec<u8> {
    let cell = cell.max(1);
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let dark = ((x / cell) + (y / cell)).is_multiple_of(2);
            pixels.extend_from_slice(if dark { &a } else { &b });
        }
    }
    pixels
}
