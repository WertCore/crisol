# Crisol — State

**Current milestone:** M2 — node tree and display list (not started)
**Last finished:** M1 — window and triangle

Read this before `ROADMAP.md`. The roadmap is the destination; this is where the work
actually is.

---

## Accept criteria for the current milestone

> **M2 — node tree and display list.** Arena-allocated node tree with generational `NodeId`
> handles. Parent/first-child/next-sibling links (**not** `Vec<NodeId>`). `DrawCommand`
> enum. A hand-built tree of nested coloured rectangles renders through the display list.
>
> **Accept:** manually constructed 3-level nested tree renders at correct positions;
> removing a node and re-rendering produces the expected output.
>
> Define the `CustomNode` trait here (ROADMAP §2.6): `measure`, `layout`, `paint`,
> `hit_test`, with a stub that draws a fixed-size coloured box.

---

## Done

### M0 — skeleton and session state

Workspace, 26 crates per ROADMAP §4, `DECISIONS.md`, CI on macOS/Linux/Windows plus a
per-commit `cargo check` of the two arm64 mobile targets.

### M1 — window and triangle

- `crisol-display-list`: geometry (`Point`, `Size`, `Rect`, `Corners`, `Edges`, `Color`),
  `DrawCommand`, `DisplayList`, and a `DisplayListBuilder` that owns the clip stack and
  culls commands that cannot reach the framebuffer.
- `crisol-render-wgpu`:
  - `Gpu` — instance, adapter, device, queue. Requests the WebGPU downlevel baseline rather
    than whatever the local adapter offers, so a desktop-only limit fails here and not at
    M22.
  - `Renderer` — one instanced pipeline for every rounded and bordered rectangle, a second
    for textured quads, batched into one draw call per clip group (D-14). `FrameStats`
    counters.
  - `WindowSurface` — `winit` window, surface configuration, resize, scale factor, and
    swapchain recovery from `Outdated`/`Lost`.
  - `HeadlessTarget` and `Pixels` — offscreen rendering and readback, which is what makes
    the renderer testable on a machine with no display.
  - `shaders/draw.wgsl` — signed-distance rounded box with per-edge borders, analytic
    coverage antialiasing, sRGB-to-linear conversion and premultiplied output (D-15).
- `examples/window.rs` — a real window with fills, radii, borders, a clip group and a
  textured quad.

**Accept:** 16 offscreen render tests in `renderer/wgpu/tests/render.rs`, including the 1x
versus 2x DPI equivalence the milestone asks for, a fractional 1.5x case, and an assertion
that half-alpha white over black lands at sRGB ~188 rather than 128 — the colour-space bug
that stays invisible until someone compares against a design. A real window was opened and
verified on macOS arm64 (Apple M2, Metal, 640x400 logical to 1280x800 physical at 2x).

---

## In progress

Nothing.

---

## Open questions

- **The window half of M1's accept is verified on macOS only.** Windows and Linux windows
  have not been opened by hand. The offscreen half runs anywhere and is what CI gates on;
  `CRISOL_REQUIRE_GPU=1` turns a missing adapter from a skip into a failure, and CI sets it.
- **`DisplayList` has no transform command, and clipping is axis-aligned scissor only.**
  A rounded clip needs either a stencil pass or a per-fragment test. Decide at M3, when the
  CSS that needs it arrives, rather than guessing now.
- **Per-corner inner border radii are approximated.** The shader shrinks a corner's inner
  radius by the thicker of its two adjacent borders; CSS uses per-axis elliptical radii. The
  difference shows only on a box with very different adjacent border widths and a large
  radius.

---

## Decisions made this session

- **D-12** — geometry lives in `crisol-display-list` rather than a separate `crisol-geom`.
- **D-13** — the renderer consumes a display list and knows nothing about the tree.
- **D-14** — one instanced pipeline and one draw call per clip group, decided now because
  retrofitting it at M22 would be a rewrite.
- **D-15** — straight sRGB colours in the display list, premultiplied linear out of the
  shader, sRGB surface format, linear blending.

---

## Next session

1. Read `DECISIONS.md` and this file.
2. Start M2 in `ui/tree` and `ui/paint`. The `CustomNode` trait is the part to get right:
   ROADMAP §2.6 calls it the single most important node kind for the eventual product and
   the hardest to retrofit.
