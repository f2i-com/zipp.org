//! Assignment lowering — plain `x = v`, indexed `a[i] = v`, property
//! `obj.x = v`, and the logical `x ||= v` / `x &&= v` / `x ??= v`
//! family.
//!
//! Destructuring (`{a, b} = obj`, `[x, y] = arr`) is handled by the
//! `assign_pattern` / `destructure_*_assignment` helpers kept in
//! `mod.rs` — those lean on a lot of binding-pattern machinery that
//! isn't assignment-specific.
//!
//! Extracted from `rcompiler/mod.rs` in 0.6 so the orchestration core
//! is easier to navigate. `impl RCompiler` in a sibling submodule,
//! no behavioural change.

use std::rc::Rc;

use crate::ast::Expression;
use crate::rcode::ROp;

use super::RCompiler;

impl RCompiler {
    pub(super) fn compile_assignment_into(
        &mut self,
        left: &Expression,
        operator: &str,
        right: &Expression,
        dst: u16,
    ) -> Result<(), String> {
        match left {
            Expression::Identifier(name) => {
                self.compile_ident_assignment_into(name, operator, right, dst)
            }
            Expression::Index {
                left: object_expr,
                index,
            } => self.compile_index_assignment_into(object_expr, index, operator, right, dst),
            Expression::Array(items) => {
                if operator != "=" {
                    return Err("only '=' supported for array destructuring assignment".to_string());
                }
                let src = self.compile_expression(right)?;
                self.destructure_array_assignment(items, src)?;
                // Skip Move if dst was claimed by a new local during destructuring
                // (ensure_local can allocate the same register as an earlier temp).
                if dst != src && !self.locals.values().any(|&r| r == dst) {
                    self.emit(ROp::Move, &[dst, src]);
                }
                Ok(())
            }
            Expression::Hash(pairs) => {
                if operator != "=" {
                    return Err(
                        "only '=' supported for object destructuring assignment".to_string()
                    );
                }
                let src = self.compile_expression(right)?;
                self.destructure_object_assignment(pairs, src)?;
                // Skip Move if dst was claimed by a new local during destructuring.
                if dst != src && !self.locals.values().any(|&r| r == dst) {
                    self.emit(ROp::Move, &[dst, src]);
                }
                Ok(())
            }
            // For partially-parsed code, handle unknown targets gracefully
            Expression::Infix { left: inner_left, operator: op, right: inner_right } => {
                // Handle `a.b = value` where dot access was parsed as infix
                if op == "." {
                    if let Expression::Identifier(prop) = inner_right.as_ref() {
                        let index_expr = Expression::String(prop.clone());
                        return self.compile_index_assignment_into(inner_left, &index_expr, operator, right, dst);
                    }
                }
                // Skip unrecognized patterns in error-tolerant mode
                let src = self.compile_expression(right)?;
                if dst != src { self.emit(ROp::Move, &[dst, src]); }
                Ok(())
            }
            _ => {
                // Error-tolerant: compile RHS and discard (assignment target unrecognized)
                let src = self.compile_expression(right)?;
                if dst != src { self.emit(ROp::Move, &[dst, src]); }
                Ok(())
            }
        }
    }

    pub(super) fn compile_ident_assignment_into(
        &mut self,
        name: &str,
        operator: &str,
        right: &Expression,
        dst: u16,
    ) -> Result<(), String> {
        if self.const_bindings.contains(name) {
            return Err(format!("Assignment to constant variable '{}'", name));
        }
        if operator == "&&=" || operator == "||=" || operator == "??=" {
            return self.compile_logical_assignment_ident(name, operator, right, dst);
        }

        if operator == "=" {
            // Fused: local = local + CONST → AddRegConst
            if let Some(&r) = self.locals.get(name) {
                if let Expression::Infix {
                    left: inner_left,
                    operator: inner_op,
                    right: inner_right,
                } = right
                {
                    if inner_op == "+" {
                        if let Expression::Identifier(inner_name) = inner_left.as_ref() {
                            if inner_name == name {
                                if let Some(const_idx) = self.try_numeric_const(inner_right) {
                                    self.emit(ROp::AddRegConst, &[r, r, const_idx]);
                                    self.mirror_local_to_global(name, r);
                                    if dst != r {
                                        self.emit(ROp::Move, &[dst, r]);
                                    }
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }

            if let Some(&r) = self.locals.get(name) {
                // Compile directly into target register to avoid extra Move
                self.compile_expression_into(right, r)?;
                self.mirror_local_to_global(name, r);
                if dst != r {
                    self.emit(ROp::Move, &[dst, r]);
                }
            } else {
                let val = self.compile_expression(right)?;
                let g = self.ensure_global_slot(name)?;
                self.emit(ROp::SetGlobal, &[g, val]);
                if dst != val {
                    self.emit(ROp::Move, &[dst, val]);
                }
            }
            return Ok(());
        }

        // Fused: local += CONST → AddRegConst
        if operator == "+=" {
            if let Some(&r) = self.locals.get(name) {
                if let Some(const_idx) = self.try_numeric_const(right) {
                    self.emit(ROp::AddRegConst, &[r, r, const_idx]);
                    self.mirror_local_to_global(name, r);
                    if dst != r {
                        self.emit(ROp::Move, &[dst, r]);
                    }
                    return Ok(());
                }
            }
        }

        // Compound assignment: ident op= right
        let base_op = match operator {
            "+=" => ROp::Add,
            "-=" => ROp::Sub,
            "*=" => ROp::Mul,
            "/=" => ROp::Div,
            "%=" => ROp::Mod,
            "**=" => ROp::Pow,
            "&=" => ROp::BitwiseAnd,
            "|=" => ROp::BitwiseOr,
            "^=" => ROp::BitwiseXor,
            "<<=" => ROp::LeftShift,
            ">>=" => ROp::RightShift,
            ">>>=" => ROp::UnsignedRightShift,
            _ => return Err(format!("unsupported assignment operator {}", operator)),
        };

        // Load current value
        let cur = self.alloc_temp();
        self.load_identifier_into(name, cur)?;

        // Compute right side
        let rhs = self.compile_expression(right)?;

        // Compute result
        let result = self.alloc_temp();
        self.emit(base_op, &[result, cur, rhs]);

        // Store back
        if let Some(&r) = self.locals.get(name) {
            if r != result {
                self.emit(ROp::Move, &[r, result]);
            }
            self.mirror_local_to_global(name, r);
            if dst != r {
                self.emit(ROp::Move, &[dst, r]);
            }
        } else {
            let g = self.ensure_global_slot(name)?;
            self.emit(ROp::SetGlobal, &[g, result]);
            if dst != result {
                self.emit(ROp::Move, &[dst, result]);
            }
        }
        Ok(())
    }

    pub(super) fn compile_index_assignment_into(
        &mut self,
        object_expr: &Expression,
        index: &Expression,
        operator: &str,
        right: &Expression,
        dst: u16,
    ) -> Result<(), String> {
        // Fused: obj.prop = obj.prop + CONST → AddConstToRegProp
        // Recognizes the non-compound form and emits the same fused opcode.
        // Must be checked BEFORE generic SetProp (more specific pattern).
        if operator == "=" {
            if let Expression::String(prop) = index {
                if let Expression::Identifier(obj_name) = object_expr {
                    if let Some(&obj_r) = self.locals.get(obj_name.as_str()) {
                        if let Expression::Infix {
                            left: inner_left,
                            operator: ref inner_op,
                            right: inner_right,
                        } = right
                        {
                            if inner_op == "+" {
                                // Check: inner_left is same_obj.same_prop
                                let left_matches = if let Expression::Index {
                                    left: src_obj,
                                    index: src_idx,
                                } = inner_left.as_ref()
                                {
                                    matches!(src_obj.as_ref(), Expression::Identifier(n) if n == obj_name)
                                        && matches!(src_idx.as_ref(), Expression::String(p) if p == prop)
                                } else {
                                    false
                                };
                                if left_matches {
                                    if let Some(val_const) = self.try_numeric_const(inner_right) {
                                        let prop_const =
                                            self.add_constant_string(Rc::from(prop.as_str()));
                                        let cache_slot = self.next_cache_slot;
                                        self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                                        self.emit(
                                            ROp::AddConstToRegProp,
                                            &[obj_r, prop_const, val_const, cache_slot],
                                        );
                                        if dst != obj_r {
                                            let prop_c2 =
                                                self.add_constant_string(Rc::from(prop.as_str()));
                                            let cache2 = self.next_cache_slot;
                                            self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                                            self.emit(
                                                ROp::GetProp,
                                                &[dst, obj_r, prop_c2, cache2],
                                            );
                                        }
                                        return Ok(());
                                    }
                                }
                                // Also check: inner_right is same_obj.same_prop (CONST + obj.prop)
                                let right_matches = if let Expression::Index {
                                    left: src_obj,
                                    index: src_idx,
                                } = inner_right.as_ref()
                                {
                                    matches!(src_obj.as_ref(), Expression::Identifier(n) if n == obj_name)
                                        && matches!(src_idx.as_ref(), Expression::String(p) if p == prop)
                                } else {
                                    false
                                };
                                if right_matches {
                                    if let Some(val_const) = self.try_numeric_const(inner_left) {
                                        let prop_const =
                                            self.add_constant_string(Rc::from(prop.as_str()));
                                        let cache_slot = self.next_cache_slot;
                                        self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                                        self.emit(
                                            ROp::AddConstToRegProp,
                                            &[obj_r, prop_const, val_const, cache_slot],
                                        );
                                        if dst != obj_r {
                                            let prop_c2 =
                                                self.add_constant_string(Rc::from(prop.as_str()));
                                            let cache2 = self.next_cache_slot;
                                            self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                                            self.emit(
                                                ROp::GetProp,
                                                &[dst, obj_r, prop_c2, cache2],
                                            );
                                        }
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fused: obj.prop += CONST → AddConstToRegProp
        if operator == "+=" {
            if let Expression::String(prop) = index {
                let obj_name: Option<&str> = match object_expr {
                    Expression::Identifier(name) => Some(name.as_str()),
                    Expression::This => Some("this"),
                    _ => None,
                };
                if let Some(name) = obj_name {
                    if let Some(&obj_r) = self.locals.get(name) {
                        if let Some(val_const) = self.try_numeric_const(right) {
                            let prop_const = self.add_constant_string(Rc::from(prop.as_str()));
                            let cache_slot = self.next_cache_slot;
                            self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                            self.emit(
                                ROp::AddConstToRegProp,
                                &[obj_r, prop_const, val_const, cache_slot],
                            );
                            // Result is the new property value; fetch it for dst
                            if dst != obj_r {
                                let prop_c2 = self.add_constant_string(Rc::from(prop.as_str()));
                                let cache2 = self.next_cache_slot;
                                self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                                self.emit(
                                    ROp::GetProp,
                                    &[dst, obj_r, prop_c2, cache2],
                                );
                            }
                            return Ok(());
                        }
                    }
                }
            }
        }

        // Fused: obj.z = obj.x + obj.y → AddRegPropsToRegProp
        // Must be checked BEFORE generic SetProp (more specific pattern).
        if operator == "=" {
            if let Expression::String(dst_prop) = index {
                if let Expression::Identifier(obj_name) = object_expr {
                    if let Some(&obj_r) = self.locals.get(obj_name.as_str()) {
                        if let Expression::Infix {
                            left: add_left,
                            operator: add_op,
                            right: add_right,
                        } = right
                        {
                            if add_op == "+" {
                                if let (
                                    Expression::Index {
                                        left: s1_obj,
                                        index: s1_idx,
                                    },
                                    Expression::Index {
                                        left: s2_obj,
                                        index: s2_idx,
                                    },
                                ) = (add_left.as_ref(), add_right.as_ref())
                                {
                                    let s1_same = matches!(s1_obj.as_ref(),
                                        Expression::Identifier(n) if n == obj_name);
                                    let s2_same = matches!(s2_obj.as_ref(),
                                        Expression::Identifier(n) if n == obj_name);
                                    if s1_same && s2_same {
                                        if let (
                                            Expression::String(s1_prop),
                                            Expression::String(s2_prop),
                                        ) = (s1_idx.as_ref(), s2_idx.as_ref())
                                        {
                                            let s1_const = self
                                                .add_constant_string(Rc::from(s1_prop.as_str()));
                                            let s1_cache = self.next_cache_slot;
                                            self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                                            let s2_const = self
                                                .add_constant_string(Rc::from(s2_prop.as_str()));
                                            let s2_cache = self.next_cache_slot;
                                            self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                                            let dst_const = self
                                                .add_constant_string(Rc::from(dst_prop.as_str()));
                                            let dst_cache = self.next_cache_slot;
                                            self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                                            self.emit(
                                                ROp::AddRegPropsToRegProp,
                                                &[
                                                    obj_r,
                                                    s1_const,
                                                    s1_cache,
                                                    s2_const,
                                                    s2_cache,
                                                    dst_const,
                                                    dst_cache,
                                                ],
                                            );
                                            return Ok(());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Generic: obj.prop = value with inline cache (fallback after fused checks)
        if operator == "=" {
            if let Expression::String(prop) = index {
                let obj_name: Option<&str> = match object_expr {
                    Expression::Identifier(name) => Some(name.as_str()),
                    Expression::This => Some("this"),
                    _ => None,
                };
                if let Some(name) = obj_name {
                    let is_local = self.locals.contains_key(name);
                    if is_local {
                        let obj_r = *self.locals.get(name).unwrap();
                        let const_idx = self.add_constant_string(Rc::from(prop.as_str()));
                        let cache_slot = self.next_cache_slot;
                        self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                        self.compile_expression_into(right, dst)?;
                        self.emit(
                            ROp::SetProp,
                            &[obj_r, const_idx, dst, cache_slot],
                        );
                        return Ok(());
                    }
                    if let Some(&global_idx) = self.globals.get(name) {
                        let const_idx = self.add_constant_string(Rc::from(prop.as_str()));
                        let cache_slot = self.next_cache_slot;
                        self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                        self.compile_expression_into(right, dst)?;
                        self.emit(
                            ROp::SetGlobalProp,
                            &[global_idx, const_idx, dst, cache_slot],
                        );
                        return Ok(());
                    }
                }
            }
        }

        // Handle short-circuit logical assignment operators for index targets
        if operator == "||=" || operator == "&&=" || operator == "??=" {
            let obj = self.compile_expression(object_expr)?;
            let key = self.compile_expression(index)?;
            let old = self.alloc_temp();
            self.emit(ROp::Index, &[old, obj, key]);

            let skip_jump = match operator {
                "||=" => self.emit(ROp::JumpIfTruthy, &[old, 9999]),
                "&&=" => self.emit(ROp::JumpIfNot, &[old, 9999]),
                "??=" => {
                    // IsNullish then JumpIfNot (skip if NOT nullish, i.e. keep existing value)
                    let is_null = self.alloc_temp();
                    self.emit(ROp::IsNullish, &[is_null, old]);
                    self.emit(ROp::JumpIfNot, &[is_null, 9999])
                }
                _ => unreachable!(),
            };
            let rhs = self.compile_expression(right)?;
            self.emit(ROp::SetIndex, &[obj, key, rhs]);
            // Write back to original local if needed
            let orig_local = match object_expr {
                Expression::Identifier(name) => self.locals.get(name.as_str()).copied(),
                Expression::This => self.locals.get("this").copied(),
                _ => None,
            };
            if let Some(local_r) = orig_local {
                if local_r != obj {
                    self.emit(ROp::Move, &[local_r, obj]);
                }
            }
            self.emit(ROp::Move, &[dst, rhs]);
            let end = self.instructions.len();
            self.patch_jump(skip_jump, end);
            // If we skipped, dst should be old value
            self.emit(ROp::Move, &[dst, old]);
            return Ok(());
        }

        // General case: obj[key] op= right
        let base_op = match operator {
            "=" => None,
            "+=" => Some(ROp::Add),
            "-=" => Some(ROp::Sub),
            "*=" => Some(ROp::Mul),
            "/=" => Some(ROp::Div),
            "%=" => Some(ROp::Mod),
            "**=" => Some(ROp::Pow),
            "&=" => Some(ROp::BitwiseAnd),
            "|=" => Some(ROp::BitwiseOr),
            "^=" => Some(ROp::BitwiseXor),
            "<<=" => Some(ROp::LeftShift),
            ">>=" => Some(ROp::RightShift),
            ">>>=" => Some(ROp::UnsignedRightShift),
            _ => {
                return Err(format!(
                    "unsupported assignment operator {} for index target",
                    operator
                ))
            }
        };

        let obj = self.compile_expression(object_expr)?;
        let key = self.compile_expression(index)?;

        let val = if let Some(op) = base_op {
            let old = self.alloc_temp();
            self.emit(ROp::Index, &[old, obj, key]);
            let rhs = self.compile_expression(right)?;
            let result = self.alloc_temp();
            self.emit(op, &[result, old, rhs]);
            result
        } else {
            self.compile_expression(right)?
        };

        self.emit(ROp::SetIndex, &[obj, key, val]);

        // Write updated object back to its original local register.
        // SetIndex stores the updated object in register `obj` (a temp),
        // but the original local (e.g., `this`) isn't updated unless we copy back.
        let orig_local = match object_expr {
            Expression::Identifier(name) => self.locals.get(name.as_str()).copied(),
            Expression::This => self.locals.get("this").copied(),
            _ => None,
        };
        if let Some(local_r) = orig_local {
            if obj != local_r {
                self.emit(ROp::Move, &[local_r, obj]);
            }
        }

        if dst != val {
            self.emit(ROp::Move, &[dst, val]);
        }
        Ok(())
    }

    pub(super) fn compile_logical_assignment_ident(
        &mut self,
        name: &str,
        operator: &str,
        right: &Expression,
        dst: u16,
    ) -> Result<(), String> {
        let cur = self.alloc_temp();
        self.load_identifier_into(name, cur)?;

        match operator {
            "&&=" => {
                if cur != dst {
                    self.emit(ROp::Move, &[dst, cur]);
                }
                let keep_pos = self.emit(ROp::JumpIfNot, &[dst, 9999]);
                self.compile_expression_into(right, dst)?;
                // Store back
                if let Some(&r) = self.locals.get(name) {
                    if r != dst {
                        self.emit(ROp::Move, &[r, dst]);
                    }
                    self.mirror_local_to_global(name, r);
                } else {
                    let g = self.ensure_global_slot(name)?;
                    self.emit(ROp::SetGlobal, &[g, dst]);
                }
                let end = self.instructions.len();
                self.patch_jump(keep_pos, end);
            }
            "||=" => {
                if cur != dst {
                    self.emit(ROp::Move, &[dst, cur]);
                }
                let keep_pos = self.emit(ROp::JumpIfTruthy, &[dst, 9999]);
                self.compile_expression_into(right, dst)?;
                if let Some(&r) = self.locals.get(name) {
                    if r != dst {
                        self.emit(ROp::Move, &[r, dst]);
                    }
                    self.mirror_local_to_global(name, r);
                } else {
                    let g = self.ensure_global_slot(name)?;
                    self.emit(ROp::SetGlobal, &[g, dst]);
                }
                let end = self.instructions.len();
                self.patch_jump(keep_pos, end);
            }
            "??=" => {
                let nullish = self.alloc_temp();
                self.emit(ROp::IsNullish, &[nullish, cur]);
                let keep_pos = self.emit(ROp::JumpIfNot, &[nullish, 9999]);
                self.compile_expression_into(right, dst)?;
                if let Some(&r) = self.locals.get(name) {
                    if r != dst {
                        self.emit(ROp::Move, &[r, dst]);
                    }
                    self.mirror_local_to_global(name, r);
                } else {
                    let g = self.ensure_global_slot(name)?;
                    self.emit(ROp::SetGlobal, &[g, dst]);
                }
                let end_pos = self.emit(ROp::Jump, &[9999]);
                let keep = self.instructions.len();
                self.patch_jump(keep_pos, keep);
                if cur != dst {
                    self.emit(ROp::Move, &[dst, cur]);
                }
                let end = self.instructions.len();
                self.patch_jump(end_pos, end);
            }
            _ => {
                return Err(format!(
                    "unsupported logical assignment operator {}",
                    operator
                ))
            }
        }
        Ok(())
    }
}
