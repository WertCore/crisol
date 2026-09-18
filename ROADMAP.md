# Crisol — Implementation Roadmap

**Repo:** `github.com/wertcore/crisol`
**Crates:** `crisol-*` on crates.io · **Packages:** `@wertcore/*` on npm
**CLI:** `crisol build | dev | run | check | package | doctor`

*Crisol* — Spanish for crucible: the vessel where raw material is fused under heat into
something new and solid.

**Goal:** Run HTML/CSS/JS applications as genuinely native binaries. HTML and CSS are the
declarative markup layer compiled to a native UI tree. JavaScript is a source language
compiled ahead-of-time to machine code. No Chromium, no WebView, no shipped interpreter
in release builds.

**Status:** Planning. Nothing implemented.

**Audience:** This document is written to be executed across many sessions.
Every milestone has a concrete deliverable and a binary acceptance test so progress is
verifiable without holding the whole system in memory.

---

## 0. How to use this document

Read `DECISIONS.md` and `STATE.md` (both created in M0) at the start of every session.
This roadmap is the destination; `STATE.md` is where you are.

Each milestone below has:

- **Deliverable** — what exists when it is done
- **Accept** — the test that proves it
- **Notes** — traps specific to that milestone

Do not start a milestone before its predecessor's Accept passes. The ordering exists
because each step constrains the next.

---

## 1. Scope

### In scope

- Rust-native UI engine: HTML/CSS → styled tree → layout → display list → GPU
- Text stack good enough to build a document editor on
- AOT compiler: JS/TS/JSX → IR → Cranelift → native code
- Runtime library providing JS semantics (objects, arrays, strings, promises, GC)
- DOM host API bridging compiled JS to the native UI tree
- React support via `react-dom` on a DOM shim
- Desktop: macOS arm64, Windows x86_64, Linux x86_64/arm64
- Mobile: iOS arm64, Android arm64 — **in scope, scheduled later** (see §3.6). Not a
  "maybe someday" target: the architecture must satisfy mobile constraints from M1
  onward even though the ports ship at M22.

### Out of scope, permanently

- Browser compatibility. This is not a browser and owes nothing to the spec.
- Floats, CSS columns, `writing-mode`, tables as layout, quirks mode
- Service workers, WebRTC, history API, multi-tab, same-origin policy
- Running arbitrary websites

### Out of scope, for now

- Vue, Svelte (see §3.5 for why Vue is harder than it looks)
- `eval`, `new Function`
- Node.js API compatibility

---

## 2. Decisions made before any code

These are the load-bearing choices. Changing them later means rewriting large amounts of
work, so they are settled here and recorded in `DECISIONS.md`.

### 2.1 Garbage collection: precise, with Cranelift stack maps

JavaScript needs a tracing GC. Closures capture, objects cycle, promise chains form
graphs. AOT compilation does not remove this requirement.

**Decision:** precise mark-sweep with compiler-emitted stack maps, using Cranelift's
`r64` reference type and safepoint support. Later: generational, with a bump-allocated
nursery.

**Rejected — conservative (Boehm-style):** fast to adopt, but leaks unpredictably and
scanning the Rust stack for false pointers gets worse the more Rust code holds JS values.
Since the whole point is native memory efficiency, a leaky collector undermines the
product claim.

**Rejected — reference counting:** pathological with closures and cycles. Would need a
cycle collector anyway, which is most of a tracing GC with extra steps.

**Deferred — concurrent marking with write barriers (the Go approach):** not rejected, but
not next, and for a reason worth stating because the instinct to reach for it is a good one.

Concurrent collection does not make collection *cheaper*. It makes it **later**: total work
goes up — barriers on pointer stores, synchronisation, re-scanning what changed during the
mark — in exchange for no single long pause. Go pays that willingly because Go targets
servers where a tail-latency spike is a product problem.

Three things make it the wrong first move here:

- **The heap is small.** M8's acceptance is around 10 MiB of process footprint. A
  stop-the-world mark-sweep over a few thousand objects is likely sub-millisecond, so
  concurrency would be solving a problem that may not exist at this scale.
- **A barrier on every pointer store fights the product claim.** The premise is AOT-compiled
  specialised native code; adding a branch to every store to buy latency we have not shown we
  lack is a bad trade for this engine.
- **Mobile's binding constraint is footprint, not pause** (§3.6: memory pressure is a
  termination risk, not a slowdown). Concurrent marking makes footprint slightly *worse*
  through floating garbage.

**Generational comes first** for the opposite reasons: UI work allocates per-frame temporaries
that die young, so a nursery collection touches few live objects and gives short pauses. Note
that generational needs a write barrier too — for old→young references — so this is not
"barriers versus none", it is a narrower barrier buying a larger win.

**And nothing has been measured.** No pause time has been recorded against compiled code,
because the collector cannot yet see compiled frames at all. Choosing a concurrency design to
fix an unmeasured cost is the kind of decision this project records *against*. If young-generation
pauses later hurt frame times on a real workload, the order is generational first, then
concurrent marking if pauses remain too long — and M20 is where that measurement belongs.

**Consequences, accepted now:**

- Every heap-allocated JS value lives behind a `GcRef` handle, never a raw pointer
- Native (Rust) code holding JS values must root them explicitly via a shadow stack
- Codegen emits safepoints at calls, loop back-edges, and allocations
- The FFI boundary is the hardest part; design it in M9 before any host API work

### 2.2 Codegen backend: Cranelift

**Chosen because** it is pure Rust, compiles fast (important for the dev loop), and has
explicit safepoint and stack map support designed for GC'd languages. That last point is
decisive.

**Rejected — LLVM:** better optimizer, but heavy dependency, slow builds, and GC stack map
support is workable but unpleasant.

**Rejected — compile to C:** what Static Hermes does, and it buys portability cheaply. But
precise GC stack maps through a C compiler are awkward, and build times get worse, not
better.

**Required design consequence:** codegen sits behind a `Backend` trait from M13. If M20
benchmarks show the optimizer is the bottleneck, add an LLVM backend for release builds and
keep Cranelift for dev builds — the same split rustc uses. Do not add a second backend
before there are benchmarks demanding it; the GC statepoint work is the expensive part.

Cranelift target coverage (x64, aarch64) satisfies every platform in §1 including iOS and
Android arm64.

### 2.3 Dev builds interpret; release builds compile

AOT compilation plus native linking takes seconds to minutes. Web developers expect
sub-second hot reload. These are irreconcilable, so run two modes:

- `dev` — QuickJS via `rquickjs`, driving the same DOM host API. Fast iteration, HMR,
  real React DevTools.
- `release` — AOT compiled, no interpreter in the binary.

The no-interpreter promise applies to shipped artifacts, which is the only place users
care about it.

**Cost, accepted:** two execution paths that must agree. Mitigated by the differential
test suite in M14, which runs the same programs through both and compares. Treat any
divergence as a P0 bug.

### 2.4 Don't write what already exists

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

The novel work is: the IR, the optimizer, codegen, the GC, the runtime library, the DOM
host API, the document/text API, and the integration of all of it. That is more than
enough.

### 2.5 Text is the core competency, not a checkbox

The eventual targets (PDF editor, document editor) live or die on text. Browsers are bad
at this — every serious web editor (Google Docs, Figma, Notion) abandons `contenteditable`
and renders text itself. That means the webview was never providing the thing these apps
need.

So the text layer is a **public API**, not an internal detail. It must expose shaped runs,
cluster boundaries, cursor affinity, selection rectangles, and line box geometry.

### 2.6 The `Custom` node is a first-class escape hatch

A PDF page or document canvas must opt out of CSS layout entirely while still
participating in hit testing, scrolling, focus, clipping, and accessibility.

Design this in M2, not later. It is the single most important node kind for the eventual
product and the hardest to retrofit.

### 2.7 Performance claims we will and won't make

**Will claim:** small memory floor, fast cold start, small binaries, one rendering engine
identical on every platform, no system webview dependency, works on iOS where JIT is
banned.

**Will not claim:** faster than V8 on hot code. A JIT observes real runtime types and
specializes; an AOT compiler without profile data cannot. Untyped AOT JS is closer to a
flattened interpreter than to optimized native code. Wins come from typed paths and from
eliminating startup and runtime overhead.

Setting this expectation now prevents a disappointing benchmark day later.

---

## 3. Known hard problems

Recorded here so they are not rediscovered as surprises.

### 3.1 The GC/FFI boundary

Rust code holding JS values must not hide them from the collector. Every host API function
that receives or returns a JS value participates in rooting. Get this wrong and you get
use-after-free bugs that appear only under memory pressure, which is the worst possible
failure mode to debug. Design in M9, test with a stress-GC mode that collects on every
allocation.

### 3.2 Proxy, and therefore Vue

`Proxy` cannot be rejected at build time if the ecosystem is a goal: Vue 3's reactivity,
MobX, Immer, Valtio and Solid stores all depend on it. But supporting it means every
property access must check whether the receiver is exotic, which erases the specialization
that makes AOT worthwhile.

**Resolution:** support `Proxy`, but make the check cheap. Object shapes carry an
`is_exotic` bit; the fast path branches on it once. Programs that never construct a Proxy
pay one predictable branch. Do not promise Vue until this is measured.

### 3.3 Transitive build failures

If the compiler rejects a construct, it rejects it in code the developer did not write and
cannot patch — a dependency's dependency using `Reflect.ownKeys`. React Native's ecosystem
pain was a milder version of this and it was still severe.

**Mitigations:** support as much of the language as possible rather than rejecting;
per-package override/shim mechanism; `crisol doctor` reports unsupported constructs across
the whole dependency graph before the build fails; a curated compatibility list.

### 3.4 Dynamic property access defeats specialization

`obj[key]` where `key` is not statically known forces the generic path. This is extremely
common. Inline caches help at runtime but AOT has no runtime feedback loop. Shape-based
lookup with a per-site monomorphic cache is the fallback. Budget for the generic path being
the common path in v1.

### 3.5 React's dev/prod builds differ

`react-dom` ships separate development and production bundles gated on `process.env.NODE_ENV`.
The build must define this at compile time and dead-code-eliminate correctly, or you will
compile the dev build's invariant machinery into release binaries.

### 3.6 Mobile constraints apply from M1, even though the ports ship at M22

iOS prohibits JIT for third-party apps, which is precisely where AOT's value is
unambiguous. Mobile is not a deferred maybe — it is the strongest justification for the
compiler track existing at all. It is scheduled late only because the desktop path
validates the architecture faster.

Consequently, nothing may assume desktop:

- **No `mmap`-with-exec anywhere.** All generated code is AOT and linked, never emitted at
  runtime. This is already true by design; do not let a dev-mode shortcut violate it.
- **Touch, gesture, and soft-keyboard input designed into the event model at M5**, not
  bolted on. Momentum scrolling, touch targets, pointer cancellation, safe-area insets.
- **Renderer must tolerate tile-based deferred GPUs.** Avoid patterns that are cheap on
  desktop immediate-mode GPUs and catastrophic on mobile tilers — mid-pass render target
  switches, frequent readback, large overdraw.
- **Memory pressure is a termination risk, not a slowdown.** The incremental work in M6
  and the "bodies stay in Rust" pattern from M19 matter more on mobile than anywhere else.
- **Windowing abstraction must not assume a resizable desktop window.** `winit` supports
  both; keep platform assumptions out of `ui/` entirely.
- **Cranelift target check:** arm64 covers both iOS and modern Android. 32-bit ARM is not
  supported and is not required.

Treat "would this work on iOS?" as a review question for every architectural decision in
Tracks A and B.

---

## 4. Repository layout

```
crisol/
├── ui/
│   ├── tree/              # crisol-tree      node arena, handles, dirty tracking
│   ├── html/              # crisol-html      html5ever -> tree
│   ├── css/               # crisol-css       lightningcss + selectors
│   ├── style/             # crisol-style     cascade, inheritance, interning
│   ├── layout/            # crisol-layout    taffy integration, custom-node protocol
│   ├── text/              # crisol-text      PUBLIC API: shaping, cursors, selection
│   ├── paint/             # crisol-paint     tree -> display list
│   ├── events/            # crisol-events    hit testing, capture/bubble, focus, IME
│   └── a11y/              # crisol-a11y      accesskit bridge
├── renderer/
│   ├── display-list/      # crisol-display-list
│   ├── wgpu/              # crisol-render-wgpu
│   └── text-gpu/          # crisol-text-gpu  glyphon integration
├── compiler/
│   ├── frontend/          # crisol-frontend  oxc wrapper, module graph
│   ├── ir/                # crisol-ir        SSA-ish typed IR
│   ├── opt/               # crisol-opt       passes
│   ├── codegen/           # crisol-codegen   Backend trait; cranelift impl
│   └── diag/              # crisol-diag      diagnostics with source spans
├── runtime/
│   ├── gc/                # crisol-gc        collector, shadow stack, rooting
│   ├── value/             # crisol-value     NaN-boxing, shapes
│   ├── builtins/          # crisol-builtins  Object, Array, Promise, RegExp...
│   ├── async/             # crisol-async     microtask queue, event loop
│   └── interp/            # crisol-interp    dev-mode rquickjs bridge
├── dom/                   # crisol-dom       host API, shared by AOT and interp
├── host/                  # crisol-host      fetch, fs, storage, clipboard, timers
├── cli/                   # crisol           build | dev | run | check | package | doctor
└── tests/
    ├── conformance/       # test262 subset
    ├── differential/      # AOT vs interp vs node
    ├── layout-snapshots/
    └── render-snapshots/
```

**Naming convention:**

- Rust crates: `crisol-<component>`, with the CLI binary crate simply `crisol`
- npm packages: scoped `@wertcore/crisol`, `@wertcore/crisol-react`, etc. Scoping means bare
  npm name collisions are irrelevant.
- The umbrella crate `crisol-ui` re-exports the Track A crates so the UI engine can be
  consumed standalone, per the note at the end of Track A.

Two tracks run largely independently: **UI** (M1–M8) and **Compiler/Runtime** (M9–M15).
They converge at M16. If working solo, finish the UI track first — it is independently
useful and lower risk.

---

## 5. Milestones

### Track A — UI Engine

---

#### M0 — Skeleton and session state

**Deliverable:** Cargo workspace, CI running `cargo test` and `cargo clippy` on three
platforms, `DECISIONS.md` seeded from §2, `STATE.md` template.

**Accept:** CI green on macOS, Windows, Linux.

**Notes:** `STATE.md` must record current milestone, what was just finished, what is
half-done, and any open questions. This is how sessions chain together.

---

#### M1 — Window and triangle

**Deliverable:** `winit` window, `wgpu` surface, swapchain, resize handling, HiDPI scale
factor, a solid-colour clear and one textured quad.

**Accept:** window opens on all three platforms, resizes without panic or artifacts,
renders correctly at 1x and 2x DPI.

**Notes:** Do not touch HTML yet. Get the platform plumbing and DPI right first — it is
tedious to fix once there is a tree on top.

---

#### M2 — Node tree and display list

**Deliverable:** Arena-allocated node tree with generational `NodeId` handles.
Parent/first-child/next-sibling links (**not** `Vec<NodeId>` — sibling links make
incremental mutation cheap). `DrawCommand` enum. A hand-built tree of nested coloured
rectangles renders through the display list.

**Accept:** manually constructed 3-level nested tree renders at correct positions; removing
a node and re-rendering produces the expected output.

**Notes:**
- Define the `CustomNode` trait here (§2.6): `measure`, `layout`, `paint`, `hit_test`.
  Implement a stub that draws a fixed-size coloured box.
- No parsing, no CSS. Prove the core data flow in isolation.

---

#### M3 — CSS and layout

**Deliverable:** `lightningcss` parsing, `selectors` matching, cascade with specificity and
inheritance, `ComputedStyle` interned behind `Arc` and shared across nodes. `taffy`
integration producing layout rectangles.

Property subset: `display`, `position`, `width`/`height`/`min`/`max`, `margin`, `padding`,
`border`, `flex-*`, `gap`, `justify-content`, `align-items`, `color`, `background-color`,
`border-radius`, `opacity`, `overflow`, `visibility`, `font-*`, `line-height`.

**Accept:** layout snapshot suite of 40+ cases passes; interning verified by asserting that
100 identically-styled nodes share one `ComputedStyle` allocation.

**Notes:** Interning is not an optimization to add later. Per-node `ComputedStyle` at
document-editor scale is hundreds of megabytes, which contradicts the entire product thesis.

---

#### M4 — HTML and text

**Deliverable:** `html5ever` producing the node tree. `cosmic-text` shaping, `glyphon`
rendering. Font loading and fallback chains. Line breaking.

**Public text API** (§2.5): shaped run access, cluster boundaries, `point_to_cursor`,
`cursor_to_point`, selection rectangles for a range, line box geometry.

**Accept:** renders a paragraph with mixed Latin/CJK/emoji correctly; clicking any glyph
returns the correct cursor index including at cluster boundaries; selection rectangles are
correct across a line wrap.

**Notes:** Budget 2–3x whatever this seems like it should take. Text is where UI engines
quietly die. Bidi can be deferred to M8 but the API must not assume LTR.

---

#### M5 — Events, focus, input, accessibility

**Deliverable:** All four together, because they read the same tree and focus state.

Hit testing (respecting clip and transform), capture/bubble propagation, focus order and
keyboard navigation, text input with IME preedit rendering, mouse/touch/scroll/drag,
clipboard, and the `accesskit` bridge.

**Accept:** a form with three text inputs is fully keyboard-navigable; IME composition
works for Japanese input on all three platforms; VoiceOver/NVDA/Orca announce the tree
correctly.

**Notes:**
- Accessibility here, not in year three. It constrains the tree, focus model, and event
  system, and retrofitting means restructuring.
- Touch input designed in now, per §3.6, even though mobile ships later.

---

#### M6 — Incremental everything

**Deliverable:** Dirty flagging, style invalidation (which selectors can be affected by
which mutation), partial relayout, damage regions, glyph and texture caching.

**Accept:** in a 10,000-node tree, mutating one text node's content triggers relayout of
fewer than 20 nodes and repaints only the damaged rectangle. Instrumented counters prove it.

**Notes:** This is the line between a demo and something you can build a real app on. A
400-page document reflowing on every keystroke is the workload that kills naive engines.

---

#### M7 — Reactive API and component model

**Deliverable:** Signals, effects, a component abstraction, and the mutation API that a
foreign caller (the JS runtime, later) will drive. Design it as if an external consumer
exists, because one will.

**Accept:** a Rust-only todo app with add/remove/filter/edit runs with no full-tree
rebuilds.

---

#### M8 — Platform polish

**Deliverable:** Momentum scrolling, scrollbars, multi-window, native menus, drag and drop,
bidi text, cursor shapes, window chrome, packaging (.app, .msi, AppImage).

**Accept:** a real API-client-shaped application built entirely in Rust, measured at
< 60MB RSS idle with a 5MB JSON response loaded.

**Notes:** Record the memory number. It is the product claim and needs to be defensible.

> **Track A alone is a shippable product:** an embeddable Rust HTML/CSS/GPU UI engine with
> a document-grade text stack. If the compiler track stalls, this still has value.

---

### Track B — Compiler and Runtime

---

#### M9 — GC and value representation

**Do this before the IR.** It constrains the calling convention, the IR, and the ABI.

**Deliverable:** Value representation (NaN-boxing on 64-bit). Hidden-class/shape system
with the `is_exotic` bit from §3.2. Mark-sweep collector. Shadow stack for Rust-held roots.
`GcRef<T>` handle type. Stress mode that collects on every allocation.

**Accept:** a hand-written Rust test builds a cyclic object graph, drops all roots, and the
collector reclaims it. Stress mode runs the full suite with zero use-after-free under ASAN.

**Notes:** §3.1 is the risk. Every host function that touches a JS value participates in
rooting. Make the rooting API hard to misuse — prefer a scope guard over manual
push/pop.

---

#### M10 — Frontend and module graph

**Deliverable:** `oxc` integration: parse JS/TS/JSX, scope and symbol resolution, TS type
erasure. `oxc_resolver` for `package.json` exports, ESM, CJS interop, `node_modules`
traversal. Module graph with cycle handling.

**Accept:** resolves and parses a real `node_modules` tree containing React, producing a
complete module graph with no unresolved imports.

---

#### M11 — IR

**Deliverable:** SSA IR with the ops from the plan (Load, Store, Call, PropertyLoad,
PropertyStore, CreateObject, CreateArray, Closure, Await, Throw, Branch, Compare). Explicit
safepoints. Type lattice: `Unknown | Number | String | Bool | Object(shape) | ...`.
Lowering from the oxc AST.

**Accept:** IR text dump for a set of 30 representative programs is stable and reviewable;
an IR verifier rejects malformed graphs.

**Notes:** The IR must represent safepoints explicitly or the GC integration in M13 will
not work.

---

#### M12 — Runtime library

**Deliverable:** `Object`, `Array`, `String`, `Number`, `Boolean`, `Symbol`, `Map`, `Set`,
`Date`, `Error` hierarchy, `RegExp` (via `regress` for JS-compatible semantics),
`Iterator`/`AsyncIterator`, `Promise` with a proper microtask queue, `JSON`.

Prototype chains, property descriptors, getters/setters, `Proxy` and `Reflect`.

**Accept:** the relevant `test262` subset passes at >80% for implemented builtins.

**Notes:** This is large but mechanical, and the most parallelizable work in the project.
`Promise` semantics are subtle — job queue ordering must match spec or async code
misbehaves in ways that look like race conditions.

---

#### M13 — Codegen

**Deliverable:** IR → Cranelift IR → machine code. Stack map emission at safepoints.
Calling convention. Exception handling (unwinding or explicit result propagation — decide
and record). Object file output and linking. Targets: macOS arm64, Linux x86_64/arm64,
Windows x86_64.

**Accept:** `main.ts` containing arithmetic, closures, classes, and array methods compiles
to a standalone binary that runs and produces correct output on all four targets, with
GC stress mode enabled.

**Calling convention — uniform now, direct fast path next.** Every compiled function takes
`(closure, this, new.target, argc, argv)`, so a call site never needs to know which function
it is reaching. That is not a preference: a callback passed to `arr.map` has no statically
known arity, so a convention with the arity baked in cannot express one at all. `argv` points
into the **caller's stack frame** rather than a heap list — no allocation per call, and the
collector already traces frame slots, so arguments are rooted for free. `new.target` is in the
signature from the start even though nothing reads it until classes, because adding a
parameter later rewrites every call site.

**To do — the direct fast path.** When the callee *is* statically known, the uniform path is
pure overhead: the arguments can go straight into registers and the call can be direct. This
is deliberately not built first, because there is nothing to measure until calls work at all,
and two call paths from day one are two chances to miscompile in a way that shows up on only
one of them. It is **not deferred to M20** — measure against QuickJS as soon as calls run, and
build it inside M13 if the number says so. Note that the first fix for slow calls may not be
this at all: every live variable is currently spilled to the frame at every safepoint, because
the IR cannot yet say which slots can hold references. Narrowing that is the bigger win.

---

#### M14 — Differential testing

**Deliverable:** Harness running identical programs through (a) AOT, (b) dev-mode QuickJS,
(c) Node.js as reference, comparing output. `test262` subset runner. Fuzzing with random
program generation.

**Accept:** 1,000-program corpus with zero divergence between the three.

**Notes:** Per §2.3, any AOT/interp divergence is a P0. This suite is what makes the
dual-mode decision survivable.

---

#### M15 — Async and event loop

**Deliverable:** Microtask queue, `Promise` job draining, `setTimeout`/`setInterval`,
`queueMicrotask`, `requestAnimationFrame`, integration with the `winit` event loop.
`async`/`await` lowering to state machines in the IR.

**Accept:** ordering test suite covering the interleaving of sync code, microtasks,
timers, and rAF matches Node/browser ordering exactly.

---

### Track C — Convergence

---

#### M16 — DOM host API

**Deliverable:** The DOM surface, implemented in Rust over the M2 tree, callable from both
the AOT path and dev-mode QuickJS through one interface.

Minimum: `Document`, `Element`, `Text`, `createElement`, `createElementNS`,
`createTextNode`, `appendChild`, `insertBefore`, `removeChild`, `replaceChild`,
`nodeType`, `parentNode`, `firstChild`, `nextSibling`, `setAttribute`, `removeAttribute`,
`style` (CSSOM subset), `classList`, `addEventListener` with real capture/bubble,
`Event` objects with `preventDefault`/`stopPropagation`.

**Accept:** Preact renders and updates a component tree with no source modification.

**Notes:**
- Never hand JS a raw pointer. Opaque integer handles into a slotmap, per §3.1.
- Batch mutations: mark dirty, run one style/layout/paint pass per frame, never relayout
  per `appendChild`.
- Event fidelity matters — React's synthetic event system misbehaves confusingly on an
  approximate implementation.

---

#### M17 — React

**Deliverable:** Unmodified `react` + `react-dom` running on M16. JSX transform in the
build pipeline. `process.env.NODE_ENV` defined at build time with correct DCE (§3.5).
Fast Refresh in dev mode. `react-devtools-core` over WebSocket.

**Accept:** a non-trivial React app — hooks, context, conditional rendering, lists with
keys, controlled form inputs, `useEffect` cleanup — renders and updates correctly in both
dev and release modes.

---

#### M18 — npm compatibility

**Deliverable:** Tree shaking, cross-module optimization, per-package shim/override
mechanism, `crisol doctor` reporting unsupported constructs across the whole dependency
graph before the build fails (§3.3).

**Accept:** a curated list of 50 common packages (date, validation, utility, state, HTTP)
compiles and passes its own test suite under AOT.

---

#### M19 — Host APIs

**Deliverable:** `fetch` over `reqwest`, `WebSocket`, `localStorage` over a native KV
store, IndexedDB-shaped API over SQLite, File APIs, Web Crypto over `ring`/`RustCrypto`,
`console`, clipboard, notifications, native dialogs.

Plus the `#[native]` attribute for direct Rust↔TS interop with generated `.d.ts`.

**Accept:** an API client application built in React runs entirely on this stack, with
response bodies held in Rust and only viewport slices crossing into JS.

**Notes:** That last pattern is the memory story. A 10MB JSON body should never become a
JS object graph unless the program asks for it.

---

#### M20 — Optimization

**Deliverable:** Inlining, constant folding, DCE, escape analysis, allocation elimination,
type specialization on TS types, shape-based property access with per-site monomorphic
caches, devirtualization, LTO. Narrowing stack maps to reference-typed slots, so a call stops
spilling every live variable. The direct-call fast path from M13, if it was not needed sooner.

**Accept:** benchmark suite showing measured improvement over M13 baseline. Publish
honest numbers per §2.7 — startup, memory, binary size — not throughput comparisons
against V8.

---

#### M21 — Production

**Deliverable:** Source maps from native frames back to TS, debugger integration, profiler,
crash reporting, code signing, auto-updater, asset pipeline, i18n.

---

#### M22 — Mobile

**Deliverable:** iOS and Android targets. The AOT path is the whole point here (§3.6).

---

## 6. Schedule reality

Track A: roughly 12–18 months of focused work to M8.
Track B: roughly 2–4 years to M15 for a small team. This is Static Hermes-scale work.
Convergence: add 6–12 months.

Good tooling changes the constant factor on any given week substantially. It does not
change the order of magnitude, because the work must still be carried across hundreds of
sessions with a human holding continuity.

**Therefore: ship Track A first and independently.** It is useful on its own, it validates
the memory and rendering claims, and it means the project has a product even if the
compiler takes longer than hoped.

---

## 7. Kill criteria

Honest checkpoints. If these fail, change the plan rather than pushing through.

- **After M8:** if idle RSS is not meaningfully below a WebView2/WKWebView baseline for an
  equivalent app, the core product claim is unsupported. Re-evaluate.
- **After M13:** if AOT output is not at least 2x QuickJS on representative application
  code, the compiler is not earning its complexity. Consider shipping with an embedded
  interpreter and keeping only the UI engine as the differentiator.
- **After M16:** if the DOM shim cannot run stock `react-dom` without patches, the
  ecosystem promise fails and the product becomes a Rust-only framework.
- **At any point:** if `crisol doctor` shows that a majority of common npm packages fail to
  compile, §3.3 has materialized and the "single codebase" promise needs rewording.

---

## 8. First session

Start here:

> Read `ROADMAP.md`. We are at M0. Create the Crisol cargo workspace per §4, seed
> `DECISIONS.md` from §2 and `STATE.md` from the template, and set up CI running
> `cargo test` and `cargo clippy -- -D warnings` on macOS, Windows and Linux. Do not start
> M1 until CI is green.

Then:

1. **M0** — workspace, `DECISIONS.md`, `STATE.md`, CI green on three platforms
2. **M1** — `crisol-render-wgpu`: winit window, wgpu surface, HiDPI, textured quad
3. Update `STATE.md` before the session ends

`STATE.md` template:

```markdown
# Crisol — State

**Current milestone:** M0
**Accept criteria:** (copy from ROADMAP.md for the current milestone)

## Done
-

## In progress
-

## Open questions
-

## Decisions made this session
- (append to DECISIONS.md as well)
```

Do not start the compiler track until M6 is done. The temptation will be strong because
the compiler is the interesting part. Resist it — the UI engine is the product that ships.
