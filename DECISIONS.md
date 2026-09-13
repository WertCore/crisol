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

**Still conservative in one direction:** any style change marks `LAYOUT`, including one that
only alters a colour. Splitting `ComputedStyle` into layout-affecting and paint-only fields
would tighten that further; it is not done, and is tracked rather than assumed.

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
