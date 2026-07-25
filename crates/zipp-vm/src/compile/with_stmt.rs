// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'a> FnCompiler<'a> {
    /// The active `with`-object registers that can shadow `name`, innermost
    /// first. Empty (the common case) when no `with` is active or the name is
    /// bound by a declaration INSIDE the innermost applicable `with` body —
    /// in which case the binding resolves statically with no dynamic probe.
    pub(crate) fn with_objs_for(&self, name: &str) -> Vec<Reg> {
        if self.with_stack.is_empty() {
            return Vec::new();
        }
        // Depth of the innermost lexical scope that declares `name` (-1 if the
        // name is free here → a global/upvalue/unresolved, shadowable by all).
        let mut depth: isize = -1;
        for (i, scope) in self.scopes.iter().enumerate() {
            if scope.iter().any(|(n, _)| n == name) {
                depth = i as isize;
            }
        }
        // A with entered ABOVE the binding's scope (floor > depth) can shadow it.
        self.with_stack
            .iter()
            .rev()
            .filter(|w| w.floor as isize > depth)
            .map(|w| w.obj_reg)
            .collect()
    }

    /// Like `with_objs_for` but yields each shadowing with-object's hidden
    /// BINDING NAME (for a nested function to capture as an upvalue).
    pub(crate) fn with_names_for(&self, name: &str) -> Vec<String> {
        if self.with_stack.is_empty() {
            return Vec::new();
        }
        let mut depth: isize = -1;
        for (i, scope) in self.scopes.iter().enumerate() {
            if scope.iter().any(|(n, _)| n == name) {
                depth = i as isize;
            }
        }
        self.with_stack
            .iter()
            .rev()
            .filter(|w| w.floor as isize > depth)
            .filter_map(|w| {
                self.scopes
                    .iter()
                    .flatten()
                    .find(|(_, r)| *r == w.obj_reg)
                    .map(|(n, _)| n.clone())
            })
            .collect()
    }

    /// EVERY with-object that can shadow `name` here — this function's own
    /// `with` scopes (innermost first) followed by ENCLOSING functions' with
    /// scopes (via `inherited_with_shadows`) — each materialized into a plain
    /// register for the probe chain (own cells unwrapped with CellGet,
    /// inherited ones loaded as upvalues). The with-aware identifier paths all
    /// route through this; it returns empty on the common no-`with` path.
    pub(crate) fn with_obj_regs(&mut self, name: &str) -> Vec<Reg> {
        let mut out: Vec<Reg> = Vec::new();
        for reg in self.with_objs_for(name) {
            if self.cell_regs.contains(&reg) {
                let t = self.temp();
                self.emit(Instr::CellGet { dst: t, cell: reg });
                out.push(t);
            } else {
                out.push(reg);
            }
        }
        // Enclosing-function withs apply only to names this function does not
        // bind itself (a local/param/self-name is never shadowed from outside).
        let bound_here = self.scopes.iter().flatten().any(|(n, _)| n == name)
            || self.self_name.as_ref().is_some_and(|(n, _)| n == name);
        if !bound_here {
            if let Some(chain) = self.inherited_with_shadows.get(name).cloned() {
                for wname in chain {
                    if let Some(idx) = self.resolve_upvalue(&wname) {
                        let t = self.temp();
                        self.emit(Instr::UpvalGet { dst: t, idx });
                        out.push(t);
                    }
                }
            }
        }
        out
    }

    /// Emit the per-with-object probe+read chain for a READ of `nidx`: for each
    /// object innermost-first, `WithHas` → on hit `GetProp` into `dst` then jump
    /// past the fallback. Returns the "jump to end" ips for the caller to patch
    /// AFTER it emits its own fallback (the static binding, or a literal).
    pub(crate) fn emit_with_get_chain(&mut self, nidx: u32, objs: &[Reg], dst: Reg) -> Vec<u32> {
        let mut end_jumps = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1; // reclaim the flag temp (dead after the branch)
            // GetBindingValue re-checks HasProperty (the WithHas @@unscopables
            // getter may delete the binding); strictness is the REFERENCE
            // site's, not the with statement's.
            self.emit(Instr::WithGet { dst, obj, name: nidx, strict: self.cx.in_strict });
            let je = self.here();
            self.emit(Instr::Jump { target: 0 });
            end_jumps.push(je);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        end_jumps
    }

    /// Emit the callee-resolution chain for a bare-identifier CALL inside a
    /// `with`: probe each with-object (innermost first); the first hit reads
    /// the callee via the GetBindingValue protocol (`WithGet`: HasProperty +
    /// Get) and records the with-object as `this` (WithBaseObject); the
    /// fall-through resolves the static binding with `this` = undefined.
    /// Returns `(callee_reg, this_reg)` — two freshly-allocated registers the
    /// caller must keep live across argument evaluation.
    pub(crate) fn emit_with_callee_chain(&mut self, name: &str, objs: &[Reg]) -> (Reg, Reg) {
        let nidx = self.string_name(name);
        let callee_reg = self.alloc_reg();
        let this_reg = self.alloc_reg();
        let mut chain_done = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1; // reclaim the flag temp
            self.emit(Instr::WithGet {
                dst: callee_reg,
                obj,
                name: nidx,
                strict: self.cx.in_strict,
            });
            self.emit(Instr::Move { dst: this_reg, src: obj });
            let jd = self.here();
            self.emit(Instr::Jump { target: 0 });
            chain_done.push(jd);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        // Fallback: the static binding, this = undefined.
        let b = self.resolve(name);
        let r = self.load_binding(&b, callee_reg);
        if r != callee_reg {
            self.emit(Instr::Move { dst: callee_reg, src: r });
        }
        self.emit(Instr::LoadUndefined { dst: this_reg });
        let end = self.here();
        for j in chain_done {
            self.patch_jump(j, end);
        }
        (callee_reg, this_reg)
    }

    /// Emit a `with`-aware read of `name` into `dst`: probe each with-object
    /// (innermost first) and read from the first that has the binding; otherwise
    /// fall back to the static (lexical/global) binding. Returns `dst`.
    pub(crate) fn load_with(&mut self, name: &str, objs: &[Reg], dst: Reg) -> Reg {
        let nidx = self.string_name(name);
        let end_jumps = self.emit_with_get_chain(nidx, objs, dst);
        // Fallback: the static binding.
        let b = self.resolve(name);
        let r = self.load_binding(&b, dst);
        if r != dst {
            self.emit(Instr::Move { dst, src: r });
        }
        let end = self.here();
        for je in end_jumps {
            self.patch_jump(je, end);
        }
        dst
    }

    /// Emit a `with`-aware write of `src` to `name`: store into the first
    /// with-object (innermost first) that has the binding; otherwise fall back
    /// to the static (lexical/global) binding.
    pub(crate) fn store_with(&mut self, name: &str, objs: &[Reg], src: Reg) {
        let nidx = self.string_name(name);
        let mut end_jumps = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1;
            self.emit(Instr::WithSet { obj, name: nidx, val: src, strict: self.cx.in_strict });
            let je = self.here();
            self.emit(Instr::Jump { target: 0 });
            end_jumps.push(je);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        let b = self.resolve(name);
        self.store_binding(&b, src);
        let end = self.here();
        for je in end_jumps {
            self.patch_jump(je, end);
        }
    }

    /// Phase 1 of a reference-record-style with-aware assignment: probe each
    /// with-object (innermost first) and capture the FIRST that has the binding
    /// into a fresh register — `undefined` when none does (→ the static
    /// binding). ResolveBinding's HasProperty probes (observable through a
    /// Proxy `has` trap) run NOW, BEFORE the caller evaluates the initializer/
    /// RHS; `with_store_resolved` then writes to the base captured here even if
    /// the property was deleted in between (`var x = delete o.x` inside
    /// `with (o)` re-creates `o.x`). The register stays live until the store —
    /// callers reclaim it with their surrounding `next_reg` reset.
    pub(crate) fn with_resolve_target(&mut self, name: &str, objs: &[Reg]) -> Reg {
        let nidx = self.string_name(name);
        let target = self.alloc_reg();
        self.emit(Instr::LoadUndefined { dst: target });
        let mut end_jumps = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1;
            self.emit(Instr::Move { dst: target, src: obj });
            let je = self.here();
            self.emit(Instr::Jump { target: 0 });
            end_jumps.push(je);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        let end = self.here();
        for je in end_jumps {
            self.patch_jump(je, end);
        }
        target
    }

    /// Phase 2: PutValue to the base `with_resolve_target` picked — the captured
    /// with-object (`WithSet` = the spec's object-env SetMutableBinding:
    /// re-HasProperty + strict check + Set) or, when the register holds
    /// `undefined` (objects are always truthy, so the test is unambiguous),
    /// the static binding.
    pub(crate) fn with_store_resolved(&mut self, name: &str, target: Reg, src: Reg) {
        let nidx = self.string_name(name);
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond: target, target: 0 });
        self.emit(Instr::WithSet { obj: target, name: nidx, val: src, strict: self.cx.in_strict });
        let je = self.here();
        self.emit(Instr::Jump { target: 0 });
        let stat = self.here();
        self.patch_jump(jf, stat);
        let b = self.resolve(name);
        self.store_binding(&b, src);
        let end = self.here();
        self.patch_jump(je, end);
    }

    /// Emit a `with`-aware `delete name` into `dst`: delete from the first
    /// with-object (innermost first) that has the binding, else fall back to the
    /// static-binding delete semantics (mirrors `delete_expr`'s identifier arm).
    pub(crate) fn delete_with(&mut self, name: &str, objs: &[Reg], dst: Reg) -> Reg {
        let nidx = self.string_name(name);
        let mut end_jumps = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1;
            self.emit(Instr::DeleteProp { dst, obj, name: nidx, strict: false });
            let je = self.here();
            self.emit(Instr::Jump { target: 0 });
            end_jumps.push(je);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        // Fallback: the static binding's delete result (no with-object had it).
        let non_configurable = matches!(name, "NaN" | "Infinity" | "undefined")
            || match self.resolve_existing(name) {
                Some(Binding::Local(_))
                | Some(Binding::LocalCell(_))
                | Some(Binding::Upvalue(_))
                | Some(Binding::ClassName(_)) => true,
                Some(Binding::Global(slot)) => {
                    self.cx.hoisted_globals.contains(&slot)
                        || self.cx.lexical_globals.contains(&slot)
                        || self.cx.decl_globals.contains(&slot)
                }
                None => false,
            };
        self.emit(Instr::LoadBool { dst, val: !non_configurable });
        let end = self.here();
        for je in end_jumps {
            self.patch_jump(je, end);
        }
        dst
    }

}
