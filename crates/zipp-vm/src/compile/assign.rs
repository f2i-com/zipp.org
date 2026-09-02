// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

use crate::parse::ast::{
    self, AssignOp, CallExpr, Expr, MemberProp, PropKey, Target, TargetElem, TargetProp,
};

/// `ZIPP_NO_CONCAT_PAIR_FUSE=1` restores identifier `x += (b + c)` to the
/// historical inner `Add` followed by the outer `Add`, byte-for-byte.  The
/// switch is compiler-side because the fused instruction is a lowering, not a
/// speculative runtime shortcut.
#[inline]
fn concat_pair_fuse_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_CONCAT_PAIR_FUSE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// A syntactic producer whose result is unconditionally a primitive String.
/// Keep this deliberately narrow: the two benchmark shapes are a literal and
/// a conditional with string-producing arms.  A template is also exact by
/// construction.  Calls, identifiers, logical expressions and overloaded `+`
/// are not inferred from surrounding types.
fn definitely_string(e: &Expr) -> bool {
    match e {
        Expr::Str(_) | Expr::Template(_) => true,
        Expr::Cond { cons, alt, .. } => definitely_string(cons) && definitely_string(alt),
        _ => false,
    }
}

/// The RHS pair accepted by `AddRightPair`: exactly `b + c`, with a
/// definitely-string left operand.  That proof guarantees the INNER `+` takes
/// its string-concatenation branch after `c`'s ToPrimitive, but the runtime op
/// still retains the full two-Add fallback for adversarial values/coercions.
fn add_right_pair_parts(value: &Expr) -> Option<(&Expr, &Expr)> {
    match value {
        Expr::Binary {
            op: ast::BinaryOp::Add,
            left,
            right,
        } if definitely_string(left) => Some((left, right)),
        _ => None,
    }
}

// NOTE: signatures this file assumes of functions owned by other groups. Each
// follows mechanically from the type mapping; if one lands differently, these
// are the only call sites to adjust.
//
//   fn concat_key_literal_prefix(key: &Expr) -> Option<(&str, &Expr)>
//       — `&str` rather than `&StrVal` because it is only ever fed to
//         `string_name`, and the helper already excludes a lone-surrogate
//         literal (now `StrVal::Utf16`), so the surviving case IS `&str`.
//   fn class_key_name(key: &PropKey) -> R<String>
//   fn compound_assign_instr(op: AssignOp, dst: Reg, a: Reg, b: Reg) -> Option<Instr>
//   FnCompiler::extract_member(&mut self, obj: Reg, key: &PropKey, dst: Reg) -> R<()>
//       — the `computed: bool` parameter is gone: `PropKey::Computed` is a
//         variant now, so the flag can no longer disagree with the key.
//   FnCompiler::expr(&mut self, e: &Expr) -> R<Reg>
//   FnCompiler::expr_into(&mut self, e: &Expr, dst: Reg) -> R<Reg>
//   FnCompiler::compile_named_init(&mut self, dst: Reg, init: &Expr, name: &str) -> R<Reg>
//   FnCompiler::apply_default_in_place_named(&mut self, reg: Reg, default: &Expr,
//                                            name: Option<&str>) -> R<()>
//   FnCompiler::call(&mut self, c: &CallExpr, dst: Reg) -> R<Reg>
//
// `string_name` / `add_string_const` keep `&str`: a property key is a Rust
// `String` engine-wide (`ObjMap.keys`), so nothing here wants a `StrVal`.
//
// NOTE: `assign` is now called as `self.assign(*op, target, value, dst)` from
// the `Expr::Assign { op, target, value }` arm of `expr_into` — the struct node
// is gone and its three fields arrive separately.

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
fn builds_into_dst_incrementally(e: &Expr, target: &str) -> bool {
    match e {
        Expr::Object(..) => true,
        // A template with no interpolations is a plain constant string.
        Expr::Template(t) => !t.exprs.is_empty(),
        Expr::Cond { cons, alt, .. } => {
            builds_into_dst_incrementally(cons, target)
                || builds_into_dst_incrementally(alt, target)
        }
        Expr::Logical { left, right, .. } => {
            // A logical stores the LEFT value in the destination and only then
            // evaluates the right, so a right operand that reads the target sees
            // the left value instead of the target's old one.
            //
            // `s = m.get(s) || s` — look it up, else keep what you had — is one
            // of the most common idioms there is, and in place it always yielded
            // the miss: `m.get(s)` wrote undefined over `s`, then `|| s` read
            // that undefined back. React resolves every attribute alias this
            // way, so each `aria-*` prop reached the DOM with an undefined NAME.
            crate::capture::references_name(right, target)
                || builds_into_dst_incrementally(left, target)
                || builds_into_dst_incrementally(right, target)
        }
        Expr::Seq(exprs) => exprs
            .last()
            .is_some_and(|e| builds_into_dst_incrementally(e, target)),
        // NOTE: the former `ParenthesizedExpression` arm is gone — the AST has
        // no such node. Parenthesisation is observable only on an assignment
        // TARGET (`Target::Ident { covered }`), never on the value side, so
        // nothing is lost: a parenthesised object literal reaches the `Object`
        // arm directly instead of through a peel.
        Expr::Assign {
            target: tgt, value, ..
        } => {
            // All three member shapes (`a.b`, `a[b]`, `a.#b`) are one node now.
            matches!(tgt, Target::Member(_)) || builds_into_dst_incrementally(value, target)
        }
        _ => false,
    }
}

impl<'a> FnCompiler<'a> {
    /// Assign `src` to a destructuring-assignment target (existing binding or
    /// member, or a nested array/object pattern). Counterpart to `extract_pattern`
    /// for `=` targets that aren't declarations.
    pub(crate) fn assign_target(&mut self, target: &Target, src: Reg) -> R<()> {
        match target {
            Target::Ident { name, .. } => {
                // `src` is already computed, so the TDZ throw lands exactly where
                // PutValue would — see `emit_tdz_store_throw`.
                if self.emit_tdz_store_throw(name) {
                    return Ok(());
                }
                let b = self.resolve(name);
                self.store_binding(&b, src);
                Ok(())
            }
            // `for (super.x of it)` / `[...super.x] = it` — a super target that no
            // caller pre-resolved. `m.object` is `Expr::Super`, which has no value
            // form, so this must not reach the generic `self.expr(&m.object)` arms.
            Target::Member(m) if matches!(&m.object, Expr::Super) => {
                self.assign_super_target(target, None, src)
            }
            Target::Member(m) => match &m.prop {
                MemberProp::Ident(prop) => {
                    let save = self.next_reg;
                    let obj = self.recv_expr(&m.object)?;
                    let name = self.string_name(prop);
                    self.emit(Instr::SetProp {
                        obj,
                        name,
                        val: src,
                        strict: self.cx.strict_expr_region > 0,
                    });
                    self.set_next_reg(save);
                    Ok(())
                }
                MemberProp::Computed(key_expr) => {
                    let save = self.next_reg;
                    let obj = self.recv_expr(&m.object)?;
                    // Fuse `obj[<plain string literal> + e] = v` → SetIndexConcat
                    // (no throwaway concat-key allocation; see GetIndexConcat).
                    if let Some((name, rhs)) = concat_key_literal_prefix(key_expr) {
                        let nidx = self.string_name(name);
                        let key = self.expr(rhs)?;
                        self.emit(Instr::SetIndexConcat {
                            obj,
                            name: nidx,
                            key,
                            val: src,
                        });
                        self.set_next_reg(save);
                        return Ok(());
                    }
                    let key = self.expr(key_expr)?;
                    self.emit(Instr::SetIndex { obj, key, val: src });
                    self.set_next_reg(save);
                    Ok(())
                }
                MemberProp::Private(field) => {
                    // `[this.#x] = arr` / `({a: this.#x} = o)`: a private field as a
                    // destructuring target — brand-checked PrivateSet (the target
                    // reference is taken before the value per the destructuring driver).
                    self.check_private_declared(field)?;
                    let save = self.next_reg;
                    let obj = self.recv_expr(&m.object)?;
                    let name = self.string_name(&private_key(field));
                    self.emit(Instr::SetPrivate {
                        obj,
                        name,
                        val: src,
                    });
                    self.set_next_reg(save);
                    Ok(())
                }
            },
            Target::Array(elems) => self.assign_array_target(elems, src),
            Target::Object { props, rest } => {
                self.assign_object_target(props, rest.as_deref(), src)
            }
            Target::Call(c) => self.assign_call_target(c),
        }
    }

    /// Annex B "Runtime Errors for Function Call Assignment Targets": in SLOPPY
    /// code `AssignmentTargetType(CallExpression)` is *simple*, so `f() = 1`
    /// parses, the call is EVALUATED, and a ReferenceError is thrown at runtime —
    /// before the RHS, before any old-value read, and before any ToNumeric
    /// coercion. The parser refuses to build this in strict code, so there is
    /// nothing to re-check here.
    ///
    /// NOTE: this replaces `crate::annexb_call_target_rewrite`'s source rewrite
    /// (`f(…)` → `((f(…)), __zipp_annexb_ref_error__())[0]`) with the same
    /// observable behaviour emitted directly. It is UNREACHABLE through
    /// `parse::oxc_bridge` — oxc cannot represent a call as an assignment target —
    /// so it cannot move the byte-identical-bytecode gate; it goes live only with
    /// the hand-written parser. Error kind 4 is ReferenceError
    /// (`vm::native::ERROR_NAMES`).
    pub(crate) fn assign_call_target(&mut self, c: &CallExpr) -> R<()> {
        let save = self.next_reg;
        let r = self.alloc_reg();
        self.call(c, r)?;
        let e = self.alloc_reg();
        self.emit(Instr::NewError {
            dst: e,
            kind: 4,
            arg: None,
            opts: None,
            errors: None,
        });
        self.emit(Instr::Throw { src: e });
        self.set_next_reg(save);
        Ok(())
    }

    /// A destructuring MEMBER target's reference, evaluated BEFORE the source
    /// property read (KeyedDestructuringAssignmentEvaluation: the
    /// DestructuringAssignmentTarget reference comes first, then GetV).
    ///
    /// Takes the bare target: a `= default` is a SIBLING field on
    /// `TargetElem`/`TargetProp` now, not a wrapper node, so there is nothing to
    /// unwrap and the former "flattened member variants" duplicate arms collapse
    /// into one.
    pub(crate) fn pre_member_ref(&mut self, t: &Target) -> R<Option<(Reg, PreKey)>> {
        match t {
            // `[super.x] = it` / `({k: super[e]} = o)`. MakeSuperPropertyReference
            // runs GetThisBinding() (a derived ctor before `super()` throws HERE)
            // and evaluates the computed key when the REFERENCE is taken — i.e.
            // before the iterator step / GetV, which is what sm/destructuring/
            // order-super.js logs. The base object is not materialised: the
            // Super* opcodes read [[HomeObject]] themselves.
            Target::Member(m) if matches!(&m.object, Expr::Super) => {
                if !self.super_prop_ok() {
                    return Err("`super.x = …` is only valid in a method".into());
                }
                self.this_check();
                match &m.prop {
                    MemberProp::Ident(prop) => {
                        let name = self.string_name(prop);
                        Ok(Some((0, PreKey::Super(name))))
                    }
                    MemberProp::Computed(key_expr) => {
                        let k = self.pin_expr(key_expr)?;
                        Ok(Some((0, PreKey::SuperComputed(k))))
                    }
                    // `super.#x` is not in the grammar.
                    MemberProp::Private(_) => {
                        Err("`super.#x` is not a valid assignment target".into())
                    }
                }
            }
            Target::Member(m) => match &m.prop {
                MemberProp::Ident(prop) => {
                    let r = self.pin_expr(&m.object)?;
                    let name = self.string_name(prop);
                    Ok(Some((r, PreKey::Static(name))))
                }
                MemberProp::Computed(key_expr) => {
                    let r = self.pin_expr(&m.object)?;
                    let k = self.pin_expr(key_expr)?;
                    Ok(Some((r, PreKey::Computed(k))))
                }
                MemberProp::Private(field) => {
                    self.check_private_declared(field)?;
                    let r = self.pin_expr(&m.object)?;
                    let name = self.string_name(&private_key(field));
                    Ok(Some((r, PreKey::Private(name))))
                }
            },
            // NOTE: `Target::Call` falls here, so an Annex B call target in a
            // destructuring position is evaluated (and throws) at STORE time via
            // `assign_target`, not up front. That matches today's rewrite, whose
            // `((f()), ref_error())[0]` base is itself a member reference the
            // compiler cannot pre-resolve either.
            _ => Ok(None),
        }
    }

    /// Evaluate `e` into a PINNED register (survives later evaluation; the
    /// caller's next_reg reset reclaims it).
    pub(crate) fn pin_expr(&mut self, e: &Expr) -> R<Reg> {
        let r = self.alloc_reg();
        let v = self.expr_into(e, r)?;
        if v != r {
            self.emit(Instr::Move { dst: r, src: v });
        }
        Ok(r)
    }

    /// Store through a reference produced by `pre_member_ref`, applying any
    /// `= default` first (no NamedEvaluation — the target is a member).
    pub(crate) fn store_pre_ref(
        &mut self,
        default: Option<&Expr>,
        obj: Reg,
        key: &PreKey,
        val: Reg,
    ) -> R<()> {
        if let Some(init) = default {
            self.apply_default_in_place_named(val, init, None)?;
        }
        match *key {
            PreKey::Static(name) => self.emit(Instr::SetProp {
                obj,
                name,
                val,
                strict: self.cx.strict_expr_region > 0,
            }),
            PreKey::Computed(k) => self.emit(Instr::SetIndex { obj, key: k, val }),
            PreKey::Private(name) => self.emit(Instr::SetPrivate { obj, name, val }),
            // `obj` is the dummy from pre_member_ref — a SuperProperty store
            // routes through the class op (home_class_id) or the object-method
            // op ([[HomeObject]]) exactly as `super.x = v` does in `assign`.
            PreKey::Super(name) => match self.super_class {
                Some(p) => {
                    let b = self.temp();
                    self.emit(Instr::SuperBase {
                        dst: b,
                        home_class_id: p,
                    });
                    self.emit(Instr::SuperSet {
                        base: b,
                        home_class_id: p,
                        name,
                        val,
                    })
                }
                None => self.emit(Instr::SuperSetObj { name, val }),
            },
            PreKey::SuperComputed(k) => match self.super_class {
                Some(p) => {
                    let b = self.temp();
                    self.emit(Instr::SuperBase {
                        dst: b,
                        home_class_id: p,
                    });
                    self.emit(Instr::SuperSetComputed {
                        base: b,
                        home_class_id: p,
                        key: k,
                        val,
                    })
                }
                None => self.emit(Instr::SuperSetObjComputed { key: k, val }),
            },
        }
        Ok(())
    }

    /// True when a SuperProperty reference is legal here: inside a class method
    /// (`super_class` names the home class) or an object method with a
    /// [[HomeObject]].
    pub(crate) fn super_prop_ok(&self) -> bool {
        self.super_class.is_some() || self.super_home_obj
    }

    /// Take a `super.x` / `super[k]` reference and store `src` through it right
    /// away — the shape every destructuring position that does NOT pre-take the
    /// reference needs (`for (super.x of it)`, `[...super.x] = it`, a rest
    /// target). Kept next to `store_pre_ref` because it is the same store.
    pub(crate) fn assign_super_target(
        &mut self,
        target: &Target,
        default: Option<&Expr>,
        src: Reg,
    ) -> R<()> {
        let save = self.next_reg;
        let (obj, key) = self
            .pre_member_ref(target)?
            .ok_or("`super` assignment target lost its reference")?;
        self.store_pre_ref(default, obj, &key, src)?;
        self.set_next_reg(save);
        Ok(())
    }

    /// One element of a destructuring assignment, applying its `= default` first.
    pub(crate) fn assign_maybe_default(
        &mut self,
        target: &Target,
        default: Option<&Expr>,
        val: Reg,
    ) -> R<()> {
        if let Some(init) = default {
            // `[a = function(){}] = arr` ⇒ the default function takes name "a".
            let name = match target {
                Target::Ident { name, .. } => Some(name.to_string()),
                _ => None,
            };
            self.apply_default_in_place_named(val, init, name.as_deref())?;
            return self.assign_target(target, val);
        }
        // NOTE: the no-default member arms are deliberately NOT `assign_target`.
        // That path fuses `obj[<string literal> + e] = v` into SetIndexConcat;
        // this one never has, and the gate is byte-identical bytecode.
        match target {
            Target::Ident { name, .. } => {
                if self.emit_tdz_store_throw(name) {
                    return Ok(());
                }
                let b = self.resolve(name);
                self.store_binding(&b, val);
                Ok(())
            }
            // See `assign_target`: `Expr::Super` has no value form.
            Target::Member(m) if matches!(&m.object, Expr::Super) => {
                self.assign_super_target(target, None, val)
            }
            Target::Member(m) => match &m.prop {
                MemberProp::Ident(prop) => {
                    let save = self.next_reg;
                    let obj = self.recv_expr(&m.object)?;
                    let name = self.string_name(prop);
                    self.emit(Instr::SetProp {
                        obj,
                        name,
                        val,
                        strict: self.cx.strict_expr_region > 0,
                    });
                    self.set_next_reg(save);
                    Ok(())
                }
                MemberProp::Computed(key_expr) => {
                    let save = self.next_reg;
                    let obj = self.recv_expr(&m.object)?;
                    let key = self.expr(key_expr)?;
                    self.emit(Instr::SetIndex { obj, key, val });
                    self.set_next_reg(save);
                    Ok(())
                }
                MemberProp::Private(field) => {
                    self.check_private_declared(field)?;
                    let save = self.next_reg;
                    let obj = self.recv_expr(&m.object)?;
                    let name = self.string_name(&private_key(field));
                    self.emit(Instr::SetPrivate { obj, name, val });
                    self.set_next_reg(save);
                    Ok(())
                }
            },
            Target::Array(elems) => self.assign_array_target(elems, val),
            Target::Object { props, rest } => {
                self.assign_object_target(props, rest.as_deref(), val)
            }
            Target::Call(c) => self.assign_call_target(c),
        }
    }

    pub(crate) fn assign_array_target(&mut self, els: &[Option<TargetElem>], src_in: Reg) -> R<()> {
        // Array assignment destructuring: the SPEC's stepwise iterator driver.
        // Per element: evaluate a member target's REFERENCE first, then
        // IteratorStep (no step once exhausted), then the default, then store
        // through the saved reference. An abrupt completion anywhere closes a
        // non-exhausted iterator QUIETLY (the original throw wins); a normal
        // completion closes STRICTLY (a throwing/non-object return() result
        // propagates).
        //
        // `...rest` is the LAST entry of `els` with `rest: true` (the grammar
        // allows nothing after it), where oxc used to park it in a sibling
        // field. Split it back out so the element pass and the rest pass stay
        // the two loops they were.
        let rest: Option<&TargetElem> = match els.last() {
            Some(Some(t)) if t.rest => Some(t),
            _ => None,
        };
        let elements: &[Option<TargetElem>] = if rest.is_some() {
            &els[..els.len() - 1]
        } else {
            els
        };
        let save_top = self.next_reg;
        let iter_reg = self.alloc_reg();
        // GetIterator alone: it now raises the "not iterable" TypeError itself, so
        // the old `CheckIterable` prefix is gone — it did a SECOND observable
        // `Get(@@iterator)` on the RHS (sm/destructuring/order.js asserts exactly
        // one, and sm/destructuring/order-super.js depends on the same log).
        self.emit(Instr::GetIterator {
            dst: iter_reg,
            src: src_in,
        });
        // GetIteratorFromMethod step 3 reads `next` ONCE, as part of building the
        // iterator record — i.e. BEFORE the first element's target reference is
        // evaluated. Priming it here (as for-of already does) both fixes that
        // order and stops a mid-pattern redefinition of `iterator.next` from
        // being observed.
        let next_reg = self.alloc_reg();
        self.emit(Instr::IterPrime {
            dst: next_reg,
            iter: iter_reg,
        });
        let idx_reg = self.alloc_reg();
        self.emit(Instr::LoadInt {
            dst: idx_reg,
            val: 0,
        });
        let done = self.alloc_reg();
        self.emit(Instr::LoadBool {
            dst: done,
            val: false,
        });
        // FINALLY-kind protection (not catch): a `yield` inside the pattern
        // can suspend the generator with this iterator OPEN — a later
        // `.return()` injects a RETURN completion that unwinds through
        // finally handlers only, and IteratorClose must run then too.
        let kind_reg = self.alloc_reg();
        let val_reg = self.alloc_reg();
        let push_at = self.here();
        self.emit(Instr::PushFinally {
            target: 0,
            kind_reg,
            val_reg,
        });
        self.handler_depth += 1;
        for el in elements {
            let save = self.next_reg;
            let pre = match el {
                Some(te) => self.pre_member_ref(&te.target)?,
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
            self.emit(Instr::JumpIfTrue {
                cond: done,
                target: 0,
            });
            self.emit(Instr::LoadBool {
                dst: done,
                val: true,
            });
            self.emit(Instr::IterNext {
                value_dst: val,
                done_dst: dflag,
                iter: iter_reg,
                idx: idx_reg,
                next: next_reg,
            });
            let jexh = self.here();
            self.emit(Instr::JumpIfTrue {
                cond: dflag,
                target: 0,
            });
            self.emit(Instr::LoadBool {
                dst: done,
                val: false,
            });
            let jgot = self.here();
            self.emit(Instr::Jump { target: 0 });
            let at_undef = self.here();
            self.patch_jump(jdone, at_undef);
            self.patch_jump(jexh, at_undef);
            self.emit(Instr::LoadUndefined { dst: val });
            let got = self.here();
            self.patch_jump(jgot, got);
            if let Some(te) = el {
                match pre {
                    Some((obj, key)) => self.store_pre_ref(te.default.as_ref(), obj, &key, val)?,
                    None => self.assign_maybe_default(&te.target, te.default.as_ref(), val)?,
                }
            }
            self.set_next_reg(save);
        }
        if let Some(rest) = rest {
            let save = self.next_reg;
            let pre = self.pre_rest_ref(&rest.target)?;
            let out = self.alloc_reg();
            self.emit(Instr::ArrayCtor {
                dst: out,
                callee: None,
                arg_base: 0,
                argc: 0,
                is_construct: false,
            });
            let v = self.alloc_reg();
            let dflag = self.alloc_reg();
            let loop_top = self.here();
            let jrest_done = self.here();
            self.emit(Instr::JumpIfTrue {
                cond: done,
                target: 0,
            });
            self.emit(Instr::LoadBool {
                dst: done,
                val: true,
            });
            self.emit(Instr::IterNext {
                value_dst: v,
                done_dst: dflag,
                iter: iter_reg,
                idx: idx_reg,
                next: next_reg,
            });
            let jout = self.here();
            self.emit(Instr::JumpIfTrue {
                cond: dflag,
                target: 0,
            });
            self.emit(Instr::LoadBool {
                dst: done,
                val: false,
            });
            self.emit(Instr::ArrayAppend {
                arr: out,
                val: v,
                spread: false,
            });
            self.emit(Instr::Jump { target: loop_top });
            let rest_done = self.here();
            self.patch_jump(jrest_done, rest_done);
            self.patch_jump(jout, rest_done);
            match pre {
                // A rest target never carries a `= default` (the grammar forbids
                // it), so `store_pre_ref(None, …)` is exactly the store the three
                // inline arms used to be — and it also covers `[...super.x] = it`.
                Some((obj, key)) => self.store_pre_ref(None, obj, &key, out)?,
                None => self.assign_target(&rest.target, out)?,
            }
            self.set_next_reg(save);
        }
        self.emit(Instr::PopFinally);
        self.handler_depth -= 1;
        // Normal completion: close iff not exhausted (strict result checks).
        let jskip = self.here();
        self.emit(Instr::JumpIfTrue {
            cond: done,
            target: 0,
        });
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
        self.emit(Instr::JumpIfTrue {
            cond: done,
            target: 0,
        });
        let two = self.alloc_reg();
        self.emit(Instr::LoadInt { dst: two, val: 2 });
        let isthrow = self.alloc_reg();
        self.emit(Instr::Eq {
            dst: isthrow,
            a: kind_reg,
            b: two,
        });
        let jnotthrow = self.here();
        self.emit(Instr::JumpIfFalse {
            cond: isthrow,
            target: 0,
        });
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
        self.set_next_reg(save_top);
        Ok(())
    }

    /// `pre_member_ref` for a REST target.
    ///
    /// NOTE: identical to `pre_member_ref` now — the two differed only because
    /// oxc gave a rest target the bare `AssignmentTarget` type while an element
    /// arrived wrapped in `AssignmentTargetMaybeDefault`. Both are a `Target`
    /// here, so the name is kept (it says which caller it serves) but the body
    /// delegates.
    pub(crate) fn pre_rest_ref(&mut self, t: &Target) -> R<Option<(Reg, PreKey)>> {
        self.pre_member_ref(t)
    }

    pub(crate) fn assign_object_target(
        &mut self,
        props: &[TargetProp],
        rest: Option<&Target>,
        src: Reg,
    ) -> R<()> {
        // RequireObjectCoercible(src) for an empty pattern (`({} = x)` /
        // `({...rest} = x)`) — no member access would otherwise guard null/undefined.
        if props.is_empty() {
            self.emit(Instr::CheckCoercible { src });
        }
        // Computed sibling key + `...rest`: evaluate each sibling key once into a
        // contiguous block (reused for extraction and the ObjectRestDyn exclusion).
        // A shorthand property can never be computed, so keying off the PropKey
        // variant is exactly the old `p.computed` test.
        let has_computed =
            rest.is_some() && props.iter().any(|p| matches!(p.key, PropKey::Computed(_)));
        if has_computed {
            let block_save = self.next_reg;
            let keys_base = self.next_reg;
            let n = props.len() as u16;
            for _ in 0..props.len() {
                self.alloc_reg();
            }
            for (i, prop) in props.iter().enumerate() {
                let kreg = keys_base + i as Reg;
                match &prop.key {
                    PropKey::Computed(e) => {
                        let v = self.expr_into(e, kreg)?;
                        if v != kreg {
                            self.emit(Instr::Move { dst: kreg, src: v });
                        }
                    }
                    // Shorthand `{x}` and `{k: t}` land here together: a
                    // shorthand's key is `PropKey::Ident`, and `class_key_name`
                    // of that is the identifier itself — the same string the
                    // shorthand arm used to take from its binding.
                    key => {
                        let name = class_key_name(key)?;
                        let idx = self.add_string_const(&name);
                        self.emit(Instr::LoadConst { dst: kreg, idx });
                    }
                }
            }
            for (i, prop) in props.iter().enumerate() {
                let save = self.next_reg;
                let kreg = keys_base + i as Reg;
                let val = self.alloc_reg();
                self.emit(Instr::GetIndex {
                    dst: val,
                    obj: src,
                    key: kreg,
                });
                match (&prop.target, prop.shorthand) {
                    (Target::Ident { name, .. }, true) => {
                        if let Some(init) = &prop.default {
                            self.apply_default_in_place_named(val, init, Some(&**name))?;
                        }
                        if !self.emit_tdz_store_throw(name) {
                            let b = self.resolve(name);
                            self.store_binding(&b, val);
                        }
                    }
                    _ => {
                        self.assign_maybe_default(&prop.target, prop.default.as_ref(), val)?;
                    }
                }
                self.set_next_reg(save);
            }
            // `has_computed` is only set when there IS a rest target; no unwrap.
            let rest_target = rest.ok_or("object-rest destructuring lost its rest target")?;
            let save = self.next_reg;
            let val = self.alloc_reg();
            self.emit(Instr::ObjectRestDyn {
                dst: val,
                src,
                keys_base,
                n,
            });
            self.assign_target(rest_target, val)?;
            self.set_next_reg(save);
            self.set_next_reg(block_save);
            return Ok(());
        }
        for prop in props {
            let save = self.next_reg;
            match (&prop.target, prop.shorthand) {
                (Target::Ident { name, .. }, true) => {
                    // `({x} = o)` / `({x = d} = o)` — target is the identifier itself.
                    let val = self.alloc_reg();
                    let nidx = self.string_name(name);
                    self.emit(Instr::GetProp {
                        dst: val,
                        obj: src,
                        name: nidx,
                    });
                    if let Some(init) = &prop.default {
                        // `({x = function(){}} = o)` ⇒ default takes the name "x".
                        self.apply_default_in_place_named(val, init, Some(&**name))?;
                    }
                    if !self.emit_tdz_store_throw(name) {
                        let b = self.resolve(name);
                        self.store_binding(&b, val);
                    }
                }
                _ => {
                    // `({key: target} = o)`. For a MEMBER target the spec
                    // order is: PropertyName (incl. ToPropertyKey) -> target
                    // REFERENCE (object + uncoerced key exprs) -> GetV ->
                    // default -> PutValue (target-key coercion at store).
                    // The `= default` is a sibling field now, so the two old
                    // shapes (bare member, member-with-default) are one test.
                    let is_member_target = matches!(&prop.target, Target::Member(_));
                    if is_member_target {
                        let skey: Option<Reg> = if let PropKey::Computed(e) = &prop.key {
                            let raw = self.pin_expr(e)?;
                            let k = self.alloc_reg();
                            self.emit(Instr::ToPropKey {
                                dst: k,
                                obj: src,
                                src: raw,
                            });
                            Some(k)
                        } else {
                            None
                        };
                        let pre = self.pre_member_ref(&prop.target)?;
                        let val = self.alloc_reg();
                        match skey {
                            Some(k) => self.emit(Instr::GetIndex {
                                dst: val,
                                obj: src,
                                key: k,
                            }),
                            None => {
                                let name = class_key_name(&prop.key)?;
                                let nidx = self.string_name(&name);
                                self.emit(Instr::GetProp {
                                    dst: val,
                                    obj: src,
                                    name: nidx,
                                });
                            }
                        }
                        let (obj, key) = pre.expect("member target shape checked above");
                        self.store_pre_ref(prop.default.as_ref(), obj, &key, val)?;
                    } else {
                        let val = self.alloc_reg();
                        self.extract_member(src, &prop.key, val)?;
                        self.assign_maybe_default(&prop.target, prop.default.as_ref(), val)?;
                    }
                }
            }
            self.set_next_reg(save);
        }
        // `({a, ...rest} = o)` — a new object of `src`'s own keys minus the
        // siblings, assigned to the rest target (mirrors the declaration form).
        if let Some(rest_target) = rest {
            let exclude_start = self.string_constants.len() as u32;
            let mut exclude_count = 0u16;
            for prop in props {
                let key = match (&prop.target, prop.shorthand) {
                    (Target::Ident { name, .. }, true) => name.to_string(),
                    _ => class_key_name(&prop.key).map_err(|_| {
                        "object-rest with a computed sibling key is not in the subset"
                    })?,
                };
                self.string_name(&key);
                exclude_count += 1;
            }
            let save = self.next_reg;
            let val = self.alloc_reg();
            self.emit(Instr::ObjectRest {
                dst: val,
                src,
                exclude_start,
                exclude_count,
            });
            self.assign_target(rest_target, val)?;
            self.set_next_reg(save);
        }
        Ok(())
    }

    /// For a logical assignment (`||= &&= ??=`), emit the short-circuit test on
    /// `val` (which already holds the target's current value) and return the ip
    /// of the jump that, when taken, SKIPS the assignment (keeping `val`).
    pub(crate) fn emit_logical_skip(&mut self, op: AssignOp, val: Reg) -> u32 {
        match op {
            AssignOp::LogicalOr => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue {
                    cond: val,
                    target: 0,
                }); // truthy → skip
                j
            }
            AssignOp::LogicalAnd => {
                let j = self.here();
                self.emit(Instr::JumpIfFalse {
                    cond: val,
                    target: 0,
                }); // falsy → skip
                j
            }
            _ => {
                // ??= : skip when `val` is NOT strictly null/undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                let isnull = self.alloc_reg();
                self.emit_is_nullish(val, isnull, undef);
                let j = self.here();
                self.emit(Instr::JumpIfFalse {
                    cond: isnull,
                    target: 0,
                });
                self.set_next_reg(save);
                j
            }
        }
    }

    pub(crate) fn assign(
        &mut self,
        op: AssignOp,
        target: &Target,
        value: &Expr,
        dst: Reg,
    ) -> R<Reg> {
        let is_logical = op.is_logical();
        // Member-target assignment: `obj.x = v` / `arr[i] = v`. Only plain
        // `=` is supported for members in this subset.
        match target {
            Target::Member(m) => match &m.prop {
                // `super.x = v` / `super.x op= v` / `super.x ??= v`.
                MemberProp::Ident(prop) if matches!(&m.object, Expr::Super) => {
                    let pid = self.super_class;
                    if pid.is_none() && !self.super_home_obj {
                        return Err("`super.x = …` is only valid in a method".into());
                    }
                    self.this_check();
                    let name = self.string_name(prop);
                    // MakeSuperPropertyReference captures GetSuperBase BEFORE the
                    // RHS runs (it may retarget the home object's prototype —
                    // superPropOrdering's testAssignProp). Object-method super
                    // resolves its [[HomeObject]] base at the store op instead.
                    let sb = pid.map(|p| {
                        let b = self.temp();
                        self.emit(Instr::SuperBase {
                            dst: b,
                            home_class_id: p,
                        });
                        b
                    });
                    // A super GET/SET routes to the class op (home_class_id) or the
                    // object-method op ([[HomeObject]]), depending on the lexical context.
                    let emit_get = |s: &mut Self, d: Reg| match pid {
                        Some(p) => s.emit(Instr::SuperGet {
                            dst: d,
                            home_class_id: p,
                            name,
                        }),
                        None => s.emit(Instr::SuperGetObj { dst: d, name }),
                    };
                    let emit_set = |s: &mut Self, v: Reg| match (pid, sb) {
                        (Some(p), Some(b)) => s.emit(Instr::SuperSet {
                            base: b,
                            home_class_id: p,
                            name,
                            val: v,
                        }),
                        _ => s.emit(Instr::SuperSetObj { name, val: v }),
                    };
                    if is_logical {
                        emit_get(self, dst);
                        let j = self.emit_logical_skip(op, dst);
                        let v = self.expr_into(value, dst)?;
                        if v != dst {
                            self.emit(Instr::Move { dst, src: v });
                        }
                        emit_set(self, dst);
                        let end = self.here();
                        self.patch_jump(j, end);
                    } else if matches!(op, AssignOp::Assign) {
                        let val = self.expr_into(value, dst)?;
                        if val != dst {
                            self.emit(Instr::Move { dst, src: val });
                        }
                        emit_set(self, dst);
                    } else {
                        let cur = self.temp();
                        emit_get(self, cur);
                        let rhs = self.expr(value)?;
                        let instr = compound_assign_instr(op, dst, cur, rhs)
                            .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                        self.emit(instr);
                        emit_set(self, dst);
                    }
                    return Ok(dst);
                }
                MemberProp::Ident(prop) => {
                    let obj = self.recv_expr(&m.object)?; // evaluate the receiver once
                    let name = self.string_name(prop);
                    if is_logical {
                        // `obj.x ??= v` etc: read current; skip the store on short-circuit.
                        self.emit(Instr::GetProp { dst, obj, name });
                        let j = self.emit_logical_skip(op, dst);
                        let v = self.expr_into(value, dst)?;
                        if v != dst {
                            self.emit(Instr::Move { dst, src: v });
                        }
                        self.emit(Instr::SetProp {
                            obj,
                            name,
                            val: dst,
                            strict: self.cx.strict_expr_region > 0,
                        });
                        let end = self.here();
                        self.patch_jump(j, end);
                    } else if matches!(op, AssignOp::Assign) {
                        let val = self.expr_into(value, dst)?;
                        if val != dst {
                            self.emit(Instr::Move { dst, src: val });
                        }
                        self.emit(Instr::SetProp {
                            obj,
                            name,
                            val: dst,
                            strict: self.cx.strict_expr_region > 0,
                        });
                    } else {
                        // Compound `obj.x op= v`: read obj.x, combine, write back.
                        let cur = self.temp();
                        self.emit(Instr::GetProp {
                            dst: cur,
                            obj,
                            name,
                        });
                        let rhs = self.expr(value)?;
                        let instr = compound_assign_instr(op, dst, cur, rhs)
                            .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                        self.emit(instr);
                        self.emit(Instr::SetProp {
                            obj,
                            name,
                            val: dst,
                            strict: self.cx.strict_expr_region > 0,
                        });
                    }
                    return Ok(dst);
                }
                // `obj.#x = v` / `obj.#x op= v` — same as a static member, keyed "#x".
                MemberProp::Private(field) => {
                    self.check_private_declared(field)?;
                    let obj = self.recv_expr(&m.object)?;
                    let name = self.string_name(&private_key(field));
                    if is_logical {
                        self.emit(Instr::GetProp { dst, obj, name });
                        let j = self.emit_logical_skip(op, dst);
                        let v = self.expr_into(value, dst)?;
                        if v != dst {
                            self.emit(Instr::Move { dst, src: v });
                        }
                        self.emit(Instr::SetProp {
                            obj,
                            name,
                            val: dst,
                            strict: self.cx.strict_expr_region > 0,
                        });
                        let end = self.here();
                        self.patch_jump(j, end);
                    } else if matches!(op, AssignOp::Assign) {
                        let val = self.expr_into(value, dst)?;
                        if val != dst {
                            self.emit(Instr::Move { dst, src: val });
                        }
                        self.emit(Instr::SetPrivate {
                            obj,
                            name,
                            val: dst,
                        });
                    } else {
                        let cur = self.temp();
                        self.emit(Instr::GetProp {
                            dst: cur,
                            obj,
                            name,
                        });
                        let rhs = self.expr(value)?;
                        let instr = compound_assign_instr(op, dst, cur, rhs)
                            .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                        self.emit(instr);
                        self.emit(Instr::SetPrivate {
                            obj,
                            name,
                            val: dst,
                        });
                    }
                    return Ok(dst);
                }
                // `super[k] = v` / compound / logical.
                MemberProp::Computed(key_expr) if matches!(&m.object, Expr::Super) => {
                    let pid = self.super_class;
                    if pid.is_none() && !self.super_home_obj {
                        return Err("`super[k] = …` is only valid in a method".into());
                    }
                    self.this_check();
                    let key = self.expr(key_expr)?;
                    let key_reg = self.alloc_reg();
                    if key != key_reg {
                        self.emit(Instr::Move {
                            dst: key_reg,
                            src: key,
                        });
                    }
                    // GetSuperBase after the key, BEFORE the RHS (superPropOrdering's
                    // testElemAssign).
                    let sb = pid.map(|p| {
                        let b = self.temp();
                        self.emit(Instr::SuperBase {
                            dst: b,
                            home_class_id: p,
                        });
                        b
                    });
                    let emit_get = |s: &mut Self, d: Reg| match pid {
                        Some(p) => s.emit(Instr::SuperGetComputed {
                            dst: d,
                            home_class_id: p,
                            key: key_reg,
                        }),
                        None => s.emit(Instr::SuperGetObjComputed {
                            dst: d,
                            key: key_reg,
                        }),
                    };
                    let emit_set = |s: &mut Self, v: Reg| match (pid, sb) {
                        (Some(p), Some(b)) => s.emit(Instr::SuperSetComputed {
                            base: b,
                            home_class_id: p,
                            key: key_reg,
                            val: v,
                        }),
                        _ => s.emit(Instr::SuperSetObjComputed {
                            key: key_reg,
                            val: v,
                        }),
                    };
                    if is_logical {
                        emit_get(self, dst);
                        let j = self.emit_logical_skip(op, dst);
                        let v = self.expr_into(value, dst)?;
                        if v != dst {
                            self.emit(Instr::Move { dst, src: v });
                        }
                        emit_set(self, dst);
                        let end = self.here();
                        self.patch_jump(j, end);
                    } else if matches!(op, AssignOp::Assign) {
                        let val = self.expr_into(value, dst)?;
                        if val != dst {
                            self.emit(Instr::Move { dst, src: val });
                        }
                        emit_set(self, dst);
                    } else {
                        let cur = self.temp();
                        emit_get(self, cur);
                        let rhs = self.expr(value)?;
                        let instr = compound_assign_instr(op, dst, cur, rhs)
                            .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                        self.emit(instr);
                        emit_set(self, dst);
                    }
                    return Ok(dst);
                }
                MemberProp::Computed(key_expr) => {
                    let obj = self.recv_expr(&m.object)?; // evaluate receiver + key once
                                                     // Fuse `obj[<plain string literal> + e] = v` → SetIndexConcat,
                                                     // like the READ (`exprs.rs`), the delete and `assign_target`
                                                     // already do. Those three are sound as-is because nothing
                                                     // evaluates between their key and their store; a plain `=`
                                                     // evaluates the RHS in between, so the `+`'s OBSERVABLE
                                                     // coercion is hoisted into `ToConcatKey` at the `+`'s own
                                                     // position and only the (then-pure) concatenation is
                                                     // deferred to the store. A previous version of this fusion
                                                     // omitted the hoist and ran a user `toString` after the
                                                     // RHS — see PERF_ROADMAP B50's wrong-answer note.
                                                     //
                                                     // Only the plain `=` arm fuses: compound (`+=`) and logical
                                                     // (`||=`) assignments read and write through ONE
                                                     // ToPropKey-coerced key so a user coercion runs exactly
                                                     // once, and the fused op exposes no such key.
                    if !is_logical && matches!(op, AssignOp::Assign) {
                        if let Some((name, rhs)) = concat_key_literal_prefix(key_expr) {
                            let nidx = self.string_name(name);
                            let key = self.expr(rhs)?;
                            let keyk = self.temp();
                            self.emit(Instr::ToConcatKey {
                                dst: keyk,
                                src: key,
                            });
                            let val = self.expr_into(value, dst)?;
                            if val != dst {
                                self.emit(Instr::Move { dst, src: val });
                            }
                            self.emit(Instr::SetIndexConcat {
                                obj,
                                name: nidx,
                                key: keyk,
                                val: dst,
                            });
                            return Ok(dst);
                        }
                    }
                    let key = self.expr(key_expr)?;
                    if is_logical {
                        // A read-modify-write reuses the SAME property key for the load
                        // and the store: coerce ToPropertyKey ONCE (its toString/valueOf
                        // must not run twice).
                        let keyk = self.temp();
                        self.emit(Instr::ToPropKey {
                            dst: keyk,
                            obj,
                            src: key,
                        });
                        self.emit(Instr::GetIndex {
                            dst,
                            obj,
                            key: keyk,
                        });
                        let j = self.emit_logical_skip(op, dst);
                        let v = self.expr_into(value, dst)?;
                        if v != dst {
                            self.emit(Instr::Move { dst, src: v });
                        }
                        self.emit(Instr::SetIndex {
                            obj,
                            key: keyk,
                            val: dst,
                        });
                        let end = self.here();
                        self.patch_jump(j, end);
                    } else if matches!(op, AssignOp::Assign) {
                        // A plain store coerces the key once (the single SetIndex).
                        let val = self.expr_into(value, dst)?;
                        if val != dst {
                            self.emit(Instr::Move { dst, src: val });
                        }
                        self.emit(Instr::SetIndex { obj, key, val: dst });
                    } else {
                        let keyk = self.temp();
                        self.emit(Instr::ToPropKey {
                            dst: keyk,
                            obj,
                            src: key,
                        });
                        let cur = self.temp();
                        self.emit(Instr::GetIndex {
                            dst: cur,
                            obj,
                            key: keyk,
                        });
                        let rhs = self.expr(value)?;
                        let instr = compound_assign_instr(op, dst, cur, rhs)
                            .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                        self.emit(instr);
                        self.emit(Instr::SetIndex {
                            obj,
                            key: keyk,
                            val: dst,
                        });
                    }
                    return Ok(dst);
                }
            },
            // Destructuring assignment to existing targets: `[a,b]=arr`, `({x}=o)`.
            Target::Array(_) | Target::Object { .. } => {
                let src = self.expr_into(value, dst)?;
                if src != dst {
                    self.emit(Instr::Move { dst, src });
                }
                self.assign_target(target, dst)?;
                return Ok(dst);
            }
            // Annex B `f() = v` / `f() op= v` / `f() ??= v`: the call is
            // evaluated, then a ReferenceError is thrown — the RHS never runs.
            Target::Call(c) => {
                self.assign_call_target(c)?;
                return Ok(dst);
            }
            Target::Ident { .. } => {}
        }
        let (name, lhs_covered) = match target {
            // `covered` replaces the old span comparison (`id.span.start >
            // a.span.start`): a PARENTHESIZED target is not an IdentifierRef, so
            // NamedEvaluation does not apply and `(x) = function(){}` leaves the
            // function anonymous.
            //
            // NOTE: `parse::oxc_bridge` always lowers `covered: false` (oxc's
            // `SimpleAssignmentTarget::cover` peels the parens and drops the
            // span), so through the bridge `(x) = function(){}` now DOES get
            // NamedEvaluation where the span test suppressed it. That fidelity
            // loss is the bridge's, not this module's — the real parser sets the
            // flag — but it is the one construct in this file where bcdiff can
            // legitimately disagree with the oxc compiler.
            Target::Ident { name, covered } => (name.to_string(), *covered),
            _ => return Err("assignment to non-identifier not in zipp-vm v1".into()),
        };
        // Strict mode: assignment to `eval`/`arguments` is an early SyntaxError.
        strict_name_err(self.cx.in_strict, &name)?;
        // Assigning to a name still in its Temporal Dead Zone throws — see
        // `emit_tdz_store_throw`. A compound/logical operator's GetValue throws
        // BEFORE the RHS is evaluated; a plain `=` evaluates the RHS first and
        // only then performs PutValue, so its throw is emitted after.
        if self.param_tdz.contains(&name) {
            if op == AssignOp::Assign {
                let v = self.expr_into(value, dst)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
            }
            self.emit_tdz_store_throw(&name);
            return Ok(dst);
        }
        // Inside a `with`, an assignment target may be a property of an active
        // with-object (innermost first), else the static binding.
        let with_objs = self.with_obj_regs(&name);
        if !with_objs.is_empty() {
            return self.assign_with(op, value, &name, &with_objs, dst);
        }
        let binding = self.resolve(&name);
        match op {
            AssignOp::Assign => {
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
                        let into = if builds_into_dst_incrementally(value, &name) {
                            self.temp()
                        } else {
                            r
                        };
                        let v = if lhs_covered {
                            self.expr_into(value, into)?
                        } else {
                            self.compile_named_init(into, value, &name)?
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
                // A strict `name = …` on a global this program does NOT
                // declare: ResolveBinding runs BEFORE the RHS evaluates — an
                // eval-created (own-prop-backed) global resolves the reference,
                // and an unresolvable name throws before any RHS side effect
                // (`undeclared = (this.undeclared = 5)`). Probe now; store with
                // the resolved form below (a program-DECLARED global keeps the
                // checked store: its binding is non-configurable, so the
                // store-time check can never disagree).
                let resolved_first = matches!(
                    &binding,
                    Binding::Global(idx)
                        if self.cx.in_strict
                            && !self.cx.lexical_globals.contains(idx)
                            && !self.cx.hoisted_globals.contains(idx)
                            && !self.cx.decl_globals.contains(idx)
                );
                if resolved_first {
                    if let Binding::Global(idx) = &binding {
                        self.emit(Instr::CheckGlobalResolvable { idx: *idx });
                    }
                }
                let v = if lhs_covered {
                    self.expr_into(value, dst)?
                } else {
                    self.compile_named_init(dst, value, &name)?
                };
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.store_binding_snapped_ex(&binding, dst, snap, resolved_first);
                self.set_next_reg(save_p);
                Ok(dst)
            }
            // Logical assignment: `x ||= y` / `x &&= y` / `x ??= y` only assign
            // `y` when the short-circuit condition holds (truthy-skip for ||=,
            // falsy-skip for &&=, non-nullish-skip for ??=).
            AssignOp::LogicalOr | AssignOp::LogicalAnd | AssignOp::LogicalCoalesce => {
                let save_p = self.next_reg;
                let snap = self.eval_snap_probe(&binding);
                let cur = self.load_binding(&binding, dst);
                if cur != dst {
                    self.emit(Instr::Move { dst, src: cur });
                }
                let j = self.emit_logical_skip(op, dst);
                // NamedEvaluation: `x ||= function(){}` / `&&=` / `??=` names the
                // anonymous fn/arrow/class after the identifier LHS (IsIdentifierRef),
                // matching plain `=`. `compile_named_init` no-ops to `expr_into` for a
                // non-anonymous RHS, so a named/expression RHS is unaffected.
                // A PARENTHESIZED target is not an IdentifierRef, so `(x) ??= f`
                // leaves the function anonymous — same rule as plain `=` above.
                let v = if lhs_covered {
                    self.expr_into(value, dst)?
                } else {
                    self.compile_named_init(dst, value, &name)?
                };
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                // read-first: `load_binding` ran above (the short-circuit test
                // needs the current value), so the reference is resolved.
                self.store_binding_snapped_ex(&binding, dst, snap, true);
                let end = self.here();
                self.patch_jump(j, end);
                self.set_next_reg(save_p);
                Ok(dst)
            }
            // Arithmetic / bitwise compound assignment (`+= -= *= /= %= **= <<=
            // >>= >>>= |= ^= &=`).
            other => {
                if let Binding::Local(r) = binding {
                    if !self.const_regs.contains(&r) && !self.is_self_name_reg(r) {
                        // Do not plant AddRightPair here. A plain local may be a
                        // proven-linear loop accumulator; the post-pass then
                        // upgrades its outer Add to StrAppendInPlace. Replacing
                        // that Add here would silently discard the stronger
                        // no-result-allocation licence. The motivating top-level
                        // `var path` is a Global and takes the branch below.
                        // Plain mutable local: compute in place.
                        let rhs = self.expr(value)?;
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
                if other == AssignOp::Add && concat_pair_fuse_enabled() {
                    if let Some((left, right)) = add_right_pair_parts(value) {
                        let b = self.expr(left)?;
                        let c = self.expr(right)?;
                        self.emit(Instr::AddRightPair {
                            dst,
                            a: cur,
                            b,
                            c,
                            in_place: false,
                        });
                    } else {
                        let rhs = self.expr(value)?;
                        let instr = compound_assign_instr(other, dst, cur, rhs)
                            .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                        self.emit(instr);
                    }
                } else {
                    let rhs = self.expr(value)?;
                    let instr = compound_assign_instr(other, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                }
                // read-first: the `load_binding` above already resolved the
                // reference, so the store may not raise "is not defined".
                self.store_binding_snapped_ex(&binding, dst, snap, true);
                self.set_next_reg(save_p);
                Ok(dst)
            }
        }
    }

    /// Assignment to a plain identifier inside a `with` body, where `objs`
    /// (innermost first) may shadow the static binding. Mirrors the identifier
    /// branch of `assign`, routing the read/write through `load_with`/`store_with`.
    pub(crate) fn assign_with(
        &mut self,
        op: AssignOp,
        value: &Expr,
        name: &str,
        objs: &[Reg],
        dst: Reg,
    ) -> R<Reg> {
        match op {
            AssignOp::Assign => {
                // The REFERENCE resolves before the RHS runs (which with-object,
                // if any, holds the binding); PutValue writes through that
                // snapshot even if the RHS deletes the with-object property.
                let (found, tgt) = self.emit_with_probe(name, objs);
                let v = self.compile_named_init(dst, value, name)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.emit_with_rmw_write(name, found, tgt, dst);
                Ok(dst)
            }
            AssignOp::LogicalOr | AssignOp::LogicalAnd | AssignOp::LogicalCoalesce => {
                // Resolve the reference ONCE (which with-object, if any, holds the
                // binding), then read and write through that same target — even if
                // a getter run by the read mutates the object meanwhile.
                let (found, tgt) = self.emit_with_probe(name, objs);
                self.emit_with_rmw_read(name, found, tgt, dst);
                let j = self.emit_logical_skip(op, dst);
                let v = self.compile_named_init(dst, value, name)?;
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
                let rhs = self.expr(value)?;
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
        self.emit(Instr::LoadBool {
            dst: found,
            val: false,
        });
        self.emit(Instr::LoadUndefined { dst: tgt });
        let mut done = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas {
                dst: flag,
                obj,
                name: nidx,
            });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse {
                cond: flag,
                target: 0,
            });
            self.dec_next_reg(1); // reclaim the flag temp
            self.emit(Instr::LoadBool {
                dst: found,
                val: true,
            });
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
        self.emit(Instr::JumpIfFalse {
            cond: found,
            target: 0,
        }); // → static read
            // GetBindingValue: HasProperty AGAIN, then Get (both observable
            // through Proxy traps) — not a bare [[Get]].
        self.emit(Instr::WithGet {
            dst,
            obj: tgt,
            name: nidx,
            strict: self.cx.in_strict,
        });
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
        self.emit(Instr::JumpIfFalse {
            cond: found,
            target: 0,
        }); // → static write
            // SetMutableBinding re-checks HasProperty (the binding may have been
            // DELETED since the reference resolved): strict throws, sloppy does
            // not silently recreate the property on the with-object.
        self.emit(Instr::WithSet {
            obj: tgt,
            name: nidx,
            val: src,
            strict: self.cx.in_strict,
        });
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
