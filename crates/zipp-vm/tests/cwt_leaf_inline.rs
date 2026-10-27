//! A leaf callee called as `o.f(<arguments>)` (`CallWithThis`: the callee
//! read before its arguments), which Tier C inlines under a FuncProto guard
//! when the callee captures nothing and never reads `this`: a comparator
//! field holding a fresh arrow per owner, a plain function, a method that
//! reads `this` (not inlined), one site rotating through four functions, a
//! field replaced mid-loop, a body whose element read meets a getter, a hole,
//! a string and `null`, callees taking fewer and more arguments than passed,
//! and a callee that throws inside a `try`. Run at the default thresholds
//! and with every function compiled at once; the expected lines are Node
//! 24's.

const EXPECTED: &[&str] = &[
    r#"rot0 421650304"#,
    r#"rot1 421650304"#,
    r#"rot2 421650304"#,
    r#"rot3 421650304"#,
    r#"rot4 421650304"#,
    r#"rot5 421650304"#,
    r#"plain -421650304"#,
    r#"this-reader 1456118280"#,
    r#"poly0 -1843011648"#,
    r#"poly1 858966088"#,
    r#"poly2 -1351181120"#,
    r#"poly3 -2042757052"#,
    r#"poly4 -1843011648"#,
    r#"poly5 858966088"#,
    r#"poly6 -1351181120"#,
    r#"poly7 -2042757052"#,
    r#"swap 12000000"#,
    r#"idx -12497500"#,
    r#"TypeError"#,
    r#"hole NaN"#,
    r#"str NaN"#,
    r#"getter NaN"#,
    r#"arity 16408000"#,
    r#"caught 1713"#,
    r#"recv 16016000"#,
];

fn check(threshold: Option<&str>) {
    // The JIT threshold is read once per process, so each setting runs in a
    // child of this test binary.
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["--exact", "cwt_leaf_child", "--nocapture"])
        .env("ZIPP_CWT_LEAF_CHILD", "1");
    if let Some(t) = threshold {
        cmd.env("ZIPP_JIT_THRESHOLD", t);
    }
    let out = cmd.output().expect("re-run test binary");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cwt_leaf_child() {
    if std::env::var_os("ZIPP_CWT_LEAF_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED);
}

#[test]
fn leaf_callees_of_member_calls_inline_as_node_runs_them() {
    if std::env::var_os("ZIPP_CWT_LEAF_CHILD").is_some() {
        return;
    }
    check(None);
    check(Some("1"));
}

const PROGRAM: &str = r####"// Leaf callees called as `o.f(<args with side effects>)` (CallWithThis).
const out = [];
class Box {
  constructor(cmp) { this.cmp = cmp; this.n = 0; }
  test(a, b) { return this.cmp(a[0], b[0]); }
  testIdx(arr, i, j) { return this.cmp(arr[i], arr[j]); }
}
function run(label, box, n, mk) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    const r = box.test(mk(i), mk(i + 1));
    s = (s * 31 + (typeof r === "number" ? r : String(r).length)) | 0;
  }
  out.push(label + " " + s);
}
const tup = (i) => [i % 17, i];
// One arrow per owner, rotated: same FuncProto, different closures.
for (let k = 0; k < 6; k++) run("rot" + k, new Box((a, b) => a - b), 3000, tup);
// A plain function and a method that reads `this` (declined: reads this).
run("plain", new Box(function (a, b) { return b - a; }), 3000, tup);
const reader = new Box(function (a, b) { return this.n + a + b; });
reader.n = 5;
run("this-reader", reader, 3000, tup);
// Polymorphic across protos at one site.
const fs = [(a, b) => a - b, (a, b) => a * b, (a, b) => a === b, function (a, b) { return a + b; }];
for (let k = 0; k < 8; k++) run("poly" + k, new Box(fs[k % 4]), 1500, tup);
// Field replaced mid-loop.
const sw = new Box((a, b) => a - b);
let acc = 0;
for (let i = 0; i < 6000; i++) {
  if (i === 3000) sw.cmp = (a, b) => b - a + 1000;
  acc = (acc + sw.test([i], [i * 2])) | 0;
}
out.push("swap " + acc);
// A callee whose element read hits a getter, an array with a hole, a string.
const getterObj = [];
Object.defineProperty(getterObj, 0, { get() { out.push("getter"); return 7; } });
const idx = new Box((a, b) => a[1] - b[1]);
let t = 0;
for (let i = 0; i < 5000; i++) t += idx.testIdx([[0, i], [0, 2 * i]], 0, 1);
out.push("idx " + t);
try { idx.testIdx([[0, 1], null], 0, 1); } catch (e) { out.push(e.constructor.name); }
out.push("hole " + idx.testIdx([[0, 1], [0, , 3]], 0, 1));
out.push("str " + idx.testIdx(["ab", "cd"], 0, 1));
out.push("getter " + idx.testIdx([getterObj, [0, 2]], 1, 0));
// Arity mismatches.
const one = new Box((a) => a * 2);
const three = new Box((a, b, c) => [a, b, c].length + (c === undefined ? 100 : 0));
let z = 0;
for (let i = 0; i < 4000; i++) z += one.test([i], [1]) + three.test([i], [2]);
out.push("arity " + z);
// A throwing callee inside try.
const thrower = new Box((a, b) => { if (a > b) throw new RangeError("gt"); return a; });
let caught = 0;
for (let i = 0; i < 4000; i++) { try { thrower.test([i % 7], [3]); } catch (e) { caught++; } }
out.push("caught " + caught);
// The receiver the callee would see if it were a method: unobservable here.
const recv = { k: 3, f: (a, b) => a + b, g(a, b) { return a + b + this.k; } };
let q = 0;
for (let i = 0; i < 4000; i++) q += recv.f([i][0], [1][0]) + recv.g([i][0], [1][0]);
out.push("recv " + q);
console.log(out.join("\n"));
"####;
