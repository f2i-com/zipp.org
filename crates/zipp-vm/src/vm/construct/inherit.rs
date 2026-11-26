// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

env_off_switch! {
    /// `ZIPP_NO_INSTANCEOF_MEMO=1`: every `instanceof` against an ordinary
    /// function re-checks its right-hand side (`Vm::instanceof_memo`).
    fn instanceof_memo_enabled() = "ZIPP_NO_INSTANCEOF_MEMO"
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn instanceof_memo_on() -> bool {
    instanceof_memo_enabled()
}

/// Whether a function's own-property side table holds `@@hasInstance`, and
/// whether it holds `prototype`: the two keys `instanceof` asks of a plain
/// function. Side tables have no shape for the (shape, key) memo, so each
/// `pos` scanned every key: two scans per test of an `instanceof` against a
/// constructor with a few statics (a minifier's AST node classes, 2.95M tests
/// in a run). One pass answers both; an indexed table keeps its lookups.
fn own_has_instance_and_prototype(m: &crate::heap::ObjMap) -> (bool, bool) {
    if m.is_indexed() {
        return (m.pos("@@hasInstance").is_some(), m.pos("prototype").is_some());
    }
    let (mut has_instance, mut prototype) = (false, false);
    for k in m.keys_iter() {
        match k.len() {
            13 => has_instance |= k == "@@hasInstance",
            9 => prototype |= k == "prototype",
            _ => {}
        }
    }
    (has_instance, prototype)
}

/// Whether class `cc` has no own `@@hasInstance` (a static method, field or
/// accessor), remembered by the shape of its `statics`
/// (`ClassData::no_has_instance_shape`): every `instanceof` against a class
/// asked each level of its chain, a key scan of the statics apiece.
#[inline]
fn class_lacks_has_instance(cc: &crate::heap::ClassData) -> bool {
    let shape = cc.statics.shape();
    let no_accessors = cc.static_getters.is_empty() && cc.static_setters.is_empty();
    if no_accessors && cc.statics.shape_guardable() && cc.no_has_instance_shape.get() == shape {
        return true;
    }
    if cc.statics.pos("@@hasInstance").is_some()
        || cc.static_getters.iter().any(|(k, _)| k == "@@hasInstance")
        || cc.static_setters.iter().any(|(k, _)| k == "@@hasInstance")
    {
        return false;
    }
    if no_accessors && cc.statics.shape_guardable() {
        cc.no_has_instance_shape.set(shape);
    }
    true
}

impl<'p> Vm<'p> {
    /// Build the callable for a class constructor: a plain `Func`, or a `Closure`
    /// over the cells the ctor captured (at class-definition time) when it closes
    /// over an enclosing-function local.
    /// The function value `new` runs the class `cv`'s constructor through
    /// (`thunk`: its instance-field thunk), made the first time and kept on
    /// the class; `None` when the class has none.
    ///
    /// Every construction used to make it again: a function object, a copy
    /// of the upvalue list and of the class's brand chain, and a brand-table
    /// entry for the new object, at each level of the chain and again for
    /// each field thunk. Nothing can tell two of them apart: the value is
    /// only ever a frame's callee (a class constructor cannot be reached
    /// through `arguments.callee` or a caller property, both strict-mode
    /// errors), `ctor` and its upvalue cells are fixed when the class is
    /// made, and so is the class's brand chain.
    pub(crate) fn class_body_fn(&mut self, cv: Value, thunk: bool) -> Option<Value> {
        let ci = cv.heap_index();
        let (fid, ups, kept) = match self.heap.get(ci) {
            HeapObj::Class(c) if thunk => (c.field_thunk?, &c.field_thunk_upvalues, c.thunk_fn),
            HeapObj::Class(c) => (c.ctor?, &c.ctor_upvalues, c.ctor_fn),
            _ => return None,
        };
        if kept.is_heap() {
            return Some(kept);
        }
        let ups = ups.clone();
        let f = self.ctor_value(fid, &ups);
        // The class body's private scope: the class's lexical brand chain.
        if let Some(brands) = self.method_brand.get(&ci).cloned() {
            self.method_brand.insert(f.heap_index(), brands);
            self.ctor_owner.insert(f.heap_index(), ci);
        }
        // An old class may now hold a young function.
        self.heap.write_barrier_val(ci, f);
        if let HeapObj::Class(c) = self.heap.get_mut(ci) {
            if thunk {
                c.thunk_fn = f;
            } else {
                c.ctor_fn = f;
            }
        }
        Some(f)
    }

    pub(crate) fn ctor_value(&mut self, fid: u32, ups: &[u32]) -> Value {
        if ups.is_empty() {
            Value::heap(self.heap.alloc(HeapObj::Func(fid)))
        } else {
            Value::heap(self.heap.alloc(HeapObj::Closure {
                func: fid,
                upvalues: ups.to_vec(),
                this_val: Value::UNDEFINED,
            }))
        }
    }

    /// OrdinaryHasInstance(C, O) — the algorithm behind both the default
    /// `Function.prototype[Symbol.hasInstance]` method and the `instanceof`
    /// operator's non-overridden path. A bound function resolves to its target; a
    /// throwing `prototype` getter / non-object prototype propagates per spec
    /// (unlike the operator's cached-prototype fast path).
    /// `v instanceof c`: GetMethod(c, @@hasInstance) and its call, or
    /// OrdinaryHasInstance; `instanceof_fast` first. The interpreter's
    /// `InstanceOfDyn` and the compiled tiers' helper (`jit_instanceof_ic`)
    /// both run it.
    pub(crate) fn instance_of_op(&mut self, c: Value, v: Value) -> Result<bool, Thrown> {
        if let Some(b) = self.instanceof_fast(c, v) {
            return Ok(b);
        }
        // GetMethod(RHS, @@hasInstance): an inherited or own callable
        // handler fully governs `instanceof`; invoke it with the LHS
        // and coerce the result to boolean. If the method is absent,
        // the checked OrdinaryHasInstance path below is the only
        // fallback.
        if c.is_heap() {
            // A class's own static data method (`static [Symbol.hasInstance]`)
            // is what the Get returns: read it there, not through the generic
            // property path's class arm.
            let own = match self.heap.get(c.heap_index()) {
                HeapObj::Class(cc)
                    if !cc.static_getters.iter().any(|(k, _)| k == "@@hasInstance")
                        && !cc.static_setters.iter().any(|(k, _)| k == "@@hasInstance") =>
                {
                    cc.statics
                        .pos("@@hasInstance")
                        .filter(|&i| !cc.statics.attr_at(i).accessor)
                        .map(|i| cc.statics.val_at(i))
                }
                _ => None,
            };
            let hi = match own {
                Some(h) => h,
                None => self.get_prop(c, "@@hasInstance")?,
            };
            // The built-in Function.prototype[@@hasInstance] is
            // OrdinaryHasInstance.  Invoke that algorithm directly
            // (rather than skipping the method by native-function
            // identity): users may deliberately install the same
            // intrinsic method on an otherwise non-callable object,
            // where it must return false instead of falling through
            // to the operator's "RHS not callable" TypeError.
            let is_builtin = hi.is_heap()
                && matches!(self.heap.get(hi.heap_index()),
                    HeapObj::Native(n) if *n == native::FN_HAS_INSTANCE);
            if self.is_callable(hi) {
                let b = if is_builtin {
                    self.ordinary_has_instance(c, v)?
                } else {
                    let res = self.call_value(hi, c, &[v])?;
                    self.truthy(res)
                };
                return Ok(b);
            }
            // GetMethod step 3: a @@hasInstance that is PRESENT but
            // not callable is a TypeError — it never falls back to
            // OrdinaryHasInstance. Only undefined/null count as
            // absent. A plain-object RHS reached the same TypeError
            // by accident (kind 0 below), but a callable RHS — a
            // Proxy over a function, say — silently answered `false`.
            if hi != Value::UNDEFINED && hi != Value::NULL && !self.is_callable(hi)
            {
                return Err(Thrown(
                    "TypeError: Symbol.hasInstance handler is not callable".into(),
                ));
            }
        }
        // With no @@hasInstance method, the RHS must be callable and
        // OrdinaryHasInstance is authoritative. In particular, do
        // not consult ObjMap.class: Object.setPrototypeOf can unlink
        // an instance while that allocation-time lineage remains.
        // `Symbol`/`BigInt` are callable globals (typeof "function")
        // but not constructors (is_ctor false), so `is_callable` skips
        // them; for `instanceof` they ARE valid right operands â€”
        // OrdinaryHasInstance reads their .prototype and yields false
        // for a non-wrapper LHS (e.g. `x instanceof Symbol`), never a
        // "not callable" TypeError. (deepEqual.js guards these with
        // `typeof X === 'function'`, then does `v instanceof X`.)
        let c_callable = c.is_heap()
            && (self.is_callable(c)
                || (self.symbol_ctor != 0 && c.heap_index() == self.symbol_ctor)
                || (self.bigint_ctor != 0 && c.heap_index() == self.bigint_ctor));
        if !c_callable {
            return Err(Thrown(
                "TypeError: Right-hand side of 'instanceof' is not callable".into(),
            ));
        }
        self.ordinary_has_instance(c, v)
    }

    /// `v instanceof c` for a plain function or class `c`, when nothing can
    /// observe the difference from the full operator: no `@@hasInstance` on
    /// `c`, on a parent class or on a replaced [[Prototype]] can shadow
    /// %Function.prototype%'s (which is non-writable and non-configurable, so
    /// it is the intrinsic OrdinaryHasInstance), `c.prototype` is read without
    /// a getter, and no Proxy is on `v`'s chain (its trap would be observable).
    /// `None`, with nothing observable done, sends the caller to the full
    /// operator. The general path paid a property lookup of `@@hasInstance` up
    /// the class's statics and %Function.prototype% for every test.
    /// Extract the immediate prototype/class key of heap value `v` for
    /// `instanceof_pair_memo`: `FID_MIRROR_CLASS | class` for a non-overridden
    /// class instance, or the immediate prototype heap index (`< FID_MIRROR_CLASS`)
    /// for a plain object or dense array. Returns `0` for non-objects, proxies,
    /// `null`-prototype objects, or anything needing the full walk.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(always)]
    fn instanceof_v_key(&self, v: Value) -> u32 {
        // Sentinel key for all non-object values (primitives, strings, symbols,
        // BigInts): strictly below `FID_MIRROR_CLASS` and never a valid heap
        // prototype index.
        const VKEY_NON_OBJ: u32 = crate::heap::FID_MIRROR_STR;
        if !v.is_heap() {
            return VKEY_NON_OBJ;
        }
        let vi = v.heap_index();
        let mir = self.heap.hot_mirror_of(vi);
        if mir.fid == crate::heap::FID_MIRROR_STR {
            return VKEY_NON_OBJ;
        }
        if mir.fid >= crate::heap::FID_MIRROR_CLASS && mir.fid != crate::heap::FID_MIRROR_NONE {
            return if !self.proto_of.has(vi) {
                mir.fid
            } else {
                match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    _ => 0,
                }
            };
        }
        #[cfg(target_arch = "x86_64")]
        if mir.shape != crate::shape::DICT && mir.fid == crate::heap::FID_MIRROR_NONE {
            return match self.proto_of.get_copied(vi) {
                Some(p) if p.is_heap() => p.heap_index(),
                Some(_) => 0,
                None => self.obj_proto,
            };
        }
        #[cfg(target_arch = "x86_64")]
        if self.heap.arr_mirror_live(vi) {
            return match self.proto_of.get_copied(vi) {
                Some(p) if p.is_heap() => p.heap_index(),
                Some(_) => 0,
                None => self.arr_proto,
            };
        }
        match self.heap.get(vi) {
            HeapObj::Object(m) => {
                if let Some(c) = m.class {
                    if !m.is_ctor && !m.class_proto && !self.proto_of.has(vi) {
                        let k = crate::heap::class_mirror_fid(c);
                        if k != crate::heap::FID_MIRROR_NONE {
                            return k;
                        }
                    }
                }
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => if m.is_ctor { self.fn_proto } else { self.obj_proto },
                };
            }
            HeapObj::Array(_) => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.arr_proto,
                };
            }
            HeapObj::Date(_) => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.date_proto,
                };
            }
            HeapObj::RegExp(_) => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.regexp_proto,
                };
            }
            HeapObj::Map { .. } => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.map_proto,
                };
            }
            HeapObj::Set(_) => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.set_proto,
                };
            }
            HeapObj::TypedArray { kind, .. } => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.ta_protos.get(*kind as usize).copied().unwrap_or(0),
                };
            }
            HeapObj::Func(fid) | HeapObj::Closure { func: fid, .. } => {
                let f = self.func(*fid as usize);
                if f.is_generator || f.is_async {
                    return 0;
                }
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.fn_proto,
                };
            }
            HeapObj::Native(_) | HeapObj::Bound { .. } => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.fn_proto,
                };
            }
            HeapObj::Promise { .. } => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.promise_proto,
                };
            }
            HeapObj::ArrayBuffer { .. } => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.arraybuffer_proto,
                };
            }
            HeapObj::DataView { .. } => {
                return match self.proto_of.get_copied(vi) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => 0,
                    None => self.dataview_proto,
                };
            }
            _ => {}
        }
        if !self.is_object_value(v) {
            return VKEY_NON_OBJ;
        }
        0
    }

    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(always)]
    fn instanceof_pair_slot(v_key: u32, ci: u32) -> usize {
        ((v_key.wrapping_mul(0x9E37_79B1) ^ ci.wrapping_mul(0x85EB_CA77)).wrapping_mul(0x27D4_EB2F)
            >> 20) as usize
            & (crate::vm::INSTANCEOF_PAIR_SLOTS - 1)
    }

    pub(crate) fn instanceof_fast(&mut self, c: Value, v: Value) -> Option<bool> {
        if !c.is_heap() {
            return None;
        }
        let ci = c.heap_index();
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        let v_key = if instanceof_memo_enabled() {
            let vk = self.instanceof_v_key(v);
            if vk != 0 {
                let e = self.instanceof_pair_memo[Self::instanceof_pair_slot(vk, ci)];
                if e.v_key == vk
                    && e.ci == ci
                    && e.chain_epoch == self.heap.chain_epoch
                    && e.fn_props_gen == self.fn_props_gen
                    && e.class_proto_epoch == self.class_proto_epoch
                {
                    return Some(e.result);
                }
            }
            vk
        } else {
            0
        };
        // An ordinary function whose checks below passed since its version
        // and `fn_props` last changed (`instanceof_memo`): its prototype.
        let memo = self.instanceof_memo[ci as usize & (crate::vm::INSTANCEOF_MEMO_SLOTS - 1)];
        let p = if memo.idx == ci
            && instanceof_memo_enabled()
            && memo.ver == self.heap.version_of(ci)
            && memo.gen == self.fn_props_gen
            && !self.proto_of.has(ci)
        {
            memo.proto
        } else {
            match self.instanceof_rhs_checked(c, v)? {
                Ok(p) => p,
                Err(answer) => {
                    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
                    if v_key != 0 {
                        // Guard `ci` (+ its parent classes) and `v`'s class chain
                        // so any structural/prototype change bumps `chain_epoch`.
                        self.guard_instanceof_class_chain(ci);
                        if v_key >= crate::heap::FID_MIRROR_CLASS && v_key != crate::heap::FID_MIRROR_NONE {
                            self.guard_instanceof_class_chain(v_key & !crate::heap::FID_MIRROR_CLASS);
                        }
                        self.instanceof_pair_memo[Self::instanceof_pair_slot(v_key, ci)] =
                            crate::vm::InstanceofPairMemo {
                                v_key,
                                ci,
                                chain_epoch: self.heap.chain_epoch,
                                fn_props_gen: self.fn_props_gen,
                                class_proto_epoch: self.class_proto_epoch,
                                result: answer,
                                _pad: [0; 11],
                            };
                    }
                    return Some(answer);
                }
            }
        };
        if !self.is_object_value(v) {
            #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
            if v_key != 0 {
                self.guard_instanceof_class_chain(ci);
                self.instanceof_pair_memo[Self::instanceof_pair_slot(v_key, ci)] =
                    crate::vm::InstanceofPairMemo {
                        v_key,
                        ci,
                        chain_epoch: self.heap.chain_epoch,
                        fn_props_gen: self.fn_props_gen,
                        class_proto_epoch: self.class_proto_epoch,
                        result: false,
                        _pad: [0; 11],
                    };
            }
            return Some(false);
        }
        let mut cur = v;
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        let mut hops_buf = [0u32; 16];
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        let mut nhops = 0usize;
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        let mut cacheable = v_key != 0;
        for _ in 0..10_000 {
            cur = match self.instanceof_proto_step(cur) {
                Some(next) => next,
                None => {
                    if self.proxy_parts(cur.heap_index()).is_some() {
                        return None;
                    }
                    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
                    {
                        cacheable = false;
                    }
                    self.object_get_prototype_of(cur)
                }
            };
            if !cur.is_heap() {
                #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
                if cacheable {
                    self.guard_instanceof_pair_and_store(v_key, ci, &hops_buf[..nhops], false);
                }
                return Some(false);
            }
            #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
            if cacheable {
                if nhops < hops_buf.len() {
                    hops_buf[nhops] = cur.heap_index();
                    nhops += 1;
                } else {
                    cacheable = false;
                }
            }
            if cur == p {
                #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
                if cacheable {
                    self.guard_instanceof_pair_and_store(v_key, ci, &hops_buf[..nhops], true);
                }
                return Some(true);
            }
        }
        None
    }

    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    fn guard_instanceof_class_chain(&self, start: u32) {
        let mut cur = start;
        for _ in 0..64 {
            self.heap.guard_version(cur);
            if let Some(p) = self.proto_of.get_copied(cur) {
                if p.is_heap() {
                    let next = p.heap_index();
                    if next == self.fn_proto || next == cur {
                        break;
                    }
                    cur = next;
                    continue;
                }
                break;
            }
            match self.heap.get(cur) {
                HeapObj::Class(cc) => match cc.parent {
                    Some(p) => cur = p,
                    None => break,
                },
                _ => break,
            }
        }
        if self.fn_proto != 0 {
            self.heap.guard_version(self.fn_proto);
        }
    }

    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    fn guard_instanceof_pair_and_store(&mut self, v_key: u32, ci: u32, hops: &[u32], result: bool) {
        self.guard_instanceof_class_chain(ci);
        if v_key >= crate::heap::FID_MIRROR_CLASS && v_key != crate::heap::FID_MIRROR_NONE {
            self.guard_instanceof_class_chain(v_key & !crate::heap::FID_MIRROR_CLASS);
        }
        for &h in hops {
            self.heap.guard_version(h);
        }
        self.instanceof_pair_memo[Self::instanceof_pair_slot(v_key, ci)] =
            crate::vm::InstanceofPairMemo {
                v_key,
                ci,
                chain_epoch: self.heap.chain_epoch,
                fn_props_gen: self.fn_props_gen,
                class_proto_epoch: self.class_proto_epoch,
                result,
                _pad: [0; 11],
            };
    }

    /// The right-hand side half of [`Self::instanceof_fast`]: `None` when
    /// `c` needs the full operator, `Some(Err(answer))` when a class walk
    /// answered, else `Some(Ok(p))` with `c.prototype`, an object, which an
    /// ordinary function also leaves in `instanceof_memo`.
    fn instanceof_rhs_checked(&mut self, c: Value, v: Value) -> Option<Result<Value, bool>> {
        let ci = c.heap_index();
        let mut cur = ci;
        let mut chain_ok = false;
        // Whether `c` itself, a function, has an own `prototype`, when the
        // walk below already looked at its side table.
        let mut ci_own_prototype = None;
        // Builtin `is_ctor` object's immutable own `prototype` value.
        let mut ctor_proto_val: Option<Value> = None;
        for _ in 0..64 {
            match self.heap.get(cur) {
                HeapObj::Func(_) | HeapObj::Closure { .. } => {
                    let (has_instance, prototype) = match self.fn_props.get(&cur) {
                        Some(m) => own_has_instance_and_prototype(m),
                        None => (false, false),
                    };
                    if has_instance {
                        return None;
                    }
                    if cur == ci {
                        ci_own_prototype = Some(prototype);
                    }
                    match self.proto_of.get_copied(cur) {
                        None => {
                            chain_ok = true;
                            break;
                        }
                        Some(p) if !p.is_heap() => {
                            chain_ok = true;
                            break;
                        }
                        Some(p) if p.heap_index() == self.fn_proto => {
                            chain_ok = true;
                            break;
                        }
                        Some(p) => {
                            // Follow ES5 transpiled subclass chains
                            // (`_inherits(Sub, Super)` -> `Sub.__proto__ = Super`).
                            cur = p.heap_index();
                        }
                    }
                }
                HeapObj::Class(cc) => {
                    if !class_lacks_has_instance(cc) || self.proto_of.has(cur) {
                        return None;
                    }
                    match cc.parent {
                        // A base class (and `extends null`) inherits from
                        // %Function.prototype% itself.
                        None => {
                            chain_ok = true;
                            break;
                        }
                        Some(p) => cur = p,
                    }
                }
                HeapObj::Object(m) if m.is_ctor && m.class.is_none() && self.ic_obj_ok(cur) => {
                    if crate::vm::ic::own_pos_in(&self.own_slot_cache, m, "@@hasInstance").is_some()
                    {
                        return None;
                    }
                    if cur == ci {
                        let pi = crate::vm::ic::own_pos_in(&self.own_slot_cache, m, "prototype")?;
                        let a = m.attr_at(pi);
                        if a.accessor || a.writable || a.configurable {
                            return None;
                        }
                        let pv = m.val_at(pi);
                        if !self.is_object_value(pv) {
                            return None;
                        }
                        ctor_proto_val = Some(pv);
                    }
                    match self.proto_of.get_copied(cur) {
                        None => {
                            chain_ok = true;
                            break;
                        }
                        Some(p) if !p.is_heap() => {
                            chain_ok = true;
                            break;
                        }
                        Some(p) if p.heap_index() == self.fn_proto => {
                            chain_ok = true;
                            break;
                        }
                        Some(p) => {
                            // E.g. `Float32Array -> %TypedArray%`, `TypeError -> Error`.
                            cur = p.heap_index();
                        }
                    }
                }
                _ => return None,
            }
        }
        if !chain_ok {
            return None;
        }
        if let Some(pv) = ctor_proto_val {
            return Some(Ok(pv));
        }
        // C.prototype: a function that has none of its own (an arrow, a
        // method; the general path throws), or an own accessor / non-object
        // `prototype` keeps the full path. An own DATA object `prototype` in
        // `fn_props` (ES5 `Sub.prototype = Object.create(Base.prototype)`) is
        // safe because every `fn_props` write bumps `fn_props_gen`.
        let mut fn_own_proto_val: Option<Value> = None;
        if matches!(self.heap.get(ci), HeapObj::Func(_) | HeapObj::Closure { .. }) {
            let Some((fid, _)) = self.ic_plain_fn(c) else {
                return None;
            };
            let f = self.func(fid as usize);
            if f.non_constructable || f.lexical_this {
                return None;
            }
            let has_own_proto = ci_own_prototype.unwrap_or_else(|| {
                self.fn_props.get(&ci).is_some_and(|m| m.pos("prototype").is_some())
            });
            if has_own_proto {
                let m = self.fn_props.get(&ci)?;
                let i = m.pos("prototype")?;
                if m.attr_at(i).accessor {
                    return None;
                }
                let pv = m.val_at(i);
                if !self.is_object_value(pv) {
                    return None;
                }
                fn_own_proto_val = Some(pv);
            }
        }
        if let Some(b) = self.instanceof_class_walk(ci, v) {
            return Some(Err(b));
        }
        let p = match fn_own_proto_val {
            Some(pv) => pv,
            None => self.prototype_of(c)?,
        };
        if !self.is_object_value(p) {
            return None;
        }
        if instanceof_memo_enabled() && matches!(self.heap.get(ci), HeapObj::Func(_) | HeapObj::Closure { .. }) {
            self.instanceof_memo[ci as usize & (crate::vm::INSTANCEOF_MEMO_SLOTS - 1)] = crate::vm::InstanceofMemo {
                idx: ci,
                ver: self.heap.version_of(ci),
                gen: self.fn_props_gen,
                proto: p,
            };
        }
        Some(Ok(p))
    }

    /// `v instanceof C` for a class `C` (heap `ci`) and a class instance `v`
    /// with no [[Prototype]] of its own, by its class chain: `v`'s chain is
    /// its class's prototype, then each parent class's, as long as no level's
    /// prototype object was re-prototyped or otherwise retired
    /// (`proto_dirty`, sticky). Distinct classes have distinct prototypes, so
    /// `C.prototype` is on the chain exactly when `C` is one of the classes.
    /// `None` for anything else, including a level whose parent is not a
    /// class. The prototype walk paid a side-table lookup per hop.
    #[inline]
    fn instanceof_class_walk(&self, ci: u32, v: Value) -> Option<bool> {
        if !v.is_heap() || !matches!(self.heap.get(ci), HeapObj::Class(_)) {
            return None;
        }
        let vi = v.heap_index();
        let mut cur = match self.heap.get(vi) {
            HeapObj::Object(m) if !m.is_ctor && !m.class_proto => m.class?,
            _ => return None,
        };
        if self.proto_of.contains_key(&vi) {
            return None;
        }
        for _ in 0..64 {
            if cur == ci {
                return Some(true);
            }
            let HeapObj::Class(c) = self.heap.get(cur) else {
                return None;
            };
            if c.proto_dirty {
                return None;
            }
            match c.parent {
                // A base class's prototype inherits from %Object.prototype%
                // (or null), which no class's prototype is.
                None => return Some(false),
                Some(p) => cur = p,
            }
        }
        None
    }

    /// `object_get_prototype_of(cur)` for the objects an `instanceof` walk
    /// meets most, an ordinary object or class instance (`None`: ask it).
    /// The general one tested for a Proxy, a class value, a dozen built-in
    /// kinds and a callable before reaching an instance's class: a tenth of
    /// a loop of `o instanceof C`.
    #[inline]
    fn instanceof_proto_step(&mut self, cur: Value) -> Option<Value> {
        let idx = cur.heap_index();
        if let Some(p) = self.proto_of.get_copied(idx) {
            return if matches!(self.heap.get(idx), HeapObj::Proxy { .. }) {
                None
            } else {
                Some(p)
            };
        }
        if idx == self.obj_proto {
            return Some(Value::NULL);
        }
        match self.heap.get(idx) {
            HeapObj::Object(m) if !m.is_ctor => match m.class {
                Some(cidx) => self.prototype_of(Value::heap(cidx)),
                None if self.obj_proto != 0 => Some(Value::heap(self.obj_proto)),
                None => None,
            },
            HeapObj::Object(m) if m.is_ctor && self.fn_proto != 0 => {
                Some(Value::heap(self.fn_proto))
            }
            HeapObj::Array(_) if self.arr_proto != 0 => Some(Value::heap(self.arr_proto)),
            HeapObj::Date(_) if self.date_proto != 0 => Some(Value::heap(self.date_proto)),
            HeapObj::RegExp(_) if self.regexp_proto != 0 => Some(Value::heap(self.regexp_proto)),
            HeapObj::Map { .. } if self.map_proto != 0 => Some(Value::heap(self.map_proto)),
            HeapObj::Set(_) if self.set_proto != 0 => Some(Value::heap(self.set_proto)),
            HeapObj::TypedArray { kind, .. } => self
                .ta_protos
                .get(*kind as usize)
                .copied()
                .filter(|&p| p != 0)
                .map(Value::heap),
            HeapObj::Promise { .. } if self.promise_proto != 0 => {
                Some(Value::heap(self.promise_proto))
            }
            HeapObj::ArrayBuffer { .. } if self.arraybuffer_proto != 0 => {
                Some(Value::heap(self.arraybuffer_proto))
            }
            HeapObj::DataView { .. } if self.dataview_proto != 0 => {
                Some(Value::heap(self.dataview_proto))
            }
            HeapObj::Func(fid) | HeapObj::Closure { func: fid, .. } => {
                let f = self.func(*fid as usize);
                if !f.is_generator && !f.is_async && self.fn_proto != 0 {
                    Some(Value::heap(self.fn_proto))
                } else {
                    None
                }
            }
            HeapObj::Native(_) | HeapObj::Bound { .. } if self.fn_proto != 0 => {
                Some(Value::heap(self.fn_proto))
            }
            _ => None,
        }
    }

    pub(crate) fn ordinary_has_instance(&mut self, mut c: Value, v: Value) -> Result<bool, Thrown> {
        // Bound functions are guest-chainable. OrdinaryHasInstance delegates to
        // [[BoundTargetFunction]], but doing that with a Rust self-call lets one
        // `instanceof` exhaust the native stack before any VM frame limit runs.
        loop {
            let target = match c.is_heap().then(|| self.heap.get(c.heap_index())) {
                Some(HeapObj::Bound { target, .. }) => Some(*target),
                _ => None,
            };
            match target {
                Some(target) => c = target,
                None => break,
            }
        }
        if !self.is_callable(c) {
            // Symbol/BigInt are callable globals (typeof "function") that
            // is_callable reports false for (no user-invocable [[Construct]]);
            // as `instanceof` right operands they still take the ordinary
            // prototype-chain path (`Object(Symbol()) instanceof Symbol`).
            let special = c.is_heap()
                && ((self.symbol_ctor != 0 && c.heap_index() == self.symbol_ctor)
                    || (self.bigint_ctor != 0 && c.heap_index() == self.bigint_ctor));
            if !special {
                return Ok(false);
            }
        }
        if !self.is_object_value(v) {
            return Ok(false);
        }
        // C.prototype: a non-object DATA reassignment (`f.prototype = undefined`)
        // is recorded in fn_props because the lazy prototype cache only holds
        // object prototypes (get_prop returns that cache), so read such an override
        // directly. An accessor (`defineProperty` get) or an object prototype still
        // goes through get_prop — so a throwing prototype getter propagates.
        let data_override = if c.is_heap() {
            self.fn_props.get(&c.heap_index()).and_then(|m| {
                m.pos("prototype")
                    .and_then(|i| (!m.attr_at(i).accessor).then(|| m.val_at(i)))
            })
        } else {
            None
        };
        let p = match data_override {
            Some(v) => v,
            None => self.get_prop(c, "prototype")?,
        };
        if !self.is_object_value(p) {
            // Node's wording, which names the value.
            return Err(Thrown::join3("TypeError: Function has non-object prototype '", &self.display(p), "' in instanceof check"));
        }
        // Walk V's prototype chain via [[GetPrototypeOf]] (proxy-trap-aware): a
        // throwing getPrototypeOf trap propagates per OrdinaryHasInstance step 7.b.
        let mut cur = self.get_prototype_of_checked(v)?;
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return Ok(false);
            }
            if cur == p {
                return Ok(true);
            }
            cur = self.get_prototype_of_checked(cur)?;
        }
        Ok(false)
    }

    /// The class VALUE the constructor frame on top of the stack belongs to.
    ///
    /// `class_values[id]` holds only the LATEST evaluation of a class
    /// expression, so a chain built by re-evaluating one expression —
    ///     let chain = base;
    ///     for (let i = 0; i < 100; i++)
    ///         chain = class extends chain { constructor() { super(); } };
    ///     new chain();                      // staging/sm/class/superPropChains.js
    /// — made every level's `super()` resolve GetSuperConstructor against the
    /// NEWEST class, whose parent is the level below it: an infinite recursion
    /// that overflowed the native stack and ABORTED the process (not even a
    /// catchable RangeError). 15.7.16 GetSuperConstructor reads the *running*
    /// function object's [[GetPrototypeOf]], so recover that specific evaluation
    /// here.
    ///
    /// The link is the private brand, which is already minted per class
    /// EVALUATION: `method_brand[classValue] = [ownBrand, ...enclosing]` and
    /// `brand_owner[ownBrand] = classValue`, and run_class_ctor/[[Construct]]
    /// copy `method_brand` from the class value onto the freshly allocated ctor
    /// callable it invokes — which is this frame's `callee`. The
    /// `ctor == frame.func` check proves the recovered class really owns the
    /// running frame (a recycled heap index cannot fake it), and `None` leaves
    /// every caller on the previous `class_values` path.
    pub(crate) fn running_class_value(&self) -> Option<Value> {
        let f = self.frames.last()?;
        let callee = f.callee;
        if !callee.is_heap() {
            return None;
        }
        // The kept constructor callables answer in one load (`ctor_owner`).
        if let Some(&owner) = self.ctor_owner.get(&callee.heap_index()) {
            if let HeapObj::Class(c) = self.heap.get(owner) {
                if c.ctor == Some(f.func) {
                    return Some(Value::heap(owner));
                }
            }
        }
        let brand = *self.method_brand.get(&callee.heap_index())?.first()?;
        let owner = *self.brand_owner.get(&brand)?;
        match self.heap.get(owner) {
            HeapObj::Class(c) if c.ctor == Some(f.func) => Some(Value::heap(owner)),
            _ => None,
        }
    }

    /// The runtime class value compile-time class `class_id` denotes for the
    /// running code. `class_values` holds only the latest evaluation, which is
    /// the answer for a class evaluated once. For one evaluated more than once
    /// (a class expression inside a function called twice, a mixin applied to
    /// two bases) it is the evaluation the running code belongs to: the first
    /// brand on the running callee's lexical chain whose owner came from
    /// `class_id`, as `running_class_value` and `dec_class_of_id` recover it.
    /// With `class_values` alone, `super.m()`, `super.x = v`, a method's use of
    /// its own class's name and a computed field key in every earlier
    /// evaluation's code resolved against the newest one. Code outside the
    /// class (its heritage, or a computed key while the class is defined) has
    /// no brand for it and gets the latest, which is the one being defined.
    ///
    /// The running callee is the top frame's, so a frame-free activation (an
    /// inlined method body, a Tier C function) must not ask for a repeated
    /// class: `class_repeated` says when to decline, and
    /// `class_value_for_callee` takes the activation's own callee.
    pub(crate) fn class_value_for(&self, class_id: u32) -> Option<Value> {
        let callee = self.frames.last().map_or(Value::UNDEFINED, |f| f.callee);
        self.class_value_for_callee(class_id, callee)
    }

    /// [`Self::class_value_for`] for code running as `callee`.
    pub(crate) fn class_value_for_callee(&self, class_id: u32, callee: Value) -> Option<Value> {
        let slot = self.class_values.get(class_id as usize)?;
        if !slot.repeated {
            return slot.latest;
        }
        self.class_value_in_chain(self.private_brands_of(callee), class_id)
            .or(slot.latest)
    }

    /// The value `LoadClassValue` of `class_id` has for all running code
    /// while no class is evaluated again (`mi_class_epoch` unmoved): a class
    /// evaluated exactly once, undecorated or with no replacement. Tier O
    /// bakes it.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn class_value_fixed(&self, class_id: u32) -> Option<Value> {
        let slot = self.class_values.get(class_id as usize)?;
        let v = slot.latest.filter(|_| !slot.repeated)?;
        match self.heap.get(v.heap_index()) {
            HeapObj::Class(c) if c.dec.as_ref().is_some_and(|d| d.replacement.is_heap()) => None,
            HeapObj::Class(_) => Some(v),
            _ => None,
        }
    }

    /// Whether compile-time class `class_id` has been evaluated more than once.
    /// Asked by the frame-free JIT paths only.
    #[cfg_attr(not(all(feature = "jit", target_arch = "x86_64")), allow(dead_code))]
    pub(crate) fn class_repeated(&self, class_id: u32) -> bool {
        self.class_values
            .get(class_id as usize)
            .is_some_and(|slot| slot.repeated)
    }

    /// The evaluation of `class_id` that owns a brand on `chain`, innermost
    /// first.
    fn class_value_in_chain(&self, chain: Option<&Vec<u64>>, class_id: u32) -> Option<Value> {
        for &b in chain? {
            if let Some(&owner) = self.brand_owner.get(&b) {
                // The class_id check also guards against a brand whose owner
                // slot the GC has since recycled.
                if let HeapObj::Class(c) = self.heap.get(owner) {
                    if c.class_id == class_id {
                        return Some(Value::heap(owner));
                    }
                }
            }
        }
        None
    }

    /// Fast lookup of the running constructor's owning class evaluation for
    /// `home_class_id`: returns `class_values[home_class_id].latest` directly
    /// without touching `ctor_owner` / `method_brand` when the class was only
    /// evaluated once (`!slot.repeated`), otherwise recovers the exact
    /// evaluation from `derived_ctor_stack` or `running_class_value`.
    #[inline]
    pub(crate) fn home_class_value(&self, home_class_id: u32) -> Option<Value> {
        if let Some(slot) = self.class_values.get(home_class_id as usize) {
            if !slot.repeated {
                return slot.latest;
            }
        }
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if let Some(top) = self.active_derived_ctor() {
            if top.home_class.is_heap() {
                return Some(top.home_class);
            }
        }
        self.running_class_value()
            .or_else(|| self.class_value_for(home_class_id))
    }

    /// The fetch half of GetSuperConstructor() WITHOUT the IsConstructor check —
    /// the spec evaluates the SuperCall's argument list BETWEEN the two, so the
    /// compiler emits this (SuperCtorFetch) before the args and the check lands
    /// in the SuperCtor op that runs after them. UNDEFINED when unresolvable
    /// (the check then throws "superclass is not a constructor").
    pub(crate) fn super_ctor_fetch(&mut self, home_class_id: u32) -> Value {
        let Some(home) = self.home_class_value(home_class_id) else {
            return Value::UNDEFINED;
        };
        if home.is_heap() {
            let hi = home.heap_index();
            if let Some(p) = self.proto_of.get_copied(hi) {
                return p;
            }
            if let HeapObj::Class(c) = self.heap.get(hi) {
                return c.parent.map_or(Value::UNDEFINED, Value::heap);
            }
        }
        Value::UNDEFINED
    }

    /// The super BASE object for a `super.x` reference inside a method of class
    /// `home_class_id`: GetPrototypeOf(HomeObject), where HomeObject is the class's
    /// own `prototype`. For `class C extends B` this is `B.prototype`; for a BASE
    /// class it is `%Object.prototype%` (so `super.toString()` etc. resolve). Returns
    /// UNDEFINED if unresolvable. (Unlike `super_parent`, this does not require a
    /// parent class, so `super.x` works in base-class methods.)
    pub(crate) fn super_base(&mut self, home_class_id: u32, is_static: bool) -> Value {
        let home = match self.class_value_for(home_class_id) {
            Some(c) => c,
            None => return Value::UNDEFINED,
        };
        // A STATIC element's HomeObject is the class value itself, so its super base
        // is GetPrototypeOf(class) — the PARENT CLASS for `class C extends B` (or
        // %Function.prototype% for a base class) — letting `super.x` reach inherited
        // STATIC members. An instance element's HomeObject is the class prototype.
        if is_static {
            // LIVE GetPrototypeOf(class): Object.setPrototypeOf(C, X) after the
            // definition re-targets `super.x` in static members (a null proto
            // then throws via the caller's RequireObjectCoercible).
            return self.object_get_prototype_of(home);
        }
        let home_proto = match self.prototype_of(home) {
            Some(p) => p,
            None => return Value::UNDEFINED,
        };
        self.object_get_prototype_of(home_proto)
    }

    /// The super BASE for an OBJECT-method `super.x`: GetPrototypeOf([[HomeObject]]),
    /// where the home object is looked up by the executing closure (`callee`) in
    /// `closure_home`. `UNDEFINED` if there is no home (then the caller's
    /// RequireObjectCoercible throws — shouldn't happen for well-formed object super).
    pub(crate) fn obj_super_base(&mut self, callee: Value) -> Value {
        let home = if callee.is_heap() {
            self.closure_home
                .get(&callee.heap_index())
                .copied()
                .unwrap_or(Value::UNDEFINED)
        } else {
            Value::UNDEFINED
        };
        if home.is_heap() {
            self.object_get_prototype_of(home)
        } else {
            Value::UNDEFINED
        }
    }

    /// `super.key = v` with the base prototype already resolved (the class
    /// variant resolves it via `super_base`, the object-method variant via the
    /// [[HomeObject]]). An inherited setter on the chain is invoked with `this`
    /// = the receiver; otherwise the value is set on the receiver, with the
    /// REFERENCE SITE's strictness (a sloppy method's rejected write no-ops).
    pub(crate) fn super_set_obj(
        &mut self,
        proto: Value,
        key: &str,
        this: Value,
        v: Value,
        strict: bool,
    ) -> Result<(), Thrown> {
        // MakeSuperPropertyReference: RequireObjectCoercible(GetSuperBase()).
        self.require_object_coercible(proto)?;
        // OrdinarySetWithOwnDescriptor consults Receiver.[[GetOwnProperty]]
        // FIRST: a deferred-namespace receiver triggers evaluation, and a
        // namespace receiver's uninit export throws ReferenceError.
        self.defer_check(this, key)?;
        self.ns_tdz_check(this, key)?;
        // `super.x = v` IS `GetSuperBase().[[Set]](x, v, Receiver = this)`, so it
        // must run the full OrdinarySetWithOwnDescriptor. Approximating it with
        // `Set(this, …)` after a chain setter probe got three cases wrong, all of
        // them silently (staging/sm/class/superProp{NoOverwriting,StrictAssign}):
        //   * an own ACCESSOR on the receiver rejects the write — it must NOT run
        //     the receiver's own setter (`super.p = 1` where `this.p` is an
        //     accessor is a TypeError, not a setter call);
        //   * an own non-writable data prop on the receiver rejects it;
        //   * a NON-WRITABLE data property found on the SUPER chain rejects it
        //     (the old code only looked for a setter, so it fell through and
        //     created an own prop on the receiver instead).
        // A rejected [[Set]] is a TypeError at a strict reference site and a
        // silent no-op at a sloppy one (an object-literal method in sloppy code).
        let kv = self.key_to_value(key);
        if !self.ordinary_set_with_receiver(proto, kv, v, this)? && strict {
            return Err(Thrown::join3("TypeError: Cannot assign to read only property '", &*key, "' of object"));
        }
        Ok(())
    }

    /// Run a class's constructor contribution on an existing instance `obj` —
    /// for `super(...)` and the implicit-super chain. An explicit ctor runs its
    /// own `super`; an implicit one runs the parent chain then its fields.
    /// Completion of a `super(...)` call (the SuperCtor/SuperCtorSpread ops),
    /// AFTER the parent ctor ran (spec evaluates the SuperCall fully; only
    /// BindThisValue then throws on re-initialization): enforce the
    /// once-per-activation rule, rebind reg 0 to the produced `this`
    /// (return-override), lift the this-TDZ, mark super-called, and run the
    /// home class's deferred instance-field initializers on the result.
    pub(crate) fn super_ctor_complete(
        &mut self,
        base: usize,
        this: Value,
        produced: Value,
        home_class_id: u32,
    ) -> Result<(), Thrown> {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if let Some(top) = self.active_derived_ctor_mut() {
            if top.super_done {
                return Err(Thrown::from_static(
                    "ReferenceError: super constructor may only be called once",
                ));
            }
            top.super_done = true;
            let home_cls = top.home_class;
            if produced.is_heap() && produced != this {
                top.super_this_override = produced;
            }
            if produced.is_heap() && produced != this {
                self.set(base, 0, produced);
            }
            if this.is_heap() && !self.this_tdz.is_empty() {
                self.this_tdz.remove(&this.heap_index());
            }
            let cv = if home_cls.is_heap() {
                Some(home_cls)
            } else {
                self.home_class_value(home_class_id)
            };
            if let Some(cv) = cv {
                let inst = if produced.is_heap() { produced } else { this };
                let is_override = produced.is_heap() && produced != this;
                self.private_init_checked(inst, cv, is_override)?;
                self.run_field_thunk(inst, cv)?;
            }
            return Ok(());
        }
        let in_arrow = self
            .frames
            .last()
            .map(|f| self.func(f.func as usize).lexical_this)
            .unwrap_or(false);
        let already = if in_arrow {
            // An arrow frame has no super state of its own: the lexical ctor
            // activation already initialized `this` iff it left the TDZ.
            this.is_heap() && !self.this_tdz.contains(&this.heap_index())
        } else {
            self.frames.last().map(|f| f.super_done).unwrap_or(false)
        };
        if already {
            return Err(Thrown::from_static(
                "ReferenceError: super constructor may only be called once",
            ));
        }
        if produced.is_heap() {
            self.set(base, 0, produced);
            // A parent RETURN-OVERRIDE must become the construction result even
            // when the derived ctor later returns `undefined` (reg 0 alone
            // doesn't reach construct()) — bank it keyed by the original this.
            if produced != this && this.is_heap() {
                self.super_this.insert(this.heap_index(), produced);
            }
        }
        if let Some(f) = self.frames.last_mut() {
            f.super_done = true;
        }
        if this.is_heap() {
            self.this_tdz.remove(&this.heap_index());
            self.super_called.insert(this.heap_index());
        }
        // InitializeInstanceElements: this class's field initializers run NOW
        // (not at ctor entry), on the produced instance, with no new.target.
        // Same evaluation the `super()` above resolved against, so a chain built
        // by re-evaluating one class expression initializes each level's OWN
        // fields rather than the newest evaluation's.
        if let Some(cv) = self.home_class_value(home_class_id) {
            // Checked PrivateBrandAdd: a return-override that already carries
            // this class's private elements, or a non-extensible instance,
            // throws TypeError BEFORE the field initializers run.
            let inst = if produced.is_heap() { produced } else { this };
            let is_override = produced.is_heap() && produced != this;
            self.private_init_checked(inst, cv, is_override)?;
            self.run_field_thunk(inst, cv)?;
        }
        Ok(())
    }

    /// The instance-field half of InitializeInstanceElements for a class whose
    /// ctor is EXPLICIT: its initializers live in a separate thunk (see
    /// `defer_fields` in compile/funcs.rs) so they run at the spec's point —
    /// after super() for a derived class, before the ctor body for a base one —
    /// and in the class scope rather than the ctor's parameter scope. No-op for a
    /// class that declares no instance fields. `new.target` is deliberately left
    /// alone: it is undefined inside a field initializer.
    #[inline]
    pub(crate) fn run_field_thunk(&mut self, inst: Value, cv: Value) -> Result<(), Thrown> {
        if !cv.is_heap() {
            return Ok(());
        }
        // The thunk runs in the class body's private scope (same brand chain
        // handed to the ctor in `construct`).
        let Some(f) = self.class_body_fn(cv, true) else { return Ok(()) };
        self.call_value(f, inst, &[])?;
        Ok(())
    }

    pub(crate) fn run_class_ctor(
        &mut self,
        cval: Value,
        obj: Value,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, Thrown> {
        if !cval.is_heap() {
            return Ok(obj);
        }
        let (mut ctor, mut has_explicit, mut parent, extends_null) =
            match self.heap.get(cval.heap_index()) {
                HeapObj::Class(c) => (
                    c.ctor,
                    c.has_explicit_ctor,
                    c.parent,
                    c.extends_null,
                ),
                // `super(...)` to a BUILT-IN parent (`class X extends Error`). We model
                // the Error family: set `message` on the instance from the argument
                // (AggregateError takes it as the 2nd arg). The instance's prototype
                // chain already reaches the error prototype (so name/toString/
                // instanceof resolve), so nothing else is needed here.
                _ => {
                    // `%TypedArray%` is abstract: its first step throws, from a
                    // subclass's super() as from `new`, before newTarget's
                    // prototype is read (ES2025 23.2.1.1).
                    if cval.heap_index() == self.ta_base_ctor && self.ta_base_ctor != 0 {
                        return Err(Thrown::from_static(
                            "TypeError: Abstract class TypedArray not directly constructable",
                        ));
                    }
                    // A non-class parent is the base that creates the object: a
                    // Proxy or a bound function through its own [[Construct]]
                    // (which reads, or not, newTarget.prototype itself),
                    // anything else as its OrdinaryCreateFromConstructor, which
                    // reads it first.
                    let constructs_itself = self.proxy_parts(cval.heap_index()).is_some()
                        || matches!(self.heap.get(cval.heap_index()), HeapObj::Bound { .. });
                    if constructs_itself {
                        self.pending_ctor_proto.remove(&obj.heap_index());
                    } else {
                        self.settle_pending_ctor_proto(obj, cval)?;
                    }
                    // `super(...)` to a BUILT-IN EXOTIC parent (`class X extends Set/…`):
                    // brand the plain-Object instance with the builtin's internal
                    // representation so its methods work and it is a real instanceof. The
                    // instance keeps its own (subclass) prototype, recorded in proto_of.
                    if self.brand_builtin_subclass(cval, obj, args)? {
                        return Ok(obj);
                    }
                    // `class X extends SuppressedError`: super(error, suppressed, msg)
                    // installs the three own props on the instance and brands the
                    // [[ErrorData]] slot (so Error.isError is true).
                    if cval.heap_index() == self.suppressederror_ctor
                        && self.suppressederror_ctor != 0
                    {
                        let error = args.first().copied().unwrap_or(Value::UNDEFINED);
                        let suppressed = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                        let message = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                        let msg_val = if message != Value::UNDEFINED {
                            Some(self.to_str_value(message)?)
                        } else {
                            None
                        };
                        let attr = PropAttr {
                            writable: true,
                            enumerable: false,
                            configurable: true,
                            accessor: false,
                            setter: Value::UNDEFINED,
                        };
                        if let HeapObj::Object(map) = self.heap.get_mut(obj.heap_index()) {
                            if let Some(mv) = msg_val {
                                map.define("message", mv, attr);
                            }
                            map.define("error", error, attr);
                            map.define("suppressed", suppressed, attr);
                        }
                        self.mark_constructed_error_data(obj.heap_index(), new_target);
                        return Ok(obj);
                    }
                    if let Some(k) = self
                        .error_ctors
                        .iter()
                        .position(|&c| c == cval.heap_index())
                    {
                        // `class X extends Error` instance: it has the [[ErrorData]] slot.
                        self.mark_constructed_error_data(obj.heap_index(), new_target);
                        let msg = if k == 7 {
                            args.get(1).copied()
                        } else {
                            args.first().copied()
                        };
                        if let Some(m) = msg.filter(|m| *m != Value::UNDEFINED) {
                            let message_value = self.to_str_value(m)?;
                            self.heap.write_barrier_val(obj.heap_index(), message_value);
                            if let HeapObj::Object(map) = self.heap.get_mut(obj.heap_index()) {
                                // `message` is a non-enumerable own data property.
                                map.define(
                                    "message",
                                    message_value,
                                    PropAttr {
                                        writable: true,
                                        enumerable: false,
                                        configurable: true,
                                        accessor: false,
                                        setter: Value::UNDEFINED,
                                    },
                                );
                            }
                        }
                        // The Error constructors' remaining steps apply to a
                        // subclass instance too: InstallErrorCause, then (for
                        // AggregateError) the `errors` list.
                        let options = args
                            .get(if k == 7 { 2 } else { 1 })
                            .copied()
                            .unwrap_or(Value::UNDEFINED);
                        self.install_error_cause(obj, options)?;
                        if k == 7 {
                            let errors_arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                            self.install_agg_errors(obj, errors_arg)?;
                        }
                    }
                    // `class X extends someProxy`: SuperCall is
                    // `Construct(parent, args, newTarget)`, so the PROXY's
                    // [[Construct]] runs — its `construct` trap, or (trap-less) the
                    // forward to its target. The result IS the instance: with
                    // newTarget still the derived class, OrdinaryCreateFromConstructor
                    // gives it the derived prototype, and `super_ctor_complete`
                    // rebinds `this` to it. Without this arm the parent constructor
                    // never ran at all and `this` kept the pre-made empty object
                    // (staging/sm/class/superCall{BaseInvoked,ProperBase}.js).
                    //
                    // A bound function likewise: its [[Construct]] constructs
                    // its target with the bound arguments in front and this
                    // newTarget, and ignores the bound `this`. It was CALLED
                    // here, with the instance as a `this` a bound function
                    // does not take: a bound class threw "is not a function",
                    // and a bound function ran on its bound `this`, leaving
                    // the instance untouched.
                    if constructs_itself {
                        if !self.is_constructor(cval) {
                            return Err(Thrown::from_static(
                                "TypeError: the superclass is not a constructor",
                            ));
                        }
                        return self.construct_with_newtarget(cval, args, new_target);
                    }
                    // `class X extends f` where f is a PLAIN user function: super(...)
                    // must actually INVOKE the parent with this = the instance and the
                    // subclass new.target (a non-constructor parent is a TypeError; an
                    // object return is the return-override instance).
                    if matches!(
                        self.heap.get(cval.heap_index()),
                        HeapObj::Func(_) | HeapObj::Closure { .. }
                    ) {
                        if !self.is_constructor(cval) {
                            return Err(Thrown::from_static(
                                "TypeError: the superclass is not a constructor",
                            ));
                        }
                        self.pending_new_target = new_target;
                        let r = self.call_value(cval, obj, args)?;
                        if self.is_object_value(r) {
                            return Ok(r);
                        }
                    }
                    return Ok(obj);
                }
            };
        let mut cval = cval;
        // Skip plain ctor-less derived class levels when no deferred newTarget
        // prototype read is pending, avoiding recursive `run_class_ctor` hops.
        if !has_explicit && parent.is_some() && !extends_null && self.pending_ctor_proto.is_empty() {
            if let Some(target) = self.resolve_ctor_chain_target(cval.heap_index()) {
                match target {
                    crate::vm::engine::CtorChainTarget::EmptyBase => return Ok(obj),
                    crate::vm::engine::CtorChainTarget::ExplicitBase { class_val, ctor: c } => {
                        cval = class_val;
                        ctor = Some(c);
                        has_explicit = true;
                        parent = None;
                    }
                    crate::vm::engine::CtorChainTarget::ExplicitDerived { class_val, ctor: c } => {
                        cval = class_val;
                        ctor = Some(c);
                        has_explicit = true;
                    }
                }
            }
        }
        // A base class creates the instance: a deferred newTarget prototype
        // is read now, before its fields and constructor run.
        if parent.is_none() && !extends_null && !self.pending_ctor_proto.is_empty() {
            self.settle_pending_ctor_proto(obj, cval)?;
        }
        if has_explicit {
            // An explicit ctor produces `this`: its object-return (return-override)
            // becomes the effective instance; a non-object/undefined return keeps obj.
            if ctor.is_some() {
                // A BASE parent's InitializeInstanceElements runs at entry —
                // brand, then fields, both before the parent ctor's parameter
                // prologue.
                if parent.is_none() && !extends_null {
                    self.brand_instance(obj, cval);
                    self.run_field_thunk(obj, cval)?;
                }
                // The ctor function value is materialized AFTER the field thunk
                // (as construct_with_newtarget does for `new Base()`): the field
                // initializers are guest code, and a function Value held only in
                // a Rust local across them was swept, so super() called whatever
                // object reused its slot ("Invalid Date is not a function").
                // (It is kept on the class now, rooted by it.)
                let f = self.class_body_fn(cval, false).unwrap_or(Value::UNDEFINED);
                // A derived parent ctor begins with `this` back in TDZ (until
                // ITS OWN super() completes). No removal here: if the parent
                // throws pre-super, the caller's binding is still uninitialized
                // (a catching outer ctor may legitimately retry super()); the
                // outermost construct() clears the mark on every exit.
                if parent.is_some() || extends_null {
                    self.this_tdz.insert(obj.heap_index());
                }
                // `new.target` propagates unchanged through super() to the parent ctor.
                self.pending_new_target = new_target;
                let r = self.call_value(f, obj, args)?;
                // An undefined return yields super()'s produced this (a parent
                // return-override banked by super_ctor_complete), else obj. The
                // banked mapping is READ, not removed: it doubles as the escaped-
                // arrow `this` resolution (ThisCheck) for THIS activation, so it
                // must survive a repeated super() attempt (which runs the parent
                // ctor again before BindThisValue throws) — the GC prunes it once
                // the placeholder is unreachable.
                let result = if self.is_object_value(r) {
                    r
                } else if !self.super_this.is_empty() {
                    self.super_this.get(&obj.heap_index()).copied().unwrap_or(obj)
                } else {
                    obj
                };
                // No brand of this class for a return-override result: its
                // InitializeInstanceElements ran on `this` (a base class's at
                // entry above, a derived one's on what its own `super()`
                // produced, `super_ctor_complete`, the `super_this` result
                // included), and an object its body returns is handed back
                // as it stands ([[Construct]] step 10.a).
                return Ok(result);
            }
            Ok(obj)
        } else {
            // Implicit ctor: run the parent chain (threading its produced `this`),
            // then this class's field initializers on it.
            //
            // A default derived constructor is `constructor(...args){ super(...args) }`,
            // and its `super` resolves through GetSuperConstructor() — the class
            // object's LIVE [[GetPrototypeOf]]. Using the `parent` recorded at
            // class-definition time made `Object.setPrototypeOf(D, Other)` a no-op
            // for a class with no explicit ctor, while the explicit-ctor bytecodes
            // already retargeted
            // (staging/sm/class/superCallProperBase.js).
            let mut eff = obj;
            if parent.is_some() {
                let live = self.object_get_prototype_of(cval);
                let target = if live.is_heap() {
                    live
                } else {
                    Value::heap(parent.unwrap())
                };
                eff = self.run_class_ctor(target, eff, args, new_target)?;
            }
            // PrivateBrandAdd before this class's field initializers run.
            self.brand_instance(eff, cval);
            if ctor.is_some() {
                let f = self.class_body_fn(cval, false).unwrap_or(Value::UNDEFINED);
                self.pending_new_target = new_target;
                let r = self.call_value(f, eff, &[])?;
                if self.is_object_value(r) {
                    eff = r;
                }
            }
            Ok(eff)
        }
    }
}
