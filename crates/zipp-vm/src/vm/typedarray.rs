#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// Practical upper bound on an ArrayBuffer/TypedArray byte length. A larger
/// request is a RangeError rather than an attempted (process-aborting) alloc.
pub(crate) const MAX_ARRAY_BUFFER_LEN: i64 = 0x7FFF_FFFF;

impl<'p> Vm<'p> {
    pub(crate) fn as_array_buffer(&self, v: Value) -> Option<u32> {
        (v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::ArrayBuffer { .. }))
            .then(|| v.heap_index())
    }
    pub(crate) fn as_typed_array(&self, v: Value) -> Option<u32> {
        (v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::TypedArray { .. }))
            .then(|| v.heap_index())
    }
    pub(crate) fn array_buffer_len(&self, idx: u32) -> usize {
        match self.heap.get(idx) {
            HeapObj::ArrayBuffer { data, .. } => data.len(),
            _ => 0,
        }
    }
    /// Effective element length of a TypedArray view, accounting for a resizable
    /// backing buffer. `None` means the view is out of bounds — detached, its
    /// offset is past the (shrunk) buffer, or a fixed-length view no longer fits;
    /// methods then treat it like a detached buffer. A length-tracking view's
    /// length follows the buffer. For a non-resizable buffer this returns the
    /// fixed length unchanged (the ta_tracking set is empty in the common case).
    pub(crate) fn ta_effective_len(&self, ta_idx: u32) -> Option<usize> {
        let (buffer, kind, byte_offset, length) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray { buffer, kind, byte_offset, length } => {
                (*buffer, *kind, *byte_offset, *length)
            }
            _ => return None,
        };
        let buf_len = match self.heap.get(buffer) {
            HeapObj::ArrayBuffer { data, detached } if !*detached => data.len(),
            _ => return None,
        };
        let size = native::TA_KINDS[kind as usize].1;
        if self.ta_tracking.contains(&ta_idx) {
            if byte_offset > buf_len {
                return None;
            }
            Some((buf_len - byte_offset) / size)
        } else {
            if byte_offset.checked_add(length.checked_mul(size)?)? > buf_len {
                return None;
            }
            Some(length)
        }
    }
    pub(crate) fn alloc_array_buffer(&mut self, byte_len: usize) -> u32 {
        let idx = self.heap.alloc(HeapObj::ArrayBuffer { data: vec![0u8; byte_len], detached: false });
        if self.arraybuffer_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.arraybuffer_proto));
        }
        idx
    }
    /// Allocate a TypedArray view over `buffer`, linked to that kind's prototype.
    pub(crate) fn alloc_typed_array(&mut self, buffer: u32, kind: u8, byte_offset: usize, length: usize) -> Value {
        let idx = self.heap.alloc(HeapObj::TypedArray { buffer, kind, byte_offset, length });
        let p = self.ta_protos[kind as usize];
        if p != 0 {
            self.proto_of.insert(idx, Value::heap(p));
        }
        Value::heap(idx)
    }

    /// True if `v` is a (primitive) String value.
    pub(crate) fn is_string_value(&self, v: Value) -> bool {
        v.is_heap()
            && matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. }
            )
    }

    /// Read the base64-DECODE options object (fromBase64/setFromBase64): returns
    /// (base64url?, lastChunkHandling) where lch is 0=loose, 1=strict,
    /// 2=stop-before-partial. `alphabet` is Get first, then `lastChunkHandling`
    /// (the observable order); each must be undefined or an exact allowed string
    /// (no ToString coercion), else TypeError.
    pub(crate) fn read_b64_decode_opts(&mut self, opts: Value) -> Result<(bool, u8), Thrown> {
        if opts == Value::UNDEFINED {
            return Ok((false, 0));
        }
        if !self.is_object_value(opts) {
            return Err(Thrown("TypeError: base64 options must be an object".into()));
        }
        let a = self.get_prop(opts, "alphabet")?;
        let url = if a == Value::UNDEFINED {
            false
        } else if self.is_string_value(a) {
            match self.to_js_string(a)?.as_str() {
                "base64" => false,
                "base64url" => true,
                _ => {
                    return Err(Thrown(
                        "TypeError: base64 alphabet must be \"base64\" or \"base64url\"".into(),
                    ))
                }
            }
        } else {
            return Err(Thrown("TypeError: base64 alphabet must be a string".into()));
        };
        let l = self.get_prop(opts, "lastChunkHandling")?;
        let lch = if l == Value::UNDEFINED {
            0u8
        } else if self.is_string_value(l) {
            match self.to_js_string(l)?.as_str() {
                "loose" => 0,
                "strict" => 1,
                "stop-before-partial" => 2,
                _ => {
                    return Err(Thrown(
                        "TypeError: lastChunkHandling must be \"loose\", \"strict\", or \"stop-before-partial\"".into(),
                    ))
                }
            }
        } else {
            return Err(Thrown("TypeError: lastChunkHandling must be a string".into()));
        };
        Ok((url, lch))
    }

    /// Validate a `Uint8Array` receiver (the base64/hex methods are Uint8Array-only,
    /// even via `.call`), returning its heap index.
    pub(crate) fn u8_brand(&self, this: Value) -> Result<u32, Thrown> {
        if this.is_heap() {
            if let HeapObj::TypedArray { kind: 1, .. } = self.heap.get(this.heap_index()) {
                return Ok(this.heap_index());
            }
        }
        Err(Thrown("TypeError: method requires a Uint8Array receiver".into()))
    }

    /// The live bytes of a Uint8Array view (`None` if its buffer is detached or
    /// out of bounds).
    pub(crate) fn u8_bytes(&self, idx: u32) -> Option<Vec<u8>> {
        let len = self.ta_effective_len(idx)?;
        let (buffer, off) = match self.heap.get(idx) {
            HeapObj::TypedArray { buffer, byte_offset, .. } => (*buffer, *byte_offset),
            _ => return None,
        };
        match self.heap.get(buffer) {
            HeapObj::ArrayBuffer { data, detached } if !*detached => {
                Some(data[off..off + len].to_vec())
            }
            _ => None,
        }
    }

    /// Write `bytes` into a Uint8Array view starting at element 0 (bytes beyond the
    /// view are ignored; the caller bounds the slice to the view length).
    pub(crate) fn u8_write(&mut self, idx: u32, bytes: &[u8]) {
        let (buffer, off) = match self.heap.get(idx) {
            HeapObj::TypedArray { buffer, byte_offset, .. } => (*buffer, *byte_offset),
            _ => return,
        };
        if let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(buffer) {
            if !*detached {
                for (i, &b) in bytes.iter().enumerate() {
                    if off + i < data.len() {
                        data[off + i] = b;
                    }
                }
            }
        }
    }

    /// Read element `i` of a TypedArray as a Value (number, or BigInt for the
    /// 64-bit BigInt kinds). Out-of-bounds → undefined.
    pub(crate) fn ta_element_get(&mut self, ta_idx: u32, i: usize) -> Value {
        let (kind, bytes) = {
            let (buffer, kind, byte_offset) = match self.heap.get(ta_idx) {
                HeapObj::TypedArray { buffer, kind, byte_offset, .. } => {
                    (*buffer, *kind, *byte_offset)
                }
                _ => return Value::UNDEFINED,
            };
            if i >= self.ta_effective_len(ta_idx).unwrap_or(0) {
                return Value::UNDEFINED;
            }
            let size = native::TA_KINDS[kind as usize].1;
            let data = match self.heap.get(buffer) {
                HeapObj::ArrayBuffer { data, detached } if !*detached => data,
                _ => return Value::UNDEFINED,
            };
            let off = byte_offset + i * size;
            if off + size > data.len() {
                return Value::UNDEFINED;
            }
            let mut b = [0u8; 8];
            b[..size].copy_from_slice(&data[off..off + size]);
            (kind, b)
        };
        match kind {
            0 => Value::num(bytes[0] as i8 as f64),
            1 | 2 => Value::num(bytes[0] as f64),
            3 => Value::num(i16::from_le_bytes([bytes[0], bytes[1]]) as f64),
            4 => Value::num(u16::from_le_bytes([bytes[0], bytes[1]]) as f64),
            5 => Value::num(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
            6 => Value::num(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
            7 => Value::num(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
            8 => Value::num(f64::from_le_bytes(bytes)),
            9 => self.make_bigint(i64::from_le_bytes(bytes) as i128),
            _ => self.make_bigint(u64::from_le_bytes(bytes) as i128),
        }
    }

    /// A TypedArray element as its display string (read-only, no allocation) —
    /// for `display`/`inspect` (ToString of a TypedArray is the comma-join).
    pub(crate) fn ta_elem_string(&self, ta_idx: u32, i: usize) -> String {
        let (buffer, kind, byte_offset, length) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray { buffer, kind, byte_offset, length } => {
                (*buffer, *kind, *byte_offset, *length)
            }
            _ => return String::new(),
        };
        if i >= length {
            return "undefined".to_string();
        }
        let size = native::TA_KINDS[kind as usize].1;
        let data = match self.heap.get(buffer) {
            HeapObj::ArrayBuffer { data, .. } => data,
            _ => return String::new(),
        };
        let off = byte_offset + i * size;
        if off + size > data.len() {
            return "undefined".to_string();
        }
        let b = &data[off..off + size];
        match kind {
            0 => (b[0] as i8).to_string(),
            1 | 2 => b[0].to_string(),
            3 => i16::from_le_bytes([b[0], b[1]]).to_string(),
            4 => u16::from_le_bytes([b[0], b[1]]).to_string(),
            5 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]).to_string(),
            6 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]).to_string(),
            7 => fmt_f64(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
            8 => fmt_f64(f64::from_le_bytes(b.try_into().unwrap())),
            9 => i64::from_le_bytes(b.try_into().unwrap()).to_string(),
            _ => u64::from_le_bytes(b.try_into().unwrap()).to_string(),
        }
    }

    /// Write `v` to element `i` of a TypedArray (ToNumber/ToBigInt then encode per
    /// the element kind). Out-of-bounds → silent no-op (after coercion).
    pub(crate) fn ta_element_set(&mut self, ta_idx: u32, i: usize, v: Value) -> Result<(), Thrown> {
        let (buffer, kind, byte_offset) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray { buffer, kind, byte_offset, .. } => (*buffer, *kind, *byte_offset),
            _ => return Ok(()),
        };
        let size = native::TA_KINDS[kind as usize].1;
        let is_bigint = native::TA_KINDS[kind as usize].2;
        // Coerce BEFORE borrowing the buffer mutably (coercion can run user code).
        let bytes: [u8; 8] = if is_bigint {
            // A BigInt TypedArray element set uses ToBigInt (strict): a Number is a
            // TypeError (unlike the lenient `BigInt(5)` constructor coercion that
            // `to_bigint` implements). undefined/null/Symbol already throw in
            // `to_bigint`; only BigInt/Boolean/String are accepted here.
            if v.is_number() {
                return Err(Thrown(
                    "TypeError: cannot convert a Number to a BigInt typed-array element".into(),
                ));
            }
            let n = self.to_bigint(v)?;
            if kind == 9 {
                let mut o = [0u8; 8];
                o.copy_from_slice(&(n as i64).to_le_bytes());
                o
            } else {
                let mut o = [0u8; 8];
                o.copy_from_slice(&(n as u64).to_le_bytes());
                o
            }
        } else {
            // ToNumber(value) per SetTypedArrayElement: an object element runs
            // valueOf/@@toPrimitive (which a test may use to resize/detach the buffer
            // — re-checked below) and a Symbol/abrupt completion propagates.
            let f = self.to_number_coerce(v)?;
            ta_encode(kind, f)
        };
        // Re-check bounds after coercion (a valueOf could have resized the buffer).
        if i >= self.ta_effective_len(ta_idx).unwrap_or(0) {
            return Ok(());
        }
        if let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(buffer) {
            if *detached {
                return Ok(());
            }
            let off = byte_offset + i * size;
            if off + size <= data.len() {
                data[off..off + size].copy_from_slice(&bytes[..size]);
            }
        }
        Ok(())
    }

    /// `new ArrayBuffer(byteLength)`.
    pub(crate) fn build_array_buffer(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let n = match args.first() {
            Some(&v) if v != Value::UNDEFINED => self.to_number(v)?,
            _ => 0.0,
        };
        if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
            return Err(Thrown("RangeError: Invalid ArrayBuffer length".into()));
        }
        if n > MAX_ARRAY_BUFFER_LEN as f64 {
            return Err(Thrown("RangeError: ArrayBuffer length exceeds the maximum".into()));
        }
        // `maxByteLength` (resizable ArrayBuffer): validated, then recorded so the
        // buffer is `resizable` and `resize` accepts up to that length.
        let mut max_byte_length: Option<usize> = None;
        if let Some(&opt) = args.get(1) {
            if self.is_object_value(opt) {
                let mbl = self.get_prop(opt, "maxByteLength")?;
                if mbl != Value::UNDEFINED {
                    let m = self.to_number(mbl)?;
                    if !m.is_finite() || m < 0.0 || m.fract() != 0.0 || m > MAX_ARRAY_BUFFER_LEN as f64 {
                        return Err(Thrown("RangeError: invalid maxByteLength".into()));
                    }
                    if m < n {
                        return Err(Thrown("RangeError: maxByteLength < byteLength".into()));
                    }
                    max_byte_length = Some(m as usize);
                }
            }
        }
        let buf = self.alloc_array_buffer(n as usize);
        if let Some(m) = max_byte_length {
            self.ab_max.insert(buf, m);
        }
        Ok(Value::heap(buf))
    }

    /// `new SharedArrayBuffer(length[, {maxByteLength}])`. Reuses the ArrayBuffer
    /// representation + length/maxByteLength validation, then marks the buffer
    /// shared and links it to %SharedArrayBuffer.prototype%. A SAB is growable
    /// (never shrinks/detaches); the `maxByteLength` option makes `grow` available.
    pub(crate) fn build_shared_array_buffer(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let v = self.build_array_buffer(args)?;
        let idx = v.heap_index();
        self.shared_buffers.insert(idx);
        if self.sab_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.sab_proto));
        }
        Ok(v)
    }

    /// Validate an Atomics receiver/index: the receiver must be an INTEGER
    /// TypedArray (not Uint8Clamped/Float32/Float64), and the index in
    /// `[0, length)`. Returns `(ta_heap_index, element_index, kind)`.
    pub(crate) fn atomic_validate(&mut self, ta: Value, idx: Value) -> Result<(u32, usize, u8), Thrown> {
        let ti = match ta.is_heap().then(|| self.heap.get(ta.heap_index())) {
            Some(HeapObj::TypedArray { kind, .. }) => {
                // Uint8Clamped(2), Float32(7), Float64(8) are not integer types.
                if matches!(*kind, 2 | 7 | 8) {
                    return Err(Thrown(
                        "TypeError: Atomics operation requires an integer TypedArray".into(),
                    ));
                }
                ta.heap_index()
            }
            _ => {
                return Err(Thrown(
                    "TypeError: Atomics operation called on a non-TypedArray".into(),
                ))
            }
        };
        let kind = match self.heap.get(ti) {
            HeapObj::TypedArray { kind, .. } => *kind,
            _ => 0,
        };
        let i = self.to_integer_or_zero(idx)?;
        let len = self.ta_effective_len(ti).unwrap_or(0);
        if i < 0 || i as usize >= len {
            return Err(Thrown("RangeError: Atomics index out of bounds".into()));
        }
        Ok((ti, i as usize, kind))
    }

    /// Execute an `Atomics.<op>` call. Single-threaded, so read-modify-write ops
    /// are plain (non-contended); `wait` never blocks (no notifier → "timed-out")
    /// and `notify` reports 0 woken.
    pub(crate) fn atomics_op(&mut self, op: &str, args: &[Value]) -> Result<Value, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        let a2 = args.get(2).copied().unwrap_or(Value::UNDEFINED);
        // Operations that take no TypedArray receiver.
        if op == "isLockFree" {
            let n = self.to_integer_or_zero(a0)?;
            return Ok(Value::bool(matches!(n, 1 | 2 | 4 | 8)));
        }
        if op == "pause" {
            return Ok(Value::UNDEFINED);
        }
        let (ti, i, kind) = self.atomic_validate(a0, a1)?;
        let is_bigint = native::TA_KINDS[kind as usize].2;
        // waitAsync(ta, index, value, timeout) -> { async, value }. Single agent:
        // never truly blocks. value differs -> {async:false, value:"not-equal"};
        // matches with timeout 0 -> {async:false, value:"timed-out"}; matches with
        // a positive timeout -> {async:true, value:<pending promise>} (no notifier).
        if op == "waitAsync" {
            if !matches!(kind, 5 | 9) {
                return Err(Thrown(
                    "TypeError: Atomics.waitAsync requires an Int32Array or BigInt64Array".into(),
                ));
            }
            let cur = self.ta_element_get(ti, i);
            let eq = if is_bigint {
                self.to_bigint(a2)? == self.to_bigint(cur)?
            } else {
                self.to_integer_or_zero(a2)? == (cur.as_f64() as i64)
            };
            // ToNumber(timeout): NaN/absent -> +Infinity; clamp to >= 0.
            let t_raw = self.to_number_coerce(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
            let timeout = if t_raw.is_nan() { f64::INFINITY } else { t_raw.max(0.0) };
            let (is_async, value) = if !eq {
                (false, self.alloc_str("not-equal".to_string()))
            } else if timeout == 0.0 {
                (false, self.alloc_str("timed-out".to_string()))
            } else {
                // Would block; no notifier in a single agent, so the promise stays
                // pending (a real engine resolves it on notify or timeout).
                (true, Value::heap(self.alloc_promise()))
            };
            let mut m = crate::heap::ObjMap::new();
            let attr = crate::heap::PropAttr {
                writable: true,
                enumerable: true,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            m.define("async", Value::bool(is_async), attr);
            m.define("value", value, attr);
            let obj = self.heap.alloc(HeapObj::Object(m));
            if self.obj_proto != 0 {
                self.proto_of.insert(obj, Value::heap(self.obj_proto));
            }
            return Ok(Value::heap(obj));
        }
        // wait/notify need Int32Array or BigInt64Array; wait additionally needs a
        // SharedArrayBuffer. No real waiters in a single agent.
        if op == "wait" || op == "notify" {
            if !matches!(kind, 5 | 9) {
                return Err(Thrown(format!(
                    "TypeError: Atomics.{op} requires an Int32Array or BigInt64Array"
                )));
            }
            if op == "wait" {
                let shared = matches!(self.heap.get(ti),
                    HeapObj::TypedArray { buffer, .. } if self.shared_buffers.contains(buffer));
                if !shared {
                    return Err(Thrown("TypeError: Atomics.wait requires a SharedArrayBuffer".into()));
                }
                let cur = self.ta_element_get(ti, i);
                let eq = if is_bigint {
                    self.to_bigint(a2)? == self.to_bigint(cur)?
                } else {
                    self.to_integer_or_zero(a2)? == (cur.as_f64() as i64)
                };
                // Equal value would block; with no notifier this returns "timed-out".
                return Ok(self.alloc_str(if eq { "timed-out" } else { "not-equal" }.to_string()));
            }
            return Ok(Value::num(0.0));
        }
        // load / store / read-modify-write.
        if is_bigint {
            let v_in = if op == "load" { 0 } else { self.to_bigint(a2)? };
            let cur = self.ta_element_get(ti, i);
            let old = self.to_bigint(cur)?;
            match op {
                "load" => Ok(self.make_bigint(old)),
                "store" => {
                    let nv = self.make_bigint(v_in);
                    self.ta_element_set(ti, i, nv)?;
                    Ok(self.make_bigint(v_in))
                }
                "compareExchange" => {
                    let repl = self.to_bigint(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
                    if old == v_in {
                        let nv = self.make_bigint(repl);
                        self.ta_element_set(ti, i, nv)?;
                    }
                    Ok(self.make_bigint(old))
                }
                _ => {
                    let new = match op {
                        "add" => old.wrapping_add(v_in),
                        "sub" => old.wrapping_sub(v_in),
                        "and" => old & v_in,
                        "or" => old | v_in,
                        "xor" => old ^ v_in,
                        "exchange" => v_in,
                        _ => old,
                    };
                    let nv = self.make_bigint(new);
                    self.ta_element_set(ti, i, nv)?;
                    Ok(self.make_bigint(old))
                }
            }
        } else {
            let v_in = if op == "load" { 0 } else { self.to_integer_or_zero(a2)? };
            let cur = self.ta_element_get(ti, i);
            let old_i = cur.as_f64() as i64;
            match op {
                "load" => Ok(cur),
                "store" => {
                    self.ta_element_set(ti, i, Value::num(v_in as f64))?;
                    Ok(Value::num(v_in as f64))
                }
                "compareExchange" => {
                    let repl =
                        self.to_integer_or_zero(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
                    if old_i == v_in {
                        self.ta_element_set(ti, i, Value::num(repl as f64))?;
                    }
                    Ok(cur)
                }
                _ => {
                    let new_i = match op {
                        "add" => old_i.wrapping_add(v_in),
                        "sub" => old_i.wrapping_sub(v_in),
                        "and" => old_i & v_in,
                        "or" => old_i | v_in,
                        "xor" => old_i ^ v_in,
                        "exchange" => v_in,
                        _ => old_i,
                    };
                    self.ta_element_set(ti, i, Value::num(new_i as f64))?;
                    Ok(cur)
                }
            }
        }
    }

    /// `new <TA>(length | buffer[,off[,len]] | typedArray | array/iterable)`.
    pub(crate) fn build_typed_array(&mut self, kind: u8, args: &[Value]) -> Result<Value, Thrown> {
        let size = native::TA_KINDS[kind as usize].1;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // new TA(buffer, byteOffset?, length?)
        if let Some(buf) = self.as_array_buffer(a0) {
            // byteOffset/length are ToIndex: a negative or out-of-range value is a
            // RangeError (a bare `as usize` would silently saturate -1 to 0).
            let byte_offset = match args.get(1) {
                Some(&v) if v != Value::UNDEFINED => self.to_index(v)?,
                _ => 0,
            };
            let buf_len = self.array_buffer_len(buf);
            // A length-tracking view: no explicit length on a resizable buffer.
            let explicit_len = matches!(args.get(2), Some(&v) if v != Value::UNDEFINED);
            let tracking = !explicit_len && self.ab_max.contains_key(&buf);
            let length = match args.get(2) {
                Some(&v) if v != Value::UNDEFINED => self.to_index(v)?,
                _ => {
                    if buf_len < byte_offset || (buf_len - byte_offset) % size != 0 {
                        return Err(Thrown("RangeError: byte length not a multiple of element size".into()));
                    }
                    (buf_len - byte_offset) / size
                }
            };
            if byte_offset % size != 0 || byte_offset + length * size > buf_len {
                return Err(Thrown("RangeError: invalid TypedArray length/offset".into()));
            }
            // InitializeTypedArrayFromArrayBuffer step: the buffer must not be
            // detached (it may have been at entry, or detached by a ToIndex valueOf
            // on byteOffset/length above).
            if matches!(self.heap.get(buf), HeapObj::ArrayBuffer { detached: true, .. }) {
                return Err(Thrown(
                    "TypeError: Cannot construct a TypedArray on a detached ArrayBuffer".into(),
                ));
            }
            let ta = self.alloc_typed_array(buf, kind, byte_offset, length);
            if tracking {
                self.ta_tracking.insert(ta.heap_index());
            }
            return Ok(ta);
        }
        // new TA(typedArray) / new TA(array | iterable | array-like) → copy
        // element-by-element. Only an OBJECT first argument takes this path; a
        // primitive (Symbol/BigInt/string/number) is a LENGTH argument →
        // ToIndex (a Symbol/BigInt throws TypeError) below.
        if self.is_object_value(a0) && !a0.is_uninitialized() {
            let src: Vec<Value> = if let Some(src_ta) = self.as_typed_array(a0) {
                let len = match self.heap.get(src_ta) {
                    HeapObj::TypedArray { length, .. } => *length,
                    _ => 0,
                };
                (0..len).map(|i| self.ta_element_get(src_ta, i)).collect()
            } else {
                // A custom iterable (callable `@@iterator`) is iterated; anything
                // else is treated as ARRAY-LIKE per InitializeTypedArrayFromArrayLike
                // (read ToLength(`length`), then indices 0..length). The length read
                // and each element read propagate abrupt completions.
                let it = self.get_prop(a0, "@@iterator")?;
                if self.is_callable(it) {
                    self.iterate_to_vec(a0)?
                } else {
                    let lenv = self.get_prop(a0, "length")?;
                    let nf = self.to_number(lenv)?;
                    let n = if nf.is_nan() || nf <= 0.0 {
                        0
                    } else if nf > (MAX_ARRAY_BUFFER_LEN / size as i64) as f64 {
                        return Err(Thrown(
                            "RangeError: typed array length exceeds the maximum".into(),
                        ));
                    } else {
                        nf as usize
                    };
                    let mut v = Vec::with_capacity(n);
                    for i in 0..n {
                        v.push(self.get_index(a0, Value::int(i as i32))?);
                    }
                    v
                }
            };
            let len = src.len();
            let buf = self.alloc_array_buffer(len * size);
            let ta = self.alloc_typed_array(buf, kind, 0, len);
            for (i, v) in src.into_iter().enumerate() {
                self.ta_element_set(ta.heap_index(), i, v)?;
            }
            return Ok(ta);
        }
        // new TA(length)
        let length = if a0 == Value::UNDEFINED {
            0
        } else {
            let n = self.to_number(a0)?;
            if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
                return Err(Thrown("RangeError: invalid typed array length".into()));
            }
            if n > (MAX_ARRAY_BUFFER_LEN / size as i64) as f64 {
                return Err(Thrown("RangeError: typed array length exceeds the maximum".into()));
            }
            n as usize
        };
        let buf = self.alloc_array_buffer(length * size);
        Ok(self.alloc_typed_array(buf, kind, 0, length))
    }

    /// `new DataView(buffer, byteOffset?, byteLength?)`.
    pub(crate) fn build_data_view(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let buf = self
            .as_array_buffer(a0)
            .ok_or_else(|| Thrown("TypeError: DataView requires an ArrayBuffer".into()))?;
        let buf_len = self.array_buffer_len(buf);
        let byte_offset = match args.get(1) {
            Some(&v) if v != Value::UNDEFINED => self.to_index(v)?,
            _ => 0,
        };
        // ToIndex(byteOffset) precedes the bounds check, so a negative offset is a
        // RangeError before `offset > bufferLength` is consulted.
        if byte_offset > buf_len {
            return Err(Thrown("RangeError: invalid DataView offset".into()));
        }
        let byte_length = match args.get(2) {
            Some(&v) if v != Value::UNDEFINED => self.to_index(v)?,
            _ => buf_len.saturating_sub(byte_offset),
        };
        if byte_offset + byte_length > buf_len {
            return Err(Thrown("RangeError: invalid DataView offset/length".into()));
        }
        let idx = self.heap.alloc(HeapObj::DataView { buffer: buf, byte_offset, byte_length });
        if self.dataview_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.dataview_proto));
        }
        Ok(Value::heap(idx))
    }

    /// A binary arithmetic/bitwise op where at least one operand might be a BigInt.
    /// `Ok(None)` ⇒ neither is a BigInt (caller does its numeric path); `Ok(Some)`
    /// ⇒ both BigInt (result); `Err` ⇒ exactly one BigInt (mixing TypeError) or a
    /// BigInt-specific RangeError (÷0, negative exponent).
    pub(crate) fn bigint_binop(&mut self, op: BigOp, va: Value, vb: Value) -> Result<Option<Value>, Thrown> {
        // A BigInt is "involved" if either operand is a BigInt primitive or a BigInt
        // wrapper object (`Object(1n)`) — detectable without running user code. If
        // neither is, this is a Number op (return None → the caller's numeric path).
        if self.this_bigint_value(va).is_none() && self.this_bigint_value(vb).is_none() {
            return Ok(None);
        }
        // ApplyStringOrNumericBinaryOperator: ToNumeric each operand (ToPrimitive with
        // the number hint — `Object(1n)`/`{valueOf(){return 1n}}` → the BigInt). Both
        // must then be BigInt; mixing BigInt with a non-BigInt is a TypeError.
        let pa = self.to_primitive_number(va)?;
        let pb = self.to_primitive_number(vb)?;
        let (a, b) = match (self.bigint_value(pa), self.bigint_value(pb)) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                return Err(Thrown(
                    "TypeError: Cannot mix BigInt and other types, use explicit conversions".into(),
                ))
            }
        };
        let r = match op {
            BigOp::Add => a.wrapping_add(b),
            BigOp::Sub => a.wrapping_sub(b),
            BigOp::Mul => a.wrapping_mul(b),
            BigOp::Div | BigOp::Mod if b == 0 => {
                return Err(Thrown("RangeError: Division by zero".into()))
            }
            BigOp::Div => a.wrapping_div(b),
            BigOp::Mod => a.wrapping_rem(b),
            BigOp::Pow if b < 0 => {
                return Err(Thrown("RangeError: Exponent must be non-negative".into()))
            }
            BigOp::Pow => a.wrapping_pow(b.min(u32::MAX as i128) as u32),
            BigOp::And => a & b,
            BigOp::Or => a | b,
            BigOp::Xor => a ^ b,
            BigOp::Shl => a.wrapping_shl(b as u32),
            BigOp::Shr => a.wrapping_shr(b as u32),
        };
        Ok(Some(self.make_bigint(r)))
    }

}
