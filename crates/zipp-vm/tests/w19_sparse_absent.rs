//! W19 (sparse lane): the four absent-index mechanisms, pinned.
//!
//! `sparse-array` and `sparse-array-v2` lose most of their gap to node in ONE
//! question asked badly — "is this index present?" — answered in four places,
//! each of which walked the prototype chain (spelling the index into a fresh
//! `String` on the way) for an index that provably could not be there:
//!
//!   M1  `array_iter_get`      every hole in a `slice` / `concat` /
//!                             change-by-copy element walk. Measured through
//!                             `concat` on a 4096-element array: 0 holes
//!                             12.3 µs/call, 2048 holes 144.3 µs/call — 64.5
//!                             ns/hole, dead linear, against node's flat
//!                             1.0 µs. After: 27.3 µs at 2048 holes.
//!   M2  `jit_get_index`       an UNGUARDED hole read deopted, so a holey array
//!                             was JIT-dead: on one 4096-element array, present
//!                             1.0 ns / out-of-range 6.0 ns / HOLE 65.5 ns. The
//!                             out-of-range arm eight lines below already had
//!                             the guard, and a hole below length and an index
//!                             above it are the same question.
//!   M3  `has_property_jit`    ONE sparse element anywhere made `i in a` refuse
//!                             the shortcut for the WHOLE receiver, so a probe
//!                             landing in the dense prefix paid the sparse
//!                             price too (81.9 ns in-prefix vs 84.0 ns past it
//!                             — indistinguishable), while `hasOwn` on the same
//!                             receiver answered in 36.8 ns because it already
//!                             probed the overlay directly.
//!   M4  `forin_live`          the alloc-free own-hit probe matched
//!                             `HeapObj::Object` only, so an ARRAY receiver —
//!                             the whole point of a sparse for-in — fell to
//!                             `has_property` plus a fresh `String` per key.
//!
//! All four are licensed by the SAME sticky indexed-prototype protector, so most
//! of what follows is about the shapes that must NOT take the fast answer: an
//! overlay that can shadow an element, a `setPrototypeOf`'d receiver, an
//! arguments object, and — the one that is easy to get wrong — a NON-CANONICAL
//! numeric key (`"01"`) that no element may ever answer.
//!
//! Every expectation below was executed in node as a script and is byte-
//! identical, and every one was re-run with all four off-switches set
//! (`ZIPP_NO_HOLE_ABSENT_FAST`, `ZIPP_NO_HOLE_UNDEF`, `ZIPP_NO_INDEX_IN_OVERLAY`,
//! `ZIPP_NO_FORIN_ARR_OWN`) — same output, which is what the switches are for.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Renders holes distinguishably, so `[0, <hole>, 2]` cannot pass as
/// `[0, undefined, 2]` — the difference every one of these mechanisms turns on.
const SHOW: &str = r#"
    function show(v) { var s = []; for (var i = 0; i < v.length; i++) s.push((i in v) ? String(v[i]) : "-"); return s.join(","); }
"#;

// ── M1: the absent-index fast answer in the builtin element walk ────────────

#[test]
fn m1_a_hole_stays_a_hole_through_slice_and_concat() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        {SHOW}
        var a = [0, , 2, , 4];
        console.log(show(a.slice()) + "|" + show(a.concat([9])) + "|" + show([7].concat(a)));
        "#
    ));
    assert_eq!(out[0], "0,-,2,-,4|0,-,2,-,4,9|7,0,-,2,-,4");
}

#[test]
fn m1_a_prototype_index_at_a_hole_is_still_visited() {
    // The protector is what licenses the fast answer, and `array_iter_get` reads
    // it PER CALL — so defining the index mid-run must change the very next
    // builtin walk, with no eviction and no recompile involved.
    let out = run_ok(&format!(
        r#"
        "use strict";
        {SHOW}
        var a = [0, , 2, , 4];
        var before = show(a.slice()) + "/" + show(a.concat([9])) + "/" + a.toReversed().join(",");
        Object.prototype[3] = "OP3";
        var after = show(a.slice()) + "/" + show(a.concat([9])) + "/" + a.toReversed().join(",");
        delete Object.prototype[3];
        Array.prototype[1] = "AP1";
        var arr = show(a.slice()) + "/" + a.map(function (x) {{ return String(x); }}).join(",");
        delete Array.prototype[1];
        console.log(before + "|" + after + "|" + arr);
        "#
    ));
    assert_eq!(
        out[0],
        "0,-,2,-,4/0,-,2,-,4,9/4,,2,,0|0,-,2,OP3,4/0,-,2,OP3,4,9/4,OP3,2,,0|0,AP1,2,-,4/0,AP1,2,,4"
    );
}

#[test]
fn m1_an_overlay_index_at_a_hole_is_not_skipped() {
    // A `defineProperty`'d accessor or non-default data property at a hole index
    // lives in `arr_props`; `array_elements_overlaid` is what keeps it visible,
    // and it folds in the integrity levels, so the frozen array is covered too.
    let out = run_ok(&format!(
        r#"
        "use strict";
        {SHOW}
        var a = [0, , 2];
        Object.defineProperty(a, "1", {{ get: function () {{ return "G"; }}, configurable: true, enumerable: true }});
        var b = [0, , 2];
        Object.defineProperty(b, "1", {{ value: "D", configurable: true, writable: true, enumerable: false }});
        var c = [0, , 2];
        Object.freeze(c);
        console.log(show(a.slice()) + "|" + show(b.concat([])) + "|" + show(c.slice()));
        "#
    ));
    assert_eq!(out[0], "0,G,2|0,D,2|0,-,2");
}

#[test]
fn m1_a_setprototypeof_receiver_still_walks_its_chain() {
    let out = run_ok(
        r#"
        "use strict";
        var a = [0, , 2];
        Object.setPrototypeOf(a, { 1: "P1" });
        console.log(Array.prototype.slice.call(a).join(",") + "|" + Array.prototype.concat.call([], a).join(","));
        "#,
    );
    assert_eq!(out[0], "0,P1,2|0,P1,2");
}

// ── M2: a hole reads `undefined` under the protector, in BOTH tiers ─────────

#[test]
fn m2_hole_and_oob_reads_agree_between_the_tiers() {
    // The cold reads at the top are the interpreter twin; the hot loop is the
    // JIT helper. They must agree — that agreement IS the mechanism, and a JIT
    // that answers `undefined` where the interpreter walks the chain is exactly
    // the wrong-answer class the tier-differential fuzzer hunts.
    let out = run_ok(
        r#"
        "use strict";
        var a = [0, , 2, , 4];
        var cold = String(a[1]) + "/" + String(a[9]) + "/" + String(a[-1]);
        function hot(x, i) { var v; for (var r = 0; r < 40000; r++) v = x[i]; return String(v); }
        console.log(cold + "|" + hot(a, 1) + "/" + hot(a, 9) + "/" + hot(a, 0));
        "#,
    );
    assert_eq!(
        out[0],
        "undefined/undefined/undefined|undefined/undefined/0"
    );
}

#[test]
fn m2_a_compiled_hole_read_sees_a_later_prototype_index() {
    // B87/B89's demand: the protector is sticky and set-only, so a program that
    // invalidates it AFTER a region compiled must see the next read answer
    // correctly. Nothing is cached in compiled code — the flag is read per call
    // by the helper — and this is what pins that.
    let out = run_ok(
        r#"
        "use strict";
        var a = [0, , 2, , 4];
        function hot(x, i) { var v; for (var r = 0; r < 40000; r++) v = x[i]; return String(v); }
        var before = hot(a, 1);
        Object.defineProperty(Array.prototype, "1", { value: "AP1", configurable: true, writable: true });
        var next = String(a[1]);
        var after = hot(a, 1);
        delete Array.prototype[1];
        var gone = hot(a, 1);
        console.log(before + "|" + next + "|" + after + "|" + gone);
        "#,
    );
    // `gone` is `undefined` again by the ordinary walk, not by the fast answer:
    // the protector never clears, so the chain is really visited from here on.
    assert_eq!(out[0], "undefined|AP1|AP1|undefined");
}

#[test]
fn m2_an_overlay_or_custom_prototype_keeps_the_full_protocol() {
    let out = run_ok(
        r#"
        "use strict";
        function hot(x, i) { var v; for (var r = 0; r < 40000; r++) v = x[i]; return String(v); }
        var a = [0, , 2];
        Object.defineProperty(a, "1", { get: function () { return "G"; }, configurable: true });
        var b = [0, , 2];
        Object.setPrototypeOf(b, { 1: "P1", 7: "P7" });
        var c = []; c.length = 50000000; c[3] = "d"; c[1048581] = "s";
        console.log(hot(a, 1) + "|" + hot(b, 1) + "/" + hot(b, 7) + "|" + hot(c, 1048581) + "/" + hot(c, 1048582));
        "#,
    );
    assert_eq!(out[0], "G|P1/P7|s/undefined");
}

// ── M3: `in` on a receiver that carries a sparse overlay ────────────────────

#[test]
fn m3_in_answers_the_overlay_and_the_dense_prefix_alike() {
    let out = run_ok(
        r#"
        "use strict";
        var a = []; a.length = 50000000;
        a[10] = "d"; a[1048581] = "s1"; a[1048585] = "s2";
        function hot(x, i) { var c = 0; for (var r = 0; r < 40000; r++) if (i in x) c++; return c > 0; }
        console.log([hot(a,10), hot(a,11), hot(a,1048581), hot(a,1048582), hot(a,1048585), hot(a,49999999)].join(","));
        "#,
    );
    assert_eq!(out[0], "true,false,true,false,true,false");
}

#[test]
fn m3_in_hasown_reflect_and_gopd_stay_consistent_on_an_overlay() {
    // `in` asks only presence, so an accessor index and a non-enumerable index
    // must both answer `true` — and must answer it the same way the shipped
    // `hasOwn` intrinsic already does, which is where this arm's probe comes
    // from. All four spellings are asked so none can drift.
    let out = run_ok(
        r#"
        "use strict";
        var hasOwn = Object.prototype.hasOwnProperty;
        var a = []; a.length = 50000000;
        a[10] = "d"; a[1048581] = "s";
        Object.defineProperty(a, "1048600", { get: function () { return 1; }, configurable: true });
        Object.defineProperty(a, "20", { value: 2, enumerable: false, configurable: true });
        var r = [];
        var keys = [10, 11, 20, 1048581, 1048582, 1048600];
        for (var j = 0; j < keys.length; j++) {
          var k = keys[j];
          for (var t = 0; t < 20000; t++) { if (k in a) {} }
          r.push(k + ":" + (k in a) + "/" + hasOwn.call(a, k) + "/" + Reflect.has(a, k) +
                 "/" + (Object.getOwnPropertyDescriptor(a, k) ? "D" : "-"));
        }
        console.log(r.join(" "));
        "#,
    );
    assert_eq!(
        out[0],
        "10:true/true/true/D 11:false/false/false/- 20:true/true/true/D \
1048581:true/true/true/D 1048582:false/false/false/- 1048600:true/true/true/D"
    );
}

#[test]
fn m3_in_on_an_overlay_still_sees_a_prototype_index() {
    let out = run_ok(
        r#"
        "use strict";
        var a = []; a.length = 50000000;
        a[10] = "d"; a[1048581] = "s";
        function hot(x, i) { var c = false; for (var r = 0; r < 40000; r++) c = (i in x); return c; }
        var before = [hot(a, 11), hot(a, 1048582)].join(",");
        Object.prototype[11] = "OP11";
        var after = [hot(a, 11), hot(a, 1048582)].join(",");
        delete Object.prototype[11];
        console.log(before + "|" + after);
        "#,
    );
    assert_eq!(out[0], "false,false|true,false");
}

// ── M4: the for-in liveness probe on an array receiver ─────────────────────

#[test]
fn m4_forin_drops_keys_deleted_mid_loop() {
    // Dropping a deleted key is the ONLY job of the liveness re-check, so the
    // array arm is tested against exactly that: dense, holey, whole-array, and
    // a sparse overlay whose key lives in `arr_props`.
    let out = run_ok(
        r#"
        "use strict";
        function walk(a, mutate) { var s = []; var n = 0; for (var k in a) { s.push(k); if (n++ === 0) mutate(a); } return s.join(","); }
        var r = [];
        r.push(walk([1,2,3,4], function (a) { delete a[1]; }));
        r.push(walk([1,,3,,5], function (a) { delete a[4]; }));
        r.push(walk([1,2,3,4], function (a) { for (var q in a) delete a[q]; }));
        var sp = []; sp.length = 50000000; sp[3] = 1; sp[1048581] = 2; sp[1048585] = 3;
        r.push(walk(sp, function (a) { delete a[1048585]; }));
        console.log(r.join("|"));
        "#,
    );
    assert_eq!(out[0], "0,2,3|0,2|0|3,1048581");
}

#[test]
fn m4_a_non_canonical_numeric_key_is_never_answered_by_an_element() {
    // `"01"` parses to 1 but is an ORDINARY named property. Deleting element 1
    // must not drop it, and it must not keep element 1 alive. A bare
    // `parse::<usize>()` in the array arm fails this test in both directions.
    let out = run_ok(
        r#"
        "use strict";
        var a = [];
        a[1] = "e1"; a["01"] = "n01"; a[5] = "e5"; a["05"] = "n05"; a["1.0"] = "f"; a["-0"] = "m";
        var s1 = []; var n = 0;
        for (var k in a) { s1.push(k + "=" + a[k]); if (n++ === 0) { delete a[1]; delete a[5]; } }
        var s2 = [];
        for (var k2 in a) s2.push(k2 + "=" + a[k2]);
        console.log(s1.join(",") + "|" + s2.join(","));
        "#,
    );
    assert_eq!(
        out[0],
        "1=e1,01=n01,05=n05,1.0=f,-0=m|01=n01,05=n05,1.0=f,-0=m"
    );
}

#[test]
fn m4_named_keys_and_length_changes_are_observed() {
    let out = run_ok(
        r#"
        "use strict";
        function walk(a, mutate) { var s = []; var n = 0; for (var k in a) { s.push(k); if (n++ === 0) mutate(a); } return s.join(","); }
        var r = [];
        r.push(walk((function () { var a = [1, 2]; a.x = "X"; return a; })(), function (a) { delete a.x; }));
        r.push(walk((function () { var a = [1, 2, 3]; return a; })(), function (a) { a.length = 1; }));
        r.push(walk((function () { var a = [1, 2]; return a; })(), function (a) { a[7] = "new"; }));
        console.log(r.join("|"));
        "#,
    );
    assert_eq!(out[0], "0,1|0|0,1");
}

#[test]
fn m4_an_arguments_object_enumerates_unchanged() {
    // Arguments objects are excluded from the array arm on purpose: `length` and
    // the mapped indices are not this simple ownership question.
    let out = run_ok(
        r#"
        "use strict";
        function f() { var s = []; var n = 0; for (var k in arguments) { s.push(k + "=" + arguments[k]); if (n++ === 0) delete arguments[1]; } return s.join(","); }
        console.log(f(7, 8, 9));
        "#,
    );
    assert_eq!(out[0], "0=7,2=9");
}

// ── the two rows' own shapes, end to end ───────────────────────────────────

#[test]
fn the_bench_rows_shapes_answer_exactly_as_node_does() {
    // A miniature of both target rows: stride writes into a virtual-length
    // array, `in` and hasOwn probes over it, a for-in fold, a hole-aware scan,
    // an UNGUARDED read scan (M2's shape), and the slice+concat pair — the six
    // places the four mechanisms meet. The checksums are node's.
    let out = run_ok(
        r#"
        "use strict";
        var hasOwn = Object.prototype.hasOwnProperty;
        var SPLEN = 5000000, STRIDE = 1250;
        var sp = []; sp.length = SPLEN;
        var writes = 0;
        for (var i = 0; i < SPLEN; i += STRIDE) { sp[i] = (i % 1000) + 1; writes++; }
        var inHits = 0, ownHits = 0;
        for (var i = 0; i < 190000; i += 14) { if (i in sp) inHits++; if (hasOwn.call(sp, i + 1)) ownHits++; }
        var keyCount = 0, keyFold = 0;
        for (var k in sp) { keyCount++; keyFold = (keyFold + (+k) + sp[k]) % 1000000007; }
        var PACK = 100000;
        var packed = new Array(PACK);
        for (var i = 0; i < PACK; i++) packed[i] = (i * 7) % 1009;
        for (var i = 0; i < PACK; i += 5) delete packed[i];
        var holeCount = 0, holeSum = 0;
        for (var i = 0; i < PACK; i++) { if (i in packed) holeSum = (holeSum + packed[i]) | 0; else holeCount++; }
        var undefCount = 0, readSum = 0;
        for (var i = 0; i < PACK; i++) { var v = packed[i]; if (v === undefined) undefCount++; else readSum = (readSum + v) | 0; }
        var cc = packed.slice(10000, 35000).concat(packed.slice(60000, 70000), [1, , 3]);
        var ccHoles = 0, ccSum = 0;
        for (var i = 0; i < cc.length; i++) { if (i in cc) ccSum = (ccSum + cc[i]) | 0; else ccHoles++; }
        console.log(writes + " " + inHits + " " + ownHits + " " + keyCount + " " + keyFold + " " +
          holeCount + " " + holeSum + " " + undefCount + " " + readSum + " " + cc.length + " " + ccHoles + " " + ccSum);
        "#,
    );
    assert_eq!(
        out[0],
        "4000 22 0 4000 999003937 20000 40309318 20000 40309318 35003 7001 14114990"
    );
}
