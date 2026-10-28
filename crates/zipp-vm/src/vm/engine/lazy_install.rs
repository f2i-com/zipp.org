//! A deferred function body compiled at its function's first call
//! (`compile::LazySession`): the work of the stub's `LazyCompile`.
//!
//! The body's functions take the ids its stub reserved for them, so they are
//! main-program functions to every cache that asks; its new global names are
//! looked up as an eval's are; and the stub's id is re-pointed at the body.
//! Nothing ran the stub but this, so the only state that knew it is the call
//! caches of the one callee whose call is compiling it (reset by a version
//! bump) and the per-function decisions taken from its code (reset here).
#![allow(unused_imports)]
use super::*;
use crate::bytecode::FuncProto;

impl<'p> Vm<'p> {
    /// Give this VM the session that compiles its program's deferred bodies.
    /// Such a program runs in the interpreter alone: the native tiers' plans
    /// read a callee's code before its first call, which a stub's does not
    /// answer for.
    pub(crate) fn set_lazy_session(&mut self, session: crate::compile::LazySession<'p>) {
        #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            self.jit_enabled = false;
        }
        self.lazy = Some(Box::new(session));
    }

    /// How many deferred bodies have compiled, and how many wait (`None`
    /// when every body compiled with the program).
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), allow(dead_code))]
    pub(crate) fn lazy_counts(&self) -> Option<(usize, usize)> {
        self.lazy.as_ref().map(|s| s.counts())
    }

    /// `LazyCompile` in the top frame, stub `fid`'s: compile and install the
    /// body, then make the frame the body's, its window grown to the body's
    /// registers (the ones past the stub's undefined, as a call leaves them)
    /// and its next instruction the body's first.
    #[cold]
    #[inline(never)]
    pub(crate) fn lazy_compile_frame(&mut self, fid: u32) -> Result<(), Thrown> {
        let stub_regs = (self.func(fid as usize).reg_count as usize).max(1);
        self.lazy_install(fid)?;
        let top = self.frames.len() - 1;
        let base = self.frames[top].base;
        let regs = (self.func(fid as usize).reg_count as usize).max(1);
        if self.regs.len() < base + regs {
            if self.regs_would_overflow(base + regs) {
                return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
            }
            self.regs.resize(base + regs, Value::UNDEFINED);
        }
        for r in &mut self.regs[base + stub_regs..base + regs] {
            *r = Value::UNDEFINED;
        }
        self.frames[top].ip = 0;
        // The call caches that resolved this callee hold the stub's frame
        // facts (`lean_call_desc`), keyed by the callee's version.
        let callee = self.frames[top].callee;
        if callee.is_heap() {
            self.heap.bump_version(callee.heap_index());
        }
        Ok(())
    }

    /// Compile the body of stub `fid` and install it, with the functions it
    /// adds. A compile error is thrown where the call is.
    pub(crate) fn lazy_install(&mut self, fid: u32) -> Result<(), Thrown> {
        let Some(mut session) = self.lazy.take() else {
            return Err(Thrown(format!("internal: function {fid} is a stub but nothing compiles it")));
        };
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Compile);
        let footprint = crate::heap::footprint_bytes();
        let result = match session.compile(fid, self.funcs.len() as u32) {
            Ok(body) => self.lazy_place(&session, fid, body),
            Err(e) if e.starts_with("SyntaxError") || e.starts_with("RangeError") => Err(Thrown(e)),
            Err(e) => Err(Thrown(format!("SyntaxError: {e}"))),
        };
        self.lazy = Some(session);
        // What the compile kept is code, not objects (`Heap::discount_footprint`).
        self.heap.discount_footprint(crate::heap::footprint_bytes().saturating_sub(footprint));
        result
    }

    /// Install compiled body `body` of stub `fid`.
    fn lazy_place(
        &mut self,
        session: &crate::compile::LazySession<'p>,
        fid: u32,
        body: crate::compile::LazyBody,
    ) -> Result<(), Thrown> {
        let crate::compile::LazyBody { base, mut root, nested } = body;
        let stub = self.func(fid as usize);
        if let Some(why) = crate::compile::stub_mismatch(stub, &root) {
            return Err(Thrown(format!("internal: stub {fid} ({}) does not match its body: {why}", stub.name)));
        }
        // What the stub's maker set on it after its compile call.
        root.name = stub.name.clone();
        root.source = stub.source.clone();
        root.name_global = stub.name_global;
        root.non_constructable |= stub.non_constructable;
        let mut protos: Vec<(u32, FuncProto)> =
            nested.into_iter().enumerate().map(|(i, f)| (base + i as u32, f)).collect();
        protos.push((fid, root));
        for (_, f) in protos.iter_mut() {
            for ins in f.code.iter_mut() {
                self.lazy_map_globals(session, ins)?;
            }
        }
        // The program's own object-literal plans: its compile kept them
        // within the program's bounds, so they are charged, never declined
        // (a `FinalizeObject` site cannot lose its plan).
        if let Some((sites, bytes)) = crate::bytecode::static_key_plan_usage(protos.iter().map(|(_, f)| f)) {
            self.static_key_plan_sites = self.static_key_plan_sites.saturating_add(sites);
            self.static_key_plan_retained_bytes = self.static_key_plan_retained_bytes.saturating_add(bytes);
        }
        for (id, f) in protos {
            let node = self.eval_arena.funcs.push(self.eval_func_tail, f);
            self.eval_func_tail = Some(node);
            let r: &'p FuncProto = &node.value;
            // As for eval code: a slot the new code stores to is no longer
            // one the program's scan proved store-free.
            for ins in &r.code {
                match *ins {
                    Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::StoreGlobalResolved { idx, .. }
                    | Instr::StoreGlobalDyn { idx, .. }
                    | Instr::EvalScopeSet { idx, .. } => {
                        if self.bytecode_stored_slots.insert(idx) {
                            self.bump_global_gen(idx);
                        }
                    }
                    _ => {}
                }
            }
            let i = id as usize;
            if i < self.funcs.len() {
                self.funcs[i] = r;
                if i >= self.main_func_count {
                    self.eval_funcs[i - self.main_func_count] = r;
                }
            } else {
                debug_assert_eq!(i, self.funcs.len());
                self.install_func(r);
            }
        }
        self.lazy_forget(fid);
        Ok(())
    }

    /// `ins`'s global-slot operands, the session's numbering, as live slots:
    /// the main program's slots are the VM's, and a name only deferred code
    /// uses is looked up by name (as an eval's is, in the main realm's table:
    /// the function is the main program's wherever it is called from).
    fn lazy_map_globals(&mut self, session: &crate::compile::LazySession<'p>, ins: &mut Instr) -> Result<(), Thrown> {
        match ins {
            Instr::LoadGlobal { idx, .. }
            | Instr::LoadGlobalOrUndefined { idx, .. }
            | Instr::StoreGlobal { idx, .. }
            | Instr::StoreGlobalStrict { idx, .. }
            | Instr::StoreGlobalResolved { idx, .. }
            | Instr::LoadGlobalDyn { idx, .. }
            | Instr::LoadGlobalOrUndefinedDyn { idx, .. }
            | Instr::StoreGlobalDyn { idx, .. }
            | Instr::EvalScopeHas { idx, .. }
            | Instr::EvalScopeSet { idx, .. } => *idx = self.lazy_global(session, *idx)?,
            Instr::DeleteGlobal { slot, .. } => *slot = self.lazy_global(session, *slot)?,
            Instr::LoadUpvalDyn { name, .. } | Instr::StoreUpvalDyn { name, .. } => {
                *name = self.lazy_global(session, *name)?
            }
            Instr::MathOp {
                callee: crate::bytecode::NO_REG,
                this_v,
                ..
            } if *this_v != crate::bytecode::BARE_MATH_BY_NAME => {
                let live = self.lazy_global(session, u32::from(*this_v))?;
                *this_v = u16::try_from(live)
                    .ok()
                    .filter(|&v| v < crate::bytecode::BARE_MATH_BY_NAME)
                    .unwrap_or(crate::bytecode::BARE_MATH_BY_NAME);
            }
            _ => {}
        }
        Ok(())
    }

    /// The live slot of the session's global slot `slot`.
    fn lazy_global(&mut self, session: &crate::compile::LazySession<'p>, slot: u32) -> Result<u32, Thrown> {
        let main = session.main_globals();
        if slot < main {
            return Ok(slot);
        }
        let i = (slot - main) as usize;
        if let Some(&live) = self.lazy_gmap.get(i) {
            if live != u32::MAX {
                return Ok(live);
            }
        }
        let name = session
            .global_name(slot)
            .ok_or_else(|| Thrown(format!("internal: deferred code names global slot {slot}, which has no name")))?
            .to_string();
        let realm = self.active_realm.take();
        let live = self.eval_global_slot(&name);
        self.active_realm = realm;
        let live = live?;
        if self.lazy_gmap.len() <= i {
            self.lazy_gmap.resize(i + 1, u32::MAX);
        }
        self.lazy_gmap[i] = live;
        Ok(live)
    }

    /// Drop every decision taken from function `fid`'s code, which has just
    /// changed from its stub's to its body's.
    fn lazy_forget(&mut self, fid: u32) {
        let f = fid as usize;
        if let Some(t) = self.site_ics.get_mut(f) {
            *t = None;
        }
        if let Some(d) = self.const_string_dense.get_mut(f) {
            d.clear();
        }
        self.const_string_cache_funcs.remove(&fid);
        if let Some(m) = self.sloppy_eval_memo.get_mut(f) {
            *m = 0;
        }
        if let Some(s) = self.in_probe_sites.get_mut(f) {
            *s = None;
        }
        if let Some(g) = self.gen_inline_ok.get_mut(f) {
            *g = 0;
        }
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            self.ctor_stores.remove(&fid);
            if self.ctor_store_last.0 == fid {
                self.ctor_store_last = Default::default();
            }
            // The last class a compiled `new` resolved may have this body.
            self.new_class_memo = (0, 0, 0, 0, 0);
            if let Some(c) = self.ctor_framefree.get_mut(f) {
                *c = 0;
            }
            if let Some(m) = self.mi_cache.get_mut(f) {
                *m = i32::MIN;
            }
        }
        #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
        self.lite_forget(fid);
    }
}
