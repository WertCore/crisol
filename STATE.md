# Crisol — State

**Current milestone:** M13 — codegen (M12's surface is complete; its acceptance is blocked on M13, see below)
**Last finished:** M8 — platform polish, **acceptance met at ~10.5 MiB against a 60 MB budget**

Read this before `ROADMAP.md`. The roadmap is the destination; this is where the work
actually is.

---

## Accept criteria for the current milestone

> **M8 — Platform polish.** Momentum scrolling, scrollbars, multi-window, native menus, drag
> and drop, bidi text, cursor shapes, window chrome, packaging (.app, .msi, AppImage).
>
> **Accept:** a real API-client-shaped application built entirely in Rust, measured at
> < 60MB RSS idle with a 5MB JSON response loaded.
>
> **Notes:** Record the memory number. It is the product claim and needs to be defensible.

**§7's first kill criterion lands here**, and it is the acceptance rather than a footnote to
it: *if idle RSS is not meaningfully below a WebView2/WKWebView baseline for an equivalent
app, the core product claim is unsupported. Re-evaluate.* The number has to be measured and
written down, including if it is bad.

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

### M5 — events, focus, input, accessibility

All four together, as §M5 requires, because they read the same tree and focus state.

*Hit testing.* Reverse paint order — last sibling first, children before parents — respecting
clips, `visibility: hidden` and `display: none`. A custom node resolves the point itself and
may declare it a miss (D-19). With a text lookup the hit carries a cursor, so "which character
did the user click" is the composition of two things that already existed.

*The event model (D-32).* **Pointers, not mice.** A finger, a stylus and a mouse all produce
`PointerEvent`s differing by `PointerKind`, so an application written against a mouse is
already written against a finger. `PointerCancel` is its own event, and `TextInput` is
separate from `KeyDown`.

*Dispatch (D-33).* Capture, target, bubble, with `stop_propagation`,
`stop_immediate_propagation` and `prevent_default`. `Listener` is a trait, because at M16 a
listener is a JavaScript function. `apply_state` writes the `ElementState` bits M3 put on the
node and nothing had written until now.

*Focus (D-34).* Document order. A positive `tabindex` is accepted and ignored, because
honouring it is how keyboard-unusable interfaces get built. `BoxStyle` gained `generates_box`,
which paint now uses to skip a `display: none` subtree outright.

*IME (D-35).* Composition as a state machine: provisional text stays out of the document, a
cancel is not an empty commit, and moving focus abandons what was being typed.

*Accessibility (D-36).* A second, smaller tree — only what a user can perceive and act on —
with skipped wrappers' children floating up to take their place.

**Accept: met, with one part that cannot be automated.**

- *a form with three text inputs is fully keyboard-navigable* —
  `tabbing_through_a_form_reaches_every_input_and_comes_back`.
- *IME composition works for Japanese input on all three platforms* — the state machine is
  tested with real Japanese composition sequences. Driving a platform input method is the
  windowing layer's job and needs a human; see *Open questions*.
- *VoiceOver/NVDA/Orca announce the tree correctly* — the `TreeUpdate` those readers consume
  is asserted: roles, labels, nesting, state, focus and bounds. Whether they *say* it right
  needs a human with a screen reader; see *Open questions*.

### M6 — incremental everything

`DirtyFlags` had been on the node since M2 waiting for a consumer. This is it.

*Marking (D-38).* A class or state change dirties the node, its descendants and its
**following** siblings — nothing else, because no combinator in the dialect looks backwards or
upwards. That is what excluding `:has()` bought (D-20). Text becoming empty dirties the
parent's style for `:empty`; an ordinary edit dirties no style at all. Inserting a child
dirties the parent's other children for `:nth-child`, but not their subtrees.

*Restyle.* `restyle_incremental` reuses whole subtrees. The subtle part is inheritance: a
clean subtree still needs recomputing if its parent's style changed, so the walk carries
whether the inherited style *actually* changed — a pointer comparison, thanks to the interner.

*Layout (D-37).* `LayoutCache` moved out to the caller, because the sequence M6 is about —
lay out, mutate, lay out — is impossible while the context holds `&mut Tree`. Found by writing
the acceptance test and discovering it could not be expressed.

*Damage (D-39, D-40, D-41).* The union of where each changed box was and is, in absolute
coordinates. Paint culls subtrees that cannot touch it and repaints the background inside it;
the renderer loads rather than clears and intersects every scissor with it.

**Accept: met.**

```
10,002 nodes; one text edit
  first pass:  10,002 laid out
  second pass: 4 caches invalidated, 4 laid out, 10,002 boxes unchanged
```

Four is the edited text node plus `p`, `body`, `html` — the ancestor chain exactly; the
milestone asks for fewer than twenty. `a_damaged_frame_redraws_inside_and_preserves_outside`
checks the other half at pixel level: the damaged region changes and the rest of the surface
survives.

*Glyph and texture caching*, also named by the milestone, were already in place — glyphon's
atlas persists across frames and is trimmed (D-30), and `ImageStore` holds uploads until
removed. Nothing new was needed.

**Totals:** 398 tests passing

### M7 — reactive API and component model

- **`crisol-dom`** — the mutation API ROADMAP §M7 asks to be designed *as if an external
  consumer exists*. Create, insert, move, remove, set text, set attributes, toggle classes,
  and read the tree back. Every write marks what a selector could notice; `Tree::element_mut`
  cannot, which is why the layer exists (D-42). Writes are compared first, so setting a value
  to what it already is costs nothing. `DomStats` counts what a caller did.
- **`crisol-reactive`** — `Signal`, `Memo`, effects, and the `Scope` tree that owns them.
  Memos are pull-based and lazy: one nobody reads is never computed, and a diamond wakes its
  effect once with both sides fresh rather than twice with one stale. Dependencies are
  rebuilt on every run, so a branch that stops reading a signal stops depending on it.
  Disposal is generational, so a handle outliving its scope is an error rather than a read of
  whoever took the slot.
- **The runtime is a value, not a thread-local** (D-43). Reads go through `Track` (pure) or
  `Cx` (reads plus the DOM), which makes "a memo may not touch the DOM" a type-level fact.
- **Components run once** (D-44). They build nodes and bind effects; nothing re-renders them.
  `Keyed` reconciles lists by key, anchoring the longest increasing subsequence so a row moved
  from the end to the front costs one move rather than a thousand.
- A new crate at `ui/reactive`, which ROADMAP §4's layout does not name. `crisol-dom` was
  already there.

**Accept: met.** 1,000 todos, 3,005 nodes, driven through the DOM API, M6's incremental
restyle, and layout:

```
  add                dom:    3 created,  0 removed  | list: 1 new, 0 moved, 1000 kept | layout:    8
  edit (item 500)    dom:    0 created,  1 text     | list: did not run               | layout:    6
  toggle (item 500)  dom:    0 created,  1 attr     | list: did not run               | layout:    8
  filter (active)    dom:    0 created,  3 removed  | list: 1 gone,  0 moved, 1000 kept | layout:  3
  filter (all)       dom:    3 created,  0 removed  | list: 1 new,   0 moved, 1000 kept | layout:  6
  remove (item 100)  dom:    0 created,  3 removed  | list: 1 gone,  0 moved, 1000 kept | layout:  5
```

Editing one todo's label runs **one** effect and writes **one** text node out of a thousand.
Toggling one runs two — its own class binding and the footer count — and the list reconciler
does not run at all, because under `All` the filter never reads `done` and so never subscribed
to it.

The test also asserts **node identity** across every step: a rebuild would produce nodes that
render identically, and only the handles show the difference. A final full restyle is compared
against what the incremental passes produced, so a missed invalidation cannot pass as a saving.

**Measured against M16's minimum surface**, since §M7 asks for this to be designed for that
consumer. `crisol-dom` has `createElement`, `createTextNode`, `appendChild`, `insertBefore`,
`removeChild`, `replaceChild`, `nodeType`, `parentNode`, `firstChild`, `nextSibling`,
`setAttribute`, `removeAttribute`, `classList` and the `style` attribute. Two things are
missing and each is real work rather than a wrapper:

- **`createElementNS`** — the tree has no namespace concept at all.
- **`addEventListener`** — `crisol-events` already dispatches with real capture and bubble and
  has `preventDefault`/`stopPropagation` (M5), but nothing registers listeners.

**`style` landed** (D-50), ahead of the other two because the virtualised response view could
not be built without it: a pane moves two spacers every scroll frame, and reparsing a
stylesheet to do that is not a frame budget. It is stored as text on `ElementData` and parsed
by the cascade at `Origin::Inline`, above author rules. What is *not* there yet is the CSSOM
object — `element.style.height = "4px"` needs a JS-side wrapper over `setAttribute`; the
engine side of it is done.

M16's batching note — *mark dirty, run one style/layout/paint pass per frame, never relayout
per `appendChild`* — is already how this works: nothing touches the tree between flushes, so a
frame sees one consistent state rather than a half-applied update.

**It also runs, which is the word the acceptance uses.**
`cargo run -p crisol-ui --example todo` opens a window with the whole of Track A behind it —
reactive state, the DOM API, the cascade, layout, shaping, paint, GPU — and prints the counters
on every keystroke, so the claim is checkable while the thing is running. `--headless` drives
the same code through a scripted sequence with no window and no GPU, and asserts what it should
have left behind. **CI runs that step**, because `cargo test` builds examples and never executes
them — so the key handling, selection and edit/commit paths would otherwise compile on three
platforms and run on none.

So the interaction paths are exercised on macOS, Linux and Windows. The *windowed* path is
verified on macOS only; Windows and Linux remain
[#15](https://github.com/WertCore/crisol/issues/15), which predates this and is unchanged by it.

**One defect this milestone existed to find.** `DirtyFlags::expanded` turned `STYLE` into
`LAYOUT`, so appending one row to a 1,000-row list invalidated **1,008** layout caches — every
sibling, because a structural change marks them all for `:nth-child`. It now invalidates 8.
The flag code looks obviously right; it was the node count coming back three orders of
magnitude too large that found it (D-45).

**Totals:** 439 tests passing

---

## M8 — platform polish, and its acceptance

Complete, acceptance included. Kept here rather than folded into **Done** above because the
acceptance is §7's first kill criterion and the reasoning behind the number should stay
where it can be read.

**M8 — platform polish.** Scrolling, momentum and scrollbars are done; the rest of the
deliverable is not.

- **Scrolling.** `Node` gains `scroll_max` (layout output, from taffy's scrollable overflow
  rectangle) and `scroll_offset` (user state, survives relayout). `overflow: scroll` is
  distinguished from `overflow: clip` by a flag on `BoxStyle`, not by asking whether the
  content is taller than the box — deriving it would make every clipped card with a long word
  in it scrollable. Paint and hit testing both displace children by the offset, and
  `absolute_rect` subtracts ancestors' scroll, since its caller is the accesskit bridge.
- **Chaining.** `scroll_by` returns how far it actually moved, so `scroll_at` can walk outward
  handing each container the remainder. A list that has hit its end lets the page underneath
  keep moving instead of swallowing the gesture.
- **Momentum.** `Fling` decays at the rate the platforms settle on, integrating over the
  frame rather than holding the speed for it — a fling that went further on a 120Hz display
  is a bug people feel without being able to name, and a test pins the two rates together.
  `VelocityTracker` estimates over a 100ms window, so one still frame before the lift does not
  swallow the flick. **Not for wheel events:** a macOS trackpad has already been through the
  system's momentum by the time winit reports it.
- **Scrollbars.** Engine-drawn and overlaid, so content does not reflow when one appears, and
  taffy is never asked to reserve room. Thumb length is the visible share of the content,
  floored at 24px — the proportional thumb for a hundred screens is about two pixels.

**The constraint the whole design is arranged around:** a scroll marks `PAINT` and never
`LAYOUT`. The example asserts it end to end:

```
  scrolled 60 of 368 available
  a frame after scrolling laid out 0 nodes
```

### Bidirectional text

M4 shipped the API carrying `Direction` and `Affinity` and deferred the layout. Four things
above cosmic-text were wrong, none of them visible in an all-Latin document: caret direction
came from the line's first run, affinity was ignored at a direction boundary, selection
rectangles never split, and a click just inside an RTL run could resolve into the Latin beside
it. Cursor movement needed nothing — it was already logical rather than visual.

The tests **fail rather than skip** when no font covers Hebrew; a machine without one shapes
the string to nothing and every assertion passes without testing anything.

### The memory number, measured early

§7's first kill criterion is M8's acceptance, so it was worth taking a reading before building
the rest of the milestone: if the number is bad, it changes what is worth building.

| | phys_footprint | RSS |
|---|---|---|
| wgpu window, **no text** (the GPU floor) | **17 MB** | 83 MB |
| the todo example, release, 27 rows | **25.5 MB** (peak 30.7) | 90 MB |
| WKWebView showing equivalent HTML/CSS | **36.0 MB** | — |

**Crisol is about 30% below the WebView baseline for an equivalent app**, and the baseline is
generous to WebView: this machine was already running Safari, so the new WebView reused
infrastructure it would otherwise have spawned.

**RSS is the wrong metric and reporting it would have been an error** (D-46). On macOS it
counts shared read-only library pages that every process pays for; the same binary reads 90 MB
by RSS and 25.5 MB by footprint. The Linux equivalent is PSS, the Windows one private working
set.

**Caveats, because this is not yet the acceptance.** macOS only. A 27-row list, not the
API-client-shaped application §M8 names. No 5MB JSON loaded. What it establishes is that the
engine's floor leaves roughly 34 MB of headroom, and that the floor is almost entirely wgpu
rather than anything crisol allocates — the whole tree, styles, layout and text for this app
cost about 8.5 MB over an empty GPU window.

**What the acceptance will turn on** is virtualising the response view (D-47), not shrinking
the engine: 5MB of JSON expanded one node per token is ~100k nodes and 31 MB of arena on its
own. `ui/tree/tests/sizes.rs` keeps `Node` (328 bytes) a tracked figure so that does not drift.

*Measured later, and that 31 MB was low by 24× — the real figure for 100k nodes is 762 MiB,
because arena is 6% of what a node costs once it is styled, laid out and shaped. It makes the
conclusion stronger and the target different; see "What a 5 MB response costs" below.*

### Cursor shapes

The CSS `cursor` property, every keyword of it. Lives on `ComputedStyle` rather than
`BoxStyle`: the former is `Arc`-interned (D-21) so a field there costs nothing per node, and
the memory measurement above had just established that `BoxStyle` is the largest part of a
`Node`.

Inherited, as CSS says — without that the pointer flickers back to an arrow as it crosses a
button's own label. `auto` is the only value whose meaning depends on what is under it, and
`resolve(over_text)` is what makes it an I-beam over text and an arrow elsewhere; an explicit
keyword beats it either way, because an author who wrote `cursor: default` meant the arrow.

Named `CursorIcon`, not `Cursor`: `crisol-events` already has a `Cursor` meaning a *caret
position in text*, and `hit.cursor` beside `style.cursor` meaning two unrelated things would be
a trap for whoever read it next.

`crisol_ui::platform_cursor` maps it to `cursor_icon::CursorIcon`, which is what
`winit::Window::set_cursor` takes. The mapping lives in the umbrella because `crisol-style`
must not know windows exist and `crisol-render-wgpu` must not know a cascade does.

### Multi-window

A second window duplicates what belongs to a window and nothing that belongs to the
application (D-48). Per application: the adapter, device and queue — the 17 MB the measurement
above attributes to the GPU — plus the font database, the style interner, and a renderer per
*surface format* rather than per window, so two windows on one display share a glyph atlas.
Per window: the surface, the tree, the layout cache, and the reactive runtime.

`WindowSurface::with_gpu` is the seam. The first window creates the device; every one after it
borrows the one already chosen, which also settles presentability — an adapter picked for one
window on a multi-GPU laptop is the one attached to that display.

**Verified by pointer identity, not by memory.** The example asserts `SharedGpu::ptr_eq`
between window 1's device and every later window's — the same allocation, three holders after
two windows.

That is deliberate. The first attempt measured footprints instead, and they would not hold
still: the same binary varied by several MB run to run under `ControlFlow::Wait` depending on
whether a redraw had landed before the sample, and a one-window build at four times the window
area measured *lower* than at one times. A number that cannot order two configurations known to
differ cannot show that a device is shared. The per-window cost stays unquantified until it can
be sampled deterministically.

### Runtime handles now carry their runtime's identity

The second window is what exposed this. D-43 rejected an ambient thread-local partly because
two windows would silently cross-wire; the design it chose had the same hole. A `Signal` was a
bare index, index 0 exists in every runtime, and reading window A's signal through window B's
runtime returned *B's* value. Disabling the fix makes the test say `left: Some(99), right:
None`.

Handles carry the id of the issuing runtime, checked on every read, write and disposal. Four
bytes on a `Copy` handle. D-43 is amended accordingly rather than quietly patched — the
decision stands, but its safety argument was doing less work than it claimed.

Worth recording how the scope half of that was nearly missed: the first version of the disposal
test passed with the guard removed, because the second runtime had no scope at that index and
refused for lack of one. It only tests the guard now that *both* runtimes open a scope.

### The memory instrument

M8's acceptance is a memory measurement on three platforms, so the measurement itself had to
be worth something first. `crisol_ui::measure` reads this process's own memory, behind a
`measure` feature that is off by default.

**Why in-process.** Sampling from outside with `footprint(1)` cannot name the moment it
samples, and a GUI process under `ControlFlow::Wait` differs by megabytes either side of its
first frame. That is not a theoretical worry: it is what produced the contradiction in the
multi-window work, where a one-window build at four times the window area read *lower* than the
same build at one times.

**Read on all three platforms, in the job summary.** CI already ran the headless example with
`--features measure` on macOS, Linux and Windows — that is where the acceptance figure comes
from, since no one machine has all three. The number only reached the log, though, and a figure
that takes opening three logs and scrolling to find is not the defensible one the ROADMAP asks
for. Each job now puts its reading in its own summary.

**Three numbers, and they are not the same quantity.** Taken 13 Sep 2026 from one run of the
identical headless script:

| platform | reading | metric |
|---|---|---|
| macOS (arm64) | 4.6 MiB | `phys_footprint` |
| Linux (x86_64) | 13.5 MiB | `VmRSS` |
| Windows (x86_64) | 3.4 MiB | `PrivateUsage` |

Linux reads about three times macOS for the same work and is not three times worse: `VmRSS`
counts shared pages that `phys_footprint` discounts and `PrivateUsage` excludes outright. This
is D-46 showing up as an operational fact rather than a definition — **§M8's "< 60MB RSS" does
not name one measurement**, and the acceptance has to state which metric it means on each
platform or the claim is three different claims wearing one number.

The ceiling is now set, per platform, at roughly twice each observed figure — enough to catch a
regression that doubles the floor, loose enough to survive runner variance. One sample is a
weak basis for a tight bound, so it is deliberately generous and should tighten once there are
enough runs to know the spread. Setting it *after* the readings rather than before is the whole
point: a bound guessed ahead of its data is how D-49's tolerance came to be wider than the
thing it was checking.

**The gate fails when the reading is missing, not just when it is too big.** Otherwise deleting
the measurement would silently disable the check — a guard that cannot fail, which is the
failure mode this repo keeps rediscovering. Worth one more note: a plain substring search for
the reading matches the workflow's *own script text*, which the runner echoes into the same
log, so the grep is anchored on a digit. The first attempt at collecting these numbers found
the instrument instead of the measurement.

**And it was worse than noise.** Instrumenting the windows example showed the reading was never
taken after a frame at all — the print never fired, because the windows were never asked to
redraw and `ControlFlow::Wait` never volunteered. Every external figure taken there described a
process that had drawn nothing. The example now requests its first frame, which it should have
done anyway.

**The metric is not portable and the module says so.** macOS `phys_footprint`, Linux `VmRSS`,
Windows `PrivateUsage` are three different quantities; `Metric` is returned alongside the
number so a table comparing them cannot silently compare unlike things.

**Two bugs the tests caught, both of the kind that reads as plausible:**

- The offset of `phys_footprint` in `task_vm_info` is **144**, not the 152 arithmetic said. A
  hardcoded 152 would have reported `compressed_lifetime` as a memory figure. The probe
  computes the offset and asserts it; `mach2` supplies the struct, which is `repr(C, packed(4))`
  and would not have matched a hand-rolled `repr(C)` copy.
- The cross-check against `vmmap` first passed *while deliberately reading the wrong field* —
  its tolerance had a 4 MiB floor and the test process is 1.7 MiB, so a tolerance wider than the
  whole quantity. Tightened to 5%, it now fails on the neighbouring field. Measured, the two
  agree to within a few hundred bytes.

CI runs the headless todo example with the feature on, so all three platforms print a figure
from the same scripted run: **5.0 MiB (`phys_footprint`), debug, no GPU device**. That is not
comparable with the 25.5 MB in the table above, which was release and had a window; the line
prints its profile for that reason.

### What a 5 MB response costs, before building the application for it

M8's acceptance puts a 5 MB response in an API-client-shaped application. Whether the obvious
way to build that works at all is worth knowing first, because if it does not the answer
changes what to build rather than how to tune it.

The obvious way is what a code viewer does: one element per line, one text node inside it. A
5 MB pretty-printed JSON body is **240,884 lines**. Measured with the probe above, one
configuration per process:

| lines | nodes | memory | layout | marginal |
|---|---|---|---|---|
| 1,000 | 2,002 | 23.1 MiB | 66 ms | |
| 10,000 | 20,002 | 158.3 MiB | 1,541 ms | 15,747 bytes/line |
| 50,000 | 100,002 | 761.8 MiB | 43,328 ms | 15,821 bytes/line |

**About 15.8 KB per line of a 22-byte line**, and the whole response projects to roughly
**3.7 GiB** against a 60 MB budget. One element per line does not fit, and not by a margin any
tuning closes: the response pane has to build nodes for the lines *in view* rather than for the
response.

**Where it goes**, measured in a process that did only this:

| stage | bytes/line | |
|---|---|---|
| dom | 1,016 | an element and a text node |
| style | 16 | interned, so shared between identical lines — D-21 working |
| **layout** | **14,818** | **box tree, shaped text, cache — 93% of it** |

Layout is the whale, not the `Node` the memory measurement was worrying about earlier. Whatever
the virtualised pane looks like, what it must avoid building is laid-out and shaped text.

### Building only the window, which does fit

The same 240,884 lines with only the ~49 in view built, and two spacers standing in for the
rest so the scrollbar is the size it would be if they had been built:

| | one element per line | only the window |
|---|---|---|
| nodes | 481,768 | **103** |
| memory | 3,678 MiB | **2.4 MiB** |
| first frame | ~43 min, extrapolated | **15 ms** |
| a scroll frame | — | **1.9 ms** |

The scroll extent comes out at 4,335,212 px against the 4,335,212 px it should be, which is
checked rather than assumed — a cheap pane that scrolls to the wrong place is not a pane. So
the approach fits the budget with two orders of magnitude to spare, and the acceptance is
reachable.

**What it needed, and now has.** The first version baked the spacer heights into the
stylesheet, which a real pane cannot do: both change on every scroll frame, and the only way
to move one was to reparse a stylesheet. That was the **`style`, the CSSOM subset** gap listed
under the DOM API above — recorded as one of three missing pieces, but blocking rather than
merely missing, because the one design that fits the budget could not be built without it.

It is implemented (D-50), and the example now scrolls the pane the way a pane does: **1.9 ms a
frame**, 12% of a 60 Hz budget in a debug build, writing two `style` attributes and the rows'
text and reparsing nothing. So the acceptance is not just affordable in memory, it is
affordable per frame.

The remaining piece is the CSSOM *object* — `element.style.height = "4px"` — which is a JS-side
wrapper over `setAttribute` rather than engine work.

**A process per measurement, because the first version of this was wrong.** Measuring every
size in one process produced two numbers that disagreed sixfold — ~13 KB per line as a
difference of totals, ~2.6 KB as a difference of stages. Freeing a 670 MiB tree does not return
the memory to the operating system, so every later reading was taken against an allocator
holding an earlier one's pages, and which way that skewed a number depended on which way it was
being subtracted. Isolated, the two methods now agree to within 1% (15,821 and 15,850), and
that agreement is the only reason to believe either.

### Drag and drop: winit throws the position away

Checked before designing anything, because the engine dispatches events to nodes by
hit-testing a point, and a drop with no point has no target.

winit 0.30's `WindowEvent::DroppedFile(PathBuf)` and `HoveredFile(PathBuf)` carry **a path and
nothing else** — one event per file, with no position and no grouping. It is not a platform
limit. Every backend has the coordinate and discards it:

- **macOS** (`window_delegate.rs`) reads the pasteboard out of the `NSDraggingInfo` `sender`
  and ignores its `draggingLocation`. `draggingUpdated:` is not implemented at all, so there
  is no drag-over stream either — only entered, exited and dropped.
- **Windows** (`drop_handler.rs`) calls `DragQueryFileW` and never `DragQueryPoint`, and
  ignores the `pt` that `IDropTarget::DragOver` is handed.

So on winit 0.30, **node-targeted drag and drop is not implementable** and neither is
drop-target highlighting, which needs the drag-over stream. Tracking the last `CursorMoved`
does not rescue it: during an OS drag session the pointer belongs to the drag, not the window.

What *is* implementable is a window-level drop — "these files were dropped on this app" —
which needs no target and is the case an API client actually wants: drop a `.json` or `.har`
on the window to load it. That covers the acceptance app's need without pretending to a
precision the platform layer is not delivering.

**winit 0.31 fixes all of this, so none of that is the answer.** Checked before writing a
patch, and the patch would have been wasted work. 0.31.0-beta.3 (4 Sep 2026) replaces the
whole thing with a real drag-and-drop subsystem, split across `winit-core` and per-platform
crates:

- `DragEntered { id, position: Option<_> }`, and **`DragPosition { id, position, proposed_action }`**
  — the drag-over stream, with a position that is not optional.
- `DragDropped`, `DragLeft`, `DataTransferReceived` — data is fetched asynchronously by id and
  arrives typed, rather than as a `PathBuf` per file.
- `OutgoingDragDropped` / `OutgoingDragCanceled`, so an app can be a drag *source* too.

And the backends implement it rather than merely declaring it, which is the thing 0.30 taught
to check: `winit-appkit` now reads `sender.draggingLocation()` and implements
`draggingUpdated:`, and `winit-win32` carries a full `IDropTarget` with drag-state tracking.

**The upgrade is taken** (D-51). crisol is unreleased, so a pre-release dependency costs
nothing downstream, and the alternative was a fork that 0.31 would have made pointless. The
migration was 35 mechanical errors: `Window` is a trait now, `inner_size` is `surface_size`,
`Resized` is `SurfaceResized`, `CursorMoved` is `PointerMoved`, `resumed` is
`can_create_surfaces`, `run_app` takes its handler by value, and `set_cursor` takes a `Cursor`.

Two of those had a wrong answer that compiles, which is the part worth remembering. The
compiler suggests `outer_size` for `inner_size` — a different measurement that includes
decorations, and taking the hint would have sized every surface wrong silently. And
`MouseScrollDelta` is `#[non_exhaustive]` now, so an unknown delta returns rather than
scrolling zero: a kind this build cannot read is a scroll of unknown size, not a scroll of
none.

Drag and drop itself is not built yet — this is the dependency that makes it possible, and the
engine-side events are the next piece.

### Drag events, and the dispatch layer nothing calls

With winit 0.31 in (D-51), a drag carries a position, so it can be hit-tested to a node like
any other input. `crisol-events` grows `DragEnter`, `DragOver`, `DragLeave` and `Drop`, and
`EventSystem` grows `drag_moved`, `drag_dropped` and `drag_left` beside the pointer's.

Three decisions in it worth stating:

- **A drag keeps its own chain.** `dragged` sits next to `hovered` rather than reusing it,
  because on every platform the OS owns the cursor for the duration of a drag. If a drag set
  `:hover`, a file passing over a button would light it up as though a click were coming, and
  nothing would turn it off. There is a test for exactly that, and it was checked by injecting
  the regression and watching it fail.
- **`DragEvent` is not a `PointerEvent`.** No button, no pointer id that means anything here.
  It carries a position and modifiers, and *not* the dragged data — the platform hands that
  over asynchronously and by reference, so correlating a drop with its payload is the
  application's job, not the engine's.
- **A drop retargets rather than trusting the last move.** `DragDropped` carries no position
  of its own, and the last motion before a release is not guaranteed to arrive, so the drop
  hit-tests where it says it happened.

**Now wired into the example, both paths.** The scripted run drives a pointer and a drag
through `EventSystem` and asserts across the seam; the live window routes `PointerMoved`
through it so `:hover` matches, and translates winit's `DragEntered`, `DragPosition`,
`DragDropped` and `DragLeft` into the engine's own. Dropping on a row selects it, which is the
visible proof that a drop reaches a *node* rather than the window, and the row under a drag
carries a `drop-target` class while it is there.

`DragDropped` carries no position, so the drop is told where it happened from the last
`DragPosition` — the example keeps it for exactly that reason. A `DragEntered` without a
position is ignored rather than guessed at; the `DragPosition` along in a moment will say.

Worth being exact about coverage: the dispatch itself is asserted on all three platforms in
CI, and the live window methods are compile-checked only, because they need a real surface and
a real drag. What is tested is the part that could be silently wrong.

**The gap this closed, and what remains of it.** `EventSystem` had no consumers outside
its own tests. The todo example uses `hit_test`, `scroll_at` and `scroll_from` directly and
never registers a listener; `apply_state` — the call that writes `:hover`, `:focus` and
`:active` onto elements — is called only from the events crate's tests. No example styles any
of those pseudo-classes, so nothing is visibly broken, but the whole capture/bubble and
interaction-state layer is built, tested and integrated nowhere.

The drag events are consistent with that rather than an exception to it: they sit beside the
pointer events, equally covered by unit tests and equally unwired.

**Wiring it up found a bug that had been there since M5.** `apply_state` wrote the `:hover`,
`:focus` and `:active` bits onto elements and never marked anything dirty, so an incremental
restyle skipped the very nodes whose state had just changed. An application following the
method's own instructions — *call this after handling input and before restyling* — would get
an element whose state says hovered, a computed style that says otherwise, and no error
anywhere. It is fixed with the same invalidation an attribute write uses (D-38), because
`:hover` can be matched through a descendant or a sibling combinator.

The reason it survived is worth more than the fix. Every existing test called `apply_state`
and then read `data.state` back — asserting on one side of the seam, never across it. The new
test restyles and checks the computed colour, which is the only version of the question that
could have failed.

**And the fix exposes a price.** Crossing one row boundary in a 200-row list restyles **400 of
604 elements**, because D-38 marks a changed node's following siblings and each marked row
marks the rest of the list. It cost nothing before only because `:hover` never worked. Moving
*within* an already-hovered row is still free — the chain does not change, so nothing is
re-marked — so this is a per-boundary cost rather than a per-move one, and there is a test
pinning both numbers.

**Narrowed, and it was worth 100x.** If no rule in any loaded stylesheet uses `+` or `~`, a
node's change cannot reach its following siblings, so walking them is provably dead work.
`Stylesheet` computes that once at parse, the engine tells the tree on every restyle, and
`mark_selector_state_changed` skips the sibling walk when the answer is a positive no.
Crossing a row boundary in the 200-row list went from **400 of 604 elements to 4** — the row
left and the row entered, each with its span.

The flag is `Option<bool>` rather than `bool` so that `Default` lands on *unknown*, which is
treated as yes. The narrow answer is the one that can be wrong, and it should not be reachable
by forgetting to set something. `:is()`, `:where()` and `:not()` are walked into, because a
`+` inside one is still a `+`.

`set_attribute` gets the same win for free, since it marks through the same call.

**The guard on it needed two attempts, which is the part worth remembering.** The first test
asserted that `.row:hover + .row` still worked, and it passed with the sibling walk disabled
entirely. Entering the document hovers the chain up to `html`, and marking `html` dirties
everything — so the sibling rule appeared to work for a reason that had nothing to do with
siblings. The real test moves the pointer a *second* time, between two rows, when the
ancestors keep their bit and are not re-marked: row 2 is then reachable only by walking from
row 1. That version fails when the walk is disabled, which is the only reason to believe it.

### Font family resolution, and what "consistent across platforms" actually means

crisol draws its own everything — cascade, layout, paint, renderer — so a box is the same box
on all three platforms. Text was the exception, and not for the reason it looked like.

**`font-family` took only its first entry, and treated a generic as a name.** The shaper did
`families.first()` and handed it over as `Family::Name`, so:

- `font-family: sans-serif` asked the database for a font *called* "sans-serif". There is
  none, so it fell through to cosmic-text's default — which is sans-serif, so the output was
  right for a reason that would not survive the next line.
- `font-family: "Inter", monospace` tried Inter and stopped. A machine without it rendered
  **proportional** text where the author asked for monospace and named a fallback that would
  have delivered it. That is visible, and it is exactly what a JSON viewer asks for.

Now the list is walked: the five generics always resolve, a named family is used when the
database actually has it, and an entry that is neither is skipped rather than ending the
search. A list of only missing fonts resolves to nothing and lets cosmic-text pick, which is
what happened before for that case.

**No font is bundled, and the user-agent stylesheet gains no rule.** D-25's bar is that a rule
must describe something true of *this engine*, and `:root { font-family: sans-serif }` would
describe nothing: with no family declared the resolver already yields nothing and cosmic-text
already picks sans-serif. A rule that changes no output has not earned its place.

Which leaves the honest answer to "will it look identical everywhere": **the layout will, the
glyphs will not, until an application says which font it wants.** System fonts differ, so
`sans-serif` is Helvetica here and DejaVu there, and those have different metrics. An
application that needs identical pixels loads its own face with `FontSystem::load` and names
it in CSS — which now works, because naming it no longer discards the fallback behind it.
That is an application decision with a binary-size cost, so the engine offers it rather than
imposing it.

### Window chrome the application draws

The window is borderless now — `decorations(false)` — so what it shows is what the engine
drew, on all three platforms. That is the one part of the window crisol did not otherwise
control, and it is where the consistency question above actually bites: a system title bar
looks like its system no matter what the document does.

What it costs is the two things the system title bar was doing.

**Moving** is the application's, because only it knows which of its elements is a title bar.
The example hit-tests its own `h1` and calls `drag_window`.

**Resizing** is geometry, identical everywhere, so it is in the engine:
`crisol_ui::chrome::resize_edge` answers which edge or corner a point is in. Corners beat the
edges they are made of, the band sits inside the window rather than straddling its boundary,
and a window narrower than two borders splits the difference so both edges stay reachable —
without that last one a window shrunk to nothing could never be grown back, which is a
permanent failure rather than an awkward one.

It names its own eight directions rather than winit's, the same way `platform_cursor` returns
a `cursor-icon` type: the umbrella's `Cargo.toml` says plainly that nothing above the renderer
should need `winit`, and a resize direction is not a reason to break that. The application
writes the rename once.

**A cfg I moved by accident, and what caught it.** Adding `pub mod chrome` next to
`pub mod measure` put chrome under `#[cfg(feature = "measure")]` and left measure ungated, so
measure compiled without the optional `mach2` behind it. Every test passed — they run with
`--all-features` — and the only thing that noticed was `cargo doc` without them. Worth
remembering that the feature matrix is part of the gate and not a formality.

### A native menu bar, and the two platforms that have one

`muda` drives it, as a dev-dependency of the example for the same reason `winit` is one: the
umbrella's `Cargo.toml` says nothing above the renderer should need a windowing crate, and a
menu bar is shell rather than document.

macOS and Windows only. On Linux `muda` drives GTK, which this engine does not otherwise
depend on and will not acquire for a menu bar — `winit` speaks X11 and Wayland directly, and
Linux applications conventionally put their menus inside the window, which crisol can already
draw. `default-features = false` drops GTK and libxdo and leaves `muda` its noop backend, so
the dev-dependency still compiles there without needing a `[target.'cfg(...)']` section.

Two things about it are not obvious:

- **The menu attaches after the window, not before.** On macOS it hangs off the
  `NSApplication`, which `winit` has only finished creating by the time `resumed` runs; on
  Windows it hangs off the window itself. Building it in `main` is too early on both.
- **Menu clicks are not window events.** `muda` posts them to its own channel, so
  `about_to_wait` drains it once per turn of the loop. That is compatible with
  `ControlFlow::Wait` because the click is itself a native event and wakes the loop — nothing
  here polls, and nothing needs `ControlFlow::Poll`.

**The actions live on `App`, not on the window's `State`,** which is the part worth keeping.
The bar cannot be driven without a window, so a menu action written as a method on `State`
would have been the one interaction path in this example that CI never executes on any
platform. A layer down, the headless run exercises both — and both guards were checked by
injecting the regression and watching them fail rather than by observing that they pass:
removing the empty-draft placeholder trips *an empty draft adds the placeholder* (27 against
28), and dropping `clear_done`'s filter trips *every completed row went, and only those* (0
against 27).

A menu bar that will not build is a warning rather than a failure. The application is entirely
usable from the keyboard without it.

**Two things only CI could catch, and it caught both.** Under `-D warnings` a `cfg` on the
caller is not enough — the callee has to carry the same one, or it is dead code on every other
platform, which is how `menu_add`/`menu_clear_done` failed Linux. And `undocumented_unsafe_blocks`
wants the literal `SAFETY:` marker: `init_for_hwnd`'s block had a comment explaining exactly
why it was sound and still failed Windows for not spelling it that way. Neither is reachable
from a macOS build, but both are reachable from `cargo clippy --target`, which needs no linker
and is now worth running for the two other desktop triples before pushing anything platform-shaped.

### Packaging, split by what needs a host rather than by host

`crisol package` wraps a built executable into `.app`, `AppDir`/`.AppImage` or `.wxs`/`.msi`
(D-52). It takes a binary rather than a project because `crisol build` is M13; the input today
is a Rust application built against `crisol-ui`, which is what §M8's acceptance describes, and
the same command takes `build`'s output when there is one.

**The layout is built on any host and only the container step is gated.** A `.app` is a
directory, an AppDir is a directory, a `.wxs` is XML — none of them need the platform they
target. So all three are produced everywhere and the 13 tests covering them run identically on
all three CI runners; only `appimagetool` and WiX are host-bound, and when they are absent the
layout is left in place and reported, because the layout *is* their documented input. Built
the other way — one host-only path each — the two platforms the author does not sit in front
of are the two that quietly rot.

Verified beyond the crate's own assertions: all three artifacts were generated from macOS and
re-read with parsers that are not this code — `plistlib`, `configparser` and `ElementTree` —
checking that `CFBundleExecutable` names a file that exists and is executable, that the desktop
entry's `Exec` resolves to the staged binary, that `AppRun` carries its executable bit, and
that the `.wxs` names its payload relatively rather than by a path that only exists here. The
bundled executable was then run out of `Contents/MacOS` and answered.

**A test may not branch on what is installed.** The sealing step first ran whenever the tool
happened to be on `PATH`. That passed here, where WiX is absent, and failed on CI, where the
GitHub Windows image ships it — the one test that reached the sealing path invoked WiX for
real and got exit 204. The lesson generalises past packaging: a suite whose behaviour depends
on the machine is a suite that tests the machine. `--stage-only` makes it a decision, every
test takes it, and the subprocess call sits outside the tested surface on purpose.

**Two things that fail silently and so are refused at package time.** An MSI `UpgradeCode` is
never invented: it has to be identical across every version ever shipped or the second release
installs beside the first rather than replacing it, which works perfectly once and then never
again. And an MSI `ProductVersion` is packed into 32 bits — major and minor are bytes, build is
16 bits, the fourth field is ignored when comparing — so `1.2.3.4`, `1.2.3.5` and `1.2.65536`
all build, install, and then fail to upgrade.

**Still to do for M8:** nothing in the deliverable list. What remains is the acceptance
proper — an API-client-shaped application with a 5MB response in it, measured on all three
platforms — and that is the §7 kill criterion rather than a checkbox.

**One thing measured and left alone.** In the todo example, moving the selection runs one
effect per row: every row asks "am I the selected one?" and so subscribes to the shared
signal. The DOM layer absorbs the writes, so the cost is closure calls rather than relayouts,
but it is linear. That is what this way of modelling a selection costs, not a limit of the
engine — a list long enough to care would remember the previous row and toggle exactly two.
Said out loud in a comment rather than quietly shipped.

### The acceptance, and the number the product claim rests on

`ui/umbrella/examples/apiclient.rs` is the application §M8 asks for: a sidebar of saved
requests, a URL bar, a status line, and a response pane holding what the selected request
returned. Selecting another request swaps the response and returns the pane to the top, which
is the interaction that makes it an application rather than a layout.

**With a 5 MB JSON response loaded — 240,884 lines — it measures 10.4–10.7 MiB**, the spread
being run-to-run variation rather than a difference between profiles: debug and release land in
the same place, because what dominates is the response text and neither profile changes that.
The budget is 60 MB, so it lands with a factor of about 5.6 to spare, and §7's first kill
criterion does not fire.

| | |
|---|---|
| response | 5.0 MB, 240,884 lines |
| nodes in the tree | **118** |
| laid out, first frame | 147 |
| scrollbar extent | 4,335,248 px against the 4,335,248 px it should be |
| a scroll frame | 1.71 ms release, 2.38 ms debug |
| **memory** | **10.4–10.7 MiB (phys_footprint)**, against 60 MB |

118 nodes for 240,884 lines is the whole argument. One element per line is 481,768 nodes and
3,678 MiB (D-47), which is not a tuning problem. The pane builds only the ~47 lines in view
plus overscan, and two spacers stand in for the rest — and *that the extent is right* is
asserted rather than assumed, because a pane that is cheap by scrolling to the wrong place is
not a pane. That guard was checked by breaking the bottom spacer and watching it fail: 182 px
against 4,335,248.

**The body is held once**, as one `String` plus a `Vec<u32>` of line starts, rather than as
240,884 separate `String`s. The per-`String` header alone would be ~5.8 MB — a tenth of the
budget spent on bookkeeping nobody can see. Most of the 10.5 MiB is the response text itself,
which is the application's data rather than the engine's, and that is the shape the number
should have.

**Measured on all three, and they are three different quantities.** CI gates each at 60 MiB
and writes the reading into the job summary:

| platform | reading | metric |
|---|---|---|
| Linux | 15.7 MiB | `VmRSS` |
| macOS | 10.2 MiB | `phys_footprint` |
| Windows | 8.1 MiB | `PrivateUsage` |

**Do not read that as a ranking.** `Metric` in `measure.rs` already says why: `VmRSS` excludes
anything swapped or not yet faulted in, `PrivateUsage` counts committed private bytes whether
resident or not, and `phys_footprint` is what jetsam kills on. Windows reading lowest does not
mean Windows is cheapest — it means three operating systems were asked three different
questions. What the three jointly support is the only claim being made: *on every platform, by
that platform's own accounting, this is far under 60 MB.* A three-way comparison would need one
metric all three can produce, and none of these is that.
The extraction was checked against real output rather than assumed, and the gate was checked
in both directions — 59.9 passes, 60.1 fails — because a budget that cannot fail is not a
budget. The ceiling is written in the workflow rather than carried in the matrix, because
unlike the idle-memory step above it is §M8's number and not a per-platform observation.

**What this does not cover.** The reading is headless: no window, no GPU device, so it is the
engine's and the application's rather than the driver's. That is the right number for a
claim about *this* engine, but a user running a windowed build pays for a swapchain and a
driver on top. `todo` is the windowed path and is
measured separately; a windowed apiclient is a follow-up rather than a gap in the claim.

### A paint-only change no longer costs a relayout (issue #20)

The unfinished half of D-45. `restyle_incremental` marked `LAYOUT` on any computed-style
difference, so toggling `.done` on a todo row — where the rule is
`li.done span.label { color: … }` — relaid the row out in order to change a colour.

`ComputedStyle::layout_eq` now says whether two styles would lay out identically, and the pass
marks `PAINT` alone when they would. The todo benchmark's `toggle` step: **8 layout
invalidations to 4**.

**The issue predicted 0, and 4 is the right answer.** The remaining four are not the row. They
are the footer count being rewritten in the same step — a *text* change, not a style one —
whose ancestor chain is `text` → `p.count` → `body` → `html`, exactly four. That is a real
relayout and it should stay. Worth saying because "we predicted 0 and got 4" reads like a
partial fix, and checking which four it was is the difference between a fix and a plausible
number.

**Both directions are pinned, and both were checked by breaking them.**
`ui/style/tests/paint_only.rs` asserts that a colour change repaints *and* that width, font
size and `overflow` still relay out. Reverting the fix fails only the colour test; making
`layout_eq` return `true` for everything fails the other three. A fix that simply stopped
marking `LAYOUT` would pass the first test and fail the second three, which is why the pair
exists rather than the first alone.

The cost worry in the issue turned out not to apply: the field comparison does not replace the
pointer comparison, it runs *after* it, so it is paid only for nodes that genuinely restyled.

**Totals:** 915 tests passing — 912 at the last measured point plus 3 new; the `node_modules` and test262 cases skip without their suites

## Open questions

Anything here that is a piece of work rather than a judgement call is filed as an issue, so
it is trackable rather than buried in a document nobody greps. Current ones:
[#13](https://github.com/WertCore/crisol/issues/13) IME on a real input method,
[#14](https://github.com/WertCore/crisol/issues/14) screen readers,
[#15](https://github.com/WertCore/crisol/issues/15) a window on Windows and Linux,
[#16](https://github.com/WertCore/crisol/issues/16) nested rounded clips.


- **M1's "window opens on all three platforms" is verified on macOS only** — issue
  [#15](https://github.com/WertCore/crisol/issues/15). Everything short of the window is
  covered offscreen on all three in CI, so what is untested is specifically the `winit`
  surface and swapchain path. **One piece of it is now testable and tested:** the decision
  `WindowSurface::resize` makes is split into `reconfigure_to`, so the two cases that only a
  real window produces — Windows reporting `0x0` while minimised, and a resize event for the
  size already configured — are pinned on all three platforms. That is the failure mode that
  would have been a *panic* rather than a cosmetic artefact. What still needs a human is
  everything visual: artefacts on resize, dragging between displays of different densities,
  and whether a scale-factor change is picked up. The issue stays open for those.
- **A full `cargo test --workspace --all-features` fits, but the development machine rarely
  has room for it.** It ran repeatedly on 2026-09-13 (559 tests); what it needs is about
  2 GiB free, and the resting state of that disk is nearer 1 GiB. So the failure mode is a
  `No space left on device` in the middle of a link, not a code problem — `cargo clean` and
  retry rather than splitting the run. The whole five-step gate from a clean `target/` wants
  ~2.5 GiB. Note that `cargo clean --doc` reclaims almost nothing (~12 MiB): the doc step's
  real cost is rebuilding dependencies at *default* features, not rustdoc output.
- **`DisplayList` has no transform command.** Rounded clipping landed (D-26); transforms did
  not, and are not in any milestone's property subset yet.
- **Per-corner inner border radii are approximated.** The shader shrinks a corner's inner
  radius by the thicker of its two adjacent borders; CSS uses per-axis elliptical radii. The
  difference shows only on a box with very different adjacent border widths and a large
  radius. Revisit if a real design hits it.
- **Two parts of M5's acceptance need a human and are outstanding** — issues
  [#13](https://github.com/WertCore/crisol/issues/13) and
  [#14](https://github.com/WertCore/crisol/issues/14). The IME state machine is tested with
  real Japanese composition sequences, but nobody has driven a platform input method through
  the windowing layer. The accessibility `TreeUpdate` is asserted in detail, but nobody has
  listened to a screen reader read it. Neither can run on a CI runner. Until then the
  milestone is *believed* complete on those two points rather than *shown* to be.
- **Only one rounded clip is honoured at a time** (D-26) — issue
  [#16](https://github.com/WertCore/crisol/issues/16). A test pins the current behaviour.
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
- **D-34** — focus order is document order; a positive `tabindex` is accepted and ignored,
  because honouring it is how keyboard-unusable interfaces get built.
- **D-35** — composition is a state machine, not a stream of keystrokes: a preedit replaces
  rather than appends, a cancel is not an empty commit, and moving focus abandons it.
- **D-36** — the accessibility tree is a second, smaller tree, built in full rather than
  incrementally until M6 owns invalidation.
- **D-37** — the layout cache belongs to the caller, because a frame loop has to mutate the
  tree between passes and a context holding `&mut Tree` makes that impossible.
- **D-38** — invalidation is conservative in a shape the selector dialect guarantees:
  descendants and *following* siblings only, which is what excluding `:has()` bought.
- **D-39** — damage is the union of old and new boxes, in absolute coordinates.
- **D-40** — a damaged frame loads rather than clears, and paint repaints the background
  inside the damaged region, because a load op ignores the scissor.
- **D-41** — damage has three states, not two: an `Option` whose `None` meant both "redraw
  everything" and "draw nothing" made an off-screen damage rectangle repaint the whole
  surface.

---

## M9 — GC and value representation

### `Value`: numbers unboxed, everything else in the NaNs

`crisol-value` holds a JavaScript value in one `u64` (D-53). Numbers are stored as themselves
and everything else hides in the 2^52 NaN patterns JavaScript cannot observe, because the
double *is* JavaScript's only number type and so is the hot path by definition.

The subtle part is not the layout, it is **NaN canonicalisation**. A NaN carrying an arbitrary
payload can have the tag bits set, and without rewriting it on the way in it reads back as an
object whose address is the mantissa — §3.1's "use-after-free that appears only under memory
pressure", which is the failure mode M9 exists to design out. Checked by removing the rewrite
and watching the hostile NaN come back as an `Object`.

Two smaller things worth carrying forward:

- **`kind()` is total over all 2^64 patterns**, because `from_bits` is reachable from generated
  code and a value no safe constructor produces must not be able to abort a program.
- **Derived equality is `Object.is`, not `===`.** `Object.is(NaN, NaN)` is true and bitwise
  equality agrees *because* of the canonicalisation; `Object.is(0, -0)` is false and bitwise
  equality agrees because the sign bit differs. `===` disagrees with both. There is a test
  named after this so it is found by reading rather than by debugging.

### Shapes: a tree of remembered transitions

An object carries a `ShapeId` and a flat slot array, never its own property names (D-54).
Adding a property transitions to another shape and the transition is remembered, so the second
`{x: 1, y: 2}` a program evaluates compares no names at all. Checked by removing the reuse and
watching a hundred identical objects grow the table.

Three things in it are easy to get wrong and each has a test named after it:

- **Assigning to a property that already exists is not a transition.** Otherwise
  `for (…) obj.x = i` grows the tree once per iteration — a memory leak shaped like a hidden
  class.
- **Exoticness propagates through every transition.** A `Proxy` that quietly became an
  ordinary object after one assignment would let the fast path specialise something it must
  not, which is a wrong answer rather than a slow one.
- **Property keys are case-sensitive**, unlike `crisol_tree::Atom`, which lowercases because
  HTML names are case-insensitive. `obj.X` and `obj.x` are different properties. `PropertyKey`
  is a separate type for that reason and because the tracks do not converge until M16.

Lookup walks the chain: O(properties), and §3.4 already says the speed comes from a per-site
monomorphic cache in the IR rather than from making this O(1). A flat map per shape would be
O(n²) in memory across a transition chain, for objects that are mostly small.

### The collector, and §M9's acceptance

`crisol-gc` is a precise mark-sweep collector with a shadow stack, a scope-guard rooting API
and a stress mode (D-55).

**§M9's acceptance — "a cyclic object graph, drops all roots, and the collector reclaims it" —
passes.** `a_cycle_whose_roots_are_dropped_is_reclaimed` builds `a → b → a`, drops the scope,
and both are swept. That test matters more than it looks: a collector that leaks cycles passes
everything else here, because refcounting would too.

**Rooting is a scope guard because §M9 asks for the API to be hard to misuse.** There is no way
to get a `Rooted` without a `Scope`, and a `Rooted` borrows its scope, so the mistake is a
compile error rather than something the collector finds later. Dropping is the only way to
unroot — there is no `pop` to forget.

**A `GcRef` is a slot and a generation, not an address**, so a handle whose object was
collected reads nothing even after the slot is reused (D-17's argument, where it is worth
more). 32 + 16 bits, because 48 is what a `Value` carries (D-53); a handle that did not fit
would box every object reference in the language.

Three guards were checked by breaking them: dropping the generation check fails the stale-handle
test, a sweep that frees nothing fails both cycle tests, and a scope guard that forgets to
unroot fails six.

**About the ASAN half of the acceptance.** It asks for "stress mode runs the full suite with
zero use-after-free under ASAN". **That clause is vacuous under this design rather than
satisfied by it** — there is no `unsafe` in `crisol-gc` or `crisol-value`, so a use-after-free
in the sense ASAN detects is not expressible; a stale handle is a detected error instead.
Stronger than asked, and worth stating plainly, because "ASAN was clean" would imply a check
that did not happen.

It is also not permanent. It holds *because* objects sit in a slab behind checked handles. At
M13, compiled code will want to dereference directly — most of the point of compiling — and
then the checks stop being free and ASAN starts having something to look at. Re-open it when
there is generated code to measure.

**M9 is therefore complete** apart from that caveat being understood: value representation,
shapes, collector, shadow stack, `GcRef`, stress mode. **Accept:** a cyclic object graph whose roots are dropped is
reclaimed, and the full suite runs under stress mode with zero use-after-free under ASAN.

§3.1 is the risk and it is a design risk rather than an implementation one: every host function
that touches a JS value participates in rooting, so the rooting API has to be hard to misuse.
Prefer a scope guard over manual push/pop.

---

## M10 — frontend and module graph

### The module graph, and why it is here before the parser

`crisol-frontend` holds the graph: specifiers, edges, evaluation order, cycle detection
(D-56). It knows nothing about `oxc`, deliberately — what a graph *is*, and what a cycle in one
means, is decided by the ES module specification rather than by whichever crate read the
source, and split this way the ordering rules are tested against hand-built graphs where a
cycle takes three lines.

**A cycle is ordered, not rejected.** `react` and `react-dom` have shipped cycles for years;
a graph that refused one would refuse to build most real programs, which is §3.3's failure mode
exactly. `cycles()` reports them so a consumer can warn or explain a temporal-dead-zone error,
but nothing refuses to proceed. Reporting filters out components of one that are not
self-referential — without that, every acyclic module is reported and the report is useless.

Both walks are iterative because the input is somebody's `node_modules` and its depth is not
this code's to bound. There is a 100,000-deep chain in the tests.

### Resolving and parsing a real tree — §M10's acceptance

`Loader` wraps `oxc_resolver` and `oxc_parser`: resolve a specifier, parse the file, collect
what it asks for, repeat. **§M10's acceptance passes** — `react-dom/client` resolved from a
real npm-installed tree, every specifier resolved, nothing unresolved.

**Both module systems, because a real tree has both.** React 19 is CommonJS from top to
bottom; its entry is `module.exports = require('./cjs/react.production.js')`. ESM requests come
from the parser's `ModuleRecord` (the specification's `[[RequestedModules]]`), which does not
and should not contain `require` — that is a function call, not syntax. So `require()` is found
by an **exhaustive AST visit**, and the choice of "exhaustive" over "match the shapes we
expected" is the important one: a loader that understood only `import` would walk React and
find *no edges at all*, then report a complete graph with no unresolved imports. **It would
pass the acceptance by doing nothing.** A missing edge makes "no unresolved imports" easier to
satisfy, not harder, and getting that backwards is how this milestone would be passed without
being done.

**A real tree, not a fixture.** What makes it an acceptance is `exports` maps, conditions, CJS
entry points and a dependency in another package, laid out the way npm lays them out. CI
installs React 19.2.8 rather than vendoring it, pinned so a React release cannot turn a green
branch red without a commit, and `CRISOL_REQUIRE_NODE_MODULES` makes an absent tree a failure
rather than a skip — the same arrangement as `CRISOL_REQUIRE_GPU`, for the reason the workflow
already states.

**§3.5 is visible in the graph.** `if (process.env.NODE_ENV === 'production') require(A) else
require(B)` puts *both* bundles in it. That is correct for a graph, which records what could be
imported; eliminating one is an optimisation pass's job (M12) and §3.5's actual subject. The
test asserts both are there so the day one disappears is a failure rather than a smaller number
nobody looked at.

**Two things assumed and then checked, one of which was wrong.** `oxc` was written off as too
large for this disk — it is 292 MB of `target` and builds in fifteen seconds. And the acceptance
was written off as needing an npm that does not work in this shell — a real React tree was
already on the machine, and the npm behind the broken `pmg` alias runs fine when invoked
directly. Both were assumptions stated as blockers without being measured.

**Still to do for M10:** TypeScript and JSX go through the same parser and are untested here;
`oxc_resolver` is configured with one set of conditions (`node`, `require`, `default`) and a
browser-conditioned resolve is a different graph.

---

## M11 — IR

### The IR, the lattice, and the verifier

`crisol-ir` holds SSA with block parameters, a shallow type lattice, and a verifier (D-58).
Three choices worth carrying forward:

**Terminators are a field, not an `Op` variant.** A block holds a `Vec<Op>` and exactly one
`Terminator`, so "one terminator, at the end" is a shape that cannot be written down rather than
a rule anyone enforces. The best way to reject a class of malformed graph is to make it
unrepresentable; the verifier is for what a type cannot say.

**Safepoints are mandatory on anything that can collect, and refused on anything that cannot.**
§M11 is firm that the IR must carry them "or the GC integration in M13 will not work". The
second direction matters as much as the first: a safepoint on a `Const` means whoever built the
graph did not know which operations collect, and the ones they *missed* are the dangerous half.
`PropertyLoad` counts — a getter is a call, and on an exotic shape (D-54) the lookup itself runs
user code.

**The lattice is shallow on purpose,** and its laws are tested as laws — join commutative,
associative, idempotent, an upper bound of both operands, agreeing with the subtype relation —
over every pair and triple. A lattice that is only *mostly* a lattice gives an analysis whose
answer depends on pass order, and that surfaces as a miscompilation weeks later rather than as a
failing test. Two different object shapes join to `Object(None)`: still an object, which one no
longer known, because picking one is how a field is read from the wrong offset.

Three guards checked by breaking them: dropping the dominance check fails the cross-branch test,
allowing a missing safepoint fails two, and the dump's expected text is pinned so a format change
has to be agreed to in a diff.

### Lowering, and §M11's acceptance

`lower()` walks the oxc AST and emits IR (D-59). **§M11's acceptance passes**: forty programs
lower, every one verifies, and the dump goes into one snapshot file.

**Locals become slots, so a merge needs no block parameters.** Both arms of an `if` write the
same slot and the code after reads it. `mem2reg` — promoting slots to SSA values — is a
separate pass and belongs with the other optimisations (M12). Constructing SSA *during*
lowering means debugging Braun-style φ insertion and the AST walk at the same time; split, each
half is checkable alone.

**An unsupported construct is recorded, never guessed.** §3.3 says rejecting constructs the
developer did not write is the failure mode to avoid, but a compiler that silently emits
`undefined` for syntax it did not read is worse, because the program runs and is wrong. So
lowering always produces a function and everything it missed is in `unsupported`.
`is_faithful()` exists because checking a `Vec` is empty is easy to forget.

**One snapshot file, not forty.** A change to the IR or the dump format then shows up as a
single diff covering every program, which is what makes it reviewable — forty files each
changing by two lines is forty times the reading for the same information.

**Reading the first snapshot found a soundness bug that no test had.** An object literal was
typed `object#root`, the *empty* shape, after its properties were stored into it — so a pass
trusting that type would resolve `.a` to no slot. It is `Object(None)` now: an object, shape
unknown, which is the one thing that stays true when a value's type is fixed for its whole life
and the object's shape is not. That is the argument for snapshots being *read* rather than
regenerated.

**The corpus is representative of what lowers**, not of JavaScript — no arithmetic, no
functions, no `for` loops, because those do not lower yet. Twelve of them are in
`unfaithful_programs_are_reported_not_guessed`, which asserts each is named rather than
silently mistranslated, so the two lists are checked against each other.

**Still to do for M11 in spirit, though the acceptance is met:** arithmetic, functions and
closures, `for`, arrays, and the rest of the unsupported list.

---

## M12 — runtime library

### The object model, first, because everything stands on it

`crisol-builtins` has `ValidateAndApplyPropertyDescriptor` and the ordinary internal methods
(D-60). `Object.defineProperty`, `Object.freeze`, getters, `Proxy` and `Reflect` are all
restatements of these, so they come before any of them.

Written out rather than simplified, because every rule that looks redundant is load-bearing:
**freezing needs two bits** (non-configurable but writable still accepts a new value); a frozen
property may be re-set to the value it already has **by SameValue**, so `NaN`→`NaN` is allowed
and `0`→`-0` is not — which is the payoff for canonicalising NaN back at M9, since `Value`'s
derived equality *is* `Object.is` (D-53) and this is one comparison rather than a special case.

**`[[Set]]` consults the prototype chain before deciding where to write.** `Object.freeze(proto)`
stops `child.x = 1` from creating an own property on the child. Surprising, correct, and
invisible until someone freezes a prototype.

**`Object.keys` order is observable:** array indices first and ascending, then strings in
insertion order, and only *canonical* decimals count — `"01"`, `"1.0"` and `"-0"` are ordinary
string keys.

Three rules checked by breaking them: dropping SameValue fails two tests, making `[[Set]]` local
fails the frozen-prototype one, and leaving integer keys unsorted fails the ordering one.

**Two of these tests failed when first written, and were the tests being wrong.** Both used
`PartialDescriptor::value()` meaning "an ordinary property" — but it defaults every attribute to
`false`, which is exactly what `Object.defineProperty` does and emphatically not what assignment
does. The asymmetry proving itself on its own author is the reason it has a test.

### Promise, and the ordering that looks like a race

`Agent` holds every promise and one FIFO microtask queue (D-61). §M12 singles this out, and the
three rules that carry it are: **`then` always queues** even on a settled promise; **the queue
drains to empty**, including jobs queued by jobs; and **a missing handler passes the settlement
through as it was** — a rejection arriving at `.then(onFulfilled)` continues as a rejection.

That last one was written wrong first time. Forwarding a rejection as a *fulfilment* means
`p.then(onFulfilled).catch(handler)` never reaches the catch and the program carries on with an
`Error` where it expected data — a wrong answer rather than a crash. Caught by reading it back
before the tests existed.

The canonical check is that two chains interleave `a1, b1, a2, b2` rather than `a1, a2, b1, b2`.
A LIFO queue fails it, a fulfil-always pass-through fails the rejection test, and a synchronous
`then` fails five — all checked by breaking them.

**Not modelled:** the spec's `NewPromiseResolveThenableJob` tick, so adoption costs one extra
microtask here where a real engine charges two. The tests assert relative order, not tick
parity, because parity is a claim this has not earned.

### `Array`: holes, and a truncation that can stop halfway

Everything unusual about an array comes from `length` being tied to the indices that exist
(D-64). Two rules are easy to get wrong by being reasonable.

**A hole is not a property holding `undefined`.** `[, 1]` and `[undefined, 1]` both read
`undefined` at index 0, and only the second answers `0 in a`. The iterating methods disagree
about which they mean — `forEach` skips holes, `map` preserves them — so `has` is a separate
question from `get` rather than making each caller guess which kind of nothing it found.

**`ArraySetLength` stops at the first element it cannot delete**, leaving `length` one above it
and reporting failure. Treating it as atomic is wrong in both directions at once: it refuses a
change the spec allows, *and* discards elements the spec protects. Checked by making it atomic
and watching the test fail.

### Coercions, `Symbol`, and the `Error` hierarchy

**The coercions are written from the grammar** (D-65), because Rust's `f64` parser is close to
it and not the same: `"inf"`, `"nan"` and `"1_000"` are Rust literals that `ToNumber` rejects,
and each has a test. `Number` is not `parseInt` — `Number("10abc")` is `NaN` where `parseInt`
gives `10`, and reaching for the lenient one turns malformed input into a plausible number.
The falsy list is closed, so `Boolean("0")` is true. `String(-0)` is `"0"`: the sign is
observable through `Object.is` and not through text, which is the mirror of the `Map` rule and
why the two cannot share a comparison.

**Well-known symbols are shared without being registered** (D-66).
`Symbol.keyFor(Symbol.iterator)` is `undefined`, and a test asks the registry for that key and
asserts it gets an impostor — putting them in the registry would let `Symbol.for` reach the real
one, which is the collision the separate namespace exists to prevent.

**Every error kind inherits from `Error`** (D-67), which is what makes `instanceof Error` catch
all of them; an independent prototype per kind would pass every construction test and fail every
real catch block. `name` lives on the prototype and `message` is an own property only when
non-empty, and `toString` joins them only when both exist.

### `Proxy` invariants, and the iterator protocol

**A proxy's traps are the easy half** (D-68). What makes `Proxy` safe to have in a language is
that the spec checks each trap's answer against the target and throws on specific disagreements
— without them, a proxy could report a frozen property as holding a different value, and
everything that reasoned about `Object.freeze`, including the optimiser, would be reasoning
about a lie. Every invariant has a test that builds a *lying* trap and asserts refusal.
`isExtensible` has **no latitude at all**, unlike the property traps where invention is allowed.

An implementation with the traps and without the checks passes every test that *uses* a proxy
and fails only the ones that try to break one. Revocation is checked before the handler, because
detaching the handler is what revocation is for.

**`done` is coerced, not compared** (D-69): `{ done: 0 }` is not finished and
`{ done: "false" }` is. Leaving a loop early closes the iterator — that is how a generator's
`finally` runs — while running to exhaustion does not, and both directions have tests.

### `Object` and `Number` statics

**`Object.isFrozen(Object.preventExtensions({}))` is true** (D-70) — every condition holds over
an empty set of properties. Code branching on `isFrozen` to decide whether it may mutate takes
the frozen path for an object nobody froze, and "correcting" that with a `frozen` flag would be
more intuitive and disagree with every engine.

**`seal` and `freeze` differ by one bit:** a sealed object's values can still change, only its
shape is fixed. Both rules checked by breaking them.

**`Number.isNaN` and the global `isNaN` are different functions** (D-71) — the global coerces
first, so `isFinite("1")` is true and `Number.isFinite("1")` is false. Both are implemented next
to each other so the difference is visible where someone picks one. `isSafeInteger`'s boundary
has a test asserting the actual collision (`2^53` and `2^53 + 1` are one double), because the
boundary means nothing without it.

### `String` is UTF-16, and may be ill-formed

`JsString` stores `Vec<u16>` (D-72). A JavaScript string may hold a **lone surrogate**, which
Rust's `String` cannot represent — so using one would mean rejecting legal input or silently
replacing it with U+FFFD. `to_rust` returns `None` rather than substituting; `to_rust_lossy` is
documented as diagnostics-only.

**Length counts code units, iteration yields code points.** Slicing mid-pair splits an emoji and
yields a lone surrogate — specified, and snapping indices to code-point boundaries would both
disagree with every engine and stop `slice` composing with `indexOf`.

`slice` and `substring` differ twice (swap, and negative handling), which is what makes
substituting one for the other a reliable bug. `trim` removes U+FEFF, which Unicode does not
call whitespace and the spec trims anyway.

### `Date`

A `Date` is one number, and three rules about it carry the correctness (D-73). **`TimeClip`
invalidates rather than clamps** — clamping lets a date silently become a *different* date.
**`day_from_time` floors and `time_within_day` uses `rem_euclid`** — truncating puts
1969-12-31T23:00Z in day 0 and a plain remainder gives it an hour of −1, both of which look
right for every date tested by hand and are wrong for everything before 1970. **The epoch was a
Thursday**, so the weekday offset is 4; getting it wrong shifts every weekday by a constant and
looks like a timezone bug. All three checked by breaking them.

Months wrap and days are 1-based while months are 0-based — specified, and deliberate, since
`new Date(y, m + 1, 0)` is the idiomatic last day of month `m`.

**UTC only.** Local-time accessors need the host's zone and its historical transition table
(M15). Guessing would produce a date that is right in one timezone and silently wrong in the
rest — the worst outcome, because it works for whoever wrote it.

### `RegExp`

Over `regress` (D-74), which §M12 names because Rust's `regex` omits **backreferences and
lookaround** and real code uses both. What `regress` does not supply is the **mutable cursor**:
`const r = /a/g; r.test("a")` gives true, then false, then true.

Three rules carry it, all mutation-tested: **`test` is `exec` with the result discarded** (a
stateless `test` would disagree with `exec` on the same object — breaking it fails four tests);
**a failed match resets `lastIndex`**, which is what makes repeated calls alternate; and **`y`
anchors at `lastIndex` while `g` searches from it**. Without either flag `lastIndex` is inert.

An empty match advances by one *character*, or `/(?:)/g` never terminates.

### `Reflect`, `Boolean`, and async iteration

**`Reflect` reports failure where `Object` throws** (D-75) — that difference is the whole reason
it exists. An implementation that made `Reflect.defineProperty` throw would still pass every
test that defines a property *successfully*, so every test here exercises a **refusal**.
`Reflect.ownKeys` includes non-enumerable properties, mirroring the internal method rather than
the iteration helper.

**`new Boolean(false)` is truthy** (D-76), because every object is — the same rule that makes
`if (obj)` a null check. Not a quirk to fix: a wrapper whose truthiness followed its primitive
would make `if (obj)` unreliable for every other object type.

**A rejected async step ends the iteration and propagates** (D-77). Swallowing it would turn a
failed network page into a quietly truncated list — the failure that looks like success.

### The test262 harness, and why M12's acceptance is blocked on M13

The suite is fetched and the harness is built (D-78). It discovers **12,719 cases** for the
implemented builtins, parses every one, and reports what running them would require.

**It cannot run any of them.** Every test262 test is a JavaScript program that must be executed,
and there is no way to execute JavaScript here — `compiler/codegen` is an M13 stub and there is
no interpreter. Checked, not assumed: the only grep hit for "execute" was `evaluation_order`.

**So M12's acceptance depends on M13.** An ordering problem in the roadmap rather than the
implementation, and worth stating: M12 is described as "large but mechanical, and the most
parallelizable work in the project", which is true of *writing* the builtins and not of
*demonstrating* them.

**The pass rate is undefined, not 0%** — reporting "0 of 12,719 passing" would imply the tests
ran and failed. The harness prints that distinction, because a number in a status table outlives
the caveat beside it.

The frontmatter parser is hand-written against test262's restricted YAML subset, so the parse
test runs over **all 12,719 files** rather than a sample, and its counts were cross-checked
against independent `grep`s — files, includes, negative and async all match exactly. CI fetches
the subset sparsely on Linux only, with `CRISOL_REQUIRE_TEST262` so an absent suite fails rather
than skips; all three paths verified.

Two other gaps worth stating plainly rather than leaving implied:

- **Nothing depends on `crisol-builtins`.** It is a correct library of specification semantics
  that the engine does not yet call. The join to `Shapes` (fast path, D-54) and to the GC heap
  is still ahead, and *when an object leaves the fast path* is what §3.2's "one predictable
  branch" rests on.
- **Several pieces stop where a function call would begin** — getters are returned uncalled,
  `Proxy` traps are checked but not dispatched, iterator steps come from Rust. That is
  deliberate and documented, but it means "`Proxy` is done" reads stronger than it is.

### `Map`, `Set`, and the third equality

`Map` keys use **SameValueZero**, which agrees with neither `===` nor `Object.is` (D-62):
`NaN` equals `NaN` *and* `0` equals `-0`. `Value`'s derived equality is `Object.is` — right for
descriptors, wrong here — so keys go through a wrapper that folds `-0` into `0`. NaN needs no
handling because M9 canonicalised it, the second time that decision has paid for itself.

Entries live in a `Vec` with tombstones, not only a hash map, because the spec is specific about
mutation during iteration: an entry deleted before the iterator reaches it is not visited, and
one added during iteration is. Both fall out of positions; neither falls out of a `HashMap`.

### JSON

Strict in, exact out (D-63). Eighteen pieces of JavaScript-literal syntax that JSON does not
allow have a test, because being lenient turns a clear error at the boundary into corrupt data
further in. Surrogate pairs are joined — without it an emoji becomes two question marks
downstream with nothing at the failure point to say why — and lone surrogates are refused.

Out: `NaN` and the infinities become `null`, `-0` becomes `0` (**a round trip loses the sign**),
`/` is not escaped, and objects keep insertion order so a round trip does not rewrite a
document.

`Date`, and `RegExp` via `regress`.
The acceptance is a test262 subset at >80%, which needs the suite fetched the way M10's React
tree is.

**The join with shapes is the piece to do next.** `Shapes` (D-54) is the fast path, a data
property in a slot; this is the general path. Real engines keep both and spill from one to the
other, and *when an object leaves the fast path* is the decision §3.2's "one predictable branch"
rests on. It deserves its own diff.

---

## M13 — codegen

### The IR had no arithmetic, and M13's acceptance needs it

§M11's deliverable lists the IR's ops and **arithmetic is not among them** (D-79). §M13's
acceptance needs *"arithmetic, closures, classes, and array methods"* to compile — and **all
four were in M11's recorded `unsupported` list**. That is a gap in the plan, not the
implementation: the roadmap reads as though M13 begins where M11 stopped, and it does not.

`Op::Binary` and `Op::Unary` are now in the IR, and the lowering covers arithmetic, bitwise,
unary, logical, conditional and array literals. classes, and array *methods*.

### `this`, and a receiver the snapshot had been blessing

`Op::Call` now carries a `this_value` (D-82). **It had been missing, and the corpus snapshot had
been recording the wrong IR as correct since M11** — `o.a()` lowered to a property load and a
call with no receiver, so `this` inside would be wrong. A reviewed snapshot only catches what a
reader thinks to look for, and nobody looks for a field that does not exist yet. The failure is
silent: the call still happens and still returns something.

**`this`-binding is one flag.** A non-arrow *declares* `this` so it shadows; an arrow does not,
so `this` inside it resolves outward and becomes an ordinary capture. That is the whole rule,
and it is the payoff for `slot` and `declare` being different operations (D-81).

A test written as a substring match on `call v4(this=v1` passed for the wrong reason the moment
`this` became slot 0 and shifted everything. It is structural now.

### Closures, and where the capture analysis lives

**A name is a capture exactly when resolving it walks out of the current function's scope**
(D-81), so the lookup *is* the analysis — no free-variable pre-pass to keep in step. The whole
thing rests on separating two operations that look alike: `slot` *reads* a name and may capture;
`declare` *binds* one and always shadows. `let` and parameters declare, so
`let a = 1; (a) => a` captures nothing. Both directions mutation-tested.

Captures come back as **names**, because the inner function knows which slot they land in and
the enclosing one knows which value to put there — and resolving the name again outside is what
makes `() => () => a` capture at each level.

`verify_module` was added because a doc comment claimed the verifier checked closure arity and
that was **false**: the pairing is positional, a mismatch leaves a slot uninitialised, and no
single-function verifier can see it. The repair was to make the claim true.

**Hoisting is not modelled** — a call before a declaration reads an unset slot, recorded in
`unsupported` rather than left half-right, because a hoisting bug looks like a scoping bug.

**`Add` is typed `Unknown`, everything else `Number`.** `+` concatenates when either operand is
a string, so typing it `Number` would let codegen emit a float add for a string concatenation —
a miscompilation, not a slow path. `+` is also the only arithmetic operator that can collect,
because `ToPrimitive` calls user code.

**`&&`, `||` and `??` lower to branches** (D-80). Lowering them as instructions would evaluate
both operands, which changes what the program *does*. `??` tests nullishness rather than
falsiness — `0 ?? 1` is `0` — and emits explicit comparisons rather than branching on the value.
Both mutation-tested.

---

## Next session

1. Read `DECISIONS.md` and this file.
2. **Track A (M0–M8), M9, M10 and M11 are complete**, acceptances included. **M12 is under
   way: the object model is done, the builtins are not.** The next piece is the join between
   `Shapes` (fast path) and the descriptor table (general path) — see the M12 section. M12 also
   owns `mem2reg` (D-59) and shape-precise object types. Read D-55's last section first: the collector's safety rests on objects sitting behind
   checked handles, and M13 is where that assumption comes due. D-54's per-site monomorphic
   cache is M11's job and is what makes shape lookup fast. The rooting API is the part to get right
   rather than the collector — see §3.1 and the M9 section above.
   - **Track A's remaining gaps**, which are filed rather than buried:
     [#20](https://github.com/WertCore/crisol/issues/20) a paint-only style change still costs
     a relayout (the unfinished half of D-45, and the one with a written test for when it is
     fixed), [#16](https://github.com/WertCore/crisol/issues/16) nested rounded clips, and
     [#15](https://github.com/WertCore/crisol/issues/15) opening a window by hand on Windows
     and Linux. [#13](https://github.com/WertCore/crisol/issues/13) and
     [#14](https://github.com/WertCore/crisol/issues/14) need a human at a keyboard and a
     screen reader, so they cannot be closed from here at all.
3. Whichever comes first, run **the whole gate** before pushing — `fmt`, `clippy --workspace
   --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, both
   headless examples, and **`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`**. Two
   of five is how PR #25 failed on formatting alone.

   **The `RUSTDOCFLAGS` part is not decoration.** CI sets it and a bare `cargo doc` does not,
   so a broken intra-doc link is a warning locally and a failure there — a whole session's
   worth of doc runs were weaker than CI's before anyone noticed. The same applies to the test
   step: CI sets `CRISOL_REQUIRE_GPU`, `CRISOL_REQUIRE_FONTS` and `CRISOL_REQUIRE_NODE_MODULES`,
   and without them the tests that need those things *skip*.
4. For anything platform-shaped, add `cargo clippy --workspace --all-targets --all-features
   --target x86_64-unknown-linux-gnu` and `--target x86_64-pc-windows-msvc`. Neither needs a
   linker, both are ~400 MB, and together they caught two Windows/Linux-only failures before
   CI did. They are **not** a substitute for CI: clippy type-checks, so anything that only
   appears when a test *runs* is invisible to it.
