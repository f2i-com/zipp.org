#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, ReactionPair, Reactions,
};
use crate::value::Value;


/// High bit of a heap index marks a "string constant pending interning" slot
/// in a `LoadConst` Value (see `resolve_const`). Real heap indices never set
/// this bit (the heap would need 2^31 objects).
pub const STRING_CONST_BIT: u32 = 0x8000_0000;

/// Per-function: which global slot the function's name binds to, if any. The
/// compiler stores it in `param_count`'s sibling — but to keep `FuncProto`
/// simple we encode it via a convention: a function whose name is hoisted to a
/// global has that slot recorded in a side table. For v1 the compiler sets it
/// through `FuncProto`-adjacent metadata; we read it here.
pub(crate) fn function_global_slot(f: &crate::bytecode::FuncProto) -> Option<u32> {
    f.name_global
}

/// Maximum native self-recursion depth before the JIT self-call helper deopts
/// to the interpreter (which continues on its EXPLICIT frame stack and enforces
/// MAX_FRAMES → catchable RangeError). This MUST stay well below what the native
/// Rust stack can hold, because each native self-call nests
/// `jit_self_call → JitFn::run → call helper → jit_self_call_impl → JitFn::run`
/// on the OS stack. 256 levels is safe on a default stack and is plenty to keep
/// realistic recursion (fib, etc.) native; deeper legal recursion transparently
/// continues on the interpreter (correct, just not JIT-accelerated past 256),
/// and runaway recursion deopts → interpreter → RangeError, never a segfault.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_SELF_RECURSE_MAX: u32 = 256;

/// Public mirror of `JIT_SELF_RECURSE_MAX` for codegen's inline depth guard (the
/// native fast path compares `vm.jit_recurse_depth` against this before a direct
/// recursive call), kept identical so the inline guard and the slow path agree.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_SELF_RECURSE_MAX_PUB: u32 = JIT_SELF_RECURSE_MAX;

/// Byte offset of `jit_recurse_depth` within `Vm`, for the JIT's inline
/// native→native self-call: the compiled code reads/bumps the counter directly
/// through the `vm` pointer (rdi) rather than crossing into Rust per recursive
/// call. Computed at compile time (verified to match the live field address
/// during bring-up).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_RECURSE_DEPTH_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, jit_recurse_depth);

/// Win64 helper for the slow/finish path of the JIT's inline native→native
/// self-call (see `jit_self_call_at_impl`). The native fast path tracks register
/// windows by raw pointer, so it passes its window base EXPLICITLY in
/// `caller_base_ptr` (the native `rbx`). `packed` carries `func_id` in the low 24
/// bits and `argc` in the high 8. Returns the result bits or `SELF_CALL_DEOPT`
/// (the activation threw — `pending_throw` is set, the native chain unwinds, and
/// the top-level interpreter re-raises it). ABI: rcx=vm, rdx=caller_base_ptr,
/// r8=args_ptr, r9=packed.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `caller_base_ptr` is the caller's window base
/// within `vm.regs`; `args` points to `argc` valid `Value` bits.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_self_call_at(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    args: *const u64,
    packed: u32,
) -> u64 {
    let func_id = packed & 0x00FF_FFFF;
    let argc = (packed >> 24) as usize;
    // Catch Rust panics at the FFI boundary (UB to unwind across `extern`).
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_self_call_at_impl(func_id, caller_base_ptr, args, argc)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Depth cap for native-region → JS calls (`jit_call_method_ic` / `jit_call_ic`).
/// Each level nests `try_run_osr → native region → helper → run_loop` on the
/// Rust stack (unlike ordinary interpreter calls, which are flat frames), so
/// recursion THROUGH region call sites must be bounded well below what the OS
/// stack holds. Past the cap the helper deopts: the interpreter executes the
/// call as a flat frame (correct, just not region-accelerated at that depth).
/// 64 is comfortably safe (each level is a few KB) and deeper-than-64 recursion
/// through a hot loop's call site is rare.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_REGION_CALL_MAX: u32 = 64;

/// Win64 helper for a generic `obj.m(args…)` (`CallMethod`) inside a compiled
/// OSR region. Consults the SAME per-site inline cache the interpreter uses
/// (`ic_call_method`, keyed by `(func_id, ip)`), frame-calls the resolved plain
/// user function to completion, and returns the result Value bits. Returns
/// `SELF_CALL_DEOPT` when nothing has happened yet (IC miss / megamorphic /
/// native callee / depth cap → the interpreter re-executes this op), or
/// `CALL_THREW` when the call ran and threw (`pending_throw` is set; the OSR
/// caller unwinds — the op must NOT re-execute). ABI: rcx=vm, rdx=caller window
/// base (the region's rbx), r8=(func_id<<32)|ip, r9=(name<<32)|(obj<<16)|arg_base,
/// 5th stack arg = argc.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `caller_base_ptr` is the running frame's window
/// base within `vm.regs` (whose buffer is pinned — `reserve_jit_regs`).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_call_method_ic(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    packed_fip: u64,
    packed_args: u64,
    argc: u32,
) -> u64 {
    // Catch Rust panics at the FFI boundary (UB to unwind across `extern`).
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_region_call_impl(caller_base_ptr, packed_fip, packed_args, argc as u16, true)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for the Tier C CROSS-CALL fast path (B83): a compiled body's
/// `Call` site whose live callee is itself a Tier-C-compiled plain function is
/// dispatched native→native — no `ic_call` probe, no `setup_call` frame push,
/// no nested `run_loop` on the clean path (see `jit_cross_call_impl`). Returns
/// the result bits, `SELF_CALL_DEOPT` (not eligible — the emitted site falls
/// through to the unchanged `call_ic` helper), or `CALL_THREW`. ABI: rcx=vm,
/// rdx=caller window base (rbx), r8=&args[0] (the staged contiguous args),
/// r9=(caller_reg_count<<16)|argc, 5th stack arg = the callee's Value bits.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `caller_base_ptr` is the running frame's window
/// base within `vm.regs` (pinned buffer); `args` points to `argc` valid Values.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_cross_call(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    args: *const u64,
    packed: u64,
    callee_bits: u64,
) -> u64 {
    // Catch Rust panics at the FFI boundary (UB to unwind across `extern`).
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_cross_call_impl(caller_base_ptr, args, packed, callee_bits)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Guarded intrinsic for the hot
/// `Object.prototype.hasOwnProperty.call(array, numeric_key)` shape.
///
/// Both property reads that lead to the call are proved pristine before the
/// array probe runs: the method receiver must be the native `hasOwnProperty`
/// function with no own `call` shadow and the effective prototype must be the
/// main `%Function.prototype%`, whose own `call` slot must still be the native
/// `%Function.prototype%.call`. Every miss returns `SELF_CALL_DEOPT` before
/// coercion, getters, proxy traps, or any other observable effect, so the
/// emitter can run the unchanged generic CallMethod path.
///
/// ABI: rcx=vm, rdx=callable bits, r8=thisArg bits, r9=key bits.
///
/// # Safety
/// `vm` is the live `Vm`; the remaining operands are raw Value bits from the
/// running frame's register file.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_has_own_call(
    vm: *mut core::ffi::c_void,
    callable_bits: u64,
    this_bits: u64,
    key_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let callable = Value::from_bits(callable_bits);
    if !callable.is_heap()
        || !matches!(
            vm.heap.get(callable.heap_index()),
            HeapObj::Native(id) if *id == native::PROTO_HAS_OWN
        )
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }

    let callable_idx = callable.heap_index();
    if vm
        .fn_props
        .get(&callable_idx)
        .is_some_and(|m| m.pos("call").is_some())
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    if vm
        .proto_of
        .get(&callable_idx)
        .is_some_and(|&p| p != Value::heap(vm.fn_proto))
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }

    let pristine_call = match vm.heap.get(vm.fn_proto) {
        HeapObj::Object(m) => m.pos("call").is_some_and(|slot| {
            !m.attrs[slot].accessor
                && m.vals[slot].is_heap()
                && matches!(
                    vm.heap.get(m.vals[slot].heap_index()),
                    HeapObj::Native(id) if *id == native::FN_CALL
                )
        }),
        _ => false,
    };
    if !pristine_call {
        return crate::codegen::SELF_CALL_DEOPT;
    }

    match vm.has_own_index_fast(Value::from_bits(this_bits), Value::from_bits(key_bits)) {
        Some(answer) => {
            callstats::hasown_hit();
            Value::bool(answer).bits()
        }
        None => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for `s.indexOf(t)` inside a compiled region — a JIT INTRINSIC,
/// the same shape that already puts `charCodeAt` and `.length` at node parity
/// while every other string method pays ~47ns of call plumbing (jit_call_method_ic
/// -> jit_region_call_impl -> try_builtin_method -> dispatch_builtin_method ->
/// string_method) to reach ~5ns of actual work.
///
/// Handles the ASCII/ASCII, one-argument case and returns the Int Value bits of
/// the result; anything else returns the deopt sentinel and the region bails to
/// the interpreter at this ip, which runs the full method unchanged.
///
/// # Safety
/// `vm` is the live `Vm`; the operands are raw Value bits from the reg file.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_str_index_of(
    vm: *mut core::ffi::c_void,
    recv_bits: u64,
    needle_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let (r, n) = (Value::from_bits(recv_bits), Value::from_bits(needle_bits));
    if !r.is_heap() || !n.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // A rope receiver must be materialised before its bytes can be read.
    vm.heap.flatten(r.heap_index());
    match (vm.heap.get(r.heap_index()), vm.heap.get(n.heap_index())) {
        (crate::heap::HeapObj::Str(hay), crate::heap::HeapObj::Str(ned))
            if hay.is_ascii() && ned.is_ascii() =>
        {
            let (hc, nc) = (hay.as_str_lossy(), ned.as_str_lossy());
            let (h, nd): (&str, &str) = (&hc, &nc);
            Value::int(h.find(nd).map_or(-1, |b| b as i32)).bits()
        }
        _ => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for `s.substring(a[, b])` / `s.slice(a[, b])` inside a compiled
/// region — the same JIT-intrinsic shape as `jit_str_index_of`. `mode & 1`
/// selects the two different clamping rules (`slice` counts a negative index
/// from the end and yields "" when start >= end; `substring` clamps negatives to
/// 0 and SWAPS a reversed pair); `mode & 2` means the end argument is absent and
/// therefore defaults to the receiver length.
///
/// Restricted to an ASCII flat receiver with one or two integral Number
/// arguments, where UTF-16 unit offsets are byte offsets; anything else returns
/// the deopt sentinel and the interpreter runs the full method at that ip.
///
/// # Safety
/// `vm` is the live `Vm`; the operands are raw Value bits from the reg file.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_str_substring(
    vm: *mut core::ffi::c_void,
    recv_bits: u64,
    packed_args: u64,
    mode: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let r = Value::from_bits(recv_bits);
    if !r.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let a = Value::from_bits(unsafe { *(packed_args as *const u64) });
    // Int OR an exactly-integral double. Accepting only Int-tagged values was a
    // deopt storm: the memory path deliberately keeps `Mul` off its integer fast
    // path (hot integer multiplies overflow i32), so an ordinary `s.substring(0,
    // n * 2)` hands this a DOUBLE and every call bailed — 150 deopts in
    // markdown-render, enough to evict the region permanently.
    let as_i64 = |v: Value| -> Option<i64> {
        if v.is_int() {
            Some(v.as_int() as i64)
        } else if v.is_number() {
            let d = v.as_f64();
            (d.fract() == 0.0 && d.abs() < 9.007_199_254_740_992e15).then_some(d as i64)
        } else {
            None
        }
    };
    let Some(ax) = as_i64(a) else {
        return crate::codegen::SELF_CALL_DEOPT;
    };
    // Do not even read args[1] for a one-argument call: the bytecode's argument
    // window only promises `argc` initialized slots.
    let bx = if mode & 2 != 0 {
        None
    } else {
        let b = Value::from_bits(unsafe { *((packed_args as *const u64).add(1)) });
        let Some(bx) = as_i64(b) else {
            return crate::codegen::SELF_CALL_DEOPT;
        };
        Some(bx)
    };
    vm.heap.flatten(r.heap_index());
    let len = match vm.heap.get(r.heap_index()) {
        crate::heap::HeapObj::Str(js) if js.is_ascii() => js.units() as i64,
        _ => return crate::codegen::SELF_CALL_DEOPT,
    };
    let (mut x, mut y) = (ax, bx.unwrap_or(len));
    if mode & 1 != 0 {
        // slice: negative counts from the end, then clamp; empty if start >= end.
        if x < 0 { x += len; }
        if y < 0 { y += len; }
        x = x.clamp(0, len);
        y = y.clamp(0, len);
        if x >= y {
            return Value::heap(crate::heap::INTERN_EMPTY).bits();
        }
    } else {
        // substring: clamp both to [0, len], then order them.
        x = x.clamp(0, len);
        y = y.clamp(0, len);
        if x > y {
            core::mem::swap(&mut x, &mut y);
        }
    }
    vm.ascii_slice_value(r.heap_index(), x as usize..y as usize).bits()
}

/// Win64 helper for the 1-argument Map/Set lookups — `m.get(k)`, `m.has(k)`,
/// `s.has(v)`. `op` selects: 0 = Map.get, 1 = Map.has, 2 = Set.has.
///
/// These exist to unblock COMPILATION, not just to be fast. map-set-heavy
/// compiled nothing at all: the call-mix gate declines a region whose calls
/// always fall back to the generic helper, so all eight of its loop regions were
/// rejected and the bench ran fully interpreted (JIT 890ms == interpreter 876ms).
/// A method only earns a place on the gate's whitelist once it has a dedicated
/// helper, which is what this is.
///
/// Lookup itself was already O(1) (`coll_find` builds a `CollIndex` past a
/// threshold); what this removes is the dispatch chain, exactly as for the
/// string intrinsics. Anything that is not the expected collection kind returns
/// the deopt sentinel and the interpreter runs the real method.
///
/// # Safety
/// `vm` is the live `Vm`; operands are raw Value bits from the reg file.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_coll_lookup(
    vm: *mut core::ffi::c_void,
    recv_bits: u64,
    key_bits: u64,
    op: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let recv = Value::from_bits(recv_bits);
    if !recv.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let idx = recv.heap_index();
    let want_map = op == 0 || op == 1;
    let kind_ok = match vm.heap.get(idx) {
        crate::heap::HeapObj::Map { .. } => want_map,
        crate::heap::HeapObj::Set(_) => !want_map,
        _ => false,
    };
    if !kind_ok {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let found = vm.coll_find(idx, Value::from_bits(key_bits));
    match op {
        0 => match found {
            Some(i) => match vm.heap.get(idx) {
                crate::heap::HeapObj::Map { vals, .. } => vals[i].bits(),
                _ => Value::UNDEFINED.bits(),
            },
            None => Value::UNDEFINED.bits(),
        },
        _ => Value::bool(found.is_some()).bits(),
    }
}

/// Win64 helper for a generic `f(args…)` (`Call`) inside a compiled OSR region:
/// the plain-call sibling of [`jit_call_method_ic`] (consults `ic_call`,
/// `this = undefined`). r9 = (callee_reg<<16)|arg_base; same protocol otherwise.
///
/// # Safety
/// As [`jit_call_method_ic`].
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_call_ic(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    packed_fip: u64,
    packed_args: u64,
    argc: u32,
) -> u64 {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_region_call_impl(caller_base_ptr, packed_fip, packed_args, argc as u16, false)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper: full `===` for a region `Eq`/`Ne` whose operands are
/// non-interned heap values (multi-char strings, objects, BigInts…). Read-only
/// and side-effect-free — never deopts, never runs user code. Returns 0/1.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_strict_eq(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    let vm = unsafe { &*(vm as *const Vm) };
    crate::vm::collections::strict_eq(
        &vm.heap,
        Value::from_bits(a_bits),
        Value::from_bits(b_bits),
    ) as u64
}

/// Win64 helper: full JS truthiness for a region `Not` / `JumpIfFalse/True`
/// whose operand isn't Int/Bool (doubles incl. NaN/±0, heap values incl. empty
/// strings and [[IsHTMLDDA]], null/undefined). Read-only; returns 0/1.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_truthy(vm: *mut core::ffi::c_void, bits: u64) -> u64 {
    let vm = unsafe { &*(vm as *const Vm) };
    vm.truthy(Value::from_bits(bits)) as u64
}

/// Win64 helper: the `PROP_VIA_IC` continuation for a region `GetProp` whose
/// miss helper found an accessor or a class-instance receiver. Consults the
/// interpreter's per-site property IC (`ic_get_prop`) and frame-calls a plain
/// getter to completion. Returns the value bits, `SELF_CALL_DEOPT` (no IC
/// resolution — the interpreter re-executes the op), or `CALL_THREW`. The
/// calling region re-derives r13/r14 afterwards (user code may have run).
/// ABI: rcx=vm, rdx=caller window base, r8=(func_id<<32)|ip, r9=(name<<32)|obj_reg.
///
/// # Safety
/// As [`jit_call_method_ic`].
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_prop_slow(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    packed_fip: u64,
    packed2: u64,
) -> u64 {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_prop_slow_impl(caller_base_ptr, packed_fip, packed2, false)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// The `SetProp` sibling of [`jit_get_prop_slow`] (setter frame call; returns
/// 0 when the store completed). r9=(name<<32)|(obj_reg<<16)|val_reg.
///
/// # Safety
/// As [`jit_call_method_ic`].
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_prop_slow(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    packed_fip: u64,
    packed2: u64,
) -> u64 {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_prop_slow_impl(caller_base_ptr, packed_fip, packed2, true)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper: an ACCESSOR-way HIT for a region `GetProp` (B114). The probe
/// matched a way tagged `IC_ACC_TAG` — receiver identity, receiver version and
/// every hop version are live — so the resolution is KNOWN to be an accessor:
/// dispatch the getter directly, skipping `jit_get_prop_miss`'s 8-way-miss +
/// rediscovery round trip. `entry` (5th argument, on the stack) is the matched
/// way's address inside `vm.jit.ic_table`. Returns the value bits,
/// `SELF_CALL_DEOPT`, or `CALL_THREW`; the calling region re-derives r13/r14
/// afterwards (user code may have run). Other args as [`jit_get_prop_slow`].
///
/// # Safety
/// As [`jit_call_method_ic`]; `entry` points at a live `IcEntry` of
/// `vm.jit.ic_table` (the probe's way cursor).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_prop_acc(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    packed_fip: u64,
    packed2: u64,
    entry: *const crate::codegen::IcEntry,
) -> u64 {
    icstats::acc_hit(false);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_prop_acc_impl(caller_base_ptr, packed_fip, packed2, entry, false)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// The `SetProp` sibling of [`jit_get_prop_acc`] (setter dispatch; returns 0
/// when the store completed). r9 = (name<<32)|(obj_reg<<16)|val_reg.
///
/// # Safety
/// As [`jit_get_prop_acc`].
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_prop_acc(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    packed_fip: u64,
    packed2: u64,
    entry: *const crate::codegen::IcEntry,
) -> u64 {
    icstats::acc_hit(true);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_prop_acc_impl(caller_base_ptr, packed_fip, packed2, entry, true)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper: the INLINE-CACHE MISS path for a JIT'd `GetProp`. The native
/// fast path (identity + version check, direct `vals[slot]` read) only calls this
/// when its cache misses. Looks up `obj.<key>`, and on the fast-path-eligible case
/// (a plain Object that HAS the key) fills inline-cache slot `site` with
/// `(obj_bits, vals.as_ptr(), version, slot)` so subsequent accesses are call-free.
/// Returns the property bits, or `SELF_CALL_DEOPT` (non-Object → interpreter
/// re-executes at this ip; arrays/strings/`.length`/null/undefined handled there).
/// A missing key on an Object returns `undefined` WITHOUT caching (rare).
/// `packed = (func_id << 32) | name_idx`.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
/// Win64 helper for a JIT'd dense-array element read `a[i]` (`GetIndex`).
/// Returns the element's Value bits; `undefined` bits for an in-bounds-checks-fail
/// (negative or `>= len`) index, matching JS `a[oob] === undefined`; or
/// `SELF_CALL_DEOPT` for a non-array receiver or a non-int key (string indexing,
/// `arr["foo"]`, etc.) so the interpreter re-executes this op. Read-only — no
/// caching needed (a dense array's element address is a direct `vals[i]`).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_index(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    key_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    let key = Value::from_bits(key_bits);
    if !arr.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: read-only view; the running region holds no conflicting borrow.
    let vm = unsafe { &*(vm as *const Vm) };
    // ── plain-object computed read: `o[k]` with a flat string key ───────────
    // Mirrors the interpreter's fast path in `vm/indexing_date.rs` EXACTLY.
    // Without it every non-numeric key deopted, so `o[k]` was never compiled AND
    // the deopts evicted the whole enclosing region — an identical arithmetic
    // loop measured 12.0 ns/op with `o.a` and 82.5 ns/op with `o[k]`, and the
    // penalty grew with the loop body, which is the signature of collateral
    // eviction rather than a slow read.
    //
    // Own DATA property only. Accessors, misses (prototype/class chain),
    // uninitialised (TDZ) slots, the slot-backed global object and module /
    // deferred namespaces all keep deopting — they have live bindings or run
    // user code, and parity with the interpreter is what makes this sound.
    if key.is_heap() {
        let oidx = arr.heap_index();
        if !(oidx == vm.global_this && vm.global_this != 0)
            && !(!vm.module_namespaces.is_empty() && vm.module_namespaces.contains_key(&oidx))
            && !(!vm.deferred_ns_state.is_empty() && vm.deferred_ns_state.contains_key(&oidx))
        {
            if let Some(std::borrow::Cow::Borrowed(b)) = vm.heap.str_wtf8_cow(key.heap_index()) {
                if let (Ok(k), HeapObj::Object(m)) = (std::str::from_utf8(b), vm.heap.get(oidx)) {
                    if let Some(i) = m.pos(k) {
                        if !m.attrs[i].accessor {
                            let v = m.vals[i];
                            if !v.is_uninitialized() {
                                return v.bits();
                            }
                        }
                    }
                }
            }
        }
    }
    // Beyond the plain-object case above, only a numeric key is handled here; a
    // string/other key deopts so the interpreter applies full semantics.
    if !key.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // A TypedArray element read mirrors the interpreter's get_index TA arm
    // EXACTLY (which never consults arr_props for element keys): a numeric
    // integer index → ta_element_get semantics (undefined for OOB/detached);
    // a fractional/huge numeric key or a BigInt element kind → interpreter.
    if matches!(vm.heap.get(arr.heap_index()), HeapObj::TypedArray { .. }) {
        return match array_index(key) {
            Some(i) => ta_fast_get_bits(vm, arr.heap_index(), i)
                .unwrap_or(crate::codegen::SELF_CALL_DEOPT),
            None => crate::codegen::SELF_CALL_DEOPT,
        };
    }
    // An array whose side table can shadow an ELEMENT — a defineProperty'd index
    // whose value or accessor lives in arr_props, a sparse overlay, or an
    // integrity level — deopts so the interpreter's override-aware get_index runs
    // (keeps JIT/interpreter parity). Named properties that cannot name an element
    // do NOT disqualify it: a RegExp match result carries `index`/`input`/`groups`
    // and used to deopt every single `m[i]` because of them.
    if arr.is_heap() && vm.array_elements_overlaid(arr.heap_index()) {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match vm.heap.get(arr.heap_index()) {
        HeapObj::Array(items) => match array_index(key) {
            // In range and present → the element. A HOLE must NOT be returned (it is
            // an internal sentinel): deopt so the interpreter's get_index applies the
            // absent-index / prototype semantics.
            Some(i) if i < items.len() => {
                if items[i].is_hole() {
                    crate::codegen::SELF_CALL_DEOPT
                } else {
                    items[i].bits()
                }
            }
            // Out of range / negative / non-integral. `undefined` is only right
            // when nothing up the chain can supply that index — this returned it
            // unconditionally, so with `Array.prototype[5] = "P"` a JIT'd `a[5]`
            // read `undefined` while the interpreter and node both read `"P"`.
            // Same guard the `i in a` inline uses: the protector flag (set the
            // moment an integer-like key is defined on Array/Object.prototype)
            // plus "no setPrototypeOf'd custom prototype".
            _ => {
                if vm.array_proto_has_index || vm.proto_of.contains_key(&arr.heap_index()) {
                    crate::codegen::SELF_CALL_DEOPT
                } else {
                    Value::UNDEFINED.bits()
                }
            }
        },
        // Flat ASCII string `s[i]`: mirror the interpreter's get_index Str path
        // EXACTLY (the ASCII branch). The i-th unit is the i-th byte, and a
        // single ASCII char is interned at heap index == its byte (Heap::new),
        // so the result is that interned slot. In range →
        // that slot; out of range → undefined. Only the O(1)-and-identical
        // flat-ASCII case is handled; a non-ASCII string (unit-walk) or a rope
        // `Cons` (must flatten first, a &mut op) deopts to the interpreter. A
        // negative/fractional/non-integer key (`array_index` → None) also defers
        // (the interpreter handles `s["length"]`, methods, etc.).
        HeapObj::Str(s) if s.is_ascii() => match array_index(key) {
            Some(i) => match s.as_bytes().get(i) {
                Some(&b) => Value::heap(b as u32).bits(),
                None => Value::UNDEFINED.bits(),
            },
            None => crate::codegen::SELF_CALL_DEOPT,
        },
        _ => crate::codegen::SELF_CALL_DEOPT, // non-ASCII str / rope / other → interpreter
    }
}

/// Win64 helper for a JIT'd computed write `a[i] = v` (`SetIndex`). Dense arrays
/// and numeric TypedArray writes stay in their narrow fast paths. Ordinary
/// objects also update an existing writable string-key slot in place. All
/// unsupported receivers/keys return `SELF_CALL_DEOPT`. Reads the live heap
/// fresh each call, so no cached pointer survives an Array grow.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_index(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    key_bits: u64,
    val_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    let key = Value::from_bits(key_bits);
    if !arr.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: exclusive view; the running region holds no conflicting borrow and
    // pins only the register file (not the array's Vec, which may reallocate).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    // Nursery barrier + B6 oracle: NURSERY_DESIGN.md §1 case 2, the JIT
    // element-store route. There is NO inline region element store for a
    // dense Array (`region_mem.rs` deliberately routes every Array SetIndex
    // through this helper — growth/holes/length exotica), and pinned
    // TypedArray inline stores are numbers only, so this call IS the barrier
    // for JIT'd `a[i] = v`.
    vm.store_barrier(crate::heap::gcoracle::JIT_SET_INDEX, arr.heap_index(), Value::from_bits(val_bits));
    // ── plain-object computed write: `o[k] = v` overwriting an EXISTING own
    // writable data slot ────────────────────────────────────────────────────
    // Deliberately narrower than the read arm. Only an in-place value store on a
    // slot that already exists is handled, so nothing observable happens: no
    // shape change, no `vals` reallocation (the JIT inline caches address values
    // through `vals_ptr + slot`, so an existing-slot store cannot invalidate
    // them), no prototype involvement (an own data property shadows any
    // inherited setter), and no length/index bookkeeping.
    //
    // Everything else keeps deopting — a new key (shape change), an accessor
    // (runs user code), a non-writable slot, an uninitialised TDZ slot, the
    // slot-backed global, and module/deferred namespaces (live bindings).
    if key.is_heap() {
        let oidx = arr.heap_index();
        if !(oidx == vm.global_this && vm.global_this != 0)
            && !(!vm.module_namespaces.is_empty() && vm.module_namespaces.contains_key(&oidx))
            && !(!vm.deferred_ns_state.is_empty() && vm.deferred_ns_state.contains_key(&oidx))
            && !vm.arr_props.contains_key(&oidx)
        {
            let mut writable_slot = None;
            if let Some(std::borrow::Cow::Borrowed(bytes)) =
                vm.heap.str_wtf8_cow(key.heap_index())
            {
                if let Ok(k) = std::str::from_utf8(bytes) {
                    if let HeapObj::Object(m) = vm.heap.get(oidx) {
                        if let Some(i) = m.pos(k) {
                            let a = m.attrs[i];
                            if !a.accessor && a.writable && !m.vals[i].is_uninitialized() {
                                writable_slot = Some(i);
                            }
                        }
                    }
                }
            }
            // End the immutable key/object borrows before updating the slot.
            if let Some(i) = writable_slot {
                if let HeapObj::Object(m) = vm.heap.get_mut(oidx) {
                    m.vals[i] = Value::from_bits(val_bits);
                    return 0;
                }
            }
        }
    }
    if !key.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let i = match array_index(key) {
        Some(i) => i,
        None => return crate::codegen::SELF_CALL_DEOPT, // negative/fractional → interpreter
    };
    // A TypedArray element write with a NUMBER value mirrors ta_element_set
    // (coercion of a plain number is unobservable; bounds re-check, silent OOB
    // no-op). A non-number value (observable ToNumber/ToBigInt), a BigInt
    // element kind, or a fractional/huge key (caught by array_index above for
    // the fractional case → deopt) defers to the interpreter.
    if matches!(vm.heap.get(arr.heap_index()), HeapObj::TypedArray { .. }) {
        return ta_fast_set(vm, arr.heap_index(), i, Value::from_bits(val_bits));
    }
    // A side table may hold a special index (accessor / non-writable / arr_props
    // value) — deopt to the interpreter's override-aware set_prop for parity.
    if arr.is_heap() && vm.arr_props.contains_key(&arr.heap_index()) {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // Creating a NEW own index — an append, or filling a HOLE — is not a store,
    // it is OrdinarySet: it consults the prototype chain for a setter at that
    // index, and an append also has to Set `length`, which can be non-writable.
    // An in-range write over a PRESENT element is neither (an own data element
    // shadows the chain, and `length` does not move), and the `arr_props` test
    // above has already excluded a defineProperty'd override.
    //
    // Without this, `Object.defineProperty(a, "length", {writable: false})` then
    // a hot `a[3] = i` grew the array to length 4 — the interpreter and node
    // leave it at 3 — and an `Array.prototype[1]` setter never fired when the
    // receiver had a hole there. `array_length_nonwritable` is its own side
    // table, so `arr_props.contains_key` above does not see it.
    let creates_new_index = match vm.heap.get(arr.heap_index()) {
        HeapObj::Array(items) => i >= items.len() || items[i].is_hole(),
        _ => false,
    };
    if creates_new_index
        && (vm.array_length_nonwritable.contains(&arr.heap_index())
            || vm.array_proto_has_index
            || vm.proto_of.contains_key(&arr.heap_index()))
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match vm.heap.get_mut(arr.heap_index()) {
        HeapObj::Array(items) => {
            let len = items.len();
            if i < len {
                items[i] = Value::from_bits(val_bits); // in-range store
            } else if i == len {
                items.push(Value::from_bits(val_bits)); // append (grow by one)
            } else {
                // A sparse write (i > len) would resize-with-holes — possibly a
                // huge allocation. Deopt so the INTERPRETER does the resize: its
                // panic on a giant/failed allocation unwinds through normal Rust,
                // not across this `extern "win64"` boundary (which would be UB).
                return crate::codegen::SELF_CALL_DEOPT;
            }
            0
        }
        _ => crate::codegen::SELF_CALL_DEOPT, // non-array → interpreter
    }
}

/// Read-only mirror of `ta_element_get` for the non-BigInt element kinds:
/// returns the element's Value bits (undefined for OOB / detached / shrunk
/// views, exactly like the interpreter), or `None` for a BigInt kind (whose
/// result would allocate — the caller deopts).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn ta_fast_get_bits(vm: &Vm, ta_idx: u32, i: usize) -> Option<u64> {
    let (buffer, kind, byte_offset) = match vm.heap.get(ta_idx) {
        HeapObj::TypedArray { buffer, kind, byte_offset, .. } => (*buffer, *kind, *byte_offset),
        _ => return None,
    };
    if kind >= 9 {
        return None; // BigInt64/BigUint64 → interpreter (allocates)
    }
    if i >= vm.ta_effective_len(ta_idx).unwrap_or(0) {
        return Some(Value::UNDEFINED.bits());
    }
    let size = native::TA_KINDS[kind as usize].1;
    let data = match vm.heap.get(buffer) {
        HeapObj::ArrayBuffer { data, detached } if !*detached => data,
        _ => return Some(Value::UNDEFINED.bits()),
    };
    let off = byte_offset + i * size;
    if off + size > data.len() {
        return Some(Value::UNDEFINED.bits());
    }
    let mut b = [0u8; 8];
    b[..size].copy_from_slice(&data[off..off + size]);
    Some(
        match kind {
            0 => Value::num(b[0] as i8 as f64),
            1 | 2 => Value::num(b[0] as f64),
            3 => Value::num(i16::from_le_bytes([b[0], b[1]]) as f64),
            4 => Value::num(u16::from_le_bytes([b[0], b[1]]) as f64),
            5 => Value::num(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
            6 => Value::num(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
            7 => Value::num(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
            _ => Value::num(f64::from_le_bytes(b)),
        }
        .bits(),
    )
}

/// Mirror of `ta_element_set` for a NUMBER value on a non-BigInt element kind
/// (a plain number's ToNumber coercion is unobservable, so the interpreter's
/// coerce-then-recheck order collapses to this): bounds-check against the
/// effective length, encode, store; OOB / detached is the spec'd silent no-op.
/// Returns 0 (done) or `SELF_CALL_DEOPT` (BigInt kind / non-number value).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn ta_fast_set(vm: &mut Vm, ta_idx: u32, i: usize, val: Value) -> u64 {
    let (buffer, kind, byte_offset) = match vm.heap.get(ta_idx) {
        HeapObj::TypedArray { buffer, kind, byte_offset, .. } => (*buffer, *kind, *byte_offset),
        _ => return crate::codegen::SELF_CALL_DEOPT,
    };
    if kind >= 9 || !val.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let f = if val.is_int() { val.as_int() as f64 } else { val.as_f64() };
    if i >= vm.ta_effective_len(ta_idx).unwrap_or(0) {
        return 0; // silent OOB no-op (matches ta_element_set after coercion)
    }
    let size = native::TA_KINDS[kind as usize].1;
    let bytes = crate::vm::helpers_numeric::ta_encode(kind, f);
    if let HeapObj::ArrayBuffer { data, detached } = vm.heap.get_mut(buffer) {
        if !*detached {
            let off = byte_offset + i * size;
            if off + size <= data.len() {
                data[off..off + size].copy_from_slice(&bytes[..size]);
            }
        }
    }
    0
}

/// A pinned-TypedArray region snapshot: receiver identity bits, raw element
/// base pointer (buffer data + byteOffset) and element count. `repr(C)` with
/// the fixed layout the region's stack slot uses (`obj_bits @0, base @8,
/// len @16`).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[repr(C)]
pub struct TaSnap {
    pub obj_bits: u64,
    pub base: u64,
    pub len: u64,
}

/// Win64 helper: (re)derive a pinned TypedArray's `{obj_bits, base, len}` into
/// a region stack slot. Validates: heap TypedArray of the EXPECTED kind, buffer
/// attached and the view in bounds (`ta_effective_len`); ineligible → all-zero
/// (the region's per-access identity guard then never matches and the access
/// takes the generic-helper fallback — full interpreter semantics, no deopt
/// storm). The base points into `AbData`: a Local Vec's heap allocation (moves
/// only on resize/detach — user code, after which the region re-derives) or a
/// SharedMem allocation (FIXED for the Arc's lifetime; concurrent `grow` moves
/// only the visible length, so a stale `len` is a conservative lower bound).
/// Runs no user code and never allocates.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `out` points to a writable 24-byte slot.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_ta_snapshot(
    vm: *mut core::ffi::c_void,
    ta_bits: u64,
    kind: u32,
    out: *mut TaSnap,
) {
    // SAFETY: exclusive view (mutable only to derive *mut data pointers).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let v = Value::from_bits(ta_bits);
    let snap = (|| {
        if !v.is_heap() {
            return None;
        }
        let idx = v.heap_index();
        if kind == crate::codegen::STR_PIN_KIND as u32 {
            // String pin: snapshot a FLAT ASCII string's byte buffer (ptr + units).
            // ASCII guarantees byte i == UTF-16 unit i, so `charCodeAt(i)` is a
            // direct byte load. A rope / non-ASCII / non-string snapshots zero
            // (the region's identity guard then misses → generic helper, which
            // flattens / handles surrogates). Strings are immutable in this VM, so
            // the (ptr, len) only goes stale if the object is replaced (different
            // bits → guard miss) or GC frees it (only across a user-code helper,
            // after which the region re-snapshots).
            return match vm.heap.get(idx) {
                HeapObj::Str(js) if js.is_ascii() => {
                    let bytes = js.as_bytes();
                    Some(TaSnap {
                        obj_bits: ta_bits,
                        base: bytes.as_ptr() as u64,
                        len: js.units() as u64,
                    })
                }
                _ => None,
            };
        }
        if crate::codegen::is_arr_pin(kind as u8) {
            // Dense-Array pin: base = the `Vec<Value>` storage pointer, len =
            // its element count. DECLINE (→ None → all-zero slot → the region's
            // per-access identity guard misses → generic helper) when the array
            // carries an `arr_props` overlay (a defineProperty'd / sparse-overlay
            // index whose value/accessor is NOT in the dense Vec) or is a mapped
            // `arguments` object (a live index reads a formal's register) — both
            // need the interpreter's override-aware get_index. The base goes
            // stale on any Vec growth/realloc; the region re-derives it after
            // every such op (push / generic SetIndex / user-code helper).
            if vm.arr_props.contains_key(&idx) || vm.arguments_objs.contains_key(&idx) {
                return None;
            }
            return match vm.heap.get(idx) {
                HeapObj::Array(items) => Some(TaSnap {
                    obj_bits: ta_bits,
                    base: items.as_ptr() as u64,
                    len: items.len() as u64,
                }),
                _ => None,
            };
        }
        if kind == crate::codegen::DV_PIN_KIND as u32 {
            // DataView pin: base = data + byteOffset, len = byteLength; the
            // view must be attached and (on a shrunk resizable buffer) still
            // in bounds — mirroring dataview_method's IsViewOutOfBounds.
            let (buffer, byte_offset, byte_length) = match vm.heap.get(idx) {
                HeapObj::DataView { buffer, byte_offset, byte_length } => {
                    (*buffer, *byte_offset, *byte_length)
                }
                _ => return None,
            };
            let base = match vm.heap.get_mut(buffer) {
                HeapObj::ArrayBuffer { data, detached }
                    if !*detached && byte_offset + byte_length <= data.len() =>
                {
                    match data {
                        crate::heap::AbData::Local(v) => v.as_mut_ptr(),
                        crate::heap::AbData::Shared(m) => m.base_ptr(),
                    }
                }
                _ => return None,
            };
            return Some(TaSnap {
                obj_bits: ta_bits,
                base: unsafe { base.add(byte_offset) } as u64,
                len: byte_length as u64,
            });
        }
        let (buffer, k, byte_offset) = match vm.heap.get(idx) {
            HeapObj::TypedArray { buffer, kind, byte_offset, .. } => {
                (*buffer, *kind, *byte_offset)
            }
            _ => return None,
        };
        if k as u32 != kind || k >= 9 {
            return None;
        }
        let len = vm.ta_effective_len(idx)?;
        let base = match vm.heap.get_mut(buffer) {
            HeapObj::ArrayBuffer { data, detached } if !*detached => match data {
                crate::heap::AbData::Local(v) => v.as_mut_ptr(),
                crate::heap::AbData::Shared(m) => m.base_ptr(),
            },
            _ => return None,
        };
        // ta_effective_len guarantees byte_offset + len*size fits the live
        // buffer, and an empty view has byte_offset 0 — the add stays in
        // (or one-past) the allocation.
        Some(TaSnap {
            obj_bits: ta_bits,
            base: unsafe { base.add(byte_offset) } as u64,
            len: len as u64,
        })
    })()
    .unwrap_or(TaSnap { obj_bits: 0, base: 0, len: 0 });
    // SAFETY: caller passes a valid slot pointer.
    unsafe { core::ptr::write(out, snap) };
}

/// Win64 helper for a pinned Uint8Clamped store of a DOUBLE value: JS
/// ToUint8Clamp (clamp to [0,255], round-half-to-even) then the byte store.
/// Pure — no vm, no allocation, no user code (the caller already bounds-checked
/// `addr` against the pinned snapshot).
///
/// # Safety
/// `addr` is in-bounds for the pinned buffer (caller-checked).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_ta_clamp_store(addr: *mut u8, val_bits: u64) {
    let b = crate::vm::helpers_numeric::clamp_u8(f64::from_bits(val_bits));
    // SAFETY: caller-checked bounds.
    unsafe { *addr = b };
}

/// Win64 helper for a whitelisted region `dv.get*(pos[, littleEndian])`
/// (DataView read). Mirrors `dataview_method`'s get path for the non-BigInt,
/// non-Float16 kinds: integral non-negative number `pos` (anything needing
/// ToIndex coercion/throws → deopt), ToBoolean(le) via the read-only `truthy`,
/// detached / shrunk-OOB view or out-of-range read → deopt (the interpreter
/// re-executes and throws the spec'd TypeError/RangeError). No allocation, no
/// user code. `kind` arrives via the 5th-arg stack slot.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_dv_get(
    vm: *mut core::ffi::c_void,
    dv_bits: u64,
    pos_bits: u64,
    le_bits: u64,
    kind: u32,
) -> u64 {
    let dv = Value::from_bits(dv_bits);
    let posv = Value::from_bits(pos_bits);
    if !dv.is_heap() || !posv.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: read-only view; the running region holds no conflicting borrow.
    let vm = unsafe { &*(vm as *const Vm) };
    let (buffer, byte_offset, byte_length) = match vm.heap.get(dv.heap_index()) {
        HeapObj::DataView { buffer, byte_offset, byte_length } => {
            (*buffer, *byte_offset, *byte_length)
        }
        _ => return crate::codegen::SELF_CALL_DEOPT,
    };
    let pos = match array_index(posv) {
        Some(i) => i,
        // Fractional (ToIndex truncates) or huge → interpreter's to_index.
        None => return crate::codegen::SELF_CALL_DEOPT,
    };
    let kind = kind as u8;
    let size = native::TA_KINDS[kind as usize].1;
    let le = vm.truthy(Value::from_bits(le_bits));
    // IsViewOutOfBounds (detached or shrunk under the view) → TypeError in the
    // interpreter; failed bounds → RangeError. Both deopt.
    let data = match vm.heap.get(buffer) {
        HeapObj::ArrayBuffer { data, detached }
            if !*detached && byte_offset + byte_length <= data.len() =>
        {
            data
        }
        _ => return crate::codegen::SELF_CALL_DEOPT,
    };
    if !(size <= byte_length && pos <= byte_length - size) {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let abs = byte_offset + pos;
    if size > data.len() || abs > data.len() - size {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let mut b = [0u8; 8];
    b[..size].copy_from_slice(&data[abs..abs + size]);
    if !le {
        b[..size].reverse();
    }
    (match kind {
        0 => Value::num(b[0] as i8 as f64),
        1 => Value::num(b[0] as f64),
        3 => Value::num(i16::from_le_bytes([b[0], b[1]]) as f64),
        4 => Value::num(u16::from_le_bytes([b[0], b[1]]) as f64),
        5 => Value::num(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
        6 => Value::num(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
        7 => Value::num(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
        _ => Value::num(f64::from_le_bytes(b)),
    })
    .bits()
}

/// Win64 helper for a JIT'd `arr.push(x)` in a region. Appends and returns the
/// new length (Int bits), or `SELF_CALL_DEOPT` for a non-array receiver (the
/// interpreter then resolves the real method). Pins only the register file; the
/// array's Vec may reallocate — safe, no cached pointer.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_array_push(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    val_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    if !arr.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: exclusive view; pins only the register file, not the array's Vec.
    let vm = unsafe { &mut *(vm as *mut Vm) };
    // A SPARSE array's length is NOT items.len() — the virtual-length side table
    // governs (push must place the element AT that length and may throw): deopt
    // so the interpreter's length-aware push runs.
    if vm.array_js_len.contains_key(&arr.heap_index()) {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // `push` is `Set(O, len, v, true)` then `Set(O, "length", …, true)`, and BOTH
    // can fail. This arm was an unconditional `Vec::push`, so on a frozen, sealed
    // or non-extensible array — or one whose `length` was made non-writable —
    // a hot `a.push(x)` silently succeeded and grew the array where the
    // interpreter and node both throw TypeError. `array_ops.rs` gates the
    // interpreter's push on exactly these; this is the missing sibling.
    //
    // Also: a NEW index resolves through OrdinarySet, so an index property on
    // the prototype chain can supply a setter that must run. `array_proto_has_index`
    // is the existing protector for that, and a custom receiver prototype needs
    // the general path too.
    if vm.array_elements_overlaid(arr.heap_index())
        || vm.array_length_nonwritable.contains(&arr.heap_index())
        || vm.array_proto_has_index
        || vm.proto_of.contains_key(&arr.heap_index())
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // Nursery barrier: `arr.push(youngObj)` on a retained array is B119's
    // dominant old→young idiom — the dedicated push lane must barrier like
    // the `jit_set_index` lane does.
    vm.heap.write_barrier_val(arr.heap_index(), Value::from_bits(val_bits));
    match vm.heap.get_mut(arr.heap_index()) {
        HeapObj::Array(items) => {
            items.push(Value::from_bits(val_bits));
            Value::int(items.len() as i32).bits()
        }
        _ => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for a JIT'd `str.charCodeAt(i)` in a region. Returns the
/// UTF-16 code unit at `i` (Int bits), NaN bits for an out-of-range index, or
/// `SELF_CALL_DEOPT` for a non-int index / non-flat-string receiver (a rope or
/// non-string → the interpreter, which flattens). O(1) for ASCII.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_char_code_at(
    vm: *mut core::ffi::c_void,
    str_bits: u64,
    i_bits: u64,
) -> u64 {
    let sv = Value::from_bits(str_bits);
    let iv = Value::from_bits(i_bits);
    if !sv.is_heap() || !iv.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let i = match array_index(iv) {
        Some(i) => i,
        None => return crate::codegen::SELF_CALL_DEOPT, // negative/fractional
    };
    // SAFETY: read-only view; the running region holds no conflicting borrow.
    let vm = unsafe { &*(vm as *const Vm) };
    match vm.heap.get(sv.heap_index()) {
        // The UTF-16 unit at `i` (O(1) ASCII byte fast path inside `unit_at`),
        // matching the interpreter's `charCodeAt`.
        HeapObj::Str(js) => match js.unit_at(i) {
            Some(u) => Value::int(u as i32).bits(),
            None => Value::num(f64::NAN).bits(),
        },
        _ => crate::codegen::SELF_CALL_DEOPT, // rope/non-string → interpreter
    }
}

/// `dst = a + b` for the OSR region's `StrConcat` op: the `+` operator (rope
/// concat or numeric add) on two boxed Values, returning the result bits. A
/// throwing coercion (only possible for exotic operands a `StrConcat` hint
/// shouldn't target) returns `SELF_CALL_DEOPT` so the region bails and the
/// interpreter redoes it (raising the throw properly).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_concat(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    let a = Value::from_bits(a_bits);
    let b = Value::from_bits(b_bits);
    // SAFETY: exclusive view to allocate the rope node; the running region holds
    // no conflicting borrow (it touches only the reg file / globals base, and the
    // heap grows in a separate field).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.add_values(a, b) {
        Ok(v) => v.bits(),
        // `add_values` can run user coercion code (an object operand's
        // `valueOf`/`toString`) that has SIDE EFFECTS before it throws. Returning
        // the "redo" sentinel (`SELF_CALL_DEOPT`) would re-execute the whole `+`
        // in the interpreter — running those side effects a SECOND time. Instead
        // materialize the throw into `pending_throw` and return `CALL_THREW`, so
        // the region exits and the interpreter UNWINDS (the throw surfaces once).
        Err(t) => vm.jit_thrown_to_sentinel(t),
    }
}

/// `dst = a + b` for one `StrConcatChain` link (W11 B124) in a MEM region or a
/// Tier-C body: `Vm::add_values_chain` — the in-place chain append when `a` is
/// the chain's fresh flat-Str accumulator, the full pairwise `+` otherwise.
/// The SAME entry the interpreter arm calls, so results are byte-identical.
///
/// Unlike `jit_str_append` this helper CAN run user code (an object RHS's
/// `valueOf`/`toString`/`@@toPrimitive` via the `add_values` fallback), so a
/// throw must NEVER become the "redo" sentinel (`SELF_CALL_DEOPT`) — the
/// interpreter would re-execute the op and re-run those side effects. Err is
/// materialized into `pending_throw` and returned as `CALL_THREW`, so the
/// region exits and the interpreter UNWINDS (the half-built accumulator is a
/// dead temp register the collector reclaims).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_concat_chain(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    let a = Value::from_bits(a_bits);
    let b = Value::from_bits(b_bits);
    // SAFETY: exclusive view to mutate/allocate the string; the running region
    // holds no conflicting borrow (reg file / globals base only).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.add_values_chain(a, b) {
        Ok(v) => v.bits(),
        Err(t) => vm.jit_thrown_to_sentinel(t),
    }
}

/// Re-seat a chain builder into a `cap`-byte buffer at the chain's FIRST link.
/// A no-op at the last link, without a hint, or on a builder already past the
/// estimate. Content-preserving — only the capacity changes — and it allocates
/// on the Rust heap only, never on the VM heap.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn chain_reseat(js: &mut crate::heap::JsStr, cap: usize, last: bool) {
    if last || cap == 0 || js.as_bytes().len() >= cap {
        return;
    }
    let mut buf = Vec::with_capacity(cap);
    buf.extend_from_slice(js.as_bytes());
    *js = crate::heap::JsStr::from_wtf8(buf);
    chainstats::reseat();
}

/// Hand back a FINISHED chain string's unused capacity (at the chain's last
/// link). Fires only when the estimate over-reserved on both counts — at
/// least 32 bytes AND at least half the buffer — which bounds the retained
/// amplification by the Vec-doubling ladder the estimate replaced, and leaves
/// a builder that outgrew its estimate (`len * 2 > cap`) untouched. The move
/// mirrors the reseat: a fresh exactly-sized buffer, the same bytes, Rust
/// heap only. One small copy per completed chain, once.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn chain_trim(js: &mut crate::heap::JsStr, cap: usize) {
    let len = js.as_bytes().len();
    if len * 2 > cap || cap - len < 32 {
        return;
    }
    let mut buf = Vec::with_capacity(len);
    buf.extend_from_slice(js.as_bytes());
    *js = crate::heap::JsStr::from_wtf8(buf);
    chainstats::trim();
}

/// `dst = a + b` for one `StrConcatChain` link — the single-dispatch fast
/// sibling of `jit_concat_chain` (`ZIPP_NO_CHAIN_FAST=1` restores the
/// sibling). When `a` is the chain's mutable builder (a non-interned flat
/// `Str` — always the fresh, dead result of the previous link, per the
/// emitter's licence on the op) and the leaf is an int or a flat `Str` at a
/// DIFFERENT slot, the append happens here with ONE heap lookup and no
/// take/put `mem::replace` dance. Every other shape falls to the full
/// pairwise `+` (`Vm::add_values`), inheriting ToPrimitive order, the Symbol
/// TypeError and rope asymptotics unchanged — results are value-identical to
/// `Vm::add_values_chain` (the interpreter arm) for every operand pair.
///
/// `cap_hint` (see `chain_capacity_hint`) is a byte-capacity estimate for the
/// finished chain, tagged with `CHAIN_HINT_LAST` at the chain's last link: at
/// the FIRST link the builder is re-seated once into a buffer of that
/// capacity instead of climbing the realloc ladder link by link, and at the
/// LAST link — once the string is finished — `chain_trim` hands back what the
/// estimate over-reserved. Both halves are needed: nothing else in the engine
/// ever shrinks a `JsStr`'s buffer, so without the trim the reseat's slack is
/// retained for the string's whole lifetime (a 26-leaf chain of one-char
/// leaves holding a 256-byte buffer for 25 bytes of content measured at
/// +194 MB of steady-state RSS over 1.2M live strings) — and the allocation
/// counters are blind to it, since the buffer is the JsStr's own Rust `Vec`
/// and `vm.heap.len()` never moves. Content-preserving in both directions — a
/// wrong hint only changes capacity. The reseat sits INSIDE the two in-place
/// arms: a link that falls to the generic tail builds a fresh string, so
/// pre-sizing the old builder there would only have thrown the buffer away.
///
/// CONTRACT (the emitters' same-bits refetch elision rests on it): `a`'s own
/// bits with `a` heap-tagged come back ONLY from the two in-place arms,
/// which allocate nothing on the VM heap and run no user code — so
/// `result == acc && acc is heap` proves the pinned pointers (r13/r14/TA
/// snapshots) are still valid. The generic tail is `add_values`, never
/// `str_append_inplace`: the latter's materialise arm can allocate a
/// temporary heap string and still hand back `a`'s bits, which would break
/// the elision. `add_values` on a heap `a` always produces a fresh index
/// (pinned by `add_values_never_returns_lhs_index` in coerce.rs and the
/// debug_assert below).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_concat_chain_fast(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
    cap_hint: u64,
) -> u64 {
    let a = Value::from_bits(a_bits);
    let b = Value::from_bits(b_bits);
    // SAFETY: exclusive view to mutate/allocate the string; the running region
    // holds no conflicting borrow (reg file / globals base only).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    // The chain's last link asks for the trim; every other hint is a capacity.
    let last = cap_hint & crate::codegen::CHAIN_HINT_LAST as u64 != 0;
    let cap = (cap_hint & !(crate::codegen::CHAIN_HINT_LAST as u64)) as usize;
    if a.is_heap() && a.heap_index() > crate::heap::INTERN_EMPTY {
        let ai = a.heap_index();
        #[cfg(debug_assertions)]
        let heap_len = vm.heap.len();
        // ── in-place arms ── NO Vm::heap allocation is allowed inside either
        // arm: the builder reference (and, in the str arm, the leaf's raw byte
        // pointer) must not cross a slot-moving collection. The reseat, the
        // trim and the buffer growth are all the JsStr's own Rust Vec — they
        // never touch the VM heap (which is what the debug asserts pin).
        if b.is_int() {
            if let HeapObj::Str(js) = vm.heap.get_mut(ai) {
                chain_reseat(js, cap, last);
                let n = b.as_int();
                if (0..=9).contains(&n) {
                    js.push_ascii(b'0' + n as u8);
                } else {
                    let (buf, start) = super::coerce::fmt_i32_buf(n);
                    js.push_wtf8(&buf[start..]);
                }
                if last {
                    chain_trim(js, cap);
                }
                #[cfg(debug_assertions)]
                debug_assert_eq!(heap_len, vm.heap.len(), "in-place int arm allocated");
                chainstats::fast_int();
                return a_bits;
            }
        } else if b.is_heap() && b.heap_index() != ai {
            // The LEAF is read FIRST, and its shared borrow of the heap ends
            // before the builder's `&mut` is taken. The order is load-bearing,
            // not tidiness: with the builder's reference taken first, reaching
            // the leaf reborrows the SAME allocation (the heap's slot vector)
            // and invalidates the builder's tag under stacked borrows, so the
            // push would write through a dead pointer. This way round the
            // later `&mut` retags the slot vector, a DIFFERENT allocation from
            // the leaf's byte buffer (a JsStr owns its Vec), so the leaf
            // pointer keeps its provenance.
            let leaf = match vm.heap.get(b.heap_index()) {
                HeapObj::Str(vs) => {
                    let bytes = vs.as_bytes();
                    Some((bytes.as_ptr(), bytes.len()))
                }
                _ => None, // rope leaf → generic (O(1) rope links preserved)
            };
            if let Some((ptr, len)) = leaf {
                if let HeapObj::Str(js) = vm.heap.get_mut(ai) {
                    chain_reseat(js, cap, last);
                    // SAFETY: distinct slots (`b.heap_index() != ai`), and a
                    // JsStr owns its byte Vec, so the leaf's buffer and the
                    // builder's are disjoint allocations; nothing since the
                    // leaf pointer was taken has allocated on the VM heap or
                    // written to the leaf, so its slot cannot have moved.
                    unsafe { js.push_wtf8(std::slice::from_raw_parts(ptr, len)) };
                    if last {
                        chain_trim(js, cap);
                    }
                    #[cfg(debug_assertions)]
                    debug_assert_eq!(heap_len, vm.heap.len(), "in-place str arm allocated");
                    chainstats::fast_str();
                    return a_bits;
                }
            }
        }
    }
    chainstats::fallback();
    // The full pairwise `+` — exactly what `add_values_chain` reduces to when
    // its builder check fails. A throw is materialized into `pending_throw`,
    // never redone — see `jit_concat_chain`.
    match vm.add_values(a, b) {
        Ok(v) => {
            debug_assert!(
                v.bits() != a_bits || !a.is_heap(),
                "generic `+` returned a heap accumulator's own bits — the \
                 same-bits refetch elision premise is broken"
            );
            v.bits()
        }
        Err(t) => vm.jit_thrown_to_sentinel(t),
    }
}

/// `dst = a + b` for the OSR region's `StrAppendInPlace` op: appends into `a`'s
/// buffer in place when uniquely owned (see `str_append_inplace`). Deopts
/// (SELF_CALL_DEOPT) when the appended value needs real ToPrimitive — a user
/// `toString`/`valueOf`/`@@toPrimitive`, or a Symbol's TypeError — so the
/// interpreter re-executes the op with full semantics. The purity gate runs
/// BEFORE any mutation, so the re-execution is clean.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_str_append(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    let a = Value::from_bits(a_bits);
    let b = Value::from_bits(b_bits);
    // SAFETY: exclusive view to mutate/allocate the string; the running region
    // holds no conflicting borrow (reg file / globals base only).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.str_append_inplace(a, b) {
        Some(r) => r.bits(),
        None => crate::codegen::SELF_CALL_DEOPT,
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
/// `ZIPP_ICSTATS=1` — what a native SHAPE-keyed inline-cache way would be worth,
/// measured before one is emitted.
///
/// Every count here is taken inside the miss helper, so it is one native property
/// access that already paid eight failed 64-byte-strided compares and an
/// `extern "win64"` call. `shape_known` is the subset where `jit_shape_slot`
/// already knew the slot from the receiver's LAYOUT — exactly the accesses an
/// emitted shape way would serve with no call. `shape_new` is a layout this site
/// had not seen (a shape way would have missed too, and filled). `dict` and
/// `not_object` can never be shape-guarded at all.
///
/// Off, one relaxed atomic load per miss, on a path that already made a call.
mod icstats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static GET_MISS: AtomicU64 = AtomicU64::new(0);
    static GET_SHAPE_KNOWN: AtomicU64 = AtomicU64::new(0);
    static GET_SHAPE_NEW: AtomicU64 = AtomicU64::new(0);
    static GET_DICT: AtomicU64 = AtomicU64::new(0);
    static SET_MISS: AtomicU64 = AtomicU64::new(0);
    static SET_GUARDABLE: AtomicU64 = AtomicU64::new(0);
    static SET_DICT: AtomicU64 = AtomicU64::new(0);
    static GET_ACC_HIT: AtomicU64 = AtomicU64::new(0);
    static SET_ACC_HIT: AtomicU64 = AtomicU64::new(0);

    #[inline]
    pub(super) fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_ICSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    /// One `GetProp` miss. `known` means the `(site, shape)` memo already had the
    /// slot; `guardable` means the receiver had a non-DICT shape at all.
    #[inline]
    pub(super) fn get_miss(guardable: bool, known: bool) {
        if !enabled() {
            return;
        }
        GET_MISS.fetch_add(1, Ordering::Relaxed);
        if !guardable {
            GET_DICT.fetch_add(1, Ordering::Relaxed);
        } else if known {
            GET_SHAPE_KNOWN.fetch_add(1, Ordering::Relaxed);
        } else {
            GET_SHAPE_NEW.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One `SetProp` miss. There is no memo on the store side (B72 refuted it),
    /// so this only separates guardable receivers from dictionary ones.
    #[inline]
    pub(super) fn set_miss(guardable: bool) {
        if !enabled() {
            return;
        }
        SET_MISS.fetch_add(1, Ordering::Relaxed);
        if guardable {
            SET_GUARDABLE.fetch_add(1, Ordering::Relaxed);
        } else {
            SET_DICT.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One ACCESSOR-way hit (B114): a probe matched an accessor-tagged way and
    /// dispatched straight to the accessor helper — an access that, pre-B114,
    /// was a permanent native miss (8 failed compares + `jit_get_prop_miss` +
    /// `PROP_VIA_IC` + `jit_get_prop_slow`). Counted in the accessor helpers,
    /// which have already been called — same cost posture as `get_miss`.
    #[inline]
    pub(super) fn acc_hit(is_set: bool) {
        if !enabled() {
            return;
        }
        if is_set {
            SET_ACC_HIT.fetch_add(1, Ordering::Relaxed);
        } else {
            GET_ACC_HIT.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(get_miss, get_shape_known, get_shape_new, get_dict, set_miss,
    /// set_guardable, set_dict, get_acc_hit, set_acc_hit)`
    pub fn dump() -> (u64, u64, u64, u64, u64, u64, u64, u64, u64) {
        (
            GET_MISS.load(Ordering::Relaxed),
            GET_SHAPE_KNOWN.load(Ordering::Relaxed),
            GET_SHAPE_NEW.load(Ordering::Relaxed),
            GET_DICT.load(Ordering::Relaxed),
            SET_MISS.load(Ordering::Relaxed),
            SET_GUARDABLE.load(Ordering::Relaxed),
            SET_DICT.load(Ordering::Relaxed),
            GET_ACC_HIT.load(Ordering::Relaxed),
            SET_ACC_HIT.load(Ordering::Relaxed),
        )
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub use icstats::dump as ic_stats;

/// `ZIPP_ICSTATS=1` — B82 call/apply splice counters. `call`/`apply` count the
/// region-call helper serving a `f.call(…)`/`f.apply(…)` TARGET off-frame
/// (`try_fn_call_apply_inline`); `hasown` counts the older guarded
/// `hasOwnProperty.call(array, key)` intrinsic answering without any call
/// machinery (`jit_has_own_call` — the sparse-array phase's shape). Off, one
/// relaxed atomic load on paths that already made a helper call.
/// `ZIPP_ICSTATS=1` — W7 cross-call window-fill counters: `fill_fast` counts
/// callee windows exposed via `set_len` under the high-water mark with only
/// the may-read-before-write registers re-zeroed (the `cross_uninit_mask`
/// lever engaging); `fill_full` counts full zero-filling `resize`s (new
/// ground, analysis declined, or `ZIPP_NO_CROSSCALL2=1`). Off, one relaxed
/// atomic load on a path that already made an FFI helper call.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) mod crossstats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static FILL_FAST: AtomicU64 = AtomicU64::new(0);
    static FILL_FULL: AtomicU64 = AtomicU64::new(0);

    #[inline]
    fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => init(),
        }
    }

    #[cold]
    fn init() -> bool {
        let v = std::env::var_os("ZIPP_ICSTATS").is_some() as u8;
        ON.store(v, Ordering::Relaxed);
        v == 1
    }

    /// One cross-call callee window served by the W7 fast fill (`set_len` +
    /// mask zeroing) instead of a full `resize`.
    #[inline]
    pub(crate) fn fill_fast() {
        if enabled() {
            FILL_FAST.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One cross-call callee window zero-filled in full.
    #[inline]
    pub(crate) fn fill_full() {
        if enabled() {
            FILL_FULL.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(fast_fills, full_fills)`
    pub fn dump() -> (u64, u64) {
        (FILL_FAST.load(Ordering::Relaxed), FILL_FULL.load(Ordering::Relaxed))
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub use crossstats::dump as cross_fill_stats;

/// `ZIPP_ICSTATS=1` — chain-link fast-helper counters: `fast_int`/`fast_str`
/// count `StrConcatChain` links served by `jit_concat_chain_fast`'s in-place
/// arms (one heap lookup, no VM alloc, no take/put dance); `fallback` counts
/// links that fell to the full pairwise `+`; `reseat`/`trim` count the
/// capacity-hint buffer moves at a chain's first and last link. Off, one
/// relaxed atomic load on a path that already made an FFI helper call.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) mod chainstats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static FAST_INT: AtomicU64 = AtomicU64::new(0);
    static FAST_STR: AtomicU64 = AtomicU64::new(0);
    static FALLBACK: AtomicU64 = AtomicU64::new(0);
    static RESEAT: AtomicU64 = AtomicU64::new(0);
    static TRIM: AtomicU64 = AtomicU64::new(0);

    #[inline]
    fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => init(),
        }
    }

    #[cold]
    fn init() -> bool {
        let v = std::env::var_os("ZIPP_ICSTATS").is_some() as u8;
        ON.store(v, Ordering::Relaxed);
        v == 1
    }

    /// One chain link served by the in-place int-leaf arm.
    #[inline]
    pub(crate) fn fast_int() {
        if enabled() {
            FAST_INT.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One chain link served by the in-place flat-Str-leaf arm.
    #[inline]
    pub(crate) fn fast_str() {
        if enabled() {
            FAST_STR.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One chain link that fell to the full pairwise `+`.
    #[inline]
    pub(crate) fn fallback() {
        if enabled() {
            FALLBACK.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One first-link builder re-seated into a capacity-hinted buffer.
    #[inline]
    pub(crate) fn reseat() {
        if enabled() {
            RESEAT.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One finished chain string handed its over-reserved capacity back.
    #[inline]
    pub(crate) fn trim() {
        if enabled() {
            TRIM.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(fast_int, fast_str, fallback, reseat, trim)`
    pub fn dump() -> (u64, u64, u64, u64, u64) {
        (
            FAST_INT.load(Ordering::Relaxed),
            FAST_STR.load(Ordering::Relaxed),
            FALLBACK.load(Ordering::Relaxed),
            RESEAT.load(Ordering::Relaxed),
            TRIM.load(Ordering::Relaxed),
        )
    }
}

pub(crate) mod callstats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static CALL_HIT: AtomicU64 = AtomicU64::new(0);
    static APPLY_HIT: AtomicU64 = AtomicU64::new(0);
    static HASOWN_HIT: AtomicU64 = AtomicU64::new(0);

    #[inline]
    fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_ICSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    /// One off-frame `f.call(…)` / `f.apply(…)` target splice served.
    /// (Counted only from JIT helpers; unused — and zero — without the JIT.)
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn inline_hit(is_apply: bool) {
        if !enabled() {
            return;
        }
        if is_apply {
            APPLY_HIT.fetch_add(1, Ordering::Relaxed);
        } else {
            CALL_HIT.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One `hasOwnProperty.call(array, key)` intrinsic hit.
    /// (Counted only from JIT helpers; unused — and zero — without the JIT.)
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn hasown_hit() {
        if !enabled() {
            return;
        }
        HASOWN_HIT.fetch_add(1, Ordering::Relaxed);
    }

    /// `(call_hits, apply_hits, hasown_intrinsic_hits)`
    pub fn dump() -> (u64, u64, u64) {
        (
            CALL_HIT.load(Ordering::Relaxed),
            APPLY_HIT.load(Ordering::Relaxed),
            HASOWN_HIT.load(Ordering::Relaxed),
        )
    }
}

pub use callstats::dump as call_inline_stats;

/// Without the JIT there are no inline caches, so there is nothing to miss and
/// every counter is zero. Present in every configuration so that the public
/// `zipp_vm::ic_stats` and the CLI's `ZIPP_ICSTATS` reporting do not have to
/// know which tiers this build was compiled with.
#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
pub fn ic_stats() -> (u64, u64, u64, u64, u64, u64, u64, u64, u64) {
    (0, 0, 0, 0, 0, 0, 0, 0, 0)
}

/// Without the JIT there are no cross-call windows to fill (same contract as
/// the `ic_stats` stub above).
#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
pub fn cross_fill_stats() -> (u64, u64) {
    (0, 0)
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_prop_miss(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    site_idx: u32,
    packed: u64,
) -> u64 {
    let obj = Value::from_bits(obj_bits);
    if !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: exclusive view (updates the IC table); the running region holds no
    // conflicting borrow (the IC table and the region live in different fields).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let func_id = (packed >> 32) as u32;
    let name_idx = packed as u32;
    let idx = obj.heap_index();
    // `vm.func` — NOT `vm.program.functions[..]`. A JIT-compiled function can be
    // an EVAL function (`$262.evalScript`, and the test262 harness prelude), which
    // lives in `vm.eval_funcs` past `main_func_count`, not in the main program's
    // table. Indexing the program directly panicked with an out-of-bounds the
    // moment such a function got hot enough to compile and took a property miss —
    // "len is 3 but the index is 45", where 3 was the test's own function count.
    // `func` resolves both halves and returns the same `'p` lifetime the key
    // borrow below needs.
    let key = &vm.func(func_id as usize).string_constants[name_idx as usize];
    // Exotic receivers whose slots have live semantics layered over the ObjMap
    // (the global object, %Array.prototype%, module namespaces, realm globals,
    // deferred-namespace state) — same exclusions as the interpreter IC. A
    // private ('#') name needs brand checks — interpreter only.
    if key.as_bytes().first() == Some(&b'#')
        || !vm.deferred_ns_state.is_empty()
        || !vm.ic_obj_ok(idx)
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // ── shape memo ──────────────────────────────────────────────────────────
    // Before the key scan: if this site has already resolved this SHAPE, the slot
    // is known. That is the whole cost of a site whose receivers share a layout
    // but not an identity — the ways thrash, every access misses, and every miss
    // re-scans the key list to rediscover the same slot.
    if let HeapObj::Object(map) = vm.heap.get(idx) {
        let sh = map.shape();
        if icstats::enabled() {
            let guardable = sh != crate::shape::DICT;
            icstats::get_miss(
                guardable,
                guardable && vm.jit_shape_slot.contains_key(&(site_idx, sh)),
            );
        }
        if sh != crate::shape::DICT {
            if let Some(&slot) = vm.jit_shape_slot.get(&(site_idx, sh)) {
                let s = slot as usize;
                // The memo names a slot; the ACCESSOR flag is per-object state
                // that a shape does carry, but re-checking is one load and keeps
                // this independent of that argument.
                if !map.attr_at(s).accessor {
                    let val = map.val_at(s);
                    // Refill only while the site still has ways to give. Once it
                    // has evicted a full round (`ic_thrashing`), it is
                    // megamorphic by IDENTITY while monomorphic by SHAPE, and a
                    // fresh identity-keyed way is evicted before it is ever hit
                    // — the write is wasted and it displaces a way that may
                    // still be serving someone.
                    //
                    // Skipping the refill UNCONDITIONALLY was measurably wrong:
                    // a site with 2-8 receivers fills its ways one miss at a
                    // time, so refusing the first refill for receiver 2 means
                    // receiver 2 never gets a way at all. That took the 2-8
                    // receiver case from ~5.5ns to ~12ns.
                    if !vm.jit.ic_thrashing(site_idx) {
                        let vals_ptr = map.vals_ptr() as u64;
                        let version = vm.heap.version_of(idx);
                        if let Some(e) =
                            crate::codegen::IcEntry::own(obj_bits, vals_ptr, version, slot)
                        {
                            vm.jit.set_ic(site_idx, e);
                        }
                    }
                    return val.bits();
                }
            }
        }
    }
    let (val, vals_ptr, slot) = match vm.heap.get(idx) {
        HeapObj::Object(map) => match map.pos(key) {
            // An accessor slot stores the GETTER, not a data value — route to
            // the interpreter-IC slow helper, which frame-calls it. B114: fill
            // an ACCESSOR way first, so this receiver's next access dispatches
            // there straight from the probe instead of missing all eight ways
            // and rediscovering the accessor here forever (polymorphic-objects'
            // permanent 1.25M-miss stream, B111). Guards: identity + receiver
            // version — a redefinition (accessor→data), delete, freeze or proto
            // change bumps it. The cacheability conditions above (private name,
            // deferred-ns, `ic_obj_ok`) already returned early, exactly as for
            // a data fill.
            Some(s) if map.attrs[s].accessor => {
                // Site-gated (the B115 follow-up): `acc_way_gate` says whether
                // this site's compiled probe carries the accessor arms.
                // `Recompile` means it does NOT and the gate just flipped —
                // the owning compile is evicted and we DEOPT (the calling
                // frame IS the parked arm-less code; a single-activation loop
                // would otherwise never recompile). Filling under a tag-blind
                // probe is never an option.
                let gate = vm.jit.acc_way_gate(site_idx);
                if gate == crate::codegen::AccWayGate::Recompile {
                    return crate::codegen::SELF_CALL_DEOPT;
                }
                if gate == crate::codegen::AccWayGate::Fill {
                    let getter = map.vals[s];
                    // Bake direct dispatch only for a plain user fn without
                    // lexical `this` (an arrow accessor must deopt so
                    // `setup_call` rebinds — see jit_prop_slow_impl); the
                    // helper re-validates the baked fn against the live slot.
                    let baked = vm
                        .ic_plain_fn(getter)
                        .filter(|&(fid, _)| !vm.func(fid as usize).lexical_this)
                        .map(|(fid, closure)| (getter.bits(), fid, closure));
                    if let Some(e) = crate::codegen::IcEntry::accessor(
                        obj_bits,
                        vm.heap.version_of(idx),
                        s as u32,
                        &[],
                        baked,
                    ) {
                        vm.jit.set_ic(site_idx, e);
                    }
                }
                return crate::codegen::PROP_VIA_IC;
            }
            Some(s) => (map.vals[s], map.vals.as_ptr() as u64, s as u32),
            // Missing own key: a CLASS instance resolves methods/getters on
            // its class chain — the interpreter-IC slow helper serves those
            // (polymorphic, guard-validated); a plain object walks the PROTO
            // CHAIN (`Object.create({val})` — the chain may hold the property).
            None if map.class.is_some() => return crate::codegen::PROP_VIA_IC,
            None => {
                const MAX: usize = crate::codegen::JIT_IC_MAX_HOPS;
                let mut cur = idx;
                let mut hops: [(u32, u32); MAX] = [(0, 0); MAX];
                let mut n_hops = 0usize;
                loop {
                    let next = match vm.proto_of.get(&cur) {
                        Some(p) if p.is_heap() => p.heap_index(),
                        // Explicit null prototype: a true chain miss.
                        Some(_) => return Value::UNDEFINED.bits(),
                        None => {
                            if vm.obj_proto == 0 || cur == vm.obj_proto {
                                return Value::UNDEFINED.bits();
                            }
                            vm.obj_proto
                        }
                    };
                    if n_hops >= 64 || !vm.ic_obj_ok(next) {
                        return crate::codegen::SELF_CALL_DEOPT;
                    }
                    match vm.heap.get(next) {
                        HeapObj::Object(m2) if !m2.is_ctor && m2.class.is_none() => {
                            if let Some(i) = m2.pos(key) {
                                if m2.attrs[i].accessor {
                                    // Inherited getter: frame-called by the
                                    // interpreter-IC slow helper. B114: fill an
                                    // ACCESSOR way under the same guards a
                                    // chain-DATA hit would take — receiver
                                    // identity + receiver version + every hop's
                                    // version down to the holder (shadowing
                                    // adds, holder redefinition/delete and
                                    // setPrototypeOf all bump one of them).
                                    // Site-gated: as the own-accessor fill
                                    // above (a too-deep chain never fills, so
                                    // it neither marks nor evicts).
                                    let gate = if n_hops < MAX {
                                        vm.jit.acc_way_gate(site_idx)
                                    } else {
                                        crate::codegen::AccWayGate::Slow
                                    };
                                    if gate == crate::codegen::AccWayGate::Recompile {
                                        return crate::codegen::SELF_CALL_DEOPT;
                                    }
                                    if gate == crate::codegen::AccWayGate::Fill {
                                        hops[n_hops] =
                                            (next, vm.heap.version_of(next));
                                        let getter = m2.vals[i];
                                        let baked = vm
                                            .ic_plain_fn(getter)
                                            .filter(|&(fid, _)| {
                                                !vm.func(fid as usize).lexical_this
                                            })
                                            .map(|(fid, closure)| {
                                                (getter.bits(), fid, closure)
                                            });
                                        if let Some(e) =
                                            crate::codegen::IcEntry::accessor(
                                                obj_bits,
                                                vm.heap.version_of(idx),
                                                i as u32,
                                                &hops[..=n_hops],
                                                baked,
                                            )
                                        {
                                            vm.jit.set_ic(site_idx, e);
                                        }
                                    }
                                    return crate::codegen::PROP_VIA_IC;
                                }
                                let v = m2.vals[i];
                                // A chain DATA hit within JIT_IC_MAX_HOPS fills
                                // a hop-version-guarded way (receiver identity
                                // + receiver version + every hop's version —
                                // shadowing adds, hop key changes/deletes and
                                // setPrototypeOf all bump one of them). Deeper
                                // holders return UNCACHED (rare).
                                if n_hops < MAX {
                                    hops[n_hops] = (next, vm.heap.version_of(next));
                                    if let Some(e) = crate::codegen::IcEntry::chain(
                                        obj_bits,
                                        m2.vals.as_ptr() as u64,
                                        vm.heap.version_of(idx),
                                        i as u32,
                                        &hops[..=n_hops],
                                    ) {
                                        vm.jit.set_ic(site_idx, e);
                                    }
                                }
                                return v.bits();
                            }
                        }
                        _ => return crate::codegen::SELF_CALL_DEOPT, // exotic link
                    }
                    if n_hops < MAX {
                        hops[n_hops] = (next, vm.heap.version_of(next));
                    }
                    n_hops += 1;
                    cur = next;
                }
            }
        },
        // `arr.length` / `str.length` in a region: return the length WITHOUT
        // caching — it's derived from the container's element count, not a fixed
        // slot, so a stale cache would be wrong after the container grows. The IC
        // entry stays unset, so this site simply misses (helper call) each time —
        // cheap, and it lets a `for (i < a.length) a[i]` loop run as a region
        // instead of bailing on the first `.length` access.
        HeapObj::Array(items) if key == "length" => {
            // An arguments object's `length` is an ORDINARY (writable) prop in
            // arr_props — defer to the interpreter.
            if vm.arguments_objs.contains_key(&idx) {
                return crate::codegen::SELF_CALL_DEOPT;
            }
            // A SPARSE array's JS length lives in the virtual-length side
            // table (still uncached — correct value, plain helper miss).
            let n = vm.array_js_len.get(&idx).map_or(items.len(), |&n| n as usize);
            return len_value(n).bits();
        }
        // `ta.length` in a region. A TypedArray's `length` is an ACCESSOR
        // inherited from %TypedArray%.prototype (not an own exotic slot like an
        // Array's), so this only answers directly while that accessor is
        // provably the pristine built-in; anything else defers to the
        // interpreter, which invokes the real getter.
        //
        // Without this arm `for (i = 0; i < ta.length; i++)` — the most common
        // numeric-JS loop there is — missed here, deopted, and after
        // OSR_DEOPT_LIMIT blacklisted the region for the life of the process:
        // the identical kernel written with a constant bound ran ~57x faster.
        HeapObj::TypedArray { buffer, length, .. } if key == "length" => {
            let (buffer, length) = (*buffer, *length);
            // A length-TRACKING view over a resizable buffer re-derives its
            // length from the buffer, and a detached buffer reports 0 — both go
            // to the interpreter so this stays a single unambiguous read.
            if vm.ta_tracking.contains(&idx)
                || matches!(vm.heap.get(buffer), HeapObj::ArrayBuffer { detached: true, .. })
                || !vm.ta_length_is_intrinsic(idx)
            {
                return crate::codegen::SELF_CALL_DEOPT;
            }
            return len_value(length).bits();
        }
        HeapObj::Str(s) if key == "length" => return len_value(s.units()).bits(),
        HeapObj::Cons { len, .. } if key == "length" => return len_value(*len).bits(),
        _ => return crate::codegen::SELF_CALL_DEOPT, // other array/string props → interpreter
    };
    let version = vm.heap.version_of(idx);
    if let Some(e) = crate::codegen::IcEntry::own(obj_bits, vals_ptr, version, slot) {
        vm.jit.set_ic(site_idx, e);
    }
    // Record the resolution against the receiver's shape, so the next receiver
    // of the same layout skips the scan above. Only for an OWN data property on
    // a shaped object — a proto-chain or class resolution depends on more than
    // the receiver's own layout and is already guarded by hop versions.
    if let HeapObj::Object(map) = vm.heap.get(idx) {
        let sh = map.shape();
        if sh != crate::shape::DICT
            && map.attr_get(slot as usize).is_some_and(|a| !a.accessor)
            && vm.jit_shape_slot.len() < JIT_SHAPE_SLOT_MAX
        {
            vm.jit_shape_slot.insert((site_idx, sh), slot);
        }
    }
    val.bits()
}

/// Ceiling on the `(site, shape) -> slot` memo. Pure memo, so overflowing it
/// only costs the key scan it was avoiding.
const JIT_SHAPE_SLOT_MAX: usize = 1 << 16;

/// Win64 helper: a SITE-FREE named property read, for a `GetProp` inside an
/// INLINED LEAF BODY.
///
/// The leaf inliner previously had no `GetProp` at all, so a plain
/// `function f(o) { return o._v + 1; }` was `(not leaf-eligible)` and a hot loop
/// calling it paid a full frame call per iteration — measured at **30.1ns against
/// 7.0ns** for the identical body written as a method, which the METHOD inliner
/// does inline. That is the single commonest call shape in the language taking the
/// slowest path (B73).
///
/// Site-FREE on purpose, exactly like `jit_get_index` which the leaf emitter
/// already uses: an inlined body has no inline-cache site of its own, and giving it
/// one would mean growing `ic_table` past what `reserve_ic_sites` reserved for the
/// region — the table's base is pinned in a callee-saved register for the whole
/// native run, so it must not move. No IC also means no `(site, shape)` memo, which
/// is fine: B72 measured that memo as a PESSIMISATION for maps below
/// `PROP_INDEX_THRESHOLD`, and a leaf's receiver is usually a small object.
///
/// `packed = (callee_func_id << 32) | name_idx` — the CALLEE's id and its own
/// string-constant index, since the body's ops carry the callee's numbering.
///
/// Returns the value bits, or `SELF_CALL_DEOPT` for anything the interpreter must
/// do instead. Deopting re-runs the WHOLE call from the call ip, which is sound
/// because `callee_leaf_ok` admits `GetProp` only before a committed effect.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_prop_leaf(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    packed: u64,
) -> u64 {
    let obj = Value::from_bits(obj_bits);
    if !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: exclusive view; the running region holds no conflicting borrow.
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let func_id = (packed >> 32) as u32;
    let name_idx = packed as u32;
    let idx = obj.heap_index();
    // `vm.func`, not `program.functions[..]` — the callee can be an eval function
    // (the M1.3 lesson).
    let key = &vm.func(func_id as usize).string_constants[name_idx as usize];
    // Same exclusions as the interpreter IC: a private name needs brand checks, and
    // an exotic receiver has live semantics layered over its ObjMap.
    if key.as_bytes().first() == Some(&b'#')
        || !vm.deferred_ns_state.is_empty()
        || !vm.ic_obj_ok(idx)
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // Own data property — the case this exists for.
    match vm.heap.get(idx) {
        HeapObj::Object(map) => {
            if let Some(s) = map.pos(key) {
                if map.attrs[s].accessor {
                    return crate::codegen::SELF_CALL_DEOPT; // getter runs user code
                }
                return map.vals[s].bits();
            }
            // Not own. Walk a PROVABLY clean prototype chain so a legitimately
            // inherited read (or a provably absent one) does not deopt on every
            // iteration — a leaf that deopted per call would drive the enclosing
            // region past `OSR_DEOPT_LIMIT` and get it evicted, which is the cliff
            // shape B69 fixed elsewhere. Anything unclean defers instead.
            if map.class.is_some() {
                return crate::codegen::SELF_CALL_DEOPT; // class chain: methods/getters
            }
            let mut cur = idx;
            let mut hops = 0u32;
            loop {
                let next = match vm.proto_of.get(&cur) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => return Value::UNDEFINED.bits(), // explicit null proto
                    None => {
                        if vm.obj_proto == 0 || cur == vm.obj_proto {
                            return Value::UNDEFINED.bits(); // chain end, absent
                        }
                        vm.obj_proto
                    }
                };
                hops += 1;
                if hops > 64 || !vm.ic_obj_ok(next) {
                    return crate::codegen::SELF_CALL_DEOPT;
                }
                match vm.heap.get(next) {
                    HeapObj::Object(m2) if !m2.is_ctor && m2.class.is_none() => {
                        if let Some(i) = m2.pos(key) {
                            if m2.attrs[i].accessor {
                                return crate::codegen::SELF_CALL_DEOPT;
                            }
                            return m2.vals[i].bits();
                        }
                    }
                    _ => return crate::codegen::SELF_CALL_DEOPT, // exotic link
                }
                cur = next;
            }
        }
        // Array/Str/TypedArray/… named reads (`.length`, index-ish keys, exotics)
        // all have their own semantics — the interpreter owns them.
        _ => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper: the INLINE-CACHE MISS path for a JIT'd `SetProp`. Performs
/// `obj.<key> = val`, then (for a plain Object) fills inline-cache slot `site` so
/// later writes are call-free. Returns `0` (success — incl. a heap non-Object,
/// which no-ops, matching the interpreter) or `SELF_CALL_DEOPT` (null/undefined →
/// the interpreter throws). `packed = (func_id << 32) | name_idx`; `site_idx` is
/// the 5th argument (passed on the stack by the caller).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_prop_miss(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    val_bits: u64,
    packed: u64,
    site_idx: u32,
) -> u64 {
    let obj = Value::from_bits(obj_bits);
    if !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let func_id = (packed >> 32) as u32;
    let name_idx = packed as u32;
    let idx = obj.heap_index();
    // `vm.func` — NOT `vm.program.functions[..]`, for the reason spelled out in
    // `jit_get_prop_miss` above: a JIT-compiled function can be an EVAL function
    // living in `vm.eval_funcs` past `main_func_count`, and indexing the program's
    // table directly is an out-of-bounds panic the moment such a function gets hot
    // and takes a `SetProp` miss. The get side was fixed; this one was missed.
    // Nursery barrier + B6 oracle: NURSERY_DESIGN.md §1 case 1, the JIT miss
    // route. IC-HIT stores make no call — that is what `register_scan_root`
    // at the fill below is for.
    vm.store_barrier(crate::heap::gcoracle::JIT_SET_PROP, idx, Value::from_bits(val_bits));
    let key = &vm.func(func_id as usize).string_constants[name_idx as usize];
    // Keys with exotic write interception (the inherited `__proto__` setter,
    // restricted names, private names, canonical-index-ish keys) and exotic
    // receivers — same exclusions as the interpreter's SetProp IC: defer.
    if key == "__proto__"
        || key == "caller"
        || key == "arguments"
        || key
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_digit() || *b == b'-' || *b == b'#')
        || !vm.deferred_ns_state.is_empty()
        || !vm.ic_obj_ok(idx)
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    if icstats::enabled() {
        let guardable = matches!(vm.heap.get(idx), HeapObj::Object(m) if m.shape_guardable());
        icstats::set_miss(guardable);
    }
    // Pre-checks against a shared borrow (the write below re-borrows mutably).
    let own = match vm.heap.get(idx) {
        HeapObj::Object(map) => match map.pos(key) {
            // An accessor's SETTER must run (user code) — the interpreter-IC
            // slow helper frame-calls it. B114: fill an OWN accessor way first
            // (identity + version, like every Set way — the probe walks no
            // hops for a store, so inherited setters stay on PROP_VIA_IC).
            // The setter lives in `attrs[s].setter`, NOT `vals[s]` — B52's
            // documented asymmetry — and the accessor helper re-reads it live.
            Some(s) if map.attrs[s].accessor => {
                // Site-gated: see the GetProp miss helper's own-accessor fill.
                let gate = vm.jit.acc_way_gate(site_idx);
                if gate == crate::codegen::AccWayGate::Recompile {
                    return crate::codegen::SELF_CALL_DEOPT;
                }
                if gate == crate::codegen::AccWayGate::Fill {
                    let setter = map.attrs[s].setter;
                    let baked = vm
                        .ic_plain_fn(setter)
                        .filter(|&(fid, _)| !vm.func(fid as usize).lexical_this)
                        .map(|(fid, closure)| (setter.bits(), fid, closure));
                    if let Some(e) = crate::codegen::IcEntry::accessor(
                        obj_bits,
                        vm.heap.version_of(idx),
                        s as u32,
                        &[],
                        baked,
                    ) {
                        vm.jit.set_ic(site_idx, e);
                    }
                }
                return crate::codegen::PROP_VIA_IC;
            }
            // A non-writable own data prop: sloppy no-op / strict throw —
            // the interpreter applies the right one.
            Some(s) if !map.attrs[s].writable => return crate::codegen::SELF_CALL_DEOPT,
            Some(s) => Some(s),
            None => {
                // Own miss. A class-instance receiver may resolve a chain
                // SETTER — the interpreter-IC slow helper serves it (or
                // deopts). A non-extensible object must reject the add; an
                // inherited accessor / non-writable data prop on the proto
                // chain governs the write (OrdinarySet) — interpreter cases.
                // Only a provably-clean chain lets us append the key.
                if map.class.is_some() {
                    return crate::codegen::PROP_VIA_IC;
                }
                if !map.extensible {
                    return crate::codegen::SELF_CALL_DEOPT;
                }
                let mut cur = idx;
                let mut hops = 0;
                loop {
                    let next = match vm.proto_of.get(&cur) {
                        Some(p) if p.is_heap() => p.heap_index(),
                        Some(_) => break, // explicit null proto: chain end
                        None => {
                            if vm.obj_proto == 0 || cur == vm.obj_proto {
                                break;
                            }
                            vm.obj_proto
                        }
                    };
                    hops += 1;
                    if hops > 64 || !vm.ic_obj_ok(next) {
                        return crate::codegen::SELF_CALL_DEOPT;
                    }
                    match vm.heap.get(next) {
                        HeapObj::Object(m2) if !m2.is_ctor && m2.class.is_none() => {
                            if let Some(i) = m2.pos(key) {
                                if m2.attrs[i].accessor {
                                    // Inherited setter governs the write —
                                    // frame-called by the slow helper.
                                    return crate::codegen::PROP_VIA_IC;
                                }
                                if !m2.attrs[i].writable {
                                    return crate::codegen::SELF_CALL_DEOPT;
                                }
                                break; // writable chain data: the own add shadows it
                            }
                        }
                        _ => return crate::codegen::SELF_CALL_DEOPT, // exotic link
                    }
                    cur = next;
                }
                None
            }
        },
        // `arr.length = n` truncates/grows — deopt so the interpreter's set_prop
        // applies it (no-op here would diverge from the interpreter).
        HeapObj::Array(_) if key == "length" => return crate::codegen::SELF_CALL_DEOPT,
        // Any other heap receiver — Array, Proxy, Func, Closure, Date, Map, Set,
        // Promise, RegExp, Boxed, TypedArray — must DEOPT, not no-op.
        //
        // This used to `return 0`, which is the helper's SUCCESS code, on the
        // premise that the interpreter also no-ops. It does not: it performs the
        // store. So `for (i…) a.p = i` on an array left `a.p` at whatever value
        // it held when the region compiled, and a Proxy's `set` trap simply
        // stopped firing once the loop got hot — silent data loss, and skipped
        // observable side effects.
        _ => return crate::codegen::SELF_CALL_DEOPT,
    };
    let (added, vals_ptr, slot) = match vm.heap.get_mut(idx) {
        HeapObj::Object(map) => match own {
            Some(s) => {
                map.vals[s] = Value::from_bits(val_bits);
                (false, map.vals.as_ptr() as u64, s as u32)
            }
            None => {
                let added = map.set(key, Value::from_bits(val_bits));
                let s = map.pos(key).unwrap() as u32;
                (added, map.vals.as_ptr() as u64, s)
            }
        },
        _ => return crate::codegen::SELF_CALL_DEOPT, // unreachable (checked above)
    };
    if added {
        vm.heap.bump_version(idx);
    }
    let version = vm.heap.version_of(idx);
    // SetProp sites only ever hold OWN ways (the region's write fast path
    // skips hop checks; chain setters/non-writables deopted above).
    if let Some(e) = crate::codegen::IcEntry::own(obj_bits, vals_ptr, version, slot) {
        vm.jit.set_ic(site_idx, e);
        // Nursery: this way's HITS store into `idx` with NO call (the probe's
        // `mov [vals+slot*8], val`), so no barrier can ever see them.
        // Register the receiver as a persistent minor-trace root instead —
        // its edges are re-scanned at every minor for as long as the slot
        // lives (the way itself dies with the slot's version on free/reuse).
        vm.heap.register_scan_root(idx);
    }
    0
}

/// Win64 helper: base pointer of the heap's per-object version array, pinned by a
/// heap-op region's prologue. Stable for the run (a region never allocates a heap
/// object, so the array doesn't reallocate).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_heap_versions_base(vm: *mut core::ffi::c_void) -> *const u32 {
    let vm = unsafe { &*(vm as *const Vm) };
    vm.heap.versions_ptr()
}

/// Win64 helper: base pointer of the JIT inline-cache table, pinned by a heap-op
/// region's prologue. Stable for the run (the table grows only at compile time,
/// and a `*_miss` only updates an existing slot — never grows it).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_ic_base(vm: *mut core::ffi::c_void) -> *const core::ffi::c_void {
    let vm = unsafe { &*(vm as *const Vm) };
    vm.jit.ic_base_ptr() as *const core::ffi::c_void
}

/// Win64 helper: the base pointer of `vm.globals`, fetched once by an OSR loop
/// region's prologue and pinned in a callee-saved register for direct
/// `LoadGlobal`/`StoreGlobal`. Sound because `globals` is allocated once at VM
/// construction (`global_count` slots) and never reallocates at runtime.
///
/// # Safety
/// `vm` is a valid `*mut Vm` that outlives the region run.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_globals_base(vm: *mut core::ffi::c_void) -> *mut u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    vm.globals.as_mut_ptr() as *mut u64
}

/// Win64 helper for Q4 leaf-call inlining: does the register file have headroom
/// for a carved callee scratch window? `base_ptr` is the running region's window
/// base (its `rbx`); `needed` is the highest scratch slot index used above that
/// base (`reg_window + callee_reg_count`, summed to a max across the region's
/// inlined callees). Returns 1 when `base + needed <= reg_capacity` — i.e. the
/// scratch slots lie inside the pinned, reserved `vm.regs` buffer and writing
/// them can never reallocate (which would dangle the region's `rbx`). Returns 0
/// when it would overflow (only possible in near-MAX_FRAMES recursion), and the
/// region then runs every inlined call through the per-call helper instead.
/// Called ONCE per OSR entry (not per iteration), so its cost is amortised away.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `base_ptr` lies within `vm.regs`' pinned buffer.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_regs_fits(
    vm: *mut core::ffi::c_void,
    base_ptr: *const u64,
    needed: u64,
) -> u64 {
    let vm = unsafe { &*(vm as *const Vm) };
    let regs_base = vm.regs.as_ptr() as *const u64;
    // SAFETY: base_ptr is within the same pinned allocation as regs_base.
    let base = unsafe { base_ptr.offset_from(regs_base) } as usize;
    (base + needed as usize <= vm.reg_capacity_pub()) as u64
}

/// Win64 helper: a UNARY `Math.<op>` over an already-numeric argument. `code` is
/// `MathFn as u32` (`#[repr(u8)]`, fixed declaration order); `x_bits` is the
/// operand's raw f64 bits (the region loaded it as a double after guarding it
/// numeric). PURE — no vm, no allocation, no user code (the region bails to the
/// interpreter on a non-numeric arg, where the observable ToNumber coercion
/// runs). Returns the result's f64 bits. Delegates to the SHARED `math_unary`,
/// so JS quirks (Round half-up, Sign ±0, Clz32/Fround) match the interpreter
/// byte-for-byte.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_math_unary(code: u32, x_bits: u64) -> u64 {
    let op: crate::bytecode::MathFn = unsafe { core::mem::transmute(code as u8) };
    crate::vm::helpers_num2::math_unary(op, f64::from_bits(x_bits)).to_bits()
}

/// Win64 helper: a TWO-ARG `Math.<op>` (`Pow`/`Atan2`/`Imul`/`Min`/`Max`/
/// `Hypot`) over already-numeric arguments (raw f64 bits). PURE — mirrors the
/// `eval_math_args` arms for exactly two operands (Pow's magnitude-1 /
/// NaN|Inf-exponent → NaN deviation; Atan2; Imul's ToUint32×ToUint32 → i32;
/// Min/Max NaN-sticky + −0<+0 ordering; Hypot's ±Inf-forces-+Inf). Returns the
/// result's f64 bits.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_math_two(code: u32, a_bits: u64, b_bits: u64) -> u64 {
    use crate::bytecode::MathFn as M;
    let op: M = unsafe { core::mem::transmute(code as u8) };
    let a = f64::from_bits(a_bits);
    let b = f64::from_bits(b_bits);
    let r = match op {
        M::Pow => {
            if (a == 1.0 || a == -1.0) && (b.is_nan() || b.is_infinite()) {
                f64::NAN
            } else {
                a.powf(b)
            }
        }
        M::Atan2 => a.atan2(b),
        M::Imul => {
            (crate::vm::helpers_num2::to_uint32(a)
                .wrapping_mul(crate::vm::helpers_num2::to_uint32(b)) as i32) as f64
        }
        // Min/Max over exactly two already-numeric args (NaN-sticky; −0<+0, so
        // Min prefers −0 and Max prefers +0 on a tie). Matches eval_math_args.
        M::Min => {
            if a.is_nan() || b.is_nan() {
                f64::NAN
            } else if a == b {
                // tie (incl. ±0): prefer the negative-signed operand
                if a.is_sign_negative() { a } else { b }
            } else {
                a.min(b)
            }
        }
        M::Max => {
            if a.is_nan() || b.is_nan() {
                f64::NAN
            } else if a == b {
                if a.is_sign_positive() { a } else { b }
            } else {
                a.max(b)
            }
        }
        // Hypot over exactly two args: a ±Inf operand forces +Inf even with a NaN
        // partner (eval_math_args sets hypot_inf and returns +Inf). Otherwise the
        // reduction is acc=0; acc += a*a; acc += b*b; then sqrt — i.e.
        // sqrt(a*a + b*b), evaluated in that order.
        M::Hypot => {
            if a.is_infinite() || b.is_infinite() {
                f64::INFINITY
            } else {
                (a * a + b * b).sqrt()
            }
        }
        // Unary ops never reach here (codegen routes argc==1 to jit_math_unary).
        _ => f64::NAN,
    };
    r.to_bits()
}

/// Win64 helper: read a captured local's cell (`CellGet`). `cell_bits` is the
/// register's Value (a Heap-tagged cell); returns the cell's inner Value bits,
/// or `SELF_CALL_DEOPT` when the cell is still UNINITIALIZED (TDZ) so the region
/// bails and the interpreter throws the ReferenceError. PURE read: a single heap
/// load, NO allocation, NO user code, NO GC safe point. The returned bits are
/// stored straight into a frame register (a GC root) by the caller, so nothing
/// is held un-rooted. SOUNDNESS: a closure invoked by a Call/CallMethod earlier
/// in the SAME region can mutate the cell — but this helper is emitted as a
/// per-op load (never hoisted across a call), so each execution re-reads the
/// live value.
///
/// # Safety
/// `vm` is a valid `*const Vm`; `cell_bits` is a Heap-tagged cell Value.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_cell_get(vm: *mut core::ffi::c_void, cell_bits: u64) -> u64 {
    let vm = unsafe { &*(vm as *const Vm) };
    let idx = Value::from_bits(cell_bits).heap_index();
    let v = vm.heap.cell_get(idx);
    if v.is_uninitialized() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    v.bits()
}

/// Win64 helper: `obj["name" + i]` (`GetIndexConcat`), the fused computed-key
/// read. Handles ONLY the non-observable fast path — an Int key, a plain-Object
/// receiver that is not the global object or a namespace, and an own DATA hit —
/// which is exactly the shape `Vm::get_index_concat` short-circuits, and which
/// allocates nothing (the key is built into a reused scratch buffer) and runs no
/// user code. Anything else returns `SELF_CALL_DEOPT` so the interpreter
/// materialises the key and does the full computed read.
///
/// `packed = (func_id << 32) | name_idx`, keeping this to four register args.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_index_concat(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    packed: u64,
    key_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let obj = Value::from_bits(obj_bits);
    let key = Value::from_bits(key_bits);
    if !key.is_int() || !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let oidx = obj.heap_index();
    if (oidx == vm.global_this && vm.global_this != 0)
        || (!vm.module_namespaces.is_empty() && vm.module_namespaces.contains_key(&oidx))
        || (!vm.deferred_ns_state.is_empty() && vm.deferred_ns_state.contains_key(&oidx))
        || !matches!(vm.heap.get(oidx), HeapObj::Object(_))
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let func_id = (packed >> 32) as u32;
    let name = packed as u32;
    let mut scratch = std::mem::take(&mut vm.idx_key_scratch);
    vm.build_concat_key(&mut scratch, name, key.as_int(), func_id);
    let hit = match vm.heap.get(oidx) {
        HeapObj::Object(m) => match m.pos(&scratch) {
            Some(i) if !m.attrs[i].accessor && !m.vals[i].is_uninitialized() => Some(m.vals[i]),
            _ => None,
        },
        _ => None,
    };
    vm.idx_key_scratch = scratch;
    match hit {
        Some(v) => v.bits(),
        // A miss must take the interpreter's slow path (prototype chain,
        // accessors, arrays) — it is not "undefined".
        None => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for `ToConcatKey`: identity for every primitive and for heap
/// strings (their deferred concat at the store runs no user code); the deopt
/// sentinel for a non-string heap value, whose ToPrimitive protocol is user
/// code the interpreter must run. PURE on the non-deopt path — no alloc, so no
/// refetch.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `v_bits` is a valid Value rooted in the caller.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_to_concat_key(vm: *mut core::ffi::c_void, v_bits: u64) -> u64 {
    let vm = unsafe { &*(vm as *const Vm) };
    let v = Value::from_bits(v_bits);
    if !v.is_heap() || vm.heap.is_str_like(v.heap_index()) {
        v_bits
    } else {
        crate::codegen::SELF_CALL_DEOPT
    }
}

/// `ZIPP_NO_CONCAT_APPEND=1` restores the pre-B86 behaviour: a NEW key at a
/// JIT'd `obj["name" + i] = v` deopts instead of appending. Exists so the change
/// is A/B-able with `--ab-env` on ONE binary and bisectable without a rebuild.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn concat_append_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_CONCAT_APPEND").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Win64 helper for a region `SetIndexConcat`: the own writable DATA-slot HIT
/// on a plain object with an Int key, mirroring `jit_get_index_concat` — the
/// key is formatted into the reused scratch buffer (no allocation), the slot
/// value is overwritten in place (no shape change, no version bump — exactly
/// the interpreter's hit arm), and EVERYTHING else deopts: a NEW key (the
/// append reallocs `vals` and bumps the version — let the interpreter do it),
/// a non-writable/accessor slot, `__proto__`, an exotic receiver, a non-Int
/// key. Runs no user code.
///
/// `packed = (func_id << 32) | name_idx`; `val` rides the stack as arg 5.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; all bits are valid rooted Values.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_index_concat(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    packed: u64,
    key_bits: u64,
    val_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let obj = Value::from_bits(obj_bits);
    let key = Value::from_bits(key_bits);
    if !key.is_int() || !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let oidx = obj.heap_index();
    if (oidx == vm.global_this && vm.global_this != 0)
        || oidx == vm.obj_proto
        || !vm.realm_global_objs.is_empty()
        || (!vm.module_namespaces.is_empty() && vm.module_namespaces.contains_key(&oidx))
        || (!vm.deferred_ns_state.is_empty() && vm.deferred_ns_state.contains_key(&oidx))
        || !matches!(vm.heap.get(oidx), HeapObj::Object(_))
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let func_id = (packed >> 32) as u32;
    let name = packed as u32;
    let mut scratch = std::mem::take(&mut vm.idx_key_scratch);
    vm.build_concat_key(&mut scratch, name, key.as_int(), func_id);
    let hit = match vm.heap.get(oidx) {
        HeapObj::Object(m) if scratch != "__proto__" => match m.pos(&scratch) {
            Some(i) if !m.attrs[i].accessor && m.attrs[i].writable => Some(i),
            _ => None,
        },
        _ => None,
    };
    let out = match hit {
        Some(i) => {
            // Nursery barrier: an in-place store into a possibly-old holder
            // (the miss arm delegates to `set_index_concat`, which barriers).
            vm.heap.write_barrier_val(oidx, Value::from_bits(val_bits));
            if let HeapObj::Object(m) = vm.heap.get_mut(oidx) {
                m.vals[i] = Value::from_bits(val_bits);
                0
            } else {
                crate::codegen::SELF_CALL_DEOPT
            }
        }
        // B86: a NEW key used to deopt here, and that was the whole cost of
        // `polymorphic-objects`. Its dict-churn loop builds a FRESH `{}` per
        // outer iteration and writes 60 computed keys into it, so every single
        // write missed, deopted, and — 65 deopts being past `OSR_DEOPT_LIMIT` —
        // got the region EVICTED and blacklisted. The profiler caught it: that
        // row ran **60.5% interpreted** while reporting only six decline
        // messages, because the loop was not being rejected, it was being
        // thrown away after compiling.
        //
        // Delegating to `set_index_concat` — the exact function the interpreter's
        // `Instr::SetIndexConcat` arm calls — makes the semantics identical BY
        // CONSTRUCTION rather than by re-derivation: extensibility, a
        // prototype-chain setter, `__proto__`, canonical-index keys, frozen and
        // sealed receivers, and the strict-mode TypeError all keep whatever
        // behaviour that path already has.
        //
        // It can allocate (the key `String`, the three `Vec` growths) and it can
        // run USER CODE (an inherited setter), so the emitter re-derives the
        // pinned r13/r14 and the TypedArray snapshots after this call — the same
        // treatment every other allocating helper gets. A throw becomes
        // `CALL_THREW` rather than the redo sentinel, because the append may
        // already have run a setter's side effects and re-executing the op in
        // the interpreter would run them twice.
        None if !concat_append_enabled() => crate::codegen::SELF_CALL_DEOPT,
        None => {
            let strict = vm.func(func_id as usize).is_strict;
            let o = Value::from_bits(obj_bits);
            let v = Value::from_bits(val_bits);
            // `scratch` is borrowed back before the call: `set_index_concat`
            // rebuilds the key itself, and leaving the VM's scratch buffer
            // stolen across a re-entrant call would strand it.
            vm.idx_key_scratch = std::mem::take(&mut scratch);
            match vm.set_index_concat(o, name, key, v, strict, func_id) {
                Ok(()) => 0,
                Err(t) => vm.jit_thrown_to_sentinel(t),
            }
        }
    };
    if !scratch.is_empty() || vm.idx_key_scratch.is_empty() {
        vm.idx_key_scratch = scratch;
    }
    out
}

/// Win64 helper: write a captured cell (`CellSet`). A cell is one heap slot and
/// the store is unconditional — no TDZ check (that is `CellSetChecked`), no
/// allocation, no user code, no GC safe point — so it needs no pinned-pointer
/// refetch, exactly like `jit_cell_get`. Always succeeds; returns 0.
///
/// Admitting the WRITE matters because one captured-variable assignment used to
/// decline the entire enclosing region: a loop writing a captured local measured
/// 26-35 ns/iteration against 2.7 for the identical loop over a non-captured
/// local, and these are markdown-render's only region declines.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_cell_set(
    vm: *mut core::ffi::c_void,
    cell_bits: u64,
    val_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let idx = Value::from_bits(cell_bits).heap_index();
    vm.heap.cell_set(idx, Value::from_bits(val_bits));
    0
}

/// Win64 helper: write one of the running closure's captured cells (`UpvalSet`).
/// Resolves the closure from the TOP frame with the same reasoning as
/// `jit_upval_get`, and bails on a malformed closure rather than unwinding
/// across the FFI boundary. Returns 0, or `SELF_CALL_DEOPT`.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_upval_set(
    vm: *mut core::ffi::c_void,
    idx: u32,
    val_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let cur_closure = match vm.frames.last() {
        Some(f) => f.closure,
        None => return crate::codegen::SELF_CALL_DEOPT,
    };
    let cell = match vm.heap.get(cur_closure) {
        crate::heap::HeapObj::Closure { upvalues, .. } => match upvalues.get(idx as usize) {
            Some(&c) => c,
            None => return crate::codegen::SELF_CALL_DEOPT,
        },
        _ => return crate::codegen::SELF_CALL_DEOPT,
    };
    vm.heap.cell_set(cell, Value::from_bits(val_bits));
    0
}

/// Win64 helper: read one of the running closure's captured cells (`UpvalGet`).
/// Resolves the closure from the TOP frame — the OSR region runs in place in
/// that frame, and every helper call returns/pops before the next region op, so
/// `frames.last()` is always the region's frame (matching the interpreter's
/// `cur_closure = frames[len-1].closure`). Returns the inner Value bits, or
/// `SELF_CALL_DEOPT` for a TDZ cell or a malformed-closure edge (interpreter
/// handles it). PURE: heap loads only, no alloc, no user code, no GC safe point.
/// Same per-op no-hoist soundness as `jit_cell_get`.
///
/// # Safety
/// `vm` is a valid `*const Vm`; the region runs inside a frame with a closure.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_upval_get(vm: *mut core::ffi::c_void, idx: u32) -> u64 {
    let vm = unsafe { &*(vm as *const Vm) };
    let cur_closure = match vm.frames.last() {
        Some(f) => f.closure,
        None => return crate::codegen::SELF_CALL_DEOPT,
    };
    // Guard the closure is a real Closure (the interpreter would panic on a
    // malformed one; across an FFI boundary we must NOT unwind — bail instead).
    let cell = match vm.heap.get(cur_closure) {
        crate::heap::HeapObj::Closure { upvalues, .. } => match upvalues.get(idx as usize) {
            Some(&c) => c,
            None => return crate::codegen::SELF_CALL_DEOPT,
        },
        _ => return crate::codegen::SELF_CALL_DEOPT,
    };
    let v = vm.heap.cell_get(cell);
    if v.is_uninitialized() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    v.bits()
}

/// Win64 helper: the per-iteration for-in liveness re-check (`ForInLive`). `obj`
/// is the receiver, `key` the snapshotted key — returns the BOOL Value bits (so
/// the region stores it straight into dst, matching `Value::bool(live)`).
/// Delegates to the SHARED `Vm::forin_live`, so it is byte-identical to the
/// interpreter arm. NON-observable: no getter / Proxy trap fires, and the
/// `&self`/`&mut self` callees (`has_property`/`has_own_property`/`key_of`/the
/// proto walk) never re-enter the dispatch loop. `key_of` may allocate a Rust
/// `String`, which is NOT a VM-heap allocation and cannot trigger the VM GC.
/// We additionally take a `gc_lock_guard` so that even if a future change adds a
/// VM-heap alloc inside the liveness walk, no collection can run while `obj`/
/// `key` are held only as helper-local bit copies. SOUNDNESS: emitted per-op, so
/// a shape change by an earlier user-code helper is reflected next execution.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `obj_bits`/`key_bits` are valid Value bits whose
/// heap objects (if any) are rooted in the caller's frame registers.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_forin_live(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    key_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let _guard = vm.gc_lock_guard();
    let live = vm.forin_live(Value::from_bits(obj_bits), Value::from_bits(key_bits));
    Value::bool(live).bits()
}

/// `ZIPP_ICSTATS=1` counters for the region-compiled `IterNext` helper — the
/// mechanism evidence that a for-of loop region actually stepped its iterator
/// natively instead of blacklisting the whole region (`ZIPP_NO_ITER_REGION=1`
/// restores the decline and forces these to zero).
pub(crate) mod iterstats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static NATIVE_STEPS: AtomicU64 = AtomicU64::new(0);
    static DEOPTS: AtomicU64 = AtomicU64::new(0);
    static ON: AtomicU8 = AtomicU8::new(2);

    #[inline]
    fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_ICSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    /// One `IterNext` served natively by `jit_iter_next` (any of the three
    /// intrinsic step paths). (Counted only from the JIT helper; zero without.)
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn native_step() {
        if enabled() {
            NATIVE_STEPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One `jit_iter_next` deopt (non-intrinsic iterator / unprimed next).
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn deopt() {
        if enabled() {
            DEOPTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(native_steps, deopts)`
    pub fn dump() -> (u64, u64) {
        (NATIVE_STEPS.load(Ordering::Relaxed), DEOPTS.load(Ordering::Relaxed))
    }
}

pub use iterstats::dump as iter_region_stats;

/// Win64 helper for a region `IterNext` — the for-of step over an iterator the
/// engine can drive INTRINSICALLY: a %RegExpStringIterator% (`s.matchAll(re)`),
/// a live %ArrayIterator% (`a.values()`/`keys()`/`entries()`), or a Map/Set/
/// snapshot collection iterator — exactly the three fast paths the
/// interpreter's own `IterNext` arm takes when the primed `next` is the
/// pristine `ITER_NEXT` native (dispatch.rs; the `{value, done}` object an
/// intrinsic `next` would build is engine-internal, so skipping it is
/// unobservable there and equally unobservable here) — plus the interpreter
/// arm's FIRST fast path, the plain dense-Array positional walk (`for (v of
/// arr)`, where GetIterator left the raw Array in the iter register because
/// the iterator protocol was pristine). Everything else — a holey array, a
/// generator, a user `next`, an unprimed (u16::MAX) next register — returns
/// `SELF_CALL_DEOPT` BEFORE any state is touched, so the
/// interpreter re-executes the op with full semantics (repeated deopts evict
/// the region, restoring today's interpreted loop).
///
/// `regs` is the region's frame-register window (`rbx`); `packed` carries the
/// four register numbers. The step is read off `vm` and its results are
/// written STRAIGHT into the frame window (two outputs cannot ride the single
/// return register). A throwing step (`lastIndex` setter, a re-entrant
/// `exec`'s TypeError) routes through `jit_thrown_to_sentinel` → `CALL_THREW`,
/// so the region unwinds WITHOUT re-running the (stateful, non-idempotent)
/// step.
///
/// SAFE POINT: the step ALLOCATES (match-result array + capture strings) and
/// this helper is a region loop's only per-iteration call, so it must carry
/// its own `maybe_gc` — B117's GC-starvation warning is exactly this shape
/// (450k allocating iterations with no safe point balloon the heap and the
/// bill appears as locality, invisible to every correctness test). It runs
/// FIRST, before any Value is copied out of the frame window, so everything
/// it can collect is still rooted in `vm.regs`.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `regs` points at the current frame's register
/// window inside `vm.regs` (the region ABI's rcx), and the four packed
/// register numbers index inside that window.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_iter_next(
    vm: *mut core::ffi::c_void,
    regs: *mut u64,
    packed: u64,
    idx_reg: u32,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    // The region loop's safe point (see above) — everything live is in
    // `vm.regs`, which the GC traces.
    vm.maybe_gc();
    let iter_reg = ((packed >> 48) & 0xFFFF) as usize;
    let next_reg = ((packed >> 32) & 0xFFFF) as usize;
    let value_dst = ((packed >> 16) & 0xFFFF) as usize;
    let done_dst = (packed & 0xFFFF) as usize;
    if next_reg == u16::MAX as usize {
        // Unprimed (destructuring) site: the per-step `Get(it, "next")` is
        // observable — interpreter only. (Admission already rejects these;
        // defensive.)
        iterstats::deopt();
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let it = Value::from_bits(unsafe { *regs.add(iter_reg) });
    if !it.is_heap() {
        // The interpreter THROWS here ("x is not iterable") — deopt so it does.
        iterstats::deopt();
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // ── plain dense Array positional walk ── the interpreter arm's FIRST fast
    // path, mirrored check-for-check (virtual length via `array_js_len`,
    // element overlays, a HOLE → the generic path, i.e. deopt here). On a hit
    // it advances the `idx` cursor register exactly as the interpreter does;
    // on exhaustion it writes ONLY done (the interpreter's `Some(None)` arm
    // leaves value_dst untouched).
    let it_heap_idx = it.heap_index();
    if (vm.array_js_len.is_empty() || !vm.array_js_len.contains_key(&it_heap_idx))
        && !vm.array_elements_overlaid(it_heap_idx)
    {
        let idx_reg = idx_reg as usize;
        let cur = crate::vm::helpers_numeric::array_index(Value::from_bits(unsafe {
            *regs.add(idx_reg)
        }))
        .unwrap_or(0);
        let hit = match vm.heap.get(it_heap_idx) {
            HeapObj::Array(items) => match items.get(cur) {
                Some(v) if !v.is_hole() => Some(Some(*v)),
                Some(_) => None,    // hole → generic path (interpreter)
                None => Some(None), // exhausted
            },
            _ => None,
        };
        match hit {
            Some(Some(v)) => {
                unsafe {
                    *regs.add(value_dst) = v.bits();
                    *regs.add(done_dst) = Value::bool(false).bits();
                    *regs.add(idx_reg) = Value::int((cur + 1) as i32).bits();
                }
                iterstats::native_step();
                return 0;
            }
            Some(None) => {
                unsafe {
                    *regs.add(done_dst) = Value::bool(true).bits();
                }
                iterstats::native_step();
                return 0;
            }
            None => {
                if matches!(vm.heap.get(it_heap_idx), HeapObj::Array(_)) {
                    // A holey dense array: the element's value comes from the
                    // prototype chain — interpreter only.
                    iterstats::deopt();
                    return crate::codegen::SELF_CALL_DEOPT;
                }
            }
        }
    }
    let next = Value::from_bits(unsafe { *regs.add(next_reg) });
    // The primed `next` must be the PRISTINE intrinsic — a user `next`
    // (function or otherwise) runs arbitrary code and builds an observable
    // result object; the interpreter owns that protocol.
    if !it.is_heap()
        || !next.is_heap()
        || !matches!(vm.heap.get(next.heap_index()),
                     HeapObj::Native(n) if *n == crate::vm::native::ITER_NEXT)
    {
        iterstats::deopt();
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let it_idx = it.heap_index();
    // Same receiver-kind gate as the interpreter fast path (dispatch.rs): the
    // three step probes below key off identity side tables / Iterator
    // internals, and each returns `None` WITHOUT mutating when it_idx is not
    // its kind — so probing in the interpreter's exact order and deopting on
    // triple-None is state-identical to the interpreter arm.
    let step = if let Some(s) = vm.regexp_string_iter_step(it_idx) {
        s
    } else if let Some(s) = vm.array_iter_step(it_idx) {
        s
    } else if let Some(p) = vm.collection_iter_step(it_idx) {
        Ok(p)
    } else {
        // TypedArray-backed (per-step bounds check can throw) and every other
        // shape stay on the interpreter's general path.
        iterstats::deopt();
        return crate::codegen::SELF_CALL_DEOPT;
    };
    match step {
        Ok((v, d)) => {
            unsafe {
                *regs.add(value_dst) = v.bits();
                *regs.add(done_dst) = Value::bool(d).bits();
            }
            iterstats::native_step();
            0
        }
        Err(t) => vm.jit_thrown_to_sentinel(t),
    }
}

/// Win64 helper for a region `ToNum` (`+x`) whose operand is not already a
/// number: serves a primitive STRING only — `to_number_strict` on a
/// `Str`/`Cons` runs the pure StringToNumber grammar (no ToPrimitive, no user
/// code, no VM-heap alloc; the result is `Value::num` bits exactly as the
/// interpreter arm builds them). Everything else — bool/null/undefined (kept
/// on the interpreter as before), an object (observable `valueOf`), a BigInt
/// or Symbol (TypeError) — returns `SELF_CALL_DEOPT`; `ToNum` is read-only so
/// re-execution is always sound. This is the `+km[2]` idiom: for-of over
/// `matchAll` summing a numeric capture bailed the whole compiled loop region
/// on EVERY iteration without it.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `bits` is a valid Value rooted in the caller's
/// frame registers.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_to_num(vm: *mut core::ffi::c_void, bits: u64) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let v = Value::from_bits(bits);
    if !v.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    if !matches!(vm.heap.get(v.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. }) {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match vm.to_number_strict(v) {
        Ok(n) => Value::num(n).bits(),
        // Unreachable for a Str/Cons operand; defensive.
        Err(t) => vm.jit_thrown_to_sentinel(t),
    }
}

/// Win64 helper for a region `PushFinally`: push the finally handler frame on
/// the CURRENT interpreter frame, exactly as the interpreter arm does — so
/// that when any later helper in the iteration throws (`CALL_THREW` → region
/// exit → interpreter unwind), the handler stack is in the same state the
/// interpreted loop would have left it in. `packed` = target<<32 |
/// kind_reg<<16 | val_reg (all compile-time constants of the op). No alloc on
/// the VM heap (a Rust Vec push), no user code, total — never deopts.
///
/// # Safety
/// `vm` is a valid `*mut Vm` with at least one live frame (a region only runs
/// inside one).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_push_finally(vm: *mut core::ffi::c_void, packed: u64) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let target = (packed >> 32) as u32;
    let kind_reg = ((packed >> 16) & 0xFFFF) as u16;
    let val_reg = (packed & 0xFFFF) as u16;
    let top = vm.frames.len() - 1;
    vm.frames[top].handlers.push(Handler::Finally { target, kind_reg, val_reg });
    0
}

/// Win64 helper for a region `PopFinally` — the pop half of `jit_push_finally`
/// (the interpreter arm verbatim). Total, never deopts.
///
/// # Safety
/// `vm` is a valid `*mut Vm` with at least one live frame.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_pop_finally(vm: *mut core::ffi::c_void) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let top = vm.frames.len() - 1;
    vm.frames[top].handlers.pop();
    0
}

/// Win64 helper for Tier C `TypeOf` (`typeof v`): `vm.type_of(v)` (a fixed
/// &'static str) materialised to a heap string; returns its Value bits. The
/// downstream `=== "number"` compares by CONTENT via `region_poly_eq`'s slow
/// `jit_strict_eq` path (multi-char strings are non-interned ⇒ index ≥
/// USER_OBJ_START ⇒ slow path), so a fresh alloc each call is CORRECT. ALLOCATES
/// (a real heap Str for multi-char names) ⇒ the caller refetches r13/r14 after
/// this op when it has GetProp. `alloc` never itself collects, so no guard.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `v_bits` is a valid Value whose heap object (if
/// any) is rooted in the caller's frame registers.
/// Win64 helper for a region `StaticFn` — the BOUNDED op set the admission
/// check allows: `Promise.resolve(x)` and the four `Number.is*` predicates,
/// all at exactly one argument. `code` is the emitter's own 0..=4 mapping (not
/// the `StaticFn` discriminant), baked per-site.
///
/// `Promise.resolve` of a NON-heap value provably runs no user code: `resolve`
/// short-circuits the cycle/adoption/thenable protocol for a non-heap value
/// and settles a freshly allocated promise whose reaction Vecs are empty, so
/// no microtask is enqueued (PERF_ROADMAP B42 verified this path). A heap
/// argument returns SELF_CALL_DEOPT — the interpreter runs the identity check
/// (`Get(p, "constructor")`, user-observable) and the thenable protocol. The
/// promise ALLOCATES, so the emitter re-derives r13 after the call (the
/// StrConcat discipline); no user code runs, so r14 and the TA snapshots are
/// safe. The `Number.is*` arms are pure predicates.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `a0_bits` is a valid Value rooted in the caller.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_static_fn(
    vm: *mut core::ffi::c_void,
    code: u32,
    a0_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let a0 = Value::from_bits(a0_bits);
    match code {
        0 => {
            if a0.is_heap() {
                return crate::codegen::SELF_CALL_DEOPT;
            }
            let p = vm.alloc_promise();
            vm.resolve(p, a0);
            Value::heap(p).bits()
        }
        1 => Value::bool(crate::vm::helpers_num2::num_is_integer(a0)).bits(),
        2 => Value::bool(a0.is_double() && a0.as_f64().is_nan()).bits(),
        3 => Value::bool(crate::vm::helpers_num2::num_is_finite(a0)).bits(),
        4 => Value::bool(crate::vm::helpers_num2::num_is_safe_integer(a0)).bits(),
        _ => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for the fused `TypeOfIs` (`typeof a === "lit"`). `code_neg`
/// packs `TypeOfIs::code` in the low byte and `neg` in bit 8. Returns the Bool
/// Value bits. PURE — the classifier allocates nothing and runs no user code,
/// so unlike `jit_typeof` (which builds a heap string) the caller owes no
/// r13/r14 refetch; total — never deopts.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `v_bits` is a valid Value rooted in the caller.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_typeof_is(
    vm: *mut core::ffi::c_void,
    v_bits: u64,
    code_neg: u32,
) -> u64 {
    let vm = unsafe { &*(vm as *const Vm) };
    let t = vm.type_of(Value::from_bits(v_bits));
    let m = crate::bytecode::TYPEOF_NAMES
        .get((code_neg & 0xFF) as usize)
        .is_some_and(|&n| n == t);
    Value::bool(m != (code_neg & 0x100 != 0)).bits()
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_typeof(vm: *mut core::ffi::c_void, v_bits: u64) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    vm.typeof_value(Value::from_bits(v_bits)).bits()
}

/// Win64 helper for Tier C `IsArray` (`Array.isArray(v)`): returns the Bool
/// Value bits, or `SELF_CALL_DEOPT` for the rare throwing case (a revoked
/// Proxy) so the interpreter re-executes the op and throws — safe to redo
/// because the check is side-effect-free. PURE (no alloc, no user code).
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `v_bits` is a valid Value rooted in the caller.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_is_array(vm: *mut core::ffi::c_void, v_bits: u64) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.value_is_array_throwing(Value::from_bits(v_bits)) {
        Ok(b) => Value::bool(b).bits(),
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for Tier C `LenOf` (the for-in/for-of length op): length of a
/// for-in key snapshot Array / Array / String / Cons / Map / Set, else 0.
/// Mirrors the interpreter's `LenOf` arm (dispatch.rs). PURE, total (no deopt).
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `obj_bits` is a valid Value rooted in the caller.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_len_of(vm: *mut core::ffi::c_void, obj_bits: u64) -> u64 {
    let vm = unsafe { &*(vm as *mut Vm) };
    let o = Value::from_bits(obj_bits);
    let v = if o.is_heap() {
        match vm.heap.get(o.heap_index()) {
            HeapObj::Array(items) => len_value(
                vm.array_js_len
                    .get(&o.heap_index())
                    .map_or(items.len(), |&n| n as usize),
            ),
            HeapObj::Str(s) => len_value(s.units()),
            HeapObj::Cons { len, .. } => len_value(*len),
            HeapObj::Map { keys, .. } => {
                len_value(keys.iter().filter(|k| !k.is_hole()).count())
            }
            HeapObj::Set(items) => len_value(items.iter().filter(|v| !v.is_hole()).count()),
            _ => Value::int(0),
        }
    } else {
        Value::int(0)
    };
    v.bits()
}

/// Win64 helper for Tier C `ForInKeys`: materialise the for-in key snapshot
/// Array (a nullish receiver iterates nothing → empty Array; else `to_object`
/// then `for_in_keys`). Returns the Array Value bits, `CALL_THREW` if a Proxy
/// trap / coercion threw (`pending_throw` set — the caller unwinds, never
/// re-executes), or — never `SELF_CALL_DEOPT` here (no redo case). ALLOCATES
/// (a key Array) ⇒ the caller refetches r13/r14 after this op when it has
/// GetProp. `for_in_keys` self-guards its working set (gc_lock_guard), so NO
/// outer guard (which would wrongly suppress GC across `to_object`).
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `obj_bits` is a valid Value rooted in the caller.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_forin_keys(vm: *mut core::ffi::c_void, obj_bits: u64) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let o = Value::from_bits(obj_bits);
    let r = if o.is_nullish() {
        Ok(Value::heap(vm.heap.alloc(HeapObj::Array(Vec::new()))))
    } else {
        vm.to_object(o).and_then(|o| vm.for_in_keys(o))
    };
    match r {
        Ok(v) => v.bits(),
        Err(t) => vm.jit_thrown_to_sentinel(t),
    }
}

/// Win64 helper: the `in` operator (`HasProp`, brand=false) in a region. `key`/
/// `obj` are the operand Value bits — returns the BOOL Value bits (`i in arr`)
/// or `SELF_CALL_DEOPT` when the answer needs user code / a throw (non-object
/// RHS, an object/Symbol key, a Proxy or deferred-namespace anywhere in the
/// chain) so the region bails and the interpreter re-executes the op with full
/// `in` semantics. Delegates to the READ-ONLY, infallible `Vm::has_property_jit`,
/// which on the pure path is byte-identical to the interpreter's `has_property_dyn`
/// (the routine `in` dispatches to). PURE: `&self` heap reads only — no VM-heap
/// alloc, no user code, no GC safe point, so the TypedArray pin snapshots and the
/// r13/r14 IC pointers are unaffected (no post-call refetch needed). A
/// `gc_lock_guard` is taken belt-and-suspenders so a future alloc inside the walk
/// could not collect while `obj`/`key` are held only as helper-local bit copies.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `key_bits`/`obj_bits` are valid Value bits whose
/// heap objects (if any) are rooted in the caller's frame registers.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_has_property(
    vm: *mut core::ffi::c_void,
    key_bits: u64,
    obj_bits: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let _guard = vm.gc_lock_guard();
    match vm.has_property_jit(Value::from_bits(obj_bits), Value::from_bits(key_bits)) {
        Some(present) => Value::bool(present).bits(),
        None => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Normalise a (possibly negative) slice index into `[0, len]`. Negative
/// indices count from the end; out-of-range clamps. Matches JS slice/substring.
pub(crate) fn norm_index(i: i32, len: i32) -> i32 {
    let v = if i < 0 { len + i } else { i };
    v.clamp(0, len)
}

/// A `.length` / array-length result as a JS Number. An `Int` when it fits in
/// i32 (the overwhelmingly common case), otherwise a double — so a length beyond
/// 2^31 (cheap to reach now that ropes concatenate lazily without flattening)
/// reports its true magnitude instead of wrapping negative through `as i32`.
/// Integers up to 2^53 are exact in f64, matching JS.
#[inline]
/// A class private name is stored internally as the property "#name". Such keys
/// are NOT reflectable own properties (hidden from getOwnPropertyNames, keys,
/// for-in, hasOwnProperty, getOwnPropertyDescriptor) even though field/method
/// access reads them directly.
pub(crate) fn is_private_key(k: &str) -> bool {
    k.starts_with('#')
}

/// The numeric binary operators (see `numeric_binop` / `bigint_op` in
/// vm/bigint.rs). `Ushr` exists so the BigInt path can throw its dedicated
/// TypeError AFTER ToNumeric coercion, per spec order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BigOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Ushr,
}

/// Parse a BigInt string: optional sign + decimal, or a `0x`/`0o`/`0b` prefix.
/// `None` ⇒ not a valid BigInt literal (→ SyntaxError at the call site).
/// Values beyond i128 parse exactly into the Big tier (canonical by
/// construction: the i128 parse is attempted first and only a RANGE failure
/// falls through — the digits are pre-validated, so `from_str_radix` cannot
/// fail for any other reason).
pub(crate) fn parse_bigint_str(s: &str) -> Option<crate::vm::bigint::BigVal> {
    use crate::vm::bigint::BigVal;
    let s = s.trim();
    let (neg, body, signed) = match s.strip_prefix('-') {
        Some(r) => (true, r, true),
        None => match s.strip_prefix('+') {
            Some(r) => (false, r, true),
            None => (false, s, false),
        },
    };
    // A NonDecimalIntegerLiteral (0x/0o/0b) must NOT carry a sign — only a decimal
    // StrIntegerLiteral may. The digit run must be non-empty and contain only valid
    // radix digits (from_str_radix would otherwise accept an embedded sign).
    let non_decimal = [("0x", 16u32), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)]
        .iter()
        .find_map(|(p, r)| body.strip_prefix(p).map(|d| (*r, d)));
    let v: BigVal = if let Some((radix, digits)) = non_decimal {
        if signed || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        match i128::from_str_radix(digits, radix) {
            Ok(v) => BigVal::Small(v),
            // Overflow OR an out-of-radix (but alphanumeric) digit: parse_bytes
            // re-validates and yields None for the latter.
            Err(_) => BigVal::Big(num_bigint::BigInt::parse_bytes(digits.as_bytes(), radix)?),
        }
    } else {
        if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        match body.parse::<i128>() {
            Ok(v) => BigVal::Small(v),
            Err(_) => BigVal::Big(num_bigint::BigInt::parse_bytes(body.as_bytes(), 10)?),
        }
    };
    Some(if neg { v.neg() } else { v })
}

#[cfg(all(test, feature = "jit", target_arch = "x86_64"))]
mod chain_fast_tests {
    use super::*;

    /// The regex-log-scan gen loop's shape (the fast helper's target row):
    /// int and const-string chain leaves plus Tier-C call leaves.
    const GEN_SRC: &str = r#"
        "use strict";
        function pad2(n) { return n < 10 ? '0' + n : '' + n; }
        var seed = 12345;
        function rnd() { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed / 0x7fffffff; }
        function ri(n) { return (rnd() * n) | 0; }
        var lens = 0;
        for (var i = 0; i < 30000; i++) {
            var line = '#' + ri(9000) + ' [' + pad2(ri(24)) + ':' + pad2(ri(60)) + ']' +
                       ' status=' + ri(600) + ' bytes=' + ri(100000) + ' ms=' + ri(2000);
            lens += line.length;
        }
        console.log(lens);
    "#;

    fn fixture(src: &str) -> (crate::bytecode::Program, ()) {
        let ast = crate::front::parse_script(src).expect("source parses");
        (crate::compile::compile_program(&ast, src).expect("source compiles"), ())
    }

    /// The non-negotiable pins, called straight through the helper's win64
    /// entry: a same-index leaf (`a += a` shape) and an interned or rope
    /// accumulator take the generic path (fresh index — no raw split borrow);
    /// the in-place arms return the accumulator's own bits WITHOUT any VM
    /// heap allocation (the same-bits refetch-elision premise); an exotic
    /// leaf on a mutable builder returns a fresh index; the capacity hint
    /// preserves content; WTF-8 seams and unit accounting stay exact.
    #[test]
    fn chain_fast_hazard_pins() {
        let (program, ()) = fixture("var x = 0;");
        let mut vm = Vm::new(&program);
        vm.run().expect("program runs");
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;

        // In-place int arm: same bits, content exact, NO VM-heap alloc.
        let acc = Value::heap(vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::new("ab".into()))));
        let len0 = vm.heap.len();
        let r = jit_concat_chain_fast(vm_ptr, acc.bits(), Value::int(42).bits(), 0);
        assert_eq!(r, acc.bits(), "int leaf must append in place");
        assert_eq!(vm.heap.len(), len0, "in-place int arm allocated");
        assert_eq!(vm.display(acc), "ab42");
        let r = jit_concat_chain_fast(vm_ptr, acc.bits(), Value::int(-7).bits(), 0);
        assert_eq!(r, acc.bits());
        assert_eq!(vm.display(acc), "ab42-7");

        // In-place str arm (distinct slot): same bits, no VM-heap alloc.
        let leaf =
            Value::heap(vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::new("cd".into()))));
        let len0 = vm.heap.len();
        let r = jit_concat_chain_fast(vm_ptr, acc.bits(), leaf.bits(), 0);
        assert_eq!(r, acc.bits(), "distinct flat-Str leaf must append in place");
        assert_eq!(vm.heap.len(), len0, "in-place str arm allocated");
        assert_eq!(vm.display(acc), "ab42-7cd");
        assert_eq!(vm.display(leaf), "cd", "leaf must be untouched");

        // Same-index leaf (`a += a`): generic path, FRESH index, both intact.
        let alias =
            Value::heap(vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::new("xy".into()))));
        let r = jit_concat_chain_fast(vm_ptr, alias.bits(), alias.bits(), 0);
        assert_ne!(r, alias.bits(), "self-alias must not take the split-borrow arm");
        let rv = Value::from_bits(r);
        assert_eq!(vm.display(rv), "xyxy");
        assert_eq!(vm.display(alias), "xy");

        // Interned accumulator (single-char slot ≤ INTERN_EMPTY): generic
        // path, fresh result, the interned slot never mutated.
        let interned = Value::heap(b'x' as u32);
        assert_eq!(vm.display(interned), "x");
        let r = jit_concat_chain_fast(vm_ptr, interned.bits(), Value::int(5).bits(), 0);
        assert_ne!(r, interned.bits(), "interned accumulator must not grow in place");
        assert_eq!(vm.display(Value::from_bits(r)), "x5");
        assert_eq!(vm.display(interned), "x");

        // Rope accumulator: generic path (rope semantics inherited).
        let li = vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::new("aaa".into())));
        let ri = vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::new("bbb".into())));
        let rope = Value::heap(vm.heap.alloc_cons(li, ri, 6));
        let r = jit_concat_chain_fast(vm_ptr, rope.bits(), Value::int(5).bits(), 0);
        assert_ne!(r, rope.bits(), "rope accumulator must fall through");
        assert_eq!(vm.display(Value::from_bits(r)), "aaabbb5");

        // Exotic leaf (double) on a mutable builder: generic path, fresh
        // index (the debug_assert premise in the helper runs here too).
        let acc2 =
            Value::heap(vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::new("n=".into()))));
        let r = jit_concat_chain_fast(vm_ptr, acc2.bits(), Value::num(3.5).bits(), 0);
        assert_ne!(r, acc2.bits(), "exotic leaf must take the generic path");
        assert_eq!(vm.display(Value::from_bits(r)), "n=3.5");
        assert_eq!(vm.display(acc2), "n=", "builder untouched by the generic path");

        // Capacity hint: content-preserving re-seat, then in-place appends.
        let acc3 =
            Value::heap(vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::new("ts".into()))));
        let r = jit_concat_chain_fast(vm_ptr, acc3.bits(), Value::int(7).bits(), 256);
        assert_eq!(r, acc3.bits());
        assert_eq!(vm.display(acc3), "ts7");

        // LAST-link hint: the trim stays on the in-place arm, preserves
        // content, and allocates nothing on the VM heap (the same-bits
        // refetch-elision premise covers it too). Exercised on an ASCII
        // builder and on a lone-surrogate one, where the reconstruct has to
        // recover `units`/`wellformed` from the bytes.
        let last = crate::codegen::CHAIN_HINT_LAST as u64 | 256;
        let len0 = vm.heap.len();
        let r = jit_concat_chain_fast(vm_ptr, acc3.bits(), Value::int(8).bits(), last);
        assert_eq!(r, acc3.bits(), "the last-link trim must stay in place");
        assert_eq!(vm.heap.len(), len0, "the trim allocated on the VM heap");
        assert_eq!(vm.display(acc3), "ts78");
        let acc4 = Value::heap(
            vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(0xD83D))),
        );
        let r = jit_concat_chain_fast(vm_ptr, acc4.bits(), Value::int(1).bits(), last);
        assert_eq!(r, acc4.bits());
        match vm.heap.get(acc4.heap_index()) {
            HeapObj::Str(t) => {
                assert_eq!(t.units(), 2, "lone surrogate + '1' is two units");
                assert!(!t.is_wellformed(), "a lone surrogate stays ill-formed");
                assert!(!t.is_ascii());
            }
            other => panic!("trimmed builder degenerated to {other:?}"),
        }

        // Non-ASCII + WTF-8 seam: a lone high surrogate builder and a lone
        // low surrogate leaf canonicalize into ONE astral pair, exact units.
        let hi = Value::heap(
            vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(0xD83D))),
        );
        let lo = Value::heap(
            vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(0xDE00))),
        );
        let len0 = vm.heap.len();
        let r = jit_concat_chain_fast(vm_ptr, hi.bits(), lo.bits(), 0);
        assert_eq!(r, hi.bits());
        assert_eq!(vm.heap.len(), len0);
        match vm.heap.get(hi.heap_index()) {
            HeapObj::Str(s) => {
                assert_eq!(s.units(), 2, "astral pair is two UTF-16 units");
                assert!(s.is_wellformed(), "seam must canonicalize");
                assert_eq!(s.as_bytes(), "\u{1F600}".as_bytes());
            }
            other => panic!("builder degenerated to {other:?}"),
        }
        let snowman =
            Value::heap(vm.heap.alloc(HeapObj::Str(crate::heap::JsStr::new("é☃".into()))));
        let r = jit_concat_chain_fast(vm_ptr, hi.bits(), snowman.bits(), 0);
        assert_eq!(r, hi.bits());
        assert_eq!(vm.display(hi), "\u{1F600}é☃");

        // Non-heap accumulator numeric coincidence: the generic tail CAN
        // legitimately return the accumulator's own (non-heap) bits — the
        // reason the emitted elision also requires the heap tag.
        let r = jit_concat_chain_fast(vm_ptr, Value::int(5).bits(), Value::int(0).bits(), 0);
        assert_eq!(r, Value::int(5).bits());
        assert!(!Value::from_bits(r).is_heap());
    }

    /// Child half of the engagement census: runs the gen shape hot and
    /// prints the fast-helper counters (latched on by the parent's
    /// `ZIPP_ICSTATS=1`; prints zeros when run standalone).
    #[test]
    fn chain_fast_gen_child() {
        let out = crate::run(GEN_SRC).expect("gen source compiles");
        assert!(out.error.is_none(), "gen child error: {:?}", out.error);
        let (fi, fs, fb, rs, tr) = chainstats::dump();
        println!("CHAINSTATS fast_int={fi} fast_str={fs} fallback={fb} reseat={rs} trim={tr}");
    }

    /// Engagement census: the compiled chain arms must actually route links
    /// through `jit_concat_chain_fast`'s in-place arms on the gen shape
    /// (~3 int + ~8 str links/line x 30k lines, minus interpreter warmup).
    #[test]
    fn chain_fast_engages_on_gen_shape() {
        let exe = std::env::current_exe().expect("test exe path");
        let out = std::process::Command::new(&exe)
            .arg("chain_fast_gen_child")
            .arg("--nocapture")
            .env("ZIPP_ICSTATS", "1")
            .output()
            .expect("spawn the test binary");
        assert!(
            out.status.success(),
            "census child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with("CHAINSTATS"))
            .unwrap_or_else(|| panic!("no CHAINSTATS line in:\n{stdout}"));
        let field = |k: &str| -> u64 {
            line.split_whitespace()
                .find_map(|p| p.strip_prefix(k))
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("bad CHAINSTATS field {k} in: {line}"))
        };
        let (fi, fs, rs) = (field("fast_int="), field("fast_str="), field("reseat="));
        let tr = field("trim=");
        assert!(fi > 50_000, "int-leaf fast arm barely engaged: fast_int={fi}\n{line}");
        assert!(fs > 100_000, "str-leaf fast arm barely engaged: fast_str={fs}\n{line}");
        assert!(rs > 10_000, "first-link capacity hint barely engaged: reseat={rs}\n{line}");
        // Every re-seated chain must also reach its LAST link and give the
        // slack back — a reseat count far above the trim count is the
        // permanent-over-allocation regression (measured at +194 MB of
        // retained RSS on the 26-leaf shape when the trim was missing).
        assert!(tr > 10_000, "last-link trim barely engaged: trim={tr}\n{line}");
        assert!(
            tr * 2 > rs,
            "most re-seated chains never trimmed: reseat={rs} trim={tr}\n{line}"
        );
    }

    /// Evidence harness for the real bench row; not part of the suite.
    /// `ZIPP_ICSTATS=1 cargo test --release -p zipp-vm --lib chain_fast_row_counters -- --ignored --nocapture`
    #[test]
    #[ignore = "evidence harness: run explicitly with ZIPP_ICSTATS=1"]
    fn chain_fast_row_counters() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/real/regex-log-scan.js");
        let src = std::fs::read_to_string(path).expect("bench row readable");
        let out = crate::run(&src).expect("row compiles");
        assert!(out.error.is_none(), "row error: {:?}", out.error);
        let (fi, fs, fb, rs, tr) = chainstats::dump();
        println!("CHAINSTATS row fast_int={fi} fast_str={fs} fallback={fb} reseat={rs} trim={tr}");
    }
}
