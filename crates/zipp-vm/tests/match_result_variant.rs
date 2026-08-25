//! Node-parity coverage for the compact RegExp match-result representation.
//!
//! A pristine `exec`/`matchAll` result keeps `index`/`input`/`groups` (and
//! `indices` under `/d`) in a compact fixed record instead of an `arr_props`
//! `ObjMap`; the first operation that can observe or change descriptors, key
//! order, deletion, or integrity state materialises the ordinary properties.
//! The result must be indistinguishable from an ordinary Array carrying those
//! own data properties, so every `mrv_parity_` case asserts byte-identical
//! output against `node -e` (node v24 expected on PATH, the same precondition
//! as `dv_double_tier.rs` / `backedge_dense.rs`).
//!
//! The final test re-runs the whole `mrv_parity_` set in four more modes, each
//! in its own child process (the env latches are read once per process):
//! `ZIPP_NO_MATCH_VARIANT=1` (the off-switch — every result materialised at
//! construction), `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1`, and
//! `ZIPP_GC_STRESS=1` (the compact record's Values must be traced as roots).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The same program's output from `node -e`, so expectations aren't
/// hand-computed.
fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node v24 on PATH (expected values come from `node -e`)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

/// Identity and reflection on an untouched result: Array-ness, prototype,
/// own-key order (elements, then index/input/groups/indices in creation
/// order), descriptors, for-in, JSON, entries.
#[test]
fn mrv_parity_identity_key_order_descriptors() {
    assert_matches_node(
        r#"
        "use strict";
        var m = /(\d+)-(?<w>\w+)/d.exec("id 42-abc!");
        console.log(Array.isArray(m), m instanceof Array, m.constructor === Array);
        console.log(Object.getPrototypeOf(m) === Array.prototype);
        console.log(m.length, m[0], m[1], m[2], m.index, m.input);
        console.log(Reflect.ownKeys(m).map(String).join("+"));
        console.log(Object.keys(m).join("+"));
        console.log(Object.getOwnPropertyNames(m).join("+"));
        var fi = [];
        for (var k in m) fi.push(k);
        console.log(fi.join("+"));
        console.log(JSON.stringify(m));
        console.log(JSON.stringify(Object.entries(m)));
        console.log(JSON.stringify(Object.values(m)));
        ["index", "input", "groups", "indices", "length"].forEach(function (k) {
          var d = Object.getOwnPropertyDescriptor(m, k);
          console.log(k, d ? [JSON.stringify(d.value), d.writable, d.enumerable, d.configurable].join(",") : "none");
        });
        console.log(Object.getPrototypeOf(m.groups) === null, m.groups.w);
        console.log(JSON.stringify(m.indices), JSON.stringify(m.indices.groups.w));
        console.log(m.propertyIsEnumerable("index"), Object.hasOwn(m, "groups"), "input" in m);
        "#,
    );
}

/// A plain (no named groups, no /d) result: `groups` is an own property whose
/// value is undefined; `indices` is NOT an own property.
#[test]
fn mrv_parity_plain_result_optional_fields() {
    assert_matches_node(
        r#"
        "use strict";
        var m = /(b)(x)?/.exec("abc");
        console.log(m[0], m[1], m[2] === undefined, m.length);
        console.log(Object.hasOwn(m, "groups"), m.groups === undefined);
        console.log(Object.hasOwn(m, "indices"), m.indices === undefined);
        console.log(Reflect.ownKeys(m).map(String).join("+"));
        console.log(JSON.stringify(m));
        console.log(1 in m, 2 in m, Object.hasOwn(m, "2"));
        "#,
    );
}

/// Spread, array/object destructuring, Object.assign, object spread —
/// enumeration-driven consumers see the exact ordinary properties.
#[test]
fn mrv_parity_spread_destructuring_assign() {
    assert_matches_node(
        r#"
        "use strict";
        var m = /(a)(?<n>b)/d.exec("zab");
        console.log(JSON.stringify([...m]));
        var [w, g1, g2] = m;
        console.log(w, g1, g2);
        var { index, input, groups } = m;
        console.log(index, input, JSON.stringify(groups));
        var o = Object.assign({}, m);
        console.log(JSON.stringify(o), Object.keys(o).join("+"));
        var sp = { ...m };
        console.log(JSON.stringify(sp), Object.keys(sp).join("+"));
        console.log(JSON.stringify(Array.from(m)));
        "#,
    );
}

/// Passing the result through Array.prototype machinery.
#[test]
fn mrv_parity_array_prototype_methods() {
    assert_matches_node(
        r#"
        "use strict";
        var m = /(\d)(\d)/.exec("x42y");
        console.log(m.join("|"), m.indexOf("4"), m.includes("42"), m.lastIndexOf("2"));
        console.log(JSON.stringify(m.map(function (s) { return s + "!"; })));
        console.log(JSON.stringify(m.filter(function (s) { return s.length === 1; })));
        console.log(JSON.stringify(m.slice(1)), JSON.stringify(m.concat(["z"])));
        console.log(m.reduce(function (a, b) { return a + ":" + b; }));
        console.log(JSON.stringify(m.flat()), m.find(function (s) { return s === "2"; }));
        var out = [];
        for (var v of m) out.push(v);
        console.log(out.join("/"));
        m.forEach(function (v, i) { out.push(i + "=" + v); });
        console.log(out.join("/"));
        console.log(JSON.stringify(m.toReversed ? m.toReversed() : m.slice().reverse()));
        console.log(m.join("|"), m.index, m.input);
        "#,
    );
}

/// Mutation then re-read: writes, deletes, redefinition, added properties, and
/// the resulting own-key order — materialisation must be observable only as
/// ordinary property behaviour.
#[test]
fn mrv_parity_mutation_then_reread() {
    assert_matches_node(
        r#"
        "use strict";
        var m = /(a)(?<n>b)/d.exec("zab");
        m.index = 99;
        console.log(m.index, Object.getOwnPropertyDescriptor(m, "index").writable);
        delete m.input;
        console.log(Object.hasOwn(m, "input"), m.input === undefined);
        m.extra = "later";
        Object.defineProperty(m, "groups", { enumerable: false });
        console.log(Reflect.ownKeys(m).map(String).join("+"));
        console.log(Object.keys(m).join("+"));
        console.log(JSON.stringify(m), m.groups.n);
        Object.defineProperty(m, "index", { get: function () { return 7; }, configurable: true });
        console.log(m.index, typeof Object.getOwnPropertyDescriptor(m, "index").get);
        m[0] = "MUT";
        m.length = 2;
        console.log(JSON.stringify(m), m.length);
        var m2 = /q/.exec("q");
        m2.groups = { fake: 1 };
        console.log(JSON.stringify(m2.groups), Object.keys(m2).join("+"));
        "#,
    );
}

/// Integrity levels and Reflect on pristine results.
#[test]
fn mrv_parity_integrity_and_reflect() {
    assert_matches_node(
        r#"
        "use strict";
        var a = /(a)/.exec("a");
        Object.freeze(a);
        console.log(Object.isFrozen(a), Reflect.set(a, "index", 5), a.index);
        var b = /(b)/.exec("b");
        Object.seal(b);
        console.log(Object.isSealed(b), Reflect.deleteProperty(b, "input"), b.input);
        console.log(Reflect.set(b, "index", 8), b.index);
        var c = /(c)/.exec("c");
        Object.preventExtensions(c);
        console.log(Object.isExtensible(c), Reflect.set(c, "next", 1), Object.hasOwn(c, "next"));
        console.log(Reflect.get(c, "index"), Reflect.has(c, "groups"));
        console.log(JSON.stringify(Reflect.ownKeys(c).map(String)));
        "#,
    );
}

/// matchAll / replace(fn) / match — every result funnelled through the String
/// methods carries the same standard properties.
#[test]
fn mrv_parity_string_methods_funnel() {
    assert_matches_node(
        r#"
        "use strict";
        var all = Array.from("a1 b2 c3".matchAll(/(?<L>[a-z])(\d)/g));
        console.log(all.length);
        all.forEach(function (m) {
          console.log(m[0], m[1], m[2], m.index, m.input, m.groups.L, Object.keys(m).join("+"));
        });
        var pieces = [];
        "x7y8".replace(/(\d)/g, function (whole, g, pos, s) {
          pieces.push([whole, g, pos, s].join(","));
          return "[" + g + "]";
        });
        console.log(pieces.join(" | "));
        var mm = "q9".match(/(\d)/);
        console.log(mm[0], mm[1], mm.index, mm.input, Object.keys(mm).join("+"));
        console.log(JSON.stringify("a b".match(/\w/g)));
        "#,
    );
}

/// A JIT-hot loop over exec results: element and named reads stay correct at
/// full optimisation (and under the threshold/no-JIT modes re-run below).
#[test]
fn mrv_parity_hot_loop_reads() {
    assert_matches_node(
        r#"
        "use strict";
        var re = /(\d+)\.(\d+)/;
        var acc = 0, txt = "";
        for (var i = 0; i < 3000; i++) {
          var m = re.exec("v" + (i & 7) + "." + ((i * 7) & 15) + "end");
          acc = (acc + m.index + (+m[1]) + (+m[2]) + m[0].length + m.input.length) | 0;
          if ((i & 1023) === 0) txt += m[0] + ";";
        }
        console.log(acc, txt);
        var g = /(?<a>\d)(?<b>\d)/;
        var s = 0;
        for (var j = 0; j < 2000; j++) {
          var r = g.exec("n" + (j % 90 + 10));
          s = (s + (+r.groups.a) * 10 + (+r.groups.b)) | 0;
        }
        console.log(s);
        "#,
    );
}

/// Heavy allocation between construction and the reads: the compact record's
/// Values (input string, groups object, indices array) must be traced as GC
/// roots while the owning Array is live. Meaningful in every mode; decisive
/// under the `ZIPP_GC_STRESS=1` re-run.
#[test]
fn mrv_parity_gc_churn_roots() {
    assert_matches_node(
        r#"
        "use strict";
        var keep = [];
        for (var i = 0; i < 50; i++) {
          keep.push(/(?<k>\d+)-(\w+)/d.exec("pre " + i + "-tag" + i + " post"));
        }
        var junk = null;
        for (var j = 0; j < 20000; j++) {
          junk = { j: j, s: "pad" + j, a: [j, j + 1] };
        }
        var acc = [];
        for (var i = 0; i < 50; i += 7) {
          var m = keep[i];
          acc.push([m[0], m[1], m[2], m.index, m.input.length, m.groups.k,
                    JSON.stringify(m.indices[1])].join("&"));
        }
        console.log(acc.join(" "));
        console.log(junk.j);
        "#,
    );
}

/// Re-run every `mrv_parity_` case in four more modes, each in its own child
/// process (the env latches are read once per process): the off-switch, the
/// pure interpreter, threshold-1, and GC stress. The same node-derived
/// assertions passing in all five modes IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_MATCH_VARIANT", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("mrv_parity_")
            .env(key, val)
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the mrv_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
