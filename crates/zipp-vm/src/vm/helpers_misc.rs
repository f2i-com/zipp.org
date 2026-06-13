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
    // Only a numeric key on a heap object is handled here; a string/other key
    // (or non-heap receiver) deopts so the interpreter applies full semantics.
    if !arr.is_heap() || !key.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: read-only view; the running region holds no conflicting borrow.
    let vm = unsafe { &*(vm as *const Vm) };
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
    if !arr.is_heap() || !key.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let i = match array_index(key) {
        Some(i) => i,
        None => return crate::codegen::SELF_CALL_DEOPT, // negative/fractional → interpreter
    };
    // SAFETY: exclusive view; the running region holds no conflicting borrow and
    // pins only the register file (not the array's Vec, which may reallocate).
    let vm = unsafe { &mut *(vm as *mut Vm) };
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
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
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
        _ => return 0, // other heap non-Object props: silent no-op (matches interpreter)
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

