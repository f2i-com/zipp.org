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
        // that is a no-op apart from a redundant property read.)
        let s = self.to_js_string(recv)?;
        let s_idx = self.alloc_str(s).heap_index();
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

    /// The 1-unit string content at unit position `i` (`charAt`/`at`/bracket
    /// index): the BMP scalar, or U+FFFD for a surrogate half (not yet
    /// representable alone — exact once strings are WTF-8).
    pub(crate) fn heap_unit_char(&self, idx: u32, i: usize) -> Option<char> {
        match self.heap.get(idx) {
            HeapObj::Str(js) => js.unit_char(i),
            _ => None,
        }
    }

    pub(crate) fn string_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        self.heap.flatten(idx); // materialize a rope receiver before reading it
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // Single-char index methods: read one char directly from the heap with NO
        // full-string clone (the clone below is O(n), so these would be O(n²) in a
        // per-char loop — `s.charCodeAt(i)` scanning is a very common idiom).
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
                let c = if i >= 0 { self.heap_unit_char(idx, i as usize) } else { None };
                return Ok(Some(self.alloc_str(c.map(|c| c.to_string()).unwrap_or_default())));
            }
            "at" => {
                let len = self.heap_str_units(idx) as i64;
                let i = self.to_integer_strict(arg0)?;
                let abs = if i < 0 { i + len } else { i };
                let c = if abs >= 0 && abs < len { self.heap_unit_char(idx, abs as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => self.alloc_str(c.to_string()),
                    None => Value::UNDEFINED,
                }));
            }
            _ => {}
        }
        // Other methods need an owned String (slice/replace/split/…).
        let (s, ascii) = match self.heap.get(idx) {
            HeapObj::Str(js) => (js.as_str().to_string(), js.is_ascii()),
            _ => return Ok(None),
        };
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
        // Substring by unit positions [a, b).
        let subu = |s: &str, a: usize, b: usize| -> String {
            if ascii {
                let (a, b) = (a.min(s.len()), b.min(s.len()));
                if a >= b {
                    String::new()
                } else {
                    s[a..b].to_string()
                }
            } else {
                crate::heap::slice_units_str(s, a, b)
            }
        };
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
            "toUpperCase" => Ok(Some(self.alloc_str(s.to_uppercase()))),
            "toLowerCase" => Ok(Some(self.alloc_str(s.to_lowercase()))),
            "slice" => {
                // Negative indices count from the end; computed in i64 so a
                // saturated ±Infinity (i64::MIN/MAX) clamps correctly (an `as i32`
                // would wrap Infinity to -1).
                let len = unit_len(&s) as i64;
                let norm = |i: i64| if i < 0 { len.saturating_add(i).max(0) } else { i.min(len) };
                let start = if args.is_empty() { 0 } else { norm(self.to_integer_strict(arg0)?) };
                // An absent OR explicitly-`undefined` end defaults to the string
                // length (ToIntegerOrInfinity is only applied to a defined end).
                let end = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    len
                } else {
                    norm(self.to_integer_strict(args[1])?)
                };
                let out = subu(&s, start as usize, end as usize);
                Ok(Some(self.alloc_str(out)))
            }
            "substring" => {
                // Each index clamps to [0,len] (negatives -> 0), then start/end swap
                // so start <= end (distinct from slice's negative-from-end mapping).
                let len = unit_len(&s) as i64;
                let s0 = if args.is_empty() { 0 } else { self.to_integer_strict(arg0)?.clamp(0, len) };
                // An absent OR explicitly-`undefined` end defaults to the length.
                let e0 = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    len
                } else {
                    self.to_integer_strict(args[1])?.clamp(0, len)
                };
                let (from, to) = if s0 <= e0 { (s0, e0) } else { (e0, s0) };
                let out = subu(&s, from as usize, to as usize);
                Ok(Some(self.alloc_str(out)))
            }
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
                Ok(Some(self.alloc_str(s.repeat(n_int as usize))))
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
                    if self.as_regexp(regexp).is_some() {
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
                let global =
                    matches!(self.heap.get(re), HeapObj::RegExp { flags, .. } if flags.contains('g'));
                let repl = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let out = self.regex_replace(&s, re, repl, global)?;
                Ok(Some(self.alloc_str(out)))
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
                    // No separator → the whole string as a single element (lim 0 → []).
                    if lim == 0 { Vec::new() } else { vec![self.alloc_str(s.clone())] }
                } else {
                    // ToString(separator) — runs a user toString (propagating a
                    // throw) and rejects a Symbol; after ToUint32(limit) and before
                    // the lim==0 early-out, matching the spec ordering.
                    let sep = self.to_js_string(arg0)?;
                    if lim == 0 {
                        Vec::new()
                    } else if sep.is_empty() {
                        // Split into 1-UNIT pieces (spec: code units). An astral
                        // scalar's halves degrade to U+FFFD until the WTF-8 stage.
                        crate::heap::unit_chars(&s)
                            .into_iter()
                            .take(lim)
                            .map(|c| self.alloc_str(c.to_string()))
                            .collect()
                    } else {
                        s.split(&sep).take(lim).map(|p| self.alloc_str(p.to_string())).collect()
                    }
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(parts)))))
            }
            // ECMAScript TrimString whitespace = Unicode White_Space + U+FEFF
            // (ZWNBSP/BOM), which Rust's char::is_whitespace excludes.
            "trim" => {
                let w = |c: char| c == '\u{FEFF}' || c.is_whitespace();
                Ok(Some(self.alloc_str(s.trim_matches(w).to_string())))
            }
            "trimStart" => {
                let w = |c: char| c == '\u{FEFF}' || c.is_whitespace();
                Ok(Some(self.alloc_str(s.trim_start_matches(w).to_string())))
            }
            "trimEnd" => {
                let w = |c: char| c == '\u{FEFF}' || c.is_whitespace();
                Ok(Some(self.alloc_str(s.trim_end_matches(w).to_string())))
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
                // valueOf, throws on a Symbol), not rendered via display().
                let mut out = s.clone();
                for a in args {
                    let part = self.to_js_string(*a)?;
                    out.push_str(&part);
                }
                Ok(Some(self.alloc_str(out)))
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
                let sub = subu(&s, start, start + count);
                Ok(Some(self.alloc_str(sub)))
            }
            "localeCompare" => {
                // No full Intl collation, but canonically-equivalent strings
                // MUST compare equal (spec: the comparison honors canonical
                // equivalence) - compare NFC normal forms.
                use unicode_normalization::UnicodeNormalization;
                let other = self.to_js_string(arg0)?;
                let a: String = s.nfc().collect();
                let b: String = other.nfc().collect();
                let ord = match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Ok(Some(Value::int(ord)))
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
            // Engine strings are valid UTF-8 (no lone surrogates), so always well-formed.
            "isWellFormed" => Ok(Some(Value::bool(true))),
            "toWellFormed" => Ok(Some(self.alloc_str(s.clone()))),
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
                    return Ok(Some(self.alloc_str(s.clone())));
                }
                // ToString(fillString) — a Symbol/abrupt fill throws (after the
                // length early-return above, matching the spec's StringPad order).
                let pad = if args.len() >= 2 && args[1] != Value::UNDEFINED {
                    self.to_js_string(args[1])?
                } else {
                    " ".to_string()
                };
                if pad.is_empty() {
                    return Ok(Some(self.alloc_str(s.clone())));
                }
                // StringPad truncates the repeated filler to (target - cur) UNITS;
                // a truncation that splits an astral filler char keeps U+FFFD for
                // the lead half (a lone surrogate isn't representable yet).
                let mut padding = String::new();
                let mut need = target - cur;
                let mut fill = pad.chars().cycle();
                while need > 0 {
                    let c = fill.next().unwrap(); // non-empty: cycle never ends
                    let n = crate::heap::char_units(c);
                    if n > need {
                        padding.push('\u{FFFD}');
                        break;
                    }
                    padding.push(c);
                    need -= n;
                }
                let out = if name == "padStart" {
                    format!("{padding}{s}")
                } else {
                    format!("{s}{padding}")
                };
                Ok(Some(self.alloc_str(out)))
            }
            "replace" => {
                let r = self.string_replace_plain(&s, idx, arg0, args.get(1).copied().unwrap_or(Value::UNDEFINED), false)?;
                Ok(Some(r))
            }
            "replaceAll" => {
                let r = self.string_replace_plain(&s, idx, arg0, args.get(1).copied().unwrap_or(Value::UNDEFINED), true)?;
                Ok(Some(r))
            }
            // toLocale* default to the locale-independent case mappings.
            "toLocaleUpperCase" => Ok(Some(self.alloc_str(s.to_uppercase()))),
            "toLocaleLowerCase" => Ok(Some(self.alloc_str(s.to_lowercase()))),
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
    /// propagates), coerced in argument order. ADJACENT (high, low) surrogate
    /// halves combine into the astral scalar they encode — this is how astral
    /// chars enter via fromCharCode. A LONE surrogate half degrades to U+FFFD
    /// (not representable until strings are WTF-8 — documented stage-1 limit).
    pub(crate) fn string_from_char_codes(&mut self, args: &[Value]) -> Result<String, Thrown> {
        let mut s = String::new();
        let mut pending: Option<u16> = None; // a high half awaiting its low
        for &v in args {
            let u = crate::vm::helpers_num2::to_uint32(self.to_number_strict(v)?) as u16;
            if let Some(h) = pending.take() {
                if (0xDC00..=0xDFFF).contains(&u) {
                    let cp = 0x10000 + (((h as u32) - 0xD800) << 10) + ((u as u32) - 0xDC00);
                    s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    continue;
                }
                s.push('\u{FFFD}'); // the high half stayed lone
            }
            match u {
                0xD800..=0xDBFF => pending = Some(u),
                0xDC00..=0xDFFF => s.push('\u{FFFD}'), // lone low half
                _ => s.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}')),
            }
        }
        if pending.is_some() {
            s.push('\u{FFFD}'); // trailing lone high half
        }
        Ok(s)
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
                );
                out.push_str(&rep);
            }
            last = pos + search.len();
        }
        out.push_str(&s[last..]);
        Ok(self.alloc_str(out))
    }

}
