//! Tier-C helpers for capture-free function values and object-literal homes.
//!
//! `MakeFunc` is allocation plus two pieces of execution-context metadata:
//! the active callee's Realm and inherited dynamic EvalScope. Frame-free
//! native cross-calls have no callee `Frame`, so entry code pins the exact
//! callable in `Vm::jit_tierc_callee`; this module validates that identity
//! against immutable bytecode before committing anything.

use super::*;

/// Same-binary ablation for capture-free `MakeFunc` and `SetHomeObject`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn tierc_makefunc_home_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_MAKEFUNC_HOME").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct MakeFuncPlan {
    child: u32,
    eval_scope: Option<u32>,
    realm: u32,
}

/// Revalidate the immutable MakeFunc descriptor and the exact active callable.
/// This phase is read-only, so any failure remains safe to replay at the
/// original bytecode.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn make_func_preflight(vm: &Vm<'_>, packed_fip: u64) -> Option<MakeFuncPlan> {
    let caller = (packed_fip >> 32) as u32 as usize;
    let ip = packed_fip as u32 as usize;
    // Loader-installed module protos live in the same append-only eval table;
    // this total therefore covers both main and module ids. Ordinary eval ids
    // can be interleaved, so retain the JIT eligibility boundary as well.
    let func_count = vm.main_func_count.checked_add(vm.eval_funcs.len())?;
    if caller >= func_count || !vm.jit_func_eligible(caller as u32) {
        return None;
    }
    let child = match *vm.func(caller).code.get(ip)? {
        Instr::MakeFunc { func_id, .. } => func_id,
        _ => return None,
    };
    if child as usize >= func_count || !vm.jit_func_eligible(child) {
        return None;
    }
    let child_proto = vm.func(child as usize);
    // Compiler MakeFunc is capture-free and never an arrow. Repeat those
    // invariants here so stale/corrupt immutable metadata cannot create a
    // callable with missing lexical-this/upvalue state.
    if child_proto.lexical_this || !child_proto.upvalues.is_empty() {
        return None;
    }

    let active = vm.jit_tierc_callee;
    if active == NO_CLOSURE || active as usize >= vm.heap.len() {
        return None;
    }
    let active_fid = match vm.heap.get(active) {
        HeapObj::Func(fid) | HeapObj::Closure { func: fid, .. } => *fid as usize,
        _ => return None,
    };
    if active_fid != caller {
        return None;
    }
    let active_value = Value::heap(active);
    Some(MakeFuncPlan {
        child,
        // A Func as well as a Closure can carry an EvalScope: MakeFunc stamps
        // every callable created under one, independent of captured upvalues.
        eval_scope: vm.closure_eval_scope.get(&active).copied(),
        realm: vm.get_function_realm(active_value),
    })
}

/// Allocate the exact capture-free callable produced by `Instr::MakeFunc`.
/// `packed_fip = (caller_func_id << 32) | ip`; the child id is re-read from
/// immutable bytecode rather than trusted as generated side metadata.
///
/// Every declining check is completed before the GC/allocation commit. A panic
/// after that point is fail-stop: translating it into SELF_CALL_DEOPT could
/// replay an allocation whose Realm/EvalScope side-table writes partly landed.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_make_func(vm: *mut core::ffi::c_void, packed_fip: u64) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let plan = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &*(vm as *const Vm) };
        make_func_preflight(vm, packed_fip)
    })) {
        Ok(Some(plan)) => plan,
        Ok(None) | Err(_) => return crate::codegen::SELF_CALL_DEOPT,
    };

    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        // `jit_tierc_callee`, its EvalScope and all frame/register Values are
        // explicit roots. The collector is non-moving; the preflighted ids
        // remain valid after this safe point.
        vm.maybe_gc();
        let idx = vm.heap.alloc(HeapObj::Func(plan.child));
        if let Some(scope) = plan.eval_scope {
            vm.closure_eval_scope.insert(idx, scope);
        }
        if plan.realm != 0 {
            vm.obj_realm.insert(idx, plan.realm);
        }
        Value::heap(idx).bits()
    })) {
        Ok(bits) => bits,
        Err(_) => std::process::abort(),
    }
}

/// Exact `SetHomeObject` side-table write.
///
/// A non-heap method is the interpreter's explicit no-op. Heap indices are
/// validated before the write barrier or HashMap mutation; after that commit
/// point any panic is fail-stop, never interpreter replay.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_home_object(
    vm: *mut core::ffi::c_void,
    method_bits: u64,
    home_bits: u64,
) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let preflight = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &*(vm as *const Vm) };
        let method = Value::from_bits(method_bits);
        let home = Value::from_bits(home_bits);
        if !method.is_heap() {
            return Some(None);
        }
        if method.heap_index() as usize >= vm.heap.len()
            || (home.is_heap() && home.heap_index() as usize >= vm.heap.len())
        {
            return None;
        }
        Some(Some((method.heap_index(), home)))
    }));
    let write = match preflight {
        Ok(Some(write)) => write,
        Ok(None) | Err(_) => return crate::codegen::SELF_CALL_DEOPT,
    };
    let Some((method, home)) = write else {
        return Value::UNDEFINED.bits();
    };

    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        // Includes the old-method -> young-home generational barrier before
        // publishing the keyed strong edge.
        vm.record_closure_home(method, home);
        Value::UNDEFINED.bits()
    })) {
        Ok(bits) => bits,
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

    fn make_func_site(vm: &Vm<'_>) -> (u32, usize, u32) {
        (0..vm.main_func_count)
            .find_map(|caller| {
                vm.func(caller)
                    .code
                    .iter()
                    .enumerate()
                    .find_map(|(ip, op)| match *op {
                        Instr::MakeFunc { func_id, .. } => Some((caller as u32, ip, func_id)),
                        _ => None,
                    })
            })
            .expect("MakeFunc site")
    }

    #[test]
    fn make_func_copies_exact_active_realm_and_eval_scope_across_gc() {
        let mut vm = vm("function maker() { const f = function child() {}; return f; }");
        let (caller, ip, child) = make_func_site(&vm);
        let active = vm.heap.alloc(HeapObj::Func(caller));
        let scope = vm
            .heap
            .alloc(HeapObj::EvalScope(std::collections::HashMap::new()));
        vm.closure_eval_scope.insert(active, scope);
        vm.obj_realm.insert(active, 7);
        vm.jit_tierc_callee = active;
        vm.gc_stress = true;

        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;
        let packed = ((caller as u64) << 32) | ip as u64;
        let made = Value::from_bits(jit_make_func(vm_ptr, packed));
        assert!(matches!(vm.heap.get(made.heap_index()), HeapObj::Func(fid) if *fid == child));
        assert_eq!(vm.closure_eval_scope.get(&made.heap_index()), Some(&scope));
        assert_eq!(vm.obj_realm.get(&made.heap_index()), Some(&7));
        assert!(!vm.heap.free_indices().contains(&active));
    }

    #[test]
    fn make_func_declines_before_allocation_without_exact_active_callable() {
        let mut vm = vm("function maker() { return function child() {}; }");
        let (caller, ip, _) = make_func_site(&vm);
        vm.jit_tierc_callee = NO_CLOSURE;
        let before = vm.heap.len();
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;
        let packed = ((caller as u64) << 32) | ip as u64;
        assert_eq!(
            jit_make_func(vm_ptr, packed),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(vm.heap.len(), before);
    }

    #[test]
    fn set_home_uses_the_existing_barrier_edge_and_declines_purely() {
        let mut vm = vm("var ready = true;");
        let method = vm.heap.alloc(HeapObj::Func(0));
        let home = vm.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;

        assert_ne!(
            jit_set_home_object(vm_ptr, Value::heap(method).bits(), Value::heap(home).bits()),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(vm.closure_home.get(&method), Some(&Value::heap(home)));

        let before = vm.closure_home.len();
        let invalid = Value::heap(u32::MAX - 1);
        assert_eq!(
            jit_set_home_object(vm_ptr, invalid.bits(), Value::heap(home).bits()),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(vm.closure_home.len(), before);
        assert_ne!(
            jit_set_home_object(vm_ptr, Value::TRUE.bits(), invalid.bits()),
            crate::codegen::SELF_CALL_DEOPT,
            "the interpreter ignores home when method is non-heap"
        );
    }
}
