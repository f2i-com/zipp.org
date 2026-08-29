// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// MI (method inlining): if the resolved class/own/proto METHOD `fid` is a
    /// "trivial" straight-line body over `this`(=`recv`) + its formal params —
    /// arithmetic on numbers, own-data `this.<field>` reads, and nested
    /// `super.m(args)` calls — evaluate it DIRECTLY (no `setup_call`, no
    /// `run_loop`, no frame push, no per-call args Vec) and return the result
    /// bits. Returns `None` to fall back to the full frame call (any other body
    /// shape, an unrecognised op, a non-numeric arithmetic operand, a missing /
    /// accessor / inherited field, a non-instance receiver) and `Some(CALL_THREW)`
    /// when a nested super target threw.
    ///
    /// This is the call-floor collapse for the class-method benches: every
    /// `objs[i&3].area()` body is `return super.area() * k + …`, and `super.area()`
    /// resolves to `return this._v + 1` — so the whole two-deep call chain runs
    /// as a handful of Rust ops over `recv`'s own slots, no frame machinery.
    ///
    /// SOUNDNESS:
    /// * Reached ONLY from `jit_region_call_impl` (a JIT region helper), so the
    ///   interpreter / `ZIPP_NOJIT` path is byte-identical (never calls this).
    /// * The caller supplies the exact callable it resolved. Legacy method ops
    ///   reach this only after `ic_call_method`'s full receiver/own-shadow/class
    ///   guards; `CallWithThis` reaches it only after resolving the callable
    ///   Value captured by `GetProp`/`SuperGet`. In both cases `fid` is the live
    ///   target that will run, and `recv` is the already-captured receiver.
    /// * NO partial side effect before a `None`: a two-pass shape — pass 1
    ///   (`method_body_inlinable`) validates the ENTIRE straight-line body is
    ///   executable WITHOUT running anything; pass 2 executes. So an unsupported
    ///   op declines (pass 1) before any super call commits, and once pass 2
    ///   starts every op is known-executable.
    /// * Arithmetic delegates to the SAME value-level helpers the interpreter's
    ///   ops use (`add_values`, `numeric_binop`) so results are byte-identical;
    ///   it is admitted only on operands that are ALREADY numbers (else pass 1
    ///   declines), so no observable `valueOf`/`ToPrimitive` ever runs off-frame.
    /// * A nested legacy `super.m()` is resolved via `ic_super_method`. The
    ///   spec-correct lowering (`SuperGet; CallWithThis`) instead invokes the
    ///   exact Value produced by that `SuperGet`; it never re-resolves by name.
    ///   Both paths recurse only for a trivial, effect-free target. Otherwise
    ///   the whole method declines before entering it, so fallback cannot replay
    ///   a committed effect.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn try_method_inline(
        &mut self,
        fid: u32,
        recv: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<u64> {
        // Pass 1: validate the body shape without executing anything.
        let body_len = self.method_body_inlinable(fid)?;
        // A post-execution charge cannot enforce a hard budget before a long
        // straight-line body runs, and nested super/accessor bodies add work not
        // represented by `body_len`. Fail closed to the ordinary frame path,
        // whose per-op/native-block meter is exact and checked before execution.
        #[cfg(feature = "instrument")]
        if self.jit.metered() {
            return None;
        }
        // Pass 2: execute over a local register window.
        let out = self.run_method_inline(fid, recv, caller_base, arg_base, argc, body_len, 0);
        // These ops ran here, not in `run_loop`, so the dispatch hook never saw
        // them — charge them by hand. Only on success: a decline falls back to a
        // real frame call, which the interpreter charges itself.
        #[cfg(feature = "instrument")]
        if out.is_some() {
            self.charge_steps(body_len as i64);
        }
        out
    }

    /// `try_method_inline` with the arguments in a SLICE instead of the caller's
    /// register window — the `f.apply(thisArg, [a, b])` entry (B82), where the
    /// forwarded args live in a heap Array rather than contiguous caller regs.
    /// `regs[0]` is bound to `this_v` EXACTLY as given; the caller is
    /// responsible for having already applied (or declined on)
    /// OrdinaryCallBindThis. Same result contract as `try_method_inline`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn try_call_inline_argv(
        &mut self,
        fid: u32,
        this_v: Value,
        args: &[Value],
    ) -> Option<u64> {
        let body_len = self.method_body_inlinable(fid)?;
        #[cfg(feature = "instrument")]
        if self.jit.metered() {
            return None;
        }
        let p = self.func(fid as usize);
        let mut regs = [Value::UNDEFINED; Self::MI_MAX_REGS];
        regs[0] = this_v;
        let n = args.len().min(p.param_count as usize);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        let out = self.run_mi_ops(fid, this_v, &mut regs, body_len, 0);
        #[cfg(feature = "instrument")]
        if out.is_some() {
            self.charge_steps(body_len as i64);
        }
        out
    }

    /// Pass 1 of method inlining: is `fid`'s body a straight-line prefix of ops
    /// the off-frame evaluator implements, ending at the FIRST `Return`/
    /// `ReturnUndefined`? Returns the body length (ops up to and incl. that
    /// terminator), or `None` to decline. Performs NO execution / side effect.
    /// Mirrors the eligibility of `callee_leaf_ok` (no generator/async, simple
    /// params, no rest/arguments, bounded regs) but ADDS own-`this` GetProp and
    /// `super.m()` to the admitted op set and binds `this = recv` (a class method
    /// is strict and uses its receiver — never the global-leaf `this=undefined`).
    ///
    /// MEMOIZED in `self.mi_cache` (a FuncProto's code is immutable for life), so
    /// the hot per-call path pays the body scan ONCE per fid.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn method_body_inlinable(&mut self, fid: u32) -> Option<usize> {
        let i = fid as usize;
        if i < self.mi_cache.len() {
            match self.mi_cache[i] {
                v if v == i32::MIN => {}      // not yet computed
                -1 => return None,            // memoized ineligible
                v => return Some(v as usize), // memoized body length
            }
        } else {
            self.mi_cache.resize(i + 1, i32::MIN);
        }
        let res = self.method_body_inlinable_scan(fid);
        self.mi_cache[i] = match res {
            Some(len) => len as i32,
            None => -1,
        };
        res
    }

    /// The uncached body-shape scan behind `method_body_inlinable`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn method_body_inlinable_scan(&self, fid: u32) -> Option<usize> {
        use crate::bytecode::Instr as I;
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async {
            return None;
        }
        // No rest/`arguments` object (binding past `param_count` must not be
        // observable). We do NOT require `simple_params`: that flag is purely
        // about a SLOPPY function's MAPPED arguments object and is deliberately
        // false for every (strict) class method. A default/destructuring
        // parameter prologue would emit a `Jump`/unsupported op before the first
        // `Return`, which the straight-line whitelist below rejects — so plain
        // positional binding is the only param shape that survives here.
        if p.rest_reg.is_some() || p.arguments_reg.is_some() {
            return None;
        }
        // A bounded local register window (kept small — these are tiny bodies;
        // the executor uses a fixed `[Value; MI_MAX_REGS]` stack array).
        if p.reg_count as usize > Self::MI_MAX_REGS {
            return None;
        }
        let code = &p.code;
        let term = code
            .iter()
            .position(|i| matches!(i, I::Return { .. } | I::ReturnUndefined))?;
        for (ix, instr) in code[..term].iter().enumerate() {
            // A `SuperSet` is the evaluator's ONLY committing side effect. To keep
            // the "DEOPT only before any side effect" guarantee airtight, it may be
            // followed ONLY by the terminator (Return/RetU) — never by another op
            // that could itself decline at run time (which, after the super-set had
            // committed, would double-run it on the frame-call fallback). A trivial
            // `set x(v){ super.x = … }` always has this shape; anything else
            // declines the whole body here, before any execution.
            if matches!(instr, I::SuperSet { .. }) && ix + 1 != term {
                return None;
            }
            match *instr {
                // Pure value ops the evaluator implements.
                I::LoadInt { .. } | I::LoadBool { .. } | I::Move { .. } => {}
                I::LoadConst { idx, .. } => {
                    // Only numeric constants (the arithmetic ops require numbers;
                    // a string/heap const would only be a `+` concat operand,
                    // which we decline — `add_values` on a heap operand could run
                    // user `valueOf`).
                    match p.constants.get(idx as usize) {
                        Some(c) if c.is_number() => {}
                        _ => return None,
                    }
                }
                // `this.<field>` (and ONLY `this`): an own-data read at run time
                // (validated per-execution); any other `obj` declines.
                I::GetProp { obj: 0, .. } => {}
                // Arithmetic — admitted; per-execution the evaluator declines to
                // a frame call if an operand isn't already a number.
                I::Add { .. }
                | I::Sub { .. }
                | I::Mul { .. }
                | I::Div { .. }
                | I::Mod { .. }
                | I::AddInt { .. }
                | I::Neg { .. }
                | I::Bitwise { .. } => {}
                // `super.m(args)` — resolved + evaluated at run time.
                I::SuperMethod { .. } => {}
                // `super.<name>` read — resolved + read off-frame at run time via
                // `ic_super_get` (live, version-guarded). Pure (a read), so admitting
                // it anywhere in the straight-line prefix is effect-free.
                I::SuperGet { .. } => {}
                // Current `super.m()` lowering captures the property Value before
                // evaluating/calling it. Recover the zero-arg hot-class case only
                // for the exact adjacent pair emitted by the compiler. The pass-2
                // arm consumes the Value in `callee`; it never looks `name` up a
                // second time. Anything with args/intervening work remains on the
                // ordinary frame path.
                I::CallWithThis {
                    callee,
                    this_v: 0,
                    argc: 0,
                    ..
                } if ix > 0
                    && matches!(
                        code[ix - 1],
                        I::SuperGet { dst, .. } if dst == callee
                    ) => {}
                // `GetSuperBase` capture for a SuperMethod/SuperSet — a pure read
                // (the same lookup the interpreter's SuperBase op performs).
                I::SuperBase { .. } => {}
                // `super.<name> = val` write — the body's ONLY off-frame side
                // effect. Resolved via `ic_super_set` and committed exactly once at
                // run time (an inherited trivial setter over an own data slot); the
                // run-time arm commits ONLY on a known-trivial target, else declines
                // BEFORE committing. The check above guarantees this op is the LAST
                // before the terminator, so no later op can decline post-commit.
                I::SuperSet { .. } => {}
                _ => return None,
            }
        }
        Some(term + 1)
    }

    /// Pass 2 of method inlining: execute `fid`'s validated trivial body over a
    /// fresh local register window (`reg 0 = recv`, formals in `1..`, the rest
    /// undefined). `depth` bounds nested `super` recursion. Returns the result
    /// bits, `None` (a per-execution decline — an op's operand wasn't the
    /// expected number / own-data slot; the caller frame-calls the WHOLE method,
    /// and since no super op had committed yet this is effect-free), or
    /// `Some(CALL_THREW)` (a nested super target threw).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_method_inline(
        &mut self,
        fid: u32,
        recv: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        body_len: usize,
        depth: u32,
    ) -> Option<u64> {
        // Local register window on the STACK — NO heap allocation per call (the
        // frame-call path it replaces reuses the pinned reg file; an allocation
        // here would be far slower than the frame call it elides). `reg_count`
        // is bounded ≤ MI_MAX_REGS in pass 1. this in reg 0, positional args in
        // 1.., the rest undefined (mirrors setup_call's zero-fill).
        let p = self.func(fid as usize);
        let mut regs = [Value::UNDEFINED; Self::MI_MAX_REGS];
        regs[0] = recv;
        let nargs = (argc as usize).min(p.param_count as usize);
        for i in 0..nargs {
            regs[1 + i] = self.get(caller_base, arg_base + i as u16);
        }
        self.run_mi_ops(fid, recv, &mut regs, body_len, depth)
    }

    /// The op-evaluation core behind `run_method_inline` /
    /// `try_call_inline_argv`: execute the validated trivial body of `fid` over
    /// an ALREADY-BOUND register window (`regs[0]` = the effective `this`,
    /// formals in `1..`). Same three-state result as `run_method_inline`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn run_mi_ops(
        &mut self,
        fid: u32,
        recv: Value,
        regs: &mut [Value],
        body_len: usize,
        depth: u32,
    ) -> Option<u64> {
        use crate::bytecode::Instr as I;
        use crate::vm::helpers_misc::BigOp;
        let p = self.func(fid as usize);
        // `code`/`constants`/`string_constants` are `&'p` — they outlive
        // `&mut self`.
        let code: &'p [Instr] = &p.code;
        let consts = &p.constants;
        // Helper: numeric fast paths matching the interpreter ops EXACTLY; a
        // non-numeric operand declines (None) so no observable coercion runs.
        for (body_ip, instr) in code[..body_len].iter().enumerate() {
            match *instr {
                I::LoadInt { dst, val } => regs[dst as usize] = Value::int(val),
                I::LoadBool { dst, val } => regs[dst as usize] = Value::bool(val),
                I::LoadConst { dst, idx } => {
                    regs[dst as usize] = *consts.get(idx as usize)?;
                }
                I::Move { dst, src } => regs[dst as usize] = regs[src as usize],
                // `obj: 0` ONLY (the `this` register): pass 1 admits a GetProp
                // solely when `obj == 0`, and this arm reads `recv` (= reg 0).
                // Matching `obj: 0` here ties the read to that guarantee — any
                // future pass-1 change admitting `obj != 0` falls through to the
                // `_ => return None` decline instead of silently reading `recv`.
                I::GetProp { dst, obj: 0, name } => {
                    // `this.<field>` — own DATA slot only (a missing / accessor /
                    // inherited field needs full get_member semantics → decline).
                    if !recv.is_heap() || !self.ic_obj_ok(recv.heap_index()) {
                        return None;
                    }
                    let key = &p.string_constants[name as usize];
                    let m = match self.heap.get(recv.heap_index()) {
                        HeapObj::Object(m) if !m.is_ctor => m,
                        _ => return None,
                    };
                    let s = m.pos(key)?;
                    if m.attr_at(s).accessor {
                        return None;
                    }
                    regs[dst as usize] = m.val_at(s);
                }
                I::Add { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_add(va, vb)?;
                }
                I::Sub { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_num_binop(BigOp::Sub, va, vb)?;
                }
                I::Mul { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_num_binop(BigOp::Mul, va, vb)?;
                }
                I::Div { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_num_binop(BigOp::Div, va, vb)?;
                }
                I::Mod { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_num_binop(BigOp::Mod, va, vb)?;
                }
                I::Neg { dst, a } => {
                    let va = regs[a as usize];
                    regs[dst as usize] = if va.is_int() {
                        let i = va.as_int();
                        if i == 0 {
                            Value::num(-0.0)
                        } else {
                            match i.checked_neg() {
                                Some(v) => Value::int(v),
                                None => Value::num(-(i as f64)),
                            }
                        }
                    } else if va.is_double() {
                        Value::num(-va.as_f64())
                    } else {
                        return None;
                    };
                }
                I::AddInt { dst, a, imm, .. } => {
                    let va = regs[a as usize];
                    regs[dst as usize] = if va.is_int() {
                        match va.as_int().checked_add(imm) {
                            Some(v) => Value::int(v),
                            None => Value::num(va.as_int() as f64 + imm as f64),
                        }
                    } else if va.is_double() {
                        Value::num(va.as_f64() + imm as f64)
                    } else {
                        return None;
                    };
                }
                I::Bitwise { dst, a, b, op } => {
                    use crate::bytecode::BitwiseOp as B;
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    // Int fast path only — a non-int operand needs ToNumeric
                    // (observable on objects / BigInt) → decline to the frame call.
                    if !va.is_int() || !vb.is_int() {
                        return None;
                    }
                    let (x, y) = (va.as_int(), vb.as_int());
                    regs[dst as usize] = match op {
                        B::And => Value::int(x & y),
                        B::Or => Value::int(x | y),
                        B::Xor => Value::int(x ^ y),
                        B::Shl => Value::int(x.wrapping_shl((y as u32) & 31)),
                        B::Shr => Value::int(x >> ((y as u32) & 31)),
                        B::Ushr => {
                            let u = (x as u32) >> ((y as u32) & 31);
                            if u <= i32::MAX as u32 {
                                Value::int(u as i32)
                            } else {
                                Value::num(u as f64)
                            }
                        }
                    };
                }
                I::SuperMethod {
                    dst,
                    home_class_id,
                    name,
                    argc: sargc,
                    ..
                } => {
                    let bits =
                        self.mi_super_call(fid, body_ip, home_class_id, name, sargc, recv, depth)?;
                    // A nested super target threw — propagate (the region exits;
                    // never re-executed). `CALL_THREW`/`SELF_CALL_DEOPT` are NaN-
                    // tagged sentinels never produced as a real result.
                    if bits == crate::codegen::CALL_THREW || bits == crate::codegen::SELF_CALL_DEOPT
                    {
                        // DEOPT here would re-run the WHOLE method (incl. the
                        // super call) in the interpreter — but a super target that
                        // declined off-frame was ALREADY run by a real frame call
                        // (a committed effect), so we must NOT redo it. The only
                        // SELF_CALL_DEOPT path inside mi_super_call is BEFORE it
                        // runs anything (resolution miss / depth cap), so a
                        // SELF_CALL_DEOPT here means nothing committed → safe to
                        // decline the whole method.
                        if bits == crate::codegen::SELF_CALL_DEOPT {
                            return None;
                        }
                        return Some(crate::codegen::CALL_THREW);
                    }
                    regs[dst as usize] = Value::from_bits(bits);
                }
                I::SuperGet {
                    dst,
                    home_class_id,
                    name,
                } => {
                    let v = self.mi_super_get(fid, body_ip, home_class_id, name, recv)?;
                    regs[dst as usize] = v;
                }
                I::CallWithThis {
                    dst,
                    callee,
                    this_v: 0,
                    argc: 0,
                    ..
                } if body_ip > 0
                    && matches!(
                        code[body_ip - 1],
                        I::SuperGet { dst, .. } if dst == callee
                    ) =>
                {
                    let bits = self.mi_captured_call(regs[callee as usize], recv, depth)?;
                    if bits == crate::codegen::CALL_THREW || bits == crate::codegen::SELF_CALL_DEOPT
                    {
                        // The captured target is admitted only before it runs and
                        // only when its complete recursive body is effect-free.
                        // A deopt therefore commits nothing and may safely decline
                        // the whole outer method; a throw must propagate.
                        if bits == crate::codegen::SELF_CALL_DEOPT {
                            return None;
                        }
                        return Some(crate::codegen::CALL_THREW);
                    }
                    regs[dst as usize] = Value::from_bits(bits);
                }
                I::SuperBase { dst, home_class_id } => {
                    // The same live GetSuperBase the interpreter's op performs
                    // (a pure read — a decline commits nothing).
                    let is_static = self.func(fid as usize).super_static;
                    let v = self.super_base(home_class_id, is_static);
                    regs[dst as usize] = v;
                }
                I::SuperSet {
                    home_class_id,
                    name,
                    val,
                    base: _,
                } => {
                    // The body's only off-frame side effect. Commits exactly once
                    // (an inherited trivial setter over recv's own data slot) or
                    // declines BEFORE committing (None).
                    let value = regs[val as usize];
                    self.mi_super_set(fid, body_ip, home_class_id, name, recv, value)?;
                }
                I::Return { src } => return Some(regs[src as usize].bits()),
                I::ReturnUndefined => return Some(Value::UNDEFINED.bits()),
                // Unreachable: pass 1 admitted only the ops above.
                _ => return None,
            }
        }
        // The body ended without an explicit Return op (terminator was the last
        // op handled above) — defensively return undefined.
        Some(Value::UNDEFINED.bits())
    }

    /// `+` for the off-frame method evaluator: the interpreter's `Add` number
    /// fast paths EXACTLY (int+int with overflow → double; double+double). A
    /// non-number operand declines (None) — full `add_values` would run
    /// observable `ToPrimitive`/`valueOf` / build a string, which belongs on the
    /// frame call (so a later op declining can never double-apply it).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_add(&mut self, va: Value, vb: Value) -> Option<Value> {
        if va.is_int() && vb.is_int() {
            return Some(match va.as_int().checked_add(vb.as_int()) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + vb.as_int() as f64),
            });
        }
        if va.is_number() && vb.is_number() {
            return Some(Value::num(va.as_f64() + vb.as_f64()));
        }
        None
    }

    /// `Sub`/`Mul`/`Div`/`Mod` for the off-frame evaluator: the interpreter's
    /// number fast paths EXACTLY. A non-number operand declines (None) — its
    /// `numeric_binop` slow path can run observable coercion, so it belongs on
    /// the frame call.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_num_binop(
        &mut self,
        op: crate::vm::helpers_misc::BigOp,
        va: Value,
        vb: Value,
    ) -> Option<Value> {
        use crate::vm::helpers_misc::BigOp;
        match op {
            BigOp::Sub => {
                if va.is_int() && vb.is_int() {
                    Some(match va.as_int().checked_sub(vb.as_int()) {
                        Some(v) => Value::int(v),
                        None => Value::num(va.as_int() as f64 - vb.as_int() as f64),
                    })
                } else if va.is_number() && vb.is_number() {
                    Some(Value::num(va.as_f64() - vb.as_f64()))
                } else {
                    None
                }
            }
            BigOp::Mul => {
                if va.is_int() && vb.is_int() {
                    Some(match va.as_int().checked_mul(vb.as_int()) {
                        Some(v) => Value::int(v),
                        None => Value::num(va.as_int() as f64 * vb.as_int() as f64),
                    })
                } else if va.is_number() && vb.is_number() {
                    Some(Value::num(va.as_f64() * vb.as_f64()))
                } else {
                    None
                }
            }
            BigOp::Div => {
                if va.is_number() && vb.is_number() {
                    Some(Value::num(va.as_f64() / vb.as_f64()))
                } else {
                    None
                }
            }
            BigOp::Mod => {
                if va.is_number() && vb.is_number() {
                    Some(Value::num(va.as_f64() % vb.as_f64()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Resolve + evaluate a nested `super.m(args)` for the off-frame method
    /// evaluator. Resolution uses the SAME `ic_super_method` cache the
    /// interpreter uses; the resolved target runs off-frame (recursively,
    /// depth-bounded) when trivial, else via a real `jit_frame_call`. `home_fid`
    /// is the function whose body contains this `super` (its `super_static`
    /// flag + `string_constants` drive resolution). Returns result bits,
    /// `SELF_CALL_DEOPT` (resolution miss / depth cap — NOTHING committed, the
    /// caller may decline the whole method), or `CALL_THREW`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mi_super_call(
        &mut self,
        home_fid: u32,
        super_ip: usize,
        home_class_id: u32,
        name: u32,
        s_argc: u16,
        recv: Value,
        depth: u32,
    ) -> Option<u64> {
        use crate::codegen::SELF_CALL_DEOPT;
        if depth >= Self::METHOD_INLINE_MAX_SUPER {
            return Some(SELF_CALL_DEOPT);
        }
        let hp = self.func(home_fid as usize);
        let is_static = hp.super_static;
        let key: &'p str = &hp.string_constants[name as usize];
        // Same per-site IC the interpreter's SuperMethod arm uses, keyed by the
        // ACTUAL `(home_fid, super_ip)` of this `super.m()` op — so it shares the
        // exact cache the interpreter fills when it runs this op via run_loop
        // (no synthetic-key collision with another site in the same function).
        // `ic_super_method` re-validates the full home-value + version-guarded
        // chain on every hit, so a miss/stale entry resolves correctly.
        let (fid, closure, _callee) =
            match self.ic_super_method(home_fid, super_ip, home_class_id, is_static, key) {
                Some(t) => t,
                // Resolution miss / not a plain user fn (accessor/builtin/native):
                // NOTHING committed yet — signal the caller to decline the whole
                // method to a clean frame call.
                None => return Some(SELF_CALL_DEOPT),
            };
        let _ = closure;
        // The super target MUST itself be inline-eligible, or we DECLINE the whole
        // method (SELF_CALL_DEOPT, nothing committed) so the caller frame-calls it
        // ONCE — we never commit a partial super effect off-frame and then risk a
        // later op declining (which would double-run the super target). 0-arg
        // super calls dominate (every `super.area()`); a target with formal args
        // is supported via the local args window.
        let blen = match self.method_body_inlinable(fid) {
            Some(b) => b,
            None => return Some(SELF_CALL_DEOPT),
        };
        // The caller may still hit a later per-execution guard and decline the
        // WHOLE outer method to a frame call.  Therefore a nested super target
        // must itself be effect-free: otherwise its terminal SuperSet could
        // commit here and the outer decline would replay that write.  This
        // check is recursive in practice — a target containing SuperMethod is
        // allowed, but that deeper call applies the same gate before entering
        // its own target.  SuperSet is the only committing op admitted by
        // `method_body_inlinable_scan`.
        if self.func(fid as usize).code[..blen]
            .iter()
            .any(|instr| matches!(instr, Instr::SuperSet { .. }))
        {
            return Some(SELF_CALL_DEOPT);
        }
        // Only 0-arg super targets run off-frame (every `super.area()` is 0-arg).
        // A super call WITH arguments declines the whole method to a clean frame
        // call (nothing committed) rather than staging args into the pinned
        // register file (which could realloc near capacity). Rare in practice.
        if s_argc != 0 {
            return Some(SELF_CALL_DEOPT);
        }
        self.run_method_inline(fid, recv, 0, 0, 0, blen, depth + 1)
    }

    /// Invoke the exact callable Value captured by the adjacent
    /// `SuperGet; CallWithThis` pair admitted by `method_body_inlinable_scan`.
    /// This is deliberately narrower than a general off-frame call: zero args,
    /// strict non-arrow user functions, bounded recursion, and a fully validated
    /// effect-free body. In particular, it never converts the captured Value
    /// back into a property name, so replacing a method after `SuperGet` cannot
    /// redirect this call.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_captured_call(
        &mut self,
        callee: Value,
        recv: Value,
        depth: u32,
    ) -> Option<u64> {
        use crate::codegen::SELF_CALL_DEOPT;
        if depth >= Self::METHOD_INLINE_MAX_SUPER {
            return Some(SELF_CALL_DEOPT);
        }
        let (fid, _closure) = match self.ic_plain_fn(callee) {
            Some(target) => target,
            None => return Some(SELF_CALL_DEOPT),
        };
        let target = self.func(fid as usize);
        // Arrow `this` and sloppy OrdinaryCallBindThis both need setup_call's
        // binding rules. Class methods (the hot path) and strict functions use
        // the captured receiver exactly as supplied.
        if target.lexical_this || !target.is_strict {
            return Some(SELF_CALL_DEOPT);
        }
        let blen = match self.method_body_inlinable(fid) {
            Some(len) => len,
            None => return Some(SELF_CALL_DEOPT),
        };
        // A later guard in the outer method may still decline. Do not run a
        // target whose only admitted committing op could then be replayed.
        if self.func(fid as usize).code[..blen]
            .iter()
            .any(|instr| matches!(instr, Instr::SuperSet { .. }))
        {
            return Some(SELF_CALL_DEOPT);
        }
        self.run_method_inline(fid, recv, 0, 0, 0, blen, depth + 1)
    }

    /// Resolve + read a nested `super.<name>` (a `SuperGet` op) for the off-frame
    /// accessor/method evaluator. Resolution uses the SAME `ic_super_get` cache the
    /// interpreter's `SuperGet` arm uses (live home-class value + version-guarded
    /// hop chain via `ic_super_chain_ok`), keyed by the ACTUAL `(home_fid,
    /// super_ip)` of this op. Serves the read OFF-FRAME only when the resolved super
    /// property is:
    ///   * a DATA slot on the super chain (`GetAct::Value` — byte-identical), or
    ///   * an ACCESSOR whose getter is the trivial `return this.<field>` shape over
    ///     `recv`'s own data slot (`accessor_fast_get`, evaluated with the SAME
    ///     `this = recv` the interpreter's `GetAct::Accessor` frame-call would use).
    /// Returns the value, or `None` to DECLINE the whole accessor/method to a clean
    /// frame call (resolution miss / a non-trivial getter / a non-instance recv).
    /// A `SuperGet` is a pure read — declining commits nothing.
    ///
    /// SOUNDNESS: a `super` reference ALWAYS reads from the home object's prototype
    /// (the super base), never the receiver, so an own property of `recv` cannot
    /// shadow it — correctness comes entirely from the version-guarded `ic_super_get`
    /// chain (e.g. `Object.setPrototypeOf(C.prototype, X)` bumps the anchor hop's
    /// version → the cached entry is rejected → re-resolved). The receiver-side G3b
    /// own-shadow guard on the OUTER accessor name was already enforced by
    /// `ic_get_prop`/`ic_call_method` before this evaluator ran.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_super_get(
        &mut self,
        home_fid: u32,
        super_ip: usize,
        home_class_id: u32,
        name: u32,
        recv: Value,
    ) -> Option<Value> {
        use crate::vm::ic::GetAct;
        let hp = self.func(home_fid as usize);
        let is_static = hp.super_static;
        let key: &'p str = &hp.string_constants[name as usize];
        match self.ic_super_get(home_fid, super_ip, home_class_id, is_static, key) {
            // Inherited DATA slot — byte-identical to the interpreter's data read.
            GetAct::Value(v) => Some(v),
            // Inherited ACCESSOR resolved to a plain getter: serve it off-frame ONLY
            // if it is the trivial `return this.<field>` shape over recv's own data
            // slot. The interpreter frame-calls it with `this = recv`, so reading
            // recv's own field is byte-identical. Anything else → decline (the whole
            // accessor frame-calls; nothing committed).
            GetAct::Accessor { fid, .. } => self.accessor_fast_get(fid, recv).map(Value::from_bits),
            // No usable resolution (the interpreter would take its own slow path
            // which can differ) → decline. Nothing committed.
            GetAct::None => None,
        }
    }

    /// Resolve + perform a nested `super.<name> = value` (a `SuperSet` op) for the
    /// off-frame accessor/method evaluator — the body's ONLY off-frame side effect.
    /// Resolution uses the SAME `ic_super_set` cache the interpreter's `SuperSet`
    /// arm uses (live + version-guarded). Commits the write OFF-FRAME exactly once,
    /// and ONLY when the super chain exposes an inherited SETTER whose body is the
    /// trivial `this.<field> = arg` / `this.<field> = (arg | 0)` shape over `recv`'s
    /// own writable data slot (`accessor_fast_set`, with `this = recv` — exactly the
    /// interpreter's `SetAct::Setter` frame-call). Returns `Some(())` on the served
    /// write, or `None` to DECLINE to a clean frame call (resolution miss / a non-
    /// trivial setter / a non-number value where `arg | 0` would coerce / the spec's
    /// write-to-RECEIVER case where no inherited setter exists — that goes through
    /// full `set_prop` semantics).
    ///
    /// SOUNDNESS: `accessor_fast_set` declines (`None`) BEFORE any store when the
    /// field isn't an own writable data slot or when `arg | 0` would coerce a non-
    /// number (observable `valueOf`) — so the only committing path is an in-place
    /// data store, byte-identical to the frame-called setter, and a decline leaves
    /// the world untouched (the caller frame-calls the whole accessor once). `Done`
    /// from `ic_super_set` never happens for a write (only `Setter`/`None`): a super
    /// data write targets the RECEIVER, which `ic_super_set` reports as `None`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_super_set(
        &mut self,
        home_fid: u32,
        super_ip: usize,
        home_class_id: u32,
        name: u32,
        recv: Value,
        value: Value,
    ) -> Option<()> {
        use crate::vm::ic::SetAct;
        let hp = self.func(home_fid as usize);
        let is_static = hp.super_static;
        let key: &'p str = &hp.string_constants[name as usize];
        match self.ic_super_set(home_fid, super_ip, home_class_id, is_static, key) {
            SetAct::Setter { fid, .. } => {
                // Trivial inherited setter over recv's own data slot only; else
                // decline. `accessor_fast_set` is the SAME single-commit helper the
                // non-super setter fast path uses (in-place store, no shape change).
                self.accessor_fast_set(fid, recv, value).map(|_| ())
            }
            // `Done` (an own data slot was written) never occurs for a SUPER set:
            // ic_super_set only caches inherited SETTERS; a data write goes to the
            // receiver and is reported as `None`. `None` → the receiver-write slow
            // path (could add a slot / hit a receiver setter / no-op when frozen) —
            // decline to the frame call. Nothing committed.
            SetAct::Done | SetAct::None => None,
        }
    }

    /// Recognise a trivial getter body `return this.<field>` and return the
    /// field name (a `'p`-lived string constant). The shape is exactly a single
    /// `GetProp` of register-0 (`this`) followed by its `Return`, optionally with
    /// the compiler's trailing dead `ReturnUndefined`. Anything else → `None`
    /// (the caller frame-calls). Excludes generators/async/non-strict-irrelevant
    /// — a class getter is always a concise method (strict, no rest/arguments).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn simple_getter_field(&self, fid: u32) -> Option<&'p str> {
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async || p.param_count != 0 {
            return None;
        }
        // An ARROW's reg 0 is its CAPTURED `this`, rebound at call entry and
        // ignoring the receiver entirely (`lexical_this`). These shape matchers
        // read `obj: 0` as "the receiver", so an arrow installed as an accessor
        // would be served against the wrong object — `Object.defineProperty(p,
        // "v", {get: () => this.f})` read `p.f` instead of the captured `this.f`.
        if p.lexical_this {
            return None;
        }
        let c = &p.code;
        // [GetProp{dst, obj:0, name:N}, Return{src:dst}, ...]
        let (dst0, name) = match c.first()? {
            Instr::GetProp { dst, obj: 0, name } => (*dst, *name),
            _ => return None,
        };
        match c.get(1)? {
            Instr::Return { src } if *src == dst0 => {}
            _ => return None,
        }
        Some(&p.string_constants[name as usize])
    }

    /// Recognise a trivial setter body `this.<field> = arg` or
    /// `this.<field> = (arg | 0)` (the `x | 0` int-coercion the bench uses) and
    /// return `(field_name, applies_ToInt32)`. The recognised shapes are exactly:
    ///   * `[SetProp{obj:0, name:N, val:1}, ReturnUndefined?]`        (plain)
    ///   * `[LoadInt{dst:D, val:0}, Bitwise{dst:S, a:1, b:D, op:Or},
    ///      SetProp{obj:0, name:N, val:S}, ReturnUndefined?]`         (`arg | 0`)
    /// where register 1 is the single formal parameter (`arg`). Anything else →
    /// `None`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn simple_setter_field(&self, fid: u32) -> Option<(&'p str, bool)> {
        use crate::bytecode::BitwiseOp;
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async || p.param_count != 1 {
            return None;
        }
        // An ARROW's reg 0 is its CAPTURED `this`, rebound at call entry and
        // ignoring the receiver entirely (`lexical_this`). These shape matchers
        // read `obj: 0` as "the receiver", so an arrow installed as an accessor
        // would be served against the wrong object — `Object.defineProperty(p,
        // "v", {get: () => this.f})` read `p.f` instead of the captured `this.f`.
        if p.lexical_this {
            return None;
        }
        let c = &p.code;
        // Plain `this.field = arg` (val register == the formal param, reg 1).
        if let Instr::SetProp {
            obj: 0,
            name,
            val: 1,
            strict: _,
        } = c.first()?
        {
            return Some((&p.string_constants[*name as usize], false));
        }
        // `this.field = (arg | 0)`: LoadInt 0 → Bitwise Or(arg, 0) → SetProp.
        let (zero_dst, zero_val) = match c.first()? {
            Instr::LoadInt { dst, val } => (*dst, *val),
            _ => return None,
        };
        if zero_val != 0 {
            return None;
        }
        let or_dst = match c.get(1)? {
            Instr::Bitwise {
                dst,
                a: 1,
                b,
                op: BitwiseOp::Or,
            } if *b == zero_dst => *dst,
            _ => return None,
        };
        match c.get(2)? {
            Instr::SetProp {
                obj: 0,
                name,
                val,
                strict: _,
            } if *val == or_dst => Some((&p.string_constants[*name as usize], true)),
            _ => None,
        }
    }
}

#[cfg(all(test, feature = "jit", target_arch = "x86_64"))]
mod replay_safety_tests {
    use super::*;

    fn vm(source: &str) -> Vm<'static> {
        let ast = crate::front::parse_script(source).expect("source parses");
        let program = Box::leak(Box::new(
            crate::compile::compile_program(&ast, source).expect("source compiles"),
        ));
        let mut vm = Vm::new(program);
        vm.run().expect("program runs");
        vm
    }

    fn global(vm: &Vm<'_>, name: &str) -> Value {
        let slot = vm
            .program
            .global_names
            .iter()
            .position(|candidate| candidate == name)
            .unwrap_or_else(|| panic!("missing global {name}"));
        vm.globals[slot]
    }

    /// The reference-order lowering splits a `super.m()` into an exact
    /// `SuperGet; CallWithThis` pair. Keep the class-benchmark evaluator able
    /// to consume that pair without returning to a frame or re-resolving `m`.
    #[test]
    fn captured_super_callee_stays_inlineable() {
        let mut vm = vm(r#"
                class Parent { value() { return this.n + 1; } }
                class Child extends Parent {
                    value() { return super.value() * 3 + 1; }
                }
                var subject = new Child();
                subject.n = 4;
            "#);
        let subject = global(&vm, "subject");
        let fid = vm
            .program
            .functions
            .iter()
            .position(|proto| proto.source.contains("super.value() * 3"))
            .expect("Child.value function") as u32;

        assert!(
            vm.method_body_inlinable_scan(fid).is_some(),
            "the captured super-call pair must pass the shape scan"
        );
        assert_eq!(
            vm.try_method_inline(fid, subject, 0, 0, 0),
            Some(Value::int(16).bits()),
            "the exact captured Parent.value target must run off-frame"
        );
    }

    /// A nested off-frame super call must not commit its own SuperSet and then
    /// let a later outer guard decline to a full-frame replay of the method.
    #[test]
    fn nested_effectful_super_target_declines_before_its_store() {
        let mut vm = vm(r#"
                class Parent { set value(v) { this.n = v | 0; } }
                class Middle extends Parent {
                    bump() {
                        const next = this.n + 1;
                        super.value = next;
                        return next;
                    }
                }
                class Child extends Middle {
                    run() {
                        const next = super.bump();
                        return next + this.tail;
                    }
                }
                var subject = new Child();
                subject.n = 0;
                subject.tail = 1;
                subject.run();
                subject.tail = {};
            "#);
        let subject = global(&vm, "subject");
        let run_fid = vm
            .program
            .functions
            .iter()
            .position(|proto| proto.source.contains("const next = super.bump()"))
            .expect("Child.run function") as u32;
        let before = vm.get_prop(subject, "n").expect("read n");

        assert_eq!(
            vm.try_method_inline(run_fid, subject, 0, 0, 0),
            None,
            "the outer non-number Add must decline"
        );
        assert_eq!(
            vm.get_prop(subject, "n").expect("read n after decline"),
            before,
            "a decline must not retain the nested super setter's write"
        );
    }

    /// A metered VM must use the ordinary frame path, which checks the budget
    /// before each interpreted op/native block rather than after off-loop work.
    #[cfg(feature = "instrument")]
    #[test]
    fn metered_method_body_declines_before_off_loop_work() {
        let mut vm = vm(r#"
                class Subject {
                    value() { return this.n + 1; }
                }
                var subject = new Subject();
                subject.n = 20;
            "#);
        let subject = global(&vm, "subject");
        let fid = vm
            .program
            .functions
            .iter()
            .position(|proto| proto.source.contains("return this.n + 1"))
            .expect("Subject.value function") as u32;
        vm.set_instrumentation(crate::vm::instrument::Recorder::new());
        let before = vm.instr_rec.as_ref().expect("recorder attached").used;

        assert_eq!(
            vm.try_method_inline(fid, subject, 0, 0, 0),
            None,
            "nested work must fall back to the exactly metered frame path"
        );
        assert_eq!(
            vm.instr_rec.as_ref().unwrap().used,
            before,
            "a declined speculative path must not consume metered work"
        );
    }

    /// Direct accessor shortcuts also bypass the dispatch loop. They must not
    /// read or write a JS accessor body without charging it in a metered VM.
    #[cfg(feature = "instrument")]
    #[test]
    fn metered_accessor_shortcuts_decline_before_read_or_write() {
        let mut vm = vm(r#"
                class Subject {
                    get value() { return this.n; }
                    set value(x) { this.n = x | 0; }
                }
                var subject = new Subject();
                subject.n = 20;
            "#);
        let subject = global(&vm, "subject");
        let getter = vm
            .program
            .functions
            .iter()
            .position(|proto| proto.source.contains("return this.n"))
            .expect("value getter") as u32;
        let setter = vm
            .program
            .functions
            .iter()
            .position(|proto| proto.source.contains("this.n = x | 0"))
            .expect("value setter") as u32;
        vm.set_instrumentation(crate::vm::instrument::Recorder::new());
        let before_steps = vm.instr_rec.as_ref().expect("recorder attached").used;
        let before_value = vm.get_prop(subject, "n").expect("read n");

        assert_eq!(vm.accessor_fast_get(getter, subject), None);
        assert_eq!(vm.accessor_fast_set(setter, subject, Value::int(99)), None);
        assert_eq!(
            vm.get_prop(subject, "n").expect("read n after decline"),
            before_value,
            "the metered setter shortcut must decline before its store"
        );
        assert_eq!(
            vm.instr_rec.as_ref().unwrap().used,
            before_steps,
            "declined accessor shortcuts must not consume untracked work"
        );
    }
}
