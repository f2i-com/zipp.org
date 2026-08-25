//! Correctness boundary for invariant for-in/Object.keys loop reductions.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const MATRIX: &str = r#"
"use strict";
var LIMIT = 20000;
var src = { "10": 5, a: 1, b: -2, c: 4 };
var forSum = 7, i = 0, kk = "before";
for (; i < LIMIT; i++) {
  for (kk in src) forSum = (forSum + src[kk]) | 0;
}
var keysSum = -9, j = 0;
for (; j < LIMIT; j++) keysSum = (keysSum + Object.keys(src).length) | 0;
console.log(forSum + ":" + keysSum + ":" + i + ":" + j + ":" + kk);
"#;

const COUNT_MATRIX: &str = r#"
"use strict";
var LIMIT = 2000;
var src = []; src.length = 500000;
for (var p = 0; p < 500000; p += 500) src[p] = p;
var count = 7, i = 0, key = "before";
for (; i < LIMIT; i++) for (key in src) count++;
console.log(count + ":" + i + ":" + key);
"#;

const IN_MATRIX: &str = r#"
"use strict";
var LIMIT = 4000, probe = [];
for (var p = 0; p < 16; p++) probe[p * 8] = p;
var present = 0, hole = 0, oob = 0, q = 0;
for (; q < LIMIT; q++) {
  if ((q % 16) * 8 in probe) present++;
  if ((q % 16) * 8 + 1 in probe) hole++;
  if (1000 + (q % 16) in probe) oob++;
}
console.log(present + ":" + hole + ":" + oob + ":" + q);
"#;

const ARRAY_COPY_MATRIX: &str = r#"
"use strict";
var LIMIT = 1024, source = [];
for (var p = 0; p < 64; p++) if (p % 3 !== 0) source[p * 2] = p;
var sliceSum = 7, si = 0;
for (; si < LIMIT; si++) sliceSum = (sliceSum + source.slice(0, 64).length) | 0;
var concatSum = -9, ci = 0;
for (; ci < LIMIT; ci++) concatSum = (concatSum + source.concat([1, 2, 3]).length) | 0;
console.log(sliceSum + ":" + si + ":" + concatSum + ":" + ci);
"#;

const SPARSE_FOLD_MATRIX: &str = r#"
"use strict";
var source = []; source.length = 50000000;
for (var p = 0; p < 1000; p++) source[p * 50000] = (p % 97) - 40;
var count = 3, fold = 11, key = "before";
for (key in source) {
  count++;
  fold = (fold + (+key) + source[key]) % 1000000007;
}
console.log(count + ":" + fold + ":" + key);
"#;

#[test]
fn plain_data_enumeration_matches_the_full_loops() {
    assert_eq!(run_ok(MATRIX), ["160007:79991:20000:20000:c"]);
}

#[test]
fn sparse_array_count_reduction_preserves_count_order_and_last_key() {
    assert_eq!(run_ok(COUNT_MATRIX), ["2000007:2000:499500"]);

    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 2000;
        Array.prototype.ap = 1; Object.prototype.op = 2;
        var a = []; a[8] = 1; a[2] = 1; a.z = 1; a.op = 3;
        var count = -5, i = 0, k = "before";
        for (; i < LIMIT; i++) for (k in a) count++;
        delete Array.prototype.ap; delete Object.prototype.op;
        console.log(count + ":" + i + ":" + k);

        var empty = [], ec = 3, j = 0, ek = "untouched";
        for (; j < LIMIT; j++) for (ek in empty) ec++;
        console.log(ec + ":" + j + ":" + ek);
        "#,
    );
    // Own order: 2,8,z,op; inherited ap; own op shadows Object.prototype.op.
    assert_eq!(out, ["9995:2000:ap", "3:2000:untouched"]);
}

#[test]
fn periodic_in_probe_reduction_matches_array_and_prototype_presence() {
    assert_eq!(run_ok(IN_MATRIX), ["4000:0:0:4000"]);
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 4000, gets = 0, probe = [];
        for (var p = 0; p < 16; p++) probe[p * 8] = p;
        Object.defineProperty(probe, "1", { enumerable: true, configurable: true,
          get: function () { gets++; return 1; } });
        Object.defineProperty(Array.prototype, "1000", { configurable: true,
          get: function () { gets++; return 2; } });
        var present = 0, ownAccessor = 0, inherited = 0, q = 0;
        for (; q < LIMIT; q++) {
          if ((q % 16) * 8 in probe) present++;
          if ((q % 16) * 8 + 1 in probe) ownAccessor++;
          if (1000 + (q % 16) in probe) inherited++;
        }
        delete Array.prototype[1000];
        console.log(present + ":" + ownAccessor + ":" + inherited + ":" + q + ":" + gets);
        "#,
    );
    // `in` observes accessor PRESENCE without invoking either getter.
    assert_eq!(out, ["4000:250:250:4000:0"]);
}

#[test]
fn periodic_in_probe_reduction_declines_observable_chains_and_promotions() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 1024, traps = 0, a = [];
        Object.setPrototypeOf(a, new Proxy({}, { has: function (t, k) { traps++; return k === "2"; } }));
        var pc = 0, pi = 0;
        for (; pi < LIMIT; pi++) if ((pi % 4) in a) pc++;
        console.log(pc + ":" + pi + ":" + traps);

        var coerces = 0, weird = { valueOf: function () { coerces++; return 5; } };
        var dense = [1,2,3,4], wi = 0;
        for (; wi < LIMIT; wi++) if ((wi % 4) in dense) weird++;
        console.log(weird + ":" + wi + ":" + coerces);

        var huge = 2147483642, hi = 0;
        for (; hi < LIMIT; hi++) if ((hi % 4) in dense) huge++;
        console.log(huge + ":" + hi);
        "#,
    );
    assert_eq!(out, ["256:1024:1024", "1029:1024:1", "2147484666:1024"]);
}

#[test]
fn array_copy_length_reduction_matches_slice_and_concat() {
    // source.length is 125, so the two invariant result lengths are 64 and 128.
    assert_eq!(run_ok(ARRAY_COPY_MATRIX), ["65543:1024:131063:1024"]);
}

#[test]
fn array_copy_length_reduction_declines_observable_protocols() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 512;

        var ctorGets = 0, a = [1,,3];
        Object.defineProperty(a, "constructor", { configurable: true,
          get: function () { ctorGets++; return Array; } });
        var as = 0, ai = 0;
        for (; ai < LIMIT; ai++) as = (as + a.slice(0, 3).length) | 0;
        console.log(as + ":" + ai + ":" + ctorGets);

        var indexGets = 0, b = [1,,3];
        Object.defineProperty(Array.prototype, "1", { configurable: true,
          get: function () { indexGets++; return 9; } });
        var bs = 0, bi = 0;
        for (; bi < LIMIT; bi++) bs = (bs + b.slice(0, 3).length) | 0;
        delete Array.prototype[1];
        console.log(bs + ":" + bi + ":" + indexGets);

        var spreadGets = 0, c = [1,,3];
        Object.defineProperty(c, Symbol.isConcatSpreadable, { configurable: true,
          get: function () { spreadGets++; return true; } });
        var cs = 0, ci = 0;
        for (; ci < LIMIT; ci++) cs = (cs + c.concat([4,5]).length) | 0;
        console.log(cs + ":" + ci + ":" + spreadGets);

        var calls = 0, oldSlice = Array.prototype.slice;
        Array.prototype.slice = function () { calls++; return { length: 5 }; };
        var d = [1,2,3], ds = 0, di = 0;
        for (; di < LIMIT; di++) ds = (ds + d.slice(0, 3).length) | 0;
        Array.prototype.slice = oldSlice;
        console.log(ds + ":" + di + ":" + calls);
        "#,
    );
    assert_eq!(
        out,
        [
            "1536:512:512",
            "1536:512:512",
            "2560:512:512",
            "2560:512:512"
        ]
    );
}

#[test]
fn sparse_forin_fold_reduction_preserves_numeric_key_order_and_values() {
    assert_eq!(run_ok(SPARSE_FOLD_MATRIX), ["1003:975006838:49950000"]);
}

#[test]
fn sparse_forin_fold_reduction_declines_accessors_and_inherited_keys() {
    let out = run_ok(
        r#"
        "use strict";
        var a = []; a.length = 50000000;
        for (var p = 0; p < 1000; p++) a[p * 50000] = p;
        var gets = 0;
        Object.defineProperty(a, "49975001", { enumerable: true, configurable: true,
          get: function () { gets++; return 7; } });
        var count = 0, fold = 0, key = "";
        for (key in a) {
          count++;
          fold = (fold + (+key) + a[key]) % 1000000007;
        }
        console.log(count + ":" + fold + ":" + key + ":" + gets);

        var b = []; b.length = 50000000;
        for (var q = 0; q < 1000; q++) b[q * 50000] = q;
        Array.prototype.inherited = 5;
        var count2 = 0, fold2 = 0, key2 = "";
        for (key2 in b) {
          count2++;
          fold2 = (fold2 + (+key2) + b[key2]) % 1000000007;
        }
        delete Array.prototype.inherited;
        console.log(count2 + ":" + fold2 + ":" + key2);

        var match = /(.)/.exec("x");
        match[0] = 1; match[1] = 2; match.length = 50000000;
        for (var r = 0; r < 512; r++) match[50000 * (r + 1)] = r;
        var count3 = 0, fold3 = 0, key3 = "";
        for (key3 in match) {
          count3++;
          fold3 = (fold3 + (+key3) + match[key3]) % 1000000007;
        }
        console.log(count3 + ":" + fold3 + ":" + key3);
        "#,
    );
    assert_eq!(
        out,
        [
            "1001:25474333:49975001:1",
            "1001:NaN:inherited",
            "517:NaN:groups"
        ]
    );
}

#[test]
fn count_reduction_fails_closed_for_observable_or_non_int_cases() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 1024;
        var own = 0, desc = 0;
        var prox = new Proxy({a:1,b:2}, {
          ownKeys: function (o) { own++; return Reflect.ownKeys(o); },
          getOwnPropertyDescriptor: function (o, k) { desc++; return Object.getOwnPropertyDescriptor(o, k); }
        });
        var pc = 0, pi = 0, pk = "";
        for (; pi < LIMIT; pi++) for (pk in prox) pc++;
        console.log(pc + ":" + pi + ":" + pk + ":" + own + ":" + desc);

        var a = [1,2,3], dc = 0, di = 0, dk = "";
        for (; di < LIMIT; di++) for (dk in a) { dc++; if (dk === "0") delete a[2]; }
        console.log(dc + ":" + di + ":" + dk);

        var huge = 2147483600, hi = 0, hk = "";
        var many = []; for (var q = 0; q < 64; q++) many[q] = q;
        for (; hi < LIMIT; hi++) for (hk in many) huge++;
        console.log(huge + ":" + hi + ":" + hk);
        "#,
    );
    assert_eq!(
        out,
        ["2048:1024:b:2048:2048", "2048:1024:1", "2147549136:1024:63"]
    );
}

#[test]
fn getters_and_custom_prototypes_fail_closed() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 2000, gets = 0;
        var src = { a: 1, get b() { gets++; return gets; } };
        var sum = 0, i = 0, k = "";
        for (; i < LIMIT; i++) for (k in src) sum = (sum + src[k]) | 0;
        console.log(sum + ":" + gets + ":" + k + ":" + i);

        var proto = { p: 3 }, child = Object.create(proto); child.a = 2;
        var inherited = 0, j = 0, q = "";
        for (; j < LIMIT; j++) for (q in child) inherited = (inherited + child[q]) | 0;
        console.log(inherited + ":" + q + ":" + j);

        var keyGets = 0, withAccessor = { a: 1 };
        Object.defineProperty(withAccessor, "x", {
          enumerable: true, get: function () { keyGets++; return 9; }
        });
        var count = 0, z = 0;
        for (; z < LIMIT; z++) count = (count + Object.keys(withAccessor).length) | 0;
        console.log(count + ":" + keyGets + ":" + z);
        "#,
    );
    assert_eq!(out, ["2003000:2000:b:2000", "10000:p:2000", "4000:0:2000"]);
}

#[test]
fn proxy_enumeration_stays_on_the_observable_path() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 2000, own = 0, desc = 0, reads = 0;
        var target = { a: 2, b: 3 };
        var prox = new Proxy(target, {
          ownKeys: function (o) { own++; return Reflect.ownKeys(o); },
          getOwnPropertyDescriptor: function (o, k) { desc++; return Object.getOwnPropertyDescriptor(o, k); },
          get: function (o, k) { reads++; return o[k]; }
        });
        var sum = 0, i = 0, k = "";
        for (; i < LIMIT; i++) for (k in prox) sum = (sum + prox[k]) | 0;
        console.log(sum + ":" + own + ":" + desc + ":" + reads + ":" + k);
        var count = 0, j = 0;
        for (; j < LIMIT; j++) count = (count + Object.keys(prox).length) | 0;
        console.log(count + ":" + own + ":" + desc + ":" + reads + ":" + j);
        "#,
    );
    assert_eq!(out, ["10000:4000:4000:4000:b", "4000:6000:8000:4000:2000"]);
}

#[test]
fn zz_off_switch_and_gc_stress_agree() {
    if std::env::var_os("ZIPP_ENUM_REDUCE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (name, envs) in [
        ("off", vec![("ZIPP_NO_ENUM_LOOP_REDUCE", "1")]),
        ("count-off", vec![("ZIPP_NO_ENUM_COUNT_REDUCE", "1")]),
        ("in-off", vec![("ZIPP_NO_IN_PROBE_REDUCE", "1")]),
        (
            "array-copy-off",
            vec![("ZIPP_NO_ARRAY_COPY_LEN_REDUCE", "1")],
        ),
        ("sparse-fold-off", vec![("ZIPP_NO_SPARSE_FORIN_FOLD", "1")]),
        ("gc", vec![("ZIPP_GC_STRESS", "1")]),
        ("nojit", vec![("ZIPP_NOJIT", "1")]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--skip", "zz_off_switch_and_gc_stress_agree"])
            .env("ZIPP_ENUM_REDUCE_CHILD", "1")
            .env_remove("ZIPP_NO_ENUM_LOOP_REDUCE")
            .env_remove("ZIPP_NO_ENUM_COUNT_REDUCE")
            .env_remove("ZIPP_NO_IN_PROBE_REDUCE")
            .env_remove("ZIPP_NO_ARRAY_COPY_LEN_REDUCE")
            .env_remove("ZIPP_NO_SPARSE_FORIN_FOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NOJIT");
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "mode {name} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn mechanism_is_not_vacuous() {
    if std::env::var_os("ZIPP_ENUM_REDUCE_CHILD").is_some() {
        return;
    }
    if std::env::var_os("ZIPP_ENUM_REDUCE_LOG_CHILD").is_some() {
        let _ = run_ok(MATRIX);
        let _ = run_ok(COUNT_MATRIX);
        let _ = run_ok(IN_MATRIX);
        let _ = run_ok(ARRAY_COPY_MATRIX);
        let _ = run_ok(SPARSE_FOLD_MATRIX);
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--exact", "mechanism_is_not_vacuous", "--nocapture"])
        .env("ZIPP_ENUM_REDUCE_LOG_CHILD", "1")
        .env("ZIPP_JITLOG", "1")
        .output()
        .expect("mechanism child");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "mechanism child failed: {stderr}");
    assert!(
        stderr.contains("enum-forin-sum committed"),
        "for-in reducer did not commit:\n{stderr}"
    );
    assert!(
        stderr.contains("enum-object-keys committed"),
        "Object.keys reducer did not commit:\n{stderr}"
    );
    assert!(
        stderr.contains("enum-forin-count committed"),
        "for-in count reducer did not commit:\n{stderr}"
    );
    assert!(
        stderr.contains("periodic-in-probes committed"),
        "periodic in-probe reducer did not commit:\n{stderr}"
    );
    assert!(
        stderr.contains("array-copy-length committed slice")
            && stderr.contains("array-copy-length committed concat"),
        "array copy-length reducer did not commit both methods:\n{stderr}"
    );
    assert!(
        stderr.contains("sparse-forin-fold committed"),
        "sparse for-in fold reducer did not commit:\n{stderr}"
    );
}
