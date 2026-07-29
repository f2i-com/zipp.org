//! `typeof` returns one of eight PERMANENTLY INTERNED strings rather than
//! allocating a fresh one per evaluation.
//!
//! The unfused `TypeOf` op used to do `alloc_str(type_of(v).to_string())` — a
//! `String` malloc, a `JsStr` ASCII scan and a heap slot every time — measured at
//! 65ns against the fused `TypeOfIs` form's 4ns. Sharing one handle per result is
//! sound because heap strings are immutable and a primitive's identity is not
//! observable, but that soundness is exactly what these tests pin: the shared
//! handle must behave as an ordinary independent string in every observable way.
//!
//! See PERF_ROADMAP B62.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn every_typeof_result_is_correct() {
    let out = run_ok(
        r#"
        var vals = [1, 2.5, NaN, -0, "s", "", true, false, undefined, null, {}, [],
                    function(){}, class C{}, Symbol("k"), 10n, Math, new Date(0),
                    /re/, new Map(), Promise.resolve(1)];
        var out = [];
        for (var i = 0; i < vals.length; i++) out.push(typeof vals[i]);
        console.log(out.join(","));
        "#,
    );
    assert_eq!(
        out[0],
        "number,number,number,number,string,string,boolean,boolean,undefined,object,\
         object,object,function,function,symbol,bigint,object,object,object,object,object"
    );
}

#[test]
fn a_shared_result_behaves_as_an_ordinary_string() {
    // If the interned handle leaked its shared-ness, one of these would differ.
    let out = run_ok(
        r#"
        var t = typeof 1;
        console.log([t.length, t[0], t.toUpperCase(), t + "!", t.slice(1, 3),
                     t.charCodeAt(0), t.indexOf("mb"), JSON.stringify(t)].join("|"));
        "#,
    );
    // indexOf("mb") is 2: "number" is n-u-m-b-e-r, so "mb" starts at the `m`.
    assert_eq!(out[0], "6|n|NUMBER|number!|um|110|2|\"number\"");
}

#[test]
fn concatenating_one_holder_does_not_disturb_another() {
    // Strings are immutable, so `+=` must rebind rather than mutate the shared
    // slot. A mutating implementation would corrupt every later `typeof`.
    let out = run_ok(
        r#"
        var a = typeof 1, b = typeof 2;
        a += "X";
        console.log([a, b, typeof 3].join("|"));
        "#,
    );
    assert_eq!(out[0], "numberX|number|number");
}

#[test]
fn equality_holds_in_both_directions_and_against_literals() {
    let out = run_ok(
        r#"
        console.log([typeof 1 === typeof 2, typeof 1 === "number",
                     "number" === typeof 1, typeof 1 === typeof "s",
                     typeof 1 == "number", (typeof 1).valueOf() === "number"].join(","));
        "#,
    );
    assert_eq!(out[0], "true,true,true,false,true,true");
}

#[test]
fn results_work_as_object_and_map_keys() {
    // A shared handle must hash and compare by CONTENT wherever keys are compared.
    let out = run_ok(
        r#"
        var m = new Map();
        m.set(typeof 1, "n"); m.set(typeof "s", "s"); m.set(typeof 1, "n2");
        var o = {}; o[typeof true] = 1; o[typeof undefined] = 2;
        console.log([m.size, m.get("number"), m.get("string"),
                     o["boolean"], o["undefined"], Object.keys(o).join("+")].join("|"));
        "#,
    );
    assert_eq!(out[0], "2|n2|s|1|2|boolean+undefined");
}

#[test]
fn typeof_an_undeclared_name_still_does_not_throw() {
    let out = run_ok(r#"console.log(typeof someNameThatIsNotDeclaredAnywhere);"#);
    assert_eq!(out[0], "undefined");
}

#[test]
fn a_hot_loop_agrees_with_the_interpreter() {
    // Crosses the JIT thresholds so the Tier C `jit_typeof` helper and the
    // interpreter arm both run; they share `typeof_value`, so a divergence here
    // would mean one of them kept its own materialization.
    let out = run_ok(
        r#"
        var vals = [1, "s", true, null, {}, undefined, function(){}, Symbol("x")];
        var counts = {};
        for (var i = 0; i < 300000; i++) {
          var t = typeof vals[i & 7];
          counts[t] = (counts[t] || 0) + 1;
        }
        var ks = Object.keys(counts).sort();
        var out = [];
        for (var j = 0; j < ks.length; j++) out.push(ks[j] + "=" + counts[ks[j]]);
        console.log(out.join(","));
        "#,
    );
    // 300000 / 8 = 37500 of each of the 8 values; null and {} both give "object".
    assert_eq!(
        out[0],
        "boolean=37500,function=37500,number=37500,object=75000,string=37500,symbol=37500,undefined=37500"
    );
}

#[test]
fn the_fused_and_unfused_forms_agree() {
    // `TypeOfIs` compares the `&'static str` while `TypeOf` now yields an interned
    // handle. Both must answer identically for every result, including the
    // never-matching literal that compiles to code 255.
    let out = run_ok(
        r#"
        var vals = [1, "s", true, undefined, null, function(){}, Symbol("x"), 1n];
        var same = true, sawFalse = false;
        for (var i = 0; i < vals.length; i++) {
          var v = vals[i];
          var t = typeof v;
          for (var j = 0; j < 8; j++) {
            var lit = ["number","string","boolean","undefined","object","function","symbol","bigint"][j];
            var fused = (typeof v === lit);
            var unfused = (t === lit);
            if (fused !== unfused) same = false;
            if (!fused) sawFalse = true;
          }
          if ((typeof v === "not-a-typeof-result") !== (t === "not-a-typeof-result")) same = false;
        }
        console.log(same + " " + sawFalse);
        "#,
    );
    assert_eq!(out[0], "true true");
}
