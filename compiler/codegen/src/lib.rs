//! Backend trait, and the Cranelift implementation.
//!
//! ROADMAP §M13. §4 names Cranelift, and the deciding feature is **stack maps**: §M11 made
//! safepoints explicit in the IR precisely so that codegen could tell the collector which
//! machine locations hold live references at each one. A backend that could not emit stack maps
//! would make the whole safepoint apparatus decorative.
//!
//! # Values are one 64-bit word
//!
//! A JavaScript value is NaN-boxed into 64 bits (D-53), so Cranelift sees `I64` everywhere and
//! a number is reached by bitcasting. There is no separate float register class in the calling
//! convention and no boxing at a call boundary.
//!
//! # `+` needs a helper and the other operators do not
//!
//! This is D-79 arriving in the machine code. Every arithmetic operator except `+` coerces with
//! `ToNumber` and produces a number, so the IR types it `Number` and this lowers it to a native
//! `f64` instruction. `+` may concatenate, so the IR types it `Unknown` — and **an `Unknown`
//! cannot be lowered to a float add**, because the operands might be strings.
//!
//! So `Add` becomes a call to a runtime helper. That is not a shortcoming to be embarrassed
//! about: it is what every engine does before type feedback narrows the operands, and the
//! alternative — emitting a float add anyway — is a miscompilation. Narrowing it is M20's job,
//! and the IR already carries the type that would let a pass do so.

#![doc(html_root_url = "https://docs.rs/crisol-codegen/0.0.0")]

use std::collections::HashMap;

use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Value as ClifValue, types};
use cranelift_codegen::settings::{self, Configurable as _};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{Linkage, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};
use crisol_ir::{BinaryOp, CompareOp, Constant, Function, Op, Terminator};

/// Why a function could not be compiled.
///
/// Every variant is a **refusal**, never a silent approximation. A backend that guessed at an
/// operation it did not implement would emit machine code that runs and is wrong, which cannot
/// be distinguished from correct code by testing the compiler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodegenError {
    /// An IR operation this backend does not lower yet.
    Unsupported {
        /// What it was.
        operation: String,
    },
    /// The target triple is not one this build can emit for.
    UnknownTarget {
        /// What was asked for.
        triple: String,
    },
    /// Cranelift rejected something, which is a bug here rather than in the input.
    Backend {
        /// What it said.
        message: String,
    },
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported { operation } => {
                write!(f, "cannot compile {operation} yet")
            }
            Self::UnknownTarget { triple } => write!(f, "no backend for target {triple}"),
            Self::Backend { message } => write!(f, "backend error: {message}"),
        }
    }
}

impl std::error::Error for CodegenError {}

/// What compiling one function produced.
///
/// `stack_map_entries` exists because §M13's deliverable is *"stack map emission at
/// safepoints"*, and a test that only checks the function compiled does not verify emission at
/// all. Counting the entries makes the deliverable measurable rather than asserted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// How many values were declared live across a safepoint.
    ///
    /// **This counts what was handed to Cranelift, not what reached the object file.**
    /// `cranelift-object` does not write stack maps into a section — they live in the compiled
    /// code's metadata — so M13's GC integration will have to carry them out of band. Saying
    /// "stack maps are emitted" without that distinction would be the kind of claim that looks
    /// true until someone goes looking for the section.
    pub stack_map_entries: usize,
}

/// What a code generator has to be able to do.
///
/// A trait rather than a concrete type because §M13 lists four targets and §M22 adds mobile
/// ones, and because M14's differential testing needs a second implementation to compare
/// against — a design with one hard-coded backend cannot be differentially tested at all.
pub trait Backend {
    /// Compiles one function, appending it to the module being built.
    ///
    /// # Errors
    ///
    /// [`CodegenError`] naming what it refused, rather than emitting an approximation.
    fn compile(&mut self, function: &Function) -> Result<Report, CodegenError>;

    /// Finishes the module and returns an object file.
    ///
    /// # Errors
    ///
    /// [`CodegenError::Backend`] if the module cannot be written.
    fn finish(self) -> Result<Vec<u8>, CodegenError>;
}

/// Which runtime symbol each operator calls.
///
/// The names are the contract with `crisol-abi`, and **nothing connects the two sides until
/// link time** — a typo on either is silent through every compiler test. `crisol-abi::SYMBOLS`
/// is the defining list and a test here checks this against it.
const HELPER_SYMBOLS: &[(BinaryOp, &str)] = &[
    (BinaryOp::Add, "crisol_add"),
    (BinaryOp::Remainder, "crisol_remainder"),
    (BinaryOp::Exponent, "crisol_exponent"),
    (BinaryOp::BitAnd, "crisol_bit_and"),
    (BinaryOp::BitOr, "crisol_bit_or"),
    (BinaryOp::BitXor, "crisol_bit_xor"),
    (BinaryOp::ShiftLeft, "crisol_shift_left"),
    (BinaryOp::ShiftRight, "crisol_shift_right"),
    (BinaryOp::UnsignedShiftRight, "crisol_unsigned_shift_right"),
];

/// The runtime symbols the backend emits calls to.
///
/// Exposed so a test can check them against what `crisol-abi` defines. The two sides are
/// connected **only by name**, so a typo on either is silent through every compiler test and
/// fails when someone tries to link a binary.
#[must_use]
pub fn helper_symbols() -> Vec<&'static str> {
    HELPER_SYMBOLS.iter().map(|(_, symbol)| *symbol).collect()
}

/// The Cranelift backend.
///
/// `Debug` reports what it is targeting and nothing else: an `ObjectModule` holds every
/// function emitted so far, and printing that would bury a test failure under a megabyte of
/// machine code.
pub struct Cranelift {
    triple: String,
    module: ObjectModule,
    context: FunctionBuilderContext,
    /// The runtime helpers, one per operator that cannot be a native instruction.
    ///
    /// One symbol each rather than a single `crisol_binary(op, a, b)`: an opcode passed at
    /// runtime would be a branch the linker cannot see through, and a separate symbol is what
    /// lets a later pass replace an individual operator without touching the others.
    helpers: HashMap<BinaryOp, cranelift_module::FuncId>,
}

impl std::fmt::Debug for Cranelift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cranelift")
            .field("triple", &self.triple)
            .finish_non_exhaustive()
    }
}

impl Cranelift {
    /// A backend emitting for `triple`.
    ///
    /// # Errors
    ///
    /// [`CodegenError::UnknownTarget`] when the triple is not one Cranelift can target here.
    pub fn new(triple: &str) -> Result<Self, CodegenError> {
        let parsed: target_lexicon::Triple =
            triple.parse().map_err(|_| CodegenError::UnknownTarget {
                triple: triple.to_owned(),
            })?;

        // Stack maps are **not** a global flag in this version of Cranelift — they are
        // requested per value, via `declare_value_needs_stack_map`. That turns out to be the
        // better fit: §M11 made safepoints carry an explicit live set, and this API wants
        // exactly that set rather than a whole-function switch. Asking for a flag named
        // `enable_safepoints` fails, which is how this was found.
        // Position-independent code, because the object has to *call* the runtime helpers.
        // Without this the linker refuses with "illegal text-relocations" on macOS and
        // silently produces a non-PIE elsewhere — the first program that used `+` failed to
        // link, while one using only `-` succeeded, because only the former emits a call.
        let mut flags = settings::builder();
        flags
            .set("is_pic", "true")
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        let isa = cranelift_codegen::isa::lookup(parsed.clone())
            .map_err(|_| CodegenError::UnknownTarget {
                triple: triple.to_owned(),
            })?
            .finish(settings::Flags::new(flags))
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;

        let builder = ObjectBuilder::new(isa, "crisol", cranelift_module::default_libcall_names())
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        let mut module = ObjectModule::new(builder);

        // Every operator that is not a native instruction is a call, declared as an import so
        // the runtime supplies it at link time.
        //
        // `+` because it may concatenate (D-79). `%` and `**` because they are libm calls.
        // The bitwise operators because `ToInt32` wraps **modulo 2^32** — Cranelift's
        // float-to-int conversion saturates, so `1e10 | 0` would come out clamped rather than
        // wrapped, which is a wrong number rather than a slow one.
        let mut signature = module.make_signature();
        signature.params.push(AbiParam::new(types::I64));
        signature.params.push(AbiParam::new(types::I64));
        signature.returns.push(AbiParam::new(types::I64));
        let mut helpers = HashMap::new();
        for (op, symbol) in HELPER_SYMBOLS {
            let id = module
                .declare_function(symbol, Linkage::Import, &signature)
                .map_err(|error| CodegenError::Backend {
                    message: error.to_string(),
                })?;
            helpers.insert(*op, id);
        }

        Ok(Self {
            triple: triple.to_owned(),
            module,
            context: FunctionBuilderContext::new(),
            helpers,
        })
    }
}

impl Backend for Cranelift {
    fn compile(&mut self, function: &Function) -> Result<Report, CodegenError> {
        let mut signature = self.module.make_signature();
        for _ in &function.parameters {
            signature.params.push(AbiParam::new(types::I64));
        }
        for _ in &function.captures {
            signature.params.push(AbiParam::new(types::I64));
        }
        signature.returns.push(AbiParam::new(types::I64));

        let id = self
            .module
            .declare_function(&function.name, Linkage::Export, &signature)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;

        let mut context = cranelift_codegen::Context::new();
        context.func.signature = signature;
        let frontend_config = self.module.target_config();
        let helpers: HashMap<BinaryOp, cranelift_codegen::ir::FuncRef> = self
            .helpers
            .iter()
            .map(|(op, id)| {
                (
                    *op,
                    self.module.declare_func_in_func(*id, &mut context.func),
                )
            })
            .collect();
        let builder = FunctionBuilder::new(&mut context.func, &mut self.context);
        let mut lowering = Lowering {
            builder,
            slots: HashMap::new(),
            values: HashMap::new(),
            types: HashMap::new(),
            blocks: HashMap::new(),
            helpers,
        };
        lowering.lower(function)?;
        // `finalize` needs the target's frontend config in this version — it is what decides
        // pointer width for the safepoint spill slots it inserts.
        lowering.builder.finalize(frontend_config);

        // Counted before `define_function` consumes the context. Read from the DFG, which is
        // where `declare_value_needs_stack_map` actually records them.
        let stack_map_entries: usize = context
            .func
            .layout
            .blocks()
            .flat_map(|block| context.func.layout.block_insts(block))
            .filter_map(|inst| context.func.dfg.user_stack_map_entries(inst))
            .map(<[_]>::len)
            .sum();

        self.module
            .define_function(id, &mut context)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        Ok(Report { stack_map_entries })
    }

    fn finish(self) -> Result<Vec<u8>, CodegenError> {
        self.module
            .finish()
            .emit()
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })
    }
}

/// One function's worth of lowering state.
struct Lowering<'a> {
    builder: FunctionBuilder<'a>,
    /// IR slot to Cranelift variable.
    slots: HashMap<u32, Variable>,
    /// IR value to Cranelift value.
    values: HashMap<u32, ClifValue>,
    /// IR value to the type the IR gave it.
    ///
    /// Carried because several lowerings are only correct for a *known* type. `===` is the
    /// clearest case: on two numbers it is `f64` equality, and on boxed values of unknown type
    /// it is not. Without this the backend would have to refuse both.
    types: HashMap<u32, crisol_ir::Type>,
    /// IR block to Cranelift block.
    blocks: HashMap<u32, cranelift_codegen::ir::Block>,
    /// The declared helpers, resolved into this function.
    helpers: HashMap<BinaryOp, cranelift_codegen::ir::FuncRef>,
}

impl Lowering<'_> {
    fn lower(&mut self, function: &Function) -> Result<(), CodegenError> {
        // Every IR block gets a Cranelift block up front, because a jump may target a block
        // that has not been lowered yet.
        for index in 0..function.blocks.len() {
            let block = self.builder.create_block();
            self.blocks
                .insert(u32::try_from(index).unwrap_or(u32::MAX), block);
        }

        let entry = self.block(function.entry.index());
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);

        // Parameters then captures, matching the signature and `Function`'s own ordering.
        let incoming: Vec<ClifValue> = self.builder.block_params(entry).to_vec();
        let arriving: Vec<u32> = function
            .parameters
            .iter()
            .chain(function.captures.iter())
            .copied()
            .collect();
        for (slot, value) in arriving.iter().zip(incoming) {
            let variable = self.variable(*slot);
            self.builder.def_var(variable, value);
        }

        for (index, block) in function.blocks.iter().enumerate() {
            let clif = self.block(u32::try_from(index).unwrap_or(u32::MAX));
            self.builder.switch_to_block(clif);
            for instruction in &block.instructions {
                self.instruction(instruction)?;
            }
            self.terminator(&block.terminator)?;
        }

        self.builder.seal_all_blocks();
        Ok(())
    }

    fn block(&self, index: u32) -> cranelift_codegen::ir::Block {
        *self.blocks.get(&index).expect("every block was created")
    }

    /// The variable backing an IR slot, declared on first use.
    ///
    /// Cranelift's variables do the `mem2reg` this IR has not done yet (D-59) — which is why
    /// slots can be lowered directly instead of needing SSA construction first.
    fn variable(&mut self, slot: u32) -> Variable {
        if let Some(variable) = self.slots.get(&slot) {
            return *variable;
        }
        // Cranelift hands out the variable; it is not constructed from the slot number.
        let variable = self.builder.declare_var(types::I64);
        self.slots.insert(slot, variable);
        variable
    }

    /// Whether the IR proved this value is a number.
    ///
    /// `Type::Number` and nothing weaker: `Unknown` may be a string, and lowering a float
    /// instruction for one would be a miscompilation rather than a slow path.
    fn is_number(&self, id: crisol_ir::ValueId) -> bool {
        self.types.get(&id.index()) == Some(&crisol_ir::Type::Number)
    }

    /// Whether the IR proved this value is a boolean.
    fn is_boolean(&self, id: crisol_ir::ValueId) -> bool {
        self.types.get(&id.index()) == Some(&crisol_ir::Type::Bool)
    }

    fn value(&self, id: crisol_ir::ValueId) -> ClifValue {
        *self
            .values
            .get(&id.index())
            .expect("the verifier proved every use is dominated by its definition")
    }

    /// Unpacks a NaN-boxed value into an `f64`.
    ///
    /// A bitcast and nothing more: a number is stored as its own bits (D-53), so there is no
    /// unmasking step. Only sound for a value the IR typed `Number`.
    fn as_f64(&mut self, value: ClifValue) -> ClifValue {
        self.builder
            .ins()
            .bitcast(types::F64, MemFlagsData::new(), value)
    }

    /// Re-boxes an `f64`. Also just a bitcast.
    ///
    /// Named `box_*` rather than `from_*` because these take `self` — clippy is right that a
    /// `from_` taking a receiver reads as a conversion *of* the receiver.
    fn box_f64(&mut self, value: ClifValue) -> ClifValue {
        self.builder
            .ins()
            .bitcast(types::I64, MemFlagsData::new(), value)
    }

    /// A boxed boolean from a Cranelift condition.
    fn box_condition(&mut self, condition: ClifValue) -> ClifValue {
        let yes = self
            .builder
            .ins()
            .iconst(types::I64, crisol_value::Value::TRUE.to_bits() as i64);
        let no = self
            .builder
            .ins()
            .iconst(types::I64, crisol_value::Value::FALSE.to_bits() as i64);
        self.builder.ins().select(condition, yes, no)
    }

    fn instruction(&mut self, instruction: &crisol_ir::Instruction) -> Result<(), CodegenError> {
        // The live set the IR recorded at this safepoint is exactly what Cranelift needs, so
        // it is handed over verbatim. This is the whole reason §M11 insisted safepoints be
        // explicit rather than inferred: without it there would be nothing to declare, and a
        // collection during an allocation would miss live references (§3.1).
        if let Some(safepoint) = &instruction.safepoint {
            for live in &safepoint.live {
                if let Some(value) = self.values.get(&live.index()).copied() {
                    self.builder.declare_value_needs_stack_map(value);
                }
            }
        }

        let produced = match &instruction.op {
            Op::Const(Constant::Number(number)) => {
                let bits = crisol_value::Value::number(*number).to_bits();
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "iconst takes an i64 and the bit pattern is what matters"
                )]
                Some(self.builder.ins().iconst(types::I64, bits as i64))
            }
            Op::Const(Constant::Undefined) => Some(
                self.builder
                    .ins()
                    .iconst(types::I64, crisol_value::Value::UNDEFINED.to_bits() as i64),
            ),
            Op::Const(Constant::Null) => Some(
                self.builder
                    .ins()
                    .iconst(types::I64, crisol_value::Value::NULL.to_bits() as i64),
            ),
            Op::Const(Constant::Bool(flag)) => {
                let boxed = if *flag {
                    crisol_value::Value::TRUE
                } else {
                    crisol_value::Value::FALSE
                };
                Some(
                    self.builder
                        .ins()
                        .iconst(types::I64, boxed.to_bits() as i64),
                )
            }
            Op::Load { slot } => {
                let variable = self.variable(*slot);
                Some(self.builder.use_var(variable))
            }
            Op::Store { slot, value } => {
                let variable = self.variable(*slot);
                let value = self.value(*value);
                self.builder.def_var(variable, value);
                None
            }
            Op::Binary { op, left, right }
                if matches!(
                    op,
                    BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
                ) =>
            {
                let left = self.value(*left);
                let right = self.value(*right);
                let left = self.as_f64(left);
                let right = self.as_f64(right);
                let result = match op {
                    BinaryOp::Subtract => self.builder.ins().fsub(left, right),
                    BinaryOp::Multiply => self.builder.ins().fmul(left, right),
                    BinaryOp::Divide => self.builder.ins().fdiv(left, right),
                    // Unreachable: the guard above admits exactly these three.
                    _ => unreachable!("guarded to the three with native instructions"),
                };
                Some(self.box_f64(result))
            }
            Op::Binary { op, left, right } => {
                // Everything not handled above is a call. D-79 in the machine code for `+`;
                // a libm call for `%` and `**`; and modular `ToInt32` for the bitwise family,
                // which Cranelift's saturating conversion cannot express.
                let Some(helper) = self.helpers.get(op).copied() else {
                    return Err(CodegenError::Unsupported {
                        operation: format!("binary operator {}", op.symbol()),
                    });
                };
                let left = self.value(*left);
                let right = self.value(*right);
                let call = self.builder.ins().call(helper, &[left, right]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::Unary { op, operand } => {
                let value = self.value(*operand);
                match op {
                    // Negation on a known number is one instruction. On anything else it needs
                    // `ToNumber` first, which is a call.
                    crisol_ir::UnaryOp::Negate if self.is_number(*operand) => {
                        let unpacked = self.as_f64(value);
                        let negated = self.builder.ins().fneg(unpacked);
                        Some(self.box_f64(negated))
                    }
                    // `+x` on a number is the identity — `ToNumber` of a number is itself.
                    crisol_ir::UnaryOp::ToNumber if self.is_number(*operand) => Some(value),
                    // `void x` evaluates its operand and gives `undefined`. The operand was
                    // already emitted above, so its side effects have happened.
                    crisol_ir::UnaryOp::Void => Some(
                        self.builder
                            .ins()
                            .iconst(types::I64, crisol_value::Value::UNDEFINED.to_bits() as i64),
                    ),
                    // `!x` on a boolean is a comparison against boxed `true`, inverted.
                    crisol_ir::UnaryOp::Not if self.is_boolean(*operand) => {
                        let boxed_true = self
                            .builder
                            .ins()
                            .iconst(types::I64, crisol_value::Value::TRUE.to_bits() as i64);
                        let is_true = self.builder.ins().icmp(
                            cranelift_codegen::ir::condcodes::IntCC::Equal,
                            value,
                            boxed_true,
                        );
                        let inverted = self.builder.ins().bxor_imm_u(is_true, 1);
                        Some(self.box_condition(inverted))
                    }
                    // `~`, `typeof`, and the coercing forms of the above need the runtime:
                    // `~` needs modular `ToInt32` (D-87), and `typeof` produces a string.
                    other => {
                        return Err(CodegenError::Unsupported {
                            operation: format!("unary operator {}", other.symbol()),
                        });
                    }
                }
            }
            Op::Compare {
                op,
                left: left_id,
                right: right_id,
            } => {
                let left = self.value(*left_id);
                let right = self.value(*right_id);
                let condition = match op {
                    CompareOp::Less
                    | CompareOp::LessEqual
                    | CompareOp::Greater
                    | CompareOp::GreaterEqual => {
                        let left = self.as_f64(left);
                        let right = self.as_f64(right);
                        let cc = match op {
                            CompareOp::Less => cranelift_codegen::ir::condcodes::FloatCC::LessThan,
                            CompareOp::LessEqual => {
                                cranelift_codegen::ir::condcodes::FloatCC::LessThanOrEqual
                            }
                            CompareOp::Greater => {
                                cranelift_codegen::ir::condcodes::FloatCC::GreaterThan
                            }
                            _ => cranelift_codegen::ir::condcodes::FloatCC::GreaterThanOrEqual,
                        };
                        self.builder.ins().fcmp(cc, left, right)
                    }
                    // On **numbers**, `===` is exactly `f64` equality: `NaN === NaN` is false
                    // and `fcmp eq` on NaN is false; `+0 === -0` is true and `fcmp eq` on the
                    // two zeroes is true. The IR's type lattice is what makes that reachable —
                    // on boxed values of unknown type a bit comparison gets both of those
                    // wrong, which is D-53's whole subject, so that case is still refused.
                    CompareOp::StrictEqual | CompareOp::StrictNotEqual
                        if self.is_number(*left_id) && self.is_number(*right_id) =>
                    {
                        let left = self.as_f64(left);
                        let right = self.as_f64(right);
                        let cc = if matches!(op, CompareOp::StrictEqual) {
                            cranelift_codegen::ir::condcodes::FloatCC::Equal
                        } else {
                            cranelift_codegen::ir::condcodes::FloatCC::NotEqual
                        };
                        self.builder.ins().fcmp(cc, left, right)
                    }
                    CompareOp::StrictEqual | CompareOp::StrictNotEqual => {
                        return Err(CodegenError::Unsupported {
                            operation: "strict equality on values of unknown type".to_owned(),
                        });
                    }
                };
                Some(self.box_condition(condition))
            }
            other => {
                return Err(CodegenError::Unsupported {
                    operation: format!("{other:?}")
                        .split_whitespace()
                        .next()
                        .unwrap_or("operation")
                        .to_owned(),
                });
            }
        };

        if let (Some(result), Some(id)) = (produced, instruction.result) {
            self.values.insert(id.index(), result);
            self.types.insert(id.index(), instruction.ty);
        }
        Ok(())
    }

    fn terminator(&mut self, terminator: &Terminator) -> Result<(), CodegenError> {
        match terminator {
            Terminator::Jump { target, args } => {
                let block = self.block(target.index());
                let args: Vec<cranelift_codegen::ir::BlockArg> = args
                    .iter()
                    .map(|arg| cranelift_codegen::ir::BlockArg::Value(self.value(*arg)))
                    .collect();
                self.builder.ins().jump(block, &args);
            }
            Terminator::Branch {
                condition,
                then_block,
                else_block,
                ..
            } => {
                let condition = self.value(*condition);
                // Truthiness is a bit comparison against boxed `true` only because the IR
                // typed this `Bool`. A general `ToBoolean` is a runtime call.
                let boxed_true = self
                    .builder
                    .ins()
                    .iconst(types::I64, crisol_value::Value::TRUE.to_bits() as i64);
                let is_true = self.builder.ins().icmp(
                    cranelift_codegen::ir::condcodes::IntCC::Equal,
                    condition,
                    boxed_true,
                );
                let then_clif = self.block(then_block.index());
                let else_clif = self.block(else_block.index());
                self.builder
                    .ins()
                    .brif(is_true, then_clif, &[], else_clif, &[]);
            }
            Terminator::Return(value) => {
                let value = match value {
                    Some(id) => self.value(*id),
                    None => self
                        .builder
                        .ins()
                        .iconst(types::I64, crisol_value::Value::UNDEFINED.to_bits() as i64),
                };
                self.builder.ins().return_(&[value]);
            }
            Terminator::Throw(_) => {
                return Err(CodegenError::Unsupported {
                    operation: "throw".to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// A backend that compiles into this process's memory and hands back callable addresses.
///
/// The object emitter proves a function *compiles*. This proves it **computes the right
/// answer**, which is a different claim and the one that was missing: `6 / 3` compiling says
/// nothing about whether it yields `2`.
///
/// §2.3's "no interpreter in shipped artifacts" is about the *application* binary.
/// `crisol-codegen` is a build-time crate, and M14's differential testing needs this same
/// ability — a design with only an object emitter cannot compare two backends' results without
/// a linker in the loop.
///
/// # Symbols are supplied by the caller
///
/// The helpers are **not** resolved from the host process automatically. A caller passes them
/// in, which keeps `crisol-codegen` free of a dependency on the runtime it generates calls to —
/// and makes the contract explicit at the point of use rather than implicit in a link order.
pub struct Jit {
    module: cranelift_jit::JITModule,
    context: FunctionBuilderContext,
    helpers: HashMap<BinaryOp, cranelift_module::FuncId>,
    compiled: HashMap<String, *const u8>,
}

impl std::fmt::Debug for Jit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jit")
            .field("compiled", &self.compiled.len())
            .finish_non_exhaustive()
    }
}

impl Jit {
    /// A JIT for the host, with `symbols` registered for the generated code to call.
    ///
    /// # Errors
    ///
    /// [`CodegenError`] when the host ISA cannot be determined or a symbol cannot be declared.
    pub fn new(symbols: &[(&str, *const u8)]) -> Result<Self, CodegenError> {
        let isa = cranelift_native::builder()
            .map_err(|message| CodegenError::UnknownTarget {
                triple: message.to_owned(),
            })?
            .finish(settings::Flags::new(settings::builder()))
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        let mut builder =
            cranelift_jit::JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        for (name, address) in symbols {
            builder.symbol(*name, *address);
        }
        let mut module = cranelift_jit::JITModule::new(builder);

        let mut signature = module.make_signature();
        signature.params.push(AbiParam::new(types::I64));
        signature.params.push(AbiParam::new(types::I64));
        signature.returns.push(AbiParam::new(types::I64));
        let mut helpers = HashMap::new();
        for (op, symbol) in HELPER_SYMBOLS {
            let id = module
                .declare_function(symbol, Linkage::Import, &signature)
                .map_err(|error| CodegenError::Backend {
                    message: error.to_string(),
                })?;
            helpers.insert(*op, id);
        }

        Ok(Self {
            module,
            context: FunctionBuilderContext::new(),
            helpers,
            compiled: HashMap::new(),
        })
    }

    /// Compiles a function and makes it callable.
    ///
    /// # Errors
    ///
    /// [`CodegenError`] naming what it refused.
    pub fn compile(&mut self, function: &Function) -> Result<Report, CodegenError> {
        let mut signature = self.module.make_signature();
        for _ in function.parameters.iter().chain(function.captures.iter()) {
            signature.params.push(AbiParam::new(types::I64));
        }
        signature.returns.push(AbiParam::new(types::I64));

        let id = self
            .module
            .declare_function(&function.name, Linkage::Export, &signature)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;

        let mut context = cranelift_codegen::Context::new();
        context.func.signature = signature;
        let frontend_config = self.module.target_config();
        let helpers: HashMap<BinaryOp, cranelift_codegen::ir::FuncRef> = self
            .helpers
            .iter()
            .map(|(op, id)| {
                (
                    *op,
                    self.module.declare_func_in_func(*id, &mut context.func),
                )
            })
            .collect();
        let builder = FunctionBuilder::new(&mut context.func, &mut self.context);
        let mut lowering = Lowering {
            builder,
            slots: HashMap::new(),
            values: HashMap::new(),
            types: HashMap::new(),
            blocks: HashMap::new(),
            helpers,
        };
        lowering.lower(function)?;
        lowering.builder.finalize(frontend_config);

        let stack_map_entries: usize = context
            .func
            .layout
            .blocks()
            .flat_map(|block| context.func.layout.block_insts(block))
            .filter_map(|inst| context.func.dfg.user_stack_map_entries(inst))
            .map(<[_]>::len)
            .sum();

        self.module
            .define_function(id, &mut context)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        self.module
            .finalize_definitions()
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        self.compiled.insert(
            function.name.clone(),
            self.module.get_finalized_function(id),
        );
        Ok(Report { stack_map_entries })
    }

    /// The address of a compiled function, if it was compiled.
    #[must_use]
    pub fn address(&self, name: &str) -> Option<*const u8> {
        self.compiled.get(name).copied()
    }
}
