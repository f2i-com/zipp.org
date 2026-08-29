//! B244 correctness coverage for the CROSS3 dense-Array snapshot epoch.
//!
//! The epoch is only a raw-payload validity proof. These cases pin the other
//! half of the licence (live binding identity), every important Array storage
//! mutation, eligibility-changing indexed overlays, and named-only sidecars
//! that may conservatively remain clean.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

fn assert_matches_node(src: &str) {
    let zipp = zipp_vm::run(src).expect("source compiles in Zipp");
    assert!(zipp.error.is_none(), "Zipp runtime error: {:?}", zipp.error);
    let node = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node must be available for differential JIT tests");
    assert!(
        node.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&node.stderr)
    );
    let expected: Vec<String> = String::from_utf8_lossy(&node.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(zipp.output, expected);
}

/// Epoch equality alone is insufficient: rebinding a global from one existing
/// dense Array to another mutates no Heap payload. The emitted post-call guard
/// must compare the live source Value with the cached snapshot identity.
#[test]
fn global_reassignment_between_clean_preexisting_arrays_refreshes_identity() {
    assert_matches_node(
        r#"
        "use strict";
        var left = [1, 2, 3, 4];
        var right = [101, 102, 103, 104];
        var data = left;
        function choose(i) {
          if (i === 200000) data = right;
          return i & 3;
        }
        var sum = 0;
        for (var i = 0; i < 400000; i++) {
          var k = choose(i);
          sum = (sum + data[k]) | 0;
        }
        console.log("rebind", sum, data === right);
        "#,
    );
}

/// `get_mut` is the storage-write chokepoint: grow/realloc, shrink/extend,
/// element overwrite and indexed descriptor changes all force a re-snapshot.
/// Getter invocation makes a stale dense read immediately observable.
#[test]
fn array_storage_and_index_overlay_mutations_refresh_after_cross_calls() {
    assert_matches_node(
        r#"
        "use strict";
        var a = [2, 3, 5, 7];
        var getterReads = 0;
        function touch(i) {
          if (i === 20000) {
            for (var j = 0; j < 80; j++) a.push((j + 11) | 0);
          } else if (i === 40000) {
            a.length = 2;
          } else if (i === 60000) {
            a.length = 12;
            a[7] = 701;
          } else if (i === 80000) {
            Object.defineProperty(a, "1", {
              get() { getterReads++; return 31; },
              configurable: true,
              enumerable: true
            });
          } else if (i === 100000) {
            delete a[1];
          } else if (i === 120000) {
            Object.defineProperty(a, "1", {
              value: 47,
              writable: true,
              configurable: true,
              enumerable: true
            });
          } else if (i === 140000) {
            Object.defineProperty(a, "2", {
              value: 83,
              writable: false,
              configurable: true,
              enumerable: true
            });
          }
          return i & 7;
        }
        var sum = 0;
        for (var i = 0; i < 160000; i++) {
          var k = touch(i);
          var v = a[k];
          if (v !== undefined) sum = (sum + v) | 0;
        }
        console.log("mutate", sum, getterReads, a.length, a[1], a[7]);
        "#,
    );
}

/// Multiple pins may alias, and an unrelated Array mutation may conservatively
/// dirty the global epoch. Both are false-positive refresh cases, never a
/// licence to answer from the wrong object.
#[test]
fn aliases_multiple_pins_and_unrelated_array_mutation_stay_exact() {
    assert_matches_node(
        r#"
        "use strict";
        var a = [3, 5, 7, 11];
        var alias = a;
        var b = [13, 17, 19, 23];
        var unrelated = [];
        function step(i) {
          if ((i & 1023) === 0) unrelated.push(i);
          if (i === 90000) alias = b;
          return i & 3;
        }
        var sum = 0;
        for (var i = 0; i < 180000; i++) {
          var k = step(i);
          sum = (sum + a[k] + alias[(k + 1) & 3] + b[(k + 2) & 3]) | 0;
        }
        console.log("aliases", sum, unrelated.length, alias === b);
        "#,
    );
}

/// Freeze/seal and tagged-template SetRaw install named-only sidecars; they do
/// not change dense element values. Mapped arguments, by contrast, must never
/// use a raw snapshot because writes to the formal remain observable.
#[test]
fn named_sidecars_and_mapped_arguments_keep_full_semantics() {
    assert_matches_node(
        r#"
        "use strict";
        function tag(parts) { return parts; }
        var cooked = tag`aa${1}bb`;
        var frozen = [29, 31, 37, 41];
        var sealed = [43, 47, 53, 59];
        function barrier(i) {
          if (i === 30000) Object.freeze(frozen);
          if (i === 60000) Object.seal(sealed);
          return i & 3;
        }
        var sum = 0;
        for (var i = 0; i < 120000; i++) {
          var k = barrier(i);
          sum = (sum + frozen[k] + sealed[(k + 1) & 3] + cooked[k & 1].length) | 0;
        }

        function mapped(x, y) {
          var args = arguments;
          function choose(i) {
            if (i === 50000) x = 71;
            if (i === 100000) y = 73;
            return i & 1;
          }
          var out = 0;
          for (var j = 0; j < 150000; j++) {
            var q = choose(j);
            out = (out + args[q]) | 0;
          }
          return out;
        }
        console.log("sidecars", sum, cooked.raw[0], mapped(7, 11));
        "#,
    );
}
