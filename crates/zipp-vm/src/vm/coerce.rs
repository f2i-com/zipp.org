#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// Largest combined length (UTF-16 units) that `a + b` builds as an immediate
/// FLAT string instead of a rope node. A small rope costs MORE than the copy:
/// the Cons allocation now plus a flatten allocation + objs write-back on
/// first content access (and Map/Set keys, property names, comparisons all
/// access content immediately) — the same reasoning as V8's ConsString
/// minimum length (13). `s += part` loops building LONG strings still get
/// O(1) rope appends (their combined length exceeds this almost immediately).
///
/// MEASURED 2026-07-26 — this was 24, which is far too eager to build ropes.
/// Real string-building code assembles a line from ~20 pieces and then READS it
/// (stores it, matches it, joins it), so the rope is flattened almost
/// immediately and every node allocated on the way was waste. Worse, a
/// `str + int` past the limit loses its fused fast path and pays TWO allocations
/// per part (a heap string for the number, then the Cons).
///
/// Sweeping the threshold against the real suite and an adversarial case:
///
/// ```text
/// units    markdown  regex-log-scan   many-2KB-strings (adversarial)
///    24       857ms          2610ms                       80ms
///   128       760ms          2210ms                       83ms
///   256       767ms          1965ms                       85ms
///   512       676ms          1980ms                      112ms
///  2048       613ms          1992ms                      646ms
/// ```
///
/// 256 takes essentially the whole regex-log-scan win (-25%) for a 6% cost on
/// the adversarial shape — building many strings up to the threshold one small
/// piece at a time, which is O(n²) copying and the reason this cannot simply be
/// raised without bound. Above 512 that term dominates and the curve inverts.
const SMALL_CONCAT_FLAT_UNITS: usize = 256;

/// Decimal ASCII form of an i32 written into a stack buffer: the digits live
/// in `buf[start..]` of the returned `(buf, start)`. No allocation — the
/// string⊕int concat fast path copies them straight into its result buffer.
#[inline]
pub(crate) fn fmt_i32_buf(n: i32) -> ([u8; 12], usize) {
    let mut buf = [0u8; 12];
    let mut i = buf.len();
    let mut m = (n as i64).unsigned_abs(); // i64: |i32::MIN| representable
    loop {
        i -= 1;
        buf[i] = b'0' + (m % 10) as u8;
        m /= 10;
        if m == 0 {
            break;
        }
    }
    if n < 0 {
        i -= 1;
        buf[i] = b'-';
    }
    (buf, i)
}

impl<'p> Vm<'p> {
    /// Clone an array's current elements out of the heap. Used before invoking
    /// callbacks so a heap reallocation during the call can't dangle a borrow.
    /// Read an array-like receiver's elements (`this.length` coerced via ToLength,
    /// then `this[0 .. length]`) into a Vec — backs the generic Array.prototype
    /// methods invoked via `.call(arrayLike, …)` on a non-array (object or string).
    pub(crate) fn array_like_read(&mut self, idx: u32) -> Result<Vec<Value>, Thrown> {
        let this = Value::heap(idx);
        if let HeapObj::Array(items) = self.heap.get(idx) {
            return Ok(items.clone());
        }
        // len = ToLength(Get(this, "length")): a throwing `length` getter, or a
        // Symbol / throwing-valueOf length, propagates (ReturnIfAbrupt) instead of
        // being read as 0, so a generic Array.prototype method surfaces the
        // TypeError before any element/predicate work. The full ToPrimitive path
        // also honours an array-like whose `length` is an object with valueOf.
        let lv = self.get_prop(this, "length")?;
        let len = self.to_number_coerce(lv)?;
        // ToLength: a positive `len` (including +Infinity / "Infinity" / a huge
        // finite) clamps to MAX_DENSE_ARRAY_LEN; NaN and ≤0 (incl. -Infinity) → 0.
        // `len as usize` saturates for +Infinity, so the `.min` bounds it.
        let len = if len > 0.0 {
            (len as usize).min(crate::vm::MAX_DENSE_ARRAY_LEN)
        } else {
            0
        };
        let mut out = Vec::with_capacity(len.min(4096));
        for i in 0..len {
            // An index getter that throws must propagate (ReturnIfAbrupt) — absent
            // properties already come back Ok(undefined) from get_index.
            out.push(self.get_index(this, Value::int(i as i32))?);
        }
        Ok(out)
    }

    pub(crate) fn array_snapshot(&self, idx: u32) -> Vec<Value> {
        match self.heap.get(idx) {
            // A hole reads as `undefined` for every snapshot consumer (join, slice,
            // concat, spread, sort, JSON, …) — the internal HOLE sentinel must never
            // leak to user code. Hole-SENSITIVE methods (the callback/search/find
            // family) take the live HasProperty+Get path instead, not this snapshot.
            HeapObj::Array(items) => {
                items.iter().map(|&v| if v.is_hole() { Value::UNDEFINED } else { v }).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Whether a real array currently holds any HOLE (an absent element). Used to
    /// route the hole-sensitive callback/search methods off the dense-snapshot fast
    /// path onto the live HasProperty+Get protocol; a hole-free array keeps the fast
    /// path. O(n), but only consulted by methods that are already O(n) (and whose
    /// per-element JS callback dominates).
    pub(crate) fn array_has_holes(&self, idx: u32) -> bool {
        matches!(self.heap.get(idx), HeapObj::Array(items) if items.iter().any(|v| v.is_hole()))
    }

    /// IsArray(v) (ES 7.2.2): true for an Array exotic, recursing through Proxy
    /// targets (a revoked or non-array proxy is not an array). Used by
    /// `Array.isArray` and the `IsArray` op so `Array.isArray(new Proxy([], …))`
    /// is true. (The revoked-proxy-throws nuance is approximated as `false`.)
    /// IsArray(v) with the spec's revoked-proxy TypeError (used where the
    /// abrupt completion is observable: Array.isArray, the IsArray op,
    /// IsConcatSpreadable, ArraySpeciesCreate).
    pub(crate) fn value_is_array_throwing(&self, v: Value) -> Result<bool, Thrown> {
        if !v.is_heap() {
            return Ok(false);
        }
        // %Array.prototype% is itself an Array exotic object.
        if self.arr_proto != 0 && v.heap_index() == self.arr_proto {
            return Ok(true);
        }
        let mut idx = v.heap_index();
        for _ in 0..1000 {
            match self.heap.get(idx) {
                HeapObj::Array(_) => return Ok(!self.arguments_objs.contains_key(&idx)),
                HeapObj::Proxy { target, revoked, .. } => {
                    if *revoked {
                        return Err(Thrown(
                            "TypeError: Cannot perform 'IsArray' on a proxy that has been revoked"
                                .into(),
                        ));
                    }
                    if !target.is_heap() {
                        return Ok(false);
                    }
                    idx = target.heap_index();
                }
                _ => return Ok(false),
            }
        }
        Ok(false)
    }

    pub(crate) fn value_is_array(&self, v: Value) -> bool {
        if !v.is_heap() {
            return false;
        }
        // %Array.prototype% is itself an Array exotic object.
        if self.arr_proto != 0 && v.heap_index() == self.arr_proto {
            return true;
        }
        let mut idx = v.heap_index();
        for _ in 0..1000 {
            match self.heap.get(idx) {
                // An `arguments` exotic is Array-backed internally but is an ordinary
                // object ([[ParameterMap]]), NOT an Array exotic — IsArray is false.
                HeapObj::Array(_) => return !self.arguments_objs.contains_key(&idx),
                HeapObj::Proxy { target, revoked, .. } if !*revoked && target.is_heap() => {
                    idx = target.heap_index();
                }
                _ => return false,
            }
        }
        false
    }

    /// CreateListFromArrayLike(obj) (ES 7.3.18): the elements `0 .. ToLength(obj.length)`
    /// of an array-LIKE (any object), read via Get — so `Function.prototype.apply` /
    /// `Reflect.apply` accept `{length, 0, 1, …}`, not only real arrays. Throws if
    /// `obj` is not an object.
    pub(crate) fn create_list_from_array_like(&mut self, obj: Value) -> Result<Vec<Value>, Thrown> {
        if !self.is_object_value(obj) {
            return Err(Thrown(
                "TypeError: CreateListFromArrayLike called on a non-object".into(),
            ));
        }
        if let HeapObj::Array(items) = self.heap.get(obj.heap_index()) {
            // An arguments object takes the generic route below: its `length`
            // is an ordinary (mutable, even deletable) property and a LIVE-
            // mapped index must read the formal's register, not the snapshot.
            if !self.arguments_objs.contains_key(&obj.heap_index()) {
                return Ok(items.clone()); // dense fast path
            }
        }
        let len_v = self.get_prop(obj, "length")?;
        let len = self.to_integer_or_zero(len_v)?.clamp(0, (1i64 << 53) - 1) as usize;
        let mut out = Vec::with_capacity(len.min(4096));
        for i in 0..len {
            out.push(self.get_index(obj, Value::int(i as i32))?);
        }
        Ok(out)
    }

    /// IsConcatSpreadable(O) (ES 23.1.3.1.1): a `Symbol.isConcatSpreadable`
    /// ("@@isConcatSpreadable") flag overrides — when present it is ToBoolean'd;
    /// otherwise the value is spreadable iff it is an Array. Non-objects are never
    /// spreadable. Used by `Array.prototype.concat`.
    pub(crate) fn is_concat_spreadable(&mut self, v: Value) -> Result<bool, Thrown> {
        // Non-OBJECTS are never spreadable (step 1: If O is not an Object,
        // return false) — including heap-allocated string/symbol/bigint
        // PRIMITIVES, even when `String.prototype[@@isConcatSpreadable]` is set.
        if !self.is_object_value(v) {
            return Ok(false);
        }
        let flag = self.get_prop(v, "@@isConcatSpreadable")?;
        if flag != Value::UNDEFINED {
            return Ok(self.truthy(flag));
        }
        // Step 4 is the REAL IsArray: it pierces Proxy targets (a proxy over an
        // array IS spreadable) and throws on a revoked proxy.
        self.value_is_array_throwing(v)
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
                HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
            )
    }

    /// `ToObject(v)` — backs `Object(x)` / `new Object(x)`. Primitives box into
    /// the matching wrapper (string/number/boolean/symbol/bigint); null and
    /// undefined become a fresh ordinary object; an existing object (array,
    /// function, …) is returned unchanged. `Boxed.kind`: 0=String, 1=Number,
    /// 2=Boolean, 3=Symbol, 4=BigInt.
    /// RequireObjectCoercible: `null`/`undefined` cannot be converted to an
    /// object, so methods that ToObject their `this` throw a TypeError on them.
    pub(crate) fn require_object_coercible(&self, v: Value) -> Result<(), Thrown> {
        if v == Value::NULL || v == Value::UNDEFINED {
            return Err(Thrown(
                "TypeError: cannot convert null or undefined to an object".into(),
            ));
        }
        Ok(())
    }

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
            HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => 4u8,
            _ => return Ok(v),
        };
        Ok(Value::heap(self.heap.alloc(HeapObj::Boxed { kind, value: v })))
    }

    /// `ToString(v)` as a Rust String, honouring a user `toString`/`valueOf` on an
    /// object (ToPrimitive with the string hint). Primitives and engine strings use
    /// `display`; a plain object with only the built-in (native) toString also falls
    /// back to `display` (which already yields "[object Object]" / the array join).
    /// `ToString(v)` as a string VALUE: IDENTITY (the same heap string — exact
    /// WTF-8 content, lone surrogates preserved) when `v` is already a string;
    /// otherwise the `to_js_string` coercion (observable, may throw),
    /// allocated. Use this wherever the coerced string becomes a VALUE the
    /// program can read back (ToStr op, String(x), error.message, …) — the
    /// `to_js_string` + `alloc_str` pair would silently lossy-copy a
    /// lone-surrogate string.
    pub(crate) fn to_str_value(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            return Ok(v);
        }
        let s = self.to_js_string(v)?;
        Ok(self.alloc_str(s))
    }

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
        // ToString(bigint) is BigIntToString - direct and unobservable (the
        // user-patchable BigInt.prototype.toString must NOT run for a
        // primitive BigInt; only a Boxed BigInt object takes the protocol).
        match self.heap.get(v.heap_index()) {
            HeapObj::BigInt(b) => return Ok(b.to_string()),
            HeapObj::BigIntBig(b) => return Ok(b.to_string()),
            _ => {}
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
                // ANY primitive result completes OrdinaryToPrimitive — recurse so a
                // BigInt stringifies (via BigInt.prototype.toString) and a Symbol
                // throws (ToString of a Symbol is a TypeError), not just strings /
                // numbers. An object result means this method didn't yield a
                // primitive, so fall through to the next.
                if !self.is_object_value(r) {
                    return self.to_js_string(r);
                }
            }
        }
        // OrdinaryToPrimitive exhausted both methods without a primitive (each
        // returned an object, or neither was callable on a null-prototype object):
        // ToPrimitive throws rather than silently producing "[object Object]".
        Err(Thrown("TypeError: Cannot convert object to primitive value".into()))
    }

    /// `ToPrimitive(v, hint String)` returning the primitive Value — which may be a
    /// Symbol (so `ToPropertyKey` can use it as a symbol key rather than throwing on
    /// a stringify). Honours `@@toPrimitive`, then OrdinaryToPrimitive (`toString`
    /// then `valueOf`). A non-object value is already primitive.
    pub(crate) fn to_primitive_string(&mut self, v: Value) -> Result<Value, Thrown> {
        if !self.is_object_value(v) {
            return Ok(v); // already a primitive (string / number / Symbol / …)
        }
        if let Some(p) = self.symbol_to_primitive(v, "string")? {
            return Ok(p);
        }
        for name in ["toString", "valueOf"] {
            let f = self.get_prop(v, name)?;
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                if !self.is_object_value(r) {
                    return Ok(r);
                }
            }
        }
        Err(Thrown("TypeError: Cannot convert object to primitive value".into()))
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
            // A class is always a constructor. A plain function/closure is too, but a
            // generator / async / arrow / concise-method function has no [[Construct]].
            HeapObj::Class(_) => true,
            HeapObj::Func(_) | HeapObj::Closure { .. } => {
                match self.heap.as_callable(v.heap_index()) {
                    Some((fid, _)) => {
                        let fp = self.func(fid as usize);
                        !(fp.is_generator || fp.is_async || fp.non_constructable)
                    }
                    None => true,
                }
            }
            // The built-in constructor globals (Object/Array/Map/…) are constructors.
            HeapObj::Object(m) => m.is_ctor,
            // A bound function exposes [[Construct]] iff its target does.
            HeapObj::Bound { target, .. } => self.is_constructor(*target),
            // A non-revoked Proxy is a constructor iff its target is (the
            // `construct` trap is only callable when the target has [[Construct]]).
            HeapObj::Proxy { target, revoked, .. } => !*revoked && self.is_constructor(*target),
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

    // The single implementations live in vm/collections.rs (free functions
    // over &Heap) so the collection hash index can share them exactly — its
    // SameValueZero hash must never diverge from this equality.
    pub(crate) fn same_value_zero(&self, a: Value, b: Value) -> bool {
        super::collections::svz_eq(&self.heap, a, b)
    }

    pub(crate) fn values_strict_eq(&self, a: Value, b: Value) -> bool {
        super::collections::strict_eq(&self.heap, a, b)
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
                HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
            )
    }

    pub(crate) fn loose_eq(&mut self, a: Value, b: Value) -> Result<bool, Thrown> {
        // An [[IsHTMLDDA]] exotic (`document.all`) loosely equals null/undefined.
        if !self.is_htmldda.is_empty() {
            if a.is_heap() && b.is_nullish() && self.is_htmldda.contains(&a.heap_index()) {
                return Ok(true);
            }
            if b.is_heap() && a.is_nullish() && self.is_htmldda.contains(&b.heap_index()) {
                return Ok(true);
            }
        }
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
        let (ab, bb) = (self.bigint_val(a), self.bigint_val(b));
        if ab.is_some() || bb.is_some() {
            return Ok(match (ab, bb) {
                (Some(x), Some(y)) => x == y,
                (Some(x), None) => self.bigint_loose_eq_other(&x, b),
                (None, Some(y)) => self.bigint_loose_eq_other(&y, a),
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
        // A Symbol primitive is never loosely equal to a Number here (two Symbols
        // are both-heap and handled above; Symbol-vs-string is both-heap too). It
        // must NOT be ToNumber'd — that throws — so short-circuit to false. This
        // is reached when the object side ToPrimitive'd to a Symbol (e.g.
        // `0 == {[Symbol.toPrimitive]: () => Symbol()}`).
        let a_sym = a.is_heap() && matches!(self.heap.get(a.heap_index()), HeapObj::Symbol { .. });
        let b_sym = b.is_heap() && matches!(self.heap.get(b.heap_index()), HeapObj::Symbol { .. });
        if a_sym || b_sym {
            return Ok(false);
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
    pub(crate) fn bigint_loose_eq_other(&self, x: &BigVal, other: Value) -> bool {
        if other.is_bool() {
            return *x == BigVal::Small(if other.as_bool() { 1 } else { 0 });
        }
        if other.is_number() {
            // EXACT mathematical comparison — `x as f64` would round a large
            // i128/Big and wrongly equal a nearby Number.
            return x.cmp_f64(other.as_f64()) == Some(std::cmp::Ordering::Equal);
        }
        if other.is_heap() && self.heap.is_str_like(other.heap_index()) {
            if let Some(s) = self.heap.str_cow(other.heap_index()) {
                // StrWhiteSpace includes U+FEFF (BOM), which Rust's trim does not.
                let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
                if t.is_empty() {
                    return *x == BigVal::Small(0);
                }
                return parse_bigint_str(t).is_some_and(|y| y == *x);
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
    pub(crate) fn add_values(&mut self, va: Value, vb: Value) -> Result<Value, Thrown> {
        // Fast path: int + int with overflow check.
        if va.is_int() && vb.is_int() {
            return Ok(match va.as_int().checked_add(vb.as_int()) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + vb.as_int() as f64),
            });
        }
        // Fast path: string + string (the hot concat shape, incl. jit_concat) —
        // strings pass ToPrimitive unchanged, so skipping it is unobservable.
        if va.is_heap()
            && vb.is_heap()
            && self.heap.is_str_like(va.heap_index())
            && self.heap.is_str_like(vb.heap_index())
        {
            let (li, ri) = (va.heap_index(), vb.heap_index());
            let llen = self.heap.str_units(li).unwrap_or(0);
            let rlen = self.heap.str_units(ri).unwrap_or(0);
            // A SMALL result is built flat eagerly (one alloc, no flatten
            // later); only a large one pays for a rope node.
            if llen + rlen <= SMALL_CONCAT_FLAT_UNITS {
                return Ok(Value::heap(self.heap.alloc_concat_flat(li, ri, llen + rlen)));
            }
            return Ok(Value::heap(self.heap.alloc_cons(li, ri, llen + rlen)));
        }
        // Fast path: string + int (the `"key_" + i` map-key shape; both sides are
        // primitives, so skipping ToPrimitive is unobservable). A SMALL result is
        // built flat in ONE allocation with the int's decimal form written
        // straight into the buffer — no intermediate heap string for the number.
        if vb.is_int() && va.is_heap() {
            if let Some(lu) = self.heap.str_units(va.heap_index()) {
                let (buf, start) = fmt_i32_buf(vb.as_int());
                if lu + (buf.len() - start) <= SMALL_CONCAT_FLAT_UNITS {
                    if let Some(idx) =
                        self.heap.alloc_concat_str_ascii(va.heap_index(), &buf[start..])
                    {
                        return Ok(Value::heap(idx));
                    }
                }
            }
        }
        if va.is_int() && vb.is_heap() {
            if let Some(ru) = self.heap.str_units(vb.heap_index()) {
                let (buf, start) = fmt_i32_buf(va.as_int());
                if (buf.len() - start) + ru <= SMALL_CONCAT_FLAT_UNITS {
                    if let Some(idx) =
                        self.heap.alloc_concat_ascii_str(&buf[start..], vb.heap_index())
                    {
                        return Ok(Value::heap(idx));
                    }
                }
            }
        }
        // ToPrimitive(default hint) each operand IN ORDER — objects (including
        // boxed wrappers like `Object(1n)` / `new Number(5)`) run the
        // observable valueOf/toString/@@toPrimitive protocol; primitives pass
        // through unchanged.
        // GC INVARIANT: `pa` (possibly a fresh primitive from va's valueOf) is
        // held in a Rust local — unrooted — while vb's coercion runs user code.
        let _gc = self.gc_lock_guard();
        let pa = self.to_primitive_default(va)?;
        let pb = self.to_primitive_default(vb)?;
        let pa_str = pa.is_heap() && self.heap.is_str_like(pa.heap_index());
        let pb_str = pb.is_heap() && self.heap.is_str_like(pb.heap_index());
        if pa_str || pb_str {
            // String concatenation. ToString of a Symbol operand throws; a
            // BigInt stringifies (decimal, no "n") via to_str_idx.
            for v in [pa, pb] {
                if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
                    return Err(Thrown(
                        "TypeError: Cannot convert a Symbol value to a string".into(),
                    ));
                }
            }
            // Build a rope (cons-string) in O(1) — children point at existing
            // flat strings / ropes, so a `s += x` loop is O(n) overall.
            let li = self.to_str_idx(pa);
            let ri = self.to_str_idx(pb);
            let llen = self.heap.str_units(li).unwrap_or(0);
            let rlen = self.heap.str_units(ri).unwrap_or(0);
            // Small result → flat eagerly (see the string+string fast path).
            if llen + rlen <= SMALL_CONCAT_FLAT_UNITS {
                return Ok(Value::heap(self.heap.alloc_concat_flat(li, ri, llen + rlen)));
            }
            return Ok(Value::heap(self.heap.alloc_cons(li, ri, llen + rlen)));
        }
        // Numeric `+`: both BigInt → BigInt addition; BigInt + non-BigInt →
        // the spec's mixing TypeError; otherwise Number addition (ToNumber of
        // a Symbol throws there).
        let (ab, bb) = (self.bigint_val(pa), self.bigint_val(pb));
        match (ab, bb) {
            (Some(x), Some(y)) => self.bigint_op(crate::vm::helpers_misc::BigOp::Add, x, y),
            (None, None) => Ok(Value::num(self.to_number(pa)? + self.to_number(pb)?)),
            _ => Err(Thrown(BIGINT_MIX_ERR.into())),
        }
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
                    js.push_ascii(b'0' + n as u8);
                    return acc;
                }
            }
        }
        // General: materialise `val`'s EXACT (WTF-8) string form (same coercion
        // as `+`) — `push_wtf8` canonicalizes a high+low surrogate seam.
        let ri = self.to_str_idx(val);
        let add: Vec<u8> =
            self.heap.str_wtf8_cow(ri).map(|c| c.into_owned()).unwrap_or_default();
        if mutable {
            if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                js.push_wtf8(&add); // updates the cached unit length + ascii flag
                return acc;
            }
        }
        // Fresh buffer (first append / interned / rope acc): flatten acc + add into
        // a NON-interned `Str` (bypass `alloc_str`'s interning so it's mutable next).
        let li = self.to_str_idx(acc);
        let mut s: Vec<u8> =
            self.heap.str_wtf8_cow(li).map(|c| c.into_owned()).unwrap_or_default();
        crate::heap::wtf8_push(&mut s, &add);
        Value::heap(self.heap.alloc(HeapObj::Str(crate::heap::JsStr::from_wtf8(s))))
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
    pub(crate) fn cmp_lt(&mut self, base: usize, a: u16, b: u16, left_first: bool) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() < vb.as_int());
        }
        // Abstract relational comparison: ToPrimitive (number hint) both operands,
        // then compare two strings lexicographically, else numerically. The SOURCE
        // left operand must be coerced first: `<` passes (a,b) with left_first=true;
        // `>` passes the registers SWAPPED (b,a) with left_first=false, so the
        // original left operand `b` is ToPrimitive'd before `a` (spec LeftFirst).
        let (va, vb) = if left_first {
            let pa = self.to_primitive_default(va)?;
            let pb = self.to_primitive_default(vb)?;
            (pa, pb)
        } else {
            let pb = self.to_primitive_default(vb)?;
            let pa = self.to_primitive_default(va)?;
            (pa, pb)
        };
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_lt());
        }
        if let Some(ord) = self.bigint_relational(va, vb)? {
            return Ok(matches!(ord, Some(std::cmp::Ordering::Less)));
        }
        Ok(self.to_number(va)? < self.to_number(vb)?)
    }
    #[inline]
    pub(crate) fn cmp_le(&mut self, base: usize, a: u16, b: u16, left_first: bool) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() <= vb.as_int());
        }
        // ToPrimitive the SOURCE left operand first (see cmp_lt). `>=` swaps the
        // registers (b,a) with left_first=false so `b` coerces before `a`.
        let (va, vb) = if left_first {
            let pa = self.to_primitive_default(va)?;
            let pb = self.to_primitive_default(vb)?;
            (pa, pb)
        } else {
            let pb = self.to_primitive_default(vb)?;
            let pa = self.to_primitive_default(va)?;
            (pa, pb)
        };
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_le());
        }
        if let Some(ord) = self.bigint_relational(va, vb)? {
            return Ok(matches!(
                ord,
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            ));
        }
        Ok(self.to_number(va)? <= self.to_number(vb)?)
    }

    /// Abstract relational comparison when at least one operand is a BigInt. A
    /// BigInt must be compared by its exact mathematical value — `to_number` would
    /// round it to an f64 (so `10000000000000000000n` and `9999999999999999999n`
    /// wrongly compare equal). Returns `Ok(None)` when neither operand is a BigInt
    /// (the caller falls back to numeric comparison), `Ok(Some(None))` when the
    /// pair is unordered (the other side is NaN), else `Ok(Some(Some(ordering)))`.
    fn bigint_relational(
        &self,
        va: Value,
        vb: Value,
    ) -> Result<Option<Option<std::cmp::Ordering>>, Thrown> {
        let ab = self.bigint_val(va);
        let bb = self.bigint_val(vb);
        if ab.is_none() && bb.is_none() {
            return Ok(None);
        }
        let ord = match (ab, bb) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            (Some(x), None) => self.cmp_bigint_other(&x, vb)?,
            (None, Some(y)) => self.cmp_bigint_other(&y, va)?.map(|o| o.reverse()),
            (None, None) => unreachable!(),
        };
        Ok(Some(ord))
    }

    /// Relational comparison of a BigInt `x` against a non-BigInt operand. A
    /// STRING is StringToBigInt'd (EXACT — `"9007199254740993"` is not rounded
    /// through f64; whitespace-only → 0n; a non-integer string → undefined, i.e.
    /// unordered so every relational result is false). A Number/Boolean compares
    /// by mathematical value.
    fn cmp_bigint_other(&self, x: &BigVal, other: Value) -> Result<Option<std::cmp::Ordering>, Thrown> {
        if other.is_heap() && self.heap.is_str_like(other.heap_index()) {
            if let Some(s) = self.heap.str_cow(other.heap_index()) {
                let y =
                    if s.trim().is_empty() { Some(BigVal::Small(0)) } else { parse_bigint_str(&s) };
                return Ok(y.map(|y| x.cmp(&y)));
            }
        }
        Ok(x.cmp_f64(self.to_number(other)?))
    }

    /// JS relational comparison of two STRING operands is lexicographic by UTF-16
    /// CODE UNIT — not numeric, and not by Unicode code point. Returns the
    /// `Ordering` when both are string-like, else `None` (the caller falls back to
    /// numeric comparison).
    pub(crate) fn str_relational(&self, va: Value, vb: Value) -> Option<std::cmp::Ordering> {
        if va.is_heap()
            && vb.is_heap()
            && self.heap.is_str_like(va.heap_index())
            && self.heap.is_str_like(vb.heap_index())
        {
            let sa = self.heap.str_wtf8_cow(va.heap_index())?;
            let sb = self.heap.str_wtf8_cow(vb.heap_index())?;
            let (a, b) = (sa.as_ref(), sb.as_ref());
            // Fast path: ASCII byte order == UTF-16 code-unit order (and
            // `is_ascii` is vectorised), so the hot common case stays a byte cmp.
            if a.is_ascii() && b.is_ascii() {
                return Some(a.cmp(b));
            }
            // Non-ASCII: an astral (>BMP) char is a UTF-16 surrogate pair
            // (0xD800–0xDBFF) that sorts BELOW the 0xE000–0xFFFF BMP range, so a
            // byte (code-point-order) cmp is wrong. Compare by UTF-16 code units
            // over the WTF-8 bytes — a LONE surrogate orders by its own unit
            // value, an astral scalar by its lead surrogate first (the
            // `wtf8_units_iter` decode), exactly the spec's unit order.
            return Some(crate::heap::wtf8_units_iter(a).cmp(crate::heap::wtf8_units_iter(b)));
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
            // Canonical form: a Small (i128) never equals a Big (beyond-i128).
            match (self.heap.get(ai), self.heap.get(bi)) {
                (HeapObj::BigInt(x), HeapObj::BigInt(y)) => return x == y,
                (HeapObj::BigIntBig(x), HeapObj::BigIntBig(y)) => return x == y,
                _ => {}
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
        // An [[IsHTMLDDA]] exotic (`document.all`) is falsy.
        if !self.is_htmldda.is_empty() && self.is_htmldda.contains(&v.heap_index()) {
            return false;
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
        // arithmetic mixing is rejected earlier by `numeric_binop`).
        match self.heap.get(v.heap_index()) {
            HeapObj::BigInt(n) => return Ok(*n as f64),
            HeapObj::BigIntBig(b) => {
                use num_traits::ToPrimitive;
                // Correctly rounded; beyond-f64 magnitudes become ±Infinity.
                return Ok(b.to_f64().unwrap_or(f64::NAN));
            }
            _ => {}
        }
        if let Some(s) = self.heap.str_cow(v.heap_index()) {
            // StrWhiteSpace includes U+FEFF (BOM), which Rust's trim does not.
                let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
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
            // The only Infinity spellings a StringNumericLiteral accepts are the
            // exact `Infinity` / `+Infinity` / `-Infinity` (capital I, full word).
            match t {
                "Infinity" | "+Infinity" => return Ok(f64::INFINITY),
                "-Infinity" => return Ok(f64::NEG_INFINITY),
                _ => {}
            }
            // Rust's f64 parser also accepts word forms (`inf`, `infinity`, `INF`,
            // `nan`, …) that are NOT valid JS numeric strings. A valid JS decimal /
            // scientific literal begins (after an optional sign) with a digit or `.`,
            // never a letter — so reject a letter-led body (`Infinity` was handled
            // above). A decimal that overflows (`1e400`) still parses to Infinity.
            let body = t.strip_prefix(['+', '-']).unwrap_or(t);
            if body.as_bytes().first().is_some_and(|b| b.is_ascii_alphabetic()) {
                return Ok(f64::NAN);
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

    /// `ToIntegerOrInfinity(v)` like `to_integer_or_zero`, but via the STRICT
    /// ToNumber (a BigInt or Symbol argument is a TypeError, per spec) - for
    /// String.prototype position/count arguments.
    pub(crate) fn to_integer_strict(&mut self, v: Value) -> Result<i64, Thrown> {
        let n = self.to_number_strict(v)?;
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

    /// ToIndex(v) (ES 7.1.22): ToIntegerOrInfinity, then a RangeError if the
    /// result is negative or exceeds 2^53-1. `undefined` → 0. Backs the
    /// byteOffset/length arguments of the TypedArray/DataView constructors, so
    /// `new Int8Array(buf, -1)` throws rather than silently clamping to 0 (a bare
    /// `as usize` cast saturates a negative float to 0).
    pub(crate) fn to_index(&mut self, v: Value) -> Result<usize, Thrown> {
        // ToIndex = ToIntegerOrInfinity(value) clamped to 0..=2^53-1. The integer
        // conversion goes through ToNumber, so a BigInt or Symbol is a TypeError
        // (not silently coerced); `undefined` is 0 and NaN truncates to 0.
        if v == Value::UNDEFINED {
            return Ok(0);
        }
        let num = self.to_number_strict(v)?;
        let n = if num.is_nan() { 0.0 } else { num.trunc() };
        if n < 0.0 || n > ((1i128 << 53) - 1) as f64 {
            return Err(Thrown("RangeError: index is out of range".into()));
        }
        Ok(n as usize)
    }

    pub(crate) fn to_number_coerce(&mut self, v: Value) -> Result<f64, Thrown> {
        if !v.is_heap() {
            return self.to_number(v);
        }
        if matches!(
            self.heap.get(v.heap_index()),
            HeapObj::Date(_)
                | HeapObj::Symbol { .. }
                | HeapObj::BigInt(_)
                | HeapObj::BigIntBig(_)
                | HeapObj::Str(_)
                | HeapObj::Cons { .. }
        ) {
            return self.to_number(v);
        }
        // A boxed wrapper (and any ordinary object) ToPrimitive(number) first, so an
        // overridden valueOf/toString fires; a plain wrapper still yields its [[xData]].
        let prim = self.to_primitive_number(v)?;
        self.to_number(prim)
    }

    /// ToNumber with FULL strictness — like `to_number_coerce` (ToPrimitive on an
    /// object, honouring valueOf/@@toPrimitive and propagating an abrupt), but a
    /// BigInt OR Symbol primitive is a TypeError. The shared `to_number` is
    /// deliberately lenient on BigInt (so `1n < 2` relational comparison works), so
    /// the String code-unit/code-point statics (`fromCharCode`/`fromCodePoint`) use
    /// this instead to reject BigInt/Symbol per spec.
    pub(crate) fn to_number_strict(&mut self, v: Value) -> Result<f64, Thrown> {
        // Objects (incl. boxed wrappers) ToPrimitive(number hint) first; an
        // already-primitive value passes through unchanged.
        let prim = if v.is_heap()
            && !matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) | HeapObj::Symbol { .. }
            ) {
            self.to_primitive_number(v)?
        } else {
            v
        };
        if prim.is_heap() {
            match self.heap.get(prim.heap_index()) {
                HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => {
                    return Err(Thrown("TypeError: Cannot convert a BigInt value to a number".into()));
                }
                HeapObj::Symbol { .. } => {
                    return Err(Thrown("TypeError: Cannot convert a Symbol value to a number".into()));
                }
                _ => {}
            }
        }
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
                    | HeapObj::BigIntBig(_)
                    | HeapObj::Symbol { .. }
            )
        {
            return Ok(None);
        }
        // A Boxed SYMBOL falls through to the real GetMethod lookup: it finds
        // Symbol.prototype[@@toPrimitive] (returning the wrapped symbol, so
        // ToString(Object(sym)) THROWS) — unless user code deleted/redefined
        // the hook, in which case ordinary toString/valueOf semantics apply.
        // Other Boxed kinds have no @@toPrimitive on their chains - no hook.
        if let HeapObj::Boxed { kind, .. } = self.heap.get(v.heap_index()) {
            if *kind != 3 {
                return Ok(None);
            }
        }
        // GetMethod(v, @@toPrimitive): undefined/null → no hook (None); a
        // present-but-not-callable @@toPrimitive is a TypeError, not a fallthrough.
        let f = self.get_prop(v, "@@toPrimitive")?;
        if f == Value::UNDEFINED || f == Value::NULL {
            return Ok(None);
        }
        if !self.is_callable(f) {
            return Err(Thrown("TypeError: Symbol.toPrimitive is not a function".into()));
        }
        let hv = self.alloc_str(hint.to_string());
        let r = self.call_value(f, v, &[hv])?;
        let is_obj = r.is_heap()
            && !matches!(
                self.heap.get(r.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) | HeapObj::Symbol { .. }
            );
        if is_obj {
            return Err(Thrown("TypeError: Cannot convert object to primitive value".into()));
        }
        Ok(Some(r))
    }

    /// ToPrimitive(v, "number"): the `@@toPrimitive` hook, else OrdinaryToPrimitive
    /// (`valueOf` then `toString`, first primitive wins; TypeError if neither does).
    pub(crate) fn to_primitive_number(&mut self, v: Value) -> Result<Value, Thrown> {
        // A boxed primitive wrapper runs OrdinaryToPrimitive (valueOf/toString) like
        // any object: a PLAIN wrapper's built-in valueOf returns its [[xData]], but an
        // OVERRIDDEN valueOf/toString/@@toPrimitive must be honoured (`+new Number(1)`
        // with a custom valueOf).
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
                        HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) | HeapObj::Symbol { .. }
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
            HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) | HeapObj::Symbol { .. }
        ) {
            return Ok(v);
        }
        // A boxed primitive wrapper runs OrdinaryToPrimitive (valueOf/toString) — a
        // plain wrapper's built-in valueOf returns its [[xData]], but an overridden
        // valueOf/toString/@@toPrimitive is honoured (`new Number(1) + 0` with a
        // custom valueOf, `==` on a wrapper, …).
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
                        HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) | HeapObj::Symbol { .. }
                    );
                if is_primitive {
                    return Ok(r);
                }
            }
        }
        Err(Thrown("TypeError: Cannot convert object to primitive value".into()))
    }

    /// OrdinaryToPrimitive(O, methodNames) (ES 7.1.1.1): try each method name in
    /// `order` — `["valueOf","toString"]` for hint "number", `["toString","valueOf"]`
    /// for hint "string" — and return the first primitive result; TypeError if none.
    /// Used by `Date.prototype[Symbol.toPrimitive]`.
    pub(crate) fn ordinary_to_primitive(
        &mut self,
        v: Value,
        order: [&str; 2],
    ) -> Result<Value, Thrown> {
        for name in order {
            let f = self.get_prop(v, name)?;
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                let is_primitive = !r.is_heap()
                    || matches!(
                        self.heap.get(r.heap_index()),
                        HeapObj::Str(_)
                            | HeapObj::Cons { .. }
                            | HeapObj::BigInt(_)
                            | HeapObj::BigIntBig(_)
                            | HeapObj::Symbol { .. }
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
                    // Duration fields store f64 BITS in the i64 slots.
                    let mut f = [0f64; 10];
                    for (i, s) in f.iter_mut().enumerate() {
                        *s = f64::from_bits(*fields.get(i).unwrap_or(&0) as u64);
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
                // Display is a LOSSY observation: a lone surrogate reads U+FFFD.
                HeapObj::Str(s) => s.to_lossy_string(),
                HeapObj::Cons { .. } => {
                    self.heap.str_cow(v.heap_index()).map(|c| c.into_owned()).unwrap_or_default()
                }
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) => {
                    "function".into()
                }
                HeapObj::Cell(inner) => self.display(*inner),
                HeapObj::EvalScope(_) => "[object EvalScope]".into(),
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
                HeapObj::BigIntBig(b) => b.to_string(),
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
                // `String(date)` / `"" + date` → ToPrimitive(string) → toString(),
                // which is the human form (NOT the ISO toISOString used by toJSON).
                HeapObj::Date(ms) => date_to_string(*ms),
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
                // Top-level strings unquoted; LOSSY (console rendering).
                HeapObj::Str(s) => return s.to_lossy_string(),
                HeapObj::Cons { .. } => {
                    return self
                        .heap
                        .str_cow(v.heap_index())
                        .map(|c| c.into_owned())
                        .unwrap_or_default();
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
                // Duration fields store f64 BITS in the i64 slots.
                let mut f = [0f64; 10];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = f64::from_bits(*fields.get(i).unwrap_or(&0) as u64);
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
            HeapObj::Str(s) => format!("'{}'", s.as_str_lossy()),
            HeapObj::Cons { .. } => {
                let out =
                    self.heap.str_cow(v.heap_index()).map(|c| c.into_owned()).unwrap_or_default();
                format!("'{out}'")
            }
            HeapObj::Func(id) => self.func_label(*id),
            HeapObj::Closure { func, .. } => self.func_label(*func),
            HeapObj::Bound { .. } => "[Function: bound]".into(),
            HeapObj::Wrapped { name, .. } => {
                if name.is_empty() {
                    "[Function (anonymous)]".into()
                } else {
                    format!("[Function: {name}]")
                }
            }
            HeapObj::Native(_) => "[Function (native)]".into(),
            HeapObj::Cell(inner) => self.inspect_nested(*inner),
            HeapObj::EvalScope(_) => "[object EvalScope]".into(),
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
            HeapObj::BigIntBig(b) => format!("{b}n"),
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
    /// A `wtf8_consts`-flagged slot holds the oxc lone-surrogate MARKER form —
    /// decoded here into a real WTF-8 string (this is where `'\uD800'` becomes
    /// a 1-unit lone-surrogate string).
    #[inline]
    pub(crate) fn resolve_const(&mut self, func_id: u32, v: Value) -> Value {
        // String constants are encoded as `Value::heap(STRING_CONST_BIT | i)`.
        if v.is_heap() && (v.heap_index() & STRING_CONST_BIT) != 0 {
            let si = (v.heap_index() & !STRING_CONST_BIT) as usize;
            let f = self.func(func_id as usize);
            if !f.wtf8_consts.is_empty() && f.wtf8_consts.binary_search(&(si as u32)).is_ok() {
                let bytes = crate::heap::decode_lone_surrogate_markers(&f.string_constants[si]);
                let js = crate::heap::JsStr::from_wtf8(bytes);
                return Value::heap(self.heap.alloc_js(js));
            }
            let s = f.string_constants[si].clone();
            return self.alloc_str(s);
        }
        v
    }
}

/// Exact ordering of an i128 (a BigInt) against an f64, comparing mathematical
/// values without rounding the integer. `None` means unordered (the f64 is NaN).
pub(crate) fn cmp_i128_f64(x: i128, y: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if y.is_nan() {
        return None;
    }
    if y == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if y == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    // 2^127 — any finite f64 at or beyond this magnitude is outside i128's range.
    const TWO_127: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;
    if y >= TWO_127 {
        return Some(Ordering::Less);
    }
    if y < -TWO_127 {
        return Some(Ordering::Greater);
    }
    // y is finite and in (-2^127, 2^127): floor(y) is an integer-valued f64 that
    // converts to i128 exactly. Compare the integer parts, then break a tie by the
    // fractional part of y.
    let yf = y.floor();
    let yi = yf as i128;
    Some(match x.cmp(&yi) {
        Ordering::Equal if y > yf => Ordering::Less, // x == floor(y) < y
        ord => ord,
    })
}

#[cfg(test)]
mod cmp_i128_f64_tests {
    use super::cmp_i128_f64;
    use std::cmp::Ordering::*;

    #[test]
    fn exact_against_numbers() {
        assert_eq!(cmp_i128_f64(2, 1.5), Some(Greater));
        assert_eq!(cmp_i128_f64(1, 1.5), Some(Less));
        assert_eq!(cmp_i128_f64(5, 5.0), Some(Equal));
        assert_eq!(cmp_i128_f64(-2, -1.5), Some(Less));
        assert_eq!(cmp_i128_f64(-1, -1.5), Some(Greater));
        // Beyond 2^53 the f64 can't hold the integer exactly, but the compare must.
        assert_eq!(cmp_i128_f64(9_007_199_254_740_993, 9_007_199_254_740_992.0), Some(Greater));
        assert_eq!(cmp_i128_f64(10_000_000_000_000_001, 1e16), Some(Greater));
        // NaN / infinities.
        assert_eq!(cmp_i128_f64(1, f64::NAN), None);
        assert_eq!(cmp_i128_f64(i128::MAX, f64::INFINITY), Some(Less));
        assert_eq!(cmp_i128_f64(i128::MIN, f64::NEG_INFINITY), Some(Greater));
        // A large in-range f64 (1e30 < 2^127) still compares exactly.
        assert_eq!(cmp_i128_f64(i128::MAX, 1e30), Some(Greater));
        assert_eq!(cmp_i128_f64(i128::MIN, -1e30), Some(Less));
        // Magnitudes beyond 2^127 saturate the comparison.
        assert_eq!(cmp_i128_f64(i128::MAX, 1e40), Some(Less));
        assert_eq!(cmp_i128_f64(i128::MIN, -1e40), Some(Greater));
    }
}
