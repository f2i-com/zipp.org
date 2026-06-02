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
const MAX_ARRAY_BUFFER_LEN: i64 = 0x7FFF_FFFF;

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

    /// Read element `i` of a TypedArray as a Value (number, or BigInt for the
    /// 64-bit BigInt kinds). Out-of-bounds → undefined.
    pub(crate) fn ta_element_get(&mut self, ta_idx: u32, i: usize) -> Value {
        let (kind, bytes) = {
            let (buffer, kind, byte_offset, length) = match self.heap.get(ta_idx) {
                HeapObj::TypedArray { buffer, kind, byte_offset, length } => {
                    (*buffer, *kind, *byte_offset, *length)
                }
                _ => return Value::UNDEFINED,
            };
            if i >= length {
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
        let (buffer, kind, byte_offset, length) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray { buffer, kind, byte_offset, length } => {
                (*buffer, *kind, *byte_offset, *length)
            }
            _ => return Ok(()),
        };
        let size = native::TA_KINDS[kind as usize].1;
        let is_bigint = native::TA_KINDS[kind as usize].2;
        // Coerce BEFORE borrowing the buffer mutably (coercion can run user code).
        let bytes: [u8; 8] = if is_bigint {
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
            let f = self.to_number(v)?;
            ta_encode(kind, f)
        };
        if i >= length {
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
        // `maxByteLength` (resizable ArrayBuffer) is at least validated for range.
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
                }
            }
        }
        Ok(Value::heap(self.alloc_array_buffer(n as usize)))
    }

    /// `new <TA>(length | buffer[,off[,len]] | typedArray | array/iterable)`.
    pub(crate) fn build_typed_array(&mut self, kind: u8, args: &[Value]) -> Result<Value, Thrown> {
        let size = native::TA_KINDS[kind as usize].1;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // new TA(buffer, byteOffset?, length?)
        if let Some(buf) = self.as_array_buffer(a0) {
            let byte_offset = match args.get(1) {
                Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
                _ => 0,
            };
            let buf_len = self.array_buffer_len(buf);
            let length = match args.get(2) {
                Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
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
            return Ok(self.alloc_typed_array(buf, kind, byte_offset, length));
        }
        // new TA(typedArray) / new TA(array | iterable | array-like) → copy
        // element-by-element.
        if a0.is_heap() && !a0.is_uninitialized() {
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
            Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
            _ => 0,
        };
        let byte_length = match args.get(2) {
            Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
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
        let (a, b) = (self.bigint_value(va), self.bigint_value(vb));
        if a.is_none() && b.is_none() {
            return Ok(None);
        }
        let (a, b) = match (a, b) {
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
