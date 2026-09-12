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

