//! W19 M3 — `DeleteIndexConcat` in a MEM region, plus M1 (the split
//! `PropIndex`) and M2 (the ordinary-object `delete_prop` fast path).
//!
//! Before W19 `region_mem.rs` had no `Delete` arm of any kind, so a single
//! `delete obj["k" + i]` DECLINED and permanently blacklisted its whole region
//! (`polymorphic-objects` `[145,155]`, a 900k-delete loop that therefore ran
//! 100% interpreted). The emitted site now calls `jit_delete_index_concat`,
//! a wrapper over the SAME `Vm::delete_index_concat` the interpreter arm calls.
//!
//! What can go wrong is not "it declines" but "it answers", and answers wrong:
//!
//!  * a delete SHIFTS every later slot and bumps the receiver's version, so an
//!    inline cache that recorded a slot for another key of the same object is
//!    stale — a missing refetch is a HIT ON THE WRONG SLOT, silent;
//!  * strict-mode `delete` THROWS after a failed delete, and a Proxy
//!    `deleteProperty` trap can throw after arbitrary side effects, so the
//!    region must unwind on `CALL_THREW` and never re-execute the op;
//!  * a trap re-enters the dispatch loop and can allocate, so the region's
//!    pinned version-array / IC-table pointers can move under it.
//!
//! Every loop here runs far past `OSR_THRESHOLD` (8), so the region is compiled
//! long before the interesting iteration. `zz_*` re-runs the whole battery in
//! child processes with each W19 latch OFF and with `ZIPP_NOJIT=1`: the
//! mechanisms are perf work, so all five configurations must print the same
//! bytes.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The row's own shape: build 60 computed keys, delete half, re-add them, read
/// everything back. If the JIT'd delete leaves the index disagreeing with the
/// key vector by one slot, the READ-BACK is wrong while nothing crashes — so
/// the assertion is on the values, not on the count.
const CHURN: &str = r#"
    "use strict";
    var bad = 0, sum = 0, keys = 0;
    for (var d = 0; d < 400; d++) {
      var o = {};
      for (var p = 0; p < 60; p++) o["prop_" + p] = p + (d & 7);
      for (var p = 0; p < 60; p += 2) delete o["prop_" + p];
      for (var p = 1; p < 60; p += 2) {
        if (o["prop_" + p] !== p + (d & 7)) bad++;
      }
      for (var p = 0; p < 60; p += 2) {
        if (o["prop_" + p] !== undefined) bad++;
        o["prop_" + p] = p * 2 + (d & 3);
      }
      for (var p = 0; p < 60; p++) {
        var want = (p % 2 === 0) ? p * 2 + (d & 3) : p + (d & 7);
        if (o["prop_" + p] !== want) bad++;
        sum = (sum + o["prop_" + p]) | 0;
      }
      for (var k in o) keys++;
    }
    console.log(bad + "," + sum + "," + keys);
"#;

#[test]
fn hot_delete_and_rebuild_reads_back_every_slot() {
    assert_eq!(run_ok(CHURN), ["0,1116000,24000"]); // node v24.12.0
}

/// Strict-mode `delete` of a NON-CONFIGURABLE property throws — and does so
/// from inside a region that has been compiled and running for hundreds of
/// iterations. The catch must see a TypeError, the loop must resume, and the
/// object must be exactly as the successful deletes left it (no double
/// execution of the throwing op, no lost deletion before it).
const STRICT_THROW: &str = r#"
    "use strict";
    var o = {};
    for (var p = 0; p < 40; p++) o["k" + p] = p;
    Object.defineProperty(o, "k17", { value: 1717, configurable: false, enumerable: true, writable: true });
    var threw = 0, ok = 0, other = 0;
    for (var i = 0; i < 4000; i++) {
      var p = i % 40;
      try {
        delete o["k" + p];
        ok++;
      } catch (e) {
        if (e instanceof TypeError) threw++; else other++;
      }
    }
    console.log(threw + "," + ok + "," + other + "," + o.k17 + "," + Object.keys(o).join("|"));
"#;

#[test]
fn strict_delete_of_non_configurable_throws_from_a_compiled_region() {
    assert_eq!(run_ok(STRICT_THROW), ["100,3900,0,1717,k17"]);
}

/// The sloppy-mode twin: the same failed deletes answer `false` instead of
/// throwing, so the region keeps running and the boolean must be right every
/// time. `strict` is carried per-instruction, so this is a distinct emission.
const SLOPPY_FALSE: &str = r#"
    var o = {};
    for (var p = 0; p < 40; p++) o["k" + p] = p;
    Object.defineProperty(o, "k17", { value: 1717, configurable: false, enumerable: true, writable: true });
    var t = 0, f = 0;
    for (var i = 0; i < 4000; i++) {
      if (delete o["k" + (i % 40)]) t++; else f++;
    }
    console.log(t + "," + f + "," + o.k17 + "," + Object.keys(o).join("|"));
"#;

#[test]
fn sloppy_delete_of_non_configurable_answers_false_in_a_loop() {
    assert_eq!(run_ok(SLOPPY_FALSE), ["3900,100,1717,k17"]);
}

/// A Proxy `deleteProperty` trap in a hot loop: it runs USER CODE (which
/// allocates, so the region's pinned pointers can move), it can refuse, and it
/// can THROW. All three arms are exercised after the region is compiled.
const PROXY_TRAP: &str = r#"
    "use strict";
    var target = {};
    for (var p = 0; p < 30; p++) target["k" + p] = p;
    var seen = 0, junk = null;
    var p2 = new Proxy(target, {
      deleteProperty: function (t, k) {
        seen++;
        junk = { a: k, b: seen, c: [1, 2, 3] };   // allocate inside the trap
        if (k === "k7") return false;             // refuse -> strict TypeError
        if (k === "k9") throw new RangeError("nope");
        return Reflect.deleteProperty(t, k);
      }
    });
    var okc = 0, typeErr = 0, rangeErr = 0;
    for (var i = 0; i < 3000; i++) {
      try {
        delete p2["k" + (i % 30)];
        okc++;
      } catch (e) {
        if (e instanceof RangeError) rangeErr++;
        else if (e instanceof TypeError) typeErr++;
        else throw e;
      }
    }
    console.log(seen + "," + okc + "," + typeErr + "," + rangeErr + "," +
      Object.keys(target).join("|") + "," + junk.a);
"#;

#[test]
fn proxy_delete_trap_refuses_throws_and_allocates_inside_a_region() {
    assert_eq!(run_ok(PROXY_TRAP), ["3000,2800,100,100,k7|k9,k29"]);
}

/// A trap that MUTATES the receiver's shape while the region holds cached
/// state for it: the ICs recorded against `target` are all stale after each
/// delete, and the trap adds a fresh key on top. Any missing refetch or stale
/// slot shows up as a wrong read-back.
const TRAP_MUTATES: &str = r#"
    "use strict";
    var target = {};
    for (var p = 0; p < 20; p++) target["k" + p] = p * 10;
    var p2 = new Proxy(target, {
      deleteProperty: function (t, k) {
        t["extra_" + k] = k;
        return Reflect.deleteProperty(t, k);
      }
    });
    var bad = 0;
    for (var i = 0; i < 2000; i++) {
      var n = i % 20;
      delete p2["k" + n];
      if (target["extra_k" + n] !== "k" + n) bad++;
      if (target["k" + n] !== undefined) bad++;
      target["k" + n] = n * 10;
      if (target["k" + n] !== n * 10) bad++;
    }
    console.log(bad + "," + Object.keys(target).length);
"#;

#[test]
fn a_trap_that_reshapes_the_receiver_mid_region_still_reads_back_right() {
    assert_eq!(run_ok(TRAP_MUTATES), ["0,40"]);
}

/// The receiver KIND changes under a compiled site: the same `delete o[k]`
/// sees a plain object, an array (canonical index -> hole, non-canonical ->
/// named prop), a frozen object, `null`-prototype, a boxed String and a
/// TypedArray. The helper serves them all through the shared waterfall, so the
/// answers must match what each receiver would give on its own.
const POLY_RECEIVER: &str = r#"
    var log = [];
    function mk(i) {
      switch (i % 6) {
        case 0: { var o = {}; for (var p = 0; p < 15; p++) o["idx" + p] = p; return o; }
        case 1: { var a = [0, 1, 2, 3]; a["idx2"] = "named"; return a; }
        case 2: return Object.freeze({ idx0: 1, idx1: 2, idx2: 3 });
        case 3: { var o = Object.create(null); o["idx1"] = 1; o["idx2"] = 2; return o; }
        case 4: { var s = new String("abcd"); s["idx1"] = 9; return s; }
        default: { var t = new Int32Array(4); return t; }
      }
    }
    var acc = "";
    for (var i = 0; i < 1200; i++) {
      var o = mk(i);
      var r = delete o["idx" + (i % 4)];
      if (i < 24) acc += (r ? "1" : "0");
    }
    console.log(acc);
"#;

#[test]
fn a_compiled_delete_site_serves_every_receiver_kind() {
    // The expected string is whatever the interpreter answers; `zz_nojit` and
    // the latch-off children re-derive it independently and must agree.
    let out = run_ok(POLY_RECEIVER);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].len(), 24, "expected 24 flags, got {:?}", out[0]);
    // node v24.12.0 prints this exact string. The four zeros sit at i = 2, 8,
    // 14, 20 -- the frozen receiver (i % 6 == 2), whose idx0/idx1/idx2 are all
    // own and all non-configurable, so `delete` answers false. Every other
    // combination either removes a real own property or deletes vacuously.
    assert_eq!(out, ["110111110111110111110111"]);
}

/// `delete` on a receiver whose key is NOT an Int (so the helper's fast arm
/// declines and it materialises the key) interleaved with the Int form at the
/// SAME site, plus deletes of keys that do not exist.
const MIXED_KEYS: &str = r#"
    "use strict";
    var t = 0, f = 0;
    for (var i = 0; i < 2000; i++) {
      var o = { a: 1, b: 2, c: 3 };
      o["p" + i] = i;
      if (delete o["p" + i]) t++; else f++;
      if (delete o["p" + (i + 100000)]) t++; else f++;      // absent key
      if (delete o["p" + (i / 2)]) t++; else f++;           // non-Int key
      if (o.a !== 1 || o.b !== 2 || o.c !== 3) { console.log("CORRUPT at " + i); break; }
    }
    console.log(t + "," + f);
"#;

#[test]
fn absent_and_non_int_keys_at_a_compiled_delete_site() {
    assert_eq!(run_ok(MIXED_KEYS), ["6000,0"]);
}

/// Every battery again in a child process with one W19 latch cleared (and once
/// with the JIT off entirely). These are perf mechanisms: the output must not
/// depend on any of them.
fn rerun_with(env: &[(&str, &str)]) {
    if std::env::var_os("ZIPP_W19_DELETE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["--skip", "zz_"])
        .env("ZIPP_W19_DELETE_CHILD", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("re-run test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains(" 0 passed"),
        "{env:?} diverges:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn zz_nojit_agrees() {
    rerun_with(&[("ZIPP_NOJIT", "1")]);
}

#[test]
fn zz_no_jit_delete_agrees() {
    rerun_with(&[("ZIPP_NO_JIT_DELETE", "1")]);
}

#[test]
fn zz_no_split_propindex_agrees() {
    rerun_with(&[("ZIPP_NO_SPLIT_PROPINDEX", "1")]);
}

#[test]
fn zz_no_delete_fastpath_agrees() {
    rerun_with(&[("ZIPP_NO_DELETE_FASTPATH", "1")]);
}

#[test]
fn zz_all_w19_latches_off_agrees() {
    rerun_with(&[
        ("ZIPP_NO_JIT_DELETE", "1"),
        ("ZIPP_NO_SPLIT_PROPINDEX", "1"),
        ("ZIPP_NO_DELETE_FASTPATH", "1"),
    ]);
}
