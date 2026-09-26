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
    ArrayExpressionElement, BinaryExpression, BinaryOperator, Expression, LogicalExpression,
    LogicalOperator, ObjectPropertyKind, Program, PropertyKey as Key, Statement, UnaryExpression,
    UnaryOperator,
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
    /// Where an unlabelled `break` goes, innermost last.
    breaks: Vec<BlockId>,
    /// Where a raised exception goes, innermost last. Empty means out of the function.
    handlers: Vec<BlockId>,
    /// Where `continue` goes, innermost last. Separate from `breaks` because a `switch` is a
    /// `break` target and not a `continue` one.
    continues: Vec<BlockId>,
    /// Set while lowering a generator body: the resume points `yield` has produced. `None` for an
    /// ordinary function, so a `yield` outside a generator is refused rather than miscompiled.
    generator: Option<GenState>,
    /// The type of each SSA value this function has produced, indexed by [`ValueId::index`].
    ///
    /// Kept so a lowering step can ask an operand's type — arithmetic needs it to decide whether
    /// a result is a Number (both operands provably numbers, so it cannot be a BigInt and needs
    /// no rooting) or `Unknown` (a BigInt is possible, so the collector must be able to see it).
    /// Per scope because a `ValueId` is numbered within one function (D-248).
    value_types: Vec<Type>,
}

/// What a generator body accumulates as it lowers: one resume point per `yield`, and which slots
/// are the body's own locals (kept on the generator object so they survive a suspension).
struct GenState {
    /// The next resume-state number to hand out; `0` is the body's start.
    next_state: u32,
    /// `(state, block)` for each `yield`'s resume point, for the entry dispatch.
    resumes: Vec<(u32, BlockId)>,
    /// Slots that are the body's own locals or parameters, with the hidden key they live under on
    /// the generator object. A `Variable` would be lost on suspension; a property on the object is
    /// not — which is what lets a loop counter survive a `yield`. Captures are **not** in here:
    /// they are re-loaded from the closure at every resume, so they persist on their own.
    locals: HashMap<u32, String>,
    /// Whether this is an async function's body rather than a generator's (D-249). An async body
    /// suspends on `await` — lowered through the same `yield` machinery — and the outer drives it
    /// with a promise instead of returning it. `yield` is refused here and `await` in a plain
    /// generator.
    is_async: bool,
}

// **The three jump-target stacks live here and not on `Lowering`, because a `BlockId` names a
// block *within one function* and nothing about the type says which.** Held module-wide, a
// function defined inside a `try` inherited the enclosing function's catch block — and since
// block numbering restarts per function, that id also existed in the nested one, so the
// verifier's `NoSuchBlock` check saw nothing wrong and the jump silently went to a block of
// the nested function's own. For `[1, 2].slice({valueOf: function () { throw … }})` the id
// landed on the very block doing the jumping, which Cranelift emitted as `b .`: the program
// did not throw, it spun for ever (D-206). Per-scope, the stacks start empty for every nested
// function and an arrow, which is also what the language says — a `throw` inside a function
// leaves that function, and an enclosing `try` catches it at the *call*, not at the throw.

struct Lowering {
    functions: Vec<Function>,
    /// Innermost last. A nested function pushes; finishing it pops.
    scopes: Vec<Scope>,
    unsupported: Vec<Unsupported>,
    /// Names a closure must share rather than copy, from [`crate::escape`].
    shared: std::collections::HashSet<String>,
    /// Functions that would bind `arguments` if something asked for it, innermost last.
    ///
    /// An arrow is absent from this, which is what makes it inherit the enclosing function's —
    /// the same rule `this` follows, and it falls out of the lookup rather than being a case.
    binds_arguments: Vec<usize>,
}

/// The property a cell keeps its value in.
///
/// A cell is an ordinary one-property object, so it needs no new IR operation and no new
/// runtime call — it allocates, stores and loads exactly like an object literal. That is
/// slower than a dedicated representation and is the right first version: correctness now,
/// and a measurement before inventing machinery to make it faster.
const CELL_KEY: &str = "value";

/// The hidden keys the generator lowering and the runtime's `crisol_make_generator` share: the
/// resume state, the value passed to `next`, the value `yield` produced, and the return value.
const GEN_STATE_KEY: &str = "__genState";
const GEN_SENT_KEY: &str = "__genSent";
const GEN_YIELDED_KEY: &str = "__genYielded";
const GEN_RETURN_KEY: &str = "__genReturn";
/// The caller's `this`, kept on the generator object because the body's own `this` is the object.
const GEN_THIS_KEY: &str = "__genThis";
/// The body's return value tells the runtime what it did: `0` yielded, `1` finished.
const GEN_YIELD_SIGNAL: f64 = 0.0;
const GEN_DONE_SIGNAL: f64 = 1.0;

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
            binds_arguments: Vec::new(),
            scopes: vec![Scope {
                function: 0,
                current: entry,
                terminated: false,
                slots: HashMap::new(),
                next_slot: 0,
                captures: Vec::new(),
                cells: std::collections::HashSet::new(),
                breaks: Vec::new(),
                handlers: Vec::new(),
                continues: Vec::new(),
                generator: None,
                value_types: Vec::new(),
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

    /// The type the IR gave a value, or `Unknown` if it was produced somewhere that did not record
    /// one (a block parameter, say). `Unknown` is the safe answer — it only ever causes *more*
    /// rooting, never less.
    fn type_of(&self, id: ValueId) -> Type {
        self.scope()
            .value_types
            .get(id.index() as usize)
            .copied()
            .unwrap_or(Type::Unknown)
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
        let hoisted = declaration.kind.is_var();
        for declarator in &declaration.declarations {
            // **A `var` with no initialiser does nothing here.** The hoist already set it to
            // `undefined`; assigning again would clobber a value an earlier statement gave it,
            // which is what `var x;` after `x = 1` must not do. Only a plain identifier can be
            // written without an initialiser — a destructuring declaration's grammar requires
            // one — so this stays on the identifier fast path.
            if let oxc_ast::ast::BindingPattern::BindingIdentifier(identifier) = &declarator.id {
                if hoisted && declarator.init.is_none() {
                    continue;
                }
                let value = match &declarator.init {
                    Some(init) => self.value_expression(init),
                    None => self.emit(Type::Undefined, Op::Const(Constant::Undefined)),
                };
                if hoisted {
                    // The binding already exists, from the hoist. `slot` finds it; `declare`
                    // would make a second one and leave every reader of the first looking at
                    // `undefined`.
                    let slot = self.slot(identifier.name.as_str());
                    self.write(slot, value);
                } else {
                    // `declare`, not `slot`: a `let` shadows an outer binding rather than
                    // capturing it.
                    let slot = self.declare(identifier.name.as_str());
                    self.bind(identifier.name.as_str(), slot, value);
                }
                continue;
            }
            let value = match &declarator.init {
                Some(init) => self.expression(init),
                None => self.emit(Type::Undefined, Op::Const(Constant::Undefined)),
            };
            self.bind_pattern(&declarator.id, value, hoisted, declarator.span.start);
        }
    }

    /// Binds `value` to a destructuring `pattern`, declaring (or, for a hoisted `var`, writing)
    /// each name it names. Recursive, because a pattern nests: `{a: [b, c]}` reads `a` and then
    /// destructures the array it holds. `span` is only for the notes the unsupported parts emit.
    ///
    /// **Array elements are read through `Op::Iterate`**, the same list `for-of` walks, rather
    /// than the specification's step-by-step iterator protocol — so it covers arrays and strings
    /// and matches this engine's `for-of`, and a `.return()` on early completion is not observed.
    /// Object properties are read by name (or through the computed path), left to right.
    fn bind_pattern(
        &mut self,
        pattern: &oxc_ast::ast::BindingPattern<'_>,
        value: ValueId,
        hoisted: bool,
        span: u32,
    ) {
        match pattern {
            oxc_ast::ast::BindingPattern::BindingIdentifier(identifier) => {
                if hoisted {
                    let slot = self.slot(identifier.name.as_str());
                    self.write(slot, value);
                } else {
                    let slot = self.declare(identifier.name.as_str());
                    self.bind(identifier.name.as_str(), slot, value);
                }
            }
            // `left = default`: the default is taken only when the value is `undefined`, and its
            // expression runs only then, because it may have effects.
            oxc_ast::ast::BindingPattern::AssignmentPattern(assignment) => {
                let resolved = self.default_if_undefined(value, &assignment.right);
                self.bind_pattern(&assignment.left, resolved, hoisted, span);
            }
            oxc_ast::ast::BindingPattern::ObjectPattern(object) => {
                if object.rest.is_some() {
                    // A rest element gathers the remaining own enumerable keys into a fresh
                    // object — a runtime copy this does not have yet.
                    self.note("object rest pattern", span);
                }
                for property in &object.properties {
                    let read = self.read_binding_key(value, &property.key, property.computed, span);
                    self.bind_pattern(&property.value, read, hoisted, span);
                }
            }
            oxc_ast::ast::BindingPattern::ArrayPattern(array) => {
                if array.rest.is_some() {
                    self.note("array rest pattern", span);
                }
                // `Op::Iterate` raises on a non-iterable, so `let [a] = null` throws as it must —
                // the signal has to be honoured here or the reads below would run over it.
                let iterated = self.emit(Type::Object(None), Op::Iterate { object: value });
                let values = self.propagate(iterated);
                for (index, element) in array.elements.iter().enumerate() {
                    // A hole (`[, a]`) binds nothing but still advances the position.
                    let Some(pattern) = element else {
                        continue;
                    };
                    #[expect(clippy::cast_precision_loss, reason = "a destructuring arity")]
                    let position = index as f64;
                    let key = self.emit(Type::Number, Op::Const(Constant::Number(position)));
                    let read = self.emit(
                        Type::Unknown,
                        Op::ComputedLoad {
                            object: values,
                            key,
                        },
                    );
                    let read = self.propagate(read);
                    self.bind_pattern(pattern, read, hoisted, span);
                }
            }
        }
    }

    /// Reads the property a binding pattern's key names — `{a}` and `{a: x}` by name, `{[k]: x}`
    /// and `{0: x}` through the computed path, where the number-to-name rule lives.
    fn read_binding_key(
        &mut self,
        object: ValueId,
        key: &Key<'_>,
        computed: bool,
        span: u32,
    ) -> ValueId {
        let name = if computed {
            None
        } else {
            match key {
                Key::StaticIdentifier(identifier) => Some(identifier.name.to_string()),
                Key::StringLiteral(literal) => Some(literal.value.to_string()),
                _ => None,
            }
        };
        let read = match name {
            Some(name) => self.emit(
                Type::Unknown,
                Op::PropertyLoad {
                    object,
                    key: PropertyKey::new(&name),
                },
            ),
            None => match self.property_key_value(key) {
                Some(key) => self.emit(Type::Unknown, Op::ComputedLoad { object, key }),
                None => {
                    self.note("property key", span);
                    self.emit(Type::Undefined, Op::Const(Constant::Undefined))
                }
            },
        };
        self.propagate(read)
    }

    /// `value` unless it is `undefined`, in which case the `default` expression — evaluated only
    /// then, since it may have effects. The branch-and-join a conditional uses (see
    /// [`Self::conditional`]).
    fn default_if_undefined(&mut self, value: ValueId, default: &Expression<'_>) -> ValueId {
        let slot = self.temporary();
        let undefined = self.emit(Type::Undefined, Op::Const(Constant::Undefined));
        let missing = self.emit(
            Type::Bool,
            Op::Compare {
                op: CompareOp::StrictEqual,
                left: value,
                right: undefined,
            },
        );
        let then_block = self.new_block();
        let else_block = self.new_block();
        let join = self.new_block();
        self.terminate(Terminator::Branch {
            condition: missing,
            then_block,
            then_args: Vec::new(),
            else_block,
            else_args: Vec::new(),
        });

        self.switch_to(then_block);
        let fallback = self.expression(default);
        self.emit_effect(Op::Store {
            slot,
            value: fallback,
        });
        self.terminate(Terminator::Jump {
            target: join,
            args: Vec::new(),
        });

        self.switch_to(else_block);
        self.emit_effect(Op::Store { slot, value });
        self.terminate(Terminator::Jump {
            target: join,
            args: Vec::new(),
        });

        self.switch_to(join);
        self.emit(Type::Unknown, Op::Load { slot })
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

        self.scope_mut().breaks.push(exit);
        self.scope_mut().continues.push(step);
        self.statement(body_statement);
        self.scope_mut().breaks.pop();
        self.scope_mut().continues.pop();
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
                match first.id.get_identifier_name() {
                    Some(name) => {
                        let slot = self.declare(name.as_str());
                        self.bind(name.as_str(), slot, value);
                    }
                    // `for (const [a, b] of pairs)` / `for (const {x} in obj)`: the loop variable
                    // is a destructuring pattern, bound afresh each iteration. `let`-scoped, so
                    // `declare` (hoisted = false).
                    None => self.bind_pattern(&first.id, value, false, declaration.span.start),
                }
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
        self.scope_mut().breaks.push(exit);
        self.scope_mut().continues.push(update);
        self.statement(&statement.body);
        self.scope_mut().breaks.pop();
        self.scope_mut().continues.pop();
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
        match self.scope().handlers.last().copied() {
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

        self.scope_mut().handlers.push(handler);
        for inner in &statement.block.body {
            self.statement(inner);
        }
        self.scope_mut().handlers.pop();
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

        self.scope_mut().breaks.push(end);
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
        self.scope_mut().breaks.pop();
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
        // **`var` is hoisted too, and that is not a detail.** A `var` is function-scoped, so
        // its name exists from the top of the function whatever line declares it. Declaring it
        // where it appears left every hoisted function above it unable to see it — and
        // test262's `propertyHelper.js` is exactly that shape: `var __getOwnPropertyDescriptor
        // = …` at the top of the file, read by `verifyProperty`, a hoisted function lowered
        // before the assignment was reached. The name resolved to nothing, became a global
        // load, and the case failed with `__getOwnPropertyDescriptor is not defined`.
        let mut names = Vec::new();
        collect_var_names(statements, &mut names);
        for name in names {
            if self.scope().slots.contains_key(&name) {
                continue;
            }
            let slot = self.declare(&name);
            // Hoisted means *declared*, not assigned: reading before the declaring statement
            // runs gives `undefined`, which is what distinguishes `var` from `let`.
            let undefined = self.emit(Type::Undefined, Op::Const(Constant::Undefined));
            self.write(slot, undefined);
            if self.shared.contains(&name) {
                self.make_cell(slot);
            }
        }

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
            let (id, captures) = if declaration.r#async && declaration.generator {
                // An async generator needs `Symbol.asyncIterator` and a queue of pending reads —
                // a separate feature. Lowered as a plain function, which then refuses the `yield`
                // and `await` inside rather than miscompiling them.
                self.note("async generator", declaration.span.start);
                self.lower_function(
                    name,
                    &declaration.params,
                    declaration.body.as_deref(),
                    None,
                    true,
                )
            } else if declaration.generator || declaration.r#async {
                self.lower_generator(
                    name,
                    &declaration.params,
                    declaration.body.as_deref(),
                    declaration.r#async,
                )
            } else {
                self.lower_function(
                    name,
                    &declaration.params,
                    declaration.body.as_deref(),
                    None,
                    true,
                )
            };
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
        // Record the result's type so a later step can ask an operand's — arithmetic uses it to
        // tell a Number result (needs no rooting) from one that might be a BigInt (does).
        let index = result.index() as usize;
        let scope = self.scope_mut();
        if scope.value_types.len() <= index {
            scope.value_types.resize(index + 1, Type::Unknown);
        }
        scope.value_types[index] = ty;
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
        // A generator local is a property of the generator object (`this`), read fresh each time so
        // a value stored before a suspension is seen after it.
        if let Some(key) = self.gen_local_key(slot) {
            let this_slot = self.this_slot();
            let this = self.emit(Type::Unknown, Op::Load { slot: this_slot });
            return self.emit(
                Type::Unknown,
                Op::PropertyLoad {
                    object: this,
                    key: PropertyKey::new(&key),
                },
            );
        }
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

    /// Writes a variable, through its cell when it has one, or onto the generator object when it is
    /// a generator local.
    fn write(&mut self, slot: u32, value: ValueId) {
        if let Some(key) = self.gen_local_key(slot) {
            let this_slot = self.this_slot();
            let this = self.emit(Type::Unknown, Op::Load { slot: this_slot });
            self.emit_effect(Op::PropertyStore {
                object: this,
                key: PropertyKey::new(&key),
                value,
            });
            return;
        }
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

    /// The hidden key a slot's value lives under on the generator object, or `None` when the slot is
    /// not a generator local (an ordinary function, `this`, or a capture).
    fn gen_local_key(&self, slot: u32) -> Option<String> {
        self.scope().generator.as_ref()?.locals.get(&slot).cloned()
    }

    /// The slot holding the current function's `this` — the generator object, in a generator body.
    fn this_slot(&self) -> u32 {
        self.functions[self.scope().function]
            .this_slot
            .expect("a function that reads a generator local binds `this`")
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
    /// Binds `arguments` in the nearest enclosing function that has one, on first mention.
    ///
    /// **Lazily, and that is the point.** A function that never names `arguments` keeps the
    /// slot numbering it had before the feature existed, and its prologue builds nothing.
    /// Declaring the slot in every function shifted every parameter down by one and broke
    /// closures — visibly only under GC stress, because the damage was to the frame the
    /// collector reads rather than to any value a test printed.
    ///
    /// Returns the slot in the *current* scope, which for an arrow is the capture the ordinary
    /// machinery just created.
    fn bind_arguments(&mut self) -> Option<u32> {
        let owner = *self.binds_arguments.last()?;
        // Declared in the owning scope, which is the innermost one belonging to that function.
        let depth = self
            .scopes
            .iter()
            .rposition(|scope| scope.function == owner)?;
        let next = self.scopes[depth].next_slot;
        self.scopes[depth].next_slot += 1;
        self.scopes[depth]
            .slots
            .insert("arguments".to_owned(), next);
        self.functions[owner].arguments_slot = Some(next);
        // Now resolve it from here: inside the owner that is the slot just made, and inside an
        // arrow it walks out and becomes a capture, exactly as `this` does.
        Some(self.slot("arguments"))
    }

    fn resolves(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope.slots.contains_key(name))
    }

    fn slot(&mut self, name: &str) -> u32 {
        if name == "arguments"
            && !self.resolves(name)
            && let Some(slot) = self.bind_arguments()
        {
            return slot;
        }
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
            // A capture is not a generator local: it re-loads from the closure at every resume, so
            // it must not be redirected onto the generator object (where nothing stores it).
            if let Some(generator) = self.scope_mut().generator.as_mut() {
                generator.locals.remove(&slot);
            }
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
        // In a generator body a newly declared local lives on the generator object, so it survives
        // a suspension. `this` is the object itself and is never redirected; captures are removed
        // from this set again in `slot`, since they re-load from the closure on their own.
        if name != "this"
            && let Some(generator) = scope.generator.as_mut()
        {
            generator.locals.insert(slot, format!("$g_{name}"));
        }
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
                // A `yield e;` at statement level suspends and discards the sent value — the safe,
                // common position where nothing is live across the suspension.
                if self.is_plain_yield(&statement.expression) {
                    let Expression::YieldExpression(yield_expression) = &statement.expression
                    else {
                        unreachable!("is_plain_yield checked the shape")
                    };
                    self.lower_yield(yield_expression.argument.as_ref());
                } else if self.is_plain_await(&statement.expression) {
                    // `await e;` suspends and discards the settled value.
                    let Expression::AwaitExpression(await_expression) = &statement.expression
                    else {
                        unreachable!("is_plain_await checked the shape")
                    };
                    self.lower_yield(Some(&await_expression.argument));
                } else {
                    self.expression(&statement.expression);
                }
            }
            Statement::VariableDeclaration(declaration) => {
                self.variable_declaration(declaration);
            }
            Statement::ReturnStatement(statement) => {
                // Inside a generator, `return e` is not the function's return — it finishes the
                // generator with `e` as the result and hands the body's `DONE` signal back.
                if self.scope().generator.is_some() {
                    // `value_expression` so `return await p` / `return yield x` suspend first, then
                    // finish with the settled value.
                    let value = match &statement.argument {
                        Some(argument) => self.value_expression(argument),
                        None => self.emit(Type::Undefined, Op::Const(Constant::Undefined)),
                    };
                    let this_slot = self.slot("this");
                    let this = self.read(this_slot);
                    self.emit_effect(Op::PropertyStore {
                        object: this,
                        key: PropertyKey::new(GEN_RETURN_KEY),
                        value,
                    });
                    let done =
                        self.emit(Type::Number, Op::Const(Constant::Number(GEN_DONE_SIGNAL)));
                    self.terminate(Terminator::Return(Some(done)));
                } else {
                    let value = statement
                        .argument
                        .as_ref()
                        .map(|argument| self.expression(argument));
                    self.terminate(Terminator::Return(value));
                }
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
                self.scope_mut().breaks.push(exit);
                self.scope_mut().continues.push(header);
                self.statement(&statement.body);
                self.scope_mut().breaks.pop();
                self.scope_mut().continues.pop();
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
                self.scope_mut().breaks.push(exit);
                // `continue` goes to the *test*, not back to the top — it ends this iteration
                // rather than skipping the condition.
                self.scope_mut().continues.push(header);
                self.statement(&statement.body);
                self.scope_mut().breaks.pop();
                self.scope_mut().continues.pop();
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
                } else if let Some(target) = self.scope().continues.last().copied() {
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
                } else if let Some(target) = self.scope().breaks.last().copied() {
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
            // `literal.value` is already the base-10 digits the parser normalised `0xFFn` and
            // `0b101n` down to, so the runtime parses one radix and the frontend needs no
            // arbitrary-precision arithmetic of its own.
            Expression::BigIntLiteral(literal) => self.emit(
                Type::Unknown,
                Op::Const(Constant::BigInt(literal.value.to_string())),
            ),
            Expression::BooleanLiteral(literal) => {
                self.emit(Type::Bool, Op::Const(Constant::Bool(literal.value)))
            }
            Expression::NullLiteral(_) => self.emit(Type::Null, Op::Const(Constant::Null)),
            Expression::Identifier(identifier) => {
                if identifier.name == "undefined" {
                    return self.emit(Type::Undefined, Op::Const(Constant::Undefined));
                }
                // `arguments` resolves to a binding that does not exist until it is asked
                // for, so the check has to admit it — otherwise the first mention falls
                // through to a global load and reports the binding missing that it was about
                // to create.
                if self.resolves(identifier.name.as_str())
                    || (identifier.name == "arguments" && !self.binds_arguments.is_empty())
                {
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
                // The right side is evaluated first, so a `yield` there is safe: the target is
                // resolved afterwards, in the resume block, with nothing live across the suspension.
                let value = self.value_expression(&assignment.right);
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
                // `super.x` reads from the parent prototype; every other object reads from itself.
                let object = if matches!(member.object, Expression::Super(_)) {
                    let proto_slot = self.slot(" superproto");
                    self.read(proto_slot)
                } else {
                    self.expression(&member.object)
                };
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
                    // `super(...)`: call the captured parent constructor with the current `this`,
                    // which its body then initialises (crisol allocates `this` up front, so
                    // `super` initialises rather than allocates — the this-before-super TDZ is not
                    // enforced, D-243).
                    Expression::Super(_) => {
                        let super_slot = self.slot(" super");
                        let super_ctor = self.read(super_slot);
                        let this_slot = self.slot("this");
                        let this_value = self.read(this_slot);
                        (super_ctor, this_value)
                    }
                    // `super.m(...)`: the method comes from the parent prototype, but the receiver
                    // is the current `this`, not the prototype.
                    Expression::StaticMemberExpression(member)
                        if matches!(member.object, Expression::Super(_)) =>
                    {
                        let proto_slot = self.slot(" superproto");
                        let proto = self.read(proto_slot);
                        let method = self.emit(
                            Type::Unknown,
                            Op::PropertyLoad {
                                object: proto,
                                key: PropertyKey::new(member.property.name.as_str()),
                            },
                        );
                        let this_slot = self.slot("this");
                        (method, self.read(this_slot))
                    }
                    Expression::ComputedMemberExpression(member)
                        if matches!(member.object, Expression::Super(_)) =>
                    {
                        let proto_slot = self.slot(" superproto");
                        let proto = self.read(proto_slot);
                        let key = self.expression(&member.expression);
                        let method =
                            self.emit(Type::Unknown, Op::ComputedLoad { object: proto, key });
                        let this_slot = self.slot("this");
                        (method, self.read(this_slot))
                    }
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
                    // **`o[k]()` is a method call too.** Only the dotted form passed a
                    // receiver, so `a["push"](1)` and `it[Symbol.iterator]()` ran with `this`
                    // as `undefined` — and, as the note above says, losing it is silent: the
                    // call happens, something comes back, and only `this` is wrong.
                    Expression::ComputedMemberExpression(member) => {
                        let object = self.expression(&member.object);
                        let key = self.expression(&member.expression);
                        let method = self.emit(Type::Unknown, Op::ComputedLoad { object, key });
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
                // A spread argument (`f(...xs)`) makes the count dynamic, so the arguments are
                // gathered into an array and the call goes through `crisol_apply` (D-250). A call
                // with none stays a fixed-operand `Op::Call` and costs exactly what it did before.
                let spread = call
                    .arguments
                    .iter()
                    .any(|argument| matches!(argument, oxc_ast::ast::Argument::SpreadElement(_)));
                let result = if spread {
                    let array = self.emit(
                        Type::Object(None),
                        Op::CreateArray {
                            elements: Vec::new(),
                        },
                    );
                    for argument in &call.arguments {
                        if let oxc_ast::ast::Argument::SpreadElement(element) = argument {
                            let value = self.expression(&element.argument);
                            let extended = self.emit(
                                Type::Undefined,
                                Op::ArrayExtend {
                                    array,
                                    value,
                                    spread: true,
                                },
                            );
                            self.propagate(extended);
                        } else if let Some(expression) = argument.as_expression() {
                            let value = self.expression(expression);
                            self.emit_effect(Op::ArrayExtend {
                                array,
                                value,
                                spread: false,
                            });
                        }
                    }
                    self.emit(
                        Type::Unknown,
                        Op::CallSpread {
                            callee,
                            this_value,
                            arguments: array,
                        },
                    )
                } else {
                    let mut args = Vec::with_capacity(call.arguments.len());
                    for argument in &call.arguments {
                        if let Some(expression) = argument.as_expression() {
                            args.push(self.expression(expression));
                        }
                    }
                    self.emit(
                        Type::Unknown,
                        Op::Call {
                            callee,
                            this_value,
                            args,
                        },
                    )
                };
                self.propagate(result)
            }
            Expression::ObjectExpression(object) => self.object(object),
            Expression::FunctionExpression(function) => {
                let name = function
                    .id
                    .as_ref()
                    .map_or_else(|| "anonymous".to_owned(), |id| id.name.to_string());
                let (id, names) = if function.r#async && function.generator {
                    self.note("async generator", function.span.start);
                    self.lower_function(
                        &name,
                        &function.params,
                        function.body.as_deref(),
                        None,
                        true,
                    )
                } else if function.generator || function.r#async {
                    self.lower_generator(
                        &name,
                        &function.params,
                        function.body.as_deref(),
                        function.r#async,
                    )
                } else {
                    self.lower_function(
                        &name,
                        &function.params,
                        function.body.as_deref(),
                        None,
                        true,
                    )
                };
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
                let this = self.read(slot);
                // In a generator body the `this` slot is the generator object; the caller's `this`
                // is kept on it under `GEN_THIS_KEY`.
                if self.scope().generator.is_some() {
                    return self.emit(
                        Type::Unknown,
                        Op::PropertyLoad {
                            object: this,
                            key: PropertyKey::new(GEN_THIS_KEY),
                        },
                    );
                }
                this
            }
            Expression::YieldExpression(yield_expression) => {
                // A `yield` handled in a safe position (a statement, or the right of an initialiser
                // or simple assignment) never reaches here. Anywhere else it could leave a compiler
                // temporary live across the suspension, which this lowering cannot spill yet, so it
                // is refused rather than miscompiled.
                if self.scope().generator.is_none() {
                    self.note("yield outside a generator", yield_expression.span.start);
                } else {
                    self.note("yield in expression position", yield_expression.span.start);
                }
                self.placeholder()
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
                // **The leading run is built in one go and the rest is appended.** An array
                // with no spread costs exactly what it did before — one `CreateArray` — and
                // only what follows a spread pays for being appended one piece at a time.
                let mut spreading = false;
                let mut result = None;
                for element in &array.elements {
                    match element {
                        ArrayExpressionElement::SpreadElement(spread) => {
                            let array = *result.get_or_insert_with(|| {
                                self.emit(
                                    Type::Object(None),
                                    Op::CreateArray {
                                        elements: std::mem::take(&mut elements),
                                    },
                                )
                            });
                            let value = self.expression(&spread.argument);
                            let extended = self.emit(
                                Type::Undefined,
                                Op::ArrayExtend {
                                    array,
                                    value,
                                    spread: true,
                                },
                            );
                            self.propagate(extended);
                            spreading = true;
                        }
                        // A hole in `[1, , 3]`. **Not a spread and not `undefined`** (D-64):
                        // the IR still has no way to say "absent", so this is recorded rather
                        // than filled in with a value that reads the same and answers `in`
                        // differently.
                        ArrayExpressionElement::Elision(_) => {
                            self.note("array hole", array.span.start);
                            let placeholder = self.placeholder();
                            if spreading {
                                let array = result.unwrap_or(placeholder);
                                self.emit_effect(Op::ArrayExtend {
                                    array,
                                    value: placeholder,
                                    spread: false,
                                });
                            } else {
                                elements.push(placeholder);
                            }
                        }
                        other => {
                            let Some(expression) = other.as_expression() else {
                                continue;
                            };
                            let value = self.expression(expression);
                            if let Some(array) = result {
                                self.emit_effect(Op::ArrayExtend {
                                    array,
                                    value,
                                    spread: false,
                                });
                            } else {
                                elements.push(value);
                            }
                        }
                    }
                }
                result
                    .unwrap_or_else(|| self.emit(Type::Object(None), Op::CreateArray { elements }))
            }
            Expression::ComputedMemberExpression(member) => {
                let object = if matches!(member.object, Expression::Super(_)) {
                    let proto_slot = self.slot(" superproto");
                    self.read(proto_slot)
                } else {
                    self.expression(&member.object)
                };
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
        // **A result is a Number only when both operands are provably Numbers.** Then it cannot be
        // a BigInt or a string, so codegen may inline it and the collector need not root it. With
        // anything else an operand could be a BigInt — and every arithmetic operator on two
        // BigInts *produces* one (D-248), a heap reference the collector has to see — so the result
        // is `Unknown` and gets rooted. Typing it `Number` unconditionally, as this once did, left
        // a BigInt result invisible to the collector and freed under GC stress.
        let both_numbers =
            self.type_of(left) == Type::Number && self.type_of(right) == Type::Number;
        let ty = if op.is_always_boolean() {
            Type::Bool
        } else if matches!(op, BinaryOp::Add) {
            // `+` stays `unknown` even for two numbers: codegen never inlines it, and typing it a
            // Number could let a *consumer* inline a string concatenation as a float add. It may
            // also concatenate or add BigInts.
            Type::Unknown
        } else if both_numbers {
            Type::Number
        } else {
            Type::Unknown
        };
        let result = self.emit(ty, Op::Binary { op, left, right });
        // **`in` can always raise**, so it always checks.
        if matches!(op, BinaryOp::In) {
            return self.propagate(result);
        }
        // A BigInt operator raises on a type mismatch (`1n + 1`), a zero divisor (`1n / 0n`) or an
        // unsigned right shift. It cannot when both operands are proven Numbers — then `ty` is
        // `Number` — so the common numeric path stays a plain instruction with no unwind branch,
        // and only the `Unknown` case pays for the check.
        let arithmetic = matches!(
            op,
            BinaryOp::Add
                | BinaryOp::Subtract
                | BinaryOp::Multiply
                | BinaryOp::Divide
                | BinaryOp::Remainder
                | BinaryOp::Exponent
                | BinaryOp::BitAnd
                | BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::ShiftLeft
                | BinaryOp::ShiftRight
                | BinaryOp::UnsignedShiftRight
        );
        if arithmetic && ty == Type::Unknown {
            return self.propagate(result);
        }
        result
    }

    fn unary(&mut self, unary: &UnaryExpression<'_>) -> ValueId {
        let (op, ty) = match unary.operator {
            UnaryOperator::UnaryNegation => {
                let operand = self.expression(&unary.argument);
                // `-x` is a Number when `x` is one; on a BigInt it stays a BigInt — a reference
                // the collector must see — so an unproven operand yields `Unknown`, which roots it.
                let ty = if self.type_of(operand) == Type::Number {
                    Type::Number
                } else {
                    Type::Unknown
                };
                return self.emit(
                    ty,
                    Op::Unary {
                        op: UnaryOp::Negate,
                        operand,
                    },
                );
            }
            // `+` is always a Number: on a BigInt it throws, and the exception signal is a
            // singleton that needs no rooting, so `Number` stays sound.
            UnaryOperator::UnaryPlus => (UnaryOp::ToNumber, Type::Number),
            // `!` is `ToBoolean` inverted, so it always produces a boolean and never fails.
            UnaryOperator::LogicalNot => (UnaryOp::Not, Type::Bool),
            UnaryOperator::BitwiseNot => (UnaryOp::BitNot, Type::Number),
            // `typeof` produces one of a fixed set of strings, and is the only operator that
            // does not throw on an undeclared identifier.
            // **`typeof` is the one operator that does not throw on an undeclared name.** The
            // comment here said so long before the code did: the operand went through the
            // ordinary global load, which raises, so `typeof nothing` was a `ReferenceError`
            // instead of the string `"undefined"`.
            UnaryOperator::Typeof => {
                if let Expression::Identifier(identifier) = &unary.argument
                    && !self.resolves(identifier.name.as_str())
                {
                    let name = PropertyKey::new(identifier.name.as_str());
                    let value = self.emit(Type::Unknown, Op::GlobalLoadOptional { name });
                    return self.emit(
                        Type::String,
                        Op::Unary {
                            op: UnaryOp::TypeOf,
                            operand: value,
                        },
                    );
                }
                (UnaryOp::TypeOf, Type::String)
            }
            UnaryOperator::Void => (UnaryOp::Void, Type::Undefined),
            UnaryOperator::Delete => return self.delete(unary),
        };
        let operand = self.expression(&unary.argument);
        let result = self.emit(ty, Op::Unary { op, operand });
        // `+x` raises a `TypeError` on a BigInt — the one coercion the language forbids. A proven
        // Number cannot, so only the unproven case checks.
        if matches!(op, UnaryOp::ToNumber) && self.type_of(operand) != Type::Number {
            return self.propagate(result);
        }
        result
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

    /// Lowers `function* name(params) { body }` and returns the **outer** function — the one a call
    /// runs — plus the names it captures. The outer builds a generator object and returns it
    /// without running the body; a separate *body* function is the state machine `next` steps
    /// (D-246). Locals and parameters live on that object, so a loop counter survives a `yield`; a
    /// `yield` is handled in statement or simple-assignment position, and refused elsewhere (where a
    /// compiler temporary could be live across the suspension).
    fn lower_generator(
        &mut self,
        name: &str,
        params: &oxc_ast::ast::FormalParameters<'_>,
        body: Option<&oxc_ast::ast::FunctionBody<'_>>,
        is_async: bool,
    ) -> (FunctionId, Vec<String>) {
        let (body_id, body_captures) = self.lower_generator_body(name, params, body, is_async);

        let index = self.functions.len();
        let mut outer = Function::new(name);
        outer.id = FunctionId(u32::try_from(index).unwrap_or(u32::MAX));
        let entry = outer.entry;
        self.functions.push(outer);
        self.scopes.push(Scope {
            function: index,
            current: entry,
            terminated: false,
            slots: HashMap::new(),
            next_slot: 0,
            captures: Vec::new(),
            cells: std::collections::HashSet::new(),
            breaks: Vec::new(),
            handlers: Vec::new(),
            continues: Vec::new(),
            generator: None,
            value_types: Vec::new(),
        });
        let this_slot = self.declare("this");
        self.functions[index].this_slot = Some(this_slot);
        let mut parameter_slots = Vec::with_capacity(params.items.len());
        for param in &params.items {
            match param.pattern.get_identifier_name() {
                Some(param_name) => parameter_slots.push(self.declare(param_name.as_str())),
                None => {
                    self.note("destructuring parameter", param.span.start);
                    parameter_slots.push(self.temporary());
                }
            }
        }
        // Build the generator object over the body closure and the caller's `this`, store each
        // parameter on it under the key the body reads it by, and return it.
        let body_closure = self.close_over(body_id, &body_captures);
        let this_value = self.read(this_slot);
        let generator = self.emit(
            Type::Object(None),
            Op::MakeGenerator {
                body: body_closure,
                this_value,
            },
        );
        for (param, slot) in params.items.iter().zip(&parameter_slots) {
            if let Some(param_name) = param.pattern.get_identifier_name() {
                let value = self.read(*slot);
                self.emit_effect(Op::PropertyStore {
                    object: generator,
                    key: PropertyKey::new(&format!("$g_{param_name}")),
                    value,
                });
            }
        }
        self.functions[index].parameters = parameter_slots;
        // A generator function returns the generator object; an async function returns the promise
        // that `crisol_async_start` settles as it drives that generator to completion (D-249).
        let result = if is_async {
            self.emit(
                Type::Object(None),
                Op::Unary {
                    op: UnaryOp::AsyncStart,
                    operand: generator,
                },
            )
        } else {
            generator
        };
        self.terminate(Terminator::Return(Some(result)));

        let scope = self.scopes.pop().expect("just pushed");
        let names: Vec<String> = scope
            .captures
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let slots: Vec<u32> = scope.captures.iter().map(|(_, slot)| *slot).collect();
        self.functions[index].captures = slots;
        (
            FunctionId(u32::try_from(index).expect("functions fit in u32")),
            names,
        )
    }

    /// The state-machine body of a generator. Its `this` is the generator object; `yield` stores
    /// the value and the next state on it and returns a signal; the entry dispatches on the stored
    /// state to the block after the `yield` that suspended.
    fn lower_generator_body(
        &mut self,
        name: &str,
        params: &oxc_ast::ast::FormalParameters<'_>,
        body: Option<&oxc_ast::ast::FunctionBody<'_>>,
        is_async: bool,
    ) -> (FunctionId, Vec<String>) {
        let index = self.functions.len();
        let mut function = Function::new(&format!("{name}~body"));
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
            breaks: Vec::new(),
            handlers: Vec::new(),
            continues: Vec::new(),
            generator: Some(GenState {
                next_state: 1,
                resumes: Vec::new(),
                locals: HashMap::new(),
                is_async,
            }),
            value_types: Vec::new(),
        });
        // The body's `this` is the generator object — how it reaches its resume state and its
        // locals.
        let this_slot = self.declare("this");
        self.functions[index].this_slot = Some(this_slot);
        // Declare the parameters as generator locals (`declare` marks them, so reads redirect to
        // the generator object). The outer function stores the incoming argument values under the
        // matching keys, so the body sees them and they survive suspension.
        for param in &params.items {
            if let Some(param_name) = param.pattern.get_identifier_name() {
                self.declare(param_name.as_str());
            }
        }

        // Lower the body into a start block; the entry (block zero) becomes the dispatch below.
        let start = self.new_block();
        self.switch_to(start);
        if let Some(body) = body {
            for statement in &body.statements {
                self.statement(statement);
            }
        }
        // Falling off the end finishes with `undefined`.
        if !self.scope().terminated {
            let this = self.read(this_slot);
            let undefined = self.emit(Type::Undefined, Op::Const(Constant::Undefined));
            self.emit_effect(Op::PropertyStore {
                object: this,
                key: PropertyKey::new(GEN_RETURN_KEY),
                value: undefined,
            });
            let done = self.emit(Type::Number, Op::Const(Constant::Number(GEN_DONE_SIGNAL)));
            self.terminate(Terminator::Return(Some(done)));
        }

        // The entry dispatch: read the stored state and jump to the matching resume block. State 0
        // is the start; each `yield` recorded its own. Re-read `this`/state per comparison so no
        // value has to live across the chain's blocks.
        let resumes = self
            .scope()
            .generator
            .as_ref()
            .map(|generator| generator.resumes.clone())
            .unwrap_or_default();
        self.switch_to(entry);
        let mut targets = vec![(0u32, start)];
        targets.extend(resumes);
        for (state, block) in targets {
            let this = self.read(this_slot);
            let stored = self.emit(
                Type::Unknown,
                Op::PropertyLoad {
                    object: this,
                    key: PropertyKey::new(GEN_STATE_KEY),
                },
            );
            let wanted = self.emit(Type::Number, Op::Const(Constant::Number(f64::from(state))));
            let matches = self.emit(
                Type::Bool,
                Op::Compare {
                    op: CompareOp::StrictEqual,
                    left: stored,
                    right: wanted,
                },
            );
            let next = self.new_block();
            self.terminate(Terminator::Branch {
                condition: matches,
                then_block: block,
                then_args: Vec::new(),
                else_block: next,
                else_args: Vec::new(),
            });
            self.switch_to(next);
        }
        // An unreachable state (or an exhausted generator re-entered) simply finishes.
        let done = self.emit(Type::Number, Op::Const(Constant::Number(GEN_DONE_SIGNAL)));
        self.terminate(Terminator::Return(Some(done)));

        let scope = self.scopes.pop().expect("just pushed");
        let names: Vec<String> = scope
            .captures
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let slots: Vec<u32> = scope.captures.iter().map(|(_, slot)| *slot).collect();
        self.functions[index].captures = slots;
        (
            FunctionId(u32::try_from(index).expect("functions fit in u32")),
            names,
        )
    }

    /// The mechanics of one `yield`: store the value and the next resume state on the generator,
    /// return the yield signal, then continue in a fresh resume block whose value is what `next`
    /// sent in. The caller must be in statement or simple-assignment position, so no compiler
    /// temporary is live across the suspension (the resume block is entered from the entry
    /// dispatch, not from before the `yield`).
    fn lower_yield(&mut self, argument: Option<&Expression<'_>>) -> ValueId {
        let value = match argument {
            Some(argument) => self.expression(argument),
            None => self.emit(Type::Undefined, Op::Const(Constant::Undefined)),
        };
        let this_slot = self.slot("this");
        let this = self.read(this_slot);
        self.emit_effect(Op::PropertyStore {
            object: this,
            key: PropertyKey::new(GEN_YIELDED_KEY),
            value,
        });
        let state = {
            let generator = self
                .scope_mut()
                .generator
                .as_mut()
                .expect("a yield outside a generator is refused by the caller");
            let state = generator.next_state;
            generator.next_state += 1;
            state
        };
        let this = self.read(this_slot);
        let state_const = self.emit(Type::Number, Op::Const(Constant::Number(f64::from(state))));
        self.emit_effect(Op::PropertyStore {
            object: this,
            key: PropertyKey::new(GEN_STATE_KEY),
            value: state_const,
        });
        let signal = self.emit(Type::Number, Op::Const(Constant::Number(GEN_YIELD_SIGNAL)));
        self.terminate(Terminator::Return(Some(signal)));

        let resume = self.new_block();
        self.scope_mut()
            .generator
            .as_mut()
            .expect("checked above")
            .resumes
            .push((state, resume));
        self.switch_to(resume);
        let this = self.read(this_slot);
        self.emit(
            Type::Unknown,
            Op::PropertyLoad {
                object: this,
                key: PropertyKey::new(GEN_SENT_KEY),
            },
        )
    }

    /// Whether an expression is a `yield` that [`Self::lower_yield`] can handle here (not a
    /// delegating `yield*`, and inside a generator).
    fn is_plain_yield(&self, expression: &Expression<'_>) -> bool {
        matches!(expression, Expression::YieldExpression(yield_expression)
            if !yield_expression.delegate)
            && self.scope().generator.is_some()
    }

    /// Whether an expression is an `await` this position can suspend on (D-249) — inside an async
    /// body, where `await` reuses the `yield` machinery. Like `yield`, only the simple positions
    /// below take it; a complex one keeps no compiler temporary live across the suspension only
    /// because it is refused.
    fn is_plain_await(&self, expression: &Expression<'_>) -> bool {
        matches!(expression, Expression::AwaitExpression(_))
            && self
                .scope()
                .generator
                .as_ref()
                .is_some_and(|generator| generator.is_async)
    }

    /// A value expression that may itself be a `yield` or an `await` — the right side of an
    /// initialiser or a simple assignment, the positions where the sent/awaited value is bound
    /// with nothing else live across the suspension.
    fn value_expression(&mut self, expression: &Expression<'_>) -> ValueId {
        if self.is_plain_yield(expression) {
            let Expression::YieldExpression(yield_expression) = expression else {
                unreachable!("is_plain_yield checked the shape")
            };
            return self.lower_yield(yield_expression.argument.as_ref());
        }
        if self.is_plain_await(expression) {
            let Expression::AwaitExpression(await_expression) = expression else {
                unreachable!("is_plain_await checked the shape")
            };
            // `await e` suspends exactly as `yield e` does; the driver resumes with the settled
            // value, which becomes the expression's result.
            return self.lower_yield(Some(&await_expression.argument));
        }
        self.expression(expression)
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
            breaks: Vec::new(),
            handlers: Vec::new(),
            continues: Vec::new(),
            generator: None,
            value_types: Vec::new(),
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

            // `arguments` is **not** declared here. It is bound lazily, the first time a body
            // names it (see `slot`), so a function that never mentions it has exactly the slot
            // numbering it had before `arguments` existed. Declaring it eagerly shifted every
            // parameter down by one in every function, which broke closures in a way that only
            // showed under GC stress.
            self.binds_arguments.push(index);
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

        if binds_this {
            // Paired with the push above. Without this an arrow lowered after a nested function
            // would look up `arguments` in a function that had already finished.
            self.binds_arguments.pop();
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
        // `extends`: evaluate the parent once and expose it to the methods as the grammar-illegal
        // names ` super` (the parent constructor) and ` superproto` (its prototype), which a
        // method or the constructor captures exactly when it writes `super`.
        let parent = class
            .heritage
            .as_ref()
            .map(|heritage| self.expression(&heritage.expression));
        let mut parent_prototype = None;
        if let Some(parent) = parent {
            let proto = self.emit(
                Type::Unknown,
                Op::PropertyLoad {
                    object: parent,
                    key: PropertyKey::new("prototype"),
                },
            );
            let super_slot = self.declare(" super");
            self.bind(" super", super_slot, parent);
            let proto_slot = self.declare(" superproto");
            self.bind(" superproto", proto_slot, proto);
            parent_prototype = Some(proto);
        }

        let shape = crisol_value::Shapes::new().root();
        let prototype = self.emit(Type::Object(None), Op::CreateObject { shape });
        // An instance inherits the parent's methods through the prototype chain:
        // `B.prototype.[[Prototype]] = A.prototype`.
        if let Some(parent_prototype) = parent_prototype {
            self.emit_effect(Op::SetPrototype {
                object: prototype,
                prototype: parent_prototype,
            });
        }
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
            // A class body's accessors are accessors, exactly as a literal's are — and a
            // class method is **not** enumerable, which `crisol_define_accessor` does not
            // arrange, so that difference is recorded rather than quietly wrong (D-213).
            match method.kind {
                oxc_ast::ast::MethodDefinitionKind::Get => {
                    let absent = self.placeholder();
                    self.emit_effect(Op::DefineAccessor {
                        object: prototype,
                        key: PropertyKey::new(&method_name),
                        getter: closure,
                        setter: absent,
                    });
                }
                oxc_ast::ast::MethodDefinitionKind::Set => {
                    let absent = self.placeholder();
                    self.emit_effect(Op::DefineAccessor {
                        object: prototype,
                        key: PropertyKey::new(&method_name),
                        getter: absent,
                        setter: closure,
                    });
                }
                _ => {
                    self.emit_effect(Op::PropertyStore {
                        object: prototype,
                        key: PropertyKey::new(&method_name),
                        value: closure,
                    });
                }
            }
        }

        let constructor = match constructor {
            Some(closure) => closure,
            None => {
                // No explicit constructor: the class still needs one, because `new` has to call
                // something. A base class's does nothing; a derived class's calls `super()` so the
                // parent still runs.
                let (id, captures) = if parent.is_some() {
                    self.implicit_derived_constructor(name)
                } else {
                    self.implicit_constructor(name)
                };
                self.close_over(id, &captures)
            }
        };
        self.emit_effect(Op::PropertyStore {
            object: constructor,
            key: PropertyKey::new("prototype"),
            value: prototype,
        });
        // A static call reaches the parent's statics through the constructor's own chain:
        // `B.[[Prototype]] = A`.
        if let Some(parent) = parent {
            self.emit_effect(Op::SetPrototype {
                object: constructor,
                prototype: parent,
            });
        }
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
            breaks: Vec::new(),
            handlers: Vec::new(),
            continues: Vec::new(),
            generator: None,
            value_types: Vec::new(),
        });
        self.declare("this");
        self.terminate(Terminator::Return(None));
        self.scopes.pop();
        (
            FunctionId(u32::try_from(index).expect("functions fit in u32")),
            Vec::new(),
        )
    }

    /// The constructor a *derived* class without an explicit one still gets: `constructor(...) {
    /// super(...); }`. It forwards no arguments — this engine has no rest/spread to forward them
    /// with — so `new B()` runs the parent's constructor but `new B(x)` does not pass `x` on
    /// (D-243). It captures ` super` the same way any method that writes `super` does.
    fn implicit_derived_constructor(&mut self, name: &str) -> (FunctionId, Vec<String>) {
        let index = self.functions.len();
        let mut function = Function::new(&format!("{name}.constructor"));
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
            breaks: Vec::new(),
            handlers: Vec::new(),
            continues: Vec::new(),
            generator: None,
            value_types: Vec::new(),
        });
        // **Recorded on the function**, exactly as `lower_function` does, so the backend binds the
        // incoming receiver to this slot — without it `this` reads `undefined` and `super()`
        // initialises nothing.
        let this_slot = self.declare("this");
        self.functions[index].this_slot = Some(this_slot);
        // `super()`: call the captured parent constructor with the `this` being built.
        let super_slot = self.slot(" super");
        let super_ctor = self.read(super_slot);
        let this_value = self.read(this_slot);
        let call = self.emit(
            Type::Unknown,
            Op::Call {
                callee: super_ctor,
                this_value,
                args: Vec::new(),
            },
        );
        self.propagate(call);
        self.terminate(Terminator::Return(None));
        let scope = self.scopes.pop().expect("just pushed");
        let names: Vec<String> = scope
            .captures
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let slots: Vec<u32> = scope.captures.iter().map(|(_, slot)| *slot).collect();
        self.functions[index].captures = slots;
        (
            FunctionId(u32::try_from(index).expect("functions fit in u32")),
            names,
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
                            // **A getter is called on read and a data property is not**, so
                            // storing the function would be a wrong answer rather than a
                            // missing feature — `({get x() { return 1; }}).x` was the
                            // function, and nothing said so (D-213).
                            match property.kind {
                                oxc_ast::ast::PropertyKind::Init => {
                                    self.emit_effect(Op::PropertyStore {
                                        object: result,
                                        key: PropertyKey::new(&name),
                                        value,
                                    });
                                }
                                oxc_ast::ast::PropertyKind::Get => {
                                    let absent = self.placeholder();
                                    self.emit_effect(Op::DefineAccessor {
                                        object: result,
                                        key: PropertyKey::new(&name),
                                        getter: value,
                                        setter: absent,
                                    });
                                }
                                oxc_ast::ast::PropertyKind::Set => {
                                    let absent = self.placeholder();
                                    self.emit_effect(Op::DefineAccessor {
                                        object: result,
                                        key: PropertyKey::new(&name),
                                        getter: absent,
                                        setter: value,
                                    });
                                }
                            }
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
                            if property.kind != oxc_ast::ast::PropertyKind::Init {
                                // A computed accessor name needs a key the accessor operation
                                // cannot take — it carries a `PropertyKey`, not a value.
                                // Refused rather than stored as data, which is the wrong
                                // answer this whole arm exists to stop.
                                self.note("computed accessor name", property.span.start);
                                continue;
                            }
                            self.emit_effect(Op::ComputedStore {
                                object: result,
                                key,
                                value,
                            });
                        }
                    }
                }
                ObjectPropertyKind::SpreadProperty(spread) => {
                    // `{ ...src }`: copy src's own enumerable properties onto the literal. A getter
                    // on src can throw, so the copy's result is propagated.
                    let source = self.expression(&spread.argument);
                    let extended = self.emit(
                        Type::Undefined,
                        Op::ObjectExtend {
                            object: result,
                            source,
                        },
                    );
                    self.propagate(extended);
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

/// Every name a `var` declares in `statements`, including inside nested blocks and loops.
///
/// **Through blocks but not through functions.** A `var` is scoped to the nearest enclosing
/// *function*, so one inside an `if` belongs to the function around it — and one inside a nested
/// function belongs to that function, not this one. Walking into a function body would hoist its
/// locals into the wrong scope, which is worse than not hoisting at all.
fn collect_var_names(statements: &[Statement<'_>], into: &mut Vec<String>) {
    for statement in statements {
        collect_var_names_of(statement, into);
    }
}

/// One statement's `var` names.
fn collect_var_names_of(statement: &Statement<'_>, into: &mut Vec<String>) {
    use oxc_ast::ast::ForStatementInit;

    match statement {
        Statement::VariableDeclaration(declaration) if declaration.kind.is_var() => {
            for declarator in &declaration.declarations {
                if let Some(name) = declarator.id.get_identifier_name() {
                    into.push(name.to_string());
                }
            }
        }
        Statement::BlockStatement(block) => collect_var_names(&block.body, into),
        Statement::IfStatement(statement) => {
            collect_var_names_of(&statement.consequent, into);
            if let Some(alternate) = &statement.alternate {
                collect_var_names_of(alternate, into);
            }
        }
        Statement::ForStatement(statement) => {
            if let Some(ForStatementInit::VariableDeclaration(declaration)) = &statement.init
                && declaration.kind.is_var()
            {
                for declarator in &declaration.declarations {
                    if let Some(name) = declarator.id.get_identifier_name() {
                        into.push(name.to_string());
                    }
                }
            }
            collect_var_names_of(&statement.body, into);
        }
        Statement::ForInStatement(statement) => collect_var_names_of(&statement.body, into),
        Statement::ForOfStatement(statement) => collect_var_names_of(&statement.body, into),
        Statement::WhileStatement(statement) => collect_var_names_of(&statement.body, into),
        Statement::DoWhileStatement(statement) => collect_var_names_of(&statement.body, into),
        Statement::LabeledStatement(statement) => collect_var_names_of(&statement.body, into),
        Statement::TryStatement(statement) => {
            collect_var_names(&statement.block.body, into);
            if let Some(handler) = &statement.handler {
                collect_var_names(&handler.body.body, into);
            }
            if let Some(finalizer) = &statement.finalizer {
                collect_var_names(&finalizer.body, into);
            }
        }
        Statement::SwitchStatement(statement) => {
            for case in &statement.cases {
                collect_var_names(&case.consequent, into);
            }
        }
        // A function's own `var`s belong to it, so the walk stops here.
        _ => {}
    }
}
