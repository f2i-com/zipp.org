#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// Practical upper bound on an ArrayBuffer/TypedArray byte length. A larger
/// request is a RangeError rather than an attempted (process-aborting) alloc.
#[cfg(feature = "safe-sandbox")]
pub(crate) const MAX_ARRAY_BUFFER_LEN: i64 = 1 << 20;
#[cfg(not(feature = "safe-sandbox"))]
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
    /// Whether `key` on this TypedArray instance still resolves, along its REAL
    /// prototype chain, to the built-in %TypedArray%.prototype accessor,
    /// so the instance path may answer it directly instead of walking.
    ///
    /// Deliberately NOT an identity check against this realm's `ta_protos`: a
    /// TypedArray created in another realm carries that realm's prototype, which
    /// is just as intrinsic. What must be rejected is a chain a user re-pointed
    /// or shadowed (`Object.setPrototypeOf(ta, {length: 7})`), where the spec
    /// requires the ordinary lookup to win.
    pub(crate) fn ta_named_is_intrinsic(&self, ta_idx: u32, key: &str) -> bool {
        let want = match key {
            "length" => crate::vm::native::TA_GET_LENGTH,
            "byteLength" => crate::vm::native::TA_GET_BYTELENGTH,
            "byteOffset" => crate::vm::native::TA_GET_BYTEOFFSET,
            "buffer" => crate::vm::native::TA_GET_BUFFER,
            "@@toStringTag" => crate::vm::native::TA_GET_TOSTRINGTAG,
            _ => return false,
        };
        let Some(owner) = self.ta_chain_has_own(ta_idx, key) else {
            return false; // nothing in the chain defines it -> `undefined`
        };
        match self.heap.get(owner) {
            HeapObj::Object(m) => match m.pos(key) {
                Some(s) if m.attr_at(s).accessor && m.vals[s].is_heap() => matches!(
                    self.heap.get(m.vals[s].heap_index()),
                    HeapObj::Native(id) if *id == want
                ),
                _ => false,
            },
            _ => false,
        }
    }

    /// The nearest object on `ta_idx`'s prototype chain with an own `key`
    /// (the instance's own side-table props are checked by the caller first).
    fn ta_chain_has_own(&self, ta_idx: u32, key: &str) -> Option<u32> {
        let mut cur = match self.proto_of.get(&ta_idx) {
            Some(p) if p.is_heap() => p.heap_index(),
            Some(_) => return None, // explicit null prototype
            None => match self.heap.get(ta_idx) {
                HeapObj::TypedArray { kind, .. } => *self.ta_protos.get(*kind as usize)?,
                _ => return None,
            },
        };
        // A prototype chain may be arbitrarily deep, and `Object.setPrototypeOf`
        // can also manufacture a cycle in malformed/host-created state.  There
        // can be at most `heap.len()` distinct heap objects in a valid chain, so
        // that cardinality is both a complete finite walk and a cycle-safe,
        // fail-closed bound.  A small fixed cap would incorrectly jump over a
        // perfectly legal deep shadow (notably a `length` getter).
        for _ in 0..self.heap.len() {
            // JS-created prototype edges are range-valid by construction, but
            // embedders can inject malformed state.  Never let a proof helper
            // turn such an edge into a host panic; an invalid edge simply
            // means the intrinsic chain cannot be proven.
            if cur as usize >= self.heap.len() {
                return None;
            }
            match self.heap.get(cur) {
                HeapObj::Object(m) if m.pos(key).is_some() => return Some(cur),
                HeapObj::Object(_) => {}
                // This helper is a proof that the nearest property is the
                // intrinsic accessor, not a replacement for [[Get]].  Exotic
                // chain nodes can have virtual/side-table own properties (an
                // Array's `length`, a TypedArray's named properties, or a
                // Proxy trap), none of which an ObjMap-only walk can exclude.
                // Fail closed and let the ordinary property path resolve them.
                _ => return None,
            }
            cur = match self.proto_of.get(&cur) {
                Some(p) if p.is_heap() => p.heap_index(),
                _ => return None,
            };
        }
        None
    }

    /// Whether `ta.length` on this TypedArray still resolves to the pristine
    /// built-in %TypedArray%.prototype getter, so a caller may answer it
    /// directly instead of invoking the accessor.
    ///
    /// Unlike `Array`, whose `length` is an OWN exotic property that no
    /// prototype can shadow, a TypedArray's `length` is inherited, so all four
    /// of these must hold: no own `length` on the instance, the instance's
    /// prototype is still its kind's intrinsic, that intrinsic does not shadow
    /// `length`, and %TypedArray%.prototype's `length` is still the built-in
    /// accessor (`Native(TA_GET_LENGTH)`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ta_length_is_intrinsic(&self, ta_idx: u32) -> bool {
        let kind = match self.heap.get(ta_idx) {
            HeapObj::TypedArray { kind, .. } => *kind as usize,
            _ => return false,
        };
        // An extra NAMED own property (`ta.length = …` / defineProperty) lives
        // in the side table and wins over the inherited accessor.
        if self
            .arr_props
            .get(&ta_idx)
            .is_some_and(|m| m.pos("length").is_some())
        {
            return false;
        }
        let want_proto = match self.ta_protos.get(kind) {
            Some(&p) if p != 0 => p,
            _ => return false,
        };
        // `Object.setPrototypeOf(ta, …)` re-points the chain.
        match self.proto_of.get(&ta_idx) {
            Some(v) if v.is_heap() && v.heap_index() == want_proto => {}
            _ => return false,
        }
        // %Float64Array.prototype% etc. must not shadow `length` …
        if let HeapObj::Object(m) = self.heap.get(want_proto) {
            if m.pos("length").is_some() {
                return false;
            }
        } else {
            return false;
        }
        // …its [[Prototype]] must still be the intrinsic base. A user can
        // re-point `Uint8Array.prototype` itself without putting an own
        // `length` on it; skipping this link proof would jump over a getter on
        // the replacement chain and return the internal slot directly.
        if self.ta_base_proto == 0 {
            return false;
        }
        match self.proto_of.get(&want_proto) {
            Some(v) if v.is_heap() && v.heap_index() == self.ta_base_proto => {}
            _ => return false,
        }
        // … and the base prototype's slot must still be the built-in getter.
        match self.heap.get(self.ta_base_proto) {
            HeapObj::Object(m) => match m.pos("length") {
                Some(s) if m.attr_at(s).accessor && m.vals[s].is_heap() => matches!(
                    self.heap.get(m.vals[s].heap_index()),
                    HeapObj::Native(id) if *id == crate::vm::native::TA_GET_LENGTH
                ),
                _ => false,
            },
            _ => false,
        }
    }

    /// `IsTypedArrayFixedLength(O)`: is this view's length settled for good?
    ///
    /// False for a length-tracking view, and false for ANY view onto a
    /// resizable ArrayBuffer — that buffer can SHRINK, taking a fixed window
    /// out of bounds. A growable *Shared*ArrayBuffer can only get longer, so a
    /// fixed-length view onto one stays fixed. This is what decides whether
    /// `[[PreventExtensions]]` may succeed.
    pub(crate) fn ta_is_fixed_length(&self, ta_idx: u32, buffer: u32) -> bool {
        if self.ta_tracking.contains(&ta_idx) {
            return false;
        }
        !self.ab_max.contains_key(&buffer) || self.shared_buffers.contains(&buffer)
    }

    pub(crate) fn ta_effective_len(&self, ta_idx: u32) -> Option<usize> {
        let (buffer, kind, byte_offset, length) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                length,
            } => (*buffer, *kind, *byte_offset, *length),
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
    pub(crate) fn alloc_array_buffer(&mut self, byte_len: usize) -> Result<u32, Thrown> {
        #[cfg(feature = "instrument")]
        self.instrument_preflight_heap_growth(byte_len)
            .map_err(|message| Thrown(message.into()))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_len)
            .map_err(|_| Thrown("RangeError: ArrayBuffer allocation failed".into()))?;
        bytes.resize(byte_len, 0u8);
        let idx = self.heap.alloc(HeapObj::ArrayBuffer {
            data: bytes.into(),
            detached: false,
        });
        if self.arraybuffer_proto != 0 {
            // AllocateArrayBuffer does OrdinaryCreateFromConstructor(%ArrayBuffer%,
            // "%ArrayBuffer.prototype%") against the CURRENT realm — so the buffer a
            // child realm's TypedArray constructor allocates for itself belongs to
            // that realm. `g.eval("new Uint8Array(16)").buffer.constructor` was the
            // MAIN `ArrayBuffer`, which then made SpeciesConstructor on it pick the
            // wrong realm (staging/sm/ArrayBuffer/slice-species.js line 162).
            // `native_home` is identity outside a realm-copied built-in.
            let p = self.native_home(self.arraybuffer_proto);
            self.proto_of.insert(idx, Value::heap(p));
            if p != self.arraybuffer_proto {
                if let Some(r) = self.native_callee_realm {
                    self.obj_realm.insert(idx, r);
                }
            }
        }
        Ok(idx)
    }
    /// Allocate a SharedArrayBuffer: TRULY-SHARED bytes (`AbData::Shared`, so a
    /// worker agent handed this buffer aliases the same memory), marked in
    /// `shared_buffers` and linked to %SharedArrayBuffer.prototype%. A growable
    /// SAB preallocates `maxByteLength` zeroed bytes; `grow` is a length store.
    pub(crate) fn alloc_shared_array_buffer(
        &mut self,
        byte_len: usize,
        max: Option<usize>,
    ) -> Result<u32, Thrown> {
        let capacity = max.unwrap_or(byte_len);
        #[cfg(feature = "instrument")]
        self.instrument_preflight_heap_growth(capacity)
            .map_err(|message| Thrown(message.into()))?;
        let mem = crate::heap::SharedMem::try_new(byte_len, capacity)
            .map_err(|_| Thrown("RangeError: SharedArrayBuffer allocation failed".into()))?;
        let idx = self.heap.alloc(HeapObj::ArrayBuffer {
            data: crate::heap::AbData::Shared(std::sync::Arc::new(mem)),
            detached: false,
        });
        self.shared_buffers.insert(idx);
        if self.sab_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.sab_proto));
        }
        Ok(idx)
    }
    /// Allocate a TypedArray view over `buffer`, linked to that kind's prototype.
    pub(crate) fn alloc_typed_array(
        &mut self,
        buffer: u32,
        kind: u8,
        byte_offset: usize,
        length: usize,
    ) -> Value {
        let idx = self.heap.alloc(HeapObj::TypedArray {
            buffer,
            kind,
            byte_offset,
            length,
        });
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
            return Err(Thrown(
                "TypeError: lastChunkHandling must be a string".into(),
            ));
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
        Err(Thrown(
            "TypeError: method requires a Uint8Array receiver".into(),
        ))
    }

    /// The live bytes of a Uint8Array view (`None` if its buffer is detached or
    /// out of bounds).
    pub(crate) fn u8_bytes(&self, idx: u32) -> Option<Vec<u8>> {
        let len = self.ta_effective_len(idx)?;
        let (buffer, off) = match self.heap.get(idx) {
            HeapObj::TypedArray {
                buffer,
                byte_offset,
                ..
            } => (*buffer, *byte_offset),
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
            HeapObj::TypedArray {
                buffer,
                byte_offset,
                ..
            } => (*buffer, *byte_offset),
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
    /// Copy `count` elements from `src[src_start..]` into `dst[0..]` as RAW
    /// BYTES. Returns false when that is not possible — different element
    /// kinds, a detached or non-plain buffer, an out-of-range range — and the
    /// caller falls back to the per-element path.
    ///
    /// %TypedArray%.prototype.slice with a same-type destination is specified as
    /// a byte copy, and the difference is OBSERVABLE for the float kinds:
    /// routing an element through an f64 `Value` canonicalises a NaN payload, so
    /// a Float32Array holding 0x7F800001 came back as 0x7FC00000.
    pub(crate) fn ta_raw_copy(
        &mut self,
        src: u32,
        src_start: usize,
        dst: u32,
        count: usize,
    ) -> bool {
        if count == 0 {
            return true;
        }
        let (sbuf, skind, soff) = match self.heap.get(src) {
            HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                ..
            } => (*buffer, *kind, *byte_offset),
            _ => return false,
        };
        let (dbuf, dkind, doff) = match self.heap.get(dst) {
            HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                ..
            } => (*buffer, *kind, *byte_offset),
            _ => return false,
        };
        if skind != dkind || sbuf == dbuf {
            return false; // different types, or overlapping storage
        }
        let size = native::TA_KINDS[skind as usize].1;
        let n = count * size;
        let bytes: Vec<u8> = match self.heap.get(sbuf) {
            HeapObj::ArrayBuffer { data, detached } if !*detached => {
                let a = soff + src_start * size;
                if a + n > data.len() {
                    return false;
                }
                data[a..a + n].to_vec()
            }
            _ => return false,
        };
        match self.heap.get_mut(dbuf) {
            HeapObj::ArrayBuffer { data, detached } if !*detached => {
                if doff + n > data.len() {
                    return false;
                }
                data[doff..doff + n].copy_from_slice(&bytes);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn ta_element_get(&mut self, ta_idx: u32, i: usize) -> Value {
        let (kind, bytes) = {
            let (buffer, kind, byte_offset) = match self.heap.get(ta_idx) {
                HeapObj::TypedArray {
                    buffer,
                    kind,
                    byte_offset,
                    ..
                } => (*buffer, *kind, *byte_offset),
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
            11 => Value::num(crate::vm::helpers_num2::f16_bits_to_f64(
                u16::from_le_bytes([bytes[0], bytes[1]]),
            )),
            _ => self.make_bigint(u64::from_le_bytes(bytes) as i128),
        }
    }

    /// A TypedArray element as its display string (read-only, no allocation) —
    /// for `display`/`inspect` (ToString of a TypedArray is the comma-join).
    pub(crate) fn ta_elem_string(&self, ta_idx: u32, i: usize) -> String {
        let (buffer, kind, byte_offset) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                ..
            } => (*buffer, *kind, *byte_offset),
            _ => return String::new(),
        };
        // Bound by the EFFECTIVE length: the raw stored `length` is stale for a
        // length-tracking view after its resizable buffer grows or shrinks.
        if i >= self.ta_effective_len(ta_idx).unwrap_or(0) {
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
            11 => fmt_f64(crate::vm::helpers_num2::f16_bits_to_f64(
                u16::from_le_bytes([b[0], b[1]]),
            )),
            _ => u64::from_le_bytes(b.try_into().unwrap()).to_string(),
        }
    }

    /// Write `v` to element `i` of a TypedArray (ToNumber/ToBigInt then encode per
    /// the element kind). Out-of-bounds → silent no-op (after coercion).
    /// TypedArraySetElement's COERCION only (ToBigInt / ToNumber of the value,
    /// observable, abrupt propagates) for writes whose canonical-numeric key is
    /// NOT a valid index: the spec coerces BEFORE the IsValidIntegerIndex check,
    /// so a detached/out-of-bounds/non-integer-key write still runs valueOf.
    pub(crate) fn ta_coerce_for_set(&mut self, ta_idx: u32, v: Value) -> Result<(), Thrown> {
        let kind = match self.heap.get(ta_idx) {
            HeapObj::TypedArray { kind, .. } => *kind,
            _ => return Ok(()),
        };
        if native::TA_KINDS[kind as usize].2 {
            if v.is_number() {
                return Err(Thrown(
                    "TypeError: cannot convert a Number to a BigInt typed-array element".into(),
                ));
            }
            self.to_bigint(v)?;
        } else {
            if v.is_heap()
                && matches!(
                    self.heap.get(v.heap_index()),
                    HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
                )
            {
                return Err(Thrown(
                    "TypeError: cannot convert a BigInt to a number".into(),
                ));
            }
            self.to_number_coerce(v)?;
        }
        Ok(())
    }

    pub(crate) fn ta_element_set(&mut self, ta_idx: u32, i: usize, v: Value) -> Result<(), Thrown> {
        let (buffer, kind, byte_offset) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                ..
            } => (*buffer, *kind, *byte_offset),
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
            // NumericToRawBytes: wrap to the low 64 bits, two's complement
            // (exact for any magnitude — a Big-tier value wraps correctly).
            let n = self.to_bigint(v)?;
            n.to_u64_wrap().to_le_bytes()
        } else {
            // ToNumber(BigInt) throws in SetTypedArrayElement (the engine's
            // to_number is deliberately lenient on BigInt for comparisons).
            if v.is_heap()
                && matches!(
                    self.heap.get(v.heap_index()),
                    HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
                )
            {
                return Err(Thrown(
                    "TypeError: cannot convert a BigInt to a number".into(),
                ));
            }
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

    /// The ArrayBuffer/SharedArrayBuffer constructor's observable argument
    /// coercions, split from allocation so the ctor can run them BEFORE
    /// OrdinaryCreateFromConstructor reads newTarget.prototype: ToIndex(length)
    /// (undefined/NaN -> 0, fractional truncates, negative/huge -> RangeError,
    /// objects run ToPrimitive with abrupt propagation), then the options bag's
    /// maxByteLength (ToIndex, must be >= length).
    pub(crate) fn validate_array_buffer_args(
        &mut self,
        args: &[Value],
    ) -> Result<(usize, Option<usize>), Thrown> {
        let n = self.to_index(args.first().copied().unwrap_or(Value::UNDEFINED))?;
        let mut max_byte_length: Option<usize> = None;
        if let Some(&opt) = args.get(1) {
            if self.is_object_value(opt) {
                let mbl = self.get_prop(opt, "maxByteLength")?;
                if mbl != Value::UNDEFINED {
                    let m = self.to_index(mbl)?;
                    if m > MAX_ARRAY_BUFFER_LEN as usize {
                        return Err(Thrown("RangeError: invalid maxByteLength".into()));
                    }
                    if m < n {
                        return Err(Thrown("RangeError: maxByteLength < byteLength".into()));
                    }
                    max_byte_length = Some(m);
                }
            }
        }
        Ok((n, max_byte_length))
    }

    /// Validate an Atomics receiver/index: the receiver must be an INTEGER
    /// TypedArray (not Uint8Clamped/Float32/Float64), and the index in
    /// `[0, length)`. Returns `(ta_heap_index, element_index, kind)`.
    /// ValidateIntegerTypedArray + ValidateAtomicAccess. The type/kind/shared checks
    /// happen BEFORE the index is coerced (so a poisoned-valueOf index on a wrong
    /// array type throws the spec TypeError, not the index error). `waitable` ⇒ the
    /// view must be Int32Array/BigInt64Array (wait/waitAsync/notify); `needs_shared`
    /// ⇒ its buffer must be a SharedArrayBuffer (wait).
    pub(crate) fn atomic_validate(
        &mut self,
        ta: Value,
        idx: Value,
        waitable: bool,
        needs_shared: bool,
        is_write: bool,
        revalidate: bool,
    ) -> Result<(u32, usize, u8), Thrown> {
        let ti = match ta.is_heap().then(|| self.heap.get(ta.heap_index())) {
            Some(HeapObj::TypedArray { kind, .. }) => {
                if waitable {
                    if !matches!(*kind, 5 | 9) {
                        return Err(Thrown(
                            "TypeError: Atomics operation requires an Int32Array or BigInt64Array"
                                .into(),
                        ));
                    }
                } else if matches!(*kind, 2 | 7 | 8 | 11) {
                    // Uint8Clamped(2), Float32(7), Float64(8), Float16(11) are not integer types.
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
        // ValidateTypedArray: a DETACHED backing buffer is a TypeError, raised
        // BEFORE any index/value coercion (their valueOf must not run).
        {
            let buffer = match self.heap.get(ti) {
                HeapObj::TypedArray { buffer, .. } => *buffer,
                _ => 0,
            };
            if matches!(
                self.heap.get(buffer),
                HeapObj::ArrayBuffer { detached: true, .. }
            ) {
                return Err(Thrown(
                    "TypeError: Cannot perform Atomics operation on a detached ArrayBuffer".into(),
                ));
            }
        }
        if needs_shared {
            let shared = matches!(self.heap.get(ti),
                HeapObj::TypedArray { buffer, .. } if self.shared_buffers.contains(buffer));
            if !shared {
                return Err(Thrown(
                    "TypeError: Atomics.wait/waitAsync requires a SharedArrayBuffer".into(),
                ));
            }
        }
        // ValidateTypedArray step 4: a ~write~ access on an immutable-buffer-backed
        // view is a TypeError, raised BEFORE the index/value are coerced.
        if is_write {
            let buffer = match self.heap.get(ti) {
                HeapObj::TypedArray { buffer, .. } => *buffer,
                _ => 0,
            };
            if self.immutable_buffers.contains(&buffer) {
                return Err(Thrown(
                    "TypeError: Cannot perform an Atomics write on an immutable ArrayBuffer".into(),
                ));
            }
        }
        let kind = match self.heap.get(ti) {
            HeapObj::TypedArray { kind, .. } => *kind,
            _ => 0,
        };
        // ValidateAtomicAccess: the length is snapshotted BEFORE
        // ToIndex(requestIndex) — the index coercion may grow/shrink/detach a
        // resizable buffer, and the bounds check uses the PRE-coercion length.
        let len = self.ta_effective_len(ti).unwrap_or(0);
        // ToIndex: RangeError on a negative index, TypeError on Symbol/BigInt.
        let i = self.to_index(idx)?;
        if i >= len {
            return Err(Thrown("RangeError: Atomics index out of bounds".into()));
        }
        // RevalidateAtomicAccess (DATA ops only — wait/waitAsync/notify never
        // touch the buffer afterward): a coercion side effect that detached
        // the buffer or shrank it below the index aborts the access.
        if revalidate {
            let buffer = match self.heap.get(ti) {
                HeapObj::TypedArray { buffer, .. } => *buffer,
                _ => 0,
            };
            if matches!(
                self.heap.get(buffer),
                HeapObj::ArrayBuffer { detached: true, .. }
            ) {
                return Err(Thrown(
                    "TypeError: Cannot perform Atomics operation on a detached ArrayBuffer".into(),
                ));
            }
            let cur = self.ta_effective_len(ti).unwrap_or(0);
            if i >= cur {
                return Err(Thrown("RangeError: Atomics index out of bounds".into()));
            }
        }
        Ok((ti, i, kind))
    }

    /// The (buffer heap idx, byte address) a wait/notify on `(ta, index)`
    /// keys its waiter-list entry by.
    fn ta_wait_addr(&self, ti: u32, i: usize) -> (u32, usize) {
        match self.heap.get(ti) {
            HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                ..
            } => {
                let size = native::TA_KINDS[*kind as usize].1 as usize;
                (*buffer, byte_offset + i * size)
            }
            _ => (ti, i),
        }
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
            // iterationNumber must be undefined or an INTEGRAL Number.
            if a0 != Value::UNDEFINED {
                let bad = !a0.is_number() || {
                    let f = a0.as_f64();
                    f.is_nan() || f.is_infinite() || f.fract() != 0.0
                };
                if bad {
                    return Err(Thrown(
                        "TypeError: Atomics.pause iterationNumber must be an integral Number"
                            .into(),
                    ));
                }
            }
            return Ok(Value::UNDEFINED);
        }
        let waitable = matches!(op, "wait" | "waitAsync" | "notify");
        let needs_shared = matches!(op, "wait" | "waitAsync");
        let is_write = matches!(
            op,
            "store" | "add" | "sub" | "and" | "or" | "xor" | "exchange" | "compareExchange"
        );
        let (ti, i, kind) =
            self.atomic_validate(a0, a1, waitable, needs_shared, is_write, !waitable)?;
        let is_bigint = native::TA_KINDS[kind as usize].2;
        // waitAsync(ta, index, value, timeout) -> { async, value }. value differs
        // -> {async:false, value:"not-equal"}; matches with timeout 0 ->
        // {async:false, value:"timed-out"}; matches with a positive timeout ->
        // {async:true, value:<pending promise>}, registered BOTH in the global
        // waiter registry (any agent's notify can wake it through this Vm's
        // mailbox) and in the local `async_waiters` (deadline bookkeeping).
        if op == "waitAsync" {
            if !matches!(kind, 5 | 9) {
                return Err(Thrown(
                    "TypeError: Atomics.waitAsync requires an Int32Array or BigInt64Array".into(),
                ));
            }
            let cur = self.ta_element_get(ti, i);
            let (expected, eq) = if is_bigint {
                // ToBigInt64(value): wrap to i64 (kind 9 is the only BigInt kind here).
                let e = self.to_bigint(a2)?.to_i64_wrap() as i128;
                (e, e == self.to_bigint(cur)?.to_i64_wrap() as i128)
            } else {
                let e = self.to_integer_or_zero(a2)? as i128;
                (e, e == (cur.as_f64() as i64) as i128)
            };
            // ToNumber(timeout): NaN/absent -> +Infinity; clamp to >= 0.
            let t_raw = self.to_number_coerce(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
            let timeout = if t_raw.is_nan() {
                f64::INFINITY
            } else {
                t_raw.max(0.0)
            };
            let (buf, addr) = self.ta_wait_addr(ti, i);
            let mem = match self.heap.get(buf) {
                HeapObj::ArrayBuffer { data, .. } => data.shared().cloned(),
                _ => None,
            };
            let (is_async, value) = if let Some(mem) = mem {
                // Truly-shared storage: decide under the registry lock — the
                // element is RE-loaded there (SeqCst), making the check
                // atomic against a racing notify on another thread.
                let key = (std::sync::Arc::as_ptr(&mem) as usize, addr);
                match agents::register_async_waiter(key, timeout, &self.mailbox, || {
                    sab_atomic_op(&mem, kind, addr, "load", 0, 0) as i128 == expected
                }) {
                    agents::AsyncWaitDecision::NotEqual => {
                        (false, self.alloc_str("not-equal".to_string()))
                    }
                    agents::AsyncWaitDecision::TimedOut => {
                        (false, self.alloc_str("timed-out".to_string()))
                    }
                    agents::AsyncWaitDecision::Registered(id) => {
                        let p = self.alloc_promise();
                        self.async_waiters.push((
                            buf,
                            addr,
                            p,
                            agents::finite_deadline(timeout),
                            id,
                        ));
                        (true, Value::heap(p))
                    }
                }
            } else if !eq {
                // Non-shared storage (defensive — atomic_validate required a
                // SAB): single agent, no registry; id 0 = never registered,
                // so its deadline always resolves "timed-out".
                (false, self.alloc_str("not-equal".to_string()))
            } else if timeout == 0.0 {
                (false, self.alloc_str("timed-out".to_string()))
            } else {
                let p = self.alloc_promise();
                self.async_waiters
                    .push((buf, addr, p, agents::finite_deadline(timeout), 0));
                (true, Value::heap(p))
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
            let obj = self.heap.alloc(HeapObj::Object(Box::new(m)));
            if self.obj_proto != 0 {
                self.proto_of.insert(obj, Value::heap(self.obj_proto));
            }
            return Ok(Value::heap(obj));
        }
        // wait/notify: the Int32Array/BigInt64Array + SharedArrayBuffer checks
        // already ran in atomic_validate (before the index coercion).
        if op == "wait" || op == "notify" {
            if op == "wait" {
                let cur = self.ta_element_get(ti, i);
                let (expected, eq) = if is_bigint {
                    // ToBigInt64(value): wrap to i64 (kind 9 only).
                    let e = self.to_bigint(a2)?.to_i64_wrap() as i128;
                    (e, e == self.to_bigint(cur)?.to_i64_wrap() as i128)
                } else {
                    let e = self.to_integer_or_zero(a2)? as i128;
                    (e, e == (cur.as_f64() as i64) as i128)
                };
                // ToNumber(timeout) runs next (DoWait step 6) — a Symbol is a
                // TypeError, a poisoned valueOf throws.
                let t_raw =
                    self.to_number_coerce(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
                // DoWait step: a sync wait in an agent that cannot suspend is a
                // TypeError — AFTER the value/timeout coercions, per spec order.
                if !self.can_block {
                    return Err(Thrown(
                        "TypeError: Atomics.wait cannot suspend in this agent".into(),
                    ));
                }
                // NaN timeout -> +Infinity; clamp to >= 0.
                let timeout = if t_raw.is_nan() {
                    f64::INFINITY
                } else {
                    t_raw.max(0.0)
                };
                let (buf, addr) = self.ta_wait_addr(ti, i);
                let mem = match self.heap.get(buf) {
                    HeapObj::ArrayBuffer { data, .. } => data.shared().cloned(),
                    _ => None,
                };
                // Truly-shared storage: REALLY suspend this agent, FIFO-
                // registered so another agent's notify can wake it. The
                // element is re-loaded (SeqCst) under the registry lock.
                if let Some(mem) = mem {
                    let key = (std::sync::Arc::as_ptr(&mem) as usize, addr);
                    let outcome = agents::sync_wait(key, timeout, || {
                        sab_atomic_op(&mem, kind, addr, "load", 0, 0) as i128 == expected
                    });
                    let s = match outcome {
                        agents::WaitOutcome::NotEqual => "not-equal",
                        agents::WaitOutcome::TimedOut => "timed-out",
                        agents::WaitOutcome::Ok => "ok",
                    };
                    return Ok(self.alloc_str(s.to_string()));
                }
                // Non-shared storage (defensive — atomic_validate required a
                // SAB): single agent, no notifier — an equal value "blocks"
                // for zero time.
                return Ok(self.alloc_str(if eq { "timed-out" } else { "not-equal" }.to_string()));
            }
            // notify(ta, index, count): ToIntegerOrInfinity(count) runs (so a Symbol /
            // throwing valueOf is observed, after the index coercion). Wake up to
            // `count` waiters registered on this (memory, address) FIFO.
            let count = if a2 == Value::UNDEFINED {
                f64::INFINITY
            } else {
                let n = self.to_number_strict(a2)?;
                if n.is_nan() {
                    0.0
                } else {
                    n.trunc().max(0.0)
                }
            };
            let (buf, addr) = self.ta_wait_addr(ti, i);
            let mem = match self.heap.get(buf) {
                HeapObj::ArrayBuffer { data, .. } => data.shared().cloned(),
                _ => None,
            };
            // Truly-shared: pop from the global registry — blocked sync waits
            // on any thread wake there, a remote Vm's waitAsync is delivered
            // through its mailbox, and THIS Vm's own waitAsync entries come
            // back as ids to resolve "ok" in place.
            if let Some(mem) = mem {
                let key = (std::sync::Arc::as_ptr(&mem) as usize, addr);
                let (woken, own) = agents::notify_waiters(key, count, &self.mailbox);
                if !own.is_empty() {
                    let mut to_wake: Vec<u32> = Vec::new();
                    self.async_waiters.retain(|&(_, _, p, _, id)| {
                        if own.contains(&id) {
                            to_wake.push(p);
                            false
                        } else {
                            true
                        }
                    });
                    for p in to_wake {
                        let v = self.alloc_str("ok".to_string());
                        self.resolve(p, v);
                    }
                }
                return Ok(Value::num(woken));
            }
            // Non-shared buffer: no cross-agent waiter can exist (and none of
            // this Vm's either — waitAsync requires a SAB); reports 0 woken.
            let mut woken = 0.0;
            let mut to_wake: Vec<u32> = Vec::new();
            self.async_waiters.retain(|&(b, a, p, _, _)| {
                if woken < count && b == buf && a == addr {
                    to_wake.push(p);
                    woken += 1.0;
                    false
                } else {
                    true
                }
            });
            for p in to_wake {
                let v = self.alloc_str("ok".to_string());
                self.resolve(p, v);
            }
            return Ok(Value::num(woken));
        }
        // load / store / read-modify-write. (Immutable-buffer writes already threw in
        // atomic_validate, before any coercion.) An element backed by a SHARED
        // (SAB) buffer is accessed with REAL atomic instructions (SeqCst) at its
        // byte address, so worker-agent threads observe the op atomically; a
        // Local buffer keeps the plain single-threaded path. The Arc + offset
        // stay valid across the value coercions below (shared storage never
        // moves), and the observable coercion order is identical on both paths.
        let elem_size = native::TA_KINDS[kind as usize].1;
        let sab_target: Option<(std::sync::Arc<crate::heap::SharedMem>, usize)> = match self
            .heap
            .get(ti)
        {
            HeapObj::TypedArray {
                buffer,
                byte_offset,
                ..
            } => {
                let off = byte_offset + i * elem_size;
                match self.heap.get(*buffer) {
                    HeapObj::ArrayBuffer { data, .. } => data.shared().map(|m| (m.clone(), off)),
                    _ => None,
                }
            }
            _ => None,
        };
        if is_bigint {
            let v_b = if op == "load" {
                BigVal::Small(0)
            } else {
                self.to_bigint(a2)?
            };
            // NumericToRawBytes: the memory op uses the value wrapped to 64
            // bits (exact for any magnitude, incl. the Big tier); `store`
            // still RETURNS the unwrapped ToBigInt value.
            let v64 = v_b.to_i64_wrap();
            if let Some((mem, off)) = sab_target {
                if off + elem_size <= mem.capacity() {
                    let repl = if op == "compareExchange" {
                        self.to_bigint(args.get(3).copied().unwrap_or(Value::UNDEFINED))?
                            .to_i64_wrap()
                    } else {
                        0
                    };
                    let old = sab_atomic_op(&mem, kind, off, op, v64, repl);
                    let old_b: i128 = if kind == 9 {
                        old as i128
                    } else {
                        (old as u64) as i128
                    };
                    return Ok(if op == "store" {
                        self.make_bigint_val(v_b)
                    } else {
                        self.make_bigint(old_b)
                    });
                }
            }
            let cur = self.ta_element_get(ti, i);
            // The element value always fits i64/u64, so this is exact.
            let old = self.to_bigint(cur)?.to_i128_sat();
            match op {
                "load" => Ok(self.make_bigint(old)),
                "store" => {
                    let nv = self.make_bigint_val(v_b.clone());
                    self.ta_element_set(ti, i, nv)?;
                    Ok(self.make_bigint_val(v_b))
                }
                "compareExchange" => {
                    let repl = self.to_bigint(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
                    // NumericToRawBytes: the EXPECTED value compares as the
                    // element type (BigInt64 wraps to i64, BigUint64 to u64).
                    let expected = if kind == 9 {
                        v64 as i128
                    } else {
                        (v64 as u64) as i128
                    };
                    if old == expected {
                        let nv = self.make_bigint_val(repl);
                        self.ta_element_set(ti, i, nv)?;
                    }
                    Ok(self.make_bigint(old))
                }
                _ => {
                    // Mod-2^64 arithmetic: the wrapped operand is equivalent
                    // (ta_element_set wraps the result to the element type).
                    let v_in = v64 as i128;
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
            let v_in = if op == "load" {
                0
            } else {
                self.to_integer_or_zero(a2)?
            };
            if let Some((mem, off)) = sab_target {
                if off + elem_size <= mem.capacity() {
                    let repl = if op == "compareExchange" {
                        self.to_integer_or_zero(args.get(3).copied().unwrap_or(Value::UNDEFINED))?
                    } else {
                        0
                    };
                    let old = sab_atomic_op(&mem, kind, off, op, v_in, repl);
                    // The operand wraps to the element type inside the memory
                    // op (NumericToRawBytes); `store` still RETURNS the
                    // unwrapped ToIntegerOrInfinity value.
                    return Ok(if op == "store" {
                        Value::num(v_in as f64)
                    } else {
                        Value::num(old as f64)
                    });
                }
            }
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
                    // NumericToRawBytes: the EXPECTED value wraps to the
                    // element type before comparing (123456789 on an
                    // Int16Array compares as -13035).
                    let expected = match kind {
                        0 => v_in as i8 as i64,
                        1 | 2 => v_in as u8 as i64,
                        3 => v_in as i16 as i64,
                        4 => v_in as u16 as i64,
                        5 => v_in as i32 as i64,
                        6 => v_in as u32 as i64,
                        _ => v_in,
                    };
                    if old_i == expected {
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
            // InitializeTypedArrayFromArrayBuffer order: the offset-alignment
            // RangeError precedes the length coercion, and the detached-buffer
            // TypeError (the buffer may have been detached at entry, or by a
            // ToIndex valueOf above) precedes the byte-length RangeErrors --
            // detaching clears the data, which would otherwise mask it.
            if byte_offset % size != 0 {
                return Err(Thrown(
                    "RangeError: invalid TypedArray length/offset".into(),
                ));
            }
            let explicit: Option<usize> = match args.get(2) {
                Some(&v) if v != Value::UNDEFINED => Some(self.to_index(v)?),
                _ => None,
            };
            if matches!(
                self.heap.get(buf),
                HeapObj::ArrayBuffer { detached: true, .. }
            ) {
                return Err(Thrown(
                    "TypeError: Cannot construct a TypedArray on a detached ArrayBuffer".into(),
                ));
            }
            let buf_len = self.array_buffer_len(buf);
            // A length-tracking view: no explicit length on a resizable buffer.
            let tracking = explicit.is_none() && self.ab_max.contains_key(&buf);
            let length = match explicit {
                Some(l) => l,
                None => {
                    // A length-tracking view FLOORS over the buffer (its length
                    // follows resizes), so only a fixed auto-length view requires
                    // the remaining bytes to divide evenly.
                    if buf_len < byte_offset || (!tracking && (buf_len - byte_offset) % size != 0) {
                        return Err(Thrown(
                            "RangeError: byte length not a multiple of element size".into(),
                        ));
                    }
                    (buf_len - byte_offset) / size
                }
            };
            let end = length
                .checked_mul(size)
                .and_then(|n| byte_offset.checked_add(n))
                .ok_or_else(|| Thrown("RangeError: invalid TypedArray length/offset".into()))?;
            if end > buf_len {
                return Err(Thrown(
                    "RangeError: invalid TypedArray length/offset".into(),
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
                let src_kind = match self.heap.get(src_ta) {
                    HeapObj::TypedArray { kind, .. } => *kind,
                    _ => 0,
                };
                // A BigInt<->Number content-type mismatch is a TypeError, and the
                // source length is its EFFECTIVE length (a detached/out-of-bounds
                // view rejects; a length-tracking view follows its buffer).
                if native::TA_KINDS[src_kind as usize].2 != native::TA_KINDS[kind as usize].2 {
                    return Err(Thrown(
                        "TypeError: Cannot construct a TypedArray from a source of a different content type".into(),
                    ));
                }
                let len = self.ta_effective_len(src_ta).ok_or_else(|| {
                    Thrown(
                        "TypeError: Cannot construct a TypedArray from an out-of-bounds or detached source".into(),
                    )
                })?;
                self.preflight_native_iteration_work(len as u64)?;
                (0..len).map(|i| self.ta_element_get(src_ta, i)).collect()
            } else {
                // A custom iterable (callable `@@iterator`) is iterated; anything
                // else is treated as ARRAY-LIKE per InitializeTypedArrayFromArrayLike
                // (read ToLength(`length`), then indices 0..length). The length read
                // and each element read propagate abrupt completions.
                let it = self.get_prop(a0, "@@iterator")?;
                // GetMethod: a defined non-callable @@iterator is a TypeError;
                // undefined/null take the array-like path.
                if it != Value::UNDEFINED && it != Value::NULL && !self.is_callable(it) {
                    return Err(Thrown(
                        "TypeError: object is not iterable ([Symbol.iterator] is not a function)"
                            .into(),
                    ));
                }
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
                    self.preflight_native_iteration_work(n as u64)?;
                    let mut v = Vec::with_capacity(n);
                    for i in 0..n {
                        v.push(self.get_index(a0, Value::int(i as i32))?);
                    }
                    v
                }
            };
            let len = src.len();
            let buf = self.alloc_array_buffer(len * size)?;
            let ta = self.alloc_typed_array(buf, kind, 0, len);
            for (i, v) in src.into_iter().enumerate() {
                self.ta_element_set(ta.heap_index(), i, v)?;
            }
            return Ok(ta);
        }
        // new TA(length): ToIndex (undefined/NaN -> 0, fractional truncates,
        // negative/too-large -> RangeError, Symbol/BigInt -> TypeError).
        let length = if a0 == Value::UNDEFINED {
            0
        } else {
            self.to_index(a0)?
        };
        if length > (MAX_ARRAY_BUFFER_LEN / size as i64) as usize {
            return Err(Thrown(
                "RangeError: typed array length exceeds the maximum".into(),
            ));
        }
        let buf = self.alloc_array_buffer(length * size)?;
        Ok(self.alloc_typed_array(buf, kind, 0, length))
    }

    /// Strict ToIndex (the immutable-ArrayBuffer methods): ToNumber rejects
    /// BigInt/Symbol with TypeError (the lenient to_number converts BigInt);
    /// NaN -> 0; truncates; negative or beyond 2^53-1 -> RangeError.
    pub(crate) fn to_index_strict(&mut self, v: Value) -> Result<usize, Thrown> {
        let n = self.to_number_strict(v)?;
        let n = if n.is_nan() { 0.0 } else { n.trunc() };
        if n < 0.0 || n > 9007199254740991.0 {
            return Err(Thrown("RangeError: index out of range".into()));
        }
        Ok(n as usize)
    }

    /// ta_rel_index with strict ToNumber (BigInt/Symbol -> TypeError): resolve a
    /// relative start/end argument against `len` with the negative-from-end clamp.
    pub(crate) fn ta_rel_index_strict(
        &mut self,
        v: Value,
        default: usize,
        len: usize,
    ) -> Result<usize, Thrown> {
        if v == Value::UNDEFINED {
            return Ok(default);
        }
        let n = self.to_number_strict(v)?;
        let n = if n.is_nan() { 0.0 } else { n.trunc() };
        Ok(if n < 0.0 {
            ((len as f64) + n).max(0.0) as usize
        } else {
            (n as usize).min(len)
        })
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
        // ToIndex(byteOffset) precedes the detached check (its valueOf runs
        // exactly once, and may itself detach), which precedes the bounds checks.
        if matches!(
            self.heap.get(buf),
            HeapObj::ArrayBuffer { detached: true, .. }
        ) {
            return Err(Thrown(
                "TypeError: Cannot construct a DataView on a detached ArrayBuffer".into(),
            ));
        }
        if byte_offset > buf_len {
            return Err(Thrown("RangeError: invalid DataView offset".into()));
        }
        let auto_length = matches!(args.get(2), None | Some(&Value::UNDEFINED));
        let byte_length = if auto_length {
            buf_len.saturating_sub(byte_offset)
        } else {
            self.to_index(args[2])?
        };
        if byte_offset + byte_length > buf_len {
            return Err(Thrown("RangeError: invalid DataView offset/length".into()));
        }
        let idx = self.heap.alloc(HeapObj::DataView {
            buffer: buf,
            byte_offset,
            byte_length,
        });
        if self.dataview_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.dataview_proto));
        }
        // An auto-length DataView (no explicit byteLength) over a resizable /
        // growable buffer tracks the buffer's current size (byteLength follows it,
        // and byteLength/byteOffset throw once the offset is out of bounds).
        if auto_length && self.ab_max.contains_key(&buf) {
            self.dv_tracking.insert(idx);
        }
        Ok(Value::heap(idx))
    }
}

/// One `Atomics.<op>` data access on a truly-shared (SAB-backed) element,
/// performed with REAL atomic instructions (SeqCst everywhere) so concurrent
/// agent threads observe it atomically. `off` is the element's absolute byte
/// offset within `mem`; it is element-size aligned (a TypedArray's
/// `byteOffset` is element-aligned by construction and the SharedMem base is
/// 8-byte aligned). `v`/`repl` are the already-coerced operand(s) — they wrap
/// to the element type here, per NumericToRawBytes. Returns the OLD element
/// value (current value for `load`), sign/zero-extended to `i64` per the
/// element kind; the `store` return is unused (callers return the input).
#[cfg(not(feature = "safe-sandbox"))]
fn sab_atomic_op(
    mem: &crate::heap::SharedMem,
    kind: u8,
    off: usize,
    op: &str,
    v: i64,
    repl: i64,
) -> i64 {
    use std::sync::atomic::Ordering::SeqCst;
    use std::sync::atomic::*;
    macro_rules! go {
        ($aty:ty, $ity:ty) => {{
            // SAFETY: `off + size <= capacity` (caller-checked), the pointer is
            // element-aligned (see fn doc), and the allocation is fixed for the
            // life of the Arc — so this reference is valid; concurrent access
            // from other agent threads is exactly what the atomic type permits.
            let a = unsafe { &*(mem.base_ptr().add(off) as *const $aty) };
            let vv = v as $ity;
            let old: $ity = match op {
                "load" => a.load(SeqCst),
                "store" => {
                    a.store(vv, SeqCst);
                    vv
                }
                "add" => a.fetch_add(vv, SeqCst),
                "sub" => a.fetch_sub(vv, SeqCst),
                "and" => a.fetch_and(vv, SeqCst),
                "or" => a.fetch_or(vv, SeqCst),
                "xor" => a.fetch_xor(vv, SeqCst),
                "exchange" => a.swap(vv, SeqCst),
                "compareExchange" => match a.compare_exchange(vv, repl as $ity, SeqCst, SeqCst) {
                    Ok(o) | Err(o) => o,
                },
                _ => a.load(SeqCst),
            };
            old as i64
        }};
    }
    match kind {
        0 => go!(AtomicI8, i8),
        // Kind 2 (Uint8Clamped) never reaches Atomics (rejected in validation).
        1 | 2 => go!(AtomicU8, u8),
        3 => go!(AtomicI16, i16),
        4 => go!(AtomicU16, u16),
        5 => go!(AtomicI32, i32),
        6 => go!(AtomicU32, u32),
        9 => go!(AtomicI64, i64),
        _ => go!(AtomicU64, u64),
    }
}

#[cfg(feature = "safe-sandbox")]
fn sab_atomic_op(
    _mem: &crate::heap::SharedMem,
    _kind: u8,
    _off: usize,
    _op: &str,
    _v: i64,
    _repl: i64,
) -> i64 {
    // SharedArrayBuffer and Atomics are absent from the hardened realm. This
    // keeps internal exhaustive call paths type-correct without exposing raw
    // shared-memory operations to untrusted code.
    0
}
