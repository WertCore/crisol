//! The text dump.
//!
//! §M11's acceptance asks that this be "stable and reviewable", which is two requirements and
//! the second is the harder one. Reviewable means a person reading a diff of two dumps can see
//! what an optimisation did, so every line carries its own context: a value's type is printed
//! at its definition rather than looked up, and a safepoint's live set is printed inline rather
//! than in a table somewhere else. The dump is wider for it, and a diff of it is readable
//! without the rest of the file.
//!
//! Stable means the output depends only on the function, never on iteration order of a hash
//! map or on an address. Everything printed here comes from a `Vec`.

use std::fmt;

use crate::{Block, BlockId, Constant, Function, Instruction, Op, Terminator, Type};

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "function {}", self.name)?;
        if !self.parameters.is_empty() {
            let slots: Vec<String> = self.parameters.iter().map(|s| format!("${s}")).collect();
            write!(f, "({})", slots.join(", "))?;
        }
        if !self.captures.is_empty() {
            // Printed separately from the parameters because they arrive by a different route
            // — a parameter comes from the call site, a capture from the closure.
            let slots: Vec<String> = self.captures.iter().map(|s| format!("${s}")).collect();
            write!(f, " captures [{}]", slots.join(", "))?;
        }
        writeln!(f, " {{")?;
        for (raw, block) in self.blocks.iter().enumerate() {
            let id = BlockId(u32::try_from(raw).unwrap_or(u32::MAX));
            write_block(f, id, block, id == self.entry)?;
        }
        writeln!(f, "}}")
    }
}

fn write_block(
    f: &mut fmt::Formatter<'_>,
    id: BlockId,
    block: &Block,
    is_entry: bool,
) -> fmt::Result {
    write!(f, "{id}")?;
    if !block.params.is_empty() {
        let params: Vec<String> = block
            .params
            .iter()
            .map(|(value, ty)| format!("{value}: {ty}"))
            .collect();
        write!(f, "({})", params.join(", "))?;
    }
    write!(f, ":")?;
    if is_entry {
        // Marked rather than assumed to be block zero: a pass that reorders blocks should not
        // silently change which one runs first.
        write!(f, "  ; entry")?;
    }
    writeln!(f)?;

    for instruction in &block.instructions {
        write_instruction(f, instruction)?;
    }
    write_terminator(f, &block.terminator)
}

fn write_instruction(f: &mut fmt::Formatter<'_>, instruction: &Instruction) -> fmt::Result {
    write!(f, "    ")?;
    if let Some(result) = instruction.result {
        write!(f, "{result}: {} = ", instruction.ty)?;
    }
    write_op(f, &instruction.op)?;
    if let Some(safepoint) = &instruction.safepoint {
        let live: Vec<String> = safepoint.live.iter().map(ToString::to_string).collect();
        write!(f, "  ; safepoint [{}]", live.join(", "))?;
    }
    writeln!(f)
}

fn write_op(f: &mut fmt::Formatter<'_>, op: &Op) -> fmt::Result {
    match op {
        Op::Const(constant) => write!(f, "const {}", Literal(constant)),
        Op::Load { slot } => write!(f, "load ${slot}"),
        Op::Store { slot, value } => write!(f, "store ${slot}, {value}"),
        Op::Call {
            callee,
            this_value,
            args,
        } => {
            let args: Vec<String> = args.iter().map(ToString::to_string).collect();
            // The receiver is printed first and marked, so a call that lost one is visible in
            // a diff rather than needing to be counted out of the argument list.
            write!(
                f,
                "call {callee}(this={this_value}{}{})",
                if args.is_empty() { "" } else { ", " },
                args.join(", ")
            )
        }
        // Quoted, because a property name is an arbitrary string: `obj[""]` and
        // `obj["a b"]` are both legal, and an unquoted dump of either is ambiguous exactly
        // where someone is squinting at it to work out what went wrong.
        Op::PropertyLoad { object, key } => write!(f, "get {object}.{key:?}"),
        Op::PropertyStore { object, key, value } => write!(f, "set {object}.{key:?} = {value}"),
        Op::ComputedLoad { object, key } => write!(f, "get {object}[{key}]"),
        Op::Delete { object, key } => write!(f, "delete {object}[{key}]"),
        Op::ComputedStore { object, key, value } => write!(f, "set {object}[{key}] = {value}"),
        Op::CreateObject { shape } => write!(f, "object #{}", shape.index()),
        Op::CaughtValue => write!(f, "caught"),
        Op::GlobalLoad { name } => write!(f, "global {name:?}"),
        Op::Construct { callee, args } => {
            let args: Vec<String> = args.iter().map(ToString::to_string).collect();
            write!(f, "new {callee}({})", args.join(", "))
        }
        Op::CreateArray { elements } => {
            let elements: Vec<String> = elements.iter().map(ToString::to_string).collect();
            write!(f, "array [{}]", elements.join(", "))
        }
        Op::Closure { function, captures } => {
            let captures: Vec<String> = captures.iter().map(ToString::to_string).collect();
            write!(f, "closure @{} [{}]", function.0, captures.join(", "))
        }
        Op::Await { value } => write!(f, "await {value}"),
        Op::Compare { op, left, right } => write!(f, "{} {left}, {right}", CompareName(*op)),
        Op::Binary { op, left, right } => write!(f, "{} {left}, {right}", op.symbol()),
        Op::Unary { op, operand } => write!(f, "{} {operand}", op.symbol()),
    }
}

fn write_terminator(f: &mut fmt::Formatter<'_>, terminator: &Terminator) -> fmt::Result {
    match terminator {
        Terminator::Jump { target, args } => {
            write!(f, "    jump {target}")?;
            write_args(f, args)?;
            writeln!(f)
        }
        Terminator::Branch {
            condition,
            then_block,
            then_args,
            else_block,
            else_args,
        } => {
            write!(f, "    branch {condition} -> {then_block}")?;
            write_args(f, then_args)?;
            write!(f, ", {else_block}")?;
            write_args(f, else_args)?;
            writeln!(f)
        }
        Terminator::Return(None) => writeln!(f, "    return"),
        Terminator::Return(Some(value)) => writeln!(f, "    return {value}"),
        Terminator::Throw(value) => writeln!(f, "    throw {value}"),
    }
}

fn write_args(f: &mut fmt::Formatter<'_>, args: &[crate::ValueId]) -> fmt::Result {
    if args.is_empty() {
        return Ok(());
    }
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    write!(f, "({})", args.join(", "))
}

/// A constant, printed so it can be read back unambiguously.
struct Literal<'a>(&'a Constant);

impl fmt::Display for Literal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Constant::Undefined => f.write_str("undefined"),
            Constant::Null => f.write_str("null"),
            Constant::Bool(value) => write!(f, "{value}"),
            // `{:?}` rather than `{}` so that a whole number prints as `1.0` and not `1`. The
            // dump is read by people deciding whether a value is a double, and JavaScript
            // having only one number type is exactly what makes that worth being explicit
            // about.
            Constant::Number(value) => write!(f, "{value:?}"),
            Constant::String(value) => write!(f, "{value:?}"),
        }
    }
}

struct CompareName(crate::CompareOp);

impl fmt::Display for CompareName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::CompareOp as C;
        f.write_str(match self.0 {
            C::StrictEqual => "eq",
            C::StrictNotEqual => "ne",
            C::Less => "lt",
            C::LessEqual => "le",
            C::Greater => "gt",
            C::GreaterEqual => "ge",
        })
    }
}

/// So `Type` prints the same way in a dump as in an error.
const _: fn() = || {
    fn assert_display<T: fmt::Display>() {}
    assert_display::<Type>();
};
