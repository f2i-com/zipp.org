//! `table[i]()` — a computed member call with a NUMERIC key.
//!
//! The interpreter used to render every computed key into a String and offer it
//! to the builtin-method dispatcher before doing anything else. For `obj["push"]`
//! that is the point. For `table[i]` it is a fresh allocation and a scan of every
//! builtin name, per call, so that the scan can fail: no builtin is named "7".
//!
//! It cost 118.8ns against 44.4ns for `let h = table[i]; h()` — the same call,
//! hand-split. Pocket's Game Boy core carries a comment instructing the next
//! reader to write the split form out in the hot path, which is a bundle being
//! asked to work around its engine.
//!
//! Skipping the scan for numeric keys is only safe if nothing else about the
//! call changes, so what is pinned here is the BEHAVIOUR: receiver binding,
//! string keys still reaching builtins, and the exact property name in the error
//! — which is now formatted lazily, on the failure path only.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

fn run_err(src: &str) -> String {
    let out = zipp_vm::run(src).expect("source compiles");
    out.error.expect("expected a runtime error")
}

#[test]
fn numeric_key_calls_the_element() {
    let out = run_ok(
        r#"
        var T = [function () { return "zero" }, function () { return "one" }];
        console.log(T[0]());
        console.log(T[1]());
        var i = 1;
        console.log(T[i]());
        "#,
    );
    assert_eq!(out, vec!["zero", "one", "one"]);
}

#[test]
fn numeric_key_still_binds_the_receiver() {
    // The whole reason this opcode exists rather than a GET followed by a call.
    let out = run_ok(
        r#"
        var X = [function () { return this.length }];
        console.log(String(X[0]()));
        var O = { 3: function () { return this.tag }, tag: "obj" };
        console.log(O[3]());
        "#,
    );
    assert_eq!(out, vec!["1", "obj"]);
}

#[test]
fn a_string_key_still_reaches_the_builtin() {
    // The skip must key off the VALUE being numeric, not off the receiver being
    // an array — `A["join"]` is an array with a string key and must still work.
    let out = run_ok(
        r#"
        var A = [1, 2, 3];
        console.log(A["join"]("-"));
        var k = "join";
        console.log(A[k]("+"));
        console.log(A["map"](function (x) { return x * 2 })["join"](","));
        "#,
    );
    assert_eq!(out, vec!["1-2-3", "1+2+3", "2,4,6"]);
}

#[test]
fn a_numeric_string_key_is_not_the_numeric_path() {
    // "0" is a string, so it takes the unchanged route and must still find the
    // element rather than a builtin.
    let out = run_ok(
        r#"
        var T = [function () { return "hit" }];
        console.log(T["0"]());
        "#,
    );
    assert_eq!(out, vec!["hit"]);
}

#[test]
fn a_missing_numeric_element_still_names_the_property() {
    // The property name is now rendered only when the call fails. If that were
    // dropped, the message would silently lose the index that explains it.
    let err = run_err(
        r#"
        var T = [function () { return 1 }];
        T[5]();
        "#,
    );
    assert!(err.contains("not a function"), "{err}");
    assert!(err.contains(r#"(property "5")"#), "{err}");
}

#[test]
fn a_non_integer_numeric_key_reports_its_own_text() {
    for (key, shown) in [("1.5", "1.5"), ("0/0", "NaN")] {
        let err = run_err(&format!(
            r#"
            var T = [function () {{ return 1 }}];
            T[{key}]();
            "#
        ));
        assert!(
            err.contains(&format!(r#"(property "{shown}")"#)),
            "key {key} produced: {err}"
        );
    }
}

#[test]
fn a_dispatch_table_runs_the_element_each_index_names() {
    // The shape this exists for: an opcode table indexed by a fetched byte.
    let out = run_ok(
        r#"
        var OPS = [];
        var i = 0;
        while (i < 256) { OPS[i] = (function (n) { return function () { return n } })(i); i = i + 1 }
        var sum = 0;
        var k = 0;
        while (k < 1000) { sum = sum + OPS[k & 255](); k = k + 1 }
        console.log(String(sum));
        "#,
    );
    assert_eq!(out, vec!["124716"]);
}
