//! Guard and semantic pins for the captured RegExp `RegExpMethod` lane.
//!
//! The lane is a pure prefix: only a real RegExp whose named method still
//! resolves to the exact intrinsic can enter it. Intrinsic `test` additionally
//! proves `exec`, and declines effectful input coercions because ToString(input)
//! precedes the observable RegExpExec lookup. Every declining shape below must
//! continue through the pre-existing generic route.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const HOT: usize = 6000;

const FOREIGN_EXEC_REALM_SRC: &str = r#"
    var foreign = $262.createRealm().global;
    function hot(re, input) { return re.test(input); }
    var warm = /a/;
    for (var i = 0; i < 6000; i++) hot(warm, "a");

    var saved = RegExp.prototype.exec;
    RegExp.prototype.exec = foreign.RegExp.prototype.exec;
    var poisoned = /a/g;
    poisoned.lastIndex = foreign.Symbol("lastIndex");
    var foreignError = false, mainError = false;
    try { hot(poisoned, "a"); }
    catch (error) {
      foreignError = error.constructor === foreign.TypeError;
      mainError = error.constructor === TypeError;
    }
    RegExp.prototype.exec = saved;
    console.log(foreignError + "|" + mainError);
"#;

#[test]
fn pristine_exec_and_test_preserve_results_last_index_and_arguments() {
    let out = run_ok(
        r#"
        var extra = 0;
        function tick() { extra++; return "ignored"; }
        var re = /(a)(b)?/g;
        var m = re.exec("zab", tick());
        console.log(m[0] + "|" + m[1] + "|" + m[2] + "|" + m.index + "|" +
                    m.input + "|" + m.length + "|" + re.lastIndex + "|" + extra);

        re.lastIndex = 1;
        var yes = re.test("zab");
        var yesIndex = re.lastIndex;
        re.lastIndex = 2;
        var no = re.test("zab");
        console.log(yes + "|" + yesIndex + "|" + no + "|" + re.lastIndex + "|" +
                    /undefined/.test());
        "#,
    );
    assert_eq!(out[0], "ab|a|b|1|zab|3|3|1");
    assert_eq!(out[1], "true|3|false|0|true");
}

#[test]
fn captured_test_callee_survives_argument_mutation_and_keeps_all_effects() {
    let out = run_ok(
        r#"
        var lateCalls = 0, customCalls = 0, extras = 0;
        function late() { lateCalls++; return "late"; }
        function custom(x) { customCalls++; return this.source + ":" + x; }
        function tick() { extras++; return extras; }
        function capturedIntrinsic(re) {
          delete re.test;
          return re.test((re.test = late, "a"), tick(), tick());
        }
        function capturedCustomTest(re) {
          re.test = custom;
          return re.test((delete re.test, "z"), tick(), tick());
        }
        function capturedIntrinsicExec(re) {
          delete re.exec;
          return re.exec((re.exec = late, "a"), tick(), tick());
        }
        function capturedCustomExec(re) {
          re.exec = custom;
          return re.exec((delete re.exec, "z"), tick(), tick());
        }
        var re = /a/, intrinsic, fallback, intrinsicExec, fallbackExec;
        for (var i = 0; i < 200; i++) intrinsic = capturedIntrinsic(re);
        for (var j = 0; j < 200; j++) fallback = capturedCustomTest(re);
        for (var k = 0; k < 200; k++) intrinsicExec = capturedIntrinsicExec(re);
        for (var n = 0; n < 200; n++) fallbackExec = capturedCustomExec(re);
        console.log(intrinsic + "|" + fallback + "|" + intrinsicExec[0] + "|" +
                    fallbackExec + "|" + lateCalls + "|" + customCalls + "|" + extras);
        "#,
    );
    assert_eq!(out, ["true|a:z|a|a:z|0|400|1600"]);
}

#[test]
fn foreign_exec_realm_child() {
    if std::env::var_os("ZIPP_RX_FOREIGN_EXEC_CHILD").is_some() {
        assert_eq!(run_ok(FOREIGN_EXEC_REALM_SRC), ["true|false"]);
    }
}

#[test]
fn nested_foreign_exec_identity_is_honored_in_all_hot_modes() {
    if std::env::var_os("ZIPP_RX_FOREIGN_EXEC_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let modes: &[(&str, &[(&str, &str)])] = &[
        ("nojit", &[("ZIPP_NOJIT", "1")]),
        ("jit", &[("ZIPP_JIT_THRESHOLD", "1")]),
        (
            "gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")],
        ),
    ];
    for (mode, envs) in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["foreign_exec_realm_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_FOREIGN_EXEC_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        for &(key, value) in *envs {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn foreign-exec child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn own_accessor_proxy_and_late_prototype_overrides_are_observed() {
    let out = run_ok(&format!(
        r#"
        var own = /a/;
        own.test = function () {{ return "OWN-T"; }};
        own.exec = function () {{ return "OWN-E"; }};

        var gets = 0;
        var acc = /a/;
        Object.defineProperty(acc, "exec", {{
          get: function () {{
            gets++;
            return function (s) {{ return ["A:" + s]; }};
          }}, configurable: true
        }});
        var am = acc.exec("x");

        var traps = 0;
        var proxy = new Proxy(/a/, {{
          get: function (target, key) {{
            traps++;
            if (key === "test") return function (s) {{ return "P:" + s; }};
            return target[key];
          }}
        }});

        var late = /a/, before, after;
        for (var i = 0; i < {HOT}; i++) before = late.test("a");
        RegExp.prototype.test = function () {{ return "LATE"; }};
        for (var j = 0; j < {HOT}; j++) after = late.test("a");

        console.log(own.test("a") + "|" + own.exec("a") + "|" + am[0] + "|" + gets +
                    "|" + proxy.test("x") + "|" + traps + "|" + before + "|" + after);
        "#
    ));
    assert_eq!(out[0], "OWN-T|OWN-E|A:x|1|P:x|1|true|LATE");
}

#[test]
fn intrinsic_test_observes_custom_exec_after_input_coercion() {
    let out = run_ok(
        r#"
        var calls = 0, coerces = 0, seen = "";
        var custom = /never/;
        custom.exec = function (s) { calls++; seen = s; return { ok: 1 }; };
        var arg = { toString: function () { coerces++; return "coerced"; } };
        var first = custom.test(arg);

        // The `exec` slot is pristine when test is entered, but ToString runs
        // before RegExpExec reads it. A proof taken before this coercion cannot
        // authorize the slim builtin-exec path.
        var changedCalls = 0, changedCoerces = 0;
        var changed = /never/;
        var mutator = { toString: function () {
          changedCoerces++;
          changed.exec = function (s) { changedCalls++; return [s]; };
          return "after";
        } };
        var second = changed.test(mutator);

        var bad = /x/;
        bad.exec = function () { return 7; };
        var badThrows = 0;
        try { bad.test("x"); } catch (e) { badThrows++; }
        console.log(first + "|" + calls + "|" + coerces + "|" + seen + "|" + second +
                    "|" + changedCalls + "|" + changedCoerces + "|" + badThrows);
        "#,
    );
    assert_eq!(out[0], "true|1|1|coerced|true|1|1|1");
}

#[test]
fn coercion_last_index_and_nonwritable_throws_are_not_reexecuted() {
    let out = run_ok(
        r#"
        var inputCalls = 0, indexCalls = 0, caught = [];
        try {
          /a/.test({ toString: function () { inputCalls++; throw "input"; } });
        } catch (e) { caught.push(e); }

        var g = /a/g;
        g.lastIndex = { valueOf: function () { indexCalls++; throw "lastIndex"; } };
        try { g.exec("a"); } catch (e) { caught.push(e); }

        var ro = /a/g;
        Object.defineProperty(ro, "lastIndex", { writable: false });
        try { ro.exec("a"); } catch (e) { caught.push("readonly"); }

        console.log(caught.join("|") + "|" + inputCalls + "|" + indexCalls);
        "#,
    );
    assert_eq!(out[0], "input|lastIndex|readonly|1|1");
}

#[test]
fn last_index_value_of_can_recompile_before_matcher_state_is_read() {
    let out = run_ok(
        r#"
        var execCalls = 0;
        var e = /a/g;
        e.lastIndex = { valueOf: function () {
          execCalls++;
          e.compile("b", "g");
          return 0;
        } };
        var m = e.exec("b");

        var testCalls = 0;
        var t = /a/g;
        t.lastIndex = { valueOf: function () {
          testCalls++;
          t.compile("b", "g");
          return 0;
        } };
        var yes = t.test("b");
        console.log(m[0] + "|" + e.lastIndex + "|" + execCalls + "|" + yes + "|" +
                    t.lastIndex + "|" + testCalls);
        "#,
    );
    assert_eq!(out[0], "b|1|1|true|1|1");
}

/// Each mode needs a child because the env latches and counters are
/// process-global. Besides parity, this proves that both generated helper arms
/// actually serve hot calls and that the off switch removes the mechanism.
#[test]
fn direct_call_mechanism_counts_and_off_switch_are_non_vacuous() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in ["hits", "fallback", "throw", "off"] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["direct_call_counts_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_DIRECT_CHILD", mode)
            .env("ZIPP_RXSTATS", "1")
            .env("ZIPP_JIT_THRESHOLD", "32")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_RX_CALL_DIRECT");
        if mode == "off" {
            cmd.env("ZIPP_NO_RX_CALL_DIRECT", "1");
        }
        let out = cmd.output().expect("spawn direct-call counter child");
        assert!(
            out.status.success(),
            "{mode} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn direct_call_counts_child() {
    let Some(mode) = std::env::var_os("ZIPP_RX_DIRECT_CHILD") else {
        return;
    };
    match mode.to_string_lossy().as_ref() {
        "hits" | "off" => {
            let out = run_ok(&format!(
                r#"
                var score = 0, t = /a/;
                for (var i = 0; i < {HOT}; i++) if (t.test("a")) score++;
                var e = /(a)/;
                for (var j = 0; j < {HOT}; j++) {{
                  e.exec("a");
                  score++;
                }}
                console.log(score);
                "#
            ));
            assert_eq!(out[0], (HOT * 2).to_string());
            let (it, ie, jt, je, declines) = zipp_vm::regexp_call_direct_stats();
            if mode == "off" {
                assert_eq!((it, ie, jt, je, declines), (0, 0, 0, 0, 0));
            } else {
                assert!(
                    it + jt >= HOT as u64,
                    "test lane did not cover calls: {:?}",
                    (it, jt)
                );
                assert!(
                    ie + je >= HOT as u64,
                    "exec lane did not cover calls: {:?}",
                    (ie, je)
                );
                assert!(it > 0 && ie > 0, "interpreter warmup did not use both arms");
                if cfg!(feature = "jit") {
                    assert!(
                        jt > 0,
                        "generated test helper never served: {:?}",
                        (it, ie, jt, je, declines)
                    );
                    assert!(
                        je > 0,
                        "generated exec helper never served: {:?}",
                        (it, ie, jt, je, declines)
                    );
                } else {
                    assert_eq!((jt, je), (0, 0));
                }
            }
        }
        "fallback" => {
            let out = run_ok(&format!(
                r#"
                var re = /a/, before, after, calls = 0;
                for (var i = 0; i < {HOT}; i++) before = re.test("a");
                RegExp.prototype.test = function () {{ calls++; return "late"; }};
                for (var j = 0; j < {HOT}; j++) after = re.test("a");
                console.log(before + "|" + after + "|" + calls);
                "#
            ));
            assert_eq!(out[0], format!("true|late|{HOT}"));
            let (it, _, jt, _, declines) = zipp_vm::regexp_call_direct_stats();
            if cfg!(feature = "jit") {
                assert!(
                    jt > 0,
                    "warm pristine calls never used the generated helper"
                );
            } else {
                assert!(
                    it > 0,
                    "warm pristine calls never used the interpreter helper"
                );
            }
            assert!(
                declines >= HOT as u64,
                "late override did not exercise guard fallback: declines={declines}"
            );
        }
        "throw" => {
            let out = run_ok(&format!(
                r#"
                var calls = 0, caught = 0, re = /(a)/;
                re.lastIndex = {{ valueOf: function () {{
                  calls++;
                  if (calls === {HOT}) throw "boom";
                  return 0;
                }} }};
                try {{
                  for (var i = 0; i < {HOT}; i++) re.exec("a");
                }} catch (e) {{ caught++; }}
                console.log(calls + "|" + caught);
                "#
            ));
            assert_eq!(out[0], format!("{HOT}|1"));
            let (_, ie, _, je, _) = zipp_vm::regexp_call_direct_stats();
            if cfg!(feature = "jit") {
                assert!(je > 0, "throwing call site never used the generated helper");
            } else {
                assert!(
                    ie > 0,
                    "throwing call site never used the interpreter helper"
                );
            }
        }
        other => panic!("unknown child mode {other}"),
    }
}
