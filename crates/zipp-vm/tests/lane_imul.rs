//! Integer products in typed lanes (`codegen::inline`, `imul_const`) against
//! Node: an inlined method body or leaf that multiplies an exact integer by a
//! positive constant does it as one integer multiply while the product stays
//! within 2^53, and as doubles otherwise. Method bodies over a class hierarchy
//! with `super` calls; every operand kind against constants of each sign and
//! size (zero, negatives, the int32 edges, the 2^53 boundary, a double field,
//! minus zero, NaN, a constant past 2^53, a fraction, the constant on the
//! left, a chain, a product wrapped by `| 0`, a square); a field that changes
//! kind between calls of one site; leaf functions feeding integer math. The
//! sign of every zero is printed. In the interpreter, by default and with
//! every function compiled at once.

const SOURCE: &str = r##""use strict";
const out = [];
const inv = (x) => (x === 0 ? (1 / x > 0 ? "+0" : "-0") : String(x));

// inlined method bodies that multiply an integer field by a constant
class Shape { constructor(v) { this._v = v; } area() { return this._v + 1; } }
class Circle extends Shape { area() { return super.area() * 3 + 1; } }
class Square extends Shape { constructor(v) { super(v); this.side = v * 2; } area() { return super.area() * 4 + this.side; } }
class Tri extends Shape { area() { return super.area() * 5 + 3; } }
class Hex extends Shape { area() { return (super.area() * 6 + 5) | 0; } }
{
  const objs = [new Circle(11), new Square(22), new Tri(33), new Hex(44)];
  let acc = 0;
  for (let i = 0; i < 40000; i++) acc = (acc + objs[i & 3].area()) | 0;
  out.push("A " + acc);
}

// every kind of operand a product can meet: zero, negatives, the int32 edges,
// a field that is a double, minus zero, NaN; and constants of each sign
class P {
  constructor(v) { this.v = v; }
  k3() { return this.v * 3; }
  k1() { return this.v * 1; }
  k0() { return this.v * 0; }
  kn() { return this.v * -3; }
  kbig() { return this.v * 2147483647; }
  khuge() { return this.v * 9007199254740993; }
  kf() { return this.v * 2.5; }
  rev() { return 7 * this.v; }
  chain() { return (this.v * 3 + 1) * 5 - 2; }
  wrap() { return (this.v * 1000003) | 0; }
  sq() { return this.v * this.v; }
}
const vals = [0, 1, -1, 7, -7, 2147483647, -2147483648, 715827882, 715827883, 1.5, -0, NaN, 4294967296, -0.5];
const names = ["k3", "k1", "k0", "kn", "kbig", "khuge", "kf", "rev", "chain", "wrap", "sq"];
function run(o, name) {
  let last;
  switch (name) {
    case "k3": for (let i = 0; i < 300; i++) last = o.k3(); break;
    case "k1": for (let i = 0; i < 300; i++) last = o.k1(); break;
    case "k0": for (let i = 0; i < 300; i++) last = o.k0(); break;
    case "kn": for (let i = 0; i < 300; i++) last = o.kn(); break;
    case "kbig": for (let i = 0; i < 300; i++) last = o.kbig(); break;
    case "khuge": for (let i = 0; i < 300; i++) last = o.khuge(); break;
    case "kf": for (let i = 0; i < 300; i++) last = o.kf(); break;
    case "rev": for (let i = 0; i < 300; i++) last = o.rev(); break;
    case "chain": for (let i = 0; i < 300; i++) last = o.chain(); break;
    case "wrap": for (let i = 0; i < 300; i++) last = o.wrap(); break;
    default: for (let i = 0; i < 300; i++) last = o.sq(); break;
  }
  return last;
}
for (const name of names) {
  const row = [];
  for (const v of vals) row.push(inv(run(new P(v), name)));
  out.push(name + " " + row.join(" "));
}

// one receiver whose field changes kind between calls of the same site
{
  const o = new P(5); const seen = [];
  const set = [5, -0, 2.25, 0, -3, 1e300, 2147483647, "4", null, undefined, true];
  for (let r = 0; r < 60; r++) {
    for (const v of set) { o.v = v; seen.push(inv(o.k3()) + "/" + inv(o.kn()) + "/" + inv(o.chain())); }
    if (r === 0) out.push("M " + seen.join(" "));
    seen.length = 0;
  }
}

// leaf functions called from a hot loop, the product feeding integer math
function scale(x) { return x * 31 + 7; }
function mix(a, b) { return (a * 16777619) ^ (b * 3); }
{
  let h = 0, s = 0;
  for (let i = -20000; i < 20000; i++) { h = (h + scale(i)) | 0; s = (s + mix(i, h)) | 0; }
  out.push("L " + h + " " + s + " " + inv(scale(0)) + " " + inv(scale(-0)) + " " + inv(mix(0, -0)));
}
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "A 6210000\nk3 +0 3 -3 21 -21 6442450941 -6442450944 2147483646 2147483649 4.5 -0 NaN 12884901888 -1.5\nk1 +0 1 -1 7 -7 2147483647 -2147483648 715827882 715827883 1.5 -0 NaN 4294967296 -0.5\nk0 +0 +0 -0 +0 -0 +0 -0 +0 +0 +0 -0 NaN +0 -0\nkn -0 -3 3 -21 21 -6442450941 6442450944 -2147483646 -2147483649 -4.5 +0 NaN -12884901888 1.5\nkbig +0 2147483647 -2147483647 15032385529 -15032385529 4611686014132420600 -4611686016279904000 1537228670661645600 1537228672809129200 3221225470.5 -0 NaN 9223372032559809000 -1073741823.5\nkhuge +0 9007199254740992 -9007199254740992 63050394783186940 -63050394783186940 1.9342813104826868e+25 -1.9342813113834067e+25 6.447604365273223e+24 6.447604374280422e+24 13510798882111488 -0 NaN 3.8685626227668134e+25 -4503599627370496\nkf +0 2.5 -2.5 17.5 -17.5 5368709117.5 -5368709120 1789569705 1789569707.5 3.75 -0 NaN 10737418240 -1.25\nrev +0 7 -7 49 -49 15032385529 -15032385536 5010795174 5010795181 10.5 -0 NaN 30064771072 -3.5\nchain 3 18 -12 108 -102 32212254708 -32212254717 10737418233 10737418248 25.5 3 NaN 64424509443 -4.5\nwrap +0 1000003 -1000003 7000021 -7000021 2146483645 -2147483648 715161214 716161217 1500004 +0 +0 +0 -500001\nsq +0 1 1 49 49 4611686014132420600 4611686018427388000 512409556648605950 512409558080261700 2.25 +0 NaN 18446744073709552000 0.25\nM 15/-15/78 -0/+0/3 6.75/-6.75/36.75 +0/-0/3 -9/9/-42 3e+300/-3e+300/1.5e+301 6442450941/-6442450941/32212254708 12/-12/63 +0/-0/3 NaN/NaN/NaN 3/-3/18\nL -340000 1762796224 7 7 +0";

#[test]
fn lane_imul_worker() {
    if std::env::var_os("ZIPP_LANE_IMUL_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn lane_imul_match_node() {
    if std::env::var_os("ZIPP_LANE_IMUL_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["lane_imul_worker", "--exact", "--nocapture"])
            .env("ZIPP_LANE_IMUL_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some(v) = var {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
