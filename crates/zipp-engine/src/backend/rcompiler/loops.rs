//! Loop and iteration statement compilers — `while`, `do / while`,
//! `switch`, C-style `for`, `for … of`, `for … in`.
//!
//! Extracted from `rcompiler/mod.rs` in 0.4 to separate the relatively
//! self-contained loop lowering from the rest of the statement
//! compiler. Each function ends by patching back-edge jumps and
//! closing the current `LoopContext` on `self`.

use crate::ast::{Expression, ForBinding, Statement};
use crate::rcode::ROp;

use super::{scan_captured_names, LoopContext, RCompiler};

impl RCompiler {
    pub(super) fn compile_while_statement(
        &mut self,
        condition: &Expression,
        body: &[Statement],
        label: Option<&str>,
    ) -> Result<(), String> {
        let loop_start = self.instructions.len();
        let jump_pos = self.compile_loop_condition(condition)?;

        self.loop_stack.push(LoopContext {
            label: label.map(|s| s.to_string()),
            continue_target: loop_start,
            break_positions: vec![],
            continue_positions: vec![],
        });

        // Try to detect `x = x + CONST` or `x += CONST` as last body statement
        // and fuse into IncrementRegAndJump, saving one dispatch per iteration.
        let fused_increment = if let Some(Statement::Expression(expr)) = body.last() {
            self.try_fused_increment(expr)
                .filter(|(_, _, name)| !self.globals.contains_key(*name))
        } else {
            None
        };

        let body_to_compile = if fused_increment.is_some() {
            &body[..body.len() - 1]
        } else {
            body
        };

        let shadowed = self.enter_block_scope(body);
        for stmt in body_to_compile {
            self.compile_statement(stmt)?;
            self.next_temp = self.num_locals; // free temps each iteration
        }
        self.exit_block_scope(shadowed);

        if let Some((reg, const_idx, _)) = fused_increment {
            self.emit_jump(ROp::IncrementRegAndJump, &[reg, const_idx], loop_start);
        } else {
            self.emit_jump(ROp::Jump, &[], loop_start);
        }
        let loop_end = self.instructions.len();
        self.patch_jump(jump_pos, loop_end);

        self.patch_loop_exits(loop_end);
        Ok(())
    }

    pub(super) fn compile_do_while_statement(
        &mut self,
        body: &[Statement],
        condition: &Expression,
        label: Option<&str>,
    ) -> Result<(), String> {
        let loop_start = self.instructions.len();

        self.loop_stack.push(LoopContext {
            label: label.map(|s| s.to_string()),
            continue_target: loop_start,
            break_positions: vec![],
            continue_positions: vec![],
        });

        let shadowed = self.enter_block_scope(body);
        for stmt in body {
            self.compile_statement(stmt)?;
            self.next_temp = self.num_locals;
        }
        self.exit_block_scope(shadowed);

        // continue jumps to the condition
        let condition_start = self.instructions.len();
        if let Some(loop_ctx) = self.loop_stack.last_mut() {
            loop_ctx.continue_target = condition_start;
        }

        let cond_reg = self.compile_expression(condition)?;
        let exit_jump = self.emit(ROp::JumpIfNot, &[cond_reg, 9999]);
        self.emit_jump(ROp::Jump, &[], loop_start);
        let loop_end = self.instructions.len();
        self.patch_jump(exit_jump, loop_end);

        self.patch_loop_exits(loop_end);
        Ok(())
    }

    pub(super) fn compile_switch_statement(
        &mut self,
        discriminant: &Expression,
        cases: &[crate::ast::SwitchCase],
        label: Option<&str>,
    ) -> Result<(), String> {
        // Switch doesn't have its own continue target — inherit from
        // the enclosing loop so `continue` inside switch cases works correctly.
        let parent_continue = self.loop_stack.last().map_or(
            self.instructions.len(), |ctx| ctx.continue_target);
        self.loop_stack.push(LoopContext {
            label: label.map(|s| s.to_string()),
            continue_target: parent_continue,
            break_positions: vec![],
            continue_positions: vec![],
        });

        let disc_reg = self.compile_expression(discriminant)?;

        // Phase 1: Emit case comparisons and jumps
        let mut case_body_jumps: Vec<usize> = Vec::new();
        let mut default_body_idx: Option<usize> = None;

        let saved = self.save_temps();

        for (i, case) in cases.iter().enumerate() {
            if let Some(test) = &case.test {
                let test_reg = self.compile_expression(test)?;
                let cmp_reg = self.alloc_temp();
                self.emit(
                    ROp::StrictEqual,
                    &[cmp_reg, disc_reg, test_reg],
                );
                // Jump to body if equal
                let body_jump = self.emit(ROp::JumpIfTruthy, &[cmp_reg, 9999]);
                case_body_jumps.push(body_jump);
                self.restore_temps(saved);
            } else {
                default_body_idx = Some(i);
            }
        }

        // Jump to default or end
        let default_or_end_jump = self.emit(ROp::Jump, &[9999]);

        // Phase 2: Emit bodies with fall-through
        let mut body_starts: Vec<usize> = Vec::new();
        for case in cases {
            body_starts.push(self.instructions.len());
            for stmt in &case.consequent {
                self.compile_statement(stmt)?;
                self.next_temp = self.num_locals;
            }
        }
        let switch_end = self.instructions.len();

        // Phase 3: Patch
        let mut case_jump_idx = 0;
        for (i, case) in cases.iter().enumerate() {
            if case.test.is_some() {
                self.patch_jump(case_body_jumps[case_jump_idx], body_starts[i]);
                case_jump_idx += 1;
            }
        }

        if let Some(def_idx) = default_body_idx {
            self.patch_jump(default_or_end_jump, body_starts[def_idx]);
        } else {
            self.patch_jump(default_or_end_jump, switch_end);
        }

        self.patch_loop_exits(switch_end);

        Ok(())
    }

    pub(super) fn compile_for_statement(
        &mut self,
        init: Option<&Statement>,
        condition: Option<&Expression>,
        update: Option<&Expression>,
        body: &[Statement],
        label: Option<&str>,
    ) -> Result<(), String> {
        // Detect let-declared loop variables captured by inner function literals.
        // These need per-iteration snapshotting via MakeClosure.
        let mut loop_capture_slots: Vec<u16> = vec![];
        if let Some(Statement::Let {
            name,
            kind: crate::ast::VariableKind::Let,
            ..
        }) = init
        {
            let body_captures = scan_captured_names(body);
            if body_captures.contains(name) {
                // Ensure the loop variable has a global slot
                let g = self.ensure_global_slot(name)?;
                if !self.param_shadow_slots.contains(&g) {
                    self.param_shadow_slots.insert(g);
                    loop_capture_slots.push(g);
                }
            }
        }

        if let Some(init_stmt) = init {
            self.compile_statement(init_stmt)?;
        }

        let loop_start = self.instructions.len();

        let jump_pos = if let Some(cond) = condition {
            self.compile_loop_condition(cond)?
        } else {
            // No condition = infinite loop (like `for(;;)`)
            let r = self.alloc_temp();
            self.emit(ROp::LoadTrue, &[r]);
            self.emit(ROp::JumpIfNot, &[r, 9999])
        };

        self.loop_stack.push(LoopContext {
            label: label.map(|s| s.to_string()),
            continue_target: loop_start,
            break_positions: vec![],
            continue_positions: vec![],
        });

        let shadowed = self.enter_block_scope(body);
        for stmt in body {
            self.compile_statement(stmt)?;
            self.next_temp = self.num_locals;
        }
        self.exit_block_scope(shadowed);

        let update_start = self.instructions.len();
        if let Some(loop_ctx) = self.loop_stack.last_mut() {
            loop_ctx.continue_target = update_start;
        }

        // Try fused update+jump: `local += CONST; jump loop_start` → IncrementRegAndJump
        let used_fused_update = if let Some(upd) = update {
            if let Some((reg, const_idx, name)) = self.try_fused_increment(upd) {
                if self.globals.contains_key(name) {
                    // Variable has a global slot — can't use fully-fused IncrementRegAndJump
                    // because reload_locals_from_globals would reset it from the stale global.
                    // Use AddRegConst + SetGlobal + Jump instead.
                    self.emit(ROp::AddRegConst, &[reg, reg, const_idx]);
                    let name_owned = name.to_string();
                    self.mirror_local_to_global(&name_owned, reg);
                    false // fall through to emit Jump below
                } else {
                    self.emit_jump(ROp::IncrementRegAndJump, &[reg, const_idx], loop_start);
                    true
                }
            } else {
                let _ = self.compile_expression(upd)?;
                self.next_temp = self.num_locals;
                false
            }
        } else {
            false
        };

        if !used_fused_update {
            self.emit_jump(ROp::Jump, &[], loop_start);
        }
        let loop_end = self.instructions.len();
        self.patch_jump(jump_pos, loop_end);

        self.patch_loop_exits(loop_end);

        // Clean up: remove loop-variable capture slots so they don't affect
        // function literals outside this for-loop.
        for slot in &loop_capture_slots {
            self.param_shadow_slots.remove(slot);
        }

        Ok(())
    }

    pub(super) fn compile_for_of_statement(
        &mut self,
        binding: &ForBinding,
        iterable: &Expression,
        body: &[Statement],
        label: Option<&str>,
    ) -> Result<(), String> {
        let iter_name = self.make_temp_name("iter");
        let idx_name = self.make_temp_name("i");

        // iter = Array.from(iterable) — ensures Set, Map, String all become arrays
        let array_from_expr = Expression::Call {
            function: Box::new(Expression::Index {
                left: Box::new(Expression::Identifier("Array".to_string())),
                index: Box::new(Expression::String("from".to_string())),
            }),
            arguments: vec![iterable.clone()],
        };
        let iter_val = self.compile_expression(&array_from_expr)?;
        self.store_identifier(&iter_name, iter_val)?;

        // i = 0
        let idx_r = self.ensure_binding_register(&idx_name)?;
        let zero_idx = self.add_constant_int(0);
        self.emit(ROp::LoadConst, &[idx_r, zero_idx]);
        self.write_binding(&idx_name, idx_r)?;

        let loop_start = self.instructions.len();

        // condition: i < iter.length
        let cond_expr = Expression::Infix {
            left: Box::new(Expression::Identifier(idx_name.clone())),
            operator: "<".to_string(),
            right: Box::new(Expression::Index {
                left: Box::new(Expression::Identifier(iter_name.clone())),
                index: Box::new(Expression::String("length".to_string())),
            }),
        };
        let cond = self.compile_expression(&cond_expr)?;
        let jump_pos = self.emit(ROp::JumpIfNot, &[cond, 9999]);

        self.loop_stack.push(LoopContext {
            label: label.map(|s| s.to_string()),
            continue_target: loop_start,
            break_positions: vec![],
            continue_positions: vec![],
        });

        // item = iter[i]
        let item_expr = Expression::Index {
            left: Box::new(Expression::Identifier(iter_name.clone())),
            index: Box::new(Expression::Identifier(idx_name.clone())),
        };
        let item = self.compile_expression(&item_expr)?;

        match binding {
            ForBinding::Identifier(var_name) => {
                self.store_identifier(var_name, item)?;
            }
            ForBinding::Pattern(pattern) => {
                self.assign_pattern(pattern, item)?;
            }
        }

        let shadowed = self.enter_block_scope(body);
        for stmt in body {
            self.compile_statement(stmt)?;
        }
        self.exit_block_scope(shadowed);

        let update_start = self.instructions.len();
        if let Some(loop_ctx) = self.loop_stack.last_mut() {
            loop_ctx.continue_target = update_start;
        }

        // i += 1
        let update_expr = Expression::Assign {
            left: Box::new(Expression::Identifier(idx_name.clone())),
            operator: "+=".to_string(),
            right: Box::new(Expression::Integer(1)),
        };
        let _ = self.compile_expression(&update_expr)?;

        self.emit_jump(ROp::Jump, &[], loop_start);
        let loop_end = self.instructions.len();
        self.patch_jump(jump_pos, loop_end);

        self.patch_loop_exits(loop_end);
        Ok(())
    }

    pub(super) fn compile_for_in_statement(
        &mut self,
        var_name: &str,
        iterable: &Expression,
        body: &[Statement],
        label: Option<&str>,
    ) -> Result<(), String> {
        let keys_name = self.make_temp_name("keys");
        let idx_name = self.make_temp_name("ki");

        // keys = Object.keys(iterable)
        let iter_r = self.compile_expression(iterable)?;
        let keys_r = self.alloc_temp();
        self.emit(ROp::GetKeysIter, &[keys_r, iter_r]);
        self.store_identifier(&keys_name, keys_r)?;

        // i = 0
        let idx_r = self.ensure_binding_register(&idx_name)?;
        let zero_idx = self.add_constant_int(0);
        self.emit(ROp::LoadConst, &[idx_r, zero_idx]);
        self.write_binding(&idx_name, idx_r)?;

        let loop_start = self.instructions.len();

        let cond_expr = Expression::Infix {
            left: Box::new(Expression::Identifier(idx_name.clone())),
            operator: "<".to_string(),
            right: Box::new(Expression::Index {
                left: Box::new(Expression::Identifier(keys_name.clone())),
                index: Box::new(Expression::String("length".to_string())),
            }),
        };
        let cond = self.compile_expression(&cond_expr)?;
        let jump_pos = self.emit(ROp::JumpIfNot, &[cond, 9999]);

        self.loop_stack.push(LoopContext {
            label: label.map(|s| s.to_string()),
            continue_target: loop_start,
            break_positions: vec![],
            continue_positions: vec![],
        });

        let key_expr = Expression::Index {
            left: Box::new(Expression::Identifier(keys_name.clone())),
            index: Box::new(Expression::Identifier(idx_name.clone())),
        };
        let key = self.compile_expression(&key_expr)?;
        self.store_identifier(var_name, key)?;

        let shadowed = self.enter_block_scope(body);
        for stmt in body {
            self.compile_statement(stmt)?;
        }
        self.exit_block_scope(shadowed);

        let update_start = self.instructions.len();
        if let Some(loop_ctx) = self.loop_stack.last_mut() {
            loop_ctx.continue_target = update_start;
        }

        let update_expr = Expression::Assign {
            left: Box::new(Expression::Identifier(idx_name.clone())),
            operator: "+=".to_string(),
            right: Box::new(Expression::Integer(1)),
        };
        let _ = self.compile_expression(&update_expr)?;

        self.emit_jump(ROp::Jump, &[], loop_start);
        let loop_end = self.instructions.len();
        self.patch_jump(jump_pos, loop_end);

        self.patch_loop_exits(loop_end);
        Ok(())
    }

    pub(super) fn compile_loop_condition(&mut self, cond: &Expression) -> Result<usize, String> {
        // Try fused: local < CONST or local <= CONST
        if let Some((reg, const_idx, is_le)) = self.try_fused_cmp_const(cond) {
            let op = if is_le {
                ROp::TestLeConstJump
            } else {
                ROp::TestLtConstJump
            };
            return Ok(self.emit(op, &[reg, const_idx, 9999]));
        }
        // Try fused: local < local or local <= local (register-vs-register)
        if let Some((lr, rr, is_le)) = self.try_fused_cmp_reg(cond) {
            let op = if is_le {
                ROp::TestLeRegJump
            } else {
                ROp::TestLtRegJump
            };
            return Ok(self.emit(op, &[lr, rr, 9999]));
        }
        // Try fused: (local % CONST) === CONST
        if let Some((reg, mod_const, cmp_const)) = self.try_fused_mod_strict_eq(cond) {
            return Ok(self.emit(
                ROp::ModRegConstStrictEqConstJump,
                &[reg, mod_const, cmp_const, 9999],
            ));
        }
        // Fallback: compile condition normally + JumpIfNot
        let cond_r = self.compile_expression(cond)?;
        Ok(self.emit(ROp::JumpIfNot, &[cond_r, 9999]))
    }
}
