//! The Annex B legacy `RegExp` statics, which are stored LAZILY.
//!
//! After a successful match on a flat-ASCII subject, slots 2..=13 (`lastParen`,
//! `leftContext`, `rightContext`, `$1`..`$9`) are kept as unit RANGES into a
//! rooted subject and sliced only when a getter reads one — the eager form copied
//! `leftContext` + `rightContext`, together ~87% of the subject, on every
//! successful match including `test`, and measured 7.9% of `regex-log-scan`.
//!
//! These tests pin the OBSERVABLE side of that: every value must be what the
//! eager form produced. `zipp_vm::run` output is compared against
//! hand-written expectations derived from V8 (see
//! `PERF_ROADMAP.md` B60 for the differential run).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Print every legacy static, quoting strings so `""` and `undefined` differ.
const DUMP: &str = r#"
function q(v) { return typeof v === "string" ? JSON.stringify(v) : String(v); }
function dump() {
  return [q(RegExp.input), q(RegExp.lastMatch), q(RegExp.lastParen),
          q(RegExp.leftContext), q(RegExp.rightContext),
          q(RegExp.$1), q(RegExp.$2), q(RegExp.$3)].join(" ");
}
"#;

#[test]
fn statics_are_empty_strings_before_any_match() {
    let out = run_ok(&format!("{DUMP} console.log(dump());"));
    assert_eq!(out[0], r#""" "" "" "" "" "" "" """#);
}

#[test]
fn test_and_exec_fill_the_same_statics() {
    // `test` builds no result object but must still refresh the statics — the
    // deferred record is written before the `!build` early-out.
    let out = run_ok(&format!(
        r#"{DUMP}
        /(\d+)-(\w+)/.test("xx 123-abc yy");
        console.log("test " + dump());
        /(\d+)-(\w+)/.exec("xx 123-abc yy");
        console.log("exec " + dump());
        "#
    ));
    let expect = r#""xx 123-abc yy" "123-abc" "abc" "xx " " yy" "123" "abc" """#;
    assert_eq!(out[0], format!("test {expect}"));
    assert_eq!(out[1], format!("exec {expect}"));
}

#[test]
fn a_failed_match_leaves_the_previous_statics_intact() {
    // The record is only replaced on SUCCESS, so a miss must not clear it — and
    // must not leave a half-materialised one either.
    let out = run_ok(&format!(
        r#"{DUMP}
        /(\d+)-(\w+)/.exec("xx 123-abc yy");
        /(zzz)(qqq)/.test("nothing here");
        console.log(dump());
        "#
    ));
    assert_eq!(out[0], r#""xx 123-abc yy" "123-abc" "abc" "xx " " yy" "123" "abc" """#);
}

#[test]
fn empty_contexts_at_the_subject_edges() {
    let out = run_ok(&format!(
        r#"{DUMP}
        /^(\w+)/.exec("head tail");
        console.log("at0 " + dump());
        /(tail)$/.exec("head tail");
        console.log("atEnd " + dump());
        "#
    ));
    // leftContext is "" at index 0; rightContext is "" at the end.
    assert_eq!(out[0], r#"at0 "head tail" "head" "head" "" " tail" "head" "" """#);
    assert_eq!(out[1], r#"atEnd "head tail" "tail" "tail" "head " "" "tail" "" """#);
}

#[test]
fn last_paren_is_the_last_participating_group_and_empty_when_none() {
    let out = run_ok(&format!(
        r#"{DUMP}
        /\d+/.exec("abc 999 def");
        console.log("nogroups " + dump());
        /(a)|(b)/.exec("--b--");
        console.log("altskip " + dump());
        "#
    ));
    // No capture groups at all -> lastParen "".
    assert_eq!(out[0], r#"nogroups "abc 999 def" "999" "" "abc " " def" "" "" """#);
    // The first alternative did not participate: $1 is "", $2 is "b", and
    // lastParen is the LAST participating group.
    assert_eq!(out[1], r#"altskip "--b--" "b" "b" "--" "--" "" "b" """#);
}

#[test]
fn only_nine_dollar_groups_are_exposed() {
    let out = run_ok(
        r#"
        /(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)/.exec("__abcdefghijk__");
        console.log([RegExp.$1, RegExp.$8, RegExp.$9, RegExp.lastParen].join(","));
        "#,
    );
    // $9 is "i"; the 10th/11th groups are not addressable, but lastParen is the
    // last PARTICIPATING group, which is "k".
    assert_eq!(out[0], "a,h,i,k");
}

#[test]
fn reading_twice_is_stable_and_reading_one_slot_materialises_correctly() {
    // Materialisation fills all twelve slots at once and clears the record; a
    // second read must come from the memo, not from a cleared record.
    let out = run_ok(
        r#"
        /(\d+)-(\w+)/.exec("pre 55-zed post");
        var a = RegExp.leftContext + "|" + RegExp.$1 + "|" + RegExp.rightContext;
        var b = RegExp.leftContext + "|" + RegExp.$1 + "|" + RegExp.rightContext;
        console.log((a === b) + " " + a);
        "#,
    );
    assert_eq!(out[0], "true pre |55| post");
}

#[test]
fn statics_survive_collection_between_the_match_and_the_read() {
    // The ranges point into the subject, so the deferred record roots it. Without
    // that root a collection here frees the bytes a later read still slices.
    let out = run_ok(
        r#"
        (function () { /(\d+)-(\w+)/.exec("keepme 99-gc tailtail"); })();
        var junk = [];
        for (var i = 0; i < 20000; i++) junk.push({ a: i, b: "s" + i });
        junk = null;
        for (var i = 0; i < 20000; i++) { var t = { x: i, y: "u" + i }; }
        console.log(RegExp.leftContext + "|" + RegExp.$1 + "|" + RegExp.rightContext);
        "#,
    );
    assert_eq!(out[0], "keepme |99| tailtail");
}

#[test]
fn a_non_ascii_subject_takes_the_eager_path_with_the_same_answers() {
    // Only ASCII defers (a non-ASCII slice reads a locally decoded unit buffer
    // that does not outlive the call), so this exercises the eager arm.
    let out = run_ok(&format!(
        r#"{DUMP}
        /(\d+)/.exec("héllo—123—wörld");
        console.log(dump());
        "#
    ));
    assert_eq!(
        out[0],
        "\"héllo—123—wörld\" \"123\" \"123\" \"héllo—\" \"—wörld\" \"123\" \"\" \"\""
    );
}

#[test]
fn regexp_input_is_writable_and_does_not_disturb_the_deferred_slots() {
    // `RegExp.input = x` writes slot 0 only; the match-derived slots keep the
    // last match's values and must still materialise from the ORIGINAL subject.
    let out = run_ok(&format!(
        r#"{DUMP}
        /(\d+)/.exec("abc 42 def");
        RegExp.input = "REPLACED";
        console.log(dump());
        "#
    ));
    assert_eq!(out[0], r#""REPLACED" "42" "42" "abc " " def" "42" "" """#);
}

#[test]
fn a_global_exec_walk_refreshes_per_match() {
    let out = run_ok(
        r#"
        var g = /(\w)(\d)/g, s = "a1 b2 c3", got = [], m;
        while ((m = g.exec(s)) !== null) {
          got.push(RegExp.$1 + RegExp.$2 + "@" + RegExp.leftContext.length);
        }
        console.log(got.join(","));
        "#,
    );
    assert_eq!(out[0], "a1@0,b2@3,c3@6");
}

#[test]
fn match_and_match_all_refresh_the_statics() {
    let out = run_ok(&format!(
        r#"{DUMP}
        "k=1 j=22".match(/([a-z])=(\d+)/);
        console.log("match " + dump());
        for (var m of "k=1 j=22".matchAll(/([a-z])=(\d+)/g)) {{ }}
        console.log("matchAll " + dump());
        "#
    ));
    assert_eq!(out[0], r#"match "k=1 j=22" "k=1" "1" "" " j=22" "k" "1" """#);
    // matchAll leaves the LAST match consumed.
    assert_eq!(out[1], r#"matchAll "k=1 j=22" "j=22" "22" "k=1 " "" "j" "22" """#);
}

/// B71 moved slot 1 (`lastMatch` / `RegExp["$&"]`) into the DEFERRED set, so that a
/// successful `.test()` — which returns a boolean — stops materialising the matched
/// span. The slot is now a unit range into a rooted subject, and these pin the two
/// ways that can go wrong: the range must still read back exactly, and the subject
/// must survive until it does.
#[test]
fn deferred_last_match_reads_back_after_test() {
    let out = run_ok(
        r#"
        /b(c)(d)/.test("abcde");
        console.log(RegExp["$&"] + "|" + RegExp.lastMatch + "|" + RegExp["$`"] + "|" +
                    RegExp["$'"] + "|" + RegExp["$+"] + "|" + RegExp.$1 + RegExp.$2);
        // a second test overwrites the deferred record
        /x(y)/.test("wxyz");
        console.log(RegExp["$&"] + "|" + RegExp.$1);
        // a FAILED match must leave the previous values intact
        /nope/.test("wxyz");
        console.log(RegExp["$&"] + "|" + RegExp.$1);
        "#,
    );
    assert_eq!(out[0], "bcd|bcd|a|e|d|cd");
    assert_eq!(out[1], "xy|y");
    assert_eq!(out[2], "xy|y");
}

#[test]
fn a_deferred_last_match_survives_gc_and_input_overwrite() {
    // The ranges point into the subject, which `regexp_last_lazy.subj` roots. Churn
    // the heap between the match and the read, and separately clobber slot 0 —
    // `RegExp.input = x` overwrites the only OTHER reference to that subject.
    let out = run_ok(
        r#"
        (function () { /l(o)ng/.test("a-long-subject-string-here"); })();
        var junk = [];
        for (var i = 0; i < 20000; i++) junk.push({ a: i, s: "pad" + i });
        console.log(RegExp["$&"] + "|" + RegExp.$1 + "|" + RegExp["$`"]);
        /m(i)d/.test("xx-mid-yy");
        RegExp.input = "clobbered";
        console.log(RegExp["$&"] + "|" + RegExp.$1 + "|" + RegExp.input);
        "#,
    );
    assert_eq!(out[0], "long|o|a-");
    assert_eq!(out[1], "mid|i|clobbered");
}
