//! Node-parity coverage for the versioned `ForInKeys` snapshot guard.
//!
//! A version hit may skip only the per-key presence lookup. Every operation
//! below can make a key captured by the snapshot disappear, so each case pins
//! the fallback boundary. The final test re-runs this binary with the guard off
//! to prove that both paths produce the same observable result.

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
fn dense_mutators_invalidate_the_receiver_version() {
    let out = run_ok(
        r#"
        "use strict";
        function walk(a, mutate) {
          var out = [], n = 0;
          for (var k in a) {
            out.push(k);
            if (n++ === 0) mutate(a);
          }
          return out.join(",");
        }
        var r = [];
        r.push(walk([0, 1, 2, 3], function (a) { a.pop(); }));
        r.push(walk([0, 1, 2, 3], function (a) { a.shift(); }));
        r.push(walk([0, , 2, 3], function (a) { a.reverse(); }));
        r.push(walk([0, , 2], function (a) { a.unshift(9); }));
        r.push(walk([0, , 2, 3], function (a) { a.splice(1, 2); }));
        console.log(r.join("|"));
        "#,
    );
    // node v24.12.0
    assert_eq!(out, ["0,1,2|0,1,2|0,3|0|0"]);
}

#[test]
fn default_prototype_versions_and_receiver_reprototype_are_live() {
    let out = run_ok(
        r#"
        "use strict";
        function walk(a, mutate) {
          var out = [], n = 0;
          for (var k in a) {
            out.push(k + "=" + a[k]);
            if (n++ === 0) mutate(a);
          }
          return out.join(",");
        }
        var r = [];

        Array.prototype.tail = "A";
        r.push(walk([0], function () { delete Array.prototype.tail; }));

        Object.prototype.deep = "O";
        r.push(walk([0], function () { delete Object.prototype.deep; }));

        Array.prototype.tail = "A";
        var a = [0];
        r.push(walk(a, function (x) { Object.setPrototypeOf(x, null); }));
        delete Array.prototype.tail;

        Object.prototype.same = "O";
        Array.prototype.same = "A";
        r.push(walk([0], function () { delete Array.prototype.same; }));
        delete Object.prototype.same;

        console.log(r.join("|"));
        "#,
    );
    // The last walk keeps `same`: after the nearer property is deleted, the
    // same snapshotted name is still present on Object.prototype.
    // node v24.12.0
    assert_eq!(out, ["0=0|0=0|0=0|0=0,same=O"]);
}

#[test]
fn nested_snapshot_does_not_replace_the_outer_guard() {
    let out = run_ok(
        r#"
        "use strict";
        var a = [10, 11, 12];
        var outer = [], inner = [];
        for (var k in a) {
          outer.push(k);
          if (k === "0") {
            var b = [20, 21];
            for (var q in b) {
              inner.push(q);
              if (q === "0") delete a[2];
            }
          }
        }
        console.log(outer.join(",") + "|" + inner.join(","));
        "#,
    );
    // node v24.12.0
    assert_eq!(out, ["0,1|0,1"]);
}

#[test]
fn jit_version_misses_fall_back_to_live_presence() {
    let out = run_ok(
        r#"
        "use strict";
        function popCase() {
          var a = [];
          for (var i = 0; i < 16; i++) a[i] = i;
          var n = 0;
          for (var k in a) { n++; if (k === "0") a.pop(); }
          return n;
        }
        function reverseCase() {
          var a = [];
          for (var i = 0; i < 16; i += 2) a[i] = i;
          a.length = 16;
          var n = 0;
          for (var k in a) { n++; if (k === "0") a.reverse(); }
          return n;
        }
        function protoCase() {
          Array.prototype.tail = 1;
          var a = [];
          for (var i = 0; i < 16; i++) a[i] = i;
          var n = 0;
          for (var k in a) { n++; if (k === "0") delete Array.prototype.tail; }
          return n;
        }
        var sum = 0;
        for (var r = 0; r < 400; r++) sum += popCase() + reverseCase() + protoCase();
        console.log(String(sum));
        "#,
    );
    // Repetition crosses the region threshold: the native prefix-version
    // probe must branch to the shared presence lookup after every mutation.
    // node v24.12.0
    assert_eq!(out, ["12800"]);
}

#[test]
fn stable_sparse_walk_keeps_every_snapshotted_key() {
    let out = run_ok(
        r#"
        "use strict";
        var a = [];
        a.length = 500000;
        for (var i = 0; i < a.length; i += 500) a[i] = (i % 97) + 1;
        var count = 0, sum = 0;
        for (var r = 0; r < 64; r++) {
          for (var k in a) {
            count++;
            sum = (sum + (+k) + a[k]) % 1000000007;
          }
        }
        console.log(count + "," + sum);
        "#,
    );
    // node v24.12.0
    assert_eq!(out, ["64000,987126103"]);
}

/// `forin_version_fast_enabled` is process-latched. A fresh child is therefore
/// required for a real same-binary comparison with the old per-key lookup.
#[test]
fn zz_off_switch_agrees_with_node() {
    if std::env::var_os("ZIPP_FORIN_VERSION_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--skip", "zz_off_switch_agrees_with_node"])
        .env("ZIPP_NO_FORIN_VERSION_FAST", "1")
        .env("ZIPP_FORIN_VERSION_CHILD", "1")
        .output()
        .expect("re-run test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains(" 0 passed"),
        "old per-key path (ZIPP_NO_FORIN_VERSION_FAST=1) diverges:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
}
