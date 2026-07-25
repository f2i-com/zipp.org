#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
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
pub(crate) fn function_global_slot(f: &crate::bytecode::FuncProto) -> Option<u16> {
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

/// Win64 helper for `s.substring(a, b)` / `s.slice(a, b)` inside a compiled
/// region — the same JIT-intrinsic shape as `jit_str_index_of`. `is_slice`
/// selects the two different clamping rules (`slice` counts a negative index
/// from the end and yields "" when start >= end; `substring` clamps negatives to
/// 0 and SWAPS a reversed pair).
///
/// Restricted to an ASCII flat receiver with two Int arguments, where UTF-16
/// unit offsets are byte offsets; anything else returns the deopt sentinel and
/// the interpreter runs the full method at that ip.
///
/// # Safety
/// `vm` is the live `Vm`; the operands are raw Value bits from the reg file.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_str_substring(
    vm: *mut core::ffi::c_void,
    recv_bits: u64,
    packed_args: u64,
    is_slice: u64,
) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let r = Value::from_bits(recv_bits);
    if !r.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let (a, b) = (
        Value::from_bits(unsafe { *(packed_args as *const u64) }),
        Value::from_bits(unsafe { *((packed_args as *const u64).add(1)) }),
    );
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
    let (Some(ax), Some(bx)) = (as_i64(a), as_i64(b)) else {
        return crate::codegen::SELF_CALL_DEOPT;
    };
    vm.heap.flatten(r.heap_index());
    let len = match vm.heap.get(r.heap_index()) {
        crate::heap::HeapObj::Str(js) if js.is_ascii() => js.units() as i64,
        _ => return crate::codegen::SELF_CALL_DEOPT,
    };
    let (mut x, mut y) = (ax, bx);
    if is_slice != 0 {
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
    // An array with a side table may carry a defineProperty'd index whose value or
    // accessor lives in arr_props — deopt so the interpreter's override-aware
    // get_index runs (keeps JIT/interpreter parity).
    if arr.is_heap() && vm.arr_props.contains_key(&arr.heap_index()) {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match vm.heap.get(arr.heap_index()) {
        HeapObj::Array(items) => match array_index(key) {
            // In range and present → the element. A HOLE must NOT be returned (it is
            // an internal sentinel): deopt so the interpreter's get_index applies the
            // absent-index / prototype semantics. Out of range / negative /
            // non-integral → undefined (matches JS and the interpreter).
            Some(i) if i < items.len() => {
                if items[i].is_hole() {
                    crate::codegen::SELF_CALL_DEOPT
                } else {
                    items[i].bits()
                }
            }
            _ => Value::UNDEFINED.bits(),
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

/// Win64 helper for a JIT'd dense-array element write `a[i] = v` (`SetIndex`).
/// Stores in place when `i < len`, grows the array with `undefined` holes when
/// `i >= len` (matching JS and the interpreter's set_index). Returns `0` on
/// success, or `SELF_CALL_DEOPT` for a non-array receiver / negative / fractional
/// / non-numeric key (the interpreter then applies its no-op fallback). Reads the
/// live array fresh each call — no cached pointer, so a grow that reallocates is
/// safe (the region pins only the register file, never array storage).
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
    // ── plain-object computed write: `o[k] = v` overwriting an EXISTING own
    // writable data slot ────────────────────────────────────────────────────
    // Deliberately narrower than the read arm. Only an in-place value store on a
    // slot that already exists is handled, so nothing observable happens: no
    // shape change, no `vals` reallocation (the JIT inline caches address values
    // through `vals_ptr + slot`, so an existing-slot store cannot invalidate
    // them), no prototype involvement (an own data property shadows any
    // inherited setter), and no length/index bookkeeping.
    //
    // Everything else keeps deopting — a NEW key (shape change), an accessor
    // (runs user code), a non-writable slot (frozen/sealed objects land here,
    // and strict mode must throw), an uninitialised TDZ slot, the slot-backed
    // global object, and module / deferred namespaces (live bindings).
    if key.is_heap() {
        let oidx = arr.heap_index();
        if !(oidx == vm.global_this && vm.global_this != 0)
            && !(!vm.module_namespaces.is_empty() && vm.module_namespaces.contains_key(&oidx))
            && !(!vm.deferred_ns_state.is_empty() && vm.deferred_ns_state.contains_key(&oidx))
            && !vm.arr_props.contains_key(&oidx)
        {
            let flat = matches!(
                vm.heap.str_wtf8_cow(key.heap_index()),
                Some(std::borrow::Cow::Borrowed(_))
            );
            if flat {
                let k = match vm.heap.str_wtf8_cow(key.heap_index()) {
                    Some(std::borrow::Cow::Borrowed(b)) => std::str::from_utf8(b).ok().map(|x| x.to_string()),
                    _ => None,
                };
                if let Some(k) = k {
                    if let HeapObj::Object(m) = vm.heap.get_mut(oidx) {
                        if let Some(i) = m.pos(&k) {
                            let a = m.attrs[i];
                            if !a.accessor && a.writable && !m.vals[i].is_uninitialized() {
                                m.vals[i] = Value::from_bits(val_bits);
                                return 0;
                            }
                        }
                    }
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
        if kind == crate::codegen::ARR_PIN_KIND as u32 {
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

/// `dst = a + b` for the OSR region's `StrAppendInPlace` op: appends into `a`'s
/// buffer in place when uniquely owned (see `str_append_inplace`). Never deopts
/// (string append doesn't throw); always returns the result bits.
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
    vm.str_append_inplace(a, b).bits()
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
    let prog = vm.program; // &'p Program, independent of `vm`'s borrow
    let key = &prog.functions[func_id as usize].string_constants[name_idx as usize];
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
    let (val, vals_ptr, slot) = match vm.heap.get(idx) {
        HeapObj::Object(map) => match map.pos(key) {
            // An accessor slot stores the GETTER, not a data value — route to
            // the interpreter-IC slow helper, which frame-calls it.
            Some(s) if map.attrs[s].accessor => return crate::codegen::PROP_VIA_IC,
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
                                    // interpreter-IC slow helper.
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
    val.bits()
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
    let prog = vm.program;
    let key = &prog.functions[func_id as usize].string_constants[name_idx as usize];
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
    // Pre-checks against a shared borrow (the write below re-borrows mutably).
    let own = match vm.heap.get(idx) {
        HeapObj::Object(map) => match map.pos(key) {
            // An accessor's SETTER must run (user code) — the interpreter-IC
            // slow helper frame-calls it.
            Some(s) if map.attrs[s].accessor => return crate::codegen::PROP_VIA_IC,
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
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_typeof(vm: *mut core::ffi::c_void, v_bits: u64) -> u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let t: &'static str = vm.type_of(Value::from_bits(v_bits));
    vm.alloc_str(t.to_string()).bits()
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

