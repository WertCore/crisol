//! The verifier.
//!
//! §M11's acceptance is that this "rejects malformed graphs". What counts as malformed is
//! narrower than it sounds, because the shape of the IR already makes several kinds
//! unrepresentable — a block cannot have two terminators or none, and an operation cannot
//! appear after one, since [`crate::Terminator`] is a separate field rather than a variant of
//! [`crate::Op`]. Those are not checks here because they cannot be written.
//!
//! What is left is what a type cannot express: that a value is defined before it is used *on
//! every path*, that a branch's arguments match what the block it jumps to expects, and that
//! every operation which can collect carries a safepoint. The last is the one §M11 singles
//! out — "the IR must represent safepoints explicitly or the GC integration in M13 will not
//! work" — and a missing one is exactly §3.1's use-after-free-under-pressure, so it is refused
//! here where it is still cheap to see.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::{BlockId, Function, Terminator, Type, ValueId};

/// Why a function is malformed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// A terminator names a block that does not exist.
    NoSuchBlock {
        /// Where from.
        from: BlockId,
        /// The bad target.
        target: BlockId,
    },
    /// A value is used but never defined.
    Undefined {
        /// The value.
        value: ValueId,
        /// Where it was used.
        at: BlockId,
    },
    /// A value is defined more than once, which SSA does not allow.
    DefinedTwice {
        /// The value.
        value: ValueId,
    },
    /// A value is used somewhere its definition does not reach.
    ///
    /// The check that needs dominance: a definition in one arm of a branch is not available in
    /// the other, even though both are "earlier" in block order.
    NotDominated {
        /// The value.
        value: ValueId,
        /// Where it was used.
        at: BlockId,
    },
    /// A jump passes the wrong number of arguments.
    WrongArgumentCount {
        /// Where from.
        from: BlockId,
        /// Where to.
        target: BlockId,
        /// How many were passed.
        passed: usize,
        /// How many the block takes.
        expected: usize,
    },
    /// A jump passes an argument of an incompatible type.
    WrongArgumentType {
        /// Where from.
        from: BlockId,
        /// Where to.
        target: BlockId,
        /// Which parameter.
        index: usize,
        /// What was passed.
        passed: Type,
        /// What the block takes.
        expected: Type,
    },
    /// An operation that can collect has no safepoint.
    MissingSafepoint {
        /// Where.
        at: BlockId,
        /// Which instruction in the block.
        index: usize,
    },
    /// A closure names a function that does not exist.
    NoSuchFunction {
        /// Where.
        at: BlockId,
        /// The bad id.
        function: u32,
    },
    /// A closure passes the wrong number of captured values.
    ///
    /// [`crate::Function::captures`] and [`crate::Op::Closure`]'s `captures` pair **by
    /// position**, so a mismatch means the callee reads a slot nobody filled — uninitialised,
    /// and plausible. This is the one check that cannot be done on a single function, which is
    /// why [`verify_module`] exists.
    WrongCaptureCount {
        /// Where.
        at: BlockId,
        /// Which function was closed over.
        function: u32,
        /// How many were passed.
        passed: usize,
        /// How many it expects.
        expected: usize,
    },
    /// An operation that cannot collect has a safepoint anyway.
    ///
    /// Rejected rather than ignored: a safepoint on a `Const` means whoever built this did not
    /// know which operations collect, and the ones they *missed* are the dangerous half.
    UnexpectedSafepoint {
        /// Where.
        at: BlockId,
        /// Which instruction in the block.
        index: usize,
    },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchBlock { from, target } => {
                write!(f, "{from} jumps to {target}, which does not exist")
            }
            Self::Undefined { value, at } => write!(f, "{at} uses {value}, which is never defined"),
            Self::DefinedTwice { value } => write!(f, "{value} is defined more than once"),
            Self::NotDominated { value, at } => write!(
                f,
                "{at} uses {value}, whose definition does not reach it on every path"
            ),
            Self::WrongArgumentCount {
                from,
                target,
                passed,
                expected,
            } => write!(
                f,
                "{from} passes {passed} arguments to {target}, which takes {expected}"
            ),
            Self::WrongArgumentType {
                from,
                target,
                index,
                passed,
                expected,
            } => write!(
                f,
                "{from} passes {passed} as argument {index} to {target}, which takes {expected}"
            ),
            Self::MissingSafepoint { at, index } => write!(
                f,
                "{at}[{index}] can collect and has no safepoint, so the GC would not see its live values"
            ),
            Self::NoSuchFunction { at, function } => {
                write!(
                    f,
                    "{at} closes over function @{function}, which does not exist"
                )
            }
            Self::WrongCaptureCount {
                at,
                function,
                passed,
                expected,
            } => write!(
                f,
                "{at} passes {passed} captures to function @{function}, which takes {expected}"
            ),
            Self::UnexpectedSafepoint { at, index } => {
                write!(f, "{at}[{index}] cannot collect but carries a safepoint")
            }
        }
    }
}

impl std::error::Error for VerifyError {}

/// Checks a function, returning every problem found.
///
/// Every problem, not the first: a malformed graph usually has one cause and several symptoms,
/// and stopping at the first makes the cause the hardest one to see.
///
/// # Errors
///
/// The list of [`VerifyError`]s, empty when the function is well-formed.
pub fn verify(function: &Function) -> Result<(), Vec<VerifyError>> {
    let mut errors = Vec::new();

    // ---- definitions, and where each one lives ----
    let mut defined: HashMap<ValueId, BlockId> = HashMap::new();
    let mut order: HashMap<ValueId, usize> = HashMap::new();
    for (raw, block) in function.blocks.iter().enumerate() {
        let id = BlockId(u32::try_from(raw).expect("blocks fit in u32"));
        for (at, (value, _)) in block.params.iter().enumerate() {
            if defined.insert(*value, id).is_some() {
                errors.push(VerifyError::DefinedTwice { value: *value });
            }
            order.insert(*value, at);
        }
        for (at, instruction) in block.instructions.iter().enumerate() {
            if let Some(value) = instruction.result {
                if defined.insert(value, id).is_some() {
                    errors.push(VerifyError::DefinedTwice { value });
                }
                // Offset past the parameters so that a parameter always precedes an
                // instruction in the same block.
                order.insert(value, block.params.len() + at);
            }
        }
    }

    let dominators = dominators(function);
    let types = value_types(function);

    for (raw, block) in function.blocks.iter().enumerate() {
        let id = BlockId(u32::try_from(raw).expect("blocks fit in u32"));

        // ---- safepoints ----
        for (at, instruction) in block.instructions.iter().enumerate() {
            match (
                instruction.op.can_collect(),
                instruction.safepoint.is_some(),
            ) {
                (true, false) => errors.push(VerifyError::MissingSafepoint { at: id, index: at }),
                (false, true) => {
                    errors.push(VerifyError::UnexpectedSafepoint { at: id, index: at });
                }
                _ => {}
            }
        }

        // ---- uses ----
        //
        // Gathered first, then checked, so the checking loop can push to `errors` without a
        // closure holding it borrowed for the whole block.
        let mut uses: Vec<(ValueId, Option<usize>)> = Vec::new();
        for (at, instruction) in block.instructions.iter().enumerate() {
            let position = Some(block.params.len() + at);
            for operand in instruction.op.operands() {
                uses.push((operand, position));
            }
            if let Some(safepoint) = &instruction.safepoint {
                for live in &safepoint.live {
                    uses.push((*live, position));
                }
            }
        }
        for operand in block.terminator.operands() {
            // A terminator runs after everything in its block, so nothing in the block can be
            // too late for it.
            uses.push((operand, None));
        }

        for (value, position) in uses {
            match defined.get(&value) {
                None => errors.push(VerifyError::Undefined { value, at: id }),
                Some(&home) if home == id => {
                    // Same block: the definition has to come first.
                    if let (Some(use_at), Some(def_at)) = (position, order.get(&value))
                        && *def_at >= use_at
                    {
                        errors.push(VerifyError::NotDominated { value, at: id });
                    }
                }
                Some(&home) => {
                    if !dominators.get(raw).is_some_and(|set| set.contains(&home.0)) {
                        errors.push(VerifyError::NotDominated { value, at: id });
                    }
                }
            }
        }

        // ---- edges ----
        for target in block.terminator.successors() {
            if function.get(target).is_none() {
                errors.push(VerifyError::NoSuchBlock { from: id, target });
            }
        }
        for (target, args) in edges(&block.terminator) {
            let Some(destination) = function.get(target) else {
                continue;
            };
            if args.len() != destination.params.len() {
                errors.push(VerifyError::WrongArgumentCount {
                    from: id,
                    target,
                    passed: args.len(),
                    expected: destination.params.len(),
                });
                continue;
            }
            for (index, (argument, (_, expected))) in
                args.iter().zip(destination.params.iter()).enumerate()
            {
                let passed = types.get(argument).copied().unwrap_or(Type::Unknown);
                if !passed.is_subtype_of(*expected) {
                    errors.push(VerifyError::WrongArgumentType {
                        from: id,
                        target,
                        index,
                        passed,
                        expected: *expected,
                    });
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// The `(target, args)` pairs a terminator produces.
fn edges(terminator: &Terminator) -> Vec<(BlockId, Vec<ValueId>)> {
    match terminator {
        Terminator::Jump { target, args } => vec![(*target, args.clone())],
        Terminator::Branch {
            then_block,
            then_args,
            else_block,
            else_args,
            ..
        } => vec![
            (*then_block, then_args.clone()),
            (*else_block, else_args.clone()),
        ],
        Terminator::Return(_) | Terminator::Throw(_) => Vec::new(),
    }
}

/// Every value's declared type.
fn value_types(function: &Function) -> HashMap<ValueId, Type> {
    let mut types = HashMap::new();
    for block in &function.blocks {
        for (value, ty) in &block.params {
            types.insert(*value, *ty);
        }
        for instruction in &block.instructions {
            if let Some(value) = instruction.result {
                types.insert(value, instruction.ty);
            }
        }
    }
    types
}

/// Which blocks dominate each block.
///
/// The textbook iterative algorithm: a block is dominated by itself and by everything that
/// dominates all of its predecessors. Run to a fixed point, which for this shape of graph is a
/// handful of passes.
///
/// Unreachable blocks are dominated by everything, which falls out of starting the sets full
/// and is the right answer: a use in a block nothing can reach cannot be wrong at runtime,
/// and reporting it would bury the real error under noise from dead code.
fn dominators(function: &Function) -> Vec<HashSet<u32>> {
    let count = function.blocks.len();
    let all: HashSet<u32> = (0..u32::try_from(count).unwrap_or(u32::MAX)).collect();

    let mut predecessors: Vec<Vec<u32>> = vec![Vec::new(); count];
    for (raw, block) in function.blocks.iter().enumerate() {
        for successor in block.terminator.successors() {
            if let Some(list) = predecessors.get_mut(successor.0 as usize) {
                list.push(u32::try_from(raw).expect("blocks fit in u32"));
            }
        }
    }

    let entry = function.entry.0 as usize;
    let mut sets: Vec<HashSet<u32>> = vec![all; count];
    if let Some(set) = sets.get_mut(entry) {
        *set = std::iter::once(function.entry.0).collect();
    }

    let mut changed = true;
    while changed {
        changed = false;
        for raw in 0..count {
            if raw == entry {
                continue;
            }
            let mut next: Option<HashSet<u32>> = None;
            for predecessor in &predecessors[raw] {
                let theirs = &sets[*predecessor as usize];
                next = Some(match next {
                    None => theirs.clone(),
                    Some(so_far) => so_far.intersection(theirs).copied().collect(),
                });
            }
            let mut next = next.unwrap_or_default();
            next.insert(u32::try_from(raw).expect("blocks fit in u32"));
            if next != sets[raw] {
                sets[raw] = next;
                changed = true;
            }
        }
    }
    sets
}

/// Checks a whole module: every function, plus the relationships between them.
///
/// **One check here cannot be done on a single function**: [`crate::Op::Closure`]'s captures
/// pair positionally with the target's [`crate::Function::captures`], and a mismatch means the
/// callee reads a slot nobody filled. That value is uninitialised and plausible — the worst
/// combination — and it is invisible to a verifier that only ever sees one function at a time.
///
/// # Errors
///
/// Every problem across every function, so one malformed closure does not hide the rest.
pub fn verify_module(functions: &[Function]) -> Result<(), Vec<VerifyError>> {
    let mut errors = Vec::new();
    for function in functions {
        if let Err(mut found) = verify(function) {
            errors.append(&mut found);
        }
    }

    for function in functions {
        for (raw, block) in function.blocks.iter().enumerate() {
            let id = BlockId::from_index(u32::try_from(raw).unwrap_or(u32::MAX));
            for instruction in &block.instructions {
                let crate::Op::Closure {
                    function: target,
                    captures,
                } = &instruction.op
                else {
                    continue;
                };
                let Some(callee) = functions.get(target.0 as usize) else {
                    errors.push(VerifyError::NoSuchFunction {
                        at: id,
                        function: target.0,
                    });
                    continue;
                };
                if captures.len() != callee.captures.len() {
                    errors.push(VerifyError::WrongCaptureCount {
                        at: id,
                        function: target.0,
                        passed: captures.len(),
                        expected: callee.captures.len(),
                    });
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
