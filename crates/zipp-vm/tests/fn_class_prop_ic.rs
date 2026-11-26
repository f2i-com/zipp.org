//! Regression tests for Workstream A4: caching own and inherited data
//! properties of functions (`fn_props` / prototype chain) and classes
//! (`ClassData::statics` / superclass chain) in `IcEntry` and `dict_prop_memo`,
//! including mutation, deletion, `defineProperty` accessor conversion,
//! `setPrototypeOf`, growth/reallocation, and megamorphic reads.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
const out = [];

// 1. Monomorphic own function property: read, mutate in place, grow fn_props
//    (forcing Vec reallocation), redefine as getter, delete, re-add.
function f1() { return 1; }
f1.flag = 10;
function readFlag(f) { return f.flag; }
let acc = 0;
for (let i = 0; i < 600; i++) acc += readFlag(f1);
out.push("f1-initial=" + acc);

f1.flag = 25;
acc = 0;
for (let i = 0; i < 400; i++) acc += readFlag(f1);
out.push("f1-mutated=" + acc);

// Grow fn_props past several Vec reallocations while reading f1.flag.
for (let k = 0; k < 40; k++) {
  f1["extra_" + k] = k;
  if (readFlag(f1) !== 25) throw new Error("stale after grow " + k);
}
out.push("f1-after-grow=" + readFlag(f1));

// Convert to getter via defineProperty.
let gets = 0;
Object.defineProperty(f1, "flag", {
  configurable: true,
  get() { gets++; return 100 + gets; }
});
out.push("f1-getter=" + readFlag(f1) + "," + readFlag(f1));

// Delete and verify undefined, then re-add as data.
delete f1.flag;
out.push("f1-deleted=" + String(readFlag(f1)));
f1.flag = 77;
out.push("f1-readded=" + readFlag(f1));

// 2. Megamorphic function own-property reads (hits dict_prop_memo) +
//    per-function growth, mutation, deletion, and getter conversion.
const fns = [];
for (let i = 0; i < 20; i++) {
  const g = () => i;
  g.tag = i * 3;
  fns.push(g);
}
function readMegaFn(arr) {
  let s = 0;
  for (let i = 0; i < 600; i++) s += arr[i % arr.length].tag;
  return s;
}
out.push("mega-fn-1=" + readMegaFn(fns));
fns[7].tag = 999;
for (let k = 0; k < 32; k++) fns[7]["p" + k] = k;
out.push("mega-fn-2=" + readMegaFn(fns));
Object.defineProperty(fns[3], "tag", { configurable: true, get() { return 500; } });
delete fns[5].tag;
fns[5].tag = 55;
out.push("mega-fn-3=" + readMegaFn(fns));

// 3. Monomorphic + inherited Class static data properties + shadowing +
//    parent mutation + parent deletion + setPrototypeOf + getter override.
class Base {
  static TEXT_NODE = 3;
  static ELEMENT_NODE = 1;
}
class Mid extends Base {}
class Sub extends Mid {}

function readStatic(C) { return C.TEXT_NODE; }
let cs = 0;
for (let i = 0; i < 600; i++) cs += readStatic(Base) + readStatic(Sub);
out.push("class-1=" + cs);

Base.TEXT_NODE = 9;
for (let k = 0; k < 32; k++) Base["pad_" + k] = k;
out.push("class-2=" + readStatic(Base) + "," + readStatic(Sub));

Mid.TEXT_NODE = 42;
out.push("class-shadow-mid=" + readStatic(Base) + "," + readStatic(Mid) + "," + readStatic(Sub));
delete Mid.TEXT_NODE;
out.push("class-unshadow-mid=" + readStatic(Sub));

Object.defineProperty(Sub, "TEXT_NODE", { configurable: true, get() { return 88; } });
out.push("class-sub-getter=" + readStatic(Sub));
delete Sub.TEXT_NODE;
out.push("class-sub-restored=" + readStatic(Sub));

const Alt = { TEXT_NODE: 123 };
Object.setPrototypeOf(Sub, Alt);
out.push("class-setproto=" + readStatic(Sub));

// 4. Megamorphic Class static reads (hits dict_prop_memo) + reallocation.
const classes = [];
for (let i = 0; i < 20; i++) {
  class K {}
  K.code = i + 10;
  classes.push(K);
}
function readMegaClass(arr) {
  let s = 0;
  for (let i = 0; i < 600; i++) s += arr[i % arr.length].code;
  return s;
}
out.push("mega-cls-1=" + readMegaClass(classes));
for (let k = 0; k < 32; k++) classes[9]["x" + k] = k;
classes[9].code = 777;
Object.defineProperty(classes[2], "code", { configurable: true, get() { return 333; } });
out.push("mega-cls-2=" + readMegaClass(classes));

// 5. Inherited Function.prototype property + shadowing + proto mutation +
//    Bound function own prop + null-proto function.
Function.prototype.customVal = 11;
function readCustom(fn) { return fn.customVal; }
function plainFn() {}
const boundFn = plainFn.bind(null);
boundFn.ownX = 5;
function readOwnX(fn) { return fn.ownX; }
let psum = 0;
for (let i = 0; i < 500; i++) psum += readCustom(plainFn) + readOwnX(boundFn);
out.push("proto-1=" + psum);
Function.prototype.customVal = 22;
out.push("proto-mut=" + readCustom(plainFn));
plainFn.customVal = 33;
out.push("proto-shadow=" + readCustom(plainFn));
delete plainFn.customVal;
out.push("proto-unshadow=" + readCustom(plainFn));
Object.setPrototypeOf(plainFn, null);
out.push("proto-null=" + String(readCustom(plainFn)));
delete Function.prototype.customVal;

// 6. Verify synthesized keys (name/length/prototype) are never mis-cached.
function namedFn(a, b) {}
function readMeta(fn) { return fn.name + ":" + fn.length + ":" + (typeof fn.prototype); }
let meta = "";
for (let i = 0; i < 400; i++) meta = readMeta(namedFn);
out.push("meta=" + meta);
delete namedFn.name;
out.push("meta-del=" + readMeta(namedFn));

console.log(out.join("\n"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
        .iter()
        .flat_map(|l| l.lines().map(str::to_string))
        .collect()
}

#[test]
fn fn_and_class_prop_ic_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "f1-initial=6000",
            "f1-mutated=10000",
            "f1-after-grow=25",
            "f1-getter=101,102",
            "f1-deleted=undefined",
            "f1-readded=77",
            "mega-fn-1=17100",
            "mega-fn-2=46440",
            "mega-fn-3=62370",
            "class-1=3600",
            "class-2=9,9",
            "class-shadow-mid=9,42,42",
            "class-unshadow-mid=9",
            "class-sub-getter=88",
            "class-sub-restored=9",
            "class-setproto=123",
            "mega-cls-1=11700",
            "mega-cls-2=44070",
            "proto-1=8000",
            "proto-mut=22",
            "proto-shadow=33",
            "proto-unshadow=22",
            "proto-null=undefined",
            "meta=namedFn:2:object",
            "meta-del=:2:object",
        ]
    );
}

#[test]
fn fn_and_class_prop_ic_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("fn_and_class_prop_ic_answers_as_node")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
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
