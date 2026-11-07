//! Uint32Array reads on the unboxed INTEGER tiers.
//!
//! A Uint32 element zero-extends into its i64 home, so it may exceed
//! `i32::MAX`. These cases put such values through every consumer the tiers
//! emit: signed compares against negatives and doubles, i53 arithmetic
//! (including a product past 2^53 that must leave the tier), bitwise ops,
//! copies between homes, stores into other typed arrays and plain arrays, loop
//! phis that exit with the value, and a crc32. The expected lines are Node's.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    // One entry per console.log call; Node's side is split by line.
    out.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect()
}

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

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

const KERNELS: &str = r#"
"use strict";
const V = new Uint32Array([0, 1, 2, 0x7fffffff, 0x80000000, 0x80000001, 0xfffffffe, 0xffffffff,
  12345, 0xdeadbeef, 0x40000000, 0xc0000000, 3, 0x9e3779b9, 65536, 0xffff0000]);
const out = [];
// Signed compares: a u32 above 2^31 must stay above every negative and below 2^32.
function cmp(a, n) {
  let gt = 0, lt = 0, eq = 0, neg = 0, big = 0;
  for (let i = 0; i < n; i++) {
    const v = a[i & 15];
    if (v > 0x7fffffff) gt++;
    if (v < 0x80000000) lt++;
    if (v === 0xffffffff) eq++;
    if (v > -1) neg++;
    if (v >= 4294967295.5) big++;
  }
  return gt + "/" + lt + "/" + eq + "/" + neg + "/" + big;
}
// Arithmetic in the i53 range, plus products that overflow it.
function arith(a, n) {
  let s = 0, m = 0, d = 0, r = 0, q = 0;
  for (let i = 0; i < n; i++) {
    const v = a[i & 15], w = a[(i + 5) & 15];
    s = (s + v + 1) % 1000000007;
    d += v - w;
    r += v % 7;
    m = (m + v * 3) % 999983;
    if ((i & 255) === 0) q += v * w;
  }
  return s + " " + d + " " + r + " " + m + " " + q;
}
// Bitwise: ToInt32/ToUint32 of a wide value.
function bits(a, n) {
  let x = 0, u = 0, sh = 0, an = 0, o = 0;
  for (let i = 0; i < n; i++) {
    const v = a[i & 15];
    x ^= v;
    u = (u + (v >>> 3)) >>> 0;
    sh = (sh + (v >> 1)) | 0;
    an = (an + (v & 0xff)) | 0;
    o = Math.imul(o ^ v, 0x01000193);
  }
  return x + " " + u + " " + sh + " " + an + " " + o;
}
// Copies between homes, a loop-carried max that exits as the raw value, and
// negation (a negative zero never arises from a u32 but -v crosses below -2^31).
function carry(a, n) {
  let best = 0, last = 0, negs = 0;
  for (let i = 0; i < n; i++) {
    const v = a[i & 15];
    const c = v;
    if (c > best) best = c;
    last = c;
    negs += -v;
  }
  return [best, last, negs];
}
// Stores of a wide read into other arrays: Int32 wraps, Float64 keeps it, a
// plain array boxes it.
function stores(a, n) {
  const i32 = new Int32Array(16), f64 = new Float64Array(16), u32 = new Uint32Array(16), arr = new Array(16).fill(0);
  for (let i = 0; i < n; i++) {
    const v = a[i & 15];
    i32[i & 15] = v;
    f64[i & 15] = v;
    u32[i & 15] = v + 1;
    arr[i & 15] = v;
  }
  return Array.from(i32).join(",") + " | " + Array.from(f64).join(",") + " | " + Array.from(u32).join(",") + " | " + arr.join(",");
}
// A miss key (negative, fractional, out of range) leaves at the read.
function misses(a, n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    let k = i & 15;
    if (i === 701) k = -1;
    else if (i === 1301) k = 1.5;
    else if (i === 1901) k = 99;
    const v = a[k];
    s = (s + (v === undefined ? 7 : v)) % 1000003;
  }
  return s;
}
const T = new Uint32Array(256);
for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; T[n] = c >>> 0; }
function crc32(bytes, start, end) {
  let c = 0xffffffff;
  const t = T;
  for (let i = start; i < end; i++) c = t[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
const buf = new Uint8Array(4096);
for (let i = 0; i < buf.length; i++) buf[i] = (i * 31 + 7) & 255;
for (const n of [16, 64, 3000]) {
  out.push(cmp(V, n), arith(V, n), bits(V, n), carry(V, n).join(" "), stores(V, n), misses(V, n));
}
let h = 0;
for (let r = 0; r < 60; r++) h = (h + crc32(buf, r, buf.length)) % 4294967291;
out.push("crc " + crc32(buf, 0, buf.length) + " " + h + " " + crc32(new Uint8Array([0x31, 0x32, 0x33]), 0, 3));
// The pinned array changes between calls: the next read sees the new value.
const M = new Uint32Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
const m0 = arith(M, 3000); M[3] = 0xfffffff0; const m1 = arith(M, 3000);
out.push(m0 + " -> " + m1);
console.log(out.join("\n"));
"#;

#[test]
fn u32_parity_kernels() {
    assert_matches_node(KERNELS);
}

/// Child target for the JITLOG mechanism assertion below.
#[test]
fn u32_parity_mechanism_probe() {
    assert_matches_node(
        r#"
        function kernel(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a[i&7],33)+i)|0;return s}
        let a=new Uint32Array([0,1,2,3,0x80000000,5,0xfffffffe,0xffffffff]);
        console.log(kernel(a,64),kernel(a,2400));
        "#,
    );
}

/// Default mode compiles a Uint32 read on the INTEGER tier; the kill switch
/// restores the old decline.
#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn u32_mechanism_and_kill_switch() {
    let exe = std::env::current_exe().expect("test exe path");
    let run = |extra: &[(&str, &str)]| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("u32_parity_mechanism_probe")
            .arg("--nocapture")
            .env("ZIPP_JITLOG", "1")
            .env("ZIPP_JITDECLINE", "1");
        for &(k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn test binary")
    };
    let on = run(&[]);
    assert!(on.status.success(), "default child failed: {}", String::from_utf8_lossy(&on.stderr));
    let on_log = String::from_utf8_lossy(&on.stderr);
    assert!(
        on_log.contains("INT region") || on_log.contains("INT-GPR region"),
        "Uint32 probe did not engage the INTEGER tier:\n{on_log}"
    );
    let off = run(&[("ZIPP_NO_INT_TA_U32", "1")]);
    assert!(off.status.success(), "off child failed: {}", String::from_utf8_lossy(&off.stderr));
    let off_log = String::from_utf8_lossy(&off.stderr);
    assert!(
        off_log.contains("GetIndex") && off_log.contains("int-reject"),
        "off switch did not restore the GetIndex decline:\n{off_log}"
    );
}

/// The corpus again with the kill switch, the xmm INT emitter, eager JIT, GC at
/// every opportunity, and the interpreter, each in a fresh process.
#[test]
fn u32_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_INT_TA_U32", "1"),
        ("ZIPP_NO_GPR_HOMES", "1"),
        ("ZIPP_NO_GPR_LAZYSX", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("u32_parity_")
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
