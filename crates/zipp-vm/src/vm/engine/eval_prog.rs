// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// Install a compiled eval/module Program into the live realm (remap its global
    /// slots, function ids, and class ids onto the running tables; hoist `var`s and
    /// top-level functions) and run its top-level function to completion, returning
    /// the completion value.
    /// Phases 1-5: remap the program's global slots onto live slots (the `gmap`),
    /// install its functions + classes, and hoist vars/functions — WITHOUT running
    /// the top-level body. Returns `(gmap, base_func)`; the caller runs the body via
    /// `execute_eval_program` (split out so a module can register its namespace in the
    /// loader cache between prepare and execute, for self/cyclic imports).
    pub(crate) fn prepare_eval_program(
        &mut self,
        eval_prog: crate::bytecode::Program,
        module: bool,
        caller_home: Option<u32>,
        var_env_global: bool,
        eval_scope_idx: Option<u32>,
        import_aliases: Option<&std::collections::HashMap<u32, u32>>,
        self_aliases: Option<&std::collections::HashMap<u32, u32>>,
        prealloc: Option<&std::collections::HashMap<u32, u32>>,
    ) -> Result<(Vec<u32>, u32), Thrown> {
        use crate::bytecode::{FuncProto, Instr};
        // A $262.evalScript: SCRIPT GlobalDeclarationInstantiation semantics
        // for THIS program only (lexical-collision SyntaxErrors, realm-
        // persistent lexicals, non-configurable brandNew var/fn bindings).
        let script_gdi = std::mem::take(&mut self.eval_script_gdi);
        // Runtime base ids: eval functions and classes are appended past the
        // compile-time tables (parallel to global slots).
        let base_func = (self.main_func_count + self.eval_funcs.len()) as u32;
        let base_class = (self.main_class_count + self.eval_classes.len()) as u32;
        // 3. Remap the eval program's own global-slot numbering onto live slots.
        //    For a MODULE, each slot it DECLARES (var/let/const/function/class +
        //    `*default*`) draws a FRESH per-module slot so two modules' same-named
        //    exports don't collide — the foundation for correct live bindings. A
        //    free reference (builtin or import) still resolves realm-shared by name.
        let decl: std::collections::HashSet<u32> = if module {
            eval_prog.module_decl_globals.iter().copied().collect()
        } else {
            std::collections::HashSet::new()
        };
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        let mut gmap: Vec<u32> = Vec::with_capacity(eval_prog.global_names.len());
        for (i, name) in eval_prog.global_names.iter().enumerate() {
            // A static-import LOCAL aliases the dependency's resolved live
            // export slot — sharing one flat slot IS the live binding.
            if let Some(&alias) = import_aliases.and_then(|m| m.get(&(i as u32))) {
                gmap.push(alias);
                continue;
            }
            if module && decl.contains(&(i as u32)) {
                // A slot the module loader PRE-ALLOCATED (for cyclic re-export
                // resolution) is reused — it is already UNINITIALIZED.
                if let Some(&pre) = prealloc.and_then(|m| m.get(&(i as u32))) {
                    gmap.push(pre);
                    continue;
                }
                if self.eval_global_next >= cap {
                    return Err(Thrown(
                        "EvalError: too many distinct globals introduced by eval".into(),
                    ));
                }
                let s = self.eval_global_next;
                self.eval_global_next += 1;
                self.globals[s as usize] = Value::UNINITIALIZED;
                gmap.push(s);
            } else {
                gmap.push(self.eval_global_slot(name)?);
            }
        }
        // Second pass: a SELF-import local aliases the module's own exported
        // local — both compile slots map to ONE live slot (live binding).
        if let Some(sa) = self_aliases {
            for (&import_local, &target) in sa {
                let live = gmap[target as usize];
                gmap[import_local as usize] = live;
            }
        }
        // 4. Install eval classes: re-index their member func ids (which point into
        //    the eval functions installed below) by base_func, leak each ClassDef,
        //    and reserve a class_values slot per class (MakeClass writes it). A
        //    ClassDef holds no class-id references, so only func ids are offset.
        for mut cd in eval_prog.classes {
            if let Some(c) = cd.ctor.as_mut() {
                *c += base_func;
            }
            // The deferred instance-field thunk of a derived class with an
            // explicit ctor is a func id too — left unrebased it named a MAIN
            // program function, so `eval("class D extends B { x = 1; … }")`
            // re-entered whatever proto happened to sit at that index (typically
            // the eval body itself → stack overflow).
            if let Some(t) = cd.field_thunk.as_mut() {
                *t += base_func;
            }
            for lst in [
                &mut cd.methods,
                &mut cd.getters,
                &mut cd.setters,
                &mut cd.statics,
                &mut cd.static_getters,
                &mut cd.static_setters,
            ] {
                for (_, fid) in lst.iter_mut() {
                    *fid += base_func;
                }
            }
            self.eval_classes.push(Box::leak(Box::new(cd)));
            self.class_values.push(None);
        }
        // 5. Re-index function-id, global-slot, and class-id operands, leak each
        //    FuncProto (stable address — raw pointers live into it), append.
        let mut new_funcs: Vec<&'static FuncProto> =
            Vec::with_capacity(eval_prog.functions.len());
        for mut f in eval_prog.functions {
            for ins in f.code.iter_mut() {
                match ins {
                    Instr::MakeFunc { func_id, .. }
                    | Instr::MakeClosure { func_id, .. }
                    | Instr::MakeArrow { func_id, .. } => {
                        *func_id += base_func;
                    }
                    // A computed class member (`class C { [k](){} }`) carries its
                    // proto as a bare func id, NOT as a MakeFunc — it is installed
                    // by this op after the key is evaluated, so it needs the same
                    // offset as every other func-id operand.
                    Instr::ClassAddMember { func, .. } => *func += base_func,
                    Instr::LoadGlobal { idx, .. }
                    | Instr::LoadGlobalOrUndefined { idx, .. }
                    | Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::LoadGlobalDyn { idx, .. }
                    | Instr::LoadGlobalOrUndefinedDyn { idx, .. }
                    | Instr::StoreGlobalDyn { idx, .. }
                    | Instr::EvalScopeHas { idx, .. }
                    | Instr::EvalScopeSet { idx, .. } => {
                        *idx = gmap[*idx as usize];
                    }
                    // `delete <global>`: the slot operand maps like every other
                    // global reference (the runtime checks the MAIN program's
                    // decl lists against the mapped slot).
                    Instr::DeleteGlobal { slot, .. } => {
                        *slot = gmap[*slot as usize];
                    }
                    // The upvalue index is the eval closure's own; only the
                    // NAME handle is a global slot.
                    Instr::LoadUpvalDyn { name, .. } | Instr::StoreUpvalDyn { name, .. } => {
                        *name = gmap[*name as usize];
                    }
                    // Class-id operands: the class itself, the class inner-name
                    // read (`C` inside `class C`), and every `super` reference
                    // (which names its home class).
                    Instr::MakeClass { class_id, .. } => *class_id += base_class,
                    // A class-inner-name read carries the same SENTINEL as the
                    // `super` ops when it names the EVAL CALLER's class
                    // (`class C { m(){ return eval("C") } }`): that class lives
                    // in the caller's table, not this program's.
                    Instr::LoadClassValue { class_id, .. } => {
                        if *class_id == u32::MAX {
                            if let Some(h) = caller_home {
                                *class_id = h;
                            }
                        } else {
                            *class_id += base_class;
                        }
                    }
                    Instr::SuperCtor { home_class_id, .. }
                    | Instr::SuperCtorSpread { home_class_id, .. }
                    | Instr::SuperMethod { home_class_id, .. }
                    | Instr::SuperGet { home_class_id, .. }
                    | Instr::SuperGetComputed { home_class_id, .. }
                    | Instr::SuperMethodComputed { home_class_id, .. }
                    | Instr::SuperMethodSpread { home_class_id, .. }
                    | Instr::SuperMethodComputedSpread { home_class_id, .. }
                    | Instr::SuperSet { home_class_id, .. }
                    | Instr::SuperSetComputed { home_class_id, .. } => {
                        // The SENTINEL marks "the eval caller's home class": swap
                        // in its RUNTIME class id (already absolute); real ids
                        // shift past the main program's class table.
                        if *home_class_id == u32::MAX {
                            if let Some(h) = caller_home {
                                *home_class_id = h;
                            }
                        } else {
                            *home_class_id += base_class;
                        }
                    }
                    Instr::DirectEval { home_class, .. } => {
                        if *home_class == u32::MAX {
                            if let Some(h) = caller_home {
                                *home_class = h;
                            }
                        } else {
                            *home_class += base_class;
                        }
                    }
                    Instr::FieldInit { class_id, .. } => {
                        if *class_id != u32::MAX {
                            *class_id += base_class;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(s) = f.name_global {
                f.name_global = Some(gmap[s as usize] as u16);
            }
            new_funcs.push(Box::leak(Box::new(f)));
        }
        for r in new_funcs {
            self.eval_funcs.push(r);
        }
        // EvalDeclarationInstantiation step 5.a: a sloppy eval may not
        // var/function-declare a name that is a GLOBAL lexical (let/const) —
        // SyntaxError BEFORE any binding is created.
        let count = self.eval_funcs.len();
        let start = (base_func as usize) - self.main_func_count;
        if var_env_global {
            // SCRIPT GDI steps 3-4: every lexically-declared name of THIS
            // script must collide with NEITHER an existing var/function
            // declaration NOR an existing lexical NOR a non-configurable
            // global-object property — SyntaxError BEFORE any binding
            // (including this script's own vars) is created.
            if script_gdi {
                for &slot in &eval_prog.lexical_globals {
                    let rs = gmap[slot as usize];
                    let name = self.global_slot_name(rs).unwrap_or_default();
                    let has_var = self.program.hoisted_globals.contains(&rs)
                        || self.program.decl_globals.contains(&rs)
                        || self.eval_var_globals.contains(&rs);
                    let has_lex = self.program.lexical_globals.contains(&rs)
                        || self.eval_lexical_globals.contains(&rs);
                    let restricted = self.global_this != 0
                        && matches!(
                            self.heap.get(self.global_this),
                            HeapObj::Object(m)
                                if m.pos(&name).map_or(false, |i| !m.attrs[i].configurable)
                        );
                    if has_var || has_lex || restricted {
                        return Err(Thrown(format!(
                            "SyntaxError: Identifier '{name}' has already been declared"
                        )));
                    }
                }
            }
            let mut lex_clash: Option<String> = None;
            for &slot in &eval_prog.hoisted_globals {
                let rs = gmap[slot as usize];
                if self.program.lexical_globals.contains(&rs)
                    || self.eval_lexical_globals.contains(&rs)
                {
                    lex_clash = self.global_slot_name(rs);
                    break;
                }
            }
            if lex_clash.is_none() {
                for local in start..count {
                    if let Some(slot) = self.eval_funcs[local].name_global {
                        if self.program.lexical_globals.contains(&(slot as u32))
                            || self.eval_lexical_globals.contains(&(slot as u32))
                        {
                            lex_clash = self.global_slot_name(slot as u32);
                            break;
                        }
                    }
                }
            }
            if let Some(name) = lex_clash {
                return Err(Thrown(format!(
                    "SyntaxError: Identifier '{name}' has already been declared"
                )));
            }
            // CanDeclareGlobalVar: an ABSENT binding can only be created while
            // the global object is extensible — else TypeError (before any
            // binding is created).
            let global_extensible = self.global_this == 0
                || matches!(self.heap.get(self.global_this), HeapObj::Object(m) if m.extensible);
            if !global_extensible {
                for &slot in &eval_prog.hoisted_globals {
                    let rs = gmap[slot as usize];
                    if self.globals[rs as usize].bits() != Value::UNINITIALIZED.bits() {
                        continue;
                    }
                    if let Some(name) = self.global_slot_name(rs) {
                        let has_own = matches!(
                            self.heap.get(self.global_this),
                            HeapObj::Object(m) if m.pos(&name).is_some()
                        );
                        if !has_own && self.global_by_name(&name).is_none() {
                            return Err(Thrown(format!(
                                "TypeError: cannot declare global variable {name}"
                            )));
                        }
                    }
                }
            }
        }
        // Validation pass: CanDeclareGlobalFunction for EVERY function
        // name before ANY binding (var or function) is created — a later
        // non-definable function must leave earlier vars/functions undeclared.
        if var_env_global {
            for local in start..count {
                if let Some(slot) = self.eval_funcs[local].name_global {
                    if (slot as usize) >= self.globals.len()
                        || self.globals[slot as usize].bits() != Value::UNINITIALIZED.bits()
                    {
                        continue;
                    }
                    if let Some(name) = self.global_slot_name(slot as u32) {
                        if matches!(name.as_str(), "NaN" | "Infinity" | "undefined") {
                            return Err(Thrown(format!(
                                "TypeError: cannot declare global function {name}"
                            )));
                        }
                        if self.global_this != 0 {
                            let pos_attrs = match self.heap.get(self.global_this) {
                                HeapObj::Object(m) => m.pos(&name).map(|i| m.attrs[i]),
                                _ => None,
                            };
                            if let Some(a) = pos_attrs {
                                if !a.configurable
                                    && (a.accessor || !a.writable || !a.enumerable)
                                {
                                    return Err(Thrown(format!(
                                        "TypeError: cannot declare global function {name}"
                                    )));
                                }
                            } else if self.global_by_name(&name).is_none()
                                && !matches!(
                                    self.heap.get(self.global_this),
                                    HeapObj::Object(m) if m.extensible
                                )
                            {
                                // CanDeclareGlobalFunction step 5: an ABSENT name
                                // is only definable while the global object is
                                // extensible.
                                return Err(Thrown(format!(
                                    "TypeError: cannot declare global function {name}"
                                )));
                            }
                        }
                    }
                }
            }
        }
        // SCRIPT GDI bookkeeping: record this script's bindings in the realm
        // registries — later scripts' collision checks, const enforcement
        // (StoreGlobal* throw on a write to an INITIALIZED const slot), and
        // lexical invisibility to global-object property reflection.
        if script_gdi && var_env_global {
            for &slot in &eval_prog.lexical_globals {
                self.eval_lexical_globals.insert(gmap[slot as usize]);
            }
            for &slot in &eval_prog.const_globals {
                self.eval_const_globals.insert(gmap[slot as usize]);
            }
            for &slot in &eval_prog.hoisted_globals {
                self.eval_var_globals.insert(gmap[slot as usize]);
            }
            for local in start..count {
                if let Some(slot) = self.eval_funcs[local].name_global {
                    self.eval_var_globals.insert(slot as u32);
                }
            }
        }
        // 5. CreateGlobalVarBinding for eval `var` names: an ABSENT binding
        // becomes an own {writable, enumerable, CONFIGURABLE} property of the
        // global object (eval-created bindings are deletable and reflectable;
        // a $262.evalScript's are NON-configurable — script
        // GlobalDeclarationInstantiation passes deletable=false);
        // the slot stays UNINITIALIZED so reads/writes route through the own
        // prop (the Load/StoreGlobal fallbacks). Existing bindings untouched.
        // FUNCTION-context dynamic names: CreateMutableBinding(undefined) in
        // the caller's EvalScope (functions get their values in step 6).
        if let Some(sc) = eval_scope_idx {
            let names: Vec<String> = eval_prog.eval_dynamic_names.clone();
            if let HeapObj::EvalScope(m) = self.heap.get_mut(sc) {
                for n in names {
                    m.entry(n).or_insert(Value::UNDEFINED);
                }
            }
        }
        for &slot in &eval_prog.hoisted_globals {
            let rs = gmap[slot as usize] as usize;
            if self.globals[rs].bits() == Value::UNINITIALIZED.bits() {
                let mut own_backed = false;
                if var_env_global {
                    if let Some(name) = self.global_slot_name(rs as u32) {
                        // A builtin binding of this name already exists — leave it.
                        if self.global_by_name(&name).is_some() {
                            continue;
                        }
                        if self.global_this != 0 {
                            let gi = self.global_this;
                            let has_own = matches!(
                                self.heap.get(gi),
                                HeapObj::Object(m) if m.pos(&name).is_some()
                            );
                            if has_own {
                                own_backed = true;
                            } else if let HeapObj::Object(m) = self.heap.get_mut(gi) {
                                m.define(
                                    &name,
                                    Value::UNDEFINED,
                                    crate::heap::PropAttr {
                                        writable: true,
                                        enumerable: true,
                                        configurable: !script_gdi,
                                        accessor: false,
                                        setter: Value::UNDEFINED,
                                    },
                                );
                                own_backed = true;
                            }
                        }
                    }
                }
                if !own_backed {
                    self.globals[rs] = Value::UNDEFINED;
                }
            }
        }
        // 6. CreateGlobalFunctionBinding for eval top-level function decls:
        // when the slot is uninitialized, the binding lives as a global-object
        // own property — absent: define {w, e, configurable: true}; existing
        // configurable: redefine to that shape with the new value; existing
        // non-configurable: write the value, keep the attributes. An
        // initialized slot (a main-program binding) is written directly.
        for local in start..count {
            let global_id = (self.main_func_count + local) as u32;
            if let Some(slot) = self.eval_funcs[local].name_global {
                let v = Value::heap(self.heap.alloc(HeapObj::Func(global_id)));
                // Hoisted here rather than by MakeFunc, so this is the only place a
                // `other.eval("function f(){}")` declaration can pick up the CHILD's
                // realm tag — without it `other.f()` binds the MAIN global as `this`.
                self.realm_tag_new(v.heap_index());
                if (slot as usize) >= self.globals.len() {
                    continue;
                }
                // A dynamic (EvalScope) function: bind in the caller's scope,
                // stamp the scope on the value so its body resolves siblings.
                if let Some(sc) = eval_scope_idx {
                    if let Some(name) = self.global_slot_name(slot as u32) {
                        if eval_prog.eval_dynamic_names.iter().any(|n| *n == name) {
                            self.closure_eval_scope.insert(v.heap_index(), sc);
                            if let HeapObj::EvalScope(m) = self.heap.get_mut(sc) {
                                m.insert(name, v);
                            }
                            continue;
                        }
                    }
                }
                if var_env_global
                    && self.globals[slot as usize].bits() == Value::UNINITIALIZED.bits()
                {
                    if let Some(name) = self.global_slot_name(slot as u32) {
                        if self.global_this != 0 {
                            let gi = self.global_this;
                            let attr = crate::heap::PropAttr {
                                writable: true,
                                enumerable: true,
                                configurable: !script_gdi,
                                accessor: false,
                                setter: Value::UNDEFINED,
                            };
                            if let HeapObj::Object(m) = self.heap.get_mut(gi) {
                                if let Some(i) = m.pos(&name) {
                                    if m.attrs[i].configurable {
                                        m.attrs[i] = attr;
                                    }
                                    m.vals[i] = v;
                                } else {
                                    m.define(&name, v, attr);
                                }
                                continue;
                            }
                        }
                    }
                }
                self.globals[slot as usize] = v;
            }
        }
        Ok((gmap, base_func))
    }

    /// Phase 6: run a prepared eval/module top-level function (`base_func`) to
    /// completion, returning its completion value. `this_override` is the caller's
    /// `this` for a DIRECT eval; otherwise the top level runs with `this` = globalThis.
    pub(crate) fn execute_eval_program(
        &mut self,
        base_func: u32,
        this_override: Option<Value>,
        caller_chain: Option<Vec<u64>>,
        caller_new_target: Value,
        caller_home_obj: Option<Value>,
        caller_cells: Option<Vec<Value>>,
        eval_scope_idx: Option<u32>,
    ) -> Result<Value, Thrown> {
        // With caller bindings, the eval script is a CLOSURE over their cells
        // (UpvalGet/UpvalSet in the eval code address them directly).
        let script = match caller_cells {
            Some(cells) => {
                let ups: Vec<u32> = cells.iter().map(|v| v.heap_index()).collect();
                Value::heap(self.heap.alloc(HeapObj::Closure {
                    func: base_func,
                    upvalues: ups,
                    this_val: Value::UNDEFINED,
                }))
            }
            None => Value::heap(self.heap.alloc(HeapObj::Func(base_func))),
        };
        // A direct eval's code resolves the CALLER's private brand chain
        // (frame.callee = this script value).
        if let Some(ch) = caller_chain {
            self.method_brand.insert(script.heap_index(), ch);
        }
        // Object-method direct eval: super.x resolves via the caller's
        // [[HomeObject]] (same stamp pattern as the brand chain above).
        if let Some(home) = caller_home_obj {
            self.closure_home.insert(script.heap_index(), home);
        }
        // The eval frame resolves the caller's dynamic EvalScope through
        // the same stamp the Dyn ops use for closures.
        if let Some(sc) = eval_scope_idx {
            self.closure_eval_scope.insert(script.heap_index(), sc);
        }
        // The eval frame's new.target is the CALLER's (consumed at frame setup).
        self.pending_new_target = caller_new_target;
        // Mark the frame about to be pushed as an EVAL frame — the legacy
        // `f.caller` walk steps past it so an eval is transparent to the caller
        // chain (function-caller-skips-eval-frames.js).
        self.pending_eval_frame = true;
        let this = this_override.unwrap_or_else(|| {
            if self.global_this != 0 {
                Value::heap(self.global_this)
            } else {
                Value::UNDEFINED
            }
        });
        self.call_value(script, this, &[])
    }

    /// Install a compiled eval/module program and run its top-level body, returning
    /// `(completion, gmap)`. (prepare + execute; modules that need namespace
    /// pre-registration call the two halves directly — see `import_module`.)
    /// `$262.evalScript`: parse + compile `code` as a SCRIPT and run it in
    /// the current realm. Every top-level declaration — var, function, AND
    /// `let`/`const`/`class` — binds a persistent realm global (the eval
    /// pipeline's name-mapped slots), matching script
    /// GlobalDeclarationInstantiation rather than eval semantics.
    pub(crate) fn eval_script(&mut self, code: &str) -> Result<Value, Thrown> {
        // SCRIPT goal: module mode would make the whole program strict and
        // silently disable Annex B.3.3 hoisting, sloppy semantics, and HTML
        // comments.
        let ast = crate::front::parse_script(code).map_err(Thrown)?;
        let prog = crate::compile::compile_program(&ast, code)
            .map_err(|e| Thrown(format!("SyntaxError: {e}")))?;
        // Dev aid (same flag as the main-program dump in lib.rs).
        if std::env::var_os("ZIPP_VM_DUMP").is_some() {
            eprintln!("── evalScript program (hoisted={:?}) ──", prog.hoisted_globals);
            for (fid, f) in prog.functions.iter().enumerate() {
                eprintln!("── eval fn {fid} (regs={}, params={}) ──", f.reg_count, f.param_count);
                for (ip, instr) in f.code.iter().enumerate() {
                    eprintln!("  {ip:4}  {instr:?}");
                }
            }
        }
        // Script GlobalDeclarationInstantiation (not eval semantics) for this
        // program: prepare_eval_program consumes the flag.
        self.eval_script_gdi = true;
        let (completion, _gmap) = self.run_eval_program(
            prog,
            None,
            false,
            None,
            None,
            Value::UNDEFINED,
            None,
            true,
            None,
            None,
        )?;
        Ok(completion)
    }

    pub(crate) fn run_eval_program(
        &mut self,
        eval_prog: crate::bytecode::Program,
        this_override: Option<Value>,
        module: bool,
        caller_home: Option<u32>,
        caller_chain: Option<Vec<u64>>,
        caller_new_target: Value,
        caller_home_obj: Option<Value>,
        var_env_global: bool,
        caller_cells: Option<Vec<Value>>,
        eval_scope_idx: Option<u32>,
    ) -> Result<(Value, Vec<u32>), Thrown> {
        let (gmap, base_func) =
            self.prepare_eval_program(
                eval_prog,
                module,
                caller_home,
                var_env_global,
                eval_scope_idx,
                None,
                None,
                None,
            )?;
        let completion = self.execute_eval_program(
            base_func,
            this_override,
            caller_chain,
            caller_new_target,
            caller_home_obj,
            caller_cells,
            eval_scope_idx,
        )?;
        Ok((completion, gmap))
    }
}
