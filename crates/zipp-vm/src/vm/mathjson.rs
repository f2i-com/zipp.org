#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// `ZIPP_NO_JSON_LEAF_FAST=1` restores the old JSON.stringify leaf emission:
/// the `into_owned` copy per string leaf, the fresh `String` per number
/// (`fmt_f64`), and the cloned-key + `pos()` re-lookup object walk. Kept so
/// each change is A/B-able and bisectable on one binary.
#[inline]
fn json_leaf_fast_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_JSON_LEAF_FAST").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Default-on compact `JSON.stringify(value)` fast path for a graph made only
/// of plain data objects, dense Arrays and JSON primitive leaves. The entire
/// output is private until the walk succeeds, so an exotic node can decline
/// after an arbitrary prefix without exposing work; the ordinary serializer
/// then restarts and observes getters, proxies, `toJSON`, holes and errors.
/// `ZIPP_NO_JSON_PLAIN_FAST=1` restores the general recursive serializer.
#[inline]
fn json_plain_fast_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_JSON_PLAIN_FAST").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

impl<'p> Vm<'p> {
    /// Serialize the closed plain-data subset without paying the general
    /// serializer's per-node `toJSON` lookup, key snapshot, version probe and
    /// generic Array length/index dispatch. `None` is a side-effect-free
    /// decline, never the JavaScript `undefined` result.
    pub(crate) fn json_plain_stringify(&self, root: Value) -> Option<String> {
        if !json_plain_fast_enabled() || self.current_realm_id().is_some() {
            return None;
        }

        // Plain objects/arrays/strings still perform a live `toJSON` lookup in
        // the general serializer. Only accept the default main-realm chains
        // while none of their links can answer that lookup. An explicit proto
        // entry means user code changed the chain, even if today's end happens
        // not to contain `toJSON`, so leave it to the observable generic walk.
        if self.obj_proto == 0
            || self.arr_proto == 0
            || self.str_proto == 0
            || self.proto_of.contains_key(&self.obj_proto)
            || self.proto_of.contains_key(&self.arr_proto)
            || self.proto_of.contains_key(&self.str_proto)
        {
            return None;
        }
        let no_own_tojson = |vm: &Self, idx: u32| match vm.heap.get(idx) {
            HeapObj::Object(map) => map.pos("toJSON").is_none(),
            _ => vm
                .arr_props
                .get(&idx)
                .is_none_or(|map| map.pos("toJSON").is_none()),
        };
        if !no_own_tojson(self, self.obj_proto)
            || !no_own_tojson(self, self.arr_proto)
            || !no_own_tojson(self, self.str_proto)
        {
            return None;
        }

        let mut out = String::with_capacity(1024);
        let mut active = Vec::with_capacity(16);
        self.json_plain_value_into(root, 0, &mut active, &mut out)
            .then_some(out)
    }

    /// Recursive emitter for [`Self::json_plain_stringify`]. Depth is capped
    /// below the ordinary engine stack limit so the iterative-looking fast
    /// path cannot hide the generic serializer's stack overflow behaviour.
    fn json_plain_value_into(
        &self,
        value: Value,
        depth: usize,
        active: &mut Vec<u32>,
        out: &mut String,
    ) -> bool {
        const MAX_DEPTH: usize = 256;
        if value.is_null() {
            out.push_str("null");
            return true;
        }
        if value.is_bool() {
            out.push_str(if value.as_bool() { "true" } else { "false" });
            return true;
        }
        if value.is_number() {
            let n = value.as_f64();
            if n.is_finite() {
                fmt_f64_into(out, n);
            } else {
                out.push_str("null");
            }
            return true;
        }
        if !value.is_heap() {
            return false;
        }
        let idx = value.heap_index();
        match self.heap.get(idx) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                let Some(bytes) = self.heap.str_wtf8_cow(idx) else {
                    return false;
                };
                json_quote_wtf8_into(out, &bytes);
                true
            }
            HeapObj::Array(items) => {
                if depth >= MAX_DEPTH
                    || active.contains(&idx)
                    || self.proto_of.contains_key(&idx)
                    || self.arguments_objs.contains_key(&idx)
                    || self.arr_props.contains_key(&idx)
                    || self.array_js_len.contains_key(&idx)
                    || items.iter().any(|item| item.is_hole())
                {
                    return false;
                }
                active.push(idx);
                out.push('[');
                for (i, &item) in items.iter().enumerate() {
                    if i != 0 {
                        out.push(',');
                    }
                    if !self.json_plain_value_into(item, depth + 1, active, out) {
                        active.pop();
                        return false;
                    }
                }
                out.push(']');
                active.pop();
                true
            }
            HeapObj::Object(map) => {
                if depth >= MAX_DEPTH
                    || active.contains(&idx)
                    || idx == self.global_this
                    // `%Array.prototype%` is internally an Object map, but
                    // IsArray is true and the ordinary serializer emits it as
                    // a length-zero Array (`[]`), ignoring named properties.
                    || idx == self.arr_proto
                    || self.proto_of.contains_key(&idx)
                    || map.class.is_some()
                    || map.is_ctor
                    || map.is_raw_json
                    || map.pos("toJSON").is_some()
                    || self.module_namespaces.contains_key(&idx)
                    || self.deferred_ns_state.contains_key(&idx)
                {
                    return false;
                }
                active.push(idx);
                out.push('{');
                let mut any = false;
                for i in 0..map.keys.len() {
                    let key = &map.keys[i];
                    if is_hidden_key(key) || !map.attrs[i].enumerable {
                        continue;
                    }
                    // Integer keys need spec reordering; accessors can run user
                    // code. Both are clean declines before any visible result.
                    if map.attrs[i].accessor || canonical_index_str(key).is_some() {
                        active.pop();
                        return false;
                    }
                    if any {
                        out.push(',');
                    }
                    json_quote_into(out, key);
                    out.push(':');
                    if !self.json_plain_value_into(map.vals[i], depth + 1, active, out) {
                        active.pop();
                        return false;
                    }
                    any = true;
                }
                out.push('}');
                active.pop();
                true
            }
            _ => false,
        }
    }

    /// Guarded whole-tree execution for the exact Tier-C [`JsonWalkPlan`].
    ///
    /// The bytecode shape has no observable work except numeric global updates
    /// and reads from the visited tree.  We therefore accumulate privately,
    /// validate the *entire* graph, and commit only after traversal succeeds.
    /// Any getter/proxy/custom prototype/sparse element/cycle/unsupported leaf
    /// returns `None` with zero visible effects, so the ordinary native body can
    /// execute from instruction 0. Aliases are intentionally revisited: that is
    /// what the recursive JavaScript function does.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn json_walk_reduce(
        &mut self,
        plan: crate::codegen::JsonWalkPlan,
        callee: Value,
        root: Value,
    ) -> Option<u64> {
        // The three typeof comparison constants are heap-resolved after the JIT
        // plan was built. Validate their live contents here rather than trusting
        // source text or constant-pool positions.
        let nc = Value::from_bits(plan.number_bits);
        let sc = Value::from_bits(plan.string_bits);
        let bc = Value::from_bits(plan.boolean_bits);
        let const_is = |vm: &Self, v: Value, want: &str| {
            v.is_heap()
                && vm
                    .heap
                    .str_cow(v.heap_index())
                    .is_some_and(|s| s.as_ref() == want)
        };
        if !const_is(self, nc, "number")
            || !const_is(self, sc, "string")
            || !const_is(self, bc, "boolean")
            || self.globals.get(plan.self_global as usize).copied()? != callee
        {
            return None;
        }

        let slots = [
            plan.nodes,
            plan.nulls,
            plan.sum2x,
            plan.strings,
            plan.string_len,
            plan.bools,
        ];
        let mut acc = [0.0; 6];
        for (i, &g) in slots.iter().enumerate() {
            let v = *self.globals.get(g as usize)?;
            if !v.is_number() {
                return None; // would run ToNumeric / `+` coercion in JS
            }
            acc[i] = v.as_f64();
        }

        // Every ordinary object in the admitted graph inherits from the default
        // Object prototype. `for-in` would also visit inherited enumerable keys;
        // admit the direct own-slot walk only while that whole default tail is
        // the usual terminal, barren object.
        let object_proto_barren = self.obj_proto != 0
            && !self.proto_of.contains_key(&self.obj_proto)
            && matches!(self.heap.get(self.obj_proto), HeapObj::Object(m)
                if m.keys.iter().enumerate().all(|(i, k)|
                    is_hidden_key(k) || !m.attrs[i].enumerable));

        // `(value, exit, depth)` implements DFS without Rust recursion. Exit markers
        // keep only the current ancestry in `active`, detecting cycles while
        // allowing shared subtrees to be counted once per incoming edge. Very
        // deep inputs deliberately decline: completing them without JS frames
        // could otherwise hide the recursive body's observable RangeError.
        const MAX_REDUCED_DEPTH: usize = 256;
        // The ordinary recursive body consumes one VM frame per tree level.
        // Leave enough headroom for the deepest tree this reducer admits; when
        // a caller has already filled the stack, completing iteratively here
        // would incorrectly suppress the ordinary path's RangeError.
        if self.frames.len().saturating_add(MAX_REDUCED_DEPTH) >= MAX_FRAMES {
            return None;
        }
        let mut work: Vec<(Value, bool, usize)> = vec![(root, false, 0)];
        let mut active = rustc_hash::FxHashSet::<u32>::default();
        while let Some((v, exit, depth)) = work.pop() {
            if exit {
                active.remove(&v.heap_index());
                continue;
            }
            if depth >= MAX_REDUCED_DEPTH {
                return None;
            }
            // `nodes++` precedes every branch in the JavaScript body.
            acc[0] += 1.0;
            if v.is_null() {
                acc[1] += 1.0;
                continue;
            }
            if v.is_number() {
                // Keep the exact left-to-right f64 operation grouping:
                // `numSum2x = numSum2x + (v * 2)`.
                acc[2] += v.as_f64() * 2.0;
                continue;
            }
            if v.is_bool() {
                acc[5] += 1.0;
                continue;
            }
            if !v.is_heap() {
                return None;
            }
            let idx = v.heap_index();
            if let Some(n) = self.heap.str_units(idx) {
                acc[3] += 1.0;
                acc[4] += n as f64;
                continue;
            }
            if !active.insert(idx) {
                return None;
            }
            work.push((v, true, depth));
            match self.heap.get(idx) {
                HeapObj::Array(items) => {
                    // The source loop reads the live `length` then every index.
                    // Dense, non-overlaid, ordinary Arrays make those reads pure
                    // and identical to the backing vector. Named own properties
                    // are irrelevant because the array branch never for-ins.
                    if self.proto_of.contains_key(&idx)
                        || self.arguments_objs.contains_key(&idx)
                        || self.array_elements_overlaid(idx)
                        || self.array_js_len.contains_key(&idx)
                        || items.iter().any(|v| v.is_hole())
                    {
                        return None;
                    }
                    for &child in items.iter().rev() {
                        work.push((child, false, depth + 1));
                    }
                }
                HeapObj::Object(map) => {
                    if !object_proto_barren
                        || idx == self.global_this
                        || self.proto_of.contains_key(&idx)
                        || map.class.is_some()
                        || map.is_raw_json
                        || self.module_namespaces.contains_key(&idx)
                        || self.deferred_ns_state.contains_key(&idx)
                    {
                        return None;
                    }
                    // With no integer-index key, insertion order is exactly
                    // for-in's own-key order. Rejecting that rare shape avoids a
                    // per-object sort/allocation and preserves floating-add order.
                    for i in (0..map.keys.len()).rev() {
                        let key = &map.keys[i];
                        if is_hidden_key(key) || !map.attrs[i].enumerable {
                            continue;
                        }
                        if map.attrs[i].accessor || canonical_index_str(key).is_some() {
                            return None;
                        }
                        work.push((map.vals[i], false, depth + 1));
                    }
                }
                _ => return None,
            }
        }

        for (i, &g) in slots.iter().enumerate() {
            // This replaces the Tier-C body's ordinary bytecode store. Do not
            // bump `global_gens`: generated StoreGlobalResolved does not either.
            self.globals[g as usize] = Value::num(acc[i]);
        }
        Some(Value::UNDEFINED.bits())
    }

    /// Evaluate a `Math.<fn>` call over `argc` argument registers (coerced to
    /// numbers). Mirrors JS semantics where they differ from Rust's f64 methods:
    /// `round` is half-up (so −2.5 → −2, not −3); `sign` preserves ±0 and maps
    /// NaN→NaN; `min`/`max` are NaN-sticky (any NaN arg ⇒ NaN).
    pub(crate) fn eval_math(
        &mut self,
        op: crate::bytecode::MathFn,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<f64, Thrown> {
        // Snapshot the argument registers FIRST (a ToNumber coercion below may run a
        // user valueOf that re-enters the VM and pushes registers), then delegate to
        // the shared value-form evaluator, which ToNumber-coerces each argument.
        //
        // The snapshot used to be a `Vec`, allocated and freed on EVERY fused
        // `Math.*` -- and four of the thirteen benchmark rows call `Math.imul`
        // once per element in their mixing functions.
        self.with_argv(base, arg_base, argc, |vm, args| vm.eval_math_args(op, args))
    }

    /// `Math.<op>` reduced to a single f64 result (used by the `MathSpread`
    /// fallback for an unusual non-variadic spread like `Math.abs(...arr)`).
    pub(crate) fn eval_math_one(&self, op: crate::bytecode::MathFn, x: f64) -> f64 {
        math_unary(op, x)
    }

    /// Evaluate a Math method over an argument SLICE (the value-form `Math.abs`
    /// invoked as a native), mirroring `eval_math`'s register-based variant.
    pub(crate) fn eval_math_args(
        &mut self,
        op: crate::bytecode::MathFn,
        args: &[Value],
    ) -> Result<f64, Thrown> {
        use crate::bytecode::MathFn as M;
        let at = |args: &[Value], i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        Ok(match op {
            M::Min | M::Max | M::Hypot => {
                // ToNumber EVERY argument (observable valueOf/toString, left-to-right)
                // before reducing.
                let mut nums = Vec::with_capacity(args.len());
                for &v in args {
                    nums.push(self.to_number_coerce(v)?);
                }
                let mut acc = match op {
                    M::Min => f64::INFINITY,
                    M::Max => f64::NEG_INFINITY,
                    _ => 0.0,
                };
                let mut hypot_inf = false;
                for v in nums {
                    acc = match op {
                        // f64 min/max treat -0 and +0 as equal; spec orders -0 < +0,
                        // so tie-break on the sign (Min prefers -0, Max prefers +0).
                        M::Min => {
                            if v.is_nan() || acc.is_nan() {
                                f64::NAN
                            } else if v == acc {
                                if v.is_sign_negative() {
                                    v
                                } else {
                                    acc
                                }
                            } else {
                                acc.min(v)
                            }
                        }
                        M::Max => {
                            if v.is_nan() || acc.is_nan() {
                                f64::NAN
                            } else if v == acc {
                                if v.is_sign_positive() {
                                    v
                                } else {
                                    acc
                                }
                            } else {
                                acc.max(v)
                            }
                        }
                        _ => {
                            // Math.hypot: a ±Infinity argument forces +Infinity even
                            // when another argument is NaN (spec step 3).
                            if v.is_infinite() {
                                hypot_inf = true;
                            }
                            acc + v * v
                        }
                    };
                }
                if matches!(op, M::Hypot) {
                    if hypot_inf {
                        f64::INFINITY
                    } else {
                        acc.sqrt()
                    }
                } else {
                    acc
                }
            }
            // The two-arg ops coerce arg0 then arg1 (ToNumber, left-to-right).
            M::Pow => {
                let a = self.to_number_coerce(at(args, 0))?;
                let b = self.to_number_coerce(at(args, 1))?;
                // Spec: base of magnitude 1 with a NaN/±Infinity exponent is NaN
                // (C/Rust powf returns 1 for these — a deliberate deviation).
                if (a == 1.0 || a == -1.0) && (b.is_nan() || b.is_infinite()) {
                    f64::NAN
                } else {
                    a.powf(b)
                }
            }
            M::Atan2 => {
                let a = self.to_number_coerce(at(args, 0))?;
                let b = self.to_number_coerce(at(args, 1))?;
                a.atan2(b)
            }
            M::Imul => {
                let a = self.to_number_coerce(at(args, 0))?;
                let b = self.to_number_coerce(at(args, 1))?;
                (to_uint32(a).wrapping_mul(to_uint32(b)) as i32) as f64
            }
            _ => {
                let x = self.to_number_coerce(at(args, 0))?;
                math_unary(op, x)
            }
        })
    }

    /// The per-level indent string for `JSON.stringify`'s `space` argument: a
    /// number → that many spaces (clamped 0..10); a string → its first 10 chars;
    /// anything else → empty (compact output).
    /// JSON.stringify `space` coercion (spec sec-json.stringify step 5): a Number
    /// wrapper object is read as ToNumber(space) and a String wrapper as
    /// ToString(space) — both honouring an overridden `valueOf`/`toString` (so
    /// `new Number(1)` with `valueOf:()=>3` indents by 3, and a throwing `valueOf`
    /// propagates). Everything else passes through unchanged to `json_indent`.
    pub(crate) fn json_coerce_space(&mut self, space: Value) -> Result<Value, Thrown> {
        if !space.is_heap() {
            return Ok(space);
        }
        match self.heap.get(space.heap_index()) {
            HeapObj::Boxed { kind: 1, .. } => {
                // ToPrimitive(space, number) honouring overrides, then ToNumber.
                let prim = if let Some(p) = self.symbol_to_primitive(space, "number")? {
                    p
                } else {
                    let mut found = None;
                    for name in ["valueOf", "toString"] {
                        let f = self.get_prop(space, name)?;
                        if self.is_callable(f) {
                            let r = self.call_value(f, space, &[])?;
                            if !self.is_object_value(r) {
                                found = Some(r);
                                break;
                            }
                        }
                    }
                    found.ok_or_else(|| {
                        Thrown("TypeError: Cannot convert object to primitive value".into())
                    })?
                };
                Ok(Value::num(self.to_number(prim)?))
            }
            HeapObj::Boxed { kind: 0, .. } => {
                let s = self.to_js_string(space)?;
                Ok(self.alloc_str(s))
            }
            _ => Ok(space),
        }
    }

    pub(crate) fn json_indent(&self, space: Value) -> String {
        if space.is_number() {
            let n = space.as_f64();
            let n = if n.is_finite() && n > 0.0 {
                (n as usize).min(10)
            } else {
                0
            };
            " ".repeat(n)
        } else if space.is_heap() {
            match self.heap.str_cow(space.heap_index()) {
                Some(s) => s.chars().take(10).collect(),
                None => String::new(),
            }
        } else {
            String::new()
        }
    }

    /// Resolve `JSON.stringify`'s second argument into either a function
    /// replacer or a property allowlist. A callable is the function form; an
    /// Array is a PropertyList — its String / Number (and boxed String/Number)
    /// entries become allowed keys, ToString-coerced and deduplicated in order.
    pub(crate) fn json_resolve_replacer(
        &mut self,
        replacer: Value,
    ) -> Result<(Value, Option<Vec<String>>), Thrown> {
        if self.is_callable(replacer) {
            return Ok((replacer, None));
        }
        // IsArray on a revoked Proxy is a TypeError (value_is_array approximates it
        // as false, so check explicitly before the PropertyList branch).
        if replacer.is_heap() {
            if let HeapObj::Proxy { revoked: true, .. } = self.heap.get(replacer.heap_index()) {
                return Err(Thrown(
                    "TypeError: Cannot perform IsArray on a revoked Proxy".into(),
                ));
            }
        }
        // An array (or Proxy-wrapping-array) replacer is a PropertyList: read its
        // length + each element via REAL [[Get]] (a revoked/throwing proxy throws),
        // keeping only string / number / (String|Number)-object items, deduped, in
        // order. A non-array object replacer is ignored (no filter).
        if replacer.is_heap() && self.value_is_array(replacer) {
            let lenv = self.get_prop(replacer, "length")?;
            let lenf = self.to_number_coerce(lenv)?;
            let len: u64 = if lenf.is_nan() || lenf <= 0.0 {
                0
            } else {
                lenf.min(9007199254740991.0) as u64
            };
            let mut list: Vec<String> = Vec::new();
            let mut k: u64 = 0;
            while k < len {
                let it = self.get_index(replacer, Value::num(k as f64))?;
                let item = if it.is_number() {
                    Some(self.to_js_string(it)?)
                } else if it.is_heap() {
                    match self.heap.get(it.heap_index()) {
                        HeapObj::Str(_) | HeapObj::Cons { .. } => {
                            self.heap.str_cow(it.heap_index()).map(|s| s.into_owned())
                        }
                        HeapObj::Boxed { kind, .. } if *kind == 0 || *kind == 1 => {
                            Some(self.to_js_string(it)?)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(s) = item {
                    if !list.contains(&s) {
                        list.push(s);
                    }
                }
                k += 1;
            }
            return Ok((Value::UNDEFINED, Some(list)));
        }
        Ok((Value::UNDEFINED, None))
    }

    /// Serialize `v` to JSON (`None` ⇒ omit: undefined / function). `indent` is
    /// the per-level pad (empty ⇒ compact); `depth` is the current nesting.
    /// `holder` is the object/array `key` lives on (the `this` for a function
    /// `replacer`); `replacer` is a callable or undefined; `allowlist`, when
    /// `Some`, restricts which object keys are emitted (the array-replacer form).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn json_value(
        &mut self,
        holder: Value,
        key: &str,
        v: Value,
        indent: &str,
        depth: usize,
        visited: &mut Vec<u32>,
        replacer: Value,
        allowlist: Option<&[String]>,
    ) -> Result<Option<String>, Thrown> {
        let mut out = String::new();
        if self.json_value_into(
            holder, key, v, indent, depth, visited, replacer, allowlist, &mut out,
        )? {
            Ok(Some(out))
        } else {
            Ok(None)
        }
    }

    /// Does either default prototype (Object.prototype / Array.prototype) carry a
    /// callable `toJSON`? Cached on the two protos' shape VERSIONS, so any mutation
    /// that adds/removes `toJSON` there bumps a version and auto-invalidates the
    /// cache (no manual invalidation). Used by `json_value_into` to skip the
    /// per-value `toJSON` probe for plain objects/arrays. `false` ⇒ provably safe
    /// to skip the probe for a plain value with no own `toJSON`.
    fn json_default_protos_have_tojson(&mut self) -> bool {
        let ov = self.heap.version_of(self.obj_proto);
        let av = self.heap.version_of(self.arr_proto);
        if let Some((co, ca, r)) = self.json_default_tj {
            if co == ov && ca == av {
                return r;
            }
        }
        let has = |vm: &mut Self, proto: u32| -> bool {
            if proto == 0 {
                return false;
            }
            let tj = vm
                .get_prop(Value::heap(proto), "toJSON")
                .unwrap_or(Value::UNDEFINED);
            vm.is_callable(tj)
        };
        let r = has(self, self.obj_proto) || has(self, self.arr_proto);
        self.json_default_tj = Some((ov, av, r));
        r
    }

    /// Is `idx` a PLAIN object/array whose only possible `toJSON` would be on a
    /// default prototype? (no custom proto, not a class instance / raw-json, no
    /// own `toJSON`, no `arr_props` overlay for arrays). When true AND
    /// `!json_default_protos_have_tojson()`, `get_prop(v,"toJSON")` is provably
    /// `undefined` and the serializer skips the chain-walking probe entirely.
    fn json_plain_no_own_tojson(&self, idx: u32) -> bool {
        if self.proto_of.contains_key(&idx) {
            return false;
        }
        match self.heap.get(idx) {
            HeapObj::Object(map) => {
                map.class.is_none() && !map.is_raw_json && map.pos("toJSON").is_none()
            }
            HeapObj::Array(_) => !self.arr_props.contains_key(&idx),
            _ => false,
        }
    }

    /// The enumerable own STRING keys of a PLAIN object, in canonical spec order
    /// (integer indices ascending, then string keys in insertion order), as a
    /// `Vec<String>` built DIRECTLY from the ObjMap — no heap-Array allocation, no
    /// per-key `Value` boxing, no `display()` (the keys are already `String`s).
    /// `None` ⇒ the object needs the full `object_enum_own` path: the global
    /// object (slot-backed enumerable var/fn names), a module namespace / deferred
    /// namespace (live-binding TDZ checks), or a non-`Object` heap value (Proxy /
    /// TypedArray / Array / boxed-String exotics are separate variants). For every
    /// other `HeapObj::Object` (plain literal, JSON-parsed node, class instance)
    /// this reproduces `object_enum_own`'s plain-Object key set exactly.
    fn json_object_keys_fast(&self, idx: u32) -> Option<Vec<String>> {
        if idx == self.global_this
            || self.module_namespaces.contains_key(&idx)
            || self.deferred_ns_state.contains_key(&idx)
        {
            return None;
        }
        match self.heap.get(idx) {
            HeapObj::Object(m) => Some(
                spec_key_order(&m.keys)
                    .into_iter()
                    .filter(|&i| m.attrs[i].enumerable && !is_hidden_key(&m.keys[i]))
                    .map(|i| m.keys[i].clone())
                    .collect(),
            ),
            _ => None,
        }
    }

    /// The same key set as `json_object_keys_fast`, but as map SLOTS — no key
    /// clone, no later `pos()` re-lookup. The second element is true when every
    /// selected slot is a PRIMITIVE data property (no accessor, no heap value):
    /// serializing those (with no replacer) runs NO user code, so the map
    /// provably cannot mutate mid-walk and the slots stay exact for the whole
    /// loop. Otherwise the caller must snapshot the key texts before the first
    /// recursion, exactly like the cloning path.
    fn json_object_slots_fast(&self, idx: u32) -> Option<(Vec<usize>, bool)> {
        if idx == self.global_this
            || self.module_namespaces.contains_key(&idx)
            || self.deferred_ns_state.contains_key(&idx)
        {
            return None;
        }
        match self.heap.get(idx) {
            HeapObj::Object(m) => {
                let slots: Vec<usize> = spec_key_order(&m.keys)
                    .into_iter()
                    .filter(|&i| m.attrs[i].enumerable && !is_hidden_key(&m.keys[i]))
                    .collect();
                let all_prim = slots
                    .iter()
                    .all(|&i| !m.attrs[i].accessor && !m.vals[i].is_heap());
                Some((slots, all_prim))
            }
            _ => None,
        }
    }

    /// SerializeJSONProperty, appending straight into a single shared output
    /// buffer instead of building a per-node `String`/`Vec<String>` tree and
    /// joining at every level (the V8 approach). Returns `true` if a value was
    /// written, `false` if the property is OMITTED (undefined / function / symbol
    /// / a replacer-undefined) — for an object the caller rolls the buffer back to
    /// the entry start; for an array the caller writes `null`. Byte-for-byte
    /// identical to the old `Vec<String>` + `wrap_json` path (same escaping,
    /// indent layout, key order, toJSON/replacer/allowlist/cycle/raw-JSON rules).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn json_value_into(
        &mut self,
        holder: Value,
        key: &str,
        v: Value,
        indent: &str,
        depth: usize,
        visited: &mut Vec<u32>,
        replacer: Value,
        allowlist: Option<&[String]>,
        out: &mut String,
    ) -> Result<bool, Thrown> {
        // SerializeJSONProperty: a value with a callable `toJSON` is replaced by
        // `value.toJSON(key)` before serialization (Date, user objects, …).
        // FAST PATH (T0.1): a plain object/array with no own `toJSON` whose
        // default prototypes carry no `toJSON` provably has no callable `toJSON`,
        // so skip the per-value `get_prop(v,"toJSON")` prototype-chain walk
        // (~900k walks on the json bench). The version-keyed cache stays correct
        // if user code mutates a default prototype mid-serialization.
        let v = if v.is_heap() {
            let idx = v.heap_index();
            if self.json_plain_no_own_tojson(idx) && !self.json_default_protos_have_tojson() {
                v
            } else {
                let tj = self.get_prop(v, "toJSON")?;
                if self.is_callable(tj) {
                    let kv = self.alloc_str(key.to_string());
                    self.call_value(tj, v, &[kv])?
                } else {
                    v
                }
            }
        } else {
            v
        };
        // A function `replacer` is applied after `toJSON`: replacer(key, value)
        // with `this` = the holder. Its result is what gets serialized.
        let v = if self.is_callable(replacer) {
            let kv = self.alloc_str(key.to_string());
            self.call_value(replacer, holder, &[kv, v])?
        } else {
            v
        };
        if v.is_undefined() {
            return Ok(false);
        }
        if v.is_null() {
            out.push_str("null");
            return Ok(true);
        }
        if v.is_bool() {
            out.push_str(if v.as_bool() { "true" } else { "false" });
            return Ok(true);
        }
        if v.is_number() {
            let n = v.as_f64();
            if n.is_finite() {
                if json_leaf_fast_enabled() {
                    fmt_f64_into(out, n);
                } else {
                    out.push_str(&fmt_f64(n));
                }
            } else {
                out.push_str("null");
            }
            return Ok(true);
        }
        if !v.is_heap() {
            return Ok(false);
        }
        let idx = v.heap_index();
        // Leaf / primitive-wrapper cases (no recursion into properties).
        match self.heap.get(idx) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                // EXACT bytes: a lone surrogate must emit its \udXXX escape
                // (well-formed JSON.stringify), not a U+FFFD substitution.
                // A flat string's Cow is Borrowed — quote straight from it
                // (`out` is a separate buffer, so the heap borrow is free);
                // only a rope materializes.
                if json_leaf_fast_enabled() {
                    let b = self.heap.str_wtf8_cow(idx).unwrap();
                    json_quote_wtf8_into(out, &b);
                } else {
                    let b = self.heap.str_wtf8_cow(idx).unwrap().into_owned();
                    json_quote_wtf8_into(out, &b);
                }
                return Ok(true);
            }
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::Native(_)
            | HeapObj::NativeClosure { .. }
            | HeapObj::Symbol { .. } => return Ok(false),
            HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => {
                return Err(Thrown(
                    "TypeError: Do not know how to serialize a BigInt".into(),
                ))
            }
            // A boxed primitive serializes as ToString / ToNumber / its boolean —
            // observably invoking the wrapper's toString/valueOf (which may throw).
            HeapObj::Boxed { kind: 0, .. } => {
                let s = self.to_js_string(v)?;
                json_quote_into(out, &s);
                return Ok(true);
            }
            HeapObj::Boxed { kind: 1, .. } => {
                // ToNumber(wrapper): ToPrimitive(number) so an overridden
                // valueOf/@@toPrimitive fires (to_number_coerce reads [[NumberData]]).
                let prim = self.to_primitive_number(v)?;
                let n = self.to_number(prim)?;
                if n.is_finite() {
                    if json_leaf_fast_enabled() {
                        fmt_f64_into(out, n);
                    } else {
                        out.push_str(&fmt_f64(n));
                    }
                } else {
                    out.push_str("null");
                }
                return Ok(true);
            }
            HeapObj::Boxed { kind: 2, value } => {
                let b = self.truthy(*value);
                out.push_str(if b { "true" } else { "false" });
                return Ok(true);
            }
            // A boxed BigInt (Object(0n)) throws like a primitive BigInt; a boxed
            // Symbol falls through to SerializeJSONObject ("{}").
            HeapObj::Boxed { value, .. } => {
                if value.is_heap()
                    && matches!(
                        self.heap.get(value.heap_index()),
                        HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
                    )
                {
                    return Err(Thrown(
                        "TypeError: Do not know how to serialize a BigInt".into(),
                    ));
                }
            }
            HeapObj::Object(map) if map.is_raw_json => {
                // [[IsRawJSON]]: emit the stored "rawJSON" text verbatim.
                let raw_val = map.get("rawJSON").unwrap_or(Value::UNDEFINED);
                let s = self
                    .heap
                    .str_cow(raw_val.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                out.push_str(&s);
                return Ok(true);
            }
            _ => {}
        }
        // SerializeJSONArray / SerializeJSONObject. Both read properties via REAL
        // [[Get]] (so getters / Proxy traps fire and abrupt completions propagate),
        // and detect cycles via `visited`. The PropertyList allowlist is GLOBAL — it
        // filters object keys at EVERY nesting level, including objects inside arrays.
        if visited.contains(&idx) {
            return Err(Thrown(
                "TypeError: Converting circular structure to JSON".into(),
            ));
        }
        visited.push(idx);
        let pad = if indent.is_empty() {
            String::new()
        } else {
            indent.repeat(depth + 1)
        };
        let pad_close = if indent.is_empty() {
            String::new()
        } else {
            indent.repeat(depth)
        };
        if self.value_is_array(v) {
            // len = ToLength(Get(val, "length"))
            let lenv = self.get_prop(v, "length")?;
            let lenf = self.to_number_coerce(lenv)?;
            let len: u64 = if lenf.is_nan() || lenf <= 0.0 {
                0
            } else {
                lenf.min(9007199254740991.0) as u64
            };
            out.push('[');
            let mut i: u64 = 0;
            while i < len {
                if i > 0 {
                    out.push(',');
                }
                if !indent.is_empty() {
                    out.push('\n');
                    out.push_str(&pad);
                }
                // FAST PATH (T0.1): for a dense in-range element of an array with
                // no `arr_props` overlay, read `items[i]` directly — skipping the
                // `json_get` generic-index dispatch (string-key coercion + chain
                // resolution) per element. Any overlay / virtual-length / OOB falls
                // back to `json_get` (which handles holes/proto exactly).
                let direct = if !self.array_elements_overlaid(idx) {
                    match self.heap.get(idx) {
                        // A present (non-hole) dense element. A HOLE falls back to
                        // `json_get` so the prototype chain is walked exactly.
                        HeapObj::Array(a) => match a.get(i as usize) {
                            Some(e) if !e.is_hole() => Some(*e),
                            _ => None,
                        },
                        _ => None,
                    }
                } else {
                    None
                };
                let e = match direct {
                    Some(e) => e,
                    None => match self.json_get(v, &i.to_string()) {
                        Ok(e) => e,
                        Err(e) => {
                            visited.pop();
                            return Err(e);
                        }
                    },
                };
                // The element key is only observable if the element has a callable
                // `toJSON` / there is a replacer — defer the `i.to_string()` alloc
                // to that case (a heap element); primitives pass an empty key.
                let ks: String = if e.is_heap() || self.is_callable(replacer) {
                    i.to_string()
                } else {
                    String::new()
                };
                // An omitted array element serializes as `null` (NOT skipped).
                let wrote = match self.json_value_into(
                    v,
                    &ks,
                    e,
                    indent,
                    depth + 1,
                    visited,
                    replacer,
                    allowlist,
                    out,
                ) {
                    Ok(w) => w,
                    Err(e) => {
                        visited.pop();
                        return Err(e);
                    }
                };
                if !wrote {
                    out.push_str("null");
                }
                i += 1;
            }
            if len > 0 && !indent.is_empty() {
                out.push('\n');
                out.push_str(&pad_close);
            }
            out.push(']');
        } else {
            // FAST PATH (leaf emission): with no allowlist and no function
            // replacer, walk a plain object's keys as map SLOTS instead of
            // cloned Strings re-found by `pos()` each iteration. Two tiers:
            // every value a primitive data property ⇒ nothing in the loop can
            // run user code, so the map provably never mutates and keys are
            // quoted straight from the borrow (no clone, no re-lookup, no
            // version check); otherwise the key texts are snapshotted upfront
            // (same clones as the old path — a toJSON/getter may delete them
            // out from under the walk) and only the `pos()` re-lookup is
            // elided, guarded by the map version (a delete shifts slots and
            // bumps it). `ZIPP_NO_JSON_LEAF_FAST=1` restores the cloning walk
            // below.
            let slot_plan =
                if json_leaf_fast_enabled() && allowlist.is_none() && !self.is_callable(replacer) {
                    self.json_object_slots_fast(idx)
                } else {
                    None
                };
            if let Some((slots, all_prim)) = slot_plan {
                let sep = if indent.is_empty() { ":" } else { ": " };
                out.push('{');
                let mut any = false;
                if all_prim {
                    for &slot in &slots {
                        // Tentatively write `[,]\n pad "key"sep`, then the
                        // value; an OMITTED value (undefined) rolls the buffer
                        // back to before this entry — same as the cloning path.
                        let mark = out.len();
                        let val = match self.heap.get(idx) {
                            HeapObj::Object(m) => {
                                if any {
                                    out.push(',');
                                }
                                if !indent.is_empty() {
                                    out.push('\n');
                                    out.push_str(&pad);
                                }
                                json_quote_into(out, &m.keys[slot]);
                                out.push_str(sep);
                                m.vals[slot]
                            }
                            // Unreachable — only user code changes a heap
                            // slot's variant and none has run; an omitted
                            // entry (undefined) is the harmless answer.
                            _ => Value::UNDEFINED,
                        };
                        // The key is unobservable for a primitive value with
                        // no replacer (no toJSON probe, no replacer call) —
                        // pass "" like the array path does for primitives.
                        let wrote = match self.json_value_into(
                            v,
                            "",
                            val,
                            indent,
                            depth + 1,
                            visited,
                            replacer,
                            allowlist,
                            out,
                        ) {
                            Ok(w) => w,
                            Err(e) => {
                                visited.pop();
                                return Err(e);
                            }
                        };
                        if wrote {
                            any = true;
                        } else {
                            out.truncate(mark);
                        }
                    }
                } else {
                    let v0 = self.heap.version_of(idx);
                    let keys: Vec<(usize, String)> = match self.heap.get(idx) {
                        HeapObj::Object(m) => {
                            slots.iter().map(|&i| (i, m.keys[i].clone())).collect()
                        }
                        // Unreachable: unchanged since `json_object_slots_fast`.
                        _ => Vec::new(),
                    };
                    for (slot, k) in &keys {
                        // Value read at SERIALIZATION time (so a prior key's
                        // toJSON that mutated this one is observed). While the
                        // version is unchanged the snapshot slot IS `pos(&k)`;
                        // after a bump, re-find the key exactly as the cloning
                        // path always did (accessor / deleted ⇒ `json_get`).
                        let direct = if self.heap.version_of(idx) == v0 {
                            match self.heap.get(idx) {
                                HeapObj::Object(m) if !m.attrs[*slot].accessor => {
                                    Some(m.vals[*slot])
                                }
                                _ => None,
                            }
                        } else {
                            match self.heap.get(idx) {
                                HeapObj::Object(m) => match m.pos(k) {
                                    Some(i) if !m.attrs[i].accessor => Some(m.vals[i]),
                                    _ => None,
                                },
                                _ => None,
                            }
                        };
                        let val = match direct {
                            Some(val) => val,
                            None => match self.json_get(v, k) {
                                Ok(val) => val,
                                Err(e) => {
                                    visited.pop();
                                    return Err(e);
                                }
                            },
                        };
                        let mark = out.len();
                        if any {
                            out.push(',');
                        }
                        if !indent.is_empty() {
                            out.push('\n');
                            out.push_str(&pad);
                        }
                        json_quote_into(out, k);
                        out.push_str(sep);
                        let wrote = match self.json_value_into(
                            v,
                            k,
                            val,
                            indent,
                            depth + 1,
                            visited,
                            replacer,
                            allowlist,
                            out,
                        ) {
                            Ok(w) => w,
                            Err(e) => {
                                visited.pop();
                                return Err(e);
                            }
                        };
                        if wrote {
                            any = true;
                        } else {
                            out.truncate(mark);
                        }
                    }
                }
                if any && !indent.is_empty() {
                    out.push('\n');
                    out.push_str(&pad_close);
                }
                out.push('}');
                visited.pop();
                return Ok(true);
            }
            // EnumerableOwnPropertyNames(val) — or the PropertyList, when given.
            // FAST PATH (T0.5/T0.6): a plain object (not global / namespace) yields
            // its enumerable own string keys as a `Vec<String>` straight from the
            // ObjMap (no heap-Array, no `display()`), and below its DATA values are
            // read directly from the map — eliding `object_enum_own`'s array alloc
            // + per-key display + the per-key `json_get` dispatch. `use_fast` gates
            // both the keys and the value reads together.
            let fast_keys = if allowlist.is_none() {
                self.json_object_keys_fast(idx)
            } else {
                None
            };
            let use_fast = fast_keys.is_some();
            let keys: Vec<String> = match (allowlist, fast_keys) {
                (Some(a), _) => a.to_vec(),
                (None, Some(ks)) => ks,
                (None, None) => {
                    let kv = match self.object_enum_own(v, crate::vm::EnumWhat::Keys) {
                        Ok(kv) => kv,
                        Err(e) => {
                            visited.pop();
                            return Err(e);
                        }
                    };
                    match self.heap.get(kv.heap_index()) {
                        HeapObj::Array(a) => a.iter().map(|&k| self.display(k)).collect(),
                        _ => Vec::new(),
                    }
                }
            };
            let sep = if indent.is_empty() { ":" } else { ": " };
            out.push('{');
            let mut any = false;
            for k in keys {
                // Value read at SERIALIZATION time (so a prior key's toJSON that
                // mutated this one is observed). Fast path: a non-accessor own data
                // slot reads `vals[slot]` directly; an accessor / a key deleted
                // during recursion / anything else falls back to `json_get` (runs
                // the getter, walks the prototype, deleted⇒undefined⇒omitted).
                let direct = if use_fast {
                    match self.heap.get(idx) {
                        HeapObj::Object(m) => match m.pos(&k) {
                            Some(i) if !m.attrs[i].accessor => Some(m.vals[i]),
                            _ => None,
                        },
                        _ => None,
                    }
                } else {
                    None
                };
                let val = match direct {
                    Some(val) => val,
                    None => match self.json_get(v, &k) {
                        Ok(val) => val,
                        Err(e) => {
                            visited.pop();
                            return Err(e);
                        }
                    },
                };
                // Tentatively write `[,]\n pad "key"sep`, then the value; if the
                // value is OMITTED, roll the buffer back to before this entry (so
                // an undefined-valued property leaves no trace, incl. its comma).
                let mark = out.len();
                if any {
                    out.push(',');
                }
                if !indent.is_empty() {
                    out.push('\n');
                    out.push_str(&pad);
                }
                json_quote_into(out, &k);
                out.push_str(sep);
                let wrote = match self.json_value_into(
                    v,
                    &k,
                    val,
                    indent,
                    depth + 1,
                    visited,
                    replacer,
                    allowlist,
                    out,
                ) {
                    Ok(w) => w,
                    Err(e) => {
                        visited.pop();
                        return Err(e);
                    }
                };
                if wrote {
                    any = true;
                } else {
                    out.truncate(mark);
                }
            }
            if any && !indent.is_empty() {
                out.push('\n');
                out.push_str(&pad_close);
            }
            out.push('}');
        }
        visited.pop();
        Ok(true)
    }

    /// Parse a JSON string into a Value, or throw SyntaxError. Recursive-descent
    /// over the byte string (structure tokens are ASCII; string content is
    /// flushed as UTF-8 slices). Allocates heap objects/arrays/strings.
    pub(crate) fn json_parse(&mut self, src: &[u8]) -> Result<Value, Thrown> {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::JsonParse);
        // W9 static pretenure (NURSERY_DESIGN.md §4): the parsed tree is the
        // measured pretenure case (B119's oracle: json-large's 48% old-trace
        // share with ~zero old→young stores), so the whole builder allocates
        // OLD. No user code runs inside this scope (the reviver path,
        // `internalize_json`, is deliberately OUTSIDE it), so no GC-visible
        // young value can be created and missed here. Manual begin/end pair —
        // the error path must unwind the depth too.
        self.heap.pretenure_begin();
        let r = self.json_parse_scoped(src);
        self.heap.pretenure_end();
        r
    }

    fn json_parse_scoped(&mut self, src: &[u8]) -> Result<Value, Thrown> {
        let mut i = 0;
        json_skip_ws(src, &mut i);
        let v = self.json_parse_value(src, &mut i)?;
        json_skip_ws(src, &mut i);
        if i != src.len() {
            return Err(Thrown(
                "SyntaxError: Unexpected non-whitespace character after JSON".into(),
            ));
        }
        Ok(v)
    }

    pub(crate) fn json_parse_value(&mut self, src: &[u8], i: &mut usize) -> Result<Value, Thrown> {
        let b = src;
        match b.get(*i).copied() {
            Some(b'{') => self.json_parse_object(src, i),
            Some(b'[') => self.json_parse_array(src, i),
            Some(b'"') => {
                let js = json_parse_string(src, i)?;
                Ok(Value::heap(self.heap.alloc_js(js)))
            }
            Some(b't') => {
                json_expect(b, i, "true")?;
                Ok(Value::bool(true))
            }
            Some(b'f') => {
                json_expect(b, i, "false")?;
                Ok(Value::bool(false))
            }
            Some(b'n') => {
                json_expect(b, i, "null")?;
                Ok(Value::NULL)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => json_parse_number(b, i),
            _ => Err(Thrown("SyntaxError: Unexpected token in JSON".into())),
        }
    }

    pub(crate) fn json_parse_array(&mut self, src: &[u8], i: &mut usize) -> Result<Value, Thrown> {
        let b = src;
        *i += 1; // '['
        let mut items = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) == Some(&b']') {
            *i += 1;
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(items))));
        }
        loop {
            json_skip_ws(b, i);
            let v = self.json_parse_value(src, i)?;
            items.push(v);
            json_skip_ws(b, i);
            match b.get(*i) {
                Some(b',') => *i += 1,
                Some(b']') => {
                    *i += 1;
                    break;
                }
                _ => {
                    return Err(Thrown(
                        "SyntaxError: Expected ',' or ']' in JSON array".into(),
                    ))
                }
            }
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(items))))
    }

    pub(crate) fn json_parse_object(&mut self, src: &[u8], i: &mut usize) -> Result<Value, Thrown> {
        let b = src;
        *i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b'}') {
            loop {
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b'"') {
                    return Err(Thrown(
                        "SyntaxError: Expected property name string in JSON".into(),
                    ));
                }
                let key = json_parse_string(src, i)?.to_lossy_string();
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(Thrown("SyntaxError: Expected ':' in JSON object".into()));
                }
                *i += 1;
                json_skip_ws(b, i);
                let val = self.json_parse_value(src, i)?;
                pairs.push((key, val));
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => break,
                    _ => {
                        return Err(Thrown(
                            "SyntaxError: Expected ',' or '}' in JSON object".into(),
                        ))
                    }
                }
            }
        }
        *i += 1; // '}'
                 // `set_owned`, not `set(&k, …)`: the parser already allocated each key
                 // (`to_lossy_string` above), and `set` cloned a SECOND copy on first
                 // insertion only to drop the first. `with_capacity` then sizes the three
                 // parallel vectors once instead of growing them log n times — `pairs.len()`
                 // is exact for a duplicate-free object and a harmless over-reserve otherwise.
        let mut map = crate::heap::ObjMap::with_capacity(pairs.len());
        for (k, v) in pairs {
            map.set_owned(k, v);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(map)))))
    }

    /// If `holder` is an Array and `key` is a canonical index string, the index.
    fn array_element_index(&self, holder: Value, key: &str) -> Option<usize> {
        if holder.is_heap() {
            if let HeapObj::Array(_) = self.heap.get(holder.heap_index()) {
                return key.parse::<usize>().ok().filter(|i| i.to_string() == key);
            }
        }
        None
    }

    /// `[[Get]](holder, key)` for the reviver walk: a canonical array index goes
    /// through `get_index` (so an absent element reads up the prototype chain), any
    /// other key through the named `[[Get]]`. Both observe getters / Proxy traps.
    fn json_get(&mut self, holder: Value, key: &str) -> Result<Value, Thrown> {
        if let Ok(i) = key.parse::<u32>() {
            if i.to_string() == *key {
                return self.get_index(holder, Value::num(i as f64));
            }
        }
        self.get_prop(holder, key)
    }

    /// CreateDataProperty(target, key, value): `target.[[DefineOwnProperty]]` with a
    /// fresh `{value, writable, enumerable, configurable}` data descriptor. A Proxy's
    /// defineProperty trap may throw (propagated); an ordinary object that REJECTS the
    /// define (e.g. a non-configurable existing prop) just returns false — no throw.
    fn json_create_data(&mut self, target: Value, key: &str, value: Value) -> Result<(), Thrown> {
        let is_proxy =
            target.is_heap() && matches!(self.heap.get(target.heap_index()), HeapObj::Proxy { .. });
        let mut m = crate::heap::ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::TRUE);
        m.set("enumerable", Value::TRUE);
        m.set("configurable", Value::TRUE);
        let desc = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
        let r = self.object_define_property(target, key, desc);
        if is_proxy {
            r
        } else {
            let _ = r; // ordinary [[DefineOwnProperty]] never throws; a reject is false
            Ok(())
        }
    }

    /// InternalizeJSONProperty: walk the parsed tree bottom-up, replacing each
    /// `holder[key]` with `reviver.call(holder, key, value, context)`. Children
    /// are revived before their parent; a child revived to `undefined` is
    /// deleted. `src` is this value's parse-source node (ES2025
    /// json-parse-with-source): a primitive's `context` carries its raw source
    /// text, an array/object's `context` is an empty object.
    pub(crate) fn internalize_json(
        &mut self,
        holder: Value,
        key: &str,
        reviver: Value,
        src: Option<&JsonSrc>,
    ) -> Result<Value, Thrown> {
        // 1. val = ? Get(holder, name)  — a real [[Get]] (getters / Proxy / the
        // prototype chain are all observed, e.g. a deleted element reads its inherited
        // value).
        let val = self.json_get(holder, key)?;
        // json-parse-with-source correspondence (proposal InternalizeJSONProperty
        // step 3): the parse node applies only while the CURRENT value still
        // SameValue-matches the value it produced. A reviver that forward-modified
        // this holder entry invalidates the snapshot — its `context` carries no
        // `source` and its children no longer correspond.
        let src = src.filter(|s| self.same_value(s.snapshot(), val));
        // 2. If Type(val) is Object: recurse into its elements / enumerable props
        // using REAL object operations so a reviver that mutates the holder (changing
        // length, replacing a value with a Proxy, making a prop non-configurable, …)
        // is observed and any abrupt completion propagates.
        if val.is_heap() && self.is_object_value(val) {
            if self.value_is_array(val) {
                // 2.b.ii  len = ? ToLength(? Get(val, "length"))
                let lenv = self.get_prop(val, "length")?;
                let lenf = self.to_number_coerce(lenv)?;
                let len: u64 = if lenf.is_nan() || lenf <= 0.0 {
                    0
                } else {
                    lenf.min(9007199254740991.0) as u64
                };
                let mut i: u64 = 0;
                while i < len {
                    let k = i.to_string();
                    // Source tracking only applies to the ORIGINAL parsed element at
                    // this position; the snapshot check at the child's own entry
                    // drops a reviver-replaced value's source.
                    let child = match src {
                        Some(JsonSrc::Arr(v, _)) => v.get(i as usize),
                        _ => None,
                    };
                    let nv = self.internalize_json(val, &k, reviver, child)?;
                    if nv.is_undefined() {
                        self.delete_property(val, &k)?; // ? val.[[Delete]](ToString(I))
                    } else {
                        self.json_create_data(val, &k, nv)?; // ? CreateDataProperty
                    }
                    i += 1;
                }
            } else {
                // 2.c  keys = ? EnumerableOwnPropertyNames(val, key)  — proxy-aware
                // (the ownKeys trap may throw), in integer-then-insertion order.
                let keys_v = self.object_enum_own(val, crate::vm::EnumWhat::Keys)?;
                let keys: Vec<String> = match self.heap.get(keys_v.heap_index()) {
                    HeapObj::Array(a) => a.iter().map(|&k| self.display(k)).collect(),
                    _ => Vec::new(),
                };
                for k in keys {
                    let child = match src {
                        Some(JsonSrc::Obj(pairs, _)) => {
                            pairs.iter().find(|(pk, _)| pk == &k).map(|(_, s)| s)
                        }
                        _ => None,
                    };
                    let nv = self.internalize_json(val, &k, reviver, child)?;
                    if nv.is_undefined() {
                        self.delete_property(val, &k)?;
                    } else {
                        self.json_create_data(val, &k, nv)?;
                    }
                }
            }
        }
        let context = self.make_json_context(src);
        let kv = self.alloc_str(key.to_string());
        self.call_value(reviver, holder, &[kv, val, context])
    }

    /// The reviver `context`: a plain object that, for a primitive parse node,
    /// carries a `"source"` data property holding the value's raw JSON text.
    /// An array/object node yields an empty context.
    fn make_json_context(&mut self, src: Option<&JsonSrc>) -> Value {
        let ctx = Value::heap(
            self.heap
                .alloc(HeapObj::Object(Box::new(crate::heap::ObjMap::new()))),
        );
        if let Some(JsonSrc::Prim(s, _)) = src {
            let sv = self.alloc_str(s.clone());
            if let HeapObj::Object(m) = self.heap.get_mut(ctx.heap_index()) {
                m.set("source", sv);
            }
        }
        ctx
    }

    /// Like [`json_parse`], but also returns a parallel source tree recording the
    /// raw JSON text of every value (for the parse-with-source reviver context).
    pub(crate) fn json_parse_with_src(&mut self, src: &[u8]) -> Result<(Value, JsonSrc), Thrown> {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::JsonParse);
        // W9 static pretenure — same scope as `json_parse`; the reviver runs
        // later, outside this call, so its results stay young.
        self.heap.pretenure_begin();
        let r = self.json_parse_with_src_scoped(src);
        self.heap.pretenure_end();
        r
    }

    fn json_parse_with_src_scoped(&mut self, src: &[u8]) -> Result<(Value, JsonSrc), Thrown> {
        let mut i = 0;
        json_skip_ws(src, &mut i);
        let r = self.json_parse_value_src(src, &mut i)?;
        json_skip_ws(src, &mut i);
        if i != src.len() {
            return Err(Thrown(
                "SyntaxError: Unexpected non-whitespace character after JSON".into(),
            ));
        }
        Ok(r)
    }

    fn json_parse_value_src(
        &mut self,
        src: &[u8],
        i: &mut usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        let b = src;
        match b.get(*i).copied() {
            Some(b'{') => self.json_parse_object_src(src, i),
            Some(b'[') => self.json_parse_array_src(src, i),
            _ => {
                // A primitive (string/number/true/false/null): record its exact span.
                let start = *i;
                let v = self.json_parse_value(src, i)?;
                // `context.source` is a Rust String — LOSSY if the span holds
                // a raw lone surrogate (documented limit; escapes round-trip).
                Ok((
                    v,
                    JsonSrc::Prim(crate::heap::wtf8_to_lossy_string(&src[start..*i]), v),
                ))
            }
        }
    }

    fn json_parse_array_src(
        &mut self,
        src: &[u8],
        i: &mut usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        let b = src;
        *i += 1; // '['
        let mut items = Vec::new();
        let mut srcs = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b']') {
            loop {
                json_skip_ws(b, i);
                let (v, s) = self.json_parse_value_src(src, i)?;
                items.push(v);
                srcs.push(s);
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b']') => break,
                    _ => {
                        return Err(Thrown(
                            "SyntaxError: Expected ',' or ']' in JSON array".into(),
                        ))
                    }
                }
            }
        }
        *i += 1; // ']'
        let av = Value::heap(self.heap.alloc(HeapObj::Array(items)));
        Ok((av, JsonSrc::Arr(srcs, av)))
    }

    fn json_parse_object_src(
        &mut self,
        src: &[u8],
        i: &mut usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        let b = src;
        *i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        let mut srcs: Vec<(String, JsonSrc)> = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b'}') {
            loop {
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b'"') {
                    return Err(Thrown(
                        "SyntaxError: Expected property name string in JSON".into(),
                    ));
                }
                let key = json_parse_string(src, i)?.to_lossy_string();
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(Thrown("SyntaxError: Expected ':' in JSON object".into()));
                }
                *i += 1;
                json_skip_ws(b, i);
                let (val, s) = self.json_parse_value_src(src, i)?;
                pairs.push((key.clone(), val));
                // A DUPLICATE key OVERWRITES, exactly as the object build below
                // does (`map.set`): the LAST member is what the property ends up
                // holding, so that is the parse node `context.source` must report.
                // Appending instead made the lookup find the FIRST member, whose
                // snapshot no longer matched the property's value — so the
                // correspondence check dropped `source` altogether
                // (staging/sm/JSON/parse-with-source.js line 76,
                // `{ "b": 2, "b": 1, "b": 4 }`).
                match srcs.iter_mut().find(|(k, _)| *k == key) {
                    Some(e) => e.1 = s,
                    None => srcs.push((key, s)),
                }
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => break,
                    _ => {
                        return Err(Thrown(
                            "SyntaxError: Expected ',' or '}' in JSON object".into(),
                        ))
                    }
                }
            }
        }
        *i += 1; // '}'
                 // As in `json_parse_object`. The `key.clone()` above stays: this variant
                 // maintains a PARALLEL source tree that needs the key too, so one of the two
                 // must own a copy. `set_owned` still removes the third allocation — the one
                 // `set` made inside the map.
        let mut map = crate::heap::ObjMap::with_capacity(pairs.len());
        for (k, v) in pairs {
            map.set_owned(k, v);
        }
        let ov = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(map))));
        Ok((ov, JsonSrc::Obj(srcs, ov)))
    }
}

/// A parallel tree to a parsed JSON value recording each node's raw source text
/// AND the value the node produced (its snapshot), for the ES2025
/// parse-with-source reviver `context.source`. The snapshot drives the spec's
/// SameValue correspondence check: a holder entry the reviver forward-modified
/// no longer matches its parse node, so its `context` loses `source` and its
/// children stop corresponding. Snapshot `Value`s are held across reviver
/// callbacks — safe because the whole walk runs under a `gc_lock_guard`.
pub(crate) enum JsonSrc {
    /// A primitive leaf — the exact JSON text that produced it (e.g. `"1.1"`).
    Prim(String, Value),
    Arr(Vec<JsonSrc>, Value),
    Obj(Vec<(String, JsonSrc)>, Value),
}

impl JsonSrc {
    /// The value this parse node produced at parse time.
    pub(crate) fn snapshot(&self) -> Value {
        match self {
            JsonSrc::Prim(_, v) | JsonSrc::Arr(_, v) | JsonSrc::Obj(_, v) => *v,
        }
    }
}
