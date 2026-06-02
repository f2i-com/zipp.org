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

    /// Serialize `v` to JSON (`None` ⇒ omit: undefined / function). `indent` is
    /// the per-level pad (empty ⇒ compact); `depth` is the current nesting.
    pub(crate) fn json_value(&self, v: Value, indent: &str, depth: usize) -> Option<String> {
        if depth > 512 {
            return None; // guard against pathological / circular structures
        }
        if v.is_undefined() {
            return None;
        }
        if v.is_null() {
            return Some("null".to_string());
        }
        if v.is_bool() {
            return Some(if v.as_bool() { "true" } else { "false" }.to_string());
        }
        if v.is_number() {
            let n = v.as_f64();
            return Some(if n.is_finite() { fmt_f64(n) } else { "null".to_string() });
        }
        if !v.is_heap() {
            return None;
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                let s = self.heap.str_cow(v.heap_index()).unwrap();
                Some(json_quote(&s))
            }
            HeapObj::Func(_) | HeapObj::Closure { .. } => None, // functions are omitted
            HeapObj::Symbol { .. } => None,                    // symbols are omitted by JSON
            HeapObj::Array(items) => {
                let items = items.clone(); // release the heap borrow before recursing
                if items.is_empty() {
                    return Some("[]".to_string());
                }
                // A missing element value serializes as null inside an array.
                let parts: Vec<String> = items
                    .iter()
                    .map(|e| self.json_value(*e, indent, depth + 1).unwrap_or_else(|| "null".to_string()))
                    .collect();
                Some(wrap_json(&parts, '[', ']', indent, depth))
            }
            HeapObj::Object(map) => {
                let keys = map.keys.clone();
                let vals = map.vals.clone();
                let order = spec_key_order(&keys);
                let sep = if indent.is_empty() { ":" } else { ": " };
                let mut parts = Vec::new();
                for &i in &order {
                    let k = &keys[i];
                    // Symbol-keyed (and private) properties are skipped by JSON.
                    if is_hidden_key(k) {
                        continue;
                    }
                    if let Some(vs) = self.json_value(vals[i], indent, depth + 1) {
                        parts.push(format!("{}{}{}", json_quote(k), sep, vs));
                    }
                }
                if parts.is_empty() {
                    return Some("{}".to_string());
                }
                Some(wrap_json(&parts, '{', '}', indent, depth))
            }
            // A Map/Set/Generator has no enumerable own properties, so
            // JSON.stringify renders it as an empty object (not omitted).
            HeapObj::Map { .. } | HeapObj::Set(_) | HeapObj::Generator { .. } => Some("{}".into()),
            _ => None,
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

}
