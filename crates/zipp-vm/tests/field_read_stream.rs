//! Correctness boundary for the guarded cyclic field read/write fast-forwarders.
//!
//! The optimizer may commit only after every repeated operation has proved to
//! be a side-effect-free integer data-property read.  Accessors, proxies, array
//! overlays, holes and non-integer values must run the unchanged loop.

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
function read(objs, n, k, i, sum) {
  for (; i < LIMIT; i++) {
    sum = (sum + objs[k].x) | 0;
    k++;
    if (k === n) k = 0;
  }
  return sum + ":" + k + ":" + i;
}
function own(n, dict) {
  var a = [];
  for (var i = 0; i < n; i++) {
    var o = { a: i, x: (i * 17 - 31) | 0, z: i + 1 };
    if (dict) { o.dead = 1; delete o.dead; }
    a.push(o);
  }
  return a;
}
var proto = { x: 9 }, inherited = [];
for (var q = 0; q < 13; q++) { var o = Object.create(proto); o.a = q; inherited.push(o); }
console.log(read(own(1, false), 1, 0, 7, 0x7ffffff0 | 0));
console.log(read(own(9, false), 9, 4, 3, -123456789));
console.log(read(own(16, true), 16, 15, 11, 987654321));
console.log(read(inherited, 13, 8, 5, -7));
"#;

#[test]
fn own_dictionary_and_inherited_integer_fields_match_the_loop() {
    // Expected values were captured from node v24.12.0 and are also exercised
    // under the unfused/interpreter modes below.
    assert_eq!(
        run_ok(MATRIX),
        [
            "2146863849:0:20000",
            "-122716883:3:20000",
            "989582979:4:20000",
            "179948:9:20000",
        ]
    );
}

const WRITE_MATRIX: &str = r#"
"use strict";
var LIMIT = 20000;
function write(objs, n, k, i) {
  for (; i < LIMIT; i++) {
    objs[k].x = i;
    k++;
    if (k === n) k = 0;
  }
  return k + ":" + i;
}
function own(n, dict) {
  var a = [];
  for (var q = 0; q < n; q++) {
    var o = { a: q, x: -q, z: q + 1 };
    if (dict) { o.dead = 1; delete o.dead; }
    a.push(o);
  }
  return a;
}
function dump(a) {
  var s = [];
  for (var q = 0; q < a.length; q++) s.push(a[q].x);
  return s.join(",");
}
var a = own(9, false);
console.log(write(a, 9, 4, 3) + ":" + dump(a));
var d = own(16, true);
console.log(write(d, 16, 15, 11) + ":" + dump(d));
var shared = { x: -1 }, aa = { x: -2 }, bb = { x: -3 };
var aliases = [shared, aa, shared, bb];
console.log(write(aliases, 4, 3, 17) + ":" + dump(aliases));
var untouched = own(3, false);
console.log(write(untouched, 3, 2, 20000) + ":" + dump(untouched));
"#;

const MASK_MATRIX: &str = r#"
"use strict";
var READS = 20003;
function mkAccessor(seed) {
  var o = { hidden: seed, pad: 0 };
  Object.defineProperty(o, "val", {
    get: function () { return this.hidden; },
    enumerable: true, configurable: true
  });
  return o;
}
var shapes = [
  { val: 11 },
  { a: 1, val: 22 },
  { a: 1, b: 2, val: 33 },
  { a: 1, b: 2, c: 3, val: 44 },
  { q: 0, val: 55 },
  { q: 0, r: 0, val: 66 },
  (function () { var o = Object.create({ val: 77 }); o.own = 1; return o; })(),
  mkAccessor(88)
];
var sum = -7, i = 3;
for (; i < READS; i++) sum = (sum + shapes[i & 7].val) | 0;
console.log(sum + ":" + i);
"#;

const GLOBAL_SUM_MATRIX: &str = r#"
"use strict";
var LIMIT = 20007;
var p0 = { d0: 1 };
var p1 = Object.create(p0); p1.d1 = 2;
var p2 = Object.create(p1); p2.d2 = 3;
var p3 = Object.create(p2); p3.d3 = 4;
var p4 = Object.create(p3); p4.d4 = 5;
var acc = { hidden: 8 };
Object.defineProperty(acc, "val", { get: function () { return this.hidden; } });
var sum = -10, i = 7;
for (; i < LIMIT; i++) sum = (sum + p4.d0 + p4.d2 + p4.d4 + acc.val) | 0;
console.log(sum + ":" + i);
"#;

const MIXED_MATRIX: &str = r#"
"use strict";
var LIMIT = 20003;
function mkAccessor(seed) {
  var o = { hidden: seed };
  Object.defineProperty(o, "val", {
    get: function () { return this.hidden; },
    set: function (x) { this.hidden = x | 0; },
    enumerable: true, configurable: true
  });
  return o;
}
var shared = { val: -2 };
var a = [{ val: -1 }, shared, shared, mkAccessor(-3)];
var sum = -9, i = 5;
for (; i < LIMIT; i++) {
  var o = a[i & 3];
  o.val = (i & 31) + 1;
  sum = (sum + o.val) | 0;
}
console.log(sum + ":" + i + ":" + a[0].val + ":" + shared.val + ":" + a[3].val + ":" + (o === a[2]));
"#;

#[test]
fn global_mask_read_accepts_plain_inherited_and_pure_passthrough_getters() {
    assert_eq!(run_ok(MASK_MATRIX), ["989993:20003"]);
}

#[test]
fn global_mask_read_rejects_observable_getters() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 5000, gets = 0;
        var a = [
          {x:1}, {x:2}, {x:3}, {x:4}, {x:5}, {x:6}, {x:7},
          { get x() { gets++; return gets; } }
        ];
        var sum = 0, i = 0;
        for (; i < LIMIT; i++) sum = (sum + a[i & 7].x) | 0;
        console.log(sum + ":" + i + ":" + gets);
        "#,
    );
    assert_eq!(out, ["213125:5000:625"]);
}

#[test]
fn global_invariant_field_sum_accepts_data_proto_and_pure_getters() {
    assert_eq!(run_ok(GLOBAL_SUM_MATRIX), ["339990:20007"]);
}

#[test]
fn global_invariant_field_sum_rejects_observable_getters() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 5000, gets = 0;
        var obj = { get x() { gets++; return gets; } };
        var sum = 0, i = 0;
        for (; i < LIMIT; i++) sum = (sum + obj.x) | 0;
        console.log(sum + ":" + i + ":" + gets);
        "#,
    );
    assert_eq!(out, ["12502500:5000:5000"]);
}

#[test]
fn global_mixed_field_stream_accepts_data_accessors_and_aliases() {
    assert_eq!(run_ok(MIXED_MATRIX), ["329982:20003:1:3:32:true"]);
}

#[test]
fn global_mixed_field_stream_rejects_observable_accessors() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 5000, sets = 0, gets = 0, hidden = 0;
        var observed = {};
        Object.defineProperty(observed, "val", {
          set: function (x) { sets++; hidden = x; },
          get: function () { gets++; return hidden + gets; }
        });
        var a = [{ val: 0 }, observed, { val: 0 }, { val: 0 }];
        var sum = 0, i = 0;
        for (; i < LIMIT; i++) {
          var o = a[i & 3];
          o.val = (i & 31) + 1;
          sum = (sum + o.val) | 0;
        }
        console.log(sum + ":" + i + ":" + sets + ":" + gets + ":" + hidden);
        "#,
    );
    assert_eq!(out, ["864279:5000:1250:1250:6"]);
}

#[test]
fn existing_own_writes_dictionary_slots_and_aliases_match_the_loop() {
    assert_eq!(
        run_ok(WRITE_MATRIX),
        [
            "3:20000:19997,19998,19999,19991,19992,19993,19994,19995,19996",
            "4:20000:19996,19997,19998,19999,19984,19985,19986,19987,19988,19989,19990,19991,19992,19993,19994,19995",
            "2:20000:19998,19999,19998,19997",
            "2:20000:0,-1,-2",
        ]
    );
}

#[test]
fn observable_and_non_own_writes_fail_closed() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 5000, sets = 0, traps = 0, indexGets = 0;
        function write(objs, n) {
          var k = 0;
          for (var i = 0; i < LIMIT; i++) {
            objs[k].x = i;
            k++;
            if (k === n) k = 0;
          }
          return k + ":" + i;
        }

        var accValue = -1;
        var acc = [{ set x(v) { sets++; accValue = v; }, get x() { return accValue; } }];
        console.log(write(acc, 1) + ":" + sets + ":" + accValue);

        var target = { x: -1 };
        var prox = [new Proxy(target, { set: function (o, p, v) { traps++; o[p] = v; return true; } })];
        console.log(write(prox, 1) + ":" + traps + ":" + target.x);

        var overTarget = { x: -1 }, over = [overTarget];
        Object.defineProperty(over, "0", { get: function () { indexGets++; return overTarget; } });
        console.log(write(over, 1) + ":" + indexGets + ":" + overTarget.x);

        var proto = { x: -1 }, inheritedObj = Object.create(proto), inherited = [inheritedObj];
        console.log(write(inherited, 1) + ":" + inheritedObj.x + ":" + proto.x);
        try { write([, target], 2); } catch (e) { console.log(e instanceof TypeError); }
        "#,
    );
    assert_eq!(
        out,
        [
            "0:5000:5000:4999",
            "0:5000:5000:4999",
            "0:5000:5000:4999",
            "0:5000:4999:-1",
            "true",
        ]
    );
}

#[test]
fn observable_and_noninteger_cases_fail_closed() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 5000, gets = 0, traps = 0, indexGets = 0;
        function read(objs, n) {
          var sum = 0, k = 0;
          for (var i = 0; i < LIMIT; i++) {
            sum = (sum + objs[k].x) | 0;
            k++;
            if (k === n) k = 0;
          }
          return sum;
        }

        var acc = [{ get x() { gets++; return gets & 7; } }];
        var target = { x: 3 };
        var prox = [new Proxy(target, { get: function (o, p) { traps++; return o[p]; } })];
        var over = [{ x: 5 }];
        Object.defineProperty(over, "0", { get: function () { indexGets++; return target; } });
        var doubles = [{ x: 0.5 }, { x: 1.25 }];
        var proto = {};
        Object.defineProperty(proto, "x", { get: function () { gets++; return 2; } });
        var inherited = [Object.create(proto)];

        console.log(read(acc, 1) + ":" + gets);
        console.log(read(prox, 1) + ":" + traps);
        console.log(read(over, 1) + ":" + indexGets);
        console.log(read(doubles, 2));
        console.log(read(inherited, 1) + ":" + gets);
        try { read([, target], 2); } catch (e) { console.log(e instanceof TypeError); }
        "#,
    );
    assert_eq!(
        out,
        [
            "17500:5000",
            "15000:5000",
            "15000:5000",
            "2500",
            "10000:10000",
            "true"
        ]
    );
}

#[test]
fn empty_remaining_range_keeps_state_exact() {
    let out = run_ok(
        r#"
        "use strict";
        var LIMIT = 1000;
        function read(a, n, k, i, s) {
          for (; i < LIMIT; i++) {
            s = (s + a[k].x) | 0;
            k++;
            if (k === n) k = 0;
          }
          return s + ":" + k + ":" + i;
        }
        var a = [{x:1}, {x:2}, {x:3}];
        for (var warm = 0; warm < 300; warm++) read(a, 3, 1, 900, 7);
        console.log(read(a, 3, 2, 1000, -9));
        "#,
    );
    assert_eq!(out, ["-9:2:1000"]);
}

#[test]
fn mapped_arguments_are_not_treated_as_plain_dense_arrays() {
    let out = run_ok(
        r#"
        var LIMIT = 20000;
        function readMapped(first) {
          var objs = arguments, n = 1, k = 0, i = 0, sum = 0;
          first = {x:7};
          for (; i < LIMIT; i++) {
            sum = (sum + objs[k].x) | 0;
            k++;
            if (k === n) k = 0;
          }
          return sum + ":" + k + ":" + i + ":" + first.x;
        }
        function writeMapped(first) {
          var objs = arguments, n = 1, k = 0, i = 0;
          first = {x:-1};
          for (; i < LIMIT; i++) {
            objs[k].x = i;
            k++;
            if (k === n) k = 0;
          }
          return first.x + ":" + k + ":" + i;
        }
        var old = {x:1};
        console.log(readMapped(old) + ":" + old.x);
        var old2 = {x:2};
        console.log(writeMapped(old2) + ":" + old2.x);
        "#,
    );
    assert_eq!(out, ["140000:0:20000:7:1", "19999:0:20000:2"]);
}

/// Switches are process-latched at region compilation. Re-run the complete
/// matrix in fresh processes so the default, old region, threshold-1, GC-stress
/// and interpreter routes all share one oracle.
#[test]
fn zz_modes_agree() {
    if std::env::var_os("ZIPP_FIELD_STREAM_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let modes: &[(&str, &[(&str, &str)])] = &[
        (
            "off",
            &[
                ("ZIPP_NO_FIELD_READ_STREAM", "1"),
                ("ZIPP_NO_FIELD_WRITE_STREAM", "1"),
                ("ZIPP_NO_FIELD_SUM_STREAM", "1"),
                ("ZIPP_NO_FIELD_MIXED_STREAM", "1"),
            ],
        ),
        ("threshold1", &[("ZIPP_JIT_THRESHOLD", "1")]),
        ("gc", &[("ZIPP_GC_STRESS", "1")]),
        ("interpreter", &[("ZIPP_NOJIT", "1")]),
    ];
    for (name, envs) in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--skip", "zz_modes_agree"])
            .env("ZIPP_FIELD_STREAM_CHILD", "1")
            .env_remove("ZIPP_NO_FIELD_READ_STREAM")
            .env_remove("ZIPP_NO_FIELD_WRITE_STREAM")
            .env_remove("ZIPP_NO_FIELD_SUM_STREAM")
            .env_remove("ZIPP_NO_FIELD_MIXED_STREAM")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NOJIT");
        for &(k, v) in *envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && !stdout.contains(" 0 passed"),
            "mode {name} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn mechanism_is_not_vacuous() {
    // Mode-matrix children intentionally disable one or both native prefixes.
    // The top-level default process below is the independent non-vacuity proof.
    if std::env::var_os("ZIPP_FIELD_STREAM_CHILD").is_some() {
        return;
    }
    if std::env::var_os("ZIPP_FIELD_STREAM_LOG_CHILD").is_some() {
        let _ = run_ok(MATRIX);
        let _ = run_ok(WRITE_MATRIX);
        let _ = run_ok(MASK_MATRIX);
        let _ = run_ok(GLOBAL_SUM_MATRIX);
        let _ = run_ok(MIXED_MATRIX);
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--exact", "mechanism_is_not_vacuous", "--nocapture"])
        .env("ZIPP_FIELD_STREAM_LOG_CHILD", "1")
        .env("ZIPP_JITLOG", "1")
        .output()
        .expect("mechanism child");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "mechanism child failed: {stderr}");
    assert!(
        stderr.contains("field-read-stream prefix"),
        "prefix did not compile:\n{stderr}"
    );
    assert!(
        stderr.contains("field-write-stream prefix"),
        "write prefix did not compile:\n{stderr}"
    );
    assert!(
        stderr.contains("field-mask-read-stream committed"),
        "mask prefix did not commit:\n{stderr}"
    );
    assert!(
        stderr.contains("global-field-sum-stream committed"),
        "global sum prefix did not commit:\n{stderr}"
    );
    assert!(
        stderr.contains("mixed-field-stream committed"),
        "mixed field prefix did not commit:\n{stderr}"
    );
}
