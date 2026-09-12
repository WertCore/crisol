//! `crisol doctor` — what this machine and this build can do.
//!
//! At M18 this grows the part the roadmap actually cares about: reporting unsupported
//! JavaScript constructs across a whole dependency graph *before* a build fails
//! (ROADMAP §3.3). Until there is a compiler to ask, it reports the half that does exist —
//! the graphics stack, which is the thing most likely to be missing or broken on a given
//! machine, and the hardest for a user to diagnose without help.

use crisol_render_wgpu::Gpu;

/// `(id, name, complete)` for every milestone in ROADMAP.md.
///
/// Kept in sync with STATE.md by hand. A generated table would be less honest, not more:
/// the point is that someone had to decide a milestone was finished.
const MILESTONES: &[(&str, &str, bool)] = &[
    ("M0", "skeleton and session state", true),
    ("M1", "window and triangle", true),
    ("M2", "node tree and display list", true),
    ("M3", "CSS and layout", true),
    ("M4", "HTML and text", true),
    ("M5", "events, focus, input, accessibility", true),
    ("M6", "incremental everything", false),
    ("M7", "reactive API and component model", false),
    ("M8", "platform polish", false),
    ("M9", "GC and value representation", false),
    ("M10", "frontend and module graph", false),
    ("M11", "IR", false),
    ("M12", "runtime library", false),
    ("M13", "codegen", false),
    ("M14", "differential testing", false),
    ("M15", "async and event loop", false),
    ("M16", "DOM host API", false),
    ("M17", "React", false),
    ("M18", "npm compatibility", false),
    ("M19", "host APIs", false),
    ("M20", "optimization", false),
    ("M21", "production", false),
    ("M22", "mobile", false),
];

pub fn report() {
    println!("crisol {}", env!("CARGO_PKG_VERSION"));
    println!("  target   {}", current_target());
    println!();

    graphics();
    println!();
    milestones();
}

fn current_target() -> String {
    // `std::env::consts` is what is available without a build script. It is enough to tell
    // an arm64 mac from an x86_64 one, which is the distinction that matters here.
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn graphics() {
    println!("graphics");
    match Gpu::headless() {
        Ok(gpu) => {
            let info = gpu.adapter.get_info();
            println!("  adapter  {} ({:?})", info.name, info.device_type);
            println!("  backend  {:?}", info.backend);
            // Metal reports neither; printing two empty fields looks like a failure.
            let driver = format!("{} {}", info.driver, info.driver_info);
            if !driver.trim().is_empty() {
                println!("  driver   {}", driver.trim());
            }
            println!("  status   ok");
        }
        Err(error) => {
            println!("  status   unavailable");
            println!("  reason   {error}");
            println!();
            println!("  crisol needs Vulkan, Metal or D3D12. On a headless Linux machine,");
            println!("  install mesa-vulkan-drivers for a software rasteriser.");
        }
    }
}

fn milestones() {
    let done = MILESTONES.iter().filter(|(_, _, done)| *done).count();
    println!("milestones ({done}/{} complete)", MILESTONES.len());
    for (id, name, done) in MILESTONES {
        let mark = if *done { "done" } else { "    " };
        println!("  {mark}  {id:<4} {name}");
    }
}
