#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// `new Proxy(target, handler)` — both must be objects.
    pub(crate) fn make_proxy(&mut self, target: Value, handler: Value) -> Result<Value, Thrown> {
        if !self.is_object_value(target) || !self.is_object_value(handler) {
            return Err(Thrown(
                "TypeError: Cannot create proxy with a non-object as target or handler".into(),
            ));
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Proxy { target, handler, revoked: false })))
    }

    pub(crate) fn proxy_parts(&self, idx: u32) -> Option<(Value, Value, bool)> {
        match self.heap.get(idx) {
            HeapObj::Proxy { target, handler, revoked } => Some((*target, *handler, *revoked)),
            _ => None,
        }
    }

    /// Reconstruct a property KEY as a Value (a Symbol for an `@@`-encoded key,
    /// else a string) — so a Proxy trap / Reflect receives the real key.
    pub(crate) fn key_to_value(&mut self, key: &str) -> Value {
        if key.starts_with("@@") {
            if let Some(&sym) = self.symbol_keys.get(key) {
                return sym;
            }
        }
        self.alloc_str(key.to_string())
    }

    /// Look up a Proxy handler trap by name; `Ok(Some(fn))` if it's callable,
    /// `Ok(None)` to fall through to the target. A non-callable non-undefined trap
    /// is a TypeError. (`revoked` is checked by the caller.)
    pub(crate) fn proxy_trap(&mut self, handler: Value, name: &str) -> Result<Option<Value>, Thrown> {
        let t = self.get_prop(handler, name)?;
        if t.is_undefined() || t.is_null() {
            Ok(None)
        } else if self.is_callable(t) {
            Ok(Some(t))
        } else {
            Err(Thrown(format!("TypeError: proxy handler's {name} trap is not a function")))
        }
    }

    pub(crate) fn set_regexp_last_index(&mut self, idx: u32, n: usize) {
        if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(idx) {
            *last_index = Value::num(n as f64);
        }
    }

    /// True when `String.prototype.replace`'s internal regex fast path is
    /// UNOBSERVABLE for instance `re`: its [[Prototype]] is exactly
    /// %RegExp.prototype%, it has no own exec/flags/@@replace overrides, and
    /// the prototype's `exec` / `@@replace` are still the intrinsic natives.
    /// Anything else (a subclass instance, a patched prototype) must run the
    /// full observable @@replace protocol.
    pub(crate) fn regexp_replace_fast_ok(&self, re: u32) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| {
            m.pos("exec").is_some() || m.pos("flags").is_some() || m.pos("@@replace").is_some()
        }) {
            return false;
        }
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => {
                let intrinsic = |k: &str, id: u16| {
                    m.pos(k).is_some_and(|i| {
                        !m.attrs[i].accessor
                            && m.vals[i].is_heap()
                            && matches!(self.heap.get(m.vals[i].heap_index()),
                                        HeapObj::Native(n) if *n == id)
                    })
                };
                intrinsic("exec", native::REGEXP_EXEC)
                    && intrinsic("@@replace", native::REGEXP_SYM_REPLACE)
            }
            _ => false,
        }
    }

    /// The heap index if `v` is a RegExp, else None.
    pub(crate) fn as_regexp(&self, v: Value) -> Option<u32> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::RegExp { .. }) {
            Some(v.heap_index())
        } else {
            None
        }
    }

    /// Coerce a `String.prototype.match`/`search` argument to a RegExp: a RegExp
    /// passes through; anything else becomes `new RegExp(arg)`.
    pub(crate) fn to_regexp_arg(&mut self, v: Value) -> Result<u32, Thrown> {
        if let Some(i) = self.as_regexp(v) {
            return Ok(i);
        }
        let p = if v.is_undefined() { self.alloc_str(String::new()) } else { v };
        Ok(self.build_regexp(p, Value::UNDEFINED)?.heap_index())
    }

    /// Expand a `String.prototype.replace` string template against a match: `$&`
    /// (whole), `` $` ``/`$'` (pre/post), `$N`/`$NN` (group), `$<name>` (named), `$$`.
    pub(crate) fn expand_replacement(
        &self,
        tmpl: &str,
        whole: &str,
        groups: &[Option<String>],
        named: &[(String, Option<String>)],
        named_defined: bool,
        pre: &str,
        post: &str,
    ) -> String {
        let mut out = String::with_capacity(tmpl.len());
        let bytes = tmpl.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                let c = bytes[i + 1];
                match c {
                    b'$' => {
                        out.push('$');
                        i += 2;
                    }
                    b'&' => {
                        out.push_str(whole);
                        i += 2;
                    }
                    b'`' => {
                        out.push_str(pre);
                        i += 2;
                    }
                    b'\'' => {
                        out.push_str(post);
                        i += 2;
                    }
                    b'<' => {
                        // `$<name>` substitutes the named capture (or "" if absent)
                        // when named captures are present; otherwise (no groups
                        // object / namedCaptures undefined) "$<" is a literal.
                        if !named_defined {
                            out.push('$');
                            i += 1;
                        } else if let Some(end) = tmpl[i + 2..].find('>') {
                            let name = &tmpl[i + 2..i + 2 + end];
                            if let Some((_, Some(g))) = named.iter().find(|(n, _)| n == name) {
                                out.push_str(g);
                            }
                            i += 2 + end + 1;
                        } else {
                            out.push('$');
                            i += 1;
                        }
                    }
                    b'0'..=b'9' => {
                        // One or two digits; prefer the two-digit group if valid.
                        let d1 = (c - b'0') as usize;
                        let two = if i + 2 < bytes.len() && bytes[i + 2].is_ascii_digit() {
                            Some(d1 * 10 + (bytes[i + 2] - b'0') as usize)
                        } else {
                            None
                        };
                        if let Some(n) = two.filter(|&n| n >= 1 && n <= groups.len()) {
                            if let Some(g) = &groups[n - 1] {
                                out.push_str(g);
                            }
                            i += 3;
                        } else if d1 >= 1 && d1 <= groups.len() {
                            if let Some(g) = &groups[d1 - 1] {
                                out.push_str(g);
                            }
                            i += 2;
                        } else {
                            out.push('$');
                            i += 1;
                        }
                    }
                    _ => {
                        out.push('$');
                        i += 1;
                    }
                }
            } else {
                // copy one UTF-8 char
                let ch = tmpl[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
        out
    }

    /// RegExp instance property reads: `lastIndex`, `source` (empty → "(?:)"),
    /// `flags`, and the per-flag booleans; methods delegate to RegExp.prototype.
    /// EscapeRegExpPattern: render `source` so it round-trips between two `/`
    /// delimiters — escape a bare `/` and the line terminators, pass `\x` pairs
    /// through verbatim, and map the empty pattern to `(?:)`.
    pub(crate) fn escaped_source(&self, source: &str) -> String {
        if source.is_empty() {
            return "(?:)".to_string();
        }
        let chars: Vec<char> = source.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' && i + 1 < chars.len() {
                out.push('\\');
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            match c {
                '/' => out.push_str("\\/"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\u{2028}' => out.push_str("\\u2028"),
                '\u{2029}' => out.push_str("\\u2029"),
                _ => out.push(c),
            }
            i += 1;
        }
        out
    }

    /// WTF-8 twin of [`escaped_source`], for a pattern holding lone surrogates
    /// (`regexp_exact_source` side table): operates on code points over the
    /// exact bytes — same escapes (`/`, line terminators, `\x` pairs verbatim,
    /// empty → `(?:)`) — and returns WTF-8 bytes for the WTF-8 string
    /// constructor. A lone surrogate passes through as itself (the spec's
    /// EscapeRegExpPattern leaves it untouched), which `escaped_source` could
    /// never produce from its lossy `&str` view.
    pub(crate) fn escaped_source_wtf8(&self, bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return b"(?:)".to_vec();
        }
        let cps: Vec<u32> = crate::heap::wtf8_code_points(bytes).collect();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 4);
        let mut i = 0;
        while i < cps.len() {
            let c = cps[i];
            if c == u32::from('\\') && i + 1 < cps.len() {
                crate::heap::wtf8_push_cp(&mut out, c);
                crate::heap::wtf8_push_cp(&mut out, cps[i + 1]);
                i += 2;
                continue;
            }
            match c {
                0x2F => out.extend_from_slice(b"\\/"),
                0x0A => out.extend_from_slice(b"\\n"),
                0x0D => out.extend_from_slice(b"\\r"),
                0x2028 => out.extend_from_slice(b"\\u2028"),
                0x2029 => out.extend_from_slice(b"\\u2029"),
                _ => crate::heap::wtf8_push_cp(&mut out, c),
            }
            i += 1;
        }
        out
    }

    /// The `source` string Value for the RegExp at `idx` whose lossy escaped
    /// source is `src`: exact-WTF-8 when the side table has the pattern's
    /// exact bytes (lone surrogates round-trip), else the plain lossy string.
    pub(crate) fn regexp_source_value(&mut self, idx: u32, src: &str) -> Value {
        if let Some(b) = self.regexp_exact_source.get(&idx) {
            let esc = self.escaped_source_wtf8(b);
            let js = crate::heap::JsStr::from_wtf8(esc);
            return Value::heap(self.heap.alloc_js(js));
        }
        let s = self.escaped_source(src);
        self.alloc_str(s)
    }

    /// RegExp.prototype[Symbol.search] core: reset lastIndex to 0, exec, restore
    /// lastIndex, return the match index or -1. Shared by String.prototype.search.
    pub(crate) fn regexp_search_impl(&mut self, rx: Value, input: Value) -> Result<Value, Thrown> {
        // @@search (22.2.6.12) is spec-generic over any Object `rx`: save lastIndex
        // (Get), zero it (Set) unless already 0, RegExpExec, restore it if exec
        // changed it — all via the observable get/set_prop protocol, honouring a
        // user lastIndex getter/setter and a custom `exec`.
        let prev = self.get_prop(rx, "lastIndex")?;
        let zero = Value::int(0);
        if !self.same_value(prev, zero) {
            self.set_prop(rx, "lastIndex", zero, true)?;
        }
        let result = self.regexp_exec_abstract(rx.heap_index(), input)?;
        let cur = self.get_prop(rx, "lastIndex")?;
        if !self.same_value(cur, prev) {
            self.set_prop(rx, "lastIndex", prev, true)?;
        }
        if result == Value::NULL {
            return Ok(Value::int(-1));
        }
        self.get_prop(result, "index")
    }

    /// RegExp.prototype[Symbol.replace] (ES 22.2.6.11) — the OBSERVABLE protocol:
    /// generic over any Object `rx`, honouring a user `exec`/`flags`/`lastIndex`,
    /// reading each result's `0`/`length`/`index`/group-N/`groups` via Get, and
    /// building the replacement from THOSE values. Reuses `regexp_exec_abstract` so a
    /// user `exec` governs the matches. All positions (`index`, lastIndex, slice
    /// bounds, the replacer's offset argument) are UTF-16 unit indices.
    pub(crate) fn regexp_symbol_replace(
        &mut self,
        rx: Value,
        string: Value,
        replace_value: Value,
    ) -> Result<Value, Thrown> {
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(string)?;
        // Encode ONCE; every position below indexes this unit buffer.
        let u16s: Vec<u16> = self.value_units(s_val);
        let length_s = u16s.len();
        // `s_val` and `results` live in Rust locals across exec/replacer
        // re-entries — hold GC off for the whole protocol.
        let _gc = self.gc_lock_guard();
        let functional = self.is_callable(replace_value);
        let replace_str = if functional { String::new() } else { self.to_js_string(replace_value)? };
        // flags / global / fullUnicode are observable (Get, ToString).
        let flags_v = self.get_prop(rx, "flags")?;
        let flags = self.to_js_string(flags_v)?;
        let global = flags.contains('g');
        // fullUnicode (`u`/`v`) selects code-point AdvanceStringIndex.
        let full_unicode = flags.contains('u') || flags.contains('v');
        if global {
            self.set_prop(rx, "lastIndex", Value::int(0), true)?;
        }
        // Collect all exec results through the exec protocol (honouring user `exec`).
        let mut results: Vec<Value> = Vec::new();
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            let result = self.regexp_exec_abstract(rx.heap_index(), s_val)?;
            if result == Value::NULL {
                break;
            }
            results.push(result);
            if !global {
                break;
            }
            // An empty match advances lastIndex so the loop makes progress.
            let match0 = self.get_prop(result, "0")?;
            if self.to_js_string(match0)?.is_empty() {
                let li_v = self.get_prop(rx, "lastIndex")?;
                // ToLength: clamp to 2^53-1 BEFORE the advance.
                let this_index =
                    self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1) as usize;
                let next = advance_string_index(&u16s, this_index, full_unicode);
                self.set_prop(rx, "lastIndex", Value::num(next as f64), true)?;
            }
        }
        // Build the accumulated result (WTF-8 — subject slices stay exact),
        // reading each match's fields via Get.
        let mut accumulated: Vec<u8> = Vec::new();
        let mut next_pos: usize = 0;
        for result in results {
            let len_v = self.get_prop(result, "length")?;
            let n_captures = (self.to_integer_or_zero(len_v)?.max(0) as usize).saturating_sub(1);
            let matched_v = self.get_prop(result, "0")?;
            // ToString(Get(result,"0")) — IDENTITY for a string value; its UNIT
            // length determines how far this match consumes the subject.
            let matched_val = self.to_str_value(matched_v)?;
            let match_len = self.heap.str_units(matched_val.heap_index()).unwrap_or(0);
            let pos_v = self.get_prop(result, "index")?;
            let position = self.to_integer_or_zero(pos_v)?.clamp(0, length_s as i64) as usize;
            let mut captures: Vec<Option<String>> = Vec::with_capacity(n_captures);
            for n in 1..=n_captures {
                let cap_v = self.get_prop(result, &n.to_string())?;
                captures.push(if cap_v == Value::UNDEFINED {
                    None
                } else {
                    Some(self.to_js_string(cap_v)?)
                });
            }
            let named_v = self.get_prop(result, "groups")?;
            let named_defined = named_v != Value::UNDEFINED;
            // Replacement bytes (WTF-8): the functional path appends a returned
            // string's EXACT bytes; the template path expands over lossy views.
            let replacement: Vec<u8> = if functional {
                let mut argv: Vec<Value> = Vec::with_capacity(n_captures + 4);
                argv.push(matched_val);
                for c in &captures {
                    argv.push(match c {
                        Some(g) => self.alloc_str(g.clone()),
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(position as f64));
                argv.push(s_val);
                if named_defined {
                    argv.push(named_v);
                }
                let r = self.call_value(replace_value, Value::UNDEFINED, &argv)?;
                let rv = self.to_str_value(r)?;
                self.heap
                    .str_wtf8_cow(rv.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default()
            } else {
                // GetSubstitution: read the named-capture group object's own props.
                // Step l.i.1 — when `groups` is not undefined it is ToObject'd, so a
                // primitive (e.g. a string `groups`) is boxed and its properties
                // (`$<length>` etc.) become readable; ToObject(null) throws.
                let named_list: Vec<(String, Option<String>)> = if named_defined {
                    // ToObject(namedCaptures): null throws a TypeError (the public
                    // Object(null) would return {}, but this is the internal op).
                    self.require_object_coercible(named_v)?;
                    let obj = self.to_object(named_v)?;
                    // GetSubstitution reads EXACTLY the template's `$<name>`
                    // groups via Get — through the PROTOTYPE chain, so an
                    // inherited group property resolves (groups-object-subclass)
                    // and a missing one substitutes the empty string.
                    let mut v: Vec<(String, Option<String>)> = Vec::new();
                    let mut rest = replace_str.as_str();
                    while let Some(p) = rest.find("$<") {
                        rest = &rest[p + 2..];
                        let Some(e) = rest.find('>') else { break };
                        let name = rest[..e].to_string();
                        rest = &rest[e + 1..];
                        if !v.iter().any(|(n, _)| *n == name) {
                            let val = self.get_prop(obj, &name)?;
                            let sv = if val == Value::UNDEFINED {
                                None
                            } else {
                                Some(self.to_js_string(val)?)
                            };
                            v.push((name, sv));
                        }
                    }
                    v
                } else {
                    Vec::new()
                };
                let matched_lossy = self
                    .heap
                    .str_cow(matched_val.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                let pre = String::from_utf16_lossy(&u16s[..position]);
                let post_start = (position + match_len).min(length_s);
                let post = String::from_utf16_lossy(&u16s[post_start..]);
                self.expand_replacement(
                    &replace_str,
                    &matched_lossy,
                    &captures,
                    &named_list,
                    named_defined,
                    &pre,
                    &post,
                )
                .into_bytes()
            };
            if position >= next_pos {
                push_units(&mut accumulated, &u16s[next_pos..position]);
                crate::heap::wtf8_push(&mut accumulated, &replacement);
                next_pos = position + match_len;
            }
        }
        if next_pos < length_s {
            push_units(&mut accumulated, &u16s[next_pos..]);
        }
        Ok(Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(accumulated))))
    }

    /// RegExpExec (ES 22.2.7.1): the exec PROTOCOL. When the regex has a callable
    /// own/inherited `exec` (honouring a user override), call it with the subject
    /// string and require an Object-or-null result; otherwise fall back to the
    /// builtin RegExpBuiltinExec. The `@@match`/`@@search` (non-global) cores route
    /// through this so a custom `re.exec` governs the result.
    pub(crate) fn regexp_exec_abstract(&mut self, re: u32, input: Value) -> Result<Value, Thrown> {
        let re_v = Value::heap(re);
        let exec = self.get_prop(re_v, "exec")?;
        if self.is_callable(exec) {
            // ToString(S) — IDENTITY for a string value (exact WTF-8; a lossy
            // copy would strip lone surrogates before exec ever sees them).
            let s = self.to_str_value(input)?;
            let r = self.call_value(exec, re_v, &[s])?;
            let is_object = r.is_heap()
                && !matches!(
                    self.heap.get(r.heap_index()),
                    HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
                );
            if r != Value::NULL && !is_object {
                return Err(Thrown(
                    "TypeError: RegExp exec method returned something other than an Object or null"
                        .into(),
                ));
            }
            return Ok(r);
        }
        self.regexp_exec(re, input)
    }

    /// RegExp.prototype[Symbol.match] core: a non-global regex returns the exec
    /// result (array or null); a global regex returns the array of matched
    /// substrings (or null) and resets lastIndex. Shared by String.match.
    pub(crate) fn regexp_match_impl(&mut self, re: u32, input: Value) -> Result<Value, Thrown> {
        // OBSERVABLE @@match (22.2.6.8), generic over any Object `rx`: read
        // ToString(Get(rx,"flags")); a non-global match is just RegExpExec; a global
        // match loops RegExpExec (honouring a user `exec`) collecting ToString(Get(
        // result,"0")), resets lastIndex first, and advances past an empty match.
        let rx = Value::heap(re);
        let flags_v = self.get_prop(rx, "flags")?;
        let flags = self.to_js_string(flags_v)?;
        if !flags.contains('g') {
            return self.regexp_exec_abstract(re, input);
        }
        // fullUnicode (`u`/`v`) selects code-point AdvanceStringIndex.
        let full_unicode = flags.contains('u') || flags.contains('v');
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(input)?;
        // Unit buffer for the empty-match AdvanceStringIndex step.
        let u16s: Vec<u16> = self.value_units(s_val);
        // `s_val`/`elems` live in Rust locals across exec re-entries.
        let _gc = self.gc_lock_guard();
        self.set_prop(rx, "lastIndex", Value::int(0), false)?;
        let mut elems: Vec<Value> = Vec::new();
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            let result = self.regexp_exec_abstract(re, s_val)?;
            if result == Value::NULL {
                break;
            }
            let m0 = self.get_prop(result, "0")?;
            // ToString(Get(result,"0")) — IDENTITY for a string value, so a
            // lone-surrogate match survives into the result array.
            let m0_val = self.to_str_value(m0)?;
            let is_empty = self.heap.str_units(m0_val.heap_index()) == Some(0);
            elems.push(m0_val);
            if is_empty {
                let li_v = self.get_prop(rx, "lastIndex")?;
                // ToLength: clamp to 2^53-1 BEFORE the advance.
                let this_index =
                    self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1) as usize;
                let next = advance_string_index(&u16s, this_index, full_unicode);
                self.set_prop(rx, "lastIndex", Value::num(next as f64), true)?;
            }
        }
        if elems.is_empty() {
            return Ok(Value::NULL);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(elems))))
    }

    /// RegExp.prototype[Symbol.split] core (simplified — no capture groups in the
    /// output yet). Shared by String.prototype.split for a regex separator.
    pub(crate) fn regexp_split_impl(
        &mut self,
        re: u32,
        input: Value,
        limit: Value,
    ) -> Result<Value, Thrown> {
        // OBSERVABLE @@split (22.2.6.14), generic over any Object `rx`:
        // SpeciesConstructor(rx, %RegExp%) builds a sticky (`y`) splitter, then a
        // loop calls RegExpExec (honouring a user `exec`) reading lastIndex/length/
        // captures via Get. Positions p/q/e are UTF-16 unit indices; the no-match
        // advance is spec AdvanceStringIndex (+2 over an astral pair in `u`/`v`).
        let rx = Value::heap(re);
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(input)?;
        let u16s: Vec<u16> = self.value_units(s_val);
        let size = u16s.len();
        // `s_val`/`a` live in Rust locals across construct/exec re-entries.
        let _gc = self.gc_lock_guard();
        // SpeciesConstructor(rx, %RegExp%).
        let default_ctor = Value::heap(self.regexp_ctor);
        let c = {
            let ctor = self.get_prop(rx, "constructor")?;
            if ctor == Value::UNDEFINED {
                default_ctor
            } else if !self.is_object_value(ctor) {
                // SpeciesConstructor step 5: a defined-but-non-object constructor
                // (false / "string" / 86 / null) is a TypeError, before @@species.
                return Err(Thrown(
                    "TypeError: Symbol.split constructor property is not an object".into(),
                ));
            } else {
                let sp = self.get_prop(ctor, "@@species")?;
                if sp == Value::UNDEFINED || sp == Value::NULL {
                    default_ctor
                } else if self.is_constructor(sp) {
                    sp
                } else {
                    return Err(Thrown(
                        "TypeError: Symbol.split species constructor is not a constructor".into(),
                    ));
                }
            }
        };
        // flags (observable) + force the sticky `y` flag on the splitter copy.
        let flags_v = self.get_prop(rx, "flags")?;
        let flags = self.to_js_string(flags_v)?;
        // unicodeMatching (`u`/`v`) selects code-point AdvanceStringIndex.
        let unicode_matching = flags.contains('u') || flags.contains('v');
        let new_flags = if flags.contains('y') { flags } else { format!("{flags}y") };
        let new_flags_v = self.alloc_str(new_flags);
        let splitter = self.construct(c, &[rx, new_flags_v])?;
        let lim: u64 = if limit == Value::UNDEFINED {
            u32::MAX as u64
        } else {
            to_uint32(self.to_number_coerce(limit)?) as u64
        };
        let mut a: Vec<Value> = Vec::new();
        if lim == 0 {
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
        }
        // Empty input: one exec; if it matches, the result is empty, else [S].
        if size == 0 {
            let z = self.regexp_exec_abstract(splitter.heap_index(), s_val)?;
            if z == Value::NULL {
                a.push(s_val);
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
        }
        let mut p: usize = 0;
        let mut q: usize = 0;
        let mut guard = 0u32;
        while q < size {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            self.set_prop(splitter, "lastIndex", Value::num(q as f64), true)?;
            let z = self.regexp_exec_abstract(splitter.heap_index(), s_val)?;
            if z == Value::NULL {
                q = advance_string_index(&u16s, q, unicode_matching);
                continue;
            }
            // e = min(ToLength(Get(splitter,"lastIndex")), size).
            let li_v = self.get_prop(splitter, "lastIndex")?;
            let e = (self.to_integer_or_zero(li_v)?.max(0) as usize).min(size);
            if e == p {
                q = advance_string_index(&u16s, q, unicode_matching);
                continue;
            }
            let t = self.units_value(&u16s[p..q]);
            a.push(t);
            if a.len() as u64 == lim {
                return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
            }
            p = e;
            // Each capturing group (1..n) is emitted between the pieces.
            let zlen_v = self.get_prop(z, "length")?;
            let n_captures = (self.to_integer_or_zero(zlen_v)?.max(0) as usize).saturating_sub(1);
            for i in 1..=n_captures {
                let cap = self.get_prop(z, &i.to_string())?;
                a.push(cap);
                if a.len() as u64 == lim {
                    return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
                }
            }
            q = p;
        }
        let tail = self.units_value(&u16s[p..]);
        a.push(tail);
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))))
    }

    pub(crate) fn regexp_get_prop(
        &mut self,
        idx: u32,
        source: &str,
        flags: &str,
        last_index: Value,
        key: &str,
        eff_proto: u32,
    ) -> Result<Value, Thrown> {
        Ok(match key {
            "lastIndex" => last_index,
            // Exact-WTF-8 when the side table has the pattern's exact bytes
            // (lone surrogates round-trip), else the lossy escaped source.
            "source" => self.regexp_source_value(idx, source),
            "flags" => {
                let s = canonical_flags(flags);
                self.alloc_str(s)
            }
            "global" => Value::bool(flags.contains('g')),
            "ignoreCase" => Value::bool(flags.contains('i')),
            "multiline" => Value::bool(flags.contains('m')),
            "dotAll" => Value::bool(flags.contains('s')),
            "unicode" => Value::bool(flags.contains('u')),
            "unicodeSets" => Value::bool(flags.contains('v')),
            "sticky" => Value::bool(flags.contains('y')),
            "hasIndices" => Value::bool(flags.contains('d')),
            // A subclass instance resolves through ITS prototype (proto_of)
            // so class [Symbol.replace]/exec overrides shadow the builtin.
            _ => self.proto_member(eff_proto, key),
        })
    }

    /// `RegExp.prototype.exec(input)`: returns the match-result Array (group 0 +
    /// captures, with `.index`/`.input`/`.groups` in the side table) or `null`.
    /// Advances `lastIndex` for a global/sticky regex.
    ///
    /// Matching runs over the subject's UTF-16 CODE UNITS (regress
    /// `find_from_utf16` for `u`/`v` regexes — code-point elements — and
    /// `find_from_ucs2` otherwise — each unit is an element, so `/./` matches
    /// one surrogate half). Every position regress reports (match range,
    /// capture ranges) is a unit index, identical to JS string indexing
    /// engine-wide: `lastIndex` seeds the search directly and `.index` /
    /// `indices` / `lastIndex` writes take the ranges verbatim.
    pub(crate) fn regexp_exec(&mut self, re_idx: u32, input_v: Value) -> Result<Value, Thrown> {
        // ToString(string) — IDENTITY for a string value (exact WTF-8 content:
        // a lone-surrogate subject keeps its surrogate rather than decaying to
        // U+FFFD, so `/\uD800/` can match it).
        let input_val = self.to_str_value(input_v)?;
        // `input_val` + the result pieces below live in Rust locals across a
        // possible `lastIndex.valueOf` re-entry — hold GC off until we return.
        let _gc = self.gc_lock_guard();
        let (global, sticky, has_indices, unicode) = match self.heap.get(re_idx) {
            HeapObj::RegExp { flags, .. } => (
                flags.contains('g'),
                flags.contains('y'),
                flags.contains('d'),
                flags.contains('u') || flags.contains('v'),
            ),
            _ => {
                return Err(Thrown(
                    "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                ))
            }
        };
        // ToLength(Get(R,"lastIndex")) — invokes a user `lastIndex.valueOf` (a throw
        // propagates); read UNCONDITIONALLY per RegExpBuiltinExec, but used as the
        // search start only for a global/sticky regex (otherwise the start is 0).
        let li_v = self.get_prop(Value::heap(re_idx), "lastIndex")?;
        let li = self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1) as usize;
        let stateful = global || sticky;
        let start = if stateful { li } else { 0 };
        // Encode the subject ONCE per exec; `lastIndex` is already a unit
        // index engine-wide, so it is the search start with no conversion.
        let u16s: Vec<u16> = self.value_units(input_val);
        let found = if start > u16s.len() {
            None
        } else {
            match self.heap.get(re_idx) {
                HeapObj::RegExp { regex, .. } => {
                    if unicode {
                        regex.find_from_utf16(&u16s, start).next()
                    } else {
                        regex.find_from_ucs2(&u16s, start).next()
                    }
                }
                _ => None,
            }
        };
        // Sticky: the match must begin exactly at the search start.
        let found = found.filter(|m| !(sticky && m.start() != start));
        let m = match found {
            Some(m) => m,
            None => {
                if stateful {
                    // RegExpBuiltinExec Set(R,"lastIndex",0,true): a non-writable
                    // lastIndex makes a failed global/sticky exec throw.
                    self.set_prop(Value::heap(re_idx), "lastIndex", Value::int(0), true)?;
                }
                return Ok(Value::NULL);
            }
        };
        let (mstart, mend) = (m.start(), m.end());
        let whole = self.units_value(&u16s[mstart..mend]);
        let mut elems = vec![whole];
        for cap in &m.captures {
            let v = match cap {
                Some(r) => self.units_value(&u16s[r.clone()]),
                None => Value::UNDEFINED,
            };
            elems.push(v);
        }
        let named: Vec<(String, Option<std::ops::Range<usize>>)> =
            m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
        let groups = if named.is_empty() {
            Value::UNDEFINED
        } else {
            let mut gm = ObjMap::new();
            for (name, r) in &named {
                let v = match r {
                    Some(r) => self.units_value(&u16s[r.clone()]),
                    None => Value::UNDEFINED,
                };
                gm.set(name, v);
            }
            let gidx = self.heap.alloc(HeapObj::Object(gm));
            // The groups object is OrdinaryObjectCreate(null) — no prototype.
            self.proto_of.insert(gidx, Value::NULL);
            Value::heap(gidx)
        };
        let arr_idx = self.heap.alloc(HeapObj::Array(elems));
        let index_v = Value::num(mstart as f64);
        let input_sv = input_val;
        // index/input/groups are real own data properties of the result array
        // (writable, enumerable, configurable) so reflection sees them.
        let attr = PropAttr {
            writable: true,
            enumerable: true,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        // `/d` (hasIndices): an `indices` array of [start,end] unit ranges for
        // the whole match + each capture group, with `.groups` for named groups.
        let indices_v = if has_indices {
            let mk = |vm: &mut Self, r: &std::ops::Range<usize>| -> Value {
                let s = Value::num(r.start as f64);
                let e = Value::num(r.end as f64);
                Value::heap(vm.heap.alloc(HeapObj::Array(vec![s, e])))
            };
            let mut idx_elems = vec![mk(self, &(mstart..mend))];
            for cap in &m.captures {
                idx_elems.push(match cap {
                    Some(r) => mk(self, r),
                    None => Value::UNDEFINED,
                });
            }
            let idx_groups = if named.is_empty() {
                Value::UNDEFINED
            } else {
                let mut gm = ObjMap::new();
                for (name, r) in &named {
                    let v = match r {
                        Some(r) => mk(self, r),
                        None => Value::UNDEFINED,
                    };
                    gm.set(name, v);
                }
                let gidx = self.heap.alloc(HeapObj::Object(gm));
                self.proto_of.insert(gidx, Value::NULL);
                Value::heap(gidx)
            };
            let indices_arr = self.heap.alloc(HeapObj::Array(idx_elems));
            self.arr_props.entry(indices_arr).or_insert_with(ObjMap::new).define(
                "groups",
                idx_groups,
                attr,
            );
            Value::heap(indices_arr)
        } else {
            Value::UNDEFINED
        };
        let m = self.arr_props.entry(arr_idx).or_insert_with(ObjMap::new);
        m.define("index", index_v, attr);
        m.define("input", input_sv, attr);
        m.define("groups", groups, attr);
        if has_indices {
            m.define("indices", indices_v, attr);
        }
        if stateful {
            // RegExpBuiltinExec Set(R,"lastIndex",e,true): throws if non-writable.
            self.set_prop(Value::heap(re_idx), "lastIndex", Value::num(mend as f64), true)?;
        }
        Ok(Value::heap(arr_idx))
    }

    /// The string's UTF-16 code units — EXACT: an astral scalar yields its two
    /// halves and a lone surrogate its own 0xD800–0xDFFF value (which is what
    /// lets a `\uD800` pattern match a real lone-surrogate subject). `v` must
    /// be a string value (callers come through `to_str_value`).
    pub(crate) fn value_units(&mut self, v: Value) -> Vec<u16> {
        if !v.is_heap() {
            return Vec::new();
        }
        let idx = v.heap_index();
        self.heap.flatten(idx);
        match self.heap.get(idx) {
            HeapObj::Str(js) if js.is_ascii() => js.as_bytes().iter().map(|&b| b as u16).collect(),
            HeapObj::Str(js) => js.units_iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Allocate the string for a unit-slice of a subject — built as WTF-8 so
    /// lone surrogates round-trip exactly and a covered (high, low) pair
    /// recombines into its astral scalar (canonical form).
    pub(crate) fn units_value(&mut self, units: &[u16]) -> Value {
        let mut out: Vec<u8> = Vec::with_capacity(units.len() * 3);
        push_units(&mut out, units);
        Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out)))
    }

    /// AdvanceStringIndex reading the units from heap string `s` (for the lazy
    /// matchAll driver, which doesn't keep an encoded unit buffer around).
    pub(crate) fn advance_index_on_value(&mut self, s: Value, index: usize, unicode: bool) -> usize {
        if unicode && s.is_heap() {
            self.heap.flatten(s.heap_index());
            if let HeapObj::Str(js) = self.heap.get(s.heap_index()) {
                if let (Some(hi), Some(lo)) = (js.unit_at(index), js.unit_at(index + 1)) {
                    if (0xD800..=0xDBFF).contains(&hi) && (0xDC00..=0xDFFF).contains(&lo) {
                        return index + 2;
                    }
                }
            }
        }
        index + 1
    }

    /// Regex-backed `String.prototype.replace`/`replaceAll`. `repl` is a function
    /// (called `(match, ...groups, offset, input)`) or a template string (`$&`/`$N`/…).
    /// `s_idx` is the receiver string's heap index. All positions are UTF-16
    /// unit indices; the output is assembled as WTF-8 so the subject's lone
    /// surrogates (and a functional replacer's) round-trip exactly.
    pub(crate) fn regex_replace(
        &mut self,
        s_idx: u32,
        re: u32,
        repl: Value,
        global: bool,
    ) -> Result<Value, Thrown> {
        // Encode the subject ONCE; every regress range below indexes into it.
        let u16s: Vec<u16> = self.value_units(Value::heap(s_idx));
        let matches: Vec<regress::Match> = match self.heap.get(re) {
            HeapObj::RegExp { regex, flags, .. } => {
                let unicode = flags.contains('u') || flags.contains('v');
                match (unicode, global) {
                    (true, true) => regex.find_from_utf16(&u16s, 0).collect(),
                    (true, false) => regex.find_from_utf16(&u16s, 0).next().into_iter().collect(),
                    (false, true) => regex.find_from_ucs2(&u16s, 0).collect(),
                    (false, false) => regex.find_from_ucs2(&u16s, 0).next().into_iter().collect(),
                }
            }
            _ => Vec::new(),
        };
        let callable = repl.is_heap() && self.heap.as_callable(repl.heap_index()).is_some();
        let repl_str = if callable { String::new() } else { self.to_js_string(repl)? };
        let mut out: Vec<u8> = Vec::new();
        let mut last = 0usize;
        for m in &matches {
            let (st, en) = (m.start(), m.end());
            if st < last {
                continue;
            }
            push_units(&mut out, &u16s[last..st]);
            if callable {
                let whole = self.units_value(&u16s[m.range()]);
                let mut argv = vec![whole];
                for cap in &m.captures {
                    argv.push(match cap {
                        Some(r) => self.units_value(&u16s[r.clone()]),
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(st as f64));
                argv.push(Value::heap(s_idx));
                // RegExp.prototype[@@replace] step 14.k.iv: when the regex has named
                // capture groups, a `groups` object (OrdinaryObjectCreate(null)) is
                // the FINAL replacer argument. (Mirrors the exec/array path above.)
                let named: Vec<(String, Option<std::ops::Range<usize>>)> =
                    m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
                if !named.is_empty() {
                    let mut gm = ObjMap::new();
                    for (name, r) in &named {
                        let v = match r {
                            Some(r) => self.units_value(&u16s[r.clone()]),
                            None => Value::UNDEFINED,
                        };
                        gm.set(name, v);
                    }
                    let gidx = self.heap.alloc(HeapObj::Object(gm));
                    self.proto_of.insert(gidx, Value::NULL);
                    argv.push(Value::heap(gidx));
                }
                let r = self.call_value(repl, Value::UNDEFINED, &argv)?;
                // ToString(result) — exact bytes (a returned lone-surrogate
                // string keeps its surrogate; `wtf8_push` canonicalizes the seam).
                let rv = self.to_str_value(r)?;
                let bytes = self
                    .heap
                    .str_wtf8_cow(rv.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                crate::heap::wtf8_push(&mut out, &bytes);
            } else {
                // GetSubstitution over LOSSY views (the template + captures come
                // through ToString); positions stay unit-exact either way.
                let whole = String::from_utf16_lossy(&u16s[m.range()]);
                let groups: Vec<Option<String>> = m
                    .captures
                    .iter()
                    .map(|c| c.as_ref().map(|r| String::from_utf16_lossy(&u16s[r.clone()])))
                    .collect();
                let named: Vec<(String, Option<String>)> = m
                    .named_groups()
                    .map(|(n, r)| (n.to_string(), r.map(|r| String::from_utf16_lossy(&u16s[r]))))
                    .collect();
                let rep = self.expand_replacement(
                    &repl_str,
                    &whole,
                    &groups,
                    &named,
                    !named.is_empty(),
                    &String::from_utf16_lossy(&u16s[..st]),
                    &String::from_utf16_lossy(&u16s[en..]),
                );
                crate::heap::wtf8_push(&mut out, rep.as_bytes());
            }
            last = en;
        }
        push_units(&mut out, &u16s[last..]);
        Ok(Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out))))
    }

    // ── TypedArrays / ArrayBuffer / DataView ──

}

/// The pattern characters fed to the regress parser for a NON-`u`/`v` regex,
/// from the pattern's UTF-16 units. The spec grammar reads such a pattern per
/// CODE UNIT — an astral literal is its two surrogate halves, each its own
/// pattern character (so it matches over UCS-2 subject units) — EXCEPT inside
/// RegExpIdentifierName (a group name `(?<name>…)` / `\k<name>`), where
/// surrogate pairs recombine into code points. Tracks character-class nesting
/// so a literal `(?<` / `\k<` inside `[...]` stays plain units.
pub(crate) fn nonunicode_pattern_chars(units: &[u16]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(units.len());
    let mut i = 0usize;
    let mut in_class = false;
    let mut in_name = false;
    while i < units.len() {
        let u = units[i] as u32;
        if in_name {
            if u == '>' as u32 {
                in_name = false;
                out.push(u);
                i += 1;
            } else if (0xD800..=0xDBFF).contains(&units[i])
                && i + 1 < units.len()
                && (0xDC00..=0xDFFF).contains(&units[i + 1])
            {
                let (hi, lo) = (units[i] as u32, units[i + 1] as u32);
                out.push(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                i += 2;
            } else {
                out.push(u);
                i += 1;
            }
            continue;
        }
        match u {
            // An escape: copy `\` + the next unit verbatim (so `\[`/`\]` can't
            // flip the class state), EXCEPT `\k<` outside a class, which opens
            // a group-name reference.
            0x5C => {
                out.push(u);
                if i + 1 < units.len() {
                    let n = units[i + 1] as u32;
                    out.push(n);
                    if !in_class
                        && n == 'k' as u32
                        && i + 2 < units.len()
                        && units[i + 2] == '<' as u16
                    {
                        out.push('<' as u32);
                        in_name = true;
                        i += 3;
                        continue;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            0x5B if !in_class => {
                in_class = true;
                out.push(u);
                i += 1;
            }
            0x5D if in_class => {
                in_class = false;
                out.push(u);
                i += 1;
            }
            // `(?<` not followed by `=`/`!` (lookbehinds) opens a group name.
            0x28 if !in_class
                && i + 2 < units.len()
                && units[i + 1] == '?' as u16
                && units[i + 2] == '<' as u16
                && units.get(i + 3).map_or(true, |&n| n != '=' as u16 && n != '!' as u16) =>
            {
                out.extend_from_slice(&['(' as u32, '?' as u32, '<' as u32]);
                in_name = true;
                i += 3;
            }
            _ => {
                out.push(u);
                i += 1;
            }
        }
    }
    out
}

/// Append `units` onto WTF-8 buffer `out` — exact (`wtf8_push_cp`
/// canonicalizes an adjacent (high, low) pair back into its astral scalar).
pub(crate) fn push_units(out: &mut Vec<u8>, units: &[u16]) {
    for &u in units {
        crate::heap::wtf8_push_cp(out, u as u32);
    }
}

/// AdvanceStringIndex (ES 22.2.7.3): +1 code UNIT, or +2 when `unicode`
/// (the `u`/`v` flags) and `index` sits on a high surrogate directly followed
/// by a low surrogate (one astral code point).
pub(crate) fn advance_string_index(units: &[u16], index: usize, unicode: bool) -> usize {
    if unicode
        && index + 1 < units.len()
        && (0xD800..=0xDBFF).contains(&units[index])
        && (0xDC00..=0xDFFF).contains(&units[index + 1])
    {
        index + 2
    } else {
        index + 1
    }
}

/// Assemble a RegExp `flags` string in the canonical order `dgimsuvy`,
/// regardless of the order the flags were supplied in.
pub(crate) fn canonical_flags(flags: &str) -> String {
    let mut out = String::new();
    for ch in ['d', 'g', 'i', 'm', 's', 'u', 'v', 'y'] {
        if flags.contains(ch) {
            out.push(ch);
        }
    }
    out
}
