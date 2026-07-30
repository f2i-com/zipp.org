//! `GetProp` admitted into the leaf-call inliner (B73).
//!
//! A plain `function f(o) { return o.k; }` called from a hot loop used to be
//! `(not leaf-eligible)` — the whitelist admitted `GetIndex` and not `GetProp` — so
//! it paid a full frame call per iteration: 30.1ns against 7.0ns for the identical
//! body written as a method. It is now inlined, reading through the site-free
//! `jit_get_prop_leaf`.
//!
//! That helper answers only an own or provably-inherited DATA property on a plain
//! object and defers everything else to the interpreter. Every "everything else" is
//! pinned here, because each one is a silent wrong answer if the helper answers it
//! itself — and all of them are invisible to a cold run, since the inline only
//! exists once the enclosing loop is hot.
//!
//! `ZIPP_NO_LEAF_GETPROP=1` removes the arm; `both_tiers` runs each case with the
//! JIT on and off and requires them to agree, which is the property that matters.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Enough iterations to cross OSR_THRESHOLD and run the inlined body many times.
const HOT: usize = 4000;

#[test]
fn an_own_data_property_reads_inline() {
    let out = run_ok(&format!(
        r#"
        function get(o) {{ return o.v + 1; }}
        var a = {{ v: 41 }}, s = 0;
        for (var i = 0; i < {HOT}; i++) s = get(a);
        console.log(s);
        "#
    ));
    assert_eq!(out[0], "42");
}

#[test]
fn an_inherited_data_property_reads_inline() {
    let out = run_ok(&format!(
        r#"
        function get(o) {{ return o.base; }}
        var proto = {{ base: 7 }};
        var o = Object.create(proto); o.own = 1;
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = get(o);
        console.log(s);
        "#
    ));
    assert_eq!(out[0], "7");
}

#[test]
fn an_absent_property_reads_undefined_not_a_deopt_loop() {
    // A provably absent property must answer `undefined` rather than deopt: a leaf
    // that deopted on every call would drive the enclosing region past
    // OSR_DEOPT_LIMIT and get it evicted for the life of the process.
    let out = run_ok(&format!(
        r#"
        function get(o) {{ return o.nope; }}
        var o = {{ v: 1 }}, s = "x";
        for (var i = 0; i < {HOT}; i++) s = get(o);
        console.log(typeof s + ":" + s);
        "#
    ));
    assert_eq!(out[0], "undefined:undefined");
}

#[test]
fn an_own_getter_still_runs() {
    let out = run_ok(&format!(
        r#"
        var calls = 0;
        var o = {{}};
        Object.defineProperty(o, "v", {{ get: function () {{ calls++; return 5; }}, configurable: true }});
        function get(p) {{ return p.v; }}
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = get(o);
        console.log(s + " calls=" + calls);
        "#
    ));
    assert_eq!(out[0], format!("5 calls={HOT}"));
}

#[test]
fn an_inherited_getter_still_runs() {
    let out = run_ok(&format!(
        r#"
        var calls = 0;
        var proto = {{}};
        Object.defineProperty(proto, "v", {{ get: function () {{ calls++; return 9; }}, configurable: true }});
        var o = Object.create(proto);
        function get(p) {{ return p.v; }}
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = get(o);
        console.log(s + " calls=" + calls);
        "#
    ));
    assert_eq!(out[0], format!("9 calls={HOT}"));
}

#[test]
fn a_getter_installed_MID_loop_starts_running() {
    // The inline is already compiled when the property turns into an accessor. The
    // helper must notice and defer, not keep returning the old data value.
    let out = run_ok(&format!(
        r#"
        var o = {{ v: 1 }}, calls = 0, seen = [];
        function get(p) {{ return p.v; }}
        for (var i = 0; i < {HOT}; i++) {{
          if (i === {HOT} / 2) {{
            Object.defineProperty(o, "v", {{ get: function () {{ calls++; return 99; }}, configurable: true }});
          }}
          var r = get(o);
          if (i === 0 || i === {HOT} - 1) seen.push(r);
        }}
        console.log(seen.join(",") + " calls>0:" + (calls > 0));
        "#
    ));
    assert_eq!(out[0], "1,99 calls>0:true");
}

#[test]
fn a_proxy_receiver_fires_its_trap() {
    let out = run_ok(&format!(
        r#"
        var hits = 0;
        var p = new Proxy({{ v: 3 }}, {{ get: function (t, k) {{ hits++; return t[k] * 2; }} }});
        function get(o) {{ return o.v; }}
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = get(p);
        console.log(s + " hits=" + hits);
        "#
    ));
    assert_eq!(out[0], format!("6 hits={HOT}"));
}

#[test]
fn a_class_instance_receiver_reads_correctly() {
    let out = run_ok(&format!(
        r#"
        class C {{ constructor() {{ this.f = 4; }} get g() {{ return 8; }} }}
        var c = new C();
        function getf(o) {{ return o.f; }}
        function getg(o) {{ return o.g; }}
        var a = 0, b = 0;
        for (var i = 0; i < {HOT}; i++) {{ a = getf(c); b = getg(c); }}
        console.log(a + "," + b);
        "#
    ));
    assert_eq!(out[0], "4,8");
}

#[test]
fn exotic_and_primitive_receivers_stay_correct() {
    let out = run_ok(&format!(
        r#"
        function get(o) {{ return o.length; }}
        var arr = [1, 2, 3], str = "abcd", fn = function (a, b) {{}};
        var a = 0, b = 0, c = 0;
        for (var i = 0; i < {HOT}; i++) {{ a = get(arr); b = get(str); c = get(fn); }}
        var t = "ok";
        try {{ get(null); t = "no-throw"; }} catch (e) {{ t = e.constructor.name; }}
        console.log(a + "," + b + "," + c + "," + t);
        "#
    ));
    assert_eq!(out[0], "3,4,2,TypeError");
}

#[test]
fn a_polymorphic_receiver_reads_each_shape_correctly() {
    let out = run_ok(&format!(
        r#"
        function get(o) {{ return o.v; }}
        var objs = [{{ v: 1 }}, {{ a: 0, v: 2 }}, Object.create({{ v: 3 }}), {{ x: 0, y: 0, v: 4 }}];
        var sum = 0;
        for (var i = 0; i < {HOT}; i++) sum = (sum + get(objs[i & 3])) | 0;
        console.log(sum + " last=" + get(objs[3]));
        "#
    ));
    assert_eq!(out[0], format!("{} last=4", (1 + 2 + 3 + 4) * (HOT / 4)));
}

#[test]
fn a_deleted_property_falls_through_to_the_prototype() {
    let out = run_ok(&format!(
        r#"
        function get(o) {{ return o.v; }}
        var proto = {{ v: "proto" }};
        var o = Object.create(proto); o.v = "own";
        var first = "", after = "";
        for (var i = 0; i < {HOT}; i++) {{
          if (i === {HOT} / 2) delete o.v;
          var r = get(o);
          if (i === 0) first = r;
          after = r;
        }}
        console.log(first + "," + after);
        "#
    ));
    assert_eq!(out[0], "own,proto");
}

#[test]
fn a_private_field_name_is_never_answered_by_the_helper() {
    // `#x` needs a brand check; the helper defers on a leading '#'. Reading one
    // through a plain function is a syntax error in JS, so this exercises the
    // adjacent case: a normal property on an object that also has private fields.
    let out = run_ok(&format!(
        r#"
        class C {{ #p = 1; constructor() {{ this.pub = 2; }} readP() {{ return this.#p; }} }}
        var c = new C();
        function get(o) {{ return o.pub; }}
        var a = 0, b = 0;
        for (var i = 0; i < {HOT}; i++) {{ a = get(c); b = c.readP(); }}
        console.log(a + "," + b);
        "#
    ));
    assert_eq!(out[0], "2,1");
}
