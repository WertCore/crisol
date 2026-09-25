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

impl FunctionId {
    /// The index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

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

/// Arithmetic and bitwise operators.
///
/// **`Add` is the odd one out and the reason this is not simply "arithmetic".** Every other
/// operator here coerces both operands with `ToNumber` and produces a number. `+` does not: if
/// either operand is a string after `ToPrimitive`, it concatenates. So `1 + 1` is `2` and
/// `1 + "1"` is `"11"`, and an IR that typed `Add` as `Number` would let codegen emit a float
/// add for a string concatenation.
///
/// The shift operators are also not what they look like: they coerce to **int32** (or uint32
/// for `UnsignedShiftRight`) and back to a double, so `1 << 31` is negative and
/// `1 << 32` is `1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `+` — numeric addition **or** string concatenation.
    Add,
    /// `-`.
    Subtract,
    /// `*`.
    Multiply,
    /// `/`.
    Divide,
    /// `%` — the sign follows the *dividend*, unlike a mathematical modulo.
    Remainder,
    /// `**`.
    Exponent,
    /// `&`, on int32.
    BitAnd,
    /// `|`, on int32.
    BitOr,
    /// `^`, on int32.
    BitXor,
    /// `<<`, on int32.
    ShiftLeft,
    /// `>>`, sign-propagating, on int32.
    ShiftRight,
    /// `>>>`, zero-filling, on **uint32** — the one operator whose result can exceed `i32::MAX`.
    UnsignedShiftRight,
    /// `instanceof` — whether the right side's `prototype` is in the left side's chain.
    ///
    /// A binary operator rather than a comparison: it answers a question about the prototype
    /// chain, not about ordering or equality, and it is not symmetric in any sense `CompareOp`
    /// would suggest.
    InstanceOf,
    /// `==` — equality **after** coercion.
    ///
    /// A binary operator rather than a [`CompareOp`] on purpose. Every `CompareOp` has a
    /// machine instruction behind it when both sides are numbers; `==` never does, because
    /// deciding what to compare means reading both types first. Putting it here keeps the
    /// comparison lattice honest about which comparisons can be lowered to a `fcmp`.
    LooseEqual,
    /// `!=`.
    LooseNotEqual,
    /// `in` — whether a property is on the object or anywhere up its chain.
    In,
}

impl BinaryOp {
    /// Whether the result is always a number.
    ///
    /// This is the distinction codegen needs before it can emit a float instruction, so it is
    /// **listed rather than negated**. It was `!matches!(self, Self::Add)`, which is true of
    /// the arithmetic and also of `instanceof` — an operator that answers a boolean and was
    /// therefore typed `number` from the day it was added. Writing the true cases out means a
    /// new operator has to be classified rather than inheriting the answer for arithmetic.
    #[must_use]
    pub const fn is_always_numeric(self) -> bool {
        matches!(
            self,
            Self::Subtract
                | Self::Multiply
                | Self::Divide
                | Self::Remainder
                | Self::Exponent
                | Self::BitAnd
                | Self::BitOr
                | Self::BitXor
                | Self::ShiftLeft
                | Self::ShiftRight
                | Self::UnsignedShiftRight
        )
    }

    /// Whether the result is always a boolean.
    #[must_use]
    pub const fn is_always_boolean(self) -> bool {
        matches!(
            self,
            Self::InstanceOf | Self::LooseEqual | Self::LooseNotEqual | Self::In
        )
    }

    /// The symbol, for the text dump.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Remainder => "%",
            Self::Exponent => "**",
            Self::BitAnd => "&",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::ShiftLeft => "<<",
            Self::ShiftRight => ">>",
            Self::UnsignedShiftRight => ">>>",
            Self::InstanceOf => "instanceof",
            Self::LooseEqual => "==",
            Self::LooseNotEqual => "!=",
            Self::In => "in",
        }
    }
}

/// Operators taking one operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-`.
    Negate,
    /// `+` — `ToNumber`, which is why `+"1"` is `1`.
    ToNumber,
    /// `!` — `ToBoolean` then inverted, so it never fails.
    Not,
    /// `~`, on int32.
    BitNot,
    /// `typeof` — the only operator that does **not** throw on an undeclared identifier.
    TypeOf,
    /// `void` — evaluates its operand and gives `undefined`.
    Void,
    /// `throw` — records the operand as the value in flight and yields the exception signal.
    ///
    /// An operation rather than a terminator, so that a `throw` inside a `try` is followed by
    /// the same check every call is. One propagation path rather than two means a handler
    /// cannot be reached by one and missed by the other.
    Throw,
    /// Whether the operand is the exception signal rather than a value.
    ///
    /// **This is how explicit propagation is written down.** A call returns the signal instead
    /// of a result, and the frontend follows every call with this test and a branch — so the
    /// unwinding is ordinary control flow the verifier already checks, rather than metadata a
    /// backend has to honour.
    IsException,
}

impl UnaryOp {
    /// The symbol, for the text dump.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Negate => "-",
            Self::ToNumber => "+",
            Self::Not => "!",
            Self::BitNot => "~",
            Self::TypeOf => "typeof",
            Self::Void => "void",
            Self::Throw => "throw",
            Self::IsException => "is-exception",
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
        /// The receiver — what `this` is inside the callee.
        ///
        /// **Not optional.** A plain `f()` passes `undefined` explicitly rather than omitting
        /// it, because "no receiver" and "a receiver that is `undefined`" are the same thing
        /// in the language and making one of them absent invites a lowering to forget it.
        /// `o.m()` must pass `o`: losing the receiver means `this` inside `m` is wrong, and
        /// that is silent — the call still happens and still returns something.
        this_value: ValueId,
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
    /// The names a `for-in` over `object` visits, as an array.
    ///
    /// Computed once, before the loop runs. The specification allows a property deleted during
    /// the loop to be skipped and one added not to be visited; taking the list up front is
    /// within that, and it means the loop cannot be affected by its own body in a way that
    /// depends on enumeration order.
    Enumerate {
        /// What to enumerate.
        object: ValueId,
    },
    /// What a `for-of` over `object` walks, as something indexable.
    ///
    /// An array is itself, indexed live; a string becomes an array of its code points. Anything
    /// else raises, because without `Symbol.iterator` there is nothing to ask.
    Iterate {
        /// What to iterate.
        object: ValueId,
    },
    /// `/source/flags` — a regular expression object.
    ///
    /// The pattern and flags are constants, so they ride in the operation rather than as
    /// operands: a literal's pattern cannot be computed.
    CreateRegExp {
        /// The pattern, without its delimiters.
        source: String,
        /// The flag letters.
        flags: String,
    },
    /// Appends to an array being built by a literal.
    ///
    /// **Two jobs in one operation because they differ by one bit at the call site.** With
    /// `spread`, every element of the operand is appended; without it, the operand itself is.
    /// `[...a]` and `[a]` differ in exactly that and nothing else.
    ArrayExtend {
        /// The array under construction.
        array: ValueId,
        /// What to append, or to append the elements of.
        value: ValueId,
        /// Whether `value` is spread.
        spread: bool,
    },
    /// `delete object[key]`.
    ///
    /// One operation for both spellings, because `delete o.x` and `delete o["x"]` are the same
    /// thing — the frontend makes a string constant for the static form rather than the IR
    /// carrying two shapes of the same question.
    Delete {
        /// The receiver.
        object: ValueId,
        /// The key, as a value.
        key: ValueId,
    },
    /// Reads a name that resolves to no binding: a global, answering `undefined` when it is
    /// absent instead of raising.
    ///
    /// **Only `typeof` may ask this way.** Every other read of a missing global is a
    /// `ReferenceError`, and `typeof` is the one operator the specification exempts — which is
    /// why `typeof somethingUndeclared` is `"undefined"` and not a thrown error.
    GlobalLoadOptional {
        /// The name.
        name: PropertyKey,
    },
    /// Reads a name that resolves to no binding: a global.
    ///
    /// Distinct from [`Op::Load`] because a global is not a slot — it is a property of an
    /// object the runtime owns, and it may not exist, which is a `ReferenceError` rather than
    /// `undefined`. Making it look like a local read is what turned `Object` into a fresh
    /// empty variable.
    GlobalLoad {
        /// The name.
        name: PropertyKey,
    },
    /// The value a `catch` binds — whatever the throw in flight is carrying.
    ///
    /// Nullary, because the value is not in any register the IR can name: it was recorded by
    /// the `throw` and the frames between have already returned.
    CaughtValue,
    /// Reads a property whose name is computed: `o[k]`.
    ///
    /// Separate from [`Op::PropertyLoad`] because the key is a *value*, not a name known when
    /// the IR is built. JavaScript makes no distinction between `a[0]` and `a["0"]` — element
    /// access **is** property access with a computed key — so one operation covers both, and
    /// the runtime decides whether the key names an element or a property.
    ComputedLoad {
        /// The receiver.
        object: ValueId,
        /// The key, as a value.
        key: ValueId,
    },
    /// Writes a property whose name is computed: `o[k] = v`.
    ComputedStore {
        /// The receiver.
        object: ValueId,
        /// The key, as a value.
        key: ValueId,
        /// What to store.
        value: ValueId,
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
    /// Defines an accessor property — `{get x() {…}}` and `{set x(v) {…}}`.
    ///
    /// **Not a `PropertyStore` of a function.** A getter is *called* on read and a data
    /// property holding a function is not, so lowering one as the other is a wrong answer
    /// rather than a missing feature: `({get x() { return 1; }}).x` was the function.
    ///
    /// One operation for both halves, because `{get x() {…}, set x(v) {…}}` is a single
    /// property with two functions on it — defining them separately would make the second
    /// replace the first.
    DefineAccessor {
        /// The receiver.
        object: ValueId,
        /// The name.
        key: PropertyKey,
        /// The getter, or `undefined` when there is none.
        getter: ValueId,
        /// The setter, or `undefined` when there is none.
        setter: ValueId,
    },
    /// Sets `object`'s `[[Prototype]]`. `class B extends A` links `B.prototype` to `A.prototype`
    /// and `B` to `A`, which is what makes an instance inherit the parent's methods and a static
    /// call reach the parent's statics.
    SetPrototype {
        /// The object whose prototype changes.
        object: ValueId,
        /// The new prototype.
        prototype: ValueId,
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
    /// `new callee(args)`.
    ///
    /// One op rather than the sequence it stands for, because that sequence has a rule no
    /// caller should have to remember: **a constructor that returns an object replaces the
    /// newly created `this`**, while one returning a primitive does not. Spelling `new` out as
    /// allocate-then-call would put that rule at every call site, and the first lowering to
    /// forget it would produce a constructor whose explicit `return` is silently ignored.
    ///
    /// It also covers `OrdinaryCreateFromConstructor`, which reads `callee.prototype` — so the
    /// prototype link is established here rather than needing a separate op that could be
    /// omitted.
    Construct {
        /// The constructor.
        callee: ValueId,
        /// Its arguments.
        args: Vec<ValueId>,
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
    /// Arithmetic or a bitwise operation.
    Binary {
        /// Which operator.
        op: BinaryOp,
        /// Left operand.
        left: ValueId,
        /// Right operand.
        right: ValueId,
    },
    /// A one-operand operator.
    Unary {
        /// Which operator.
        op: UnaryOp,
        /// The operand.
        operand: ValueId,
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
                // `+` reaches `ToPrimitive`, which calls `valueOf` or `toString` — user code,
                // which can allocate. The other operators coerce primitives that already exist.
                | Self::Binary { op: BinaryOp::Add, .. }
                | Self::PropertyLoad { .. }
                | Self::PropertyStore { .. }
                | Self::DefineAccessor { .. }
                | Self::ComputedLoad { .. }
                | Self::ComputedStore { .. }
                | Self::Delete { .. }
                | Self::Enumerate { .. }
                | Self::Iterate { .. }
                | Self::CreateRegExp { .. }
                | Self::ArrayExtend { .. }
                | Self::Construct { .. }
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
            Self::Const(_)
            | Self::Load { .. }
            | Self::CreateObject { .. }
            | Self::CreateRegExp { .. }
            | Self::CaughtValue
            | Self::GlobalLoad { .. }
            | Self::GlobalLoadOptional { .. } => Vec::new(),
            Self::Store { value, .. } | Self::Await { value } => vec![*value],
            Self::Call {
                callee,
                this_value,
                args,
            } => {
                let mut all = vec![*callee, *this_value];
                all.extend(args);
                all
            }
            Self::PropertyLoad { object, .. }
            | Self::Enumerate { object }
            | Self::Iterate { object } => vec![*object],
            Self::PropertyStore { object, value, .. } => vec![*object, *value],
            Self::SetPrototype { object, prototype } => vec![*object, *prototype],
            Self::DefineAccessor {
                object,
                getter,
                setter,
                ..
            } => vec![*object, *getter, *setter],
            Self::ComputedLoad { object, key } | Self::Delete { object, key } => {
                vec![*object, *key]
            }
            Self::ArrayExtend { array, value, .. } => vec![*array, *value],
            Self::ComputedStore { object, key, value } => vec![*object, *key, *value],
            Self::CreateArray { elements } => elements.clone(),
            Self::Construct { callee, args } => {
                let mut all = vec![*callee];
                all.extend(args);
                all
            }
            Self::Closure { captures, .. } => captures.clone(),
            Self::Compare { left, right, .. } | Self::Binary { left, right, .. } => {
                vec![*left, *right]
            }
            Self::Unary { operand, .. } => vec![*operand],
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
    /// Slots that receive the arguments, in declaration order.
    ///
    /// Slots rather than block parameters because locals live in slots until `mem2reg`
    /// (D-59), and a parameter is a local that arrives pre-assigned. When that pass lands
    /// these become entry-block parameters and this list goes away.
    pub parameters: Vec<u32>,
    /// Its own index in the module's function list.
    ///
    /// [`Op::Closure`] names a callee by [`FunctionId`], and a backend has to turn that into
    /// the right compiled function. It could instead rely on compiling them in order and
    /// counting — which is true today and is exactly the kind of coupling that breaks quietly
    /// the first time anything compiles them in a different order or skips one.
    ///
    /// [`crate::verify_module`] checks `functions[i].id == i`, so the field cannot drift from
    /// the position it claims.
    pub id: FunctionId,
    /// The slot holding `this`, when the function binds one.
    ///
    /// `None` for an arrow function, which does not bind its own `this` but captures the
    /// enclosing one — so for an arrow, `this` arrives as a capture like any other value.
    ///
    /// Recorded rather than left implicit. The frontend declares `this` ahead of the
    /// parameters, so it is slot zero in every ordinary function, and the backend could simply
    /// assume that. It would be right today and wrong the first time anything is declared
    /// earlier, and the failure would be a `this` bound to some other local — a plausible
    /// value, not a crash.
    pub this_slot: Option<u32>,
    /// The slot holding `arguments`, for a function whose body names it.
    ///
    /// `None` when the body never mentions it, which is almost every function — **building the
    /// array unconditionally would put an allocation in the prologue of every call**, and the
    /// collector would have to trace it, for a binding nothing reads.
    ///
    /// On the function rather than inferred from position, for the same reason `this_slot` is:
    /// the backend has to know which slot to fill, and guessing would bind a plausible wrong
    /// value rather than fail.
    pub arguments_slot: Option<u32>,
    /// Slots that receive the captured values, positionally matching [`Op::Closure`]'s
    /// `captures`.
    ///
    /// **The pairing is by position**, so the two must be built together. A closure that passed
    /// three values to a function expecting two would otherwise leave the third slot
    /// uninitialised — and an uninitialised slot holds a plausible value, which is the worst
    /// kind of wrong.
    ///
    /// [`crate::verify_module`] checks this, and it is the one rule that **cannot** be checked
    /// by looking at a single function, which is why that entry point exists alongside
    /// [`crate::verify`].
    pub captures: Vec<u32>,
    /// The first block.
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
            id: FunctionId(0),
            name: name.to_owned(),
            parameters: Vec::new(),
            this_slot: None,
            arguments_slot: None,
            captures: Vec::new(),
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
