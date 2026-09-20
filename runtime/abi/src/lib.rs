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
    "crisol_property_load",
    "crisol_closure_capture",
    "crisol_create_closure",
    "crisol_closure_set_capture",
    "crisol_closure_code",
    "crisol_not_a_function",
    "crisol_construct_this",
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
        roots.extend(SET_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(SYMBOL_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(ARRAY_ITERATOR_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(NUMBER_PROTOTYPE.with(std::cell::Cell::get));
        roots.extend(BOOLEAN_PROTOTYPE.with(std::cell::Cell::get));
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
        let length = elements_of(target).map_or(0, |(_, len)| len);
        let result = crisol_create_object();
        with_rooted(&[result], || {
            let Some(into) = handle_of(result) else {
                return;
            };
            if at >= length {
                with_runtime(|runtime| {
                    runtime.define(into, "value", Value::UNDEFINED);
                    runtime.define(into, "done", Value::TRUE);
                });
                return;
            }
            let Some((array, _)) = elements_of(target) else {
                return;
            };
            // Built and stored one at a time, because each allocates (D-127).
            let value = if kind == 0.0 {
                index_value(at)
            } else if kind == 2.0 {
                array_of_values(&[index_value(at), element_at(array, at)])
            } else {
                element_at(array, at)
            };
            with_runtime(|runtime| {
                runtime.define(into, "value", Value::from_bits(value));
                runtime.define(into, "done", Value::FALSE);
            });
            #[expect(clippy::cast_precision_loss, reason = "an index into an array")]
            let next = (at + 1) as f64;
            if let Some(handle) = handle_of(this_value) {
                with_runtime(|runtime| {
                    runtime.define_hidden(handle, ITERATOR_POSITION, Value::number(next));
                });
            }
        });
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
    let Some((array, length)) = elements_of(this_value) else {
        return this_value;
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = relative_index(unsafe { argument(argc, argv, 0) }, length, 0);
    // SAFETY: as above.
    let start = relative_index(unsafe { argument(argc, argv, 1) }, length, 0);
    // SAFETY: as above.
    let end = relative_index(unsafe { argument(argc, argv, 2) }, length, length);

    let taken = end.saturating_sub(start).min(length - target);
    // Read before writing, because the source and destination runs may overlap — copying in
    // place forwards would read values it had already overwritten.
    let moved: Vec<u64> = (0..taken).map(|at| element_at(array, start + at)).collect();
    with_runtime(|runtime| {
        for (at, value) in moved.iter().enumerate() {
            runtime
                .heap
                .set_element(array, target + at, Value::from_bits(*value));
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
    let start = relative_index(unsafe { argument(argc, argv, 0) }, length, 0);
    let removing = if argc < 2 {
        length - start
    } else {
        // SAFETY: as above.
        let asked = Value::from_bits(unsafe { argument(argc, argv, 1) })
            .as_number()
            .unwrap_or(0.0);
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
    let start = relative_index(unsafe { argument(argc, argv, 0) }, length, 0);
    // **No second argument removes everything from `start` on**; a second argument of
    // `undefined` removes nothing. The two are different, which is why `argc` is read rather
    // than the value.
    let removing = if argc < 2 {
        length - start
    } else {
        // SAFETY: as above.
        let asked = Value::from_bits(unsafe { argument(argc, argv, 1) })
            .as_number()
            .unwrap_or(0.0);
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
    let length = indexed_length(this_value);
    with_rooted(&[this_value], || {
        let mut out = String::new();
        for index in 0..length {
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
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
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
            element_at(array, position)
        };
        while position > 0 {
            position -= 1;
            let element = element_at(array, position);
            total = call_value(
                callback,
                Value::UNDEFINED.to_bits(),
                &[total, element, index_value(position), this_value],
            );
            if Value::from_bits(total).is_exception() {
                return total;
            }
        }
        total
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
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let wanted = Value::from_bits(unsafe { argument(argc, argv, 0) })
        .as_number()
        .unwrap_or(0.0);
    if wanted.is_nan() {
        return element_at(array, 0);
    }
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
    element_at(array, index)
}

/// `findLast` and `findLastIndex`, which walk backwards.
fn find_last_with(this_value: u64, argc: u64, argv: *const u64, want_index: bool) -> u64 {
    let length = indexed_length(this_value);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        for index in (0..length).rev() {
            let element = indexed_get(this_value, index);
            let verdict = call_value(
                callback,
                this_value,
                &[element, index_value(index), this_value],
            );
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
];

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
fn this_number(this_value: u64) -> f64 {
    if handle_of(this_value).is_some() {
        return property_number(this_value, STRING_PRIMITIVE).unwrap_or(f64::NAN);
    }
    to_number(this_value)
}

/// `Number.prototype.toString(radix)`.
extern "C" fn number_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let value = this_number(this_value);
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
    from_number(this_number(this_value))
}

/// `Number.prototype.toFixed(digits)`.
extern "C" fn number_to_fixed(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let value = this_number(this_value);
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
fn this_boolean(this_value: u64) -> bool {
    if handle_of(this_value).is_some() {
        return property_number(this_value, STRING_PRIMITIVE).is_some_and(|held| held != 0.0);
    }
    is_truthy(Value::from_bits(this_value))
}

/// `Boolean.prototype.toString`.
extern "C" fn boolean_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    new_string(if this_boolean(this_value) {
        "true"
    } else {
        "false"
    })
}

/// `Boolean.prototype.valueOf`.
extern "C" fn boolean_value_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    boolean(this_boolean(this_value)).to_bits()
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

/// Records how many entries a collection now holds.
fn set_collection_size(collection: u64, entries: usize, stride: usize) {
    if let Some(handle) = handle_of(collection) {
        #[expect(clippy::cast_precision_loss, reason = "a count of heap entries")]
        let size = (entries / stride) as f64;
        with_runtime(|runtime| runtime.define_hidden(handle, COLLECTION_SIZE, Value::number(size)));
    }
}

/// Builds a `Map` or a `Set`: an object with a backing array and a size.
fn new_collection(prototype: Option<GcRef>) -> u64 {
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
    new_collection(MAP_PROTOTYPE.with(std::cell::Cell::get))
}

/// `new Set()`.
extern "C" fn make_set(
    _closure: u64,
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    new_collection(SET_PROTOTYPE.with(std::cell::Cell::get))
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
        // made here.
        let target = handle_of(this_value).map_or_else(|| handle_of(crisol_create_object()), Some);
        let Some(target) = target else {
            return Value::UNDEFINED.to_bits();
        };
        with_runtime(|runtime| {
            if Value::from_bits(message).kind() != crisol_value::Kind::Undefined {
                let text = to_text(message).unwrap_or_default();
                runtime.define(target, "message", Value::from_bits(new_string(&text)));
            }
            if let Some(name) = handle_of(closure).and_then(|c| {
                let key = PropertyKey::new("name");
                let shape = runtime.heap.shape_of(c)?;
                let slot = runtime.shapes.borrow().lookup(shape, &key)?;
                runtime.heap.get(c, slot.index())
            }) && name.kind() == crisol_value::Kind::String
            {
                runtime.define(target, "name", name);
            }
        });
        target.to_value().to_bits()
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
    // A `new Boolean(…)` wrapper records what it wraps, as the number and string ones do.
    if let Some(handle) = handle_of(this_value) {
        let held = f64::from(u8::from(Value::from_bits(truth) == Value::TRUE));
        with_runtime(|runtime| {
            runtime.define_hidden(handle, STRING_PRIMITIVE, Value::number(held));
        });
    }
    truth
}

/// Built-ins reachable only as the body of a namespace object, not by any name.
///
/// Numbered last, after [`NATIVES`], [`GLOBAL_NATIVES`] and [`NAMESPACE_NATIVES`]. A table of
/// its own because the index space is shared: the first version of this pointed at index 0 of
/// the *first* table, so calling `Object()` ran `Array.prototype.map`.
const ANONYMOUS_NATIVES: &[Native] = &[construct_plain_object, bound_call];

/// The index within [`ANONYMOUS_NATIVES`] of the plain-object constructor.
///
/// `Object()` and `Array()` both answer with a plain object, which is right for `Object` and
/// wrong for `Array` — `Array(3)` should give a three-element array. Recorded rather than left
/// to be discovered.
const CONSTRUCT_PLAIN_OBJECT: usize = 0;

/// The index within [`ANONYMOUS_NATIVES`] of the body every bound function runs.
const BOUND_CALL: usize = 1;

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
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    // `new Object()` already has a receiver; a plain call does not.
    if handle_of(this_value).is_some() {
        return this_value;
    }
    crisol_create_object()
}

/// Methods that hang off a global object rather than being one.
///
/// Numbered after [`NATIVES`] and [`GLOBAL_NATIVES`], continuing the one negative index space
/// so `crisol_closure_code` still has a single rule.
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
];

/// The time value a date holds, or `NaN` if it is not a date.
fn time_of(this_value: u64) -> f64 {
    property_number(this_value, DATE_TIME).unwrap_or(f64::NAN)
}

/// One of the field readers, all of which answer `NaN` for an invalid date.
fn date_field(this_value: u64, read: impl FnOnce(&crisol_builtins::Fields) -> i64) -> u64 {
    let time = time_of(this_value);
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
    from_number(time_of(this_value))
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
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    from_number(0.0)
}

/// `Date.prototype.toISOString` and `toJSON`.
extern "C" fn date_to_iso(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    // **An invalid date raises here and prints as text elsewhere.** `toISOString` has no
    // spelling for one, where `toString` does.
    crisol_builtins::to_iso_string(time_of(this_value)).map_or_else(
        || raise("this date cannot be represented as ISO text", "RangeError"),
        |text| new_string(&text),
    )
}

/// `Date.prototype.toString`.
extern "C" fn date_to_text(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    crisol_builtins::to_iso_string(time_of(this_value)).map_or_else(
        || new_string(crisol_builtins::INVALID_DATE),
        |text| new_string(&text),
    )
}

/// Methods on `Map.prototype`.
///
/// **A map stores key and value adjacently** in one backing array, so an entry is a pair at an
/// even offset. One array rather than two keeps them from ever disagreeing about length.
const MAP_NATIVES: &[(&str, Native)] = &[
    ("get", map_get),
    ("set", map_set),
    ("has", map_has),
    ("delete", map_delete),
    ("clear", collection_clear),
    ("forEach", map_for_each),
];

/// Methods on `Set.prototype`.
const SET_NATIVES: &[(&str, Native)] = &[
    ("add", set_add),
    ("has", set_has),
    ("delete", set_delete),
    ("clear", collection_clear),
    ("forEach", set_for_each),
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
    let Some((array, _length)) = entries_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
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
    let Some((array, _)) = entries_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
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
    ("hasOwnProperty", object_has_own_property),
    ("propertyIsEnumerable", object_property_is_enumerable),
    ("isPrototypeOf", object_is_prototype_of),
    ("toString", object_to_text),
    ("toLocaleString", object_to_text),
    ("valueOf", object_value_of),
];

/// `Object.prototype.hasOwnProperty`.
///
/// **Own means own**: a property found on the prototype answers `false`, which is the whole
/// reason this exists rather than `key in object`.
extern "C" fn object_has_own_property(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(name) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return Value::FALSE.to_bits();
    };
    if let Some(index) = as_index(Value::from_bits(
        // SAFETY: as above.
        unsafe { argument(argc, argv, 0) },
    )) && let Some((_, length)) = elements_of(this_value)
    {
        return boolean(index < length).to_bits();
    }
    boolean(own_property(this_value, &name).is_some()).to_bits()
}

/// `Object.prototype.propertyIsEnumerable`.
extern "C" fn object_property_is_enumerable(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let Some(name) = to_text(unsafe { argument(argc, argv, 0) }) else {
        return Value::FALSE.to_bits();
    };
    let Some(handle) = handle_of(this_value) else {
        return Value::FALSE.to_bits();
    };
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
    // **The array tag is the only one distinguished.** A real engine reads
    // `Symbol.toStringTag` and a class list; without symbols the honest choice is the one
    // distinction that can be made without guessing.
    if elements_of(this_value).is_some() {
        return new_string("[object Array]");
    }
    match Value::from_bits(this_value).kind() {
        crisol_value::Kind::Undefined => new_string("[object Undefined]"),
        crisol_value::Kind::Null => new_string("[object Null]"),
        _ if is_callable(this_value) => new_string("[object Function]"),
        _ => new_string("[object Object]"),
    }
}

/// `Object.prototype.valueOf`.
extern "C" fn object_value_of(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
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

    with_rooted(&[this_value], || {
        let whole = text
            .get(found.start..found.end)
            .unwrap_or_default()
            .to_owned();
        with_new_array(found.groups.len() + 1, |array| {
            let first = new_string(&whole);
            with_runtime(|runtime| {
                runtime.heap.set_element(array, 0, Value::from_bits(first));
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
            // Byte offsets become code-unit offsets, so `index` is in the same space
            // `length` and `charAt` use (D-115).
            let prefix = text.get(..found.start).unwrap_or_default();
            #[expect(clippy::cast_precision_loss, reason = "an index into a string")]
            let index = Value::number(prefix.encode_utf16().count() as f64);
            let input = new_string(&text);
            with_runtime(|runtime| {
                runtime.define(array, "index", index);
                runtime.define(array, "input", Value::from_bits(input));
            });
            array.to_value().to_bits()
        })
    })
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
    let Some(text) = this_text(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    let units = code_units(&text);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let wanted = Value::from_bits(unsafe { argument(argc, argv, 0) })
        .as_number()
        .unwrap_or(0.0);
    let wanted = if wanted.is_nan() { 0.0 } else { wanted };
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
    this_text(this_value).map_or_else(|| new_string(""), |text| new_string(text.trim_start()))
}

/// `String.prototype.trimEnd`.
extern "C" fn string_trim_end(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    this_text(this_value).map_or_else(|| new_string(""), |text| new_string(text.trim_end()))
}

/// `padStart` and `padEnd`, which differ only in which side the filling goes.
fn pad_with(this_value: u64, argc: u64, argv: *const u64, at_start: bool) -> u64 {
    let Some(text) = this_text(this_value) else {
        return new_string("");
    };
    let units = code_units(&text);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let target = Value::from_bits(unsafe { argument(argc, argv, 0) })
        .as_number()
        .unwrap_or(0.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to a length no string can exceed"
    )]
    let target = target.clamp(0.0, f64::from(u32::MAX)) as usize;
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
    let Some(text) = this_text(this_value) else {
        return new_string("");
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let pattern = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let replacement = unsafe { argument(argc, argv, 1) };

    // A regular expression pattern, recognised by its `source` rather than by a type tag.
    if let Some(source) = property_text(pattern, "source") {
        let flags = property_text(pattern, "flags").unwrap_or_default();
        let global = flags.contains('g');
        if all && !global {
            return raise("replaceAll needs a global regular expression", "TypeError");
        }
        let Some(replacement) = to_text(replacement) else {
            return new_string(&text);
        };
        let Ok(parsed) = crisol_builtins::Flags::parse(&flags) else {
            return new_string(&text);
        };
        let Ok(mut compiled) = crisol_builtins::JsRegExp::new(&source, parsed) else {
            return new_string(&text);
        };
        let mut out = String::new();
        let mut cursor = 0;
        for found in compiled.all_matches(&text) {
            out.push_str(text.get(cursor..found.start).unwrap_or_default());
            out.push_str(&replacement);
            cursor = found.end;
            if !global {
                break;
            }
        }
        out.push_str(text.get(cursor..).unwrap_or_default());
        return new_string(&out);
    }

    let Some(needle) = to_text(pattern) else {
        return new_string(&text);
    };
    let Some(replacement) = to_text(replacement) else {
        return new_string(&text);
    };
    new_string(&if all {
        text.replace(&needle, &replacement)
    } else {
        text.replacen(&needle, &replacement, 1)
    })
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

/// A string as the code units JavaScript counts.
///
/// **UTF-16, not bytes and not scalar values.** `length`, `charAt` and every index are in code
/// units, so an emoji is two positions and a `é` is one. Measuring bytes reads correctly for
/// ASCII and wrongly for everything else, which is the worst way to be wrong.
fn code_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// The receiver of a string method, as text.
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
    let Some(text) = this_text(this_value) else {
        return new_string("");
    };
    let units = code_units(&text);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let index = Value::from_bits(unsafe { argument(argc, argv, 0) })
        .as_number()
        .unwrap_or(0.0);
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
    let Some(text) = this_text(this_value) else {
        return from_number(f64::NAN);
    };
    let units = code_units(&text);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let index = Value::from_bits(unsafe { argument(argc, argv, 0) })
        .as_number()
        .unwrap_or(0.0);
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
    let (Some(text), Some(needle)) = (this_text(this_value), {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        to_text(unsafe { argument(argc, argv, 0) })
    }) else {
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
    let (Some(text), Some(needle)) = (this_text(this_value), {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        to_text(unsafe { argument(argc, argv, 0) })
    }) else {
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
    let (Some(text), Some(needle)) = (this_text(this_value), {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        to_text(unsafe { argument(argc, argv, 0) })
    }) else {
        return Value::FALSE.to_bits();
    };
    if text.contains(&needle) {
        Value::TRUE
    } else {
        Value::FALSE
    }
    .to_bits()
}

/// `String.prototype.startsWith`.
extern "C" fn string_starts_with(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let (Some(text), Some(needle)) = (this_text(this_value), {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        to_text(unsafe { argument(argc, argv, 0) })
    }) else {
        return Value::FALSE.to_bits();
    };
    if text.starts_with(&needle) {
        Value::TRUE
    } else {
        Value::FALSE
    }
    .to_bits()
}

/// `String.prototype.endsWith`.
extern "C" fn string_ends_with(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let (Some(text), Some(needle)) = (this_text(this_value), {
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        to_text(unsafe { argument(argc, argv, 0) })
    }) else {
        return Value::FALSE.to_bits();
    };
    if text.ends_with(&needle) {
        Value::TRUE
    } else {
        Value::FALSE
    }
    .to_bits()
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
    let Some(text) = this_text(this_value) else {
        return new_string("");
    };
    let units = code_units(&text);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = relative_index(unsafe { argument(argc, argv, 0) }, units.len(), 0);
    // SAFETY: as above.
    let end = relative_index(unsafe { argument(argc, argv, 1) }, units.len(), units.len());
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
    let Some(text) = this_text(this_value) else {
        return new_string("");
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
    this_text(this_value).map_or_else(|| new_string(""), |text| new_string(&text.to_uppercase()))
}

/// `String.prototype.toLowerCase`.
extern "C" fn string_to_lower(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    this_text(this_value).map_or_else(|| new_string(""), |text| new_string(&text.to_lowercase()))
}

/// `String.prototype.trim`.
extern "C" fn string_trim(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    this_text(this_value).map_or_else(|| new_string(""), |text| new_string(text.trim()))
}

/// `String.prototype.concat`.
extern "C" fn string_concat(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let mut out = this_text(this_value).unwrap_or_default();
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
    let Some(text) = this_text(this_value) else {
        return new_string("");
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let count = Value::from_bits(unsafe { argument(argc, argv, 0) })
        .as_number()
        .unwrap_or(0.0);
    // A negative or infinite count is a `RangeError`, which is worth raising rather than
    // silently producing an empty string that reads like a legitimate answer.
    if count < 0.0 || !count.is_finite() {
        return raise("repeat count is out of range", "RangeError");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked non-negative and finite"
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
    let Some(text) = this_text(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let given = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let pieces: Vec<String> = match to_text(given) {
            // **An empty separator splits into characters**, and no separator at all gives a
            // one-element array holding the whole string — not an empty one.
            Some(separator) if separator.is_empty() => {
                text.chars().map(|c| c.to_string()).collect()
            }
            Some(separator) => text.split(&separator).map(ToOwned::to_owned).collect(),
            None => vec![text.clone()],
        };
        with_new_array(pieces.len(), |array| {
            for (index, piece) in pieces.iter().enumerate() {
                let value = new_string(piece);
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
    _new_target: u64,
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
];

/// Writes a property regardless of whether it is writable.
///
/// `defineProperty` redefines rather than assigns, so the check an assignment makes must not
/// apply — otherwise a property defined non-writable could never be redefined.
///
/// # Safety
///
/// `name` must be a live string.
unsafe fn define_ignoring_writability(handle: GcRef, name: &str, value: u64) -> u64 {
    let key = PropertyKey::new(name);
    with_runtime(|runtime| {
        let Some(current) = runtime.heap.shape_of(handle) else {
            return Value::UNDEFINED.to_bits();
        };
        let (shape, slot, width) = {
            let mut shapes = runtime.shapes.borrow_mut();
            let shape = shapes.add(current, &key);
            let Some(slot) = shapes.lookup(shape, &key) else {
                return Value::UNDEFINED.to_bits();
            };
            (shape, slot, shapes.len(shape) as usize)
        };
        if shape != current {
            runtime.heap.transition(handle, shape, width);
        }
        runtime
            .heap
            .set(handle, slot.index(), Value::from_bits(value));
        Value::UNDEFINED.to_bits()
    })
}

/// Reads a property of `object` by name, without walking the prototype chain.
fn own_property(object: u64, name: &str) -> Option<(u32, Value)> {
    let handle = handle_of(object)?;
    with_runtime(|runtime| {
        let shape = runtime.heap.shape_of(handle)?;
        let key = PropertyKey::new(name);
        let slot = runtime.shapes.borrow().lookup(shape, &key)?;
        runtime
            .heap
            .get(handle, slot.index())
            .map(|value| (slot.index(), value))
    })
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

        let Some(handle) = handle_of(target) else {
            return raise("cannot define a property on a non-object", "TypeError");
        };
        let Some(name) = to_text(key) else {
            return raise("a property key must be a name", "TypeError");
        };

        let existing = own_property(target, &name);
        let value_key = "value".to_owned();
        // SAFETY: `value_key` is a live Rust string.
        let given =
            unsafe { crisol_property_load(descriptor, value_key.as_ptr(), value_key.len() as u64) };
        let has_value = Value::from_bits(given).kind() != crisol_value::Kind::Undefined
            || own_property(descriptor, "value").is_some();

        // The write goes through the ordinary path so the shape transition happens there once.
        let stored = if has_value {
            given
        } else {
            existing.map_or(Value::UNDEFINED.to_bits(), |(_, value)| value.to_bits())
        };
        // SAFETY: `name` is a live Rust string.
        let outcome = unsafe { define_ignoring_writability(handle, &name, stored) };
        if Value::from_bits(outcome).is_exception() {
            return outcome;
        }

        let Some((slot, _)) = own_property(target, &name) else {
            return target;
        };
        let previous = with_runtime(|runtime| runtime.heap.attributes_of(handle, slot));
        let base = if existing.is_some() {
            previous
        } else {
            crisol_value::Attributes::DEFINED
        };
        let attributes = crisol_value::Attributes {
            writable: descriptor_flag(descriptor, "writable").unwrap_or(base.writable),
            enumerable: descriptor_flag(descriptor, "enumerable").unwrap_or(base.enumerable),
            configurable: descriptor_flag(descriptor, "configurable").unwrap_or(base.configurable),
        };
        with_runtime(|runtime| runtime.heap.set_attributes(handle, slot, attributes));
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
        let Some(name) = to_text(key) else {
            return Value::UNDEFINED.to_bits();
        };
        let Some(handle) = handle_of(target) else {
            return Value::UNDEFINED.to_bits();
        };
        // **`undefined` for an absent property**, which is how a caller tells "not there" from
        // "there and not writable".
        let Some((slot, value)) = own_property(target, &name) else {
            return Value::UNDEFINED.to_bits();
        };
        let attributes = with_runtime(|runtime| runtime.heap.attributes_of(handle, slot));

        let descriptor = crisol_create_object();
        with_rooted(&[descriptor], || {
            let Some(into) = handle_of(descriptor) else {
                return;
            };
            with_runtime(|runtime| {
                runtime.define(into, "value", value);
                runtime.define(into, "writable", boolean(attributes.writable));
                runtime.define(into, "enumerable", boolean(attributes.enumerable));
                runtime.define(into, "configurable", boolean(attributes.configurable));
            });
        });
        descriptor
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
    COLLECTION_ENTRIES,
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

/// The own property names of `this`'s first argument.
fn own_keys(object: u64) -> Vec<String> {
    let Some(handle) = handle_of(object) else {
        return Vec::new();
    };
    with_runtime(|runtime| {
        let mut names: Vec<String> = Vec::new();
        // **Indices come first and in numeric order**, before the string-named properties, which
        // is the enumeration order the specification fixes rather than insertion order.
        if let Some(count) = runtime.heap.element_count(handle) {
            for index in 0..count {
                names.push(number_text(index_as_f64(index)));
            }
        }
        if let Some(shape) = runtime.heap.shape_of(handle) {
            for (key, slot) in runtime.shapes.borrow().properties(shape) {
                if runtime.heap.is_deleted(handle, slot.index())
                    || is_internal_property(key.as_str())
                {
                    continue;
                }
                names.push(key.as_str().to_owned());
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
    own_keys(object)
        .into_iter()
        .filter(|name| {
            own_property(object, name).is_none_or(|(slot, _)| {
                with_runtime(|runtime| runtime.heap.attributes_of(handle, slot).enumerable)
            })
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
        let created = crisol_create_object();
        if let (Some(object), Some(parent)) = (handle_of(created), handle_of(proto)) {
            with_runtime(|runtime| runtime.heap.set_prototype(object, Some(parent)));
        } else if Value::from_bits(proto).kind() == crisol_value::Kind::Null {
            // `Object.create(null)` is the one way to get an object with no prototype at all.
            if let Some(object) = handle_of(created) {
                with_runtime(|runtime| runtime.heap.set_prototype(object, None));
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
    let Some(handle) = handle_of(target) else {
        return Value::NULL.to_bits();
    };
    with_runtime(|runtime| {
        runtime
            .heap
            .prototype_of(handle)
            .map_or_else(|| Value::NULL.to_bits(), |p| p.to_value().to_bits())
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
    if let Some(handle) = handle_of(target) {
        let parent = handle_of(proto);
        with_runtime(|runtime| runtime.heap.set_prototype(handle, parent));
    }
    target
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
    // SAFETY: as above.
    let key = unsafe { argument(argc, argv, 1) };
    let Some(wanted) = to_text(key) else {
        return Value::FALSE.to_bits();
    };
    // **Own**, so the prototype chain is not walked — which is the whole point of the method,
    // and the reason it cannot be written as a property read against `undefined`.
    if own_keys(target).contains(&wanted) {
        Value::TRUE
    } else {
        Value::FALSE
    }
    .to_bits()
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
        for position in 1..argc as usize {
            // SAFETY: as above.
            let source = unsafe { argument(argc, argv, position) };
            for name in own_keys(source) {
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

/// Whether an object refuses new properties.
///
/// **Absent means extensible**, so an object nobody has frozen carries nothing. The flag is a
/// hidden property for the same reason a date's time is (D-126): there is nowhere else to put
/// one that `Object.keys` will not find.
const NOT_EXTENSIBLE: &str = "__sealed";

/// Whether `object` still accepts new properties.
fn is_extensible(object: u64) -> bool {
    property_number(object, NOT_EXTENSIBLE).is_none()
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
        own_property(object, &name).is_none_or(|(slot, _)| {
            ready(with_runtime(|runtime| {
                runtime.heap.attributes_of(handle, slot)
            }))
        })
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
        !attributes.writable && !attributes.configurable
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
    _this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    array_of_values(&[])
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
        runtime.build_globals();
        runtime
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

    /// The global named `name`, creating it as a callable object if it is not there.
    fn ensure_global_object(&self, globals: GcRef, name: &str) -> GcRef {
        if let Some(existing) = self.global_object(globals, name) {
            return existing;
        }
        let shape = self.shapes.borrow().root();
        let scope = self.heap.scope();
        // One internal slot, holding the index of the built-in it runs when called. `Object`
        // and `Array` are constructors as well as namespaces, so they need to be callable.
        let object = scope.alloc_with_internals(shape, 0, 1);
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
        let text = self.string(name);
        self.define_named(object.handle(), "name", text);
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
            // function does.
            let prototype = scope.alloc(shape, 0);
            self.define(function.handle(), "prototype", prototype.to_value());
            // The constructor's own name, which `make_error` reads back so that `TypeError`
            // and `RangeError` can be the same code with different bindings. Not enumerable,
            // for the same reason a method's name is not.
            let text = self.string(name);
            self.define_named(function.handle(), "name", text);
            self.define(globals.handle(), name, function.to_value());
        }
        // `Object` and `Array` are functions that also carry methods. Created here rather than
        // in `GLOBAL_NATIVES` because they need properties hung off them, and the constructor
        // they answer to is the same `make_error`-shaped thing: called or `new`ed, it returns
        // an object.
        for (index, (namespace, method, _)) in NAMESPACE_NATIVES.iter().enumerate() {
            let owner = self.ensure_global_object(globals.handle(), namespace);
            let function = self.native_function(NATIVES.len() + GLOBAL_NATIVES.len() + index);
            self.define_method(owner, method, function.to_value());
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
        ] {
            if let (Some(constructor), Some(prototype)) =
                (self.global_object(globals.handle(), name), cell)
            {
                self.define(constructor, "prototype", prototype.to_value());
            }
        }
        // The well-known symbols, as values on `Symbol`. **Present but not yet usable as
        // property keys**: a `PropertyKey` is a string, so `obj[Symbol.iterator]` cannot name
        // one. They exist so a program that reads `Symbol.iterator` gets a symbol rather than
        // `undefined`, which is what most feature tests check — and so that when keys learn
        // about symbols, the values are already the right ones.
        if let Some(symbol) = self.global_object(globals.handle(), "Symbol") {
            for name in [
                "iterator",
                "asyncIterator",
                "hasInstance",
                "toPrimitive",
                "toStringTag",
            ] {
                let value = self.symbol(Some(&format!("Symbol.{name}")));
                self.define_named(symbol, name, value);
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
                self.define(number, name, Value::number(value));
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
                self.define(math, name, Value::number(value));
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
    fn define_method(&self, object: GcRef, name: &str, value: Value) {
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
            self.define_method(prototype, name, method.to_value());
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
            self.define_method(prototype.handle(), name, function.to_value());
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
            self.define_method(prototype.handle(), name, method.to_value());
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
            self.define_method(prototype.handle(), name, method.to_value());
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
            self.define_method(prototype.handle(), name, method.to_value());
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
        for (cell, natives, offset) in [
            (&MAP_PROTOTYPE, MAP_NATIVES, 0),
            (&SET_PROTOTYPE, SET_NATIVES, MAP_NATIVES.len()),
            (
                &SYMBOL_PROTOTYPE,
                SYMBOL_NATIVES,
                MAP_NATIVES.len() + SET_NATIVES.len(),
            ),
            (
                &ARRAY_ITERATOR_PROTOTYPE,
                ARRAY_ITERATOR_NATIVES,
                MAP_NATIVES.len() + SET_NATIVES.len() + SYMBOL_NATIVES.len(),
            ),
            (
                &NUMBER_PROTOTYPE,
                NUMBER_NATIVES,
                MAP_NATIVES.len()
                    + SET_NATIVES.len()
                    + SYMBOL_NATIVES.len()
                    + ARRAY_ITERATOR_NATIVES.len(),
            ),
            (
                &BOOLEAN_PROTOTYPE,
                BOOLEAN_NATIVES,
                MAP_NATIVES.len()
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
                self.define_method(prototype.handle(), name, method.to_value());
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
            self.define_method(prototype.handle(), name, method.to_value());
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
    let Some(handle) = handle_of(object) else {
        return nullish_access(object);
    };
    // SAFETY: the caller promises `key` names `length` readable bytes of UTF-8.
    let Some(name) = (unsafe { key_text(key, length) }) else {
        return Value::UNDEFINED.to_bits();
    };
    let key = PropertyKey::new(&name);

    // **Assigning to an array's `length` resizes it.** `length` is not stored anywhere — it
    // *is* the element count — so writing it has to change the elements rather than add a
    // property. Without this `a.length = 0` silently did nothing, and test262's own
    // `buildString` helper, which empties a scratch array that way each chunk, instead
    // re-sent everything it had accumulated: quadratic growth, and the process killed on
    // memory rather than any error a test could report.
    if name == "length"
        && let Some(count) = with_runtime(|runtime| runtime.heap.element_count(handle))
    {
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

    // **A non-extensible object refuses a property it does not already have.** Silently,
    // outside strict mode — the same rule a non-writable property follows, and the reason
    // `Object.freeze` is worth anything at all.
    if own_property(object, &name).is_none() && !is_extensible(object) {
        return Value::UNDEFINED.to_bits();
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
            runtime.heap.set_deleted(handle, slot.index(), false);
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

    with_runtime(|runtime| {
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
            return Value::number(length).to_bits();
        }
        if name == "length"
            && let Some(count) = runtime.heap.element_count(handle)
        {
            #[expect(
                clippy::cast_precision_loss,
                reason = "an array this long cannot be allocated"
            )]
            let length = count as f64;
            return Value::number(length).to_bits();
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
                .and_then(|cell| runtime.heap.with_text(cell, ToOwned::to_owned));
            if let Some(text) = held {
                let units: Vec<u16> = text.encode_utf16().collect();
                return units.get(index).map_or_else(
                    || Value::UNDEFINED.to_bits(),
                    |unit| new_string(&String::from_utf16_lossy(&[*unit])),
                );
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
                return value.to_bits();
            }
            current = runtime.heap.prototype_of(object);
        }
        Value::UNDEFINED.to_bits()
    })
}

/// How far a property lookup walks before giving up.
///
/// Deep chains are rare and a cycle is illegal, so this is a backstop rather than a budget —
/// reaching it means the heap holds a chain the specification says cannot exist.
const PROTOTYPE_CHAIN_LIMIT: usize = 1000;

/// Allocates the `this` for `new callee(...)`, inheriting from `callee.prototype`.
///
/// This is `OrdinaryCreateFromConstructor`: the prototype link is established here rather than
/// by a separate step that could be omitted, which is why `Op::Construct` is one operation
/// rather than the sequence it stands for.
///
/// A callee with no `prototype` property still yields an object, just one with no prototype.
/// That is wrong for a real constructor and right for the only way to reach it here — a
/// `new` on something that is not a class — and it beats returning nothing at all.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn crisol_construct_this(callee: u64) -> u64 {
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
fn relative_index(value: u64, length: usize, fallback: usize) -> usize {
    let Some(number) = Value::from_bits(value).as_number() else {
        return fallback;
    };
    if number.is_nan() {
        return 0;
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
    index
}

/// How many elements an **array-like** has.
///
/// **Not just an array.** test262 applies the array methods to anything with a `length` and
/// indexed properties — `Array.prototype.filter.call(new String("abc"), …)` is a whole family
/// of its cases — and a method that insisted on real elements answered `undefined` for every
/// one of them. An array answers from its element count, which is why that stays the first
/// question.
fn indexed_length(value: u64) -> usize {
    if let Some((_, length)) = elements_of(value) {
        return length;
    }
    let asked = property_number(value, "length").unwrap_or(0.0);
    if !asked.is_finite() || asked <= 0.0 {
        return 0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to a length no array can exceed"
    )]
    let length = asked.min(f64::from(u32::MAX)) as usize;
    length
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
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let length = indexed_length(this_value);
        // SAFETY: the convention guarantees `argc` readable values at `argv`.
        let callback = unsafe { argument(argc, argv, 0) };

        with_new_array(length, |result| {
            for index in 0..length {
                let element = indexed_get(this_value, index);
                let mapped = call_value(
                    callback,
                    this_value,
                    &[element, index_value(index), this_value],
                );
                with_runtime(|runtime| {
                    runtime
                        .heap
                        .set_element(result, index, Value::from_bits(mapped))
                });
            }
            result.to_value().to_bits()
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
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let length = indexed_length(this_value);
        // SAFETY: as above.
        let callback = unsafe { argument(argc, argv, 0) };

        // Allocated at full length and shortened after, because the result is rooted through the
        // whole loop and the count is not known until the end.
        with_new_array(length, |result| {
            let mut kept = 0;
            for index in 0..length {
                let element = indexed_get(this_value, index);
                let verdict = call_value(
                    callback,
                    this_value,
                    &[element, index_value(index), this_value],
                );
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
}

/// `Array.prototype.forEach` — the callback for its effects, and `undefined`.
extern "C" fn array_for_each(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let length = indexed_length(this_value);
        // SAFETY: as above.
        let callback = unsafe { argument(argc, argv, 0) };
        for index in 0..length {
            let element = indexed_get(this_value, index);
            call_value(
                callback,
                this_value,
                &[element, index_value(index), this_value],
            );
        }
        Value::UNDEFINED.to_bits()
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
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        let Some((array, length)) = elements_of(this_value) else {
            return Value::UNDEFINED.to_bits();
        };
        // SAFETY: as above.
        let callback = unsafe { argument(argc, argv, 0) };

        let (mut accumulator, start) = if argc >= 2 {
            // SAFETY: as above.
            (unsafe { argument(argc, argv, 1) }, 0)
        } else if length == 0 {
            return Value::UNDEFINED.to_bits();
        } else {
            (element_at(array, 0), 1)
        };

        for index in start..length {
            let element = element_at(array, index);
            accumulator = call_value(
                callback,
                Value::UNDEFINED.to_bits(),
                &[accumulator, element, index_value(index), this_value],
            );
        }
        accumulator
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
        let Some((array, length)) = elements_of(this_value) else {
            return Value::UNDEFINED.to_bits();
        };
        let mut at = length;
        for position in 0..argc as usize {
            // SAFETY: as above.
            let value = unsafe { argument(argc, argv, position) };
            with_runtime(|runtime| runtime.heap.set_element(array, at, Value::from_bits(value)));
            at += 1;
        }
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
    let length = indexed_length(this_value);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let wanted = Value::from_bits(unsafe { argument(argc, argv, 0) });
    for index in (0..length).rev() {
        if same_value(Value::from_bits(indexed_get(this_value, index)), wanted) {
            return index_value(index);
        }
    }
    Value::number(-1.0).to_bits()
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
    let length = indexed_length(this_value);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let wanted = Value::from_bits(unsafe { argument(argc, argv, 0) });
    let seeking_nan = wanted.as_number().is_some_and(f64::is_nan);
    for index in 0..length {
        let element = Value::from_bits(indexed_get(this_value, index));
        let found = if seeking_nan {
            element.as_number().is_some_and(f64::is_nan)
        } else {
            same_value(element, wanted)
        };
        if found {
            return Value::TRUE.to_bits();
        }
    }
    Value::FALSE.to_bits()
}

/// `Array.prototype.join`.
extern "C" fn array_join(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    argc: u64,
    argv: *const u64,
) -> u64 {
    let length = indexed_length(this_value);
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
    let length = indexed_length(this_value);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let start = relative_index(unsafe { argument(argc, argv, 0) }, length, 0);
    // SAFETY: as above.
    let end = relative_index(unsafe { argument(argc, argv, 1) }, length, length);
    let taken = end.saturating_sub(start);

    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
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
extern "C" fn array_reverse(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    for index in 0..length / 2 {
        let mirror = length - 1 - index;
        let left = element_at(array, index);
        let right = element_at(array, mirror);
        with_runtime(|runtime| {
            runtime
                .heap
                .set_element(array, index, Value::from_bits(right));
            runtime
                .heap
                .set_element(array, mirror, Value::from_bits(left));
        });
    }
    this_value
}

/// `Array.prototype.pop`.
extern "C" fn array_pop(
    _closure: u64,
    this_value: u64,
    _new_target: u64,
    _argc: u64,
    _argv: *const u64,
) -> u64 {
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    if length == 0 {
        return Value::UNDEFINED.to_bits();
    }
    let last = element_at(array, length - 1);
    with_runtime(|runtime| runtime.heap.truncate_elements(array, length - 1));
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
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    if length == 0 {
        return Value::UNDEFINED.to_bits();
    }
    let first = element_at(array, 0);
    with_runtime(|runtime| {
        for index in 1..length {
            let moved = runtime
                .heap
                .element(array, index)
                .unwrap_or(Value::UNDEFINED);
            runtime.heap.set_element(array, index - 1, moved);
        }
        runtime.heap.truncate_elements(array, length - 1);
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
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    let added = argc as usize;
    if added == 0 {
        return index_value(length);
    }
    with_runtime(|runtime| {
        // Grown first, then moved from the back, so nothing is overwritten before it has moved.
        runtime
            .heap
            .set_element(array, length + added - 1, Value::UNDEFINED);
        for index in (0..length).rev() {
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
    index_value(length + added)
}

/// `find` and `findIndex`, which differ only in what they answer with.
fn find_with(this_value: u64, argc: u64, argv: *const u64, want_index: bool) -> u64 {
    let length = indexed_length(this_value);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        for index in 0..length {
            let element = indexed_get(this_value, index);
            let verdict = call_value(
                callback,
                this_value,
                &[element, index_value(index), this_value],
            );
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
fn quantify(this_value: u64, argc: u64, argv: *const u64, want_all: bool) -> u64 {
    let length = indexed_length(this_value);
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let callback = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let live = unsafe { live_values(this_value, argc, argv) };
    with_rooted(&live, || {
        for index in 0..length {
            let element = indexed_get(this_value, index);
            let verdict = call_value(
                callback,
                this_value,
                &[element, index_value(index), this_value],
            );
            if is_truthy(Value::from_bits(verdict)) != want_all {
                return if want_all { Value::FALSE } else { Value::TRUE }.to_bits();
            }
        }
        // **Empty is `true` for `every` and `false` for `some`**, which follows from each
        // stopping on the opposite answer and neither ever stopping.
        if want_all { Value::TRUE } else { Value::FALSE }.to_bits()
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
    let Some((array, length)) = elements_of(this_value) else {
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: the convention guarantees `argc` readable values at `argv`.
    let value = unsafe { argument(argc, argv, 0) };
    // SAFETY: as above.
    let start = relative_index(unsafe { argument(argc, argv, 1) }, length, 0);
    // SAFETY: as above.
    let end = relative_index(unsafe { argument(argc, argv, 2) }, length, length);
    with_runtime(|runtime| {
        for index in start..end {
            runtime
                .heap
                .set_element(array, index, Value::from_bits(value));
        }
    });
    this_value
}

/// `Array.prototype.indexOf`, by `===` on numbers and by identity otherwise.
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
        let length = indexed_length(this_value);
        // SAFETY: as above.
        let wanted = Value::from_bits(unsafe { argument(argc, argv, 0) });
        for index in 0..length {
            let element = Value::from_bits(indexed_get(this_value, index));
            // `indexOf` uses strict equality, so `NaN` is never found — `[NaN].indexOf(NaN)` is
            // `-1`. Comparing the numbers rather than the bits is what gets that right.
            let same = match (element.as_number(), wanted.as_number()) {
                (Some(left), Some(right)) => left == right,
                (None, None) => element == wanted,
                _ => false,
            };
            if same {
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
        return BOOLEAN_NATIVES
            .get(native.wrapping_sub(offset))
            .map_or(fallback, |(_, function)| *function as *const u8);
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
            // A symbol has no spelling, and `None` reads as a missing property — which beats
            // naming the wrong one.
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
    let text = name.as_str().to_owned();
    // SAFETY: `text` is a live Rust string, so its pointer and length describe readable UTF-8.
    unsafe { crisol_property_load(object, text.as_ptr(), text.len() as u64) }
}

/// `object[key] = value`.
#[unsafe(no_mangle)]
pub extern "C" fn crisol_computed_store(object: u64, key: u64, value: u64) -> u64 {
    let Some(handle) = handle_of(object) else {
        return nullish_access(object);
    };
    let key = Value::from_bits(key);

    let stored = with_runtime(|runtime| {
        as_index(key).is_some_and(|index| {
            runtime
                .heap
                .set_element(handle, index, Value::from_bits(value))
        })
    });
    if stored {
        return Value::UNDEFINED.to_bits();
    }
    let Some(name) = key_of(key) else {
        return Value::UNDEFINED.to_bits();
    };
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
        // Stored one at a time. Creating both and then storing them leaves the first reachable
        // only from a Rust local while the second allocates — and under stress that allocation
        // collects it, which is how the message came back unreadable.
        let text = new_string(message);
        with_runtime(|runtime| runtime.define(handle, "message", Value::from_bits(text)));
        let name = new_string(kind);
        with_runtime(|runtime| runtime.define(handle, "name", Value::from_bits(name)));
    });
    crisol_throw(error)
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
    let key_value = Value::from_bits(key);

    // An element is removed by shortening the array when it is the last one, and otherwise left
    // as `undefined` — a hole and an `undefined` element differ (D-64) and nothing here can say
    // which it is yet.
    if let Some(index) = as_index(key_value) {
        let handled = with_runtime(|runtime| {
            let Some(count) = runtime.heap.element_count(handle) else {
                return false;
            };
            if index >= count {
                return true;
            }
            if index + 1 == count {
                runtime.heap.truncate_elements(handle, index);
            } else {
                runtime.heap.set_element(handle, index, Value::UNDEFINED);
            }
            true
        });
        if handled {
            return Value::TRUE.to_bits();
        }
    }

    let Some(name) = key_of(key_value) else {
        return Value::TRUE.to_bits();
    };
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
        let mut current = object;
        for _ in 0..PROTOTYPE_CHAIN_LIMIT {
            let Some(handle) = handle_of(current) else {
                break;
            };
            for name in enumerable_keys(current) {
                if !names.contains(&name) {
                    names.push(name);
                }
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
    raise("value is not iterable", "TypeError")
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
        .is_some_and(|handle| with_runtime(|runtime| runtime.heap.internal(handle, 0).is_some()))
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
    if let Some(index) = as_index(Value::from_bits(key))
        && let Some((_, length)) = elements_of(object)
    {
        return boolean(index < length).to_bits();
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
