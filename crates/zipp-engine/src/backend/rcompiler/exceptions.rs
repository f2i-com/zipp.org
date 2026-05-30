//! Exception-handling compilers — `throw` and `try / catch / finally`.
//!
//! The VM side of this is in `vm/rvm.rs` (the `ROp::Throw`,
//! `ROp::EnterTry`, `ROp::LeaveTry` arms) and in `try_catch_error` /
//! `unwind_rframes` which handle cross-frame unwinding. This file is
//! just the compiler that emits those opcodes.

use crate::ast::{Expression, Statement};
use crate::rcode::ROp;

use super::{RCompiler, TryContext};

impl RCompiler {
    pub(super) fn compile_throw_statement(&mut self, value: &Expression) -> Result<(), String> {
        if self.try_stack.is_empty() {
            let r = self.compile_expression(value)?;
            self.emit(ROp::Throw, &[r]);
            return Ok(());
        }

        let r = self.compile_expression(value)?;
        let exception_temp = self
            .try_stack
            .last()
            .map(|ctx| ctx.exception_temp.clone())
            .unwrap_or_else(|| self.make_temp_name("exc_fallback"));
        self.store_identifier(&exception_temp, r)?;

        let jump_pos = self.emit(ROp::Jump, &[9999]);
        if let Some(ctx) = self.try_stack.last_mut() {
            ctx.throw_jumps.push(jump_pos);
        }
        Ok(())
    }

    pub(super) fn compile_try_statement(
        &mut self,
        try_block: &[Statement],
        catch_param: Option<&str>,
        catch_block: Option<&[Statement]>,
        finally_block: Option<&[Statement]>,
    ) -> Result<Option<u16>, String> {
        let exception_temp = self.make_temp_name("exc");
        // Initialize exception temp to null
        let null_r = self.alloc_temp();
        self.emit(ROp::LoadNull, &[null_r]);
        self.store_identifier(&exception_temp, null_r)?;

        let has_finally = finally_block.is_some();
        let (return_temp, return_flag_temp) = if has_finally {
            let rt = self.make_temp_name("ret_val");
            let rf = self.make_temp_name("ret_flag");
            // Initialize return flag to false
            let false_r = self.alloc_temp();
            self.emit(ROp::LoadFalse, &[false_r]);
            self.store_identifier(&rf, false_r)?;
            (Some(rt), Some(rf))
        } else {
            (None, None)
        };

        self.try_stack.push(TryContext {
            exception_temp: exception_temp.clone(),
            throw_jumps: vec![],
            has_finally,
            return_temp: return_temp.clone(),
            return_flag_temp: return_flag_temp.clone(),
            return_jumps: vec![],
        });

        // Emit EnterTry: use the LOCAL register for exception_temp directly.
        // compile_expression would allocate a temp copy, but we need the runtime
        // to store the exception in the actual local register so the catch block
        // can read it.
        let exc_local_reg = *self.locals.get(&exception_temp)
            .ok_or_else(|| "exception temp not in locals".to_string())?;
        let enter_try_pos = self.emit(ROp::EnterTry, &[9999, exc_local_reg]);

        let mut last_reg: Option<u16> = None;
        for stmt in try_block {
            if let Some(r) = self.compile_statement(stmt)? {
                last_reg = Some(r);
            }
        }
        self.emit(ROp::LeaveTry, &[]);
        let jump_after_try = self.emit(ROp::Jump, &[9999]);

        let ctx = self
            .try_stack
            .pop()
            .ok_or_else(|| "internal error: missing try context".to_string())?;

        let catch_start = self.instructions.len();
        for pos in ctx.throw_jumps {
            self.patch_jump(pos, catch_start);
        }
        // Patch EnterTry's catch target to point to catch block
        self.patch_jump(enter_try_pos, catch_start);

        // Keep return_jumps from the try block
        let mut all_return_jumps = ctx.return_jumps;

        if let Some(catch_stmts) = catch_block {
            // The runtime catch handler stores the exception value in the
            // register for exception_temp. But if exception_temp has a global
            // slot, reads go through GetGlobal. Mirror the register to the
            // global slot so the exception value is accessible.
            {
                if let Some(&r) = self.locals.get(&exception_temp) {
                    if let Some(&g) = self.globals.get(&exception_temp) {
                        self.emit(ROp::SetGlobal, &[g, r]);
                    }
                }
            }
            // Push a context for the catch block so returns inside catch are
            // also deferred to the finally block.
            if has_finally {
                self.try_stack.push(TryContext {
                    exception_temp: exception_temp.clone(),
                    throw_jumps: vec![],
                    has_finally: true,
                    return_temp: return_temp.clone(),
                    return_flag_temp: return_flag_temp.clone(),
                    return_jumps: vec![],
                });
            }
            if let Some(param) = catch_param {
                let exc =
                    self.compile_expression(&Expression::Identifier(exception_temp.clone()))?;
                self.store_identifier(param, exc)?;
            }
            for stmt in catch_stmts {
                if let Some(r) = self.compile_statement(stmt)? {
                    last_reg = Some(r);
                }
            }
            // Pop the catch context and collect return jumps
            if has_finally {
                if let Some(catch_ctx) = self.try_stack.pop() {
                    all_return_jumps.extend(catch_ctx.return_jumps);
                }
            }
        }

        let finally_start = self.instructions.len();
        if let Some(finally_stmts) = finally_block {
            for stmt in finally_stmts {
                self.compile_statement(stmt)?;
            }
            // After finally block: check if a deferred return is pending
            if let (Some(ref rf), Some(ref rt)) = (&return_flag_temp, &return_temp) {
                let flag_r = self.compile_expression(&Expression::Identifier(rf.clone()))?;
                let skip_return = self.emit(ROp::JumpIfNot, &[flag_r, 9999]);
                let val_r = self.compile_expression(&Expression::Identifier(rt.clone()))?;
                self.emit(ROp::Return, &[val_r]);
                let end = self.instructions.len();
                self.patch_jump(skip_return, end);
            }
        }

        // Patch return_jumps from try/catch to finally start
        for pos in all_return_jumps {
            self.patch_jump(pos, finally_start);
        }

        let end = self.instructions.len();
        if finally_block.is_some() {
            self.patch_jump(jump_after_try, finally_start);
        } else {
            self.patch_jump(jump_after_try, end);
        }
        Ok(last_reg)
    }
}
