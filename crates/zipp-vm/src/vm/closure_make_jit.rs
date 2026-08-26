//! Tier-C helpers for CLOSURE-CREATING bytecode: capture cells and
//! `MakeClosure`/`MakeArrow`.
//!
//! `MakeFunc` (capture-free) landed first because it needs no lexical state.
//! Application-shaped code creates real closures in its hot paths — a React
//! render allocates a handler arrow per item — and rejecting those ops
//! blacklists the whole enclosing function. Every helper here revalidates the
//! immutable creation site against the exact ACTIVE callable
//! (`Vm::jit_tierc_activation`, or the verified top frame for a frame-backed
//! body) before committing, and every decline happens before the first
//! observable effect so the interpreter replays the exact bytecode.

use super::*;

/// Same-binary ablation for the whole closure-creation lane.
///
/// DEFAULT-ON again since B184: B181 parked it because a widened body could
/// reach a mid-body `SELF_CALL_DEOPT` after an effect (the general
/// `CallMethod` miss and the arrow lexical-`this` resolution), which a
/// cross-called frame can only survive by replaying the whole call. Both
/// deopt edges now COMPLETE through the interpreter-equivalent slow path
/// (`jit_method_builtin_fallback`'s general tail; `call_value` for the
/// arrow rebinding), so the replay hazard those edges carried is gone.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn tierc_closure_make_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_CLOSURE_MAKE").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// The verified execution context for one creation site.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct MakeSite {
    /// Heap index of the exact active callable (Func or Closure).
    active: u32,
    /// The activation's closure for `ParentUpval` capture sources, or
    /// `NO_CLOSURE`.
    closure: u32,
    /// Whether a real interpreter `Frame` backs this activation (whole-function
    /// Tier-C entry from the interpreter); frame-free means a native
    /// cross-call, which is always an ordinary (non-construct) call.
    frame_backed: bool,
}

/// Validate the window pointer and the activation identity for `caller`.
/// Read-only; every failure is safe to replay.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn make_site_preflight(
    vm: &Vm<'_>,
    regs: *const u64,
    reg_count: usize,
    caller: usize,
) -> Option<MakeSite> {
    let func_count = vm.main_func_count.checked_add(vm.eval_funcs.len())?;
    if caller >= func_count
        || !vm.jit_func_eligible(caller as u32)
        || !crate::vm::regs_window_valid(vm, regs, reg_count)
        || reg_count < vm.func(caller).reg_count.max(1) as usize
    {
        return None;
    }
    if !vm.jit_tierc_activation.active {
        return None;
    }
    let active = vm.jit_tierc_activation.callee;
    if active == NO_CLOSURE || active as usize >= vm.heap.len() {
        return None;
    }
    let (active_fid, active_closure) = match vm.heap.get(active) {
        HeapObj::Func(fid) => (*fid as usize, NO_CLOSURE),
        HeapObj::Closure { func, .. } => (*func as usize, active),
        _ => return None,
    };
    if active_fid != caller {
        return None;
    }
    let frame_backed = !vm.jit_tierc_activation.frame_free;
    if frame_backed {
        // The top frame must BE this activation, or its new-target/eval state
        // would describe someone else's call.
        let frame = vm.frames.last()?;
        if frame.func as usize != caller {
            return None;
        }
    }
    Some(MakeSite {
        active,
        closure: active_closure,
        frame_backed,
    })
}

/// Resolve the child proto's capture sources against the live window and
/// activation closure. Read-only; `None` declines before any effect.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn resolve_upvalues(
    vm: &Vm<'_>,
    regs: *const u64,
    reg_count: usize,
    site: MakeSite,
    child: usize,
) -> Option<Vec<u32>> {
    let sources = &vm.func(child).upvalues;
    let mut cells = Vec::with_capacity(sources.len());
    for src in sources {
        let cell = match *src {
            UpvalSource::ParentLocal(reg) => {
                if reg as usize >= reg_count {
                    return None;
                }
                let v = Value::from_bits(unsafe { *regs.add(reg as usize) });
                // The interpreter trusts compiled bytecode here; the helper
                // additionally requires a live heap Cell so a malformed direct
                // call cannot mint a dangling capture edge.
                if !v.is_heap() || v.heap_index() as usize >= vm.heap.len() {
                    return None;
                }
                if !matches!(vm.heap.get(v.heap_index()), HeapObj::Cell(_)) {
                    return None;
                }
                v.heap_index()
            }
            UpvalSource::ParentUpval(idx) => {
                if site.closure == NO_CLOSURE {
                    return None;
                }
                match vm.heap.get(site.closure) {
                    HeapObj::Closure { upvalues, .. } => *upvalues.get(idx as usize)?,
                    _ => return None,
                }
            }
        };
        cells.push(cell);
    }
    Some(cells)
}

/// `packed_fip = (caller_func_id << 32) | ip`. The op (and thus the child id,
/// the cell register, `this_reg`) is re-read from immutable bytecode rather
/// than trusted as generated side metadata.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn site_instr(vm: &Vm<'_>, packed_fip: u64) -> Option<(usize, Instr)> {
    let caller = (packed_fip >> 32) as u32 as usize;
    let ip = packed_fip as u32 as usize;
    let func_count = vm.main_func_count.checked_add(vm.eval_funcs.len())?;
    if caller >= func_count {
        return None;
    }
    Some((caller, vm.func(caller).code.get(ip)?.clone()))
}

/// `MakeCell { reg }` / `MakeCellTdz { reg }` / `MakeCellFnName { reg }` /
/// `MarkCellConst { reg }` — one helper, discriminated by the re-read op.
/// Cell creation writes the fresh cell back into the live window slot, so a
/// decline must happen before the allocation and the write together.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_make_cell(
    vm: *mut core::ffi::c_void,
    regs: *mut u64,
    packed_fip: u64,
    reg_count: u64,
) -> u64 {
    if vm.is_null() || regs.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let reg_count = reg_count as usize;
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let Some((caller, instr)) = site_instr(vm, packed_fip) else {
            return crate::codegen::SELF_CALL_DEOPT;
        };
        if make_site_preflight(vm, regs, reg_count, caller).is_none() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        match instr {
            Instr::MakeCell { reg } | Instr::MakeCellFnName { reg }
                if (reg as usize) < reg_count =>
            {
                // The boxed value lives in the window (a GC root) until the
                // cell is committed, so the safe point precedes the read.
                vm.maybe_gc();
                let v = Value::from_bits(unsafe { *regs.add(reg as usize) });
                let cell = vm.heap.alloc(HeapObj::Cell(v));
                if matches!(instr, Instr::MakeCellFnName { .. }) {
                    vm.fn_name_cells.insert(cell);
                }
                unsafe { *regs.add(reg as usize) = Value::heap(cell).bits() };
                0
            }
            Instr::MakeCellTdz { reg } if (reg as usize) < reg_count => {
                vm.maybe_gc();
                let cell = vm.heap.alloc(HeapObj::Cell(Value::UNINITIALIZED));
                unsafe { *regs.add(reg as usize) = Value::heap(cell).bits() };
                0
            }
            Instr::MarkCellConst { reg } if (reg as usize) < reg_count => {
                let v = Value::from_bits(unsafe { *regs.add(reg as usize) });
                // The cell was just created by a MakeCell* this same body
                // executed; anything else is malformed and declines.
                if !v.is_heap()
                    || v.heap_index() as usize >= vm.heap.len()
                    || !matches!(vm.heap.get(v.heap_index()), HeapObj::Cell(_))
                {
                    return crate::codegen::SELF_CALL_DEOPT;
                }
                vm.const_cells.insert(v.heap_index());
                0
            }
            _ => crate::codegen::SELF_CALL_DEOPT,
        }
    })) {
        Ok(bits) => bits,
        // A panic past the allocation/side-table commit must never replay.
        Err(_) => std::process::abort(),
    }
}

/// `MakeClosure { dst, func_id }` and `MakeArrow { dst, func_id, this_reg }`.
/// Returns the fresh callable's Value bits; the emitted site stores them to
/// `dst`. Everything observable — capture cells, lexical `this`, inherited
/// `[[HomeObject]]`, captured `new.target`, EvalScope stamp, realm tag —
/// replicates the interpreter arms exactly, sourced from the verified
/// activation instead of `frames.last()`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_make_closure(
    vm: *mut core::ffi::c_void,
    regs: *mut u64,
    packed_fip: u64,
    reg_count: u64,
) -> u64 {
    if vm.is_null() || regs.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let reg_count = reg_count as usize;
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let Some((caller, instr)) = site_instr(vm, packed_fip) else {
            return crate::codegen::SELF_CALL_DEOPT;
        };
        let Some(site) = make_site_preflight(vm, regs, reg_count, caller) else {
            return crate::codegen::SELF_CALL_DEOPT;
        };
        let (child, this_reg) = match instr {
            Instr::MakeClosure { func_id, .. } => (func_id as usize, None),
            Instr::MakeArrow {
                func_id, this_reg, ..
            } => (func_id as usize, Some(this_reg)),
            _ => return crate::codegen::SELF_CALL_DEOPT,
        };
        let func_count = vm.main_func_count.saturating_add(vm.eval_funcs.len());
        if child >= func_count || this_reg.is_some_and(|r| r as usize >= reg_count) {
            return crate::codegen::SELF_CALL_DEOPT;
        }

        // Safe point BEFORE resolving live state: cells sit in window slots
        // and the activation's closure/callee are explicit roots, so every
        // resolved index below stays valid (the collector is non-moving), and
        // no allocation happens between resolution and commit.
        vm.maybe_gc();
        let Some(cells) = resolve_upvalues(vm, regs, reg_count, site, child) else {
            return crate::codegen::SELF_CALL_DEOPT;
        };
        let this_val = match this_reg {
            Some(r) => Value::from_bits(unsafe { *regs.add(r as usize) }),
            None => Value::UNDEFINED,
        };

        // Lexical inheritance reads, all before the allocation commit.
        let inherited_home = if this_reg.is_some() {
            vm.closure_home.get(&site.active).copied()
        } else {
            None
        };
        let new_target = if this_reg.is_some() {
            let frame_nt = if site.frame_backed {
                vm.frames
                    .last()
                    .map(|f| f.new_target)
                    .unwrap_or(Value::UNDEFINED)
            } else {
                // A frame-free cross-call is always an ordinary call.
                Value::UNDEFINED
            };
            if frame_nt != Value::UNDEFINED {
                frame_nt
            } else {
                vm.closure_new_target
                    .get(&site.active)
                    .copied()
                    .unwrap_or(Value::UNDEFINED)
            }
        } else {
            Value::UNDEFINED
        };
        let eval_scope = vm.closure_eval_scope.get(&site.active).copied();

        let idx = vm.heap.alloc(HeapObj::Closure {
            func: child as u32,
            upvalues: cells,
            this_val,
        });
        if let Some(home) = inherited_home {
            vm.record_closure_home(idx, home);
        }
        if new_target != Value::UNDEFINED {
            vm.record_closure_new_target(idx, new_target);
        }
        if let Some(scope) = eval_scope {
            vm.closure_eval_scope.insert(idx, scope);
        }
        vm.realm_tag_new(idx);
        Value::heap(idx).bits()
    })) {
        Ok(bits) => bits,
        Err(_) => std::process::abort(),
    }
}

/// `CellSetChecked { cell, src }` — the TDZ-checked sibling of the region
/// path's `jit_cell_set`: a write to a lexical cell still in its TDZ declines
/// so the interpreter throws its exact ReferenceError. The nursery barrier
/// lives inside `Heap::cell_set`, exactly like the interpreter arm.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_cell_set_tdz_checked(
    vm: *mut core::ffi::c_void,
    cell_bits: u64,
    val_bits: u64,
) -> u64 {
    if vm.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let cell = Value::from_bits(cell_bits);
        if !cell.is_heap() || cell.heap_index() as usize >= vm.heap.len() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let idx = cell.heap_index();
        if !matches!(vm.heap.get(idx), HeapObj::Cell(_)) || vm.heap.cell_get(idx).is_uninitialized()
        {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        vm.heap.cell_set(idx, Value::from_bits(val_bits));
        0
    })) {
        Ok(bits) => bits,
        // The barrier/write may have committed; never replay past it.
        Err(_) => std::process::abort(),
    }
}

/// `ArrayCtor { dst, arg_base, argc }` — the interpreter arm's dense subset:
/// `Array(n)` with a valid dense-capped integer length becomes `n` holes, and
/// any other argument list becomes its elements. Invalid lengths (interpreter
/// RangeError) and past-cap sparse lengths decline as pure prefixes.
/// `packed = (reg_count << 32) | (arg_base << 16) | argc`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_array_ctor(
    vm: *mut core::ffi::c_void,
    regs: *const u64,
    packed: u64,
) -> u64 {
    if vm.is_null() || regs.is_null() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let reg_count = (packed >> 32) as u32 as usize;
    let arg_base = (packed >> 16) as u16 as usize;
    let argc = packed as u16 as usize;
    if arg_base
        .checked_add(argc.max(1))
        .is_none_or(|end| end > reg_count)
    {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        if !crate::vm::regs_window_valid(vm, regs, reg_count) {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let read = |offset: usize| Value::from_bits(unsafe { *regs.add(arg_base + offset) });
        let arr = if argc == 1 && read(0).is_number() {
            let n = read(0).as_f64();
            if n < 0.0
                || n.fract() != 0.0
                || n > u32::MAX as f64
                || n as usize > crate::vm::MAX_DENSE_ARRAY_LEN
            {
                // RangeError / sparse virtual-length: interpreter semantics.
                return crate::codegen::SELF_CALL_DEOPT;
            }
            vec![Value::HOLE; n as usize]
        } else {
            (0..argc).map(read).collect()
        };
        Value::heap(vm.heap.alloc(HeapObj::Array(arr))).bits()
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
            crate::compile::compile_main_program(&ast, source).expect("source compiles"),
        ));
        let mut vm = Vm::new(program);
        vm.run().expect("program runs");
        vm
    }

    fn find_site(vm: &Vm<'_>, want: fn(&Instr) -> bool) -> (u32, usize) {
        let count = vm.main_func_count + vm.eval_funcs.len();
        for fid in 0..count {
            if let Some(ip) = vm.func(fid).code.iter().position(want) {
                return (fid as u32, ip);
            }
        }
        panic!("creation site not found");
    }

    /// Malformed direct calls decline before any observable effect: a bad
    /// window, a bad ip, a mismatched activation, or a non-cell register.
    #[test]
    fn malformed_helper_inputs_decline_without_effects() {
        let mut vm = vm("function outer(v) { let c = v; return function inner() { return c; }; } var keep = outer(7);");
        let (fid, ip) = find_site(&vm, |i| matches!(i, Instr::MakeClosure { .. }));
        let packed = ((fid as u64) << 32) | ip as u64;
        let heap_before = vm.heap.len();
        let regs = vm.regs.as_mut_ptr() as *mut u64;
        let n = vm.regs.len() as u64;
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;

        // No live Tier-C activation: every helper declines purely.
        assert_eq!(
            jit_make_closure(vm_ptr, regs, packed, n),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(
            jit_make_cell(vm_ptr, regs, packed, n),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(
            jit_make_closure(vm_ptr, core::ptr::null_mut(), packed, n),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(
            jit_make_closure(vm_ptr, regs, u64::MAX, n),
            crate::codegen::SELF_CALL_DEOPT
        );
        assert_eq!(vm.heap.len(), heap_before, "declines must not allocate");
    }

    /// The helper-created closure is indistinguishable from the interpreter's:
    /// shared mutable capture cells, lexical `this`, and realm/eval defaults.
    #[test]
    fn helper_closure_matches_interpreter_capture_semantics() {
        let mut vm = vm(
            "function outer(v) { let c = v; return function inner() { return c; }; } var keep = outer(7);",
        );
        let (fid, ip) = find_site(&vm, |i| matches!(i, Instr::MakeClosure { .. }));
        let Instr::MakeClosure { func_id: child, .. } = vm.func(fid as usize).code[ip] else {
            unreachable!()
        };

        // Stage a window shaped like outer's frame: the captured local's cell
        // in the register MakeCell boxed it into.
        let Some(&UpvalSource::ParentLocal(cell_reg)) = vm.func(child as usize).upvalues.first()
        else {
            panic!("inner must capture outer's local");
        };
        let outer_regs = vm.func(fid as usize).reg_count.max(1) as usize;
        vm.regs = vec![Value::UNDEFINED; outer_regs];
        let cell = vm.heap.alloc(HeapObj::Cell(Value::int(41)));
        vm.regs[cell_reg as usize] = Value::heap(cell);
        let callee = vm.heap.alloc(HeapObj::Func(fid));
        vm.jit_tierc_activation = TiercActivationState {
            active: true,
            frame_free: true,
            closure: NO_CLOSURE,
            callee,
        };

        let packed = ((fid as u64) << 32) | ip as u64;
        let regs = vm.regs.as_mut_ptr() as *mut u64;
        let n = vm.regs.len() as u64;
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;
        let bits = jit_make_closure(vm_ptr, regs, packed, n);
        assert_ne!(bits, crate::codegen::SELF_CALL_DEOPT);
        let made = Value::from_bits(bits);
        match vm.heap.get(made.heap_index()) {
            HeapObj::Closure {
                func,
                upvalues,
                this_val,
            } => {
                assert_eq!(*func, child);
                assert_eq!(upvalues.as_slice(), &[cell]);
                assert_eq!(*this_val, Value::UNDEFINED);
            }
            other => panic!("expected a closure, got {other:?}"),
        }

        // Cell creation: the window slot is replaced by a fresh live cell —
        // value-carrying for `MakeCell`, TDZ-uninitialized for `MakeCellTdz`
        // (whichever form this compiler build uses for the captured local).
        let (cell_fid, cell_ip) = find_site(&vm, |i| {
            matches!(i, Instr::MakeCell { .. } | Instr::MakeCellTdz { .. })
        });
        let (reg, tdz) = match vm.func(cell_fid as usize).code[cell_ip] {
            Instr::MakeCell { reg } => (reg, false),
            Instr::MakeCellTdz { reg } => (reg, true),
            _ => unreachable!(),
        };
        let cell_regs = vm.func(cell_fid as usize).reg_count.max(1) as usize;
        vm.regs = vec![Value::UNDEFINED; cell_regs];
        vm.regs[reg as usize] = Value::int(9);
        let cell_callee = vm.heap.alloc(HeapObj::Func(cell_fid));
        vm.jit_tierc_activation = TiercActivationState {
            active: true,
            frame_free: true,
            closure: NO_CLOSURE,
            callee: cell_callee,
        };
        let packed_cell = ((cell_fid as u64) << 32) | cell_ip as u64;
        let regs = vm.regs.as_mut_ptr() as *mut u64;
        let n = vm.regs.len() as u64;
        let vm_ptr = &mut vm as *mut Vm as *mut core::ffi::c_void;
        assert_eq!(jit_make_cell(vm_ptr, regs, packed_cell, n), 0);
        let boxed = vm.regs[reg as usize];
        assert!(boxed.is_heap());
        let expected = if tdz {
            Value::UNINITIALIZED
        } else {
            Value::int(9)
        };
        match vm.heap.get(boxed.heap_index()) {
            HeapObj::Cell(v) => assert_eq!(*v, expected),
            other => panic!("expected a cell, got {other:?}"),
        }
    }
}
