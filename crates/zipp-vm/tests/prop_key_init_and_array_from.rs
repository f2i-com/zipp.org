//! Regression tests for Commit 3A: inline flat-string `ToPropKey`/`ToConcatKey`,
//! shaped `InitDataPropDyn` (`{ [k]: v }`), direct intrinsic iterator stepping
//! in `Array.from`, and `WeakMap` tombstone purging.
//!
//! Expected output lines are Node 24's.

const PROGRAM: &str = r#"
"use strict";

function expectThrow(fn) {
  try {
    fn();
    return "no-throw";
  } catch (e) {
    return e.name;
  }
}

// 1. ToPropKey in Tier C and loop regions (flat strings, empty string, Symbol,
//    numbers, bool, objects, nullish obj).
function testToPropKey() {
  const sym = Symbol("s");
  const o = { a: 1, "": 2, "0": 3, "true": 4, "null": 5, "undefined": 6, [sym]: 7, custom: 8 };
  const ks = ["a", "", 0, true, null, undefined, sym];
  let sum = 0;
  for (let i = 0; i < 60; i++) {
    const k = ks[i % ks.length];
    o[k] += 1;
    sum += o[k];
  }
  const keyObj = {
    toString() {
      return "custom";
    }
  };
  for (let i = 0; i < 40; i++) {
    o[keyObj] += 2;
  }
  const symKeyObj = {
    [Symbol.toPrimitive]() {
      return sym;
    }
  };
  for (let i = 0; i < 20; i++) {
    o[symKeyObj] += 1;
  }
  let coerceCount = 0;
  const onceKey = { toString() { coerceCount++; return "custom"; } };
  let readSum = 0;
  for (let i = 0; i < 15; i++) {
    readSum += o[onceKey];
  }
  // Nullish base must throw BEFORE coercing key.
  let side = 0;
  const badKey = { toString() { side++; return "x"; } };
  const e1 = expectThrow(() => { const n = null; n[badKey] += 1; });
  const e2 = expectThrow(() => { const u = undefined; u[badKey] += 1; });
  return `tpk:${sum}:${o.a}:${o[""]}:${o["0"]}:${o[sym]}:${o.custom}:${coerceCount}:${readSum}:${e1}:${e2}:${side}`;
}

// 2. ToConcatKey with flat strings, primitives, objects, Symbols.
function testToConcatKey() {
  const base = {};
  const ks = ["alpha", "beta", 42, true, null, undefined];
  for (let i = 0; i < 60; i++) {
    const k = ks[i % ks.length];
    base["p_" + k] = (base["p_" + k] || 0) + 1;
  }
  const objK = { toString() { return "obj"; } };
  for (let i = 0; i < 20; i++) {
    base["p_" + objK] = (base["p_" + objK] || 0) + 1;
  }
  const sym = Symbol("bad");
  const symErr = expectThrow(() => { base["p_" + sym] = 1; });
  return `tck:${base.p_alpha}:${base.p_42}:${base.p_true}:${base.p_null}:${base.p_undefined}:${base.p_obj}:${symErr}`;
}

// 3. InitDataPropDyn ({ [k]: v }) with strings, Symbols, @@-prefix, __proto__,
//    duplicates, accessor overwrite, and element keys.
function testInitDataPropDyn() {
  const sym = Symbol("k");
  const out = [];
  for (let i = 0; i < 60; i++) {
    const k1 = "foo";
    const k2 = "bar";
    const k3 = "@@escaped";
    const k4 = "__proto__";
    const k5 = 0;
    const obj = {
      a: i,
      [k1]: i + 1,
      [k2]: i + 2,
      [sym]: i + 3,
      [k3]: i + 4,
      [k4]: { custom: i },
      [k5]: "zero",
      [k1]: i + 10,
    };
    if (i === 59) {
      out.push(
        obj.a,
        obj.foo,
        obj.bar,
        obj[sym],
        obj["@@escaped"],
        Object.getPrototypeOf(obj) === Object.prototype,
        Object.prototype.hasOwnProperty.call(obj, "__proto__"),
        obj.__proto__.custom,
        obj[0],
        Object.keys(obj).join(",")
      );
    }
  }
  const k = "x";
  const over = { get x() { return 999; }, [k]: 42 };
  const desc = Object.getOwnPropertyDescriptor(over, "x");
  out.push(over.x, typeof desc.get, desc.value, desc.writable, desc.enumerable, desc.configurable);
  return "idp:" + out.join("|");
}

// 4. Array.from on intrinsic iterators (Set, Map, Array.values, matchAll)
//    with/without mapfn, IteratorClose on throw, and custom constructor.
function testArrayFrom() {
  const set = new Set([10, 20, 30, 40]);
  const map = new Map([["a", 1], ["b", 2], ["c", 3]]);
  const arr = [5, 6, 7];
  const res = [];
  for (let i = 0; i < 40; i++) {
    const a1 = Array.from(set);
    const a2 = Array.from(map.keys());
    const a3 = Array.from(map.values(), function (v, idx) { return v * this.mul + idx; }, { mul: 10 });
    const a4 = Array.from(map.entries(), ([k, v]) => `${k}=${v}`);
    const a5 = Array.from(arr.values(), (x) => x * 2);
    const a6 = Array.from("x1y22z333".matchAll(/\d+/g), (m) => m[0]);
    if (i === 39) {
      res.push(a1.join(","), a2.join(","), a3.join(","), a4.join(","), a5.join(","), a6.join(","));
    }
  }
  let closed = 0;
  const customIter = {
    [Symbol.iterator]() {
      let n = 0;
      return {
        next() { return n < 3 ? { value: ++n, done: false } : { value: undefined, done: true }; },
        return() { closed++; return { value: undefined, done: true }; }
      };
    }
  };
  const closeErr = expectThrow(() => Array.from(customIter, (v) => { if (v === 2) throw new RangeError("stop"); return v; }));
  res.push(closeErr, closed);
  class SubArr extends Array {}
  const sub = SubArr.from(new Set([1, 2, 3]), (x) => x + 1);
  res.push(sub instanceof SubArr, sub.join(","));
  return "af:" + res.join("|");
}

// 5. WeakMap churn & tombstone purging.
function testWeakMapChurn() {
  const wm = new WeakMap();
  const pinned = [];
  for (let i = 0; i < 50; i++) {
    const k = { id: i };
    wm.set(k, i * 3);
    pinned.push(k);
  }
  for (let round = 0; round < 30; round++) {
    for (let j = 0; j < 200; j++) {
      const tmp = { r: round, j };
      wm.set(tmp, j);
    }
  }
  let ok = 0;
  for (let i = 0; i < pinned.length; i++) {
    if (wm.get(pinned[i]) === i * 3) ok++;
  }
  for (let i = 0; i < 20; i++) wm.delete(pinned[i]);
  for (let i = 0; i < 20; i++) wm.set(pinned[i], i * 5);
  let sum = 0;
  for (let i = 0; i < pinned.length; i++) sum += wm.get(pinned[i]);
  return `wm:${ok}:${sum}`;
}

console.log(testToPropKey());
console.log(testToConcatKey());
console.log(testInitDataPropDyn());
console.log(testArrayFrom());
console.log(testWeakMapChurn());
"#;

fn run_ok(source: &str) -> Vec<String> {
    zipp_vm::run(source).expect("program runs").output
}

#[test]
fn prop_key_init_and_array_from_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "tpk:522:10:11:12:35:88:15:1320:TypeError:TypeError:0",
            "tck:10:10:10:10:10:20:TypeError",
            "idp:59|69|61|62|63|true|true|59|zero|0,a,foo,bar,@@escaped,__proto__|42|undefined|42|true|true|true",
            "af:10,20,30,40|a,b,c|10,21,32|a=1,b=2,c=3|10,12,14|1,22,333|RangeError|1|true|2,3,4",
            "wm:50:4055",
        ]
    );
}

#[test]
fn prop_key_init_and_array_from_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
        ("nursery_verify", "ZIPP_NURSERY_VERIFY", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("prop_key_init_and_array_from_answers_as_node")
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
