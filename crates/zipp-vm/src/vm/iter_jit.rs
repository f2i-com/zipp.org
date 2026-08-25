//! Tier-C helpers for the sync `for-of` machinery and its finally bracket.
//!
//! An application-shaped tree walk (`for (const k of Object.keys(o))`,
//! `key in other`, recursion) used to blacklist its whole function: the
//! iterator ops, the close-on-abrupt-exit finally bracket, `ObjectKeys` and
//! `HasProp` all had no Tier-C arm. Each helper here implements exactly the
//! interpreter arm's built-in FAST PATH — the pristine dense-array iterator,
//! the no-op normal-completion finally tails, the ordinary-object key
//! snapshot — and declines everything else as a pure prefix, so the
//! interpreter replays observable protocol steps (patched iterators,
//! generators, proxies, abrupt completions) exactly once.
//!
//! The finally bracket itself reuses the REGION helpers (`jit_push_finally` /
//! `jit_pop_finally` / `jit_iter_next` / `jit_has_property`), which mutate the
//! ACTIVE FRAME's state. Functions containing handler ops are excluded from
//! native cross-entries (codegen), so a frame-free activation can never reach
//! them: every invocation is an ordinary framed call, and an unwind through a
//! native throw finds the handlers exactly where the interpreter's own
//! unwinder looks for them.

use super::*;

/// Same-binary ablation for the whole iterator/finally Tier-C lane.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn tierc_iter_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_ITER").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// `GetIterator { dst, src }` — the pristine dense-array identity subset. All
/// four proofs are pure reads: no own `@@iterator` (or any named override) on
/// the array, the DEFAULT `%Array.prototype%` link, `%Array.prototype%`'s own
/// `@@iterator` being the pristine data method, and the pristine
/// `%ArrayIteratorPrototype%.next`. Anything else — including the observable
/// replaced-iterator call and every non-array iterable — declines so the
/// interpreter performs the real protocol exactly once.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_iterator(vm: *mut core::ffi::c_void, v_bits: u64) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &*(vm as *const Vm) };
        let v = Value::from_bits(v_bits);
        if !v.is_heap() || v.heap_index() as usize >= vm.heap.len() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let idx = v.heap_index();
        if !matches!(vm.heap.get(idx), HeapObj::Array(_)) {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        // Any named side-table property or prototype override could shadow or
        // redirect `@@iterator`; both are rare on iterated arrays.
        if vm.arr_props.get(&idx).is_some() || vm.proto_of.get(&idx).is_some() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let pristine_data = |holder: u32, key: &str, expected: Value| -> bool {
            match vm.heap.get(holder) {
                HeapObj::Object(map) => match map.pos(key) {
                    Some(i) => !map.attrs[i].accessor && map.vals[i] == expected,
                    None => false,
                },
                _ => false,
            }
        };
        if !pristine_data(vm.arr_proto, "@@iterator", vm.default_array_iter)
            || !pristine_data(vm.array_iter_proto, "next", vm.default_array_iter_next)
        {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        v_bits
    }))
    .unwrap_or(crate::codegen::SELF_CALL_DEOPT)
}

/// `IterPrime { dst, iter }` — built-in fast iterables perform no observable
/// `next` get and prime `undefined`; user-object/proxy/iterator-object kinds
/// decline (their get is observable).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_iter_prime(vm: *mut core::ffi::c_void, it_bits: u64) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &*(vm as *const Vm) };
        let it = Value::from_bits(it_bits);
        if it.is_heap() {
            if it.heap_index() as usize >= vm.heap.len() {
                return crate::codegen::SELF_CALL_DEOPT;
            }
            if matches!(
                vm.heap.get(it.heap_index()),
                HeapObj::Object(_)
                    | HeapObj::Proxy { .. }
                    | HeapObj::Iterator { .. }
                    | HeapObj::IterHelper { .. }
                    | HeapObj::Intl { .. }
            ) {
                return crate::codegen::SELF_CALL_DEOPT;
            }
        }
        Value::UNDEFINED.bits()
    }))
    .unwrap_or(crate::codegen::SELF_CALL_DEOPT)
}

/// `EndFinally { kind_reg, val_reg }` — only the NORMAL completion resumes
/// inline (a no-op fall-through); return/throw/jump completions decline so the
/// interpreter routes them through its full finally/unwind machinery.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_end_finally(vm: *mut core::ffi::c_void, kind_bits: u64) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let kind = Value::from_bits(kind_bits);
    if kind.is_int() && kind.as_int() & 3 == 0 {
        0
    } else {
        crate::codegen::SELF_CALL_DEOPT
    }
}

/// `IterCloseFinally { iter, kind_reg }` — normal and jump completions do not
/// close (the loop exhausted or re-enters); return/throw completions decline
/// (IteratorClose runs observable user code).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_iter_close_finally(
    vm: *mut core::ffi::c_void,
    kind_bits: u64,
) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let kind = Value::from_bits(kind_bits);
    if kind.is_int() && !matches!(kind.as_int() & 3, 1 | 2) {
        0
    } else {
        crate::codegen::SELF_CALL_DEOPT
    }
}

/// `ObjectKeys { dst, obj }` — the own enumerable string-key snapshot for
/// ordinary objects and arrays (no traps, no user code). Proxies, primitives,
/// and nullish receivers decline purely.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_object_keys(vm: *mut core::ffi::c_void, obj_bits: u64) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let o = Value::from_bits(obj_bits);
        if !o.is_heap() || o.heap_index() as usize >= vm.heap.len() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        if !matches!(
            vm.heap.get(o.heap_index()),
            HeapObj::Object(_) | HeapObj::Array(_)
        ) {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        vm.maybe_gc();
        match vm.object_enum_own(o, EnumWhat::Keys) {
            Ok(v) => v.bits(),
            Err(t) => vm.jit_thrown_to_sentinel(t),
        }
    })) {
        Ok(bits) => bits,
        Err(_) => std::process::abort(),
    }
}
