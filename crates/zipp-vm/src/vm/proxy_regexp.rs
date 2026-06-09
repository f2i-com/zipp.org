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
    /// user `exec` governs the matches. (zipp indexes strings by Unicode scalar, so
    /// AdvanceStringIndex advances one scalar.)
    pub(crate) fn regexp_symbol_replace(
        &mut self,
        rx: Value,
        string: Value,
        replace_value: Value,
    ) -> Result<Value, Thrown> {
        let s = self.to_js_string(string)?;
        let s_chars: Vec<char> = s.chars().collect();
        let length_s = s_chars.len();
        let s_val = self.alloc_str(s.clone());
        let functional = self.is_callable(replace_value);
        let replace_str = if functional { String::new() } else { self.to_js_string(replace_value)? };
        // flags / global / fullUnicode are observable (Get, ToString).
        let flags_v = self.get_prop(rx, "flags")?;
        let flags = self.to_js_string(flags_v)?;
        let global = flags.contains('g');
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
                let this_index = self.to_integer_or_zero(li_v)?.max(0) as usize;
                self.set_prop(rx, "lastIndex", Value::num((this_index + 1) as f64), true)?;
            }
        }
        // Build the accumulated result, reading each match's fields via Get.
        let mut accumulated = String::new();
        let mut next_pos: usize = 0;
        for result in results {
            let len_v = self.get_prop(result, "length")?;
            let n_captures = (self.to_integer_or_zero(len_v)?.max(0) as usize).saturating_sub(1);
            let matched_v = self.get_prop(result, "0")?;
            let matched = self.to_js_string(matched_v)?;
            let match_len = matched.chars().count();
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
            let replacement = if functional {
                let mut argv: Vec<Value> = Vec::with_capacity(n_captures + 4);
                argv.push(self.alloc_str(matched.clone()));
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
                self.to_js_string(r)?
            } else {
                // GetSubstitution: read the named-capture group object's own props.
                // Step l.i.1 — when `groups` is not undefined it is ToObject'd, so a
                // primitive (e.g. a string `groups`) is boxed and its properties
                // (`$<length>` etc.) become readable; ToObject(null) throws.
                let named_list: Vec<(String, Option<String>)> = if named_defined {
                    let obj = self.to_object(named_v)?;
                    let names_v = self.object_own_property_names(obj)?;
                    let key_vals = self.array_snapshot(names_v.heap_index());
                    let mut keys: Vec<String> = Vec::with_capacity(key_vals.len());
                    for k in key_vals {
                        keys.push(self.display(k));
                    }
                    let mut v = Vec::with_capacity(keys.len());
                    for k in keys {
                        let val = self.get_prop(obj, &k)?;
                        let sv = if val == Value::UNDEFINED { None } else { Some(self.to_js_string(val)?) };
                        v.push((k, sv));
                    }
                    v
                } else {
                    Vec::new()
                };
                let pre: String = s_chars[..position].iter().collect();
                let post_start = (position + match_len).min(length_s);
                let post: String = s_chars[post_start..].iter().collect();
                self.expand_replacement(&replace_str, &matched, &captures, &named_list, named_defined, &pre, &post)
            };
            if position >= next_pos {
                let prefix: String = s_chars[next_pos..position].iter().collect();
                accumulated.push_str(&prefix);
                accumulated.push_str(&replacement);
                next_pos = position + match_len;
            }
        }
        if next_pos < length_s {
            let tail: String = s_chars[next_pos..].iter().collect();
            accumulated.push_str(&tail);
        }
        Ok(self.alloc_str(accumulated))
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
            let s_str = self.to_js_string(input)?;
            let s = self.alloc_str(s_str);
            let r = self.call_value(exec, re_v, &[s])?;
            let is_object = r.is_heap()
                && !matches!(
                    self.heap.get(r.heap_index()),
                    HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_)
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
        let s_str = self.to_js_string(input)?;
        let s_val = self.alloc_str(s_str);
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
            let match_str = self.to_js_string(m0)?;
            let is_empty = match_str.is_empty();
            elems.push(self.alloc_str(match_str));
            if is_empty {
                let li_v = self.get_prop(rx, "lastIndex")?;
                let this_index = self.to_integer_or_zero(li_v)?.max(0) as usize;
                self.set_prop(rx, "lastIndex", Value::num((this_index + 1) as f64), true)?;
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
        // captures via Get. zipp indexes by Unicode scalar (AdvanceStringIndex = +1).
        let rx = Value::heap(re);
        let s_str = self.to_js_string(input)?;
        let s_chars: Vec<char> = s_str.chars().collect();
        let size = s_chars.len();
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
        let s_val = self.alloc_str(s_str.clone());
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
                q += 1;
                continue;
            }
            // e = min(ToLength(Get(splitter,"lastIndex")), size).
            let li_v = self.get_prop(splitter, "lastIndex")?;
            let e = (self.to_integer_or_zero(li_v)?.max(0) as usize).min(size);
            if e == p {
                q += 1;
                continue;
            }
            let t: String = s_chars[p..q].iter().collect();
            a.push(self.alloc_str(t));
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
        let tail: String = s_chars[p..].iter().collect();
        a.push(self.alloc_str(tail));
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))))
    }

    pub(crate) fn regexp_get_prop(
        &mut self,
        source: &str,
        flags: &str,
        last_index: Value,
        key: &str,
    ) -> Result<Value, Thrown> {
        Ok(match key {
            "lastIndex" => last_index,
            "source" => {
                let s = self.escaped_source(source);
                self.alloc_str(s)
            }
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
            _ => self.proto_member(self.regexp_proto, key),
        })
    }

    /// `RegExp.prototype.exec(input)`: returns the match-result Array (group 0 +
    /// captures, with `.index`/`.input`/`.groups` in the side table) or `null`.
    /// Advances `lastIndex` for a global/sticky regex.
    pub(crate) fn regexp_exec(&mut self, re_idx: u32, input_v: Value) -> Result<Value, Thrown> {
        let input = self.to_js_string(input_v)?;
        let (global, sticky, has_indices) = match self.heap.get(re_idx) {
            HeapObj::RegExp { flags, .. } => {
                (flags.contains('g'), flags.contains('y'), flags.contains('d'))
            }
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
        let byte_start = char_to_byte(&input, start);
        let found = if start > input.chars().count() {
            None
        } else {
            match self.heap.get(re_idx) {
                HeapObj::RegExp { regex, .. } => regex.find_from(&input, byte_start).next(),
                _ => None,
            }
        };
        // Sticky: the match must begin exactly at the search start.
        let found = found.filter(|m| !(sticky && m.start() != byte_start));
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
        let whole = self.alloc_str(input[m.range()].to_string());
        let mut elems = vec![whole];
        let caps = m.captures.clone();
        for cap in &caps {
            let v = match cap {
                Some(r) => self.alloc_str(input[r.clone()].to_string()),
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
                    Some(r) => self.alloc_str(input[r.clone()].to_string()),
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
        let index_v = Value::num(byte_to_char(&input, mstart) as f64);
        let input_sv = self.alloc_str(input.clone());
        // index/input/groups are real own data properties of the result array
        // (writable, enumerable, configurable) so reflection sees them.
        let attr = PropAttr {
            writable: true,
            enumerable: true,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        // `/d` (hasIndices): an `indices` array of [start,end] char ranges for the
        // whole match + each capture group, with `.groups` for named groups.
        let indices_v = if has_indices {
            let mk = |vm: &mut Self, r: &std::ops::Range<usize>| -> Value {
                let s = Value::num(byte_to_char(&input, r.start) as f64);
                let e = Value::num(byte_to_char(&input, r.end) as f64);
                Value::heap(vm.heap.alloc(HeapObj::Array(vec![s, e])))
            };
            let mut idx_elems = vec![mk(self, &(mstart..mend))];
            for cap in &caps {
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
            let e = byte_to_char(&input, mend) as f64;
            self.set_prop(Value::heap(re_idx), "lastIndex", Value::num(e), true)?;
        }
        Ok(Value::heap(arr_idx))
    }

    /// Regex-backed `String.prototype.replace`/`replaceAll`. `repl` is a function
    /// (called `(match, ...groups, offset, input)`) or a template string (`$&`/`$N`/…).
    pub(crate) fn regex_replace(&mut self, s: &str, re: u32, repl: Value, global: bool) -> Result<String, Thrown> {
        let matches: Vec<regress::Match> = match self.heap.get(re) {
            HeapObj::RegExp { regex, .. } => {
                if global {
                    regex.find_iter(s).collect()
                } else {
                    regex.find(s).into_iter().collect()
                }
            }
            _ => Vec::new(),
        };
        let callable = repl.is_heap() && self.heap.as_callable(repl.heap_index()).is_some();
        let repl_str = if callable { String::new() } else { self.to_js_string(repl)? };
        let mut out = String::new();
        let mut last = 0usize;
        for m in &matches {
            let (st, en) = (m.start(), m.end());
            if st < last {
                continue;
            }
            out.push_str(&s[last..st]);
            let whole = s[m.range()].to_string();
            if callable {
                let mut argv = vec![self.alloc_str(whole)];
                for cap in &m.captures {
                    argv.push(match cap {
                        Some(r) => self.alloc_str(s[r.clone()].to_string()),
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(byte_to_char(s, st) as f64));
                argv.push(self.alloc_str(s.to_string()));
                // RegExp.prototype[@@replace] step 14.k.iv: when the regex has named
                // capture groups, a `groups` object (OrdinaryObjectCreate(null)) is
                // the FINAL replacer argument. (Mirrors the exec/array path above.)
                let named: Vec<(String, Option<std::ops::Range<usize>>)> =
                    m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
                if !named.is_empty() {
                    let mut gm = ObjMap::new();
                    for (name, r) in &named {
                        let v = match r {
                            Some(r) => self.alloc_str(s[r.clone()].to_string()),
                            None => Value::UNDEFINED,
                        };
                        gm.set(name, v);
                    }
                    let gidx = self.heap.alloc(HeapObj::Object(gm));
                    self.proto_of.insert(gidx, Value::NULL);
                    argv.push(Value::heap(gidx));
                }
                let r = self.call_value(repl, Value::UNDEFINED, &argv)?;
                let rs = self.to_js_string(r)?;
                out.push_str(&rs);
            } else {
                let groups: Vec<Option<String>> =
                    m.captures.iter().map(|c| c.as_ref().map(|r| s[r.clone()].to_string())).collect();
                let named: Vec<(String, Option<String>)> = m
                    .named_groups()
                    .map(|(n, r)| (n.to_string(), r.map(|r| s[r].to_string())))
                    .collect();
                let rep = self.expand_replacement(
                    &repl_str,
                    &whole,
                    &groups,
                    &named,
                    !named.is_empty(),
                    &s[..st],
                    &s[en..],
                );
                out.push_str(&rep);
            }
            last = en;
        }
        out.push_str(&s[last..]);
        Ok(out)
    }

    // ── TypedArrays / ArrayBuffer / DataView ──

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
