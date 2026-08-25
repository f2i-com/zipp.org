//! `delete a[k]` on an Array must decide index-vs-named with the CANONICAL
//! array-index rule (`"0"` or `[1-9][0-9]*` with value <= 2^32-2). Before the
//! fix, `delete a["05"]` parsed the key non-canonically and punched a hole in
//! element 5; `"05"`, `"+5"`, `" 5"`, `"1e3"`, `"-0"` and `"4294967295"`
//! (2^32-1) are ordinary NAMED string properties and must take the
//! string-property route (access.rs `delete_prop`: the non-configurable
//! arr_props gate, the index-delete path, and the arr_props fallback).
//!
//! Every expected line below is node v24.12.0 output (the same program run as
//! a script). `zz_nojit_agrees_with_node` re-runs the whole battery under
//! `ZIPP_NOJIT=1`, so both tiers must match node byte-for-byte.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Dense array with an own string prop literally named "05": deleting "05"
/// removes ONLY the string prop (element 5 intact); the non-canonical
/// spellings delete vacuously (true, nothing removed); deleting "5" punches
/// the hole; Object.keys / for-in confirm the survivors; "4294967294" /
/// "4294967295" delete vacuously (neither is an own prop here).
const DENSE: &str = r#"
    "use strict";
    var a = [10, 11, 12, 13, 14, 15, 16, 17];
    a["05"] = "s05";
    console.log((delete a["05"]) + "," + a[5] + "," + a["05"] + "," + a.hasOwnProperty("05") + "," + a.hasOwnProperty("5"));
    a["05"] = "s05b";
    console.log((delete a["+5"]) + "," + (delete a[" 5"]) + "," + (delete a["1e3"]) + "," + (delete a["-0"]) + "," + (delete a["5.0"]));
    console.log(a[5] + "," + a["05"] + "," + (5 in a) + "," + a.length);
    console.log((delete a["5"]) + "," + a[5] + "," + ("5" in a) + "," + a.hasOwnProperty("5") + "," + a["05"] + "," + a.length);
    console.log(Object.keys(a).join("|"));
    var r = []; for (var k in a) r.push(k); console.log(r.join("|"));
    console.log((delete a["4294967294"]) + "," + (delete a["4294967295"]) + "," + a.length);
"#;

#[test]
fn dense_array_delete_battery() {
    let out = run_ok(DENSE);
    assert_eq!(
        out,
        [
            "true,15,undefined,false,true",
            "true,true,true,true,true",
            "15,s05b,true,8",
            "true,undefined,false,false,s05b,8",
            "0|1|2|3|4|6|7|05",
            "0|1|2|3|4|6|7|05",
            "true,true,8",
        ]
    );
}

/// Sparse array (length 2^32-1 via the "4294967294" index): "4294967294" IS a
/// canonical index (own element); "4294967295" is a plain STRING prop —
/// deleting it must remove the arr_props entry (pre-fix, the index path
/// claimed it, answered true, and LEFT the prop in place). Deleting the
/// string prop / an absent index never shrinks length.
const SPARSE: &str = r#"
    "use strict";
    var b = [];
    b.length = 1000;
    b[0] = "z";
    b[5] = "own5";
    b[300] = "far";
    b["05"] = "s05";
    b["4294967294"] = "hi94";
    b["4294967295"] = "s95";
    console.log(b.length + "," + b.hasOwnProperty("4294967294") + "," + b.hasOwnProperty("4294967295"));
    console.log((delete b["05"]) + "," + b[5] + "," + b["05"] + "," + b.hasOwnProperty("5"));
    console.log((delete b["4294967295"]) + "," + b.hasOwnProperty("4294967295") + "," + b["4294967295"] + "," + b.length);
    console.log((delete b["4294967294"]) + "," + b.hasOwnProperty("4294967294") + "," + ("4294967294" in b) + "," + b.length);
    console.log((delete b["+5"]) + "," + (delete b[" 5"]) + "," + b[5] + "," + ("5" in b));
    console.log((delete b["5"]) + "," + b[5] + "," + ("5" in b) + "," + b.hasOwnProperty("5") + "," + b.length);
    var r = []; for (var k in b) r.push(k); console.log(r.join("|"));
    console.log(Object.keys(b).join("|"));
"#;

#[test]
fn sparse_array_delete_battery() {
    let out = run_ok(SPARSE);
    assert_eq!(
        out,
        [
            "4294967295,true,true",
            "true,own5,undefined,true",
            "true,false,undefined,4294967295",
            "true,false,false,4294967295",
            "true,true,own5,true",
            "true,undefined,false,false,4294967295",
            "0|300",
            "0|300",
        ]
    );
}

/// defineProperty'd "05" (configurable, then NON-configurable): delete "05"
/// drops the named prop and leaves element 5 intact; the non-configurable
/// re-define refuses Reflect.deleteProperty (false) and throws in strict
/// `delete`. A non-configurable NAMED "4294967295" (2^32-1, NOT an index)
/// must ALSO refuse deletion — pre-fix it was excluded from the
/// non-configurable arr_props gate as a "numeric" key.
const DEFINE: &str = r#"
    "use strict";
    var c = [0, 1, 2, 3, 4, 55, 6];
    Object.defineProperty(c, "05", { value: "dp05", writable: true, enumerable: true, configurable: true });
    console.log(c["05"] + "," + c[5] + "," + Object.keys(c).join("|"));
    console.log((delete c["05"]) + "," + c["05"] + "," + c[5] + "," + ("5" in c) + "," + c.length);
    Object.defineProperty(c, "05", { value: "nc05", configurable: false });
    console.log(Reflect.deleteProperty(c, "05") + "," + c["05"] + "," + c[5]);
    var threw = "no";
    try { delete c["05"]; } catch (e) { threw = e instanceof TypeError ? "TypeError" : "other"; }
    console.log(threw + "," + c["05"] + "," + c[5]);
    console.log(Reflect.deleteProperty(c, "5") + "," + c[5] + "," + ("5" in c) + "," + c.length);
    var d = [0, 1, 2];
    Object.defineProperty(d, "4294967295", { value: "nc95", enumerable: true, configurable: false });
    console.log(Reflect.deleteProperty(d, "4294967295") + "," + d["4294967295"] + "," + d.hasOwnProperty("4294967295") + "," + d.length);
    var threw2 = "no";
    try { delete d["4294967295"]; } catch (e) { threw2 = e instanceof TypeError ? "TypeError" : "other"; }
    console.log(threw2 + "," + d["4294967295"]);
    var g = Object.getOwnPropertyDescriptor(d, "4294967295");
    console.log(g.value + "," + g.writable + "," + g.enumerable + "," + g.configurable);
"#;

#[test]
fn define_property_then_delete() {
    let out = run_ok(DEFINE);
    assert_eq!(
        out,
        [
            "dp05,55,0|1|2|3|4|5|6|05",
            "true,undefined,55,true,7",
            "false,nc05,55",
            "TypeError,nc05,55",
            "true,undefined,false,7",
            "false,nc95,true,3",
            "TypeError,nc95",
            "nc95,false,true,false",
        ]
    );
}

/// The same canonicality through Reflect.deleteProperty, plus a SEALED array:
/// an in-bounds index refuses deletion (false / strict TypeError) while the
/// sealed array's non-configurable NAMED "01" prop must refuse too (pre-fix
/// the fallback parsed "01" as index 1 and answered by hole-punching rules).
const REFLECT: &str = r#"
    "use strict";
    var e = [0, 1, 2, 3, 4, 5, 6];
    e["05"] = "s05";
    console.log(Reflect.deleteProperty(e, "05") + "," + e[5] + "," + e.hasOwnProperty("05"));
    console.log(Reflect.deleteProperty(e, "+5") + "," + Reflect.deleteProperty(e, " 5") + "," + e[5]);
    console.log(Reflect.deleteProperty(e, "5") + "," + e[5] + "," + ("5" in e) + "," + e.length);
    console.log(Reflect.deleteProperty(e, "4294967294") + "," + Reflect.deleteProperty(e, "4294967295") + "," + e.length);
    var f = [7, 8];
    f["01"] = "s01";
    Object.seal(f);
    console.log(Reflect.deleteProperty(f, "1") + "," + f[1] + "," + Reflect.deleteProperty(f, "01") + "," + f[1] + "," + f["01"]);
    var threw = "no";
    try { delete f[1]; } catch (err) { threw = err instanceof TypeError ? "TypeError" : "other"; }
    console.log(threw + "," + f[1]);
    var threw2 = "no";
    try { delete f["01"]; } catch (err) { threw2 = err instanceof TypeError ? "TypeError" : "other"; }
    console.log(threw2 + "," + f["01"] + "," + f[1]);
"#;

#[test]
fn reflect_delete_and_sealed() {
    let out = run_ok(REFLECT);
    assert_eq!(
        out,
        [
            "true,5,false",
            "true,true,5",
            "true,undefined,false,7",
            "true,true,7",
            "false,8,false,8,s01",
            "TypeError,8",
            "TypeError,s01,8",
        ]
    );
}

/// A Proxy's deleteProperty trap sees the raw key strings and forwards to the
/// Array target with the same canonical routing; a String exotic's in-range
/// index refuses deletion while "00"/"01" (non-canonical, not own) delete
/// vacuously without touching the chars.
const PROXY_STRING: &str = r#"
    "use strict";
    var t = [0, 1, 2, 3, 4, 5];
    t["05"] = "s05";
    var seen = [];
    var p = new Proxy(t, { deleteProperty: function (tt, k) { seen.push(k); return Reflect.deleteProperty(tt, k); } });
    console.log((delete p["05"]) + "," + t[5] + "," + t.hasOwnProperty("05"));
    console.log((delete p["5"]) + "," + t[5] + "," + ("5" in t));
    console.log(seen.join("|"));
    var s = new String("abc");
    s.foo = 1;
    var nn = "no";
    try { delete s["0"]; } catch (e) { nn = e instanceof TypeError ? "TypeError" : "other"; }
    console.log(nn + "," + (delete s["00"]) + "," + (delete s["01"]) + "," + s[0]);
    console.log((delete s.foo) + "," + s.foo);
"#;

#[test]
fn proxy_trap_and_string_exotic() {
    let out = run_ok(PROXY_STRING);
    assert_eq!(
        out,
        [
            "true,5,false",
            "true,undefined,false",
            "05|5",
            "TypeError,true,true,a",
            "true,undefined",
        ]
    );
}

/// The whole battery again with `ZIPP_NOJIT=1` (pure interpreter) in a child
/// process — the env latch is read once per process, so a fresh re-run of
/// this test binary is the only way to exercise the other tier.
#[test]
fn zz_nojit_agrees_with_node() {
    if std::env::var_os("ZIPP_DELETE_CANONICAL_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--skip", "zz_nojit_agrees_with_node"])
        .env("ZIPP_NOJIT", "1")
        .env("ZIPP_DELETE_CANONICAL_CHILD", "1")
        .output()
        .expect("re-run test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains(" 0 passed"),
        "ZIPP_NOJIT=1 diverges:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
}
