//! A Tier C body that reaches a loop owned by a register-homed region runs
//! that region in place (`jit_tierc_run_region`) instead of bailing to the
//! interpreter at the loop head. The kernels are short loops in functions
//! called from hot compiled callers, so Tier C enters them both framed and
//! frame-free. They cover `break` exits, early returns, regions that deopt
//! mid-loop (an element turning into a double, a string into a rope, an index
//! out of range), nested loops, and loops reading globals. The expected lines
//! are Node's.

const SOURCE: &str = r##"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const T = new Uint32Array(256);
for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; T[n] = c >>> 0; }
function crc32(bytes, start, end) {
  let c = 0xffffffff;
  const t = T;
  for (let i = start; i < end; i++) c = t[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function fnv(h, s) {
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}
function firstOver(a, lim) {
  let i = 0;
  for (; i < a.length; i++) { if (a[i] > lim) break; }
  return i;
}
function sumTo(a, n) {
  let s = 0;
  for (let i = 0; i < n; i++) s = (s + a[i]) | 0;
  return s;
}
let G = 3;
function globalLoop(n) {
  let s = 0;
  for (let i = 0; i < n; i++) s = (s + G * i) | 0;
  G = (G + 1) & 7;
  return s;
}
function nested(a, rows) {
  let h = 0;
  for (let r = 0; r < rows; r++) {
    h = (h + sumTo(a, (r & 7) + 1)) | 0;
    for (let k = 0; k < 4; k++) h = (h * 31 + k) | 0;
  }
  return h;
}
const buf = new Uint8Array(4096);
for (let i = 0; i < buf.length; i++) buf[i] = (i * 31 + 7) & 255;
const ints = [];
for (let i = 0; i < 64; i++) ints.push(i * 3);
const words = ["", "a", "ab", "hello", "zipp engine", "été", "x".repeat(40)];
let h = 0;
for (let r = 0; r < 3000; r++) {
  h = (h + crc32(buf, r & 255, (r & 255) + 64)) | 0;
  h = (h + crc32(buf.subarray(r & 63, (r & 63) + 16), 0, 16)) | 0;
  h = fnv(h, words[r % words.length]);
  h = (h + firstOver(ints, r % 200)) | 0;
  h = (h + sumTo(ints, r & 63) + globalLoop(r & 15) + nested(ints, 3)) | 0;
}
log("warm", h >>> 0);
// Deopts inside the region: a double element, a hole, a non-int string length source, an out-of-range end.
ints[10] = 2.5; log("double", sumTo(ints, 20), firstOver(ints, 2), nested(ints, 9));
ints[10] = 30; ints[70] = 1; log("hole", sumTo(ints, 71), firstOver(ints, 1e9));
log("rope", fnv(7, "ab".repeat(5) + "cd"), fnv(7, words[4] + words[3]));
log("range", crc32(buf, 4090, 4100), crc32(new Uint8Array(0), 0, 0), crc32([1, 2, 3], 0, 3));
G = 1.5; log("global double", globalLoop(9), globalLoop(9));
let h2 = 0;
for (let r = 0; r < 2000; r++) h2 = (h2 + crc32(buf, r & 1023, (r & 1023) + (r & 63)) + fnv(r, words[r % 7]) + sumTo(ints, r & 63)) | 0;
log("after", h2 >>> 0);
console.log(out.join("\n"));
"##;

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn tierc_yield_entry_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    assert_eq!(got, node_output(SOURCE));
}

/// The corpus again with the entry switched off, with the xmm INT emitter,
/// eager JIT, GC stress and the interpreter, each in a fresh process.
#[test]
fn tierc_yield_entry_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_TIERC_YIELD_ENTRY", "1"),
        ("ZIPP_NO_GPR_HOMES", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["tierc_yield_entry_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}
