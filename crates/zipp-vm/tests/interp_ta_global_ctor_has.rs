//! Regression tests for Commit 2A: shift-based TypedArray numeric reads
//! (`ta_interp_numeric_get_view` / `ta_interp_numeric_get_idx`), inline
//! 512-byte bitset for `global_real_own_route`, shared `resolve_ctor_chain_target`
//! in `construct_class` / `run_class_ctor`, single-pass `plain_string_key_has`
//! in `has_property_jit`, and deduplicated ASCII `toLowerCase`/`toUpperCase`.
//!
//! Expected output lines are Node 24's.

const PROGRAM: &str = r##"
// 1. TypedArray numeric reads across all kinds, offsets, resizable/tracking, detached.
function testTypedArrays() {
  const ab = new ArrayBuffer(32);
  const u8 = new Uint8Array(ab);
  for (let i = 0; i < 32; i++) u8[i] = (i * 37 + 11) & 255;
  const i8 = new Int8Array(ab, 4, 8);
  const u8c = new Uint8ClampedArray(ab, 2, 10);
  const i16 = new Int16Array(ab, 2, 8);
  const u16 = new Uint16Array(ab, 4, 12);
  const i32 = new Int32Array(ab, 4, 6);
  const u32 = new Uint32Array(ab, 0, 8);
  u32[0] = 0x80000005; // > i32::MAX
  const f32 = new Float32Array(4);
  f32[0] = 1.5; f32[1] = -2.25;
  const f64 = new Float64Array(4);
  f64[0] = 3.125; f64[1] = -0.5;

  // Tight loop exercising lite loop `ta_interp_numeric_get_idx`
  function sumU16(arr) {
    let s = 0;
    for (let k = 0; k < 200; k++) {
      for (let j = 0; j < arr.length; j++) {
        s = (s + arr[j]) | 0;
      }
    }
    return s;
  }
  function sumU8(arr) {
    let s = 0;
    for (let k = 0; k < 200; k++) {
      for (let j = 0; j < arr.length; j++) {
        s = (s + arr[j]) | 0;
      }
    }
    return s;
  }
  const s16 = sumU16(u16);
  const s8 = sumU8(u8);
  const kinds = [
    i8[0], i8[7], i8[8],
    u8c[0], u8c[9], u8c[-1],
    i16[0], i16[7],
    u16[0], u16[11], u16[12],
    i32[0], i32[5],
    u32[0], u32[1],
    f32[0], f32[1], f32[4],
    f64[0], f64[1],
    u16[2.0], u16[1.5],
  ].join(",");

  // Resizable ArrayBuffer: fixed view out-of-bounds after shrink + tracking view.
  const rab = new ArrayBuffer(16, { maxByteLength: 32 });
  const fixedView = new Uint16Array(rab, 4, 4); // bytes 4..12
  const trackView = new Uint16Array(rab, 4);    // bytes 4..end
  fixedView[0] = 100; fixedView[1] = 200; fixedView[2] = 300; fixedView[3] = 400;
  const beforeResize = [fixedView[0], fixedView[3], trackView[5]].join("/");
  rab.resize(8); // fixedView now OOB, trackView has len 2
  const afterShrink = [fixedView[0], trackView[0], trackView[1], trackView[2]].join("/");
  rab.resize(2); // byteOffset 4 > byteLength 2
  const afterTiny = [fixedView[0], trackView[0]].join("/");
  rab.resize(16);
  const afterGrow = [fixedView[0], trackView[0], trackView[5]].join("/");

  // Detached ArrayBuffer.
  const dab = new ArrayBuffer(8);
  const du16 = new Uint16Array(dab);
  du16[0] = 777;
  const preDetach = du16[0];
  dab.transfer();
  const postDetach = du16[0];

  return `ta:${s16}|${s8}|${kinds}|${beforeResize}|${afterShrink}|${afterTiny}|${afterGrow}|${preDetach}|${postDetach}`;
}

// 2. global_real_own_route bitset invalidation (defineProperty, accessor, delete, eval).
var gPlain = 10;
gAcc = 20;
var gRo = 30;
function testGlobals() {
  let sum = 0;
  for (let i = 0; i < 120; i++) {
    gPlain = gPlain + 1;
    sum += gPlain + gRo;
  }
  let accGets = 0, accSets = 0, accVal = 100;
  Object.defineProperty(globalThis, "gAcc", {
    get() { accGets++; return accVal; },
    set(v) { accSets++; accVal = v + 1; },
    configurable: true,
  });
  Object.defineProperty(globalThis, "gRo", {
    value: 55,
    writable: false,
  });
  let accSum = 0;
  for (let i = 0; i < 60; i++) {
    gAcc = i;
    accSum += gAcc;
    gRo = 999; // sloppy no-op on non-writable real own prop
  }
  globalThis.extraProp1 = 1;
  globalThis.extraProp2 = 2;
  delete globalThis.extraProp1;
  (0, eval)("var evalDefinedG = 42;");
  Object.defineProperty(globalThis, "evalDefinedG", {
    get() { return 420; },
    configurable: true,
  });
  return `glob:${sum}:${gPlain}:${gRo}:${accGets}:${accSets}:${accSum}:${evalDefinedG}`;
}

// 3. Ctor-less derived classes (construct_class + run_class_ctor + Reflect.construct + setPrototypeOf).
function testCtorChains() {
  class Base {
    constructor(a, b) {
      this.a = a;
      this.b = b;
    }
  }
  class Mid1 extends Base {}
  class Mid2 extends Mid1 {
    constructor(a, b) {
      super(a * 2, b + 1);
      this.c = this.a + this.b;
    }
  }
  class Leaf1 extends Mid2 {}
  class Leaf2 extends Leaf1 {}

  class Empty0 {}
  class Empty1 extends Empty0 {}
  class Empty2 extends Empty1 {}

  class RetOvBase {
    constructor(tag) {
      return { tag, fromOv: true };
    }
  }
  class RetOvSub1 extends RetOvBase {}
  class RetOvSub2 extends RetOvSub1 {}

  let total = 0;
  let last = null;
  for (let i = 0; i < 80; i++) {
    const x = new Leaf2(i, 3);
    total += x.c;
    last = x;
  }
  const e = new Empty2();
  const ov = new RetOvSub2("ok");

  // Reflect.construct with custom newTarget must NOT take the new_target == cv shortcut.
  function CustomNT() {}
  CustomNT.prototype = { customMarker: 777 };
  const rc = Reflect.construct(Leaf2, [5, 2], CustomNT);

  // Object.setPrototypeOf on a ctor-less class must redirect super().
  class AltBase {
    constructor(x) { this.alt = x + 100; }
  }
  class ReprotoSub extends Base {}
  const beforeRe = new ReprotoSub(7, 8);
  Object.setPrototypeOf(ReprotoSub, AltBase);
  const afterRe = new ReprotoSub(7, 8);

  return [
    "ctor",
    total,
    last instanceof Leaf2,
    last instanceof Base,
    e instanceof Empty2,
    e instanceof Empty0,
    ov.tag + "/" + ov.fromOv,
    rc.a, rc.b, rc.c, rc.customMarker, Object.getPrototypeOf(rc) === CustomNT.prototype,
    beforeRe.a + "," + beforeRe.b,
    afterRe.alt + "," + afterRe.a,
  ].join(":");
}

// 4. `in` operator: plain shaped, dictionary, Object.prototype, null-proto, #private, Proxy.
function testInOp() {
  const shaped = { alpha: 1, beta: 2, "#lit": 3 };
  const dict = { a: 1, b: 2, c: 3, d: 4 };
  delete dict.b;
  const nullProto = Object.create(null);
  nullProto.k = 1;

  class WithPriv {
    #secret = 99;
    hasLit() { return "#secret" in this; }
    readSecret() { return this.#secret; }
  }
  const wp = new WithPriv();

  let proxyHits = 0;
  const pTarget = { x: 1 };
  const p = new Proxy(pTarget, {
    has(t, k) {
      proxyHits++;
      return k === "magic" || k in t;
    }
  });
  const childOfProxy = Object.create(p);
  childOfProxy.own = 1;

  let hits = 0;
  const keys = ["alpha", "missing", "toString", "#lit", "c", "b"];
  for (let i = 0; i < 100; i++) {
    const k = keys[i % keys.length];
    if (k in shaped) hits++;
    if (k in dict) hits++;
    if (k in nullProto) hits++;
  }
  return [
    "in",
    hits,
    "alpha" in shaped,
    "toString" in shaped,
    "toString" in nullProto,
    "#lit" in shaped,
    wp.hasLit(),
    wp.readSecret(),
    "magic" in p,
    "x" in p,
    "nope" in p,
    "magic" in childOfProxy,
    "own" in childOfProxy,
    proxyHits,
  ].join(":");
}

// 5. ASCII & Unicode toLowerCase / toUpperCase (unchanged, 1-byte, short cached, long >32B, non-ASCII).
function testCaseMap() {
  const shortTags = ["DIV", "SPAN", "div", "A", "z", "SECTION", "ARTICLE"];
  const longAscii = "The Quick Brown Fox Jumps Over The Lazy Dog 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ!";
  const longLowerAlready = longAscii.toLowerCase();
  const longUpperAlready = longAscii.toUpperCase();
  let out = [];
  for (let i = 0; i < 80; i++) {
    const t = shortTags[i % shortTags.length];
    const lo = t.toLowerCase();
    const up = t.toUpperCase();
    if (i === 79) out.push(lo, up);
  }
  let longChecks = 0;
  for (let i = 0; i < 60; i++) {
    if (longAscii.toLowerCase() === longLowerAlready) longChecks++;
    if (longLowerAlready.toLowerCase() === longLowerAlready) longChecks++;
    if (longAscii.toUpperCase() === longUpperAlready) longChecks++;
    if (longUpperAlready.toUpperCase() === longUpperAlready) longChecks++;
  }
  const uni = ["Straße".toUpperCase(), "ÉCOLE".toLowerCase(), "µ".toUpperCase()].join("/");
  return `case:${out.join(",")}:${longChecks}:${uni}:${longAscii.toLowerCase().slice(0, 15)}:${longAscii.toUpperCase().slice(0, 15)}`;
}

console.log(testTypedArrays());
console.log(testGlobals());
console.log(testCtorChains());
console.log(testInOp());
console.log(testCaseMap());
"##;

const EXPECTED: &[&str] = &[
    "ta:82459200|744600|-97,-94,,0,162,,-32768,-32677,50335,62157,,250201247,-221403005,2147483653,250201247,1.5,-2.25,,3.125,-0.5,22579,|100/400/0|/100/200/|/|0/0/0|777|undefined",
    "glob:12060:130:55:60:60:1830:420",
    "ctor:6640:true:true:true:true:ok/true:10:3:13:777:true:7,8:107,undefined",
    "in:84:true:true:false:true:false:99:true:true:false:true:true:4",
    "case:div,DIV:240:STRASSE/école/Μ:the quick brown:THE QUICK BROWN",
];

fn run_ok(source: &str) -> Vec<String> {
    zipp_vm::run(source).expect("program runs").output
}

#[test]
fn interp_ta_global_ctor_has_matches_node() {
    assert_eq!(run_ok(PROGRAM), EXPECTED);
}

#[test]
fn interp_ta_global_ctor_has_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
        ("nursery_verify", "ZIPP_NURSERY_VERIFY", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("interp_ta_global_ctor_has_matches_node")
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
