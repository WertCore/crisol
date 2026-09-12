# Cross-cutting test suites

Tests that belong to one crate live in that crate (`<crate>/tests/`). These four are
suites that span crates or compare against an external oracle, and they get their own
directory because their fixtures are data, not code.

| Directory | Holds | Lands at |
|---|---|---|
| `conformance/` | The `test262` subset and its runner | M12 |
| `differential/` | Programs run through AOT, dev-mode QuickJS and Node, compared | M14 |
| `layout-snapshots/` | Documents with their expected layout rectangles | M3 |
| `render-snapshots/` | Documents with their expected rendered pixels | M4 |

A `.actual.*` file next to an expected snapshot is a failure artifact, written so the two
can be diffed. They are gitignored; do not commit one.
