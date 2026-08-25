#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// `ZIPP_BUILTINSTATS=1` counts every builtin method dispatch by
/// `(receiver kind, method name)` and prints the histogram at exit.
///
/// It exists because the B5.3 target list should come from the benchmarks, not
/// from reading `string_ops.rs` and picking what looks expensive — §5's standing
/// lesson is that every probe which started from reading the code and reasoning
/// about what ought to be costly has been wrong. Measured with it OFF the
/// counter is one relaxed atomic load, and the `Mutex` is only ever reached when
/// it is on.
///
/// What makes the list actionable: a builtin that HAS a region intrinsic runs at
/// or near node (`charCodeAt` 0.5ns, `map.get` 6.5ns, `set.has` 7.0ns) and one
/// that does not costs 26-45ns in BOTH tiers — and in compiled code it is
/// actually slower than the interpreter, because the region pays the
/// `jit_call_method_ic` round trip on top of the identical shared dispatch
/// (`str.startsWith` 44.5ns JIT against 39.0ns NOJIT).
mod bstats {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Mutex;

    static ON: AtomicU8 = AtomicU8::new(2);
    static TABLE: Mutex<Option<HashMap<(&'static str, String), u64>>> = Mutex::new(None);

    #[inline]
    pub(super) fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_BUILTINSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    pub(super) fn bump(kind: &'static str, name: &str) {
        let mut g = match TABLE.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *g.get_or_insert_with(HashMap::new)
            .entry((kind, name.to_string()))
            .or_insert(0) += 1;
    }

    /// `(kind, name, calls)` sorted by call count, descending.
    pub fn dump() -> Vec<(&'static str, String, u64)> {
        let g = match TABLE.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let mut v: Vec<_> = match &*g {
            Some(m) => m.iter().map(|((k, n), c)| (*k, n.clone(), *c)).collect(),
            None => Vec::new(),
        };
        v.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.0.cmp(b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        v
    }
}

pub use bstats::dump as builtin_stats;

/// Classify a receiver for the [`bstats`] histogram. Deliberately coarse — the
/// question it answers is "which (kind, name) pairs deserve a region intrinsic",
/// and that is decided per heap kind.
#[inline]
pub(super) fn builtin_stats_count(vm: &Vm<'_>, recv: Value, name: &str) {
    if !bstats::enabled() {
        return;
    }
    let kind = if recv.is_number() {
        "number"
    } else if !recv.is_heap() {
        "primitive"
    } else {
        match vm.heap.get(recv.heap_index()) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => "string",
            HeapObj::Array(_) => "array",
            HeapObj::Object(_) => "object",
            HeapObj::Map { .. } => "map",
            HeapObj::Set(_) => "set",
            HeapObj::Promise { .. } => "promise",
            HeapObj::RegExp { .. } => "regexp",
            HeapObj::Date(_) => "date",
            HeapObj::TypedArray { .. } => "typedarray",
            HeapObj::DataView { .. } => "dataview",
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Native(_) => "function",
            _ => "other",
        }
    };
    bstats::bump(kind, name);
}

/// `ZIPP_NO_PROMISE_PRISTINE=1` makes `promise_method_is_intrinsic` always
/// decline, which restores the original `get_prop`-walk proof EXACTLY — the
/// probe sits behind a `||`, so a `false` runs the old expression unchanged.
/// Exists so B79 can be A/B'd with `tools/bench.py --ab-env` on ONE binary (no
/// fat-LTO layout confound, the thing B77 was reverted for), and so any
/// behaviour question can be bisected against the old path without a rebuild.
#[inline]
fn promise_pristine_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_PROMISE_PRISTINE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Conservative comparison bound for the two TypedArray sort implementations.
/// The callback path is insertion sort (quadratic in the adversarial case); the
/// intrinsic comparator delegates to Rust's comparison sort (O(n log n)).
fn typed_array_sort_work_bound(len: usize, callback: bool) -> u64 {
    let n = len as u64;
    if callback {
        n.saturating_mul(n.saturating_sub(1)) / 2
    } else if n <= 1 {
        0
    } else {
        let levels = u64::BITS - (n - 1).leading_zeros();
        n.saturating_mul(levels as u64)
    }
}

impl<'p> Vm<'p> {
    /// Try a builtin method on an array or string receiver. Returns
    /// `Ok(Some(result))` when `name` is a recognised builtin, `Ok(None)` when
    /// it isn't (the caller then treats it as a user-defined method/property).
    ///
    /// Dispatch is split by receiver type into focused helpers so each stays
    /// readable. Methods that take a JS callback (`map`/`filter`/`reduce`/
    /// `sort`) clone the element snapshot out of the heap BEFORE invoking the
    /// callback, because a callback can mutate the same array (which would
    /// reallocate its `Vec` and invalidate any borrow held across the call).
    pub(crate) fn try_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<Option<Value>, Thrown> {
        // Gather args into a stack buffer for the common small-arity case (1-2
        // args for push/map/filter/…), avoiding a heap Vec alloc per call; only
        // a rare >8-arg call falls back to the heap.
        let mut stackbuf = [Value::UNDEFINED; 8];
        let heapbuf: Vec<Value>;
        let n = arg_base as usize;
        let args: &[Value] = if argc as usize <= stackbuf.len() {
            for i in 0..argc as usize {
                stackbuf[i] = self.regs[base + n + i];
            }
            &stackbuf[..argc as usize]
        } else {
            heapbuf = (0..argc as usize)
                .map(|i| self.regs[base + n + i])
                .collect();
            &heapbuf
        };
        self.dispatch_builtin_method(recv, name, args)
    }

    /// Is a `recv.call(…)` / `recv.apply(…)` / `recv.bind(…)` name-dispatch sound — i.e. would a
    /// real Get on `recv` resolve `name` to the pristine `%Function.prototype%`
    /// native? Requires: an ORDINARY callable (Func/Closure/Native/Bound — a
    /// Class's statics and a Proxy's get trap must resolve generically), no own
    /// `fn_props` shadow, `[[Prototype]]` still the main `%Function.prototype%`,
    /// and that prototype's own slot still the `FN_CALL`/`FN_APPLY` native data
    /// property. Pure reads — a `false` sends the caller to the generic
    /// get_prop + call_value tail, which observes whatever is really installed.
    pub(crate) fn fn_call_apply_pristine(&self, recv: Value, name: &str) -> bool {
        use crate::vm::native::{FN_APPLY, FN_BIND, FN_CALL};
        if !recv.is_heap() {
            return false;
        }
        let idx = recv.heap_index();
        if !matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Native(_) | HeapObj::Bound { .. }
        ) {
            return false;
        }
        if self
            .fn_props
            .get(&idx)
            .is_some_and(|m| m.pos(name).is_some())
        {
            return false;
        }
        if self
            .proto_of
            .get(&idx)
            .is_some_and(|&p| p != Value::heap(self.fn_proto))
        {
            return false;
        }
        let want = match name {
            "apply" => FN_APPLY,
            "bind" => FN_BIND,
            _ => FN_CALL,
        };
        match self.heap.get(self.fn_proto) {
            HeapObj::Object(m) => m.pos(name).is_some_and(|slot| {
                !m.attrs[slot].accessor
                    && m.vals[slot].is_heap()
                    && matches!(
                        self.heap.get(m.vals[slot].heap_index()),
                        HeapObj::Native(id) if *id == want
                    )
            }),
            _ => false,
        }
    }

    /// Whether an Array receiver's live `slice` / `concat` method Get resolves
    /// to the matching main-realm intrinsic. These two copy methods participate
    /// in the allocation-loop reducer, so their ordinary name-dispatched path
    /// must share the same override contract: an own shadow, custom prototype,
    /// deleted slot, accessor, or replacement function falls through to the
    /// generic Get + call path.
    pub(crate) fn array_copy_method_is_intrinsic(&self, idx: u32, name: &str) -> bool {
        if !matches!(name, "slice" | "concat")
            || !matches!(self.heap.get(idx), HeapObj::Array(_))
            || self
                .arr_props
                .get(&idx)
                .is_some_and(|m| m.pos(name).is_some())
            || self
                .proto_of
                .get(&idx)
                .is_some_and(|&proto| proto != Value::heap(self.arr_proto))
            || self.arr_proto == 0
        {
            return false;
        }
        match self.heap.get(self.arr_proto) {
            HeapObj::Object(map) => map.pos(name).is_some_and(|slot| {
                !map.attrs[slot].accessor
                    && map.vals[slot].is_heap()
                    && matches!(self.heap.get(map.vals[slot].heap_index()), HeapObj::Native(id)
                        if native::proto_method(*id)
                            .is_some_and(|(method, kind, _)| method == name && kind == 0))
            }),
            _ => false,
        }
    }

    /// Whether a name-dispatched Map/Set call really resolves to the matching
    /// main-realm intrinsic.  Receiver-kind dispatch alone is not sufficient:
    /// collection instances are ordinary objects for named properties, their
    /// `[[Prototype]]` can be replaced, and every prototype method is writable.
    ///
    /// This is a read-only, fail-closed proof shared by the interpreter's
    /// builtin shortcut and the native collection helpers.  An own shadow, a
    /// subclass/custom prototype, a deleted/accessor/replaced prototype slot,
    /// or a child-realm prototype sends the caller through ordinary Get+Call.
    pub(crate) fn collection_method_is_intrinsic(&self, idx: u32, name: &str, kind: u8) -> bool {
        let proto = match (self.heap.get(idx), kind) {
            (HeapObj::Set(_), 3) => self.set_proto,
            (HeapObj::Map { .. }, 4) => self.map_proto,
            _ => return false,
        };
        if proto == 0
            || self.active_realm_proto(proto) != proto
            || self
                .arr_props
                .get(&idx)
                .is_some_and(|props| props.pos(name).is_some())
            || self
                .proto_of
                .get(&idx)
                .is_some_and(|&actual| actual != Value::heap(proto))
        {
            return false;
        }
        match self.heap.get(proto) {
            HeapObj::Object(map) => map.pos(name).is_some_and(|slot| {
                !map.attrs[slot].accessor
                    && map.vals[slot].is_heap()
                    && matches!(
                        self.heap.get(map.vals[slot].heap_index()),
                        HeapObj::Native(id)
                            if native::proto_method(*id)
                                .is_some_and(|(method, actual_kind, _)| {
                                    method == name && actual_kind == kind
                                })
                    )
            }),
            _ => false,
        }
    }

    /// Does a primitive string's method Get resolve to the exact main-realm
    /// intrinsic for the two regex-heavy methods served by the direct lane?
    ///
    /// This is also a correctness gate for the ordinary name-dispatched string
    /// builtin path.  That path historically selected `replace`/`matchAll` from
    /// the receiver kind and method *name* alone, so overwriting, deleting, or
    /// accessorizing the live String prototype slot was silently ignored.  A
    /// false answer is a pure read-only prefix; callers fall through to the
    /// generic property Get, which observes the override.
    ///
    /// Slot attrs, Value bits, and the Native id are re-read on every call.  In
    /// particular, an in-place value overwrite need not bump an ObjMap version,
    /// so a version-only cache would be unsound.  The expected ids come from the
    /// native metadata table, never from hard-coded positional constants.
    pub(crate) fn string_regexp_method_is_intrinsic(&self, name: &str) -> bool {
        let Some(want) = native::string_regexp_proto_method_id(name) else {
            return false;
        };
        if self.str_proto == 0 || self.active_realm_proto(self.str_proto) != self.str_proto {
            // A primitive evaluated in a child realm resolves through that
            // realm's String prototype image.  Let the generic Get select and
            // call the realm-native method (or an override) there.
            return false;
        }
        match self.heap.get(self.str_proto) {
            HeapObj::Object(m) => m.pos(name).is_some_and(|slot| {
                !m.attrs[slot].accessor
                    && m.vals[slot].is_heap()
                    && matches!(
                        self.heap.get(m.vals[slot].heap_index()),
                        HeapObj::Native(id) if *id == want
                    )
            }),
            _ => false,
        }
    }

    /// Whether a primitive string case-conversion call resolves to the exact
    /// main-realm String prototype intrinsic.  Name + receiver-kind dispatch
    /// alone is unsound because these prototype slots are writable and may be
    /// deleted, accessorized, or replaced.  Child-realm code must likewise use
    /// that realm's prototype image through ordinary Get+Call.
    pub(crate) fn string_case_method_is_intrinsic(&self, name: &str) -> bool {
        if !matches!(name, "toUpperCase" | "toLowerCase")
            || self.str_proto == 0
            || self.active_realm_proto(self.str_proto) != self.str_proto
        {
            return false;
        }
        match self.heap.get(self.str_proto) {
            HeapObj::Object(map) => map.pos(name).is_some_and(|slot| {
                !map.attrs[slot].accessor
                    && map.vals[slot].is_heap()
                    && matches!(
                        self.heap.get(map.vals[slot].heap_index()),
                        HeapObj::Native(id)
                            if native::proto_method(*id)
                                .is_some_and(|(method, kind, _)| method == name && kind == 1)
                    )
            }),
            _ => false,
        }
    }

    /// Dispatch a builtin method on `recv` with an already-materialized args
    /// slice. Shared by `try_builtin_method` (args gathered from registers) and
    /// the spread method-call path (args taken from an array). `Ok(None)` means
    /// no builtin matched the receiver kind.
    ///
    /// This inline dispatch resolves a built-in by NAME + receiver kind, skipping
    /// the prototype walk that would otherwise reach a `$262.createRealm()`
    /// child's %X.prototype% IMAGE and its realm-copied native. When the
    /// receiver's [[Prototype]] belongs to a child realm the method found there
    /// IS that realm's built-in, so the objects it allocates must come from that
    /// realm's intrinsics (`g.a.map(f)` returns an array with `g.Array.prototype`
    /// — staging/sm/Array/species.js line 156). Re-establish that context here;
    /// `call_value` already does it for a realm native reached the slow way.
    /// One `is_empty` check until `$262.createRealm()` is called.
    pub(crate) fn dispatch_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        if !self.realm_global_objs.is_empty() && recv.is_heap() {
            let proto = self
                .proto_of
                .get(&recv.heap_index())
                .copied()
                .unwrap_or(Value::UNDEFINED);
            let r = self.get_function_realm(proto);
            if r != 0 && r != self.native_callee_realm.unwrap_or(0) {
                let prev = self.native_callee_realm;
                self.native_callee_realm = Some(r);
                let res = self.dispatch_builtin_method_inner(recv, name, args);
                self.native_callee_realm = prev;
                return res;
            }
        }
        self.dispatch_builtin_method_inner(recv, name, args)
    }

    fn dispatch_builtin_method_inner(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        builtin_stats_count(self, recv, name);
        // The reassignable prototype methods (`toString`/`valueOf`/
        // `toLocaleString`) must resolve through the prototype chain, not be
        // shadowed by the built-in type fast path. After e.g.
        // `Date.prototype.toString = Object.prototype.toString`, `d.toString()`
        // must use the override (a user function, or a different built-in). Defer
        // these three names to the caller's get_prop + call_value, which invokes
        // whatever is actually installed — for an unshadowed receiver that is the
        // same type native, just reached via one call_value instead of inline.
        if (recv.is_number() || recv.is_heap())
            && matches!(name, "toString" | "valueOf" | "toLocaleString" | "toJSON")
        {
            return Ok(None);
        }
        // Number receivers (Int or double) support a small method set.
        if recv.is_number() {
            return self.number_method(recv, name, args);
        }
        if !recv.is_heap() {
            return Ok(None);
        }
        let idx = recv.heap_index();
        // ── strings first ──
        // A string receiver reaches the same `string_method` call further down,
        // but only after a Temporal probe, `is_callable`, a realm lookup and a
        // Boxed probe — each its own heap load, none of which a string can ever
        // match (a string is not a number, a Temporal, callable, or a wrapper
        // object). The `toString`/`valueOf`/`toLocaleString`/`toJSON` deferral
        // above still runs first, so an overridden `String.prototype.toString` is
        // unaffected; this is purely the same dispatch reached sooner.
        //
        // Worth it because string method calls are everywhere and the fixed
        // dispatch cost dominated them: `s.indexOf(…)`, `s.slice(…)`,
        // `s.toUpperCase()` all measured ~70-95ns per call against node's ~3ns,
        // while `charCodeAt` and `length` — which have inline JIT fast paths and
        // never reach here — were already at parity.
        if matches!(self.heap.get(idx), HeapObj::Str(_) | HeapObj::Cons { .. }) {
            // Unlike most historical receiver-kind builtin arms, these two
            // names are explicitly override-safe.  Besides protecting the new
            // direct CallMethod prefix, this switchless gate repairs computed
            // calls and every generic fallback (`s["replace"](...)` included).
            if matches!(name, "matchAll" | "replace")
                && !self.string_regexp_method_is_intrinsic(name)
            {
                return Ok(None);
            }
            if matches!(name, "toUpperCase" | "toLowerCase")
                && !self.string_case_method_is_intrinsic(name)
            {
                return Ok(None);
            }
            return self.string_method(idx, name, args);
        }
        // ── RegExp `test` / `exec` ──
        // The terminal receiver-kind match below has arms for eleven heap kinds and
        // NONE for RegExp, so `re.test(s)` used to run this whole probe as dead work,
        // return `Ok(None)`, and then take the generic route: `get_prop(recv,"test")`
        // (whose fast path bails on the RegExp discriminant into `get_member_slow`'s
        // exotic preamble, ending in a `PropIndex` hash over %RegExp.prototype%'s 20
        // keys), a per-call `Vec<Value>`, and `call_value`. B68 measured the fixed
        // overhead of a successful `test` at 213ns against node's 133 and against
        // `indexOf`'s 53 — where `indexOf`, which has an arm, is at exact parity.
        //
        // Worse, it was a JIT cliff: in a region `jit_method_builtin_fallback` calls
        // `try_builtin_method`, got `Ok(None)` here, and returned `SELF_CALL_DEOPT`
        // without `osr_deopt_exempt` — so every `re.test()` exited the region AND was
        // counted as a deopt, and past `OSR_DEOPT_LIMIT` the region was evicted. A hot
        // loop containing `re.test()` could not stay compiled. Returning `Some` here
        // keeps it compiled.
        //
        // Guarded, unlike its siblings — see `regexp_method_is_intrinsic`. `test` also
        // needs `exec` to be intrinsic (RegExpExec reads it), and the natives already
        // check that and fall back correctly, so only the entry point is guarded here.
        if matches!(name, "test" | "exec") && matches!(self.heap.get(idx), HeapObj::RegExp { .. }) {
            let want = if name == "test" {
                native::REGEXP_TEST
            } else {
                native::REGEXP_EXEC
            };
            if self.regexp_method_is_intrinsic(idx, name, want) {
                let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                return self.call_native(want, recv, &[a0]).map(Some);
            }
        }
        // Temporal receivers route to their own dispatch (so valueOf throws and
        // toString gives the ISO string, not the generic Object behavior).
        if matches!(self.heap.get(idx), HeapObj::Temporal { .. }) {
            return self.temporal_method(idx, name, args);
        }
        // ── Function.prototype.call / apply / bind (callable receivers) ──
        // A createRealm-child function resolves these through its OWN realm's
        // %Function.prototype% copies (so e.g. apply's CreateListFromArrayLike
        // TypeError carries the child's constructor identity): skip the inline
        // fast path and let get_prop + call_value find the realm copy.
        if self.is_callable(recv)
            && !(!self.realm_global_objs.is_empty() && self.get_function_realm(recv) != 0)
        {
            match name {
                // `call`/`apply` are name-dispatched here, which is only sound
                // while the resolution a real Get would produce IS the pristine
                // `%Function.prototype%` native. An own `f.call = g` shadow, a
                // swapped [[Prototype]], a patched prototype slot, or an exotic
                // callable (Class statics, Proxy get traps) must resolve through
                // the generic get_prop + call_value tail below instead — B82's
                // fallback contract (`Ok(None)` here reaches that tail).
                "call" if self.fn_call_apply_pristine(recv, name) => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let rest: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                    return Ok(Some(self.call_value(recv, this, rest)?));
                }
                "apply" if self.fn_call_apply_pristine(recv, name) => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let arr = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                    // argArray null/undefined -> no args; else CreateListFromArrayLike
                    // (an array-like — `{length, 0, …}` — not necessarily iterable).
                    let callargs = if arr.is_nullish() {
                        Vec::new()
                    } else {
                        self.create_list_from_array_like(arr)?
                    };
                    return Ok(Some(self.call_value(recv, this, &callargs)?));
                }
                // Bind snapshots target length/name and may reject a sandbox-cap
                // overflow while composing "bound ". Keep that fallible work in
                // FN_BIND instead of duplicating an incomplete inline version.
                "bind" => return Ok(None),
                _ => {}
            }
        }
        // ── Boxed primitive: dispatch on the wrapped value (so new Number(5).
        // toFixed(), new String("x").charAt(), and valueOf/toString unwrap) — this
        // must precede the generic Object.prototype valueOf/toString below.
        if let HeapObj::Boxed { kind, value } = self.heap.get(idx) {
            let (k, v) = (*kind, *value);
            // The new direct lane deliberately accepts primitive Str/Cons
            // receivers only.  Keep boxed strings on generic property
            // resolution for these two names as well: an own shadow, a custom
            // [[Prototype]], or a live String.prototype override must win over
            // receiver-kind name dispatch.
            if k == 0 && matches!(name, "replace" | "matchAll") {
                return Ok(None);
            }
            return match k {
                // replace/replaceAll/split/match/search/matchAll delegate to a
                // searchValue's @@-method with O = the receiver (RequireObjectCoercible,
                // NOT ToString'd), so a boxed String must pass the box itself — not
                // its unwrapped [[StringData]] primitive. Other methods operate on
                // the primitive.
                0 if matches!(
                    name,
                    "replace" | "replaceAll" | "split" | "match" | "search" | "matchAll"
                ) =>
                {
                    Ok(Some(self.string_symbol_method(recv, name, args)?))
                }
                0 => self.string_method(v.heap_index(), name, args),
                1 => self.number_method(v, name, args),
                _ => match name {
                    "toString" | "valueOf" => Ok(Some(self.boolean_method(v, name)?)),
                    _ => Ok(None),
                },
            };
        }
        // ── Object.prototype methods (available on every object) ──
        match name {
            "hasOwnProperty" => {
                let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                if let Some(b) = self.has_own_index_fast(recv, a0) {
                    return Ok(Some(Value::bool(b)));
                }
                let key = self.to_property_key(a0)?;
                return Ok(Some(Value::bool(self.has_own_property(recv, &key))));
            }
            "propertyIsEnumerable" => {
                let key =
                    self.to_property_key(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                return Ok(Some(Value::bool(self.own_is_enumerable_dyn(recv, &key)?)));
            }
            "isPrototypeOf" => {
                let target = args.first().copied().unwrap_or(Value::UNDEFINED);
                return Ok(Some(Value::bool(self.is_prototype_of(recv, target))));
            }
            "valueOf" => {
                // Only the DEFAULT Object.prototype.valueOf (returns the receiver)
                // is handled inline. A custom own/inherited valueOf — or a
                // type-specific one (Date/Map/…) — must be invoked normally, so
                // defer (fall through) when `valueOf` does not resolve to the
                // generic intrinsic. (`obj.valueOf()` / `date.valueOf()` were
                // wrongly returning the object itself.)
                if self.method_is_generic(recv, "valueOf", native::PROTO_VALUE_OF)? {
                    return Ok(Some(recv));
                }
            }
            "toString" => {
                // Generic `Object.prototype.toString` for a plain object; arrays /
                // numbers / dates etc. have their own toString in the type dispatch.
                if matches!(self.heap.get(idx), HeapObj::Object(_)) {
                    // An error instance inherits Error.prototype.toString ("name: message").
                    if self.is_error_instance(idx) {
                        return self
                            .call_native(native::ERROR_TO_STRING, recv, args)
                            .map(Some);
                    }
                    // Defer to a custom own/inherited `toString` (user function or
                    // class method); only the generic intrinsic is handled inline
                    // (`obj.toString()` was ignoring an own toString).
                    if self.method_is_generic(recv, "toString", native::PROTO_TO_STRING)? {
                        // Honour a string `@@toStringTag` (`[object Cool]`), matching
                        // the Object.prototype.toString value form.
                        let tag = self.object_to_string_tag(recv)?;
                        let total = tag
                            .len()
                            .checked_add(9)
                            .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
                        let mut out = self.guest_string_with_capacity(total)?;
                        out.push_str("[object ");
                        out.push_str(&tag);
                        out.push(']');
                        return Ok(Some(self.alloc_str(out)));
                    }
                }
            }
            _ => {}
        }
        match self.heap.get(idx) {
            HeapObj::Array(_)
                if matches!(name, "slice" | "concat")
                    && !self.array_copy_method_is_intrinsic(idx, name) =>
            {
                Ok(None)
            }
            HeapObj::Array(_) => self.array_method(idx, name, args),
            HeapObj::Str(_) | HeapObj::Cons { .. } => self.string_method(idx, name, args),
            HeapObj::Map { .. } if self.collection_method_is_intrinsic(idx, name, 4) => {
                self.map_method(idx, name, args)
            }
            HeapObj::Set(_) if self.collection_method_is_intrinsic(idx, name, 3) => {
                self.set_method(idx, name, args)
            }
            HeapObj::Generator { .. } => self.generator_method(idx, name, args),
            HeapObj::AsyncGenerator(_) => Ok(self.async_generator_method(idx, name, args)),
            HeapObj::Promise { .. } => {
                // `then`/`catch`/`finally` must resolve through the receiver's
                // WHOLE prototype chain — an OWN shadow (`p.then = fn`), a
                // patched `Promise.prototype.then`, or a subclass override must
                // win over the intrinsic (tests observe those calls). Only when
                // the chain resolves to the matching kind-7 intrinsic native
                // does the inline path fire; otherwise defer to the caller's
                // get_prop + call_value so the override runs.
                //
                // B79: that proof used to be a full `get_prop(recv, name)` — a
                // Promise receiver misses `get_member`'s fast path on the heap
                // discriminant, so it took `get_member_slow`'s exotic preamble
                // and then walked the chain, on EVERY `.then()`. The pristine
                // probe below decides the same question from three cheap reads
                // (`proto_of` is a paged `SlotTable`, not a hash map), and it is
                // the guard shape B69 already uses for `re.test`/`re.exec`.
                // `async-promise-chain` makes 1,500,003 of these calls and
                // nothing else — 100% of its builtin dispatches.
                //
                // A subclass instance, an own shadow, or a patched
                // `Promise.prototype.then` all fail the probe and fall through
                // to the unchanged `get_prop` proof below, so the override
                // semantics the tests observe are decided by exactly the same
                // code as before.
                let is_intrinsic = if let Some(want) = native::promise_proto_method_id(name) {
                    self.promise_method_is_intrinsic(idx, name, want) || {
                        let m = self.get_prop(recv, name)?;
                        m.is_heap()
                            && matches!(self.heap.get(m.heap_index()),
                                        HeapObj::Native(id) if native::proto_method(*id)
                                            .is_some_and(|(n, k, _)| k == 7 && n == name))
                    }
                } else {
                    let m = self.get_prop(recv, name)?;
                    m.is_heap()
                        && matches!(self.heap.get(m.heap_index()),
                                    HeapObj::Native(id) if native::proto_method(*id)
                                        .is_some_and(|(n, k, _)| k == 7 && n == name))
                };
                if is_intrinsic {
                    self.promise_method(idx, name, args)
                } else {
                    Ok(None)
                }
            }
            HeapObj::Date(_) => self.date_method(idx, name, args),
            HeapObj::TypedArray { .. } => self.typed_array_method(idx, name, args),
            HeapObj::DataView { .. } => self.dataview_method(idx, name, args),
            HeapObj::ArrayBuffer { .. } => self.arraybuffer_method(idx, name, args),
            _ => Ok(None),
        }
    }

    /// True when `p.<name>` provably resolves to the intrinsic
    /// `Promise.prototype.<name>` native `want`, decided WITHOUT walking the
    /// prototype chain (B79). The `re.test`/`re.exec` guard
    /// (`regexp_method_is_intrinsic`) verbatim, over `promise_proto`:
    ///
    ///   * the receiver's `[[Prototype]]` is %Promise.prototype% — either
    ///     explicitly, or by absence, since `object_get_prototype_of` sends a
    ///     `HeapObj::Promise` with no `proto_of` entry there. A SUBCLASS
    ///     instance has an explicit entry naming the subclass prototype and so
    ///     fails here;
    ///   * the instance carries no OWN shadow of `name` (own properties of a
    ///     non-`Object` heap kind live in the `arr_props` side table); and
    ///   * %Promise.prototype% still holds `want` at that key as a plain data
    ///     property.
    ///
    /// A `false` costs nothing but the fallback proof the caller ran anyway, so
    /// this is only ever allowed to be conservative — never permissive.
    pub(crate) fn promise_method_is_intrinsic(&self, p: u32, name: &str, want: u16) -> bool {
        if !promise_pristine_enabled() {
            return false; // ZIPP_NO_PROMISE_PRISTINE=1 → the old get_prop proof
        }
        match self.proto_of.get(&p) {
            None => {}
            Some(pr) if pr.is_heap() && pr.heap_index() == self.promise_proto => {}
            _ => return false,
        }
        if self
            .arr_props
            .get(&p)
            .is_some_and(|m| m.pos(name).is_some())
        {
            return false;
        }
        match self.heap.get(self.promise_proto) {
            HeapObj::Object(m) => m.pos(name).is_some_and(|i| {
                !m.attrs[i].accessor
                    && m.vals[i].is_heap()
                    && matches!(self.heap.get(m.vals[i].heap_index()),
                                HeapObj::Native(n) if *n == want)
            }),
            _ => false,
        }
    }

    /// True when `recv.name` resolves (own → prototype chain) to the generic
    /// intrinsic `generic_id` (`Object.prototype.toString`/`valueOf`), i.e. there
    /// is NO custom override. Used so the inline fast path only fires for the
    /// default method and a custom `toString`/`valueOf` is actually invoked.
    /// Resolving a method does not invoke it, so this has no observable effect.
    fn method_is_generic(
        &mut self,
        recv: Value,
        name: &str,
        generic_id: u16,
    ) -> Result<bool, Thrown> {
        let m = self.get_prop(recv, name)?;
        Ok(m.is_heap()
            && matches!(self.heap.get(m.heap_index()), HeapObj::Native(id) if *id == generic_id))
    }

    /// Infallible ToNumber (Symbol/etc. → NaN) — for index/length args in the
    /// TypedArray/DataView methods, where a closure can't propagate `?`.
    pub(crate) fn value_num(&self, v: Value) -> f64 {
        self.to_number(v).unwrap_or(f64::NAN)
    }

    /// `Object.prototype.toString`'s tag: the builtin tag (Array/Function/Error/…),
    /// overridden by a string `@@toStringTag` if present. (`[object <tag>]`.)
    pub(crate) fn object_to_string_tag(&mut self, this: Value) -> Result<String, Thrown> {
        if this.is_undefined() {
            return Ok("Undefined".to_string());
        }
        if this.is_null() {
            return Ok("Null".to_string());
        }
        // A callable object that isn't one of the explicit function heap variants —
        // notably a built-in constructor global (Array/Map/Temporal.PlainDate/…),
        // which zipp stores as an `is_ctor` HeapObj::Object — is still "Function".
        // [[Call]] pierces a proxy chain (a proxy over a callable IS callable);
        // the builtinTag then flows through the @@toStringTag override below,
        // so a proxied ASYNC function still tags "AsyncFunction" via the Get.
        let callable = this.is_heap()
            && (self.is_callable(this) || {
                let mut t = this;
                while t.is_heap() {
                    match self.proxy_parts(t.heap_index()) {
                        Some((t2, _, _)) => t = t2,
                        None => break,
                    }
                }
                t.is_heap() && t != this && self.is_callable(t)
            });
        // Step 4 IsArray(O) pierces proxy targets (revoked throws) — "Array".
        // It picks the builtinTag and nothing more: returning here skipped step
        // 15's `Get(O, @@toStringTag)`, which is UNCONDITIONAL and observable
        // through the proxy's `get` trap.
        let proxy_array = this.is_heap()
            && self.proxy_parts(this.heap_index()).is_some()
            && self.value_is_array_throwing(this)?;
        let builtin = if proxy_array {
            "Array"
        } else if this.is_heap() {
            match self.heap.get(this.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => "String",
                // An `arguments` exotic ([[ParameterMap]]) tags "Arguments" even
                // though it is Array-backed internally.
                HeapObj::Array(_) if self.arguments_objs.contains_key(&this.heap_index()) => {
                    "Arguments"
                }
                HeapObj::Array(_) => "Array",
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Native(_)
                | HeapObj::Bound { .. } => "Function",
                HeapObj::Boxed { kind: 0, .. } => "String",
                HeapObj::Boxed { kind: 1, .. } => "Number",
                HeapObj::Boxed { kind: 2, .. } => "Boolean",
                // Date/RegExp have built-in tags ([[DateValue]]/[[RegExpMatcher]]);
                // Map/Set/Promise/etc instead carry a @@toStringTag (handled below).
                HeapObj::Date(_) => "Date",
                HeapObj::RegExp { .. } => "RegExp",
                // The built-in prototype objects are themselves exotics of their
                // type (Number.prototype is a Number, String.prototype a String,
                // …), so Object.prototype.toString tags them by that type. zipp
                // allocates them as plain HeapObj::Object, so match their heap idx.
                _ if self.num_proto != 0 && this.heap_index() == self.num_proto => "Number",
                _ if self.str_proto != 0 && this.heap_index() == self.str_proto => "String",
                _ if self.bool_proto != 0 && this.heap_index() == self.bool_proto => "Boolean",
                _ if self.arr_proto != 0 && this.heap_index() == self.arr_proto => "Array",
                // [[ErrorData]] ⇒ "Error". An error INSTANCE carries an own
                // error-name; the Error/NativeError PROTOTYPES also have a `name`
                // property but NO [[ErrorData]], so exclude them (they tag "Object").
                _ if self.error_name(this.heap_index()).is_some()
                    && !self.error_protos.contains(&this.heap_index()) =>
                {
                    "Error"
                }
                _ if callable => "Function",
                _ => "Object",
            }
        } else if this.is_number() {
            "Number"
        } else if this.is_bool() {
            "Boolean"
        } else {
            "Object"
        };
        // A string @@toStringTag overrides the builtin tag. Step 15 reads it from
        // ToObject(this), so a primitive number/boolean consults its wrapper
        // prototype (`Boolean.prototype[Symbol.toStringTag] = 'x'` retags `true`).
        // String/Symbol/BigInt primitives are heap values here and walk their
        // prototype chain through get_prop directly.
        let tag_src = if this.is_heap() {
            Some(this)
        } else if this.is_number() && self.num_proto != 0 {
            Some(Value::heap(self.num_proto))
        } else if this.is_bool() && self.bool_proto != 0 {
            Some(Value::heap(self.bool_proto))
        } else {
            None
        };
        if let Some(src) = tag_src {
            let tag = self.get_prop(src, "@@toStringTag")?;
            if tag.is_heap() && self.heap.is_str_like(tag.heap_index()) {
                return Ok(self.display(tag));
            }
        }
        Ok(builtin.to_string())
    }

    pub(crate) fn ta_len_kind(&self, idx: u32) -> (usize, u8) {
        match self.heap.get(idx) {
            // Effective length (0 if out of bounds on a resized buffer).
            HeapObj::TypedArray { kind, .. } => (self.ta_effective_len(idx).unwrap_or(0), *kind),
            _ => (0, 0),
        }
    }

    /// IsValidIntegerIndex for a TypedArray: `key` is the canonical string of a
    /// non-negative integer `n` (so "0".."N", rejecting "01"/"-0"/"1.5"/"x") and
    /// `0 <= n < length`. Returns `Some(n)` for an in-bounds integer index — the
    /// integer-indexed exotic own properties — else `None`. (`idx` must be a
    /// TypedArray; a non-TA has length 0 so every key returns `None`.)
    pub(crate) fn ta_valid_index(&self, idx: u32, key: &str) -> Option<usize> {
        let n: usize = key.parse().ok()?;
        if n.to_string() != key {
            return None;
        }
        (n < self.ta_len_kind(idx).0).then_some(n)
    }

    /// CanonicalNumericIndexString(key) (ES 7.1.21): `key` is "-0", or the canonical
    /// `Number→String` form of a numeric value (so it round-trips: "0","1","1.5",
    /// "NaN","Infinity" yes; "01","1.0","foo" no). Such a key is ABSORBED by a
    /// TypedArray's integer-indexed exotic methods — HasProperty / DefineOwnProperty
    /// never consult the prototype or named props for it.
    pub(crate) fn is_canonical_numeric_index(&self, key: &str) -> bool {
        if key == "-0" {
            return true;
        }
        match key.parse::<f64>() {
            Ok(n) => crate::vm::helpers_num2::fmt_f64(n) == key,
            Err(_) => false,
        }
    }
    /// Snapshot a TypedArray's elements as Values (numbers / BigInts).
    pub(crate) fn ta_snapshot(&mut self, idx: u32) -> Vec<Value> {
        let len = self.ta_len_kind(idx).0;
        (0..len).map(|i| self.ta_element_get(idx, i)).collect()
    }
    /// Build a fresh TypedArray of `kind` from element Values (coerced/encoded).
    pub(crate) fn ta_build_from(&mut self, kind: u8, vals: &[Value]) -> Result<Value, Thrown> {
        let size = native::TA_KINDS[kind as usize].1;
        let buf = self.alloc_array_buffer(vals.len() * size)?;
        let ta = self.alloc_typed_array(buf, kind, 0, vals.len());
        for (i, v) in vals.iter().enumerate() {
            self.ta_element_set(ta.heap_index(), i, *v)?;
        }
        Ok(ta)
    }

    /// TypedArraySpeciesCreate(exemplar, «count»): the result TypedArray that
    /// slice/map/filter/etc. build. SpeciesConstructor reads exemplar.constructor
    /// then its [[Symbol.species]] — undefined/null (or no constructor) yields a
    /// default zero-filled view of the exemplar's kind; otherwise the species must
    /// be a constructor, Construct(species,«count») must return a TypedArray, and
    /// its length must be >= count. The caller writes the elements afterwards.
    pub(crate) fn ta_species_create(
        &mut self,
        exemplar_idx: u32,
        count: usize,
    ) -> Result<Value, Thrown> {
        let (_, kind) = self.ta_len_kind(exemplar_idx);
        let ctor = self.get_prop(Value::heap(exemplar_idx), "constructor")?;
        let species = if ctor == Value::UNDEFINED {
            Value::UNDEFINED
        } else if !self.is_object_value(ctor) {
            return Err(Thrown(
                "TypeError: constructor property is not an object".into(),
            ));
        } else {
            let s = self.get_prop(ctor, "@@species")?;
            if s == Value::NULL {
                Value::UNDEFINED
            } else {
                s
            }
        };
        if species == Value::UNDEFINED {
            // Default constructor: a fresh zero-filled view of the exemplar's kind.
            let size = native::TA_KINDS[kind as usize].1;
            let buf = self.alloc_array_buffer(count * size)?;
            return Ok(self.alloc_typed_array(buf, kind, 0, count));
        }
        if !self.is_constructor(species) {
            return Err(Thrown(
                "TypeError: TypedArray [Symbol.species] is not a constructor".into(),
            ));
        }
        let result = self.construct(species, &[Value::num(count as f64)])?;
        // ValidateTypedArray in write access mode: the result must be a
        // TypedArray, not backed by an immutable buffer, not detached/out of
        // bounds (its EFFECTIVE length decides - a length-tracking view over a
        // resizable buffer counts what it currently sees), and long enough.
        let ridx = match result.is_heap().then(|| self.heap.get(result.heap_index())) {
            Some(HeapObj::TypedArray { buffer, .. }) => {
                let b = *buffer;
                if self.immutable_buffers.contains(&b) {
                    return Err(Thrown(
                        "TypeError: species-created TypedArray is backed by an immutable ArrayBuffer".into(),
                    ));
                }
                result.heap_index()
            }
            _ => {
                return Err(Thrown(
                    "TypeError: TypedArray [Symbol.species] did not return a TypedArray".into(),
                ))
            }
        };
        match self.ta_effective_len(ridx) {
            None => {
                return Err(Thrown(
                    "TypeError: species-created TypedArray is detached or out of bounds".into(),
                ))
            }
            Some(eff) if eff < count => {
                return Err(Thrown(
                    "TypeError: species-created TypedArray is shorter than required".into(),
                ))
            }
            _ => {}
        }
        Ok(result)
    }

    /// `%TypedArray%.prototype` methods (most mirror Array.prototype, but map/filter/
    /// slice/etc. return TypedArrays and `sort` is numeric by default). `idx` is the
    /// receiver TypedArray's heap index.
    /// Resolve a TypedArray relative index argument (negative counts from the
    /// end) into [0,len], via ToInteger — which throws on a Symbol and honours a
    /// valueOf (abrupt completion); `undefined` yields `def`.
    fn ta_rel_index(&mut self, v: Value, def: usize, len: usize) -> Result<usize, Thrown> {
        if v == Value::UNDEFINED {
            return Ok(def);
        }
        let n = self.to_integer_or_zero(v)?;
        Ok(if n < 0 {
            ((len as i64) + n).max(0) as usize
        } else {
            (n as usize).min(len)
        })
    }

    pub(crate) fn typed_array_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        if !matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            return Ok(None);
        }
        // ValidateTypedArray: nearly every TypedArray prototype method throws a
        // TypeError when the view is out of bounds — a detached buffer, or (on a
        // resizable buffer that shrank) an offset/length that no longer fits.
        // subarray is the one exception (it just builds another view). `set` is a
        // second: 23.2.3.26 does only RequireInternalSlot on entry and defers the
        // out-of-bounds check to SetTypedArrayFrom*, i.e. AFTER
        // ToIntegerOrInfinity(offset) — so `detachedTA.set(null, {valueOf(){throw}})`
        // must surface the valueOf's throw, not a TypeError. Its arm re-checks.
        // The Uint8Array base64/hex methods are also excluded: they aren't handled
        // here (they fall through to their prototype Natives) and have their own
        // spec-ordered checks — e.g. toBase64 reads its options object BEFORE
        // observing detachedness, so the blanket check must not preempt that.
        if !matches!(
            name,
            "subarray" | "set" | "toHex" | "setFromHex" | "toBase64" | "setFromBase64"
        ) && self.ta_effective_len(idx).is_none()
        {
            return Err(Thrown(format!(
                "TypeError: Cannot perform {name} on an out-of-bounds or detached TypedArray"
            )));
        }
        // A mutating method rejects an IMMUTABLE backing buffer (the ES2025
        // immutable-arraybuffer write-access-mode check) BEFORE any argument
        // coercion, comparator call, or element read — even for a length-0 array.
        if matches!(name, "fill" | "copyWithin" | "sort" | "reverse" | "set") {
            let buffer = match self.heap.get(idx) {
                HeapObj::TypedArray { buffer, .. } => *buffer,
                _ => 0,
            };
            if self.immutable_buffers.contains(&buffer) {
                return Err(Thrown(format!(
                    "TypeError: Cannot {name} a TypedArray backed by an immutable ArrayBuffer"
                )));
            }
        }
        let (len, kind) = self.ta_len_kind(idx);
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        match name {
            "at" => {
                // ToInteger(index): throws on a Symbol, honours a valueOf.
                let n = self.to_integer_or_zero(a0)?;
                let i = if n < 0 { len as i64 + n } else { n };
                Ok(Some(if i >= 0 && (i as usize) < len {
                    self.ta_element_get(idx, i as usize)
                } else {
                    Value::UNDEFINED
                }))
            }
            "join" => {
                let sep = if a0 == Value::UNDEFINED {
                    ",".to_string()
                } else {
                    self.to_js_string(a0)?
                };
                self.preflight_native_iteration_work(len as u64)?;
                // The element COUNT is fixed at entry; a detach (or resizable shrink)
                // during separator ToString makes each now-out-of-range element read
                // as "" (Get → undefined → ""), so e.g. a detached length-3 array
                // joins to ",,".
                let eff = self.ta_effective_len(idx).unwrap_or(0);
                let mut out = String::new();
                for i in 0..len {
                    let part = if i < eff {
                        self.ta_elem_string(idx, i)
                    } else {
                        String::new()
                    };
                    self.append_guest_join_part(&mut out, &sep, &part, i)?;
                }
                Ok(Some(self.alloc_str(out)))
            }
            "toString" => {
                self.preflight_native_iteration_work(len as u64)?;
                let mut out = String::new();
                for i in 0..len {
                    let part = self.ta_elem_string(idx, i);
                    self.append_guest_join_part(&mut out, ",", &part, i)?;
                }
                Ok(Some(self.alloc_str(out)))
            }
            "toLocaleString" => {
                // ToString(Invoke(element, "toLocaleString")) for each element,
                // joined by ",". Unlike toString this calls the element's own
                // toLocaleString and uses a real ToString, so a throwing
                // toLocaleString / toString / valueOf propagates as an abrupt
                // completion (the elements are numbers/bigints, never nullish).
                self.preflight_native_iteration_work(len as u64)?;
                let mut out = String::new();
                for i in 0..len {
                    let el = self.ta_element_get(idx, i);
                    // A user toLocaleString may shrink the buffer: later reads come
                    // back undefined, which joins as the empty string per spec.
                    if el == Value::UNDEFINED || el == Value::NULL {
                        self.append_guest_join_part(&mut out, ",", "", i)?;
                        continue;
                    }
                    let f = self.get_prop(el, "toLocaleString")?;
                    let s = if self.is_callable(f) {
                        // ECMA-402 forwards (locales, options) to each element.
                        let fwd = [
                            args.first().copied().unwrap_or(Value::UNDEFINED),
                            args.get(1).copied().unwrap_or(Value::UNDEFINED),
                        ];
                        let r = self.call_value(f, el, &fwd)?;
                        self.to_js_string(r)?
                    } else {
                        return Err(Thrown(
                            "TypeError: element toLocaleString is not callable".into(),
                        ));
                    };
                    self.append_guest_join_part(&mut out, ",", &s, i)?;
                }
                Ok(Some(self.alloc_str(out)))
            }
            "indexOf" | "lastIndexOf" | "includes" => {
                // Length is fixed at method entry (ValidateTypedArray already ran).
                // An empty array returns the not-found result BEFORE coercing
                // fromIndex, so a fromIndex valueOf never runs on an empty array.
                let entry_len = self.ta_effective_len(idx).unwrap_or(0) as i64;
                if entry_len == 0 {
                    return Ok(Some(if name == "includes" {
                        Value::bool(false)
                    } else {
                        Value::num(-1.0)
                    }));
                }
                // fromIndex (ToInteger) may run a valueOf that detaches the buffer.
                // lastIndexOf defaults to len-1; indexOf/includes default to 0.
                let from = if args.len() >= 2 {
                    self.to_integer_or_zero(a1)?
                } else if name == "lastIndexOf" {
                    entry_len - 1
                } else {
                    0
                };
                // The loop is bounded by the ENTRY length with FRESH per-index
                // reads: a coercion that shrank/detached the buffer makes the now
                // out-of-range indices read undefined (Get semantics) — includes
                // can still match a searched undefined, while indexOf/lastIndexOf
                // skip them (HasProperty is false); a grow leaves the entry bound.
                let mut found: i64 = -1;
                if name == "lastIndexOf" {
                    let hi = if from < 0 {
                        entry_len + from
                    } else {
                        from.min(entry_len - 1)
                    };
                    if hi >= 0 {
                        self.preflight_native_iteration_work((hi as u64).saturating_add(1))?;
                        for i in (0..=hi as usize).rev() {
                            let e = self.ta_element_get(idx, i);
                            if e == Value::UNDEFINED {
                                continue;
                            }
                            if self.values_strict_eq(e, a0) {
                                found = i as i64;
                                break;
                            }
                        }
                    }
                } else {
                    let lo = if from < 0 {
                        (entry_len + from).max(0)
                    } else {
                        from.min(entry_len)
                    } as usize;
                    self.preflight_native_iteration_work(
                        (entry_len as u64).saturating_sub(lo as u64),
                    )?;
                    for i in lo..entry_len as usize {
                        let e = self.ta_element_get(idx, i);
                        let eq = if name == "includes" {
                            self.same_value_zero(e, a0)
                        } else {
                            e != Value::UNDEFINED && self.values_strict_eq(e, a0)
                        };
                        if eq {
                            found = i as i64;
                            break;
                        }
                    }
                }
                Ok(Some(if name == "includes" {
                    Value::bool(found >= 0)
                } else {
                    Value::num(found as f64)
                }))
            }
            "map" => {
                if !self.is_callable(a0) {
                    return Err(Thrown("TypeError: map callback is not a function".into()));
                }
                self.preflight_native_iteration_work(len as u64)?;
                // TypedArraySpeciesCreate runs FIRST (its user code can resize or
                // detach buffers), then each element is re-Get, mapped, and Set
                // into the destination (per-element [[Set]] semantics: coercion
                // throws propagate; an out-of-bounds destination write no-ops).
                // The destination is held across user callbacks: guard the GC.
                let _gc = self.gc_lock_guard();
                let dest = self.ta_species_create(idx, len)?;
                for i in 0..len {
                    let e = self.ta_element_get(idx, i);
                    let r = self.call_value(a0, a1, &[e, Value::num(i as f64), recv])?;
                    self.ta_element_set(dest.heap_index(), i, r)?;
                }
                Ok(Some(dest))
            }
            "forEach" | "filter" | "find" | "findIndex" | "findLast" | "findLastIndex"
            | "every" | "some" => {
                if !self.is_callable(a0) {
                    return Err(Thrown(format!(
                        "TypeError: {name} callback is not a function"
                    )));
                }
                self.preflight_native_iteration_work(len as u64)?;
                // `mapped` holds Values across user callbacks: guard the GC.
                let _gc = self.gc_lock_guard();
                let mut mapped: Vec<Value> = Vec::new();
                let order: Vec<usize> = if name == "findLast" || name == "findLastIndex" {
                    (0..len).rev().collect()
                } else {
                    (0..len).collect()
                };
                for &i in &order {
                    // Read each element fresh (the spec re-Gets per iteration, so a
                    // callback that mutates the TypedArray is observed — values are
                    // not cached before iteration).
                    let e = self.ta_element_get(idx, i);
                    let r = self.call_value(a0, a1, &[e, Value::num(i as f64), recv])?;
                    match name {
                        "forEach" => {}
                        "filter" => {
                            if self.truthy(r) {
                                mapped.push(e);
                            }
                        }
                        "find" => {
                            if self.truthy(r) {
                                return Ok(Some(e));
                            }
                        }
                        "findLast" => {
                            if self.truthy(r) {
                                return Ok(Some(e));
                            }
                        }
                        "findIndex" | "findLastIndex" => {
                            if self.truthy(r) {
                                return Ok(Some(Value::num(i as f64)));
                            }
                        }
                        "every" => {
                            if !self.truthy(r) {
                                return Ok(Some(Value::bool(false)));
                            }
                        }
                        "some" => {
                            if self.truthy(r) {
                                return Ok(Some(Value::bool(true)));
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Some(match name {
                    "filter" => {
                        // Result via TypedArraySpeciesCreate (constructor[@@species]),
                        // AFTER the callbacks (filter captures the kept count first).
                        let dest = self.ta_species_create(idx, mapped.len())?;
                        for (i, v) in mapped.iter().enumerate() {
                            self.ta_element_set(dest.heap_index(), i, *v)?;
                        }
                        dest
                    }
                    "find" | "findLast" => Value::UNDEFINED,
                    "findIndex" | "findLastIndex" => Value::num(-1.0),
                    "every" => Value::bool(true),
                    "some" => Value::bool(false),
                    _ => Value::UNDEFINED, // forEach
                }))
            }
            "reduce" | "reduceRight" => {
                if !self.is_callable(a0) {
                    return Err(Thrown(format!(
                        "TypeError: {name} callback is not a function"
                    )));
                }
                self.preflight_native_iteration_work(len as u64)?;
                let order: Vec<usize> = if name == "reduceRight" {
                    (0..len).rev().collect()
                } else {
                    (0..len).collect()
                };
                let mut acc;
                let mut start = 0;
                if args.len() >= 2 {
                    acc = a1;
                } else {
                    if order.is_empty() {
                        return Err(Thrown(
                            "TypeError: Reduce of empty array with no initial value".into(),
                        ));
                    }
                    acc = self.ta_element_get(idx, order[0]);
                    start = 1;
                }
                for &i in &order[start..] {
                    // Read each element fresh (not cached) per the spec.
                    let e = self.ta_element_get(idx, i);
                    acc = self.call_value(
                        a0,
                        Value::UNDEFINED,
                        &[acc, e, Value::num(i as f64), recv],
                    )?;
                }
                Ok(Some(acc))
            }
            "fill" => {
                // ToBigInt/ToNumber(value) runs ONCE and FIRST — its valueOf before
                // the start/end ToInteger coercions, per spec (so the value is
                // coerced a single time and in the right order). The coerced Value
                // is built after the index coercions so it needs no GC rooting.
                let is_big = native::TA_KINDS[kind as usize].2;
                let big = if is_big {
                    Some(self.to_bigint(a0)?)
                } else {
                    None
                };
                let num = if is_big {
                    0.0
                } else {
                    self.to_number_coerce(a0)?
                };
                let start = self.ta_rel_index(a1, 0, len)?;
                let end =
                    self.ta_rel_index(args.get(2).copied().unwrap_or(Value::UNDEFINED), len, len)?;
                let v = if let Some(b) = big {
                    self.make_bigint_val(b)
                } else {
                    Value::num(num)
                };
                // The value/start/end coercions above may have run user code (a
                // valueOf / @@toPrimitive) that detached the buffer — re-check before
                // writing (spec step: a detached buffer here is a TypeError).
                if self.ta_effective_len(idx).is_none() {
                    return Err(Thrown(
                        "TypeError: Cannot fill a detached or out-of-bounds TypedArray".into(),
                    ));
                }
                self.preflight_native_iteration_work(end.saturating_sub(start) as u64)?;
                for i in start..end {
                    self.ta_element_set(idx, i, v)?;
                }
                Ok(Some(recv))
            }
            "reverse" => {
                self.preflight_native_iteration_work(len as u64)?;
                let mut snap = self.ta_snapshot(idx);
                snap.reverse();
                for (i, v) in snap.into_iter().enumerate() {
                    self.ta_element_set(idx, i, v)?;
                }
                Ok(Some(recv))
            }
            // ES2023 change-array-by-copy: build a NEW typed array of the same kind.
            "toReversed" => {
                self.preflight_native_iteration_work(len as u64)?;
                let mut snap = self.ta_snapshot(idx);
                snap.reverse();
                Ok(Some(self.ta_build_from(kind, &snap)?))
            }
            "toSorted" => {
                let cmp = a0;
                if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                    return Err(Thrown(
                        "TypeError: the comparator argument must be a function or undefined".into(),
                    ));
                }
                self.preflight_native_iteration_work(typed_array_sort_work_bound(
                    len,
                    self.is_callable(cmp),
                ))?;
                let mut snap = self.ta_snapshot(idx);
                if self.is_callable(cmp) {
                    let n = snap.len();
                    for i in 1..n {
                        let mut j = i;
                        while j > 0 {
                            let r =
                                self.call_value(cmp, Value::UNDEFINED, &[snap[j - 1], snap[j]])?;
                            // ToNumber on the comparator result (observable on
                            // objects; abrupt propagates; NaN acts as +0).
                            if self.to_number_coerce(r)? > 0.0 {
                                snap.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                    }
                } else {
                    // Default TypedArray sort: ascending with -0 before +0
                    // (total_cmp) and ALL NaNs last regardless of their sign bit.
                    snap.sort_by(|a, b| {
                        let (x, y) = (self.value_num(*a), self.value_num(*b));
                        match (x.is_nan(), y.is_nan()) {
                            (true, true) => std::cmp::Ordering::Equal,
                            (true, false) => std::cmp::Ordering::Greater,
                            (false, true) => std::cmp::Ordering::Less,
                            (false, false) => x.total_cmp(&y),
                        }
                    });
                }
                Ok(Some(self.ta_build_from(kind, &snap)?))
            }
            "with" => {
                // %TypedArray%.prototype.with(index, value) (23.2.3.36): a copy with
                // one element replaced. Spec coercion ORDER, all BEFORE the range
                // check: (1) ToIntegerOrInfinity(index) — its valueOf, and it THROWS;
                // (2) ToNumber/ToBigInt(value) — its valueOf, and it THROWS (so a
                // throwing value surfaces before a RangeError). (Previously the index
                // used the non-coercing value_num and the range check ran first.)
                let relative = self.to_number_coerce(a0)?;
                let relative = if relative.is_nan() {
                    0.0
                } else {
                    relative.trunc()
                };
                let actual = if relative < 0.0 {
                    len as f64 + relative
                } else {
                    relative
                };
                let is_big = native::TA_KINDS[kind as usize].2;
                let coerced = if is_big {
                    // ToBigInt (strict, as ta_element_set): a Number is a TypeError
                    // (unlike the lenient BigInt(5) constructor coercion).
                    if a1.is_number() {
                        return Err(Thrown(
                            "TypeError: cannot convert a Number to a BigInt typed-array element"
                                .into(),
                        ));
                    }
                    let big = self.to_bigint(a1)?;
                    self.make_bigint_val(big)
                } else {
                    let num = self.to_number_coerce(a1)?;
                    Value::num(num)
                };
                // The range check runs AFTER both coercions and against the
                // CURRENT length (a coercion may have resized the buffer)...
                let cur = self.ta_effective_len(idx).unwrap_or(0);
                if actual < 0.0 || actual >= cur as f64 {
                    return Err(Thrown("RangeError: invalid typed array index".into()));
                }
                self.preflight_native_iteration_work(len as u64)?;
                // ...but the result has exactly the ENTRY length: re-Get each
                // element (shrunk indices read undefined -> 0/0n via Set), and the
                // replacement is silently skipped when its now-valid index lies
                // beyond the entry length.
                let mut snap: Vec<Value> = (0..len).map(|i| self.ta_element_get(idx, i)).collect();
                if let Some(slot) = snap.get_mut(actual as usize) {
                    *slot = coerced;
                }
                Ok(Some(self.ta_build_from(kind, &snap)?))
            }
            "slice" => {
                let start = self.ta_rel_index(a0, 0, len)?;
                let end = self.ta_rel_index(a1, len, len)?;
                let count = end.max(start) - start;
                self.preflight_native_iteration_work(count as u64)?;
                // TypedArraySpeciesCreate (constructor[@@species]) FIRST — it can run
                // user code that detaches the source buffer.
                let dest = self.ta_species_create(idx, count)?;
                // Per spec %TypedArray%.prototype.slice: when count > 0, re-check
                // IsDetachedBuffer(O) AFTER the species create and throw TypeError.
                // ta_effective_len returns None for a detached / out-of-bounds source.
                if count > 0 && self.ta_effective_len(idx).is_none() {
                    return Err(Thrown(
                        "TypeError: Cannot slice a TypedArray backed by a detached buffer".into(),
                    ));
                }
                // The species create may have SHRUNK the source (resizable buffer):
                // copy only what it can still see; the destination tail keeps its
                // constructor zeros.
                let avail = self.ta_effective_len(idx).unwrap_or(0);
                let copy = count.min(avail.saturating_sub(start));
                // Same element type: a BYTE copy, per spec — and observably so,
                // since a float NaN payload does not survive a round trip
                // through an f64 `Value`. Falls back per-element when the raw
                // copy does not apply (different kinds, shared storage, …).
                if !self.ta_raw_copy(idx, start, dest.heap_index(), copy) {
                    for i in 0..copy {
                        let v = self.ta_element_get(idx, start + i);
                        self.ta_element_set(dest.heap_index(), i, v)?;
                    }
                }
                Ok(Some(dest))
            }
            "subarray" => {
                let start = self.ta_rel_index(a0, 0, len)?;
                let end = self.ta_rel_index(a1, len, len)?;
                let (buffer, byte_offset) = match self.heap.get(idx) {
                    HeapObj::TypedArray {
                        buffer,
                        byte_offset,
                        ..
                    } => (*buffer, *byte_offset),
                    _ => return Ok(None),
                };
                let size = native::TA_KINDS[kind as usize].1;
                let new_len = end.saturating_sub(start);
                let new_offset = byte_offset + start * size;
                // TypedArraySpeciesCreate(O, «buffer, beginByteOffset, newLength»):
                // a custom constructor[@@species] builds the sub-view.
                let ctor = self.get_prop(recv, "constructor")?;
                let species = if ctor == Value::UNDEFINED {
                    Value::UNDEFINED
                } else if !self.is_object_value(ctor) {
                    return Err(Thrown(
                        "TypeError: constructor property is not an object".into(),
                    ));
                } else {
                    let s = self.get_prop(ctor, "@@species")?;
                    if s == Value::NULL {
                        Value::UNDEFINED
                    } else {
                        s
                    }
                };
                if species == Value::UNDEFINED {
                    // The default TypedArrayCreate runs the buffer constructor, which
                    // throws on a detached buffer — mirror that for the fast path.
                    if matches!(
                        self.heap.get(buffer),
                        HeapObj::ArrayBuffer { detached: true, .. }
                    ) {
                        return Err(Thrown(
                            "TypeError: Cannot create a subarray view of a detached ArrayBuffer"
                                .into(),
                        ));
                    }
                    let result = self.alloc_typed_array(buffer, kind, new_offset, new_len);
                    // A subarray of a length-tracking view with no explicit `end` is
                    // itself length-tracking (newLength stays auto), so it grows/shrinks
                    // with the resizable buffer rather than snapshotting the length.
                    if a1 == Value::UNDEFINED && self.ta_tracking.contains(&idx) {
                        self.ta_tracking.insert(result.heap_index());
                    }
                    return Ok(Some(result));
                }
                if !self.is_constructor(species) {
                    return Err(Thrown(
                        "TypeError: TypedArray [Symbol.species] is not a constructor".into(),
                    ));
                }
                // TypedArraySpeciesCreate: a length-tracking source with no explicit
                // `end` passes NO newLength (it stays auto), so the species view tracks
                // the resizable buffer instead of snapshotting the current length.
                let result = if a1 == Value::UNDEFINED && self.ta_tracking.contains(&idx) {
                    self.construct(
                        species,
                        &[Value::heap(buffer), Value::num(new_offset as f64)],
                    )?
                } else {
                    self.construct(
                        species,
                        &[
                            Value::heap(buffer),
                            Value::num(new_offset as f64),
                            Value::num(new_len as f64),
                        ],
                    )?
                };
                if !matches!(
                    self.heap.get(result.heap_index()),
                    HeapObj::TypedArray { .. }
                ) {
                    return Err(Thrown(
                        "TypeError: TypedArray [Symbol.species] did not return a TypedArray".into(),
                    ));
                }
                Ok(Some(result))
            }
            "sort" => {
                let cmp = a0;
                if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                    return Err(Thrown(
                        "TypeError: the comparator argument must be a function or undefined".into(),
                    ));
                }
                self.preflight_native_iteration_work(typed_array_sort_work_bound(
                    len,
                    self.is_callable(cmp),
                ))?;
                let mut snap = self.ta_snapshot(idx);
                if self.is_callable(cmp) {
                    // Comparator sort (stable insertion to allow VM re-entry).
                    let n = snap.len();
                    for i in 1..n {
                        let mut j = i;
                        while j > 0 {
                            let r =
                                self.call_value(cmp, Value::UNDEFINED, &[snap[j - 1], snap[j]])?;
                            // ToNumber on the comparator result (observable on
                            // objects; abrupt propagates; NaN acts as +0).
                            if self.to_number_coerce(r)? > 0.0 {
                                snap.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                    }
                } else {
                    // Default TypedArray sort: ascending with -0 before +0
                    // (total_cmp) and ALL NaNs last regardless of their sign bit.
                    snap.sort_by(|a, b| {
                        let (x, y) = (self.value_num(*a), self.value_num(*b));
                        match (x.is_nan(), y.is_nan()) {
                            (true, true) => std::cmp::Ordering::Equal,
                            (true, false) => std::cmp::Ordering::Greater,
                            (false, true) => std::cmp::Ordering::Less,
                            (false, false) => x.total_cmp(&y),
                        }
                    });
                }
                for (i, v) in snap.into_iter().enumerate() {
                    self.ta_element_set(idx, i, v)?;
                }
                Ok(Some(recv))
            }
            "copyWithin" => {
                let target = self.ta_rel_index(a0, 0, len)?;
                let start = self.ta_rel_index(a1, 0, len)?;
                let end =
                    self.ta_rel_index(args.get(2).copied().unwrap_or(Value::UNDEFINED), len, len)?;
                // The target/start/end coercions above may have run user code that
                // detached the buffer — re-check before copying (a detached buffer
                // here is a TypeError, not a silent no-op).
                if self.ta_effective_len(idx).is_none() {
                    return Err(Thrown(
                        "TypeError: Cannot copyWithin a detached or out-of-bounds TypedArray"
                            .into(),
                    ));
                }
                // A coercion may have SHRUNK a resizable buffer: both cursors stop
                // where the higher one falls off the current length (the spec's
                // byte loop ends at the live boundary).
                let cur = self.ta_effective_len(idx).unwrap_or(0);
                let count = end.max(start) - start;
                let bound = count.min(cur.saturating_sub(start.max(target)));
                self.preflight_native_iteration_work(bound as u64)?;
                let src: Vec<Value> = (0..bound)
                    .map(|k| self.ta_element_get(idx, start + k))
                    .collect();
                for (k, v) in src.into_iter().enumerate() {
                    if target + k < len {
                        self.ta_element_set(idx, target + k, v)?;
                    }
                }
                Ok(Some(recv))
            }
            "set" => {
                // offset = ToIntegerOrInfinity (throws on a Symbol / abrupt valueOf);
                // a negative offset is a RangeError (not a wrapping `as usize`).
                let offset = if a1 == Value::UNDEFINED {
                    0
                } else {
                    let n = self.to_integer_or_zero(a1)?;
                    if n < 0 {
                        return Err(Thrown("RangeError: offset is out of bounds".into()));
                    }
                    n as usize
                };
                // ToInteger(offset)'s valueOf may have detached the TARGET buffer:
                // re-check (SetTypedArrayFromArrayLike / FromTypedArray step).
                if self.ta_effective_len(idx).is_none() {
                    return Err(Thrown(
                        "TypeError: Cannot set values on a detached/out-of-bounds TypedArray"
                            .into(),
                    ));
                }
                // A BigInt typed array only mixes with a BigInt source (checked up
                // front when the source is itself a TypedArray); a TypedArray source
                // must also not be detached.
                let target_big = native::TA_KINDS[kind as usize].2;
                if a0.is_heap() {
                    if let HeapObj::TypedArray { kind: sk, .. } = self.heap.get(a0.heap_index()) {
                        if native::TA_KINDS[*sk as usize].2 != target_big {
                            return Err(Thrown(
                                "TypeError: cannot mix BigInt and other types when setting a TypedArray"
                                    .into(),
                            ));
                        }
                        if self.ta_effective_len(a0.heap_index()).is_none() {
                            return Err(Thrown(
                                "TypeError: source TypedArray has a detached buffer".into(),
                            ));
                        }
                    }
                }
                // SetTypedArrayFromTypedArray: a TypedArray source is snapshotted
                // up front (the source buffer may overlap the target's), then
                // written with element-type conversion.
                if a0.is_heap()
                    && matches!(self.heap.get(a0.heap_index()), HeapObj::TypedArray { .. })
                {
                    let src = self.ta_snapshot(a0.heap_index());
                    let end = offset.checked_add(src.len()).ok_or_else(|| {
                        Thrown("RangeError: source array is too long for the target offset".into())
                    })?;
                    if end > len {
                        return Err(Thrown(
                            "RangeError: source array is too long for the target offset".into(),
                        ));
                    }
                    self.preflight_native_iteration_work(src.len() as u64)?;
                    for (k, v) in src.into_iter().enumerate() {
                        self.ta_element_set(idx, offset + k, v)?;
                    }
                    return Ok(Some(Value::UNDEFINED));
                }
                // SetTypedArrayFromArrayLike: ToLength(Get(src,"length")) (a Symbol
                // length or a throwing length.valueOf propagates the abrupt
                // TypeError), then the RangeError bounds check BEFORE reading any
                // element, then an interleaved Get→ToNumber/ToBigInt→write loop so a
                // mid-iteration throw leaves the already-written elements in place
                // ("the values are set until exception"). The source is treated as
                // an array-like (length + integer indices), NOT iterated — matching
                // the spec, which never invokes the source's @@iterator.
                let len_val = self.get_prop(a0, "length")?;
                let src_len_u64 =
                    self.to_integer_or_zero(len_val)?.clamp(0, (1i64 << 53) - 1) as u64;
                // Keep ToLength in its 53-bit domain until the hostile-code
                // work check has run. Casting first makes 2^32 become zero on
                // wasm32, bypassing both the bounds check and the native-loop
                // budget.
                self.preflight_native_iteration_work(src_len_u64)?;
                let src_len = usize::try_from(src_len_u64).map_err(|_| {
                    Thrown("RangeError: source array is too long for the target offset".into())
                })?;
                let end = offset.checked_add(src_len).ok_or_else(|| {
                    Thrown("RangeError: source array is too long for the target offset".into())
                })?;
                if end > len {
                    return Err(Thrown(
                        "RangeError: source array is too long for the target offset".into(),
                    ));
                }
                for k in 0..src_len {
                    let v = self.get_index(a0, Value::num(k as f64))?;
                    self.ta_element_set(idx, offset + k, v)?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // Live iterators (kind 0=keys, 1=values, 2=entries): each step re-reads
            // the view's current length, so a resizable buffer's grow yields new
            // elements and an out-of-bounds view throws — matching ArrayIterator's
            // per-step IsTypedArrayOutOfBounds / Get(O, index).
            "keys" => Ok(Some(self.make_live_iterator(idx, 0, self.array_iter_proto))),
            "values" | "@@iterator" => {
                Ok(Some(self.make_live_iterator(idx, 1, self.array_iter_proto)))
            }
            "entries" => Ok(Some(self.make_live_iterator(idx, 2, self.array_iter_proto))),
            _ => Ok(None),
        }
    }

    /// `DataView.prototype.get/setInt8 … getFloat64` (+ `byteLength`/`byteOffset`/
    /// `buffer` are getters in get_prop). `name` is e.g. "getInt32".
    pub(crate) fn dataview_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let (buffer, byte_offset, byte_length) = match self.heap.get(idx) {
            HeapObj::DataView {
                buffer,
                byte_offset,
                byte_length,
            } => (*buffer, *byte_offset, *byte_length),
            _ => return Ok(None),
        };
        let (op, ty) = if let Some(t) = name.strip_prefix("get") {
            (0u8, t)
        } else if let Some(t) = name.strip_prefix("set") {
            (1u8, t)
        } else {
            return Ok(None);
        };
        // Element kind index for the suffix (Int8..Float64 / BigInt64 / BigUint64).
        let kind = match ty {
            "Int8" => 0,
            "Uint8" => 1,
            "Int16" => 3,
            "Uint16" => 4,
            "Int32" => 5,
            "Uint32" => 6,
            "Float32" => 7,
            "Float64" => 8,
            "BigInt64" => 9,
            "BigUint64" => 10,
            "Float16" => 16, // sentinel (not a TA_KIND); a 2-byte half float
            _ => return Ok(None),
        };
        let size = if kind == 16 {
            2
        } else {
            native::TA_KINDS[kind as usize].1
        };
        // SetViewValue step 3: writing through a DataView backed by an immutable
        // ArrayBuffer is a TypeError — BEFORE the byteOffset/value coercions (their
        // valueOf must not run). (Reads are fine on an immutable buffer.)
        if op == 1 && self.immutable_buffers.contains(&buffer) {
            return Err(Thrown(
                "TypeError: Cannot set a value on a DataView backed by an immutable ArrayBuffer"
                    .into(),
            ));
        }
        // requestIndex = ToIndex(arg0): runs valueOf/toString and throws RangeError
        // on a negative / too-large index — BEFORE the bounds check (and, for set*,
        // before the value conversion below) per GetViewValue/SetViewValue order.
        let pos = self.to_index(args.first().copied().unwrap_or(Value::UNDEFINED))?;
        // get(pos, littleEndian?) / set(pos, value, littleEndian?). ToBoolean never
        // throws, so its position relative to the bounds check is unobservable.
        let little_endian = if op == 0 {
            self.truthy(args.get(1).copied().unwrap_or(Value::UNDEFINED))
        } else {
            self.truthy(args.get(2).copied().unwrap_or(Value::UNDEFINED))
        };
        let bounds_ok = size <= byte_length && pos <= byte_length - size;
        let abs = byte_offset + pos;
        // GetViewValue/SetViewValue step "If IsViewOutOfBounds, throw TypeError":
        // the view is out of bounds if its backing buffer is detached OR (on a
        // resizable buffer that shrank) its byte range no longer fits. This
        // precedes the RangeError bounds check (per spec order — for get, after
        // ToIndex; for set, after the value conversion done above). A non-resizable
        // buffer can never shrink, so this only fires for detached/shrunk-resizable.
        let oob = match self.heap.get(buffer) {
            HeapObj::ArrayBuffer { data, detached } => {
                *detached || byte_offset + byte_length > data.len()
            }
            _ => true,
        };
        if op == 0 {
            if oob {
                return Err(Thrown(
                    "TypeError: Cannot perform DataView read on a detached or out-of-bounds ArrayBuffer".into(),
                ));
            }
            if !bounds_ok {
                return Err(Thrown(
                    "RangeError: Offset is outside the bounds of the DataView".into(),
                ));
            }
            // read
            let mut b = [0u8; 8];
            {
                let data = match self.heap.get(buffer) {
                    HeapObj::ArrayBuffer { data, .. } => data,
                    _ => return Ok(Some(Value::UNDEFINED)),
                };
                if size > data.len() || abs > data.len() - size {
                    return Err(Thrown("RangeError: DataView out of bounds".into()));
                }
                b[..size].copy_from_slice(&data[abs..abs + size]);
            }
            if !little_endian {
                b[..size].reverse();
            }
            Ok(Some(match kind {
                0 => Value::num(b[0] as i8 as f64),
                1 => Value::num(b[0] as f64),
                3 => Value::num(i16::from_le_bytes([b[0], b[1]]) as f64),
                4 => Value::num(u16::from_le_bytes([b[0], b[1]]) as f64),
                5 => Value::num(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                6 => Value::num(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                7 => Value::num(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                8 => Value::num(f64::from_le_bytes(b)),
                16 => Value::num(crate::vm::helpers_num2::f16_bits_to_f64(
                    u16::from_le_bytes([b[0], b[1]]),
                )),
                9 => self.make_bigint(i64::from_le_bytes(b) as i128),
                _ => self.make_bigint(u64::from_le_bytes(b) as i128),
            }))
        } else {
            // write
            let v = args.get(1).copied().unwrap_or(Value::UNDEFINED);
            let mut bytes = if kind == 16 {
                // Float16: ToNumber → nearest binary16 bits → 2 bytes (in [0..2]).
                let f = self.to_number_coerce(v)?;
                let mut a = [0u8; 8];
                a[..2].copy_from_slice(&crate::vm::helpers_num2::f64_to_f16_bits(f).to_le_bytes());
                a
            } else if kind >= 9 {
                // NumericToRawBytes: wrap to the low 64 bits (two's complement
                // — same byte image for BigInt64 and BigUint64; exact for any
                // magnitude, incl. the Big tier).
                self.to_bigint(v)?.to_u64_wrap().to_le_bytes()
            } else {
                let f = self.to_number_coerce(v)?;
                ta_encode(kind, f)
            };
            // SetViewValue converts the VALUE before checking the bounds — so a
            // throwing valueOf wins over an out-of-range offset. The out-of-bounds
            // check (TypeError) comes after the conversion (a valueOf may itself
            // detach/resize the buffer) and before the RangeError bounds check.
            let oob = match self.heap.get(buffer) {
                HeapObj::ArrayBuffer { data, detached } => {
                    *detached || byte_offset + byte_length > data.len()
                }
                _ => true,
            };
            if oob {
                return Err(Thrown(
                    "TypeError: Cannot perform DataView write on a detached or out-of-bounds ArrayBuffer".into(),
                ));
            }
            if !bounds_ok {
                return Err(Thrown(
                    "RangeError: Offset is outside the bounds of the DataView".into(),
                ));
            }
            if !little_endian {
                bytes[..size].reverse();
            }
            if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(buffer) {
                if abs + size <= data.len() {
                    data[abs..abs + size].copy_from_slice(&bytes[..size]);
                }
            }
            Ok(Some(Value::UNDEFINED))
        }
    }

    /// `ArrayBuffer.prototype.slice(begin?, end?)` → a new ArrayBuffer copy.
    pub(crate) fn arraybuffer_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let len = self.array_buffer_len(idx);
        match name {
            // `ArrayBuffer.prototype.resize(newLength)` — only for a resizable
            // buffer (created with maxByteLength); grows with zero-fill, shrinks
            // by truncation, within [0, maxByteLength].
            "resize" => {
                let max = match self.ab_max.get(&idx) {
                    Some(&m) => m,
                    None => return Err(Thrown("TypeError: ArrayBuffer is not resizable".into())),
                };
                let n =
                    self.to_integer_or_zero(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                if n < 0 {
                    return Err(Thrown(
                        "RangeError: ArrayBuffer resize length out of range".into(),
                    ));
                }
                // The detached check runs AFTER the newLength coercion (whose
                // valueOf always runs and may itself detach the buffer).
                if matches!(
                    self.heap.get(idx),
                    HeapObj::ArrayBuffer { detached: true, .. }
                ) {
                    return Err(Thrown(
                        "TypeError: Cannot resize a detached ArrayBuffer".into(),
                    ));
                }
                if n as usize > max {
                    return Err(Thrown(
                        "RangeError: ArrayBuffer resize length out of range".into(),
                    ));
                }
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(idx) {
                    data.resize_bytes(n as usize);
                }
                Ok(Some(Value::UNDEFINED))
            }
            // `SharedArrayBuffer.prototype.grow(newLength)` — only GROWS (never
            // shrinks), within [currentLength, maxByteLength]. SABs are never
            // detached.
            "grow" => {
                let max = match self.ab_max.get(&idx) {
                    Some(&m) => m,
                    None => {
                        return Err(Thrown(
                            "TypeError: SharedArrayBuffer is not growable".into(),
                        ))
                    }
                };
                let n =
                    self.to_integer_or_zero(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                if n < len as i64 || n as usize > max {
                    return Err(Thrown(
                        "RangeError: SharedArrayBuffer grow length out of range".into(),
                    ));
                }
                // A Shared store grows by an atomic length store (the bytes are
                // preallocated to maxByteLength, zeroed); Local falls back to a
                // Vec resize.
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(idx) {
                    data.resize_bytes(n as usize);
                }
                Ok(Some(Value::UNDEFINED))
            }
            "slice" => {
                // IsDetachedBuffer(O) -> TypeError (per spec, before index coercion).
                if matches!(
                    self.heap.get(idx),
                    HeapObj::ArrayBuffer { detached: true, .. }
                ) {
                    return Err(Thrown(
                        "TypeError: Cannot slice a detached ArrayBuffer".into(),
                    ));
                }
                let start =
                    self.ta_rel_index(args.first().copied().unwrap_or(Value::UNDEFINED), 0, len)?;
                let end =
                    self.ta_rel_index(args.get(1).copied().unwrap_or(Value::UNDEFINED), len, len)?;
                // A coercing index argument may have detached the buffer — re-check
                // and throw (rather than clamping the now-empty data to a 0 slice).
                if matches!(
                    self.heap.get(idx),
                    HeapObj::ArrayBuffer { detached: true, .. }
                ) {
                    return Err(Thrown(
                        "TypeError: Cannot slice a detached ArrayBuffer".into(),
                    ));
                }
                let dl = match self.heap.get(idx) {
                    HeapObj::ArrayBuffer { data, .. } => data.len(),
                    _ => 0,
                };
                let s = start.min(dl);
                let e = end.max(start).min(dl);
                let new_len = e - s;
                let is_shared = self.shared_buffers.contains(&idx);
                // SpeciesConstructor(O, %ArrayBuffer%): a user constructor[@@species]
                // builds the result; the default allocs a plain (or shared) buffer.
                let ctor = self.get_prop(Value::heap(idx), "constructor")?;
                let species = if ctor == Value::UNDEFINED {
                    Value::UNDEFINED
                } else if !self.is_object_value(ctor) {
                    return Err(Thrown(
                        "TypeError: ArrayBuffer constructor is not an object".into(),
                    ));
                } else {
                    let sp = self.get_prop(ctor, "@@species")?;
                    if sp == Value::NULL {
                        Value::UNDEFINED
                    } else {
                        sp
                    }
                };
                let new_idx = if species == Value::UNDEFINED {
                    if is_shared {
                        // A SAB slice is a NEW SharedArrayBuffer (copied bytes,
                        // not aliased memory) — allocate truly-shared storage.
                        self.alloc_shared_array_buffer(new_len, None)?
                    } else {
                        self.alloc_array_buffer(new_len)?
                    }
                } else {
                    if !self.is_constructor(species) {
                        return Err(Thrown(
                            "TypeError: ArrayBuffer [Symbol.species] is not a constructor".into(),
                        ));
                    }
                    let result = self.construct(species, &[Value::num(new_len as f64)])?;
                    // Validate: an ArrayBuffer, not shared (for a non-shared source),
                    // not detached, not the SAME buffer, and large enough.
                    let ridx = match result.is_heap().then(|| self.heap.get(result.heap_index())) {
                        Some(HeapObj::ArrayBuffer { .. }) => result.heap_index(),
                        _ => return Err(Thrown(
                            "TypeError: ArrayBuffer [Symbol.species] did not return an ArrayBuffer"
                                .into(),
                        )),
                    };
                    if !is_shared && self.shared_buffers.contains(&ridx) {
                        return Err(Thrown(
                            "TypeError: ArrayBuffer.prototype.slice species returned a SharedArrayBuffer".into(),
                        ));
                    }
                    if matches!(
                        self.heap.get(ridx),
                        HeapObj::ArrayBuffer { detached: true, .. }
                    ) {
                        return Err(Thrown(
                            "TypeError: ArrayBuffer.prototype.slice species returned a detached buffer".into(),
                        ));
                    }
                    if self.immutable_buffers.contains(&ridx) {
                        return Err(Thrown(
                            "TypeError: ArrayBuffer.prototype.slice species returned an immutable ArrayBuffer".into(),
                        ));
                    }
                    if ridx == idx {
                        return Err(Thrown(
                            "TypeError: ArrayBuffer.prototype.slice species returned the source buffer".into(),
                        ));
                    }
                    if self.array_buffer_len(ridx) < new_len {
                        return Err(Thrown(
                            "TypeError: ArrayBuffer.prototype.slice species buffer is too small"
                                .into(),
                        ));
                    }
                    ridx
                };
                // The species construction may have detached the source — re-check.
                if matches!(
                    self.heap.get(idx),
                    HeapObj::ArrayBuffer { detached: true, .. }
                ) {
                    return Err(Thrown(
                        "TypeError: source ArrayBuffer detached during species construction".into(),
                    ));
                }
                let slice: Vec<u8> = match self.heap.get(idx) {
                    HeapObj::ArrayBuffer { data, .. } => data[s..e].to_vec(),
                    _ => Vec::new(),
                };
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(new_idx) {
                    data[..slice.len()].copy_from_slice(&slice);
                }
                Ok(Some(Value::heap(new_idx)))
            }
            // ES2026: copy the buffer's bytes into a new IMMUTABLE ArrayBuffer and
            // detach the original (transfer semantics).
            "transferToImmutable" => {
                // ArrayBufferCopyAndDetach order: the newLength ToIndex coercion
                // (observable; may itself detach) runs BEFORE the detached and
                // immutable receiver checks.
                let new_len = match args.first() {
                    Some(&v) if v != Value::UNDEFINED => {
                        let n = self.to_index_strict(v)?;
                        if n > super::typedarray::MAX_ARRAY_BUFFER_LEN as usize {
                            return Err(Thrown("RangeError: invalid ArrayBuffer length".into()));
                        }
                        n
                    }
                    _ => len,
                };
                if matches!(
                    self.heap.get(idx),
                    HeapObj::ArrayBuffer { detached: true, .. }
                ) {
                    return Err(Thrown(
                        "TypeError: Cannot transfer a detached ArrayBuffer".into(),
                    ));
                }
                if self.immutable_buffers.contains(&idx) {
                    return Err(Thrown(
                        "TypeError: Cannot transfer an immutable ArrayBuffer".into(),
                    ));
                }
                let bytes: Vec<u8> = match self.heap.get(idx) {
                    HeapObj::ArrayBuffer { data, .. } => data.to_vec(),
                    _ => Vec::new(),
                };
                let new_idx = self.alloc_array_buffer(new_len)?;
                let n = bytes.len().min(new_len);
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(new_idx) {
                    data[..n].copy_from_slice(&bytes[..n]);
                }
                self.immutable_buffers.insert(new_idx);
                // Detach the source. (A SAB never reaches transfer — the native
                // brand check rejects shared receivers — so `data` is Local; the
                // resize_bytes(0) clear keeps the quirk fast path total anyway.)
                if let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(idx) {
                    data.resize_bytes(0);
                    *detached = true;
                }
                Ok(Some(Value::heap(new_idx)))
            }
            // ES2026: like slice but the result is an immutable ArrayBuffer.
            "sliceToImmutable" => {
                // Spec order: a detached receiver throws BEFORE the start/end
                // coercions; the bounds resolve against the ENTRY length; a detach
                // DURING coercion throws after them; a shrink below the resolved
                // end is a RangeError (no silent clamping).
                if matches!(
                    self.heap.get(idx),
                    HeapObj::ArrayBuffer { detached: true, .. }
                ) {
                    return Err(Thrown(
                        "TypeError: Cannot sliceToImmutable a detached ArrayBuffer".into(),
                    ));
                }
                let start = self.ta_rel_index_strict(
                    args.first().copied().unwrap_or(Value::UNDEFINED),
                    0,
                    len,
                )?;
                let end = self.ta_rel_index_strict(
                    args.get(1).copied().unwrap_or(Value::UNDEFINED),
                    len,
                    len,
                )?;
                let fin = end.max(start);
                if matches!(
                    self.heap.get(idx),
                    HeapObj::ArrayBuffer { detached: true, .. }
                ) {
                    return Err(Thrown(
                        "TypeError: Cannot sliceToImmutable a detached ArrayBuffer".into(),
                    ));
                }
                let slice: Vec<u8> = match self.heap.get(idx) {
                    HeapObj::ArrayBuffer { data, .. } => {
                        if data.len() < fin {
                            return Err(Thrown(
                                "RangeError: ArrayBuffer was resized below the resolved slice end"
                                    .into(),
                            ));
                        }
                        data[start..fin].to_vec()
                    }
                    _ => Vec::new(),
                };
                let new_idx = self.alloc_array_buffer(slice.len())?;
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(new_idx) {
                    data.copy_from_slice(&slice);
                }
                self.immutable_buffers.insert(new_idx);
                Ok(Some(Value::heap(new_idx)))
            }
            // ES2024: copy the bytes into a NEW (mutable) ArrayBuffer of `newLength`
            // bytes and detach the source. `transfer` preserves resizability (keeps
            // maxByteLength); `transferToFixedLength` produces a fixed buffer.
            "transfer" | "transferToFixedLength" => {
                // ArrayBufferCopyAndDetach order: coerce newLength (observable)
                // BEFORE the detached and immutable receiver checks.
                let new_len = match args.first() {
                    Some(&v) if v != Value::UNDEFINED => {
                        let n = self.to_index_strict(v)?;
                        if n > super::typedarray::MAX_ARRAY_BUFFER_LEN as usize {
                            return Err(Thrown("RangeError: invalid ArrayBuffer length".into()));
                        }
                        n
                    }
                    _ => len,
                };
                if matches!(
                    self.heap.get(idx),
                    HeapObj::ArrayBuffer { detached: true, .. }
                ) {
                    return Err(Thrown(
                        "TypeError: Cannot transfer a detached ArrayBuffer".into(),
                    ));
                }
                if self.immutable_buffers.contains(&idx) {
                    return Err(Thrown(
                        "TypeError: Cannot transfer an immutable ArrayBuffer".into(),
                    ));
                }
                let bytes: Vec<u8> = match self.heap.get(idx) {
                    HeapObj::ArrayBuffer { data, .. } => data.to_vec(),
                    _ => Vec::new(),
                };
                let new_idx = self.alloc_array_buffer(new_len)?;
                let n = bytes.len().min(new_len);
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(new_idx) {
                    data[..n].copy_from_slice(&bytes[..n]);
                }
                // `transfer` keeps the source's resizability (maxByteLength).
                if name == "transfer" {
                    if let Some(&m) = self.ab_max.get(&idx) {
                        self.ab_max.insert(new_idx, m.max(new_len));
                    }
                }
                if let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(idx) {
                    data.resize_bytes(0);
                    *detached = true;
                }
                Ok(Some(Value::heap(new_idx)))
            }
            _ => Ok(None),
        }
    }

    /// `Promise.prototype.then/catch/finally`. Returns a NEW dependent promise.
    /// All handlers run as microtasks (never synchronously). `idx` is the
    /// receiver promise's heap index.
    pub(crate) fn promise_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "then" => {
                let on_r = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                Ok(Some(self.perform_promise_then(idx, a0, on_r)?))
            }
            "catch" => {
                // Spec: Promise.prototype.catch(onRejected) is exactly
                // Invoke(this, "then", «undefined, onRejected») — it must observably
                // go through the receiver's own `then`, so an overridden `then` is
                // seen (mirrors `finally`). Unshadowed receivers reach the intrinsic
                // `then` via one call_value.
                let this = Value::heap(idx);
                let then = self.get_prop(this, "then")?;
                Ok(Some(self.call_value(
                    then,
                    this,
                    &[Value::UNDEFINED, a0],
                )?))
            }
            "finally" => {
                // Generic spec algorithm: Invoke(this, "then", «thenFinally,
                // catchFinally») via the receiver's own `then`, so an overridden
                // `then` / custom species constructor are observed (and the original
                // value/reason forwards through the wrappers). See `promise_finally`.
                Ok(Some(self.promise_finally(Value::heap(idx), a0)?))
            }
            _ => Ok(None),
        }
    }

    /// `Map.prototype.*`. `idx` is the Map's heap index. Returns `Ok(None)` for an
    /// unknown method (→ TypeError at the call site). `forEach` snapshots the
    /// entries before invoking the callback (which may mutate the map).
    pub(crate) fn map_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        // Brand check: `Map.prototype.<m>.call(x)` requires x to have [[MapData]].
        if !matches!(self.heap.get(idx), HeapObj::Map { .. }) {
            return Err(Thrown(format!(
                "TypeError: Map.prototype.{name} called on incompatible receiver"
            )));
        }
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "get" => {
                let v = self.coll_find(idx, a0).map(|i| match self.heap.get(idx) {
                    HeapObj::Map { vals, .. } => vals[i],
                    _ => Value::UNDEFINED,
                });
                Ok(Some(v.unwrap_or(Value::UNDEFINED)))
            }
            "has" => Ok(Some(Value::bool(self.coll_find(idx, a0).is_some()))),
            "set" => {
                let key = normalize_zero(a0);
                let val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pos = self.coll_find(idx, key);
                let mut pushed = None;
                // Nursery barrier for the VALUE (both arms): the insert's
                // `coll_index_insert` barriers the key, but a young value can
                // ride an old (or non-heap) key.
                self.heap.write_barrier_val(idx, val);
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val, // update in place, keep position
                        None => {
                            pushed = Some(keys.len());
                            keys.push(key);
                            vals.push(val);
                        }
                    }
                }
                if let Some(p) = pushed {
                    self.coll_index_insert(idx, key, p);
                }
                Ok(Some(recv)) // chainable
            }
            "getOrInsert" => {
                // Existing value wins; otherwise insert `value` and return it.
                let key = normalize_zero(a0);
                let val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if let Some(i) = self.coll_find(idx, key) {
                    if let HeapObj::Map { vals, .. } = self.heap.get(idx) {
                        return Ok(Some(vals[i]));
                    }
                }
                let mut pushed = None;
                // Nursery barrier for the VALUE (see the `set` arm).
                self.heap.write_barrier_val(idx, val);
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    pushed = Some(keys.len());
                    keys.push(key);
                    vals.push(val);
                }
                if let Some(p) = pushed {
                    self.coll_index_insert(idx, key, p);
                }
                Ok(Some(val))
            }
            "getOrInsertComputed" => {
                let cb = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if !self.is_callable(cb) {
                    return Err(Thrown(
                        "TypeError: the callback argument must be a function".into(),
                    ));
                }
                let key = normalize_zero(a0);
                if let Some(i) = self.coll_find(idx, key) {
                    if let HeapObj::Map { vals, .. } = self.heap.get(idx) {
                        return Ok(Some(vals[i]));
                    }
                }
                // Compute (may re-enter and mutate the map), then set key -> value
                // (overwriting if the callback inserted it) and return value.
                let val = self.call_value(cb, Value::UNDEFINED, &[key])?;
                let pos = self.coll_find(idx, key);
                let mut pushed = None;
                // Nursery barrier for the VALUE (see the `set` arm).
                self.heap.write_barrier_val(idx, val);
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val,
                        None => {
                            pushed = Some(keys.len());
                            keys.push(key);
                            vals.push(val);
                        }
                    }
                }
                if let Some(p) = pushed {
                    self.coll_index_insert(idx, key, p);
                }
                Ok(Some(val))
            }
            "delete" => {
                if let Some(i) = self.coll_find(idx, a0) {
                    if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                        // Tombstone (don't shift): a live forEach / iterator holds an index
                        // cursor. A tombstoned key (HOLE) is filtered out everywhere the
                        // entries are enumerated.
                        keys[i] = Value::HOLE;
                        vals[i] = Value::UNDEFINED;
                    }
                    // Positions don't shift, so only the dead key leaves the index.
                    self.coll_index_remove(idx, a0, i);
                    return Ok(Some(Value::bool(true)));
                }
                Ok(Some(Value::bool(false)))
            }
            "clear" => {
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    keys.clear();
                    vals.clear();
                }
                // Every slot position died: drop the index (rebuilds lazily).
                self.coll_index_invalidate(idx);
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                let cb = a0;
                if !self.is_callable(cb) {
                    return Err(Thrown(
                        "TypeError: Map.prototype.forEach callback is not a function".into(),
                    ));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // Walk the LIVE entries by index so a key the callback `set`s during
                // iteration is visited (sec-map.prototype.foreach).
                let mut i = 0;
                loop {
                    let (k, v) = match self.heap.get(idx) {
                        HeapObj::Map { keys, vals } if i < keys.len() => (keys[i], vals[i]),
                        _ => break,
                    };
                    i += 1;
                    self.preflight_native_iteration_work(i as u64)?;
                    // A tombstoned (deleted) entry is skipped.
                    if k.is_hole() {
                        continue;
                    }
                    // callback(value, key, map)
                    self.call_value(cb, this_arg, &[v, k, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // LIVE iterators over %MapIteratorPrototype% (step the backing store, so a
            // delete/add after the iterator is created is observed).
            "keys" => {
                let proto = self.map_iter_proto;
                Ok(Some(self.make_live_iterator(idx, 0, proto)))
            }
            "values" => {
                let proto = self.map_iter_proto;
                Ok(Some(self.make_live_iterator(idx, 1, proto)))
            }
            "entries" => {
                let proto = self.map_iter_proto;
                Ok(Some(self.make_live_iterator(idx, 2, proto)))
            }
            _ => Ok(None),
        }
    }

    /// `WeakMap.prototype.{get,set,has,delete}`. Brand-checked (the receiver must be
    /// a WeakMap, so `WeakMap.prototype.set.call(aMap)` throws) and keys must be
    /// objects. No GC, so entries are held strongly (unobservable without GC).
    /// CanBeHeldWeakly(v) (ES 7.3.X): a value usable as a WeakMap/WeakSet/WeakRef
    /// key/target — any Object, or a Symbol that is NOT in the global Symbol
    /// registry (a `Symbol.for` result cannot be held weakly).
    pub(crate) fn can_be_held_weakly(&self, v: Value) -> bool {
        if self.is_object_value(v) {
            return true;
        }
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
            return !self.symbol_registry.values().any(|&s| s.bits() == v.bits());
        }
        false
    }

    pub(crate) fn weakmap_method(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::WeakMap { .. }) {
            return Err(Thrown(format!(
                "TypeError: WeakMap.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "get" => {
                let v = self.coll_find(idx, a0).map(|i| match self.heap.get(idx) {
                    HeapObj::WeakMap { vals, .. } => vals[i],
                    _ => Value::UNDEFINED,
                });
                Ok(v.unwrap_or(Value::UNDEFINED))
            }
            "has" => Ok(Value::bool(self.coll_find(idx, a0).is_some())),
            "set" => {
                if !self.can_be_held_weakly(a0) {
                    return Err(Thrown(
                        "TypeError: Invalid value used as weak map key".into(),
                    ));
                }
                let val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pos = self.coll_find(idx, a0);
                let mut pushed = None;
                // Nursery barrier for the VALUE (the insert barriers the key).
                self.heap.write_barrier_val(idx, val);
                if let HeapObj::WeakMap { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val,
                        None => {
                            pushed = Some(keys.len());
                            keys.push(a0);
                            vals.push(val);
                        }
                    }
                }
                if let Some(p) = pushed {
                    self.coll_index_insert(idx, a0, p);
                }
                Ok(this) // chainable
            }
            // ES2025 upsert: existing value wins, else insert `value` (getOrInsert)
            // or the callback's result (getOrInsertComputed) and return it.
            "getOrInsert" | "getOrInsertComputed" => {
                if !self.can_be_held_weakly(a0) {
                    return Err(Thrown(
                        "TypeError: Invalid value used as weak map key".into(),
                    ));
                }
                let computed = name == "getOrInsertComputed";
                let cb = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if computed && !self.is_callable(cb) {
                    return Err(Thrown(
                        "TypeError: the callback argument must be a function".into(),
                    ));
                }
                if let Some(i) = self.coll_find(idx, a0) {
                    if let HeapObj::WeakMap { vals, .. } = self.heap.get(idx) {
                        return Ok(vals[i]);
                    }
                }
                let val = if computed {
                    // The callback may re-enter and mutate; re-find after.
                    self.call_value(cb, Value::UNDEFINED, &[a0])?
                } else {
                    cb // getOrInsert's `value` argument
                };
                let pos = self.coll_find(idx, a0);
                let mut pushed = None;
                // Nursery barrier for the VALUE (the insert barriers the key).
                self.heap.write_barrier_val(idx, val);
                if let HeapObj::WeakMap { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val,
                        None => {
                            pushed = Some(keys.len());
                            keys.push(a0);
                            vals.push(val);
                        }
                    }
                }
                if let Some(p) = pushed {
                    self.coll_index_insert(idx, a0, p);
                }
                Ok(val)
            }
            "delete" => {
                if let Some(i) = self.coll_find(idx, a0) {
                    if let HeapObj::WeakMap { keys, vals } = self.heap.get_mut(idx) {
                        keys.remove(i);
                        vals.remove(i);
                    }
                    // Vec::remove SHIFTS every later position: drop the whole
                    // index (rebuilds lazily) rather than patch it.
                    self.coll_index_invalidate(idx);
                    return Ok(Value::bool(true));
                }
                Ok(Value::bool(false))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `WeakSet.prototype.{add,has,delete}`. Brand-checked; values must be objects.
    pub(crate) fn weakset_method(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::WeakSet(_)) {
            return Err(Thrown(format!(
                "TypeError: WeakSet.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "has" => Ok(Value::bool(self.coll_find(idx, a0).is_some())),
            "add" => {
                if !self.can_be_held_weakly(a0) {
                    return Err(Thrown("TypeError: Invalid value used in weak set".into()));
                }
                if self.coll_find(idx, a0).is_none() {
                    let mut pushed = None;
                    if let HeapObj::WeakSet(items) = self.heap.get_mut(idx) {
                        pushed = Some(items.len());
                        items.push(a0);
                    }
                    if let Some(p) = pushed {
                        self.coll_index_insert(idx, a0, p);
                    }
                }
                Ok(this) // chainable
            }
            "delete" => {
                if let Some(i) = self.coll_find(idx, a0) {
                    if let HeapObj::WeakSet(items) = self.heap.get_mut(idx) {
                        items.remove(i);
                    }
                    // Vec::remove SHIFTS every later position: drop the whole
                    // index (rebuilds lazily) rather than patch it.
                    self.coll_index_invalidate(idx);
                    return Ok(Value::bool(true));
                }
                Ok(Value::bool(false))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `FinalizationRegistry.prototype.{register,unregister}`. No GC, so cleanup
    /// never fires; only the register/unregister bookkeeping (+ arg validation) is
    /// observable. `tokens` tracks live unregister tokens for `unregister`.
    pub(crate) fn finreg_method(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        if !this.is_heap()
            || !matches!(
                self.heap.get(this.heap_index()),
                HeapObj::FinalizationRegistry { .. }
            )
        {
            return Err(Thrown(format!(
                "TypeError: FinalizationRegistry.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "register" => {
                let held = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let token = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                // CanBeHeldWeakly: any object, or a non-registered Symbol.
                if !self.can_be_held_weakly(a0) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.register: target cannot be held weakly"
                            .into(),
                    ));
                }
                if self.same_value(a0, held) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.register: target and held value must not be the same".into(),
                    ));
                }
                if token != Value::UNDEFINED && !self.can_be_held_weakly(token) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.register: unregister token cannot be held weakly".into(),
                    ));
                }
                if token != Value::UNDEFINED {
                    // Nursery barrier: a young token pushed into an old registry.
                    self.heap.write_barrier_val(idx, token);
                    if let HeapObj::FinalizationRegistry { tokens, .. } = self.heap.get_mut(idx) {
                        tokens.push(token);
                    }
                }
                Ok(Value::UNDEFINED)
            }
            "unregister" => {
                if !self.can_be_held_weakly(a0) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.unregister: token cannot be held weakly"
                            .into(),
                    ));
                }
                let mut removed = false;
                if let HeapObj::FinalizationRegistry { tokens, .. } = self.heap.get_mut(idx) {
                    let before = tokens.len();
                    tokens.retain(|t| *t != a0); // object identity = Value bit-equality
                    removed = tokens.len() != before;
                }
                Ok(Value::bool(removed))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `Set.prototype.*`. `idx` is the Set's heap index. `keys`/`values`/`entries`
    /// return arrays (the iterator approximation).
    pub(crate) fn set_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        // Brand check: `Set.prototype.<m>.call(x)` requires x to have [[SetData]].
        if !matches!(self.heap.get(idx), HeapObj::Set(_)) {
            return Err(Thrown(format!(
                "TypeError: Set.prototype.{name} called on incompatible receiver"
            )));
        }
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "has" => Ok(Some(Value::bool(self.coll_find(idx, a0).is_some()))),
            "add" => {
                let val = normalize_zero(a0);
                if self.coll_find(idx, val).is_none() {
                    let mut pushed = None;
                    if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                        pushed = Some(items.len());
                        items.push(val);
                    }
                    if let Some(p) = pushed {
                        self.coll_index_insert(idx, val, p);
                    }
                }
                Ok(Some(recv)) // chainable
            }
            "delete" => {
                if let Some(i) = self.coll_find(idx, a0) {
                    if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                        // Tombstone (don't shift): a live forEach / iterator holds an index
                        // cursor, so removing the slot would skip the next element. The
                        // slot is filtered out by size / has / iteration / set algebra.
                        items[i] = Value::HOLE;
                    }
                    // Positions don't shift, so only the dead value leaves the index.
                    self.coll_index_remove(idx, a0, i);
                    return Ok(Some(Value::bool(true)));
                }
                Ok(Some(Value::bool(false)))
            }
            "clear" => {
                if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                    items.clear();
                }
                // Every slot position died: drop the index (rebuilds lazily).
                self.coll_index_invalidate(idx);
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                let cb = a0;
                if !self.is_callable(cb) {
                    return Err(Thrown(
                        "TypeError: Set.prototype.forEach callback is not a function".into(),
                    ));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // Iterate the LIVE backing store by index (not a frozen clone) so a
                // value the callback `add`s during iteration is still visited
                // (sec-set.prototype.foreach walks the live entries list).
                let mut i = 0;
                loop {
                    let v = match self.heap.get(idx) {
                        HeapObj::Set(items) if i < items.len() => items[i],
                        _ => break,
                    };
                    i += 1;
                    self.preflight_native_iteration_work(i as u64)?;
                    // A tombstoned (deleted) slot is skipped.
                    if v.is_hole() {
                        continue;
                    }
                    // callback(value, value, set) — value passed twice, mirroring Map.
                    self.call_value(cb, this_arg, &[v, v, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // keys() === values() for a Set; both yield the values. A LIVE iterator
            // (steps the backing store) so a delete/add after creation is observed.
            "keys" | "values" => {
                let proto = self.set_iter_proto;
                Ok(Some(self.make_live_iterator(idx, 1, proto)))
            }
            "entries" => {
                let proto = self.set_iter_proto;
                Ok(Some(self.make_live_iterator(idx, 2, proto)))
            }
            // ES2025 set methods. `other` must be set-like; the common (and tested)
            // case is a real Set, whose elements we read directly.
            "union"
            | "intersection"
            | "difference"
            | "symmetricDifference"
            | "isSubsetOf"
            | "isSupersetOf"
            | "isDisjointFrom" => {
                // Calls user has()/keys() (Set-like arg), so suspend GC for the scope.
                let _gc = self.gc_lock_guard();
                // GetSetRecord (read size / has / keys in spec order) WITHOUT yet
                // calling keys(): a real Set uses its elements directly; a Set-like
                // ({size, has, keys}) keeps its has/keys methods so the size-favoured
                // branches use has() rather than iterating its keys.
                let (other_real, other_size, other_has, other_keys): (
                    Option<Vec<Value>>,
                    i64,
                    Value,
                    Value,
                ) = match a0.is_heap().then(|| self.heap.get(a0.heap_index())) {
                    Some(HeapObj::Set(items)) => {
                        // Skip tombstoned (deleted) slots.
                        self.preflight_native_iteration_work(items.len() as u64)?;
                        let items: Vec<Value> =
                            items.iter().copied().filter(|v| !v.is_hole()).collect();
                        let n = items.len() as i64;
                        (Some(items), n, Value::UNDEFINED, Value::UNDEFINED)
                    }
                    _ => {
                        if !self.is_object_value(a0) {
                            return Err(Thrown(
                                "TypeError: Set.prototype set method called with a non-object"
                                    .into(),
                            ));
                        }
                        let raw_size = self.get_prop(a0, "size")?;
                        if raw_size.is_heap()
                            && matches!(
                                self.heap.get(raw_size.heap_index()),
                                HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
                            )
                        {
                            return Err(Thrown(
                                "TypeError: Set-like 'size' cannot be a BigInt".into(),
                            ));
                        }
                        let num_size = self.to_number_coerce(raw_size)?;
                        if num_size.is_nan() {
                            return Err(Thrown("TypeError: Set-like 'size' is NaN".into()));
                        }
                        let int_size = if num_size.is_infinite() {
                            i64::MAX
                        } else {
                            num_size.trunc() as i64
                        };
                        if int_size < 0 {
                            return Err(Thrown("RangeError: Set-like 'size' is negative".into()));
                        }
                        let has = self.get_prop(a0, "has")?;
                        if !self.is_callable(has) {
                            return Err(Thrown("TypeError: Set-like 'has' is not callable".into()));
                        }
                        let keys = self.get_prop(a0, "keys")?;
                        if !self.is_callable(keys) {
                            return Err(Thrown(
                                "TypeError: Set-like 'keys' is not callable".into(),
                            ));
                        }
                        (None, int_size, has, keys)
                    }
                };
                // Snapshot `this`'s elements AFTER GetSetRecord: the `size`/`has`/`keys`
                // getters of a Set-like argument may have mutated this Set (the spec
                // copies O.[[SetData]] at this point, so any element added by the getters
                // is included — see union/difference/symmetricDifference mutation tests).
                let this_items: Vec<Value> = match self.heap.get(idx) {
                    HeapObj::Set(items) => {
                        self.preflight_native_iteration_work(items.len() as u64)?;
                        items.iter().copied().filter(|v| !v.is_hole()).collect()
                    }
                    _ => Vec::new(),
                };
                let mem = |hay: &[Value], v: Value, vm: &Self| {
                    hay.iter().any(|x| vm.same_value_zero(*x, v))
                };
                let this_size = this_items.len() as i64;
                let result = match name {
                    // union / symmetricDifference always iterate the other set.
                    "union" => {
                        // The copy of O.[[SetData]] is step 5, AFTER
                        // GetKeysIterator (step 4) — so a `next` GETTER that
                        // mutates the receiver still shapes the result.
                        let it = self.set_rec_keys_iter(&other_real, other_keys, a0)?;
                        let mut r: Vec<Value> = match self.heap.get(idx) {
                            HeapObj::Set(items) => {
                                items.iter().copied().filter(|v| !v.is_hole()).collect()
                            }
                            _ => Vec::new(),
                        };
                        let mut finder = crate::vm::collections::LocalFinder::new();
                        for v in self.set_rec_drain(&other_real, it)? {
                            if finder.find(&self.heap, &r, v).is_none() {
                                finder.record_push(&self.heap, &r, v);
                                r.push(v);
                            }
                        }
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "symmetricDifference" => {
                        // resultSetData = copy of O (after GetSetRecord). For each key
                        // the spec decides remove-vs-keep by SetDataHas(O.[[SetData]], key)
                        // — the LIVE receiver, which the keys() iterator may have mutated —
                        // not by the result. (See symmetricDifference set-like-class-mutation.)
                        // Same step order as union: GetKeysIterator, THEN copy
                        // O.[[SetData]] into the result.
                        let it = self.set_rec_keys_iter(&other_real, other_keys, a0)?;
                        let mut r: Vec<Value> = match self.heap.get(idx) {
                            HeapObj::Set(items) => {
                                items.iter().copied().filter(|v| !v.is_hole()).collect()
                            }
                            _ => Vec::new(),
                        };
                        // LAZY, for the same reason as intersection's else arm:
                        // the live test must run between two steps.
                        let mut real_pos = 0usize;
                        let mut native_work = 0u64;
                        loop {
                            let v = match it {
                                None => {
                                    let items = other_real.as_deref().unwrap_or(&[]);
                                    if real_pos >= items.len() {
                                        break;
                                    }
                                    real_pos += 1;
                                    items[real_pos - 1]
                                }
                                Some((kiter, next)) => match self.set_rec_step(kiter, next)? {
                                    Some(v) => v,
                                    None => break,
                                },
                            };
                            native_work = native_work.saturating_add(r.len().max(1) as u64);
                            self.preflight_native_iteration_work(native_work)?;
                            if self.set_has_live(idx, v) {
                                // In O → remove it from the result if present.
                                r.retain(|&x| !self.same_value_zero(x, v));
                            } else if !mem(&r, v, self) {
                                // Not in O → add it to the result if absent.
                                r.push(v);
                            }
                        }
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    // intersection / difference / isDisjointFrom use has() when this
                    // is the smaller side (and must NOT iterate the other's keys).
                    "intersection" => {
                        let mut r: Vec<Value> = Vec::new();
                        let mut finder = crate::vm::collections::LocalFinder::new();
                        if this_size <= other_size {
                            if let Some(items) = &other_real {
                                self.preflight_native_iteration_work(
                                    (this_size as u64).saturating_mul(items.len() as u64),
                                )?;
                            }
                            // Walk O.[[SetData]] LIVE by index, re-reading its
                            // length each step. `other`'s has() may delete an
                            // element and re-add it, which moves it to the END
                            // of the list — so the SAME element is visited
                            // twice, which the spec calls out in a NOTE.
                            let mut index = 0usize;
                            loop {
                                let e = match self.heap.get(idx) {
                                    HeapObj::Set(items) if index < items.len() => items[index],
                                    _ => break,
                                };
                                index += 1;
                                if e.is_hole() {
                                    continue; // tombstoned (deleted) slot
                                }
                                if self.set_rec_has(&other_real, other_has, a0, e)?
                                    && finder.find(&self.heap, &r, e).is_none()
                                {
                                    finder.record_push(&self.heap, &r, e);
                                    r.push(e);
                                }
                            }
                        } else {
                            // LAZY: `SetDataHas(O.[[SetData]], next)` runs
                            // between two `IteratorStepValue` calls, so a
                            // generator `keys()` that mutates the receiver
                            // after yielding must not affect the keys it
                            // already yielded.
                            let it = self.set_rec_keys_iter(&other_real, other_keys, a0)?;
                            match it {
                                None => {
                                    for v in other_real.clone().unwrap_or_default() {
                                        if self.set_has_live(idx, v)
                                            && finder.find(&self.heap, &r, v).is_none()
                                        {
                                            finder.record_push(&self.heap, &r, v);
                                            r.push(v);
                                        }
                                    }
                                }
                                Some((kiter, next)) => {
                                    let mut native_work = 0u64;
                                    while let Some(v) = self.set_rec_step(kiter, next)? {
                                        native_work = native_work.saturating_add(1);
                                        if let Err(e) =
                                            self.preflight_native_iteration_work(native_work)
                                        {
                                            self.iterator_close_quiet(kiter);
                                            return Err(e);
                                        }
                                        if self.set_has_live(idx, v)
                                            && finder.find(&self.heap, &r, v).is_none()
                                        {
                                            finder.record_push(&self.heap, &r, v);
                                            r.push(v);
                                        }
                                    }
                                }
                            }
                        }
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "difference" => {
                        let mut r = this_items.clone();
                        if this_size <= other_size {
                            if let Some(items) = &other_real {
                                self.preflight_native_iteration_work(
                                    (this_size as u64).saturating_mul(items.len() as u64),
                                )?;
                            }
                            let mut keep: Vec<Value> = Vec::new();
                            for &e in &r {
                                if !self.set_rec_has(&other_real, other_has, a0, e)? {
                                    keep.push(e);
                                }
                            }
                            r = keep;
                        } else {
                            let mut native_work = 0u64;
                            for v in self.set_rec_keys(&other_real, other_keys, a0)? {
                                native_work = native_work.saturating_add(r.len().max(1) as u64);
                                self.preflight_native_iteration_work(native_work)?;
                                r.retain(|&x| !self.same_value_zero(x, v));
                            }
                        }
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "isSubsetOf" => {
                        if this_size > other_size {
                            Value::bool(false)
                        } else {
                            if let Some(items) = &other_real {
                                self.preflight_native_iteration_work(
                                    (this_size as u64).saturating_mul(items.len() as u64),
                                )?;
                            }
                            // Iterate `this` LIVE: the argument's has() may delete a
                            // not-yet-visited element, so re-read the Set's length and
                            // current element each step (the spec re-reads thisSize and
                            // skips emptied slots — see isSubsetOf set-like-class-mutation).
                            let mut ok = true;
                            let mut index = 0usize;
                            loop {
                                let e = match self.heap.get(idx) {
                                    HeapObj::Set(items) if index < items.len() => items[index],
                                    _ => break,
                                };
                                index += 1;
                                if e.is_hole() {
                                    continue; // tombstoned (deleted) slot
                                }
                                if !self.set_rec_has(&other_real, other_has, a0, e)? {
                                    ok = false;
                                    break;
                                }
                            }
                            Value::bool(ok)
                        }
                    }
                    "isSupersetOf" => {
                        if this_size < other_size {
                            Value::bool(false)
                        } else if let Some(items) = &other_real {
                            let items = items.clone();
                            let mut ok = true;
                            for v in items {
                                // `SetDataHas(O.[[SetData]], next)` — the LIVE
                                // receiver: the keys iterator's `value` getter
                                // may add the very element about to be tested.
                                if !self.set_has_live(idx, v) {
                                    ok = false;
                                    break;
                                }
                            }
                            Value::bool(ok)
                        } else {
                            // Set-like: step keys() LAZILY and IteratorClose on an early
                            // break, so the iterator's return() is invoked exactly once
                            // when we stop short (see isSupersetOf set-like-iter-return /
                            // set-like-class-order).
                            let kiter = self.call_value(other_keys, a0, &[])?;
                            if !self.is_object_value(kiter) {
                                return Err(Thrown(
                                    "TypeError: Set-like keys() did not return an object".into(),
                                ));
                            }
                            let next = self.get_prop(kiter, "next")?;
                            let mut ok = true;
                            let mut native_work = 0u64;
                            while let Some(v) = self.iterator_step_with(kiter, next)? {
                                native_work = native_work.saturating_add(1);
                                if let Err(e) = self.preflight_native_iteration_work(native_work) {
                                    self.iterator_close_quiet(kiter);
                                    return Err(e);
                                }
                                if !self.set_has_live(idx, v) {
                                    ok = false;
                                    self.iterator_close(kiter)?;
                                    break;
                                }
                            }
                            Value::bool(ok)
                        }
                    }
                    _ => {
                        // isDisjointFrom
                        let mut disjoint = true;
                        if this_size <= other_size {
                            if let Some(items) = &other_real {
                                self.preflight_native_iteration_work(
                                    (this_size as u64).saturating_mul(items.len() as u64),
                                )?;
                            }
                            // Iterate `this` LIVE (has() may delete not-yet-visited
                            // elements — see isDisjointFrom set-like-class-mutation).
                            let mut index = 0usize;
                            loop {
                                let e = match self.heap.get(idx) {
                                    HeapObj::Set(items) if index < items.len() => items[index],
                                    _ => break,
                                };
                                index += 1;
                                if e.is_hole() {
                                    continue; // tombstoned (deleted) slot
                                }
                                if self.set_rec_has(&other_real, other_has, a0, e)? {
                                    disjoint = false;
                                    break;
                                }
                            }
                        } else if let Some(items) = &other_real {
                            let items = items.clone();
                            for v in items {
                                // `SetDataHas(O.[[SetData]], nextValue)`: live.
                                if self.set_has_live(idx, v) {
                                    disjoint = false;
                                    break;
                                }
                            }
                        } else {
                            // Set-like: step keys() lazily, IteratorClose on early break.
                            let kiter = self.call_value(other_keys, a0, &[])?;
                            if !self.is_object_value(kiter) {
                                return Err(Thrown(
                                    "TypeError: Set-like keys() did not return an object".into(),
                                ));
                            }
                            let next = self.get_prop(kiter, "next")?;
                            let mut native_work = 0u64;
                            while let Some(v) = self.iterator_step_with(kiter, next)? {
                                native_work = native_work.saturating_add(1);
                                if let Err(e) = self.preflight_native_iteration_work(native_work) {
                                    self.iterator_close_quiet(kiter);
                                    return Err(e);
                                }
                                if self.set_has_live(idx, v) {
                                    disjoint = false;
                                    self.iterator_close(kiter)?;
                                    break;
                                }
                            }
                        }
                        Value::bool(disjoint)
                    }
                };
                Ok(Some(result))
            }
            _ => Ok(None),
        }
    }

    /// Set-operation membership test against the OTHER set: a real Set checks its
    /// elements directly; a Set-like calls its `has` method. (Calling `has` rather
    /// than iterating `keys` is required by the spec for the size-favoured branch
    /// of intersection/difference/isSubsetOf/isDisjointFrom.)
    fn set_rec_has(
        &mut self,
        real: &Option<Vec<Value>>,
        has_fn: Value,
        obj: Value,
        v: Value,
    ) -> Result<bool, Thrown> {
        if let Some(items) = real {
            return Ok(items.iter().any(|x| self.same_value_zero(*x, v)));
        }
        let r = self.call_value(has_fn, obj, &[v])?;
        Ok(self.truthy(r))
    }

    /// Materialize the OTHER set's elements: a real Set clones them; a Set-like
    /// calls `keys()` and drains the iterator (normalizing -0 → +0). Only invoked
    /// by the branches that genuinely iterate the other set.
    fn set_rec_keys(
        &mut self,
        real: &Option<Vec<Value>>,
        keys_fn: Value,
        obj: Value,
    ) -> Result<Vec<Value>, Thrown> {
        let it = self.set_rec_keys_iter(real, keys_fn, obj)?;
        self.set_rec_drain(real, it)
    }

    /// `SetDataHas(O.[[SetData]], v)` against the LIVE receiver.
    ///
    /// Every spec step that consults the receiver mid-iteration reads
    /// `O.[[SetData]]` itself, not a copy — an `other` whose `has`, `next` or
    /// `value` runs user code can add to or delete from the receiver in
    /// between, and that must be visible.
    fn set_has_live(&mut self, set_idx: u32, v: Value) -> bool {
        self.coll_find(set_idx, v).is_some()
    }

    /// `GetKeysIterator(otherRec)` on its own: call `keys()`, require an object,
    /// then read and validate `next`. `None` for a real Set, whose elements are
    /// read directly.
    ///
    /// Split from the drain below because `union` and `symmetricDifference` copy
    /// `O.[[SetData]]` *between* the two — a `next` getter that clears the
    /// receiver and adds a new element must be reflected in the result.
    fn set_rec_keys_iter(
        &mut self,
        real: &Option<Vec<Value>>,
        keys_fn: Value,
        obj: Value,
    ) -> Result<Option<(Value, Value)>, Thrown> {
        if real.is_some() {
            return Ok(None);
        }
        let kiter = self.call_value(keys_fn, obj, &[])?;
        // GetSetRecord treats the `keys()` result as an ALREADY-OBTAINED Iterator
        // Record: `Get(keysIter, "next")` once, then repeated Call. Draining it
        // with the general iterable helper additionally read
        // `keysIter[Symbol.iterator]` and called it, which is observable on a
        // Proxy-wrapped set-like and is not in the algorithm.
        let next = self.get_prop(kiter, "next")?;
        if !self.is_callable(next) {
            return Err(Thrown(
                "TypeError: set-like keys() iterator has no next method".into(),
            ));
        }
        Ok(Some((kiter, next)))
    }

    /// One `IteratorStepValue(keysIter)`. `None` once the iterator is done.
    fn set_rec_step(&mut self, kiter: Value, next: Value) -> Result<Option<Value>, Thrown> {
        let res = self.call_value(next, kiter, &[])?;
        if !self.is_object_value(res) {
            return Err(Thrown("TypeError: iterator result is not an object".into()));
        }
        let done = self.get_prop(res, "done")?;
        if self.truthy(done) {
            return Ok(None);
        }
        let v = self.get_prop(res, "value")?;
        // -0 normalises to +0 for SameValueZero membership.
        Ok(Some(if v.is_number() && v.as_f64() == 0.0 {
            Value::int(0)
        } else {
            v
        }))
    }

    /// Drain the iterator [`Self::set_rec_keys_iter`] returned.
    ///
    /// Only for the branches whose per-element test reads NOTHING live — the
    /// ones that consult `O.[[SetData]]` must step lazily instead, or a
    /// generator's later mutations would decide earlier elements.
    fn set_rec_drain(
        &mut self,
        real: &Option<Vec<Value>>,
        it: Option<(Value, Value)>,
    ) -> Result<Vec<Value>, Thrown> {
        let Some((kiter, next)) = it else {
            return Ok(real.clone().unwrap_or_default());
        };
        let mut out = Vec::new();
        let mut native_work = 0u64;
        while let Some(v) = self.set_rec_step(kiter, next)? {
            native_work = native_work.saturating_add(1);
            if let Err(e) = self.preflight_native_iteration_work(native_work) {
                self.iterator_close_quiet(kiter);
                return Err(e);
            }
            out.try_reserve(1).map_err(|_| {
                self.iterator_close_quiet(kiter);
                Thrown("RangeError: set-like iterator allocation failed".into())
            })?;
            out.push(v);
        }
        Ok(out)
    }
}
