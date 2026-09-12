# Crisol — State

**Current milestone:** M5 — events, focus, input, accessibility (in progress)
**Last finished:** M4 — HTML and text

Read this before `ROADMAP.md`. The roadmap is the destination; this is where the work
actually is.

---

## Accept criteria for the current milestone

> **M5 — events, focus, input, accessibility.** All four together, because they read the same
> tree and focus state.
>
> Hit testing (respecting clip and transform), capture/bubble propagation, focus order and
> keyboard navigation, text input with IME preedit rendering, mouse/touch/scroll/drag,
> clipboard, and the `accesskit` bridge.
>
> **Accept:** a form with three text inputs is fully keyboard-navigable; IME composition works
> for Japanese input on all three platforms; VoiceOver/NVDA/Orca announce the tree correctly.
>
> Accessibility here, not in year three — it constrains the tree, focus model and event
> system, and retrofitting means restructuring. Touch input designed in now (ROADMAP §3.6),
> even though mobile ships later.

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

### M4 — HTML and text

*`crisol-html`.* The `TreeSink` driving html5ever into the tree. Comments, processing
instructions and doctypes are dropped rather than stored — the tree is what the engine
renders, not an archive of the source. Rooted at `<html>`, because CSS's `:root` means
`<html>`.

*`crisol-text`.* The public API §2.5 requires: shaped runs, cluster boundaries,
`point_to_cursor`, `cursor_to_point`, selection rectangles, line box geometry. cosmic-text
shapes; the vocabulary is ours, and three parts of it are long-lived commitments (D-28):
byte-offset positions, cursors that carry affinity, and `Direction` on every run even though
bidi layout is M8's.

*Measurement.* Shaping is wired into taffy's leaf measure, and the shaped result is kept in a
side table for paint rather than reshaped.

*`crisol-text-gpu` and rendering.* glyphon for the atlas and the draw, with one renderer per
text run so text keeps painter's order inside the engine's single render pass (D-30). The
display list refers to text by the node's own packed handle, exactly as it refers to images
(D-31).

**Accept: met.**

- *renders a paragraph with mixed Latin/CJK/emoji correctly* —
  `mixed_scripts_shape_without_losing_any_text` and `runs_split_where_the_font_changes`.
- *clicking any glyph returns the correct cursor index including at cluster boundaries* —
  `hit_testing_a_multi_byte_character_never_lands_inside_it` sweeps a whole line at half-pixel
  steps and asserts every answer is a legal caret position.
- *selection rectangles are correct across a line wrap* —
  `a_selection_across_a_wrap_produces_one_rectangle_per_line`.

And `ui/paint/tests/text.rs` runs the whole of Track A end to end — parse, cascade, layout,
shape, paint, rasterise — and looks at the pixels.

**Totals:** 334 tests passing, 0 failing.  clean,  clean,  clean with
`RUSTDOCFLAGS=-D warnings`.

---

## In progress

**M5, hit testing and the event model.**

*Hit testing.* A point to a node, walking in reverse paint order — last sibling first,
children before parents — because the last thing painted is the top thing on screen.
Respects clips, `visibility: hidden` and `display: none`; a custom node resolves the point
itself and may declare it a miss, which is how it says it has holes (D-19). With a text
lookup it also returns the cursor position, composing straight into "which character did the
user click".

*The event model (D-32).* Pointers, not mice: a finger, a stylus and a mouse all produce
`PointerEvent`s that differ by `PointerKind`, so an application written against a mouse today
is already written against a finger. `PointerCancel` is its own event rather than a variant
of `PointerUp`, and `TextInput` is separate from `KeyDown` because a key press may produce no
text and an IME produces text with no key press.

*Dispatch.* Capture down, target, bubble up, with `stop_propagation`,
`stop_immediate_propagation` and `prevent_default`. `Listener` is a trait rather than a boxed
closure, because at M16 a listener is a JavaScript function (D-33).

*The payoff.* `EventSystem::apply_state` writes the `ElementState` bits M3 put on the node
and nothing has written until now — the cascade has been matching `:hover`, `:focus`,
`:focus-within` and `:active` against them all along and always getting `false`.
`a_hover_selector_matches_once_the_state_is_applied` moves a pointer, applies the state,
restyles, and watches the background change from white to red.

37 tests.

**Still to do for M5:** focus order and keyboard navigation (Tab through a form, which is
what the milestone's acceptance names), IME preedit, and the `accesskit` bridge.

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
- **D-26** — rounded clipping as a per-fragment test rather than a stencil pass, because a
  stencil costs an attachment and a second pass over the clipped geometry.
- **D-27** — borders carry a colour per edge, meeting on the miter diagonal.
- **D-28** — byte-offset positions, cursors with affinity, `Direction` on every run, clusters
  as byte ranges.
- **D-29** — text tests assert relations rather than pixel positions, because the installed
  fonts differ between machines.
- **D-30** — one glyphon renderer per text run, so text keeps painter's order without a
  second render pass.
- **D-31** — the display list refers to text by handle, exactly as it does to images.
- **D-32** — pointers, not mice; `PointerCancel` distinct from `PointerUp`; `TextInput`
  distinct from `KeyDown`.
- **D-33** — listeners are a trait rather than a boxed closure, and interaction state is a
  side table that `apply_state` writes onto the node for the cascade to read.

---

## Next session

1. Read `DECISIONS.md` and this file.
2. Start M5. Read §M5's note first: accessibility belongs *here*, not in year three, because
   it constrains the tree, the focus model and the event system, and retrofitting means
   restructuring. Touch is designed in now too (§3.6), even though mobile ships at M22. Text is the one milestone the roadmap explicitly
   says to over-budget for, and it is the core competency rather than a checkbox: the text
   layer is a *public API*, not an internal detail.
3. Suggested order, because each step makes the next testable:
   a. Hit testing in `crisol-events`: a point to a node, respecting clips. `crisol-text`
      already resolves a point *within* a text block to a cursor, so the two compose into
      "which character did the user click" as soon as the first exists.
   b. The event model: capture/bubble over the tree, with touch and pointer cancellation in
      the vocabulary from the start rather than added for M22.
   c. Focus: order, keyboard navigation, and the `ElementState` bits that M3 put on the node
      and nothing has written yet — `:hover`, `:focus`, `:focus-within`, `:active` are all
      matched by the cascade already and all currently always false.
   d. IME preedit, then the `accesskit` bridge.
