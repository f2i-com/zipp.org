//! Annex B legacy RegExp statics (`RegExp.lastMatch`, `leftContext`,
//! `rightContext`, `$1`..`$9`) are recorded as ranges into the subject and
//! materialised on the rare read -- for EVERY subject, not only ASCII ones.
//!
//! Until this deferral covered non-ASCII subjects, each successful match over
//! one copied the whole subject into `leftContext` + `rightContext`. A global
//! `match`, `split` or functional `replace` over a 24 KB non-ASCII string
//! therefore held a thousand such copies live inside one native call (72 MB),
//! which the hardened profile's 32 MiB of heap headroom convicted as a regex
//! memory failure while the ASCII twin of the same program used 245 KB.
//!
//! The values here are what node prints for the same programs.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn legacy_statics_read_back_correctly_for_a_non_ascii_subject() {
    let out = run_ok(
        r#"
        var s = "\u00e9a1b\u00e9c22d\u4e2d33";
        var m = /(\d)(\d)?/.exec(s);
        console.log(m.index, m[0], RegExp.lastMatch, RegExp["$&"]);
        console.log(JSON.stringify(RegExp.leftContext), JSON.stringify(RegExp["$`"]));
        console.log(JSON.stringify(RegExp.rightContext), JSON.stringify(RegExp["$'"]));
        console.log(RegExp.$1, JSON.stringify(RegExp.$2));
        console.log(RegExp.input === s, RegExp.$_ === s);
        // A later match over an ASCII subject replaces every static.
        /c(\d+)/.exec("abc42xyz");
        console.log(RegExp.lastMatch, RegExp.leftContext, RegExp.rightContext, RegExp.$1);
        // And a non-ASCII match after that replaces them again.
        /(\u4e2d)(3+)/.exec(s);
        console.log(RegExp.lastMatch, RegExp.leftContext.length, RegExp.rightContext, RegExp.$2);
        "#,
    );
    assert_eq!(
        out,
        [
            "2 1 1 1",
            "\"\u{e9}a\" \"\u{e9}a\"",
            "\"b\u{e9}c22d\u{4e2d}33\" \"b\u{e9}c22d\u{4e2d}33\"",
            "1 \"\"",
            "true true",
            "c42 ab xyz 42",
            "\u{4e2d}33 9  33",
        ]
    );
}

#[test]
fn legacy_statics_follow_global_match_over_a_non_ascii_subject() {
    // After a global match the statics describe the LAST successful match.
    let out = run_ok(
        r#"
        var s = "\u00e9x1\u00e9x22\u00e9x333";
        var all = s.match(/\d+/g);
        console.log(all.join(","), RegExp.lastMatch, RegExp.leftContext.length, JSON.stringify(RegExp.rightContext));
        var parts = s.split(/\d+/);
        console.log(parts.length, RegExp.lastMatch);
        var replaced = s.replace(/\d+/g, function (d) { return "[" + d.length + "]"; });
        console.log(replaced, RegExp.lastMatch, RegExp.leftContext.length);
        "#,
    );
    assert_eq!(
        out,
        [
            "1,22,333 333 9 \"\"",
            "4 333",
            "\u{e9}x[1]\u{e9}x[2]\u{e9}x[3] 333 9",
        ]
    );
}
