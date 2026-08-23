//! Exactness coverage for the adjacent GetIndex + proven-linear append fusion.
//! The fast prefix is deliberately tiny (mutable flat accumulator, flat ASCII
//! receiver, in-range tagged-int key); every miss must execute the original
//! indexed read and append once, including user coercion and GC safe points.

use std::process::Command;

use zipp_vm::{compile_to_text, run};

const COPY_SOURCE: &str = r#"
    "use strict";
    function copy(s) {
        var out = "";
        for (var i = 0; i < s.length; i++) out += s[i];
        return out;
    }
"#;

fn run_ok(src: &str) -> Vec<String> {
    let outcome = run(src).expect("source compiles");
    assert!(
        outcome.error.is_none(),
        "runtime error: {:?}",
        outcome.error
    );
    outcome.output
}

#[test]
fn parity_ascii_unicode_wtf8_and_interned_first_append() {
    let src = format!(
        r#"
        {COPY_SOURCE}
        var ascii = "";
        for (var j = 0; j < 128; j++) ascii += String.fromCharCode(j);
        var got = copy(ascii), sum = 0;
        for (var k = 0; k < got.length; k++) sum += got.charCodeAt(k);
        console.log(got.length, sum, got.charCodeAt(0), got.charCodeAt(127));

        var odd = "A😀\uD800B\uDC00";
        var oddCopy = copy(odd), units = [];
        for (var q = 0; q < oddCopy.length; q++) units.push(oddCopy.charCodeAt(q));
        console.log(oddCopy === odd, oddCopy.length, units.join(","));
        "#
    );
    assert_eq!(
        run_ok(&src),
        ["128 8128 0 127", "true 6 65,55357,56832,55296,66,56320",]
    );
}

#[test]
fn parity_generic_get_key_coercion_object_coercion_and_symbol_throw() {
    let src = format!(
        r#"
        {COPY_SOURCE}
        var gets = 0, strings = 0;
        var indexed = {{}};
        Object.defineProperty(indexed, "0", {{ get: function () {{
            gets++;
            return {{ toString: function () {{
                strings++;
                var garbage = [];
                for (var g = 0; g < 8; g++) garbage.push({{ n: g }});
                return "x";
            }} }};
        }} }});
        function objectValues(o) {{
            var out = "";
            for (var i = 0; i < 40; i++) out += o[0];
            return out;
        }}
        var objectOut = objectValues(indexed);
        console.log(objectOut.length, gets, strings, objectOut === "x".repeat(40));

        var keyCalls = 0, valueGets = 0;
        var key = {{ toString: function () {{ keyCalls++; return "field"; }} }};
        var keyed = {{ get field() {{ valueGets++; return "q"; }} }};
        function objectKeys(o, key) {{
            var out = "";
            for (var i = 0; i < 24; i++) out += o[key];
            return out;
        }}
        var keyOut = objectKeys(keyed, key);
        console.log(keyOut.length, keyCalls, valueGets, keyOut === "q".repeat(24));

        var symbolGets = 0;
        var symbols = {{ get 0() {{ symbolGets++; return Symbol("x"); }} }};
        function appendSymbol(o) {{
            var out = "";
            for (var i = 0; i < 2; i++) out += o[0];
            return out;
        }}
        try {{ appendSymbol(symbols); }} catch (e) {{
            console.log(e.name, symbolGets);
        }}
        "#
    );
    assert_eq!(
        run_ok(&src),
        ["40 40 40 true", "24 24 24 true", "TypeError 1"]
    );
}

#[test]
fn shared_accumulator_is_not_licensed_for_fusion() {
    let src = r#"
        "use strict";
        function retained(input) {
            var out = "seed", held = out, snap = "";
            for (var i = 0; i < input.length; i++) {
                out += input[i];
                if (i === 10) snap = out;
            }
            return held + "|" + snap + "|" + out.length + "|" + out.slice(-4);
        }
        console.log(retained("ab".repeat(250)));
    "#;
    let bc = compile_to_text(src, false).expect("source compiles");
    assert!(
        !bc.contains("StrAppendIndex"),
        "a retained accumulator must not receive the mutating fusion:\n{bc}"
    );
    assert_eq!(run_ok(src), ["seed|seedabababababa|504|abab"]);
}

/// The compiler gate is process-latched, so inspect ON and OFF bytecode in
/// fresh children. OFF must retain both historical operations verbatim.
#[test]
fn bytecode_shape_child() {
    let Some(mode) = std::env::var_os("ZIPP_APPEND_INDEX_BC_CHILD") else {
        return;
    };
    let bc = compile_to_text(COPY_SOURCE, false).expect("source compiles");
    if mode == "on" {
        assert!(bc.contains("StrAppendIndex"), "fusion absent:\n{bc}");
    } else {
        assert!(!bc.contains("StrAppendIndex"), "off switch ignored:\n{bc}");
        assert!(
            bc.contains("GetIndex"),
            "historical indexed read absent:\n{bc}"
        );
        assert!(
            bc.contains("StrAppendInPlace"),
            "historical append absent:\n{bc}"
        );
    }
}

#[test]
fn bytecode_on_and_off_switch_are_exact() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, off) in [("on", false), ("off", true)] {
        let mut cmd = Command::new(&exe);
        cmd.args(["bytecode_shape_child", "--exact"])
            .env("ZIPP_APPEND_INDEX_BC_CHILD", mode)
            .env_remove("ZIPP_NO_APPEND_INDEX_FUSE");
        if off {
            cmd.env("ZIPP_NO_APPEND_INDEX_FUSE", "1");
        }
        let out = cmd.output().expect("spawn bytecode child");
        assert!(
            out.status.success(),
            "{mode} bytecode child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Exercise the fallback under interpreter-only execution, immediate native
/// compilation, GC at every safe point, and the exact unfused bytecode.
#[test]
fn all_execution_modes_preserve_answers() {
    if std::env::var_os("ZIPP_APPEND_INDEX_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("hot", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
        ("off", &[("ZIPP_NO_APPEND_INDEX_FUSE", "1")][..]),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.arg("parity_")
            .env("ZIPP_APPEND_INDEX_MODE_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_APPEND_INDEX_FUSE");
        for &(key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && !stdout.contains("running 0 tests"),
            "{mode} child failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
