//! `TypedArray.prototype.fill` and a `set` from a TypedArray of another
//! element kind store their run of Numbers in bulk (`Vm::ta_store_numbers`,
//! `Vm::ta_read_numbers`) instead of one `ta_element_set` per element. Every
//! result here is compared byte for byte with the same stores made one
//! indexed element at a time: each kind (Int8 to Float64, Uint8Clamped's
//! rounding, Float16 where present), NaN, -0, fractions and values out of
//! every integer kind's range, partial and negative ranges, sources holding
//! NaN payloads, a cross-kind set into an overlapping view of the same
//! buffer (the source is read first), and a length-tracking view of a
//! resizable buffer.

const SOURCE: &str = r#"
"use strict";
const bad = [];
let cases = 0;
function bytes(ta) { return Array.from(new Uint8Array(ta.buffer, ta.byteOffset, ta.byteLength)).join(","); }
function check(label, got, want) { cases++; if (bytes(got) !== bytes(want)) bad.push(label); }
const kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
if (typeof Float16Array === "function") kinds.push(Float16Array);
const values = [0, -0, 1.5, -1.5, 2.5, 3.5, 255.5, 256, -129, 65537.25, 1e10, -1e10, 0.1, NaN, Infinity, -Infinity];
for (const K of kinds) {
  for (const v of values) {
    const a = new K(6), ra = new K(6);
    a.fill(v);
    for (let i = 0; i < 6; i++) ra[i] = v;
    check("fill " + K.name + " " + v, a, ra);
    const b = new K(7), rb = new K(7);
    b.fill(v, -5, -1);
    for (let i = 2; i < 6; i++) rb[i] = v;
    check("fill range " + K.name + " " + v, b, rb);
  }
}
const src64 = new Float64Array([NaN, -0, 1.5, -2.75, 300.25, -70000.5, 1e20, 0.1]);
const u64 = new Uint8Array(src64.buffer);
u64[7] = 0x7f; u64[6] = 0xf4; u64[0] = 0x55;
const src32 = new Float32Array([NaN, -0, 1.5, -2.75, 300.25, -70000.5, 1e20, 0.1]);
new Uint8Array(src32.buffer)[0] = 0x33;
const srcI = new Int16Array([-32768, -1, 0, 1, 32767, 300, -300, 7]);
for (const K of kinds) {
  for (const src of [src64, src32, srcI]) {
    if (src.constructor === K) continue;
    const d = new K(10), rd = new K(10);
    d.set(src, 1);
    for (let i = 0; i < src.length; i++) rd[1 + i] = src[i];
    check("set " + src.constructor.name + "->" + K.name, d, rd);
  }
}
function overlap(viaSet) {
  const buf = new ArrayBuffer(64);
  const f64 = new Float64Array(buf, 0, 4);
  f64[0] = 1.25; f64[1] = -2.5; f64[2] = 3.75; f64[3] = 1e300;
  const f32 = new Float32Array(buf, 4, 6);
  if (viaSet) f32.set(f64, 1);
  else { const snap = Array.from(f64); for (let i = 0; i < snap.length; i++) f32[1 + i] = snap[i]; }
  return new Uint8Array(buf);
}
check("overlap", overlap(true), overlap(false));
if (typeof ArrayBuffer.prototype.resize === "function") {
  function tracking(bulk) {
    const rb = new ArrayBuffer(8, { maxByteLength: 32 });
    const lt = new Int16Array(rb);
    rb.resize(24);
    const src = new Float32Array([7.9, -8.9, 9.5]);
    if (bulk) { lt.fill(-300, 2); lt.set(src, 5); }
    else { for (let i = 2; i < lt.length; i++) lt[i] = -300; for (let i = 0; i < 3; i++) lt[5 + i] = src[i]; }
    return lt;
  }
  check("length-tracking", tracking(true), tracking(false));
}
console.log("cases " + cases + " mismatches " + bad.length + " " + bad.slice(0, 5).join("; "));
"#;

#[test]
fn bulk_fill_and_set_store_what_element_stores_store() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let line = out.output.join("\n");
    assert!(line.contains(" mismatches 0 "), "{line}");
    let cases: usize = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(cases >= 300, "{line}");
}

/// The same program in the interpreter and under GC stress.
#[test]
fn bulk_fill_and_set_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [("nojit", ("ZIPP_NOJIT", "1")), ("gc", ("ZIPP_GC_STRESS", "1"))] {
        let out = std::process::Command::new(&exe)
            .args(["bulk_fill_and_set_store_what_element_stores_store", "--exact"])
            .env(env.0, env.1)
            .output()
            .expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
