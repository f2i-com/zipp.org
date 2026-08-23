//! Node-parity boundaries for the guarded sparse/prototype `jit_get_index` lane.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn sparse_own_data_is_read_live_across_overwrite_delete_and_define() {
    let out = run_ok(
        r#"
        "use strict";
        function hot(a, i) { var v; for (var r = 0; r < 40000; r++) v = a[i]; return String(v); }
        var a = []; a.length = 50000000; a[1048581] = 7;
        var r = [hot(a, 1048581)];
        a[1048581] = 9; r.push(hot(a, 1048581));
        delete a[1048581]; r.push(hot(a, 1048581));
        Object.defineProperty(a, "1048581", { value: 11, writable: true, configurable: true });
        r.push(hot(a, 1048581));
        console.log(r.join(","));
        "#,
    );
    assert_eq!(out, ["7,9,undefined,11"]);
}

#[test]
fn compiled_absent_read_observes_both_default_prototype_anchors_live() {
    let out = run_ok(
        r#"
        "use strict";
        function hot(a, i) { var v; for (var r = 0; r < 40000; r++) v = a[i]; return String(v); }
        var a = []; a.length = 100;
        var r = [hot(a, 23)];
        Array.prototype[23] = "A"; r.push(hot(a, 23));
        Array.prototype[23] = "B"; r.push(hot(a, 23));
        Object.prototype[23] = "O"; delete Array.prototype[23]; r.push(hot(a, 23));
        delete Object.prototype[23]; r.push(hot(a, 23));
        console.log(r.join(","));
        "#,
    );
    assert_eq!(out, ["undefined,A,B,O,undefined"]);
}

#[test]
fn accessors_custom_prototypes_and_proxy_gets_never_take_the_data_lane() {
    let out = run_ok(
        r#"
        "use strict";
        function hot(a, i, n) { var v; for (var r = 0; r < n; r++) v = a[i]; return String(v); }
        var ownHits = 0, own = []; own.length = 100;
        Object.defineProperty(own, "17", { get: function () { ownHits++; return "G"; }, configurable: true });

        var protoHits = 0;
        Object.defineProperty(Array.prototype, "19", { get: function () { protoHits++; return "P"; }, configurable: true });

        var customHits = 0, custom = { get 21() { customHits++; return "C"; } };
        var ca = []; Object.setPrototypeOf(ca, custom);

        var proxyHits = 0;
        var pa = []; Object.setPrototypeOf(pa, new Proxy({}, { get: function (t, k) { proxyHits++; return k === "25" ? "X" : undefined; } }));

        var vals = [hot(own,17,40), hot([],19,40), hot(ca,21,40), hot(pa,25,40)];
        delete Array.prototype[19];
        console.log(vals.join(",") + "|" + [ownHits,protoHits,customHits,proxyHits].join(","));
        "#,
    );
    assert_eq!(out, ["G,P,C,X|40,40,40,40"]);
}

#[test]
fn mapped_arguments_alias_is_not_treated_as_array_storage() {
    let out = run_ok(
        r#"
        function hot(a) { var v; for (var r = 0; r < 40000; r++) v = a[0]; return String(v); }
        function f(x) {
          var a = hot(arguments);
          x = 9;
          var b = hot(arguments);
          arguments[0] = 13;
          var c = hot(arguments);
          return a + "," + b + "," + c + "," + x;
        }
        console.log(f(5));
        "#,
    );
    assert_eq!(out, ["5,9,13,13"]);
}

/// Both optimizations are process-latched. Re-running the same test binary with
/// them disabled proves the old formatting/hash/deopt paths remain equivalent.
#[test]
fn zz_off_switches_agree() {
    if std::env::var_os("ZIPP_SPARSE_GET_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--skip", "zz_off_switches_agree"])
        .env("ZIPP_NO_SPARSE_NUM_INDEX", "1")
        .env("ZIPP_NO_JIT_SPARSE_GET", "1")
        .env("ZIPP_SPARSE_GET_CHILD", "1")
        .output()
        .expect("re-run test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains(" 0 passed"),
        "off paths diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
}
