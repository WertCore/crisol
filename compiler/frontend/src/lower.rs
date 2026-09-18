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
    if !parsed.errors.is_empty() {
        return Err(ParseFailed {
            errors: parsed.errors.iter().map(ToString::to_string).collect(),
        });
    }
    let mut lowering = Lowering::new(name);
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
}

struct Lowering {
    functions: Vec<Function>,
    /// Innermost last. A nested function pushes; finishing it pops.
    scopes: Vec<Scope>,
    unsupported: Vec<Unsupported>,
}

impl Lowering {
    fn new(name: &str) -> Self {
        let function = Function::new(name);
        let entry = function.entry;
        Self {
            functions: vec![function],
            scopes: vec![Scope {
                function: 0,
                current: entry,
                terminated: false,
                slots: HashMap::new(),
                next_slot: 0,
                captures: Vec::new(),
            }],
            unsupported: Vec::new(),
        }
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
        for statement in &program.body {
            self.statement(statement);
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
                for declarator in &declaration.declarations {
                    let Some(name) = declarator.id.get_identifier_name() else {
                        self.note("destructuring declaration", declarator.span.start);
                        continue;
                    };
                    let value = match &declarator.init {
                        Some(init) => self.expression(init),
                        None => self.emit(Type::Undefined, Op::Const(Constant::Undefined)),
                    };
                    // `declare`, not `slot`: a `let` shadows an outer binding rather than
                    // capturing it.
                    let slot = self.declare(name.as_str());
                    self.emit_effect(Op::Store { slot, value });
                }
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
                self.statement(&statement.body);
                self.terminate(Terminator::Jump {
                    target: header,
                    args: Vec::new(),
                });

                self.switch_to(exit);
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
                self.terminate(Terminator::Throw(value));
            }
            Statement::FunctionDeclaration(declaration) => {
                let name = declaration
                    .id
                    .as_ref()
                    .map_or_else(|| "anonymous".to_owned(), |id| id.name.to_string());
                let (id, names) = self.lower_function(
                    &name,
                    &declaration.params,
                    declaration.body.as_deref(),
                    None,
                );
                let closure = self.close_over(id, &names);
                // A declaration binds its name in the enclosing scope. Hoisting is not modelled
                // — the binding appears where the declaration does, so a call before it reads
                // an unset slot rather than working. Recorded rather than silently half-right.
                self.note("function declaration hoisting", declaration.span.start);
                let slot = self.declare(&name);
                self.emit_effect(Op::Store {
                    slot,
                    value: closure,
                });
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
                let slot = self.slot(identifier.name.as_str());
                self.emit(Type::Unknown, Op::Load { slot })
            }
            Expression::BinaryExpression(binary) => self.binary(binary),
            Expression::AssignmentExpression(assignment) => {
                let value = self.expression(&assignment.right);
                match assignment.left.get_identifier_name() {
                    Some(name) => {
                        let slot = self.slot(name);
                        self.emit_effect(Op::Store { slot, value });
                    }
                    None => self.note("assignment target", assignment.span.start),
                }
                value
            }
            Expression::StaticMemberExpression(member) => {
                let object = self.expression(&member.object);
                self.emit(
                    Type::Unknown,
                    Op::PropertyLoad {
                        object,
                        key: PropertyKey::new(member.property.name.as_str()),
                    },
                )
            }
            Expression::CallExpression(call) => {
                let callee = self.expression(&call.callee);
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
                self.emit(Type::Unknown, Op::Call { callee, args })
            }
            Expression::ObjectExpression(object) => self.object(object),
            Expression::FunctionExpression(function) => {
                let name = function
                    .id
                    .as_ref()
                    .map_or_else(|| "anonymous".to_owned(), |id| id.name.to_string());
                let (id, names) =
                    self.lower_function(&name, &function.params, function.body.as_deref(), None);
                self.close_over(id, &names)
            }
            Expression::ArrowFunctionExpression(arrow) => {
                // A concise body — `x => x + 1` — is parsed as a body holding one expression
                // statement, which the parser flags rather than restructuring.
                let concise = arrow.expression.then(|| {
                    arrow
                        .body
                        .statements
                        .first()
                        .and_then(|statement| match statement {
                            Statement::ExpressionStatement(statement) => {
                                Some(&statement.expression)
                            }
                            _ => None,
                        })
                });
                match concise.flatten() {
                    Some(expression) => {
                        let (id, names) =
                            self.lower_function("arrow", &arrow.params, None, Some(expression));
                        self.close_over(id, &names)
                    }
                    None => {
                        let (id, names) =
                            self.lower_function("arrow", &arrow.params, Some(&arrow.body), None);
                        self.close_over(id, &names)
                    }
                }
            }
            Expression::UnaryExpression(unary) => self.unary(unary),
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
            // `==`, `!=`, `in`, `instanceof` — each needs machinery this does not have yet.
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
        } else {
            Type::Unknown
        };
        self.emit(ty, Op::Binary { op, left, right })
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
            UnaryOperator::Delete => {
                self.note("delete operator", unary.span.start);
                return self.placeholder();
            }
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
    fn lower_function(
        &mut self,
        name: &str,
        params: &oxc_ast::ast::FormalParameters<'_>,
        body: Option<&oxc_ast::ast::FunctionBody<'_>>,
        expression_body: Option<&Expression<'_>>,
    ) -> (FunctionId, Vec<String>) {
        let index = self.functions.len();
        let function = Function::new(name);
        let entry = function.entry;
        self.functions.push(function);
        self.scopes.push(Scope {
            function: index,
            current: entry,
            terminated: false,
            slots: HashMap::new(),
            next_slot: 0,
            captures: Vec::new(),
        });

        let mut parameter_slots = Vec::with_capacity(params.items.len());
        for param in &params.items {
            match param.pattern.get_identifier_name() {
                // `declare`, not `slot`: a parameter shadows an outer binding of the same name.
                Some(param_name) => parameter_slots.push(self.declare(param_name.as_str())),
                None => {
                    self.note("destructuring parameter", param.span.start);
                    // Still consumes a position, or every later parameter would shift down one
                    // and silently receive the wrong argument.
                    let placeholder = self.temporary();
                    parameter_slots.push(placeholder);
                }
            }
        }

        if let Some(body) = body {
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

    /// A slot no source name can collide with.
    ///
    /// Named with a character the grammar does not allow in an identifier, so a program cannot
    /// declare a variable that shadows a compiler temporary.
    fn temporary(&mut self) -> u32 {
        let name = format!(" tmp{}", self.scope().next_slot);
        self.declare(&name)
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
                        Key::StaticIdentifier(identifier) => identifier.name.to_string(),
                        Key::StringLiteral(literal) => literal.value.to_string(),
                        _ => {
                            self.note("computed property key", property.span.start);
                            continue;
                        }
                    };
                    let value = self.expression(&property.value);
                    self.emit_effect(Op::PropertyStore {
                        object: result,
                        key: PropertyKey::new(&name),
                        value,
                    });
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
        Statement::ForStatement(_) => "for statement",
        Statement::ForInStatement(_) => "for-in statement",
        Statement::ForOfStatement(_) => "for-of statement",
        Statement::FunctionDeclaration(_) => "function declaration",
        Statement::ClassDeclaration(_) => "class declaration",
        Statement::TryStatement(_) => "try statement",
        Statement::SwitchStatement(_) => "switch statement",
        Statement::BreakStatement(_) => "break statement",
        Statement::ContinueStatement(_) => "continue statement",
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
        Expression::ComputedMemberExpression(_) => "computed member access",
        Expression::UnaryExpression(_) => "unary expression",
        Expression::LogicalExpression(_) => "logical expression",
        Expression::ConditionalExpression(_) => "conditional expression",
        Expression::TemplateLiteral(_) => "template literal",
        Expression::AwaitExpression(_) => "await expression",
        Expression::NewExpression(_) => "new expression",
        _ => "expression",
    }
}
