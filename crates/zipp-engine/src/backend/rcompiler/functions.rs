//! Function and class compilation.
//!
//! Extracted from `rcompiler/mod.rs` in 0.4 so the orchestration core of
//! the compiler doesn't have to sit alongside 500 lines of function-body
//! compilation. Everything here attaches back to `RCompiler` via
//! `impl super::RCompiler`; no behavioural change from the move.

use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::{ClassMember, Expression, Statement, VariableKind};
use crate::object::{ClassObject, CompiledFunctionObject, StaticInitializer, VmCell};
use crate::rcode::ROp;

use super::{scan_captured_names, RCompiler};

impl RCompiler {
    pub(super) fn compile_function_literal(
        &mut self,
        parameters: &[String],
        body: &[Statement],
        takes_this: bool,
        is_async: bool,
        is_generator: bool,
    ) -> Result<CompiledFunctionObject, String> {
        let mut normalized_params: Vec<String> = vec![];
        let mut rest_parameter_index: Option<usize> = None;
        for (i, p) in parameters.iter().enumerate() {
            if let Some(rest_name) = p.strip_prefix("...") {
                if rest_name.is_empty() {
                    return Err("invalid rest parameter name".to_string());
                }
                if i + 1 != parameters.len() {
                    return Err("rest parameter must be last".to_string());
                }
                rest_parameter_index = Some(i);
                normalized_params.push(rest_name.to_string());
            } else {
                if rest_parameter_index.is_some() {
                    return Err("rest parameter must be last".to_string());
                }
                normalized_params.push(p.clone());
            }
        }

        let mut effective_params = vec![];
        if takes_this {
            effective_params.push("this".to_string());
        }
        effective_params.extend(normalized_params.iter().cloned());
        // Scan the function body for names referenced by nested function
        // literals. Only those names need global mirrors (for closure capture).
        // All other locals remain purely in registers, which is correct for
        // recursion: each activation gets its own register frame.
        let captured = scan_captured_names(body);

        // Snapshot parent globals BEFORE child compilation. Any new globals
        // created by the child should NOT leak back to the parent.
        let parent_globals_snapshot: FxHashSet<String> =
            self.globals.keys().cloned().collect();
        let mut fn_compiler = RCompiler::new_function_scope(
            self.globals.clone(),
            self.next_global,
            &effective_params,
            captured,
        );

        // Remove inherited global slots for parameters that shadow parent names.
        // Without this, the parameter mirror (SetGlobal) writes to the PARENT's
        // global slot, corrupting it. After the function returns, the parent's
        // reload_locals_from_globals would see the parameter's value.
        {
            let captured_set = fn_compiler.captured_names.clone();
            for param_name in &effective_params {
                if fn_compiler.globals.contains_key(param_name) {
                    fn_compiler.globals.remove(param_name);
                    // Re-allocate a fresh slot if inner functions capture this param
                    if let Some(ref cap) = captured_set {
                        if cap.contains(param_name) {
                            let _ = fn_compiler.ensure_global_slot(param_name);
                        }
                    }
                }
            }
        }

        // Mirror captured parameters to global slots so inner functions can
        // read them via GetGlobal.  Without this, parameters stay in registers
        // only and nested closures see uninitialised globals.
        {
            let params_to_mirror: Vec<(u16, String)> = effective_params
                .iter()
                .enumerate()
                .filter(|(_, p)| fn_compiler.needs_global(p))
                .map(|(i, p)| (i as u16, p.clone()))
                .collect();
            for (reg, name) in params_to_mirror {
                let is_new = !fn_compiler.globals.contains_key(&name);
                let g = fn_compiler.ensure_global_slot(&name)?;
                fn_compiler.emit(ROp::SetGlobal, &[g, reg]);
                if is_new {
                    fn_compiler.param_shadow_slots.insert(g);
                }
            }
        }

        fn_compiler.hoist_var_declarations(body)?;

        // Remove inherited globals for local declarations that shadow
        // parent-scope names. Two reasons:
        // 1. Captured names need fresh global slots (for closure capture)
        // 2. ALL locals must not be reloaded from parent globals by
        //    reload_locals_from_globals — that would overwrite the local
        //    value with the parent's value after any function call.
        // Parameters are already handled by new_function_scope.
        {
            let mut local_decl_names: Vec<String> = Vec::new();
            Self::collect_var_names(body, &mut local_decl_names);
            for stmt in body {
                match stmt {
                    Statement::FunctionDecl { name, .. } => {
                        local_decl_names.push(name.clone());
                    }
                    Statement::Let { name, kind, .. }
                        if *kind != VariableKind::Var =>
                    {
                        local_decl_names.push(name.clone());
                    }
                    Statement::LetPattern { kind, pattern, .. }
                        if *kind != VariableKind::Var =>
                    {
                        let mut names = FxHashSet::default();
                        Self::collect_pattern_names(pattern, &mut names);
                        local_decl_names.extend(names);
                    }
                    Statement::MultiLet(stmts) => {
                        for s in stmts {
                            match s {
                                Statement::Let { name, kind, .. }
                                    if *kind != VariableKind::Var =>
                                {
                                    local_decl_names.push(name.clone());
                                }
                                Statement::LetPattern { kind, pattern, .. }
                                    if *kind != VariableKind::Var =>
                                {
                                    let mut names = FxHashSet::default();
                                    Self::collect_pattern_names(pattern, &mut names);
                                    local_decl_names.extend(names);
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            // NOTE: for-init let/const names are NOT collected here because
            // removing their inherited globals can break other code paths.
            // Instead, the bundle patches in js.rs convert for(let) to for(var)
            // in the critical matchRoutes/matchRouteBranch functions.
            let param_set: FxHashSet<&str> =
                effective_params.iter().map(|s| s.as_str()).collect();
            let captured_set = fn_compiler.captured_names.clone();
            let mut names_to_realloc: Vec<String> = Vec::new();
            for name in &local_decl_names {
                if !param_set.contains(name.as_str())
                    && fn_compiler.globals.contains_key(name)
                {
                    fn_compiler.globals.remove(name);
                    if let Some(ref cap) = captured_set {
                        if cap.contains(name) {
                            names_to_realloc.push(name.clone());
                        }
                    }
                }
            }
            for name in &names_to_realloc {
                let _ = fn_compiler.ensure_global_slot(name);
            }
        }

        // Check if the function body uses `arguments`. If so, emit MakeArguments
        // at the function prologue to create the arguments array-like object.
        // Only for non-arrow functions (arrows don't have arguments).
        if takes_this {
            // Check if "arguments" is used anywhere in the body
            // Use the captured_names scan which finds all identifiers in function bodies
            let uses_arguments = {
                let src = format!("{:?}", body);
                src.contains("\"arguments\"")
            };
            if uses_arguments {
                // Remove any inherited "arguments" global slot from the parent.
                // Without this, mirror_local_to_global reuses the PARENT's slot,
                // and every function's arguments object overwrites the same slot.
                // Each function needs its own fresh global slot for arguments.
                fn_compiler.globals.remove("arguments");
                // Declare "arguments" as a local variable
                let args_reg = fn_compiler.ensure_local("arguments");
                // Arguments start after 'this' (if present)
                let arg_start = if takes_this { 1u16 } else { 0u16 };
                let num_formal = normalized_params.len() as u16;
                fn_compiler.emit(ROp::MakeArguments, &[args_reg, arg_start, num_formal]);
                fn_compiler.mirror_local_to_global("arguments", args_reg);
            }
        }

        // Hoist function declarations to top of function body (JS semantics)
        for stmt in body.iter() {
            if matches!(stmt, Statement::FunctionDecl { .. }) {
                let saved_len = fn_compiler.instructions.len();
                let saved_consts = fn_compiler.constants.len();
                match fn_compiler.compile_statement(stmt) {
                    Ok(_) => {}
                    Err(_e) => {
                        fn_compiler.instructions.truncate(saved_len);
                        fn_compiler.constants.truncate(saved_consts);
                    }
                }
                fn_compiler.next_temp = fn_compiler.num_locals;
            }
        }

        let mut _last_reg = None;
        for stmt in body {
            if matches!(stmt, Statement::FunctionDecl { .. }) {
                continue; // already compiled above
            }
            let saved_len = fn_compiler.instructions.len();
            let saved_consts = fn_compiler.constants.len();
            match fn_compiler.compile_statement(stmt) {
                Ok(reg) => _last_reg = reg,
                Err(_e) => {
                    // Roll back any partially-emitted bytecode from the failed statement
                    fn_compiler.instructions.truncate(saved_len);
                    fn_compiler.constants.truncate(saved_consts);
                }
            }
            fn_compiler.next_temp = fn_compiler.num_locals;
        }

        // Only arrow functions with expression bodies implicitly return.
        // Regular functions return undefined unless they have an explicit return.
        // Arrow expression bodies are parsed as: body = [Return { value: expr }]
        // so the Return opcode is already emitted by compile_statement.

        // Ensure function ends with return
        let last_byte = fn_compiler.instructions.last().copied();
        if last_byte != Some(ROp::Return as u8) && last_byte != Some(ROp::ReturnUndef as u8) {
            fn_compiler.emit(ROp::ReturnUndef, &[]);
        }
        fn_compiler.emit(ROp::Halt, &[]);

        // Validate backwards Jump targets in large functions
        if fn_compiler.instructions.len() > 50_000 {
            let insts = &fn_compiler.instructions;
            // Build set of valid instruction boundaries
            let mut boundaries = std::collections::HashSet::new();
            let mut vip = 0usize;
            while vip < insts.len() {
                boundaries.insert(vip);
                let byte = insts[vip];
                if let Some(op) = ROp::from_byte(byte) {
                    let mut sz = op.size();
                    if op == ROp::MakeClosure && vip + 5 < insts.len() {
                        sz = 6 + insts[vip + 5] as usize * 2;
                    }
                    vip += sz;
                } else {
                    eprintln!("[VERIFY] Bad byte 0x{:02x} at ip={}", byte, vip);
                    break;
                }
            }
            // Count MakeClosures
            let mut mc_count = 0;
            let mut mc_total_extra = 0usize;
            {
                let mut mp = 0usize;
                while mp < insts.len() {
                    let byte = insts[mp];
                    if let Some(op) = ROp::from_byte(byte) {
                        let mut sz = op.size();
                        if op == ROp::MakeClosure && mp + 5 < insts.len() {
                            let count = insts[mp + 5] as usize;
                            sz = 6 + count * 2;
                            mc_count += 1;
                            mc_total_extra += count * 2;
                        }
                        mp += sz;
                    } else { break; }
                }
            }
            if mc_count > 0 {
                eprintln!("[VERIFY] {} MakeClosures, total extra bytes: {}", mc_count, mc_total_extra);
            }
            // Check all Jump/JumpIfNot/JumpIfTruthy targets
            vip = 0;
            while vip < insts.len() {
                let byte = insts[vip];
                if let Some(op) = ROp::from_byte(byte) {
                    let mut sz = op.size();
                    if op == ROp::MakeClosure && vip + 5 < insts.len() {
                        sz = 6 + insts[vip + 5] as usize * 2;
                    }
                    // Check jump targets
                    let target_offset = match op {
                        ROp::Jump => Some(1),
                        ROp::JumpIfNot | ROp::JumpIfTruthy => Some(3),
                        ROp::TestLtConstJump | ROp::TestLeConstJump
                        | ROp::IncrementRegAndJump
                        | ROp::TestLtRegJump | ROp::TestLeRegJump => Some(5),
                        ROp::ModRegConstStrictEqConstJump
                        | ROp::TestModRegStrictEqConstJump => Some(7),
                        _ => None,
                    };
                    if let Some(toff) = target_offset {
                        // Jump targets are u32 big-endian
                        let target = u32::from_be_bytes([
                            insts[vip + toff], insts[vip + toff + 1],
                            insts[vip + toff + 2], insts[vip + toff + 3],
                        ]) as usize;
                        if target < insts.len() && !boundaries.contains(&target) {
                            eprintln!("[VERIFY] Jump at ip={} targets ip={} which is NOT an instruction boundary! (byte=0x{:02x})",
                                vip, target, insts[target]);
                        }
                    }
                    vip += sz;
                } else {
                    break;
                }
            }
        }

        if fn_compiler.next_global > self.next_global {
            self.next_global = fn_compiler.next_global;
        }

        // Bubble up the inner compiler's overflow flag — without this
        // a runaway counter inside a nested function body would never
        // surface to the top-level `compile_program*` Result and the
        // outer compilation would emit aliased indices.
        if let Some(msg) = fn_compiler.overflow_error.take() {
            self.record_overflow(&msg);
        }

        // Determine which global slots from the parent's param_shadow_slots are
        // referenced by this function (or its nested closures). These need to be
        // captured at closure creation time via MakeClosure.
        let closure_captures: Vec<u16> = if !self.param_shadow_slots.is_empty() {
            fn_compiler
                .globals
                .values()
                .copied()
                .filter(|slot| self.param_shadow_slots.contains(slot))
                .collect()
        } else {
            vec![]
        };

        // Merge child globals back to parent, but SKIP names that are:
        // 1. New to the child (not in parent snapshot) AND
        // 2. Local declarations of the child (var/let/function in body)
        // This prevents variable collisions in webpack-concatenated modules
        // while still allowing forward references and shared globals.
        let child_local_names: FxHashSet<String> = {
            let mut names = FxHashSet::default();
            // Collect var declarations (hoisted to function scope)
            let mut var_names = Vec::new();
            Self::collect_var_names(body, &mut var_names);
            names.extend(var_names);
            // Collect function declarations and top-level let/const
            // that should NOT leak to parent scope
            for stmt in body {
                match stmt {
                    Statement::FunctionDecl { name, .. } => { names.insert(name.clone()); }
                    Statement::Let { name, kind, .. } if *kind != VariableKind::Var => {
                        names.insert(name.clone());
                    }
                    Statement::LetPattern { kind, pattern, .. } if *kind != VariableKind::Var => {
                        Self::collect_pattern_names(pattern, &mut names);
                    }
                    _ => {}
                }
            }
            names.extend(effective_params.iter().cloned());
            names
        };
        for (name, idx) in &fn_compiler.globals {
            if !parent_globals_snapshot.contains(name) && child_local_names.contains(name) {
                // Child-local new global — keep it private
                continue;
            }
            self.globals.entry(name.clone()).or_insert(*idx);
        }

        Ok(CompiledFunctionObject {
            instructions: Rc::new(fn_compiler.instructions),
            constants: Rc::new(fn_compiler.constants),
            num_locals: fn_compiler.num_locals as usize,
            num_parameters: rest_parameter_index.unwrap_or(normalized_params.len()),
            rest_parameter_index,
            takes_this,
            is_async,
            is_generator,
            num_cache_slots: fn_compiler.next_cache_slot,
            max_stack_depth: 0,
            register_count: fn_compiler.max_reg + 1,
            inline_cache: Rc::new(VmCell::new(vec![
                (0, 0);
                fn_compiler.next_cache_slot as usize
            ])),
            closure_captures,
            captured_values: vec![],
            properties: None,
        })
    }

    pub(super) fn compile_class_literal(
        &mut self,
        name: &str,
        extends: Option<&str>,
        members: &[ClassMember],
    ) -> Result<ClassObject, String> {
        let mut class_obj = ClassObject {
            name: name.to_string(),
            parent_chain: vec![],
            constructor: None,
            methods: FxHashMap::default(),
            static_methods: FxHashMap::default(),
            getters: FxHashMap::default(),
            setters: FxHashMap::default(),
            super_methods: FxHashMap::default(),
            super_getters: FxHashMap::default(),
            super_setters: FxHashMap::default(),
            super_constructor_chain: vec![],
            field_initializers: vec![],
            static_initializers: vec![],
            static_fields: FxHashMap::default(),
        };

        if let Some(parent_name) = extends {
            if let Some(parent) = self.class_defs.get(parent_name) {
                // Cap inheritance depth. Each child clones the
                // parent's parent_chain, super_methods,
                // super_constructor_chain etc. — the work is
                // O(N²) in chain depth. At compile time, before
                // `VM::run`, so no runtime limit catches it.
                // 64 is deeper than any real-world hierarchy and
                // keeps the O(N²) clone cost under a MiB total.
                const MAX_INHERITANCE_DEPTH: usize = 64;
                if parent.parent_chain.len() + 1 > MAX_INHERITANCE_DEPTH {
                    return Err(format!(
                        "class {}: inheritance depth exceeds {} levels",
                        name, MAX_INHERITANCE_DEPTH
                    ));
                }
                class_obj.parent_chain.push(parent_name.to_string());
                class_obj.parent_chain.extend(parent.parent_chain.clone());
                class_obj.super_methods = parent.methods.clone();
                if let Some(parent_ctor) = parent.constructor.clone() {
                    class_obj
                        .super_methods
                        .insert("constructor".to_string(), parent_ctor);
                }
                class_obj.super_getters = parent.getters.clone();
                class_obj.super_setters = parent.setters.clone();
                // Build the constructor chain for multi-level super() calls.
                // Chain[0] = grandparent (parent's super), chain[1] = great-grandparent, etc.
                if !parent.super_methods.is_empty() {
                    class_obj.super_constructor_chain.push((
                        parent.super_methods.clone(),
                        parent.super_getters.clone(),
                        parent.super_setters.clone(),
                    ));
                    class_obj
                        .super_constructor_chain
                        .extend(parent.super_constructor_chain.clone());
                }
                for (k, v) in &parent.methods {
                    class_obj.methods.entry(k.clone()).or_insert(v.clone());
                }
                for (k, v) in &parent.getters {
                    class_obj.getters.entry(k.clone()).or_insert(v.clone());
                }
                for (k, v) in &parent.setters {
                    class_obj.setters.entry(k.clone()).or_insert(v.clone());
                }
                // Inherit parent instance field initializers
                class_obj
                    .field_initializers
                    .extend(parent.field_initializers.clone());
            }
        }

        for member in members {
            match member {
                ClassMember::Method(method) => {
                    let compiled = self.compile_function_literal(
                        &method.parameters,
                        &method.body,
                        true,
                        false,
                        false,
                    )?;
                    if method.name == "constructor" {
                        class_obj.constructor = Some(compiled);
                    } else if method.is_static {
                        class_obj
                            .static_methods
                            .insert(method.name.clone(), compiled);
                    } else if method.is_getter {
                        class_obj.getters.insert(method.name.clone(), compiled);
                    } else if method.is_setter {
                        class_obj.setters.insert(method.name.clone(), compiled);
                    } else {
                        class_obj.methods.insert(method.name.clone(), compiled);
                    }
                }
                ClassMember::Field {
                    name: field_name,
                    initializer,
                    is_static,
                } => {
                    let init_body = if let Some(expr) = initializer {
                        vec![Statement::Return {
                            value: expr.clone(),
                        }]
                    } else {
                        vec![Statement::Return {
                            value: Expression::Identifier("undefined".to_string()),
                        }]
                    };
                    let compiled =
                        self.compile_function_literal(&[], &init_body, true, false, false)?;
                    if *is_static {
                        class_obj
                            .static_initializers
                            .push(StaticInitializer::Field {
                                name: field_name.clone(),
                                thunk: compiled,
                            });
                    } else {
                        class_obj
                            .field_initializers
                            .push((field_name.clone(), compiled));
                    }
                }
                ClassMember::StaticBlock { body } => {
                    let compiled = self.compile_function_literal(&[], body, true, false, false)?;
                    class_obj
                        .static_initializers
                        .push(StaticInitializer::Block { thunk: compiled });
                }
            }
        }

        Ok(class_obj)
    }
}
