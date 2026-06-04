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
            *last_index = n;
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
                        if let Some(end) = tmpl[i + 2..].find('>') {
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
            self.set_prop(rx, "lastIndex", zero)?;
        }
        let result = self.regexp_exec_abstract(rx.heap_index(), input)?;
        let cur = self.get_prop(rx, "lastIndex")?;
        if !self.same_value(cur, prev) {
            self.set_prop(rx, "lastIndex", prev)?;
        }
        if result == Value::NULL {
            return Ok(Value::int(-1));
        }
        self.get_prop(result, "index")
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
        let global =
            matches!(self.heap.get(re), HeapObj::RegExp { flags, .. } if flags.contains('g'));
        if !global {
            return self.regexp_exec_abstract(re, input);
        }
        let s = self.to_js_string(input)?;
        let strs: Vec<String> = match self.heap.get(re) {
            HeapObj::RegExp { regex, .. } => {
                regex.find_iter(&s).map(|m| s[m.range()].to_string()).collect()
            }
            _ => Vec::new(),
        };
        self.set_regexp_last_index(re, 0);
        if strs.is_empty() {
            return Ok(Value::NULL);
        }
        let elems: Vec<Value> = strs.into_iter().map(|m| self.alloc_str(m)).collect();
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
        let s = self.to_js_string(input)?;
        // ToUint32(limit); undefined -> 2^32-1. lim == 0 yields the empty array.
        let lim: usize = if limit == Value::UNDEFINED {
            u32::MAX as usize
        } else {
            to_uint32(self.to_number_coerce(limit)?) as usize
        };
        if lim == 0 {
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(Vec::new()))));
        }
        // Empty input: [] if the pattern matches the empty string, else [""].
        if s.is_empty() {
            let matches_empty = matches!(self.heap.get(re), HeapObj::RegExp { regex, .. } if regex.find(&s).is_some());
            let parts = if matches_empty { Vec::new() } else { vec![self.alloc_str(String::new())] };
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(parts))));
        }
        // Collect matches with the text of each capturing group (group 0 excluded;
        // `captures` holds groups 1..n, None for a non-participating group).
        let matches: Vec<(usize, usize, Vec<Option<String>>)> = match self.heap.get(re) {
            HeapObj::RegExp { regex, .. } => regex
                .find_iter(&s)
                .map(|m| {
                    let caps = m
                        .captures
                        .iter()
                        .map(|c| c.as_ref().map(|r| s[r.clone()].to_string()))
                        .collect();
                    (m.start(), m.end(), caps)
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut parts: Vec<Value> = Vec::new();
        let mut last = 0usize;
        'outer: for (st, en, caps) in matches {
            // The spec only processes matches before the end of the string; a match
            // AT the end (e.g. an empty match at position length) is not a split
            // point — the trailing piece S[last..] covers it.
            if st >= s.len() {
                break;
            }
            if st < last || (st == en && st == last) {
                continue; // skip overlapping / empty-at-cursor matches
            }
            parts.push(self.alloc_str(s[last..st].to_string()));
            if parts.len() >= lim {
                return Ok(Value::heap(self.heap.alloc(HeapObj::Array(parts))));
            }
            // Spec @@split emits each capturing group between the pieces.
            for c in &caps {
                match c {
                    Some(t) => parts.push(self.alloc_str(t.clone())),
                    None => parts.push(Value::UNDEFINED),
                }
                if parts.len() >= lim {
                    break 'outer;
                }
            }
            last = en;
        }
        if parts.len() < lim {
            parts.push(self.alloc_str(s[last..].to_string()));
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(parts))))
    }

    pub(crate) fn regexp_get_prop(
        &mut self,
        source: &str,
        flags: &str,
        last_index: usize,
        key: &str,
    ) -> Result<Value, Thrown> {
        Ok(match key {
            "lastIndex" => Value::num(last_index as f64),
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
        let (global, sticky, has_indices, start_char) = match self.heap.get(re_idx) {
            HeapObj::RegExp { flags, last_index, .. } => {
                (flags.contains('g'), flags.contains('y'), flags.contains('d'), *last_index)
            }
            _ => {
                return Err(Thrown(
                    "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                ))
            }
        };
        let stateful = global || sticky;
        let start = if stateful { start_char } else { 0 };
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
                    self.set_regexp_last_index(re_idx, 0);
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
            self.set_regexp_last_index(re_idx, byte_to_char(&input, mend));
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
                let rep =
                    self.expand_replacement(&repl_str, &whole, &groups, &named, &s[..st], &s[en..]);
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
