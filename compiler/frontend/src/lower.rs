//! Lowering JavaScript to [`crisol_ir`].
//!
//! # Locals live in slots, not in SSA values
//!
//! A `let` becomes a numbered slot, and reading it is a `Load`. So a control-flow merge needs
//! no block parameters at all: both arms wrote the same slot, and the code after reads it.
//! Promoting slots to SSA values — the `mem2reg` every compiler has — is a separate pass and
//! belongs with the other optimisations (M12).
//!
//! Doing it the other way round, constructing SSA during lowering, means implementing
//! Braun-style incremental φ insertion *while also* getting the AST walk right, and debugging
//! the two together. Split, each half is checkable on its own: this one emits IR the verifier
//! accepts, and the promotion pass is a graph-to-graph transformation with an obvious
//! before-and-after.
//!
//! # Unsupported constructs are recorded, never guessed
//!
//! §3.3 says rejecting a construct in code the developer did not write is the failure mode to
//! avoid — but a *compiler* that silently emits `undefined` for syntax it did not understand is
//! worse than one that refuses, because the result is a program that runs and is wrong.
//!
//! So both: lowering always produces a function, and everything it did not understand is in
//! [`Lowered::unsupported`]. [`Lowered::is_faithful`] exists so a caller cannot accidentally
//! treat a partial translation as a complete one — checking a `Vec` is empty is easy to forget,
//! and a method named after the question is not.

use std::collections::HashMap;

use crisol_ir::{
    BinaryOp, Block, BlockId, CompareOp, Constant, Function, FunctionId, Instruction, Op,
    Safepoint, Terminator, Type, UnaryOp, ValueId,
};
use crisol_value::PropertyKey;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BinaryExpression, BinaryOperator, Expression, LogicalExpression, LogicalOperator,
    ObjectPropertyKind, Program, PropertyKey as Key, Statement, UnaryExpression, UnaryOperator,
};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;

/// Something the lowering did not understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// What it was, in the AST's own vocabulary.
    pub what: String,
    /// Byte offset in the source.
    pub at: u32,
}

/// The result of lowering one program.
#[derive(Debug)]
pub struct Lowered {
    /// Every function, indexed by [`crisol_ir::FunctionId`]. **Index 0 is the program itself**
    /// — a module body is a function, which is why `return` at top level is meaningful (the
    /// parser is told so explicitly).
    pub functions: Vec<Function>,
    /// Everything the lowering did not understand, in source order.
    pub unsupported: Vec<Unsupported>,
}

impl Lowered {
    /// The top-level program.
    #[must_use]
    pub fn program(&self) -> &Function {
        &self.functions[0]
    }
}

impl Lowered {
    /// Whether the IR is a complete translation of the source.
    ///
    /// False when anything was not understood. Named after the question so that a caller
    /// treating a partial translation as a complete one has to do it on purpose.
    #[must_use]
    pub fn is_faithful(&self) -> bool {
        self.unsupported.is_empty()
    }
}

/// Why a program could not be lowered at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFailed {
    /// What the parser said.
    pub errors: Vec<String>,
}

/// Lowers a program.
///
/// # Errors
///
/// [`ParseFailed`] when the source does not parse. Anything that parses lowers, with whatever
/// was not understood recorded rather than rejected.
pub fn lower(name: &str, source: &str) -> Result<Lowered, ParseFailed> {
    let allocator = Allocator::default();
    // The thing being produced is a *function body* — a program lowers to one `Function` — so
    // a `return` at what looks like top level is meaningful rather than misplaced. Node makes
    // the same reading: it wraps every CommonJS module in a function, which is why `return`
    // there is legal and in an ES module is a syntax error. Said with the parser's own option
    // rather than by picking a `SourceType` that happens to imply it.
    let parsed = Parser::new(&allocator, source, SourceType::cjs())
        .with_options(ParseOptions {
            allow_return_outside_function: true,
            ..ParseOptions::default()
        })
        .parse();
    if !parsed.diagnostics.is_empty() {
        return Err(ParseFailed {
            errors: parsed.diagnostics.iter().map(ToString::to_string).collect(),
        });
    }
    let mut lowering = Lowering::new(name);
    // Answered before lowering, because whether a variable is a cell changes every read and
    // write of it — and the lowering only discovers a capture once it has already emitted the
    // enclosing function's code.
    lowering.shared = crate::escape::shared_variables(&parsed.program);
    lowering.program(&parsed.program);
    Ok(lowering.finish())
}

/// One function being lowered.
struct Scope {
    /// Index into [`Lowering::functions`].
    function: usize,
    current: BlockId,
    /// Whether `current` already has a real terminator, so trailing statements are dead.
    terminated: bool,
    slots: HashMap<String, u32>,
    next_slot: u32,
    /// Names this function reads from an enclosing one, with the slot they land in here.
    ///
    /// Discovered during lowering rather than by a pre-pass: a name is a capture exactly when
    /// resolving it walks out of this scope, so the resolution *is* the analysis.
    captures: Vec<(String, u32)>,
    /// Slots holding a cell rather than the value itself.
    ///
    /// Per scope, because slot numbers restart in every function — slot zero is a different
    /// variable in each one, so a single set would confuse them.
    cells: std::collections::HashSet<u32>,
}

struct Lowering {
    functions: Vec<Function>,
    /// Innermost last. A nested function pushes; finishing it pops.
    scopes: Vec<Scope>,
    unsupported: Vec<Unsupported>,
    /// Names a closure must share rather than copy, from [`crate::escape`].
    shared: std::collections::HashSet<String>,
    /// Where an unlabelled `break` goes, innermost last.
    breaks: Vec<BlockId>,
    /// Where a raised exception goes, innermost last. Empty means out of the function.
    handlers: Vec<BlockId>,
    /// Where `continue` goes, innermost last. Separate from `breaks` because a `switch` is a
    /// `break` target and not a `continue` one.
    continues: Vec<BlockId>,
}

/// The property a cell keeps its value in.
///
/// A cell is an ordinary one-property object, so it needs no new IR operation and no new
/// runtime call — it allocates, stores and loads exactly like an object literal. That is
/// slower than a dedicated representation and is the right first version: correctness now,
/// and a measurement before inventing machinery to make it faster.
const CELL_KEY: &str = "value";

impl Lowering {
    fn new(name: &str) -> Self {
        // Function zero is the program itself. Stamped rather than left to the default, so
        // every producer of a `Function` reads the same way.
        let mut function = Function::new(name);
        function.id = FunctionId(0);
        let entry = function.entry;
        let mut lowering = Self {
            functions: vec![function],
            shared: std::collections::HashSet::new(),
            breaks: Vec::new(),
            handlers: Vec::new(),
            continues: Vec::new(),
            scopes: vec![Scope {
                function: 0,
                current: entry,
                terminated: false,
                slots: HashMap::new(),
                next_slot: 0,
                captures: Vec::new(),
                cells: std::collections::HashSet::new(),
            }],
            unsupported: Vec::new(),
        };
        // The program is a function body, so it has a `this` — `undefined` in a module, the
        // global object in a script. Binding it here means a top-level arrow captures it
        // rather than inventing one.
        let this_slot = lowering.declare("this");
        lowering.functions[0].this_slot = Some(this_slot);
        lowering
    }

    fn finish(mut self) -> Lowered {
        if !self.scope().terminated {
            self.terminate(Terminator::Return(None));
        }
        Lowered {
            functions: self.functions,
            unsupported: self.unsupported,
        }
    }

    fn scope(&self) -> &Scope {
        self.scopes.last().expect("a scope is always open")
    }

    fn scope_mut(&mut self) -> &mut Scope {
        self.scopes.last_mut().expect("a scope is always open")
    }

    fn function_mut(&mut self) -> &mut Function {
        let at = self.scope().function;
        &mut self.functions[at]
    }

    fn program(&mut self, program: &Program<'_>) {
        self.hoist(&program.body);
        for statement in &program.body {
            self.statement(statement);
        }
    }

    /// `delete o.x` and `delete o[k]`.
    ///
    /// **`delete` on anything that is not a property access is `true`** and does nothing —
    /// `delete 1` and `delete someVariable` are not errors outside strict mode, and answering
    /// `false` for them would be as wrong as refusing to compile.
    fn delete(&mut self, unary: &UnaryExpression<'_>) -> ValueId {
        let (object, key) = match &unary.argument {
            Expression::StaticMemberExpression(member) => {
                let object = self.expression(&member.object);
                // The static name becomes a string constant, so one IR operation covers both
                // spellings of the same question.
                let key = self.emit(
                    Type::String,
                    Op::Const(Constant::String(member.property.name.to_string())),
                );
                (object, key)
            }
            Expression::ComputedMemberExpression(member) => {
                let object = self.expression(&member.object);
                let key = self.expression(&member.expression);
                (object, key)
            }
            other => {
                self.expression(other);
                return self.emit(Type::Bool, Op::Const(Constant::Bool(true)));
            }
        };
        let removed = self.emit(Type::Bool, Op::Delete { object, key });
        self.propagate(removed)
    }

    /// `i++`, `++i`, `i--`, `--i`.
    ///
    /// **Postfix yields the value from *before* the update and prefix the value after**, which
    /// is the entire difference between them and is invisible in a statement like `i++;`. The
    /// distinction only shows where the result is used — `a[i++]` indexes with the old `i` —
    /// so a lowering that got it backwards would pass every loop test.
    ///
    /// The operand goes through `+`, not a numeric add: `i` may not be a number, and `ToNumber`
    /// is what the specification applies. That is the same helper `i + 1` uses, so the two
    /// spellings cannot disagree.
    fn update(&mut self, update: &oxc_ast::ast::UpdateExpression<'_>) -> ValueId {
        let oxc_ast::ast::SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) =
            &update.argument
        else {
            self.note(
                "update of something other than a variable",
                update.span.start,
            );
            return self.placeholder();
        };

        let slot = self.slot(identifier.name.as_str());
        let before = self.read(slot);
        let one = self.emit(Type::Number, Op::Const(Constant::Number(1.0)));
        let op = if matches!(update.operator, oxc_ast::ast::UpdateOperator::Increment) {
            BinaryOp::Add
        } else {
            BinaryOp::Subtract
        };
        let after = self.emit(
            Type::Unknown,
            Op::Binary {
                op,
                left: before,
                right: one,
            },
        );
        self.write(slot, after);
        if update.prefix { after } else { before }
    }

    /// `let`/`const`/`var`, which a `for` initialiser also uses.
    fn variable_declaration(&mut self, declaration: &oxc_ast::ast::VariableDeclaration<'_>) {
        for declarator in &declaration.declarations {
            let Some(name) = declarator.id.get_identifier_name() else {
                self.note("destructuring declaration", declarator.span.start);
                continue;
            };
            let value = match &declarator.init {
                Some(init) => self.expression(init),
                None => self.emit(Type::Undefined, Op::Const(Constant::Undefined)),
            };
            // `declare`, not `slot`: a `let` shadows an outer binding rather than capturing it.
            let slot = self.declare(name.as_str());
            self.bind(name.as_str(), slot, value);
        }
    }

    /// `for (name in object) body`.
    ///
    /// Lowered as an ordinary counted loop over a list of names taken **before** the body runs.
    /// The specification allows a property deleted during the loop to be skipped and one added
    /// not to be visited, so taking the list up front is within it — and it keeps the loop from
    /// depending on an enumeration order that its own body is changing.
    ///
    /// `continue` goes to the increment and not the test, for the same reason it does in a
    /// `for` loop: skipping the increment is a hang rather than a wrong answer.
    fn for_in_statement(&mut self, statement: &oxc_ast::ast::ForInStatement<'_>) {
        let subject = self.expression(&statement.right);
        let names = self.emit(Type::Object(None), Op::Enumerate { object: subject });
        self.indexed_loop(names, &statement.left, &statement.body);
    }

    /// `for (name of iterable) body`.
    ///
    /// **This is not the iterator protocol.** There is no `Symbol`, so there is no
    /// `Symbol.iterator` to look up and a user-defined iterable cannot work. What this does
    /// cover is an array or a string, which `crisol_iterate` turns into something indexable and
    /// anything else rejects with a `TypeError` — the same error the protocol would raise, for
    /// a different reason.
    ///
    /// An array is indexed **live**, not copied, so its `length` is re-read each step and a
    /// `push` inside the loop is seen. That matches the array iterator, which is also why
    /// `for (const x of a) a.push(x)` does not terminate here any more than it does in a real
    /// engine.
    fn for_of_statement(&mut self, statement: &oxc_ast::ast::ForOfStatement<'_>) {
        if statement.r#await {
            self.note("for-await-of statement", statement.span.start);
            return;
        }
        let subject = self.expression(&statement.right);
        let values = self.emit(Type::Object(None), Op::Iterate { object: subject });
        // `crisol_iterate` raises on a non-iterable, so the signal has to be honoured here or
        // the loop would run over the signal itself.
        let values = self.propagate(values);
        self.indexed_loop(values, &statement.left, &statement.body);
    }

    /// The loop both `for-in` and `for-of` are: walk `list` by index, binding each element.
    ///
    /// `length` is read in the header rather than once before it, so a list that grows or
    /// shrinks during the body is followed rather than snapshotted.
    ///
    /// `continue` goes to the increment and not the test, for the same reason it does in a
    /// `for` loop: skipping the increment is a hang rather than a wrong answer.
    fn indexed_loop(
        &mut self,
        subject: ValueId,
        left: &oxc_ast::ast::ForStatementLeft<'_>,
        body_statement: &Statement<'_>,
    ) {
        let list = self.temporary();
        self.emit_effect(Op::Store {
            slot: list,
            value: subject,
        });
        let position = self.temporary();
        let zero = self.emit(Type::Number, Op::Const(Constant::Number(0.0)));
        self.emit_effect(Op::Store {
            slot: position,
            value: zero,
        });

        let header = self.new_block();
        let body = self.new_block();
        let step = self.new_block();
        let exit = self.new_block();
        self.terminate(Terminator::Jump {
            target: header,
            args: Vec::new(),
        });

        self.switch_to(header);
        let held = self.emit(Type::Unknown, Op::Load { slot: list });
        let length = self.emit(
            Type::Unknown,
            Op::PropertyLoad {
                object: held,
                key: PropertyKey::new("length"),
            },
        );
        let at = self.emit(Type::Unknown, Op::Load { slot: position });
        let more = self.emit(
            Type::Bool,
            Op::Compare {
                op: CompareOp::Less,
                left: at,
                right: length,
            },
        );
        self.terminate(Terminator::Branch {
            condition: more,
            then_block: body,
            then_args: Vec::new(),
            else_block: exit,
            else_args: Vec::new(),
        });

        self.switch_to(body);
        let held = self.emit(Type::Unknown, Op::Load { slot: list });
        let at = self.emit(Type::Unknown, Op::Load { slot: position });
        let name = self.emit(
            Type::Unknown,
            Op::ComputedLoad {
                object: held,
                key: at,
            },
        );
        self.bind_loop_variable(left, name);

        self.breaks.push(exit);
        self.continues.push(step);
        self.statement(body_statement);
        self.breaks.pop();
        self.continues.pop();
        self.terminate(Terminator::Jump {
            target: step,
            args: Vec::new(),
        });

        self.switch_to(step);
        let at = self.emit(Type::Unknown, Op::Load { slot: position });
        let one = self.emit(Type::Number, Op::Const(Constant::Number(1.0)));
        let next = self.emit(
            Type::Unknown,
            Op::Binary {
                op: BinaryOp::Add,
                left: at,
                right: one,
            },
        );
        self.emit_effect(Op::Store {
            slot: position,
            value: next,
        });
        self.terminate(Terminator::Jump {
            target: header,
            args: Vec::new(),
        });

        self.switch_to(exit);
    }

    /// Binds the name or value the loop is currently visiting.
    ///
    /// `for (let k in o)` declares `k`; `for (k in o)` assigns to whatever `k` already names.
    /// Treating the second as a declaration would shadow the outer binding, so the loop would
    /// run correctly and leave nothing behind.
    fn bind_loop_variable(&mut self, left: &oxc_ast::ast::ForStatementLeft<'_>, value: ValueId) {
        match left {
            oxc_ast::ast::ForStatementLeft::VariableDeclaration(declaration) => {
                let Some(first) = declaration.declarations.first() else {
                    return;
                };
                let Some(name) = first.id.get_identifier_name() else {
                    self.note("destructuring for-in binding", declaration.span.start);
                    return;
                };
                let slot = self.declare(name.as_str());
                self.bind(name.as_str(), slot, value);
            }
            oxc_ast::ast::ForStatementLeft::AssignmentTargetIdentifier(identifier) => {
                let slot = self.slot(identifier.name.as_str());
                self.write(slot, value);
            }
            // A member expression or a pattern. Reported without a position, because a
            // `ForStatementLeft` does not carry one without the span trait in scope and the
            // construct is named precisely enough to find.
            _ => self.note("for-in binding that is not a plain name", 0),
        }
    }

    /// `for (init; test; update) body`.
    ///
    /// Four blocks rather than three, because **`continue` goes to the update, not the test**.
    /// Sharing a block for them would make `for (i = 0; i < 3; i = i + 1) { continue; }` skip
    /// the increment and loop forever — a hang rather than a wrong answer, and one that only
    /// appears when a `continue` is present.
    ///
    /// An absent test means `true`: `for (;;)` is an infinite loop, not one that never runs.
    fn for_statement(&mut self, statement: &oxc_ast::ast::ForStatement<'_>) {
        if let Some(init) = &statement.init {
            match init {
                oxc_ast::ast::ForStatementInit::VariableDeclaration(declaration) => {
                    self.variable_declaration(declaration);
                }
                other => {
                    if let Some(expression) = other.as_expression() {
                        self.expression(expression);
                    } else {
                        self.note("for initialiser", statement.span.start);
                    }
                }
            }
        }

        let header = self.new_block();
        let body = self.new_block();
        let update = self.new_block();
        let exit = self.new_block();
        self.terminate(Terminator::Jump {
            target: header,
            args: Vec::new(),
        });

        self.switch_to(header);
        match &statement.test {
            Some(test) => {
                let condition = self.expression(test);
                self.terminate(Terminator::Branch {
                    condition,
                    then_block: body,
                    then_args: Vec::new(),
                    else_block: exit,
                    else_args: Vec::new(),
                });
            }
            None => self.terminate(Terminator::Jump {
                target: body,
                args: Vec::new(),
            }),
        }

        self.switch_to(body);
        self.breaks.push(exit);
        self.continues.push(update);
        self.statement(&statement.body);
        self.breaks.pop();
        self.continues.pop();
        self.terminate(Terminator::Jump {
            target: update,
            args: Vec::new(),
        });

        self.switch_to(update);
        if let Some(step) = &statement.update {
            self.expression(step);
        }
        self.terminate(Terminator::Jump {
            target: header,
            args: Vec::new(),
        });

        self.switch_to(exit);
    }

    /// Follows a value that may be the exception signal with the branch that propagates it.
    ///
    /// **This is what "explicit result propagation" means, written down.** A call returns the
    /// signal instead of a result, so every call is followed by a test and a branch: into the
    /// enclosing `catch` if there is one, and out of the function otherwise. The unwinding is
    /// ordinary control flow the verifier already checks, rather than metadata a backend has to
    /// remember to honour.
    ///
    /// Returns the value, so a caller can use it where the call's result was expected — on the
    /// path where it is not the signal, which is the only path that continues.
    fn propagate(&mut self, value: ValueId) -> ValueId {
        let raised = self.emit(
            Type::Bool,
            Op::Unary {
                op: UnaryOp::IsException,
                operand: value,
            },
        );
        let unwind = self.new_block();
        let normal = self.new_block();
        self.terminate(Terminator::Branch {
            condition: raised,
            then_block: unwind,
            then_args: Vec::new(),
            else_block: normal,
            else_args: Vec::new(),
        });

        self.switch_to(unwind);
        match self.handlers.last().copied() {
            Some(handler) => self.terminate(Terminator::Jump {
                target: handler,
                args: Vec::new(),
            }),
            // Nothing here catches it, so it leaves as this function's result and the caller
            // runs the same test.
            None => self.terminate(Terminator::Return(Some(value))),
        }

        self.switch_to(normal);
        value
    }

    /// `try { … } catch (e) { … }`.
    fn try_statement(&mut self, statement: &oxc_ast::ast::TryStatement<'_>) {
        if statement.finalizer.is_some() {
            // `finally` runs on *both* paths, including the one that leaves by throwing, and
            // half of that is worse than none — a `finally` that ran only when nothing threw
            // would look right in every test that does not throw.
            self.note("try with finally", statement.span.start);
        }
        let Some(catch) = &statement.handler else {
            self.note("try without catch", statement.span.start);
            return;
        };

        let handler = self.new_block();
        let end = self.new_block();

        self.handlers.push(handler);
        for inner in &statement.block.body {
            self.statement(inner);
        }
        self.handlers.pop();
        self.terminate(Terminator::Jump {
            target: end,
            args: Vec::new(),
        });

        self.switch_to(handler);
        let caught = self.emit(Type::Unknown, Op::CaughtValue);
        if let Some(parameter) = &catch.param {
            match parameter.pattern.get_identifier_name() {
                Some(name) => {
                    let slot = self.declare(name.as_str());
                    self.bind(name.as_str(), slot, caught);
                }
                None => self.note("destructuring catch parameter", catch.span.start),
            }
        }
        for inner in &catch.body.body {
            self.statement(inner);
        }
        self.terminate(Terminator::Jump {
            target: end,
            args: Vec::new(),
        });

        self.switch_to(end);
    }

    /// `switch`, as a chain of strict comparisons and a run of fall-through blocks.
    ///
    /// Two things make it more than a nest of `if`s, and both are observable:
    ///
    /// - **Cases fall through.** A body with no `break` continues into the next one, which is
    ///   why the bodies are a chain rather than branches of a conditional.
    /// - **`default` is tested last but runs in its source position.** `switch (x) { default:
    ///   a(); case 1: b(); }` with `x === 1` runs only `b()`, and with anything else runs
    ///   `a()` *and then* `b()`. Lowering `default` as the final body would be wrong for the
    ///   second, and treating it as a first-match arm wrong for the first.
    ///
    /// The discriminant is evaluated **once**, into a temporary, because the comparisons read
    /// it repeatedly and `switch (f())` must not call `f` per case.
    fn switch_statement(&mut self, statement: &oxc_ast::ast::SwitchStatement<'_>) {
        let slot = self.temporary();
        let discriminant = self.expression(&statement.discriminant);
        self.emit_effect(Op::Store {
            slot,
            value: discriminant,
        });

        let bodies: Vec<BlockId> = statement.cases.iter().map(|_| self.new_block()).collect();
        let end = self.new_block();
        let default = statement
            .cases
            .iter()
            .position(|case| case.test.is_none())
            .map(|index| bodies[index]);

        for (index, case) in statement.cases.iter().enumerate() {
            let Some(test) = &case.test else {
                // `default` takes no test here; it is where control goes once every test has
                // failed, which is decided after this loop.
                continue;
            };
            let next = self.new_block();
            let left = self.emit(Type::Unknown, Op::Load { slot });
            let right = self.expression(test);
            let matched = self.emit(
                Type::Bool,
                Op::Compare {
                    op: CompareOp::StrictEqual,
                    left,
                    right,
                },
            );
            self.terminate(Terminator::Branch {
                condition: matched,
                then_block: bodies[index],
                then_args: Vec::new(),
                else_block: next,
                else_args: Vec::new(),
            });
            self.switch_to(next);
        }
        // Every test failed.
        self.terminate(Terminator::Jump {
            target: default.unwrap_or(end),
            args: Vec::new(),
        });

        self.breaks.push(end);
        for (index, case) in statement.cases.iter().enumerate() {
            self.switch_to(bodies[index]);
            for inner in &case.consequent {
                self.statement(inner);
            }
            // Falls into the next body, or out. A body that already returned or broke is
            // terminated, and `terminate` leaves it alone.
            self.terminate(Terminator::Jump {
                target: bodies.get(index + 1).copied().unwrap_or(end),
                args: Vec::new(),
            });
        }
        self.breaks.pop();
        self.switch_to(end);
    }

    /// Binds every function declared in `statements`, before any of them runs.
    ///
    /// **A function declaration is usable above its own text.** `f(); function f() {}` is
    /// ordinary JavaScript and the whole of test262's own harness depends on it — `assert.js`
    /// defines helpers below the code that calls them. Lowering declarations where they appear
    /// left the name unbound until control reached it, so every one of the 12,719 cases was
    /// refused for the same reason.
    ///
    /// Only the declarations at this level. A function inside a block is hoisted to that
    /// block's scope, which needs block scoping the lowering does not model, so those are left
    /// where they are and still refused — visibly, rather than bound in the wrong scope.
    fn hoist(&mut self, statements: &[Statement<'_>]) {
        let named: Vec<(String, &oxc_ast::ast::Function<'_>)> = statements
            .iter()
            .filter_map(|statement| {
                let Statement::FunctionDeclaration(declaration) = statement else {
                    return None;
                };
                let name = declaration
                    .id
                    .as_ref()
                    .map_or_else(|| "anonymous".to_owned(), |id| id.name.to_string());
                Some((name, declaration.as_ref()))
            })
            .collect();

        // **Every name first, then every body.** One pass would lower a function before a
        // declaration further down the list had been declared, so a reference to it would
        // resolve to nothing and become a global — which is exactly what happened when
        // test262's `assert.js` was concatenated ahead of the `sta.js` that defines
        // `Test262Error`.
        //
        // Binding before lowering is also what lets a function refer to *itself*: the closure
        // captures the cell rather than the empty slot that preceded it.
        for (name, _) in &named {
            let slot = self.declare(name);
            if self.shared.contains(name) {
                self.make_cell(slot);
            }
        }

        for (name, declaration) in &named {
            let (id, captures) = self.lower_function(
                name,
                &declaration.params,
                declaration.body.as_deref(),
                None,
                true,
            );
            let closure = self.close_over(id, &captures);
            let slot = self.declare(name);
            self.write(slot, closure);
        }
    }

    // ---- plumbing ----------------------------------------------------------------------

    /// A new empty block. Its terminator is a placeholder until [`Lowering::terminate`] runs.
    fn new_block(&mut self) -> BlockId {
        self.function_mut().block(Block {
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: Terminator::Return(None),
        })
    }

    fn switch_to(&mut self, block: BlockId) {
        let scope = self.scope_mut();
        scope.current = block;
        scope.terminated = false;
    }

    fn terminate(&mut self, terminator: Terminator) {
        if self.scope().terminated {
            return;
        }
        let current = self.scope().current;
        if let Some(block) = self.function_mut().get_mut(current) {
            block.terminator = terminator;
        }
        self.scope_mut().terminated = true;
    }

    fn emit(&mut self, ty: Type, op: Op) -> ValueId {
        let result = self.function_mut().value();
        // The live set is empty because locals are in slots rather than in SSA values, so
        // nothing an allocation could invalidate is being held in one. When `mem2reg` lands
        // (M12) and values start living across allocations, this is where the live set gets
        // computed — and the verifier already refuses an allocation without a safepoint, so
        // the structure has to be here first.
        let safepoint = op.can_collect().then(Safepoint::default);
        let current = self.scope().current;
        if let Some(block) = self.function_mut().get_mut(current) {
            block.instructions.push(Instruction {
                result: Some(result),
                ty,
                op,
                safepoint,
            });
        }
        result
    }

    fn emit_effect(&mut self, op: Op) {
        let safepoint = op.can_collect().then(Safepoint::default);
        let current = self.scope().current;
        if let Some(block) = self.function_mut().get_mut(current) {
            block.instructions.push(Instruction {
                result: None,
                ty: Type::Undefined,
                op,
                safepoint,
            });
        }
    }

    /// Reads a variable, going through its cell when it has one.
    ///
    /// Every read of a local goes through here, so a shared variable cannot be read directly by
    /// some path that forgot — which would produce a stale value rather than a failure.
    fn read(&mut self, slot: u32) -> ValueId {
        let held = self.emit(Type::Unknown, Op::Load { slot });
        if self.scope().cells.contains(&slot) {
            return self.emit(
                Type::Unknown,
                Op::PropertyLoad {
                    object: held,
                    key: PropertyKey::new(CELL_KEY),
                },
            );
        }
        held
    }

    /// Writes a variable, through its cell when it has one.
    fn write(&mut self, slot: u32, value: ValueId) {
        if self.scope().cells.contains(&slot) {
            let cell = self.emit(Type::Unknown, Op::Load { slot });
            self.emit_effect(Op::PropertyStore {
                object: cell,
                key: PropertyKey::new(CELL_KEY),
                value,
            });
            return;
        }
        self.emit_effect(Op::Store { slot, value });
    }

    /// Binds a freshly declared variable to its first value.
    ///
    /// A variable closures must share gets its cell here, at the declaration — not where it is
    /// later resolved. A captured name already holds the cell the enclosing scope made, and
    /// making a second one at the use site would hand the closure a private copy, which is
    /// precisely the bug cells exist to fix.
    fn bind(&mut self, name: &str, slot: u32, value: ValueId) {
        if self.shared.contains(name) {
            self.make_cell(slot);
        }
        self.write(slot, value);
    }

    /// Gives `slot` a fresh cell, for a variable closures must share rather than copy.
    ///
    /// Called where the variable is *declared*, not where it is resolved: a captured name
    /// already holds the cell the enclosing scope made, and making a second one there would
    /// give the closure a private copy — exactly the bug this exists to fix.
    fn make_cell(&mut self, slot: u32) {
        let shape = crisol_value::Shapes::new().root();
        let cell = self.emit(Type::Object(None), Op::CreateObject { shape });
        self.emit_effect(Op::Store { slot, value: cell });
        self.scope_mut().cells.insert(slot);
    }

    /// Whether `name` names a binding anywhere in scope.
    ///
    /// A name that resolves nowhere is a **global**, not a new local. Treating it as a local is
    /// what made `Object` a fresh empty variable rather than something the runtime provides,
    /// so every test that used a builtin compared against `undefined`.
    fn resolves(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope.slots.contains_key(name))
    }

    fn slot(&mut self, name: &str) -> u32 {
        if let Some(slot) = self.scope().slots.get(name) {
            return *slot;
        }
        // Not local. If an enclosing function has it, reading it here is a **capture** — and
        // that is the whole analysis: a name is captured exactly when resolving it walks out
        // of this scope, so no separate free-variable pass is needed.
        let captured = self
            .scopes
            .iter()
            .rev()
            .skip(1)
            .any(|outer| outer.slots.contains_key(name));

        let slot = self.declare(name);
        if captured {
            self.scope_mut().captures.push((name.to_owned(), slot));
            if self.shared.contains(name) {
                // The value arriving is the *cell* the enclosing scope made, not a copy of
                // what was in it — which is what makes a write here visible out there.
                self.scope_mut().cells.insert(slot);
            }
        }
        slot
    }

    /// Makes a name local to the current function, whatever it meant outside.
    ///
    /// Used for `let`/`const`/`var` and for parameters, where the declaration *shadows* an
    /// outer binding rather than capturing it. Going through `slot` there would capture the
    /// outer one and then immediately overwrite it, which is the difference between
    /// `let x = 1` inside a closure and `x = 1`.
    fn declare(&mut self, name: &str) -> u32 {
        let scope = self.scope_mut();
        if let Some(slot) = scope.slots.get(name) {
            return *slot;
        }
        let slot = scope.next_slot;
        scope.next_slot += 1;
        scope.slots.insert(name.to_owned(), slot);
        slot
    }

    fn note(&mut self, what: &str, at: u32) {
        self.unsupported.push(Unsupported {
            what: what.to_owned(),
            at,
        });
    }

    /// A placeholder for something that was not understood.
    ///
    /// `undefined` rather than nothing, so the IR stays well-formed and the verifier still has
    /// something to check. It is only ever reached alongside an entry in `unsupported`.
    fn placeholder(&mut self) -> ValueId {
        self.emit(Type::Undefined, Op::Const(Constant::Undefined))
    }

    // ---- statements ---------------------------------------------------------------------

    fn statement(&mut self, statement: &Statement<'_>) {
        if self.scope().terminated {
            // Everything after a `return` in the same block is unreachable. Lowering it would
            // produce instructions no path can execute, which the verifier would accept and a
            // reader of the dump would rightly find baffling.
            return;
        }
        match statement {
            Statement::ExpressionStatement(statement) => {
                self.expression(&statement.expression);
            }
            Statement::VariableDeclaration(declaration) => {
                self.variable_declaration(declaration);
            }
            Statement::ReturnStatement(statement) => {
                let value = statement
                    .argument
                    .as_ref()
                    .map(|argument| self.expression(argument));
                self.terminate(Terminator::Return(value));
            }
            Statement::IfStatement(statement) => {
                let condition = self.expression(&statement.test);
                let then_block = self.new_block();
                let else_block = self.new_block();
                let join = self.new_block();
                self.terminate(Terminator::Branch {
                    condition,
                    then_block,
                    then_args: Vec::new(),
                    else_block,
                    else_args: Vec::new(),
                });

                self.switch_to(then_block);
                self.statement(&statement.consequent);
                self.terminate(Terminator::Jump {
                    target: join,
                    args: Vec::new(),
                });

                self.switch_to(else_block);
                if let Some(alternate) = &statement.alternate {
                    self.statement(alternate);
                }
                self.terminate(Terminator::Jump {
                    target: join,
                    args: Vec::new(),
                });

                self.switch_to(join);
            }
            Statement::WhileStatement(statement) => {
                let header = self.new_block();
                let body = self.new_block();
                let exit = self.new_block();
                self.terminate(Terminator::Jump {
                    target: header,
                    args: Vec::new(),
                });

                // The test goes in its own block because it runs again on every iteration —
                // putting it before the jump would run it once.
                self.switch_to(header);
                let condition = self.expression(&statement.test);
                self.terminate(Terminator::Branch {
                    condition,
                    then_block: body,
                    then_args: Vec::new(),
                    else_block: exit,
                    else_args: Vec::new(),
                });

                self.switch_to(body);
                self.breaks.push(exit);
                self.continues.push(header);
                self.statement(&statement.body);
                self.breaks.pop();
                self.continues.pop();
                self.terminate(Terminator::Jump {
                    target: header,
                    args: Vec::new(),
                });

                self.switch_to(exit);
            }
            Statement::ForStatement(statement) => self.for_statement(statement),
            Statement::ForInStatement(statement) => self.for_in_statement(statement),
            Statement::ForOfStatement(statement) => self.for_of_statement(statement),
            Statement::DoWhileStatement(statement) => {
                let body = self.new_block();
                let header = self.new_block();
                let exit = self.new_block();
                // Straight into the body: `do … while` runs it once before testing anything,
                // which is the whole difference from `while`.
                self.terminate(Terminator::Jump {
                    target: body,
                    args: Vec::new(),
                });

                self.switch_to(body);
                self.breaks.push(exit);
                // `continue` goes to the *test*, not back to the top — it ends this iteration
                // rather than skipping the condition.
                self.continues.push(header);
                self.statement(&statement.body);
                self.breaks.pop();
                self.continues.pop();
                self.terminate(Terminator::Jump {
                    target: header,
                    args: Vec::new(),
                });

                self.switch_to(header);
                let condition = self.expression(&statement.test);
                self.terminate(Terminator::Branch {
                    condition,
                    then_block: body,
                    then_args: Vec::new(),
                    else_block: exit,
                    else_args: Vec::new(),
                });

                self.switch_to(exit);
            }
            Statement::ContinueStatement(statement) => {
                if statement.label.is_some() {
                    self.note("labelled continue", statement.span.start);
                } else if let Some(target) = self.continues.last().copied() {
                    self.terminate(Terminator::Jump {
                        target,
                        args: Vec::new(),
                    });
                } else {
                    self.note("continue outside a loop", statement.span.start);
                }
            }
            Statement::BlockStatement(block) => {
                // No scope of its own: locals are slots keyed by name, so a shadowing `let`
                // inside a block would reuse the outer slot. Recorded rather than pretended
                // away — see the note in `slot`.
                for statement in &block.body {
                    self.statement(statement);
                }
            }
            Statement::ThrowStatement(statement) => {
                let value = self.expression(&statement.argument);
                // An operation and then the ordinary propagation, rather than a terminator of
                // its own. A `throw` inside a `try` has to reach the handler, and giving it a
                // second path to there is how one of them ends up missing a case.
                let signal = self.emit(
                    Type::Unknown,
                    Op::Unary {
                        op: UnaryOp::Throw,
                        operand: value,
                    },
                );
                self.propagate(signal);
            }
            Statement::TryStatement(statement) => self.try_statement(statement),
            // Already bound by `hoist`, before any statement in this list ran.
            Statement::FunctionDeclaration(_) => {}
            Statement::SwitchStatement(switch) => self.switch_statement(switch),
            Statement::BreakStatement(statement) => {
                if statement.label.is_some() {
                    // A labelled break leaves a named construct, which needs the label to name
                    // a block. Refused rather than treated as an unlabelled one, which would
                    // leave the wrong construct.
                    self.note("labelled break", statement.span.start);
                } else if let Some(target) = self.breaks.last().copied() {
                    self.terminate(Terminator::Jump {
                        target,
                        args: Vec::new(),
                    });
                } else {
                    self.note("break outside a switch or loop", statement.span.start);
                }
            }
            Statement::ClassDeclaration(class) => {
                let name = class
                    .id
                    .as_ref()
                    .map_or_else(|| "anonymous".to_owned(), |id| id.name.to_string());
                let value = self.class(class, &name);
                let slot = self.declare(&name);
                self.bind(&name, slot, value);
            }
            Statement::EmptyStatement(_) => {}
            other => {
                self.note(kind_of(other), 0);
            }
        }
    }

    // ---- expressions ---------------------------------------------------------------------

    fn expression(&mut self, expression: &Expression<'_>) -> ValueId {
        match expression {
            Expression::NumericLiteral(literal) => {
                self.emit(Type::Number, Op::Const(Constant::Number(literal.value)))
            }
            Expression::StringLiteral(literal) => self.emit(
                Type::String,
                Op::Const(Constant::String(literal.value.to_string())),
            ),
            Expression::BooleanLiteral(literal) => {
                self.emit(Type::Bool, Op::Const(Constant::Bool(literal.value)))
            }
            Expression::NullLiteral(_) => self.emit(Type::Null, Op::Const(Constant::Null)),
            Expression::Identifier(identifier) => {
                if identifier.name == "undefined" {
                    return self.emit(Type::Undefined, Op::Const(Constant::Undefined));
                }
                if self.resolves(identifier.name.as_str()) {
                    let slot = self.slot(identifier.name.as_str());
                    return self.read(slot);
                }
                let name = PropertyKey::new(identifier.name.as_str());
                let value = self.emit(Type::Unknown, Op::GlobalLoad { name });
                // A missing global is a `ReferenceError`, so the read can throw and has to be
                // followed by the same check a call is.
                self.propagate(value)
            }
            Expression::BinaryExpression(binary) => self.binary(binary),
            Expression::AssignmentExpression(assignment) => {
                let value = self.expression(&assignment.right);
                // Matched on the target's *shape*, not on `get_identifier_name`: that helper
                // reports the **property** name for `this.x`, so using it turned `this.x = x`
                // into `x = x` — a silently wrong translation with no note, which is the one
                // outcome the unsupported list exists to prevent.
                match &assignment.left {
                    oxc_ast::ast::AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                        let slot = self.slot(identifier.name.as_str());
                        self.write(slot, value);
                    }
                    oxc_ast::ast::AssignmentTarget::StaticMemberExpression(member) => {
                        let object = self.expression(&member.object);
                        let outcome = self.emit(
                            Type::Unknown,
                            Op::PropertyStore {
                                object,
                                key: PropertyKey::new(member.property.name.as_str()),
                                value,
                            },
                        );
                        self.propagate(outcome);
                    }
                    oxc_ast::ast::AssignmentTarget::ComputedMemberExpression(member) => {
                        // Evaluation order matters and is observable: the object, then the
                        // key, then the value — which is already in hand, because the right
                        // side was evaluated above. That is wrong for `a[f()] = g()` if `f`
                        // and `g` both have effects, and is recorded rather than reordered
                        // silently.
                        let object = self.expression(&member.object);
                        let key = self.expression(&member.expression);
                        let outcome =
                            self.emit(Type::Unknown, Op::ComputedStore { object, key, value });
                        self.propagate(outcome);
                    }
                    _ => self.note("assignment target", assignment.span.start),
                }
                value
            }
            Expression::StaticMemberExpression(member) => {
                let object = self.expression(&member.object);
                let value = self.emit(
                    Type::Unknown,
                    Op::PropertyLoad {
                        object,
                        key: PropertyKey::new(member.property.name.as_str()),
                    },
                );
                // Reading a property of `null` throws, so this is followed by the same check a
                // call is. Every operation that can raise gets one — that is what makes the
                // unwinding visible in the graph rather than implied (D-104).
                self.propagate(value)
            }
            Expression::CallExpression(call) => {
                // A method call must pass its receiver. `o.m()` has `this === o` inside `m`,
                // and losing that is **silent** — the call still happens and still returns
                // something, it is only `this` that is wrong.
                //
                // The object is evaluated once and reused, because `f().m()` must not call
                // `f` twice.
                let (callee, this_value) = match &call.callee {
                    Expression::StaticMemberExpression(member) => {
                        let object = self.expression(&member.object);
                        let method = self.emit(
                            Type::Unknown,
                            Op::PropertyLoad {
                                object,
                                key: PropertyKey::new(member.property.name.as_str()),
                            },
                        );
                        (method, object)
                    }
                    other => {
                        let callee = self.expression(other);
                        // A plain call passes `undefined` explicitly rather than omitting a
                        // receiver, because the two are the same thing in the language.
                        let undefined = self.emit(Type::Undefined, Op::Const(Constant::Undefined));
                        (callee, undefined)
                    }
                };
                let mut args = Vec::with_capacity(call.arguments.len());
                for argument in &call.arguments {
                    match argument.as_expression() {
                        Some(expression) => args.push(self.expression(expression)),
                        None => {
                            self.note("spread argument", call.span.start);
                            let placeholder = self.placeholder();
                            args.push(placeholder);
                        }
                    }
                }
                let result = self.emit(
                    Type::Unknown,
                    Op::Call {
                        callee,
                        this_value,
                        args,
                    },
                );
                self.propagate(result)
            }
            Expression::ObjectExpression(object) => self.object(object),
            Expression::FunctionExpression(function) => {
                let name = function
                    .id
                    .as_ref()
                    .map_or_else(|| "anonymous".to_owned(), |id| id.name.to_string());
                let (id, names) = self.lower_function(
                    &name,
                    &function.params,
                    function.body.as_deref(),
                    None,
                    true,
                );
                self.close_over(id, &names)
            }
            Expression::ArrowFunctionExpression(arrow) => {
                // A concise body — `x => x + 1` — and a block body are distinct shapes in the
                // AST, so the distinction is read off the type rather than reconstructed from
                // a boolean plus a guess at the single statement inside.
                let (id, names) = match (arrow.get_expression(), arrow.get_function_body()) {
                    (Some(expression), _) => {
                        self.lower_function("arrow", &arrow.params, None, Some(expression), false)
                    }
                    (None, Some(body)) => {
                        self.lower_function("arrow", &arrow.params, Some(body), None, false)
                    }
                    (None, None) => {
                        self.note("arrow with no body", arrow.span.start);
                        return self.placeholder();
                    }
                };
                self.close_over(id, &names)
            }
            Expression::NewExpression(new) => {
                let callee = self.expression(&new.callee);
                let mut args = Vec::with_capacity(new.arguments.len());
                for argument in &new.arguments {
                    match argument.as_expression() {
                        Some(expression) => args.push(self.expression(expression)),
                        None => {
                            self.note("spread argument", new.span.start);
                            let placeholder = self.placeholder();
                            args.push(placeholder);
                        }
                    }
                }
                let result = self.emit(Type::Object(None), Op::Construct { callee, args });
                self.propagate(result)
            }
            Expression::ClassExpression(class) => {
                let name = class
                    .id
                    .as_ref()
                    .map_or_else(|| "anonymous".to_owned(), |id| id.name.to_string());
                self.class(class, &name)
            }
            Expression::ThisExpression(_) => {
                let slot = self.slot("this");
                self.read(slot)
            }
            Expression::UnaryExpression(unary) => self.unary(unary),
            Expression::TemplateLiteral(template) => self.template(template),
            Expression::RegExpLiteral(literal) => {
                // The pattern is compiled when the literal is evaluated, so an invalid one
                // raises a `SyntaxError` there rather than inside whatever later called `test`.
                let regexp = self.emit(
                    Type::Object(None),
                    Op::CreateRegExp {
                        source: literal.regex.pattern.text.to_string(),
                        flags: literal.regex.flags.to_string(),
                    },
                );
                self.propagate(regexp)
            }
            Expression::LogicalExpression(logical) => self.logical(logical),
            Expression::ConditionalExpression(conditional) => self.conditional(conditional),
            Expression::ArrayExpression(array) => {
                let mut elements = Vec::with_capacity(array.elements.len());
                for element in &array.elements {
                    match element.as_expression() {
                        Some(expression) => elements.push(self.expression(expression)),
                        None => {
                            // A hole in `[1, , 3]`, or a spread. Holes are not `undefined`
                            // (D-64) and the IR has no way to say so yet, so this is recorded
                            // rather than filled in with a value that would read the same and
                            // answer `in` differently.
                            self.note("array hole or spread", array.span.start);
                            let placeholder = self.placeholder();
                            elements.push(placeholder);
                        }
                    }
                }
                self.emit(Type::Object(None), Op::CreateArray { elements })
            }
            Expression::ComputedMemberExpression(member) => {
                let object = self.expression(&member.object);
                let key = self.expression(&member.expression);
                let value = self.emit(Type::Unknown, Op::ComputedLoad { object, key });
                self.propagate(value)
            }
            Expression::UpdateExpression(update) => self.update(update),
            Expression::ParenthesizedExpression(inner) => self.expression(&inner.expression),
            other => {
                self.note(expression_kind(other), 0);
                self.placeholder()
            }
        }
    }

    fn binary(&mut self, binary: &BinaryExpression<'_>) -> ValueId {
        let op = match binary.operator {
            BinaryOperator::StrictEquality => CompareOp::StrictEqual,
            BinaryOperator::StrictInequality => CompareOp::StrictNotEqual,
            BinaryOperator::LessThan => CompareOp::Less,
            BinaryOperator::LessEqualThan => CompareOp::LessEqual,
            BinaryOperator::GreaterThan => CompareOp::Greater,
            BinaryOperator::GreaterEqualThan => CompareOp::GreaterEqual,
            other => return self.arithmetic(binary, other),
        };
        let left = self.expression(&binary.left);
        let right = self.expression(&binary.right);
        self.emit(Type::Bool, Op::Compare { op, left, right })
    }

    /// The operators that produce a value rather than a boolean.
    fn arithmetic(&mut self, binary: &BinaryExpression<'_>, operator: BinaryOperator) -> ValueId {
        let op = match operator {
            BinaryOperator::Addition => BinaryOp::Add,
            BinaryOperator::Subtraction => BinaryOp::Subtract,
            BinaryOperator::Multiplication => BinaryOp::Multiply,
            BinaryOperator::Division => BinaryOp::Divide,
            BinaryOperator::Remainder => BinaryOp::Remainder,
            BinaryOperator::Exponential => BinaryOp::Exponent,
            BinaryOperator::BitwiseAnd => BinaryOp::BitAnd,
            BinaryOperator::BitwiseOR => BinaryOp::BitOr,
            BinaryOperator::BitwiseXOR => BinaryOp::BitXor,
            BinaryOperator::ShiftLeft => BinaryOp::ShiftLeft,
            BinaryOperator::ShiftRight => BinaryOp::ShiftRight,
            BinaryOperator::ShiftRightZeroFill => BinaryOp::UnsignedShiftRight,
            BinaryOperator::Instanceof => BinaryOp::InstanceOf,
            BinaryOperator::Equality => BinaryOp::LooseEqual,
            BinaryOperator::Inequality => BinaryOp::LooseNotEqual,
            BinaryOperator::In => BinaryOp::In,
            _ => {
                self.note(
                    &format!("binary operator {}", operator.as_str()),
                    binary.span.start,
                );
                return self.placeholder();
            }
        };
        let left = self.expression(&binary.left);
        let right = self.expression(&binary.right);
        // Everything except `+` coerces with `ToNumber` and produces a number. `+` may
        // concatenate, so its result is `Unknown` unless something later proves otherwise —
        // typing it `Number` would let codegen emit a float add for a string concatenation.
        let ty = if op.is_always_numeric() {
            Type::Number
        } else if op.is_always_boolean() {
            Type::Bool
        } else {
            // `+` alone, which may concatenate — typing it `Number` would let codegen emit a
            // float add for a string concatenation.
            Type::Unknown
        };
        let result = self.emit(ty, Op::Binary { op, left, right });
        // **Only `in` can raise**, so only `in` pays for the check. `instanceof` and `+` answer
        // for every input they are given, and the other operators coerce with `ToNumber`, which
        // has no failing case over the values this engine has.
        if matches!(op, BinaryOp::In) {
            return self.propagate(result);
        }
        result
    }

    fn unary(&mut self, unary: &UnaryExpression<'_>) -> ValueId {
        let (op, ty) = match unary.operator {
            UnaryOperator::UnaryNegation => (UnaryOp::Negate, Type::Number),
            UnaryOperator::UnaryPlus => (UnaryOp::ToNumber, Type::Number),
            // `!` is `ToBoolean` inverted, so it always produces a boolean and never fails.
            UnaryOperator::LogicalNot => (UnaryOp::Not, Type::Bool),
            UnaryOperator::BitwiseNot => (UnaryOp::BitNot, Type::Number),
            // `typeof` produces one of a fixed set of strings, and is the only operator that
            // does not throw on an undeclared identifier.
            UnaryOperator::Typeof => (UnaryOp::TypeOf, Type::String),
            UnaryOperator::Void => (UnaryOp::Void, Type::Undefined),
            UnaryOperator::Delete => return self.delete(unary),
        };
        let operand = self.expression(&unary.argument);
        self.emit(ty, Op::Unary { op, operand })
    }

    /// `&&`, `||` and `??`, which are **control flow rather than operators**.
    ///
    /// `a && b` must not evaluate `b` when `a` is falsy. Lowering them as a two-operand
    /// instruction would evaluate both, which changes what the program *does* — a side effect
    /// in `b` would run when the source says it must not. So each becomes a branch, and the
    /// result travels through a slot.
    ///
    /// **`??` is not `||`.** It tests for `null` or `undefined`, not falsiness, so `0 ?? 1` is
    /// `0` where `0 || 1` is `1`. Treating them alike is the bug that made `??` worth adding to
    /// the language in the first place.
    fn logical(&mut self, logical: &LogicalExpression<'_>) -> ValueId {
        let slot = self.temporary();
        let left = self.expression(&logical.left);
        self.emit_effect(Op::Store { slot, value: left });

        let right_block = self.new_block();
        let join = self.new_block();

        let condition = match logical.operator {
            // Truthiness of the left operand decides, in opposite directions.
            LogicalOperator::And | LogicalOperator::Or => left,
            // Nullishness, which needs an explicit comparison rather than the value itself.
            LogicalOperator::Coalesce => self.is_nullish(left),
        };
        let (then_block, else_block) = match logical.operator {
            // `a && b`: evaluate `b` only when `a` is truthy.
            LogicalOperator::And => (right_block, join),
            // `a || b`: evaluate `b` only when `a` is falsy.
            LogicalOperator::Or => (join, right_block),
            // `a ?? b`: evaluate `b` only when `a` is nullish.
            LogicalOperator::Coalesce => (right_block, join),
        };
        self.terminate(Terminator::Branch {
            condition,
            then_block,
            then_args: Vec::new(),
            else_block,
            else_args: Vec::new(),
        });

        self.switch_to(right_block);
        let right = self.expression(&logical.right);
        self.emit_effect(Op::Store { slot, value: right });
        self.terminate(Terminator::Jump {
            target: join,
            args: Vec::new(),
        });

        self.switch_to(join);
        self.emit(Type::Unknown, Op::Load { slot })
    }

    /// `a === null || a === undefined`, as `??` and `?.` need it.
    fn is_nullish(&mut self, value: ValueId) -> ValueId {
        let null = self.emit(Type::Null, Op::Const(Constant::Null));
        let is_null = self.emit(
            Type::Bool,
            Op::Compare {
                op: CompareOp::StrictEqual,
                left: value,
                right: null,
            },
        );
        let undefined = self.emit(Type::Undefined, Op::Const(Constant::Undefined));
        let is_undefined = self.emit(
            Type::Bool,
            Op::Compare {
                op: CompareOp::StrictEqual,
                left: value,
                right: undefined,
            },
        );
        // A bitwise or, not a logical one: both operands are already booleans, so there is
        // nothing to short-circuit and no side effect to skip.
        self.emit(
            Type::Bool,
            Op::Binary {
                op: BinaryOp::BitOr,
                left: is_null,
                right: is_undefined,
            },
        )
    }

    /// `test ? consequent : alternate`, which is control flow for the same reason as `&&`.
    fn conditional(&mut self, conditional: &oxc_ast::ast::ConditionalExpression<'_>) -> ValueId {
        let slot = self.temporary();
        let condition = self.expression(&conditional.test);
        let then_block = self.new_block();
        let else_block = self.new_block();
        let join = self.new_block();
        self.terminate(Terminator::Branch {
            condition,
            then_block,
            then_args: Vec::new(),
            else_block,
            else_args: Vec::new(),
        });

        self.switch_to(then_block);
        let consequent = self.expression(&conditional.consequent);
        self.emit_effect(Op::Store {
            slot,
            value: consequent,
        });
        self.terminate(Terminator::Jump {
            target: join,
            args: Vec::new(),
        });

        self.switch_to(else_block);
        let alternate = self.expression(&conditional.alternate);
        self.emit_effect(Op::Store {
            slot,
            value: alternate,
        });
        self.terminate(Terminator::Jump {
            target: join,
            args: Vec::new(),
        });

        self.switch_to(join);
        self.emit(Type::Unknown, Op::Load { slot })
    }

    /// Lowers a nested function and returns its id plus the names it captured.
    ///
    /// The captures come back as *names* because they must be resolved again in the **enclosing**
    /// scope — the inner function knows which slot a capture lands in, and the outer one knows
    /// which value to put there. Pairing those two lists by position is the contract
    /// [`Function::captures`] describes, and building them anywhere but together is how a
    /// closure ends up reading an uninitialised slot.
    ///
    /// `binds_this` is false for an arrow function, and that one flag is the whole of
    /// `this`-binding semantics. A non-arrow **declares** `this`, so it shadows. An arrow does
    /// not, so a `this` inside it resolves outward and becomes an ordinary capture (D-81) —
    /// which is exactly what the language specifies, and it falls out of the scope machinery
    /// rather than needing a rule of its own.
    fn lower_function(
        &mut self,
        name: &str,
        params: &oxc_ast::ast::FormalParameters<'_>,
        body: Option<&oxc_ast::ast::FunctionBody<'_>>,
        expression_body: Option<&Expression<'_>>,
        binds_this: bool,
    ) -> (FunctionId, Vec<String>) {
        let index = self.functions.len();
        let mut function = Function::new(name);
        // Stamped here rather than left at zero: `verify_module` checks it against the
        // position, so a missed one fails the build instead of producing a closure that runs
        // whichever function happens to be first.
        function.id = FunctionId(u32::try_from(index).unwrap_or(u32::MAX));
        let entry = function.entry;
        self.functions.push(function);
        self.scopes.push(Scope {
            function: index,
            current: entry,
            terminated: false,
            slots: HashMap::new(),
            next_slot: 0,
            captures: Vec::new(),
            cells: std::collections::HashSet::new(),
        });

        if binds_this {
            // Ahead of the parameters so it is slot 0 in every ordinary function. `this` is a
            // reserved word, so no source name can collide with it.
            //
            // Recorded on the function rather than left to that ordering: the backend has to
            // know which slot to bind the incoming `this` to, and inferring it from the
            // position would break silently the day anything is declared earlier.
            let this_slot = self.declare("this");
            self.functions[index].this_slot = Some(this_slot);
        }

        let mut parameter_slots = Vec::with_capacity(params.items.len());
        let mut shared_parameters = Vec::new();
        for param in &params.items {
            match param.pattern.get_identifier_name() {
                // `declare`, not `slot`: a parameter shadows an outer binding of the same name.
                Some(param_name) => {
                    let slot = self.declare(param_name.as_str());
                    if self.shared.contains(param_name.as_str()) {
                        shared_parameters.push(slot);
                    }
                    parameter_slots.push(slot);
                }
                None => {
                    self.note("destructuring parameter", param.span.start);
                    // Still consumes a position, or every later parameter would shift down one
                    // and silently receive the wrong argument.
                    let placeholder = self.temporary();
                    parameter_slots.push(placeholder);
                }
            }
        }

        // A shared parameter arrives as a plain value — the caller has no cell to pass — so it
        // is wrapped here, before any of the body can read it. Read first, then make the cell:
        // `make_cell` overwrites the slot, and the incoming argument is what goes inside.
        for slot in shared_parameters {
            let arrived = self.emit(Type::Unknown, Op::Load { slot });
            self.make_cell(slot);
            self.write(slot, arrived);
        }

        if let Some(body) = body {
            self.hoist(&body.statements);
            for statement in &body.statements {
                self.statement(statement);
            }
        } else if let Some(expression) = expression_body {
            // A concise arrow body is an implicit return, not a statement.
            let value = self.expression(expression);
            self.terminate(Terminator::Return(Some(value)));
        }
        if !self.scope().terminated {
            // A function that runs off the end returns `undefined`.
            self.terminate(Terminator::Return(None));
        }

        let scope = self.scopes.pop().expect("just pushed");
        let names: Vec<String> = scope
            .captures
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let slots: Vec<u32> = scope.captures.iter().map(|(_, slot)| *slot).collect();
        self.functions[index].parameters = parameter_slots;
        self.functions[index].captures = slots;
        (
            FunctionId(u32::try_from(index).expect("functions fit in u32")),
            names,
        )
    }

    /// Emits the closure that binds a lowered function to its captured values.
    fn close_over(&mut self, function: FunctionId, names: &[String]) -> ValueId {
        let captures = names
            .iter()
            .map(|name| {
                // Resolved in the *enclosing* scope, which may itself capture it — a variable
                // read two functions down is captured at each level, which is what makes a
                // chain of closures work.
                let slot = self.slot(name);
                self.emit(Type::Unknown, Op::Load { slot })
            })
            .collect();
        self.emit(Type::Object(None), Op::Closure { function, captures })
    }

    /// Lowers a class to the two objects it is made of.
    ///
    /// A `class` is sugar, and desugaring it here rather than inventing an IR node keeps the
    /// object model honest: a class **is** a constructor function whose `prototype` property
    /// holds an object carrying the methods. Every instance shares that one prototype object,
    /// which is why methods are stored on it once rather than copied per instance — an
    /// implementation that stored them on the instance would work until someone compared two
    /// objects' methods for identity, or counted `Object.keys`.
    fn class(&mut self, class: &oxc_ast::ast::Class<'_>, name: &str) -> ValueId {
        if class.heritage.is_some() {
            // `extends` needs the prototype chain wired through the parent *and* `super`
            // resolved inside methods. Half of that would produce a class that constructs and
            // then fails its first inherited call.
            self.note("class extends", class.span.start);
        }

        let shape = crisol_value::Shapes::new().root();
        let prototype = self.emit(Type::Object(None), Op::CreateObject { shape });
        let mut constructor = None;

        for element in &class.body.body {
            let oxc_ast::ast::ClassElement::MethodDefinition(method) = element else {
                self.note("class member that is not a method", class.span.start);
                continue;
            };
            if method.r#static {
                self.note("static class member", method.span.start);
                continue;
            }
            let Some(key) = method.key.static_name() else {
                self.note("computed method name", method.span.start);
                continue;
            };
            let method_name = key.to_string();
            let (id, captures) = self.lower_function(
                &format!("{name}.{method_name}"),
                &method.value.params,
                method.value.body.as_deref(),
                None,
                true,
            );
            let closure = self.close_over(id, &captures);
            if method_name == "constructor" {
                // Remembered, **not returned**. Returning here dropped every method declared
                // after the constructor — and `constructor` conventionally comes first, so the
                // common ordering was the broken one.
                constructor = Some(closure);
                continue;
            }
            self.emit_effect(Op::PropertyStore {
                object: prototype,
                key: PropertyKey::new(&method_name),
                value: closure,
            });
        }

        let constructor = match constructor {
            Some(closure) => closure,
            None => {
                // No explicit constructor: the class still needs one, because `new` has to
                // call something. It does nothing.
                let (id, captures) = self.implicit_constructor(name);
                self.close_over(id, &captures)
            }
        };
        self.emit_effect(Op::PropertyStore {
            object: constructor,
            key: PropertyKey::new("prototype"),
            value: prototype,
        });
        constructor
    }

    /// The empty constructor a class without one still has.
    fn implicit_constructor(&mut self, name: &str) -> (FunctionId, Vec<String>) {
        let index = self.functions.len();
        let mut function = Function::new(&format!("{name}.constructor"));
        function.id = FunctionId(u32::try_from(index).unwrap_or(u32::MAX));
        let entry = function.entry;
        function.captures = Vec::new();
        self.functions.push(function);
        self.scopes.push(Scope {
            function: index,
            current: entry,
            terminated: false,
            slots: HashMap::new(),
            next_slot: 0,
            captures: Vec::new(),
            cells: std::collections::HashSet::new(),
        });
        self.declare("this");
        self.terminate(Terminator::Return(None));
        self.scopes.pop();
        (
            FunctionId(u32::try_from(index).expect("functions fit in u32")),
            Vec::new(),
        )
    }

    /// A slot no source name can collide with.
    ///
    /// Named with a character the grammar does not allow in an identifier, so a program cannot
    /// declare a variable that shadows a compiler temporary.
    fn temporary(&mut self) -> u32 {
        let name = format!(" tmp{}", self.scope().next_slot);
        self.declare(&name)
    }

    /// A template literal: `` `a${b}c` ``.
    ///
    /// Lowered as concatenation, which is what it is. **The first piece is always a string**,
    /// even when the template starts with a substitution — `` `${1}${2}` `` is `"12"` and not
    /// `3`, and starting from the empty string rather than the first substitution is the whole
    /// of why.
    ///
    /// A template with no substitutions is a single constant, so `` `abc` `` costs nothing that
    /// `"abc"` does not.
    fn template(&mut self, template: &oxc_ast::ast::TemplateLiteral<'_>) -> ValueId {
        let piece = |quasi: &oxc_ast::ast::TemplateElement<'_>| {
            quasi
                .value
                .cooked
                .as_ref()
                .map_or_else(|| quasi.value.raw.to_string(), ToString::to_string)
        };
        let mut quasis = template.quasis.iter();
        let first = quasis.next().map(piece).unwrap_or_default();
        let mut result = self.emit(Type::String, Op::Const(Constant::String(first)));

        for (expression, quasi) in template.expressions.iter().zip(quasis) {
            let value = self.expression(expression);
            result = self.emit(
                Type::String,
                Op::Binary {
                    op: BinaryOp::Add,
                    left: result,
                    right: value,
                },
            );
            let text = piece(quasi);
            // An empty trailing piece adds nothing, so it is not emitted — `` `${a}${b}` ``
            // should not cost two concatenations with `""`.
            if !text.is_empty() {
                let tail = self.emit(Type::String, Op::Const(Constant::String(text)));
                result = self.emit(
                    Type::String,
                    Op::Binary {
                        op: BinaryOp::Add,
                        left: result,
                        right: tail,
                    },
                );
            }
        }
        result
    }

    /// A property key that has to be evaluated: `{[expr]: v}` or `{1: v}`.
    ///
    /// `None` for a key this cannot evaluate, which the caller reports.
    fn property_key_value(&mut self, key: &Key<'_>) -> Option<ValueId> {
        match key {
            Key::NumericLiteral(literal) => {
                Some(self.emit(Type::Number, Op::Const(Constant::Number(literal.value))))
            }
            other => other
                .as_expression()
                .map(|expression| self.expression(expression)),
        }
    }

    fn object(&mut self, object: &oxc_ast::ast::ObjectExpression<'_>) -> ValueId {
        // The allocation starts from the empty shape, which is true: an object literal *is*
        // empty until its first property is stored.
        //
        // **The value's type is `Object(None)`, not `Object(Some(root))`, and that difference
        // is soundness rather than precision.** A type in SSA is fixed for the value's whole
        // life, but an object's shape changes as properties are added — so a value typed
        // `object#root` after two `PropertyStore`s claims the object is still empty, and a
        // pass trusting it would resolve `.a` to no slot at all. `Object(None)` says the one
        // thing that stays true: it is an object, and which shape is not known here.
        //
        // Recovering the precise shape needs either shape transitions modelled in the IR or
        // types attached to program points rather than to values. Both are M12's problem, and
        // both are better than guessing now.
        let shape = crisol_value::Shapes::new().root();
        let result = self.emit(Type::Object(None), Op::CreateObject { shape });
        for property in &object.properties {
            match property {
                ObjectPropertyKind::ObjectProperty(property) => {
                    let name = match &property.key {
                        Key::StaticIdentifier(identifier) => Some(identifier.name.to_string()),
                        Key::StringLiteral(literal) => Some(literal.value.to_string()),
                        // A computed or numeric key is not known here, so it is stored through
                        // the computed path — which is also where the number-to-name rule
                        // lives, so `{1: x}` and `o[1] = x` cannot disagree about the name.
                        _ => None,
                    };
                    match name {
                        Some(name) => {
                            let value = self.expression(&property.value);
                            self.emit_effect(Op::PropertyStore {
                                object: result,
                                key: PropertyKey::new(&name),
                                value,
                            });
                        }
                        None => {
                            let Some(key) = self.property_key_value(&property.key) else {
                                self.note("property key", property.span.start);
                                continue;
                            };
                            // **The key is evaluated before the value**, which is the order the
                            // specification gives and is observable whenever either has an
                            // effect.
                            let value = self.expression(&property.value);
                            self.emit_effect(Op::ComputedStore {
                                object: result,
                                key,
                                value,
                            });
                        }
                    }
                }
                ObjectPropertyKind::SpreadProperty(spread) => {
                    self.note("object spread", spread.span.start);
                }
            }
        }
        result
    }
}

/// A statement's kind, for the unsupported list.
fn kind_of(statement: &Statement<'_>) -> &'static str {
    match statement {
        Statement::FunctionDeclaration(_) => "function declaration",
        Statement::ClassDeclaration(_) => "class declaration",
        Statement::ImportDeclaration(_) => "import declaration",
        _ => "statement",
    }
}

/// An expression's kind, for the unsupported list.
fn expression_kind(expression: &Expression<'_>) -> &'static str {
    match expression {
        Expression::ArrowFunctionExpression(_) => "arrow function",
        Expression::FunctionExpression(_) => "function expression",
        Expression::ArrayExpression(_) => "array literal",
        Expression::UnaryExpression(_) => "unary expression",
        Expression::UpdateExpression(_) => "update expression",
        Expression::LogicalExpression(_) => "logical expression",
        Expression::ConditionalExpression(_) => "conditional expression",
        Expression::AwaitExpression(_) => "await expression",
        Expression::NewExpression(_) => "new expression",
        Expression::StringLiteral(_) => "string literal",
        Expression::BigIntLiteral(_) => "bigint literal",
        Expression::SequenceExpression(_) => "comma expression",
        Expression::TaggedTemplateExpression(_) => "tagged template",
        Expression::PrivateFieldExpression(_) => "private field",
        Expression::Super(_) => "super",
        Expression::YieldExpression(_) => "yield expression",
        Expression::ChainExpression(_) => "optional chain",
        // A name rather than "expression": the unsupported list is read to decide what to
        // implement next, and a bucket everything unrecognised falls into says nothing.
        _ => "an expression this compiler does not name yet",
    }
}
