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
    "crisol_create_object",
    "crisol_property_store",
    "crisol_property_load",
    "crisol_closure_capture",
    "crisol_create_closure",
    "crisol_closure_set_capture",
    "crisol_closure_code",
    "crisol_not_a_function",
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
    value.as_number().unwrap_or(f64::NAN)
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
    // `+` concatenates when either operand is a string after `ToPrimitive`. Strings need the
    // runtime's string table, which does not exist yet, so a string operand gives `NaN` rather
    // than a wrong concatenation or a silent numeric answer.
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
        crisol_value::Kind::Number => match value.as_number() {
            Some(number) if number.is_nan() => println!("NaN"),
            Some(number) if number.is_infinite() && number > 0.0 => println!("Infinity"),
            Some(number) if number.is_infinite() => println!("-Infinity"),
            // Whole numbers print without a decimal point, as JavaScript does — `1`, not `1.0`.
            Some(number) if number.fract() == 0.0 && number.abs() < 1e21 => {
                println!("{number:.0}");
            }
            Some(number) => println!("{number}"),
            None => println!("NaN"),
        },
        crisol_value::Kind::String | crisol_value::Kind::Symbol | crisol_value::Kind::Object => {
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
        unsafe { compiled_roots(FRAME_LIMIT) }
            .into_iter()
            .filter_map(|value| value.as_address().map(GcRef::from_address))
            .collect()
    }));
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
        Self {
            heap,
            shapes: RefCell::new(Shapes::new()),
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
        scope.alloc(shape, 0).to_value().to_bits()
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
) {
    let Some(handle) = handle_of(object) else {
        return;
    };
    // SAFETY: the caller promises `key` names `length` readable bytes of UTF-8.
    let Some(name) = (unsafe { key_text(key, length) }) else {
        return;
    };
    let key = PropertyKey::new(&name);

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
        }
        runtime
            .heap
            .set(handle, slot.index(), Value::from_bits(value));
    });
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
        return Value::UNDEFINED.to_bits();
    };
    // SAFETY: as above.
    let Some(name) = (unsafe { key_text(key, length) }) else {
        return Value::UNDEFINED.to_bits();
    };
    let key = PropertyKey::new(&name);

    with_runtime(|runtime| {
        let Some(shape) = runtime.heap.shape_of(handle) else {
            return Value::UNDEFINED.to_bits();
        };
        let slot = runtime.shapes.borrow().lookup(shape, &key);
        slot.and_then(|slot| runtime.heap.get(handle, slot.index()))
            .unwrap_or(Value::UNDEFINED)
            .to_bits()
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
            .get(handle, index + CLOSURE_CAPTURES_AT)
            .unwrap_or(Value::UNDEFINED)
            .to_bits()
    })
}

/// Where a closure's captures begin, in slots.
///
/// Slot zero holds which function the closure runs, so captures start at one. The index is
/// stored rather than the code address because a code address does not fit a NaN-boxed value's
/// 48-bit payload on every platform, and a heap slot holds a `Value`.
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
        let closure = scope.alloc(shape, captures + CLOSURE_CAPTURES_AT as usize);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a function index is far below 2^53"
        )]
        let index = Value::number(function as f64);
        runtime.heap.set(closure.handle(), 0, index);
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
            .set(handle, index + CLOSURE_CAPTURES_AT, Value::from_bits(value));
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
    Value::UNDEFINED.to_bits()
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
    let index = with_runtime(|runtime| runtime.heap.get(handle, 0).and_then(|v| v.as_number()));
    let Some(index) = index else {
        return fallback;
    };
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
