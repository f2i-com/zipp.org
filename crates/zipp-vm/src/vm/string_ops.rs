#![allow(unused_imports)]
use super::*;

use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// `ZIPP_NO_MATCHALL_PRISTINE=1` disables the pristine dispatch shortcut for
/// `String.prototype.matchAll` (the B77 retry), restoring the fully observable
/// preamble on every call — the rollback switch and one side of a one-binary
/// A/B (`tools/bench.py --ab-env`). Same idiom as `ZIPP_NO_PROMISE_SLOT_CACHE`.
#[inline]
fn matchall_pristine_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_MATCHALL_PRISTINE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

impl<'p> Vm<'p> {
    /// Narrow interpreter leaf for the overwhelmingly common
    /// `flat_primitive_string.charCodeAt(int)` shape. A miss is read-only and
    /// returns to the full named-method route, which performs property lookup,
    /// argument coercion, rope flattening and cross-realm selection.
    ///
    /// The live intrinsic proof is deliberately shared with the generic string
    /// builtin dispatcher: it rejects a replaced/deleted/accessorized prototype
    /// slot and any active child realm. Boxed strings, proxies, ropes and
    /// non-Int arguments are excluded before any observable work.
    pub(crate) fn interp_char_code_at_fast(&mut self, recv: Value, arg: Value) -> Option<Value> {
        if !recv.is_heap()
            || !arg.is_int()
            || !matches!(self.heap.get(recv.heap_index()), HeapObj::Str(_))
            || !self.string_method_is_intrinsic("charCodeAt")
        {
            return None;
        }

        // Keep the optional diagnostics identical to dispatch_builtin_method:
        // stats are counted outside the StringOps phase, then the actual string
        // operation is attributed to that phase when the sampler is enabled.
        super::builtins::builtin_stats_count(self, recv, "charCodeAt");
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::StringOps);
        let i = arg.as_int();
        let unit = if i >= 0 {
            self.heap_unit_at(recv.heap_index(), i as usize)
        } else {
            None
        };
        Some(match unit {
            Some(unit) => Value::int(unit as i32),
            None => Value::num(f64::NAN),
        })
    }

    /// IsRegExp(v) (ES 7.2.8): a `@@match` property overrides — when present it is
    /// ToBoolean'd; otherwise true iff `v` is a RegExp exotic. Non-objects are not
    /// regexps. Used by `String.prototype.{includes,startsWith,endsWith}`, which
    /// reject a regexp searchString.
    pub(crate) fn is_regexp(&mut self, v: Value) -> Result<bool, Thrown> {
        if !self.is_object_value(v) {
            return Ok(false);
        }
        let m = self.get_prop(v, "@@match")?;
        if m != Value::UNDEFINED {
            return Ok(self.truthy(m));
        }
        Ok(matches!(
            self.heap.get(v.heap_index()),
            HeapObj::RegExp { .. }
        ))
    }

    /// The value-form (`.call`/`.apply`) entry for the String.prototype methods that
    /// consult an argument's well-known Symbol method: replace/replaceAll/split/
    /// match/search/matchAll. Per spec these do RequireObjectCoercible(this) and
    /// observe the argument (IsRegExp/flags for replaceAll/matchAll, then
    /// GetMethod(arg, @@…)) with the RAW receiver BEFORE ToString(this) — so a
    /// poison `this` is not coerced early and an @@-method receives the raw
    /// receiver. When no @@-method applies, the receiver is ToString'd and the call
    /// falls to the default algorithm in `string_method`.
    pub(crate) fn string_symbol_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        if recv == Value::UNDEFINED || recv == Value::NULL {
            return Err(Thrown(format!(
                "TypeError: String.prototype.{name} called on null or undefined"
            )));
        }
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // ── pristine dispatch shortcut (the B77 retry) ──
        // When every read the preamble below would perform is proven to yield
        // the intrinsic, the whole sequence reduces to invoking
        // %RegExp.prototype[Symbol.matchAll]% directly. `None` falls through
        // to the fully observable protocol.
        if name == "matchAll" && matchall_pristine_enabled() {
            if let Some(r) = self.matchall_pristine_dispatch(recv, arg0) {
                return r;
            }
        }
        let sym = match name {
            "replace" | "replaceAll" => "@@replace",
            "split" => "@@split",
            "match" => "@@match",
            "search" => "@@search",
            "matchAll" => "@@matchAll",
            _ => unreachable!("string_symbol_method called with {name}"),
        };
        // replaceAll/matchAll require a RegExp argument to be global — observed
        // (IsRegExp → Get flags → RequireObjectCoercible → ToString contains 'g')
        // BEFORE any ToString of the receiver.
        if (name == "replaceAll" || name == "matchAll")
            && arg0 != Value::UNDEFINED
            && arg0 != Value::NULL
            && self.is_regexp(arg0)?
        {
            let flags = self.get_prop(arg0, "flags")?;
            if flags == Value::UNDEFINED || flags == Value::NULL {
                return Err(Thrown(format!(
                    "TypeError: String.prototype.{name} called with a RegExp whose flags is not coercible"
                )));
            }
            let fs = self.to_js_string(flags)?;
            if !fs.contains('g') {
                return Err(Thrown(format!(
                    "TypeError: String.prototype.{name} must be called with a global RegExp"
                )));
            }
        }
        // GetMethod(arg0, @@sym) with the RAW receiver (a present-but-not-callable
        // method is a TypeError; null/undefined falls through to the default path).
        if self.is_object_value(arg0) {
            let m = self.get_prop(arg0, sym)?;
            if m != Value::UNDEFINED && m != Value::NULL {
                if !self.is_callable(m) {
                    return Err(Thrown(format!("TypeError: {sym} is not a function")));
                }
                return match name {
                    "replace" | "replaceAll" | "split" => {
                        let extra = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                        self.call_value(m, arg0, &[recv, extra])
                    }
                    _ => self.call_value(m, arg0, &[recv]),
                };
            }
        }
        // No @@-method: ToString(receiver), then the default algorithm. (The default
        // arms in string_method re-check the @@-method, but it is absent here, so
        // that is a no-op apart from a redundant property read.) A receiver that
        // already IS a string passes through EXACTLY (its lone surrogates
        // survive — `to_js_string` would be lossy).
        let s_idx = if recv.is_heap() && self.heap.is_str_like(recv.heap_index()) {
            recv.heap_index()
        } else {
            let s = self.to_js_string(recv)?;
            self.alloc_str(s).heap_index()
        };
        Ok(self
            .string_method(s_idx, name, args)?
            .unwrap_or(Value::UNDEFINED))
    }

    /// The pristine dispatch shortcut for `String.prototype.matchAll` — B77's
    /// mechanism, retried with different PLACEMENT per its revert note: the
    /// guards live in this separate `#[inline(never)]` function instead of
    /// inline in `string_symbol_method`, so the hot ~40 lines get their own
    /// code address rather than reshaping the caller (B77's target win was
    /// real twice; the revert was fat-LTO layout collateral on a row with no
    /// `matchAll` in it).
    ///
    /// `Some(result)` when the preamble's every observable step provably
    /// yields the intrinsic, in which case the whole protocol reduces to
    /// calling %RegExp.prototype[Symbol.matchAll]% directly:
    ///
    ///   * IsRegExp(arg): `Get(arg, @@match)` — no own `@@match`, prototype is
    ///     exactly %RegExp.prototype%, its `@@match` still the intrinsic DATA
    ///     property (truthy, so IsRegExp answers true);
    ///   * `Get(arg, "flags")` + ToString + the `'g'` test —
    ///     `regexp_pristine_flags` (no own `flags`/flag-name shadow, all eight
    ///     flag accessors intrinsic — a lying `global` getter declines) plus
    ///     the `flags` slot itself still the intrinsic accessor, checked here;
    ///   * `GetMethod(arg, @@matchAll)` — no own `@@matchAll`, the prototype
    ///     slot still the intrinsic native;
    ///   * the `call_value` — replaced by a direct native call.
    ///
    /// `None` (any guard fails, including a missing `'g'` — the generic path
    /// re-derives and throws the identical TypeError) falls through to the
    /// fully observable protocol. Every slot value and accessor bit is re-read
    /// per call: an in-place `RegExp.prototype.exec = f`-style overwrite bumps
    /// no version (B67/B110), so nothing here is trusted from a cache.
    #[inline(never)]
    pub(crate) fn matchall_pristine_dispatch(
        &mut self,
        recv: Value,
        arg0: Value,
    ) -> Option<Result<Value, Thrown>> {
        let re = self.as_regexp(arg0)?;
        // Own shadows of the two symbols make the preamble observable again.
        // (`flags` and the eight flag names are re-checked by
        // `regexp_pristine_flags`, which also pins the [[Prototype]].)
        if self
            .arr_props
            .get(&re)
            .is_some_and(|m| m.pos("@@match").is_some() || m.pos("@@matchAll").is_some())
        {
            return None;
        }
        if !self.regexp_pristine_flag_accessors_ok(re, arg0) {
            return None;
        }
        let proto_ok = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => {
                let data_native =
                    |k: &str, id: u16| self.regexp_proto_slot_is_intrinsic(m, k, false, id);
                data_native("@@match", native::REGEXP_SYM_MATCH)
                    && data_native("@@matchAll", native::REGEXP_SYM_MATCHALL)
            }
            _ => false,
        };
        if !proto_ok {
            return None;
        }
        if !matches!(
            self.heap.get(re),
            HeapObj::RegExp { flags, .. } if flags.contains('g')
        ) {
            return None;
        }
        Some(self.call_native(native::REGEXP_SYM_MATCHALL, arg0, &[recv]))
    }

    /// Guarded captured `RegExpMethod` for the two hot primitive-string /
    /// RegExp combinations: `s.matchAll(re)` and `s.replace(re, string)`.
    ///
    /// `Ok(None)` is a PURE decline.  Every type, realm, live method-slot and
    /// RegExp-protocol proof is complete before coercion, getters, writes,
    /// allocation, or matching.  The caller can therefore run the unchanged
    /// generic method route without replaying an effect.  `is_replace = false`
    /// selects matchAll; `true` selects replace.  `from_jit` is diagnostic only.
    pub(crate) fn string_regexp_call_direct(
        &mut self,
        callee: Value,
        recv: Value,
        arg0: Value,
        replacement: Value,
        is_replace: bool,
        from_jit: bool,
    ) -> Result<Option<Value>, Thrown> {
        let name = if is_replace { "replace" } else { "matchAll" };
        let s_idx = if recv.is_heap()
            && matches!(
                self.heap.get(recv.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. }
            ) {
            recv.heap_index()
        } else {
            super::proxy_regexp::rxstats::count_string_call_direct_decline();
            return Ok(None);
        };

        // EvaluateCall already performed the observable method Get. Validate
        // its captured result against the permanent main-realm identity; never
        // re-read the live prototype slot after argument effects.
        let op = if is_replace {
            crate::bytecode::RegExpMethod::Replace
        } else {
            crate::bytecode::RegExpMethod::MatchAll
        };
        if !self.captured_regexp_method_is_intrinsic(op, callee) {
            super::proxy_regexp::rxstats::count_string_call_direct_decline();
            return Ok(None);
        }

        let Some(re) = self.as_regexp(arg0) else {
            super::proxy_regexp::rxstats::count_string_call_direct_decline();
            return Ok(None);
        };

        if !is_replace {
            // Keep the older matchAll pristine mechanism's rollback contract:
            // disabling it also removes this direct route's final sub-proof.
            if !matchall_pristine_enabled() {
                super::proxy_regexp::rxstats::count_string_call_direct_decline();
                return Ok(None);
            }
            let Some(result) = self.matchall_pristine_dispatch(recv, arg0) else {
                super::proxy_regexp::rxstats::count_string_call_direct_decline();
                return Ok(None);
            };
            // The dispatcher has now committed to (and run) the intrinsic.  An
            // Err is still a served call and must be counted before propagation.
            super::builtins::builtin_stats_count(self, recv, name);
            super::proxy_regexp::rxstats::count_string_call_direct_hit(false, from_jit);
            return result.map(Some);
        }

        // A replacement object can run ToString (or be callable) at a spec point
        // that precedes the live `exec`/flags reads.  It could invalidate proofs
        // made above, so this lane accepts only already-primitive heap strings.
        if !replacement.is_heap()
            || !matches!(
                self.heap.get(replacement.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. }
            )
            // Conservative exact-instance proof: even an unrelated expando
            // sends the call to the fully observable protocol.
            || self.arr_props.get(&re).is_some()
            || !self.regexp_replace_fast_ok(re)
        {
            super::proxy_regexp::rxstats::count_string_call_direct_decline();
            return Ok(None);
        }

        let global = matches!(
            self.heap.get(re),
            HeapObj::RegExp { flags, .. } if flags.contains('g')
        );
        super::builtins::builtin_stats_count(self, recv, name);
        super::proxy_regexp::rxstats::count_string_call_direct_hit(true, from_jit);
        self.regex_replace(s_idx, re, replacement, global).map(Some)
    }

    /// UTF-16 unit length (JS `.length`) of a flat string by heap index — O(1).
    pub(crate) fn heap_str_units(&self, idx: u32) -> usize {
        match self.heap.get(idx) {
            HeapObj::Str(js) => js.units(),
            _ => 0,
        }
    }

    /// The UTF-16 code unit at unit position `i` (`charCodeAt`) — O(1) for
    /// ASCII (i-th byte), else an O(i) decode. `None` if out of range or not a
    /// flat string.
    pub(crate) fn heap_unit_at(&self, idx: u32, i: usize) -> Option<u16> {
        match self.heap.get(idx) {
            HeapObj::Str(js) => js.unit_at(i),
            _ => None,
        }
    }

    /// CodePointAt(unit position) per spec: the FULL code point at a lead
    /// unit, the trail surrogate's value in the middle of a pair.
    pub(crate) fn heap_code_point_at(&self, idx: u32, i: usize) -> Option<u32> {
        match self.heap.get(idx) {
            HeapObj::Str(js) => js.code_point_at(i),
            _ => None,
        }
    }

    /// The 1-unit string Value for code unit `u` (`charAt`/`at`/bracket
    /// index): an interned slot for ASCII, else a fresh 1-unit string — a REAL
    /// lone-surrogate string when `u` is a surrogate half.
    pub(crate) fn str_from_unit(&mut self, u: u16) -> Value {
        if u < 128 {
            return Value::heap(u as u32);
        }
        Value::heap(
            self.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(u as u32))),
        )
    }

    /// The string Value for code point `cp` (for-of / iterator steps): an
    /// interned slot for ASCII, else a fresh 1-code-point string (`cp` may be
    /// a lone surrogate).
    pub(crate) fn str_from_cp(&mut self, cp: u32) -> Value {
        if cp < 128 {
            return Value::heap(cp);
        }
        Value::heap(
            self.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(cp))),
        )
    }

    /// Allocate the receiver substring corresponding to subslice `t` of the
    /// receiver's LOSSY view `s` (e.g. a trim result): the lossy form is
    /// byte-length preserving, so `t`'s offsets address the same content in
    /// the EXACT WTF-8 bytes `js`. Well-formed receivers (the common case)
    /// just allocate `t`.
    fn alloc_recv_slice(&mut self, js: &crate::heap::JsStr, s: &str, t: &str) -> Value {
        if js.is_wellformed() {
            return self.alloc_str(t.to_string());
        }
        let start = t.as_ptr() as usize - s.as_ptr() as usize;
        let exact = crate::heap::JsStr::from_wtf8(js.as_bytes()[start..start + t.len()].to_vec());
        Value::heap(self.heap.alloc_js(exact))
    }

    /// Append ToString(`v`) to a WTF-8 buffer EXACTLY: a string value's own
    /// bytes are copied straight out of the heap (a lone surrogate survives,
    /// and no intermediate `String` is allocated per element), an object's
    /// ToString result goes the same way, and every other primitive renders as
    /// UTF-8, which is already WTF-8. A Symbol still throws, in ToString.
    ///
    /// This is the join/concat counterpart of `append_guest_string`: it is what
    /// makes `['\uD83D', '\uDE00'].join('')` the astral character it spells
    /// rather than two U+FFFDs.
    pub(crate) fn append_guest_tostring(
        &mut self,
        out: &mut Vec<u8>,
        v: Value,
    ) -> Result<(), Thrown> {
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            return self.append_guest_heap_str(out, v.heap_index());
        }
        if self.is_object_value(v) {
            let sv = self.to_str_value(v)?;
            return self.append_guest_heap_str(out, sv.heap_index());
        }
        let s = self.to_js_string(v)?;
        self.append_guest_wtf8(out, s.as_bytes())
    }

    /// Append a heap string's exact bytes to a WTF-8 buffer, admitted and
    /// reserved as [`Self::append_guest_wtf8`] does, but copied without an
    /// owned intermediate.
    pub(crate) fn append_guest_heap_str(
        &mut self,
        out: &mut Vec<u8>,
        idx: u32,
    ) -> Result<(), Thrown> {
        self.heap.flatten(idx);
        let len = match self.heap.get(idx) {
            HeapObj::Str(js) => js.as_bytes().len(),
            _ => 0,
        };
        if len == 0 {
            return Ok(());
        }
        let total = out
            .len()
            .checked_add(len)
            .filter(|&n| n <= MAX_STRING_BYTES)
            .ok_or_else(invalid_string_length)?;
        #[cfg(feature = "instrument")]
        self.instrument_preflight_heap_growth(total)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(not(feature = "instrument"))]
        let _ = total;
        out.try_reserve(len)
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        if let HeapObj::Str(js) = self.heap.get(idx) {
            crate::heap::wtf8_push(out, js.as_bytes());
        }
        Ok(())
    }

    /// Allocate a WTF-8 buffer built by the appenders above as a guest string.
    pub(crate) fn alloc_wtf8(&mut self, out: Vec<u8>) -> Value {
        Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out)))
    }

    /// Append guest-derived text without allowing a native helper to build an
    /// unbounded Rust `String` between VM meter polls. `total` is charged as an
    /// external in-flight allocation because `out` is not in the VM heap until
    /// it is moved into `alloc_str`.
    pub(crate) fn append_guest_string(
        &mut self,
        out: &mut String,
        text: &str,
    ) -> Result<(), Thrown> {
        let total = out
            .len()
            .checked_add(text.len())
            .filter(|&n| n <= MAX_STRING_BYTES)
            .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
        #[cfg(feature = "instrument")]
        self.instrument_preflight_heap_growth(total)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(not(feature = "instrument"))]
        let _ = total;
        out.try_reserve(text.len())
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        out.push_str(text);
        Ok(())
    }

    /// Admit a complete guest-visible string result before constructing it.
    /// Native helpers use this when a cheap first pass can determine the exact
    /// size, preventing a large unmetered temporary between interpreter polls.
    pub(crate) fn preflight_guest_string_size(&mut self, total: usize) -> Result<(), Thrown> {
        if total > MAX_STRING_BYTES {
            return Err(Thrown("RangeError: Invalid string length".into()));
        }
        #[cfg(feature = "instrument")]
        self.instrument_preflight_heap_growth(total)
            .map_err(|message| Thrown(message.into()))?;
        Ok(())
    }

    /// Create a fallibly allocated result buffer after applying the guest
    /// string cap and instrumentation heap budget.
    pub(crate) fn guest_string_with_capacity(&mut self, total: usize) -> Result<String, Thrown> {
        self.preflight_guest_string_size(total)?;
        let mut out = String::new();
        out.try_reserve_exact(total)
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        Ok(out)
    }

    pub(crate) fn append_guest_join_part(
        &mut self,
        out: &mut String,
        separator: &str,
        part: &str,
        index: usize,
    ) -> Result<(), Thrown> {
        if index != 0 {
            self.append_guest_string(out, separator)?;
        }
        self.append_guest_string(out, part)
    }

    pub(crate) fn string_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::StringOps);
        self.heap.flatten(idx); // materialize a rope receiver before reading it
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // Single-char index methods: read one char directly from the heap with NO
        // full-string clone (the clone below is O(n), so these would be O(n²) in a
        // per-char loop — `s.charCodeAt(i)` scanning is a very common idiom).
        // ── no-clone search methods ──
        // The generic path further down does `js.clone()` — a full copy of the
        // RECEIVER — purely to release the `self.heap` borrow before it can
        // allocate a result. These five allocate nothing (they return a number or
        // a boolean), so they run under a plain immutable borrow of both
        // operands. That clone was essentially the entire cost of a string method
        // call: `s.indexOf(t)` on an 85-char subject measured ~90ns against
        // node's ~3ns, while `charCodeAt`/`length` — which never reach here —
        // were already at parity.
        //
        // Restricted to the shapes where byte offsets equal UTF-16 unit offsets
        // and no coercion is observable: both operands already ASCII heap
        // strings, and no second argument (`fromIndex`/`position` change the
        // answer and go the general way). A RegExp argument can never match
        // `HeapObj::Str`, so the `includes`/`startsWith`/`endsWith` TypeError
        // still comes from the general path.
        if args.len() <= 1
            && arg0.is_heap()
            && matches!(
                name,
                "indexOf" | "lastIndexOf" | "includes" | "startsWith" | "endsWith"
            )
        {
            if let (HeapObj::Str(hay), HeapObj::Str(ned)) =
                (self.heap.get(idx), self.heap.get(arg0.heap_index()))
            {
                if hay.is_ascii() && ned.is_ascii() {
                    let (hc, nc) = (hay.as_str_lossy(), ned.as_str_lossy());
                    let (h, n): (&str, &str) = (&hc, &nc);
                    return Ok(Some(match name {
                        "indexOf" => Value::int(h.find(n).map_or(-1, |b| b as i32)),
                        "lastIndexOf" => Value::int(h.rfind(n).map_or(-1, |b| b as i32)),
                        "includes" => Value::bool(h.contains(n)),
                        "startsWith" => Value::bool(h.starts_with(n)),
                        _ => Value::bool(h.ends_with(n)),
                    }));
                }
            }
        }
        match name {
            "charCodeAt" => {
                let i = self.to_integer_strict(arg0)?;
                let u = if i >= 0 {
                    self.heap_unit_at(idx, i as usize)
                } else {
                    None
                };
                return Ok(Some(match u {
                    Some(u) => Value::int(u as i32),
                    None => Value::num(f64::NAN),
                }));
            }
            "codePointAt" => {
                let i = self.to_integer_strict(arg0)?;
                let c = if i >= 0 {
                    self.heap_code_point_at(idx, i as usize)
                } else {
                    None
                };
                return Ok(Some(match c {
                    Some(cp) => Value::int(cp as i32),
                    None => Value::UNDEFINED,
                }));
            }
            "charAt" => {
                let i = self.to_integer_strict(arg0)?;
                let u = if i >= 0 {
                    self.heap_unit_at(idx, i as usize)
                } else {
                    None
                };
                return Ok(Some(match u {
                    Some(u) => self.str_from_unit(u),
                    None => Value::heap(crate::heap::INTERN_EMPTY),
                }));
            }
            "at" => {
                let len = self.heap_str_units(idx) as i64;
                let i = self.to_integer_strict(arg0)?;
                let abs = if i < 0 { i + len } else { i };
                let u = if abs >= 0 && abs < len {
                    self.heap_unit_at(idx, abs as usize)
                } else {
                    None
                };
                return Ok(Some(match u {
                    Some(u) => self.str_from_unit(u),
                    None => Value::UNDEFINED,
                }));
            }
            // No-clone substring/slice: produce the O(slice) result by borrowing
            // the receiver and slicing its WTF-8 directly — skipping the two
            // full-receiver copies (`js.clone()` + `to_lossy_string()`) the generic
            // path below makes. Hot in string-rendering / scanning loops.
            "slice" => {
                // Negative indices count from the end (i64 so a saturated
                // ±Infinity clamps correctly); absent/undefined end -> length.
                let len = self.heap_str_units(idx) as i64;
                let norm = |i: i64| {
                    if i < 0 {
                        len.saturating_add(i).max(0)
                    } else {
                        i.min(len)
                    }
                };
                let start = if args.is_empty() {
                    0
                } else {
                    norm(self.to_integer_strict(arg0)?)
                };
                let end = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    len
                } else {
                    norm(self.to_integer_strict(args[1])?)
                };
                // Arg coercion (valueOf) is complete; borrow the receiver, slice,
                // and DROP the borrow before alloc_js (which may GC). `idx` is the
                // rooted receiver and stays valid across the coercion above.
                let out = match self.heap.get(idx) {
                    HeapObj::Str(js) => js.slice_units(start as usize, end as usize),
                    _ => return Ok(None),
                };
                return Ok(Some(Value::heap(self.heap.alloc_js(out))));
            }
            "substring" => {
                // Each index clamps to [0,len] (negatives -> 0), then start/end
                // swap so start <= end (distinct from slice's negative-from-end).
                let len = self.heap_str_units(idx) as i64;
                let s0 = if args.is_empty() {
                    0
                } else {
                    self.to_integer_strict(arg0)?.clamp(0, len)
                };
                let e0 = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    len
                } else {
                    self.to_integer_strict(args[1])?.clamp(0, len)
                };
                let (from, to) = if s0 <= e0 { (s0, e0) } else { (e0, s0) };
                let out = match self.heap.get(idx) {
                    HeapObj::Str(js) => js.slice_units(from as usize, to as usize),
                    _ => return Ok(None),
                };
                return Ok(Some(Value::heap(self.heap.alloc_js(out))));
            }
            // The rest of the search family (a position argument, a non-ASCII
            // or non-string operand) also allocates nothing, so it reads both
            // operands in place instead of taking the receiver copy below.
            "indexOf" | "lastIndexOf" | "includes" | "startsWith" | "endsWith" => {
                if !matches!(self.heap.get(idx), HeapObj::Str(_)) {
                    return Ok(None);
                }
                return self.string_search(idx, name, args).map(Some);
            }
            _ => {}
        }
        // Other methods need owned content (slice/replace/split/…): the exact
        // WTF-8 form `js_recv` (position math, slicing — surrogate-exact) and a
        // LOSSY `&str` view `s` for the byte-oriented search/Unicode paths.
        // The lossy form replaces each lone surrogate with U+FFFD — SAME byte
        // length — so byte offsets/unit positions computed on `s` are valid for
        // `js_recv` too. For a well-formed receiver (the overwhelmingly common
        // case) `s` IS the exact content.
        let (js_recv, ascii) = match self.heap.get(idx) {
            HeapObj::Str(js) => (js.clone(), js.is_ascii()),
            _ => return Ok(None),
        };
        // BORROW the lossy view rather than copying it. `as_str_lossy` is
        // `Cow::Borrowed` whenever the receiver is well-formed — i.e. always,
        // outside lone-surrogate strings — so this turns a second full copy of
        // the receiver into a pointer. Every string method reaching this point
        // paid it: `s.indexOf(t)` on an 880-char subject was copying 880 bytes
        // per call on top of the `js.clone()` above.
        let s_cow = js_recv.as_str_lossy();
        let s: &str = &s_cow;
        // JS positions/lengths are UTF-16 code units; `ascii` short-circuits the
        // walks (unit == byte). The closure takes the RECEIVER `s` only.
        let unit_len = |s: &str| -> usize {
            if ascii {
                s.len()
            } else {
                crate::heap::str_units(s)
            }
        };
        // Substring by unit positions [a, b) — EXACT (slices the WTF-8 bytes;
        // a bound splitting a surrogate pair keeps the REAL covered half).
        let subu = |a: usize, b: usize| -> crate::heap::JsStr { js_recv.slice_units(a, b) };
        match name {
            "toUpperCase" => {
                let mapped_len = case_map_exact_len(js_recv.as_bytes(), true)?;
                self.preflight_guest_string_size(mapped_len)?;
                Ok(Some(Value::heap(self.heap.alloc_js(case_map_exact(
                    js_recv.as_bytes(),
                    true,
                    mapped_len,
                )?))))
            }
            "toLowerCase" => {
                let mapped_len = case_map_exact_len(js_recv.as_bytes(), false)?;
                self.preflight_guest_string_size(mapped_len)?;
                Ok(Some(Value::heap(self.heap.alloc_js(case_map_exact(
                    js_recv.as_bytes(),
                    false,
                    mapped_len,
                )?))))
            }
            // NB: `slice` / `substring` are handled by the no-clone fast path in
            // the early match above (before the receiver is copied).
            "repeat" => {
                // ToIntegerOrInfinity(count): a NEGATIVE or +Infinity count is a
                // RangeError — checked on the coerced number BEFORE the empty-string
                // fast path (`"".repeat(Infinity)` must still throw, not yield "").
                let nf = self.to_number_strict(arg0)?;
                let n_int = if nf.is_nan() { 0.0 } else { nf.trunc() };
                if n_int < 0.0 || n_int == f64::INFINITY {
                    return Err(Thrown("RangeError: Invalid count value".into()));
                }
                // Bound the result (an unbounded build would hang / OOM): a too-long
                // string is a RangeError per spec. (n_int is now finite and ≥ 0.)
                let result_bytes = n_int * (s.len() as f64);
                if result_bytes > MAX_STRING_BYTES as f64 {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                #[cfg(feature = "instrument")]
                self.instrument_preflight_heap_growth(result_bytes as usize)
                    .map_err(|message| Thrown(message.into()))?;
                if js_recv.is_wellformed() {
                    return Ok(Some(self.alloc_str(s.repeat(n_int as usize))));
                }
                // Non-well-formed receiver: repeat the EXACT bytes, with seam
                // canonicalization — '\uDC00\uD800'.repeat(2) forms a real
                // astral scalar at each junction (UTF-16 unit semantics).
                let mut out: Vec<u8> =
                    Vec::with_capacity(js_recv.as_bytes().len() * n_int as usize);
                for _ in 0..n_int as usize {
                    crate::heap::wtf8_push(&mut out, js_recv.as_bytes());
                }
                let js = crate::heap::JsStr::from_wtf8(out);
                Ok(Some(Value::heap(self.heap.alloc_js(js))))
            }
            "search" => {
                // Per spec, an OBJECT regexp's `@@search` method overrides the
                // default (a real RegExp's RegExp.prototype[@@search] is found here
                // too, routing through the same regexp_search_impl). A primitive
                // argument is NOT consulted — it builds a RegExp. Mirrors `matchAll`.
                if self.is_object_value(arg0) {
                    let searcher = self.get_prop(arg0, "@@search")?;
                    if searcher != Value::UNDEFINED && searcher != Value::NULL {
                        return Ok(Some(self.call_value(
                            searcher,
                            arg0,
                            &[Value::heap(idx)],
                        )?));
                    }
                }
                // Build a RegExp from the (non-object) argument, then Invoke its
                // @@search — honouring a monkeypatched RegExp.prototype[@@search]
                // (the unpatched native routes through regexp_search_impl, same result).
                let rxv = Value::heap(self.to_regexp_arg(arg0)?);
                let searcher = self.get_prop(rxv, "@@search")?;
                Ok(Some(self.call_value(searcher, rxv, &[Value::heap(idx)])?))
            }
            "match" => {
                // An OBJECT regexp's `@@match` overrides the default (a real RegExp's
                // RegExp.prototype[@@match] routes through the same regexp_match_impl);
                // a primitive argument builds a RegExp. Mirrors `matchAll`.
                if self.is_object_value(arg0) {
                    let matcher = self.get_prop(arg0, "@@match")?;
                    if matcher != Value::UNDEFINED && matcher != Value::NULL {
                        return Ok(Some(self.call_value(matcher, arg0, &[Value::heap(idx)])?));
                    }
                }
                // Build a RegExp, then Invoke its @@match — honouring a monkeypatched
                // RegExp.prototype[@@match] (the unpatched native routes through
                // regexp_match_impl, same result).
                let rxv = Value::heap(self.to_regexp_arg(arg0)?);
                let matcher = self.get_prop(rxv, "@@match")?;
                Ok(Some(self.call_value(matcher, rxv, &[Value::heap(idx)])?))
            }
            "matchAll" => {
                // ── pristine dispatch shortcut (the B77 retry) ── the
                // primitive-receiver path (`"s".matchAll(re)`) lands HERE, not
                // in string_symbol_method; same proof, same fall-through.
                if matchall_pristine_enabled() {
                    if let Some(r) = self.matchall_pristine_dispatch(Value::heap(idx), arg0) {
                        return r.map(Some);
                    }
                }
                let regexp = arg0;
                let s_val = Value::heap(idx);
                // Per spec the `@@matchAll` method is only consulted when `regexp`
                // is an OBJECT — a primitive argument must NOT trigger a
                // `Number.prototype[@@matchAll]` getter etc. (it builds a RegExp).
                if self.is_object_value(regexp) {
                    // A real RegExp argument must be global (spec: IsRegExp +
                    // RequireObjectCoercible(flags) + 'g' check).
                    //
                    // IsRegExp is OBSERVABLE: it reads `@@match` and ToBoolean's
                    // the result, and that read must happen BEFORE Get(flags).
                    // `as_regexp(..).is_some()` answered the same question for a
                    // plain RegExp while performing no property lookup at all, so
                    // on the PRIMITIVE-receiver path (`"s".matchAll(re)`, which
                    // lands here rather than in string_symbol_method) a user
                    // `@@match` getter never fired — staging/sm/String/matchAll.js
                    // counts those calls and requires exactly two. It was also
                    // wrong for a non-RegExp object carrying a truthy `@@match`,
                    // which the spec still requires to be global.
                    if self.is_regexp(regexp)? {
                        let flags_v = self.get_prop(regexp, "flags")?;
                        let flags = self.to_js_string(flags_v)?;
                        if !flags.contains('g') {
                            return Err(Thrown(
                                "TypeError: String.prototype.matchAll called with a non-global RegExp argument".into(),
                            ));
                        }
                    }
                    let matcher = self.get_prop(regexp, "@@matchAll")?;
                    if matcher != Value::UNDEFINED && matcher != Value::NULL {
                        return Ok(Some(self.call_value(matcher, regexp, &[s_val])?));
                    }
                }
                // Otherwise build a fresh global RegExp and use its @@matchAll.
                let gflag = self.alloc_str("g".to_string());
                let rx = self.build_regexp(regexp, gflag)?;
                let matcher = self.get_prop(rx, "@@matchAll")?;
                Ok(Some(self.call_value(matcher, rx, &[s_val])?))
            }
            // The internal splitter IS `RegExp.prototype[@@split]`, so taking it
            // directly is unobservable only while that is the method a
            // `GetMethod(re, @@split)` would find (`regexp_split_fast_ok`). A
            // user `@@split` — own, on the prototype, or on a subclass — takes
            // the generic `"split"` arm below, which performs the lookup.
            "split" if self.as_regexp(arg0).is_some_and(|re| self.regexp_split_fast_ok(re)) => {
                let re = self.as_regexp(arg0).unwrap();
                let limit = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                Ok(Some(self.regexp_split_impl(re, Value::heap(idx), limit)?))
            }
            "replace" if self.as_regexp(arg0).is_some() => {
                let re = self.as_regexp(arg0).unwrap();
                let repl = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // The internal fast path is valid only for a PLAIN regex: its
                // [[Prototype]] is %RegExp.prototype% and exec/@@replace are
                // still the intrinsics. A SUBCLASS instance (overridden exec)
                // or a patched prototype must run the OBSERVABLE @@replace
                // protocol — user exec result, `groups` via Get (incl. the
                // prototype chain), GetSubstitution $<name> via Get.
                // An object replacement's ToString (or callable invocation)
                // participates in the observable @@replace ordering.  In
                // particular `repl.toString()` may install `re.exec` before the
                // first RegExpExec lookup.  The internal matcher proves exec
                // before it coerces the replacement, so reserve it for an
                // already-primitive string and send every effectful shape to
                // the full Symbol.replace protocol below.
                let primitive_string = repl.is_heap()
                    && matches!(
                        self.heap.get(repl.heap_index()),
                        HeapObj::Str(_) | HeapObj::Cons { .. }
                    );
                if primitive_string && self.regexp_replace_fast_ok(re) {
                    let global = matches!(
                        self.heap.get(re),
                        HeapObj::RegExp { flags, .. } if flags.contains('g')
                    );
                    Ok(Some(self.regex_replace(idx, re, repl, global)?))
                } else {
                    Ok(Some(self.string_replace_plain(idx, &js_recv, arg0, repl, false)?))
                }
            }
            // `replaceAll` (regexp or otherwise) funnels into `string_replace_plain`,
            // which performs the spec step-2 checks (IsRegExp → global-flag, GetMethod
            // @@replace) with the proper observable Get/ToString semantics. (Routing a
            // real RegExp through the @@replace protocol here, rather than the internal
            // regex_replace, is what makes a custom `flags`/`@@match`/`@@replace`
            // observable.)
            "split" => {
                // A custom @@split fully overrides the default algorithm and runs
                // FIRST — before any ToString / ToUint32 — receiving the receiver
                // and the RAW limit (it does its own coercion). (RegExp's @@split is
                // wired here too.) Only an OBJECT separator is consulted: a
                // primitive separator's @@split is NOT accessed (test262
                // cstm-split-on-*-primitive), it is just ToString'd as a delimiter.
                if self.is_object_value(arg0) {
                    let m = self.get_prop(arg0, "@@split")?;
                    if self.is_callable(m) {
                        let limit_raw = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                        return Ok(Some(self.call_value(
                            m,
                            arg0,
                            &[Value::heap(idx), limit_raw],
                        )?));
                    }
                    // GetMethod step 3: a present-but-NOT-CALLABLE @@split is a
                    // TypeError, not a silent fall-through to the default
                    // algorithm — `"a-a".split({[Symbol.split]: 1, toString(){…}})`
                    // must throw (staging/sm/String/split-GetMethod.js).
                    // undefined/null alone mean "no splitter".
                    if !m.is_nullish() {
                        return Err(Thrown(
                            "TypeError: Symbol.split method is not a function".into(),
                        ));
                    }
                }
                // lim = ToUint32(ToNumber(limit)) — runs valueOf/@@toPrimitive and
                // propagates a throw; `undefined` → no cap.
                let lim = match args.get(1).copied() {
                    Some(v) if v != Value::UNDEFINED => {
                        crate::vm::helpers_num2::to_uint32(self.to_number_strict(v)?) as usize
                    }
                    _ => usize::MAX,
                };
                // ToString(separator) — runs a user toString (propagating a
                // throw) and rejects a Symbol; after ToUint32(limit) and before
                // the lim==0 early-out, matching the spec ordering. Hoisted
                // ABOVE the pretenure scope below so no user code (and no `?`)
                // runs inside it. Taken as a string VALUE, so a lone-surrogate
                // separator is itself and not U+FFFD.
                let sep_or = if args.is_empty() || arg0 == Value::UNDEFINED {
                    None
                } else {
                    Some(self.to_js_str_owned(arg0)?)
                };
                // Every part is located and admitted before anything is
                // allocated. A single split could otherwise build millions of
                // parts inside one instruction, past the heap meter, and a
                // failed infallible allocation traps a WebAssembly host.
                let ranges = match &sep_or {
                    Some(sep) if lim != 0 && sep.units() != 0 => {
                        Some(self.split_ranges(&js_recv, &s, sep, lim)?)
                    }
                    Some(_) if lim != 0 => {
                        let count = js_recv.units().min(lim);
                        let bytes = if ascii { 0 } else { js_recv.as_bytes().len() };
                        self.split_admit(count, bytes)?;
                        None
                    }
                    _ => None,
                };
                let count = match (&sep_or, &ranges) {
                    (_, Some((r, _))) => r.len(),
                    (Some(_), None) if lim != 0 => js_recv.units().min(lim),
                    _ => 1,
                };
                let mut parts: Vec<Value> = Vec::new();
                parts
                    .try_reserve_exact(count)
                    .map_err(|_| Thrown("RangeError: Invalid array length".into()))?;
                // W9 static pretenure (NURSERY_DESIGN.md §4): split's parts and
                // result array are the markdown/regex rows' retained "builder"
                // output — measured to survive minors wholesale, so they
                // allocate OLD. Purely internal from here down: nothing throws
                // and no user code runs, so the begin/end pair is airtight.
                self.heap.pretenure_begin();
                let parts: Vec<Value> = match &sep_or {
                    // No separator → the whole string as a single element (lim 0
                    // → []). The receiver itself — exact, strings are immutable.
                    None => {
                        if lim != 0 {
                            parts.push(Value::heap(idx));
                        }
                        parts
                    }
                    Some(_) => {
                        match &ranges {
                            // Split into 1-UNIT pieces (spec: code units). An
                            // astral scalar's halves are REAL lone-surrogate
                            // strings.
                            None => {
                                if lim != 0 {
                                    for u in js_recv.units_iter().take(lim) {
                                        parts.push(self.str_from_unit(u));
                                    }
                                }
                            }
                            // Byte ranges of the exact bytes.
                            Some((ranges, false)) => {
                                for &(a, b) in ranges {
                                    let js = crate::heap::JsStr::from_wtf8(
                                        js_recv.as_bytes()[a..b].to_vec(),
                                    );
                                    parts.push(Value::heap(self.heap.alloc_js(js)));
                                }
                            }
                            // Unit ranges: a bound between the halves of a pair
                            // keeps the real half.
                            Some((ranges, true)) => {
                                for &(a, b) in ranges {
                                    let js = js_recv.slice_units(a, b);
                                    parts.push(Value::heap(self.heap.alloc_js(js)));
                                }
                            }
                        }
                        parts
                    }
                };
                let arr = Value::heap(self.heap.alloc(HeapObj::Array(parts)));
                self.heap.pretenure_end();
                if sep_or.is_none() && lim != 0 {
                    // The one element is the pre-existing receiver, not a part
                    // allocated in the scope: an OLD-born array holding a
                    // possibly-young string is an old->young edge no store saw.
                    self.heap.write_barrier_val(arr.heap_index(), Value::heap(idx));
                }
                Ok(Some(arr))
            }
            // ECMAScript TrimString whitespace is `str_white_space`: Unicode
            // White_Space plus U+FEFF (ZWNBSP/BOM) and MINUS U+0085 (NEL),
            // which is not ECMAScript whitespace at all — `char::is_whitespace`
            // alone trimmed a NEL the spec keeps. The trim is computed on the
            // lossy view (U+FFFD is not whitespace, neither are surrogates) and
            // the result sliced from the EXACT bytes at the same offsets.
            "trim" => {
                let t = s.trim_matches(crate::vm::helpers_numeric::str_white_space);
                Ok(Some(self.alloc_recv_slice(&js_recv, &s, t)))
            }
            "trimStart" => {
                let t = s.trim_start_matches(crate::vm::helpers_numeric::str_white_space);
                Ok(Some(self.alloc_recv_slice(&js_recv, &s, t)))
            }
            "trimEnd" => {
                let t = s.trim_end_matches(crate::vm::helpers_numeric::str_white_space);
                Ok(Some(self.alloc_recv_slice(&js_recv, &s, t)))
            }
            "concat" => {
                // Each argument is ToString-coerced (honours @@toPrimitive/toString/
                // valueOf, throws on a Symbol), not rendered via display(). Built
                // as WTF-8 with seam canonicalization: a string argument joins
                // EXACTLY (its lone surrogates survive, and a trailing high +
                // leading low across arguments merges into the astral scalar).
                // Every append is charged against the guest string cap BEFORE
                // it happens: `s = s.concat(s)` doubles each round, and an
                // unbounded build asked the allocator for 137 GB and ABORTED
                // the process where the spec (and node) raise RangeError.
                let mut out: Vec<u8> = js_recv.as_bytes().to_vec();
                for a in args {
                    let av = *a;
                    if av.is_heap() && self.heap.is_str_like(av.heap_index()) {
                        let part = self
                            .heap
                            .str_wtf8_cow(av.heap_index())
                            .map(|c| c.into_owned())
                            .unwrap_or_default();
                        self.preflight_guest_string_size(out.len().saturating_add(part.len()))?;
                        crate::heap::wtf8_push(&mut out, &part);
                    } else if self.is_object_value(av) {
                        // An object's ToString result is kept as the string
                        // VALUE it is (a `toString` returning a lone surrogate).
                        let sv = self.to_str_value(av)?;
                        let part = self
                            .heap
                            .str_wtf8_cow(sv.heap_index())
                            .map(|c| c.into_owned())
                            .unwrap_or_default();
                        self.preflight_guest_string_size(out.len().saturating_add(part.len()))?;
                        crate::heap::wtf8_push(&mut out, &part);
                    } else {
                        let part = self.to_js_string(av)?;
                        self.preflight_guest_string_size(out.len().saturating_add(part.len()))?;
                        crate::heap::wtf8_push(&mut out, part.as_bytes());
                    }
                }
                let js = crate::heap::JsStr::from_wtf8(out);
                Ok(Some(Value::heap(self.heap.alloc_js(js))))
            }
            "substr" => {
                // Legacy substr(start, length); negative start counts from the end.
                let len = unit_len(&s) as i64;
                let mut start = if args.is_empty() {
                    0
                } else {
                    self.to_integer_strict(arg0)?
                };
                if start < 0 {
                    start = (len + start).max(0);
                }
                let start = start.min(len) as usize;
                let avail = len as usize - start;
                let count = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    avail
                } else {
                    let c = self.to_integer_strict(args[1])?;
                    if c < 0 {
                        0
                    } else {
                        (c as usize).min(avail)
                    }
                };
                let sub = subu(start, start + count);
                Ok(Some(Value::heap(self.heap.alloc_js(sub))))
            }
            "localeCompare" => {
                // ECMA-402 defines this as `Intl.Collator(locales, options)
                // .compare(this, that)` — so it is routed through a real
                // Collator rather than reimplemented. Two things follow that the
                // old standalone NFC comparison got wrong: the `locales`/
                // `options` arguments are validated (a bad tag or an invalid
                // `sensitivity` throws exactly what the constructor throws), and
                // the ordering cannot drift from `Intl.Collator.prototype.compare`
                // (`localeCompare/{throws-same-exceptions-as,returns-same-results-as}
                // -Collator.js`).
                //
                // `that` is ToString'd to a string VALUE and copied out before
                // the Collator's options can run user code.
                let that = self.to_js_str_owned(arg0)?;
                let locales = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let options = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let coll = self.make_intl(crate::vm::native::INTL_COLLATOR, locales, options)?;
                let resolved = self.intl_this(coll, crate::vm::native::INTL_COLLATOR, "compare")?;
                let (a, b) = (collation_view(&js_recv), collation_view(&that));
                let ord = self.collator_compare(resolved, &a, &b)?;
                Ok(Some(Value::int(ord as i32)))
            }
            "normalize" => {
                // Validate the form; engine strings are already normalized for ASCII
                // (full Unicode normalization isn't modelled).
                // ToString(form) runs (and may throw — TypeError for a Symbol, or a
                // propagated toString error) BEFORE the form-name validation, per
                // spec steps 5-7. `display` is infallible and skips toString, so it
                // wrongly turned those into the RangeError below.
                let form = if args.is_empty() || arg0 == Value::UNDEFINED {
                    "NFC".to_string()
                } else {
                    self.to_js_string(arg0)?
                };
                if !matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
                    return Err(Thrown(
                        "RangeError: The normalization form should be one of NFC, NFD, NFKC, NFKD."
                            .into(),
                    ));
                }
                if !js_recv.is_wellformed() {
                    // The lossy view shows each lone surrogate as U+FFFD, and
                    // the result kept it. Normalize each well-formed run of the
                    // exact bytes and copy the surrogates through: a surrogate
                    // is a starter with no decomposition, so it blocks
                    // reordering and composition exactly as a run boundary.
                    let bytes = js_recv.as_bytes();
                    let mut out_len = 0usize;
                    for run in wtf8_runs(bytes) {
                        match run {
                            Ok(r) => for_each_normalized(&form, r, &mut |c| out_len += c.len_utf8()),
                            Err(b) => out_len += b.len(),
                        }
                        if out_len > MAX_STRING_BYTES {
                            return Err(invalid_string_length());
                        }
                    }
                    self.preflight_guest_string_size(out_len)?;
                    let mut out: Vec<u8> = Vec::new();
                    out.try_reserve_exact(out_len)
                        .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
                    for run in wtf8_runs(bytes) {
                        match run {
                            Ok(r) => for_each_normalized(&form, r, &mut |c| {
                                let mut buf = [0u8; 4];
                                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                            }),
                            Err(b) => out.extend_from_slice(b),
                        }
                    }
                    let js = crate::heap::JsStr::from_wtf8(out);
                    return Ok(Some(Value::heap(self.heap.alloc_js(js))));
                }
                use unicode_normalization::UnicodeNormalization;
                let out_len = match form.as_str() {
                    "NFC" => checked_char_output_len(s.nfc())?,
                    "NFD" => checked_char_output_len(s.nfd())?,
                    "NFKC" => checked_char_output_len(s.nfkc())?,
                    _ => checked_char_output_len(s.nfkd())?,
                };
                let mut out = self.guest_string_with_capacity(out_len)?;
                match form.as_str() {
                    "NFC" => extend_chars(&mut out, s.nfc()),
                    "NFD" => extend_chars(&mut out, s.nfd()),
                    "NFKC" => extend_chars(&mut out, s.nfkc()),
                    _ => extend_chars(&mut out, s.nfkd()),
                }
                debug_assert_eq!(out.len(), out_len);
                Ok(Some(self.alloc_str(out)))
            }
            // Real well-formedness: the WTF-8 representation tracks lone
            // surrogates, and the flag is computed once at construction (O(1)).
            "isWellFormed" => Ok(Some(Value::bool(js_recv.is_wellformed()))),
            // `s` is the lossy view — each lone surrogate already replaced with
            // U+FFFD, which is EXACTLY the ToWellFormed result. A well-formed
            // receiver returns itself (identity is unobservable for strings).
            "toWellFormed" => Ok(Some(if js_recv.is_wellformed() {
                Value::heap(idx)
            } else {
                self.alloc_str(s.to_string())
            })),
            // String.prototype.valueOf/toString return the string primitive itself
            // (used by a boxed String's valueOf/toString after unwrapping).
            "valueOf" | "toString" => Ok(Some(Value::heap(idx))),
            "padStart" | "padEnd" => {
                let cur = unit_len(&s);
                let t = self.to_integer_strict(arg0)?;
                let target = if t > 0 { t as usize } else { 0 };
                if target > MAX_STRING_UNITS {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                if cur >= target {
                    return Ok(Some(Value::heap(idx)));
                }
                // ToString(fillString) — a Symbol/abrupt fill throws (after the
                // length early-return above, matching the spec's StringPad order).
                // The filler is taken as a string VALUE (its own lone
                // surrogates, or those of a `toString` result, survive).
                let fill_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pad: crate::heap::JsStr = if fill_arg != Value::UNDEFINED {
                    self.to_js_str_owned(fill_arg)?
                } else {
                    crate::heap::JsStr::new(" ".to_string())
                };
                if pad.units() == 0 {
                    return Ok(Some(Value::heap(idx)));
                }
                // StringPad truncates the repeated filler to (target - cur)
                // UNITS; a truncation that splits an astral filler char keeps
                // the REAL lead half (a 1-unit lone high surrogate).
                let need = target - cur;
                let (whole, tail) = (need / pad.units(), pad.slice_units(0, need % pad.units()));
                // The unit cap above bounds units, not bytes: a 3-byte filler
                // character made one call build up to 3x MAX_STRING_BYTES in an
                // unmetered buffer. Admit the exact size first (an upper bound:
                // a seam that pairs two surrogate halves saves two bytes), and
                // build into that one reservation.
                let total = whole
                    .checked_mul(pad.as_bytes().len())
                    .and_then(|n| n.checked_add(tail.as_bytes().len()))
                    .and_then(|n| n.checked_add(js_recv.as_bytes().len()))
                    .ok_or_else(invalid_string_length)?;
                self.preflight_guest_string_size(total)?;
                let mut out: Vec<u8> = Vec::new();
                out.try_reserve_exact(total)
                    .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
                // Joined as WTF-8: a seam may canonicalize (a filler ending in a
                // high surrogate against a receiver starting with a low one).
                if name == "padEnd" {
                    out.extend_from_slice(js_recv.as_bytes());
                }
                for _ in 0..whole {
                    crate::heap::wtf8_push(&mut out, pad.as_bytes());
                }
                crate::heap::wtf8_push(&mut out, tail.as_bytes());
                if name == "padStart" {
                    crate::heap::wtf8_push(&mut out, js_recv.as_bytes());
                }
                let js = crate::heap::JsStr::from_wtf8(out);
                Ok(Some(Value::heap(self.heap.alloc_js(js))))
            }
            "replace" => {
                let r = self.string_replace_plain(
                    idx,
                    &js_recv,
                    arg0,
                    args.get(1).copied().unwrap_or(Value::UNDEFINED),
                    false,
                )?;
                Ok(Some(r))
            }
            "replaceAll" => {
                let r = self.string_replace_plain(
                    idx,
                    &js_recv,
                    arg0,
                    args.get(1).copied().unwrap_or(Value::UNDEFINED),
                    true,
                )?;
                Ok(Some(r))
            }
            // TransformCase (ECMA-402): CanonicalizeLocaleList first (so a
            // structurally invalid tag is a RangeError), then BestAvailableLocale
            // over "the languages for which the UCD contains language sensitive
            // case mappings" — az, lt, tr. Anything else, including no argument
            // at all, is "und" and takes the locale-independent mapping.
            "toLocaleUpperCase" | "toLocaleLowerCase" => {
                let locales = self.canonicalize_locale_list(arg0)?;
                let upper = name == "toLocaleUpperCase";
                let lang = locales
                    .first()
                    .and_then(|t| crate::vm::special_casing::special_casing_language(t));
                // Both mappings size the result exactly and admit it before
                // building, over the EXACT bytes. A case mapping can triple a
                // string (U+0390 uppercases to three code points); building it
                // straight into an unchecked `String` went past
                // MAX_STRING_BYTES and trapped a WebAssembly host, where
                // toUpperCase threw RangeError. Lone surrogates have no mapping
                // and copy through, as in toUpperCase/toLowerCase.
                let bytes = js_recv.as_bytes();
                let out = match lang {
                    // "und": the locale-independent mapping.
                    None => {
                        let mapped_len = case_map_exact_len(bytes, upper)?;
                        self.preflight_guest_string_size(mapped_len)?;
                        case_map_exact(bytes, upper, mapped_len)?
                    }
                    Some(lang) => {
                        let mapped_len = special_case_exact_len(bytes, lang, upper);
                        self.preflight_guest_string_size(mapped_len)?;
                        special_case_exact(bytes, lang, upper, mapped_len)?
                    }
                };
                Ok(Some(Value::heap(self.heap.alloc_js(out))))
            }
            // Annex B HTML wrapper methods (B.2.3): wrap the string in a tag, with
            // the attribute value's `"` escaped to `&quot;`.
            "anchor" | "big" | "blink" | "bold" | "fixed" | "fontcolor" | "fontsize"
            | "italics" | "link" | "small" | "strike" | "sub" | "sup" => {
                let (tag, attr): (&str, Option<&str>) = match name {
                    "anchor" => ("a", Some("name")),
                    "big" => ("big", None),
                    "blink" => ("blink", None),
                    "bold" => ("b", None),
                    "fixed" => ("tt", None),
                    "fontcolor" => ("font", Some("color")),
                    "fontsize" => ("font", Some("size")),
                    "italics" => ("i", None),
                    "link" => ("a", Some("href")),
                    "small" => ("small", None),
                    "strike" => ("strike", None),
                    "sub" => ("sub", None),
                    _ => ("sup", None),
                };
                // The attribute value is ToString(value) (can throw, e.g. a
                // {toString(){throw}}), not the non-throwing display(). Keep it
                // borrowed while quote expansion is streamed into the single
                // admitted output buffer; `replace` + two `format!` temporaries
                // could otherwise multiply a near-limit guest string.
                let aval = match attr {
                    Some(_) => Some(self.to_js_string(arg0)?),
                    None => None,
                };
                let mut total = checked_string_output_add(0, 1 + tag.len() + 1)?; // <tag>
                if let (Some(aname), Some(aval)) = (attr, aval.as_deref()) {
                    total = checked_string_output_add(total, 1 + aname.len() + 2)?; // name="
                    total = checked_string_output_add(total, aval.len())?;
                    let quote_growth = aval
                        .as_bytes()
                        .iter()
                        .filter(|&&b| b == b'"')
                        .count()
                        .checked_mul("&quot;".len() - 1)
                        .ok_or_else(invalid_string_length)?;
                    total = checked_string_output_add(total, quote_growth)?;
                    total = checked_string_output_add(total, 1)?; // closing quote
                }
                total = checked_string_output_add(total, s.len())?;
                total = checked_string_output_add(total, 2 + tag.len() + 1)?; // </tag>

                let mut out = self.guest_string_with_capacity(total)?;
                out.push('<');
                out.push_str(tag);
                if let (Some(aname), Some(aval)) = (attr, aval.as_deref()) {
                    out.push(' ');
                    out.push_str(aname);
                    out.push_str("=\"");
                    for (i, part) in aval.split('"').enumerate() {
                        if i != 0 {
                            out.push_str("&quot;");
                        }
                        out.push_str(part);
                    }
                    out.push('"');
                }
                out.push('>');
                out.push_str(s);
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
                debug_assert_eq!(out.len(), total);
                Ok(Some(self.alloc_str(out)))
            }
            _ => Ok(None),
        }
    }

    /// `indexOf` / `lastIndexOf` / `includes` / `startsWith` / `endsWith` over
    /// the EXACT operands, borrowed in place.
    ///
    /// The needle is coerced to a string VALUE, never the lossy `String` of
    /// `to_js_string`, whose U+FFFD made a lone-surrogate needle match a real
    /// U+FFFD. Two strategies then cover every operand pair:
    ///
    /// * The `&str` search over the receiver's lossy view is exact whenever the
    ///   needle is well-formed and either the receiver is too (the view then IS
    ///   the content, borrowed) or the needle holds no U+FFFD (the only thing a
    ///   substituted surrogate could falsely match). Positions go through the
    ///   receiver's memoized unit/byte bounds, so a scanning loop like
    ///   `while ((i = s.indexOf(",", i + 1)) !== -1)` is linear, not the
    ///   quadratic receiver copy the generic path paid per call.
    /// * Otherwise both sides are mapped to one `char` per UTF-16 code unit
    ///   ([`unit_chars_into`]) and searched there, which is the spec's
    ///   code-unit comparison: a lone-surrogate needle can match half of a pair.
    fn string_search(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Value, Thrown> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        if name != "indexOf" && name != "lastIndexOf" && self.is_regexp(arg0)? {
            return Err(Thrown(format!(
                "TypeError: String.prototype.{name} argument must not be a RegExp"
            )));
        }
        // ToString(searchString) before the position coercion (spec order).
        let needle = self.to_str_value(arg0)?;
        let len = self.heap_str_units(idx);
        // The position coercion can run a user `valueOf`, so a needle
        // allocated by the ToString above is rooted across it.
        let roots = self.host_result_roots.len();
        self.host_result_roots.push(needle);
        let pos = self.string_search_position(name, args.get(1).copied(), len);
        self.host_result_roots.truncate(roots);
        let pos = pos?;
        let nidx = needle.heap_index();
        self.heap.flatten(nidx);

        let (hay, ned) = match (self.heap.get(idx), self.heap.get(nidx)) {
            (HeapObj::Str(hay), HeapObj::Str(ned)) => (hay, ned),
            _ => return Ok(Value::UNDEFINED),
        };
        if ned.units() == 0 {
            return Ok(match name {
                "indexOf" | "lastIndexOf" => Value::int(pos as i32),
                _ => Value::bool(true),
            });
        }
        let lossy_exact = ned.is_wellformed()
            && (hay.is_wellformed() || !bytes_contain_replacement_char(ned.as_bytes()));
        if lossy_exact {
            let s = hay.as_str_lossy();
            let n = ned.as_str_wf();
            let (lo, hi) = hay.unit_byte_bounds(pos);
            return Ok(match name {
                "indexOf" => match s[hi..].find(n) {
                    Some(b) if hay.is_ascii() => Value::int((hi + b) as i32),
                    Some(b) => {
                        // `hi` is unit `pos`, or `pos + 1` when `pos` split a pair.
                        let from = pos + usize::from(lo != hi);
                        let at = from + crate::heap::wtf8_units(&hay.as_bytes()[hi..hi + b]);
                        Value::int(at as i32)
                    }
                    None => Value::int(-1),
                },
                "includes" => Value::bool(s[hi..].contains(n)),
                // Anchored positions: a start or end between the halves of a
                // pair leaves a lone surrogate at the edge of the spec's
                // substring, which a well-formed needle cannot match.
                "startsWith" => Value::bool(lo == hi && s[lo..].starts_with(n)),
                "endsWith" => Value::bool(lo == hi && s[..lo].ends_with(n)),
                _ => {
                    // lastIndexOf: the last match starting at or before `lo`.
                    let mut end = (lo + n.len()).min(s.len());
                    while !s.is_char_boundary(end) {
                        end += 1;
                    }
                    let found = loop {
                        match s[..end].rfind(n) {
                            Some(b) if b <= lo => break Some(b),
                            Some(b) => {
                                end = b + n.len() - 1;
                                while !s.is_char_boundary(end) {
                                    end -= 1;
                                }
                            }
                            None => break None,
                        }
                    };
                    match found {
                        None => Value::int(-1),
                        Some(b) if hay.is_ascii() => Value::int(b as i32),
                        Some(b) => {
                            // `lo` is unit `pos`, or `pos - 1` inside a pair.
                            let lo_unit = pos - usize::from(lo != hi);
                            let at = lo_unit - crate::heap::wtf8_units(&hay.as_bytes()[b..lo]);
                            Value::int(at as i32)
                        }
                    }
                }
            });
        }

        let (hay_len, ned_len, ned_units) = (
            unit_chars_len(hay.as_bytes()),
            unit_chars_len(ned.as_bytes()),
            ned.units(),
        );
        let mut hm = self.string_scratch(hay_len)?;
        let mut nm = self.string_scratch(ned_len)?;
        if let (HeapObj::Str(hay), HeapObj::Str(ned)) = (self.heap.get(idx), self.heap.get(nidx)) {
            unit_chars_into(hay.as_bytes(), &mut hm);
            unit_chars_into(ned.as_bytes(), &mut nm);
        }
        // One char per unit: a unit position is a char index.
        let byte_of = |u: usize| hm.char_indices().nth(u).map_or(hm.len(), |(b, _)| b);
        Ok(match name {
            "indexOf" => {
                let from = byte_of(pos);
                match hm[from..].find(nm.as_str()) {
                    Some(b) => Value::int((pos + hm[from..from + b].chars().count()) as i32),
                    None => Value::int(-1),
                }
            }
            "includes" => Value::bool(hm[byte_of(pos)..].contains(nm.as_str())),
            "startsWith" => Value::bool(hm[byte_of(pos)..].starts_with(nm.as_str())),
            "endsWith" => Value::bool(hm[..byte_of(pos)].ends_with(nm.as_str())),
            _ => {
                let end = byte_of((pos + ned_units).min(len));
                match hm[..end].rfind(nm.as_str()) {
                    Some(b) => Value::int(hm[..b].chars().count() as i32),
                    None => Value::int(-1),
                }
            }
        })
    }

    /// The clamped unit position argument of a [`Self::string_search`]
    /// method: ToIntegerOrInfinity for `indexOf`/`includes`/`startsWith`
    /// (absent is 0), the same for `endsWith` with absent or undefined meaning
    /// the length, and `lastIndexOf`'s ToNumber, where NaN means the length.
    fn string_search_position(
        &mut self,
        name: &str,
        arg: Option<Value>,
        len: usize,
    ) -> Result<usize, Thrown> {
        let len_i = len as i64;
        Ok(match (name, arg) {
            ("endsWith" | "lastIndexOf", None) => len,
            ("endsWith" | "lastIndexOf", Some(v)) if v == Value::UNDEFINED => len,
            ("lastIndexOf", Some(v)) => {
                let n = self.to_number_strict(v)?;
                if n.is_nan() {
                    len
                } else {
                    (n.trunc().max(0.0) as usize).min(len)
                }
            }
            (_, None) => 0,
            (_, Some(v)) => self.to_integer_strict(v)?.clamp(0, len_i) as usize,
        })
    }

    /// An empty scratch `String` with exactly `bytes` reserved: charged to the
    /// instrumented heap budget first and fallibly allocated, so a native
    /// helper's temporary copy of guest text cannot trap the host.
    fn string_scratch(&mut self, bytes: usize) -> Result<String, Thrown> {
        #[cfg(feature = "instrument")]
        self.instrument_preflight_heap_growth(bytes)
            .map_err(|message| Thrown(message.into()))?;
        let mut out = String::new();
        out.try_reserve_exact(bytes)
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        Ok(out)
    }

    /// The part ranges of `recv.split(sep)` for a non-empty separator, at most
    /// `lim` of them: byte ranges of `recv`'s exact bytes (`false`), or unit
    /// ranges (`true`) when the separator can only be matched per code unit
    /// (see [`Self::string_search`] for the two strategies). `s` is `recv`'s
    /// lossy view. Under a size cap or heap ceiling
    /// ([`Self::split_counts_first`]), a receiver past [`SPLIT_ADMIT_BYTES`]
    /// has its parts counted and admitted before the range vector is
    /// allocated.
    fn split_ranges(
        &mut self,
        recv: &crate::heap::JsStr,
        s: &str,
        sep: &crate::heap::JsStr,
        lim: usize,
    ) -> Result<(Vec<(usize, usize)>, bool), Thrown> {
        let alloc_error = || Thrown("RangeError: Invalid array length".into());
        if sep.is_wellformed()
            && (recv.is_wellformed() || !bytes_contain_replacement_char(sep.as_bytes()))
        {
            let n = sep.as_str_wf();
            // A one-character separator (the common `split(",")`) searches
            // as a `char`, which is memchr-based.
            let mut chars = n.chars();
            let single = match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            };
            let mut ranges: Vec<(usize, usize)> = Vec::new();
            let counted = s.len() > SPLIT_ADMIT_BYTES && self.split_counts_first();
            if counted {
                let count = match single {
                    Some(c) => s.split(c).take(lim).count(),
                    None => s.split(n).take(lim).count(),
                };
                self.split_admit(count, s.len())?;
                ranges.try_reserve_exact(count).map_err(|_| alloc_error())?;
            }
            let base = s.as_ptr() as usize;
            let mut push = |p: &str| -> Result<(), Thrown> {
                if ranges.len() == ranges.capacity() {
                    ranges
                        .try_reserve(ranges.len().max(16))
                        .map_err(|_| alloc_error())?;
                }
                let off = p.as_ptr() as usize - base;
                ranges.push((off, off + p.len()));
                Ok(())
            };
            match single {
                Some(c) => s.split(c).take(lim).try_for_each(&mut push)?,
                None => s.split(n).take(lim).try_for_each(&mut push)?,
            }
            if !counted {
                self.split_admit(ranges.len(), s.len())?;
            }
            return Ok((ranges, false));
        }
        let mut hm = self.string_scratch(unit_chars_len(recv.as_bytes()))?;
        let mut nm = self.string_scratch(unit_chars_len(sep.as_bytes()))?;
        unit_chars_into(recv.as_bytes(), &mut hm);
        unit_chars_into(sep.as_bytes(), &mut nm);
        let count = hm.split(nm.as_str()).take(lim).count();
        self.split_admit(count, recv.as_bytes().len())?;
        let mut ranges = Vec::new();
        ranges.try_reserve_exact(count).map_err(|_| alloc_error())?;
        let mut at = 0usize;
        for piece in hm.split(nm.as_str()).take(lim) {
            let end = at + piece.chars().count();
            ranges.push((at, end));
            at = end + sep.units();
        }
        Ok((ranges, true))
    }

    /// Whether a large `split` counts its parts before it collects their
    /// ranges: the hardened profile's dense-array cap and an attached heap
    /// ceiling must refuse a result before even its range vector exists. The
    /// ordinary run collects the ranges in one pass, growing fallibly, and
    /// admits them afterwards.
    fn split_counts_first(&self) -> bool {
        #[cfg(feature = "safe-sandbox")]
        return true;
        #[cfg(all(not(feature = "safe-sandbox"), feature = "instrument"))]
        return self
            .instr_rec
            .as_ref()
            .is_some_and(|rec| rec.heap_limit != usize::MAX);
        #[cfg(all(not(feature = "safe-sandbox"), not(feature = "instrument")))]
        return false;
    }

    /// Admit a split result of `parts` strings holding at most `bytes` of
    /// text: the hardened profile's dense-array cap, the native iteration
    /// bound, and the heap budget for the parts, their slots and the result
    /// array.
    fn split_admit(&mut self, parts: usize, bytes: usize) -> Result<(), Thrown> {
        #[cfg(feature = "safe-sandbox")]
        if parts > MAX_DENSE_ARRAY_LEN {
            return Err(Thrown("RangeError: Invalid array length".into()));
        }
        self.preflight_native_iteration_work(parts as u64)?;
        #[cfg(feature = "instrument")]
        {
            let per_part = std::mem::size_of::<HeapObj>()
                + std::mem::size_of::<Value>()
                + std::mem::size_of::<(usize, usize)>();
            self.instrument_preflight_heap_growth(parts.saturating_mul(per_part).saturating_add(bytes))
                .map_err(|message| Thrown(message.into()))?;
        }
        #[cfg(not(feature = "instrument"))]
        let _ = bytes;
        Ok(())
    }

    /// `String.fromCharCode(...codes)`: each arg is ToUint16(ToNumber) — strict
    /// ToNumber (ToPrimitive-aware, BigInt/Symbol → TypeError, a throwing valueOf
    /// propagates), coerced in argument order. The result is built as WTF-8:
    /// `wtf8_push_cp` combines ADJACENT (high, low) surrogate halves into the
    /// astral scalar they encode (canonical form), and a LONE half is stored
    /// as a real lone surrogate.
    pub(crate) fn string_from_char_codes(
        &mut self,
        args: &[Value],
    ) -> Result<crate::heap::JsStr, Thrown> {
        self.preflight_native_iteration_work(args.len() as u64)?;
        // One UTF-16 code unit needs at most three WTF-8 bytes. Reserve that
        // admitted worst case once so a large argument vector cannot drive
        // geometric, infallible growth inside this single native instruction.
        let capacity = args
            .len()
            .checked_mul(3)
            .filter(|&n| n <= MAX_STRING_BYTES)
            .ok_or_else(invalid_string_length)?;
        self.preflight_guest_string_size(capacity)?;
        let mut out: Vec<u8> = Vec::new();
        out.try_reserve_exact(capacity)
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        for &v in args {
            let u = crate::vm::helpers_num2::to_uint32(self.to_number_strict(v)?) as u16;
            crate::heap::wtf8_push_cp(&mut out, u as u32);
        }
        Ok(crate::heap::JsStr::from_wtf8(out))
    }

    /// String.prototype.replace / replaceAll with a NON-regexp searchValue.
    /// `s_idx` is the receiver string's heap index and `recv` the caller's copy
    /// of its content; `all` selects replaceAll.
    /// Delegates to a custom `searchValue[Symbol.replace]` if present, else does a
    /// plain substring replacement with full GetSubstitution ($-pattern) support
    /// and functional replacers.
    pub(crate) fn string_replace_plain(
        &mut self,
        s_idx: u32,
        recv: &crate::heap::JsStr,
        search_v: Value,
        repl_v: Value,
        all: bool,
    ) -> Result<Value, Thrown> {
        // replaceAll step 2.b: when searchValue is a RegExp (IsRegExp — reads its
        // @@match, propagating an abrupt), it must be global — `Get(searchValue,
        // "flags")` (propagating an abrupt getter) must be object-coercible (a null/
        // undefined `flags` is a TypeError) and `ToString(flags)` must contain "g".
        // All of this BEFORE the @@replace delegation and before any ToString of the
        // receiver/searchValue. String.prototype.replace has no such restriction.
        if all
            && search_v != Value::UNDEFINED
            && search_v != Value::NULL
            && self.is_regexp(search_v)?
        {
            let flags = self.get_prop(search_v, "flags")?;
            if flags == Value::UNDEFINED || flags == Value::NULL {
                return Err(Thrown(
                    "TypeError: String.prototype.replaceAll called with a RegExp whose flags is not coercible"
                        .into(),
                ));
            }
            let fs = self.to_js_string(flags)?;
            if !fs.contains('g') {
                return Err(Thrown(
                    "TypeError: replaceAll must be called with a global RegExp".into(),
                ));
            }
        }
        // If searchValue is an OBJECT with a @@replace method, defer to it
        // (GetMethod: a present-but-not-callable @@replace is a TypeError; null/
        // undefined falls through). The `is_object_value` guard matters: per spec the
        // `@@replace` property is only accessed when searchValue is an Object, so a
        // primitive searchValue (a number/string/boolean) must NOT trigger a
        // `Number.prototype[@@replace]` getter etc.
        if self.is_object_value(search_v) {
            let m = self.get_prop(search_v, "@@replace")?;
            if m != Value::UNDEFINED && m != Value::NULL {
                if !self.is_callable(m) {
                    return Err(Thrown(
                        "TypeError: searchValue[Symbol.replace] is not a function".into(),
                    ));
                }
                let sval = Value::heap(s_idx);
                return self.call_value(m, search_v, &[sval, repl_v]);
            }
        }
        // ToString(searchValue), then ToString(replaceValue) unless it is
        // callable, each as the exact string VALUE. The lossy `String` form
        // made a lone-surrogate needle match a real U+FFFD and dropped the
        // surrogates of a replacement. Both are copied out of the heap, as the
        // receiver is, so a functional replacer cannot disturb them.
        let search = self.to_js_str_owned(search_v)?;
        let functional = self.is_callable(repl_v);
        let tmpl = if functional {
            crate::heap::JsStr::new(String::new())
        } else {
            self.to_js_str_owned(repl_v)?
        };
        let bytes = recv.as_bytes();
        let tmpl = tmpl.as_bytes();
        let len = recv.units();
        // `` $` `` and `$'` copy the text around a match; it is sliced only
        // when the template names one of them.
        let context = !functional && tmpl.windows(2).any(|w| w == b"$`" || w == b"$'");
        let empty = crate::heap::JsStr::new(String::new());
        let mut out: Vec<u8> = Vec::new();
        let mut any = false;
        if search.units() == 0 {
            // An empty searchValue matches at position 0 (replace), or at every
            // UNIT boundary including the end (replaceAll), so also between the
            // halves of a surrogate pair, which stay apart unless the
            // replacement between them is empty.
            let ends = if all { len } else { 0 };
            let mut units = recv.units_iter();
            for p in 0..=ends {
                let (pre, post) = if context {
                    (recv.slice_units(0, p), recv.slice_units(p, len))
                } else {
                    (empty.clone(), empty.clone())
                };
                self.append_plain_replacement(
                    &mut out,
                    s_idx,
                    repl_v,
                    functional,
                    tmpl,
                    &[],
                    p,
                    pre.as_bytes(),
                    post.as_bytes(),
                )?;
                if p < ends {
                    if let Some(u) = units.next() {
                        self.append_guest_unit(&mut out, u)?;
                    }
                }
            }
            if !all {
                self.append_guest_wtf8(&mut out, bytes)?;
            }
            any = true;
        } else if search.is_wellformed()
            && (recv.is_wellformed() || !bytes_contain_replacement_char(search.as_bytes()))
        {
            // The receiver's lossy view differs from its content only where a
            // lone surrogate reads as U+FFFD, which this needle cannot match,
            // and it is byte-for-byte aligned with the content, so its match
            // offsets slice the exact bytes. Matches are non-overlapping.
            let s = recv.as_str_lossy();
            let needle = search.as_str_wf();
            let ascii = recv.is_ascii();
            // The replacer's position argument is a UNIT position, counted
            // forward from the previous match rather than from byte 0.
            let (mut last, mut counted, mut units) = (0usize, 0usize, 0usize);
            while let Some(off) = s[last..].find(needle) {
                let pos = last + off;
                any = true;
                self.append_guest_wtf8(&mut out, &bytes[last..pos])?;
                let position = if ascii {
                    pos
                } else {
                    units += crate::heap::wtf8_units(&bytes[counted..pos]);
                    counted = pos;
                    units
                };
                let end = pos + needle.len();
                let (pre, post): (&[u8], &[u8]) = if context {
                    (&bytes[..pos], &bytes[end..])
                } else {
                    (&[], &[])
                };
                self.append_plain_replacement(
                    &mut out,
                    s_idx,
                    repl_v,
                    functional,
                    tmpl,
                    search.as_bytes(),
                    position,
                    pre,
                    post,
                )?;
                last = end;
                if !all {
                    break;
                }
            }
            if any {
                self.append_guest_wtf8(&mut out, &bytes[last..])?;
            }
        } else {
            // A lone-surrogate needle (or a U+FFFD one against a receiver
            // whose lossy view has substitutes) is matched per UTF-16 code
            // unit, where it can also match half of a pair: both sides map to
            // one `char` per unit (see `string_search`), so a char index is a
            // unit position.
            let mut hm = self.string_scratch(unit_chars_len(bytes))?;
            let mut nm = self.string_scratch(unit_chars_len(search.as_bytes()))?;
            unit_chars_into(bytes, &mut hm);
            unit_chars_into(search.as_bytes(), &mut nm);
            let n_units = search.units();
            let (mut last, mut at_byte, mut at_unit) = (0usize, 0usize, 0usize);
            for (b, _) in hm.match_indices(nm.as_str()) {
                at_unit += hm[at_byte..b].chars().count();
                at_byte = b;
                let pos = at_unit;
                any = true;
                let gap = recv.slice_units(last, pos);
                self.append_guest_wtf8(&mut out, gap.as_bytes())?;
                let (pre, post) = if context {
                    (recv.slice_units(0, pos), recv.slice_units(pos + n_units, len))
                } else {
                    (empty.clone(), empty.clone())
                };
                self.append_plain_replacement(
                    &mut out,
                    s_idx,
                    repl_v,
                    functional,
                    tmpl,
                    search.as_bytes(),
                    pos,
                    pre.as_bytes(),
                    post.as_bytes(),
                )?;
                last = pos + n_units;
                if !all {
                    break;
                }
            }
            if any {
                let tail = recv.slice_units(last, len);
                self.append_guest_wtf8(&mut out, tail.as_bytes())?;
            }
        }
        if !any {
            // No match: the receiver itself (strings are immutable, so the
            // same value is indistinguishable from a copy).
            return Ok(Value::heap(s_idx));
        }
        Ok(Value::heap(
            self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out)),
        ))
    }

    /// Append the replacement for one match of a plain `replace`/`replaceAll`:
    /// the functional replacer's result, ToString'd exactly, or
    /// GetSubstitution of the template. A string search has no captures and
    /// no groups object, so only `$$`, `$&`, `` $` `` and `$'` are special and
    /// `$n`/`$<` stay literal. The template is scanned as WTF-8 bytes, which is
    /// exact: `$` and the four selectors are ASCII, and no byte of a
    /// multi-byte sequence is.
    #[allow(clippy::too_many_arguments)]
    fn append_plain_replacement(
        &mut self,
        out: &mut Vec<u8>,
        s_idx: u32,
        repl_v: Value,
        functional: bool,
        tmpl: &[u8],
        matched: &[u8],
        position: usize,
        pre: &[u8],
        post: &[u8],
    ) -> Result<(), Thrown> {
        if functional {
            let m = self
                .heap
                .alloc_js(crate::heap::JsStr::from_wtf8(matched.to_vec()));
            // The third callback argument is the original receiver string.
            // Reuse its immutable heap value: cloning a near-limit source
            // once per match made replaceAll's native loop an unchecked
            // O(matches * source_len) allocator despite an O(1) alias being
            // semantically identical.
            let argv = [Value::heap(m), Value::num(position as f64), Value::heap(s_idx)];
            let r = self.call_value(repl_v, Value::UNDEFINED, &argv)?;
            let r = self.to_str_value(r)?;
            let exact = self
                .heap
                .str_wtf8_cow(r.heap_index())
                .map(|c| c.into_owned())
                .unwrap_or_default();
            return self.append_guest_wtf8(out, &exact);
        }
        let (mut literal, mut i) = (0usize, 0usize);
        while i + 1 < tmpl.len() {
            let piece: Option<&[u8]> = if tmpl[i] == b'$' {
                match tmpl[i + 1] {
                    b'$' => Some(b"$"),
                    b'&' => Some(matched),
                    b'`' => Some(pre),
                    b'\'' => Some(post),
                    _ => None,
                }
            } else {
                None
            };
            match piece {
                Some(piece) => {
                    self.append_guest_wtf8(out, &tmpl[literal..i])?;
                    self.append_guest_wtf8(out, piece)?;
                    i += 2;
                    literal = i;
                }
                None => i += 1,
            }
        }
        self.append_guest_wtf8(out, &tmpl[literal..])
    }

    /// `append_guest_string` for a WTF-8 buffer: the total is admitted against
    /// MAX_STRING_BYTES and the heap budget, the buffer grows fallibly, and the
    /// segment joins with `wtf8_push`, so surrogate halves meeting at the seam
    /// pair up as they do in the UTF-16 string they spell.
    pub(crate) fn append_guest_wtf8(&mut self, out: &mut Vec<u8>, seg: &[u8]) -> Result<(), Thrown> {
        if seg.is_empty() {
            return Ok(());
        }
        let total = out
            .len()
            .checked_add(seg.len())
            .filter(|&n| n <= MAX_STRING_BYTES)
            .ok_or_else(invalid_string_length)?;
        #[cfg(feature = "instrument")]
        self.instrument_preflight_heap_growth(total)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(not(feature = "instrument"))]
        let _ = total;
        out.try_reserve(seg.len())
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        crate::heap::wtf8_push(out, seg);
        Ok(())
    }

    /// Append one UTF-16 code unit to a WTF-8 buffer, as [`Self::append_guest_wtf8`].
    fn append_guest_unit(&mut self, out: &mut Vec<u8>, unit: u16) -> Result<(), Thrown> {
        // Generalized UTF-8 of one BMP code point (a surrogate included).
        let u = unit as u32;
        let (buf, n) = match u {
            0..=0x7F => ([u as u8, 0, 0], 1),
            0x80..=0x7FF => ([0xC0 | (u >> 6) as u8, 0x80 | (u & 0x3F) as u8, 0], 2),
            _ => (
                [
                    0xE0 | (u >> 12) as u8,
                    0x80 | ((u >> 6) & 0x3F) as u8,
                    0x80 | (u & 0x3F) as u8,
                ],
                3,
            ),
        };
        self.append_guest_wtf8(out, &buf[..n])
    }

    /// ToString(`v`) as an owned copy of the exact string (lone surrogates
    /// kept), safe to hold across user code.
    pub(crate) fn to_js_str_owned(&mut self, v: Value) -> Result<crate::heap::JsStr, Thrown> {
        let sv = self.to_str_value(v)?;
        self.heap.flatten(sv.heap_index());
        Ok(match self.heap.get(sv.heap_index()) {
            HeapObj::Str(js) => js.clone(),
            _ => crate::heap::JsStr::new(String::new()),
        })
    }
}

fn invalid_string_length() -> Thrown {
    Thrown("RangeError: Invalid string length".into())
}

/// A `split` receiver at most this long never takes the counting pass: its
/// result is bounded by its length and admitted after the ranges are found.
const SPLIT_ADMIT_BYTES: usize = 4096;

/// Whether WTF-8 `bytes` hold a U+FFFD. A needle without one can be searched
/// for in a receiver's LOSSY view exactly: the view differs from the content
/// only where a lone surrogate reads as U+FFFD.
fn bytes_contain_replacement_char(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD])
}

/// UTF-8 length of [`unit_chars_into`]'s output for `bytes`.
fn unit_chars_len(bytes: &[u8]) -> usize {
    if bytes.is_ascii() {
        return bytes.len();
    }
    crate::heap::wtf8_units_iter(bytes)
        .map(|u| match u {
            0..=0x7F => 1,
            0x80..=0x7FF => 2,
            0xD800..=0xDFFF => 4,
            _ => 3,
        })
        .sum()
}

/// Append one `char` per UTF-16 code unit of WTF-8 `bytes`: each non-surrogate
/// unit as itself, and each surrogate (lone, or a half of an astral pair) as
/// U+F0000 plus its offset from U+D800. No other unit produces that
/// supplementary private-use range, so `str` search over two mapped strings is
/// exactly the spec's code-unit search, and a char index is a unit position.
fn unit_chars_into(bytes: &[u8], out: &mut String) {
    if bytes.is_ascii() {
        // ASCII is valid UTF-8 and maps to itself.
        out.push_str(std::str::from_utf8(bytes).unwrap_or_default());
        return;
    }
    for u in crate::heap::wtf8_units_iter(bytes) {
        let cp = match u {
            0xD800..=0xDFFF => 0xF0000 + (u as u32 - 0xD800),
            _ => u as u32,
        };
        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
    }
}

fn checked_string_output_add(current: usize, additional: usize) -> Result<usize, Thrown> {
    current
        .checked_add(additional)
        .filter(|&total| total <= MAX_STRING_BYTES)
        .ok_or_else(invalid_string_length)
}

fn checked_char_output_len(iter: impl Iterator<Item = char>) -> Result<usize, Thrown> {
    let mut total = 0usize;
    for c in iter {
        total = checked_string_output_add(total, c.len_utf8())?;
    }
    Ok(total)
}

fn extend_chars(out: &mut String, iter: impl Iterator<Item = char>) {
    for c in iter {
        out.push(c);
    }
}

/// `toUpperCase`/`toLowerCase` over the receiver's EXACT WTF-8 bytes. The
/// lossy `&str` view decays each lone surrogate to U+FFFD, but case mapping is
/// per UTF-16 code unit: a lone surrogate has no mapping and must survive
/// unchanged (staging/sm/String/string-upper-lower-mapping.js). Each maximal
/// well-formed segment maps through Rust's Unicode tables (which include the
/// locale-independent context rules — final sigma needs the whole segment, so
/// per-char mapping would be wrong); a surrogate's WTF-8 bytes copy through
/// verbatim. A surrogate is neither cased nor case-ignorable, so it breaks the
/// UCD context exactly where the segments break.
fn case_map_exact_len(bytes: &[u8], upper: bool) -> Result<usize, Thrown> {
    // ASCII maps within ASCII, one byte for one.
    if bytes.is_ascii() {
        return Ok(bytes.len());
    }
    let mut total = 0usize;
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                for c in s.chars() {
                    let mapped_len: usize = if upper {
                        c.to_uppercase().map(char::len_utf8).sum()
                    } else {
                        // `str::to_lowercase`'s contextual final sigma has the
                        // same UTF-8 width as the ordinary sigma emitted here.
                        c.to_lowercase().map(char::len_utf8).sum()
                    };
                    total = checked_string_output_add(total, mapped_len)?;
                }
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                let s = std::str::from_utf8(&rest[..valid]).unwrap();
                for c in s.chars() {
                    let mapped_len: usize = if upper {
                        c.to_uppercase().map(char::len_utf8).sum()
                    } else {
                        c.to_lowercase().map(char::len_utf8).sum()
                    };
                    total = checked_string_output_add(total, mapped_len)?;
                }
                let skip = e.error_len().unwrap_or(rest.len() - valid);
                total = checked_string_output_add(total, skip)?;
                rest = &rest[valid + skip..];
            }
        }
    }
    Ok(total)
}

fn case_map_exact(
    bytes: &[u8],
    upper: bool,
    mapped_len: usize,
) -> Result<crate::heap::JsStr, Thrown> {
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(mapped_len)
        .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
    if bytes.is_ascii() && bytes.len() == mapped_len {
        out.extend(bytes.iter().map(|b| {
            if upper {
                b.to_ascii_uppercase()
            } else {
                b.to_ascii_lowercase()
            }
        }));
        return Ok(crate::heap::JsStr::from_wtf8(out));
    }
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                let mapped = if upper {
                    s.to_uppercase()
                } else {
                    s.to_lowercase()
                };
                if out
                    .len()
                    .checked_add(mapped.len())
                    .filter(|&total| total <= mapped_len)
                    .is_none()
                {
                    return Err(invalid_string_length());
                }
                out.extend_from_slice(mapped.as_bytes());
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                // The valid prefix is UTF-8 by construction.
                let s = std::str::from_utf8(&rest[..valid]).unwrap();
                let mapped = if upper {
                    s.to_uppercase()
                } else {
                    s.to_lowercase()
                };
                if out
                    .len()
                    .checked_add(mapped.len())
                    .filter(|&total| total <= mapped_len)
                    .is_none()
                {
                    return Err(invalid_string_length());
                }
                out.extend_from_slice(mapped.as_bytes());
                // Copy the invalid (lone-surrogate) bytes verbatim and resume.
                let skip = e.error_len().unwrap_or(rest.len() - valid);
                if out
                    .len()
                    .checked_add(skip)
                    .filter(|&total| total <= mapped_len)
                    .is_none()
                {
                    return Err(invalid_string_length());
                }
                out.extend_from_slice(&rest[valid..valid + skip]);
                rest = &rest[valid + skip..];
            }
        }
    }
    if out.len() != mapped_len {
        return Err(invalid_string_length());
    }
    Ok(crate::heap::JsStr::from_wtf8(out))
}

/// WTF-8 `bytes` as its maximal well-formed runs (`Ok`) and the lone-surrogate
/// bytes between them (`Err`), in order. A lone surrogate is neither cased nor
/// case-ignorable, has combining class 0 and no decomposition, so a
/// per-character context (final sigma, `After_I`, `More_Above`, normalization
/// blocking) ends at it exactly as it ends at a run boundary.
fn wtf8_runs(bytes: &[u8]) -> impl Iterator<Item = Result<&str, &[u8]>> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (run, tail) = match std::str::from_utf8(rest) {
            Ok(s) => (Ok(s), &rest[rest.len()..]),
            Err(e) if e.valid_up_to() > 0 => {
                let (head, tail) = rest.split_at(e.valid_up_to());
                // The valid prefix is UTF-8 by construction.
                (Ok(std::str::from_utf8(head).unwrap_or_default()), tail)
            }
            Err(e) => {
                let (bad, tail) = rest.split_at(e.error_len().unwrap_or(rest.len()));
                (Err(bad), tail)
            }
        };
        rest = tail;
        Some(run)
    })
}

/// UTF-8 length of the language-sensitive (`az`/`lt`/`tr`) case mapping of
/// WTF-8 `bytes`: every well-formed run mapped, lone surrogates copied. At
/// most three output bytes per input byte, so it cannot overflow `usize` for
/// an admitted receiver.
fn special_case_exact_len(bytes: &[u8], lang: &str, upper: bool) -> usize {
    let mut total = 0usize;
    for run in wtf8_runs(bytes) {
        match run {
            Ok(s) => {
                crate::vm::special_casing::transform_case_each(s, lang, upper, &mut |c| {
                    total += c.len_utf8()
                });
            }
            Err(b) => total += b.len(),
        }
    }
    total
}

/// Build the mapping [`special_case_exact_len`] sized into exactly
/// `mapped_len` fallibly reserved bytes.
fn special_case_exact(
    bytes: &[u8],
    lang: &str,
    upper: bool,
    mapped_len: usize,
) -> Result<crate::heap::JsStr, Thrown> {
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(mapped_len)
        .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
    for run in wtf8_runs(bytes) {
        match run {
            Ok(s) => {
                crate::vm::special_casing::transform_case_each(s, lang, upper, &mut |c| {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                });
            }
            Err(b) => out.extend_from_slice(b),
        }
    }
    if out.len() != mapped_len {
        return Err(invalid_string_length());
    }
    Ok(crate::heap::JsStr::from_wtf8(out))
}

/// Emit the `form` normalization of the well-formed run `s` (`form` is one of
/// the four validated names; anything else is NFKD).
fn for_each_normalized(form: &str, s: &str, emit: &mut impl FnMut(char)) {
    use unicode_normalization::UnicodeNormalization;
    match form {
        "NFC" => s.nfc().for_each(emit),
        "NFD" => s.nfd().for_each(emit),
        "NFKC" => s.nfkc().for_each(emit),
        _ => s.nfkd().for_each(emit),
    }
}

/// The text `localeCompare` and `Intl.Collator.prototype.compare` collate for
/// `js`. A well-formed string is itself (borrowed). The lossy view shows every
/// lone surrogate as U+FFFD, so two different surrogates, or one and a real
/// U+FFFD, collated equal. Each is shown instead as U+D0000 plus the
/// surrogate, a code point of unassigned plane 13: equal to no assigned or
/// private-use character, ordered by code unit, and after every letter, digit
/// and symbol (emoji included), where ICU's implicit weights put a lone
/// surrogate. (ICU also puts it before U+FFFD and the private-use characters,
/// which no one code point can stand for as well.) Changes here reach
/// `Intl.Collator.prototype.compare` too, which views its operands through
/// this so the two keep agreeing.
pub(crate) fn collation_view(js: &crate::heap::JsStr) -> std::borrow::Cow<'_, str> {
    if js.is_wellformed() {
        return std::borrow::Cow::Borrowed(js.as_str_wf());
    }
    std::borrow::Cow::Owned(
        crate::heap::wtf8_code_points(js.as_bytes())
            .map(|cp| match cp {
                0xD800..=0xDFFF => char::from_u32(0xD_0000 + cp).unwrap_or('\u{FFFD}'),
                _ => char::from_u32(cp).unwrap_or('\u{FFFD}'),
            })
            .collect(),
    )
}
