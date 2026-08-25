//! Splice-aware INT admission (`ZIPP_NO_INT_SPLICE=1`).
//!
//! `compile_region_int` used to be handed the raw bytecode, so a `Call` the
//! leaf planner had ALREADY proved inlinable (`INLINE-ELIGIBLE`, a slot-
//! generation guard, a splice-lite mask) still hit the catch-all reject in
//! `int_unadmitted_ips` and demoted the whole region to the memory tier — the
//! parse-large-js mix loop among them, at ~0.9ns/op instead of ~0.1ns/op.
//!
//! The region is now FLATTENED before admission runs: each proven-splice
//! callee's body is substituted for its `Call` over a per-site scratch window,
//! the callee's own `LoadGlobal` is dropped, and the identity guard is hoisted
//! out of the loop to one `global_gens[g]` compare at region entry. The deopt
//! resume map sends every exit inside a `[callee-load, call]` span back to the
//! callee load, so the interpreter replays the whole call rather than resuming
//! at an ip that no longer exists.
//!
//! Every `intsplice_parity_*` case asserts byte-identical output against
//! `node -e` — the tier is the thing under test, so an intra-engine comparison
//! would not catch a shared miscompile. `intsplice_all_modes_answer_identically`
//! re-runs them under the off-switch and the neighbouring tier switches.
//!
//! The `intsplice_mechanism_*` cases read the plan back out of a child's
//! `ZIPP_JITLOG`: a parity case that quietly stopped reaching the integer tier
//! would be testing nothing at all, and every DECLINE path has to be a decline
//! (falling back), never a miscompile.

const PRELUDE: &str = r#""use strict";
var N = 20000;
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The oracle. Fed on STDIN rather than `-e`: node evaluates both in global
/// SCRIPT scope (so a `function` declaration is a global binding, which is what
/// zipp's own top level is), and the generated enumeration below is far past
/// the Windows command-line limit.
fn node_output(src: &str) -> Vec<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("node")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node v24 on PATH (expected values come from node)");
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(src.as_bytes())
        .expect("write to node");
    let out = child.wait_with_output().expect("node exits");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for:\n{src}");
}

fn prog(body: &str) -> String {
    format!("{PRELUDE}{body}")
}

/// THE target shape: the parse-large-js mix loop. Three pinned dense-Int arrays
/// and a pinned flat-ASCII string feed three calls to an 8-op leaf that stores
/// a global. Before the flatten this declined with three `Call` int-rejects.
#[test]
fn intsplice_parity_mix_loop() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) { kinds.push(i % 13); starts.push(i % 2000); ends.push((i % 2000) + 3); }
var h = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    var ch = src.charCodeAt(starts[ti]);
    mix(kinds[ti]); mix(ends[ti] - starts[ti]); mix(ch);
  }
}
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("mix h=" + h);
"#,
    ));
}

/// A callee that RETURNS a value, so the flatten has to route the body's result
/// register into the caller's `Call` dst (the `ReturnUndefined` shape drops that
/// write instead, which is only sound because the dst is proved dead).
#[test]
fn intsplice_parity_returning_callee() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var s = 0;
function step(x) { return Math.imul(x ^ 31, 16777619) >>> 0; }
function f(n) {
  for (var ti = 0; ti < n; ti++) { s = (s ^ step(a[ti])) | 0; }
}
for (var r = 0; r < 5; r++) { s = 0; f(N); }
console.log("ret s=" + s);
"#,
    ));
}

/// TWO distinct callees in one region — two independent entry guards, two
/// scratch windows, two stored globals.
#[test]
fn intsplice_parity_two_distinct_callees() {
    assert_matches_node(&prog(
        r#"var a = [], b = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); }
var h = 0, g = 0;
function mixh(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function mixg(x) { g = Math.imul(g + x, 40503) | 0; }
function f(n) { for (var ti = 0; ti < n; ti++) { mixh(a[ti]); mixg(b[ti]); } }
for (var r = 0; r < 5; r++) { h = 2166136261; g = 0; f(N); }
console.log("two h=" + h + " g=" + g);
"#,
    ));
}

/// The loop reads the global the spliced body writes, BETWEEN the calls: the
/// stored global lives in a home for the whole region, so a splice that lost
/// the write would still print a plausible-looking hash.
#[test]
fn intsplice_parity_stored_global_read_in_the_loop() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var h = 0, seen = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function f(n) {
  for (var ti = 0; ti < n; ti++) { mix(a[ti]); seen = (seen + (h & 15)) | 0; mix(h & 255); }
}
for (var r = 0; r < 5; r++) { h = 2166136261; seen = 0; f(N); }
console.log("stored h=" + h + " seen=" + seen);
"#,
    ));
}

// ── decline paths ── each of these must fall back to the memory tier and still
// answer correctly. A miscompile here is the failure mode that matters: the
// flatten declining is free, the flatten mis-firing is a wrong answer.

/// A closure callee reading a captured variable. An upvalue read is a
/// `jit_cell_get` FFI call, which the integer emitters have no arm for.
#[test]
fn intsplice_parity_callee_with_an_upvalue() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var h = 0;
var mk = function () { var k = 16777619; return function (x) { h = Math.imul(h ^ x, k) >>> 0; }; };
var mix = mk();
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti]); } }
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("upval h=" + h);
"#,
    ));
}

/// A `Div` in the callee body: fractional, so no i64 home can hold it and the
/// flattened body fails INT admission as a whole.
#[test]
fn intsplice_parity_callee_with_a_div() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push((i % 13) + 1);
var h = 0;
function mix(x) { h = (h + (x / 2)) | 0; }
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti]); } }
for (var r = 0; r < 5; r++) { h = 0; f(N); }
console.log("div h=" + h);
"#,
    ));
}

/// A sloppy unresolved assignment compiles as `StoreGlobalResolved`, whose
/// dynamic binding route is deliberately not licensed by the raw r12 proof.
/// The leaf and INT splice must both retain the real-call fallback.
#[test]
fn intsplice_parity_resolved_global_store_stays_on_the_real_call() {
    assert_matches_node(
        r#"var N = 20000;
var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var h = 0;
function step(x) { implicitRoute = x; return (x + 1) | 0; }
function f(n) {
  for (var ti = 0; ti < n; ti++) h = (h + step(a[ti])) | 0;
}
for (var r = 0; r < 5; r++) { h = 0; f(N); }
console.log("resolved h=" + h + " implicit=" + implicitRoute);
"#,
    );
}

/// Fewer arguments than formals: the missing parameter is `undefined`, which
/// the flatten cannot seed into an i64 home, so the site declines.
#[test]
fn intsplice_parity_argc_below_param_count() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var h = 0;
function mix(x, y) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti]); } }
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("argc h=" + h);
"#,
    ));
}

/// More arguments than formals — the extra one is evaluated and discarded.
#[test]
fn intsplice_parity_argc_above_param_count() {
    assert_matches_node(&prog(
        r#"var a = [], b = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); }
var h = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti], b[ti]); } }
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("argc2 h=" + h);
"#,
    ));
}

/// A callee reading `this`. Reg 0 is not a parameter, so the splice-lite
/// read-before-write mask is non-zero and the flatten declines rather than
/// giving `undefined` an i64 home.
#[test]
fn intsplice_parity_callee_reading_this() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var h = 0;
function mix(x) { h = Math.imul(h ^ (this === undefined ? x : x + 1), 16777619) >>> 0; }
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti]); } }
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("this h=" + h);
"#,
    ));
}

/// The callee's global slot is one a bytecode store can reach, so no
/// slot-generation guard can be keyed for it and there is nothing to hoist.
/// Reassigning it mid-run must simply run the new function.
#[test]
fn intsplice_parity_callee_slot_is_bytecode_stored() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var h = 0;
function mixA(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function mixB(x) { h = Math.imul(h + x, 40503) >>> 0; }
var mix = mixA;
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti]); } }
var out = [];
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
mix = mixB;
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
console.log("rebound " + out.join(","));
"#,
    ));
}

/// THE ENTRY GUARD. `globalThis.mix = other` is a NON-bytecode write to the
/// callee's slot: it bumps the slot generation, which is exactly what the baked
/// entry compare exists to catch. A guard that did not fire would keep running
/// the flattened body of the OLD function for the rest of the run.
#[test]
fn intsplice_parity_callee_rebound_through_the_global_object() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var h = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function other(x) { h = Math.imul(h + x, 40503) >>> 0; }
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti]); } }
var out = [];
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
globalThis.mix = other;
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
globalThis.mix = mix;
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
console.log("gtrebound " + out.join(","));
"#,
    ));
}

// ── exits out of a flattened body ──

/// A double and a string planted in the pinned arrays: the element tag guard
/// deopts inside the `[callee-load, call]` span, so the interpreter replays the
/// span — the callee load included — instead of resuming at an ip the flattened
/// body no longer has.
#[test]
fn intsplice_parity_deopt_on_a_non_int_element() {
    assert_matches_node(&prog(
        r#"var a = [], b = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); }
a[N - 7] = 2.5;
b[N - 11] = "x";
var h = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti]); mix(b[ti]); } }
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("deopt h=" + h);
"#,
    ));
}

/// The index running off the end of the second array: the bounds guard deopts
/// every iteration near the tail, once per spliced site.
#[test]
fn intsplice_parity_index_out_of_range() {
    assert_matches_node(&prog(
        r#"var a = [], b = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); }
var h = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function f(n) { for (var ti = 0; ti < n; ti++) { mix(a[ti]); mix(b[ti + 5]); } }
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("oob h=" + h);
"#,
    ));
}

/// An i53 range guard INSIDE the spliced body: the accumulator is seeded past
/// 2^53 for the last pass, so the callee's own `+` leaves the region. That exit
/// resumes at the callee load and re-runs the whole call, which is only sound
/// because no effect can precede it.
#[test]
fn intsplice_parity_i53_overflow_inside_the_body() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var acc = 0;
function bump(x) { acc = acc + x * 1048576 + 1048576; }
function f(n) { for (var ti = 0; ti < n; ti++) { bump(a[ti]); } }
var out = [];
for (var r = 0; r < 5; r++) { acc = 0; f(N); out.push(acc); }
acc = 9007199254000000;
f(N);
out.push(acc);
console.log("i53 " + out.join(","));
"#,
    ));
}

/// The pinned array rebound between OSR entries — shorter, then carrying a
/// double, then not an array at all. The pin guard, not the splice, must catch
/// each one; the splice only has to leave the region's state flushable.
#[test]
fn intsplice_parity_array_rebound_between_entries() {
    assert_matches_node(&prog(
        r#"var a = [];
for (var i = 0; i < N; i++) a.push(i % 13);
var h = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
function f(n) { for (var ti = 0; ti < n && ti < a.length; ti++) { mix(a[ti]); } }
var out = [];
for (var r = 0; r < 3; r++) { h = 2166136261; f(N); out.push(h); }
a = [1, 2, 3, 4, 5, 6, 7, 8];
for (var r = 0; r < 3; r++) { h = 2166136261; f(N); out.push(h); }
a = [1, 2, 2.5, 4];
for (var r = 0; r < 3; r++) { h = 2166136261; f(N); out.push(h); }
console.log("rebind " + out.join(","));
"#,
    ));
}

/// ── generated shapes ──
///
/// The hand-picked cases above are the ones somebody thought of. This one is
/// the other half of the lesson `jit_tier_parity.rs` records: which shapes
/// FLATTEN, and what register allocation the flattened body then gets, are
/// functions of incidental properties — how many arguments the site passes,
/// whether the callee stores a global or returns, how many pinned receivers the
/// caller recycles, which element of which array is not an Int. This enumerates
/// them instead.
///
/// It is a `parity_` case on purpose: the mode sweep below then re-runs the
/// whole enumeration under the splice and typed-lane off-switches,
/// `ZIPP_NO_MULTI_SPLIT=1`, `ZIPP_JIT_THRESHOLD=1`, GC stress and the
/// interpreter.
///
/// Every reported value passes through `| 0`, so no answer can depend on
/// Number→String formatting and `node -e` is an exact oracle.
fn generated_kernels(count: u32) -> String {
    // A fixed-seed LCG: the enumeration must be the same program on every run,
    // or a failure is not reproducible.
    let mut state: u64 = 0x2F6B_1D07_C41A_9E5B;
    let mut next = |n: u64| -> u64 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % n
    };
    let mut src = String::from("var OUT = [];\n");
    for k in 0..count {
        let nargs = 1 + next(2); // 1 or 2 formals, always matched by the site
        let calls = 1 + next(3); // 1..3 spliced sites in one region
        let body_kind = next(6);
        let returns = next(3) == 0; // `return v` vs a stored global
        let poison = next(4); // 0 = none, 1 = double, 2 = string, 3 = out of range
        let use_str = next(2) == 0;
        let x = "x";
        let y = if nargs == 2 { "y" } else { "0" };
        let expr = match body_kind {
            0 => format!("Math.imul(h{k} ^ {x}, 16777619) >>> 0"),
            1 => format!("(h{k} + {x} + {y}) | 0"),
            2 => format!("Math.imul(h{k} + {x}, 40503) ^ ({y} << 3)"),
            3 => format!("((h{k} << 5) - h{k} + {x} + {y}) | 0"),
            4 => format!("(h{k} ^ ({x} >>> 1) ^ ({y} & 255)) | 0"),
            _ => format!("Math.imul(h{k} ^ ({x} - {y}), 2654435761 | 0) | 0"),
        };
        let formals = if nargs == 2 { "x, y" } else { "x" };
        let body = if returns {
            format!("function cal{k}({formals}) {{ return {expr}; }}")
        } else {
            format!("function cal{k}({formals}) {{ h{k} = {expr}; }}")
        };
        let arg = |i: u64| -> String {
            match (i + body_kind) % 4 {
                0 => format!("a{k}[t]"),
                1 => format!("(b{k}[t] - a{k}[t]) | 0"),
                2 if use_str => format!("s{k}.charCodeAt(a{k}[t]) | 0"),
                2 => format!("(a{k}[t] * 3) | 0"),
                _ => format!("(t ^ {}) | 0", 7 + i),
            }
        };
        let mut site = String::new();
        for c in 0..calls {
            let args = if nargs == 2 {
                format!("{}, {}", arg(c), arg(c + 1))
            } else {
                arg(c)
            };
            if returns {
                site.push_str(&format!("h{k} = (h{k} ^ cal{k}({args})) | 0; "));
            } else {
                site.push_str(&format!("cal{k}({args}); "));
            }
        }
        src.push_str(&format!(
            r#"var a{k} = [], b{k} = [];
for (var q = 0; q < 400; q++) {{ a{k}.push(q % 29); b{k}.push(q % 61); }}
var s{k} = "";
for (var q = 0; q < 8; q++) s{k} += "abcdefghijklmnopqrstuvwxyz0123456789 ";
var h{k} = 0;
{body}
function ker{k}(n) {{ for (var t = 0; t < n; t++) {{ {site}}} }}
"#
        ));
        match poison {
            1 => src.push_str(&format!("a{k}[371] = 2.5;\n")),
            2 => src.push_str(&format!("b{k}[233] = \"x\";\n")),
            3 => src.push_str(&format!("a{k}.length = 390;\n")),
            _ => {}
        }
        src.push_str(&format!(
            "for (var r = 0; r < 4; r++) {{ h{k} = 2166136261; ker{k}(400); }}\n\
             OUT.push(\"{k}=\" + (h{k} | 0));\n"
        ));
    }
    src.push_str("console.log(OUT.join(\" \"));\n");
    src
}

/// 96 generated kernels in one program (prefix-isolated, so `node -e` runs the
/// whole enumeration in a single process).
#[test]
fn intsplice_parity_generated_kernels() {
    assert_matches_node(&prog(&generated_kernels(96)));
}

/// The enumeration is worth running only while it still reaches the mechanism:
/// a generator change that stopped producing flattenable sites would leave the
/// parity case green and testing nothing.
#[test]
fn intsplice_mechanism_generated_kernels_reach_the_flatten() {
    let log = jitlog_of("intsplice_parity_generated_kernels", &[]);
    let flattened = log.lines().filter(|l| l.contains("INT splice [")).count();
    assert!(
        flattened >= 8,
        "only {flattened} of the generated kernels flattened — the enumeration \
         has drifted off the mechanism:\n{}",
        log.lines()
            .filter(|l| l.starts_with("[int-splice]"))
            .take(20)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every case above must answer identically in every mode.
#[test]
fn intsplice_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 7] = [
        &[("ZIPP_NO_INT_SPLICE", "1")],
        &[("ZIPP_NO_TYPED_SPLICE", "1")],
        &[("ZIPP_NO_MULTI_SPLIT", "1")],
        &[("ZIPP_NO_INT_SPLICE", "1"), ("ZIPP_NO_MULTI_SPLIT", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("intsplice_parity_");
        for (key, val) in mode {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the intsplice_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}

fn jitlog_of(test_name: &str, env: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(test_name)
        .arg("--exact")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDECLINE", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{test_name} child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

/// The mechanism: the mix loop compiles on the INTEGER tier, and with the
/// switch off the SAME loop is rejected on its `Call`s and lands on MEM. Those
/// `[int-reject] … Call` lines are the exact decline the map named; if they
/// stop appearing with the switch off, this stopped being an off-switch.
#[test]
fn intsplice_mechanism_the_mix_loop_reaches_the_int_tier() {
    for name in [
        "intsplice_parity_mix_loop",
        "intsplice_parity_returning_callee",
        "intsplice_parity_two_distinct_callees",
        "intsplice_parity_stored_global_read_in_the_loop",
    ] {
        let on = jitlog_of(name, &[]);
        assert!(
            on.contains("INT splice ["),
            "{name}: no region was flattened:\n{on}"
        );
        assert!(
            on.contains("INT region fn"),
            "{name}: the kernel is not on the integer tier:\n{on}"
        );

        let off = jitlog_of(name, &[("ZIPP_NO_INT_SPLICE", "1")]);
        assert!(
            !off.contains("INT splice ["),
            "{name}: ZIPP_NO_INT_SPLICE=1 still flattened a region:\n{off}"
        );
        assert!(
            off.lines()
                .any(|l| l.starts_with("[int-reject]") && l.contains("Call {")),
            "{name}: with the switch off the region should be rejected on its \
             Call:\n{off}"
        );
        assert!(
            off.contains("MEM region fn"),
            "{name}: with the switch off the kernel must fall to MEM:\n{off}"
        );
    }
}

/// Two callees in one region bake TWO entry guards. One guard for two sites
/// would be a plausible-looking bug that only shows up when one of the two is
/// rebound, which no parity case can be relied on to hit.
#[test]
fn intsplice_mechanism_one_entry_guard_per_callee() {
    let log = jitlog_of("intsplice_parity_two_distinct_callees", &[]);
    assert!(
        log.contains("2 call(s) flattened, 2 entry guard(s)"),
        "expected two sites and two guards:\n{log}"
    );
    let one = jitlog_of("intsplice_parity_mix_loop", &[]);
    assert!(
        one.contains("3 call(s) flattened, 1 entry guard(s)"),
        "the mix loop's three sites share ONE callee and so one guard:\n{one}"
    );
}

/// Every decline path names itself and falls back — the parity cases prove the
/// answer, this proves they are testing the path they claim to.
#[test]
fn intsplice_mechanism_declines_are_named_and_fall_back() {
    for (name, reason) in [
        (
            "intsplice_parity_callee_with_an_upvalue",
            "callee reads upvalues",
        ),
        ("intsplice_parity_callee_with_a_div", "is not flattenable"),
        (
            "intsplice_parity_argc_below_param_count",
            "argc 1 != param_count 2",
        ),
        (
            "intsplice_parity_callee_slot_is_bytecode_stored",
            "no slot_guard",
        ),
    ] {
        let log = jitlog_of(name, &[]);
        assert!(
            log.lines()
                .any(|l| l.starts_with("[int-splice]") && l.contains(reason)),
            "{name}: expected the decline `{reason}`:\n{log}"
        );
        assert!(
            log.contains("MEM region fn"),
            "{name}: a declined flatten must leave the region on MEM:\n{log}"
        );
    }

    let resolved = jitlog_of(
        "intsplice_parity_resolved_global_store_stays_on_the_real_call",
        &[],
    );
    assert!(
        resolved.contains("DECLINE (not leaf-eligible)")
            && resolved.contains("no leaf plan (not monomorphic / not inline-eligible)"),
        "StoreGlobalResolved must be rejected before a raw leaf plan exists:\n{resolved}"
    );
    assert!(
        resolved.contains("MEM region fn"),
        "the resolved-store control must retain the real-call MEM path:\n{resolved}"
    );
}

/// The entry guard actually fires: the region compiles INT for the first phase
/// and the rebinding through the global object is caught. `node -e` agrees with
/// the answer (the parity case), so all this has to pin is that the flattened
/// tier was reached at all — otherwise the guard would be untested.
#[test]
fn intsplice_mechanism_rebinding_is_caught_by_the_entry_guard() {
    let log = jitlog_of(
        "intsplice_parity_callee_rebound_through_the_global_object",
        &[],
    );
    assert!(
        log.contains("INT splice [") && log.contains("INT region fn"),
        "the rebinding case never reached the flattened tier, so its guard was \
         never exercised:\n{log}"
    );
}
