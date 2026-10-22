//! `TextEncoder` and `TextDecoder` (the WHATWG Encoding Standard) for UTF-8
//! and UTF-16.
//!
//! Neither is ECMAScript, but every host engine provides them and bundled
//! applications reach them at load time: React's browser server renderer
//! builds its `TextEncoder` while its module initialises, so the bundle
//! could not load at all. A decoder of another encoding (`windows-1252`,
//! the CJK legacy encodings) is a RangeError rather than a wrong answer.

use super::*;
use crate::heap::TextCodec;

/// The encodings a `TextDecoder` decodes (`TextCodec::encoding`).
const UTF8: u8 = 0;
const UTF16LE: u8 = 1;
const UTF16BE: u8 = 2;

/// The encoding the Encoding Standard's label `label` names, among those
/// decoded here: the label with ASCII whitespace trimmed, matched without
/// case.
fn encoding_for_label(label: &str) -> Option<u8> {
    let l = label
        .trim_matches(|c| matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' '))
        .to_ascii_lowercase();
    Some(match l.as_str() {
        "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8" | "utf-8" | "utf8" | "x-unicode20utf8" => UTF8,
        "csunicode" | "iso-10646-ucs-2" | "ucs-2" | "unicode" | "unicodefeff" | "utf-16" | "utf-16le" => UTF16LE,
        "unicodefffe" | "utf-16be" => UTF16BE,
        _ => return None,
    })
}

fn encoding_name(e: u8) -> &'static str {
    match e {
        UTF16LE => "utf-16le",
        UTF16BE => "utf-16be",
        _ => "utf-8",
    }
}

/// A string's WTF-8 bytes as UTF-8: each lone surrogate (`ED A0..=BF xx`; a
/// pair is always one four-byte sequence in WTF-8) becomes U+FFFD, as the
/// conversion to a USVString the encoder starts with does.
fn usv_utf8(w: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let lone = |i: usize| w[i] == 0xED && w.get(i + 1).is_some_and(|&b| b >= 0xA0);
    if !(0..w.len()).any(lone) {
        return std::borrow::Cow::Borrowed(w);
    }
    let mut out = Vec::with_capacity(w.len());
    let mut i = 0;
    while i < w.len() {
        if lone(i) {
            out.extend_from_slice("\u{FFFD}".as_bytes());
            i += 3;
        } else {
            out.push(w[i]);
            i += 1;
        }
    }
    std::borrow::Cow::Owned(out)
}

/// How many bytes at the end of `b` begin a UTF-8 sequence that more input
/// could still complete: what a streaming decode keeps for its next call.
fn incomplete_utf8_tail(b: &[u8]) -> usize {
    let n = b.len();
    for back in 1..=n.min(3) {
        if b[n - back] & 0xC0 == 0x80 {
            continue;
        }
        return match std::str::from_utf8(&b[n - back..]) {
            Err(e) if e.valid_up_to() == 0 && e.error_len().is_none() => back,
            _ => 0,
        };
    }
    0
}

/// Decode `buf` as `encoding`: the text and the bytes a streaming decode
/// keeps for the next call. Invalid input is U+FFFD (the Unicode practice
/// of one per maximal subpart, which the Encoding Standard's decoders
/// follow), or `None` when `fatal`.
fn decode_bytes(buf: &[u8], encoding: u8, stream: bool, fatal: bool) -> Option<(String, Vec<u8>)> {
    if encoding == UTF8 {
        let keep = if stream { incomplete_utf8_tail(buf) } else { 0 };
        let (head, tail) = buf.split_at(buf.len() - keep);
        let text = if fatal {
            std::str::from_utf8(head).ok()?.to_string()
        } else {
            String::from_utf8_lossy(head).into_owned()
        };
        return Some((text, tail.to_vec()));
    }
    let be = encoding == UTF16BE;
    let mut units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|p| if be { u16::from_be_bytes([p[0], p[1]]) } else { u16::from_le_bytes([p[0], p[1]]) })
        .collect();
    let mut keep = buf[units.len() * 2..].to_vec();
    // A lead surrogate ending a streamed chunk waits for its trail.
    if stream && units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
        let lead = buf.len() - keep.len() - 2;
        keep = buf[lead..].to_vec();
        units.pop();
    }
    let mut text = if fatal {
        String::from_utf16(&units).ok()?
    } else {
        String::from_utf16_lossy(&units)
    };
    if !stream && !keep.is_empty() {
        // An odd byte at the end of the stream.
        if fatal {
            return None;
        }
        text.push('\u{FFFD}');
        keep.clear();
    }
    Some((text, keep))
}

impl<'p> Vm<'p> {
    /// `new TextEncoder()` and `new TextDecoder(label, options)`.
    pub(crate) fn text_codec_construct(
        &mut self,
        decoder: bool,
        args: &[Value],
        new_target: Value,
        cv: Value,
    ) -> Result<Value, Thrown> {
        let mut codec = TextCodec {
            decoder,
            ..TextCodec::default()
        };
        if decoder {
            let label = args.first().copied().unwrap_or(Value::UNDEFINED);
            if label != Value::UNDEFINED {
                let l = self.to_js_string(label)?;
                codec.encoding = encoding_for_label(&l).ok_or_else(|| {
                    Thrown(format!("RangeError: The encoding label provided ('{l}') is invalid."))
                })?;
            }
            let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
            if !opts.is_nullish() {
                if !self.is_object_value(opts) {
                    return Err(Thrown::from_static(
                        "TypeError: TextDecoder options must be an object",
                    ));
                }
                let f = self.get_prop(opts, "fatal")?;
                codec.fatal = self.truthy(f);
                let b = self.get_prop(opts, "ignoreBOM")?;
                codec.ignore_bom = self.truthy(b);
            }
        }
        let proto = if decoder { self.text_decoder_proto } else { self.text_encoder_proto };
        let over = self.newtarget_proto_override(new_target, cv, proto)?;
        let v = Value::heap(self.heap.alloc(HeapObj::TextCodec(codec)));
        Ok(self.set_ctor_proto(v, over))
    }

    /// The `TextEncoder` (`decoder` false) or `TextDecoder` behind `this`.
    fn text_codec_of(&self, this: Value, decoder: bool, what: &str) -> Result<(u32, TextCodec), Thrown> {
        if this.is_heap() {
            if let HeapObj::TextCodec(c) = self.heap.get(this.heap_index()) {
                if c.decoder == decoder {
                    return Ok((this.heap_index(), *c));
                }
            }
        }
        Err(Thrown(format!("TypeError: {what} called on an incompatible receiver")))
    }

    /// The bytes of a BufferSource: an ArrayBuffer, a TypedArray's or a
    /// DataView's view of one (a detached or out-of-bounds view has none).
    fn buffer_source_bytes(&self, v: Value) -> Result<Vec<u8>, Thrown> {
        let bad = || Thrown::from_static(
            "TypeError: The provided value is not of type '(ArrayBuffer or ArrayBufferView)'",
        );
        if !v.is_heap() {
            return Err(bad());
        }
        let idx = v.heap_index();
        let (buffer, off, len) = match self.heap.get(idx) {
            HeapObj::ArrayBuffer { data, detached } => {
                return Ok(if *detached { Vec::new() } else { data.to_vec() });
            }
            HeapObj::TypedArray { buffer, kind, byte_offset, .. } => {
                let size = native::TA_KINDS[*kind as usize].1;
                let n = self.ta_effective_len(idx).unwrap_or(0);
                (*buffer, *byte_offset, n.saturating_mul(size))
            }
            HeapObj::DataView { buffer, byte_offset, byte_length, .. } => {
                let buf_len = match self.heap.get(*buffer) {
                    HeapObj::ArrayBuffer { data, detached: false } => data.len(),
                    _ => 0,
                };
                let n = self.dv_effective_len_at(idx, *byte_offset, *byte_length, buf_len).unwrap_or(0);
                (*buffer, *byte_offset, n)
            }
            _ => return Err(bad()),
        };
        Ok(match self.heap.get(buffer) {
            HeapObj::ArrayBuffer { data, detached: false } if off.saturating_add(len) <= data.len() => {
                data[off..off + len].to_vec()
            }
            _ => Vec::new(),
        })
    }

    /// The `TextEncoder.prototype` / `TextDecoder.prototype` natives.
    pub(crate) fn text_codec_native(&mut self, id: u16, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        Ok(match id {
            native::TE_ENCODING_GET => {
                self.text_codec_of(this, false, "TextEncoder.prototype.encoding")?;
                self.alloc_str("utf-8".to_string())
            }
            native::TE_ENCODE => {
                self.text_codec_of(this, false, "TextEncoder.prototype.encode")?;
                let bytes: Vec<u8> = if a0 == Value::UNDEFINED {
                    Vec::new()
                } else if self.is_string_value(a0) {
                    self.heap.flatten(a0.heap_index());
                    let w = self.heap.str_wtf8_cow(a0.heap_index()).unwrap_or_default();
                    usv_utf8(&w).into_owned()
                } else {
                    self.to_js_string(a0)?.into_bytes()
                };
                let buf = self.alloc_array_buffer(bytes.len())?;
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(buf) {
                    data.copy_from_slice(&bytes);
                }
                self.alloc_typed_array(buf, 1, 0, bytes.len())
            }
            native::TE_ENCODE_INTO => {
                self.text_codec_of(this, false, "TextEncoder.prototype.encodeInto")?;
                let src = if self.is_string_value(a0) {
                    self.heap.flatten(a0.heap_index());
                    let w = self.heap.str_wtf8_cow(a0.heap_index()).unwrap_or_default();
                    String::from_utf8(usv_utf8(&w).into_owned()).unwrap_or_default()
                } else {
                    self.to_js_string(a0)?
                };
                let dst = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let didx = self.u8_brand(dst)?;
                let room = self.ta_effective_len(didx).unwrap_or(0);
                // Whole characters only: stop at the first that does not fit.
                let (mut read, mut written) = (0usize, 0usize);
                for c in src.chars() {
                    let n = c.len_utf8();
                    if written + n > room {
                        break;
                    }
                    written += n;
                    read += c.len_utf16();
                }
                self.u8_write(didx, &src.as_bytes()[..written]);
                let mut map = ObjMap::new();
                map.set("read", Value::num(read as f64));
                map.set("written", Value::num(written as f64));
                self.alloc_object_current_realm(map)
            }
            native::TD_ENCODING_GET | native::TD_FATAL_GET | native::TD_IGNORE_BOM_GET => {
                let what = match id {
                    native::TD_ENCODING_GET => "TextDecoder.prototype.encoding",
                    native::TD_FATAL_GET => "TextDecoder.prototype.fatal",
                    _ => "TextDecoder.prototype.ignoreBOM",
                };
                let (_, c) = self.text_codec_of(this, true, what)?;
                match id {
                    native::TD_ENCODING_GET => self.alloc_str(encoding_name(c.encoding).to_string()),
                    native::TD_FATAL_GET => Value::bool(c.fatal),
                    _ => Value::bool(c.ignore_bom),
                }
            }
            native::TD_DECODE => {
                const WHAT: &str = "TextDecoder.prototype.decode";
                self.text_codec_of(this, true, WHAT)?;
                let input = if a0 == Value::UNDEFINED { Vec::new() } else { self.buffer_source_bytes(a0)? };
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let stream = if opts.is_nullish() {
                    false
                } else if self.is_object_value(opts) {
                    let s = self.get_prop(opts, "stream")?;
                    self.truthy(s)
                } else {
                    return Err(Thrown::from_static(
                        "TypeError: TextDecoder.prototype.decode options must be an object",
                    ));
                };
                // The decoder's state as it is now: a `stream` getter runs
                // guest code, which can decode with this decoder too, and
                // this call continues from where that left it (a copy taken
                // before the getter wrote the nested call's state over).
                let (idx, mut c) = self.text_codec_of(this, true, WHAT)?;
                let mut buf = c.pending[..c.pending_len as usize].to_vec();
                buf.extend_from_slice(&input);
                let decoded = decode_bytes(&buf, c.encoding, stream, c.fatal);
                let Some((mut text, keep)) = decoded else {
                    // A fatal error drops the sequence it is in. A streaming
                    // call's stream goes on, its BOM seen or not as before
                    // (the next call's U+FEFF is text if one was); a final
                    // call's ends, as every final call's does.
                    c.pending_len = 0;
                    if !stream {
                        c.bom_seen = false;
                    }
                    if let HeapObj::TextCodec(s) = self.heap.get_mut(idx) {
                        *s = c;
                    }
                    return Err(Thrown::from_static("TypeError: The encoded data was not valid."));
                };
                if !c.ignore_bom && !c.bom_seen && text.starts_with('\u{FEFF}') {
                    text.drain(..'\u{FEFF}'.len_utf8());
                    c.bom_seen = true;
                } else if !text.is_empty() {
                    c.bom_seen = true;
                }
                c.pending_len = keep.len() as u8;
                c.pending[..keep.len()].copy_from_slice(&keep);
                if !stream {
                    c.pending_len = 0;
                    c.bom_seen = false;
                }
                if let HeapObj::TextCodec(s) = self.heap.get_mut(idx) {
                    *s = c;
                }
                self.alloc_str(text)
            }
            _ => unreachable!("text_codec_native: not a TextEncoder/TextDecoder native"),
        })
    }
}
