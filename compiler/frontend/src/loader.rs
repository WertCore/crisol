//! Reading a real module tree: resolve, parse, and record what each file imports.
//!
//! This is the half of §M10 that touches the world. [`crate::ModuleGraph`] decides what an
//! edge *means*; this decides which edges exist, by asking `oxc_resolver` where a specifier
//! points and `oxc_parser` what a file asks for.
//!
//! # Both module systems, because a real tree has both
//!
//! React 19 is CommonJS from top to bottom — its entry is
//! `module.exports = require('./cjs/react.production.js')` — so a loader that understood only
//! `import` would walk a React tree and find no edges at all, then report a complete graph with
//! no unresolved imports. It would pass §M10's acceptance by doing nothing. That is the
//! failure this module is shaped to avoid, which is why `require()` is found through an
//! exhaustive AST visit rather than by pattern-matching the shapes someone expected.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{Argument, CallExpression, Expression};
use oxc_ast_visit::Visit;
use oxc_parser::Parser;
use oxc_resolver::{ResolveOptions, Resolver};
use oxc_span::SourceType;

use crate::{ModuleGraph, ModuleId};

/// A specifier a file asked for, and where it asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The text between the quotes.
    pub specifier: String,
    /// Byte offset in the source, used to put ESM and CJS requests back into one order.
    pub at: u32,
}

/// A specifier that could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    /// The file that asked.
    pub importer: PathBuf,
    /// What it asked for.
    pub specifier: String,
    /// What the resolver said.
    pub reason: String,
}

/// Walks a module tree from an entry point, building a [`ModuleGraph`].
pub struct Loader {
    resolver: Resolver,
    graph: ModuleGraph,
    unresolved: Vec<Unresolved>,
    parsed: usize,
    parse_errors: Vec<(PathBuf, String)>,
}

impl Loader {
    /// A loader with Node's resolution rules.
    #[must_use]
    pub fn new() -> Self {
        // `require` before `import`, and `.js` before anything else, because this is reading a
        // `node_modules` tree as Node would. A different set of conditions is a different
        // graph — that is the point of them, and why they are stated rather than defaulted.
        let options = ResolveOptions {
            condition_names: vec!["node".into(), "require".into(), "default".into()],
            extensions: vec![".js".into(), ".mjs".into(), ".cjs".into(), ".json".into()],
            main_fields: vec!["main".into()],
            ..ResolveOptions::default()
        };
        Self {
            resolver: Resolver::new(options),
            graph: ModuleGraph::new(),
            unresolved: Vec::new(),
            parsed: 0,
            parse_errors: Vec::new(),
        }
    }

    /// Loads `entry` and everything it reaches.
    ///
    /// Breadth-first over files, which is incidental — the graph decides evaluation order, and
    /// it does that from the edges rather than from the order they were discovered in.
    ///
    /// # Errors
    ///
    /// Returns the entry's own read error. A failure *inside* the tree is recorded rather than
    /// returned: one unreadable file in `node_modules` should produce a report, not abort the
    /// walk, and [`Loader::unresolved`] is that report.
    pub fn load(&mut self, entry: &Path) -> std::io::Result<ModuleId> {
        let entry = entry.canonicalize()?;
        let root = self.graph.intern(&entry.to_string_lossy());
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut queue = vec![(entry, root)];
        seen.insert(queue[0].0.clone());

        while let Some((path, id)) = queue.pop() {
            let Ok(source) = std::fs::read_to_string(&path) else {
                self.parse_errors
                    .push((path.clone(), "could not be read".to_owned()));
                continue;
            };
            for request in self.requests(&path, &source) {
                let directory = path.parent().unwrap_or(Path::new("."));
                match self.resolver.resolve(directory, &request.specifier) {
                    Ok(resolution) => {
                        let target = resolution.full_path();
                        let target_id = self.graph.intern(&target.to_string_lossy());
                        self.graph.add_import(id, target_id);
                        if seen.insert(target.clone()) {
                            queue.push((target, target_id));
                        }
                    }
                    Err(error) => self.unresolved.push(Unresolved {
                        importer: path.clone(),
                        specifier: request.specifier.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
        }
        Ok(root)
    }

    /// Loads a package by specifier, as `require("react")` from `directory` would.
    ///
    /// The way a real tree is entered: an application names a package, and where that lands is
    /// the resolver's answer rather than a path anyone wrote down.
    ///
    /// # Errors
    ///
    /// When the specifier does not resolve, or the file it resolves to cannot be read.
    pub fn load_specifier(
        &mut self,
        directory: &Path,
        specifier: &str,
    ) -> std::io::Result<ModuleId> {
        let resolution = self
            .resolver
            .resolve(directory, specifier)
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{specifier}: {error}"),
                )
            })?;
        let path = resolution.full_path();
        self.load(&path)
    }

    /// Every specifier a source asks for, in source order.
    ///
    /// ESM requests come from the parser's `ModuleRecord`, which is the specification's
    /// `[[RequestedModules]]` and so is exactly right for `import` and `export … from`. CJS
    /// requests are not in it — `require` is a function call, not syntax — so they are visited
    /// for separately and the two are merged by source position.
    fn requests(&mut self, path: &Path, source: &str) -> Vec<Request> {
        let allocator = Allocator::default();
        let source_type = SourceType::from_path(path).unwrap_or_default();
        let parsed = Parser::new(&allocator, source, source_type).parse();
        self.parsed += 1;
        if !parsed.diagnostics.is_empty() {
            // Recorded, not fatal: oxc recovers, and a file with a syntax error still has
            // imports worth knowing about.
            self.parse_errors.push((
                path.to_path_buf(),
                format!("{} syntax errors", parsed.diagnostics.len()),
            ));
        }

        let mut requests: Vec<Request> = parsed
            .module_record
            .requested_modules
            .iter()
            .flat_map(|(specifier, occurrences)| {
                occurrences.iter().map(move |occurrence| Request {
                    specifier: specifier.to_string(),
                    at: occurrence.span.start,
                })
            })
            .collect();

        let mut requires = Requires::default();
        requires.visit_program(&parsed.program);
        requests.extend(requires.found);

        // `requested_modules` is a map, so its iteration order is not source order. Sorting by
        // position restores it, and it is the order that decides evaluation.
        requests.sort_by_key(|request| request.at);
        requests
    }

    /// The graph built so far.
    #[must_use]
    pub const fn graph(&self) -> &ModuleGraph {
        &self.graph
    }

    /// Specifiers that did not resolve. §M10's acceptance is that this is empty.
    #[must_use]
    pub fn unresolved(&self) -> &[Unresolved] {
        &self.unresolved
    }

    /// Files parsed.
    #[must_use]
    pub const fn parsed(&self) -> usize {
        self.parsed
    }

    /// Files that could not be read, or that oxc recovered from.
    #[must_use]
    pub fn parse_errors(&self) -> &[(PathBuf, String)] {
        &self.parse_errors
    }
}

impl Default for Loader {
    fn default() -> Self {
        Self::new()
    }
}

/// Finds `require("…")` anywhere in a file.
///
/// A visitor rather than a hand-rolled walk over the shapes CJS "usually" takes. React's entry
/// puts its requires inside an `if`, its bundles put them inside functions, and a walker that
/// covered the cases someone thought of would miss edges *silently* — and a missing edge makes
/// §M10's "no unresolved imports" easier to satisfy, not harder. Getting that backwards is how
/// this milestone would be passed without being done.
#[derive(Default)]
struct Requires {
    found: Vec<Request>,
}

impl<'a> Visit<'a> for Requires {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee
            && callee.name == "require"
            && let Some(Argument::StringLiteral(literal)) = call.arguments.first()
        {
            self.found.push(Request {
                specifier: literal.value.to_string(),
                at: call.span.start,
            });
        }
        // Keep walking regardless: `require(require("./which"))` is legal, and so is a
        // require nested in an argument to something else.
        oxc_ast_visit::walk::walk_call_expression(self, call);
    }
}
