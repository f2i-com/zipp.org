//! Tier-C helpers for the compiler-proved static object-literal prefix.
//!
//! These helpers deliberately implement only the two bytecodes whose fast
//! semantics are closed over VM-owned state: allocating a fresh ordinary
//! object and appending a compiler-proved-new static data property.  Any
//! malformed/stale input declines before mutation so the interpreter can
//! replay the exact bytecode.

#![allow(unused_imports)]
use super::*;

/// Same-binary ablation for the Tier-C object-literal lane.
///
/// Latching once keeps environment access out of both compilation and native
/// execution.  The feature is default-on; `ZIPP_NO_TIERC_OBJECT_LITERAL=1`
/// restores the old whole-function rejection.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn tierc_object_literal_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_OBJECT_LITERAL").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Independent ablation for fixed-block array literals in Tier C.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn tierc_new_array_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_NEW_ARRAY").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Independent ablation for the exact null/undefined subset of loose equality.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn tierc_loose_null_eq_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_LOOSE_NULL_EQ").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Independent same-binary ablation for Tier C's primitive tagged-Int
/// `String(value)` lane.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn tierc_int_string_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_INT_STRING").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn regs_window_valid(vm: &Vm<'_>, regs: *const u64, reg_count: usize) -> bool {
    if regs.is_null() {
        return false;
    }
    let vm_start = vm.regs.as_ptr() as usize;
    let Some(vm_bytes) = vm.regs.len().checked_mul(std::mem::size_of::<u64>()) else {
        return false;
    };
    let Some(vm_end) = vm_start.checked_add(vm_bytes) else {
        return false;
    };
    let win_start = regs as usize;
    let Some(win_bytes) = reg_count.checked_mul(std::mem::size_of::<u64>()) else {
        return false;
    };
    let Some(win_end) = win_start.checked_add(win_bytes) else {
        return false;
    };
    win_start >= vm_start
        && (win_start - vm_start) % std::mem::size_of::<u64>() == 0
        && win_end <= vm_end
}

/// Allocate the exact ordinary object produced by `Instr::NewObject`.
///
/// The native frame's live values remain in `vm.regs`, so the pre-allocation
/// safe point may collect.  `realm_born` is load-bearing: object literals
/// evaluated by child-realm/module code must inherit that realm's
/// `%Object.prototype%`, exactly like the interpreter arm.
///
/// `hint` is defensively bounded even though well-formed bytecode stores a
/// `u16`; a corrupt direct call cannot request an attacker-sized reserve.
/// Panics are caught inside the FFI boundary.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_new_object(vm: *mut core::ffi::c_void, hint: u32) -> u64 {
    if vm.is_null() || hint > u16::MAX as u32 {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        let idx = vm
            .heap
            .alloc(HeapObj::Object(Box::new(ObjMap::with_capacity(
                hint as usize,
            ))));
        vm.realm_born(idx, vm.obj_proto);
        Value::heap(idx).bits()
    })) {
        Ok(bits) => bits,
        // A panic after Heap::alloc/realm_born may have committed VM-internal
        // state.  Returning the replay sentinel would violate its pure-prefix
        // contract, so this impossible/corruption edge is deliberately
        // fail-stop instead of re-executing NewObject.
        Err(_) => std::process::abort(),
    }
}

/// Allocate a fixed-block array literal from a validated native register
/// window.  `packed = (reg_count << 32) | (arg_base << 16) | argc`.
///
/// The compiler emits `NewArray` only after placing the literal elements in a
/// contiguous register block.  Spread literals use later `ArrayAppend`
/// bytecodes and holey literals contain `LoadHole`; neither operation is
/// admitted by Tier C, so those functions stay on the interpreter path.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_new_array(
    vm: *mut core::ffi::c_void,
    regs: *const u64,
    packed: u64,
) -> u64 {
    if vm.is_null() || regs.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let reg_count = (packed >> 32) as u16 as usize;
    let arg_base = (packed >> 16) as u16 as usize;
    let argc = packed as u16 as usize;
    if argc > 1024 || arg_base.checked_add(argc).is_none_or(|end| end > reg_count) {
        return crate::codegen::SELF_CALL_DEOPT;
    }

    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();

        // A generated caller passes the base of its live window inside
        // `vm.regs`.  Validate the entire declared window before the first
        // unsafe read so a malformed direct FFI call fails closed.
        if !regs_window_valid(vm, regs, reg_count) {
            return crate::codegen::SELF_CALL_DEOPT;
        }

        let mut items = Vec::with_capacity(argc);
        for offset in 0..argc {
            let bits = unsafe { *regs.add(arg_base + offset) };
            items.push(Value::from_bits(bits));
        }
        let idx = vm.heap.alloc(HeapObj::Array(items));
        vm.realm_born(idx, vm.arr_proto);
        Value::heap(idx).bits()
    })) {
        Ok(bits) => bits,
        // Allocation/realm bookkeeping may already be committed.  Never replay
        // after an unwind across this boundary.
        Err(_) => std::process::abort(),
    }
}

/// Exact `String(value)` for a tagged Int.
///
/// The live-value guard precedes every possible effect, so any other value
/// returns `SELF_CALL_DEOPT` and the interpreter resumes at the original
/// `GlobalFn String` bytecode.  Decimal values 0..99 reuse the heap's immutable
/// pinned string prefix (single ASCII digits or the existing two-digit table);
/// all other i32 values take a GC safe point and allocate one flat ASCII
/// primitive string.  A panic after that safe point may follow committed GC or
/// allocation state, so the FFI boundary is fail-stop rather than replaying.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_int_string(vm: *mut core::ffi::c_void, value_bits: u64) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let value = Value::from_bits(value_bits);
    if !value.is_int() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let n = value.as_int();
    if (0..=9).contains(&n) {
        return Value::heap((b'0' as i32 + n) as u32).bits();
    }
    if (10..=99).contains(&n) {
        return Value::heap(crate::heap::INTERN_PAD2_START + n as u32).bits();
    }

    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        // The argument is immediate and every other live Value remains in the
        // native frame's vm.regs window, so collection is safe before copying
        // the stack-local decimal bytes into the new heap string.
        vm.maybe_gc();
        let (digits, start) = super::coerce::fmt_i32_buf(n);
        let text = crate::heap::JsStr::from_ascii(digits[start..].to_vec());
        Value::heap(vm.heap.alloc_js(text)).bits()
    })) {
        Ok(bits) => bits,
        Err(_) => std::process::abort(),
    }
}

/// Exact read-only implementation of Abstract Equality when at least one live
/// operand is nullish.  That subset cannot coerce or execute user code; its only
/// special case is the engine's rooted `[[IsHTMLDDA]]` exotic.  If neither
/// operand is currently nullish, decline before effects.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_loose_null_eq(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &*(vm as *const Vm) };
        let a = Value::from_bits(a_bits);
        let b = Value::from_bits(b_bits);
        if !a.is_nullish() && !b.is_nullish() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let equal = (a.is_nullish() && b.is_nullish())
            || (a.is_heap() && b.is_nullish() && vm.is_htmldda.contains(&a.heap_index()))
            || (b.is_heap() && a.is_nullish() && vm.is_htmldda.contains(&b.heap_index()));
        Value::bool(equal).bits()
    }))
    .unwrap_or(crate::codegen::SELF_CALL_DEOPT)
}

/// Append one compiler-proved-new static data property.
///
/// `packed_name = (func_id << 32) | string_constant_index`.  All fallible
/// identity/table checks happen before the write, making `SELF_CALL_DEOPT` a
/// pure prefix.  Only an exact plain `HeapObj::Object` is mutated; exotic
/// receivers go back to the interpreter's generic `set_prop` path.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_append_data_prop(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    packed_name: u64,
    val_bits: u64,
) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let obj = Value::from_bits(obj_bits);
        if !obj.is_heap() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let obj_idx = obj.heap_index();
        if obj_idx as usize >= vm.heap.len() || !matches!(vm.heap.get(obj_idx), HeapObj::Object(_))
        {
            return crate::codegen::SELF_CALL_DEOPT;
        }

        let func_id = (packed_name >> 32) as u32;
        let name = packed_name as u32 as usize;
        // Unified ids are dense over main functions followed by `eval_funcs`.
        // The module loader appends every imported module proto to that SAME
        // `eval_funcs` table before native planning (and separately records its
        // ranges in `module_func_ranges`), so this bound intentionally accepts
        // loader-added modules as well as eval/new-Function code.
        let func_count = vm.main_func_count.saturating_add(vm.eval_funcs.len());
        if func_id as usize >= func_count {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let key = {
            let func = vm.func(func_id as usize);
            let Some(key) = func.string_constants.get(name).cloned() else {
                return crate::codegen::SELF_CALL_DEOPT;
            };
            key
        };
        let val = Value::from_bits(val_bits);
        if val.is_heap() && val.heap_index() as usize >= vm.heap.len() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let HeapObj::Object(map) = vm.heap.get(obj_idx) else {
            return crate::codegen::SELF_CALL_DEOPT;
        };
        if map.pos(&key).is_some() {
            return crate::codegen::SELF_CALL_DEOPT;
        }

        // A value expression may have re-entered and promoted the still-rooted
        // literal before this append.  Preserve an old-object -> young-value
        // edge just like every other VM mutation choke point.
        vm.heap.write_barrier_val(obj_idx, val);
        let HeapObj::Object(map) = vm.heap.get_mut(obj_idx) else {
            // Revalidated defensively after the barrier.  The barrier cannot
            // mutate the heap object, so this remains a pure decline.
            return crate::codegen::SELF_CALL_DEOPT;
        };
        map.push_data(key, val);
        0
    })) {
        Ok(bits) => bits,
        // `push_data` is the commit point.  Never translate a panic after it
        // (or its write barrier) into SELF_CALL_DEOPT: interpreter replay could
        // append the same property twice and corrupt insertion order/shapes.
        Err(_) => std::process::abort(),
    }
}

#[cfg(all(test, feature = "jit", target_arch = "x86_64"))]
mod tests {
    use super::*;

    fn vm(source: &str) -> Vm<'static> {
        let ast = crate::front::parse_script(source).expect("source parses");
        let program = Box::leak(Box::new(
            crate::compile::compile_program(&ast, source).expect("source compiles"),
        ));
        let mut vm = Vm::new(program);
        vm.run().expect("program runs");
        vm
    }

    fn named_slot(vm: &Vm<'_>, wanted: &str) -> (u32, u32) {
        let count = vm.main_func_count + vm.eval_funcs.len();
        for func_id in 0..count {
            if let Some(name) = vm
                .func(func_id)
                .string_constants
                .iter()
                .position(|name| name == wanted)
            {
                return (func_id as u32, name as u32);
            }
        }
        panic!("static property name not found: {wanted:?}");
    }

    #[test]
    fn helper_builds_exact_plain_object_and_declines_before_bad_appends() {
        let mut vm = vm("function literal(v) { return { boundedKey: v }; }");
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;
        let object_bits = jit_new_object(vm_ptr, 2);
        assert_ne!(object_bits, crate::codegen::SELF_CALL_DEOPT);
        let object = Value::from_bits(object_bits);
        assert!(matches!(
            vm.heap.get(object.heap_index()),
            HeapObj::Object(_)
        ));

        let (func_id, name) = named_slot(&vm, "boundedKey");
        let packed = ((func_id as u64) << 32) | name as u64;
        assert_eq!(
            jit_append_data_prop(vm_ptr, object_bits, packed, Value::int(37).bits()),
            0
        );
        let HeapObj::Object(map) = vm.heap.get(object.heap_index()) else {
            unreachable!()
        };
        assert_eq!(map.len(), 1);
        assert_eq!(map.key_at(0), "boundedKey");
        assert_eq!(map.val_at(0), Value::int(37));

        let before = map.len();
        let bad_name = ((func_id as u64) << 32) | u32::MAX as u64;
        assert_eq!(
            jit_append_data_prop(vm_ptr, object_bits, bad_name, Value::int(9).bits()),
            crate::codegen::SELF_CALL_DEOPT
        );
        let HeapObj::Object(map) = vm.heap.get(object.heap_index()) else {
            unreachable!()
        };
        assert_eq!(map.len(), before, "a declined append must be a pure prefix");
    }

    #[test]
    fn allocation_hint_is_bounded_before_heap_mutation() {
        let mut vm = vm("function literal(v) { return { x: v }; }");
        let before = vm.heap.len();
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;
        assert_eq!(
            jit_new_object(vm_ptr, u32::MAX),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(vm.heap.len(), before);
    }

    #[test]
    fn fixed_array_validates_window_and_preserves_values() {
        let mut vm = vm("function probe() { return 0; }");
        vm.regs = vec![
            Value::UNDEFINED,
            Value::int(11),
            Value::TRUE,
            Value::int(99),
        ];
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;
        let regs = vm.regs.as_ptr() as *const u64;
        let packed = (4u64 << 32) | (1u64 << 16) | 2;
        let bits = jit_new_array(vm_ptr, regs, packed);
        let array = Value::from_bits(bits);
        let HeapObj::Array(items) = vm.heap.get(array.heap_index()) else {
            panic!("helper did not allocate an array")
        };
        assert_eq!(items, &[Value::int(11), Value::TRUE]);

        let before = vm.heap.len();
        let bad_range = (4u64 << 32) | (3u64 << 16) | 2;
        assert_eq!(
            jit_new_array(vm_ptr, regs, bad_range),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(vm.heap.len(), before);
    }

    #[test]
    fn int_string_is_exact_interned_bounded_and_pure_on_decline() {
        let mut vm = vm("function probe() { return 0; }");
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;

        for (n, expected) in [(0, "0"), (9, "9"), (10, "10"), (63, "63"), (99, "99")] {
            let value = Value::from_bits(jit_int_string(vm_ptr, Value::int(n).bits()));
            assert_eq!(vm.display(value), expected);
            let expected_slot = if n < 10 {
                (b'0' as i32 + n) as u32
            } else {
                crate::heap::INTERN_PAD2_START + n as u32
            };
            assert_eq!(value.heap_index(), expected_slot);
        }

        vm.gc_stress = true;
        for (n, expected) in [
            (-1, "-1"),
            (100, "100"),
            (i32::MIN, "-2147483648"),
            (i32::MAX, "2147483647"),
        ] {
            let value = Value::from_bits(jit_int_string(vm_ptr, Value::int(n).bits()));
            assert_eq!(vm.display(value), expected);
        }

        let before = vm.heap.len();
        assert_eq!(
            jit_int_string(vm_ptr, Value::TRUE.bits()),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(
            vm.heap.len(),
            before,
            "guard decline must be allocation-free"
        );
    }

    #[test]
    fn loose_null_subset_includes_undefined_and_htmldda_only() {
        let mut vm = vm("var ready = true;");
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;
        let eq = |a: Value, b: Value| jit_loose_null_eq(vm_ptr, a.bits(), b.bits());
        assert_eq!(eq(Value::NULL, Value::UNDEFINED), Value::TRUE.bits());
        assert_eq!(eq(Value::NULL, Value::int(0)), Value::FALSE.bits());
        assert_eq!(
            eq(Value::int(0), Value::int(0)),
            crate::codegen::SELF_CALL_DEOPT
        );
        let htmldda = Value::heap(*vm.is_htmldda.iter().next().expect("$262.IsHTMLDDA"));
        assert_eq!(eq(htmldda, Value::NULL), Value::TRUE.bits());
    }
}
