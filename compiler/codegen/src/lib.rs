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
use cranelift_codegen::settings;
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

/// The Cranelift backend.
///
/// `Debug` reports what it is targeting and nothing else: an `ObjectModule` holds every
/// function emitted so far, and printing that would bury a test failure under a megabyte of
/// machine code.
pub struct Cranelift {
    triple: String,
    module: ObjectModule,
    context: FunctionBuilderContext,
    /// The runtime helper `+` calls. Declared once and reused.
    add_helper: cranelift_module::FuncId,
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
        let flags = settings::builder();
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

        // `+` is a call, for the reason in the module docs. Declared as an import so the
        // runtime supplies it at link time.
        let mut signature = module.make_signature();
        signature.params.push(AbiParam::new(types::I64));
        signature.params.push(AbiParam::new(types::I64));
        signature.returns.push(AbiParam::new(types::I64));
        let add_helper = module
            .declare_function("crisol_add", Linkage::Import, &signature)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;

        Ok(Self {
            triple: triple.to_owned(),
            module,
            context: FunctionBuilderContext::new(),
            add_helper,
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
        let add = self
            .module
            .declare_func_in_func(self.add_helper, &mut context.func);
        let builder = FunctionBuilder::new(&mut context.func, &mut self.context);
        let mut lowering = Lowering {
            builder,
            slots: HashMap::new(),
            values: HashMap::new(),
            blocks: HashMap::new(),
            add,
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
    /// IR block to Cranelift block.
    blocks: HashMap<u32, cranelift_codegen::ir::Block>,
    /// The declared `+` helper, resolved into this function.
    add: cranelift_codegen::ir::FuncRef,
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
            Op::Binary { op, left, right } if op.is_always_numeric() => {
                let left = self.value(*left);
                let right = self.value(*right);
                let left = self.as_f64(left);
                let right = self.as_f64(right);
                let result = match op {
                    BinaryOp::Subtract => self.builder.ins().fsub(left, right),
                    BinaryOp::Multiply => self.builder.ins().fmul(left, right),
                    BinaryOp::Divide => self.builder.ins().fdiv(left, right),
                    // `%`, `**` and the bitwise operators need int32 coercion or a libm call,
                    // and guessing at either would produce plausible wrong numbers.
                    other => {
                        return Err(CodegenError::Unsupported {
                            operation: format!("binary operator {}", other.symbol()),
                        });
                    }
                };
                Some(self.box_f64(result))
            }
            Op::Binary {
                op: BinaryOp::Add,
                left,
                right,
            } => {
                // D-79 in the machine code: `+` may concatenate, so the IR types it `Unknown`
                // and an `Unknown` cannot become a float add. A call is the correct lowering,
                // not a fallback.
                let left = self.value(*left);
                let right = self.value(*right);
                let call = self.builder.ins().call(self.add, &[left, right]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::Compare { op, left, right } => {
                let left = self.value(*left);
                let right = self.value(*right);
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
                    // `===` on boxed values is a bit comparison *except* for NaN and ±0, and
                    // getting that wrong is D-53's whole subject. Refused rather than
                    // approximated.
                    CompareOp::StrictEqual | CompareOp::StrictNotEqual => {
                        return Err(CodegenError::Unsupported {
                            operation: "strict equality".to_owned(),
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
