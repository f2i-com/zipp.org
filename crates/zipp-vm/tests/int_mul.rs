//! `a * b` in the boxed compiled tiers (`dbinop`): an Int product of two Ints
//! is an Int, as the interpreter's, and every product no Int holds takes the
//! f64 path: -0 from a zero times a negative, i32 overflow either way,
//! fractions, NaN, infinities, coercing operands (which leave compiled code)
//! and BigInts. Products as element keys read and written, hashes that
//! overflow on most iterations, and a -0 observed through division. Run
//! interpreted, at the default thresholds, with every function compiled at
//! once, under GC stress, with the int product off and with the f64 path's
//! operand reload back; the expected line is Node 24's.

const EXPECTED: &str = "12,-12,-12,12,0,0,-0,-0,0,-0,4294967296,4294967295,-2147483648,-2147549184,2147488281,2147395600,2147483647,4294967294,-2147483648,2147483648,2147483648,3,1,NaN,NaN,-0,-0,Infinity,19220000,1999.0.2000.0.2001.0.2002.0.2003.0.2004.0.2005.0.2006.0,-458123433,878381,12157665459056929000,Infinity,-Infinity,-Infinity,Infinity,-0.3333333333333333,42,12,5,0,NaN,501,TypeError,bigint";

#[test]
fn int_mul_child() {
    if std::env::var_os("ZIPP_INT_MUL_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn compiled_products_match_the_interpreter() {
    if std::env::var_os("ZIPP_INT_MUL_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("int-mul-off", Some(("ZIPP_NO_INT_MUL", "1"))),
        ("split-off", Some(("ZIPP_NO_NUM_SPLIT", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "int_mul_child", "--nocapture"])
            .env("ZIPP_INT_MUL_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// `a * b` in compiled code: an Int product of two Ints stays an Int, and
// every product no Int can hold takes the f64 path.
"use strict";
const out = [];
function mul(a, b) { return a * b; }
function key(a, i) { return a[2 * i + 1]; }
function put(a, i, v) { a[2 * i] = v; return a.length; }
const cases = [
  [3, 4], [-3, 4], [3, -4], [-3, -4], [0, 5], [5, 0], [0, -5], [-5, 0], [0, 0], [-1, 0],
  [65536, 65536], [65535, 65537], [-65536, 32768], [-65536, 32769], [46341, 46341], [46340, 46340],
  [2147483647, 1], [2147483647, 2], [-2147483648, 1], [-2147483648, -1], [-1, -2147483648],
  [1.5, 2], [2, 0.5], [NaN, 3], [Infinity, 0], [-0, 3], [3, -0], [1e300, 1e10],
];
for (let r = 0; r < 400; r++) for (const [a, b] of cases) mul(a, b);
for (const [a, b] of cases) {
  const p = mul(a, b);
  out.push(Object.is(p, -0) ? "-0" : String(p));
}
// Keys from a product, read and written.
const arr = []; for (let k = 0; k < 64; k++) arr.push(k * 10);
let s = 0; for (let r = 0; r < 2000; r++) for (let i = 0; i < 31; i++) s += key(arr, i);
out.push(s);
const w = new Array(16).fill(0); for (let r = 0; r < 2000; r++) for (let i = 0; i < 8; i++) put(w, i, r + i);
out.push(w.join("."));
// A hash that overflows on most iterations, and one kept in range by `| 0`.
function h1(n) { let h = 7; for (let k = 0; k < n; k++) h = (h * 31 + k) | 0; return h; }
function h2(n) { let h = 1; for (let k = 0; k < n; k++) h = (h * 33) % 1000003; return h; }
function h3(n) { let x = 1; for (let k = 0; k < n; k++) x = x * 3; return x; }
out.push(h1(100000), h2(100000), h3(40), h3(800));
// Products feeding a compare and a -0 observed through division.
function sign(a, b) { const p = a * b; return 1 / p; }
for (let r = 0; r < 2000; r++) sign(r & 3, (r & 1) ? -1 : 1);
out.push(sign(0, -1), sign(-2, 0), sign(0, 1), sign(3, -1));
// Coercing operands go through the general path.
let calls = 0; const o = { valueOf() { calls++; return 6; } };
for (let r = 0; r < 500; r++) mul(o, 7);
out.push(mul(o, 7), mul("3", 4), mul(true, 5), mul(null, 2), mul(undefined, 2), calls);
try { mul(1n, 2); } catch (e) { out.push(e.constructor.name); }
out.push(typeof mul(2n, 3n) === "bigint" ? "bigint" : "x");
console.log(out.join(","));
"####;
