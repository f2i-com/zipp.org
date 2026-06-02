#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
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
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => Value::int(c as i32),
                    None => Value::num(f64::NAN),
                }));
            }
            "codePointAt" => {
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => Value::int(c as i32),
                    None => Value::UNDEFINED,
                }));
            }
            "charAt" => {
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(self.alloc_str(c.map(|c| c.to_string()).unwrap_or_default())));
            }
            "at" => {
                let len = self.heap_char_len(idx) as i64;
                let i = if arg0.is_number() { arg0.as_f64() as i64 } else { 0 };
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
                let needle = self.display(arg0);
                // Optional fromIndex (a char position) to start searching at.
                let from = if args.len() >= 2 && args[1].is_number() {
                    args[1].as_f64().max(0.0) as usize
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
                let needle = self.display(arg0);
                Ok(Some(Value::bool(s.contains(&needle))))
            }
            "toUpperCase" => Ok(Some(self.alloc_str(s.to_uppercase()))),
            "toLowerCase" => Ok(Some(self.alloc_str(s.to_lowercase()))),
            "slice" | "substring" => {
                let len = char_len(&s) as i32;
                let start = norm_index(if args.is_empty() { 0 } else { arg0.as_f64() as i32 }, len);
                let end = if args.len() < 2 { len } else { norm_index(args[1].as_f64() as i32, len) };
                let out: String = if start < end {
                    s.chars().skip(start as usize).take((end - start) as usize).collect()
                } else {
                    String::new()
                };
                Ok(Some(self.alloc_str(out)))
            }
            "repeat" => {
                let n = arg0.as_f64();
                if n < 0.0 || !n.is_finite() {
                    return Err(Thrown("RangeError: Invalid count value".into()));
                }
                // Bound the result (an unbounded build would hang / OOM) — a too-long
                // string is a RangeError per spec. (Empty string repeats to "" for any
                // count, so its length is always 0 — no bound needed.)
                if n * (s.len() as f64) > (1u64 << 28) as f64 {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                Ok(Some(self.alloc_str(s.repeat(n as usize))))
            }
            "search" => {
                let re = self.to_regexp_arg(arg0)?;
                let found = match self.heap.get(re) {
                    HeapObj::RegExp { regex, .. } => regex.find(&s),
                    _ => None,
                };
                Ok(Some(match found {
                    Some(m) => Value::num(byte_to_char(&s, m.start()) as f64),
                    None => Value::int(-1),
                }))
            }
            "match" => {
                let re = self.to_regexp_arg(arg0)?;
                let global =
                    matches!(self.heap.get(re), HeapObj::RegExp { flags, .. } if flags.contains('g'));
                if global {
                    let strs: Vec<String> = match self.heap.get(re) {
                        HeapObj::RegExp { regex, .. } => {
                            regex.find_iter(&s).map(|m| s[m.range()].to_string()).collect()
                        }
                        _ => Vec::new(),
                    };
                    self.set_regexp_last_index(re, 0);
                    if strs.is_empty() {
                        return Ok(Some(Value::NULL));
                    }
                    let elems: Vec<Value> = strs.into_iter().map(|m| self.alloc_str(m)).collect();
                    Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(elems)))))
                } else {
                    let r = self.regexp_exec(re, Value::heap(idx))?;
                    Ok(Some(r))
                }
            }
            "split" if self.as_regexp(arg0).is_some() => {
                let re = self.as_regexp(arg0).unwrap();
                let limit = match args.get(1) {
                    Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
                    _ => usize::MAX,
                };
                let spans: Vec<(usize, usize)> = match self.heap.get(re) {
                    HeapObj::RegExp { regex, .. } => {
                        regex.find_iter(&s).map(|m| (m.start(), m.end())).collect()
                    }
                    _ => Vec::new(),
                };
                let mut parts: Vec<Value> = Vec::new();
                let mut last = 0usize;
                for (st, en) in spans {
                    if parts.len() >= limit {
                        break;
                    }
                    if st < last || (st == en && st == last) {
                        continue; // skip overlapping / empty-at-cursor matches
                    }
                    parts.push(self.alloc_str(s[last..st].to_string()));
                    last = en;
                }
                if parts.len() < limit {
                    parts.push(self.alloc_str(s[last..].to_string()));
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(parts)))))
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
                let sep = self.display(arg0);
                let parts: Vec<Value> = if args.is_empty() {
                    vec![self.alloc_str(s.clone())]
                } else if sep.is_empty() {
                    s.chars().map(|c| self.alloc_str(c.to_string())).collect()
                } else {
                    s.split(&sep).map(|p| self.alloc_str(p.to_string())).collect()
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(parts)))))
            }
            "trim" => Ok(Some(self.alloc_str(s.trim().to_string()))),
            "trimStart" => Ok(Some(self.alloc_str(s.trim_start().to_string()))),
            "trimEnd" => Ok(Some(self.alloc_str(s.trim_end().to_string()))),
            "startsWith" => Ok(Some(Value::bool(s.starts_with(&self.display(arg0))))),
            "endsWith" => Ok(Some(Value::bool(s.ends_with(&self.display(arg0))))),
            "concat" => {
                let mut out = s.clone();
                for a in args {
                    out.push_str(&self.display(*a));
                }
                Ok(Some(self.alloc_str(out)))
            }
            "substr" => {
                // Legacy substr(start, length); negative start counts from the end.
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i64;
                let sr = if args.is_empty() { 0.0 } else { arg0.as_f64() };
                let mut start = if sr.is_nan() { 0 } else { sr as i64 };
                if start < 0 {
                    start = (len + start).max(0);
                }
                let start = start.min(len) as usize;
                let avail = chars.len() - start;
                let count = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    avail
                } else {
                    let c = args[1].as_f64();
                    if c.is_nan() || c < 0.0 { 0 } else { (c as usize).min(avail) }
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
                let t = arg0.as_f64();
                let target = if t.is_finite() && t > 0.0 { t as usize } else { 0 };
                if target as u64 > (1u64 << 28) {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                if cur >= target {
                    return Ok(Some(self.alloc_str(s.clone())));
                }
                let pad = if args.len() >= 2 { self.display(args[1]) } else { " ".to_string() };
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
                // String search: replaces only the FIRST occurrence (JS semantics).
                let search = self.display(arg0);
                let repl = if args.len() >= 2 { self.display(args[1]) } else { "undefined".to_string() };
                let out = match s.find(&search) {
                    Some(pos) => {
                        let mut r = String::with_capacity(s.len() + repl.len());
                        r.push_str(&s[..pos]);
                        r.push_str(&repl);
                        r.push_str(&s[pos + search.len()..]);
                        r
                    }
                    None => s.clone(),
                };
                Ok(Some(self.alloc_str(out)))
            }
            "replaceAll" => {
                let search = self.display(arg0);
                let repl = if args.len() >= 2 { self.display(args[1]) } else { "undefined".to_string() };
                Ok(Some(self.alloc_str(s.replace(&search, &repl))))
            }
            // toLocale* default to the locale-independent case mappings.
            "toLocaleUpperCase" => Ok(Some(self.alloc_str(s.to_uppercase()))),
            "toLocaleLowerCase" => Ok(Some(self.alloc_str(s.to_lowercase()))),
            "lastIndexOf" => {
                let needle = self.display(arg0);
                let sc: Vec<char> = s.chars().collect();
                let nc: Vec<char> = needle.chars().collect();
                let len = sc.len();
                let cap = if args.len() >= 2 && args[1].is_number() && !args[1].as_f64().is_nan() {
                    (args[1].as_f64().max(0.0) as usize).min(len)
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

}
