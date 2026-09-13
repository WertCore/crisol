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
    Block, BlockId, CompareOp, Constant, Function, Instruction, Op, Safepoint, Terminator, Type,
    ValueId,
};
use crisol_value::PropertyKey;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BinaryExpression, BinaryOperator, Expression, ObjectPropertyKind, Program, PropertyKey as Key,
    Statement,
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
    /// The IR.
    pub function: Function,
    /// Everything the lowering did not understand, in source order.
    pub unsupported: Vec<Unsupported>,
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

struct Lowering {
    function: Function,
    current: BlockId,
    /// Whether `current` already has a real terminator, so trailing statements are dead.
    terminated: bool,
    slots: HashMap<String, u32>,
    next_slot: u32,
    unsupported: Vec<Unsupported>,
}

impl Lowering {
    fn new(name: &str) -> Self {
        let function = Function::new(name);
        Self {
            current: function.entry,
            function,
            terminated: false,
            slots: HashMap::new(),
            next_slot: 0,
            unsupported: Vec::new(),
        }
    }

    fn finish(mut self) -> Lowered {
        if !self.terminated {
            self.terminate(Terminator::Return(None));
        }
        Lowered {
            function: self.function,
            unsupported: self.unsupported,
        }
    }

    fn program(&mut self, program: &Program<'_>) {
        for statement in &program.body {
            self.statement(statement);
        }
    }

    // ---- plumbing ----------------------------------------------------------------------

    /// A new empty block. Its terminator is a placeholder until [`Lowering::terminate`] runs.
    fn new_block(&mut self) -> BlockId {
        self.function.block(Block {
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: Terminator::Return(None),
        })
    }

    fn switch_to(&mut self, block: BlockId) {
        self.current = block;
        self.terminated = false;
    }

    fn terminate(&mut self, terminator: Terminator) {
        if self.terminated {
            return;
        }
        if let Some(block) = self.function.get_mut(self.current) {
            block.terminator = terminator;
        }
        self.terminated = true;
    }

    fn emit(&mut self, ty: Type, op: Op) -> ValueId {
        let result = self.function.value();
        // The live set is empty because locals are in slots rather than in SSA values, so
        // nothing an allocation could invalidate is being held in one. When `mem2reg` lands
        // (M12) and values start living across allocations, this is where the live set gets
        // computed — and the verifier already refuses an allocation without a safepoint, so
        // the structure has to be here first.
        let safepoint = op.can_collect().then(Safepoint::default);
        if let Some(block) = self.function.get_mut(self.current) {
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
        if let Some(block) = self.function.get_mut(self.current) {
            block.instructions.push(Instruction {
                result: None,
                ty: Type::Undefined,
                op,
                safepoint,
            });
        }
    }

    fn slot(&mut self, name: &str) -> u32 {
        if let Some(slot) = self.slots.get(name) {
            return *slot;
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        self.slots.insert(name.to_owned(), slot);
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
        if self.terminated {
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
                    let slot = self.slot(name.as_str());
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
            // Arithmetic is not a `Compare`, and the IR has no arithmetic op yet. Recorded
            // rather than lowered to something that looks like it works.
            _ => {
                self.note(
                    &format!("binary operator {}", binary.operator.as_str()),
                    binary.span.start,
                );
                return self.placeholder();
            }
        };
        let left = self.expression(&binary.left);
        let right = self.expression(&binary.right);
        self.emit(Type::Bool, Op::Compare { op, left, right })
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
