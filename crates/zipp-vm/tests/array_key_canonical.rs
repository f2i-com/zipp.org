//! Canonical numeric-string array keys take the element path with no per-probe
//! String allocation (values.rs `[[HasProperty]]` Array arms, indexing_date.rs
//! `get_index`). A string is an array index IFF it is `"0"` or
//! `[1-9][0-9]{0,9}` with value <= 2^32-2 — `"05"`, `"+5"`, `" 5"`,
//! `"4294967295"`, `"-0"` and `"1e3"` are ordinary string properties and must
//! keep resolving through the named-property route.
//!
//! Every expected line below is node v24.12.0 output (the same program run as
//! a script). `zz_old_paths_agree_with_node` re-runs this whole binary with
//! `ZIPP_NO_ARRKEY_FAST=1` (the old key_of + parse + to_string / generic
//! get_prop paths), so BOTH modes must match node exactly.
//!
//! NOT covered here: `delete a["05"]` — the delete path's canonical routing
//! has its own battery in `delete_canonical_key.rs` (access.rs `delete_prop`
//! now decides index-vs-named with the same `canonical_u32_key`).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Sparse array, default %Array.prototype% chain: `in` / hasOwnProperty /
/// computed reads over the whole canonicality battery, numeric-key agreement,
/// and an index delete + re-add re-check. `"4294967294"` is an own INDEX
/// (grows length to 2^32-1); `"4294967295"` is an own plain STRING prop.
const SPARSE_NO_PROTO: &str = r#"
    "use strict";
    var b = [];
    b.length = 1000;
    b[0] = "z";
    b[5] = "own5";
    b[300] = "far";
    b["05"] = "s05";
    b["1e3"] = "sE3";
    b["4294967294"] = "hi94";
    b["4294967295"] = "s95";
    var ks = ["5", "05", "+5", " 5", "4294967294", "4294967295", "4294967296", "-0", "1e3", "1000", "0", "300", "299", "length"];
    var r1 = [], r2 = [];
    for (var i = 0; i < ks.length; i++) r1.push(ks[i] + "=" + (ks[i] in b));
    for (var i = 0; i < ks.length; i++) r2.push(ks[i] + "=" + b.hasOwnProperty(ks[i]));
    console.log(r1.join("|"));
    console.log(r2.join("|"));
    console.log((5 in b) + "," + (4294967294 in b) + "," + (4294967295 in b) + "," + (-0 in b) + "," + (1000 in b));
    // Keep the canonical-key probe broad without making the test itself rely
    // on a >16-link binary-expression chain, which the hostile-code parser
    // profile deliberately rejects.
    var readKeys = ["5", "05", "+5", " 5", "4294967294", "4294967295", "4294967296", "-0", "1e3", "300", "0"];
    var readValues = [];
    for (var j = 0; j < readKeys.length; j++) readValues.push(String(b[readKeys[j]]));
    console.log(readValues.join(","));
    console.log(b.length);
    delete b[300];
    console.log(("300" in b) + "," + b.hasOwnProperty("300") + "," + b["300"] + "," + (300 in b));
    b[300] = "far2";
    console.log(("300" in b) + "," + b["300"] + "," + b.hasOwnProperty("300"));
"#;

#[test]
fn sparse_array_without_custom_proto() {
    let out = run_ok(SPARSE_NO_PROTO);
    assert_eq!(
        out,
        [
            "5=true|05=true|+5=false| 5=false|4294967294=true|4294967295=true|4294967296=false|-0=false|1e3=true|1000=false|0=true|300=true|299=false|length=true",
            "5=true|05=true|+5=false| 5=false|4294967294=true|4294967295=true|4294967296=false|-0=false|1e3=true|1000=false|0=true|300=true|299=false|length=true",
            "true,true,true,true,false",
            "own5,s05,undefined,undefined,hi94,s95,undefined,undefined,sE3,far,z",
            "4294967295",
            "false,false,undefined,false",
            "true,far2,true",
        ]
    );
}

/// Sparse array whose prototype carries index-named AND non-canonical
/// properties: `in` must see them through the chain, hasOwnProperty must not,
/// and reads must route `"5"` (element miss -> proto index walk) and `"05"`
/// (named route) to DIFFERENT proto values. Then delete-then-re-check where
/// the proto still answers.
const SPARSE_WITH_PROTO: &str = r#"
    "use strict";
    var proto = Object.create(Array.prototype);
    proto["5"] = "p5";
    proto["05"] = "p05";
    proto["7"] = "p7";
    proto["4294967294"] = "p94";
    proto["4294967295"] = "p95";
    proto["1e3"] = "pE3";
    proto["+5"] = "pPLUS";
    proto[" 5"] = "pSP";
    proto["-0"] = "pNEG0";
    var a = [];
    a.length = 50;
    a[5] = "own5";
    a[9] = "own9";
    Object.setPrototypeOf(a, proto);
    var ks = ["5", "05", "+5", " 5", "7", "9", "4294967294", "4294967295", "4294967296", "-0", "1e3"];
    var r1 = [], r2 = [], r3 = [];
    for (var i = 0; i < ks.length; i++) r1.push(ks[i] + "=" + (ks[i] in a));
    for (var i = 0; i < ks.length; i++) r2.push(ks[i] + "=" + a.hasOwnProperty(ks[i]));
    for (var i = 0; i < ks.length; i++) r3.push(ks[i] + "=" + a[ks[i]]);
    console.log(r1.join("|"));
    console.log(r2.join("|"));
    console.log(r3.join("|"));
    console.log(a[5] + "," + a[7] + "," + a["05"] + "," + a["5"]);
    delete a[5];
    console.log(("5" in a) + "," + a.hasOwnProperty("5") + "," + a["5"] + "," + a[5]);
    a[5] = "again5";
    console.log(("5" in a) + "," + a.hasOwnProperty("5") + "," + a["5"]);
"#;

#[test]
fn sparse_array_with_index_named_proto() {
    let out = run_ok(SPARSE_WITH_PROTO);
    assert_eq!(
        out,
        [
            "5=true|05=true|+5=true| 5=true|7=true|9=true|4294967294=true|4294967295=true|4294967296=false|-0=true|1e3=true",
            "5=true|05=false|+5=false| 5=false|7=false|9=true|4294967294=false|4294967295=false|4294967296=false|-0=false|1e3=false",
            "5=own5|05=p05|+5=pPLUS| 5=pSP|7=p7|9=own9|4294967294=p94|4294967295=p95|4294967296=undefined|-0=pNEG0|1e3=pE3",
            "own5,p7,p05,own5",
            "true,false,p5,p5",
            "true,true,again5",
        ]
    );
}

/// Dense array: the canonicality trap on a fully packed store (`"05" in d` is
/// false while `d[5]` exists), a delete-punched hole, then a proto carrying
/// both `"5"` and `"05"` answering differently through the hole.
const DENSE: &str = r#"
    "use strict";
    var d = [10, 11, 12, 13, 14, 15, 16];
    console.log(("5" in d) + "," + ("05" in d) + "," + ("6" in d) + "," + ("7" in d) + "," + ("-0" in d) + "," + (" 5" in d) + "," + ("+5" in d));
    console.log(d.hasOwnProperty("5") + "," + d.hasOwnProperty("05") + "," + d.hasOwnProperty("7"));
    console.log(d["5"] + "," + d["05"] + "," + d["6"] + "," + d["07"]);
    delete d[5];
    console.log(("5" in d) + "," + d.hasOwnProperty("5") + "," + d["5"] + "," + d.length);
    var proto2 = Object.create(Array.prototype);
    proto2["5"] = "q5";
    proto2["05"] = "q05";
    Object.setPrototypeOf(d, proto2);
    console.log(("5" in d) + "," + ("05" in d) + "," + d["5"] + "," + d["05"] + "," + d[5]);
"#;

#[test]
fn dense_array_canonicality_and_proto_shadow() {
    let out = run_ok(DENSE);
    assert_eq!(
        out,
        [
            "true,false,true,false,false,false,false",
            "true,false,false",
            "15,undefined,16,undefined",
            "false,false,undefined,7",
            "true,true,q5,q05,q5",
        ]
    );
}

/// for-in over a sparse array with a defineProperty'd index override and a
/// string prop; the loop body reads `c[k]` with the SNAPSHOT'S string key —
/// the exact shape the element fast path serves. Then the same walk with a
/// proto contributing an unshadowed index and a named prop, and after
/// deletions.
const FORIN_OVERRIDE: &str = r#"
    "use strict";
    var c = [];
    c.length = 40;
    c[3] = "c3";
    c[10] = "c10";
    Object.defineProperty(c, "7", { value: "ov7", enumerable: true, writable: true, configurable: true });
    c.name = "cstr";
    var r = [];
    for (var k in c) r.push(k + ":" + c[k]);
    console.log(r.join(","));
    var proto3 = Object.create(Array.prototype);
    proto3["7"] = "p7";
    proto3["12"] = "p12";
    proto3.pstr = "ps";
    Object.setPrototypeOf(c, proto3);
    var r2 = [];
    for (var k in c) r2.push(k + ":" + c[k]);
    console.log(r2.join(","));
    delete c[10];
    delete c.name;
    var r3 = [];
    for (var k in c) r3.push(k);
    console.log(r3.join(","));
"#;

#[test]
fn forin_sparse_with_index_override_and_string_prop() {
    let out = run_ok(FORIN_OVERRIDE);
    assert_eq!(
        out,
        [
            "3:c3,7:ov7,10:c10,name:cstr",
            "3:c3,7:ov7,10:c10,name:cstr,12:p12,pstr:ps",
            "3,7,12,pstr",
        ]
    );
}

/// Deleting snapshotted keys DURING for-in: the per-iteration liveness
/// re-check (forin_live -> has_property with the snapshot's string key) must
/// drop the deleted index and the deleted string prop, and must keep a
/// deleted own index alive when the proto still carries it.
const FORIN_LIVENESS: &str = r#"
    "use strict";
    var e = [];
    e.length = 30;
    e[2] = "a";
    e[5] = "b";
    e[9] = "c";
    e[20] = "d";
    e.s1 = "x";
    e.s2 = "y";
    var r = [];
    for (var k in e) { r.push(k); if (k === "2") { delete e[9]; delete e.s1; } }
    console.log(r.join(","));
    var proto4 = Object.create(Array.prototype);
    proto4["20"] = "p20";
    var e2 = [];
    e2.length = 30;
    e2[2] = "a";
    e2[20] = "d";
    Object.setPrototypeOf(e2, proto4);
    var r2 = [];
    for (var k in e2) { r2.push(k + ":" + e2[k]); if (k === "2") { delete e2[20]; } }
    console.log(r2.join(","));
"#;

#[test]
fn forin_liveness_recheck_after_deletes() {
    let out = run_ok(FORIN_LIVENESS);
    assert_eq!(out, ["2,5,20,s2", "2:a,20:p20"]);
}

/// The whole file again with `ZIPP_NO_ARRKEY_FAST=1`. The flag latches into a
/// process-wide AtomicU8 on first query (see `arrkey_fast_enabled`), so the
/// old paths can only be exercised by a fresh process: re-run this test
/// binary as a child with the flag set and every other test selected.
#[test]
fn zz_old_paths_agree_with_node() {
    if std::env::var_os("ZIPP_ARRKEY_CANONICAL_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--skip", "zz_old_paths_agree_with_node"])
        .env("ZIPP_NO_ARRKEY_FAST", "1")
        .env("ZIPP_ARRKEY_CANONICAL_CHILD", "1")
        .output()
        .expect("re-run test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains(" 0 passed"),
        "old paths (ZIPP_NO_ARRKEY_FAST=1) diverge:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
}
