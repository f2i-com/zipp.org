// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// Frame-call a resolved plain user function FROM NATIVE REGION CODE and
    /// run it to completion: the shared tail of the region call helpers and the
    /// property-slow helpers (getters/setters). Returns the result bits,
    /// `SELF_CALL_DEOPT` (setup failed without materializing a JS error — the
    /// interpreter re-executes the op), or `CALL_THREW` (`pending_throw` set;
    /// the region exits and the interpreter unwinds). `dst` = 0 is never
    /// written: `run_loop` returns the stop frame's value BEFORE delivering to
    /// `ret_dst`; the native caller stores/discards it itself.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn jit_frame_call(
        &mut self,
        fid: u32,
        closure: u32,
        this_v: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        ip: usize,
        callee_v: Value,
    ) -> u64 {
        use crate::codegen::{CALL_THREW, SELF_CALL_DEOPT};
        if self
            .setup_call(fid, closure, this_v, caller_base, arg_base, argc, 0, ip + 1, callee_v)
            .is_err()
        {
            // MAX_FRAMES / regs overflow / this-boxing error. If a JS error
            // value was already materialized, unwind (never re-execute);
            // otherwise let the interpreter re-execute the op and surface the
            // identical error itself.
            return if self.pending_throw.is_some() {
                self.osr_deopt_exempt = true;
                CALL_THREW
            } else {
                SELF_CALL_DEOPT
            };
        }
        let stop = self.frames.len() - 1;
        self.jit_call_depth += 1;
        let r = self.run_loop(stop);
        self.jit_call_depth -= 1;
        match r {
            Ok(v) => v.bits(),
            Err(_) => {
                // The callee threw and nothing below `stop` handled it:
                // `pending_throw` is set, the callee frames are unwound, and the
                // region must EXIT so the interpreter unwinds the region's own
                // frame (its try handlers were pushed before the loop). A throw
                // is not a region-quality signal — exempt the deopt counter.
                self.osr_deopt_exempt = true;
                CALL_THREW
            }
        }
    }

    /// The implementation behind `jit_get_prop_slow` / `jit_set_prop_slow`: a
    /// region's prop-miss helper found a resolution only the interpreter's
    /// per-site IC machinery can serve (an accessor needing a frame call, or a
    /// class-instance receiver). Consults `ic_get_prop` / `ic_set_prop` — the
    /// SAME caches the interpreter uses — and frame-calls plain getters and
    /// setters to completion. Returns the value bits (get) or 0 (set),
    /// `SELF_CALL_DEOPT` (no IC resolution — the interpreter re-executes the
    /// op), or `CALL_THREW`. Reentrancy contract: as `jit_region_call_impl`
    /// (the calling region re-derives r13/r14 after this helper).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_prop_slow_impl(
        &mut self,
        caller_base_ptr: *const u64,
        packed_fip: u64,
        packed2: u64,
        is_set: bool,
    ) -> u64 {
        use crate::codegen::{CALL_THREW, SELF_CALL_DEOPT};
        use crate::vm::ic::{GetAct, SetAct};
        if self.jit_call_depth >= JIT_REGION_CALL_MAX {
            self.osr_deopt_exempt = true;
            return SELF_CALL_DEOPT;
        }
        let func_id = (packed_fip >> 32) as u32;
        let ip = (packed_fip as u32) as usize;
        let name = (packed2 >> 32) as u32;
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' pinned buffer.
        let base = unsafe { caller_base_ptr.offset_from(regs_base) } as usize;
        let key: &str = &self.func(func_id as usize).string_constants[name as usize];
        if is_set {
            let obj_reg = ((packed2 >> 16) & 0xFFFF) as u16;
            let val_reg = (packed2 & 0xFFFF) as u16;
            let recv = self.get(base, obj_reg);
            let val = self.get(base, val_reg);
            match self.ic_set_prop(func_id, ip, recv, key, val) {
                SetAct::Done => 0,
                SetAct::Setter { fid, closure, setter } => {
                    // Q7 S-ACC: a trivial `this.field = arg|0` setter over an own
                    // writable data slot is served off-frame (no setup_call /
                    // run_loop) — the dominant cost of the rt loop.
                    if let Some(r) = self.accessor_fast_set(fid, recv, val) {
                        return r;
                    }
                    // S2 SUPER-ACC: a trivial setter body whose only effect is a
                    // `super.<name> = …` over an inherited trivial setter (e.g.
                    // `set v(x){ super.v = x }`) runs off-frame via the two-pass
                    // method-inline evaluator (pass 1 validates the WHOLE body before
                    // pass 2 commits the single super-set). `argc = 1`, the value in
                    // `val_reg`. `None` ⇒ a non-trivial body ⇒ frame-call below.
                    match self.try_method_inline(fid, recv, base, val_reg, 1) {
                        Some(CALL_THREW) | Some(SELF_CALL_DEOPT) => {
                            // A nested super target threw / declined mid-flight.
                            // CALL_THREW: a committed throw — propagate (region
                            // exits). SELF_CALL_DEOPT cannot escape try_method_inline
                            // (its arms convert it to None), but propagate defensively.
                            return CALL_THREW;
                        }
                        Some(_) => return 0, // served — the setter's return is discarded
                        None => {}           // not inlinable — fall through to frame call
                    }
                    let r = self.jit_frame_call(fid, closure, recv, base, val_reg, 1, ip, setter);
                    if r == CALL_THREW || r == SELF_CALL_DEOPT {
                        r
                    } else {
                        0 // the setter's return value is discarded
                    }
                }
                SetAct::None => SELF_CALL_DEOPT,
            }
        } else {
            let obj_reg = (packed2 & 0xFFFF) as u16;
            let recv = self.get(base, obj_reg);
            match self.ic_get_prop(func_id, ip, recv, key) {
                GetAct::Value(v) => v.bits(),
                GetAct::Accessor { fid, closure, getter } => {
                    // Q7 S-ACC: a trivial `return this.field` getter over an own
                    // data slot is served off-frame (no setup_call / run_loop).
                    if let Some(bits) = self.accessor_fast_get(fid, recv) {
                        return bits;
                    }
                    // S2 SUPER-ACC: a trivial getter body containing a `super.<name>`
                    // read (e.g. `get v(){ return super.v * 2 }`) runs off-frame via
                    // the two-pass method-inline evaluator (`argc = 0`). It resolves
                    // the super read with the version-guarded `ic_super_get` and reads
                    // an inherited data slot / trivial super-getter field directly.
                    // `None` ⇒ a non-trivial body ⇒ frame-call below.
                    if let Some(bits) = self.try_method_inline(fid, recv, base, 0, 0) {
                        return bits;
                    }
                    self.jit_frame_call(fid, closure, recv, base, 0, 0, ip, getter)
                }
                GetAct::None => SELF_CALL_DEOPT,
            }
        }
    }

    /// Reserve enough register-file capacity that a full JIT self-recursion
    /// never reallocates `self.regs` (which would dangle native window
    /// pointers). Called before entering the top-level run.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn reserve_jit_regs(&mut self) {
        // The register file must NEVER reallocate while a native JIT frame holds
        // a raw pointer into it (the caller's window pointer lives in a callee-
        // saved register across the self-call helper). A realloc there dangles
        // it → memory corruption. So reserve the absolute worst case up front:
        // every possible frame (`MAX_FRAMES`) holding the largest window. Then
        // no growth site can ever exceed capacity (each is also guarded), so the
        // Vec is pinned for the VM's lifetime.
        //
        // Cost is bounded: capacity is clamped so the reserve can't exceed
        // ~256 MiB even for a pathological max_window; if the cap is hit, deep
        // recursion simply throws RangeError sooner (a `reg_capacity` field
        // records the real limit so the growth guards agree).
        let max_window = self
            .program
            .functions
            .iter()
            .map(|f| (f.reg_count as usize).max(1))
            .max()
            .unwrap_or(1);
        const MAX_REGS_BYTES: usize = 256 * 1024 * 1024; // 256 MiB ceiling
        let worst_case = max_window.saturating_mul(MAX_FRAMES);
        let capped = worst_case.min(MAX_REGS_BYTES / std::mem::size_of::<Value>());
        let target = self.regs.len() + capped;
        self.regs.reserve(target - self.regs.len());
        // Record the pinned capacity: growth sites must not exceed it (else the
        // Vec would realloc). Use the ACTUAL capacity Rust gave us (≥ requested).
        self.reg_capacity = self.regs.capacity();
    }

    /// Allocate a string on the heap and return its boxed Value.
    pub fn alloc_str(&mut self, s: String) -> Value {
        Value::heap(self.heap.alloc_str(s))
    }

    /// Set the directory used to resolve a dynamic `import(specifier)` against the
    /// filesystem (the running script's directory). Without it, `import()` rejects.
    pub fn set_module_base_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.module_base_dir = dir;
    }

}
