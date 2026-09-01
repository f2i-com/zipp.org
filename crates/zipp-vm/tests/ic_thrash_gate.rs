//! The inline-cache refill thrash-gate and its rotation escape.
//!
//! A JIT `GetProp`/`SetProp` site owns eight identity-keyed ways. Once it has
//! cycled more receivers than that, every refill evicts the way that is about
//! to be needed, so the site sits at 100% miss where an 8-way cache should
//! deliver `(n-8)/n`. `Jit::ic_thrashing` already detected exactly that state,
//! but only ONE of the four data-fill paths consulted it; the proto-chain fill,
//! the own-data fill after the key scan (the dictionary and shape-new path) and
//! the `SetProp` fill all refilled unconditionally.
//!
//! Gating those three is only half of it. `ic_rot` — the round-robin cursor
//! `ic_thrashing` reads — is normally advanced by `set_ic`, so a site that
//! stops calling `set_ic` freezes forever on whichever eight receivers were
//! resident when it tripped. At a site reused across several receiver-count
//! phases those eight are already dead, and the frozen site still misses on
//! every live receiver. The escape is that a SUPPRESSED miss bumps the cursor
//! too: the `u8` wraps, `ic_thrashing` becomes periodic with period 256, and
//! the site re-samples its live working set for eight fills every 256 misses.
//!
//! An inline cache cannot change a value, only a timing — so every case below
//! must produce the same bytes with the gate on, with `ZIPP_NO_ICGATE=1`, and
//! under node. Each expectation was executed in node (v24) and diffed exactly.
//! The whole file must also pass with `ZIPP_NO_ICGATE=1` set for the process.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// A `GetProp` site walked over 1, 2, 4, 8, 9, 16 and 1024 receivers of ONE
/// shape — the gated memo path, and the phase sequence that leaves the ways
/// full of dead receivers before the megamorphic phases start.
#[test]
fn megamorphic_own_read_is_value_identical() {
    let out = run_ok(
        r#""use strict";
        function mk(n) { var a = []; for (var i = 0; i < n; i++) a.push({ rd: i * 3 + 1, pad: i }); return a; }
        function readLoop(objs, n, iters) {
          var k = 0, s = 0;
          for (var i = 0; i < iters; i++) { s = (s + objs[k].rd) | 0; k++; if (k === n) k = 0; }
          return s;
        }
        var t = 0, counts = [1, 2, 4, 8, 9, 16, 1024];
        for (var p = 0; p < counts.length; p++) t = (t + readLoop(mk(counts[p]), counts[p], 40000)) | 0;
        console.log(t);
        "#,
    );
    assert_eq!(out, vec!["63607810"]);
}

/// The same shape on the STORE path — `jit_set_prop_miss`'s own-data fill, the
/// one whose scan-root registration moved inside the fill branch.
#[test]
fn megamorphic_own_write_is_value_identical() {
    let out = run_ok(
        r#""use strict";
        function mk(n) { var a = []; for (var i = 0; i < n; i++) a.push({ w: 0, pad: i }); return a; }
        function writeLoop(objs, n, iters) {
          var k = 0;
          for (var i = 0; i < iters; i++) { objs[k].w = (i * 7) | 0; k++; if (k === n) k = 0; }
          var s = 0; for (var j = 0; j < n; j++) s = (s + objs[j].w) | 0; return s;
        }
        var t = 0, counts = [1, 2, 4, 8, 9, 16, 1024];
        for (var p = 0; p < counts.length; p++) t = (t + writeLoop(mk(counts[p]), counts[p], 40000)) | 0;
        console.log(t);
        "#,
    );
    assert_eq!(out, vec!["294244783"]);
}

/// An INHERITED read over the same receiver counts: the `IcEntry::chain` fill,
/// whose hop-version guards must stay exactly as sound when a fill is declined
/// as when it is taken.
#[test]
fn megamorphic_prototype_read_is_value_identical() {
    let out = run_ok(
        r#""use strict";
        function mk(n) {
          var proto = { inh: 11 }, a = [];
          for (var i = 0; i < n; i++) { var o = Object.create(proto); o.own = i; a.push(o); }
          return a;
        }
        function readLoop(objs, n, iters) {
          var k = 0, s = 0;
          for (var i = 0; i < iters; i++) { s = (s + objs[k].inh) | 0; k++; if (k === n) k = 0; }
          return s;
        }
        var t = 0, counts = [1, 2, 4, 8, 9, 16, 1024];
        for (var p = 0; p < counts.length; p++) t = (t + readLoop(mk(counts[p]), counts[p], 40000)) | 0;
        console.log(t);
        "#,
    );
    assert_eq!(out, vec!["3080000"]);
}

/// DICTIONARY receivers (a delete forces `shape::DICT`), which the shape memo
/// can never serve — every access reaches the own-data fill after a full key
/// scan, so this is the path the gate has to suppress without losing a value.
#[test]
fn megamorphic_dictionary_read_is_value_identical() {
    let out = run_ok(
        r#""use strict";
        function mk(n) {
          var a = [];
          for (var i = 0; i < n; i++) { var o = { drop: 1, dv: i + 5, pad: i }; delete o.drop; a.push(o); }
          return a;
        }
        function readLoop(objs, n, iters) {
          var k = 0, s = 0;
          for (var i = 0; i < iters; i++) { s = (s + objs[k].dv) | 0; k++; if (k === n) k = 0; }
          return s;
        }
        var t = 0, counts = [1, 2, 4, 8, 9, 16, 1024];
        for (var p = 0; p < counts.length; p++) t = (t + readLoop(mk(counts[p]), counts[p], 40000)) | 0;
        console.log(t);
        "#,
    );
    assert_eq!(out, vec!["22509270"]);
}

/// Mid-loop mutation at a thrashing site: a delete-then-re-add (to DICT), a
/// freeze and a `setPrototypeOf` + delete, each on one receiver of twelve. A
/// declined fill leaves whatever way is already resident in place, so this is
/// the case that would surface a way outliving the state it guards.
#[test]
fn mutation_mid_loop_at_a_thrashing_site() {
    let out = run_ok(
        r#""use strict";
        var objs = [];
        for (var i = 0; i < 12; i++) objs.push({ a: i, b: i * 2 });
        function loop(objs, n, iters) {
          var k = 0, s = 0;
          for (var i = 0; i < iters; i++) {
            s = (s + objs[k].a) | 0;
            if (i === 30000) { delete objs[3].a; objs[3].a = 999; }
            if (i === 40000) { Object.freeze(objs[5]); }
            if (i === 50000) { Object.setPrototypeOf(objs[7], { a: 4242 }); delete objs[7].a; }
            k++; if (k === n) k = 0;
          }
          return s;
        }
        console.log(loop(objs, 12, 80000));
        "#,
    );
    assert_eq!(out, vec!["15177816"]);
}

/// The scan-root relocation. `jit_set_prop_miss` registers the receiver as a
/// persistent minor-trace root because a filled way's HITS store with no call
/// and no barrier; that root now lives inside the fill branch, so a suppressed
/// fill registers nothing. Stores HEAP values through megamorphic sites, forces
/// minor collections between phases (38 on the release build), and reads every
/// stored value back afterwards — an under-rooted receiver loses one here.
#[test]
fn heap_stores_survive_minors_at_a_thrashing_site() {
    let out = run_ok(
        r#""use strict";
        function mk(n) { var a = []; for (var i = 0; i < n; i++) a.push({ w: null, pad: i }); return a; }
        function pool(m) { var a = []; for (var i = 0; i < m; i++) a.push({ v: i, junk: [i, i + 1] }); return a; }
        function writeLoop(objs, n, p, iters) {
          var k = 0;
          for (var i = 0; i < iters; i++) { objs[k].w = p[i & 63]; k++; if (k === n) k = 0; }
        }
        function churn(m) { var s = 0; for (var i = 0; i < m; i++) { var o = { g: i, h: [i] }; s = (s + o.h[0]) | 0; } return s; }
        var t = 0, counts = [1, 2, 4, 8, 9, 16, 1024];
        var live = [];
        for (var p = 0; p < counts.length; p++) {
          var objs = mk(counts[p]);
          writeLoop(objs, counts[p], pool(64), 60000);
          t = (t + churn(40000)) | 0;
          for (var j = 0; j < counts[p]; j++) t = (t + objs[j].w.v + objs[j].w.junk[1]) | 0;
          live.push(objs);
        }
        t = (t + churn(60000)) | 0;
        for (var p2 = 0; p2 < live.length; p2++) {
          var o2 = live[p2];
          for (var j2 = 0; j2 < o2.length; j2++) t = (t + o2[j2].w.v + o2[j2].w.junk[1]) | 0;
        }
        console.log(t);
        "#,
    );
    assert_eq!(out, vec!["-1189969244"]);
}

/// ACCESSOR receivers are deliberately NOT gated — those three fills answer to
/// `Jit::acc_way_gate` and its `SELF_CALL_DEOPT` eviction protocol instead, and
/// a second gate over them would interact with it. Megamorphic accessor reads
/// therefore have to keep working unchanged.
#[test]
fn megamorphic_accessor_read_is_untouched() {
    let out = run_ok(
        r#""use strict";
        function mk(n) {
          var a = [];
          for (var i = 0; i < n; i++) {
            var o = { base: i };
            Object.defineProperty(o, "acc", { get: function () { return this.base + 1; }, configurable: true });
            a.push(o);
          }
          return a;
        }
        function readLoop(objs, n, iters) {
          var k = 0, s = 0;
          for (var i = 0; i < iters; i++) { s = (s + objs[k].acc) | 0; k++; if (k === n) k = 0; }
          return s;
        }
        var t = 0, counts = [1, 8, 9, 16];
        for (var p = 0; p < counts.length; p++) t = (t + readLoop(mk(counts[p]), counts[p], 40000)) | 0;
        console.log(t);
        "#,
    );
    assert_eq!(out, vec!["759990"]);
}

// ── mechanism pin ───────────────────────────────────────────────────────────
// The parity tests above pass whether or not the gate does anything, so they
// cannot notice it silently ceasing to engage. This pin counts `SetProp`
// misses through `ZIPP_ICSTATS` instead. Both the counter latch and the gate
// latch are per-process, so the two sides are two child processes of this test
// binary running one `#[ignore]`d probe.

/// Phases of 1, 2, 4, 8 then 9 receivers through ONE store site. The first four
/// leave all eight ways occupied by dead receivers with the cursor at 7, so the
/// 9-receiver phase trips the gate on its very first eviction — the exact state
/// in which a freeze without the rotation escape stays at ~100% miss forever.
const PIN_JS: &str = r#""use strict";
var ITERS = 120000;
function mk(n, tag) {
  var a = [];
  for (var i = 0; i < n; i++) a.push({ w: 0, tag: tag + i });
  return a;
}
function writeLoop(objs, n) {
  var k = 0;
  for (var i = 0; i < ITERS; i++) {
    objs[k].w = i;
    k++; if (k === n) k = 0;
  }
  var s = 0;
  for (var j = 0; j < n; j++) s = (s + objs[j].w) | 0;
  return s;
}
var t = 0;
var counts = [1, 2, 4, 8, 9];
for (var p = 0; p < counts.length; p++) t = (t + writeLoop(mk(counts[p], p * 100), counts[p])) | 0;
console.log(t);
"#;

/// Accesses in `PIN_JS`'s measured (9-receiver) phase.
const PIN_ITERS: u64 = 120_000;

const PIN_MARKER: &str = "ZIPP_ICGATE_PIN";

/// The measured half, run in its own process so it sees one setting of the
/// gate and a private `ZIPP_ICSTATS` counter. Ignored: only the re-exec below
/// ever names it.
#[test]
#[ignore]
fn icgate_pin_child() {
    let out = zipp_vm::run(PIN_JS).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    let set_miss = zipp_vm::ic_stats().4;
    println!(
        "{PIN_MARKER} out={} set_miss={set_miss}",
        out.output.join("|")
    );
}

/// `(stdout of the script, SetProp misses)` from one child run.
fn pin_child(gate_off: bool) -> (String, u64) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "--exact",
        "icgate_pin_child",
        "--ignored",
        "--nocapture",
        "--test-threads",
        "1",
    ]);
    cmd.env("ZIPP_ICSTATS", "1");
    cmd.env("ZIPP_ICGATE_PIN_CHILD", "1");
    // Isolate the older refill/rotation mechanism. Adaptive direct-miss sites
    // deliberately call the Rust helper on every access, so their ICSTATS
    // "miss" count cannot measure how many identity ways would have hit.
    cmd.env("ZIPP_NO_DIRECT_IC_MISS", "1");
    // This is an IC mechanism pin: keep the field-write stream reducer from
    // consuming the measured loop before its SetProp site reaches the IC.
    cmd.env("ZIPP_NO_FIELD_WRITE_STREAM", "1");
    if gate_off {
        cmd.env("ZIPP_NO_ICGATE", "1");
    } else {
        cmd.env_remove("ZIPP_NO_ICGATE");
    }
    let out = cmd.output().expect("re-exec the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    // `--nocapture` prints the probe's line after the harness's own
    // `test icgate_pin_child ... ` prefix, on the same line.
    let line = stdout
        .lines()
        .find_map(|l| l.find(PIN_MARKER).map(|i| &l[i..]))
        .unwrap_or_else(|| panic!("child produced no {PIN_MARKER} line; stdout:\n{stdout}"));
    let mut script_out = String::new();
    let mut misses = None;
    for field in line.split_whitespace().skip(1) {
        if let Some(v) = field.strip_prefix("out=") {
            script_out = v.to_string();
        } else if let Some(v) = field.strip_prefix("set_miss=") {
            misses = Some(v.parse().expect("miss count parses"));
        }
    }
    (script_out, misses.expect("child reported a miss count"))
}

/// The gate must actually gate, and the rotation escape must actually escape.
///
/// With the gate off the 9-receiver phase misses on essentially every access.
/// With it on the site converges to the `1/9` an 8-way cache can hold, and the
/// overshoot is bounded by the escape period: a site that froze without the
/// escape would stay near the off number, and a cursor widened from `u8` to
/// `u16` would spend 65,536 misses per window instead of 256 and land near
/// 70,000 here. `IC_ROT_PERIOD`'s own `const` assertion pins the arithmetic;
/// this pins the behaviour it buys.
#[test]
fn gate_and_rotation_escape_are_engaged() {
    if std::env::var_os("ZIPP_ICGATE_PIN_CHILD").is_some() {
        return; // a child process re-running the whole binary would recurse
    }
    let (off_out, off) = pin_child(true);
    let (on_out, on) = pin_child(false);

    assert_eq!(off_out, on_out, "the gate changed a value, not a timing");
    assert_eq!(off_out, "2879905", "expectation drifted from node");

    // Ungated: the 9-receiver phase evicts the way it is about to need, so
    // every access misses.
    assert!(
        off > PIN_ITERS * 9 / 10,
        "ZIPP_NO_ICGATE=1 should reproduce the unconditional refill \
         (expected ~{PIN_ITERS} SetProp misses, got {off})"
    );
    // Gated, with the escape: the floor for eight ways over nine receivers.
    assert!(
        on < PIN_ITERS / 3,
        "the gate is not engaging: {on} SetProp misses against {off} ungated \
         (the (n-8)/n floor here is ~{})",
        PIN_ITERS / 9
    );
    assert!(
        on * 4 < off,
        "gated {on} vs ungated {off} is not a mechanism"
    );
}
