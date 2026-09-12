# Crisol — State

**Current milestone:** M4 — HTML and text (not started)
**Last finished:** M3 — CSS and layout

Read this before `ROADMAP.md`. The roadmap is the destination; this is where the work
actually is.

---

## Accept criteria for the current milestone

> **M4 — HTML and text.** `html5ever` producing the node tree. `cosmic-text` shaping,
> `glyphon` rendering. Font loading and fallback chains. Line breaking.
>
> **Public text API** (ROADMAP §2.5): shaped run access, cluster boundaries,
> `point_to_cursor`, `cursor_to_point`, selection rectangles for a range, line box geometry.
>
> **Accept:** renders a paragraph with mixed Latin/CJK/emoji correctly; clicking any glyph
> returns the correct cursor index including at cluster boundaries; selection rectangles are
> correct across a line wrap.
>
> Budget 2–3x whatever this seems like it should take. Bidi can be deferred to M8 but the
> API must not assume LTR.

---

## Done

### M0 — skeleton and session state

- Cargo workspace, 26 crates, laid out per ROADMAP §4. Edition 2024, MSRV 1.87, toolchain
  pinned in `rust-toolchain.toml`.
- Workspace-inherited package metadata and lints. `missing_docs` is on everywhere and
  promoted to an error in CI.
- `DECISIONS.md` seeded from ROADMAP §2 and §3 (D-01…D-09), plus the decisions the
  implementation forced (D-10…D-19).
- `README.md`, `.gitignore`, `rustfmt.toml`.
- CI workflow at `.github/workflows/ci.yml`: fmt, clippy `-D warnings`, test and rustdoc on
  macOS, Linux and Windows, plus a `cargo check` of the two arm64 mobile targets on every
  commit so a desktop-only assumption fails the day it is introduced (D-09).

**Accept: met.** CI is green on macOS arm64, Linux x86_64 and Windows x86_64, plus the
`aarch64-apple-ios` and `aarch64-linux-android` target checks.

### M1 — window and triangle

- `crisol-display-list`: geometry (`Point`, `Size`, `Rect`, `Corners`, `Edges`, `Color`),
  `DrawCommand`, `DisplayList`, `DisplayListBuilder` with a clip stack and CPU-side culling.
- `crisol-render-wgpu`:
  - `Gpu` — instance, adapter, device, queue. Requests the WebGPU downlevel baseline rather
    than whatever the local adapter offers, so a desktop-only limit fails here and not at
    M22.
  - `Renderer` — one instanced pipeline for every rounded/bordered rectangle, a second for
    textured quads, batched into one draw call per clip group (D-14). `FrameStats` counters.
  - `WindowSurface` — `winit` window, surface configuration, resize, scale factor,
    swapchain recovery from `Outdated`/`Lost`.
  - `HeadlessTarget` + `Pixels` — offscreen rendering and readback, which is what makes the
    renderer testable without a display.
  - `shaders/draw.wgsl` — signed-distance rounded box with per-edge borders, analytic
    coverage antialiasing, sRGB→linear conversion and premultiplied output (D-15).
- `examples/window.rs` — a real window with fills, radii, borders, a clip group and a
  textured quad.

**Accept:** 16 offscreen render tests in `renderer/wgpu/tests/render.rs`, including the 1x
vs 2x DPI equivalence the milestone asks for, a fractional (1.5x) scale case, and an
assertion that half-alpha white over black lands at sRGB ~188 rather than 128 — the colour
bug that is invisible until someone compares against a design. A real window was opened and
verified on macOS arm64 (Apple M2, Metal, 640x400 logical → 1280x800 physical at 2x).

The offscreen tests run on every CI platform and are not skipped anywhere:
`CRISOL_REQUIRE_GPU=1` turns a missing adapter into a failure, and the Linux runner
executes them against the lavapipe software rasteriser. So the pixel assertions — including
the linear-light blending one — are verified on Metal, on Vulkan and on Windows, not just on
the machine they were written on.

### M2 — node tree and display list

- `crisol-tree`:
  - Arena with generational `NodeId` (8 bytes; `Option<NodeId>` also 8). A stale handle
    fails its liveness check rather than resolving to whatever reused its slot (D-17).
  - `first_child`/`last_child`/`prev_sibling`/`next_sibling` intrusive links, not
    `Vec<NodeId>`, so a mid-list insert or remove is O(1).
  - `append_child`, `insert_before`, `remove_child`, `replace_child`, `detach`,
    `remove_subtree`. Cycle and stale-handle rejection returns `TreeError` rather than
    panicking, because at M16 these become DOM exceptions.
  - `DirtyFlags` with self and subtree bits, implication expansion (`STYLE` ⇒ `LAYOUT` ⇒
    `PAINT`), and an ancestor walk that stops at the first ancestor that already knows
    (D-18).
  - `CustomNode`: `measure` / `layout` / `paint` / `hit_test` (D-06, D-19). `ColorBox` is
    the stub implementation and doubles as the test fixture.
  - `TreeStats` counters.
- `crisol-paint`: iterative tree walk producing a display list. Absolute-position
  accumulation, `overflow: hidden` clip groups, `visibility` handling, engine-owned clipping
  around custom nodes, `PaintStats` counters.
- `crisol-ui`: umbrella re-exporting Track A, with the renderer behind a default-on `render`
  feature (D-11). `examples/tree.rs` runs the whole `tree → paint → display-list → render`
  path in a window.
- `crisol` CLI: `build | dev | run | check | package | doctor`. `doctor` is real today —
  it reports the platform, the GPU adapter and backend, and the milestone table. The rest
  exit with code 2 and name the milestone that implements them.

**Accept:** `ui/paint/tests/paint.rs` (16 tests) asserts the display list for a three-level
nested tree at absolute positions and after a node is removed; `ui/paint/tests/render.rs`
(5 tests) asserts the same through the GPU to pixels. `ui/tree/tests/tree.rs` (21 tests)
covers the structural guarantees.

### M3 — CSS and layout

Four steps, landed as three pull requests.

*(a) Selector matching — `crisol-css`.* `crisol_tree::Atom`; `ElementData` with `id`,
`classes`, `attributes` and an `ElementState` bitflag set; `CrisolSelectors` with a closed
pseudo-class allowlist (D-20); `ElementRef` as the `selectors::Element` adapter;
`parse_selector_list` / `matches` / `MatchCaches`.

*(b) Stylesheet parsing — `crisol-css`.* lightningcss for the grammar; selectors re-read into
our dialect; shorthands flattened to longhands at parse time so the cascade compares like
with like; `!important` sorted last within a rule; unsupported rules kept as warnings.
Nesting is lowered by printing the sheet with nesting disabled and reading it back, because
lightningcss implements that transform in its printer rather than its rule tree.

*(c) The cascade — `crisol-style`.* `ComputedStyle` for the whole M3 property subset,
`Eq + Hash` so it can be a map key; `StyleInterner`; `StyleEngine::restyle` with precedence
as `(important, origin, specificity, source order)`; inheritance through text nodes; `em`
and `rem` resolved, percentages left for layout (D-22); a one-rule user-agent stylesheet
(D-25). `crisol_tree::NodeMap<T>` is the side table computed style lives in (D-21).

*(d) Layout — `crisol-layout`.* taffy 0.14 driven over the Crisol tree through its trait
API, so there is no second tree and **no second per-node style allocation**: `StyleRef`
implements taffy's style traits directly over `ComputedStyle`. `CustomNode::measure` is wired
to taffy's leaf measure function, custom nodes are replaced elements (D-24), and a custom
node is a real element with a painter rather than a thing outside the document (D-23).
Layout writes each node's border box into `Node::layout` and projects computed style onto
the `BoxStyle` paint reads — which is where a percentage `border-radius` finally becomes a
number of pixels.

**Accept: met.**

- *Layout snapshot suite of 40+ cases* — **54 cases** in `ui/layout/tests/{block,flex}.rs`,
  written as box-tree snapshots so a failure shows what moved rather than which number
  changed.
- *Interning verified by asserting that 100 identically-styled nodes share one
  `ComputedStyle` allocation* — `a_hundred_identically_styled_nodes_share_one_allocation`:
  101 elements cost 2 allocations and 99 interner hits.

**Totals:** 233 tests passing, 0 failing. `cargo clippy --workspace --all-targets
--all-features -- -D warnings` clean, `cargo fmt --all --check` clean.

---

## In progress

Nothing. M3 is closed and M4 has not been started.

## Open questions

- **M1's "window opens on all three platforms" is verified on macOS only.** A window was
  opened and looked at by hand there; nobody has done that on Windows or Linux. Everything
  short of the window — device acquisition, pipelines, rasterisation, pixel output — is
  covered offscreen on all three in CI, so what is untested is specifically the `winit`
  surface and swapchain path.
- **A full `cargo test --workspace` no longer fits on the development machine's disk.**
  wgpu, naga and lightningcss together overflow it. Local runs go in two halves — the
  graphics crates and everything else — and CI runs the whole thing. Nothing about the code
  requires this; it is a note so the next session does not rediscover it as a mysterious
  linker failure.
- **`DisplayList` has no transform command.** Rounded clipping landed (D-26); transforms did
  not, and are not in any milestone's property subset yet.
- **Per-corner inner border radii are approximated.** The shader shrinks a corner's inner
  radius by the thicker of its two adjacent borders; CSS uses per-axis elliptical radii. The
  difference shows only on a box with very different adjacent border widths and a large
  radius. Revisit if a real design hits it.
- **Only one rounded clip is honoured at a time** (D-26). Nested rounded clips keep the
  innermost corners and intersect only their bounds. A test pins the behaviour.
- **`BoxStyle` is a placeholder producer, not a placeholder contract.** M3's cascade
  computes into it; the struct itself should survive. If `ComputedStyle` ends up wanting to
  be the thing stored on the node, that is a change to `crisol-tree` and should be recorded
  as a decision.

---

## Decisions made this session

Appended to `DECISIONS.md` in full; summarised here.

- **D-10** — edition 2024, workspace-inherited metadata and lints, internal crates declared
  with both `path` and `version` so publishing later does not mean touching 26 manifests.
- **D-11** — the umbrella crate gates the renderer behind a `render` feature, so a headless
  consumer does not compile `wgpu` and `winit`.
- **D-12** — geometry lives in `crisol-display-list` rather than a separate `crisol-geom`.
- **D-13** — the renderer consumes a display list and knows nothing about the tree.
- **D-14** — one instanced pipeline and one draw call per clip group, decided at M1 because
  retrofitting it at M22 would be a rewrite.
- **D-15** — straight sRGB colours in the display list, premultiplied linear out of the
  shader, sRGB surface format, linear blending.
- **D-16** — Android uses `GameActivity`, not `NativeActivity`. Forced by the first CI run:
  `android-activity` will not compile without the choice, and `NativeActivity` cannot
  properly drive the IME. Twenty-one milestones before the Android port, which is the
  per-commit mobile check doing exactly what ROADMAP §3.6 asks of it.
- **D-17** — generational `NodeId`, intrusive sibling links, no `Vec<NodeId>` children.
- **D-18** — dirty tracking as per-node flags plus ancestor-marked subtree bits.
- **D-19** — `CustomNode` as an object-safe measure/layout/paint/hit-test contract, with
  `measure` taking constraints rather than a fixed size.
- **D-20** — match with the upstream `selectors` crate rather than lightningcss's embedded
  `parcel_selectors`, whose `SelectorImpl` is unnameable from outside. Owning the impl means
  owning the dialect: the pseudo-class list is a closed allowlist mirroring
  `ElementState`, and anything else is a parse error rather than a silent non-match.
- **D-21** — computed style is an interned `Arc` in a `NodeMap` side table, not a field on
  the node. Forces `ComputedStyle` to be `Eq + Hash`, which is why lengths are newtypes that
  reject NaN and normalise negative zero.
- **D-22** — percentages reach layout unresolved; `em` and `rem` do not. `em` inside
  `font-size` means the parent's, everywhere else it means this element's. `line-height`
  inherits as a multiple, not as a resolved length.
- **D-23** — a custom node is an element with a painter attached, not a thing outside the
  document. Without a tag it matched no selector and could not be given a `width` or an
  `overflow`, which is most of what ROADMAP §2.6 asks the escape hatch to support.
- **D-24** — custom nodes are *replaced* elements: `width: auto` takes the intrinsic size
  their `measure` reported instead of stretching to the container. CSS still overrides it.
- **D-25** — a one-rule user-agent stylesheet, `:root { width: 100%; height: 100% }`, and an
  explanation of why exactly one rule qualifies.

---

## Next session

1. Read `DECISIONS.md` and this file.
2. Start M4, and read ROADMAP §2.5 first. Text is the one milestone the roadmap explicitly
   says to over-budget for, and it is the core competency rather than a checkbox: the text
   layer is a *public API*, not an internal detail.
3. Suggested order:
   a. `crisol-html`: `html5ever` into the tree. Small, and it makes every later test easier
      to write — the layout suite currently builds documents through a bespoke fixture
      because there is no parser yet.
   b. `crisol-text`: `cosmic-text` shaping behind an API that exposes shaped runs, cluster
      boundaries, `point_to_cursor`, `cursor_to_point`, selection rectangles and line box
      geometry. Design the API before the implementation; §2.5 is the requirement, and it
      must not assume LTR even though bidi can be deferred to M8.
   c. `crisol-text-gpu`: `glyphon` for the atlas and the draw.
   d. Wire text measurement into `measure_leaf` in `crisol-layout`, which currently returns
      a zero size for every text node and has a test saying so.
