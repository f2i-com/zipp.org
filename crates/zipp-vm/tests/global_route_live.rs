//! A global whose route changes while the code that reads or writes it is
//! already running, against Node. Compiled code reaches a global by its slot,
//! and checked that the slot is still the binding only when it was entered
//! (`Vm::jit_globals_still_routable`); a `delete`, or a getter, a setter or
//! read-only attributes put behind the name by something the running loop or
//! function itself called, left it on the raw slot until it returned: reads
//! answered the old value or the deleted sentinel, writes skipped the setter
//! and the read-only check, calls went to the function the slot still held.
//! Now the change sets `Vm::jit_route_dirty`, a compiled global access tests
//! it first, and the interpreter takes the instruction. Here: a global
//! deleted under a loop (the next read throws); given a getter, a setter and
//! read-only attributes (a sloppy write dropped, a strict one throwing); the
//! change made by a getter the loop reads and by a function it calls, read
//! before and after the call; a function with no loop reading and writing
//! after its own call; a loop inside a loop; a global function replaced by a
//! getter that answers another; a counter global in a typed-array loop; a
//! global made an accessor, deleted and made plain again, and the function
//! run again afterwards; a script's own loop; a method and a closure. In the
//! interpreter with and without its loop runner, by default, and with every
//! function compiled at once.

const SOURCE: &str = r##"// A global whose route changes while the code reading or writing it is already running: deleted, or given a
// getter, a setter or read-only attributes, by something the running loop or function itself called. Every
// global here is an implicit one (a configurable property of the global object).
var out = [];
function change(f) { [0].forEach(f); }

// 1. Deleted under a loop: the next read throws.
delG = 1;
function delUnder(n) { var t = 0; try { for (var i = 0; i < n; i++) { t += delG; if (i === 1500) change(function () { delete globalThis.delG; }); } } catch (e) { return e.name + " at " + t; } return "no throw " + t; }
out.push(delUnder(3500));

// 2. Given a getter under a loop: reads run it.
getG = 1; var getHits = 0;
function getUnder(n) { var t = 0; for (var i = 0; i < n; i++) { t += getG; if (i === 1500) change(function () { Object.defineProperty(globalThis, "getG", { get: function () { getHits++; return 5; }, configurable: true }); }); } return t + ":" + getHits; }
out.push(getUnder(3500));

// 3. Given a setter under a loop: writes run it.
setG = 0; var setHits = 0;
function setUnder(n) { for (var i = 0; i < n; i++) { setG = i; if (i === 1500) change(function () { Object.defineProperty(globalThis, "setG", { set: function (v) { setHits += v & 1; }, get: function () { return -1; }, configurable: true }); }); } return setG + ":" + setHits; }
out.push(setUnder(3500));

// 4. Made read-only under a loop: a sloppy write is dropped, a strict one throws.
roG = 0;
function roUnder(n) { for (var i = 0; i < n; i++) { roG = i; if (i === 1500) change(function () { Object.defineProperty(globalThis, "roG", { value: 7, writable: false, configurable: true }); }); } return roG; }
out.push(roUnder(3500));
roS = 0;
function roStrict(n) { "use strict"; var i = 0; try { for (; i < n; i++) { roS = i; if (i === 1500) change(function () { Object.defineProperty(globalThis, "roS", { value: 9, writable: false, configurable: true }); }); } } catch (e) { return e.name + " at " + i + " " + roS; } return "no throw " + roS; }
out.push(roStrict(3500));

// 5. The change made by a getter the loop reads (no call in the loop's own code).
viaG = 1; var viaHits = 0;
var trigger = { arm: 0, get now() { if (this.arm++ === 1500) Object.defineProperty(globalThis, "viaG", { get: function () { viaHits++; return 5; }, configurable: true }); return 0; } };
function viaGetter(n) { var t = 0; for (var i = 0; i < n; i++) { t += viaG + trigger.now; } return t + ":" + viaHits; }
out.push(viaGetter(3500));

// 6. The change made in a function the loop calls directly, read before and after the call in one iteration.
calG = 1; var calHits = 0;
function maybe(i) { if (i === 1500) Object.defineProperty(globalThis, "calG", { get: function () { calHits++; return 5; }, configurable: true }); return 0; }
function calUnder(n) { var t = 0; for (var i = 0; i < n; i++) { t += calG + maybe(i) + calG; } return t + ":" + calHits; }
out.push(calUnder(3500));

// 7. A function with no loop, called many times: the read after its call, in the same activation.
bodyG = 1; var bodyHits = 0;
function touch(i) { if (i === 2500) Object.defineProperty(globalThis, "bodyG", { get: function () { bodyHits++; return 3; }, configurable: true }); return 0; }
function body(i) { var a = bodyG; touch(i); var b = bodyG; return a * 10 + b; }
function bodies(n) { var t = 0; for (var i = 0; i < n; i++) t += body(i); return t + ":" + bodyHits; }
out.push(bodies(5000));

// 8. The same for a write after the call.
wrG = 0; var wrSeen = 0;
function arm(i) { if (i === 2500) Object.defineProperty(globalThis, "wrG", { set: function (v) { wrSeen++; }, get: function () { return -2; }, configurable: true }); }
function writer(i) { arm(i); wrG = i; return wrG; }
function writers(n) { var t = 0; for (var i = 0; i < n; i++) t += writer(i) & 1; return t + ":" + wrSeen; }
out.push(writers(5000));

// 9. A loop that calls a function with a loop of its own, which makes the change: both go on reading.
nestG = 1; var nestHits = 0;
function inner(i) { var s = 0; for (var j = 0; j < 20; j++) { s += nestG; if (i === 200 && j === 10) Object.defineProperty(globalThis, "nestG", { get: function () { nestHits++; return 2; }, configurable: true }); } return s; }
function outer(n) { var t = 0; for (var i = 0; i < n; i++) { t += inner(i) + nestG; } return t + ":" + nestHits; }
out.push(outer(400));

// 10. A global function replaced by a getter that answers another function.
fnG = function () { return 1; }; var fnHits = 0;
function callsUnder(n) { var t = 0; for (var i = 0; i < n; i++) { t += fnG(); if (i === 1500) change(function () { Object.defineProperty(globalThis, "fnG", { get: function () { fnHits++; return function () { return 4; }; }, configurable: true }); }); } return t + ":" + fnHits; }
out.push(callsUnder(3500));

// 11. A counter kept in a global through a typed-array loop that calls out once.
cntG = 0; var cntSets = 0;
function counted(n) {
  var ta = new Int32Array(8), s = 0;
  for (var i = 0; i < n; i++) { ta[i & 7] = cntG; cntG = cntG + 1; s = (s + ta[(i + 3) & 7]) | 0; if (i === 1500) change(function () { var v = cntG; Object.defineProperty(globalThis, "cntG", { get: function () { return v; }, set: function (x) { cntSets++; v = x + 1; }, configurable: true }); }); }
  return s + ":" + cntG + ":" + cntSets;
}
out.push(counted(3500));

// 12. Deleted and made a plain global again under the loop; and the function run again after it.
backG = 1; var backHits = 0;
function back(n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t += backG;
    if (i === 1000) change(function () { Object.defineProperty(globalThis, "backG", { get: function () { backHits++; return 5; }, configurable: true }); });
    if (i === 2000) change(function () { delete globalThis.backG; globalThis.backG = 2; });
  }
  return t + ":" + backHits;
}
out.push(back(3000));
out.push(back(3000));

// 13. A script's own loop.
topG = 1; var topHits = 0, topT = 0;
for (var ti = 0; ti < 3500; ti++) { topT += topG; if (ti === 1500) change(function () { Object.defineProperty(globalThis, "topG", { get: function () { topHits++; return 5; }, configurable: true }); }); }
out.push(topT + ":" + topHits);

// 14. A method and a closure reading a global that changes under them.
clsG = 1; var clsHits = 0;
class Reader { sum(n) { var t = 0; for (var i = 0; i < n; i++) { t += clsG; if (i === 1500) change(function () { Object.defineProperty(globalThis, "clsG", { get: function () { clsHits++; return 6; }, configurable: true }); }); } return t; } }
out.push(new Reader().sum(3500) + ":" + clsHits);
cloG = 1; var cloHits = 0;
function makeReader() { var acc = 0; return function (n) { for (var i = 0; i < n; i++) { acc += cloG; if (i === 1500) change(function () { delete globalThis.cloG; globalThis.cloG = 3; }); } return acc; }; }
out.push(makeReader()(3500) + ":" + cloHits);

console.log(out.join("\n"));
"##;

const EXPECTED: &str = "ReferenceError at 1501\n11496:1999\n-1:1000\n7\nTypeError at 1501 9\n11496:1999\n22996:3999\n109980:4999\n1250:2500\n12589:4189\n9497:1999\n8092786:5499:1999\n7999:1000\n9000:2000\n11496:1999\n13495:1999\n7498:0";

/// Every switch a mode below may set, cleared before each run.
const SWITCHES: [&str; 6] = [
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
    "ZIPP_LOOP_RUNNER_EAGER",
    "ZIPP_NO_LOOP_RUNNER",
    "ZIPP_NO_LOOP_FRAMES",
    "ZIPP_NO_LITE",
];

#[test]
fn global_route_live_worker() {
    if std::env::var_os("ZIPP_GLOBAL_ROUTE_LIVE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn global_route_live_match_node() {
    if std::env::var_os("ZIPP_GLOBAL_ROUTE_LIVE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let modes: [(&str, &[&str]); 7] = [
        ("default", &[]),
        ("interpreter", &["ZIPP_NOJIT"]),
        ("interpreter, runner from the first back-edge", &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER"]),
        (
            "interpreter, runner from the first back-edge, no lite loop",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LITE"],
        ),
        (
            "interpreter, runner from the first back-edge, no frame of its own",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LOOP_FRAMES"],
        ),
        ("interpreter, no runner", &["ZIPP_NOJIT", "ZIPP_NO_LOOP_RUNNER"]),
        ("compile-at-once", &["ZIPP_JIT_THRESHOLD"]),
    ];
    for (mode, vars) in modes {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["global_route_live_worker", "--exact", "--nocapture"])
            .env("ZIPP_GLOBAL_ROUTE_LIVE_WORKER", "1");
        for v in SWITCHES {
            command.env_remove(v);
        }
        for v in vars {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
