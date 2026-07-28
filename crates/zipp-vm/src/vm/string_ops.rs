#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
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
        Ok(matches!(self.heap.get(v.heap_index()), HeapObj::RegExp { .. }))
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
        Ok(self.string_method(s_idx, name, args)?.unwrap_or(Value::UNDEFINED))
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
        Value::heap(self.heap.alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(u as u32))))
    }

    /// The string Value for code point `cp` (for-of / iterator steps): an
    /// interned slot for ASCII, else a fresh 1-code-point string (`cp` may be
    /// a lone surrogate).
    pub(crate) fn str_from_cp(&mut self, cp: u32) -> Value {
        if cp < 128 {
            return Value::heap(cp);
        }
        Value::heap(self.heap.alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(cp))))
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

    pub(crate) fn string_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
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
                let u = if i >= 0 { self.heap_unit_at(idx, i as usize) } else { None };
                return Ok(Some(match u {
                    Some(u) => Value::int(u as i32),
                    None => Value::num(f64::NAN),
                }));
            }
            "codePointAt" => {
                let i = self.to_integer_strict(arg0)?;
                let c = if i >= 0 { self.heap_code_point_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(cp) => Value::int(cp as i32),
                    None => Value::UNDEFINED,
                }));
            }
            "charAt" => {
                let i = self.to_integer_strict(arg0)?;
                let u = if i >= 0 { self.heap_unit_at(idx, i as usize) } else { None };
                return Ok(Some(match u {
                    Some(u) => self.str_from_unit(u),
                    None => Value::heap(crate::heap::INTERN_EMPTY),
                }));
            }
            "at" => {
                let len = self.heap_str_units(idx) as i64;
                let i = self.to_integer_strict(arg0)?;
                let abs = if i < 0 { i + len } else { i };
                let u = if abs >= 0 && abs < len { self.heap_unit_at(idx, abs as usize) } else { None };
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
                let norm = |i: i64| if i < 0 { len.saturating_add(i).max(0) } else { i.min(len) };
                let start = if args.is_empty() { 0 } else { norm(self.to_integer_strict(arg0)?) };
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
                let s0 = if args.is_empty() { 0 } else { self.to_integer_strict(arg0)?.clamp(0, len) };
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
        // walks (unit == byte). All three closures take the RECEIVER `s` only.
        let unit_len = |s: &str| -> usize {
            if ascii {
                s.len()
            } else {
                crate::heap::str_units(s)
            }
        };
        // Byte offset of a SEARCH-START unit position (mid-pair rounds up — exact
        // for searches; anchored uses go through `unit_byte_bounds`).
        let u2b = |s: &str, u: usize| -> usize {
            if ascii {
                u.min(s.len())
            } else {
                crate::heap::unit_to_byte(s, u)
            }
        };
        // Unit position of a result byte offset (always a scalar boundary).
        let b2u = |s: &str, b: usize| -> usize {
            if ascii {
                b
            } else {
                crate::heap::byte_to_units(s, b)
            }
        };
        // Substring by unit positions [a, b) — EXACT (slices the WTF-8 bytes;
        // a bound splitting a surrogate pair keeps the REAL covered half).
        let subu = |a: usize, b: usize| -> crate::heap::JsStr { js_recv.slice_units(a, b) };
        match name {
            "indexOf" => {
                // ToString(searchString) (honours @@toPrimitive/toString/valueOf,
                // throws on a Symbol) BEFORE ToInteger(position) — spec arg order.
                let needle = self.to_js_string(arg0)?;
                // Optional fromIndex (ToInteger, a unit position) to start at.
                let from = if args.len() >= 2 {
                    self.to_integer_strict(args[1])?.max(0) as usize
                } else {
                    0
                };
                let byte_from = u2b(&s, from);
                let pos = s[byte_from..]
                    .find(&needle)
                    .map(|b| b2u(&s, byte_from + b) as i32)
                    .unwrap_or(-1);
                Ok(Some(Value::int(pos)))
            }
            "includes" => {
                if self.is_regexp(arg0)? {
                    return Err(Thrown(
                        "TypeError: String.prototype.includes argument must not be a RegExp".into(),
                    ));
                }
                let needle = self.to_js_string(arg0)?;
                let len = unit_len(&s) as i64;
                let pos = if args.len() >= 2 {
                    self.to_integer_strict(args[1])?.clamp(0, len)
                } else {
                    0
                } as usize;
                let byte = u2b(&s, pos);
                Ok(Some(Value::bool(s[byte..].contains(&needle))))
            }
            "toUpperCase" => Ok(Some(Value::heap(
                self.heap.alloc_js(case_map_exact(js_recv.as_bytes(), true)),
            ))),
            "toLowerCase" => Ok(Some(Value::heap(
                self.heap.alloc_js(case_map_exact(js_recv.as_bytes(), false)),
            ))),
            // NB: `slice` / `substring` are handled by the no-clone fast path in
            // the early match above (before the receiver is copied).
            "repeat" => {
                // ToIntegerOrInfinity(count): a NEGATIVE or +Infinity count is a
                // RangeError — checked on the coerced number BEFORE the empty-string
                // fast path (`"".repeat(Infinity)` must still throw, not yield "").
                let nf = self.to_number_coerce(arg0)?;
                let n_int = if nf.is_nan() { 0.0 } else { nf.trunc() };
                if n_int < 0.0 || n_int == f64::INFINITY {
                    return Err(Thrown("RangeError: Invalid count value".into()));
                }
                // Bound the result (an unbounded build would hang / OOM): a too-long
                // string is a RangeError per spec. (n_int is now finite and ≥ 0.)
                if n_int * (s.len() as f64) > (1u64 << 28) as f64 {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                if js_recv.is_wellformed() {
                    return Ok(Some(self.alloc_str(s.repeat(n_int as usize))));
                }
                // Non-well-formed receiver: repeat the EXACT bytes, with seam
                // canonicalization — '\uDC00\uD800'.repeat(2) forms a real
                // astral scalar at each junction (UTF-16 unit semantics).
                let mut out: Vec<u8> = Vec::with_capacity(js_recv.as_bytes().len() * n_int as usize);
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
                        return Ok(Some(self.call_value(searcher, arg0, &[Value::heap(idx)])?));
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
            "split" if self.as_regexp(arg0).is_some() => {
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
                if self.regexp_replace_fast_ok(re) {
                    let global = matches!(
                        self.heap.get(re),
                        HeapObj::RegExp { flags, .. } if flags.contains('g')
                    );
                    Ok(Some(self.regex_replace(idx, re, repl, global)?))
                } else {
                    let sl = self
                        .heap
                        .str_cow(idx)
                        .map(|c| c.into_owned())
                        .unwrap_or_default();
                    Ok(Some(self.string_replace_plain(&sl, idx, arg0, repl, false)?))
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
                        return Ok(Some(self.call_value(m, arg0, &[Value::heap(idx), limit_raw])?));
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
                        crate::vm::helpers_num2::to_uint32(self.to_number_coerce(v)?) as usize
                    }
                    _ => usize::MAX,
                };
                let parts: Vec<Value> = if args.is_empty() || arg0 == Value::UNDEFINED {
                    // No separator → the whole string as a single element (lim 0
                    // → []). The receiver itself — exact, strings are immutable.
                    if lim == 0 { Vec::new() } else { vec![Value::heap(idx)] }
                } else {
                    // ToString(separator) — runs a user toString (propagating a
                    // throw) and rejects a Symbol; after ToUint32(limit) and before
                    // the lim==0 early-out, matching the spec ordering.
                    let sep = self.to_js_string(arg0)?;
                    if lim == 0 {
                        Vec::new()
                    } else if sep.is_empty() {
                        // Split into 1-UNIT pieces (spec: code units). An astral
                        // scalar's halves are REAL lone-surrogate strings.
                        let units: Vec<u16> = js_recv.units_iter().take(lim).collect();
                        units.into_iter().map(|u| self.str_from_unit(u)).collect()
                    } else {
                        // Byte offsets in the lossy view match the exact bytes, so
                        // each part slices `js_recv`'s WTF-8 exactly.
                        let ranges: Vec<(usize, usize)> = s
                            .split(&sep)
                            .take(lim)
                            .map(|p| {
                                let off = p.as_ptr() as usize - s.as_ptr() as usize;
                                (off, off + p.len())
                            })
                            .collect();
                        ranges
                            .into_iter()
                            .map(|(a, b)| {
                                let js = crate::heap::JsStr::from_wtf8(
                                    js_recv.as_bytes()[a..b].to_vec(),
                                );
                                Value::heap(self.heap.alloc_js(js))
                            })
                            .collect()
                    }
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(parts)))))
            }
            // ECMAScript TrimString whitespace = Unicode White_Space + U+FEFF
            // (ZWNBSP/BOM), which Rust's char::is_whitespace excludes. The trim
            // is computed on the lossy view (U+FFFD is not whitespace, neither
            // are surrogates) and the result sliced from the EXACT bytes at the
            // same offsets.
            "trim" => {
                let w = |c: char| c == '\u{FEFF}' || c.is_whitespace();
                let t = s.trim_matches(w);
                Ok(Some(self.alloc_recv_slice(&js_recv, &s, t)))
            }
            "trimStart" => {
                let w = |c: char| c == '\u{FEFF}' || c.is_whitespace();
                let t = s.trim_start_matches(w);
                Ok(Some(self.alloc_recv_slice(&js_recv, &s, t)))
            }
            "trimEnd" => {
                let w = |c: char| c == '\u{FEFF}' || c.is_whitespace();
                let t = s.trim_end_matches(w);
                Ok(Some(self.alloc_recv_slice(&js_recv, &s, t)))
            }
            "startsWith" => {
                if self.is_regexp(arg0)? {
                    return Err(Thrown(
                        "TypeError: String.prototype.startsWith argument must not be a RegExp".into(),
                    ));
                }
                let needle = self.to_js_string(arg0)?;
                let len = unit_len(&s) as i64;
                let pos =
                    if args.len() >= 2 { self.to_integer_strict(args[1])?.clamp(0, len) } else { 0 }
                        as usize;
                // An ANCHORED position: a start in the middle of a surrogate pair
                // makes the spec substring begin with a trail surrogate, which a
                // well-formed needle can never match (only the empty one).
                let r = if ascii {
                    s[pos.min(s.len())..].starts_with(&needle)
                } else {
                    let (lo, hi) = crate::heap::unit_byte_bounds(&s, pos);
                    if lo != hi { needle.is_empty() } else { s[lo..].starts_with(&needle) }
                };
                Ok(Some(Value::bool(r)))
            }
            "endsWith" => {
                if self.is_regexp(arg0)? {
                    return Err(Thrown(
                        "TypeError: String.prototype.endsWith argument must not be a RegExp".into(),
                    ));
                }
                let needle = self.to_js_string(arg0)?;
                let len = unit_len(&s) as i64;
                let end = if args.len() >= 2 && args[1] != Value::UNDEFINED {
                    self.to_integer_strict(args[1])?.clamp(0, len)
                } else {
                    len
                } as usize;
                // ANCHORED end position: an end mid-pair leaves the spec substring
                // ending in a lead surrogate — only an empty needle can match.
                let r = if ascii {
                    s[..end.min(s.len())].ends_with(&needle)
                } else {
                    let (lo, hi) = crate::heap::unit_byte_bounds(&s, end);
                    if lo != hi { needle.is_empty() } else { s[..lo].ends_with(&needle) }
                };
                Ok(Some(Value::bool(r)))
            }
            "concat" => {
                // Each argument is ToString-coerced (honours @@toPrimitive/toString/
                // valueOf, throws on a Symbol), not rendered via display(). Built
                // as WTF-8 with seam canonicalization: a string argument joins
                // EXACTLY (its lone surrogates survive, and a trailing high +
                // leading low across arguments merges into the astral scalar).
                let mut out: Vec<u8> = js_recv.as_bytes().to_vec();
                for a in args {
                    let av = *a;
                    if av.is_heap() && self.heap.is_str_like(av.heap_index()) {
                        let part =
                            self.heap.str_wtf8_cow(av.heap_index()).map(|c| c.into_owned());
                        crate::heap::wtf8_push(&mut out, &part.unwrap_or_default());
                    } else {
                        let part = self.to_js_string(av)?;
                        crate::heap::wtf8_push(&mut out, part.as_bytes());
                    }
                }
                let js = crate::heap::JsStr::from_wtf8(out);
                Ok(Some(Value::heap(self.heap.alloc_js(js))))
            }
            "substr" => {
                // Legacy substr(start, length); negative start counts from the end.
                let len = unit_len(&s) as i64;
                let mut start = if args.is_empty() { 0 } else { self.to_integer_strict(arg0)? };
                if start < 0 {
                    start = (len + start).max(0);
                }
                let start = start.min(len) as usize;
                let avail = len as usize - start;
                let count = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    avail
                } else {
                    let c = self.to_integer_strict(args[1])?;
                    if c < 0 { 0 } else { (c as usize).min(avail) }
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
                let other = self.to_js_string(arg0)?;
                let locales = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let options = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let coll = self.make_intl(crate::vm::native::INTL_COLLATOR, locales, options)?;
                let resolved = self.intl_this(coll, crate::vm::native::INTL_COLLATOR, "compare")?;
                let ord = self.collator_compare(resolved, &s, &other);
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
                        "RangeError: The normalization form should be one of NFC, NFD, NFKC, NFKD.".into(),
                    ));
                }
                use unicode_normalization::UnicodeNormalization;
                let out: String = match form.as_str() {
                    "NFC" => s.nfc().collect(),
                    "NFD" => s.nfd().collect(),
                    "NFKC" => s.nfkc().collect(),
                    _ => s.nfkd().collect(),
                };
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
                if target as u64 > (1u64 << 28) {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                if cur >= target {
                    return Ok(Some(Value::heap(idx)));
                }
                // ToString(fillString) — a Symbol/abrupt fill throws (after the
                // length early-return above, matching the spec's StringPad order).
                // The filler goes through the EXACT string path when it is a
                // string value (its own lone surrogates survive).
                let fill_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pad: crate::heap::JsStr = if fill_arg != Value::UNDEFINED {
                    if fill_arg.is_heap() && self.heap.is_str_like(fill_arg.heap_index()) {
                        let fi = fill_arg.heap_index();
                        self.heap.flatten(fi);
                        match self.heap.get(fi) {
                            HeapObj::Str(js) => js.clone(),
                            _ => crate::heap::JsStr::new(String::new()),
                        }
                    } else {
                        crate::heap::JsStr::new(self.to_js_string(fill_arg)?)
                    }
                } else {
                    crate::heap::JsStr::new(" ".to_string())
                };
                if pad.units() == 0 {
                    return Ok(Some(Value::heap(idx)));
                }
                // StringPad truncates the repeated filler to (target - cur)
                // UNITS; a truncation that splits an astral filler char keeps
                // the REAL lead half (a 1-unit lone high surrogate).
                let mut padding: Vec<u8> = Vec::new();
                let mut need = target - cur;
                while need > 0 {
                    if need >= pad.units() {
                        crate::heap::wtf8_push(&mut padding, pad.as_bytes());
                        need -= pad.units();
                    } else {
                        let part = pad.slice_units(0, need);
                        crate::heap::wtf8_push(&mut padding, part.as_bytes());
                        need = 0;
                    }
                }
                // Join padding and receiver as WTF-8 (the seam may canonicalize:
                // e.g. a filler ending in a high surrogate against a receiver
                // starting with a low one).
                let mut out: Vec<u8> = Vec::with_capacity(padding.len() + js_recv.as_bytes().len());
                if name == "padStart" {
                    out.extend_from_slice(&padding);
                    crate::heap::wtf8_push(&mut out, js_recv.as_bytes());
                } else {
                    out.extend_from_slice(js_recv.as_bytes());
                    crate::heap::wtf8_push(&mut out, &padding);
                }
                let js = crate::heap::JsStr::from_wtf8(out);
                Ok(Some(Value::heap(self.heap.alloc_js(js))))
            }
            "replace" => {
                let r = self.string_replace_plain(&s, idx, arg0, args.get(1).copied().unwrap_or(Value::UNDEFINED), false)?;
                Ok(Some(r))
            }
            "replaceAll" => {
                let r = self.string_replace_plain(&s, idx, arg0, args.get(1).copied().unwrap_or(Value::UNDEFINED), true)?;
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
                let out = match lang
                    .and_then(|l| crate::vm::special_casing::transform_case(&s, l, upper))
                {
                    Some(mapped) => mapped,
                    None if upper => s.to_uppercase(),
                    None => s.to_lowercase(),
                };
                Ok(Some(self.alloc_str(out)))
            }
            "lastIndexOf" => {
                // ToString(searchString) before the position coercion (spec order).
                let needle = self.to_js_string(arg0)?;
                let len = unit_len(&s);
                // position: ToNumber, then NaN -> search the whole string (per
                // lastIndexOf), else ToInteger clamped to [0, len]. A unit cap.
                let cap = if args.len() >= 2 && args[1] != Value::UNDEFINED {
                    let np = self.to_number_coerce(args[1])?;
                    if np.is_nan() {
                        len
                    } else {
                        (np.trunc().max(0.0) as usize).min(len)
                    }
                } else {
                    len
                };
                let result: i64 = if needle.is_empty() {
                    cap as i64
                } else {
                    // Last OVERLAPPING byte match whose start is ≤ the cap. A cap
                    // that lands mid-pair floors to the pair's start (a match can
                    // begin at the pair's lead unit, never at its trail).
                    let cap_byte = crate::heap::unit_byte_bounds(&s, cap).0;
                    let mut best: Option<usize> = None;
                    let mut from = 0usize;
                    while let Some(p) = s[from..].find(&needle) {
                        let b = from + p;
                        if b > cap_byte {
                            break;
                        }
                        best = Some(b);
                        // Restart one scalar later so overlapping matches are seen.
                        from = b + s[b..].chars().next().map_or(1, |c| c.len_utf8());
                    }
                    best.map_or(-1, |b| b2u(&s, b) as i64)
                };
                Ok(Some(Value::num(result as f64)))
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
                let open = if let Some(aname) = attr {
                    // The attribute value is ToString(value) (can throw, e.g. a
                    // {toString(){throw}}), not the non-throwing display().
                    let aval = self.to_js_string(arg0)?.replace('"', "&quot;");
                    format!("<{tag} {aname}=\"{aval}\">")
                } else {
                    format!("<{tag}>")
                };
                Ok(Some(self.alloc_str(format!("{open}{s}</{tag}>"))))
            }
            _ => Ok(None),
        }
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
        let mut out: Vec<u8> = Vec::with_capacity(args.len());
        for &v in args {
            let u = crate::vm::helpers_num2::to_uint32(self.to_number_strict(v)?) as u16;
            crate::heap::wtf8_push_cp(&mut out, u as u32);
        }
        Ok(crate::heap::JsStr::from_wtf8(out))
    }

    /// String.prototype.replace / replaceAll with a NON-regexp searchValue.
    /// `s_idx` is the receiver string's heap index, `all` selects replaceAll.
    /// Delegates to a custom `searchValue[Symbol.replace]` if present, else does a
    /// plain substring replacement with full GetSubstitution ($-pattern) support
    /// and functional replacers.
    pub(crate) fn string_replace_plain(
        &mut self,
        s: &str,
        s_idx: u32,
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
        if all && search_v != Value::UNDEFINED && search_v != Value::NULL && self.is_regexp(search_v)? {
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
        let search = self.to_js_string(search_v)?;
        let functional = self.is_callable(repl_v);
        let repl_str = if functional { String::new() } else { self.to_js_string(repl_v)? };
        // Match byte offsets (non-overlapping). An empty searchValue matches at
        // every char boundary including the end (replaceAll), or just position 0.
        // (Spec: every UNIT boundary — the position between a surrogate pair's
        // halves is skipped here since the splice can't represent the halves;
        // exact once strings are WTF-8.)
        let positions: Vec<usize> = if search.is_empty() {
            if all {
                let mut v: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
                v.push(s.len());
                v
            } else {
                vec![0]
            }
        } else if all {
            s.match_indices(&search).map(|(i, _)| i).collect()
        } else {
            s.find(&search).into_iter().collect()
        };
        let mut out = String::new();
        let mut last = 0usize;
        for pos in positions {
            out.push_str(&s[last..pos]);
            if functional {
                let m = self.alloc_str(search.clone());
                // The replacer's position argument is a UNIT position.
                let off = Value::num(crate::heap::byte_to_units(s, pos) as f64);
                let sv = self.alloc_str(s.to_string());
                let r = self.call_value(repl_v, Value::UNDEFINED, &[m, off, sv])?;
                let rs = self.to_js_string(r)?;
                if rs.len() > (1usize << 28).saturating_sub(out.len()) {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                out.push_str(&rs);
            } else {
                let rep = self.expand_replacement(
                    &repl_str,
                    &search,
                    &[],
                    &[],
                    false, // a string search has no named captures: `$<…>` is literal
                    &s[..pos],
                    &s[pos + search.len()..],
                    (1usize << 28).saturating_sub(out.len()),
                )?;
                out.push_str(&rep);
            }
            last = pos + search.len();
        }
        out.push_str(&s[last..]);
        Ok(self.alloc_str(out))
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
fn case_map_exact(bytes: &[u8], upper: bool) -> crate::heap::JsStr {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.extend_from_slice(if upper { s.to_uppercase() } else { s.to_lowercase() }.as_bytes());
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                // The valid prefix is UTF-8 by construction.
                let s = std::str::from_utf8(&rest[..valid]).unwrap();
                out.extend_from_slice(if upper { s.to_uppercase() } else { s.to_lowercase() }.as_bytes());
                // Copy the invalid (lone-surrogate) bytes verbatim and resume.
                let skip = e.error_len().unwrap_or(rest.len() - valid);
                out.extend_from_slice(&rest[valid..valid + skip]);
                rest = &rest[valid + skip..];
            }
        }
    }
    crate::heap::JsStr::from_wtf8(out)
}
