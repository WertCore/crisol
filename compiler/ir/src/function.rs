//! Functions, blocks, instructions.
//!
//! # Shape of the IR
//!
//! SSA with **block parameters** rather than phi nodes. The two are equivalent, and block
//! parameters are what Cranelift takes — §4 names Cranelift as the codegen backend, so an IR
//! that already speaks in block parameters lowers without an intermediate rewrite. They also
//! make the awkward case of a phi obvious rather than implicit: a value that differs by
//! predecessor is *passed*, and the verifier can check the argument count at every edge.
//!
//! **Terminators are a separate type from operations.** A block holds a list of [`Op`] and
//! exactly one [`Terminator`], so "every block ends with exactly one terminator, and no
//! terminator appears in the middle" is not a rule the verifier enforces — it is a shape the
//! type system does not permit to be wrong. What is left for the verifier is the part that
//! genuinely cannot be structural.
//!
//! # Safepoints
//!
//! §M11 says the IR must represent safepoints explicitly *or the GC integration in M13 will
//! not work*, which is a strong enough claim to build around rather than annotate afterwards.
//! Any operation that can allocate or transfer control — and therefore can collect — carries a
//! [`Safepoint`] naming the values that must stay reachable across it. [`Op::can_collect`]
//! decides which those are, and the verifier rejects one without a safepoint: a missing
//! safepoint is precisely the bug that produces a use-after-free under memory pressure
//! (§3.1), so it is refused at the point where it is still cheap to see.

use std::fmt;

use crisol_value::{PropertyKey, ShapeId};

use crate::Type;

/// An SSA value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ValueId(pub(crate) u32);

impl ValueId {
    /// The index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }

    /// Rebuilds an id from [`ValueId::index`]. For tests and for a pass keeping a side table.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }
}

impl fmt::Display for ValueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A basic block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub(crate) u32);

impl BlockId {
    /// The index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }

    /// Rebuilds an id from [`BlockId::index`].
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }

    /// The block every function starts with.
    pub const ENTRY: Self = Self(0);
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

/// Another function, for [`Op::Closure`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FunctionId(pub u32);

/// A literal.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    /// `undefined`.
    Undefined,
    /// `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number.
    Number(f64),
    /// A string.
    String(String),
}

impl Constant {
    /// The type this constant has, which is always exact.
    #[must_use]
    pub const fn type_of(&self) -> Type {
        match self {
            Self::Undefined => Type::Undefined,
            Self::Null => Type::Null,
            Self::Bool(_) => Type::Bool,
            Self::Number(_) => Type::Number,
            Self::String(_) => Type::String,
        }
    }
}

/// How two values are compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareOp {
    /// `===`.
    StrictEqual,
    /// `!==`.
    StrictNotEqual,
    /// `<`.
    Less,
    /// `<=`.
    LessEqual,
    /// `>`.
    Greater,
    /// `>=`.
    GreaterEqual,
}

/// An operation that produces a value, or has an effect, but does not end a block.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// A literal.
    Const(Constant),
    /// Reads a local slot.
    Load {
        /// Which slot.
        slot: u32,
    },
    /// Writes a local slot.
    Store {
        /// Which slot.
        slot: u32,
        /// What to write.
        value: ValueId,
    },
    /// Calls a value.
    Call {
        /// What is being called.
        callee: ValueId,
        /// Its arguments.
        args: Vec<ValueId>,
    },
    /// Reads a property.
    PropertyLoad {
        /// The receiver.
        object: ValueId,
        /// The name.
        key: PropertyKey,
    },
    /// Writes a property.
    PropertyStore {
        /// The receiver.
        object: ValueId,
        /// The name.
        key: PropertyKey,
        /// What to write.
        value: ValueId,
    },
    /// Allocates an object.
    CreateObject {
        /// Its initial shape.
        shape: ShapeId,
    },
    /// Allocates an array.
    CreateArray {
        /// Its initial elements.
        elements: Vec<ValueId>,
    },
    /// Allocates a closure.
    Closure {
        /// The function it runs.
        function: FunctionId,
        /// The values it captures.
        captures: Vec<ValueId>,
    },
    /// Suspends until a promise settles.
    Await {
        /// What is awaited.
        value: ValueId,
    },
    /// Compares two values.
    Compare {
        /// Which comparison.
        op: CompareOp,
        /// Left operand.
        left: ValueId,
        /// Right operand.
        right: ValueId,
    },
}

impl Op {
    /// Whether a collection can happen during this operation.
    ///
    /// The allocating ones, and the ones that transfer control to code that might allocate.
    /// `Const`, `Load`, `Store` and `Compare` cannot: they touch values that already exist.
    ///
    /// A `PropertyLoad` **can** — a getter is a call, and on an exotic shape (D-54) the lookup
    /// itself runs user code. Treating it as safe would be right for the common case and wrong
    /// for the one that matters, which is the wrong way round for something whose failure mode
    /// is a use-after-free.
    #[must_use]
    pub const fn can_collect(&self) -> bool {
        matches!(
            self,
            Self::Call { .. }
                | Self::PropertyLoad { .. }
                | Self::PropertyStore { .. }
                | Self::CreateObject { .. }
                | Self::CreateArray { .. }
                | Self::Closure { .. }
                | Self::Await { .. }
        )
    }

    /// Every value this operation reads.
    #[must_use]
    pub fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Const(_) | Self::Load { .. } | Self::CreateObject { .. } => Vec::new(),
            Self::Store { value, .. } | Self::Await { value } => vec![*value],
            Self::Call { callee, args } => {
                let mut all = vec![*callee];
                all.extend(args);
                all
            }
            Self::PropertyLoad { object, .. } => vec![*object],
            Self::PropertyStore { object, value, .. } => vec![*object, *value],
            Self::CreateArray { elements } => elements.clone(),
            Self::Closure { captures, .. } => captures.clone(),
            Self::Compare { left, right, .. } => vec![*left, *right],
        }
    }
}

/// The values that must stay reachable across an operation that can collect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Safepoint {
    /// Values live across this point.
    pub live: Vec<ValueId>,
}

/// One instruction: an operation, what it defines, and its safepoint if it needs one.
#[derive(Clone, Debug, PartialEq)]
pub struct Instruction {
    /// The value produced, when there is one. `Store` produces nothing.
    pub result: Option<ValueId>,
    /// The type of `result`.
    pub ty: Type,
    /// What it does.
    pub op: Op,
    /// Present exactly when [`Op::can_collect`] is true — the verifier says so.
    pub safepoint: Option<Safepoint>,
}

/// How a block ends. Exactly one per block, by construction.
#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    /// Unconditional branch.
    Jump {
        /// Where to.
        target: BlockId,
        /// Arguments for the target's parameters.
        args: Vec<ValueId>,
    },
    /// Conditional branch.
    Branch {
        /// What decides.
        condition: ValueId,
        /// Taken when truthy.
        then_block: BlockId,
        /// Arguments for it.
        then_args: Vec<ValueId>,
        /// Taken otherwise.
        else_block: BlockId,
        /// Arguments for it.
        else_args: Vec<ValueId>,
    },
    /// Leaves the function.
    Return(Option<ValueId>),
    /// Throws.
    Throw(ValueId),
}

impl Terminator {
    /// The blocks this can transfer to.
    #[must_use]
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Self::Jump { target, .. } => vec![*target],
            Self::Branch {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
            Self::Return(_) | Self::Throw(_) => Vec::new(),
        }
    }

    /// Every value this reads.
    #[must_use]
    pub fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Jump { args, .. } => args.clone(),
            Self::Branch {
                condition,
                then_args,
                else_args,
                ..
            } => {
                let mut all = vec![*condition];
                all.extend(then_args);
                all.extend(else_args);
                all
            }
            Self::Return(value) => value.iter().copied().collect(),
            Self::Throw(value) => vec![*value],
        }
    }
}

/// A basic block.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// Values that arrive from predecessors, with their types. SSA's phi nodes, made explicit.
    pub params: Vec<(ValueId, Type)>,
    /// What it does.
    pub instructions: Vec<Instruction>,
    /// How it ends.
    pub terminator: Terminator,
}

/// A function in SSA form.
#[derive(Clone, Debug)]
pub struct Function {
    /// For the text dump.
    pub name: String,
    /// Parameters, as the entry block's parameters.
    pub entry: BlockId,
    /// Every block. Index is [`BlockId`].
    pub blocks: Vec<Block>,
    /// How many SSA values have been handed out.
    pub values: u32,
}

impl Function {
    /// An empty function with one block that returns `undefined`.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            entry: BlockId(0),
            blocks: vec![Block {
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: Terminator::Return(None),
            }],
            values: 0,
        }
    }

    /// Hands out a fresh SSA value.
    pub fn value(&mut self) -> ValueId {
        let id = ValueId(self.values);
        self.values += 1;
        id
    }

    /// Adds a block.
    pub fn block(&mut self, block: Block) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).expect("blocks fit in u32"));
        self.blocks.push(block);
        id
    }

    /// A block, if the id names one.
    #[must_use]
    pub fn get(&self, block: BlockId) -> Option<&Block> {
        self.blocks.get(block.0 as usize)
    }

    /// A block, mutably.
    pub fn get_mut(&mut self, block: BlockId) -> Option<&mut Block> {
        self.blocks.get_mut(block.0 as usize)
    }
}
