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

