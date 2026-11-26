//! Regression tests for Workstream C3: inline `ReadyCells` (`MakeCell` /
//! `MakeCellTdz`) and inline `ReadyClosures` (`MakeClosure` / `MakeArrow`),
//! including upvalue counts 0..=9+, multi-level `ParentUpval` capture,
//! lexical `this` / `new.target` / `super` / `#private` / direct `eval`,
//! TDZ & const cells, and side-table hygiene across recycled ready slots.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
const out = [];

// 1. Hot loop allocating 0, 1, 2, 4, 8, and 9-upvalue closures & arrows
//    (spans READY_CLOSURE_UPVALS=8 boundary and triggers pool refills + minors).
function makeBundle(n) {
  let a = n, b = n + 1, c = n + 2, d = n + 3;
  let e = n + 4, f = n + 5, g = n + 6, h = n + 7, k = n + 8;
  const f0 = () => 1;
  const f1 = () => { a += 1; return a; };
  const f2 = function () { return a + b; };
  const f4 = () => a + b + c + d;
  const f8 = () => a + b + c + d + e + f + g + h;
  const f9 = () => a + b + c + d + e + f + g + h + k;
  return f0() + f1() + f2() + f4() + f8() + f9();
}
let s1 = 0;
for (let i = 0; i < 600; i++) s1 += makeBundle(i & 15);
out.push("bundle=" + s1);

// 2. Multi-level nested closures (ParentLocal -> ParentUpval -> ParentUpval)
//    + young object captured in arrow lexical `this` surviving minors.
function outerNested(seed) {
  let x = seed;
  let obj = { v: seed * 2 };
  function mid(y) {
    let z = y + 1;
    const arrow = () => {
      x += z;
      return x + obj.v + this.bonus;
    };
    return arrow;
  }
  return mid.call({ bonus: seed & 7 }, seed & 3);
}
const keptNested = [];
for (let i = 0; i < 600; i++) {
  const fn = outerNested(i);
  if (i % 100 === 0) keptNested.push(fn);
  else fn();
}
// Churn to force minors & slot recycling before invoking keptNested.
for (let i = 0; i < 800; i++) {
  let t = i;
  (() => t + 1)();
}
out.push("nested=" + keptNested.map(fn => fn()).join(","));

// 3. TDZ cell (`MakeCellTdz`) ReferenceError before initialization, and const cell TypeError.
function testTdz(trigger) {
  const read = () => val;
  if (trigger) {
    try { read(); return "no-err"; } catch (e) { return e.name; }
  }
  let val = 42;
  return read();
}
let tdzOk = 0;
for (let i = 0; i < 400; i++) {
  if (testTdz(i % 2 === 0) === (i % 2 === 0 ? "ReferenceError" : 42)) tdzOk++;
}
out.push("tdz=" + tdzOk);

// 4. Non-plain arrows that MUST preserve new.target, super.x, #private, and direct eval.
class Base {
  #secret = 10;
  tag() { return "B"; }
  getSecretFn() { return () => this.#secret; }
}
class Sub extends Base {
  constructor(v) {
    super();
    this.ntFn = () => new.target.name;
    this.supFn = () => super.tag() + ":" + v;
  }
}
let clsOk = 0;
for (let i = 0; i < 300; i++) {
  const s = new Sub(i);
  if (s.ntFn() === "Sub" && s.supFn() === "B:" + i && s.getSecretFn()() === 10) clsOk++;
}
out.push("class-arrows=" + clsOk);

function evalScopeTest(x) {
  eval("var hidden = x + 5;");
  const reader = () => hidden;
  return reader();
}
let evalSum = 0;
for (let i = 0; i < 200; i++) evalSum += evalScopeTest(i);
out.push("eval-scope=" + evalSum);

// 5. Side-table hygiene on recycled ready closures: attach fn_props and prototype,
//    let them die and recycle, ensure fresh closures do not inherit stale props.
for (let i = 0; i < 500; i++) {
  let k = i;
  const f = function () { return k; };
  f.customTag = i;
  void f.prototype;
}
let stale = 0;
for (let i = 0; i < 500; i++) {
  let k = i;
  const f = function () { return k; };
  if ("customTag" in f) stale++;
}
out.push("hygiene-stale=" + stale);

for (const line of out) print(line);
"#;

#[test]
fn ready_cells_and_closures_answers_as_node() {
    let lines = zipp_vm::run(PROGRAM).expect("program runs").output;
    assert_eq!(
        lines,
        vec![
            "bundle=153432",
            "nested=1,305,601,905,1201,1505",
            "tdz=400",
            "class-arrows=300",
            "eval-scope=20900",
            "hygiene-stale=0",
        ]
    );
}

#[test]
fn ready_cells_and_closures_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, envs) in [
        ("eager", vec![("ZIPP_JIT_THRESHOLD", "1")]),
        ("stress", vec![("ZIPP_GC_STRESS", "1")]),
        ("nojit", vec![("ZIPP_NOJIT", "1")]),
        (
            "nursery-verify",
            vec![
                ("ZIPP_NURSERY_VERIFY", "1"),
                ("ZIPP_NURSERY_YOUNG_BUDGET", "256"),
            ],
        ),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("ready_cells_and_closures_answers_as_node")
            .arg("--nocapture");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
