// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// The native self-recursive-call implementation behind the `jit_self_call`
    /// FFI trampoline. Runs `self`-recursion natively on a fresh register window
    /// appended to `self.regs`. Returns result Value bits, or
    /// `codegen::SELF_CALL_DEOPT` to make the native caller bail to the interp.
    ///
    /// Register-stability invariant: `self.regs` has reserved capacity for the
    /// whole recursion (`reserve_jit_regs`), so appending the callee window
    /// NEVER reallocates — the native CALLER's window pointer (`rbx`) therefore
    /// stays valid across this call. We `truncate` back to the caller's length
    /// before returning so the register file is exactly as the caller left it.
    ///
    /// NOTE: superseded by the inline native→native fast path + `jit_self_call_at`
    /// (the codegen now calls its own entry directly, no per-call Rust). Retained
    /// for reference / potential reuse; not on any hot path.
    #[allow(dead_code)]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_self_call_impl(&mut self, func_id: u32, args: *const u64, argc: usize) -> u64 {
        // An arrow needs its lexically-captured `this` bound at reg 0; this fast
        // path sets reg 0 = UNDEFINED, so deopt arrows to the interpreter (which
        // rebinds correctly). Recursive arrows that read `this` are rare.
        if self.func(func_id as usize).lexical_this {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        // Depth guard: deopt (not crash) past the native recursion budget; the
        // interpreter path then enforces MAX_FRAMES / throws RangeError.
        if self.jit_recurse_depth >= JIT_SELF_RECURSE_MAX {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        // Native entry via the one-entry self-call cache (skips the HashMap
        // lookup on the hot recursive path — it always targets the same func_id).
        let entry = match self.jit.self_call_entry(func_id) {
            Some(e) => e,
            None => return crate::codegen::SELF_CALL_DEOPT,
        };
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;

        // Fresh window appended to regs. Reserved capacity guarantees no realloc.
        let new_base = self.regs.len();
        let needed = new_base + reg_count;
        if needed > self.regs.capacity() {
            // Out of reserved headroom — deopt rather than risk a realloc that
            // would invalidate the caller's live `rbx`.
            return crate::codegen::SELF_CALL_DEOPT;
        }
        if needed > self.regs_hw {
            // New ground: zero-fill the freshly exposed slots and advance the mark.
            self.regs.resize(needed, Value::UNDEFINED);
            self.regs_hw = needed;
        } else {
            // Window lies within already-initialized memory (a previous recursion
            // reached at least this deep). Slots hold valid Value bits (stale, but
            // the compiled code writes before it reads), so skip the zero-fill —
            // this is the hot path for all but the deepest recursive call.
            // SAFETY: needed ≤ regs_hw ≤ a prior len ≤ capacity; [0..regs_hw] was
            // initialized by an earlier resize and the buffer is pinned, so these
            // slots are live, valid `Value`s.
            unsafe {
                self.regs.set_len(needed);
            }
        }
        // reg 0 = `this` (undefined for a plain self-call); params at 1..
        self.regs[new_base] = Value::UNDEFINED;
        let n = argc.min(params);
        for i in 0..n {
            // SAFETY: args points to `argc` valid Value bits (the caller's reg
            // window); n ≤ argc.
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }

        self.jit_recurse_depth += 1;
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(new_base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // Call the cached native entry directly (same win64 ABI as JitFn::run).
        // SAFETY: `entry` is this function's compiled win64 code (stable across
        // HashMap rehashes); the window has `reg_count` valid slots; vm is valid.
        let (bits, bail) = {
            let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Jit);
            unsafe {
                let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
                    core::mem::transmute(entry);
                let mut bail: u32 = crate::codegen::NO_BAIL;
                let r = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
                (r, bail)
            }
        };

        let result_bits = if bail == crate::codegen::NO_BAIL {
            bits
        } else {
            // The native callee bailed mid-body: finish this activation on the
            // interpreter over the SAME window via a transient frame. The frame
            // base is `new_base` into self.regs (stable — reserved capacity).
            self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX, arg_win: u32::MAX, argc: 0, is_eval: false,
                func: func_id,
                base: new_base,
                ip: bail as usize,
                ret_dst: 0,
                closure: NO_CLOSURE,
                handlers: Vec::new(),
                new_target: Value::UNDEFINED,
                callee: Value::UNDEFINED,
            });
            let stop = self.frames.len() - 1;
            match self.run_loop(stop) {
                Ok(v) => v.bits(),
                // A throw inside the recursion: there is no JS-level way to
                // surface it through the native ABI here, so deopt the whole
                // self-call. pending_throw stays set; the interpreter caller
                // (the original top-level run_loop) re-raises it. We restore
                // regs and return the sentinel.
                Err(_) => {
                    self.jit_recurse_depth -= 1;
                    self.regs.truncate(new_base);
                    return crate::codegen::SELF_CALL_DEOPT;
                }
            }
        };

        self.jit_recurse_depth -= 1;
        self.regs.truncate(new_base);
        result_bits
    }

    /// Slow/finish path for the JIT's inline native→native self-call. Called
    /// when the inline fast path can't complete a recursive call purely natively:
    /// either the native depth limit was hit, or the callee bailed mid-body. The
    /// caller passes its window base EXPLICITLY (`caller_base_ptr`, the native
    /// `rbx`) because the fast path tracks windows by raw pointer, not
    /// `self.regs.len()`. Runs the activation on the interpreter over a transient
    /// frame at the callee window, holding `jit_recurse_depth` ELEVATED for the
    /// duration so the dispatch JIT-entry gate (`== 0`) stays closed and the
    /// recursion can't re-enter native and livelock — frames then accumulate
    /// monotonically to `MAX_FRAMES` → catchable RangeError. Returns the result
    /// bits, or `SELF_CALL_DEOPT` if the activation threw (the throw is left in
    /// `pending_throw`; the native chain unwinds and the top-level interpreter
    /// re-raises it).
    ///
    /// # Safety
    /// `caller_base_ptr` is the caller's window base within `self.regs`; `args`
    /// points to `argc` valid `Value` bits.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_self_call_at_impl(
        &mut self,
        func_id: u32,
        caller_base_ptr: *const u64,
        args: *const u64,
        argc: usize,
    ) -> u64 {
        // Arrows need their lexical `this` at reg 0 (this path sets UNDEFINED) —
        // deopt them to the interpreter, which rebinds correctly.
        if self.func(func_id as usize).lexical_this {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;

        // Caller window base as a slot index (the fast path placed it by raw
        // pointer); the callee window sits contiguously above it.
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' (non-reallocating) buffer.
        let caller_base =
            unsafe { (caller_base_ptr).offset_from(regs_base) } as usize;
        let new_base = caller_base + reg_count;
        let needed = new_base + reg_count;
        if self.regs_would_overflow(needed) {
            // Out of reserved register headroom (very deep): treat as stack
            // overflow — throw so the interpreter surfaces a catchable RangeError.
            let e =
                self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            self.pending_throw = Some(e);
            return crate::codegen::SELF_CALL_DEOPT;
        }

        // RESYNC self.regs.len() to span the callee window so the transient
        // interpreter frame + MAX_FRAMES accounting are consistent. Save the
        // entry length and restore it on the way out (the native caller doesn't
        // use `len`, but the eventual return to the dispatch loop expects it
        // unchanged).
        let saved_len = self.regs.len();
        // CRITICAL: grow `len` with `set_len`, NOT `resize`. The native fast path
        // advanced the register windows by raw pointer WITHOUT touching
        // `self.regs.len()`, so on entry here `len` (≈ the warmup top) is far below
        // the live native windows, which occupy slots up to `new_base`. A
        // `resize(needed, UNDEFINED)` would ZERO-FILL `[len, needed)` — overwriting
        // every parked native frame's registers with `undefined` and corrupting the
        // recursion (this was the bug that capped JIT recursion below the
        // interpreter). The native windows hold valid `Value`s already (each native
        // frame defs its registers before reading — the same def-before-use
        // invariant the leaf JIT relies on), and the buffer is pinned to capacity
        // by `reserve_jit_regs`, so simply exposing them via `set_len` is correct.
        // Bounds: `needed ≤ capacity` (guarded above by `regs_would_overflow`).
        // SAFETY: `needed ≤ capacity`; slots `[0, needed)` are live `Value`s —
        // `[0, len)` from the interpreter, `[len, new_base+reg_count)` written by
        // the native frames whose windows we're spanning.
        unsafe { self.regs.set_len(needed); }
        if needed > self.regs_hw {
            self.regs_hw = needed;
        }
        self.regs[new_base] = Value::UNDEFINED;
        let n = argc.min(params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }

        // Run this activation on the interpreter via a transient frame. Depth is
        // held ELEVATED across the whole run (we only restore it after), so any
        // self-call inside stays interpreted (the dispatch gate sees depth != 0)
        // and the recursion can't re-enter native → no livelock; frames grow to
        // MAX_FRAMES → RangeError on runaway.
        self.jit_recurse_depth += 1;
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX, arg_win: u32::MAX, argc: 0, is_eval: false,
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure: NO_CLOSURE,
            handlers: Vec::new(),
            new_target: Value::UNDEFINED,
            callee: Value::UNDEFINED,
        });
        let stop = self.frames.len() - 1;
        let r = self.run_loop(stop);
        self.jit_recurse_depth -= 1;
        // SAFETY: restore the entry length (allocation unchanged, slots valid).
        unsafe { self.regs.set_len(saved_len); }
        match r {
            Ok(v) => v.bits(),
            // Threw (e.g. RangeError): leave it in pending_throw and signal the
            // native caller to unwind; the top-level interpreter re-raises it.
            Err(_) => crate::codegen::SELF_CALL_DEOPT,
        }
    }

    /// The Tier C CROSS-CALL fast path (B83), behind the `jit_cross_call` FFI
    /// helper: a compiled body's `Call` site dispatches a Tier-C-compiled plain
    /// callee native→native. Mutual recursion (`pExpr → pTerm → pFactor →
    /// pExpr`) then runs as native calls through this thin helper instead of
    /// paying `ic_call` + `setup_call` + `frames.push` + a nested `run_loop`
    /// per hop (~64ns/call measured; the whole `parse-large-js` parse phase).
    ///
    /// Resolution: the callee's live Value (read from the caller's register by
    /// the emitted site) is resolved HERE, each call, straight off the heap
    /// object — `Func`/`Closure` → func id. Rebinding the global mid-run makes
    /// the register hold a different Value, which resolves to the new function
    /// (or deopts) — the same observable behaviour as the interpreter's
    /// identity-guarded IC.
    ///
    /// Eligibility (everything else returns `SELF_CALL_DEOPT`, and the emitted
    /// site falls through to the unchanged `call_ic` helper, a pure prefix):
    /// * the resolved function has a live cross entry (Tier-C compiled — which
    ///   already excludes generators/async and rest/`arguments` bodies, and
    ///   never bakes a Tier A self-binding assumption);
    /// * it is not an arrow (`lexical_this` — reg 0 must be the captured
    ///   `this`, which only `setup_call` rebinds);
    /// * its direct global routes still validate (the `try_run_jit` entry
    ///   check, which this path would otherwise skip);
    /// * the caller's window is the TOP of the live register file (it always
    ///   is — `setup_call` and this helper both leave `len` at the window end
    ///   — but a mismatch deopts rather than trusts).
    ///
    /// GC: the callee window is exposed under `self.regs.len()` BEFORE the
    /// callee runs (zero-filling resize on new ground; `set_len` over the
    /// already-initialized high-water region on the W7 fast fill, with the
    /// may-read-before-write registers explicitly re-zeroed — see the fill
    /// comment in the body), so `len` covers every live native window and the
    /// root set stays complete even when the callee re-enters the interpreter
    /// through its own helpers. Every exposed slot always holds a valid
    /// `Value` (stale at worst — retention-conservative, never forged bits).
    /// `self.regs` never reallocates (`regs_would_overflow` guard + pinned
    /// capacity), so the caller's window pointer stays valid.
    ///
    /// Depth: shares `jit_call_depth` / `JIT_REGION_CALL_MAX` with the region
    /// call helpers (each level is a real Rust/native stack frame); past the
    /// cap the call deopts to the interpreter's flat frames, which enforce
    /// MAX_FRAMES → catchable RangeError.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_cross_call_impl(
        &mut self,
        caller_base_ptr: *const u64,
        args: *const u64,
        packed: u64,
        callee_bits: u64,
    ) -> u64 {
        use crate::codegen::{CALL_THREW, SELF_CALL_DEOPT};
        if self.jit_call_depth >= JIT_REGION_CALL_MAX {
            self.osr_deopt_exempt = true;
            return SELF_CALL_DEOPT;
        }
        let argc = (packed & 0xFFFF) as usize;
        let caller_regs = ((packed >> 16) & 0xFFFF) as usize;
        let cv = Value::from_bits(callee_bits);
        if !cv.is_heap() {
            return SELF_CALL_DEOPT;
        }
        let (fid, closure) = match self.heap.get(cv.heap_index()) {
            HeapObj::Func(id) => (*id, NO_CLOSURE),
            HeapObj::Closure { func, .. } => (*func, cv.heap_index()),
            _ => return SELF_CALL_DEOPT,
        };
        let (entry, uninit_mask) = match self.jit.cross_entry(fid) {
            Some(e) => e,
            None => return SELF_CALL_DEOPT,
        };
        // The entry checks `try_run_jit` performs and a direct call would skip:
        // direct global routes can be invalidated by `delete` / defineProperty
        // on globalThis (Tier C never records a self-binding, so that check is
        // structurally unnecessary — asserted at install).
        if self.global_route_epoch != 0 && !self.jit_globals_still_routable(fid) {
            return SELF_CALL_DEOPT;
        }
        // GC safe point — the frame-transition parity point. The route this
        // path replaces ran `maybe_gc` in `dispatch_body` on EVERY call's frame
        // push; without this, a hot loop whose only safe points were its call
        // transitions defers collection for the loop's whole lifetime and the
        // heap balloons (regex-log-scan's corpus-gen: 74 collections → 1, avg
        // live slots 199k → 7.4M, and every subsequent versions/IC access pays
        // the locality bill — measured +9% on the row, all charged to jit-mem).
        // Safe here for the same reason it is safe in `dispatch_body`: every
        // live Value sits in regs[0..len]/globals/frames/side-tables (the
        // callee Value is still in the caller's register; `args` point into the
        // caller's window), no native helper is mid-flight, and the collector
        // is non-moving. The calling region refetches r13/r14 after this
        // helper, so a versions-array reallocation cannot dangle its pins.
        // Cost when no collection is due: two field compares.
        self.maybe_gc();
        let proto = self.func(fid as usize);
        if proto.lexical_this {
            return SELF_CALL_DEOPT; // arrow: needs its captured `this` at reg 0
        }
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;
        // OrdinaryCallBindThis for a plain `f()` (`this` = undefined): a strict
        // callee binds undefined; a sloppy one binds its realm's global object.
        let this_v = if proto.is_strict {
            Value::UNDEFINED
        } else if self.global_this != 0 {
            Value::heap(self.callee_this_global(cv))
        } else {
            return SELF_CALL_DEOPT;
        };
        // The callee window sits contiguously above the caller's, which must be
        // the top of the live register file (see the doc note).
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' pinned buffer.
        let caller_base = unsafe { caller_base_ptr.offset_from(regs_base) } as usize;
        let new_base = caller_base + caller_regs;
        if new_base != self.regs.len() {
            return SELF_CALL_DEOPT;
        }
        let needed = new_base + reg_count;
        if self.regs_would_overflow(needed) {
            let e =
                self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            self.pending_throw = Some(e);
            self.osr_deopt_exempt = true;
            return CALL_THREW;
        }
        // Window init. The interpreter zero-fills the whole window (a body may
        // legally read an unwritten local as `undefined`); re-zeroing all
        // `reg_count` slots per call measured ~2.7ns of the ~19ns cross-call
        // residual (W7 census). When the window lies UNDER the high-water mark
        // (`regs_hw` — every slot below it was zero-initialized once and has
        // only ever been overwritten with live `Value`s since), expose it with
        // `set_len` and zero ONLY what the callee can observe as uninitialized:
        //   * `uninit_mask` — the compile-time may-read-before-write register
        //     set (`cross_uninit_mask`, a must-def dataflow over the CLOSED
        //     Tier-C op set; `u64::MAX` = analysis declined → full fill). Every
        //     other register is proven def-before-use on every path from entry,
        //     and Tier C stores every def to the window (memory tier), so an
        //     interpreter resume after a mid-body bail sees the same defs.
        //   * missing arguments `[1+n, 1+params)` — the dataflow assumes params
        //     are entry-defined; short calls must make that true.
        // GC-COMPLETENESS (the B117 argument, unchanged): `len` spans the
        // window BEFORE the callee runs, and every exposed slot holds a valid
        // (possibly stale) `Value` — the root set stays complete, merely
        // over-approximate, exactly like the `jit_self_call_impl` precedent.
        // The safe point above ran at `len == new_base`, so no stale slot is
        // ever scanned before this call's own `this`/args land.
        let n = argc.min(params);
        if uninit_mask != u64::MAX && needed <= self.regs_hw {
            // SAFETY: needed ≤ regs_hw ≤ capacity; [0..regs_hw] was initialized
            // by an earlier resize and the buffer is pinned, so these slots are
            // live, valid `Value`s.
            unsafe { self.regs.set_len(needed) };
            let mut m = uninit_mask;
            while m != 0 {
                let r = m.trailing_zeros() as usize;
                self.regs[new_base + r] = Value::UNDEFINED;
                m &= m - 1;
            }
            for r in (1 + n)..(1 + params) {
                self.regs[new_base + r] = Value::UNDEFINED;
            }
            crate::vm::helpers_misc::crossstats::fill_fast();
        } else {
            // Full zero-fill: new ground past the high-water mark, or the
            // analysis declined (`ZIPP_NO_CROSSCALL2` forces this arm — the
            // mask is pinned to `u64::MAX` at install time).
            self.regs.resize(needed, Value::UNDEFINED);
            if needed > self.regs_hw {
                self.regs_hw = needed;
            }
            crate::vm::helpers_misc::crossstats::fill_full();
        }
        self.regs[new_base] = this_v;
        for i in 0..n {
            // SAFETY: args points to `argc` valid Value bits (the caller's
            // staged contiguous arg registers); n ≤ argc.
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }
        self.jit_call_depth += 1;
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(new_base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `entry` is `fid`'s Tier-C win64 code (mmap'd, never moves);
        // the window has `reg_count` valid slots; vm is valid.
        let (bits, bail) = {
            let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Jit);
            unsafe {
                let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
                    core::mem::transmute(entry);
                let mut b: u32 = crate::codegen::NO_BAIL;
                let r = f(regs_ptr, &mut b as *mut u32, vm_ptr);
                (r, b)
            }
        };
        let out = if bail == crate::codegen::NO_BAIL {
            self.regs.truncate(new_base);
            bits
        } else if self.pending_throw.is_some() {
            // The callee's own (deeper) call threw and its native code
            // signalled unwind via a bail with the throw pending (the
            // `try_run_jit` (b) case): unwind, never resume.
            self.regs.truncate(new_base);
            self.osr_deopt_exempt = true;
            CALL_THREW
        } else {
            // Guard bail mid-body: finish this activation on the interpreter
            // over the SAME window via a transient frame (regs stay as the
            // native code left them; ip = the recorded resume point).
            if self.frames.len() >= MAX_FRAMES {
                let e = self
                    .alloc_error_from_message("RangeError: Maximum call stack size exceeded");
                self.pending_throw = Some(e);
                self.regs.truncate(new_base);
                self.jit_call_depth -= 1;
                self.osr_deopt_exempt = true;
                return CALL_THREW;
            }
            let arg_win = unsafe { args.offset_from(regs_base) } as u32;
            let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
            self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX, arg_win, argc: argc as u16, is_eval: false,
                func: fid,
                base: new_base,
                ip: bail as usize,
                ret_dst: 0,
                closure,
                handlers: Vec::new(),
                new_target,
                callee: cv,
            });
            let stop = self.frames.len() - 1;
            match self.run_loop(stop) {
                Ok(v) => {
                    self.regs.truncate(new_base);
                    v.bits()
                }
                Err(_) => {
                    // pending_throw is set; the native chain unwinds and the
                    // enclosing interpreter dispatches it to a handler.
                    self.osr_deopt_exempt = true;
                    CALL_THREW
                }
            }
        };
        self.jit_call_depth -= 1;
        out
    }

    /// The implementation behind the region call helpers `jit_call_method_ic` /
    /// `jit_call_ic` (`is_method` selects which). A compiled OSR region reached
    /// a `CallMethod`/`Call` op: consult the SAME per-site inline cache the
    /// interpreter uses, push the resolved plain user function with FULL
    /// `setup_call` semantics (this-binding, rest, arguments object,
    /// MAX_FRAMES), and run it to completion via `run_loop`. Three-state
    /// result (see the helper docs): result bits / `SELF_CALL_DEOPT` /
    /// `CALL_THREW`.
    ///
    /// Reentrancy contract (what the calling region may rely on):
    /// * `self.regs` never reallocates (pinned by `reserve_jit_regs`; every
    ///   growth site checks `regs_would_overflow`) — the region's window base
    ///   (rbx) stays valid. The callee windows are appended above the region's
    ///   frame and truncated back before this returns.
    /// * `self.globals` never reallocates — r12 stays valid.
    /// * The heap's versions array and the JIT IC table MAY move (the callee
    ///   can allocate, and can trigger a nested region compile — nested
    ///   execution runs at `jit_recurse_depth == 0`, so the JIT gates are
    ///   open) — the region RE-FETCHES r13/r14 after this helper returns.
    /// * A nested deopt can EVICT the (still-running) calling region; evicted
    ///   regions are parked in `Jit::retired`, never dropped mid-run.
    /// * No `Value` is held in native registers across this call (the memory
    ///   path stores every result to the reg file before the next op), and
    ///   this function holds no `Vec<Value>` across the callee run — all live
    ///   values sit in regs/frames/heap, the GC root set.
    ///
    /// Depth: each level nests `run_loop` on the Rust stack, so recursion
    /// through region call sites is capped at `JIT_REGION_CALL_MAX`; past it
    /// the call deopts to the interpreter's flat frames (and sets
    /// `osr_deopt_exempt` so the region isn't punished for legal recursion).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_region_call_impl(
        &mut self,
        caller_base_ptr: *const u64,
        packed_fip: u64,
        packed_args: u64,
        argc: u16,
        is_method: bool,
    ) -> u64 {
        use crate::codegen::{CALL_THREW, SELF_CALL_DEOPT};
        if self.jit_call_depth >= JIT_REGION_CALL_MAX {
            // Legal-but-deep recursion: not a region-quality signal — don't
            // count it toward eviction.
            self.osr_deopt_exempt = true;
            return SELF_CALL_DEOPT;
        }
        let func_id = (packed_fip >> 32) as u32;
        let ip = (packed_fip as u32) as usize;
        let arg_base = (packed_args & 0xFFFF) as u16;
        // Caller window base as a slot index (the region tracks it by raw
        // pointer; the buffer is pinned, so the offset is stable).
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' pinned buffer.
        let base = unsafe { caller_base_ptr.offset_from(regs_base) } as usize;

        // Resolve through the interpreter's per-site IC — IDENTICAL resolution
        // order to the interpreter (which consults the IC first; everything the
        // IC won't claim, incl. '#private' names, builtins, natives, accessors,
        // generators/async, exotic receivers, deopts to the interpreter).
        let (fid, closure, this_v, callee_v) = if is_method {
            let obj = ((packed_args >> 16) & 0xFFFF) as u16;
            let name = (packed_args >> 32) as u32;
            let recv = self.get(base, obj);
            // `func()` borrows &'p (program lifetime) — outlives `&mut self`.
            let key: &str = &self.func(func_id as usize).string_constants[name as usize];
            match self.ic_call_method(func_id, ip, recv, key) {
                // An ARROW binds `this` LEXICALLY: reg 0 is its captured
                // `this_val`, and the receiver is ignored entirely. This arm
                // hands `this_v = recv` to `try_method_inline` (which evaluates
                // the body off-frame against exactly that value), so
                // `function Maker(){ this.f=111; this.o={f:3, m:()=>this.f} }`
                // made a hot `o.m()` return 3 where the interpreter and node
                // return 111 — silent, at default thresholds, and an ordinary
                // shape. Deopt so `setup_call` does the rebinding, which is what
                // the two sibling fast paths above already do for the same
                // reason (`jit_self_call_impl`, `jit_fast_call_impl`).
                Some((fid, _, _)) if self.func(fid as usize).lexical_this => {
                    return crate::codegen::SELF_CALL_DEOPT;
                }
                Some((fid, closure, callee)) => (fid, closure, recv, callee),
                None => {
                    if jit_call_log() {
                        eprintln!("[call] METHOD MISS fn{func_id}@{ip} key={key}");
                    }
                    // B82: a `f.call(…)`/`f.apply(…)` site whose receiver is a
                    // plain user function and whose `call`/`apply` resolves to
                    // the PRISTINE `%Function.prototype%` native — splice the
                    // TARGET call off-frame (the this/args shuffle done here,
                    // the body run by the method inliner's evaluator), skipping
                    // `call_value`'s frames.push + nested run_loop. Every guard
                    // is re-checked per call, so a monkey-patched `.call`, an
                    // own `f.call` shadow, a swapped [[Prototype]], or a bound/
                    // exotic target declines to the unchanged fallback below.
                    if matches!(key, "call" | "apply") {
                        if let Some(bits) = self.try_fn_call_apply_inline(
                            recv,
                            key == "apply",
                            base,
                            arg_base,
                            argc,
                        ) {
                            return bits;
                        }
                    }
                    // Builtin fallback: the EXACT paths the interpreter runs
                    // next for this op (`try_builtin_method`, then a
                    // ctor-object native like `Math.floor`) — run to
                    // completion, never deopting after a side effect.
                    return self.jit_method_builtin_fallback(recv, key, base, arg_base, argc);
                }
            }
        } else {
            let callee_reg = ((packed_args >> 16) & 0xFFFF) as u16;
            let cv = self.get(base, callee_reg);
            match self.ic_call(func_id, ip, cv) {
                Some((fid, closure)) => (fid, closure, Value::UNDEFINED, cv),
                None => {
                    if jit_call_log() {
                        eprintln!("[call] CALL MISS fn{func_id}@{ip}");
                    }
                    // A plain NATIVE callee (parseInt, …): invoke via
                    // call_value with this=undefined, exactly like the
                    // interpreter's Call op. Everything else deopts.
                    if cv.is_heap()
                        && matches!(self.heap.get(cv.heap_index()), HeapObj::Native(_))
                    {
                        let argv: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        return match self.call_value(cv, Value::UNDEFINED, &argv) {
                            Ok(v) => v.bits(),
                            Err(t) => self.jit_thrown_to_sentinel(t),
                        };
                    }
                    return SELF_CALL_DEOPT;
                }
            }
        };

        // MI (method inlining): for a `CallMethod` whose resolved target is a
        // trivial straight-line body over `this` + params (incl. nested
        // `super.m()`), evaluate it OFF-FRAME — no `setup_call`, no `run_loop`,
        // no per-call args Vec. This collapses the dominant class-method call
        // floor (`objs[i&3].area()` over `super.area()*k`). `None` falls through
        // to the full frame call (any non-trivial body / non-numeric operand /
        // non-instance receiver). Only for method calls — `this = recv` is
        // load-bearing; a plain `Call` binds `this = undefined`.
        if is_method {
            if let Some(bits) = self.try_method_inline(fid, this_v, base, arg_base, argc) {
                return bits;
            }
        }
        // Push the callee frame exactly like the interpreter's Call/CallMethod
        // IC-hit path, and run it to completion.
        self.jit_frame_call(fid, closure, this_v, base, arg_base, argc, ip, callee_v)
    }

    /// The interpreter's post-IC `CallMethod` fallbacks, run from a region call
    /// helper TO COMPLETION (a path that has side effects must never deopt
    /// afterwards): `try_builtin_method` (array/string/… builtins, exactly the
    /// interpreter's next step), then a ctor-object data prop holding a NATIVE
    /// (`Math.floor`, `JSON.parse`, … — a side-effect-free resolution) invoked
    /// via `call_value`. Anything else deopts BEFORE doing anything.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_method_builtin_fallback(
        &mut self,
        recv: Value,
        key: &str,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> u64 {
        match self.try_builtin_method(recv, key, base, arg_base, argc) {
            Ok(Some(v)) => return v.bits(),
            Ok(None) => {}
            Err(t) => return self.jit_thrown_to_sentinel(t),
        }
        if recv.is_heap() {
            if let HeapObj::Object(m) = self.heap.get(recv.heap_index()) {
                if m.is_ctor {
                    if let Some(i) = m.pos(key) {
                        if !m.attrs[i].accessor {
                            let f = m.vals[i];
                            if f.is_heap()
                                && matches!(self.heap.get(f.heap_index()), HeapObj::Native(_))
                            {
                                let called = self.with_argv(base, arg_base, argc, |vm, argv| {
                                    vm.call_value(f, recv, argv)
                                });
                                return match called {
                                    Ok(v) => v.bits(),
                                    Err(t) => self.jit_thrown_to_sentinel(t),
                                };
                            }
                        }
                    }
                }
            }
        }
        crate::codegen::SELF_CALL_DEOPT
    }

    /// B82: inline the TARGET of `f.call(…)` / `f.apply(…)` at a region
    /// `CallMethod` site. `recv` is the `f` — the method RECEIVER, which is the
    /// underlying callable. Returns the result bits (or `CALL_THREW`) when the
    /// splice served the call, `None` to fall through to the unchanged generic
    /// fallback (`try_builtin_method` → the `call`/`apply` arm → `call_value`).
    ///
    /// GUARD SET (all re-checked per call — nothing is baked into machine code,
    /// so no invalidation protocol is needed):
    /// * `f` is a plain user function (`ic_plain_fn`: Func/Closure, not
    ///   generator/async — a Bound / native / class receiver declines) and not
    ///   an arrow (`lexical_this` — its captured `this` needs `setup_call`'s
    ///   rebinding).
    /// * No own `f.call`/`f.apply` shadow (`fn_props`), `f`'s [[Prototype]] is
    ///   the main `%Function.prototype%` (`proto_of` override declines), and
    ///   that prototype's own `call`/`apply` slot is still the pristine
    ///   `FN_CALL`/`FN_APPLY` native data property — the same three-step proof
    ///   `jit_has_own_call` uses. A monkey-patched `.call` therefore falls back
    ///   mid-loop with no compiled-code invalidation.
    /// * Realm gate mirroring `dispatch_builtin_method_inner`'s call/apply arm:
    ///   a createRealm-child function resolves these through its OWN realm's
    ///   prototype copies — decline.
    /// * OrdinaryCallBindThis is applied UP FRONT: a SLOPPY target with a
    ///   nullish or primitive `thisArg` needs the global substitution / boxing
    ///   — decline to the frame call rather than replicate it (a strict target
    ///   receives `thisArg` exactly as passed, so `return this` off-frame is
    ///   byte-identical).
    /// * `.apply`'s argArray must be nullish or a PLAIN dense Array (no
    ///   `arr_props` overlay, no virtual `array_js_len` length, no
    ///   `Array.prototype` index pollution, no holes, ≤ 32 elements) — anything
    ///   where CreateListFromArrayLike could observe a getter/trap/coercion
    ///   declines BEFORE any effect.
    /// * The target body itself is admitted by the SAME pass-1 whitelist the
    ///   method inliner uses (`method_body_inlinable`), and pass 2 declines on
    ///   any per-execution surprise before any side effect — so the fallback
    ///   frame call never double-runs anything.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn try_fn_call_apply_inline(
        &mut self,
        recv: Value,
        is_apply: bool,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<u64> {
        if !crate::codegen::call_inline_enabled() {
            return None;
        }
        // Arity subset: `.call` with 0-3 forwarded args, `.apply(this[, arr])`.
        if if is_apply { argc > 2 } else { argc > 4 } {
            return None;
        }
        let (fid, _closure) = self.ic_plain_fn(recv)?;
        if self.func(fid as usize).lexical_this {
            return None;
        }
        if !self.realm_global_objs.is_empty() && self.get_function_realm(recv) != 0 {
            return None;
        }
        let idx = recv.heap_index();
        let name = if is_apply { "apply" } else { "call" };
        if self.fn_props.get(&idx).is_some_and(|m| m.pos(name).is_some()) {
            return None;
        }
        if self.proto_of.get(&idx).is_some_and(|&p| p != Value::heap(self.fn_proto)) {
            return None;
        }
        // Pristine `%Function.prototype%.call`/`.apply`: the own SLOT index is
        // memoized behind the object's version (a key add/remove/descriptor
        // change bumps it); the slot VALUE is re-read every call, because an
        // in-place overwrite (`Function.prototype.call = g`) bumps nothing —
        // the fn-bits re-read precedent.
        let fp_ver = self.heap.version_of(self.fn_proto);
        if self.ci_pristine.0 != fp_ver {
            self.ci_pristine = match self.heap.get(self.fn_proto) {
                HeapObj::Object(m) => (
                    fp_ver,
                    m.pos("call").map_or(u32::MAX, |s| s as u32),
                    m.pos("apply").map_or(u32::MAX, |s| s as u32),
                ),
                _ => (fp_ver, u32::MAX, u32::MAX),
            };
        }
        let slot = if is_apply { self.ci_pristine.2 } else { self.ci_pristine.1 };
        if slot == u32::MAX {
            return None;
        }
        let want = if is_apply { crate::vm::native::FN_APPLY } else { crate::vm::native::FN_CALL };
        let pristine = match self.heap.get(self.fn_proto) {
            HeapObj::Object(m) => {
                let s = slot as usize;
                !m.attrs[s].accessor
                    && m.vals[s].is_heap()
                    && matches!(
                        self.heap.get(m.vals[s].heap_index()),
                        HeapObj::Native(id) if *id == want
                    )
            }
            _ => false,
        };
        if !pristine {
            return None;
        }
        // OrdinaryCallBindThis, applied before the body runs: strict targets
        // take `thisArg` raw; sloppy targets with a nullish/primitive `thisArg`
        // decline (global substitution / boxing stays on the frame call).
        let this_v = if argc >= 1 { self.get(base, arg_base) } else { Value::UNDEFINED };
        if !self.func(fid as usize).is_strict
            && (this_v.is_nullish() || !self.is_object_value(this_v))
        {
            return None;
        }
        if !is_apply {
            // Forwarded args are contiguous at `arg_base + 1` — the method
            // inliner's caller-window entry reads them in place.
            let bits =
                self.try_method_inline(fid, this_v, base, arg_base + 1, argc.saturating_sub(1))?;
            crate::vm::helpers_misc::callstats::inline_hit(false);
            return Some(bits);
        }
        // `.apply`: materialize the forwarded args from the argArray.
        let arr = if argc >= 2 { self.get(base, arg_base + 1) } else { Value::UNDEFINED };
        let mut buf = [Value::UNDEFINED; Self::MI_MAX_REGS];
        let nargs = if arr.is_nullish() {
            0
        } else {
            if !arr.is_heap() || self.array_proto_has_index {
                return None;
            }
            let aidx = arr.heap_index();
            if !self.arr_props.is_empty() && self.arr_props.get(&aidx).is_some() {
                return None;
            }
            if !self.array_js_len.is_empty() && self.array_js_len.get(&aidx).is_some() {
                return None;
            }
            match self.heap.get(aidx) {
                HeapObj::Array(items) => {
                    // A HOLE element would take CreateListFromArrayLike through
                    // a proto-chain Get; ≤ 32 keeps the per-call scan bounded.
                    if items.len() > 32 || items.iter().any(|v| v.is_hole()) {
                        return None;
                    }
                    let n = items.len().min(Self::MI_MAX_REGS);
                    buf[..n].copy_from_slice(&items[..n]);
                    n
                }
                _ => return None,
            }
        };
        let bits = self.try_call_inline_argv(fid, this_v, &buf[..nargs])?;
        crate::vm::helpers_misc::callstats::inline_hit(true);
        Some(bits)
    }

    /// Materialize a `Thrown` from a region-helper-run operation exactly like
    /// `run_loop`'s Err path does (synthesize the Error object if no JS value
    /// is pending), then signal `CALL_THREW` so the region exits and the
    /// interpreter unwinds. Exempt from deopt accounting (a throw is not a
    /// region-quality signal).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_thrown_to_sentinel(&mut self, t: Thrown) -> u64 {
        if self.pending_throw.is_none() {
            let v = self.alloc_error_from_message(&t.0);
            self.realm_adopt_error(v);
            self.pending_throw = Some(v);
        }
        self.osr_deopt_exempt = true;
        crate::codegen::CALL_THREW
    }

    /// Q7 S-ACC: if the getter `fid` is the trivial shape `return this.<field>`
    /// AND `<field>` is a plain own writable/non-accessor DATA slot of `recv`,
    /// serve the read DIRECTLY (no frame push, no `setup_call`, no `run_loop`) —
    /// returning the slot's Value bits. Returns `None` to fall back to the full
    /// frame call (any other body shape, a non-instance receiver, a missing /
    /// accessor / inherited field).
    ///
    /// SOUNDNESS: the trivial body `[GetProp{obj:0, name:N}, Return{src}]` reads
    /// exactly `this.<field>` with `this == recv`. We only take the fast path
    /// when `recv.<field>` is an OWN DATA slot (so the read is a pure slot load
    /// with no further accessor / proto-chain semantics — byte-identical to what
    /// the frame-called getter would compute). Reg numbers are irrelevant: the
    /// body's single observable effect is the field load it returns. No side
    /// effect, so no deopt/re-execution hazard. The outer own-shadow guard (G3b)
    /// is ALREADY enforced by `ic_get_prop` (the `ClassGetter` validate arm
    /// requires `own.is_none()`), which ran before this — so a same-name instance
    /// own-write correctly never reaches here.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn accessor_fast_get(&self, fid: u32, recv: Value) -> Option<u64> {
        let field = self.simple_getter_field(fid)?;
        if !recv.is_heap() {
            return None;
        }
        let idx = recv.heap_index();
        if !self.ic_obj_ok(idx) {
            return None;
        }
        let m = match self.heap.get(idx) {
            HeapObj::Object(m) if !m.is_ctor => m,
            _ => return None,
        };
        let s = m.pos(field)?;
        // OWN, DATA (non-accessor) slot only — a nested accessor / inherited
        // field would need real semantics; defer those to the frame call.
        if m.attrs[s].accessor {
            return None;
        }
        Some(m.vals[s].bits())
    }

    /// Q7 S-ACC: if the setter `fid` is the trivial shape `this.<field> = arg`
    /// or `this.<field> = (arg | 0)` AND `<field>` is a plain own writable
    /// non-accessor DATA slot of `recv`, perform the write DIRECTLY (no frame).
    /// Returns `Some(0)` on a served write, or `None` to fall back to the frame
    /// call (any other shape, or a field that is missing / an accessor / non-
    /// writable / inherited — those need full `set_prop` semantics).
    ///
    /// SOUNDNESS: the body writes `this.<field>` with `this == recv`, optionally
    /// applying `ToInt32` (`x | 0`) to the value first. We serve it ONLY when the
    /// field is already an OWN WRITABLE DATA slot of `recv`, so the write is a
    /// pure in-place slot store — byte-identical to the frame-called setter. The
    /// slot value changes but the OBJECT SHAPE does not (no add/delete/redefine),
    /// so NO version bump is needed (mirrors the JIT SetProp IC fast write, which
    /// also stores in place without a bump). The outer own-shadow guard (G3b) is
    /// already enforced by `ic_set_prop`'s `ClassSetter` validate arm
    /// (`own.is_none()`), which ran before this.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn accessor_fast_set(&mut self, fid: u32, recv: Value, val: Value) -> Option<u64> {
        let (field, to_int32) = self.simple_setter_field(fid)?;
        if !recv.is_heap() {
            return None;
        }
        let idx = recv.heap_index();
        if !self.ic_obj_ok(idx) {
            return None;
        }
        // The stored value: optionally ToInt32 (`x | 0`). ToInt32 of a plain
        // number is unobservable (no user code), so it is safe off-frame; a
        // non-number value would need ToNumber (potentially observable
        // valueOf) — defer that to the frame call.
        let stored = if to_int32 {
            if !val.is_number() {
                return None;
            }
            let f = if val.is_int() { val.as_int() as f64 } else { val.as_f64() };
            Value::int(crate::vm::helpers_num2::to_int32(f))
        } else {
            val
        };
        // Nursery barrier: the off-frame trivial-setter store bypasses every
        // barriered helper (this is the single Rust chokepoint for it — the
        // method-inline evaluator's `mi_super_set` commits through here too).
        self.heap.write_barrier_val(idx, stored);
        // Re-borrow mutably and verify the field is an own writable data slot.
        match self.heap.get_mut(idx) {
            HeapObj::Object(m) if !m.is_ctor => {
                let s = m.pos(field)?;
                if m.attrs[s].accessor || !m.attrs[s].writable {
                    return None;
                }
                m.vals[s] = stored; // in-place data store — shape unchanged
                Some(0)
            }
            _ => None,
        }
    }

    /// Maximum nested `super.m()` hops the off-frame method evaluator follows
    /// before deopting to the full frame call. Class hierarchies deeper than
    /// this are rare; the bound keeps the recursion (a plain Rust call per hop)
    /// trivially safe and bounds the validation cost.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) const METHOD_INLINE_MAX_SUPER: u32 = 6;

    /// Capacity of the off-frame method evaluator's STACK register window. A body
    /// with `reg_count` above this declines to the frame call. Trivial method
    /// bodies are tiny; the cap keeps the per-call stack array small and avoids
    /// any heap allocation on the hot path.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) const MI_MAX_REGS: usize = 24;

}
