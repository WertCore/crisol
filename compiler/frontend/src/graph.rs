//! The module graph: what imports what, and in what order it runs.
//!
//! Deliberately independent of the parser. ROADMAP §M10 wants `oxc` for parsing and
//! `oxc_resolver` for resolution, and both will feed this — but what a graph *is*, and what a
//! cycle in one means, is decided by the ES module specification rather than by whichever
//! crate read the source. Keeping the two apart means the ordering rules below are tested
//! against hand-built graphs, where a cycle can be constructed in three lines, instead of
//! against a `node_modules` tree where reproducing one is an afternoon.
//!
//! # Cycles are legal
//!
//! This is the part that is easy to get wrong by treating the graph as a build system's
//! dependency graph, where a cycle is an error to report. In ES modules a cycle is *specified*
//! behaviour: the modules are instantiated together, evaluated in depth-first post-order, and
//! a binding read before its module has evaluated is a `ReferenceError` from the temporal dead
//! zone rather than a link failure. `react` and `react-dom` have shipped cycles for years.
//!
//! So [`ModuleGraph::evaluation_order`] never fails. It orders a cycle deterministically and
//! [`ModuleGraph::cycles`] reports which modules are in one, because a consumer may want to
//! warn — but nothing here refuses to proceed.

use std::collections::HashMap;

/// Identifies a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(u32);

impl ModuleId {
    /// The index, for a caller keeping a side table.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }

    /// Rebuilds an id from [`ModuleId::index`].
    ///
    /// Valid only for an index this graph handed out; a larger one simply refers to no
    /// module, and every accessor returns nothing for it rather than panicking.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }
}

#[derive(Debug)]
struct Module {
    specifier: String,
    /// Resolved imports, in source order.
    ///
    /// Source order, because evaluation order depends on it: `import "./a"; import "./b"`
    /// evaluates `a` first, and a graph that sorted or deduplicated these would produce an
    /// order the specification does not.
    imports: Vec<ModuleId>,
}

/// Every module in a program, and the edges between them.
#[derive(Debug, Default)]
pub struct ModuleGraph {
    modules: Vec<Module>,
    by_specifier: HashMap<String, ModuleId>,
}

impl ModuleGraph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The id for `specifier`, adding it if it is new.
    ///
    /// Idempotent, because the same module is reached from every importer of it and each one
    /// asks for it by name.
    pub fn intern(&mut self, specifier: &str) -> ModuleId {
        if let Some(existing) = self.by_specifier.get(specifier) {
            return *existing;
        }
        let id = ModuleId(u32::try_from(self.modules.len()).expect("modules fit in u32"));
        self.modules.push(Module {
            specifier: specifier.to_owned(),
            imports: Vec::new(),
        });
        self.by_specifier.insert(specifier.to_owned(), id);
        id
    }

    /// Records that `from` imports `to`.
    ///
    /// Duplicates are kept rather than merged: `import {a} from "./m"; import {b} from "./m"`
    /// is two import records of one module, and the graph should not quietly decide otherwise.
    /// Evaluation visits each module once regardless.
    pub fn add_import(&mut self, from: ModuleId, to: ModuleId) {
        self.modules[from.0 as usize].imports.push(to);
    }

    /// The specifier a module was interned under.
    #[must_use]
    pub fn specifier(&self, module: ModuleId) -> Option<&str> {
        self.modules
            .get(module.0 as usize)
            .map(|module| module.specifier.as_str())
    }

    /// What a module imports, in source order.
    #[must_use]
    pub fn imports(&self, module: ModuleId) -> &[ModuleId] {
        self.modules
            .get(module.0 as usize)
            .map_or(&[], |module| module.imports.as_slice())
    }

    /// How many modules there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Whether the graph holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    /// The order modules evaluate in, starting from `entry`.
    ///
    /// Depth-first post-order: a module's imports evaluate before it does, each module once,
    /// and imports are visited in source order. That is what the specification's
    /// `Evaluate()` does, and it is why a diamond evaluates its shared dependency once and
    /// before both of its dependents.
    ///
    /// **Never fails.** A cycle is ordered rather than rejected — see the module docs. The
    /// module that closes a cycle is simply already on the stack when it is reached again, so
    /// it is not visited twice and the order that comes out is the one the specification
    /// produces.
    ///
    /// Modules unreachable from `entry` are not included, because they do not evaluate.
    #[must_use]
    pub fn evaluation_order(&self, entry: ModuleId) -> Vec<ModuleId> {
        let mut order = Vec::with_capacity(self.modules.len());
        let mut visited = vec![false; self.modules.len()];
        // Iterative rather than recursive: a deep `node_modules` graph is exactly the input
        // that turns a recursive post-order walk into a stack overflow, and it arrives from
        // a user's dependency tree rather than from anything this code controls.
        let mut stack = vec![(entry, 0_usize)];
        if let Some(seen) = visited.get_mut(entry.0 as usize) {
            *seen = true;
        } else {
            return order;
        }

        while let Some((module, next)) = stack.pop() {
            let imports = self.imports(module);
            if next < imports.len() {
                stack.push((module, next + 1));
                let child = imports[next];
                let Some(seen) = visited.get_mut(child.0 as usize) else {
                    continue;
                };
                if !*seen {
                    *seen = true;
                    stack.push((child, 0));
                }
            } else {
                order.push(module);
            }
        }
        order
    }

    /// The modules that take part in a cycle, grouped.
    ///
    /// Tarjan's strongly connected components, minus the ones of size one that are not
    /// self-referential — a module that merely appears in the graph is not a cycle.
    ///
    /// Reported rather than rejected. A consumer may want to warn about a cycle, or explain a
    /// temporal-dead-zone `ReferenceError` by pointing at one, but the program is still valid.
    #[must_use]
    pub fn cycles(&self) -> Vec<Vec<ModuleId>> {
        Tarjan::new(self).run()
    }

    /// Whether `module` is in a cycle.
    #[must_use]
    pub fn in_cycle(&self, module: ModuleId) -> bool {
        self.cycles().iter().any(|group| group.contains(&module))
    }
}

/// Tarjan's strongly connected components, iteratively.
///
/// Iterative for the same reason the evaluation walk is: the input is somebody's dependency
/// tree and its depth is not this code's to bound.
struct Tarjan<'graph> {
    graph: &'graph ModuleGraph,
    index: Vec<Option<u32>>,
    low: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<ModuleId>,
    next_index: u32,
    out: Vec<Vec<ModuleId>>,
}

impl<'graph> Tarjan<'graph> {
    fn new(graph: &'graph ModuleGraph) -> Self {
        let count = graph.len();
        Self {
            graph,
            index: vec![None; count],
            low: vec![0; count],
            on_stack: vec![false; count],
            stack: Vec::new(),
            next_index: 0,
            out: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<Vec<ModuleId>> {
        for raw in 0..self.graph.len() {
            let id = ModuleId(u32::try_from(raw).expect("modules fit in u32"));
            if self.index[raw].is_none() {
                self.walk(id);
            }
        }
        self.out
    }

    fn walk(&mut self, root: ModuleId) {
        let mut work = vec![(root, 0_usize)];
        self.enter(root);

        while let Some((module, next)) = work.pop() {
            let at = module.0 as usize;
            let imports = self.graph.imports(module);
            if next < imports.len() {
                work.push((module, next + 1));
                let child = imports[next];
                let child_at = child.0 as usize;
                if self.index[child_at].is_none() {
                    self.enter(child);
                    work.push((child, 0));
                } else if self.on_stack[child_at] {
                    self.low[at] = self.low[at].min(self.index[child_at].unwrap_or(u32::MAX));
                }
                continue;
            }

            // Finished with this module: fold its low-link into its parent's, then close a
            // component if this is its root.
            if let Some((parent, _)) = work.last() {
                let parent_at = parent.0 as usize;
                self.low[parent_at] = self.low[parent_at].min(self.low[at]);
            }
            if self.index[at] == Some(self.low[at]) {
                self.close(module);
            }
        }
    }

    fn enter(&mut self, module: ModuleId) {
        let at = module.0 as usize;
        self.index[at] = Some(self.next_index);
        self.low[at] = self.next_index;
        self.next_index += 1;
        self.stack.push(module);
        self.on_stack[at] = true;
    }

    fn close(&mut self, root: ModuleId) {
        let mut group = Vec::new();
        while let Some(module) = self.stack.pop() {
            self.on_stack[module.0 as usize] = false;
            group.push(module);
            if module == root {
                break;
            }
        }
        // A component of one is a cycle only if the module imports itself. Everything else is
        // just a module, and reporting every module as a cycle would make the report useless.
        let is_cycle = group.len() > 1
            || group
                .first()
                .is_some_and(|only| self.graph.imports(*only).contains(only));
        if is_cycle {
            group.reverse();
            self.out.push(group);
        }
    }
}
