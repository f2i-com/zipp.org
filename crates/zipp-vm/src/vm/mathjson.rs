#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Evaluate a `Math.<fn>` call over `argc` argument registers (coerced to
    /// numbers). Mirrors JS semantics where they differ from Rust's f64 methods:
    /// `round` is half-up (so −2.5 → −2, not −3); `sign` preserves ±0 and maps
    /// NaN→NaN; `min`/`max` are NaN-sticky (any NaN arg ⇒ NaN).
    pub(crate) fn eval_math(&self, op: crate::bytecode::MathFn, base: usize, arg_base: u16, argc: u16) -> Result<f64, Thrown> {
        use crate::bytecode::MathFn as M;
        let arg = |i: u16| -> Result<f64, Thrown> {
            if i < argc {
                self.to_number(self.get(base, arg_base + i))
            } else {
                Ok(f64::NAN)
            }
        };
        Ok(match op {
            M::Min | M::Max | M::Hypot => {
                let mut acc = match op {
                    M::Min => f64::INFINITY,
                    M::Max => f64::NEG_INFINITY,
                    _ => 0.0, // Hypot: sum of squares
                };
                for i in 0..argc {
                    let v = arg(i)?;
                    acc = match op {
                        M::Min => {
                            if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.min(v) }
                        }
                        M::Max => {
                            if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.max(v) }
                        }
                        _ => acc + v * v,
                    };
                }
                if matches!(op, M::Hypot) { acc.sqrt() } else { acc }
            }
            M::Pow => arg(0)?.powf(arg(1)?),
            M::Atan2 => arg(0)?.atan2(arg(1)?),
            // Math.imul(a,b): ToUint32 multiply, result as signed int32.
            M::Imul => (to_uint32(arg(0)?).wrapping_mul(to_uint32(arg(1)?)) as i32) as f64,
            _ => math_unary(op, arg(0)?),
        })
    }

    /// `Math.<op>` reduced to a single f64 result (used by the `MathSpread`
    /// fallback for an unusual non-variadic spread like `Math.abs(...arr)`).
    pub(crate) fn eval_math_one(&self, op: crate::bytecode::MathFn, x: f64) -> f64 {
        math_unary(op, x)
    }

    /// Evaluate a Math method over an argument SLICE (the value-form `Math.abs`
    /// invoked as a native), mirroring `eval_math`'s register-based variant.
    pub(crate) fn eval_math_args(&self, op: crate::bytecode::MathFn, args: &[Value]) -> Result<f64, Thrown> {
        use crate::bytecode::MathFn as M;
        let arg = |i: usize| -> Result<f64, Thrown> {
            match args.get(i) {
                Some(v) => self.to_number(*v),
                None => Ok(f64::NAN),
            }
        };
        Ok(match op {
            M::Min | M::Max | M::Hypot => {
                let mut acc = match op {
                    M::Min => f64::INFINITY,
                    M::Max => f64::NEG_INFINITY,
                    _ => 0.0,
                };
                for i in 0..args.len() {
                    let v = arg(i)?;
                    acc = match op {
                        M::Min => if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.min(v) },
                        M::Max => if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.max(v) },
                        _ => acc + v * v,
                    };
                }
                if matches!(op, M::Hypot) { acc.sqrt() } else { acc }
            }
            M::Pow => arg(0)?.powf(arg(1)?),
            M::Atan2 => arg(0)?.atan2(arg(1)?),
            M::Imul => (to_uint32(arg(0)?).wrapping_mul(to_uint32(arg(1)?)) as i32) as f64,
            _ => math_unary(op, arg(0)?),
        })
    }

    /// The per-level indent string for `JSON.stringify`'s `space` argument: a
    /// number → that many spaces (clamped 0..10); a string → its first 10 chars;
    /// anything else → empty (compact output).
    pub(crate) fn json_indent(&self, space: Value) -> String {
        if space.is_number() {
            let n = space.as_f64();
            let n = if n.is_finite() && n > 0.0 { (n as usize).min(10) } else { 0 };
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
        if replacer.is_heap() {
            let idx = replacer.heap_index();
            if let HeapObj::Array(items) = self.heap.get(idx) {
                let items = items.clone();
                let mut list: Vec<String> = Vec::new();
                for it in items {
                    let key = if it.is_number() {
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
                    if let Some(k) = key {
                        if !list.contains(&k) {
                            list.push(k);
                        }
                    }
                }
                return Ok((Value::UNDEFINED, Some(list)));
            }
        }
        Ok((Value::UNDEFINED, None))
    }

    /// Serialize `v` to JSON (`None` ⇒ omit: undefined / function). `indent` is
    /// the per-level pad (empty ⇒ compact); `depth` is the current nesting.
    /// `holder` is the object/array `key` lives on (the `this` for a function
    /// `replacer`); `replacer` is a callable or undefined; `allowlist`, when
    /// `Some`, restricts which object keys are emitted (the array-replacer form).
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
        // SerializeJSONProperty: a value with a callable `toJSON` is replaced by
        // `value.toJSON(key)` before serialization (Date, user objects, …).
        let v = if v.is_heap() {
            let tj = self.get_prop(v, "toJSON")?;
            if self.is_callable(tj) {
                let kv = self.alloc_str(key.to_string());
                self.call_value(tj, v, &[kv])?
            } else {
                v
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
            return Ok(None);
        }
        if v.is_null() {
            return Ok(Some("null".to_string()));
        }
        if v.is_bool() {
            return Ok(Some(if v.as_bool() { "true" } else { "false" }.to_string()));
        }
        if v.is_number() {
            let n = v.as_f64();
            return Ok(Some(if n.is_finite() { fmt_f64(n) } else { "null".to_string() }));
        }
        if !v.is_heap() {
            return Ok(None);
        }
        let idx = v.heap_index();
        // Extract under a short borrow; recursion needs &mut self, so no heap
        // borrow may be held across it.
        enum Node {
            Str(String),
            Omit,
            BigInt,
            Boxed(Value),
            Arr(Vec<Value>),
            Obj(Vec<(String, Value)>),
            EmptyObj,
            /// A `JSON.rawJSON` object: emit its stored text verbatim.
            Raw(String),
        }
        let node = match self.heap.get(idx) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                Node::Str(self.heap.str_cow(idx).unwrap().into_owned())
            }
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::Native(_)
            | HeapObj::Symbol { .. } => Node::Omit,
            HeapObj::BigInt(_) => Node::BigInt,
            HeapObj::Boxed { value, .. } => Node::Boxed(*value),
            HeapObj::Array(items) => Node::Arr(items.clone()),
            HeapObj::Object(map) if map.is_raw_json => {
                // [[IsRawJSON]]: emit the stored "rawJSON" text verbatim.
                let raw_val = map.get("rawJSON").unwrap_or(Value::UNDEFINED);
                let s = self
                    .heap
                    .str_cow(raw_val.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                Node::Raw(s)
            }
            HeapObj::Object(map) => {
                let mut pairs = Vec::new();
                if let Some(allow) = allowlist {
                    // Array-replacer (PropertyList): emit keys in the allowlist's
                    // order, only those the object owns enumerably.
                    for k in allow {
                        if let Some(pos) = map.keys.iter().position(|kk| kk == k) {
                            if map.attrs[pos].enumerable && !is_hidden_key(&map.keys[pos]) {
                                pairs.push((k.clone(), map.vals[pos]));
                            }
                        }
                    }
                } else {
                    let order = spec_key_order(&map.keys);
                    for i in order {
                        if map.attrs[i].enumerable && !is_hidden_key(&map.keys[i]) {
                            pairs.push((map.keys[i].clone(), map.vals[i]));
                        }
                    }
                }
                Node::Obj(pairs)
            }
            HeapObj::Map { .. } | HeapObj::Set(_) | HeapObj::Generator { .. } => Node::EmptyObj,
            _ => Node::Omit,
        };
        match node {
            Node::Omit => Ok(None),
            Node::Raw(s) => Ok(Some(s)),
            Node::Str(s) => Ok(Some(json_quote(&s))),
            Node::BigInt => Err(Thrown("TypeError: Do not know how to serialize a BigInt".into())),
            // A boxed Number/String/Boolean serializes as its wrapped primitive.
            // The replacer has already fired on the wrapper, so don't re-apply it.
            Node::Boxed(inner) => {
                self.json_value(holder, key, inner, indent, depth, visited, Value::UNDEFINED, allowlist)
            }
            Node::EmptyObj => Ok(Some("{}".to_string())),
            Node::Arr(items) => {
                if visited.contains(&idx) {
                    return Err(Thrown("TypeError: Converting circular structure to JSON".into()));
                }
                visited.push(idx);
                let mut parts = Vec::with_capacity(items.len());
                for (i, e) in items.iter().enumerate() {
                    let ks = i.to_string();
                    // Array elements ignore the PropertyList allowlist (which only
                    // filters object keys), but a function replacer still applies.
                    let part = self
                        .json_value(v, &ks, *e, indent, depth + 1, visited, replacer, None)?
                        .unwrap_or_else(|| "null".to_string());
                    parts.push(part);
                }
                visited.pop();
                Ok(Some(if parts.is_empty() {
                    "[]".to_string()
                } else {
                    wrap_json(&parts, '[', ']', indent, depth)
                }))
            }
            Node::Obj(pairs) => {
                if visited.contains(&idx) {
                    return Err(Thrown("TypeError: Converting circular structure to JSON".into()));
                }
                visited.push(idx);
                let sep = if indent.is_empty() { ":" } else { ": " };
                let mut parts = Vec::new();
                for (k, val) in pairs {
                    if let Some(vs) =
                        self.json_value(v, &k, val, indent, depth + 1, visited, replacer, allowlist)?
                    {
                        parts.push(format!("{}{}{}", json_quote(&k), sep, vs));
                    }
                }
                visited.pop();
                Ok(Some(if parts.is_empty() {
                    "{}".to_string()
                } else {
                    wrap_json(&parts, '{', '}', indent, depth)
                }))
            }
        }
    }

    /// Parse a JSON string into a Value, or throw SyntaxError. Recursive-descent
    /// over the byte string (structure tokens are ASCII; string content is
    /// flushed as UTF-8 slices). Allocates heap objects/arrays/strings.
    pub(crate) fn json_parse(&mut self, src: &str) -> Result<Value, Thrown> {
        let mut i = 0;
        json_skip_ws(src.as_bytes(), &mut i);
        let v = self.json_parse_value(src, &mut i)?;
        json_skip_ws(src.as_bytes(), &mut i);
        if i != src.len() {
            return Err(Thrown("SyntaxError: Unexpected non-whitespace character after JSON".into()));
        }
        Ok(v)
    }

    pub(crate) fn json_parse_value(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
        match b.get(*i).copied() {
            Some(b'{') => self.json_parse_object(src, i),
            Some(b'[') => self.json_parse_array(src, i),
            Some(b'"') => {
                let s = json_parse_string(src, i)?;
                Ok(self.alloc_str(s))
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

    pub(crate) fn json_parse_array(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
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
                _ => return Err(Thrown("SyntaxError: Expected ',' or ']' in JSON array".into())),
            }
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(items))))
    }

    pub(crate) fn json_parse_object(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
        *i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b'}') {
            loop {
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b'"') {
                    return Err(Thrown("SyntaxError: Expected property name string in JSON".into()));
                }
                let key = json_parse_string(src, i)?;
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
                    _ => return Err(Thrown("SyntaxError: Expected ',' or '}' in JSON object".into())),
                }
            }
        }
        *i += 1; // '}'
        let mut map = crate::heap::ObjMap::new();
        for (k, v) in pairs {
            map.set(&k, v);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Object(map))))
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
        // Read `holder[key]`, routing a numeric key on an array to its element
        // (`get_prop` only handles named properties).
        let val = match self.array_element_index(holder, key) {
            Some(i) => match self.heap.get(holder.heap_index()) {
                HeapObj::Array(items) => items.get(i).copied().unwrap_or(Value::UNDEFINED),
                _ => Value::UNDEFINED,
            },
            None => self.get_prop(holder, key)?,
        };
        if val.is_heap() {
            // Snapshot the child shape under a short borrow, then recurse (no heap
            // borrow may be held across the reviver re-entry).
            enum Kind {
                Arr(usize),
                Obj(Vec<String>),
                Other,
            }
            let kind = match self.heap.get(val.heap_index()) {
                HeapObj::Array(items) => Kind::Arr(items.len()),
                HeapObj::Object(map) => {
                    let mut keys = Vec::new();
                    for i in 0..map.keys.len() {
                        if map.attrs[i].enumerable && !is_hidden_key(&map.keys[i]) {
                            keys.push(map.keys[i].clone());
                        }
                    }
                    Kind::Obj(keys)
                }
                _ => Kind::Other,
            };
            match kind {
                Kind::Arr(len) => {
                    for i in 0..len {
                        let k = i.to_string();
                        let child = match src {
                            Some(JsonSrc::Arr(v)) => v.get(i),
                            _ => None,
                        };
                        let nv = self.internalize_json(val, &k, reviver, child)?;
                        // Array elements: write/clear in place (a deleted element
                        // becomes a hole, which serialises back as `null`).
                        if let HeapObj::Array(items) = self.heap.get_mut(val.heap_index()) {
                            if i < items.len() {
                                items[i] = if nv.is_undefined() { Value::UNDEFINED } else { nv };
                            }
                        }
                    }
                }
                Kind::Obj(keys) => {
                    for k in keys {
                        let child = match src {
                            Some(JsonSrc::Obj(pairs)) => {
                                pairs.iter().find(|(pk, _)| pk == &k).map(|(_, s)| s)
                            }
                            _ => None,
                        };
                        let nv = self.internalize_json(val, &k, reviver, child)?;
                        if nv.is_undefined() {
                            self.delete_property(val, &k)?;
                        } else {
                            self.set_prop(val, &k, nv)?;
                        }
                    }
                }
                Kind::Other => {}
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
        let ctx = Value::heap(self.heap.alloc(HeapObj::Object(crate::heap::ObjMap::new())));
        if let Some(JsonSrc::Prim(s)) = src {
            let sv = self.alloc_str(s.clone());
            if let HeapObj::Object(m) = self.heap.get_mut(ctx.heap_index()) {
                m.set("source", sv);
            }
        }
        ctx
    }

    /// Like [`json_parse`], but also returns a parallel source tree recording the
    /// raw JSON text of every value (for the parse-with-source reviver context).
    pub(crate) fn json_parse_with_src(&mut self, src: &str) -> Result<(Value, JsonSrc), Thrown> {
        let mut i = 0;
        json_skip_ws(src.as_bytes(), &mut i);
        let r = self.json_parse_value_src(src, &mut i)?;
        json_skip_ws(src.as_bytes(), &mut i);
        if i != src.len() {
            return Err(Thrown("SyntaxError: Unexpected non-whitespace character after JSON".into()));
        }
        Ok(r)
    }

    fn json_parse_value_src(
        &mut self,
        src: &str,
        i: &mut usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        let b = src.as_bytes();
        match b.get(*i).copied() {
            Some(b'{') => self.json_parse_object_src(src, i),
            Some(b'[') => self.json_parse_array_src(src, i),
            _ => {
                // A primitive (string/number/true/false/null): record its exact span.
                let start = *i;
                let v = self.json_parse_value(src, i)?;
                Ok((v, JsonSrc::Prim(src[start..*i].to_string())))
            }
        }
    }

    fn json_parse_array_src(
        &mut self,
        src: &str,
        i: &mut usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        let b = src.as_bytes();
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
                    _ => return Err(Thrown("SyntaxError: Expected ',' or ']' in JSON array".into())),
                }
            }
        }
        *i += 1; // ']'
        let av = Value::heap(self.heap.alloc(HeapObj::Array(items)));
        Ok((av, JsonSrc::Arr(srcs)))
    }

    fn json_parse_object_src(
        &mut self,
        src: &str,
        i: &mut usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        let b = src.as_bytes();
        *i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        let mut srcs: Vec<(String, JsonSrc)> = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b'}') {
            loop {
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b'"') {
                    return Err(Thrown("SyntaxError: Expected property name string in JSON".into()));
                }
                let key = json_parse_string(src, i)?;
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(Thrown("SyntaxError: Expected ':' in JSON object".into()));
                }
                *i += 1;
                json_skip_ws(b, i);
                let (val, s) = self.json_parse_value_src(src, i)?;
                pairs.push((key.clone(), val));
                srcs.push((key, s));
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => break,
                    _ => return Err(Thrown("SyntaxError: Expected ',' or '}' in JSON object".into())),
                }
            }
        }
        *i += 1; // '}'
        let mut map = crate::heap::ObjMap::new();
        for (k, v) in pairs {
            map.set(&k, v);
        }
        let ov = Value::heap(self.heap.alloc(HeapObj::Object(map)));
        Ok((ov, JsonSrc::Obj(srcs)))
    }
}

/// A parallel tree to a parsed JSON value recording each node's raw source text,
/// for the ES2025 parse-with-source reviver `context.source`.
pub(crate) enum JsonSrc {
    /// A primitive leaf — the exact JSON text that produced it (e.g. `"1.1"`).
    Prim(String),
    Arr(Vec<JsonSrc>),
    Obj(Vec<(String, JsonSrc)>),
}
