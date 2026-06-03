#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Clone an array's current elements out of the heap. Used before invoking
    /// callbacks so a heap reallocation during the call can't dangle a borrow.
    /// Read an array-like receiver's elements (`this.length` coerced via ToLength,
    /// then `this[0 .. length]`) into a Vec — backs the generic Array.prototype
    /// methods invoked via `.call(arrayLike, …)` on a non-array (object or string).
    pub(crate) fn array_like_read(&mut self, idx: u32) -> Vec<Value> {
        let this = Value::heap(idx);
        if let HeapObj::Array(items) = self.heap.get(idx) {
            return items.clone();
        }
        // ToLength(Get(this, "length")): the length value is ToNumber-coerced via
        // the full ToPrimitive path (`to_number_coerce`), so an array-like whose
        // `length` is an object with `toString`/`valueOf` (or a getter) is honoured
        // rather than read as 0 (the bare `&self` `to_number` can't run that code).
        let len = match self.get_prop(this, "length") {
            Ok(v) => self.to_number_coerce(v).unwrap_or(0.0),
            Err(_) => 0.0,
        };
        let len = if len.is_finite() && len > 0.0 {
            (len as usize).min(crate::vm::MAX_DENSE_ARRAY_LEN)
        } else {
            0
        };
        let mut out = Vec::with_capacity(len.min(4096));
        for i in 0..len {
            out.push(self.get_index(this, Value::int(i as i32)).unwrap_or(Value::UNDEFINED));
        }
        out
    }

    pub(crate) fn array_snapshot(&self, idx: u32) -> Vec<Value> {
        match self.heap.get(idx) {
            HeapObj::Array(items) => items.clone(),
            _ => Vec::new(),
        }
    }

    /// IsConcatSpreadable(O) (ES 23.1.3.1.1): a `Symbol.isConcatSpreadable`
    /// ("@@isConcatSpreadable") flag overrides — when present it is ToBoolean'd;
    /// otherwise the value is spreadable iff it is an Array. Non-objects are never
    /// spreadable. Used by `Array.prototype.concat`.
    pub(crate) fn is_concat_spreadable(&mut self, v: Value) -> Result<bool, Thrown> {
        if !v.is_heap() {
            return Ok(false);
        }
        let flag = self.get_prop(v, "@@isConcatSpreadable")?;
        if flag != Value::UNDEFINED {
            return Ok(self.truthy(flag));
        }
        Ok(matches!(self.heap.get(v.heap_index()), HeapObj::Array(_)))
    }

    /// Recursively flatten nested arrays up to `depth` levels (for `Array.flat`).
    /// Each nested array is cloned out before recursing (releases the heap borrow).
    pub(crate) fn flatten_array(&self, items: &[Value], depth: i32) -> Vec<Value> {
        let mut out = Vec::new();
        for v in items {
            let nested: Option<Vec<Value>> = if depth > 0 && v.is_heap() {
                match self.heap.get(v.heap_index()) {
                    HeapObj::Array(a) => Some(a.clone()),
                    _ => None,
                }
            } else {
                None
            };
            match nested {
                Some(a) => out.extend(self.flatten_array(&a, depth - 1)),
                None => out.push(*v),
            }
        }
        out
    }

    /// Strict equality between two raw values (no register indirection). Mirrors
    /// `strict_eq` but takes values directly, for builtin use.
    /// SameValueZero — Map/Set key & element equality. Like `===` but NaN equals
    /// NaN (so NaN is a usable key and all NaNs dedupe). +0/-0 are equal here too
    /// (matching `===`); the store side normalizes -0 → +0. Strings compare by
    /// value, objects by reference identity, and there is no type coercion.
    /// Whether `v` is a JS Object (Type(v) === Object): a heap value that is not a
    /// primitive string (`Str`/`Cons`). Used by Reflect, which throws on non-objects.
    pub(crate) fn is_object_value(&self, v: Value) -> bool {
        // Symbol and BigInt are heap-allocated but are primitives, not objects
        // (typeof is "symbol"/"bigint"), so they must not count here.
        v.is_heap()
            && !self.heap.is_str_like(v.heap_index())
            && !matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Symbol { .. } | HeapObj::BigInt(_)
            )
    }

    /// `ToObject(v)` — backs `Object(x)` / `new Object(x)`. Primitives box into
    /// the matching wrapper (string/number/boolean/symbol/bigint); null and
    /// undefined become a fresh ordinary object; an existing object (array,
    /// function, …) is returned unchanged. `Boxed.kind`: 0=String, 1=Number,
    /// 2=Boolean, 3=Symbol, 4=BigInt.
    pub(crate) fn to_object(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_number() {
            return Ok(Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 1, value: v })));
        }
        if v.is_bool() {
            return Ok(Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 2, value: v })));
        }
        if !v.is_heap() {
            // null / undefined → a fresh ordinary object.
            return Ok(Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new()))));
        }
        // A heap value: string/symbol/bigint primitives box; every real object
        // (Object/Array/Func/Map/Boxed/…) is already an object → unchanged.
        let kind = match self.heap.get(v.heap_index()) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => 0u8,
            HeapObj::Symbol { .. } => 3u8,
            HeapObj::BigInt(_) => 4u8,
            _ => return Ok(v),
        };
        Ok(Value::heap(self.heap.alloc(HeapObj::Boxed { kind, value: v })))
    }

    /// `ToString(v)` as a Rust String, honouring a user `toString`/`valueOf` on an
    /// object (ToPrimitive with the string hint). Primitives and engine strings use
    /// `display`; a plain object with only the built-in (native) toString also falls
    /// back to `display` (which already yields "[object Object]" / the array join).
    pub(crate) fn to_js_string(&mut self, v: Value) -> Result<String, Thrown> {
        if !v.is_heap() || self.heap.is_str_like(v.heap_index()) {
            return Ok(self.display(v));
        }
        // ToString of a Symbol is a TypeError (use `.toString()` / `String(sym)`
        // explicitly instead — but even `String(sym)` routes through the dedicated
        // path, not this coercion).
        if matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
            return Err(Thrown("TypeError: Cannot convert a Symbol value to a string".into()));
        }
        // ToString(object) is ToPrimitive(input, "string") then a string coercion;
        // honour a `@@toPrimitive` hook before falling back to toString/valueOf.
        if let Some(p) = self.symbol_to_primitive(v, "string")? {
            return self.to_js_string(p);
        }
        for name in ["toString", "valueOf"] {
            let f = self.get_prop(v, name)?;
            // `is_callable` (not `as_callable`) so native methods count — notably
            // `Function.prototype.toString`, which yields the real source text.
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                if !r.is_heap() || self.heap.is_str_like(r.heap_index()) {
                    return Ok(self.display(r));
                }
            }
        }
        Ok(self.display(v))
    }

    /// Whether `v` has a `[[Construct]]` slot — i.e. `new v` / `Reflect.construct`
    /// is valid. Plain functions and classes qualify; native methods, bound values,
    /// and non-callables do not. (test262's `isConstructor` helper probes this via
    /// `Reflect.construct(fn, [], v)`, so getting it right matters across the suite.)
    pub(crate) fn is_constructor(&self, v: Value) -> bool {
        if !v.is_heap() {
            return false;
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Class(_) => true,
            // The built-in constructor globals (Object/Array/Map/…) are constructors.
            HeapObj::Object(m) => m.is_ctor,
            _ => false,
        }
    }

    /// JS `SameValue` (Object.is): like SameValueZero but +0 and -0 are distinct.
    pub(crate) fn same_value(&self, a: Value, b: Value) -> bool {
        if a.is_number() && b.is_number() {
            let (x, y) = (a.as_f64(), b.as_f64());
            if x == 0.0 && y == 0.0 {
                return x.is_sign_negative() == y.is_sign_negative();
            }
            if x.is_nan() && y.is_nan() {
                return true;
            }
            return x == y;
        }
        self.same_value_zero(a, b)
    }

    pub(crate) fn same_value_zero(&self, a: Value, b: Value) -> bool {
        if a.is_number() && b.is_number() {
            let (na, nb) = (a.as_f64(), b.as_f64());
            return na == nb || (na.is_nan() && nb.is_nan());
        }
        self.values_strict_eq(a, b)
    }

    pub(crate) fn values_strict_eq(&self, a: Value, b: Value) -> bool {
        if a.bits() == b.bits() {
            if a.is_double() && a.as_f64().is_nan() {
                return false;
            }
            return true;
        }
        if a.is_number() && b.is_number() {
            return a.as_f64() == b.as_f64();
        }
        if a.is_heap() && b.is_heap() {
            let (ai, bi) = (a.heap_index(), b.heap_index());
            if self.heap.is_str_like(ai) && self.heap.is_str_like(bi) {
                return self.heap.str_eq(ai, bi);
            }
            if let (HeapObj::BigInt(x), HeapObj::BigInt(y)) = (self.heap.get(ai), self.heap.get(bi)) {
                return x == y;
            }
        }
        false
    }

    /// JS loose equality `==` (the Abstract Equality Comparison). Same-type
    /// compares like `===`; cross-type coerces per spec: null == undefined;
    /// number vs string coerces the string to a number; boolean coerces to a
    /// number; an object vs a primitive coerces the object to its primitive
    /// (here: string coercion, since we have no valueOf). NaN is never equal.
    /// Whether `v` is an OBJECT for abstract-equality purposes — a heap value
    /// that is not a string/symbol/bigint primitive. (Boxed wrappers count as
    /// objects; they ToPrimitive to their wrapped value.)
    fn is_eq_object(&self, v: Value) -> bool {
        v.is_heap()
            && !matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_)
            )
    }

    pub(crate) fn loose_eq(&mut self, a: Value, b: Value) -> Result<bool, Thrown> {
        // Object vs primitive (`[1] == 1`, `{} == "[object Object]"`,
        // `Object('x') == 'x'`): ToPrimitive the object side, then retry. Two
        // objects fall through to reference equality; object vs null/undefined is
        // never ToPrimitive'd (handled by the nullish check below).
        let (a_obj, b_obj) = (self.is_eq_object(a), self.is_eq_object(b));
        if a_obj && !b_obj && !b.is_nullish() {
            let pa = self.to_primitive_default(a)?;
            return self.loose_eq(pa, b);
        }
        if b_obj && !a_obj && !a.is_nullish() {
            let pb = self.to_primitive_default(b)?;
            return self.loose_eq(a, pb);
        }
        // BigInt loose equality compares mathematical values across types
        // (`1n == 1`, `1n == "1"`, `1n == true`), so handle it before the generic
        // same-tag/heap shortcuts (two distinct 1n allocations aren't bit-equal).
        let (ab, bb) = (self.bigint_value(a), self.bigint_value(b));
        if ab.is_some() || bb.is_some() {
            return Ok(match (ab, bb) {
                (Some(x), Some(y)) => x == y,
                (Some(x), None) => self.bigint_loose_eq_other(x, b),
                (None, Some(y)) => self.bigint_loose_eq_other(y, a),
                _ => false,
            });
        }
        // Same NaN-box tag class → strict semantics already cover it.
        if (a.is_number() && b.is_number())
            || (a.is_bool() && b.is_bool())
            || (a.is_heap() && b.is_heap())
        {
            return Ok(self.values_strict_eq(a, b));
        }
        // null == undefined (and each with itself), but not with anything else.
        if a.is_nullish() || b.is_nullish() {
            return Ok(a.is_nullish() && b.is_nullish());
        }
        // From here neither side is null/undefined. Coerce toward numbers,
        // except string-vs-string (handled above via the heap case) and
        // string-vs-heapobject which JS compares by string.
        // boolean → number, then retry.
        if a.is_bool() {
            return self.loose_eq(Value::num(if a.as_bool() { 1.0 } else { 0.0 }), b);
        }
        if b.is_bool() {
            return self.loose_eq(a, Value::num(if b.as_bool() { 1.0 } else { 0.0 }));
        }
        // number vs string: coerce string to number.
        // string vs object / number vs object: coerce via to_number (objects
        // become NaN here, matching `1 == {}` → false; `"[object Object]"`
        // string comparisons aren't reached because both-heap is handled above).
        let an = self.to_number(a)?;
        let bn = self.to_number(b)?;
        Ok(an == bn)
    }

    /// `BigInt x == <non-BigInt other>`: compare mathematical values. Number must
    /// be a finite integer; a string is parsed as a BigInt; boolean → 0/1; an
    /// object/symbol/null/undefined is never loosely equal to a BigInt here.
    pub(crate) fn bigint_loose_eq_other(&self, x: i128, other: Value) -> bool {
        if other.is_bool() {
            return x == if other.as_bool() { 1 } else { 0 };
        }
        if other.is_number() {
            let n = other.as_f64();
            return n.is_finite() && n.fract() == 0.0 && (x as f64) == n;
        }
        if other.is_heap() && self.heap.is_str_like(other.heap_index()) {
            if let Some(s) = self.heap.str_cow(other.heap_index()) {
                let t = s.trim();
                if t.is_empty() {
                    return x == 0;
                }
                return parse_bigint_str(t).is_some_and(|y| y == x);
            }
        }
        false
    }

    // ── arithmetic / coercion helpers ──

    #[inline]
    pub(crate) fn add(&mut self, base: usize, a: u16, b: u16) -> Result<Value, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        self.add_values(va, vb)
    }

    /// The `+` operator on two already-fetched Values (shared by the interpreter's
    /// `Add`/`StrConcat` and the JIT's `jit_concat` helper).
    #[inline]
    /// ToPrimitive a boxed primitive (`new Number(5)` → 5) for use in operators;
    /// a non-box passes through. (Our boxes' valueOf returns the wrapped value, so
    /// this is ToPrimitive with the default/number hint.)
    pub(crate) fn unwrap_boxed(&self, v: Value) -> Value {
        if v.is_heap() {
            if let HeapObj::Boxed { value, .. } = self.heap.get(v.heap_index()) {
                return *value;
            }
        }
        v
    }

    pub(crate) fn add_values(&mut self, va: Value, vb: Value) -> Result<Value, Thrown> {
        let (va, vb) = (self.unwrap_boxed(va), self.unwrap_boxed(vb));
        // A Symbol operand can't be added: ToString (string side) and ToNumber
        // (numeric side) both throw for a Symbol.
        for v in [va, vb] {
            if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
                return Err(Thrown("TypeError: Cannot convert a Symbol value to a string".into()));
            }
        }
        // Fast path: int + int with overflow check.
        if va.is_int() && vb.is_int() {
            return Ok(match va.as_int().checked_add(vb.as_int()) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + vb.as_int() as f64),
            });
        }
        // BigInt `+`: both BigInt → addition; BigInt + string → concatenation
        // (ToString the BigInt); BigInt + anything-else → mixing TypeError.
        let (ab, bb) = (self.bigint_value(va), self.bigint_value(vb));
        if ab.is_some() || bb.is_some() {
            if let (Some(x), Some(y)) = (ab, bb) {
                return Ok(self.make_bigint(x.wrapping_add(y)));
            }
            let other = if ab.is_some() { vb } else { va };
            let other_is_str = other.is_heap() && self.heap.is_str_like(other.heap_index());
            if !other_is_str {
                return Err(Thrown(
                    "TypeError: Cannot mix BigInt and other types, use explicit conversions".into(),
                ));
            }
            // else: fall through to string concatenation (to_str_idx → decimal).
        }
        // If either side is a heap value, reduce both to primitives first
        // (ToPrimitive, default hint). If either primitive is a String, `+` is
        // string concatenation; otherwise it is numeric addition.
        if va.is_heap() || vb.is_heap() {
            let pa = self.to_primitive_default(va)?;
            let pb = self.to_primitive_default(vb)?;
            let pa_str = pa.is_heap() && self.heap.is_str_like(pa.heap_index());
            let pb_str = pb.is_heap() && self.heap.is_str_like(pb.heap_index());
            if pa_str || pb_str {
                // Build a rope (cons-string) in O(1) — children point at existing
                // flat strings / ropes, so a `s += x` loop is O(n) overall.
                let li = self.to_str_idx(pa);
                let ri = self.to_str_idx(pb);
                let llen = self.heap.str_char_len(li).unwrap_or(0);
                let rlen = self.heap.str_char_len(ri).unwrap_or(0);
                return Ok(Value::heap(self.heap.alloc_cons(li, ri, llen + rlen)));
            }
            return Ok(Value::num(self.to_number(pa)? + self.to_number(pb)?));
        }
        Ok(Value::num(self.to_number(va)? + self.to_number(vb)?))
    }

    /// `acc + val` as a string append that MUTATES `acc`'s buffer in place when
    /// `acc` is a uniquely-owned, non-interned flat string (`Str` at a user heap
    /// index). Otherwise — `acc` is the interned `""`/single-char (first append),
    /// a rope, or not a string — it allocates a FRESH non-interned flat string
    /// `display(acc) + display(val)` (never interned, so the NEXT append mutates
    /// it). Correctness rests on the emitter's linearity proof: the only reference
    /// to the mutated buffer is the accumulator itself, so the mutation is
    /// unobservable. Returns the (possibly unchanged) accumulator Value.
    pub(crate) fn str_append_inplace(&mut self, acc: Value, val: Value) -> Value {
        let mutable = acc.is_heap()
            && acc.heap_index() > crate::heap::INTERN_EMPTY
            && matches!(self.heap.get(acc.heap_index()), HeapObj::Str(_));
        // Fast path: appending a single decimal digit (the `s += i%10` shape) —
        // no temporary allocation for the value's string form.
        if mutable && val.is_int() {
            let n = val.as_int();
            if (0..=9).contains(&n) {
                if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                    js.bytes.push((b'0' + n as u8) as char);
                    js.char_len += 1;
                    return acc;
                }
            }
        }
        // General: materialise `val`'s string form (same coercion as `+`).
        let ri = self.to_str_idx(val);
        let add: String = self.heap.str_cow(ri).map(|c| c.into_owned()).unwrap_or_default();
        if mutable {
            if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                let cl = add.chars().count();
                let asc = add.is_ascii();
                js.bytes.push_str(&add);
                js.char_len += cl;
                js.ascii &= asc;
                return acc;
            }
        }
        // Fresh buffer (first append / interned / rope acc): flatten acc + add into
        // a NON-interned `Str` (bypass `alloc_str`'s interning so it's mutable next).
        let li = self.to_str_idx(acc);
        let mut s: String =
            self.heap.str_cow(li).map(|c| c.into_owned()).unwrap_or_default();
        s.push_str(&add);
        Value::heap(self.heap.alloc(HeapObj::Str(crate::heap::JsStr::new(s))))
    }

    /// Heap index of a string-like object representing `v`: `v`'s own index when
    /// it is already a string (flat or rope), else a freshly allocated flat
    /// string from `v`'s string coercion. Used to build rope children.
    pub(crate) fn to_str_idx(&mut self, v: Value) -> u32 {
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            return v.heap_index();
        }
        // A single-digit int is a 1-char ASCII string, already interned at its
        // byte — return that slot directly (no temporary `String` alloc). This is
        // the hot `s += (i % 10)` digit-concat case.
        if v.is_int() {
            let n = v.as_int();
            if (0..=9).contains(&n) {
                return (b'0' as i32 + n) as u32;
            }
        }
        let s = self.display(v);
        self.heap.alloc_str(s)
    }

    #[inline]
    pub(crate) fn cmp_lt(&mut self, base: usize, a: u16, b: u16) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() < vb.as_int());
        }
        // Abstract relational comparison: ToPrimitive (number hint) both operands
        // first (`[2] < 10`, `{valueOf(){return 5}} < 10`), then compare two
        // strings lexicographically, else numerically.
        let va = self.to_primitive_default(va)?;
        let vb = self.to_primitive_default(vb)?;
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_lt());
        }
        Ok(self.to_number(va)? < self.to_number(vb)?)
    }
    #[inline]
    pub(crate) fn cmp_le(&mut self, base: usize, a: u16, b: u16) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() <= vb.as_int());
        }
        let va = self.to_primitive_default(va)?;
        let vb = self.to_primitive_default(vb)?;
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_le());
        }
        Ok(self.to_number(va)? <= self.to_number(vb)?)
    }

    /// JS relational comparison of two STRING operands is lexicographic (by code
    /// unit) — not numeric. Returns the `Ordering` when both are string-like, else
    /// `None` (the caller falls back to numeric comparison). Mirrors the engine's
    /// code-point ordering (≈ UTF-16 for the BMP; astral chars are a known edge).
    pub(crate) fn str_relational(&self, va: Value, vb: Value) -> Option<std::cmp::Ordering> {
        if va.is_heap()
            && vb.is_heap()
            && self.heap.is_str_like(va.heap_index())
            && self.heap.is_str_like(vb.heap_index())
        {
            let sa = self.heap.str_cow(va.heap_index())?;
            let sb = self.heap.str_cow(vb.heap_index())?;
            return Some(sa.as_ref().cmp(sb.as_ref()));
        }
        None
    }

    pub(crate) fn strict_eq(&self, base: usize, a: u16, b: u16) -> bool {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        // Same bits → equal (covers int, bool, null, undefined, same heap idx).
        if va.bits() == vb.bits() {
            // NaN !== NaN even with identical bits.
            if va.is_double() && va.as_f64().is_nan() {
                return false;
            }
            return true;
        }
        // Numeric cross-representation (int vs double) compares by value.
        if va.is_number() && vb.is_number() {
            return va.as_f64() == vb.as_f64();
        }
        // Distinct heap strings with equal contents are `===` equal.
        if va.is_heap() && vb.is_heap() {
            let (ai, bi) = (va.heap_index(), vb.heap_index());
            // Two DISTINCT interned single-ASCII-char slots (idx < INTERN_EMPTY,
            // see Heap::new) are different chars — bits already differ here, so
            // they can't be equal; skip the content compare. This is the hot
            // `s[i] === 'x'` char-check in scanners/lexers.
            if ai < crate::heap::INTERN_EMPTY && bi < crate::heap::INTERN_EMPTY {
                return false;
            }
            if self.heap.is_str_like(ai) && self.heap.is_str_like(bi) {
                return self.heap.str_eq(ai, bi);
            }
            // BigInt === BigInt compares by value (1n === 1n), not heap identity.
            if let (HeapObj::BigInt(x), HeapObj::BigInt(y)) =
                (self.heap.get(ai), self.heap.get(bi))
            {
                return x == y;
            }
        }
        false
    }

    #[inline]
    pub(crate) fn truthy(&self, v: Value) -> bool {
        if let Some(t) = v.truthy_primitive() {
            return t;
        }
        // Heap: empty string is falsy; 0n is falsy; everything else truthy.
        if let Some(empty) = self.heap.str_is_empty(v.heap_index()) {
            return !empty;
        }
        if let HeapObj::BigInt(n) = self.heap.get(v.heap_index()) {
            return *n != 0;
        }
        true
    }

    pub(crate) fn to_number(&self, v: Value) -> Result<f64, Thrown> {
        if v.is_number() {
            return Ok(v.as_f64());
        }
        if v.is_bool() {
            return Ok(if v.as_bool() { 1.0 } else { 0.0 });
        }
        if v.is_null() {
            return Ok(0.0);
        }
        if v.is_undefined() {
            return Ok(f64::NAN);
        }
        // A Date coerces to its epoch ms (so `d2 - d1`, `+d`, `d1 < d2` work).
        if let HeapObj::Date(ms) = self.heap.get(v.heap_index()) {
            return Ok(*ms);
        }
        // A boxed primitive coerces to its wrapped value's number (ToPrimitive).
        if let HeapObj::Boxed { value, .. } = self.heap.get(v.heap_index()) {
            return self.to_number(*value);
        }
        // ToNumber of a Symbol is a TypeError.
        if matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
            return Err(Thrown("TypeError: Cannot convert a Symbol value to a number".into()));
        }
        // A BigInt's numeric value (for `Number(1n)` and relational comparison;
        // arithmetic mixing is rejected earlier by `bigint_binop`).
        if let HeapObj::BigInt(n) = self.heap.get(v.heap_index()) {
            return Ok(*n as f64);
        }
        if let Some(s) = self.heap.str_cow(v.heap_index()) {
            let t = s.trim();
            if t.is_empty() {
                return Ok(0.0);
            }
            // Non-decimal integer literals `0x…`/`0o…`/`0b…` (StringNumericLiteral;
            // no sign allowed). Fold digits into an f64 so arbitrarily long
            // literals don't overflow.
            let radix = match t.as_bytes() {
                [b'0', b'x' | b'X', ..] => Some((16u32, &t[2..])),
                [b'0', b'o' | b'O', ..] => Some((8, &t[2..])),
                [b'0', b'b' | b'B', ..] => Some((2, &t[2..])),
                _ => None,
            };
            if let Some((base, digits)) = radix {
                let mut acc = 0.0f64;
                for c in digits.chars() {
                    match c.to_digit(base) {
                        Some(d) => acc = acc * base as f64 + d as f64,
                        None => return Ok(f64::NAN),
                    }
                }
                return Ok(if digits.is_empty() { f64::NAN } else { acc });
            }
            return Ok(t.parse::<f64>().unwrap_or(f64::NAN));
        }
        Ok(f64::NAN)
    }

    /// `ToNumber(v)` that honours a user `valueOf`/`toString` when `v` is an
    /// object (ToPrimitive with the number hint) — unlike the immutable
    /// `to_number`, which returns NaN for an un-handled object. Primitives and the
    /// already-handled heap types (Date/Boxed/Symbol/BigInt/String) defer straight
    /// to `to_number`; a plain object is reduced to a primitive first.
    /// `ToIntegerOrInfinity(v)` clamped to `i64` — ToNumber then NaN→0, truncate
    /// toward zero (±Infinity saturate to i64::MAX/MIN). Backs string index/
    /// position args (`charAt`/`charCodeAt`/`at`/…), which use ToInteger, not a
    /// plain number cast (so `"42".charAt(true)` is index 1, `"1"` is 1, etc).
    pub(crate) fn to_integer_or_zero(&mut self, v: Value) -> Result<i64, Thrown> {
        let n = self.to_number_coerce(v)?;
        if n.is_nan() {
            return Ok(0);
        }
        let t = n.trunc();
        Ok(if t >= i64::MAX as f64 {
            i64::MAX
        } else if t <= i64::MIN as f64 {
            i64::MIN
        } else {
            t as i64
        })
    }

    pub(crate) fn to_number_coerce(&mut self, v: Value) -> Result<f64, Thrown> {
        if !v.is_heap() {
            return self.to_number(v);
        }
        if matches!(
            self.heap.get(v.heap_index()),
            HeapObj::Date(_)
                | HeapObj::Boxed { .. }
                | HeapObj::Symbol { .. }
                | HeapObj::BigInt(_)
                | HeapObj::Str(_)
                | HeapObj::Cons { .. }
        ) {
            return self.to_number(v);
        }
        let prim = self.to_primitive_number(v)?;
        self.to_number(prim)
    }

    /// ToPrimitive's `@@toPrimitive` hook (ES ToPrimitive step 2a-c): if `v` is an
    /// object with a callable `Symbol.toPrimitive` ("@@toPrimitive") method, invoke
    /// it with the hint ("number" / "string" / "default") and require a primitive
    /// result (else TypeError). Returns `None` when there is no such method, so the
    /// caller falls back to OrdinaryToPrimitive (valueOf/toString). Already-primitive
    /// heap values (str/bigint/symbol) and boxed wrappers are left to the caller.
    pub(crate) fn symbol_to_primitive(&mut self, v: Value, hint: &str) -> Result<Option<Value>, Thrown> {
        if !v.is_heap()
            || matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::BigInt(_)
                    | HeapObj::Symbol { .. }
                    | HeapObj::Boxed { .. }
            )
        {
            return Ok(None);
        }
        let f = self.get_prop(v, "@@toPrimitive")?;
        if !self.is_callable(f) {
            return Ok(None);
        }
        let hv = self.alloc_str(hint.to_string());
        let r = self.call_value(f, v, &[hv])?;
        let is_obj = r.is_heap()
            && !matches!(
                self.heap.get(r.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::Symbol { .. }
            );
        if is_obj {
            return Err(Thrown("TypeError: Cannot convert object to primitive value".into()));
        }
        Ok(Some(r))
    }

    /// ToPrimitive(v, "number"): the `@@toPrimitive` hook, else OrdinaryToPrimitive
    /// (`valueOf` then `toString`, first primitive wins; TypeError if neither does).
    pub(crate) fn to_primitive_number(&mut self, v: Value) -> Result<Value, Thrown> {
        // A boxed primitive wrapper yields its wrapped primitive directly.
        if v.is_heap() {
            if let HeapObj::Boxed { value, .. } = self.heap.get(v.heap_index()) {
                return Ok(*value);
            }
        }
        if let Some(p) = self.symbol_to_primitive(v, "number")? {
            return Ok(p);
        }
        for name in ["valueOf", "toString"] {
            let f = self.get_prop(v, name)?;
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                let is_primitive = !r.is_heap()
                    || matches!(
                        self.heap.get(r.heap_index()),
                        HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::Symbol { .. }
                    );
                if is_primitive {
                    return Ok(r);
                }
            }
        }
        Err(Thrown("TypeError: Cannot convert object to primitive value".into()))
    }

    /// ToPrimitive(v) with the DEFAULT hint (used by binary `+` and `==`): an
    /// already-primitive value passes through; a Date prefers `toString` (string
    /// hint), every other object prefers `valueOf` (number hint). The
    /// `Symbol.toPrimitive` hook is not consulted yet.
    pub(crate) fn to_primitive_default(&mut self, v: Value) -> Result<Value, Thrown> {
        if !v.is_heap() {
            return Ok(v);
        }
        if matches!(
            self.heap.get(v.heap_index()),
            HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::Symbol { .. }
        ) {
            return Ok(v);
        }
        // A boxed primitive wrapper (String/Number/Boolean/Symbol/BigInt) yields
        // its wrapped primitive — the built-in valueOf would return the same.
        if let HeapObj::Boxed { value, .. } = self.heap.get(v.heap_index()) {
            return Ok(*value);
        }
        if let Some(p) = self.symbol_to_primitive(v, "default")? {
            return Ok(p);
        }
        let order: [&str; 2] = if matches!(self.heap.get(v.heap_index()), HeapObj::Date(_)) {
            ["toString", "valueOf"]
        } else {
            ["valueOf", "toString"]
        };
        for name in order {
            let f = self.get_prop(v, name)?;
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                let is_primitive = !r.is_heap()
                    || matches!(
                        self.heap.get(r.heap_index()),
                        HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::Symbol { .. }
                    );
                if is_primitive {
                    return Ok(r);
                }
            }
        }
        Err(Thrown("TypeError: Cannot convert object to primitive value".into()))
    }

    /// String COERCION (`String(v)`, `'' + v`, property keys). Arrays join with
    /// commas; objects become `[object Object]` — JS `toString` semantics.
    pub(crate) fn display(&self, v: Value) -> String {
        if v.is_int() {
            v.as_int().to_string()
        } else if v.is_double() {
            fmt_f64(v.as_f64())
        } else if v.is_bool() {
            v.as_bool().to_string()
        } else if v.is_null() {
            "null".into()
        } else if v.is_undefined() {
            "undefined".into()
        } else if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Proxy { target, .. } => return self.display(*target),
                HeapObj::Temporal { kind: 0, fields } => {
                    let mut f = [0i64; 10];
                    for (i, s) in f.iter_mut().enumerate() {
                        *s = *fields.get(i).unwrap_or(&0);
                    }
                    duration_to_string(&f)
                }
                HeapObj::Temporal { kind: 1, fields } => {
                    iso_date_string(fields[0], fields[1], fields[2])
                }
                HeapObj::Temporal { kind: 2, fields } => {
                    let mut f = [0i64; 6];
                    for (i, s) in f.iter_mut().enumerate() {
                        *s = *fields.get(i).unwrap_or(&0);
                    }
                    time_string(&f)
                }
                HeapObj::Temporal { kind: 3, fields } => {
                    let g = |i: usize| *fields.get(i).unwrap_or(&0);
                    format!(
                        "{}T{}",
                        iso_date_string(g(0), g(1), g(2)),
                        time_string(&[g(3), g(4), g(5), g(6), g(7), g(8)])
                    )
                }
                HeapObj::Temporal { kind: 4, fields } => {
                    let ns = ((fields[0] as i128) << 64) | ((fields[1] as u64) as i128);
                    instant_to_string(ns)
                }
                HeapObj::Temporal { kind: 5, fields } => year_month_string(fields[0], fields[1]),
                HeapObj::Temporal { kind: 6, fields } => format!("{:02}-{:02}", fields[1], fields[2]),
                HeapObj::Temporal { kind: 7, .. } => self.zdt_to_string(v.heap_index()),
                HeapObj::Temporal { .. } => "[object Temporal]".into(),
                HeapObj::Intl { .. } => "[object Object]".into(),
                HeapObj::Str(s) => s.bytes.clone(),
                HeapObj::Cons { .. } => {
                    let mut out = String::new();
                    self.heap.write_str(v.heap_index(), &mut out);
                    out
                }
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                    "function".into()
                }
                HeapObj::Cell(inner) => self.display(*inner),
                HeapObj::Array(items) => items
                    .iter()
                    .map(|e| if e.is_nullish() { String::new() } else { self.display(*e) })
                    .collect::<Vec<_>>()
                    .join(","),
                HeapObj::Object(_) => {
                    // Error instances ToString to "name: message" (Error.prototype.toString).
                    if self.is_error_instance(v.heap_index()) {
                        self.error_display_string(v.heap_index())
                    } else {
                        "[object Object]".into()
                    }
                }
                HeapObj::Class(c) => format!("class {} {{ }}", c.name),
                HeapObj::Map { .. } => "[object Map]".into(),
                HeapObj::Set(_) => "[object Set]".into(),
                HeapObj::WeakMap { .. } => "[object WeakMap]".into(),
                HeapObj::WeakSet(_) => "[object WeakSet]".into(),
                HeapObj::WeakRef(_) => "[object WeakRef]".into(),
                HeapObj::FinalizationRegistry { .. } => "[object FinalizationRegistry]".into(),
                HeapObj::Iterator { .. } => "[object Array Iterator]".into(),
                HeapObj::IterHelper { .. } => "[object Iterator Helper]".into(),
                // A boxed primitive stringifies as its wrapped value (ToString).
                HeapObj::Boxed { value, .. } => self.display(*value),
                // ToString of a Symbol actually throws (see `to_js_string`); this
                // infallible debug form is "Symbol(desc)".
                HeapObj::Symbol { desc, .. } => {
                    let d = if *desc == Value::UNDEFINED { String::new() } else { self.display(*desc) };
                    format!("Symbol({d})")
                }
                // ToString(BigInt) is the decimal digits with NO "n" (String(1n) === "1").
                HeapObj::BigInt(n) => n.to_string(),
                HeapObj::RegExp { source, flags, .. } => {
                    let s = if source.is_empty() { "(?:)" } else { source };
                    format!("/{s}/{flags}")
                }
                // ToString of a TypedArray is the comma-joined elements (like Array).
                HeapObj::TypedArray { length, .. } => {
                    let n = *length;
                    let idx = v.heap_index();
                    (0..n).map(|i| self.ta_elem_string(idx, i)).collect::<Vec<_>>().join(",")
                }
                HeapObj::ArrayBuffer { .. } => "[object ArrayBuffer]".into(),
                HeapObj::DataView { .. } => "[object DataView]".into(),
                HeapObj::Generator { .. } => "[object Generator]".into(),
                HeapObj::AsyncGenerator(_) => "[object AsyncGenerator]".into(),
                HeapObj::Promise { .. } => "[object Promise]".into(),
                HeapObj::BoundResolver { .. } => "function".into(),
                // Internal: never user-visible (an async call yields its Promise).
                HeapObj::AsyncState(_) => "[object Promise]".into(),
                HeapObj::Combinator { .. } | HeapObj::CombinatorResolver { .. } => {
                    "[object Object]".into()
                }
                // `String(date)` / `"" + date` → the date string (ISO here).
                HeapObj::Date(ms) => {
                    if ms.is_nan() {
                        "Invalid Date".into()
                    } else {
                        date_to_iso(*ms)
                    }
                }
            }
        } else {
            "undefined".into()
        }
    }

    /// INSPECT (`console.log` rendering). Strings are quoted only when nested;
    /// arrays/objects use node's spaced bracket style (`[ 1, 2, 3 ]`,
    /// `{ a: 1 }`).
    pub(crate) fn inspect(&self, v: Value) -> String {
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Str(s) => return s.bytes.clone(), // top-level strings unquoted
                HeapObj::Cons { .. } => {
                    let mut out = String::new();
                    self.heap.write_str(v.heap_index(), &mut out);
                    return out;
                }
                _ => return self.inspect_nested(v),
            }
        }
        self.display(v)
    }

    /// `console.log` label for a function value: `[Function: name]`, or
    /// `[Function (anonymous)]` for an arrow / unnamed expression (synthetic
    /// names start with `<`). Class methods are stored as `Class.method`; show
    /// just the method part, as node does.
    pub(crate) fn func_label(&self, fid: u32) -> String {
        let name = &self.func(fid as usize).name;
        if name.is_empty() || name.starts_with('<') {
            "[Function (anonymous)]".into()
        } else {
            let short = name.rsplit('.').next().unwrap_or(name);
            format!("[Function: {short}]")
        }
    }

    pub(crate) fn inspect_nested(&self, v: Value) -> String {
        if !v.is_heap() {
            return self.display(v);
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Proxy { target, .. } => {
                let t = *target;
                return self.inspect_nested(t);
            }
            HeapObj::Temporal { kind: 0, fields } => {
                let mut f = [0i64; 10];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                format!("Temporal.Duration <{}>", duration_to_string(&f))
            }
            HeapObj::Temporal { kind: 1, fields } => {
                format!("Temporal.PlainDate <{}>", iso_date_string(fields[0], fields[1], fields[2]))
            }
            HeapObj::Temporal { kind: 2, fields } => {
                let mut f = [0i64; 6];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                format!("Temporal.PlainTime <{}>", time_string(&f))
            }
            HeapObj::Temporal { kind: 3, fields } => {
                let g = |i: usize| *fields.get(i).unwrap_or(&0);
                format!(
                    "Temporal.PlainDateTime <{}T{}>",
                    iso_date_string(g(0), g(1), g(2)),
                    time_string(&[g(3), g(4), g(5), g(6), g(7), g(8)])
                )
            }
            HeapObj::Temporal { kind: 4, fields } => {
                let ns = ((fields[0] as i128) << 64) | ((fields[1] as u64) as i128);
                format!("Temporal.Instant <{}>", instant_to_string(ns))
            }
            HeapObj::Temporal { kind: 5, fields } => {
                format!("Temporal.PlainYearMonth <{}>", year_month_string(fields[0], fields[1]))
            }
            HeapObj::Temporal { kind: 6, fields } => {
                format!("Temporal.PlainMonthDay <{:02}-{:02}>", fields[1], fields[2])
            }
            HeapObj::Temporal { kind: 7, .. } => {
                format!("Temporal.ZonedDateTime <{}>", self.zdt_to_string(v.heap_index()))
            }
            HeapObj::Temporal { .. } => "[object Temporal]".into(),
            HeapObj::Intl { kind, .. } => {
                const NAMES: [&str; 10] = [
                    "NumberFormat", "DateTimeFormat", "Collator", "PluralRules", "ListFormat",
                    "RelativeTimeFormat", "Segmenter", "Locale", "DisplayNames", "DurationFormat",
                ];
                format!("Intl.{} {{}}", NAMES.get(*kind as usize).copied().unwrap_or("?"))
            }
            HeapObj::Str(s) => format!("'{}'", s.bytes),
            HeapObj::Cons { .. } => {
                let mut out = String::new();
                self.heap.write_str(v.heap_index(), &mut out);
                format!("'{out}'")
            }
            HeapObj::Func(id) => self.func_label(*id),
            HeapObj::Closure { func, .. } => self.func_label(*func),
            HeapObj::Bound { .. } => "[Function: bound]".into(),
            HeapObj::Native(_) => "[Function (native)]".into(),
            HeapObj::Cell(inner) => self.inspect_nested(*inner),
            HeapObj::Array(items) => {
                if items.is_empty() {
                    return "[]".into();
                }
                let parts: Vec<String> = items.iter().map(|e| self.inspect_nested(*e)).collect();
                format!("[ {} ]", parts.join(", "))
            }
            HeapObj::Object(map) => {
                // A class instance prints with its constructor name (`Pt { … }`).
                let prefix = match map.class {
                    Some(cidx) => match self.heap.get(cidx) {
                        HeapObj::Class(c) => format!("{} ", c.name),
                        _ => String::new(),
                    },
                    None => String::new(),
                };
                if map.keys.is_empty() {
                    return format!("{prefix}{{}}");
                }
                let parts: Vec<String> = map
                    .keys
                    .iter()
                    .zip(map.vals.iter())
                    .map(|(k, val)| format!("{k}: {}", self.inspect_nested(*val)))
                    .collect();
                format!("{prefix}{{ {} }}", parts.join(", "))
            }
            HeapObj::Class(c) => format!("[class {}]", c.name),
            HeapObj::Map { keys, vals } => {
                if keys.is_empty() {
                    return "Map(0) {}".into();
                }
                let parts: Vec<String> = keys
                    .iter()
                    .zip(vals.iter())
                    .map(|(k, v)| format!("{} => {}", self.inspect_nested(*k), self.inspect_nested(*v)))
                    .collect();
                format!("Map({}) {{ {} }}", keys.len(), parts.join(", "))
            }
            HeapObj::Set(items) => {
                if items.is_empty() {
                    return "Set(0) {}".into();
                }
                let parts: Vec<String> = items.iter().map(|v| self.inspect_nested(*v)).collect();
                format!("Set({}) {{ {} }}", items.len(), parts.join(", "))
            }
            HeapObj::WeakMap { .. } => "WeakMap { <items unknown> }".into(),
            HeapObj::WeakSet(_) => "WeakSet { <items unknown> }".into(),
            HeapObj::WeakRef(_) => "WeakRef {}".into(),
            HeapObj::FinalizationRegistry { .. } => "FinalizationRegistry {}".into(),
            HeapObj::Iterator { .. } => "Object [Array Iterator] {}".into(),
            HeapObj::IterHelper { .. } => "Object [Iterator Helper] {}".into(),
            HeapObj::Boxed { kind, value } => {
                let inner = self.inspect_nested(*value);
                match kind {
                    0 => format!("[String: {inner}]"),
                    1 => format!("[Number: {inner}]"),
                    _ => format!("[Boolean: {inner}]"),
                }
            }
            HeapObj::Symbol { desc, .. } => {
                let d = if *desc == Value::UNDEFINED { String::new() } else { self.display(*desc) };
                format!("Symbol({d})")
            }
            // console.log shows BigInt with the `n` suffix (1n), unlike ToString.
            HeapObj::BigInt(n) => format!("{n}n"),
            HeapObj::RegExp { source, flags, .. } => {
                let s = if source.is_empty() { "(?:)" } else { source };
                format!("/{s}/{flags}")
            }
            HeapObj::TypedArray { kind, length, .. } => {
                let (name, n, idx) = (native::TA_KINDS[*kind as usize].0, *length, v.heap_index());
                let parts: Vec<String> = (0..n).map(|i| self.ta_elem_string(idx, i)).collect();
                if n == 0 {
                    format!("{name}(0) []")
                } else {
                    format!("{name}({n}) [ {} ]", parts.join(", "))
                }
            }
            HeapObj::ArrayBuffer { data, .. } => format!("ArrayBuffer {{ byteLength: {} }}", data.len()),
            HeapObj::DataView { byte_length, .. } => {
                format!("DataView {{ byteLength: {byte_length} }}")
            }
            HeapObj::Generator { .. } => "Object [Generator] {}".into(),
            HeapObj::AsyncGenerator(_) => "Object [AsyncGenerator] {}".into(),
            HeapObj::Promise { state, result, .. } => match state {
                crate::heap::PromiseState::Pending => "Promise { <pending> }".into(),
                crate::heap::PromiseState::Fulfilled => {
                    format!("Promise {{ {} }}", self.inspect_nested(*result))
                }
                crate::heap::PromiseState::Rejected => {
                    format!("Promise {{ <rejected> {} }}", self.inspect_nested(*result))
                }
            },
            HeapObj::BoundResolver { .. } => "[Function (anonymous)]".into(),
            // Internal: never user-visible (an async call yields its Promise).
            HeapObj::AsyncState(_) => "Promise { <pending> }".into(),
            HeapObj::Combinator { .. } | HeapObj::CombinatorResolver { .. } => "[object Object]".into(),
            // node renders a Date in console.log as its ISO string (unquoted).
            HeapObj::Date(ms) => {
                if ms.is_nan() {
                    "Invalid Date".into()
                } else {
                    date_to_iso(*ms)
                }
            }
        }
    }

    /// Resolve a constant slot: most are plain Values; string constants are
    /// stored as a sentinel index into the function's `string_constants` and
    /// interned to a heap string on first use.
    #[inline]
    pub(crate) fn resolve_const(&mut self, func_id: u32, v: Value) -> Value {
        // String constants are encoded as `Value::heap(STRING_CONST_BIT | i)`.
        if v.is_heap() && (v.heap_index() & STRING_CONST_BIT) != 0 {
            let si = (v.heap_index() & !STRING_CONST_BIT) as usize;
            let s = self.func(func_id as usize).string_constants[si].clone();
            return self.alloc_str(s);
        }
        v
    }
}
