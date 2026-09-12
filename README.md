# Crisol

**Run HTML/CSS/JS applications as genuinely native binaries.**

HTML and CSS are the declarative markup layer, compiled to a native UI tree. JavaScript is a
source language, compiled ahead of time to machine code. No Chromium, no WebView, no shipped
interpreter in release builds.

*Crisol* — Spanish for crucible: the vessel where raw material is fused under heat into
something new and solid.

---

## Status

Early. See [`STATE.md`](STATE.md) for exactly where the work is, [`ROADMAP.md`](ROADMAP.md)
for where it is going, and [`DECISIONS.md`](DECISIONS.md) for why it is built this way.

The project runs as two largely independent tracks that converge:

| Track | Crates | What it is |
|---|---|---|
| **A — UI engine** | `ui/`, `renderer/` | HTML/CSS → styled tree → layout → display list → GPU |
| **B — Compiler & runtime** | `compiler/`, `runtime/` | JS/TS/JSX → IR → Cranelift → native code |
| **C — Convergence** | `dom/`, `host/` | DOM host API, React, npm, platform APIs |

Track A is a shippable product on its own: an embeddable Rust HTML/CSS/GPU UI engine with a
document-grade text stack.

## Layout

```
ui/          tree · html · css · style · layout · text · paint · events · a11y · umbrella
renderer/    display-list · wgpu · text-gpu
compiler/    frontend · ir · opt · codegen · diag
runtime/     gc · value · builtins · async · interp
dom/         DOM host API, shared by the AOT and interpreter paths
host/        fetch, fs, storage, clipboard, timers
cli/         crisol build | dev | run | check | package | doctor
tests/       conformance · differential · layout-snapshots · render-snapshots
```

Rust crates are `crisol-<component>`; the CLI binary crate is `crisol`. The umbrella crate
`crisol-ui` re-exports Track A so the UI engine can be consumed standalone.

## Building

Requires the toolchain pinned in `rust-toolchain.toml` (rustup reads it automatically).

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Supported targets: `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`. iOS and Android arm64 are in scope
and checked in CI from the start — see `ROADMAP.md` §3.6 — even though the ports land at M22.
32-bit ARM is not supported.

On Linux you need the X11/Wayland/xkbcommon development headers; see the CI workflow for the
exact package list.

## Non-goals

This is not a browser and owes nothing to the spec. No browser compatibility, no floats, no
CSS columns, no `writing-mode`, no tables-as-layout, no quirks mode, no service workers, no
same-origin policy, no running arbitrary websites.

## Licence

MIT OR Apache-2.0.
