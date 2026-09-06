//! B289: the captured-intrinsic lane. Under the strict default call order
//! (B280) a member call whose argument is not order-transparent lowers to
//! `GetProp; <args>; CallWithThis`, and the interpreter serves the captured
//! callee through the fused lowering's builtin lanes when — and only when —
//! it is the boot intrinsic the receiver's prototype still holds. These
//! probes pin the observable contract around that proof: an override
//! installed by the argument leaves the captured intrinsic in charge, an
//! override installed before the capture is what runs, an own shadow and a
//! foreign intrinsic under the name decline, a frozen receiver still throws,
//! and a throwing callee Get runs before any argument. Expected values come
//! from `node -e` of the same program (v22).

const PROBES: &str = r#""use strict";
var res = "";
var origPush = Array.prototype.push, origCCA = String.prototype.charCodeAt;
// 1 captured push over a global-read argument
var a = []; var g = 0;
for (g = 0; g < 5; g++) a.push(g % 3);
res += ";" + ("p1=" + a.join(","));
// 2 the argument replaces the prototype method: the captured intrinsic is still what runs
function f() { Array.prototype.push = function () { return "over"; }; return 1; }
var r = a.push(f());
res += ";" + ("p2=" + r + "/" + a.length);
var r2 = a.push(f());
res += ";" + ("p3=" + r2 + "/" + a.length);
Array.prototype.push = origPush;
// 4 own shadow
var b = []; b.push = function (x) { return "mine" + x; }; var q = 1;
res += ";" + ("p4=" + b.push(q + 1));
// 5 override before capture, restored by the argument: the override was captured
Array.prototype.push = function () { return "over2"; };
var c = []; function restore() { Array.prototype.push = origPush; return 1; }
res += ";" + ("p5=" + c.push(restore()) + "/" + c.length);
// 6 another intrinsic assigned under this name
var d = [1, 2, 3]; d.push = Array.prototype.pop; var z = 0;
res += ";" + ("p6=" + d.push(z + 1) + "/" + d.length);
// 7 charCodeAt over a non-transparent argument, then an override installed by the argument
var s = "abc"; var i = 0;
res += ";" + ("p7=" + s.charCodeAt(i + 1));
function h() { String.prototype.charCodeAt = function () { return -1; }; return 0; }
res += ";" + ("p8=" + s.charCodeAt(h()));
res += ";" + ("p9=" + s.charCodeAt(0));
String.prototype.charCodeAt = origCCA;
// 10 frozen receiver must throw through the captured lane
var fr = Object.freeze([1]); var k = 1;
try { fr.push(k + 1); res += ";" + ("p10=no-throw"); } catch (e) { res += ";" + ("p10=" + (e instanceof TypeError)); }
// 11 rope receiver
var big = ""; for (var n = 0; n < 40; n++) big += "ab"; var j = 3;
res += ";" + ("p11=" + big.charCodeAt(j + 0));
// 12 a captured array builtin without a dedicated lane goes through the dispatcher
var e = [5, 6, 7]; var w = 6;
res += ";" + ("p12=" + e.indexOf(w + 1) + "/" + e.slice(w - 5).join("-"));
// 13 the callee Get itself throws before any argument runs
var order = []; var o = { get m() { order.push("get"); throw new Error("boom"); } };
function arg() { order.push("arg"); return 1; }
try { o.m(arg()); } catch (err) { order.push(err.message); }
res += ";" + ("p13=" + order.join(">"));
console.log(res.slice(1));
"#;

const EXPECTED: &str = "p1=0,1,2,0,1;p2=6/6;p3=over/6;p4=mine2;p5=over2/0;p6=3/2;p7=98;p8=97;p9=-1;p10=true;p11=98;p12=2/6-7;p13=get>boom";

/// The read half: `arr.push` / `s.charCodeAt` answered from the baseline
/// proof must be exactly what a real Get resolves — the boot intrinsic by
/// identity, an own shadow (function or not), a replaced prototype or
/// prototype slot, a prototype accessor, a subclass instance, a null
/// prototype — and the fused lowering must observe an own shadow on an
/// array too (r4, r5: served the intrinsic at v0.0.14).
const READ_PROBES: &str = r#""use strict";
var res = "";
var arr = [1, 2, 3]; var s = "abc";
// 1 the read answers the boot intrinsic by identity
var f = arr.push; res += ";" + ("r1=" + (f === Array.prototype.push));
res += ";" + ("r2=" + (s.charCodeAt === String.prototype.charCodeAt));
// 3 data properties on the prototype that are not functions
res += ";" + ("r3=" + (arr.constructor === Array) + "/" + arr.length + "/" + s.length);
// 4 an own shadow wins the read and the fused call alike
var c = [1]; c.push = function (x) { return "lit" + x; };
res += ";" + ("r4=" + c.push(1) + "/" + (c.push === Array.prototype.push));
var e = [1, 2]; e.indexOf = function () { return "sh"; };
res += ";" + ("r5=" + e.indexOf(1) + "/" + e.slice(1).length);
// 6 a replaced prototype wins the read
var p = []; Object.setPrototypeOf(p, { push: 7 });
res += ";" + ("r6=" + p.push);
// 7 a replaced prototype slot is what the read answers, and the fused call observes it
var origCCA = String.prototype.charCodeAt;
String.prototype.charCodeAt = function () { return -1; };
res += ";" + ("r7=" + (s.charCodeAt === origCCA) + "/" + s.charCodeAt(0));
String.prototype.charCodeAt = origCCA;
res += ";" + ("r8=" + (s.charCodeAt === origCCA) + "/" + s.charCodeAt(0));
// 9 an accessor on the prototype is honoured
Object.defineProperty(Array.prototype, "peek", { get: function () { return this.length * 10; }, configurable: true });
res += ";" + ("r9=" + arr.peek);
delete Array.prototype.peek;
// 10 an own non-function shadow, then removing it restores the intrinsic
var q = [1, 2]; q.push = 5; res += ";" + ("r10=" + q.push); delete q.push; res += ";" + ("r11=" + (q.push === Array.prototype.push));
// 12 a subclass instance reads its subclass method
class MyArr extends Array { push(x) { return "sub" + x; } }
var m = new MyArr(); res += ";" + ("r12=" + m.push(1) + "/" + m.push(1 + 1));
// 13 an array whose prototype was set to null
var n = [1]; Object.setPrototypeOf(n, null); res += ";" + ("r13=" + n.push);
console.log(res.slice(1));
"#;

const READ_EXPECTED: &str = "r1=true;r2=true;r3=true/3/3;r4=lit1/false;r5=sh/1;r6=7;r7=false/-1;r8=true/97;r9=30;r10=5;r11=true;r12=sub1/sub2;r13=undefined";

/// Map and Set receivers: the collection proof keys the same lane and read
/// path. Own shadow, argument-installed override, subclass instance and
/// replaced prototype all resolve as a real Get would.
const COLL_PROBES: &str = r#""use strict";
var res = "";
var m = new Map([[1, "a"], [2, "b"]]); var st = new Set([1, 2]); var k = 1;
// 1 captured get/has over non-transparent arguments
res += ";" + ("c1=" + m.get(k + 1) + "/" + st.has(k + 1) + "/" + (m.get === Map.prototype.get));
// 2 own shadow on a Map wins the read and both call forms
var m2 = new Map([[1, "x"]]); m2.get = function (key) { return "own" + key; };
res += ";" + ("c2=" + m2.get(1) + "/" + m2.get(k + 0) + "/" + (m2.get === Map.prototype.get));
// 3 a prototype override installed by the argument leaves the captured intrinsic running
var origGet = Map.prototype.get;
function swap() { Map.prototype.get = function () { return "over"; }; return 1; }
res += ";" + ("c3=" + m.get(swap()) + "/" + m.get(k + 0));
Map.prototype.get = origGet;
res += ";" + ("c4=" + m.get(k + 0) + "/" + (m.get === origGet));
// 5 subclass instance
class MyMap extends Map { get(key) { return "sub" + key; } }
var mm = new MyMap([[1, "z"]]);
res += ";" + ("c5=" + mm.get(1) + "/" + mm.get(k + 0) + "/" + (mm.get === origGet));
// 6 replaced prototype
var m3 = new Map(); Object.setPrototypeOf(m3, { get: function () { return "proto"; } });
res += ";" + ("c6=" + m3.get(k + 0) + "/" + typeof m3.get);
// 7 Set add through the captured lane returns the set itself
res += ";" + ("c7=" + (st.add(k + 5) === st) + "/" + st.size);
console.log(res.slice(1));
"#;

const COLL_EXPECTED: &str =
    "c1=b/true/true;c2=own1/own1/false;c3=a/over;c4=a/true;c5=sub1/sub1/false;c6=proto/function;c7=true/3";

fn run_probes() -> String {
    let out = zipp_vm::run(PROBES).expect("probes compile");
    assert!(out.error.is_none(), "probes threw: {:?}", out.error);
    out.output.join("\n")
}

#[test]
fn captured_intrinsic_lane_matches_node() {
    assert_eq!(run_probes(), EXPECTED);
    let out = zipp_vm::run(READ_PROBES).expect("read probes compile");
    assert!(out.error.is_none(), "read probes threw: {:?}", out.error);
    assert_eq!(out.output.join("\n"), READ_EXPECTED);
    let out = zipp_vm::run(COLL_PROBES).expect("collection probes compile");
    assert!(
        out.error.is_none(),
        "collection probes threw: {:?}",
        out.error
    );
    assert_eq!(out.output.join("\n"), COLL_EXPECTED);
}

/// The captured lowering carries the member name: the compiler's
/// `CallWithThis` for `a.push(g % 3)` names `push`, and the `with`-call
/// lowering — a captured callee with no member — carries the sentinel.
#[test]
fn captured_call_carries_the_member_name() {
    let text =
        zipp_vm::compile_to_text("var a = []; var g = 1; a.push(g % 3);", false).expect("compiles");
    let call = text
        .find("CallWithThis {")
        .expect("the strict default lowers a global-read argument to CallWithThis");
    let block = &text[call..call + text[call..].find('}').expect("block end")];
    assert!(
        block.contains("name:") && !block.contains("name: 4294967295"),
        "captured member call without its name:\n{block}"
    );
    let with_text = zipp_vm::compile_to_text(
        "var o = { f: function () { return 1; } }; with (o) { f(); }",
        false,
    )
    .expect("compiles");
    let call = with_text
        .find("CallWithThis {")
        .expect("with-call lowering");
    let block = &with_text[call..call + with_text[call..].find('}').expect("block end")];
    assert!(
        block.contains("name: 4294967295"),
        "with-resolved call must carry NO_NAME:\n{block}"
    );
}

/// The same probes under the relaxed switch (the pre-B280 lowering) and
/// with the JIT off: the lane is an interpreter path, and the contract it
/// serves must not depend on which lowering the compiler chose.
#[test]
fn captured_intrinsic_lane_probe_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (label, extra) in [
        ("relaxed-order", vec![("ZIPP_RELAXED_CALL_ORDER", "1")]),
        ("interpreter", vec![("ZIPP_NOJIT", "1")]),
        ("gc-stress", vec![("ZIPP_GC_STRESS", "1")]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("captured_intrinsic_lane_matches_node")
            .arg("--exact")
            .env_remove("ZIPP_RELAXED_CALL_ORDER")
            .env_remove("ZIPP_STRICT_CALL_ORDER");
        for (k, v) in &extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn the probe child");
        assert!(
            out.status.success(),
            "{label} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
