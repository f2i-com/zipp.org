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

    /// The i-th char of a flat string by heap index, WITHOUT cloning the string —
    /// O(1) for ASCII (i-th byte), else an O(i) scalar scan. `None` if out of range
    /// or not a flat string. (A full-string clone here would make `charCodeAt(i)`
    /// in a loop O(n²) in the string length — the real cost of these methods.)
    pub(crate) fn heap_char_at(&self, idx: u32, i: usize) -> Option<char> {
        match self.heap.get(idx) {
            HeapObj::Str(js) => {
                if js.ascii {
                    js.bytes.as_bytes().get(i).map(|&b| b as char)
                } else {
                    js.bytes.chars().nth(i)
                }
            }
            _ => None,
        }
    }

    /// Char length of a flat string by heap index — O(1) for ASCII.
    pub(crate) fn heap_char_len(&self, idx: u32) -> usize {
        match self.heap.get(idx) {
            HeapObj::Str(js) => {
                if js.ascii {
                    js.bytes.len()
                } else {
                    js.bytes.chars().count()
                }
            }
            _ => 0,
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
                let i = self.to_integer_or_zero(arg0)?;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => Value::int(c as i32),
                    None => Value::num(f64::NAN),
                }));
            }
            "codePointAt" => {
                let i = self.to_integer_or_zero(arg0)?;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => Value::int(c as i32),
                    None => Value::UNDEFINED,
                }));
            }
            "charAt" => {
                let i = self.to_integer_or_zero(arg0)?;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(self.alloc_str(c.map(|c| c.to_string()).unwrap_or_default())));
            }
            "at" => {
                let len = self.heap_char_len(idx) as i64;
                let i = self.to_integer_or_zero(arg0)?;
                let abs = if i < 0 { i + len } else { i };
                let c = if abs >= 0 && abs < len { self.heap_char_at(idx, abs as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => self.alloc_str(c.to_string()),
                    None => Value::UNDEFINED,
                }));
            }
            _ => {}
        }
        // Other methods need an owned String (slice/replace/split/…).
        let (s, ascii) = match self.heap.get(idx) {
            HeapObj::Str(js) => (js.bytes.clone(), js.ascii),
            _ => return Ok(None),
        };
        let char_len = |s: &str| -> usize {
            if ascii {
                s.len()
            } else {
                s.chars().count()
            }
        };
        match name {
            "indexOf" => {
                // ToString(searchString) (honours @@toPrimitive/toString/valueOf,
                // throws on a Symbol) BEFORE ToInteger(position) — spec arg order.
                let needle = self.to_js_string(arg0)?;
                // Optional fromIndex (ToInteger, a char position) to start at.
                let from = if args.len() >= 2 {
                    self.to_integer_or_zero(args[1])?.max(0) as usize
                } else {
                    0
                };
                let byte_from = s.char_indices().nth(from).map(|(b, _)| b).unwrap_or(s.len());
                let pos = s[byte_from..]
                    .find(&needle)
                    .map(|b| s[..byte_from + b].chars().count() as i32)
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
                let len = char_len(&s) as i64;
                let pos = if args.len() >= 2 {
                    self.to_integer_or_zero(args[1])?.clamp(0, len)
                } else {
                    0
                } as usize;
                let byte = s.char_indices().nth(pos).map(|(b, _)| b).unwrap_or(s.len());
                Ok(Some(Value::bool(s[byte..].contains(&needle))))
            }
            "toUpperCase" => Ok(Some(self.alloc_str(s.to_uppercase()))),
            "toLowerCase" => Ok(Some(self.alloc_str(s.to_lowercase()))),
            "slice" => {
                // Negative indices count from the end; computed in i64 so a
                // saturated ±Infinity (i64::MIN/MAX) clamps correctly (an `as i32`
                // would wrap Infinity to -1).
                let len = char_len(&s) as i64;
                let norm = |i: i64| if i < 0 { len.saturating_add(i).max(0) } else { i.min(len) };
                let start = if args.is_empty() { 0 } else { norm(self.to_integer_or_zero(arg0)?) };
                let end = if args.len() < 2 { len } else { norm(self.to_integer_or_zero(args[1])?) };
                let out: String = if start < end {
                    s.chars().skip(start as usize).take((end - start) as usize).collect()
                } else {
                    String::new()
                };
                Ok(Some(self.alloc_str(out)))
            }
            "substring" => {
                // Each index clamps to [0,len] (negatives -> 0), then start/end swap
                // so start <= end (distinct from slice's negative-from-end mapping).
                let len = char_len(&s) as i64;
                let s0 = if args.is_empty() { 0 } else { self.to_integer_or_zero(arg0)?.clamp(0, len) };
                let e0 = if args.len() < 2 { len } else { self.to_integer_or_zero(args[1])?.clamp(0, len) };
                let (from, to) = if s0 <= e0 { (s0, e0) } else { (e0, s0) };
                let out: String = s.chars().skip(from as usize).take((to - from) as usize).collect();
                Ok(Some(self.alloc_str(out)))
            }
            "repeat" => {
                let n = self.to_integer_or_zero(arg0)?;
                if n < 0 {
                    return Err(Thrown("RangeError: Invalid count value".into()));
                }
                // Bound the result (an unbounded build would hang / OOM) — a too-long
                // string (or count===+Infinity, saturated to i64::MAX here) is a
                // RangeError per spec. Empty string repeats to "" for any count.
                if (n as f64) * (s.len() as f64) > (1u64 << 28) as f64 {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                Ok(Some(self.alloc_str(s.repeat(n as usize))))
            }
            "search" => {
                let re = self.to_regexp_arg(arg0)?;
                Ok(Some(self.regexp_search_impl(Value::heap(re), Value::heap(idx))?))
            }
            "match" => {
                let re = self.to_regexp_arg(arg0)?;
                Ok(Some(self.regexp_match_impl(re, Value::heap(idx))?))
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
            "replaceAll" if self.as_regexp(arg0).is_some() => {
                let re = self.as_regexp(arg0).unwrap();
                let global =
                    matches!(self.heap.get(re), HeapObj::RegExp { flags, .. } if flags.contains('g'));
                if !global {
                    return Err(Thrown(
                        "TypeError: replaceAll must be called with a global RegExp".into(),
                    ));
                }
                let repl = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let out = self.regex_replace(&s, re, repl, true)?;
                Ok(Some(self.alloc_str(out)))
            }
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
                        s.chars().take(lim).map(|c| self.alloc_str(c.to_string())).collect()
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
                let len = char_len(&s) as i64;
                let pos =
                    if args.len() >= 2 { self.to_integer_or_zero(args[1])?.clamp(0, len) } else { 0 }
                        as usize;
                let byte = s.char_indices().nth(pos).map(|(b, _)| b).unwrap_or(s.len());
                Ok(Some(Value::bool(s[byte..].starts_with(&needle))))
            }
            "endsWith" => {
                if self.is_regexp(arg0)? {
                    return Err(Thrown(
                        "TypeError: String.prototype.endsWith argument must not be a RegExp".into(),
                    ));
                }
                let needle = self.to_js_string(arg0)?;
                let len = char_len(&s) as i64;
                let end = if args.len() >= 2 && args[1] != Value::UNDEFINED {
                    self.to_integer_or_zero(args[1])?.clamp(0, len)
                } else {
                    len
                } as usize;
                let byte = s.char_indices().nth(end).map(|(b, _)| b).unwrap_or(s.len());
                Ok(Some(Value::bool(s[..byte].ends_with(&needle))))
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
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i64;
                let mut start = if args.is_empty() { 0 } else { self.to_integer_or_zero(arg0)? };
                if start < 0 {
                    start = (len + start).max(0);
                }
                let start = start.min(len) as usize;
                let avail = chars.len() - start;
                let count = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    avail
                } else {
                    let c = self.to_integer_or_zero(args[1])?;
                    if c < 0 { 0 } else { (c as usize).min(avail) }
                };
                let sub: String = chars[start..start + count].iter().collect();
                Ok(Some(self.alloc_str(sub)))
            }
            "localeCompare" => {
                // No Intl: a code-unit ordinal comparison (the default approximation).
                let other = self.display(arg0);
                let ord = match s.as_str().cmp(other.as_str()) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Ok(Some(Value::int(ord)))
            }
            "normalize" => {
                // Validate the form; engine strings are already normalized for ASCII
                // (full Unicode normalization isn't modelled).
                let form = if args.is_empty() || arg0 == Value::UNDEFINED {
                    "NFC".to_string()
                } else {
                    self.display(arg0)
                };
                if !matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
                    return Err(Thrown(
                        "RangeError: The normalization form should be one of NFC, NFD, NFKC, NFKD.".into(),
                    ));
                }
                Ok(Some(self.alloc_str(s.clone())))
            }
            // Engine strings are valid UTF-8 (no lone surrogates), so always well-formed.
            "isWellFormed" => Ok(Some(Value::bool(true))),
            "toWellFormed" => Ok(Some(self.alloc_str(s.clone()))),
            // String.prototype.valueOf/toString return the string primitive itself
            // (used by a boxed String's valueOf/toString after unwrapping).
            "valueOf" | "toString" => Ok(Some(Value::heap(idx))),
            "padStart" | "padEnd" => {
                let cur = char_len(&s);
                let t = self.to_integer_or_zero(arg0)?;
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
                let padchars: Vec<char> = pad.chars().collect();
                if padchars.is_empty() {
                    return Ok(Some(self.alloc_str(s.clone())));
                }
                let mut padding = String::new();
                for k in 0..(target - cur) {
                    padding.push(padchars[k % padchars.len()]);
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
                let sc: Vec<char> = s.chars().collect();
                let nc: Vec<char> = needle.chars().collect();
                let len = sc.len();
                // position: ToNumber, then NaN -> search the whole string (per
                // lastIndexOf), else ToInteger clamped to [0, len].
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
                let mut result: i64 = -1;
                if nc.is_empty() {
                    result = cap.min(len) as i64;
                } else if nc.len() <= len {
                    let max_start = (len - nc.len()).min(cap);
                    for start in (0..=max_start).rev() {
                        if sc[start..start + nc.len()] == nc[..] {
                            result = start as i64;
                            break;
                        }
                    }
                }
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
                    let aval = self.display(arg0).replace('"', "&quot;");
                    format!("<{tag} {aname}=\"{aval}\">")
                } else {
                    format!("<{tag}>")
                };
                Ok(Some(self.alloc_str(format!("{open}{s}</{tag}>"))))
            }
            _ => Ok(None),
        }
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
        // If searchValue is an OBJECT with a @@replace method, defer to it. The
        // `is_object_value` guard matters: per spec the `@@replace` property is
        // only accessed when searchValue is an Object, so a primitive searchValue
        // (a number/string/boolean) must NOT trigger a `Number.prototype[@@replace]`
        // getter etc.
        if self.is_object_value(search_v) {
            let m = self.get_prop(search_v, "@@replace")?;
            if self.is_callable(m) {
                let sval = Value::heap(s_idx);
                return self.call_value(m, search_v, &[sval, repl_v]);
            }
        }
        let search = self.to_js_string(search_v)?;
        let functional = self.is_callable(repl_v);
        let repl_str = if functional { String::new() } else { self.to_js_string(repl_v)? };
        // Match byte offsets (non-overlapping). An empty searchValue matches at
        // every char boundary including the end (replaceAll), or just position 0.
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
                let off = Value::num(byte_to_char(s, pos) as f64);
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
