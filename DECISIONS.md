# Crisol — Decisions

Load-bearing choices. Changing one of these means rewriting a large amount of work, so
each is recorded with the alternatives that were rejected and the consequences that were
accepted at the time.

**Format:** every decision gets a stable `D-nn` id, a status, and a record of what it costs
us. Append new decisions; do not renumber. When a decision is reversed, mark it
`Superseded by D-nn` and leave the original text in place — the reasoning is the valuable
part, not the conclusion.

Status values: `Accepted` · `Provisional` (may change once measured) · `Superseded`.

---

## D-01 — Garbage collection: precise mark-sweep with Cranelift stack maps

**Status:** Accepted (ROADMAP §2.1) · **Affects:** M9, M11, M13, M16, M19

JavaScript needs a tracing GC. Closures capture, objects cycle, promise chains form graphs.
AOT compilation does not remove this requirement.

Precise mark-sweep with compiler-emitted stack maps, using Cranelift's `r64` reference type
and safepoint support. Generational collection with a bump-allocated nursery comes later.

**Rejected — conservative (Boehm-style).** Fast to adopt, but leaks unpredictably, and
scanning the Rust stack for false pointers gets worse the more Rust code holds JS values.
The product claim is native memory efficiency; a leaky collector undermines it.

**Rejected — reference counting.** Pathological with closures and cycles. Needs a cycle
collector anyway, which is most of a tracing GC with extra steps.

**Consequences accepted:**

- Every heap-allocated JS value lives behind a `GcRef` handle, never a raw pointer.
- Native (Rust) code holding JS values roots them explicitly via a shadow stack.
- Codegen emits safepoints at calls, loop back-edges, and allocations.
- The FFI boundary is the hardest part and is designed in M9, before any host API work.

---

## D-02 — Codegen backend: Cranelift

**Status:** Accepted (ROADMAP §2.2) · **Affects:** M13, M20, M22

Pure Rust, compiles fast (which the dev loop needs), and has explicit safepoint and stack
map support designed for GC'd languages. That last point is decisive.

**Rejected — LLVM.** Better optimizer, but a heavy dependency, slow builds, and GC stack map
support that is workable but unpleasant.

**Rejected — compile to C.** What Static Hermes does, and it buys portability cheaply. But
precise GC stack maps through a C compiler are awkward, and build times get worse, not
better.

**Required design consequence:** codegen sits behind a `Backend` trait from M13. If M20
benchmarks show the optimizer is the bottleneck, add an LLVM backend for release builds and
keep Cranelift for dev builds — the same split rustc uses. Do not add a second backend
before benchmarks demand it; the GC statepoint work is the expensive part.

Cranelift target coverage (x64, aarch64) satisfies every platform in §1 including iOS and
Android arm64.

---

## D-03 — Dev builds interpret; release builds compile

**Status:** Accepted (ROADMAP §2.3) · **Affects:** M14, M16, M17

AOT compilation plus native linking takes seconds to minutes. Web developers expect
sub-second hot reload. These are irreconcilable, so there are two modes:

- `dev` — QuickJS via `rquickjs`, driving the same DOM host API. Fast iteration, HMR, real
  React DevTools.
- `release` — AOT compiled, no interpreter in the binary.

The no-interpreter promise applies to shipped artifacts, which is the only place users care
about it.

**Cost accepted:** two execution paths that must agree. Mitigated by the differential test
suite in M14, which runs the same programs through both and compares. Any divergence is a
P0 bug.

---

## D-04 — Don't write what already exists

**Status:** Accepted (ROADMAP §2.4) · **Affects:** everything

| Need | Use | Not |
|---|---|---|
| JS/TS/JSX parsing, scopes, symbols | `oxc` | hand-written parser |
| Module resolution | `oxc_resolver` | custom |
| CSS parsing | `lightningcss` | custom |
| Selector matching + cascade | `selectors` (Servo) | custom |
| HTML parsing | `html5ever` | custom |
| Flexbox/grid/block layout | `taffy` | custom |
| Text shaping | `swash` + `rustybuzz` via `cosmic-text` | custom |
| Glyph atlas + GPU text | `glyphon` | custom |
| Windowing, input | `winit` | custom |
| GPU abstraction | `wgpu` | raw Vulkan/Metal |
| Accessibility bridge | `accesskit` | custom |
| Dev-mode JS engine | `rquickjs` | custom |

The novel work is the IR, the optimizer, codegen, the GC, the runtime library, the DOM host
API, the document/text API, and the integration of all of it. That is more than enough.

---

## D-05 — Text is a public API, not an internal detail

**Status:** Accepted (ROADMAP §2.5) · **Affects:** M4, M6, M8

The eventual targets (PDF editor, document editor) live or die on text. Every serious web
editor — Google Docs, Figma, Notion — abandons `contenteditable` and renders text itself,
which means the webview was never providing the thing these apps need.

`crisol-text` therefore exposes shaped runs, cluster boundaries, cursor affinity, selection
rectangles, and line box geometry as a supported surface, not as internals.

---

## D-06 — The `Custom` node is a first-class escape hatch

**Status:** Accepted (ROADMAP §2.6) · **Affects:** M2, M3, M5, M6

A PDF page or document canvas must opt out of CSS layout entirely while still participating
in hit testing, scrolling, focus, clipping, and accessibility.

Designed in M2, not later. It is the single most important node kind for the eventual
product and the hardest to retrofit.

---

## D-07 — Performance claims we will and won't make

**Status:** Accepted (ROADMAP §2.7) · **Affects:** M8, M20 and all external communication

**Will claim:** small memory floor, fast cold start, small binaries, one rendering engine
identical on every platform, no system webview dependency, works on iOS where JIT is banned.

**Will not claim:** faster than V8 on hot code. A JIT observes real runtime types and
specializes; an AOT compiler without profile data cannot. Untyped AOT JS is closer to a
flattened interpreter than to optimized native code. Wins come from typed paths and from
eliminating startup and runtime overhead.

---

## D-08 — Support `Proxy`, and make the check cheap

**Status:** Provisional — revisit when measured at M20 (ROADMAP §3.2) · **Affects:** M9, M12, M20

`Proxy` cannot be rejected at build time if the ecosystem is a goal: Vue 3's reactivity,
MobX, Immer, Valtio and Solid stores all depend on it. But supporting it means every
property access must check whether the receiver is exotic, which erases the specialization
that makes AOT worthwhile.

Object shapes carry an `is_exotic` bit; the fast path branches on it once. Programs that
never construct a `Proxy` pay one predictable branch.

**Do not promise Vue support until this is measured.**

---

## D-09 — Mobile constraints bind from M1, not from M22

**Status:** Accepted (ROADMAP §3.6) · **Affects:** every architectural decision in Tracks A and B

iOS prohibits JIT for third-party apps, which is precisely where AOT's value is unambiguous.
Mobile is scheduled late only because the desktop path validates the architecture faster.

Nothing may assume desktop:

- **No `mmap`-with-exec anywhere.** All generated code is AOT and linked, never emitted at
  runtime. Do not let a dev-mode shortcut violate this.
- **Touch, gesture, and soft-keyboard input designed into the event model at M5**, not
  bolted on — momentum scrolling, touch targets, pointer cancellation, safe-area insets.
- **Renderer must tolerate tile-based deferred GPUs.** Avoid mid-pass render target
  switches, frequent readback, and large overdraw — cheap on desktop immediate-mode GPUs,
  catastrophic on mobile tilers.
- **Memory pressure is a termination risk, not a slowdown.**
- **Windowing abstraction must not assume a resizable desktop window.** Keep platform
  assumptions out of `ui/` entirely.
- **arm64 only.** 32-bit ARM is not supported and is not required.

"Would this work on iOS?" is a review question for every change.

---

## Decisions made during implementation

Decisions below were not in the roadmap. They were forced by contact with the code.

---

## D-10 — Rust edition 2024, workspace-inherited metadata and lints

**Status:** Accepted (M0) · **Affects:** every crate

All crates inherit `version`, `edition`, `rust-version`, `license`, `repository` and
`[lints]` from `[workspace.package]` / `[workspace.lints]`. `missing_docs` is `warn` in the
workspace and promoted to an error in CI via `-D warnings`, so documentation rots loudly
rather than silently.

Internal crates are declared in `[workspace.dependencies]` with both `path` and `version`,
which is what publishing to crates.io later requires. Doing it now costs nothing; doing it
at publish time means touching 25 manifests.

---

## D-11 — The umbrella crate gates the renderer behind a feature

**Status:** Accepted (M0) · **Affects:** M1, M8, M22

`crisol-ui` re-exports Track A. Its `render` feature (default on) gates `crisol-render-wgpu`,
and therefore `wgpu` and `winit`. A consumer that wants the tree, style, layout and paint
pipeline without a GPU or a window — a headless layout service, a snapshot test, a PDF
rasterizer — turns it off and does not compile a graphics stack it will not use.

---

## D-12 — Geometry types live in `crisol-display-list`, not in a separate crate

**Status:** Accepted (M1) · **Affects:** M1, M2, M3, M5

`Point`, `Size`, `Rect`, `Color` and `Transform` are needed by the tree, layout, paint,
events and the renderer. They could live in a `crisol-geom` crate.

They live in `crisol-display-list` instead, which has no dependencies of its own and sits
below everything that needs them. A separate geometry crate would be a 26th manifest
carrying about 300 lines. Revisit if a crate needs geometry but must not depend on the
display list; nothing does today.

---

## D-13 — The renderer consumes a display list; it does not know about the tree

**Status:** Accepted (M1) · **Affects:** M1, M2, M6, M22

`crisol-render-wgpu` takes a `DisplayList` and nothing else. It has no reference to
`crisol-tree`, no style, no layout. This keeps the GPU backend swappable, makes render
snapshot tests possible without constructing a document, and is what lets M6's damage
regions be expressed as a property of the display list rather than of the renderer.

---

## D-14 — One pipeline, one instanced draw call for rectangles

**Status:** Accepted (M1) · **Affects:** M1, M6, M22

Rounded rectangles, borders and solid fills are drawn by a single shader that
signed-distance-fields the rounded box, fed by a per-instance vertex buffer. One pipeline,
one draw call per clip group, no mid-pass render target switches.

This is D-09's tile-GPU constraint applied concretely: a naive "one draw call per node"
renderer is acceptable on a desktop immediate-mode GPU and pathological on a mobile tiler.
Building it instanced from the start costs a day at M1 and avoids a rewrite at M22.

---

## D-15 — Premultiplied alpha in the display list, sRGB surface

**Status:** Accepted (M1) · **Affects:** M1, M3, M4

`Color` stores straight (non-premultiplied) `f32` RGBA components in the sRGB color space
because that is what CSS authors write and what `lightningcss` will hand us. Premultiplication
happens in the shader, and the surface is configured with an sRGB texture format so the GPU
does the encode. Blending is therefore correct without any manual gamma arithmetic in the
display list.

---

---

## D-16 — Android entrypoint: GameActivity, not NativeActivity

**Status:** Accepted (M1) · **Affects:** M5, M22

`android-activity` refuses to compile unless the application picks one, so this was forced
the first time CI checked the `aarch64-linux-android` target — which is exactly what that
per-commit check exists to do (D-09). Deciding it at M1 rather than M22 is the point.

`winit`'s `android-game-activity` feature, which selects AndroidX `GameActivity`.

**Rejected — `NativeActivity`.** Simpler, no AndroidX dependency, and the obvious default.
But its text input is the part of the Android platform it handles worst: it cannot properly
drive the IME, which is why `GameActivity` exists. ROADMAP §2.5 says text is the core
competency and §3.6 requires soft-keyboard and IME input designed into the event model at
M5. A document editor that cannot take dictation from the Android keyboard is not a
document editor, so the simpler option is disqualified on the one axis that matters most.

**Consequences accepted:**

- Packaging at M22 must pull in the AndroidX `games-activity` AAR; a bare NDK build will not
  be enough.
- `GameActivity` compiles C++ glue, so an Android build needs the NDK's toolchain, not just
  the Rust target.
- The M5 event model must be written against `GameActivity`'s input API. Since M5 is where
  touch and IME are designed anyway, this constrains work that has not started rather than
  work that has.

---

## D-17 — Generational `NodeId`, sibling links, no `Vec<NodeId>` children

**Status:** Accepted (M2, mandated by ROADMAP §M2) · **Affects:** M2, M6, M16

Nodes live in an arena. `NodeId` carries a generation counter, so a handle to a removed node
fails a liveness check rather than silently addressing whatever was allocated into the slot
next — the failure mode that matters once JS holds DOM handles (M16).

Children are a `first_child` / `last_child` / `next_sibling` / `prev_sibling` intrusive list,
not a `Vec<NodeId>` on the parent. Inserting or removing in the middle of a 400-page
document's child list is then O(1) pointer work instead of an O(n) memmove, which is the
whole point of M6.

---

## D-18 — Dirty tracking is a bitflag set per node plus an ancestor-marked subtree bit

**Status:** Provisional — M6 will extend it (M2) · **Affects:** M2, M3, M6

Each node carries `DirtyFlags` (`STYLE`, `LAYOUT`, `PAINT`, `SUBTREE_*`). Marking a node
dirty sets its own bit and walks to the root setting the corresponding `SUBTREE_` bit, which
lets a pass skip a clean subtree in O(1) instead of visiting it.

The walk is O(depth), and it is done on mutation rather than on traversal, because mutation
is the rarer operation in a document editor's steady state (one keystroke, one dirty text
node, a 400-page tree that must not be walked).

M6 replaces the "recompute everything dirty" consumers with real invalidation, but the flag
vocabulary is fixed here so consumers do not have to change.

---

## D-19 — `CustomNode` is an object-safe trait with a measure/layout/paint/hit-test contract

**Status:** Accepted (M2, mandated by ROADMAP §2.6) · **Affects:** M2, M3, M5, M6

`CustomNode` is stored as `Box<dyn CustomNode>` in the node arena. Its four methods are the
complete protocol between the engine and a node that opts out of CSS layout:

- `measure(constraints) -> Size` — called by layout, mirrors taffy's measure function so M3
  can hand it straight to taffy
- `layout(size)` — the node lays out its own interior once the engine has given it a box
- `paint(bounds, &mut DisplayListBuilder)` — the node emits draw commands directly
- `hit_test(local_point) -> Option<CustomHit>` — the node resolves a point inside itself to
  something meaningful, so M5 hit testing does not stop at the custom node's boundary

`measure` takes constraints rather than a fixed size because a document canvas needs to know
available width to decide its own height. Deciding this at M2 is the point of §2.6.

---

## D-20 — Match with the upstream `selectors` crate and a closed selector dialect

**Status:** Accepted (M3) · **Affects:** M3, M5, M6

ROADMAP §2.4 says to use `lightningcss` for CSS parsing and Servo's `selectors` for matching.
Those turn out not to be the same crate: lightningcss embeds `parcel_selectors`, Parcel's
fork, and defines its `SelectorImpl` in a private module. The type is reachable through
public aliases but cannot be *named*, so `selectors::Element` cannot be implemented against
lightningcss's already-parsed selectors from outside the crate.

lightningcss parses stylesheets and declarations. Matching uses the upstream `selectors`
crate with a `SelectorImpl` of our own. Both depend on `cssparser` 0.37, so there is exactly
one tokenizer in the dependency graph.

**Rejected — vendor or fork lightningcss** to expose its `SelectorImpl`. Cheap today,
a merge burden on every upgrade of a crate that is still pre-1.0.

**Rejected — Servo's `stylo`** for the whole cascade. It solves this and much more, and it
is an enormous dependency built around Gecko's constraints. ROADMAP §2.4's list is about not
writing a parser or a matcher, not about not owning a cascade.

**Rejected — write the matcher.** §2.4 says no, and it is right: selector matching is
subtle in exactly the ways that produce bugs nobody can reproduce.

**Consequence, and the reason this is a decision rather than a workaround:** owning the
`SelectorImpl` means owning the *dialect*. The supported pseudo-classes are an explicit
allowlist — `:hover`, `:active`, `:focus`, `:focus-within`, `:focus-visible`, `:disabled`,
`:enabled`, `:checked`, `:invalid` — and everything else is a parse error with a source
location rather than a selector that silently never matches. That list is deliberately the
same set as `crisol-tree`'s `ElementState` bitflags: a pseudo-class the engine cannot answer
from a node does not exist.

Absent on purpose:

- `:visited`, `:target` — there is no history and no fragment navigation (ROADMAP §1).
- `::before`, `::after` — generated content needs a box in the tree. Adding them is a tree
  change, not a parser change, so they fail loudly until that work is done.
- `:has()` — it makes the matcher look *down* the tree, which turns invalidation from an
  ancestor walk into a subtree scan. Revisit at M6, when invalidation exists and the cost
  can be measured rather than guessed.

`:is()` and `:where()` are supported: they are how a stylesheet avoids the combinatorial
blow-up that makes large selector lists slow.

---

## D-21 — Computed style is an interned `Arc` in a side table, not a field on the node

**Status:** Accepted (M3) · **Affects:** M3, M6, M16

ROADMAP §M3 requires that a hundred identically-styled nodes share one `ComputedStyle`
allocation, and is explicit that this is not an optimisation to add later: per-node computed
style at document-editor scale is hundreds of megabytes, which contradicts the entire product
thesis. A four-hundred-page document is mostly paragraphs that compute to the same style.

`StyleEngine` holds a `StyleInterner`, a hash map keyed on the style itself. `restyle`
produces a `NodeMap<Arc<ComputedStyle>>` — a side table indexed by arena slot — rather than
writing a style onto each `Node`.

**Rejected — a `ComputedStyle` field on `Node`.** Simpler to reach, and it would put the
allocation back per node, which is the thing being avoided. It would also force `crisol-tree`
to depend on `crisol-style`, inverting the layering: the tree is below the cascade and has to
stay usable without it.

**Rejected — interning by hash only, without storing the key.** Half the memory, and a hash
collision silently gives two different styles the same allocation. A rendering bug that
appears only for particular pairs of styles is not worth the saving.

**Consequences, accepted now:**

- **`ComputedStyle` and everything in it must be `Eq + Hash`.** That is why `crisol-style`
  has `Px` and `Number` newtypes rather than bare `f32`: floats are compared and hashed by
  bit pattern, which is a true equivalence relation only if no NaN is present. `Px::new`
  therefore rejects non-finite input — `calc(1px / 0)` can produce one — and normalises
  `-0.0` to `0.0`, since the two have different bit patterns and would otherwise be two
  distinct styles that look identical.
- **The side table does not hear about node removal.** `NodeMap` guarantees that a new node
  reusing a slot never reads its predecessor's value, which is the property that matters. A
  handle to a removed node still reads its own stale entry until the slot is reused. Passes
  clear the map, or check the tree first — which they do anyway, since they need the node.
- **The interner needs sweeping.** Restyling churns styles, and without
  `StyleInterner::collect_unused` the map grows for the life of the process. M6 decides when
  to call it.

## D-22 — Percentages reach layout unresolved; font-relative units do not

**Status:** Accepted (M3) · **Affects:** M3, M4

A percentage resolves against a containing block that layout has not measured when the
cascade runs, so `width: 50%` stays a percentage in `ComputedStyle` and `taffy` does the
arithmetic. `em` and `rem` resolve against font sizes, which the cascade *does* know, so
they become pixels immediately.

The consequence worth remembering is that `em` inside `font-size` means the **parent's**
font size, while `em` everywhere else means this element's — otherwise `font-size: 1.5em`
would be circular. There is a test for each.

`line-height` is the exception that proves the rule: it inherits as the *multiple*, not as
the resolved length, so a child with a larger font gets a proportionally larger line box.
That is the whole point of writing `line-height: 1.5`, and resolving it during the cascade
would quietly break it.

---

## D-23 — A custom node is an element with a painter, not a thing outside the document

**Status:** Accepted (M3) · **Affects:** M3, M5, M6, M16

`NodeKind::Custom` originally held only a `Box<dyn CustomNode>`. That made a custom node
invisible to the cascade: it matched no selector, so it could not be given a `width`, a
`margin` or an `overflow`, and layout fell back to initial values for it. ROADMAP §2.6 asks
for an escape hatch that still participates in hit testing, scrolling, focus, clipping and
accessibility — and clipping is a *style*.

`NodeKind::Custom(CustomElement)`, where `CustomElement` carries an `ElementData` alongside
the painter. `<canvas>` is the model: an element with a painter attached. It has a tag,
classes and attributes, it matches selectors, and the cascade styles it.

**Rejected — leave it outside the document and style it some other way.** Every mechanism
that would have worked — a parallel style API, inheritance-only styling — is a second way to
say something CSS already says.

**Consequence:** `Tree::create_custom` takes a tag. A PDF page is `create_custom("canvas",
…)` and `canvas { width: 100% }` applies to it.

## D-24 — Custom nodes are replaced elements

**Status:** Accepted (M3) · **Affects:** M3, M4

A block-level box with `width: auto` stretches to its container. A custom node that did so
would make a PDF page the width of the window rather than the width of the page, which
defeats the point of asking it to measure itself.

Custom nodes are therefore *replaced* elements, like `<img>`: `width: auto` resolves to the
intrinsic size their `measure` reported. `StyleRef` carries a `replaced` bit that the cascade
cannot know — it is a property of the node kind, not of any declaration — and taffy is told
`is_block() == false` and `is_compressible_replaced() == true` for them.

CSS still has the final word: an explicit `width` overrides the measured one, because
`measure` is a request and the engine decides the box. There is a test for each direction.

## D-25 — A one-rule user-agent stylesheet, and why it is exactly one rule

**Status:** Accepted (M3) · **Affects:** M3 onwards

A browser's user-agent stylesheet is hundreds of rules because it has to make a document
written in 1998 render sensibly. This engine renders applications and owes nothing to that
(ROADMAP §1), so every rule has to earn its place by describing something true of *this*
engine rather than of HTML.

Exactly one rule qualifies today: `:root { width: 100%; height: 100% }`.

In a browser the root has `height: auto` and shrinks to its content, with the viewport merely
being what you can see of it. In an application the root *is* the window: without this, a
layout has no way to say "fill the space", and every author would open with this rule anyway.
Leaving it out produces a root box shorter than the window — subtly wrong rather than
obviously broken, which is the worst kind of default.

`StyleEngine::new()` loads it; `StyleEngine::without_user_agent_styles()` exists for tests
that want to see raw behaviour. An author who wants a content-sized root writes
`:root { height: auto }`, which wins because author rules beat user-agent rules regardless of
specificity.

---

## D-26 — Rounded clipping is a per-fragment test, not a stencil pass

**Status:** Accepted (M3 follow-up) · **Affects:** M3, M6, M22 · **Supersedes** the
axis-aligned-only note in D-14

A card with `border-radius` and `overflow: hidden` has to cut its content at the corners. A
scissor rectangle cannot: it leaves square corners with the content showing through. Until
M3 the engine had `border-radius` and `overflow` but clipped square, so this rendered wrong
without saying so.

The clip's axis-aligned bounds stay a scissor rectangle, and the corners become a
signed-distance test in the fragment shader, fed by two more instance vectors.

**Rejected — a stencil buffer.** The standard answer, and it costs a depth-stencil
attachment plus a second pass over the clipped geometry. Mid-pass attachment changes are
exactly what D-09 rules out for tile-based mobile GPUs.

**Rejected — a separate render pass per rounded clip.** Same objection, worse.

**Consequences accepted:**

- **One set of corners is honoured at a time.** The intersection of two rounded rectangles is
  not a rounded rectangle, so a rounded clip nested inside another keeps the innermost
  corners and intersects only the bounds. In practice an outer rounded clip's corners lie
  outside the inner one; there is a test pinning the intersection behaviour so the day that
  stops being true is a failing test rather than a rendering artefact.
- **A clip carries the box its radii were written against**, separately from the intersected
  bounds. A corner radius belongs to the box it was declared on, and clipping a rounded card
  to a smaller ancestor must not move its corners.
- Instances grow by 32 bytes. Still one pipeline, still one draw call per clip group.

## D-27 — Borders carry a colour per edge

**Status:** Accepted (M3 follow-up) · **Affects:** M3, M8

`BoxStyle` originally carried one border colour, taken from the top edge. That makes
`border-bottom: 1px solid #ddd` — one of the most common declarations there is — render in
whatever the *top* edge computed to, which is normally black, because the other three edges
keep their initial `currentColor` while only the bottom has a width.

`Edges4<Color>`, and the shader picks per fragment.

Edges meet on the **miter diagonal**, which is what CSS draws. The fragment belongs to
whichever edge it has travelled the smallest *fraction* of the way across; comparing
fractions rather than distances is what makes a thick top and a thin left edge meet on the
correct slope instead of at forty-five degrees.

**Rejected — four draw commands, one per edge.** No shader change, and it quadruples the
instance count for every bordered box while making rounded corners a special case at each
join.

---

## D-28 — Text positions are byte offsets, and cursors carry affinity

**Status:** Accepted (M4) · **Affects:** M4, M5, M6, M16

ROADMAP §2.5 makes the text layer a public API rather than an internal detail, so the
vocabulary it uses is a long-lived commitment. Two choices in it are load-bearing.

**Positions are byte offsets into the source string**, always on a `char` boundary.

- **Rejected — `char` counts.** Indexing a `String` by `char` is O(n), and every caller
  already holds the bytes.
- **Rejected — UTF-16 offsets.** What the DOM uses, and what M16 will have to convert to.
  Doing it now would make every Rust-side caller pay for a conversion that only JavaScript
  needs, at the one boundary where it is cheap to do instead.

**A cursor is an offset plus an [`Affinity`].** One byte offset can be two places on screen:
at a soft wrap it is both the end of one line and the start of the next, and at a direction
change in bidirectional text it is both the end of one run and the start of another. Affinity
is how a caller says which. Dropping it is why so many editors put the caret in the wrong
place at the end of a wrapped line, and it cannot be added later without changing every
signature that mentions a position.

**`Direction` is on every run from the first version**, even though bidirectional layout is
deferred to M8 (ROADMAP §M4 permits deferring it, but requires that the API not assume LTR).
An API that assumes left-to-right cannot be extended to one that does not, because every
caller will have baked the assumption into its own arithmetic by then. A test asserts that
Arabic is *reported* as RTL today, even though laying it out correctly is M8's work.

**A cluster is a byte range, not an offset.** The mapping is not one to one in either
direction: `é` as `e` plus a combining accent is two characters and one glyph, a ligature is
several characters and one glyph, and an emoji with a skin-tone modifier is several
characters and several glyphs that must not be split. Callers step by cluster, so one press
of an arrow key moves past a whole grapheme rather than leaving the text visibly broken.

## D-29 — Tests assert relations, not pixel positions

**Status:** Accepted (M4) · **Affects:** M4 onwards

Text tests run against whatever fonts the machine has, and those differ between a developer's
laptop and a CI runner. A test that pins an advance width to two decimal places tests the
font, not the engine, and fails for the wrong reason on a machine that happens to have a
different one.

So the text suite asserts *relations*: glyph positions increase monotonically, clusters tile
the string with no gaps, a cursor round-trips through a point, selection rectangles line up
with the lines they cover, hit testing anywhere along a line always lands on a cluster
boundary. Those hold for any font.

`CRISOL_REQUIRE_FONTS=1` turns a machine with no fonts from a skip into a failure, the same
way `CRISOL_REQUIRE_GPU` does for the renderer. A suite where everything silently skipped is
indistinguishable from one where everything passed.

---

## D-30 — One glyphon renderer per text run, so text keeps painter's order

**Status:** Accepted (M4) · **Affects:** M4, M6, M22

A `glyphon::TextRenderer` prepares a set of text areas and then draws *all of them with one
call*. A single renderer can therefore only put **all** text above or below **all**
rectangles — so a label on a card would either vanish behind the next background or float on
top of a modal that should cover it.

`crisol-text-gpu` pools renderers and uses one per run of text in the display list, drawing
each where it appears. The pool is kept across frames, so a steady-state frame allocates
nothing.

**Rejected — one renderer, all text last.** Free, and wrong in a way that only shows up on
overlapping content, which is exactly the kind of bug that survives to a release.

**Rejected — a render pass per text run.** glyphon's examples sometimes do this. It is a
mid-pass render target switch, which D-09 rules out for tile-based mobile GPUs.

The atlas, the rasterisation cache and the viewport are shared across all the renderers, so
the cost of an extra run is a vertex buffer rather than a second copy of every glyph.
`GlyphRenderer::trim` runs after each frame: without it the atlas grows to the union of every
glyph ever drawn, which for a document editor is the entire font.

## D-31 — The display list refers to text by handle, exactly as it does to images

**Status:** Accepted (M4) · **Affects:** M4, M6, M16

Shaping is expensive and happens during layout. A display list that carried glyphs would
either duplicate them every frame or pin the list to the lifetime of the layout that produced
them, and D-13 says the list is rebuilt every frame and knows nothing about the tree.

`DrawCommand::Text` carries a `TextId`, the same arrangement `ImageId` already uses. Paint
writes the text node's own packed handle (`NodeId::to_bits`, D-17's integer form), and
`crisol-layout::ShapedText` looks the shaped layout back up with it.

The correspondence between those two is asserted rather than assumed
(`paint_refers_to_text_by_the_nodes_own_handle`): if the two ever disagree the text silently
vanishes, which is not a failure mode worth debugging twice.

---

## D-32 — Pointers, not mice

**Status:** Accepted (M5) · **Affects:** M5, M7, M16, M22

ROADMAP §3.6 requires touch and gesture input designed into the event model at M5 rather
than bolted on for M22. This is what that means concretely: **there is no mouse event.**

A finger, a stylus and a mouse all produce `PointerEvent`s carrying a `PointerKind` and a
`PointerId`. An application written against a mouse today is already written against a
finger, and multi-touch needs no new event type — only more ids.

**`PointerCancel` is a distinct event, not a variant of `PointerUp`.** The system takes the
pointer away when a gesture is recognised, a call arrives, or the window loses focus
mid-drag. A cancelled press is *not* a click, and conflating the two is why so many
interfaces leave a button stuck looking pressed after an interruption. §3.6 names pointer
cancellation specifically.

**Rejected — mouse events now, touch events beside them at M22.** What the web did, and the
reason every web application carries a compatibility layer reconciling the two. The engine
would have inherited that at exactly the point it could least afford to.

`TextInput` is likewise separate from `KeyDown`, because they are not the same question: a
key press may produce no text, and text may arrive with no key press at all — which is
precisely what an IME does.

## D-33 — Listeners are a trait, and interaction state is a side table

**Status:** Accepted (M5) · **Affects:** M5, M7, M16

`Listener` is a trait, not a `Box<dyn FnMut>`. At M16 a listener is a JavaScript function and
the runtime decides how to call it; an engine that had baked a Rust closure into its dispatch
signature would need rewriting to get there. A blanket impl means a closure still works
wherever one is convenient.

Hover, focus and press state live in `EventSystem`, not on the node — the same arrangement
as computed style (D-21), and for the same reason: `crisol-tree` sits below the event system
and has to stay usable without it.

`apply_state` then writes those into each element's `ElementState`, which is what the cascade
has been matching `:hover`, `:focus`, `:focus-within` and `:active` against since M3 and
always getting `false` for. It touches only the four interaction bits; `DISABLED`, `CHECKED`
and `INVALID` belong to whoever set them.

Hover is kept as the whole ancestor chain rather than the deepest node, because `:hover` on a
container is a real thing authors rely on. Focus is kept as a single node and the chain
derived, because only one node can have focus but every ancestor is `:focus-within`.

---

## D-34 — Focus order is document order; a positive `tabindex` does nothing

**Status:** Accepted (M5) · **Affects:** M5, M7, M16

ROADMAP §M5's acceptance is *a form with three text inputs is fully keyboard-navigable*, and
§M5 puts accessibility here rather than in year three precisely because it constrains the
focus model.

Tab follows **document order**. `tabindex="0"` makes a node focusable and `tabindex="-1"`
makes it focusable by pointer and script but not by Tab. A **positive** `tabindex` is parsed,
accepted, and then treated as `0`.

A positive `tabindex` lets an author reorder the keyboard sequence independently of the
document, which is the single most reliable way to produce an interface that cannot be used
with a keyboard: the tab order stops matching the reading order, and a screen reader user
hears one thing while the focus ring goes somewhere else. Every accessibility guideline says
not to use it. Honouring it would mean the engine's own conformance depended on authors
declining a feature it offered them.

**Rejected — honour it.** Correct by the letter of the spec, wrong by its purpose, and this
engine owes the spec nothing (ROADMAP §1).

**Rejected — reject it as a parse error.** Tempting, and it would break real documents that
carry a harmless `tabindex="1"` on a single element. Treating it as `0` keeps the element
focusable, which is what the author was reaching for, without letting them reorder anything.

A disabled control is not focusable, a control whose subtree is `visibility: hidden` is
skipped, and one whose subtree is `display: none` is not there at all — which is what
`BoxStyle::generates_box` was added for. A focus ring on empty space is worse than no focus
ring.

---

## D-35 — Composition is a state machine, not a stream of keystrokes

**Status:** Accepted (M5) · **Affects:** M5, M16

ROADMAP §M5's acceptance names IME composition for Japanese on all three platforms, and it is
called out because an input method is not a keyboard with extra steps. The user types several
keys, the system shows *provisional* text that is not in the document, and only later does
that text commit — or get abandoned.

An engine that treats composition as ordinary key input gets three things wrong at once: the
provisional text ends up in the document, undo gets an entry per keystroke instead of per
word, and cancelling leaves the abandoned text behind.

So `ImeState` holds the preedit, and `apply` returns text to insert **only** on a commit.
Three consequences that each correspond to a bug applications actually ship:

- **A preedit replaces rather than appends.** An input method sends the whole provisional
  string each time, not a delta. Appending is how `にほん` becomes `にには ほにほん`.
- **A cancel is not an empty commit.** They mean opposite things to undo: an abandoned
  composition never happened, while an empty commit is a deletion.
- **Moving focus resets composition.** The provisional text belongs to the node that was
  focused; carrying it across pastes half a word into the next one.

`TextInput` being a separate event from `KeyDown` (D-32) is the other half of this: while
composition is in progress, ordinary key handling must not also run, or the character is
typed twice.

## D-36 — The accessibility tree is a second, smaller tree

**Status:** Accepted (M5) · **Affects:** M5, M6, M7

§M5 puts accessibility at M5 rather than in year three because *it constrains the tree, focus
model, and event system, and retrofitting means restructuring*. By the time the bridge was
written those constraints had already been paid — focus order is document order (D-34),
`display: none` genuinely removes a node, and the tree walks in reading order — so the bridge
is a translation rather than a redesign. That is the point of doing it now.

What a screen reader gets is a **second tree**, smaller than the box tree: only the things a
user can perceive and act on. A `<div>` used for spacing is not one of those, and a tree that
announced every one would drown the content in structure. Skipped nodes' children float up to
take their place, so a wrapper disappears without taking its contents with it.

**Full updates, not incremental ones.** M6 owns invalidation; building a diff now would mean
maintaining a cache against a story that does not exist yet, and the failure mode is a button
that announces the wrong label to one user and is never reproduced by anyone else.

**Node handles are the same packed integers** taffy takes and JavaScript will get at M16
(D-17). A screen reader routes "activate this" back by id, and a mapping that is not
reversible sends the action nowhere; there is a test for the round trip.

**What cannot be tested here:** whether VoiceOver, NVDA and Orca actually announce it
correctly. No CI runner has a screen reader attached. What is asserted is the `TreeUpdate`
they consume — roles, labels, nesting, state and focus. If that is right, the announcement is
the platform's problem. Verifying it against a real screen reader is a manual step and is
recorded in `STATE.md` as outstanding.

---

## D-37 — The layout cache belongs to the caller, not to the pass

**Status:** Accepted (M6) · **Affects:** M6, M7, M8

`LayoutContext` held taffy's measurement caches and borrowed `&mut Tree`. That makes the one
sequence M6 is about impossible: lay out, **mutate the tree**, lay out again. The mutation
needs the tree, and the context is holding it.

`LayoutCache` is now a separate thing the caller owns and lends to each pass. A frame loop
keeps one; the convenience `layout()` builds one, uses it and throws it away, which is honest
about being a full pass every time.

This was found by writing M6's acceptance test and discovering it could not be expressed.
That is the useful kind of test failure — the API was wrong, not the assertion.

## D-38 — Invalidation is conservative in a shape the selector dialect guarantees

**Status:** Accepted (M6) · **Affects:** M6, M7

A change to something a selector can see — a class, an id, an attribute, a state bit — can
affect:

- **the node itself**;
- **its descendants**, through descendant and child combinators (`.open .panel`);
- **its following siblings**, through the sibling combinators (`.open + .panel`, `.open ~ x`).

It cannot affect its ancestors or its *preceding* siblings, because **no combinator in the
supported dialect looks backwards or upwards**. That is not an accident: it is why `:has()` is
excluded (D-20). One selector would turn this from an O(subtree) walk into an O(document)
one, and the cost would be paid on every class toggle rather than only by documents that use
it.

Two other invalidation rules, each corresponding to a selector that would otherwise go stale:

- **Text becoming empty or stopping being empty dirties its parent's style**, because
  `:empty` matches on whether an element has content. An ordinary edit — non-empty to
  non-empty — dirties no style at all, which is what makes a keystroke in a large document
  cost zero cascade work.
- **Inserting or removing a child dirties all of the parent's children**, because
  `:first-child`, `:last-child` and `:nth-child` depend on position among siblings. Their
  *descendants* are left alone: a grandchild's position among its own siblings did not
  change.

**Inheritance is what makes the style walk subtle.** A subtree can be entirely clean and still
need recomputing, because its parent's style changed and half the properties inherit. So the
walk carries whether the inherited style actually changed, and stops descending only when the
subtree is clean *and* the inherited style is the same allocation as last time — which the
interner (D-21) makes a pointer comparison rather than a field-by-field one.

## D-39 — Damage is the union of old and new boxes, in absolute coordinates

**Status:** Accepted (M6) · **Affects:** M6, M8

A box that shrank or moved leaves pixels behind that have to be painted over, so the damaged
region is the union of where a box **was** and where it **is**. Taking only the new box leaves
a ghost of the old one on screen.

Boxes are stored relative to their parent (D-16), which is what makes moving a subtree one
write. Damage cannot be: the renderer scissors in absolute coordinates, and unioning
parent-relative rectangles produces a region that means nothing. The write-back walk
therefore accumulates the absolute origin as it descends.

That bug was live and passing its test until the test was strengthened to use real fonts —
with an empty font system every string measures to nothing, so no box ever changed and the
assertion held vacuously. Worth remembering: a test whose fixture cannot produce the
condition it checks for is not a test.

---

## D-40 — A damaged frame loads and repaints its own background

**Status:** Accepted (M6) · **Affects:** M6, M8

A render pass's load op covers the **whole attachment** and ignores the scissor. So a damaged
frame cannot clear: doing so would wipe exactly the region it is trying to preserve. It loads
instead, and `crisol-paint` emits a background rectangle covering the damaged region before
anything is drawn over it.

Putting that in paint rather than the renderer keeps the background colour in one place. The
renderer would otherwise have to inject an instance at index zero and shift every batch index
to match.

**The precondition, which a caller has to satisfy:** the surface must still hold the previous
frame. That is true of an offscreen target and true of a swapchain only when the present mode
preserves it. `FrameTarget::damage` documents that a caller who is unsure should pass `None` —
a stale region on screen is a worse bug than a slow frame.

## D-41 — Damage has three states, not two

**Status:** Accepted (M6) · **Affects:** M6

`Option<Scissor>` had to mean both *no damage region, so redraw everything* and *the damage
region clips to nothing, so draw nothing*. Those are opposites, and conflating them made an
off-screen damage rectangle repaint the entire surface — the exact opposite of what was asked
for.

`Damage` is therefore `Everything | Region(..) | Nothing`, and the `Nothing` case returns
before a pass is even begun: submitting an empty pass still costs a load and a store of the
whole attachment, which on a tile-based GPU is the expensive part (D-09).

The bug was live and invisible until a test asked what happens when the damage is entirely
off screen. Worth remembering as a shape: an `Option` whose `None` means two different things
is a bug waiting for the second meaning to occur.

Paint's culling has the mirror-image subtlety. A subtree whose own box misses the damage may
still contain something that hits it, because `overflow: visible` is the initial value and a
child overflowing its parent is the common case. So the test descends — but stops immediately
at a node with `overflow: hidden`, which confines its descendants and therefore cannot hide
anything that reaches further.


## D-42 — All mutation goes through `crisol-dom`, not through `Tree`

**Status:** Accepted (M7) · **Affects:** M7, M16

A mutation has to mark what it invalidates, and the rules are not guessable: changing a class
affects the node, its descendants and its *following* siblings; inserting a child affects the
parent's other children through `:nth-child`; text becoming empty affects the parent through
`:empty` (D-38). `Tree::element_mut` hands out a `&mut` that enforces none of it, and a caller
who forgets gets a stale style that reads as a cascade bug rather than as a missing call.

`crisol-dom` is the API that cannot forget. It is also the surface ROADMAP §M7 asks to be
designed *as if an external consumer exists* — the consumer is the JS runtime at M16, and
having a Rust caller drive it first is how the shape gets corrected while that is still cheap.

**Rejected: a `&mut Tree` with a convention.** Conventions are not enforced by anything, and
the failure mode is silent and delayed.

Two properties fell out of writing it that were not the original motivation. Writes are
compared before they are applied, so setting a value to what it already is costs nothing —
which matters because a reactive system makes redundant writes constantly, and it is what lets
the reactive layer notify unconditionally rather than requiring `PartialEq` everywhere.
And the counters (`DomStats`) are what M7's acceptance is actually measured in: an engine that
rebuilt the world would render the same pixels.

## D-43 — The reactive runtime is a value, not an ambient thread-local

**Status:** Accepted (M7) · **Affects:** M7, M16

Signals are ids into a `Runtime` that is passed by reference. The ergonomic alternative — a
thread-local current runtime, which is what makes `signal.get()` work without an argument in
most Rust reactive libraries — was rejected.

Two windows means two runtimes, and an ambient one turns that into a silent cross-wiring
rather than a type error. More to the point, M16 puts a JS engine behind this: a host function
has to say which runtime it is driving rather than inherit whichever thread it happens to be
called on.

The cost is real and visible at every call site: `track.get(signal)` instead of `signal.get()`.
Reads are threaded through a `Track` (pure, no DOM) or a `Cx` (reads plus the DOM), which also
makes the purity of memos a type-level fact rather than a rule in a doc comment — a memo runs
lazily at an unpredictable point inside somebody else's read, and a DOM write from there would
land at a time no caller could reason about.

**A consequence worth stating:** a read must not hold the arena borrow while the caller's
closure runs, or a nested read — filtering a list of todos by a flag each one owns — panics
inside `RefCell`. The value is moved out for the duration of the read and moved back, and a
write that lands during the read wins. An API whose reads cannot nest is not one a foreign
caller can drive, and M16's caller will nest them without asking.

**Amended (M8), because the safety half of this was not true as written.** The claim above is
that an ambient runtime turns two windows into a silent cross-wiring "rather than a type
error". Building the second window showed that this design had the same hole: a `Signal` was a
bare index, index 0 exists in every runtime, and handing window A's signal to window B's
runtime read *B's* value at that index and returned it without complaint. A test that disables
the fix reports `left: Some(99), right: None` — one window's state appearing in another's,
which is precisely the failure an ambient runtime was rejected for.

Handles now carry the id of the runtime that issued them, checked on every read, write and
disposal. Four bytes on a `Copy` handle, and it is a refusal at runtime rather than the type
error the original text implied — encoding runtime identity in the type would infect every
signature between here and M16's host functions with a parameter nobody could name. The
decision stands; the argument for it was doing less work than it claimed.

## D-44 — Components run once; effects update, not re-renders

**Status:** Accepted (M7) · **Affects:** M7, M17

A component builds its nodes, registers effects that bind specific text and attributes to
specific signals, and returns. Nothing ever re-runs it. Changing state wakes an effect, which
writes one text node.

**Rejected: a virtual DOM.** Re-running a component to produce a description and diffing it
against the last one is the better-known design, and it is what react-dom will do on top of
this at M17 anyway. Doing it here too would mean the cost is paid twice, and it makes "no
full-tree rebuilds" an optimisation to be maintained rather than the only thing the design can
express. Editing one todo's label in a thousand-item list runs **one** effect and writes
**one** text node, and there is no diff that could have been skipped.

Lists are the case that genuinely needs reconciliation, and `Keyed` does it by key: surviving
items keep their nodes and their effects. The positions that stay put are a longest increasing
subsequence of the previous order, so moving one row from the end to the front costs one move
rather than a thousand.

## D-45 — `STYLE` no longer implies `LAYOUT`

**Status:** Accepted (M7) · **Affects:** M6, M7 · **Supersedes part of** D-18

`DirtyFlags::expanded` used to expand `STYLE` into `LAYOUT` into `PAINT`, on the reasoning
that a style change can change the box and a box change changes the pixels. The second half is
sound. The first is not knowable at the point it was being asserted: `mark_dirty` is told a
style *may* have changed.

Inserting one row into a thousand-row list marks every sibling `STYLE`, because `:nth-child`
could have moved (D-38) — and so relaid out all thousand. Measured: appending one `<li>` to a
1,000-item list invalidated **1,008** layout caches. None of those boxes changed.

The implication now happens where the answer is known. `restyle_incremental` compares each
node's recomputed style against the previous pass and marks `LAYOUT` only where they differ,
which interning (D-21) makes a pointer comparison. The same append now invalidates **8**.

**The remaining direction, closed (issue #20).** Any style change used to mark `LAYOUT`,
including one that only altered a colour, so toggling `.done` on a todo row relaid the row out
to change its text colour. `ComputedStyle::layout_eq` now answers whether two styles would lay
out identically, and the pass marks `PAINT` alone when they would. The todo benchmark's
`toggle` step went from **8** layout invalidations to **4** — and the 4 that remain are not the
row, they are the footer count being rewritten in the same step, which is a text change and a
genuine relayout.

Two things about how it is written, both deliberate:

- **The comparison is asked only where the pointers already differ.** The worry when this was
  filed was that a field comparison would cost more than the pointer comparison it replaced.
  It does not replace it: the pointer comparison still decides *whether* anything changed, and
  only then is the field comparison asked *what kind* of change it was. So its cost is bounded
  by the number of nodes that genuinely restyled — normally small — and what it saves on each
  is a relayout. The precomputed per-group hash the issue suggested is not needed.
- **`layout_eq` copies the paint-only fields across and compares whole structs**, rather than
  listing the layout-affecting fields to compare. The two spellings agree today and fail in
  opposite directions later: under this one, a field added to `ComputedStyle` and forgotten is
  treated as layout-affecting, costing a relayout nobody needed. Under the other, the same
  omission skips a relayout that *was* needed and leaves a box at a stale size. A performance
  bug is recoverable; a wrong picture is not.

**How it was found:** not by reading the flag code, which looks obviously right, but because
M7's acceptance counts nodes laid out and the number came back three orders of magnitude too
large. A milestone that only checked the rendered result would have shipped it.

## D-46 — The memory claim is measured as physical footprint, not RSS

**Status:** Accepted (M8) · **Affects:** M8, §7

ROADMAP §M8 says *"< 60MB RSS idle"* and §7 says *"idle RSS … below a WebView2/WKWebView
baseline"*. Measured literally on macOS, RSS is the wrong number and flatters nobody
consistently: it counts shared read-only library pages, which every process on the machine
pays for and which are not the app's cost.

The same crisol example reports **90 MB RSS and 25.5 MB of physical footprint**. The gap is
Metal, CoreGraphics and the rest, resident once and shared by everything running. Reporting 90
would have declared the product claim dead on a number that says nothing about the product.

So the recorded figure is `phys_footprint` — what Activity Monitor calls "Memory" — with RSS
noted alongside. The Linux equivalent is PSS rather than RSS, for the same reason; the Windows
equivalent is private working set.

**This is not moving the goalposts, and it has to be applied to both sides.** The WebView
baseline is measured the same way, and the comparison is what §7 actually asks about. A
measurement that made only our side look good would be worse than none.

**The trap it took a wrong answer to find:** WKWebView is multi-process, and its helpers are
not its children — they are launched through XPC. Filtering by process name collects every
browser on the machine, which produced a first "baseline" of 1,037 MB, most of it Safari's.
Snapshot the WebKit processes before launching and diff.

## D-47 — The response view has to be virtualised for the acceptance to hold

**Status:** Accepted (M8) · **Affects:** M8

`Node` is **328 bytes** (`ui/tree/tests/sizes.rs` keeps that honest). A 5MB JSON response
expanded one node per token is on the order of 100,000 nodes: **31 MB of arena alone**, before
styles, taffy's per-node cache, or shaped text. That is half the budget spent on structure the
user can see forty rows of.

Virtualised, the same response is a window of roughly a thousand nodes — 328 KB — and the
budget goes almost entirely to the parsed JSON, which is the application's data rather than
the engine's.

**Measured afterwards, and the estimate above was low by 24×.**
`ui/umbrella/examples/bigresponse.rs` builds it: 100,002 nodes is **762 MiB**, not 31 MB. The
arithmetic was right and the accounting was not — "before styles, taffy's per-node cache, or
shaped text" waves at 95% of the cost. Per line of a 240,884-line response: dom **1,017**
bytes, style **15**, layout **14,818**.

That sharpens the conclusion rather than changing it, and it moves the target. Layout is 93%
of the cost and `Node` is 6%, so the section below — that this is not a reason to shrink
`Node` — is righter than it knew: shrinking `Node` to nothing at all would leave 94% of the
bill. What a virtualised pane must avoid building is **laid-out and shaped text**.

The window was measured too, and it holds: 103 nodes, **2.4 MiB**, first frame 15 ms and
**1.9 ms** a frame to scroll (D-50), against 3,678 MiB projected for the whole response. Most of that 2.2 MiB is the font system rather
than the nodes, so it stays flat as the window grows. The scroll extent was checked rather
than assumed — 4,335,212 px against the 4,335,212 px it should be — because a pane that is
cheap by scrolling to the wrong place would otherwise pass as a good memory number.

So the acceptance is a statement about the *application*, not only the engine, and the engine's
job is to make a virtualised list cheap: M6's incremental relayout and M7's keyed reconciler
already do, and M8's scrolling is what drives it.

**What this is not.** It is not a reason to shrink `Node` in a hurry. Two reductions are
available — packing `Color` to 8-bit sRGB, which `BoxStyle` spends 96 of its 132 bytes on, and
interning `BoxStyle` behind an `Arc` the way `ComputedStyle` already is (D-21) — and both are
worth doing on their own merits. Neither is what decides the number.

## D-48 — A second window shares the device; it does not share the runtime

**Status:** Accepted (M8) · **Affects:** M8

Opening a second window duplicates exactly what belongs to a window and nothing that belongs
to the application. The split is not a matter of taste: it is what decides whether window count
multiplies the memory floor.

**Per application, created once.** The adapter, the logical device and the queue, which D-47's
table puts at about 17 MB — most of what an idle GPU application costs against the 60 MB the
product claim allows. (That figure is inherited from a measurement taken before the sampling
problem below was understood, so treat it as an order of magnitude rather than a reading.)
The font database, which is a filesystem scan. The style engine, whose
interner means two windows computing the same style share one `Arc` (D-21). And a renderer per
*surface format* rather than per window, so two windows on one display share a glyph atlas and
one set of pipelines.

**Per window.** The surface, the tree, the layout cache, and the reactive runtime with its
signals. `WindowSurface::with_gpu` is the seam: the first window creates the device, every one
after it borrows the one already chosen — which also settles presentability, since an adapter
picked for one window on a multi-GPU laptop is the one attached to that display.

**Verified by identity, not by footprint.** The example asserts `SharedGpu::ptr_eq` between
the first window's device and every later one — the same allocation, three holders after two
windows. That check is deterministic. Memory sampling here is not: footprints taken while the
app idles under `ControlFlow::Wait` moved several MB between runs of the *same* binary, and a
one-window build at four times the window area measured *lower* than at one times, which cannot
be true. A measurement that cannot order two configurations known to differ is not evidence
that a device is shared; a pointer comparison is. The per-window cost is therefore left
unquantified until there is a deterministic way to sample it.

**What this cost.** Two runtimes is what D-43 said this milestone would need, and it was right
that the runtimes must be separate — but see the amendment there, because the second window is
also what exposed that separate runtimes were not yet *safely* separate.

## D-49 — The memory number is read from inside the process

**Status:** Accepted (M8) · **Affects:** M8, §7

§7's first kill criterion is a memory figure, so the figure has to be one that can be
re-derived rather than one that was once observed. `crisol_ui::measure` reads this process's
own memory; the `measure` feature is off by default, because an embedder has no use for it,
and ships anyway because a claim nobody can re-run is not evidence.

**Rejected — sampling from outside.** `footprint(1)` and `vmmap` are the obvious instruments
and are what produced D-47's table. They share a defect that only showed up when two readings
had to be compared: the sampler picks the moment, and a GUI process under `ControlFlow::Wait`
differs by megabytes depending on whether it has drawn a frame. Measured that way, a one-window
build at four times the window area reported **less** memory than the same build at one times.
Instrumenting the process showed why — it had never drawn at all, and neither had the runs it
was being compared against. An instrument that cannot order two configurations known to differ
is not measuring the configuration.

**The metric is per-platform and is returned with the number.** macOS `phys_footprint`, Linux
`VmRSS`, Windows `PrivateUsage`. These are not the same quantity — the first counts compressed
pages, the last counts commit charge whether resident or not — and a table that puts them in
one column without saying so is comparing unlike things. `Metric` makes that impossible to do
by accident.

**On trusting an unsafe binding.** The Mach struct is taken from `mach2` rather than declared
here: `task_vm_info` is `repr(C, packed(4))`, and a hand-rolled `repr(C)` copy differs in
alignment without differing in any way a compiler would mention. The offset of `phys_footprint`
is computed and asserted rather than written down — the assertion caught it at 144 where the
arithmetic in the comment above it said 152, which as a hardcoded constant would have reported
`compressed_lifetime` as a memory footprint. A wrong memory number is worse than no memory
number, because it still gets quoted.

**A tolerance can be wider than the thing it bounds.** The cross-check against `vmmap` passed
while deliberately reading the wrong field, because its tolerance had a 4 MiB floor and the
test process weighs 1.7 MiB. Any tolerance expressed as an absolute floor needs checking
against the smallest case it will ever see, not the largest.

## D-50 — An inline style is an origin, and it is stored as text

**Status:** Accepted (M8) · **Affects:** M8, M16

A virtualised response view (D-47) moves two spacers every scroll frame. Without somewhere to
put a per-element length, the only way to do that was to reparse a stylesheet per frame, which
made the one design that fits the memory budget unbuildable. So the CSSOM `style` gap stopped
being one of three missing DOM pieces and became the thing in the way.

**It is its own cascade origin, not a very specific selector.** `Origin::Inline` sorts above
`Origin::Author`, so an inline declaration beats `#id` without carrying a specificity that
could be out-argued by a longer selector. `important` still sorts above origin, so
`!important` in a stylesheet beats a normal inline declaration and loses to an important one —
which is the CSS rule, and falls out of the existing `Precedence` ordering rather than needing
a special case.

**Stored as text on `ElementData`, parsed by the cascade.** The parsed form would be the
obvious choice and it is not available: `Property` is lightningcss's, `crisol-css` depends on
`crisol-tree`, and storing parsed declarations on an element would either invert that or pull
a CSS grammar into the tree crate. `Option<Box<str>>` costs a pointer on elements that have no
inline style — `Node` went from 328 to 344 bytes, against the 400-byte budget
`ui/tree/tests/sizes.rs` holds — and only elements that do have one pay to parse it, only when
they are restyled.

Parsing per restyle sounds like the cost this was meant to avoid, and the measurement says it
is not: scrolling a 240,884-line response costs **1.9 ms a frame**, 12% of a 60 Hz budget, in
a debug build. Two declarations is not a stylesheet. If it ever does show up, the cache goes
beside the string rather than replacing it.

**`attribute()` has to answer for it.** `style` lives in its own field like `id` and `class`,
and all three are invisible to a caller that looks in the attribute list. Forgetting the
accessor would not just break `getAttribute("style")`: `set_attribute`'s no-op check reads
back through it, so every write would look like a change and mark the node dirty forever.
`rewriting_a_style_to_the_same_text_costs_nothing` is the test that fails if it goes missing.

## D-51 — winit 0.31, pre-release, taken now

**Status:** Accepted (M8) · **Affects:** M8, M22

M8 lists drag and drop. winit 0.30 cannot do it: `DroppedFile` carries a path and no position,
every backend has the coordinate and discards it, and macOS never implements `draggingUpdated:`
so there is no drag-over stream to highlight a target with. The engine dispatches events to
nodes by hit-testing a point, so a drop with no point has no target.

0.31.0-beta.3 replaces file drops with a real subsystem — `DragPosition` carrying a
non-optional position, typed data fetched asynchronously by id, and outgoing drags so an app
can be a source as well as a target. The backends implement it rather than declaring it:
`winit-appkit` reads `draggingLocation` and implements `draggingUpdated:`, `winit-win32`
carries a full `IDropTarget`.

**Taken as a pre-release, deliberately.** The alternative was a fork, which would have been
work thrown away the moment 0.31 shipped, or waiting, which blocks an M8 deliverable on a
schedule nobody here controls. crisol is unreleased and has no downstream to break, so the
usual argument against a beta dependency — *you inflict it on your consumers* — does not
apply yet. It has to stop applying before 1.0, which is the condition on this decision rather
than a footnote to it.

**What the upgrade cost**, so the next beta bump is estimated from evidence rather than hope:
35 errors across four examples and one library, all mechanical. `Window` became a trait, so
`Arc<Window>` is `Arc<dyn Window>` and `create_window` hands back a `Box<dyn Window>`.
`inner_size` became `surface_size`, `Resized` became `SurfaceResized`, `CursorMoved` became
`PointerMoved`, `resumed` became `can_create_surfaces`, `run_app` takes the handler by value,
and `set_cursor` takes a `Cursor` rather than a `CursorIcon`.

**Two of those had a wrong answer that compiles.** `inner_size`'s rename is suggested by the
compiler as `outer_size`, which is a different measurement — it includes decorations, and
taking the hint would have sized every surface wrong with no error anywhere. And
`MouseScrollDelta` is now `#[non_exhaustive]`: the arm added for it returns rather than
scrolling zero, because a delta this build cannot read is a scroll of unknown size and
inventing a distance for it is worse than ignoring it.

`NamedKey::Space` is gone, which is a correction rather than a loss — `keyboard-types` follows
the spec, where space is a character. The two arms that special-cased it were deleted outright
because the `Key::Character` arms beside them already did the same work.

---

## D-52 — The bundle layout is built anywhere; only the container is host-gated

**Status:** Accepted (M8) · **Affects:** M8, M21, M22

M8 lists packaging as `.app`, `.msi` and `AppImage`. The obvious shape is three host-only
code paths, each compiled and tested on its own runner. That shape is how a packager ends up
broken on two platforms out of three: the macOS path is the one the author runs, and the other
two are exercised only when someone tries to ship.

So the split is not by host but by **what actually needs one**. A `.app` is a directory with a
plist in it. An AppDir is a directory with a shell script in it. A `.wxs` is XML. None of those
need the platform they target, and all three are therefore built, and tested, on every runner.
What genuinely needs a host is the container step — `appimagetool` wants FUSE and a Linux
kernel, WiX wants Windows — and that step runs when the tool is on `PATH` and reports itself
missing when it is not, with the layout left in place because the layout *is* that tool's
documented input.

The result is that the 13 tests covering this run identically on all three CI platforms, and
the part that can only run in one place is the part that is a subprocess call.

**`crisol package` takes a binary, not a project.** `crisol build` is M13, so until it exists
there is nothing to compile from. The input is a Rust application built against `crisol-ui`,
which is exactly what §M8's acceptance describes — "built entirely in Rust". When `build`
lands, its output is handed to the same command; nothing here knows which of the two made the
executable, which is why it takes a path.

**The `UpgradeCode` is never invented.** An MSI's `UpgradeCode` must be byte-identical across
every version an application ever ships, or Windows installs the new version *beside* the old
one instead of replacing it. A generated one would work perfectly on the first release and
fail on the second, which is the worst possible time to find out. It is required, and refusing
is a one-line error rather than a silent future defect.

**An MSI version is checked against what the installer can hold**, not against a style rule.
Windows Installer packs `ProductVersion` into 32 bits: major and minor are bytes, build is
16 bits, and a fourth field is ignored entirely when comparing. So `1.2.3.4` and `1.2.3.5` are
the same release as far as upgrades are concerned, and `1.2.65536` wraps. All of these build,
install, and then fail to upgrade. Rejecting them at package time is the only place it is
cheap.

**Icons are copied, not converted.** The three platforms want `.icns`, `.png` and `.ico`, and
they disagree about sizes and colour profiles in ways a generic conversion gets wrong. A
packager that re-encodes images is a packager that owns an image pipeline; this one takes the
format the target asked for and says so.

**Signing and notarisation are not here.** Both need credentials and Apple's own tooling, and
an unsigned `.app` is still the correct input to `codesign`. Shipping to other people is M21.

**Sealing is a switch, not a consequence of what is installed.** The first version ran
`appimagetool` or WiX whenever it found one and staged when it did not, which reads as
helpful and is actually a test that cannot hold still: it passed here, where WiX is absent,
and failed on CI, where the GitHub Windows image ships it. *Whether a tool is installed* is
not something a test may branch on — a suite that does is testing the runner. `--stage-only`
makes the choice explicit, every test takes it, and the sealing path is a subprocess call
that is deliberately outside the tested surface. It is also a mode worth having on its own:
a pipeline that runs those tools itself wants their input without this one guessing at their
arguments.

**Rejected: cargo-bundle or tauri-bundler.** Both would have worked and both are more complete
than this. They are also a dependency on someone else's opinion about what a crisol application
is, at the exact milestone where that question is being answered, and `tauri-bundler` in
particular carries a WebView-shaped worldview that is the thing this engine exists to avoid.
Revisit at M21 when there is a real release process to serve.

---

## D-53 — Numbers are unboxed and everything else hides in the NaNs

**Status:** Accepted (M9) · **Affects:** M9, M11, M13, M16

ROADMAP §M9 says NaN-boxing on 64-bit and this records which way round it was done, because
the arrangement is not the obvious one and it is hard to change later: `Value` is the calling
convention as much as it is a type, so the IR (M11) and codegen (M13) are both built on this
byte layout.

**Numbers are stored as themselves; tagged values live in the space doubles do not use.** A
double has 2^52 NaN bit patterns and JavaScript can observe exactly one of them, so all but one
are free. The alternative — tag the pointers and box the doubles — costs an allocation and an
indirection on every arithmetic result. JavaScript's only number type *is* the double, so that
is the hot path by definition, and making it the slow one in order to keep pointer handling
tidy is the wrong way round.

**The tag reserves one mantissa bit beyond the quiet bit.** `TAG_BASE` is `0x7FFC…`, not
`0x7FF8…`. That extra bit is what leaves the canonical quiet NaN — the pattern every FPU
produces — on the *number* side of the line, so arithmetic that overflows into NaN needs no
handling at the point it happens. Reserving only the quiet bit would have made every NaN the
hardware produces look like a tagged value with payload zero.

**NaN is canonicalised on the way in, and this is the load-bearing safety property.** A NaN
carrying an arbitrary payload — which bit manipulation or a foreign producer can hand over,
even though arithmetic will not — can have the tag bits set. Without the rewrite it comes back
as an `Object` whose address is the mantissa, and the first thing that dereferences it crashes
a long way from the cause. That is exactly §3.1's "use-after-free bugs that appear only under
memory pressure", which is the failure mode M9 exists to design out. The rewrite costs one
predictable branch, and JavaScript cannot tell two NaNs apart, so nothing is lost.

Checked by removing it: `a_nan_with_the_tag_bits_set_is_still_a_number` fails, and the hostile
NaN is read back as an object.

**48 bits of address, refused rather than truncated.** Every platform this engine targets gives
user space a 48-bit virtual address, so a heap pointer fits exactly. `Address::new` returns
`None` above that instead of masking, because a truncated pointer is *wrong* rather than
obviously invalid and the crash lands somewhere else entirely.

**`kind()` is total over all 2^64 patterns.** `from_bits` is reachable from generated code, so
a pattern no safe constructor produces must not be able to abort the program. An unrecognised
singleton payload reads as `undefined` rather than panicking.

**A consequence worth knowing before someone reaches for `==`:** derived equality on `Value` is
JavaScript's `Object.is`, not `===`. `Object.is(NaN, NaN)` is true and bitwise equality agrees
*because* of the canonicalisation above; `Object.is(0, -0)` is false and bitwise equality agrees
because the sign bit differs. `===` disagrees with both. There is a test named after this so it
is discovered by reading rather than by debugging.

**Not decided here:** small-integer unboxing. Storing int32s in their own tag saves the
double↔int conversion in loops, and it also adds a second numeric representation that every
arithmetic site has to handle. Worth measuring against real code at M20 rather than assuming
now; nothing in this layout precludes it, since two tag slots are still free.

---

## D-54 — Shapes are a tree of remembered transitions, and lookup walks it

**Status:** Accepted (M9) · **Affects:** M9, M11, M13, §3.2, §3.4

An object carries a `ShapeId` and a flat slot array, never its own property names. Objects
built the same way share a shape, so the names are stored once for all of them — the same
argument as the style interner (D-21) applied to a different kind of repetition.

**Adding a property transitions to another shape, and the transition is remembered.** So
`{}` → `.x` → `.y` is walked once and reused forever: the second `{x: 1, y: 2}` a program
evaluates compares no names at all. Checked by removing the reuse — three tests fail, including
one that builds the same object a hundred times and asserts the table did not grow.

**Order is part of a shape's identity**, so `{x, y}` and `{y, x}` are different shapes. Not an
implementation artefact: JavaScript specifies insertion order for string keys and `Object.keys`
has to produce it.

**Assigning to a property the shape already has is not a transition.** Without that,
`for (…) obj.x = i` grows the tree once per iteration — a memory leak shaped like a hidden
class. It has its own test because it is the kind of thing that looks correct and is not.

**Lookup walks the chain to the root: O(properties).** The alternative is a flat map per shape,
which is O(1) to read and O(n²) in memory across a transition chain, for objects that are
mostly small. ROADMAP §3.4 already says where the speed comes from instead — "shape-based
lookup with a per-site monomorphic cache" — so the walk happens once per *call site* rather
than once per access, and §3.4 is equally explicit that "the generic path being the common
path in v1" is the expected budget. The cache belongs to the IR (M11). What belongs here is a
lookup whose answer is stable enough to cache, which is why `ShapeId` is dense and `Copy`.

A property table for wide shapes is the known next step if measurement asks for it. It is not
done speculatively.

**Two roots, and exoticness propagates.** §3.2's resolution is that shapes carry an `is_exotic`
bit and the fast path branches on it once, so programs that never construct a `Proxy` pay one
predictable branch rather than a check per access. Every transition inherits the bit — checked
by breaking it, because a `Proxy` that quietly became an ordinary object after one property
assignment would let the fast path specialise something it must not, and that is a wrong answer
rather than a slow one.

**`PropertyKey` is deliberately not `crisol_tree::Atom`.** The tracks do not converge until M16
(§4), and a runtime crate reaching into the UI tree for a string type would couple them years
early. The requirements differ as well: `Atom::lowercase` exists because HTML names are
case-insensitive, and applying it to a JavaScript property name would be a bug — `obj.X` and
`obj.x` are different properties. There is a test named after that.

---

## D-55 — Objects live in a slab behind checked handles, not behind raw pointers

**Status:** Accepted (M9) · **Affects:** M9, M13, §3.1

ROADMAP §3.1 names the GC/FFI boundary as M9's risk: Rust code holding a JS value must not
hide it from the collector, and getting it wrong gives use-after-free bugs that appear only
under memory pressure — "the worst possible failure mode to debug". Two decisions follow from
taking that seriously rather than agreeing with it.

**A `GcRef` is a slot index and a generation, not an address.** Reading through a handle whose
object has been collected fails its liveness check and returns `None`, even when the slot has
since been reused. That is `crisol-tree`'s `NodeId` argument (D-17) applied to a collector,
where it is worth more: a stale node handle is a bug, a stale object handle is the exact
failure §3.1 describes. Checked by freeing an object, allocating into its slot, and asserting
the old handle reads nothing.

Thirty-two bits of slot and sixteen of generation, because 48 is what a `Value` carries (D-53).
A handle that did not fit would have to be boxed and every object reference in the language
would cost an indirection. When the generation cannot advance the slot is **retired rather than
reused** — that leaks one slot per 65,536 reuses, against an ABA bug that reads one object
through another's handle.

**Rooting is a scope guard, because §M9 says to make it hard to misuse and prefers one to
manual push/pop.** There is no way to obtain a `Rooted` without a `Scope`, and a `Rooted`
borrows its scope, so the mistake is a compile error rather than something the collector
discovers later. Dropping is the only way to unroot; there is no `pop` to forget.

`alloc` and `collect` therefore take `&self` and the heap uses interior mutability. They have
to: a guard that restored the root stack on drop while allocation held `&mut self` would make
two live scopes impossible, and nested scopes are the ordinary shape of a call stack.

**Marking is precise.** An object's outgoing references are exactly the slot values that carry
an address, so nothing is retained because an integer happened to look like a pointer. There
is a test that puts a handle's bits into a slot *as a number* and asserts the target is still
collected.

### What this costs, and when it comes due

**The acceptance's ASAN clause is vacuous under this design rather than satisfied by it.**
There is no `unsafe` in `crisol-gc` or `crisol-value`, so a use-after-free in the sense ASAN
detects is not expressible. That is stronger than the acceptance asks for and it is worth
saying plainly, because "we ran ASAN and it was clean" would imply a check that did not
happen.

It is also not free and not permanent. It holds *because* objects sit in a slab behind checked
handles. At M13 compiled code will want to dereference objects directly — that is most of the
point of compiling — and at that moment the bounds and generation checks stop being free, the
representation has to grow a raw-pointer path, and ASAN starts having something to look at.
The right time to re-open this is when there is generated code to measure, not now.

**Also not decided:** generational or incremental collection. Mark-sweep stops the world and
walks everything live, which is fine for a heap that has not been measured yet and is the
first thing to revisit if pause times matter. §M9 asks for mark-sweep and that is what this is.

---

## D-56 — A cycle in the module graph is ordered, not rejected

**Status:** Accepted (M10); its MSRV clause **superseded by D-85** · **Affects:** M10, M13, M17

The easy mistake is to treat a module graph the way a build system treats a dependency graph,
where a cycle is an error to report and refuse. In ES modules a cycle is **specified
behaviour**: the modules are instantiated together, evaluated in depth-first post-order, and a
binding read before its module has evaluated is a `ReferenceError` from the temporal dead zone
rather than a link failure. `react` and `react-dom` have shipped cycles for years, so a graph
that refused one would refuse to build most real programs — and §3.3 is explicit that rejecting
constructs in code the developer did not write is the failure mode to avoid.

So `evaluation_order` cannot fail. `cycles()` reports which modules are in one, because a
consumer may want to warn or may want to explain a temporal-dead-zone error by pointing at one,
but nothing refuses to proceed. Reporting is deliberately *not* "every strongly connected
component": a component of one is a cycle only if the module imports itself, and without that
filter every acyclic module is reported and the report is useless. There is a test for exactly
that, and it fails when the filter is removed.

**The graph does not know about the parser.** §M10 wants `oxc` for parsing and `oxc_resolver`
for resolution, and both will feed this — but what a graph *is*, and what a cycle in one means,
is decided by the specification rather than by whichever crate read the source. Split, the
ordering rules are tested against hand-built graphs where a cycle takes three lines, instead of
against a `node_modules` tree where reproducing one is an afternoon.

**Both walks are iterative.** The input is somebody's dependency tree and its depth is not this
code's to bound; a recursive post-order walk turns a deep graph into a stack overflow, arriving
from a user's `node_modules` rather than from anything here. A 100,000-deep chain is in the
tests for that reason.

**Import records are kept, not merged.** `import {a} from "./m"; import {b} from "./m"` is two
import records of one module. Evaluation visits the module once regardless, but source order
decides evaluation order, so a graph that sorted or deduplicated edges would produce an order
the specification does not.

---

## D-57 — `require` is found by an exhaustive visit, not by matching the shapes we expected

**Status:** Accepted (M10); its MSRV clause **superseded by D-85** · **Affects:** M10, M12, §3.5

§M10's acceptance is "resolves and parses a real `node_modules` tree containing React,
producing a complete module graph with no unresolved imports". React 19 is CommonJS from top to
bottom — its entry is `module.exports = require('./cjs/react.production.js')` — so every edge in
that graph comes from a `require` call rather than from `import` syntax.

**A loader that understood only ESM would pass this acceptance by doing nothing.** It would walk
React, find no edges at all, and report a complete graph with no unresolved imports. That is the
trap in the acceptance's wording: *a missing edge makes "no unresolved imports" easier to
satisfy, not harder*. Any incompleteness in finding requests is therefore invisible to the very
test that is supposed to catch it.

So `require()` is found with `oxc_ast_visit`'s visitor, which walks every node, rather than by a
hand-rolled walk over the statement and expression shapes CJS "usually" takes. React's entry
puts its requires inside an `if`; its bundles put them inside functions; `require(require(x))` is
legal. A walker covering the cases someone thought of would miss edges silently, and silence is
exactly what this acceptance cannot detect.

ESM requests come from the parser's `ModuleRecord` — the specification's `[[RequestedModules]]`
— which is exactly right for `import` and `export … from` and correctly does *not* contain
`require`. The two sets are merged by source position, because `requested_modules` is a map and
its iteration order is not source order, and source order is what decides evaluation order.

**The tree is installed, not vendored.** What makes this an acceptance is `exports` maps,
conditions, CJS entry points and a dependency living in another package, laid out the way npm
lays them out. A committed fixture would satisfy the sentence and test nothing. CI installs a
**pinned** React so that a React release cannot turn a green branch red without a commit, and
`CRISOL_REQUIRE_NODE_MODULES` makes an absent tree a failure rather than a skip — the same
arrangement as `CRISOL_REQUIRE_GPU`, for the reason the workflow already gives.

**Both `NODE_ENV` branches stay in the graph.** `if (process.env.NODE_ENV === 'production')
require(A) else require(B)` contributes two edges. That is correct for a graph, which records
what *could* be imported; §3.5's concern — that the dev build's invariant machinery must not
reach a release binary — is an optimisation pass's job at M12, and it needs both branches to be
present in order to remove one. The test asserts both are there, so the day one disappears is a
failure rather than a smaller number nobody looked at.

### A note on how this decision was nearly not made

Both of this milestone's supposed blockers were asserted without measurement and both were
wrong. `oxc` was called too large for this machine's disk; it is 292 MB of `target` and builds
in fifteen seconds. The acceptance was called unreachable because `npm` is aliased to a broken
`pmg` wrapper; a real React tree was already on the machine, and the npm behind the alias runs
fine when invoked directly. Neither claim survived thirty seconds of checking, and both were
written into `STATE.md` as facts first.
## D-58 — Terminators are a field, safepoints are mandatory, and the lattice is shallow

**Status:** Accepted (M11) · **Affects:** M11, M12, M13, §3.1

Three decisions in the IR, each of which is about what the *next* milestone will be able to
rely on.

**Terminators are a separate type from operations, so a block holds a `Vec<Op>` and exactly one
`Terminator`.** "Every block ends with one terminator, and none appears in the middle" is then
not a rule the verifier enforces — it is a shape that cannot be written down. §M11's acceptance
asks for a verifier that "rejects malformed graphs", and the best way to reject a class of
malformed graph is to make it unrepresentable, leaving the verifier for what a type cannot say.

**Safepoints are mandatory on anything that can collect, and the verifier rejects both
directions.** §M11 is unusually firm — "the IR must represent safepoints explicitly or the GC
integration in M13 will not work" — so a missing one is refused rather than inferred later. The
*other* direction is refused too: a safepoint on a `Const` means whoever built the graph did not
know which operations collect, and the ones they missed are the dangerous half. Both were
checked by removing the check and watching the tests fail.

`PropertyLoad` counts as able to collect. A getter is a call, and on an exotic shape (D-54) the
lookup itself runs user code. Treating it as safe would be right for the common case and wrong
for the one that matters, which is the wrong way round when the failure mode is §3.1's
use-after-free under memory pressure.

**SSA uses block parameters rather than phi nodes.** They are equivalent, Cranelift takes block
parameters (§4 names it as the backend), and they make the verifier's job concrete: an edge
passes arguments, so argument count and type can be checked per edge instead of a phi's operands
being checked against an implicit predecessor order.

**The type lattice is deliberately shallow.** `Never ⊑ {Undefined, Null, Bool, Number, String,
Object(shape?)} ⊑ Unknown`, and that is all. A richer lattice infers more and gives the analysis
more places to be subtly wrong in a way that produces *faster incorrect code*. This one answers
the question codegen actually asks — "can I skip the check?" — and says `Unknown` whenever it
cannot be sure, which is safe and merely slow.

`Object` carries an optional shape because that is where specialisation comes from: a property
access on `Object(Some(s))` resolves to a slot at compile time. Joining two different shapes
forgets both, because the alternative is picking one, which is how a field gets read from the
wrong offset.

The laws are tested as laws — join commutative, associative, idempotent, an upper bound of both
operands, and agreeing with the subtype relation — over every pair and triple of types. A
lattice that is only *mostly* a lattice yields an analysis whose answer depends on the order
passes ran in, and that surfaces as a miscompilation weeks later rather than as a failing test.

---

## D-59 — Locals lower to slots; SSA construction is a separate pass

**Status:** Accepted (M11) · **Affects:** M11, M12

A `let` becomes a numbered slot and reading it is a `Load`, so a control-flow merge needs **no
block parameters at all**: both arms of an `if` wrote the same slot and the code after reads it.
Promoting slots to SSA values — the `mem2reg` every compiler has — is a separate pass and
belongs with the other optimisations (M12).

The alternative is constructing SSA during lowering, which means implementing Braun-style
incremental φ insertion *while also* getting the AST walk right, and then debugging the two
together when a value comes out wrong. Split, each half is checkable alone: this one emits IR
the verifier accepts, and the promotion pass is a graph-to-graph transformation with an obvious
before and after.

The IR's block parameters are therefore unused by lowering today. They are not speculative —
they are what the promotion pass will write, and having them in the IR first is why that pass
can be written without changing the IR underneath it.

**An unsupported construct is recorded, never guessed.** §3.3 says rejecting a construct in code
the developer did not write is the failure mode to avoid — but a *compiler* that silently emits
`undefined` for syntax it did not understand is worse than one that refuses, because the result
is a program that runs and is wrong. So lowering always produces a function, everything it did
not understand lands in `Lowered::unsupported`, and `Lowered::is_faithful` exists because
checking a `Vec` is empty is easy to forget and a method named after the question is not.

`unfaithful_programs_are_reported_not_guessed` takes twelve constructs the lowering does not
handle and asserts each is *named*. The corpus is what works; that test is what does not, and
the two are checked against each other rather than against a claim in a comment.

**An object literal's value is typed `Object(None)`, not `Object(Some(root))`, and the
difference is soundness rather than precision.** A type in SSA is fixed for the value's whole
life, but an object's shape changes as properties are added — so typing the result
`object#root` after two `PropertyStore`s claims the object is still empty, and a pass trusting
that would resolve `.a` to no slot at all. `Object(None)` says the one thing that stays true.

Recovering the precise shape needs either shape transitions modelled in the IR or types attached
to program points rather than to values. Both are M12's, and both beat guessing now. This was
caught by reading the first generated snapshot rather than by a test, which is the argument for
the snapshot being reviewed rather than merely regenerated.

**The corpus is representative of what lowers**, not of JavaScript. It has no arithmetic, no
functions and no `for` loops, because those do not lower yet. Saying "thirty representative
programs" without saying that would be the more flattering sentence and the less true one.

---

## D-60 — The object model follows the specification's shape, including its asymmetries

**Status:** Accepted (M12) · **Affects:** M12, M13, §3.2

`ValidateAndApplyPropertyDescriptor` (ECMA-262 10.1.6.3) is written out rather than
simplified, because every rule in it that looks redundant is load-bearing:

- A **non-configurable** property can still have its value changed if it is also **writable**.
  Freezing needs both bits, and an implementation that treats `configurable: false` as "frozen"
  rejects legal programs.
- A frozen property may be "changed" to the value it already has, by **SameValue** — so `NaN`
  to `NaN` is allowed and `0` to `-0` is not. `Value`'s derived equality *is* `Object.is`
  (D-53), so this is one comparison rather than a special case, which is the payoff for having
  canonicalised NaN back at M9.
- Writability goes true → false and never back, on a non-configurable property.
- Changing a data property into an accessor, or back, needs `configurable` — in both
  directions.
- A descriptor asking for nothing is allowed on anything, including a frozen property on a
  non-extensible object. Asking for no change is not a change.

Each is a line in the spec and a test. The code's shape follows the specification's on purpose:
when the two disagree, the diff should be obvious rather than requiring someone to re-derive
the rule.

**`[[Set]]` consults the prototype chain before deciding where to write**, which is the rule
most likely to be "simplified" away. `Object.freeze(proto)` stops `child.x = 1` from creating
an own property on the child — surprising, correct, and invisible until someone freezes a
prototype. Checked by making `[[Set]]` local and watching the test fail.

**`[[OwnPropertyKeys]]` puts array indices first, ascending, then strings in insertion order.**
`Object.keys({b: 1, 2: 2, a: 3, 1: 4})` is `["1", "2", "b", "a"]`, and code that renders a
keyed list depends on it. Only *canonical* decimals count: `"01"`, `"1.0"` and `"-0"` are
ordinary string keys, and moving them into the numeric group would reorder `Object.keys` in a
way no engine does.

**Getters are returned, not called.** `[[Get]]` on an accessor has to call a function and
nothing in this crate can call one, so it hands back `Got::Getter` and the caller performs the
call. Pretending otherwise would mean inventing a calling convention here, in the crate least
equipped to own one.

### What this is not yet joined to

[`crisol_value::Shapes`] is the fast path — a data property in a slot, resolved at compile time
(D-54). This is the general path, where a property can be an accessor, non-enumerable or
frozen. Real engines keep both and spill from the first to the second when a property stops
being ordinary.

Marrying them is the next piece of work and deliberately not done here: the join decides when
an object leaves the fast path, which is the decision §3.2's "one predictable branch" rests on,
and it deserves its own diff rather than arriving underneath a descriptor implementation.

---

## D-61 — The microtask queue is FIFO, drains to empty, and `then` is never synchronous

**Status:** Accepted (M12) · **Affects:** M12, M15

§M12 singles `Promise` out: *"job queue ordering must match spec or async code misbehaves in
ways that look like race conditions."* Nothing here is concurrent — every job runs to completion
on one thread — so the bugs look like races only because the order is observable and the code
depending on it never says so. Three rules carry that:

**`then` always queues, even on a settled promise.** `Promise.resolve(1).then(f)` does not call
`f` before `then` returns. Code that relied on the synchronous case would work until the promise
happened to be pending, which is exactly the intermittent failure §M12 describes.

**The queue is FIFO and drains to empty, including jobs queued by jobs.** That is what makes two
chains interleave step by step — `a1, b1, a2, b2`, not `a1, a2, b1, b2` — and a queue that ran
one chain to completion first would change the behaviour of every `await`-heavy program. It is
also why an endless `.then` chain starves the event loop rather than yielding: specified, not an
oversight.

**A missing handler passes the settlement through *as it was*.** A rejection arriving at
`.then(onFulfilled)` must continue as a rejection. Forwarding it as a fulfilment means
`p.then(onFulfilled).catch(handler)` never reaches the `catch`, and the program carries on with
an `Error` where it expected data — a wrong answer rather than a crash. This was written wrong
first time and caught by reading it back before the tests existed.

All three were checked by breaking them: a LIFO queue fails the interleaving test, a
fulfil-always pass-through fails the rejection-forwarding test, and a synchronous `then` fails
five.

**Reactions are Rust closures, and the queue really runs them.** Elsewhere in this crate a thing
that would need to call a JavaScript function hands it back instead ([`Got::Getter`]) — but here
*ordering is the entire content*, so returning jobs uncalled would leave nothing to test. When
the interpreter lands, a job becomes "call this function" and none of the ordering above
changes.

**Not modelled yet:** the spec's `NewPromiseResolveThenableJob` adds a tick that this does not,
so adoption costs one extra microtask here where a real engine charges two. The tests assert the
*relative* order that follows from adoption being asynchronous at all, not a tick count — an
assertion of parity would be a claim this implementation has not earned.

---

## D-62 — Three equalities, and `Map` uses the one that is neither of the others

**Status:** Accepted (M12) · **Affects:** M12

| | `NaN` vs `NaN` | `0` vs `-0` |
|---|---|---|
| `===` (strict) | different | same |
| `Object.is` (SameValue) | same | different |
| **`Map`/`Set` (SameValueZero)** | **same** | **same** |

`Value`'s derived equality is `Object.is` (D-53). That is exactly right for property
descriptors, where the spec asks for SameValue (D-60), and **wrong for `Map` keys**. Using it
would give a map with two entries that print identically and neither of which `get(0)` reliably
finds — a bug that survives every casual test, because nobody writes `-0` on purpose. It arrives
from arithmetic.

So map keys go through a wrapper that folds `-0` into `0`. NaN needs no handling because M9
canonicalises it on the way into a `Value`, which is the second time that decision has paid for
itself. Checked by removing the fold: three tests fail.

**Entries live in a `Vec` with tombstones, not only in a hash map.** Insertion order is
observable, and the spec is specific about mutation during iteration: an entry deleted before
the iterator reaches it is *not* visited, and one added during iteration *is*. A `Vec` of
positions gives both; a `HashMap` alone gives neither. It is also why an iterator over a `Map`
whose body keeps adding will not terminate — specified, not an oversight.

Re-setting an existing key keeps its **position and its original key**: a `Map` used as an LRU
by re-setting would not work, and `map.keys()` after `set(-0, …)` on a `0`-keyed map still
reports `0`.

## D-63 — JSON is strict on the way in and exact on the way out

**Status:** Accepted (M12) · **Affects:** M12

JSON looks like a JavaScript literal and is not. Trailing commas, comments, single quotes,
unquoted keys, leading `+`, leading zeros, hexadecimal, `NaN` and `Infinity` are all rejected,
because accepting any of them makes `JSON.parse` succeed on input every other parser refuses —
which turns a clear error at the boundary into corrupt data further in. Eighteen of them have a
test.

**Surrogate pairs are joined.** `😀` is one character, not two. Without joining, both
halves become replacement characters and an emoji turns into two question marks somewhere
downstream, with nothing at the point of failure to say why. Lone surrogates are refused rather
than passed through.

**A raw control character inside a string is an error**, not the character: a literal newline
between the quotes is malformed JSON even though it is obvious what was meant.

On the way out: `NaN` and the infinities become `null`, because JSON cannot write them and
emitting `NaN` would produce output no other parser accepts. `-0` becomes `0`, so **a round trip
loses the sign** — specified, and a real information loss worth knowing rather than discovering.
`/` is deliberately *not* escaped; it is legal either way and escaping it differs from every
other implementation for no benefit.

**Objects keep insertion order**, in a `Vec` rather than a `BTreeMap`. Sorting keys would
quietly rewrite every document that round-tripped through, which is the kind of change that
shows up as a spurious diff in someone else's repository.

---

## D-64 — A hole is not `undefined`, and truncation can fail halfway

**Status:** Accepted (M12) · **Affects:** M12

Everything unusual about an array comes from `length` being tied to the indices that exist:
writing an index at or beyond it raises it, writing a smaller one deletes the elements above.
Two consequences are worth writing down because both are easy to get wrong *by being
reasonable*.

**A hole is not a property holding `undefined`.** `[, 1]` and `[undefined, 1]` both read
`undefined` at index 0, and only the second answers `0 in a` with true. Every iterating method
has to decide which it means and they do not all agree — `forEach` skips holes, `map` preserves
them, `Array.from` fills them. So `has` is a separate question from `get` here, rather than
`get` returning `undefined` and leaving each caller to guess which kind of nothing it found.

**`ArraySetLength` deletes from the top down and stops at the first element it cannot delete**,
leaving `length` one above it and reporting failure. Freezing one element makes `a.length = 0`
shrink the array only as far as that element — a *partial* success.

An implementation that treated truncation as atomic would be wrong in both directions at once:
it would refuse a change the spec allows (when the obstruction is above what was asked for) and
discard elements the spec protects (if it deleted first and checked after). Checked by making it
atomic and watching the test fail.

Smaller ones that follow from the same place: `delete` leaves a hole and does **not** shorten
the array, which is the whole difference between `delete` and `pop`; a non-writable `length`
stops `push` as well as assignment past the end, because growing *is* writing `length`; and
`2^32 - 1` is a valid length but not a valid index, so `a[4294967295] = x` creates an ordinary
string-keyed property rather than an element.

---

## D-65 — The coercions are written from the grammar, not from a parser that is nearly right

**Status:** Accepted (M12) · **Affects:** M12, M13

`ToBoolean`, `ToNumber` and `ToString` are short and have an unusually high density of
surprises. Three are worth recording because getting them wrong is *easy* and *silent*.

**The falsy list is closed.** `undefined`, `null`, `false`, `±0`, `NaN`, `""`. Nothing else.
`Boolean("0")` and `Boolean("false")` are both true, and an implementation that "helpfully"
added `"0"` would break every truthiness check on a string in the ecosystem.

**`ToNumber` is not `parseInt`.** `Number("10abc")` is `NaN`; `parseInt("10abc")` is `10`.
Reaching for the lenient one because it usually works turns malformed input into a plausible
number, which is worse than a failure. `Number("")` is `0` — that is why `+[]` is `0` — and
`Number("   ")` is `0` too.

**Rust's `f64` parser is close to the grammar and not the same as it**, so the differences are
excluded explicitly rather than hoped over: `"inf"`, `"infinity"` and `"nan"` are Rust literals
and not JavaScript ones, `"1_000"` uses a separator that `ToNumber` does not allow, and a lone
`"."`, `"+"` or `"-"` is neither. Each has a test. Delegating to a parser that is *nearly* right
is the kind of shortcut that shows up years later as one engine disagreeing with the others.

**`String(-0)` is `"0"`.** The sign is observable through `Object.is` and not through text —
the mirror image of the `Map` key rule (D-62), and the reason those two cannot share one
comparison. The changeover to exponential form is exactly at `1e21` and below `1e-6`, both
specified rather than float-printing accidents, and the exponent always carries its sign
(`1e+21`, not Rust's `1e21`).

## D-66 — Well-known symbols are shared without being registered

**Status:** Accepted (M12) · **Affects:** M12

Three kinds of symbol behave differently in ways that are easy to conflate:

| | equal to another with the same description? | in the registry? |
|---|---|---|
| `Symbol("x")` | no | no |
| `Symbol.for("x")` | yes | yes |
| `Symbol.iterator` | there is only one | **no** |

The third row is the trap. Well-known symbols are shared across every realm, which *looks* like
registry behaviour — but `Symbol.keyFor(Symbol.iterator)` is `undefined`. Putting them in the
registry would make `Symbol.for("Symbol.iterator")` hand back the real one, which is exactly the
collision the registry's separate namespace exists to prevent. There is a test that asks for
that key and asserts it gets an impostor.

Also recorded because it is routinely conflated: `Symbol()` has **no** description while
`Symbol("")` has an empty one, and `description` reports `undefined` for the first.

## D-67 — Every error kind inherits from `Error`, and `name` lives on the prototype

**Status:** Accepted (M12) · **Affects:** M12

`TypeError.prototype`'s prototype **is** `Error.prototype`. That is what makes
`new TypeError() instanceof Error` true and what makes `catch (e) { if (e instanceof Error) }`
catch all of them. An implementation that gave each kind an independent prototype would pass
every test that constructs one and fail every real catch block in the wild — a failure that
only appears in someone else's code.

`name` comes from the prototype rather than the instance, which is observable:
`Object.keys(new TypeError("x"))` does not contain `"name"`, and `err.name = "Mine"` shadows
rather than replaces. `message` is the opposite — an own property, and only when non-empty,
which is why `new Error()` and `new Error("")` differ from `new Error("x")` in `Object.keys`.

`Error.prototype.toString` joins the two with `": "` **only when both halves exist**: a message
with no name is just the message, and a name with no message has no trailing colon. That detail
is in every stack trace anyone has ever read.

---

## D-68 — A `Proxy`'s traps are the easy half; the invariants are the point

**Status:** Accepted (M12) · **Affects:** M12, M13, §3.2

§3.2 makes `Proxy` a named product risk — it cannot be rejected if the ecosystem is a goal,
because Vue 3's reactivity, MobX, Immer, Valtio and Solid stores all depend on it. That
decision is about *cost*: shapes carry an `is_exotic` bit (D-54) so the fast path branches once.
This is about *correctness*, which is a separate and larger problem.

**A trap is a function call. What makes `Proxy` safe to have in a language is that the spec
checks the trap's answer against the target and throws when they disagree.** Without those
checks a proxy could report that a frozen property holds a different value than it does, and
every piece of code that reasoned about `Object.freeze` — including the engine's own optimiser —
would be reasoning about a lie.

So the invariant checks are the content, and each has a test that builds a *lying* trap and
asserts the lie is refused:

| trap | may not |
|---|---|
| `get` | report a non-configurable, non-writable property as anything but its value |
| `set` | claim success when that property's value would change |
| `has` | report a non-configurable own property as absent |
| `deleteProperty` | claim to have deleted a non-configurable property |
| `getOwnPropertyDescriptor` | report `undefined` for a non-configurable property |
| `ownKeys` | omit a non-configurable key, or invent one on a non-extensible target |
| `isExtensible` | disagree with the target **at all** |

That last row has *no latitude*, unlike the property traps where a proxy may invent properties
freely. `Object.isExtensible` is how code decides whether a shape can still change, so a proxy
that lied about it would invalidate that reasoning everywhere.

**An implementation with the traps and without the checks passes every test that uses a proxy
and fails only the ones that try to break one** — which is the direction real code exercises
after someone has already shipped a bug. Checked by removing the `get` check and watching its
test fail.

**Revocation is checked before the handler**, because revocation exists precisely to detach the
handler; consulting it first would defeat the mechanism. A revoked proxy with a lying trap
reports *revoked*, not *invalid*, and there is a test for that ordering.

## D-69 — `done` is coerced, and leaving a loop early closes the iterator

**Status:** Accepted (M12) · **Affects:** M12

`IteratorComplete` calls `ToBoolean` on `result.done`. So `{ done: 0 }` is **not** finished and
`{ done: "false" }` **is** — `"false"` is a non-empty string and therefore truthy (D-65). An
implementation comparing `done === true` loops forever on the first; one comparing `done == true`
disagrees somewhere else again. Checked by replacing the coercion with a comparison.

**The final result's `value` is a return value, not an element.** `for…of` and spread discard
it; `yield*` is the one place it is visible. Collecting has to stop *at* the done step rather
than after it.

**Leaving a loop early calls the iterator's `return`.** That is how a generator's `finally` runs
and how whatever the iterator was holding gets released. Skipping it leaks, and the leak is
invisible because the happy path — running to exhaustion — never exercises it. Exhaustion is
*not* early exit and must not close; there are tests for both directions.

**A finished iterator stays finished.** Once a done step has come out, later calls report done
again even if the underlying sequence has more. Without it an exhausted iterator asked again
would restart, and the protocol has no way to notice.

---

## D-70 — `isFrozen` is vacuously true, and `seal` differs from `freeze` by one bit

**Status:** Accepted (M12) · **Affects:** M12

`Object.isFrozen(Object.preventExtensions({}))` is **true**. Freeze was never called; the object
simply has no properties that could change and cannot gain any, so every condition in the
definition holds over an empty set. `isSealed` is the same.

This matters because code branches on `isFrozen` to decide whether it may mutate, and will take
the frozen path for an object nobody froze. An implementation that "corrected" it by tracking a
`frozen` flag would be more intuitive and would disagree with every engine. Checked by making
`isFrozen` demand at least one property and watching the test fail.

**`seal` and `freeze` differ by exactly one bit.** `seal` makes every own property
non-configurable and stops extensions; `freeze` does that *and* makes data properties
non-writable. **A sealed object's values can still change — only its shape is fixed.**
Conflating them gives either an object that reports sealed and silently accepts writes, or one
that rejects writes nobody asked it to reject. Checked by making `seal` also clear `writable`.

An accessor has no `writable` attribute, so freezing must not ask for one on it — and an
accessor does not prevent an object from being frozen.

## D-71 — `Number.isNaN` and the global `isNaN` are different functions

**Status:** Accepted (M12) · **Affects:** M12

| | `"NaN"` | `"1"` | `undefined` | `true` |
|---|---|---|---|---|
| `Number.isNaN` | `false` | `false` | `false` | `false` |
| global `isNaN` | **`true`** | `false` | **`true`** | `false` |
| `Number.isFinite` | `false` | **`false`** | `false` | **`false`** |
| global `isFinite` | `false` | **`true`** | `false` | **`true`** |

The globals coerce with `ToNumber` first; the `Number` ones do not. Reaching for whichever
happens to be in scope is how a string that looks numeric passes a guard meant to reject it —
`isFinite("1")` is `true`. Both are implemented, next to each other, so the difference is
visible at the point where someone picks one.

`Number.isInteger(5.0)` is **true**: every JavaScript number is a double, so `5` *is* `5.0` and
there is no separate integer type for this to distinguish.

`Number.isSafeInteger` stops at `2^53 - 1` because beyond it the representable doubles are
further apart than 1 — `2^53` and `2^53 + 1` are the same value. There is a test asserting that
collision directly, because the boundary means nothing without it. It is also why APIs with
64-bit ids send them as strings.

---

## D-72 — A JavaScript string is UTF-16, and is not required to be well-formed

**Status:** Accepted (M12) · **Affects:** M12, M13, M15

`JsString` stores `Vec<u16>`, not Rust's `String`.

A JavaScript string is a sequence of 16-bit code units and **may contain a lone surrogate**:
`"\uD800"` is an ordinary one-element string. Rust's `String` cannot hold that at all, so using
one would mean either rejecting legal input or silently replacing it with U+FFFD — corrupting
data at the boundary and losing the ability to round-trip anything that arrived over a network.

The cost is a conversion at every Rust boundary. What it buys is correctness on the cases that
actually occur: half an emoji arriving in one chunk of a stream, a filename from a Windows API,
a `JSON.parse` of a document written by something careless. `to_rust` returns `None` rather than
substituting, because a lossy replacement is how text gets corrupted somewhere far from where it
went wrong; `to_rust_lossy` exists and is documented as diagnostics-only.

**Length counts code units; iteration yields code points.** `"😀".length` is 2 and
`[..."😀"].length` is 1, and every index-taking method — `charAt`, `slice`, `indexOf`,
`substring` — works in code units. Slicing at an odd boundary therefore splits an emoji in half
and yields a lone surrogate. That is specified, and an implementation that "helpfully" snapped
indices to code-point boundaries would return different strings than every engine *and* stop
`slice` composing with `indexOf`.

**`slice` and `substring` differ twice**, which is what makes substituting one for the other a
reliable bug: `substring` **swaps** arguments that are the wrong way round and **clamps**
negatives to zero, while `slice` returns `""` and counts negatives from the end.

**`trim` removes the byte-order mark.** U+FEFF is not classified as whitespace by Unicode and
the specification trims it anyway — the one people miss, and the reason a file beginning with a
BOM otherwise leaves an invisible character on the front of its first field.

Three of these were checked by breaking them: making `slice` swap, dropping U+FEFF from the
whitespace set, and making `to_rust` lossy each fail their tests.

---

## D-73 — `Invalid Date` is a state, not an error, and pre-epoch arithmetic must floor

**Status:** Accepted (M12) · **Affects:** M12, M15

A `Date` is one number: milliseconds since the epoch. Three rules about that number carry most
of the correctness.

**`TimeClip` invalidates rather than clamps.** The time value must be an integer with magnitude
at most `8.64e15`; anything else becomes `NaN` and the date is permanently invalid. Clamping
would let a date **silently become a different date**, which is worse than an obviously broken
one. It must run on every construction and mutation, and `-0` is normalised so two dates either
side of the epoch do not compare unequal through a sign nobody can see.

**`day_from_time` floors; `time_within_day` uses `rem_euclid`.** Truncating puts
1969-12-31T23:00Z in day 0 rather than day -1, and a plain remainder gives it an hour of `-1`.
Both look correct for every date anyone tests by hand, and are wrong for everything before 1970.

**The epoch was a Thursday**, so the weekday offset is 4. Getting that constant wrong shifts
every weekday in the program by a fixed amount, which looks like a timezone bug and is not.

All three were checked by breaking them.

**Months wrap and days are 1-based while months are 0-based.** `new Date(2020, 12, 1)` is
January 2021 and `new Date(2020, 0, 0)` is 31 December 2019 — both specified, both used
deliberately, since `new Date(y, m + 1, 0)` is the idiomatic last day of month `m`. The
0-based/1-based inconsistency is in the language; normalising it here would make every ported
program wrong by one month.

**Only UTC.** Local-time accessors need the host's zone *and* its historical transition table,
which is M15's to supply. Implementing them against a guess would produce a date that is right
in one timezone and silently wrong in the rest — the worst available outcome, because it works
for whoever wrote it.

`toISOString` returns `None` for an invalid date rather than a string, because it **throws** a
`RangeError` where `toString` returns `"Invalid Date"` — two methods on the same object with
different failure modes, and the caller has to pick.

---

## D-74 — `regress`, because `lastIndex` is the hard part and backreferences are non-negotiable

**Status:** Accepted (M12) · **Affects:** M12

§M12 names `regress` and the reason is worth stating. Rust's `regex` crate deliberately omits
**backreferences and lookaround** to guarantee linear-time matching. JavaScript has both and
real code uses them, so a "close enough" engine would reject patterns that work in every
browser. The trade is that a pathological pattern can backtrack — a real denial-of-service
surface, and it belongs in the same conversation as any other untrusted input.

**What `regress` does not supply is the mutable cursor**, because it is not a JavaScript engine.
That cursor is the single most surprising thing about `RegExp`:

```js
const r = /a/g;
r.test("a");   // true   — lastIndex is now 1
r.test("a");   // false  — searching from 1 finds nothing, and resets to 0
r.test("a");   // true   — again
```

Three rules carry it, each mutation-tested:

- **`test` is `exec` with the result discarded**, so it mutates exactly as much. A stateless
  `test` would disagree with `exec` on the same object, which is worse than either behaviour on
  its own. Breaking this fails four tests.
- **A failed match resets `lastIndex` to zero.** That reset is what makes repeated calls
  *alternate* rather than staying false forever.
- **`y` anchors at `lastIndex`; `g` searches from it.** A sticky match found later in the string
  is not a match.

Without `g` or `y`, `lastIndex` is **inert** — assignable, and changes nothing. That is its own
source of confusion and there is a test pinning it.

`flags` reports in the specification's fixed order, so `/x/yg.flags` is `"gy"`. Echoing source
order would make two equivalent regexes compare unequal as strings. A repeated flag is a
`SyntaxError`, not something to ignore — accepting `/x/gg` lets a typo through.

**An empty match advances by one character**, or iteration never terminates: `/(?:)/g` matches
empty at every position. By *character*, not byte, so the bump cannot land inside a multi-byte
sequence and panic.

---

## D-75 — `Reflect` reports failure; `Object` throws. That difference is the point

**Status:** Accepted (M12) · **Affects:** M12

| | on failure |
|---|---|
| `Object.defineProperty` | **throws** a `TypeError` |
| `Reflect.defineProperty` | returns **`false`** |

The same split runs through `set`, `deleteProperty`, `preventExtensions` and `setPrototypeOf`.
Code that wants to *attempt* an operation and branch on the outcome has to wrap the `Object`
form in a `try`, which conflates "this was not allowed" with "something else went wrong inside a
getter". `Reflect` separates them, and that is the whole reason it exists.

**An implementation that made `Reflect.defineProperty` throw would still pass every test that
defines a property successfully.** The difference only appears on the failure path — the path
people write least and rely on most — so every test here exercises a refusal rather than a
success.

`Reflect.set` returning a boolean matters for the same reason: plain assignment *evaluates to
the value*, so it cannot report failure at all, and in sloppy mode a refused write is silent.

**`Reflect.ownKeys` includes non-enumerable properties**, unlike `Object.keys`. It mirrors the
internal method, not the iteration helper, and conflating the two was mutation-tested.

## D-76 — `new Boolean(false)` is truthy, and that is not a quirk

**Status:** Accepted (M12) · **Affects:** M12

`Boolean(x)` is `ToBoolean` and gives a primitive. `new Boolean(x)` gives an **object**, and
every object is truthy — so `if (new Boolean(false))` takes the branch.

This follows from objects being truthy, which is the same rule that makes `if (obj)` a null
check. It is not a special case for `Boolean` and must not be "fixed": a wrapper whose
truthiness followed its primitive would make `if (obj)` unreliable for every other object type.

`BooleanObject::is_truthy` is a `const fn` returning `true` precisely so the claim is in the
type rather than in a comment, and there is a test — because the assertion reads as a mistake.

## D-77 — An async step is a promise, and a rejected one ends the iteration

**Status:** Accepted (M12) · **Affects:** M12, M15

`for await` awaits each result, so the async protocol is the synchronous one with a promise
around every answer. Two consequences that are easy to miss:

**`done` is still coerced, and coerced *after* the promise settles.** A promise that fulfils
with `{ done: "false" }` finishes the loop (D-65, D-69).

**A rejected step ends the iteration and the rejection propagates.** Swallowing it and treating
it as "no more items" would turn a failed network page into a **quietly truncated list** — the
failure mode that looks like success, and the one that gets noticed weeks later by whoever
counts rows.

An already-fulfilled step promise is still asynchronous: its handler runs as a microtask (D-61),
which is what makes `for await` yield to the queue on every iteration even when nothing waits.

---

## D-78 — M12's acceptance cannot be met before M13, and the harness says so in numbers

**Status:** Accepted (M12) · **Affects:** M12, M13

§M12's acceptance is *"the relevant `test262` subset passes at >80% for implemented builtins"*.
**Every test262 test is a JavaScript program that must be executed**, and there is no way to
execute JavaScript in this repository: `compiler/codegen` is a stub scheduled for M13, `opt` is
a stub scheduled for M20, and there is no interpreter. Checked rather than assumed — the one
grep hit for "execute" was `evaluation_order`, which orders modules.

**So M12's acceptance depends on M13.** That is an ordering problem in the roadmap, not in the
implementation, and it is worth writing down: M12 is described as "large but mechanical, and
the most parallelizable work in the project", which is true of *writing* the builtins and not
true of *demonstrating* them.

**The pass rate is undefined, not 0%.** Reporting "0 of 12,719 passing" would imply the tests
ran and failed, which is a different and less accurate claim than "there is no way to run them".
The harness prints the distinction explicitly, because a number in a status table outlives the
caveat next to it.

### What the harness does do

It **measures the acceptance** rather than asserting one. Over the sparse subset for the
implemented builtins it finds:

| | |
|---|---|
| cases | 12,719 |
| need extra harness includes | 3,169 |
| expect failure (`negative:`) | 192 |
| `async` | 505 |
| distinct features required | 99 |

The frontmatter parser is hand-written against test262's restricted YAML subset — scalars, `|`
block text, `[a, b]` flow sequences — because a general parser would accept documents the corpus
does not contain while still needing the same amount of glue. The risk that carries is *silently
mis-parsing something unusual*, so the parse test runs over **all 12,719 files** rather than a
sample, and the counts were cross-checked against independent `grep`s: files, includes, negative
and async all match exactly.

`_FIXTURE.js` files are excluded. They are imported *by* tests rather than being tests, and
counting them would inflate the denominator — the wrong direction to be wrong in for a
percentage-based acceptance.

### CI

Fetched sparsely, **on Linux only**. The suite is a large checkout for a step that currently
validates a parser rather than running anything, so paying for it on three platforms would be
cost without a matching guarantee. When there is an execution engine this should widen to every
platform, because a pass rate is per-target.

`CRISOL_REQUIRE_TEST262` makes an absent suite a failure rather than a skip — the same
arrangement as `CRISOL_REQUIRE_GPU` and `CRISOL_REQUIRE_NODE_MODULES`, for the same reason. All
three paths were verified: absent-and-optional skips, absent-and-required fails,
present-and-required runs.
## D-79 — The IR had no arithmetic, and M13's acceptance needs it

**Status:** Accepted (M13) · **Affects:** M11, M13

§M11's deliverable lists the IR's ops: *"Load, Store, Call, PropertyLoad, PropertyStore,
CreateObject, CreateArray, Closure, Await, Throw, Branch, Compare"*. There is **no arithmetic
op in that list**, and M11 was built to it.

§M13's acceptance is that *"`main.ts` containing arithmetic, closures, classes, and array
methods compiles to a standalone binary"*. **All four of those were in M11's recorded
`unsupported` list**, and the first of them could not even be represented in the IR.

That is a gap in the plan rather than the implementation, and it is worth recording because the
plan reads as though M13 begins where M11 stopped. It does not: M13's acceptance requires
extending M11 first.

### `Add` is the odd operator

Every arithmetic operator except `+` coerces both operands with `ToNumber` and produces a
number. `+` does not — after `ToPrimitive`, a string operand concatenates. So `1 + 1` is `2` and
`1 + "1"` is `"11"`.

`Op::Binary` therefore types `Add` as **`Unknown`** and everything else as `Number`. Typing
`Add` as `Number` would let codegen emit a float add for a string concatenation, which is a
miscompilation rather than a slow path. `BinaryOp::is_always_numeric` exists so that decision is
asked as a question rather than assumed, and it is mutation-tested.

`+` is also the only arithmetic operator that **can collect**: `ToPrimitive` calls `valueOf` or
`toString`, which is user code. The others coerce primitives that already exist.

## D-80 — `&&`, `||` and `??` are control flow, not operators

**Status:** Accepted (M13) · **Affects:** M11, M13

`a && b` must not evaluate `b` when `a` is falsy. Lowering these as a two-operand instruction
would evaluate both — which does not merely lose an optimisation, it **changes what the program
does**: a side effect in `b` would run when the source says it must not.

So each lowers to a branch, with the result travelling through a compiler temporary. The
temporaries are named with a leading space, which the grammar does not allow in an identifier,
so a program cannot declare a variable that shadows one.

**`??` is not `||`.** It tests for `null` or `undefined`, not falsiness, so `0 ?? 1` is `0`
where `0 || 1` is `1`. Conflating them is precisely the bug that made `??` worth adding to the
language, and the lowering emits explicit comparisons against both nullish values rather than
branching on the operand. Checked by making it branch on truthiness: two tests fail, including
the corpus snapshot.

The conditional operator is the same shape for the same reason.

**A hole in an array literal is still recorded as unsupported.** `[1, , 3]` has a hole at index
1, and a hole is not `undefined` (D-64) — the IR has no way to express one, so filling it with
`undefined` would produce a value that reads the same and answers `in` differently. Recorded
rather than guessed, per D-59.

---

## D-81 — A name is captured exactly when resolving it leaves the scope

**Status:** Accepted (M13) · **Affects:** M11, M13

Closures need to know which outer variables a nested function reads. That is usually a
free-variable pre-pass over the AST. Here it falls out of resolution: **a name is a capture
exactly when looking it up walks out of the current function's scope**, so the lookup *is* the
analysis and there is no second traversal to keep in step with the first.

The distinction that makes it work is between two operations that look alike:

- `slot(name)` — *read* a name. Walks outward, and records a capture if it finds one.
- `declare(name)` — *bind* a name. Always local, and shadows whatever is outside.

`let`, `const`, `var` and **parameters** all declare. Using `slot` for a parameter would capture
the outer binding of the same name and then immediately overwrite it with the argument, so
`let a = 1; (a) => a` would close over a value it never reads. Both directions are
mutation-tested.

**Captures come back from `lower_function` as names, not slots.** The inner function knows which
slot a capture lands in; the *enclosing* one knows which value to put there. Resolving the name
again in the enclosing scope is what makes a chain work — `() => () => a` captures `a` at each
level, because the middle function's read of `a` is itself a capture.

### The check that needs more than one function

[`Function::captures`] and `Op::Closure`'s `captures` pair **by position**. A mismatch leaves a
slot uninitialised, and an uninitialised slot holds a *plausible* value — the worst kind of
wrong.

That cannot be checked by a verifier that sees one function at a time, which is why
`verify_module` exists alongside `verify`. It was added because a doc comment claimed "nothing
checks it but the verifier" and that was **false** when written — the honest repair was to make
the claim true rather than soften it, since the check is worth having.

### Not modelled

**Function declaration hoisting.** The binding appears where the declaration does, so calling a
function before its declaration reads an unset slot rather than working. Recorded in
`unsupported` rather than left silently half-right — a hoisting bug looks like a scoping bug and
is very hard to find from the symptom.

---

## D-82 — A call carries its receiver, and `this`-binding falls out of scoping

**Status:** Accepted (M13) · **Affects:** M13

`Op::Call` gained a `this_value`, and it is **not optional**. A plain `f()` passes `undefined`
explicitly rather than omitting it, because "no receiver" and "a receiver that is `undefined`"
are the same thing in the language — making one of them absent invites a lowering to forget it.

**It had already been forgotten.** `o.m()` lowered to a `PropertyLoad` followed by a `Call` with
no receiver, so `this` inside `m` was wrong. The corpus contained `let o = { a: 1 }; let v =
o.a();` and **the snapshot had been recording that wrong IR as correct** since M11 — a reviewed
snapshot only catches what a reader thinks to look for, and nobody looks for a field that does
not exist yet.

The failure mode is why it survived: losing a receiver is **silent**. The call still happens and
still returns something. Only `this` is wrong, and only inside the callee.

The object is evaluated **once** and shared between the property load and the receiver, because
`f().m()` must not call `f` twice.

### `this`-binding is one flag

A non-arrow function **declares** `this`, so it shadows. An arrow does not, so a `this` inside it
resolves outward and becomes an ordinary capture (D-81).

That is the entire rule, and it falls out of the scope machinery rather than needing logic of
its own — which is the payoff for having made `slot` (read, may capture) and `declare` (bind,
always shadows) different operations. `this` is a reserved word, so using it as a slot name
cannot collide with anything a program can write.

The program itself binds `this` too, so a top-level arrow captures it rather than inventing one.

### Testing against the dump, and not against value numbers

The receiver test was first written as a substring match on `call v4(this=v1`. That passes for
the wrong reason as soon as an earlier instruction moves — and it did, because adding `this` as
slot 0 shifted every other slot. It is now structural: find the `PropertyLoad`, find the `Call`,
and assert the call's receiver *is* the object the method was loaded from.

---

## D-84 — The Cranelift backend, and three things the plan did not anticipate

**Status:** Accepted (M13) · **Affects:** M13, M14, M20

§4 names Cranelift and §M13 needs stack maps at safepoints, which is the deciding feature
rather than a convenience. A JavaScript value is NaN-boxed into 64 bits (D-53), so Cranelift
sees `I64` everywhere and reaching a number is a bitcast — no separate float register class in
the calling convention, no boxing at a call boundary.

Three things came out differently from what the plan implies.

### The oxc pin caps Cranelift at 0.128

`oxc_allocator 0.91` depends on `bumpalo` **exactly** `=3.19.0`; Cranelift 0.132+ needs
`^3.20.2`. No version satisfies both, so the workspace cannot resolve.

D-57 pinned oxc at 0.91 because newer oxc needs Rust 1.96 against a 1.87 MSRV, reasoning that
"a parser dependency in Track B is not a reason to raise the floor for everybody". That still
holds — 0.128 is a current, supported Cranelift — but the pin now constrains the *backend* as
well, which is a stronger consequence than the original decision weighed. Worth revisiting when
either side moves.

`all-arch` is not a default feature. Without it only the host ISA is available, so three of
§M13's four named targets would fail to construct — and a backend that only built for the host
would pass every test written on one machine.

### Stack maps are per-value, not a flag

There is no `enable_safepoints` setting in this version; asking for one fails, which is how
this was found. Stack maps are requested with `declare_value_needs_stack_map` per value.

**That is the better fit.** §M11 made safepoints carry an explicit live set, and this API wants
exactly that set rather than a whole-function switch — the two line up without translation.

`Report::stack_map_entries` counts what was handed over, because §M13's deliverable is
*emission* and a test that only checks the function compiled verifies nothing about it. The
count is honest about its limit: **`cranelift-object` does not write stack maps into a section**,
so M13's GC integration will have to carry them out of band. Claiming "stack maps are emitted"
without that distinction is the kind of statement that looks true until someone goes looking for
the section.

### `+` is a call, and that is D-79 arriving in the machine code

Every arithmetic operator except `+` is typed `Number` by the IR and lowers to a native `f64`
instruction. `+` is typed `Unknown` because it may concatenate — and **an `Unknown` cannot
become a float add**, because the operands might be strings.

So `Add` lowers to a call to `crisol_add`. That is not a shortcoming: it is what every engine
does before type feedback narrows the operands, and the alternative is a miscompilation.
Narrowing it is M20's, and the IR already carries the type a pass would need. There is a test
that looks for the symbol in the object file, because an `fadd` would leave no relocation to
find.

**Everything else is refused rather than approximated.** `%`, `**` and the bitwise operators
need int32 coercion or a libm call; `===` on boxed values is a bit comparison *except* for NaN
and ±0, which is D-53's whole subject. A backend that guessed at any of them would emit code
that runs and is wrong — indistinguishable from correct code by testing the compiler, and only
visible by running the program and noticing the answer.

### A test that was wrong about liveness

The stack-map test first declared a value whose only use *was* the call's argument, and asserted
one entry. The count was 0 — **and the test was wrong, not the backend**: a value whose last use
is the call argument does not need to survive the collection. Corrected to use the value after
the call.

---

## D-85 — The floor is 1.96, and it is now verified

**Status:** Accepted (M13) · **Supersedes:** D-57's MSRV clause · **Affects:** whole workspace

D-57 pinned oxc at 0.91 to hold a 1.87 floor, reasoning that "a parser dependency in Track B is
not a reason to raise the floor for everybody". Two things have changed that.

**The pin stopped being about the parser.** `oxc_allocator 0.91` requires `bumpalo` *exactly*
`=3.19.0`, and Cranelift 0.132+ requires `^3.20.2` — so the pin capped the **backend** at 0.128
as well. A constraint chosen to protect embeddability was, by then, deciding which code
generator the project could use.

**The conflict dissolves on upgrade.** `oxc_allocator 0.150` has **no `bumpalo` dependency at
all**. Upgrading removes the wall rather than working around it, which is why this is a version
bump and not a vendored patch.

The floor is now **1.96**: oxc 0.150 needs 1.96 and Cranelift 0.135.2 needs 1.95, so 1.96 is the
lower bound of "both current". The alternative considered was a per-crate floor — 1.96 for the
two compiler crates and 1.87 for the other twenty-five, since Track A touches neither dependency
— and it was rejected in favour of one honest number for the workspace.

### The floor was never verified

This is the part worth recording. `rust-version = "1.87"` was declared and **nothing ever built
against it**: every CI job used `stable`, and there was no MSRV job. The number was an
aspiration presented as a guarantee, and a crate could have broken it at any point without
anyone noticing until an embedder complained.

There is now an `msrv` job that reads the floor **out of `Cargo.toml`** — rather than repeating
it, so the job cannot drift from the number it checks — installs exactly that toolchain, and
runs `cargo check --workspace --all-features`.

`check` and not `test`: the promise is that the crates *compile* on the floor. Running the suite
there would also bind dev-dependencies to it, which is not part of what an embedder relies on
and would raise the floor for a reason nobody asked for.

An unverified MSRV is indistinguishable from a false one — the same argument as
`CRISOL_REQUIRE_GPU`, where a skipped test and a passing test look alike.

### What the upgrade cost

Three API changes, each an improvement upstream:

- `ParserReturn::errors` became `diagnostics`.
- **Arrow bodies are a proper enum now.** The old code reconstructed concise-versus-block from a
  boolean plus a guess at the single statement inside; `get_expression()` and
  `get_function_body()` make it a type distinction, which deleted a comment apologising for the
  old shape.
- `MemFlags` became `MemFlagsData` for `bitcast`, and `finalize` takes the target's frontend
  config.

**Every frontend test passed unchanged, including the corpus snapshot.** A fifty-nine-release
jump in the parser produced byte-identical IR, which is the strongest evidence available that
the upgrade changed nothing about meaning.

### Raising the floor changed what clippy advises

A second-order effect worth recording, because it failed CI in a crate the upgrade never
touched.

Clippy gates lints on the declared MSRV. `collapsible_if` suggests a **let-chain**, which
stabilised in 1.88 — so at a 1.87 floor the lint stayed quiet, and at 1.96 it fires. One
instance existed, in `ui/umbrella/examples/todo.rs`, and CI caught it on all three desktop
platforms.

The consequence to remember: **an MSRV bump can fail CI in code the change never touched**, and
a crate-scoped local gate cannot see it. The lint fired in Track A, while every edit was in the
compiler crates.

**The second instance was inside `#[cfg(target_os = "windows")]`**, which a macOS clippy run
structurally cannot see — the first fix went green locally and failed Windows anyway. That gap
is closeable: `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu` were already installed, and
`cargo clippy --target <triple>` type-checks platform-gated code without needing a linker.

So the local gate now includes a cross-target clippy pass for the two non-host desktop targets.
Earlier in the project three CI failures were attributed to "what local runs structurally
cannot catch" — for *this* class of failure that was not true, only unattempted.

---

## D-86 — The type lattice makes `===` cheap, and deleting a test cannot fail a test run

**Status:** Accepted (M13) · **Affects:** M13, M20

The first backend refused `===` outright, reasoning that "`===` on boxed values is a bit
comparison *except* for NaN and ±0". That is true, and it was **over-cautious for the case the
IR had already proved**.

On two values the lattice typed `Number`, `===` is exactly `f64` equality: `NaN === NaN` is
false and `fcmp eq` on NaN is false; `+0 === -0` is true and `fcmp eq` on the two zeroes is
true. The hard cases are hard *because the operands are boxed*, and the lattice says when they
are not. So `===` now lowers to a native comparison when both operands are `Type::Number`, and
is still refused otherwise.

This is the first place the type lattice (D-58) has paid for itself in emitted code, and it is
worth noting the shape: the lattice did not make a *fast* path possible, it made a **correct**
one possible that had been refused for want of the information.

### The operators that are calls, and why each one

| operator | why not an instruction |
|---|---|
| `+` | may concatenate — the IR types it `Unknown` (D-79) |
| `%`, `**` | libm calls |
| `&`, `\|`, `^`, `<<`, `>>`, `>>>` | `ToInt32` wraps **modulo 2^32** |

The bitwise row is the one worth stating: Cranelift's float-to-int conversion **saturates**, so
lowering `1e10 \| 0` with it would clamp rather than wrap. That is a *wrong number*, not a slow
one, and it would look entirely plausible.

One symbol per operator rather than a single `crisol_binary(op, a, b)`: an opcode passed at
runtime is a branch the linker cannot see through, and separate symbols are what let a later
pass replace one operator without touching the others.

### Deleting a test cannot fail a test run

While replacing two superseded tests, a slice-based edit also removed the two **stack map**
tests — the ones verifying §M13's own deliverable. The suite went green, because removing a
test never fails.

That asymmetry is worth naming: every other kind of mistake in a test file shows up as a
failure, and this one shows up as a slightly smaller number that nobody is watching. Caught by
reading the list of test names in the output rather than the pass count, and the count is now
asserted alongside them.

---

## D-87 — The runtime ABI, and a contract nothing checked until link time

**Status:** Accepted (M13) · **Affects:** M13

`crisol-abi` defines the symbols generated code calls. Until it existed, every object file the
backend produced referenced undefined symbols, so "compiles" and "links" were separated by a gap
nothing measured.

Every function takes and returns `u64` — a value NaN-boxed into 64 bits (D-53). No wrapper type
at the boundary: the caller is machine code with no notion of Rust types, so a
`#[repr(transparent)]` newtype would be a comment rather than a guarantee.

### `ToInt32` is why the bitwise operators are calls

The specification truncates toward zero and wraps **modulo 2³²**. The hardware's conversion
**saturates**. So `1e10 | 0` is `1410065408` in JavaScript and `i32::MAX` if lowered as an
instruction — *both are numbers*, and only one is right. There is a test asserting the correct
value and asserting that the saturating cast gives a different one, so the reason these are
calls is visible rather than asserted.

Related, and each with a test: `%` takes the sign of the **dividend** (`-5 % 3` is `-2`, not
`1`); the shift count is masked to five bits, so `1 << 32` is `1`; and `>>>` is the only shift
whose result reads as unsigned, which is why `-1 >>> 0` is `4294967295` and it cannot be folded
in with the other two.

**A non-numeric operand yields `NaN`, never `0`.** Strings need the runtime's string table,
which does not exist yet. Returning `0` would make `"5" * 2` evaluate to `0` instead of `10` —
arithmetic that looks like arithmetic, rather than a gap that looks like a gap.

### The symbol contract

The backend declares imports **by name** and this crate defines them **by name**, and nothing
connects the two until a linker runs. A typo on either side is silent through every compiler
test — the object file still builds, with an undefined symbol in it — and fails only when
someone first tries to produce a binary.

So `crisol-abi::SYMBOLS` is the defining list, `crisol_codegen::helper_symbols()` exposes what
the backend emits, and a test compares them. It was checked by introducing a typo and watching
it fail, because a test that reads two lists and finds them equal is exactly the kind that can
pass while comparing nothing.

---

## D-88 — Compiling is not computing, and only running tells them apart

**Status:** Accepted (M13) · **Affects:** M13, M14

Every codegen test written before this one checked that a function **compiled**. None checked
what it **returned**. `6 / 3` compiling says nothing about whether it yields `2`, and the gap
had been open since the backend landed.

`Jit` compiles into this process's memory and hands back a callable address, so the tests call
the generated code and check the answer. Three of them can be made *no other way*:

- **`1e10 | 0` returns `1410065408`.** This is the assertion the whole call-not-instruction
  decision (D-87) exists for. A saturating conversion gives `i32::MAX` — a number, and the wrong
  one. Inspecting the object file cannot distinguish them; only running can.
- **`NaN === NaN` is `false` and `+0 === -0` is `true`**, checked by execution rather than by
  reasoning about which instruction was emitted (D-86).
- **A branch takes the right arm**, with the other arm returning a number too — so branching
  wrongly produces a plausible answer rather than a crash.

### Symbols are registered by the caller

The helpers are **not** resolved from the host process. The test passes them in from
`crisol-abi`, which keeps `crisol-codegen` free of a dependency on the runtime it generates
calls to, and makes the contract visible at the point of use rather than implicit in a link
order.

It also turns the symbol check from a comparison into an *exercise*: D-87's test reads two lists
and finds them equal, which is the kind of test that can pass while comparing nothing. This one
fails to **resolve** if a name is wrong.

### On shipping a JIT

§2.3 promises no interpreter in shipped artifacts. That is about the **application** binary;
`crisol-codegen` is a build-time crate and never ships inside one. M14's differential testing
needs this same ability besides — comparing two backends' results is not possible with only an
object emitter and a linker in the loop.
## D-83 — A class is sugar, and reading the snapshot found two bugs no test would have

**Status:** Accepted (M13) · **Affects:** M13

A `class` desugars here rather than becoming an IR node, because a class **is** a constructor
function whose `prototype` property holds an object carrying the methods. Every instance shares
that one prototype object — an implementation that copied methods onto each instance would work
until someone compared two objects' methods for identity, or counted `Object.keys`.

`new` is **one op**, not the sequence it stands for. That sequence has a rule no call site should
have to remember: **a constructor returning an object replaces the newly created `this`**, while
one returning a primitive does not. Spelling `new` out as allocate-then-call would put that rule
at every site, and the first lowering to forget it would produce a constructor whose explicit
`return` is silently ignored. `Op::Construct` also covers `OrdinaryCreateFromConstructor`, so the
prototype link cannot be omitted separately.

### Two bugs, both found by reading the generated snapshot

Neither was caught by a test, because both produced IR that verified and dumped cleanly.

**Methods declared after the constructor were dropped.** The lowering returned as soon as it
found the constructor. `constructor` conventionally comes first, so **the common ordering was
the broken one** — `class C { constructor() {} m() {} }` lost `m` entirely.

**`this.x = x` lowered to `x = x`.** oxc's `AssignmentTarget::get_identifier_name` reports the
**property** name for a member target, so `this.x` came back as `"x"` and was treated as a
variable. It produced a store to the parameter's own slot: no note, no error, verified fine.
That is precisely the outcome the `unsupported` list exists to prevent (D-59) — a translation
that runs and is wrong beats one that refuses, and this one *claimed to be faithful*.

The fix matches on the target's **shape** rather than asking a helper for a name. Both bugs now
have a regression test, and both mutations that reintroduce them fail.

This is the third time reading a generated snapshot has found a defect no test did — after the
object-shape soundness bug (D-59) and the missing call receiver (D-82). The pattern is
consistent: a snapshot catches what a reader notices, and misses what the IR cannot yet express.

### Recorded rather than half-done

`extends` needs the prototype chain wired through the parent *and* `super` resolved inside
methods; half of that produces a class that constructs and then fails its first inherited call.
Static members, computed method names and non-method class elements are recorded too.

---

## D-89 — Source to a running binary, and what that revealed

**Status:** Accepted (M13) · **Affects:** M13

`crisol build` now takes a file and produces a native executable: parse, lower, verify, compile,
link. §M13's acceptance is phrased as *"compiles to a standalone binary that runs and produces
correct output"*, and the tests build and **run** real programs — one that only checked the
binary existed would pass for a binary that printed nothing.

**Position-independent code is not optional.** The object has to *call* the runtime helpers, and
without `is_pic` the linker refuses with "illegal text-relocations". The failure was instructive:
a program using only `-` linked and ran, while `2 + 3` did not — because only the latter emits a
call. A backend tested solely on native instructions would never have found it.

**The entry point is C, not Rust.** Five lines, written to a temporary file and compiled by the
host's `cc`. A Rust `main` would drag in `std`'s runtime initialisation and make a compiled
program's contents depend on a Rust version rather than on what was compiled. It calls the
runtime's `crisol_print` rather than decoding the value, because the NaN-box layout is the
runtime's business and a copy of it in generated C would be a second place for it to drift.

**Build errors name the stage.** Parse, unsupported, malformed, codegen, link — because "it did
not build" is not actionable, and the interesting part is always *which* gave up. `Unsupported`
is deliberately separate from `Parse`: one is a mistake in the program and the other is a gap in
the compiler, and they need different responses from whoever reads them.

A program the compiler cannot fully handle is **refused**, not compiled with the gaps omitted
(D-59). There is a test for that, because a compiler that silently drops what it did not
understand produces a binary that runs and is wrong.

### What the acceptance actually covers

§M13 names four things. Measured by building and running:

| | |
|---|---|
| arithmetic, comparisons, control flow, locals | **works end to end** |
| closures | `Op::Closure` is not lowered |
| classes | `Op::Construct` is not lowered |
| array methods | `Op::CreateArray` is not lowered |

**One of the four.** The pipeline is real and the coverage is not there yet, and saying
"M13's acceptance path works" without that table would be the more flattering sentence and the
less true one.

---

## D-90 — The stack map table, and why it is flat

**Status:** Accepted (M13) · **Affects:** M13, M9

A precise collector needs to know, at the instant it runs, where every live reference is. Rust
code says so by pushing onto a shadow stack; **compiled machine code cannot** — its values are
in registers and frame slots, with no list anywhere. That is what a stack map is for, and
without one a collection during compiled code would free values still in use: §3.1's exact
failure.

D-87 recorded that `cranelift-object` does not write stack maps into a section. That is true of
the *writer* and not of the information: `MachBufferFinalized::user_stack_maps()` is public and
yields `(code_offset, frame_size, map)`. Carrying it across is this crate's job — the same job
Go's linker does for `pclntab`.

**Cranelift spills every live value to the frame before a safepoint.** That is the detail that
makes this tractable: the collector reads stack slots and nothing else, so no register maps are
needed. Go needed those too, but only once it began preempting goroutines *mid-function*
(1.14). Safepoints at calls and allocations stay in the simpler regime.

### Flat, one row per live value

`{ function address, code offset, frame offset }`, sixteen bytes, repeated. Not a nested
structure with per-safepoint length prefixes.

It costs a few bytes. What it buys is that **the runtime reading this table will be walking a
stack while the heap is mid-collection**, which is the worst imaginable place for a
length-prefix parser to be subtly wrong. A flat table needs no parsing at all.

The function address is a **relocation** — nothing at compile time knows where the code will
land, so the linker fills it in.

**Little-endian is written explicitly** rather than using native byte order. The object is for
the *target*, which need not be the host. All four targets are little-endian today, so this
cannot currently be observed — which is precisely what would make it a miserable bug to find
later.

An empty table is emitted for a program with no safepoints, rather than omitting the symbol:
the runtime looks it up unconditionally, and "absent" and "empty" would otherwise need
different handling for no reason.

### Still ahead

The table exists in the linked binary — verified with `nm`, not assumed. What remains is the
runtime side: walking native frames to find return addresses, matching them against the table,
and reading the live slots. Until that lands, collection during compiled code is still unsafe,
and §M13's GC stress requirement is not met.

## D-91

**The collector asks for compiled roots through a callback, rather than calling the runtime.**

Status: Accepted

`mark` started from the shadow stack alone, which is every root a Rust embedder has and none
of the ones compiled code holds. Compiled code keeps values in registers and frame slots and
pushes nothing, so without this a collection frees values a running program is still using —
the use-after-free ROADMAP §3.1 calls the worst failure mode of this milestone to debug.

The obvious wiring is for `Heap::mark` to call `crisol_abi::compiled_roots` directly. That
inverts the dependency: `crisol-abi` needs `crisol-gc` to allocate, so `crisol-gc` cannot
depend on `crisol-abi`. Instead the collector declares the hole — `set_extra_roots`, taking a
closure — and `crisol-abi::install_compiled_roots` fills it, because walking a native stack is
the ABI's business and the bit layout of a frame is not something the collector should know.

The seam is worth having for its own sake: the tests can install a provider that returns a
known handle, so the "a root only compiled code holds survives" behaviour is checked without
generating and running machine code. Both halves are tested — the paired case where nothing
reports the object and it is correctly swept is what makes the surviving case evidence.

**Consequence:** a heap with no provider installed is not wrong, it is an embedder running no
compiled code. That makes "forgot to install" indistinguishable from "nothing to install" at
the API level, which is a real hazard: the symptom is a freed live value, far from the cause.
`has_extra_roots` exists so a caller can assert it, and the compiled entry point installs
before it runs anything.

## D-92

**`Heap::transition` refuses to shrink an object.**

Status: Accepted

An object literal is lowered as an empty allocation followed by one `PropertyStore` per
property (see the comment in `lower.rs::object`), and a shape names the properties an object
has. So storing a new property must move the object to the shape that includes it, and the
heap had no way to do that — `alloc` fixed shape and slot count for the object's whole life.
Every object literal in compiled code was blocked on this, not on codegen.

Growing is the only direction allowed. Shrinking would drop the values in the slots past the
new end, and any of those may be the last reference to a live object — so a shape transition
that happened to narrow would silently turn into a collection bug, appearing later and
somewhere else. Removing a property therefore has to be written as an explicit rebuild, where
the values being discarded are discarded visibly.

New slots arrive as `undefined` rather than uninitialised, for the same reason the collector is
precise: an unwritten slot holding a plausible bit pattern is exactly what a precise marker
would read as a reference and follow.

**Consequence:** `transition` trusts its caller that the shape and the slot count agree. The
heap cannot check it, because it holds no `Shapes` table and deliberately does not — shapes are
`crisol-value`'s business. A caller that grows to a slot count disagreeing with the shape gets
an object whose properties resolve to the wrong slots, which is why the only intended caller is
the runtime's property-store path rather than embedder code.

## D-93

**Stack map offsets are measured from the stack pointer, and the frame walk reports one.**

Status: Accepted

Cranelift documents a user stack map entry as *"the offset from SP"* — given `(i64, 0x42)`,
`SP + 0x42` holds the live reference. The collector was reading `FP - offset`: the wrong origin
and the wrong direction, so every root it reported was whatever happened to sit that far the
wrong way from the other end of the frame.

Nothing failed. Every acceptance test built and ran a program that printed the right answer,
because none of them allocated enough to trigger a collection — the roots were wrong and never
consulted. `CRISOL_GC_STRESS=1` collects on every allocation, and under it the same programs
printed `NaN`: a property read from an object that had already been freed. That is the whole
argument for the stress mode ROADMAP §3.1 asks for, and it earned its place on the first run.

The walk reports each frame's **stack pointer at the call**, not its frame pointer. Both
aarch64 and x86-64 enter a function with the return address and the saved frame pointer at the
top of the callee's frame, so the callee's `fp` points at those two words and the caller's
stack pointer at the call is `fp + 16`. The walk already had that value; it was discarding it
in favour of the caller's frame pointer.

The `u32` alongside each map is a **span**, not a frame size. It had been named `frame_size`
and documented as letting the collector find slot zero from the frame pointer, which is what
made the wrong arithmetic look reasonable. It cannot: it is how many bytes the map covers, and
no arithmetic on it converts a frame pointer into a stack pointer.

**Consequence:** the walk now depends on the standard prologue on both architectures, which is
already required for the chain itself and is why `preserve_frame_pointers` is set. A target
that laid its frame out differently would need its own rule here, and there is nothing in the
code that would catch it — the stress-mode acceptance test would, which is the argument for
running it in CI on every target rather than only where it is convenient.

## D-94

**Every compiled function takes `(closure, this, new.target, argc, argv)`.**

Status: Accepted

A function used to compile to a machine function with its JavaScript arity baked into the
signature, so a call site had to know exactly which function it was reaching. A first-class
function value has no statically known arity — a callback handed to `arr.map` could take any
number of parameters — so that convention cannot express a callback at all, and with it go
closures, class methods and every array method. M13's acceptance asks for all three.

The five operands are the same for every function whatever its source arity. They land in
registers on all four targets by being the first parameters; that is the platform's own
calling convention doing the work rather than a choice made here.

`argv` points into the **caller's stack frame**, not a heap list. Two reasons, and the second
is the one that matters: there is no allocation per call, and arguments are already traced,
because the collector reads frame slots through the stack maps (D-93). A heap list would need
its own rooting and would allocate on the hottest path in the language.

*(Corrected by D-208: that second reason is about the argument **values**, which are live at
the call and so are in its stack map. The slot itself is not scanned, and the difference shows
the moment anything allocates between filling it and entering the callee.)*

`new.target` is in the signature although nothing reads it until classes. Adding a parameter
later rewrites every call site, and the slot costs a register that is free anyway.

A parameter the caller did not pass reads as `undefined`, which is what the specification says
rather than a convenience. It is read under a *select* rather than a branch: the index is
clamped to zero so the load is in bounds at any arity, the load always happens, and the result
is discarded when the parameter was not passed. `ARGV_MIN_SLOTS` is what makes the clamped load
safe, so a call with no arguments still reserves one slot — eight bytes of stack to remove a
branch from every parameter of every function.

**Rejected — a direct fast path now (the other half of the answer to "why not both").** When
the callee is statically known the uniform path is pure overhead, and that path is worth
having. It is not built first because nothing can be measured until calls work at all, and two
call paths from the start are two chances to miscompile in a way that shows up on only one of
them. It is recorded in ROADMAP M13 as the next step rather than deferred to M20, to be built
as soon as there is a number saying it is needed. Note the first fix for slow calls may not be
this: every live variable is currently spilled to the frame at every safepoint, because the IR
cannot say which slots can hold references. Narrowing that is the larger win.

**Consequence:** `this` and `new.target` arrive but are not yet bound. The frontend models
`this` as a slot it declares ahead of the parameters, and nothing in `Function` records which
slot that is — depending on "slot zero by construction" would couple the two silently, so
binding it needs a field on `Function`. Until then a program reading `this` will not compile,
which is the honest failure rather than a wrong value.

## D-95

**A closure holds its function's *index*, and the program registers a table of addresses.**

Status: Accepted

A closure has to say which code it runs. The obvious thing is to store the code address, and it
does not fit: a heap slot holds a `Value`, and a NaN-boxed value's payload is 48 bits, which is
not a promise every platform's code addresses keep. So slot zero holds the `FunctionId` as an
ordinary number and the captures follow it.

Turning that index into an address needs a table, and only the linker knows where the code
landed — the same problem the stack map table has, solved the same way (D-90): the object
carries a data symbol of relocated function addresses, and the C entry point hands it to the
runtime before anything runs.

**`crisol_closure_code` never returns null.** A value that is not a closure, or an index outside
the table, yields `crisol_not_a_function` — a real function with the uniform signature that
returns `undefined`. `5()` is a `TypeError` and throwing needs a path M13 does not have, but
the important part is that a bad callee costs a wasted call rather than a jump through a null
pointer. Putting the check in the runtime rather than at every call site costs nothing on the
hot path, and when exceptions land that helper is where the `TypeError` is raised, with no call
site changing.

**Symbols are named from the id, not the source name.** Source names are neither unique nor
valid identifiers: two `function (x) {…}` expressions are both "anonymous", a method is
"C.method", a temporary is " tmp0". Naming symbols after them made a two-anonymous-function
program fail to link with a duplicate-symbol error — and two functions sharing a *source* name
would have been worse, because one would have silently won. Function zero keeps the name the C
entry point calls; the rest carry their source name only as a readable suffix.

**Consequence:** captures are written one at a time after the allocation rather than passed to
it. A variadic call would need the backend and the runtime to agree on argument layout, and
those two meet only at link time, where a disagreement is silent. The cost is a call per
capture at closure creation, which is not the hot path — calling the closure is.

## D-96

**A closure's function index and captures are engine-private, not property slots.**

Status: Accepted

They were property slots, and it was silently wrong. A shape numbers properties from zero, so
the first property stored on a function took slot zero — where the function index lived. The
function then named whatever that property held, `crisol_closure_code` found no number, and
the call landed on `crisol_not_a_function`.

`class C {}` does exactly that to its own constructor: the class lowering stores `prototype` on
it. So **every class constructor was uncallable**, and the failure was invisible — `new C()`
still returned a correctly prototyped object, because `crisol_construct_this` runs before the
constructor. Only the constructor's *body* never ran, so instances came back with no fields and
`p.x` was `undefined`. Nothing crashed and no test caught it until one ran a class end to end.

Heap objects now carry an `internals` array beside their slots: engine state that no property
access can reach, traced by the collector like anything else, since captures are values the
program can still get at.

**Consequence:** the two kinds of state can no longer collide by construction, rather than by
everyone remembering to leave slot zero alone. It also means a function has no property slots
at all until something stores one, which is what an object literal already does.

## D-97

**A variable that is both captured and assigned lives in a heap cell, shared rather than copied.**

Status: Accepted — the defect below is fixed

`Op::Closure` copies each captured value into the closure when it is created. JavaScript
captures the *binding*, not the value, so two programs give the wrong answer today:

```js
let n = 0; let f = function () { n = 1; }; f(); n;   // gives 0, should be 1
let n = 1; let f = function () { return n; }; n = 2; f();  // gives 1, should be 2
```

Both compile, run, and print a plausible number. That is the failure mode D-59 argues is worse
than not building at all, so it is written down here rather than left to be discovered.

The fix is the standard one. Such a variable lives in a heap cell; the closure and the
enclosing scope hold the same cell, so a write through either is seen by both.

**The decision has to be made before lowering, which is why it is a separate pass.** The
lowering discovers a capture *when it happens* — a name is captured exactly when resolving it
walks out of the current scope — and by then the enclosing function's code is already emitted.
Whether a variable is a cell changes every read and write of it, so the question cannot be
answered late. `escape::shared_variables` walks the AST first and answers it for the program.

**It over-approximates by name, on purpose.** A name assigned anywhere and mentioned inside any
function anywhere is shared, so `let n = 1; function f() { let n = 2; n = 3; }` gives both `n`s
a cell although neither is shared. Being too eager costs a cell and an indirection; being too
clever costs correctness, and that failure reads a stale value rather than stopping. Real scope
resolution is worth having later — guessing at it is not.

A cell is an ordinary one-property object, so this needed no new IR operation and no new runtime
call. That is slower than a dedicated representation and is the right first version: correct
now, and a measurement before inventing machinery to make it faster.

**Consequence:** a shared *parameter* has no cell to arrive in, because the caller passes a
plain value. The callee wraps it at entry, reading the argument before `make_cell` overwrites
the slot. Captures are the opposite and must **not** be wrapped: the value arriving is already
the enclosing scope's cell, and making a second one there would hand the closure a private copy
— which is the bug, reintroduced at the use site.

## D-98

**The emitted tables declare an alignment.**

Status: Accepted

Cranelift gives a data object no declared alignment unless asked, so `crisol_stack_maps` and
`crisol_functions` were emitted with alignment 1. The runtime reads both as arrays of 8-byte
values, and `slice::from_raw_parts` asserts alignment — **including for a zero-length slice**.

For every program written until now the symbol happened to land on an 8-byte boundary. Then one
did not: `let a = []; return a.length;` put the table at an odd address and the program aborted
before running a line. Nothing about the program was unusual; the array work merely changed the
data section enough to move it.

That is the worst shape a latent bug can have — correct by luck, and the thing that breaks it
is unrelated to the thing that is wrong. Both tables now set an alignment, and the runtime
asserts it on registration rather than relying on `from_raw_parts` to notice, so a future
regression names its cause.

## D-99

**A value that may hold a reference is in the stack map, not only the slots.**

Status: Accepted

Every *slot* was declared as needing a stack map. SSA values were not, and a temporary never
stored into a slot is invisible to the collector. `[{v: 1}]` is exactly that: the object is an
SSA value used directly as an element, so allocating the array collected it.

Under `CRISOL_GC_STRESS=1` every array of objects came back holding stale handles. Without
stress nothing failed at all — and three class tests that had been passing were also wrong,
because a method closure and a receiver are the same kind of temporary.

The slots are declared unconditionally because nothing there knows what they hold. Values are
declared by their IR type, which is a real narrowing rather than a guess: a `Number` or a `Bool`
cannot be a reference by the lattice's own statement, and `Unknown` means exactly that and is
included.

**Consequence, and the more important half:** the acceptance harness now builds each program
once and runs it **twice**, the second time collecting on every allocation, requiring the same
answer. Separate stress tests would not have caught this, because the programs that find these
bugs are not the ones that look like collector tests. `[{v: 1}]` does not look like a GC test.

## D-100

**A built-in roots its receiver and arguments; a compiled function does not have to.**

Status: Accepted

Arguments arrive in `argv`, a buffer in the *caller's* frame that no stack map describes. That
is safe for a compiled callee for a reason that is easy to state and easier to forget: its
prologue copies them into stack-mapped variables, and nothing allocates in between, so the
window where they are unreachable contains no collection.

A built-in never runs that prologue. It reads `argv` directly and then allocates — and in that
window its arguments are reachable from nowhere the collector looks.

`[1, 2].map(f)` called `f` **zero times** under `CRISOL_GC_STRESS`: allocating the result array
collected the callback, and the call landed on `crisol_not_a_function`, which returns
`undefined` rather than failing. The receiver needs rooting for the same reason — `a.map(…)`
leaves `a` dead at the call site, so the array being mapped is no better off than the callback.

**Rejected — making `argv` itself a root.** Cranelift's user stack maps describe *values*, not
explicit stack slots, so the buffer cannot be declared. Rooting at the boundary that actually
needs it also keeps the cost off compiled calls, which are the common case.

## D-101

**Built-ins are closures carrying a negative function index.**

Status: Accepted

`Array.prototype.map` has to be callable exactly as a compiled function is, or every call site
would need to know which kind it holds. So a built-in is an ordinary closure whose function
index is negative: non-negative indexes the compiled function table, negative indexes the
runtime's own list.

The sign rather than a reserved range or a second internal slot, because the two tables are
disjoint by construction and there is no boundary to pick wrongly.

`Array.prototype` is built before the first array and rooted through a `Cell` that the root
provider reads. A `Cell` rather than reaching through `with_runtime`, because the provider runs
*during* a collection, which may have been triggered inside a borrow of the shape table.

**Consequence:** `crisol_closure_code` is the single place that decides what a value is
callable as, so a native and a compiled callee reach the same call site by construction. It is
also where `crisol_not_a_function` comes from, which means a built-in that goes missing degrades
to `undefined` rather than a jump through a null pointer — and, as D-100 records, that made a
collector bug look like a callback that simply did nothing.

## D-102

**Function declarations are bound before any statement in their list runs.**

Status: Accepted

A function declaration is usable above its own text — `f(); function f() {}` is ordinary
JavaScript. The lowering bound the name where the declaration appeared, so a call above it read
an unset slot. That was recorded as unsupported rather than miscompiled, which was the right
call and made **every one of test262's 12,719 cases refuse to compile**: the suite's own
`assert.js` defines its helpers below the code that uses them.

Only the declarations at the top level of a statement list are hoisted. A function inside a
block belongs to that block's scope, which needs block scoping the lowering does not model, so
those stay where they are and are still refused — visibly, rather than bound in the wrong scope
and shadowing something.

## D-103

**`switch` is a chain of comparisons and a run of fall-through blocks, not nested `if`s.**

Status: Accepted

Two behaviours rule out the obvious lowering, and both are observable:

- **Cases fall through.** A body with no `break` continues into the next, so the bodies are a
  chain rather than arms of a conditional.
- **`default` is tested last but runs in its source position.** `switch (x) { default: a();
  case 1: b(); }` runs only `b()` when `x === 1`, and `a()` *then* `b()` otherwise. Putting
  `default` last is wrong for the second; treating it as a first-match arm is wrong for the
  first.

The discriminant is evaluated once into a temporary, so `switch (f())` does not call `f` per
case.

**Consequence: `===` on values of unknown type had to become a call.** Every case comparison is
one, and the backend refused them for D-53's reasons — `NaN` has identical bits to itself and
is not equal to itself, `+0` and `-0` differ in bits and are equal. `crisol_strict_equal`
compares as numbers when both are numbers, which gets both right because IEEE equality already
says exactly that, and falls back to identity otherwise. Strings will need revisiting: two
distinct string objects with the same characters are `===` and are not the same handle.

## D-104

**Exceptions propagate as an explicit value, and the propagation is written into the IR.**

Status: Accepted

M13's deliverable says to decide between unwinding and explicit result propagation and record
it. This is the record: **explicit propagation**.

A call returns `Value::EXCEPTION` — a reserved singleton that is not a JavaScript value —
instead of a result, and the thrown value waits in the runtime. Putting the signal in the value
space rather than a second return register means adding exceptions changed no function's
signature: a call site that ignores it compiles exactly as before.

**The propagation is ordinary control flow in the IR, not metadata a backend must honour.** The
frontend follows every call with `UnaryOp::IsException` and a branch — into the enclosing
`catch` if there is one, and out of the function otherwise. So the verifier checks it like any
other graph, a dump shows it, and the backend needs no notion of a handler at all. The
alternative — a handler recorded on the instruction for codegen to act on — puts the unwinding
somewhere nothing else looks.

`throw` is an *operation* followed by that same check, not a terminator of its own. A `throw`
inside a `try` has to reach the handler, and a second path to there is how one of them ends up
missing a case.

**Rejected — unwinding.** Zero cost on the path that does not throw, which is nearly all of
them. It needs different machinery per target (Windows differs from the rest), and it has to
coexist with the frame-pointer walk the collector now depends on. Explicit propagation costs a
compare and a branch after every call, which predicts perfectly, and is the same shape as
Rust's `Result`.

**Consequence:** `Value::kind` reports the signal as `Undefined` rather than giving it a kind of
its own. It should never reach a program; if a propagation is ever missed the value behaves as
`undefined` rather than aborting, because a wrong answer in a corner is recoverable and a crash
inside a half-unwound call is not. The value in flight is also a **GC root** — the frame that
made it has returned and no handler holds it yet, so without that, throwing an object and
catching it after any allocation would catch a freed one.

## D-105

**Strings are heap cells, and `===` on them compares characters.**

Status: Accepted

A `Value` carries 48 bits and text does not fit, so a string is a cell the collector owns —
like an object, though it has no properties and no shape that matters. A literal allocates a
fresh cell **on every evaluation**, which is correct because strings are primitives and `===`
compares characters, and wasteful because `"a"` in a loop allocates each time. Interning
constants is the obvious fix and wants a table that is a permanent GC root; correctness first.

`+` concatenates when *either* operand is a string, which is why the test is on the operands
rather than on both being numbers: `1 + "2"` is `"12"` and not `3`. An object operand still
gives `NaN`, because `ToPrimitive` calls user code.

`ToNumber` had been "the number, or `NaN`". It now follows the specification for the types that
exist: a string parses, **an empty or all-whitespace one is `0` rather than `NaN`** — the one
case a plain `parse` gets wrong, since Rust rejects an empty string — a boolean is `1` or `0`,
and `null` is `0` while `undefined` is `NaN`.

**Known wrong:** `.length` counts **bytes**, and JavaScript counts UTF-16 code units. It reads
correctly for every ASCII test, which is exactly why it is written down here.

## D-106

**An uncaught throw exits non-zero, and that is what made a pass rate possible.**

Status: Accepted

test262 reports failure **by throwing**. Once exceptions existed, 221 of 379 sampled cases
"ran to completion" — and reporting that as a pass rate would have claimed 58%, because a case
whose assertion fired threw, propagated out of `crisol_program`, and the entry point printed the
signal as `undefined` and exited successfully.

The entry point now checks for the signal, reports what was thrown, and exits 1. The runner
reads exit 1 as a *failed test* and anything else — a signal, a panic — as a bug here. The real
figures are **16 passed, 205 failed, 0 crashed, 158 refused**.

That the number fell from 221 to 16 is the whole argument for the harness having reported
three numbers separately from the start, rather than collapsing "it finished" into "it passed".

## D-107

**A branch on a value of unknown type is `ToBoolean`, not a bit comparison.**

Status: Accepted

The backend lowered every branch as "does this equal boxed `true`", with a comment saying that
was sound because the IR had typed the condition `Bool`. It had not: the frontend emits a
branch on whatever `if`, `while`, `&&` and `||` are given.

So **every truthy value that was not literally `true` took the false path** — a non-empty
string, a non-zero number, any object. The shape that exposed it is test262's own error class:

```js
this.message = message || "";
```

which assigned `""` whatever it was handed, so every failing case reported an error with no
message. The condition now goes through `crisol_truthy` unless the lattice has proved it a
boolean, in which case the comparison is still right and still free.

An empty string is falsy and every other string is truthy — the one case where a string's
characters decide a branch, and the reason `is_truthy` needs the text rather than the kind.

## D-108

**Every function has a `prototype` object, and its name is bound before its body is lowered.**

Status: Accepted

Two bugs with one shape: a function that refers to itself.

`new f()` links an instance to `f.prototype` and `x instanceof f` looks for it, and only the
*class* lowering was creating one. So a plain constructor function produced objects that its own
`instanceof` denied — and test262's error class guards on exactly that:

```js
if (!(this instanceof Test262Error)) return new Test262Error(message);
```

which recursed instead of initialising. Closures now get a `prototype` eagerly. That costs an
allocation per closure that most never use; creating it on first read would avoid it, at the
price of a property read that mutates the heap.

The second: `hoist` built the closure and *then* bound the name, so a recursive function
captured the slot's value from before it was bound — nothing. The binding and its cell are now
made first, and the closure written into them afterwards. A function declaration's own name
therefore counts as an assignment in the escape pass (D-97), because that is what gives it a
cell, and a cell is what a closure can share with the scope that fills it in.

**Consequence, and the reason the pass count fell from 16 to 13:** several cases had been
passing because their checks never fired. A test whose guard took the wrong branch, or whose
error carried no message, exits cleanly and scores as a pass. Making the semantics right makes
those cases fail correctly. The harness now prints the passing cases by name so a fall can be
read rather than trusted — and reading them shows they are tests of `Object.defineProperty`,
`Promise` and `RegExp`, none of which exist here. The 13 are mostly accidents too.

## D-109

**A name that resolves to no binding is a global, and a missing global is a `ReferenceError`.**

Status: Accepted

The lowering resolved an unknown name by *declaring a local for it*. So `Object` became a fresh
empty variable holding `undefined`, and a test comparing a builtin against an expected value saw
a wrong value rather than a missing one — 101 of test262's failures read
`Expected SameValue(«undefined», …)` for exactly that reason.

A name that resolves nowhere now becomes `Op::GlobalLoad`, looked up in an object the runtime
owns. Absent means **`ReferenceError`**, which is the specification and is also the difference
between "this builtin is wrong" and "this builtin does not exist" — the failures now say which.

Globals that are functions are built-ins numbered *after* the array methods in one negative
index space (D-101), so `crisol_closure_code` needs no second rule. `Error` and its subclasses
are one implementation: they differ only in `name`, which is read off the constructor rather
than hard-coded, so adding another is a line in a table.

**Consequence:** the pass count fell from 13 to 2. Those cases were passing because a missing
builtin read as `undefined` and their checks happened not to fire; they now throw, correctly.
The number is a truer 2 than it was a 13, and the harness lists passing cases by name (D-108)
so that can be read rather than taken on trust.

**Two bugs this shook out, both about when things are reachable.** The globals table is filled
while the runtime is constructed, and the runtime is constructed lazily on first use — so
reading the table before entering `with_runtime` found it empty whenever a global was the first
thing a program touched, which is usually. And `raise` built two strings and then stored them,
leaving the first reachable only from a Rust local while the second allocated; under stress that
collected it, and the error came back with an unreadable message.

## D-110

**`Object` and `Array` are objects that are also callable, built from one negative index space.**

Status: Accepted

A global like `Object` is a function *and* a namespace: `Object({})` constructs and
`Object.keys(o)` does not. Both are the same cell — a closure carrying a built-in index, with
the methods hung on it as ordinary properties.

The index space now spans four tables in order: the array methods, the named global functions,
the namespace methods, and the ones reachable only as a namespace's own body. One space because
`crisol_closure_code` decodes a single negative number; four tables because they are bound in
different ways. **The first attempt pointed the namespace body at index 0 of the first table, so
calling `Object()` ran `Array.prototype.map`** — which is what the separate table and the named
constant exist to prevent.

`Array.prototype` is the object arrays already inherit from rather than a fresh one, or
`[].map === Array.prototype.map` would be false.

`Object.keys` and `Object.getOwnPropertyNames` are the same function, which is wrong in general
— the second includes non-enumerable properties — and right here, because nothing can make a
property non-enumerable yet.

## D-111

**Hoisting declares every name before lowering any body.**

Status: Accepted

One pass over the statements lowered each function as it was reached, so a function could not
call one declared further down: the name resolved to nothing and became a global, failing at
run time with `ReferenceError`.

test262 concatenates `assert.js` ahead of the `sta.js` that defines the error class it throws,
so **24 sampled cases failed with `Test262Error is not defined`** — the suite's own class,
reported missing by a compiler that had just compiled it.

Declaring first also gives a function its cell before its body is lowered, which is what lets a
recursive function capture itself (D-108). The two are the same requirement seen from different
directions: a body must be lowered against the complete set of bindings, not a prefix of it.

**Consequence:** this is hoisting of *bindings*, not of assignments. A call before the
declaration still reads the slot's value at that moment, which is `undefined` until the
declaration runs — right for `var`, wrong for a function declaration, whose closure the
specification installs before any statement executes. That difference is now the only part of
hoisting still missing.

## D-112

**Property access raises `TypeError` on `null` and `undefined`, and every access checks.**

Status: Accepted

Reading a property of nothing answered `undefined`. That is not a shortcut with a small cost:
`x.y.z` on a missing `x` then fails *two lines later* carrying a value that looks like a
legitimate absence, so the failure names the wrong place. 33 of test262's sampled cases were
exactly this, reported as a wrong value rather than the error they expected.

All four accesses now raise — static and computed, read and write — which means the two store
helpers had to start returning a value for the caller to check. A store is no longer an
effect-only operation in the IR; it produces the exception signal or `undefined`, and the
frontend follows it with the same branch a call gets (D-104).

Calling a non-function raises too. That needed no new machinery at all: `crisol_not_a_function`
already existed as the fallback that kept a bad callee from jumping through a null pointer
(D-95), and raising there is a one-line change no call site knew about.

**Consequence: the sampled pass count fell from 12 to 6, and the failures got sharper.** 90 now
read `TypeError: is not a function` and 33 `cannot read a property of undefined` — a method
that does not exist, called. That is a truer description of what is missing than a comparison
against `undefined`, and it is the list of builtins to write.

The cost is a branch after every property access. The IR roughly doubles for property-heavy
code, which is the price of the unwinding being visible in the graph rather than implied — and
it is the shape a later pass can collapse once the IR can prove a receiver is an object.

## D-113

**Every function inherits from `Function.prototype`, so `call` and `apply` exist.**

Status: Accepted

A closure was an object with a `prototype` *property* and no `[[Prototype]]` *link*, so `f.call`
resolved to nothing. That is not a small gap: test262 reaches a method through `call` whenever
it wants to test what the method does to a receiver it was not written for —
`Array.prototype.indexOf.call(true)` is a whole family of cases, and every one of them failed
with `is not a function`.

`this` inside `call` is the **function**, not the receiver; the receiver is the first argument.
That inversion is the whole of what `call` does.

`Function.prototype` is built first, before the array prototype and the globals, because every
function made afterwards links to it — including the two that live on it.

**Consequence:** built-in functions are now made in exactly one place. Four builders had each
been allocating a cell, writing the index and defining properties in their own way, and only
one of them would have gained the prototype link. A function made here and one made by
`crisol_create_closure` now agree on what a function *is*: a cell whose internal zero says which
code it runs, inheriting from `Function.prototype`.

## D-114

**The array methods, and what their edge cases are for.**

Status: Accepted

Fourteen more, chosen by what test262 was calling. The ones worth recording are the pairs that
differ only at an edge, because a single implementation covering both is how the edge gets lost:

- **`includes` finds `NaN` and `indexOf` does not.** The first uses SameValueZero and the second
  `===`, so `[NaN].includes(NaN)` is `true` and `[NaN].indexOf(NaN)` is `-1`.
- **`find` answers `undefined` and `findIndex` answers `-1`** when nothing matches. They share an
  implementation, which is safe only because that difference is the parameter.
- **Empty is `true` for `every` and `false` for `some`.** Both stop on the opposite answer, and
  on an empty array neither stops — so the answer is whichever the loop falls through to.
- **`concat` spreads an array argument and appends anything else whole**, which is what makes
  `[1].concat([2, 3])` three elements and `[1].concat(2)` two.
- **A negative index counts from the end** and past either end clamps, so `slice(-1)` is the last
  element and `slice(5)` on a short array is empty rather than an error.
- **`null` and `undefined` join as empty**, not as their names.

`unshift` grows the array before moving anything, so no element is overwritten before it has
moved — the same reason `reverse` reads both ends before writing either.

## D-115

**A string is measured and indexed in UTF-16 code units.**

Status: Accepted, correcting D-105

`length` counted bytes. That reads correctly for every ASCII test and wrongly for everything
else, which is the worst way to be wrong — `"é".length` was 2 and `"😀".length` was 4.
JavaScript counts UTF-16 code units, so those are 1 and 2. Every index a string method takes or
returns is in the same space, or `indexOf` and `charAt` would disagree about where something is.

Strings now inherit from `String.prototype`, which is where the methods live, so they are
reached by the same prototype walk an object's methods are.

The pairs worth recording, again because one implementation covering both is how the difference
is lost:

- **`charAt` answers `""` out of range and `charCodeAt` answers `NaN`.**
- **`substring` clamps a negative index to zero and swaps its arguments if they are reversed;
  `slice` counts a negative index from the end and does not swap.** `"hello".substring(3, 1)` is
  `"el"` and `"hello".slice(3, 1)` is `""`.
- **An empty separator splits into characters, and no separator at all gives a one-element
  array** holding the whole string rather than an empty one.

`repeat` with a negative or infinite count raises a `RangeError` rather than answering with an
empty string, which would read like a legitimate result.

**Consequence:** the receiver is read with `to_text`, not a string-only accessor, because
`String.prototype.slice.call(5)` coerces — which is exactly how test262 reaches these methods.

## D-116

**Property attributes live on the object, not in the shape.**

Status: Accepted

A production engine puts them in the shape, so every object sharing it answers without a
lookup. This does not, for a reason specific to what attributes *are*: they do not affect
layout. Putting them in the shape would mean rebuilding the chain whenever `defineProperty`
changes an existing property's writability — rebuilding a description of where values live
because something that is not where values live has changed.

The cost is real and worth stating: attribute lookup is not shape-cached, so a hot property
access that had to consult them would pay per object. Nothing does yet, because only assignment
consults `writable` and only enumeration consults `enumerable`. An object nobody calls
`defineProperty` on carries an empty map and pays nothing.

**Assignment and `defineProperty` default to opposite ends.** `o.x = 1` creates a property that
is writable, enumerable and configurable; `Object.defineProperty(o, "x", {})` creates one that
is none of those. An implementation reusing the assignment default passes every test that does
not check the difference — which is most of them, and none of the ones that matter.

Three consequences, each of which was previously wrong:

- **`Object.keys` and `getOwnPropertyNames` are no longer the same function.** D-110 recorded
  that as wrong-in-general and right-then, because nothing could make a property
  non-enumerable. Something can now.
- **A write to a non-writable property is silently ignored**, not an error. That is sloppy mode,
  which is the only mode there is here.
- **`defineProperty` writes past a non-writable property** where assignment does not, because it
  redefines rather than assigns. Sharing the write path would make a property defined
  non-writable impossible to redefine.

An absent property's descriptor is `undefined`, which is how a caller distinguishes "not there"
from "there and not writable".

**Not done: accessors.** A descriptor with `get` or `set` is ignored rather than refused, which
is the one part of this that fails quietly. Recorded here rather than left to be found.

## D-117

**`delete` marks a tombstone rather than reshaping the object.**

Status: Accepted

Removing a property from a shape leaves an object whose layout no longer matches the chain
describing it. A production engine answers that by abandoning shapes for a dictionary. This
marks the slot instead: the shape still names it, and a tombstone says it is gone.

The cost is that the slot stays allocated and every read of a once-deleted property pays a
lookup. The benefit is one representation rather than two, and re-assigning a deleted property
revives it — the shape already names the slot, so clearing the tombstone is the whole operation.
The value is cleared when the tombstone is set, or the collector would keep whatever it pointed
at alive for as long as the object lived.

**`delete` asks whether the property is gone afterwards, not whether it removed anything.** A
property that was never there answers `true`. Only a non-configurable one answers `false`, and
it answers rather than throwing, which is sloppy mode — the only mode here. `delete` on
something that is not a property access is `true` and does nothing.

One operation covers `delete o.x` and `delete o[k]`: the frontend makes a string constant for
the static form rather than the IR carrying two shapes of the same question.

**This uncovered a bug older than itself.** `key_of` returned `None` for a **string** key,
because it was written before strings existed and never revisited. So `o["a"]` silently did
nothing — a computed read answered `undefined` and a computed write was discarded, neither
saying a word. Only a number key worked, which is why every array test passed over it.

**Consequence:** the two side tables are `Option<Box<…>>` rather than inline collections.
Clippy objects to boxing a collection and is right in general; here the point is the *inline*
size, which every cell in the heap pays for including the free ones — eight bytes against
forty-eight, twice. The allocation happens only for an object that has attributes or deletions.

## D-118

**`for-in` takes its list of names before the body runs, and built-in methods are not
enumerable.**

Status: Accepted

Lowered as an ordinary counted loop over a list computed once. The specification allows a
property deleted during the loop to be skipped and one added not to be visited, so taking the
list up front is within it — and it keeps the loop from depending on an enumeration order its
own body is changing.

**Inherited enumerable properties are visited**, which is what separates `for-in` from
`Object.keys`, so the enumeration walks the prototype chain. A name found on an object shadows
the same name further up and is visited once, at the first place it appears.

`for (let k in o)` declares `k`; `for (k in o)` assigns to whatever `k` already names. Treating
the second as a declaration would shadow the outer binding, so the loop would run correctly and
leave nothing behind — a failure with no symptom inside the loop at all.

**The test for it caught that every built-in method was enumerable.** `for (k in [])` visited
`map`, `filter` and the rest: the loop was right and the properties were wrong. Every method the
specification puts on a prototype is `{ writable: true, enumerable: false, configurable: true }`,
which descriptors (D-116) had just made expressible — the feature and the bug it exposed landed
one after the other.

**Consequence:** built-in methods are now defined through one function that sets those
attributes. Four builders had been defining them four ways; the one that mattered was the one
nobody had thought about, because nothing could observe enumerability until `for-in` existed.

## D-119

**Computed property keys and template literals.**

Status: Accepted

A computed key `{[k]: v}` and a numeric key `{1: v}` both go through the computed store rather
than a static name. The numeric case could have been converted to text in the frontend, but the
number-to-name rule already lives in the runtime — putting a second copy in the compiler is how
`{1: x}` and `o[1] = x` come to disagree about what the property is called.

**The key is evaluated before the value**, which is the order the specification gives and is
observable whenever either has an effect.

A template literal is lowered as concatenation, because that is what it is. **The first piece is
always a string** even when the template opens with a substitution: starting from the empty
string is the whole reason `` `${1}${2}` `` is `"12"` and not `3`. An empty trailing piece emits
nothing, so `` `${a}${b}` `` does not pay for two concatenations with `""`.

## D-120

**`for-of` covers arrays and strings, and is not the iterator protocol.**

Status: Accepted, and deliberately partial

There is no `Symbol`, so there is no `Symbol.iterator` to look up and a user-defined iterable
cannot be recognised at all. What this covers is an array or a string; anything else raises a
`TypeError` — the error the protocol would raise for a non-iterable, reached for a different
reason. Recorded as partial rather than presented as done, because the failure for a custom
iterable is indistinguishable from the failure for a number.

**An array is indexed live, not copied.** `length` is read in the loop header each step, so a
`push` inside the body is seen and `for (const x of a) a.push(x)` does not terminate — which is
what a real engine does. Copying the elements up front would have made it terminate, which is
the quieter answer and the wrong one.

**A string is walked by code point, not code unit**: `for (const c of "😀")` runs once where
`"😀".length` is 2 (D-115). The snapshot is indistinguishable from live indexing because a
string cannot change.

**Consequence:** `for-in` and `for-of` are one loop. They differ only in what produces the list —
`Enumerate` gives names, `Iterate` gives something indexable — and sharing the lowering is what
makes `break`, `continue` and the declare-versus-assign rule identical in both without being
written twice.

## D-121

**The test count in STATE.md is measured, not incremented.**

Status: Accepted, correcting several earlier entries

The recorded total had been carried forward by hand — 1146, then 1175, 1183, 1188, 1200, 1207 —
each step adding the tests a commit introduced to the previous line. The measured workspace
total is **1146**, and every figure above it was arithmetic on a number nobody re-read.

The same narrowing had turned the gate green while it was red. Running `cargo clippy` on the
packages a change touched, rather than `--workspace`, hid a lint failure in `cli/src/build.rs`
from the commit that introduced it (`3f182a4`) until now — six commits. A per-package gate is
not a smaller version of the workspace gate; it is a different gate that happens to agree most
of the time.

Both failures have the same shape: a number or a check that was true once, reused as though it
were still being taken.

## D-122

**`crisol-builtins` exists, is tested, and nothing ships it.**

Status: Recorded, partly acted on

The crate holds seventeen modules and **230 passing tests**: an object model with `Realm`,
descriptors, `Proxy`, `Reflect`, `Promise`, `RegExp` over `regress`, `Date`, `JSON`, `Map` and
`Set`, `Symbol`, the iterator protocol, and the conversion algorithms. No production code
depends on it. `crisol-abi` — the runtime compiled programs actually call — depends only on
`crisol-gc` and `crisol-value`, and reimplements a smaller version of the same ground.

So 230 of the workspace's passing tests cover code no compiled program can reach, and
`Object.defineProperty` was written twice: once in `descriptor.rs` with `validate_and_apply`,
and once again in D-116 by someone who had not looked.

The two halves are not interchangeable. `crisol-builtins` has its own object model —
`ObjectId` indexes a `Realm`, not the collector's heap — so adopting it wholesale means moving
compiled code off `crisol-gc`, which is not a refactor.

What *is* reachable is everything that does not touch `Realm`, which is most of it:
`regexp`, `date`, `json`, `convert`, `collections`, `string`, `symbol`, `array`, `error`,
`iterator`, `promise` and `proxy` are all free of it. Those are pure algorithms over Rust types
and can be called directly.

`RegExp` is the first one taken (D-123). `Date`, `JSON`, `Map` and `Set` are the same shape of
work and are the obvious next ones — each is currently "not defined" at runtime while sitting
finished and tested in the tree.

## D-123

**`RegExp` is `crisol-builtins::JsRegExp`, and `lastIndex` lives on the object.**

Status: Accepted

The pattern is compiled when the literal is **evaluated**, not at first use, so an invalid one
raises a `SyntaxError` where it is written rather than inside whatever later called `test`.

`JsRegExp` owns a cursor, and so does the JavaScript object — `lastIndex` is writable from a
program, so it cannot live only in Rust. The property is authoritative: the compiled pattern is
set from it before each use and read back after. Two owners of a value the program can change
would disagree the first time it changed one.

Compiled patterns are memoised by source and flags. That is a memo and not ownership, which is
what makes it safe to share one compiled pattern between two objects with the same literal.

**`exec` answers `null`, not `undefined`** — `while ((m = re.exec(s)) !== null)` is the idiom
that depends on it. Its result is an array carrying `index` and `input` as properties, and
**a group that did not participate is `undefined` rather than `""`**, which is the distinction
`Captured` keeps by storing `Option` per group.

**Consequence: a rooting bug, caught by the test that read the property back.** Both the source
and the flags strings were built before either was stored, leaving the first unrooted while the
second allocated — a collection in between freed a value the object was about to hold. It read
back as `[unreadable string]`. The fix is the rule the array methods already follow: create and
store one at a time. The bug is only reachable when a collection lands in that window, so the
test that caught it was the one asserting `.source` rather than any test of matching.

## D-124

**`JSON` is `crisol-builtins::json`, and the two containers disagree about absence.**

Status: Accepted

The second of the unshipped builtins taken (D-122). `Json` is owned data with no `Realm` in it,
so the work is two converters and nothing else.

**An object drops a property JSON cannot spell; an array cannot.** `JSON.stringify({a:
undefined})` is `"{}"` and `JSON.stringify([undefined])` is `"[null]"` — an array would have to
change its length to drop an element, so the same absence has to be written two ways. A single
"skip what you cannot spell" rule gets one of them wrong.

**`undefined` for a value JSON cannot spell at the top level**, not the string `"undefined"`.
**A non-finite number is `null`**, because JSON has no spelling for `NaN` or an infinity and
refusing the whole document over one would be worse. A structure containing itself raises,
rather than producing a document that silently stops describing the value.

Not done, and recorded rather than left to be found: **the reviver and replacer arguments are
ignored.** A program passing one gets the unchanged document, which is wrong quietly.

## D-125

**A constructor's `prototype` is the object its instances already inherit from.**

Status: Accepted

`Function`, `String` and `RegExp` were each missing that link, so `Function.prototype` existed
and could not be named. Fifty test262 cases failed on `Function is not defined` while the
object they wanted was built and rooted — the same failure as D-122, one level down: the thing
existed and nothing pointed at it.

Binding all four through one loop is what makes `[].map === Array.prototype.map` and
`"".trim === String.prototype.trim` true for the same reason rather than by coincidence.

**`Function` is bound so its prototype is reachable, not because `new Function(body)` works** —
that compiles source at runtime, which this engine does not do. Calling it raises rather than
answering something wrong.

**Consequence: `property_text` returned `Some("undefined")` for an absent property**, because
`to_text` spells every value out. A caller asking whether a property exists was told yes and
handed the word — which is how `new RegExp("ab+")` came to compile the pattern `undefined`. The
test that caught it was the plain one, `new RegExp("ab+").test("abb")`, not any of the ones
written for the interesting cases.

## D-126

**`Date` is `crisol-builtins::date`, and its time value lives in a hidden property.**

Status: Accepted

The third of the unshipped builtins taken (D-122). The calendar arithmetic was already written
and tested; what was missing was somewhere to keep the time value and a prototype to hang the
readers on.

**Internal slot zero already means "callable"** — [`is_callable`] reads it, and that is what
makes `typeof f` answer `"function"`. A date borrowing it would become a function. So the time
value is a property that enumeration does not see and `delete` cannot remove, which descriptors
(D-116) made expressible. It is still readable by name, which a real internal slot would not
be; that gap is the price of not having internal slots and is written down rather than hidden.

**`getMonth` is 0-based and `getDate` is 1-based.** They disagree deliberately, and a single
field reader parameterised on the wrong thing would get one of them wrong silently.

**The local-time methods are the UTC ones.** There is no timezone database here, so `getHours`
and `getUTCHours` are the same function — correct exactly where the offset is zero and wrong by
the offset everywhere else. `getTimezoneOffset` answers `0` for the same reason, which at least
makes the three consistent with each other rather than consistently wrong in different
directions.

**An invalid date raises from `toISOString` and prints from `toString`.** The first has no
spelling for one; the second has `"Invalid Date"`. The field readers answer `NaN`. Three
different right answers to the same broken input.

`new Date()` reads the clock through `Date.now`, so there is one clock rather than two that
could drift apart.

## D-127

**A built-in knows its own name, and naming it caught the rooting rule again.**

Status: Accepted

`Array.prototype.forEach.name` is `"forEach"`, and test262 checks it for every built-in it
covers. The name was sitting in the table that created each function and had simply never been
written onto it. It is not writable but is configurable — so `f.name = "x"` silently does
nothing while `Object.defineProperty(f, "name", …)` works.

**The change broke nearly every test, and only under GC stress.** `native_function` hands back
an unrooted handle; the function becomes reachable when it is stored on the prototype.
Allocating the name string *before* storing it left a window where a collection freed the
function being named. The symptom was a method that was `undefined` under stress and fine
without it.

This is the third time this session the same rule has been the bug: **create, store, then
allocate again** — `new RegExp` (D-123), `flatMap` below, and here. The rule is not "root
carefully"; it is that a value between allocation and its first store is invisible, and every
allocation in that gap is a chance to lose it.

`flatMap` had it in a different shape: results accumulated in a Rust `Vec` while the callback
allocated, so every result but the newest was unreachable. Fixed by mapping into a rooted array
first and flattening afterwards, when no JavaScript runs and nothing can move.

## D-128

**More of `Array.prototype` and `String.prototype`, and the pairs that differ.**

Status: Accepted

- **`reduceRight` is not `reduce` over a reversed list.** The callback still receives each
  element's real index, so reversing first would hand it the wrong ones — a wrong answer rather
  than a slower one, for any callback that reads the index.
- **`flat` goes one level by default**, not all of them; `flatMap` goes exactly one, always,
  because it takes no depth.
- **`at` counts a negative index from the end and answers `undefined` out of range**, where
  `charAt` answers `""`. The two differ at exactly the place a caller conflates them.
- **`replaceAll` with a non-global pattern is a `TypeError`**, not a quiet `replace`. The
  pattern's own flags decide how many matches are replaced, so a method name that disagreed
  with them would have to pick one to ignore.
- **An empty pad filler pads nothing.** Answering the original rather than looping forever is
  the whole reason that case is written down.

**Consequence: a test expectation that could never pass.** `execute` trims the program's
output, so `check(…, "  a")` compares against `"a"` however correct the code is. The test was
wrong and `padStart` was right. Expectations now carry a sentinel where leading or trailing
space is the point.

## D-129

**`Function.prototype.bind`, and `Object.prototype` existing at all.**

Status: Accepted

test262's own `propertyHelper.js` opens with
`Function.prototype.call.bind(Object.prototype.hasOwnProperty)`. Neither `bind` nor
`Object.prototype` existed, so the helper threw while loading and **every test that includes it
failed** — whatever the test was about. That is why one commit moved `is not a function` from
121 to 51.

**A native can see its own object.** The calling convention passes the callee as the first
operand, which is what lets a bound function find its target without the engine having closures
a native could capture. The target, receiver and leading arguments are hidden properties, for
the same reason a date's time value is (D-126): internal slot zero already means "callable",
and a bound function is exactly a callable.

**The bound arguments come first and the call's own follow**, which is what makes
`f.bind(null, 1)(2)` the same as `f(1, 2)`.

`Object.prototype` is now the end of every chain — plain objects, and the other prototypes too,
so `[].hasOwnProperty` and `({}).hasOwnProperty` are one function rather than a copy each.
**Own means own**: `hasOwnProperty` answers `false` for something found on the prototype, which
is the whole reason it exists rather than `in`.

**Consequence: an ordering bug that hid one property deep.** The object at the end of every
chain has to exist before anything links to it, so it was built first — but its methods are
*functions*, and functions made before `Function.prototype` exists do not get `call`. So
`Object.prototype.toString` was fine and `Object.prototype.toString.call` was not. Allocating
the object early and populating it after `Function.prototype` is the fix, and the split is now
the documented point of having two functions.

## D-130

**An identity test between two absent things passes.**

Status: Accepted

`({}).hasOwnProperty === Object.prototype.hasOwnProperty` was green while **both sides were
`undefined`**. It went green the moment it was written and stayed green through the bug it was
supposed to catch.

Every identity assertion in the acceptance suite is now preceded by a `typeof` check that there
is something to identify. The pattern generalises past this case: an assertion whose two sides
can both be missing is not testing what it appears to test, and `===` on `undefined` is the
commonest way to get one.

## D-131

**`==`, `!=` and `in`. `===` was never the gap.**

Status: Accepted

`===` and `!==` have been lowered since the comparison operators landed, including the part that
is easy to get wrong: **strings compare by their characters, not by identity**, so `"a" === "a"`
holds however many separate cells the two came from. What the refusal list called "binary
operator ==" was *loose* equality.

`==` is a [`BinaryOp`] rather than a [`CompareOp`] on purpose. Every `CompareOp` has a machine
instruction behind it when both sides are numbers; `==` never does, because deciding what to
compare means reading both types first. Keeping it out of the comparison lattice keeps that
lattice honest about which comparisons can become an `fcmp`.

The rule is short and the consequences are not:

- **`null` and `undefined` equal each other and nothing else** — not `0`, not `""`, not `false`.
  Checked before any coercion, or `null == 0` would become `0 == 0`.
- **A boolean becomes a number first**, on whichever side it is.
- **A string meeting a number becomes a number**, never the reverse.
- **An object becomes a primitive** through `valueOf` then `toString`.
- Same type defers to `===`, so everything already right about `NaN` and the two zeroes is
  inherited rather than restated.

**Not transitive**, and the example is in the tests: `"" == 0` and `"0" == 0` are both true
while `"" == "0"` is false.

A round limit stops the recursion, because a `valueOf` returning another object would otherwise
spin. The specification throws there; throwing from inside `==` would need an exception path the
operator does not have, and that difference is recorded rather than papered over.

**Only `in` can raise**, so only `in` pays for an exception check at the call site. `instanceof`
and `+` answer for every input, and the rest coerce with `ToNumber`, which has no failing case
over this engine's values.

## D-132

**A binary operator's result type is listed, not negated.**

Status: Accepted, fixing a bug older than this change

`is_always_numeric` was `!matches!(self, Self::Add)` — "everything except `+`". That is true of
the arithmetic and also of **`instanceof`**, which answers a boolean and has therefore been
typed `number` in the IR since the day it was added. Adding `==`, `!=` and `in` would have
inherited the same wrong answer, which is how the negation compounds.

The predicate now lists the numeric operators, and a companion lists the boolean ones, so a new
operator has to be classified rather than inheriting the answer for arithmetic.

**The corpus snapshot is what caught it.** The dump prints each value's type, so `v4: number =
== v2, v3` was visible in the diff the test asked to be reviewed. No assertion anywhere
mentioned operator result types; the snapshot showed one and made it obvious.

## D-133

**Array spread, separated from array holes.**

Status: Accepted; holes remain refused

The two shared one note, `array hole or spread`, so `[...a]` looked like a gap it had not been
for any good reason — they are unrelated problems that happened to arrive at the same match arm.

**A hole is still refused**, and D-64's reasoning is unchanged: a hole is not `undefined`, the
IR has no way to say "absent", and filling one in with `undefined` produces a value that reads
the same and answers `in` differently. That is a wrong answer, not a missing feature.

Spread has no such obstacle. **The leading run is built in one `CreateArray` and only what
follows a spread is appended piece by piece**, so an array with no spread costs exactly what it
did before.

`spread` is one bit on one operation because that is the whole difference at the call site:
with it, every element of the operand is appended; without it, the operand itself is. `[...a]`
and `[a]` differ in that and nothing else.

Spreading uses the same rule `for-of` does (D-120) — an array or a string, `TypeError`
otherwise — so `[...5]` and `for (x of 5)` fail the same way. A separate rule here would have
let one of them quietly produce a one-element array.

**The corpus snapshot is the review**, and this is what it is for: the dump shows the leading
`array [v2]`, the `extend v3, ...v4` with its exception branch, and the trailing `extend v3,
v7`. Reading that is how the lowering was checked, not by trusting that it compiled.

## D-134

**`Math`, and the three places its functions are not their Rust namesakes.**

Status: Accepted

Forty-nine test262 failures were `Math is not defined`. The functions are pure `f64` work with
nothing to root, so most of them are a one-line macro. The three that are not are the whole
value of writing this down:

- **`Math.round` is not `f64::round`.** JavaScript rounds a half *upward*, toward positive
  infinity; Rust rounds it *away from zero*. They agree on `0.5` and disagree on `-0.5`, which
  is `-0` in JavaScript and `-1` in Rust. `floor(x + 0.5)` is the rule, with the non-finite
  cases passed through because adding to an infinity would not survive it.
- **`Math.sign` is not `f64::signum`**, which answers `1.0` for a zero and never `NaN`.
  JavaScript gives back `0`, `-0` and `NaN` as themselves.
- **`Math.min`/`max` are not `f64::min`/`max`**, which return the *other* operand when one is
  `NaN`. In JavaScript one `NaN` anywhere wins. With no arguments each returns the opposite
  infinity, because each has to lose to the first real argument.

Each of those would have passed a casual reading and failed a specific test, which is why each
has one.

**`Math.random` is a xorshift generator seeded from the clock, and is not suitable for anything
needing unpredictability.** The specification asks only for an implementation-dependent value
in `[0, 1)`, which is what it delivers — said plainly because the name reads like a guarantee
it is not making.

The constants are defined separately from the functions: they are properties, so they have no
table entry, and the object they hang on exists only because naming a method created it.

## D-135

**`arguments` is bound lazily, and is an array.**

Status: Accepted, with two differences from the specification recorded

The slot is created the first time a body names `arguments`, not when the function is lowered.
That is not an optimisation — the first attempt declared it in every function that binds `this`,
which **shifted every parameter down by one slot** and broke closures. It broke them only under
GC stress, because the damage was to the frame the collector reads rather than to any value a
test printed. Binding lazily means a function that never mentions `arguments` has exactly the
numbering it had before the feature existed.

**An arrow inherits the enclosing function's `arguments`**, which is the rule `this` follows and
falls out of the lookup: arrows are absent from the stack of functions that bind it, so
resolution walks past them and the ordinary capture machinery does the rest. Not a special case.

Two differences from the specification, both of which read as correct until something looks
straight at them:

- **It is an array, not an array-*like*.** Everything array-shaped works at once — `length`,
  indexing, `for-of`, spread — and `Array.isArray(arguments)` answers `true` where a real engine
  says `false`.
- **It is a copy, so it does not alias the named parameters.** Outside strict mode a real
  engine makes `arguments[0] = 1` change `a`. Here it does not, and there is a test asserting
  the difference rather than a comment hoping nobody notices.

**Consequence: the fifth rooting bug of this shape.** `crisol_create_arguments` runs in the
callee's prologue, before any of its slots exist, so the only thing describing the argument
values is the caller's frame — and the array's own allocation could collect one it was about to
hold. The rule has not changed since D-127: a value between allocation and its first store is
invisible, and every allocation in that gap is a chance to lose it.

## D-136

**`var` is hoisted, which is what made test262's own helpers work.**

Status: Accepted, fixing a bug with a large blast radius

`var` and `let` were lowered identically — declared where they appear. A `var` is
**function-scoped**, so its name exists from the top of the function whatever line declares it,
and function declarations are hoisted *above* it. So a hoisted function could not see a `var`
declared below it: the name resolved to nothing and became a global load.

test262's `propertyHelper.js` is exactly that shape — `var __getOwnPropertyDescriptor = …` at
the top of the file, read by `verifyProperty`, a hoisted function lowered before the assignment
was reached. Eighteen cases failed with `__getOwnPropertyDescriptor is not defined`, and the
variable was right there in the same file.

The hoist pass already had the insight it needed — *"every name first, then every body"* — and
simply did not include `var`. Now it collects them through blocks, loops, `try` and `switch`,
but **not into nested functions**, because a `var` belongs to the nearest enclosing *function*
and hoisting one out of a nested function would bind it in the wrong scope.

**Hoisted means declared, not assigned.** Reading before the declaring statement gives
`undefined`; `var x;` after `x = 1` must not reset it, which is why a declarator with no
initialiser does nothing at its own site.

**Consequence: a `var` initialiser counts as an assignment in the escape analysis.** The binding
already exists by then, so the declaration is a write — and without that, a function declared
above it captured the slot's value when the closure was made, which is the `undefined` the hoist
had just put there. The function was permanently blind to the value assigned a line later.

## D-137

**Three operators were answering confidently and wrongly.**

Status: Accepted

Each was found by a test written for something else, which is the argument for writing the
obvious assertions down even when the feature looks finished.

- **`typeof` threw on an undeclared name.** The lowering's own comment said it is "the only
  operator that does not throw on an undeclared identifier" — and then sent the operand through
  the ordinary global load, which raises. `typeof nothingHere` was a `ReferenceError` instead of
  `"undefined"`. It now reads the global through a variant that answers `undefined`, and clears
  the pending throw so the next `catch` is not handed an exception nobody raised.
- **`<`, `<=`, `>`, `>=` coerced both sides to `f64` unconditionally**, so every string
  comparison was a `NaN` comparison — **false in both directions**. `"a" < "b"` and `"b" < "a"`
  were both false, so a sort comparator written the ordinary way answered `0` for every pair and
  sorted nothing. Two strings now compare lexicographically and everything else numerically,
  with the `fcmp` fast path kept for operands the lattice already knows are numbers.
- **`to_text` read `[object Object]` off every object without asking it.** `String([1, 2])` was
  that string rather than `"1,2"`; the array had a perfectly good `toString` that nothing
  called. Objects are now asked, `toString` first — the mirror of the `valueOf`-first order
  `==` uses, and **the order is the whole difference**: a string context asks for text first and
  a numeric one asks for a number first.

## D-138

**`sort` and `splice`.**

Status: Accepted

**The default sort order is by text, not by number.** `[10, 9].sort()` is `[10, 9]` because
`"10"` sorts before `"9"`. **`undefined` sorts to the end and never reaches the comparator**,
which is why it is partitioned out rather than compared.

The sort is a hand-rolled merge rather than `sort_by`, because Rust's sort may **panic** when
the comparison is not a total order — and a JavaScript comparator is arbitrary user code, so
`sort(() => 1)` is legal and inconsistent. A panic in a runtime helper is not recoverable; a
strange permutation is. It is stable, as the specification has required since ES2019.

**`splice` answers the removed elements and mutates in place**, the pair of jobs that makes it
the odd one out among the array methods. **No second argument removes everything from `start`
on, and a second argument of `0` removes nothing** — different behaviours, so the argument
*count* decides rather than the value.

## D-139

**test262 gets its own CI job, because its cost grows as the engine improves.**

Status: Accepted

A refused case costs milliseconds; a compiled one costs a `cc` invocation and a link. So the
suite has got slower every time something stopped being refused — the run is now long enough
that it cannot share a job with checks that should finish quickly, and long enough that local
runs were being lost to timeouts before they reported anything.

The split is: a **smoke sample of 40** in the matrix job, which catches an outright break, and a
**dedicated job running the whole corpus** with three hours and a runner to itself. The full
numbers and the reason breakdown go to the job summary, and the log is kept as an artifact —
a pass rate says how much, and the reasons say what to do next, so both are published rather
than left in a log nobody opens.

`CRISOL_TEST262_SAMPLE` exists for iterating locally. The default is left alone for anything
reported, because **two sample sizes are two different measurements** and comparing them says
nothing.

## D-140

**CI had been red for two days and the local gate said green.**

Status: Accepted, and the gap is the point

Every one of the last twenty-five runs failed, across every commit of this session. The cause
was one line: **`cargo test` does not build a `staticlib`.** The test262 runner links compiled
programs against `libcrisol_abi.a`, which only `cargo build -p crisol-abi` emits — so on CI the
suite asserted its absence, every time, since `CRISOL_REQUIRE_TEST262` was added.

It passed locally because a developer runs `cargo build -p crisol-abi` out of habit before
testing. That habit *is* the difference between the two environments, and it is exactly what a
CI job exists to catch — so the job caught it, and nobody read the result.

D-121 recorded the same shape once already: a per-package clippy run standing in for the
workspace one. This is worse, because the substitute was not even the same machine. **Running
the five steps locally is not "running CI"**; it is running a similar thing on a host that has
been configured by everything done on it since.

**Consequence, found immediately once the corpus actually ran: compiled programs have never
linked on Linux.** A Rust `staticlib` leaves the allocator, threads and `dlopen` to the final
link; macOS's driver supplies them implicitly and GNU ld does not, so `-lpthread -ldl -lm` have
to be named. All 12,213 cases failed at the link step.

And the failure reported as the single word `link`, with the message discarded — twelve
thousand cases under one row that said nothing about why, when the message named the missing
library outright. That is the third time a diagnostic has hidden its own cause (D-113, D-129);
the reason string now carries the linker's last line.

## D-141

**One CI run per ref, and the corpus only when somebody will read it.**

Status: Accepted

The corpus job failed with `exit code 143` and *"the runner has received a shutdown signal"* —
which says nothing about the code, because it is not about the code. A push to a branch with an
open pull request fires the `pull_request` workflow, and a manual dispatch fires another; each
spawns the whole matrix, so sixteen concurrent jobs were competing and the runners were
reclaimed. **Both** runs died within seconds of each other, which is what distinguishes this
from one cancelling the other.

Two changes. A **concurrency group** keyed on the ref, so a newer run supersedes an older one
cleanly instead of racing it. And the full corpus now runs **only on `main` or on request** —
the matrix job already carries a sample of 40 as a smoke check, and a compile-and-link per case
across twelve thousand cases is not something to spend on every commit when nobody is going to
read the result.

The general point, which cost three runs to learn: **a red CI job is not necessarily a claim
about the code**. Two of the three failures this session were the environment — a missing
`staticlib`, then a reclaimed runner — and reading the error rather than assuming a regression
is what separated them.

## D-142

**The collector is wrong on Linux x86-64, and nothing could see it until the linker worked.**

Status: Open — characterised, not fixed

With `-lpthread -ldl -lm` supplied (D-140), compiled programs link on Linux for the first time
and the acceptance suite ran there for the first time. **103 of 238 cases fail, and every one
of them fails only under GC stress.** Without stress, all 238 pass. macOS arm64 passes all 238
in both modes.

That is a precise fault, not a vague one: the engine is correct on Linux and **the collector is
not**. The symptom is a live value read back as `undefined` — `let outer = function (n) { let
bump = function () { n = n + 1; }; bump(); return n; }` answers `undefined` instead of `6` when
a collection lands at every allocation.

The frame walk itself looks right for both targets and was checked rather than assumed: on
aarch64 `stp x29, x30, [sp, #-16]!` and on x86-64 `call` plus `push rbp` both leave the saved
frame pointer at `[fp]` and the return address at `[fp + 8]`, so `fp + 16` is the caller's stack
pointer at the call on each. The divergence is somewhere past that — most likely what Cranelift
reports stack map slots *relative to* on x86-64.

Not fixed here, because bisecting it needs a Linux host to run against and guessing at a
collector is how a subtle bug becomes a silent one. Recorded as the top open issue instead.

**The reason it hid for so long is the interesting part.** Three separate things had to be
wrong at once for this to stay invisible: the archive was never built in CI, so the suite
asserted its absence rather than running; the link would have failed anyway for want of three
libraries; and the failure was reported as the word `link` with the message discarded. Each of
those looked like a small infrastructure annoyance. Together they hid a real defect in the one
subsystem the project's own notes call the hardest to test.

## D-143

**The Rust frames had no frame pointers on Linux, and macOS hid it.**

Status: Accepted — the cause behind D-142

The bisect named the variable. A `Linux (arm64)` job was added for exactly this: the two known
points, `macOS (arm64)` passing and `Linux (x86_64)` failing, differ in *two* things at once.
Holding the architecture fixed against macOS and the system fixed against x86-64 leaves one
answer, and it came back **Linux (arm64): 64 failed, every one under GC stress, none without** —
the same signature as x86-64 (103 failed). So the operating system is the variable and the
architecture is not.

The cause: `preserve_frame_pointers` was set for **Cranelift's output only**. The collector
walks the stack by following frame pointers, and the frames between the allocation that
triggers a collection and the JavaScript frame whose roots it is looking for —
`crisol_create_object` → `Heap::alloc` → `collect` → `walk_frames` — are ordinary Rust.
**rustc omits the frame pointer on Linux by default.** The walk lost the chain before reaching
any compiled JavaScript, found none of its roots, and freed values that were live.

**macOS hid this for the life of the project**, and not by luck that anyone chose: Apple's ABI
*mandates* the frame pointer on arm64, so rustc emits it there whatever the flags say. Every
local run, and the only passing CI job, was on the one platform where the bug cannot appear.

Fixed with `-C force-frame-pointers=yes` in `.cargo/config.toml`, workspace-wide — the chain
runs through several crates and a gap anywhere in it breaks the walk at that point.

**One limit, stated rather than discovered later:** this does not cover `std`, which ships
precompiled without frame pointers. If the chain ever runs through a `std` frame the walk will
break there too, and no rustflag in this repository changes that — it would need `-Z build-std`
or a walk that does not depend on frame pointers at all. CI is what says whether the chain as
it stands is clear.

## D-144

**Frame pointers fixed x86-64 Linux. arm64 Linux has a second fault.**

Status: Open — separated, not solved

`-C force-frame-pointers=yes` (D-143) took `Linux (x86_64)` from **103 failures to zero**. That
diagnosis was right and is verified.

It changed **nothing** on `Linux (arm64)`: 64 failed before and 64 after, the same 52 assertions
under GC stress, and the failing test list is byte-identical. Both jobs recompiled — 503 and 409
compile lines — so the flag was applied and simply had no effect there, which is what you would
expect if rustc already emits frame pointers for `aarch64-unknown-linux-gnu`. So the two Linux
failures were never one fault wearing two hats.

What is left is specific to **aarch64 Linux** and not to aarch64: macOS arm64 passes all 238 in
both modes, on the same architecture.

**A hypothesis, held as one.** The codegen sets no calling convention, so Cranelift takes it
from the triple — `AppleAarch64` on macOS and `SystemV` on Linux, two conventions on one
architecture. If the two differ in what a stack map slot's offset is measured *from*, the walk
would read the right frames and the wrong slots, which matches the symptom: live values read
back as `undefined`, but only when a collection actually happens. That is a guess with a
mechanism, not a finding, and it is written down as such — it has not been tested, because
testing it needs an aarch64 Linux host and the last attempt at one filled this machine's disk.

**The job stays, and stays non-blocking.** Deleting it would hide a real fault on a real target;
leaving it blocking would make every run red for something already understood and recorded.
`continue-on-error` reports it without failing the build, and the line comes out the day it
goes green.

## D-145

**Teaching `to_text` to ask objects broke every uncaught error message.**

Status: Accepted — a regression I introduced, found by the first corpus run

`crisol_report_uncaught` was `to_text(thrown).or_else(describe_error)`. That ordering was
correct only while `to_text` **failed** on objects: an error fell through to `describe_error`,
which reads `name` and `message`, and printed `TypeError: …`.

D-137 made `to_text` ask an object for its text. From then on it *succeeded* — with whatever
`Object.prototype.toString` returns — so `describe_error` never ran and every uncaught error
described itself as **`[object Object]`**. In the first full-corpus run that was **581 of 1061
failures**: more than half the suite's output was a string that says nothing.

`describe_error` now runs first. The order is not arbitrary: `name` and `message` are what an
error *carries*, and reading them beats calling a `toString` that most errors inherit rather
than define.

Two things worth keeping from how this was found:

- **A fallback chain encodes an assumption about the first branch failing.** Making the first
  branch more capable silently disabled the second. Nothing about D-137 looked like it touched
  error reporting.
- **The corpus is a diagnostic instrument, not only a score.** The pass count barely moved when
  this broke; what moved was 581 failures collapsing into one indistinguishable bucket. Reading
  the *reasons* is what caught it, and the reason it was worth reading is that they had been
  informative before.

## D-146

**A `String` wrapper carries its own text, because asking it recursed.**

Status: Accepted — the twelve crashes

The corpus run reported **12 crashed**, and the runner's assertion that crashes must be zero is
what turned that into a failing job. It is the right assertion: a case that built and then died
is a bug here, not a missing feature.

All twelve were `new String(…)`. The wrapper is an ordinary object, so a method reached through
it called `this_text`, which called `to_text`, which — since D-137 taught it to ask an object —
called `String.prototype.toString`, which called `this_text`. `new String("x").slice(0, 1)`
overflowed the stack instead of answering.

Two fixes, and both are about a wrapper being a thing in its own right rather than a view:

- **It stores the text it wraps**, in a hidden property, and `this_text` *reads* an object
  receiver rather than asking it. Asking is what recursed.
- **It stores its own `length`.** On a primitive that is answered by the property load, which
  has a string cell to measure; on a wrapper nothing would find it. Fixed at construction,
  because the text cannot change.

**This is the third consequence of D-137** — after `String([1,2])` being fixed and uncaught
errors being broken (D-145). Teaching one function to ask objects for text reached further than
it looked: into error reporting, and into anything whose `toString` leads back to the asker.

## D-147

**The summary step lost its output exactly when there was something to say.**

Status: Accepted

`sed … | head -60` under `set -euo pipefail`: `head` closes the pipe once it has its lines,
`sed` takes SIGPIPE, and `pipefail` turns that into a failed step. So the job summary — the
counts and the reason breakdown — was published only when the run was short enough not to need
truncating.

## D-148

**`Map` and `Set` keep their contents in the heap, not in `crisol-builtins`.**

Status: Accepted — a deliberate departure from D-122

`crisol-builtins` has a tested `JsMap` and `JsSet` and they are **not** used, which needs saying
because D-122 argues for taking what is already written. They hold `Value`s in a Rust
collection, and a `Value` can be a reference the collector must trace. Using them would mean a
registry of live maps in the root set — and that registry would keep the contents of **dead**
maps alive too, because nothing tells it when a wrapper is collected. A leak is not a better
trade than a rewrite.

A backing array inside the heap is traced already, for free and without a leak. One array with
key and value adjacent, rather than two, so the pair can never disagree about length.

The cost is honest: lookup is a **scan** where a `HashMap` is not. Correct and linear beats fast
and leaking, and the day it matters the fix is a real hash table *in the heap*, not a Rust one
beside it.

Three behaviours worth pinning, each of which looks like a mistake until you know the rule:

- **SameValueZero, so `NaN` is its own key.** `===` says `NaN !== NaN`, and without the
  difference every `add(NaN)` would add another. `+0` and `-0` are one key.
- **Re-setting a key keeps its position.** Insertion order is observable through `forEach`, and
  a reassignment is not a reinsertion.
- **`Map.forEach` passes value *then* key** — the opposite of storage order — and
  **`Set.forEach` passes the value twice**, so a callback written for a map works on a set.

`size` is a plain property maintained on every mutation, because the specification makes it an
accessor and there are no accessors here. Iterators (`keys`, `values`, `entries`) are absent for
the same reason `for-of` is partial (D-120): there is no `Symbol.iterator` to hang them on.

## D-149

**A symbol is a heap cell, because `as_address` does not check the tag.**

Status: Accepted

Symbols are not objects and hold nothing a program can add, so the obvious representation is a
tagged payload with no allocation behind it. That would have been a live hazard:
`Value::as_address` answers for `TAG_SYMBOL` exactly as for an object, and the root walk is
`value.as_address().map(GcRef::from_address)` with **no kind check**. A bare payload would have
been traced as though it addressed a cell, on the first collection after any symbol reached a
frame.

Allocating a real cell costs one allocation per symbol and makes that impossible rather than
avoided by convention — every existing path that turns a value into a reference stays correct
without knowing symbols exist. **Identity then falls out**: two `Symbol("x")` differ because two
cells differ, rather than because anything arranged it.

**`Symbol.for`'s registry is rooted and immortal, and that is correct.** A registered symbol must
come back for the same key however long later. That is the opposite verdict to `Map` (D-148),
where an immortal registry would have been a leak — the same mechanism, judged by what the
specification asks the lifetime to be rather than by what is convenient.

`Symbol.keyFor` answers `undefined` for `Symbol("x")` even though its description is `"x"`:
**the description is not the key**, and only registration makes one.

**Not usable as property keys**, which is the limit worth stating: a `PropertyKey` is a string
wrapper, so `obj[Symbol.iterator]` cannot name a symbol. The well-known symbols exist as values
so that reading `Symbol.iterator` yields a symbol rather than `undefined` — what most feature
tests check — and so the values are already the right ones when keys learn about symbols. Until
then this is also what blocks `Map`/`Set` iterators and user-defined iterables (D-120, D-148).

## D-150

**The rest of `Array.prototype`, and two gaps that are symbol-shaped.**

Status: Accepted

`toReversed`, `toSorted`, `toSpliced` and `with` — **the copying counterparts leave the original
alone**, which is the whole reason they exist beside `reverse`, `sort` and `splice`. `toSorted`
routes through the same sort the in-place one uses, so the two cannot drift apart on the default
text ordering or on where `undefined` lands.

**`with` raises out of range where `at` answers `undefined`**: it builds an array, and there is
no array to build for an index that does not exist.

**`copyWithin` never changes the length** — a run copied past the end is truncated, not grown —
and it reads the source run before writing, because source and destination can overlap and
copying forwards in place would read values it had already overwritten.

Two gaps, both the same shape and both waiting on symbols as property keys (D-149):

- **`Array.from` cannot take a user-defined iterable.** The specification asks for an iterator
  first and falls back to `length`; with no `Symbol.iterator` to look up there is nothing to
  ask. An array, a string or any array-like works; something merely *iterable* answers empty.
- **`keys`, `values` and `entries` return objects with `next`, which `for-of` cannot find.**
  Calling them directly works, which is what most of test262's coverage of these methods does.

`{value, done}` every time, and **`done` stays `true` once reached** — an exhausted iterator does
not restart, which is what lets a caller loop on `done` without counting.

## D-151

**`Object.freeze` needed somewhere to keep "no new properties".**

Status: Accepted

Descriptors (D-116) already say what a *property* permits. Extensibility is a fact about the
**object**, and there was nowhere to put it — so it is a hidden property, for the same reason a
date's time value is one (D-126): internal slots do not exist and `Object.keys` must not see it.
Absent means extensible, so an object nobody has frozen carries nothing.

A non-extensible object refuses a property it does not already have, **silently** — the same
rule a non-writable property follows outside strict mode, and the reason `Object.freeze` is
worth anything at all.

**Freezing keeps enumerability.** A frozen object still lists its properties; it is the writing
and the deleting that stop. **Sealing leaves the values writable**, which is the whole
difference between the two.

**A primitive is frozen and sealed vacuously but never extensible** — it has no properties to
change, and none can be added. Those two answers look contradictory and both follow from the
same fact.

## D-152

**Three ways to compare, and the differences are the point.**

Status: Accepted

`Object.is` joins `===` and SameValueZero, and no two of the three agree:

| | `NaN, NaN` | `0, -0` |
|---|---|---|
| `===` | false | true |
| SameValueZero (`includes`, `Map`) | true | true |
| `Object.is` | true | **false** |

`Object.is` is the only one that separates the zeroes. Implementing it as "`===` with a `NaN`
special case" would have been wrong in exactly one cell of that table, and the test pins it.

**The `Number` predicates do no coercion where the globals do**: `isNaN("x")` is true and
`Number.isNaN("x")` is false, because the first asks *"is this not a number"* after converting
and the second asks *"is this the value `NaN`"*. **`parseInt` reads a prefix and stops** where
`Number` demands the whole string — `parseInt("12abc")` is twelve and `Number("12abc")` is
`NaN`.

## D-153

**A prototype cell read before `with_runtime` is always empty.**

Status: Accepted

`(255).toString(16)` answered `undefined`. Five CI runs went into finding out why, and the
answer was an **ordering** the code gave no sign of.

`with_runtime` is what constructs the runtime on first use, and the prototype cells are filled
in *during* that construction. The new branch in `crisol_property_load` read
`NUMBER_PROTOTYPE` **before** entering `with_runtime` — so in any program whose first act was a
property load on a primitive, nothing had built the runtime yet and the cell was still `None`.
`let n = 255; n.toString` allocates nothing beforehand; there is no earlier call to construct
anything.

Three things conspired to make it unreadable:

- **`Number.prototype.toString` worked**, because `Number` is a *global* load, which enters
  `with_runtime` and builds everything before the lookup. So the prototype was demonstrably
  populated — from inside a program that had already touched the runtime.
- **`nullish_access` answers `undefined` for a number rather than raising**, correctly, so a
  `None` prototype surfaced as a missing method rather than as anything pointing at the cell.
- **`Map` and `Set` were unaffected**, because reaching them requires naming a global first.

The fix is to read the cell inside `with_runtime`. The general rule, which the existing cells
did not need to state because nothing read them this early: **a thread-local filled during
construction cannot be read on a path that might be the thing triggering construction.**

**On method, since it cost five runs:** the hypotheses were tested by reading code that looked
correct, repeatedly. What finally worked was probes that *partition* — is the prototype
populated, does the route through the compiler matter, does `call` bypass it — each of which
eliminated half the space whatever its answer. Reading found nothing in three attempts because
the code *was* right; only the order it ran in was wrong.

## D-154

**Assigning to `array.length` resizes the array.**

Status: Accepted

`length` is not a stored property — it *is* the element count, which is why reading it is a
special case in the property load. Writing it was not, so `a.length = 0` silently added nothing
and changed nothing.

That surfaced as **three crashes**, not as a wrong answer. test262's own `buildString` helper
fills a scratch array in chunks and empties it with `codePoints.length = 0` each time. With the
assignment doing nothing, every chunk re-sent everything accumulated so far: quadratic growth in
a string being built a code point at a time, and the process killed on memory. The report was a
signal, with nothing to say which line caused it.

Growing fills with `undefined` where the specification says holes — the same approximation array
literals already make, and refused rather than faked there (D-133). Shrinking is exact.

## D-155

**A panic must not cross the `extern "C"` boundary.**

Status: Accepted

Three test262 cases were reported as **crashes**, and the names all pointed at Unicode regex
support. The first guess — that they were building enormous strings — was wrong, and fixing that
(D-154) changed nothing for them. The actual cause is that `regress` *panics* on `\p{…}`
property escapes rather than returning an error, and an unwind out of an `extern "C"` function
aborts the process.

**A pattern the engine cannot compile is a `SyntaxError` whether the compiler says so or falls
over saying it.** The compilation is now wrapped so either answer arrives as an error a program
can catch, rather than as a signal with nothing to say which pattern did it.

**Correction: property escapes were not the trigger.** A test asserting that
`new RegExp("\\p{Script=Arabic}", "u")` raises proved the opposite — `regress` compiles them.
The guard did take the crash count from three to zero, so something in that path panics; which
pattern is *not* established, and the first sentence of this entry named one on no evidence
beyond the test file names. What is known: the boundary is now safe, and the trigger is
unidentified.

This is worth stating generally: every `crisol_*` entry point is a boundary a panic must not
cross. Regex compilation is the one known to panic today; it is not obviously the only place
user input reaches library code that may.

**And a second crash was mine.** `[].length = 4294967297` tried to materialise four billion
elements, because D-154 made the assignment resize without bounding it. A length above 2^32-1
is a `RangeError`, which is both the specification's rule and the thing standing between that
assignment and the process dying. Adding a feature added a crash; the corpus said so within one
run, which is the argument for measuring after every change rather than at the end of a batch.

## D-156

**A throw is not a return value.**

Status: Accepted

`crisol_construct_result` implements the rule that a constructor answering a primitive yields
the instance rather than the primitive. The exception signal is not an object either, so it took
the same branch: a constructor that raised handed back a perfectly good empty object, and the
`try` around it saw nothing at all.

`new RegExp("(")` was silent while `/(/ ` raised correctly — the literal propagates at the call
site and the constructor's exception never reached one. Every constructor was affected, not just
this one; a user function that throws was equally swallowed.

**The shape of the bug is worth keeping**: a rule written as "if not an object, do X" is a
rule that also catches the sentinel, and the sentinel was introduced later than the rule. Every
place that tests a value's kind to decide control flow is a place where the exception signal
needs its own answer first.

## D-157

**The array methods take an array-*like*, not an array.**

Status: Accepted

`Array.prototype.filter.call(new String("abc"), …)` is not an exotic case in test262 — applying
the array methods to anything with a `length` and indexed properties is a whole family of its
coverage, and every one of them answered `undefined` because the methods insisted on real
elements and gave up when there were none.

Length and element access now go through one pair of helpers that ask an array for its element
count first and fall back to reading `length` and numbered properties. A real array therefore
costs exactly what it did; only the array-like path is slower, and it did not work at all
before.

Converted are the read-only methods — `map`, `filter`, `forEach`, `indexOf`, `lastIndexOf`,
`includes`, `join`, `slice`, `find`, `findIndex`, `findLast`, `every`, `some`, `toString`. The
mutating ones still require a real array, because writing back through numbered properties is a
different question and one this has not answered.

## D-158

**A lone surrogate is legal and cannot be represented.**

Status: Accepted, with the limit stated

`String.fromCodePoint(0xD800)` must succeed: JavaScript strings are UTF-16 and may hold an
unpaired surrogate. These strings are Rust `String`s, which are UTF-8 and may not. Raising a
`RangeError` was the wrong answer to the right problem — the specification says this succeeds —
and it accounted for forty failures, all of them introduced by the method that raised.

A replacement character stands in. That is a **visible wrong answer** rather than an error a
program cannot expect, which is the better of two bad options: a test comparing the string sees
a mismatch it can report, where a throw stops the test before it can look.

Fixing it properly means WTF-8 or a UTF-16 rope — a representation change, not a patch — and
this is the second place the UTF-8 choice has shown through (D-115 was the first, where `length`
had to count code units over a representation that does not store them).

## D-159

**A string wrapper is indexed by its characters, lazily.**

Status: Accepted

`new String("abc")[0]` is `"a"`. The wrapper holds its text whole (D-146) rather than one
property per character, so the index had nothing to find — and once the array methods learned to
read an array-like (D-157), `Array.prototype.filter.call(new String("abc"), …)` built an array
of `undefined` instead of failing outright, which is a worse answer than the one it replaced.

Characters are read out **on demand**. Defining them at construction would charge every wrapper
for a case most never reach, and a wrapper is usually made to be passed somewhere, not indexed.

**A test's expectation was the previous behaviour, not the required one.**
`Array.prototype.indexOf.call(true)` was pinned at `undefined` because that is what the old
implementation answered when it could not find real elements. The specification says `-1`: a
boolean has no `length`, so the search runs over zero elements and reports not-found. The test
was written to lock in an answer rather than to check one, and it took a change that made the
behaviour *correct* to expose that.

## D-160

**`indexOf` compared bits where it had to compare characters.**

Status: Accepted — a bug older than everything that found it

`["a", "b"].indexOf("b")` answered `-1`, and had for as long as the method existed. The
comparison was written inline:

    (None, None) => element == wanted,

which is identity, and two cells holding `"b"` are not the same cell. `same_value` — the shared
rule that compares string *text* — was written later for `lastIndexOf` and `includes` (D-114),
and `indexOf` never adopted it.

**Every test of `indexOf` used numbers**, where comparing bits happens to agree with comparing
values, so nothing caught it. What finally did was a test of something else entirely: an
array-*like* whose elements were strings (D-157). The array-like work was not looking for this
and could not have been; it just happened to be the first `indexOf` test written with a string
in it.

That is the argument for varying the *types* in a test and not only the shapes. A method that
takes any value and was only ever tested with one kind of value has been tested for one kind of
value.

## D-161

**`Array(3)` is three elements; `Array("3")` is one.**

Status: Accepted, closing a gap the code had already admitted to

`ensure_global_object` gives every namespace the plain-object body, and a comment beside it had
said for some time that this is *"right for `Object` and wrong for `Array`"*. It was: `new
Array(10)` answered `{}`, so every test that built an array that way then called a method on it
found nothing to iterate.

**One number is a length and anything else is an element.** That is the most surprising rule in
the constructor and the reason `Array.of` exists to mean the other thing (D-150). A length that
is negative, fractional, or 2^32 or more is a `RangeError`, matching what assigning to `length`
now does (D-154).

The constructor is re-pointed after the globals are built rather than special-cased inside
`ensure_global_object`: that helper's job is to make a namespace exist, and which body one of
them runs is a different question.

## D-162

**Elements are dense; the specification's arrays are not.**

Status: Accepted, as an approximation with a stated cost

`a[4294967294] = 2` is legal JavaScript — 2^32-2 is the highest array index — and a dense `Vec`
answers it by asking for every slot below as well. That is not a slow answer but a dead process,
and it arrived as the last crash in the corpus.

Past four million, an index becomes a **named property** instead of an element. The value is
still stored and still readable by the same key; what it is not is an element, so `length` does
not count it. **That is wrong**, and the reason to do it anyway is that it is wrong in a way a
test can report rather than a way that kills the run — the same trade already made for lone
surrogates (D-158).

Four million keeps a worst case near thirty megabytes, which is an array somebody might really
build. Below the cap nothing changes: an ordinary index is an element and stays fast.

Doing this properly means sparse element storage, which is a representation change of the same
size as the UTF-16 one (D-158) and the hash-table one (D-148). Three approximations now point at
the same conclusion — the heap's value representations were chosen for the common case, and
test262 is mostly not the common case.

## D-163

**Accessor properties, which D-116 recorded as ignored.**

Status: Accepted, closing the gap that entry left open

`Object.defineProperty(o, "x", {get, set})` was accepted and silently produced a data property
holding nothing. D-116 recorded that as *"the one part of this that fails quietly"*, and it has
been quiet ever since.

**An accessor is a property whose value is computed**, so the slot cannot hold what the program
sees — it holds the pair of functions. Putting the pair in the *slot the property already
occupies* means the collector traces them exactly as it traces any other property value, with
nothing added to the heap's idea of what an object holds. The attribute that says which kind of
property this is rides with the other three.

**The call happens outside the runtime borrow**, for both directions. A getter is JavaScript and
will reach back in; calling it while the chain walk still holds the borrow is re-entering what
it is inside — the same failure as D-153, arrived at from the other end. So the walk now answers
*"a value"* or *"an accessor, here is the pair"*, and the caller does the calling.

Three rules that are each one line and each observable:

- **The receiver is the object the property was reached *through***, not the one it was found
  on, so an inherited getter sees the instance.
- **A getter with no setter swallows a write**, silently outside strict mode. That is what makes
  a read-only computed property read-only.
- **A setter with no getter reads as `undefined`** — the whole of what a write-only property is.

A descriptor carrying both a value and an accessor is a `TypeError`: they describe two different
kinds of property and one cannot be both.

## D-164

**A non-configurable property is nearly immutable, and `defineProperty` now says so.**

Status: Accepted

`Object.defineProperty` wrote whatever it was given. That is not a missing check but a
contradiction: a property made non-configurable, or an object passed to `Object.freeze`, could
be quietly thawed by redefining it. The guarantee existed only until somebody asked again.

The specification allows **exactly one** change to a non-configurable property: a writable data
property may be made non-writable, and while it is still writable its value may be set.
Everything else is a `TypeError` — turning enumerability on or off, making it configurable
again, swapping a data property for an accessor, or changing the value of one already
non-writable.

The asymmetry is worth keeping in view: writability may go **down** and never up, because every
other direction would hand back something the object had already promised not to allow.

## D-165

**`Object.create`'s second argument, and the accessor definers that predate `defineProperty`.**

Status: Accepted

`Object.create(proto, descriptors)` ignored everything after the prototype. **The second
argument is a map of descriptors, not of values** — `Object.create(p, {x: {value: 1}})` gives
`x` the value one, and `Object.create(p, {x: 1})` gives it none, because `1` describes nothing.
It is handed to `defineProperties` rather than reimplemented, so the two cannot disagree about
what a descriptor means.

`__defineGetter__` and `__defineSetter__` are older than `defineProperty` and still covered by
test262, because they were the only way a program written before ES5 could make an accessor.
They route through `defineProperty` for the same reason — with one difference that is theirs
and not a mistake: **they make an enumerable, configurable property**, where `defineProperty`'s
defaults are the opposite (D-116). Two functions that do the same thing with opposite defaults
is the sort of difference that is only safe to have written down.

## D-166

**An array's `length` is its element count, so defining it resizes.**

Status: Accepted

`Object.defineProperty(arr, "length", {value: 1})` stored a `length` *property* on an array that
already derives its length from the elements. The array then reported **two lengths at once** —
the descriptor said one and the elements said two — and every question after that got whichever
answer its asker happened to consult. `arr.length` read the elements; a descriptor read the
property; `hasOwnProperty("1")` read the elements again.

Defining it now resizes, exactly as assigning to it does (D-154), so there is one answer.

**`writable: false` on a length needs somewhere to live.** A length is derived rather than
stored, so it has no slot whose attributes could carry the flag — it is a hidden property
beside it, and the assignment path consults it. That is the fourth thing kept this way (D-126,
D-146, D-151), and the list is starting to argue for real internal slots rather than a
convention plus a growing set of names to exclude from enumeration.

**On finding it:** the corpus reported a crash and reading the case did not explain one.
Reproducing the same call as an acceptance case reported a *wrong answer* instead — `2` where
`1` was wanted — which is what the harness difference is for: one reports a signal and the
other prints what the program said. The crash was downstream of the disagreement, in a helper
that trusted the two answers to match.

## D-167

**An array owns `length`, and `getOwnPropertyNames` has to say so.**

Status: Accepted

`Object.getOwnPropertyNames([0])` answered `["0"]`. An array owns its indices **and** `length`,
even though nothing stores the latter — it is derived from the element count, which is why it
was missing from a list built by walking what is stored.

It is added after the indices, because the specification fixes that order, and it is **kept out
of enumeration**: `Object.keys` and `for-in` must not see it. That needed its own filter rather
than falling out of the attribute check, because the attribute check asks a property for its
permissions and a derived length has none — so it would have answered "enumerable" by default.
The two lists differ in exactly this one name, which is the whole reason they are two lists.

**A test of mine asserted the total was zero.** It was checking that an internal flag stays
hidden, and reached for the easiest observable — a count — which encoded a second wrong belief
about arrays while testing the first thing correctly. It now asserts the flag *by name*. A
count is a bad assertion for "X is absent": it passes for the wrong reason whenever the total
is wrong for another one.

## D-168

**`__proto__` is an accessor on `Object.prototype`, answered where the walk reaches it.**

Status: Accepted

`o.__proto__` read `undefined` and `o.__proto__ = p` stored a property called `__proto__`
without re-parenting anything. Both are load-bearing: the read is how most code still asks for
a prototype, and the write is what `{__proto__: p}` in an object literal lowers to.

The obvious fix — check the name at the top of `crisol_property_load` — is shorter and wrong
twice over. An own `__proto__` (which `Object.defineProperty` can still make) could no longer
shadow it, and `Object.create(null)` would grow one, when the whole point of a null prototype
is that the chain never reaches the object that publishes the accessor. So the check sits
**inside the chain walk**, at the step that arrives at `Object.prototype`, and answers with the
receiver the walk started from. That is what an accessor does, expressed in the walk rather
than in a pair of functions the bootstrap would have to allocate before it can allocate
functions.

**The two refusals are shared.** `Object.setPrototypeOf`, the `__proto__` setter and (when it
exists) `Reflect.setPrototypeOf` all route through one `set_prototype_of`, which refuses a
cycle and refuses to re-parent a non-extensible object. A cycle is the one that matters: every
lookup that misses would walk it, and the bound that stops that being a hang
(`PROTOTYPE_CHAIN_LIMIT`) is a backstop, not a licence to build one. `Object.setPrototypeOf`
also previously accepted a number as a prototype and quietly set `null`.

## D-169

**An array's elements are own properties, so every question asked of a descriptor must answer
for them.**

Status: Accepted

`own_property` reads the shape. An array's elements are not in the shape — they are a dense
`Vec` beside it — so everything built on that function answered "absent" for exactly the
properties an array is made of:

- `Object.getOwnPropertyDescriptor([1], 0)` was `undefined`. test262's `propertyHelper` reads a
  field off that, which made it **the single largest failure reason in the corpus** (119 cases
  of "cannot read a property of undefined"), from a method that looked finished.
- `Object.freeze([1])` froze nothing. The loop it runs visits stored properties; an array has
  none, so the call ran zero iterations and looked like it had worked.
- `Object.isFrozen(Object.preventExtensions([1]))` was `true`, because an unslotted key was
  *passed over* rather than asked about.
- `Object.defineProperty(a, "0", …)` added a slot beside the element, so the array held two
  answers for one key — the element reads use, the slot descriptors use.

`derived_own_property` is the one place that answers for them, and the three callers that used
to shrug at a missing slot now ask it. Elements share **one** set of attributes for the whole
run, carried as two hidden flags, because there is nowhere per-element to put them; that is
exact for `freeze`, `seal` and `preventExtensions`, which set them for every element at once,
and approximate for a `defineProperty` that restricts a single index — which still falls
through to the slot path rather than being dropped.

**A string wrapper's characters are the same problem.** They are materialised on demand
(D-157), so nothing lists them either; `Object.keys(new String("ab"))` answered without them.

**The bookkeeping flags are now read as own properties, not as property reads.** They stand in
for internal slots, and an internal slot belongs to one object — reading one up the chain meant
`Object.freeze(proto)` made every object later created from it report itself non-extensible.
That was a prototype doing to its instances something only they can do to themselves. It is
also cheaper: a shape lookup instead of a chain walk that nearly always misses, on a path every
`for-in` takes.

## D-170

**`Object`'s statics coerce their argument; only `null` and `undefined` are the error.**

Status: Accepted

`Object.keys(null)` answered `[]`, `Object.getPrototypeOf(1)` answered `null`, and
`Object.assign(null, {})` did nothing. All three are the same mistake read two ways: treating
"not an object" as the failure condition, when the specification's failure condition is
`RequireObjectCoercible` — nullish — and everything else is wrapped first.

The two halves matter separately. Failing to throw loses the 74 corpus cases that check the
throw. Failing to *coerce* is worse than it looks: `Object.getPrototypeOf(1)` answering `null`
is not a missing answer but a wrong one, because `null` is itself a legal prototype and says
the number has none.

`Object.freeze`, `seal`, `preventExtensions` and the three `is…` queries are the exception —
they take a primitive and hand it straight back, which is ES2015's change and not an oversight.

**A string's characters come with this.** Once a primitive is coerced rather than refused,
`Object.keys("ab")` has to be `["0", "1"]` — the same properties the wrapper has, from the
same place, rather than from a second rule that could disagree.

## D-171

**A boolean wrapper stores a boolean.**

Status: Accepted

It stored `1` or `0`, which read back correctly everywhere that asked "true or false" and made
it **indistinguishable from a `Number` wrapper** — the one distinction
`Object.prototype.toString` has to make to answer `[object Boolean]`. The tag is not a detail:
it is the only classification an engine without `Symbol.toStringTag` can offer, and answering
`[object Object]` for every wrapper is what made the method useless for the job it exists for.

A representation chosen to satisfy one reader is a representation that loses whatever the other
readers would have asked. Keeping the primitive as the primitive costs nothing and answers both.

## D-172

**Enumeration order is indices first, ascending, then names in insertion order.**

Status: Accepted

Every own key went into one list in the order the shape recorded it, which is right for the
names and wrong for everything else: `Object.keys({b: 1, 2: 1, 1: 1})` answered
`["b", "2", "1"]` where the specification fixes `["1", "2", "b"]`. The order is observable
through `Object.keys`, `Object.values`, `Object.entries`, `for-in` and `JSON.stringify`, so one
wrong list is five wrong answers.

Only the **canonical** spelling is an index. `"01"` parses as one and is not an array index, so
it stays among the names — a parse alone is not the test, and using one would have moved keys a
program wrote as text.

## D-173

**`Object(x)` is `ToObject(x)`.**

Status: Accepted

It answered a fresh empty object for every argument, so `Object(o) === o` was false and
`Object(5)` had no `valueOf`. The wrapper is built here rather than by calling the `String`,
`Number` or `Boolean` globals, because a program can replace those and `ToObject` is an
internal operation that must not be reroutable — but it stores exactly what those constructors
store, so a method reached through either wrapper reads the same primitive back.

## D-174

**An index names an element, even spelled as text.**

Status: Accepted

`a[0]` reached the elements and `a["0"]` did not. They are the same property, so the second
answered `undefined` for every element an array has — and it is the spelling every path that
works by *name* uses. `Object.values([7, 8])` was `[undefined, undefined]`,
`Object.entries` the same, and `Object.assign({}, [7, 8])` copied two undefineds and a
`length`.

The computed path handled a number key; nothing handled the string one. Both spellings now go
through one `store_element` on the way in and one branch of the named load on the way out, so
they cannot disagree again. A primitive string reads its characters there too, rather than
only through a wrapper it never made.

**`Object.assign` copies the enumerable ones.** It walked every own key, which includes an
array's `length` and a wrapper's — so the target gained a `length` it had no business having.
That was invisible while the values it copied were all `undefined` anyway.

## D-175

**`Object.groupBy` walks by index, and says so.**

Status: Accepted

The specification iterates its argument. This engine cannot iterate a user-defined iterable —
a `PropertyKey` is a string, so `Symbol.iterator` cannot be looked up (D-149) — so this walks
`length` and the indices instead. That is exact for arrays, strings and array-likes, which is
what the corpus passes it, and groups *nothing* for anything else rather than grouping it
wrongly. When keys learn about symbols this becomes a real iteration and nothing else changes.

The result has **no prototype**, which is the method's whole design: the keys come out of the
data, so a group named `"toString"` has to be a group and not the inherited method.

## D-176

**A prototype points back at its constructor, and a function's `prototype` is not enumerable.**

Status: Accepted

Nothing defined `constructor` anywhere, so `({}).constructor` was `undefined` — the ordinary
way a program asks what made an object, and the property a subclass replaces. It is defined on
every built-in prototype and on every compiled function's, as writable, non-enumerable and
configurable.

Not through `define_method`, which is the other place with those attributes: that one also
names the function it defines, so `Object.prototype.constructor` would have come out called
`"constructor"`.

**And the other direction was enumerable.** A function's `prototype` was an ordinary property,
so `Object.keys(f)` reported it for every function a program can see. That is one line in
three places and it was wrong in all three.

## D-177

**`toLocaleString` calls the receiver's `toString`, not the default one.**

Status: Accepted

It was wired straight to `Object.prototype.toString`, which is right for a plain object and
wrong for every object with an override — and being a hook for exactly that override is the
only reason the method exists.

## D-178

**`defineProperty` refuses to add to a non-extensible object.**

Status: Accepted

It was the one refusal the method never made. `Object.preventExtensions(o)` stopped assignment
and let a *definition* straight through, which is precisely the hole it exists to close — and
test262's very first `defineProperty` case checks it.

An existing property may still be redefined, subject to the configurability rules: it is the
adding that stops. On an array the same rule reads as "growing the run of elements is the
addition", so defining an index past the end is refused and defining one below it is not.

**A frozen or sealed array skips the fast path.** The array-index shortcut writes the element
directly, which would have gone straight past the refusal that freezing is for; it now defers
to the generic path whenever the elements are not ordinary, and that path reads their
attributes from the derived answer (D-169).

## D-179

**An element carries its own attributes, in a rule array beside the run.**

Status: Accepted

Elements are a dense `Vec` with no room for attributes, so the first version of this carried
two flags for the whole run (D-169) — exact for `freeze` and `seal`, which restrict every
element at once, and unable to express `Object.defineProperty(a, 0, {writable: false})` at all.
That matters more than it sounds: `defineProperty` and `defineProperties` are **more than half**
of test262's `Object` directory, and a sixth of those target arrays.

The rule array has two levels. Position 0 is the rule for every element; position `i + 1`
overrides it for element `i`; a position holding `undefined` is not an override. Two levels
rather than one entry per element because the two writers want different things — freezing a
million-element array must not cost a million entries, and `defineProperty` restricts exactly
one. **Absent means ordinary**, so an array nobody restricts carries nothing and the write path
pays the one shape lookup it already paid.

**`defineProperty` now has one path.** The array-index case used to be a separate shortcut that
only handled unrestricted descriptors; the current attributes come from `derived_own_property`
either way, and only the two *writes* differ. An accessor is still the exception — the place
the pair of functions would live *is* the element — and falls through to the slot path.

## D-180

**A thrown error is an instance of what threw it.**

Status: Accepted

`raise` built a plain object with `name` and `message` on it. Every check a program makes about
what it caught — `e instanceof TypeError`, `e.constructor`, `Object.prototype.toString.call(e)`
— therefore said `Object`, for an engine that had thrown exactly the right thing.

This was invisible until `constructor` existed (D-176). Before that, test262's `assert.throws`
read `thrown.constructor.name` off `undefined` and failed with a different message; the fix to
one hole made the other one legible. **55 corpus cases changed their complaint the moment
`constructor` landed, and none of them changed their outcome** — which is the useful kind of
regression: the same failures, finally saying what they are.

The prototype is read off the global constructor rather than from a cell of its own, so
replacing `TypeError.prototype` changes what the engine throws. That is wrong for an internal
operation and right against the alternative, which is five more thread-locals kept in step by
hand.

**`name` moved to the prototype**, where one string serves every instance, and `message` became
non-enumerable — so `Object.keys(new TypeError("x"))` is empty, as it is everywhere else.
`Error.prototype.toString` exists; errors inherited `Object.prototype.toString` and described
themselves as `[object Object]`.

## D-181

**`[object Error]` is keyed on what made the object, not on what it inherits from.**

Status: Accepted

The specification reports the tag for an object carrying an internal slot the `Error`
constructors install, so `Object.create(Error.prototype)` is `[object Object]`. Reading the
prototype chain instead would have been right about every error and wrong about the one case
that distinguishes the two — which is the case the tests check, because it is the only one
where the answer is not obvious. The tag is a hidden property, as the other four internal
slots are (D-126), and stays out of every enumeration.

## D-182

**A deleted property is absent from every question, not just from enumeration.**

Status: Accepted

`own_property` read the slot the shape still names, because the shape *does* still name it —
that is what the tombstone is for. Enumeration checked the tombstone separately and everything
else did not, so a deleted property kept answering with the permissions it had before it went:
a redefinition validated against a property that is no longer there, and `Object.assign` could
be refused by a `writable: false` nothing holds any more.

Checking it in the one place that reads slots by name is what makes the rest agree, and it
tightened three answers that were separately wrong — `getOwnPropertyDescriptor`,
`hasOwnProperty` through `own_keys`, and the `defineProperty` validation.

**`Object.assign` throws on a read-only target.** It uses the throwing form of `Set`, which is
one of the few places a program not written in strict mode can see the difference between a
refused write and a silent one.

## D-183

**A descriptor has to describe something, and a namespace's constants are not enumerable.**

Status: Accepted

Three refusals `defineProperty` was not making, all found by reading what the corpus still
complained about rather than by guessing:

- **A primitive descriptor.** The check asked for a *handle*, and a string has one without
  being an object — so `Object.create({}, {p: "abc"})` defined `p` as `undefined` where the
  specification throws. The same mistake as the target check, in the line below it.
- **A `get` that is present and not callable.** Treating it as "not an accessor" made
  `{get: "string"}` a data descriptor with no value, which is a quiet wrong answer where there
  is an error to report.
- **`Math.PI` and `Number.MAX_VALUE` were enumerable.** That is visible in
  `Object.keys(Math)`, and load-bearing in `Object.defineProperties(o, Math)` — which walks
  the enumerable own properties and reads each one as a descriptor, so a constant became
  `3.14159…` where a descriptor was wanted. The constants are now readable and nothing else,
  which is the attribute set the specification gives them.

## D-184

**A built-in declares how many arguments it takes.**

Status: Accepted

`Function.length` is the count of parameters before the first with a default or a rest — a
number the specification fixes per method, not something an implementation chooses. No built-in
here had one at all, so `Object.keys.length` was `undefined`, and 227 corpus files that check
nothing else failed on methods that were otherwise complete.

The table is keyed by **owner and name**, because one name disagrees with itself:
`Number.prototype.toString` takes a radix and every other `toString` takes nothing. A method
the table does not list gets no `length`, which is what it had before — a missing answer rather
than a wrong one, and the difference matters because a wrong `length` is invisible until
something reads it.

**The numbers were read out of test262, not recalled.** 138 of the 180 appear in a `length.js`
or an older `.length ===` assertion; the rest are the specification's and unambiguous
(`Math.pow` is two, `Math.random` is zero). Transcribing 180 arities from memory would have put
wrong numbers where there had been none, which is worse than the gap — and there is no way to
derive one from a `Native`, whose signature is the same five operands for every built-in.

## D-185

**`this` at the top of a script is the global object.**

Status: Accepted

The entry point passed `undefined`, which is what `this` is inside a strict function and never
what it is at the top of a sloppy script. So `this.x = 1` did nothing, `this === globalThis`
was false, and every test that reaches a global through `this` — test262 has a lot of them —
read a property of `undefined` instead.

The entry point asks the runtime for it rather than being handed a constant, which means the
runtime is built before the program starts. That is safe in the one way it needs to be: the
stack maps are registered first, so a collection during construction has everything it needs to
walk.

**`Object.assign` coerces its target too.** `Object.assign(true, {a: 1})` answers a `Boolean`
wrapper carrying the assignment; the primitive was handed straight back, unable to carry
anything. That is the same rule as D-170 in the one static that had been missed.

## D-186

**Every `Date` setter is one operation with a different starting field.**

Status: Accepted

`Date` had every getter and no setters at all, which was the single largest failure reason in
the corpus — 70 cases of `is not a function`, nearly all of them `Date.prototype.set…`.

They are written as one function taking a starting field and a count, because that is what they
are: `setHours(h, m, s, ms)` writes four of the seven broken-down fields and
`setMinutes(m, s, ms)` writes three of the same four. Seven separate implementations would be
the same decompose-replace-recompose seven times over, with seven chances to get an argument
count subtly wrong in a way one test notices and the others do not.

Three details that are easy to get backwards and are each a test:

- **Arguments are coerced before the date is checked.** Coercion runs user code and the
  specification orders those effects first, so an invalid date still calls the `valueOf` it was
  handed.
- **The first argument is coerced even when absent**, which is why `d.setHours()` yields an
  invalid date rather than leaving the date alone.
- **`setFullYear` starts from the epoch when the date is invalid** and every other setter
  answers `NaN`. A year is enough to name a date and an hour is not.

**The UTC twins are the same function**, as the getters already were: this engine has no
local-time offset, and `getTimezoneOffset` answers zero. That is honest rather than convenient
— when an offset exists the two have to split, and the pairing here is what will make that
obvious.

`MakeDay` bounds the year before converting to the integer calendar arithmetic. A year of 1e20
would otherwise wrap into a plausible date rather than the `NaN` that `TimeClip` would have
produced anyway.

## D-187

**`Reflect` is `Object`'s operations with the failures reported rather than thrown.**

Status: Accepted

Every method is machinery a property access already uses, exposed as a function — which is why
it can exist here at all without proxies, the other half of what `Reflect` was designed for.
Seventeen corpus cases were failing on `Reflect is not defined` and nothing else.

The interesting part is the failure convention. `Object.defineProperty` throws where
`Reflect.defineProperty` answers `false`, and both run the same code — so the exception the
shared implementation raised has to be taken back off the runtime. Left there, the next `catch`
in the program would receive a throw that nothing performed, which is a far worse bug than the
one being papered over. `swallow_exception` is that, and it is the only place the pending throw
is read for a reason other than reporting it.

**Two things `Reflect` still throws for**, because they are not refusals: a target that is not
an object, and a descriptor that describes nothing. The specification's `false` is for a
definition the target declines, not for an argument that was never a request.

**`Reflect.get`'s `receiver` is ignored**, and `ownKeys` reports no symbols (D-149). The first
exists so a proxy trap can read through to a getter with the original receiver; honouring it
means giving the property walk a receiver separate from the object it is walking, which is a
change to the walk rather than to this.

**`refuses_assignment` learned about extensibility**, which `Reflect.set` needed and
`Object.assign` had wanted all along: a write to an absent property on a non-extensible object
adds one, which is exactly what it will not do — and the store ignores it silently, so nothing
downstream would have noticed.

## D-188

**An iteration method checks its callback before it reads an element.**

Status: Accepted

None of them checked. `Array.prototype.map`, `filter`, `forEach`, `every`, `some`, `find`,
`findIndex`, `findLast`, `findLastIndex`, `reduce`, `reduceRight`, `flatMap`, `sort` and both
collection `forEach`es went straight to calling what they were handed — which reaches
`crisol_not_a_function`, and that answers `undefined` by design (it exists so a bad callee
costs a wasted call rather than a jump through a null pointer).

The result was that `[1, 2].map(5)` answered `[undefined, undefined]`. Not an error, not a
crash: a plausible array of the right length, which is the worst of the three. Twelve call
sites, and the check is the same at all of them.

**A comparator is the exception**: `sort` takes one or nothing, so only a present
non-callable is refused. Without that check every comparison answered `undefined`, which
compares as neither less nor greater — so `[3, 1].sort(5)` silently kept its input order and
looked like a stable sort of an already-sorted array.

**Found by reading what the corpus still complained about**, not by inspection: "Expected a
TypeError to be thrown but no" was 62 cases and the six examples the runner prints named three
distinct causes, of which this was the most common. The other two — `ToNumber(symbol)` and a
`ToPrimitive` whose `valueOf` and `toString` both answer objects — need a fallible coercion
path that can propagate, which the current `to_number` (returning a bare `f64`) cannot.

## D-189

**A length that cannot become a number is an error, not a zero.**

Status: Accepted

`indexed_length` read `length` through `property_number`, which answers `None` for anything
that is not already a number — so a symbol, or an object whose `valueOf` and `toString` both
answer objects, became a length of zero and the method walked nothing. The specification
throws in both cases, and test262 checks each against several methods.

Two conversions fail where the rest answer `NaN`:

- **A symbol refuses to be a number.** That is the point of symbols: one exists to be unequal
  to everything, and a number it could be compared as would defeat it.
- **`ToPrimitive` with no primitive to reach.** The older `to_primitive` hands the object back
  instead, which turns a reportable error into arithmetic on `NaN`.

`coerce_number` and `coerce_primitive` are the fallible pair; `to_number` and `to_primitive`
stay for the callers that cannot throw — `==` among them, because its result is a value the
codegen does not check for the exception sentinel. **That is a real split and the wrong one
long-term**: two ways to ask a question are two answers. It is recorded rather than hidden
because closing it means giving the operators an exception path, which is its own change.

**The tests assert the order, not just the throw.** An engine that never called `valueOf` at
all would pass a test that only checks for a `TypeError`, so the case that matters records
`"vs"` and would catch a conversion that skipped straight to failing.

## D-190

**A namespace is an ordinary object, and `String()` with no argument is empty.**

Status: Accepted

Two bugs, both found by a check added for something else — `defineProperty` refusing a
descriptor that is not an object (D-183) started throwing on a case that should have worked,
and following it back found the second.

**`ensure_global_object` never linked a prototype.** `Math`, `JSON`, `Reflect`, `Object` and
`Array` all ended their chain immediately, so `Math.hasOwnProperty("x")` was not a function.
One line, and it had been wrong since those objects existed — invisible because nothing
reaches for `Object.prototype`'s methods on a namespace until a test does.

**`String()` answered `"undefined"`.** An absent argument reads as `undefined`, and
`String(undefined)` really is `"undefined"`, so the two cases have to be told apart by the
argument count rather than by the value — and they were not. The consequence was worse than a
wrong string: `new String()` wrapped nine characters, so it had nine own enumerable properties
and behaved as an array-like of letters. That is what `Object.create({}, new String())` tripped
over, and the reason it surfaced as a descriptor error three layers away.

## D-191

**A numeric argument is coerced, truncated, and able to refuse.**

Status: Accepted

Ten methods read a positional argument with `Value::as_number().unwrap_or(0.0)`, which answers
zero for a string, a symbol and an object alike. So `"abc".charCodeAt("1")` read character
zero — and looked like a working call, because the answer was a plausible character code.

`integer_argument` is `ToIntegerOrInfinity` built on the fallible coercion (D-189), and it
fixes three separate things at once:

- **A string or an object converts**, rather than reading as zero.
- **It truncates.** `"abc".charAt(1.7)` is `"b"`; indexing with the raw number left whatever
  the cast did with the fraction, which is right for every whole number anybody tests by hand.
- **`NaN` is zero and a symbol is an error**, which were the same answer before.

`charAt(NaN)` was separately wrong in the other direction — it took the `!is_finite` branch and
answered `""` where the specification says the first character.

## D-192

**A symbol can be a property key, and it is the address that identifies it.**

Status: Accepted, superseding the limitation recorded in D-149

`PropertyKey` was an `Arc<str>`, so `obj[Symbol.iterator]` could be neither set nor found:
`key_of` answered `None` for a symbol, which reads as a missing property. That was the ceiling
on the iterator protocol, on `for-of` over a user-defined iterable, and on most of what is
left in `RegExp.prototype`, where `Symbol.match`, `Symbol.replace` and `Symbol.split` are how
the methods are reached at all.

A key now carries an optional address, and **that is what equality compares**. Two symbols
described alike stay different properties, and neither is the string that describes them. The
description rides along for `Debug` only, which the type says in as many words — anything
deciding behaviour from `as_str` has to ask `is_symbol` first, or a symbol described
`"length"` becomes the property of that name.

**A symbol used as a key is rooted for the life of the program.** A key lives in the *shape*
table, which outlives any object holding the property, so a collected symbol would leave a
shape naming an address that no longer means anything — and `getOwnPropertySymbols` would hand
that back as a value. That is a leak, and it is the same bargain the specification makes for
`Symbol.for`'s registry. The alternative is teaching the collector to trace shapes, which is a
larger change than this one and buys only the memory back.

**Symbols are not names.** `Object.keys`, `getOwnPropertyNames` and `for-in` report strings;
`getOwnPropertySymbols` reports symbols and now actually has something to report. Mixing them
would put a description where a property name was expected, which is the failure the type's
documentation exists to prevent.

## D-193

**A symbol key never round-trips through its description.**

Status: Accepted

D-192 taught `key_of` to build a symbol key and stopped there, which was half the change. The
computed paths take that key, call `as_str()` on it, and hand the text to
`crisol_property_load` — so both halves of `Symbol("k")` and `Symbol("k")` arrived as the
string `"k"` and were the same property. The acceptance test that caught it asserted exactly
that two symbols described alike stay distinct; without it the feature would have looked
finished, because every test using *one* symbol passes either way.

`crisol_property_load` and `crisol_property_store` take `*const u8` and a length, so there is
no way to pass a symbol through them — the fix is a key-based pair beside them, used by the
computed paths when the key is a symbol. None of the named path's special cases apply on the
way: a symbol is never an index, never `length`, and never a character of a string, so the
symbol path is the chain walk and nothing else.

`in` and `delete` had the same shape of bug in smaller form. `in` reached for `to_text`, which
a symbol has none of, and answered `false`; `delete` asked the derived-property table by
description, which would have answered about whatever string property shared the name.

## D-194

**`for-of` asks for `Symbol.iterator` before it recognises a shape.**

Status: Accepted

`crisol_iterate` handled arrays and strings and refused everything else, so a user-defined
iterable was a `TypeError` — the protocol existed in the language and not in the engine. Now
that a symbol can be a property key (D-192), the object can be asked, and the two fast paths
are what they always should have been: shortcuts for built-ins that would answer the same way.
Asking first is also what lets a program override either, which is the point of the protocol
being a property rather than a type.

**It drains eagerly, which the caller's contract already required.** `crisol_iterate` hands
back something the loop walks by index, so the whole sequence is materialised before the body
runs once: a generator's side effects all happen up front, and an endless iterator is refused
at the dense cap rather than filling memory. Making it lazy means giving `for-of` an iterator
object to step, which is a change to the lowering and not to this.

The errors are the ones the protocol specifies — a `next` that is not callable, a step that is
not an object — and a throw from inside `next` reaches the program rather than quietly ending
the loop, which is the failure that would look like an empty collection.

## D-195

**A well-known-symbol method is an alias, not a second function.**

Status: Accepted

`Array.prototype[Symbol.iterator]` **is** `Array.prototype.values` — the specification says
the same function object, and a test comparing the two would catch a copy. So the wiring reads
the method back off the prototype and defines it a second time under the symbol key, rather
than making another native that does the same thing.

It runs after `build_globals` because it needs both halves: the prototypes and the well-known
symbols, and the symbols are made inside that.

**Only `Array.prototype` is wired**, because an alias needs something to alias. `Map` and
`Set` have no `values` or `entries` yet, and `String.prototype`'s iteration is the character
walk `crisol_iterate` already performs — pointing the symbol at some other method would be
worse than leaving the fast path to answer. The absence is asserted in the acceptance suite so
it stays visible rather than being assumed closed.

The symbol is added to the permanent key roots (D-192) even though it is also reachable from
`Symbol.iterator`, because a program can delete that property and the shapes would outlive it.

## D-196

**`o[k]()` passes its receiver, and `for-of` over an array stays live.**

Status: Accepted

Two bugs found by one acceptance run, and the second was mine.

**Only the dotted form passed a receiver.** The lowering matched
`StaticMemberExpression` and sent every other callee shape down the plain-call path with
`undefined` as `this` — so `a["push"](1)` pushed onto nothing. The comment directly above that
match says losing a receiver is silent, which is exactly what happened: the call runs,
something comes back, and only `this` is wrong. It surfaced because
`[1, 2, 3][Symbol.iterator]()` built an iterator over `undefined` and answered
`{done: true}` immediately.

**And asking the protocol first broke live iteration.** D-194 put `Symbol.iterator` ahead of
the array fast path, which is the right order in the abstract and wrong here, because this
engine drains the protocol eagerly: the loop then walks a snapshot, and `for (x of a) a.pop()`
visits three elements where the specification says two. The array iterator re-reads the length
each step and the fast path preserves that; the drain cannot.

So the order is **own symbol, then shape, then inherited protocol**. An override placed on the
object is obeyed, a plain array stays live, and a user-defined iterable still works. The cost
is that replacing `Array.prototype[Symbol.iterator]` wholesale is not obeyed for arrays —
recorded rather than hidden, and it closes when `for-of` steps an iterator instead of walking
an index.

## D-197

**Promises, with the queue held as data rather than as closures.**

Status: Accepted

`crisol-builtins::promise` has a complete agent — states, microtask ordering, `adopt`, its own
tests — and it cannot be wired to the ABI as it stands. Its queue holds
`Box<dyn FnOnce(&mut Agent, …)>`, so a reaction that calls a JavaScript handler needs the
agent while the drain loop already holds it exclusively, and every workaround reduces to two
live `&mut` to the same object. That is not a Rust obstacle to route around; it is the design
saying the host cannot drive it.

So the machinery here holds **data**: a job is `(handler, value, derived, rejected)`, and the
drain pops one under a short borrow, **drops it**, calls the handler, and takes a fresh borrow
to record the outcome. The handler can attach more reactions, settle other promises or throw,
and none of it re-enters a live borrow.

Holding data rather than closures pays a second time: **the collector can walk it**. A
`Box<dyn FnOnce>` hides its captures, so a settled value reachable only from a queued job
would be freed under the queue. The root walk now covers both structures directly.

The queue drains once, from the entry point, after the program body returns.

**The acceptance suite can only see the synchronous half.** The drain happens after
`crisol_program` returns, so nothing a handler does reaches the value the harness compares —
a test claiming to check that a handler ran would pass whether or not the drain happened at
all. What is asserted is everything observable before the return (ordering, that `then`
answers a promise, the errors) plus a clean exit, which does catch a drain that faults or
hangs. Observing the asynchronous half needs a harness that can read state *after* the drain,
and that is a change to the harness rather than to this.

## D-198

**The cost of the symbol and promise work, paid down.**

Status: Accepted

Four costs the first versions carried, found by reading the hot paths rather than by profiling
— each is on a path every program takes, so none of them needed a benchmark to be worth
fixing.

- **`key_of` allocated a `String` per symbol-keyed access.** It read the symbol's description
  so a key could carry it, and the description is only ever printed by `Debug`. Now it carries
  the empty string and the allocation is gone.
- **`key_of` rooted the symbol on every *read*.** A permanent set grew for keys the heap never
  recorded. The root belongs where a shape actually gains the address — the store path — which
  is also the only place D-192's argument applies.
- **`for-of` rebuilt `Symbol.iterator`'s key on every loop.** A globals lookup, two shape
  lookups and a heap read, per entry to a loop rather than per iteration, so a loop inside a
  hot function paid it every call. The symbol is made once and never replaced, so the key is
  cached and cloning it is an `Arc` bump.
- **`own_keys` allocated a `HashSet` per call** to remove duplicates that only an array with
  stored properties can produce — on a function every `for-in` and every `Object.keys` runs.
  It now skips the pass unless that case is actually present, and scans linearly when it is,
  because the list is short exactly when the scan happens.

**And `PropertyKey` lost a word.** `Option<Address>` costs two, because `Address` wraps a plain
`u64` and leaves Rust no niche; a sentinel outside the 48 bits an address can hold costs one.
A key sits in every shape transition and every property list, so that is eight bytes per
property of every object in the heap.

**One cost is recorded rather than fixed.** `PROMISES` grows for the life of the program: a
record cannot be freed while its promise object might still be reached, and nothing tells the
runtime when that stops being true. Closing it needs the collector to report unreachable
promise objects, which is a larger change than the machinery it would serve.

## D-199

**The eighth rooting bug, and the one the convention exists to prevent.**

Status: Accepted

`make_promise` allocated the promise object before rooting the executor it had been handed.
Under GC stress the executor — still sitting in the argument buffer — was collected, so the
closure never ran and the promise was born pending with nothing to settle it. Silent without
stress, wrong with it, exactly like the seven before.

What makes this one worth recording separately is that **every other native opens with
`live_values` and this one did not**. The convention is not decoration and it is not advice:
it is the only thing standing between an argument and the first allocation. A native that
skips it is not saving a line, it is opting out of the rule — and the failure it buys is
invisible to every run that does not collect at the wrong moment.

The body moved into a second function so the rooting is the whole of the entry point and
cannot be stepped around by a later edit adding work above it.

## D-200

**Proxies, with the marker in an internal slot and the invariants in the builtin.**

Status: Accepted

Two halves, and the interesting decisions are in how each is paid for.

**The test is an internal slot, not a hidden property.** Every property access has to ask "is
this a proxy?", and a hidden property is a shape lookup — a real tax on every program, most of
which contain no proxy at all. An internal slot is a `Vec` index inside a borrow the load path
already takes, so the common answer costs one integer compare. The marker is `null` because no
callable stores one: every function puts a number in slot zero, its index or its code pointer.

That forced a change to `is_callable`, which tested for the *presence* of slot zero rather
than its contents — so every proxy would have reported `typeof "function"`. It now requires a
number, which is strictly more precise and true of every function the engine makes.

**The invariants come from `crisol-builtins::proxy`**, which already had them: a `get` trap
cannot report anything but the held value for a non-configurable, non-writable property,
because that property is a promise the target made. Those checks are pure — they compare a
trap's answer against the target's state and call nothing — so unlike the promise agent
(D-197) they wire in as they stand. The ABI supplies a `Target` over the heap and calls the
trap; the builtin decides whether the answer was allowed.

**A proxy has no prototype of its own.** Every lookup goes to the trap or the target, so a
chain on the proxy cell would be a second answer nothing consults.

**`apply` and `construct` are not trapped**, so a proxy of a function is not callable. Doing
it means `is_callable` following a proxy to its target, on the hot path, for a case the corpus
barely exercises — recorded rather than smuggled in.

## D-201

**A promise's state lives on the promise, so a dead promise dies.**

Status: Accepted, replacing the side table in D-197

D-197 kept every promise in a `Vec` indexed by an id stored on the object. That table grew for
the life of the program: a record could not be freed while its promise might still be reached,
and nothing told the runtime when that stopped being true. It was recorded as a known leak,
which is not the same as acceptable — a loop making promises leaks a record per iteration for
ever.

The fix was already in the heap: **`Heap::reachable` traces internal slots.** So a promise's
state — settled-or-not, its value, and the list of reactions waiting on it — lives in its own
internal slots, and the whole thing is freed with the promise by the collector that already
runs. No table, no ids, no reclamation logic to get wrong, and one fewer root walk.

The reaction list is a JavaScript array in a slot, holding groups of three. It is **made on
first use**, because most promises settle before anything waits on them and those never
allocate one, and it is **dropped as it is taken** on settling — a settled promise never needs
its reactions again, and keeping them holds every handler and everything each closure captured
alive for as long as the promise is.

The microtask queue stays beside the heap, and that is fine for the reason the table was not:
a queue empties.

**`finally` was wrong, not merely incomplete.** It called the handler at the moment `finally`
was called — before the promise settled, once rather than on whichever way it went — and the
comment beside it claimed the drain did the work. It is a registered reaction now, with a
pass-through flag: the handler runs for its effect, takes no argument, has its answer
discarded, and the original settlement survives. A throw from it still replaces the outcome,
which is the one way `finally` is allowed to change anything.

**And the combinators exist**: `all`, `allSettled`, `race`, `any`, as one walk with four
endings. The empty list is where they differ most and where a shared implementation earns its
keep — `all` and `allSettled` fulfil at once, `any` rejects because no fulfilment can arrive,
and `race` stays pending because nothing will settle it.

Still missing, and named rather than implied: thenable assimilation (resolving with a
non-promise object that has a `then`), subclassing through `Symbol.species`, and
unhandled-rejection reporting.

## D-202

**The reflective traps, and where a proxy is allowed to disagree with its target.**

Status: Accepted, completing D-200

`ownKeys`, `getOwnPropertyDescriptor`, `defineProperty`, `getPrototypeOf`, `isExtensible` and
`preventExtensions`. Each forwards to the target when the handler defines nothing, which is
what a handler with one trap depends on.

Two places where the shape of the answer is not obvious:

- **`defineProperty` returning `false` is an error here.** `Object.defineProperty` throws when
  a definition does not take, and a trap answering falsish is a definition that did not take —
  where `Reflect.defineProperty` reports the same refusal as its answer. Same operation, two
  conventions, and the proxy sits under both.
- **`isExtensible` cannot lie at all.** Most invariants are corner cases about
  non-configurable properties; this one is the whole rule, since a proxy must report exactly
  what its target reports. The trap exists only to observe, and the test asserts both that
  disagreeing throws and that agreeing does not.

**`ownKeys` reports strings only**, which is what `own_keys` is for; a symbol the trap lists
is dropped there and belongs to `getOwnPropertySymbols`, exactly as for an ordinary object.

**The hot path stayed out of it.** The proxy checks went on the `Object.isExtensible` and
`Object.preventExtensions` *natives*, not on the `is_extensible` and `prevent_extensions`
helpers — those are on the element-write path, where the cost is one shape lookup and has to
stay that way. A proxy never reaches them: `proxy_store` answers first.

## D-203

**A relative index is coerced, and a throw from the coercion is the answer.**

Status: Accepted

`relative_index` — shared by `slice`, `splice`, `toSpliced`, `copyWithin`, `fill` and
`String.prototype.slice` — read its argument with `as_number` and fell back to the default
when that answered `None`. `None` covers `undefined`, but it also covers a string, an object
and a symbol, so `[1, 2, 3].slice("1")` started at zero and a `valueOf` that throws never ran
at all.

**Only `undefined` takes the default.** That is what makes `slice(1)` and `slice(1, undefined)`
the same call, and it is the whole of the rule — `null` is zero, a string converts, and a
symbol is an error. The same applied to `splice`'s delete count beside it.

This is the third cluster of the same shape, after the length read (D-189) and the positional
arguments (D-191): a conversion written as `as_number().unwrap_or(…)` is a silent default
wherever the specification has a coercion, and the tests that catch it are the ones asserting
that a throwing `valueOf` is *reached*, not the ones checking the ordinary value.

## D-204

**A string the engine will not build is an error, not an attempt.**

Status: Accepted

Coercing the count (D-203) turned a silent wrong answer into a real request for a gigabyte.
`"a".repeat("1e9")` used to read the count as zero, because `as_number` on a string answered
`None`; once it actually converted, `String::repeat` was asked for a billion characters and
the acceptance run **hung rather than failed** — forty-five minutes in a step that takes
ninety seconds.

`MAX_STRING_UNITS` is V8's limit, which is the number everything in the wild is written
against. Every engine has one; the difference is whether it says so before trying or dies
after. `repeat` and `padStart`/`padEnd` now check before allocating.

**A hang is the worst failure a test run can have**, worse than a wrong answer: it reports
nothing, holds a runner, and looks like infrastructure. The lesson is narrower than "add
limits" — it is that a conversion which starts *working* makes previously unreachable code
reachable, and the code on the other side had never been asked for anything absurd before.

## D-205

**Neither harness waits for ever.**

Status: Accepted

A case that loops took the whole run with it. `Command::output` has no timeout, so one hung
program held a CI job for forty-five minutes of a ninety-second step — twice — and reported
nothing at all. A hang is the worst failure a test run can have: no name, no output, and it
reads as broken infrastructure rather than as the bug it is.

Both harnesses now spawn, poll, and kill. In the corpus runner a killed case reports as a
**crash**, which is the honest category — it built, it ran, and it did not come back — and
crashes are already asserted empty, so the run fails with the case's path rather than
stopping. In the acceptance suite it fails with the program's name and whether it was the
GC-stress pass.

The child is killed *before* the panic, not after. A panic alone leaves the process running,
which is the runner-holding half of the problem.

**Ten seconds for a corpus case and sixty for an acceptance program.** The second is generous
because every acceptance program is run twice and the second pass collects on every
allocation, which is genuinely slow; the first is not, because a compiled case is
milliseconds of work and anything beyond that is stuck rather than slow.

**What made this urgent was a fix working.** Coercing a count (D-203) made
`"a".repeat("1e9")` a real request where it had been a silent zero — so code that had never
been handed an absurd value started receiving them. The lesson generalises past strings: a
conversion that begins converting reaches code that was previously unreachable, and that code
has never been tested with what the conversion now produces.

## D-206

**A function written inside a `try` is lowered with no handler in scope.**

Status: Accepted

`[1, 2].slice({valueOf: function () { throw new RangeError("x"); }})` did not throw. It spun,
for ever, and took a CI job with it.

The three jump-target stacks — `break`, `continue`, and the exception handler — were fields of
the whole lowering rather than of the function being lowered. A nested function therefore
began with whatever the enclosing one had pushed, so a `function` written between `try {` and
`}` lowered its `throw` as a jump to the enclosing function's **catch block**.

That is wrong twice over. It is wrong about the language: an enclosing `try` catches a throw
from a function it contains at the **call**, not at the throw, and the two are different
places — the second example in the regression test calls the function after the `try` has
finished, where there is no handler at all.

And it is wrong in a way nothing could see. A `BlockId` names a block *within one function*
and carries nothing that says which, and block numbering restarts per function — so the
enclosing function's catch block id also named a real block in the nested one. The verifier's
`NoSuchBlock` check passed. The jump went somewhere valid and wrong. Where it landed on the
very block doing the jumping, Cranelift emitted `b .`.

**So the fix is where the state lives, not a matching pop.** The stacks moved onto `Scope`,
which is pushed per function; a nested function and an arrow both start empty because that is
the only thing an empty `Vec` can do. A save-and-restore pair at the top of `lower_function`
would have fixed this exact bug and left the next one available.

**What this says about the verifier**: it cannot catch a cross-function block reference,
because the type it would have to check does not carry the function. Adding the check would
mean giving `BlockId` a `FunctionId`, which is a real option and a larger change than this
bug justifies on its own — recorded here so the next reference of this kind is not diagnosed
from scratch.

**And what it says about the corpus**: a hang is not a slow case. `a || b` where `a` throws
inside a callback, `assert.throws(TypeError, function () { … })` — test262's most common
shape is a function written inside something that catches, and the engine could not run it at
all.

## D-207

**Not everything callable can be constructed.**

Status: Accepted

`new Array.prototype.find()` is a `TypeError`, and the engine answered an object. test262 says
so in a case of its own for very nearly every built-in method it covers — `not-a-constructor.js`
is one of the largest single families in the corpus — and it asks twice: once with `new`, and
once through `isConstructor`, which is `Reflect.construct(function () {}, [], f)` and reads the
answer from whether that threw. `Reflect.construct` did not exist, so the second question was a
missing function rather than a wrong answer.

**The answer is read off the function index already in internal zero.** The native tables share
one negative index space in a fixed order, so the index says which table a function came from,
and a prototype method's table never holds a constructor. Nothing is stored per function. A flag
beside the index would have been clearer to read and would cost eight bytes on every closure a
program allocates, which for a language where closures are how everything is expressed is the
wrong eight bytes to spend.

`Math`, `JSON` and `Reflect` stopped being callable at all. They were given the plain-object
constructor by the helper that makes a namespace exist, which is right for `Object` and `Array`
— both are namespaces *and* constructors — and wrong for the three that are only namespaces:
`typeof Math` answered `"function"` and `new JSON()` answered an object.

**A compiled function is a constructor here and an arrow is not one in the language.** Telling
them apart needs the frontend to record which it lowered, because by the time a closure exists
the two are the same object. That is a change to the closure ABI rather than to this rule, and
is not made here.

## D-208

**`argv` is traced because its values are still live, not because it is a frame slot.**

Status: Accepted

D-94 says arguments need no rooting of their own, "because the collector reads frame slots
through the stack maps". That is true of the *values*, which are live SSA values across the
call and therefore in the map. It is not true of the **slot**: nothing scans those words.

The distinction had never mattered, because nothing allocates between laying the arguments out
and the callee taking them — a compiled callee's prologue reads them into locals, and a native
calls `live_values` before its first allocation. Reordering `new` to do its allocation after
`build_arguments` broke that, and `new P({y: 1}, {z: 2})` read `NaN` under stress: both
arguments were collected while the receiver was being made.

So the order in `Op::Construct` is load-bearing and is written down as such. Allocate the
receiver first, resolve the body second, lay the arguments out last — which keeps every
argument a live value over the one call that can collect.

**The general rule, which D-94 should have said**: a value is traced while something the stack
map knows about holds it. A frame slot is not that. Anything that allocates between filling
`argv` and entering the callee has to root what it wrote there.

## D-209

**The rest of the well-known symbols, and the two string methods that need a pattern.**

Status: Accepted

Five well-known symbols existed and eight did not. A `PropertyKey` carries a symbol's address
now, so the note saying they were "present but not yet usable as property keys" had stopped
being true and the list had not grown with it.

**A program branches on whether `Symbol.species` exists far more often than it uses it.** That
is what a feature test is, and one that reads `undefined` sends the program down a path
written for an engine from before the symbol existed — so the missing eight were not eight
missing features but a wrong answer to thirteen questions. Defining the symbol and acting on
it are separate; this does the first and does not pretend to have done the second.

They are defined frozen, which is what `Object.getOwnPropertyDescriptor(Symbol, "iterator")`
reads and what the specification says.

`String.prototype.match` and `String.prototype.search` were missing outright, which is a
`TypeError` rather than a wrong answer — the largest single bucket in the corpus report is
"is not a function". `match` answers **two shapes**: a global pattern gives the matched text
and nothing else, a non-global one gives what `exec` gives, and a program written for one and
handed the other reads `undefined` where it expected a group. `search` does not touch
`lastIndex`, so asking twice answers the same — which `exec` deliberately does not.

Both take a string argument as a *pattern* rather than a literal, which is the one thing about
them a reader coming from `indexOf` gets wrong, so it is a test rather than a comment.

`exec` and `match` build the same array through the same function now. They are compared
against each other in the corpus, so two builders would have been two chances to disagree
about `index`.

## D-210

**`Date.prototype.toString` is not the ISO form.**

Status: Accepted

It answered `1970-01-01T00:00:00.000Z`, which is `toISOString`'s spelling. The specification
gives `toString` a different one — `Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal
Time)` — and the difference is not cosmetic: `String(date)` is what a date turns into in every
concatenation, and `Date.parse(String(d))` is how a program round-trips one. An engine that
prints a form no other engine prints breaks both quietly.

`toUTCString`, `toDateString`, `toTimeString` and the two `toLocale*` partners did not exist
at all, which is a `TypeError` rather than a wrong answer.

**The zone is UTC and everything agrees on that.** `getTimezoneOffset` answers zero, so the
printed offset is `+0000` rather than the host's — printing a local offset beside an accessor
that says there is none would put a program that reconstructs a date from its own output an
hour out.

The day and month names are tables, which is exactly the shape of thing that is right for one
entry and wrong for the next, so the tests walk all seven and all twelve rather than sampling.

## D-211

**A global that is a function inherits from `Function.prototype`, and `new` through a bound
function constructs.**

Status: Accepted

Two gaps that only showed once `new` started checking what it was given.

`Date.bind` was `undefined`. A global is built before `Function.prototype` exists, so nothing
linked the two — which also made `Object instanceof Function` false. The *prototype methods*
had the link, and that is what made it hard to see: `Math.max.bind` worked and `Date.bind` did
not, so a program testing one concluded the other. Linked in a late pass rather than at each
creation, because this is the first point where the object exists and one pass in the right
place beats three that have to be kept in the right order.

`new (Date.bind(null, 0))()` called `Date(0)` instead of constructing it. That is not a near
miss: `Date` called as a function answers a *string*, so the `new` fell back to the bare
receiver and `.getTime` was not a function. A bound function's body now reads the `new.target`
the convention already passes it — which is what that parameter was reserved for (D-94) — and
constructs the target when it is set. The receiver the caller made is discarded on that path:
the target builds its own from its own `prototype`, which a bound function does not have.

**Both were found by the acceptance suite, not by reading.** Making a bound function report
itself constructable is what made calling-instead-of-constructing observable, and the test for
it is what found the missing prototype link one line earlier.

## D-212

**A replacement may be a function, and `$` in a string one means something.**

Status: Accepted

`"abc".replace(/b/, function (m) { return m.toUpperCase(); })` produced
`afunction (m) { return m.toUpperCase(); }c`. The replacement was run through `ToString`
whatever it was, so a function replacer substituted its own source text into the result.

That is not a missing optimisation. A string replacement cannot see a group **as a value** —
only as text — so `s.replace(/(\d+)/, n => n * 2)` has no other spelling, and the callback
form is how every non-trivial replacement in the wild is written.

The callback gets `(matched, …groups, position, whole)`, with `position` in **code units**,
the space every other index in the language is in (D-115). A byte offset would read correctly
for ASCII and wrongly for exactly the strings that make the difference visible.

The `$` patterns were not expanded either: `"$&"` was two literal characters. All of `$$`,
`$&`, `` $` ``, `$'` and `$1`–`$99` now are. **`$` is not an escape for the next character** —
`$x` stays two characters — which matters because a replacement assembled from user text
produces `$&` by accident, and smoothing that over would be a different language.

Two digits are tried before one, so `$12` is group twelve where there are twelve and group one
followed by `2` where there are not.

The string-pattern path shares the same splice rather than keeping `str::replace`. One
substitution rule, not two — and an empty needle advances by a character, without which
`"ab".replaceAll("", "-")` did not terminate.

## D-213

**A getter is called on read, so it cannot be lowered as a property holding a function.**

Status: Accepted

`({get x() { return 1; }}).x` answered the function. The object-literal lowering read the
property's *name* and *value* and ignored its **kind**, so `get x() {…}`, `set x(v) {…}` and
`x: …` were the same thing — and a class body did the same with `get x() {…}`.

This is the failure D-59 exists to prevent, and it slipped past because the rule it states is
about constructs the compiler does not *understand*. The parser understood this one perfectly.
What went missing was a field on a node the lowering was already reading, which no refusal
could have caught: the compiler had no reason to think anything was absent.

`Op::DefineAccessor` carries both halves, because `{get x() {…}, set x(v) {…}}` is one
property with two functions on it. It routes through `defineProperty` like
`__defineGetter__` does, so the three ways of making an accessor cannot disagree about what
one is.

**And `defineProperty` was rebuilding the pair from the descriptor alone**, so defining the
setter erased the getter. A partial accessor descriptor merges with the one already there —
which is what `validate_and_apply` in `crisol-builtins` has always said and what the caller
in `crisol-abi` was not doing. Distinguishing "absent" from "present and `undefined`" is part
of that: `{get: undefined}` clears the getter and omitting `get` keeps it.

**Still missing, and recorded rather than half-done**: a computed accessor name
(`{get [k]() {…}}`) is refused, because the operation carries a `PropertyKey` and not a value;
and a class body's accessors come out enumerable, where the specification says a class member
is not.

## D-214

**The array methods are generic over anything with a `length`.**

Status: Accepted

`Array.prototype.reverse.call({0: 1, 1: 2, length: 2})` did nothing. Eighteen methods opened
with `let Some((array, length)) = elements_of(this_value) else { return … }`, which is the
fast path written as though it were the only path — so every one of them was a silent no-op on
the receivers test262 spends a whole family of cases on.

Two consequences, and the second is the worse one. Reading `length` from an object **can
throw**, from a getter or from a symbol that refuses to be a number, and a method that bailed
before reading it swallowed that. `indexed_length` already propagates; the methods now use it.

`indexed_get` existed and `indexed_set` did not, which is why the reads were already generic
and the writes were not. Both now branch on `elements_of` internally, so a real array keeps
the element path and pays one test per element — which is what generality over a plain object
costs when the fast case has to stay fast.

**`length` is written back even when nothing moved.** `push()` with no arguments and `pop()`
on an empty receiver both assign it, and on a receiver that refuses the write that assignment
is where they throw. Returning early skipped it.

`reduce` on an empty array with no initial value is now a `TypeError` rather than `undefined`:
there is no value to answer with, and inventing one makes the mistake quiet where the
specification is loud. `reduceRight` already did this, which is how the disagreement was
spotted.

## D-215

**The methods that were simply absent.**

Status: Accepted

"TypeError: is not a function" is the largest single reason in the corpus report, and it is
not one problem: it is a list. These are the ones on it that are small.

`String.prototype.codePointAt` is **what `charCodeAt` is not** — the whole character rather
than half of a surrogate pair, which is the difference between counting storage and counting
text. `substr`'s second argument is a *count*, where `substring`'s is an end and `slice`'s is
an end that may be negative; three methods that look alike and disagree at every edge, so each
is tested against the others rather than alone.

`localeCompare` answers **code-unit order**, which the specification permits and this note
exists to stop anyone believing otherwise: a real collation is locale data the engine does not
carry. What the corpus checks is that the answer is consistent and correctly signed.

`isWellFormed` is always `true` and `toWellFormed` is the identity, and that is **a property
of the representation rather than an optimisation**: a string is a Rust `str`, which is UTF-8
and cannot hold a lone surrogate, so there is no ill-formed string for either to find. The
place this becomes interesting is a UTF-16 string type, which is a much larger change.

`Map.groupBy` differs from `Object.groupBy` in exactly one way and it is the reason both
exist: **its keys are values**, so grouping by `1` and by `"1"` collides in an object and not
in a map, and grouping by an object is only possible through this one.

`Error.isError` reads the mark an error was made with rather than asking `instanceof`, which
gets a cross-realm error wrong in one direction and a re-prototyped one wrong in the other.

## D-216

**`split` looks at its separator.**

Status: Accepted

`"a1b".split(/[0-9]/)` answered `["a1b"]`. The separator went through `ToString` whatever it
was, so a regular expression became the literal text `/[0-9]/` — which is never in the
subject, so the method answered the whole string and looked like a working call. That is the
worst shape of wrong: no error, a plausible result, and a program that only notices when the
data changes.

Three things the walk has to get right, and each is a test rather than a comment because each
is a place another engine and this one could quietly differ:

- **The captures go into the result.** `"a1b".split(/([0-9])/)` is three elements, and a group
  that did not participate is `undefined` rather than `""`.
- **A zero-width match where the cursor already is contributes nothing**, and a match starting
  at the end is past the last position the specification looks at. Without both,
  `"ab".split(/(?:)/)` gains a trailing `""`.
- **An empty subject is decided by whether the pattern matches it**, not by the walk — which
  never runs. `"".split(/x/)` is `[""]` and `"".split(/(?:)/)` is `[]`.

`limit` was not read at all. It truncates, and zero is an empty array rather than everything.

**What is still missing, and is the next piece of work**: `RegExp.prototype[Symbol.split]`,
and the same for `match`, `replace` and `search`. The string methods do the work themselves
rather than delegating to the pattern, so a `RegExp` subclass that overrides one is ignored
and a user-supplied `exec` is never called. That is most of what remains in the `RegExp`
areas of the corpus report.

## D-217

**A walk over an array-like is bounded by `ToLength`, and stops when a getter throws.**

Status: Accepted

Making the array methods generic (D-214) made one of them hang. `Array.prototype.reverse` on
`{length: 2 ** 53 + 2}` walked two billion positions and was killed by the case timeout —
which reported it as a crash, and one crash fails the corpus job.

Two things were wrong and only together do they explain it.

**The clamp was an array's, not an object's.** `indexed_length` stops at 2^32-1 because no
array can be longer; a plain object's `length` is whatever it says, and `ToLength` stops at
2^53-1. test262's case puts a throwing getter at index 2^53-2 precisely so that the *first*
step of the reverse reaches it — a walk clamped to four billion never gets there, and reads
`undefined` two billion times instead. `walk_length` is the second question asked separately:
it bounds a walk, where `indexed_length` bounds an allocation and the smaller clamp is the
whole point.

**And a throwing getter did not stop the loop.** `indexed_get` answers the exception signal
like any other value, which is right for a caller that stores it and wrong for a loop that
keeps going. `indexed_get_checked` is the same read with the answer the walkers need.

So the bound on these loops is no longer arithmetic — a length of 2^53 that never throws does
not finish. That is what the specification says to do, and what every engine does; the case
timeout (D-205) is the backstop, and it is the thing that turned this from a held runner into
a named failure.

## D-218

**A string method asks the pattern, and the pattern asks `exec`.**

Status: Accepted

`String.prototype.match` is not defined as "match a regular expression". It is defined as
calling `pattern[Symbol.match](this)`, and `RegExp.prototype[Symbol.match]` is defined as
calling `this.exec(…)`. Two hooks, and neither existed: the string methods did the matching
themselves, so a `RegExp` subclass that overrode either was ignored and an object that is not
a pattern at all could not stand in for one.

That is invisible until somebody overrides something, which is what makes it worth fixing
rather than noting. It is also a large share of what was left in the corpus: `RegExp.prototype`
is forty cases and most of them are the protocol.

`RegExp.prototype[Symbol.match]`, `[Symbol.search]`, `[Symbol.replace]` and `[Symbol.split]`
are new functions rather than aliases — unlike `Symbol.iterator`, which *is*
`Array.prototype.values` and must be the same object — because there is no named method on
`RegExp.prototype` that does any of these.

The string methods ask through `GetMethod` and fall through to the built-in path when there
is no such method, which is how a plain string separator still works: the same rule, not an
exception to it.

**Two details the tests pin because another engine and this one could quietly differ.** An
empty match has to be stepped over by hand, or the next `exec` finds it again for ever. And
`[Symbol.search]` puts `lastIndex` back, so asking twice answers the same — `exec` moves the
cursor by design and `search` must not.

**And a rooting bug this found**: `native_function` hands back an unrooted handle, so the
function has to reach the prototype before anything else allocates. Building its `name` first
freed it under stress, and `typeof` then answered `"object"` because what came back was a
different cell. `define_method` had the same note; the new installer did not.

## D-219

**A symbol is a property key where a descriptor is involved too.**

Status: Accepted

`Object.defineProperty(o, Symbol.iterator, …)` was a `TypeError` — on the one property a
program is most likely to define that way. Both it and `getOwnPropertyDescriptor` turned the
key into text with `to_text`, which refuses a symbol by design, so a symbol-keyed property
could be created by assignment and then neither redefined nor described.

The key goes through `key_of` now, which knows both spellings. The *string* is kept alongside
it, but only for the two questions that are genuinely about names: whether this is `length`,
and whether it is an array index. A symbol is neither, so both are skipped rather than
answered wrongly.

`own_property` and `define_ignoring_writability` grew keyed twins for the same reason
`define_keyed` exists (D-193): the named forms take a `&str`, and routing a symbol through its
description made two symbols the same property.

## D-220

**A date setter converts its arguments, in order, before using any of them.**

Status: Accepted

`d.setDate({valueOf: () => 3})` produced `Invalid Date`. The setters used `to_number`, which
answers `NaN` for an object without asking it anything — so a `valueOf` was never called and
one that threw was swallowed.

Three things the order gets right, and each is a test. The **receiver** is checked before a
single argument is converted, so `Date.prototype.setDate.call({}, o)` throws without reaching
`o.valueOf`. Every argument is converted before any is used, which is observable whenever two
of them have effects. And the receiver's handle is re-read afterwards, because a conversion
runs user code that can collect.

**And the arguments are rooted before the first conversion, not during it.** They live in the
caller's frame slot, which nothing scans (D-208) — so converting the first freed the second
and third, and `d.setHours(a, b, c)` reported that it could not convert an object to a
primitive. That is the third time this exact hazard has appeared; the rule is in D-208 and it
is worth reading before writing any built-in that converts more than one argument.

## D-221

**An iterating method binds `this` to its second argument, and a throw from the callback stops it.**

Status: Accepted

`[11].map(fn, o)` runs `fn` with `this === o`. All seven iterating methods — `map`, `filter`,
`forEach`, `find`/`findIndex`, `findLast`/`findLastIndex`, `every`, `some` — passed the *array*
as the callback's receiver and ignored the `thisArg` argument entirely. A callback that read
`this` got the wrong object, silently, because the call still happened and still returned
something.

They also did not propagate a throw. `call_value` answers the exception signal like any other
value, so a callback that threw had its signal stored as a mapped element (or read as a truthy
verdict) and the loop carried on. A throw now stops the walk and reaches the caller — and the
element reads go through `indexed_get_checked`, so a throwing getter on an array-like does too.

And `Array.prototype.values.call(undefined)` made an iterator over `undefined` that answered
`{done: true}` on the first `next` — a working empty walk where the specification's
`RequireObjectCoercible` demands a `TypeError`. `keys`, `values` and `entries` reject a nullish
receiver before allocating anything.

**Not done here, and it is why some `filter`/`slice` cases still fail**: the species protocol.
`filter` and `map` build their result with the plain array constructor rather than
`this.constructor[Symbol.species]`, so a subclass gets a plain `Array` back and the
`create-species-*` cases — which poison that lookup to observe it happening — see nothing to
throw. That is a larger change than this batch and is left for one of its own.

## D-222

**The species protocol is consulted, even though its result is discarded.**

Status: Accepted

`filter`, `map`, `slice` and `splice` build their result through `ArraySpeciesCreate`, which
reads `originalArray.constructor`, then `constructor[@@species]`, then constructs from whatever
that resolves to. test262 has a family of cases — `create-ctor-poisoned`,
`create-species-poisoned`, `create-species-abrupt`, `create-species-neg-zero` — that make each
of those steps observable and assert it happens *before* the method touches an element (a
poisoned lookup asserts the callback's call count is still zero).

None of that was happening: the four methods went straight to building a plain array.

**crisol cannot subclass `Array`** — `class X extends Array` is refused — so a species that
resolves to a real `Array`, which is every non-throwing realistic case, produces an array
indistinguishable from `ArrayCreate(length)`. So `array_species_create` is called for its
*observable lookups* — the two property reads, either of which may be a throwing getter, and
the constructor call, which may throw or may not be a constructor — and its result is
**discarded**; the method builds the array it returns as before.

That is a real deviation: a non-throwing custom species constructor has its return value
ignored where the specification would use it. It has no observable consequence while `Array`
cannot be subclassed, and it is written down here rather than hidden. When subclassing lands,
this becomes "thread the species result through as the output array", which is a larger change
to how each method stores — the reason it is not done now.

The lookup order is the specification's and is load-bearing: `.constructor` before `@@species`
before the construct, and all of it before the first element read — which is what the
call-count-zero assertions check. `splice(0, -0)` constructs with a single `+0` argument, which
`create-species-neg-zero` checks exactly.

## D-223

**A method that reads a receiver's internal slot throws when the slot is absent.**

Status: Accepted

`Date.prototype.getFullYear.call({})` answered `NaN`; `Boolean.prototype.toString.call(1)`
answered `"true"`. Both should be a `TypeError`. The pattern is `thisTimeValue` /
`thisBooleanValue`: a method that reads `[[DateValue]]` or `[[BooleanData]]` throws before
reading when the receiver does not have that slot — and test262 has a `this-value-non-*` case
for nearly every one.

The distinction the check preserves is the point: `NaN` is a *real* Date holding an invalid
time (`new Date(0/0).getTime()` is `NaN`, not a throw), and `false` is a real Boolean. Only the
absence of the slot — the hidden `__time` property, or a wrapper whose stored primitive reads
back as a boolean — is a type error. Every Date getter, `getTime`/`valueOf`, `toISOString`
(a `TypeError` for a non-date, still a `RangeError` for an invalid one) and the four `toString`
printers now go through `require_time`; `Boolean.prototype.toString`/`valueOf` through
`require_boolean`.

**And a correction to D-222**: `ArraySpeciesCreate` with a non-object, non-nullish constructor
— `a.constructor = 1` — must throw, because `1` is neither replaced by a species (it has none)
nor the default. The first cut defaulted `C` to `undefined` and used `Array`; `C` now starts as
the constructor itself and falls through to the `IsConstructor` check, which is what
`create-ctor-non-object` requires.

## D-224

**A Map or Set method throws on a receiver that is not one — including the other one.**

Status: Accepted

`Map.prototype.has.call(1)` answered `false`; `Map.prototype.get.call(new Set())` answered
`undefined`. Both are a `TypeError`: `thisMapData`/`thisSetData` check for the internal slot
before doing anything, and test262 has a `this-not-object-throw` and a
`does-not-have-mapdata-internal-slot` (called on a Set) for each method.

Map and Set share one backing store (`__entries`), so "is a collection" was not enough to tell
them apart — a Map method on a Set has to throw. Each is now branded on construction with a
distinct hidden marker (`__mapData` / `__setData`), and the methods check it: `require_map` for
`get`/`set`/`has`/`delete`/`forEach`, `require_set` for `add`/`has`/`delete`/`forEach`.

`clear` is the exception: it is **one function on both prototypes** and cannot tell which it was
reached through, so it requires only "a collection". That still rejects every non-collection
receiver — the case a program actually hits — and misses only `Map.prototype.clear.call(aSet)`,
which is harmless (both empty the same store) and which no reachable corpus case checks.

The brands are two names rather than one kind number so the test is a plain `own_flag` presence
check with no float comparison. `Map`/`Set` `keys`/`values`/`entries` are still absent — those
are the iterator protocol, a separate piece of work.

## D-225

**A string method coerces its receiver, and the search methods honour their position.**

Status: Accepted

`String.prototype.codePointAt.call(undefined)` answered a value and
`String.prototype.includes.call(Symbol())` answered `false`. Both are a `TypeError`:
`ToString(RequireObjectCoercible(this))` rejects `null`/`undefined` at the first step and a
symbol at the second. `this_text` answered `"undefined"` for the first and empty for the
second — a working call where there should be a throw. `coercible_text` returns the throw, and
the failing methods (`codePointAt`, `indexOf`, `lastIndexOf`, `includes`, `startsWith`,
`endsWith`) go through it; a number receiver still coerces, so `"".indexOf.call(1234, "3")`
keeps working.

`includes`, `startsWith` and `endsWith` also ignored their **position** argument entirely.
`"the future".endsWith("future", 10)` was `false`. The position is coerced like any index — a
symbol there throws — and the match is measured in code units: `endsWith` against the slice
ending at its argument, the other two starting at theirs. The comparison moved from byte-based
`str::contains`/`starts_with`/`ends_with` to a code-unit `units_match_at`, so a position that
splits a surrogate pair is counted the way `length` and `charAt` count (D-115).

Not done: the `IsRegExp` guard that makes `"x".includes(/re/)` a `TypeError` before coercing,
and the full `this_text`→`coercible_text` conversion for the ~20 other string methods that
should also `RequireObjectCoercible`. Both are mechanical extensions of this and left for a
sweep of their own rather than mixed in blind.

## D-226

**The rest of the string methods `RequireObjectCoercible`, completing D-225's sweep.**

Status: Accepted

D-225 fixed the six search methods and left the note that ~20 others still answered `"undefined"`
or empty for a nullish or symbol receiver. This is that sweep: every remaining
`String.prototype` method that read its receiver through `this_text` now reads it through
`coercible_text`, so `charAt`, `at`, `slice`, `substring`, `substr`, the two `toLocale*Case`
and case methods, `trim`/`trimStart`/`trimEnd`, `repeat`, `concat` and `split` all throw a
`TypeError` on `null`/`undefined`/symbol and coerce everything else.

**`toString` and `valueOf` are deliberately not converted.** They use `thisStringValue`, which
is stricter than `RequireObjectCoercible`: a *number* receiver is a `TypeError` there, not
coerced to `"5"`. `coercible_text` would coerce it, so those two keep their own reading.
`toWellFormed`/`isWellFormed` already reject nullish through `reject_nullish` and are left as
they are.

Mechanical, one pattern applied 19 times, which is why it is one commit rather than nineteen.

## D-227

**A `Number.prototype` method needs a number receiver, `thisNumberValue`-strict.**

Status: Accepted

`Number.prototype.valueOf.call("5")` answered `5`. It should be a `TypeError`:
`thisNumberValue` accepts only a number or a Number wrapper, where `this_number` coerced
anything through `ToNumber`. The same shape as Boolean (D-223) and the distinction that a
string which *happens* to coerce is still not a Number.

`require_number` replaces `this_number` — a number primitive answers itself, a wrapper answers
the primitive it stores (detected by `as_number` on the shared slot returning `Some`, which a
string or boolean wrapper does not), everything else throws. `toString`, `toLocaleString`,
`valueOf` and `toFixed` route through it; `this_number` had no other caller and is gone.

## D-228

**`every`, `some` and the `find` family `ToObject` their receiver.**

Status: Accepted

`Array.prototype.every.call(2.5, cb)` ran `cb` with the primitive `2.5` as its array argument,
so `obj instanceof Number` was false and a whole family of test262's `this-value` cases failed.
The specification's first step is `O = ToObject(this)`: a primitive is boxed, a nullish receiver
throws. `object_receiver` does that — `to_object` returns a real array unchanged, so the fast
path is untouched — and the three helpers read length and elements from the box and pass it as
the callback's third argument.

The box is rooted for the length read and the whole callback loop, since a boxed primitive is a
fresh object held by nothing else.

**Only `every`/`some`/`find`/`findIndex`/`findLast`/`findLastIndex` this batch** — the ones whose
structure is a plain scan. `map`, `filter`, `forEach`, `reduce` and `reduceRight` want the same
`ToObject` and are the follow-up; they were left out because their result-array and seed logic
makes the change bigger, and one localizable batch at a time is worth more than one broad one
that is hard to bisect when it fails.

## D-229

**The result-building array methods `ToObject` their receiver too, completing D-228.**

Status: Accepted

`map`, `filter`, `forEach`, `reduce` and `reduceRight` now box a primitive receiver through
`object_receiver`, the same as `every`/`some`/`find` in D-228. Held back there because each has
extra structure — `map`/`filter` a result array and a species probe, `reduce` a seed — but the
change is uniform: box once, root the box across the loop, and read length, elements and the
callback's array argument from it while the result array (for `map`/`filter`) stays separate.
The species probe reads the boxed receiver, matching `ToObject` then `ArraySpeciesCreate`.

With this, every callback-taking `Array.prototype` method treats its receiver the way the
specification's first step does, and a nullish one throws before the callback rather than after.

## D-230

**`indexOf`/`lastIndexOf` skip absent indices, compare strictly, and read `fromIndex`.**

Status: Accepted

`Array.prototype.lastIndexOf.call({1: null, length: 2}, undefined)` answered `0` — it read the
absent index `0` as `undefined` and matched. The specification does `HasProperty` at each index
and skips a hole; `indexed_has` (dense arrays by range, everything else through the `in`
operator, which walks the chain like `HasProperty`) is that check.

Two more corrections in the same methods:

- **Strict equality, not `SameValue`.** They compared with `same_value`, which finds `NaN` and
  separates `-0` from `0` — backwards for `indexOf`, whose rule is `===`. And `same_value` as
  written compared *bits*, so `["a", "b"].indexOf("b")` was `-1` for as long as the method
  existed, because two string cells holding `"b"` are different addresses. `strict_equal_bool`
  goes through `crisol_strict_equal`, which compares strings by value.
- **`fromIndex`.** It was ignored. `indexOf` starts there (negative counts from the end),
  `lastIndexOf` defaults it to the last index and walks down. It is coerced after the
  `len == 0` early return, which is the specification's step order and observable through a
  throwing `valueOf`.

`same_value` keeps its other callers (`defineProperty`'s value check, `includes` via
`SameValueZero`), so the rule that is right *there* is untouched.

## D-231

**`includes` is `indexOf`'s SameValueZero twin, and carried the same bugs.**

Status: Accepted

`["a", "b"].includes("b")` was false — the same `same_value`-on-bits bug as D-230, here in
`array_includes`. It also separated `-0` from `0` (SameValueZero treats them equal) and ignored
`fromIndex`.

Rewritten to match D-230's shape: `strict_equal_bool` for the comparison, plus an explicit
`NaN`-matches-`NaN` clause — which is exactly SameValueZero, the rule `includes` takes and
`indexOf` does not. `fromIndex` is read after the `len == 0` early return. The one difference
from `indexOf` is deliberate: `includes` does **not** skip a hole, it reads it as `undefined`,
so there is no `HasProperty` check — only the throwing-getter-propagating read.

## D-232

**Map and Set iterate, over a snapshot, by reusing the array iterator.**

Status: Accepted

`map.keys()`, `.values()`, `.entries()` and the Set equivalents were missing — `for (x of map)`,
`[...set]` and `Array.from(m.keys())` all failed with "is not a function", and it was the
blocker behind several Map/Set cases (including `Map.groupBy`'s tests, which use
`Array.from(map.keys())`).

**Built on the array iterator, not a new one.** Each method collects the current contents into
an array and returns an ordinary array iterator over it. That reuse is the whole point: no new
iterator prototype, no new dispatch entry in the negative-index space — the riskiest thing to
add blind — just three more entries in `MAP_NATIVES`/`SET_NATIVES`, whose offsets are
`.len()`-chained and self-adjust.

**A snapshot, not a live view.** The specification iterates lazily and observes a deletion made
mid-loop; this materialises once. Every ordinary use sees the same result, and only a program
that mutates the collection *while* iterating it can tell — the one corpus case that does
(`delete/does-not-break-iterators`) stays failing, recorded rather than hidden. Closing it needs
the live iterator object that D-194 also wants for arrays.

`Map.prototype[Symbol.iterator]` aliases `entries`, `Set.prototype[Symbol.iterator]` aliases
`values` — the same-object aliasing the array already used. And `%IteratorPrototype%` gained
`[Symbol.iterator]` returning `this` (`iterator_self`), so an iterator is its own iterable and
`Array.from`/spread drain it directly, not only the collection behind it.

## D-233

**`Object.prototype.toString` consults `Symbol.toStringTag`.**

Status: Accepted

It answered `"[object Object]"` for `Math`, `JSON` and `Reflect`, and ignored a program's own
`Symbol.toStringTag` — the code even said so, with a note that symbols could not be keys yet.
D-149 closed that; the note was stale.

`object_to_text` now computes the builtin tag (from the internal slot the receiver carries) as
the *fallback*, then reads `this[Symbol.toStringTag]` and uses it when it is a string — the
specification's order. A non-string tag is ignored; a getter that throws propagates. `Math`,
`JSON` and `Reflect` are given the tag (`"Math"` etc.), which is what
`Object.prototype.toString.call(Math)` reads.

`undefined` and `null` still answer before any lookup — they have no object to read the tag
from. The builtin `RegExp` and `Arguments` tags are still not distinguished (they need a brand
this does not carry), so `Object.prototype.toString.call(/x/)` remains `"[object Object]"` —
recorded rather than pretended.

## D-234

**`RegExp.escape` — a string that matches itself as a pattern.**

Status: Accepted

The ES2025 static was missing. It requires a string (a number is a `TypeError`, not coerced),
and produces a pattern that matches the input literally: the pattern syntax characters take a
backslash, control characters and white space and a set of punctuators become `\xHH`/`\uHHHH`,
and — the one non-obvious rule — a leading letter or digit is hex-escaped so the result can
never begin a quantifier or fuse with what precedes it.

New static method, zero blast radius: a wrong escape only fails `RegExp.escape`'s own tests, it
cannot regress anything else — which is why it was a good one to add without a local compile.

## D-235

**`String.prototype[Symbol.iterator]` — a code-point iterator as a callable method.**

Status: Accepted

`[...s]` and `for (const c of s)` already walked a string by code point through
`crisol_iterate`'s fast path, but `s[Symbol.iterator]` was `undefined` — so a program calling
the method directly, or passing it to `Array.from`, got nothing.

`string_iterator` snapshots the code points into an array and returns an ordinary array
iterator (D-232's approach), wired onto `String.prototype` under the symbol. By code point, not
code unit, so an astral character is one step. With `%IteratorPrototype%[Symbol.iterator]`
(D-232) it is itself iterable, so `Array.from(s[Symbol.iterator]())` works.

## D-236

**A length descriptor value is coerced through `ToNumber`, running `valueOf`/`toString`.**

Status: Accepted

`Object.defineProperty(arr, "length", {value: {toString: () => "2"}})` was a `RangeError`. The
length path used `to_number`, which answers `NaN` for an object without calling `valueOf` or
`toString` — so a length given as a coercible object read as `NaN` and failed the integer
check. `coerce_number` runs `ToPrimitive` (both methods, in order) and propagates a throw from
them, which is what the specification's `ToUint32(ToNumber(value))` requires.

## D-237

**`hasOwnProperty` recognises the properties that have no shape slot.**

Status: Accepted

`[].hasOwnProperty("0")` — after the index was defined — answered `false`, and so did
`hasOwnProperty("length")` on any array. The element check used `as_index`, which reads a
*number* value, but `hasOwnProperty` is called with the *string* `"0"`; the string never
parsed, the element branch was skipped, and a shape lookup that cannot see an element answered
false.

This was the single highest-impact bug in the corpus: test262's `verifyProperty` opens with
`assert(__hasOwnProperty(obj, name), name + " should be an own property")`, and `name` is a
string — so **every** `verifyProperty`-based test on an array element, `length`, or a string
character failed identically, ~127 of them in the sampled set alone.

`has_own_key` now checks the shape slot *and* `derived_own_property` (which knows elements,
`length` and string characters), and handles a symbol key by identity. `Object.prototype.
hasOwnProperty` and `Object.hasOwn` both route through it.

**Still open**: an accessor defined on an array or arguments *index that already holds an
element* is shadowed by the element on read (`arg[0]` answers the element, not the getter),
because the element and the accessor slot coexist and reads check the element first. Closing it
needs the element removed when an index becomes an accessor — element/slot unification, larger
than this and left for its own change.

## D-238

**The local acceptance loop needs `cargo build -p crisol-abi` before `cargo test`.**

Status: Accepted

A runtime change in `crisol-abi` does not reach the acceptance suite through `cargo test` alone.
The suite compiles each JS program to a native binary and links it against
`target/debug/libcrisol_abi.a` — the **staticlib**, which only `cargo build -p crisol-abi`
emits. `cargo test` rebuilds the *rlib* the harness itself uses, not the staticlib the programs
link, so a fix appears to have no effect until the archive is rebuilt. Always
`cargo build -p crisol-abi && cargo test -p crisol --test build`. (The CI has this as a
separate step for the same reason; the local loop has to do it by hand.)

## D-239

**`delete` resolves an index spelled as text, not only a numeric key.**

Status: Accepted

`delete o[key]` took the element branch only when `key` was a *number* (`as_index`, which reads
`Value::as_number`). Called with the string `"0"` — which is how the harness's `isConfigurable`
deletes, and how any `delete o["0"]` reaches the runtime — the branch was skipped, the fallback
shape lookup found no slot (an element has none), and the delete answered `true` **without
removing anything**. Paired with the D-237 `hasOwnProperty` fix this was newly observable:
`isConfigurable` deletes, then asks `!hasOwnProperty`, and the element it failed to remove was
still there, so a configurable element read back as non-configurable and `verifyProperty`
failed. The element branch now also resolves a `String` key through `canonical_index` — the same
canonicalisation the set/load paths already use (`o["01"]` is a property named `"01"`, not
element one). `Reflect.deleteProperty` routes through the same `crisol_delete`, so it is covered.

Known limit: this makes the *last* element genuinely gone (the branch truncates, and for an
array `length == element_count`), which is the common `verifyProperty` shape — a single-element
array's index `"0"`. A non-last element can only be set to `undefined`, and D-64's hole/undefined
conflation still reports it present, so `delete a["1"]` on `[1,2,3]` cannot yet make index one
absent. Real holes are the fix and are out of scope here.

## D-240

**`ArrayBuffer` and the typed arrays, built on a raw byte store.**

Status: Accepted

The corpus histogram put the typed-array family (`TypedArray.prototype`, the constructors,
`DataView`, `ArrayBuffer`, `Atomics`) at the top of the *actionable* backlog — roughly a tenth
of a broad built-ins sample, second only to `Temporal`, which is a far larger standalone feature.
So this lands the family, less `DataView`/`Atomics`/the BigInt-backed kinds, which follow.

Shape. An `ArrayBuffer` is an ordinary object carrying a brand and a **raw byte store** on its
heap cell — a new `Option<Box<[u8]>>` beside `text`, holding no references and traced like it
(nothing), fixed-length like it. A `Vec<Value>` of small numbers would cost eight times the
memory and read each byte through the number-boxing path; the requirement that crisol be tight on
memory made the raw store the only defensible choice. A typed array is an object with a brand and
four hidden properties — buffer, byte offset, element count, element kind — whose integer indices
read and write the buffer through a per-kind little-endian codec (`ElementKind`), the native order
of every target crisol builds for. Both follow the `Map`/`Set` pattern (a brand plus a backing
store), so the value representation did not change.

Integration points, each chosen to stay off the hot path:

- **Indexing** hooks `crisol_property_load`/`_store` in the one branch a digit-leading key takes,
  and only there does it test the brand — a named load (`length`, a method) and every ordinary
  object pay nothing. `crisol_computed_load`/`_store` funnel here because a typed array has no
  `elements` vector for their fast paths to hit.
- **`length`/`byteLength`/`byteOffset`/`buffer`/`Symbol.toStringTag`** are accessor getters on
  `%TypedArray%.prototype`, found by the ordinary chain walk.
- **The generic methods** (`forEach`, `reduce`, `indexOf`, `join`, `reverse`, `fill`, the
  iterators, …) are the *same* `Array.prototype` functions, aliased on: each works through
  `length` and the indices, which a typed array answers from its buffer. The ones that must build
  a typed array rather than an `Array` (`map`, `filter`, `slice`, `subarray`, `set`) are its own.
- **The constructors** join `GLOBAL_NATIVES`, so `is_constructor` and the global binding come for
  free; their prototypes are re-pointed at the shared per-kind objects in `build_globals`, exactly
  as `Array`'s and `Map`'s are. The prototype methods and getters are a new `TYPED_NATIVES` table
  appended **last** in the `crisol_closure_code` dispatch chain, where nothing before it moves.

Two rooting hazards, both GC-stress-only and both found by the macOS/Linux stress run: an argument
array reaches a constructor through `argv`, which the collector does not scan (D-208), so it is
rooted before the fresh buffer is allocated or the copy reads it back as zeroes; and the accessor
pair is built through `self.heap` rather than the free `array_of_values`, whose `with_runtime`
would re-enter the thread-local that `Runtime::new` is still initialising.

Known gaps, deferred: `DataView`, `Atomics`, `BigInt64Array`/`BigUint64Array` (no BigInt), a
resizable `ArrayBuffer`, buffer detachment, `%TypedArray%` reachable as a distinct intrinsic, and
`@@species` on the copying methods. `ArrayBuffer.isView` answers for typed arrays only until
`DataView` exists.

## D-241

**`DataView`, on the same byte store.**

Status: Accepted

The other view over an `ArrayBuffer`, and the next `ReferenceError: … is not defined` after
`Temporal` in a broad sample. It reuses everything D-240 built: the raw byte store, and the
`ElementKind` codec — extended with `read_ordered`/`to_bytes_ordered`, which reverse the bytes for
a big-endian call. That is the one thing a `DataView` needs that a typed array does not: its byte
order is an argument, defaulting to big-endian, where a typed array is always the platform's
native little-endian.

A `DataView` is branded and carries its buffer, byte offset and byte length as hidden properties,
exactly as a typed array does. `buffer`/`byteLength`/`byteOffset` are accessor getters; the
sixteen `getInt8`…`setFloat64` are two shared bodies (`data_view_get`/`data_view_set`) behind
sixteen one-line wrappers a small macro stamps out — the value is coerced before the range is
checked, since its `valueOf` is observable. The constructor joins `GLOBAL_NATIVES`, the prototype
members extend `TYPED_NATIVES` (still last in the dispatch chain, so the typed-array indices before
them do not move), and `ArrayBuffer.isView` now answers for a `DataView` too.

Deferred, as for the typed arrays: detachment, `@@species`, and resizable buffers.

## D-242

**Destructuring, in declarations and `for`-loop bindings.**

Status: Accepted

The largest `refused` cluster after BigInt, and — unlike a missing built-in subsystem — one that
is used *pervasively* through the corpus, so accepting the syntax lets far more than the directly-
counted cases run. `bind_pattern` is a recursive lowering that, given a source value and a
`BindingPattern`, declares (or, for a hoisted `var`, writes) each name:

- **Identifier** — `declare`+`bind`, as a plain `let` does.
- **`x = default`** — the default is taken only when the value is `undefined`, and its expression
  runs only then (it may have effects), through the branch-and-join a conditional uses.
- **Object pattern** — each property read by name, or through the computed path for `{[k]: x}` and
  `{0: x}`; nesting via recursion. Reading a property of a nullish source throws, which is how
  `let {a} = null` is a `TypeError`.
- **Array pattern** — through `Op::Iterate`, the same list `for-of` walks, then indexed. So it
  covers arrays and strings and matches this engine's `for-of`, holes advance without binding, and
  out-of-range reads are `undefined` (so a default fills them).

Wired into `variable_declaration` (the identifier fast path stays, since only it can be written
with no initialiser — `var x;`) and into the `for (const [a] of xs)` / `for (const {x} in o)` loop
binding.

Deferred, each with a note rather than a wrong answer: **rest** (`...r`, in both object and array
patterns — it gathers into a fresh object/array this has no runtime copy for), destructuring
**parameters** and **catch** bindings, and **assignment**-target destructuring (`[a] = x` with no
`let`). Array patterns use `Op::Iterate`'s eager materialisation rather than the specification's
step-by-step protocol, so a `.return()` on early completion is not observed — the same limit the
`for-of` lowering already has. Proper `var` hoisting of pattern names is not done; `slot` declares
on miss, so a decl-before-use `var {a} = …` still binds.

## D-243

**`class extends` and `super`.**

Status: Accepted

One new IR op, `SetPrototype { object, prototype }` (through `crisol-ir`, `crisol-codegen`, and a
`crisol_set_prototype` ABI fn over the existing `set_prototype_of`), plus frontend lowering:

- **The chain.** `class B extends A` evaluates the parent once, then links `B.prototype`'s
  `[[Prototype]]` to `A.prototype` (so an instance inherits the parent's methods and `instanceof A`
  holds) and `B`'s to `A` (so a static call reaches the parent's statics). That is all
  `SetPrototype` is for.
- **`super`.** The parent constructor and its prototype are exposed to the class body as the
  grammar-illegal locals ` super` and ` superproto`, which a method or constructor **captures**
  exactly when it writes `super` — the same closure machinery every other capture uses, so nothing
  new was needed for scoping. `super(...)` calls ` super` with the current `this`; `super.m(...)`
  loads `m` off ` superproto` but calls it with the current `this`; `super.x` reads ` superproto`.
- **The derived implicit constructor.** A derived class with no constructor gets
  `constructor(...) { super(...); }` — built by hand like the base implicit one, but recording
  `this_slot` (the base one does not, which cost an hour: the receiver was never bound, so `this`
  read `undefined` and `super()` initialised nothing) and capturing ` super`.

Simplifications, each a deliberate deviation not a silent gap: `this` is allocated up front from
`new.target.prototype` (crisol's existing model), so `super()` **initialises** rather than
allocates and the this-before-`super` TDZ is not enforced; the implicit derived constructor
forwards **no** arguments (no rest/spread yet), so `new B()` runs the parent but `new B(x)` does
not pass `x`; and `extends` of a native (`Array`, `Error`) does not adopt the exotic behaviour,
since `super()` runs the native as a plain call. Class fields and static members remain unsupported
(noted), and the corpus honesty test now names a field rather than `extends`.

## D-244

**`$262.detachArrayBuffer` and buffer detachment.**

Status: Accepted

test262's detachment tests reach for `$262.detachArrayBuffer(ab)` — the biggest single use of the
`$262` host object, and the "$262 is not defined" line in the corpus. A real engine has the *runner*
inject `$262`; crisol's runner does not, so it is registered as an engine namespace instead
(`NAMESPACE_NATIVES` + `NAMESPACES_ONLY`) — harmless, since no real program names `$262`, and it
unblocks the tests. Only `detachArrayBuffer` is provided; the other `$262` members are not.

Detaching **shrinks the byte store to zero** (`attach_bytes(handle, 0)`) and sets a `__detached`
flag. Most of the observable consequences then fall out of the shrunk store for free: `byteLength`
reads `0`, and every typed-array element read misses (`read_bytes` on a zero-length store) and comes
back `undefined`. The one thing that does not fall out is the view's `length`, which is stored at
construction — so the `length`/`byteLength` getters consult the flag and report `0` when detached.

Deferred: the exact `TypeError`-on-detached that many operations owe (a `DataView` read on a
detached buffer answers `RangeError` from the short store rather than `TypeError`), and the rest of
the `$262` surface (`createRealm`, `evalScript`, `agent`, `global`, `gc`, `IsHTMLDDA`).

## D-245

**The RegExp `v` flag (unicodeSets).**

Status: Accepted

`regress` — the JavaScript-regex crate crisol already builds on — supports unicodeSets in full
(`Flags.unicode_sets`, set operations in character classes, properties of strings). So `v` is not
an engine feature to write but a flag to wire: crisol's `Flags` gains a `unicode_sets` field, `parse`
accepts `v` and rejects `u`+`v` together (the `SyntaxError` the specification names — `v` is a
different dialect of the same mode, not an addition), `flags` reports it in the `dgimsuvy` order, and
`JsRegExp::new` passes it through to `regress`. The instance also gains `unicodeSets` (and the
previously-absent `hasIndices`) as a data property. That clears the "invalid regular expression
flags" cases that were only failing because the flag was refused, and set operations like
`/[[a-z]&&[aeiou]]/v` now match through `regress`.

## D-246

**Generators — the design, and why it is a focused effort rather than one commit.**

Status: Designed, not yet implemented

A generator cannot be a suspended native stack (crisol is AOT — there is no interpreter to
suspend, and locals are Cranelift `Variable`s that vanish on return). But it does **not** need a
codegen change either. The workable shape is a frontend state-machine transform over the existing
ops plus a runtime generator object:

- **`function* g(...)` splits into two functions.** The outer one, called as `g(...)`, allocates a
  generator object (prototype `%GeneratorPrototype%`), stores the parameters and `__state = 0` on
  it, and returns it without running the body. The inner *body* function runs the state machine
  with the generator object as its `this`.
- **The body's locals and resume-state live on that object**, as hidden properties, so they persist
  across suspension. Reading/writing a generator local lowers to a property load/store on `this`
  rather than an `Op::Load`/`Store` — the one invasive frontend change, a "generator mode" in the
  variable path.
- **Entry dispatches on `this.__state`**: a chain of `Compare`+`Branch` (no new terminator) that
  jumps to the block right after the `yield` that suspended.
- **`yield e`** → `this.__yielded = e; this.__state = N; return YIELD_SIGNAL`; the resume block
  reads the sent value from `this.__sent`. **`return e`** → `this.__return = e; return DONE_SIGNAL`.
- **Restriction:** no SSA temporary may be live across a `yield`, so statement-position and
  `x = yield e` are supported and `a + (yield b)` is refused (noted, not miscompiled) until a
  live-value spill lands.

Two prerequisites generators share with any iterable, and the reason this is multi-piece rather
than one commit: **the iterator protocol** (`for-of` today only walks arrays/strings via
`crisol_iterate`; it must learn to drive `Symbol.iterator`/`next()`), and a **lazy `for-of`**
(today it eagerly materialises through `Op::Iterate`, which would run a generator to completion up
front — wrong for an infinite one and for yield timing). Plus the runtime `%GeneratorPrototype%`
(`next`/`return`/`throw`/`Symbol.iterator`). Each is correct or it is silently wrong about laziness,
so it is landed as a deliberate sequence, not rushed.

## D-247

**Generators, implemented — the D-246 design, and the scope that made it landable.**

Status: Accepted

The design in D-246 became feasible on one discovery: `crisol_iterate` **already drains the
iterator protocol** (`iterate_by_protocol`) for anything with a `Symbol.iterator`. So a finite
generator, once it *is* a proper iterable, works in `for-of`/spread/`Array.from` with **no change to
iteration** — the feared for-of rewrite was unnecessary. `next()` steps it lazily; only an infinite
generator in `for-of` stays eager (a noted limit).

What shipped: one IR op `MakeGenerator{body, this}` (ir/codegen + `crisol_make_generator`); the
runtime `%GeneratorPrototype%` and generator object (state/done/sent/yielded/return/this/body as
hidden properties); and the frontend transform — `function*` splits into an **outer** function
(builds the generator, stores the parameters, returns it) and a **body** function whose `this` is
the generator object. The body's locals and parameters live on that object (`declare` marks them,
`read`/`write` redirect to `this."$g_<name>"`, captures excluded since they re-load from the
closure), so a loop counter and a parameter survive a `yield` — the case that makes generators
useful. `yield` stores the value and the next state and returns a signal; the entry dispatches on
the stored state to the block after the suspending `yield`; `return e` finishes with `e`.

Restriction kept from the design: `yield` is handled in statement and simple-assignment position
(where nothing is live across the suspension) and refused in a complex expression position, since
spilling a live SSA temporary across a `yield` is not done. Also deferred: `yield*` delegation, and
`return`/`throw` running the generator's `finally`/`catch` (they finish abruptly). One rooting bug
found under GC stress and fixed the D-208 way — `crisol_make_generator` roots `body`/`this` before
allocating, since the caller holds the body closure only in an SSA value the stack map does not yet
carry, so allocating first collected it and the generator carried an uncallable object.

## D-248

**BigInt — a fifth reference tag, bought by narrowing the payload one bit, with the magnitude in the heap cell's byte store.**

Status: Accepted

BigInt is unbounded, so it cannot be a machine word — it has to be a heap reference like a string
or an object, and a `Value` distinguishes those by a tag. The tag was two bits (object, string,
symbol, singleton), all four values used, and there was no spare bit above the 48-bit payload: the
tagged space below the reserved `e` bit is exactly fifty bits. So the tag became **three bits** and
the payload **47**. Nothing else could give: the exponent and quiet bits are IEEE's, and the `e`
bit is what keeps the canonical NaN on the number side (D-53).

The 47th bit came from the *slot*, not the generation. `GcRef` packed 32 bits of slot and 16 of
generation; it now packs 31 and 16. Two billion live slots is still more than any reachable heap —
the code's own comment already called four billion "more than the address space allows" — while
halving the generation would have halved stale-handle detection, the worse trade. The narrowing is
one chokepoint (`crisol-value`'s masks and `crisol-gc`'s `SLOT_BITS`); codegen never spelled the
tag layout, so it did not move, and the singletons and numbers are bit-identical.

The magnitude lives in the **existing** `bytes` byte store the ArrayBuffer work added, as
`num-bigint`'s two's-complement little-endian digits, distinguished from an ArrayBuffer by the
value's tag. The rejected alternative was a `Box<BigInt>` field on every heap cell: it would spend
eight bytes on every string, array and object to save a small allocation on the rare BigInt
operation — the wrong trade for a niche type when struct size is paid by every cell. `with_bytes`
hands the digits to `num-bigint` without a copy, so a decode is one allocation, not two.

Operators dispatch in the runtime: each arithmetic helper returns early to a BigInt path when either
operand is one (both must be, or it is a `TypeError` — these never coerce across the boundary as `+`
does with a string), and comparison and equality compare by mathematical value. `handle_of` excludes
BigInt so the hundred-plus "is this an object?" tests written as `handle_of(x).is_some()` keep
answering no, while the collector still traces it through `as_address`. The frontend types an
arithmetic result `Number` only when both operands are proven Numbers — otherwise it might be a
BigInt, a reference the collector must root, and typing it `Number` unconditionally (as it once did)
freed it under GC stress. The same operators now propagate exceptions when their result is `unknown`,
since a BigInt operator can throw where number arithmetic never does.

Shipped across four commits: the value re-encoding + every operator; the `BigInt()` function (a
Number converts here via `NumberToBigInt`, the one place it may) and `BigInt.asIntN`/`asUintN`;
`BigInt.prototype.toString`(radix)/`valueOf`/`toLocaleString`; and `BigInt64Array`/`BigUint64Array`
— two new `ElementKind`s (the fixed typed-array arrays grew 9→11, the two BigInt kinds last so the
Number tags held), whose integer-indexed get/set take the BigInt path (`ToBigInt`, where a Number is
a `TypeError` — *not* `NumberToBigInt`, which is the asymmetry that makes `bigIntArray[0] = 1` throw).
The stored bytes are `value mod 2**64`, identical for signed and unsigned; only the read distinguishes
them. Still deferred: `DataView.prototype.getBigInt64`/`setBigInt64` (a separate method surface), and
`~` on a BigInt (out of reach until `~` is supported for Numbers, which it is not).

## D-249

**async/await, as a generator driven by the promise queue.**

Status: Accepted

An async function is a generator whose suspension points are `await` rather than `yield` (D-246),
and the two are lowered through the same machinery: the frontend routes an async function to
`lower_generator` with `is_async` set, lowers each `await e` as `yield e`, and the outer function
calls `crisol_async_start(generator)` — a new one-in/one-out `UnaryOp::AsyncStart` — instead of
returning the generator. That reuses the whole state-machine transform for nothing but the driver
on top, which is the standard "spawn" of async over generators.

The driver (`crisol_async_start` + `async_drive`) creates the promise the function settles, steps
the generator to its first `await`, wraps the awaited value with `Promise.resolve`, and attaches a
resume callback through the existing `promise_then`. The callback is a native carrying the generator
and the result promise as hidden properties — the same pattern `new_settling_function` uses for a
promise's own `resolve`/`reject`. On fulfilment it resumes the generator with the value; on the
generator returning it resolves the promise; on the body throwing (`generator_resume` answers the
exception signal) it rejects. No new microtask machinery: `await` rides the promise queue that
already exists, so ordering ("a" before the await runs synchronously, the caller's code next, the
continuation in the drain) falls out for free.

Scope kept deliberately at the generator transform's: `await` is taken in statement, simple-
assignment, initialiser and `return` position — where nothing is live across the suspension — and
refused in a complex one (`f(await x)`, `await a + await b`), the same restriction `yield` carries.
Deferred: **a rejected `await` rejects the result promise rather than resuming the body's
`try`/`catch`** (that needs resume-with-throw at the suspension point, the same gap a generator's
`throw` has); a bare `return aPromise` is fulfilled with the promise rather than adopting it
(`return await p` resolves first and is the common form); async generators (`async function*`, which
need `Symbol.asyncIterator`); and `for await`. A hoisted async declaration cannot capture a `let`
declared below it — a general hoisting-order property (its body lowers before the `let`), not an
async one; a `var` or a function expression captures normally.
