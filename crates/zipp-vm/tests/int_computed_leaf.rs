//! Bounded dense computed-call splicing on the integer tier.
//!
//! The fast path is deliberately narrow: a pristine dense Array with at most
//! four exact, capture-free, pure integer leaf functions.  Entry guards freeze
//! the receiver and every element; all other keys, mutations, accessors,
//! closures, and meter modes return to the ordinary call path before effects.

const PRELUDE: &str = r#""use strict";
var N = 6000;
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

fn node_output(src: &str) -> Vec<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("node")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node on PATH");
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(src.as_bytes())
        .expect("write source to node");
    let out = child.wait_with_output().expect("node exits");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_matches_node(body: &str) {
    let src = format!("{PRELUDE}{body}");
    assert_eq!(run_ok(&src), node_output(&src), "zipp != node for:\n{src}");
}

/// Runtime dispatch covers all admitted integer keys; every non-canonical or
/// uncovered key must replay the original computed call, including its throw.
/// After the native region is warm, receiver/element/accessor/hole mutations
/// exercise every entry-guard decline without allowing a duplicated call.
#[test]
fn computed_parity_keys_mutations_and_exceptions() {
    assert_matches_node(
        r#"
        function a(x) { return (Math.imul(x, 3) + 1) | 0; }
        function b(x) { return ((x ^ 0x5a5a) + 7) | 0; }
        function c(x) { return (Math.imul(x + 11, 17) ^ 31) | 0; }
        function alt(x) { return (Math.imul(x, 5) - 9) | 0; }
        var ops = [a, b, c];
        function hot(key, n, seed) {
          let s = seed | 0;
          for (let i = 0; i < n; i++) s = ops[key](s);
          return s;
        }
        let out = [];
        out.push(hot(0, N, 1));
        out.push(hot(1, 211, 2), hot(2, 213, 3), hot("0", 215, 4));
        for (const key of [-1, 3, 1.5, "-0", "1.5"]) {
          try { hot(key, 1, 5); out.push("miss"); }
          catch (e) { out.push(e instanceof TypeError); }
        }

        ops[1] = alt;
        out.push(hot(1, 217, 6));
        let throws = 0;
        ops[1] = function boom(x) { throws++; throw new Error("boom" + x); };
        try { hot(1, 9, 7); } catch (e) { out.push(e.message.slice(0, 4), throws); }
        ops[1] = 17;
        try { hot(1, 1, 8); } catch (e) { out.push(e instanceof TypeError); }

        let gets = 0;
        Object.defineProperty(ops, "1", {
          configurable: true,
          get() { gets++; return alt; }
        });
        out.push(hot(1, 31, 9), gets);
        delete ops[1];
        Array.prototype[1] = alt;
        out.push(hot(1, 29, 10));
        delete Array.prototype[1];

        ops = [a, alt, c];
        out.push(hot(1, 223, 11));
        console.log(out.join("/"));
        "#,
    );
}

/// The shared-home/spill retry must preserve values whose ranges overlap the
/// computed dispatch and remain observable after the loop.  This shape is
/// intentionally hostile to assigning every synthetic arm a distinct home.
#[test]
fn computed_parity_conflicting_live_outs() {
    assert_matches_node(
        r#"
        function a(x) { return (Math.imul(x, 3) + 1) | 0; }
        function b(x) { return ((x ^ 0x55aa) + 9) | 0; }
        function c(x) { return (Math.imul(x + 7, 13) ^ 19) | 0; }
        const ops = [a, b, c];
        function pressure(n) {
          let s=1,a0=2,a1=3,a2=5,a3=7,a4=11,a5=13,a6=17,a7=19,a8=23,a9=29;
          for (let i=0;i<n;i++) {
            s = ops[i % 3](s);
            a0=(a0+i)|0; a1=(a1^s)|0; a2=(a2+a0)|0; a3=(a3+a1)|0;
            a4=(a4^a2)|0; a5=(a5+a3)|0; a6=(a6^a4)|0; a7=(a7+a5)|0;
            a8=(a8^a6)|0; a9=(a9+a7)|0;
          }
          return [s,a0,a1,a2,a3,a4,a5,a6,a7,a8,a9].join(":");
        }
        console.log(pressure(N));
        "#,
    );
}

/// A receiver definition followed by an admitted `Array.prototype.push` used
/// to make the computed-call fallback prefix observably effectful.  A string
/// index deliberately misses the bounded integer dispatch while still naming
/// the same function through ordinary Array property semantics.  The planner
/// must decline the splice; otherwise every native miss pushes once and the
/// interpreter replay pushes the same element a second time.
#[test]
fn computed_parity_effectful_prefix_is_not_replayed() {
    assert_matches_node(
        r#"
        function a(x) { return (Math.imul(x, 3) + 1) | 0; }
        function b(x) { return ((x ^ 0x55aa) + 9) | 0; }
        const ops = [a, b];
        function hot(key, n) {
          const sink = [];
          let s = 1;
          for (let i = 0; i < n; i++) {
            const chosen = ops;
            sink.push(i);
            s = chosen[key](s);
          }
          return sink.length + ":" + s;
        }
        hot(0, N);
        console.log(hot("0", 257));
        "#,
    );
}

/// Regression for the dropped-receiver liveness proof: `chosen` is read after
/// the computed call but before the next definition.  The planner must decline
/// rather than erasing its object-valued Move from the success path.
#[test]
fn computed_post_call_receiver_probe() {
    assert_matches_node(
        r#"
        function a(x) { return (x + 3) | 0; }
        function b(x) { return (x ^ 17) | 0; }
        const ops = [a,b];
        function hot(table,n) {
          let s=0;
          for(let i=0;i<n;i++) {
            let chosen=table;
            s=chosen[i&1](s);
            s=(s+chosen.length)|0;
          }
          return s;
        }
        console.log(hot(ops,N));
        "#,
    );
}

/// Small child target whose log must show the complete path: dense plan,
/// computed flatten, symbolic-home retry, GPR mapping, and native install.
#[test]
fn computed_mechanism_probe() {
    assert_matches_node(
        r#"
        (function main(){
          const ops=[
            x => (Math.imul(x,3)+1)|0,
            x => ((x^91)+7)|0,
            x => (Math.imul(x+5,17)^31)|0
          ];
          let s=1;
          for(let i=0;i<N;i++)s=ops[i%3](s);
          console.log(s);
        })();
        "#,
    );
}

/// Dedicated bounded-pressure target: the loop carries enough overlapping
/// live-outs to overflow the physical-home plan while the computed-only
/// symbolic retry remains within its documented spill budget.
#[test]
fn computed_parity_virtual_retry_probe() {
    assert_matches_node(
        r#"
        function a(x) { return (Math.imul(x, 3) + 1) | 0; }
        function b(x) { return ((x ^ 0x55aa) + 9) | 0; }
        function c(x) { return (Math.imul(x + 7, 13) ^ 19) | 0; }
        const ops = [a, b, c];
        function pressure(n) {
          let s=1,a0=2,a1=3,a2=5,a3=7,a4=11,a5=13;
          for (let i=0;i<n;i++) {
            s = ops[i % 3](s);
            a0=(a0+i)|0; a1=(a1^s)|0; a2=(a2+a0)|0;
            a3=(a3+a1)|0; a4=(a4^a2)|0; a5=(a5+a3)|0;
          }
          return [s,a0,a1,a2,a3,a4,a5].join(":");
        }
        console.log(pressure(N));
        "#,
    );
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child_log(test: &str, envs: &[(&str, &str)]) -> (String, String) {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args([test, "--exact", "--nocapture"])
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDECLINE", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_INT_COMPUTED_LEAF")
        .env_remove("ZIPP_NO_GPR_SPILL_SLOTS");
    for &(key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn test child");
    assert!(
        out.status.success(),
        "child {test} {envs:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn computed_mechanism_engages_and_off_switch_restores_fallback() {
    let (_, on) = child_log("computed_mechanism_probe", &[]);
    for needle in [
        "[int-computed]",
        "ELIGIBLE arms=3",
        "INT computed splice",
        "GPR homes engaged",
        "INT region fn",
    ] {
        assert!(on.contains(needle), "missing {needle:?} in:\n{on}");
    }
    assert!(
        !on.contains("deopt at ip"),
        "computed scratch entry guards chronically deopted:\n{on}"
    );

    let (_, virtual_log) = child_log(
        "computed_parity_virtual_retry_probe",
        &[("ZIPP_GPR_SPILL_SLOTS", "1")],
    );
    for needle in [
        "INT computed splice",
        "INT-GPR computed retry",
        "GPR homes engaged",
        "spilled",
        "INT region fn",
    ] {
        assert!(
            virtual_log.contains(needle),
            "wide retry missing {needle:?}:\n{virtual_log}"
        );
    }
    let symbolic_homes = virtual_log
        .lines()
        .find_map(|line| line.split_once("glob-range plan:").map(|(_, tail)| tail))
        .and_then(|tail| tail.rsplit_once("homes=").map(|(_, homes)| homes))
        .and_then(|homes| homes.parse::<usize>().ok())
        .expect("computed retry must report its symbolic-home count");
    assert!(
        (15..=18).contains(&symbolic_homes),
        "computed retry did not exceed the physical cap while staying bounded: {symbolic_homes}\n{virtual_log}"
    );
    assert!(
        !virtual_log.contains("deopt at ip"),
        "wide computed region deopted during its stable run:\n{virtual_log}"
    );

    let (_, off) = child_log(
        "computed_mechanism_probe",
        &[("ZIPP_NO_INT_COMPUTED_LEAF", "1")],
    );
    assert!(
        !off.contains("INT computed splice") && !off.contains("INT-GPR computed retry"),
        "computed off-switch still emitted the lane:\n{off}"
    );
    assert!(
        off.contains("GetIndex {")
            && off.contains("CallWithThis {")
            && off.contains("[jit] MEM region"),
        "off-switch did not restore the ordinary computed-call fallback:\n{off}"
    );
}

#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn computed_post_call_receiver_use_is_a_planning_decline() {
    let (_, log) = child_log("computed_post_call_receiver_probe", &[]);
    assert!(
        (log.contains("receiver temp is observable")
            || log.contains("computed flattened body is not INT-admissible")
            || log.contains("GetIndex/SetIndex (element not a pinned TypedArray)"))
            && log.contains("[jit] INT decline")
            && log.contains("[jit] MEM region"),
        "post-call receiver program did not take a fail-closed planning path:\n{log}"
    );
    assert!(
        !log.contains("INT computed splice"),
        "post-call receiver read reached the computed splice:\n{log}"
    );
}

#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn computed_effectful_prefix_is_a_planning_decline() {
    let (_, log) = child_log("computed_parity_effectful_prefix_is_not_replayed", &[]);
    assert!(
        (log.contains("non-replayable op")
            || log.contains("computed flattened body is not INT-admissible")
            || log.contains("CallWithThis (not a captured pinned string/DataView)"))
            && log.contains("[jit] INT decline")
            && log.contains("[jit] MEM region"),
        "effectful prefix did not take a fail-closed planning path:\n{log}"
    );
    assert!(
        !log.contains("INT computed splice"),
        "effectful prefix reached the computed splice:\n{log}"
    );
}

/// Fresh children isolate every memoized switch.  These are Node-differential
/// parity cases, so each mode checks semantics rather than only self-agreement.
#[test]
fn computed_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("off", "ZIPP_NO_INT_COMPUTED_LEAF", "1"),
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("gc", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
        ("no-spill", "ZIPP_NO_GPR_SPILL_SLOTS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("computed_parity_")
            .arg("--nocapture")
            .env(mode.1, mode.2)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{} failed:\n{stdout}\n{}",
            mode.0,
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "computed parity filter matched nothing in {} mode",
            mode.0
        );
    }
}

#[cfg(feature = "instrument")]
#[test]
fn computed_meter_matches_interpreter_exactly() {
    use zipp_vm::embed;
    const BIG: u64 = 1_000_000_000;
    const BOOT: &str = "void globalThis; void eval;";
    const SCRIPT: &str = r#"
      (function(){
        function a(x){return (Math.imul(x,3)+1)|0}
        function b(x){return ((x^91)+7)|0}
        const ops=[a,b];
        let s=1;for(let i=0;i<6000;i++)s=ops[i&1](s);return s;
      })()
    "#;

    fn js_string(s: &str) -> String {
        format!("{:?}", s)
    }
    fn run(interpreter_only: bool) -> (u64, String) {
        let mut st = embed::compile_script(BOOT).expect("bootstrap compiles");
        st.set_limits(BIG, None);
        if interpreter_only {
            st.start_trace(usize::MAX);
        }
        st.run_init().expect("bootstrap runs");
        let before = st.steps_remaining();
        st.eval_in_context(&format!(
            "globalThis.__computed_result=(0,eval)({})",
            js_string(SCRIPT)
        ))
        .expect("computed script runs");
        let used = before - st.steps_remaining();
        let result = st
            .eval_in_context("String(globalThis.__computed_result)")
            .expect("read result")
            .as_str()
            .expect("string result")
            .to_owned();
        (used, result)
    }

    assert_eq!(run(false), run(true));
}
