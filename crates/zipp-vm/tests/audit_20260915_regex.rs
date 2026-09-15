//! The 15 September 2026 audit's RegExp findings, pinned from JavaScript.
//!
//! - Z-05 / R155 + R149: a `u`/`v` global or sticky exec whose `lastIndex` is
//!   the trailing half of a surrogate pair starts at the pair (RegExpBuiltinExec
//!   step 12.b, as SpiderMonkey does; V8 revisits the mid-pair position for a
//!   zero-width assertion). A greedy loop started mid-pair used to backtrack
//!   past its own start into `unreachable_unchecked` — a native SIGSEGV from
//!   `/.*x/uy` with `lastIndex = 1` on "😀ab". The probes run in child
//!   processes so a crash is a failed mode, not a dead test harness.
//! - R146: a sticky exec is ONE anchored attempt; a failed attempt no longer
//!   scans the rest of the subject (split and sticky lexers were quadratic).
//!   A pristine split scans forward between matches instead of making one
//!   exec per position, with the same pieces and legacy statics.
//! - R147: repeated execs over one non-ASCII string reuse its UTF-16 units;
//!   the reuse must never outlive, or misdescribe, the string (in-place `+=`
//!   growth, slot reuse, alternating subjects).
//! - R145: `split`/`match`/`replace` have no silent 5,000,000-iteration cap.
//! - R156: named groups inside a lookbehind resolve to their own captures.
//! - R302: `String.prototype.split` honours a RegExp separator's `@@split`.
//! - R157: `v` is a `[UnicodeMode]` grammar (plus `[\b]` under `v`, and the
//!   Annex B `\x` identity escape re-reading what follows it).
//! - R152: `@@match` Sets `lastIndex` with `throw = true`.
//! - R153: `$<name>` is one Get + ToString per occurrence; `$$<a>` is literal.
//! - R148: `` $` `` / `$'` context is built only for templates that use it.
//! - R150: RegExp GetSubstitution keeps lone surrogates exact (they were
//!   U+FFFD) and joins halves that meet across its pieces.
//!
//! Every expectation line below is Node 24's output for the same probe. The
//! modes are the default tiers, the interpreter, forced JIT, the regex JIT
//! disabled, and GC stress (the units cache must survive collections). The
//! remaining tests pin the asymptotic fixes with generous wall-clock bounds.

use std::time::{Duration, Instant};

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const PROBES: &str = r##"
  var lines = [];
  function log(label, value) { lines.push(label + "=" + value); }
  function units(s) {
    if (s === null || s === undefined) return String(s);
    var out = [];
    for (var i = 0; i < s.length; i++) out.push(s.charCodeAt(i).toString(16));
    return out.join(" ");
  }
  function ex(m) { return m === null ? "null" : "[" + Array.prototype.map.call(m, units).join(",") + "]@" + m.index; }

  // R155/R149: mid-pair lastIndex on a u/v stateful regex.
  (function () {
    var emoji = "\u{1F600}ab";
    var re = /.*x/uy; re.lastIndex = 1;
    log("sticky-greedy-test", re.test(emoji) + ":" + re.lastIndex);
    re = /.?x/uy; re.lastIndex = 1;
    log("sticky-optional-test", re.test(emoji) + ":" + re.lastIndex);
    re = /[^]*x/ug; re.lastIndex = 1;
    log("global-class-exec", ex(re.exec(emoji)) + ":" + re.lastIndex);
    re = /.*?x/uy; re.lastIndex = 1;
    log("sticky-lazy-test", re.test(emoji) + ":" + re.lastIndex);
    re = /.*x/ug; re.lastIndex = 1;
    log("global-greedy-exec", ex(re.exec("\u{1F600}abx")) + ":" + re.lastIndex);
    re = /(?:)/uy; re.lastIndex = 1;
    log("sticky-empty-exec", ex(re.exec(emoji)) + ":" + re.lastIndex);
    re = /\uDE00/uy; re.lastIndex = 1;
    log("sticky-trail-exec", ex(re.exec("\u{1F600}")) + ":" + re.lastIndex);
    re = /\uDE00/gu; re.lastIndex = 1;
    log("global-trail-exec", ex(re.exec("\u{1F600}")) + ":" + re.lastIndex);
    re = /./uy; re.lastIndex = 1;
    log("sticky-dot-exec", ex(re.exec("\u{1F600}")) + ":" + re.lastIndex);
    re = /./vy; re.lastIndex = 1;
    log("sticky-dot-v-exec", ex(re.exec("\u{1F600}")) + ":" + re.lastIndex);
    // Non-unicode regexes still see code units.
    re = /\uDE00/y; re.lastIndex = 1;
    log("sticky-trail-nonunicode", ex(re.exec("\u{1F600}")) + ":" + re.lastIndex);
    log("replace-trail", units("\u{1F600}".replace(/\uDE00/gu, "X")));
    // A zero-width assertion at a mid-pair lastIndex: exec snaps to the pair,
    // as node does. `split` is where V8 differs — its search loop revisits the
    // mid-pair position, so node prints "61 d83d|de00" for the last line.
    re = /\B/uy; re.lastIndex = 1;
    log("sticky-boundary-mid-pair", ex(re.exec(emoji)) + ":" + re.lastIndex);
    re = /\B/ug; re.lastIndex = 1;
    log("global-boundary-mid-pair", ex(re.exec(emoji)) + ":" + re.lastIndex);
    log("split-boundary", "a\u{1F600}".split(/\B/u).map(units).join("|"));
    var hot = 0, r2 = /.*x/uy;
    for (var i = 0; i < 3000; i++) { r2.lastIndex = 1; if (r2.test(emoji)) hot++; }
    log("sticky-greedy-hot", hot);
  })();

  // R146: sticky execs are anchored — results are unchanged.
  (function () {
    var s = "a1 bb22 ccc333";
    var toks = [], rules = [/\d+/y, /[a-z]+/y, /\s+/y];
    var pos = 0;
    while (pos < s.length) {
      var hit = false;
      for (var k = 0; k < rules.length; k++) {
        rules[k].lastIndex = pos;
        var m = rules[k].exec(s);
        if (m) { toks.push(k + ":" + m[0] + "@" + m.index); pos = rules[k].lastIndex; hit = true; break; }
      }
      if (!hit) break;
    }
    log("lexer", toks.join("|"));
    var y = /b*,/y; y.lastIndex = 1;
    log("sticky-miss", ex(y.exec("aab,")) + ":" + y.lastIndex);
    y.lastIndex = 2;
    log("sticky-hit", ex(y.exec("aab,")) + ":" + y.lastIndex);
    y.lastIndex = 1;
    log("sticky-hit-after-miss", ex(y.exec("abb,")) + ":" + y.lastIndex);
    log("split-spaces", JSON.stringify(" a ,b,  c ,d".split(/\s*,\s*/)));
    log("split-captures", JSON.stringify("a1b22c".split(/(\d)(\d)?/)));
    log("split-limit", JSON.stringify("a,b,c,d".split(/,/, 2)));
    log("split-empty", JSON.stringify("abc".split(/(?:)/)));
    log("split-unicode", JSON.stringify("x\u{1F600}y".split(/(?:)/u).map(units)));
    log("replace-gy", "aaba".replace(/a/gy, "X"));
    log("matchall-gy", JSON.stringify([..."aaba".matchAll(/a/gy)].map(function (m) { return m.index; })));
    log("match-gy", JSON.stringify("aaba".match(/a/gy)));
    var sy = /(?<=a)b/y; sy.lastIndex = 1;
    log("sticky-lookbehind", ex(sy.exec("ab")) + ":" + sy.lastIndex);
    var su = /\u{1F600}/uy; su.lastIndex = 2;
    log("sticky-astral", ex(su.exec("ab\u{1F600}")) + ":" + su.lastIndex);
    // A pristine split scans forward between matches: the pieces and the
    // legacy statics are those of the per-position loop (a match at the end
    // of the subject is never attempted), and the observable protocol paths
    // still exec per position.
    function st() {
      return [RegExp.lastMatch, RegExp.leftContext, RegExp.rightContext, RegExp.input].map(units).join("/");
    }
    var cases = [[/$/, "ab"], [/(?:)/, "ab"], [/\b/, "a b"], [/(?:)/u, "x\u{1F600}"], [/X*/, "aXbXXc"],
      [/(?<=a)/, "aXa"], [/,/, "a,b,,c,"], [/é/, "café au"], [/\r?\n/, "l1\r\nl2\n"], [/(\d)(\d)?/, "a12b3"]];
    var scan = [];
    for (var i = 0; i < cases.length; i++) {
      /zzz/.exec("zzz");
      scan.push(cases[i][1].split(cases[i][0]).map(units).join(",") + "|" + st());
    }
    log("split-scan", scan.join(";"));
    var savedExec = RegExp.prototype.exec, calls = 0;
    RegExp.prototype.exec = function (s) { calls++; return savedExec.call(this, s); };
    var patched = "a,b,,c".split(/,/);
    RegExp.prototype.exec = savedExec;
    log("split-patched-exec", JSON.stringify(patched) + ":" + calls);
    var rn = /,/;
    rn.constructor = { [Symbol.species]: function (src, flags) { return new RegExp(src, flags.replace("y", "g")); } };
    log("split-nonsticky-species", JSON.stringify("a,b,c".split(rn)));
  })();

  // R147: repeated execs over one non-ASCII subject.
  (function () {
    var s = "é word word \u{1F600} word";
    var re = /\w+/g, got = [], m;
    while ((m = re.exec(s))) got.push(m[0] + "@" + m.index);
    log("exec-loop", got.join("|"));
    log("split-nonascii", JSON.stringify(s.split(/\s+/).map(units)));
    var acc = "é";
    var seen = [];
    for (var i = 0; i < 6; i++) {
      var r = /\w+$/g;
      var mm = r.exec(acc);
      seen.push(mm ? mm[0] : "null");
      acc += "ab" + i;
    }
    log("appended-subject", seen.join("|"));
    var subjects = [];
    for (var i = 0; i < 200; i++) subjects.push("é" + "x".repeat(i % 7) + i);
    var sum = 0;
    for (var i = 0; i < subjects.length; i++) {
      var d = /\d+/g; d.lastIndex = 0;
      var hit = d.exec(subjects[i]);
      sum += hit ? +hit[0] : -1;
    }
    log("many-subjects", sum);
    var t = "éaa";
    var g = /a/g;
    log("alternating", [g.exec(t) && g.lastIndex, /b/.exec("ébbb").index, g.exec(t) && g.lastIndex, g.exec(t)].join(","));
  })();

  // R156: named groups inside a lookbehind keep their own values.
  (function () {
    var m = "12.50USD".match(/(?<=(?<int>\d+)\.(?<frac>\d+))USD/);
    log("lb-groups", JSON.stringify(m.groups) + ":" + Object.keys(m.groups).join(","));
    m = "pqr".match(/(?<=(?<a>p)(?<b>q)(?<c>r))/);
    log("lb-three", JSON.stringify(m.groups) + ":" + JSON.stringify([...m]));
    log("lb-replace", "xyz".replace(/(?<=(?<a>x)(?<b>y))z/, "[$<a>|$<b>]"));
    log("lb-indices", JSON.stringify(/(?<=(?<a>x)(?<b>y))z/d.exec("xyz").indices.groups));
    log("lb-mixed", JSON.stringify("abc".match(/(?<=(?<a>a)(?<b>b))(?<c>c)/).groups));
    log("lb-fn-replace", "xyz".replace(/(?<=(?<a>x)(?<b>y))z/, function () { var g = arguments[arguments.length - 1]; return g.a + g.b; }));
    var hot = "";
    for (var i = 0; i < 3000; i++) hot = "12x".match(/(?<=(?<a>\d)(?<b>\d))x/).groups.a;
    log("lb-hot", hot);
  })();

  // R302: String.prototype.split honours @@split on a RegExp separator.
  (function () {
    var own = /a/;
    own[Symbol.split] = function (s, lim) { return ["own", s, String(lim)]; };
    log("split-own", JSON.stringify("xay".split(own, 3)));
    class R extends RegExp { [Symbol.split](s) { return ["subclass:" + s]; } }
    log("split-subclass", JSON.stringify("xay".split(new R("a"))));
    var saved = RegExp.prototype[Symbol.split];
    RegExp.prototype[Symbol.split] = function () { return ["proto"]; };
    log("split-proto", JSON.stringify(["xay".split(/a/), String.prototype.split.call("xay", /a/)]));
    RegExp.prototype[Symbol.split] = saved;
    log("split-restored", JSON.stringify("xay".split(/a/)));
  })();

  // R157: v-mode (UnicodeMode) grammar.
  (function () {
    var bad = ["a{", "q{3", "(?=a)+", "(?!a)*", "a}", "a]", "{", "}", "]", "x{1,", "\\c", "\\x1", "\\u12", "\\1", "\\01", "\\a", "\\-", "\\k", "\\8", "[\\u12]", "[\\a]", "[\\1]"];
    var out = [];
    for (var i = 0; i < bad.length; i++) {
      var r;
      try { new RegExp(bad[i], "v"); r = "ok"; } catch (e) { r = e.constructor.name; }
      out.push(r);
    }
    log("v-errors", out.join(","));
    var good = ["a{1}", "a{1,2}", "(?:a){2}", "\\/", "\\^", "[\\-]", "[\\&]", "[\\q{abc}]", "\\p{L}", "(?<n>a)\\k<n>", "(a)\\1", "\\0"];
    out = [];
    for (var i = 0; i < good.length; i++) {
      var r;
      try { new RegExp(good[i], "v"); r = "ok"; } catch (e) { r = e.constructor.name; }
      out.push(r);
    }
    log("v-valid", out.join(","));
    log("v-class-backspace", /[\b]/v.test("\b") + "," + /[\b]/v.test("b") + "," + /[\b-\n]/v.test("\t"));
    log("annexb-hex", JSON.stringify([/\x1/.exec("x1"), /[\x1]/.exec("1"), /[\x1]/.exec("x"), /a\xZ/.exec("axZ")]));
  })();

  // R152: @@match on a generic receiver Sets lastIndex with throw = true.
  (function () {
    var out = [];
    var o = { flags: "g", exec: function () { return null; } };
    Object.defineProperty(o, "lastIndex", { value: 0, writable: false });
    try { RegExp.prototype[Symbol.match].call(o, "a"); out.push("none"); } catch (e) { out.push(e.constructor.name); }
    var f = Object.freeze({ flags: "g", exec: function () { return null; } });
    try { RegExp.prototype[Symbol.match].call(f, "a"); out.push("none"); } catch (e) { out.push(e.constructor.name); }
    var g = { flags: "gu", exec: function () { return null; }, get lastIndex() { return 0; } };
    try { RegExp.prototype[Symbol.match].call(g, "a"); out.push("none"); } catch (e) { out.push(e.constructor.name); }
    var w = { flags: "g", lastIndex: 5, exec: function () { return null; } };
    out.push(String(RegExp.prototype[Symbol.match].call(w, "a")) + ":" + w.lastIndex);
    log("match-lastindex", out.join(","));
  })();

  // R153: GetSubstitution Gets each `$<name>` occurrence, and `$$<a>` is literal.
  (function () {
    var trace = [];
    var re = /./;
    re.exec = function () {
      var r = ["x"]; r.index = 0;
      var n = 0;
      r.groups = {
        get a() { trace.push("a"); return "A"; },
        get b() { trace.push("b"); n++; return "B" + n; },
      };
      return r;
    };
    var out = RegExp.prototype[Symbol.replace].call(re, "xyz", "[$$<a>][$<b>$<b>]");
    log("named-occurrences", out + ":" + trace.join(","));
    var ts = [];
    var re2 = /./;
    re2.exec = function () {
      var r = ["x"]; r.index = 0;
      r.groups = { a: { toString: function () { ts.push("ts"); return "T" + ts.length; } } };
      return r;
    };
    log("named-tostring", RegExp.prototype[Symbol.replace].call(re2, "xyz", "$<a>$<a>$<missing>") + ":" + ts.join(","));
  })();

  // R148: `$\`` / `$'` still expand on the paths that now build them lazily.
  (function () {
    var doc = "café\nline one\nline two";
    log("ctx-nonascii", units(doc.replace(/\n/g, "[$`|$']").slice(0, 40)));
    log("ctx-plain", doc.replace(/\n/g, "<br>").length);
    log("ctx-sticky", "aab".replace(/a/gy, "($`)"));
    log("ctx-dollar", "ab".replace(/b/g, "$$`"));
    class Sub extends RegExp {}
    log("ctx-subclass", "xay".replace(new Sub("a", "g"), "<$'$`>"));
  })();

  // R150: GetSubstitution is exact over lone surrogates in `$&`, `` $` ``,
  // `$'`, `$n`, `$<name>` and the template text, and halves from different
  // pieces join into the pair they spell.
  (function () {
    log("subst-pre", units("\uD800a".replace(/a/g, "[$`]")));
    log("subst-whole", units("x\uD800".replace(/\uD800/g, "[$&]")));
    log("subst-group", units("x\uD800".replace(/(\uD800)/g, "[$1]")));
    log("subst-post", units("a\uDC00".replace(/a/, "[$']")));
    log("subst-named", units("x\uD800".replace(/(?<n>\uD800)/, "[$<n>]")));
    log("subst-template", units("xa".replace(/a/, "\uD800$&")));
    class Sub2 extends RegExp {}
    log("subst-subclass", units("x\uD800".replace(new Sub2("\uD800", "g"), "[$&]")));
    log("subst-seam-template", units("\uD800a".replace(/a/, "\uDC00")));
    log("subst-seam-context", units("\uD800a".replace(/a/, "$`")));
    log("subst-seam-match", units("😀".replace(/\uDE00/, "$&$&")));
    log("subst-seam-groups", units("ab".replace(/(a)(b)/, "\uD800$2\uDC00$1")));
    log("subst-nonascii-context", units("é\uD800x".replace(/x/g, "[$`|$']")));
    log("subst-nonascii-named", units("é\uD800x".replace(/(?<g>x)/g, "<$<g>$<g>>")));
    var re = /./;
    re.exec = function () {
      var r = ["\uDC00", "\uD800"]; r.index = 1; r.groups = { n: "\uDFFF" };
      return r;
    };
    log("subst-observable", units(RegExp.prototype[Symbol.replace].call(re, "a\uD83Db", "[$&|$1|$<n>|$`|$']")));
    log("subst-literals", [
      "abc".replace(/b/, "$$-é-$&-$"),
      "abc".replace(/b/g, "\u{1F600}$&$").length,
      "xyz".replace(/(y)/, "$01$10$001"),
      "xyz".replace(/(?<a>y)/, "$<a$<b>$<a>"),
      "xyz".replace(/y/, "$<a>"),
      "éxyz".replace(/(?<a>y)/, "$<a"),
    ].join("|"));
  })();

  console.log(lines.join("\n"));
"##;

const EXPECTED: &[&str] = &[
    r#"sticky-greedy-test=false:0"#,
    r#"sticky-optional-test=false:0"#,
    r#"global-class-exec=null:0"#,
    r#"sticky-lazy-test=false:0"#,
    r#"global-greedy-exec=[d83d de00 61 62 78]@0:5"#,
    r#"sticky-empty-exec=[]@0:0"#,
    r#"sticky-trail-exec=null:0"#,
    r#"global-trail-exec=null:0"#,
    r#"sticky-dot-exec=[d83d de00]@0:2"#,
    r#"sticky-dot-v-exec=[d83d de00]@0:2"#,
    r#"sticky-trail-nonunicode=[de00]@1:2"#,
    r#"replace-trail=d83d de00"#,
    r#"sticky-boundary-mid-pair=[]@0:0"#,
    r#"global-boundary-mid-pair=[]@0:0"#,
    // node prints "61 d83d|de00": V8 splits the pair here, SpiderMonkey does not.
    r#"split-boundary=61 d83d de00"#,
    r#"sticky-greedy-hot=0"#,
    r#"lexer=1:a@0|0:1@1|2: @2|1:bb@3|0:22@5|2: @7|1:ccc@8|0:333@11"#,
    r#"sticky-miss=null:0"#,
    r#"sticky-hit=[62 2c]@2:4"#,
    r#"sticky-hit-after-miss=[62 62 2c]@1:4"#,
    r#"split-spaces=[" a","b","c","d"]"#,
    r#"split-captures=["a","1",null,"b","2","2","c"]"#,
    r#"split-limit=["a","b"]"#,
    r#"split-empty=["a","b","c"]"#,
    r#"split-unicode=["78","d83d de00","79"]"#,
    r#"replace-gy=XXba"#,
    r#"matchall-gy=[0,1]"#,
    r#"match-gy=["a","a"]"#,
    r#"sticky-lookbehind=[62]@1:2"#,
    r#"sticky-astral=[d83d de00]@2:4"#,
    r#"split-scan=61 62|7a 7a 7a///7a 7a 7a;61,62|/61/62/61 62;61,20,62|/61 20/62/61 20 62;78,d83d de00|/78/d83d de00/78 d83d de00;61,62,63|/61 58 62 58 58/63/61 58 62 58 58 63;61,58 61|/61/58 61/61 58 61;61,62,,63,|2c/61 2c 62 2c 2c 63//61 2c 62 2c 2c 63 2c;63 61 66,20 61 75|e9/63 61 66/20 61 75/63 61 66 e9 20 61 75;6c 31,6c 32,|a/6c 31 d a 6c 32//6c 31 d a 6c 32 a;61,31,32,62,33,undefined,|33/61 31 32 62//61 31 32 62 33"#,
    r#"split-patched-exec=["a","b","","c"]:6"#,
    r#"split-nonsticky-species=["","","c"]"#,
    r#"exec-loop=word@2|word@7|word@15"#,
    r#"split-nonascii=["e9","77 6f 72 64","77 6f 72 64","d83d de00","77 6f 72 64"]"#,
    r#"appended-subject=null|ab0|ab0ab1|ab0ab1ab2|ab0ab1ab2ab3|ab0ab1ab2ab3ab4"#,
    r#"many-subjects=19900"#,
    r#"alternating=2,1,3,"#,
    r#"lb-groups={"int":"12","frac":"50"}:int,frac"#,
    r#"lb-three={"a":"p","b":"q","c":"r"}:["","p","q","r"]"#,
    r#"lb-replace=xy[x|y]"#,
    r#"lb-indices={"a":[0,1],"b":[1,2]}"#,
    r#"lb-mixed={"a":"a","b":"b","c":"c"}"#,
    r#"lb-fn-replace=xyxy"#,
    r#"lb-hot=1"#,
    r#"split-own=["own","xay","3"]"#,
    r#"split-subclass=["subclass:xay"]"#,
    r#"split-proto=[["proto"],["proto"]]"#,
    r#"split-restored=["x","y"]"#,
    r#"v-errors=SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError"#,
    r#"v-valid=ok,ok,ok,ok,ok,ok,ok,ok,ok,ok,ok,ok"#,
    r#"v-class-backspace=true,false,true"#,
    r#"annexb-hex=[["x1"],["1"],["x"],["axZ"]]"#,
    r#"match-lastindex=TypeError,TypeError,TypeError,null:0"#,
    r#"named-occurrences=[$<a>][B1B2]yz:b,b"#,
    r#"named-tostring=T1T2yz:ts,ts"#,
    r#"ctx-nonascii=63 61 66 e9 5b 63 61 66 e9 7c 6c 69 6e 65 20 6f 6e 65 a 6c 69 6e 65 20 74 77 6f 5d 6c 69 6e 65 20 6f 6e 65 5b 63 61 66"#,
    r#"ctx-plain=28"#,
    r#"ctx-sticky=()(a)b"#,
    r#"ctx-dollar=a$`"#,
    r#"ctx-subclass=x<yx>y"#,
    r#"subst-pre=d800 5b d800 5d"#,
    r#"subst-whole=78 5b d800 5d"#,
    r#"subst-group=78 5b d800 5d"#,
    r#"subst-post=5b dc00 5d dc00"#,
    r#"subst-named=78 5b d800 5d"#,
    r#"subst-template=78 d800 61"#,
    r#"subst-subclass=78 5b d800 5d"#,
    r#"subst-seam-template=d800 dc00"#,
    r#"subst-seam-context=d800 d800"#,
    r#"subst-seam-match=d83d de00 de00"#,
    r#"subst-seam-groups=d800 62 dc00 61"#,
    r#"subst-nonascii-context=e9 d800 5b e9 d800 7c 5d"#,
    r#"subst-nonascii-named=e9 d800 3c 78 78 3e"#,
    r#"subst-observable=61 5b dc00 7c d800 7c dfff 7c 61 7c 62 5d 62"#,
    r#"subst-literals=a$-é-b-$c|6|xyy0$001z|xyz|x$<a>z|éx$<az"#,
];

const CHILD_ENV: &str = "ZIPP_REGEX_AUDIT_20260915_CHILD";

#[test]
fn regex_audit_probes_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let out = run_ok(PROBES);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    for (index, (got, want)) in got.iter().zip(EXPECTED).enumerate() {
        assert_eq!(got, want, "probe line {index}");
    }
    assert_eq!(got.len(), EXPECTED.len(), "probe line count");
}

#[test]
fn regex_audit_probes_match_node_in_every_mode() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("no-regex-jit", Some(("ZIPP_NO_RX_JIT", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "regex_audit_probes_child", "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NO_RX_JIT")
            .env_remove("ZIPP_GC_STRESS")
            .env(CHILD_ENV, "1");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "regex audit probes / {mode} failed ({:?}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn timed(src: &str) -> (Vec<String>, Duration) {
    let started = Instant::now();
    let out = run_ok(src);
    (out, started.elapsed())
}

/// R145: a regex split longer than the old 5,000,000-step cap returns every
/// piece (the cap returned the unsplit remainder as the last element).
#[test]
fn regex_split_longer_than_the_old_iteration_cap_is_complete() {
    let (out, _) = timed(
        r#"
        var s = ("x".repeat(99) + "\n").repeat(50001);
        var p = s.split(/\n/);
        console.log(s.length, p.length, p[p.length - 1].length, p[50000].length);
    "#,
    );
    assert_eq!(out, ["5000100 50002 0 99"]);
}

/// R146: a failed sticky attempt costs one attempt. Before, each of these
/// scanned the rest of the subject: the split below took 11 s and the failing
/// exec loop 1.9 s in a release build.
#[test]
fn failed_sticky_attempts_do_not_scan_the_rest_of_the_subject() {
    let (out, elapsed) = timed(
        r#"
        var gaps = ("x".repeat(20000) + ", ").repeat(10).split(/\s*,\s*/);
        var s = "a".repeat(200000) + ",", re = /b*,/y, hits = 0;
        for (var i = 0; i < 2000; i++) { re.lastIndex = i; if (re.exec(s)) hits++; }
        re.lastIndex = 200000;
        console.log(gaps.length, gaps[3].length, hits, re.test(s));
    "#,
    );
    assert_eq!(out, ["11 20000 0 true"]);
    assert!(
        elapsed < Duration::from_secs(10),
        "sticky attempts scanned the subject: {elapsed:?}"
    );
}

/// R147 + R148: one non-ASCII character no longer makes every exec re-encode
/// the subject, nor a template replace copy the text around every match.
/// Release-build baselines: split 9.3 s, match 1.8 s, template replace 0.5 s,
/// `/gy` template replace 1.7 s. The safe-sandbox profile keeps its metered
/// per-exec subject copy (the units cache is outside the audited heap), so
/// this bound pins the default profile only.
#[cfg(not(feature = "safe-sandbox"))]
#[test]
fn non_ascii_subjects_are_not_re_encoded_per_exec() {
    let (out, elapsed) = timed(
        r#"
        var uni = "é " + "word ".repeat(16000);
        var doc = "café\n" + "the quick brown fox jumps over the lazy dog \n".repeat(4000);
        var n = 0, re = /\w+/g;
        while (re.exec(uni)) n++;
        console.log(
            uni.split(/\s+/).length,
            uni.match(/\w+/g).length,
            n,
            doc.replace(/\n/g, "<br>").length,
            "a".repeat(50000).replace(/a/gy, "b").length
        );
    "#,
    );
    assert_eq!(out, ["16002 16000 16000 192008 50000"]);
    assert!(
        elapsed < Duration::from_secs(10),
        "non-ASCII regex loops are still quadratic: {elapsed:?}"
    );
}

/// A nested empty-matching loop terminates. `EnterLoop` reset the loop's
/// iteration count before `prepare_to_enter_loop` saved it, so backtracking
/// into an enclosing iteration restored a zero count and the empty-iteration
/// check never fired: `/(?:(?:a?)?)+n/.test("a")` ran until the process ran
/// out of memory (found while reviewing the 15 September 2026 audit). Every
/// expectation is node's.
#[test]
fn nested_empty_loops_terminate() {
    let (out, elapsed) = timed(
        r#"
        console.log(
            /(?:(?:a?)?)+n/.test("a"),
            /(?:(?:a?)?){1,5}n/.test("a"),
            JSON.stringify(/(?:(?:a?)?)+/.exec(", b\n,aa,\n")),
            /(?:(?:a?)?)+n/.test("aan"),
            JSON.stringify("aab".match(/(?:(a?)(b?))+/)),
            /(?:a*)*b/.test("aaa"),
            JSON.stringify(/(?:(?:a|b?)*)+c/.exec("abab"))
        );
    "#,
    );
    assert_eq!(out, [r#"false false [""] true ["aab","a","b"] false null"#]);
    assert!(
        elapsed < Duration::from_secs(10),
        "a nested empty loop did not terminate: {elapsed:?}"
    );
}
