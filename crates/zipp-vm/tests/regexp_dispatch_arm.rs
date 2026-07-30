//! The RegExp `test`/`exec` receiver-kind dispatch arm (B69) must be INVISIBLE.
//!
//! `dispatch_builtin_method_inner` had arms for eleven heap kinds and none for
//! RegExp, so `re.test(s)` ran the whole builtin probe as dead work and then took
//! the generic `get_prop` + `call_value` route. Adding the arm is worth ~17% on a
//! hot `re.test()` loop — but the sibling arms are NOT override-safe, and RegExp
//! was correct *because* it had no arm:
//!
//!     String.prototype.indexOf = () => "OVERRIDDEN";
//!     "abc".indexOf("b")   // node: "OVERRIDDEN"   zipp: 1   ← pre-existing bug
//!
//! So every way the name can stop reaching the intrinsic is pinned here. Each
//! expectation was checked against node, not reasoned about. If one of these
//! regresses, the arm has become a faster wrong answer and must come out.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Hot enough to cross the thresholds, so the compiled paths are exercised too.
const HOT: usize = 3000;

#[test]
fn an_own_method_on_the_instance_wins() {
    let out = run_ok(&format!(
        r#"
        var re = /a/;
        re.test = function () {{ return "OWN-TEST"; }};
        re.exec = function () {{ return "OWN-EXEC"; }};
        var t, e;
        for (var i = 0; i < {HOT}; i++) {{ t = re.test("a"); e = re.exec("a"); }}
        console.log(t + "," + e);
        "#
    ));
    assert_eq!(out[0], "OWN-TEST,OWN-EXEC");
}

#[test]
fn a_replaced_prototype_method_wins() {
    let out = run_ok(&format!(
        r#"
        RegExp.prototype.test = function () {{ return "PROTO-TEST"; }};
        RegExp.prototype.exec = function () {{ return "PROTO-EXEC"; }};
        var re = /a/, t, e;
        for (var i = 0; i < {HOT}; i++) {{ t = re.test("a"); e = re.exec("a"); }}
        console.log(t + "," + e);
        "#
    ));
    assert_eq!(out[0], "PROTO-TEST,PROTO-EXEC");
}

#[test]
fn a_prototype_method_replaced_AFTER_the_loop_is_hot_still_wins() {
    // The guard is re-checked per call, not cached — `ObjMap::set` does not bump
    // the heap version on a plain overwrite (B67), so a version-keyed cache here
    // would go stale exactly in this case.
    let out = run_ok(&format!(
        r#"
        var re = /a/, before, after;
        for (var i = 0; i < {HOT}; i++) before = re.test("a");
        RegExp.prototype.test = function () {{ return "LATE"; }};
        for (var j = 0; j < {HOT}; j++) after = re.test("a");
        console.log(before + "," + after);
        "#
    ));
    assert_eq!(out[0], "true,LATE");
}

#[test]
fn an_accessor_in_the_prototype_slot_wins() {
    // Same VALUE, but reached through a getter: the slot still holds the intrinsic
    // native, so a guard that only compared value bits would wrongly fast-path it.
    let out = run_ok(&format!(
        r#"
        var real = RegExp.prototype.test;
        Object.defineProperty(RegExp.prototype, "test", {{
          get: function () {{ return function () {{ return "VIA-GETTER"; }}; }}, configurable: true
        }});
        var re = /a/, t;
        for (var i = 0; i < {HOT}; i++) t = re.test("a");
        Object.defineProperty(RegExp.prototype, "test", {{ value: real, writable: true, configurable: true }});
        console.log(t + "," + /a/.test("a"));
        "#
    ));
    assert_eq!(out[0], "VIA-GETTER,true");
}

#[test]
fn a_reprototyped_instance_does_not_use_the_arm() {
    let out = run_ok(&format!(
        r#"
        var re = /a/;
        Object.setPrototypeOf(re, {{ test: function () {{ return "OTHER-PROTO"; }} }});
        var t;
        for (var i = 0; i < {HOT}; i++) t = re.test("a");
        console.log(t);
        "#
    ));
    assert_eq!(out[0], "OTHER-PROTO");
}

#[test]
fn test_still_routes_through_a_patched_exec() {
    // `test` is intrinsic but `exec` is not: RegExpExec reads `exec`, so the
    // patched one must run and its result decides the boolean.
    let out = run_ok(&format!(
        r#"
        var calls = 0;
        var re = /zzz/;
        re.exec = function () {{ calls++; return ["forced"]; }};
        var t;
        for (var i = 0; i < {HOT}; i++) t = re.test("no match here");
        console.log(t + " calls=" + calls);
        "#
    ));
    assert_eq!(out[0], format!("true calls={HOT}"));
}

#[test]
fn a_subclass_instance_uses_its_own_override() {
    let out = run_ok(&format!(
        r#"
        class MyRe extends RegExp {{ test(s) {{ return "SUB"; }} }}
        var re = new MyRe("a"), t;
        for (var i = 0; i < {HOT}; i++) t = re.test("a");
        console.log(t + "," + (re instanceof RegExp));
        "#
    ));
    assert_eq!(out[0], "SUB,true");
}

#[test]
fn the_unpatched_fast_path_still_answers_correctly() {
    // The whole point, and the shape the arm exists for: global and non-global,
    // lastIndex advancing, captures, and a miss.
    let out = run_ok(&format!(
        r#"
        var g = /([a-z]+)=(\d+)/g, ng = /([a-z]+)=(\d+)/;
        var s = "a=1 bb=22 ccc=333";
        var hits = 0, caps = "";
        for (var i = 0; i < {HOT}; i++) {{
          g.lastIndex = 0;
          var m;
          while ((m = g.exec(s)) !== null) {{ hits++; if (i === 0) caps += m[1] + ":" + m[2] + " "; }}
        }}
        var t1 = ng.test(s), t2 = /nope/.test(s);
        console.log("hits=" + (hits / {HOT}) + " caps=" + caps.trim() + " t=" + t1 + "," + t2 +
                    " li=" + g.lastIndex);
        "#
    ));
    assert_eq!(out[0], "hits=3 caps=a:1 bb:22 ccc:333 t=true,false li=0");
}
