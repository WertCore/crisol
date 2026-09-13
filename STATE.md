# Crisol — State

**Current milestone:** M8 — platform polish (in progress: scrolling done)
**Last finished:** M7 — reactive API and component model

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

## In progress

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

**Still to do for M8:** native menus, drag and drop, window chrome, packaging (.app, .msi,
AppImage) — and then the acceptance proper: an API-client-shaped application with a 5MB
response in it, measured on all three platforms.

**One thing measured and left alone.** In the todo example, moving the selection runs one
effect per row: every row asks "am I the selected one?" and so subscribes to the shared
signal. The DOM layer absorbs the writes, so the cost is closure calls rather than relayouts,
but it is linear. That is what this way of modelling a selection costs, not a limit of the
engine — a list long enough to care would remember the previous row and toggle exactly two.
Said out loud in a comment rather than quietly shipped.

**Totals:** 506 tests passing

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
