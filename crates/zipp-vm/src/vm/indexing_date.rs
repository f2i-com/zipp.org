#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    pub(crate) fn get_index(&mut self, obj: Value, key: Value) -> Result<Value, Thrown> {
        // A rope must be materialized before random access; no-op (one tag
        // check) for arrays, objects, and already-flat strings.
        if obj.is_heap() {
            self.heap.flatten(obj.heap_index());
        }
        if !obj.is_heap() {
            // null/undefined throw; a number/boolean primitive resolves method-as-value
            // through its prototype (`(5)["toFixed"]`, `true["toString"]`).
            if obj.is_nullish() {
                return Err(Thrown(format!(
                    "TypeError: cannot read property of {}",
                    self.display(obj)
                )));
            }
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        // A boxed String indexes its wrapped string (chars / length); a boxed
        // Number/Boolean has no index, so computed access goes through the prototype.
        if let HeapObj::Boxed { kind, value } = self.heap.get(obj.heap_index()) {
            let (k, v) = (*kind, *value);
            if k == 0 {
                return self.get_index(v, key);
            }
            let key_s = self.key_of(key);
            return self.get_prop(obj, &key_s);
        }
        // Object / callable / class index access is property access: delegate to
        // `get_prop` so a computed key reaches inherited methods/getters (e.g. a
        // class instance's `obj[Symbol.iterator]`), a callable's `fn["name"]`, and
        // static members (`C["m"]`) — not just own data properties. The built-in
        // instance types (Date/Promise/Weak*) have no integer-index meaning, so all
        // their computed access delegates here too.
        // A TypedArray: a canonical numeric index reads the element; everything
        // else (length/byteLength/methods) delegates to get_prop.
        if matches!(self.heap.get(obj.heap_index()), HeapObj::TypedArray { .. }) {
            if let Some(i) = array_index(key) {
                return Ok(self.ta_element_get(obj.heap_index(), i));
            }
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        if matches!(
            self.heap.get(obj.heap_index()),
            HeapObj::Object(_)
                | HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Bound { .. }
                | HeapObj::Native(_)
                | HeapObj::Iterator { .. }
                | HeapObj::Date(_)
                | HeapObj::Promise { .. }
                | HeapObj::WeakMap { .. }
                | HeapObj::WeakSet(_)
                | HeapObj::WeakRef(_)
                | HeapObj::FinalizationRegistry { .. }
                | HeapObj::RegExp { .. }
                | HeapObj::Symbol { .. }
                | HeapObj::BigInt(_)
                | HeapObj::ArrayBuffer { .. }
                | HeapObj::DataView { .. }
                | HeapObj::Proxy { .. }
        ) {
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                // Numeric key (incl. an integral double like 1.0 — the JIT region
                // produces f64 indices): direct element access, else undefined.
                if let Some(i) = array_index(key) {
                    if i < items.len() {
                        return Ok(items[i]);
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-int key on an array: "length", else resolve via the prototype
                // (a computed method name / `@@iterator`, mirroring dot access).
                let k = self.key_of(key);
                if k == "length" {
                    return Ok(len_value(items.len()));
                }
                self.get_prop(obj, &k)
            }
            HeapObj::Object(map) => {
                let k = self.key_of(key);
                Ok(map.get(&k).unwrap_or(Value::UNDEFINED))
            }
            HeapObj::Str(s) => {
                // Numeric key (incl. an integral double — a JIT region produces
                // f64 indices, and a deopted string index must agree): char at i.
                if let Some(i) = array_index(key) {
                    // A single ASCII char is interned at heap index == its byte
                    // (see Heap::new), so return that slot DIRECTLY — no temporary
                    // 1-char String + re-intern per access (that alloc dominated
                    // `s[i]` scans). O(1) for ASCII (i-th char == i-th byte); a
                    // multi-byte string walks scalars (O(i), correct).
                    if s.ascii {
                        return Ok(match s.bytes.as_bytes().get(i) {
                            Some(&b) => Value::heap(b as u32),
                            None => Value::UNDEFINED,
                        });
                    }
                    match s.bytes.chars().nth(i) {
                        Some(ch) if (ch as u32) < 128 => return Ok(Value::heap(ch as u32)),
                        Some(ch) => {
                            let cs = ch.to_string();
                            return Ok(self.alloc_str(cs));
                        }
                        None => return Ok(Value::UNDEFINED),
                    }
                }
                // Non-numeric key: `s["length"]`, else resolve via String.prototype
                // (a computed method name / `@@iterator`), mirroring dot access.
                let char_len = s.char_len;
                let k = self.key_of(key);
                if k == "length" {
                    return Ok(len_value(char_len));
                }
                self.get_prop(obj, &k)
            }
            // Positional access drives for-of / spread over a Map (the i-th
            // [key, value] entry) and a Set (the i-th value). Insertion order.
            HeapObj::Map { keys, vals } => {
                if let Some(i) = array_index(key) {
                    if i < keys.len() {
                        let (k, v) = (keys[i], vals[i]);
                        return Ok(Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))));
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-numeric key (`map[Symbol.iterator]`, `map["set"]`): via prototype.
                let k = self.key_of(key);
                self.get_prop(obj, &k)
            }
            HeapObj::Set(items) => {
                if let Some(i) = array_index(key) {
                    if i < items.len() {
                        return Ok(items[i]);
                    }
                    return Ok(Value::UNDEFINED);
                }
                let k = self.key_of(key);
                self.get_prop(obj, &k)
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    pub(crate) fn set_index(&mut self, obj: Value, key: Value, val: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: cannot set property of non-object".into()));
        }
        let idx = obj.heap_index();
        // A TypedArray: a canonical numeric index writes the element (coerced +
        // out-of-bounds is a silent no-op); other keys go to set_prop.
        if matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            if let Some(i) = array_index(key) {
                return self.ta_element_set(idx, i, val);
            }
            let k = self.key_of(key);
            return self.set_prop(obj, &k, val);
        }
        // Callable / class computed assignment (`fn["x"] = v`, `C["s"] = v`) is
        // property assignment: route through `set_prop` (honours non-writable
        // `name`/`length`, static setters, function own props).
        if matches!(
            self.heap.get(idx),
            HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Bound { .. }
                | HeapObj::Native(_)
                | HeapObj::Proxy { .. }
        ) {
            let k = self.key_of(key);
            return self.set_prop(obj, &k, val);
        }
        match self.heap.get_mut(idx) {
            HeapObj::Array(items) => {
                // Numeric key (incl. an integral double — the JIT region produces
                // f64 indices): store, growing with `undefined` holes past the end.
                if let Some(i) = array_index(key) {
                    if i >= items.len() {
                        items.resize(i + 1, Value::UNDEFINED);
                    }
                    items[i] = val;
                }
                // Non-numeric / negative / fractional key: no-op in this subset.
                Ok(())
            }
            HeapObj::Object(_) => {
                // Route through set_prop so bracket assignment honours property
                // attributes (writable / accessors / extensibility) exactly like
                // dot assignment — propertyHelper.js's writability probe uses
                // `obj[key] = v`, so a raw map.set silently reported every
                // non-writable property as writable.
                let k = self.key_of(key);
                self.set_prop(obj, &k, val)
            }
            _ => Ok(()),
        }
    }

    /// `new Date(...)` → epoch ms. 0 args = now; 1 number = ms (time-clipped);
    /// 1 Date = copy; 1 string = parsed; ≥2 = UTC components (month0-based).
    pub(crate) fn date_new_ms(&self, args: &[Value]) -> Result<f64, Thrown> {
        match args.len() {
            0 => Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as f64)
                .unwrap_or(0.0)),
            1 => {
                let a = args[0];
                if a.is_heap() {
                    if let HeapObj::Date(ms) = self.heap.get(a.heap_index()) {
                        return Ok(*ms);
                    }
                    if matches!(self.heap.get(a.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. }) {
                        let s = self.heap.str_cow(a.heap_index()).unwrap().into_owned();
                        return Ok(parse_date(&s));
                    }
                }
                Ok(time_clip(self.to_number(a)?))
            }
            _ => {
                let mut comp = [0i64, 0, 1, 0, 0, 0, 0]; // y, mo0, day, h, mi, s, ms
                for (i, &v) in args.iter().enumerate().take(7) {
                    let n = self.to_number(v)?;
                    if n.is_nan() {
                        return Ok(f64::NAN);
                    }
                    comp[i] = n as i64;
                }
                comp[0] = legacy_year(comp[0]);
                Ok(time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6])))
            }
        }
    }

    /// `Date.UTC(year, month0, …)` → epoch ms (NaN with no args / a NaN field).
    pub(crate) fn date_utc_ms(&self, args: &[Value]) -> Result<f64, Thrown> {
        if args.is_empty() {
            return Ok(f64::NAN);
        }
        let mut comp = [0i64, 0, 1, 0, 0, 0, 0];
        for (i, &v) in args.iter().enumerate().take(7) {
            let n = self.to_number(v)?;
            if n.is_nan() {
                return Ok(f64::NAN);
            }
            comp[i] = n as i64;
        }
        comp[0] = legacy_year(comp[0]);
        Ok(time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6])))
    }

    /// Dispatch a method on a `Date` receiver (`idx` is its heap index). All
    /// getters/setters are UTC. Returns `Ok(None)` if `name` isn't a Date method.
    pub(crate) fn date_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let ms = match self.heap.get(idx) {
            HeapObj::Date(m) => *m,
            // thisTimeValue brand check: every Date.prototype method requires a
            // Date receiver (reached here only via the method-as-value path with a
            // non-Date `this`; the direct dispatch already matched HeapObj::Date).
            _ => return Err(Thrown("TypeError: this is not a Date object".into())),
        };
        let p = date_parts(ms); // (year, month0, day, hour, min, sec, ms, weekday)
        let field = |v: i64| if ms.is_nan() { Value::num(f64::NAN) } else { Value::num(v as f64) };
        let r = match name {
            "getTime" | "valueOf" => Value::num(ms),
            "getFullYear" | "getUTCFullYear" => field(p.0),
            "getMonth" | "getUTCMonth" => field(p.1),
            "getDate" | "getUTCDate" => field(p.2),
            "getHours" | "getUTCHours" => field(p.3),
            "getMinutes" | "getUTCMinutes" => field(p.4),
            "getSeconds" | "getUTCSeconds" => field(p.5),
            "getMilliseconds" | "getUTCMilliseconds" => field(p.6),
            "getDay" | "getUTCDay" => field(p.7),
            "getTimezoneOffset" => Value::num(if ms.is_nan() { f64::NAN } else { 0.0 }),
            "toISOString" => {
                if ms.is_nan() {
                    return Err(Thrown("RangeError: Invalid time value".into()));
                }
                self.alloc_str(date_to_iso(ms))
            }
            "toJSON" => {
                if ms.is_nan() {
                    Value::NULL
                } else {
                    self.alloc_str(date_to_iso(ms))
                }
            }
            // Simplified: ISO (node's local/tz-formatted strings aren't matched).
            // toGMTString is a legacy (Annex B) alias of toUTCString.
            "toString" | "toUTCString" | "toGMTString" | "toDateString" | "toTimeString"
            | "toLocaleString" | "toLocaleDateString" | "toLocaleTimeString" => {
                if ms.is_nan() {
                    self.alloc_str("Invalid Date".to_string())
                } else {
                    self.alloc_str(date_to_iso(ms))
                }
            }
            // Legacy (Annex B): getYear = full year - 1900; setYear maps 0..99 to 19xx.
            "getYear" => field(p.0 - 1900),
            "setYear" => {
                let y = match args.first() {
                    Some(&v) => self.to_number(v)?,
                    None => f64::NAN,
                };
                if y.is_nan() {
                    if let HeapObj::Date(m) = self.heap.get_mut(idx) {
                        *m = f64::NAN;
                    }
                    Value::num(f64::NAN)
                } else {
                    let yi = y as i64;
                    let full = if (0..=99).contains(&yi) { 1900 + yi } else { yi };
                    self.date_set(idx, &p, &[Value::num(full as f64)], 0)?
                }
            }
            "setTime" => {
                let n = match args.first() {
                    Some(&v) => time_clip(self.to_number(v)?),
                    None => f64::NAN,
                };
                if let HeapObj::Date(m) = self.heap.get_mut(idx) {
                    *m = n;
                }
                Value::num(n)
            }
            "setFullYear" | "setUTCFullYear" => self.date_set(idx, &p, args, 0)?,
            "setMonth" | "setUTCMonth" => self.date_set(idx, &p, args, 1)?,
            "setDate" | "setUTCDate" => self.date_set(idx, &p, args, 2)?,
            "setHours" | "setUTCHours" => self.date_set(idx, &p, args, 3)?,
            "setMinutes" | "setUTCMinutes" => self.date_set(idx, &p, args, 4)?,
            "setSeconds" | "setUTCSeconds" => self.date_set(idx, &p, args, 5)?,
            "setMilliseconds" | "setUTCMilliseconds" => self.date_set(idx, &p, args, 6)?,
            _ => return Ok(None),
        };
        Ok(Some(r))
    }

    /// A Date setter starting at component `start` (0=year … 6=ms): overwrite that
    /// field and the following ones from `args`, recompute, store, return the new ms.
    pub(crate) fn date_set(
        &mut self,
        idx: u32,
        p: &(i64, i64, i64, i64, i64, i64, i64, i64),
        args: &[Value],
        start: usize,
    ) -> Result<Value, Thrown> {
        let mut comp = [p.0, p.1, p.2, p.3, p.4, p.5, p.6];
        let mut any_nan = false;
        for (i, &v) in args.iter().enumerate() {
            if start + i >= 7 {
                break;
            }
            let n = self.to_number(v)?;
            if n.is_nan() {
                any_nan = true;
            }
            comp[start + i] = n as i64;
        }
        let ms = if any_nan {
            f64::NAN
        } else {
            time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6]))
        };
        if let HeapObj::Date(m) = self.heap.get_mut(idx) {
            *m = ms;
        }
        Ok(Value::num(ms))
    }

}
