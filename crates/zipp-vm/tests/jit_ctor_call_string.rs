//! Compiled code calling a built-in constructor as a function (`BigInt(x)`,
//! `Number(s)`, `Boolean(v)`, `Array(n)`, `Map()` throwing) through the call
//! fallback's `call_value`, and Tier C's `String(x)` on doubles, primitive
//! strings and the values it still declines (booleans, null, undefined,
//! objects with `toString`, Symbols). The kernels run hot enough for Tier C;
//! the expected lines are Node's.

const SOURCE: &str = r##"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const t = (f) => { try { return f(); } catch (e) { return e.constructor.name; } };
let seed = 7;
function u32() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return seed >>> 0; }
function randBig(words) {
  let x = 0n;
  for (let k = 0; k < words; k++) x = (x << 32n) | BigInt(u32());
  return x;
}
function ctorCalls(i) {
  const n = Number(String(i * 3) + "5"), b = Boolean(i & 1), a = Array(i & 3), o = Object(i);
  return n + (b ? 1 : 0) + a.length + (typeof o === "object" ? 10 : 0) + Number(BigInt(i) * 3n);
}
function str(v) { return String(v); }
let h = 0n, s = 0;
for (let r = 0; r < 4000; r++) {
  h ^= randBig(1 + (r & 3));
  s = (s + ctorCalls(r)) % 1000003;
}
log("warm", String(h & 0xffffffffffn), s);
const vals = [0, -0, 1.5, -2.25, NaN, Infinity, -Infinity, 1e21, 1e-7, 123456789.125, 2 ** 53, 5e-324, "s", "", "é",
  true, false, null, undefined, { toString() { return "obj"; } }, [1, 2], 7, -7];
let acc = "";
for (let r = 0; r < 3000; r++) acc = str(vals[r % vals.length]);
log("str", vals.map(str).join("|"), acc);
log("str symbol", t(() => str(Symbol("q"))), t(() => str(Object(Symbol("w")))));
log("bigint errors", t(() => BigInt(1.5)), t(() => BigInt("x")), t(() => BigInt(undefined)), String(BigInt("0x1f")));
function mapCall() { return Map(); }
function dateCall() { return typeof Date(); }
let thrown = 0;
for (let r = 0; r < 2000; r++) { try { mapCall(); } catch (e) { thrown++; } }
log("ctor throws", thrown, dateCall(), typeof Symbol("z"), t(() => Promise()), t(() => Uint8Array(2)));
let calls = 0;
const obj = { valueOf() { calls++; return 41; } };
let nsum = 0;
for (let r = 0; r < 2000; r++) nsum += Number(obj) + Number(BigInt(r & 7));
log("valueOf", nsum, calls);
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
fn jit_ctor_call_string_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    assert_eq!(got, node_output(SOURCE));
}

/// The corpus again with the constructor lane off, eager JIT, GC stress and
/// the interpreter, each in a fresh process.
#[test]
fn jit_ctor_call_string_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_JIT_CTOR_CALL", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["jit_ctor_call_string_parity", "--exact"])
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
