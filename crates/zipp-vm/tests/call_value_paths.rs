//! `call_value` decides the common case — a plain JS function — with ONE
//! heap-discriminant load; every exotic receiver (bound / proxy / native /
//! wrapped / resolver / callable object / non-callable) lives in the `#[cold]`
//! `call_value_exotic` cascade behind it, in the cascade's original arm order.
//!
//! Every case below reaches the callee through a call site that lands in
//! `call_value` (Function.prototype.call/apply, Reflect.apply, a direct call
//! on an exotic, or the microtask reaction path), covering each receiver class
//! the cascade distinguishes.
//!
//! Every VALUE-producing expectation was executed in node v24 as a script and
//! matches byte-for-byte. The TypeError MESSAGES are zipp's own and are pinned
//! as such: node names the source expression ("pBad is not a function",
//! "Class constructor K cannot be invoked without 'new'") where zipp displays
//! the VALUE ("[object Object] is not a function", "class K { } is not a
//! function") — asserted against the behavior of the unmodified engine.
//!
//! The whole file produces byte-identical output with
//! `ZIPP_NO_CALLVALUE_FLAT=1` (the old sequential cascade for every callee) —
//! run the suite once in each mode to A/B.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Route a call through `call_value` and report either the result or the
/// caught error as `Ctor: message` — the shape the expectations pin.
const HARNESS: &str = r#"
    function show(fn) {
        try { return String(fn()); }
        catch (e) { return e.constructor.name + ": " + e.message; }
    }
"#;

#[test]
fn a_plain_function_arrow_and_capturing_closure_via_call() {
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        function plain(a) {{ return "plain:" + a; }}
        var arrow = (x) => "arrow:" + x;
        function mk(n) {{ return function (m) {{ return "closure:" + (n + m); }}; }}
        var clo = mk(10);
        console.log(show(() => plain.call(null, 1)));
        console.log(show(() => arrow.call(null, 2)));
        console.log(show(() => clo.call(null, 5)));
        "#
    ));
    assert_eq!(out, vec!["plain:1", "arrow:2", "closure:15"]);
}

#[test]
fn a_bound_function_a_double_bound_one_and_a_bound_native() {
    // bind fixes `this` and prepends args; a second bind cannot re-fix `this`.
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        function whoami(a, b) {{ return "bound:" + this.tag + ":" + a + ":" + b; }}
        var b1 = whoami.bind({{ tag: "T" }}, "x");
        var b2 = b1.bind({{ tag: "IGNORED" }}, "z");
        var bn = Math.max.bind(null, 5);
        console.log(show(() => b1.call(null, "y")));
        console.log(show(() => b2()));
        console.log(show(() => bn.call(null, 3)));
        "#
    ));
    assert_eq!(out, vec!["bound:T:x:y", "bound:T:x:z", "5"]);
}

#[test]
fn a_class_constructor_called_without_new_is_a_typeerror() {
    // node says "Class constructor K cannot be invoked without 'new'"; zipp
    // reports the displayed value — pinned to the unmodified engine's text.
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        class K {{ constructor() {{ this.v = 1; }} }}
        console.log(show(() => K.call(null)));
        console.log(show(() => Reflect.apply(K, null, [])));
        "#
    ));
    assert_eq!(out[0], "TypeError: class K { } is not a function");
    assert_eq!(out[1], "TypeError: class K { } is not a function");
}

#[test]
fn a_proxy_with_an_apply_trap_runs_the_trap() {
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        var p = new Proxy(function () {{ return "target"; }}, {{
            apply: function (t, th, a) {{ return "trap:" + a.join("+"); }}
        }});
        console.log(show(() => p("a", "b")));
        console.log(show(() => p.call(null, "c", "d")));
        "#
    ));
    assert_eq!(out, vec!["trap:a+b", "trap:c+d"]);
}

#[test]
fn a_trapless_proxy_forwards_and_a_non_callable_target_throws() {
    // No apply trap: a callable target is called; a non-callable target is a
    // TypeError (node: "pBad is not a function" — zipp displays the value).
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        var pFwd = new Proxy(function (x) {{ return "fwd:" + x; }}, {{}});
        var pBad = new Proxy({{}}, {{}});
        console.log(show(() => pFwd("e")));
        console.log(show(() => pBad()));
        "#
    ));
    assert_eq!(out[0], "fwd:e");
    assert_eq!(out[1], "TypeError: [object Object] is not a function");
}

#[test]
fn a_native_builtin_via_call() {
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        console.log(show(() => Math.max.call(null, 1, 2)));
        "#
    ));
    assert_eq!(out, vec!["2"]);
}

#[test]
fn non_callable_callees_are_typeerrors_naming_the_value() {
    // `Function.prototype.call.call(x)` forces `x` itself through call_value.
    // node blames the source expression ("callOf.call is not a function") for
    // all four; zipp displays each value — pinned to the unmodified engine.
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        var callOf = Function.prototype.call;
        console.log(show(() => callOf.call(Symbol.iterator)));
        console.log(show(() => callOf.call(7)));
        console.log(show(() => callOf.call(undefined)));
        console.log(show(() => callOf.call(null)));
        "#
    ));
    assert_eq!(
        out,
        vec![
            "TypeError: Symbol(Symbol.iterator) is not a function",
            "TypeError: 7 is not a function",
            "TypeError: undefined is not a function",
            "TypeError: null is not a function",
        ]
    );
}

#[test]
fn a_getter_produced_callee() {
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        var o = {{ get f() {{ return function () {{ return "got"; }}; }} }};
        console.log(show(() => o.f.call(null)));
        console.log(show(() => o.f()));
        "#
    ));
    assert_eq!(out, vec!["got", "got"]);
}

#[test]
fn function_prototype_call_and_apply_chains() {
    // `call.call` / `apply.apply`: the OUTER native's `this` is the inner
    // native, which then re-enters call_value with the real function.
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        function plain(a) {{ return "plain:" + a; }}
        console.log(show(() => plain.call.call(plain, null, "cc")));
        console.log(show(() => plain.apply.apply(plain, [null, ["aa"]])));
        "#
    ));
    assert_eq!(out, vec!["plain:cc", "plain:aa"]);
}

#[test]
fn a_generator_function_builds_a_suspended_generator() {
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        function* gen() {{ yield "genval"; }}
        console.log(show(() => gen.call(null).next().value));
        "#
    ));
    assert_eq!(out, vec!["genval"]);
}

#[test]
fn builtin_constructor_objects_called_as_functions_coerce() {
    // String/Number are `is_ctor` Objects: calling them (indirectly, so the
    // compiler cannot lower it) coerces instead of constructing.
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        console.log(show(() => String.call(null, 123)));
        console.log(show(() => Number.call(null, "42")));
        "#
    ));
    assert_eq!(out, vec!["123", "42"]);
}

#[test]
fn function_prototype_itself_is_callable_and_returns_undefined() {
    let out = run_ok(&format!(
        r#"
        {HARNESS}
        console.log(show(() => Function.prototype()));
        "#
    ));
    assert_eq!(out, vec!["undefined"]);
}

#[test]
fn async_functions_then_callbacks_and_thenable_jobs_dispatch() {
    // The microtask reaction path: every `.then` callback and every thenable's
    // `then` goes through call_value FROM RUST (run_microtask), which is the
    // path the flattening exists for. Async ordering is part of the pin.
    let out = run_ok(
        r#"
        async function af() { return 7; }
        af.call(null).then(function (v) { console.log("af:" + v); });
        Promise.resolve(41).then(function (x) { return x + 1; }).then(function (v) {
            console.log("then:" + v);
        });
        // A non-callable onFulfilled is IGNORED per spec (no throw).
        Promise.resolve("pass").then(5).then(function (v) { console.log(v); });
        // PromiseResolveThenableJob calls `then` through call_value.
        var thenable = { then: function (res, rej) { res("thenable-ok"); } };
        Promise.resolve().then(function () { return thenable; }).then(function (v) {
            console.log(v);
        });
        console.log("sync-first");
        "#,
    );
    assert_eq!(
        out,
        vec!["sync-first", "af:7", "then:42", "pass", "thenable-ok"]
    );
}
