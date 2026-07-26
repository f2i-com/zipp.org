// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Does compiling this expression into a destination register WRITE that
/// register before it has finished READING its operands?
///
/// Almost every form computes its operands into temporaries and only then
/// writes the destination, so `x = f(x)` and `x = [x]` are safe to compile
/// straight into `x`'s register. Two forms are not: an object literal emits
/// `NewObject{dst}` and then evaluates each property value, and a template
/// literal builds its result in the destination across the interpolations. For
/// those, an operand that reads the target variable would observe the
/// half-built value rather than the variable's old one, so the caller must
/// route them through a temporary.
///
/// A nested assignment to a MEMBER is the third: `x = x.p = v` compiles the
/// inner store by resolving its base object to a register and then evaluating
/// `v` into the destination. When `x` is a local, the base "register" IS the
/// destination — no copy is made for a plain identifier — so writing `v` there
/// clobbers the object before the store, and `.p` lands on `v` itself. React's
/// minified `useState` mount is exactly this shape
/// (`e = e.dispatch = K.bind(null, fiber, e)`), which left every state setter
/// null on re-render: the first click of any control worked and the second
/// threw "null is not a function".
///
/// Conditionals, logicals and sequences are transparent: their value comes from
/// a sub-expression compiled into the same destination, so they inherit the
/// property. This is deliberately a whitelist of what is SAFE-by-omission — a
/// new in-place-building form must be added here, and `assign_reads_target` in
/// the tests covers each shape.
fn builds_into_dst_incrementally(e: &ox::Expression) -> bool {
    use ox::Expression as E;
    match e {
        E::ObjectExpression(_) => true,
        // A template with no interpolations is a plain constant string.
        E::TemplateLiteral(t) => !t.expressions.is_empty(),
        E::ConditionalExpression(c) => {
            builds_into_dst_incrementally(&c.consequent)
                || builds_into_dst_incrementally(&c.alternate)
        }
        E::LogicalExpression(l) => {
            builds_into_dst_incrementally(&l.left) || builds_into_dst_incrementally(&l.right)
        }
        E::SequenceExpression(s) => {
            s.expressions.last().is_some_and(builds_into_dst_incrementally)
        }
        E::ParenthesizedExpression(p) => builds_into_dst_incrementally(&p.expression),
        E::AssignmentExpression(a) => {
            use ox::AssignmentTarget as T;
            matches!(
                a.left,
                T::StaticMemberExpression(_)
                    | T::ComputedMemberExpression(_)
                    | T::PrivateFieldExpression(_)
            ) || builds_into_dst_incrementally(&a.right)
        }
        _ => false,
    }
}

impl<'a> FnCompiler<'a> {
    /// Assign `src` to a destructuring-assignment target (existing binding or
    /// member, or a nested array/object pattern). Counterpart to `extract_pattern`
    /// for `=` targets that aren't declarations.
    pub(crate) fn assign_target(&mut self, target: &ox::AssignmentTarget, src: Reg) -> R<()> {
        use ox::AssignmentTarget as T;
        match target {
            T::AssignmentTargetIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, src);
                Ok(())
            }
            T::StaticMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                self.emit(Instr::SetProp { obj, name, val: src });
                self.next_reg = save;
                Ok(())
            }
            T::ComputedMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                // Fuse `obj[<plain string literal> + e] = v` → SetIndexConcat
                // (no throwaway concat-key allocation; see GetIndexConcat).
                if let Some((name, rhs)) = concat_key_literal_prefix(&m.expression) {
                    let nidx = self.string_name(name);
                    let key = self.expr(rhs)?;
                    self.emit(Instr::SetIndexConcat { obj, name: nidx, key, val: src });
                    self.next_reg = save;
                    return Ok(());
                }
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SetIndex { obj, key, val: src });
                self.next_reg = save;
                Ok(())
            }
            T::PrivateFieldExpression(m) => {
                // `[this.#x] = arr` / `({a: this.#x} = o)`: a private field as a
                // destructuring target — brand-checked PrivateSet (the target
                // reference is taken before the value per the destructuring driver).
                self.check_private_declared(&m.field.name)?;
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(&private_key(&m.field.name));
                self.emit(Instr::SetPrivate { obj, name, val: src });
                self.next_reg = save;
                Ok(())
            }
            T::ArrayAssignmentTarget(arr) => self.assign_array_target(arr, src),
            T::ObjectAssignmentTarget(o) => self.assign_object_target(o, src),
            _ => Err("unsupported destructuring-assignment target in the zipp-vm subset".into()),
        }
    }

    /// A destructuring MEMBER target's reference, evaluated BEFORE the source
    /// property read (KeyedDestructuringAssignmentEvaluation: the
    /// DestructuringAssignmentTarget reference comes first, then GetV).
    pub(crate) fn pre_member_ref(
        &mut self,
        m: &ox::AssignmentTargetMaybeDefault,
    ) -> R<Option<(Reg, PreKey)>> {
        use ox::AssignmentTargetMaybeDefault as M;
        // Unwrap a `target = default` to its inner target.
        let inner: &ox::AssignmentTarget = match m {
            M::AssignmentTargetWithDefault(d) => &d.binding,
            // The flattened member variants reuse the same node types — handle
            // them via a reconstructed reference below.
            M::StaticMemberExpression(sm) => {
                let r = self.pin_expr(&sm.object)?;
                let name = self.string_name(sm.property.name.as_str());
                return Ok(Some((r, PreKey::Static(name))));
            }
            M::ComputedMemberExpression(cm) => {
                let r = self.pin_expr(&cm.object)?;
                let k = self.pin_expr(&cm.expression)?;
                return Ok(Some((r, PreKey::Computed(k))));
            }
            M::PrivateFieldExpression(pm) => {
                self.check_private_declared(&pm.field.name)?;
                let r = self.pin_expr(&pm.object)?;
                let name = self.string_name(&private_key(&pm.field.name));
                return Ok(Some((r, PreKey::Private(name))));
            }
            _ => return Ok(None),
        };
        use ox::AssignmentTarget as T;
        match inner {
            T::StaticMemberExpression(sm) => {
                let r = self.pin_expr(&sm.object)?;
                let name = self.string_name(sm.property.name.as_str());
                Ok(Some((r, PreKey::Static(name))))
            }
            T::ComputedMemberExpression(cm) => {
                let r = self.pin_expr(&cm.object)?;
                let k = self.pin_expr(&cm.expression)?;
                Ok(Some((r, PreKey::Computed(k))))
            }
            T::PrivateFieldExpression(pm) => {
                self.check_private_declared(&pm.field.name)?;
                let r = self.pin_expr(&pm.object)?;
                let name = self.string_name(&private_key(&pm.field.name));
                Ok(Some((r, PreKey::Private(name))))
            }
            _ => Ok(None),
        }
    }

    /// Evaluate `e` into a PINNED register (survives later evaluation; the
    /// caller's next_reg reset reclaims it).
    pub(crate) fn pin_expr(&mut self, e: &ox::Expression) -> R<Reg> {
        let r = self.alloc_reg();
        let v = self.expr_into(e, r)?;
        if v != r {
            self.emit(Instr::Move { dst: r, src: v });
        }
        Ok(r)
    }

    /// Store through a reference produced by `pre_member_ref`, applying any
    /// `= default` of `m` first (no NamedEvaluation — the target is a member).
    pub(crate) fn store_pre_ref(
        &mut self,
        m: &ox::AssignmentTargetMaybeDefault,
        obj: Reg,
        key: &PreKey,
        val: Reg,
    ) -> R<()> {
        if let ox::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) = m {
            self.apply_default_in_place_named(val, &d.init, None)?;
        }
        match *key {
            PreKey::Static(name) => self.emit(Instr::SetProp { obj, name, val }),
            PreKey::Computed(k) => self.emit(Instr::SetIndex { obj, key: k, val }),
            PreKey::Private(name) => self.emit(Instr::SetPrivate { obj, name, val }),
        }
        Ok(())
    }

    /// One element of a destructuring assignment, applying its `= default` first.
    pub(crate) fn assign_maybe_default(
        &mut self,
        m: &ox::AssignmentTargetMaybeDefault,
        val: Reg,
    ) -> R<()> {
        use ox::AssignmentTargetMaybeDefault as M;
        match m {
            M::AssignmentTargetWithDefault(d) => {
                // `[a = function(){}] = arr` ⇒ the default function takes name "a".
                let name = match &d.binding {
                    ox::AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.to_string()),
                    _ => None,
                };
                self.apply_default_in_place_named(val, &d.init, name.as_deref())?;
                self.assign_target(&d.binding, val)
            }
            M::AssignmentTargetIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, val);
                Ok(())
            }
            M::StaticMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                self.emit(Instr::SetProp { obj, name, val });
                self.next_reg = save;
                Ok(())
            }
            M::ComputedMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SetIndex { obj, key, val });
                self.next_reg = save;
                Ok(())
            }
            M::PrivateFieldExpression(m) => {
                self.check_private_declared(&m.field.name)?;
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(&private_key(&m.field.name));
                self.emit(Instr::SetPrivate { obj, name, val });
                self.next_reg = save;
                Ok(())
            }
            M::ArrayAssignmentTarget(arr) => self.assign_array_target(arr, val),
            M::ObjectAssignmentTarget(o) => self.assign_object_target(o, val),
            _ => Err("unsupported destructuring-assignment element in the zipp-vm subset".into()),
        }
    }

    pub(crate) fn assign_array_target(&mut self, arr: &ox::ArrayAssignmentTarget, src_in: Reg) -> R<()> {
        // Array assignment destructuring: the SPEC's stepwise iterator driver.
        // Per element: evaluate a member target's REFERENCE first, then
        // IteratorStep (no step once exhausted), then the default, then store
        // through the saved reference. An abrupt completion anywhere closes a
        // non-exhausted iterator QUIETLY (the original throw wins); a normal
        // completion closes STRICTLY (a throwing/non-object return() result
        // propagates).
        let save_top = self.next_reg;
        let iter_reg = self.alloc_reg();
        self.emit(Instr::CheckIterable { src: src_in });
        self.emit(Instr::GetIterator { dst: iter_reg, src: src_in });
        let idx_reg = self.alloc_reg();
        self.emit(Instr::LoadInt { dst: idx_reg, val: 0 });
        let done = self.alloc_reg();
        self.emit(Instr::LoadBool { dst: done, val: false });
        // FINALLY-kind protection (not catch): a `yield` inside the pattern
        // can suspend the generator with this iterator OPEN — a later
        // `.return()` injects a RETURN completion that unwinds through
        // finally handlers only, and IteratorClose must run then too.
        let kind_reg = self.alloc_reg();
        let val_reg = self.alloc_reg();
        let push_at = self.here();
        self.emit(Instr::PushFinally { target: 0, kind_reg, val_reg });
        self.handler_depth += 1;
        for el in &arr.elements {
            let save = self.next_reg;
            let pre = match el {
                Some(maybe) => self.pre_member_ref(maybe)?,
                None => None,
            };
            let val = self.alloc_reg();
            let dflag = self.alloc_reg();
            // Step (skipped once exhausted): done elements read undefined.
            // `done` is PRE-SET before the step — an abrupt completion FROM
            // the iterator itself (next()/done-getter/value-getter throw)
            // leaves it true, so the catch path skips IteratorClose (the
            // spec marks [[Done]] before propagating); a successful value
            // clears it.
            let jdone = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
            self.emit(Instr::LoadBool { dst: done, val: true });
            self.emit(Instr::IterNext { value_dst: val, done_dst: dflag, iter: iter_reg, idx: idx_reg, next: Reg::MAX });
            let jexh = self.here();
            self.emit(Instr::JumpIfTrue { cond: dflag, target: 0 });
            self.emit(Instr::LoadBool { dst: done, val: false });
            let jgot = self.here();
            self.emit(Instr::Jump { target: 0 });
            let at_undef = self.here();
            self.patch_jump(jdone, at_undef);
            self.patch_jump(jexh, at_undef);
            self.emit(Instr::LoadUndefined { dst: val });
            let got = self.here();
            self.patch_jump(jgot, got);
            if let Some(maybe) = el {
                match pre {
                    Some((obj, key)) => self.store_pre_ref(maybe, obj, &key, val)?,
                    None => self.assign_maybe_default(maybe, val)?,
                }
            }
            self.next_reg = save;
        }
        if let Some(rest) = &arr.rest {
            let save = self.next_reg;
            let pre = self.pre_rest_ref(&rest.target)?;
            let out = self.alloc_reg();
            self.emit(Instr::ArrayCtor { dst: out, arg_base: 0, argc: 0 });
            let v = self.alloc_reg();
            let dflag = self.alloc_reg();
            let loop_top = self.here();
            let jrest_done = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
            self.emit(Instr::LoadBool { dst: done, val: true });
            self.emit(Instr::IterNext { value_dst: v, done_dst: dflag, iter: iter_reg, idx: idx_reg, next: Reg::MAX });
            let jout = self.here();
            self.emit(Instr::JumpIfTrue { cond: dflag, target: 0 });
            self.emit(Instr::LoadBool { dst: done, val: false });
            self.emit(Instr::ArrayAppend { arr: out, val: v, spread: false });
            self.emit(Instr::Jump { target: loop_top });
            let rest_done = self.here();
            self.patch_jump(jrest_done, rest_done);
            self.patch_jump(jout, rest_done);
            match pre {
                Some((obj, key)) => {
                    match key {
                        PreKey::Static(name) => self.emit(Instr::SetProp { obj, name, val: out }),
                        PreKey::Computed(k) => self.emit(Instr::SetIndex { obj, key: k, val: out }),
                        PreKey::Private(name) => self.emit(Instr::SetPrivate { obj, name, val: out }),
                    }
                }
                None => self.assign_target(&rest.target, out)?,
            }
            self.next_reg = save;
        }
        self.emit(Instr::PopFinally);
        self.handler_depth -= 1;
        // Normal completion: close iff not exhausted (strict result checks).
        let jskip = self.here();
        self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
        self.emit(Instr::IterClose { iter: iter_reg });
        let jend = self.here();
        self.emit(Instr::Jump { target: 0 });
        // ABRUPT exits land here (a throw from the pattern, or a RETURN
        // completion unwinding through a `yield` inside it). Exhausted /
        // iterator-own abrupts skip the close; a THROW closes QUIETLY (the
        // original reason wins); a return closes STRICTLY (IteratorClose's
        // own throw / non-object TypeError replaces the completion).
        // EndFinally then resumes whatever completion remains pending.
        let fin_start = self.here();
        if let Instr::PushFinally { target, .. } = &mut self.code[push_at as usize] {
            *target = fin_start;
        }
        let jresume = self.here();
        self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
        let two = self.alloc_reg();
        self.emit(Instr::LoadInt { dst: two, val: 2 });
        let isthrow = self.alloc_reg();
        self.emit(Instr::Eq { dst: isthrow, a: kind_reg, b: two });
        let jnotthrow = self.here();
        self.emit(Instr::JumpIfFalse { cond: isthrow, target: 0 });
        self.emit(Instr::IterCloseQuiet { iter: iter_reg });
        let jresume2 = self.here();
        self.emit(Instr::Jump { target: 0 });
        let at_ret = self.here();
        self.patch_jump(jnotthrow, at_ret);
        self.emit(Instr::IterClose { iter: iter_reg });
        let resume = self.here();
        self.patch_jump(jresume, resume);
        self.patch_jump(jresume2, resume);
        self.emit(Instr::EndFinally { kind_reg, val_reg });
        let end = self.here();
        self.patch_jump(jskip, end);
        self.patch_jump(jend, end);
        self.next_reg = save_top;
        Ok(())
    }

    /// `pre_member_ref` for a REST target (a plain AssignmentTarget).
    pub(crate) fn pre_rest_ref(&mut self, t: &ox::AssignmentTarget) -> R<Option<(Reg, PreKey)>> {
        use ox::AssignmentTarget as T;
        match t {
            T::StaticMemberExpression(sm) => {
                let r = self.pin_expr(&sm.object)?;
                let name = self.string_name(sm.property.name.as_str());
                Ok(Some((r, PreKey::Static(name))))
            }
            T::ComputedMemberExpression(cm) => {
                let r = self.pin_expr(&cm.object)?;
                let k = self.pin_expr(&cm.expression)?;
                Ok(Some((r, PreKey::Computed(k))))
            }
            T::PrivateFieldExpression(pm) => {
                self.check_private_declared(&pm.field.name)?;
                let r = self.pin_expr(&pm.object)?;
                let name = self.string_name(&private_key(&pm.field.name));
                Ok(Some((r, PreKey::Private(name))))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn assign_object_target(&mut self, o: &ox::ObjectAssignmentTarget, src: Reg) -> R<()> {
        // RequireObjectCoercible(src) for an empty pattern (`({} = x)` /
        // `({...rest} = x)`) — no member access would otherwise guard null/undefined.
        if o.properties.is_empty() {
            self.emit(Instr::CheckCoercible { src });
        }
        // Computed sibling key + `...rest`: evaluate each sibling key once into a
        // contiguous block (reused for extraction and the ObjectRestDyn exclusion).
        let has_computed = o.rest.is_some()
            && o.properties.iter().any(|p| {
                matches!(p, ox::AssignmentTargetProperty::AssignmentTargetPropertyProperty(pp) if pp.computed)
            });
        if has_computed {
            use ox::AssignmentTargetProperty as ATP;
            let block_save = self.next_reg;
            let keys_base = self.next_reg;
            let n = o.properties.len() as u16;
            for _ in 0..o.properties.len() {
                self.alloc_reg();
            }
            for (i, prop) in o.properties.iter().enumerate() {
                let kreg = keys_base + i as Reg;
                match prop {
                    ATP::AssignmentTargetPropertyProperty(p) if p.computed => {
                        let e = p.name.as_expression().ok_or("unsupported computed destructuring key")?;
                        let v = self.expr_into(e, kreg)?;
                        if v != kreg {
                            self.emit(Instr::Move { dst: kreg, src: v });
                        }
                    }
                    ATP::AssignmentTargetPropertyProperty(p) => {
                        let name = class_key_name(&p.name)?;
                        let idx = self.add_string_const(&name);
                        self.emit(Instr::LoadConst { dst: kreg, idx });
                    }
                    ATP::AssignmentTargetPropertyIdentifier(p) => {
                        let idx = self.add_string_const(&p.binding.name);
                        self.emit(Instr::LoadConst { dst: kreg, idx });
                    }
                }
            }
            for (i, prop) in o.properties.iter().enumerate() {
                let save = self.next_reg;
                let kreg = keys_base + i as Reg;
                let val = self.alloc_reg();
                self.emit(Instr::GetIndex { dst: val, obj: src, key: kreg });
                match prop {
                    ATP::AssignmentTargetPropertyIdentifier(p) => {
                        if let Some(init) = &p.init {
                            self.apply_default_in_place_named(val, init, Some(&p.binding.name))?;
                        }
                        let b = self.resolve(&p.binding.name);
                        self.store_binding(&b, val);
                    }
                    ATP::AssignmentTargetPropertyProperty(p) => {
                        self.assign_maybe_default(&p.binding, val)?;
                    }
                }
                self.next_reg = save;
            }
            let rest = o.rest.as_ref().unwrap();
            let save = self.next_reg;
            let val = self.alloc_reg();
            self.emit(Instr::ObjectRestDyn { dst: val, src, keys_base, n });
            self.assign_target(&rest.target, val)?;
            self.next_reg = save;
            self.next_reg = block_save;
            return Ok(());
        }
        for prop in &o.properties {
            let save = self.next_reg;
            match prop {
                ox::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                    // `({x} = o)` / `({x = d} = o)` — target is the identifier itself.
                    let val = self.alloc_reg();
                    let name = self.string_name(&p.binding.name);
                    self.emit(Instr::GetProp { dst: val, obj: src, name });
                    if let Some(init) = &p.init {
                        // `({x = function(){}} = o)` ⇒ default takes the name "x".
                        self.apply_default_in_place_named(val, init, Some(&p.binding.name))?;
                    }
                    let b = self.resolve(&p.binding.name);
                    self.store_binding(&b, val);
                }
                ox::AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                    // `({key: target} = o)`. For a MEMBER target the spec
                    // order is: PropertyName (incl. ToPropertyKey) -> target
                    // REFERENCE (object + uncoerced key exprs) -> GetV ->
                    // default -> PutValue (target-key coercion at store).
                    let is_member_target = matches!(
                        &p.binding,
                        ox::AssignmentTargetMaybeDefault::StaticMemberExpression(_)
                            | ox::AssignmentTargetMaybeDefault::ComputedMemberExpression(_)
                            | ox::AssignmentTargetMaybeDefault::PrivateFieldExpression(_)
                    ) || matches!(
                        &p.binding,
                        ox::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d)
                            if matches!(
                                &d.binding,
                                ox::AssignmentTarget::StaticMemberExpression(_)
                                    | ox::AssignmentTarget::ComputedMemberExpression(_)
                                    | ox::AssignmentTarget::PrivateFieldExpression(_)
                            )
                    );
                    if is_member_target {
                        let skey: Option<Reg> = if p.computed {
                            let e = p
                                .name
                                .as_expression()
                                .ok_or("unsupported computed destructuring key")?;
                            let raw = self.pin_expr(e)?;
                            let k = self.alloc_reg();
                            self.emit(Instr::ToPropKey { dst: k, obj: src, src: raw });
                            Some(k)
                        } else {
                            None
                        };
                        let pre = self.pre_member_ref(&p.binding)?;
                        let val = self.alloc_reg();
                        match skey {
                            Some(k) => self.emit(Instr::GetIndex { dst: val, obj: src, key: k }),
                            None => {
                                let name = class_key_name(&p.name)?;
                                let nidx = self.string_name(&name);
                                self.emit(Instr::GetProp { dst: val, obj: src, name: nidx });
                            }
                        }
                        let (obj, key) = pre.expect("member target shape checked above");
                        self.store_pre_ref(&p.binding, obj, &key, val)?;
                    } else {
                        let val = self.alloc_reg();
                        self.extract_member(src, &p.name, p.computed, val)?;
                        self.assign_maybe_default(&p.binding, val)?;
                    }
                }
            }
            self.next_reg = save;
        }
        // `({a, ...rest} = o)` — a new object of `src`'s own keys minus the
        // siblings, assigned to the rest target (mirrors the declaration form).
        if let Some(rest) = &o.rest {
            let exclude_start = self.string_constants.len() as u32;
            let mut exclude_count = 0u16;
            for prop in &o.properties {
                let key = match prop {
                    ox::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                        p.binding.name.to_string()
                    }
                    ox::AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                        class_key_name(&p.name).map_err(|_| {
                            "object-rest with a computed sibling key is not in the subset"
                        })?
                    }
                };
                self.string_name(&key);
                exclude_count += 1;
            }
            let save = self.next_reg;
            let val = self.alloc_reg();
            self.emit(Instr::ObjectRest { dst: val, src, exclude_start, exclude_count });
            self.assign_target(&rest.target, val)?;
            self.next_reg = save;
        }
        Ok(())
    }

    /// For a logical assignment (`||= &&= ??=`), emit the short-circuit test on
    /// `val` (which already holds the target's current value) and return the ip
    /// of the jump that, when taken, SKIPS the assignment (keeping `val`).
    pub(crate) fn emit_logical_skip(&mut self, op: ox::AssignmentOperator, val: Reg) -> u32 {
        use ox::AssignmentOperator as Op;
        match op {
            Op::LogicalOr => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue { cond: val, target: 0 }); // truthy → skip
                j
            }
            Op::LogicalAnd => {
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: val, target: 0 }); // falsy → skip
                j
            }
            _ => {
                // ??= : skip when `val` is NOT strictly null/undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                let isnull = self.alloc_reg();
                self.emit_is_nullish(val, isnull, undef);
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: isnull, target: 0 });
                self.next_reg = save;
                j
            }
        }
    }

    pub(crate) fn assign(&mut self, a: &ox::AssignmentExpression, dst: Reg) -> R<Reg> {
        use ox::AssignmentOperator as Op;
        let is_logical =
            matches!(a.operator, Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish);
        // Member-target assignment: `obj.x = v` / `arr[i] = v`. Only plain
        // `=` is supported for members in this subset.
        match &a.left {
            // `super.x = v` / `super.x op= v` / `super.x ??= v`.
            ox::AssignmentTarget::StaticMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class;
                if pid.is_none() && !self.super_home_obj {
                    return Err("`super.x = …` is only valid in a method".into());
                }
                self.this_check();
                let name = self.string_name(m.property.name.as_str());
                // A super GET/SET routes to the class op (home_class_id) or the
                // object-method op ([[HomeObject]]), depending on the lexical context.
                let emit_get = |s: &mut Self, d: Reg| match pid {
                    Some(p) => s.emit(Instr::SuperGet { dst: d, home_class_id: p, name }),
                    None => s.emit(Instr::SuperGetObj { dst: d, name }),
                };
                let emit_set = |s: &mut Self, v: Reg| match pid {
                    Some(p) => s.emit(Instr::SuperSet { home_class_id: p, name, val: v }),
                    None => s.emit(Instr::SuperSetObj { name, val: v }),
                };
                if is_logical {
                    emit_get(self, dst);
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    emit_set(self, dst);
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    emit_set(self, dst);
                } else {
                    let cur = self.temp();
                    emit_get(self, cur);
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    emit_set(self, dst);
                }
                return Ok(dst);
            }
            ox::AssignmentTarget::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?; // evaluate the receiver once
                let name = self.string_name(m.property.name.as_str());
                if is_logical {
                    // `obj.x ??= v` etc: read current; skip the store on short-circuit.
                    self.emit(Instr::GetProp { dst, obj, name });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SetProp { obj, name, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SetProp { obj, name, val: dst });
                } else {
                    // Compound `obj.x op= v`: read obj.x, combine, write back.
                    let cur = self.temp();
                    self.emit(Instr::GetProp { dst: cur, obj, name });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SetProp { obj, name, val: dst });
                }
                return Ok(dst);
            }
            // `obj.#x = v` / `obj.#x op= v` — same as a static member, keyed "#x".
            ox::AssignmentTarget::PrivateFieldExpression(p) => {
                self.check_private_declared(&p.field.name)?;
                let obj = self.expr(&p.object)?;
                let name = self.string_name(&private_key(&p.field.name));
                if is_logical {
                    self.emit(Instr::GetProp { dst, obj, name });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SetProp { obj, name, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SetPrivate { obj, name, val: dst });
                } else {
                    let cur = self.temp();
                    self.emit(Instr::GetProp { dst: cur, obj, name });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SetPrivate { obj, name, val: dst });
                }
                return Ok(dst);
            }
            // `super[k] = v` / compound / logical.
            ox::AssignmentTarget::ComputedMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class;
                if pid.is_none() && !self.super_home_obj {
                    return Err("`super[k] = …` is only valid in a method".into());
                }
                self.this_check();
                let key = self.expr(&m.expression)?;
                let key_reg = self.alloc_reg();
                if key != key_reg {
                    self.emit(Instr::Move { dst: key_reg, src: key });
                }
                let emit_get = |s: &mut Self, d: Reg| match pid {
                    Some(p) => s.emit(Instr::SuperGetComputed { dst: d, home_class_id: p, key: key_reg }),
                    None => s.emit(Instr::SuperGetObjComputed { dst: d, key: key_reg }),
                };
                let emit_set = |s: &mut Self, v: Reg| match pid {
                    Some(p) => s.emit(Instr::SuperSetComputed { home_class_id: p, key: key_reg, val: v }),
                    None => s.emit(Instr::SuperSetObjComputed { key: key_reg, val: v }),
                };
                if is_logical {
                    emit_get(self, dst);
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    emit_set(self, dst);
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    emit_set(self, dst);
                } else {
                    let cur = self.temp();
                    emit_get(self, cur);
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    emit_set(self, dst);
                }
                return Ok(dst);
            }
            ox::AssignmentTarget::ComputedMemberExpression(m) => {
                let obj = self.expr(&m.object)?; // evaluate receiver + key once
                let key = self.expr(&m.expression)?;
                if is_logical {
                    // A read-modify-write reuses the SAME property key for the load
                    // and the store: coerce ToPropertyKey ONCE (its toString/valueOf
                    // must not run twice).
                    let keyk = self.temp();
                    self.emit(Instr::ToPropKey { dst: keyk, obj, src: key });
                    self.emit(Instr::GetIndex { dst, obj, key: keyk });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SetIndex { obj, key: keyk, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    // A plain store coerces the key once (the single SetIndex).
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SetIndex { obj, key, val: dst });
                } else {
                    let keyk = self.temp();
                    self.emit(Instr::ToPropKey { dst: keyk, obj, src: key });
                    let cur = self.temp();
                    self.emit(Instr::GetIndex { dst: cur, obj, key: keyk });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SetIndex { obj, key: keyk, val: dst });
                }
                return Ok(dst);
            }
            // Destructuring assignment to existing targets: `[a,b]=arr`, `({x}=o)`.
            ox::AssignmentTarget::ArrayAssignmentTarget(_)
            | ox::AssignmentTarget::ObjectAssignmentTarget(_) => {
                let src = self.expr_into(&a.right, dst)?;
                if src != dst {
                    self.emit(Instr::Move { dst, src });
                }
                self.assign_target(&a.left, dst)?;
                return Ok(dst);
            }
            _ => {}
        }
        let (name, lhs_covered) = match &a.left {
            ox::AssignmentTarget::AssignmentTargetIdentifier(id) => (
                id.name.to_string(),
                // oxc strips cover parens but keeps spans: a target starting
                // AFTER the assignment expression was parenthesized — not an
                // IdentifierRef, so NamedEvaluation does not apply.
                id.span.start > a.span.start,
            ),
            _ => return Err("assignment to non-identifier not in zipp-vm v1".into()),
        };
        // Strict mode: assignment to `eval`/`arguments` is an early SyntaxError.
        strict_name_err(self.cx.in_strict, &name)?;
        // Inside a `with`, an assignment target may be a property of an active
        // with-object (innermost first), else the static binding.
        let with_objs = self.with_obj_regs(&name);
        if !with_objs.is_empty() {
            return self.assign_with(a, &name, &with_objs, dst);
        }
        let binding = self.resolve(&name);
        match a.operator {
            Op::Assign => {
                // `x = function(){}` / `x = class {}` names the anonymous value
                // after the target (NamedEvaluation), like a declaration.
                // A const local takes the store_binding path so the RHS is evaluated
                // (side effects) and the assignment then throws a TypeError.
                if let Binding::Local(r) = binding {
                    if !self.const_regs.contains(&r) && !self.is_self_name_reg(r) {
                        // Plain mutable local: evaluate the RHS directly into its
                        // reg, saving a Move — but ONLY when the RHS finishes
                        // reading its operands before anything is written to the
                        // destination. An object literal and a template literal
                        // both materialise into the destination FIRST and fill it
                        // in afterwards, so compiling `e = { v: e }` in place makes
                        // the property read the half-built object instead of `e`'s
                        // old value (and `` e = `${e}` `` splice its own accumulator).
                        // React's minified `createContext` is exactly this shape —
                        // `e = {_currentValue: e, …}` — so getting it wrong makes
                        // every context self-referential.
                        let into = if builds_into_dst_incrementally(&a.right) {
                            self.temp()
                        } else {
                            r
                        };
                        let v = if lhs_covered {
                            self.expr_into(&a.right, into)?
                        } else {
                            self.compile_named_init(into, &a.right, &name)?
                        };
                        if v != r {
                            self.emit(Instr::Move { dst: r, src: v });
                        }
                        if r != dst {
                            self.emit(Instr::Move { dst, src: r });
                        }
                        return Ok(dst);
                    }
                }
                // Cell / upvalue / global / const-local: evaluate into dst, store.
                // The target reference resolves BEFORE the RHS (a direct eval
                // there may introduce a shadowing `var` — snapshot first).
                let save_p = self.next_reg;
                let snap = self.eval_snap_probe(&binding);
                let v = if lhs_covered {
                    self.expr_into(&a.right, dst)?
                } else {
                    self.compile_named_init(dst, &a.right, &name)?
                };
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.store_binding_snapped(&binding, dst, snap);
                self.next_reg = save_p;
                Ok(dst)
            }
            // Logical assignment: `x ||= y` / `x &&= y` / `x ??= y` only assign
            // `y` when the short-circuit condition holds (truthy-skip for ||=,
            // falsy-skip for &&=, non-nullish-skip for ??=).
            Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish => {
                let save_p = self.next_reg;
                let snap = self.eval_snap_probe(&binding);
                let cur = self.load_binding(&binding, dst);
                if cur != dst {
                    self.emit(Instr::Move { dst, src: cur });
                }
                let j = self.emit_logical_skip(a.operator, dst);
                // NamedEvaluation: `x ||= function(){}` / `&&=` / `??=` names the
                // anonymous fn/arrow/class after the identifier LHS (IsIdentifierRef),
                // matching plain `=`. `compile_named_init` no-ops to `expr_into` for a
                // non-anonymous RHS, so a named/expression RHS is unaffected.
                let v = self.compile_named_init(dst, &a.right, &name)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.store_binding_snapped(&binding, dst, snap);
                let end = self.here();
                self.patch_jump(j, end);
                self.next_reg = save_p;
                Ok(dst)
            }
            // Arithmetic / bitwise compound assignment (`+= -= *= /= %= **= <<=
            // >>= >>>= |= ^= &=`).
            other => {
                if let Binding::Local(r) = binding {
                    if !self.const_regs.contains(&r) && !self.is_self_name_reg(r) {
                        // Plain mutable local: compute in place.
                        let rhs = self.expr(&a.right)?;
                        let instr = compound_assign_instr(other, r, r, rhs)
                            .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                        self.emit(instr);
                        if r != dst {
                            self.emit(Instr::Move { dst, src: r });
                        }
                        return Ok(dst);
                    }
                }
                // Cell / upvalue / global / const-local: load current → dst, compute
                // → dst, store back through the binding (store_binding throws for a
                // const, after the RHS + arithmetic side effects). The reference
                // is resolved (snapshotted) before the RHS, like plain `=`.
                let save_p = self.next_reg;
                let snap = self.eval_snap_probe(&binding);
                let cur = self.load_binding(&binding, dst); // == dst
                let rhs = self.expr(&a.right)?;
                let instr = compound_assign_instr(other, dst, cur, rhs)
                    .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                self.emit(instr);
                self.store_binding_snapped(&binding, dst, snap);
                self.next_reg = save_p;
                Ok(dst)
            }
        }
    }

    /// Assignment to a plain identifier inside a `with` body, where `objs`
    /// (innermost first) may shadow the static binding. Mirrors the identifier
    /// branch of `assign`, routing the read/write through `load_with`/`store_with`.
    pub(crate) fn assign_with(
        &mut self,
        a: &ox::AssignmentExpression,
        name: &str,
        objs: &[Reg],
        dst: Reg,
    ) -> R<Reg> {
        use ox::AssignmentOperator as Op;
        match a.operator {
            Op::Assign => {
                // The REFERENCE resolves before the RHS runs (which with-object,
                // if any, holds the binding); PutValue writes through that
                // snapshot even if the RHS deletes the with-object property.
                let (found, tgt) = self.emit_with_probe(name, objs);
                let v = self.compile_named_init(dst, &a.right, name)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.emit_with_rmw_write(name, found, tgt, dst);
                Ok(dst)
            }
            Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish => {
                // Resolve the reference ONCE (which with-object, if any, holds the
                // binding), then read and write through that same target — even if
                // a getter run by the read mutates the object meanwhile.
                let (found, tgt) = self.emit_with_probe(name, objs);
                self.emit_with_rmw_read(name, found, tgt, dst);
                let j = self.emit_logical_skip(a.operator, dst);
                let v = self.compile_named_init(dst, &a.right, name)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.emit_with_rmw_write(name, found, tgt, dst);
                let end = self.here();
                self.patch_jump(j, end);
                Ok(dst)
            }
            other => {
                // Compound `x op= y` in a `with`: PutValue reuses the Reference from
                // GetValue, so the write targets the same object the read used even
                // when the getter deletes/replaces the property in between.
                let (found, tgt) = self.emit_with_probe(name, objs);
                self.emit_with_rmw_read(name, found, tgt, dst); // current value → dst
                let rhs = self.expr(&a.right)?;
                let instr = compound_assign_instr(other, dst, dst, rhs)
                    .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                self.emit(instr);
                self.emit_with_rmw_write(name, found, tgt, dst);
                Ok(dst)
            }
        }
    }

    /// Emit a runtime `with`-target probe for a read-modify-write of `name`: find
    /// the innermost with-object that HAS the binding, recording whether one
    /// matched (`found`, a bool reg) and which it is (`tgt`). The reference is
    /// resolved ONCE so a later read and write target the SAME object even if a
    /// getter mutates that object's properties in between (spec: PutValue reuses
    /// the Reference produced by reference resolution). The two returned registers
    /// stay live across the caller's read, RHS evaluation, and write.
    pub(crate) fn emit_with_probe(&mut self, name: &str, objs: &[Reg]) -> (Reg, Reg) {
        let nidx = self.string_name(name);
        let found = self.alloc_reg();
        let tgt = self.alloc_reg();
        self.emit(Instr::LoadBool { dst: found, val: false });
        self.emit(Instr::LoadUndefined { dst: tgt });
        let mut done = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1; // reclaim the flag temp
            self.emit(Instr::LoadBool { dst: found, val: true });
            self.emit(Instr::Move { dst: tgt, src: obj });
            let jd = self.here();
            self.emit(Instr::Jump { target: 0 });
            done.push(jd);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        let end = self.here();
        for jd in done {
            self.patch_jump(jd, end);
        }
        (found, tgt)
    }

    /// Read `name` for a with read-modify-write: `dst = tgt.name` when a with-object
    /// matched (`found`), else read the static (lexical/global) binding into `dst`.
    pub(crate) fn emit_with_rmw_read(&mut self, name: &str, found: Reg, tgt: Reg, dst: Reg) {
        let nidx = self.string_name(name);
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond: found, target: 0 }); // → static read
        // GetBindingValue: HasProperty AGAIN, then Get (both observable
        // through Proxy traps) — not a bare [[Get]].
        self.emit(Instr::WithGet { dst, obj: tgt, name: nidx, strict: self.cx.in_strict });
        let je = self.here();
        self.emit(Instr::Jump { target: 0 });
        let stat = self.here();
        self.patch_jump(jf, stat);
        let b = self.resolve(name);
        let r = self.load_binding(&b, dst);
        if r != dst {
            self.emit(Instr::Move { dst, src: r });
        }
        let end = self.here();
        self.patch_jump(je, end);
    }

    /// Write `src` to `name` for a with read-modify-write: `tgt.name = src` when a
    /// with-object matched (`found`), else store to the static binding.
    pub(crate) fn emit_with_rmw_write(&mut self, name: &str, found: Reg, tgt: Reg, src: Reg) {
        let nidx = self.string_name(name);
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond: found, target: 0 }); // → static write
        // SetMutableBinding re-checks HasProperty (the binding may have been
        // DELETED since the reference resolved): strict throws, sloppy does
        // not silently recreate the property on the with-object.
        self.emit(Instr::WithSet { obj: tgt, name: nidx, val: src, strict: self.cx.in_strict });
        let je = self.here();
        self.emit(Instr::Jump { target: 0 });
        let stat = self.here();
        self.patch_jump(jf, stat);
        let b = self.resolve(name);
        self.store_binding(&b, src);
        let end = self.here();
        self.patch_jump(je, end);
    }

}
