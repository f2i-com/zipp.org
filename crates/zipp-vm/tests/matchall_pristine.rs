//! The pristine `String.prototype.matchAll` dispatch (the B77 retry) and the
//! `regexp_matchall_fast_ok` slot memo (B68 item 2) must be INVISIBLE.
//!
//! Two mechanisms on the matchAll-creation path share these pins:
//!
//!   * `matchall_pristine_dispatch` skips IsRegExp's `@@match` Get, the
//!     `flags` Get + ToString and `GetMethod(@@matchAll)` + `call_value` when
//!     every one of those reads provably yields the intrinsic — B77's
//!     mechanism, re-landed in a separate `#[inline(never)]` function per its
//!     revert note (the semantics won twice; the loss was fat-LTO layout);
//!   * `regexp_matchall_fast_ok_cached` answers the gate's prototype/
//!     constructor half from version-guarded slot indices instead of nine
//!     hashed `pos()` scans per call.
//!
//! Both re-read slot VALUES and accessor bits per call, because an in-place
//! `RegExp.prototype.exec = f` overwrite does not bump the heap version
//! (B67), and a descriptor-only change flips attrs (B107) — so every way the
//! protocol can stop reaching the intrinsic MID-RUN, after a hot warm-up, is
//! pinned here. Every `ma_parity_*` expectation below was executed in node
//! v24.12.0 and diffs byte-identical. The whole set re-runs in child
//! processes with `ZIPP_NO_MATCHALL_PRISTINE=1`, `ZIPP_NO_FASTOK_MEMO=1` and
//! `ZIPP_NOJIT=1`, so all modes are held to the same node-derived outputs.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Hot enough to cross the JIT thresholds and warm both the dispatch
/// shortcut and the slot memo before any mutation lands.
const HOT: usize = 2000;

#[test]
fn ma_parity_a_hot_loop_matches_with_captures_and_a_resting_lastindex() {
    let out = run_ok(&format!(
        r#"
        var re = /([a-z]+)=(\d+)/g;
        var s = "a=1 bb=22 ccc=333";
        var total = 0, caps = "";
        for (var i = 0; i < {HOT}; i++) {{
          var n = 0;
          for (var m of s.matchAll(re)) {{
            n++;
            if (i === 0) caps += m[1] + ":" + m[2] + "@" + m.index + " ";
          }}
          total += n;
        }}
        console.log("total=" + (total / {HOT}) + " caps=" + caps.trim() + " li=" + re.lastIndex);
        "#
    ));
    assert_eq!(out[0], "total=3 caps=a:1@0 bb:22@4 ccc:333@10 li=0");
}

#[test]
fn ma_parity_exec_replaced_on_the_prototype_mid_run_is_routed_through() {
    // `RegExp.prototype.exec = f` is an IN-PLACE data write — no version bump
    // anywhere — so only the per-call value re-read can catch it. RegExpExec
    // reads `exec` per STEP: 3 matches + the final null = 4 calls.
    let out = run_ok(&format!(
        r#"
        var re = /a/g;
        var s = "aaa";
        var before = 0;
        for (var i = 0; i < {HOT}; i++) {{ before = Array.from(s.matchAll(re)).length; }}
        var calls = 0;
        var orig = RegExp.prototype.exec;
        RegExp.prototype.exec = function (str) {{ calls++; return orig.call(this, str); }};
        var after = Array.from(s.matchAll(re)).length;
        RegExp.prototype.exec = orig;
        console.log(before + "/" + after + "/" + calls);
        "#
    ));
    assert_eq!(out[0], "3/3/4");
}

#[test]
fn ma_parity_flags_redefined_as_an_accessor_mid_run_is_read() {
    // `defineProperty` bumps the proto version, and the accessor bit is
    // re-read regardless: the getter must run (once — String.prototype
    // .matchAll's 'g' test; the construction consumes the same protocol),
    // and a LYING empty-flags getter on a global regex must throw.
    let out = run_ok(&format!(
        r#"
        var re = /x/g;
        var s = "xx";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var reads = 0;
        Object.defineProperty(RegExp.prototype, "flags", {{
          get: function () {{ reads++; return "g"; }},
          configurable: true
        }});
        var n = Array.from(s.matchAll(re)).length;
        console.log(n + "/" + reads);
        var lying = 0;
        Object.defineProperty(RegExp.prototype, "flags", {{
          get: function () {{ lying++; return ""; }},
          configurable: true
        }});
        try {{
          s.matchAll(/y/g);
          console.log("no-throw/" + lying);
        }} catch (e) {{
          console.log("TypeError/" + lying);
        }}
        "#
    ));
    assert_eq!(out[0], "2/2");
    assert_eq!(out[1], "TypeError/1");
}

#[test]
fn ma_parity_symbol_matchall_replaced_mid_run_wins_proto_and_own() {
    let out = run_ok(&format!(
        r#"
        var re = /b/g;
        var s = "bb";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var orig = RegExp.prototype[Symbol.matchAll];
        RegExp.prototype[Symbol.matchAll] = function (str) {{ return "PROTO:" + str; }};
        var a = s.matchAll(re);
        RegExp.prototype[Symbol.matchAll] = orig;
        var re2 = /b/g;
        re2[Symbol.matchAll] = function (str) {{ return "OWN:" + str; }};
        var b = s.matchAll(re2);
        console.log(a + "|" + b + "|" + Array.from(s.matchAll(re)).length);
        "#
    ));
    assert_eq!(out[0], "PROTO:bb|OWN:bb|2");
}

#[test]
fn ma_parity_subclass_receivers_observe_species_and_an_exec_override() {
    // A subclass instance's proto_of names the SUBCLASS prototype, so both
    // mechanisms decline before their shared proofs are consulted; the
    // species getter and an overridden per-step exec must run.
    let out = run_ok(
        r#"
        class MyRe extends RegExp {}
        var made = 0;
        class Tracked extends RegExp {
          static get [Symbol.species]() { made++; return RegExp; }
        }
        var s = "cc";
        var m1 = Array.from(s.matchAll(new MyRe("c", "g")));
        var m2 = Array.from(s.matchAll(new Tracked("c", "g")));
        console.log(m1.length + "/" + m2.length + "/" + made);
        class ExecRe extends RegExp {
          exec(str) { this.lastIndex = 99; return null; }
        }
        var m3 = Array.from(s.matchAll(new ExecRe("c", "g")));
        console.log(m3.length);
        "#,
    );
    assert_eq!(out[0], "2/2/1");
    assert_eq!(out[1], "0");
}

#[test]
fn ma_parity_lastindex_games() {
    // The matcher copies ToLength(lastIndex) (a mid-string start skips a
    // match; the SOURCE regex is untouched); an object lastIndex runs
    // valueOf, a throwing one throws, and a non-global regex is a TypeError.
    let out = run_ok(
        r#"
        var s = "dd dd";
        var re = /dd/g;
        re.lastIndex = 3;
        var n1 = Array.from(s.matchAll(re)).length;
        var li = re.lastIndex;
        var vo = 0;
        var re2 = /dd/g;
        re2.lastIndex = { valueOf: function () { vo++; return 0; } };
        var n2 = Array.from(s.matchAll(re2)).length;
        var re3 = /dd/g;
        re3.lastIndex = { valueOf: function () { throw new Error("boom"); } };
        var threw;
        try { s.matchAll(re3); threw = "no"; } catch (e) { threw = e.message; }
        var nong;
        try { s.matchAll(/dd/); nong = "no"; } catch (e) { nong = e.constructor.name; }
        console.log(n1 + "/" + li + "/" + n2 + "," + vo + "/" + threw + "/" + nong);
        "#,
    );
    assert_eq!(out[0], "1/3/2,1/boom/TypeError");
}

#[test]
fn ma_parity_symbol_match_games() {
    // A plain object with @@matchAll is dispatched to directly; hiding
    // @@match with `undefined` mid-run leaves IsRegExp answering via
    // [[RegExpMatcher]] — a non-global regex still throws, a global one
    // still iterates.
    let out = run_ok(&format!(
        r#"
        var s = "ee";
        var obj = {{ [Symbol.matchAll]: function (str) {{ return "OBJ:" + str; }} }};
        var a = s.matchAll(obj);
        var re = /e/g;
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        Object.defineProperty(RegExp.prototype, Symbol.match,
                              {{ value: undefined, configurable: true, writable: true }});
        var b;
        try {{ b = Array.from(s.matchAll(/e/)).length; }} catch (e) {{ b = e.constructor.name; }}
        var c = Array.from(s.matchAll(re)).length;
        console.log(a + "|" + b + "|" + c);
        "#
    ));
    assert_eq!(out[0], "OBJ:ee|TypeError|2");
}

/// The env latches are read once per process, so each off mode needs its own
/// process: re-run every `ma_parity_*` expectation in a child with the switch
/// set. The same node-derived assertions passing in every mode IS the parity
/// check.
fn rerun_parity_with(var: &str) {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(exe)
        .arg("ma_parity_")
        .env(var, "1")
        .output()
        .expect("spawn the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{var}=1 run failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.contains("running 0 tests"),
        "the ma_parity_ filter matched nothing:\n{stdout}"
    );
}

#[test]
fn the_pristine_dispatch_off_switch_behaves_identically() {
    rerun_parity_with("ZIPP_NO_MATCHALL_PRISTINE");
}

#[test]
fn the_fastok_memo_off_switch_behaves_identically() {
    rerun_parity_with("ZIPP_NO_FASTOK_MEMO");
}

#[test]
fn the_interpreter_only_mode_behaves_identically() {
    rerun_parity_with("ZIPP_NOJIT");
}
