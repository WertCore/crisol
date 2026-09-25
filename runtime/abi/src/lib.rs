//! The C ABI that compiled code calls into.
//!
//! §M13's backend emits calls to these symbols for every operator it cannot express as a
//! native instruction (D-86). Until they exist, an object file references undefined symbols and
//! nothing links — which is why this crate is the last thing between "compiles" and "runs".
//!
//! # The shape of the boundary
//!
//! Every function takes and returns `u64`: a JavaScript value NaN-boxed into 64 bits (D-53).
//! There is no wrapper type at the boundary because the callee is machine code with no notion
//! of Rust types, and a `#[repr(transparent)]` newtype would be a comment rather than a
//! guarantee. [`crisol_value::Value::from_bits`] is the only thing that gives the bits meaning.
//!
//! # `ToInt32` is the reason the bitwise operators are here
//!
//! The specification's `ToInt32` truncates toward zero and then wraps **modulo 2³²**.
//! Cranelift's float-to-int conversion **saturates**, so `1e10 | 0` lowered as a machine
//! instruction would clamp to `i32::MAX` where JavaScript gives `1410065408`. That is a wrong
//! number that looks entirely plausible, which is why these are calls and not instructions.

#![doc(html_root_url = "https://docs.rs/crisol-abi/0.0.0")]

use std::cell::RefCell;

use crisol_gc::{GcRef, Heap};
use crisol_value::{PropertyKey, Shapes, Value};

/// Every symbol this crate provides to generated code.
///
/// The backend declares imports by name and this crate defines them by name, and **nothing
/// connects the two until link time** — a typo on either side is silent through every compiler
/// test and fails when someone tries to produce a binary. So the list lives here, on the side
/// that defines them, and `crisol-codegen`'s tests assert that every symbol it emits appears
/// in it.
pub const SYMBOLS: &[&str] = &[
    "crisol_add",
    "crisol_subtract",
    "crisol_multiply",
    "crisol_divide",
    "crisol_remainder",
    "crisol_exponent",
    "crisol_bit_and",
    "crisol_bit_or",
    "crisol_bit_xor",
    "crisol_shift_left",
    "crisol_shift_right",
    "crisol_unsigned_shift_right",
    "crisol_instanceof",
    "crisol_create_object",
    "crisol_property_store",
    "crisol_define_accessor",
    "crisol_property_load",
    "crisol_closure_capture",
    "crisol_create_closure",
    "crisol_closure_set_capture",
    "crisol_closure_code",
    "crisol_not_a_function",
    "crisol_construct_this",
    "crisol_construct_code",
    "crisol_construct_result",
    "crisol_create_array",
    "crisol_computed_load",
    "crisol_computed_store",
    "crisol_strict_equal",
    "crisol_throw",
    "crisol_pending_exception",
    "crisol_create_string",
    "crisol_negate",
    "crisol_to_number",
    "crisol_not",
    "crisol_typeof",
    "crisol_report_uncaught",
    "crisol_truthy",
    "crisol_global_load",
    "crisol_delete",
    "crisol_enumerate",
    "crisol_iterate",
    "crisol_create_regexp",
    "crisol_loose_equal",
    "crisol_loose_not_equal",
    "crisol_in",
    "crisol_array_extend",
    "crisol_create_arguments",
    "crisol_global_load_optional",
    "crisol_relational",
];

/// `ToNumber` for a value that is already a number, and `NaN` otherwise.
///
/// The arithmetic helpers below are reached only for operands the compiler could **not** prove
/// numeric, so each one has to do the coercion itself. Anything that is not a number yields
/// `NaN` for now: strings need the runtime's string table, and objects need `ToPrimitive`,
/// which calls user code. Both are recorded rather than approximated — returning `0` for a
/// string would make `"5" * 2` evaluate to `0` instead of `10`.
fn to_number(bits: u64) -> f64 {
    let value = Value::from_bits(bits);
    match value.kind() {
        crisol_value::Kind::Number => value.as_number().unwrap_or(f64::NAN),
        // **An empty or all-whitespace string is `0`, not `NaN`** — `+"" === 0` — which is the
        // one case a plain `parse` gets wrong, because Rust rejects an empty string.
        crisol_value::Kind::String => text_of(bits).map_or(f64::NAN, |text| {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                0.0
            } else {
                trimmed.parse().unwrap_or(f64::NAN)
            }
        }),
        crisol_value::Kind::Boolean => {
            if value.as_boolean().unwrap_or(false) {
                1.0
            } else {
                0.0
            }
        }
        // `null` is `0` and `undefined` is `NaN`, which is the asymmetry behind `null >= 0`
        // being true while `null == 0` is false.
        crisol_value::Kind::Null => 0.0,
        // An object needs `ToPrimitive`, which calls user code.
        _ => f64::NAN,
    }
}

fn from_number(number: f64) -> u64 {
    Value::number(number).to_bits()
}

/// The specification's `ToInt32`.
///
/// Not a cast. `NaN`, the infinities and `±0` all give `0`; everything else truncates toward
/// zero and wraps modulo 2³², so `2³¹` comes back as `-2³¹` and `1e10` as `1410065408`.
///
/// A saturating conversion — which is what the hardware offers — gets every one of those
/// wrong in a way that still produces a number.
#[must_use]
pub fn to_int32(number: f64) -> i32 {
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let truncated = number.trunc();
    // `rem_euclid` on the 2³² modulus, done in `f64` because the value may be far outside
    // `i64`'s range before reduction — casting first is exactly the saturation this avoids.
    let wrapped = truncated.rem_euclid(4_294_967_296.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rem_euclid put this in 0..2^32, which is exactly u32's range"
    )]
    let unsigned = wrapped as u32;
    #[expect(
        clippy::cast_possible_wrap,
        reason = "the wrap to a signed 32-bit value is the specified behaviour"
    )]
    let signed = unsigned as i32;
    signed
}

/// The specification's `ToUint32`, which differs from [`to_int32`] only in the final step.
#[must_use]
pub fn to_uint32(number: f64) -> u32 {
    #[expect(
        clippy::cast_sign_loss,
        reason = "reinterpreting the same 32 bits is the specified behaviour"
    )]
    let unsigned = to_int32(number) as u32;
    unsigned
}

/// `left + right`.
///
/// # Safety
///
/// Called from generated machine code with two NaN-boxed values. There is nothing unsafe about
/// the body; the `extern "C"` surface is what the backend emits calls to.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_add(left: u64, right: u64) -> u64 {
    // **`+` concatenates when either operand is a string**, and adds otherwise. The test is on
    // the operands rather than on both being numbers, because `1 + "2"` is `"12"` and not `3`.
    //
    // An object operand still gives `NaN`: `ToPrimitive` calls user code, and guessing at it
    // would turn `{} + ""` into something that reads plausibly and is wrong.
    let is_string = |bits: u64| Value::from_bits(bits).kind() == crisol_value::Kind::String;
    if is_string(left) || is_string(right) {
        return match (to_text(left), to_text(right)) {
            (Some(a), Some(b)) => new_string(&(a + &b)),
            _ => from_number(f64::NAN),
        };
    }
    from_number(to_number(left) + to_number(right))
}

/// `left - right`, for callers that could not prove both operands numeric.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_subtract(left: u64, right: u64) -> u64 {
    from_number(to_number(left) - to_number(right))
}

/// `left * right`.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_multiply(left: u64, right: u64) -> u64 {
    from_number(to_number(left) * to_number(right))
}

/// `left / right`.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_divide(left: u64, right: u64) -> u64 {
    from_number(to_number(left) / to_number(right))
}

/// `left % right`.
///
/// **The result takes the sign of the dividend**, not the divisor — `-5 % 3` is `-2`, where a
/// mathematical modulo would give `1`. Rust's `%` on `f64` is `fmod` and agrees, which is why
/// this is a one-liner and not a correction.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_remainder(left: u64, right: u64) -> u64 {
    from_number(to_number(left) % to_number(right))
}

/// `left ** right`.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_exponent(left: u64, right: u64) -> u64 {
    from_number(to_number(left).powf(to_number(right)))
}

/// `left & right`, on int32.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_bit_and(left: u64, right: u64) -> u64 {
    from_number(f64::from(
        to_int32(to_number(left)) & to_int32(to_number(right)),
    ))
}

/// `left | right`, on int32.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_bit_or(left: u64, right: u64) -> u64 {
    from_number(f64::from(
        to_int32(to_number(left)) | to_int32(to_number(right)),
    ))
}

/// `left ^ right`, on int32.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_bit_xor(left: u64, right: u64) -> u64 {
    from_number(f64::from(
        to_int32(to_number(left)) ^ to_int32(to_number(right)),
    ))
}

/// `left << right`.
///
/// **The shift count is masked to five bits**, so `1 << 32` is `1` and not `0`. Rust's `<<`
/// panics on an over-wide shift in debug builds, so the mask is doing real work rather than
/// matching what the hardware happens to do.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_shift_left(left: u64, right: u64) -> u64 {
    let count = to_uint32(to_number(right)) & 31;
    from_number(f64::from(to_int32(to_number(left)) << count))
}

/// `left >> right`, sign-propagating.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_shift_right(left: u64, right: u64) -> u64 {
    let count = to_uint32(to_number(right)) & 31;
    from_number(f64::from(to_int32(to_number(left)) >> count))
}

/// `left >>> right`, zero-filling.
///
/// The only shift whose result is read as **unsigned**, so `-1 >>> 0` is `4294967295` rather
/// than `-1`. That is also the reason it cannot be folded in with the other two.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_unsigned_shift_right(left: u64, right: u64) -> u64 {
    let count = to_uint32(to_number(right)) & 31;
    from_number(f64::from(to_uint32(to_number(left)) >> count))
}

/// Prints a value the way a program's result should appear.
///
/// Exists so the C entry point a compiled program links against does not have to understand
/// NaN boxing. Decoding 64 bits into a JavaScript value is this crate's job, and duplicating
/// the tag layout in generated C would be a second place for it to drift.
///
/// # Safety
///
/// Called from the generated entry point with a NaN-boxed value.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_print(bits: u64) {
    let value = Value::from_bits(bits);
    // Deliberately not `Display`: a compiled program's output is a product surface, and
    // `Debug` would print Rust's idea of the value rather than JavaScript's.
    match value.kind() {
        crisol_value::Kind::Undefined => println!("undefined"),
        crisol_value::Kind::Null => println!("null"),
        crisol_value::Kind::Boolean => println!("{}", value.as_boolean().unwrap_or(false)),
        crisol_value::Kind::Number => {
            println!("{}", number_text(value.as_number().unwrap_or(f64::NAN)));
        }
        crisol_value::Kind::String => match text_of(bits) {
            Some(text) => println!("{text}"),
            None => println!("[unreadable string]"),
        },
        crisol_value::Kind::Symbol | crisol_value::Kind::Object => {
            // Reaching into the heap needs a runtime this crate does not have. Saying so beats
            // printing a pointer that looks like a number.
            println!("[unprintable: the heap is not wired up yet]");
        }
    }
}

/// One row of a compiled program's stack map table.
///
/// Laid out to match exactly what the backend emits (D-90): a function address the linker
/// filled in, the offset of a safepoint within that function, and the frame offset of one live
/// value. `#[repr(C)]` because the producer is a code generator, not rustc — a Rust layout
/// would be free to reorder these and the two sides would disagree silently.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StackMapRow {
    /// Where the function starts, once linked.
    pub function: *const u8,
    /// Offset of the safepoint within it.
    pub code_offset: u32,
    /// Offset within the frame of one live value.
    pub frame_offset: u32,
}

/// The table a compiled program registers before it runs.
///
/// **The program hands its table to the runtime rather than the runtime looking up a symbol.**
/// An `extern "C" { static crisol_stack_maps }` here would make this crate fail to link
/// anywhere the symbol does not exist — including its own test binary, which has no compiled
/// program in it. Passing the address keeps the dependency pointing the way it actually runs.
pub struct StackMaps {
    rows: &'static [StackMapRow],
}

impl StackMaps {
    /// The rows, in the order the compiler emitted them.
    #[must_use]
    pub const fn rows(&self) -> &'static [StackMapRow] {
        self.rows
    }

    /// Every frame offset live at `return_address`, if it is a safepoint.
    #[must_use]
    pub fn live_at(&self, return_address: *const u8) -> Vec<u32> {
        live_at(self.rows, return_address)
    }
}

/// Every frame offset live at `return_address`, given a table.
///
/// The lookup is an **exact match** on `function + code_offset`, because a return address
/// points at the instruction after a call and that is where the safepoint sits. An address
/// matching nothing is not an error: it is a frame that was not at a safepoint, which is every
/// frame except those at a call into the runtime.
///
/// A free function taking the rows, rather than only a method on the registered table, so it
/// can be tested against a hand-built table — a lookup reachable only through a process-global
/// registered by generated code is a lookup nothing can check.
#[must_use]
pub fn live_at(rows: &[StackMapRow], return_address: *const u8) -> Vec<u32> {
    rows.iter()
        .filter(|row| {
            // `wrapping_add` rather than `add`: the function pointer comes from a linker, and
            // arithmetic on it must not be undefined behaviour if the table is malformed.
            row.function.wrapping_add(row.code_offset as usize) == return_address
        })
        .map(|row| row.frame_offset)
        .collect()
}

/// The registered table, if a compiled program has registered one.
static mut STACK_MAPS: Option<StackMaps> = None;

/// Registers a compiled program's stack map table.
///
/// Called once from the program's entry point before anything else runs.
///
/// # Safety
///
/// `table` must point at `count` rows emitted by this toolchain's backend, valid for the life
/// of the process — which is true of a symbol in the program's own data section and of nothing
/// else.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crisol_register_stack_maps(table: *const StackMapRow, count: u64) {
    let len = usize::try_from(count).unwrap_or(0);
    // A zero-row table is registered, not skipped. Returning early here would make "the
    // program never registered its table" and "the program has no safepoints" indistinguishable
    // — and the first is a bug that frees live values while the second is ordinary.
    if table.is_null() {
        return;
    }
    assert!(
        table.is_aligned(),
        "the stack map table is misaligned at {table:p}; the backend must align it (D-98)"
    );
    // SAFETY: the caller promises `table` points at `count` valid rows living as long as the
    // process, which is what a data symbol in the program does. A length of zero is fine:
    // `from_raw_parts` accepts it for a non-null, aligned pointer.
    let rows = unsafe { std::slice::from_raw_parts(table, len) };
    // SAFETY: called once, from the entry point, before any other thread exists.
    unsafe {
        STACK_MAPS = Some(StackMaps { rows });
    }

    // Registration is otherwise invisible: a program that never registered its table runs
    // exactly the same until the first collection, at which point it frees live values. This
    // makes the step observable so a test can check it happened rather than infer it from the
    // program producing the right answer.
    if std::env::var_os("CRISOL_DEBUG_STACK_MAPS").is_some() {
        println!("crisol: registered {len} stack map rows");
    }
}

/// The registered table.
#[must_use]
pub fn stack_maps() -> Option<&'static StackMaps> {
    // SAFETY: only written once by `crisol_register_stack_maps` before the program runs.
    unsafe { (*std::ptr::addr_of!(STACK_MAPS)).as_ref() }
}

/// One native frame: where it returns to, and where its locals live.
///
/// The pairing is the part worth getting right. A frame pointer `fp` holds the **caller's**
/// frame pointer at `[fp]` and the return address **into the caller** at `[fp + 8]`. So a
/// return address and the frame its live values sit in come from *different* links of the
/// chain — reading offsets from the wrong one yields whatever happened to be at that spot,
/// which is a plausible-looking reference pointing at nothing.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// Return address into the function that owns [`Frame::base`].
    pub return_address: *const u8,
    /// **Stack pointer** of that function at the call, which is what a stack map offset is
    /// measured from.
    ///
    /// Not its frame pointer. Cranelift documents a stack map entry as *"the offset from SP"*
    /// — `SP + 0x42` holds the reference — and a frame pointer is at the other end of the
    /// frame. Using one for the other reads whatever sits that far the wrong way from the
    /// wrong end, which is a plausible-looking reference pointing at nothing.
    pub base: *const usize,
}

/// Walks native frames from the caller outwards.
///
/// Relies on the frame-pointer chain, which the backend enables explicitly
/// (`preserve_frame_pointers`). On both aarch64 and x86-64 a frame laid out that way holds the
/// caller's frame pointer at `[fp]` and the return address at `[fp + 8]`.
///
/// **It stops at the first frame that does not look like one.** A null, unaligned, or
/// non-increasing frame pointer ends the walk rather than being followed: the chain leaves
/// compiled code eventually — into the C entry point, then into libc — and following a pointer
/// out of a frame built by something else is how a stack walker reads unmapped memory.
///
/// # Safety
///
/// Must be called with the program stopped at a safepoint. Walking a stack that is being
/// modified reads frames mid-construction.
#[must_use]
pub unsafe fn walk_frames(limit: usize) -> Vec<Frame> {
    let mut found = Vec::new();
    let mut frame: *const usize = current_frame_pointer();

    for _ in 0..limit {
        if frame.is_null() || !frame.is_aligned() {
            break;
        }
        // SAFETY: `frame` is non-null and aligned and points at a frame built by
        // frame-pointer-preserving code, so `[fp]` is the caller's frame pointer and
        // `[fp + 8]` the return address. The loop stops as soon as either stops looking like
        // one.
        let (caller, return_address) = unsafe { (*frame as *const usize, *frame.add(1)) };
        if return_address == 0 || caller.is_null() || caller <= frame {
            // Stacks grow downwards, so a caller's frame is always at a higher address. A
            // chain that does not move outwards is not a chain.
            break;
        }
        found.push(Frame {
            return_address: return_address as *const u8,
            // The caller's stack pointer *at the call*, not its frame pointer.
            //
            // Both aarch64 and x86-64 enter a function with the return address and the saved
            // frame pointer at the top of the callee's frame — `stp x29, x30, [sp, #-16]!`
            // and `call` + `push rbp`. So this frame's `fp` points at those two words, and
            // immediately above them is where the caller's stack pointer stood when it made
            // the call. That is the origin every stack map offset is measured from.
            base: frame.wrapping_add(2),
        });
        frame = caller;
    }
    found
}

/// Reads the frame pointer register.
#[must_use]
fn current_frame_pointer() -> *const usize {
    #[cfg(target_arch = "aarch64")]
    {
        let fp: *const usize;
        // SAFETY: reads a register; no memory is touched.
        unsafe {
            std::arch::asm!("mov {}, x29", out(reg) fp, options(nomem, nostack, preserves_flags));
        }
        fp
    }
    #[cfg(target_arch = "x86_64")]
    {
        let fp: *const usize;
        // SAFETY: reads a register; no memory is touched.
        unsafe {
            std::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack, preserves_flags));
        }
        fp
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        // Walking without a known frame layout would be guessing. Returning null yields no
        // roots, which makes collection refuse rather than collect wrongly.
        std::ptr::null()
    }
}

/// Every live value the compiled frames on this stack are holding.
///
/// This is what the collector needs and could not previously obtain: the roots that exist only
/// in machine code. A frame whose return address matches no safepoint contributes nothing,
/// which is the normal case for every frame except those at a call into the runtime.
///
/// # Safety
///
/// Must be called with the program stopped at a safepoint, for the reason in [`walk_frames`].
#[must_use]
pub unsafe fn compiled_roots(limit: usize) -> Vec<Value> {
    let Some(maps) = stack_maps() else {
        return Vec::new();
    };
    // SAFETY: the caller promises the program is stopped at a safepoint.
    let frames = unsafe { walk_frames(limit) };
    let mut roots = Vec::new();
    for frame in frames {
        for offset in maps.live_at(frame.return_address) {
            let slot = frame.base.wrapping_byte_add(offset as usize);
            if slot.is_null() || !slot.is_aligned() {
                continue;
            }
            // SAFETY: `offset` came from a stack map the compiler emitted for this exact
            // return address, so it names a slot inside this frame. The alignment check above
            // rejects a malformed table rather than dereferencing whatever it named.
            let bits = unsafe { *slot } as u64;
            roots.push(Value::from_bits(bits));
        }
    }
    roots
}

/// How many frames a root scan walks before giving up.
///
/// A bound rather than "until the stack ends" because the walk follows frame pointers, and a
/// corrupt one turns an unbounded loop into a hang inside the collector — the single worst
/// place to hang, since nothing has run yet that could report why.
pub const FRAME_LIMIT: usize = 1024;

/// Teaches `heap` to find the roots that live only in compiled frames.
///
/// Until this is called a collection sees just the shadow stack, so every value held by
/// compiled code looks like garbage. Call it once, before running any compiled code.
///
/// Values that are not heap references — a number in a spilled slot, say — are reported by the
/// walk and dropped here: a NaN-boxed value carries its own tag, so this is precise rather
/// than a guess about which bit patterns look like pointers (D-53).
///
/// # Safety
///
/// The caller promises collections happen only at safepoints, since that is when the stack
/// maps describe the frames truthfully. Allocation-triggered collection satisfies this — an
/// allocation site *is* a safepoint — but a collection forced from arbitrary code does not.
pub unsafe fn install_compiled_roots(heap: &Heap) {
    heap.set_extra_roots(Box::new(|| {
        // SAFETY: the caller of `install_compiled_roots` promised collections happen only at
        // safepoints, and this closure runs only from a collection.
        let mut roots: Vec<GcRef> = unsafe { compiled_roots(FRAME_LIMIT) }
            .into_iter()
            .filter_map(|value| value.as_address().map(GcRef::from_address))
            .collect();
        // `Array.prototype` is reachable from every array, but nothing holds it while no array
        // exists — and it is built before the first one. It is read from a `Cell` rather than
        // through `with_runtime` because this runs *during* a collection, which may have been
        // triggered inside a borrow of the runtime's shape table.
        roots.extend(ARRAY_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(pending_root());
        roots.extend(GLOBALS.with(std::cell::Cell::get));
        roots.extend(FUNCTION_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(STRING_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(REGEXP_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(DATE_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(OBJECT_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(MAP_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(PROMISE_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(SET_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(SYMBOL_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(ARRAY_ITERATOR_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(NUMBER_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(BOOLEAN_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(ARRAY_BUFFER_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(TYPED_ARRAY_PROTOTYPE.with(std::cell::Cell::get));
        TYPED_ARRAY_PROTOTYPES
            .with(|protos| roots.extend(protos.iter().filter_map(std::cell::Cell::get)));
        // The microtask queue. It is data rather than closures precisely so this walk is
        // possible — a queue of `Box<dyn FnOnce>` hides its captures from the collector, and
        // a settled value reachable only from one would be freed under it. Everything *else*
        // a promise holds lives in its own internal slots, which the heap already traces.
        PROMISE_JOBS.with(|jobs| {
            if let Ok(entries) = jobs.try_borrow() {
                for job in entries.iter() {
                    for held in [job.handler, job.value, job.derived] {
                        roots.extend(Value::from_bits(held).as_address().map(GcRef::from_address));
                    }
                }
            }
        });
        // Every symbol a shape names. `try_borrow` for the same reason as the registry below.
        KEY_SYMBOLS.with(|symbols| {
            if let Ok(entries) = symbols.try_borrow() {
                roots.extend(
                    entries
                        .iter()
                        .filter_map(|value| Value::from_bits(*value).as_address())
                        .map(GcRef::from_address),
                );
            }
        });
        // Every registered symbol. `try_borrow` because this runs *during* a collection, which
        // may have been triggered from inside `Symbol.for` while the registry was borrowed —
        // and failing to root is better than panicking in the collector.
        SYMBOL_REGISTRY.with(|registry| {
            if let Ok(entries) = registry.try_borrow() {
                roots.extend(
                    entries
                        .values()
                        .filter_map(|value| Value::from_bits(*value).as_address())
                        .map(GcRef::from_address),
                );
            }
        });
        roots
    }));
}

thread_local! {
    /// The prototype every array inherits from, once it has been built.
    static ARRAY_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The object holding every global binding.
    static GLOBALS: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every function inherits from, which is where `call` and `apply` live.
    static FUNCTION_PROTOTYPE: std::cell::Cell<Option<GcRef>> =
        const { std::cell::Cell::new(None) };
    /// The prototype every string inherits from.
    static STRING_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every regular expression inherits from.
    static REGEXP_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every date inherits from.
    static DATE_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every object inherits from, at the end of every chain.
    static OBJECT_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every promise inherits from.
    static PROMISE_PROTOTYPE: std::cell::Cell<Option<GcRef>> =
        const { std::cell::Cell::new(None) };
    /// The prototype every `Map` inherits from.
    static MAP_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every `Set` inherits from.
    static SET_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every symbol inherits from.
    static SYMBOL_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every array iterator inherits from.
    static ARRAY_ITERATOR_PROTOTYPE: std::cell::Cell<Option<GcRef>> =
        const { std::cell::Cell::new(None) };
    /// The prototype every number inherits from.
    static NUMBER_PROTOTYPE: std::cell::Cell<Option<GcRef>> = const { std::cell::Cell::new(None) };
    /// The prototype every boolean inherits from.
    static BOOLEAN_PROTOTYPE: std::cell::Cell<Option<GcRef>> =
        const { std::cell::Cell::new(None) };
    /// The prototype every `ArrayBuffer` inherits from.
    static ARRAY_BUFFER_PROTOTYPE: std::cell::Cell<Option<GcRef>> =
        const { std::cell::Cell::new(None) };
    /// `%TypedArray%.prototype` — where every typed array's shared methods and getters live, and
    /// what each per-kind prototype below inherits from.
    static TYPED_ARRAY_PROTOTYPE: std::cell::Cell<Option<GcRef>> =
        const { std::cell::Cell::new(None) };
    /// The nine per-kind prototypes (`Int8Array.prototype`, …), in [`ELEMENT_KINDS`] order. Each
    /// carries its own `BYTES_PER_ELEMENT` and `constructor` and inherits the shared methods from
    /// [`TYPED_ARRAY_PROTOTYPE`].
    static TYPED_ARRAY_PROTOTYPES: [std::cell::Cell<Option<GcRef>>; 9] =
        const { [const { std::cell::Cell::new(None) }; 9] };
    /// The microtask queue. Drained to empty, and jobs queued by jobs run in the same drain,
    /// which is what "microtasks run to completion" means.
    ///
    /// **The only promise state that is not on a promise.** A queue empties by definition, so
    /// it cannot grow the way a table of every promise ever made would. Thread-local because
    /// the drain reaches it while no borrow of the runtime is held.
    static PROMISE_JOBS: RefCell<std::collections::VecDeque<PromiseJob>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
    /// Every symbol that has been used as a property key.
    ///
    /// **Rooted for the life of the program**, which is a leak and the right one. A key lives
    /// in the *shape* table, which outlives any object that holds the property — so a
    /// collected symbol would leave a shape naming an address that no longer means anything,
    /// and `getOwnPropertySymbols` would hand that back as a value. The specification's own
    /// `Symbol.for` registry is permanent for exactly this reason; this is the same bargain
    /// over a smaller set, and the alternative is teaching the collector to trace shapes.
    static KEY_SYMBOLS: RefCell<std::collections::HashSet<u64>> =
        RefCell::new(std::collections::HashSet::new());
    /// `Symbol.for`'s registry, keyed by the string a symbol was registered under.
    ///
    /// Rooted, and that is the specification's design: a registered symbol must come back for
    /// the same key however long later, so it cannot be collected.
    static SYMBOL_REGISTRY: RefCell<std::collections::HashMap<String, u64>> =
        RefCell::new(std::collections::HashMap::new());
    /// Compiled patterns, keyed by their source and flags.
    ///
    /// **A memo, not ownership.** The authoritative `lastIndex` is a property on the JavaScript
    /// object, because a program can read and write it; the compiled pattern here is set from
    /// that property before each use and read back after. Keeping the `JsRegExp` as the owner
    /// of its cursor would mean two copies of a value the program can change, and they would
    /// disagree the first time it did.
    static PATTERNS: RefCell<std::collections::HashMap<(String, String), crisol_builtins::JsRegExp>> =
        RefCell::new(std::collections::HashMap::new());
}

/// A function implemented here rather than compiled, called through the uniform convention.
///
/// The same signature as a compiled function, so a call site cannot tell the difference — which
/// is the point: `[1, 2].map(f)` is an ordinary call whose callee happens to be native.
type Native = extern "C" fn(u64, u64, u64, u64, *const u64) -> u64;

/// Every built-in, in the order their indices name them.
const NATIVES: &[(&str, Native)] = &[
    ("map", array_map),
    ("filter", array_filter),
    ("forEach", array_for_each),
    ("reduce", array_reduce),
    ("push", array_push),
    ("indexOf", array_index_of),
    ("lastIndexOf", array_last_index_of),
    ("includes", array_includes),
    ("join", array_join),
    ("slice", array_slice),
    ("concat", array_concat),
    ("reverse", array_reverse),
    ("pop", array_pop),
    ("shift", array_shift),
    ("unshift", array_unshift),
    ("find", array_find),
    ("findIndex", array_find_index),
    ("every", array_every),
    ("some", array_some),
    ("fill", array_fill),
    ("reduceRight", array_reduce_right),
    ("flat", array_flat),
    ("flatMap", array_flat_map),
    ("at", array_at),
    ("findLast", array_find_last),
    ("findLastIndex", array_find_last_index),
    ("sort", array_sort),
    ("splice", array_splice),
    ("toString", array_to_text),
    ("toLocaleString", array_to_text),
    ("copyWithin", array_copy_within),
    ("toReversed", array_to_reversed),
    ("toSorted", array_to_sorted),
    ("toSpliced", array_to_spliced),
    ("with", array_with),
    ("keys", array_keys),
    ("values", array_values),
    ("entries", array_entries),
];

/// What an array iterator walks: its positions, its elements, or both.
const ITERATOR_KIND: &str = "__kind";
/// What an array iterator walks over.
const ITERATOR_TARGET: &str = "__target";
/// How far an array iterator has got.
const ITERATOR_POSITION: &str = "__position";

/// Builds an iterator over `target`.
///
/// **A real object with a `next`, not a language-level iterator.** `for-of` cannot find it,
/// because finding it means looking up `Symbol.iterator` and a property key cannot be a symbol
/// yet (D-149). What it *can* do is be called directly, which is what
/// `const it = a.values(); it.next()` does and what most of test262's coverage of these
/// methods checks.
fn new_array_iterator(target: u64, kind: f64) -> u64 {
    // **A nullish receiver is a `TypeError`.** `Array.prototype.values.call(undefined)` throws
    // before making anything, which is `RequireObjectCoercible` at the top of each of these —
    // and a program feature-tests exactly this. An iterator over `undefined` was made instead
    // and answered `{done: true}` on the first `next`, which reads as a working empty walk.
    if let Some(thrown) = reject_nullish(target, "cannot iterate") {
        return thrown;
    }
    // **`target` is rooted before anything is allocated.** Every other native roots its
    // receiver through `live_values` before doing work; these three did not, so the array in
    // `[7, 8].values()` — a temporary, held by nothing else — could be freed by the very
    // allocation that made the iterator meant to walk it.
    with_rooted(&[target], || {
        let iterator = crisol_create_object();
        with_rooted(&[iterator, target], || {
            let Some(handle) = handle_of(iterator) else {
                return;
            };
            with_runtime(|runtime| {
                runtime.define_hidden(handle, ITERATOR_TARGET, Value::from_bits(target));
                runtime.define_hidden(handle, ITERATOR_POSITION, Value::number(0.0));
                runtime.define_hidden(handle, ITERATOR_KIND, Value::number(kind));
                if let Some(prototype) = ARRAY_ITERATOR_PROTOTYPE.with(std::cell::Cell::get) {
                    runtime.heap.set_prototype(handle, Some(prototype));
                }
            });
        });
        iterator
    })
}

/// `Array.prototype.keys`.
extern "C" fn array_keys(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    new_array_iterator(this_value, 0.0)
}

/// `Array.prototype.values`.
extern "C" fn array_values(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    new_array_iterator(this_value, 1.0)
}

/// `Array.prototype.entries`.
extern "C" fn array_entries(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    new_array_iterator(this_value, 2.0)
}

/// Methods on the array iterator's prototype.
const ARRAY_ITERATOR_NATIVES: &[(&str, Native)] = &[("next", array_iterator_next)];

/// `next()` on an array iterator.
///
/// **`{value, done}` every time, and `done` stays `true` once reached.** An exhausted iterator
/// answers `{value: undefined, done: true}` for ever rather than restarting, which is what lets
/// a caller loop on `done` without counting.
///
/// The length is re-read on each step, so an array that shrinks mid-iteration ends the walk
/// rather than reading past its end.
extern "C" fn array_iterator_next(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let target = {
        let key = ITERATOR_TARGET.to_owned();
        // SAFETY: `key` is a live Rust string.
        unsafe { crisol_property_load(this_value, key.as_ptr(), key.len() as u64) }
    };
    let position = property_number(this_value, ITERATOR_POSITION).unwrap_or(0.0);
    let kind = property_number(this_value, ITERATOR_KIND).unwrap_or(1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a position this code wrote, always a non-negative whole number"
    )]
    let at = position as usize;

    with_rooted(&[this_value, target], || {
        // **Walked by `indexed_length`/`indexed_get`, not `element_at`.** The iterator's
        // receiver may be an array-like — `Array.prototype.values.call({0:…, length:…})` is a
        // whole family of test262 cases — and reading its elements as though it were a dense
        // array answered `undefined` for every one.
        let length = match indexed_length(target) {
            Ok(len) => len,
            Err(thrown) => return thrown,
        };
        if at >= length {
            let result = crisol_create_object();
            with_rooted(&[result], || {
                if let Some(into) = handle_of(result) {
                    with_runtime(|runtime| {
                        runtime.define(into, "value", Value::UNDEFINED);
                        runtime.define(into, "done", Value::TRUE);
                    });
                }
            });
            return result;
        }
        // A `keys` walk reads no element, so a throwing getter cannot reach it; the other two
        // read one, and a throw there propagates rather than ending the walk quietly.
        let element = if kind == 0.0 {
            index_value(at)
        } else {
            match indexed_get_checked(target, at) {
                Ok(element) => element,
                Err(thrown) => return thrown,
            }
        };
        let result = crisol_create_object();
        with_rooted(&[result, element], || {
            let Some(into) = handle_of(result) else {
                return;
            };
            // Built and stored one at a time, because each allocates (D-127).
            let value = if kind == 2.0 {
                array_of_values(&[index_value(at), element])
            } else {
                element
            };
            with_runtime(|runtime| {
                runtime.define(into, "value", Value::from_bits(value));
                runtime.define(into, "done", Value::FALSE);
            });
        });
        #[expect(clippy::cast_precision_loss, reason = "an index into an array")]
        let next = (at + 1) as f64;
        if let Some(handle) = handle_of(this_value) {
            with_runtime(|runtime| {
                runtime.define_hidden(handle, ITERATOR_POSITION, Value::number(next));
            });
        }
        result
    })
}

/// `Array.prototype.copyWithin` — moves a run within the array, in place.
///
/// **The length never changes.** A run copied past the end is truncated rather than growing the
/// array, which is what separates this from `splice`.
extern "C" fn array_copy_within(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = match relative_index(unsafe { argument(argc, argv, 0) }, length, 0) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let start = match relative_index(unsafe { argument(argc, argv, 1) }, length, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let end = match relative_index(unsafe { argument(argc, argv, 2) }, length, length) {
        Ok(end) => end,
        Err(thrown) => return thrown,
    };

    let taken = end.saturating_sub(start).min(length - target);
    // Read before writing, because the source and destination runs may overlap — copying in
    // place forwards would read values it had already overwritten.
    let mut moved: Vec<u64> = Vec::with_capacity(taken);
    for at in 0..taken {
        // A getter may throw, and a walk that swallowed it would keep going over a length
        // the receiver only claims to have.
        match with_rooted(&moved, || indexed_get_checked(this_value, start + at)) {
            Ok(value) => moved.push(value),
            Err(thrown) => return thrown,
        }
    }
    with_rooted(&moved, || {
        for (at, value) in moved.iter().enumerate() {
            indexed_set(this_value, target + at, *value);
        }
    });
    this_value
}

/// `Array.prototype.toReversed` — a reversed copy.
///
/// **The copying counterparts leave the original alone**, which is the whole of why they exist
/// alongside `reverse`, `sort` and `splice`.
extern "C" fn array_to_reversed(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    with_rooted(&[this_value], || {
        let reversed: Vec<u64> = (0..length)
            .rev()
            .map(|index| element_at(array, index))
            .collect();
        array_of_values(&reversed)
    })
}

/// `Array.prototype.toSorted` — a sorted copy.
extern "C" fn array_to_sorted(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    let copy = with_rooted(&live, || {
        let values: Vec<u64> = (0..length).map(|index| element_at(array, index)).collect();
        array_of_values(&values)
    });
    // Sorted through the same code the in-place sort uses, so the two cannot drift apart on
    // the default ordering or on where `undefined` lands.
    with_rooted(&[copy], || {
        array_sort(0, copy, 0, argc, argv);
        copy
    })
}

/// `Array.prototype.toSpliced` — a copy with a run replaced.
extern "C" fn array_to_spliced(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = match relative_index(unsafe { argument(argc, argv, 0) }, length, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    let removing = if argc < 2 {
        length - start
    } else {
        // Coerced, and a throw from the coercion is the answer — the same rule as the start
        // index beside it.
        let asked = match integer_argument(argc, argv, 1) {
            Ok(asked) => asked,
            Err(thrown) => return thrown,
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=remaining"
        )]
        let count = asked.max(0.0) as usize;
        count.min(length - start)
    };

    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let mut built: Vec<u64> = (0..start).map(|index| element_at(array, index)).collect();
        for position in 2..argc as usize {
            // SAFETY: as above.
            built.push(unsafe { argument(argc, argv, position) });
        }
        built.extend((start + removing..length).map(|index| element_at(array, index)));
        array_of_values(&built)
    })
}

/// `Array.prototype.with` — a copy with one index replaced.
extern "C" fn array_with(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let wanted = Value::from_bits(unsafe { argument(argc, argv, 0) })
        .as_number()
        .unwrap_or(0.0);
    #[expect(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
    let span = length as f64;
    let resolved = if wanted < 0.0 { span + wanted } else { wanted };
    // **Out of range is a `RangeError`**, where `at` answers `undefined` — this one builds an
    // array and there is no array to build for an index that does not exist.
    if resolved < 0.0 || resolved >= span || wanted.is_nan() {
        return raise("index is out of range", "RangeError");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked against both ends just above"
    )]
    let at = resolved as usize;
    // SAFETY: as above.
    let replacement = unsafe { argument(argc, argv, 1) };

    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let values: Vec<u64> = (0..length)
            .map(|index| {
                if index == at {
                    replacement
                } else {
                    element_at(array, index)
                }
            })
            .collect();
        array_of_values(&values)
    })
}

/// A stable merge sort over `values`, ordered by `before`.
///
/// **Hand-rolled rather than `sort_by`.** Rust's sort may panic when the comparison is not a
/// total order, and a JavaScript comparator is arbitrary user code — `sort(() => 1)` is legal
/// and inconsistent. A panic in a runtime helper is not recoverable, so the order has to be
/// merged by hand, where an inconsistent answer produces a strange permutation and nothing
/// worse.
fn merge_sort(values: &mut Vec<u64>, before: &mut impl FnMut(u64, u64) -> bool) {
    let length = values.len();
    if length < 2 {
        return;
    }
    let mut buffer = values.clone();
    let mut width = 1;
    while width < length {
        let mut start = 0;
        while start < length {
            let middle = (start + width).min(length);
            let end = (start + 2 * width).min(length);
            let (mut left, mut right, mut at) = (start, middle, start);
            while left < middle && right < end {
                if before(values[right], values[left]) {
                    buffer[at] = values[right];
                    right += 1;
                } else {
                    // `!before(right, left)` keeps equal elements in order, which is what makes
                    // this stable — the specification has required a stable sort since ES2019.
                    buffer[at] = values[left];
                    left += 1;
                }
                at += 1;
            }
            while left < middle {
                buffer[at] = values[left];
                left += 1;
                at += 1;
            }
            while right < end {
                buffer[at] = values[right];
                right += 1;
                at += 1;
            }
            start += 2 * width;
        }
        std::mem::swap(values, &mut buffer);
        width *= 2;
    }
}

/// `Array.prototype.sort`.
///
/// **The default order is by text, not by number.** `[10, 9].sort()` is `[10, 9]`, because
/// `"10"` sorts before `"9"`. That surprises everyone once and is the specification's rule.
///
/// **`undefined` sorts to the end** and never reaches the comparator, which is why it is
/// partitioned out rather than compared.
extern "C" fn array_sort(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return this_value;
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let comparator = unsafe { argument(argc, argv, 0) };
    // **A comparator is optional and, when given, has to be callable.**
    // `[3, 1].sort(5)` is a `TypeError` before anything is compared; without
    // the check every comparison reached `crisol_not_a_function`, which
    // answers `undefined`, and the sort silently kept its input order.
    if !Value::from_bits(comparator).is_undefined() && !is_callable(comparator) {
        return raise("a comparator must be a function", "TypeError");
    }
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // Every value here is still in the array, which is rooted, so the collector can see
        // them all while the comparator runs.
        let mut present = Vec::with_capacity(length);
        let mut absent = 0;
        for index in 0..length {
            let element = element_at(array, index);
            if Value::from_bits(element).kind() == crisol_value::Kind::Undefined {
                absent += 1;
            } else {
                present.push(element);
            }
        }

        let mut before = |left: u64, right: u64| -> bool {
            if is_callable(comparator) {
                let verdict = call_value(comparator, Value::UNDEFINED.to_bits(), &[left, right]);
                return Value::from_bits(verdict)
                    .as_number()
                    .is_some_and(|order| order < 0.0);
            }
            to_text(left)
                .zip(to_text(right))
                .is_some_and(|(a, b)| a < b)
        };
        merge_sort(&mut present, &mut before);

        with_runtime(|runtime| {
            for (index, value) in present.iter().enumerate() {
                runtime
                    .heap
                    .set_element(array, index, Value::from_bits(*value));
            }
            for offset in 0..absent {
                runtime
                    .heap
                    .set_element(array, present.len() + offset, Value::UNDEFINED);
            }
        });
        this_value
    })
}

/// `Array.prototype.splice`.
///
/// **Answers the removed elements and mutates in place**, which is the pair of jobs that makes
/// it the odd one out among the array methods — every other mutator answers the array or a
/// count.
extern "C" fn array_splice(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = match relative_index(unsafe { argument(argc, argv, 0) }, length, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    // **No second argument removes everything from `start` on**; a second argument of
    // `undefined` removes nothing. The two are different, which is why `argc` is read rather
    // than the value.
    let removing = if argc < 2 {
        length - start
    } else {
        // Coerced, and a throw from the coercion is the answer — the same rule as the start
        // index beside it.
        let asked = match integer_argument(argc, argv, 1) {
            Ok(asked) => asked,
            Err(thrown) => return thrown,
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=remaining"
        )]
        let count = asked.max(0.0) as usize;
        count.min(length - start)
    };

    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // Species is consulted with the delete count, which `splice(0, -0)` makes a `+0`
        // passed as the one argument — `create-species-neg-zero` checks exactly that. Inside
        // `with_rooted` so the discarded probe allocation cannot collect the receiver.
        let probe = array_species_create(this_value, removing);
        if Value::from_bits(probe).is_exception() {
            return probe;
        }
        let removed: Vec<u64> = (0..removing)
            .map(|offset| element_at(array, start + offset))
            .collect();
        let inserted: Vec<u64> = (2..argc as usize)
            // SAFETY: as above.
            .map(|position| unsafe { argument(argc, argv, position) })
            .collect();
        let tail: Vec<u64> = (start + removing..length)
            .map(|index| element_at(array, index))
            .collect();

        with_runtime(|runtime| {
            let mut at = start;
            for value in inserted.iter().chain(tail.iter()) {
                runtime
                    .heap
                    .set_element(array, at, Value::from_bits(*value));
                at += 1;
            }
            runtime.heap.truncate_elements(array, at);
        });
        array_of_values(&removed)
    })
}

/// `Array.prototype.toString` and `toLocaleString` — the elements, comma-separated.
extern "C" fn array_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    with_rooted(&[this_value], || {
        let mut out = String::new();
        for index in 0..length {
            // As in `join`: the length is the program's, so the result is unbounded unless
            // something bounds it.
            if out.len() > MAX_STRING_UNITS {
                return raise("joined string is too long", "RangeError");
            }
            if index > 0 {
                out.push(',');
            }
            let element = Value::from_bits(indexed_get(this_value, index));
            if !matches!(
                element.kind(),
                crisol_value::Kind::Null | crisol_value::Kind::Undefined
            ) && let Some(text) = to_text(element.to_bits())
            {
                out.push_str(&text);
            }
        }
        new_string(&out)
    })
}

/// `Array.prototype.reduceRight`.
///
/// **Not `reduce` with a reversed list.** The callback still receives each element's real
/// index, so reversing the array first would hand it the wrong ones — and for a callback that
/// looks at the index, that is a wrong answer rather than a slower one.
extern "C" fn array_reduce_right(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let receiver = match object_receiver(this_value) {
        Ok(receiver) => receiver,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        with_rooted(&[receiver], || {
            let length = match indexed_length(receiver) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            // SAFETY: the convention guarantees `argc` readable values at `argv`.
            let callback = unsafe { argument(argc, argv, 0) };
            if !is_callable(callback) {
                return raise("a callback must be a function", "TypeError");
            }
            let mut position = length;
            let mut total = if argc > 1 {
                // SAFETY: the count was just checked.
                unsafe { argument(argc, argv, 1) }
            } else if position == 0 {
                // **An empty array with no initial value is a `TypeError`**, not `undefined` —
                // there is no answer to give, and inventing one hides the mistake.
                return raise(
                    "reduceRight of an empty array with no initial value",
                    "TypeError",
                );
            } else {
                position -= 1;
                match indexed_get_checked(receiver, position) {
                    Ok(last) => last,
                    Err(thrown) => return thrown,
                }
            };
            while position > 0 {
                position -= 1;
                let element =
                    match with_rooted(&[total], || indexed_get_checked(receiver, position)) {
                        Ok(element) => element,
                        Err(thrown) => return thrown,
                    };
                total = with_rooted(&[total, element], || {
                    call_value(
                        callback,
                        Value::UNDEFINED.to_bits(),
                        &[total, element, index_value(position), receiver],
                    )
                });
                if Value::from_bits(total).is_exception() {
                    return total;
                }
            }
            total
        })
    })
}

/// Appends `value` to `into`, spreading it if it is an array and `depth` allows.
fn flatten_into(into: &mut Vec<u64>, value: u64, depth: i32) {
    match elements_of(value) {
        Some((array, length)) if depth > 0 => {
            for index in 0..length {
                flatten_into(into, element_at(array, index), depth - 1);
            }
        }
        _ => into.push(value),
    }
}

/// `Array.prototype.flat`.
extern "C" fn array_flat(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = Value::from_bits(unsafe { argument(argc, argv, 0) });
    // **One level by default**, not all of them — `[[1, [2]]].flat()` still holds an array.
    let depth = given
        .as_number()
        .map_or(1.0, |number| if number.is_nan() { 0.0 } else { number });
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to a depth no array can exceed"
    )]
    let depth = depth.clamp(0.0, f64::from(i32::MAX)) as i32;

    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let mut flattened = Vec::new();
        for index in 0..length {
            flatten_into(&mut flattened, element_at(array, index), depth);
        }
        array_of_values(&flattened)
    })
}

/// `Array.prototype.flatMap` — map, then flatten one level.
extern "C" fn array_flat_map(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
    // **Checked before a single element is read.** `[1, 2].map(5)` throws
    // rather than calling nothing twice and answering `[undefined,
    // undefined]` — which is what reaching `crisol_not_a_function` per
    // element produced, and it looked like a working call every time.
    if !is_callable(callback) {
        return raise("a callback must be a function", "TypeError");
    }
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // **Mapped into a rooted array before anything is flattened.** The callback allocates,
        // and a plain `Vec` of results is invisible to the collector — every result but the
        // newest would be freed under it. Flattening afterwards runs no JavaScript, so nothing
        // can move once the loop is done.
        with_new_array(length, |mapped| {
            for index in 0..length {
                let result = call_value(
                    callback,
                    Value::UNDEFINED.to_bits(),
                    &[element_at(array, index), index_value(index), this_value],
                );
                if Value::from_bits(result).is_exception() {
                    return result;
                }
                with_runtime(|runtime| {
                    runtime
                        .heap
                        .set_element(mapped, index, Value::from_bits(result));
                });
            }
            let mut flattened = Vec::new();
            for index in 0..length {
                // Exactly one level, always — `flatMap` takes no depth.
                flatten_into(&mut flattened, element_at(mapped, index), 1);
            }
            array_of_values(&flattened)
        })
    })
}

/// `Array.prototype.at`.
///
/// **A negative index counts from the end and out of range is `undefined`** — which is what
/// separates `at` from indexing, where `-1` is a property name rather than a position.
extern "C" fn array_at(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    let wanted = match integer_argument(argc, argv, 0) {
        Ok(wanted) => wanted,
        Err(thrown) => return thrown,
    };
    #[expect(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
    let span = length as f64;
    let resolved = if wanted < 0.0 { span + wanted } else { wanted };
    if resolved < 0.0 || resolved >= span {
        return Value::UNDEFINED.to_bits();
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked against both ends just above"
    )]
    let index = resolved as usize;
    indexed_get(this_value, index)
}

/// `findLast` and `findLastIndex`, which walk backwards.
fn find_last_with(this_value: u64, argc: u64, argv: *const u64, want_index: bool) -> u64 {
    let receiver = match object_receiver(this_value) {
        Ok(receiver) => receiver,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        with_rooted(&[receiver], || {
            let length = match indexed_length(receiver) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            // SAFETY: as above.
            let callback = unsafe { argument(argc, argv, 0) };
            if !is_callable(callback) {
                return raise("a callback must be a function", "TypeError");
            }
            // SAFETY: as above.
            let this_arg = unsafe { argument(argc, argv, 1) };
            for index in (0..length).rev() {
                let element = match indexed_get_checked(receiver, index) {
                    Ok(element) => element,
                    Err(thrown) => return thrown,
                };
                let verdict =
                    call_value(callback, this_arg, &[element, index_value(index), receiver]);
                if Value::from_bits(verdict).is_exception() {
                    return verdict;
                }
                if is_truthy(Value::from_bits(verdict)) {
                    return if want_index {
                        index_value(index)
                    } else {
                        element
                    };
                }
            }
            if want_index {
                Value::number(-1.0).to_bits()
            } else {
                Value::UNDEFINED.to_bits()
            }
        })
    })
}

/// `Array.prototype.findLast`.
extern "C" fn array_find_last(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    find_last_with(this_value, argc, argv, false)
}

/// `Array.prototype.findLastIndex`.
extern "C" fn array_find_last_index(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    find_last_with(this_value, argc, argv, true)
}

/// Every global that is a function, in the order their indices name them.
///
/// Numbered *after* [`NATIVES`], so one negative index space covers both and
/// `crisol_closure_code` needs no second rule.
const GLOBAL_NATIVES: &[(&str, Native)] = &[
    ("Error", make_error),
    ("TypeError", make_error),
    ("RangeError", make_error),
    ("ReferenceError", make_error),
    ("SyntaxError", make_error),
    ("String", to_string_global),
    ("Number", to_number_global),
    ("Boolean", to_boolean_global),
    ("Function", unconstructable),
    ("parseInt", global_parse_int),
    ("parseFloat", global_parse_float),
    ("isNaN", global_is_nan),
    ("isFinite", global_is_finite),
    ("RegExp", make_regexp),
    ("Date", make_date_object),
    ("Map", make_map),
    ("Set", make_set),
    ("Symbol", make_symbol),
    ("Promise", make_promise),
    ("Proxy", make_proxy),
    ("ArrayBuffer", make_array_buffer),
    ("Int8Array", make_int8_array),
    ("Uint8Array", make_uint8_array),
    ("Uint8ClampedArray", make_uint8_clamped_array),
    ("Int16Array", make_int16_array),
    ("Uint16Array", make_uint16_array),
    ("Int32Array", make_int32_array),
    ("Uint32Array", make_uint32_array),
    ("Float32Array", make_float32_array),
    ("Float64Array", make_float64_array),
];

/// The nine typed-array constructor names, in [`ELEMENT_KINDS`] order.
const TYPED_ARRAY_NAMES: [&str; 9] = [
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float32Array",
    "Float64Array",
];

/// The natives behind the `ArrayBuffer` and typed-array prototypes, installed by hand in
/// [`Runtime::build_typed_array_prototypes`] rather than looked up by name, so this table is a
/// plain list addressed by the constants below. Appended after every other table, so the indices
/// before it never move.
const TYPED_NATIVES: &[Native] = &[
    array_buffer_byte_length,       // AB_BYTE_LENGTH
    array_buffer_slice,             // AB_SLICE
    array_buffer_is_view,           // AB_IS_VIEW
    typed_array_length_getter,      // TA_LENGTH_GET
    typed_array_byte_length_getter, // TA_BYTE_LENGTH_GET
    typed_array_byte_offset_getter, // TA_BYTE_OFFSET_GET
    typed_array_buffer_getter,      // TA_BUFFER_GET
    typed_array_tag_getter,         // TA_TAG_GET
    typed_array_set,                // TA_SET_METHOD
    typed_array_subarray,           // TA_SUBARRAY_METHOD
    typed_array_slice,              // TA_SLICE_METHOD
];

/// Indices into [`TYPED_NATIVES`].
const AB_BYTE_LENGTH: usize = 0;
const AB_SLICE: usize = 1;
const AB_IS_VIEW: usize = 2;
const TA_LENGTH_GET: usize = 3;
const TA_BYTE_LENGTH_GET: usize = 4;
const TA_BYTE_OFFSET_GET: usize = 5;
const TA_BUFFER_GET: usize = 6;
const TA_TAG_GET: usize = 7;
const TA_SET_METHOD: usize = 8;
const TA_SUBARRAY_METHOD: usize = 9;
const TA_SLICE_METHOD: usize = 10;

/// The global index at which [`TYPED_NATIVES`] begins — the sum of every table before it, in the
/// order [`crisol_closure_code`] chains them. Shared by the two places that address the table so
/// they cannot drift.
fn typed_natives_base() -> usize {
    NATIVES.len()
        + GLOBAL_NATIVES.len()
        + NAMESPACE_NATIVES.len()
        + ANONYMOUS_NATIVES.len()
        + FUNCTION_NATIVES.len()
        + STRING_NATIVES.len()
        + REGEXP_NATIVES.len()
        + DATE_NATIVES.len()
        + OBJECT_NATIVES.len()
        + PROMISE_NATIVES.len()
        + MAP_NATIVES.len()
        + SET_NATIVES.len()
        + SYMBOL_NATIVES.len()
        + ARRAY_ITERATOR_NATIVES.len()
        + NUMBER_NATIVES.len()
        + BOOLEAN_NATIVES.len()
}

/// Where a symbol keeps its description.
const SYMBOL_DESCRIPTION: &str = "description";

/// `Symbol(description)`.
///
/// **A symbol is a heap cell wearing a different tag.** It could have been a bare payload —
/// symbols are not objects and have no properties a program can add — but `as_address` answers
/// for `TAG_SYMBOL` as readily as for an object, so every place that turns a value into a
/// `GcRef` (the root walk among them) would have traced that payload as though it pointed at a
/// cell. Making it *actually* point at one costs an allocation per symbol and makes the hazard
/// impossible rather than avoided by convention.
///
/// Identity is the cell's: two `Symbol("x")` are different symbols, and that falls out rather
/// than being arranged.
extern "C" fn make_symbol(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 0) };
    let description = if Value::from_bits(given).kind() == crisol_value::Kind::Undefined {
        None
    } else {
        to_text(given)
    };
    new_symbol(description.as_deref())
}

/// Makes a symbol with an optional description.
fn new_symbol(description: Option<&str>) -> u64 {
    let cell = with_runtime(|runtime| {
        let shape = runtime.shapes.borrow().root();
        let scope = runtime.heap.scope();
        let cell = scope.alloc(shape, 0);
        if let Some(prototype) = SYMBOL_PROTOTYPE.with(std::cell::Cell::get) {
            runtime.heap.set_prototype(cell.handle(), Some(prototype));
        }
        cell.handle()
    });
    // Re-tagged: the same cell, described as a symbol rather than an object, which is what
    // makes `typeof` answer `"symbol"` while the collector still sees a cell it understands.
    let symbol = cell.to_value().as_address().map_or_else(
        || Value::UNDEFINED.to_bits(),
        |at| Value::symbol(at).to_bits(),
    );

    if let Some(text) = description {
        with_rooted(&[symbol], || {
            let described = new_string(text);
            with_runtime(|runtime| {
                runtime.define_hidden(cell, SYMBOL_DESCRIPTION, Value::from_bits(described));
            });
        });
    }
    symbol
}

/// `Symbol.for(key)` — the cross-realm registry.
///
/// **Deliberately immortal.** A registered symbol has to come back for the same key however
/// long later, so the registry is a root and its entries are never collected. That is the
/// specification's design rather than a leak, which is the difference between this and the
/// registry `Map` was not given (D-148).
extern "C" fn symbol_for(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = to_text(unsafe { argument(argc, argv, 0) }).unwrap_or_default();
    if let Some(existing) = SYMBOL_REGISTRY.with(|registry| registry.borrow().get(&key).copied()) {
        return existing;
    }
    let symbol = new_symbol(Some(&key));
    SYMBOL_REGISTRY.with(|registry| registry.borrow_mut().insert(key, symbol));
    symbol
}

/// `Symbol.keyFor(symbol)` — the key a registered symbol was made with, or `undefined`.
extern "C" fn symbol_key_for(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let symbol = unsafe { argument(argc, argv, 0) };
    // **Only a registered symbol has a key.** One made by `Symbol("x")` answers `undefined`
    // even though its description is `"x"` — the description is not the key.
    let found = SYMBOL_REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .find(|(_, value)| **value == symbol)
            .map(|(key, _)| key.clone())
    });
    found.map_or_else(|| Value::UNDEFINED.to_bits(), |key| new_string(&key))
}

/// `Symbol.prototype.toString`.
extern "C" fn symbol_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let description = property_text(this_value, SYMBOL_DESCRIPTION).unwrap_or_default();
    new_string(&format!("Symbol({description})"))
}

/// `Symbol.prototype.valueOf`.
extern "C" fn symbol_value_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    this_value
}

/// Methods on `Number.prototype`.
const NUMBER_NATIVES: &[(&str, Native)] = &[
    ("toString", number_to_text),
    ("toLocaleString", number_to_text),
    ("valueOf", number_value_of),
    ("toFixed", number_to_fixed),
];

/// Methods on `Boolean.prototype`.
const BOOLEAN_NATIVES: &[(&str, Native)] =
    &[("toString", boolean_to_text), ("valueOf", boolean_value_of)];

/// The number a receiver stands for: itself, or the value its wrapper holds.
///
/// **An object receiver is read, not coerced.** `new Number(5).valueOf()` has to find the five
/// the wrapper was built with, and coercing the wrapper would run its own `valueOf` — which is
/// this function, and does not end (D-146).
/// `thisNumberValue` — the number a receiver *is*, or a `TypeError` if it is not one.
///
/// **Stricter than coercion.** `Number.prototype.valueOf.call("5")` is a `TypeError`, not `5`:
/// the receiver must be a number or a Number wrapper, where [`this_number`] coerced anything.
/// A wrapper keeps its primitive in the shared slot, so `as_number` answering `Some` is what
/// says this one holds a number rather than a string or a boolean.
fn require_number(this_value: u64) -> Result<f64, u64> {
    let held = Value::from_bits(this_value);
    if held.kind() == crisol_value::Kind::Number {
        return Ok(held.as_number().unwrap_or(f64::NAN));
    }
    if handle_of(this_value).is_some()
        && let Some(value) = property_number(this_value, STRING_PRIMITIVE)
    {
        return Ok(value);
    }
    Err(raise("this is not a Number", "TypeError"))
}

/// `Number.prototype.toString(radix)`.
extern "C" fn number_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let value = match require_number(this_value) {
        Ok(value) => value,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let radix = to_number(unsafe { argument(argc, argv, 0) });
    if !radix.is_finite() || radix == 10.0 {
        return new_string(&number_text(value));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked finite, and rejected below unless 2..=36"
    )]
    let radix = radix as u32;
    if !(2..=36).contains(&radix) {
        return raise("radix must be between 2 and 36", "RangeError");
    }
    if !value.is_finite() {
        return new_string(&number_text(value));
    }

    // **Whole part only.** A fraction in another radix is a longer story than this needs, and
    // truncating quietly would be worse than saying so here.
    let negative = value < 0.0;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the fractional part is deliberately dropped"
    )]
    let mut whole = value.abs().trunc() as i64;
    let mut digits = Vec::new();
    if whole == 0 {
        digits.push(b'0');
    }
    while whole > 0 {
        let digit = u32::try_from(whole % i64::from(radix)).unwrap_or(0);
        digits.push(char::from_digit(digit, radix).unwrap_or('0') as u8);
        whole /= i64::from(radix);
    }
    if negative {
        digits.push(b'-');
    }
    digits.reverse();
    new_string(&String::from_utf8_lossy(&digits))
}

/// `Number.prototype.valueOf`.
extern "C" fn number_value_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match require_number(this_value) {
        Ok(value) => from_number(value),
        Err(thrown) => thrown,
    }
}

/// `Number.prototype.toFixed(digits)`.
extern "C" fn number_to_fixed(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let value = match require_number(this_value) {
        Ok(value) => value,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let digits = to_number(unsafe { argument(argc, argv, 0) });
    let digits = if digits.is_finite() { digits } else { 0.0 };
    if !(0.0..=100.0).contains(&digits) {
        return raise("digits must be between 0 and 100", "RangeError");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range-checked immediately above"
    )]
    let places = digits as usize;
    if !value.is_finite() {
        return new_string(&number_text(value));
    }
    new_string(&format!("{value:.places$}"))
}

/// The boolean a receiver stands for.
fn require_boolean(this_value: u64) -> Result<bool, u64> {
    let held = Value::from_bits(this_value);
    if held.kind() == crisol_value::Kind::Boolean {
        return Ok(held.as_boolean().unwrap_or(false));
    }
    // **A Boolean wrapper, and not a Number or String one.** All three keep their primitive in
    // the same slot; `as_boolean` answering `Some` is what says this one holds a boolean, so
    // `Boolean.prototype.toString.call(new Number(1))` reaches the throw rather than reading a
    // number as `false`.
    if handle_of(this_value).is_some()
        && let Some((_, wrapped)) = own_property(this_value, STRING_PRIMITIVE)
        && let Some(value) = wrapped.as_boolean()
    {
        return Ok(value);
    }
    Err(raise("this is not a Boolean", "TypeError"))
}

/// `Boolean.prototype.toString`.
extern "C" fn boolean_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match require_boolean(this_value) {
        Ok(true) => new_string("true"),
        Ok(false) => new_string("false"),
        Err(thrown) => thrown,
    }
}

/// `Boolean.prototype.valueOf`.
extern "C" fn boolean_value_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match require_boolean(this_value) {
        Ok(value) => boolean(value).to_bits(),
        Err(thrown) => thrown,
    }
}

/// Methods on `Symbol.prototype`.
const SYMBOL_NATIVES: &[(&str, Native)] =
    &[("toString", symbol_to_text), ("valueOf", symbol_value_of)];

/// Where a `Map` or `Set` keeps its contents.
///
/// **A heap array, not a Rust collection.** `crisol-builtins` has a `JsMap` and it is not used
/// here, which is a departure from D-122 worth stating: it holds `Value`s in a Rust `HashMap`,
/// and a `Value` can be a reference the collector must trace. Keeping them outside the heap
/// would need a registry of live maps in the root set — and that registry would keep the
/// contents of *dead* maps alive too, because nothing tells it when a wrapper is collected.
/// A backing array inside the heap is traced already, for free and without a leak.
///
/// The cost is lookup: this is a scan, where a `HashMap` is not. Correct and linear beats fast
/// and leaking, and the day it matters the fix is a real hash table in the heap rather than a
/// Rust one beside it.
const COLLECTION_ENTRIES: &str = "__entries";
/// How many entries a `Map` or `Set` holds.
///
/// A plain property because `size` is an accessor in the specification and there are no
/// accessors here. It is maintained on every mutation rather than counted on every read.
const COLLECTION_SIZE: &str = "size";

/// SameValueZero — the comparison `Map` and `Set` key on.
///
/// **`NaN` equals itself here**, which `===` does not do. That is the whole difference, and it
/// is why a set can contain `NaN` at all: without it, every `add(NaN)` would add another.
fn same_value_zero(left: u64, right: u64) -> bool {
    let (a, b) = (Value::from_bits(left), Value::from_bits(right));
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => same_value(a, b),
    }
}

/// The backing array of a `Map` or `Set`.
fn entries_of(collection: u64) -> Option<(GcRef, usize)> {
    let key = COLLECTION_ENTRIES.to_owned();
    // SAFETY: `key` is a live Rust string.
    let held = unsafe { crisol_property_load(collection, key.as_ptr(), key.len() as u64) };
    elements_of(held)
}

/// The mark a Map carries and a Set does not, and vice versa — the brand each method checks.
///
/// **Two names rather than one kind number** so the check is a plain presence test (`own_flag`)
/// with no float comparison, and so `Map.prototype.get.call(new Set())` — a real collection of
/// the wrong kind — is a `TypeError` as the specification's `thisMapData`/`thisSetData` require.
const MAP_BRAND: &str = "__mapData";
/// The mark a Set carries; see [`MAP_BRAND`].
const SET_BRAND: &str = "__setData";

/// A `TypeError` unless `this` is a Map, returned as the signal to propagate.
fn require_map(this_value: u64) -> Option<u64> {
    if own_flag(this_value, MAP_BRAND) {
        None
    } else {
        Some(raise("this is not a Map", "TypeError"))
    }
}

/// A `TypeError` unless `this` is a Set.
fn require_set(this_value: u64) -> Option<u64> {
    if own_flag(this_value, SET_BRAND) {
        None
    } else {
        Some(raise("this is not a Set", "TypeError"))
    }
}

/// A `TypeError` unless `this` is a Map or a Set.
///
/// `clear` is one function on both prototypes and cannot tell which it was reached through, so
/// it can require only "a collection" — which still rejects every non-collection receiver, the
/// case a program actually hits.
fn require_collection(this_value: u64) -> Option<u64> {
    if own_flag(this_value, MAP_BRAND) || own_flag(this_value, SET_BRAND) {
        None
    } else {
        Some(raise("this is not a Map or Set", "TypeError"))
    }
}

/// Records how many entries a collection now holds.
fn set_collection_size(collection: u64, entries: usize, stride: usize) {
    if let Some(handle) = handle_of(collection) {
        #[expect(clippy::cast_precision_loss, reason = "a count of heap entries")]
        let size = (entries / stride) as f64;
        with_runtime(|runtime| runtime.define_hidden(handle, COLLECTION_SIZE, Value::number(size)));
    }
}

/// Builds a `Map` or a `Set`: an object with a backing array and a size.
fn new_collection(prototype: Option<GcRef>, is_set: bool) -> u64 {
    let object = crisol_create_object();
    with_rooted(&[object], || {
        let Some(handle) = handle_of(object) else {
            return;
        };
        // Stored before the size is written, so nothing is unrooted across an allocation.
        let entries = array_of_values(&[]);
        with_runtime(|runtime| {
            runtime.define_hidden(handle, COLLECTION_ENTRIES, Value::from_bits(entries));
            runtime.define_hidden(handle, COLLECTION_SIZE, Value::number(0.0));
            // The brand, so a method can tell a Map from a Set from anything else.
            let brand = if is_set { SET_BRAND } else { MAP_BRAND };
            runtime.define_hidden(handle, brand, Value::TRUE);
            if let Some(prototype) = prototype {
                runtime.heap.set_prototype(handle, Some(prototype));
            }
        });
    });
    object
}

/// `new Map()`.
extern "C" fn make_map(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    new_collection(MAP_PROTOTYPE.with(std::cell::Cell::get), false)
}

/// `new Set()`.
extern "C" fn make_set(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    new_collection(SET_PROTOTYPE.with(std::cell::Cell::get), true)
}

/// A global that exists so its `prototype` can be reached, but cannot be called.
///
/// **`Function` is bound because `Function.prototype` has to be reachable**, not because
/// `new Function(body)` works — that compiles source at runtime, which this engine does not
/// do. Calling it raises rather than answering something wrong, and the binding existing is
/// what lets `Function.prototype.call` be named at all.
extern "C" fn unconstructable(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    raise("this constructor is not supported", "TypeError")
}

// ============================== Typed arrays ==============================
//
// An `ArrayBuffer` is an object carrying the [`ARRAY_BUFFER_BRAND`] and a raw byte store on its
// heap cell (see `Heap::attach_bytes`). A typed array is an object carrying the
// [`TYPED_ARRAY_BRAND`] and four hidden properties naming its buffer, byte offset, element count
// and element kind; its integer indices read and write the buffer through the kind's codec
// rather than living in an `elements` vector. Both follow the `Map`/`Set` shape — a brand plus a
// backing store — so nothing in the value representation had to change.

/// The mark every `ArrayBuffer` carries, so a method can tell one from any other object.
const ARRAY_BUFFER_BRAND: &str = "__arrayBuffer";
/// The mark every typed array carries; see [`ARRAY_BUFFER_BRAND`].
const TYPED_ARRAY_BRAND: &str = "__typedArray";
/// A typed array's backing `ArrayBuffer`, as a hidden property.
const TA_BUFFER: &str = "__taBuffer";
/// A typed array's first byte within its buffer.
const TA_OFFSET: &str = "__taOffset";
/// A typed array's element count.
const TA_LENGTH: &str = "__taLength";
/// A typed array's element kind, as the tag [`ElementKind::tag`] gives.
const TA_KIND: &str = "__taKind";

/// The nine element kinds a typed array can have. `BigInt64`/`BigUint64` are absent because
/// BigInt is not implemented.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ElementKind {
    I8,
    U8,
    U8Clamped,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

/// The kinds in tag order — the order every table that lists the typed arrays uses.
const ELEMENT_KINDS: [ElementKind; 9] = [
    ElementKind::I8,
    ElementKind::U8,
    ElementKind::U8Clamped,
    ElementKind::I16,
    ElementKind::U16,
    ElementKind::I32,
    ElementKind::U32,
    ElementKind::F32,
    ElementKind::F64,
];

impl ElementKind {
    /// The kind for a tag, or `None` if the tag names no kind.
    fn from_tag(tag: usize) -> Option<Self> {
        ELEMENT_KINDS.get(tag).copied()
    }

    /// This kind's position in [`ELEMENT_KINDS`], the number stored in [`TA_KIND`].
    fn tag(self) -> usize {
        self as usize
    }

    /// How many bytes one element occupies.
    fn bytes(self) -> usize {
        match self {
            Self::I8 | Self::U8 | Self::U8Clamped => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    /// The constructor name, which is also the `Symbol.toStringTag`.
    fn ctor_name(self) -> &'static str {
        match self {
            Self::I8 => "Int8Array",
            Self::U8 => "Uint8Array",
            Self::U8Clamped => "Uint8ClampedArray",
            Self::I16 => "Int16Array",
            Self::U16 => "Uint16Array",
            Self::I32 => "Int32Array",
            Self::U32 => "Uint32Array",
            Self::F32 => "Float32Array",
            Self::F64 => "Float64Array",
        }
    }

    /// Decodes one element from `b`, which is exactly [`ElementKind::bytes`] long and
    /// little-endian, the native order of every target crisol builds for.
    fn read(self, b: &[u8]) -> f64 {
        match self {
            Self::I8 => f64::from(i8::from_le_bytes([b[0]])),
            Self::U8 | Self::U8Clamped => f64::from(b[0]),
            Self::I16 => f64::from(i16::from_le_bytes([b[0], b[1]])),
            Self::U16 => f64::from(u16::from_le_bytes([b[0], b[1]])),
            Self::I32 => f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Self::U32 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Self::F32 => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Self::F64 => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        }
    }

    /// Encodes `value` into the first [`ElementKind::bytes`] of the returned array, applying the
    /// kind's conversion — `ToInt8`/`ToUint8`/`ToUint8Clamp` and their wider cousins for the
    /// integers, a narrowing for `Float32`.
    fn to_bytes(self, value: f64) -> [u8; 8] {
        let mut out = [0u8; 8];
        match self {
            Self::U8Clamped => out[0] = clamp_to_u8(value),
            Self::F32 => out[..4].copy_from_slice(&narrow_to_f32(value).to_le_bytes()),
            Self::F64 => out = value.to_le_bytes(),
            _ => {
                let bits = self.bytes() * 8;
                out = wrap_to_bits(value, bits).to_le_bytes();
            }
        }
        out
    }
}

/// `value` reduced modulo `2**bits`, as `ToInt{8,16,32}`/`ToUint{8,16,32}` require. A non-finite
/// value becomes zero; the low `bits` bits of the result are the two's-complement pattern a
/// signed read decodes back to the right negative number.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "reduced into `0..2**bits`, which is at most `2**32` and non-negative"
)]
fn wrap_to_bits(value: f64, bits: usize) -> u64 {
    if !value.is_finite() {
        return 0;
    }
    let modulus = two_pow(bits);
    value.trunc().rem_euclid(modulus) as u64
}

/// `2**bits`, as an exact `f64` — `bits` is 8, 16 or 32.
#[expect(
    clippy::cast_precision_loss,
    reason = "an exact power of two below 2**53"
)]
fn two_pow(bits: usize) -> f64 {
    (1u64 << bits) as f64
}

/// `ToUint8Clamp(value)`: `NaN` and anything at or below zero clamp to zero, anything at or above
/// 255 to 255, and the rest round half to even.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "every path clamps into `0..=255` before the cast"
)]
fn clamp_to_u8(value: f64) -> u8 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= 255.0 {
        return 255;
    }
    let floor = value.floor();
    let frac = value - floor;
    let rounded = if frac < 0.5 {
        floor
    } else if frac > 0.5 {
        floor + 1.0
    } else if (floor as i64) % 2 == 0 {
        floor
    } else {
        floor + 1.0
    };
    rounded as u8
}

/// `value` narrowed to `Float32`, the storage a `Float32Array` keeps.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the narrowing is the point"
)]
fn narrow_to_f32(value: f64) -> f32 {
    value as f32
}

/// `f64` read back as a `usize` count crisol stored itself, so it is a small non-negative
/// integer.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a count crisol wrote, so non-negative and within range"
)]
fn count_of(value: f64) -> usize {
    value as usize
}

/// Whether `object` is an `ArrayBuffer`.
fn is_array_buffer(object: u64) -> bool {
    own_flag(object, ARRAY_BUFFER_BRAND)
}

/// Whether `object` is a typed array.
fn is_typed_array(object: u64) -> bool {
    own_flag(object, TYPED_ARRAY_BRAND)
}

/// A typed array's buffer, byte offset, element count and kind, or `None` if `object` is not one
/// or is missing a field.
fn typed_array_parts(object: u64) -> Option<(u64, usize, usize, ElementKind)> {
    let kind = ElementKind::from_tag(count_of(property_number(object, TA_KIND)?))?;
    let offset = count_of(property_number(object, TA_OFFSET)?);
    let length = count_of(property_number(object, TA_LENGTH)?);
    let buffer = property_of(object, TA_BUFFER);
    Some((buffer, offset, length, kind))
}

/// `ToIndex(value)` — a non-negative integer no larger than `2**53 - 1`, or the `RangeError` it
/// is not one.
fn to_index(value: u64) -> Result<usize, u64> {
    let number = coerce_number(value)?;
    let integer = if number.is_nan() { 0.0 } else { number.trunc() };
    if integer < 0.0 || integer > crisol_builtins::MAX_SAFE_INTEGER {
        return Err(raise("invalid length or index", "RangeError"));
    }
    Ok(count_of(integer))
}

/// Reads a typed array's element `index`, or `undefined` when it is out of range or the buffer is
/// gone. The caller has already established that `object` is a typed array.
fn typed_array_element_load(object: u64, index: usize) -> u64 {
    let Some((buffer, offset, length, kind)) = typed_array_parts(object) else {
        return Value::UNDEFINED.to_bits();
    };
    if index >= length {
        return Value::UNDEFINED.to_bits();
    }
    let Some(handle) = handle_of(buffer) else {
        return Value::UNDEFINED.to_bits();
    };
    let at = offset + index * kind.bytes();
    match with_runtime(|runtime| runtime.heap.read_bytes(handle, at, kind.bytes())) {
        Some(bytes) => Value::number(kind.read(&bytes)).to_bits(),
        None => Value::UNDEFINED.to_bits(),
    }
}

/// Writes `value` to a typed array's element `index`, coercing it to a number first. An index out
/// of range is a no-op, as the specification's integer-indexed `[[Set]]` requires. Returns the
/// exception if coercion threw. The caller has established that `object` is a typed array.
fn typed_array_element_store(object: u64, index: usize, value: u64) -> Option<u64> {
    // **The coercion happens even when the index is out of range**, because it can run a
    // `valueOf` the specification observes, and the store is skipped only afterwards.
    let number = match coerce_number(value) {
        Ok(number) => number,
        Err(thrown) => return Some(thrown),
    };
    let (buffer, offset, length, kind) = typed_array_parts(object)?;
    if index >= length {
        return None;
    }
    let handle = handle_of(buffer)?;
    let at = offset + index * kind.bytes();
    let bytes = kind.to_bytes(number);
    with_runtime(|runtime| runtime.heap.write_bytes(handle, at, &bytes[..kind.bytes()]));
    None
}

/// Builds an `ArrayBuffer` of `length` zeroed bytes.
/// The most bytes a buffer may hold. `ToIndex` admits values up to `2**53 - 1`, and
/// `new ArrayBuffer(2 ** 53)` must answer a `RangeError` rather than attempt to reserve seven
/// petabytes — an allocation that aborts the process rather than failing. Two gibibytes is the
/// ceiling every mainstream engine draws and comfortably more than any test asks to allocate.
const MAX_BYTE_LENGTH: usize = 0x7FFF_FFFF;

/// `count` elements of `per` bytes as a total byte length, or the `RangeError` for a total that
/// overflows or exceeds [`MAX_BYTE_LENGTH`] — the `CreateByteDataBlock` failure the specification
/// raises, in place of an allocation that would abort.
fn checked_byte_length(count: usize, per: usize) -> Result<usize, u64> {
    count
        .checked_mul(per)
        .filter(|&total| total <= MAX_BYTE_LENGTH)
        .ok_or_else(|| raise("invalid array buffer length", "RangeError"))
}

fn new_array_buffer(length: usize) -> u64 {
    let object = crisol_create_object();
    with_rooted(&[object], || {
        if let Some(handle) = handle_of(object) {
            with_runtime(|runtime| {
                runtime.heap.attach_bytes(handle, length);
                runtime.define_hidden(handle, ARRAY_BUFFER_BRAND, Value::TRUE);
                if let Some(prototype) = ARRAY_BUFFER_PROTOTYPE.with(std::cell::Cell::get) {
                    runtime.heap.set_prototype(handle, Some(prototype));
                }
            });
        }
    });
    object
}

/// `new ArrayBuffer(length)`.
extern "C" fn make_array_buffer(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let length = match to_index(unsafe { argument(argc, argv, 0) }) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    match checked_byte_length(length, 1) {
        Ok(length) => new_array_buffer(length),
        Err(thrown) => thrown,
    }
}

/// `ArrayBuffer.isView(value)` — whether `value` is a typed array (or, once it exists, a
/// `DataView`).
extern "C" fn array_buffer_is_view(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    let value = unsafe { argument(argc, argv, 0) };
    boolean(is_typed_array(value)).to_bits()
}

/// `get ArrayBuffer.prototype.byteLength`.
extern "C" fn array_buffer_byte_length(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if !is_array_buffer(this_value) {
        return raise("this is not an ArrayBuffer", "TypeError");
    }
    let length = handle_of(this_value)
        .and_then(|handle| with_runtime(|runtime| runtime.heap.byte_len(handle)))
        .unwrap_or(0);
    #[expect(clippy::cast_precision_loss, reason = "a byte length crisol allocated")]
    let length = length as f64;
    Value::number(length).to_bits()
}

/// `ArrayBuffer.prototype.slice(start, end)` — a fresh buffer holding the chosen bytes.
extern "C" fn array_buffer_slice(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if !is_array_buffer(this_value) {
        return raise("this is not an ArrayBuffer", "TypeError");
    }
    let Some(handle) = handle_of(this_value) else {
        return raise("this is not an ArrayBuffer", "TypeError");
    };
    let total = with_runtime(|runtime| runtime.heap.byte_len(handle)).unwrap_or(0);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = match relative_index(unsafe { argument(argc, argv, 0) }, total, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let end_arg = unsafe { argument(argc, argv, 1) };
    let end = if Value::from_bits(end_arg).is_undefined() {
        total
    } else {
        match relative_index(end_arg, total, total) {
            Ok(end) => end,
            Err(thrown) => return thrown,
        }
    };
    let count = end.saturating_sub(start);
    let bytes =
        with_runtime(|runtime| runtime.heap.read_bytes(handle, start, count)).unwrap_or_default();
    let fresh = new_array_buffer(count);
    with_rooted(&[fresh], || {
        if let Some(into) = handle_of(fresh) {
            with_runtime(|runtime| runtime.heap.write_bytes(into, 0, &bytes));
        }
    });
    fresh
}

/// A count as an `f64`, for storing in a hidden property or handing back from a getter.
#[expect(
    clippy::cast_precision_loss,
    reason = "a count crisol allocated, below 2**53"
)]
fn index_number(count: usize) -> f64 {
    count as f64
}

/// The per-kind prototype a typed array of `kind` inherits from.
fn typed_array_prototype(kind: ElementKind) -> Option<GcRef> {
    TYPED_ARRAY_PROTOTYPES.with(|protos| protos[kind.tag()].get())
}

/// Builds a typed array of `kind` viewing `length` elements of `buffer` from `offset`.
fn make_typed_array(kind: ElementKind, buffer: u64, offset: usize, length: usize) -> u64 {
    let object = crisol_create_object();
    with_rooted(&[object, buffer], || {
        if let Some(handle) = handle_of(object) {
            with_runtime(|runtime| {
                runtime.define_hidden(handle, TYPED_ARRAY_BRAND, Value::TRUE);
                runtime.define_hidden(handle, TA_BUFFER, Value::from_bits(buffer));
                runtime.define_hidden(handle, TA_OFFSET, Value::number(index_number(offset)));
                runtime.define_hidden(handle, TA_LENGTH, Value::number(index_number(length)));
                runtime.define_hidden(handle, TA_KIND, Value::number(index_number(kind.tag())));
                if let Some(prototype) = typed_array_prototype(kind) {
                    runtime.heap.set_prototype(handle, Some(prototype));
                }
            });
        }
    });
    object
}

/// `new TA(length)` — a fresh buffer sized to hold `length` elements, or the `RangeError` for a
/// length too large to allocate.
fn typed_array_over_new_buffer(kind: ElementKind, length: usize) -> Result<u64, u64> {
    let bytes = checked_byte_length(length, kind.bytes())?;
    let buffer = new_array_buffer(bytes);
    Ok(with_rooted(&[buffer], || {
        make_typed_array(kind, buffer, 0, length)
    }))
}

/// `new TA(buffer, byteOffset, length)` — a view over an existing buffer.
///
/// # Safety
///
/// `argv` must point to `argc` readable values.
unsafe fn typed_array_over_buffer(
    kind: ElementKind,
    buffer: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let per = kind.bytes();
    // SAFETY: the caller promises `argc` readable values at `argv`.
    let byte_offset = match to_index(unsafe { argument(argc, argv, 1) }) {
        Ok(offset) => offset,
        Err(thrown) => return thrown,
    };
    if !byte_offset.is_multiple_of(per) {
        return raise("start offset is not aligned", "RangeError");
    }
    let total = handle_of(buffer)
        .and_then(|handle| with_runtime(|runtime| runtime.heap.byte_len(handle)))
        .unwrap_or(0);
    if byte_offset > total {
        return raise("offset is outside the buffer", "RangeError");
    }
    // SAFETY: as above.
    let length_arg = unsafe { argument(argc, argv, 2) };
    let length = if Value::from_bits(length_arg).is_undefined() {
        let span = total - byte_offset;
        if !span.is_multiple_of(per) {
            return raise("byte length is not aligned", "RangeError");
        }
        span / per
    } else {
        let requested = match to_index(length_arg) {
            Ok(length) => length,
            Err(thrown) => return thrown,
        };
        if byte_offset + requested * per > total {
            return raise("length is outside the buffer", "RangeError");
        }
        requested
    };
    make_typed_array(kind, buffer, byte_offset, length)
}

/// `new TA(source)` where `source` is a typed array or an array-like — copy its elements, coerced
/// to this kind, into a fresh buffer.
fn typed_array_from_elements(kind: ElementKind, source: u64) -> u64 {
    let length = match walk_length(source) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    let result = match typed_array_over_new_buffer(kind, length) {
        Ok(result) => result,
        Err(thrown) => return thrown,
    };
    with_rooted(&[result, source], || {
        for index in 0..length {
            let element = match indexed_get_checked(source, index) {
                Ok(element) => element,
                Err(thrown) => return thrown,
            };
            if let Some(thrown) = typed_array_element_store(result, index, element) {
                return thrown;
            }
        }
        result
    })
}

/// The shared body behind every `new Int8Array(...)` and its siblings.
///
/// # Safety
///
/// `argv` must point to `argc` readable values.
unsafe fn new_typed_array(kind: ElementKind, argc: u64, argv: *const u64) -> u64 {
    // SAFETY: the caller promises `argc` readable values at `argv`.
    let arg0 = unsafe { argument(argc, argv, 0) };
    // **Only a true object is a buffer or a source.** A string or a number is a length, even
    // though a string is a heap cell here — `new Int8Array("5")` is five elements long.
    if matches!(Value::from_bits(arg0).kind(), crisol_value::Kind::Object) {
        // **`arg0` is rooted across the allocations below.** It arrived in `argv`, which the
        // collector does not scan (D-208), so a source array or buffer is otherwise freed the
        // moment the fresh buffer's allocation collects under stress — and the copy then reads a
        // reclaimed cell back as zeroes.
        return with_rooted(&[arg0], || {
            if is_array_buffer(arg0) {
                // SAFETY: as above.
                unsafe { typed_array_over_buffer(kind, arg0, argc, argv) }
            } else {
                typed_array_from_elements(kind, arg0)
            }
        });
    }
    let length = match to_index(arg0) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    match typed_array_over_new_buffer(kind, length) {
        Ok(result) => result,
        Err(thrown) => thrown,
    }
}

/// `get %TypedArray%.prototype.length`.
extern "C" fn typed_array_length_getter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match property_number(this_value, TA_LENGTH) {
        Some(length) if is_typed_array(this_value) => Value::number(length).to_bits(),
        _ => raise("this is not a typed array", "TypeError"),
    }
}

/// `get %TypedArray%.prototype.byteLength`.
extern "C" fn typed_array_byte_length_getter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let Some((_, _, length, kind)) = typed_array_parts(this_value) else {
        return raise("this is not a typed array", "TypeError");
    };
    Value::number(index_number(length * kind.bytes())).to_bits()
}

/// `get %TypedArray%.prototype.byteOffset`.
extern "C" fn typed_array_byte_offset_getter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match property_number(this_value, TA_OFFSET) {
        Some(offset) if is_typed_array(this_value) => Value::number(offset).to_bits(),
        _ => raise("this is not a typed array", "TypeError"),
    }
}

/// `get %TypedArray%.prototype.buffer`.
extern "C" fn typed_array_buffer_getter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if !is_typed_array(this_value) {
        return raise("this is not a typed array", "TypeError");
    }
    property_of(this_value, TA_BUFFER)
}

/// `get %TypedArray%.prototype[Symbol.toStringTag]` — the kind's name, or `undefined` for a
/// receiver that is not a typed array (the specification returns `undefined` here rather than
/// throwing, so `Object.prototype.toString.call({})` still works).
extern "C" fn typed_array_tag_getter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match typed_array_parts(this_value) {
        Some((_, _, _, kind)) => new_string(kind.ctor_name()),
        None => Value::UNDEFINED.to_bits(),
    }
}

/// `%TypedArray%.prototype.set(source, offset)` — copy `source`'s elements in at `offset`.
extern "C" fn typed_array_set(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((_, _, length, _)) = typed_array_parts(this_value) else {
        return raise("this is not a typed array", "TypeError");
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let offset = match to_index(unsafe { argument(argc, argv, 1) }) {
        Ok(offset) => offset,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let source = unsafe { argument(argc, argv, 0) };
    let source_length = match walk_length(source) {
        Ok(source_length) => source_length,
        Err(thrown) => return thrown,
    };
    if offset + source_length > length {
        return raise("source is too large", "RangeError");
    }
    with_rooted(&[this_value, source], || {
        for index in 0..source_length {
            let element = match indexed_get_checked(source, index) {
                Ok(element) => element,
                Err(thrown) => return thrown,
            };
            if let Some(thrown) = typed_array_element_store(this_value, offset + index, element) {
                return thrown;
            }
        }
        Value::UNDEFINED.to_bits()
    })
}

/// `%TypedArray%.prototype.subarray(start, end)` — a view over the same buffer.
extern "C" fn typed_array_subarray(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((buffer, byte_offset, length, kind)) = typed_array_parts(this_value) else {
        return raise("this is not a typed array", "TypeError");
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = match relative_index(unsafe { argument(argc, argv, 0) }, length, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let end = match relative_index(unsafe { argument(argc, argv, 1) }, length, length) {
        Ok(end) => end,
        Err(thrown) => return thrown,
    };
    let count = end.saturating_sub(start);
    with_rooted(&[buffer], || {
        make_typed_array(kind, buffer, byte_offset + start * kind.bytes(), count)
    })
}

/// `%TypedArray%.prototype.slice(start, end)` — a fresh typed array with copied elements.
extern "C" fn typed_array_slice(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((_, _, length, kind)) = typed_array_parts(this_value) else {
        return raise("this is not a typed array", "TypeError");
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = match relative_index(unsafe { argument(argc, argv, 0) }, length, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let end = match relative_index(unsafe { argument(argc, argv, 1) }, length, length) {
        Ok(end) => end,
        Err(thrown) => return thrown,
    };
    let count = end.saturating_sub(start);
    let result = match typed_array_over_new_buffer(kind, count) {
        Ok(result) => result,
        Err(thrown) => return thrown,
    };
    with_rooted(&[result, this_value], || {
        for index in 0..count {
            let element = typed_array_element_load(this_value, start + index);
            typed_array_element_store(result, index, element);
        }
    });
    result
}

/// The nine `new Int8Array(...)` bodies. Each names its kind and shares [`new_typed_array`].
extern "C" fn make_int8_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    unsafe { new_typed_array(ElementKind::I8, argc, argv) }
}

extern "C" fn make_uint8_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    unsafe { new_typed_array(ElementKind::U8, argc, argv) }
}

extern "C" fn make_uint8_clamped_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    unsafe { new_typed_array(ElementKind::U8Clamped, argc, argv) }
}

extern "C" fn make_int16_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    unsafe { new_typed_array(ElementKind::I16, argc, argv) }
}

extern "C" fn make_uint16_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    unsafe { new_typed_array(ElementKind::U16, argc, argv) }
}

extern "C" fn make_int32_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    unsafe { new_typed_array(ElementKind::I32, argc, argv) }
}

extern "C" fn make_uint32_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    unsafe { new_typed_array(ElementKind::U32, argc, argv) }
}

extern "C" fn make_float32_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    unsafe { new_typed_array(ElementKind::F32, argc, argv) }
}

extern "C" fn make_float64_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: as above.
    unsafe { new_typed_array(ElementKind::F64, argc, argv) }
}

/// `RegExp.escape(string)` — a string that, used as a pattern, matches itself literally.
///
/// **A string is required, not coerced.** `RegExp.escape(1)` is a `TypeError`; escaping a number
/// would invite passing one by mistake and quietly matching `"1"`.
///
/// The first character, when it is a letter or digit, is hex-escaped so the result can never
/// begin a quantifier or merge with what precedes it; the pattern syntax characters take a
/// backslash; control characters, white space and a set of punctuators become `\xHH`/`\uHHHH`;
/// everything else stands for itself.
extern "C" fn regexp_escape(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    if Value::from_bits(value).kind() != crisol_value::Kind::String {
        return raise("RegExp.escape needs a string", "TypeError");
    }
    let Some(text) = text_of(value) else {
        return new_string("");
    };
    let mut out = String::new();
    for (position, ch) in text.chars().enumerate() {
        if position == 0 && ch.is_ascii_alphanumeric() {
            push_regexp_hex(ch, &mut out);
        } else {
            encode_for_regexp_escape(ch, &mut out);
        }
    }
    new_string(&out)
}

/// One character of `RegExp.escape`'s output, after the leading-alphanumeric rule.
fn encode_for_regexp_escape(ch: char, out: &mut String) {
    match ch {
        '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        | '/' => {
            out.push('\\');
            out.push(ch);
        }
        '\t' => out.push_str("\\t"),
        '\n' => out.push_str("\\n"),
        '\u{0B}' => out.push_str("\\v"),
        '\u{0C}' => out.push_str("\\f"),
        '\r' => out.push_str("\\r"),
        // The "other punctuators" the specification escapes, plus white space and the line and
        // paragraph separators — each as a hex escape rather than a backslash.
        ',' | '-' | '=' | '<' | '>' | '#' | '&' | '!' | '%' | ':' | ';' | '@' | '~' | '\''
        | '`' | '"' | ' ' | '\u{A0}' | '\u{2028}' | '\u{2029}' | '\u{FEFF}' => {
            push_regexp_hex(ch, out);
        }
        ch if (ch as u32) <= 0x1F => push_regexp_hex(ch, out),
        ch => out.push(ch),
    }
}

/// A character as `\xHH` for a single byte, or one `\uHHHH` per UTF-16 code unit above that.
fn push_regexp_hex(ch: char, out: &mut String) {
    let point = ch as u32;
    if point <= 0xFF {
        out.push_str(&format!("\\x{point:02x}"));
    } else {
        let mut buffer = [0u16; 2];
        for unit in ch.encode_utf16(&mut buffer) {
            out.push_str(&format!("\\u{unit:04x}"));
        }
    }
}

/// `RegExp(source, flags)` and `new RegExp(source, flags)`.
extern "C" fn make_regexp(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let first = unsafe { argument(argc, argv, 0) };
    // **An existing regular expression is re-read through `source`**, so `new RegExp(/a/g)`
    // copies the pattern rather than stringifying the object into `"/a/g"`.
    let source = property_text(first, "source")
        .or_else(|| to_text(first))
        .unwrap_or_default();
    // SAFETY: as above.
    let given = unsafe { argument(argc, argv, 1) };
    let flags = if Value::from_bits(given).kind() == crisol_value::Kind::Undefined {
        property_text(first, "flags").unwrap_or_default()
    } else {
        to_text(given).unwrap_or_default()
    };
    // SAFETY: both strings are live for the call.
    unsafe {
        crisol_create_regexp(
            source.as_ptr(),
            source.len() as u64,
            flags.as_ptr(),
            flags.len() as u64,
        )
    }
}

/// `new Error(message)` and every error subclass.
///
/// One implementation for all of them because they differ only in `name`, which is read off the
/// constructor rather than hard-coded — so `TypeError` and `RangeError` are the same code with
/// different bindings, and adding another is a line in the table.
///
/// Called as a function rather than with `new` it behaves the same, which is what the
/// specification says for `Error` and what test262's own class does deliberately.
extern "C" fn make_error(
    closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let message = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // `new Error(…)` gives a receiver to fill; a plain call gives `undefined`, so one is
        // made here — **and it inherits from the constructor's own `prototype`**, because
        // `Error("x") instanceof Error` is true: called without `new`, the constructor still
        // constructs.
        let plain = handle_of(this_value).is_none();
        let receiver = if plain {
            crisol_create_object()
        } else {
            this_value
        };
        // **Rooted before anything else allocates.** The receiver a plain call makes is
        // reachable from nothing until it is returned, and the message below allocates a
        // string — under GC stress the error was collected between the two, and what came
        // back was a stale handle whose prototype nothing had managed to set. The symptom
        // was `Error("x") instanceof Error` answering `false` under stress and `true`
        // without, which is the shape every one of these bugs has had.
        with_rooted(&[receiver], || {
            let Some(target) = handle_of(receiver) else {
                return Value::UNDEFINED.to_bits();
            };
            with_runtime(|runtime| {
                runtime.define_hidden(target, ERROR_DATA, Value::number(1.0));
            });
            if plain {
                let prototype = handle_of(closure).and_then(|closure| {
                    with_runtime(|runtime| {
                        let key = PropertyKey::new("prototype");
                        let shape = runtime.heap.shape_of(closure)?;
                        let slot = runtime.shapes.borrow().lookup(shape, &key)?;
                        runtime
                            .heap
                            .get(closure, slot.index())
                            .and_then(|value| value.as_address())
                            .map(GcRef::from_address)
                    })
                });
                if let Some(prototype) = prototype {
                    with_runtime(|runtime| runtime.heap.set_prototype(target, Some(prototype)));
                }
            }
            // **`message` is not enumerable**, which `Object.keys(new Error("x"))` reports
            // and `JSON.stringify` copies. `name` is not set here at all: it belongs to the
            // prototype, where one string serves every instance of the kind.
            if Value::from_bits(message).kind() != crisol_value::Kind::Undefined {
                let text = to_text(message).unwrap_or_default();
                let held = new_string(&text);
                with_rooted(&[held], || {
                    with_runtime(|runtime| {
                        runtime.define_hidden(target, "message", Value::from_bits(held));
                    });
                });
            }
            receiver
        })
    })
}

/// `String(value)`.
extern "C" fn to_string_global(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    // **No argument is the empty string, not `"undefined"`.** An absent argument reads as
    // `undefined` and `String(undefined)` really is `"undefined"`, so the two cases have to
    // be told apart by the count — and they were not. `new String()` wrapped nine characters,
    // which gave it nine own properties and made it an array-like of letters.
    if argc == 0 {
        return with_rooted(&live, || {
            if let Some(handle) = handle_of(this_value) {
                let empty = new_string("");
                with_rooted(&[empty], || {
                    with_runtime(|runtime| {
                        runtime.define_hidden(handle, STRING_PRIMITIVE, Value::from_bits(empty));
                        runtime.define_hidden(handle, "length", Value::number(0.0));
                    });
                });
            }
            new_string("")
        });
    }
    with_rooted(&live, || match to_text(value) {
        Some(text) => {
            // Called with `new`, the receiver is a fresh object that will be the result, so
            // the text it wraps is recorded on it. Called plainly, the receiver is not an
            // object and this does nothing.
            if let Some(handle) = handle_of(this_value) {
                let wrapped = new_string(&text);
                with_runtime(|runtime| {
                    runtime.define_hidden(handle, STRING_PRIMITIVE, Value::from_bits(wrapped));
                });
                // **`length` has to be a real property here.** On a primitive it is answered by
                // the property load itself, which has a string cell to measure; a wrapper is an
                // ordinary object, so nothing would find it. Fixed at construction because the
                // text it wraps cannot change.
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a string this long cannot be allocated"
                )]
                let units = text.encode_utf16().count() as f64;
                with_runtime(|runtime| {
                    runtime.define_hidden(handle, "length", Value::number(units));
                });
            }
            new_string(&text)
        }
        // An object needs `ToPrimitive`, which calls user code.
        None => new_string("[object Object]"),
    })
}

/// `Number(value)`.
extern "C" fn to_number_global(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    // `Number()` with no argument is `0`, not `NaN`.
    if argc == 0 {
        return Value::number(0.0).to_bits();
    }
    // A `new Number(…)` wrapper records what it wraps, so its methods have a value to read
    // rather than coercing the wrapper and recursing (D-146).
    let number = to_number(value);
    if let Some(handle) = handle_of(this_value) {
        with_runtime(|runtime| {
            runtime.define_hidden(handle, STRING_PRIMITIVE, Value::number(number));
        });
    }
    from_number(number)
}

/// `Boolean(value)`.
extern "C" fn to_boolean_global(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    let truth = crisol_truthy(value);
    // A `new Boolean(…)` wrapper records what it wraps, as the number and string ones do —
    // **as a boolean**, not as one or zero. Stored as a number it was indistinguishable from a
    // `Number` wrapper, which is the only thing `Object.prototype.toString` has to tell them
    // apart by.
    if let Some(handle) = handle_of(this_value) {
        with_runtime(|runtime| {
            runtime.define_hidden(handle, STRING_PRIMITIVE, Value::from_bits(truth));
        });
    }
    truth
}

/// How many arguments each built-in declares.
///
/// **`Function.length` is a fixed number per method**, not a property of the implementation:
/// the count of parameters before the first with a default or a rest. test262 checks it for
/// every built-in it covers — 227 files do nothing else — and no built-in here had one at all,
/// so each of those failed on a method that was otherwise complete.
///
/// Keyed by owner as well as name, because one name disagrees with itself:
/// `Number.prototype.toString` takes a radix and every other `toString` takes nothing. A
/// method absent from this table gets no `length`, which is what it had before — a missing
/// answer rather than a wrong one.
///
/// The numbers were read out of test262 rather than recalled: 138 of these appear in a
/// `length.js` or an older `.length ===` assertion, and the rest are the specification's and
/// unambiguous. Transcribing them from memory would have put wrong numbers where there had
/// been none, which is worse than the gap.
const ARITIES: &[(&str, &str, u32)] = &[
    // The constructors themselves, owned by no object — `Array.length` is one, not the number
    // of arrays. Their arities come from the same reading.
    ("global", "Array", 1),
    ("global", "Boolean", 1),
    ("global", "Date", 7),
    ("global", "Error", 1),
    ("global", "Function", 1),
    ("global", "Map", 0),
    ("global", "Number", 1),
    ("global", "Object", 1),
    ("global", "RangeError", 1),
    ("global", "ReferenceError", 1),
    ("global", "RegExp", 2),
    ("global", "Set", 0),
    ("global", "String", 1),
    ("global", "Symbol", 0),
    ("global", "SyntaxError", 1),
    ("global", "TypeError", 1),
    ("global", "isFinite", 1),
    ("global", "isNaN", 1),
    ("global", "parseFloat", 1),
    ("global", "parseInt", 2),
    ("Array", "from", 1),
    ("Array", "isArray", 1),
    ("Array", "of", 0),
    ("Array.prototype", "at", 1),
    ("Array.prototype", "concat", 1),
    ("Array.prototype", "copyWithin", 2),
    ("Array.prototype", "entries", 0),
    ("Array.prototype", "every", 1),
    ("Array.prototype", "fill", 1),
    ("Array.prototype", "filter", 1),
    ("Array.prototype", "find", 1),
    ("Array.prototype", "findIndex", 1),
    ("Array.prototype", "findLast", 1),
    ("Array.prototype", "findLastIndex", 1),
    ("Array.prototype", "flat", 0),
    ("Array.prototype", "flatMap", 1),
    ("Array.prototype", "forEach", 1),
    ("Array.prototype", "includes", 1),
    ("Array.prototype", "indexOf", 1),
    ("Array.prototype", "join", 1),
    ("Array.prototype", "keys", 0),
    ("Array.prototype", "lastIndexOf", 1),
    ("Array.prototype", "map", 1),
    ("Array.prototype", "pop", 0),
    ("Array.prototype", "push", 1),
    ("Array.prototype", "reduce", 1),
    ("Array.prototype", "reduceRight", 1),
    ("Array.prototype", "reverse", 0),
    ("Array.prototype", "shift", 0),
    ("Array.prototype", "slice", 2),
    ("Array.prototype", "some", 1),
    ("Array.prototype", "sort", 1),
    ("Array.prototype", "splice", 2),
    ("Array.prototype", "toLocaleString", 0),
    ("Array.prototype", "toReversed", 0),
    ("Array.prototype", "toSorted", 1),
    ("Array.prototype", "toSpliced", 2),
    ("Array.prototype", "toString", 0),
    ("Array.prototype", "unshift", 1),
    ("Array.prototype", "values", 0),
    ("Array.prototype", "with", 2),
    ("Date", "UTC", 7),
    ("Date", "now", 0),
    ("Date", "parse", 1),
    ("Date.prototype", "getDate", 0),
    ("Date.prototype", "getDay", 0),
    ("Date.prototype", "getFullYear", 0),
    ("Date.prototype", "getHours", 0),
    ("Date.prototype", "getMilliseconds", 0),
    ("Date.prototype", "getMinutes", 0),
    ("Date.prototype", "getMonth", 0),
    ("Date.prototype", "getSeconds", 0),
    ("Date.prototype", "getTime", 0),
    ("Date.prototype", "getTimezoneOffset", 0),
    ("Date.prototype", "getUTCDate", 0),
    ("Date.prototype", "getUTCDay", 0),
    ("Date.prototype", "getUTCFullYear", 0),
    ("Date.prototype", "getUTCHours", 0),
    ("Date.prototype", "getUTCMilliseconds", 0),
    ("Date.prototype", "getUTCMinutes", 0),
    ("Date.prototype", "getUTCMonth", 0),
    ("Date.prototype", "getUTCSeconds", 0),
    ("Date.prototype", "setDate", 1),
    ("Date.prototype", "setFullYear", 3),
    ("Date.prototype", "setHours", 4),
    ("Date.prototype", "setMilliseconds", 1),
    ("Date.prototype", "setMinutes", 3),
    ("Date.prototype", "setMonth", 2),
    ("Date.prototype", "setSeconds", 2),
    ("Date.prototype", "setTime", 1),
    ("Date.prototype", "setUTCDate", 1),
    ("Date.prototype", "setUTCFullYear", 3),
    ("Date.prototype", "setUTCHours", 4),
    ("Date.prototype", "setUTCMilliseconds", 1),
    ("Date.prototype", "setUTCMinutes", 3),
    ("Date.prototype", "setUTCMonth", 2),
    ("Date.prototype", "setUTCSeconds", 2),
    ("Date.prototype", "toLocaleString", 0),
    ("Date.prototype", "toISOString", 0),
    ("Date.prototype", "toJSON", 1),
    ("Date.prototype", "toString", 0),
    ("Date.prototype", "toUTCString", 0),
    ("Date.prototype", "toDateString", 0),
    ("Date.prototype", "toTimeString", 0),
    ("Date.prototype", "toLocaleDateString", 0),
    ("Date.prototype", "toLocaleTimeString", 0),
    ("Date.prototype", "valueOf", 0),
    ("Function.prototype", "apply", 2),
    ("Function.prototype", "bind", 1),
    ("Function.prototype", "call", 1),
    ("JSON", "parse", 2),
    ("JSON", "stringify", 3),
    ("Map.prototype", "clear", 0),
    ("Map.prototype", "delete", 1),
    ("Map.prototype", "forEach", 1),
    ("Map.prototype", "get", 1),
    ("Map.prototype", "has", 1),
    ("Map.prototype", "set", 2),
    ("Math", "abs", 1),
    ("Math", "acos", 1),
    ("Math", "asin", 1),
    ("Math", "atan", 1),
    ("Math", "atan2", 2),
    ("Math", "cbrt", 1),
    ("Math", "ceil", 1),
    ("Math", "cos", 1),
    ("Math", "exp", 1),
    ("Math", "floor", 1),
    ("Math", "hypot", 2),
    ("Math", "log", 1),
    ("Math", "log10", 1),
    ("Math", "log2", 1),
    ("Math", "max", 2),
    ("Math", "min", 2),
    ("Math", "pow", 2),
    ("Math", "random", 0),
    ("Math", "round", 1),
    ("Math", "sign", 1),
    ("Math", "sin", 1),
    ("Math", "sqrt", 1),
    ("Math", "tan", 1),
    ("Math", "trunc", 1),
    ("Number", "isFinite", 1),
    ("Number", "isInteger", 1),
    ("Number", "isNaN", 1),
    ("Number", "isSafeInteger", 1),
    ("Number", "parseFloat", 1),
    ("Number", "parseInt", 2),
    ("Number.prototype", "toFixed", 1),
    ("Number.prototype", "toLocaleString", 0),
    ("Number.prototype", "toString", 1),
    ("Number.prototype", "valueOf", 0),
    ("Object", "assign", 2),
    ("Object", "create", 2),
    ("Object", "defineProperties", 2),
    ("Object", "defineProperty", 3),
    ("Object", "entries", 1),
    ("Object", "freeze", 1),
    ("Object", "fromEntries", 1),
    ("Object", "getOwnPropertyDescriptor", 2),
    ("Object", "getOwnPropertyDescriptors", 1),
    ("Object", "getOwnPropertyNames", 1),
    ("Object", "getOwnPropertySymbols", 1),
    ("Object", "getPrototypeOf", 1),
    ("Object", "groupBy", 2),
    ("Object", "hasOwn", 2),
    ("Object", "is", 2),
    ("Object", "isExtensible", 1),
    ("Object", "isFrozen", 1),
    ("Object", "isSealed", 1),
    ("Object", "keys", 1),
    ("Object", "preventExtensions", 1),
    ("Object", "seal", 1),
    ("Object", "setPrototypeOf", 2),
    ("Object", "values", 1),
    ("Object.prototype", "__defineGetter__", 2),
    ("Object.prototype", "__defineSetter__", 2),
    ("Object.prototype", "__lookupGetter__", 1),
    ("Object.prototype", "__lookupSetter__", 1),
    ("Object.prototype", "hasOwnProperty", 1),
    ("Object.prototype", "isPrototypeOf", 1),
    ("Object.prototype", "propertyIsEnumerable", 1),
    ("Object.prototype", "toLocaleString", 0),
    ("Object.prototype", "toString", 0),
    ("Object.prototype", "valueOf", 0),
    ("Promise", "all", 1),
    ("Promise", "allSettled", 1),
    ("Promise", "any", 1),
    ("Promise", "race", 1),
    ("Promise", "reject", 1),
    ("Promise", "resolve", 1),
    ("Promise.prototype", "catch", 1),
    ("Promise.prototype", "finally", 1),
    ("Promise.prototype", "then", 2),
    ("global", "Promise", 1),
    ("Proxy", "revocable", 2),
    ("global", "Proxy", 2),
    ("Reflect", "apply", 3),
    ("Reflect", "construct", 2),
    ("RegExp", "escape", 1),
    ("Map", "groupBy", 2),
    ("Error", "isError", 1),
    ("Reflect", "defineProperty", 3),
    ("Reflect", "deleteProperty", 2),
    ("Reflect", "get", 2),
    ("Reflect", "getOwnPropertyDescriptor", 2),
    ("Reflect", "getPrototypeOf", 1),
    ("Reflect", "has", 2),
    ("Reflect", "isExtensible", 1),
    ("Reflect", "ownKeys", 1),
    ("Reflect", "preventExtensions", 1),
    ("Reflect", "set", 3),
    ("Reflect", "setPrototypeOf", 2),
    ("RegExp.prototype", "exec", 1),
    ("RegExp.prototype", "test", 1),
    ("RegExp.prototype", "toString", 0),
    ("Set.prototype", "add", 1),
    ("Set.prototype", "clear", 0),
    ("Set.prototype", "delete", 1),
    ("Set.prototype", "forEach", 1),
    ("Set.prototype", "has", 1),
    ("String", "fromCharCode", 1),
    ("String", "fromCodePoint", 1),
    ("String.prototype", "at", 1),
    ("String.prototype", "charAt", 1),
    ("String.prototype", "charCodeAt", 1),
    ("String.prototype", "concat", 1),
    ("String.prototype", "endsWith", 1),
    ("String.prototype", "includes", 1),
    ("String.prototype", "indexOf", 1),
    ("String.prototype", "lastIndexOf", 1),
    ("String.prototype", "padEnd", 1),
    ("String.prototype", "padStart", 1),
    ("String.prototype", "repeat", 1),
    ("String.prototype", "replace", 2),
    ("String.prototype", "replaceAll", 2),
    ("String.prototype", "match", 1),
    ("String.prototype", "codePointAt", 1),
    ("String.prototype", "localeCompare", 1),
    ("String.prototype", "substr", 2),
    ("String.prototype", "isWellFormed", 0),
    ("String.prototype", "toWellFormed", 0),
    ("String.prototype", "search", 1),
    ("String.prototype", "slice", 2),
    ("String.prototype", "split", 2),
    ("String.prototype", "startsWith", 1),
    ("String.prototype", "substring", 2),
    ("String.prototype", "toLowerCase", 0),
    ("String.prototype", "toString", 0),
    ("String.prototype", "toUpperCase", 0),
    ("String.prototype", "trim", 0),
    ("String.prototype", "trimEnd", 0),
    ("String.prototype", "trimStart", 0),
    ("String.prototype", "valueOf", 0),
    ("Symbol", "for", 1),
    ("Symbol", "keyFor", 1),
];

/// The arity the specification gives `owner.name`, if it gives one.
fn arity_of(owner: &str, name: &str) -> Option<u32> {
    ARITIES
        .iter()
        .find(|(table, method, _)| *table == owner && *method == name)
        .map(|(_, _, arity)| *arity)
}

/// Built-ins reachable only as the body of a namespace object, not by any name.
///
/// Numbered last, after [`NATIVES`], [`GLOBAL_NATIVES`] and [`NAMESPACE_NATIVES`]. A table of
/// its own because the index space is shared: the first version of this pointed at index 0 of
/// the *first* table, so calling `Object()` ran `Array.prototype.map`.
const ANONYMOUS_NATIVES: &[Native] = &[
    construct_plain_object,
    bound_call,
    construct_array,
    error_to_text,
    promise_settle_call,
    proxy_revoke_call,
    combine_call,
    regexp_symbol_match,
    regexp_symbol_search,
    regexp_symbol_replace,
    regexp_symbol_split,
    iterator_self,
    string_iterator,
];

/// Where a `resolve`/`reject` function keeps the promise it settles.
const PROMISE_SETTLES: &str = "__settles";

/// Whether a settling function rejects rather than fulfils.
const PROMISE_REJECTS: &str = "__promiseRejects";

/// What internal slot zero holds on a promise.
///
/// A boolean rather than the `null` a proxy uses (see [`PROXY_MARKER`]): the two need telling
/// apart by one compare, and neither is a number, so neither reads as callable.
const PROMISE_MARKER: Value = Value::TRUE;

/// Internal slot holding a promise's state: pending, fulfilled or rejected.
const PROMISE_STATE_SLOT: u32 = 1;

/// Internal slot holding what it settled to.
const PROMISE_VALUE_SLOT: u32 = 2;

/// Internal slot holding the reactions waiting on it.
///
/// A JavaScript array, in groups of three: the handler, the promise the reaction settles, and
/// a flag word. **An array rather than a `Vec` beside the heap**, because internal slots are
/// traced (`Heap::reachable` walks them) — so everything a pending promise holds is freed
/// with the promise, and there is no side table to grow.
const PROMISE_REACTIONS_SLOT: u32 = 3;

/// A reaction's flag word: set when it runs on rejection.
const REACTION_ON_REJECTION: u32 = 1;

/// A reaction's flag word: set when the settlement passes through unchanged, as `finally`
/// needs — the handler runs for its effect and the original value survives it.
const REACTION_PASSTHROUGH: u32 = 2;

/// What a promise has settled to, if anything.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Settled {
    /// Not settled.
    Pending,
    /// Settled with a value.
    Fulfilled,
    /// Settled with a reason.
    Rejected,
}

/// One queued reaction: which handler, on what, settling which promise.
///
/// Every field is a JavaScript value or a flag, never a closure — which is what lets the
/// drain put the borrow down before calling the handler, and what lets the collector see
/// what the queue is holding (D-197).
struct PromiseJob {
    handler: u64,
    value: u64,
    derived: u64,
    rejected: bool,
    passthrough: bool,
}

/// `Error.prototype.toString` — `"name: message"`, or whichever of the two is there.
///
/// Reached through the chain, so `new TypeError("x").toString()` is `"TypeError: x"` with the
/// name coming from `TypeError.prototype` and the message from the instance. Errors inherited
/// `Object.prototype.toString` before this, which answered `[object Object]` for every one of
/// them.
extern "C" fn error_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if handle_of(this_value).is_none() {
        return raise("an error is an object", "TypeError");
    }
    let name = property_text(this_value, "name").unwrap_or_else(|| "Error".to_owned());
    let message = property_text(this_value, "message").unwrap_or_default();
    // Either being empty takes the separator with it, which is what makes `new Error()`
    // describe itself as `"Error"` rather than as `"Error: "`.
    new_string(&if name.is_empty() {
        message
    } else if message.is_empty() {
        name
    } else {
        format!("{name}: {message}")
    })
}

/// The index within [`ANONYMOUS_NATIVES`] of the plain-object constructor.
///
/// `Object()` answers a plain object. `Array()` has its own, because `Array(3)` is a
/// three-element array and `Array(1, 2)` is a two-element one — see [`CONSTRUCT_ARRAY`].
const CONSTRUCT_PLAIN_OBJECT: usize = 0;

/// The index within [`ANONYMOUS_NATIVES`] of the body every bound function runs.
const BOUND_CALL: usize = 1;

/// The index within [`ANONYMOUS_NATIVES`] of the array constructor.
const CONSTRUCT_ARRAY: usize = 2;

/// The index within [`ANONYMOUS_NATIVES`] of `Error.prototype.toString`.
const ERROR_TO_TEXT: usize = 3;

/// The index within [`ANONYMOUS_NATIVES`] of the body every `resolve`/`reject` pair runs.
const PROMISE_SETTLE_CALL: usize = 4;

/// The index within [`ANONYMOUS_NATIVES`] of the body a `revoke` function runs.
const PROXY_REVOKE_CALL: usize = 5;

/// The index within [`ANONYMOUS_NATIVES`] of the body every combinator reaction runs.
const COMBINE_CALL: usize = 6;

/// Where the symbol-keyed regular-expression methods begin in [`ANONYMOUS_NATIVES`].
///
/// A run rather than four constants, because they are installed by one loop over the names
/// they answer to and the loop needs the order to be the table's order.
const REGEXP_SYMBOL_METHODS: usize = 7;

/// The symbols those four answer to, in the order the table holds them.
const REGEXP_SYMBOL_NAMES: &[&str] = &["match", "search", "replace", "split"];

/// The index within [`ANONYMOUS_NATIVES`] of `%IteratorPrototype%[Symbol.iterator]`, which
/// answers its own receiver so that an iterator is itself iterable.
const ITERATOR_SELF: usize = 11;

/// The index within [`ANONYMOUS_NATIVES`] of `String.prototype[Symbol.iterator]`.
const STRING_ITERATOR: usize = 12;

/// Marks an array whose `length` has been made non-writable.
///
/// A length is derived from the element count rather than stored, so there is no slot to carry
/// its attributes — the flag has to live beside it.
const FIXED_LENGTH: &str = "__fixedLength";

/// The longest string this engine will build.
///
/// **Every engine has one**; the difference is whether it says so before or after trying. A
/// count comes from a program, so `"a".repeat(n)` is a request for `n` characters of memory
/// and `n` is whatever arithmetic produced it — the specification makes the limit
/// implementation-defined precisely so an engine can refuse rather than die.
///
/// This is V8's, which is the number everything in the wild is written against.
const MAX_STRING_UNITS: usize = (1 << 29) - 24;

/// The highest index stored as an array *element* rather than as a named property.
///
/// **Elements are dense and the specification's arrays are not.** Writing `a[4294967294] = 1`
/// is legal JavaScript and asks a dense store for four billion slots, which is not a slow
/// answer but a dead process. Four million keeps a worst case around thirty megabytes, which
/// is a real array somebody might build; past that the value becomes a named property, still
/// stored and still readable, but not counted by `length`.
const DENSE_ELEMENT_LIMIT: usize = 4_194_303;

/// `Array(…)` and `new Array(…)`.
///
/// **One number is a length and anything else is an element.** `Array(3)` is three empty slots
/// and `Array("3")` is one string — the single most surprising rule in the constructor, and the
/// reason `Array.of` exists to mean the other thing (D-150).
extern "C" fn construct_array(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if argc == 1 {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        let only = Value::from_bits(unsafe { argument(argc, argv, 0) });
        if let Some(length) = only.as_number() {
            if !length.is_finite()
                || length < 0.0
                || length.fract() != 0.0
                || length > 4_294_967_295.0
            {
                return raise("invalid array length", "RangeError");
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "range-checked immediately above"
            )]
            let length = length as usize;
            return with_new_array(length, |array| {
                // Filled with `undefined` where the specification says holes, which is the
                // approximation array literals already make (D-133).
                if length > 0 {
                    with_runtime(|runtime| {
                        runtime
                            .heap
                            .set_element(array, length - 1, Value::UNDEFINED);
                    });
                }
                array.to_value().to_bits()
            });
        }
    }
    let given: Vec<u64> = (0..argc as usize)
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        .map(|position| unsafe { argument(argc, argv, position) })
        .collect();
    with_rooted(&given, || array_of_values(&given))
}

/// Where a bound function keeps what it was bound to.
///
/// Hidden rather than internal for the same reason a date's time value is (D-126): internal
/// slot zero already means "callable", and a bound function is exactly a callable.
const BOUND_TARGET: &str = "__target";
/// The receiver a bound function supplies.
const BOUND_THIS: &str = "__boundThis";
/// The leading arguments a bound function supplies.
const BOUND_ARGS: &str = "__boundArgs";

/// What a namespace object does when called: give back an object.
extern "C" fn construct_plain_object(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    // **`Object(o) === o`.** Given something that is already an object, this is the identity —
    // it answered a fresh empty object instead, which is the same shape of answer and a
    // different object, so every identity test on it failed while every property test passed.
    if let Some(wrapped) = to_object(value) {
        return wrapped;
    }
    // `new Object()` already has a receiver; a plain call does not.
    if handle_of(this_value).is_some() {
        return this_value;
    }
    crisol_create_object()
}

/// `ToObject` — an object argument unchanged, a primitive wrapped, nullish refused.
///
/// The wrapper is built here rather than by calling `String`, `Number` or `Boolean`, because
/// those are reached through the globals and a program can replace them; `ToObject` is an
/// internal operation and must not be reroutable. What it stores is what those constructors
/// store, so a method reached through either wrapper reads the same primitive back.
fn to_object(value: u64) -> Option<u64> {
    let held = Value::from_bits(value);
    let prototype = match held.kind() {
        crisol_value::Kind::Object => return Some(value),
        crisol_value::Kind::Undefined | crisol_value::Kind::Null => return None,
        crisol_value::Kind::String => STRING_PROTOTYPE.with(std::cell::Cell::get),
        crisol_value::Kind::Number => NUMBER_PROTOTYPE.with(std::cell::Cell::get),
        crisol_value::Kind::Boolean => BOOLEAN_PROTOTYPE.with(std::cell::Cell::get),
        crisol_value::Kind::Symbol => SYMBOL_PROTOTYPE.with(std::cell::Cell::get),
    };
    // The primitive stays rooted across the allocation: a string and a symbol are heap cells,
    // and one held only in a Rust local while something else allocates is invisible.
    Some(with_rooted(&[value], || {
        let wrapper = crisol_create_object();
        with_rooted(&[wrapper, value], || {
            let Some(handle) = handle_of(wrapper) else {
                return;
            };
            with_runtime(|runtime| {
                runtime.heap.set_prototype(handle, prototype);
                runtime.define_hidden(handle, STRING_PRIMITIVE, held);
            });
            // A string wrapper's `length` is a real property, because nothing else would
            // find it — the same reason `String` defines one at construction.
            if let Some(text) = text_of(value) {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a string this long cannot be allocated"
                )]
                let units = text.encode_utf16().count() as f64;
                with_runtime(|runtime| {
                    runtime.define_hidden(handle, "length", Value::number(units));
                });
            }
        });
        wrapper
    }))
}

/// Methods that hang off a global object rather than being one.
///
/// Numbered after [`NATIVES`] and [`GLOBAL_NATIVES`], continuing the one negative index space
/// so `crisol_closure_code` still has a single rule.
/// Marks an object the `Error` constructors made.
///
/// **The tag, not the prototype chain, is what `[object Error]` means.** The specification
/// keys it on an internal slot the constructor installs, so `Object.create(Error.prototype)`
/// is `[object Object]` — inheritance is not the test, and using it would have been right
/// about every error and wrong about the one case that distinguishes the two.
const ERROR_DATA: &str = "__errorData";

/// Where a date keeps its time value.
///
/// **A hidden property standing in for an internal slot.** Internal slot zero already means
/// "this is callable" (see [`is_callable`]), so a date cannot use one without becoming a
/// function. The property is non-enumerable and non-configurable, so `Object.keys` and
/// `for-in` do not see it and a program cannot delete it — but it is still readable by name,
/// which a real internal slot would not be.
const DATE_TIME: &str = "__time";

/// Where a `new String(…)` wrapper keeps the text it wraps.
///
/// **A wrapper has to carry its own value.** Without it, a method reached through the wrapper
/// asks the object for text, which calls `String.prototype.toString`, which asks again — and
/// `new String("x").slice(0, 1)` overflows the stack instead of answering. Hidden for the same
/// reason a date's time value is (D-126): internal slot zero already means "callable".
const STRING_PRIMITIVE: &str = "__primitive";

/// Methods on `Date.prototype`.
///
/// **The local-time methods are the UTC ones.** There is no timezone database here, so
/// `getHours` and `getUTCHours` are the same function — correct exactly where the offset is
/// zero, and wrong by the offset everywhere else. `getTimezoneOffset` answers `0` for the
/// same reason, which at least makes the three consistent with each other.
const DATE_NATIVES: &[(&str, Native)] = &[
    ("getTime", date_get_time),
    ("valueOf", date_get_time),
    ("getFullYear", date_full_year),
    ("getUTCFullYear", date_full_year),
    ("getMonth", date_month),
    ("getUTCMonth", date_month),
    ("getDate", date_day_of_month),
    ("getUTCDate", date_day_of_month),
    ("getDay", date_week_day),
    ("getUTCDay", date_week_day),
    ("getHours", date_hours),
    ("getUTCHours", date_hours),
    ("getMinutes", date_minutes),
    ("getUTCMinutes", date_minutes),
    ("getSeconds", date_seconds),
    ("getUTCSeconds", date_seconds),
    ("getMilliseconds", date_milliseconds),
    ("getUTCMilliseconds", date_milliseconds),
    ("getTimezoneOffset", date_timezone_offset),
    ("toISOString", date_to_iso),
    ("toJSON", date_to_iso),
    ("toString", date_to_text),
    ("toLocaleString", date_to_text),
    ("toUTCString", date_to_utc_text),
    ("toGMTString", date_to_utc_text),
    ("toDateString", date_to_date_text),
    ("toTimeString", date_to_time_text),
    ("toLocaleDateString", date_to_date_text),
    ("toLocaleTimeString", date_to_time_text),
    ("setTime", date_set_time),
    ("setFullYear", date_set_full_year),
    ("setUTCFullYear", date_set_full_year),
    ("setMonth", date_set_month),
    ("setUTCMonth", date_set_month),
    ("setDate", date_set_day_of_month),
    ("setUTCDate", date_set_day_of_month),
    ("setHours", date_set_hours),
    ("setUTCHours", date_set_hours),
    ("setMinutes", date_set_minutes),
    ("setUTCMinutes", date_set_minutes),
    ("setSeconds", date_set_seconds),
    ("setUTCSeconds", date_set_seconds),
    ("setMilliseconds", date_set_milliseconds),
    ("setUTCMilliseconds", date_set_milliseconds),
];

/// `MakeDay` — a day number from a year, a **0-based** month and a **1-based** date.
///
/// **Everything rolls over rather than erroring**, which is the whole reason the setters can
/// be one operation: `setMonth(13)` moves the year and `setDate(0)` moves to the last day of
/// the previous month, and neither needs a special case.
fn make_day(year: f64, month: f64, day: f64) -> f64 {
    if !year.is_finite() || !month.is_finite() || !day.is_finite() {
        return f64::NAN;
    }
    let (year, month, day) = (year.trunc(), month.trunc(), day.trunc());
    let years = year + (month / 12.0).floor();
    // **Bounded before the conversion.** The civil arithmetic below is integer, and a year of
    // 1e20 would wrap rather than answer — silently, and into a plausible date. Anything this
    // far out is outside the range `time_clip` accepts, so `NaN` is the answer either way;
    // this is only about reaching it safely.
    if years.abs() > 400_000.0 {
        return f64::NAN;
    }
    let months = month.rem_euclid(12.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "both bounded immediately above"
    )]
    let days = crisol_builtins::days_from_civil(years as i64, months as i64, 1);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a day count from a year within 400000, far inside f64's exact-integer range"
    )]
    let days = days as f64;
    days + day - 1.0
}

/// Writes a date's time value and answers it, which is what every setter returns.
fn store_time(handle: GcRef, time: f64) -> u64 {
    with_runtime(|runtime| {
        runtime.define_hidden(handle, DATE_TIME, Value::number(time));
    });
    from_number(time)
}

/// Where in the broken-down fields each setter starts writing.
const DATE_YEAR: usize = 0;

/// The shared body of every `Date.prototype.set…` but `setTime`.
///
/// **They are one operation with a different starting field.** `setHours(h, m, s, ms)` writes
/// four of the seven and `setMinutes(m, s, ms)` writes three of the same four, so written
/// separately they are the same decompose-replace-recompose seven times over — with seven
/// chances to get the argument count subtly wrong in a way only one test notices.
///
/// **The arguments are coerced before the date is checked.** Coercion runs user code, and the
/// specification orders those effects before the answer — so an invalid date still calls the
/// `valueOf` it was handed. The first argument is coerced even when absent, which is why
/// `d.setHours()` yields an invalid date rather than leaving the date alone.
fn date_set(this_value: u64, argc: u64, argv: *const u64, first: usize, count: usize) -> u64 {
    // **The receiver is checked before a single argument is converted**, which is the
    // specification's order and is observable: `Date.prototype.setDate.call({}, o)` must
    // throw without ever reaching `o.valueOf`.
    if own_property(this_value, DATE_TIME).is_none() {
        return raise("not a date", "TypeError");
    }
    let supplied = (argc as usize).min(count).max(1);
    let mut given = [f64::NAN; 7];
    // **Rooted before the first conversion, not during it.** A conversion runs user code
    // that allocates, and the arguments live in the caller's frame slot, which nothing
    // scans (D-208) — so converting the first freed the second and third, and
    // `d.setHours(a, b, c)` reported that it could not convert an object to a primitive.
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    let converted = with_rooted(&live, || {
        for (index, slot) in given.iter_mut().enumerate().take(supplied) {
            // SAFETY: as above.
            let value = unsafe { argument(argc, argv, index) };
            // **`ToNumber`, which calls user code and may throw.** `to_number` answers `NaN`
            // for an object without asking it anything, so `d.setDate({valueOf: () => 3})`
            // set the date to `NaN` — and a `valueOf` that threw was swallowed. Every
            // argument is converted, in order, before any of them is used: the specification
            // says so, and it is observable whenever two of them have effects.
            *slot = coerce_number(value)?;
        }
        Ok(())
    });
    if let Err(thrown) = converted {
        return thrown;
    }
    // Re-read after the coercions, which run user code that can collect.
    let Some(handle) = handle_of(this_value) else {
        return raise("not a date", "TypeError");
    };

    let time = time_of(this_value);
    // **`setFullYear` starts from the epoch when the date is invalid**, and every other setter
    // answers `NaN`. That asymmetry is the specification's: a year is enough to name a date
    // and an hour is not.
    let base = if time.is_nan() {
        if first == DATE_YEAR {
            0.0
        } else {
            return store_time(handle, f64::NAN);
        }
    } else {
        time
    };
    let Some(fields) = crisol_builtins::fields(base) else {
        return store_time(handle, f64::NAN);
    };

    #[expect(clippy::cast_precision_loss, reason = "calendar fields")]
    let mut parts = [
        fields.year as f64,
        fields.month as f64,
        fields.day as f64,
        fields.hour as f64,
        fields.minute as f64,
        fields.second as f64,
        fields.millisecond as f64,
    ];
    for (index, value) in given.iter().enumerate().take(supplied) {
        parts[first + index] = *value;
    }
    let stamp = crisol_builtins::time_clip(crisol_builtins::make_date(
        make_day(parts[0], parts[1], parts[2]),
        crisol_builtins::make_time(parts[3], parts[4], parts[5], parts[6]),
    ));
    store_time(handle, stamp)
}

/// `Date.prototype.setTime` — the time value outright, with no calendar arithmetic at all.
extern "C" fn date_set_time(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if own_property(this_value, DATE_TIME).is_none() {
        return raise("not a date", "TypeError");
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    let given = match coerce_number(value) {
        Ok(number) => number,
        Err(thrown) => return thrown,
    };
    // Re-read after the coercion, which runs user code that can collect.
    let Some(handle) = handle_of(this_value) else {
        return raise("not a date", "TypeError");
    };
    store_time(handle, crisol_builtins::time_clip(given))
}

/// `Date.prototype.setFullYear` and its UTC twin.
///
/// The two are the same function because this engine has no local-time offset — see
/// `date_timezone_offset`, which answers zero.
extern "C" fn date_set_full_year(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    date_set(this_value, argc, argv, 0, 3)
}

/// `Date.prototype.setMonth`.
extern "C" fn date_set_month(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    date_set(this_value, argc, argv, 1, 2)
}

/// `Date.prototype.setDate`.
extern "C" fn date_set_day_of_month(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    date_set(this_value, argc, argv, 2, 1)
}

/// `Date.prototype.setHours`.
extern "C" fn date_set_hours(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    date_set(this_value, argc, argv, 3, 4)
}

/// `Date.prototype.setMinutes`.
extern "C" fn date_set_minutes(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    date_set(this_value, argc, argv, 4, 3)
}

/// `Date.prototype.setSeconds`.
extern "C" fn date_set_seconds(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    date_set(this_value, argc, argv, 5, 2)
}

/// `Date.prototype.setMilliseconds`.
extern "C" fn date_set_milliseconds(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    date_set(this_value, argc, argv, 6, 1)
}

/// The time value a date holds, or `NaN` if it is not a date.
fn time_of(this_value: u64) -> f64 {
    property_number(this_value, DATE_TIME).unwrap_or(f64::NAN)
}

/// The time value of `this`, or a `TypeError` when `this` is not a Date.
///
/// **`thisTimeValue` throws before it reads.** A Date getter on a non-Date receiver —
/// `Date.prototype.getFullYear.call({})`, or `.call(Date.prototype)` itself — is a `TypeError`,
/// not `NaN`: the two are different answers, and `NaN` is reserved for a real Date that holds
/// an invalid time. The mark is the hidden `__time` slot, which only a real Date carries.
fn require_time(this_value: u64) -> Result<f64, u64> {
    if own_property(this_value, DATE_TIME).is_none() {
        return Err(raise("this is not a Date", "TypeError"));
    }
    Ok(time_of(this_value))
}

/// One of the field readers, all of which answer `NaN` for an invalid date and throw for a
/// non-date receiver.
fn date_field(this_value: u64, read: impl FnOnce(&crisol_builtins::Fields) -> i64) -> u64 {
    let time = match require_time(this_value) {
        Ok(time) => time,
        Err(thrown) => return thrown,
    };
    crisol_builtins::fields(time).map_or_else(
        || from_number(f64::NAN),
        |fields| {
            #[expect(clippy::cast_precision_loss, reason = "a calendar field")]
            let value = read(&fields) as f64;
            from_number(value)
        },
    )
}

/// `Date.prototype.getTime` and `valueOf`.
extern "C" fn date_get_time(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match require_time(this_value) {
        Ok(time) => from_number(time),
        Err(thrown) => thrown,
    }
}

/// `Date.prototype.getFullYear`.
extern "C" fn date_full_year(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    date_field(this_value, |fields| fields.year)
}

/// `Date.prototype.getMonth`, which is **0-based**.
extern "C" fn date_month(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    date_field(this_value, |fields| fields.month)
}

/// `Date.prototype.getDate`, which is **1-based** — unlike `getMonth`, and unlike `getDay`.
extern "C" fn date_day_of_month(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    date_field(this_value, |fields| fields.day)
}

/// `Date.prototype.getDay` — the weekday, 0 for Sunday.
extern "C" fn date_week_day(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    date_field(this_value, |fields| i64::from(fields.week_day))
}

/// `Date.prototype.getHours`.
extern "C" fn date_hours(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    date_field(this_value, |fields| fields.hour)
}

/// `Date.prototype.getMinutes`.
extern "C" fn date_minutes(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    date_field(this_value, |fields| fields.minute)
}

/// `Date.prototype.getSeconds`.
extern "C" fn date_seconds(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    date_field(this_value, |fields| fields.second)
}

/// `Date.prototype.getMilliseconds`.
extern "C" fn date_milliseconds(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    date_field(this_value, |fields| fields.millisecond)
}

/// `Date.prototype.getTimezoneOffset`, which is always zero here.
extern "C" fn date_timezone_offset(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    // **Zero for a real date, `NaN` for an invalid one, and a throw for a non-date.** The
    // engine keeps one zone, UTC, so the offset is always zero — but it still has to answer
    // `NaN` when the time is `NaN` and reject a receiver that is not a date at all.
    match require_time(this_value) {
        Ok(time) if time.is_nan() => from_number(f64::NAN),
        Ok(_) => from_number(0.0),
        Err(thrown) => thrown,
    }
}

/// `Date.prototype.toISOString` and `toJSON`.
extern "C" fn date_to_iso(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    // **A non-date is a `TypeError`; an invalid date is a `RangeError`.** Two receivers, two
    // failures: `toISOString` has no spelling for an invalid date, where `toString` does.
    let time = match require_time(this_value) {
        Ok(time) => time,
        Err(thrown) => return thrown,
    };
    crisol_builtins::to_iso_string(time).map_or_else(
        || raise("this date cannot be represented as ISO text", "RangeError"),
        |text| new_string(&text),
    )
}

/// `Date.prototype.toString` — `"Thu Jan 01 1970 00:00:00 GMT+0000 (…)"`.
///
/// **Not the ISO form**, which is what it used to answer. The two are different methods with
/// different spellings and different failure modes, and a `toString` that printed
/// `1970-01-01T00:00:00.000Z` made `String(date)` disagree with every engine a program was
/// written against — including `Date.parse(String(d))`, which is how a program round-trips.
extern "C" fn date_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let time = match require_time(this_value) {
        Ok(time) => time,
        Err(thrown) => return thrown,
    };
    crisol_builtins::to_date_time_string(time).map_or_else(
        || new_string(crisol_builtins::INVALID_DATE),
        |text| new_string(&text),
    )
}

/// `Date.prototype.toUTCString` — `"Thu, 01 Jan 1970 00:00:00 GMT"`.
extern "C" fn date_to_utc_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let time = match require_time(this_value) {
        Ok(time) => time,
        Err(thrown) => return thrown,
    };
    crisol_builtins::to_utc_string(time).map_or_else(
        || new_string(crisol_builtins::INVALID_DATE),
        |text| new_string(&text),
    )
}

/// `Date.prototype.toDateString` — the date half of `toString`.
extern "C" fn date_to_date_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let time = match require_time(this_value) {
        Ok(time) => time,
        Err(thrown) => return thrown,
    };
    crisol_builtins::to_date_string(time).map_or_else(
        || new_string(crisol_builtins::INVALID_DATE),
        |text| new_string(&text),
    )
}

/// `Date.prototype.toTimeString` — the time half of `toString`.
extern "C" fn date_to_time_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let time = match require_time(this_value) {
        Ok(time) => time,
        Err(thrown) => return thrown,
    };
    crisol_builtins::to_time_string(time).map_or_else(
        || new_string(crisol_builtins::INVALID_DATE),
        |text| new_string(&text),
    )
}

/// Methods on `Map.prototype`.
///
/// **A map stores key and value adjacently** in one backing array, so an entry is a pair at an
/// even offset. One array rather than two keeps them from ever disagreeing about length.
const PROMISE_NATIVES: &[(&str, Native)] = &[
    ("then", promise_then_method),
    ("catch", promise_catch),
    ("finally", promise_finally),
];

/// `Promise.prototype.then`.
extern "C" fn promise_then_method(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if !is_promise(this_value) {
        return raise("`then` needs a promise", "TypeError");
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let on_fulfilled = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let on_rejected = unsafe { argument(argc, argv, 1) };
    // **A handler that is not callable is ignored, not an error.** `p.then(null)` is the
    // pass-through that makes a `.then` in the middle of a chain transparent.
    let on_fulfilled = if is_callable(on_fulfilled) {
        on_fulfilled
    } else {
        Value::UNDEFINED.to_bits()
    };
    let on_rejected = if is_callable(on_rejected) {
        on_rejected
    } else {
        Value::UNDEFINED.to_bits()
    };
    with_rooted(&[this_value, on_fulfilled, on_rejected], || {
        promise_then(this_value, on_fulfilled, on_rejected, false)
    })
}

/// `Promise.prototype.catch` — `then(undefined, handler)` and nothing else.
extern "C" fn promise_catch(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let handler = unsafe { argument(argc, argv, 0) };
    let arguments = [Value::UNDEFINED.to_bits(), handler];
    with_rooted(&arguments, || {
        promise_then_method(0, this_value, 0, 2, arguments.as_ptr())
    })
}

/// `Promise.prototype.finally`.
///
/// **The same handler on both sides**, and the settlement passes through unchanged — which is
/// the difference from `then(f, f)`, where the value a handler returns replaces it.
extern "C" fn promise_finally(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if !is_promise(this_value) {
        return raise("`finally` needs a promise", "TypeError");
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let handler = unsafe { argument(argc, argv, 0) };
    // **Registered, not run.** The first version called the handler here, which is `finally`
    // at the wrong time entirely: before the promise settles, and once rather than on
    // whichever way it goes. The pass-through flag is what carries both halves — the handler
    // runs for its effect and the original settlement survives it, which is the whole
    // difference from `then(f, f)`, where what the handler returns replaces the value.
    with_rooted(&[this_value, handler], || {
        promise_then(this_value, handler, handler, true)
    })
}

/// `new Promise(executor)`.
extern "C" fn make_promise(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    // **Rooted before the first allocation**, which every other native does and this one did
    // not. `new_promise_object` allocates, and under GC stress that collected the executor
    // still sitting in the argument buffer — so the closure never ran and the promise was
    // born pending with nothing to settle it. Silent without stress, and wrong with it.
    with_rooted(&live, || make_promise_rooted(argc, argv))
}

/// The body of [`make_promise`], with the arguments already rooted.
fn make_promise_rooted(argc: u64, argv: *const u64) -> u64 {
    // SAFETY: the caller guarantees `argc` readable values at `argv`.
    let executor = unsafe { argument(argc, argv, 0) };
    if !is_callable(executor) {
        return raise("a promise needs an executor function", "TypeError");
    }
    let promise = new_promise_object();
    with_rooted(&[promise, executor], || {
        let resolve = new_settling_function(promise, false);
        let reject = with_rooted(&[resolve], || new_settling_function(promise, true));
        // **A throw from the executor rejects the promise**, which is what lets
        // `new Promise(() => { throw x; })` be caught rather than escaping the constructor.
        let outcome = with_rooted(&[resolve, reject], || {
            call_value(executor, Value::UNDEFINED.to_bits(), &[resolve, reject])
        });
        if Value::from_bits(outcome).is_exception() {
            let reason = crisol_pending_exception();
            settle_promise(promise, reason, true);
        }
    });
    promise
}

/// `Promise.resolve(value)` — the value already settled, or the promise unchanged.
extern "C" fn promise_resolve(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    // **A promise is handed back as it is**, which is what makes `Promise.resolve` the way to
    // normalise something that may or may not be one.
    if is_promise(value) {
        return value;
    }
    let promise = with_rooted(&[value], new_promise_object);
    settle_promise(promise, value, false);
    promise
}

/// `Promise.reject(reason)`.
extern "C" fn promise_reject(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let reason = unsafe { argument(argc, argv, 0) };
    let promise = with_rooted(&[reason], new_promise_object);
    settle_promise(promise, reason, true);
    promise
}

/// Which way a combinator folds a list of promises.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Combine {
    /// `all` — every value, or the first rejection.
    All,
    /// `allSettled` — a report per entry, never rejecting.
    AllSettled,
    /// `race` — whichever settles first, either way.
    Race,
    /// `any` — the first fulfilment, or an error when none arrive.
    Any,
}

/// The shared body of `Promise.all`, `allSettled`, `race` and `any`.
///
/// **One walk with four endings.** Each reads the same list, attaches the same pair of
/// reactions to each entry, and differs only in what a settlement does to the shared counter
/// — so writing them separately is the same bookkeeping four times with four chances to get
/// the empty case wrong, which is exactly where they differ most.
fn combine_promises(argc: u64, argv: *const u64, how: Combine) -> u64 {
    // SAFETY: the caller guarantees `argc` readable values at `argv`.
    let list = unsafe { argument(argc, argv, 0) };
    let result = with_rooted(&[list], new_promise_object);
    with_rooted(&[list, result], || {
        let length = match indexed_length(list) {
            Ok(length) => length,
            Err(thrown) => return thrown,
        };
        // **The empty case is where they disagree.** `all` and `allSettled` are immediately
        // fulfilled with nothing, `any` is immediately rejected because no fulfilment can
        // ever arrive, and `race` stays pending for ever because nothing will settle it.
        if length == 0 {
            match how {
                Combine::All | Combine::AllSettled => {
                    let empty = crisol_create_array(0);
                    with_rooted(&[empty], || settle_promise(result, empty, false));
                }
                Combine::Any => {
                    let reason = raise_value("no promise was fulfilled", "TypeError");
                    with_rooted(&[reason], || settle_promise(result, reason, true));
                }
                Combine::Race => {}
            }
            return result;
        }

        // The collected values, and how many entries are still outstanding. Both live in
        // heap arrays so the collector sees them while the handlers run.
        let wanted = u64::try_from(length).unwrap_or(0);
        let values = with_rooted(&[result], || crisol_create_array(wanted));
        with_rooted(&[result, values, list], || {
            let pending = with_runtime(|runtime| {
                let scope = runtime.heap.scope();
                let shape = runtime.shapes.borrow().root();
                let cell = scope.alloc(shape, 0);
                runtime.heap.make_array(cell.handle(), 1);
                #[expect(clippy::cast_precision_loss, reason = "a list length")]
                let count = length as f64;
                runtime
                    .heap
                    .set_element(cell.handle(), 0, Value::number(count));
                cell.to_value().to_bits()
            });
            with_rooted(&[pending], || {
                for index in 0..length {
                    let entry = indexed_get(list, index);
                    let settled =
                        with_rooted(&[entry], || promise_resolve(0, 0, 0, 1, [entry].as_ptr()));
                    if Value::from_bits(settled).is_exception() {
                        return settled;
                    }
                    let state = CombineState {
                        result,
                        values,
                        pending,
                        index,
                        how,
                    };
                    with_rooted(
                        &[settled, state.result, state.values, state.pending],
                        || {
                            attach_combiner(settled, state);
                        },
                    );
                }
                result
            })
        })
    })
}

/// What one entry of a combinator needs to know when it settles.
#[derive(Clone, Copy)]
struct CombineState {
    result: u64,
    values: u64,
    pending: u64,
    index: usize,
    how: Combine,
}

/// Attaches the pair of reactions one entry of a combinator needs.
fn attach_combiner(entry: u64, state: CombineState) {
    let fulfil = new_combiner_function(state, false);
    let reject = with_rooted(&[fulfil], || new_combiner_function(state, true));
    with_rooted(&[entry, fulfil, reject], || {
        promise_then(entry, fulfil, reject, false);
    });
}

/// Where a combiner keeps the promise it is filling in.
const COMBINE_RESULT: &str = "__combineResult";
/// Where a combiner keeps the array of collected values.
const COMBINE_VALUES: &str = "__combineValues";
/// Where a combiner keeps the outstanding count.
const COMBINE_PENDING: &str = "__combinePending";
/// Where a combiner keeps its slot in the result, and which way it folds.
const COMBINE_INDEX: &str = "__combineIndex";
/// Where a combiner keeps whether it runs on rejection, and which combinator made it.
const COMBINE_SHAPE: &str = "__combineShape";

/// One reaction of a combinator, carrying everything it needs to fold a settlement in.
fn new_combiner_function(state: CombineState, rejects: bool) -> u64 {
    let function = with_runtime(|runtime| {
        runtime
            .native_function(
                NATIVES.len() + GLOBAL_NATIVES.len() + NAMESPACE_NATIVES.len() + COMBINE_CALL,
            )
            .to_value()
            .to_bits()
    });
    with_rooted(
        &[function, state.result, state.values, state.pending],
        || {
            let Some(handle) = handle_of(function) else {
                return;
            };
            // Stored one at a time: each `define_hidden` can transition the shape, and a value
            // held only in a Rust local while that happens is invisible (D-127).
            with_runtime(|runtime| {
                runtime.define_hidden(handle, COMBINE_RESULT, Value::from_bits(state.result));
            });
            with_runtime(|runtime| {
                runtime.define_hidden(handle, COMBINE_VALUES, Value::from_bits(state.values));
            });
            with_runtime(|runtime| {
                runtime.define_hidden(handle, COMBINE_PENDING, Value::from_bits(state.pending));
            });
            #[expect(clippy::cast_precision_loss, reason = "an index into a list")]
            let index = state.index as f64;
            with_runtime(|runtime| {
                runtime.define_hidden(handle, COMBINE_INDEX, Value::number(index));
            });
            let shape = f64::from(u8::from(rejects)) + 2.0 * f64::from(state.how as u8);
            with_runtime(|runtime| {
                runtime.define_hidden(handle, COMBINE_SHAPE, Value::number(shape));
            });
        },
    );
    function
}

/// The body every combinator reaction runs.
extern "C" fn combine_call(
    closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let read = |name: &str| own_property(closure, name).map(|(_, value)| value.to_bits());
    let (Some(result), Some(values), Some(pending)) = (
        read(COMBINE_RESULT),
        read(COMBINE_VALUES),
        read(COMBINE_PENDING),
    ) else {
        return Value::UNDEFINED.to_bits();
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "values this code wrote"
    )]
    let index = property_number(closure, COMBINE_INDEX).unwrap_or(0.0) as usize;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "as above"
    )]
    let shape = property_number(closure, COMBINE_SHAPE).unwrap_or(0.0) as u8;
    let rejects = shape & 1 != 0;
    let how = match shape >> 1 {
        1 => Combine::AllSettled,
        2 => Combine::Race,
        3 => Combine::Any,
        _ => Combine::All,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };

    with_rooted(&[result, values, pending, value], || {
        match (how, rejects) {
            // The first settlement of either kind wins, and the rest are ignored because a
            // settled promise never settles again.
            (Combine::Race, _) => settle_promise(result, value, rejects),
            (Combine::All, true) => settle_promise(result, value, true),
            (Combine::Any, false) => settle_promise(result, value, false),
            _ => {
                let recorded = if how == Combine::AllSettled {
                    combine_report(value, rejects)
                } else {
                    value
                };
                with_rooted(&[recorded], || {
                    if let Some(array) = handle_of(values) {
                        with_runtime(|runtime| {
                            runtime
                                .heap
                                .set_element(array, index, Value::from_bits(recorded));
                        });
                    }
                });
                // The last one in settles the result — a counter rather than a scan, so a
                // list of a thousand promises costs one decrement each rather than a thousand
                // checks each.
                let left = decrement_pending(pending);
                if left == 0 {
                    match how {
                        Combine::Any => {
                            let reason = raise_value("no promise was fulfilled", "TypeError");
                            with_rooted(&[reason], || settle_promise(result, reason, true));
                        }
                        _ => settle_promise(result, values, false),
                    }
                }
            }
        }
    });
    Value::UNDEFINED.to_bits()
}

/// One `allSettled` entry: `{status, value}` or `{status, reason}`.
fn combine_report(value: u64, rejected: bool) -> u64 {
    // A closure, not the function item: an `extern "C"` fn does not implement `FnOnce`.
    let report = with_rooted(&[value], || crisol_create_object());
    with_rooted(&[report, value], || {
        let Some(into) = handle_of(report) else {
            return;
        };
        let status = new_string(if rejected { "rejected" } else { "fulfilled" });
        with_rooted(&[status], || {
            with_runtime(|runtime| {
                runtime.define(into, "status", Value::from_bits(status));
            });
        });
        with_runtime(|runtime| {
            runtime.define(
                into,
                if rejected { "reason" } else { "value" },
                Value::from_bits(value),
            );
        });
    });
    report
}

/// Takes one off the outstanding count and answers what is left.
fn decrement_pending(pending: u64) -> usize {
    let Some(handle) = handle_of(pending) else {
        return 0;
    };
    with_runtime(|runtime| {
        let left = runtime
            .heap
            .element(handle, 0)
            .and_then(|value| value.as_number())
            .unwrap_or(0.0)
            - 1.0;
        runtime
            .heap
            .set_element(handle, 0, Value::number(left.max(0.0)));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a count this code wrote, clamped at zero"
        )]
        let left = left.max(0.0) as usize;
        left
    })
}

/// `Promise.all`.
extern "C" fn promise_all(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || combine_promises(argc, argv, Combine::All))
}

/// `Promise.allSettled`.
extern "C" fn promise_all_settled(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || combine_promises(argc, argv, Combine::AllSettled))
}

/// `Promise.race`.
extern "C" fn promise_race(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || combine_promises(argc, argv, Combine::Race))
}

/// `Promise.any`.
extern "C" fn promise_any(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || combine_promises(argc, argv, Combine::Any))
}

/// Runs queued reactions until there are none.
///
/// **No borrow is held across a handler**, which is the whole reason the queue holds data
/// rather than closures: the handler is JavaScript and can attach more reactions, settle
/// other promises, or throw.
///
/// Jobs queued *by* jobs run in the same drain, which is what "microtasks run to completion"
/// means — and why an endless `.then` chain starves rather than yielding. That is the
/// specified behaviour; the bound below is only so a test runner stops rather than hangs.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_run_microtasks(keep: u64) -> u64 {
    // **`keep` is rooted for the whole drain and handed back.** The entry point holds the
    // program's result in a C local that no stack map describes, and every job here can
    // allocate — so without this the value about to be printed is collected by the queue
    // that runs after the program returned.
    with_rooted(&[keep], drain_microtasks);
    keep
}

/// The drain itself, with whatever the caller needs kept already rooted.
fn drain_microtasks() {
    for _ in 0..MICROTASK_LIMIT {
        let Some(job) = PROMISE_JOBS.with(|jobs| jobs.borrow_mut().pop_front()) else {
            return;
        };
        if !is_callable(job.handler) {
            // No handler: the settlement passes through **as it was**. Passing a rejection on
            // as a fulfilment is what makes `p.then(f).catch(g)` never reach `g`.
            settle_promise(job.derived, job.value, job.rejected);
            continue;
        }
        let outcome = with_rooted(&[job.handler, job.value, job.derived], || {
            // **`finally`'s handler takes no argument and its answer is discarded**, which is
            // the difference from `then(f, f)` — there, what the handler returns replaces the
            // value, and `finally` must leave the settlement exactly as it found it.
            if job.passthrough {
                call_value(job.handler, Value::UNDEFINED.to_bits(), &[])
            } else {
                call_value(job.handler, Value::UNDEFINED.to_bits(), &[job.value])
            }
        });
        if Value::from_bits(outcome).is_exception() {
            // A throw from a `finally` handler *does* replace the settlement: that is the one
            // way it can change the outcome, and the specification keeps it.
            let reason = crisol_pending_exception();
            settle_promise(job.derived, reason, true);
            continue;
        }
        if job.passthrough {
            settle_promise(job.derived, job.value, job.rejected);
            continue;
        }
        // Returning a promise from a handler makes the derived one follow it, which costs an
        // extra tick — the adoption is itself a job.
        if is_promise(outcome) {
            adopt_promise(job.derived, outcome);
            continue;
        }
        settle_promise(job.derived, outcome, false);
    }
}

/// How many microtasks one drain runs before giving up.
///
/// The specification has no limit and an endless chain is specified to starve the loop. This
/// exists so a test runner reports rather than hangs; no terminating program reaches it.
const MICROTASK_LIMIT: usize = 1_000_000;

/// `String.prototype[Symbol.iterator]` — a string iterator over its code points.
///
/// **By code point, not code unit**, so an astral character is one step, not two — the same
/// walk `for (const c of s)` already takes through `crisol_iterate`'s fast path, but reachable
/// now as a method a program can call directly. A snapshot into an array reuses the array
/// iterator (D-232), so there is no new iterator type.
extern "C" fn string_iterator(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    with_rooted(&[this_value], || {
        let points: Vec<String> = text.chars().map(|point| point.to_string()).collect();
        let array = names_as_array(&points);
        with_rooted(&[array], || new_array_iterator(array, 1.0))
    })
}

/// `%IteratorPrototype%[Symbol.iterator]` — an iterator is its own iterable.
///
/// **This is what makes `Array.from(map.keys())` and `[...someIterator]` work.** `Array.from`
/// and spread ask their argument for `Symbol.iterator`; an iterator answers itself, so the
/// same drain loop that consumes a Map consumes the iterator a Map's `keys()` returns.
extern "C" fn iterator_self(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    this_value
}

/// A snapshot iterator over a Map or Set — its keys, values, or `[key, value]` entries.
///
/// **A snapshot, not a live view.** The specification iterates lazily, so a deletion made
/// during the loop is observed; this materialises the contents once into an array and hands
/// back an ordinary array iterator over it. Every ordinary use — `for (x of m)`, `[...m]`,
/// `Array.from(m.keys())` — sees the same result; only a program that mutates the collection
/// *while* iterating it can tell the difference (D-232). Reusing the array iterator is why this
/// needs no new prototype or dispatch entry.
///
/// `want`: 0 keys, 1 values, 2 entries.
fn collection_iterator(this_value: u64, is_map: bool, want: u8) -> u64 {
    let Some((array, length)) = entries_of(this_value) else {
        return new_array_iterator(crisol_create_array(0), 1.0);
    };
    let stride = if is_map { 2 } else { 1 };
    with_rooted(&[this_value], || {
        let mut items: Vec<u64> = Vec::new();
        let mut index = 0;
        while index < length {
            let key = element_at(array, index);
            let value = if is_map {
                element_at(array, index + 1)
            } else {
                key
            };
            let item = match want {
                0 => key,
                1 => value,
                // An entry is its own two-element array, which allocates — so the items
                // gathered so far, and the key and value, stay rooted across the build.
                _ => with_rooted(&items, || {
                    with_rooted(&[key, value], || array_of_values(&[key, value]))
                }),
            };
            items.push(item);
            index += stride;
        }
        // The array iterator walks a real array's *values*, so a keys/values/entries snapshot
        // is always read with kind 1.
        let snapshot = with_rooted(&items, || array_of_values(&items));
        with_rooted(&[snapshot], || new_array_iterator(snapshot, 1.0))
    })
}

/// `Map.prototype.keys`.
extern "C" fn map_keys(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_map(this_value) {
        return thrown;
    }
    collection_iterator(this_value, true, 0)
}

/// `Map.prototype.values`.
extern "C" fn map_values(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_map(this_value) {
        return thrown;
    }
    collection_iterator(this_value, true, 1)
}

/// `Map.prototype.entries`, and `Map.prototype[Symbol.iterator]`.
extern "C" fn map_entries(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_map(this_value) {
        return thrown;
    }
    collection_iterator(this_value, true, 2)
}

/// `Set.prototype.values`, which is also `keys` and `Set.prototype[Symbol.iterator]` — a set's
/// key *is* its value.
extern "C" fn set_values(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_set(this_value) {
        return thrown;
    }
    collection_iterator(this_value, false, 1)
}

/// `Set.prototype.entries`, whose entries are `[value, value]`.
extern "C" fn set_entries(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_set(this_value) {
        return thrown;
    }
    collection_iterator(this_value, false, 2)
}

const MAP_NATIVES: &[(&str, Native)] = &[
    ("get", map_get),
    ("set", map_set),
    ("has", map_has),
    ("delete", map_delete),
    ("clear", collection_clear),
    ("forEach", map_for_each),
    ("keys", map_keys),
    ("values", map_values),
    ("entries", map_entries),
];

/// Methods on `Set.prototype`.
const SET_NATIVES: &[(&str, Native)] = &[
    ("add", set_add),
    ("has", set_has),
    ("delete", set_delete),
    ("clear", collection_clear),
    ("forEach", set_for_each),
    ("keys", set_values),
    ("values", set_values),
    ("entries", set_entries),
];

/// Where `key` sits in the backing array, stepping by `stride`.
fn find_entry(array: GcRef, length: usize, stride: usize, key: u64) -> Option<usize> {
    (0..length)
        .step_by(stride)
        .find(|index| same_value_zero(element_at(array, *index), key))
}

/// `Map.prototype.get`.
extern "C" fn map_get(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_map(this_value) {
        return thrown;
    }
    let Some((array, length)) = entries_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 0) };
    // **`undefined` for a missing key**, which is indistinguishable from a key whose value is
    // `undefined` — that is what `has` is for, and why both exist.
    find_entry(array, length, 2, key).map_or_else(
        || Value::UNDEFINED.to_bits(),
        |at| element_at(array, at + 1),
    )
}

/// `Map.prototype.set`.
extern "C" fn map_set(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_map(this_value) {
        return thrown;
    }
    let Some((array, length)) = entries_of(this_value) else {
        return this_value;
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let value = unsafe { argument(argc, argv, 1) };

    let existing = find_entry(array, length, 2, key);
    with_runtime(|runtime| match existing {
        // **An existing key keeps its position.** Insertion order is observable through
        // `forEach`, and re-setting a key does not move it to the end.
        Some(at) => {
            runtime
                .heap
                .set_element(array, at + 1, Value::from_bits(value));
        }
        None => {
            runtime
                .heap
                .set_element(array, length, Value::from_bits(key));
            runtime
                .heap
                .set_element(array, length + 1, Value::from_bits(value));
        }
    });
    if existing.is_none() {
        set_collection_size(this_value, length + 2, 2);
    }
    // Answers the map, so `m.set(a, 1).set(b, 2)` chains.
    this_value
}

/// `Map.prototype.has`.
extern "C" fn map_has(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_map(this_value) {
        return thrown;
    }
    let Some((array, length)) = entries_of(this_value) else {
        return Value::FALSE.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 0) };
    boolean(find_entry(array, length, 2, key).is_some()).to_bits()
}

/// Removes the entry at `at`, closing the gap so insertion order survives.
fn remove_entry(collection: u64, array: GcRef, length: usize, at: usize, stride: usize) {
    with_runtime(|runtime| {
        for index in at..length - stride {
            let moved = runtime
                .heap
                .element(array, index + stride)
                .unwrap_or(Value::UNDEFINED);
            runtime.heap.set_element(array, index, moved);
        }
        runtime.heap.truncate_elements(array, length - stride);
    });
    set_collection_size(collection, length - stride, stride);
}

/// `Map.prototype.delete`.
extern "C" fn map_delete(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_map(this_value) {
        return thrown;
    }
    let Some((array, length)) = entries_of(this_value) else {
        return Value::FALSE.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 0) };
    let Some(at) = find_entry(array, length, 2, key) else {
        // **`false` for a key that was not there**, where `delete` on an object answers `true`.
        // The two operators are asking different questions.
        return Value::FALSE.to_bits();
    };
    remove_entry(this_value, array, length, at, 2);
    Value::TRUE.to_bits()
}

/// `Map.prototype.clear` and `Set.prototype.clear`.
extern "C" fn collection_clear(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_collection(this_value) {
        return thrown;
    }
    if let Some((array, _)) = entries_of(this_value) {
        with_runtime(|runtime| runtime.heap.truncate_elements(array, 0));
        set_collection_size(this_value, 0, 1);
    }
    Value::UNDEFINED.to_bits()
}

/// `Map.prototype.forEach`, which passes `(value, key, map)`.
///
/// **Value first, then key** — the opposite of how the pair is stored and of how most people
/// read it, and the specification's order.
extern "C" fn map_for_each(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_map(this_value) {
        return thrown;
    }
    let Some((array, _length)) = entries_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
    // **Checked before a single element is read.** `[1, 2].map(5)` throws
    // rather than calling nothing twice and answering `[undefined,
    // undefined]` — which is what reaching `crisol_not_a_function` per
    // element produced, and it looked like a working call every time.
    if !is_callable(callback) {
        return raise("a callback must be a function", "TypeError");
    }
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let mut index = 0;
        // `length` is re-read rather than captured, so a callback that deletes an entry does
        // not walk off the end of a shortened array.
        while index + 1 < entries_of(this_value).map_or(0, |(_, len)| len) {
            let key = element_at(array, index);
            let value = element_at(array, index + 1);
            let outcome = call_value(
                callback,
                Value::UNDEFINED.to_bits(),
                &[value, key, this_value],
            );
            if Value::from_bits(outcome).is_exception() {
                return outcome;
            }
            index += 2;
        }
        Value::UNDEFINED.to_bits()
    })
}

/// `Set.prototype.add`.
extern "C" fn set_add(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_set(this_value) {
        return thrown;
    }
    let Some((array, length)) = entries_of(this_value) else {
        return this_value;
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    if find_entry(array, length, 1, value).is_none() {
        with_runtime(|runtime| {
            runtime
                .heap
                .set_element(array, length, Value::from_bits(value));
        });
        set_collection_size(this_value, length + 1, 1);
    }
    this_value
}

/// `Set.prototype.has`.
extern "C" fn set_has(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_set(this_value) {
        return thrown;
    }
    let Some((array, length)) = entries_of(this_value) else {
        return Value::FALSE.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    boolean(find_entry(array, length, 1, value).is_some()).to_bits()
}

/// `Set.prototype.delete`.
extern "C" fn set_delete(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_set(this_value) {
        return thrown;
    }
    let Some((array, length)) = entries_of(this_value) else {
        return Value::FALSE.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    let Some(at) = find_entry(array, length, 1, value) else {
        return Value::FALSE.to_bits();
    };
    remove_entry(this_value, array, length, at, 1);
    Value::TRUE.to_bits()
}

/// `Set.prototype.forEach`, which passes `(value, value, set)`.
///
/// **The value twice**, so a callback written for a map's `(value, key)` works unchanged on a
/// set — the specification's reason, and it looks like a mistake until you know it.
extern "C" fn set_for_each(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = require_set(this_value) {
        return thrown;
    }
    let Some((array, _)) = entries_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
    // **Checked before a single element is read.** `[1, 2].map(5)` throws
    // rather than calling nothing twice and answering `[undefined,
    // undefined]` — which is what reaching `crisol_not_a_function` per
    // element produced, and it looked like a working call every time.
    if !is_callable(callback) {
        return raise("a callback must be a function", "TypeError");
    }
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let mut index = 0;
        while index < entries_of(this_value).map_or(0, |(_, len)| len) {
            let value = element_at(array, index);
            let outcome = call_value(
                callback,
                Value::UNDEFINED.to_bits(),
                &[value, value, this_value],
            );
            if Value::from_bits(outcome).is_exception() {
                return outcome;
            }
            index += 1;
        }
        Value::UNDEFINED.to_bits()
    })
}

/// Methods on `Object.prototype`, which every object inherits.
const OBJECT_NATIVES: &[(&str, Native)] = &[
    ("__defineGetter__", object_define_getter),
    ("__defineSetter__", object_define_setter),
    ("__lookupGetter__", object_lookup_getter),
    ("__lookupSetter__", object_lookup_setter),
    ("hasOwnProperty", object_has_own_property),
    ("propertyIsEnumerable", object_property_is_enumerable),
    ("isPrototypeOf", object_is_prototype_of),
    ("toString", object_to_text),
    ("toLocaleString", object_to_locale_text),
    ("valueOf", object_value_of),
];

/// `Object.prototype.toLocaleString` — **`this.toString()`, not `Object.prototype.toString`**.
///
/// It is a hook: the point of it is that an object overriding `toString` is localised through
/// that override. Pointing it straight at the default made every override invisible, which
/// looks right for a plain object and is wrong for every object that has one.
extern "C" fn object_to_locale_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = reject_nullish(this_value, "cannot convert") {
        return thrown;
    }
    let method = "toString".to_owned();
    // SAFETY: `method` is a live Rust string.
    let to_string =
        unsafe { crisol_property_load(this_value, method.as_ptr(), method.len() as u64) };
    if !is_callable(to_string) {
        return raise("toString is not a function", "TypeError");
    }
    call_value(to_string, this_value, &[])
}

/// `Object.prototype.__defineGetter__` and `__defineSetter__`.
///
/// **Older than `defineProperty` and still in use**, which is why they are here: they are the
/// only way a program written before ES5 could make an accessor, and test262 covers them.
/// Both route through `defineProperty` so the three cannot disagree about what an accessor is.
fn define_accessor(this_value: u64, argc: u64, argv: *const u64, as_getter: bool) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let function = unsafe { argument(argc, argv, 1) };
    if !is_callable(function) {
        return raise("an accessor needs a function", "TypeError");
    }
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let descriptor = crisol_create_object();
        with_rooted(&[descriptor, function], || {
            if let Some(into) = handle_of(descriptor) {
                with_runtime(|runtime| {
                    runtime.define(
                        into,
                        if as_getter { "get" } else { "set" },
                        Value::from_bits(function),
                    );
                    // **Enumerable and configurable**, which is what these two make and
                    // `defineProperty` does not — its defaults are the opposite.
                    runtime.define(into, "enumerable", Value::TRUE);
                    runtime.define(into, "configurable", Value::TRUE);
                });
            }
            let arguments = [this_value, key, descriptor];
            with_rooted(&arguments, || {
                object_define_property(0, 0, 0, 3, arguments.as_ptr())
            })
        })
    })
}

/// `{get x() {…}}` and `{set x(v) {…}}` — an accessor from a literal or a class body.
///
/// **Not a property holding a function.** A getter is *called* on read and a data property is
/// not, so lowering one as the other is a wrong answer rather than a missing feature:
/// `({get x() { return 1; }}).x` was the function itself, and the compiler said nothing.
///
/// Routed through `defineProperty` like `__defineGetter__` is, so the three ways of making an
/// accessor cannot disagree about what one is. A literal's accessor is **enumerable and
/// configurable**, which is what a literal makes and what `defineProperty`'s own defaults are
/// the opposite of.
///
/// # Safety
///
/// `key` must point to `length` readable UTF-8 bytes.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn crisol_define_accessor(
    object: u64,
    key: *const u8,
    length: u64,
    getter: u64,
    setter: u64,
) -> u64 {
    // SAFETY: the caller promises `length` readable UTF-8 bytes at `key`.
    let Some(name) = (unsafe { key_text(key, length) }) else {
        return Value::UNDEFINED.to_bits();
    };
    with_rooted(&[object, getter, setter], || {
        let descriptor = crisol_create_object();
        let named = with_rooted(&[descriptor, getter, setter], || new_string(&name));
        with_rooted(&[descriptor, getter, setter, named], || {
            let Some(into) = handle_of(descriptor) else {
                return Value::UNDEFINED.to_bits();
            };
            with_runtime(|runtime| {
                // **Only the half that was written.** `{get x() {…}}` has no setter, and a
                // descriptor carrying `set: undefined` says something different from one
                // carrying no `set` at all when the property is being redefined.
                if !Value::from_bits(getter).is_undefined() {
                    runtime.define(into, "get", Value::from_bits(getter));
                }
                if !Value::from_bits(setter).is_undefined() {
                    runtime.define(into, "set", Value::from_bits(setter));
                }
                runtime.define(into, "enumerable", Value::TRUE);
                runtime.define(into, "configurable", Value::TRUE);
            });
            let arguments = [object, named, descriptor];
            with_rooted(&arguments, || {
                object_define_property(0, 0, 0, 3, arguments.as_ptr())
            })
        })
    })
}

/// `Error.isError(value)`.
///
/// **Not `instanceof`.** An object from another realm, or one whose prototype has been
/// replaced, is still an error; `instanceof` answers the first question wrong and the second
/// one wrong in the other direction. This reads the mark the error was made with.
extern "C" fn error_is_error(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    let Some(handle) = handle_of(value) else {
        return Value::FALSE.to_bits();
    };
    if Value::from_bits(value).kind() != crisol_value::Kind::Object {
        return Value::FALSE.to_bits();
    }
    let marked = with_runtime(|runtime| {
        let key = PropertyKey::new(ERROR_DATA);
        runtime
            .heap
            .shape_of(handle)
            .and_then(|shape| runtime.shapes.borrow().lookup(shape, &key))
            .is_some()
    });
    boolean(marked).to_bits()
}

/// `Object.prototype.__lookupGetter__` and `__lookupSetter__`.
///
/// **Inherited, unlike `getOwnPropertyDescriptor`.** These walk the chain, which is the whole
/// reason they still exist beside it: they answer "what would reading this call", and the
/// answer can live on a prototype.
fn lookup_accessor(this_value: u64, argc: u64, argv: *const u64, as_getter: bool) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 0) };
    let Some(name) = to_text(key) else {
        return Value::UNDEFINED.to_bits();
    };
    let Some(handle) = handle_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    let key = PropertyKey::new(&name);
    let pair = with_runtime(|runtime| {
        let mut current = Some(handle);
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let object = current?;
            let shape = runtime.heap.shape_of(object)?;
            let found = runtime.shapes.borrow().lookup(shape, &key);
            if let Some(slot) = found
                && !runtime.heap.is_deleted(object, slot.index())
            {
                // Found, accessor or not — a data property shadows an inherited accessor, so
                // the walk stops here either way and answers `undefined` for the data case.
                return runtime
                    .heap
                    .attributes_of(object, slot.index())
                    .accessor
                    .then(|| runtime.heap.get(object, slot.index()))
                    .flatten()
                    .map(|pair| pair.to_bits());
            }
            current = runtime.heap.prototype_of(object);
        }
        None
    });
    pair.and_then(elements_of)
        .map_or(Value::UNDEFINED.to_bits(), |(functions, _)| {
            element_at(functions, usize::from(!as_getter))
        })
}

/// `Object.prototype.__lookupGetter__`.
extern "C" fn object_lookup_getter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    lookup_accessor(this_value, argc, argv, true)
}

/// `Object.prototype.__lookupSetter__`.
extern "C" fn object_lookup_setter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    lookup_accessor(this_value, argc, argv, false)
}

/// `Object.prototype.__defineGetter__`.
extern "C" fn object_define_getter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    define_accessor(this_value, argc, argv, true)
}

/// `Object.prototype.__defineSetter__`.
extern "C" fn object_define_setter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    define_accessor(this_value, argc, argv, false)
}

/// `Object.prototype.hasOwnProperty`.
///
/// **Own means own**: a property found on the prototype answers `false`, which is the whole
/// reason this exists rather than `key in object`.
/// `HasOwnProperty(object, key)` — own, and covering the array elements, `length` and string
/// characters that have no shape slot.
///
/// **The key may be a string, a number or a symbol.** `hasOwnProperty("0")` passes the string
/// `"0"`, which `as_index` (a number test) does not see — so the element check was skipped and
/// an array element read as absent, which is what made every `verifyProperty` test report
/// "N should be an own property" (D-237).
fn has_own_key(object: u64, key: u64) -> bool {
    if Value::from_bits(key).kind() == crisol_value::Kind::Symbol {
        return key_of(Value::from_bits(key))
            .is_some_and(|key| own_property_keyed(object, &key).is_some());
    }
    let Some(name) = to_text(key) else {
        return false;
    };
    own_property(object, &name).is_some() || derived_own_property(object, &name).is_some()
}

extern "C" fn object_has_own_property(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = reject_nullish(this_value, "cannot read a property") {
        return thrown;
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 0) };
    boolean(has_own_key(this_value, key)).to_bits()
}

/// `Object.prototype.propertyIsEnumerable`.
extern "C" fn object_property_is_enumerable(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = reject_nullish(this_value, "cannot read a property") {
        return thrown;
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(name) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return Value::FALSE.to_bits();
    };
    let Some(handle) = handle_of(this_value) else {
        return Value::FALSE.to_bits();
    };
    // An element has no slot to ask, so it answers through the derived descriptor — without
    // which `[1].propertyIsEnumerable(0)` was `false` about the one property it has.
    if own_property(this_value, &name).is_none() {
        return boolean(
            derived_own_property(this_value, &name)
                .is_some_and(|(_, attributes)| attributes.enumerable),
        )
        .to_bits();
    }
    let found = own_property(this_value, &name).is_some_and(|(slot, _)| {
        with_runtime(|runtime| runtime.heap.attributes_of(handle, slot).enumerable)
    });
    boolean(found).to_bits()
}

/// `Object.prototype.isPrototypeOf`.
extern "C" fn object_is_prototype_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = reject_nullish(this_value, "cannot ask about the prototype") {
        return thrown;
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let other = unsafe { argument(argc, argv, 0) };
    let Some(wanted) = handle_of(this_value) else {
        return Value::FALSE.to_bits();
    };
    let Some(mut current) = handle_of(other) else {
        return Value::FALSE.to_bits();
    };
    for _ in 0..PROTOTYPE_CHAIN_LIMIT {
        let Some(parent) = with_runtime(|runtime| runtime.heap.prototype_of(current)) else {
            return Value::FALSE.to_bits();
        };
        if parent == wanted {
            return Value::TRUE.to_bits();
        }
        current = parent;
    }
    Value::FALSE.to_bits()
}

/// `Object.prototype.toString`.
extern "C" fn object_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    // **`undefined` and `null` answer without a lookup**, and before `ToObject` — they have no
    // object to read `Symbol.toStringTag` from.
    let held = Value::from_bits(this_value);
    if held.kind() == crisol_value::Kind::Undefined {
        return new_string("[object Undefined]");
    }
    if held.kind() == crisol_value::Kind::Null {
        return new_string("[object Null]");
    }
    // The builtin tag, from the internal slot the receiver carries — the fallback when there is
    // no `Symbol.toStringTag`.
    let builtin = if elements_of(this_value).is_some() {
        "Array"
    } else {
        match held.kind() {
            // A primitive receiver is wrapped first, and the wrapper's class is what is
            // reported.
            crisol_value::Kind::Number => "Number",
            crisol_value::Kind::Boolean => "Boolean",
            crisol_value::Kind::String => "String",
            crisol_value::Kind::Symbol => "Object",
            _ => {
                if is_callable(this_value) {
                    "Function"
                } else if own_flag(this_value, DATE_TIME) {
                    "Date"
                } else if own_flag(this_value, ERROR_DATA) {
                    "Error"
                } else {
                    match own_property(this_value, STRING_PRIMITIVE).map(|(_, value)| value.kind())
                    {
                        Some(crisol_value::Kind::String) => "String",
                        Some(crisol_value::Kind::Number) => "Number",
                        Some(crisol_value::Kind::Boolean) => "Boolean",
                        _ => "Object",
                    }
                }
            }
        }
    };
    // **`Symbol.toStringTag` overrides the builtin tag when it is a string.** `Math`, `JSON` and
    // `Reflect` carry one, and a class may define one; a getter there may throw, which
    // propagates (D-233). D-149 is closed, so this lookup is now possible.
    if let Some(symbol) = well_known_symbol("toStringTag") {
        let tag = with_rooted(&[this_value], || crisol_computed_load(this_value, symbol));
        if Value::from_bits(tag).is_exception() {
            return tag;
        }
        if let Some(text) = text_of(tag) {
            return new_string(&format!("[object {text}]"));
        }
    }
    new_string(&format!("[object {builtin}]"))
}

/// `Object.prototype.valueOf`.
extern "C" fn object_value_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = reject_nullish(this_value, "cannot read the value") {
        return thrown;
    }
    this_value
}

/// `parseFloat(text)`.
///
/// **Reads a prefix and stops**, where `Number("12abc")` is `NaN`. That leniency is the whole
/// difference between them, and it is why `parseFloat` is the wrong tool for validating input.
extern "C" fn global_parse_float(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(text) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return from_number(f64::NAN);
    };
    let trimmed = text.trim_start();
    // The longest prefix that parses. Walking down from the whole string is not the fastest
    // way and is the one that cannot disagree with `f64`'s own parser about what it accepts.
    let mut end = trimmed.len();
    while end > 0 {
        if let Ok(value) = trimmed[..end].parse::<f64>() {
            return from_number(value);
        }
        end -= 1;
    }
    from_number(f64::NAN)
}

/// `parseInt(text, radix)`.
extern "C" fn global_parse_int(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(text) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return from_number(f64::NAN);
    };
    // SAFETY: as above.
    let asked = to_number(unsafe { argument(argc, argv, 1) });
    let mut body = text.trim_start();

    let negative = body.starts_with('-');
    if negative || body.starts_with('+') {
        body = &body[1..];
    }
    // **A leading `0x` means sixteen** unless a radix says otherwise, which is the rule that
    // makes `parseInt("0x10")` sixteen and `parseInt("0x10", 10)` zero.
    let mut radix = if asked.is_finite() && asked != 0.0 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked finite, and rejected below unless 2..=36"
        )]
        let given = asked as u32;
        given
    } else {
        10
    };
    if (radix == 16 || !asked.is_finite() || asked == 0.0)
        && (body.starts_with("0x") || body.starts_with("0X"))
    {
        body = &body[2..];
        radix = 16;
    }
    if !(2..=36).contains(&radix) {
        return from_number(f64::NAN);
    }

    // The longest prefix of digits valid in this radix, which is what makes `parseInt("12ab")`
    // twelve rather than `NaN`.
    let digits: String = body.chars().take_while(|c| c.is_digit(radix)).collect();
    if digits.is_empty() {
        return from_number(f64::NAN);
    }
    let mut value = 0.0_f64;
    for character in digits.chars() {
        let digit = character.to_digit(radix).unwrap_or(0);
        value = value.mul_add(f64::from(radix), f64::from(digit));
    }
    from_number(if negative { -value } else { value })
}

/// `isNaN(value)` — **coerces first**, unlike `Number.isNaN`.
extern "C" fn global_is_nan(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = to_number(unsafe { argument(argc, argv, 0) });
    boolean(value.is_nan()).to_bits()
}

/// `isFinite(value)` — **coerces first**, unlike `Number.isFinite`.
extern "C" fn global_is_finite(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = to_number(unsafe { argument(argc, argv, 0) });
    boolean(value.is_finite()).to_bits()
}

/// `Date.UTC(year, month, …)`.
///
/// **Not `new Date(…)` with the same arguments**, though they look alike: this answers a time
/// *value* rather than a date, and a single argument is a year rather than a time value.
extern "C" fn date_utc(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let part = |position: usize, fallback: f64| -> f64 {
        if (position as u64) < argc {
            // SAFETY: the position was just checked against `argc`.
            to_number(unsafe { argument(argc, argv, position) })
        } else {
            fallback
        }
    };
    let year = part(0, f64::NAN);
    let month = part(1, 0.0);
    let day = part(2, 1.0);
    if !year.is_finite() || !month.is_finite() || !day.is_finite() {
        return from_number(f64::NAN);
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "checked finite just above; TimeClip rejects anything out of range"
    )]
    let (year, month, day) = (year as i64, month as i64, day as i64);
    from_number(crisol_builtins::time_from_civil(
        year,
        month,
        day,
        part(3, 0.0),
        part(4, 0.0),
        part(5, 0.0),
        part(6, 0.0),
    ))
}

/// `Date.parse(text)`.
///
/// **Only the ISO form**, which is the one the specification actually requires an
/// implementation to accept. Everything else is implementation-defined, and answering `NaN` for
/// a format this does not read is within that — inventing a guess would not be.
extern "C" fn date_parse(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(text) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return from_number(f64::NAN);
    };
    from_number(parse_iso_date(&text).unwrap_or(f64::NAN))
}

/// Reads `YYYY-MM-DD` with an optional `THH:MM:SS.sssZ`.
fn parse_iso_date(text: &str) -> Option<f64> {
    let (date, time) = text.split_once('T').unwrap_or((text, ""));
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next().map_or(Some(1), |m| m.parse().ok())?;
    let day: i64 = parts.next().map_or(Some(1), |d| d.parse().ok())?;

    let clock = time.trim_end_matches('Z');
    let mut fields = clock.split(':');
    let hour: f64 = fields.next().map_or(Some(0.0), |h| {
        if h.is_empty() {
            Some(0.0)
        } else {
            h.parse().ok()
        }
    })?;
    let minute: f64 = fields.next().map_or(Some(0.0), |m| m.parse().ok())?;
    let second: f64 = fields.next().map_or(Some(0.0), |s| s.parse().ok())?;

    // **Months are 1-based in the text and 0-based in the time value**, which is the one
    // conversion this function exists to get right.
    Some(crisol_builtins::time_from_civil(
        year,
        month - 1,
        day,
        hour,
        minute,
        second.trunc(),
        (second.fract() * 1000.0).round(),
    ))
}

/// `Number.isInteger`.
///
/// **A whole number, not a number that looks whole after coercion.** `Number.isInteger("1")` is
/// false where `parseInt` would say one — these predicates do no conversion at all, which is
/// what separates them from the global `isNaN` and `isFinite`.
extern "C" fn number_is_integer(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = Value::from_bits(unsafe { argument(argc, argv, 0) });
    boolean(
        value
            .as_number()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0),
    )
    .to_bits()
}

/// `Number.isSafeInteger`.
extern "C" fn number_is_safe_integer(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = Value::from_bits(unsafe { argument(argc, argv, 0) });
    let safe = value.as_number().is_some_and(|n| {
        n.is_finite() && n.fract() == 0.0 && n.abs() <= crisol_builtins::MAX_SAFE_INTEGER
    });
    boolean(safe).to_bits()
}

/// `Number.isFinite` — no coercion, unlike the global.
extern "C" fn number_is_finite(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = Value::from_bits(unsafe { argument(argc, argv, 0) });
    boolean(value.as_number().is_some_and(f64::is_finite)).to_bits()
}

/// `Number.isNaN` — no coercion, unlike the global.
extern "C" fn number_is_nan(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = Value::from_bits(unsafe { argument(argc, argv, 0) });
    boolean(value.as_number().is_some_and(f64::is_nan)).to_bits()
}

/// `String.fromCharCode(…)` — code *units*, so a surrogate pair takes two arguments.
extern "C" fn string_from_char_code(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let units: Vec<u16> = (0..argc as usize)
        .map(|position| {
            // SAFETY: the convention guarantees `argc` readable values at `argv`.
            let value = to_number(unsafe { argument(argc, argv, position) });
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the specification truncates to a 16-bit code unit"
            )]
            let unit = (value as i64 as u64 & 0xFFFF) as u16;
            unit
        })
        .collect();
    new_string(&String::from_utf16_lossy(&units))
}

/// `String.fromCodePoint(…)` — whole code points, so an emoji takes one argument.
extern "C" fn string_from_code_point(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let mut out = String::new();
    for position in 0..argc as usize {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        let value = to_number(unsafe { argument(argc, argv, position) });
        if !value.is_finite() || value < 0.0 || value > 0x0010_FFFF as f64 {
            return raise("code point is out of range", "RangeError");
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "range-checked immediately above"
        )]
        let point = value as u32;
        match char::from_u32(point) {
            Some(character) => out.push(character),
            // **A lone surrogate is legal here and cannot be represented.** JavaScript strings
            // are UTF-16 and may hold an unpaired surrogate; these are Rust `String`s, which
            // are UTF-8 and cannot. Raising was wrong — the specification says this succeeds —
            // so the replacement character stands in, and the string is wrong in a way a test
            // can see rather than an error a program cannot expect. Fixing it properly means
            // WTF-8 or a UTF-16 rope, which is a representation change, not a patch.
            None => out.push('\u{FFFD}'),
        }
    }
    new_string(&out)
}

/// `Date.now()`.
extern "C" fn date_now(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(f64::NAN, |since| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "milliseconds since 1970 stay inside f64's exact-integer range for \
                          another quarter of a million years"
            )]
            let millis = since.as_millis() as f64;
            millis
        });
    from_number(now)
}

/// `Date(…)` and `new Date(…)`.
///
/// **No arguments is now, one is a time value, and more are calendar fields.** The three are
/// different enough that reading the count is the whole of the dispatch — and a missing
/// argument is not the same as `undefined` for the middle case, because `new Date(undefined)`
/// is an invalid date where `new Date()` is not.
extern "C" fn make_date_object(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let time = match argc {
        // No arguments is now, which is the same clock `Date.now` reads.
        0 => Value::from_bits(date_now(0, 0, 0, 0, std::ptr::null()))
            .as_number()
            .unwrap_or(f64::NAN),
        1 => {
            // SAFETY: the convention guarantees `argc` readable values at `argv`.
            let given = unsafe { argument(argc, argv, 0) };
            to_number(given)
        }
        _ => {
            let part = |position: usize, fallback: f64| -> f64 {
                if (position as u64) < argc {
                    // SAFETY: the position was just checked against `argc`.
                    to_number(unsafe { argument(argc, argv, position) })
                } else {
                    fallback
                }
            };
            let year = part(0, f64::NAN);
            let month = part(1, 0.0);
            let day = part(2, 1.0);
            // The calendar parts are whole numbers and the clock parts are not, which is the
            // signature `time_from_civil` has. A non-finite year makes the whole date invalid,
            // so it is checked rather than cast.
            if !year.is_finite() || !month.is_finite() || !day.is_finite() {
                f64::NAN
            } else {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "checked finite just above; TimeClip rejects anything out of range"
                )]
                let (year, month, day) = (year as i64, month as i64, day as i64);
                crisol_builtins::time_from_civil(
                    year,
                    month,
                    day,
                    part(3, 0.0),
                    part(4, 0.0),
                    part(5, 0.0),
                    part(6, 0.0),
                )
            }
        }
    };
    let time = crisol_builtins::time_clip(time);

    let object = crisol_create_object();
    with_rooted(&[object], || {
        if let Some(handle) = handle_of(object) {
            with_runtime(|runtime| {
                runtime.define_hidden(handle, DATE_TIME, Value::number(time));
                if let Some(prototype) = DATE_PROTOTYPE.with(std::cell::Cell::get) {
                    runtime.heap.set_prototype(handle, Some(prototype));
                }
            });
        }
    });
    object
}

/// Methods on `RegExp.prototype`.
const REGEXP_NATIVES: &[(&str, Native)] = &[
    ("test", regexp_test),
    ("exec", regexp_exec),
    ("toString", regexp_to_string),
];

/// Runs `body` against the pattern compiled from `source` and `flags`.
///
/// The cursor is loaded from the object's `lastIndex` before and stored back after, so the
/// property stays authoritative and a program that assigns to it is obeyed.
fn with_pattern<T>(
    this_value: u64,
    body: impl FnOnce(&mut crisol_builtins::JsRegExp) -> T,
) -> Option<T> {
    let source = property_text(this_value, "source")?;
    let flags_text = property_text(this_value, "flags")?;
    let cursor = property_number(this_value, "lastIndex").unwrap_or(0.0);

    let (outcome, moved) = PATTERNS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let key = (source.clone(), flags_text.clone());
        if !cache.contains_key(&key) {
            let flags = crisol_builtins::Flags::parse(&flags_text).ok()?;
            let compiled = crisol_builtins::JsRegExp::new(&source, flags).ok()?;
            cache.insert(key.clone(), compiled);
        }
        let pattern = cache.get_mut(&key)?;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a cursor past the end is handled by `exec`, and a negative one clamps"
        )]
        let start = cursor.max(0.0) as usize;
        pattern.set_last_index(start);
        let outcome = body(pattern);
        Some((outcome, pattern.last_index()))
    })?;

    // Written back through the ordinary path, so a `lastIndex` the program made non-writable
    // is honoured rather than bypassed.
    if let Some(handle) = handle_of(this_value) {
        let name = "lastIndex".to_owned();
        #[expect(clippy::cast_precision_loss, reason = "an index into a string")]
        let value = Value::number(moved as f64);
        with_runtime(|runtime| runtime.define(handle, &name, value));
    }
    Some(outcome)
}

/// A named property of `object`, as text, or `None` if it is not there.
///
/// **Absent has to be `None` and not `"undefined"`.** `to_text` spells every value out,
/// including `undefined`, so a caller asking whether a property exists would be told yes and
/// handed the word — which is how `new RegExp("ab+")` came to compile the pattern `undefined`.
fn property_text(object: u64, name: &str) -> Option<String> {
    let key = name.to_owned();
    // SAFETY: `key` is a live Rust string.
    let bits = unsafe { crisol_property_load(object, key.as_ptr(), key.len() as u64) };
    if Value::from_bits(bits).kind() == crisol_value::Kind::Undefined {
        return None;
    }
    to_text(bits)
}

/// A named property of `object`, as a number.
fn property_number(object: u64, name: &str) -> Option<f64> {
    let key = name.to_owned();
    // SAFETY: `key` is a live Rust string.
    let bits = unsafe { crisol_property_load(object, key.as_ptr(), key.len() as u64) };
    Value::from_bits(bits).as_number()
}

/// `RegExp.prototype.test`.
extern "C" fn regexp_test(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let text = to_text(unsafe { argument(argc, argv, 0) }).unwrap_or_default();
    let found = with_pattern(this_value, |pattern| pattern.test(&text)).unwrap_or(false);
    if found { Value::TRUE } else { Value::FALSE }.to_bits()
}

/// `RegExp.prototype.exec`.
///
/// **The result is an array with extra properties**, not a plain array: `index` and `input`
/// ride along with the matched text and its groups. A group that did not participate is
/// `undefined`, which is **not** the same as one that matched empty — the difference is why
/// the groups are stored one at a time rather than filtered.
extern "C" fn regexp_exec(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let text = to_text(unsafe { argument(argc, argv, 0) }).unwrap_or_default();
    let Some(Some(found)) = with_pattern(this_value, |pattern| pattern.exec(&text)) else {
        // **`null`, not `undefined`**, which is what `while ((m = re.exec(s)) !== null)` tests.
        return Value::NULL.to_bits();
    };

    // Rooted across the array's allocation: `this_value` reached here in a register and the
    // text came out of it.
    with_rooted(&[this_value], || match_result(&text, &found))
}

/// `RegExp.prototype.toString`.
extern "C" fn regexp_to_string(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let source = property_text(this_value, "source").unwrap_or_default();
    let flags = property_text(this_value, "flags").unwrap_or_default();
    new_string(&format!("/{source}/{flags}"))
}

/// Methods on `String.prototype`.
const STRING_NATIVES: &[(&str, Native)] = &[
    ("charAt", string_char_at),
    ("charCodeAt", string_char_code_at),
    ("indexOf", string_index_of),
    ("lastIndexOf", string_last_index_of),
    ("includes", string_includes),
    ("startsWith", string_starts_with),
    ("endsWith", string_ends_with),
    ("slice", string_slice),
    ("substring", string_substring),
    ("toUpperCase", string_to_upper),
    ("toLowerCase", string_to_lower),
    ("trim", string_trim),
    ("concat", string_concat),
    ("repeat", string_repeat),
    ("split", string_split),
    ("toString", string_to_string),
    ("valueOf", string_to_string),
    ("at", string_at),
    ("trimStart", string_trim_start),
    ("trimEnd", string_trim_end),
    ("padStart", string_pad_start),
    ("padEnd", string_pad_end),
    ("replace", string_replace),
    ("replaceAll", string_replace_all),
    ("match", string_match),
    ("search", string_search),
    ("codePointAt", string_code_point_at),
    ("localeCompare", string_locale_compare),
    ("substr", string_substr),
    ("isWellFormed", string_is_well_formed),
    ("toWellFormed", string_to_well_formed),
    ("toLocaleUpperCase", string_to_upper),
    ("toLocaleLowerCase", string_to_lower),
];

/// `String.prototype.at`.
///
/// **A negative index counts from the end and out of range is `undefined`**, where `charAt`
/// gives `""` — the two differ at exactly the place a caller is most likely to conflate them.
extern "C" fn string_at(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let wanted = match integer_argument(argc, argv, 0) {
        Ok(wanted) => wanted,
        Err(thrown) => return thrown,
    };
    #[expect(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
    let span = units.len() as f64;
    let resolved = if wanted < 0.0 { span + wanted } else { wanted };
    if resolved < 0.0 || resolved >= span {
        return Value::UNDEFINED.to_bits();
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked against both ends just above"
    )]
    let index = resolved as usize;
    units_between(&units, index, index + 1)
}

/// `String.prototype.trimStart`.
extern "C" fn string_trim_start(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match coercible_text(this_value) {
        Ok(text) => new_string(text.trim_start()),
        Err(thrown) => thrown,
    }
}

/// `String.prototype.trimEnd`.
extern "C" fn string_trim_end(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match coercible_text(this_value) {
        Ok(text) => new_string(text.trim_end()),
        Err(thrown) => thrown,
    }
}

/// `padStart` and `padEnd`, which differ only in which side the filling goes.
fn pad_with(this_value: u64, argc: u64, argv: *const u64, at_start: bool) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let target = match integer_argument(argc, argv, 0) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "a limit far below 2^53, compared rather than stored"
    )]
    let ceiling = MAX_STRING_UNITS as f64;
    if target > ceiling {
        return raise("padded length is out of range", "RangeError");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked against the ceiling just above"
    )]
    let target = target.max(0.0) as usize;
    if target <= units.len() {
        return new_string(&text);
    }
    // SAFETY: as above.
    let given = unsafe { argument(argc, argv, 1) };
    let filler = if Value::from_bits(given).kind() == crisol_value::Kind::Undefined {
        " ".to_owned()
    } else {
        to_text(given).unwrap_or_default()
    };
    // **An empty filler pads nothing**, and answering the original rather than looping is the
    // whole of why that case is checked.
    if filler.is_empty() {
        return new_string(&text);
    }

    let wanted = target - units.len();
    let filler_units = code_units(&filler);
    let mut padding: Vec<u16> = Vec::with_capacity(wanted);
    while padding.len() < wanted {
        let take = wanted - padding.len();
        padding.extend(filler_units.iter().take(take));
    }
    let padding = String::from_utf16_lossy(&padding);
    new_string(&if at_start {
        format!("{padding}{text}")
    } else {
        format!("{text}{padding}")
    })
}

/// `String.prototype.padStart`.
extern "C" fn string_pad_start(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    pad_with(this_value, argc, argv, true)
}

/// `String.prototype.padEnd`.
extern "C" fn string_pad_end(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    pad_with(this_value, argc, argv, false)
}

/// `replace` and `replaceAll`.
///
/// **A string pattern replaces the first occurrence and a global regular expression replaces
/// every one**, which is why the pattern's own flags decide rather than the method name — and
/// why `replaceAll` with a non-global regular expression is a `TypeError` rather than quietly
/// behaving like `replace`.
fn replace_with(this_value: u64, argc: u64, argv: *const u64, all: bool) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let pattern = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let replacement = unsafe { argument(argc, argv, 1) };

    // A regular expression pattern, recognised by its `source` rather than by a type tag.
    if property_text(pattern, "source").is_some() {
        let flags = property_text(pattern, "flags").unwrap_or_default();
        if all && !flags.contains('g') {
            return raise("replaceAll needs a global regular expression", "TypeError");
        }
        // Asked of the pattern, which is what the specification says — see `symbol_method`.
        let replacer = symbol_method(pattern, "replace");
        if is_callable(replacer) {
            let subject = with_rooted(&[pattern, replacer, replacement], || new_string(&text));
            return with_rooted(&[pattern, replacer, replacement, subject], || {
                call_value(replacer, pattern, &[subject, replacement])
            });
        }
        return with_rooted(&[this_value, pattern, replacement], || {
            replace_using_pattern(&text, pattern, replacement)
        });
    }

    let Some(needle) = to_text(pattern) else {
        return new_string(&text);
    };
    // A plain string pattern has no groups, so the same splice serves it — found by
    // searching rather than matching, and once unless this is `replaceAll`.
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = text.get(from..).and_then(|rest| rest.find(&needle)) {
        let start = from + offset;
        found.push(crisol_builtins::Captured {
            start,
            end: start + needle.len(),
            groups: Vec::new(),
        });
        if !all {
            break;
        }
        // **An empty needle advances by a character, not by nothing.** Without this,
        // `"ab".replaceAll("", "-")` never terminates.
        from = if needle.is_empty() {
            match text[start..].chars().next() {
                Some(character) => start + character.len_utf8(),
                None => break,
            }
        } else {
            start + needle.len()
        };
    }
    with_rooted(&[this_value, pattern, replacement], || {
        splice_matches(&text, &found, replacement)
    })
}

/// Every match of `rx` in `text`, replaced by what `replacement` says.
///
/// Shared by `String.prototype.replace` and `RegExp.prototype[Symbol.replace]`, which are the
/// same operation reached two ways — the string method is *defined* as asking the pattern.
fn replace_using_pattern(text: &str, rx: u64, replacement: u64) -> u64 {
    let Some(source) = property_text(rx, "source") else {
        return new_string(text);
    };
    let flags = property_text(rx, "flags").unwrap_or_default();
    let Ok(parsed) = crisol_builtins::Flags::parse(&flags) else {
        return new_string(text);
    };
    let Ok(mut compiled) = crisol_builtins::JsRegExp::new(&source, parsed) else {
        return new_string(text);
    };
    let found: Vec<crisol_builtins::Captured> = if flags.contains('g') {
        compiled.all_matches(text)
    } else {
        compiled.exec(text).into_iter().collect()
    };
    splice_matches(text, &found, replacement)
}

/// Rebuilds `text` with each match replaced by what `replacement` says.
///
/// **`replacement` may be a function**, and calling it is not an optimisation — a string
/// replacement cannot see the groups as values, so `s.replace(/(\d+)/, n => n * 2)` has no
/// spelling without it. Stringifying the function instead, which is what happened before,
/// substituted its own source text into the result.
fn splice_matches(text: &str, found: &[crisol_builtins::Captured], replacement: u64) -> u64 {
    let callable = is_callable(replacement);
    let template = if callable {
        String::new()
    } else {
        match to_text(replacement) {
            Some(template) => template,
            None => return new_string(text),
        }
    };
    let mut out = String::new();
    let mut cursor = 0;
    for capture in found {
        // Matches come back in order and cannot overlap, but a caller-supplied list could be
        // anything; skipping a match that starts behind the cursor keeps this total.
        if capture.start < cursor {
            continue;
        }
        out.push_str(text.get(cursor..capture.start).unwrap_or_default());
        if callable {
            let produced = call_replacer(text, capture, replacement);
            match produced {
                Ok(piece) => out.push_str(&piece),
                Err(thrown) => return thrown,
            }
        } else {
            expand_replacement(text, capture, &template, &mut out);
        }
        cursor = capture.end;
    }
    out.push_str(text.get(cursor..).unwrap_or_default());
    new_string(&out)
}

/// Calls a function replacement with `(matched, …groups, position, whole)`.
///
/// `position` is in **code units**, the space every other index in the language is in
/// (D-115) — handing over a byte offset reads correctly for ASCII and wrongly for the strings
/// that make the difference visible.
fn call_replacer(
    text: &str,
    capture: &crisol_builtins::Captured,
    replacement: u64,
) -> Result<String, u64> {
    let mut arguments = Vec::with_capacity(capture.groups.len() + 3);
    arguments.push(new_string(
        text.get(capture.start..capture.end).unwrap_or_default(),
    ));
    // Each argument is rooted as it is made: the one before it is held only by this `Vec`,
    // which the collector does not read, and the next one allocates.
    for group in &capture.groups {
        let value = with_rooted(&arguments, || match group {
            Some((start, end)) => new_string(text.get(*start..*end).unwrap_or_default()),
            None => Value::UNDEFINED.to_bits(),
        });
        arguments.push(value);
    }
    #[expect(clippy::cast_precision_loss, reason = "an index into a string")]
    let position = text
        .get(..capture.start)
        .unwrap_or_default()
        .encode_utf16()
        .count() as f64;
    arguments.push(Value::number(position).to_bits());
    let whole = with_rooted(&arguments, || new_string(text));
    arguments.push(whole);

    let produced = with_rooted(&arguments, || {
        call_value(replacement, Value::UNDEFINED.to_bits(), &arguments)
    });
    if Value::from_bits(produced).is_exception() {
        return Err(produced);
    }
    Ok(to_text(produced).unwrap_or_default())
}

/// `GetSubstitution` — the `$` patterns a string replacement may use.
///
/// **`$` is not an escape for the next character.** `$x` is two literal characters and `$&` is
/// the match, so a replacement built by concatenating user text can produce either by
/// accident; that is the language's design and not something to smooth over.
fn expand_replacement(
    text: &str,
    capture: &crisol_builtins::Captured,
    template: &str,
    out: &mut String,
) {
    let bytes = template.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'$' || at + 1 >= bytes.len() {
            // Pushed as a slice rather than a byte, so a multi-byte character survives.
            let character = template[at..].chars().next().unwrap_or('$');
            out.push(character);
            at += character.len_utf8();
            continue;
        }
        match bytes[at + 1] {
            b'$' => {
                out.push('$');
                at += 2;
            }
            b'&' => {
                out.push_str(text.get(capture.start..capture.end).unwrap_or_default());
                at += 2;
            }
            b'`' => {
                out.push_str(text.get(..capture.start).unwrap_or_default());
                at += 2;
            }
            b'\'' => {
                out.push_str(text.get(capture.end..).unwrap_or_default());
                at += 2;
            }
            b'0'..=b'9' => {
                // **Two digits are tried before one**, so `$12` is group twelve where there
                // are twelve and group one followed by `2` where there are not.
                let two = bytes
                    .get(at + 2)
                    .filter(|byte| byte.is_ascii_digit())
                    .map(|byte| usize::from(bytes[at + 1] - b'0') * 10 + usize::from(*byte - b'0'))
                    .filter(|index| *index >= 1 && *index <= capture.groups.len());
                let one = usize::from(bytes[at + 1] - b'0');
                if let Some(index) = two {
                    push_group(text, capture, index, out);
                    at += 3;
                } else if one >= 1 && one <= capture.groups.len() {
                    push_group(text, capture, one, out);
                    at += 2;
                } else {
                    // Not a group anybody has, so it stays as it was written.
                    out.push('$');
                    at += 1;
                }
            }
            _ => {
                out.push('$');
                at += 1;
            }
        }
    }
}

/// Appends group `index` (1-based), which contributes nothing when it did not participate.
fn push_group(text: &str, capture: &crisol_builtins::Captured, index: usize, out: &mut String) {
    if let Some(Some((start, end))) = capture.groups.get(index - 1) {
        out.push_str(text.get(*start..*end).unwrap_or_default());
    }
}

/// `String.prototype.replace`.
extern "C" fn string_replace(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    replace_with(this_value, argc, argv, false)
}

/// `String.prototype.replaceAll`.
extern "C" fn string_replace_all(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    replace_with(this_value, argc, argv, true)
}

/// A pattern argument compiled, whether it arrived as a regular expression or as text.
///
/// **A string is a pattern, not a literal.** `"a.c".match(".")` matches `"a"`, because the
/// argument is given to the `RegExp` constructor rather than searched for — which is the one
/// thing about `match` and `search` that a reader coming from `indexOf` gets wrong.
fn pattern_argument(value: u64) -> Option<(crisol_builtins::JsRegExp, String)> {
    let (source, flags_text) = match property_text(value, "source") {
        Some(source) => (source, property_text(value, "flags").unwrap_or_default()),
        // `"".match()` with nothing is the empty pattern, which matches at zero.
        None if Value::from_bits(value).is_undefined() => (String::new(), String::new()),
        None => (to_text(value)?, String::new()),
    };
    let flags = crisol_builtins::Flags::parse(&flags_text).ok()?;
    let compiled = crisol_builtins::JsRegExp::new(&source, flags).ok()?;
    Some((compiled, flags_text))
}

/// `GetMethod(value, @@name)` — the symbol-keyed method a pattern answers to, if it has one.
///
/// **This is what makes the string methods delegate.** `"a".match(p)` is defined as
/// `p[Symbol.match]("a")` whenever `p` has one, which is how a `RegExp` subclass changes what
/// every string method does; doing the work in the string method skips that entirely.
///
/// A plain string pattern has no such method, so it falls through to the built-in path — that
/// is the same rule, not an exception to it.
fn symbol_method(value: u64, name: &str) -> u64 {
    if handle_of(value).is_none() {
        return Value::UNDEFINED.to_bits();
    }
    let Some(symbol) = well_known_symbol(name) else {
        return Value::UNDEFINED.to_bits();
    };
    crisol_computed_load(value, symbol)
}

/// `RegExpExec(rx, S)` — the pattern's **own** `exec` when it has a callable one.
///
/// **A program may replace `exec`, and the specification says the replacement is used.** That
/// is the whole point of the `Symbol.*` protocol: `String.prototype.match` is defined to ask
/// the pattern, and the pattern is defined to ask `exec`, so a subclass that overrides one
/// changes what every string method does. Calling the built-in directly skips both hooks and
/// is indistinguishable from working until somebody overrides something.
fn regexp_exec_value(rx: u64, subject: u64) -> u64 {
    let exec = property_of(rx, "exec");
    if is_callable(exec) {
        let result = with_rooted(&[rx, exec, subject], || call_value(exec, rx, &[subject]));
        if Value::from_bits(result).is_exception() {
            return result;
        }
        let kind = Value::from_bits(result).kind();
        if kind != crisol_value::Kind::Object && kind != crisol_value::Kind::Null {
            return raise("exec must answer an object or null", "TypeError");
        }
        return result;
    }
    let arguments = [subject];
    with_rooted(&[rx, subject], || {
        regexp_exec(0, rx, 0, 1, arguments.as_ptr())
    })
}

/// Reads a pattern's `lastIndex`, which a program may have assigned.
fn last_index_of(rx: u64) -> f64 {
    property_number(rx, "lastIndex").unwrap_or(0.0)
}

/// Writes a pattern's `lastIndex` through the ordinary path, so one made non-writable is
/// honoured rather than bypassed.
fn set_last_index(rx: u64, to: f64) {
    if let Some(handle) = handle_of(rx) {
        with_runtime(|runtime| runtime.define(handle, "lastIndex", Value::number(to)));
    }
}

/// `RegExp.prototype[Symbol.match]`.
///
/// **Two shapes from one method.** A global pattern answers the matched text and nothing
/// else; a non-global one answers what `exec` answers, groups and `index` included. A program
/// written for one and handed the other reads `undefined` where it expected a group.
extern "C" fn regexp_symbol_match(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if handle_of(this_value).is_none() {
        return raise("this matcher needs a pattern", "TypeError");
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 0) };
    let subject = with_rooted(&[this_value, given], || {
        new_string(&to_text(given).unwrap_or_default())
    });
    with_rooted(&[this_value, subject], || {
        let global = is_truthy(Value::from_bits(property_of(this_value, "global")));
        if !global {
            return regexp_exec_value(this_value, subject);
        }
        set_last_index(this_value, 0.0);
        let mut collected: Vec<u64> = Vec::new();
        loop {
            let result = with_rooted(&collected, || regexp_exec_value(this_value, subject));
            if Value::from_bits(result).is_exception() {
                return result;
            }
            if Value::from_bits(result).kind() == crisol_value::Kind::Null {
                break;
            }
            let matched = with_rooted(&collected, || {
                let first = indexed_get(result, 0);
                to_text(first).unwrap_or_default()
            });
            let piece = with_rooted(&collected, || new_string(&matched));
            collected.push(piece);
            // **An empty match has to be stepped over by hand**, because it leaves
            // `lastIndex` where it was and the next call would find it again, for ever.
            if matched.is_empty() {
                set_last_index(this_value, last_index_of(this_value) + 1.0);
            }
        }
        // **No matches at all is `null`, not an empty array** — `if (s.match(/x/g))` is how a
        // program asks, and an empty array is truthy.
        if collected.is_empty() {
            return Value::NULL.to_bits();
        }
        with_rooted(&collected, || {
            with_new_array(collected.len(), |array| {
                for (index, piece) in collected.iter().enumerate() {
                    with_runtime(|runtime| {
                        runtime
                            .heap
                            .set_element(array, index, Value::from_bits(*piece));
                    });
                }
                array.to_value().to_bits()
            })
        })
    })
}

/// `RegExp.prototype[Symbol.search]` — where the first match starts, or `-1`.
///
/// **`lastIndex` is put back.** A `search` that moved the cursor would make the same call
/// answer differently the second time, which is the behaviour `exec` has by design and this
/// deliberately does not.
extern "C" fn regexp_symbol_search(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if handle_of(this_value).is_none() {
        return raise("this searcher needs a pattern", "TypeError");
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 0) };
    let subject = with_rooted(&[this_value, given], || {
        new_string(&to_text(given).unwrap_or_default())
    });
    with_rooted(&[this_value, subject], || {
        let previous = last_index_of(this_value);
        if previous != 0.0 {
            set_last_index(this_value, 0.0);
        }
        let result = regexp_exec_value(this_value, subject);
        if Value::from_bits(result).is_exception() {
            return result;
        }
        if last_index_of(this_value) != previous {
            set_last_index(this_value, previous);
        }
        if Value::from_bits(result).kind() == crisol_value::Kind::Null {
            return Value::number(-1.0).to_bits();
        }
        property_of(result, "index")
    })
}

/// `RegExp.prototype[Symbol.replace]`.
extern "C" fn regexp_symbol_replace(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if handle_of(this_value).is_none() {
        return raise("this replacer needs a pattern", "TypeError");
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let replacement = unsafe { argument(argc, argv, 1) };
    let Some(text) = with_rooted(&[this_value, given, replacement], || to_text(given)) else {
        return new_string("");
    };
    with_rooted(&[this_value, replacement], || {
        replace_using_pattern(&text, this_value, replacement)
    })
}

/// `RegExp.prototype[Symbol.split]`.
extern "C" fn regexp_symbol_split(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if handle_of(this_value).is_none() {
        return raise("this splitter needs a pattern", "TypeError");
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let limit = unsafe { argument(argc, argv, 1) };
    let cap = match split_limit(limit) {
        Ok(cap) => cap,
        Err(thrown) => return thrown,
    };
    let Some(text) = with_rooted(&[this_value, given], || to_text(given)) else {
        return one_piece_array("");
    };
    with_rooted(&[this_value], || {
        if cap == 0 {
            return with_new_array(0, |array| array.to_value().to_bits());
        }
        split_using_pattern(&text, this_value, cap)
    })
}

/// How many pieces a `split` may answer — `ToUint32(limit)`, or everything.
fn split_limit(limit: u64) -> Result<usize, u64> {
    if Value::from_bits(limit).is_undefined() {
        return Ok(usize::MAX);
    }
    let number = coerce_number(limit)?;
    if !number.is_finite() || number <= 0.0 {
        return Ok(0);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked finite and positive"
    )]
    let cap = number.min(f64::from(u32::MAX)) as usize;
    Ok(cap)
}

/// `String.prototype.codePointAt` — the whole code point, not half of a surrogate pair.
///
/// **This is what `charCodeAt` is not.** `"\u{1f4a9}".charCodeAt(0)` is the leading surrogate
/// and `codePointAt(0)` is the character, which is the difference between counting storage
/// and counting text.
extern "C" fn string_code_point_at(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let position = match integer_argument(argc, argv, 0) {
        Ok(position) => position,
        Err(thrown) => return thrown,
    };
    #[expect(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
    let span = units.len() as f64;
    if position < 0.0 || position >= span {
        return Value::UNDEFINED.to_bits();
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked against both ends just above"
    )]
    let at = position as usize;
    let first = units[at];
    // A leading surrogate followed by a trailing one is one character; anything else — an
    // unpaired surrogate included — is the unit itself, which is what the specification says
    // rather than an error.
    if (0xd800..0xdc00).contains(&first)
        && let Some(second) = units.get(at + 1).copied()
        && (0xdc00..0xe000).contains(&second)
    {
        let combined =
            0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
        return Value::number(f64::from(combined)).to_bits();
    }
    Value::number(f64::from(first)).to_bits()
}

/// `String.prototype.localeCompare`.
///
/// **Code-unit order, and the specification allows it.** A real collation is locale data this
/// engine does not carry; what the corpus checks is that the answer is consistent and
/// correctly signed, which this is. A wrong *order* for accented text is a worse answer than
/// no method at all only if somebody believes it is localised — hence this note.
extern "C" fn string_locale_compare(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    if let Some(thrown) = reject_nullish(this_value, "cannot compare") {
        return thrown;
    }
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let other = unsafe { argument(argc, argv, 0) };
    let Some(other) = to_text(other) else {
        return Value::number(0.0).to_bits();
    };
    let order = match code_units(&text).cmp(&code_units(&other)) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    };
    Value::number(order).to_bits()
}

/// `String.prototype.substr(start, length)`.
///
/// **Not `substring` and not `slice`.** The second argument is a *count*, and a negative start
/// counts from the end where `substring` would clamp it to zero — three methods that look
/// alike and disagree on every edge.
extern "C" fn string_substr(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let start = match integer_argument(argc, argv, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    #[expect(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
    let span = units.len() as f64;
    let from = if start < 0.0 {
        (span + start).max(0.0)
    } else {
        start.min(span)
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 1) };
    let count = if Value::from_bits(given).is_undefined() {
        span - from
    } else {
        match integer_argument(argc, argv, 1) {
            Ok(count) => count.clamp(0.0, span - from),
            Err(thrown) => return thrown,
        }
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "both clamped into 0..=length just above"
    )]
    let (from, count) = (from as usize, count as usize);
    new_string(&String::from_utf16_lossy(&units[from..from + count]))
}

/// `String.prototype.isWellFormed`.
///
/// **Always `true` here, and that is a property of the representation rather than an
/// optimisation.** A string is stored as Rust `str`, which is UTF-8 and cannot hold a lone
/// surrogate at all — so there is no ill-formed string for this to find. The honest answer is
/// this one; the alternative is a UTF-16 string type, which is a much larger change and the
/// place this would become interesting.
extern "C" fn string_is_well_formed(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = reject_nullish(this_value, "cannot inspect") {
        return thrown;
    }
    boolean(true).to_bits()
}

/// `String.prototype.toWellFormed` — the identity, for the reason above.
extern "C" fn string_to_well_formed(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some(thrown) = reject_nullish(this_value, "cannot convert") {
        return thrown;
    }
    this_text(this_value).map_or_else(|| new_string(""), |text| new_string(&text))
}

/// `String.prototype.match`.
///
/// **A global pattern answers differently**: a list of the matched text and nothing else,
/// where a non-global one answers what `exec` would — an array with the groups on it and an
/// `index`. Two shapes from one method, which is why this cannot simply loop.
extern "C" fn string_match(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let pattern = unsafe { argument(argc, argv, 0) };
    // **Asked of the pattern first**, which is what the specification says and what lets a
    // subclass or a plain object with a `Symbol.match` answer instead.
    let matcher = symbol_method(pattern, "match");
    if is_callable(matcher) {
        let subject = with_rooted(&[pattern, matcher], || new_string(&text));
        return with_rooted(&[pattern, matcher, subject], || {
            call_value(matcher, pattern, &[subject])
        });
    }
    let Some((mut compiled, flags)) = pattern_argument(pattern) else {
        return Value::NULL.to_bits();
    };
    if !flags.contains('g') {
        let Some(found) = compiled.exec(&text) else {
            return Value::NULL.to_bits();
        };
        return match_result(&text, &found);
    }
    let matched: Vec<String> = compiled
        .all_matches(&text)
        .into_iter()
        .map(|found| {
            text.get(found.start..found.end)
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    // **No matches at all is `null`, not an empty array** — `if (s.match(/x/g))` is how a
    // program asks, and an empty array is truthy.
    if matched.is_empty() {
        return Value::NULL.to_bits();
    }
    with_new_array(matched.len(), |array| {
        for (index, text) in matched.iter().enumerate() {
            let value = new_string(text);
            with_runtime(|runtime| {
                runtime
                    .heap
                    .set_element(array, index, Value::from_bits(value));
            });
        }
        array.to_value().to_bits()
    })
}

/// `String.prototype.search` — where the first match starts, or `-1`.
///
/// Does not read or write `lastIndex`: a `search` that moved the cursor would make the same
/// call answer differently the second time, which is the bug `exec` has by design and this
/// deliberately does not.
extern "C" fn string_search(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let pattern = unsafe { argument(argc, argv, 0) };
    let searcher = symbol_method(pattern, "search");
    if is_callable(searcher) {
        let subject = with_rooted(&[pattern, searcher], || new_string(&text));
        return with_rooted(&[pattern, searcher, subject], || {
            call_value(searcher, pattern, &[subject])
        });
    }
    let Some((mut compiled, _)) = pattern_argument(pattern) else {
        return Value::number(-1.0).to_bits();
    };
    let Some(found) = compiled.exec(&text) else {
        return Value::number(-1.0).to_bits();
    };
    // Byte offsets become code-unit offsets, which is the space every other index is in
    // (D-115).
    let prefix = text.get(..found.start).unwrap_or_default();
    #[expect(clippy::cast_precision_loss, reason = "an index into a string")]
    let index = prefix.encode_utf16().count() as f64;
    Value::number(index).to_bits()
}

/// One match as the array `exec` answers: the whole match, then the groups, then `index`.
///
/// Shared by `exec` and `match`, because a caller that compares the two — and test262 does,
/// repeatedly — is comparing objects rather than the text in them.
fn match_result(text: &str, found: &crisol_builtins::Captured) -> u64 {
    with_new_array(found.groups.len() + 1, |array| {
        let whole = new_string(text.get(found.start..found.end).unwrap_or_default());
        with_runtime(|runtime| {
            runtime.heap.set_element(array, 0, Value::from_bits(whole));
        });
        for (position, group) in found.groups.iter().enumerate() {
            let value = match group {
                Some((start, end)) => new_string(text.get(*start..*end).unwrap_or_default()),
                None => Value::UNDEFINED.to_bits(),
            };
            with_runtime(|runtime| {
                runtime
                    .heap
                    .set_element(array, position + 1, Value::from_bits(value));
            });
        }
        let prefix = text.get(..found.start).unwrap_or_default();
        #[expect(clippy::cast_precision_loss, reason = "an index into a string")]
        let index = Value::number(prefix.encode_utf16().count() as f64);
        let input = new_string(text);
        with_runtime(|runtime| {
            runtime.define(array, "index", index);
            runtime.define(array, "input", Value::from_bits(input));
        });
        array.to_value().to_bits()
    })
}

/// A string as the code units JavaScript counts.
///
/// **UTF-16, not bytes and not scalar values.** `length`, `charAt` and every index are in code
/// units, so an emoji is two positions and a `é` is one. Measuring bytes reads correctly for
/// ASCII and wrongly for everything else, which is the worst way to be wrong.
fn code_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// The receiver of a string method, as text.
/// `ToString(RequireObjectCoercible(this))` — a string method's receiver as text, or the throw.
///
/// **Two receivers are errors, not values.** `null` and `undefined` fail
/// `RequireObjectCoercible`, and a symbol fails `ToString`; both are a `TypeError`, where
/// [`this_text`] answered `"undefined"` for the first and empty for the second. Everything else
/// coerces, which is why `String.prototype.indexOf.call(5, …)` still works.
fn coercible_text(this_value: u64) -> Result<String, u64> {
    let held = Value::from_bits(this_value);
    if held.is_nullish() {
        return Err(raise(
            "a string method needs a receiver that is not null or undefined",
            "TypeError",
        ));
    }
    if held.kind() == crisol_value::Kind::Symbol {
        return Err(raise("a symbol is not a string", "TypeError"));
    }
    Ok(this_text(this_value).unwrap_or_default())
}

/// Whether `needle` sits at `haystack[at..]`, both in UTF-16 code units.
fn units_match_at(haystack: &[u16], needle: &[u16], at: usize) -> bool {
    haystack
        .get(at..at + needle.len())
        .is_some_and(|window| window == needle)
}

fn this_text(this_value: u64) -> Option<String> {
    // **An object receiver is read, not asked.** Asking would call `to_text`, which calls the
    // object's `toString`, which for a string wrapper is this function again — unbounded
    // recursion, and `new String("x").slice(0, 1)` overflowed the stack rather than answering.
    // A wrapper carries its text in a hidden property; anything else object-shaped has no text
    // to give.
    if handle_of(this_value).is_some()
        && Value::from_bits(this_value).kind() != crisol_value::Kind::String
    {
        return property_text(this_value, STRING_PRIMITIVE);
    }
    // `String.prototype.slice.call(5)` coerces, which is why this is `to_text` and not a
    // string-only read.
    to_text(this_value)
}

/// `String.prototype.charAt`.
extern "C" fn string_char_at(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let index = match integer_argument(argc, argv, 0) {
        Ok(index) => index,
        Err(thrown) => return thrown,
    };
    // **Out of range is the empty string, not `undefined`**, which is what distinguishes
    // `charAt` from indexing.
    if index < 0.0 || !index.is_finite() {
        return new_string("");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked non-negative and finite"
    )]
    let at = index as usize;
    units.get(at).map_or_else(
        || new_string(""),
        |unit| new_string(&String::from_utf16_lossy(&[*unit])),
    )
}

/// `String.prototype.charCodeAt`.
extern "C" fn string_char_code_at(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let index = match integer_argument(argc, argv, 0) {
        Ok(index) => index,
        Err(thrown) => return thrown,
    };
    if index < 0.0 || !index.is_finite() {
        return from_number(f64::NAN);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked non-negative and finite"
    )]
    let at = index as usize;
    // **Out of range is `NaN`**, where `charAt` gives the empty string — the pair disagree
    // deliberately.
    units.get(at).map_or_else(
        || from_number(f64::NAN),
        |unit| from_number(f64::from(*unit)),
    )
}

/// `String.prototype.indexOf`.
extern "C" fn string_index_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(needle) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return Value::number(-1.0).to_bits();
    };
    // Reported in code units, so the answer is an index into the same space `charAt` uses.
    match text.find(&needle) {
        Some(byte) => index_value(code_units(&text[..byte]).len()),
        None => Value::number(-1.0).to_bits(),
    }
}

/// `String.prototype.lastIndexOf`.
extern "C" fn string_last_index_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(needle) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return Value::number(-1.0).to_bits();
    };
    match text.rfind(&needle) {
        Some(byte) => index_value(code_units(&text[..byte]).len()),
        None => Value::number(-1.0).to_bits(),
    }
}

/// `String.prototype.includes`.
extern "C" fn string_includes(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(needle) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return Value::FALSE.to_bits();
    };
    // **The position is coerced, and a symbol there throws** — the same rule as any index.
    let from = match integer_argument(argc, argv, 1) {
        Ok(from) => from,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let needle = code_units(&needle);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped into 0..=len below"
    )]
    let start = (from.max(0.0) as usize).min(units.len());
    // Empty needle is found at any in-range position; otherwise scan from `start`.
    let found = needle.is_empty()
        || (start..=units.len().saturating_sub(needle.len()))
            .any(|at| units_match_at(&units, &needle, at));
    boolean(found).to_bits()
}

/// `String.prototype.startsWith`.
extern "C" fn string_starts_with(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(needle) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return Value::FALSE.to_bits();
    };
    let from = match integer_argument(argc, argv, 1) {
        Ok(from) => from,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let needle = code_units(&needle);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped into 0..=len below"
    )]
    let start = (from.max(0.0) as usize).min(units.len());
    boolean(units_match_at(&units, &needle, start)).to_bits()
}

/// `String.prototype.endsWith`.
extern "C" fn string_ends_with(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(needle) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return Value::FALSE.to_bits();
    };
    let units = code_units(&text);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let end_arg = unsafe { argument(argc, argv, 1) };
    // **The end position defaults to the length, and is where the match must finish** — which
    // is what `"the future".endsWith("future", 10)` needs and a plain `ends_with` ignores.
    let end = if Value::from_bits(end_arg).is_undefined() {
        units.len()
    } else {
        let asked = match integer_argument(argc, argv, 1) {
            Ok(asked) => asked,
            Err(thrown) => return thrown,
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=len below"
        )]
        let end = (asked.max(0.0) as usize).min(units.len());
        end
    };
    let needle = code_units(&needle);
    let found = needle.len() <= end && units_match_at(&units, &needle, end - needle.len());
    boolean(found).to_bits()
}

/// The text between two code-unit positions.
fn units_between(units: &[u16], start: usize, end: usize) -> u64 {
    let slice = units.get(start..end.max(start)).unwrap_or(&[]);
    new_string(&String::from_utf16_lossy(slice))
}

/// `String.prototype.slice`.
extern "C" fn string_slice(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = match relative_index(unsafe { argument(argc, argv, 0) }, units.len(), 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let end = match relative_index(unsafe { argument(argc, argv, 1) }, units.len(), units.len()) {
        Ok(end) => end,
        Err(thrown) => return thrown,
    };
    units_between(&units, start, end)
}

/// `String.prototype.substring`.
///
/// **Unlike `slice` a negative index clamps to zero rather than counting from the end**, and
/// the two arguments swap if they are the wrong way round. Sharing an implementation with
/// `slice` would get both wrong.
extern "C" fn string_substring(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let units = code_units(&text);
    let clamp = |bits: u64, fallback: usize| -> usize {
        let Some(number) = Value::from_bits(bits).as_number() else {
            return fallback;
        };
        if number.is_nan() || number < 0.0 {
            return 0;
        }
        #[expect(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
        let span = units.len() as f64;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=len just above"
        )]
        let index = number.min(span) as usize;
        index
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let first = clamp(unsafe { argument(argc, argv, 0) }, 0);
    // SAFETY: as above.
    let second = clamp(unsafe { argument(argc, argv, 1) }, units.len());
    units_between(&units, first.min(second), first.max(second))
}

/// `String.prototype.toUpperCase`.
extern "C" fn string_to_upper(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match coercible_text(this_value) {
        Ok(text) => new_string(&text.to_uppercase()),
        Err(thrown) => thrown,
    }
}

/// `String.prototype.toLowerCase`.
extern "C" fn string_to_lower(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match coercible_text(this_value) {
        Ok(text) => new_string(&text.to_lowercase()),
        Err(thrown) => thrown,
    }
}

/// `String.prototype.trim`.
extern "C" fn string_trim(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    match coercible_text(this_value) {
        Ok(text) => new_string(text.trim()),
        Err(thrown) => thrown,
    }
}

/// `String.prototype.concat`.
extern "C" fn string_concat(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let mut out = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    for position in 0..argc as usize {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        let piece = unsafe { argument(argc, argv, position) };
        out.push_str(&to_text(piece).unwrap_or_default());
    }
    new_string(&out)
}

/// `String.prototype.repeat`.
extern "C" fn string_repeat(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    let count = match integer_argument(argc, argv, 0) {
        Ok(count) => count,
        Err(thrown) => return thrown,
    };
    // A negative or infinite count is a `RangeError`, which is worth raising rather than
    // silently producing an empty string that reads like a legitimate answer.
    if count < 0.0 || !count.is_finite() {
        return raise("repeat count is out of range", "RangeError");
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a limit far below 2^53, compared rather than stored"
    )]
    let ceiling = MAX_STRING_UNITS as f64;
    // **A length no string can have is an error, not an attempt.** Until the count was
    // actually coerced this was unreachable from a string argument — `"a".repeat("1e9")`
    // read as zero — and coercing it turned a silent wrong answer into a real request for a
    // gigabyte. Every engine has this limit; the difference is whether it says so.
    if count * index_as_f64(text.len().max(1)) > ceiling {
        return raise("repeat count is out of range", "RangeError");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked against the ceiling just above"
    )]
    let times = count as usize;
    new_string(&text.repeat(times))
}

/// `String.prototype.split`.
extern "C" fn string_split(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let text = match coercible_text(this_value) {
        Ok(text) => text,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let limit = unsafe { argument(argc, argv, 1) };
    let cap = match split_limit(limit) {
        Ok(cap) => cap,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        if cap == 0 {
            return with_new_array(0, |array| array.to_value().to_bits());
        }
        // **A regular expression separator, recognised by its `source`.** Without this the
        // pattern went through `ToString` and `"a1b".split(/[0-9]/)` looked for the literal
        // text `/[0-9]/` — which is never there, so it answered the whole string and looked
        // like a working call.
        if property_text(given, "source").is_some() {
            let splitter = symbol_method(given, "split");
            if is_callable(splitter) {
                let subject = with_rooted(&[given, splitter], || new_string(&text));
                return with_rooted(&[given, splitter, subject], || {
                    call_value(splitter, given, &[subject, limit])
                });
            }
            return split_using_pattern(&text, given, cap);
        }

        let pieces: Vec<Option<String>> = match to_text(given) {
            // **An empty separator splits into characters**, and no separator at all gives a
            // one-element array holding the whole string — not an empty one.
            Some(separator) if separator.is_empty() => {
                text.chars().map(|c| Some(c.to_string())).collect()
            }
            Some(separator) => text
                .split(&separator)
                .map(|piece| Some(piece.to_owned()))
                .collect(),
            None => vec![Some(text.clone())],
        };
        let mut pieces = pieces;
        pieces.truncate(cap);
        pieces_array(&pieces)
    })
}

/// `text` split on every match of `rx`, at most `cap` pieces.
///
/// Shared by `String.prototype.split` and `RegExp.prototype[Symbol.split]`, which are the
/// same operation reached two ways.
fn split_using_pattern(text: &str, rx: u64, cap: usize) -> u64 {
    let Some(source) = property_text(rx, "source") else {
        return one_piece_array(text);
    };
    let flags = property_text(rx, "flags").unwrap_or_default();
    let Ok(parsed) = crisol_builtins::Flags::parse(&flags) else {
        return one_piece_array(text);
    };
    let Ok(mut compiled) = crisol_builtins::JsRegExp::new(&source, parsed) else {
        return one_piece_array(text);
    };
    // **An empty subject is decided by whether the pattern matches it**, not by the walk:
    // `"".split(/x/)` is `[""]` and `"".split(/(?:)/)` is `[]`, and the loop below cannot
    // tell those apart because it never runs.
    if text.is_empty() {
        if compiled.exec("").is_some() {
            return with_new_array(0, |array| array.to_value().to_bits());
        }
        return one_piece_array(text);
    }
    let mut pieces: Vec<Option<String>> = Vec::new();
    let mut cursor = 0;
    for found in compiled.all_matches(text) {
        // A match starting at the end is past the last position the specification looks at,
        // and a zero-width one where the cursor already is contributes nothing — without
        // both, `"ab".split(/(?:)/)` gains a trailing `""`.
        if found.start >= text.len() {
            break;
        }
        if found.end == cursor {
            continue;
        }
        pieces.push(Some(
            text.get(cursor..found.start).unwrap_or_default().to_owned(),
        ));
        // **The captures go into the result too**, which is what makes
        // `"a1b".split(/([0-9])/)` three elements rather than two.
        for group in &found.groups {
            pieces.push(
                group.map(|(start, end)| text.get(start..end).unwrap_or_default().to_owned()),
            );
        }
        cursor = found.end;
        if pieces.len() >= cap {
            break;
        }
    }
    if pieces.len() < cap {
        pieces.push(Some(text.get(cursor..).unwrap_or_default().to_owned()));
    }
    pieces.truncate(cap);
    pieces_array(&pieces)
}

/// A one-element array holding `text`, which is what a separator that never matches gives.
fn one_piece_array(text: &str) -> u64 {
    with_new_array(1, |array| {
        let value = new_string(text);
        with_runtime(|runtime| {
            runtime.heap.set_element(array, 0, Value::from_bits(value));
        });
        array.to_value().to_bits()
    })
}

/// The pieces of a split, with `None` for a capture that did not participate.
///
/// **A group that did not match is `undefined`, not `""`.** `"ab".split(/(x)|b/)` has a hole
/// in it, and filling the hole with an empty string is the kind of difference a test written
/// against another engine notices and a reader does not.
fn pieces_array(pieces: &[Option<String>]) -> u64 {
    with_new_array(pieces.len(), |array| {
        for (index, piece) in pieces.iter().enumerate() {
            let value = match piece {
                Some(text) => new_string(text),
                None => Value::UNDEFINED.to_bits(),
            };
            with_runtime(|runtime| {
                runtime
                    .heap
                    .set_element(array, index, Value::from_bits(value));
            });
        }
        array.to_value().to_bits()
    })
}

/// `String.prototype.toString` and `valueOf`.
extern "C" fn string_to_string(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    this_text(this_value).map_or_else(|| new_string(""), |text| new_string(&text))
}

/// Methods on `Function.prototype`, which every function inherits.
const FUNCTION_NATIVES: &[(&str, Native)] = &[
    ("call", function_call),
    ("apply", function_apply),
    ("bind", function_bind),
];

/// `f.bind(receiver, …leading)`.
///
/// **A native can see its own object**: the calling convention passes the callee as the first
/// operand, which is what lets a bound function find what it was bound to without the engine
/// having closures that a native could capture.
extern "C" fn function_bind(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let bound = with_runtime(|runtime| {
            runtime
                .native_function(
                    NATIVES.len() + GLOBAL_NATIVES.len() + NAMESPACE_NATIVES.len() + BOUND_CALL,
                )
                .to_value()
                .to_bits()
        });
        with_rooted(&[bound], || {
            let Some(handle) = handle_of(bound) else {
                return Value::UNDEFINED.to_bits();
            };
            // Each piece is stored before the next is made, so nothing sits unrooted while an
            // allocation runs (D-127).
            with_runtime(|runtime| {
                runtime.define_hidden(handle, BOUND_TARGET, Value::from_bits(this_value));
            });
            // SAFETY: the convention guarantees `argc` readable values at `argv`.
            let receiver = unsafe { argument(argc, argv, 0) };
            with_runtime(|runtime| {
                runtime.define_hidden(handle, BOUND_THIS, Value::from_bits(receiver));
            });
            let leading: Vec<u64> = (1..argc as usize)
                // SAFETY: as above.
                .map(|position| unsafe { argument(argc, argv, position) })
                .collect();
            let held = array_of_values(&leading);
            with_runtime(|runtime| {
                runtime.define_hidden(handle, BOUND_ARGS, Value::from_bits(held));
            });
            bound
        })
    })
}

/// What every bound function runs.
///
/// **The bound arguments come first and the call's own follow**, which is what makes
/// `f.bind(null, 1)(2)` the same as `f(1, 2)`.
extern "C" fn bound_call(
    closure: u64,
    _this_value: u64,
    new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let read = |name: &str| {
        let key = name.to_owned();
        // SAFETY: `key` is a live Rust string.
        unsafe { crisol_property_load(closure, key.as_ptr(), key.len() as u64) }
    };
    let target = read(BOUND_TARGET);
    let receiver = read(BOUND_THIS);
    let held = read(BOUND_ARGS);

    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(closure, argc, argv) };
    with_rooted(&live, || {
        let mut all = Vec::new();
        if let Some((array, length)) = elements_of(held) {
            for index in 0..length {
                all.push(element_at(array, index));
            }
        }
        for position in 0..argc as usize {
            // SAFETY: as above.
            all.push(unsafe { argument(argc, argv, position) });
        }
        // **`new` through a bound function constructs the target**, and calling it instead is
        // not a near miss: `new (Date.bind(null, 0))()` ran `Date(0)`, which answers a string,
        // so the `new` fell back to the bare receiver and `.getTime` was not a function. The
        // receiver the caller made is discarded on this path — the target makes its own, from
        // its own `prototype`, which a bound function does not have.
        if !Value::from_bits(new_target).is_undefined() {
            // The convention requires at least one readable slot even for no arguments.
            let count = all.len() as u64;
            if all.is_empty() {
                all.push(Value::UNDEFINED.to_bits());
            }
            return with_rooted(&all, || {
                // SAFETY: `all` holds `count` values and at least one slot.
                unsafe { construct_with(target, target, count, all.as_ptr()) }
            });
        }
        call_value(target, receiver, &all)
    })
}

/// `f.call(receiver, …args)`.
///
/// **`this` here is the function being called**, not its receiver — the receiver is the first
/// argument. That inversion is the whole of what `call` does, and it is why so many test262
/// cases reach a method through it: `Array.prototype.indexOf.call(true)` tests what `indexOf`
/// does to a receiver that is not an array.
extern "C" fn function_call(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let receiver = unsafe { argument(argc, argv, 0) };
        let rest: Vec<u64> = (1..argc as usize)
            // SAFETY: as above.
            .map(|position| unsafe { argument(argc, argv, position) })
            .collect();
        call_value(this_value, receiver, &rest)
    })
}

/// `f.apply(receiver, args)`, where the arguments arrive as an array.
extern "C" fn function_apply(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let receiver = unsafe { argument(argc, argv, 0) };
        // SAFETY: as above.
        let given = unsafe { argument(argc, argv, 1) };
        let rest: Vec<u64> = match elements_of(given) {
            Some((array, length)) => (0..length).map(|index| element_at(array, index)).collect(),
            // `null` or `undefined` means no arguments, which is not an error.
            None => Vec::new(),
        };
        call_value(this_value, receiver, &rest)
    })
}

const NAMESPACE_NATIVES: &[(&str, &str, Native)] = &[
    ("Object", "keys", object_keys),
    ("Object", "getOwnPropertyNames", object_own_names),
    ("Object", "defineProperty", object_define_property),
    ("Date", "now", date_now),
    ("Date", "UTC", date_utc),
    ("Date", "parse", date_parse),
    ("Number", "isInteger", number_is_integer),
    ("Number", "isSafeInteger", number_is_safe_integer),
    ("Number", "isFinite", number_is_finite),
    ("Number", "isNaN", number_is_nan),
    ("Number", "parseFloat", global_parse_float),
    ("Number", "parseInt", global_parse_int),
    ("String", "fromCharCode", string_from_char_code),
    ("String", "fromCodePoint", string_from_code_point),
    ("JSON", "parse", json_parse),
    ("JSON", "stringify", json_stringify),
    ("Object", "getOwnPropertyDescriptor", object_own_descriptor),
    ("Object", "values", object_values),
    ("Object", "create", object_create),
    ("Object", "getPrototypeOf", object_get_prototype),
    ("Object", "setPrototypeOf", object_set_prototype),
    ("Object", "hasOwn", object_has_own),
    ("Object", "assign", object_assign),
    ("Object", "entries", object_entries),
    ("Object", "fromEntries", object_from_entries),
    ("Object", "freeze", object_freeze),
    ("Object", "isFrozen", object_is_frozen),
    ("Object", "seal", object_seal),
    ("Object", "isSealed", object_is_sealed),
    ("Object", "preventExtensions", object_prevent_extensions),
    ("Object", "isExtensible", object_is_extensible),
    ("Object", "defineProperties", object_define_properties),
    ("Object", "is", object_is),
    ("Object", "getOwnPropertySymbols", object_own_symbols),
    (
        "Object",
        "getOwnPropertyDescriptors",
        object_own_descriptors,
    ),
    ("Object", "groupBy", object_group_by),
    ("Array", "isArray", array_is_array),
    ("Array", "from", array_from),
    ("Symbol", "for", symbol_for),
    ("Symbol", "keyFor", symbol_key_for),
    ("Math", "abs", math_abs),
    ("Math", "floor", math_floor),
    ("Math", "ceil", math_ceil),
    ("Math", "round", math_round),
    ("Math", "trunc", math_trunc),
    ("Math", "sign", math_sign),
    ("Math", "sqrt", math_sqrt),
    ("Math", "cbrt", math_cbrt),
    ("Math", "exp", math_exp),
    ("Math", "log", math_log),
    ("Math", "log2", math_log2),
    ("Math", "log10", math_log10),
    ("Math", "sin", math_sin),
    ("Math", "cos", math_cos),
    ("Math", "tan", math_tan),
    ("Math", "asin", math_asin),
    ("Math", "acos", math_acos),
    ("Math", "atan", math_atan),
    ("Math", "atan2", math_atan2),
    ("Math", "pow", math_pow),
    ("Math", "hypot", math_hypot),
    ("Math", "min", math_min),
    ("Math", "max", math_max),
    ("Math", "random", math_random),
    ("Array", "of", array_of),
    ("Proxy", "revocable", proxy_revocable),
    ("Promise", "all", promise_all),
    ("Promise", "allSettled", promise_all_settled),
    ("Promise", "race", promise_race),
    ("Promise", "any", promise_any),
    ("Promise", "resolve", promise_resolve),
    ("Promise", "reject", promise_reject),
    ("Reflect", "get", reflect_get),
    ("Reflect", "set", reflect_set),
    ("Reflect", "has", reflect_has),
    ("Reflect", "deleteProperty", reflect_delete_property),
    ("Reflect", "ownKeys", reflect_own_keys),
    ("Reflect", "getPrototypeOf", reflect_get_prototype_of),
    ("Reflect", "setPrototypeOf", reflect_set_prototype_of),
    ("Reflect", "defineProperty", reflect_define_property),
    (
        "Reflect",
        "getOwnPropertyDescriptor",
        reflect_own_descriptor,
    ),
    ("Reflect", "isExtensible", reflect_is_extensible),
    ("Reflect", "preventExtensions", reflect_prevent_extensions),
    ("Reflect", "apply", reflect_apply),
    ("Reflect", "construct", reflect_construct),
    ("RegExp", "escape", regexp_escape),
    ("Map", "groupBy", map_group_by),
    ("Error", "isError", error_is_error),
];

/// Whether `value` is the exception signal, **clearing the pending throw if it is**.
///
/// `Reflect`'s operations answer `false` where `Object`'s throw, so the exception the shared
/// implementation raised has to be taken back off the runtime — left there, the next `catch`
/// would receive a throw that nothing performed.
fn swallow_exception(value: u64) -> bool {
    if Value::from_bits(value).is_exception() {
        // Reading it is what clears it, which is why the answer is dropped on purpose.
        let _ = crisol_pending_exception();
        return true;
    }
    false
}

/// The receiver `Reflect` requires: an object, and not merely something with a handle.
fn reflect_target(argc: u64, argv: *const u64) -> Result<u64, u64> {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    if Value::from_bits(target).kind() == crisol_value::Kind::Object {
        Ok(target)
    } else {
        Err(raise("Reflect works on objects", "TypeError"))
    }
}

/// `Reflect.get(target, key)`.
///
/// **The `receiver` argument is ignored.** It exists so a proxy's trap can read through to a
/// getter with the original receiver, and there are no proxies here — honouring it would need
/// the property walk to take a receiver separate from the object, which is a change to the
/// walk rather than to this.
extern "C" fn reflect_get(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let target = match reflect_target(argc, argv) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 1) };
    crisol_computed_load(target, key)
}

/// `Reflect.set(target, key, value)` — **`false` where an assignment would be ignored**, which
/// is the whole difference from writing the property directly.
extern "C" fn reflect_set(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let target = match reflect_target(argc, argv) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 1) };
    // SAFETY: as above.
    let value = unsafe { argument(argc, argv, 2) };
    let Some(name) = to_text(key) else {
        return Value::FALSE.to_bits();
    };
    if refuses_assignment(target, &name) {
        return Value::FALSE.to_bits();
    }
    let outcome = crisol_computed_store(target, key, value);
    boolean(!swallow_exception(outcome)).to_bits()
}

/// `Reflect.has(target, key)` — the `in` operator as a function.
extern "C" fn reflect_has(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let target = match reflect_target(argc, argv) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 1) };
    crisol_in(key, target)
}

/// `Reflect.deleteProperty(target, key)` — `delete` as a function.
extern "C" fn reflect_delete_property(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let target = match reflect_target(argc, argv) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 1) };
    crisol_delete(target, key)
}

/// `Reflect.ownKeys(target)` — every own key, enumerable or not.
///
/// **Symbol keys are missing**, not omitted by choice: a `PropertyKey` is a string here
/// (D-149), so an object cannot have one to report.
extern "C" fn reflect_own_keys(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let target = match reflect_target(argc, argv) {
            Ok(target) => target,
            Err(thrown) => return thrown,
        };
        names_as_array(&own_keys(target))
    })
}

/// `Reflect.getPrototypeOf(target)` — which refuses a primitive where `Object`'s coerces it.
extern "C" fn reflect_get_prototype_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let target = match reflect_target(argc, argv) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    let arguments = [target];
    object_get_prototype(0, this_value, 0, 1, arguments.as_ptr())
}

/// `Reflect.setPrototypeOf(target, proto)` — `false` rather than a throw when it is refused.
extern "C" fn reflect_set_prototype_of(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let target = match reflect_target(argc, argv) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let proto = unsafe { argument(argc, argv, 1) };
    let held = Value::from_bits(proto);
    if held.kind() != crisol_value::Kind::Object && !held.is_null() {
        return raise("a prototype must be an object or null", "TypeError");
    }
    let Some(handle) = handle_of(target) else {
        return Value::FALSE.to_bits();
    };
    boolean(set_prototype_of(handle, proto).is_ok()).to_bits()
}

/// `Reflect.defineProperty(target, key, descriptor)` — `false` where `Object`'s throws.
extern "C" fn reflect_define_property(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let target = match reflect_target(argc, argv) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 1) };
    // SAFETY: as above.
    let descriptor = unsafe { argument(argc, argv, 2) };
    // **A descriptor that is not an object still throws.** The specification's `false` is for
    // a definition the target refuses, not for an argument that describes nothing — the one
    // check `Reflect` keeps.
    if Value::from_bits(descriptor).kind() != crisol_value::Kind::Object {
        return raise("a property description must be an object", "TypeError");
    }
    let arguments = [target, key, descriptor];
    let outcome = with_rooted(&arguments, || {
        object_define_property(0, this_value, 0, 3, arguments.as_ptr())
    });
    boolean(!swallow_exception(outcome)).to_bits()
}

/// `Reflect.getOwnPropertyDescriptor(target, key)`.
extern "C" fn reflect_own_descriptor(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let target = match reflect_target(argc, argv) {
        Ok(target) => target,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let key = unsafe { argument(argc, argv, 1) };
    let arguments = [target, key];
    with_rooted(&arguments, || {
        object_own_descriptor(0, this_value, 0, 2, arguments.as_ptr())
    })
}

/// `Reflect.isExtensible(target)`.
extern "C" fn reflect_is_extensible(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    match reflect_target(argc, argv) {
        Ok(target) => boolean(is_extensible(target)).to_bits(),
        Err(thrown) => thrown,
    }
}

/// `Reflect.preventExtensions(target)`.
extern "C" fn reflect_prevent_extensions(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    match reflect_target(argc, argv) {
        Ok(target) => {
            prevent_extensions(target);
            Value::TRUE.to_bits()
        }
        Err(thrown) => thrown,
    }
}

/// `Reflect.apply(target, thisArgument, argumentsList)`.
extern "C" fn reflect_apply(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        if !is_callable(target) {
            return raise("Reflect.apply needs a function", "TypeError");
        }
        // SAFETY: as above.
        let receiver = unsafe { argument(argc, argv, 1) };
        // SAFETY: as above.
        let list = unsafe { argument(argc, argv, 2) };
        let length = match indexed_length(list) {
            Ok(length) => length,
            Err(thrown) => return thrown,
        };
        let arguments: Vec<u64> = (0..length).map(|index| indexed_get(list, index)).collect();
        with_rooted(&arguments, || call_value(target, receiver, &arguments))
    })
}

/// `Reflect.construct(target, argumentsList, newTarget)`.
///
/// **test262 asks every built-in whether it can be constructed through this**, with
/// `isConstructor`, which is `Reflect.construct(function () {}, [], f)` and reads the answer
/// from whether it threw. So the third argument is not an exotic corner here: it is the only
/// argument those cases vary, and refusing a `newTarget` that is not a constructor is the
/// whole of what they check.
extern "C" fn reflect_construct(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        // **Absent is not `undefined` here.** `Reflect.construct(f, [])` constructs `f`, and
        // `Reflect.construct(f, [], undefined)` is a `TypeError` — so the default is taken
        // from how many arguments arrived rather than from what the third one holds.
        let new_target = if argc > 2 {
            // SAFETY: as above.
            unsafe { argument(argc, argv, 2) }
        } else {
            target
        };
        if !is_constructor(target) {
            return raise("Reflect.construct needs a constructor", "TypeError");
        }
        if !is_constructor(new_target) {
            return raise("a new target must be a constructor", "TypeError");
        }
        // SAFETY: as above.
        let list = unsafe { argument(argc, argv, 1) };
        if handle_of(list).is_none() {
            return raise("an argument list must be an object", "TypeError");
        }
        let length = match indexed_length(list) {
            Ok(length) => length,
            Err(thrown) => return thrown,
        };
        // The convention requires at least one readable slot even for no arguments.
        let mut arguments: Vec<u64> = (0..length).map(|index| indexed_get(list, index)).collect();
        let count = arguments.len() as u64;
        if arguments.is_empty() {
            arguments.push(Value::UNDEFINED.to_bits());
        }
        with_rooted(&arguments, || {
            // SAFETY: `arguments` holds `count` values and at least one slot.
            unsafe { construct_with(target, new_target, count, arguments.as_ptr()) }
        })
    })
}

/// Writes a property regardless of whether it is writable.
///
/// `defineProperty` redefines rather than assigns, so the check an assignment makes must not
/// apply — otherwise a property defined non-writable could never be redefined.
///
/// Takes a [`PropertyKey`] rather than a name, because a symbol has no identifying one.
fn define_keyed_ignoring_writability(handle: GcRef, key: &PropertyKey, value: u64) -> u64 {
    with_runtime(|runtime| {
        let Some(current) = runtime.heap.shape_of(handle) else {
            return Value::UNDEFINED.to_bits();
        };
        let (shape, slot, width) = {
            let mut shapes = runtime.shapes.borrow_mut();
            let shape = shapes.add(current, key);
            let Some(slot) = shapes.lookup(shape, key) else {
                return Value::UNDEFINED.to_bits();
            };
            (shape, slot, shapes.len(shape) as usize)
        };
        if shape != current {
            runtime.heap.transition(handle, shape, width);
        }
        // **A defined property is present**, so the tombstone goes. The shape keeps naming a
        // deleted property's slot, and writing a value into it without clearing the mark left
        // the property both defined and absent: `defineProperty` after a `delete` set the
        // value, found nothing when it went back to set the attributes, and returned as
        // though it had worked.
        runtime.heap.set_deleted(handle, slot.index(), false);
        runtime
            .heap
            .set(handle, slot.index(), Value::from_bits(value));
        Value::UNDEFINED.to_bits()
    })
}

/// Reads a property of `object` by name, without walking the prototype chain.
fn own_property(object: u64, name: &str) -> Option<(u32, Value)> {
    own_property_keyed(object, &PropertyKey::new(name))
}

/// [`own_property`] for a key that may name a symbol.
///
/// Separate because the named form takes a `&str`, and a symbol has no identifying one —
/// routing one through its description made two symbols the same property (D-193).
fn own_property_keyed(object: u64, key: &PropertyKey) -> Option<(u32, Value)> {
    let handle = handle_of(object)?;
    with_runtime(|runtime| {
        let shape = runtime.heap.shape_of(handle)?;
        let slot = runtime.shapes.borrow().lookup(shape, key)?;
        // **A deleted property is absent**, and the shape still names its slot — that is what
        // the tombstone is for. Answering from the slot anyway handed back the attributes the
        // property had before it was deleted, so a redefinition validated against a property
        // that is no longer there and an assignment could be refused by a permission nothing
        // holds any more.
        if runtime.heap.is_deleted(handle, slot.index()) {
            return None;
        }
        runtime
            .heap
            .get(handle, slot.index())
            .map(|value| (slot.index(), value))
    })
}

/// The own property `name` names when nothing stores it.
///
/// **An array's elements and its `length` are own properties with no slot.** They live beside
/// the object rather than in its shape, so [`own_property`] — which reads the shape — cannot
/// see them, and everything asked through it answered "absent" for exactly the properties an
/// array is made of. `Object.getOwnPropertyDescriptor([1], 0)` was `undefined`, and test262's
/// `propertyHelper` then read a field off it: the largest single source of "cannot read a
/// property of undefined" in the corpus, from a method that looked complete.
///
/// A string wrapper's characters are the same shape of problem, for the same reason (D-157).
fn derived_own_property(object: u64, name: &str) -> Option<(u64, crisol_value::Attributes)> {
    let handle = handle_of(object)?;
    if let Some(count) = with_runtime(|runtime| runtime.heap.element_count(handle)) {
        if name == "length" {
            return Some((
                Value::number(index_as_f64(count)).to_bits(),
                crisol_value::Attributes {
                    // **Writable, and neither enumerable nor configurable** — the one
                    // attribute set no ordinary property has.
                    writable: !own_flag(object, FIXED_LENGTH),
                    enumerable: false,
                    configurable: false,
                    accessor: false,
                },
            ));
        }
        let index = canonical_index(name)?;
        if index >= count {
            return None;
        }
        return Some((indexed_get(object, index), element_rule(object, index)));
    }

    // A string's characters, which are fixed in every way a property can be.
    let text = wrapped_text(object)?;
    let units: Vec<u16> = text.encode_utf16().collect();
    if name == "length" {
        return Some((
            Value::number(index_as_f64(units.len())).to_bits(),
            crisol_value::Attributes {
                writable: false,
                enumerable: false,
                configurable: false,
                accessor: false,
            },
        ));
    }
    let index = canonical_index(name)?;
    let unit = units.get(index)?;
    Some((
        new_string(&String::from_utf16_lossy(&[*unit])),
        crisol_value::Attributes {
            writable: false,
            enumerable: true,
            configurable: false,
            accessor: false,
        },
    ))
}

/// Writes `value` at `index`, saying whether `object` was something that keeps elements.
///
/// **The refusals are the element ones, not the slot ones.** An existing element may be
/// written on a non-extensible object and a new one may not, because growing the run *is* the
/// addition that being non-extensible refuses — and `set_element` grows to fit, so the
/// question has to be asked before the write rather than inside it.
///
/// **A sparse index is not an element.** Elements are a dense `Vec`, so `a[4294967294] = 2` —
/// a legal array index — asks for every slot below it as well. The specification's arrays are
/// sparse; these are not, and the honest approximation is to stop pretending past the point
/// where the memory would be absurd. Beyond the cap this answers `false` and the value becomes
/// a named property: still stored, still readable by the same key, but not counted by
/// `length`. That is wrong in a way a test can report rather than one that kills the process.
fn store_element(object: u64, index: usize, value: u64) -> bool {
    if index > DENSE_ELEMENT_LIMIT {
        return false;
    }
    let Some(handle) = handle_of(object) else {
        return false;
    };
    let Some(count) = with_runtime(|runtime| runtime.heap.element_count(handle)) else {
        return false;
    };
    let refused = if index < count {
        !element_rule(object, index).writable
    } else {
        !is_extensible(object)
    };
    if refused {
        // Handled, and silently — outside strict mode, as every other refused write is.
        return true;
    }
    with_runtime(|runtime| {
        runtime
            .heap
            .set_element(handle, index, Value::from_bits(value))
    })
}

/// The index `name` spells, if it spells one.
///
/// **Only the canonical spelling.** `"01"` parses as one and `"1.0"` as one, and neither is an
/// array index — a program that writes `o["01"] = 1` has written a property called `"01"`, and
/// treating it as element one would put the value somewhere the program cannot read it back
/// from. Round-tripping through the number's own spelling is the test the specification makes.
fn canonical_index(name: &str) -> Option<usize> {
    let index = name.parse::<usize>().ok()?;
    (number_text(index_as_f64(index)) == name).then_some(index)
}

/// `RequireObjectCoercible` — the check nearly every `Object` static opens with.
///
/// **`null` and `undefined` are the error, not "anything that is not an object".** The
/// specification coerces its argument, so `Object.keys("ab")` answers `["0", "1"]` and only a
/// nullish one throws. Returning the raised exception rather than a boolean keeps the caller
/// to one line and makes the message the same wherever it comes from.
fn reject_nullish(value: u64, what: &str) -> Option<u64> {
    Value::from_bits(value)
        .is_nullish()
        .then(|| raise(&format!("{what} of null or undefined"), "TypeError"))
}

/// One field of a property descriptor, read as a value.
fn read_descriptor_field(descriptor: u64, name: &str) -> u64 {
    let field = name.to_owned();
    // SAFETY: `field` is a live Rust string.
    unsafe { crisol_property_load(descriptor, field.as_ptr(), field.len() as u64) }
}

/// Whether a descriptor field is present and truthy.
fn descriptor_flag(descriptor: u64, name: &str) -> Option<bool> {
    let key = name.to_owned();
    // SAFETY: `key` is a live Rust string.
    let bits = unsafe { crisol_property_load(descriptor, key.as_ptr(), key.len() as u64) };
    let value = Value::from_bits(bits);
    // **Absent and `false` are different.** A descriptor that omits `writable` leaves an
    // existing property's writability alone, and one that says `writable: false` clears it.
    if value.kind() == crisol_value::Kind::Undefined {
        return None;
    }
    Some(is_truthy(value))
}

/// `Object.defineProperty(target, key, descriptor)`.
///
/// **A defined property defaults to none of writable, enumerable or configurable**, which is
/// the opposite of what assignment creates. That difference is the whole reason descriptors
/// exist, and an implementation that reused the assignment default would pass every test that
/// does not check it.
extern "C" fn object_define_property(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        // SAFETY: as above.
        let key = unsafe { argument(argc, argv, 1) };
        // SAFETY: as above.
        let descriptor = unsafe { argument(argc, argv, 2) };

        // **A string and a symbol are cells too**, so a handle is not the test — the kind is.
        // Without that, `Object.defineProperty("ab", …)` reached the string's own cell and
        // defined a property on a primitive, which is a `TypeError` in the specification and
        // a property nothing can read here.
        let handle = match handle_of(target) {
            Some(handle) if Value::from_bits(target).kind() == crisol_value::Kind::Object => handle,
            _ => return raise("cannot define a property on a non-object", "TypeError"),
        };
        // **A symbol is a key too, and `to_text` refuses one.** `Object.defineProperty(o,
        // Symbol.iterator, …)` was a `TypeError` — on the one property a program is most
        // likely to define that way. The key goes through `key_of`, which knows both
        // spellings, and the string is kept alongside only for the two questions that are
        // genuinely about names: whether this is `length`, and whether it is an index.
        let Some(property) = key_of(Value::from_bits(key)) else {
            return raise("a property key must be a name", "TypeError");
        };
        let name = if property.is_symbol() {
            String::new()
        } else {
            property.as_str().to_owned()
        };
        let named = !property.is_symbol();
        if let Some((behind, handler)) = proxy_parts(target) {
            return proxy_define(behind, handler, key, descriptor);
        }
        // **A descriptor has to be an object**, and a string is a cell without being one. A
        // primitive has no `value` and no `writable`, so reading fields off it found nothing
        // and the call quietly defined the property as `undefined` — a wrong answer where the
        // specification has an error.
        if Value::from_bits(descriptor).kind() != crisol_value::Kind::Object {
            return raise("a property description must be an object", "TypeError");
        }

        // **An array's `length` is its element count, not a property**, so defining it has to
        // resize rather than store. Storing left the array reporting two lengths at once — the
        // descriptor said one and the elements said two — and every question after that got
        // whichever answer its asker happened to consult.
        if named
            && name == "length"
            && let Some(count) = with_runtime(|runtime| runtime.heap.element_count(handle))
        {
            let read_value = "value".to_owned();
            // SAFETY: `read_value` is a live Rust string.
            let given = unsafe {
                crisol_property_load(descriptor, read_value.as_ptr(), read_value.len() as u64)
            };
            if Value::from_bits(given).kind() != crisol_value::Kind::Undefined {
                // **`ToNumber`, which runs `valueOf`/`toString`** — a length given as an object
                // that coerces to a number is coerced through them, not read as `NaN`. `to_number`
                // skipped `ToPrimitive` and made `{length: {value: {toString: () => "2"}}}` a
                // `RangeError` (D-236).
                let wanted = match coerce_number(given) {
                    Ok(wanted) => wanted,
                    Err(thrown) => return thrown,
                };
                if !wanted.is_finite()
                    || wanted < 0.0
                    || wanted.fract() != 0.0
                    || wanted > f64::from(u32::MAX)
                {
                    return raise("invalid array length", "RangeError");
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "range-checked immediately above"
                )]
                let wanted = wanted as usize;
                with_runtime(|runtime| {
                    if wanted < count {
                        runtime.heap.truncate_elements(handle, wanted);
                    } else if wanted > count {
                        runtime
                            .heap
                            .set_element(handle, wanted - 1, Value::UNDEFINED);
                    }
                });
            }
            // `writable: false` on a length is remembered separately, because there is no slot
            // to hang an attribute on — the length is derived, so its permissions must be too.
            if descriptor_flag(descriptor, "writable") == Some(false) {
                with_runtime(|runtime| {
                    runtime.define_hidden(handle, FIXED_LENGTH, Value::number(1.0));
                });
            }
            return target;
        }

        // **An array index is an element, not a slot.** Defining one has to write the
        // element, or the array ends up holding two answers for the same key — the element
        // the reads use and the slot the descriptor questions use — which disagree from then
        // on. Everything below this point is shared with the ordinary path: the current
        // attributes come from `derived_own_property` and only the two *writes* differ.
        //
        // Past `DENSE_ELEMENT_LIMIT` an index is not an element (see `store_element`), so it
        // takes the slot path — readable by the same key, but not counted by `length`.
        let element = with_runtime(|runtime| runtime.heap.element_count(handle))
            .filter(|_| named)
            .and(canonical_index(&name))
            .filter(|index| *index <= DENSE_ELEMENT_LIMIT);

        let existing = own_property_keyed(target, &property);
        let read_field = |field: &str| -> u64 { read_descriptor_field(descriptor, field) };
        let given = read_field("value");
        let has_value = Value::from_bits(given).kind() != crisol_value::Kind::Undefined
            || own_property(descriptor, "value").is_some();

        // **An accessor is a property whose value is computed**, so the slot holds the pair of
        // functions rather than anything the program reads. `get` and `set` are looked for
        // before `value`, because a descriptor carrying both is a `TypeError` and carrying
        // either makes this an accessor whatever else is present.
        let getter = read_field("get");
        let setter = read_field("set");
        // **Present and not callable is an error**, not "not an accessor". A descriptor
        // carrying `get: "string"` describes nothing the engine can do, and treating it as a
        // data descriptor defined the property as `undefined` instead of saying so.
        for (field, value) in [("get", getter), ("set", setter)] {
            if Value::from_bits(value).kind() != crisol_value::Kind::Undefined
                && !is_callable(value)
            {
                return raise(
                    if field == "get" {
                        "a getter must be a function"
                    } else {
                        "a setter must be a function"
                    },
                    "TypeError",
                );
            }
        }
        // **Present and `undefined` is not absent.** `{get: undefined}` on an existing
        // accessor clears the getter and keeps the property an accessor; omitting `get`
        // leaves the one that is there. The two read the same through a plain field read, so
        // the descriptor is asked whether it has the key at all — the same question `value`
        // is already asked.
        let has_getter = is_callable(getter) || own_property(descriptor, "get").is_some();
        let has_setter = is_callable(setter) || own_property(descriptor, "set").is_some();
        let is_accessor = has_getter || has_setter;
        if is_accessor && has_value {
            return raise(
                "a descriptor cannot have both a value and an accessor",
                "TypeError",
            );
        }

        // **A non-configurable property is nearly immutable.** The specification allows exactly
        // one change to one: a writable data property may be made non-writable, and its value
        // may still be set. Everything else — turning enumerability on or off, making it
        // configurable again, swapping a data property for an accessor, or changing the value
        // of one already non-writable — is a `TypeError`.
        //
        // Without this, `Object.defineProperty` would undo its own guarantees: a property
        // frozen by `Object.freeze` could be quietly thawed by redefining it.
        //
        // Read from the slot where there is one and from the derived answer where there is
        // not — an element and a string's characters are own properties with no slot, so
        // asking only the shape said "absent" and let every refusal through.
        let current_state = match existing {
            Some((slot, value)) => Some((
                with_runtime(|runtime| runtime.heap.attributes_of(handle, slot)),
                value,
            )),
            None if named => derived_own_property(target, &name)
                .map(|(bits, attributes)| (attributes, Value::from_bits(bits))),
            None => None,
        };
        // **A non-extensible object refuses a property it does not have**, which is the one
        // refusal `defineProperty` never made — so `Object.preventExtensions(o)` stopped
        // assignment and let a definition straight through, which is the hole it exists to
        // close.
        if current_state.is_none() && !is_extensible(target) {
            return raise(
                "cannot add a property to a non-extensible object",
                "TypeError",
            );
        }

        if let Some((current, current_value)) = current_state
            && !current.configurable
        {
            let asked_configurable = descriptor_flag(descriptor, "configurable");
            let asked_enumerable = descriptor_flag(descriptor, "enumerable");
            let asked_writable = descriptor_flag(descriptor, "writable");
            let changes_kind = is_accessor != current.accessor;
            let unwritable_value_change = !current.writable
                && has_value
                && !same_value(Value::from_bits(given), current_value);
            if asked_configurable == Some(true)
                || asked_enumerable.is_some_and(|wanted| wanted != current.enumerable)
                || asked_writable.is_some_and(|wanted| wanted && !current.writable)
                || changes_kind
                || unwritable_value_change
            {
                return raise("cannot redefine a non-configurable property", "TypeError");
            }
        }

        // The write goes through the ordinary path so the shape transition happens there once.
        let stored = if is_accessor {
            // **A partial accessor descriptor merges with the one already there.**
            // `{get x() {…}, set x(v) {…}}` is two definitions of *one* property, and
            // rebuilding the pair from the descriptor alone made the second erase the first —
            // so the literal's getter disappeared the moment its setter was defined.
            let mut pair = [getter, setter];
            if let Some((current, current_value)) = current_state
                && current.accessor
            {
                let existing = current_value.to_bits();
                if !has_getter {
                    pair[0] = indexed_get(existing, 0);
                }
                if !has_setter {
                    pair[1] = indexed_get(existing, 1);
                }
            }
            // The pair, in the slot the property already occupies — so the collector traces
            // them exactly as it traces any other property value, with nothing added to the
            // heap's idea of what an object holds.
            with_rooted(&pair, || array_of_values(&pair))
        } else if has_value {
            given
        } else {
            // From `current_state`, not from `existing`: an element has no slot, so keeping
            // its value on a redefinition that names no value has to read the derived answer.
            current_state.map_or(Value::UNDEFINED.to_bits(), |(_, value)| value.to_bits())
        };
        let base = current_state.map_or(crisol_value::Attributes::DEFINED, |(current, _)| current);
        let wanted = crisol_value::Attributes {
            writable: descriptor_flag(descriptor, "writable").unwrap_or(base.writable),
            enumerable: descriptor_flag(descriptor, "enumerable").unwrap_or(base.enumerable),
            configurable: descriptor_flag(descriptor, "configurable").unwrap_or(base.configurable),
            accessor: is_accessor,
        };

        // An element, written as an element and recorded in the rules beside it.
        //
        // **Except an accessor**, which an element cannot be: the place the pair of functions
        // would live *is* the element. That falls through to the slot path, which stores it
        // beside the element rather than dropping it — wrong, and wrong where a test can see
        // it rather than where the heap comes apart.
        if let Some(index) = element
            && !is_accessor
        {
            with_rooted(&[target, stored], || {
                with_runtime(|runtime| {
                    runtime
                        .heap
                        .set_element(handle, index, Value::from_bits(stored));
                });
                set_element_rule(target, Some(index), wanted);
            });
            return target;
        }

        let outcome = define_keyed_ignoring_writability(handle, &property, stored);
        if Value::from_bits(outcome).is_exception() {
            return outcome;
        }

        let Some((slot, _)) = own_property_keyed(target, &property) else {
            return target;
        };
        with_runtime(|runtime| runtime.heap.set_attributes(handle, slot, wanted));
        target
    })
}

/// `Object.getOwnPropertyDescriptor(target, key)`.
extern "C" fn object_own_descriptor(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        // SAFETY: as above.
        let key = unsafe { argument(argc, argv, 1) };
        if let Some(thrown) = reject_nullish(target, "cannot read a property descriptor") {
            return thrown;
        }
        if let Some((behind, handler)) = proxy_parts(target) {
            return proxy_descriptor(behind, handler, key);
        }
        // A symbol is a key here too — see `object_define_property`, which refused one for
        // the same reason until it stopped asking `to_text`.
        let Some(property) = key_of(Value::from_bits(key)) else {
            return Value::UNDEFINED.to_bits();
        };
        let named = !property.is_symbol();
        let name = if named {
            property.as_str().to_owned()
        } else {
            String::new()
        };
        let Some(handle) = handle_of(target) else {
            return Value::UNDEFINED.to_bits();
        };
        // **The result object is allocated first**, before the value it will describe, because
        // a derived value can be a string this call makes — `new String("ab")`'s characters
        // are materialised on demand — and a value between its allocation and its first store
        // is invisible to the collector. Made in the other order it was the descriptor's own
        // allocation that could free it, which under GC stress is a descriptor whose `value`
        // is garbage and without stress is nothing at all.
        let descriptor = crisol_create_object();
        // **`undefined` for an absent property**, which is how a caller tells "not there" from
        // "there and not writable".
        let found = with_rooted(&[descriptor], || {
            match own_property_keyed(target, &property) {
                Some((slot, value)) => Some((
                    value,
                    with_runtime(|runtime| runtime.heap.attributes_of(handle, slot)),
                )),
                None if named => derived_own_property(target, &name)
                    .map(|(bits, attributes)| (Value::from_bits(bits), attributes)),
                None => None,
            }
        });
        let Some((value, attributes)) = found else {
            return Value::UNDEFINED.to_bits();
        };
        with_rooted(&[descriptor, value.to_bits()], || {
            let Some(into) = handle_of(descriptor) else {
                return;
            };
            // **An accessor descriptor has `get` and `set` where a data one has `value` and
            // `writable`** — four fields, never mixed, and a caller tells them apart by which
            // pair is present.
            if attributes.accessor {
                let (getter, setter) = elements_of(value.to_bits()).map_or(
                    (Value::UNDEFINED.to_bits(), Value::UNDEFINED.to_bits()),
                    |(pair, _)| (element_at(pair, 0), element_at(pair, 1)),
                );
                with_runtime(|runtime| {
                    runtime.define(into, "get", Value::from_bits(getter));
                    runtime.define(into, "set", Value::from_bits(setter));
                });
            } else {
                with_runtime(|runtime| {
                    runtime.define(into, "value", value);
                    runtime.define(into, "writable", boolean(attributes.writable));
                });
            }
            with_runtime(|runtime| {
                runtime.define(into, "enumerable", boolean(attributes.enumerable));
                runtime.define(into, "configurable", boolean(attributes.configurable));
            });
        });
        descriptor
    })
}

/// `Map.groupBy(items, classify)` — the same grouping, keyed by the value rather than a name.
///
/// **That is the whole difference from `Object.groupBy`**, and it is the reason both exist: an
/// object's keys are strings, so grouping by `1` and by `"1"` collides there and does not
/// here. Grouping by an object is only possible through this one.
extern "C" fn map_group_by(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let items = unsafe { argument(argc, argv, 0) };
        // SAFETY: as above.
        let classify = unsafe { argument(argc, argv, 1) };
        if let Some(thrown) = reject_nullish(items, "cannot group") {
            return thrown;
        }
        if !is_callable(classify) {
            return raise("a grouping needs a function", "TypeError");
        }
        let groups = new_collection(MAP_PROTOTYPE.with(std::cell::Cell::get), false);
        with_rooted(&[groups, items, classify], || {
            let length = match indexed_length(items) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            for index in 0..length {
                let value = indexed_get(items, index);
                let arguments = [value, Value::number(index_as_f64(index)).to_bits()];
                let key = with_rooted(&arguments, || {
                    call_value(classify, Value::UNDEFINED.to_bits(), &arguments)
                });
                if Value::from_bits(key).is_exception() {
                    return key;
                }
                // The group is read back and extended rather than rebuilt, so two items with
                // the same key land in one array instead of the second replacing the first.
                let read = [key];
                let existing =
                    with_rooted(&[value, key], || map_get(0, groups, 0, 1, read.as_ptr()));
                let list = if elements_of(existing).is_some() {
                    existing
                } else {
                    with_rooted(&[value, key], || crisol_create_array(0))
                };
                with_rooted(&[list, key, value], || {
                    if let Some((array, count)) = elements_of(list) {
                        with_runtime(|runtime| {
                            runtime
                                .heap
                                .set_element(array, count, Value::from_bits(value));
                        });
                    }
                    let write = [key, list];
                    map_set(0, groups, 0, 2, write.as_ptr());
                });
            }
            groups
        })
    })
}

/// `Object.groupBy(items, classify)` — the items, filed under what `classify` answers.
///
/// The result has **no prototype**, which is the point of the method: the keys come from the
/// data, so a group called `"toString"` must not collide with anything inherited.
///
/// **Walked by index, not by the iterator protocol.** The specification iterates, and this
/// engine cannot yet iterate a user-defined iterable (D-149 — a symbol cannot be a property
/// key, so `Symbol.iterator` cannot be looked up). Arrays, strings and array-likes are what
/// the corpus passes it, and those this walks correctly; anything else groups nothing rather
/// than grouping wrongly.
extern "C" fn object_group_by(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let items = unsafe { argument(argc, argv, 0) };
        // SAFETY: as above.
        let classify = unsafe { argument(argc, argv, 1) };
        if let Some(thrown) = reject_nullish(items, "cannot group") {
            return thrown;
        }
        if !is_callable(classify) {
            return raise("a grouping needs a function", "TypeError");
        }
        let groups = crisol_create_object();
        with_rooted(&[groups, items, classify], || {
            if let Some(handle) = handle_of(groups) {
                with_runtime(|runtime| runtime.heap.set_prototype(handle, None));
            }
            let length = match indexed_length(items) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            for index in 0..length {
                let value = indexed_get(items, index);
                let arguments = [value, Value::number(index_as_f64(index)).to_bits()];
                let key = with_rooted(&arguments, || {
                    call_value(classify, Value::UNDEFINED.to_bits(), &arguments)
                });
                if Value::from_bits(key).is_exception() {
                    return key;
                }
                let Some(name) = with_rooted(&[value, key], || to_text(key)) else {
                    continue;
                };
                // The group is read back and extended rather than rebuilt, so two items with
                // the same key land in one array instead of the second replacing the first.
                let existing = with_rooted(&[value], || {
                    // SAFETY: `name` is a live Rust string.
                    unsafe { crisol_property_load(groups, name.as_ptr(), name.len() as u64) }
                });
                let group = if elements_of(existing).is_some() {
                    existing
                } else {
                    let made = with_rooted(&[value], || crisol_create_array(0));
                    with_rooted(&[made, value], || {
                        // SAFETY: `name` is a live Rust string.
                        unsafe {
                            crisol_property_store(groups, name.as_ptr(), name.len() as u64, made);
                        }
                    });
                    made
                };
                // The group is an array this function made, so its length is its element
                // count and cannot throw — but it is read through the same fallible path as
                // every other length, because a second way to ask is a second answer.
                let at = match indexed_length(group) {
                    Ok(at) => at,
                    Err(thrown) => return thrown,
                };
                with_rooted(&[group, value], || {
                    store_element(group, at, value);
                });
            }
            groups
        })
    })
}

/// `Object.getOwnPropertyDescriptors(target)` — all of them, in one object.
///
/// **What makes a faithful copy possible.** `Object.assign` reads values and drops attributes,
/// so the only way to duplicate an object without flattening its accessors and its
/// non-writables is to pair this with `defineProperties`.
extern "C" fn object_own_descriptors(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        if Value::from_bits(target).is_nullish() {
            return raise(
                "cannot read the properties of null or undefined",
                "TypeError",
            );
        }
        let into = crisol_create_object();
        with_rooted(&[into, target], || {
            for name in own_keys(target) {
                let key = new_string(&name);
                let descriptor = with_rooted(&[key], || {
                    let arguments = [target, key];
                    object_own_descriptor(0, 0, 0, 2, arguments.as_ptr())
                });
                if Value::from_bits(descriptor).is_exception() {
                    return descriptor;
                }
                // A key with no descriptor gets no entry, rather than an entry saying
                // `undefined` — the two are how a caller tells absence from an empty answer.
                if Value::from_bits(descriptor).kind() == crisol_value::Kind::Undefined {
                    continue;
                }
                with_rooted(&[descriptor], || {
                    // SAFETY: `name` is a live Rust string.
                    unsafe {
                        crisol_property_store(into, name.as_ptr(), name.len() as u64, descriptor)
                    }
                });
            }
            into
        })
    })
}

/// A boolean as a value.
const fn boolean(flag: bool) -> Value {
    if flag { Value::TRUE } else { Value::FALSE }
}

/// `Object.getOwnPropertyNames` — every own property, enumerable or not.
extern "C" fn object_own_names(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        if let Some(thrown) = reject_nullish(target, "cannot read the property names") {
            return thrown;
        }
        names_as_array(&own_keys(target))
    })
}

/// An array of strings, each stored before the next is made.
fn names_as_array(names: &[String]) -> u64 {
    with_new_array(names.len(), |array| {
        for (index, name) in names.iter().enumerate() {
            let text = new_string(name);
            with_runtime(|runtime| {
                runtime
                    .heap
                    .set_element(array, index, Value::from_bits(text))
            });
        }
        array.to_value().to_bits()
    })
}

/// The properties this engine keeps on an object because it has no internal slots.
///
/// **They are not the program's properties and must not be reported as such.** Each stands in
/// for something the specification puts in an internal slot — a date's time, a wrapper's
/// primitive, whether an object is extensible — and every one of them was visible to
/// `Object.getOwnPropertyNames` and, worse, to `Object.isFrozen`, which asks whether *every*
/// own property is non-writable and found this bookkeeping among them.
const INTERNAL_PROPERTIES: &[&str] = &[
    DATE_TIME,
    STRING_PRIMITIVE,
    NOT_EXTENSIBLE,
    FIXED_LENGTH,
    ELEMENT_RULES,
    ERROR_DATA,
    PROMISE_SETTLES,
    PROMISE_REJECTS,
    COMBINE_RESULT,
    COMBINE_VALUES,
    COMBINE_PENDING,
    COMBINE_INDEX,
    COMBINE_SHAPE,
    PROXY_REVOKE_TARGET,
    COLLECTION_ENTRIES,
    MAP_BRAND,
    SET_BRAND,
    BOUND_TARGET,
    BOUND_THIS,
    BOUND_ARGS,
    ITERATOR_TARGET,
    ITERATOR_POSITION,
    ITERATOR_KIND,
];

/// Whether `name` is one of this engine's stand-ins for an internal slot.
fn is_internal_property(name: &str) -> bool {
    INTERNAL_PROPERTIES.contains(&name)
}

/// The text a string wrapper holds, if `object` is one.
///
/// **The kind is checked, not assumed.** Every wrapper keeps its primitive under the same
/// hidden name, so a `Number` wrapper answers this too — and reading `new Number(12345)`'s
/// primitive as text would give it five characters and five own properties.
fn wrapped_text(object: u64) -> Option<String> {
    // A primitive string *is* the text; `Object.keys("ab")` coerces it to a wrapper and gets
    // the same properties, so the two answer alike here rather than at each caller.
    if let Some(text) = text_of(object) {
        return Some(text);
    }
    // An own lookup rather than a property read: a wrapper's primitive is its own, and this
    // is asked once per `for-in`, where a chain walk that nearly always misses is not free.
    text_of(own_property(object, STRING_PRIMITIVE)?.1.to_bits())
}

/// The own property names of `this`'s first argument.
fn own_keys(object: u64) -> Vec<String> {
    let Some(handle) = handle_of(object) else {
        return Vec::new();
    };
    if let Some((target, handler)) = proxy_parts(object) {
        // A proxy with no `ownKeys` trap reports its target's keys, which is what `None`
        // from here means.
        if let Some(names) = proxy_own_keys(target, handler) {
            return names;
        }
        return own_keys(target);
    }
    // Read before the borrow below: a hidden property is read by a property load, and a
    // property load enters the runtime itself.
    let wrapped = wrapped_text(object);
    with_runtime(|runtime| {
        let mut names: Vec<String> = Vec::new();
        // **Indices come first and in numeric order**, before the string-named properties, which
        // is the enumeration order the specification fixes rather than insertion order.
        let is_array = if let Some(count) = runtime.heap.element_count(handle) {
            for index in 0..count {
                names.push(number_text(index_as_f64(index)));
            }
            true
        } else {
            // **A string wrapper owns one property per character.** They are materialised on
            // demand rather than stored (D-157), so the shape lists none of them and
            // `Object.keys(new String("ab"))` answered without them.
            if let Some(text) = &wrapped {
                for index in 0..text.encode_utf16().count() {
                    names.push(number_text(index_as_f64(index)));
                }
            }
            false
        };
        let count_before_properties = names.len();
        if let Some(shape) = runtime.heap.shape_of(handle) {
            // **An integer-like key is an index, and every index comes before every name, in
            // ascending order** — whatever order they were inserted in. That is the
            // specification's ordering and it is observable: `Object.keys({b: 1, 2: 1, 1: 1})`
            // is `["1", "2", "b"]`, not insertion order. Storing them in one list gave
            // insertion order, which is right for the names and wrong for the rest.
            let mut indices: Vec<(usize, String)> = Vec::new();
            let mut strings: Vec<String> = Vec::new();
            for (key, slot) in runtime.shapes.borrow().properties(shape) {
                // **A symbol-keyed property is not an own *name*.** `Object.keys`,
                // `getOwnPropertyNames` and `for-in` report strings; symbols are reported
                // only by `getOwnPropertySymbols`, and mixing them would put a description
                // where a property name was expected.
                if runtime.heap.is_deleted(handle, slot.index())
                    || key.is_symbol()
                    || is_internal_property(key.as_str())
                {
                    continue;
                }
                let name = key.as_str().to_owned();
                // The canonical spelling only: `"01"` parses as one and is not an index, so
                // it stays where it was written.
                match name.parse::<usize>() {
                    Ok(index) if number_text(index_as_f64(index)) == name => {
                        indices.push((index, name));
                    }
                    _ => strings.push(name),
                }
            }
            indices.sort_unstable_by_key(|(index, _)| *index);
            names.extend(indices.into_iter().map(|(_, name)| name));
            names.extend(strings);
        }
        // Whether the shape contributed anything, which is the only way a name can repeat.
        let stored_any = names.len() > count_before_properties;
        // **An array owns `length`**, even though nothing stores it. `getOwnPropertyNames` has
        // to say so, and it did not — an array reported its indices and nothing else. It is
        // added last because the specification puts the indices first and the rest after, and
        // it is not enumerable, so `Object.keys` and `for-in` still leave it out.
        if is_array || (wrapped.is_some() && !names.iter().any(|name| name == "length")) {
            names.push("length".to_owned());
        }
        // **Once each.** An index can reach this list twice — as an element and, if something
        // defined it as an ordinary property, as a slot — and a key listed twice is visited
        // twice by everything built on this.
        //
        // Only an array can produce that pair, and only if it also carries stored properties,
        // so everything else skips the pass entirely: this runs on every `for-in` and every
        // `Object.keys`, and a `HashSet` allocation per call is a poor trade for a case most
        // objects cannot reach. The scan is quadratic and deliberately so — it runs only for
        // an array with named properties, where the list is short.
        if is_array && stored_any {
            let mut at = 0;
            while at < names.len() {
                if names[..at].contains(&names[at]) {
                    names.remove(at);
                } else {
                    at += 1;
                }
            }
        }
        names
    })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "an index this large is unreachable"
)]
fn index_as_f64(index: usize) -> f64 {
    index as f64
}

/// Builds an array holding `values`, rooted while it is filled.
fn array_of_values(values: &[u64]) -> u64 {
    with_new_array(values.len(), |array| {
        for (index, value) in values.iter().enumerate() {
            with_runtime(|runtime| {
                runtime
                    .heap
                    .set_element(array, index, Value::from_bits(*value))
            });
        }
        array.to_value().to_bits()
    })
}

/// `Object.keys` and `Object.getOwnPropertyNames`.
///
/// The same function for both, which is **not** right in general — `getOwnPropertyNames`
/// includes non-enumerable properties and `keys` does not — and is right here, because nothing
/// can make a property non-enumerable yet. Recorded so the day descriptors land this splits.
extern "C" fn object_keys(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        if let Some(thrown) = reject_nullish(target, "cannot read the keys") {
            return thrown;
        }
        // Each string is stored **before the next one is made**. Collecting them into a Rust
        // vector first leaves every earlier string reachable from nothing the collector can
        // see while the next allocates — which under stress returns an array of freed cells.
        // **Only the enumerable ones**, which is the whole difference from
        // `getOwnPropertyNames` — and the reason the two stopped being the same function.
        names_as_array(&enumerable_keys(target))
    })
}

/// The own property names a `for-in` or `Object.keys` would see.
fn enumerable_keys(object: u64) -> Vec<String> {
    let Some(handle) = handle_of(object) else {
        return Vec::new();
    };
    // **A proxy is asked, key by key.** Its own shape holds nothing, so filtering by that
    // dropped every name `ownKeys` reported — `Object.keys` on a proxy answered empty however
    // many properties the target had. The specification says to ask
    // `getOwnPropertyDescriptor` for each key, which is also the only way a trap can make a
    // property enumerable that the target does not.
    if let Some((target, handler)) = proxy_parts(object) {
        return own_keys(object)
            .into_iter()
            .filter(|name| {
                let key = new_string(name);
                let descriptor =
                    with_rooted(&[object, key], || proxy_descriptor(target, handler, key));
                handle_of(descriptor).is_some_and(|_| {
                    is_truthy(Value::from_bits(property_of(descriptor, "enumerable")))
                })
            })
            .collect();
    }
    own_keys(object)
        .into_iter()
        .filter(|name| match own_property(object, name) {
            Some((slot, _)) => {
                with_runtime(|runtime| runtime.heap.attributes_of(handle, slot).enumerable)
            }
            // **An array's `length` is not enumerable**, and it has no slot to say so — which
            // is why the derived answer has to be asked for rather than assumed. Defaulting
            // an unslotted key to enumerable put `length` in `Object.keys([1])`.
            None => derived_own_property(object, name)
                .is_some_and(|(_, attributes)| attributes.enumerable),
        })
        .collect()
}

/// `Object.values`.
extern "C" fn object_values(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        // As in `object_keys`: read and store one at a time rather than collecting first.
        if let Some(thrown) = reject_nullish(target, "cannot read the values") {
            return thrown;
        }
        let names = enumerable_keys(target);
        with_new_array(names.len(), |array| {
            for (index, name) in names.iter().enumerate() {
                // SAFETY: `name` is a live Rust string.
                let value =
                    unsafe { crisol_property_load(target, name.as_ptr(), name.len() as u64) };
                with_runtime(|runtime| {
                    runtime
                        .heap
                        .set_element(array, index, Value::from_bits(value))
                });
            }
            array.to_value().to_bits()
        })
    })
}

/// `Object.create(proto)`.
extern "C" fn object_create(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let proto = unsafe { argument(argc, argv, 0) };
        // **A prototype is an object or `null`, and nothing else.** A number was neither
        // accepted nor refused — the object came back with `Object.prototype` still on it,
        // which is a third answer the specification does not have.
        if handle_of(proto).is_none() && Value::from_bits(proto).kind() != crisol_value::Kind::Null
        {
            return raise("a prototype must be an object or null", "TypeError");
        }
        let created = crisol_create_object();
        if let (Some(object), Some(parent)) = (handle_of(created), handle_of(proto)) {
            with_runtime(|runtime| runtime.heap.set_prototype(object, Some(parent)));
        } else if Value::from_bits(proto).kind() == crisol_value::Kind::Null {
            // `Object.create(null)` is the one way to get an object with no prototype at all.
            if let Some(object) = handle_of(created) {
                with_runtime(|runtime| runtime.heap.set_prototype(object, None));
            }
        }

        // **The second argument is a map of descriptors**, not of values —
        // `Object.create(p, {x: {value: 1}})` gives `x` the value one, and
        // `Object.create(p, {x: 1})` gives it no value at all, because `1` describes nothing.
        // Handed to `defineProperties` so the two agree by construction.
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        let descriptors = unsafe { argument(argc, argv, 1) };
        if handle_of(descriptors).is_some() {
            let arguments = [created, descriptors];
            let outcome = with_rooted(&arguments, || {
                object_define_properties(0, 0, 0, 2, arguments.as_ptr())
            });
            if Value::from_bits(outcome).is_exception() {
                return outcome;
            }
        }
        created
    })
}

/// `Object.getPrototypeOf(o)`.
extern "C" fn object_get_prototype(
    _closure: u64,
    _this: u64,
    argc_or: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let _ = argc_or;
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    if let Some(thrown) = reject_nullish(target, "cannot read the prototype") {
        return thrown;
    }
    if let Some((behind, handler)) = proxy_parts(target) {
        return proxy_prototype(behind, handler);
    }
    // **A primitive is coerced, not refused.** `Object.getPrototypeOf(1)` is
    // `Number.prototype`, because the specification wraps its argument first — and answering
    // `null` instead said the number had no prototype, which is a different claim entirely.
    let Some(handle) = handle_of(target) else {
        let prototype = match Value::from_bits(target).kind() {
            crisol_value::Kind::Number => NUMBER_PROTOTYPE.with(std::cell::Cell::get),
            crisol_value::Kind::Boolean => BOOLEAN_PROTOTYPE.with(std::cell::Cell::get),
            _ => None,
        };
        return prototype.map_or_else(|| Value::NULL.to_bits(), |p| p.to_value().to_bits());
    };
    with_runtime(|runtime| {
        runtime
            .heap
            .prototype_of(handle)
            .map_or_else(|| Value::NULL.to_bits(), |p| p.to_value().to_bits())
    })
}

/// Re-parents `target`, or says why it may not be.
///
/// Shared by `Object.setPrototypeOf`, `Reflect.setPrototypeOf` and the `__proto__` setter,
/// because the three are the same operation and disagreeing about the two refusals below
/// would mean a program could reach through whichever one checked least.
///
/// **A cycle is the refusal that matters.** `a.__proto__ = b; b.__proto__ = a` makes every
/// lookup that misses walk the pair forever. The walk is bounded (see
/// [`PROTOTYPE_CHAIN_LIMIT`]) so it returns rather than hangs, but absorbing a cycle at every
/// lookup is a worse deal than refusing to build one here. The other refusal is extensibility:
/// `Object.preventExtensions` freezes the prototype link too, which is what stops a sealed
/// object being re-parented out from under its own guarantees.
///
/// `Ok(false)` means the request was not a change worth making — `proto` was neither an object
/// nor `null`, which the specification ignores rather than faults.
fn set_prototype_of(handle: GcRef, proto: u64) -> Result<bool, &'static str> {
    let held = Value::from_bits(proto);
    let parent = if held.is_null() {
        None
    } else if held.kind() == crisol_value::Kind::Object {
        handle_of(proto)
    } else {
        return Ok(false);
    };

    let current = with_runtime(|runtime| runtime.heap.prototype_of(handle));
    if current == parent {
        // Setting the prototype it already has is not a change, so neither refusal applies —
        // a frozen object may be re-parented to where it already is.
        return Ok(true);
    }
    // Asked outside the borrow below: extensibility is a hidden *property*, so reading it is
    // a property load, and a property load enters the runtime itself.
    if !is_extensible(handle.to_value().to_bits()) {
        return Err("cannot change the prototype of a non-extensible object");
    }

    with_runtime(|runtime| {
        let mut walk = parent;
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let Some(step) = walk else { break };
            if step == handle {
                return Err("cyclic prototype chain");
            }
            walk = runtime.heap.prototype_of(step);
        }
        runtime.heap.set_prototype(handle, parent);
        Ok(true)
    })
}

/// `Object.setPrototypeOf(o, proto)`.
extern "C" fn object_set_prototype(
    _closure: u64,
    _this: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let proto = unsafe { argument(argc, argv, 1) };
    // **`null` and `undefined` are the error**, not "anything that is not an object": the
    // specification coerces the target, so `Object.setPrototypeOf(1, null)` answers `1` and
    // only a nullish one throws.
    if Value::from_bits(target).is_nullish() {
        return raise("cannot set the prototype of null or undefined", "TypeError");
    }
    let Some(handle) = handle_of(target) else {
        return target;
    };
    match set_prototype_of(handle, proto) {
        Ok(_) => target,
        Err(message) => raise(message, "TypeError"),
    }
}

/// `Object.hasOwn(o, key)`.
extern "C" fn object_has_own(
    _closure: u64,
    _this: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    if let Some(thrown) = reject_nullish(target, "cannot read a property") {
        return thrown;
    }
    // SAFETY: as above.
    let key = unsafe { argument(argc, argv, 1) };
    // **Own**, so the prototype chain is not walked — the whole point of the method — and
    // covering elements/`length`/characters through the shared `has_own_key` (D-237).
    boolean(has_own_key(target, key)).to_bits()
}

/// Whether writing `name` on `object` would be refused rather than performed.
///
/// An accessor is not refused here: a setter may accept the write, and one without a setter
/// is refused by the store itself.
fn refuses_assignment(object: u64, name: &str) -> bool {
    let Some(handle) = handle_of(object) else {
        return false;
    };
    match own_property(object, name) {
        Some((slot, _)) => {
            let attributes = with_runtime(|runtime| runtime.heap.attributes_of(handle, slot));
            !attributes.writable && !attributes.accessor
        }
        None => match derived_own_property(object, name) {
            Some((_, attributes)) => !attributes.writable,
            // **Absent is refused when the object is closed.** The write would *add* a
            // property, which is exactly what a non-extensible object will not do — and the
            // store below ignores it silently, so nothing downstream would have noticed.
            None => !is_extensible(object),
        },
    }
}

/// `Object.assign(target, …sources)`.
extern "C" fn object_assign(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        // **Coerced, like every other static's argument.** `Object.assign(true, …)` answers a
        // `Boolean` wrapper carrying the assignments, not the primitive `true` — which cannot
        // carry them and was handed straight back.
        let Some(target) = to_object(target) else {
            return raise("cannot assign to null or undefined", "TypeError");
        };
        // Rooted for the copy: a coerced target is a wrapper this call just made, and every
        // read and write below allocates.
        with_rooted(&[target], || {
            for position in 1..argc as usize {
                // SAFETY: as above.
                let source = unsafe { argument(argc, argv, position) };
                // **Only the enumerable ones.** `own_keys` includes an array's `length` and a
                // string wrapper's, so copying from either wrote a `length` the target had no
                // business having.
                for name in enumerable_keys(source) {
                    // **A read-only property on the target is a `TypeError` here**, not a write
                    // that quietly does nothing: `Object.assign` uses the throwing form of `Set`.
                    // It is one of the few places the difference is observable from source that
                    // is not in strict mode.
                    if refuses_assignment(target, &name) {
                        return raise("cannot assign to a read-only property", "TypeError");
                    }
                    // SAFETY: `name` is a live Rust string.
                    let value =
                        unsafe { crisol_property_load(source, name.as_ptr(), name.len() as u64) };
                    // SAFETY: as above.
                    unsafe {
                        crisol_property_store(target, name.as_ptr(), name.len() as u64, value);
                    }
                }
            }
            target
        })
    })
}

/// The first argument as a number, which is what every one-argument `Math` function takes.
fn math_argument(argc: u64, argv: *const u64) -> f64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    to_number(unsafe { argument(argc, argv, 0) })
}

/// Defines a one-argument `Math` function from a plain `f64` operation.
macro_rules! math_unary {
    ($name:ident, $doc:expr, $body:expr) => {
        #[doc = $doc]
        extern "C" fn $name(
            _closure: u64,
            _this_value: u64,
            _new_target: u64,
            argc: u64,
            argv: *const u64,
        ) -> u64 {
            let operation: fn(f64) -> f64 = $body;
            from_number(operation(math_argument(argc, argv)))
        }
    };
}

math_unary!(math_abs, "`Math.abs`.", f64::abs);
math_unary!(math_floor, "`Math.floor`.", f64::floor);
math_unary!(math_ceil, "`Math.ceil`.", f64::ceil);
math_unary!(math_trunc, "`Math.trunc`.", f64::trunc);
math_unary!(math_sqrt, "`Math.sqrt`.", f64::sqrt);
math_unary!(math_cbrt, "`Math.cbrt`.", f64::cbrt);
math_unary!(math_exp, "`Math.exp`.", f64::exp);
math_unary!(math_log, "`Math.log` — the natural logarithm.", f64::ln);
math_unary!(math_log2, "`Math.log2`.", f64::log2);
math_unary!(math_log10, "`Math.log10`.", f64::log10);
math_unary!(math_sin, "`Math.sin`.", f64::sin);
math_unary!(math_cos, "`Math.cos`.", f64::cos);
math_unary!(math_tan, "`Math.tan`.", f64::tan);
math_unary!(math_asin, "`Math.asin`.", f64::asin);
math_unary!(math_acos, "`Math.acos`.", f64::acos);
math_unary!(math_atan, "`Math.atan`.", f64::atan);

/// `Math.round`.
///
/// **Not `f64::round`.** JavaScript rounds a half *upward* — toward positive infinity — and
/// Rust rounds it *away from zero*. They agree on `0.5` and disagree on `-0.5`, which is `-0`
/// in JavaScript and `-1` in Rust. `floor(x + 0.5)` is the rule, with the non-finite cases
/// passed through because adding to an infinity or a `NaN` would not survive it.
extern "C" fn math_round(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let value = math_argument(argc, argv);
    if !value.is_finite() {
        return from_number(value);
    }
    from_number((value + 0.5).floor())
}

/// `Math.sign`.
///
/// **Not `f64::signum`**, which answers `1.0` for a zero and never `NaN`. JavaScript preserves
/// the zero's sign and propagates `NaN`, so all three of `0`, `-0` and `NaN` come back as
/// themselves.
extern "C" fn math_sign(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let value = math_argument(argc, argv);
    if value.is_nan() || value == 0.0 {
        return from_number(value);
    }
    from_number(if value > 0.0 { 1.0 } else { -1.0 })
}

/// `Math.atan2(y, x)` — note the order.
extern "C" fn math_atan2(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let y = to_number(unsafe { argument(argc, argv, 0) });
    // SAFETY: as above.
    let x = to_number(unsafe { argument(argc, argv, 1) });
    from_number(y.atan2(x))
}

/// `Math.pow`.
extern "C" fn math_pow(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let base = to_number(unsafe { argument(argc, argv, 0) });
    // SAFETY: as above.
    let exponent = to_number(unsafe { argument(argc, argv, 1) });
    from_number(base.powf(exponent))
}

/// `Math.hypot`.
extern "C" fn math_hypot(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let mut total = 0.0;
    for position in 0..argc as usize {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        let value = to_number(unsafe { argument(argc, argv, position) });
        total += value * value;
    }
    from_number(total.sqrt())
}

/// `min` and `max`, which differ only in which way they lean.
///
/// **No arguments gives the identity, and it is the *opposite* infinity each time**: `min()` is
/// `Infinity` and `max()` is `-Infinity`, because each has to lose to the first real argument.
/// **One `NaN` anywhere wins**, which `f64::min` does not do — it returns the other operand.
fn extremum(argc: u64, argv: *const u64, want_max: bool) -> u64 {
    let mut best = if want_max {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    };
    for position in 0..argc as usize {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        let value = to_number(unsafe { argument(argc, argv, position) });
        if value.is_nan() {
            return from_number(f64::NAN);
        }
        // `>` and `<` rather than `f64::max`, so `-0` and `0` keep the specification's order:
        // `Math.max(-0, 0)` is `0` and `Math.min(0, -0)` is `-0`.
        let better = if want_max {
            value > best || (value == 0.0 && best == 0.0 && best.is_sign_negative())
        } else {
            value < best || (value == 0.0 && best == 0.0 && value.is_sign_negative())
        };
        if better {
            best = value;
        }
    }
    from_number(best)
}

/// `Math.min`.
extern "C" fn math_min(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    extremum(argc, argv, false)
}

/// `Math.max`.
extern "C" fn math_max(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    extremum(argc, argv, true)
}

/// `Math.random`, over a xorshift generator seeded from the clock.
///
/// **Not suitable for anything that needs unpredictability**, and the specification does not
/// require it to be — it asks only for an implementation-dependent value in `[0, 1)`. Stated
/// here because "random" reads like a guarantee it is not making.
extern "C" fn math_random(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    RANDOM_STATE.with(|cell| {
        let mut state = cell.get();
        if state == 0 {
            state = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0x2545_f491_4f6c_dd1d, |since| since.as_nanos() as u64)
                | 1;
        }
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        cell.set(state);
        // The top 53 bits, which is exactly the mantissa a double can hold without rounding.
        #[expect(
            clippy::cast_precision_loss,
            reason = "53 bits is what an f64 represents exactly"
        )]
        let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
        from_number(unit)
    })
}

/// `Array.from(source, mapper?)`.
///
/// **Array-like before iterable.** The specification asks for an iterator first and falls back
/// to `length` — without `Symbol.iterator` usable as a key (D-149) there is nothing to ask, so
/// this takes what `for-of` takes (an array or a string) and otherwise reads `length` and
/// indexes, which covers any `{length, 0, 1, …}`. A user-defined iterable that is not
/// array-like answers an empty array rather than its elements: a gap, not a decision.
/// Whether `value` is iterable — has a callable `Symbol.iterator`, inherited or own.
fn has_symbol_iterator(value: u64) -> bool {
    iterator_key().is_some_and(|key| is_callable(symbol_property_load(value, &key)))
}

extern "C" fn array_from(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let source = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let mapper = unsafe { argument(argc, argv, 1) };
    // SAFETY: as above.
    let live = unsafe { live_values(source, argc, argv) };
    with_rooted(&live, || {
        let values: Vec<u64> = match elements_of(source) {
            Some((array, length)) => (0..length).map(|index| element_at(array, index)).collect(),
            None if Value::from_bits(source).kind() == crisol_value::Kind::String => {
                // **The code points are held only by `taken`**, and `array_of_values` below
                // allocates — so the array of them has to stay rooted until its contents are
                // somewhere the collector can see. Collecting into a `Vec` first roots
                // nothing (D-127).
                let taken = crisol_iterate(source);
                return with_rooted(&[taken], || {
                    let values: Vec<u64> = match elements_of(taken) {
                        Some((array, length)) => {
                            (0..length).map(|index| element_at(array, index)).collect()
                        }
                        None => Vec::new(),
                    };
                    if is_callable(mapper) {
                        map_into_array(&values, mapper)
                    } else {
                        array_of_values(&values)
                    }
                });
            }
            None => {
                // **An iterable takes the iterator path** — `Array.from(m.keys())` and
                // `Array.from(anySet)` drain via `Symbol.iterator` rather than reading a
                // `length` that an iterator does not have.
                if has_symbol_iterator(source) {
                    let taken = crisol_iterate(source);
                    if Value::from_bits(taken).is_exception() {
                        return taken;
                    }
                    return with_rooted(&[taken], || {
                        let values: Vec<u64> = match elements_of(taken) {
                            Some((array, length)) => {
                                (0..length).map(|index| element_at(array, index)).collect()
                            }
                            None => Vec::new(),
                        };
                        if is_callable(mapper) {
                            map_into_array(&values, mapper)
                        } else {
                            array_of_values(&values)
                        }
                    });
                }
                // Array-like: `length` and indices.
                let count = property_number(source, "length").unwrap_or(0.0);
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped to a length no array can exceed"
                )]
                let count = count.max(0.0).min(f64::from(u32::MAX)) as usize;
                (0..count)
                    .map(|index| {
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "an index below the clamp above"
                        )]
                        let key = number_text(index as f64);
                        // SAFETY: `key` is a live Rust string.
                        unsafe { crisol_property_load(source, key.as_ptr(), key.len() as u64) }
                    })
                    .collect()
            }
        };

        if is_callable(mapper) {
            map_into_array(&values, mapper)
        } else {
            array_of_values(&values)
        }
    })
}

/// Maps `values` through `mapper` into a fresh array.
///
/// **Into a rooted array one at a time**, because the callback allocates and a `Vec` of
/// results is invisible to the collector — every result but the newest would be freed under
/// it (D-127).
fn map_into_array(values: &[u64], mapper: u64) -> u64 {
    with_new_array(values.len(), |mapped| {
        for (index, value) in values.iter().enumerate() {
            let result = call_value(
                mapper,
                Value::UNDEFINED.to_bits(),
                &[*value, index_value(index)],
            );
            if Value::from_bits(result).is_exception() {
                return result;
            }
            with_runtime(|runtime| {
                runtime
                    .heap
                    .set_element(mapped, index, Value::from_bits(result));
            });
        }
        mapped.to_value().to_bits()
    })
}

/// The name of the accessor `Object.prototype` publishes for the prototype link.
///
/// **Deprecated in Annex B and universally shipped**, so it is not optional. It is a constant
/// rather than a literal because it is checked in two places that have to agree — the load
/// walk and the store walk — and neither may report it as a property anything owns.
const PROTO_ACCESSOR: &str = "__proto__";

/// Whether an object refuses new properties.
///
/// **Absent means extensible**, so an object nobody has frozen carries nothing. The flag is a
/// hidden property for the same reason a date's time is (D-126): there is nowhere else to put
/// one that `Object.keys` will not find.
const NOT_EXTENSIBLE: &str = "__sealed";

/// What an element permits, for the elements that do not permit everything.
///
/// **Elements have nowhere of their own to record attributes.** They live in a dense `Vec`
/// beside the object's slots rather than as entries in its shape, so `Object.freeze([1])` and
/// `Object.defineProperty(a, 0, {writable: false})` have nothing to write on. This is that
/// place: an array whose **first position is the rule for every element**, and whose position
/// `i + 1` overrides it for element `i`.
///
/// Two levels rather than one entry per element, because the two writers want different
/// things. Freezing restricts the whole run at once and must not cost an entry per element of
/// a million-element array; `defineProperty` restricts exactly one. A position holding
/// `undefined` is not an override, which is what keeps the second from having to know about
/// the first.
///
/// **Absent means ordinary**, so an array nobody restricts carries nothing at all and the
/// write path pays one shape lookup that misses — which is what it paid before this existed.
const ELEMENT_RULES: &str = "__elementRules";

/// What an element permits unless a rule says otherwise.
const ORDINARY_ELEMENT: crisol_value::Attributes = crisol_value::Attributes::DATA;

/// Whether `object` itself carries the bookkeeping flag `name`.
///
/// **Own, not inherited**, which a plain property read is not. These flags stand in for
/// internal slots, and an internal slot belongs to one object — reading one up the chain made
/// `Object.freeze(proto)` freeze every object created from it afterwards, which is a
/// prototype doing something only its own instances should be able to do to themselves.
fn own_flag(object: u64, name: &str) -> bool {
    own_property(object, name).is_some()
}

/// The three attribute bits, as the number a rule position holds.
fn pack_attributes(attributes: crisol_value::Attributes) -> f64 {
    f64::from(
        u8::from(attributes.writable)
            | (u8::from(attributes.enumerable) << 1)
            | (u8::from(attributes.configurable) << 2),
    )
}

/// The inverse of [`pack_attributes`].
fn unpack_attributes(bits: f64) -> crisol_value::Attributes {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "written by `pack_attributes`, which produces 0 to 7"
    )]
    let bits = bits as u8;
    crisol_value::Attributes {
        writable: bits & 1 != 0,
        enumerable: bits & 2 != 0,
        configurable: bits & 4 != 0,
        accessor: false,
    }
}

/// What element `index` of `object` permits. See [`ELEMENT_RULES`].
fn element_rule(object: u64, index: usize) -> crisol_value::Attributes {
    let Some((_, rules)) = own_property(object, ELEMENT_RULES) else {
        return ORDINARY_ELEMENT;
    };
    let Some((array, length)) = elements_of(rules.to_bits()) else {
        return ORDINARY_ELEMENT;
    };
    let at = |position: usize| {
        (position < length)
            .then(|| Value::from_bits(element_at(array, position)).as_number())
            .flatten()
    };
    // The override, then the whole-run rule, then the ordinary answer.
    at(index + 1)
        .or_else(|| at(0))
        .map_or(ORDINARY_ELEMENT, unpack_attributes)
}

/// Records what one element permits, or what every element permits when `index` is `None`.
fn set_element_rule(object: u64, index: Option<usize>, attributes: crisol_value::Attributes) {
    let Some(handle) = handle_of(object) else {
        return;
    };
    let existing = own_property(object, ELEMENT_RULES)
        .map(|(_, value)| value.to_bits())
        .filter(|rules| elements_of(*rules).is_some());
    let rules = match existing {
        Some(rules) => rules,
        None => {
            // Made on first use, so an array nobody restricts never allocates one. The
            // receiver stays rooted across it: everything that reaches here holds it as an
            // argument and is about to write to it.
            let made = with_rooted(&[object], || crisol_create_array(0));
            with_rooted(&[object, made], || {
                with_runtime(|runtime| {
                    runtime.define_hidden(handle, ELEMENT_RULES, Value::from_bits(made));
                });
            });
            made
        }
    };
    let Some(target) = handle_of(rules) else {
        return;
    };
    let position = index.map_or(0, |index| index + 1);
    // Rooted through the write: growing the rule array to reach `position` is an allocation,
    // and the array is reachable only from `object` — which is a bare `u64` here, not
    // something the collector can see on its own.
    with_rooted(&[object, rules], || {
        with_runtime(|runtime| {
            runtime
                .heap
                .set_element(target, position, Value::number(pack_attributes(attributes)));
        });
    });
}

/// Whether `object` still accepts new properties.
fn is_extensible(object: u64) -> bool {
    !own_flag(object, NOT_EXTENSIBLE)
}

/// Stops `object` accepting new properties.
fn prevent_extensions(object: u64) {
    if let Some(handle) = handle_of(object) {
        with_runtime(|runtime| {
            runtime.define_hidden(handle, NOT_EXTENSIBLE, Value::number(1.0));
        });
    }
}

/// Applies `attributes` to every own property of `object`.
fn restrict_own_properties(object: u64, writable: bool) {
    let Some(handle) = handle_of(object) else {
        return;
    };
    // Rooted for the whole pass: writing the element rules allocates, and `Object.freeze` is
    // called with its argument in the caller's frame rather than anywhere the collector has
    // been told to look.
    with_rooted(&[object], || {
        restrict_own_properties_rooted(object, handle, writable)
    });
}

/// The body of [`restrict_own_properties`], with the receiver already rooted.
fn restrict_own_properties_rooted(object: u64, handle: GcRef, writable: bool) {
    // **The elements first, and separately**, because they have no slots to carry attributes
    // and the loop below only reaches properties that do. Without this `Object.freeze([1])`
    // froze nothing at all: the array has no stored properties, so the loop ran zero times
    // and the call looked like it had worked.
    if let Some(count) = with_runtime(|runtime| runtime.heap.element_count(handle)) {
        let wanted = crisol_value::Attributes {
            writable,
            enumerable: true,
            configurable: false,
            accessor: false,
        };
        // **Read before the new rule is written**, and only if there is something to read:
        // an array with no rules has ordinary elements, which the whole-run rule already
        // describes, so the common case costs one lookup and no loop at all.
        let previous: Option<Vec<crisol_value::Attributes>> =
            own_flag(object, ELEMENT_RULES).then(|| {
                (0..count)
                    .map(|index| element_rule(object, index))
                    .collect()
            });
        set_element_rule(object, None, wanted);
        // Folded, not discarded: an element already made non-writable stays non-writable
        // when the object is only sealed.
        for (index, was) in previous.into_iter().flatten().enumerate() {
            let folded = crisol_value::Attributes {
                writable: writable && was.writable,
                enumerable: was.enumerable,
                configurable: false,
                accessor: false,
            };
            if folded != wanted {
                set_element_rule(object, Some(index), folded);
            }
        }
        if !writable {
            // A frozen array's `length` is not writable either, and that flag already exists
            // for the one place a length's permissions can live.
            with_runtime(|runtime| {
                runtime.define_hidden(handle, FIXED_LENGTH, Value::number(1.0));
            });
        }
    }
    for name in own_keys(object) {
        let Some((slot, _)) = own_property(object, &name) else {
            continue;
        };
        with_runtime(|runtime| {
            let current = runtime.heap.attributes_of(handle, slot);
            runtime.heap.set_attributes(
                handle,
                slot,
                crisol_value::Attributes {
                    // **Freezing keeps enumerability**, so a frozen object still lists its
                    // properties — it is the writing and the deleting that stop.
                    writable: writable && current.writable,
                    enumerable: current.enumerable,
                    configurable: false,
                    // **And it keeps being an accessor.** Clearing this turned the pair of
                    // functions in the slot into the property's *value*, so a frozen getter
                    // read back as a two-element array instead of being called.
                    accessor: current.accessor,
                },
            );
        });
    }
}

/// `Object.freeze`.
extern "C" fn object_freeze(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    if handle_of(target).is_some() {
        restrict_own_properties(target, false);
        prevent_extensions(target);
    }
    // **Answers its argument**, so `const o = Object.freeze({})` is the idiom it is.
    target
}

/// `Object.seal` — like freezing, but the values may still change.
extern "C" fn object_seal(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    if handle_of(target).is_some() {
        restrict_own_properties(target, true);
        prevent_extensions(target);
    }
    target
}

/// Whether every own property of `object` satisfies `ready`, and it is not extensible.
fn all_properties_are(object: u64, ready: impl Fn(crisol_value::Attributes) -> bool) -> bool {
    let Some(handle) = handle_of(object) else {
        // **A primitive is frozen and sealed**, vacuously: it has no properties to change.
        return true;
    };
    if is_extensible(object) {
        return false;
    }
    own_keys(object).into_iter().all(|name| {
        match own_property(object, &name) {
            Some((slot, _)) => ready(with_runtime(|runtime| {
                runtime.heap.attributes_of(handle, slot)
            })),
            // Unslotted keys were passed over, which made every array vacuously frozen the
            // moment it stopped being extensible — `Object.isFrozen([1])` said yes with the
            // element still writable.
            None => {
                derived_own_property(object, &name).is_none_or(|(_, attributes)| ready(attributes))
            }
        }
    })
}

/// `Object.isFrozen`.
extern "C" fn object_is_frozen(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    boolean(all_properties_are(target, |attributes| {
        // **An accessor has no writability to freeze**, so being non-configurable is the
        // whole of what frozen means for one. Asking about `writable` as well made every
        // object with a getter unfreezable.
        !attributes.configurable && (attributes.accessor || !attributes.writable)
    }))
    .to_bits()
}

/// `Object.isSealed`.
extern "C" fn object_is_sealed(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    boolean(all_properties_are(target, |attributes| {
        !attributes.configurable
    }))
    .to_bits()
}

/// `Object.preventExtensions`.
extern "C" fn object_prevent_extensions(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    if let Some((behind, handler)) = proxy_parts(target) {
        let outcome = proxy_prevent_extensions(behind, handler);
        return if Value::from_bits(outcome).is_exception() {
            outcome
        } else {
            target
        };
    }
    prevent_extensions(target);
    target
}

/// `Object.isExtensible`.
extern "C" fn object_is_extensible(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    if let Some((behind, handler)) = proxy_parts(target) {
        return proxy_is_extensible(behind, handler);
    }
    // **A primitive is never extensible**, which is the opposite of it being vacuously frozen.
    boolean(handle_of(target).is_some() && is_extensible(target)).to_bits()
}

/// `Object.entries` — `[key, value]` pairs, enumerable own properties only.
extern "C" fn object_entries(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        if let Some(thrown) = reject_nullish(target, "cannot read the entries") {
            return thrown;
        }
        let names = enumerable_keys(target);
        with_new_array(names.len(), |array| {
            for (index, name) in names.iter().enumerate() {
                // Built and stored one at a time: each pair allocates twice (D-127).
                let key = name.clone();
                // SAFETY: `key` is a live Rust string.
                let value = unsafe { crisol_property_load(target, key.as_ptr(), key.len() as u64) };
                let pair = with_rooted(&[value], || {
                    let text = new_string(name);
                    with_rooted(&[text], || array_of_values(&[text, value]))
                });
                with_runtime(|runtime| {
                    runtime
                        .heap
                        .set_element(array, index, Value::from_bits(pair));
                });
            }
            array.to_value().to_bits()
        })
    })
}

/// `Object.fromEntries` — the inverse of `entries`.
extern "C" fn object_from_entries(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let source = unsafe { argument(argc, argv, 0) };
        let object = crisol_create_object();
        with_rooted(&[object, source], || {
            let Some((pairs, length)) = elements_of(source) else {
                return;
            };
            let Some(handle) = handle_of(object) else {
                return;
            };
            for index in 0..length {
                let pair = element_at(pairs, index);
                let Some((entry, _)) = elements_of(pair) else {
                    continue;
                };
                let Some(name) = to_text(element_at(entry, 0)) else {
                    continue;
                };
                let value = element_at(entry, 1);
                with_runtime(|runtime| {
                    runtime.define(handle, &name, Value::from_bits(value));
                });
            }
        });
        object
    })
}

/// `Object.defineProperties`.
extern "C" fn object_define_properties(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let descriptors = unsafe { argument(argc, argv, 1) };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        if Value::from_bits(target).kind() != crisol_value::Kind::Object {
            return raise("cannot define properties on a non-object", "TypeError");
        }
        // The map of descriptors is coerced like any other argument, so a primitive is
        // wrapped and describes nothing — and a nullish one is the error.
        if let Some(thrown) = reject_nullish(descriptors, "cannot read the descriptors") {
            return thrown;
        }
        for name in enumerable_keys(descriptors) {
            let key = name.clone();
            // SAFETY: `key` is a live Rust string.
            let descriptor =
                unsafe { crisol_property_load(descriptors, key.as_ptr(), key.len() as u64) };
            let text = new_string(&name);
            let arguments = [target, text, descriptor];
            let outcome = with_rooted(&arguments, || {
                object_define_property(0, 0, 0, 3, arguments.as_ptr())
            });
            if Value::from_bits(outcome).is_exception() {
                return outcome;
            }
        }
        target
    })
}

/// `Object.is` — SameValue.
///
/// **Not `===` and not SameValueZero.** `Object.is(NaN, NaN)` is true where `===` says false,
/// and `Object.is(0, -0)` is *false* where both of the others say true. It is the only one of
/// the three that separates the zeroes.
extern "C" fn object_is(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let left = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let right = unsafe { argument(argc, argv, 1) };
    let (a, b) = (Value::from_bits(left), Value::from_bits(right));
    let same = match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) if x.is_nan() && y.is_nan() => true,
        // The sign is what distinguishes this from SameValueZero.
        (Some(x), Some(y)) => x == y && x.is_sign_negative() == y.is_sign_negative(),
        _ => same_value(a, b),
    };
    boolean(same).to_bits()
}

/// `Object.getOwnPropertySymbols`.
///
/// **Always empty**, and honestly so: a property key cannot be a symbol yet (D-149), so no
/// object has a symbol-keyed property for this to find. It exists because a program that calls
/// it should get an array rather than a `TypeError`.
extern "C" fn object_own_symbols(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        let Some(handle) = handle_of(target) else {
            return array_of_values(&[]);
        };
        // The symbols the shape names, in the order it names them — the counterpart of
        // `own_keys`, which reports every key that is *not* one of these.
        let symbols = with_runtime(|runtime| {
            let Some(shape) = runtime.heap.shape_of(handle) else {
                return Vec::new();
            };
            runtime
                .shapes
                .borrow()
                .properties(shape)
                .into_iter()
                .filter(|(key, slot)| {
                    key.is_symbol() && !runtime.heap.is_deleted(handle, slot.index())
                })
                .filter_map(|(key, _)| {
                    key.symbol_address()
                        .map(|address| Value::symbol(address).to_bits())
                })
                .collect::<Vec<u64>>()
        });
        with_rooted(&symbols, || array_of_values(&symbols))
    })
}

/// `Array.isArray(value)`.
extern "C" fn array_is_array(
    _closure: u64,
    _this: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    let is = handle_of(value)
        .is_some_and(|handle| with_runtime(|runtime| runtime.heap.element_count(handle).is_some()));
    if is { Value::TRUE } else { Value::FALSE }.to_bits()
}

/// `Array.of(…values)`.
extern "C" fn array_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let values: Vec<u64> = (0..argc as usize)
            // SAFETY: as above.
            .map(|position| unsafe { argument(argc, argv, position) })
            .collect();
        array_of_values(&values)
    })
}

/// One argument of a native call, or `undefined` if it was not passed.
///
/// # Safety
///
/// `argv` must point to `argc` readable values, which the convention guarantees.
unsafe fn argument(argc: u64, argv: *const u64, index: usize) -> u64 {
    if index as u64 >= argc || argv.is_null() {
        return Value::UNDEFINED.to_bits();
    }
    // SAFETY: bounds checked against the count the caller passed.
    unsafe { *argv.add(index) }
}

/// Calls `callee` with `this` and `args`, through the uniform convention.
///
/// This is how a built-in reaches a callback. The code address comes from the same lookup a
/// compiled call site uses, so a native calling `f` and compiled code calling `f` go to the
/// same place by construction.
fn call_value(callee: u64, this_value: u64, args: &[u64]) -> u64 {
    let code = crisol_closure_code(callee);
    if code.is_null() {
        return Value::UNDEFINED.to_bits();
    }
    // The convention requires at least one readable slot even for no arguments.
    let mut slots = args.to_vec();
    if slots.is_empty() {
        slots.push(Value::UNDEFINED.to_bits());
    }
    // SAFETY: `code` came from `crisol_closure_code`, which returns either a compiled function
    // or `crisol_not_a_function` — both have exactly this signature.
    let function: Native = unsafe { std::mem::transmute::<*const u8, Native>(code) };
    function(
        callee,
        this_value,
        Value::UNDEFINED.to_bits(),
        args.len() as u64,
        slots.as_ptr(),
    )
}

/// The heap and shape table a compiled program allocates into.
///
/// Compiled code cannot be handed a `Heap` to carry around — its calls into the runtime are C
/// functions taking machine words — so the two have to be reachable from a fixed place.
///
/// Thread-local rather than a global: `Heap` uses interior mutability and is deliberately not
/// `Sync`, and a shared heap would need a lock on every allocation. A per-thread heap is also
/// what the eventual design wants, since JavaScript's agents do not share objects.
#[derive(Debug)]
pub struct Runtime {
    /// Where objects live.
    pub heap: Heap,
    /// The shape tree every object's layout is drawn from.
    pub shapes: RefCell<Shapes>,
}

impl Runtime {
    fn new() -> Self {
        let heap = Heap::new();
        // ROADMAP §3.1 asks for a stress mode, and for compiled code it is the only way to
        // test rooting at all: a missing root normally shows up as a use-after-free under
        // memory pressure, far from the code that caused it. Collecting on every allocation
        // turns that into a failure on the very next line.
        //
        // An environment variable rather than a build feature, so the *shipped* binary can be
        // run under it — a stress mode that needs a recompile tests a different program from
        // the one that has the bug.
        if std::env::var_os("CRISOL_GC_STRESS").is_some() {
            heap.set_stress(true);
        }
        // SAFETY: collections here are triggered by allocation, and an allocation site is a
        // safepoint — which is exactly what `install_compiled_roots` asks its caller to
        // promise. Installing at construction rather than leaving it to the entry point means
        // there is no window in which the heap exists but cannot see compiled frames.
        unsafe { install_compiled_roots(&heap) };
        let runtime = Self {
            heap,
            shapes: RefCell::new(Shapes::new()),
        };
        // The object at the end of every chain has to exist before anything links to it, but
        // its methods are functions, so they cannot be made until `Function.prototype` does.
        runtime.allocate_object_prototype();
        // This next, because every function made afterwards links to it — including the ones
        // that live on it, and the ones on `Object.prototype` below.
        runtime.build_function_prototype();
        runtime.build_object_prototype();
        runtime.build_string_prototype();
        runtime.build_regexp_prototype();
        runtime.build_date_prototype();
        runtime.build_collection_prototypes();
        runtime.build_array_prototype();
        // Before `build_globals`: it points each constructor's `prototype` at the shared object
        // these fill, and the cells must hold something by then.
        runtime.build_typed_array_prototypes();
        runtime.build_globals();
        // **Last, because it needs both halves.** A symbol-keyed method needs the prototypes
        // *and* the well-known symbols, and the symbols are made inside `build_globals`.
        runtime.build_symbol_keyed_methods();
        runtime
    }

    /// Hangs the well-known-symbol methods on the prototypes that answer to them.
    ///
    /// **These are aliases, not new functions.** `Array.prototype[Symbol.iterator]` *is*
    /// `Array.prototype.values` — the specification says the same function object, and a test
    /// comparing the two would catch a copy. So each is read back off the prototype and
    /// defined a second time under the symbol.
    fn build_symbol_keyed_methods(&self) {
        let Some(globals) = GLOBALS.with(std::cell::Cell::get) else {
            return;
        };
        let Some(symbol) = self.global_object(globals, "Symbol") else {
            return;
        };
        let iterator = {
            let key = PropertyKey::new("iterator");
            self.heap
                .shape_of(symbol)
                .and_then(|shape| self.shapes.borrow().lookup(shape, &key))
                .and_then(|slot| self.heap.get(symbol, slot.index()))
        };
        let Some(iterator) = iterator.and_then(|value| value.as_address()) else {
            return;
        };
        let key = PropertyKey::symbol(iterator, "Symbol.iterator");
        // Rooted like any other symbol a shape names (D-192). It is also reachable from
        // `Symbol.iterator`, but a program may delete that and the shapes would outlive it.
        KEY_SYMBOLS.with(|symbols| {
            if let Ok(mut entries) = symbols.try_borrow_mut() {
                entries.insert(Value::symbol(iterator).to_bits());
            }
        });
        // **Only `Array.prototype` for now**, because an alias needs something to alias.
        // `Map` and `Set` have no `values` or `entries` yet, and `String.prototype`'s
        // iteration is the character walk `crisol_iterate` already performs — pointing the
        // symbol at some other method would be worse than leaving the fast path to answer.
        // The loop is a loop because the list is the part that grows.
        // **`Symbol.iterator` is an alias to a named method**, the same object: `Array` and
        // `Set` iterate by `values`, `Map` by `entries` — which is why `for (const [k, v] of m)`
        // destructures a pair. Each is read back off its prototype and defined a second time
        // under the symbol.
        for (cell, name) in [
            (&ARRAY_PROTOTYPE, "values"),
            (&MAP_PROTOTYPE, "entries"),
            (&SET_PROTOTYPE, "values"),
            (&TYPED_ARRAY_PROTOTYPE, "values"),
        ] {
            let Some(prototype) = cell.with(std::cell::Cell::get) else {
                continue;
            };
            let existing = {
                let named = PropertyKey::new(name);
                self.heap
                    .shape_of(prototype)
                    .and_then(|shape| self.shapes.borrow().lookup(shape, &named))
                    .and_then(|slot| self.heap.get(prototype, slot.index()))
            };
            let Some(method) = existing else {
                continue;
            };
            self.define_keyed(prototype, &key, method);
        }

        // **`%TypedArray%.prototype[Symbol.toStringTag]` is a getter**, so
        // `Object.prototype.toString.call(new Int8Array())` reads `"Int8Array"` and answers
        // `"[object Int8Array]"`. Installed as an accessor under the well-known symbol, which is
        // read off `Symbol` the way the regular-expression ones below are.
        if let Some(prototype) = TYPED_ARRAY_PROTOTYPE.with(std::cell::Cell::get) {
            let tag = {
                let named = PropertyKey::new("toStringTag");
                self.heap
                    .shape_of(symbol)
                    .and_then(|shape| self.shapes.borrow().lookup(shape, &named))
                    .and_then(|slot| self.heap.get(symbol, slot.index()))
                    .and_then(|value| value.as_address())
            };
            if let Some(tag) = tag {
                KEY_SYMBOLS.with(|symbols| {
                    if let Ok(mut entries) = symbols.try_borrow_mut() {
                        entries.insert(Value::symbol(tag).to_bits());
                    }
                });
                let getter = self.native_function(typed_natives_base() + TA_TAG_GET);
                let tag_key = PropertyKey::symbol(tag, "Symbol.toStringTag");
                self.install_accessor_keyed(prototype, &tag_key, getter);
            }
        }

        // **An iterator is its own iterable.** `%IteratorPrototype%` defines
        // `[Symbol.iterator]` to return `this`, which is what lets `Array.from(map.keys())` and
        // `[...anIterator]` drain an iterator directly rather than only the collection behind it.
        if let Some(prototype) = ARRAY_ITERATOR_PROTOTYPE.with(std::cell::Cell::get) {
            let index =
                NATIVES.len() + GLOBAL_NATIVES.len() + NAMESPACE_NATIVES.len() + ITERATOR_SELF;
            let function = self.native_function(index);
            self.define_keyed(prototype, &key, function.to_value());
        }

        // **A string answers `Symbol.iterator` with a code-point iterator.** `[...s]` already
        // works through the fast path, but `s[Symbol.iterator]()` needs the method itself.
        if let Some(prototype) = STRING_PROTOTYPE.with(std::cell::Cell::get) {
            let index =
                NATIVES.len() + GLOBAL_NATIVES.len() + NAMESPACE_NATIVES.len() + STRING_ITERATOR;
            let function = self.native_function(index);
            self.define_keyed(prototype, &key, function.to_value());
            let text = self.string("[Symbol.iterator]");
            self.define_named(function, "name", text);
        }

        // **The regular-expression protocol.** `String.prototype.match` is *defined* as
        // asking the pattern, and the pattern is defined as asking `exec` — so a subclass
        // that overrides either changes what every string method does. Without these the
        // string methods did the work themselves and both hooks were unreachable.
        //
        // New functions rather than aliases, unlike `Symbol.iterator` above: there is no
        // named method on `RegExp.prototype` that does any of these, so there is nothing to
        // alias.
        let Some(prototype) = REGEXP_PROTOTYPE.with(std::cell::Cell::get) else {
            return;
        };
        for (offset, name) in REGEXP_SYMBOL_NAMES.iter().enumerate() {
            let named = PropertyKey::new(name);
            let held = self
                .heap
                .shape_of(symbol)
                .and_then(|shape| self.shapes.borrow().lookup(shape, &named))
                .and_then(|slot| self.heap.get(symbol, slot.index()));
            let Some(address) = held.and_then(|value| value.as_address()) else {
                continue;
            };
            // Rooted for the same reason `Symbol.iterator` is: a shape names it, and a
            // program may delete the property it was read from (D-192).
            KEY_SYMBOLS.with(|symbols| {
                if let Ok(mut entries) = symbols.try_borrow_mut() {
                    entries.insert(Value::symbol(address).to_bits());
                }
            });
            let index = NATIVES.len()
                + GLOBAL_NATIVES.len()
                + NAMESPACE_NATIVES.len()
                + REGEXP_SYMBOL_METHODS
                + offset;
            let function = self.native_function(index);
            // **On the prototype before anything else is allocated.** `native_function` hands
            // back an unrooted handle, so the function is reachable only once it is stored —
            // and building its `name` allocates, which under stress freed it between the two
            // lines. `typeof` then answered `"object"`, because what came back was a
            // different cell.
            let key = PropertyKey::symbol(address, &format!("Symbol.{name}"));
            self.define_keyed(prototype, &key, function.to_value());
            let text = self.string(&format!("[Symbol.{name}]"));
            self.define_named(function, "name", text);
            self.define_named(function, "length", Value::number(1.0));
        }

        // **`Symbol.toStringTag` on the namespaces that carry one**, so
        // `Object.prototype.toString.call(Math)` is `"[object Math]"` rather than `"[object
        // Object]"`. The tag is read by `object_to_text`; defining it here is what gives it
        // something to read.
        let tag = {
            let named = PropertyKey::new("toStringTag");
            self.heap
                .shape_of(symbol)
                .and_then(|shape| self.shapes.borrow().lookup(shape, &named))
                .and_then(|slot| self.heap.get(symbol, slot.index()))
                .and_then(|value| value.as_address())
        };
        if let Some(tag) = tag {
            KEY_SYMBOLS.with(|symbols| {
                if let Ok(mut entries) = symbols.try_borrow_mut() {
                    entries.insert(Value::symbol(tag).to_bits());
                }
            });
            let key = PropertyKey::symbol(tag, "Symbol.toStringTag");
            for name in ["Math", "JSON", "Reflect"] {
                if let Some(namespace) = self.global_object(globals, name) {
                    let text = self.string(name);
                    self.define_keyed(namespace, &key, text);
                }
            }
        }
    }

    /// Writes `value` under a key that may name a symbol, with a method's attributes.
    ///
    /// The named `define` takes a `&str`, which a symbol has no identifying one of — see
    /// D-193, where routing one through its description made two symbols the same property.
    fn define_keyed(&self, object: GcRef, key: &PropertyKey, value: Value) {
        let Some(current) = self.heap.shape_of(object) else {
            return;
        };
        let (shape, slot, width) = {
            let mut shapes = self.shapes.borrow_mut();
            let shape = shapes.add(current, key);
            let Some(slot) = shapes.lookup(shape, key) else {
                return;
            };
            (shape, slot, shapes.len(shape) as usize)
        };
        if shape != current {
            self.heap.transition(object, shape, width);
        }
        self.heap.set(object, slot.index(), value);
        self.heap.set_attributes(
            object,
            slot.index(),
            crisol_value::Attributes {
                writable: true,
                enumerable: false,
                configurable: true,
                accessor: false,
            },
        );
    }

    /// Writes `value` as a property of `object`, transitioning its shape.
    ///
    /// The same three steps `crisol_property_store` takes, without going through a raw pointer
    /// — the runtime knows its own keys.
    fn define(&self, object: GcRef, name: &str, value: Value) {
        let key = PropertyKey::new(name);
        let Some(current) = self.heap.shape_of(object) else {
            return;
        };
        let (target, slot, width) = {
            let mut shapes = self.shapes.borrow_mut();
            let target = shapes.add(current, &key);
            let Some(slot) = shapes.lookup(target, &key) else {
                return;
            };
            (target, slot, shapes.len(target) as usize)
        };
        if target != current {
            self.heap.transition(object, target, width);
        }
        self.heap.set(object, slot.index(), value);
    }

    /// Allocates a string using this runtime directly.
    ///
    /// Not `new_string`, which goes through `with_runtime` — during construction that would
    /// re-enter the thread-local currently being initialised.
    /// A symbol, made during construction.
    ///
    /// The same distinction as [`Runtime::string`] and for the same reason: the free
    /// `new_symbol` goes through `with_runtime`, and calling it while the runtime is being
    /// built re-enters the thread-local currently being initialised. That does not fail
    /// gracefully — every compiled program died on startup, before `main` reached any of its
    /// own code.
    fn symbol(&self, description: Option<&str>) -> Value {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let cell = scope.alloc(shape, 0);
        if let Some(prototype) = SYMBOL_PROTOTYPE.with(std::cell::Cell::get) {
            self.heap.set_prototype(cell.handle(), Some(prototype));
        }
        if let Some(text) = description {
            let described = self.string(text);
            self.define_hidden(cell.handle(), SYMBOL_DESCRIPTION, described);
        }
        cell.handle()
            .to_value()
            .as_address()
            .map_or(Value::UNDEFINED, Value::symbol)
    }

    fn string(&self, text: &str) -> Value {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let cell = scope.alloc(shape, 0);
        self.heap.make_string(cell.handle(), text);
        cell.handle()
            .to_value()
            .as_address()
            .map_or(Value::UNDEFINED, Value::string)
    }

    /// The global named `name`, if it is an object.
    fn global_object(&self, globals: GcRef, name: &str) -> Option<GcRef> {
        let key = PropertyKey::new(name);
        let shape = self.heap.shape_of(globals)?;
        let slot = self.shapes.borrow().lookup(shape, &key)?;
        self.heap
            .get(globals, slot.index())
            .and_then(|value| value.as_address())
            .map(GcRef::from_address)
    }

    /// The global named `name`, creating it if it is not there.
    ///
    /// Callable only when it should be. `Object` and `Array` are constructors as well as
    /// namespaces; `Math`, `JSON` and `Reflect` are **not functions at all** — `Math()` is a
    /// `TypeError` — and giving them a body made `typeof Math` answer `"function"` and
    /// `new Reflect()` answer an object.
    fn ensure_global_object(&self, globals: GcRef, name: &str) -> GcRef {
        if let Some(existing) = self.global_object(globals, name) {
            return existing;
        }
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let callable = !NAMESPACES_ONLY.contains(&name);
        // One internal slot for a callable, holding the index of the built-in it runs; none
        // at all for a namespace, because internal zero is exactly what makes an object a
        // function (see `is_callable`).
        let object = scope.alloc_with_internals(shape, 0, usize::from(callable));
        // **A namespace is an ordinary object**, and nothing linked it to one. `Math`, `JSON`,
        // `Reflect`, `Object` and `Array` all reached the end of their chain immediately, so
        // `Math.hasOwnProperty(…)` was not a function — on the objects a program is most
        // likely to ask that of.
        self.inherit_from_object(object.handle());
        if callable {
            #[expect(
                clippy::cast_precision_loss,
                reason = "there are a handful of built-ins"
            )]
            let encoded = -((NATIVES.len()
                + GLOBAL_NATIVES.len()
                + NAMESPACE_NATIVES.len()
                + CONSTRUCT_PLAIN_OBJECT) as f64
                + 1.0);
            self.heap
                .set_internal(object.handle(), 0, Value::number(encoded));
            // A function's own name and arity. A namespace has neither: `Math.name` is
            // `undefined`, and a `length` on it would be a property the specification does
            // not give it.
            let text = self.string(name);
            self.define_named(object.handle(), "name", text);
            if let Some(arity) = arity_of("global", name) {
                self.define_named(object.handle(), "length", Value::number(f64::from(arity)));
            }
        }
        self.define(globals, name, object.to_value());
        object.handle()
    }

    /// Builds the object every unresolved name is looked up in.
    ///
    /// Rooted before anything is put in it, because each entry allocates and under stress each
    /// allocation collects.
    fn build_globals(&self) {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let globals = scope.alloc(shape, 0);
        GLOBALS.with(|cell| cell.set(Some(globals.handle())));

        for (index, (name, _)) in GLOBAL_NATIVES.iter().enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "there are a handful of globals")]
            let encoded = -((NATIVES.len() + index) as f64 + 1.0);
            let function = scope.alloc_with_internals(shape, 0, 1);
            self.heap
                .set_internal(function.handle(), 0, Value::number(encoded));
            // A constructor needs a `prototype` for `instanceof` to find, exactly as a compiled
            // function does — and not enumerable, exactly as a compiled function's is not.
            // `define_hidden` gives the attribute set the specification names here: writable,
            // neither enumerable nor configurable.
            let prototype = scope.alloc(shape, 0);
            self.define_hidden(function.handle(), "prototype", prototype.to_value());
            self.define_linking(prototype.handle(), "constructor", function.to_value());
            // The constructor's own name. Not enumerable, for the same reason a method's
            // name is not.
            let text = self.string(name);
            self.define_named(function.handle(), "name", text);
            if let Some(arity) = arity_of("global", name) {
                self.define_named(function.handle(), "length", Value::number(f64::from(arity)));
            }
            self.define(globals.handle(), name, function.to_value());
        }
        // `Object` and `Array` are functions that also carry methods. Created here rather than
        // in `GLOBAL_NATIVES` because they need properties hung off them, and the constructor
        // they answer to is the same `make_error`-shaped thing: called or `new`ed, it returns
        // an object.
        for (index, (namespace, method, _)) in NAMESPACE_NATIVES.iter().enumerate() {
            let owner = self.ensure_global_object(globals.handle(), namespace);
            let function = self.native_function(NATIVES.len() + GLOBAL_NATIVES.len() + index);
            self.define_method(owner, namespace, method, function.to_value());
        }
        // **An error's kind lives on its prototype.** `new TypeError("x").name` is `"TypeError"`
        // and `Object.keys` of the instance is empty, which only works if the string is on the
        // prototype rather than copied onto each error. The chain runs through `Error.prototype`,
        // so `e instanceof Error` is true for every kind of error — which is how most code
        // that catches one asks what it caught.
        for name in [
            "Error",
            "TypeError",
            "RangeError",
            "ReferenceError",
            "SyntaxError",
        ] {
            let Some(constructor) = self.global_object(globals.handle(), name) else {
                continue;
            };
            let key = PropertyKey::new("prototype");
            let prototype = self
                .heap
                .shape_of(constructor)
                .and_then(|shape| self.shapes.borrow().lookup(shape, &key))
                .and_then(|slot| self.heap.get(constructor, slot.index()))
                .and_then(|value| value.as_address())
                .map(GcRef::from_address);
            let Some(prototype) = prototype else {
                continue;
            };
            let text = self.string(name);
            self.define_linking(prototype, "name", text);
            let empty = self.string("");
            self.define_linking(prototype, "message", empty);
            if name == "Error" {
                self.inherit_from_object(prototype);
                let method = self.native_function(
                    NATIVES.len() + GLOBAL_NATIVES.len() + NAMESPACE_NATIVES.len() + ERROR_TO_TEXT,
                );
                self.define_method(prototype, "Error.prototype", "toString", method.to_value());
            } else if let Some(base) =
                self.global_object(globals.handle(), "Error")
                    .and_then(|base| {
                        let shape = self.heap.shape_of(base)?;
                        let slot = self.shapes.borrow().lookup(shape, &key)?;
                        self.heap
                            .get(base, slot.index())
                            .and_then(|value| value.as_address())
                            .map(GcRef::from_address)
                    })
            {
                self.heap.set_prototype(prototype, Some(base));
            }
        }

        // **`Array` runs its own constructor.** `ensure_global_object` gives every namespace the
        // plain-object body, which is right for `Object` and wrong here — `Array(3)` has to be
        // three elements long. Re-pointed rather than special-cased in that helper, because the
        // helper's job is to make a namespace exist and this is about what one of them does.
        if let Some(array) = self.global_object(globals.handle(), "Array") {
            #[expect(
                clippy::cast_precision_loss,
                reason = "there are a handful of built-ins"
            )]
            let encoded = -((NATIVES.len()
                + GLOBAL_NATIVES.len()
                + NAMESPACE_NATIVES.len()
                + CONSTRUCT_ARRAY) as f64
                + 1.0);
            self.heap.set_internal(array, 0, Value::number(encoded));
        }

        // Each constructor's `prototype` is the object its instances already inherit from, not
        // a new one — otherwise `[].map === Array.prototype.map` would be false, and the same
        // for every other pair.
        for (name, cell) in [
            ("Array", ARRAY_PROTOTYPE.with(std::cell::Cell::get)),
            ("Function", FUNCTION_PROTOTYPE.with(std::cell::Cell::get)),
            ("String", STRING_PROTOTYPE.with(std::cell::Cell::get)),
            ("RegExp", REGEXP_PROTOTYPE.with(std::cell::Cell::get)),
            ("Date", DATE_PROTOTYPE.with(std::cell::Cell::get)),
            ("Object", OBJECT_PROTOTYPE.with(std::cell::Cell::get)),
            ("Map", MAP_PROTOTYPE.with(std::cell::Cell::get)),
            ("Set", SET_PROTOTYPE.with(std::cell::Cell::get)),
            ("Symbol", SYMBOL_PROTOTYPE.with(std::cell::Cell::get)),
            ("Number", NUMBER_PROTOTYPE.with(std::cell::Cell::get)),
            ("Boolean", BOOLEAN_PROTOTYPE.with(std::cell::Cell::get)),
            ("Promise", PROMISE_PROTOTYPE.with(std::cell::Cell::get)),
        ] {
            if let (Some(constructor), Some(prototype)) =
                (self.global_object(globals.handle(), name), cell)
            {
                self.define_hidden(constructor, "prototype", prototype.to_value());
                // **And the link back.** `({}).constructor === Object` is how a program asks
                // what made something, and `Array.prototype.constructor` is what a subclass
                // replaces; neither existed, so both answered `undefined`.
                self.define_linking(prototype, "constructor", constructor.to_value());
            }
        }

        // The typed-array family, kept apart because the per-kind prototypes come from an array
        // indexed by kind rather than a named cell, and because the constructors carry members
        // (`BYTES_PER_ELEMENT`, `isView`) the ones above do not.
        if let (Some(constructor), Some(prototype)) = (
            self.global_object(globals.handle(), "ArrayBuffer"),
            ARRAY_BUFFER_PROTOTYPE.with(std::cell::Cell::get),
        ) {
            self.define_hidden(constructor, "prototype", prototype.to_value());
            self.define_linking(prototype, "constructor", constructor.to_value());
            let is_view = self.native_function(typed_natives_base() + AB_IS_VIEW);
            self.define_method(constructor, "ArrayBuffer", "isView", is_view.to_value());
        }
        for kind in ELEMENT_KINDS {
            let name = TYPED_ARRAY_NAMES[kind.tag()];
            let prototype = TYPED_ARRAY_PROTOTYPES.with(|protos| protos[kind.tag()].get());
            if let (Some(constructor), Some(prototype)) =
                (self.global_object(globals.handle(), name), prototype)
            {
                self.define_hidden(constructor, "prototype", prototype.to_value());
                self.define_linking(prototype, "constructor", constructor.to_value());
                // `BYTES_PER_ELEMENT` lives on the constructor as well as the prototype.
                self.define_frozen(
                    constructor,
                    "BYTES_PER_ELEMENT",
                    Value::number(index_number(kind.bytes())),
                );
            }
        }

        // **Every global that is a function inherits from `Function.prototype`.** They did
        // not: a global was built before that object existed, so `Date.bind` was `undefined`
        // and `Object instanceof Function` was false — on the objects a program is most
        // likely to ask either of. The prototype methods already had it, which is what made
        // the gap hard to see: `Math.max.bind` worked and `Date.bind` did not.
        //
        // Done here rather than at each creation because this is the first point where
        // `Function.prototype` exists, and one late pass is better than three early ones that
        // have to be kept in the right order.
        if let Some(functions) = FUNCTION_PROTOTYPE.with(std::cell::Cell::get) {
            for (name, _) in GLOBAL_NATIVES {
                if let Some(global) = self.global_object(globals.handle(), name) {
                    self.heap.set_prototype(global, Some(functions));
                }
            }
            for name in ["Object", "Array"] {
                if let Some(global) = self.global_object(globals.handle(), name) {
                    self.heap.set_prototype(global, Some(functions));
                }
            }
        }

        // The well-known symbols, as values on `Symbol`. A `PropertyKey` carries a symbol's
        // address now, so these are usable as property keys and not merely readable.
        //
        // **All of them, including the ones nothing consults yet.** A program branches on
        // whether `Symbol.species` exists far more often than it uses it — that is what a
        // feature test is — and one that is `undefined` sends the program down a path written
        // for an engine from before it existed. Defining the symbol is a value; acting on it
        // is a separate question per symbol, and answering the first does not pretend to
        // answer the second.
        if let Some(symbol) = self.global_object(globals.handle(), "Symbol") {
            for name in [
                "iterator",
                "asyncIterator",
                "hasInstance",
                "toPrimitive",
                "toStringTag",
                "species",
                "match",
                "matchAll",
                "replace",
                "search",
                "split",
                "isConcatSpreadable",
                "unscopables",
            ] {
                let value = self.symbol(Some(&format!("Symbol.{name}")));
                // **Neither writable nor configurable**, which is what the specification says
                // and what `Object.getOwnPropertyDescriptor(Symbol, "iterator")` checks.
                self.define_frozen(symbol, name, value);
            }
        }

        // `Math`'s constants, which are properties rather than functions and so have no table
        // entry. The object itself already exists: naming a method in `NAMESPACE_NATIVES` is
        // what creates it.
        // `Number`'s constants, which are properties rather than functions.
        if let Some(number) = self.global_object(globals.handle(), "Number") {
            for (name, value) in [
                ("MAX_SAFE_INTEGER", crisol_builtins::MAX_SAFE_INTEGER),
                ("MIN_SAFE_INTEGER", -crisol_builtins::MAX_SAFE_INTEGER),
                ("MAX_VALUE", f64::MAX),
                ("MIN_VALUE", f64::MIN_POSITIVE),
                ("EPSILON", f64::EPSILON),
                ("POSITIVE_INFINITY", f64::INFINITY),
                ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
                ("NaN", f64::NAN),
            ] {
                // **Not enumerable, and neither writable nor configurable.** Defined as
                // ordinary properties they turned up in `Object.keys(Number)`, and — worse —
                // `Object.defineProperties(o, Math)` read `3.14159…` as a property
                // descriptor, because a namespace's constants are exactly what that walks.
                self.define_frozen(number, name, Value::number(value));
            }
        }

        if let Some(math) = self.global_object(globals.handle(), "Math") {
            for (name, value) in [
                ("PI", std::f64::consts::PI),
                ("E", std::f64::consts::E),
                ("LN2", std::f64::consts::LN_2),
                ("LN10", std::f64::consts::LN_10),
                ("LOG2E", std::f64::consts::LOG2_E),
                ("LOG10E", std::f64::consts::LOG10_E),
                ("SQRT2", std::f64::consts::SQRT_2),
                ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
            ] {
                self.define_frozen(math, name, Value::number(value));
            }
        }
        self.define(globals.handle(), "globalThis", globals.to_value());
        self.define(globals.handle(), "undefined", Value::UNDEFINED);
        self.define(globals.handle(), "NaN", Value::number(f64::NAN));
        self.define(globals.handle(), "Infinity", Value::number(f64::INFINITY));
    }

    /// Allocates a built-in function: its index, its `prototype`, and its own prototype link.
    ///
    /// One place, so a function made here and one made by `crisol_create_closure` agree about
    /// what a function *is* — a cell whose internal zero says which code it runs, inheriting
    /// from `Function.prototype` so `call` and `apply` are reachable.
    fn native_function(&self, index: usize) -> GcRef {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let function = scope.alloc_with_internals(shape, 0, 1);
        #[expect(
            clippy::cast_precision_loss,
            reason = "there are a handful of built-ins"
        )]
        let encoded = -((index as f64) + 1.0);
        self.heap
            .set_internal(function.handle(), 0, Value::number(encoded));
        if let Some(prototype) = FUNCTION_PROTOTYPE.with(std::cell::Cell::get) {
            self.heap.set_prototype(function.handle(), Some(prototype));
        }
        function.handle()
    }

    /// Defines a built-in method, which is **not enumerable**.
    ///
    /// Every method the specification puts on a prototype is `{ writable: true, enumerable:
    /// false, configurable: true }`. Defining them as ordinary properties made `for (k in [])`
    /// visit `map`, `filter` and every other array method — the loop was right and the
    /// properties were wrong.
    fn define_method(&self, object: GcRef, owner: &str, name: &str, value: Value) {
        // **Stored before anything else is allocated.** `native_function` hands back an
        // unrooted handle, so the function is only reachable once it is on the prototype —
        // allocating first leaves a window where a collection frees the thing being defined.
        // The symptom is a method that is `undefined` under GC stress and fine without it.
        self.define(object, name, value);

        let key = PropertyKey::new(name);
        let slot = self
            .heap
            .shape_of(object)
            .and_then(|shape| self.shapes.borrow().lookup(shape, &key));
        if let Some(slot) = slot {
            self.heap.set_attributes(
                object,
                slot.index(),
                crisol_value::Attributes {
                    writable: true,
                    enumerable: false,
                    configurable: true,
                    accessor: false,
                },
            );
        }

        // **A function knows its own name.** `Array.prototype.forEach.name` is `"forEach"`, and
        // test262 checks it for every built-in it covers — the name was in the table that
        // created the function and was simply never written down on it. Safe here: the
        // function is reachable from `object`, which the caller roots.
        if let Some(function) = value.as_address().map(GcRef::from_address) {
            let text = self.string(name);
            self.define_named(function, "name", text);
            // **And how many arguments it declares**, which carries the same attributes as
            // the name and is checked just as often. Only where [`ARITIES`] knows: a method
            // it does not list keeps having none, which is a missing answer rather than a
            // wrong one.
            if let Some(arity) = arity_of(owner, name) {
                self.define_named(function, "length", Value::number(f64::from(arity)));
            }
        }
    }

    /// Defines a built-in's `name`, which is not writable but is configurable.
    ///
    /// Those are the attributes the specification gives it: a program cannot assign to
    /// `f.name` but can redefine it, which is what makes `Object.defineProperty(f, "name", …)`
    /// work where `f.name = "x"` silently does nothing.
    fn define_named(&self, object: GcRef, name: &str, value: Value) {
        self.define(object, name, value);
        let key = PropertyKey::new(name);
        let slot = self
            .heap
            .shape_of(object)
            .and_then(|shape| self.shapes.borrow().lookup(shape, &key));
        if let Some(slot) = slot {
            self.heap.set_attributes(
                object,
                slot.index(),
                crisol_value::Attributes {
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    accessor: false,
                },
            );
        }
    }

    /// Defines a link enumeration skips but a program may still replace or remove.
    ///
    /// The attribute set `constructor` has — writable, not enumerable, configurable. It is
    /// **not** `define_method`, because that one also names the function it defines, and
    /// `Object.prototype.constructor` has to keep being called `Object`.
    fn define_linking(&self, object: GcRef, name: &str, value: Value) {
        self.define(object, name, value);
        let key = PropertyKey::new(name);
        let slot = self
            .heap
            .shape_of(object)
            .and_then(|shape| self.shapes.borrow().lookup(shape, &key));
        if let Some(slot) = slot {
            self.heap.set_attributes(
                object,
                slot.index(),
                crisol_value::Attributes {
                    writable: true,
                    enumerable: false,
                    configurable: true,
                    accessor: false,
                },
            );
        }
    }

    /// Defines a constant: readable, and nothing else.
    ///
    /// The attribute set the specification gives `Math.PI` and `Number.MAX_VALUE` — none of
    /// writable, enumerable or configurable. It is not [`Runtime::define_hidden`], which is
    /// writable, because these are not bookkeeping but values a program is meant to read and
    /// not meant to change.
    fn define_frozen(&self, object: GcRef, name: &str, value: Value) {
        self.define(object, name, value);
        let key = PropertyKey::new(name);
        let slot = self
            .heap
            .shape_of(object)
            .and_then(|shape| self.shapes.borrow().lookup(shape, &key));
        if let Some(slot) = slot {
            self.heap.set_attributes(
                object,
                slot.index(),
                crisol_value::Attributes {
                    writable: false,
                    enumerable: false,
                    configurable: false,
                    accessor: false,
                },
            );
        }
    }

    /// Defines a property that enumeration does not see and `delete` cannot remove.
    ///
    /// Used where the specification has an internal slot and this engine has nowhere to put
    /// one: internal slot zero already means "callable" ([`is_callable`]), so an object cannot
    /// borrow it without becoming a function. **The property is still readable by name**,
    /// which a real internal slot would not be.
    fn define_hidden(&self, object: GcRef, name: &str, value: Value) {
        self.define(object, name, value);
        let key = PropertyKey::new(name);
        let slot = self
            .heap
            .shape_of(object)
            .and_then(|shape| self.shapes.borrow().lookup(shape, &key));
        if let Some(slot) = slot {
            self.heap.set_attributes(
                object,
                slot.index(),
                crisol_value::Attributes {
                    writable: true,
                    enumerable: false,
                    configurable: false,
                    accessor: false,
                },
            );
        }
    }

    /// Builds the object at the end of every prototype chain.
    /// Links `object` to the end of every prototype chain.
    ///
    /// Called for each of the other prototypes, so `[].hasOwnProperty` resolves the same way
    /// `({}).hasOwnProperty` does — through one object rather than a copy per prototype.
    fn inherit_from_object(&self, object: GcRef) {
        if let Some(base) = OBJECT_PROTOTYPE.with(std::cell::Cell::get)
            && base != object
        {
            self.heap.set_prototype(object, Some(base));
        }
    }

    /// Allocates the object at the end of every prototype chain, with nothing on it yet.
    ///
    /// **Split from its methods, and the split is the point.** The object has to exist before
    /// anything can link to it, and its methods are functions, which have to be made *after*
    /// `Function.prototype` exists or they will not have `call`. Building it in one step left
    /// `Object.prototype.toString.call` out of reach while `Object.prototype.toString` was
    /// perfectly fine — a gap that shows up only one property further along.
    fn allocate_object_prototype(&self) {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let prototype = scope.alloc(shape, 0);
        OBJECT_PROTOTYPE.with(|cell| cell.set(Some(prototype.handle())));
    }

    /// Puts the methods on it, once functions can be made properly.
    fn build_object_prototype(&self) {
        let Some(prototype) = OBJECT_PROTOTYPE.with(std::cell::Cell::get) else {
            return;
        };
        let base = NATIVES.len()
            + GLOBAL_NATIVES.len()
            + NAMESPACE_NATIVES.len()
            + ANONYMOUS_NATIVES.len()
            + FUNCTION_NATIVES.len()
            + STRING_NATIVES.len()
            + REGEXP_NATIVES.len()
            + DATE_NATIVES.len();
        for (index, (name, _)) in OBJECT_NATIVES.iter().enumerate() {
            let method = self.native_function(base + index);
            self.define_method(prototype, "Object.prototype", name, method.to_value());
        }
    }

    /// Builds the object every function inherits from.
    ///
    /// The object is rooted **before** its own methods are made, because those are functions
    /// and will link to it.
    fn build_function_prototype(&self) {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let prototype = scope.alloc(shape, 0);
        FUNCTION_PROTOTYPE.with(|cell| cell.set(Some(prototype.handle())));
        self.inherit_from_object(prototype.handle());

        let base = NATIVES.len()
            + GLOBAL_NATIVES.len()
            + NAMESPACE_NATIVES.len()
            + ANONYMOUS_NATIVES.len();
        for (index, (name, _)) in FUNCTION_NATIVES.iter().enumerate() {
            let function = self.native_function(base + index);
            self.define_method(
                prototype.handle(),
                "Function.prototype",
                name,
                function.to_value(),
            );
        }
    }

    /// Builds the object every string inherits from.
    fn build_string_prototype(&self) {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let prototype = scope.alloc(shape, 0);
        STRING_PROTOTYPE.with(|cell| cell.set(Some(prototype.handle())));
        self.inherit_from_object(prototype.handle());

        let base = NATIVES.len()
            + GLOBAL_NATIVES.len()
            + NAMESPACE_NATIVES.len()
            + ANONYMOUS_NATIVES.len()
            + FUNCTION_NATIVES.len();
        for (index, (name, _)) in STRING_NATIVES.iter().enumerate() {
            let method = self.native_function(base + index);
            self.define_method(
                prototype.handle(),
                "String.prototype",
                name,
                method.to_value(),
            );
        }
    }

    /// Builds the object every regular expression inherits from.
    fn build_regexp_prototype(&self) {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let prototype = scope.alloc(shape, 0);
        REGEXP_PROTOTYPE.with(|cell| cell.set(Some(prototype.handle())));
        self.inherit_from_object(prototype.handle());

        let base = NATIVES.len()
            + GLOBAL_NATIVES.len()
            + NAMESPACE_NATIVES.len()
            + ANONYMOUS_NATIVES.len()
            + FUNCTION_NATIVES.len()
            + STRING_NATIVES.len();
        for (index, (name, _)) in REGEXP_NATIVES.iter().enumerate() {
            let method = self.native_function(base + index);
            self.define_method(
                prototype.handle(),
                "RegExp.prototype",
                name,
                method.to_value(),
            );
        }
    }

    /// Builds the object every date inherits from.
    fn build_date_prototype(&self) {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let prototype = scope.alloc(shape, 0);
        DATE_PROTOTYPE.with(|cell| cell.set(Some(prototype.handle())));
        self.inherit_from_object(prototype.handle());

        let base = NATIVES.len()
            + GLOBAL_NATIVES.len()
            + NAMESPACE_NATIVES.len()
            + ANONYMOUS_NATIVES.len()
            + FUNCTION_NATIVES.len()
            + STRING_NATIVES.len()
            + REGEXP_NATIVES.len();
        for (index, (name, _)) in DATE_NATIVES.iter().enumerate() {
            let method = self.native_function(base + index);
            self.define_method(
                prototype.handle(),
                "Date.prototype",
                name,
                method.to_value(),
            );
        }
    }

    /// Builds the prototypes `Map` and `Set` instances inherit from.
    fn build_collection_prototypes(&self) {
        let base = NATIVES.len()
            + GLOBAL_NATIVES.len()
            + NAMESPACE_NATIVES.len()
            + ANONYMOUS_NATIVES.len()
            + FUNCTION_NATIVES.len()
            + STRING_NATIVES.len()
            + REGEXP_NATIVES.len()
            + DATE_NATIVES.len()
            + OBJECT_NATIVES.len();
        for (cell, owner, natives, offset) in [
            (&PROMISE_PROTOTYPE, "Promise.prototype", PROMISE_NATIVES, 0),
            (
                &MAP_PROTOTYPE,
                "Map.prototype",
                MAP_NATIVES,
                PROMISE_NATIVES.len(),
            ),
            (
                &SET_PROTOTYPE,
                "Set.prototype",
                SET_NATIVES,
                PROMISE_NATIVES.len() + MAP_NATIVES.len(),
            ),
            (
                &SYMBOL_PROTOTYPE,
                "Symbol.prototype",
                SYMBOL_NATIVES,
                PROMISE_NATIVES.len() + MAP_NATIVES.len() + SET_NATIVES.len(),
            ),
            (
                &ARRAY_ITERATOR_PROTOTYPE,
                "Array Iterator",
                ARRAY_ITERATOR_NATIVES,
                PROMISE_NATIVES.len()
                    + MAP_NATIVES.len()
                    + SET_NATIVES.len()
                    + SYMBOL_NATIVES.len(),
            ),
            (
                &NUMBER_PROTOTYPE,
                "Number.prototype",
                NUMBER_NATIVES,
                PROMISE_NATIVES.len()
                    + MAP_NATIVES.len()
                    + SET_NATIVES.len()
                    + SYMBOL_NATIVES.len()
                    + ARRAY_ITERATOR_NATIVES.len(),
            ),
            (
                &BOOLEAN_PROTOTYPE,
                "Boolean.prototype",
                BOOLEAN_NATIVES,
                PROMISE_NATIVES.len()
                    + MAP_NATIVES.len()
                    + SET_NATIVES.len()
                    + SYMBOL_NATIVES.len()
                    + ARRAY_ITERATOR_NATIVES.len()
                    + NUMBER_NATIVES.len(),
            ),
        ] {
            let shape = self.shapes.borrow().root();
            let scope = self.heap.scope();
            let prototype = scope.alloc(shape, 0);
            cell.with(|slot| slot.set(Some(prototype.handle())));
            self.inherit_from_object(prototype.handle());
            for (index, (name, _)) in natives.iter().enumerate() {
                let method = self.native_function(base + offset + index);
                self.define_method(prototype.handle(), owner, name, method.to_value());
            }
        }
    }

    /// Builds the object every array inherits its methods from.
    ///
    /// Rooted through `ARRAY_PROTOTYPE` **before** the methods are installed, because
    /// installing them allocates — and under stress each allocation collects, which would
    /// reclaim a prototype nothing else refers to yet.
    fn build_array_prototype(&self) {
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        let prototype = scope.alloc(shape, 0);
        ARRAY_PROTOTYPE.with(|cell| cell.set(Some(prototype.handle())));
        self.inherit_from_object(prototype.handle());

        for (index, (name, _)) in NATIVES.iter().enumerate() {
            let method = self.native_function(index);
            self.define_method(
                prototype.handle(),
                "Array.prototype",
                name,
                method.to_value(),
            );
        }
    }

    /// Installs an accessor property whose getter is `getter` and whose setter is absent, with the
    /// attributes a built-in getter carries: writable makes no sense for one, and it is not
    /// enumerable but is configurable. The stored value is the `[getter, setter]` pair the read
    /// path decodes (see `crisol_property_load`).
    fn install_accessor(&self, object: GcRef, name: &str, getter: GcRef) {
        self.install_accessor_keyed(object, &PropertyKey::new(name), getter);
    }

    /// [`Runtime::install_accessor`] for a key that may be a symbol.
    fn install_accessor_keyed(&self, object: GcRef, key: &PropertyKey, getter: GcRef) {
        // **Built through `self`, not the free `array_of_values`.** This runs inside
        // `Runtime::new`, where `with_runtime` would re-enter the thread-local that is still
        // initialising — the crash every builder avoids by touching `self.heap` directly. The
        // getter is rooted across the pair's allocation, since `native_function` hands back an
        // unrooted handle and `alloc` may collect under stress.
        let pair = {
            let scope = self.heap.scope();
            let _held = scope.root(getter);
            let shape = self.shapes.borrow().root();
            let pair = scope.alloc(shape, 0);
            self.heap.make_array(pair.handle(), 2);
            self.heap.set_element(pair.handle(), 0, getter.to_value());
            self.heap.set_element(pair.handle(), 1, Value::UNDEFINED);
            pair.handle()
        };
        self.define_keyed(object, key, pair.to_value());
        let slot = self
            .heap
            .shape_of(object)
            .and_then(|shape| self.shapes.borrow().lookup(shape, key));
        if let Some(slot) = slot {
            self.heap.set_attributes(
                object,
                slot.index(),
                crisol_value::Attributes {
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    accessor: true,
                },
            );
        }
    }

    /// Points a set of `%TypedArray%.prototype` methods at the identical `Array.prototype`
    /// functions. Each of these reads and writes through `length` and the integer indices, which
    /// a typed array answers from its buffer — so the array method works over one unchanged. The
    /// ones that would build a plain array (`map`, `filter`, `slice`, `toReversed`, …) are absent:
    /// a typed array's must build a typed array, so it has its own.
    fn alias_array_methods(&self, target: GcRef) {
        const REUSED: &[&str] = &[
            "at",
            "join",
            "toString",
            "toLocaleString",
            "indexOf",
            "lastIndexOf",
            "includes",
            "forEach",
            "reduce",
            "reduceRight",
            "every",
            "some",
            "find",
            "findIndex",
            "findLast",
            "findLastIndex",
            "fill",
            "reverse",
            "copyWithin",
            "keys",
            "values",
            "entries",
        ];
        let Some(source) = ARRAY_PROTOTYPE.with(std::cell::Cell::get) else {
            return;
        };
        for name in REUSED {
            let key = PropertyKey::new(name);
            let method = self
                .heap
                .shape_of(source)
                .and_then(|shape| self.shapes.borrow().lookup(shape, &key))
                .and_then(|slot| self.heap.get(source, slot.index()));
            if let Some(method) = method {
                self.define_method(target, "TypedArray.prototype", name, method);
            }
        }
    }

    /// Builds `ArrayBuffer.prototype`, `%TypedArray%.prototype` and the nine per-kind prototypes.
    ///
    /// Runs before `build_globals`, which reads the cells this fills to point each constructor's
    /// `prototype` at the shared object rather than the fresh one every global otherwise gets. The
    /// symbol-keyed members (`Symbol.iterator`, `Symbol.toStringTag`) wait for
    /// `build_symbol_keyed_methods`, because the well-known symbols do not exist yet.
    fn build_typed_array_prototypes(&self) {
        let base = typed_natives_base();

        // `ArrayBuffer.prototype`.
        {
            let shape = self.shapes.borrow().root();
            let scope = self.heap.scope();
            let prototype = scope.alloc(shape, 0);
            ARRAY_BUFFER_PROTOTYPE.with(|cell| cell.set(Some(prototype.handle())));
            self.inherit_from_object(prototype.handle());
            let getter = self.native_function(base + AB_BYTE_LENGTH);
            self.install_accessor(prototype.handle(), "byteLength", getter);
            let slice = self.native_function(base + AB_SLICE);
            self.define_method(
                prototype.handle(),
                "ArrayBuffer.prototype",
                "slice",
                slice.to_value(),
            );
        }

        // `%TypedArray%.prototype`, shared by every kind.
        let shared = {
            let shape = self.shapes.borrow().root();
            let scope = self.heap.scope();
            let prototype = scope.alloc(shape, 0);
            TYPED_ARRAY_PROTOTYPE.with(|cell| cell.set(Some(prototype.handle())));
            self.inherit_from_object(prototype.handle());
            for (name, offset) in [
                ("length", TA_LENGTH_GET),
                ("byteLength", TA_BYTE_LENGTH_GET),
                ("byteOffset", TA_BYTE_OFFSET_GET),
                ("buffer", TA_BUFFER_GET),
            ] {
                let getter = self.native_function(base + offset);
                self.install_accessor(prototype.handle(), name, getter);
            }
            for (name, offset) in [
                ("set", TA_SET_METHOD),
                ("subarray", TA_SUBARRAY_METHOD),
                ("slice", TA_SLICE_METHOD),
            ] {
                let method = self.native_function(base + offset);
                self.define_method(
                    prototype.handle(),
                    "TypedArray.prototype",
                    name,
                    method.to_value(),
                );
            }
            self.alias_array_methods(prototype.handle());
            prototype.handle()
        };

        // The nine per-kind prototypes, each inheriting the shared one and carrying its own
        // `BYTES_PER_ELEMENT`.
        for kind in ELEMENT_KINDS {
            let shape = self.shapes.borrow().root();
            let scope = self.heap.scope();
            let prototype = scope.alloc(shape, 0);
            TYPED_ARRAY_PROTOTYPES.with(|protos| protos[kind.tag()].set(Some(prototype.handle())));
            self.heap.set_prototype(prototype.handle(), Some(shared));
            self.define_frozen(
                prototype.handle(),
                "BYTES_PER_ELEMENT",
                Value::number(index_number(kind.bytes())),
            );
        }
    }
}

thread_local! {
    static RUNTIME: Runtime = Runtime::new();
}

/// Runs `f` against this thread's runtime.
pub fn with_runtime<R>(f: impl FnOnce(&Runtime) -> R) -> R {
    RUNTIME.with(f)
}

/// The handle a boxed value names, if it names one.
fn handle_of(bits: u64) -> Option<GcRef> {
    Value::from_bits(bits).as_address().map(GcRef::from_address)
}

/// The object every unresolved name is looked up in, as a value.
///
/// **What `this` is at the top level of a script.** The entry point passed `undefined`, which
/// is what `this` is inside a strict function and never what it is at the top of a sloppy
/// script — so `this.x = 1` did nothing and `this === globalThis` was false, and the tests
/// that use `this` to reach a global all read a property of `undefined` instead.
///
/// Calling this builds the runtime if nothing has yet, which is why the entry point can call
/// it before the program starts: the stack maps are already registered by then, so a
/// collection during construction has everything it needs.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_global_object() -> u64 {
    with_runtime(|_| {
        GLOBALS.with(std::cell::Cell::get).map_or_else(
            || Value::UNDEFINED.to_bits(),
            |globals| globals.to_value().to_bits(),
        )
    })
}

/// Allocates `{}` — an object at the root shape, with no properties.
///
/// Properties arrive through [`crisol_property_store`], which moves the object to the shape
/// that includes each one. That is why this takes no shape argument: an object literal *is*
/// empty until its first property is stored, and the lowering says so (D-92).
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_create_object() -> u64 {
    with_runtime(|runtime| {
        let shape = runtime.shapes.borrow().root();
        // Rooted only for the allocation itself. What keeps it alive afterwards is the caller's
        // frame: the value is about to land in a slot the stack map describes, and the next
        // collection cannot happen before then, because only an allocation triggers one.
        let scope = runtime.heap.scope();
        let object = scope.alloc(shape, 0);
        // Every object ends its chain at `Object.prototype`, which is what makes
        // `({}).hasOwnProperty` resolve at all.
        runtime.inherit_from_object(object.handle());
        object.to_value().to_bits()
    })
}

/// `object[key] = value`.
///
/// Storing a property the object does not have moves it to the shape that includes it and
/// grows it by a slot; storing one it already has is an assignment and leaves the shape alone.
/// `Shapes::add` draws that distinction, which is what stops a loop assigning the same property
/// from growing the shape tree once per iteration.
///
/// A store through a value that is not an object is ignored rather than faulted. That is not
/// the specification — `null.x = 1` is a `TypeError` — but throwing needs the unwinding path
/// M13 does not have yet, and ignoring is the one behaviour that cannot corrupt the heap.
///
/// # Safety
///
/// `key` must point to `length` readable bytes of UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crisol_property_store(
    object: u64,
    key: *const u8,
    length: u64,
    value: u64,
) -> u64 {
    if let Some((target, handler)) = proxy_parts(object) {
        // SAFETY: the caller promises `length` readable UTF-8 bytes at `key`.
        let Some(name) = (unsafe { key_text(key, length) }) else {
            return Value::UNDEFINED.to_bits();
        };
        return proxy_store(object, target, handler, &PropertyKey::new(&name), value);
    }
    let Some(handle) = handle_of(object) else {
        return nullish_access(object);
    };
    // SAFETY: the caller promises `key` names `length` readable bytes of UTF-8.
    let Some(name) = (unsafe { key_text(key, length) }) else {
        return Value::UNDEFINED.to_bits();
    };
    let key = PropertyKey::new(&name);

    // **A typed array's integer index writes its buffer, not a slot.** Digit-leading only, as
    // the load is. The coercion of `value` runs even for an out-of-range index — it can observe a
    // `valueOf` — so a canonical index defers to `typed_array_element_store`, which coerces first
    // and only then checks the range, and a digit-leading non-index coerces and discards.
    if name.as_bytes().first().is_some_and(u8::is_ascii_digit) && is_typed_array(object) {
        return match canonical_index(&name) {
            Some(index) => typed_array_element_store(object, index, value)
                .unwrap_or(Value::UNDEFINED.to_bits()),
            None => match coerce_number(value) {
                Ok(_) => Value::UNDEFINED.to_bits(),
                Err(thrown) => thrown,
            },
        };
    }

    // **Assigning to an array's `length` resizes it.** `length` is not stored anywhere — it
    // *is* the element count — so writing it has to change the elements rather than add a
    // property. Without this `a.length = 0` silently did nothing, and test262's own
    // `buildString` helper, which empties a scratch array that way each chunk, instead
    // re-sent everything it had accumulated: quadratic growth, and the process killed on
    // memory rather than any error a test could report.
    if name == "length"
        && let Some(count) = with_runtime(|runtime| runtime.heap.element_count(handle))
    {
        // **A length made non-writable ignores an assignment**, silently outside strict mode,
        // exactly as a non-writable property does. Checked here because the length has no slot
        // whose attributes the ordinary path could consult.
        if own_flag(object, FIXED_LENGTH) {
            return Value::UNDEFINED.to_bits();
        }
        let wanted = to_number(value);
        // **Above 2^32-1 is a `RangeError`**, which is the specification's rule and also the
        // only thing standing between `[].length = 4294967297` and an attempt to materialise
        // four billion elements. That attempt was a crash, not an error a test could report.
        if !wanted.is_finite()
            || wanted < 0.0
            || wanted > f64::from(u32::MAX)
            || wanted.fract() != 0.0
        {
            return raise("invalid array length", "RangeError");
        }
        {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "checked finite and non-negative just above"
            )]
            let wanted = wanted as usize;
            with_runtime(|runtime| {
                if wanted < count {
                    runtime.heap.truncate_elements(handle, wanted);
                } else if wanted > count {
                    // Growing fills with `undefined`, which is not what the specification
                    // says — those should be holes (D-64) — and is the same approximation
                    // array literals already make.
                    runtime
                        .heap
                        .set_element(handle, wanted - 1, Value::UNDEFINED);
                }
            });
        }
        return Value::UNDEFINED.to_bits();
    }

    // **An index names an element, even spelled as text** — see `crisol_property_load`. Before
    // the extensibility check below, because that one asks about *slots*: an element has none,
    // so it read as absent and a non-extensible array refused a write to an element it
    // already had.
    if let Some(index) = canonical_index(&name)
        && store_element(object, index, value)
    {
        return Value::UNDEFINED.to_bits();
    }

    // **A non-extensible object refuses a property it does not already have.** Silently,
    // outside strict mode — the same rule a non-writable property follows, and the reason
    // `Object.freeze` is worth anything at all.
    if own_property(object, &name).is_none() && !is_extensible(object) {
        return Value::UNDEFINED.to_bits();
    }

    // **A write to an accessor calls its setter**, and that is JavaScript — so the pair is
    // fetched and the call made outside any runtime borrow, for the same reason a getter is
    // (see `crisol_property_load`). Looked for up the chain, because a setter inherited from a
    // prototype still receives a write to the instance.
    /// What the chain walk decided a write should do.
    enum Write {
        /// Call this accessor pair's setter.
        Accessor(u64),
        /// Re-parent the receiver: the key was `__proto__` and nothing shadowed it.
        Prototype,
        /// Nothing found; store it ordinarily.
        Store,
    }

    let action = with_runtime(|runtime| {
        let mut current = Some(handle);
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let Some(object) = current else { break };
            let Some(shape) = runtime.heap.shape_of(object) else {
                break;
            };
            let found = runtime.shapes.borrow().lookup(shape, &key);
            if let Some(slot) = found
                && !runtime.heap.is_deleted(object, slot.index())
            {
                return runtime
                    .heap
                    .attributes_of(object, slot.index())
                    .accessor
                    .then(|| runtime.heap.get(object, slot.index()))
                    .flatten()
                    .map_or(Write::Store, |pair| Write::Accessor(pair.to_bits()));
            }
            // The setter half of the `__proto__` accessor — found the same way its getter is,
            // and for the same reasons. See `crisol_property_load`.
            if name == PROTO_ACCESSOR && Some(object) == OBJECT_PROTOTYPE.with(std::cell::Cell::get)
            {
                return Write::Prototype;
            }
            current = runtime.heap.prototype_of(object);
        }
        Write::Store
    });
    match action {
        Write::Accessor(pair) => {
            if let Some((functions, _)) = elements_of(pair) {
                let setter = element_at(functions, 1);
                if is_callable(setter) {
                    return call_value(setter, object, &[value]);
                }
            }
            // **A getter with no setter swallows the write**, silently outside strict mode —
            // which is what makes a read-only computed property read-only.
            return Value::UNDEFINED.to_bits();
        }
        Write::Prototype => {
            return match set_prototype_of(handle, value) {
                Ok(_) => Value::UNDEFINED.to_bits(),
                Err(message) => raise(message, "TypeError"),
            };
        }
        Write::Store => {}
    }

    with_runtime(|runtime| {
        let Some(current) = runtime.heap.shape_of(handle) else {
            return;
        };
        let (shape, slot, width) = {
            let mut shapes = runtime.shapes.borrow_mut();
            let shape = shapes.add(current, &key);
            let Some(slot) = shapes.lookup(shape, &key) else {
                return;
            };
            (shape, slot, shapes.len(shape) as usize)
        };
        if shape != current {
            runtime.heap.transition(handle, shape, width);
        } else if runtime.heap.is_deleted(handle, slot.index()) {
            // The shape still names the slot, so the tombstone is the only thing that made it
            // absent — clearing it is what brings the property back.
            //
            // **As a new property, not as the one that was deleted.** The slot still carries
            // whatever attributes that one had, and an assignment creates a writable,
            // enumerable, configurable property whatever stood there before: a property made
            // read-only, deleted, and then assigned to came back read-only, so the second
            // write to it was silently dropped.
            runtime.heap.set_deleted(handle, slot.index(), false);
            runtime
                .heap
                .set_attributes(handle, slot.index(), crisol_value::Attributes::DATA);
        } else if !runtime.heap.attributes_of(handle, slot.index()).writable {
            // **A write to a non-writable property is silently ignored**, not an error —
            // outside strict mode, which is the only mode there is here. Only an existing
            // property can be non-writable, which is why this is on the no-transition path.
            return;
        }
        runtime
            .heap
            .set(handle, slot.index(), Value::from_bits(value));
    });
    Value::UNDEFINED.to_bits()
}

/// `object[key]`, or `undefined` if it has no such property.
///
/// Missing is `undefined` rather than an error, which is the specification's behaviour and not
/// a shortcut — but note it makes a *misspelled* property indistinguishable from an absent one,
/// so a lookup that silently yields `undefined` is not evidence the object is wrong.
///
/// # Safety
///
/// `key` must point to `length` readable bytes of UTF-8.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn crisol_property_load(object: u64, key: *const u8, length: u64) -> u64 {
    // **One integer compare for every program that uses no proxies** — see `PROXY_MARKER`.
    // A trap is JavaScript, so it runs from here, before any borrow is taken.
    if let Some((target, handler)) = proxy_parts(object) {
        // SAFETY: the caller promises `length` readable UTF-8 bytes at `key`.
        let Some(name) = (unsafe { key_text(key, length) }) else {
            return Value::UNDEFINED.to_bits();
        };
        return proxy_load(object, target, handler, &PropertyKey::new(&name));
    }
    /// What the chain walk found: a value to hand back, or an accessor still to be called.
    ///
    /// The distinction has to survive the walk because a getter is JavaScript and will reach
    /// back into the runtime — calling it while the walk still holds the borrow would be
    /// re-entering what it is inside.
    enum Found {
        Value(u64),
        Get(u64),
    }

    let Some(handle) = handle_of(object) else {
        // **A number or a boolean is not a cell**, so there is no object to walk from — but
        // `(255).toString(16)` and `true.toString()` still have to find their prototypes. A
        // string does not need this because a string *is* a cell, which is why this gap only
        // showed when the other two grew methods worth reaching.
        let held = Value::from_bits(object);
        if !matches!(held.kind(), crisol_value::Kind::Boolean) && held.as_number().is_none() {
            return nullish_access(object);
        }
        // SAFETY: the caller promises `length` readable UTF-8 bytes at `key`.
        let Some(name) = (unsafe { key_text(key, length) }) else {
            return Value::UNDEFINED.to_bits();
        };
        // **The prototype is read *inside* `with_runtime`, and that ordering is the whole
        // thing.** `with_runtime` is what constructs the runtime on first use, and the
        // prototypes are filled in during that construction. Reading the cell before entering
        // it saw `None` in any program whose first act was a property load on a primitive —
        // `let n = 255; n.toString` allocates nothing beforehand, so nothing had built the
        // runtime yet. It then fell through to `nullish_access`, which answers `undefined` for
        // a number rather than raising, so the failure arrived as a missing method rather than
        // as anything pointing here.
        return with_runtime(|runtime| {
            let prototype = if held.kind() == crisol_value::Kind::Boolean {
                BOOLEAN_PROTOTYPE.with(std::cell::Cell::get)
            } else {
                NUMBER_PROTOTYPE.with(std::cell::Cell::get)
            };
            let Some(prototype) = prototype else {
                return Value::UNDEFINED.to_bits();
            };
            // The receiver stays the primitive, so a method reached this way still sees the
            // number or boolean it was called on rather than the prototype.
            let found = runtime.heap.shape_of(prototype).and_then(|shape| {
                let key = PropertyKey::new(&name);
                runtime.shapes.borrow().lookup(shape, &key)
            });
            found
                .and_then(|slot| runtime.heap.get(prototype, slot.index()))
                .map_or(Value::UNDEFINED.to_bits(), |value| value.to_bits())
        });
    };
    // SAFETY: as above.
    let Some(name) = (unsafe { key_text(key, length) }) else {
        return Value::UNDEFINED.to_bits();
    };
    let key = PropertyKey::new(&name);

    // **A typed array's integer index reads its buffer, not a slot.** Only a digit-leading key
    // can name an element — `length`, `buffer` and the methods are found by the ordinary walk on
    // the prototype — so the brand check stays off every named load and every ordinary object.
    if name.as_bytes().first().is_some_and(u8::is_ascii_digit) && is_typed_array(object) {
        return match canonical_index(&name) {
            Some(index) => typed_array_element_load(object, index),
            // A digit-leading string that is not a canonical index ("1.5", "01") still names no
            // element on a typed array: `undefined`, and never the prototype.
            None => Value::UNDEFINED.to_bits(),
        };
    }

    let found = with_runtime(|runtime| {
        // `length` on an array is not stored anywhere — it *is* the element count, and has to
        // answer correctly after `a[9] = 1` grew the array without any property being written.
        if name == "length"
            && let Some(units) = runtime
                .heap
                .with_text(handle, |text| text.encode_utf16().count())
        {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a string this long cannot be allocated"
            )]
            // **UTF-16 code units**, which is what JavaScript counts — so an emoji is two and
            // `é` is one. This counted bytes, which reads correctly for ASCII and wrongly for
            // everything else.
            let length = units as f64;
            return Found::Value(Value::number(length).to_bits());
        }
        if name == "length"
            && let Some(count) = runtime.heap.element_count(handle)
        {
            #[expect(
                clippy::cast_precision_loss,
                reason = "an array this long cannot be allocated"
            )]
            let length = count as f64;
            return Found::Value(Value::number(length).to_bits());
        }
        // **An index names an element, even spelled as text.** `a["0"]` and `a[0]` are the
        // same property and only the second reached the elements, so every path that reads by
        // name — `Object.values`, `Object.entries`, `Object.assign` — saw `undefined` for
        // every element an array has. The computed path handles a *number* key; nothing
        // handled the string one, and the two spellings have to agree.
        if let Some(index) = canonical_index(&name)
            && let Some(value) = runtime.heap.element(handle, index)
        {
            return Found::Value(value.to_bits());
        }
        // **A string wrapper is indexed by its characters.** `new String("abc")[0]` is `"a"`,
        // and the wrapper holds its text whole rather than one property per character. Reading
        // a character out on demand costs nothing for the wrappers nobody indexes, where
        // defining them all at construction would charge every wrapper for a case most never
        // reach. This is also what lets the array methods walk one (D-157).
        if let Ok(index) = name.parse::<usize>() {
            let held = runtime
                .heap
                .shape_of(handle)
                .and_then(|shape| {
                    let key = PropertyKey::new(STRING_PRIMITIVE);
                    runtime.shapes.borrow().lookup(shape, &key)
                })
                .and_then(|slot| runtime.heap.get(handle, slot.index()))
                .and_then(|value| value.as_address())
                .map(GcRef::from_address)
                .and_then(|cell| runtime.heap.with_text(cell, ToOwned::to_owned))
                // A *primitive* string is the cell itself, so `"ab"["0"]` reads here rather
                // than through a wrapper it never made.
                .or_else(|| runtime.heap.with_text(handle, ToOwned::to_owned));
            if let Some(text) = held {
                let units: Vec<u16> = text.encode_utf16().collect();
                return Found::Value(units.get(index).map_or_else(
                    || Value::UNDEFINED.to_bits(),
                    |unit| new_string(&String::from_utf16_lossy(&[*unit])),
                ));
            }
        }

        // Walks the prototype chain. A class's methods live on one shared prototype object,
        // not on each instance, so a lookup that stopped at the receiver would find every
        // field and no method at all.
        //
        // Bounded rather than "until the chain ends": `a.__proto__ = b; b.__proto__ = a` is a
        // cycle the specification forbids, and nothing has rejected it yet. An unbounded walk
        // would hang inside a property read, which is a far worse failure than a miss.
        let mut current = Some(handle);
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let Some(object) = current else { break };
            let Some(shape) = runtime.heap.shape_of(object) else {
                break;
            };
            let found = runtime.shapes.borrow().lookup(shape, &key);
            if let Some(slot) = found
                && !runtime.heap.is_deleted(object, slot.index())
                && let Some(value) = runtime.heap.get(object, slot.index())
            {
                // **An accessor is read by calling its getter**, with the original receiver —
                // not the object the property was found on, so a getter inherited from a
                // prototype sees the instance it was reached through.
                if runtime.heap.attributes_of(object, slot.index()).accessor {
                    return Found::Get(value.to_bits());
                }
                return Found::Value(value.to_bits());
            }
            // **`__proto__` is an accessor on `Object.prototype`**, not a property anything
            // stores — so it is answered here, at the point the walk arrives at the object
            // that owns it, with the receiver the walk started from. Answering it at the top
            // of the lookup instead would be shorter and wrong twice over: an own `__proto__`
            // could no longer shadow it, and `Object.create(null)` would grow one, when the
            // whole point of a null prototype is that the chain never reaches here.
            if name == PROTO_ACCESSOR && Some(object) == OBJECT_PROTOTYPE.with(std::cell::Cell::get)
            {
                return Found::Value(
                    runtime
                        .heap
                        .prototype_of(handle)
                        .map_or(Value::NULL, GcRef::to_value)
                        .to_bits(),
                );
            }
            current = runtime.heap.prototype_of(object);
        }
        Found::Value(Value::UNDEFINED.to_bits())
    });

    match found {
        Found::Value(value) => value,
        // Called outside the runtime borrow: a getter is JavaScript and will reach back in.
        Found::Get(pair) => match elements_of(pair) {
            Some((functions, _)) => {
                let getter = element_at(functions, 0);
                if is_callable(getter) {
                    call_value(getter, object, &[])
                } else {
                    // A setter with no getter reads as `undefined`, which is the whole of what
                    // a write-only property does.
                    Value::UNDEFINED.to_bits()
                }
            }
            None => Value::UNDEFINED.to_bits(),
        },
    }
}

/// How far a property lookup walks before giving up.
///
/// Deep chains are rare and a cycle is illegal, so this is a backstop rather than a budget —
/// reaching it means the heap holds a chain the specification says cannot exist.
const PROTOTYPE_CHAIN_LIMIT: usize = 1000;

/// The `this` for `new callee(...)`, or the signal that `callee` cannot be constructed.
///
/// **This is where `new` asks the question**, because it is the only step that happens before
/// the body runs. Asking later is asking after the side effects.
///
/// It answers the exception signal rather than taking a branch of its own: the call site then
/// resolves the body through [`crisol_construct_code`], which hands back one that runs
/// nothing, and [`crisol_construct_result`] passes the signal out. So a refusal costs the same
/// straight line as an ordinary `new` — the same arrangement, and for the same reason, as
/// [`crisol_not_a_function`].
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_construct_this(callee: u64) -> u64 {
    if !is_constructor(callee) {
        return raise("is not a constructor", "TypeError");
    }
    allocate_receiver(callee)
}

/// Allocates the `this` for `new callee(...)`, inheriting from `callee.prototype`.
///
/// This is `OrdinaryCreateFromConstructor`: the prototype link is established here rather than
/// by a separate step that could be omitted, which is why `Op::Construct` is one operation
/// rather than the sequence it stands for.
///
/// A callee with no `prototype` property still yields an object, just one with no prototype.
/// That is wrong for a real constructor and right for the only way to reach it here — a
/// `new` on something that is not a class — and it beats returning nothing at all.
fn allocate_receiver(callee: u64) -> u64 {
    let prototype = {
        let key = PropertyKey::new("prototype");
        handle_of(callee).and_then(|handle| {
            with_runtime(|runtime| {
                let shape = runtime.heap.shape_of(handle)?;
                let slot = runtime.shapes.borrow().lookup(shape, &key)?;
                runtime.heap.get(handle, slot.index())
            })
        })
    };

    with_runtime(|runtime| {
        let shape = runtime.shapes.borrow().root();
        let scope = runtime.heap.scope();
        let object = scope.alloc(shape, 0);
        if let Some(prototype) = prototype.and_then(|value| value.as_address()) {
            runtime
                .heap
                .set_prototype(object.handle(), Some(GcRef::from_address(prototype)));
        }
        object.to_value().to_bits()
    })
}

/// Which value `new` evaluates to.
///
/// **A constructor returning an object replaces the newly created `this`; one returning a
/// primitive does not.** That rule is why `Op::Construct` exists as one operation — spelling
/// `new` out as allocate-then-call would put it at every call site, and the first lowering to
/// forget it would produce a constructor whose explicit `return` is silently ignored.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_construct_result(this_value: u64, returned: u64) -> u64 {
    // **A throw is not a return value.** The rule below — a constructor answering a primitive
    // yields the instance instead — was swallowing the exception signal, which is not an
    // object either. So `new RegExp("(")` raised a `SyntaxError` inside and handed back a
    // perfectly good empty object, and the `try` around it never saw anything. The literal
    // form `/(/ ` raised correctly, which is what made the two disagree.
    if Value::from_bits(returned).is_exception() {
        return returned;
    }
    if Value::from_bits(returned).kind() == crisol_value::Kind::Object {
        returned
    } else {
        this_value
    }
}

/// A body for a `new` that was refused.
///
/// Runs nothing and hands the signal back, so the call site's straight line — allocate,
/// resolve, call, decide — needs no branch for the case where the first step said no. The
/// value it throws was recorded by [`crisol_construct_this`]; this must not raise one of its
/// own, which would replace "is not a constructor" with something less true.
extern "C" fn refused_construct(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    Value::EXCEPTION.to_bits()
}

/// The body `new callee(...)` should run, given the receiver that was made for it.
///
/// Never null, exactly as [`crisol_closure_code`] is never null: when the receiver is the
/// exception signal the answer is a body that runs nothing, so the call site can jump through
/// whatever this returns without checking.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_construct_code(callee: u64, this_value: u64) -> *const u8 {
    if Value::from_bits(this_value).is_exception() {
        let refused: Native = refused_construct;
        return refused as *const u8;
    }
    crisol_closure_code(callee)
}

/// `Construct(callee, args, new_target)`, for the runtime's own callers.
///
/// `new_target` is what the receiver's prototype comes from and what the body sees as
/// `new.target`; for `new f()` the two are the same function, and `Reflect.construct` is the
/// only way to make them differ — which is why the generated code does not come through here
/// and `Reflect.construct` does.
///
/// # Safety
///
/// `argv` must point to `argc` readable values, and to at least one.
unsafe fn construct_with(callee: u64, new_target: u64, argc: u64, argv: *const u64) -> u64 {
    if !is_constructor(callee) {
        return raise("is not a constructor", "TypeError");
    }
    if !is_constructor(new_target) {
        return raise("a new target must be a constructor", "TypeError");
    }
    with_runtime(|runtime| {
        let scope = runtime.heap.scope();
        // Everything here is held by a Rust local across an allocation and by nothing else:
        // the caller's frame is not scanned for values it has finished with. Rooted one at a
        // time rather than through `with_rooted`, which collects a `Vec` of its own.
        let _callee = handle_of(callee).map(|handle| scope.root(handle));
        let _target = handle_of(new_target).map(|handle| scope.root(handle));
        let this_value = allocate_receiver(new_target);
        let code = crisol_closure_code(callee);
        // SAFETY: `crisol_closure_code` answers either a compiled function or
        // `crisol_not_a_function`, and both have exactly this signature.
        let function: Native = unsafe { std::mem::transmute::<*const u8, Native>(code) };
        let _receiver = handle_of(this_value).map(|handle| scope.root(handle));
        // SAFETY: the caller promises `argc` readable values at `argv`.
        let returned = function(callee, this_value, new_target, argc, argv);
        crisol_construct_result(this_value, returned)
    })
}

/// Reads a key passed as a pointer and a length.
///
/// Returns `None` for a null pointer or bytes that are not UTF-8, so a malformed call yields a
/// missing property rather than reading past the end of whatever was passed.
///
/// # Safety
///
/// `key` must point to `length` readable bytes when it is not null.
unsafe fn key_text(key: *const u8, length: u64) -> Option<String> {
    if key.is_null() {
        return None;
    }
    let length = usize::try_from(length).ok()?;
    // SAFETY: the caller promises `length` readable bytes at `key`.
    let bytes = unsafe { std::slice::from_raw_parts(key, length) };
    std::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
}

/// The value a closure captured at `index`.
///
/// Captures are positional: the `index`-th slot of the closure pairs with the `index`-th entry
/// of `Op::Closure`'s capture list and with the `index`-th entry of the callee's `captures`.
/// `crisol_ir::verify_module` checks that pairing, because it is the one rule that cannot be
/// checked by looking at a single function — and a mismatch would leave a slot holding a
/// plausible value rather than failing.
///
/// A missing capture reads as `undefined` rather than faulting: an out-of-range index means
/// the compiler and the runtime disagree about the closure's width, and returning a value the
/// program can see beats reading past the end of the object.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_closure_capture(closure: u64, index: u64) -> u64 {
    let Some(handle) = handle_of(closure) else {
        return Value::UNDEFINED.to_bits();
    };
    let Ok(index) = u32::try_from(index) else {
        return Value::UNDEFINED.to_bits();
    };
    with_runtime(|runtime| {
        runtime
            .heap
            .internal(handle, index + CLOSURE_CAPTURES_AT)
            .unwrap_or(Value::UNDEFINED)
            .to_bits()
    })
}

/// Where a closure's captures begin, among its engine-private values.
///
/// Internal zero holds which function the closure runs, so captures start at one. The index is
/// stored rather than the code address because a code address does not fit a NaN-boxed value's
/// 48-bit payload on every platform.
///
/// These are `internals`, **not property slots**. A shape numbers properties from zero, so a
/// closure keeping its function index in property slot zero lost it as soon as anything stored
/// a property on the function — and `class C {}` stores `prototype` on its constructor, which
/// made every class constructor silently uncallable.
const CLOSURE_CAPTURES_AT: u32 = 1;

/// Every compiled function's entry point, indexed by `FunctionId`.
///
/// Registered by the program at startup for the same reason the stack map table is: only the
/// linker knows where the code landed, and an `extern` reference here would make this crate
/// fail to link anywhere the symbol does not exist — including its own tests.
static mut FUNCTIONS: &[*const u8] = &[];

/// Hands the runtime the addresses of the program's compiled functions.
///
/// # Safety
///
/// `table` must point to `count` function addresses that outlive the program, indexed by
/// `FunctionId`. Called once, before any compiled code runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crisol_register_functions(table: *const *const u8, count: u64) {
    let count = usize::try_from(count).unwrap_or(0);
    let rows: &'static [*const u8] = if table.is_null() || count == 0 {
        &[]
    } else {
        // SAFETY: the caller promises `count` addresses at `table`, living as long as the
        // program.
        unsafe { std::slice::from_raw_parts(table, count) }
    };
    // SAFETY: written once before any compiled code runs, and read-only afterwards.
    unsafe { FUNCTIONS = rows };
    if std::env::var_os("CRISOL_DEBUG_STACK_MAPS").is_some() {
        eprintln!("crisol: registered {} functions", rows.len());
    }
}

/// Allocates a closure over `function`, with room for `captures` captured values.
///
/// The captures arrive afterwards through [`crisol_closure_set_capture`] rather than here,
/// because a variadic C call would have to agree with the backend about how the arguments were
/// passed — and the two only meet at link time, where a disagreement is silent.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_create_closure(function: u64, captures: u64) -> u64 {
    let captures = usize::try_from(captures).unwrap_or(0);
    with_runtime(|runtime| {
        let shape = runtime.shapes.borrow().root();
        let scope = runtime.heap.scope();
        // No property slots: a function's properties arrive later through the ordinary path,
        // and its engine-private state is kept where they cannot reach it.
        let closure = scope.alloc_with_internals(shape, 0, captures + CLOSURE_CAPTURES_AT as usize);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a function index is far below 2^53"
        )]
        let index = Value::number(function as f64);
        runtime.heap.set_internal(closure.handle(), 0, index);
        // Inherits from `Function.prototype`, which is what makes `f.call` and `f.apply` reach
        // anything at all.
        if let Some(prototype) = FUNCTION_PROTOTYPE.with(std::cell::Cell::get) {
            runtime
                .heap
                .set_prototype(closure.handle(), Some(prototype));
        }

        // **Every function gets a `prototype` object.** `new f()` links an instance to it and
        // `x instanceof f` looks for it, so a function without one makes both silently wrong —
        // `instanceof` answers `false` for an object the constructor just made. The class
        // lowering stores its own over this, which is the same property being assigned.
        //
        // Eager, and that costs an allocation per closure that most never use. Creating it on
        // first read would avoid that, at the price of a property read that mutates the heap.
        let prototype = scope.alloc(shape, 0);
        let key = PropertyKey::new("prototype");
        let (target, slot, width) = {
            let mut shapes = runtime.shapes.borrow_mut();
            let current = runtime
                .heap
                .shape_of(closure.handle())
                .unwrap_or_else(|| shapes.root());
            let target = shapes.add(current, &key);
            match shapes.lookup(target, &key) {
                Some(slot) => (target, slot, shapes.len(target) as usize),
                None => return closure.to_value().to_bits(),
            }
        };
        runtime.heap.transition(closure.handle(), target, width);
        runtime
            .heap
            .set(closure.handle(), slot.index(), prototype.to_value());
        // **A function's `prototype` is not enumerable.** Left as an ordinary property it
        // turned up in `Object.keys(f)` and in `for (k in f)` — for every function a program
        // can see, which is most of the objects it has.
        runtime.heap.set_attributes(
            closure.handle(),
            slot.index(),
            crisol_value::Attributes {
                writable: true,
                enumerable: false,
                configurable: false,
                accessor: false,
            },
        );
        // The link back, which is what `new f().constructor === f` reads.
        runtime.define_linking(prototype.handle(), "constructor", closure.to_value());
        closure.to_value().to_bits()
    })
}

/// Writes one of a closure's captured values.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_closure_set_capture(closure: u64, index: u64, value: u64) {
    let Some(handle) = handle_of(closure) else {
        return;
    };
    let Ok(index) = u32::try_from(index) else {
        return;
    };
    with_runtime(|runtime| {
        runtime
            .heap
            .set_internal(handle, index + CLOSURE_CAPTURES_AT, Value::from_bits(value));
    });
}

/// Calling something that is not a function.
///
/// `let x = 5; x();` is a `TypeError`, and throwing needs the unwinding path M13 does not have
/// yet. Until then this returns `undefined` — but the important part is that it *exists*: it
/// gives [`crisol_closure_code`] a real address to hand back, so a bad callee costs a wasted
/// call instead of a jump through a null pointer.
///
/// It takes the uniform convention's five operands because it stands in for a compiled
/// function and is called exactly like one. When exceptions land, this is where the `TypeError`
/// is raised, and no call site has to change.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_not_a_function(
    _closure: u64,
    _this: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    raise("is not a function", "TypeError")
}

/// The `TypeError` a property access on `null` or `undefined` raises.
///
/// Answering `undefined` instead makes `x.y.z` on a missing `x` fail two lines later carrying a
/// value that looks like a legitimate absence — which is why nearly every test expecting a
/// `TypeError` saw a wrong value rather than the error.
fn nullish_access(object: u64) -> u64 {
    let what = match Value::from_bits(object).kind() {
        crisol_value::Kind::Null => "null",
        crisol_value::Kind::Undefined => "undefined",
        // A number or a boolean has no properties here, but reading one is not an error — the
        // specification wraps it, and until that exists `undefined` is the closer answer.
        _ => return Value::UNDEFINED.to_bits(),
    };
    raise(&format!("cannot read a property of {what}"), "TypeError")
}

/// Roots `values` on the shadow stack for the duration of `body`.
///
/// **A built-in has to do this and a compiled function does not**, and the asymmetry is easy to
/// miss. Arguments arrive in `argv`, a buffer in the *caller's* frame that no stack map
/// describes. A compiled callee is safe anyway: its prologue copies them into stack-mapped
/// variables before anything can allocate. A built-in never runs that prologue — it reads
/// `argv` directly and then allocates, so between the call and the first allocation its
/// arguments are reachable from nowhere the collector looks.
///
/// The symptom was `[1, 2].map(f)` calling `f` zero times under `CRISOL_GC_STRESS`: allocating
/// the result array collected the callback, and the call landed on `crisol_not_a_function`.
/// `this` needs it too — `a.map(…)` leaves `a` dead at the call site, so the array being
/// mapped is no more rooted than the callback.
fn with_rooted<R>(values: &[u64], body: impl FnOnce() -> R) -> R {
    with_runtime(|runtime| {
        let scope = runtime.heap.scope();
        let _roots: Vec<_> = values
            .iter()
            .filter_map(|value| handle_of(*value))
            .map(|handle| scope.root(handle))
            .collect();
        body()
    })
}

/// Everything a built-in call must keep alive: the receiver and every argument.
///
/// # Safety
///
/// `argv` must point to `argc` readable values.
unsafe fn live_values(this_value: u64, argc: u64, argv: *const u64) -> Vec<u64> {
    let mut values = vec![this_value];
    for index in 0..argc as usize {
        // SAFETY: bounds come from the count the caller passed.
        values.push(unsafe { argument(argc, argv, index) });
    }
    values
}

/// The elements of `this`, if it is an array.
fn elements_of(this_value: u64) -> Option<(GcRef, usize)> {
    let handle = handle_of(this_value)?;
    let count = with_runtime(|runtime| runtime.heap.element_count(handle))?;
    Some((handle, count))
}

/// Allocates an array already rooted for the caller's use.
///
/// The `Rooted` guard matters more than it looks: a built-in fills its result by calling back
/// into JavaScript, every such call can allocate, and an unrooted half-built array would be
/// collected part-way through being filled.
fn with_new_array<R>(length: usize, body: impl FnOnce(GcRef) -> R) -> R {
    with_runtime(|runtime| {
        let shape = runtime.shapes.borrow().root();
        let scope = runtime.heap.scope();
        let array = scope.alloc(shape, 0);
        runtime.heap.make_array(array.handle(), length);
        if let Some(prototype) = ARRAY_PROTOTYPE.with(std::cell::Cell::get) {
            runtime.heap.set_prototype(array.handle(), Some(prototype));
        }
        body(array.handle())
    })
}

/// `ArraySpeciesCreate(originalArray, length)` — the array a species-aware method builds into.
///
/// **crisol has no `Array` subclassing** (`class X extends Array` is refused), so a species
/// that resolves to a real `Array` — which is every non-throwing case — produces an array
/// indistinguishable from `ArrayCreate(length)`. What this call is *for*, then, is its
/// observable lookups: reading `originalArray.constructor` and `constructor[@@species]`, either
/// of which may be a getter that throws, and calling a custom species constructor, which may
/// throw or may not be a constructor. test262 checks each of those directly, and checks that
/// they happen *before* the method touches an element.
///
/// The result is used only to propagate a throw; the caller builds the array it returns with
/// [`with_new_array`]. A custom species that returns without throwing has its result discarded
/// — a deviation with no observable consequence while `Array` cannot be subclassed, and
/// recorded rather than pretended away (D-222).
fn array_species_create(original: u64, length: usize) -> u64 {
    // `IsArray(O)` is false → `ArrayCreate`, with no constructor lookup at all.
    if elements_of(original).is_none() {
        return with_new_array(length, |array| array.to_value().to_bits());
    }
    // **`C` starts as the constructor itself**, not as `undefined`. A non-object, non-nullish
    // constructor — `a.constructor = 1` — is neither replaced by a species (it has none) nor
    // taken as the default; it falls through to the `IsConstructor` check and throws, which is
    // what `create-ctor-non-object` requires. Defaulting `C` to `undefined` here quietly used
    // `Array` instead.
    let mut c = property_of(original, "constructor");
    if Value::from_bits(c).is_exception() {
        return c;
    }
    if Value::from_bits(c).kind() == crisol_value::Kind::Object {
        let Some(symbol) = well_known_symbol("species") else {
            return with_new_array(length, |array| array.to_value().to_bits());
        };
        c = crisol_computed_load(c, symbol);
        if Value::from_bits(c).is_exception() {
            return c;
        }
    }
    let held = Value::from_bits(c);
    // **`null` and `undefined` both mean the default**, which is `Array` — the one place the
    // two nullish values are treated alike here.
    if held.is_undefined() || held.kind() == crisol_value::Kind::Null {
        return with_new_array(length, |array| array.to_value().to_bits());
    }
    if !is_constructor(c) {
        return raise("the array species is not a constructor", "TypeError");
    }
    #[expect(clippy::cast_precision_loss, reason = "a length below 2^32")]
    let arg = [Value::number(length as f64).to_bits()];
    // SAFETY: `arg` holds exactly one readable value and outlives the call.
    with_rooted(&[original, c], || unsafe {
        construct_with(c, c, 1, arg.as_ptr())
    })
}

/// Reads one element, or `undefined` past the end.
fn element_at(array: GcRef, index: usize) -> u64 {
    with_runtime(|runtime| runtime.heap.element(array, index))
        .unwrap_or(Value::UNDEFINED)
        .to_bits()
}

/// An index as a JavaScript number, for the second argument every callback gets.
fn index_value(index: usize) -> u64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an index this large is unreachable"
    )]
    let number = index as f64;
    Value::number(number).to_bits()
}

/// Whether `element` is `wanted`, by `===`.
fn same_value(element: Value, wanted: Value) -> bool {
    match (element.as_number(), wanted.as_number()) {
        (Some(left), Some(right)) => left == right,
        (None, None) if element.kind() == crisol_value::Kind::String => text_of(element.to_bits())
            .zip(text_of(wanted.to_bits()))
            .is_some_and(|(a, b)| a == b),
        (None, None) => element == wanted,
        _ => false,
    }
}

/// An index argument, resolved the way the specification does.
///
/// **A negative index counts from the end** — `[1,2,3].slice(-1)` is `[3]` — and anything past
/// either end clamps rather than erroring. `undefined` takes `fallback`, which is why `slice()`
/// with no arguments is the whole array and `slice(1)` runs to the end.
fn relative_index(value: u64, length: usize, fallback: usize) -> Result<usize, u64> {
    // **Only `undefined` takes the default.** That is what makes `slice(1)` and
    // `slice(1, undefined)` the same call — and the reason the others cannot join it: reading
    // `as_number` and falling back on `None` swallowed a string, an object and a symbol
    // alike, so `[1, 2, 3].slice("1")` started at zero and a throwing `valueOf` never ran at
    // all. A throw from a coercion is the answer, not a default.
    if Value::from_bits(value).is_undefined() {
        return Ok(fallback);
    }
    let number = coerce_number(value)?;
    if number.is_nan() {
        return Ok(0);
    }
    #[expect(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
    let span = length as f64;
    let resolved = if number < 0.0 {
        (span + number).max(0.0)
    } else {
        number.min(span)
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped into 0..=length just above"
    )]
    let index = resolved as usize;
    Ok(index)
}

/// How many elements an **array-like** has.
///
/// **Not just an array.** test262 applies the array methods to anything with a `length` and
/// indexed properties — `Array.prototype.filter.call(new String("abc"), …)` is a whole family
/// of its cases — and a method that insisted on real elements answered `undefined` for every
/// one of them. An array answers from its element count, which is why that stays the first
/// question.
fn indexed_length(value: u64) -> Result<usize, u64> {
    if let Some((_, length)) = elements_of(value) {
        return Ok(length);
    }
    let key = "length".to_owned();
    // SAFETY: `key` is a live Rust string.
    let asked = unsafe { crisol_property_load(value, key.as_ptr(), key.len() as u64) };
    if Value::from_bits(asked).is_exception() {
        // A getter threw. Its exception is the answer, not a length of zero.
        return Err(asked);
    }
    let asked = coerce_number(asked)?;
    if !asked.is_finite() || asked <= 0.0 {
        return Ok(0);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to a length no array can exceed"
    )]
    let length = asked.min(f64::from(u32::MAX)) as usize;
    Ok(length)
}

/// Reads a property named by a symbol, walking the chain as the named path does.
///
/// **Separate from `crisol_property_load` because that one takes text**, and a symbol has
/// none that identifies it. Routing a symbol through its description is what made two
/// symbols described alike the same property — the key reached `key_of` correctly and was
/// then flattened back to a string one line later.
///
/// None of the named path's special cases apply: a symbol is never an index, never `length`,
/// and never a character of a string.
fn symbol_property_load(object: u64, key: &PropertyKey) -> u64 {
    /// A value to hand back, or an accessor still to be called. As in `crisol_property_load`,
    /// the getter runs outside the runtime borrow.
    enum Found {
        Value(u64),
        Get(u64),
    }

    if let Some((target, handler)) = proxy_parts(object) {
        return proxy_load(object, target, handler, key);
    }

    let Some(handle) = handle_of(object) else {
        return Value::UNDEFINED.to_bits();
    };
    let found = with_runtime(|runtime| {
        let mut current = Some(handle);
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let Some(step) = current else { break };
            let Some(shape) = runtime.heap.shape_of(step) else {
                break;
            };
            let slot = runtime.shapes.borrow().lookup(shape, key);
            if let Some(slot) = slot
                && !runtime.heap.is_deleted(step, slot.index())
                && let Some(value) = runtime.heap.get(step, slot.index())
            {
                if runtime.heap.attributes_of(step, slot.index()).accessor {
                    return Found::Get(value.to_bits());
                }
                return Found::Value(value.to_bits());
            }
            current = runtime.heap.prototype_of(step);
        }
        Found::Value(Value::UNDEFINED.to_bits())
    });
    match found {
        Found::Value(value) => value,
        Found::Get(pair) => match elements_of(pair) {
            Some((functions, _)) => {
                let getter = element_at(functions, 0);
                if is_callable(getter) {
                    call_value(getter, object, &[])
                } else {
                    Value::UNDEFINED.to_bits()
                }
            }
            None => Value::UNDEFINED.to_bits(),
        },
    }
}

/// Writes a property named by a symbol. See [`symbol_property_load`].
fn symbol_property_store(object: u64, key: &PropertyKey, value: u64) -> u64 {
    if let Some((target, handler)) = proxy_parts(object) {
        return proxy_store(object, target, handler, key, value);
    }
    let Some(handle) = handle_of(object) else {
        return Value::UNDEFINED.to_bits();
    };
    // A setter anywhere up the chain receives the write, as it does for a named property.
    let accessor = with_runtime(|runtime| {
        let mut current = Some(handle);
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let step = current?;
            let shape = runtime.heap.shape_of(step)?;
            let slot = runtime.shapes.borrow().lookup(shape, key);
            if let Some(slot) = slot
                && !runtime.heap.is_deleted(step, slot.index())
            {
                return runtime
                    .heap
                    .attributes_of(step, slot.index())
                    .accessor
                    .then(|| runtime.heap.get(step, slot.index()))
                    .flatten()
                    .map(|pair| pair.to_bits());
            }
            current = runtime.heap.prototype_of(step);
        }
        None
    });
    if let Some(pair) = accessor {
        if let Some((functions, _)) = elements_of(pair) {
            let setter = element_at(functions, 1);
            if is_callable(setter) {
                return call_value(setter, object, &[value]);
            }
        }
        return Value::UNDEFINED.to_bits();
    }
    if symbol_own_slot(object, key).is_none() && !is_extensible(object) {
        return Value::UNDEFINED.to_bits();
    }
    // **Rooted here, where the shape is about to hold the address**, rather than on every
    // read of a symbol key. A shape outlives the objects using it, so the cell has to survive
    // as long as the shape names it (D-192) — but a read that stores nothing has no such
    // claim, and rooting there grew the set for symbols the heap never recorded.
    if let Some(address) = key.symbol_address() {
        KEY_SYMBOLS.with(|symbols| {
            if let Ok(mut entries) = symbols.try_borrow_mut() {
                entries.insert(Value::symbol(address).to_bits());
            }
        });
    }
    with_runtime(|runtime| {
        let Some(current) = runtime.heap.shape_of(handle) else {
            return;
        };
        let (shape, slot, width) = {
            let mut shapes = runtime.shapes.borrow_mut();
            let shape = shapes.add(current, key);
            let Some(slot) = shapes.lookup(shape, key) else {
                return;
            };
            (shape, slot, shapes.len(shape) as usize)
        };
        if shape != current {
            runtime.heap.transition(handle, shape, width);
        } else if runtime.heap.is_deleted(handle, slot.index()) {
            runtime.heap.set_deleted(handle, slot.index(), false);
            runtime
                .heap
                .set_attributes(handle, slot.index(), crisol_value::Attributes::DATA);
        } else if !runtime.heap.attributes_of(handle, slot.index()).writable {
            return;
        }
        runtime
            .heap
            .set(handle, slot.index(), Value::from_bits(value));
    });
    Value::UNDEFINED.to_bits()
}

/// The slot a symbol-keyed own property occupies, if it has one.
fn symbol_own_slot(object: u64, key: &PropertyKey) -> Option<u32> {
    let handle = handle_of(object)?;
    with_runtime(|runtime| {
        let shape = runtime.heap.shape_of(handle)?;
        let slot = runtime.shapes.borrow().lookup(shape, key)?;
        (!runtime.heap.is_deleted(handle, slot.index())).then_some(slot.index())
    })
}

/// `ToIntegerOrInfinity` on argument `position`, or the reason it has no number.
///
/// **Absent is zero, and so is `NaN`** — the rule that makes `"abc".charAt()` the first
/// character rather than an error. A symbol is neither, and neither is an object whose
/// conversion fails, so the answer has to be able to say so.
///
/// **And it truncates.** `"abc".charAt(1.7)` is `"b"`; reading the argument as a raw number
/// and indexing with it gave whatever the cast did with the fraction, which was right for
/// every whole number anybody tests by hand.
fn integer_argument(argc: u64, argv: *const u64, position: usize) -> Result<f64, u64> {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, position) };
    let number = coerce_number(value)?;
    if number.is_nan() {
        return Ok(0.0);
    }
    Ok(number.trunc())
}

/// `ToPrimitive` — an object as the primitive it stands for, or the reason it has none.
///
/// **Both methods answering objects is an error**, not a value. The older `to_primitive` hands
/// the object back instead, which turns a reportable `TypeError` into arithmetic on `NaN` —
/// and test262 checks both that the throw happens *and* that `valueOf` and `toString` were
/// each tried, so answering wrongly and answering without asking are separately caught.
fn coerce_primitive(value: u64, prefer_string: bool) -> Result<u64, u64> {
    if Value::from_bits(value).kind() != crisol_value::Kind::Object {
        return Ok(value);
    }
    let order = if prefer_string {
        ["toString", "valueOf"]
    } else {
        ["valueOf", "toString"]
    };
    for name in order {
        let key = name.to_owned();
        // SAFETY: `key` is a live Rust string.
        let method = unsafe { crisol_property_load(value, key.as_ptr(), key.len() as u64) };
        if !is_callable(method) {
            continue;
        }
        let result = with_rooted(&[value], || call_value(method, value, &[]));
        if Value::from_bits(result).is_exception() {
            return Err(result);
        }
        if Value::from_bits(result).kind() != crisol_value::Kind::Object {
            return Ok(result);
        }
    }
    Err(raise(
        "cannot convert an object to a primitive",
        "TypeError",
    ))
}

/// `ToNumber`, with the two conversions that are errors rather than `NaN`.
///
/// **A symbol refuses to be a number.** `+Symbol()` is a `TypeError`, not `NaN`, and the
/// reason is the point of symbols: one exists to be unequal to everything, and a number it
/// could be compared as would defeat that. `to_number` answers `NaN`, which is what every
/// caller that cannot throw still gets.
fn coerce_number(bits: u64) -> Result<f64, u64> {
    match Value::from_bits(bits).kind() {
        crisol_value::Kind::Symbol => Err(raise("a symbol is not a number", "TypeError")),
        crisol_value::Kind::Object => {
            let primitive = coerce_primitive(bits, false)?;
            // One step only: `coerce_primitive` answers a primitive or an error, so this
            // cannot reach the object arm again.
            coerce_number(primitive)
        }
        _ => Ok(to_number(bits)),
    }
}

/// Whether an array-like's `length` is one an array could actually have.
///
/// **2^32 is not a length**, and a method that builds a result sized by it has to say so rather
/// than try. `indexed_length` clamps, which is right for walking — it keeps a loop finite — and
/// wrong for allocating, because the clamped value looks buildable and is not. So the question
/// is asked separately by the methods that allocate.
fn indexed_length_is_valid(value: u64) -> bool {
    if elements_of(value).is_some() {
        return true;
    }
    property_number(value, "length").is_none_or(|length| length < 4_294_967_296.0)
}

/// The element at `index` of an array-like.
fn indexed_get(value: u64, index: usize) -> u64 {
    if let Some((array, length)) = elements_of(value) {
        if index < length {
            return element_at(array, index);
        }
        return Value::UNDEFINED.to_bits();
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "an index below the clamp in `indexed_length`"
    )]
    let key = number_text(index as f64);
    // SAFETY: `key` is a live Rust string.
    unsafe { crisol_property_load(value, key.as_ptr(), key.len() as u64) }
}

/// How many positions a method that *walks* an array-like has to visit.
///
/// **`ToLength`, which clamps at 2^53-1, not at 2^32-1.** The smaller clamp is right for an
/// array — no array can be longer — and wrong for a plain object, whose `length` is whatever
/// it says. The difference is observable rather than theoretical: test262 reverses
/// `{length: 2 ** 53 + 2}` with a getter at the top and expects the *first* step to reach it,
/// which a walk that clamped to four billion never does. It read `undefined` two billion
/// times instead and was killed by the harness.
///
/// Separate from [`indexed_length`] because the two answer different questions. This one
/// bounds a walk, and a walk of 2^53 steps that never throws does not finish — which the
/// case timeout exists to name (D-205). `indexed_length` bounds an *allocation*, where the
/// four-billion clamp is the point.
fn walk_length(value: u64) -> Result<usize, u64> {
    if let Some((_, length)) = elements_of(value) {
        return Ok(length);
    }
    let key = "length";
    // SAFETY: `key` is a live Rust string.
    let asked = unsafe { crisol_property_load(value, key.as_ptr(), key.len() as u64) };
    if Value::from_bits(asked).is_exception() {
        return Err(asked);
    }
    let asked = coerce_number(asked)?;
    if !asked.is_finite() || asked <= 0.0 {
        return Ok(0);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to the largest integer a length may be"
    )]
    let length = asked.min(crisol_builtins::MAX_SAFE_INTEGER) as usize;
    Ok(length)
}

/// The element at `index`, or the exception a getter raised reaching for it.
///
/// **A getter can throw, and the walkers have to stop when one does.** `indexed_get` answers
/// the signal like any other value, which is right for a caller that stores it and wrong for
/// a loop that keeps going — and "keeps going" over a length of 2^53 is not slow, it is
/// stuck.
fn indexed_get_checked(value: u64, index: usize) -> Result<u64, u64> {
    let element = indexed_get(value, index);
    if Value::from_bits(element).is_exception() {
        return Err(element);
    }
    Ok(element)
}

/// Writes the element at `index` of an array-like.
///
/// The counterpart of [`indexed_get`], and it exists for the same reason: **test262 applies
/// the array methods to anything with a `length`**, and a method that wrote only to real
/// elements silently did nothing on `Array.prototype.reverse.call({0: 1, 1: 2, length: 2})`.
///
/// A real array still takes the element path; the branch costs one test per element, which is
/// what generality over a plain object costs when the fast case has to stay fast.
fn indexed_set(value: u64, index: usize, item: u64) {
    if let Some((array, length)) = elements_of(value)
        && index < length
    {
        with_runtime(|runtime| {
            runtime
                .heap
                .set_element(array, index, Value::from_bits(item));
        });
        return;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "an index below the clamp in `indexed_length`"
    )]
    let key = number_text(index as f64);
    // SAFETY: `key` is a live Rust string, and `item` is rooted by the caller.
    unsafe {
        crisol_property_store(value, key.as_ptr(), key.len() as u64, item);
    }
}

/// Removes the element at `index` of an array-like.
///
/// **A hole is not `undefined`**, which is the whole reason this is separate from writing one:
/// `delete` leaves the position absent, and `"0" in a` is how the corpus tells the two apart.
fn indexed_delete(value: u64, index: usize) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an index below the clamp in `indexed_length`"
    )]
    let key = Value::number(index as f64);
    // The answer is whether the delete was allowed; a method that is removing an element it
    // has already read has nothing to do with a refusal, and the specification does not look
    // at it either.
    let _allowed = crisol_delete(value, key.to_bits());
}

/// Writes an array-like's `length`.
///
/// A real array's is derived from its elements and cannot be assigned here, so this only has
/// work to do for everything else — which is exactly where the corpus looks.
fn indexed_set_length(value: u64, length: usize) {
    if elements_of(value).is_some() {
        return;
    }
    let key = "length";
    #[expect(clippy::cast_precision_loss, reason = "lengths are far below 2^53")]
    let count = Value::number(length as f64).to_bits();
    // SAFETY: `key` is a live Rust string.
    unsafe {
        crisol_property_store(value, key.as_ptr(), key.len() as u64, count);
    }
}

/// `Array.prototype.map` — a new array of the results.
///
/// The callback gets `(element, index, array)`, which is the specification's signature and not
/// a convenience: code that passes a method as a callback depends on the extra arguments
/// arriving, and code that ignores them is unaffected by their presence.
extern "C" fn array_map(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let receiver = match object_receiver(this_value) {
        Ok(receiver) => receiver,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        with_rooted(&[receiver], || {
            // **A length of 2^32 or more is a `RangeError`**, because the result cannot exist.
            // Walking such a thing is merely slow; building one is four billion allocations,
            // and that arrived as a killed process rather than an error a program could catch.
            if !indexed_length_is_valid(receiver) {
                return raise("invalid array length", "RangeError");
            }
            let length = match indexed_length(receiver) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            // SAFETY: the convention guarantees `argc` readable values at `argv`.
            let callback = unsafe { argument(argc, argv, 0) };
            if !is_callable(callback) {
                return raise("a callback must be a function", "TypeError");
            }
            // Before a single element is read, so a throwing species lookup leaves the callback
            // uncalled — `create-species-poisoned` asserts a call count of zero.
            let probe = array_species_create(receiver, length);
            if Value::from_bits(probe).is_exception() {
                return probe;
            }
            // SAFETY: as above.
            let this_arg = unsafe { argument(argc, argv, 1) };
            with_new_array(length, |result| {
                for index in 0..length {
                    let element = match indexed_get_checked(receiver, index) {
                        Ok(element) => element,
                        Err(thrown) => return thrown,
                    };
                    // **The callback's `this` is the second argument, not the array.**
                    // `[11].map(fn, o)` runs `fn` with `this === o`.
                    let mapped =
                        call_value(callback, this_arg, &[element, index_value(index), receiver]);
                    // A throw from the callback stops the walk and reaches the caller.
                    if Value::from_bits(mapped).is_exception() {
                        return mapped;
                    }
                    with_runtime(|runtime| {
                        runtime
                            .heap
                            .set_element(result, index, Value::from_bits(mapped))
                    });
                }
                result.to_value().to_bits()
            })
        })
    })
}

/// `Array.prototype.filter` — the elements the callback keeps.
extern "C" fn array_filter(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let receiver = match object_receiver(this_value) {
        Ok(receiver) => receiver,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        with_rooted(&[receiver], || {
            let length = match indexed_length(receiver) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            // SAFETY: as above.
            let callback = unsafe { argument(argc, argv, 0) };
            if !is_callable(callback) {
                return raise("a callback must be a function", "TypeError");
            }
            // Like `map`, before the callback so a throwing species lookup leaves the count at
            // zero. `filter` species-creates with length zero in the specification; the result
            // is discarded either way (see `array_species_create`).
            let probe = array_species_create(receiver, 0);
            if Value::from_bits(probe).is_exception() {
                return probe;
            }
            // Allocated at full length and shortened after, because the result is rooted
            // through the whole loop and the count is not known until the end.
            // SAFETY: as above.
            let this_arg = unsafe { argument(argc, argv, 1) };
            with_new_array(length, |result| {
                let mut kept = 0;
                for index in 0..length {
                    let element = match indexed_get_checked(receiver, index) {
                        Ok(element) => element,
                        Err(thrown) => return thrown,
                    };
                    let verdict =
                        call_value(callback, this_arg, &[element, index_value(index), receiver]);
                    if Value::from_bits(verdict).is_exception() {
                        return verdict;
                    }
                    if is_truthy(Value::from_bits(verdict)) {
                        with_runtime(|runtime| {
                            runtime
                                .heap
                                .set_element(result, kept, Value::from_bits(element))
                        });
                        kept += 1;
                    }
                }
                // `truncate_elements`, not `make_array`: the latter replaces the elements, so
                // sizing the result this way discarded everything `filter` had just kept.
                with_runtime(|runtime| runtime.heap.truncate_elements(result, kept));
                result.to_value().to_bits()
            })
        })
    })
}

/// `Array.prototype.forEach` — the callback for its effects, and `undefined`.
extern "C" fn array_for_each(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let receiver = match object_receiver(this_value) {
        Ok(receiver) => receiver,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        with_rooted(&[receiver], || {
            let length = match indexed_length(receiver) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            // SAFETY: as above.
            let callback = unsafe { argument(argc, argv, 0) };
            if !is_callable(callback) {
                return raise("a callback must be a function", "TypeError");
            }
            // SAFETY: as above.
            let this_arg = unsafe { argument(argc, argv, 1) };
            for index in 0..length {
                let element = match indexed_get_checked(receiver, index) {
                    Ok(element) => element,
                    Err(thrown) => return thrown,
                };
                let outcome =
                    call_value(callback, this_arg, &[element, index_value(index), receiver]);
                if Value::from_bits(outcome).is_exception() {
                    return outcome;
                }
            }
            Value::UNDEFINED.to_bits()
        })
    })
}

/// `Array.prototype.reduce`.
///
/// **Without an initial value the first element is the seed and the walk starts at the
/// second** — not `undefined` as the seed, which would make `[1, 2].reduce(add)` `NaN` rather
/// than `3`. On an empty array with no seed the specification throws; that needs a throw path
/// M13 does not have, so this yields `undefined`.
extern "C" fn array_reduce(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let receiver = match object_receiver(this_value) {
        Ok(receiver) => receiver,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        with_rooted(&[receiver], || {
            let length = match indexed_length(receiver) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            // SAFETY: as above.
            let callback = unsafe { argument(argc, argv, 0) };
            if !is_callable(callback) {
                return raise("a callback must be a function", "TypeError");
            }
            let (mut accumulator, start) = if argc >= 2 {
                // SAFETY: as above.
                (unsafe { argument(argc, argv, 1) }, 0)
            } else if length == 0 {
                // **Empty with no seed is a `TypeError`**, not `undefined`: there is no value to
                // answer with, and inventing one makes `[].reduce(add)` quietly wrong where the
                // specification is loud.
                return raise(
                    "reduce of an empty array with no initial value",
                    "TypeError",
                );
            } else {
                match indexed_get_checked(receiver, 0) {
                    Ok(first) => (first, 1),
                    Err(thrown) => return thrown,
                }
            };
            for index in start..length {
                let element =
                    match with_rooted(&[accumulator], || indexed_get_checked(receiver, index)) {
                        Ok(element) => element,
                        Err(thrown) => return thrown,
                    };
                accumulator = with_rooted(&[accumulator, element], || {
                    call_value(
                        callback,
                        Value::UNDEFINED.to_bits(),
                        &[accumulator, element, index_value(index), receiver],
                    )
                });
                if Value::from_bits(accumulator).is_exception() {
                    return accumulator;
                }
            }
            accumulator
        })
    })
}

/// `Array.prototype.push` — appends, and answers the new length.
extern "C" fn array_push(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let mut at = match indexed_length(this_value) {
            Ok(length) => length,
            Err(thrown) => return thrown,
        };
        for position in 0..argc as usize {
            // SAFETY: as above.
            let value = unsafe { argument(argc, argv, position) };
            indexed_set(this_value, at, value);
            at += 1;
        }
        // **Written back even when nothing was pushed**, which is what `push()` on a frozen
        // array-like throws on and what the specification's step order requires.
        indexed_set_length(this_value, at);
        index_value(at)
    })
}

/// `Array.prototype.lastIndexOf`.
extern "C" fn array_last_index_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let length = match indexed_length(this_value) {
            Ok(length) => length,
            Err(thrown) => return thrown,
        };
        if length == 0 {
            return Value::number(-1.0).to_bits();
        }
        // SAFETY: as above.
        let wanted = unsafe { argument(argc, argv, 0) };
        #[expect(clippy::cast_precision_loss, reason = "a length below 2^32")]
        let span = length as f64;
        // **`fromIndex` defaults to the last index, not zero** — the walk starts from the end.
        let from = if argc >= 2 {
            match integer_argument(argc, argv, 1) {
                Ok(from) => from,
                Err(thrown) => return thrown,
            }
        } else {
            span - 1.0
        };
        let start = if from >= 0.0 {
            from.min(span - 1.0)
        } else {
            span + from
        };
        if start < 0.0 {
            return Value::number(-1.0).to_bits();
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked non-negative and clamped to len-1 above"
        )]
        let start = start as usize;
        for index in (0..=start).rev() {
            if !indexed_has(this_value, index) {
                continue;
            }
            let element = match indexed_get_checked(this_value, index) {
                Ok(element) => element,
                Err(thrown) => return thrown,
            };
            if strict_equal_bool(element, wanted) {
                return index_value(index);
            }
        }
        Value::number(-1.0).to_bits()
    })
}

/// `Array.prototype.includes`.
///
/// **Unlike `indexOf` this finds `NaN`**: it uses SameValueZero rather than `===`, so
/// `[NaN].includes(NaN)` is true where `[NaN].indexOf(NaN)` is `-1`.
extern "C" fn array_includes(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let length = match indexed_length(this_value) {
            Ok(length) => length,
            Err(thrown) => return thrown,
        };
        if length == 0 {
            return Value::FALSE.to_bits();
        }
        // SAFETY: as above.
        let wanted = unsafe { argument(argc, argv, 0) };
        let from = match integer_argument(argc, argv, 1) {
            Ok(from) => from,
            Err(thrown) => return thrown,
        };
        #[expect(clippy::cast_precision_loss, reason = "a length below 2^32")]
        let span = length as f64;
        let start = if from >= 0.0 {
            from.min(span)
        } else {
            (span + from).max(0.0)
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=len just above"
        )]
        let start = start as usize;
        // **SameValueZero**, which is strict equality *except* that `NaN` matches `NaN` —
        // so `[NaN].includes(NaN)` is true where `indexOf` is `-1`, and `-0` still equals `0`.
        // Written with `strict_equal_bool` so a string compares by value, not by cell address
        // (the bug `same_value` carried here, the twin of D-230).
        let seeking_nan = Value::from_bits(wanted)
            .as_number()
            .is_some_and(f64::is_nan);
        for index in start..length {
            // **`includes` does not skip a hole** — it reads it as `undefined`, unlike
            // `indexOf`. So no `HasProperty` check here, only the propagating read.
            let element = match indexed_get_checked(this_value, index) {
                Ok(element) => element,
                Err(thrown) => return thrown,
            };
            let found = if seeking_nan {
                Value::from_bits(element)
                    .as_number()
                    .is_some_and(f64::is_nan)
            } else {
                strict_equal_bool(element, wanted)
            };
            if found {
                return Value::TRUE.to_bits();
            }
        }
        Value::FALSE.to_bits()
    })
}

/// `Array.prototype.join`.
extern "C" fn array_join(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 0) };
    let separator = if Value::from_bits(given).kind() == crisol_value::Kind::Undefined {
        ",".to_owned()
    } else {
        to_text(given).unwrap_or_else(|| ",".to_owned())
    };

    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let mut out = String::new();
        for index in 0..length {
            // **The length comes from the program**, so the result does too — and a walk
            // that only checks the index would build until it ran out of memory. Checked as
            // it grows rather than predicted, because each element's text is whatever its
            // `toString` decides.
            if out.len() > MAX_STRING_UNITS {
                return raise("joined string is too long", "RangeError");
            }
            if index > 0 {
                out.push_str(&separator);
            }
            let element = Value::from_bits(indexed_get(this_value, index));
            // **`null` and `undefined` join as empty**, not as their names.
            if !matches!(
                element.kind(),
                crisol_value::Kind::Null | crisol_value::Kind::Undefined
            ) && let Some(text) = to_text(element.to_bits())
            {
                out.push_str(&text);
            }
        }
        new_string(&out)
    })
}

/// `Array.prototype.slice`.
extern "C" fn array_slice(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = match relative_index(unsafe { argument(argc, argv, 0) }, length, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let end = match relative_index(unsafe { argument(argc, argv, 1) }, length, length) {
        Ok(end) => end,
        Err(thrown) => return thrown,
    };
    let taken = end.saturating_sub(start);

    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // **The species lookup happens before any element is read**, and it can throw — a
        // constructor or `@@species` getter that does is the whole of `create-species-abrupt`
        // and its neighbours. Inside `with_rooted` so the discarded probe array's allocation
        // cannot collect the receiver that is not yet held anywhere else (D-208 territory).
        let probe = array_species_create(this_value, taken);
        if Value::from_bits(probe).is_exception() {
            return probe;
        }
        with_new_array(taken, |result| {
            for offset in 0..taken {
                let element = indexed_get(this_value, start + offset);
                with_runtime(|runtime| {
                    runtime
                        .heap
                        .set_element(result, offset, Value::from_bits(element))
                });
            }
            result.to_value().to_bits()
        })
    })
}

/// `Array.prototype.concat`.
extern "C" fn array_concat(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // **An array argument is spread and anything else appended whole**, which is what makes
        // `[1].concat([2, 3])` three elements and `[1].concat(2)` two.
        let mut flattened: Vec<u64> = Vec::new();
        let mut take = |value: u64| {
            if let Some((array, length)) = elements_of(value) {
                for index in 0..length {
                    flattened.push(element_at(array, index));
                }
            } else {
                flattened.push(value);
            }
        };
        take(this_value);
        for position in 0..argc as usize {
            // SAFETY: as above.
            take(unsafe { argument(argc, argv, position) });
        }
        array_of_values(&flattened)
    })
}

/// `Array.prototype.reverse`, in place.
///
/// **Not just an array.** `Array.prototype.reverse.call({0: 1, 1: 2, length: 2})` reverses
/// that object's properties, and reading its `length` may throw — which has to reach the
/// caller rather than be a quiet no-op.
extern "C" fn array_reverse(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let length = match walk_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    for index in 0..length / 2 {
        let mirror = length - 1 - index;
        // **Lower then upper, which is the specification's order and is observable**: both
        // may be getters, and which one throws first decides what the program sees.
        let left = match indexed_get_checked(this_value, index) {
            Ok(left) => left,
            Err(thrown) => return thrown,
        };
        let right = match with_rooted(&[this_value, left], || {
            indexed_get_checked(this_value, mirror)
        }) {
            Ok(right) => right,
            Err(thrown) => return thrown,
        };
        with_rooted(&[this_value, left, right], || {
            indexed_set(this_value, index, right);
            indexed_set(this_value, mirror, left);
        });
    }
    this_value
}

/// `Array.prototype.pop`.
///
/// **An empty array-like still has its `length` written back**, and that write is what
/// `Array.prototype.pop.call("")` throws on — a string's `length` is not writable. Returning
/// early on an empty receiver skipped it and answered `undefined` quietly.
extern "C" fn array_pop(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    if length == 0 {
        indexed_set_length(this_value, 0);
        return Value::UNDEFINED.to_bits();
    }
    if let Some((array, count)) = elements_of(this_value) {
        let last = element_at(array, count - 1);
        with_runtime(|runtime| runtime.heap.truncate_elements(array, count - 1));
        return last;
    }
    let last = indexed_get(this_value, length - 1);
    with_rooted(&[this_value, last], || {
        indexed_delete(this_value, length - 1);
        indexed_set_length(this_value, length - 1);
    });
    last
}

/// `Array.prototype.shift`.
extern "C" fn array_shift(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    if length == 0 {
        indexed_set_length(this_value, 0);
        return Value::UNDEFINED.to_bits();
    }
    if let Some((array, count)) = elements_of(this_value) {
        let first = element_at(array, 0);
        with_runtime(|runtime| {
            for index in 1..count {
                let moved = runtime
                    .heap
                    .element(array, index)
                    .unwrap_or(Value::UNDEFINED);
                runtime.heap.set_element(array, index - 1, moved);
            }
            runtime.heap.truncate_elements(array, count - 1);
        });
        return first;
    }
    let first = indexed_get(this_value, 0);
    with_rooted(&[this_value, first], || {
        for index in 1..length {
            let Ok(moved) = indexed_get_checked(this_value, index) else {
                // A getter threw. The move stops here rather than reading past it; the
                // signal reaches the caller through the value this answers.
                break;
            };
            with_rooted(&[moved], || indexed_set(this_value, index - 1, moved));
        }
        indexed_delete(this_value, length - 1);
        indexed_set_length(this_value, length - 1);
    });
    first
}

/// `Array.prototype.unshift`.
extern "C" fn array_unshift(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    let added = argc as usize;
    if added == 0 {
        // **Still written back.** `unshift()` with nothing sets `length` to what it already
        // was, and on a receiver that refuses the write that is where it throws.
        indexed_set_length(this_value, length);
        return index_value(length);
    }
    if let Some((array, count)) = elements_of(this_value) {
        with_runtime(|runtime| {
            // Grown first, then moved from the back, so nothing is overwritten before it has
            // moved.
            runtime
                .heap
                .set_element(array, count + added - 1, Value::UNDEFINED);
            for index in (0..count).rev() {
                let moved = runtime
                    .heap
                    .element(array, index)
                    .unwrap_or(Value::UNDEFINED);
                runtime.heap.set_element(array, index + added, moved);
            }
            for position in 0..added {
                // SAFETY: the convention guarantees `argc` readable values at `argv`.
                let value = unsafe { argument(argc, argv, position) };
                runtime
                    .heap
                    .set_element(array, position, Value::from_bits(value));
            }
        });
        return index_value(count + added);
    }
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        for index in (0..length).rev() {
            let Ok(moved) = indexed_get_checked(this_value, index) else {
                break;
            };
            with_rooted(&[moved], || indexed_set(this_value, index + added, moved));
        }
        for position in 0..added {
            // SAFETY: as above.
            let value = unsafe { argument(argc, argv, position) };
            indexed_set(this_value, position, value);
        }
        indexed_set_length(this_value, length + added);
    });
    index_value(length + added)
}

/// `find` and `findIndex`, which differ only in what they answer with.
fn find_with(this_value: u64, argc: u64, argv: *const u64, want_index: bool) -> u64 {
    let receiver = match object_receiver(this_value) {
        Ok(receiver) => receiver,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        with_rooted(&[receiver], || {
            let length = match indexed_length(receiver) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            // SAFETY: as above.
            let callback = unsafe { argument(argc, argv, 0) };
            if !is_callable(callback) {
                return raise("a callback must be a function", "TypeError");
            }
            // SAFETY: as above.
            let this_arg = unsafe { argument(argc, argv, 1) };
            for index in 0..length {
                // **`find` reads a hole**, unlike `forEach` which skips one — it visits every
                // index, so a throwing getter has to propagate rather than be skipped.
                let element = match indexed_get_checked(receiver, index) {
                    Ok(element) => element,
                    Err(thrown) => return thrown,
                };
                let verdict =
                    call_value(callback, this_arg, &[element, index_value(index), receiver]);
                if Value::from_bits(verdict).is_exception() {
                    return verdict;
                }
                if is_truthy(Value::from_bits(verdict)) {
                    return if want_index {
                        index_value(index)
                    } else {
                        element
                    };
                }
            }
            // **`find` answers `undefined` and `findIndex` answers `-1`** when nothing matches.
            if want_index {
                Value::number(-1.0).to_bits()
            } else {
                Value::UNDEFINED.to_bits()
            }
        })
    })
}

/// `Array.prototype.find`.
extern "C" fn array_find(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    find_with(this_value, argc, argv, false)
}

/// `Array.prototype.findIndex`.
extern "C" fn array_find_index(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    find_with(this_value, argc, argv, true)
}

/// `every` and `some`, which differ only in what stops them.
/// `ToObject(this)` for the generic array methods, or a `TypeError` for a nullish receiver.
///
/// **A primitive receiver is boxed, not read raw.** `Array.prototype.every.call(2.5, cb)` runs
/// `cb` with a `Number` object as the array — `obj instanceof Number` is true — where reading
/// `2.5` directly gave the callback a primitive. `to_object` returns a real array unchanged, so
/// the fast path is untouched.
fn object_receiver(this_value: u64) -> Result<u64, u64> {
    to_object(this_value).ok_or_else(|| {
        raise(
            "an array method needs a receiver that is not null or undefined",
            "TypeError",
        )
    })
}

fn quantify(this_value: u64, argc: u64, argv: *const u64, want_all: bool) -> u64 {
    // ToObject first, so a primitive receiver is boxed and a nullish one throws before the
    // length is read or the callback checked.
    let receiver = match object_receiver(this_value) {
        Ok(receiver) => receiver,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        with_rooted(&[receiver], || {
            let length = match indexed_length(receiver) {
                Ok(length) => length,
                Err(thrown) => return thrown,
            };
            // SAFETY: as above.
            let callback = unsafe { argument(argc, argv, 0) };
            if !is_callable(callback) {
                return raise("a callback must be a function", "TypeError");
            }
            // SAFETY: as above.
            let this_arg = unsafe { argument(argc, argv, 1) };
            for index in 0..length {
                let element = match indexed_get_checked(receiver, index) {
                    Ok(element) => element,
                    Err(thrown) => return thrown,
                };
                let verdict =
                    call_value(callback, this_arg, &[element, index_value(index), receiver]);
                if Value::from_bits(verdict).is_exception() {
                    return verdict;
                }
                if is_truthy(Value::from_bits(verdict)) != want_all {
                    return if want_all { Value::FALSE } else { Value::TRUE }.to_bits();
                }
            }
            // **Empty is `true` for `every` and `false` for `some`**, which follows from each
            // stopping on the opposite answer and neither ever stopping.
            if want_all { Value::TRUE } else { Value::FALSE }.to_bits()
        })
    })
}

/// `Array.prototype.every`.
extern "C" fn array_every(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    quantify(this_value, argc, argv, true)
}

/// `Array.prototype.some`.
extern "C" fn array_some(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    quantify(this_value, argc, argv, false)
}

/// `Array.prototype.fill`.
extern "C" fn array_fill(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let length = match indexed_length(this_value) {
        Ok(length) => length,
        Err(thrown) => return thrown,
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let start = match relative_index(unsafe { argument(argc, argv, 1) }, length, 0) {
        Ok(start) => start,
        Err(thrown) => return thrown,
    };
    // SAFETY: as above.
    let end = match relative_index(unsafe { argument(argc, argv, 2) }, length, length) {
        Ok(end) => end,
        Err(thrown) => return thrown,
    };
    with_rooted(&[this_value, value], || {
        for index in start..end {
            indexed_set(this_value, index, value);
        }
    });
    this_value
}

/// `Array.prototype.indexOf`, by `===` on numbers and by identity otherwise.
/// `HasProperty` for an array-like index — whether the position is actually there.
///
/// **A hole is not `undefined`.** `indexOf`/`lastIndexOf` skip an absent index rather than
/// comparing it as `undefined`, so `[, undefined].lastIndexOf(undefined)` finds index 1 and not
/// index 0. A dense array is present exactly in range; anything else asks the `in` operator,
/// which walks the prototype chain as `HasProperty` does.
fn indexed_has(value: u64, index: usize) -> bool {
    if let Some((_, length)) = elements_of(value) {
        return index < length;
    }
    if handle_of(value).is_none() {
        return false;
    }
    #[expect(clippy::cast_precision_loss, reason = "an index below 2^32")]
    let key = Value::number(index as f64).to_bits();
    Value::from_bits(crisol_in(key, value))
        .as_boolean()
        .unwrap_or(false)
}

/// `IsStrictlyEqual(a, b)` as a bool — the comparison `indexOf` and `lastIndexOf` use.
///
/// **`===`, not `SameValue`.** `[NaN].indexOf(NaN)` is `-1` and `[-0].indexOf(0)` is `0`; the
/// two rules differ at exactly `NaN` and signed zero, and these methods take the strict one.
fn strict_equal_bool(a: u64, b: u64) -> bool {
    Value::from_bits(crisol_strict_equal(a, b))
        .as_boolean()
        .unwrap_or(false)
}

extern "C" fn array_index_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let length = match indexed_length(this_value) {
            Ok(length) => length,
            Err(thrown) => return thrown,
        };
        // **`len == 0` answers before `fromIndex` is even coerced**, which the specification's
        // step order requires and a throwing `valueOf` there would otherwise observe.
        if length == 0 {
            return Value::number(-1.0).to_bits();
        }
        // SAFETY: as above.
        let wanted = unsafe { argument(argc, argv, 0) };
        let from = match integer_argument(argc, argv, 1) {
            Ok(from) => from,
            Err(thrown) => return thrown,
        };
        #[expect(clippy::cast_precision_loss, reason = "a length below 2^32")]
        let span = length as f64;
        // A negative `fromIndex` counts from the end; past the end means no match.
        let start = if from >= 0.0 {
            from.min(span)
        } else {
            (span + from).max(0.0)
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=len just above"
        )]
        let start = start as usize;
        for index in start..length {
            // A missing index is skipped, not compared as `undefined`.
            if !indexed_has(this_value, index) {
                continue;
            }
            let element = match indexed_get_checked(this_value, index) {
                Ok(element) => element,
                Err(thrown) => return thrown,
            };
            if strict_equal_bool(element, wanted) {
                return index_value(index);
            }
        }
        Value::number(-1.0).to_bits()
    })
}

/// Whether a value is truthy, for `filter`.
fn is_truthy(value: Value) -> bool {
    match value.kind() {
        crisol_value::Kind::Undefined | crisol_value::Kind::Null => false,
        crisol_value::Kind::Boolean => value.as_boolean().unwrap_or(false),
        crisol_value::Kind::Number => value.as_number().is_some_and(|n| n != 0.0 && !n.is_nan()),
        // **An empty string is falsy and every other string is truthy** — the one case where a
        // string's characters decide a branch.
        crisol_value::Kind::String => text_of(value.to_bits()).is_some_and(|text| !text.is_empty()),
        // An object is always truthy, including `new Boolean(false)`.
        _ => true,
    }
}

/// The machine code a closure runs.
///
/// Never null. A value that is not a closure, or one naming a function outside the registered
/// table, yields [`crisol_not_a_function`] — so the caller can jump to whatever this returns
/// without checking, and a wrong callee is a defined outcome rather than a segmentation fault.
/// Putting the check here rather than at every call site costs nothing at all on the hot path.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_closure_code(closure: u64) -> *const u8 {
    let fallback: extern "C" fn(u64, u64, u64, u64, *const u64) -> u64 = crisol_not_a_function;
    let fallback = fallback as *const u8;

    let Some(handle) = handle_of(closure) else {
        return fallback;
    };
    let index =
        with_runtime(|runtime| runtime.heap.internal(handle, 0).and_then(|v| v.as_number()));
    let Some(index) = index else {
        return fallback;
    };
    // A negative index names a built-in. Encoded in the sign rather than in a second slot or
    // a reserved range: the two tables are disjoint by construction, and there is no boundary
    // to pick wrongly.
    if index < 0.0 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked negative, and the count of built-ins is tiny"
        )]
        let native = (-index - 1.0) as usize;
        if let Some((_, function)) = NATIVES.iter().chain(GLOBAL_NATIVES.iter()).nth(native) {
            return *function as *const u8;
        }
        let offset = NATIVES.len() + GLOBAL_NATIVES.len();
        if let Some((_, _, function)) = NAMESPACE_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + NAMESPACE_NATIVES.len();
        if let Some(function) = ANONYMOUS_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + ANONYMOUS_NATIVES.len();
        if let Some((_, function)) = FUNCTION_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + FUNCTION_NATIVES.len();
        if let Some((_, function)) = STRING_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + STRING_NATIVES.len();
        if let Some((_, function)) = REGEXP_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + REGEXP_NATIVES.len();
        if let Some((_, function)) = DATE_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + DATE_NATIVES.len();
        if let Some((_, function)) = OBJECT_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + OBJECT_NATIVES.len();
        // **In the same order the prototypes are built**, because the two share one index
        // space — a table inserted here and not there resolves every later method to its
        // neighbour, which is a call to the wrong function rather than an error.
        if let Some((_, function)) = PROMISE_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + PROMISE_NATIVES.len();
        if let Some((_, function)) = MAP_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + MAP_NATIVES.len();
        if let Some((_, function)) = SET_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + SET_NATIVES.len();
        if let Some((_, function)) = SYMBOL_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + SYMBOL_NATIVES.len();
        if let Some((_, function)) = ARRAY_ITERATOR_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + ARRAY_ITERATOR_NATIVES.len();
        if let Some((_, function)) = NUMBER_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + NUMBER_NATIVES.len();
        if let Some((_, function)) = BOOLEAN_NATIVES.get(native.wrapping_sub(offset)) {
            return *function as *const u8;
        }
        let offset = offset + BOOLEAN_NATIVES.len();
        // **Last, and a plain list**: `TYPED_NATIVES` is addressed by the `AB_*`/`TA_*` constants,
        // not by name, so it holds bare functions rather than `(name, function)` pairs.
        return TYPED_NATIVES
            .get(native.wrapping_sub(offset))
            .map_or(fallback, |function| *function as *const u8);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a function index is a small non-negative integer by construction"
    )]
    let index = index as usize;
    // SAFETY: written once by `crisol_register_functions` before any compiled code runs.
    let functions = unsafe { FUNCTIONS };
    functions.get(index).copied().unwrap_or(fallback)
}

/// How JavaScript spells a number.
///
/// Shared by printing and by property keys, because `a[1]` is `a["1"]` — the two have to agree
/// on the spelling or they name different properties. `NaN`, the infinities and the
/// no-decimal-point rule for whole numbers are all observable, and a second copy of them would
/// eventually disagree with this one.
fn number_text(number: f64) -> String {
    if number.is_nan() {
        return "NaN".to_owned();
    }
    if number.is_infinite() {
        return if number > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .to_owned();
    }
    // Whole numbers print without a decimal point, as JavaScript does — `1`, not `1.0`.
    if number.fract() == 0.0 && number.abs() < 1e21 {
        return format!("{number:.0}");
    }
    format!("{number}")
}

/// The array index a value names, if it names one.
///
/// `a[0]` and `a["0"]` are the same access in JavaScript, so a string that reads as a
/// non-negative integer counts. Anything else — `a[-1]`, `a[1.5]`, `a["x"]` — is an ordinary
/// property, which is why this returns `None` rather than rounding.
fn as_index(key: Value) -> Option<usize> {
    let number = key.as_number()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked non-negative and integral just above"
    )]
    let index = number as usize;
    Some(index)
}

/// How a computed key reads as a property name.
///
/// `a[0]` on a non-array is `a["0"]`, so a number becomes its own decimal spelling. Written
/// through `Value`'s own formatting rather than Rust's, because `1e21` and `-0` do not print
/// the same in the two languages and a property name that differs by a character is a
/// different property.
fn key_of(key: Value) -> Option<PropertyKey> {
    // **A string key is the ordinary case.** This returned `None` for one until strings
    // existed, which made `o["a"]` silently do nothing — a computed read answered `undefined`
    // and a computed write was discarded, neither saying a word.
    if key.kind() == crisol_value::Kind::String {
        return text_of(key.to_bits()).map(|text| PropertyKey::new(&text));
    }
    key.as_number().map_or_else(
        || match key.kind() {
            crisol_value::Kind::Undefined => Some(PropertyKey::new("undefined")),
            crisol_value::Kind::Null => Some(PropertyKey::new("null")),
            crisol_value::Kind::Boolean => {
                Some(PropertyKey::new(if key.as_boolean().unwrap_or(false) {
                    "true"
                } else {
                    "false"
                }))
            }
            // **A symbol names a property by identity, not by spelling.** `None` here read as
            // a missing property, which is why `obj[Symbol.iterator]` could neither be set
            // nor found and every iterator protocol was out of reach (D-149).
            // **Nothing is read and nothing is recorded here.** This runs on every computed
            // access, and the two obvious conveniences are both real costs: reading the
            // description allocates a `String` for something only `Debug` ever prints, and
            // rooting the symbol on a *read* grows a permanent set for a key that may never
            // reach a shape. The root is taken where a shape actually gains the key — see
            // `symbol_property_store`.
            crisol_value::Kind::Symbol => key
                .as_address()
                .map(|address| PropertyKey::symbol(address, "")),
            _ => None,
        },
        |number| Some(PropertyKey::new(&number_text(number))),
    )
}

/// Allocates `[…]` with `length` elements, all `undefined`.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_create_array(length: u64) -> u64 {
    let length = usize::try_from(length).unwrap_or(0);
    with_runtime(|runtime| {
        let shape = runtime.shapes.borrow().root();
        let scope = runtime.heap.scope();
        let array = scope.alloc(shape, 0);
        runtime.heap.make_array(array.handle(), length);
        if let Some(prototype) = ARRAY_PROTOTYPE.with(std::cell::Cell::get) {
            runtime.heap.set_prototype(array.handle(), Some(prototype));
        }
        array.to_value().to_bits()
    })
}

/// `object[key]`.
///
/// An index on an array reads an element; anything else is a property, including on an array —
/// `a.length` and `a["length"]` are the same thing and neither is an element.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_computed_load(object: u64, key: u64) -> u64 {
    let Some(handle) = handle_of(object) else {
        // **A primitive has no elements but may have methods.** `n["toString"]` reaches the
        // same prototype `n.toString` does, so the named path handles it — this one only has
        // to stop treating a non-object as nothing at all.
        let Some(name) = key_of(Value::from_bits(key)) else {
            return nullish_access(object);
        };
        let text = name.as_str().to_owned();
        // SAFETY: `text` is a live Rust string, so its pointer and length describe UTF-8.
        return unsafe { crisol_property_load(object, text.as_ptr(), text.len() as u64) };
    };
    let key = Value::from_bits(key);

    let element =
        with_runtime(|runtime| as_index(key).and_then(|index| runtime.heap.element(handle, index)));
    if let Some(value) = element {
        return value.to_bits();
    }
    let Some(name) = key_of(key) else {
        return Value::UNDEFINED.to_bits();
    };
    // **A symbol is not its description**, so it cannot go through the text path below.
    if name.is_symbol() {
        return symbol_property_load(object, &name);
    }
    let text = name.as_str().to_owned();
    // SAFETY: `text` is a live Rust string, so its pointer and length describe readable UTF-8.
    unsafe { crisol_property_load(object, text.as_ptr(), text.len() as u64) }
}

/// `object[key] = value`.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_computed_store(object: u64, key: u64, value: u64) -> u64 {
    if handle_of(object).is_none() {
        return nullish_access(object);
    }
    let key = Value::from_bits(key);

    if let Some(index) = as_index(key)
        && store_element(object, index, value)
    {
        return Value::UNDEFINED.to_bits();
    }
    let Some(name) = key_of(key) else {
        return Value::UNDEFINED.to_bits();
    };
    if name.is_symbol() {
        return symbol_property_store(object, &name, value);
    }
    let text = name.as_str().to_owned();
    // SAFETY: as above.
    unsafe { crisol_property_store(object, text.as_ptr(), text.len() as u64, value) }
}

/// `left === right`, on values of any type.
///
/// **Three things make this not a bit comparison**, which is why D-53 had the backend refuse it
/// rather than guess:
///
/// - `NaN === NaN` is **false**, and two `NaN`s have identical bits.
/// - `+0 === -0` is **true**, and their bits differ.
/// - Different types are never equal, whatever their payloads.
///
/// Comparing as numbers when both are numbers gets the first two right for free, because IEEE
/// equality already says exactly that. Everything else is identity, which is correct for
/// `undefined`, `null`, booleans and objects — and will need revisiting for strings, where two
/// distinct objects with the same characters are `===` and are not the same handle.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_strict_equal(left: u64, right: u64) -> u64 {
    let left = Value::from_bits(left);
    let right = Value::from_bits(right);
    let equal = match (left.as_number(), right.as_number()) {
        (Some(a), Some(b)) => a == b,
        // **Strings compare by their characters, not by identity.** They are primitives, so
        // `"a" === "a"` is true however many separate cells the two came from — and constants
        // do allocate a fresh one per evaluation today.
        (None, None)
            if left.kind() == crisol_value::Kind::String
                && right.kind() == crisol_value::Kind::String =>
        {
            text_of(left.to_bits())
                .zip(text_of(right.to_bits()))
                .is_some_and(|(a, b)| a == b)
        }
        (None, None) => left.kind() == right.kind() && left.to_bits() == right.to_bits(),
        _ => false,
    };
    if equal { Value::TRUE } else { Value::FALSE }.to_bits()
}

thread_local! {
    /// The generator behind `Math.random`. Zero means "not seeded yet".
    static RANDOM_STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// The value a `throw` is carrying, while it propagates.
    ///
    /// Held here rather than returned alongside the signal because a call returns one word.
    /// Exactly one throw is in flight at a time: propagation is immediate and synchronous, so
    /// a second cannot begin before the first is caught or reaches the top.
    static PENDING: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The value a throw is carrying, as a root the collector must trace.
///
/// A thrown object is reachable from nowhere else while it propagates — the frame that made it
/// has returned, and no handler holds it yet. Without this, throwing an object and catching it
/// after any allocation would catch a freed one.
fn pending_root() -> Option<GcRef> {
    let bits = PENDING.with(std::cell::Cell::get);
    Value::from_bits(bits).as_address().map(GcRef::from_address)
}

/// `throw value` — records it and answers the signal every caller checks for.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_throw(value: u64) -> u64 {
    PENDING.with(|pending| pending.set(value));
    Value::EXCEPTION.to_bits()
}

/// The value being thrown, for a `catch` to bind.
///
/// Clears it: the throw is over once a handler has it, and leaving it set would keep a caught
/// object alive for as long as the program runs.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_pending_exception() -> u64 {
    PENDING.with(|pending| {
        let value = pending.get();
        pending.set(Value::UNDEFINED.to_bits());
        value
    })
}

/// `left instanceof right`.
///
/// Walks `left`'s prototype chain looking for `right.prototype`. A non-object on the left is
/// always `false` — `1 instanceof Object` is `false`, not an error — while a non-callable on
/// the right is a `TypeError`, which needs a throw this cannot raise from here, so it answers
/// `false` too and the difference is recorded rather than pretended away.
///
/// Bounded like every other chain walk: a cycle is illegal and an unbounded loop inside an
/// operator is worse than a wrong answer.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_instanceof(left: u64, right: u64) -> u64 {
    let Some(object) = handle_of(left) else {
        return Value::FALSE.to_bits();
    };
    let Some(constructor) = handle_of(right) else {
        return Value::FALSE.to_bits();
    };

    let key = PropertyKey::new("prototype");
    let found = with_runtime(|runtime| {
        let shape = runtime.heap.shape_of(constructor)?;
        let slot = runtime.shapes.borrow().lookup(shape, &key)?;
        runtime.heap.get(constructor, slot.index())
    });
    let Some(prototype) = found
        .and_then(|value| value.as_address())
        .map(GcRef::from_address)
    else {
        return Value::FALSE.to_bits();
    };

    with_runtime(|runtime| {
        let mut current = runtime.heap.prototype_of(object);
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let Some(link) = current else { break };
            if link == prototype {
                return Value::TRUE.to_bits();
            }
            current = runtime.heap.prototype_of(link);
        }
        Value::FALSE.to_bits()
    })
}

/// The characters of a string value.
fn text_of(bits: u64) -> Option<String> {
    let value = Value::from_bits(bits);
    if value.kind() != crisol_value::Kind::String {
        return None;
    }
    let handle = value.as_address().map(GcRef::from_address)?;
    with_runtime(|runtime| runtime.heap.with_text(handle, ToOwned::to_owned))
}

/// Allocates a string cell holding `text`.
fn new_string(text: &str) -> u64 {
    with_runtime(|runtime| {
        let shape = runtime.shapes.borrow().root();
        let scope = runtime.heap.scope();
        let cell = scope.alloc(shape, 0);
        runtime.heap.make_string(cell.handle(), text);
        if let Some(prototype) = STRING_PROTOTYPE.with(std::cell::Cell::get) {
            runtime.heap.set_prototype(cell.handle(), Some(prototype));
        }
        // The same 48 bits the handle packs into, re-tagged as a string rather than an object.
        // `to_value` is where that packing lives, so this cannot drift from it.
        cell.handle().to_value().as_address().map_or_else(
            || Value::UNDEFINED.to_bits(),
            |address| Value::string(address).to_bits(),
        )
    })
}

/// A string literal, from bytes the object file carries.
///
/// A fresh cell per evaluation, which is correct because strings are primitives and `===`
/// compares characters — and wasteful, because `"a"` in a loop allocates every time. Interning
/// constants is the obvious fix and is deliberately not done yet: it wants a table that is a
/// permanent GC root, and correctness first.
///
/// # Safety
///
/// `text` must point to `length` readable bytes of UTF-8.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn crisol_create_string(text: *const u8, length: u64) -> u64 {
    // SAFETY: the caller promises `length` readable UTF-8 bytes at `text`.
    let Some(text) = (unsafe { key_text(text, length) }) else {
        return Value::UNDEFINED.to_bits();
    };
    new_string(&text)
}

/// How a value reads as text, for `+` and for printing.
///
/// **An object is asked**, through `toString` and then `valueOf`. Reading `[object Object]` off
/// every object without asking made `String([1, 2])` that string instead of `"1,2"`, and
/// `"" + [1]` likewise — the array had a perfectly good `toString` that nothing called.
fn to_text(bits: u64) -> Option<String> {
    let value = Value::from_bits(bits);
    if value.kind() == crisol_value::Kind::Object && handle_of(bits).is_some() && !is_callable(bits)
    {
        let asked = to_primitive_text(bits);
        if asked != bits {
            return to_text(asked);
        }
    }
    match value.kind() {
        crisol_value::Kind::String => text_of(bits),
        crisol_value::Kind::Number => value.as_number().map(number_text),
        crisol_value::Kind::Undefined => Some("undefined".to_owned()),
        crisol_value::Kind::Null => Some("null".to_owned()),
        crisol_value::Kind::Boolean => {
            Some(if value.as_boolean()? { "true" } else { "false" }.to_owned())
        }
        // An object needs `ToPrimitive`, which calls user code. Recorded rather than guessed at
        // with something that would read plausibly.
        _ => None,
    }
}

/// `-value`, after `ToNumber`.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_negate(value: u64) -> u64 {
    from_number(-to_number(value))
}

/// `+value` — `ToNumber`, which is why `+"1"` is `1`.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_to_number(value: u64) -> u64 {
    from_number(to_number(value))
}

/// `!value` — `ToBoolean` then inverted, so it never fails.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_not(value: u64) -> u64 {
    if is_truthy(Value::from_bits(value)) {
        Value::FALSE
    } else {
        Value::TRUE
    }
    .to_bits()
}

/// `typeof value`.
///
/// **`typeof null` is `"object"`**, which is a bug in the language old enough to be part of it —
/// and `typeof` a function is `"function"` although a function is an object, so neither answer
/// can be read off the value's kind alone.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_typeof(value: u64) -> u64 {
    let value = Value::from_bits(value);
    let name = match value.kind() {
        crisol_value::Kind::Undefined => "undefined",
        crisol_value::Kind::Null => "object",
        crisol_value::Kind::Boolean => "boolean",
        crisol_value::Kind::Number => "number",
        crisol_value::Kind::String => "string",
        crisol_value::Kind::Symbol => "symbol",
        crisol_value::Kind::Object => {
            if is_callable(value.to_bits()) {
                "function"
            } else {
                "object"
            }
        }
    };
    new_string(name)
}

/// Reports the throw that reached the top of the program.
///
/// Called by the entry point when `crisol_program` answers with the exception signal rather
/// than a value. Without it an uncaught `throw` would exit successfully and print the signal as
/// `undefined` — which is how a test suite that reports failure *by throwing* would score every
/// failing case as a pass.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_report_uncaught() {
    let thrown = crisol_pending_exception();
    // **`describe_error` first.** This was `to_text(...).or_else(describe_error)`, which was
    // right only while `to_text` failed on objects. Once it learned to ask an object for its
    // text (D-137) it started succeeding with whatever `Object.prototype.toString` returns, so
    // every thrown error described itself as `[object Object]` and `describe_error` never ran.
    // 581 of 1061 test262 failures reported that and nothing else.
    //
    // The order is not arbitrary: `name` and `message` are what an error carries, and reading
    // them beats calling a `toString` that most errors inherit rather than define.
    let described = describe_error(thrown).or_else(|| to_text(thrown));
    eprintln!("uncaught: {}", described.as_deref().unwrap_or("an object"));
}

/// How a thrown *object* describes itself.
///
/// An `Error` carries its explanation in `message` and its kind in `name`, so an object without
/// them is genuinely opaque and anything else is readable. This is not `ToPrimitive`: it reads
/// two known properties rather than calling user code, which is the difference between a
/// diagnostic and running more of the program that just failed.
fn describe_error(thrown: u64) -> Option<String> {
    let read = |name: &str| {
        let key = name.to_owned();
        // SAFETY: `key` is a live Rust string, so its pointer and length describe UTF-8.
        let bits = unsafe { crisol_property_load(thrown, key.as_ptr(), key.len() as u64) };
        // An **absent** property reads as `undefined`, and `to_text` would turn that into the
        // characters "undefined" — so every error without a `name` described itself as one
        // called `undefined`. Requiring a string is what distinguishes missing from empty.
        (Value::from_bits(bits).kind() == crisol_value::Kind::String)
            .then(|| to_text(bits))
            .flatten()
            // **Empty counts as absent too.** `Error.prototype.message` is the empty string,
            // which every error without one of its own now inherits — kept, it described a
            // bare `new TypeError()` as `"TypeError: "` with nothing after the colon.
            .filter(|text| !text.is_empty())
    };
    match (read("name"), read("message")) {
        (Some(name), Some(message)) => Some(format!("{name}: {message}")),
        (None, Some(message)) => Some(message),
        (Some(name), None) => Some(name),
        (None, None) => None,
    }
}

/// `ToBoolean` — whether a value takes the true branch.
///
/// **A branch cannot be a bit comparison against boxed `true`.** Every truthy value that is not
/// literally `true` — a non-empty string, a number, any object — would take the false path, so
/// `if (name)` and `x || y` would be wrong for everything except booleans. That is how
/// `this.message = message || ""` came to assign `""` whatever it was given.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_truthy(value: u64) -> u64 {
    if is_truthy(Value::from_bits(value)) {
        Value::TRUE
    } else {
        Value::FALSE
    }
    .to_bits()
}

/// Reads a global, or raises a `ReferenceError` if there is none.
///
/// **A missing global is an error, not `undefined`.** `foo` on its own throws where `foo.bar`
/// would have quietly produced nothing — and reading it as `undefined` is what let a test
/// comparing against a builtin report a wrong *value* instead of a missing one.
///
/// # Safety
///
/// `name` must point to `length` readable bytes of UTF-8.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn crisol_global_load(name: *const u8, length: u64) -> u64 {
    // SAFETY: the caller promises `length` readable UTF-8 bytes at `name`.
    let Some(text) = (unsafe { key_text(name, length) }) else {
        return Value::UNDEFINED.to_bits();
    };
    let key = PropertyKey::new(&text);
    // **Inside `with_runtime`, and that is load-bearing.** `GLOBALS` is filled while the
    // runtime is constructed, and the runtime is constructed lazily on first use — so reading
    // the cell first finds `None` whenever a global is the first thing a program touches,
    // which it usually is. Every global then read as absent.
    let found = with_runtime(|runtime| {
        let globals = GLOBALS.with(std::cell::Cell::get)?;
        let shape = runtime.heap.shape_of(globals)?;
        let slot = runtime.shapes.borrow().lookup(shape, &key)?;
        runtime.heap.get(globals, slot.index())
    });
    match found {
        Some(value) => value.to_bits(),
        None => raise(&format!("{text} is not defined"), "ReferenceError"),
    }
}

/// Throws a fresh error of `kind` carrying `message`.
fn raise(message: &str, kind: &str) -> u64 {
    let error = crisol_create_object();
    let Some(handle) = handle_of(error) else {
        return crisol_throw(Value::UNDEFINED.to_bits());
    };
    with_rooted(&[error], || {
        // **The prototype is what makes it an error rather than an object with two fields.**
        // `assert.throws` in test262 compares `thrown.constructor` against the constructor it
        // expected, and `catch (e) { e instanceof TypeError }` is how a program does the
        // same. Without the link both answer `Object`, so every one of those checks failed on
        // an engine that had thrown exactly the right thing.
        if let Some(prototype) = error_prototype(kind) {
            with_runtime(|runtime| runtime.heap.set_prototype(handle, Some(prototype)));
        }
        with_runtime(|runtime| {
            runtime.define_hidden(handle, ERROR_DATA, Value::number(1.0));
        });
        // Stored one at a time. Creating both and then storing them leaves the first reachable
        // only from a Rust local while the second allocates — and under stress that allocation
        // collects it, which is how the message came back unreadable.
        let text = new_string(message);
        with_runtime(|runtime| runtime.define_hidden(handle, "message", Value::from_bits(text)));
    });
    crisol_throw(error)
}

/// The prototype an error of kind `kind` inherits from.
///
/// Read off the global constructor rather than from a cell of its own, so a program that
/// replaces `TypeError.prototype` sees its replacement on what the engine throws — which is
/// wrong for a real internal operation and right for the only alternative available, which is
/// six more thread-local cells kept in step by hand.
fn error_prototype(kind: &str) -> Option<GcRef> {
    with_runtime(|runtime| {
        let globals = GLOBALS.with(std::cell::Cell::get)?;
        let constructor = runtime.global_object(globals, kind)?;
        let key = PropertyKey::new("prototype");
        let shape = runtime.heap.shape_of(constructor)?;
        let slot = runtime.shapes.borrow().lookup(shape, &key)?;
        runtime
            .heap
            .get(constructor, slot.index())
            .and_then(|value| value.as_address())
            .map(GcRef::from_address)
    })
}

/// `delete object[key]`.
///
/// **Answers `true` for a property that was never there.** `delete` asks whether the property
/// is gone afterwards, not whether it removed anything — so only a non-configurable property
/// answers `false`.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_delete(object: u64, key: u64) -> u64 {
    let Some(handle) = handle_of(object) else {
        return nullish_access(object);
    };
    if let Some((target, handler)) = proxy_parts(object) {
        let Some(name) = key_of(Value::from_bits(key)) else {
            return Value::TRUE.to_bits();
        };
        return proxy_delete(target, handler, &name);
    }
    let key_value = Value::from_bits(key);

    // An element is removed by shortening the array when it is the last one, and otherwise left
    // as `undefined` — a hole and an `undefined` element differ (D-64) and nothing here can say
    // which it is yet. An index reaches here spelled as text too — `delete o["0"]`, which is
    // exactly how the harness's `isConfigurable` deletes — so a string that is the canonical
    // spelling of one names the element, not a slot (see `crisol_property_store`). Without this
    // the delete silently found no slot, answered `true`, and left the element in place, so a
    // configurable element read back as non-configurable.
    let element_index = as_index(key_value).or_else(|| {
        (key_value.kind() == crisol_value::Kind::String)
            .then(|| text_of(key_value.to_bits()))
            .flatten()
            .and_then(|text| canonical_index(&text))
    });
    if let Some(index) = element_index {
        // Read before the borrow below: this is a hidden property, so asking is a property
        // load, and a property load enters the runtime itself.
        let configurable = element_rule(object, index).configurable;
        let handled = with_runtime(|runtime| {
            let count = runtime.heap.element_count(handle)?;
            if index >= count {
                return Some(true);
            }
            if !configurable {
                // **Non-configurable answers `false`**, as a sealed named property does.
                return Some(false);
            }
            if index + 1 == count {
                runtime.heap.truncate_elements(handle, index);
            } else {
                runtime.heap.set_element(handle, index, Value::UNDEFINED);
            }
            Some(true)
        });
        if let Some(gone) = handled {
            return boolean(gone).to_bits();
        }
    }

    let Some(name) = key_of(key_value) else {
        return Value::TRUE.to_bits();
    };
    // A property nothing stores can still be non-configurable — a string's characters are —
    // and there is no slot for the walk below to read that off. A symbol never names one of
    // those, and asking by its description would answer about a different property.
    if !name.is_symbol()
        && own_property(object, name.as_str()).is_none()
        && derived_own_property(object, name.as_str())
            .is_some_and(|(_, attributes)| !attributes.configurable)
    {
        return Value::FALSE.to_bits();
    }
    with_runtime(|runtime| {
        let Some(shape) = runtime.heap.shape_of(handle) else {
            return Value::TRUE.to_bits();
        };
        let Some(slot) = runtime.shapes.borrow().lookup(shape, &name) else {
            // Never there, so it is gone.
            return Value::TRUE.to_bits();
        };
        if runtime.heap.is_deleted(handle, slot.index()) {
            return Value::TRUE.to_bits();
        }
        if !runtime
            .heap
            .attributes_of(handle, slot.index())
            .configurable
        {
            // **Non-configurable answers `false`** rather than throwing, outside strict mode.
            return Value::FALSE.to_bits();
        }
        runtime.heap.set_deleted(handle, slot.index(), true);
        Value::TRUE.to_bits()
    })
}

/// Every name a `for-in` over `object` visits, as an array of strings.
///
/// **Inherited enumerable properties are visited too**, which is what separates `for-in` from
/// `Object.keys` — and the reason it walks the prototype chain rather than reading one object.
/// A name found on an object shadows the same name further up, so each is visited once and at
/// the first place it appears.
///
/// A non-object visits nothing, which is not an error: `for (k in undefined)` runs zero times
/// rather than throwing.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_enumerate(object: u64) -> u64 {
    with_rooted(&[object], || {
        let mut names: Vec<String> = Vec::new();
        // **Every own name shadows, not only the enumerable ones.** A non-enumerable property
        // hides an inherited one of the same name — the loop must not visit it — and tracking
        // only what was emitted meant the inherited one showed through the thing hiding it.
        let mut shadowed: Vec<String> = Vec::new();
        let mut current = object;
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let Some(handle) = handle_of(current) else {
                break;
            };
            let enumerable = enumerable_keys(current);
            for name in own_keys(current) {
                if shadowed.contains(&name) {
                    continue;
                }
                if enumerable.contains(&name) {
                    names.push(name.clone());
                }
                shadowed.push(name);
            }
            let next = with_runtime(|runtime| runtime.heap.prototype_of(handle));
            match next {
                Some(parent) => current = parent.to_value().to_bits(),
                None => break,
            }
        }
        names_as_array(&names)
    })
}

/// What a `for-of` over `value` walks, as something indexable.
///
/// **An array is returned as itself, not copied.** The loop re-reads `length` each step, so a
/// `push` inside the body is seen — which is what the array iterator does, and is why
/// `for (const x of a) a.push(x)` does not terminate here any more than in a real engine.
/// Copying would have made it terminate, which is a quieter answer and the wrong one.
///
/// A string becomes an array of its **code points**, not its code units: `for (const c of "😀")`
/// runs once where `"😀".length` is 2. The snapshot is indistinguishable from live indexing
/// because a string cannot change.
///
/// Anything else raises a `TypeError`. That is the error the iterator protocol would raise for
/// a non-iterable, reached for a different reason: there is no `Symbol`, so there is no
/// `Symbol.iterator` to look up and a user-defined iterable cannot be recognised at all.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_iterate(value: u64) -> u64 {
    // **An *own* `Symbol.iterator` wins**, and otherwise a shape the engine recognises takes
    // the fast path below. The order matters and it is not the obvious one: draining the
    // protocol eagerly would make `for-of` over an array walk a snapshot, and the loop is
    // specified to re-read the length each step — `for (x of a) a.pop()` visits two of three
    // elements, not three.
    //
    // The cost is that replacing `Array.prototype[Symbol.iterator]` wholesale is not obeyed
    // for arrays; replacing it on the array itself is. Closing that means giving `for-of` an
    // iterator object to step rather than something to walk by index (D-194).
    if iterator_key().is_some_and(|key| symbol_own_slot(value, &key).is_some())
        && let Some(items) = iterate_by_protocol(value)
    {
        return items;
    }
    if elements_of(value).is_some() {
        return value;
    }
    if Value::from_bits(value).kind() == crisol_value::Kind::String {
        let Some(text) = text_of(value) else {
            return raise("cannot iterate this value", "TypeError");
        };
        return with_rooted(&[value], || {
            let points: Vec<String> = text.chars().map(|point| point.to_string()).collect();
            names_as_array(&points)
        });
    }
    if let Some(items) = iterate_by_protocol(value) {
        return items;
    }
    raise("value is not iterable", "TypeError")
}

/// What internal slot zero holds on a proxy.
///
/// **A marker rather than a hidden property**, because the test runs on every property
/// access. A hidden property is a shape lookup; an internal slot is a `Vec` index inside a
/// borrow the load path already holds, so a program with no proxies in it pays one integer
/// compare. That is the difference between a feature nobody uses being free and being a tax.
///
/// `null` because no callable stores it: every function puts a number there (its index or its
/// code pointer), which is what [`is_callable`] now checks rather than mere presence.
const PROXY_MARKER: Value = Value::NULL;

/// Internal slot holding a proxy's target.
const PROXY_TARGET_SLOT: u32 = 1;

/// Internal slot holding a proxy's handler, or `null` once revoked.
const PROXY_HANDLER_SLOT: u32 = 2;

/// A proxy's target and handler, or `None` if this is not a proxy.
///
/// The one integer compare every property access pays. Ordered so the common answer costs a
/// single slot read.
fn proxy_parts(object: u64) -> Option<(u64, u64)> {
    let handle = handle_of(object)?;
    with_runtime(|runtime| {
        if runtime.heap.internal(handle, 0)? != PROXY_MARKER {
            return None;
        }
        let target = runtime.heap.internal(handle, PROXY_TARGET_SLOT)?;
        let handler = runtime.heap.internal(handle, PROXY_HANDLER_SLOT)?;
        Some((target.to_bits(), handler.to_bits()))
    })
}

/// A heap object seen through the invariant checks' eyes.
struct HeapTarget(u64);

impl crisol_builtins::Target for HeapTarget {
    fn own_property(&self, key: &PropertyKey) -> Option<crisol_builtins::Property> {
        // Symbol keys reach the shape directly; a string key can also name an element or a
        // character, which `derived_own_property` answers for.
        let (value, attributes) = if key.is_symbol() {
            let slot = symbol_own_slot(self.0, key)?;
            let handle = handle_of(self.0)?;
            with_runtime(|runtime| {
                Some((
                    runtime.heap.get(handle, slot)?,
                    runtime.heap.attributes_of(handle, slot),
                ))
            })?
        } else {
            let name = key.as_str();
            match own_property(self.0, name) {
                Some((slot, value)) => {
                    let handle = handle_of(self.0)?;
                    (
                        value,
                        with_runtime(|runtime| runtime.heap.attributes_of(handle, slot)),
                    )
                }
                None => {
                    let (bits, attributes) = derived_own_property(self.0, name)?;
                    (Value::from_bits(bits), attributes)
                }
            }
        };
        if attributes.accessor {
            let (get, set) = elements_of(value.to_bits()).map_or(
                (Value::UNDEFINED, Value::UNDEFINED),
                |(pair, _)| {
                    (
                        Value::from_bits(element_at(pair, 0)),
                        Value::from_bits(element_at(pair, 1)),
                    )
                },
            );
            return Some(crisol_builtins::Property::Accessor {
                get,
                set,
                enumerable: attributes.enumerable,
                configurable: attributes.configurable,
            });
        }
        Some(crisol_builtins::Property::Data {
            value,
            writable: attributes.writable,
            enumerable: attributes.enumerable,
            configurable: attributes.configurable,
        })
    }

    fn is_extensible(&self) -> bool {
        is_extensible(self.0)
    }
}

/// The trap `name` on `handler`, or `None` when the handler does not define one.
///
/// **Absent means forward to the target**, which is what makes a handler with one trap a
/// pass-through for everything else.
fn proxy_trap(handler: u64, trap: crisol_builtins::Trap) -> Option<u64> {
    let method = property_of(handler, trap.name());
    is_callable(method).then_some(method)
}

/// Checks a trap's answer against the target, turning a violation into a thrown `TypeError`.
fn proxy_checked<R>(
    outcome: Result<R, crisol_builtins::ProxyError>,
    ok: impl FnOnce(R) -> u64,
) -> u64 {
    match outcome {
        Ok(value) => ok(value),
        // **A violated invariant is the proxy's fault, not the program's**, and the message
        // says which trap lied — without that a `TypeError` from deep inside a property read
        // is unattributable.
        Err(error) => raise(&error.to_string(), "TypeError"),
    }
}

/// Builds the checker for `target`, honouring revocation.
fn proxy_of(target: u64, handler: u64) -> crisol_builtins::Proxy<HeapTarget> {
    let mut proxy = crisol_builtins::Proxy::new(HeapTarget(target));
    if Value::from_bits(handler).is_null() {
        proxy.revoke();
    }
    proxy
}

/// A key as the value a trap is handed.
fn key_value(key: &PropertyKey) -> u64 {
    key.symbol_address().map_or_else(
        || new_string(key.as_str()),
        |address| Value::symbol(address).to_bits(),
    )
}

/// Reads `key` from `object` by whichever path its kind needs.
fn load_with_key(object: u64, key: &PropertyKey) -> u64 {
    if key.is_symbol() {
        return symbol_property_load(object, key);
    }
    let text = key.as_str().to_owned();
    // SAFETY: `text` is a live Rust string.
    unsafe { crisol_property_load(object, text.as_ptr(), text.len() as u64) }
}

/// Writes `key` on `object` by whichever path its kind needs.
fn store_with_key(object: u64, key: &PropertyKey, value: u64) -> u64 {
    if key.is_symbol() {
        return symbol_property_store(object, key, value);
    }
    let text = key.as_str().to_owned();
    // SAFETY: `text` is a live Rust string.
    unsafe { crisol_property_store(object, text.as_ptr(), text.len() as u64, value) }
}

/// `proxy[key]`, through the `get` trap or straight to the target.
fn proxy_load(receiver: u64, target: u64, handler: u64, key: &PropertyKey) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::Get) else {
        return load_with_key(target, key);
    };
    with_rooted(&[receiver, target, handler, trap], || {
        let name = key_value(key);
        let reported = with_rooted(&[name], || {
            call_value(trap, handler, &[target, name, receiver])
        });
        if Value::from_bits(reported).is_exception() {
            return reported;
        }
        proxy_checked(
            proxy.checked_get(key, Value::from_bits(reported)),
            |value| value.to_bits(),
        )
    })
}

/// `proxy[key] = value`, through the `set` trap or straight to the target.
fn proxy_store(receiver: u64, target: u64, handler: u64, key: &PropertyKey, value: u64) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::Set) else {
        return store_with_key(target, key, value);
    };
    with_rooted(&[receiver, target, handler, trap, value], || {
        let name = key_value(key);
        let reported = with_rooted(&[name], || {
            call_value(trap, handler, &[target, name, value, receiver])
        });
        if Value::from_bits(reported).is_exception() {
            return reported;
        }
        let accepted = is_truthy(Value::from_bits(reported));
        proxy_checked(
            proxy.checked_set(key, Value::from_bits(value), accepted),
            |_| Value::UNDEFINED.to_bits(),
        )
    })
}

/// `key in proxy`, through the `has` trap or straight to the target.
fn proxy_has(target: u64, handler: u64, key: &PropertyKey) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::Has) else {
        let name = key_value(key);
        return with_rooted(&[target, name], || crisol_in(name, target));
    };
    with_rooted(&[target, handler, trap], || {
        let name = key_value(key);
        let reported = with_rooted(&[name], || call_value(trap, handler, &[target, name]));
        if Value::from_bits(reported).is_exception() {
            return reported;
        }
        let present = is_truthy(Value::from_bits(reported));
        proxy_checked(proxy.checked_has(key, present), |answer| {
            boolean(answer).to_bits()
        })
    })
}

/// `delete proxy[key]`, through the `deleteProperty` trap or straight to the target.
fn proxy_delete(target: u64, handler: u64, key: &PropertyKey) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::DeleteProperty) else {
        let name = key_value(key);
        return with_rooted(&[target, name], || crisol_delete(target, name));
    };
    with_rooted(&[target, handler, trap], || {
        let name = key_value(key);
        let reported = with_rooted(&[name], || call_value(trap, handler, &[target, name]));
        if Value::from_bits(reported).is_exception() {
            return reported;
        }
        let removed = is_truthy(Value::from_bits(reported));
        proxy_checked(proxy.checked_delete(key, removed), |answer| {
            boolean(answer).to_bits()
        })
    })
}

/// `Object.keys(proxy)` and friends: the `ownKeys` trap, or the target's own keys.
///
/// Answers the **string** keys, which is what `own_keys` reports; a symbol the trap lists is
/// dropped here and picked up by `getOwnPropertySymbols`, exactly as for an ordinary object.
fn proxy_own_keys(target: u64, handler: u64) -> Option<Vec<String>> {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return Some(Vec::new());
    }
    let trap = proxy_trap(handler, crisol_builtins::Trap::OwnKeys)?;
    let reported = with_rooted(&[target, handler, trap], || {
        call_value(trap, handler, &[target])
    });
    if Value::from_bits(reported).is_exception() {
        return Some(Vec::new());
    }
    let length = indexed_length(reported).unwrap_or(0);
    let mut names = Vec::with_capacity(length);
    for index in 0..length {
        let entry = indexed_get(reported, index);
        if Value::from_bits(entry).kind() == crisol_value::Kind::String
            && let Some(text) = text_of(entry)
        {
            names.push(text);
        }
    }
    Some(names)
}

/// The `getOwnPropertyDescriptor` trap, or the target's descriptor.
fn proxy_descriptor(target: u64, handler: u64, key: u64) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::GetOwnPropertyDescriptor) else {
        let arguments = [target, key];
        return with_rooted(&arguments, || {
            object_own_descriptor(0, 0, 0, 2, arguments.as_ptr())
        });
    };
    with_rooted(&[target, handler, trap, key], || {
        call_value(trap, handler, &[target, key])
    })
}

/// The `defineProperty` trap, or the target's definition.
fn proxy_define(target: u64, handler: u64, key: u64, descriptor: u64) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::DefineProperty) else {
        let arguments = [target, key, descriptor];
        return with_rooted(&arguments, || {
            object_define_property(0, 0, 0, 3, arguments.as_ptr())
        });
    };
    with_rooted(&[target, handler, trap, key, descriptor], || {
        let reported = call_value(trap, handler, &[target, key, descriptor]);
        if Value::from_bits(reported).is_exception() {
            return reported;
        }
        // **A refusal is an error here**, unlike `Reflect.defineProperty` where it is the
        // answer: `Object.defineProperty` throws when the definition does not take, and a
        // proxy saying `false` is a definition that did not take.
        if is_truthy(Value::from_bits(reported)) {
            return target;
        }
        raise(
            "'defineProperty' on proxy: trap returned falsish",
            "TypeError",
        )
    })
}

/// The `getPrototypeOf` trap, or the target's prototype.
fn proxy_prototype(target: u64, handler: u64) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::GetPrototypeOf) else {
        let arguments = [target];
        return with_rooted(&arguments, || {
            object_get_prototype(0, 0, 0, 1, arguments.as_ptr())
        });
    };
    with_rooted(&[target, handler, trap], || {
        call_value(trap, handler, &[target])
    })
}

/// The `isExtensible` trap, checked against the target.
///
/// **This one cannot lie at all.** A proxy must report exactly what its target reports, so
/// the invariant is not a corner case but the whole rule — which is why the trap exists only
/// to observe.
fn proxy_is_extensible(target: u64, handler: u64) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::IsExtensible) else {
        return boolean(is_extensible(target)).to_bits();
    };
    with_rooted(&[target, handler, trap], || {
        let reported = call_value(trap, handler, &[target]);
        if Value::from_bits(reported).is_exception() {
            return reported;
        }
        let claimed = is_truthy(Value::from_bits(reported));
        proxy_checked(proxy.checked_is_extensible(claimed), |answer| {
            boolean(answer).to_bits()
        })
    })
}

/// The `preventExtensions` trap, or the target's.
fn proxy_prevent_extensions(target: u64, handler: u64) -> u64 {
    let proxy = proxy_of(target, handler);
    if proxy.is_revoked() {
        return raise(
            &crisol_builtins::ProxyError::Revoked.to_string(),
            "TypeError",
        );
    }
    let Some(trap) = proxy_trap(handler, crisol_builtins::Trap::PreventExtensions) else {
        prevent_extensions(target);
        return Value::TRUE.to_bits();
    };
    with_rooted(&[target, handler, trap], || {
        let reported = call_value(trap, handler, &[target]);
        if Value::from_bits(reported).is_exception() {
            return reported;
        }
        boolean(is_truthy(Value::from_bits(reported))).to_bits()
    })
}

/// `new Proxy(target, handler)`.
extern "C" fn make_proxy(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        // SAFETY: as above.
        let target = unsafe { argument(argc, argv, 0) };
        // SAFETY: as above.
        let handler = unsafe { argument(argc, argv, 1) };
        if Value::from_bits(target).kind() != crisol_value::Kind::Object
            || Value::from_bits(handler).kind() != crisol_value::Kind::Object
        {
            return raise("a proxy needs an object target and handler", "TypeError");
        }
        new_proxy_object(target, handler)
    })
}

/// Allocates the proxy cell: the marker, the target and the handler, in internal slots.
fn new_proxy_object(target: u64, handler: u64) -> u64 {
    with_rooted(&[target, handler], || {
        with_runtime(|runtime| {
            let shape = runtime.shapes.borrow().root();
            let scope = runtime.heap.scope();
            let proxy = scope.alloc_with_internals(shape, 0, 3);
            let handle = proxy.handle();
            runtime.heap.set_internal(handle, 0, PROXY_MARKER);
            runtime
                .heap
                .set_internal(handle, PROXY_TARGET_SLOT, Value::from_bits(target));
            runtime
                .heap
                .set_internal(handle, PROXY_HANDLER_SLOT, Value::from_bits(handler));
            // **No prototype of its own.** Every lookup goes to the trap or the target, so a
            // chain here would be a second answer nothing consults.
            runtime.heap.set_prototype(handle, None);
            proxy.to_value().to_bits()
        })
    })
}

/// `Proxy.revocable(target, handler)`.
extern "C" fn proxy_revocable(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let proxy = make_proxy(0, this_value, 0, argc, argv);
        if Value::from_bits(proxy).is_exception() {
            return proxy;
        }
        with_rooted(&[proxy], || {
            let result = crisol_create_object();
            with_rooted(&[result, proxy], || {
                let revoke = with_runtime(|runtime| {
                    runtime
                        .native_function(
                            NATIVES.len()
                                + GLOBAL_NATIVES.len()
                                + NAMESPACE_NATIVES.len()
                                + PROXY_REVOKE_CALL,
                        )
                        .to_value()
                        .to_bits()
                });
                with_rooted(&[revoke], || {
                    if let Some(handle) = handle_of(revoke) {
                        with_runtime(|runtime| {
                            runtime.define_hidden(
                                handle,
                                PROXY_REVOKE_TARGET,
                                Value::from_bits(proxy),
                            );
                        });
                    }
                });
                if let Some(into) = handle_of(result) {
                    with_runtime(|runtime| {
                        runtime.define(into, "proxy", Value::from_bits(proxy));
                        runtime.define(into, "revoke", Value::from_bits(revoke));
                    });
                }
            });
            result
        })
    })
}

/// Where a revoker keeps the proxy it revokes.
const PROXY_REVOKE_TARGET: &str = "__revokes";

/// The body a `revoke` function runs: clear the handler, which is what revocation *is*.
extern "C" fn proxy_revoke_call(
    closure: u64,
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    if let Some((_, proxy)) = own_property(closure, PROXY_REVOKE_TARGET)
        && let Some(handle) = handle_of(proxy.to_bits())
    {
        with_runtime(|runtime| {
            runtime
                .heap
                .set_internal(handle, PROXY_HANDLER_SLOT, Value::NULL);
        });
    }
    Value::UNDEFINED.to_bits()
}

/// Whether `value` is a promise this engine made.
fn is_promise(value: u64) -> bool {
    handle_of(value).is_some_and(|handle| {
        with_runtime(|runtime| runtime.heap.internal(handle, 0) == Some(PROMISE_MARKER))
    })
}

/// A promise's state and what it settled to.
fn promise_state(promise: u64) -> Option<(Settled, u64)> {
    let handle = handle_of(promise)?;
    with_runtime(|runtime| {
        if runtime.heap.internal(handle, 0)? != PROMISE_MARKER {
            return None;
        }
        // Compared rather than matched: a float *pattern* is a future-compatibility warning
        // in its own right, so the guard clippy objects to cannot simply become a literal
        // arm — the shape it wants is the one this cannot have.
        let marker = runtime
            .heap
            .internal(handle, PROMISE_STATE_SLOT)?
            .as_number()?;
        let state = if marker == 1.0 {
            Settled::Fulfilled
        } else if marker == 2.0 {
            Settled::Rejected
        } else {
            Settled::Pending
        };
        let value = runtime.heap.internal(handle, PROMISE_VALUE_SLOT)?;
        Some((state, value.to_bits()))
    })
}

/// A fresh pending promise.
///
/// **Its whole state lives in its own internal slots**, which the collector already traces —
/// so a promise nobody holds is freed with everything waiting on it, and there is no table of
/// every promise ever made to grow for the life of the program.
fn new_promise_object() -> u64 {
    with_runtime(|runtime| {
        let shape = runtime.shapes.borrow().root();
        let scope = runtime.heap.scope();
        let promise = scope.alloc_with_internals(shape, 0, 4);
        let handle = promise.handle();
        runtime.heap.set_internal(handle, 0, PROMISE_MARKER);
        runtime
            .heap
            .set_internal(handle, PROMISE_STATE_SLOT, Value::number(0.0));
        runtime
            .heap
            .set_internal(handle, PROMISE_VALUE_SLOT, Value::UNDEFINED);
        // The reaction list is left `undefined` and made on first use: most promises are
        // settled before anything waits on them, and those never allocate one.
        if let Some(prototype) = PROMISE_PROTOTYPE.with(std::cell::Cell::get) {
            runtime.heap.set_prototype(handle, Some(prototype));
        }
        promise.to_value().to_bits()
    })
}

/// Records a reaction on a pending promise.
fn promise_add_reaction(promise: u64, handler: u64, derived: u64, flags: u32) {
    let Some(handle) = handle_of(promise) else {
        return;
    };
    let existing = with_runtime(|runtime| runtime.heap.internal(handle, PROMISE_REACTIONS_SLOT));
    let list = match existing.map(|value| value.to_bits()) {
        Some(list) if elements_of(list).is_some() => list,
        _ => {
            let made = with_rooted(&[promise, handler, derived], || crisol_create_array(0));
            with_runtime(|runtime| {
                runtime
                    .heap
                    .set_internal(handle, PROMISE_REACTIONS_SLOT, Value::from_bits(made));
            });
            made
        }
    };
    let Some(target) = handle_of(list) else {
        return;
    };
    // Three elements per reaction: the handler, the promise it settles, and the flags.
    with_rooted(&[promise, list, handler, derived], || {
        with_runtime(|runtime| {
            let at = runtime.heap.element_count(target).unwrap_or(0);
            runtime
                .heap
                .set_element(target, at, Value::from_bits(handler));
            runtime
                .heap
                .set_element(target, at + 1, Value::from_bits(derived));
            runtime
                .heap
                .set_element(target, at + 2, Value::number(f64::from(flags)));
        });
    });
}

/// Settles `promise`, queueing everything that was waiting on it.
///
/// **A settled promise never settles again.** The first call wins, which is what makes the
/// `resolve` handed to an executor safe to call twice.
fn settle_promise(promise: u64, value: u64, rejected: bool) {
    let Some(handle) = handle_of(promise) else {
        return;
    };
    let waiting = with_runtime(|runtime| {
        if runtime.heap.internal(handle, 0) != Some(PROMISE_MARKER) {
            return None;
        }
        let state = runtime
            .heap
            .internal(handle, PROMISE_STATE_SLOT)
            .and_then(|slot| slot.as_number())
            .unwrap_or(0.0);
        if state != 0.0 {
            return None;
        }
        runtime.heap.set_internal(
            handle,
            PROMISE_STATE_SLOT,
            Value::number(if rejected { 2.0 } else { 1.0 }),
        );
        runtime
            .heap
            .set_internal(handle, PROMISE_VALUE_SLOT, Value::from_bits(value));
        let list = runtime.heap.internal(handle, PROMISE_REACTIONS_SLOT);
        // **Dropped as it is taken.** A settled promise never needs its reactions again, and
        // holding them keeps every handler — and everything each closure captured — alive for
        // as long as the promise is reachable.
        runtime
            .heap
            .set_internal(handle, PROMISE_REACTIONS_SLOT, Value::UNDEFINED);
        list.map(|value| value.to_bits())
    });
    let Some(list) = waiting else {
        return;
    };
    let Some((array, length)) = elements_of(list) else {
        return;
    };
    for at in (0..length).step_by(3) {
        let handler = element_at(array, at);
        let derived = element_at(array, at + 1);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a flag word this code wrote"
        )]
        let flags = Value::from_bits(element_at(array, at + 2))
            .as_number()
            .unwrap_or(0.0) as u32;
        let on_rejection = flags & REACTION_ON_REJECTION != 0;
        let passthrough = flags & REACTION_PASSTHROUGH != 0;
        // A reaction runs on the settlement it was registered for. `finally`'s runs on both,
        // which is what the pass-through flag says.
        if on_rejection == rejected || passthrough {
            enqueue_reaction(handler, value, derived, rejected, passthrough);
        }
    }
}

/// Queues one reaction. A missing handler passes the settlement through **as it was**, which
/// is what makes `.then(onFulfilled)` transparent to an error and `.catch` transparent when
/// nothing threw.
fn enqueue_reaction(handler: u64, value: u64, derived: u64, rejected: bool, passthrough: bool) {
    PROMISE_JOBS.with(|jobs| {
        jobs.borrow_mut().push_back(PromiseJob {
            handler,
            value,
            derived,
            rejected,
            passthrough,
        });
    });
}

/// `promise.then(onFulfilled, onRejected)`, answering the derived promise.
///
/// **Always asynchronous.** Attaching to an already-settled promise queues a job rather than
/// running it, so `Promise.resolve(1).then(f)` does not call `f` before `then` returns. Code
/// relying on the synchronous case works until the promise happens to be pending, which is
/// the intermittent failure worth never allowing.
fn promise_then(promise: u64, on_fulfilled: u64, on_rejected: u64, passthrough: bool) -> u64 {
    let derived = with_rooted(&[promise, on_fulfilled, on_rejected], new_promise_object);
    let extra = if passthrough { REACTION_PASSTHROUGH } else { 0 };
    with_rooted(
        &[promise, derived, on_fulfilled, on_rejected],
        || match promise_state(promise) {
            Some((Settled::Pending, _)) => {
                promise_add_reaction(promise, on_fulfilled, derived, extra);
                if !passthrough {
                    promise_add_reaction(promise, on_rejected, derived, REACTION_ON_REJECTION);
                }
            }
            Some((Settled::Fulfilled, value)) => {
                enqueue_reaction(on_fulfilled, value, derived, false, passthrough);
            }
            Some((Settled::Rejected, reason)) => {
                let handler = if passthrough {
                    on_fulfilled
                } else {
                    on_rejected
                };
                enqueue_reaction(handler, reason, derived, true, passthrough);
            }
            None => {}
        },
    );
    derived
}

/// One `resolve` or `reject` function, carrying the promise it settles.
fn new_settling_function(promise: u64, rejects: bool) -> u64 {
    let function = with_rooted(&[promise], || {
        with_runtime(|runtime| {
            runtime
                .native_function(
                    NATIVES.len()
                        + GLOBAL_NATIVES.len()
                        + NAMESPACE_NATIVES.len()
                        + PROMISE_SETTLE_CALL,
                )
                .to_value()
                .to_bits()
        })
    });
    with_rooted(&[function, promise], || {
        if let Some(handle) = handle_of(function) {
            with_runtime(|runtime| {
                runtime.define_hidden(handle, PROMISE_SETTLES, Value::from_bits(promise));
                runtime.define_hidden(handle, PROMISE_REJECTS, boolean(rejects));
            });
        }
    });
    function
}

/// The body every `resolve` and `reject` runs: settle the promise named on the function.
extern "C" fn promise_settle_call(
    closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let Some((_, promise)) = own_property(closure, PROMISE_SETTLES) else {
        return Value::UNDEFINED.to_bits();
    };
    let promise = promise.to_bits();
    let rejects = own_property(closure, PROMISE_REJECTS)
        .and_then(|(_, value)| value.as_boolean())
        .unwrap_or(false);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    // **Resolving with a promise adopts it**, which is what makes a chain of `then`s flatten
    // rather than nest. Rejecting never adopts: a reason is a value even when it is a
    // promise.
    if !rejects && is_promise(value) {
        with_rooted(&[promise, value], || adopt_promise(promise, value));
        return Value::UNDEFINED.to_bits();
    }
    with_rooted(&[promise, value], || {
        settle_promise(promise, value, rejects)
    });
    Value::UNDEFINED.to_bits()
}

/// Makes `derived` follow `other`, which is what returning a promise from a handler does.
fn adopt_promise(derived: u64, other: u64) {
    if derived == other {
        // Resolving a promise with itself can never settle, which is worse than an error
        // because it is indistinguishable from a call that has not come back.
        let reason = raise_value("a promise cannot resolve to itself", "TypeError");
        settle_promise(derived, reason, true);
        return;
    }
    // A pass-through pair: no handler, so the settlement arrives at `derived` unchanged.
    let pass = Value::UNDEFINED.to_bits();
    with_rooted(&[derived, other], || match promise_state(other) {
        Some((Settled::Pending, _)) => {
            promise_add_reaction(other, pass, derived, 0);
            promise_add_reaction(other, pass, derived, REACTION_ON_REJECTION);
        }
        Some((Settled::Fulfilled, value)) => enqueue_reaction(pass, value, derived, false, false),
        Some((Settled::Rejected, reason)) => enqueue_reaction(pass, reason, derived, true, false),
        None => {}
    });
}

/// Builds an error value **without throwing it**, for the places that carry a reason.
fn raise_value(message: &str, kind: &str) -> u64 {
    let thrown = raise(message, kind);
    if Value::from_bits(thrown).is_exception() {
        return crisol_pending_exception();
    }
    thrown
}

thread_local! {
    /// `Symbol.iterator`'s key, built once.
    ///
    /// **`for-of` asks for this every time a loop starts**, and building it meant a globals
    /// lookup, two shape lookups and a heap read — per loop, not per iteration, but a loop
    /// inside a hot function pays it every call. The symbol is made once during construction
    /// and never replaced, so the key can be too; cloning it is an `Arc` bump.
    static ITERATOR_KEY: RefCell<Option<PropertyKey>> = const { RefCell::new(None) };
}

/// The key `Symbol.iterator` names, if the runtime has got that far.
fn iterator_key() -> Option<PropertyKey> {
    if let Some(cached) = ITERATOR_KEY.with(|key| key.borrow().clone()) {
        return Some(cached);
    }
    let key = key_of(Value::from_bits(well_known_symbol("iterator")?))?;
    ITERATOR_KEY.with(|cached| {
        if let Ok(mut slot) = cached.try_borrow_mut() {
            *slot = Some(key.clone());
        }
    });
    Some(key)
}

/// The value a well-known symbol names, read off the `Symbol` global.
///
/// Read rather than cached, so a program that replaces `Symbol.iterator` is obeyed. That is
/// wrong for an internal operation and right against the alternative, which is a second copy
/// of each symbol kept in step by hand.
fn well_known_symbol(name: &str) -> Option<u64> {
    with_runtime(|runtime| {
        let globals = GLOBALS.with(std::cell::Cell::get)?;
        let symbol = runtime.global_object(globals, "Symbol")?;
        let key = PropertyKey::new(name);
        let shape = runtime.heap.shape_of(symbol)?;
        let slot = runtime.shapes.borrow().lookup(shape, &key)?;
        runtime
            .heap
            .get(symbol, slot.index())
            .map(|value| value.to_bits())
    })
}

/// Drains `value`'s own iterator into an array, or `None` if it has no `Symbol.iterator`.
///
/// **Eager, which the caller's contract already required.** `crisol_iterate` hands back
/// something the loop walks by index, so the whole sequence is materialised before the first
/// iteration of the body — a generator's side effects all happen up front, and an endless
/// iterator is refused at the cap rather than filling memory. Making it lazy means giving
/// `for-of` an iterator object to step, which is a change to the lowering.
fn iterate_by_protocol(value: u64) -> Option<u64> {
    let key = iterator_key()?;
    let method = symbol_property_load(value, &key);
    if !is_callable(method) {
        return None;
    }
    Some(with_rooted(&[value, method], || {
        let iterator = call_value(method, value, &[]);
        if Value::from_bits(iterator).is_exception() {
            return iterator;
        }
        with_rooted(&[iterator], || drain_iterator(iterator))
    }))
}

/// Walks an iterator to exhaustion, collecting what it yields.
fn drain_iterator(iterator: u64) -> u64 {
    with_new_array(0, |array| {
        let mut count = 0usize;
        loop {
            let advance = property_of(iterator, "next");
            if !is_callable(advance) {
                return raise("an iterator needs a `next` method", "TypeError");
            }
            let outcome = with_rooted(&[iterator, advance], || call_value(advance, iterator, &[]));
            if Value::from_bits(outcome).is_exception() {
                return outcome;
            }
            if handle_of(outcome).is_none() {
                return raise("an iterator step must be an object", "TypeError");
            }
            if is_truthy(Value::from_bits(property_of(outcome, "done"))) {
                break;
            }
            let item = with_rooted(&[iterator, outcome], || property_of(outcome, "value"));
            if Value::from_bits(item).is_exception() {
                return item;
            }
            if count > DENSE_ELEMENT_LIMIT {
                return raise("this iterator does not end", "RangeError");
            }
            with_rooted(&[iterator, item], || {
                with_runtime(|runtime| {
                    runtime
                        .heap
                        .set_element(array, count, Value::from_bits(item));
                });
            });
            count += 1;
        }
        array.to_value().to_bits()
    })
}

/// A named property of `object`, read as a value.
fn property_of(object: u64, name: &str) -> u64 {
    let key = name.to_owned();
    // SAFETY: `key` is a live Rust string.
    unsafe { crisol_property_load(object, key.as_ptr(), key.len() as u64) }
}

/// `/source/flags` — a regular expression object.
///
/// **The pattern is compiled here, not at first use**, so a syntactically invalid one is a
/// `SyntaxError` at the point the literal is evaluated rather than a surprise inside whatever
/// later called `test`.
///
/// # Safety
///
/// `source` and `flags` must each point to `*_len` readable bytes of UTF-8.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn crisol_create_regexp(
    source: *const u8,
    source_len: u64,
    flags: *const u8,
    flags_len: u64,
) -> u64 {
    // SAFETY: the caller guarantees the lengths and the encoding.
    let Some(source) = (unsafe { key_text(source, source_len) }) else {
        return raise("a regular expression needs a pattern", "SyntaxError");
    };
    // SAFETY: as above.
    let Some(flags_text) = (unsafe { key_text(flags, flags_len) }) else {
        return raise("a regular expression needs its flags", "SyntaxError");
    };

    let Ok(parsed) = crisol_builtins::Flags::parse(&flags_text) else {
        return raise("invalid regular expression flags", "SyntaxError");
    };
    // **A panic must not cross this boundary.** `crisol_create_regexp` is `extern "C"`, so an
    // unwind out of it aborts the process — the program dies on a signal with nothing to say
    // which pattern did it. A pattern the engine cannot compile is a `SyntaxError`, whether
    // the compiler says so or falls over saying it: three test262 cases were being reported as
    // crashes because `\p{…}` property escapes take the second route.
    let compiled = std::panic::catch_unwind(|| crisol_builtins::JsRegExp::new(&source, parsed));
    let compiled = match compiled {
        Ok(Ok(compiled)) => compiled,
        Ok(Err(message)) => return raise(&message, "SyntaxError"),
        Err(_) => return raise("this pattern is not supported", "SyntaxError"),
    };
    PATTERNS.with(|cache| {
        cache
            .borrow_mut()
            .insert((source.clone(), flags_text.clone()), compiled);
    });

    let object = crisol_create_object();
    with_rooted(&[object], || {
        let Some(handle) = handle_of(object) else {
            return;
        };
        // **Created and stored one at a time.** Both strings built first would leave the first
        // one unrooted while the second allocates, and a collection in between would free a
        // value the object was about to hold — which reads back as a string that is not there.
        let source_value = new_string(&source);
        with_runtime(|runtime| {
            runtime.define(handle, "source", Value::from_bits(source_value));
        });
        let flags_value = new_string(&flags_text);
        with_runtime(|runtime| {
            runtime.define(handle, "flags", Value::from_bits(flags_value));
            // `lastIndex` is the cursor, and it is a property because a program may assign to
            // it — the compiled pattern is set from it rather than owning it.
            runtime.define(handle, "lastIndex", Value::number(0.0));
            runtime.define(handle, "global", boolean(parsed.global));
            runtime.define(handle, "ignoreCase", boolean(parsed.ignore_case));
            runtime.define(handle, "multiline", boolean(parsed.multiline));
            runtime.define(handle, "sticky", boolean(parsed.sticky));
            runtime.define(handle, "unicode", boolean(parsed.unicode));
            runtime.define(handle, "dotAll", boolean(parsed.dot_all));
            if let Some(prototype) = REGEXP_PROTOTYPE.with(std::cell::Cell::get) {
                runtime.heap.set_prototype(handle, Some(prototype));
            }
        });
    });
    object
}

/// Whether `value` can be called.
///
/// A closure keeps its function index where no property can reach it, so **having one is what
/// makes an object callable** — which is what `typeof` reports on, and what JSON leaves out.
fn is_callable(value: u64) -> bool {
    Value::from_bits(value)
        .as_address()
        .map(GcRef::from_address)
        .is_some_and(|handle| {
            // **A number, not merely a slot.** Every function puts one there — a native's
            // index or a compiled function's code pointer — and a proxy puts `null` there to
            // mark itself (see `PROXY_MARKER`). Testing for presence alone made every proxy
            // report `typeof "function"`, which is the one thing the marker must not cost.
            with_runtime(|runtime| {
                runtime
                    .heap
                    .internal(handle, 0)
                    .is_some_and(|slot| slot.as_number().is_some())
            })
        })
}

/// The globals that are functions and **not** constructors.
///
/// Small enough to name, which is the point: everything else reachable through
/// [`GLOBAL_NATIVES`] is a constructor, so the list that has to stay correct is the short one.
/// `Symbol` is here because `new Symbol()` is a `TypeError` — a symbol exists to be unequal to
/// everything, and a wrapper for one would have an identity of its own.
const NOT_CONSTRUCTORS: &[&str] = &["parseInt", "parseFloat", "isNaN", "isFinite", "Symbol"];

/// The globals that own methods but are not functions themselves.
///
/// Every other name [`NAMESPACE_NATIVES`] hangs a method on is either already a global function
/// — `String`, `Number`, `Date` — or one of the two that is both a namespace and a constructor.
const NAMESPACES_ONLY: &[&str] = &["Math", "JSON", "Reflect"];

/// Whether `new value` is allowed — the specification's [[Construct]].
///
/// **Being callable is not enough.** `Array.prototype.find` is a function and
/// `new Array.prototype.find()` is a `TypeError`; test262 says so in a case of its own for
/// very nearly every built-in method it covers, and asks through `Reflect.construct`'s third
/// argument as well as through `new`.
///
/// Answered from the function index that is already in internal zero, rather than from a flag
/// stored beside it. The native tables share one index space laid out in a fixed order, so the
/// index says which table a function came from — and a method's table never holds a
/// constructor. Nothing is stored per function, which matters because the alternative costs
/// eight bytes on every closure a program allocates.
///
/// **A compiled function is a constructor here, and an arrow is not one in the language.**
/// Telling them apart needs the frontend to record which it lowered, because by the time a
/// closure exists the two are the same object; that is a change to the closure ABI rather than
/// to this, and is not made here.
fn is_constructor(value: u64) -> bool {
    let Some(handle) = handle_of(value) else {
        return false;
    };
    let index = with_runtime(|runtime| {
        runtime
            .heap
            .internal(handle, 0)
            .and_then(|slot| slot.as_number())
    });
    let Some(index) = index else {
        return false;
    };
    if index >= 0.0 {
        return true;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked negative, and the count of built-ins is tiny"
    )]
    let native = (-index - 1.0) as usize;
    // Before the globals is [`NATIVES`], which is every prototype method.
    let Some(global) = native.checked_sub(NATIVES.len()) else {
        return false;
    };
    if let Some((name, _)) = GLOBAL_NATIVES.get(global) {
        return !NOT_CONSTRUCTORS.contains(name);
    }
    // `Object` and `Array` run bodies from the anonymous table. The namespaces used to share
    // the first of them and no longer do — see `ensure_global_object` — so reaching it here
    // means one of those two rather than `Math`.
    let anonymous = global.wrapping_sub(GLOBAL_NATIVES.len() + NAMESPACE_NATIVES.len());
    if anonymous == CONSTRUCT_PLAIN_OBJECT || anonymous == CONSTRUCT_ARRAY {
        return true;
    }
    // **A bound function is constructable exactly when what it was bound to is.**
    // `new (Date.bind(null))()` is a date and `new (Math.max.bind(null))()` is a `TypeError`,
    // so the question has to be passed along rather than answered from the wrapper — which
    // looks the same in both cases.
    if anonymous == BOUND_CALL {
        return is_constructor(property_of(value, BOUND_TARGET));
    }
    false
}

/// A heap value as JSON, or `None` for one JSON has no spelling for.
///
/// **`undefined` and functions are `None`, not null.** The difference is what makes
/// `JSON.stringify({a: undefined})` the string `"{}"` rather than `{"a":null}`, while
/// `JSON.stringify([undefined])` *is* `[null]` — an array cannot drop an element without
/// changing its length, so the two containers treat the same absence differently.
///
/// `visiting` carries the objects above this one, so a cycle is caught rather than followed.
fn to_json(value: u64, visiting: &mut Vec<GcRef>) -> Result<Option<crisol_builtins::Json>, ()> {
    let held = Value::from_bits(value);
    match held.kind() {
        crisol_value::Kind::Undefined => return Ok(None),
        crisol_value::Kind::Null => return Ok(Some(crisol_builtins::Json::Null)),
        crisol_value::Kind::Boolean => {
            return Ok(Some(crisol_builtins::Json::Bool(
                held.as_boolean().unwrap_or(false),
            )));
        }
        crisol_value::Kind::String => {
            return Ok(text_of(value).map(crisol_builtins::Json::String));
        }
        _ => {}
    }
    if let Some(number) = held.as_number() {
        // **A non-finite number is `null`**, because JSON has no spelling for `NaN` or an
        // infinity and refusing the whole document over one would be worse.
        return Ok(Some(if number.is_finite() {
            crisol_builtins::Json::Number(number)
        } else {
            crisol_builtins::Json::Null
        }));
    }

    let Some(handle) = handle_of(value) else {
        return Ok(None);
    };
    if visiting.contains(&handle) {
        // A cycle. `Err` rather than a truncated document, because a document that silently
        // stops describing the value is worse than no document.
        return Err(());
    }
    if is_callable(value) {
        return Ok(None);
    }
    visiting.push(handle);
    let converted = if let Some((array, length)) = elements_of(value) {
        let mut items = Vec::with_capacity(length);
        for index in 0..length {
            // An element with no JSON spelling becomes `null`: an array cannot drop one
            // without changing its length.
            let item = to_json(element_at(array, index), visiting)?;
            items.push(item.unwrap_or(crisol_builtins::Json::Null));
        }
        crisol_builtins::Json::Array(items)
    } else {
        let mut entries = Vec::new();
        for name in enumerable_keys(value) {
            let key = name.clone();
            // SAFETY: `key` is a live Rust string.
            let held = unsafe { crisol_property_load(value, key.as_ptr(), key.len() as u64) };
            // A property with no JSON spelling is dropped, which is what makes
            // `{a: undefined}` stringify as `{}`.
            if let Some(item) = to_json(held, visiting)? {
                entries.push((name, item));
            }
        }
        crisol_builtins::Json::Object(entries)
    };
    visiting.pop();
    Ok(Some(converted))
}

/// JSON as a heap value.
fn from_json(json: &crisol_builtins::Json) -> u64 {
    match json {
        crisol_builtins::Json::Null => Value::NULL.to_bits(),
        crisol_builtins::Json::Bool(flag) => boolean(*flag).to_bits(),
        crisol_builtins::Json::Number(number) => from_number(*number),
        crisol_builtins::Json::String(text) => new_string(text),
        crisol_builtins::Json::Array(items) => with_new_array(items.len(), |array| {
            for (index, item) in items.iter().enumerate() {
                // Built and stored one at a time: the array is rooted and the element is not,
                // so holding several before storing any would leave them collectable.
                let value = from_json(item);
                with_runtime(|runtime| {
                    runtime
                        .heap
                        .set_element(array, index, Value::from_bits(value));
                });
            }
            array.to_value().to_bits()
        }),
        crisol_builtins::Json::Object(entries) => {
            let object = crisol_create_object();
            with_rooted(&[object], || {
                let Some(handle) = handle_of(object) else {
                    return;
                };
                for (name, item) in entries {
                    let value = from_json(item);
                    with_runtime(|runtime| {
                        runtime.define(handle, name, Value::from_bits(value));
                    });
                }
            });
            object
        }
    }
}

/// `JSON.parse`.
extern "C" fn json_parse(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(text) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return raise("cannot parse this value as JSON", "SyntaxError");
    };
    match crisol_builtins::parse(&text) {
        // The reviver argument is not applied. Recorded rather than ignored silently: a
        // program passing one gets the parsed document unchanged, which is wrong quietly.
        Ok(json) => from_json(&json),
        Err(error) => raise(&format!("{error}"), "SyntaxError"),
    }
}

/// `JSON.stringify`.
extern "C" fn json_stringify(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above. The second argument is a replacer, which is not applied.
    let space = Value::from_bits(unsafe { argument(argc, argv, 2) })
        .as_number()
        .unwrap_or(0.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "an indent, clamped to what the specification allows"
    )]
    let indent = space.clamp(0.0, 10.0) as usize;

    let mut visiting = Vec::new();
    match to_json(value, &mut visiting) {
        // **`undefined` for a value JSON cannot spell**, which is what
        // `JSON.stringify(undefined)` answers — not the string `"undefined"`.
        Ok(None) => Value::UNDEFINED.to_bits(),
        Ok(Some(json)) => new_string(&crisol_builtins::stringify(&json, indent)),
        Err(()) => raise(
            "cannot stringify a structure that contains itself",
            "TypeError",
        ),
    }
}

/// `left == right` — equality after coercion.
///
/// The rule is short and the consequences are not. **Same type defers to `===`**, so everything
/// `===` already gets right about `NaN` and the two zeroes is inherited rather than restated.
/// Across types exactly three coercions apply, in this order:
///
/// - **`null` and `undefined` equal each other and nothing else.** Not `0`, not `""`, not
///   `false`. This is the rule that makes `x == null` the idiomatic "is it either", and it is
///   also why `document.all`-style exceptions are the only ones a real engine carves out.
/// - **A boolean becomes a number first**, on whichever side it is. That is why `[] == false`
///   is true: `false` becomes `0`, the array becomes `""`, and `""` becomes `0`.
/// - **An object becomes a primitive, and a string meeting a number becomes a number.** Never
///   the reverse — `"10" == 10` compares `10` with `10`, not `"10"` with `"10"`.
///
/// Not transitive, and the example is worth keeping in view: `"" == 0` and `"0" == 0` are both
/// true while `"" == "0"` is false, because the first two coerce and the third does not.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_loose_equal(left: u64, right: u64) -> u64 {
    // **Both operands rooted for the whole comparison.** Coercing an object calls its
    // `valueOf`, which is JavaScript and allocates — and the *other* operand is a live value
    // nothing else is holding. The symptom is a comparison that is right until a collection
    // lands in the middle of it.
    with_rooted(&[left, right], || {
        boolean(loosely_equal(left, right, 0)).to_bits()
    })
}

/// `left != right`.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_loose_not_equal(left: u64, right: u64) -> u64 {
    with_rooted(&[left, right], || {
        boolean(!loosely_equal(left, right, 0)).to_bits()
    })
}

/// How many times `==` may re-enter itself before giving up.
///
/// Each coercion strictly simplifies one side — object to primitive, boolean to number, string
/// to number — so two rounds is the most the specification can need. The limit is here so a
/// `valueOf` that returns another object cannot spin forever.
const COERCION_ROUNDS: u32 = 4;

/// The comparison behind `==`.
fn loosely_equal(left: u64, right: u64, round: u32) -> bool {
    if round >= COERCION_ROUNDS {
        return false;
    }
    let a = Value::from_bits(left);
    let b = Value::from_bits(right);

    let nullish = |value: Value| {
        matches!(
            value.kind(),
            crisol_value::Kind::Undefined | crisol_value::Kind::Null
        )
    };
    // **`null` and `undefined` equal each other and nothing else**, so this is checked before
    // any coercion — `null == 0` must not become `0 == 0`.
    if nullish(a) || nullish(b) {
        return nullish(a) && nullish(b);
    }

    // Same type is `===`, which already knows about `NaN` and the two zeroes.
    if a.kind() == b.kind() && !(a.as_number().is_some() ^ b.as_number().is_some()) {
        return Value::from_bits(crisol_strict_equal(left, right)) == Value::TRUE;
    }

    // A boolean becomes a number first, whichever side it is on.
    if a.kind() == crisol_value::Kind::Boolean {
        return loosely_equal(from_number(to_number(left)), right, round + 1);
    }
    if b.kind() == crisol_value::Kind::Boolean {
        return loosely_equal(left, from_number(to_number(right)), round + 1);
    }

    let numeric = |value: Value| value.as_number().is_some();
    let stringy = |value: Value| value.kind() == crisol_value::Kind::String;

    // A string meeting a number becomes a number — never the reverse.
    if stringy(a) && numeric(b) {
        return loosely_equal(from_number(to_number(left)), right, round + 1);
    }
    if numeric(a) && stringy(b) {
        return loosely_equal(left, from_number(to_number(right)), round + 1);
    }

    // An object meeting a primitive becomes a primitive.
    let objectish = |value: u64| {
        handle_of(value).is_some() && Value::from_bits(value).kind() != crisol_value::Kind::String
    };
    // **The fresh primitive is rooted before the next round.** `toString` returns a new
    // string, and the round after this one may call *another* `valueOf` — which allocates,
    // with the string reachable from nothing.
    if objectish(left) && (numeric(b) || stringy(b)) {
        let primitive = to_primitive(left);
        return with_rooted(&[primitive], || loosely_equal(primitive, right, round + 1));
    }
    if (numeric(a) || stringy(a)) && objectish(right) {
        let primitive = to_primitive(right);
        return with_rooted(&[primitive], || loosely_equal(left, primitive, round + 1));
    }
    false
}

/// An object as a primitive, preferring `toString`.
///
/// The mirror of [`to_primitive`], which prefers `valueOf`. **The order is the whole
/// difference**: a string context asks for text first and a numeric one asks for a number
/// first, and an object that answers both would otherwise give the wrong one to one of them.
fn to_primitive_text(value: u64) -> u64 {
    for name in ["toString", "valueOf"] {
        let key = name.to_owned();
        // SAFETY: `key` is a live Rust string.
        let method = unsafe { crisol_property_load(value, key.as_ptr(), key.len() as u64) };
        if !is_callable(method) {
            continue;
        }
        let result = call_value(method, value, &[]);
        if handle_of(result).is_none()
            || Value::from_bits(result).kind() == crisol_value::Kind::String
        {
            return result;
        }
    }
    value
}

/// An object as a primitive, for `==`.
///
/// `valueOf` first and `toString` second, which is the order for everything except `Date`. A
/// result that is still an object is handed back unchanged and the round limit stops the
/// recursion — the specification throws there, and throwing from inside `==` would need an
/// exception path the operator does not have.
fn to_primitive(value: u64) -> u64 {
    for name in ["valueOf", "toString"] {
        let key = name.to_owned();
        // SAFETY: `key` is a live Rust string.
        let method = unsafe { crisol_property_load(value, key.as_ptr(), key.len() as u64) };
        if !is_callable(method) {
            continue;
        }
        let result = call_value(method, value, &[]);
        if handle_of(result).is_none()
            || Value::from_bits(result).kind() == crisol_value::Kind::String
        {
            return result;
        }
    }
    value
}

/// `key in object` — whether the property is on it or anywhere up its chain.
///
/// **Inherited counts**, which is the whole difference from `hasOwnProperty`. An index past the
/// end of an array is absent, so `5 in [1, 2]` is false where `1 in [1, 2]` is true.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_in(key: u64, object: u64) -> u64 {
    if handle_of(object).is_none() {
        return raise("the right side of `in` must be an object", "TypeError");
    }
    if let Some((target, handler)) = proxy_parts(object) {
        let Some(name) = key_of(Value::from_bits(key)) else {
            return Value::FALSE.to_bits();
        };
        return proxy_has(target, handler, &name);
    }
    if let Some(index) = as_index(Value::from_bits(key))
        && let Some((_, length)) = elements_of(object)
    {
        return boolean(index < length).to_bits();
    }
    if Value::from_bits(key).kind() == crisol_value::Kind::Symbol {
        let Some(symbol) = key_of(Value::from_bits(key)) else {
            return Value::FALSE.to_bits();
        };
        let mut current = object;
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            if symbol_own_slot(current, &symbol).is_some() {
                return Value::TRUE.to_bits();
            }
            let Some(handle) = handle_of(current) else {
                break;
            };
            match with_runtime(|runtime| runtime.heap.prototype_of(handle)) {
                Some(parent) => current = parent.to_value().to_bits(),
                None => break,
            }
        }
        return Value::FALSE.to_bits();
    }
    let Some(name) = to_text(key) else {
        return Value::FALSE.to_bits();
    };

    let mut current = object;
    for _ in 0..PROTOTYPE_CHAIN_LIMIT {
        if own_property(current, &name).is_some() {
            return Value::TRUE.to_bits();
        }
        let Some(handle) = handle_of(current) else {
            break;
        };
        match with_runtime(|runtime| runtime.heap.prototype_of(handle)) {
            Some(parent) => current = parent.to_value().to_bits(),
            None => break,
        }
    }
    Value::FALSE.to_bits()
}

/// Appends to an array a literal is building.
///
/// **`spread` is the whole difference between `[...a]` and `[a]`.** Without it the operand is
/// appended as one element; with it, each of the operand's elements is.
///
/// Spreading uses the same rule `for-of` does (D-120) — an array or a string, and a `TypeError`
/// for anything else — so `[...5]` and `for (x of 5)` fail the same way rather than one of them
/// quietly producing a one-element array.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_array_extend(array: u64, value: u64, spread: u64) -> u64 {
    let Some(handle) = handle_of(array) else {
        return Value::UNDEFINED.to_bits();
    };
    if spread == 0 {
        let at = with_runtime(|runtime| runtime.heap.element_count(handle).unwrap_or(0));
        with_runtime(|runtime| {
            runtime
                .heap
                .set_element(handle, at, Value::from_bits(value));
        });
        return Value::UNDEFINED.to_bits();
    }

    let source = crisol_iterate(value);
    if Value::from_bits(source).is_exception() {
        return source;
    }
    with_rooted(&[array, source], || {
        let Some((from, length)) = elements_of(source) else {
            return Value::UNDEFINED.to_bits();
        };
        for index in 0..length {
            let element = element_at(from, index);
            let at = with_runtime(|runtime| runtime.heap.element_count(handle).unwrap_or(0));
            with_runtime(|runtime| {
                runtime
                    .heap
                    .set_element(handle, at, Value::from_bits(element));
            });
        }
        Value::UNDEFINED.to_bits()
    })
}

/// The `arguments` object for a call, as an array.
///
/// **An array rather than the specification's array-*like*.** A real `arguments` is a plain
/// object with a `length`, `Symbol.iterator`, and — outside strict mode — aliasing between its
/// elements and the named parameters, so `arguments[0] = 1` changes `a`. None of that is here:
/// this is a genuine array holding a *copy* of what the caller passed.
///
/// What that buys is everything array-shaped working immediately — `length`, indexing,
/// `for-of`, spread. What it costs is the aliasing, and `Array.isArray(arguments)` answering
/// `true` where a real engine says `false`. Recorded rather than left to be discovered, because
/// both differences read as correct until a test looks straight at them.
///
/// # Safety
///
/// `argv` must point to `argc` readable values.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn crisol_create_arguments(argc: u64, argv: *const u64) -> u64 {
    let given: Vec<u64> = (0..argc as usize)
        // SAFETY: the caller guarantees `argc` readable values at `argv`.
        .map(|position| unsafe { argument(argc, argv, position) })
        .collect();
    // **Rooted across the allocation.** This runs in the callee's prologue, before any of its
    // slots exist, so the only thing describing these values is the caller's frame — and a
    // collection during the array's own allocation would free an argument the array is about
    // to hold. It reads back as an element that is there and unreadable.
    with_rooted(&given, || array_of_values(&given))
}

/// Reads a global, answering `undefined` when it is absent rather than raising.
///
/// **Only `typeof` asks this way.** Every other read of a missing global is a `ReferenceError`;
/// `typeof` is the one operator the specification exempts, which is what makes
/// `typeof somethingUndeclared` the string `"undefined"` instead of a thrown error.
///
/// # Safety
///
/// `name` must point to `length` readable bytes of UTF-8.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn crisol_global_load_optional(name: *const u8, length: u64) -> u64 {
    // SAFETY: the caller promises `length` readable UTF-8 bytes at `name`.
    let loaded = unsafe { crisol_global_load(name, length) };
    if Value::from_bits(loaded).is_exception() {
        // The throw is already recorded as pending, so it has to be cleared — leaving it would
        // hand the *next* `catch` an exception nobody raised.
        let _cleared = crisol_pending_exception();
        return Value::UNDEFINED.to_bits();
    }
    loaded
}

/// `<`, `<=`, `>`, `>=` on values that are not both known to be numbers.
///
/// **Two strings compare lexicographically, and everything else numerically.** `"a" < "b"` is
/// true and `"10" < "9"` is *also* true — as text, `"1"` precedes `"9"` — while `10 < 9` is
/// false. Coercing both sides to a number unconditionally made every string comparison a `NaN`
/// comparison, which is `false` for all four operators; so `"a" < "b"` and `"b" < "a"` were both
/// false, and a sort comparator written the ordinary way returned `0` for every pair.
///
/// `which` is 0 for `<`, 1 for `<=`, 2 for `>`, 3 for `>=`.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_relational(left: u64, right: u64, which: u64) -> u64 {
    with_rooted(&[left, right], || {
        let left = to_primitive(left);
        with_rooted(&[left], || {
            let right = to_primitive(right);
            let both_strings = Value::from_bits(left).kind() == crisol_value::Kind::String
                && Value::from_bits(right).kind() == crisol_value::Kind::String;

            let outcome = if both_strings {
                let (Some(a), Some(b)) = (text_of(left), text_of(right)) else {
                    return Value::FALSE.to_bits();
                };
                match which {
                    0 => a < b,
                    1 => a <= b,
                    2 => a > b,
                    _ => a >= b,
                }
            } else {
                let a = to_number(left);
                let b = to_number(right);
                // **`NaN` makes all four false**, which falls out of IEEE comparison and is
                // worth not second-guessing: `!(a < b)` is not `a >= b` here.
                match which {
                    0 => a < b,
                    1 => a <= b,
                    2 => a > b,
                    _ => a >= b,
                }
            };
            boolean(outcome).to_bits()
        })
    })
}
