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

/// Where a live value sits in a frame at one safepoint.
///
/// Cranelift **spills every live value to the frame** before a safepoint, which is the detail
/// that makes precise collection tractable here: the collector needs to read stack slots and
/// nothing else. Go's collector needed register maps as well, but only once it began preempting
/// goroutines *mid-function* — safepoints at calls and allocations stay in the simpler regime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafepointMap {
    /// Offset of the safepoint from the start of the function's machine code.
    ///
    /// The collector has a **return address**, so it looks up the instruction *after* the call.
    /// This is that offset.
    pub code_offset: u32,
    /// How many bytes of stack this map covers.
    ///
    /// **Not a frame size, and not the distance from the frame pointer to the stack pointer.**
    /// Cranelift calls this a span and measures every live offset from **SP**, so this cannot
    /// be used to reach a slot from a frame pointer — the collector finds SP from the frame
    /// chain instead. It is carried because a map covering no bytes describes nothing, which
    /// is worth being able to see.
    pub span: u32,
    /// Byte offsets within the frame holding live values.
    pub live_offsets: Vec<u32>,
}

/// What compiling one function produced.
///
/// `stack_map_entries` exists because §M13's deliverable is *"stack map emission at
/// safepoints"*, and a test that only checks the function compiled does not verify emission at
/// all. Counting the entries makes the deliverable measurable rather than asserted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Every safepoint in the function, with what is live at it.
    ///
    /// This is the table a precise collector needs. Extracting it is the whole reason D-87's
    /// note — "`cranelift-object` does not write stack maps into a section" — is a statement
    /// about the *writer* rather than about the information being unavailable: the compiled
    /// buffer exposes it, and carrying it across is this crate's job.
    pub safepoints: Vec<SafepointMap>,
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
    (BinaryOp::InstanceOf, "crisol_instanceof"),
    (BinaryOp::LooseEqual, "crisol_loose_equal"),
    (BinaryOp::LooseNotEqual, "crisol_loose_not_equal"),
    (BinaryOp::In, "crisol_in"),
];

/// The runtime symbols an object operation calls.
///
/// Separate from [`HELPER_SYMBOLS`] because these do not share the two-in-one-out shape of a
/// binary operator: allocation takes nothing, a store takes four words and returns nothing.
const CREATE_OBJECT_SYMBOL: &str = "crisol_create_object";
const PROPERTY_STORE_SYMBOL: &str = "crisol_property_store";
const PROPERTY_LOAD_SYMBOL: &str = "crisol_property_load";
const CLOSURE_CAPTURE_SYMBOL: &str = "crisol_closure_capture";
const CREATE_CLOSURE_SYMBOL: &str = "crisol_create_closure";
const SET_CAPTURE_SYMBOL: &str = "crisol_closure_set_capture";
const CLOSURE_CODE_SYMBOL: &str = "crisol_closure_code";
const CONSTRUCT_THIS_SYMBOL: &str = "crisol_construct_this";
const CONSTRUCT_RESULT_SYMBOL: &str = "crisol_construct_result";
const CREATE_ARRAY_SYMBOL: &str = "crisol_create_array";
const COMPUTED_LOAD_SYMBOL: &str = "crisol_computed_load";
const COMPUTED_STORE_SYMBOL: &str = "crisol_computed_store";
const STRICT_EQUAL_SYMBOL: &str = "crisol_strict_equal";
const THROW_SYMBOL: &str = "crisol_throw";
const PENDING_EXCEPTION_SYMBOL: &str = "crisol_pending_exception";
const CREATE_STRING_SYMBOL: &str = "crisol_create_string";
const TRUTHY_SYMBOL: &str = "crisol_truthy";
const GLOBAL_LOAD_SYMBOL: &str = "crisol_global_load";
const DELETE_SYMBOL: &str = "crisol_delete";
const ENUMERATE_SYMBOL: &str = "crisol_enumerate";
const ITERATE_SYMBOL: &str = "crisol_iterate";
const CREATE_REGEXP_SYMBOL: &str = "crisol_create_regexp";
const ARRAY_EXTEND_SYMBOL: &str = "crisol_array_extend";

/// The runtime symbol each unary operator calls when its operand's type is not known.
///
/// A table rather than fields, because these all share one shape — one value in, one out — and
/// the backend picks by operator exactly as it does for the binary helpers.
const UNARY_SYMBOLS: &[(crisol_ir::UnaryOp, &str)] = &[
    (crisol_ir::UnaryOp::Negate, "crisol_negate"),
    (crisol_ir::UnaryOp::ToNumber, "crisol_to_number"),
    (crisol_ir::UnaryOp::Not, "crisol_not"),
    (crisol_ir::UnaryOp::TypeOf, "crisol_typeof"),
];

/// The symbol holding the addresses of the program's compiled functions.
pub const FUNCTION_TABLE_SYMBOL: &str = "crisol_functions";

/// The runtime entry points objects and closures need, as declared in a module.
#[derive(Clone, Debug)]
struct ObjectHelpers<T> {
    /// `crisol_create_object() -> value`
    create: T,
    /// `crisol_property_store(object, key, length, value)`
    store: T,
    /// `crisol_property_load(object, key, length) -> value`
    load: T,
    /// `crisol_closure_capture(closure, index) -> value`
    capture: T,
    /// `crisol_create_closure(function, captures) -> closure`
    create_closure: T,
    /// `crisol_closure_set_capture(closure, index, value)`
    set_capture: T,
    /// `crisol_closure_code(closure) -> address`
    code: T,
    /// `crisol_construct_this(callee) -> object`
    construct_this: T,
    /// `crisol_construct_result(this, returned) -> value`
    construct_result: T,
    /// `crisol_create_array(length) -> array`
    create_array: T,
    /// `crisol_computed_load(object, key) -> value`
    computed_load: T,
    /// `crisol_computed_store(object, key, value)`
    computed_store: T,
    /// `crisol_strict_equal(left, right) -> boolean`
    strict_equal: T,
    /// `crisol_throw(value) -> exception signal`
    throw: T,
    /// `crisol_pending_exception() -> value`
    pending: T,
    /// `crisol_create_string(text, length) -> string`
    create_string: T,
    /// One per unary operator that needs a runtime coercion.
    unary: Vec<(crisol_ir::UnaryOp, T)>,
    /// `crisol_truthy(value) -> boolean`
    truthy: T,
    /// `crisol_global_load(name, length) -> value`
    global_load: T,
    /// `crisol_delete(object, key) -> boolean`
    delete: T,
    /// `crisol_enumerate(object) -> array of names`
    enumerate: T,
    /// `crisol_iterate(value) -> something indexable`
    iterate: T,
    /// `crisol_create_regexp(source, source_len, flags, flags_len) -> object`
    create_regexp: T,
    /// `crisol_array_extend(array, value, spread) -> exception or undefined`
    array_extend: T,
}

/// Declares the object helpers as imports in `module`.
///
/// Shared by both backends rather than written twice: the two lists drifting apart would mean
/// a program that compiles to an object file and one that runs in-process disagree about what
/// the runtime provides, and only one of them would be tested.
fn declare_object_helpers<M: cranelift_module::Module>(
    module: &mut M,
) -> Result<ObjectHelpers<cranelift_module::FuncId>, CodegenError> {
    let pointer = module.target_config().pointer_type();

    let mut create = module.make_signature();
    create.returns.push(AbiParam::new(types::I64));

    let mut store = module.make_signature();
    store.params.push(AbiParam::new(types::I64));
    store.params.push(AbiParam::new(pointer));
    store.params.push(AbiParam::new(types::I64));
    store.params.push(AbiParam::new(types::I64));
    store.returns.push(AbiParam::new(types::I64));

    let mut load = module.make_signature();
    load.params.push(AbiParam::new(types::I64));
    load.params.push(AbiParam::new(pointer));
    load.params.push(AbiParam::new(types::I64));
    load.returns.push(AbiParam::new(types::I64));

    let mut capture = module.make_signature();
    capture.params.push(AbiParam::new(types::I64));
    capture.params.push(AbiParam::new(types::I64));
    capture.returns.push(AbiParam::new(types::I64));

    let mut create_closure = module.make_signature();
    create_closure.params.push(AbiParam::new(types::I64));
    create_closure.params.push(AbiParam::new(types::I64));
    create_closure.returns.push(AbiParam::new(types::I64));

    let mut set_capture = module.make_signature();
    set_capture.params.push(AbiParam::new(types::I64));
    set_capture.params.push(AbiParam::new(types::I64));
    set_capture.params.push(AbiParam::new(types::I64));

    let mut code = module.make_signature();
    code.params.push(AbiParam::new(types::I64));
    code.returns.push(AbiParam::new(pointer));

    let mut construct_this = module.make_signature();
    construct_this.params.push(AbiParam::new(types::I64));
    construct_this.returns.push(AbiParam::new(types::I64));

    let mut construct_result = module.make_signature();
    construct_result.params.push(AbiParam::new(types::I64));
    construct_result.params.push(AbiParam::new(types::I64));
    construct_result.returns.push(AbiParam::new(types::I64));

    let mut create_array = module.make_signature();
    create_array.params.push(AbiParam::new(types::I64));
    create_array.returns.push(AbiParam::new(types::I64));

    let mut computed_load = module.make_signature();
    computed_load.params.push(AbiParam::new(types::I64));
    computed_load.params.push(AbiParam::new(types::I64));
    computed_load.returns.push(AbiParam::new(types::I64));

    let mut computed_store = module.make_signature();
    computed_store.params.push(AbiParam::new(types::I64));
    computed_store.params.push(AbiParam::new(types::I64));
    computed_store.params.push(AbiParam::new(types::I64));
    computed_store.returns.push(AbiParam::new(types::I64));

    let mut strict_equal = module.make_signature();
    strict_equal.params.push(AbiParam::new(types::I64));
    strict_equal.params.push(AbiParam::new(types::I64));
    strict_equal.returns.push(AbiParam::new(types::I64));

    let mut throw = module.make_signature();
    throw.params.push(AbiParam::new(types::I64));
    throw.returns.push(AbiParam::new(types::I64));

    let mut pending = module.make_signature();
    pending.returns.push(AbiParam::new(types::I64));

    let mut create_string = module.make_signature();
    create_string.params.push(AbiParam::new(pointer));
    create_string.params.push(AbiParam::new(types::I64));
    create_string.returns.push(AbiParam::new(types::I64));

    let mut truthy = module.make_signature();
    truthy.params.push(AbiParam::new(types::I64));
    truthy.returns.push(AbiParam::new(types::I64));

    let mut global_load = module.make_signature();
    global_load.params.push(AbiParam::new(pointer));
    global_load.params.push(AbiParam::new(types::I64));
    global_load.returns.push(AbiParam::new(types::I64));

    let mut delete = module.make_signature();
    delete.params.push(AbiParam::new(types::I64));
    delete.params.push(AbiParam::new(types::I64));
    delete.returns.push(AbiParam::new(types::I64));

    let mut enumerate = module.make_signature();
    enumerate.params.push(AbiParam::new(types::I64));
    enumerate.returns.push(AbiParam::new(types::I64));

    let mut iterate = module.make_signature();
    iterate.params.push(AbiParam::new(types::I64));
    iterate.returns.push(AbiParam::new(types::I64));

    let mut array_extend = module.make_signature();
    array_extend.params.push(AbiParam::new(types::I64));
    array_extend.params.push(AbiParam::new(types::I64));
    array_extend.params.push(AbiParam::new(types::I64));
    array_extend.returns.push(AbiParam::new(types::I64));

    let mut create_regexp = module.make_signature();
    create_regexp.params.push(AbiParam::new(pointer));
    create_regexp.params.push(AbiParam::new(types::I64));
    create_regexp.params.push(AbiParam::new(pointer));
    create_regexp.params.push(AbiParam::new(types::I64));
    create_regexp.returns.push(AbiParam::new(types::I64));

    let mut unary_signature = module.make_signature();
    unary_signature.params.push(AbiParam::new(types::I64));
    unary_signature.returns.push(AbiParam::new(types::I64));
    let mut unary = Vec::with_capacity(UNARY_SYMBOLS.len());
    for (op, symbol) in UNARY_SYMBOLS {
        unary.push((
            *op,
            module
                .declare_function(symbol, Linkage::Import, &unary_signature)
                .map_err(|error| CodegenError::Backend {
                    message: error.to_string(),
                })?,
        ));
    }

    let mut declare = |symbol: &str, signature: &cranelift_codegen::ir::Signature| {
        module
            .declare_function(symbol, Linkage::Import, signature)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })
    };
    Ok(ObjectHelpers {
        create: declare(CREATE_OBJECT_SYMBOL, &create)?,
        store: declare(PROPERTY_STORE_SYMBOL, &store)?,
        load: declare(PROPERTY_LOAD_SYMBOL, &load)?,
        capture: declare(CLOSURE_CAPTURE_SYMBOL, &capture)?,
        create_closure: declare(CREATE_CLOSURE_SYMBOL, &create_closure)?,
        set_capture: declare(SET_CAPTURE_SYMBOL, &set_capture)?,
        code: declare(CLOSURE_CODE_SYMBOL, &code)?,
        construct_this: declare(CONSTRUCT_THIS_SYMBOL, &construct_this)?,
        construct_result: declare(CONSTRUCT_RESULT_SYMBOL, &construct_result)?,
        create_array: declare(CREATE_ARRAY_SYMBOL, &create_array)?,
        computed_load: declare(COMPUTED_LOAD_SYMBOL, &computed_load)?,
        computed_store: declare(COMPUTED_STORE_SYMBOL, &computed_store)?,
        strict_equal: declare(STRICT_EQUAL_SYMBOL, &strict_equal)?,
        throw: declare(THROW_SYMBOL, &throw)?,
        pending: declare(PENDING_EXCEPTION_SYMBOL, &pending)?,
        create_string: declare(CREATE_STRING_SYMBOL, &create_string)?,
        truthy: declare(TRUTHY_SYMBOL, &truthy)?,
        global_load: declare(GLOBAL_LOAD_SYMBOL, &global_load)?,
        delete: declare(DELETE_SYMBOL, &delete)?,
        enumerate: declare(ENUMERATE_SYMBOL, &enumerate)?,
        iterate: declare(ITERATE_SYMBOL, &iterate)?,
        create_regexp: declare(CREATE_REGEXP_SYMBOL, &create_regexp)?,
        array_extend: declare(ARRAY_EXTEND_SYMBOL, &array_extend)?,
        unary,
    })
}

/// Appends the five operands every compiled function takes.
///
/// One function rather than written out at each backend, because the two must agree exactly:
/// a mismatch would be a call frame read with the wrong layout, which is not a crash but a
/// wrong value.
fn push_fixed_params(signature: &mut cranelift_codegen::ir::Signature, pointer: types::Type) {
    signature.params.push(AbiParam::new(types::I64)); // closure
    signature.params.push(AbiParam::new(types::I64)); // this
    signature.params.push(AbiParam::new(types::I64)); // new.target
    signature.params.push(AbiParam::new(types::I64)); // argc
    signature.params.push(AbiParam::new(pointer)); // argv
    debug_assert_eq!(signature.params.len(), FIXED_PARAMS);
}

/// The linker symbol for a function.
///
/// Derived from the id, not the name. Source names are neither unique nor valid identifiers:
/// two `function (x) {...}` expressions are both "anonymous", a method is "C.method", and a
/// compiler temporary is " tmp0". Naming symbols after them made a program with two anonymous
/// functions fail to compile with a duplicate-symbol error — and a program with two functions
/// of the *same* source name would have been worse, because one would silently win.
///
/// Function zero keeps the name the C entry point calls. The rest carry their source name only
/// as a suffix, so a disassembly is still readable.
fn symbol_name(function: &Function) -> String {
    if function.id.index() == 0 {
        return function.name.clone();
    }
    let readable: String = function
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("crisol_fn{}_{readable}", function.id.index())
}

/// Every piece of text a function names, in the order first seen.
///
/// Property keys **and** string literals, because both become bytes in the object's data
/// section and are reached the same way. Collected before lowering because defining data needs
/// the module, which the lowering deliberately does not hold.
fn keys_of(function: &Function) -> Vec<String> {
    let mut keys = Vec::new();
    for block in &function.blocks {
        for instruction in &block.instructions {
            let key = match &instruction.op {
                Op::PropertyLoad { key, .. }
                | Op::PropertyStore { key, .. }
                | Op::GlobalLoad { name: key } => key.as_str(),
                Op::Const(Constant::String(text)) => text.as_str(),
                // A pattern and its flags are interned the same way, so the data section holds
                // one copy of each however often the literal appears.
                Op::CreateRegExp { source, flags } => {
                    for text in [source, flags] {
                        if !keys.iter().any(|seen: &String| seen == text) {
                            keys.push(text.clone());
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            if !keys.iter().any(|seen: &String| seen == key) {
                keys.push(key.to_owned());
            }
        }
    }
    keys
}

/// Emits a constant for every key `function` names that has not been emitted already.
///
/// The keys are `Local`, so two modules can each define their own `"length"` without the
/// linker having to pick one. They are not NUL-terminated: the runtime takes a pointer *and* a
/// length, which is what lets a key contain a NUL at all — `obj["a\0b"]` is a legal property
/// name, and a C string could not express it.
fn intern_keys<M: cranelift_module::Module>(
    module: &mut M,
    interned: &mut HashMap<String, cranelift_module::DataId>,
    function: &Function,
) -> Result<(), CodegenError> {
    for text in keys_of(function) {
        if interned.contains_key(&text) {
            continue;
        }
        let mut description = cranelift_module::DataDescription::new();
        description.define(text.as_bytes().to_vec().into_boxed_slice());
        // Numbered rather than named after the key: a property name is any UTF-16 string, and
        // most of them are not legal symbol names.
        let symbol = format!("crisol_key_{}", interned.len());
        let id = module
            .declare_data(&symbol, Linkage::Local, false, false)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        module
            .define_data(id, &description)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        interned.insert(text, id);
    }
    Ok(())
}

/// How many operands every compiled function takes before its JavaScript arguments.
///
/// `(closure, this, new.target, argc, argv)`. **Every** function takes exactly these, whatever
/// its source arity, so a call site never has to know which function it is reaching — which it
/// cannot know for a callback passed to `arr.map`. A convention with the arity baked into the
/// signature cannot express a first-class function at all.
///
/// These five land in registers on all four targets simply by being the first parameters; that
/// is the platform's own convention doing the work, not a choice made here.
const FIXED_PARAMS: usize = 5;

/// The smallest `argv` a caller may pass, in slots.
///
/// Public because the C entry point has to honour it, and it lives in another crate — a second
/// copy of the number is a second thing to keep in step.
///
/// One, and it matters. A parameter is read as `argv[min(i, argc - 1 ... )]` guarded by
/// `i < argc`, and the guard is a *select* rather than a branch — so the load happens either
/// way and has to be in bounds even when the answer is discarded. Clamping the index to zero
/// makes it in bounds for every arity, provided there is always a slot zero to read.
///
/// So a call passing no arguments still reserves one slot. That costs eight bytes of stack and
/// removes a branch from every parameter of every function.
pub const ARGV_MIN_SLOTS: usize = 1;

/// Whether a value of this type could be a reference the collector must trace.
///
/// The slots are declared unconditionally because nothing there knows what they hold. Here the
/// IR does, so a number or a boolean is left out — and that is a real narrowing rather than a
/// guess: these are the types the lattice states, not an inference about bit patterns.
///
/// `Unknown` counts, because it means exactly that.
const fn may_hold_a_reference(ty: crisol_ir::Type) -> bool {
    match ty {
        crisol_ir::Type::Never
        | crisol_ir::Type::Undefined
        | crisol_ir::Type::Null
        | crisol_ir::Type::Bool
        | crisol_ir::Type::Number => false,
        crisol_ir::Type::String | crisol_ir::Type::Object(_) | crisol_ir::Type::Unknown => true,
    }
}

/// A borrowed piece of interned text, so the two lookups read the same.
struct TextRef<'a>(&'a str);

/// What both emitted tables are aligned to.
///
/// They are arrays of 8-byte values — addresses and counts — and the runtime reads them as
/// such. Eight rather than the natural alignment of a row so the count at element zero is
/// aligned too.
const TABLE_ALIGN: u64 = 8;

/// The symbol holding the stack map table a compiled program's collector reads.
pub const STACK_MAP_SYMBOL: &str = "crisol_stack_maps";

/// Reads the safepoint tables out of a compiled function.
///
/// Read after compilation rather than from the IR, because the offsets only exist once
/// registers have been allocated and live values spilled — the IR knows *which* values are
/// live, and only the machine code knows *where*.
fn read_safepoints(context: &cranelift_codegen::Context) -> Vec<SafepointMap> {
    let Some(compiled) = context.compiled_code() else {
        return Vec::new();
    };
    compiled
        .buffer
        .user_stack_maps()
        .iter()
        .map(|(code_offset, span, map)| SafepointMap {
            code_offset: *code_offset,
            span: *span,
            live_offsets: map.entries().map(|(_, offset)| offset).collect(),
        })
        .collect()
}

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
    /// One row per live value at one safepoint, accumulated until [`Backend::finish`] can
    /// write them out with relocations.
    rows: Vec<(cranelift_module::FuncId, u32, u32)>,
    module: ObjectModule,
    context: FunctionBuilderContext,
    /// The runtime helpers, one per operator that cannot be a native instruction.
    ///
    /// One symbol each rather than a single `crisol_binary(op, a, b)`: an opcode passed at
    /// runtime would be a branch the linker cannot see through, and a separate symbol is what
    /// lets a later pass replace an individual operator without touching the others.
    helpers: HashMap<BinaryOp, cranelift_module::FuncId>,
    /// The object helpers, declared once for the module.
    objects: ObjectHelpers<cranelift_module::FuncId>,
    /// Property keys already emitted as data, so a key used twice is one constant.
    keys: HashMap<String, cranelift_module::DataId>,
    /// Each compiled function against the `FunctionId` it answers to.
    ///
    /// Kept by id rather than by compile order: `finish` writes them into a table the runtime
    /// indexes with the id a closure carries, so the two have to agree on more than sequence.
    functions: Vec<(u32, cranelift_module::FuncId)>,
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
        // Frame pointers, because the collector walks native frames to find return addresses
        // and there is no other portable way to do it. Off by default — a frame pointer costs
        // a register, which matters for code that is never unwound and does not matter at all
        // for code that must be.
        flags
            .set("preserve_frame_pointers", "true")
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
        let objects = declare_object_helpers(&mut module)?;

        Ok(Self {
            triple: triple.to_owned(),
            rows: Vec::new(),
            module,
            context: FunctionBuilderContext::new(),
            helpers,
            objects,
            keys: HashMap::new(),
            functions: Vec::new(),
        })
    }
}

impl Cranelift {
    /// Writes the stack map table into the object as a data symbol.
    ///
    /// **One row per live value per safepoint**, flat, rather than a nested structure with
    /// variable-length lists. A flat table costs a few bytes and needs no length-prefix parsing
    /// in the runtime — and the runtime reading it will be walking a stack at a moment when the
    /// heap is mid-collection, which is the worst possible place for a parser bug.
    ///
    /// The function address in each row is a **relocation**: the linker fills it in, because
    /// nothing here knows where the code will land. That is the same job Go's linker does for
    /// `pclntab`.
    ///
    /// Little-endian is written explicitly rather than using native byte order: the object is
    /// for the *target*, which need not be the host. All four targets are little-endian today,
    /// so this cannot currently be observed — which is exactly why it would be a difficult bug
    /// to find later.
    /// Writes the addresses of the compiled functions into the object as a data symbol.
    ///
    /// Indexed by `FunctionId`, because that is what a closure carries. A gap — an id nothing
    /// compiled — stays zero rather than being left out, so the index still lines up; the
    /// runtime turns a zero entry into `crisol_not_a_function`, which makes a hole a defined
    /// outcome rather than a jump to address zero.
    ///
    /// Same layout as the stack map table: element zero is the count, the rows follow.
    fn emit_function_table(&mut self) -> Result<(), CodegenError> {
        let width = self
            .functions
            .iter()
            .map(|(index, _)| *index as usize + 1)
            .max()
            .unwrap_or(0);

        let mut description = cranelift_module::DataDescription::new();
        description.set_align(TABLE_ALIGN);
        let mut bytes = Vec::with_capacity(8 + width * 8);
        bytes.extend_from_slice(&(width as u64).to_le_bytes());
        bytes.extend_from_slice(&vec![0u8; width * 8]);
        description.define(bytes.into_boxed_slice());

        for (index, function) in &self.functions {
            let at = 8 + *index as usize * 8;
            let reference = self
                .module
                .declare_func_in_data(*function, &mut description);
            description.write_function_addr(u32::try_from(at).unwrap_or(u32::MAX), reference);
        }

        let id = self
            .module
            .declare_data(FUNCTION_TABLE_SYMBOL, Linkage::Export, false, false)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        self.module
            .define_data(id, &description)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        Ok(())
    }

    fn emit_stack_map_table(&mut self) -> Result<(), CodegenError> {
        let mut description = cranelift_module::DataDescription::new();
        // **Without this the symbol may land at an odd address.** Cranelift defaults a data
        // object to no declared alignment, and both tables are read as arrays of 8-byte
        // values — so the runtime's `from_raw_parts` asserts on a misaligned pointer, even for
        // an empty table. It happened to land aligned for every program until one did not,
        // which is the worst way for this to be found.
        description.set_align(TABLE_ALIGN);
        let count = u64::try_from(self.rows.len()).unwrap_or(0);
        let mut bytes = Vec::with_capacity(8 + self.rows.len() * 16);
        bytes.extend_from_slice(&count.to_le_bytes());

        // The function addresses are written as zeroes and then relocated; the offsets are
        // known now.
        let mut relocations = Vec::with_capacity(self.rows.len());
        for (index, (function, code_offset, frame_offset)) in self.rows.iter().enumerate() {
            let at = 8 + index * 16;
            bytes.extend_from_slice(&0u64.to_le_bytes());
            bytes.extend_from_slice(&code_offset.to_le_bytes());
            bytes.extend_from_slice(&frame_offset.to_le_bytes());
            relocations.push((u32::try_from(at).unwrap_or(u32::MAX), *function));
        }

        description.define(bytes.into_boxed_slice());
        for (at, function) in relocations {
            let reference = self.module.declare_func_in_data(function, &mut description);
            description.write_function_addr(at, reference);
        }

        let id = self
            .module
            .declare_data(STACK_MAP_SYMBOL, Linkage::Export, false, false)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        self.module
            .define_data(id, &description)
            .map_err(|error| CodegenError::Backend {
                message: error.to_string(),
            })?;
        Ok(())
    }
}

impl Backend for Cranelift {
    fn compile(&mut self, function: &Function) -> Result<Report, CodegenError> {
        let mut signature = self.module.make_signature();
        push_fixed_params(&mut signature, self.module.target_config().pointer_type());
        signature.returns.push(AbiParam::new(types::I64));

        let id = self
            .module
            .declare_function(&symbol_name(function), Linkage::Export, &signature)
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
        intern_keys(&mut self.module, &mut self.keys, function)?;
        let interned: Vec<(String, cranelift_module::DataId)> = keys_of(function)
            .into_iter()
            .map(|text| {
                let id = self.keys[&text];
                (text, id)
            })
            .collect();
        let keys: HashMap<String, cranelift_codegen::ir::GlobalValue> = interned
            .into_iter()
            .map(|(text, id)| {
                (
                    text,
                    self.module.declare_data_in_func(id, &mut context.func),
                )
            })
            .collect();
        let objects = ObjectHelpers {
            create: self
                .module
                .declare_func_in_func(self.objects.create, &mut context.func),
            store: self
                .module
                .declare_func_in_func(self.objects.store, &mut context.func),
            load: self
                .module
                .declare_func_in_func(self.objects.load, &mut context.func),
            capture: self
                .module
                .declare_func_in_func(self.objects.capture, &mut context.func),
            create_closure: self
                .module
                .declare_func_in_func(self.objects.create_closure, &mut context.func),
            set_capture: self
                .module
                .declare_func_in_func(self.objects.set_capture, &mut context.func),
            code: self
                .module
                .declare_func_in_func(self.objects.code, &mut context.func),
            construct_this: self
                .module
                .declare_func_in_func(self.objects.construct_this, &mut context.func),
            construct_result: self
                .module
                .declare_func_in_func(self.objects.construct_result, &mut context.func),
            create_array: self
                .module
                .declare_func_in_func(self.objects.create_array, &mut context.func),
            computed_load: self
                .module
                .declare_func_in_func(self.objects.computed_load, &mut context.func),
            computed_store: self
                .module
                .declare_func_in_func(self.objects.computed_store, &mut context.func),
            strict_equal: self
                .module
                .declare_func_in_func(self.objects.strict_equal, &mut context.func),
            throw: self
                .module
                .declare_func_in_func(self.objects.throw, &mut context.func),
            pending: self
                .module
                .declare_func_in_func(self.objects.pending, &mut context.func),
            create_string: self
                .module
                .declare_func_in_func(self.objects.create_string, &mut context.func),
            truthy: self
                .module
                .declare_func_in_func(self.objects.truthy, &mut context.func),
            global_load: self
                .module
                .declare_func_in_func(self.objects.global_load, &mut context.func),
            delete: self
                .module
                .declare_func_in_func(self.objects.delete, &mut context.func),
            enumerate: self
                .module
                .declare_func_in_func(self.objects.enumerate, &mut context.func),
            iterate: self
                .module
                .declare_func_in_func(self.objects.iterate, &mut context.func),
            create_regexp: self
                .module
                .declare_func_in_func(self.objects.create_regexp, &mut context.func),
            array_extend: self
                .module
                .declare_func_in_func(self.objects.array_extend, &mut context.func),
            unary: self
                .objects
                .unary
                .iter()
                .map(|(op, id)| {
                    (
                        *op,
                        self.module.declare_func_in_func(*id, &mut context.func),
                    )
                })
                .collect(),
        };
        let pointer = frontend_config.pointer_type();
        // Every indirect call goes through this one signature. That it is the *same* signature
        // every compiled function is defined with is the whole point of the convention — the
        // two are built by `push_fixed_params` so they cannot drift apart.
        let mut uniform = self.module.make_signature();
        push_fixed_params(&mut uniform, pointer);
        uniform.returns.push(AbiParam::new(types::I64));
        let uniform = context.func.import_signature(uniform);
        let builder = FunctionBuilder::new(&mut context.func, &mut self.context);
        let mut lowering = Lowering {
            builder,
            slots: HashMap::new(),
            values: HashMap::new(),
            types: HashMap::new(),
            blocks: HashMap::new(),
            helpers,
            objects,
            keys,
            pointer,
            uniform,
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
        self.functions.push((function.id.index(), id));
        let safepoints = read_safepoints(&context);
        for map in &safepoints {
            for offset in &map.live_offsets {
                self.rows.push((id, map.code_offset, *offset));
            }
        }
        Ok(Report {
            safepoints,
            stack_map_entries,
        })
    }

    fn finish(mut self) -> Result<Vec<u8>, CodegenError> {
        self.emit_stack_map_table()?;
        self.emit_function_table()?;
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
    /// The object helpers, resolved into this function.
    objects: ObjectHelpers<cranelift_codegen::ir::FuncRef>,
    /// Each property key this function names, as the address of its constant.
    keys: HashMap<String, cranelift_codegen::ir::GlobalValue>,
    /// The target's pointer type, for materialising those addresses.
    pointer: cranelift_codegen::ir::Type,
    /// The signature every indirect call goes through.
    uniform: cranelift_codegen::ir::SigRef,
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

        // The prologue gets a block of its own, ahead of the IR's entry block.
        //
        // It cannot share one. Unpacking arguments emits real instructions, and the loop below
        // walks the IR's blocks in index order — which need not start at the entry — so it
        // would switch away from a half-filled block. Cranelift rejects that, and it is right
        // to: the instructions would be stranded in a block nothing branches to.
        let prologue = self.builder.create_block();
        self.builder
            .append_block_params_for_function_params(prologue);
        self.builder.switch_to_block(prologue);

        // The five fixed operands arrive as block parameters; everything else is unpacked.
        let incoming: Vec<ClifValue> = self.builder.block_params(prologue).to_vec();
        let closure = incoming[0];
        let argc = incoming[3];
        let argv = incoming[4];
        // `this` goes to the slot the function says holds it. An arrow has none: it captures
        // the enclosing `this` instead, so the value arrives as a capture below and the one
        // passed here is ignored.
        if let Some(slot) = function.this_slot {
            let variable = self.variable(slot);
            self.builder.def_var(variable, incoming[1]);
        }
        // `incoming[2]` is `new.target`, which nothing reads until classes.

        let undefined = self
            .builder
            .ins()
            .iconst(types::I64, crisol_value::Value::UNDEFINED.to_bits() as i64);

        // A parameter the caller did not pass is `undefined` — `f(1)` on `function f(a, b)`
        // binds `b` to `undefined`, it is not an error. So each one is read under a guard.
        //
        // The guard is a pair of selects rather than a branch: the index is clamped to zero so
        // the load is in bounds whatever the arity, the load always happens, and its result is
        // then discarded if the parameter was never passed. `ARGV_MIN_SLOTS` is what makes the
        // clamped load safe.
        for (index, slot) in function.parameters.iter().enumerate() {
            let position = i64::try_from(index).unwrap_or(i64::MAX);
            let position = self.builder.ins().iconst(types::I64, position);
            let passed = self.builder.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
                position,
                argc,
            );
            let zero = self.builder.ins().iconst(types::I64, 0);
            let safe = self.builder.ins().select(passed, position, zero);
            let byte_offset = self.builder.ins().imul_imm_u(safe, 8);
            let address = self.builder.ins().iadd(argv, byte_offset);
            let loaded = self
                .builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), address, 0);
            let value = self.builder.ins().select(passed, loaded, undefined);
            let variable = self.variable(*slot);
            self.builder.def_var(variable, value);
        }

        // Captures come out of the closure, not off the argument list. They are positional and
        // the pairing with `Op::Closure`'s `captures` is by index, which `verify_module` checks
        // — it is the one rule that cannot be checked from a single function.
        for (index, slot) in function.captures.iter().enumerate() {
            let position = i64::try_from(index).unwrap_or(i64::MAX);
            let position = self.builder.ins().iconst(types::I64, position);
            let call = self
                .builder
                .ins()
                .call(self.objects.capture, &[closure, position]);
            let value = self.builder.inst_results(call)[0];
            let variable = self.variable(*slot);
            self.builder.def_var(variable, value);
        }

        let entry = self.block(function.entry.index());
        self.builder.ins().jump(entry, &[]);

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
        // **Every slot is declared as needing a stack map**, because any of them may hold a
        // reference and nothing here knows which. That sounds conservative and is not: a
        // NaN-boxed value carries its own tag (D-53), so the collector reads each slot and
        // asks what it is. A number in a slot is reported and then ignored — precisely, not
        // heuristically, which is the difference from scanning the stack for things that look
        // like pointers (ROADMAP §2.1).
        //
        // The cost is that every slot is spilled at every safepoint. Narrowing that needs the
        // IR to carry which slots can hold references, which is M20's.
        self.builder.declare_var_needs_stack_map(variable);
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
            Op::Const(Constant::String(text)) => {
                // A call rather than a constant: a string is a heap cell, so it has to be
                // allocated. The bytes are already in the data section — the same constant a
                // property key of the same text would use, interned once.
                let (pointer, length) = self.text_operands(text)?;
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.create_string, &[pointer, length]);
                Some(self.builder.inst_results(call)[0])
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
                    crisol_ir::UnaryOp::Throw => {
                        let call = self.builder.ins().call(self.objects.throw, &[value]);
                        Some(self.builder.inst_results(call)[0])
                    }
                    // A plain bit comparison, and correct because the signal is a singleton —
                    // none of `===`'s difficulties apply, since it is not a number and there
                    // is exactly one of it.
                    crisol_ir::UnaryOp::IsException => {
                        let sentinel = self
                            .builder
                            .ins()
                            .iconst(types::I64, crisol_value::Value::EXCEPTION.to_bits() as i64);
                        let is = self.builder.ins().icmp(
                            cranelift_codegen::ir::condcodes::IntCC::Equal,
                            value,
                            sentinel,
                        );
                        Some(self.box_condition(is))
                    }
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
                    // Everything else is a call. The coercing forms of the above need
                    // `ToNumber` or `ToBoolean`, and `typeof` produces a string — none of
                    // which is an instruction. `~` still has none, because it needs modular
                    // `ToInt32` (D-87).
                    other => {
                        let Some((_, helper)) =
                            self.objects.unary.iter().find(|(op, _)| *op == *other)
                        else {
                            return Err(CodegenError::Unsupported {
                                operation: format!("unary operator {}", other.symbol()),
                            });
                        };
                        let call = self.builder.ins().call(*helper, &[value]);
                        Some(self.builder.inst_results(call)[0])
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
                    // On values of unknown type this is a call, for D-53's reasons: `NaN` has
                    // identical bits to itself and is not equal to itself, and `+0` and `-0`
                    // have different bits and are. A bit comparison gets both wrong.
                    CompareOp::StrictEqual | CompareOp::StrictNotEqual => {
                        let call = self
                            .builder
                            .ins()
                            .call(self.objects.strict_equal, &[left, right]);
                        let equal = self.builder.inst_results(call)[0];
                        let boxed_true = self
                            .builder
                            .ins()
                            .iconst(types::I64, crisol_value::Value::TRUE.to_bits() as i64);
                        let condition = if matches!(op, CompareOp::StrictEqual) {
                            cranelift_codegen::ir::condcodes::IntCC::Equal
                        } else {
                            cranelift_codegen::ir::condcodes::IntCC::NotEqual
                        };
                        self.builder.ins().icmp(condition, equal, boxed_true)
                    }
                };
                Some(self.box_condition(condition))
            }
            Op::Closure { function, captures } => {
                // The captures are written one at a time rather than passed to the allocation.
                // A variadic C call would need the backend and the runtime to agree on how the
                // arguments were laid out, and those two only meet at link time — where a
                // disagreement is silent.
                let count = i64::try_from(captures.len()).unwrap_or(i64::MAX);
                let index = self
                    .builder
                    .ins()
                    .iconst(types::I64, i64::from(function.index()));
                let count_value = self.builder.ins().iconst(types::I64, count);
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.create_closure, &[index, count_value]);
                let closure = self.builder.inst_results(call)[0];

                for (position, capture) in captures.iter().enumerate() {
                    let value = self.value(*capture);
                    let position = i64::try_from(position).unwrap_or(i64::MAX);
                    let position = self.builder.ins().iconst(types::I64, position);
                    self.builder
                        .ins()
                        .call(self.objects.set_capture, &[closure, position, value]);
                }
                Some(closure)
            }
            Op::Call {
                callee,
                this_value,
                args,
            } => {
                let callee = self.value(*callee);
                let this_value = self.value(*this_value);

                let (argv, argc) = self.build_arguments(args);
                // `new.target` is `undefined` for an ordinary call — that is what distinguishes
                // `f()` from `new f()` inside the callee.
                let undefined = self.undefined();
                Some(self.call_through(callee, this_value, undefined, argc, argv))
            }
            Op::Construct { callee, args } => {
                let callee = self.value(*callee);
                // The receiver is allocated from `callee.prototype` before the constructor
                // runs, which is `OrdinaryCreateFromConstructor` — establishing the prototype
                // link here rather than in a separate step that could be omitted.
                let created = self
                    .builder
                    .ins()
                    .call(self.objects.construct_this, &[callee]);
                let this_value = self.builder.inst_results(created)[0];

                let (argv, argc) = self.build_arguments(args);
                // `new.target` is the constructor being invoked, which is what makes `new f()`
                // distinguishable from `f()` inside the body.
                let returned = self.call_through(callee, this_value, callee, argc, argv);

                // **A constructor returning an object replaces `this`; one returning a
                // primitive does not.** The runtime decides, so the rule lives in one place
                // rather than at every `new`.
                let result = self
                    .builder
                    .ins()
                    .call(self.objects.construct_result, &[this_value, returned]);
                Some(self.builder.inst_results(result)[0])
            }
            Op::CreateArray { elements } => {
                // Allocated at its final length and then filled, so the array never exists in
                // a half-built state across a safepoint — each element's evaluation can
                // allocate, and a shorter array would be a live object the collector traces
                // with fewer elements than the program thinks it has.
                let length = i64::try_from(elements.len()).unwrap_or(i64::MAX);
                let length = self.builder.ins().iconst(types::I64, length);
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.create_array, &[length]);
                let array = self.builder.inst_results(call)[0];

                for (position, element) in elements.iter().enumerate() {
                    let value = self.value(*element);
                    let position = i64::try_from(position).unwrap_or(i64::MAX);
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "an index this large cannot be reached"
                    )]
                    let index = crisol_value::Value::number(position as f64).to_bits();
                    let index = self.builder.ins().iconst(types::I64, index as i64);
                    // The result is the exception signal or `undefined`; an element store on a
                    // freshly made array cannot throw, so it is discarded here rather than
                    // checked at every element.
                    self.builder
                        .ins()
                        .call(self.objects.computed_store, &[array, index, value]);
                }
                Some(array)
            }
            Op::CreateRegExp { source, flags } => {
                // Both halves are interned like any other constant text, so a pattern used
                // twice is stored once.
                let (source_pointer, source_length) = self.text_operands(source)?;
                let (flags_pointer, flags_length) = self.text_operands(flags)?;
                let call = self.builder.ins().call(
                    self.objects.create_regexp,
                    &[source_pointer, source_length, flags_pointer, flags_length],
                );
                Some(self.builder.inst_results(call)[0])
            }
            Op::ArrayExtend {
                array,
                value,
                spread,
            } => {
                let array = self.value(*array);
                let value = self.value(*value);
                let spread = self.builder.ins().iconst(types::I64, i64::from(*spread));
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.array_extend, &[array, value, spread]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::Iterate { object } => {
                let object = self.value(*object);
                let call = self.builder.ins().call(self.objects.iterate, &[object]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::Enumerate { object } => {
                let object = self.value(*object);
                let call = self.builder.ins().call(self.objects.enumerate, &[object]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::Delete { object, key } => {
                let object = self.value(*object);
                let key = self.value(*key);
                let call = self.builder.ins().call(self.objects.delete, &[object, key]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::ComputedLoad { object, key } => {
                let object = self.value(*object);
                let key = self.value(*key);
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.computed_load, &[object, key]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::ComputedStore { object, key, value } => {
                let object = self.value(*object);
                let key = self.value(*key);
                let value = self.value(*value);
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.computed_store, &[object, key, value]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::GlobalLoad { name } => {
                let (pointer, length) = self.key_operands(name)?;
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.global_load, &[pointer, length]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::CaughtValue => {
                let call = self.builder.ins().call(self.objects.pending, &[]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::CreateObject { .. } => {
                // No shape argument: an object literal is empty until its first property is
                // stored, and the lowering says so by allocating at the root shape and then
                // emitting a `PropertyStore` per property (D-92). The `shape` the IR carries
                // is the root of a throwaway table, so passing it would mean nothing.
                let call = self.builder.ins().call(self.objects.create, &[]);
                Some(self.builder.inst_results(call)[0])
            }
            Op::PropertyStore { object, key, value } => {
                let object = self.value(*object);
                let value = self.value(*value);
                let (pointer, length) = self.key_operands(key)?;
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.store, &[object, pointer, length, value]);
                // A store answers with the exception signal or `undefined`, because writing a
                // property of `null` throws. The frontend checks it like any call.
                Some(self.builder.inst_results(call)[0])
            }
            Op::PropertyLoad { object, key } => {
                let object = self.value(*object);
                let (pointer, length) = self.key_operands(key)?;
                let call = self
                    .builder
                    .ins()
                    .call(self.objects.load, &[object, pointer, length]);
                Some(self.builder.inst_results(call)[0])
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
            // A value that may hold a reference has to be in the stack map too, not only the
            // slots. `[{v: 1}]` never stores the object in a slot — it is an SSA value used
            // directly as an element — so with only slots declared the collector could not see
            // it, and the allocation of the *array* freed it. Under stress every array of
            // objects came back with stale elements; without stress nothing failed at all.
            if may_hold_a_reference(instruction.ty) {
                self.builder.declare_value_needs_stack_map(result);
            }
            self.values.insert(id.index(), result);
            self.types.insert(id.index(), instruction.ty);
        }
        Ok(())
    }

    /// Boxed `undefined`, which several lowerings need.
    fn undefined(&mut self) -> ClifValue {
        self.builder
            .ins()
            .iconst(types::I64, crisol_value::Value::UNDEFINED.to_bits() as i64)
    }

    /// Lays out `args` for a call, returning where they are and how many there are.
    ///
    /// They go in a slot of this function's own frame. Not a heap list: that would allocate on
    /// the hottest path in the language, and the collector already reaches frame slots through
    /// the stack maps (D-94).
    ///
    /// Shared by `Op::Call` and `Op::Construct` rather than written twice. The two must agree
    /// with the *callee's* prologue about the layout, and three copies of one contract is two
    /// too many.
    fn build_arguments(&mut self, args: &[crisol_ir::ValueId]) -> (ClifValue, ClifValue) {
        let slots = args.len().max(ARGV_MIN_SLOTS);
        let size = u32::try_from(slots * 8).unwrap_or(u32::MAX);
        let argv_slot =
            self.builder
                .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    size,
                    3,
                ));
        for (position, argument) in args.iter().enumerate() {
            let value = self.value(*argument);
            let offset = i32::try_from(position * 8).unwrap_or(i32::MAX);
            self.builder
                .ins()
                .stack_store(self.pointer, value, argv_slot, offset);
        }
        // Any slot past the last argument holds `undefined`. The callee never *uses* what it
        // reads there — its guard discards it — but it does read it, and leaving stack garbage
        // where a value belongs is how a later change that does trust it becomes very hard to
        // debug.
        let undefined = self.undefined();
        for position in args.len()..slots {
            let offset = i32::try_from(position * 8).unwrap_or(i32::MAX);
            self.builder
                .ins()
                .stack_store(self.pointer, undefined, argv_slot, offset);
        }
        let argv = self.builder.ins().stack_addr(self.pointer, argv_slot, 0);
        let argc = i64::try_from(args.len()).unwrap_or(i64::MAX);
        let argc = self.builder.ins().iconst(types::I64, argc);
        (argv, argc)
    }

    /// Calls whatever `callee` names, through the uniform convention.
    fn call_through(
        &mut self,
        callee: ClifValue,
        this_value: ClifValue,
        new_target: ClifValue,
        argc: ClifValue,
        argv: ClifValue,
    ) -> ClifValue {
        let code = self.builder.ins().call(self.objects.code, &[callee]);
        let code = self.builder.inst_results(code)[0];
        let call = self.builder.ins().call_indirect(
            self.uniform,
            code,
            &[callee, this_value, new_target, argc, argv],
        );
        self.builder.inst_results(call)[0]
    }

    /// A property key as the pair the runtime takes: where the constant is, and how long.
    ///
    /// The length is a separate operand rather than a NUL terminator because a property name
    /// may contain a NUL — `obj["a\0b"]` is legal — and scanning for one would truncate it.
    fn key_operands(
        &mut self,
        key: &crisol_value::PropertyKey,
    ) -> Result<(ClifValue, ClifValue), CodegenError> {
        self.text_operands(key.as_str())
    }

    /// Where a piece of interned text is, and how long.
    fn text_operands(&mut self, text: &str) -> Result<(ClifValue, ClifValue), CodegenError> {
        let key = TextRef(text);
        let Some(global) = self.keys.get(key.0).copied() else {
            // Unreachable unless `keys_of` and the lowering disagree about which operations
            // name a key, which is exactly the kind of drift worth failing loudly on.
            return Err(CodegenError::Backend {
                message: format!("the text {:?} was never interned", key.0),
            });
        };
        let address = self.builder.ins().symbol_value(self.pointer, global);
        let length = i64::try_from(key.0.len()).unwrap_or(i64::MAX);
        let length = self.builder.ins().iconst(types::I64, length);
        Ok((address, length))
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
                let known_boolean = self.is_boolean(*condition);
                let condition = self.value(*condition);
                // A bit comparison against boxed `true` is right **only** when the IR has
                // proved this is a boolean. On anything else every truthy value that is not
                // literally `true` — a non-empty string, a number, an object — would take the
                // false branch, so `if (name)` and `x || y` are a `ToBoolean` call.
                let condition = if known_boolean {
                    condition
                } else {
                    let call = self.builder.ins().call(self.objects.truthy, &[condition]);
                    self.builder.inst_results(call)[0]
                };
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
    objects: ObjectHelpers<cranelift_module::FuncId>,
    keys: HashMap<String, cranelift_module::DataId>,
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
        let objects = declare_object_helpers(&mut module)?;

        Ok(Self {
            module,
            context: FunctionBuilderContext::new(),
            helpers,
            objects,
            keys: HashMap::new(),
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
        push_fixed_params(&mut signature, self.module.target_config().pointer_type());
        signature.returns.push(AbiParam::new(types::I64));

        let id = self
            .module
            .declare_function(&symbol_name(function), Linkage::Export, &signature)
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
        intern_keys(&mut self.module, &mut self.keys, function)?;
        let interned: Vec<(String, cranelift_module::DataId)> = keys_of(function)
            .into_iter()
            .map(|text| {
                let id = self.keys[&text];
                (text, id)
            })
            .collect();
        let keys: HashMap<String, cranelift_codegen::ir::GlobalValue> = interned
            .into_iter()
            .map(|(text, id)| {
                (
                    text,
                    self.module.declare_data_in_func(id, &mut context.func),
                )
            })
            .collect();
        let objects = ObjectHelpers {
            create: self
                .module
                .declare_func_in_func(self.objects.create, &mut context.func),
            store: self
                .module
                .declare_func_in_func(self.objects.store, &mut context.func),
            load: self
                .module
                .declare_func_in_func(self.objects.load, &mut context.func),
            capture: self
                .module
                .declare_func_in_func(self.objects.capture, &mut context.func),
            create_closure: self
                .module
                .declare_func_in_func(self.objects.create_closure, &mut context.func),
            set_capture: self
                .module
                .declare_func_in_func(self.objects.set_capture, &mut context.func),
            code: self
                .module
                .declare_func_in_func(self.objects.code, &mut context.func),
            construct_this: self
                .module
                .declare_func_in_func(self.objects.construct_this, &mut context.func),
            construct_result: self
                .module
                .declare_func_in_func(self.objects.construct_result, &mut context.func),
            create_array: self
                .module
                .declare_func_in_func(self.objects.create_array, &mut context.func),
            computed_load: self
                .module
                .declare_func_in_func(self.objects.computed_load, &mut context.func),
            computed_store: self
                .module
                .declare_func_in_func(self.objects.computed_store, &mut context.func),
            strict_equal: self
                .module
                .declare_func_in_func(self.objects.strict_equal, &mut context.func),
            throw: self
                .module
                .declare_func_in_func(self.objects.throw, &mut context.func),
            pending: self
                .module
                .declare_func_in_func(self.objects.pending, &mut context.func),
            create_string: self
                .module
                .declare_func_in_func(self.objects.create_string, &mut context.func),
            truthy: self
                .module
                .declare_func_in_func(self.objects.truthy, &mut context.func),
            global_load: self
                .module
                .declare_func_in_func(self.objects.global_load, &mut context.func),
            delete: self
                .module
                .declare_func_in_func(self.objects.delete, &mut context.func),
            enumerate: self
                .module
                .declare_func_in_func(self.objects.enumerate, &mut context.func),
            iterate: self
                .module
                .declare_func_in_func(self.objects.iterate, &mut context.func),
            create_regexp: self
                .module
                .declare_func_in_func(self.objects.create_regexp, &mut context.func),
            array_extend: self
                .module
                .declare_func_in_func(self.objects.array_extend, &mut context.func),
            unary: self
                .objects
                .unary
                .iter()
                .map(|(op, id)| {
                    (
                        *op,
                        self.module.declare_func_in_func(*id, &mut context.func),
                    )
                })
                .collect(),
        };
        let pointer = frontend_config.pointer_type();
        // Every indirect call goes through this one signature. That it is the *same* signature
        // every compiled function is defined with is the whole point of the convention — the
        // two are built by `push_fixed_params` so they cannot drift apart.
        let mut uniform = self.module.make_signature();
        push_fixed_params(&mut uniform, pointer);
        uniform.returns.push(AbiParam::new(types::I64));
        let uniform = context.func.import_signature(uniform);
        let builder = FunctionBuilder::new(&mut context.func, &mut self.context);
        let mut lowering = Lowering {
            builder,
            slots: HashMap::new(),
            values: HashMap::new(),
            types: HashMap::new(),
            blocks: HashMap::new(),
            helpers,
            objects,
            keys,
            pointer,
            uniform,
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
        let safepoints = read_safepoints(&context);
        Ok(Report {
            safepoints,
            stack_map_entries,
        })
    }

    /// The address of a compiled function, if it was compiled.
    #[must_use]
    pub fn address(&self, name: &str) -> Option<*const u8> {
        self.compiled.get(name).copied()
    }
}
