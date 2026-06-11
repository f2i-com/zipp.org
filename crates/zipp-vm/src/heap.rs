//! Heap object storage.
//!
//! Heap values are referenced by a `u32` index packed into a [`crate::value::Value`].
//! Reference semantics fall out naturally: copying a `Value` copies the index,
//! so `let b = a` makes `a` and `b` alias the same heap slot, and a mutation
//! through either is visible through both — exactly JS object/array semantics.
//!
//! v1 does not reclaim memory (programs are short-lived per `eval`); a real GC
//! slots in here later without touching the value representation. Objects use a
//! simple insertion-ordered property list, which preserves JS string-key
//! enumeration order and is correct (if not yet fast — shapes/inline-caches are
//! a later tier).

use crate::value::Value;
use std::borrow::Cow;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A JS object: insertion-ordered string-keyed properties.
#[derive(Clone, Debug, Default)]
pub struct ObjMap {
    pub keys: Vec<String>,
    pub vals: Vec<Value>,
    /// Per-property attributes, parallel to `keys`/`vals` (a property descriptor's
    /// writable/enumerable/configurable + accessor get/set). For a DATA property
    /// `vals[i]` is the value; for an ACCESSOR `vals[i]` is the getter and
    /// `attrs[i].setter` the setter.
    pub attrs: Vec<PropAttr>,
    /// Heap index of the class this object is an instance of (`new C()`), used
    /// for prototype-style method lookup and `instanceof`. `None` for a plain
    /// object literal. Own properties (the fields) live in `keys`/`vals`;
    /// methods are resolved through the class, so they stay non-enumerable.
    pub class: Option<u32>,
    /// `[[Extensible]]`: whether new own properties may be added. Cleared by
    /// `Object.preventExtensions`/`seal`/`freeze`. Default `true`.
    pub extensible: bool,
    /// True for the built-in constructor globals (Object/Array/Map/…), which are
    /// modelled as objects but are callable constructors: `typeof` reports
    /// "function" and they satisfy IsConstructor. False for ordinary objects and
    /// the namespace globals (Reflect/Math/JSON).
    pub is_ctor: bool,
    /// `[[IsRawJSON]]`: set only on the frozen objects returned by
    /// `JSON.rawJSON`. `JSON.isRawJSON` reports it, and `JSON.stringify` emits
    /// the object's `"rawJSON"` string property verbatim instead of serialising.
    pub is_raw_json: bool,
    /// Explicit Object.seal / Object.freeze markers. For a PLAIN object the
    /// per-property `attrs` already encode sealed/frozen, but an exotic object
    /// whose elements live OUTSIDE this map (a dense Array's Vec, a TypedArray's
    /// buffer) has no per-element attrs, so seal/freeze on it is recorded here.
    pub sealed: bool,
    pub frozen: bool,
}

/// One property's attributes — the ECMAScript property-descriptor flags plus an
/// accessor pair. A data property uses `writable` and the parallel `vals` entry;
/// an accessor (`accessor == true`) uses `vals[i]` as the getter and `setter`.
#[derive(Clone, Copy, Debug)]
pub struct PropAttr {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
    pub accessor: bool,
    /// The setter function for an accessor property (`UNDEFINED` if none / data).
    pub setter: Value,
}

impl PropAttr {
    /// The default attributes for an ordinary created property (`obj.x = v`,
    /// object literals): a writable, enumerable, configurable data property.
    pub fn data() -> PropAttr {
        PropAttr { writable: true, enumerable: true, configurable: true, accessor: false, setter: Value::UNDEFINED }
    }
}

impl ObjMap {
    pub fn new() -> ObjMap {
        ObjMap {
            keys: Vec::new(),
            vals: Vec::new(),
            attrs: Vec::new(),
            class: None,
            extensible: true,
            is_ctor: false,
            is_raw_json: false,
            sealed: false,
            frozen: false,
        }
    }

    /// `Object.isSealed`: not extensible and every own property non-configurable.
    pub fn is_sealed(&self) -> bool {
        !self.extensible && self.attrs.iter().all(|a| !a.configurable)
    }

    /// `Object.isFrozen`: sealed and every own data property non-writable.
    pub fn is_frozen(&self) -> bool {
        !self.extensible && self.attrs.iter().all(|a| !a.configurable && (a.accessor || !a.writable))
    }

    /// `Object.seal`: clear extensibility and make every own property non-configurable.
    pub fn seal(&mut self) {
        self.extensible = false;
        self.sealed = true;
        for a in &mut self.attrs {
            a.configurable = false;
        }
    }

    /// `Object.freeze`: seal, and make every own data property non-writable too.
    pub fn freeze(&mut self) {
        self.extensible = false;
        self.sealed = true;
        self.frozen = true;
        for a in &mut self.attrs {
            a.configurable = false;
            if !a.accessor {
                a.writable = false;
            }
        }
    }

    pub fn pos(&self, key: &str) -> Option<usize> {
        self.keys.iter().position(|k| k == key)
    }

    /// The raw stored value for `key` (a data value, or an accessor's getter).
    /// Callers that must honour accessors check `attrs[i].accessor` first.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.pos(key).map(|i| self.vals[i])
    }

    /// Set `key = val` as a DATA property. Returns `true` if a NEW key was
    /// appended (which may have reallocated `vals`), `false` if an existing slot
    /// was overwritten. New keys get default data attributes; existing keys keep
    /// their attributes (only the value changes). The JIT inline cache uses the
    /// return to bump the object's version on a key-add.
    pub fn set(&mut self, key: &str, val: Value) -> bool {
        if let Some(i) = self.pos(key) {
            self.vals[i] = val;
            false
        } else {
            self.keys.push(key.to_string());
            self.vals.push(val);
            self.attrs.push(PropAttr::data());
            true
        }
    }

    /// Define `key` with explicit attributes (`Object.defineProperty`, or a method
    /// with non-default enumerability). Overwrites any existing slot. Returns
    /// `true` if a new key was appended.
    pub fn define(&mut self, key: &str, val: Value, attr: PropAttr) -> bool {
        if let Some(i) = self.pos(key) {
            self.vals[i] = val;
            self.attrs[i] = attr;
            false
        } else {
            self.keys.push(key.to_string());
            self.vals.push(val);
            self.attrs.push(attr);
            true
        }
    }

    /// Remove `key`'s own property; returns whether it existed. Shifts later
    /// slots, so the caller MUST bump the object's version (a JIT inline cache
    /// may have recorded a now-stale slot index for another key).
    pub fn remove(&mut self, key: &str) -> bool {
        if let Some(i) = self.pos(key) {
            self.keys.remove(i);
            self.vals.remove(i);
            self.attrs.remove(i);
            true
        } else {
            false
        }
    }
}

/// A flat (contiguous) JS string with cached metadata so `.length` and indexing
/// are O(1) for the common all-ASCII case. `bytes` holds WTF-8: well-formed
/// UTF-8 for the overwhelmingly common case, PLUS lone surrogates (which JS
/// strings can contain but UTF-8 prohibits) encoded as the 3-byte sequence
/// UTF-8 *would* use for their code point: `0xED 0xA0-0xBF 0x80-0xBF` (high
/// halves `0xED 0xA0-0xAF ..`, low halves `0xED 0xB0-0xBF ..`). The buffer is
/// kept CANONICAL — an encoded high surrogate is never immediately followed by
/// an encoded low surrogate (that pair is always stored as the astral scalar's
/// 4-byte encoding; see `wtf8_push`/`wtf8_push_cp`) — so byte equality remains
/// content equality. The fields are PRIVATE: every access funnels through the
/// accessors below, which decode WTF-8 (never `from_utf8_unchecked` over
/// surrogate bytes — that would be UB through `str`'s validity invariant).
/// `units` caches the length in UTF-16 CODE UNITS — the measure of every
/// JS-observable string position (`.length`, `charCodeAt`, `slice`, …); a lone
/// surrogate is 1 unit, an astral scalar 2. `ascii` flags the all-ASCII case,
/// where the i-th unit is the i-th byte — O(1) random access. `wellformed`
/// (no lone surrogates ⇔ the bytes are valid UTF-8) is computed once at
/// construction; only well-formed strings may be viewed as `&str`.
#[derive(Clone, Debug)]
pub struct JsStr {
    bytes: Vec<u8>,
    units: usize,
    ascii: bool,
    wellformed: bool,
}

/// UTF-16 code units contributed by one Unicode scalar: 1 for BMP, 2 for an
/// astral (supplementary-plane) scalar. This is THE unit/scalar switch — every
/// positional helper below counts through it.
#[inline]
pub fn char_units(c: char) -> usize {
    c.len_utf16()
}

/// UTF-16 code-unit length of a well-formed `&str`.
pub fn str_units(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().map(char_units).sum()
    }
}

/// Unit position of char-boundary byte offset `b` in `s` (clamped to the end).
pub fn byte_to_units(s: &str, b: usize) -> usize {
    str_units(&s[..b.min(s.len())])
}

/// Resolve unit position `u` in `s` to byte offsets, clamped to the end:
/// `(floor, ceil)` are equal at a scalar boundary; a `u` that lands BETWEEN the
/// halves of a surrogate pair gives the enclosing astral scalar's (start, end).
pub fn unit_byte_bounds(s: &str, u: usize) -> (usize, usize) {
    if u == 0 {
        return (0, 0);
    }
    let mut units = 0usize;
    for (b, c) in s.char_indices() {
        if units == u {
            return (b, b);
        }
        let n = char_units(c);
        if units + n > u {
            // `u` addresses this scalar's trail half (only possible when n == 2).
            return (b, b + c.len_utf8());
        }
        units += n;
    }
    (s.len(), s.len())
}

/// Byte offset of unit position `u`, rounding a mid-pair position UP to the next
/// scalar boundary — exact for SEARCH-START positions (a well-formed needle can
/// never match starting at a trail unit). Anchored positions (`startsWith`/
/// `endsWith`/`lastIndexOf` caps) use `unit_byte_bounds` to detect the split.
pub fn unit_to_byte(s: &str, u: usize) -> usize {
    unit_byte_bounds(s, u).1
}

// ── WTF-8 primitives ──
// The byte-level helpers every accessor builds on. All of them treat the
// surrogate range exactly like any other 3-byte sequence; none of them ever
// construct a `&str` over the bytes.

/// UTF-16 code units contributed by code point `cp` (1 for BMP — including a
/// lone surrogate — 2 for an astral scalar).
#[inline]
pub fn cp_units(cp: u32) -> usize {
    if cp < 0x10000 { 1 } else { 2 }
}

/// The `off`-th UTF-16 unit of code point `cp` (`off` 0 = the code point
/// itself / the high surrogate; 1 = the low surrogate of an astral scalar).
#[inline]
fn unit_of_cp(cp: u32, off: usize) -> u16 {
    if cp < 0x10000 {
        cp as u16
    } else {
        let v = cp - 0x10000;
        if off == 0 {
            0xD800 | (v >> 10) as u16
        } else {
            0xDC00 | (v & 0x3FF) as u16
        }
    }
}

/// Decode the code point whose encoding starts at byte `i` of WTF-8 buffer `b`
/// (which must be a lead position of a valid sequence — the engine only builds
/// valid WTF-8). Returns `(code point, encoded byte length)`. A surrogate
/// decodes like any 3-byte sequence — the one place WTF-8 differs from UTF-8.
#[inline]
pub fn wtf8_decode(b: &[u8], i: usize) -> (u32, usize) {
    let b0 = b[i] as u32;
    if b0 < 0x80 {
        (b0, 1)
    } else if b0 < 0xE0 {
        (((b0 & 0x1F) << 6) | (b[i + 1] as u32 & 0x3F), 2)
    } else if b0 < 0xF0 {
        ((((b0 & 0x0F) << 12) | ((b[i + 1] as u32 & 0x3F) << 6)) | (b[i + 2] as u32 & 0x3F), 3)
    } else {
        (
            ((b0 & 0x07) << 18)
                | ((b[i + 1] as u32 & 0x3F) << 12)
                | ((b[i + 2] as u32 & 0x3F) << 6)
                | (b[i + 3] as u32 & 0x3F),
            4,
        )
    }
}

/// UTF-16 unit count of a WTF-8 buffer: one per lead byte, plus one extra per
/// 4-byte (astral) sequence. A 3-byte lone-surrogate encoding counts 1.
pub fn wtf8_units(b: &[u8]) -> usize {
    b.iter().map(|&x| ((x & 0xC0) != 0x80) as usize + (x >= 0xF0) as usize).sum()
}

/// Whether WTF-8 buffer `b` contains NO surrogate encodings — for engine-built
/// buffers this is exactly "the bytes are valid UTF-8".
pub fn wtf8_is_wellformed(b: &[u8]) -> bool {
    !b.windows(2).any(|w| w[0] == 0xED && (0xA0..=0xBF).contains(&w[1]))
}

/// Raw WTF-8 encode of `cp` (a surrogate allowed) onto `out` — NO seam
/// canonicalization (use `wtf8_push_cp` when `cp` may be a low surrogate
/// completing a trailing high).
fn push_cp_raw(out: &mut Vec<u8>, cp: u32) {
    if cp < 0x80 {
        out.push(cp as u8);
    } else if cp < 0x800 {
        out.push(0xC0 | (cp >> 6) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else if cp < 0x10000 {
        out.push(0xE0 | (cp >> 12) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else {
        out.push(0xF0 | (cp >> 18) as u8);
        out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    }
}

/// Push code point `cp` (a surrogate allowed) onto WTF-8 buffer `out`,
/// CANONICALIZING: a low surrogate that completes a trailing high surrogate
/// merges into the astral scalar's 4-byte encoding — exactly the JS rule that
/// `'\uD800' + '\uDC00'` is the 1-code-point string `'\u{10000}'`.
pub fn wtf8_push_cp(out: &mut Vec<u8>, cp: u32) {
    if (0xDC00..=0xDFFF).contains(&cp) {
        let n = out.len();
        if n >= 3 && out[n - 3] == 0xED && (0xA0..=0xAF).contains(&out[n - 2]) {
            let (hi, _) = wtf8_decode(out, n - 3);
            out.truncate(n - 3);
            push_cp_raw(out, 0x10000 + ((hi - 0xD800) << 10) + (cp - 0xDC00));
            return;
        }
    }
    push_cp_raw(out, cp);
}

/// Append WTF-8 `seg` onto WTF-8 `out`, canonicalizing the SEAM: a trailing
/// high surrogate in `out` followed by a leading low surrogate in `seg` merges
/// into the astral 4-byte encoding (unit count is unaffected: 1+1 halves = the
/// astral scalar's 2 units, so rope length math stays additive). The common
/// case bails on one byte compare (an ASCII tail can't end a surrogate).
pub fn wtf8_push(out: &mut Vec<u8>, seg: &[u8]) {
    let n = out.len();
    if n >= 3
        && seg.len() >= 3
        && *out.last().unwrap() >= 0x80
        && out[n - 3] == 0xED
        && (0xA0..=0xAF).contains(&out[n - 2])
        && seg[0] == 0xED
        && (0xB0..=0xBF).contains(&seg[1])
    {
        let (hi, _) = wtf8_decode(out, n - 3);
        let (lo, _) = wtf8_decode(seg, 0);
        out.truncate(n - 3);
        push_cp_raw(out, 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
        out.extend_from_slice(&seg[3..]);
        return;
    }
    out.extend_from_slice(seg);
}

/// Iterate the UTF-16 code units of a WTF-8 buffer: BMP code points (including
/// lone surrogates) yield their own value; an astral scalar yields its two
/// halves. This is the UTF-16 view every JS string comparison/order is defined
/// over.
pub fn wtf8_units_iter(b: &[u8]) -> impl Iterator<Item = u16> + '_ {
    let mut i = 0usize;
    let mut low: Option<u16> = None;
    std::iter::from_fn(move || {
        if let Some(u) = low.take() {
            return Some(u);
        }
        if i >= b.len() {
            return None;
        }
        let (cp, len) = wtf8_decode(b, i);
        i += len;
        if cp >= 0x10000 {
            low = Some(unit_of_cp(cp, 1));
            Some(unit_of_cp(cp, 0))
        } else {
            Some(cp as u16)
        }
    })
}

/// Iterate the code points of a WTF-8 buffer (a lone surrogate yields its
/// surrogate value 0xD800–0xDFFF — NOT a `char`, which can't hold it).
pub fn wtf8_code_points(b: &[u8]) -> impl Iterator<Item = u32> + '_ {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        if i >= b.len() {
            return None;
        }
        let (cp, len) = wtf8_decode(b, i);
        i += len;
        Some(cp)
    })
}

/// Owned LOSSY `String` of a WTF-8 buffer: each lone-surrogate triple becomes
/// U+FFFD. Both encodings are 3 bytes, so byte offsets and unit positions in
/// the lossy form are IDENTICAL to the exact form — position math computed on
/// the lossy view remains valid for the WTF-8 original.
pub fn wtf8_to_lossy_string(b: &[u8]) -> String {
    wtf8_into_lossy_string(b.to_vec())
}

/// Decode oxc's lone-surrogate marker encoding into WTF-8 bytes. The parser
/// cooks a string/template literal containing lone-surrogate escapes (e.g.
/// `'\uD800'`) into text where each lone surrogate is the 5-char marker
/// `\u{FFFD}XXXX` (4 lowercase hex = the code unit) and a LITERAL U+FFFD is
/// `\u{FFFD}fffd`, setting `.lone_surrogates` on the AST node. Only flagged
/// literals are decoded (an unflagged string may contain genuine U+FFFD +
/// hex-looking text). Output is canonical WTF-8 via `wtf8_push_cp`.
pub fn decode_lone_surrogate_markers(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\u{FFFD}' {
            let rest = it.as_str();
            if rest.len() >= 4 && rest.as_bytes()[..4].iter().all(|b| b.is_ascii_hexdigit()) {
                let cu = u32::from_str_radix(&rest[..4], 16).unwrap();
                for _ in 0..4 {
                    it.next();
                }
                wtf8_push_cp(&mut out, cu);
                continue;
            }
            // Defensive: an unmarked U+FFFD (oxc escapes them all when the
            // flag is set) passes through literally.
        }
        wtf8_push_cp(&mut out, c as u32);
    }
    out
}

/// Inverse of [`decode_lone_surrogate_markers`]: encode WTF-8 bytes into the
/// oxc lone-surrogate MARKER form — each lone surrogate becomes the 5-char
/// marker `\u{FFFD}XXXX` (4 lowercase hex = the code unit) and a LITERAL
/// U+FFFD becomes `\u{FFFD}fffd` (so an unmarked U+FFFD can never be
/// mistaken for a marker). Used when the compiler recovers exact pattern
/// bytes for a regex literal whose lossy parse text contained U+FFFD: the
/// result feeds `add_string_const_wtf8`, and `resolve_const`'s decode
/// round-trips it back to these exact bytes.
pub fn encode_lone_surrogate_markers(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() + 8);
    for cp in wtf8_code_points(b) {
        match char::from_u32(cp) {
            // A lone surrogate (no `char` exists for it) → marker.
            None => {
                out.push('\u{FFFD}');
                out.push_str(&format!("{cp:04x}"));
            }
            Some('\u{FFFD}') => out.push_str("\u{FFFD}fffd"),
            Some(c) => out.push(c),
        }
    }
    out
}

/// Consuming form of [`wtf8_to_lossy_string`] (patches the buffer in place).
pub fn wtf8_into_lossy_string(mut v: Vec<u8>) -> String {
    let mut i = 0;
    while i + 2 < v.len() {
        if v[i] == 0xED && (0xA0..=0xBF).contains(&v[i + 1]) {
            // U+FFFD's UTF-8 encoding, also 3 bytes.
            v[i] = 0xEF;
            v[i + 1] = 0xBF;
            v[i + 2] = 0xBD;
            i += 3;
        } else {
            i += 1;
        }
    }
    // Engine-built buffers are valid UTF-8 after the patch; degrade any
    // unexpected residue through the checked lossy path rather than UB.
    match String::from_utf8(v) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(&e.into_bytes()).into_owned(),
    }
}

impl JsStr {
    /// Construct from a (necessarily well-formed) Rust `String` — the common
    /// path for every string the engine builds out of `&str` material.
    pub fn new(bytes: String) -> JsStr {
        let ascii = bytes.is_ascii();
        let units = if ascii { bytes.len() } else { str_units(&bytes) };
        JsStr { bytes: bytes.into_bytes(), units, ascii, wellformed: true }
    }

    /// Construct from WTF-8 bytes (the creation sites that can produce lone
    /// surrogates: literal marker decode, fromCharCode/fromCodePoint,
    /// JSON.parse, slicing, rope flattening). The buffer must be valid,
    /// CANONICAL WTF-8 — every producer in the engine builds it through
    /// `wtf8_push`/`wtf8_push_cp`/`slice_units`, which maintain that.
    pub fn from_wtf8(bytes: Vec<u8>) -> JsStr {
        if bytes.is_ascii() {
            return JsStr { units: bytes.len(), bytes, ascii: true, wellformed: true };
        }
        let wellformed = wtf8_is_wellformed(&bytes);
        debug_assert!(
            !wellformed || std::str::from_utf8(&bytes).is_ok(),
            "from_wtf8: surrogate-free buffer must be valid UTF-8"
        );
        debug_assert!(
            {
                // Canonical form: no encoded high surrogate immediately
                // followed by an encoded low surrogate.
                !bytes.windows(6).any(|w| {
                    w[0] == 0xED
                        && (0xA0..=0xAF).contains(&w[1])
                        && w[3] == 0xED
                        && (0xB0..=0xBF).contains(&w[4])
                })
            },
            "from_wtf8: non-canonical surrogate pair encoding"
        );
        let units = wtf8_units(&bytes);
        JsStr { bytes, units, ascii: false, wellformed }
    }

    /// A 1-code-point string (`cp` may be a lone surrogate).
    pub fn from_code_point(cp: u32) -> JsStr {
        let mut v = Vec::with_capacity(4);
        push_cp_raw(&mut v, cp);
        JsStr::from_wtf8(v)
    }

    /// The content as `&str` — ONLY for well-formed strings (the type's
    /// validity invariant forbids surrogate bytes). Callers that can see a
    /// lone-surrogate string use `as_str_lossy` (observation paths: display,
    /// parsing, regex input, …) or the WTF-8 accessors (exact paths).
    /// Panics on a non-well-formed string — every call site must guarantee
    /// well-formedness (e.g. just constructed from `&str` material).
    #[allow(dead_code)]
    #[inline]
    pub fn as_str_wf(&self) -> &str {
        assert!(self.wellformed, "as_str_wf on a string containing lone surrogates");
        // SAFETY: `wellformed` records that `bytes` holds no surrogate
        // encodings; the bytes otherwise originate from safe `String`s or the
        // engine's WTF-8 encoders, so they are valid UTF-8 (checked by
        // `from_wtf8`'s debug assertion).
        unsafe { std::str::from_utf8_unchecked(&self.bytes) }
    }

    /// The content as `&str`, LOSSY for the lone-surrogate case (each lone
    /// surrogate reads as U+FFFD — same byte length, so positions computed on
    /// the lossy view remain valid for the exact bytes). Borrowed (free) for
    /// well-formed strings — the overwhelmingly common case.
    #[inline]
    pub fn as_str_lossy(&self) -> Cow<'_, str> {
        if self.wellformed {
            // SAFETY: as in `as_str_wf` — `wellformed` ⇒ valid UTF-8.
            Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&self.bytes) })
        } else {
            Cow::Owned(wtf8_to_lossy_string(&self.bytes))
        }
    }

    /// Owned lossy `String` (see `as_str_lossy`).
    pub fn to_lossy_string(&self) -> String {
        self.as_str_lossy().into_owned()
    }

    /// The raw WTF-8 bytes. NOT necessarily valid UTF-8 — never view them as
    /// `&str`; decode with the `wtf8_*` helpers.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Length in UTF-16 code units — the JS `.length`.
    #[inline]
    pub fn units(&self) -> usize {
        self.units
    }

    #[inline]
    pub fn is_ascii(&self) -> bool {
        self.ascii
    }

    /// No lone surrogates (`String.prototype.isWellFormed`) — O(1), computed
    /// at construction.
    #[inline]
    pub fn is_wellformed(&self) -> bool {
        self.wellformed
    }

    /// Append one ASCII byte (the `s += digit` fast path), updating metadata.
    #[inline]
    pub fn push_ascii(&mut self, b: u8) {
        debug_assert!(b < 128);
        self.bytes.push(b);
        self.units += 1;
    }

    /// Append a well-formed string, updating the cached metadata. No seam
    /// canonicalization is needed: a `&str` can never START with a low
    /// surrogate, so a trailing high surrogate in `self` stays lone.
    /// (Currently unreferenced — the append path goes through `push_wtf8` —
    /// kept as the natural `&str` entry of the accessor layer.)
    #[allow(dead_code)]
    pub fn push_str(&mut self, add: &str) {
        self.units += str_units(add);
        self.ascii &= add.is_ascii();
        self.bytes.extend_from_slice(add.as_bytes());
    }

    /// Append WTF-8 bytes (an exact `+=` of another string's content),
    /// canonicalizing the seam. Unit length stays additive across the merge.
    pub fn push_wtf8(&mut self, add: &[u8]) {
        let add_ascii = add.is_ascii();
        self.units += if add_ascii { add.len() } else { wtf8_units(add) };
        if self.wellformed && (add_ascii || wtf8_is_wellformed(add)) {
            // Surrogate-free on both sides: plain append, no seam possible.
            self.ascii &= add_ascii;
            self.bytes.extend_from_slice(add);
        } else {
            self.ascii = false;
            wtf8_push(&mut self.bytes, add);
            self.wellformed = wtf8_is_wellformed(&self.bytes);
        }
    }

    /// Locate unit position `i`: the code point containing it and `i`'s offset
    /// within that code point's units (0 = lead, 1 = the trail of an astral
    /// pair). O(1) for ASCII, O(i) otherwise.
    fn locate_unit(&self, i: usize) -> Option<(u32, usize)> {
        if self.ascii {
            return self.bytes.get(i).map(|&b| (b as u32, 0));
        }
        if i >= self.units {
            return None;
        }
        let mut pos = 0usize;
        let mut bi = 0usize;
        while bi < self.bytes.len() {
            let (cp, blen) = wtf8_decode(&self.bytes, bi);
            let n = cp_units(cp);
            if i < pos + n {
                return Some((cp, i - pos));
            }
            pos += n;
            bi += blen;
        }
        None
    }

    /// The UTF-16 code unit at unit position `i` (a lone surrogate's own
    /// value; a surrogate half for an astral scalar) — `charCodeAt` semantics.
    pub fn unit_at(&self, i: usize) -> Option<u16> {
        self.locate_unit(i).map(|(cp, off)| unit_of_cp(cp, off))
    }

    /// CodePointAt(unit position) per spec: the FULL code point when `i`
    /// addresses a lead unit, the trail surrogate's value mid-pair, and a lone
    /// surrogate's own value.
    pub fn code_point_at(&self, i: usize) -> Option<u32> {
        self.locate_unit(i)
            .map(|(cp, off)| if off == 0 { cp } else { unit_of_cp(cp, 1) as u32 })
    }

    /// Substring by UNIT positions `[a, b)`. A bound that splits a surrogate
    /// pair yields the REAL covered half (a 1-unit lone-surrogate string).
    /// The output of slicing a canonical buffer is canonical: a low half cut
    /// from one scalar can only be FOLLOWED by what followed that scalar, and
    /// a high half can only END the slice.
    pub fn slice_units(&self, a: usize, b: usize) -> JsStr {
        if self.ascii {
            let (a, b) = (a.min(self.bytes.len()), b.min(self.bytes.len()));
            return JsStr::from_wtf8(if a >= b { Vec::new() } else { self.bytes[a..b].to_vec() });
        }
        let mut out: Vec<u8> = Vec::new();
        if a < b {
            let (mut pos, mut bi) = (0usize, 0usize);
            while bi < self.bytes.len() && pos < b {
                let (cp, blen) = wtf8_decode(&self.bytes, bi);
                let n = cp_units(cp);
                if pos >= a && pos + n <= b {
                    out.extend_from_slice(&self.bytes[bi..bi + blen]);
                } else if n == 2 && pos >= a && pos < b {
                    // Window covers only the lead half.
                    push_cp_raw(&mut out, unit_of_cp(cp, 0) as u32);
                } else if n == 2 && pos + 1 >= a && pos + 1 < b {
                    // Window covers only the trail half.
                    push_cp_raw(&mut out, unit_of_cp(cp, 1) as u32);
                }
                pos += n;
                bi += blen;
            }
        }
        JsStr::from_wtf8(out)
    }

    /// Iterate the code points (for-of/spread semantics — one item per code
    /// point; a lone surrogate yields its 0xD800–0xDFFF value).
    pub fn code_points(&self) -> impl Iterator<Item = u32> + '_ {
        wtf8_code_points(&self.bytes)
    }

    /// Iterate the UTF-16 code units (split('') / string-spread semantics —
    /// an astral scalar contributes its two halves).
    pub fn units_iter(&self) -> impl Iterator<Item = u16> + '_ {
        wtf8_units_iter(&self.bytes)
    }

    /// One for-of step at unit position `pos`: the code point starting there
    /// and the position one CODE POINT later (units advance by 1 or 2). `None`
    /// once past the end.
    pub fn cp_step(&self, pos: usize) -> Option<(u32, usize)> {
        self.locate_unit(pos)
            .map(|(cp, off)| (cp, pos - off + cp_units(cp)))
    }
}

/// A generator's execution state. `Suspended(ip)` parks at the bytecode index of
/// the `Yield` that paused it (resume re-decodes that op to deliver the sent
/// value into its `dst`, then continues at `ip + 1`); `ip == 0` is the
/// not-yet-started state (the first `next()` runs from the top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenState {
    Suspended(usize),
    Running,
    Completed,
}

/// An active `try` handler in a frame, innermost last. A `Catch` lands a thrown
/// value in `reg` and jumps to `target`. A `Finally` is visited on EVERY exit
/// from its protected region — throw, `return`, or normal completion — running
/// the finally block (at `target`) with a completion record deposited into
/// `kind_reg` (0 normal, 1 return, 2 throw) and `val_reg` (the return value /
/// thrown reason), which `EndFinally` then resumes.
#[derive(Clone, Copy, Debug)]
pub enum Handler {
    Catch { target: u32, reg: u16 },
    Finally { target: u32, kind_reg: u16, val_reg: u16 },
}

/// A Promise's settlement state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

/// Which Promise combinator a `Combinator` is tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombKind {
    /// `Promise.all` — fulfil with all values, or reject on the first rejection.
    All,
    /// `Promise.allSettled` — fulfil with `{status, value|reason}` records.
    AllSettled,
    /// `Promise.race` — settle as the first input settles.
    Race,
    /// `Promise.any` — first fulfilment, or an AggregateError if all reject.
    Any,
    /// `Promise.allKeyed` — like `all`, but over an object's own enumerable keys;
    /// fulfils with a null-prototype object mapping each key to its value.
    AllKeyed,
    /// `Promise.allSettledKeyed` — like `allSettled` over an object's keys; fulfils
    /// with a null-prototype object mapping each key to its `{status, …}` record.
    AllSettledKeyed,
}

/// A registered reaction on a pending promise: when it settles, `callback` runs
/// (as a microtask) and its outcome settles `dependent`. A `callback` of
/// `undefined` is a pass-through (the value/reason forwards to `dependent`).
#[derive(Clone, Debug)]
pub struct Reaction {
    pub callback: Value,
    pub dependent: u32,
    /// A `.finally(cb)` reaction: run `callback` (no args) for its side effect,
    /// then forward the ORIGINAL value/reason (a throw in `callback` overrides).
    pub finally: bool,
    /// An `await` reaction: `dependent` is the suspended async ACTIVATION's heap
    /// index, resumed (value or thrown rejection) instead of running a callback.
    pub is_async: bool,
}

/// Boxed payload of a [`HeapObj::Class`] (see that variant's docs). Kept behind a
/// `Box` so the rarely-allocated class object — 8 fields incl. a `String`, three
/// `Vec`s and an `ObjMap` — does not inflate `size_of::<HeapObj>()` for the hot,
/// tiny variants (`Cons`/`Str`/`Array`/`Object`) that pay it on every alloc.
#[derive(Clone, Debug)]
pub struct ClassData {
    pub name: String,
    pub ctor: Option<u32>,
    /// Whether `ctor` is an explicit constructor (its body calls `super`
    /// itself) vs. a fields-only proto (the `new` path runs the parent ctor).
    pub has_explicit_ctor: bool,
    pub methods: Vec<(String, Value)>,
    /// `get x()` accessors, invoked with `this` = instance on property read.
    pub getters: Vec<(String, Value)>,
    /// `set x(v)` accessors, invoked with `this` = instance on property write.
    pub setters: Vec<(String, Value)>,
    /// Static members — own properties of the class value (`C.method`,
    /// `C.field`). Methods start here; static fields are added by SetProp.
    pub statics: ObjMap,
    /// `static get`/`set` accessors, invoked with `this` = the class value on
    /// read/write of a static property.
    pub static_getters: Vec<(String, Value)>,
    pub static_setters: Vec<(String, Value)>,
    /// Heap index of the superclass value (`class C extends P`), for
    /// inherited method/getter lookup and `instanceof` up the chain.
    pub parent: Option<u32>,
    /// `class C extends null {}`: derived-class semantics (super required in
    /// an explicit ctor; implicit super throws) with a null prototype parent.
    pub extends_null: bool,
    /// Computed instance-field keys (`[expr] = v`), evaluated ONCE at class
    /// definition (in source order) and read per-instance by the `FieldInit` op
    /// during construction. Empty for classes with no computed instance fields.
    pub computed_field_keys: Vec<Value>,
    /// Exact `class … { … }` source text, for `Function.prototype.toString`.
    pub source: String,
    /// Upvalue cells captured by the constructor (incl. its field initializers)
    /// from the frame where the class was defined — supplied when `new` runs the
    /// ctor. Empty unless the class is nested in a function and its ctor/fields
    /// close over a local of that function.
    pub ctor_upvalues: Vec<u32>,
    /// Fields-initializer thunk for a DERIVED class with an explicit ctor: run
    /// by the SuperCtor ops on `this` right after `super()` completes (spec
    /// InitializeInstanceElements timing). `None` when the ctor layout carries
    /// entry inits (base/implicit classes) or there are no instance fields.
    pub field_thunk: Option<u32>,
    /// Upvalue cells for `field_thunk`, captured at MakeClass like
    /// `ctor_upvalues` (field initializers may close over enclosing locals).
    pub field_thunk_upvalues: Vec<u32>,
    /// A fresh per-EVALUATION private brand id, minted at MakeClass, giving each
    /// class evaluation a distinct private-name identity (so two classes that both
    /// declare `#m` don't collide). 0 = unbranded.
    pub private_brand: u64,
}

/// Boxed payload of a [`HeapObj::AsyncState`] (see that variant's docs). Boxed for
/// the same reason as [`ClassData`]: it carries two `Vec`s and so is one of the
/// largest variants, but is allocated only when an `async` function suspends.
#[derive(Clone, Debug)]
pub struct AsyncStateData {
    pub func: u32,
    pub closure: u32,
    pub state: GenState,
    pub regs: Vec<Value>,
    pub result: u32,
    pub handlers: Vec<Handler>,
}

/// One pending request on an async generator (spec AsyncGeneratorRequest): the
/// completion kind a `.next()`/`.throw()`/`.return()` call wants delivered, its
/// argument, and the result promise that call returned. Requests are serviced
/// FIFO by `async_gen_service_queue`. GC: `arg` and `promise` are traced via the
/// owning [`AsyncGenState`]'s edge arm in `gc.rs`.
#[derive(Clone, Debug)]
pub struct AsyncGenRequest {
    /// 0 = next, 1 = throw, 2 = return.
    pub kind: u8,
    pub arg: Value,
    pub promise: u32,
}

/// Payload of [`HeapObj::AsyncGenerator`] (an `async function*` activation). Like
/// a generator (suspend/resume on `yield`) AND an async activation (suspend on
/// `await`), so it carries the saved window + handlers like both. `queue` holds
/// the pending `.next()`/`.return()`/`.throw()` requests awaiting the next
/// yield/return (FIFO) — each call returns a Promise.
#[derive(Clone, Debug)]
pub struct AsyncGenState {
    pub func: u32,
    pub closure: u32,
    pub state: GenState,
    pub regs: Vec<Value>,
    pub handlers: Vec<Handler>,
    /// Pending requests, FIFO. The argument must be stored (not just the promise)
    /// because a request can be QUEUED while the generator is awaiting/running, and
    /// the value must be delivered when the request is finally serviced.
    pub queue: Vec<AsyncGenRequest>,
    /// Spec state "awaiting-return": the front request is a `.return(v)` whose
    /// argument is being awaited (AsyncGeneratorAwaitReturn / the Await step of
    /// UnwrapYieldResumption). While set, new requests only enqueue; the await's
    /// settlement re-enters `drive_async_gen`, which routes it.
    pub awaiting_return: bool,
}

/// An ArrayBuffer's byte storage. A plain `ArrayBuffer` owns its bytes per-VM
/// (`Local`); a `SharedArrayBuffer` holds an `Arc` to process-shared memory
/// (`Shared`) so agents on other threads alias the SAME bytes — cloning the
/// heap object (or handing the buffer to a worker agent) clones the Arc, never
/// the memory, which is exactly SharedArrayBuffer semantics. `Deref<[u8]>` /
/// `DerefMut` make almost every byte access (`data.len()`, `&data[a..b]`,
/// `data[i] = x`, `copy_from_slice`) work unchanged on both variants.
#[derive(Clone, Debug)]
pub enum AbData {
    Local(Vec<u8>),
    Shared(std::sync::Arc<SharedMem>),
}

/// The process-shared byte store behind a `SharedArrayBuffer`. The backing
/// allocation is FIXED at construction (a growable SAB preallocates
/// `maxByteLength` bytes, zeroed); only the visible byte length moves, via an
/// atomic store, so `grow` never reallocates and raw pointers held by other
/// agent threads stay valid forever (the `Arc` keeps the allocation alive).
/// Storage is allocated as `u64` words so the base is 8-byte aligned: Atomics
/// element accesses cast interior pointers to `AtomicU8`..`AtomicU64`, and a
/// TypedArray's `byteOffset` is element-size aligned by construction.
pub struct SharedMem {
    buf: UnsafeCell<Box<[u64]>>,
    /// Fixed capacity in bytes (== `maxByteLength`; == the initial length for a
    /// non-growable SAB).
    cap: usize,
    /// Current visible byte length (`grow` stores Release; readers load Acquire).
    len: AtomicUsize,
}

// SAFETY: `SharedMem` is the engine's model of JS shared memory, which is racy
// BY SPEC (ECMA-262 memory model): concurrent non-atomic accesses to a
// SharedArrayBuffer may tear, and that is a permitted outcome for non-atomic
// ops. The allocation itself is fixed (never moved/freed while an Arc holds
// it) and `len` only changes through an atomic. Atomic ops (Atomics.*) go
// through real atomic instructions on interior pointers, never through the
// plain slice views.
unsafe impl Send for SharedMem {}
unsafe impl Sync for SharedMem {}

impl SharedMem {
    /// Allocate `cap` bytes of zeroed shared storage with `len` initially
    /// visible (`len <= cap`; a non-growable SAB passes `len == cap`).
    pub fn new(len: usize, cap: usize) -> SharedMem {
        let words = cap.div_ceil(8).max(1);
        SharedMem {
            buf: UnsafeCell::new(vec![0u64; words].into_boxed_slice()),
            cap,
            len: AtomicUsize::new(len.min(cap)),
        }
    }
    /// The current visible byte length.
    #[inline]
    pub fn byte_len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }
    /// Fixed capacity in bytes (`maxByteLength`).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.cap
    }
    /// Move the visible length (clamped to capacity). `grow` only ever raises
    /// it; the shrink direction (engine-internal quirk paths only — spec SABs
    /// never shrink) zeroes the dropped tail so a later grow re-exposes zeroes,
    /// matching `Vec::resize` semantics on the Local variant.
    pub fn set_byte_len(&self, n: usize) {
        let n = n.min(self.cap);
        let old = self.len.swap(n, Ordering::AcqRel);
        if n < old {
            // SAFETY: n..old is within the fixed allocation; single-VM quirk
            // path (no concurrent agents reach a shrinking SAB).
            unsafe {
                std::ptr::write_bytes(self.base_ptr().add(n), 0, old - n);
            }
        }
    }
    /// Raw base pointer (8-byte aligned) — for the Atomics element accesses.
    #[inline]
    pub fn base_ptr(&self) -> *mut u8 {
        unsafe { (*self.buf.get()).as_mut_ptr() as *mut u8 }
    }
    #[inline]
    fn as_slice(&self) -> &[u8] {
        // SAFETY: the allocation is fixed and outlives `self`; see the
        // Send/Sync note for why cross-thread tearing is acceptable here.
        unsafe { std::slice::from_raw_parts(self.base_ptr(), self.byte_len()) }
    }
}

impl std::fmt::Debug for SharedMem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedMem")
            .field("len", &self.byte_len())
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

impl AbData {
    /// Current byte length (Shared: the visible length, not the capacity).
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            AbData::Local(v) => v.len(),
            AbData::Shared(m) => m.byte_len(),
        }
    }
    #[inline]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The mutable `Vec` for STRUCTURAL mutations (detach-clear / resize /
    /// transfer) — `None` for a Shared buffer, whose allocation is fixed
    /// (callers either error first or route length changes via
    /// [`AbData::resize_bytes`]). Reserved for paths that must distinguish the
    /// variants; the current sites all go through `resize_bytes`.
    #[inline]
    #[allow(dead_code)]
    pub fn local_mut(&mut self) -> Option<&mut Vec<u8>> {
        match self {
            AbData::Local(v) => Some(v),
            AbData::Shared(_) => None,
        }
    }
    /// The shared store, when this is a SharedArrayBuffer's data.
    #[inline]
    pub fn shared(&self) -> Option<&std::sync::Arc<SharedMem>> {
        match self {
            AbData::Local(_) => None,
            AbData::Shared(m) => Some(m),
        }
    }
    /// Structural resize to `n` bytes: Local resizes the Vec (zero-filling
    /// growth); Shared stores the new visible length (the allocation is
    /// preallocated to `maxByteLength` — callers validate `n <= max`).
    pub fn resize_bytes(&mut self, n: usize) {
        match self {
            AbData::Local(v) => v.resize(n, 0u8),
            AbData::Shared(m) => m.set_byte_len(n),
        }
    }
}

impl From<Vec<u8>> for AbData {
    #[inline]
    fn from(v: Vec<u8>) -> AbData {
        AbData::Local(v)
    }
}

impl std::ops::Deref for AbData {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            AbData::Local(v) => v,
            AbData::Shared(m) => m.as_slice(),
        }
    }
}

impl std::ops::DerefMut for AbData {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            AbData::Local(v) => v,
            // SAFETY (engine contract): JS SharedArrayBuffer memory is racy by
            // spec — tearing on concurrent non-atomic access is a permitted
            // outcome, so handing out a byte view of shared memory is sound
            // for the engine's usage. Within one VM the heap hands out only
            // one buffer borrow at a time; Atomics ops never use this path
            // (they use real atomic instructions on SharedMem directly).
            AbData::Shared(m) => unsafe {
                std::slice::from_raw_parts_mut(m.base_ptr(), m.byte_len())
            },
        }
    }
}

/// A heap-allocated object.
#[derive(Clone, Debug)]
pub enum HeapObj {
    /// An owned, contiguous JS string (with cached length / ASCII metadata).
    Str(JsStr),
    /// A lazily-concatenated string ("rope" / cons-string, as in V8). `left` and
    /// `right` are heap indices of string-like objects (flat `Str` or nested
    /// `Cons`); `len` is the total character count, so `.length` is O(1) without
    /// materializing. `+` builds one in O(1) instead of copying both operands;
    /// it is flattened to a contiguous `Str` in place on first content access
    /// (indexing, methods, comparison). JS strings are immutable here
    /// (`set_index` no-ops on them), so the structural sharing is sound.
    Cons { left: u32, right: u32, len: usize },
    /// A plain function: index into `Program::functions`. No captured state.
    Func(u32),
    /// A closure: a function id plus captured upvalue cells (indices of `Cell`
    /// heap objects). Captured variables are boxed into cells so mutation is
    /// shared between the closure and its defining scope. `this_val` is the
    /// lexically-captured `this` for an ARROW function (its proto has
    /// `lexical_this`); it is `UNDEFINED` and unused for ordinary closures.
    Closure { func: u32, upvalues: Vec<u32>, this_val: Value },
    /// A boxed mutable variable cell (an upvalue's storage).
    Cell(Value),
    /// A sloppy direct eval's DYNAMIC variable environment for a FUNCTION
    /// context: name -> value bindings the eval's var/function declarations
    /// created in the caller activation (spec: the caller's varEnv). Reached
    /// via Frame.eval_scope / the closure_eval_scope stamps.
    EvalScope(std::collections::HashMap<String, Value>),
    /// A bound function (`fn.bind(thisArg, ...boundArgs)`): calling it invokes
    /// `target` with `this` fixed to `this` and `args` prepended to the call args.
    Bound { target: Value, this: Value, args: Vec<Value> },
    /// A ShadowRealm WrappedFunction exotic: a fresh wrapper created each time
    /// a callable crosses the realm boundary. Calling it wraps the arguments,
    /// calls `target` with `this` = undefined, and wraps the result; any abrupt
    /// target completion surfaces as a caller-realm TypeError. `name`/`length`
    /// are the CopyNameAndLength snapshot taken at wrap time.
    Wrapped { target: Value, name: String, length: f64 },
    /// A built-in (native) function value, identified by a small id (see the
    /// `native` ids in vm.rs). Callable as a first-class value — this is what backs
    /// `Object.defineProperty`, `Array.isArray`, `Object.prototype.hasOwnProperty`,
    /// `Function.prototype.call`, etc. when accessed as values (not just called).
    Native(u16),
    /// A dense array.
    Array(Vec<Value>),
    /// A plain object.
    Object(ObjMap),
    /// A JS Promise. `result` holds the fulfillment value / rejection reason
    /// (undefined while Pending); `fulfill`/`reject` are reactions registered
    /// while Pending (drained as microtasks on settle). `handled` tracks whether
    /// a rejection handler was attached (for optional unhandled-rejection report).
    Promise {
        state: PromiseState,
        result: Value,
        fulfill: Vec<Reaction>,
        reject: Vec<Reaction>,
        handled: bool,
    },
    /// A native `resolve`/`reject` function bound to a promise — the pair handed
    /// to a `new Promise(executor)`. Calling it settles `promise`.
    BoundResolver { promise: u32, is_reject: bool },
    /// A `Date`: milliseconds since the Unix epoch (NaN = Invalid Date). The
    /// engine treats all component getters/setters as UTC (a documented
    /// simplification — node uses the host time zone for the non-UTC ones).
    Date(f64),
    /// Shared state for a Promise combinator (`all`/`allSettled`/`race`/`any`).
    /// `results` collects per-input outcomes (sized to the input count);
    /// `remaining` counts inputs still outstanding; `result` is the combinator's
    /// own promise (settled when the combinator's condition is met). `settled`
    /// is the per-index [[AlreadyCalled]] guard: a misbehaving thenable that calls
    /// a resolve/reject element more than once is ignored after the first.
    /// `cap_resolve`/`cap_reject` are the result capability's [[Resolve]]/[[Reject]]
    /// functions: the combinator settles its result THROUGH them (per spec), so a
    /// custom `this`-constructor's executor-provided functions are observably
    /// invoked. On the native path they are `BoundResolver`s bound to `result`, so
    /// calling them is identical to `self.resolve/reject(result, …)`.
    Combinator { kind: CombKind, results: Vec<Value>, remaining: u32, result: u32, settled: Vec<bool>, cap_resolve: Value, cap_reject: Value, keys: Vec<String> },
    /// A native resolve/reject element for a combinator: performs one combinator
    /// step (`is_reject` selects fulfill vs reject when CALLED directly by a custom
    /// thenable; via the native reaction the kind comes from the reaction list).
    CombinatorResolver { combinator: u32, index: u32, is_reject: bool },
    /// A suspended generator (`function*`). Owns a DETACHED register window (off
    /// the contiguous live `regs` Vec, so the JIT's pinned-capacity invariant
    /// holds while parked); `func`/`closure` re-create the frame on resume, and
    /// `state` carries the resume ip / completion. `handlers` preserves the
    /// frame's active `try` handlers across a yield, so `gen.throw(e)` resumes
    /// into an enclosing `try`/`catch` (and `gen.return(v)` can run `finally`).
    Generator { func: u32, closure: u32, state: GenState, regs: Vec<Value>, handlers: Vec<Handler> },
    /// An `async function*` activation — see [`AsyncGenState`]. Its `.next()`
    /// returns a Promise; the body may both `yield` and `await`.
    AsyncGenerator(Box<AsyncGenState>),
    /// A suspended `async function` activation — like Generator (detached window
    /// resumed at each `await`) but it also owns its `result` Promise's heap index
    /// and PRESERVES `try` handlers across an await (so `try { await p } catch`
    /// works). `handlers` are (catch_target, catch_reg) pairs.
    AsyncState(Box<AsyncStateData>),
    /// A JS `Map`: insertion-ordered (key, value) entries with SameValueZero key
    /// equality. Parallel `keys`/`vals` Vecs (small Maps dominate; linear scan).
    Map { keys: Vec<Value>, vals: Vec<Value> },
    /// A JS `Set`: insertion-ordered unique values (SameValueZero equality).
    Set(Vec<Value>),
    /// A JS `WeakMap`: like `Map` but keys must be objects and there is no
    /// iteration/size (a distinct type so the [[WeakMapData]] brand check works —
    /// `WeakMap.prototype.set.call(aMap)` must throw). No GC, so refs stay strong.
    WeakMap { keys: Vec<Value>, vals: Vec<Value> },
    /// A JS `WeakSet`: like `Set` but values must be objects, no iteration/size.
    WeakSet(Vec<Value>),
    /// A JS `WeakRef`: a weak reference to an object. No GC, so `deref()` always
    /// returns the (still-live) target.
    WeakRef(Value),
    /// A JS `FinalizationRegistry`: holds a cleanup callback and the live
    /// unregister tokens. No GC, so cleanup never fires (spec-permitted); only
    /// `register`/`unregister` are observable. `tokens` tracks unregister tokens.
    FinalizationRegistry { cleanup: Value, tokens: Vec<Value> },
    /// A boxed primitive wrapper (`new String(x)`/`new Number(x)`/`new Boolean(x)`,
    /// or `Object(primitive)`). `kind` 0=String/1=Number/2=Boolean; `value` is the
    /// wrapped primitive ([[PrimitiveValue]]). `typeof` is "object"; valueOf returns
    /// the value; the kind's prototype provides the methods.
    Boxed { kind: u8, value: Value },
    /// A JS `RegExp`. `regex` is the compiled `regress` engine (ECMAScript regex);
    /// `source` is the pattern text, `flags` the JS flag string (`"gi"`); `last_index`
    /// is the writable `lastIndex` own data property — stored as a raw `Value` (not a
    /// coerced offset) so an assigned object survives until `exec`/the @@-methods
    /// apply ToLength, invoking its `valueOf` at the spec-mandated time.
    RegExp { regex: Box<regress::Regex>, source: String, flags: String, last_index: Value },
    /// A JS `ArrayBuffer` — a raw byte buffer backing TypedArrays/DataViews.
    /// `detached` is set by transfer (we never detach via GC); `data` is the bytes
    /// ([`AbData`]: per-VM `Local` for ArrayBuffers, `Shared` for SharedArrayBuffers).
    ArrayBuffer { data: AbData, detached: bool },
    /// A JS TypedArray view (`Int8Array`, `Float64Array`, …). `kind` indexes the
    /// element type (see `vm::native::TA_KINDS`); `buffer` is the backing
    /// `ArrayBuffer`'s heap index; `byte_offset`/`length` (in elements) frame the view.
    TypedArray { buffer: u32, kind: u8, byte_offset: usize, length: usize },
    /// A JS `DataView` over an ArrayBuffer (`buffer` heap index, byte window).
    DataView { buffer: u32, byte_offset: usize, byte_length: usize },
    /// A JS `Proxy`: property/call operations route through `handler`'s traps (or
    /// fall through to `target`). `revoked` cuts it off (every op then throws).
    Proxy { target: Value, handler: Value, revoked: bool },
    /// A `Temporal.*` value. `kind` selects the type (0=Duration, 1=PlainDate,
    /// 2=PlainTime, 3=PlainDateTime, …); `fields` holds its integer slots in a
    /// per-kind layout (Duration: y,mo,w,d,h,mi,s,ms,us,ns; PlainDate: isoY,isoM,isoD).
    Temporal { kind: u8, fields: Vec<i64> },
    /// An `Intl.*` instance. `kind` selects the service (0=NumberFormat,
    /// 1=DateTimeFormat, 2=Collator, 3=PluralRules, 4=ListFormat,
    /// 5=RelativeTimeFormat, 6=Segmenter, 7=Locale, 8=DisplayNames,
    /// 9=DurationFormat). `resolved` is the heap index of an Object holding the
    /// instance's resolved options (insertion-ordered, so resolvedOptions() can
    /// clone it directly); for Locale it also holds the parsed language/region/…
    /// subtags read back by the prototype getters. `typeof` is "object".
    Intl { kind: u8, resolved: u32 },
    /// A JS `BigInt` primitive. Stored as `i128` (covers the common test262
    /// magnitudes; true arbitrary precision is a later refinement). Compared by
    /// VALUE (`1n === 1n`), not identity; `typeof` is "bigint".
    BigInt(i128),
    /// A JS `Symbol` primitive. Identity is the heap index (so `===` and use as a
    /// property key dedupe correctly). `desc` is the description (a string Value or
    /// UNDEFINED). `prop_key` is the internal string under which the symbol is
    /// stored as an object property — `"@@iterator"` etc. for the well-known
    /// symbols (matching the engine's existing iterator-key convention) and
    /// `"@@sym:N"` for user symbols. `typeof` is "symbol".
    Symbol { desc: Value, prop_key: String },
    /// A built-in iterator (Array/Map/Set `entries()`/`keys()`/`values()` and the
    /// default `@@iterator`). A snapshot of the values to yield plus a cursor;
    /// `proto` is its prototype heap index (%ArrayIteratorPrototype% etc., distinct
    /// per collection). `.next()` yields `items[index]` then advances. When `live`
    /// is `Some((coll, kind))` it is a LIVE Map/Set iterator that steps the backing
    /// collection `coll` at `index` (skipping tombstoned/HOLE slots) instead of the
    /// `items` snapshot, so a delete/add after the iterator is created is observed
    /// (`kind`: 0 = keys, 1 = values, 2 = entries `[k, v]`).
    Iterator { items: Vec<Value>, index: usize, proto: u32, live: Option<(u32, u8)> },
    /// A lazy Iterator Helper (the result of `Iterator.prototype.{map,filter,
    /// take,drop,flatMap}`). `source` is the underlying iterator; `kind` selects
    /// the transform (0=map,1=filter,2=take,3=drop,4=flatMap); `arg` is the
    /// callback (map/filter/flatMap); `n` is the remaining count (take/drop);
    /// `idx` is the 0-based counter passed to callbacks; `done` marks exhaustion;
    /// `inner` is flatMap's current inner iterator (or UNDEFINED).
    IterHelper {
        source: Value,
        kind: u8,
        arg: Value,
        n: i64,
        idx: i64,
        done: bool,
        inner: Value,
        /// The source's `next` method, read ONCE at creation (GetIteratorDirect), so
        /// stepping calls the cached method rather than re-reading `source.next` each
        /// time. `UNDEFINED` when the source needs the generic step path (a generator,
        /// or a multi-source zip/concat helper).
        next: Value,
        /// `[[GeneratorState]] == "executing"` brand: set while a `.next()` step is in
        /// flight so that a callback re-entering `.next()`/`.return()` on the same
        /// helper is a TypeError (GeneratorValidate) rather than infinite recursion.
        running: bool,
    },
    /// A class value (`class C {…}`). Fields live in the boxed [`ClassData`]:
    /// `ctor` is the func id that runs instance field initializers then the user
    /// constructor (or `None`); `methods` maps each instance method name to its
    /// func id. `new C(args)` builds a plain object, links it to its class for
    /// method lookup, and runs the ctor with `this` = the new object.
    Class(Box<ClassData>),
}

/// Heap index of the interned empty string. The 128 single-ASCII-char strings
/// occupy indices `0..128`; the empty string is `128`; user objects start at
/// `129` (see [`Heap::new`]).
pub const INTERN_EMPTY: u32 = 128;

pub struct Heap {
    objs: Vec<HeapObj>,
    /// Per-object version, parallel to `objs` (one `u32` per heap object). Bumped
    /// whenever an object gains a NEW key (which may reallocate its `vals`). The
    /// JIT inline cache reads this (by heap index) to validate a cached
    /// `vals`-pointer: a matching version proves `vals` hasn't reallocated since
    /// the cache was filled. Allocated in lockstep with `objs` so indices align.
    versions: Vec<u32>,
    /// Free list of reclaimed slot indices (filled by the mark-sweep GC's sweep,
    /// drained by `alloc`). A reused slot is overwritten and its version bumped so
    /// any stale JIT inline-cache entry misses. Empty until the first collection.
    free: Vec<u32>,
    /// Number of live (allocated, non-free) slots — `objs.len()` minus the free
    /// list and the pinned built-in prefix bookkeeping. Used to decide when to GC.
    live: usize,
    /// `alloc` sets this once the live count passes `gc_threshold`; the interpreter
    /// dispatch loop polls it at a safe point and runs a collection.
    gc_requested: bool,
    /// Live-count at which the next collection is requested (grown adaptively after
    /// each GC to amortise; never below `GC_MIN_THRESHOLD`).
    gc_threshold: usize,
}

/// Smallest live-object count that triggers a collection — below this the heap is
/// trivially small and collecting would be pure overhead.
pub const GC_MIN_THRESHOLD: usize = 1 << 16;

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        // Pre-intern the 128 single-ASCII-char strings (indices 0..128) and the
        // empty string (index 128). These are immutable and ubiquitous — every
        // `s[i]` and every `s += <digit>` produces one — so sharing a single
        // heap slot eliminates per-iteration allocation in string loops.
        let mut objs = Vec::with_capacity(160);
        let mut versions = Vec::with_capacity(160);
        for b in 0u8..128 {
            objs.push(HeapObj::Str(JsStr::new((b as char).to_string())));
            versions.push(0);
        }
        objs.push(HeapObj::Str(JsStr::new(String::new())));
        versions.push(0);
        let live = objs.len();
        Heap { objs, versions, free: Vec::new(), live, gc_requested: false, gc_threshold: GC_MIN_THRESHOLD }
    }

    #[inline]
    pub fn alloc(&mut self, obj: HeapObj) -> u32 {
        self.live += 1;
        if self.live >= self.gc_threshold {
            self.gc_requested = true;
        }
        // Reuse a reclaimed slot when one is available (its version is bumped so a
        // stale inline-cache entry for the old occupant misses).
        if let Some(idx) = self.free.pop() {
            self.objs[idx as usize] = obj;
            self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
            return idx;
        }
        let idx = self.objs.len() as u32;
        self.objs.push(obj);
        self.versions.push(0);
        idx
    }

    /// Total slot count (live + free + pinned). Sweeps iterate `0..len`.
    #[inline]
    pub fn len(&self) -> usize {
        self.objs.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.objs.is_empty()
    }

    /// Whether the dispatch loop should run a collection (live count passed the
    /// adaptive threshold). Cleared by `note_gc_done`.
    #[inline]
    pub fn gc_requested(&self) -> bool {
        self.gc_requested
    }

    /// The currently-free slot indices (so the GC can protect them from a
    /// double-free without tracing them).
    #[inline]
    pub fn free_indices(&self) -> &[u32] {
        &self.free
    }

    /// Reclaim slot `idx`: drop its (possibly large) contents to a tiny tombstone
    /// and return the slot to the free list. The caller (GC sweep) guarantees no
    /// live reference remains. Never call on a pinned built-in slot.
    #[inline]
    pub fn free_slot(&mut self, idx: u32) {
        self.objs[idx as usize] = HeapObj::Date(f64::NAN);
        self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
        self.free.push(idx);
    }

    /// Record the post-sweep live count and grow the next threshold to ~2x it
    /// (amortising collection cost), clearing the request flag.
    #[inline]
    pub fn note_gc_done(&mut self, live: usize) {
        self.live = live;
        self.gc_threshold = (live.saturating_mul(2)).max(GC_MIN_THRESHOLD);
        self.gc_requested = false;
    }

    /// Bump object `idx`'s version (call after a key-add reallocates its `vals`).
    ///
    /// The counter is `u32`. A false inline-cache hit would require it to wrap
    /// (2^32 key-adds to a SINGLE object); that is ~36 GB of keys on one object
    /// (OOM long before), and the cache is re-filled on every miss, so it is
    /// practically unreachable. A `u64` would remove even the theoretical edge.
    #[inline]
    pub fn bump_version(&mut self, idx: u32) {
        self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
    }

    /// Base pointer of the parallel version array (for the JIT inline cache). The
    /// array does not reallocate during a native region run (a region never
    /// allocates a heap object), so this stays valid for the run.
    #[inline]
    pub fn versions_ptr(&self) -> *const u32 {
        self.versions.as_ptr()
    }

    /// Current version of object `idx` (for filling an inline-cache entry).
    #[inline]
    pub fn version_of(&self, idx: u32) -> u32 {
        self.versions[idx as usize]
    }

    #[inline]
    pub fn get(&self, idx: u32) -> &HeapObj {
        &self.objs[idx as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, idx: u32) -> &mut HeapObj {
        &mut self.objs[idx as usize]
    }

    #[inline]
    pub fn alloc_str(&mut self, s: String) -> u32 {
        // Reuse the interned slot for the empty string and single-ASCII-char
        // strings instead of allocating (see `Heap::new`). Safe because strings
        // are immutable — nothing ever mutates a heap string in place.
        match s.len() {
            0 => return INTERN_EMPTY,
            1 => {
                let b = s.as_bytes()[0];
                if b < 128 {
                    return b as u32;
                }
            }
            _ => {}
        }
        self.alloc(HeapObj::Str(JsStr::new(s)))
    }

    /// `alloc_str` for an already-built `JsStr` (the WTF-8 creation sites):
    /// same interning of the empty / single-ASCII-char strings.
    pub fn alloc_js(&mut self, js: JsStr) -> u32 {
        let b = js.as_bytes();
        match b.len() {
            0 => return INTERN_EMPTY,
            1 if b[0] < 128 => return b[0] as u32,
            _ => {}
        }
        self.alloc(HeapObj::Str(js))
    }

    /// Allocate a rope node over two string-like children (O(1) concatenation).
    /// `len` is the children's combined length in the SAME measure as
    /// `JsStr::units` (UTF-16 code units) — `str_units` of both sides summed,
    /// which stays additive across concatenation.
    #[inline]
    pub fn alloc_cons(&mut self, left: u32, right: u32, len: usize) -> u32 {
        self.alloc(HeapObj::Cons { left, right, len })
    }

    /// Is this heap object a string — flat `Str` or rope `Cons`?
    #[inline]
    pub fn is_str_like(&self, idx: u32) -> bool {
        matches!(self.get(idx), HeapObj::Str(_) | HeapObj::Cons { .. })
    }

    /// UTF-16 code-unit length of a string-like object (the JS `.length`) —
    /// O(1): a rope stores it; a flat `JsStr` caches it (computed once in
    /// `JsStr::new`). `None` if not a string.
    pub fn str_units(&self, idx: u32) -> Option<usize> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.units()),
            HeapObj::Cons { len, .. } => Some(*len),
            _ => None,
        }
    }

    /// `Some(true)` if the string-like object is empty (O(1)); `None` if not a
    /// string. Reads the cached/stored length rather than scanning the bytes.
    #[inline]
    pub fn str_is_empty(&self, idx: u32) -> Option<bool> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.units() == 0),
            HeapObj::Cons { len, .. } => Some(*len == 0),
            _ => None,
        }
    }

    /// Append the full WTF-8 content of a (possibly rope) string to `out`,
    /// canonicalizing each segment seam (a high surrogate ending one segment
    /// merges with a low surrogate opening the next — `wtf8_push`).
    /// Iterative, not recursive: a `s += x` loop builds a left-leaning rope that
    /// can be thousands of nodes deep, which would overflow the stack.
    pub fn write_wtf8(&self, idx: u32, out: &mut Vec<u8>) {
        // Explicit stack; push the right child then the left so the left is
        // popped (appended) first — preserving left-to-right concatenation.
        let mut stack = vec![idx];
        while let Some(n) = stack.pop() {
            match self.get(n) {
                HeapObj::Str(s) => wtf8_push(out, s.as_bytes()),
                HeapObj::Cons { left, right, .. } => {
                    stack.push(*right);
                    stack.push(*left);
                }
                _ => {}
            }
        }
    }

    /// Borrow a string-like as `&str` without allocating when it is already
    /// flat AND well-formed (the common case); materialize a rope / a
    /// lone-surrogate string into an owned `String` otherwise — LOSSY for lone
    /// surrogates (each reads as U+FFFD, byte-length preserving, so positions
    /// stay exchangeable with the exact bytes). Exact consumers use
    /// `str_wtf8_cow`. `None` if `idx` isn't a string.
    pub fn str_cow(&self, idx: u32) -> Option<Cow<'_, str>> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.as_str_lossy()),
            HeapObj::Cons { len, .. } => {
                let mut out = Vec::with_capacity(*len);
                self.write_wtf8(idx, &mut out);
                Some(Cow::Owned(wtf8_into_lossy_string(out)))
            }
            _ => None,
        }
    }

    /// The EXACT (WTF-8) byte content of a string-like: borrowed when flat,
    /// materialized (with seam canonicalization) for a rope. `None` if not a
    /// string.
    pub fn str_wtf8_cow(&self, idx: u32) -> Option<Cow<'_, [u8]>> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(Cow::Borrowed(s.as_bytes())),
            HeapObj::Cons { len, .. } => {
                let mut out = Vec::with_capacity(*len);
                self.write_wtf8(idx, &mut out);
                Some(Cow::Owned(out))
            }
            _ => None,
        }
    }

    /// Whether every flat leaf under a string-like object is well-formed —
    /// from the cached `JsStr` flags only, no flattening or byte scan. All
    /// leaves well-formed ⇒ the concatenation holds no surrogate bytes at
    /// all ⇒ well-formed. (The converse may not hold — a surrogate pair
    /// joining at a rope seam canonicalizes away on flatten — so a `false`
    /// here is only a conservative "may hold lone surrogates".)
    fn str_leaves_wellformed(&self, idx: u32) -> bool {
        match self.get(idx) {
            HeapObj::Str(s) => s.is_wellformed(),
            HeapObj::Cons { left, right, .. } => {
                self.str_leaves_wellformed(*left) && self.str_leaves_wellformed(*right)
            }
            _ => true,
        }
    }

    /// EXACT WTF-8 bytes of a string-like object, but ONLY when it is NOT
    /// well-formed (holds lone surrogates) — `None` for a well-formed string
    /// or a non-string. Rejection reads cached flags (O(1) flat, O(leaves)
    /// rope) — no flattening or byte scan on the well-formed path. The side
    /// channel for paths whose lossy `String` view would decay surrogates to
    /// U+FFFD (eval source capture, RegExp pattern/source exactness).
    pub fn str_exact_if_not_wellformed(&self, idx: u32) -> Option<Vec<u8>> {
        if !self.is_str_like(idx) || self.str_leaves_wellformed(idx) {
            return None;
        }
        // Flatten and re-check: a rope seam can canonicalize a high+low pair
        // into an astral scalar, leaving the WHOLE string well-formed even
        // though a leaf was not.
        let b = self.str_wtf8_cow(idx)?;
        (!wtf8_is_wellformed(&b)).then(|| b.into_owned())
    }

    /// Content equality of two string-like objects. Fast (no allocation) when
    /// both are already flat — the common case for a hot `a === b` comparison.
    /// Byte equality IS content equality: the WTF-8 buffers are canonical
    /// (`write_wtf8`/`wtf8_push` merge cross-segment surrogate pairs), so two
    /// equal unit sequences always have identical bytes.
    pub fn str_eq(&self, a: u32, b: u32) -> bool {
        match (self.get(a), self.get(b)) {
            (HeapObj::Str(x), HeapObj::Str(y)) => x.as_bytes() == y.as_bytes(),
            _ => {
                let (mut sa, mut sb) = (Vec::new(), Vec::new());
                self.write_wtf8(a, &mut sa);
                self.write_wtf8(b, &mut sb);
                sa == sb
            }
        }
    }

    /// Flatten the rope at `idx` into a contiguous `Str` in place. No-op if it is
    /// already flat (or not a string). The already-flat fast path is a single tag
    /// check, so this is cheap to call unconditionally before content access.
    #[inline]
    pub fn flatten(&mut self, idx: u32) {
        if matches!(self.objs[idx as usize], HeapObj::Cons { .. }) {
            self.flatten_cold(idx);
        }
    }

    #[cold]
    fn flatten_cold(&mut self, idx: u32) {
        let len = match &self.objs[idx as usize] {
            HeapObj::Cons { len, .. } => *len,
            _ => return,
        };
        let mut out = Vec::with_capacity(len);
        self.write_wtf8(idx, &mut out);
        self.objs[idx as usize] = HeapObj::Str(JsStr::from_wtf8(out));
    }

    /// Resolve a callable (plain function or closure) to its function id and
    /// upvalue list. Returns `None` for non-callables.
    #[inline]
    pub fn as_callable(&self, idx: u32) -> Option<(u32, &[u32])> {
        match self.get(idx) {
            HeapObj::Func(id) => Some((*id, &[])),
            HeapObj::Closure { func, upvalues, .. } => Some((*func, upvalues.as_slice())),
            _ => None,
        }
    }

    #[inline]
    pub fn cell_get(&self, idx: u32) -> Value {
        match self.get(idx) {
            HeapObj::Cell(v) => *v,
            _ => Value::UNDEFINED,
        }
    }

    #[inline]
    pub fn cell_set(&mut self, idx: u32, v: Value) {
        if let HeapObj::Cell(slot) = self.get_mut(idx) {
            *slot = v;
        }
    }
}
