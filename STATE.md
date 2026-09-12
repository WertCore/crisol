# Crisol — State

**Current milestone:** M1 — window and triangle (not started)
**Last finished:** M0 — skeleton and session state

Read this before `ROADMAP.md`. The roadmap is the destination; this is where the work
actually is.

---

## Accept criteria for the current milestone

> **M1 — window and triangle.** `winit` window, `wgpu` surface, swapchain, resize handling,
> HiDPI scale factor, a solid-colour clear and one textured quad.
>
> **Accept:** window opens on all three platforms, resizes without panic or artifacts,
> renders correctly at 1x and 2x DPI.

---

## Done

### M0 — skeleton and session state

- Cargo workspace, 26 crates, laid out per ROADMAP §4. Edition 2024, toolchain pinned in
  `rust-toolchain.toml`.
- Workspace-inherited package metadata and lints. `missing_docs` is on everywhere and
  promoted to an error in CI.
- `DECISIONS.md` seeded from ROADMAP §2 and §3 (D-01…D-09), plus the two choices setting up
  the workspace forced (D-10, D-11).
- `README.md`, `.gitignore`, `rustfmt.toml`, and a `tests/` tree with a README per suite
  recording what goes in it and which milestone fills it.
- CI at `.github/workflows/ci.yml`: fmt, clippy `-D warnings`, test and rustdoc on macOS,
  Linux and Windows, plus a `cargo check` of the two arm64 mobile targets on every commit
  so a desktop-only assumption fails the day it is introduced (D-09).

Every crate outside `cli/` and `ui/umbrella/` is a documented stub naming the milestone
that fills it. They exist now so that later work has a home and the workspace layout does
not churn.

---

## In progress

Nothing.

---

## Open questions

- **M0's accept is "CI green on macOS, Windows, Linux".** That cannot be observed until the
  repo is pushed and Actions runs. Confirm it before starting M1 — especially the Linux
  runner, where `winit` will need the X11/Wayland headers the workflow installs.

---

## Decisions made this session

- **D-10** — edition 2024, workspace-inherited metadata and lints, internal crates declared
  with both `path` and `version` so publishing later does not mean touching 26 manifests.
- **D-11** — the umbrella crate gates the renderer behind a `render` feature, so a headless
  consumer does not compile `wgpu` and `winit`.

---

## Next session

1. Read `DECISIONS.md` and this file.
2. Confirm CI is green on all three platforms. M0's accept is a prerequisite for M1, not a
   formality.
3. Start M1 in `renderer/wgpu`. Get the platform plumbing and DPI right before there is a
   tree on top of it — ROADMAP §M1 is explicit that it is tedious to fix afterwards.
