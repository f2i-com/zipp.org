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

/// A decoder's state across calls that re-enter it and calls that fail: a
/// `stream` getter that decodes with the same decoder (the outer call goes
/// on from the inner one's state, as the Encoding Standard's argument
/// conversion before the method steps has it, warm or cold), a getter
/// that throws (nothing changes), and a fatal error, after which a
/// streaming decoder's BOM is still seen and a final one starts afresh.
/// The expected lines are Node's (v24), which Chromium's agree with.
const STATE_PROBE: &str = r###""use strict";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));
const cps = (s) => JSON.stringify(Array.from(s, (c) => c.codePointAt(0)));
const u8 = (...b) => new Uint8Array(b);
const thrown = (f) => { try { return "returned " + JSON.stringify(f()); } catch (e) { return e.name; } };
const warm = new TextDecoder();
warm.decode(u8(), { stream: true });
log("reenter-warm", cps(warm.decode(u8(0xa9), { get stream() { warm.decode(u8(0xc3), { stream: true }); return false; } })));
const cold = new TextDecoder();
log("reenter-cold", cps(cold.decode(u8(0xa9), { get stream() { cold.decode(u8(0xc3), { stream: true }); return false; } })));
const held = new TextDecoder();
held.decode(u8(0xc3), { stream: true });
log("getter-throws", thrown(() => held.decode(u8(0x41), { get stream() { throw new RangeError("no"); } })), cps(held.decode(u8(0xa9))));
const f = new TextDecoder("utf-8", { fatal: true });
f.decode(u8(0x41), { stream: true });
log("fatal-stream", thrown(() => f.decode(u8(0xff), { stream: true })), cps(f.decode(u8(0xef, 0xbb, 0xbf, 0x42), { stream: true })), cps(f.decode()));
const g = new TextDecoder("utf-8", { fatal: true });
log("fatal-final", cps(g.decode(u8(0xef, 0xbb, 0xbf, 0x41), { stream: true })), thrown(() => g.decode(u8(0xff))), cps(g.decode(u8(0xef, 0xbb, 0xbf, 0x42))));
const h = new TextDecoder("utf-8", { fatal: true });
log("fatal-first", thrown(() => h.decode(u8(0xff), { stream: true })), cps(h.decode(u8(0xef, 0xbb, 0xbf, 0x43))));
const split = new TextDecoder();
log("split-bom", cps(split.decode(u8(0xef), { stream: true })), cps(split.decode(u8(0xbb, 0xbf, 0x41), { stream: true })), cps(split.decode(u8(0xef, 0xbb, 0xbf))), cps(split.decode(u8(0xef, 0xbb, 0xbf, 0x44))));
const keep = new TextDecoder("utf-8", { ignoreBOM: true });
log("ignore-bom", cps(keep.decode(u8(0xef, 0xbb), { stream: true })), cps(keep.decode(u8(0xbf, 0x41))));
const w = new TextDecoder("utf-16le", { fatal: true });
log("utf16-odd", thrown(() => w.decode(u8(0x41))), cps(w.decode(u8(0xff, 0xfe, 0x41, 0x00))));
log("utf16-lone", thrown(() => w.decode(u8(0x00, 0xd8, 0x41, 0x00), { stream: true })), cps(w.decode(u8(0x42, 0x00))));
console.log(out.join("\n"));
"###;

const STATE_EXPECTED: &str = r###"reenter-warm [233]
reenter-cold [233]
getter-throws RangeError [233]
fatal-stream TypeError [65279,66] []
fatal-final [65] TypeError [66]
fatal-first TypeError [67]
split-bom [] [65] [65279] [68]
ignore-bom [] [65279,65]
utf16-odd TypeError [65]
utf16-lone TypeError [66]"###;

#[test]
fn decoder_state_across_reentry_and_errors_matches_node() {
    assert_eq!(run(STATE_PROBE), STATE_EXPECTED);
}