//! Correctness coverage for the one-argument region/Tier-C
//! `String.prototype.substring` / `slice` intrinsic.
//!
//! `ZIPP_NO_SUBSTRING1_INTRINSIC=1` deliberately restores the generic call
//! path; every expectation here must remain identical with that switch and
//! with `ZIPP_NOJIT=1`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Well past JIT_THRESHOLD/OSR_THRESHOLD (both 8).
const HOT: usize = 3000;

#[test]
fn one_arg_ascii_calls_make_tier_c_eligible() {
    // Repeated calls cross the whole-function threshold as well as the inner
    // loop's OSR threshold. With the kill switch this function is deliberately
    // rejected by Tier C and continues through the generic MEM-region call.
    let out = run_ok(
        r#"
        "use strict";
        function cut(s, n) {
          var r = "";
          for (var i = 0; i < n; i++) r = s.substring(1);
          return r;
        }
        var result = "";
        for (var k = 0; k < 40; k++) result = cut("abcdef", 40);
        console.log(result);
        "#,
    );
    assert_eq!(out, ["bcdef"]);
}

#[test]
fn ascii_clamping_and_missing_end_match_the_interpreter() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        function sub(s, start, n) {{
          var r = "";
          for (var i = 0; i < n; i++) r = s.substring(start);
          return r;
        }}
        function slice(s, start, n) {{
          var r = "";
          for (var i = 0; i < n; i++) r = s.slice(start);
          return r;
        }}
        var integralDouble = 6 / 3;
        var fractional = 5 / 2;
        console.log(
          sub("abcdef", 2, {HOT}) + "|" +
          sub("abcdef", -2, {HOT}) + "|" +
          sub("abcdef", 99, {HOT}) + "|" +
          sub("abcdef", integralDouble, {HOT}) + "|" +
          sub("abcdef", fractional, {HOT})
        );
        console.log(
          slice("abcdef", 2, {HOT}) + "|" +
          slice("abcdef", -2, {HOT}) + "|" +
          slice("abcdef", -99, {HOT}) + "|" +
          slice("abcdef", 99, {HOT}) + "|" +
          slice("abcdef", integralDouble, {HOT}) + "|" +
          slice("abcdef", fractional, {HOT})
        );
        "#
    ));
    assert_eq!(out, ["cdef|abcdef||cdef|cdef", "cdef|ef|abcdef||cdef|cdef"]);
}

#[test]
fn declined_receivers_and_arguments_run_observable_fallbacks_once() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        var coerces = 0;
        var start = {{ valueOf: function () {{ coerces++; return 2; }} }};
        function coerced(n) {{
          var r = "";
          for (var i = 0; i < n; i++) r = "abcdef".substring(start);
          return r;
        }}

        function unicode(n) {{
          var r = "";
          for (var i = 0; i < n; i++) r = "A💩B".substring(2);
          return r.length + "," + r.charCodeAt(0) + "," + r.charCodeAt(1);
        }}

        var calls = 0;
        var receiver = {{
          base: 10,
          substring: function (x) {{ calls++; return this.base + x; }}
        }};
        function custom(n) {{
          var r = 0;
          for (var i = 0; i < n; i++) r = receiver.substring(i & 1);
          return r;
        }}

        function explicitUndefined(n) {{
          var r = "";
          for (var i = 0; i < n; i++) r = "abcdef".substring(2, undefined);
          return r;
        }}

        console.log(coerced({HOT}) + "," + coerces);
        console.log(unicode({HOT}));
        console.log(custom({HOT}) + "," + calls);
        console.log(explicitUndefined({HOT}));
        "#
    ));
    assert_eq!(
        out,
        [
            format!("cdef,{HOT}"),
            "2,56489,66".into(),
            format!("11,{HOT}"),
            "cdef".into(),
        ]
    );
}

#[test]
fn result_allocations_keep_later_inline_cache_version_reads_valid() {
    // More than Heap::GC_MIN_THRESHOLD successful slices in one native loop:
    // the result allocations repeatedly grow (and can relocate) Heap::versions.
    // The following `o.x` access consumes the region/Tier-C r13 version pin.
    let out = run_ok(
        r#"
        "use strict";
        function stress(s, o, n) {
          var acc = 0, tail = "";
          for (var i = 0; i < n; i++) {
            tail = (i & 1) ? s.substring(1) : s.slice(1);
            acc = (acc + o.x + tail.length) | 0;
          }
          return acc + ":" + tail;
        }
        console.log(stress("abcdef", { x: 3 }, 90000));
        "#,
    );
    assert_eq!(out, ["720000:bcdef"]);
}
