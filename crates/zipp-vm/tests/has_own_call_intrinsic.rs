//! Guard and fallback coverage for the region-JIT
//! `Object.prototype.hasOwnProperty.call(array, numericKey)` intrinsic.
//!
//! The same cases are expected to be byte-identical with the intrinsic enabled,
//! with `ZIPP_NO_HASOWN_CALL_INTRINSIC=1`, and with `ZIPP_NOJIT=1`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Well past JIT_THRESHOLD/OSR_THRESHOLD (both 8).
const HOT: usize = 3000;

#[test]
fn dense_holes_and_numeric_overlays_match_has_own_property() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        var hop = Object.prototype.hasOwnProperty;
        var a = [10, , 30];
        Object.defineProperty(a, "5", {{ value: 50, enumerable: false }});
        var hits = 0;
        for (var i = 0; i < {HOT}; i++) {{
          if (hop.call(a, i % 6)) hits++;
        }}
        console.log(hits);
        console.log(
          hop.call(a, -0) + "," +
          hop.call(a, 3.5) + "," +
          hop.call(a, -1) + "," +
          hop.call(a, "02") + "," +
          hop.call(a, 99)
        );
        "#
    ));
    assert_eq!(out, ["1500", "true,false,false,false,false"]);
}

#[test]
fn a_different_function_dot_call_uses_the_generic_fallback() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        function add(x) {{ return this.base + x; }}
        var receiver = {{ base: 7 }};
        var sum = 0;
        for (var i = 0; i < {HOT}; i++) {{
          sum += add.call(receiver, i & 1);
        }}
        console.log(sum);
        "#
    ));
    assert_eq!(out, ["22500"]);
}

#[test]
fn a_plain_object_target_uses_the_generic_fallback() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        var hop = Object.prototype.hasOwnProperty;
        var o = {{ 0: "zero" }};
        Object.defineProperty(o, "2", {{ value: "two", enumerable: false }});
        var hits = 0;
        for (var i = 0; i < {HOT}; i++) {{
          if (hop.call(o, i & 3)) hits++;
        }}
        console.log(hits);
        "#
    ));
    assert_eq!(out, ["1500"]);
}

#[test]
fn fallback_observes_key_coercion_and_proxy_traps_once() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        var hop = Object.prototype.hasOwnProperty;
        var a = [1];

        var keyCoercions = 0, keyHits = 0;
        var key = {{ toString: function () {{ keyCoercions++; return "0"; }} }};
        for (var i = 0; i < {HOT}; i++) {{
          if (hop.call(a, key)) keyHits++;
        }}

        var traps = 0, proxyHits = 0;
        var p = new Proxy(a, {{
          getOwnPropertyDescriptor: function (target, prop) {{
            traps++;
            return Object.getOwnPropertyDescriptor(target, prop);
          }}
        }});
        for (var i = 0; i < {HOT}; i++) {{
          if (hop.call(p, 0)) proxyHits++;
        }}

        console.log(keyHits + "," + keyCoercions);
        console.log(proxyHits + "," + traps);
        "#
    ));
    assert_eq!(out, ["3000,3000", "3000,3000"]);
}
