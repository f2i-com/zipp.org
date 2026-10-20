//! `TextEncoder` and `TextDecoder` (vm/text_codec.rs) against Node.
//!
//! React's browser server renderer builds a `TextEncoder` while its module
//! initialises, so a bundle of it could not load. The probe covers encoding
//! (ASCII, two- to four-byte characters, lone surrogates, non-string input),
//! `encodeInto` stopping at a whole character, decoding (a BOM and
//! `ignoreBOM`, invalid and overlong UTF-8, `fatal`, a byte-at-a-time
//! stream, a truncated end, DataView and other views, UTF-16LE and BE),
//! labels, and the receiver, constructor and subclass behaviour. The
//! expected lines are Node's (v24).

const PROBE: &str = r###""use strict";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));
const hex = (u8) => Array.from(u8, (b) => b.toString(16).padStart(2, "0")).join("");
const show = (s) => JSON.stringify(s);
const te = new TextEncoder();
log("enc-encoding", te.encoding, Object.prototype.toString.call(te), typeof TextEncoder, TextEncoder.length, TextDecoder.length);
log("encode-ascii", hex(te.encode("abc")), te.encode("abc") instanceof Uint8Array, te.encode("abc").length);
log("encode-empty", te.encode().length, te.encode("").length, te.encode(undefined).length);
log("encode-bmp", hex(te.encode("é€中")));
log("encode-astral", hex(te.encode("\u{1F600}x")));
log("encode-lone", hex(te.encode("a\ud800b\udc00c")), hex(te.encode("\ud83d")));
log("encode-number", hex(te.encode(123)), hex(te.encode(null)));
const dst = new Uint8Array(5);
const r1 = te.encodeInto("hé€z", dst);
log("encodeInto", r1.read, r1.written, hex(dst), JSON.stringify(Object.keys(r1)));
const r2 = te.encodeInto("\u{1F600}\u{1F600}", new Uint8Array(6));
log("encodeInto-astral", r2.read, r2.written);
const td = new TextDecoder();
log("dec-props", td.encoding, td.fatal, td.ignoreBOM, Object.prototype.toString.call(td));
log("decode-basic", show(td.decode(new Uint8Array([0x68, 0x69]))), show(td.decode()), show(td.decode(new Uint8Array([]).buffer)));
log("decode-bom", show(td.decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x41]))), show(new TextDecoder("utf-8", { ignoreBOM: true }).decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x41]))));
log("decode-invalid", show(td.decode(new Uint8Array([0x61, 0xff, 0x62, 0xc3, 0x28, 0xe2, 0x82]))));
log("decode-overlong", show(td.decode(new Uint8Array([0xc0, 0xaf, 0xed, 0xa0, 0x80, 0xf4, 0x90, 0x80, 0x80]))));
try {
  new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array([0x61, 0xff]));
  log("fatal", "no throw");
} catch (e) {
  log("fatal", e instanceof TypeError);
}
const sd = new TextDecoder();
const bytes = te.encode("a€\u{1F600}b");
let acc = "";
for (let i = 0; i < bytes.length; i++) acc += sd.decode(bytes.subarray(i, i + 1), { stream: true });
acc += sd.decode();
log("stream", show(acc), acc === "a€\u{1F600}b");
log("stream-trunc", show(new TextDecoder().decode(new Uint8Array([0xe2, 0x82]), { stream: false })));
const dv = new DataView(new Uint8Array([0x7a, 0x41, 0x42]).buffer, 1, 2);
log("dataview", show(td.decode(dv)), show(td.decode(new Uint16Array([0x4241]))));
const u16 = new TextDecoder("utf-16le");
log("utf16le", u16.encoding, show(u16.decode(new Uint8Array([0xff, 0xfe, 0x41, 0x00, 0x3d, 0xd8, 0x00, 0xde]))));
log("utf16be", new TextDecoder("UTF-16BE").encoding, show(new TextDecoder("utf-16be").decode(new Uint8Array([0x00, 0x41, 0xd8]))));
log("labels", new TextDecoder(" Unicode-1-1-UTF-8 ").encoding, new TextDecoder("utf8").encoding);
try { new TextDecoder("bogus"); log("bad-label", "no throw"); } catch (e) { log("bad-label", e instanceof RangeError); }
try { TextEncoder(); log("call", "no throw"); } catch (e) { log("call", e instanceof TypeError); }
try { TextEncoder.prototype.encode.call({}, "x"); log("brand", "no throw"); } catch (e) { log("brand", e instanceof TypeError); }
try { td.decode("str"); log("decode-type", "no throw"); } catch (e) { log("decode-type", e instanceof TypeError); }
class Sub extends TextDecoder {}
log("subclass", new Sub().encoding, new Sub() instanceof TextDecoder, Object.getPrototypeOf(new Sub()) === Sub.prototype);
log("ctor", TextEncoder.prototype.constructor === TextEncoder, TextDecoder.name, TextEncoder.name);
console.log(out.join("\n"));
"###;

const EXPECTED: &str = r###"enc-encoding utf-8 [object TextEncoder] function 0 0
encode-ascii 616263 true 3
encode-empty 0 0 0
encode-bmp c3a9e282ace4b8ad
encode-astral f09f988078
encode-lone 61efbfbd62efbfbd63 efbfbd
encode-number 313233 6e756c6c
encodeInto 2 3 68c3a90000 ["read","written"]
encodeInto-astral 2 4
dec-props utf-8 false false [object TextDecoder]
decode-basic "hi" "" ""
decode-bom "A" "﻿A"
decode-invalid "a�b�(�"
decode-overlong "���������"
fatal true
stream "a€😀b" true
stream-trunc "�"
dataview "AB" "AB"
utf16le utf-16le "A😀"
utf16be utf-16be "A�"
labels utf-8 utf-8
bad-label true
call true
brand true
decode-type true
subclass utf-8 true true
ctor true TextDecoder TextEncoder"###;

/// Run src on a 256 MiB stack, as the CLI does, and return its output.
fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let out = zipp_vm::run(&src).expect("the probe compiles");
            assert!(out.error.is_none(), "runtime error: {:?}", out.error);
            out.output.join("\n")
        })
        .expect("spawn the probe thread")
        .join()
        .expect("the probe thread finishes")
}

#[test]
fn text_encoder_and_decoder_match_node() {
    assert_eq!(run(PROBE), EXPECTED);
}