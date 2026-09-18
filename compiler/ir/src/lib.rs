//! SSA-ish typed IR with explicit safepoints.
//!
//! ROADMAP §M11. The IR is the shape everything after it is written against: M12's runtime
//! calls into it, M13's codegen lowers it, and M13's GC integration depends on the safepoints
//! being in it rather than inferred afterwards — §M11 says so outright.

#![doc(html_root_url = "https://docs.rs/crisol-ir/0.0.0")]

mod display;
mod function;
mod types;
mod verify;

pub use function::{
    BinaryOp, Block, BlockId, CompareOp, Constant, Function, FunctionId, Instruction, Op,
    Safepoint, Terminator, UnaryOp, ValueId,
};
pub use types::Type;
pub use verify::{VerifyError, verify, verify_module};
