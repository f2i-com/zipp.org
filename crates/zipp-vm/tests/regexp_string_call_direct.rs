//! Guard, protocol-ordering, and mechanism pins for the primitive-string
//! `matchAll(RegExp)` / `replace(RegExp, string)` captured RegExpMethod lane.
//!
//! A direct attempt is a pure prefix: receiver/argument kinds, active realm,
//! the live String prototype method, and every RegExp dependency are proven
//! before coercion, accessors, writes, allocation, or matching.  Every shape
//! outside that small set must continue through the full observable protocol.

use std::process::Command;

const HOT: usize = 5000;

const FOREIGN_PROTOCOL_REALM_SRC: &str = r#"
    "use strict";
    var foreign = $262.createRealm().global;
    function hotMatch(s, re) { return s.matchAll(re); }
    function hotReplace(s, re, replacement) { return s.replace(re, replacement); }
    for (var i = 0; i < 6000; i++) {
      Array.from(hotMatch("aba", /a/g));
      hotReplace("aba", /a/g, "x");
    }

    var foreignIterProto = Object.getPrototypeOf(
      foreign.RegExp.prototype[Symbol.matchAll].call(/a/g, "a")
    );
    function withProtoSlot(key, body) {
      var main = Object.getOwnPropertyDescriptor(RegExp.prototype, key);
      var child = Object.getOwnPropertyDescriptor(foreign.RegExp.prototype, key);
      Object.defineProperty(RegExp.prototype, key, child);
      try { return body(); }
      finally { Object.defineProperty(RegExp.prototype, key, main); }
    }

    var foreignIterator = withProtoSlot(Symbol.matchAll, function () {
      return hotMatch("a", /a/g);
    });
    var matchAllRealm = Object.getPrototypeOf(foreignIterator) === foreignIterProto;

    var matchValue = withProtoSlot(Symbol.match, function () {
      return Array.from(hotMatch("a", /a/g)).length;
    });
    var flagsValue = withProtoSlot("flags", function () {
      return Array.from(hotMatch("a", /a/g)).length;
    });

    var componentNames = [
      "hasIndices", "global", "ignoreCase", "multiline",
      "dotAll", "unicode", "unicodeSets", "sticky"
    ];
    var componentValues = true;
    for (var c = 0; c < componentNames.length; c++) {
      var count = withProtoSlot(componentNames[c], function () {
        return Array.from(hotMatch("a", /a/g)).length;
      });
      componentValues = componentValues && count === 1;
    }

    var foreignMatchArray = withProtoSlot("exec", function () {
      return hotMatch("a", /a/g).next().value;
    });
    var execRealm = Object.getPrototypeOf(foreignMatchArray) === foreign.Array.prototype;

    var mainSpecies = Object.getOwnPropertyDescriptor(RegExp, Symbol.species);
    var childSpecies = Object.getOwnPropertyDescriptor(foreign.RegExp, Symbol.species);
    Object.defineProperty(RegExp, Symbol.species, childSpecies);
    var speciesValue;
    try { speciesValue = Array.from(hotMatch("a", /a/g)).length; }
    finally { Object.defineProperty(RegExp, Symbol.species, mainSpecies); }

    var replaceForeignError = false, replaceMainError = false, replaceErrorName = "none";
    withProtoSlot(Symbol.replace, function () {
      var re = /a/;
      Object.defineProperty(re, "flags", { value: foreign.Symbol("flags") });
      try { hotReplace("a", re, "x"); }
      catch (error) {
        replaceForeignError = error.constructor === foreign.TypeError;
        replaceMainError = error.constructor === TypeError;
        replaceErrorName = error.constructor && error.constructor.name;
      }
    });

    console.log([
      matchAllRealm, matchValue, flagsValue, componentValues, execRealm,
      speciesValue, replaceForeignError, replaceMainError, replaceErrorName
    ].join("|"));
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
    let out = Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from node -e)");
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

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

#[test]
fn pristine_hot_results_last_index_and_string_replacements_match_node() {
    assert_matches_node(&format!(
        r#""use strict";
        var left = "a=1 b=", right = "22 c=333";
        var subject = left + right; // runtime concat: exercises Cons receiver too
        var ma = /([a-z])=(\d+)/g, last;
        for (var i = 0; i < {HOT}; i++) last = Array.from(subject.matchAll(ma));
        console.log(last.map(function (m) {{
          return m[0] + ":" + m[1] + ":" + m[2] + "@" + m.index + ":" + m.input;
        }}).join("|"));
        console.log("ma.lastIndex=" + ma.lastIndex);

        var one = /(b)(=)(22)/;
        var oneOut = "a=1 b=22 c=333".replace(one, "$3<$1>[$&]-$$-$`-$'");
        console.log(oneOut + "|" + one.lastIndex);

        var all = /([a-z])=(\d+)/g, replOut = "";
        for (var j = 0; j < {HOT}; j++)
          replOut = subject.replace(all, "$2<$1>");
        console.log(replOut + "|" + all.lastIndex);
        "#
    ));
}

#[test]
fn replace_fast_path_updates_annex_b_regexp_statics() {
    assert_matches_node(&format!(
        r#""use strict";
        var ascii = /(a)(b)/g, asciiOut = "";
        for (var i = 0; i < {HOT}; i++)
          asciiOut = "xabYabz".replace(ascii, "$2$1");
        console.log(asciiOut + "|" + [
          RegExp.input, RegExp.lastMatch, RegExp.lastParen,
          RegExp.leftContext, RegExp.rightContext, RegExp.$1, RegExp.$2
        ].join("|"));

        var wide = /(α)(β)/g;
        var wideOut = "xαβYαβz".replace(wide, "$2$1");
        console.log(wideOut + "|" + [
          RegExp.input, RegExp.lastMatch, RegExp.lastParen,
          RegExp.leftContext, RegExp.rightContext, RegExp.$1, RegExp.$2
        ].join("|"));
        "#
    ));
}

#[test]
fn global_replace_normalises_negative_zero_last_index() {
    assert_matches_node(
        r#""use strict";
        var re = /a/g;
        re.lastIndex = -0;
        var out = "a".replace(re, "x");
        console.log(out + "|" + Object.is(re.lastIndex, -0) + "|" + re.lastIndex);
        "#,
    );
}

#[test]
fn captured_string_callees_survive_argument_mutation_and_keep_all_effects() {
    let out = run_ok(
        r#""use strict";
        var intrinsicReplace = String.prototype.replace;
        var intrinsicMatchAll = String.prototype.matchAll;
        var lateCalls = 0, customCalls = 0, extras = 0;
        function tick() { extras++; return extras; }
        function late() { lateCalls++; return "late"; }
        function customReplace(search, replacement) {
          customCalls++;
          return "R:" + this + ":" + replacement + ":" + arguments.length;
        }
        function customMatchAll(search) {
          customCalls++;
          return ["M:" + this + ":" + search.source + ":" + arguments.length];
        }
        function capturedIntrinsicReplace(s, re) {
          String.prototype.replace = intrinsicReplace;
          return s.replace((String.prototype.replace = late, re), "x", tick(), tick());
        }
        function capturedCustomReplace(s, re) {
          String.prototype.replace = customReplace;
          return s.replace((String.prototype.replace = intrinsicReplace, re), "x", tick(), tick());
        }
        function capturedIntrinsicMatchAll(s, re) {
          String.prototype.matchAll = intrinsicMatchAll;
          return Array.from(s.matchAll(
            (String.prototype.matchAll = late, re), tick(), tick()
          )).length;
        }
        function capturedCustomMatchAll(s, re) {
          String.prototype.matchAll = customMatchAll;
          return s.matchAll(
            (String.prototype.matchAll = intrinsicMatchAll, re), tick(), tick()
          )[0];
        }

        var ir, cr, im, cm;
        for (var i = 0; i < 200; i++) ir = capturedIntrinsicReplace("aba", /a/g);
        for (var j = 0; j < 200; j++) cr = capturedCustomReplace("aba", /a/g);
        for (var k = 0; k < 200; k++) im = capturedIntrinsicMatchAll("aba", /a/g);
        for (var n = 0; n < 200; n++) cm = capturedCustomMatchAll("aba", /a/g);
        String.prototype.replace = intrinsicReplace;
        String.prototype.matchAll = intrinsicMatchAll;
        console.log(ir + "|" + cr + "|" + im + "|" + cm + "|" +
                    lateCalls + "|" + customCalls + "|" + extras);
        "#,
    );
    assert_eq!(out, ["xbx|R:aba:x:4|2|M:aba:a:3|0|400|1600"]);
}

#[test]
fn foreign_protocol_realm_child() {
    let Some(mode) = std::env::var_os("ZIPP_RX_FOREIGN_PROTOCOL_CHILD") else {
        return;
    };
    assert_eq!(
        run_ok(FOREIGN_PROTOCOL_REALM_SRC),
        ["true|1|1|true|true|1|true|false|TypeError"]
    );
    let stats = zipp_vm::regexp_string_call_direct_stats();
    // Twelve mutations decline at RegExpMethod itself. `exec` and `@@species`
    // are re-proved inside the still-exact main @@matchAll native, so their
    // slow-protocol fallback is covered by the foreign result realm / semantic
    // checks above rather than the outer decline counter.
    assert!(
        stats.4 >= 12,
        "same-id foreign protocol slots bypassed exact guards in {mode:?}: {stats:?}"
    );
    if mode == "nojit" || !cfg!(feature = "jit") {
        assert!(
            stats.0 > 0 && stats.1 > 0,
            "interpreter direct arms were vacuous: {stats:?}"
        );
    } else {
        assert!(
            stats.2 > 0 && stats.3 > 0,
            "generated direct arms were vacuous: {stats:?}"
        );
    }
}

#[test]
fn nested_foreign_protocol_identities_are_honored_in_all_hot_modes() {
    if std::env::var_os("ZIPP_RX_FOREIGN_PROTOCOL_CHILD").is_some() {
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
        let mut cmd = Command::new(&exe);
        cmd.args(["foreign_protocol_realm_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_FOREIGN_PROTOCOL_CHILD", *mode)
            .env("ZIPP_RXSTATS", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        for &(key, value) in *envs {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn foreign-protocol child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// This is both an adversarial direct-guard test and a regression pin for the
/// switchless generic-builtin fix.  Static and computed CallMethod used to bind
/// these two methods from receiver kind + name and ignore the live prototype.
#[test]
fn string_prototype_data_accessor_delete_inherit_and_late_mutation_are_observed() {
    assert_matches_node(&format!(
        r#""use strict";
        var s = "aba", r = /a/g, before = "", after = "", calls = 0;
        for (var i = 0; i < {HOT}; i++) before = s.replace(r, "x");
        String.prototype.replace = function (search, repl) {{
          calls++; return "DATA:" + search.source + ":" + repl;
        }};
        for (var j = 0; j < {HOT}; j++) after = s.replace(r, "x");
        console.log(before + "|" + after + "|" + calls);

        var gets = 0;
        Object.defineProperty(String.prototype, "matchAll", {{
          configurable: true,
          get: function () {{
            gets++;
            return function (rx) {{ return ["ACCESSOR:" + rx.source]; }};
          }}
        }});
        console.log("z".matchAll(/z/g)[0] + "|" + gets);

        delete String.prototype.matchAll;
        Object.prototype.matchAll = function (rx) {{ return ["INHERITED:" + rx.source]; }};
        console.log("q"["matchAll"](/q/g)[0]);

        delete String.prototype.replace;
        Object.prototype.replace = function (rx, repl) {{
          return "INHERITED-REPLACE:" + rx.source + ":" + repl;
        }};
        console.log("q"["replace"](/q/g, "Q"));
        "#
    ));
}

#[test]
fn matchall_protocol_overrides_global_species_exec_and_boxed_receiver_match_node() {
    assert_matches_node(
        r#""use strict";
        var ownCalls = 0;
        var own = /a/g;
        own[Symbol.matchAll] = function (s) {
          ownCalls++; return ["OWN:" + s + ":" + this.source];
        };
        console.log("aba".matchAll(own)[0] + "|" + ownCalls);

        var matchGets = 0, flagsGets = 0, methodGets = 0;
        var observed = /b/g;
        Object.defineProperty(observed, Symbol.match, {
          configurable: true, get: function () { matchGets++; return true; }
        });
        Object.defineProperty(observed, "flags", {
          configurable: true, get: function () { flagsGets++; return "g"; }
        });
        Object.defineProperty(observed, Symbol.matchAll, {
          configurable: true, get: function () {
            methodGets++;
            return function (s) { return ["OBS:" + s]; };
          }
        });
        console.log("abc".matchAll(observed)[0] + "|" +
                    matchGets + "|" + flagsGets + "|" + methodGets);

        var nonglobal = "no-throw";
        try { "abc".matchAll(/b/); } catch (e) { nonglobal = e.constructor.name; }
        console.log(nonglobal);

        var speciesGets = 0, constructions = 0;
        class Child extends RegExp {
          constructor(p, f) { super(p, f); constructions++; }
        }
        class R extends RegExp {
          static get [Symbol.species]() { speciesGets++; return Child; }
        }
        var sub = new R("(a)", "g"), vals = [];
        for (var m of "aba".matchAll(sub)) vals.push(m[0] + "@" + m.index);
        console.log(vals.join("|") + "|" + speciesGets + "|" + constructions);

        var execCalls = 0, oldExec = RegExp.prototype.exec;
        RegExp.prototype.exec = function (s) {
          execCalls++;
          if (execCalls > 1) return null;
          return { 0: "CUSTOM", index: 1, length: 1, groups: undefined };
        };
        var customOut = Array.from("abc".matchAll(/x/g));
        RegExp.prototype.exec = oldExec;
        console.log(customOut[0][0] + "@" + customOut[0].index + "|" + execCalls);

        var boxed = new String("aba");
        console.log(Array.from(boxed.matchAll(/a/g)).map(function (x) { return x.index; }).join(","));
        boxed.matchAll = function () { return ["BOX-OWN"]; };
        boxed.replace = function () { return "BOX-REPLACE"; };
        console.log(boxed.matchAll(/a/g)[0] + "|" + boxed.replace(/a/g, "x"));
        "#,
    );
}

#[test]
fn replace_protocol_overrides_flags_frozen_substitutions_and_effectful_replacers_match_node() {
    assert_matches_node(
        r#""use strict";
        var symCalls = 0, sym = /a/g;
        sym[Symbol.replace] = function (s, r) {
          symCalls++; return "SYM:" + s + ":" + r;
        };
        console.log("aba".replace(sym, "x") + "|" + symCalls);

        var execCalls = 0, custom = /x/g;
        custom.exec = function (s) {
          execCalls++;
          if (execCalls === 1)
            return { 0: "Q", 1: "cap", index: 1, length: 2, groups: { n: "named" } };
          return null;
        };
        console.log("abc".replace(custom, "$1:$<n>:$&") + "|" + execCalls);

        var gd = Object.getOwnPropertyDescriptor(RegExp.prototype, "global");
        var ud = Object.getOwnPropertyDescriptor(RegExp.prototype, "unicode");
        var globalGets = 0, unicodeGets = 0;
        Object.defineProperty(RegExp.prototype, "global", {
          configurable: true, get: function () { globalGets++; return gd.get.call(this); }
        });
        Object.defineProperty(RegExp.prototype, "unicode", {
          configurable: true, get: function () { unicodeGets++; return ud.get.call(this); }
        });
        var accessorOut = "aba".replace(/a/g, "x");
        Object.defineProperty(RegExp.prototype, "global", gd);
        Object.defineProperty(RegExp.prototype, "unicode", ud);
        console.log(accessorOut + "|" + globalGets + "|" + unicodeGets);

        var frozen = /a/g, frozenResult;
        Object.freeze(frozen);
        try { frozenResult = "a".replace(frozen, "x"); }
        catch (e) { frozenResult = e.constructor.name; }
        console.log(frozenResult + "|" + frozen.lastIndex);

        var groups = "zabq".replace(/(?<first>a)(b)/g,
          "$<first>-$2-$&-$$-$`-$'");
        console.log(groups);

        var fnCalls = 0;
        var functional = "a1 b22".replace(/([a-z])(\d+)/g,
          function (whole, a, n, offset, input) {
            fnCalls++; return n + a + "@" + offset + "/" + input.length;
          });
        console.log(functional + "|" + fnCalls);

        var boxed = new String("<$&>");
        console.log("aba".replace(/a/g, boxed));

        // ToString(replaceValue) precedes the flags/exec loop.  This mutation
        // must govern the very first match; proving exec before coercion and
        // entering the internal matcher would produce a different answer.
        var patchCalls = 0, patchedExec = 0, re = /a/g;
        var effectful = { toString: function () {
          patchCalls++;
          re.exec = function () {
            patchedExec++;
            if (patchedExec > 1) return null;
            return { 0: "Z", index: 1, length: 1, groups: undefined };
          };
          return "<$&>";
        } };
        console.log("aa".replace(re, effectful) + "|" + patchCalls + "|" + patchedExec);
        "#,
    );
}

const MODE_SRC: &str = r#""use strict";
    var s = "a1 b22 c333", ma = /([a-z])(\d+)/g, rp = /([a-z])(\d+)/g;
    var n = 0, out = "";
    for (var i = 0; i < 500; i++) {
      n += Array.from(s.matchAll(ma)).length;
      out = s.replace(rp, "$2<$1>");
    }
    console.log(n + "|" + out + "|" + ma.lastIndex + "|" + rp.lastIndex);

    var hits = 0, late = /a/g;
    var repl = { toString: function () {
      hits++;
      late.exec = function () { return null; };
      return "x";
    } };
    console.log("a".replace(late, repl) + "|" + hits);
"#;

#[test]
fn string_regexp_mode_child() {
    if std::env::var_os("ZIPP_RX_STRING_MODE_CHILD").is_some() {
        assert_matches_node(MODE_SRC);
    }
}

#[test]
fn zz_string_regexp_direct_modes_agree_with_node() {
    if std::env::var_os("ZIPP_RX_STRING_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let modes: &[(&str, &[(&str, &str)])] = &[
        ("default", &[]),
        ("off", &[("ZIPP_NO_RX_STRING_CALL_DIRECT", "1")]),
        ("nojit", &[("ZIPP_NOJIT", "1")]),
        ("threshold1", &[("ZIPP_JIT_THRESHOLD", "1")]),
        ("gcstress", &[("ZIPP_GC_STRESS", "1")]),
    ];
    for (mode, envs) in modes {
        let mut cmd = Command::new(&exe);
        cmd.args(["string_regexp_mode_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_STRING_MODE_CHILD", "1")
            .env_remove("ZIPP_NO_RX_STRING_CALL_DIRECT")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        for &(key, value) in *envs {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
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
fn string_regexp_counts_child() {
    let Some(mode) = std::env::var_os("ZIPP_RX_STRING_COUNTS_CHILD") else {
        return;
    };
    match mode.to_string_lossy().as_ref() {
        "hits" | "off" => {
            let out = run_ok(&format!(
                r#"var s = "a1 b22", ma = /([a-z])(\d+)/g, rp = /([a-z])(\d+)/g;
                var it, text = "";
                for (var i = 0; i < {HOT}; i++) it = s.matchAll(ma);
                for (var j = 0; j < {HOT}; j++) text = s.replace(rp, "$2<$1>");
                console.log(Array.from(it).length + "|" + text);"#
            ));
            assert_eq!(out[0], "2|1<a> 22<b>");
            let stats = zipp_vm::regexp_string_call_direct_stats();
            if mode == "off" {
                assert_eq!(stats, (0, 0, 0, 0, 0));
            } else {
                let (im, ir, jm, jr, _) = stats;
                assert!(im > 0 && ir > 0, "interpreter arms were vacuous: {stats:?}");
                if cfg!(feature = "jit") {
                    assert!(jm > 0 && jr > 0, "generated arms were vacuous: {stats:?}");
                } else {
                    assert_eq!((jm, jr), (0, 0));
                }
                assert!(
                    im + jm >= HOT as u64 && ir + jr >= HOT as u64,
                    "lost calls: {stats:?}"
                );
            }
        }
        "fallback" => {
            let out = run_ok(
                r#"var s = "aba", r = /a/g, box = new String("x"), out;
                for (var i = 0; i < 1000; i++) out = s.replace(r, box);
                var fake = { [Symbol.matchAll]: function () { return ["fake"]; } };
                for (var j = 0; j < 1000; j++) out = s.matchAll(fake)[0];
                console.log(out);"#,
            );
            assert_eq!(out[0], "fake");
            let stats = zipp_vm::regexp_string_call_direct_stats();
            assert!(stats.4 >= 2000, "guard fallback was vacuous: {stats:?}");
        }
        "throw" => {
            let out = run_ok(&format!(
                r#"var calls = 0, caught = 0, r = /a/g, it;
                r.lastIndex = {{ valueOf: function () {{
                  calls++; if (calls === {HOT}) throw "boom"; return 0;
                }} }};
                try {{ for (var i = 0; i < {HOT}; i++) it = "a".matchAll(r); }}
                catch (e) {{ caught++; }}
                console.log(calls + "|" + caught);"#
            ));
            assert_eq!(out[0], format!("{HOT}|1"));
            let stats = zipp_vm::regexp_string_call_direct_stats();
            if cfg!(feature = "jit") {
                assert!(
                    stats.2 > 0,
                    "throwing site never entered generated helper: {stats:?}"
                );
            } else {
                assert!(
                    stats.0 > 0,
                    "throwing site never entered interpreter helper: {stats:?}"
                );
            }
        }
        other => panic!("unknown child mode {other}"),
    }
}

/// Process-global env latches and counters require one child per case.  Besides
/// semantic coverage this proves both generated arms engage, guard declines are
/// real, committed throws are not replayed, and the off switch emits/probes none.
#[test]
fn zz_string_regexp_direct_mechanism_counts_and_off_switch() {
    if std::env::var_os("ZIPP_RX_STRING_COUNTS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for mode in ["hits", "fallback", "throw", "off"] {
        let mut cmd = Command::new(&exe);
        cmd.args(["string_regexp_counts_child", "--exact", "--nocapture"])
            .env("ZIPP_RX_STRING_COUNTS_CHILD", mode)
            .env("ZIPP_RXSTATS", "1")
            .env("ZIPP_JIT_THRESHOLD", "32")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_RX_STRING_CALL_DIRECT");
        if mode == "off" {
            cmd.env("ZIPP_NO_RX_STRING_CALL_DIRECT", "1");
        }
        let out = cmd.output().expect("spawn mechanism child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
